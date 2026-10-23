//! Observabilidad común de los binarios: logs estructurados, métricas Prometheus y sondas
//! de liveness/readiness.
//!
//! - [`init_tracing`]: logs JSON (por defecto) o texto según `LOG_FORMAT`.
//! - [`probe_routes`]: `/livez` (el proceso responde) y `/readyz` (sus dependencias responden).
//! - [`metrics_routes`]: `/metrics` en formato OpenMetrics, para un listener aparte
//!   ([`serve`]) que no se expone al público.

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use core_domain::ports::HealthProbe;
use prometheus_client::encoding::text::encode;
use prometheus_client::registry::Registry;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry as Subscribers};

pub use prometheus_client;

/// Inicializa los logs. `LOG_FORMAT=text` los emite legibles; cualquier otro valor (o su
/// ausencia) produce una línea JSON por evento, con los campos de los spans activos.
/// `RUST_LOG` define el filtro; si falta, se usa `default_filter`.
pub fn init_tracing(default_filter: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let text = std::env::var("LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("text"));
    let registry = Subscribers::default().with(filter);
    if text {
        registry.with(tracing_subscriber::fmt::layer()).init();
    } else {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(false)
                    .flatten_event(true),
            )
            .init();
    }
}

/// Registro de métricas compartido (se completa al arrancar y luego solo se lee).
pub type SharedRegistry = Arc<Registry>;

const OPENMETRICS: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

async fn metrics(State(registry): State<SharedRegistry>) -> Response {
    let mut body = String::new();
    match encode(&mut body, &registry) {
        Ok(()) => ([(header::CONTENT_TYPE, OPENMETRICS)], body).into_response(),
        Err(error) => {
            tracing::error!(%error, "could not encode metrics");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn livez() -> Response {
    (StatusCode::OK, Json(status_body("UP"))).into_response()
}

async fn readyz(State(health): State<Arc<dyn HealthProbe>>) -> Response {
    match health.ping().await {
        Ok(()) => (StatusCode::OK, Json(status_body("READY"))).into_response(),
        Err(error) => {
            tracing::warn!(%error, "readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, Json(status_body("NOT_READY"))).into_response()
        }
    }
}

/// Cuerpo `{"status": ...}`.
fn status_body(status: &'static str) -> std::collections::BTreeMap<&'static str, &'static str> {
    std::collections::BTreeMap::from([("status", status)])
}

/// `/livez` y `/readyz`. Readiness falla (503) si `health` no responde.
pub fn probe_routes<S: Clone + Send + Sync + 'static>(health: Arc<dyn HealthProbe>) -> Router<S> {
    Router::new()
        .route("/livez", get(livez))
        .route("/readyz", get(readyz).with_state(health))
}

/// `/metrics` sobre `registry`.
pub fn metrics_routes<S: Clone + Send + Sync + 'static>(registry: SharedRegistry) -> Router<S> {
    Router::new().route("/metrics", get(metrics).with_state(registry))
}

/// Sirve `router` en `addr` hasta que `shutdown` se complete.
pub async fn serve(
    addr: SocketAddr,
    router: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).with_graceful_shutdown(shutdown).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use core_domain::ports::DomainError;
    use http_body_util::BodyExt;
    use prometheus_client::metrics::counter::Counter;
    use tower::ServiceExt;

    struct Probe(bool);

    #[async_trait::async_trait]
    impl HealthProbe for Probe {
        async fn ping(&self) -> Result<(), DomainError> {
            if self.0 {
                Ok(())
            } else {
                Err(DomainError::Database("down".into()))
            }
        }
    }

    async fn get(router: &Router, uri: &str) -> (StatusCode, String, Option<String>) {
        let response = router
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_string());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap(), content_type)
    }

    #[tokio::test]
    async fn readiness_reflects_dependencies_and_liveness_does_not() {
        let down: Router = probe_routes(Arc::new(Probe(false)));
        assert_eq!(get(&down, "/livez").await.0, StatusCode::OK);
        assert_eq!(get(&down, "/readyz").await.0, StatusCode::SERVICE_UNAVAILABLE);
        let up: Router = probe_routes(Arc::new(Probe(true)));
        assert_eq!(get(&up, "/readyz").await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn metrics_are_exposed_in_openmetrics_format() {
        let mut registry = Registry::default();
        let counter = Counter::<u64>::default();
        registry.register("demo_events", "Demo events", counter.clone());
        counter.inc();
        let router: Router = metrics_routes(Arc::new(registry));
        let (status, body, content_type) = get(&router, "/metrics").await;
        assert_eq!(status, StatusCode::OK);
        assert!(content_type.unwrap().starts_with("application/openmetrics-text"));
        assert!(body.contains("demo_events_total 1"), "{}", body);
        assert!(body.trim_end().ends_with("# EOF"));
    }
}

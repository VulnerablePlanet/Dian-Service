//! Métricas HTTP: peticiones y latencia por método, ruta (plantilla, no la URL concreta) y
//! código de respuesta.

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RequestLabels {
    pub method: String,
    pub route: String,
    pub status: u16,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RouteLabels {
    pub method: String,
    pub route: String,
}

#[derive(Clone)]
pub struct HttpMetrics {
    requests: Family<RequestLabels, Counter>,
    duration: Family<RouteLabels, Histogram>,
}

impl Default for HttpMetrics {
    fn default() -> Self {
        Self {
            requests: Family::default(),
            // 5 ms .. ~10 s
            duration: Family::new_with_constructor(|| Histogram::new(exponential_buckets(0.005, 2.0, 12))),
        }
    }
}

impl HttpMetrics {
    pub fn register(&self, registry: &mut Registry) {
        registry.register("http_requests", "HTTP requests handled", self.requests.clone());
        registry.register(
            "http_request_duration_seconds",
            "HTTP request latency",
            self.duration.clone(),
        );
    }
}

/// Middleware que registra cada petición.
pub async fn track(State(metrics): State<Arc<HttpMetrics>>, request: Request, next: Next) -> Response {
    let method = request.method().to_string();
    // Plantilla de la ruta (`/api/v1/documents/:id`): acota la cardinalidad de las etiquetas.
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unmatched".to_string());
    let started = Instant::now();
    let response = next.run(request).await;
    metrics
        .duration
        .get_or_create(&RouteLabels { method: method.clone(), route: route.clone() })
        .observe(started.elapsed().as_secs_f64());
    metrics
        .requests
        .get_or_create(&RequestLabels { method, route, status: response.status().as_u16() })
        .inc();
    response
}

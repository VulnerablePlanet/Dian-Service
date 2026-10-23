pub mod config;
pub mod error;
pub mod events;
pub mod handlers;
pub mod metrics;
pub mod middleware;
pub mod state;
pub mod webhook;

use crate::events::{receive_inbound_invoice, register_document_event};
use crate::handlers::{create_invoice, create_payroll, create_support_document, get_document, health_check};
use crate::middleware::auth_middleware;
use crate::state::SharedState;
use axum::http::{HeaderName, Request};
use axum::{
    middleware::{from_fn, from_fn_with_state},
    routing::{get, post},
    Router,
};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Span de cada petición: método, ruta, id de petición (cabecera `x-request-id`, generada si
/// el cliente no la envía) y tenant, que completa el middleware de autenticación.
fn request_span<B>(request: &Request<B>) -> tracing::Span {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .unwrap_or_default();
    tracing::info_span!(
        "http",
        method = %request.method(),
        path = %request.uri().path(),
        request_id = %request_id,
        tenant_id = tracing::field::Empty,
    )
}

/// Construye el router completo de la API sobre el estado dado.
pub fn router(state: SharedState) -> Router {
    let api_routes = Router::new()
        .route("/invoices", post(create_invoice))
        .route("/documents/:id", get(get_document))
        .route("/inbound/invoices", post(receive_inbound_invoice))
        .route("/documents/:id/events", post(register_document_event))
        .route("/payroll", post(create_payroll))
        .route("/support-documents", post(create_support_document))
        .layer(from_fn_with_state(state.clone(), auth_middleware));

    let public_routes = Router::new().route("/health", get(health_check));

    Router::new()
        .nest("/api/v1", api_routes.merge(public_routes))
        .merge(observability::probe_routes(state.health.clone()))
        .route_layer(from_fn_with_state(state.metrics.clone(), crate::metrics::track))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(request_span)
                // Una línea por petición (estado y latencia) dentro del span de la petición.
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(PropagateRequestIdLayer::new(REQUEST_ID))
        .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
        .layer(from_fn(crate::middleware::no_store))
        .with_state(state)
}

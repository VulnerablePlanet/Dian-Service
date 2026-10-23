pub mod config;
pub mod error;
pub mod events;
pub mod handlers;
pub mod middleware;
pub mod state;
pub mod telemetry;
pub mod webhook;

use crate::events::{receive_inbound_invoice, register_document_event};
use crate::handlers::{create_invoice, create_payroll, create_support_document, get_document, health_check};
use crate::middleware::auth_middleware;
use crate::state::SharedState;
use axum::{
    middleware::from_fn_with_state,
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;

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
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use core_domain::ports::DomainError;
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Not implemented: {0}")]
    NotImplemented(String),

    #[error(transparent)]
    Domain(#[from] DomainError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            ApiError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg),
            ApiError::NotImplemented(msg) => (StatusCode::NOT_IMPLEMENTED, "NOT_IMPLEMENTED", msg),
            ApiError::Domain(error) => match error {
                DomainError::Validation(msg) => (StatusCode::UNPROCESSABLE_ENTITY, "VALIDATION_ERROR", msg),
                DomainError::Forbidden(msg) => (StatusCode::FORBIDDEN, "FORBIDDEN", msg),
                DomainError::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg),
                DomainError::Conflict(msg) => (StatusCode::CONFLICT, "CONFLICT", msg),
                // Errores de infraestructura: el detalle va al log, no al cliente.
                DomainError::Database(_) | DomainError::Queue(_) => {
                    tracing::error!(%error, "infrastructure error");
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "SERVICE_UNAVAILABLE",
                        "Servicio temporalmente no disponible; reintente mas tarde".to_string(),
                    )
                }
                DomainError::Soap(_) => {
                    tracing::error!(%error, "DIAN upstream error");
                    (
                        StatusCode::BAD_GATEWAY,
                        "UPSTREAM_ERROR",
                        "Error comunicandose con la DIAN; reintente mas tarde".to_string(),
                    )
                }
                DomainError::Crypto(_) | DomainError::Internal(_) => {
                    tracing::error!(%error, "internal error");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "INTERNAL_ERROR",
                        "Error interno del servidor".to_string(),
                    )
                }
            },
        };

        (status, Json(json!({ "error": { "code": code, "message": message } }))).into_response()
    }
}

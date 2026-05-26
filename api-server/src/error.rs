use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Validation failed: {0}")]
    Validation(String),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Domain logic error: {0}")]
    Domain(#[from] core_domain::ports::DomainError),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Not Found: {0}")]
    NotFound(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error_code, message) = match self {
            ApiError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg),
            ApiError::Validation(msg) => (StatusCode::UNPROCESSABLE_ENTITY, "VALIDATION_ERROR", msg),
            ApiError::Database(e) => (StatusCode::INTERNAL_SERVER_ERROR, "DATABASE_ERROR", e.to_string()),
            ApiError::Domain(e) => (StatusCode::BAD_REQUEST, "DOMAIN_ERROR", e.to_string()),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, "CONFLICT_ERROR", msg),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg),
        };

        let body = Json(json!({
            "error": {
                "code": error_code,
                "message": message
            }
        }));

        (status, body).into_response()
    }
}

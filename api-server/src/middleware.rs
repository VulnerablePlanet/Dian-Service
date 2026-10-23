use crate::error::ApiError;
use crate::state::SharedState;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

/// Middleware de autenticación multi-tenant: `Authorization: Bearer <api key>`.
pub async fn auth_middleware(
    State(state): State<SharedState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = request
        .headers()
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::Unauthorized("Missing Authorization header".to_string()))?
        .strip_prefix("Bearer ")
        .ok_or_else(|| ApiError::Unauthorized("Invalid Authorization scheme. Use Bearer".to_string()))?
        .trim()
        .to_string();

    let tenant = state
        .tenants
        .find_by_api_key_hash(&core_domain::api_keys::hash(&token))
        .await?
        .ok_or_else(|| ApiError::Unauthorized("Unauthorized: Invalid API Key".to_string()))?;

    tracing::Span::current().record("tenant_id", tracing::field::display(tenant.id));
    request.extensions_mut().insert(tenant);
    Ok(next.run(request).await)
}

/// Las respuestas de la API contienen datos tributarios: ningún intermediario debe guardarlas.
pub async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
    response
}

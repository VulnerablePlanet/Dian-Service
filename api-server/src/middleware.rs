use crate::error::ApiError;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use core_domain::models::Tenant;
use sqlx::PgPool;

/// Middleware de autenticación multi-tenant.
/// Valida la cabecera Authorization: Bearer <key> contra api_keys->>'secret' en tenants.
pub async fn auth_middleware(
    State(pool): State<PgPool>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::Unauthorized("Missing Authorization header".to_string()))?;

    if !auth_header.starts_with("Bearer ") {
        return Err(ApiError::Unauthorized("Invalid Authorization scheme. Use Bearer".to_string()));
    }

    let token = auth_header["Bearer ".len()..].trim();

    // Query PostgreSQL buscando en el JSONB de api_keys
    let tenant = sqlx::query_as::<_, Tenant>(
        "SELECT * FROM tenants WHERE api_keys->>'secret' = $1"
    )
    .bind(token)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| ApiError::Unauthorized("Unauthorized: Invalid API Key".to_string()))?;

    // Inyectar el Tenant para aislamiento en handlers
    request.extensions_mut().insert(tenant);

    Ok(next.run(request).await)
}

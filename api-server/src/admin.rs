//! Rutas de administración (`/admin/v1`), para el operador de la plataforma.
//!
//! Se autentican con `Authorization: Bearer <token de administración>`; el servicio solo
//! conoce su SHA-256 (`ADMIN_TOKEN_SHA256`). Sin esa variable las rutas no se montan.

use crate::error::ApiError;
use crate::state::SharedState;
use application::admin::{CreateTenant, RegisterCertificate, RegisterNumberingRange};
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{from_fn_with_state, Next};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use core_domain::api_keys::{constant_time_eq, hash};
use core_domain::ports::DomainError;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

fn to_json<T: serde::Serialize>(value: T) -> Result<Json<Value>, ApiError> {
    Ok(Json(serde_json::to_value(value).map_err(|e| DomainError::Internal(e.to_string()))?))
}

async fn admin_auth(State(state): State<SharedState>, request: Request, next: Next) -> Result<Response, ApiError> {
    let expected = state
        .config
        .admin_token_sha256
        .as_deref()
        .ok_or_else(|| ApiError::Unauthorized("Administration is disabled".to_string()))?;
    let token = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .ok_or_else(|| ApiError::Unauthorized("Missing admin token".to_string()))?;
    if !constant_time_eq(hash(token).as_bytes(), expected.as_bytes()) {
        tracing::warn!("rejected admin request with an invalid token");
        return Err(ApiError::Unauthorized("Invalid admin token".to_string()));
    }
    Ok(next.run(request).await)
}

async fn create_tenant(State(state): State<SharedState>, Json(body): Json<CreateTenant>) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant = state.admin.create_tenant(body).await?;
    tracing::info!(target: "admin_audit", action = "tenant.created", tenant_id = %tenant.id);
    Ok((StatusCode::CREATED, to_json(tenant_view(&tenant))?))
}

fn tenant_view(tenant: &core_domain::models::Tenant) -> Value {
    json!({
        "id": tenant.id,
        "tax_id": tenant.tax_id,
        "registration_name": tenant.registration_name,
        "tax_regime": tenant.tax_regime,
        "reception_email": tenant.reception_email,
        "created_at": tenant.created_at,
    })
}

async fn get_tenant(State(state): State<SharedState>, Path(id): Path<Uuid>) -> Result<Json<Value>, ApiError> {
    Ok(Json(tenant_view(&state.admin.get_tenant(id).await?)))
}

#[derive(Deserialize)]
struct CreateApiKey {
    name: String,
}

async fn create_api_key(
    State(state): State<SharedState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateApiKey>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let issued = state.admin.create_api_key(id, &body.name).await?;
    tracing::info!(target: "admin_audit", action = "api_key.created", tenant_id = %id, key_id = %issued.id);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": issued.id,
            "name": issued.name,
            "prefix": issued.prefix,
            "key": issued.key,
            "message": "Guarda la clave: no se volvera a mostrar"
        })),
    ))
}

async fn list_api_keys(State(state): State<SharedState>, Path(id): Path<Uuid>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "api_keys": state.admin.list_api_keys(id).await? })))
}

async fn revoke_api_key(State(state): State<SharedState>, Path((id, key_id)): Path<(Uuid, Uuid)>) -> Result<StatusCode, ApiError> {
    state.admin.revoke_api_key(id, key_id).await?;
    tracing::info!(target: "admin_audit", action = "api_key.revoked", tenant_id = %id, key_id = %key_id);
    Ok(StatusCode::NO_CONTENT)
}

async fn register_certificate(
    State(state): State<SharedState>,
    Path(id): Path<Uuid>,
    Json(body): Json<RegisterCertificate>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let certificate = state.admin.register_certificate(id, body).await?;
    tracing::info!(target: "admin_audit", action = "certificate.activated", tenant_id = %id, certificate_id = %certificate.id);
    Ok((StatusCode::CREATED, to_json(certificate)?))
}

async fn register_numbering_range(
    State(state): State<SharedState>,
    Path(id): Path<Uuid>,
    Json(body): Json<RegisterNumberingRange>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let range = state.admin.register_numbering_range(id, body).await?;
    tracing::info!(target: "admin_audit", action = "numbering_range.created", tenant_id = %id, range_id = %range.id);
    // La clave técnica no se devuelve.
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": range.id,
            "document_type": range.document_type,
            "prefix": range.prefix,
            "from_number": range.from_number,
            "to_number": range.to_number,
            "valid_date_from": range.valid_date_from,
            "valid_date_to": range.valid_date_to,
            "current_counter": range.current_counter,
        })),
    ))
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<i64>,
}

async fn dead_jobs(State(state): State<SharedState>, Query(q): Query<ListQuery>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "jobs": state.admin.dead_jobs(q.limit.unwrap_or(50)).await? })))
}

async fn retry_job(State(state): State<SharedState>, Path(id): Path<Uuid>) -> Result<StatusCode, ApiError> {
    state.admin.retry_dead_job(id).await?;
    tracing::info!(target: "admin_audit", action = "job.retried", job_id = %id);
    Ok(StatusCode::ACCEPTED)
}

async fn dead_deliveries(State(state): State<SharedState>, Query(q): Query<ListQuery>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "deliveries": state.admin.dead_deliveries(q.limit.unwrap_or(50)).await? })))
}

async fn retry_delivery(State(state): State<SharedState>, Path(id): Path<Uuid>) -> Result<StatusCode, ApiError> {
    state.admin.retry_dead_delivery(id).await?;
    tracing::info!(target: "admin_audit", action = "webhook_delivery.retried", delivery_id = %id);
    Ok(StatusCode::ACCEPTED)
}

pub fn routes(state: SharedState) -> Router<SharedState> {
    Router::new()
        .route("/tenants", post(create_tenant))
        .route("/tenants/:id", get(get_tenant))
        .route("/tenants/:id/api-keys", post(create_api_key).get(list_api_keys))
        .route("/tenants/:id/api-keys/:key_id", delete(revoke_api_key))
        .route("/tenants/:id/certificates", post(register_certificate))
        .route("/tenants/:id/numbering-ranges", post(register_numbering_range))
        .route("/jobs/dead", get(dead_jobs))
        .route("/jobs/:id/retry", post(retry_job))
        .route("/webhook-deliveries/dead", get(dead_deliveries))
        .route("/webhook-deliveries/:id/retry", post(retry_delivery))
        .layer(from_fn_with_state(state, admin_auth))
}

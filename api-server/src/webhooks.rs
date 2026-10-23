//! Suscripciones de webhooks del tenant autenticado.

use crate::error::ApiError;
use crate::state::SharedState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use core_domain::models::Tenant;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct CreateWebhook {
    pub target_url: String,
    /// `*` o un evento concreto (`document.accepted`, `document.rejected`,
    /// `document.contingency`, `document.failed`).
    pub event_type: String,
}

/// POST /api/v1/webhooks — devuelve el secreto de firma una única vez.
pub async fn create_webhook(
    State(state): State<SharedState>,
    Extension(tenant): Extension<Tenant>,
    Json(body): Json<CreateWebhook>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let created = state.webhooks.register(&tenant, &body.target_url, &body.event_type).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": created.id,
            "target_url": created.target_url,
            "event_type": created.event_type,
            "secret": created.secret,
            "message": "Guarda el secreto: no se volvera a mostrar"
        })),
    ))
}

/// GET /api/v1/webhooks
pub async fn list_webhooks(
    State(state): State<SharedState>,
    Extension(tenant): Extension<Tenant>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "webhooks": state.webhooks.list(&tenant).await? })))
}

/// DELETE /api/v1/webhooks/:id
pub async fn delete_webhook(
    State(state): State<SharedState>,
    Extension(tenant): Extension<Tenant>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.webhooks.delete(&tenant, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

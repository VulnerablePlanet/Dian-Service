use crate::error::ApiError;
use crate::state::SharedState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{DateTime, Utc};
use core_domain::models::{Document, Tenant};
use core_domain::ports::IssueOutcome;
use serde::Serialize;
use serde_json::json;
use ubl_engine::payload::{InvoicePayload, PayrollPayload, SupportDocumentPayload};
use uuid::Uuid;

/// Campos del payload que contienen secretos del tenant y no se devuelven nunca.
const SECRET_PAYLOAD_FIELDS: &[&str] = &["software_pin", "technical_key"];

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    headers
        .get("Idempotency-Key")
        .map(|value| {
            value.to_str().map(str::to_string).map_err(|_| {
                ApiError::Domain(core_domain::ports::DomainError::Validation(
                    "Idempotency-Key debe ser ASCII".to_string(),
                ))
            })
        })
        .transpose()
}

/// Respuesta 202 de una emisión. Una repetición con la misma `Idempotency-Key` devuelve el
/// mismo cuerpo y la cabecera `Idempotent-Replayed: true`.
fn accepted(outcome: IssueOutcome, message: &str) -> Response {
    let (issued, replayed) = match outcome {
        IssueOutcome::Created(issued) => (issued, false),
        IssueOutcome::Replayed(issued) => (issued, true),
    };
    let mut response = (
        StatusCode::ACCEPTED,
        Json(json!({
            "document_id": issued.document_id,
            "document_number": issued.document_number,
            "status": "QUEUED",
            "message": message
        })),
    )
        .into_response();
    if replayed {
        response
            .headers_mut()
            .insert("Idempotent-Replayed", HeaderValue::from_static("true"));
    }
    response
}

/// POST /api/v1/invoices
pub async fn create_invoice(
    State(state): State<SharedState>,
    Extension(tenant): Extension<Tenant>,
    headers: HeaderMap,
    Json(payload): Json<InvoicePayload>,
) -> Result<Response, ApiError> {
    let outcome = state
        .issue
        .issue_invoice(&tenant, payload, idempotency_key(&headers)?)
        .await?;
    Ok(accepted(outcome, "Factura encolada correctamente para su firmado y transmision"))
}

/// POST /api/v1/payroll
pub async fn create_payroll(
    State(state): State<SharedState>,
    Extension(tenant): Extension<Tenant>,
    headers: HeaderMap,
    Json(payload): Json<PayrollPayload>,
) -> Result<Response, ApiError> {
    let outcome = state
        .issue
        .issue_payroll(&tenant, payload, idempotency_key(&headers)?)
        .await?;
    Ok(accepted(outcome, "Reporte de nomina encolado correctamente"))
}

/// POST /api/v1/support-documents
pub async fn create_support_document(
    State(state): State<SharedState>,
    Extension(tenant): Extension<Tenant>,
    headers: HeaderMap,
    Json(payload): Json<SupportDocumentPayload>,
) -> Result<Response, ApiError> {
    let outcome = state
        .issue
        .issue_support_document(&tenant, payload, idempotency_key(&headers)?)
        .await?;
    Ok(accepted(outcome, "Documento soporte reservado y encolado correctamente"))
}

/// Representación pública de un documento: los secretos del payload se omiten.
#[derive(Serialize)]
pub struct DocumentView {
    id: Uuid,
    tenant_id: Uuid,
    direction: String,
    document_type: String,
    prefix: String,
    document_number: i32,
    cufe_cude: String,
    payload: serde_json::Value,
    original_xml: Option<String>,
    signed_xml: Option<String>,
    pdf_url: Option<String>,
    status: String,
    created_at: DateTime<Utc>,
}

impl From<Document> for DocumentView {
    fn from(document: Document) -> Self {
        let mut payload = document.payload;
        if let Some(object) = payload.as_object_mut() {
            for field in SECRET_PAYLOAD_FIELDS {
                object.remove(*field);
            }
        }
        Self {
            id: document.id,
            tenant_id: document.tenant_id,
            direction: document.direction.to_string(),
            document_type: document.document_type.to_string(),
            prefix: document.prefix,
            document_number: document.document_number,
            cufe_cude: document.cufe_cude,
            payload,
            original_xml: document.original_xml,
            signed_xml: document.signed_xml,
            pdf_url: document.pdf_url,
            status: document.status.to_string(),
            created_at: document.created_at,
        }
    }
}

/// GET /api/v1/documents/:id
pub async fn get_document(
    State(state): State<SharedState>,
    Extension(tenant): Extension<Tenant>,
    Path(doc_id): Path<Uuid>,
) -> Result<Json<DocumentView>, ApiError> {
    let document = state
        .documents
        .find_tenant_document(tenant.id, doc_id)
        .await?
        .ok_or_else(|| core_domain::ports::DomainError::NotFound(format!("Documento {} no encontrado", doc_id)))?;
    Ok(Json(document.into()))
}

/// GET /api/v1/health — 200 si la base de datos responde, 503 si no.
pub async fn health_check(State(state): State<SharedState>) -> Response {
    match state.health.ping().await {
        Ok(()) => (StatusCode::OK, Json(json!({ "status": "UP", "database": "CONNECTED" }))).into_response(),
        Err(error) => {
            tracing::error!(%error, "health check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "DOWN", "database": "DISCONNECTED" })),
            )
                .into_response()
        }
    }
}

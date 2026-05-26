use crate::error::ApiError;
use axum::{
    extract::{Path, State},
    Extension, Json, response::IntoResponse,
};
use core_domain::models::{Document, DocumentStatus, DocumentType, Tenant};
use redis::AsyncCommands;
use serde_json::json;
use sqlx::PgPool;
use ubl_engine::payload::InvoicePayload;
use uuid::Uuid;

/// POST /api/v1/invoices
/// Recibe la factura del ERP, la inserta en base de datos y la encola en Redis.
pub async fn create_invoice(
    State((pool, redis_client)): State<(PgPool, redis::Client)>,
    Extension(tenant): Extension<Tenant>,
    Json(payload): Json<InvoicePayload>,
) -> Result<impl IntoResponse, ApiError> {
    // 1. Validar el payload localmente (Fail-Fast)
    payload.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;

    // 2. Verificar que el NIT coincide con el del Tenant autenticado
    if payload.company_nit != tenant.tax_id {
        return Err(ApiError::Unauthorized(
            "El NIT de la empresa emisora no coincide con el del tenant autenticado".to_string(),
        ));
    }

    // 3. Persistir en PostgreSQL
    let doc_id = Uuid::new_v4();
    let document = Document {
        id: doc_id,
        tenant_id: tenant.id,
        document_type: DocumentType::Invoice,
        prefix: payload.prefix.clone(),
        document_number: payload.number,
        cufe_cude: "".to_string(), // Se calculará en el worker asíncrono
        payload: serde_json::to_value(&payload).unwrap(),
        original_xml: None,
        signed_xml: None,
        pdf_url: None,
        status: DocumentStatus::Pending,
        created_at: chrono::Utc::now(),
    };

    sqlx::query(
        "INSERT INTO documents (id, tenant_id, document_type, prefix, document_number, cufe_cude, payload, status, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
    )
    .bind(document.id)
    .bind(document.tenant_id)
    .bind(document.document_type)
    .bind(&document.prefix)
    .bind(document.document_number)
    .bind(&document.cufe_cude)
    .bind(&document.payload)
    .bind(document.status)
    .bind(document.created_at)
    .execute(&pool)
    .await?;

    // 4. Encolar el trabajo en Redis para el worker asíncrono
    let mut redis_conn = redis_client.get_async_connection().await
        .map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Queue(e.to_string())))?;

    let job = json!({
        "document_id": doc_id,
        "attempt": 1
    });
    let job_str = serde_json::to_string(&job).unwrap();
    let _: () = redis_conn.lpush("dian_jobs", job_str).await
        .map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Queue(e.to_string())))?;

    // Responder 202 Accepted
    let response_body = json!({
        "document_id": doc_id,
        "status": "QUEUED",
        "message": "Factura encolada correctamente para su firmado y transmision"
    });

    Ok((axum::http::StatusCode::ACCEPTED, Json(response_body)))
}

/// GET /api/v1/documents/:id
/// Consulta el estado de procesamiento de un documento electrónico.
pub async fn get_document(
    State((pool, _)): State<(PgPool, redis::Client)>,
    Extension(tenant): Extension<Tenant>,
    Path(doc_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let document = sqlx::query_as::<_, Document>(
        "SELECT * FROM documents WHERE id = $1 AND tenant_id = $2"
    )
    .bind(doc_id)
    .bind(tenant.id)
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("Documento {} no encontrado", doc_id)))?;

    Ok(Json(document))
}

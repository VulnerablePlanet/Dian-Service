use crate::error::ApiError;
use axum::{
    extract::{Path, State},
    Extension, Json, response::IntoResponse,
};
use core_domain::models::{Document, DocumentStatus, DocumentType, Tenant};
use redis::AsyncCommands;
use serde_json::json;
use sqlx::PgPool;
use ubl_engine::payload::{InvoicePayload, PayrollPayload, SupportDocumentPayload};
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

    let doc_type = match payload.document_type.as_str() {
        "invoice" => DocumentType::Invoice,
        "credit_note" => DocumentType::CreditNote,
        "debit_note" => DocumentType::DebitNote,
        _ => return Err(ApiError::Validation(
            "Tipo de documento invalido. Debe ser 'invoice', 'credit_note' o 'debit_note'".to_string()
        )),
    };

    // 3. Persistir en PostgreSQL usando transaccion corta con bloqueo pesimista
    let doc_id = Uuid::new_v4();
    let mut tx = pool.begin().await?;

    let range_row = sqlx::query(
        "SELECT id, current_counter, to_number 
         FROM numbering_ranges 
         WHERE tenant_id = $1 AND document_type = $2 AND prefix = $3 
         FOR UPDATE"
    )
    .bind(tenant.id)
    .bind(&doc_type)
    .bind(&payload.prefix)
    .fetch_optional(&mut *tx)
    .await?;

    let (range_id, current_counter, to_number) = match range_row {
        Some(row) => {
            use sqlx::Row;
            (
                row.get::<Uuid, _>("id"),
                row.get::<i32, _>("current_counter"),
                row.get::<i32, _>("to_number"),
            )
        }
        None => {
            return Err(ApiError::Validation(format!(
                "No se encontro un rango de numeracion activo para el prefijo '{}' y tipo '{}'",
                payload.prefix, payload.document_type
            )));
        }
    };

    if payload.number <= current_counter {
        return Err(ApiError::Validation(format!(
            "El numero de documento {} ya ha sido superado por el consecutivo actual {}",
            payload.number, current_counter
        )));
    }

    if payload.number > to_number {
        return Err(ApiError::Validation(format!(
            "El numero de documento {} excede el limite superior de la resolucion {}",
            payload.number, to_number
        )));
    }

    // Actualizar consecutivo en numbering_ranges
    sqlx::query(
        "UPDATE numbering_ranges SET current_counter = $1 WHERE id = $2"
    )
    .bind(payload.number)
    .bind(range_id)
    .execute(&mut *tx)
    .await?;

    let document = Document {
        id: doc_id,
        tenant_id: tenant.id,
        document_type: doc_type,
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
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

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

/// POST /api/v1/payroll
/// Recibe el reporte de nómina electrónica del ERP, lo persiste y encola.
pub async fn create_payroll(
    State((pool, redis_client)): State<(PgPool, redis::Client)>,
    Extension(tenant): Extension<Tenant>,
    Json(payload): Json<PayrollPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;

    if payload.employer_nit != tenant.tax_id {
        return Err(ApiError::Unauthorized(
            "El NIT del empleador no coincide con el del tenant autenticado".to_string(),
        ));
    }

    let doc_id = Uuid::new_v4();
    let mut tx = pool.begin().await?;

    let range_row = sqlx::query(
        "SELECT id, current_counter, to_number 
         FROM numbering_ranges 
         WHERE tenant_id = $1 AND document_type = 'payroll' AND prefix = $2 
         FOR UPDATE"
    )
    .bind(tenant.id)
    .bind(&payload.prefix)
    .fetch_optional(&mut *tx)
    .await?;

    let (range_id, current_counter, to_number) = match range_row {
        Some(row) => {
            use sqlx::Row;
            (
                row.get::<Uuid, _>("id"),
                row.get::<i32, _>("current_counter"),
                row.get::<i32, _>("to_number"),
            )
        }
        None => {
            return Err(ApiError::Validation(format!(
                "No se encontro un rango de numeracion activo para el prefijo '{}' y tipo 'payroll'",
                payload.prefix
            )));
        }
    };

    if payload.number <= current_counter {
        return Err(ApiError::Validation(format!(
            "El numero de nomina {} ya ha sido superado por el consecutivo actual {}",
            payload.number, current_counter
        )));
    }

    if payload.number > to_number {
        return Err(ApiError::Validation(format!(
            "El numero de nomina {} excede el limite superior de la resolucion {}",
            payload.number, to_number
        )));
    }

    // Actualizar consecutivo en numbering_ranges
    sqlx::query(
        "UPDATE numbering_ranges SET current_counter = $1 WHERE id = $2"
    )
    .bind(payload.number)
    .bind(range_id)
    .execute(&mut *tx)
    .await?;

    let document = Document {
        id: doc_id,
        tenant_id: tenant.id,
        document_type: DocumentType::Payroll,
        prefix: payload.prefix.clone(),
        document_number: payload.number,
        cufe_cude: "".to_string(),
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
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let mut redis_conn = redis_client.get_async_connection().await
        .map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Queue(e.to_string())))?;

    let job = json!({
        "document_id": doc_id,
        "attempt": 1
    });
    let job_str = serde_json::to_string(&job).unwrap();
    let _: () = redis_conn.lpush("dian_jobs", job_str).await
        .map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Queue(e.to_string())))?;

    let response_body = json!({
        "document_id": doc_id,
        "status": "QUEUED",
        "message": "Reporte de nomina encolado correctamente"
    });

    Ok((axum::http::StatusCode::ACCEPTED, Json(response_body)))
}

/// POST /api/v1/support-documents
/// Recibe un documento soporte, adquiere el consecutivo de forma atómica y lo encola.
pub async fn create_support_document(
    State((pool, redis_client)): State<(PgPool, redis::Client)>,
    Extension(tenant): Extension<Tenant>,
    Json(payload): Json<SupportDocumentPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;

    if payload.buyer_nit != tenant.tax_id {
        return Err(ApiError::Unauthorized(
            "El NIT del comprador no coincide con el del tenant autenticado".to_string(),
        ));
    }

    // Corta transacción para el bloqueo pesimista concurrent (FOR UPDATE)
    let doc_id = Uuid::new_v4();
    let mut tx = pool.begin().await?;

    let range = sqlx::query!(
        "SELECT id, current_counter, to_number 
         FROM numbering_ranges 
         WHERE tenant_id = $1 AND document_type = 'support_document' AND prefix = $2 
         FOR UPDATE",
        tenant.id,
        payload.prefix
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::Validation(format!(
        "No se encontro un rango de numeracion activo para el prefijo '{}' y tipo 'support_document'",
        payload.prefix
    )))?;

    if range.current_counter >= range.to_number {
        return Err(ApiError::Validation("Rango de numeracion agotado para documento soporte".to_string()));
    }

    let next_number = range.current_counter + 1;

    // Incrementar consecutivo
    sqlx::query!(
        "UPDATE numbering_ranges SET current_counter = current_counter + 1 WHERE id = $1",
        range.id
    )
    .execute(&mut *tx)
    .await?;

    // Actualizar el payload inyectando el número consecutivo reservado
    let mut updated_payload = payload.clone();
    updated_payload.number = next_number;
    let payload_val = serde_json::to_value(&updated_payload).unwrap();

    let document = Document {
        id: doc_id,
        tenant_id: tenant.id,
        document_type: DocumentType::SupportDocument,
        prefix: payload.prefix.clone(),
        document_number: next_number,
        cufe_cude: "".to_string(),
        payload: payload_val,
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
    .execute(&mut *tx)
    .await?;

    // Commit inmediato para liberar base de datos antes de I/O de Redis o SOAP
    tx.commit().await?;

    let mut redis_conn = redis_client.get_async_connection().await
        .map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Queue(e.to_string())))?;

    let job = json!({
        "document_id": doc_id,
        "attempt": 1
    });
    let job_str = serde_json::to_string(&job).unwrap();
    let _: () = redis_conn.lpush("dian_jobs", job_str).await
        .map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Queue(e.to_string())))?;

    let response_body = json!({
        "document_id": doc_id,
        "document_number": next_number,
        "status": "QUEUED",
        "message": "Documento soporte reservado y encolado correctamente"
    });

    Ok((axum::http::StatusCode::ACCEPTED, Json(response_body)))
}

/// GET /api/v1/health
/// Chequea la salud de la base de datos y Redis.
pub async fn health_check(
    State((pool, redis_client)): State<(PgPool, redis::Client)>,
) -> Result<impl IntoResponse, ApiError> {
    // 1. Chequear PostgreSQL
    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Database(e.to_string())))?;

    // 2. Chequear Redis
    let mut redis_conn = redis_client.get_async_connection().await
        .map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Queue(e.to_string())))?;
        
    let _: () = redis::cmd("PING")
        .query_async(&mut redis_conn)
        .await
        .map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Queue(e.to_string())))?;

    Ok(Json(json!({
        "status": "UP",
        "database": "CONNECTED",
        "redis": "CONNECTED"
    })))
}

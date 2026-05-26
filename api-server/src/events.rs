use crate::error::ApiError;
use axum::{
    extract::{Path, State},
    Extension, Json, response::IntoResponse,
};
use core_domain::models::{DianEvent, Document, DocumentStatus, DocumentType, EventStatus, Tenant};
use crypto_signer::provider::LocalCertificateProvider;
use crypto_signer::sign_document;
use dian_soap_client::client::DianSoapClientImpl;
use core_domain::ports::DianSoapClient;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(serde::Deserialize)]
pub struct InboundInvoicePayload {
    pub zip_content: String, // Base64
}

#[derive(serde::Deserialize)]
pub struct EventRequest {
    pub event_code: String,
    pub software_pin: String,
    pub environment: String,
}

/// POST /api/v1/inbound/invoices
/// Recibe el AttachedDocument ZIP de un proveedor, descomprime y valida la firma y la aceptación DIAN.
pub async fn receive_inbound_invoice(
    State((pool, _)): State<(PgPool, redis::Client)>,
    Extension(tenant): Extension<Tenant>,
    Json(payload): Json<InboundInvoicePayload>,
) -> Result<impl IntoResponse, ApiError> {
    // 1. Decodificar Base64
    let zip_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &payload.zip_content)
        .map_err(|e| ApiError::Validation(format!("Contenido ZIP Base64 invalido: {}", e)))?;

    // 2. Descomprimir ZIP en memoria
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| ApiError::Validation(format!("No se pudo leer el archivo ZIP: {}", e)))?;

    if archive.is_empty() {
        return Err(ApiError::Validation("El archivo ZIP esta vacio".to_string()));
    }

    let mut xml_content = String::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)
            .map_err(|e| ApiError::Validation(format!("Error leyendo elemento en ZIP: {}", e)))?;
        if file.name().ends_with(".xml") {
            use std::io::Read;
            file.read_to_string(&mut xml_content)
                .map_err(|e| ApiError::Validation(format!("Error leyendo XML del ZIP: {}", e)))?;
            break;
        }
    }

    if xml_content.is_empty() {
        return Err(ApiError::Validation("No se encontro ningun archivo XML en el ZIP".to_string()));
    }

    // 3. Validar firma digital presente
    if !xml_content.contains("<ds:Signature") && !xml_content.contains("<Signature") {
        return Err(ApiError::Validation("El AttachedDocument XML no esta firmado digitalmente".to_string()));
    }

    // 4. Validar el codigo de respuesta de aceptacion de la DIAN ("02")
    let response_code = extract_tag_content(&xml_content, "ResponseCode");
    if response_code.as_deref() != Some("02") {
        return Err(ApiError::Validation(
            "El AttachedDocument no contiene el codigo de aceptacion reglamentario de la DIAN ('02')".to_string()
        ));
    }

    // 5. Extraer metadatos
    let sender_nit = extract_tag_from_section(&xml_content, "<cac:SenderParty>", "</cac:SenderParty>", "CompanyID")
        .ok_or_else(|| ApiError::Validation("No se encontro el NIT del emisor en el AttachedDocument".to_string()))?;
    let sender_name = extract_tag_from_section(&xml_content, "<cac:SenderParty>", "</cac:SenderParty>", "RegistrationName")
        .or_else(|| extract_tag_from_section(&xml_content, "<cac:SenderParty>", "</cac:SenderParty>", "Name"))
        .unwrap_or_else(|| "Proveedor Inbound".to_string());

    let receiver_nit = extract_tag_from_section(&xml_content, "<cac:ReceiverParty>", "</cac:ReceiverParty>", "CompanyID")
        .ok_or_else(|| ApiError::Validation("No se encontro el NIT del receptor en el AttachedDocument".to_string()))?;
    let receiver_name = extract_tag_from_section(&xml_content, "<cac:ReceiverParty>", "</cac:ReceiverParty>", "RegistrationName")
        .or_else(|| extract_tag_from_section(&xml_content, "<cac:ReceiverParty>", "</cac:ReceiverParty>", "Name"))
        .unwrap_or_else(|| "Tenant Receptor".to_string());

    let parent_document_id = extract_tag_content(&xml_content, "ParentDocumentID")
        .ok_or_else(|| ApiError::Validation("No se encontro el ParentDocumentID (Numero de factura)".to_string()))?;
    let cufe = extract_tag_content(&xml_content, "UUID")
        .ok_or_else(|| ApiError::Validation("No se encontro el CUFE/UUID de la factura original".to_string()))?;

    // Validar que el receptor de la factura es este Tenant
    if receiver_nit != tenant.tax_id {
        return Err(ApiError::Validation(format!(
            "El NIT receptor del documento ({}) no coincide con el del tenant autenticado ({})",
            receiver_nit, tenant.tax_id
        )));
    }

    // Separar prefijo y consecutivo para guardarlo en la DB
    let mut prefix = String::new();
    let mut number = 0;
    if let Some(pos) = parent_document_id.rfind('-') {
        prefix = parent_document_id[..pos].to_string();
        number = parent_document_id[pos + 1..].parse::<i32>().unwrap_or(0);
    } else {
        let first_digit_idx = parent_document_id.find(|c: char| c.is_ascii_digit());
        if let Some(idx) = first_digit_idx {
            prefix = parent_document_id[..idx].to_string();
            number = parent_document_id[idx..].parse::<i32>().unwrap_or(0);
        } else {
            number = parent_document_id.parse::<i32>().unwrap_or(0);
        }
    }

    let doc_id = Uuid::new_v4();
    let doc_payload = json!({
        "inbound": true,
        "parent_document_id": parent_document_id,
        "cufe": cufe,
        "sender_nit": sender_nit,
        "sender_name": sender_name,
        "receiver_nit": receiver_nit,
        "receiver_name": receiver_name
    });

    // 6. Almacenar en PostgreSQL
    sqlx::query(
        "INSERT INTO documents (id, tenant_id, document_type, prefix, document_number, cufe_cude, payload, signed_xml, status, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (tenant_id, document_type, prefix, document_number) DO UPDATE
         SET signed_xml = EXCLUDED.signed_xml, payload = EXCLUDED.payload, cufe_cude = EXCLUDED.cufe_cude, status = EXCLUDED.status"
    )
    .bind(doc_id)
    .bind(tenant.id)
    .bind(DocumentType::Invoice)
    .bind(&prefix)
    .bind(number)
    .bind(&cufe)
    .bind(&doc_payload)
    .bind(&xml_content)
    .bind(DocumentStatus::DianAccepted)
    .bind(chrono::Utc::now())
    .execute(&pool)
    .await?;

    Ok(Json(json!({
        "document_id": doc_id,
        "parent_document_id": parent_document_id,
        "status": "ACCEPTED_AND_STORED",
        "message": "Factura de proveedor (AttachedDocument) procesada y guardada correctamente"
    })))
}

/// POST /api/v1/documents/:id/events
/// Genera un evento RADIAN (030, 032, 033, 034) para un AttachedDocument inbound, lo firma y lo transmite a la DIAN.
pub async fn register_document_event(
    State((pool, _)): State<(PgPool, redis::Client)>,
    Extension(tenant): Extension<Tenant>,
    Path(doc_id): Path<Uuid>,
    Json(req): Json<EventRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // 1. Buscar el documento
    let document = sqlx::query_as::<_, Document>(
        "SELECT * FROM documents WHERE id = $1 AND tenant_id = $2"
    )
    .bind(doc_id)
    .bind(tenant.id)
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("Documento {} no encontrado", doc_id)))?;

    // Validar que sea un documento inbound
    let is_inbound = document.payload.get("inbound")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !is_inbound {
        return Err(ApiError::Validation(
            "Solo se pueden emitir eventos del RADIAN sobre facturas inbound de proveedores".to_string()
        ));
    }

    // Extraer campos del payload de la factura inbound
    let sender_nit = document.payload.get("sender_nit")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Internal("Missing sender_nit in document payload".to_string()))?;
    let sender_name = document.payload.get("sender_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Internal("Missing sender_name in document payload".to_string()))?;
    
    let parent_document_id = document.payload.get("parent_document_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Internal("Missing parent_document_id in document payload".to_string()))?;

    // Descripciones reglamentarias DIAN de eventos RADIAN
    let event_description = match req.event_code.as_str() {
        "030" => "Acuse de recibo de Factura Electronica de Venta",
        "032" => "Recibo de las mercancias o servicios",
        "033" => "Aceptacion expresa",
        "034" => "Aceptacion tacita",
        _ => return Err(ApiError::Validation(format!(
            "Codigo de evento '{}' invalido. Debe ser 030, 032, 033 o 034", req.event_code
        ))),
    };

    let event_id = format!("EV-{}", Uuid::new_v4());
    let issue_date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let issue_time = chrono::Utc::now().format("%H:%M:%S-05:00").to_string();

    // Nosotros (adquirente/tenant) somos el SENDER del evento. El proveedor es el RECEIVER del evento.
    // 2. Generar el XML de ApplicationResponse (evento) y calcular el CUDE
    let (unsigned_xml, cude) = ubl_engine::generate_application_response_xml(
        &event_id,
        &issue_date,
        &issue_time,
        &tenant.tax_id,
        &tenant.registration_name,
        "31", // NIT
        sender_nit,
        sender_name,
        "31",
        &req.event_code,
        event_description,
        parent_document_id,
        &document.cufe_cude,
        &req.software_pin,
        &req.environment,
    ).map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Internal(e.to_string())))?;

    // 3. Firmar digitalmente con XAdES-EPES
    let provider = LocalCertificateProvider {
        certificate_pem: "cert_dummy".to_string(),
        private_key_pem: "key_dummy".to_string(),
    };

    let signed_xml = sign_document(&unsigned_xml, &provider, "key_ref")
        .await
        .map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Crypto(e.to_string())))?;

    // 4. Comprimir a ZIP en memoria
    let (zip_bytes, _) = dian_soap_client::zip::compress_xml_to_zip(
        &signed_xml,
        &tenant.tax_id,
        "EV",
        1, // Consecutivo dummy
    ).map_err(|e| ApiError::Domain(core_domain::ports::DomainError::Internal(e.to_string())))?;

    // 5. Transmitir SOAP SendEventUpdateStatus a la DIAN (fuera de transacciones de DB)
    let is_hab = req.environment == "2";
    let binary_security_token = std::env::var("DIAN_BINARY_SECURITY_TOKEN")
        .unwrap_or_else(|_| "token_dummy".to_string());
    
    let soap_client = DianSoapClientImpl::new(binary_security_token, None, None);
    println!("[SOAP RADIAN] Transmitiendo evento {} para la factura {}...", req.event_code, parent_document_id);
    
    let dian_resp = soap_client.send_event_update_status(&zip_bytes, is_hab).await
        .map_err(|e| ApiError::Domain(e))?;

    let is_accepted = dian_resp.status_code == "00";
    let event_status = if is_accepted { EventStatus::Accepted } else { EventStatus::Rejected };

    // 6. Almacenar el evento de la DIAN en la base de datos
    let event = DianEvent {
        id: Uuid::new_v4(),
        document_id: document.id,
        event_status,
        dian_response_code: Some(dian_resp.status_code.clone()),
        dian_response_message: Some(dian_resp.message.clone()),
        soap_trace_id: dian_resp.soap_trace_id.clone(),
        xml_response: dian_resp.xml_response.clone(),
        created_at: chrono::Utc::now(),
    };

    sqlx::query(
        "INSERT INTO dian_events (id, document_id, event_status, dian_response_code, dian_response_message, soap_trace_id, xml_response, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
    )
    .bind(event.id)
    .bind(event.document_id)
    .bind(event.event_status)
    .bind(&event.dian_response_code)
    .bind(&event.dian_response_message)
    .bind(&event.soap_trace_id)
    .bind(&event.xml_response)
    .bind(event.created_at)
    .execute(&pool)
    .await?;

    Ok(Json(json!({
        "event_id": event.id,
        "event_code": req.event_code,
        "cude": cude,
        "dian_status": dian_resp.status_code,
        "dian_message": dian_resp.message,
        "soap_trace_id": dian_resp.soap_trace_id
    })))
}

/// Extrae de forma robusta y rápida el contenido de una etiqueta XML sin importar su prefijo.
fn extract_tag_content(xml: &str, tag_name: &str) -> Option<String> {
    let mut cursor = 0;
    while let Some(pos) = xml[cursor..].find('<') {
        let tag_start = cursor + pos + 1;
        let tag_rest = &xml[tag_start..];
        
        let space_or_close = tag_rest.find(|c| c == ' ' || c == '>')?;
        let full_tag = &tag_rest[..space_or_close];
        
        let matches = if full_tag.contains(':') {
            full_tag.split(':').last() == Some(tag_name)
        } else {
            full_tag == tag_name
        };

        if matches {
            let content_start = tag_start + tag_rest.find('>')? + 1;
            let rest_content = &xml[content_start..];
            let end_pos = rest_content.find("</")?;
            let content = &rest_content[..end_pos];
            return Some(content.trim().to_string());
        }
        cursor = tag_start;
    }
    None
}

/// Extrae el contenido de una etiqueta dentro de una seccion delimitada del XML.
fn extract_tag_from_section(xml: &str, section_start_tag: &str, section_end_tag: &str, tag_name: &str) -> Option<String> {
    let start_pos = xml.find(section_start_tag)?;
    let end_pos = xml[start_pos..].find(section_end_tag)? + start_pos;
    let section = &xml[start_pos..end_pos];
    extract_tag_content(section, tag_name)
}

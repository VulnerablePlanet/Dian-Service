use crate::error::ApiError;
use crate::state::SharedState;
use application::radian::RadianEventRequest;
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use core_domain::models::Tenant;
use core_domain::ports::DomainError;
use serde_json::{json, Value};
use std::io::Read;
use uuid::Uuid;

/// Tamaño máximo descomprimido del XML de un AttachedDocument (protección contra bombas ZIP).
const MAX_INBOUND_XML_BYTES: u64 = 10 * 1024 * 1024;
/// Número máximo de entradas que se inspeccionan dentro del ZIP recibido.
const MAX_INBOUND_ZIP_ENTRIES: usize = 20;

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

fn validation(message: impl Into<String>) -> ApiError {
    ApiError::Domain(DomainError::Validation(message.into()))
}

/// POST /api/v1/inbound/invoices
/// Recibe el AttachedDocument ZIP de un proveedor y lo registra como documento recibido.
pub async fn receive_inbound_invoice(
    State(state): State<SharedState>,
    Extension(tenant): Extension<Tenant>,
    Json(payload): Json<InboundInvoicePayload>,
) -> Result<Json<Value>, ApiError> {
    // La verificación criptográfica de la firma aún no está implementada: sin ella no se
    // puede distinguir una factura legítima de una fabricada.
    if !state.config.inbound_allow_unverified_signatures {
        return Err(ApiError::NotImplemented(
            "El registro de facturas recibidas está deshabilitado hasta implementar la \
             verificación criptográfica de la firma del AttachedDocument"
                .to_string(),
        ));
    }

    let zip_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &payload.zip_content)
        .map_err(|e| validation(format!("Contenido ZIP Base64 invalido: {}", e)))?;
    let xml = extract_first_xml(&zip_bytes)?;
    let registered = state.inbound.register_unverified(&tenant, xml).await?;

    Ok(Json(json!({
        "document_id": registered.document_id,
        "parent_document_id": registered.parent_document_id,
        "status": "ACCEPTED_AND_STORED",
        "signature_verified": false,
        "message": "Factura de proveedor (AttachedDocument) registrada sin verificacion criptografica de firma"
    })))
}

/// POST /api/v1/documents/:id/events
/// Genera, firma y transmite un evento RADIAN (030, 032, 033, 034) sobre una factura recibida.
pub async fn register_document_event(
    State(state): State<SharedState>,
    Extension(tenant): Extension<Tenant>,
    Path(doc_id): Path<Uuid>,
    Json(req): Json<EventRequest>,
) -> Result<Json<Value>, ApiError> {
    let outcome = state
        .radian
        .register_event(
            &tenant,
            doc_id,
            RadianEventRequest {
                event_code: req.event_code,
                software_pin: req.software_pin,
                environment: req.environment,
            },
        )
        .await?;
    Ok(Json(serde_json::to_value(outcome).map_err(|e| DomainError::Internal(e.to_string()))?))
}

/// Devuelve el primer `.xml` del ZIP, limitando el tamaño descomprimido y el número de
/// entradas para que un ZIP pequeño no pueda agotar la memoria del proceso.
fn extract_first_xml(zip_bytes: &[u8]) -> Result<String, ApiError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| validation(format!("No se pudo leer el archivo ZIP: {}", e)))?;

    if archive.is_empty() {
        return Err(validation("El archivo ZIP esta vacio"));
    }
    if archive.len() > MAX_INBOUND_ZIP_ENTRIES {
        return Err(validation(format!(
            "El ZIP contiene demasiados archivos (maximo {})",
            MAX_INBOUND_ZIP_ENTRIES
        )));
    }

    for i in 0..archive.len() {
        let file = archive.by_index(i)
            .map_err(|e| validation(format!("Error leyendo elemento en ZIP: {}", e)))?;
        let is_xml = file.name().map(|name| name.ends_with(".xml")).unwrap_or(false);
        if !is_xml {
            continue;
        }
        // El tamaño declarado puede mentir: además se corta la lectura en el límite.
        if file.size() > MAX_INBOUND_XML_BYTES {
            return Err(xml_too_large());
        }
        let mut raw = Vec::new();
        file.take(MAX_INBOUND_XML_BYTES + 1)
            .read_to_end(&mut raw)
            .map_err(|e| validation(format!("Error leyendo XML del ZIP: {}", e)))?;
        if raw.len() as u64 > MAX_INBOUND_XML_BYTES {
            return Err(xml_too_large());
        }
        let xml = String::from_utf8(raw)
            .map_err(|_| validation("El XML del ZIP no es UTF-8 valido"))?;
        if xml.trim().is_empty() {
            break;
        }
        return Ok(xml);
    }

    Err(validation("No se encontro ningun archivo XML en el ZIP"))
}

fn xml_too_large() -> ApiError {
    validation(format!(
        "El XML del ZIP excede el tamano maximo permitido ({} bytes)",
        MAX_INBOUND_XML_BYTES
    ))
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (name, content) in entries {
                zip.start_file(*name, options).unwrap();
                zip.write_all(content).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extracts_first_xml_entry() {
        let zip = zip_with(&[("readme.txt", b"hola"), ("ad.xml", b"<AttachedDocument/>")]);
        assert_eq!(extract_first_xml(&zip).unwrap(), "<AttachedDocument/>");
    }

    #[test]
    fn rejects_zip_bomb() {
        let huge = vec![b' '; (MAX_INBOUND_XML_BYTES + 1024) as usize];
        let zip = zip_with(&[("bomb.xml", &huge)]);
        assert!(zip.len() < 100 * 1024, "the bomb must be small once compressed");
        let err = extract_first_xml(&zip).unwrap_err();
        assert!(err.to_string().contains("excede"));
    }

    #[test]
    fn rejects_zip_without_xml_or_with_too_many_entries() {
        assert!(extract_first_xml(&zip_with(&[("a.txt", b"x")])).is_err());
        let names: Vec<String> = (0..=MAX_INBOUND_ZIP_ENTRIES).map(|i| format!("f{}.txt", i)).collect();
        let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), b"x" as &[u8])).collect();
        assert!(extract_first_xml(&zip_with(&entries)).is_err());
    }
}

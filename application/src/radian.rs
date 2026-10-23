//! Caso de uso: eventos RADIAN sobre facturas recibidas (ApplicationResponse).
//!
//! Sigue siendo síncrono: solo se admite sobre facturas cuya firma fue verificada, y esa
//! verificación aún no existe, de modo que hoy toda solicitud termina en `Validation`.

use chrono::Utc;
use core_domain::models::{DianEvent, EventStatus, Tenant};
use core_domain::ports::{CryptoSigner, DianSoapClient, DocumentRepository, DomainError};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RadianEventRequest {
    pub event_code: String,
    pub software_pin: String,
    pub environment: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RadianEventOutcome {
    pub event_id: Uuid,
    pub event_code: String,
    pub cude: String,
    pub dian_status: String,
    pub dian_message: String,
    pub soap_trace_id: Option<String>,
}

pub struct RadianService {
    documents: Arc<dyn DocumentRepository>,
    signer: Arc<dyn CryptoSigner>,
    soap: Arc<dyn DianSoapClient>,
}

/// Descripción reglamentaria de cada evento RADIAN soportado.
fn event_description(code: &str) -> Option<&'static str> {
    match code {
        "030" => Some("Acuse de recibo de Factura Electronica de Venta"),
        "032" => Some("Recibo de las mercancias o servicios"),
        "033" => Some("Aceptacion expresa"),
        "034" => Some("Aceptacion tacita"),
        _ => None,
    }
}

impl RadianService {
    pub fn new(documents: Arc<dyn DocumentRepository>, signer: Arc<dyn CryptoSigner>, soap: Arc<dyn DianSoapClient>) -> Self {
        Self { documents, signer, soap }
    }

    pub async fn register_event(
        &self,
        tenant: &Tenant,
        document_id: Uuid,
        request: RadianEventRequest,
    ) -> Result<RadianEventOutcome, DomainError> {
        let document = self
            .documents
            .find_tenant_document(tenant.id, document_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("Documento {} no encontrado", document_id)))?;

        let payload = &document.payload;
        if payload.get("inbound").and_then(|v| v.as_bool()) != Some(true) {
            return Err(DomainError::Validation(
                "Solo se pueden emitir eventos del RADIAN sobre facturas inbound de proveedores".to_string(),
            ));
        }
        // Un evento RADIAN tiene efectos de título valor: no se emite sobre una factura cuya
        // firma no fue verificada criptográficamente.
        if payload.get("signature_verified").and_then(|v| v.as_bool()) != Some(true) {
            return Err(DomainError::Validation(
                "No se pueden emitir eventos RADIAN sobre una factura cuya firma no fue verificada".to_string(),
            ));
        }

        let field = |name: &str| {
            payload
                .get(name)
                .and_then(|v| v.as_str())
                .ok_or_else(|| DomainError::Internal(format!("Missing {} in document payload", name)))
        };
        let sender_nit = field("sender_nit")?;
        let sender_name = field("sender_name")?;
        let parent_document_id = field("parent_document_id")?;
        let description = event_description(&request.event_code).ok_or_else(|| {
            DomainError::Validation(format!(
                "Codigo de evento '{}' invalido. Debe ser 030, 032, 033 o 034",
                request.event_code
            ))
        })?;

        let event_id = format!("EV-{}", Uuid::new_v4());
        let (issue_date, issue_time) = core_domain::time::colombia_date_time(Utc::now());
        // El tenant (adquirente) es el emisor del evento y el proveedor, su receptor.
        let (unsigned_xml, cude) = ubl_engine::generate_application_response_xml(
            &event_id,
            &issue_date,
            &issue_time,
            &tenant.tax_id,
            &tenant.registration_name,
            "31",
            sender_nit,
            sender_name,
            "31",
            &request.event_code,
            description,
            parent_document_id,
            &document.cufe_cude,
            &request.software_pin,
            &request.environment,
        )
        .map_err(|e| DomainError::Internal(e.to_string()))?;

        let signed_xml = self.signer.sign_for_tenant(tenant.id, &unsigned_xml).await?;
        // TODO(F4): consecutivo real de eventos; hoy el nombre del ZIP usa 1 para todos.
        let (zip, _) = ubl_engine::packaging::compress_xml_to_zip(&signed_xml, &tenant.tax_id, "EV", 1)
            .map_err(|e| DomainError::Internal(e.to_string()))?;
        let response = self
            .soap
            .send_event_update_status(tenant.id, &zip, request.environment == "2")
            .await?;

        let accepted = response.status_code == "00";
        let event = DianEvent {
            soap_trace_id: response.soap_trace_id.clone(),
            xml_response: response.xml_response.clone(),
            ..DianEvent::new(
                document.id,
                if accepted { EventStatus::Accepted } else { EventStatus::Rejected },
                &response.status_code,
                response.message.clone(),
            )
        };
        self.documents.record_event(&event).await?;

        Ok(RadianEventOutcome {
            event_id: event.id,
            event_code: request.event_code,
            cude,
            dian_status: response.status_code,
            dian_message: response.message,
            soap_trace_id: response.soap_trace_id,
        })
    }
}

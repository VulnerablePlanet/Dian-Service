//! Caso de uso: emisión de documentos (factura, nómina, documento soporte).

use core_domain::jobs::NewJob;
use core_domain::models::{DocumentType, Tenant};
use core_domain::ports::{
    DocumentRepository, DomainError, IdempotencyKey, IssueOutcome, NewOutboundDocument,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use ubl_engine::error::UblError;
use ubl_engine::payload::{InvoicePayload, PayrollPayload, SupportDocumentPayload};
use uuid::Uuid;

/// Longitud máxima admitida para una clave de idempotencia.
pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 255;

pub struct IssueService {
    documents: Arc<dyn DocumentRepository>,
    transmit_max_attempts: i32,
}

fn validation(e: UblError) -> DomainError {
    DomainError::Validation(e.to_string())
}

fn require_issuer(tenant: &Tenant, nit: &str, role: &str) -> Result<(), DomainError> {
    if nit != tenant.tax_id {
        return Err(DomainError::Forbidden(format!(
            "El NIT del {} no coincide con el del tenant autenticado",
            role
        )));
    }
    Ok(())
}

/// Huella SHA-256 (hex) del tipo de documento y del cuerpo normalizado de la solicitud. Los
/// objetos JSON de `serde_json` ordenan sus claves, así que el orden de los campos enviados
/// por el cliente no altera la huella.
fn request_hash(document_type: DocumentType, payload: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(document_type.as_str().as_bytes());
    hasher.update(b"\n");
    hasher.update(payload.to_string().as_bytes());
    hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

fn idempotency(
    key: Option<String>,
    document_type: DocumentType,
    payload: &serde_json::Value,
) -> Result<Option<IdempotencyKey>, DomainError> {
    let Some(key) = key else { return Ok(None) };
    let key = key.trim().to_string();
    if key.is_empty() || key.len() > MAX_IDEMPOTENCY_KEY_LEN || !key.is_ascii() {
        return Err(DomainError::Validation(format!(
            "Idempotency-Key debe tener entre 1 y {} caracteres ASCII",
            MAX_IDEMPOTENCY_KEY_LEN
        )));
    }
    Ok(Some(IdempotencyKey {
        key,
        request_hash: request_hash(document_type, payload),
    }))
}

impl IssueService {
    pub fn new(documents: Arc<dyn DocumentRepository>, transmit_max_attempts: i32) -> Self {
        Self {
            documents,
            transmit_max_attempts,
        }
    }

    async fn issue<P: Serialize>(
        &self,
        tenant: &Tenant,
        document_type: DocumentType,
        prefix: &str,
        requested_number: Option<i32>,
        payload: &P,
        idempotency_key: Option<String>,
    ) -> Result<IssueOutcome, DomainError> {
        let payload = serde_json::to_value(payload).map_err(|e| DomainError::Internal(e.to_string()))?;
        let idempotency = idempotency(idempotency_key, document_type, &payload)?;
        let id = Uuid::new_v4();
        self.documents
            .issue_outbound(
                NewOutboundDocument {
                    id,
                    tenant_id: tenant.id,
                    document_type,
                    prefix: prefix.to_string(),
                    requested_number,
                    payload,
                    idempotency,
                },
                NewJob::transmit(id, self.transmit_max_attempts),
            )
            .await
    }

    /// El cliente propone el consecutivo y debe ser exactamente el siguiente de la resolución.
    pub async fn issue_invoice(
        &self,
        tenant: &Tenant,
        payload: InvoicePayload,
        idempotency_key: Option<String>,
    ) -> Result<IssueOutcome, DomainError> {
        payload.validate().map_err(validation)?;
        require_issuer(tenant, &payload.company_nit, "emisor")?;
        self.issue(tenant, DocumentType::Invoice, &payload.prefix, Some(payload.number), &payload, idempotency_key)
            .await
    }

    pub async fn issue_payroll(
        &self,
        tenant: &Tenant,
        payload: PayrollPayload,
        idempotency_key: Option<String>,
    ) -> Result<IssueOutcome, DomainError> {
        payload.validate().map_err(validation)?;
        require_issuer(tenant, &payload.employer_nit, "empleador")?;
        self.issue(tenant, DocumentType::Payroll, &payload.prefix, Some(payload.number), &payload, idempotency_key)
            .await
    }

    /// El servidor asigna el consecutivo; el `number` del cuerpo se ignora.
    pub async fn issue_support_document(
        &self,
        tenant: &Tenant,
        payload: SupportDocumentPayload,
        idempotency_key: Option<String>,
    ) -> Result<IssueOutcome, DomainError> {
        payload.validate().map_err(validation)?;
        require_issuer(tenant, &payload.buyer_nit, "comprador")?;
        self.issue(tenant, DocumentType::SupportDocument, &payload.prefix, None, &payload, idempotency_key)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use core_domain::models::{DocumentStatus, JobKind};
    use core_domain::testing::InMemoryStore;

    fn service(store: &Arc<InMemoryStore>) -> IssueService {
        IssueService::new(store.clone(), 5)
    }

    #[tokio::test]
    async fn invoice_is_stored_pending_with_a_transmit_job_and_no_technical_key() {
        let (store, tenant) = store_with_tenant();
        store.add_range(range(&tenant, DocumentType::Invoice, "FE", 1));
        let mut payload = invoice_payload(1);
        payload.technical_key = Some("client-supplied-secret".into());

        let outcome = service(&store).issue_invoice(&tenant, payload, None).await.unwrap();
        let IssueOutcome::Created(issued) = outcome else { panic!("expected Created") };

        let doc = store.document(issued.document_id).unwrap();
        assert_eq!(doc.status, DocumentStatus::Pending);
        assert_eq!(doc.document_number, 1);
        assert!(doc.payload.get("technical_key").is_none(), "secrets must not be persisted");
        let jobs = store.jobs_for(issued.document_id);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, JobKind::Transmit);
    }

    #[tokio::test]
    async fn foreign_issuer_is_forbidden() {
        let (store, tenant) = store_with_tenant();
        store.add_range(range(&tenant, DocumentType::Invoice, "FE", 1));
        let mut payload = invoice_payload(1);
        payload.company_nit = "811111111".into();
        assert!(matches!(
            service(&store).issue_invoice(&tenant, payload, None).await,
            Err(DomainError::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn idempotent_retry_returns_the_same_document() {
        let (store, tenant) = store_with_tenant();
        store.add_range(range(&tenant, DocumentType::Invoice, "FE", 1));
        let svc = service(&store);

        let first = svc.issue_invoice(&tenant, invoice_payload(1), Some("k1".into())).await.unwrap();
        let again = svc.issue_invoice(&tenant, invoice_payload(1), Some("k1".into())).await.unwrap();
        let (IssueOutcome::Created(a), IssueOutcome::Replayed(b)) = (first, again) else {
            panic!("expected Created then Replayed")
        };
        assert_eq!(a, b);

        let mut different = invoice_payload(1);
        different.customer_name = "Otro".into();
        assert!(matches!(
            svc.issue_invoice(&tenant, different, Some("k1".into())).await,
            Err(DomainError::Validation(_))
        ));
    }

    #[tokio::test]
    async fn support_document_number_is_assigned_by_the_server() {
        let (store, tenant) = store_with_tenant();
        store.add_range(range(&tenant, DocumentType::SupportDocument, "DS", 5000));
        let mut payload = support_payload();
        payload.number = 1;
        let IssueOutcome::Created(issued) =
            service(&store).issue_support_document(&tenant, payload, None).await.unwrap()
        else {
            panic!("expected Created")
        };
        assert_eq!(issued.document_number, 5000);
    }

    #[tokio::test]
    async fn invalid_idempotency_key_is_rejected() {
        let (store, tenant) = store_with_tenant();
        store.add_range(range(&tenant, DocumentType::Invoice, "FE", 1));
        let long = "x".repeat(MAX_IDEMPOTENCY_KEY_LEN + 1);
        assert!(service(&store).issue_invoice(&tenant, invoice_payload(1), Some(long)).await.is_err());
    }

    #[test]
    fn request_hash_ignores_field_order() {
        let a: serde_json::Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        assert_eq!(request_hash(DocumentType::Invoice, &a), request_hash(DocumentType::Invoice, &b));
        assert_ne!(request_hash(DocumentType::Invoice, &a), request_hash(DocumentType::Payroll, &a));
    }
}

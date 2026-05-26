use crate::models::{
    Certificate, DateTime, DianEvent, Document, DocumentType, NumberingRange, Tenant, Webhook,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("Entity not found: {0}")]
    NotFound(String),

    #[error("Conflict error: {0}")]
    Conflict(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Cryptographic signing error: {0}")]
    Crypto(String),

    #[error("SOAP communication error: {0}")]
    Soap(String),

    #[error("Queue/Broker error: {0}")]
    Queue(String),

    #[error("Internal/Unexpected error: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DianResponse {
    pub status_code: String,
    pub message: String,
    pub xml_response: Option<String>,
    pub soap_trace_id: Option<String>,
}

#[async_trait]
pub trait DocumentRepository: Send + Sync {
    async fn save_tenant(&self, tenant: &Tenant) -> Result<(), DomainError>;
    async fn find_tenant_by_id(&self, id: Uuid) -> Result<Option<Tenant>, DomainError>;

    async fn save_certificate(&self, cert: &Certificate) -> Result<(), DomainError>;
    async fn find_certificate_by_id(&self, id: Uuid) -> Result<Option<Certificate>, DomainError>;
    async fn find_active_certificate_by_tenant(&self, tenant_id: Uuid) -> Result<Option<Certificate>, DomainError>;

    async fn save_numbering_range(&self, range: &NumberingRange) -> Result<(), DomainError>;
    async fn find_numbering_range_by_id(&self, id: Uuid) -> Result<Option<NumberingRange>, DomainError>;
    async fn increment_numbering_range(&self, id: Uuid) -> Result<i32, DomainError>;

    async fn save_document(&self, doc: &Document) -> Result<(), DomainError>;
    async fn find_document_by_id(&self, id: Uuid) -> Result<Option<Document>, DomainError>;
    async fn find_document_by_number(
        &self,
        tenant_id: Uuid,
        doc_type: DocumentType,
        prefix: &str,
        number: i32,
    ) -> Result<Option<Document>, DomainError>;

    async fn save_dian_event(&self, event: &DianEvent) -> Result<(), DomainError>;
    async fn find_dian_events_by_document_id(&self, doc_id: Uuid) -> Result<Vec<DianEvent>, DomainError>;

    async fn save_webhook(&self, webhook: &Webhook) -> Result<(), DomainError>;
    async fn find_webhooks_by_tenant_id(&self, tenant_id: Uuid) -> Result<Vec<Webhook>, DomainError>;
}

#[async_trait]
pub trait CryptoSigner: Send + Sync {
    async fn sign_xml(
        &self,
        xml_content: &str,
        vault_reference_path: &str,
    ) -> Result<String, DomainError>;
}

#[async_trait]
pub trait DianSoapClient: Send + Sync {
    async fn send_bill_sync(
        &self,
        signed_xml_zip: &[u8],
        is_hab: bool,
    ) -> Result<DianResponse, DomainError>;

    async fn send_bill_async(
        &self,
        signed_xml_zip: &[u8],
        is_hab: bool,
    ) -> Result<String, DomainError>; // Returns TrackId

    async fn send_test_set_async(
        &self,
        signed_xml_zip: &[u8],
        test_set_id: &str,
        is_hab: bool,
    ) -> Result<String, DomainError>; // Returns zipKey/TrackId

    async fn get_status(
        &self,
        track_id: &str,
        is_hab: bool,
    ) -> Result<DianResponse, DomainError>;

    async fn get_status_zip(
        &self,
        track_id: &str,
        is_hab: bool,
    ) -> Result<DianResponse, DomainError>;

    async fn send_event_update_status(
        &self,
        signed_event_zip: &[u8],
        is_hab: bool,
    ) -> Result<DianResponse, DomainError>;
}

#[async_trait]
pub trait QueuePublisher: Send + Sync {
    async fn publish_document_job(&self, document_id: Uuid) -> Result<(), DomainError>;
}

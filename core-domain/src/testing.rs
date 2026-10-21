//! Repositorio en memoria para pruebas. Solo se compila con la feature `testing`.

use crate::models::{
    Certificate, DianEvent, Document, DocumentType, NumberingRange, Tenant, Webhook,
};
use crate::ports::{DocumentRepository, DomainError};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use uuid::Uuid;

#[derive(Default)]
pub struct InMemoryRepository {
    pub tenants: Mutex<HashMap<Uuid, Tenant>>,
    pub certificates: Mutex<HashMap<Uuid, Certificate>>,
    pub ranges: Mutex<HashMap<Uuid, NumberingRange>>,
    pub documents: Mutex<HashMap<Uuid, Document>>,
    pub events: Mutex<Vec<DianEvent>>,
    pub webhooks: Mutex<Vec<Webhook>>,
}

impl InMemoryRepository {
    pub fn with_document(document: Document) -> Self {
        let repo = Self::default();
        repo.documents.lock().unwrap().insert(document.id, document);
        repo
    }

    pub fn document(&self, id: Uuid) -> Option<Document> {
        self.documents.lock().unwrap().get(&id).cloned()
    }

    pub fn events_for(&self, document_id: Uuid) -> Vec<DianEvent> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.document_id == document_id)
            .cloned()
            .collect()
    }
}

#[async_trait]
impl DocumentRepository for InMemoryRepository {
    async fn save_tenant(&self, tenant: &Tenant) -> Result<(), DomainError> {
        self.tenants.lock().unwrap().insert(tenant.id, tenant.clone());
        Ok(())
    }

    async fn find_tenant_by_id(&self, id: Uuid) -> Result<Option<Tenant>, DomainError> {
        Ok(self.tenants.lock().unwrap().get(&id).cloned())
    }

    async fn save_certificate(&self, cert: &Certificate) -> Result<(), DomainError> {
        self.certificates.lock().unwrap().insert(cert.id, cert.clone());
        Ok(())
    }

    async fn find_certificate_by_id(&self, id: Uuid) -> Result<Option<Certificate>, DomainError> {
        Ok(self.certificates.lock().unwrap().get(&id).cloned())
    }

    async fn find_active_certificate_by_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<Option<Certificate>, DomainError> {
        Ok(self
            .certificates
            .lock()
            .unwrap()
            .values()
            .find(|c| c.tenant_id == tenant_id && c.status == crate::models::CertificateStatus::Active)
            .cloned())
    }

    async fn save_numbering_range(&self, range: &NumberingRange) -> Result<(), DomainError> {
        self.ranges.lock().unwrap().insert(range.id, range.clone());
        Ok(())
    }

    async fn find_numbering_range_by_id(&self, id: Uuid) -> Result<Option<NumberingRange>, DomainError> {
        Ok(self.ranges.lock().unwrap().get(&id).cloned())
    }

    async fn increment_numbering_range(&self, id: Uuid) -> Result<i32, DomainError> {
        let mut ranges = self.ranges.lock().unwrap();
        let range = ranges
            .get_mut(&id)
            .ok_or_else(|| DomainError::NotFound(format!("range {}", id)))?;
        range.current_counter += 1;
        Ok(range.current_counter)
    }

    async fn save_document(&self, doc: &Document) -> Result<(), DomainError> {
        self.documents.lock().unwrap().insert(doc.id, doc.clone());
        Ok(())
    }

    async fn find_document_by_id(&self, id: Uuid) -> Result<Option<Document>, DomainError> {
        Ok(self.document(id))
    }

    async fn find_document_by_number(
        &self,
        tenant_id: Uuid,
        doc_type: DocumentType,
        prefix: &str,
        number: i32,
    ) -> Result<Option<Document>, DomainError> {
        Ok(self
            .documents
            .lock()
            .unwrap()
            .values()
            .find(|d| {
                d.tenant_id == tenant_id
                    && d.document_type == doc_type
                    && d.prefix == prefix
                    && d.document_number == number
            })
            .cloned())
    }

    async fn save_dian_event(&self, event: &DianEvent) -> Result<(), DomainError> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }

    async fn find_dian_events_by_document_id(&self, doc_id: Uuid) -> Result<Vec<DianEvent>, DomainError> {
        Ok(self.events_for(doc_id))
    }

    async fn save_webhook(&self, webhook: &Webhook) -> Result<(), DomainError> {
        self.webhooks.lock().unwrap().push(webhook.clone());
        Ok(())
    }

    async fn find_webhooks_by_tenant_id(&self, tenant_id: Uuid) -> Result<Vec<Webhook>, DomainError> {
        Ok(self
            .webhooks
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w.tenant_id == tenant_id)
            .cloned()
            .collect())
    }
}

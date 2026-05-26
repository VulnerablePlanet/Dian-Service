use crate::models::{
    Certificate, DianEvent, Document, DocumentType, NumberingRange, Tenant, Webhook,
};
use crate::ports::{DocumentRepository, DomainError};
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

pub struct PgDocumentRepository {
    pool: PgPool,
}

impl PgDocumentRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DocumentRepository for PgDocumentRepository {
    async fn save_tenant(&self, tenant: &Tenant) -> Result<(), DomainError> {
        sqlx::query(
            "INSERT INTO tenants (id, tax_id, registration_name, tax_regime, address_info, reception_email, api_keys, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (id) DO UPDATE 
             SET tax_id = EXCLUDED.tax_id, registration_name = EXCLUDED.registration_name, tax_regime = EXCLUDED.tax_regime, 
                 address_info = EXCLUDED.address_info, reception_email = EXCLUDED.reception_email, api_keys = EXCLUDED.api_keys, 
                 updated_at = CURRENT_TIMESTAMP"
        )
        .bind(tenant.id)
        .bind(&tenant.tax_id)
        .bind(&tenant.registration_name)
        .bind(&tenant.tax_regime)
        .bind(&tenant.address_info)
        .bind(&tenant.reception_email)
        .bind(&tenant.api_keys)
        .bind(tenant.created_at)
        .bind(tenant.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| DomainError::Database(e.to_string()))?;
        Ok(())
    }

    async fn find_tenant_by_id(&self, id: Uuid) -> Result<Option<Tenant>, DomainError> {
        sqlx::query_as::<_, Tenant>("SELECT * FROM tenants WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| DomainError::Database(e.to_string()))
    }

    async fn save_certificate(&self, cert: &Certificate) -> Result<(), DomainError> {
        sqlx::query(
            "INSERT INTO certificates (id, tenant_id, vault_reference_path, thumbprint, expiration_date, status)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (id) DO UPDATE 
             SET vault_reference_path = EXCLUDED.vault_reference_path, thumbprint = EXCLUDED.thumbprint, 
                 expiration_date = EXCLUDED.expiration_date, status = EXCLUDED.status"
        )
        .bind(cert.id)
        .bind(cert.tenant_id)
        .bind(&cert.vault_reference_path)
        .bind(&cert.thumbprint)
        .bind(cert.expiration_date)
        .bind(&cert.status)
        .execute(&self.pool)
        .await
        .map_err(|e| DomainError::Database(e.to_string()))?;
        Ok(())
    }

    async fn find_certificate_by_id(&self, id: Uuid) -> Result<Option<Certificate>, DomainError> {
        sqlx::query_as::<_, Certificate>("SELECT * FROM certificates WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| DomainError::Database(e.to_string()))
    }

    async fn find_active_certificate_by_tenant(&self, tenant_id: Uuid) -> Result<Option<Certificate>, DomainError> {
        sqlx::query_as::<_, Certificate>(
            "SELECT * FROM certificates WHERE tenant_id = $1 AND status = 'active' LIMIT 1"
        )
        .bind(tenant_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DomainError::Database(e.to_string()))
    }

    async fn save_numbering_range(&self, range: &NumberingRange) -> Result<(), DomainError> {
        sqlx::query(
            "INSERT INTO numbering_ranges (id, tenant_id, document_type, prefix, from_number, to_number, technical_key, valid_date_from, valid_date_to, current_counter)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (id) DO UPDATE 
             SET from_number = EXCLUDED.from_number, to_number = EXCLUDED.to_number, technical_key = EXCLUDED.technical_key, 
                 valid_date_from = EXCLUDED.valid_date_from, valid_date_to = EXCLUDED.valid_date_to, current_counter = EXCLUDED.current_counter"
        )
        .bind(range.id)
        .bind(range.tenant_id)
        .bind(&range.document_type)
        .bind(&range.prefix)
        .bind(range.from_number)
        .bind(range.to_number)
        .bind(&range.technical_key)
        .bind(range.valid_date_from)
        .bind(range.valid_date_to)
        .bind(range.current_counter)
        .execute(&self.pool)
        .await
        .map_err(|e| DomainError::Database(e.to_string()))?;
        Ok(())
    }

    async fn find_numbering_range_by_id(&self, id: Uuid) -> Result<Option<NumberingRange>, DomainError> {
        sqlx::query_as::<_, NumberingRange>("SELECT * FROM numbering_ranges WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| DomainError::Database(e.to_string()))
    }

    async fn increment_numbering_range(&self, id: Uuid) -> Result<i32, DomainError> {
        let row = sqlx::query(
            "UPDATE numbering_ranges 
             SET current_counter = current_counter + 1 
             WHERE id = $1 
             RETURNING current_counter"
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| DomainError::Database(e.to_string()))?;
        use sqlx::Row;
        Ok(row.get::<i32, _>("current_counter"))
    }

    async fn save_document(&self, doc: &Document) -> Result<(), DomainError> {
        sqlx::query(
            "INSERT INTO documents (id, tenant_id, document_type, prefix, document_number, cufe_cude, payload, original_xml, signed_xml, pdf_url, status, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             ON CONFLICT (id) DO UPDATE 
             SET cufe_cude = EXCLUDED.cufe_cude, payload = EXCLUDED.payload, original_xml = EXCLUDED.original_xml, 
                 signed_xml = EXCLUDED.signed_xml, pdf_url = EXCLUDED.pdf_url, status = EXCLUDED.status"
        )
        .bind(doc.id)
        .bind(doc.tenant_id)
        .bind(&doc.document_type)
        .bind(&doc.prefix)
        .bind(doc.document_number)
        .bind(&doc.cufe_cude)
        .bind(&doc.payload)
        .bind(&doc.original_xml)
        .bind(&doc.signed_xml)
        .bind(&doc.pdf_url)
        .bind(&doc.status)
        .bind(doc.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| DomainError::Database(e.to_string()))?;
        Ok(())
    }

    async fn find_document_by_id(&self, id: Uuid) -> Result<Option<Document>, DomainError> {
        sqlx::query_as::<_, Document>("SELECT * FROM documents WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| DomainError::Database(e.to_string()))
    }

    async fn find_document_by_number(
        &self,
        tenant_id: Uuid,
        doc_type: DocumentType,
        prefix: &str,
        number: i32,
    ) -> Result<Option<Document>, DomainError> {
        sqlx::query_as::<_, Document>(
            "SELECT * FROM documents 
             WHERE tenant_id = $1 AND document_type = $2 AND prefix = $3 AND document_number = $4"
        )
        .bind(tenant_id)
        .bind(doc_type)
        .bind(prefix)
        .bind(number)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DomainError::Database(e.to_string()))
    }

    async fn save_dian_event(&self, event: &DianEvent) -> Result<(), DomainError> {
        sqlx::query(
            "INSERT INTO dian_events (id, document_id, event_status, dian_response_code, dian_response_message, soap_trace_id, xml_response, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
        )
        .bind(event.id)
        .bind(event.document_id)
        .bind(&event.event_status)
        .bind(&event.dian_response_code)
        .bind(&event.dian_response_message)
        .bind(&event.soap_trace_id)
        .bind(&event.xml_response)
        .bind(event.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| DomainError::Database(e.to_string()))?;
        Ok(())
    }

    async fn find_dian_events_by_document_id(&self, doc_id: Uuid) -> Result<Vec<DianEvent>, DomainError> {
        sqlx::query_as::<_, DianEvent>(
            "SELECT * FROM dian_events WHERE document_id = $1 ORDER BY created_at ASC"
        )
        .bind(doc_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DomainError::Database(e.to_string()))
    }

    async fn save_webhook(&self, webhook: &Webhook) -> Result<(), DomainError> {
        sqlx::query(
            "INSERT INTO webhooks (id, tenant_id, target_url, webhook_secret, event_type, subscription_status)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (id) DO UPDATE 
             SET target_url = EXCLUDED.target_url, webhook_secret = EXCLUDED.webhook_secret, 
                 event_type = EXCLUDED.event_type, subscription_status = EXCLUDED.subscription_status"
        )
        .bind(webhook.id)
        .bind(webhook.tenant_id)
        .bind(&webhook.target_url)
        .bind(&webhook.webhook_secret)
        .bind(&webhook.event_type)
        .bind(&webhook.subscription_status)
        .execute(&self.pool)
        .await
        .map_err(|e| DomainError::Database(e.to_string()))?;
        Ok(())
    }

    async fn find_webhooks_by_tenant_id(&self, tenant_id: Uuid) -> Result<Vec<Webhook>, DomainError> {
        sqlx::query_as::<_, Webhook>("SELECT * FROM webhooks WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DomainError::Database(e.to_string()))
    }
}

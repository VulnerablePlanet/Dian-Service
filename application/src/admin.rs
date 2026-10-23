//! Caso de uso: administración de la plataforma (alta de tenants, API keys, certificados y
//! rangos de numeración; reintento de la cola de errores). Toda acción que modifica datos
//! queda en la bitácora administrativa en la misma transacción.

use chrono::{DateTime, Utc};
use core_domain::admin::{ApiKeyRecord, AuditEntry, DeadDelivery, DeadJob, NewTenant};
use core_domain::api_keys::{hash, KEY_PREFIX, VISIBLE_PREFIX_LEN};
use core_domain::models::{Certificate, CertificateStatus, DocumentType, NumberingRange, Tenant};
use core_domain::ports::{AdminRepository, DomainError};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

/// Máximo de elementos en los listados de la cola de errores.
pub const MAX_LIST_LIMIT: i64 = 200;

fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::Validation(message.into())
}

fn text(field: &str, value: &str, max: usize) -> Result<String, DomainError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max {
        return Err(invalid(format!("{} debe tener entre 1 y {} caracteres", field, max)));
    }
    Ok(value.to_string())
}

fn audit(action: &'static str, tenant_id: Option<Uuid>, subject_id: Option<Uuid>, details: serde_json::Value) -> AuditEntry {
    AuditEntry {
        action,
        tenant_id,
        subject_id,
        details,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTenant {
    /// NIT sin puntos ni dígito de verificación.
    pub tax_id: String,
    pub registration_name: String,
    pub tax_regime: String,
    pub reception_email: String,
}

/// API key recién creada: la clave solo se entrega aquí.
#[derive(Debug, Clone, Serialize)]
pub struct IssuedApiKey {
    pub id: Uuid,
    pub name: String,
    pub prefix: String,
    pub key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegisterCertificate {
    /// Ruta KV v2 del secreto con `certificate` y `transit_key` (o `private_key`, legado).
    pub vault_reference_path: String,
    pub thumbprint: String,
    pub expiration_date: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegisterNumberingRange {
    pub document_type: String,
    pub prefix: String,
    pub from_number: i32,
    pub to_number: i32,
    /// Clave técnica de la resolución (interviene en el CUFE de las facturas).
    #[serde(default)]
    pub technical_key: String,
    pub valid_date_from: DateTime<Utc>,
    pub valid_date_to: DateTime<Utc>,
    /// Último consecutivo ya emitido con este rango (p. ej. por el software anterior).
    #[serde(default)]
    pub last_issued_number: Option<i32>,
}

pub struct AdminService {
    repository: Arc<dyn AdminRepository>,
}

impl AdminService {
    pub fn new(repository: Arc<dyn AdminRepository>) -> Self {
        Self { repository }
    }

    async fn tenant(&self, tenant_id: Uuid) -> Result<Tenant, DomainError> {
        self.repository
            .find_tenant(tenant_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("Tenant {} no encontrado", tenant_id)))
    }

    pub async fn create_tenant(&self, request: CreateTenant) -> Result<Tenant, DomainError> {
        let tax_id = request.tax_id.trim().to_string();
        if !(5..=15).contains(&tax_id.len()) || !tax_id.chars().all(|c| c.is_ascii_digit()) {
            return Err(invalid("tax_id debe ser el NIT numerico sin digito de verificacion"));
        }
        let reception_email = text("reception_email", &request.reception_email, 255)?;
        if !reception_email.contains('@') {
            return Err(invalid("reception_email no es un correo valido"));
        }
        let tenant = NewTenant {
            id: Uuid::new_v4(),
            tax_id: tax_id.clone(),
            registration_name: text("registration_name", &request.registration_name, 255)?,
            tax_regime: text("tax_regime", &request.tax_regime, 100)?,
            reception_email,
        };
        let entry = audit("tenant.created", Some(tenant.id), Some(tenant.id), json!({ "tax_id": tax_id }));
        self.repository.create_tenant(tenant, &entry).await
    }

    pub async fn get_tenant(&self, tenant_id: Uuid) -> Result<Tenant, DomainError> {
        self.tenant(tenant_id).await
    }

    pub async fn create_api_key(&self, tenant_id: Uuid, name: &str) -> Result<IssuedApiKey, DomainError> {
        self.tenant(tenant_id).await?;
        let name = text("name", name, 100)?;
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        let key = format!("{}{}", KEY_PREFIX, secret.iter().map(|b| format!("{:02x}", b)).collect::<String>());
        let record = ApiKeyRecord {
            id: Uuid::new_v4(),
            tenant_id,
            name: name.clone(),
            prefix: key[..VISIBLE_PREFIX_LEN].to_string(),
            key_hash: hash(&key),
            created_at: Utc::now(),
            revoked_at: None,
        };
        let issued = IssuedApiKey {
            id: record.id,
            name,
            prefix: record.prefix.clone(),
            key,
        };
        let entry = audit(
            "api_key.created",
            Some(tenant_id),
            Some(record.id),
            json!({ "name": issued.name, "prefix": issued.prefix }),
        );
        self.repository.create_api_key(record, &entry).await?;
        Ok(issued)
    }

    pub async fn list_api_keys(&self, tenant_id: Uuid) -> Result<Vec<ApiKeyRecord>, DomainError> {
        self.tenant(tenant_id).await?;
        self.repository.list_api_keys(tenant_id).await
    }

    pub async fn revoke_api_key(&self, tenant_id: Uuid, key_id: Uuid) -> Result<(), DomainError> {
        let entry = audit("api_key.revoked", Some(tenant_id), Some(key_id), json!({}));
        if self.repository.revoke_api_key(tenant_id, key_id, &entry).await? {
            Ok(())
        } else {
            Err(DomainError::NotFound(format!("API key {} activa no encontrada", key_id)))
        }
    }

    pub async fn register_certificate(&self, tenant_id: Uuid, request: RegisterCertificate) -> Result<Certificate, DomainError> {
        self.tenant(tenant_id).await?;
        let path = request.vault_reference_path.trim();
        let valid_path = !path.is_empty()
            && path.len() <= 500
            && path.split('/').all(|segment| {
                !segment.is_empty()
                    && segment != "."
                    && segment != ".."
                    && segment.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            });
        if !valid_path {
            return Err(invalid("vault_reference_path no es una ruta de Vault valida"));
        }
        if request.expiration_date <= Utc::now() {
            return Err(invalid("El certificado ya esta vencido"));
        }
        let certificate = Certificate {
            id: Uuid::new_v4(),
            tenant_id,
            vault_reference_path: path.to_string(),
            thumbprint: text("thumbprint", &request.thumbprint, 255)?,
            expiration_date: request.expiration_date,
            status: CertificateStatus::Active,
        };
        let entry = audit(
            "certificate.activated",
            Some(tenant_id),
            Some(certificate.id),
            json!({ "thumbprint": certificate.thumbprint, "expiration_date": certificate.expiration_date }),
        );
        self.repository.activate_certificate(certificate.clone(), &entry).await?;
        Ok(certificate)
    }

    pub async fn register_numbering_range(
        &self,
        tenant_id: Uuid,
        request: RegisterNumberingRange,
    ) -> Result<NumberingRange, DomainError> {
        self.tenant(tenant_id).await?;
        let document_type: DocumentType = request
            .document_type
            .parse()
            .map_err(|_| invalid(format!("document_type '{}' no es valido", request.document_type)))?;
        let prefix = request.prefix.trim().to_string();
        if prefix.len() > 4 || !prefix.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(invalid("prefix admite hasta 4 caracteres alfanumericos"));
        }
        if request.from_number < 1 || request.to_number < request.from_number {
            return Err(invalid("El rango debe cumplir 1 <= from_number <= to_number"));
        }
        if request.valid_date_from >= request.valid_date_to {
            return Err(invalid("valid_date_from debe ser anterior a valid_date_to"));
        }
        let technical_key = request.technical_key.trim().to_string();
        if document_type == DocumentType::Invoice && technical_key.is_empty() {
            return Err(invalid("Las facturas requieren la technical_key de la resolucion"));
        }
        if technical_key.len() > 255 {
            return Err(invalid("technical_key es demasiado larga"));
        }
        let current_counter = match request.last_issued_number {
            None => 0,
            Some(n) if n >= request.from_number - 1 && n <= request.to_number => n,
            Some(_) => return Err(invalid("last_issued_number debe estar dentro del rango")),
        };
        let range = NumberingRange {
            id: Uuid::new_v4(),
            tenant_id,
            document_type,
            prefix,
            from_number: request.from_number,
            to_number: request.to_number,
            technical_key,
            valid_date_from: request.valid_date_from,
            valid_date_to: request.valid_date_to,
            current_counter,
        };
        // La clave técnica no se registra en la bitácora.
        let entry = audit(
            "numbering_range.created",
            Some(tenant_id),
            Some(range.id),
            json!({
                "document_type": range.document_type.as_str(),
                "prefix": range.prefix,
                "from_number": range.from_number,
                "to_number": range.to_number,
            }),
        );
        self.repository.add_numbering_range(range.clone(), &entry).await?;
        Ok(range)
    }

    pub async fn dead_jobs(&self, limit: i64) -> Result<Vec<DeadJob>, DomainError> {
        self.repository.list_dead_jobs(limit.clamp(1, MAX_LIST_LIMIT)).await
    }

    pub async fn retry_dead_job(&self, job_id: Uuid) -> Result<(), DomainError> {
        let entry = audit("job.retried", None, Some(job_id), json!({}));
        if self.repository.retry_dead_job(job_id, &entry).await? {
            Ok(())
        } else {
            Err(DomainError::NotFound(format!("Trabajo muerto {} no encontrado", job_id)))
        }
    }

    pub async fn dead_deliveries(&self, limit: i64) -> Result<Vec<DeadDelivery>, DomainError> {
        self.repository.list_dead_deliveries(limit.clamp(1, MAX_LIST_LIMIT)).await
    }

    pub async fn retry_dead_delivery(&self, delivery_id: Uuid) -> Result<(), DomainError> {
        let entry = audit("webhook_delivery.retried", None, Some(delivery_id), json!({}));
        if self.repository.retry_dead_delivery(delivery_id, &entry).await? {
            Ok(())
        } else {
            Err(DomainError::NotFound(format!("Entrega muerta {} no encontrada", delivery_id)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use chrono::Duration;
    use core_domain::models::{DocumentStatus, JobStatus};
    use core_domain::ports::{DocumentRepository, JobRepository, TenantRepository};
    use core_domain::testing::InMemoryStore;

    fn service(store: &Arc<InMemoryStore>) -> AdminService {
        AdminService::new(store.clone())
    }

    fn tenant_request(tax_id: &str) -> CreateTenant {
        CreateTenant {
            tax_id: tax_id.into(),
            registration_name: "Nuevo SAS".into(),
            tax_regime: "48".into(),
            reception_email: "fe@nuevo.co".into(),
        }
    }

    fn range_request(prefix: &str, from_days: i64, to_days: i64) -> RegisterNumberingRange {
        RegisterNumberingRange {
            document_type: "invoice".into(),
            prefix: prefix.into(),
            from_number: 1,
            to_number: 5000,
            technical_key: "tk".into(),
            valid_date_from: Utc::now() + Duration::days(from_days),
            valid_date_to: Utc::now() + Duration::days(to_days),
            last_issued_number: None,
        }
    }

    #[tokio::test]
    async fn tenants_get_hashed_api_keys_that_can_be_revoked() {
        let store = Arc::new(InMemoryStore::default());
        let admin = service(&store);
        let tenant = admin.create_tenant(tenant_request("901234567")).await.unwrap();
        assert!(matches!(admin.create_tenant(tenant_request("901234567")).await, Err(DomainError::Conflict(_))));
        assert!(matches!(admin.create_tenant(tenant_request("90.123")).await, Err(DomainError::Validation(_))));

        let issued = admin.create_api_key(tenant.id, "ERP produccion").await.unwrap();
        assert!(issued.key.starts_with("dsk_") && issued.key.len() == 4 + 64);
        assert!(issued.key.starts_with(&issued.prefix));
        let listed = admin.list_api_keys(tenant.id).await.unwrap();
        assert_eq!(listed[0].key_hash, hash(&issued.key), "only the hash is stored");
        assert!(!serde_json::to_string(&listed).unwrap().contains(&listed[0].key_hash), "hash is not serialized");

        let found = store.find_by_api_key_hash(&hash(&issued.key)).await.unwrap().unwrap();
        assert_eq!(found.id, tenant.id);
        admin.revoke_api_key(tenant.id, issued.id).await.unwrap();
        assert!(store.find_by_api_key_hash(&hash(&issued.key)).await.unwrap().is_none());
        assert!(matches!(admin.revoke_api_key(tenant.id, issued.id).await, Err(DomainError::NotFound(_))));

        let actions: Vec<&str> = store.audit_log().iter().map(|e| e.action).collect();
        assert_eq!(actions, vec!["tenant.created", "api_key.created", "api_key.revoked"]);
        assert!(!format!("{:?}", store.audit_log()).contains(&issued.key));
    }

    #[tokio::test]
    async fn certificates_replace_the_active_one() {
        let store = Arc::new(InMemoryStore::default());
        let admin = service(&store);
        let tenant = admin.create_tenant(tenant_request("901234567")).await.unwrap();
        let request = |path: &str, days: i64| RegisterCertificate {
            vault_reference_path: path.into(),
            thumbprint: "AB:CD".into(),
            expiration_date: Utc::now() + Duration::days(days),
        };
        admin.register_certificate(tenant.id, request("tenants/nuevo/2026", 300)).await.unwrap();
        let second = admin.register_certificate(tenant.id, request("tenants/nuevo/2027", 600)).await.unwrap();
        let active: Vec<_> = store
            .certificates_of(tenant.id)
            .into_iter()
            .filter(|c| c.status == CertificateStatus::Active)
            .collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, second.id);
        assert!(admin.register_certificate(tenant.id, request("../sys/policy", 300)).await.is_err());
        assert!(admin.register_certificate(tenant.id, request("tenants/x", -1)).await.is_err());
        assert!(matches!(
            admin.register_certificate(Uuid::new_v4(), request("tenants/x", 300)).await,
            Err(DomainError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn numbering_ranges_are_validated_and_cannot_overlap() {
        let store = Arc::new(InMemoryStore::default());
        let admin = service(&store);
        let tenant = admin.create_tenant(tenant_request("901234567")).await.unwrap();
        admin.register_numbering_range(tenant.id, range_request("FE", -1, 365)).await.unwrap();
        assert!(matches!(
            admin.register_numbering_range(tenant.id, range_request("FE", 100, 400)).await,
            Err(DomainError::Conflict(_))
        ));
        admin.register_numbering_range(tenant.id, range_request("FE", 365, 730)).await.unwrap();
        admin.register_numbering_range(tenant.id, range_request("SETP", -1, 365)).await.unwrap();

        let mut no_key = range_request("FV", -1, 10);
        no_key.technical_key = String::new();
        assert!(admin.register_numbering_range(tenant.id, no_key).await.is_err());
        let mut inverted = range_request("FV", 10, 1);
        inverted.technical_key = "tk".into();
        assert!(admin.register_numbering_range(tenant.id, inverted).await.is_err());
        let mut migrated = range_request("FV", -1, 10);
        migrated.last_issued_number = Some(120);
        assert_eq!(admin.register_numbering_range(tenant.id, migrated).await.unwrap().current_counter, 120);
        let mut outside = range_request("FX", -1, 10);
        outside.last_issued_number = Some(9000);
        assert!(admin.register_numbering_range(tenant.id, outside).await.is_err());
    }

    #[tokio::test]
    async fn dead_jobs_can_be_retried_when_the_document_still_needs_them() {
        let (store, tenant) = store_with_tenant();
        store.add_range(range(&tenant, DocumentType::Invoice, "FE", 1));
        let issue = crate::issue::IssueService::new(store.clone(), 3);
        let core_domain::ports::IssueOutcome::Created(doc) =
            issue.issue_invoice(&tenant, invoice_payload(1), None).await.unwrap()
        else {
            panic!()
        };
        let job = store.claim("w", 1, std::time::Duration::from_secs(60)).await.unwrap().remove(0);
        store
            .apply_transition(
                core_domain::ports::Transition::new(doc.document_id, DocumentStatus::ContingencyPending)
                    .with_job(core_domain::ports::JobOutcome::Bury(job.id, "DIAN down".into())),
            )
            .await
            .unwrap();
        let admin = service(&store);
        let dead = admin.dead_jobs(50).await.unwrap();
        assert_eq!((dead.len(), dead[0].tenant_id), (1, tenant.id));

        admin.retry_dead_job(job.id).await.unwrap();
        let stored = store.jobs_for(doc.document_id).remove(0);
        assert_eq!((stored.status, stored.attempts), (JobStatus::Queued, 0));
        assert_eq!(store.document(doc.document_id).unwrap().status, DocumentStatus::Pending);
        assert!(matches!(admin.retry_dead_job(job.id).await, Err(DomainError::NotFound(_))));

        // Un documento ya aceptado no se retransmite.
        let job = store.claim("w", 1, std::time::Duration::from_secs(60)).await.unwrap().remove(0);
        store
            .apply_transition(
                core_domain::ports::Transition::new(doc.document_id, DocumentStatus::DianAccepted)
                    .with_job(core_domain::ports::JobOutcome::Bury(job.id, "late".into())),
            )
            .await
            .unwrap();
        assert!(matches!(admin.retry_dead_job(job.id).await, Err(DomainError::Conflict(_))));
    }
}

//! Implementación en memoria de todos los puertos de persistencia, para pruebas de casos de
//! uso sin base de datos. Solo se compila con la feature `testing`.
//!
//! Reproduce la semántica del adaptador PostgreSQL: un único `Mutex` hace de transacción, de
//! modo que cada operación del puerto es atómica.

use crate::jobs::{Job, NewJob};
use crate::models::{
    Certificate, CertificateStatus, DianEvent, Direction, Document, DocumentStatus, JobKind,
    JobStatus, NumberingRange, Tenant,
};
use crate::numbering::next_number;
use crate::ports::{
    CertificateRepository, DocumentRepository, DomainError, HealthProbe, IssueOutcome,
    IssuedDocument, JobOutcome, JobRepository, NewInboundDocument, NewOutboundDocument,
    TenantRepository, Transition,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use uuid::Uuid;

/// Estado persistido de un trabajo, visible para las aserciones de las pruebas.
#[derive(Debug, Clone)]
pub struct StoredJob {
    pub id: Uuid,
    pub kind: JobKind,
    pub document_id: Uuid,
    pub payload: serde_json::Value,
    pub status: JobStatus,
    pub attempts: i32,
    pub max_attempts: i32,
    pub run_at: DateTime<Utc>,
    pub locked_until: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Default)]
struct State {
    tenants: Vec<Tenant>,
    certificates: Vec<Certificate>,
    ranges: HashMap<Uuid, NumberingRange>,
    documents: HashMap<Uuid, Document>,
    events: Vec<DianEvent>,
    jobs: HashMap<Uuid, StoredJob>,
    idempotency: HashMap<(Uuid, String), (String, IssuedDocument)>,
}

impl State {
    fn has_active_job(&self, document_id: Uuid, kind: JobKind) -> bool {
        self.jobs.values().any(|j| {
            j.document_id == document_id
                && j.kind == kind
                && matches!(j.status, JobStatus::Queued | JobStatus::Running)
        })
    }

    fn insert_job(&mut self, job: NewJob) -> Result<(), DomainError> {
        if self.has_active_job(job.document_id, job.kind) {
            return Err(DomainError::Conflict(format!(
                "document {} already has an active {} job",
                job.document_id, job.kind
            )));
        }
        self.jobs.insert(
            job.id,
            StoredJob {
                id: job.id,
                kind: job.kind,
                document_id: job.document_id,
                payload: job.payload,
                status: JobStatus::Queued,
                attempts: 0,
                max_attempts: job.max_attempts,
                run_at: job.run_at,
                locked_until: None,
                last_error: None,
            },
        );
        Ok(())
    }

    fn job_mut(&mut self, id: Uuid) -> Result<&mut StoredJob, DomainError> {
        self.jobs
            .get_mut(&id)
            .ok_or_else(|| DomainError::NotFound(format!("job {}", id)))
    }
}

#[derive(Default)]
pub struct InMemoryStore {
    state: Mutex<State>,
}

impl InMemoryStore {
    pub fn add_tenant(&self, tenant: Tenant) {
        self.state.lock().unwrap().tenants.push(tenant);
    }

    pub fn add_certificate(&self, certificate: Certificate) {
        self.state.lock().unwrap().certificates.push(certificate);
    }

    pub fn add_range(&self, range: NumberingRange) {
        self.state.lock().unwrap().ranges.insert(range.id, range);
    }

    /// Inserta un documento ya existente (sin pasar por la emisión).
    pub fn insert_document(&self, document: Document) {
        self.state.lock().unwrap().documents.insert(document.id, document);
    }

    pub fn enqueue(&self, job: NewJob) {
        self.state.lock().unwrap().insert_job(job).unwrap();
    }

    pub fn document(&self, id: Uuid) -> Option<Document> {
        self.state.lock().unwrap().documents.get(&id).cloned()
    }

    pub fn range(&self, id: Uuid) -> Option<NumberingRange> {
        self.state.lock().unwrap().ranges.get(&id).cloned()
    }

    pub fn events_for(&self, document_id: Uuid) -> Vec<DianEvent> {
        self.state
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|e| e.document_id == document_id)
            .cloned()
            .collect()
    }

    pub fn jobs_for(&self, document_id: Uuid) -> Vec<StoredJob> {
        let mut jobs: Vec<StoredJob> = self
            .state
            .lock()
            .unwrap()
            .jobs
            .values()
            .filter(|j| j.document_id == document_id)
            .cloned()
            .collect();
        jobs.sort_by_key(|j| j.run_at);
        jobs
    }

    /// Hace que todos los trabajos en cola estén listos ya (simula el paso del tiempo).
    pub fn make_jobs_due(&self) {
        let now = Utc::now();
        for job in self.state.lock().unwrap().jobs.values_mut() {
            if job.status == JobStatus::Queued {
                job.run_at = now;
            }
        }
    }
}

#[async_trait]
impl TenantRepository for InMemoryStore {
    async fn find_by_api_key(&self, api_key: &str) -> Result<Option<Tenant>, DomainError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .tenants
            .iter()
            .find(|t| t.api_keys.get("secret").and_then(|v| v.as_str()) == Some(api_key))
            .cloned())
    }
}

#[async_trait]
impl CertificateRepository for InMemoryStore {
    async fn find_active_certificate(&self, tenant_id: Uuid) -> Result<Option<Certificate>, DomainError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .certificates
            .iter()
            .find(|c| c.tenant_id == tenant_id && c.status == CertificateStatus::Active)
            .cloned())
    }
}

#[async_trait]
impl DocumentRepository for InMemoryStore {
    async fn issue_outbound(
        &self,
        document: NewOutboundDocument,
        initial_job: NewJob,
    ) -> Result<IssueOutcome, DomainError> {
        let mut state = self.state.lock().unwrap();

        if let Some(key) = &document.idempotency {
            if let Some((hash, issued)) = state.idempotency.get(&(document.tenant_id, key.key.clone())) {
                if *hash != key.request_hash {
                    return Err(DomainError::Validation(
                        "La clave de idempotencia ya se uso con un cuerpo distinto".to_string(),
                    ));
                }
                return Ok(IssueOutcome::Replayed(*issued));
            }
        }

        let now = Utc::now();
        let range = state
            .ranges
            .values()
            .filter(|r| {
                r.tenant_id == document.tenant_id
                    && r.document_type == document.document_type
                    && r.prefix == document.prefix
                    && r.valid_date_from <= now
                    && r.valid_date_to >= now
            })
            .max_by_key(|r| r.valid_date_from)
            .cloned()
            .ok_or_else(|| {
                DomainError::Validation(format!(
                    "No hay una resolucion de numeracion vigente para el prefijo '{}' y tipo '{}'",
                    document.prefix, document.document_type
                ))
            })?;

        let number = next_number(&range, document.requested_number, now)?;
        let duplicate = state.documents.values().any(|d| {
            d.tenant_id == document.tenant_id
                && d.direction == Direction::Outbound
                && d.document_type == document.document_type
                && d.prefix == document.prefix
                && d.document_number == number
        });
        if duplicate {
            return Err(DomainError::Conflict(format!("El documento {}{} ya existe", document.prefix, number)));
        }

        state.ranges.get_mut(&range.id).unwrap().current_counter = number;
        state.documents.insert(
            document.id,
            Document {
                id: document.id,
                tenant_id: document.tenant_id,
                direction: Direction::Outbound,
                issuer_nit: String::new(),
                document_type: document.document_type,
                prefix: document.prefix.clone(),
                document_number: number,
                numbering_range_id: Some(range.id),
                cufe_cude: String::new(),
                payload: document.payload.clone(),
                original_xml: None,
                signed_xml: None,
                pdf_url: None,
                status: DocumentStatus::Pending,
                created_at: now,
            },
        );
        state.insert_job(initial_job)?;

        let issued = IssuedDocument {
            document_id: document.id,
            document_number: number,
        };
        if let Some(key) = document.idempotency {
            state
                .idempotency
                .insert((document.tenant_id, key.key), (key.request_hash, issued));
        }
        Ok(IssueOutcome::Created(issued))
    }

    async fn insert_inbound(&self, document: NewInboundDocument) -> Result<bool, DomainError> {
        let mut state = self.state.lock().unwrap();
        let duplicate = state.documents.values().any(|d| {
            d.tenant_id == document.tenant_id
                && d.direction == Direction::Inbound
                && d.issuer_nit == document.issuer_nit
                && d.document_type == document.document_type
                && d.prefix == document.prefix
                && d.document_number == document.document_number
        });
        if duplicate {
            return Ok(false);
        }
        state.documents.insert(
            document.id,
            Document {
                id: document.id,
                tenant_id: document.tenant_id,
                direction: Direction::Inbound,
                issuer_nit: document.issuer_nit,
                document_type: document.document_type,
                prefix: document.prefix,
                document_number: document.document_number,
                numbering_range_id: None,
                cufe_cude: document.cufe,
                payload: document.payload,
                original_xml: None,
                signed_xml: Some(document.signed_xml),
                pdf_url: None,
                status: document.status,
                created_at: Utc::now(),
            },
        );
        Ok(true)
    }

    async fn find_document(&self, id: Uuid) -> Result<Option<Document>, DomainError> {
        Ok(self.document(id))
    }

    async fn find_tenant_document(&self, tenant_id: Uuid, id: Uuid) -> Result<Option<Document>, DomainError> {
        Ok(self.document(id).filter(|d| d.tenant_id == tenant_id))
    }

    async fn find_numbering_range(&self, id: Uuid) -> Result<Option<crate::models::NumberingRange>, DomainError> {
        Ok(self.range(id))
    }

    async fn store_artifacts(
        &self,
        id: Uuid,
        cufe: &str,
        original_xml: &str,
        signed_xml: &str,
    ) -> Result<(), DomainError> {
        let mut state = self.state.lock().unwrap();
        let doc = state
            .documents
            .get_mut(&id)
            .ok_or_else(|| DomainError::NotFound(format!("document {}", id)))?;
        doc.cufe_cude = cufe.to_string();
        doc.original_xml = Some(original_xml.to_string());
        doc.signed_xml = Some(signed_xml.to_string());
        Ok(())
    }

    async fn record_event(&self, event: &DianEvent) -> Result<(), DomainError> {
        self.state.lock().unwrap().events.push(event.clone());
        Ok(())
    }

    async fn find_events(&self, document_id: Uuid) -> Result<Vec<DianEvent>, DomainError> {
        Ok(self.events_for(document_id))
    }

    async fn apply_transition(&self, transition: Transition) -> Result<bool, DomainError> {
        let mut state = self.state.lock().unwrap();
        let current = state
            .documents
            .get(&transition.document_id)
            .ok_or_else(|| DomainError::NotFound(format!("document {}", transition.document_id)))?
            .status;
        if !current.can_transition_to(transition.to) {
            return Ok(false);
        }
        state.documents.get_mut(&transition.document_id).unwrap().status = transition.to;
        if let Some(event) = transition.event {
            state.events.push(event);
        }
        match transition.job {
            Some(JobOutcome::Complete(id)) => state.job_mut(id)?.status = JobStatus::Done,
            Some(JobOutcome::Bury(id, error)) => {
                let job = state.job_mut(id)?;
                job.status = JobStatus::Dead;
                job.last_error = Some(error);
            }
            None => {}
        }
        if let Some(job) = transition.follow_up {
            state.insert_job(job)?;
        }
        Ok(true)
    }
}

#[async_trait]
impl JobRepository for InMemoryStore {
    async fn claim(&self, _worker_id: &str, limit: i64, lease: Duration) -> Result<Vec<Job>, DomainError> {
        let now = Utc::now();
        let mut state = self.state.lock().unwrap();
        let mut ready: Vec<Uuid> = state
            .jobs
            .values()
            .filter(|j| {
                (j.status == JobStatus::Queued && j.run_at <= now)
                    || (j.status == JobStatus::Running && j.locked_until.map_or(false, |t| t < now))
            })
            .filter(|j| j.attempts < j.max_attempts)
            .map(|j| j.id)
            .collect();
        ready.sort_by_key(|id| state.jobs[id].run_at);
        ready.truncate(limit.max(0) as usize);

        let lease = chrono::Duration::from_std(lease).unwrap_or_else(|_| chrono::Duration::minutes(10));
        Ok(ready
            .into_iter()
            .map(|id| {
                let job = state.jobs.get_mut(&id).unwrap();
                job.status = JobStatus::Running;
                job.attempts += 1;
                job.locked_until = Some(now + lease);
                Job {
                    id: job.id,
                    kind: job.kind,
                    document_id: job.document_id,
                    payload: job.payload.clone(),
                    attempts: job.attempts,
                    max_attempts: job.max_attempts,
                }
            })
            .collect())
    }

    async fn complete(&self, job_id: Uuid) -> Result<(), DomainError> {
        self.state.lock().unwrap().job_mut(job_id)?.status = JobStatus::Done;
        Ok(())
    }

    async fn retry(&self, job_id: Uuid, run_at: DateTime<Utc>, error: &str) -> Result<(), DomainError> {
        let mut state = self.state.lock().unwrap();
        let job = state.job_mut(job_id)?;
        job.status = JobStatus::Queued;
        job.run_at = run_at;
        job.locked_until = None;
        job.last_error = Some(error.to_string());
        Ok(())
    }

    async fn release(&self, job_id: Uuid, run_at: DateTime<Utc>) -> Result<(), DomainError> {
        let mut state = self.state.lock().unwrap();
        let job = state.job_mut(job_id)?;
        job.status = JobStatus::Queued;
        job.run_at = run_at;
        job.locked_until = None;
        job.attempts = (job.attempts - 1).max(0);
        Ok(())
    }

    async fn bury(&self, job_id: Uuid, error: &str) -> Result<(), DomainError> {
        let mut state = self.state.lock().unwrap();
        let job = state.job_mut(job_id)?;
        job.status = JobStatus::Dead;
        job.last_error = Some(error.to_string());
        Ok(())
    }

    async fn requeue_contingency(&self, limit: i64, max_attempts: i32) -> Result<u64, DomainError> {
        let mut state = self.state.lock().unwrap();
        let candidates: Vec<Uuid> = state
            .documents
            .values()
            .filter(|d| d.status == DocumentStatus::ContingencyPending)
            .map(|d| d.id)
            .filter(|id| !state.has_active_job(*id, JobKind::Transmit))
            .take(limit.max(0) as usize)
            .collect();
        for id in &candidates {
            state.documents.get_mut(id).unwrap().status = DocumentStatus::Pending;
            state.insert_job(NewJob::transmit(*id, max_attempts))?;
        }
        Ok(candidates.len() as u64)
    }
}

#[async_trait]
impl HealthProbe for InMemoryStore {
    async fn ping(&self) -> Result<(), DomainError> {
        Ok(())
    }
}

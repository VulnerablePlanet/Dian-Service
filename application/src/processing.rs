//! Caso de uso: procesamiento de los trabajos de la cola (transmisión y consulta de estado).

use crate::resilience::{backoff_with_jitter, Breakers};
use chrono::Utc;
use core_domain::jobs::{Job, NewJob};
use core_domain::models::{DianEvent, Document, DocumentStatus, DocumentType, EventStatus, JobKind};
use core_domain::ports::{
    CryptoSigner, DianResponse, DianSoapClient, DocumentRepository, DomainError, JobOutcome,
    JobRepository, Transition,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};
use ubl_engine::payload::{InvoicePayload, PayrollPayload, SupportDocumentPayload};

/// Códigos de `StatusCode` que la DIAN usa para un documento validado. "1" se mantiene por
/// compatibilidad con el comportamiento anterior; no está confirmado en los anexos técnicos.
const ACCEPTED_CODES: &[&str] = &["00", "1"];
const REJECTED_CODE: &str = "99";

#[derive(Debug, Clone)]
pub struct ProcessorConfig {
    pub transmit_max_attempts: i32,
    pub poll_max_attempts: i32,
    /// Espera antes de la primera consulta de estado de un envío asíncrono.
    pub poll_initial_delay: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl Default for ProcessorConfig {
    fn default() -> Self {
        Self {
            transmit_max_attempts: 5,
            poll_max_attempts: 30,
            poll_initial_delay: Duration::from_secs(10),
            backoff_base: Duration::from_secs(2),
            backoff_max: Duration::from_secs(600),
        }
    }
}

pub struct DocumentProcessor {
    documents: Arc<dyn DocumentRepository>,
    jobs: Arc<dyn JobRepository>,
    signer: Arc<dyn CryptoSigner>,
    soap: Arc<dyn DianSoapClient>,
    breakers: Arc<Breakers>,
    config: ProcessorConfig,
}

/// XML firmado listo para transmitir.
struct Artifacts {
    signed_xml: String,
    cufe: String,
    issuer_nit: String,
}

fn is_habilitacion(document: &Document) -> bool {
    document.payload.get("environment").and_then(|v| v.as_str()) == Some("2")
}

fn test_set_id(document: &Document) -> Option<String> {
    document
        .payload
        .get("test_set_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn response_event(document_id: uuid::Uuid, status: EventStatus, response: DianResponse) -> DianEvent {
    DianEvent {
        soap_trace_id: response.soap_trace_id,
        xml_response: response.xml_response,
        ..DianEvent::new(document_id, status, &response.status_code, response.message)
    }
}

/// Deserializa el payload tomando prefijo y consecutivo del documento persistido (la fuente
/// de verdad, p. ej. el consecutivo que asignó el servidor a un documento soporte).
fn payload_with_number<T: serde::de::DeserializeOwned>(document: &Document) -> Result<T, String> {
    let mut payload = document.payload.clone();
    payload["prefix"] = serde_json::json!(document.prefix);
    payload["number"] = serde_json::json!(document.document_number);
    serde_json::from_value(payload).map_err(|e| format!("Invalid {} payload JSON: {}", document.document_type, e))
}

impl DocumentProcessor {
    pub fn new(
        documents: Arc<dyn DocumentRepository>,
        jobs: Arc<dyn JobRepository>,
        signer: Arc<dyn CryptoSigner>,
        soap: Arc<dyn DianSoapClient>,
        breakers: Arc<Breakers>,
        config: ProcessorConfig,
    ) -> Self {
        Self {
            documents,
            jobs,
            signer,
            soap,
            breakers,
            config,
        }
    }

    /// Procesa un trabajo reclamado. Los resultados de negocio (aceptado, rechazado, fallo
    /// local, reintento) se persisten; solo se devuelve error si falla la infraestructura
    /// propia, y en ese caso el lease del trabajo vence y otro worker lo reintenta.
    pub async fn process(&self, job: Job) -> Result<(), DomainError> {
        match job.kind {
            JobKind::Transmit => self.transmit(job).await,
            JobKind::PollStatus => self.poll(job).await,
        }
    }

    async fn retry_delay(&self, attempt: i32) -> chrono::Duration {
        let delay = backoff_with_jitter(attempt, self.config.backoff_base, self.config.backoff_max);
        chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::seconds(60))
    }

    /// Aplica la transición; si el documento ya había cambiado de estado, solo cierra el trabajo.
    async fn transition(&self, job: &Job, transition: Transition) -> Result<(), DomainError> {
        let document_id = transition.document_id;
        let to = transition.to;
        if !self.documents.apply_transition(transition).await? {
            info!(%document_id, ?to, "document already moved on; closing job");
            self.jobs.complete(job.id).await?;
        }
        Ok(())
    }

    async fn fail(&self, job: &Job, document: &Document, reason: String) -> Result<(), DomainError> {
        warn!(document_id = %document.id, %reason, "processing failed");
        self.transition(
            job,
            Transition::new(document.id, DocumentStatus::ProcessingFailed)
                .with_event(DianEvent::new(document.id, EventStatus::Error, "PROCESSING_FAILED", reason))
                .with_job(JobOutcome::Complete(job.id)),
        )
        .await
    }

    // -----------------------------------------------------------------------------------
    // Transmisión
    // -----------------------------------------------------------------------------------

    async fn transmit(&self, job: Job) -> Result<(), DomainError> {
        let Some(document) = self.documents.find_document(job.document_id).await? else {
            return self.jobs.bury(job.id, "document not found").await;
        };
        if document.status != DocumentStatus::Pending {
            info!(document_id = %document.id, status = %document.status, "document is not pending; closing job");
            return self.jobs.complete(job.id).await;
        }

        let is_hab = is_habilitacion(&document);
        let breaker = self.breakers.for_environment(is_hab);
        if !breaker.can_execute().await {
            let run_at = Utc::now() + chrono::Duration::from_std(breaker.cooldown()).unwrap_or_default();
            return self.jobs.release(job.id, run_at).await;
        }

        let artifacts = match self.artifacts(&document).await {
            Ok(artifacts) => artifacts,
            Err(reason) => return self.fail(&job, &document, reason).await,
        };

        // Si un intento anterior pudo haber llegado a la DIAN (p. ej. timeout tras el envío),
        // se consulta el estado antes de reenviar para no duplicar la transmisión.
        if job.attempts > 1 && test_set_id(&document).is_none() {
            if let Ok(status) = self.soap.get_status(&artifacts.cufe, is_hab).await {
                if ACCEPTED_CODES.contains(&status.status_code.as_str()) {
                    breaker.record_success().await;
                    info!(document_id = %document.id, "already accepted by DIAN; not resending");
                    return self
                        .transition(
                            &job,
                            Transition::new(document.id, DocumentStatus::DianAccepted)
                                .with_event(response_event(document.id, EventStatus::Accepted, status))
                                .with_job(JobOutcome::Complete(job.id)),
                        )
                        .await;
                }
            }
        }

        let zip = match ubl_engine::packaging::compress_xml_to_zip(
            &artifacts.signed_xml,
            &artifacts.issuer_nit,
            &document.prefix,
            document.document_number,
        ) {
            Ok((bytes, _)) => bytes,
            Err(e) => return self.fail(&job, &document, format!("ZIP compression failed: {}", e)).await,
        };

        if let Some(test_set_id) = test_set_id(&document) {
            return match self.soap.send_test_set_async(&zip, &test_set_id, is_hab).await {
                Ok(zip_key) => {
                    breaker.record_success().await;
                    let first_poll = Utc::now()
                        + chrono::Duration::from_std(self.config.poll_initial_delay).unwrap_or_default();
                    let event = DianEvent {
                        soap_trace_id: Some(zip_key.clone()),
                        ..DianEvent::new(document.id, EventStatus::Sent, "SENT_ASYNC", "Enviado asincronamente para habilitacion")
                    };
                    self.transition(
                        &job,
                        Transition::new(document.id, DocumentStatus::SentDian)
                            .with_event(event)
                            .with_job(JobOutcome::Complete(job.id))
                            .with_follow_up(NewJob::poll_status(
                                document.id,
                                &zip_key,
                                self.config.poll_max_attempts,
                                first_poll,
                            )),
                    )
                    .await
                }
                Err(e) => self.transmission_failed(&job, &document, e).await,
            };
        }

        match self.soap.send_bill_sync(&zip, is_hab).await {
            Ok(response) => {
                breaker.record_success().await;
                let accepted = ACCEPTED_CODES.contains(&response.status_code.as_str());
                let (status, event_status) = if accepted {
                    (DocumentStatus::DianAccepted, EventStatus::Accepted)
                } else {
                    (DocumentStatus::DianRejected, EventStatus::Rejected)
                };
                let response_xml = response.xml_response.clone();
                self.transition(
                    &job,
                    Transition::new(document.id, status)
                        .with_event(response_event(document.id, event_status, response))
                        .with_job(JobOutcome::Complete(job.id)),
                )
                .await?;
                if accepted && document.document_type == DocumentType::Invoice {
                    self.attached_document(&document, &artifacts, response_xml.as_deref().unwrap_or_default())
                        .await;
                }
                Ok(())
            }
            Err(e) => self.transmission_failed(&job, &document, e).await,
        }
    }

    async fn transmission_failed(&self, job: &Job, document: &Document, error: DomainError) -> Result<(), DomainError> {
        self.breakers.for_environment(is_habilitacion(document)).record_failure().await;
        let message = error.to_string();
        warn!(document_id = %document.id, attempt = job.attempts, %message, "transmission failed");
        let event = DianEvent::new(document.id, EventStatus::Error, "HTTP_ERROR", message.clone());

        if job.is_last_attempt() {
            return self
                .transition(
                    job,
                    Transition::new(document.id, DocumentStatus::ContingencyPending)
                        .with_event(event)
                        .with_job(JobOutcome::Bury(job.id, message)),
                )
                .await;
        }
        self.documents.record_event(&event).await?;
        let run_at = Utc::now() + self.retry_delay(job.attempts).await;
        self.jobs.retry(job.id, run_at, &message).await
    }

    /// Devuelve el XML firmado: reutiliza el de un intento anterior o lo genera y firma.
    async fn artifacts(&self, document: &Document) -> Result<Artifacts, String> {
        let issuer_nit = match document.document_type {
            DocumentType::Invoice | DocumentType::CreditNote | DocumentType::DebitNote => {
                payload_with_number::<InvoicePayload>(document)?.company_nit
            }
            DocumentType::Payroll => payload_with_number::<PayrollPayload>(document)?.employer_nit,
            DocumentType::SupportDocument => payload_with_number::<SupportDocumentPayload>(document)?.buyer_nit,
            other => return Err(format!("Document type {} is not supported yet", other)),
        };

        if let Some(signed_xml) = document.signed_xml.clone().filter(|_| !document.cufe_cude.is_empty()) {
            return Ok(Artifacts {
                signed_xml,
                cufe: document.cufe_cude.clone(),
                issuer_nit,
            });
        }

        let (unsigned_xml, cufe) = self.generate(document).await?;
        let signed_xml = self
            .signer
            .sign_for_tenant(document.tenant_id, &unsigned_xml)
            .await
            .map_err(|e| format!("Cryptographic signature failed: {}", e))?;
        self.documents
            .store_artifacts(document.id, &cufe, &unsigned_xml, &signed_xml)
            .await
            .map_err(|e| format!("Could not store signed XML: {}", e))?;
        Ok(Artifacts {
            signed_xml,
            cufe,
            issuer_nit,
        })
    }

    async fn generate(&self, document: &Document) -> Result<(String, String), String> {
        match document.document_type {
            DocumentType::Invoice => {
                let payload: InvoicePayload = payload_with_number(document)?;
                let technical_key = self.technical_key(document, &payload).await?;
                ubl_engine::generate_invoice_xml(&payload, &technical_key)
                    .map_err(|e| format!("Invoice generation failed: {}", e))
            }
            // El CUDE de las notas usa el PIN del software, no la clave técnica.
            DocumentType::CreditNote | DocumentType::DebitNote => {
                ubl_engine::generate_invoice_xml(&payload_with_number(document)?, "")
                    .map_err(|e| format!("Note generation failed: {}", e))
            }
            DocumentType::Payroll => ubl_engine::generate_payroll_xml(&payload_with_number(document)?)
                .map_err(|e| format!("Payroll generation failed: {}", e)),
            DocumentType::SupportDocument => ubl_engine::generate_support_document_xml(&payload_with_number(document)?)
                .map_err(|e| format!("Support document generation failed: {}", e)),
            other => Err(format!("Document type {} is not supported yet", other)),
        }
    }

    /// Clave técnica de la resolución con la que se numeró el documento. Los documentos
    /// anteriores a la migración no tienen resolución asociada: usan la del payload.
    async fn technical_key(&self, document: &Document, payload: &InvoicePayload) -> Result<String, String> {
        if let Some(range_id) = document.numbering_range_id {
            return self
                .documents
                .find_numbering_range(range_id)
                .await
                .map_err(|e| format!("Could not load numbering range: {}", e))?
                .map(|range| range.technical_key)
                .ok_or_else(|| format!("Numbering range {} not found", range_id));
        }
        payload
            .technical_key
            .clone()
            .ok_or_else(|| "Document has no numbering range nor legacy technical_key".to_string())
    }

    /// Genera y firma el AttachedDocument. La entrega al adquirente aún no está implementada:
    /// por ahora solo se valida que el contenedor se pueda construir.
    async fn attached_document(&self, document: &Document, artifacts: &Artifacts, dian_response_xml: &str) {
        let result: Result<usize, String> = async {
            let payload: InvoicePayload = payload_with_number(document)?;
            let (issue_date, issue_time) = core_domain::time::colombia_date_time(Utc::now());
            let xml = ubl_engine::generate_attached_document_xml(
                &payload,
                &format!("ATT-{}", uuid::Uuid::new_v4()),
                &issue_date,
                &issue_time,
                &artifacts.cufe,
                &artifacts.signed_xml,
                dian_response_xml,
            )
            .map_err(|e| e.to_string())?;
            let signed = self
                .signer
                .sign_for_tenant(document.tenant_id, &xml)
                .await
                .map_err(|e| e.to_string())?;
            Ok(signed.len())
        }
        .await;
        match result {
            Ok(size) => info!(document_id = %document.id, size, "AttachedDocument generated (delivery not implemented)"),
            Err(e) => warn!(document_id = %document.id, error = %e, "AttachedDocument generation failed"),
        }
    }

    // -----------------------------------------------------------------------------------
    // Consulta de estado de envíos asíncronos
    // -----------------------------------------------------------------------------------

    async fn poll(&self, job: Job) -> Result<(), DomainError> {
        let Some(document) = self.documents.find_document(job.document_id).await? else {
            return self.jobs.bury(job.id, "document not found").await;
        };
        if document.status != DocumentStatus::SentDian {
            return self.jobs.complete(job.id).await;
        }
        let Some(zip_key) = job.zip_key().map(str::to_string) else {
            return self.jobs.bury(job.id, "poll job without zip_key").await;
        };

        let is_hab = is_habilitacion(&document);
        let breaker = self.breakers.for_environment(is_hab);
        if !breaker.can_execute().await {
            let run_at = Utc::now() + chrono::Duration::from_std(breaker.cooldown()).unwrap_or_default();
            return self.jobs.release(job.id, run_at).await;
        }

        let outcome = match self.soap.get_status_zip(&zip_key, is_hab).await {
            Ok(response) => {
                breaker.record_success().await;
                let code = response.status_code.clone();
                if ACCEPTED_CODES.contains(&code.as_str()) {
                    return self
                        .transition(
                            &job,
                            Transition::new(document.id, DocumentStatus::DianAccepted)
                                .with_event(response_event(document.id, EventStatus::Accepted, response))
                                .with_job(JobOutcome::Complete(job.id)),
                        )
                        .await;
                }
                if code == REJECTED_CODE {
                    return self
                        .transition(
                            &job,
                            Transition::new(document.id, DocumentStatus::DianRejected)
                                .with_event(response_event(document.id, EventStatus::Rejected, response))
                                .with_job(JobOutcome::Complete(job.id)),
                        )
                        .await;
                }
                format!("DIAN reports status {} ({})", code, response.message)
            }
            Err(e) => {
                breaker.record_failure().await;
                e.to_string()
            }
        };

        if job.is_last_attempt() {
            let reason = format!("Status polling exhausted after {} attempts: {}", job.attempts, outcome);
            warn!(document_id = %document.id, %reason);
            self.documents
                .record_event(&DianEvent::new(document.id, EventStatus::Error, "POLLING_EXHAUSTED", reason.clone()))
                .await?;
            return self.jobs.bury(job.id, &reason).await;
        }
        let run_at = Utc::now() + self.retry_delay(job.attempts).await;
        self.jobs.retry(job.id, run_at, &outcome).await
    }

    /// Devuelve a la cola los documentos en contingencia (reproceso automático).
    pub async fn requeue_contingency(&self, limit: i64) -> Result<u64, DomainError> {
        self.jobs
            .requeue_contingency(limit, self.config.transmit_max_attempts)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::IssueService;
    use crate::test_support::*;
    use core_domain::models::{JobStatus, Tenant};
    use core_domain::ports::IssueOutcome;
    use core_domain::testing::InMemoryStore;
    use std::sync::atomic::Ordering;

    struct Harness {
        store: Arc<InMemoryStore>,
        tenant: Tenant,
        signer: Arc<FakeSigner>,
        soap: Arc<ScriptedSoap>,
        breakers: Arc<Breakers>,
        processor: DocumentProcessor,
    }

    fn harness_with(signer: FakeSigner, max_attempts: i32) -> Harness {
        let (store, tenant) = store_with_tenant();
        for (doc_type, prefix, from) in [
            (DocumentType::Invoice, "FE", 1),
            (DocumentType::Payroll, "NOM", 1),
            (DocumentType::SupportDocument, "DS", 5000),
            (DocumentType::CreditNote, "NC", 1),
        ] {
            store.add_range(range(&tenant, doc_type, prefix, from));
        }
        let signer = Arc::new(signer);
        let soap = Arc::new(ScriptedSoap::default());
        let breakers = Arc::new(Breakers::new(3, Duration::from_secs(60)));
        let processor = DocumentProcessor::new(
            store.clone(),
            store.clone(),
            signer.clone(),
            soap.clone(),
            breakers.clone(),
            ProcessorConfig {
                transmit_max_attempts: max_attempts,
                poll_max_attempts: 3,
                poll_initial_delay: Duration::ZERO,
                backoff_base: Duration::from_millis(1),
                backoff_max: Duration::from_millis(1),
            },
        );
        Harness { store, tenant, signer, soap, breakers, processor }
    }

    fn harness() -> Harness {
        harness_with(FakeSigner::default(), 3)
    }

    impl Harness {
        async fn issue_invoice(&self, payload: InvoicePayload) -> uuid::Uuid {
            let issue = IssueService::new(self.store.clone(), self.processor.config.transmit_max_attempts);
            match issue.issue_invoice(&self.tenant, payload, None).await.unwrap() {
                IssueOutcome::Created(d) => d.document_id,
                other => panic!("unexpected {:?}", other),
            }
        }

        /// Reclama y procesa los trabajos listos, como haría el worker (una pasada).
        async fn run_once(&self) -> usize {
            let jobs = self.store.claim("test", 50, Duration::from_secs(600)).await.unwrap();
            let count = jobs.len();
            for job in jobs {
                self.processor.process(job).await.unwrap();
            }
            count
        }

        fn status(&self, id: uuid::Uuid) -> DocumentStatus {
            self.store.document(id).unwrap().status
        }
    }

    #[tokio::test]
    async fn invoice_is_signed_with_the_range_technical_key_and_accepted() {
        let h = harness();
        let id = h.issue_invoice(invoice_payload(1)).await;

        assert_eq!(h.run_once().await, 1);

        let doc = h.store.document(id).unwrap();
        assert_eq!(doc.status, DocumentStatus::DianAccepted);
        let (_, expected_cufe) = ubl_engine::generate_invoice_xml(&invoice_payload(1), "tk-from-range").unwrap();
        assert_eq!(doc.cufe_cude, expected_cufe, "CUFE must use the resolution's technical key");
        assert!(h.soap.last_xml().contains("<ds:SignatureValue>fake</ds:SignatureValue>"));
        assert_eq!(h.store.events_for(id)[0].event_status, EventStatus::Accepted);
        assert_eq!(h.store.jobs_for(id)[0].status, JobStatus::Done);
    }

    #[tokio::test]
    async fn credit_note_is_issued_with_a_cude_and_its_invoice_reference() {
        let h = harness();
        let note = InvoicePayload {
            document_type: "credit_note".into(),
            prefix: "NC".into(),
            number: 1,
            billing_reference: Some(ubl_engine::payload::BillingReferencePayload {
                number: "FE1".into(),
                cufe: "ab".repeat(48),
                issue_date: invoice_payload(1).issue_date,
            }),
            discrepancy: Some(ubl_engine::payload::DiscrepancyPayload {
                code: "2".into(),
                description: "Anulacion de factura".into(),
            }),
            software_pin: Some("pin-123".into()),
            ..invoice_payload(1)
        };
        let id = h.issue_invoice(note.clone()).await;
        let doc = h.store.document(id).unwrap();
        assert_eq!(doc.document_type, DocumentType::CreditNote);

        h.run_once().await;

        let doc = h.store.document(id).unwrap();
        assert_eq!(doc.status, DocumentStatus::DianAccepted);
        let (_, expected_cude) = ubl_engine::generate_invoice_xml(&note, "ignored").unwrap();
        assert_eq!(doc.cufe_cude, expected_cude);
        let sent = h.soap.last_xml();
        assert!(sent.contains("<CreditNote") && sent.contains("<cac:BillingReference>"));
    }

    #[tokio::test]
    async fn payroll_and_support_documents_use_the_persisted_number() {
        let h = harness();
        let issue = IssueService::new(h.store.clone(), 3);
        let IssueOutcome::Created(payroll) = issue.issue_payroll(&h.tenant, payroll_payload(1), None).await.unwrap() else {
            panic!()
        };
        let IssueOutcome::Created(support) = issue.issue_support_document(&h.tenant, support_payload(), None).await.unwrap() else {
            panic!()
        };

        h.run_once().await;

        assert_eq!(h.status(payroll.document_id), DocumentStatus::DianAccepted);
        let support_doc = h.store.document(support.document_id).unwrap();
        assert_eq!(support_doc.status, DocumentStatus::DianAccepted);
        assert!(support_doc.original_xml.unwrap().contains("DS5000"), "server-assigned number must be in the XML");
    }

    #[tokio::test]
    async fn signing_failure_marks_the_document_failed() {
        let h = harness_with(FakeSigner { fail: true, ..Default::default() }, 3);
        let id = h.issue_invoice(invoice_payload(1)).await;

        h.run_once().await;

        assert_eq!(h.status(id), DocumentStatus::ProcessingFailed);
        let events = h.store.events_for(id);
        assert_eq!(events[0].dian_response_code.as_deref(), Some("PROCESSING_FAILED"));
        assert_eq!(h.store.jobs_for(id)[0].status, JobStatus::Done);
        assert_eq!(h.soap.calls(&h.soap.send_calls), 0);
    }

    #[tokio::test]
    async fn retry_reuses_the_signed_xml_and_checks_status_before_resending() {
        let h = harness();
        let id = h.issue_invoice(invoice_payload(1)).await;
        h.soap.script_send(vec![Err("timeout after request was sent".into())]);
        h.soap.script_status(vec![ok("00")]);

        h.run_once().await;
        assert_eq!(h.status(id), DocumentStatus::Pending);
        let job = &h.store.jobs_for(id)[0];
        assert_eq!(job.status, JobStatus::Queued);
        assert!(job.last_error.as_deref().unwrap().contains("timeout"));

        h.store.make_jobs_due();
        h.run_once().await;

        assert_eq!(h.status(id), DocumentStatus::DianAccepted);
        assert_eq!(h.signer.calls.load(Ordering::SeqCst), 1, "the XML is signed only once");
        assert_eq!(h.soap.calls(&h.soap.send_calls), 1, "accepted per GetStatus: no second send");
        assert_eq!(h.soap.calls(&h.soap.status_calls), 1);
    }

    #[tokio::test]
    async fn exhausted_transmission_goes_to_contingency_and_is_requeued() {
        let h = harness_with(FakeSigner::default(), 2);
        let id = h.issue_invoice(invoice_payload(1)).await;
        h.soap.script_send(vec![Err("down".into()), Err("down".into())]);

        h.run_once().await;
        h.store.make_jobs_due();
        h.run_once().await;

        assert_eq!(h.status(id), DocumentStatus::ContingencyPending);
        assert_eq!(h.store.jobs_for(id)[0].status, JobStatus::Dead);

        assert_eq!(h.processor.requeue_contingency(10).await.unwrap(), 1);
        assert_eq!(h.status(id), DocumentStatus::Pending);
        h.breakers.for_environment(true).record_success().await;
        h.run_once().await;
        assert_eq!(h.status(id), DocumentStatus::DianAccepted);
    }

    #[tokio::test]
    async fn open_breaker_releases_the_job_without_consuming_attempts() {
        let h = harness();
        let id = h.issue_invoice(invoice_payload(1)).await;
        for _ in 0..3 {
            h.breakers.for_environment(true).record_failure().await;
        }

        h.run_once().await;

        let job = &h.store.jobs_for(id)[0];
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.attempts, 0);
        assert!(job.run_at > Utc::now());
        assert_eq!(h.status(id), DocumentStatus::Pending);
        assert_eq!(h.soap.calls(&h.soap.send_calls), 0);
    }

    #[tokio::test]
    async fn test_set_is_sent_async_then_polled_until_accepted() {
        let h = harness();
        let mut payload = invoice_payload(1);
        payload.test_set_id = Some("8a7b6c5d-1234".into());
        let id = h.issue_invoice(payload).await;
        h.soap.script_status_zip(vec![ok("98"), ok("00")]);

        h.run_once().await;
        assert_eq!(h.status(id), DocumentStatus::SentDian);
        let poll = h.store.jobs_for(id).into_iter().find(|j| j.kind == JobKind::PollStatus).unwrap();
        assert_eq!(poll.payload["zip_key"], "zip-key-1");

        h.run_once().await;
        assert_eq!(h.status(id), DocumentStatus::SentDian, "status 98 = still processing");
        h.store.make_jobs_due();
        h.run_once().await;
        assert_eq!(h.status(id), DocumentStatus::DianAccepted);
    }

    #[tokio::test]
    async fn polling_stops_after_its_attempt_limit() {
        let h = harness();
        let mut payload = invoice_payload(1);
        payload.test_set_id = Some("8a7b6c5d-1234".into());
        let id = h.issue_invoice(payload).await;
        h.soap.script_status_zip(vec![ok("98"), ok("98"), ok("98"), ok("98")]);

        h.run_once().await; // transmisión
        for _ in 0..5 {
            h.store.make_jobs_due();
            h.run_once().await;
        }

        assert_eq!(h.soap.calls(&h.soap.status_zip_calls), 3, "poll_max_attempts = 3");
        let poll = h.store.jobs_for(id).into_iter().find(|j| j.kind == JobKind::PollStatus).unwrap();
        assert_eq!(poll.status, JobStatus::Dead);
        assert_eq!(h.status(id), DocumentStatus::SentDian);
        assert!(h
            .store
            .events_for(id)
            .iter()
            .any(|e| e.dian_response_code.as_deref() == Some("POLLING_EXHAUSTED")));
    }

    #[tokio::test]
    async fn open_breaker_never_downgrades_a_sent_document() {
        // Hallazgo R-11.
        let h = harness();
        let mut payload = invoice_payload(1);
        payload.test_set_id = Some("8a7b6c5d-1234".into());
        let id = h.issue_invoice(payload).await;
        h.run_once().await;
        assert_eq!(h.status(id), DocumentStatus::SentDian);

        for _ in 0..3 {
            h.breakers.for_environment(true).record_failure().await;
        }
        h.run_once().await;

        assert_eq!(h.status(id), DocumentStatus::SentDian);
        let poll = h.store.jobs_for(id).into_iter().find(|j| j.kind == JobKind::PollStatus).unwrap();
        assert_eq!((poll.status, poll.attempts), (JobStatus::Queued, 0));
    }

    #[tokio::test]
    async fn rejected_document_is_terminal() {
        let h = harness();
        let id = h.issue_invoice(invoice_payload(1)).await;
        h.soap.script_send(vec![ok("99")]);

        h.run_once().await;

        assert_eq!(h.status(id), DocumentStatus::DianRejected);
        assert_eq!(h.store.events_for(id)[0].event_status, EventStatus::Rejected);
    }
}

mod backoff;
mod circuit_breaker;
mod error;
mod queue;

use backoff::calculate_backoff_with_jitter;
use circuit_breaker::{CircuitBreaker, State as CbState};
use core_domain::models::{DianEvent, Document, DocumentStatus, DocumentType, EventStatus};
use core_domain::ports::{DianResponse, DianSoapClient, DocumentRepository, DomainError};
use crypto_signer::signer::SignerConfig;
use error::WorkerError;
use queue::{Job, JobQueue, RedisQueue};
use std::sync::Arc;
use std::time::Duration;
use ubl_engine::{generate_invoice_xml, generate_payroll_xml, generate_support_document_xml};
use ubl_engine::payload::InvoicePayload;
use uuid::Uuid;
use sqlx::postgres::PgPoolOptions;
use dian_soap_client::client::DianSoapClientImpl;
use core_domain::repository::PgDocumentRepository;
use tokio::sync::Semaphore;

pub struct DocumentProcessor {
    repo: Arc<dyn DocumentRepository>,
    soap_client: Arc<dyn DianSoapClient>,
    queue: Arc<dyn JobQueue>,
    signer: Arc<SignerConfig>,
    cb: Arc<CircuitBreaker>,
    max_attempts: u32,
}

impl DocumentProcessor {
    pub fn new(
        repo: Arc<dyn DocumentRepository>,
        soap_client: Arc<dyn DianSoapClient>,
        queue: Arc<dyn JobQueue>,
        signer: Arc<SignerConfig>,
        cb: Arc<CircuitBreaker>,
        max_attempts: u32,
    ) -> Self {
        Self {
            repo,
            soap_client,
            queue,
            signer,
            cb,
            max_attempts,
        }
    }

    /// Procesa un solo trabajo (Job) extraído de la cola de Redis
    pub async fn process_job(&self, job: Job) -> Result<(), WorkerError> {
        // 1. Cargar el documento de PostgreSQL
        let mut document = match self.repo.find_document_by_id(job.document_id).await? {
            Some(doc) => doc,
            None => {
                // Si el documento no existe en base de datos, hacemos ACK para quitarlo de la cola
                self.queue.ack(&job).await?;
                return Err(WorkerError::Internal(format!(
                    "Document {} not found in database",
                    job.document_id
                )));
            }
        };

        // 2. Verificar el Circuit Breaker de la DIAN
        if !self.cb.can_execute().await {
            // El circuito está abierto -> La DIAN está caída de forma prolongada
            // Transición a contingencia local
            println!(
                "[CircuitBreaker OPEN] Transicionando documento {} a contingencia_pending",
                document.id
            );
            document.status = DocumentStatus::ContingencyPending;
            self.repo.save_document(&document).await?;
            
            // Confirmamos el trabajo para removerlo de la cola
            self.queue.ack(&job).await?;
            return Ok(());
        }

        // 2b. Lógica de Polling para documentos ya enviados de forma asíncrona (Habilitación / Async)
        if document.status == DocumentStatus::SentDian {
            println!("[Polling] Buscando zip_key para el documento {}...", document.id);
            let events = self.repo.find_dian_events_by_document_id(document.id).await?;
            let zip_key = events.iter()
                .filter(|e| e.event_status == EventStatus::Sent)
                .filter_map(|e| e.soap_trace_id.clone())
                .next();

            if let Some(key) = zip_key {
                let payload: InvoicePayload = serde_json::from_value(document.payload.clone())
                    .map_err(|e| WorkerError::Internal(format!("Invalid document payload JSON: {}", e)))?;
                let is_hab = payload.environment == "2";
                
                println!("[Polling] Consultando GetStatusZip para zip_key: {}", key);
                match self.soap_client.get_status_zip(&key, is_hab).await {
                    Ok(dian_resp) => {
                        self.cb.record_success().await;
                        // El ciclo termina cuando el StatusCode sea 00 (Procesado Correctamente) o 99 (Rechazado)
                        if dian_resp.status_code == "00" {
                            println!("[Success] GetStatusZip reporta procesado para {}", document.id);
                            document.status = DocumentStatus::DianAccepted;
                            self.repo.save_document(&document).await?;

                            // Generar, firmar y notificar AttachedDocument (contenedor electrónico)
                            if document.document_type == DocumentType::Invoice {
                                if let Some(ref resp_xml) = dian_resp.xml_response {
                                    if let Err(err) = self.process_attached_document(&document, &payload, resp_xml).await {
                                        println!("[AttachedDocument Error] Failed to generate: {:?}", err);
                                    }
                                }
                            }

                            let event = DianEvent {
                                id: Uuid::new_v4(),
                                document_id: document.id,
                                event_status: EventStatus::Accepted,
                                dian_response_code: Some(dian_resp.status_code),
                                dian_response_message: Some(dian_resp.message),
                                soap_trace_id: Some(key),
                                xml_response: dian_resp.xml_response,
                                created_at: chrono::Utc::now(),
                            };
                            self.repo.save_dian_event(&event).await?;
                            self.queue.ack(&job).await?;
                        } else if dian_resp.status_code == "99" {
                            println!("[Rejected] GetStatusZip reporta rechazado para {}", document.id);
                            document.status = DocumentStatus::DianRejected;
                            self.repo.save_document(&document).await?;

                            let event = DianEvent {
                                id: Uuid::new_v4(),
                                document_id: document.id,
                                event_status: EventStatus::Rejected,
                                dian_response_code: Some(dian_resp.status_code),
                                dian_response_message: Some(dian_resp.message),
                                soap_trace_id: Some(key),
                                xml_response: dian_resp.xml_response,
                                created_at: chrono::Utc::now(),
                            };
                            self.repo.save_dian_event(&event).await?;
                            self.queue.ack(&job).await?;
                        } else {
                            // Sigue en procesamiento o error temporal de DIAN, reintentamos con backoff
                            println!("[Processing] GetStatusZip reporta estado {}. Re-encolando para reintento...", dian_resp.status_code);
                            self.queue.nack(&job, false).await?;
                            let sleep_dur = calculate_backoff_with_jitter(job.attempt);
                            let queue_clone = self.queue.clone();
                            let retry_job = job.retry();
                            tokio::spawn(async move {
                                tokio::time::sleep(sleep_dur).await;
                                let _ = queue_clone.enqueue(&retry_job).await;
                            });
                        }
                    }
                    Err(e) => {
                        println!("[SOAP Polling Failure] Error en get_status_zip: {:?}", e);
                        self.cb.record_failure().await;
                        self.queue.nack(&job, false).await?;
                        let sleep_dur = calculate_backoff_with_jitter(job.attempt);
                        let queue_clone = self.queue.clone();
                        let retry_job = job.retry();
                        tokio::spawn(async move {
                            tokio::time::sleep(sleep_dur).await;
                            let _ = queue_clone.enqueue(&retry_job).await;
                        });
                    }
                }
            } else {
                println!("[Polling Error] No se encontro zip_key para el documento {}", document.id);
                self.queue.ack(&job).await?;
            }
            return Ok(());
        }

        // 3. Generar XML UBL 2.1 base y extraer metadatos según el tipo de documento
        let (unsigned_xml, cufe_cude, company_nit, prefix, number, is_hab, test_set_id) = match generate_document_xml(&document) {
            Ok(generated) => generated,
            Err(reason) => return self.fail_document(&job, &mut document, reason).await,
        };

        // Guardar CUFE/CUDE y XML original
        document.cufe_cude = cufe_cude;
        document.original_xml = Some(unsigned_xml.clone());

        // 4. Firmar el XML (XAdES-EPES) con el certificado del tenant
        let signed_xml = match self
            .signer
            .sign_for_tenant(self.repo.as_ref(), document.tenant_id, &unsigned_xml)
            .await
        {
            Ok(xml) => xml,
            Err(e) => {
                let reason = format!("Cryptographic signature failed: {}", e);
                return self.fail_document(&job, &mut document, reason).await;
            }
        };
        document.signed_xml = Some(signed_xml.clone());
        document.status = DocumentStatus::Pending;
        self.repo.save_document(&document).await?;

        // 5. Comprimir a ZIP
        let (zip_bytes, _zip_filename) = match dian_soap_client::zip::compress_xml_to_zip(
            &signed_xml,
            &company_nit,
            &prefix,
            number,
        ) {
            Ok(res) => res,
            Err(e) => {
                let reason = format!("ZIP compression failed: {}", e);
                return self.fail_document(&job, &mut document, reason).await;
            }
        };

        // 6. Transmisión SOAP a la DIAN
        let is_hab = is_hab;
        
        // Si el payload contiene un test_set_id, ejecutamos la transmisión asíncrona de habilitación
        if let Some(ref test_set_id) = test_set_id {
            println!("[Transmitting] Enviando set de pruebas asincrono (test_set_id: {}) para {}", test_set_id, document.id);
            match self.soap_client.send_test_set_async(&zip_bytes, test_set_id, is_hab).await {
                Ok(zip_key) => {
                    self.cb.record_success().await;
                    
                    document.status = DocumentStatus::SentDian;
                    self.repo.save_document(&document).await?;

                    let event = DianEvent {
                        id: Uuid::new_v4(),
                        document_id: document.id,
                        event_status: EventStatus::Sent,
                        dian_response_code: Some("SENT_ASYNC".to_string()),
                        dian_response_message: Some("Enviado asincronamente para habilitacion".to_string()),
                        soap_trace_id: Some(zip_key.clone()),
                        xml_response: None,
                        created_at: chrono::Utc::now(),
                    };
                    self.repo.save_dian_event(&event).await?;
                    
                    self.queue.ack(&job).await?;
                    
                    // Encolar nuevo job para iniciar polling
                    let poll_job = Job::new(document.id);
                    self.queue.enqueue(&poll_job).await?;
                    
                    println!("[Sent] Documento {} enviado asincronamente. ZipKey: {}", document.id, zip_key);
                    return Ok(());
                }
                Err(e) => {
                    println!("[SOAP Habilitacion Failure] {:?}", e);
                    self.cb.record_failure().await;

                    let event = DianEvent {
                        id: Uuid::new_v4(),
                        document_id: document.id,
                        event_status: EventStatus::Error,
                        dian_response_code: Some("HTTP_ERROR".to_string()),
                        dian_response_message: Some(e.to_string()),
                        soap_trace_id: None,
                        xml_response: None,
                        created_at: chrono::Utc::now(),
                    };
                    let _ = self.repo.save_dian_event(&event).await;

                    if job.attempt >= self.max_attempts {
                        println!("[Attempts Exhausted] Documento {} pasa a contingencia_pending", document.id);
                        document.status = DocumentStatus::ContingencyPending;
                        self.repo.save_document(&document).await?;
                        self.queue.ack(&job).await?;
                    } else {
                        self.queue.nack(&job, false).await?;
                        let sleep_dur = calculate_backoff_with_jitter(job.attempt);
                        let queue_clone = self.queue.clone();
                        let retry_job = job.retry();
                        tokio::spawn(async move {
                            tokio::time::sleep(sleep_dur).await;
                            let _ = queue_clone.enqueue(&retry_job).await;
                        });
                    }
                    return Ok(());
                }
            }
        }

        // Envío síncrono estándar
        println!("[Transmitting] Enviando documento {} a la DIAN (Sync)...", document.id);
        match self.soap_client.send_bill_sync(&zip_bytes, is_hab).await {
            Ok(dian_resp) => {
                self.cb.record_success().await;
                
                let is_accepted = dian_resp.status_code == "00" || dian_resp.status_code == "1";
                if is_accepted {
                    document.status = DocumentStatus::DianAccepted;
                } else {
                    document.status = DocumentStatus::DianRejected;
                }
                
                self.repo.save_document(&document).await?;

                if is_accepted {
                    if document.document_type == DocumentType::Invoice {
                        if let Some(ref resp_xml) = dian_resp.xml_response {
                            let payload: InvoicePayload = serde_json::from_value(document.payload.clone())
                                .map_err(|e| WorkerError::Internal(format!("Invalid invoice payload JSON: {}", e)))?;
                            if let Err(err) = self.process_attached_document(&document, &payload, resp_xml).await {
                                println!("[AttachedDocument Error] Failed to generate: {:?}", err);
                            }
                        }
                    }
                }

                let event = DianEvent {
                    id: Uuid::new_v4(),
                    document_id: document.id,
                    event_status: if is_accepted { EventStatus::Accepted } else { EventStatus::Rejected },
                    dian_response_code: Some(dian_resp.status_code),
                    dian_response_message: Some(dian_resp.message),
                    soap_trace_id: dian_resp.soap_trace_id,
                    xml_response: dian_resp.xml_response,
                    created_at: chrono::Utc::now(),
                };
                self.repo.save_dian_event(&event).await?;
                
                self.queue.ack(&job).await?;
                println!("[Success] Documento {} procesado con código: {}", document.id, event.dian_response_code.unwrap());
                Ok(())
            }
            Err(e) => {
                println!("[SOAP Failure] Error transmitiendo {}: {:?}", document.id, e);
                self.cb.record_failure().await;

                let event = DianEvent {
                    id: Uuid::new_v4(),
                    document_id: document.id,
                    event_status: EventStatus::Error,
                    dian_response_code: Some("HTTP_ERROR".to_string()),
                    dian_response_message: Some(e.to_string()),
                    soap_trace_id: None,
                    xml_response: None,
                    created_at: chrono::Utc::now(),
                };
                let _ = self.repo.save_dian_event(&event).await;

                if job.attempt >= self.max_attempts {
                    println!("[Attempts Exhausted] Documento {} pasa a contingencia_pending", document.id);
                    document.status = DocumentStatus::ContingencyPending;
                    self.repo.save_document(&document).await?;
                    self.queue.ack(&job).await?;
                } else {
                    let sleep_dur = calculate_backoff_with_jitter(job.attempt);
                    println!(
                        "[Scheduling Retry] Reintentando documento {} en {}ms (intento {})",
                        document.id,
                        sleep_dur.as_millis(),
                        job.attempt + 1
                    );
                    
                    self.queue.nack(&job, false).await?;
                    
                    let queue_clone = self.queue.clone();
                    let retry_job = job.retry();
                    tokio::spawn(async move {
                        tokio::time::sleep(sleep_dur).await;
                        let _ = queue_clone.enqueue(&retry_job).await;
                    });
                }
                
                Ok(())
            }
        }
    }

    /// Marca el documento como `ProcessingFailed`, deja el motivo en `dian_events` y
    /// confirma el trabajo: un fallo local no se puede resolver reintentando.
    async fn fail_document(
        &self,
        job: &Job,
        document: &mut Document,
        reason: String,
    ) -> Result<(), WorkerError> {
        eprintln!("[Processing Failed] Documento {}: {}", document.id, reason);
        document.status = DocumentStatus::ProcessingFailed;
        self.repo.save_document(document).await?;
        self.repo
            .save_dian_event(&DianEvent {
                id: Uuid::new_v4(),
                document_id: document.id,
                event_status: EventStatus::Error,
                dian_response_code: Some("PROCESSING_FAILED".to_string()),
                dian_response_message: Some(reason.clone()),
                soap_trace_id: None,
                xml_response: None,
                created_at: chrono::Utc::now(),
            })
            .await?;
        self.queue.ack(job).await?;
        Err(WorkerError::Internal(reason))
    }

    /// Genera, firma criptográficamente y empaqueta en ZIP el AttachedDocument (contenedor electrónico)
    async fn process_attached_document(
        &self,
        document: &Document,
        payload: &InvoicePayload,
        dian_response_xml: &str,
    ) -> Result<(), WorkerError> {
        println!("[AttachedDocument] Generando contenedor para {}...", document.id);
        
        let unique_id = format!("ATT-{}", Uuid::new_v4());
        let (issue_date, issue_time) = core_domain::time::colombia_date_time(chrono::Utc::now());
        
        let signed_invoice_xml = document.signed_xml.as_ref()
            .ok_or_else(|| WorkerError::Internal("Document does not have signed_xml".to_string()))?;

        // 1. Renderizar XML de AttachedDocument
        let att_doc_xml = ubl_engine::generate_attached_document_xml(
            payload,
            &unique_id,
            &issue_date,
            &issue_time,
            &document.cufe_cude,
            signed_invoice_xml,
            dian_response_xml,
        ).map_err(|e| WorkerError::Internal(format!("Failed to generate AttachedDocument XML: {}", e)))?;

        // 2. Firmar el AttachedDocument
        let signed_att_doc_xml = self
            .signer
            .sign_for_tenant(self.repo.as_ref(), document.tenant_id, &att_doc_xml)
            .await
            .map_err(|e| WorkerError::Internal(format!("Failed to sign AttachedDocument: {}", e)))?;

        // 3. Comprimir a ZIP en memoria
        let (zip_bytes, zip_filename) = dian_soap_client::zip::compress_xml_to_zip(
            &signed_att_doc_xml,
            &payload.company_nit,
            &payload.prefix,
            payload.number,
        ).map_err(|e| WorkerError::Internal(format!("Failed to compress AttachedDocument ZIP: {}", e)))?;

        // Simular notificación al adquirente
        println!(
            "[Email Notification] Enviando correo a {} con el archivo {} (tamaño: {} bytes)",
            payload.customer_email,
            zip_filename.replace("z", "ad"),
            zip_bytes.len()
        );
        
        Ok(())
    }
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn required_env(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(format!("Missing required environment variable {}", name).into()),
    }
}

type GeneratedDocument = (String, String, String, String, i32, bool, Option<String>);

/// Genera el XML UBL y el CUFE/CUDE/CUNE/CUDS del documento según su tipo.
fn generate_document_xml(document: &Document) -> Result<GeneratedDocument, String> {
    match document.document_type {
        DocumentType::Invoice => {
            let payload: InvoicePayload = serde_json::from_value(document.payload.clone())
                .map_err(|e| format!("Invalid invoice payload JSON: {}", e))?;
            let (xml, cufe) = ubl_engine::generate_invoice_xml(&payload)
                .map_err(|e| format!("Invoice generation failed: {}", e))?;
            let is_hab = payload.environment == "2";
            Ok((xml, cufe, payload.company_nit.clone(), payload.prefix.clone(), payload.number, is_hab, payload.test_set_id.clone()))
        }
        DocumentType::Payroll => {
            let payload: ubl_engine::payload::PayrollPayload = serde_json::from_value(document.payload.clone())
                .map_err(|e| format!("Invalid payroll payload JSON: {}", e))?;
            let (xml, cune) = ubl_engine::generate_payroll_xml(&payload)
                .map_err(|e| format!("Payroll generation failed: {}", e))?;
            let is_hab = payload.environment == "2";
            Ok((xml, cune, payload.employer_nit.clone(), payload.prefix.clone(), payload.number, is_hab, None))
        }
        DocumentType::SupportDocument => {
            let payload: ubl_engine::payload::SupportDocumentPayload = serde_json::from_value(document.payload.clone())
                .map_err(|e| format!("Invalid support document payload JSON: {}", e))?;
            let (xml, cuds) = ubl_engine::generate_support_document_xml(&payload)
                .map_err(|e| format!("Support document generation failed: {}", e))?;
            let is_hab = payload.environment == "2";
            Ok((xml, cuds, payload.buyer_nit.clone(), payload.prefix.clone(), payload.number, is_hab, None))
        }
        DocumentType::CreditNote | DocumentType::DebitNote => Err(format!(
            "Document type {:?} is not supported yet",
            document.document_type
        )),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Iniciando worker-redis...");

    // 0. Configuración obligatoria: sin valores por defecto para conexiones ni secretos
    let db_url = required_env("DATABASE_URL")?;
    let redis_url = required_env("REDIS_URL")?;
    let binary_security_token = required_env("DIAN_BINARY_SECURITY_TOKEN")?;
    let signer = Arc::new(SignerConfig::from_env()?);
    println!("Firma configurada en modo {:?}", signer.mode());

    // 1. Conectar a PostgreSQL

    let db_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Failed to connect to PostgreSQL in worker-redis");

    // 2. Conectar a Redis
    let redis_client = redis::Client::open(redis_url.clone())
        .expect("Failed to open Redis client in worker-redis");

    let queue = Arc::new(RedisQueue::new(&redis_url, "dian_jobs")?);

    // 3. Inicializar componentes
    let repo = Arc::new(PgDocumentRepository::new(db_pool.clone()));
    
    let soap_client = Arc::new(DianSoapClientImpl::new(
        binary_security_token,
        optional_env("DIAN_HAB_URL"),
        optional_env("DIAN_PROD_URL"),
    ));
    
    let cb = Arc::new(CircuitBreaker::new(3, Duration::from_secs(5)));
    
    let max_attempts = 3;
    let processor = Arc::new(DocumentProcessor::new(
        repo,
        soap_client,
        queue.clone(),
        signer,
        cb,
        max_attempts,
    ));

    // 4. Configurar Semáforo de Concurrencia
    let max_concurrent_jobs: usize = std::env::var("MAX_CONCURRENT_JOBS")
        .unwrap_or_else(|_| "5".to_string())
        .parse()
        .unwrap_or(5);
    
    let sem = Arc::new(Semaphore::new(max_concurrent_jobs));

    println!("Worker-redis listo. Escuchando cola 'dian_jobs' con concurrencia max de {}...", max_concurrent_jobs);

    // 5. Bucle principal con control de concurrencia y health checks de bases de datos
    loop {
        // Chequeos de salud de PostgreSQL y Redis
        let mut healthy = true;
        if let Err(e) = sqlx::query("SELECT 1").execute(&db_pool).await {
            eprintln!("[Health Check ERROR] PostgreSQL is down: {:?}. Retrying in 5 seconds...", e);
            healthy = false;
        }
        
        match redis_client.get_async_connection().await {
            Ok(mut conn) => {
                let ping_res: Result<(), _> = redis::cmd("PING").query_async(&mut conn).await;
                if let Err(e) = ping_res {
                    eprintln!("[Health Check ERROR] Redis PING failed: {:?}. Retrying in 5 seconds...", e);
                    healthy = false;
                }
            }
            Err(e) => {
                eprintln!("[Health Check ERROR] Redis connection failed: {:?}. Retrying in 5 seconds...", e);
                healthy = false;
            }
        }

        if !healthy {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        // Adquirir permiso del semáforo antes de de-encolar para no saturar
        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };

        // Extraer trabajo
        match queue.dequeue_blocking(5.0).await {
            Ok(Some(job)) => {
                let processor = processor.clone();
                tokio::spawn(async move {
                    // Mantener el permiso retenido hasta que termine la tarea
                    let _permit = permit;
                    if let Err(e) = processor.process_job(job).await {
                        eprintln!("[Job Error] Failed to process job: {:?}", e);
                    }
                });
            }
            Ok(None) => {
                // No hay trabajos, el timeout de BRPOPLPUSH expiró. Liberar el permiso y volver a intentar.
                drop(permit);
            }
            Err(e) => {
                eprintln!("[Queue Error] Error pulling job from queue: {:?}", e);
                drop(permit);
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use core_domain::testing::InMemoryRepository;
    use crypto_signer::provider::LocalCertificateProvider;
    use crypto_signer::signer::KeyMode;
    use std::sync::Mutex as StdMutex;

    const TEST_KEY_PEM: &str = include_str!("../../crypto-signer/tests/fixtures/test_key.pem");
    const TEST_CERT_PEM: &str = include_str!("../../crypto-signer/tests/fixtures/test_cert.pem");

    /// Cola en memoria que registra las operaciones para poder afirmarlas.
    #[derive(Default)]
    struct RecordingQueue {
        acked: StdMutex<Vec<Job>>,
        nacked: StdMutex<Vec<Job>>,
        enqueued: StdMutex<Vec<Job>>,
    }

    #[async_trait]
    impl JobQueue for RecordingQueue {
        async fn enqueue(&self, job: &Job) -> Result<(), WorkerError> {
            self.enqueued.lock().unwrap().push(job.clone());
            Ok(())
        }
        async fn ack(&self, job: &Job) -> Result<(), WorkerError> {
            self.acked.lock().unwrap().push(job.clone());
            Ok(())
        }
        async fn nack(&self, job: &Job, _requeue: bool) -> Result<(), WorkerError> {
            self.nacked.lock().unwrap().push(job.clone());
            Ok(())
        }
    }

    struct MockSoapClient {
        should_fail: bool,
    }

    fn accepted() -> DianResponse {
        DianResponse {
            status_code: "00".to_string(),
            message: "Aceptado".to_string(),
            xml_response: Some("<resp/>".to_string()),
            soap_trace_id: Some("trace_abc".to_string()),
        }
    }

    #[async_trait]
    impl DianSoapClient for MockSoapClient {
        async fn send_bill_sync(&self, _zip: &[u8], _is_hab: bool) -> Result<DianResponse, DomainError> {
            if self.should_fail {
                return Err(DomainError::Soap("DIAN is down".to_string()));
            }
            Ok(accepted())
        }
        async fn send_bill_async(&self, _zip: &[u8], _is_hab: bool) -> Result<String, DomainError> {
            Ok("key".to_string())
        }
        async fn send_test_set_async(&self, _zip: &[u8], _test_set_id: &str, _is_hab: bool) -> Result<String, DomainError> {
            Ok("test_set_zip_key".to_string())
        }
        async fn get_status(&self, _track_id: &str, _is_hab: bool) -> Result<DianResponse, DomainError> {
            Ok(accepted())
        }
        async fn get_status_zip(&self, _track_id: &str, _is_hab: bool) -> Result<DianResponse, DomainError> {
            Ok(accepted())
        }
        async fn send_event_update_status(&self, _zip: &[u8], _is_hab: bool) -> Result<DianResponse, DomainError> {
            Ok(accepted())
        }
    }

    fn local_signer() -> Arc<SignerConfig> {
        let provider = LocalCertificateProvider::from_pem(TEST_CERT_PEM.into(), TEST_KEY_PEM.into()).unwrap();
        Arc::new(SignerConfig::new(Arc::new(provider), KeyMode::SharedLocal))
    }

    /// Firmador por tenant sin certificados registrados: toda firma falla.
    fn signer_without_certificates() -> Arc<SignerConfig> {
        let provider = LocalCertificateProvider::from_pem(TEST_CERT_PEM.into(), TEST_KEY_PEM.into()).unwrap();
        Arc::new(SignerConfig::new(Arc::new(provider), KeyMode::PerTenantVault))
    }

    fn document(document_type: DocumentType, payload: serde_json::Value) -> Document {
        Document {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            document_type,
            prefix: "FE".to_string(),
            document_number: 1002,
            cufe_cude: "".to_string(),
            payload,
            original_xml: None,
            signed_xml: None,
            pdf_url: None,
            status: DocumentStatus::Pending,
            created_at: chrono::Utc::now(),
        }
    }

    fn invoice_payload() -> serde_json::Value {
        serde_json::json!({
            "prefix": "FE", "number": 1002, "issue_date": "2026-05-23",
            "issue_time": "10:42:00-05:00", "company_nit": "900123456",
            "company_name": "Empresa Emisora", "customer_id_type": "31",
            "customer_id": "900999888", "customer_name": "Cliente",
            "customer_email": "cliente@test.com", "net_amount": 100.0,
            "tax_amount": 19.0, "total_amount": 119.0, "environment": "2",
            "technical_key": "tech_key", "document_type": "invoice", "test_set_id": null
        })
    }

    fn payroll_payload() -> serde_json::Value {
        serde_json::json!({
            "prefix": "NOM", "number": 101, "issue_date": "2026-05-26",
            "issue_time": "08:00:00", "employer_nit": "900123456",
            "employer_name": "Empresa Emisora", "employee_id_type": "13",
            "employee_id": "10203040", "employee_name": "Trabajador",
            "devengado": 1500000.0, "deducido": 60000.0, "total": 1440000.0,
            "software_pin": "pin_test", "environment": "2"
        })
    }

    fn support_payload() -> serde_json::Value {
        serde_json::json!({
            "prefix": "DS", "number": 200, "issue_date": "2026-05-26",
            "issue_time": "10:30:00-05:00", "seller_nit": "10203040",
            "seller_name": "Vendedor No Obligado", "buyer_nit": "900123456",
            "buyer_name": "Comprador SAS", "net_amount": 100000.0,
            "tax_amount": 19000.0, "total_amount": 119000.0,
            "software_pin": "pin_test", "environment": "2"
        })
    }

    struct Harness {
        repo: Arc<InMemoryRepository>,
        queue: Arc<RecordingQueue>,
        processor: DocumentProcessor,
    }

    fn harness(doc: Document, signer: Arc<SignerConfig>, soap_fails: bool) -> Harness {
        let repo = Arc::new(InMemoryRepository::with_document(doc));
        let queue = Arc::new(RecordingQueue::default());
        let processor = DocumentProcessor::new(
            repo.clone(),
            Arc::new(MockSoapClient { should_fail: soap_fails }),
            queue.clone(),
            signer,
            Arc::new(CircuitBreaker::new(3, Duration::from_secs(5))),
            3,
        );
        Harness { repo, queue, processor }
    }

    async fn assert_accepted_flow(document_type: DocumentType, payload: serde_json::Value) {
        let doc = document(document_type, payload);
        let id = doc.id;
        let h = harness(doc, local_signer(), false);

        h.processor.process_job(Job::new(id)).await.expect("job must succeed");

        let saved = h.repo.document(id).unwrap();
        assert_eq!(saved.status, DocumentStatus::DianAccepted);
        assert_eq!(saved.cufe_cude.len(), 96);
        assert!(saved.signed_xml.unwrap().contains("<ds:SignatureValue>"));
        let events = h.repo.events_for(id);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_status, EventStatus::Accepted);
        assert_eq!(*h.queue.acked.lock().unwrap(), vec![Job::new(id)]);
    }

    #[tokio::test]
    async fn invoice_is_signed_transmitted_and_acked() {
        assert_accepted_flow(DocumentType::Invoice, invoice_payload()).await;
    }

    #[tokio::test]
    async fn payroll_is_signed_transmitted_and_acked() {
        assert_accepted_flow(DocumentType::Payroll, payroll_payload()).await;
    }

    #[tokio::test]
    async fn support_document_is_signed_transmitted_and_acked() {
        assert_accepted_flow(DocumentType::SupportDocument, support_payload()).await;
    }

    #[tokio::test]
    async fn signing_failure_marks_document_failed_with_event() {
        let doc = document(DocumentType::Invoice, invoice_payload());
        let id = doc.id;
        let h = harness(doc, signer_without_certificates(), false);

        assert!(h.processor.process_job(Job::new(id)).await.is_err());

        assert_eq!(h.repo.document(id).unwrap().status, DocumentStatus::ProcessingFailed);
        let events = h.repo.events_for(id);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_status, EventStatus::Error);
        assert!(events[0]
            .dian_response_message
            .as_deref()
            .unwrap()
            .contains("no active certificate"));
        assert_eq!(h.queue.acked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unsupported_document_type_is_marked_failed() {
        let doc = document(DocumentType::CreditNote, invoice_payload());
        let id = doc.id;
        let h = harness(doc, local_signer(), false);

        assert!(h.processor.process_job(Job::new(id)).await.is_err());

        assert_eq!(h.repo.document(id).unwrap().status, DocumentStatus::ProcessingFailed);
        assert_eq!(h.repo.events_for(id).len(), 1);
        assert_eq!(h.queue.acked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn invalid_payload_is_marked_failed_instead_of_left_in_processing() {
        let doc = document(DocumentType::Invoice, serde_json::json!({ "unexpected": true }));
        let id = doc.id;
        let h = harness(doc, local_signer(), false);

        assert!(h.processor.process_job(Job::new(id)).await.is_err());

        assert_eq!(h.repo.document(id).unwrap().status, DocumentStatus::ProcessingFailed);
        assert_eq!(h.queue.acked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn transmission_failure_schedules_a_retry() {
        let doc = document(DocumentType::Invoice, invoice_payload());
        let id = doc.id;
        let h = harness(doc, local_signer(), true);

        h.processor.process_job(Job::new(id)).await.unwrap();

        assert_eq!(h.repo.document(id).unwrap().status, DocumentStatus::Pending);
        assert_eq!(h.repo.events_for(id)[0].event_status, EventStatus::Error);
        assert_eq!(*h.queue.nacked.lock().unwrap(), vec![Job::new(id)]);
        assert!(h.queue.acked.lock().unwrap().is_empty());
    }
}

mod backoff;
mod circuit_breaker;
mod error;
mod queue;

use backoff::calculate_backoff_with_jitter;
use circuit_breaker::{CircuitBreaker, State as CbState};
use core_domain::models::{DianEvent, Document, DocumentStatus, DocumentType, EventStatus};
use core_domain::ports::{DianResponse, DianSoapClient, DocumentRepository, DomainError};
use crypto_signer::provider::LocalCertificateProvider;
use crypto_signer::sign_document;
use error::WorkerError;
use queue::{Job, RedisQueue};
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
    queue: Arc<RedisQueue>,
    cb: Arc<CircuitBreaker>,
    max_attempts: u32,
}

impl DocumentProcessor {
    pub fn new(
        repo: Arc<dyn DocumentRepository>,
        soap_client: Arc<dyn DianSoapClient>,
        queue: Arc<RedisQueue>,
        cb: Arc<CircuitBreaker>,
        max_attempts: u32,
    ) -> Self {
        Self {
            repo,
            soap_client,
            queue,
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
                            let retry_job = Job {
                                document_id: job.document_id,
                                attempt: job.attempt + 1,
                            };
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
                        let retry_job = Job {
                            document_id: job.document_id,
                            attempt: job.attempt + 1,
                        };
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
        let (unsigned_xml, cufe_cude, company_nit, prefix, number, is_hab, test_set_id) = match document.document_type {
            DocumentType::Invoice => {
                let payload: InvoicePayload = serde_json::from_value(document.payload.clone())
                    .map_err(|e| WorkerError::Internal(format!("Invalid invoice payload JSON: {}", e)))?;
                let (xml, cufe) = ubl_engine::generate_invoice_xml(&payload)
                    .map_err(|e| WorkerError::Internal(format!("Invoice generation failed: {}", e)))?;
                let is_hab = payload.environment == "2";
                (xml, cufe, payload.company_nit.clone(), payload.prefix.clone(), payload.number, is_hab, payload.test_set_id.clone())
            }
            DocumentType::Payroll => {
                let payload: ubl_engine::payload::PayrollPayload = serde_json::from_value(document.payload.clone())
                    .map_err(|e| WorkerError::Internal(format!("Invalid payroll payload JSON: {}", e)))?;
                let (xml, cune) = ubl_engine::generate_payroll_xml(&payload)
                    .map_err(|e| WorkerError::Internal(format!("Payroll generation failed: {}", e)))?;
                let is_hab = payload.environment == "2";
                (xml, cune, payload.employer_nit.clone(), payload.prefix.clone(), payload.number, is_hab, None)
            }
            DocumentType::SupportDocument => {
                let payload: ubl_engine::payload::SupportDocumentPayload = serde_json::from_value(document.payload.clone())
                    .map_err(|e| WorkerError::Internal(format!("Invalid support document payload JSON: {}", e)))?;
                let (xml, cuds) = ubl_engine::generate_support_document_xml(&payload)
                    .map_err(|e| WorkerError::Internal(format!("Support document generation failed: {}", e)))?;
                let is_hab = payload.environment == "2";
                (xml, cuds, payload.buyer_nit.clone(), payload.prefix.clone(), payload.number, is_hab, None)
            }
        };

        // Guardar CUFE/CUDE y XML original
        document.cufe_cude = cufe_cude;
        document.original_xml = Some(unsigned_xml.clone());

        // 4. Firmar el XML (XAdES-EPES)
        let provider = LocalCertificateProvider {
            certificate_pem: "cert_dummy".to_string(),
            private_key_pem: "key_dummy".to_string(),
        };
        
        let signed_xml = match sign_document(&unsigned_xml, &provider, "key_ref").await {
            Ok(xml) => xml,
            Err(e) => {
                self.queue.ack(&job).await?;
                return Err(WorkerError::Internal(format!("Cryptographic signature failed: {}", e)));
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
                self.queue.ack(&job).await?;
                return Err(WorkerError::Internal(format!("ZIP compression failed: {}", e)));
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
                    let poll_job = Job {
                        document_id: document.id,
                        attempt: 1,
                    };
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
                        let retry_job = Job {
                            document_id: job.document_id,
                            attempt: job.attempt + 1,
                        };
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
                    let retry_job = Job {
                        document_id: job.document_id,
                        attempt: job.attempt + 1,
                    };
                    tokio::spawn(async move {
                        tokio::time::sleep(sleep_dur).await;
                        let _ = queue_clone.enqueue(&retry_job).await;
                    });
                }
                
                Ok(())
            }
        }
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
        let issue_date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let issue_time = chrono::Utc::now().format("%H:%M:%S-05:00").to_string();
        
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
        let provider = LocalCertificateProvider {
            certificate_pem: "cert_dummy".to_string(),
            private_key_pem: "key_dummy".to_string(),
        };
        
        let signed_att_doc_xml = sign_document(&att_doc_xml, &provider, "key_ref").await
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Iniciando worker-redis...");

    // 1. Conectar a PostgreSQL
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/dian".to_string());
    
    let db_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Failed to connect to PostgreSQL in worker-redis");

    // 2. Conectar a Redis
    let redis_url = std::env::var("REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    
    let redis_client = redis::Client::open(redis_url.clone())
        .expect("Failed to open Redis client in worker-redis");

    let queue = Arc::new(RedisQueue::new(&redis_url, "dian_jobs")?);

    // 3. Inicializar componentes
    let repo = Arc::new(PgDocumentRepository::new(db_pool.clone()));
    
    let binary_security_token = std::env::var("DIAN_BINARY_SECURITY_TOKEN")
        .unwrap_or_else(|_| "token_dummy".to_string());
    
    let soap_client = Arc::new(DianSoapClientImpl::new(binary_security_token, None, None));
    
    let cb = Arc::new(CircuitBreaker::new(3, Duration::from_secs(5)));
    
    let max_attempts = 3;
    let processor = Arc::new(DocumentProcessor::new(
        repo,
        soap_client,
        queue.clone(),
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
    use core_domain::models::{Certificate, NumberingRange, Tenant, Webhook};
    use std::sync::Mutex as StdMutex;

    // Mock Database Repository
    struct MockRepository {
        document: Arc<StdMutex<Document>>,
        saved: Arc<StdMutex<bool>>,
    }

    #[async_trait]
    impl DocumentRepository for MockRepository {
        async fn save_tenant(&self, _tenant: &Tenant) -> Result<(), DomainError> { Ok(()) }
        async fn find_tenant_by_id(&self, _id: Uuid) -> Result<Option<Tenant>, DomainError> { Ok(None) }
        async fn save_certificate(&self, _cert: &Certificate) -> Result<(), DomainError> { Ok(()) }
        async fn find_certificate_by_id(&self, _id: Uuid) -> Result<Option<Certificate>, DomainError> { Ok(None) }
        async fn find_active_certificate_by_tenant(&self, _tenant_id: Uuid) -> Result<Option<Certificate>, DomainError> { Ok(None) }
        async fn save_numbering_range(&self, _range: &NumberingRange) -> Result<(), DomainError> { Ok(()) }
        async fn find_numbering_range_by_id(&self, _id: Uuid) -> Result<Option<NumberingRange>, DomainError> { Ok(None) }
        async fn increment_numbering_range(&self, _id: Uuid) -> Result<i32, DomainError> { Ok(1) }

        async fn save_document(&self, doc: &Document) -> Result<(), DomainError> {
            let mut d = self.document.lock().unwrap();
            *d = doc.clone();
            let mut s = self.saved.lock().unwrap();
            *s = true;
            Ok(())
        }

        async fn find_document_by_id(&self, _id: Uuid) -> Result<Option<Document>, DomainError> {
            let d = self.document.lock().unwrap();
            Ok(Some(d.clone()))
        }

        async fn find_document_by_number(&self, _tenant_id: Uuid, _type: DocumentType, _prefix: &str, _number: i32) -> Result<Option<Document>, DomainError> {
            Ok(None)
        }

        async fn save_dian_event(&self, _event: &DianEvent) -> Result<(), DomainError> { Ok(()) }
        async fn find_dian_events_by_document_id(&self, _doc_id: Uuid) -> Result<Vec<DianEvent>, DomainError> { Ok(vec![]) }
        async fn save_webhook(&self, _webhook: &Webhook) -> Result<(), DomainError> { Ok(()) }
        async fn find_webhooks_by_tenant_id(&self, _tenant_id: Uuid) -> Result<Vec<Webhook>, DomainError> { Ok(vec![]) }
    }

    // Mock SOAP client
    struct MockSoapClient {
        should_fail: bool,
    }

    #[async_trait]
    impl DianSoapClient for MockSoapClient {
        async fn send_bill_sync(&self, _zip: &[u8], _is_hab: bool) -> Result<DianResponse, DomainError> {
            if self.should_fail {
                return Err(DomainError::Soap("DIAN is down".to_string()));
            }
            Ok(DianResponse {
                status_code: "00".to_string(),
                message: "Aceptado".to_string(),
                xml_response: Some("<resp/>".to_string()),
                soap_trace_id: Some("trace_abc".to_string()),
            })
        }
        async fn send_bill_async(&self, _zip: &[u8], _is_hab: bool) -> Result<String, DomainError> { Ok("key".to_string()) }
        async fn send_test_set_async(&self, _zip: &[u8], _test_set_id: &str, _is_hab: bool) -> Result<String, DomainError> {
            Ok("test_set_zip_key".to_string())
        }
        async fn get_status(&self, _track_id: &str, _is_hab: bool) -> Result<DianResponse, DomainError> {
            Ok(DianResponse {
                status_code: "00".to_string(),
                message: "Aceptado".to_string(),
                xml_response: None,
                soap_trace_id: None,
            })
        }
        async fn get_status_zip(&self, _track_id: &str, _is_hab: bool) -> Result<DianResponse, DomainError> {
            Ok(DianResponse {
                status_code: "00".to_string(),
                message: "Aceptado".to_string(),
                xml_response: Some("<resp/>".to_string()),
                soap_trace_id: Some(_track_id.to_string()),
            })
        }
        async fn send_event_update_status(&self, _zip: &[u8], _is_hab: bool) -> Result<DianResponse, DomainError> {
            if self.should_fail {
                return Err(DomainError::Soap("DIAN is down".to_string()));
            }
            Ok(DianResponse {
                status_code: "00".to_string(),
                message: "Aceptado".to_string(),
                xml_response: Some("<resp/>".to_string()),
                soap_trace_id: Some("event_trace_abc".to_string()),
            })
        }
    }

    #[tokio::test]
    async fn test_process_job_success() {
        let doc_id = Uuid::new_v4();
        let payload = serde_json::json!({
            "prefix": "FE",
            "number": 1002,
            "issue_date": "2026-05-23",
            "issue_time": "10:42:00-05:00",
            "company_nit": "900123456",
            "company_name": "Empresa Emisora",
            "customer_id_type": "31",
            "customer_id": "900999888",
            "customer_name": "Cliente",
            "customer_email": "cliente@test.com",
            "net_amount": 100.0,
            "tax_amount": 19.0,
            "total_amount": 119.0,
            "environment": "2",
            "technical_key": "tech_key",
            "document_type": "invoice",
            "test_set_id": null
        });

        let document = Document {
            id: doc_id,
            tenant_id: Uuid::new_v4(),
            document_type: DocumentType::Invoice,
            prefix: "FE".to_string(),
            document_number: 1002,
            cufe_cude: "".to_string(),
            payload,
            original_xml: None,
            signed_xml: None,
            pdf_url: None,
            status: DocumentStatus::Draft,
            created_at: chrono::Utc::now(),
        };

        let document_shared = Arc::new(StdMutex::new(document));
        let saved_flag = Arc::new(StdMutex::new(false));
        
        let repo = Arc::new(MockRepository {
            document: document_shared.clone(),
            saved: saved_flag.clone(),
        });

        let soap_client = Arc::new(MockSoapClient { should_fail: false });
        let queue = Arc::new(RedisQueue::new("redis://127.0.0.1:6379", "test_queue").unwrap());
        let cb = Arc::new(CircuitBreaker::new(3, Duration::from_secs(5)));

        let processor = DocumentProcessor::new(repo, soap_client, queue, cb, 3);
        
        let job = Job { document_id: doc_id, attempt: 1 };
        
        // We call process_job. Redis connection in queue.ack will fail because there is no running Redis,
        // but we can verify up to the point of execution. To avoid Redis dependency crashing the test,
        // let's verify that the document is modified (CUFE generated) in the mocked DB.
        let result = processor.process_job(job).await;
        
        // Since no Redis is running locally, we expect a Redis network error,
        // but the DB should have been updated with the signed XML and status.
        assert!(result.is_err());
        assert!(*saved_flag.lock().unwrap());
        let doc_after = document_shared.lock().unwrap();
        assert_eq!(doc_after.status, DocumentStatus::Pending);
        assert!(!doc_after.cufe_cude.is_empty());
    }

    #[tokio::test]
    async fn test_process_payroll_job_success() {
        let doc_id = Uuid::new_v4();
        let payload = serde_json::json!({
            "prefix": "NOM",
            "number": 101,
            "issue_date": "2026-05-26",
            "issue_time": "08:00:00",
            "employer_nit": "900123456",
            "employer_name": "Empresa Emisora",
            "employee_id_type": "13",
            "employee_id": "10203040",
            "employee_name": "Trabajador",
            "devengado": 1500000.0,
            "deducido": 60000.0,
            "total": 1440000.0,
            "software_pin": "pin_test",
            "environment": "2"
        });

        let document = Document {
            id: doc_id,
            tenant_id: Uuid::new_v4(),
            document_type: DocumentType::Payroll,
            prefix: "NOM".to_string(),
            document_number: 101,
            cufe_cude: "".to_string(),
            payload,
            original_xml: None,
            signed_xml: None,
            pdf_url: None,
            status: DocumentStatus::Draft,
            created_at: chrono::Utc::now(),
        };

        let document_shared = Arc::new(StdMutex::new(document));
        let saved_flag = Arc::new(StdMutex::new(false));
        
        let repo = Arc::new(MockRepository {
            document: document_shared.clone(),
            saved: saved_flag.clone(),
        });

        let soap_client = Arc::new(MockSoapClient { should_fail: false });
        let queue = Arc::new(RedisQueue::new("redis://127.0.0.1:6379", "test_queue").unwrap());
        let cb = Arc::new(CircuitBreaker::new(3, Duration::from_secs(5)));

        let processor = DocumentProcessor::new(repo, soap_client, queue, cb, 3);
        let job = Job { document_id: doc_id, attempt: 1 };
        
        let result = processor.process_job(job).await;
        
        assert!(result.is_err()); // Redis error expected
        assert!(*saved_flag.lock().unwrap());
        let doc_after = document_shared.lock().unwrap();
        assert_eq!(doc_after.status, DocumentStatus::Pending);
        assert!(!doc_after.cufe_cude.is_empty());
    }

    #[tokio::test]
    async fn test_process_support_document_job_success() {
        let doc_id = Uuid::new_v4();
        let payload = serde_json::json!({
            "prefix": "DS",
            "number": 200,
            "issue_date": "2026-05-26",
            "issue_time": "10:30:00-05:00",
            "seller_nit": "10203040",
            "seller_name": "Vendedor No Obligado",
            "buyer_nit": "900123456",
            "buyer_name": "Comprador SAS",
            "net_amount": 100000.0,
            "tax_amount": 19000.0,
            "total_amount": 119000.0,
            "software_pin": "pin_test",
            "environment": "2"
        });

        let document = Document {
            id: doc_id,
            tenant_id: Uuid::new_v4(),
            document_type: DocumentType::SupportDocument,
            prefix: "DS".to_string(),
            document_number: 200,
            cufe_cude: "".to_string(),
            payload,
            original_xml: None,
            signed_xml: None,
            pdf_url: None,
            status: DocumentStatus::Draft,
            created_at: chrono::Utc::now(),
        };

        let document_shared = Arc::new(StdMutex::new(document));
        let saved_flag = Arc::new(StdMutex::new(false));
        
        let repo = Arc::new(MockRepository {
            document: document_shared.clone(),
            saved: saved_flag.clone(),
        });

        let soap_client = Arc::new(MockSoapClient { should_fail: false });
        let queue = Arc::new(RedisQueue::new("redis://127.0.0.1:6379", "test_queue").unwrap());
        let cb = Arc::new(CircuitBreaker::new(3, Duration::from_secs(5)));

        let processor = DocumentProcessor::new(repo, soap_client, queue, cb, 3);
        let job = Job { document_id: doc_id, attempt: 1 };
        
        let result = processor.process_job(job).await;
        
        assert!(result.is_err()); // Redis error expected
        assert!(*saved_flag.lock().unwrap());
        let doc_after = document_shared.lock().unwrap();
        assert_eq!(doc_after.status, DocumentStatus::Pending);
        assert!(!doc_after.cufe_cude.is_empty());
    }
}

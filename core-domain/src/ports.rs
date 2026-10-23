//! Puertos del dominio: contratos que implementan los adaptadores de infraestructura.

use crate::admin::{ApiKeyRecord, AuditEntry, DeadDelivery, DeadJob, NewTenant};
use crate::jobs::{Job, NewJob};
use crate::models::{
    Certificate, DianEvent, Document, DocumentStatus, DocumentType, NumberingRange, Tenant, Webhook,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
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

    /// El tenant autenticado no puede operar sobre el recurso (p. ej. NIT emisor ajeno).
    #[error("Forbidden: {0}")]
    Forbidden(String),

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

// ---------------------------------------------------------------------------------------
// Persistencia
// ---------------------------------------------------------------------------------------

#[async_trait]
pub trait TenantRepository: Send + Sync {
    /// Tenant dueño de la API key no revocada cuya huella es `key_hash` (`api_keys::hash`).
    async fn find_by_api_key_hash(&self, key_hash: &str) -> Result<Option<Tenant>, DomainError>;
}

#[async_trait]
pub trait CertificateRepository: Send + Sync {
    async fn find_active_certificate(&self, tenant_id: Uuid) -> Result<Option<Certificate>, DomainError>;
}

/// Clave de idempotencia enviada por el cliente y huella del cuerpo de la solicitud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey {
    pub key: String,
    pub request_hash: String,
}

/// Documento emitido por el tenant, todavía sin consecutivo asignado.
#[derive(Debug, Clone)]
pub struct NewOutboundDocument {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub document_type: DocumentType,
    pub prefix: String,
    /// Consecutivo propuesto por el cliente; `None` lo asigna el servidor.
    pub requested_number: Option<i32>,
    pub payload: serde_json::Value,
    pub idempotency: Option<IdempotencyKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct IssuedDocument {
    pub document_id: Uuid,
    pub document_number: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueOutcome {
    Created(IssuedDocument),
    /// La misma clave de idempotencia ya creó este documento: se devuelve sin repetir nada.
    Replayed(IssuedDocument),
}

/// Documento recibido de un proveedor.
#[derive(Debug, Clone)]
pub struct NewInboundDocument {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub issuer_nit: String,
    pub document_type: DocumentType,
    pub prefix: String,
    pub document_number: i32,
    pub cufe: String,
    pub payload: serde_json::Value,
    pub signed_xml: String,
    pub status: DocumentStatus,
}

/// Qué hacer con el trabajo en curso al aplicar una transición.
#[derive(Debug, Clone, PartialEq)]
pub enum JobOutcome {
    Complete(Uuid),
    /// Agotó sus intentos: pasa a la cola de mensajes muertos con el motivo.
    Bury(Uuid, String),
}

/// Cambio de estado de un documento, aplicado de forma atómica junto con su evento, el
/// cierre del trabajo en curso y un posible trabajo de seguimiento. Solo se aplica si el
/// estado actual está en `DocumentStatus::allowed_predecessors(to)`.
#[derive(Debug, Clone)]
pub struct Transition {
    pub document_id: Uuid,
    pub to: DocumentStatus,
    pub event: Option<DianEvent>,
    pub job: Option<JobOutcome>,
    pub follow_up: Option<NewJob>,
}

impl Transition {
    pub fn new(document_id: Uuid, to: DocumentStatus) -> Self {
        Self {
            document_id,
            to,
            event: None,
            job: None,
            follow_up: None,
        }
    }

    pub fn with_event(mut self, event: DianEvent) -> Self {
        self.event = Some(event);
        self
    }

    pub fn with_job(mut self, outcome: JobOutcome) -> Self {
        self.job = Some(outcome);
        self
    }

    pub fn with_follow_up(mut self, job: NewJob) -> Self {
        self.follow_up = Some(job);
        self
    }
}

#[async_trait]
pub trait DocumentRepository: Send + Sync {
    /// Asigna el consecutivo (resolución vigente bloqueada), inserta el documento en
    /// `pending`, encola `initial_job` y registra la clave de idempotencia, todo en una
    /// transacción. Si la clave ya existe con la misma huella devuelve `Replayed`; con otra
    /// huella devuelve `Validation`.
    async fn issue_outbound(
        &self,
        document: NewOutboundDocument,
        initial_job: NewJob,
    ) -> Result<IssueOutcome, DomainError>;

    /// Inserta un documento recibido. Devuelve `false` si ya existía (nunca sobrescribe).
    async fn insert_inbound(&self, document: NewInboundDocument) -> Result<bool, DomainError>;

    async fn find_document(&self, id: Uuid) -> Result<Option<Document>, DomainError>;

    async fn find_tenant_document(&self, tenant_id: Uuid, id: Uuid) -> Result<Option<Document>, DomainError>;

    async fn find_numbering_range(&self, id: Uuid) -> Result<Option<NumberingRange>, DomainError>;

    /// Guarda CUFE/CUDE y XML generado y firmado, sin cambiar el estado.
    async fn store_artifacts(
        &self,
        id: Uuid,
        cufe: &str,
        original_xml: &str,
        signed_xml: &str,
    ) -> Result<(), DomainError>;

    async fn record_event(&self, event: &DianEvent) -> Result<(), DomainError>;

    async fn find_events(&self, document_id: Uuid) -> Result<Vec<DianEvent>, DomainError>;

    /// Aplica la transición si el estado actual lo permite. `Ok(false)` significa que el
    /// documento ya estaba en otro estado y no se aplicó nada.
    async fn apply_transition(&self, transition: Transition) -> Result<bool, DomainError>;
}

#[async_trait]
pub trait JobRepository: Send + Sync {
    /// Reclama hasta `limit` trabajos listos (o con lease vencido) para `worker_id`.
    async fn claim(&self, worker_id: &str, limit: i64, lease: Duration) -> Result<Vec<Job>, DomainError>;

    async fn complete(&self, job_id: Uuid) -> Result<(), DomainError>;

    /// Reprograma un intento fallido.
    async fn retry(&self, job_id: Uuid, run_at: DateTime<Utc>, error: &str) -> Result<(), DomainError>;

    /// Devuelve el trabajo a la cola sin consumir el intento (p. ej. circuito abierto).
    async fn release(&self, job_id: Uuid, run_at: DateTime<Utc>) -> Result<(), DomainError>;

    /// Mueve el trabajo a la cola de mensajes muertos.
    async fn bury(&self, job_id: Uuid, error: &str) -> Result<(), DomainError>;

    /// Devuelve a `pending` hasta `limit` documentos en contingencia sin trabajo activo y les
    /// encola una nueva transmisión. Devuelve cuántos se reprocesaron.
    async fn requeue_contingency(&self, limit: i64, max_attempts: i32) -> Result<u64, DomainError>;

    /// Trabajos pendientes por estado.
    async fn queue_depth(&self) -> Result<crate::jobs::QueueDepth, DomainError>;
}

#[async_trait]
pub trait HealthProbe: Send + Sync {
    async fn ping(&self) -> Result<(), DomainError>;
}

/// Notificación pendiente de entrega, con los datos del destino.
#[derive(Debug, Clone)]
pub struct WebhookDelivery {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub event_type: String,
    /// Datos del documento (`webhooks::notification_data`).
    pub data: serde_json::Value,
    pub attempts: i32,
    pub max_attempts: i32,
    pub created_at: DateTime<Utc>,
    pub target_url: String,
    pub secret: String,
    /// Si la suscripción sigue activa.
    pub active: bool,
}

/// Suscripciones de webhooks y cola de entregas. Las entregas se encolan en la misma
/// transacción que el cambio de estado que notifican (`DocumentRepository::apply_transition`).
#[async_trait]
pub trait WebhookRepository: Send + Sync {
    async fn create_webhook(&self, webhook: Webhook) -> Result<(), DomainError>;
    async fn list_webhooks(&self, tenant_id: Uuid) -> Result<Vec<Webhook>, DomainError>;
    /// Borra la suscripción (y sus entregas pendientes). `false` si no existe para el tenant.
    async fn delete_webhook(&self, tenant_id: Uuid, webhook_id: Uuid) -> Result<bool, DomainError>;

    /// Reclama hasta `limit` entregas listas (o con lease vencido).
    async fn claim_deliveries(&self, limit: i64, lease: Duration) -> Result<Vec<WebhookDelivery>, DomainError>;
    async fn complete_delivery(&self, delivery_id: Uuid, status_code: u16) -> Result<(), DomainError>;
    async fn retry_delivery(
        &self,
        delivery_id: Uuid,
        run_at: DateTime<Utc>,
        error: &str,
        status_code: Option<u16>,
    ) -> Result<(), DomainError>;
    async fn bury_delivery(&self, delivery_id: Uuid, error: &str, status_code: Option<u16>) -> Result<(), DomainError>;
}

/// Operaciones del operador de la plataforma. Cada operación que modifica datos registra su
/// `AuditEntry` en la misma transacción.
#[async_trait]
pub trait AdminRepository: Send + Sync {
    /// `Conflict` si ya existe un tenant con ese NIT.
    async fn create_tenant(&self, tenant: NewTenant, audit: &AuditEntry) -> Result<Tenant, DomainError>;
    async fn find_tenant(&self, tenant_id: Uuid) -> Result<Option<Tenant>, DomainError>;

    async fn create_api_key(&self, key: ApiKeyRecord, audit: &AuditEntry) -> Result<(), DomainError>;
    async fn list_api_keys(&self, tenant_id: Uuid) -> Result<Vec<ApiKeyRecord>, DomainError>;
    /// `false` si la clave no existe para el tenant o ya estaba revocada.
    async fn revoke_api_key(&self, tenant_id: Uuid, key_id: Uuid, audit: &AuditEntry) -> Result<bool, DomainError>;

    /// Registra el certificado como el activo del tenant y revoca el anterior.
    async fn activate_certificate(&self, certificate: Certificate, audit: &AuditEntry) -> Result<(), DomainError>;
    /// `Conflict` si se solapa en vigencia con otro rango del mismo tipo y prefijo.
    async fn add_numbering_range(&self, range: NumberingRange, audit: &AuditEntry) -> Result<(), DomainError>;

    async fn list_dead_jobs(&self, limit: i64) -> Result<Vec<DeadJob>, DomainError>;
    /// Devuelve a la cola un trabajo muerto (intentos a cero). `false` si no está muerto;
    /// `Conflict` si el estado del documento ya no admite ese trabajo o hay otro activo.
    async fn retry_dead_job(&self, job_id: Uuid, audit: &AuditEntry) -> Result<bool, DomainError>;

    async fn list_dead_deliveries(&self, limit: i64) -> Result<Vec<DeadDelivery>, DomainError>;
    /// Devuelve a la cola una entrega muerta. `false` si no está muerta.
    async fn retry_dead_delivery(&self, delivery_id: Uuid, audit: &AuditEntry) -> Result<bool, DomainError>;
}

// ---------------------------------------------------------------------------------------
// Servicios externos
// ---------------------------------------------------------------------------------------

#[async_trait]
pub trait CryptoSigner: Send + Sync {
    /// Firma el XML (XAdES-EPES) con el certificado que corresponde al tenant.
    async fn sign_for_tenant(&self, tenant_id: Uuid, xml: &str) -> Result<String, DomainError>;

    /// Completa la cabecera WS-Security de un envelope SOAP con el certificado del tenant:
    /// sustituye [`WSSE_SECURITY_PLACEHOLDER`] por `wsu:Timestamp`, `wsse:BinarySecurityToken`
    /// y una `ds:Signature` que cubre el Timestamp y cada elemento que lleve `wsu:Id`.
    async fn sign_soap_for_tenant(&self, tenant_id: Uuid, envelope: &str) -> Result<String, DomainError>;
}

/// Resultado de una verificación de firma exitosa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSignature {
    /// Sujeto (RFC 4514) del certificado firmante.
    pub signer_subject: String,
}

#[async_trait]
pub trait SignatureVerifier: Send + Sync {
    /// Verifica criptográficamente la firma XML-DSig/XAdES envolvente del documento (que debe
    /// cubrirlo completo) y la cadena de confianza de su certificado. Una firma inválida o no
    /// confiable es `DomainError::Validation`.
    async fn verify(&self, xml: &str) -> Result<VerifiedSignature, DomainError>;
}

#[async_trait]
pub trait WebhookSender: Send + Sync {
    /// Envía `body` (JSON) por POST con las cabeceras dadas y devuelve el código HTTP. Un
    /// destino no permitido o un fallo de red es un error.
    async fn post(&self, url: &str, headers: &[(&str, String)], body: &str) -> Result<u16, DomainError>;
}

/// Marcador que el cliente SOAP coloca dentro de `wsse:Security` para que el firmador
/// inserte allí el Timestamp, el certificado y la firma.
pub const WSSE_SECURITY_PLACEHOLDER: &str = "<!-- WSSE_SECURITY -->";

#[async_trait]
pub trait DianSoapClient: Send + Sync {
    async fn send_bill_sync(
        &self,
        tenant_id: Uuid,
        signed_xml_zip: &[u8],
        is_hab: bool,
    ) -> Result<DianResponse, DomainError>;

    async fn send_bill_async(
        &self,
        tenant_id: Uuid,
        signed_xml_zip: &[u8],
        is_hab: bool,
    ) -> Result<String, DomainError>; // Returns TrackId

    async fn send_test_set_async(
        &self,
        tenant_id: Uuid,
        signed_xml_zip: &[u8],
        test_set_id: &str,
        is_hab: bool,
    ) -> Result<String, DomainError>; // Returns zipKey/TrackId

    async fn get_status(
        &self,
        tenant_id: Uuid,
        track_id: &str,
        is_hab: bool,
    ) -> Result<DianResponse, DomainError>;

    async fn get_status_zip(
        &self,
        tenant_id: Uuid,
        track_id: &str,
        is_hab: bool,
    ) -> Result<DianResponse, DomainError>;

    async fn send_event_update_status(
        &self,
        tenant_id: Uuid,
        signed_event_zip: &[u8],
        is_hab: bool,
    ) -> Result<DianResponse, DomainError>;
}

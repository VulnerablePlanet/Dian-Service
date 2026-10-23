//! Tipos de la administración del servicio (operador de la plataforma).

use crate::models::JobKind;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct NewTenant {
    pub id: Uuid,
    pub tax_id: String,
    pub registration_name: String,
    pub tax_regime: String,
    pub reception_email: String,
}

/// API key persistida (sin la clave).
#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyRecord {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub prefix: String,
    #[serde(skip)]
    pub key_hash: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Trabajo que agotó sus intentos.
#[derive(Debug, Clone, Serialize)]
pub struct DeadJob {
    pub id: Uuid,
    pub kind: JobKind,
    pub document_id: Uuid,
    pub tenant_id: Uuid,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub failed_at: DateTime<Utc>,
}

/// Entrega de webhook que agotó sus intentos.
#[derive(Debug, Clone, Serialize)]
pub struct DeadDelivery {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub webhook_id: Uuid,
    pub event_type: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub last_status_code: Option<i32>,
    pub failed_at: DateTime<Utc>,
}

/// Entrada de la bitácora administrativa; se escribe en la misma transacción que la acción.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    pub action: &'static str,
    pub tenant_id: Option<Uuid>,
    pub subject_id: Option<Uuid>,
    pub details: Value,
}

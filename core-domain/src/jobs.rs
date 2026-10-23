//! Trabajos de la cola persistente (tabla `jobs`).
//!
//! La cola vive en la misma base de datos que los documentos: el trabajo inicial se inserta
//! en la misma transacción que el documento, por lo que no existe un documento sin trabajo
//! ni un trabajo sin documento.

use crate::models::JobKind;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

/// Trabajo reclamado por un worker. `attempts` ya incluye el intento en curso.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: Uuid,
    pub kind: JobKind,
    pub document_id: Uuid,
    pub payload: Value,
    pub attempts: i32,
    pub max_attempts: i32,
}

impl Job {
    pub fn is_last_attempt(&self) -> bool {
        self.attempts >= self.max_attempts
    }

    /// Clave de seguimiento DIAN (`ZipKey`) de un trabajo de consulta de estado.
    pub fn zip_key(&self) -> Option<&str> {
        self.payload.get("zip_key").and_then(Value::as_str)
    }
}

/// Trabajos pendientes por estado, para métricas y alertas.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueDepth {
    /// En cola y listos para ejecutarse.
    pub ready: i64,
    /// En cola con ejecución programada para más tarde (reintentos, consultas de estado).
    pub scheduled: i64,
    pub running: i64,
    /// Agotaron sus intentos (cola de errores): requieren intervención.
    pub dead: i64,
}

/// Trabajo por encolar.
#[derive(Debug, Clone, PartialEq)]
pub struct NewJob {
    pub id: Uuid,
    pub kind: JobKind,
    pub document_id: Uuid,
    pub payload: Value,
    pub max_attempts: i32,
    pub run_at: DateTime<Utc>,
}

impl NewJob {
    pub fn transmit(document_id: Uuid, max_attempts: i32) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind: JobKind::Transmit,
            document_id,
            payload: json!({}),
            max_attempts,
            run_at: Utc::now(),
        }
    }

    pub fn poll_status(document_id: Uuid, zip_key: &str, max_attempts: i32, run_at: DateTime<Utc>) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind: JobKind::PollStatus,
            document_id,
            payload: json!({ "zip_key": zip_key }),
            max_attempts,
            run_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_job_carries_zip_key() {
        let new = NewJob::poll_status(Uuid::new_v4(), "zk-1", 10, Utc::now());
        let job = Job {
            id: new.id,
            kind: new.kind,
            document_id: new.document_id,
            payload: new.payload,
            attempts: 1,
            max_attempts: new.max_attempts,
        };
        assert_eq!(job.zip_key(), Some("zk-1"));
        assert!(!job.is_last_attempt());
    }
}

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Mensaje que viaja por la cola de documentos entre `api-server` y `worker-redis`.
///
/// La cola confirma (ACK) los trabajos eliminando la cadena serializada exacta de la
/// lista de procesamiento, por lo que productor y consumidor deben serializar con este
/// mismo tipo y con [`DocumentJob::to_payload`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentJob {
    pub document_id: Uuid,
    pub attempt: u32,
}

impl DocumentJob {
    pub fn new(document_id: Uuid) -> Self {
        Self {
            document_id,
            attempt: 1,
        }
    }

    pub fn retry(&self) -> Self {
        Self {
            document_id: self.document_id,
            attempt: self.attempt + 1,
        }
    }

    pub fn to_payload(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_payload(payload: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_roundtrip_is_byte_identical() {
        let job = DocumentJob::new(Uuid::new_v4());
        let payload = job.to_payload().unwrap();
        let parsed = DocumentJob::from_payload(&payload).unwrap();
        assert_eq!(parsed, job);
        assert_eq!(parsed.to_payload().unwrap(), payload);
    }

    #[test]
    fn payload_field_order_is_stable() {
        let id = Uuid::nil();
        let payload = DocumentJob { document_id: id, attempt: 3 }.to_payload().unwrap();
        assert_eq!(
            payload,
            r#"{"document_id":"00000000-0000-0000-0000-000000000000","attempt":3}"#
        );
    }

    #[test]
    fn retry_increments_attempt() {
        let job = DocumentJob::new(Uuid::new_v4());
        assert_eq!(job.retry().attempt, 2);
        assert_eq!(job.retry().document_id, job.document_id);
    }
}

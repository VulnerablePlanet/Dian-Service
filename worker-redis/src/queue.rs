use crate::error::WorkerError;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Job {
    pub document_id: Uuid,
    pub attempt: u32,
}

pub struct RedisQueue {
    client: redis::Client,
    queue_name: String,
    processing_queue_name: String,
}

impl RedisQueue {
    pub fn new(redis_url: &str, queue_name: &str) -> Result<Self, WorkerError> {
        let client = redis::Client::open(redis_url)
            .map_err(|e| WorkerError::Redis(e))?;
        let processing_queue_name = format!("{}:processing", queue_name);
        
        Ok(Self {
            client,
            queue_name: queue_name.to_string(),
            processing_queue_name,
        })
    }

    /// Encola un trabajo en la cola principal.
    pub async fn enqueue(&self, job: &Job) -> Result<(), WorkerError> {
        let mut conn = self.client.get_async_connection().await?;
        let payload = serde_json::to_string(job)?;
        let _: () = conn.lpush(&self.queue_name, payload).await?;
        Ok(())
    }

    /// Extrae un trabajo de forma confiable (RPOPLPUSH bloqueante) moviéndolo a la cola de procesamiento.
    /// Retorna `None` si la cola está vacía tras expiración del timeout de bloqueo.
    pub async fn dequeue_blocking(&self, timeout_sec: f64) -> Result<Option<Job>, WorkerError> {
        let mut conn = self.client.get_async_connection().await?;
        
        // Usamos BRPOPLPUSH para bloquear de forma segura sin polling activo
        let result: Option<String> = redis::cmd("BRPOPLPUSH")
            .arg(&self.queue_name)
            .arg(&self.processing_queue_name)
            .arg(timeout_sec)
            .query_async(&mut conn)
            .await?;

        match result {
            Some(payload) => {
                let job: Job = serde_json::from_str(&payload)?;
                Ok(Some(job))
            }
            None => Ok(None),
        }
    }

    /// Confirma la finalización exitosa de un trabajo (ACK). Lo elimina de la cola de procesamiento.
    pub async fn ack(&self, job: &Job) -> Result<(), WorkerError> {
        let mut conn = self.client.get_async_connection().await?;
        let payload = serde_json::to_string(job)?;
        let _: () = conn.lrem(&self.processing_queue_name, 1, payload).await?;
        Ok(())
    }

    /// Reporta fallo de procesamiento (NACK). Lo elimina de procesamiento y opcionalmente lo re-encola en la cola principal.
    pub async fn nack(&self, job: &Job, requeue: bool) -> Result<(), WorkerError> {
        let mut conn = self.client.get_async_connection().await?;
        let payload = serde_json::to_string(job)?;
        let _: () = conn.lrem(&self.processing_queue_name, 1, &payload).await?;
        
        if requeue {
            let _: () = conn.lpush(&self.queue_name, &payload).await?;
        }
        Ok(())
    }
}

use crate::error::WorkerError;
use async_trait::async_trait;
use redis::AsyncCommands;

/// El contrato del mensaje se comparte con `api-server` para que el ACK (que elimina la
/// cadena serializada exacta) encuentre siempre el trabajo.
pub use core_domain::jobs::DocumentJob as Job;

#[async_trait]
pub trait JobQueue: Send + Sync {
    /// Encola un trabajo en la cola principal.
    async fn enqueue(&self, job: &Job) -> Result<(), WorkerError>;
    /// Confirma la finalización de un trabajo eliminándolo de la cola de procesamiento.
    async fn ack(&self, job: &Job) -> Result<(), WorkerError>;
    /// Elimina el trabajo de procesamiento y opcionalmente lo re-encola.
    async fn nack(&self, job: &Job, requeue: bool) -> Result<(), WorkerError>;
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
            Some(payload) => Ok(Some(Job::from_payload(&payload)?)),
            None => Ok(None),
        }
    }
}

#[async_trait]
impl JobQueue for RedisQueue {
    async fn enqueue(&self, job: &Job) -> Result<(), WorkerError> {
        let mut conn = self.client.get_async_connection().await?;
        let _: () = conn.lpush(&self.queue_name, job.to_payload()?).await?;
        Ok(())
    }

    async fn ack(&self, job: &Job) -> Result<(), WorkerError> {
        let mut conn = self.client.get_async_connection().await?;
        let _: () = conn.lrem(&self.processing_queue_name, 1, job.to_payload()?).await?;
        Ok(())
    }

    async fn nack(&self, job: &Job, requeue: bool) -> Result<(), WorkerError> {
        let mut conn = self.client.get_async_connection().await?;
        let payload = job.to_payload()?;
        let _: () = conn.lrem(&self.processing_queue_name, 1, &payload).await?;
        
        if requeue {
            let _: () = conn.lpush(&self.queue_name, &payload).await?;
        }
        Ok(())
    }
}

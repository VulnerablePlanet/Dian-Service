//! Worker de procesamiento: reclama trabajos de la cola persistente (tabla `jobs`) y los
//! procesa con los casos de uso de `application`.
//!
//! - Concurrencia acotada por `MAX_CONCURRENT_JOBS`.
//! - Cada trabajo reclamado tiene un lease: si el proceso muere, otro worker lo retoma
//!   cuando el lease vence.
//! - Un reconciliador periódico devuelve a la cola los documentos en contingencia.
//! - Ante SIGTERM/SIGINT deja de reclamar y espera a que terminen los trabajos en curso.

use application::processing::{DocumentProcessor, ProcessorConfig};
use application::resilience::Breakers;
use core_domain::ports::JobRepository;
use crypto_signer::signer::{SignerConfig, TenantSigner};
use dian_soap_client::client::DianSoapClientImpl;
use infra_postgres::PgStore;
use sqlx::postgres::PgPoolOptions;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Semaphore};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

struct WorkerConfig {
    database_url: String,
    database_max_connections: u32,
    binary_security_token: String,
    dian_hab_url: Option<String>,
    dian_prod_url: Option<String>,
    worker_id: String,
    max_concurrent_jobs: usize,
    poll_interval: Duration,
    job_lease: Duration,
    job_timeout: Duration,
    shutdown_grace: Duration,
    contingency_interval: Duration,
    breaker_threshold: u32,
    breaker_cooldown: Duration,
    processor: ProcessorConfig,
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn required_env(name: &str) -> Result<String, String> {
    optional_env(name).ok_or_else(|| format!("Missing required environment variable {}", name))
}

fn parsed_env<T: FromStr>(name: &str, default: T) -> Result<T, String> {
    match optional_env(name) {
        None => Ok(default),
        Some(value) => value
            .parse()
            .map_err(|_| format!("Invalid value for {}: '{}'", name, value)),
    }
}

fn seconds_env(name: &str, default: u64) -> Result<Duration, String> {
    parsed_env(name, default).map(Duration::from_secs)
}

impl WorkerConfig {
    fn from_env() -> Result<Self, String> {
        let job_lease = seconds_env("JOB_LEASE_SECS", 600)?;
        let job_timeout = seconds_env("JOB_TIMEOUT_SECS", 540)?;
        if job_timeout >= job_lease {
            return Err("JOB_TIMEOUT_SECS must be lower than JOB_LEASE_SECS".to_string());
        }
        Ok(Self {
            database_url: required_env("DATABASE_URL")?,
            database_max_connections: parsed_env("DATABASE_MAX_CONNECTIONS", 10)?,
            binary_security_token: required_env("DIAN_BINARY_SECURITY_TOKEN")?,
            dian_hab_url: optional_env("DIAN_HAB_URL"),
            dian_prod_url: optional_env("DIAN_PROD_URL"),
            worker_id: optional_env("WORKER_ID").unwrap_or_else(|| {
                format!(
                    "{}-{}",
                    optional_env("HOSTNAME").unwrap_or_else(|| "worker".to_string()),
                    std::process::id()
                )
            }),
            max_concurrent_jobs: parsed_env("MAX_CONCURRENT_JOBS", 5usize)?.max(1),
            poll_interval: Duration::from_millis(parsed_env("POLL_INTERVAL_MS", 1000)?),
            job_lease,
            job_timeout,
            shutdown_grace: seconds_env("SHUTDOWN_GRACE_SECS", 120)?,
            contingency_interval: seconds_env("CONTINGENCY_REQUEUE_INTERVAL_SECS", 300)?,
            breaker_threshold: parsed_env("BREAKER_FAILURE_THRESHOLD", 3)?,
            breaker_cooldown: seconds_env("BREAKER_COOLDOWN_SECS", 30)?,
            processor: ProcessorConfig {
                transmit_max_attempts: parsed_env("TRANSMIT_MAX_ATTEMPTS", 5)?,
                poll_max_attempts: parsed_env("POLL_MAX_ATTEMPTS", 30)?,
                ..ProcessorConfig::default()
            },
        })
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn")))
        .init();

    let config = WorkerConfig::from_env().unwrap_or_else(|e| exit_with(&e));
    let signer_config = SignerConfig::from_env().unwrap_or_else(|e| exit_with(&e.to_string()));

    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&config.database_url)
        .await
        .unwrap_or_else(|e| exit_with(&format!("Failed to connect to PostgreSQL: {}", e)));
    let store = Arc::new(PgStore::new(pool));

    let signer = Arc::new(TenantSigner::new(signer_config, store.clone()));
    info!(signing_mode = ?signer.mode(), "Firma configurada");
    let soap = Arc::new(DianSoapClientImpl::new(
        config.binary_security_token.clone(),
        config.dian_hab_url.clone(),
        config.dian_prod_url.clone(),
    ));
    let processor = Arc::new(DocumentProcessor::new(
        store.clone(),
        store.clone(),
        signer,
        soap,
        Arc::new(Breakers::new(config.breaker_threshold, config.breaker_cooldown)),
        config.processor.clone(),
    ));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    tokio::spawn(contingency_reconciler(
        processor.clone(),
        config.contingency_interval,
        shutdown_rx.clone(),
    ));

    info!(
        worker_id = %config.worker_id,
        max_concurrent_jobs = config.max_concurrent_jobs,
        "Worker listo"
    );
    let semaphore = Arc::new(Semaphore::new(config.max_concurrent_jobs));
    claim_loop(&config, store, processor, semaphore.clone(), shutdown_rx).await;

    info!("Apagado: esperando a que terminen los trabajos en curso");
    let all = u32::try_from(config.max_concurrent_jobs).unwrap_or(u32::MAX);
    let drained = tokio::time::timeout(config.shutdown_grace, semaphore.acquire_many(all))
        .await
        .is_ok();
    if drained {
        info!("Worker detenido sin trabajos pendientes");
    } else {
        warn!("Trabajos aun en curso al vencer SHUTDOWN_GRACE_SECS; se retomaran al vencer su lease");
    }
}

async fn claim_loop(
    config: &WorkerConfig,
    store: Arc<PgStore>,
    processor: Arc<DocumentProcessor>,
    semaphore: Arc<Semaphore>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }

        let free = semaphore.available_permits();
        if free == 0 {
            tokio::select! {
                permit = semaphore.clone().acquire_owned() => drop(permit),
                _ = shutdown.changed() => {}
            }
            continue;
        }

        match store.claim(&config.worker_id, free as i64, config.job_lease).await {
            Ok(jobs) if !jobs.is_empty() => {
                for job in jobs {
                    let Ok(permit) = semaphore.clone().acquire_owned().await else { return };
                    let processor = processor.clone();
                    let timeout = config.job_timeout;
                    tokio::spawn(async move {
                        let _permit = permit;
                        let (job_id, document_id, kind) = (job.id, job.document_id, job.kind);
                        match tokio::time::timeout(timeout, processor.process(job)).await {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => {
                                error!(%job_id, %document_id, %kind, error = %e, "job failed; it will be retried when its lease expires")
                            }
                            Err(_) => {
                                error!(%job_id, %document_id, %kind, "job timed out; it will be retried when its lease expires")
                            }
                        }
                    });
                }
            }
            Ok(_) => idle(config.poll_interval, &mut shutdown).await,
            Err(e) => {
                error!(error = %e, "could not claim jobs");
                idle(Duration::from_secs(5), &mut shutdown).await;
            }
        }
    }
}

async fn idle(duration: Duration, shutdown: &mut watch::Receiver<bool>) {
    tokio::select! {
        _ = tokio::time::sleep(duration) => {}
        _ = shutdown.changed() => {}
    }
}

async fn contingency_reconciler(
    processor: Arc<DocumentProcessor>,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        idle(interval, &mut shutdown).await;
        if *shutdown.borrow() {
            return;
        }
        match processor.requeue_contingency(100).await {
            Ok(0) => {}
            Ok(count) => info!(count, "documentos en contingencia devueltos a la cola"),
            Err(e) => error!(error = %e, "contingency reconciliation failed"),
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("Senal de apagado recibida");
}

fn exit_with(message: &str) -> ! {
    eprintln!("Configuracion invalida: {}", message);
    std::process::exit(1);
}

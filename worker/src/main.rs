//! Worker de procesamiento: reclama trabajos de la cola persistente (tabla `jobs`) y los
//! procesa con los casos de uso de `application`.
//!
//! - Concurrencia acotada por `MAX_CONCURRENT_JOBS`.
//! - Cada trabajo reclamado tiene un lease: si el proceso muere, otro worker lo retoma
//!   cuando el lease vence.
//! - Un reconciliador periódico devuelve a la cola los documentos en contingencia.
//! - Ante SIGTERM/SIGINT deja de reclamar y espera a que terminen los trabajos en curso.
//! - Logs JSON con un span por trabajo; `/livez`, `/readyz` y `/metrics` en `METRICS_ADDR`.

mod metrics;
mod webhook_http;

use application::processing::{DocumentProcessor, ProcessorConfig};
use application::resilience::Breakers;
use application::webhooks::{DispatchConfig, WebhookDispatcher};
use core_domain::ports::JobRepository;
use crypto_signer::signer::{SignerConfig, TenantSigner};
use dian_soap_client::client::DianSoapClientImpl;
use infra_postgres::PgStore;
use sqlx::postgres::PgPoolOptions;
use metrics::WorkerMetrics;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{watch, Semaphore};
use tracing::{error, info, info_span, warn, Instrument};

struct WorkerConfig {
    database_url: String,
    database_max_connections: u32,
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
    metrics_addr: Option<SocketAddr>,
    webhooks: webhook_http::HttpWebhookSender,
    processor: ProcessorConfig,
}

fn flag_env(name: &str) -> bool {
    optional_env(name).is_some_and(|v| v.eq_ignore_ascii_case("true"))
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
            // `off` desactiva el listener de sondas y métricas.
            metrics_addr: match optional_env("METRICS_ADDR").as_deref().unwrap_or("0.0.0.0:9091") {
                "off" => None,
                value => Some(value.parse().map_err(|_| format!("Invalid METRICS_ADDR '{}'", value))?),
            },
            // Solo desarrollo: entregar webhooks por http:// o a redes privadas.
            webhooks: webhook_http::HttpWebhookSender {
                allow_http: flag_env("WEBHOOK_ALLOW_HTTP"),
                allow_private_networks: flag_env("WEBHOOK_ALLOW_PRIVATE_NETWORKS"),
            },
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
    observability::init_tracing("info,sqlx=warn");

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
    // Los envelopes SOAP se firman (WS-Security) con el mismo certificado del tenant.
    let soap = Arc::new(DianSoapClientImpl::new(
        signer.clone(),
        config.dian_hab_url.clone(),
        config.dian_prod_url.clone(),
    ));
    let breakers = Arc::new(Breakers::new(config.breaker_threshold, config.breaker_cooldown));
    let processor = Arc::new(DocumentProcessor::new(
        store.clone(),
        store.clone(),
        signer.clone(),
        soap,
        breakers.clone(),
        config.processor.clone(),
    ));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    let metrics = Arc::new(WorkerMetrics::default());
    if let Some(addr) = config.metrics_addr {
        let mut registry = observability::prometheus_client::registry::Registry::with_prefix("dian_worker");
        metrics.register(&mut registry);
        let router = axum::Router::new()
            .merge(observability::probe_routes(store.clone()))
            .merge(observability::metrics_routes(Arc::new(registry)));
        let mut stop = shutdown_rx.clone();
        tokio::spawn(async move {
            info!(%addr, "Sondas y metricas en /livez, /readyz y /metrics");
            let shutdown = async move {
                let _ = stop.wait_for(|stopping| *stopping).await;
            };
            if let Err(e) = observability::serve(addr, router, shutdown).await {
                error!(error = %e, "metrics listener failed");
            }
        });
    }
    tokio::spawn(sampler(store.clone(), breakers, metrics.clone(), shutdown_rx.clone()));
    let dispatcher = Arc::new(WebhookDispatcher::new(
        store.clone(),
        Arc::new(config.webhooks),
        DispatchConfig::default(),
    ));
    let webhooks = tokio::spawn(webhook_loop(dispatcher, metrics.clone(), config.poll_interval, shutdown_rx.clone()));

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
    claim_loop(&config, store, processor, metrics, semaphore.clone(), shutdown_rx).await;

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
    // Una entrega interrumpida se reenvía al vencer su lease (el receptor deduplica por id).
    if tokio::time::timeout(Duration::from_secs(15), webhooks).await.is_err() {
        warn!("Entregas de webhooks aun en curso al apagar; se reenviaran al vencer su lease");
    }
}

/// Entrega las notificaciones pendientes. Si una pasada llena el lote, sigue sin esperar.
async fn webhook_loop(
    dispatcher: Arc<WebhookDispatcher>,
    metrics: Arc<WorkerMetrics>,
    poll_interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    const BATCH: i64 = 20;
    loop {
        if *shutdown.borrow() {
            return;
        }
        match dispatcher.run_once(BATCH).await {
            Ok(stats) => {
                metrics.webhooks_dispatched(stats);
                if stats.claimed as i64 == BATCH {
                    continue;
                }
            }
            Err(e) => error!(error = %e, "webhook dispatch failed"),
        }
        idle(poll_interval, &mut shutdown).await;
    }
}

async fn claim_loop(
    config: &WorkerConfig,
    store: Arc<PgStore>,
    processor: Arc<DocumentProcessor>,
    metrics: Arc<WorkerMetrics>,
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
                    let metrics = metrics.clone();
                    let timeout = config.job_timeout;
                    // Todo log emitido al procesar el trabajo lleva estos campos.
                    let span = info_span!(
                        "job",
                        job_id = %job.id,
                        document_id = %job.document_id,
                        kind = %job.kind,
                        attempt = job.attempts
                    );
                    tokio::spawn(
                        async move {
                            let _permit = permit;
                            let kind = job.kind.to_string();
                            metrics.in_flight.inc();
                            let started = Instant::now();
                            let outcome = match tokio::time::timeout(timeout, processor.process(job)).await {
                                // Procesado (incluye reintentos programados); el resultado ante la
                                // DIAN queda en el estado del documento y en sus eventos.
                                Ok(Ok(())) => "handled",
                                Ok(Err(e)) => {
                                    error!(error = %e, "job failed; it will be retried when its lease expires");
                                    "error"
                                }
                                Err(_) => {
                                    error!("job timed out; it will be retried when its lease expires");
                                    "timeout"
                                }
                            };
                            metrics.in_flight.dec();
                            metrics.job_finished(&kind, outcome, started.elapsed().as_secs_f64());
                        }
                        .instrument(span),
                    );
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

/// Muestrea periódicamente la profundidad de la cola y el estado de los circuit breakers.
async fn sampler(
    store: Arc<PgStore>,
    breakers: Arc<Breakers>,
    metrics: Arc<WorkerMetrics>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        match store.queue_depth().await {
            Ok(depth) => metrics.set_queue_depth(depth),
            Err(e) => warn!(error = %e, "could not sample queue depth"),
        }
        metrics.sample_breakers(&breakers).await;
        idle(Duration::from_secs(15), &mut shutdown).await;
        if *shutdown.borrow() {
            return;
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

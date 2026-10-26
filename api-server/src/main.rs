use api_server::config::{optional_env, parsed_env, required_env, ApiConfig};
use api_server::state::AppState;
use api_server::metrics::HttpMetrics;
use application::admin::AdminService;
use application::inbound::{InboundService, InboundVerification};
use application::issue::IssueService;
use application::radian::RadianService;
use application::webhooks::WebhookService;
use crypto_signer::signer::{SignerConfig, TenantSigner};
use crypto_signer::verify::XmlSignatureVerifier;
use dian_soap_client::client::DianSoapClientImpl;
use infra_postgres::PgStore;
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    observability::init_tracing("info,api_server=debug,sqlx=warn");

    // Configuración obligatoria: sin valores por defecto para conexiones ni secretos.
    let db_url = required_env("DATABASE_URL").unwrap_or_else(|e| exit_with(&e));
    let config = ApiConfig::from_env().unwrap_or_else(|e| exit_with(&e));
    let signer_config = SignerConfig::from_env().unwrap_or_else(|e| exit_with(&e.to_string()));
    let max_connections: u32 = parsed_env("DATABASE_MAX_CONNECTIONS", 10).unwrap_or_else(|e| exit_with(&e));
    // Verificación de facturas recibidas: requiere xmlsec1 y el bundle de CAs (se comprueba al arrancar).
    let inbound_verification = config.inbound_trusted_ca_pem.as_ref().map(|ca| {
        let mut verifier = XmlSignatureVerifier::new(ca);
        if let Some(intermediates) = &config.inbound_intermediates_pem {
            verifier = verifier.with_intermediates(intermediates);
        }
        verifier
            .check_ready()
            .unwrap_or_else(|e| exit_with(&format!("Inbound signature verification: {}", e)));
        InboundVerification {
            verifier: Arc::new(verifier),
            dian_signer_subject: config.inbound_dian_signer_subject.clone(),
        }
    });
    if inbound_verification.is_none() && config.inbound_allow_unverified_signatures {
        tracing::warn!("INBOUND_ALLOW_UNVERIFIED_SIGNATURES=true: facturas recibidas sin verificacion criptografica");
    }

    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .unwrap_or_else(|e| exit_with(&format!("Failed to connect to PostgreSQL: {}", e)));
    let store = Arc::new(PgStore::new(pool));

    let signer = Arc::new(TenantSigner::new(signer_config, store.clone()));
    tracing::info!(signing_mode = ?signer.mode(), "Firma configurada");
    // Los envelopes SOAP se firman (WS-Security) con el certificado del tenant.
    let soap = Arc::new(DianSoapClientImpl::new(
        signer.clone(),
        config.dian_hab_url.clone(),
        config.dian_prod_url.clone(),
    ));

    let metrics = Arc::new(HttpMetrics::default());
    let mut registry = observability::prometheus_client::registry::Registry::with_prefix("dian_api");
    metrics.register(&mut registry);
    let metrics_addr = metrics_addr("0.0.0.0:9090").unwrap_or_else(|e| exit_with(&e));

    let state = Arc::new(AppState {
        tenants: store.clone(),
        documents: store.clone(),
        health: store.clone(),
        issue: IssueService::new(store.clone(), config.transmit_max_attempts),
        inbound: InboundService::new(store.clone(), inbound_verification),
        radian: RadianService::new(store.clone(), signer, soap),
        webhooks: WebhookService::new(store.clone(), config.webhook_policy),
        admin: AdminService::new(store.clone()),
        config,
        metrics,
    });

    // Métricas en un puerto aparte, para no exponerlas en el listener público.
    if let Some(addr) = metrics_addr {
        let router = observability::metrics_routes(Arc::new(registry));
        tokio::spawn(async move {
            tracing::info!(%addr, "Metricas expuestas en /metrics");
            if let Err(e) = observability::serve(addr, router, shutdown_signal()).await {
                tracing::error!(error = %e, "metrics listener failed");
            }
        });
    }

    let port = optional_env("PORT").unwrap_or_else(|| "8080".to_string());
    let addr: SocketAddr = format!("0.0.0.0:{}", port)
        .parse()
        .unwrap_or_else(|_| exit_with(&format!("Invalid PORT '{}'", port)));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| exit_with(&format!("Failed to bind {}: {}", addr, e)));
    tracing::info!(%addr, "Servidor REST escuchando");

    axum::serve(listener, api_server::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap_or_else(|e| exit_with(&format!("Axum server failed: {}", e)));
    tracing::info!("Servidor REST detenido");
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
    tracing::info!("Senal de apagado recibida: terminando solicitudes en curso");
}

/// `METRICS_ADDR`: dirección del listener de métricas; `off` lo desactiva.
fn metrics_addr(default: &str) -> Result<Option<SocketAddr>, String> {
    match optional_env("METRICS_ADDR").as_deref().unwrap_or(default) {
        "off" => Ok(None),
        value => value
            .parse()
            .map(Some)
            .map_err(|_| format!("Invalid METRICS_ADDR '{}'", value)),
    }
}

fn exit_with(message: &str) -> ! {
    eprintln!("Configuracion invalida: {}", message);
    std::process::exit(1);
}

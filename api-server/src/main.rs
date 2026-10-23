use api_server::config::{optional_env, parsed_env, required_env, ApiConfig};
use api_server::state::AppState;
use api_server::telemetry::init_telemetry;
use application::inbound::InboundService;
use application::issue::IssueService;
use application::radian::RadianService;
use crypto_signer::signer::{SignerConfig, TenantSigner};
use dian_soap_client::client::DianSoapClientImpl;
use infra_postgres::PgStore;
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    init_telemetry();

    // Configuración obligatoria: sin valores por defecto para conexiones ni secretos.
    let db_url = required_env("DATABASE_URL").unwrap_or_else(|e| exit_with(&e));
    let config = ApiConfig::from_env().unwrap_or_else(|e| exit_with(&e));
    let signer_config = SignerConfig::from_env().unwrap_or_else(|e| exit_with(&e.to_string()));
    let max_connections: u32 = parsed_env("DATABASE_MAX_CONNECTIONS", 10).unwrap_or_else(|e| exit_with(&e));
    if config.inbound_allow_unverified_signatures {
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
    let soap = Arc::new(DianSoapClientImpl::new(
        config.dian_binary_security_token.clone(),
        config.dian_hab_url.clone(),
        config.dian_prod_url.clone(),
    ));

    let state = Arc::new(AppState {
        tenants: store.clone(),
        documents: store.clone(),
        health: store.clone(),
        issue: IssueService::new(store.clone(), config.transmit_max_attempts),
        inbound: InboundService::new(store.clone()),
        radian: RadianService::new(store.clone(), signer, soap),
        config,
    });

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

fn exit_with(message: &str) -> ! {
    eprintln!("Configuracion invalida: {}", message);
    std::process::exit(1);
}

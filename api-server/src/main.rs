use api_server::handlers::{create_invoice, get_document, create_payroll, create_support_document, health_check};
use api_server::events::{receive_inbound_invoice, register_document_event};
use api_server::config::{required_env, ApiConfig};
use api_server::middleware::auth_middleware;
use api_server::telemetry::init_telemetry;
use axum::{
    middleware::from_fn_with_state,
    routing::{get, post},
    Extension, Router,
};
use crypto_signer::signer::SignerConfig;
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() {
    // 1. Inicializar telemetría (Subscriber de tracing)
    init_telemetry();

    // 2. Configuración obligatoria: sin valores por defecto para conexiones ni secretos
    let db_url = required_env("DATABASE_URL").unwrap_or_else(|e| exit_with(&e));
    let redis_url = required_env("REDIS_URL").unwrap_or_else(|e| exit_with(&e));
    let config = Arc::new(ApiConfig::from_env().unwrap_or_else(|e| exit_with(&e)));
    let signer = Arc::new(SignerConfig::from_env().unwrap_or_else(|e| exit_with(&e.to_string())));
    tracing::info!(signing_mode = ?signer.mode(), "Firma configurada");
    if config.inbound_allow_unverified_signatures {
        tracing::warn!("INBOUND_ALLOW_UNVERIFIED_SIGNATURES=true: facturas recibidas sin verificacion criptografica");
    }

    // Conectar a PostgreSQL
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Failed to connect to PostgreSQL");

    // 3. Conectar a Redis
    let redis_client = redis::Client::open(redis_url)
        .expect("Failed to open Redis client connection");

    // Compartir conexiones como estado del Servidor REST
    let shared_state = (pool.clone(), redis_client);

    // 4. Configurar Enrutamiento y Middleware
    let health_route = Router::new()
        .route("/health", get(health_check))
        .with_state(shared_state.clone());

    let api_routes = Router::new()
        .route("/invoices", post(create_invoice))
        .route("/documents/:id", get(get_document))
        .route("/inbound/invoices", post(receive_inbound_invoice))
        .route("/documents/:id/events", post(register_document_event))
        .route("/payroll", post(create_payroll))
        .route("/support-documents", post(create_support_document))
        .layer(from_fn_with_state(pool, auth_middleware))
        .layer(Extension(config))
        .layer(Extension(signer))
        .with_state(shared_state)
        .layer(TraceLayer::new_for_http());

    let app = Router::new().nest("/api/v1", api_routes.merge(health_route));

    // 5. Iniciar Servidor
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr: SocketAddr = format!("0.0.0.0:{}", port)
        .parse()
        .expect("Invalid address formatting");

    println!("Servidor REST corriendo en http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind port listener");

    axum::serve(listener, app)
        .await
        .expect("Axum server run failed");
}

fn exit_with(message: &str) -> ! {
    eprintln!("Configuracion invalida: {}", message);
    std::process::exit(1);
}

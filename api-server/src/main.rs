use api_server::handlers::{create_invoice, get_document};
use api_server::events::{receive_inbound_invoice, register_document_event};
use api_server::middleware::auth_middleware;
use api_server::telemetry::init_telemetry;
use axum::{
    middleware::from_fn_with_state,
    routing::{get, post},
    Router,
};
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() {
    // 1. Inicializar telemetría (Subscriber de tracing)
    init_telemetry();

    // 2. Conectar a PostgreSQL
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/dian".to_string());
    
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Failed to connect to PostgreSQL");

    // 3. Conectar a Redis
    let redis_url = std::env::var("REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    
    let redis_client = redis::Client::open(redis_url)
        .expect("Failed to open Redis client connection");

    // Compartir conexiones como estado del Servidor REST
    let shared_state = (pool.clone(), redis_client);

    // 4. Configurar Enrutamiento y Middleware
    let api_routes = Router::new()
        .route("/invoices", post(create_invoice))
        .route("/documents/:id", get(get_document))
        .route("/inbound/invoices", post(receive_inbound_invoice))
        .route("/documents/:id/events", post(register_document_event))
        .layer(from_fn_with_state(pool, auth_middleware))
        .with_state(shared_state)
        .layer(TraceLayer::new_for_http());

    let app = Router::new().nest("/api/v1", api_routes);

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

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Registry};

/// Inicializa el sistema de tracing y observabilidad para el servidor.
pub fn init_telemetry() {
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(true);

    let filter_layer = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,api_server=debug,sqlx=warn"));

    Registry::default()
        .with(filter_layer)
        .with(fmt_layer)
        .init();
}

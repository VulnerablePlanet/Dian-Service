/// Configuración del servidor REST leída del entorno al arrancar.
pub struct ApiConfig {
    pub dian_binary_security_token: String,
    /// Endpoints SOAP opcionales; si faltan se usan los oficiales de la DIAN.
    pub dian_hab_url: Option<String>,
    pub dian_prod_url: Option<String>,
    /// Permite registrar facturas recibidas sin verificar criptográficamente su firma.
    /// Solo para ambientes de prueba: los documentos quedan marcados como no verificados
    /// y no admiten eventos RADIAN.
    pub inbound_allow_unverified_signatures: bool,
}

impl ApiConfig {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            dian_binary_security_token: required_env("DIAN_BINARY_SECURITY_TOKEN")?,
            dian_hab_url: optional_env("DIAN_HAB_URL"),
            dian_prod_url: optional_env("DIAN_PROD_URL"),
            inbound_allow_unverified_signatures: std::env::var("INBOUND_ALLOW_UNVERIFIED_SIGNATURES")
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        })
    }
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

pub fn required_env(name: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(format!("Missing required environment variable {}", name)),
    }
}

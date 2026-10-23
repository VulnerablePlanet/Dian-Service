/// Configuración del servidor REST leída del entorno al arrancar.
pub struct ApiConfig {
    /// Endpoints SOAP opcionales; si faltan se usan los oficiales de la DIAN.
    pub dian_hab_url: Option<String>,
    pub dian_prod_url: Option<String>,
    /// Permite registrar facturas recibidas sin verificar criptográficamente su firma.
    /// Solo para ambientes de prueba: los documentos quedan marcados como no verificados
    /// y no admiten eventos RADIAN.
    pub inbound_allow_unverified_signatures: bool,
    /// Intentos de transmisión de cada documento antes de pasar a contingencia.
    pub transmit_max_attempts: i32,
    /// Bundle PEM de CAs de confianza para verificar facturas recibidas (CAs acreditadas
    /// ante la ONAC y la CA de la DIAN). Sin él no se verifican firmas de documentos recibidos.
    pub inbound_trusted_ca_pem: Option<String>,
    /// Certificados intermedios (PEM) que las firmas recibidas pueden no incluir.
    pub inbound_intermediates_pem: Option<String>,
    /// Texto que debe contener el sujeto del certificado que firma el ApplicationResponse.
    pub inbound_dian_signer_subject: String,
}

impl ApiConfig {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            dian_hab_url: optional_env("DIAN_HAB_URL"),
            dian_prod_url: optional_env("DIAN_PROD_URL"),
            inbound_allow_unverified_signatures: std::env::var("INBOUND_ALLOW_UNVERIFIED_SIGNATURES")
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            transmit_max_attempts: parsed_env("TRANSMIT_MAX_ATTEMPTS", 5)?,
            inbound_trusted_ca_pem: optional_env("INBOUND_TRUSTED_CA_PEM"),
            inbound_intermediates_pem: optional_env("INBOUND_INTERMEDIATES_PEM"),
            // NIT de la DIAN; confirmar contra el certificado de un ApplicationResponse real.
            inbound_dian_signer_subject: optional_env("INBOUND_DIAN_SIGNER_SUBJECT")
                .unwrap_or_else(|| "800197268".to_string()),
        })
    }
}

pub fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

pub fn required_env(name: &str) -> Result<String, String> {
    optional_env(name).ok_or_else(|| format!("Missing required environment variable {}", name))
}

pub fn parsed_env<T: std::str::FromStr>(name: &str, default: T) -> Result<T, String> {
    match optional_env(name) {
        None => Ok(default),
        Some(value) => value
            .parse()
            .map_err(|_| format!("Invalid value for {}: '{}'", name, value)),
    }
}

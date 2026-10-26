//! Cliente mínimo de HashiCorp Vault: KV v2 (lectura) y Transit (firma).

use crate::error::CryptoError;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::json;

pub struct VaultClient {
    client: reqwest::Client,
    address: String,
    kv_mount: String,
    transit_mount: String,
}

#[derive(Debug, Deserialize)]
struct KvResponse {
    data: KvData,
}

#[derive(Debug, Deserialize)]
struct KvData {
    data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct TransitResponse {
    data: TransitData,
}

#[derive(Debug, Deserialize)]
struct TransitData {
    signature: String,
}

/// Segmentos de ruta admitidos en URLs de Vault: evita que una referencia guardada en la
/// base de datos salga de su mount (`..`) o altere la URL.
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

impl VaultClient {
    pub fn new(address: String, token: String) -> Result<Self, CryptoError> {
        let mut headers = HeaderMap::new();
        let mut token_val = HeaderValue::from_str(&token)
            .map_err(|_| CryptoError::Vault("Invalid Vault Token characters".to_string()))?;
        token_val.set_sensitive(true);
        headers.insert("X-Vault-Token", token_val);

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| CryptoError::Vault(format!("Failed to build HTTP client: {}", e)))?;

        Ok(Self {
            client,
            address: address.trim_end_matches('/').to_string(),
            kv_mount: "secret".to_string(),
            transit_mount: "transit".to_string(),
        })
    }

    /// Mounts de los motores KV v2 y Transit (por defecto `secret` y `transit`).
    pub fn with_mounts(mut self, kv_mount: impl Into<String>, transit_mount: impl Into<String>) -> Result<Self, CryptoError> {
        let (kv_mount, transit_mount) = (kv_mount.into(), transit_mount.into());
        if !valid_path(&kv_mount) || !valid_path(&transit_mount) {
            return Err(CryptoError::Vault("Invalid Vault mount path".to_string()));
        }
        self.kv_mount = kv_mount;
        self.transit_mount = transit_mount;
        Ok(self)
    }

    async fn checked(response: reqwest::Response) -> Result<reqwest::Response, CryptoError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        // El cuerpo de error de Vault no contiene secretos, pero se limita su tamaño.
        let body: String = response.text().await.unwrap_or_default().chars().take(300).collect();
        Err(CryptoError::Vault(format!("Vault returned status {}: {}", status, body)))
    }

    /// Lee un secreto de KV v2. `path` no incluye `data/`: el cliente lo añade.
    pub async fn read_secret(&self, path: &str) -> Result<serde_json::Value, CryptoError> {
        if !valid_path(path) {
            return Err(CryptoError::Vault(format!("Invalid Vault secret path '{}'", path)));
        }
        let url = format!("{}/v1/{}/data/{}", self.address, self.kv_mount, path);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| CryptoError::Vault(format!("Network error: {}", e)))?;
        let body: KvResponse = Self::checked(response)
            .await?
            .json()
            .await
            .map_err(|e| CryptoError::Vault(format!("Failed to parse JSON response: {}", e)))?;
        Ok(body.data.data)
    }

    /// Firma `data` con la llave RSA `key_name` de Transit (PKCS#1 v1.5, SHA-256 calculado
    /// por Vault). Devuelve la firma en bruto.
    pub async fn transit_sign_pkcs1_sha256(&self, key_name: &str, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if !valid_path(key_name) || key_name.contains('/') {
            return Err(CryptoError::Vault(format!("Invalid Transit key name '{}'", key_name)));
        }
        let url = format!("{}/v1/{}/sign/{}", self.address, self.transit_mount, key_name);
        let response = self
            .client
            .post(&url)
            .json(&json!({
                "input": B64.encode(data),
                "hash_algorithm": "sha2-256",
                "signature_algorithm": "pkcs1v15",
                "prehashed": false
            }))
            .send()
            .await
            .map_err(|e| CryptoError::Vault(format!("Network error: {}", e)))?;
        let body: TransitResponse = Self::checked(response)
            .await?
            .json()
            .await
            .map_err(|e| CryptoError::Vault(format!("Failed to parse JSON response: {}", e)))?;
        // Formato: "vault:v<versión de la llave>:<firma en Base64>".
        let encoded = body
            .data
            .signature
            .strip_prefix("vault:v")
            .and_then(|rest| rest.split_once(':'))
            .filter(|(version, _)| !version.is_empty() && version.chars().all(|c| c.is_ascii_digit()))
            .map(|(_, signature)| signature)
            .ok_or_else(|| CryptoError::Vault("Unexpected Transit signature format".to_string()))?;
        B64.decode(encoded)
            .map_err(|e| CryptoError::Vault(format!("Transit signature is not valid Base64: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_that_escape_their_mount() {
        assert!(valid_path("tenants/acme/cert"));
        assert!(valid_path("acme-signing_v2"));
        for bad in ["", "../sys/policy", "tenants//cert", "a/./b", "a?b", "a b", "a/../../b"] {
            assert!(!valid_path(bad), "{}", bad);
        }
    }
}

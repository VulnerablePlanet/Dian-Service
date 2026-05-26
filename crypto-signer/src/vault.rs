use crate::error::CryptoError;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;

pub struct VaultClient {
    client: reqwest::Client,
    address: String,
}

#[derive(Debug, Deserialize)]
struct VaultResponse {
    data: VaultData,
}

#[derive(Debug, Deserialize)]
struct VaultData {
    data: serde_json::Value,
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
            .build()
            .map_err(|e| CryptoError::Vault(format!("Failed to build HTTP client: {}", e)))?;

        Ok(Self { client, address })
    }

    /// Lee un secreto usando la API de Vault KV Engine v2
    /// El path esperado no incluye '/data/', el cliente lo inyecta automáticamente.
    pub async fn read_secret(&self, path: &str) -> Result<serde_json::Value, CryptoError> {
        let url = format!("{}/v1/secret/data/{}", self.address, path);
        let resp = self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| CryptoError::Vault(format!("Network error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CryptoError::Vault(format!(
                "Vault returned status {}: {}",
                status, body
            )));
        }

        let body: VaultResponse = resp
            .json()
            .await
            .map_err(|e| CryptoError::Vault(format!("Failed to parse JSON response: {}", e)))?;

        Ok(body.data.data)
    }
}

use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::Serialize;
use sha2::Sha256;
use std::time::Duration;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Serialize)]
pub struct WebhookPayload<'a, T: Serialize> {
    pub event_type: &'a str,
    pub timestamp: String,
    pub data: &'a T,
}

/// Envía una notificación HTTP POST con firma HMAC-SHA256 en las cabeceras.
pub async fn dispatch_webhook<T: Serialize>(
    target_url: &str,
    webhook_secret: &str,
    event_type: &str,
    data: &T,
) -> Result<(), reqwest::Error> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let payload = WebhookPayload {
        event_type,
        timestamp: chrono::Utc::now().to_rfc3339(),
        data,
    };

    let serialized = serde_json::to_string(&payload)
        .expect("Failed to serialize webhook payload");

    // Calcular firma HMAC-SHA256
    let mut mac = HmacSha256::new_from_slice(webhook_secret.as_bytes())
        .expect("HMAC can take any key size");
    mac.update(serialized.as_bytes());
    let result_bytes = mac.finalize().into_bytes();
    
    // Formatear firma a hexadecimal
    let signature_hex = result_bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();

    // Disparar POST HTTP
    let _response = client
        .post(target_url)
        .header("Content-Type", "application/json")
        .header("X-Hub-Signature-256", format!("sha256={}", signature_hex))
        .body(serialized)
        .send()
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_webhook_signature_calculation() {
        let secret = "my_secret_key";
        let data = json!({ "id": "doc_123", "status": "accepted" });
        
        // Simplemente validamos que no paniquee la firma
        let payload = WebhookPayload {
            event_type: "dian_accepted",
            timestamp: "2026-05-23T10:42:00Z".to_string(),
            data: &data,
        };
        let serialized = serde_json::to_string(&payload).unwrap();

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(serialized.as_bytes());
        let result = mac.finalize().into_bytes();
        let sig_hex = result.iter().map(|b| format!("{:02x}", b)).collect::<String>();

        assert_eq!(sig_hex.len(), 64); // SHA-256 hex length is 64 chars
    }
}

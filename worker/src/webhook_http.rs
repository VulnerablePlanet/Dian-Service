//! Envío HTTP de webhooks con protección SSRF en el momento del envío:
//! - el host se resuelve aquí y se rechaza si alguna de sus direcciones no es pública;
//! - la conexión se fija a la dirección verificada (sin segunda resolución DNS que permita
//!   "DNS rebinding") y no pasa por proxies;
//! - no se siguen redirecciones (un 3xx es una entrega fallida).

use async_trait::async_trait;
use core_domain::ports::{DomainError, WebhookSender};
use core_domain::webhooks::is_public_ip;
use std::net::SocketAddr;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Default)]
pub struct HttpWebhookSender {
    /// Permite `http://` (solo desarrollo).
    pub allow_http: bool,
    /// Permite destinos en redes privadas o loopback (solo desarrollo y pruebas).
    pub allow_private_networks: bool,
}

fn rejected(message: impl Into<String>) -> DomainError {
    DomainError::Validation(format!("Webhook destination rejected: {}", message.into()))
}

impl HttpWebhookSender {
    /// Dirección verificada a la que se conectará.
    async fn vetted_address(&self, url: &url::Url) -> Result<(Option<String>, SocketAddr), DomainError> {
        match url.scheme() {
            "https" => {}
            "http" if self.allow_http => {}
            other => return Err(rejected(format!("scheme '{}' is not allowed", other))),
        }
        let port = url.port_or_known_default().ok_or_else(|| rejected("no port"))?;
        let (domain, addresses): (Option<String>, Vec<SocketAddr>) = match url.host() {
            Some(url::Host::Domain(domain)) => {
                let resolved = tokio::net::lookup_host((domain, port))
                    .await
                    .map_err(|e| DomainError::Internal(format!("DNS resolution of {} failed: {}", domain, e)))?
                    .collect();
                (Some(domain.to_string()), resolved)
            }
            Some(url::Host::Ipv4(ip)) => (None, vec![SocketAddr::new(ip.into(), port)]),
            Some(url::Host::Ipv6(ip)) => (None, vec![SocketAddr::new(ip.into(), port)]),
            None => return Err(rejected("no host")),
        };
        if addresses.is_empty() {
            return Err(DomainError::Internal(format!("{} did not resolve", url)));
        }
        // Basta una dirección interna para rechazar: el cliente podría elegir cualquiera.
        if !self.allow_private_networks {
            if let Some(internal) = addresses.iter().find(|a| !is_public_ip(a.ip())) {
                return Err(rejected(format!("{} resolves to a non-public address ({})", url, internal.ip())));
            }
        }
        Ok((domain, addresses[0]))
    }
}

#[async_trait]
impl WebhookSender for HttpWebhookSender {
    async fn post(&self, target_url: &str, headers: &[(&str, String)], body: &str) -> Result<u16, DomainError> {
        let url = url::Url::parse(target_url).map_err(|e| rejected(e.to_string()))?;
        let (domain, address) = self.vetted_address(&url).await?;

        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .user_agent("Dian-Service-Webhooks/1");
        if let Some(domain) = &domain {
            builder = builder.resolve(domain, address);
        }
        let client = builder
            .build()
            .map_err(|e| DomainError::Internal(format!("HTTP client: {}", e)))?;

        let mut request = client.post(url).header("Content-Type", "application/json").body(body.to_string());
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|e| DomainError::Internal(format!("webhook request failed: {}", e)))?;
        // El cuerpo de la respuesta no se lee: solo importa el código.
        Ok(response.status().as_u16())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Servidor de un solo uso que responde `status` y guarda la petición.
    async fn server(status: &'static str) -> (String, Arc<Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(String::new()));
        let shared = captured.clone();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 16384];
            let n = socket.read(&mut buffer).await.unwrap();
            *shared.lock().unwrap() = String::from_utf8_lossy(&buffer[..n]).into_owned();
            let response = format!(
                "HTTP/1.1 {}\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                status
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });
        (format!("http://localhost:{}/hooks", address.port()), captured)
    }

    const DEV: HttpWebhookSender = HttpWebhookSender { allow_http: true, allow_private_networks: true };

    #[tokio::test]
    async fn posts_with_headers_and_returns_the_status() {
        let (url, request) = server("204 No Content").await;
        let status = DEV.post(&url, &[("X-Dian-Webhook-Id", "abc".into())], r#"{"a":1}"#).await.unwrap();
        assert_eq!(status, 204);
        let request = request.lock().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("post /hooks"));
        assert!(request.contains("x-dian-webhook-id: abc"));
        assert!(request.contains("content-type: application/json"));
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let (url, _) = server("302 Found").await;
        assert_eq!(DEV.post(&url, &[], "{}").await.unwrap(), 302);
    }

    #[tokio::test]
    async fn internal_destinations_and_plain_http_are_rejected_by_default() {
        let strict = HttpWebhookSender::default();
        for url in [
            "https://localhost/h",
            "https://127.0.0.1/h",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/h",
            "https://10.0.0.1/h",
            "http://8.8.8.8/h",
        ] {
            let error = strict.post(url, &[], "{}").await.unwrap_err();
            assert!(matches!(error, DomainError::Validation(_)), "{} -> {:?}", url, error);
        }
    }
}

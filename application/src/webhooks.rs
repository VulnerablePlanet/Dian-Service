//! Casos de uso: suscripciones de webhooks y entrega de notificaciones desde la cola.
//!
//! Formato de cada entrega (POST, `application/json`):
//! - Cuerpo: `{"id", "event", "created_at", "data": {...}}`; `id` identifica la entrega y no
//!   cambia entre reintentos (el receptor puede deduplicar con él).
//! - `X-Dian-Webhook-Id`, `X-Dian-Webhook-Timestamp` (segundos Unix del envío) y
//!   `X-Dian-Webhook-Signature: v1=<hex HMAC-SHA256(secreto, "{timestamp}.{cuerpo}")>`.
//!   El receptor debe recalcular la firma y rechazar timestamps con más de 5 minutos de
//!   diferencia (anti-replay).

use crate::resilience::backoff_with_jitter;
use chrono::{DateTime, Utc};
use core_domain::models::{Tenant, Webhook, WebhookSubscriptionStatus};
use core_domain::ports::{DomainError, WebhookDelivery, WebhookRepository, WebhookSender};
use core_domain::webhooks::{is_public_ip, is_valid_subscription, EVENT_ALL, EVENT_TYPES};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::Serialize;
use serde_json::json;
use sha2::Sha256;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

const MAX_WEBHOOKS_PER_TENANT: usize = 10;

/// `v1=<hex>` de HMAC-SHA256 sobre `"{timestamp}.{body}"`.
pub fn signature(secret: &str, timestamp: i64, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    let hex: String = mac.finalize().into_bytes().iter().map(|b| format!("{:02x}", b)).collect();
    format!("v1={}", hex)
}

/// Política de destinos al registrar una suscripción. La resolución DNS se vuelve a
/// comprobar en cada envío (ver el adaptador HTTP).
#[derive(Debug, Clone, Copy, Default)]
pub struct DestinationPolicy {
    /// Permite `http://` (solo desarrollo).
    pub allow_http: bool,
    /// Permite destinos en redes privadas o loopback (solo desarrollo y pruebas).
    pub allow_private_networks: bool,
}

impl DestinationPolicy {
    pub fn check(&self, target_url: &str) -> Result<(), DomainError> {
        let invalid = |m: &str| DomainError::Validation(format!("target_url invalida: {}", m));
        if target_url.len() > 1024 {
            return Err(invalid("demasiado larga"));
        }
        let url = url::Url::parse(target_url).map_err(|e| invalid(&e.to_string()))?;
        match url.scheme() {
            "https" => {}
            "http" if self.allow_http => {}
            _ => return Err(invalid("debe usar https")),
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(invalid("no puede incluir credenciales"));
        }
        if self.allow_private_networks {
            return Ok(());
        }
        match url.host() {
            None => Err(invalid("sin host")),
            Some(url::Host::Domain(domain)) => {
                let domain = domain.trim_end_matches('.').to_ascii_lowercase();
                if domain == "localhost" || domain.ends_with(".localhost") || !domain.contains('.') {
                    Err(invalid("el host debe ser un dominio publico"))
                } else {
                    Ok(())
                }
            }
            Some(url::Host::Ipv4(ip)) if is_public_ip(IpAddr::V4(ip)) => Ok(()),
            Some(url::Host::Ipv6(ip)) if is_public_ip(IpAddr::V6(ip)) => Ok(()),
            Some(_) => Err(invalid("la direccion IP no es publica")),
        }
    }
}

/// Suscripción recién creada: el secreto solo se muestra aquí.
#[derive(Debug, Clone, Serialize)]
pub struct RegisteredWebhook {
    pub id: Uuid,
    pub target_url: String,
    pub event_type: String,
    pub secret: String,
}

/// Suscripción sin su secreto.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookView {
    pub id: Uuid,
    pub target_url: String,
    pub event_type: String,
    pub status: String,
}

pub struct WebhookService {
    repository: Arc<dyn WebhookRepository>,
    policy: DestinationPolicy,
}

impl WebhookService {
    pub fn new(repository: Arc<dyn WebhookRepository>, policy: DestinationPolicy) -> Self {
        Self { repository, policy }
    }

    pub async fn register(&self, tenant: &Tenant, target_url: &str, event_type: &str) -> Result<RegisteredWebhook, DomainError> {
        if !is_valid_subscription(event_type) {
            return Err(DomainError::Validation(format!(
                "event_type debe ser '{}' o uno de: {}",
                EVENT_ALL,
                EVENT_TYPES.join(", ")
            )));
        }
        self.policy.check(target_url)?;
        if self.repository.list_webhooks(tenant.id).await?.len() >= MAX_WEBHOOKS_PER_TENANT {
            return Err(DomainError::Validation(format!(
                "Se admiten como maximo {} webhooks por tenant",
                MAX_WEBHOOKS_PER_TENANT
            )));
        }
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let secret = format!("whsec_{}", bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>());
        let webhook = Webhook {
            id: Uuid::new_v4(),
            tenant_id: tenant.id,
            target_url: target_url.to_string(),
            webhook_secret: secret.clone(),
            event_type: event_type.to_string(),
            subscription_status: WebhookSubscriptionStatus::Active,
        };
        self.repository.create_webhook(webhook.clone()).await?;
        Ok(RegisteredWebhook {
            id: webhook.id,
            target_url: webhook.target_url,
            event_type: webhook.event_type,
            secret,
        })
    }

    pub async fn list(&self, tenant: &Tenant) -> Result<Vec<WebhookView>, DomainError> {
        Ok(self
            .repository
            .list_webhooks(tenant.id)
            .await?
            .into_iter()
            .map(|w| WebhookView {
                id: w.id,
                target_url: w.target_url,
                event_type: w.event_type,
                status: w.subscription_status.to_string(),
            })
            .collect())
    }

    pub async fn delete(&self, tenant: &Tenant, webhook_id: Uuid) -> Result<(), DomainError> {
        if self.repository.delete_webhook(tenant.id, webhook_id).await? {
            Ok(())
        } else {
            Err(DomainError::NotFound(format!("Webhook {} no encontrado", webhook_id)))
        }
    }
}

#[derive(Debug, Clone)]
pub struct DispatchConfig {
    pub lease: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(60),
            backoff_base: Duration::from_secs(30),
            backoff_max: Duration::from_secs(3600),
        }
    }
}

/// Resultado de una pasada del despachador.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchStats {
    pub claimed: usize,
    pub delivered: usize,
    pub retried: usize,
    pub dead: usize,
}

enum Outcome {
    Delivered,
    Retried,
    Dead,
}

pub struct WebhookDispatcher {
    repository: Arc<dyn WebhookRepository>,
    sender: Arc<dyn WebhookSender>,
    config: DispatchConfig,
}

/// Cuerpo de una entrega.
pub fn delivery_body(delivery: &WebhookDelivery) -> String {
    json!({
        "id": delivery.id,
        "event": delivery.event_type,
        "created_at": delivery.created_at,
        "data": delivery.data,
    })
    .to_string()
}

impl WebhookDispatcher {
    pub fn new(repository: Arc<dyn WebhookRepository>, sender: Arc<dyn WebhookSender>, config: DispatchConfig) -> Self {
        Self { repository, sender, config }
    }

    /// Reclama hasta `limit` entregas y las envía en paralelo.
    pub async fn run_once(self: &Arc<Self>, limit: i64) -> Result<DispatchStats, DomainError> {
        let deliveries = self.repository.claim_deliveries(limit, self.config.lease).await?;
        let mut stats = DispatchStats {
            claimed: deliveries.len(),
            ..Default::default()
        };
        let mut tasks = tokio::task::JoinSet::new();
        for delivery in deliveries {
            let dispatcher = self.clone();
            tasks.spawn(async move { dispatcher.deliver(delivery, Utc::now()).await });
        }
        while let Some(result) = tasks.join_next().await {
            match result.map_err(|e| DomainError::Internal(e.to_string()))? {
                Ok(Outcome::Delivered) => stats.delivered += 1,
                Ok(Outcome::Retried) => stats.retried += 1,
                Ok(Outcome::Dead) => stats.dead += 1,
                Err(e) => tracing::error!(error = %e, "could not record webhook delivery outcome"),
            }
        }
        Ok(stats)
    }

    async fn deliver(&self, delivery: WebhookDelivery, now: DateTime<Utc>) -> Result<Outcome, DomainError> {
        if !delivery.active {
            self.repository.bury_delivery(delivery.id, "subscription inactive", None).await?;
            return Ok(Outcome::Dead);
        }
        let body = delivery_body(&delivery);
        let timestamp = now.timestamp();
        let headers = [
            ("X-Dian-Webhook-Id", delivery.id.to_string()),
            ("X-Dian-Webhook-Event", delivery.event_type.clone()),
            ("X-Dian-Webhook-Timestamp", timestamp.to_string()),
            ("X-Dian-Webhook-Signature", signature(&delivery.secret, timestamp, &body)),
        ];
        let (error, status_code) = match self.sender.post(&delivery.target_url, &headers, &body).await {
            Ok(code) if (200..300).contains(&code) => {
                self.repository.complete_delivery(delivery.id, code).await?;
                return Ok(Outcome::Delivered);
            }
            Ok(code) => (format!("HTTP {}", code), Some(code)),
            Err(e) => (e.to_string(), None),
        };
        tracing::warn!(
            delivery_id = %delivery.id,
            tenant_id = %delivery.tenant_id,
            attempt = delivery.attempts,
            %error,
            "webhook delivery failed"
        );
        if delivery.attempts >= delivery.max_attempts {
            self.repository.bury_delivery(delivery.id, &error, status_code).await?;
            return Ok(Outcome::Dead);
        }
        let delay = backoff_with_jitter(delivery.attempts, self.config.backoff_base, self.config.backoff_max);
        let run_at = now + chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::hours(1));
        self.repository.retry_delivery(delivery.id, run_at, &error, status_code).await?;
        Ok(Outcome::Retried)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use async_trait::async_trait;
    use core_domain::models::{DocumentStatus, DocumentType};
    use core_domain::ports::{DocumentRepository, Transition};
    use core_domain::testing::InMemoryStore;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// `(url, cabeceras, cuerpo)` de cada envío.
    type SentRequest = (String, Vec<(String, String)>, String);

    #[derive(Default)]
    struct RecordingSender {
        responses: Mutex<VecDeque<Result<u16, DomainError>>>,
        requests: Mutex<Vec<SentRequest>>,
    }

    #[async_trait]
    impl WebhookSender for RecordingSender {
        async fn post(&self, url: &str, headers: &[(&str, String)], body: &str) -> Result<u16, DomainError> {
            self.requests.lock().unwrap().push((
                url.to_string(),
                headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
                body.to_string(),
            ));
            self.responses.lock().unwrap().pop_front().unwrap_or(Ok(200))
        }
    }

    const STRICT: DestinationPolicy = DestinationPolicy { allow_http: false, allow_private_networks: false };

    /// Tenant con un documento enviado a la DIAN y una suscripción a `event_type`.
    async fn setup(event_type: &str) -> (Arc<InMemoryStore>, uuid::Uuid, RegisteredWebhook) {
        let (store, tenant) = store_with_tenant();
        store.add_range(range(&tenant, DocumentType::Invoice, "FE", 1));
        let issue = crate::issue::IssueService::new(store.clone(), 3);
        let core_domain::ports::IssueOutcome::Created(doc) =
            issue.issue_invoice(&tenant, invoice_payload(1), None).await.unwrap()
        else {
            panic!()
        };
        let service = WebhookService::new(store.clone(), STRICT);
        let webhook = service.register(&tenant, "https://erp.example.com/hooks/dian", event_type).await.unwrap();
        store.apply_transition(Transition::new(doc.document_id, DocumentStatus::SentDian)).await.unwrap();
        (store, doc.document_id, webhook)
    }

    fn dispatcher(store: Arc<InMemoryStore>, sender: Arc<RecordingSender>) -> Arc<WebhookDispatcher> {
        Arc::new(WebhookDispatcher::new(
            store,
            sender,
            DispatchConfig { lease: Duration::from_secs(60), backoff_base: Duration::from_millis(1), backoff_max: Duration::from_millis(1) },
        ))
    }

    #[test]
    fn signature_covers_timestamp_and_body() {
        let a = signature("whsec_x", 1_700_000_000, r#"{"a":1}"#);
        assert!(a.starts_with("v1=") && a.len() == 3 + 64);
        assert_ne!(a, signature("whsec_x", 1_700_000_001, r#"{"a":1}"#));
        assert_ne!(a, signature("whsec_x", 1_700_000_000, r#"{"a":2}"#));
        assert_ne!(a, signature("whsec_y", 1_700_000_000, r#"{"a":1}"#));
        // Vector calculado con: printf '1700000000.{"a":1}' | openssl dgst -sha256 -hmac whsec_x
        assert_eq!(a, "v1=b3438763ab46d9634b9844c8722818eaf523f803db1327484c6067462a5fd6d1");
    }

    #[test]
    fn destination_policy_blocks_internal_targets() {
        for url in [
            "http://erp.example.com/h",
            "https://localhost/h",
            "https://127.0.0.1/h",
            "https://10.0.0.5/h",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/h",
            "https://intranet/h",
            "https://user:pass@erp.example.com/h",
            "ftp://erp.example.com/h",
            "not a url",
        ] {
            assert!(STRICT.check(url).is_err(), "{} must be rejected", url);
        }
        assert!(STRICT.check("https://erp.example.com/hooks?x=1").is_ok());
        assert!(STRICT.check("https://8.8.8.8/h").is_ok());
        let dev = DestinationPolicy { allow_http: true, allow_private_networks: true };
        assert!(dev.check("http://127.0.0.1:8080/h").is_ok());
    }

    #[tokio::test]
    async fn final_states_enqueue_signed_deliveries_in_the_same_transition() {
        let (store, document_id, webhook) = setup("document.accepted").await;
        assert!(store.deliveries().is_empty(), "sent_dian is not notified");

        store.apply_transition(Transition::new(document_id, DocumentStatus::DianAccepted)).await.unwrap();
        let deliveries = store.deliveries();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].data["status"], "dian_accepted");

        let sender = Arc::new(RecordingSender::default());
        let stats = dispatcher(store.clone(), sender.clone()).run_once(10).await.unwrap();
        assert_eq!((stats.claimed, stats.delivered), (1, 1));
        assert_eq!(store.deliveries()[0].status, "delivered");

        let (url, headers, body) = sender.requests.lock().unwrap()[0].clone();
        assert_eq!(url, webhook.target_url);
        let header = |name: &str| headers.iter().find(|(k, _)| k == name).unwrap().1.clone();
        let timestamp: i64 = header("X-Dian-Webhook-Timestamp").parse().unwrap();
        assert_eq!(header("X-Dian-Webhook-Signature"), signature(&webhook.secret, timestamp, &body));
        assert_eq!(header("X-Dian-Webhook-Id"), deliveries[0].id.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["event"], "document.accepted");
        assert_eq!(parsed["data"]["document_id"], document_id.to_string());
        assert!(!body.contains("signed_xml") && !body.contains(&webhook.secret));
    }

    #[tokio::test]
    async fn failed_deliveries_are_retried_then_buried() {
        let (store, document_id, _) = setup("*").await;
        store.apply_transition(Transition::new(document_id, DocumentStatus::DianRejected)).await.unwrap();
        let sender = Arc::new(RecordingSender::default());
        sender
            .responses
            .lock()
            .unwrap()
            .extend((0..core_domain::webhooks::MAX_DELIVERY_ATTEMPTS).map(|i| {
                if i % 2 == 0 { Ok(500) } else { Err(DomainError::Internal("connection refused".into())) }
            }));
        let dispatcher = dispatcher(store.clone(), sender.clone());

        let first = dispatcher.run_once(10).await.unwrap();
        assert_eq!(first.retried, 1);
        assert_eq!(store.deliveries()[0].last_status_code, Some(500));
        for _ in 1..core_domain::webhooks::MAX_DELIVERY_ATTEMPTS {
            store.make_deliveries_due();
            dispatcher.run_once(10).await.unwrap();
        }
        let delivery = store.deliveries().remove(0);
        assert_eq!(delivery.status, "dead");
        assert_eq!(delivery.attempts, core_domain::webhooks::MAX_DELIVERY_ATTEMPTS);
        let ids: std::collections::HashSet<String> = sender
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, h, _)| h.iter().find(|(k, _)| k == "X-Dian-Webhook-Id").unwrap().1.clone())
            .collect();
        assert_eq!(ids.len(), 1, "retries keep the delivery id");
    }

    #[tokio::test]
    async fn subscriptions_are_validated_and_secrets_never_listed() {
        let (store, tenant) = store_with_tenant();
        let service = WebhookService::new(store.clone(), STRICT);
        assert!(matches!(
            service.register(&tenant, "https://erp.example.com/h", "document.created").await,
            Err(DomainError::Validation(_))
        ));
        let created = service.register(&tenant, "https://erp.example.com/h", "*").await.unwrap();
        assert!(created.secret.starts_with("whsec_") && created.secret.len() == 6 + 64);
        let listed = serde_json::to_string(&service.list(&tenant).await.unwrap()).unwrap();
        assert!(!listed.contains(&created.secret));

        let (_, other_tenant) = store_with_tenant();
        assert!(matches!(service.delete(&other_tenant, created.id).await, Err(DomainError::NotFound(_))));
        service.delete(&tenant, created.id).await.unwrap();
        assert!(service.list(&tenant).await.unwrap().is_empty());
    }
}

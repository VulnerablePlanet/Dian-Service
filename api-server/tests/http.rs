//! Pruebas HTTP del router completo sobre el repositorio en memoria.

use api_server::config::ApiConfig;
use api_server::metrics::HttpMetrics;
use api_server::state::AppState;
use application::admin::AdminService;
use application::inbound::InboundService;
use application::issue::IssueService;
use application::radian::RadianService;
use application::webhooks::{DestinationPolicy, WebhookService};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use core_domain::jobs::NewJob;
use core_domain::models::{DianEvent, Document, DocumentType, NumberingRange, Tenant};
use core_domain::ports::{
    CryptoSigner, DianResponse, DianSoapClient, DocumentRepository, DomainError, HealthProbe,
    IssueOutcome, NewInboundDocument, NewOutboundDocument, Transition,
};
use core_domain::testing::InMemoryStore;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

struct NoSigner;

#[async_trait]
impl CryptoSigner for NoSigner {
    async fn sign_for_tenant(&self, _tenant_id: Uuid, _xml: &str) -> Result<String, DomainError> {
        Err(DomainError::Crypto("not used in HTTP tests".into()))
    }
    async fn sign_soap_for_tenant(&self, _tenant_id: Uuid, _envelope: &str) -> Result<String, DomainError> {
        Err(DomainError::Crypto("not used in HTTP tests".into()))
    }
}

struct NoSoap;

#[async_trait]
impl DianSoapClient for NoSoap {
    async fn send_bill_sync(&self, _: Uuid, _: &[u8], _: bool) -> Result<DianResponse, DomainError> {
        unreachable!()
    }
    async fn send_bill_async(&self, _: Uuid, _: &[u8], _: bool) -> Result<String, DomainError> {
        unreachable!()
    }
    async fn send_test_set_async(&self, _: Uuid, _: &[u8], _: &str, _: bool) -> Result<String, DomainError> {
        unreachable!()
    }
    async fn get_status(&self, _: Uuid, _: &str, _: bool) -> Result<DianResponse, DomainError> {
        unreachable!()
    }
    async fn get_status_zip(&self, _: Uuid, _: &str, _: bool) -> Result<DianResponse, DomainError> {
        unreachable!()
    }
    async fn send_event_update_status(&self, _: Uuid, _: &[u8], _: bool) -> Result<DianResponse, DomainError> {
        unreachable!()
    }
}

/// Repositorio cuya base de datos "se cayó": toda operación falla con un detalle interno.
struct BrokenDatabase;

fn down<T>() -> Result<T, DomainError> {
    Err(DomainError::Database("connection to 10.0.0.5:5432 refused (password=hunter2)".into()))
}

#[async_trait]
impl DocumentRepository for BrokenDatabase {
    async fn issue_outbound(&self, _: NewOutboundDocument, _: NewJob) -> Result<IssueOutcome, DomainError> {
        down()
    }
    async fn insert_inbound(&self, _: NewInboundDocument) -> Result<bool, DomainError> {
        down()
    }
    async fn find_document(&self, _: Uuid) -> Result<Option<Document>, DomainError> {
        down()
    }
    async fn find_tenant_document(&self, _: Uuid, _: Uuid) -> Result<Option<Document>, DomainError> {
        down()
    }
    async fn find_numbering_range(&self, _: Uuid) -> Result<Option<NumberingRange>, DomainError> {
        down()
    }
    async fn store_artifacts(&self, _: Uuid, _: &str, _: &str, _: &str) -> Result<(), DomainError> {
        down()
    }
    async fn record_event(&self, _: &DianEvent) -> Result<(), DomainError> {
        down()
    }
    async fn find_events(&self, _: Uuid) -> Result<Vec<DianEvent>, DomainError> {
        down()
    }
    async fn apply_transition(&self, _: Transition) -> Result<bool, DomainError> {
        down()
    }
}

#[async_trait]
impl HealthProbe for BrokenDatabase {
    async fn ping(&self) -> Result<(), DomainError> {
        down()
    }
}

fn tenant() -> Tenant {
    Tenant {
        id: Uuid::new_v4(),
        tax_id: "900123456".into(),
        registration_name: "Tenant SAS".into(),
        tax_regime: "48".into(),
        address_info: json!({}),
        reception_email: "f@t.co".into(),
        api_keys: json!({ "secret": "good-key" }),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn range(tenant: &Tenant, document_type: DocumentType, prefix: &str) -> NumberingRange {
    NumberingRange {
        id: Uuid::new_v4(),
        tenant_id: tenant.id,
        document_type,
        prefix: prefix.into(),
        from_number: 1,
        to_number: 1000,
        technical_key: "tk".into(),
        valid_date_from: Utc::now() - Duration::days(1),
        valid_date_to: Utc::now() + Duration::days(1),
        current_counter: 0,
    }
}

const ADMIN_TOKEN: &str = "admin-token-for-tests";

fn config() -> ApiConfig {
    ApiConfig {
        dian_hab_url: None,
        dian_prod_url: None,
        inbound_allow_unverified_signatures: false,
        transmit_max_attempts: 5,
        inbound_trusted_ca_pem: None,
        inbound_intermediates_pem: None,
        inbound_dian_signer_subject: "800197268".into(),
        webhook_policy: DestinationPolicy::default(),
        admin_token_sha256: Some(core_domain::api_keys::hash(ADMIN_TOKEN)),
    }
}

fn app_with(documents: Arc<dyn DocumentRepository>, health: Arc<dyn HealthProbe>, store: Arc<InMemoryStore>) -> axum::Router {
    app_with_metrics(documents, health, store, Arc::new(HttpMetrics::default()))
}

fn app_with_metrics(
    documents: Arc<dyn DocumentRepository>,
    health: Arc<dyn HealthProbe>,
    store: Arc<InMemoryStore>,
    metrics: Arc<HttpMetrics>,
) -> axum::Router {
    api_server::router(Arc::new(AppState {
        tenants: store.clone(),
        documents: documents.clone(),
        health,
        issue: IssueService::new(documents.clone(), 5),
        inbound: InboundService::new(documents.clone(), None),
        radian: RadianService::new(documents, Arc::new(NoSigner), Arc::new(NoSoap)),
        webhooks: WebhookService::new(store.clone(), DestinationPolicy::default()),
        admin: AdminService::new(store.clone()),
        config: config(),
        metrics,
    }))
}

fn app() -> (axum::Router, Arc<InMemoryStore>) {
    let store = Arc::new(InMemoryStore::default());
    let t = tenant();
    store.add_range(range(&t, DocumentType::Invoice, "FE"));
    store.add_range(range(&t, DocumentType::Payroll, "NOM"));
    store.add_tenant(t);
    (app_with(store.clone(), store.clone(), store.clone()), store)
}

fn invoice(number: i32) -> Value {
    json!({
        "prefix": "FE", "number": number, "issue_date": "2026-10-23", "issue_time": "10:42:00-05:00",
        "company_nit": "900123456", "company_name": "Tenant SAS", "customer_id_type": "31",
        "customer_id": "900999888", "customer_name": "Cliente SAS", "customer_email": "c@x.co",
        "net_amount": 100000.0, "tax_amount": 19000.0, "total_amount": 119000.0,
        "environment": "2", "technical_key": "legacy", "document_type": "invoice", "test_set_id": null
    })
}

async fn send(app: &axum::Router, method: &str, uri: &str, body: Option<Value>, headers: &[(&str, &str)]) -> (StatusCode, Option<String>, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = match body {
        Some(body) => request
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let replayed = response
        .headers()
        .get("Idempotent-Replayed")
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, replayed, json)
}

const AUTH: (&str, &str) = ("Authorization", "Bearer good-key");

#[tokio::test]
async fn health_reports_up() {
    let (app, _) = app();
    let (status, _, body) = send(&app, "GET", "/api/v1/health", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "UP");
}

#[tokio::test]
async fn requests_without_valid_api_key_are_rejected() {
    let (app, _) = app();
    let (status, _, _) = send(&app, "POST", "/api/v1/invoices", Some(invoice(1)), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(&app, "POST", "/api/v1/invoices", Some(invoice(1)), &[("Authorization", "Bearer wrong")]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn invoice_is_accepted_and_idempotent() {
    let (app, store) = app();
    let key = ("Idempotency-Key", "order-77");

    let (status, replayed, first) = send(&app, "POST", "/api/v1/invoices", Some(invoice(1)), &[AUTH, key]).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(replayed, None);
    assert_eq!(first["document_number"], 1);

    let (status, replayed, again) = send(&app, "POST", "/api/v1/invoices", Some(invoice(1)), &[AUTH, key]).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(replayed.as_deref(), Some("true"));
    assert_eq!(again["document_id"], first["document_id"]);

    let mut changed = invoice(1);
    changed["customer_name"] = json!("Otro");
    let (status, _, _) = send(&app, "POST", "/api/v1/invoices", Some(changed), &[AUTH, key]).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let id: Uuid = first["document_id"].as_str().unwrap().parse().unwrap();
    assert!(store.document(id).unwrap().payload.get("technical_key").is_none());
}

#[tokio::test]
async fn business_errors_map_to_4xx() {
    let (app, _) = app();
    let mut foreign = invoice(1);
    foreign["company_nit"] = json!("811111111");
    assert_eq!(send(&app, "POST", "/api/v1/invoices", Some(foreign), &[AUTH]).await.0, StatusCode::FORBIDDEN);

    assert_eq!(
        send(&app, "POST", "/api/v1/invoices", Some(invoice(5)), &[AUTH]).await.0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "gap in numbering"
    );

    let mut note = invoice(1);
    note["document_type"] = json!("credit_note"); // sin billing_reference, discrepancy ni PIN
    assert_eq!(send(&app, "POST", "/api/v1/invoices", Some(note), &[AUTH]).await.0, StatusCode::UNPROCESSABLE_ENTITY);

    let missing = format!("/api/v1/documents/{}", Uuid::new_v4());
    assert_eq!(send(&app, "GET", &missing, None, &[AUTH]).await.0, StatusCode::NOT_FOUND);

    let (status, _, _) = send(&app, "POST", "/api/v1/inbound/invoices", Some(json!({ "zip_content": "" })), &[AUTH]).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn document_view_hides_tenant_secrets() {
    let (app, _) = app();
    let payroll = json!({
        "prefix": "NOM", "number": 1, "issue_date": "2026-10-23", "issue_time": "08:00:00",
        "employer_nit": "900123456", "employer_name": "Tenant SAS", "employee_id_type": "13",
        "employee_id": "10203040", "employee_name": "Trabajador", "devengado": 1500000.0,
        "deducido": 60000.0, "total": 1440000.0, "software_pin": "super-secret-pin", "environment": "2"
    });
    let (status, _, created) = send(&app, "POST", "/api/v1/payroll", Some(payroll), &[AUTH]).await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let uri = format!("/api/v1/documents/{}", created["document_id"].as_str().unwrap());
    let (status, _, document) = send(&app, "GET", &uri, None, &[AUTH]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(document["status"], "pending");
    assert!(document["payload"].get("software_pin").is_none());
    assert!(!document.to_string().contains("super-secret-pin"));
}

#[tokio::test]
async fn infrastructure_failures_return_5xx_without_internal_details() {
    let store = Arc::new(InMemoryStore::default());
    store.add_tenant(tenant());
    let app = app_with(Arc::new(BrokenDatabase), Arc::new(BrokenDatabase), store);

    let (status, _, body) = send(&app, "POST", "/api/v1/invoices", Some(invoice(1)), &[AUTH]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!body.to_string().contains("hunter2"));
    assert!(!body.to_string().contains("10.0.0.5"));

    let (status, _, body) = send(&app, "GET", "/api/v1/health", None, &[]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "DOWN");

    assert_eq!(send(&app, "GET", "/livez", None, &[]).await.0, StatusCode::OK, "liveness ignores dependencies");
    assert_eq!(send(&app, "GET", "/readyz", None, &[]).await.0, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn requests_are_measured_by_route_template_and_status() {
    let store = Arc::new(InMemoryStore::default());
    let t = tenant();
    store.add_range(range(&t, DocumentType::Invoice, "FE"));
    store.add_tenant(t);
    let metrics = Arc::new(HttpMetrics::default());
    let app = app_with_metrics(store.clone(), store.clone(), store, metrics.clone());

    send(&app, "POST", "/api/v1/invoices", Some(invoice(1)), &[AUTH]).await;
    let missing = format!("/api/v1/documents/{}", Uuid::new_v4());
    send(&app, "GET", &missing, None, &[AUTH]).await;
    send(&app, "GET", &missing, None, &[]).await;
    assert_eq!(send(&app, "GET", "/readyz", None, &[]).await.0, StatusCode::OK);

    let mut registry = prometheus_client::registry::Registry::default();
    metrics.register(&mut registry);
    let mut text = String::new();
    prometheus_client::encoding::text::encode(&mut text, &registry).unwrap();
    for expected in [
        r#"http_requests_total{method="POST",route="/api/v1/invoices",status="202"} 1"#,
        r#"http_requests_total{method="GET",route="/api/v1/documents/:id",status="404"} 1"#,
        r#"http_requests_total{method="GET",route="/api/v1/documents/:id",status="401"} 1"#,
        r#"http_request_duration_seconds_count{method="POST",route="/api/v1/invoices"} 1"#,
    ] {
        assert!(text.contains(expected), "missing {}\n{}", expected, text);
    }
    assert!(!text.contains(&missing), "concrete URLs must not become labels");
}

#[tokio::test]
async fn responses_carry_request_id_and_are_not_cacheable() {
    let (app, _) = app();
    let request = |id: Option<&str>| {
        let mut builder = Request::builder().uri("/livez");
        if let Some(id) = id {
            builder = builder.header("x-request-id", id);
        }
        builder.body(Body::empty()).unwrap()
    };
    let generated = app.clone().oneshot(request(None)).await.unwrap();
    assert!(Uuid::parse_str(generated.headers()["x-request-id"].to_str().unwrap()).is_ok());
    assert_eq!(generated.headers()["cache-control"], "no-store");
    let propagated = app.clone().oneshot(request(Some("erp-req-42"))).await.unwrap();
    assert_eq!(propagated.headers()["x-request-id"], "erp-req-42");
}

#[tokio::test]
async fn tenants_manage_their_webhook_subscriptions() {
    let (app, _) = app();
    let body = json!({ "target_url": "https://erp.example.com/hooks/dian", "event_type": "document.accepted" });
    let (status, _, created) = send(&app, "POST", "/api/v1/webhooks", Some(body), &[AUTH]).await;
    assert_eq!(status, StatusCode::CREATED);
    let secret = created["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("whsec_"));

    let (status, _, listed) = send(&app, "GET", "/api/v1/webhooks", None, &[AUTH]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["webhooks"].as_array().unwrap().len(), 1);
    assert!(!listed.to_string().contains(&secret), "the secret is shown only once");

    let internal = json!({ "target_url": "https://169.254.169.254/latest", "event_type": "*" });
    assert_eq!(send(&app, "POST", "/api/v1/webhooks", Some(internal), &[AUTH]).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let anonymous = json!({ "target_url": "https://erp.example.com/h", "event_type": "*" });
    assert_eq!(send(&app, "POST", "/api/v1/webhooks", Some(anonymous), &[]).await.0, StatusCode::UNAUTHORIZED);

    let uri = format!("/api/v1/webhooks/{}", created["id"].as_str().unwrap());
    assert_eq!(send(&app, "DELETE", &uri, None, &[AUTH]).await.0, StatusCode::NO_CONTENT);
    assert_eq!(send(&app, "DELETE", &uri, None, &[AUTH]).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn operators_onboard_tenants_through_the_admin_api() {
    let (app, store) = app();
    let admin = ("Authorization", "Bearer admin-token-for-tests");

    let tenant = json!({ "tax_id": "901234567", "registration_name": "Nuevo SAS", "tax_regime": "48", "reception_email": "fe@nuevo.co" });
    assert_eq!(send(&app, "POST", "/admin/v1/tenants", Some(tenant.clone()), &[]).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&app, "POST", "/admin/v1/tenants", Some(tenant.clone()), &[AUTH]).await.0, StatusCode::UNAUTHORIZED, "tenant keys are not admin tokens");
    let (status, _, created) = send(&app, "POST", "/admin/v1/tenants", Some(tenant.clone()), &[admin]).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(send(&app, "POST", "/admin/v1/tenants", Some(tenant), &[admin]).await.0, StatusCode::CONFLICT);
    let tenant_id = created["id"].as_str().unwrap().to_string();

    let range = json!({
        "document_type": "invoice", "prefix": "FE", "from_number": 1, "to_number": 100, "technical_key": "tk-secret",
        "valid_date_from": (Utc::now() - Duration::days(1)).to_rfc3339(), "valid_date_to": (Utc::now() + Duration::days(365)).to_rfc3339()
    });
    let (status, _, body) = send(&app, "POST", &format!("/admin/v1/tenants/{}/numbering-ranges", tenant_id), Some(range), &[admin]).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(!body.to_string().contains("tk-secret"), "the technical key is not echoed");

    let (status, _, key) = send(&app, "POST", &format!("/admin/v1/tenants/{}/api-keys", tenant_id), Some(json!({ "name": "ERP" })), &[admin]).await;
    assert_eq!(status, StatusCode::CREATED);
    let bearer = format!("Bearer {}", key["key"].as_str().unwrap());
    let auth = ("Authorization", bearer.as_str());

    let mut invoice = invoice(1);
    invoice["company_nit"] = json!("901234567");
    assert_eq!(send(&app, "POST", "/api/v1/invoices", Some(invoice), &[auth]).await.0, StatusCode::ACCEPTED);

    let (_, _, keys) = send(&app, "GET", &format!("/admin/v1/tenants/{}/api-keys", tenant_id), None, &[admin]).await;
    assert!(!keys.to_string().contains(key["key"].as_str().unwrap()));
    let revoke = format!("/admin/v1/tenants/{}/api-keys/{}", tenant_id, key["id"].as_str().unwrap());
    assert_eq!(send(&app, "DELETE", &revoke, None, &[admin]).await.0, StatusCode::NO_CONTENT);
    assert_eq!(send(&app, "GET", "/api/v1/webhooks", None, &[auth]).await.0, StatusCode::UNAUTHORIZED);

    let (status, _, dead) = send(&app, "GET", "/admin/v1/jobs/dead?limit=10", None, &[admin]).await;
    assert_eq!((status, dead["jobs"].as_array().unwrap().len()), (StatusCode::OK, 0));
    let missing = format!("/admin/v1/jobs/{}/retry", Uuid::new_v4());
    assert_eq!(send(&app, "POST", &missing, None, &[admin]).await.0, StatusCode::NOT_FOUND);

    let actions: Vec<&str> = store.audit_log().iter().map(|e| e.action).collect();
    assert_eq!(actions, vec!["tenant.created", "numbering_range.created", "api_key.created", "api_key.revoked"]);
}

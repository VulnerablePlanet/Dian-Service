//! Pruebas HTTP del router completo sobre el repositorio en memoria.

use api_server::config::ApiConfig;
use api_server::state::AppState;
use application::inbound::InboundService;
use application::issue::IssueService;
use application::radian::RadianService;
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
}

struct NoSoap;

#[async_trait]
impl DianSoapClient for NoSoap {
    async fn send_bill_sync(&self, _: &[u8], _: bool) -> Result<DianResponse, DomainError> {
        unreachable!()
    }
    async fn send_bill_async(&self, _: &[u8], _: bool) -> Result<String, DomainError> {
        unreachable!()
    }
    async fn send_test_set_async(&self, _: &[u8], _: &str, _: bool) -> Result<String, DomainError> {
        unreachable!()
    }
    async fn get_status(&self, _: &str, _: bool) -> Result<DianResponse, DomainError> {
        unreachable!()
    }
    async fn get_status_zip(&self, _: &str, _: bool) -> Result<DianResponse, DomainError> {
        unreachable!()
    }
    async fn send_event_update_status(&self, _: &[u8], _: bool) -> Result<DianResponse, DomainError> {
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

fn config() -> ApiConfig {
    ApiConfig {
        dian_binary_security_token: "token".into(),
        dian_hab_url: None,
        dian_prod_url: None,
        inbound_allow_unverified_signatures: false,
        transmit_max_attempts: 5,
    }
}

fn app_with(documents: Arc<dyn DocumentRepository>, health: Arc<dyn HealthProbe>, store: Arc<InMemoryStore>) -> axum::Router {
    api_server::router(Arc::new(AppState {
        tenants: store,
        documents: documents.clone(),
        health,
        issue: IssueService::new(documents.clone(), 5),
        inbound: InboundService::new(documents.clone()),
        radian: RadianService::new(documents, Arc::new(NoSigner), Arc::new(NoSoap)),
        config: config(),
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
    note["document_type"] = json!("credit_note");
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
}

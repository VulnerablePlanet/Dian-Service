//! Pruebas de integración contra PostgreSQL real.
//!
//! Requieren `TEST_DATABASE_URL` (por ejemplo `postgres://postgres@127.0.0.1:5432/postgres`):
//! cada prueba crea una base de datos nueva, aplica las migraciones y trabaja aislada. Sin la
//! variable, las pruebas se omiten con un aviso.

use chrono::{Duration as ChronoDuration, Utc};
use core_domain::jobs::NewJob;
use core_domain::models::{DianEvent, DocumentStatus, DocumentType, EventStatus, JobKind};
use core_domain::ports::{
    DocumentRepository, DomainError, IdempotencyKey, IssueOutcome, JobOutcome, JobRepository,
    NewInboundDocument, NewOutboundDocument, Transition, WebhookRepository, AdminRepository, TenantRepository,
};
use core_domain::admin::{ApiKeyRecord, AuditEntry, NewTenant};
use core_domain::models::{Certificate, CertificateStatus, NumberingRange};
use core_domain::models::{Webhook, WebhookSubscriptionStatus};
use infra_postgres::{PgStore, MIGRATOR};
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, Executor, PgPool};
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

struct Fixture {
    store: Arc<PgStore>,
    pool: PgPool,
    tenant_id: Uuid,
}

async fn fixture() -> Option<Fixture> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL not set: skipping PostgreSQL integration test");
        return None;
    };
    let admin = PgConnectOptions::from_str(&url).expect("valid TEST_DATABASE_URL");
    let name = format!("dian_test_{}", Uuid::new_v4().simple());
    let mut conn = admin.connect().await.expect("connect to admin database");
    conn.execute(format!("CREATE DATABASE {}", name).as_str()).await.unwrap();

    let pool = PgPoolOptions::new()
        .max_connections(40)
        .connect_with(admin.database(&name))
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.expect("migrations apply cleanly");

    let tenant_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tenants (id, tax_id, registration_name, tax_regime, reception_email, api_keys)
         VALUES ($1, '900123456', 'Tenant SAS', '48', 'f@t.co', '{\"secret\":\"k\"}')",
    )
    .bind(tenant_id)
    .execute(&pool)
    .await
    .unwrap();

    Some(Fixture {
        store: Arc::new(PgStore::new(pool.clone())),
        pool,
        tenant_id,
    })
}

impl Fixture {
    async fn add_range(&self, doc_type: DocumentType, prefix: &str, from: i32, to: i32) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO numbering_ranges (id, tenant_id, document_type, prefix, from_number, to_number, technical_key,
                                           valid_date_from, valid_date_to, current_counter)
             VALUES ($1, $2, $3::document_type, $4, $5, $6, 'tk', now() - interval '1 day', now() + interval '1 day', 0)",
        )
        .bind(id)
        .bind(self.tenant_id)
        .bind(doc_type.as_str())
        .bind(prefix)
        .bind(from)
        .bind(to)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    fn new_doc(&self, prefix: &str, requested: Option<i32>, idem: Option<(&str, &str)>) -> (NewOutboundDocument, NewJob) {
        let id = Uuid::new_v4();
        (
            NewOutboundDocument {
                id,
                tenant_id: self.tenant_id,
                document_type: DocumentType::Invoice,
                prefix: prefix.to_string(),
                requested_number: requested,
                payload: json!({ "k": "v" }),
                idempotency: idem.map(|(k, h)| IdempotencyKey {
                    key: k.to_string(),
                    request_hash: h.to_string(),
                }),
            },
            NewJob::transmit(id, 5),
        )
    }

    async fn issue(&self, prefix: &str, requested: Option<i32>) -> Result<IssueOutcome, DomainError> {
        let (doc, job) = self.new_doc(prefix, requested, None);
        self.store.issue_outbound(doc, job).await
    }

    async fn job_status(&self, document_id: Uuid) -> Vec<(String, String, i32)> {
        sqlx::query_as::<_, (String, String, i32)>(
            "SELECT kind::text, status::text, attempts FROM jobs WHERE document_id = $1 ORDER BY created_at",
        )
        .bind(document_id)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

fn created(outcome: IssueOutcome) -> (Uuid, i32) {
    match outcome {
        IssueOutcome::Created(d) => (d.document_id, d.document_number),
        other => panic!("expected Created, got {:?}", other),
    }
}

#[tokio::test]
async fn issue_assigns_consecutive_numbers_and_enqueues_atomically() {
    let Some(f) = fixture().await else { return };
    let range_id = f.add_range(DocumentType::Invoice, "FE", 5000, 6000).await;

    let (id1, n1) = created(f.issue("FE", Some(5000)).await.unwrap());
    let (_, n2) = created(f.issue("FE", None).await.unwrap());
    assert_eq!((n1, n2), (5000, 5001));

    let doc = f.store.find_document(id1).await.unwrap().unwrap();
    assert_eq!(doc.status, DocumentStatus::Pending);
    assert_eq!(doc.numbering_range_id, Some(range_id));
    assert_eq!(f.job_status(id1).await, vec![("transmit".into(), "queued".into(), 0)]);

    // Un número fuera de secuencia se rechaza y no consume consecutivo.
    assert!(matches!(f.issue("FE", Some(5010)).await, Err(DomainError::Validation(_))));
    let (_, n3) = created(f.issue("FE", None).await.unwrap());
    assert_eq!(n3, 5002);
}

#[tokio::test]
async fn concurrent_issuance_never_duplicates_or_skips_numbers() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;
    let f = Arc::new(f);

    let tasks: Vec<_> = (0..30)
        .map(|_| {
            let f = f.clone();
            tokio::spawn(async move { created(f.issue("FE", None).await.unwrap()).1 })
        })
        .collect();
    let mut numbers = Vec::new();
    for t in tasks {
        numbers.push(t.await.unwrap());
    }
    numbers.sort();
    assert_eq!(numbers, (1..=30).collect::<Vec<_>>());
}

#[tokio::test]
async fn idempotency_key_replays_and_rejects_different_body() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;

    let (doc, job) = f.new_doc("FE", None, Some(("key-1", "hash-a")));
    let (first_id, first_number) = created(f.store.issue_outbound(doc, job).await.unwrap());

    let (doc, job) = f.new_doc("FE", None, Some(("key-1", "hash-a")));
    match f.store.issue_outbound(doc, job).await.unwrap() {
        IssueOutcome::Replayed(d) => assert_eq!((d.document_id, d.document_number), (first_id, first_number)),
        other => panic!("expected Replayed, got {:?}", other),
    }

    let (doc, job) = f.new_doc("FE", None, Some(("key-1", "hash-b")));
    assert!(matches!(f.store.issue_outbound(doc, job).await, Err(DomainError::Validation(_))));

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM documents").fetch_one(&f.pool).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn concurrent_requests_with_same_key_create_one_document() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;
    let f = Arc::new(f);

    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let f = f.clone();
            tokio::spawn(async move {
                let (doc, job) = f.new_doc("FE", None, Some(("same-key", "same-hash")));
                match f.store.issue_outbound(doc, job).await.unwrap() {
                    IssueOutcome::Created(d) | IssueOutcome::Replayed(d) => d.document_id,
                }
            })
        })
        .collect();
    let mut ids = HashSet::new();
    for t in tasks {
        ids.insert(t.await.unwrap());
    }
    assert_eq!(ids.len(), 1, "every request must resolve to the same document");

    let (docs, counter): (i64, i32) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM documents), (SELECT current_counter FROM numbering_ranges LIMIT 1)",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!((docs, counter), (1, 1), "rolled-back attempts must not consume numbers");
}

#[tokio::test]
async fn claims_are_exclusive_and_expired_leases_are_recovered() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;
    for _ in 0..10 {
        f.issue("FE", None).await.unwrap();
    }
    let store = f.store.clone();

    let (a, b) = tokio::join!(
        store.claim("worker-a", 10, Duration::from_secs(600)),
        store.claim("worker-b", 10, Duration::from_secs(600))
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    let ids: HashSet<Uuid> = a.iter().chain(b.iter()).map(|j| j.id).collect();
    assert_eq!(a.len() + b.len(), 10);
    assert_eq!(ids.len(), 10, "no job may be claimed twice");
    assert!(store.claim("worker-c", 10, Duration::from_secs(600)).await.unwrap().is_empty());

    // Un worker que muere deja su lease vencer: el trabajo se vuelve a reclamar.
    sqlx::query("UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1")
        .bind(a.first().or(b.first()).unwrap().id)
        .execute(&f.pool)
        .await
        .unwrap();
    let recovered = store.claim("worker-d", 10, Duration::from_secs(600)).await.unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].attempts, 2);
}

#[tokio::test]
async fn retry_release_and_bury_follow_their_contracts() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;
    let (doc_id, _) = created(f.issue("FE", None).await.unwrap());
    let store = f.store.clone();

    let job = store.claim("w", 1, Duration::from_secs(600)).await.unwrap().remove(0);
    store.retry(job.id, Utc::now() + ChronoDuration::hours(1), "boom").await.unwrap();
    assert!(store.claim("w", 1, Duration::from_secs(600)).await.unwrap().is_empty(), "retry respects run_at");

    sqlx::query("UPDATE jobs SET run_at = now()").execute(&f.pool).await.unwrap();
    let job = store.claim("w", 1, Duration::from_secs(600)).await.unwrap().remove(0);
    assert_eq!(job.attempts, 2);
    store.release(job.id, Utc::now()).await.unwrap();
    let job = store.claim("w", 1, Duration::from_secs(600)).await.unwrap().remove(0);
    assert_eq!(job.attempts, 2, "release must not consume an attempt");

    store.bury(job.id, "gave up").await.unwrap();
    assert_eq!(f.job_status(doc_id).await, vec![("transmit".into(), "dead".into(), 2)]);
}

#[tokio::test]
async fn transitions_are_guarded_and_atomic() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;
    let (doc_id, _) = created(f.issue("FE", None).await.unwrap());
    let store = f.store.clone();
    let job = store.claim("w", 1, Duration::from_secs(600)).await.unwrap().remove(0);

    let applied = store
        .apply_transition(
            Transition::new(doc_id, DocumentStatus::SentDian)
                .with_event(DianEvent::new(doc_id, EventStatus::Sent, "SENT_ASYNC", "ok"))
                .with_job(JobOutcome::Complete(job.id))
                .with_follow_up(NewJob::poll_status(doc_id, "zip-1", 10, Utc::now())),
        )
        .await
        .unwrap();
    assert!(applied);
    assert_eq!(
        f.job_status(doc_id).await,
        vec![("transmit".into(), "done".into(), 1), ("poll_status".into(), "queued".into(), 0)]
    );

    // Hallazgo R-11: un documento enviado no puede retroceder a contingencia.
    let regressed = store
        .apply_transition(
            Transition::new(doc_id, DocumentStatus::ContingencyPending)
                .with_event(DianEvent::new(doc_id, EventStatus::Error, "X", "must not be stored")),
        )
        .await
        .unwrap();
    assert!(!regressed);
    let doc = store.find_document(doc_id).await.unwrap().unwrap();
    assert_eq!(doc.status, DocumentStatus::SentDian);
    assert_eq!(store.find_events(doc_id).await.unwrap().len(), 1, "a rejected transition stores nothing");

    // Una transición que falla a mitad (trabajo de seguimiento duplicado) no deja rastro.
    let poll = store.claim("w", 1, Duration::from_secs(600)).await.unwrap().remove(0);
    assert_eq!(poll.kind, JobKind::PollStatus);
    let duplicate_follow_up = store
        .apply_transition(
            Transition::new(doc_id, DocumentStatus::DianAccepted)
                .with_event(DianEvent::new(doc_id, EventStatus::Accepted, "00", "ok"))
                .with_follow_up(NewJob::poll_status(doc_id, "zip-2", 10, Utc::now())),
        )
        .await;
    assert!(matches!(duplicate_follow_up, Err(DomainError::Conflict(_))));
    assert_eq!(store.find_document(doc_id).await.unwrap().unwrap().status, DocumentStatus::SentDian);
}

#[tokio::test]
async fn inbound_duplicates_are_rejected_without_overwriting() {
    let Some(f) = fixture().await else { return };
    let inbound = |issuer: &str| NewInboundDocument {
        id: Uuid::new_v4(),
        tenant_id: f.tenant_id,
        issuer_nit: issuer.to_string(),
        document_type: DocumentType::Invoice,
        prefix: "FEV".to_string(),
        document_number: 7,
        cufe: "cufe".to_string(),
        payload: json!({ "inbound": true }),
        signed_xml: "<x/>".to_string(),
        status: DocumentStatus::DianAccepted,
    };
    assert!(f.store.insert_inbound(inbound("800111222")).await.unwrap());
    assert!(!f.store.insert_inbound(inbound("800111222")).await.unwrap());
    assert!(f.store.insert_inbound(inbound("811222333")).await.unwrap(), "same number, other supplier");
}

#[tokio::test]
async fn contingency_documents_are_requeued_once() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;
    let (doc_id, _) = created(f.issue("FE", None).await.unwrap());
    let store = f.store.clone();
    let job = store.claim("w", 1, Duration::from_secs(600)).await.unwrap().remove(0);
    store
        .apply_transition(
            Transition::new(doc_id, DocumentStatus::ContingencyPending).with_job(JobOutcome::Bury(job.id, "down".into())),
        )
        .await
        .unwrap();

    assert_eq!(store.requeue_contingency(10, 5).await.unwrap(), 1);
    assert_eq!(store.requeue_contingency(10, 5).await.unwrap(), 0, "already has an active job");
    assert_eq!(store.find_document(doc_id).await.unwrap().unwrap().status, DocumentStatus::Pending);
}

#[tokio::test]
async fn queue_depth_counts_jobs_by_state() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;
    for _ in 0..3 {
        f.issue("FE", None).await.unwrap();
    }
    let store = f.store.clone();
    let mut claimed = store.claim("w", 2, Duration::from_secs(600)).await.unwrap();
    let dead = claimed.remove(0);
    store.bury(dead.id, "gave up").await.unwrap();
    let depth = store.queue_depth().await.unwrap();
    assert_eq!((depth.ready, depth.scheduled, depth.running, depth.dead), (1, 0, 1, 1));

    store.retry(claimed[0].id, Utc::now() + ChronoDuration::minutes(5), "later").await.unwrap();
    assert_eq!(store.queue_depth().await.unwrap().scheduled, 1);
}

#[tokio::test]
async fn final_transitions_enqueue_webhook_deliveries_atomically() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;
    let store = f.store.clone();
    let webhook = |event: &str| Webhook {
        id: Uuid::new_v4(),
        tenant_id: f.tenant_id,
        target_url: "https://erp.example.com/h".into(),
        webhook_secret: "whsec_test".into(),
        event_type: event.into(),
        subscription_status: WebhookSubscriptionStatus::Active,
    };
    let accepted_only = webhook("document.accepted");
    store.create_webhook(accepted_only.clone()).await.unwrap();
    store.create_webhook(webhook("*")).await.unwrap();
    store.create_webhook(webhook("document.rejected")).await.unwrap();
    assert!(store.create_webhook(webhook("document.created")).await.is_err(), "event type is checked by the database");
    assert_eq!(store.list_webhooks(f.tenant_id).await.unwrap().len(), 3);

    let (doc_id, _) = created(f.issue("FE", None).await.unwrap());
    store.apply_transition(Transition::new(doc_id, DocumentStatus::SentDian)).await.unwrap();
    assert!(store.claim_deliveries(10, Duration::from_secs(60)).await.unwrap().is_empty());

    // Una transición rechazada (estado de origen inválido) no encola nada.
    assert!(!store.apply_transition(Transition::new(doc_id, DocumentStatus::Pending)).await.unwrap());
    store.apply_transition(Transition::new(doc_id, DocumentStatus::DianAccepted)).await.unwrap();

    let claimed = store.claim_deliveries(10, Duration::from_secs(60)).await.unwrap();
    assert_eq!(claimed.len(), 2, "accepted-only and wildcard subscriptions");
    assert!(claimed.iter().all(|d| d.event_type == "document.accepted" && d.attempts == 1 && d.active));
    assert_eq!(claimed[0].data["document_id"], doc_id.to_string());
    assert!(store.claim_deliveries(10, Duration::from_secs(60)).await.unwrap().is_empty(), "claims are exclusive");

    store.complete_delivery(claimed[0].id, 204).await.unwrap();
    store.retry_delivery(claimed[1].id, Utc::now() - ChronoDuration::seconds(1), "HTTP 500", Some(500)).await.unwrap();
    let again = store.claim_deliveries(10, Duration::from_secs(60)).await.unwrap();
    assert_eq!((again.len(), again[0].attempts), (1, 2));
    store.bury_delivery(again[0].id, "gave up", None).await.unwrap();
    assert!(store.claim_deliveries(10, Duration::from_secs(60)).await.unwrap().is_empty());

    assert!(store.delete_webhook(f.tenant_id, accepted_only.id).await.unwrap());
    assert!(!store.delete_webhook(Uuid::new_v4(), accepted_only.id).await.unwrap());
}

fn entry(action: &'static str) -> AuditEntry {
    AuditEntry { action, tenant_id: None, subject_id: None, details: json!({ "k": "v" }) }
}

#[tokio::test]
async fn admin_operations_are_atomic_with_their_audit_entry() {
    let Some(f) = fixture().await else { return };
    let store = f.store.clone();
    let tenant = store
        .create_tenant(
            NewTenant {
                id: Uuid::new_v4(),
                tax_id: "901234567".into(),
                registration_name: "Nuevo SAS".into(),
                tax_regime: "48".into(),
                reception_email: "fe@nuevo.co".into(),
            },
            &entry("tenant.created"),
        )
        .await
        .unwrap();
    let duplicate = NewTenant {
        id: Uuid::new_v4(),
        tax_id: "901234567".into(),
        registration_name: "Otro".into(),
        tax_regime: "48".into(),
        reception_email: "a@b.co".into(),
    };
    assert!(matches!(store.create_tenant(duplicate, &entry("tenant.created")).await, Err(DomainError::Conflict(_))));

    // API keys: búsqueda por hash, revocación.
    let key = ApiKeyRecord {
        id: Uuid::new_v4(),
        tenant_id: tenant.id,
        name: "ERP".into(),
        prefix: "dsk_abcdef12".into(),
        key_hash: core_domain::api_keys::hash("dsk_secret"),
        created_at: Utc::now(),
        revoked_at: None,
    };
    store.create_api_key(key.clone(), &entry("api_key.created")).await.unwrap();
    let found = store.find_by_api_key_hash(&core_domain::api_keys::hash("dsk_secret")).await.unwrap();
    assert_eq!(found.unwrap().id, tenant.id);
    assert!(store.find_by_api_key_hash(&core_domain::api_keys::hash("k")).await.unwrap().is_none(), "plaintext column is not used");
    assert!(store.revoke_api_key(tenant.id, key.id, &entry("api_key.revoked")).await.unwrap());
    assert!(!store.revoke_api_key(tenant.id, key.id, &entry("api_key.revoked")).await.unwrap());
    assert!(store.find_by_api_key_hash(&key.key_hash).await.unwrap().is_none());

    // Certificados: uno activo.
    let certificate = |path: &str| Certificate {
        id: Uuid::new_v4(),
        tenant_id: tenant.id,
        vault_reference_path: path.into(),
        thumbprint: "AB".into(),
        expiration_date: Utc::now() + ChronoDuration::days(300),
        status: CertificateStatus::Active,
    };
    store.activate_certificate(certificate("t/1"), &entry("certificate.activated")).await.unwrap();
    store.activate_certificate(certificate("t/2"), &entry("certificate.activated")).await.unwrap();
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM certificates WHERE tenant_id = $1 AND status = 'active'")
        .bind(tenant.id).fetch_one(&f.pool).await.unwrap();
    assert_eq!(active, 1);

    // Rangos: sin solape de vigencia.
    let range = |from: i64, to: i64| NumberingRange {
        id: Uuid::new_v4(),
        tenant_id: tenant.id,
        document_type: DocumentType::Invoice,
        prefix: "FE".into(),
        from_number: 1,
        to_number: 100,
        technical_key: "tk".into(),
        valid_date_from: Utc::now() + ChronoDuration::days(from),
        valid_date_to: Utc::now() + ChronoDuration::days(to),
        current_counter: 0,
    };
    store.add_numbering_range(range(-1, 100), &entry("numbering_range.created")).await.unwrap();
    assert!(matches!(
        store.add_numbering_range(range(50, 200), &entry("numbering_range.created")).await,
        Err(DomainError::Conflict(_))
    ));
    store.add_numbering_range(range(100, 200), &entry("numbering_range.created")).await.unwrap();

    let audited: Vec<String> = sqlx::query_scalar("SELECT action FROM admin_audit_log ORDER BY created_at, action")
        .fetch_all(&f.pool).await.unwrap();
    assert_eq!(audited.len(), 7, "failed and no-op operations leave no audit entry: {:?}", audited);
}

#[tokio::test]
async fn dead_jobs_are_listed_and_retried_with_document_guard() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;
    let store = f.store.clone();
    let (doc_id, _) = created(f.issue("FE", None).await.unwrap());
    let job = store.claim("w", 1, Duration::from_secs(600)).await.unwrap().remove(0);
    store
        .apply_transition(Transition::new(doc_id, DocumentStatus::ContingencyPending).with_job(JobOutcome::Bury(job.id, "down".into())))
        .await
        .unwrap();

    let dead = store.list_dead_jobs(10).await.unwrap();
    assert_eq!((dead.len(), dead[0].tenant_id, dead[0].kind), (1, f.tenant_id, JobKind::Transmit));
    assert!(store.retry_dead_job(job.id, &entry("job.retried")).await.unwrap());
    assert!(!store.retry_dead_job(job.id, &entry("job.retried")).await.unwrap(), "no longer dead");
    assert_eq!(store.find_document(doc_id).await.unwrap().unwrap().status, DocumentStatus::Pending);
    let again = store.claim("w", 1, Duration::from_secs(600)).await.unwrap().remove(0);
    assert_eq!((again.id, again.attempts), (job.id, 1));

    store
        .apply_transition(Transition::new(doc_id, DocumentStatus::DianAccepted).with_job(JobOutcome::Bury(job.id, "late".into())))
        .await
        .unwrap();
    assert!(matches!(store.retry_dead_job(job.id, &entry("job.retried")).await, Err(DomainError::Conflict(_))));
}

#[tokio::test]
async fn tenants_cannot_be_deleted_while_they_have_documents() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;
    f.issue("FE", None).await.unwrap();
    let result = sqlx::query("DELETE FROM tenants WHERE id = $1").bind(f.tenant_id).execute(&f.pool).await;
    assert!(result.is_err());
}

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
    NewInboundDocument, NewOutboundDocument, Transition,
};
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
async fn tenants_cannot_be_deleted_while_they_have_documents() {
    let Some(f) = fixture().await else { return };
    f.add_range(DocumentType::Invoice, "FE", 1, 1000).await;
    f.issue("FE", None).await.unwrap();
    let result = sqlx::query("DELETE FROM tenants WHERE id = $1").bind(f.tenant_id).execute(&f.pool).await;
    assert!(result.is_err());
}

//! Adaptador PostgreSQL de los puertos de persistencia del dominio.
//!
//! Los enums del dominio se guardan en tipos enum de PostgreSQL: se escriben como texto con
//! un cast explícito (`$1::document_status`) y se leen como texto (`status::text`), de modo
//! que el dominio no depende de `sqlx`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use core_domain::admin::{ApiKeyRecord, AuditEntry, DeadDelivery, DeadJob, NewTenant};
use core_domain::jobs::{Job, NewJob, QueueDepth};
use core_domain::models::{
    Certificate, DianEvent, Document, DocumentStatus, JobKind, NumberingRange, Tenant, Webhook,
};
use core_domain::numbering::next_number;
use core_domain::ports::{
    AdminRepository, CertificateRepository, DocumentRepository, DomainError, HealthProbe, IdempotencyKey,
    IssueOutcome, IssuedDocument, JobOutcome, JobRepository, NewInboundDocument,
    NewOutboundDocument, TenantRepository, Transition, WebhookDelivery, WebhookRepository,
};
use core_domain::webhooks::{event_for, notification_data, NotifiedDocument, MAX_DELIVERY_ATTEMPTS};
use sqlx::postgres::{PgPool, PgRow};
use sqlx::{Postgres, Row, Transaction};
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;

/// Migraciones del proyecto (`migrations/`), embebidas en el binario.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../migrations");

const DOCUMENT_COLUMNS: &str = "id, tenant_id, direction, issuer_nit, document_type::text AS document_type, \
     prefix, document_number, numbering_range_id, cufe_cude, payload, original_xml, signed_xml, \
     pdf_url, status::text AS status, created_at";

const EVENT_COLUMNS: &str = "id, document_id, event_status::text AS event_status, dian_response_code, \
     dian_response_message, soap_trace_id, xml_response, created_at";

const RANGE_COLUMNS: &str = "id, tenant_id, document_type::text AS document_type, prefix, from_number, \
     to_number, technical_key, valid_date_from, valid_date_to, current_counter";

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

// ---------------------------------------------------------------------------------------
// Conversión de errores y filas
// ---------------------------------------------------------------------------------------

fn db(e: sqlx::Error) -> DomainError {
    DomainError::Database(e.to_string())
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23505"))
}

fn parse<T: FromStr>(row: &PgRow, column: &str) -> Result<T, DomainError>
where
    T::Err: std::fmt::Display,
{
    let text: String = row.try_get(column).map_err(db)?;
    T::from_str(&text).map_err(|e| DomainError::Internal(format!("column {}: {}", column, e)))
}

fn tenant_from_row(row: &PgRow) -> Result<Tenant, DomainError> {
    Ok(Tenant {
        id: row.try_get("id").map_err(db)?,
        tax_id: row.try_get("tax_id").map_err(db)?,
        registration_name: row.try_get("registration_name").map_err(db)?,
        tax_regime: row.try_get("tax_regime").map_err(db)?,
        address_info: row.try_get("address_info").map_err(db)?,
        reception_email: row.try_get("reception_email").map_err(db)?,
        api_keys: row.try_get("api_keys").map_err(db)?,
        created_at: row.try_get("created_at").map_err(db)?,
        updated_at: row.try_get("updated_at").map_err(db)?,
    })
}

fn document_from_row(row: &PgRow) -> Result<Document, DomainError> {
    Ok(Document {
        id: row.try_get("id").map_err(db)?,
        tenant_id: row.try_get("tenant_id").map_err(db)?,
        direction: parse(row, "direction")?,
        issuer_nit: row.try_get("issuer_nit").map_err(db)?,
        document_type: parse(row, "document_type")?,
        prefix: row.try_get("prefix").map_err(db)?,
        document_number: row.try_get("document_number").map_err(db)?,
        numbering_range_id: row.try_get("numbering_range_id").map_err(db)?,
        cufe_cude: row.try_get("cufe_cude").map_err(db)?,
        payload: row.try_get("payload").map_err(db)?,
        original_xml: row.try_get("original_xml").map_err(db)?,
        signed_xml: row.try_get("signed_xml").map_err(db)?,
        pdf_url: row.try_get("pdf_url").map_err(db)?,
        status: parse(row, "status")?,
        created_at: row.try_get("created_at").map_err(db)?,
    })
}

fn event_from_row(row: &PgRow) -> Result<DianEvent, DomainError> {
    Ok(DianEvent {
        id: row.try_get("id").map_err(db)?,
        document_id: row.try_get("document_id").map_err(db)?,
        event_status: parse(row, "event_status")?,
        dian_response_code: row.try_get("dian_response_code").map_err(db)?,
        dian_response_message: row.try_get("dian_response_message").map_err(db)?,
        soap_trace_id: row.try_get("soap_trace_id").map_err(db)?,
        xml_response: row.try_get("xml_response").map_err(db)?,
        created_at: row.try_get("created_at").map_err(db)?,
    })
}

fn range_from_row(row: &PgRow) -> Result<NumberingRange, DomainError> {
    Ok(NumberingRange {
        id: row.try_get("id").map_err(db)?,
        tenant_id: row.try_get("tenant_id").map_err(db)?,
        document_type: parse(row, "document_type")?,
        prefix: row.try_get("prefix").map_err(db)?,
        from_number: row.try_get("from_number").map_err(db)?,
        to_number: row.try_get("to_number").map_err(db)?,
        technical_key: row.try_get("technical_key").map_err(db)?,
        valid_date_from: row.try_get("valid_date_from").map_err(db)?,
        valid_date_to: row.try_get("valid_date_to").map_err(db)?,
        current_counter: row.try_get("current_counter").map_err(db)?,
    })
}

fn job_from_row(row: &PgRow) -> Result<Job, DomainError> {
    Ok(Job {
        id: row.try_get("id").map_err(db)?,
        kind: parse(row, "kind")?,
        document_id: row.try_get("document_id").map_err(db)?,
        payload: row.try_get("payload").map_err(db)?,
        attempts: row.try_get("attempts").map_err(db)?,
        max_attempts: row.try_get("max_attempts").map_err(db)?,
    })
}

async fn insert_job(tx: &mut Transaction<'_, Postgres>, job: &NewJob) -> Result<(), DomainError> {
    sqlx::query(
        "INSERT INTO jobs (id, kind, document_id, payload, max_attempts, run_at)
         VALUES ($1, $2::job_kind, $3, $4, $5, $6)",
    )
    .bind(job.id)
    .bind(job.kind.as_str())
    .bind(job.document_id)
    .bind(&job.payload)
    .bind(job.max_attempts)
    .bind(job.run_at)
    .execute(&mut **tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            DomainError::Conflict(format!(
                "document {} already has an active {} job",
                job.document_id, job.kind
            ))
        } else {
            db(e)
        }
    })?;
    Ok(())
}

async fn insert_event(tx: &mut Transaction<'_, Postgres>, event: &DianEvent) -> Result<(), DomainError> {
    sqlx::query(
        "INSERT INTO dian_events (id, document_id, event_status, dian_response_code, dian_response_message, soap_trace_id, xml_response, created_at)
         VALUES ($1, $2, $3::event_status, $4, $5, $6, $7, $8)",
    )
    .bind(event.id)
    .bind(event.document_id)
    .bind(event.event_status.as_str())
    .bind(&event.dian_response_code)
    .bind(&event.dian_response_message)
    .bind(&event.soap_trace_id)
    .bind(&event.xml_response)
    .bind(event.created_at)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(())
}

/// Compara la huella de una clave de idempotencia ya registrada con la de la solicitud.
fn replay(stored_hash: &str, issued: IssuedDocument, key: &IdempotencyKey) -> Result<IssueOutcome, DomainError> {
    if stored_hash.trim() != key.request_hash {
        return Err(DomainError::Validation(
            "La clave de idempotencia ya se uso con un cuerpo distinto".to_string(),
        ));
    }
    Ok(IssueOutcome::Replayed(issued))
}

impl PgStore {
    async fn find_idempotency(
        &self,
        tenant_id: Uuid,
        key: &IdempotencyKey,
    ) -> Result<Option<IssueOutcome>, DomainError> {
        let row = sqlx::query(
            "SELECT request_hash, document_id, document_number FROM idempotency_keys
             WHERE tenant_id = $1 AND idempotency_key = $2",
        )
        .bind(tenant_id)
        .bind(&key.key)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        match row {
            None => Ok(None),
            Some(row) => {
                let hash: String = row.try_get("request_hash").map_err(db)?;
                let issued = IssuedDocument {
                    document_id: row.try_get("document_id").map_err(db)?,
                    document_number: row.try_get("document_number").map_err(db)?,
                };
                replay(&hash, issued, key).map(Some)
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Puertos
// ---------------------------------------------------------------------------------------

#[async_trait]
impl TenantRepository for PgStore {
    async fn find_by_api_key_hash(&self, key_hash: &str) -> Result<Option<Tenant>, DomainError> {
        let row = sqlx::query(
            "SELECT t.id, t.tax_id, t.registration_name, t.tax_regime, t.address_info, t.reception_email,
                    t.api_keys, t.created_at, t.updated_at
             FROM api_keys k JOIN tenants t ON t.id = k.tenant_id
             WHERE k.key_hash = $1 AND k.revoked_at IS NULL",
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(tenant_from_row).transpose()
    }
}

#[async_trait]
impl CertificateRepository for PgStore {
    async fn find_active_certificate(&self, tenant_id: Uuid) -> Result<Option<Certificate>, DomainError> {
        let row = sqlx::query(
            "SELECT id, tenant_id, vault_reference_path, thumbprint, expiration_date, status::text AS status
             FROM certificates
             WHERE tenant_id = $1 AND status = 'active'
             ORDER BY expiration_date DESC
             LIMIT 1",
        )
        .bind(tenant_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.map(|row| {
            Ok(Certificate {
                id: row.try_get("id").map_err(db)?,
                tenant_id: row.try_get("tenant_id").map_err(db)?,
                vault_reference_path: row.try_get("vault_reference_path").map_err(db)?,
                thumbprint: row.try_get("thumbprint").map_err(db)?,
                expiration_date: row.try_get("expiration_date").map_err(db)?,
                status: parse(&row, "status")?,
            })
        })
        .transpose()
    }
}

#[async_trait]
impl DocumentRepository for PgStore {
    async fn issue_outbound(
        &self,
        document: NewOutboundDocument,
        initial_job: NewJob,
    ) -> Result<IssueOutcome, DomainError> {
        if let Some(key) = &document.idempotency {
            if let Some(outcome) = self.find_idempotency(document.tenant_id, key).await? {
                return Ok(outcome);
            }
        }

        let mut tx = self.pool.begin().await.map_err(db)?;

        // Bloqueo pesimista corto sobre la resolución vigente: serializa la asignación del
        // consecutivo entre solicitudes concurrentes del mismo prefijo.
        let range_row = sqlx::query(&format!(
            "SELECT {RANGE_COLUMNS} FROM numbering_ranges
             WHERE tenant_id = $1 AND document_type = $2::document_type AND prefix = $3
               AND valid_date_from <= now() AND valid_date_to >= now()
             ORDER BY valid_date_from DESC
             LIMIT 1
             FOR UPDATE"
        ))
        .bind(document.tenant_id)
        .bind(document.document_type.as_str())
        .bind(&document.prefix)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| {
            DomainError::Validation(format!(
                "No hay una resolucion de numeracion vigente para el prefijo '{}' y tipo '{}'",
                document.prefix, document.document_type
            ))
        })?;
        let range = range_from_row(&range_row)?;
        let number = next_number(&range, document.requested_number, Utc::now())?;

        sqlx::query("UPDATE numbering_ranges SET current_counter = $1 WHERE id = $2")
            .bind(number)
            .bind(range.id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;

        sqlx::query(
            "INSERT INTO documents (id, tenant_id, direction, issuer_nit, document_type, prefix, document_number,
                                    numbering_range_id, cufe_cude, payload, status, created_at)
             VALUES ($1, $2, 'outbound', '', $3::document_type, $4, $5, $6, '', $7, 'pending', now())",
        )
        .bind(document.id)
        .bind(document.tenant_id)
        .bind(document.document_type.as_str())
        .bind(&document.prefix)
        .bind(number)
        .bind(range.id)
        .bind(&document.payload)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                DomainError::Conflict(format!("El documento {}{} ya existe", document.prefix, number))
            } else {
                db(e)
            }
        })?;

        insert_job(&mut tx, &initial_job).await?;

        let issued = IssuedDocument {
            document_id: document.id,
            document_number: number,
        };

        if let Some(key) = &document.idempotency {
            // Si otra solicitud con la misma clave confirmó primero, este INSERT espera a que
            // termine y no inserta nada: se descarta esta transacción y se devuelve la suya.
            let inserted = sqlx::query(
                "INSERT INTO idempotency_keys (tenant_id, idempotency_key, request_hash, document_id, document_number)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (tenant_id, idempotency_key) DO NOTHING
                 RETURNING tenant_id",
            )
            .bind(document.tenant_id)
            .bind(&key.key)
            .bind(&key.request_hash)
            .bind(document.id)
            .bind(number)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?;

            if inserted.is_none() {
                tx.rollback().await.map_err(db)?;
                return self
                    .find_idempotency(document.tenant_id, key)
                    .await?
                    .ok_or_else(|| DomainError::Internal("idempotency key vanished".to_string()));
            }
        }

        tx.commit().await.map_err(db)?;
        Ok(IssueOutcome::Created(issued))
    }

    async fn insert_inbound(&self, document: NewInboundDocument) -> Result<bool, DomainError> {
        let inserted = sqlx::query(
            "INSERT INTO documents (id, tenant_id, direction, issuer_nit, document_type, prefix, document_number,
                                    cufe_cude, payload, signed_xml, status, created_at)
             VALUES ($1, $2, 'inbound', $3, $4::document_type, $5, $6, $7, $8, $9, $10::document_status, now())
             ON CONFLICT ON CONSTRAINT uq_tenant_document DO NOTHING
             RETURNING id",
        )
        .bind(document.id)
        .bind(document.tenant_id)
        .bind(&document.issuer_nit)
        .bind(document.document_type.as_str())
        .bind(&document.prefix)
        .bind(document.document_number)
        .bind(&document.cufe)
        .bind(&document.payload)
        .bind(&document.signed_xml)
        .bind(document.status.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(inserted.is_some())
    }

    async fn find_document(&self, id: Uuid) -> Result<Option<Document>, DomainError> {
        let row = sqlx::query(&format!("SELECT {DOCUMENT_COLUMNS} FROM documents WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
        row.as_ref().map(document_from_row).transpose()
    }

    async fn find_tenant_document(&self, tenant_id: Uuid, id: Uuid) -> Result<Option<Document>, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {DOCUMENT_COLUMNS} FROM documents WHERE id = $1 AND tenant_id = $2"
        ))
        .bind(id)
        .bind(tenant_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(document_from_row).transpose()
    }

    async fn find_numbering_range(&self, id: Uuid) -> Result<Option<NumberingRange>, DomainError> {
        let row = sqlx::query(&format!("SELECT {RANGE_COLUMNS} FROM numbering_ranges WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
        row.as_ref().map(range_from_row).transpose()
    }

    async fn store_artifacts(
        &self,
        id: Uuid,
        cufe: &str,
        original_xml: &str,
        signed_xml: &str,
    ) -> Result<(), DomainError> {
        let result = sqlx::query(
            "UPDATE documents SET cufe_cude = $2, original_xml = $3, signed_xml = $4 WHERE id = $1",
        )
        .bind(id)
        .bind(cufe)
        .bind(original_xml)
        .bind(signed_xml)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound(format!("document {}", id)));
        }
        Ok(())
    }

    async fn record_event(&self, event: &DianEvent) -> Result<(), DomainError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        insert_event(&mut tx, event).await?;
        tx.commit().await.map_err(db)
    }

    async fn find_events(&self, document_id: Uuid) -> Result<Vec<DianEvent>, DomainError> {
        let rows = sqlx::query(&format!(
            "SELECT {EVENT_COLUMNS} FROM dian_events WHERE document_id = $1 ORDER BY created_at ASC"
        ))
        .bind(document_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(event_from_row).collect()
    }

    async fn apply_transition(&self, transition: Transition) -> Result<bool, DomainError> {
        let predecessors: Vec<&str> = transition
            .to
            .allowed_predecessors()
            .iter()
            .map(DocumentStatus::as_str)
            .collect();

        let mut tx = self.pool.begin().await.map_err(db)?;
        let updated = sqlx::query(
            "UPDATE documents SET status = $2::document_status
             WHERE id = $1 AND status::text = ANY($3)
             RETURNING tenant_id, document_type::text AS document_type, prefix, document_number, cufe_cude",
        )
        .bind(transition.document_id)
        .bind(transition.to.as_str())
        .bind(&predecessors)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;

        let Some(document) = updated else {
            tx.rollback().await.map_err(db)?;
            return Ok(false);
        };

        // Outbox: las notificaciones del cambio de estado se encolan en la misma transacción.
        if let Some(event_type) = event_for(transition.to) {
            let document_type: String = document.try_get("document_type").map_err(db)?;
            let prefix: String = document.try_get("prefix").map_err(db)?;
            let cufe: Option<String> = document.try_get("cufe_cude").map_err(db)?;
            let data = notification_data(&NotifiedDocument {
                document_id: transition.document_id,
                document_type: &document_type,
                prefix: &prefix,
                number: document.try_get("document_number").map_err(db)?,
                status: transition.to,
                cufe: cufe.as_deref(),
            });
            sqlx::query(
                "INSERT INTO webhook_deliveries (id, webhook_id, document_id, event_type, data, max_attempts)
                 SELECT gen_random_uuid(), w.id, $1, $2, $3, $4 FROM webhooks w
                 WHERE w.tenant_id = $5 AND w.subscription_status = 'active' AND w.event_type IN ($2, '*')",
            )
            .bind(transition.document_id)
            .bind(event_type)
            .bind(&data)
            .bind(MAX_DELIVERY_ATTEMPTS)
            .bind(document.try_get::<Uuid, _>("tenant_id").map_err(db)?)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }

        if let Some(event) = &transition.event {
            insert_event(&mut tx, event).await?;
        }
        match &transition.job {
            Some(JobOutcome::Complete(id)) => {
                sqlx::query("UPDATE jobs SET status = 'done', locked_until = NULL WHERE id = $1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
            }
            Some(JobOutcome::Bury(id, error)) => {
                sqlx::query("UPDATE jobs SET status = 'dead', locked_until = NULL, last_error = $2 WHERE id = $1")
                    .bind(id)
                    .bind(error)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
            }
            None => {}
        }
        if let Some(job) = &transition.follow_up {
            insert_job(&mut tx, job).await?;
        }

        tx.commit().await.map_err(db)?;
        Ok(true)
    }
}

#[async_trait]
impl JobRepository for PgStore {
    async fn claim(&self, worker_id: &str, limit: i64, lease: Duration) -> Result<Vec<Job>, DomainError> {
        // Un trabajo cuyo lease venció en su último intento no se vuelve a reclamar: pasa a
        // la cola de mensajes muertos para que no quede en un ciclo de caídas.
        sqlx::query(
            "UPDATE jobs SET status = 'dead', locked_until = NULL,
                    last_error = COALESCE(last_error || ' | ', '') || 'lease expired on final attempt'
             WHERE status = 'running' AND locked_until < now() AND attempts >= max_attempts",
        )
        .execute(&self.pool)
        .await
        .map_err(db)?;

        let rows = sqlx::query(
            "WITH ready AS (
                 SELECT id FROM jobs
                 WHERE ((status = 'queued' AND run_at <= now())
                        OR (status = 'running' AND locked_until < now()))
                   AND attempts < max_attempts
                 ORDER BY run_at
                 LIMIT $1
                 FOR UPDATE SKIP LOCKED
             )
             UPDATE jobs j
             SET status = 'running',
                 attempts = j.attempts + 1,
                 locked_until = now() + make_interval(secs => $2),
                 locked_by = $3
             FROM ready
             WHERE j.id = ready.id
             RETURNING j.id, j.kind::text AS kind, j.document_id, j.payload, j.attempts, j.max_attempts",
        )
        .bind(limit)
        .bind(lease.as_secs_f64())
        .bind(worker_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(job_from_row).collect()
    }

    async fn complete(&self, job_id: Uuid) -> Result<(), DomainError> {
        sqlx::query("UPDATE jobs SET status = 'done', locked_until = NULL WHERE id = $1")
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn retry(&self, job_id: Uuid, run_at: DateTime<Utc>, error: &str) -> Result<(), DomainError> {
        sqlx::query(
            "UPDATE jobs SET status = 'queued', run_at = $2, locked_until = NULL, locked_by = NULL, last_error = $3
             WHERE id = $1",
        )
        .bind(job_id)
        .bind(run_at)
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn release(&self, job_id: Uuid, run_at: DateTime<Utc>) -> Result<(), DomainError> {
        sqlx::query(
            "UPDATE jobs SET status = 'queued', run_at = $2, locked_until = NULL, locked_by = NULL,
                    attempts = GREATEST(attempts - 1, 0)
             WHERE id = $1",
        )
        .bind(job_id)
        .bind(run_at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn bury(&self, job_id: Uuid, error: &str) -> Result<(), DomainError> {
        sqlx::query("UPDATE jobs SET status = 'dead', locked_until = NULL, last_error = $2 WHERE id = $1")
            .bind(job_id)
            .bind(error)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn requeue_contingency(&self, limit: i64, max_attempts: i32) -> Result<u64, DomainError> {
        let result = sqlx::query(
            "WITH candidates AS (
                 SELECT d.id FROM documents d
                 WHERE d.status = 'contingency_pending'
                   AND NOT EXISTS (
                       SELECT 1 FROM jobs j
                       WHERE j.document_id = d.id AND j.kind = 'transmit' AND j.status IN ('queued', 'running'))
                 ORDER BY d.created_at
                 LIMIT $1
                 FOR UPDATE SKIP LOCKED
             ),
             moved AS (
                 UPDATE documents d SET status = 'pending'
                 FROM candidates c WHERE d.id = c.id
                 RETURNING d.id
             )
             INSERT INTO jobs (id, kind, document_id, max_attempts)
             SELECT gen_random_uuid(), 'transmit', id, $2 FROM moved",
        )
        .bind(limit)
        .bind(max_attempts)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(result.rows_affected())
    }

    async fn queue_depth(&self) -> Result<QueueDepth, DomainError> {
        let (ready, scheduled, running, dead): (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE status = 'queued' AND run_at <= now()),
                    count(*) FILTER (WHERE status = 'queued' AND run_at > now()),
                    count(*) FILTER (WHERE status = 'running'),
                    count(*) FILTER (WHERE status = 'dead')
             FROM jobs WHERE status IN ('queued', 'running', 'dead')",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(QueueDepth { ready, scheduled, running, dead })
    }
}

fn webhook_from_row(row: &PgRow) -> Result<Webhook, DomainError> {
    Ok(Webhook {
        id: row.try_get("id").map_err(db)?,
        tenant_id: row.try_get("tenant_id").map_err(db)?,
        target_url: row.try_get("target_url").map_err(db)?,
        webhook_secret: row.try_get("webhook_secret").map_err(db)?,
        event_type: row.try_get("event_type").map_err(db)?,
        subscription_status: parse(row, "subscription_status")?,
    })
}

#[async_trait]
impl WebhookRepository for PgStore {
    async fn create_webhook(&self, webhook: Webhook) -> Result<(), DomainError> {
        sqlx::query(
            "INSERT INTO webhooks (id, tenant_id, target_url, webhook_secret, event_type, subscription_status)
             VALUES ($1, $2, $3, $4, $5, $6::subscription_status)",
        )
        .bind(webhook.id)
        .bind(webhook.tenant_id)
        .bind(&webhook.target_url)
        .bind(&webhook.webhook_secret)
        .bind(&webhook.event_type)
        .bind(webhook.subscription_status.as_str())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn list_webhooks(&self, tenant_id: Uuid) -> Result<Vec<Webhook>, DomainError> {
        sqlx::query(
            "SELECT id, tenant_id, target_url, webhook_secret, event_type, subscription_status::text AS subscription_status
             FROM webhooks WHERE tenant_id = $1 ORDER BY id",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?
        .iter()
        .map(webhook_from_row)
        .collect()
    }

    async fn delete_webhook(&self, tenant_id: Uuid, webhook_id: Uuid) -> Result<bool, DomainError> {
        let result = sqlx::query("DELETE FROM webhooks WHERE id = $1 AND tenant_id = $2")
            .bind(webhook_id)
            .bind(tenant_id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(result.rows_affected() == 1)
    }

    async fn claim_deliveries(&self, limit: i64, lease: Duration) -> Result<Vec<WebhookDelivery>, DomainError> {
        let rows = sqlx::query(
            "WITH candidates AS (
                 SELECT id FROM webhook_deliveries
                 WHERE (status = 'queued' AND run_at <= now())
                    OR (status = 'running' AND locked_until < now())
                 ORDER BY run_at
                 LIMIT $1
                 FOR UPDATE SKIP LOCKED
             )
             UPDATE webhook_deliveries d
             SET status = 'running', attempts = d.attempts + 1,
                 locked_until = now() + make_interval(secs => $2)
             FROM candidates c, webhooks w
             WHERE d.id = c.id AND w.id = d.webhook_id
             RETURNING d.id, w.tenant_id, d.event_type, d.data, d.attempts, d.max_attempts, d.created_at,
                       w.target_url, w.webhook_secret, w.subscription_status = 'active' AS active",
        )
        .bind(limit)
        .bind(lease.as_secs_f64())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter()
            .map(|row| {
                Ok(WebhookDelivery {
                    id: row.try_get("id").map_err(db)?,
                    tenant_id: row.try_get("tenant_id").map_err(db)?,
                    event_type: row.try_get("event_type").map_err(db)?,
                    data: row.try_get("data").map_err(db)?,
                    attempts: row.try_get("attempts").map_err(db)?,
                    max_attempts: row.try_get("max_attempts").map_err(db)?,
                    created_at: row.try_get("created_at").map_err(db)?,
                    target_url: row.try_get("target_url").map_err(db)?,
                    secret: row.try_get("webhook_secret").map_err(db)?,
                    active: row.try_get("active").map_err(db)?,
                })
            })
            .collect()
    }

    async fn complete_delivery(&self, delivery_id: Uuid, status_code: u16) -> Result<(), DomainError> {
        sqlx::query(
            "UPDATE webhook_deliveries SET status = 'delivered', locked_until = NULL, last_status_code = $2
             WHERE id = $1",
        )
        .bind(delivery_id)
        .bind(i32::from(status_code))
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn retry_delivery(
        &self,
        delivery_id: Uuid,
        run_at: DateTime<Utc>,
        error: &str,
        status_code: Option<u16>,
    ) -> Result<(), DomainError> {
        sqlx::query(
            "UPDATE webhook_deliveries
             SET status = 'queued', run_at = $2, locked_until = NULL, last_error = $3, last_status_code = $4
             WHERE id = $1",
        )
        .bind(delivery_id)
        .bind(run_at)
        .bind(error)
        .bind(status_code.map(i32::from))
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn bury_delivery(&self, delivery_id: Uuid, error: &str, status_code: Option<u16>) -> Result<(), DomainError> {
        sqlx::query(
            "UPDATE webhook_deliveries
             SET status = 'dead', locked_until = NULL, last_error = $2, last_status_code = $3
             WHERE id = $1",
        )
        .bind(delivery_id)
        .bind(error)
        .bind(status_code.map(i32::from))
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }
}

async fn insert_audit(tx: &mut Transaction<'_, Postgres>, audit: &AuditEntry) -> Result<(), DomainError> {
    sqlx::query(
        "INSERT INTO admin_audit_log (id, action, tenant_id, subject_id, details) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(audit.action)
    .bind(audit.tenant_id)
    .bind(audit.subject_id)
    .bind(&audit.details)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(())
}

fn conflict_on_unique(e: sqlx::Error, message: &str) -> DomainError {
    if is_unique_violation(&e) {
        DomainError::Conflict(message.to_string())
    } else {
        db(e)
    }
}

fn api_key_from_row(row: &PgRow) -> Result<ApiKeyRecord, DomainError> {
    Ok(ApiKeyRecord {
        id: row.try_get("id").map_err(db)?,
        tenant_id: row.try_get("tenant_id").map_err(db)?,
        name: row.try_get("name").map_err(db)?,
        prefix: row.try_get("prefix").map_err(db)?,
        key_hash: row.try_get("key_hash").map_err(db)?,
        created_at: row.try_get("created_at").map_err(db)?,
        revoked_at: row.try_get("revoked_at").map_err(db)?,
    })
}

#[async_trait]
impl AdminRepository for PgStore {
    async fn create_tenant(&self, tenant: NewTenant, audit: &AuditEntry) -> Result<Tenant, DomainError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            "INSERT INTO tenants (id, tax_id, registration_name, tax_regime, reception_email)
             VALUES ($1, $2, $3, $4, $5)
             RETURNING id, tax_id, registration_name, tax_regime, address_info, reception_email, api_keys, created_at, updated_at",
        )
        .bind(tenant.id)
        .bind(&tenant.tax_id)
        .bind(&tenant.registration_name)
        .bind(&tenant.tax_regime)
        .bind(&tenant.reception_email)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| conflict_on_unique(e, &format!("Ya existe un tenant con NIT {}", tenant.tax_id)))?;
        insert_audit(&mut tx, audit).await?;
        tx.commit().await.map_err(db)?;
        tenant_from_row(&row)
    }

    async fn find_tenant(&self, tenant_id: Uuid) -> Result<Option<Tenant>, DomainError> {
        let row = sqlx::query(
            "SELECT id, tax_id, registration_name, tax_regime, address_info, reception_email, api_keys, created_at, updated_at
             FROM tenants WHERE id = $1",
        )
        .bind(tenant_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(tenant_from_row).transpose()
    }

    async fn create_api_key(&self, key: ApiKeyRecord, audit: &AuditEntry) -> Result<(), DomainError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query(
            "INSERT INTO api_keys (id, tenant_id, name, prefix, key_hash, created_at) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(key.id)
        .bind(key.tenant_id)
        .bind(&key.name)
        .bind(&key.prefix)
        .bind(&key.key_hash)
        .bind(key.created_at)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        insert_audit(&mut tx, audit).await?;
        tx.commit().await.map_err(db)
    }

    async fn list_api_keys(&self, tenant_id: Uuid) -> Result<Vec<ApiKeyRecord>, DomainError> {
        sqlx::query(
            "SELECT id, tenant_id, name, prefix, key_hash, created_at, revoked_at
             FROM api_keys WHERE tenant_id = $1 ORDER BY created_at",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?
        .iter()
        .map(api_key_from_row)
        .collect()
    }

    async fn revoke_api_key(&self, tenant_id: Uuid, key_id: Uuid, audit: &AuditEntry) -> Result<bool, DomainError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let revoked = sqlx::query(
            "UPDATE api_keys SET revoked_at = now() WHERE id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
        )
        .bind(key_id)
        .bind(tenant_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?
        .rows_affected()
            == 1;
        if revoked {
            insert_audit(&mut tx, audit).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(revoked)
    }

    async fn activate_certificate(&self, certificate: Certificate, audit: &AuditEntry) -> Result<(), DomainError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("UPDATE certificates SET status = 'revoked' WHERE tenant_id = $1 AND status = 'active'")
            .bind(certificate.tenant_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query(
            "INSERT INTO certificates (id, tenant_id, vault_reference_path, thumbprint, expiration_date, status)
             VALUES ($1, $2, $3, $4, $5, $6::certificate_status)",
        )
        .bind(certificate.id)
        .bind(certificate.tenant_id)
        .bind(&certificate.vault_reference_path)
        .bind(&certificate.thumbprint)
        .bind(certificate.expiration_date)
        .bind(certificate.status.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| conflict_on_unique(e, "El tenant ya tiene otro certificado activo"))?;
        insert_audit(&mut tx, audit).await?;
        tx.commit().await.map_err(db)
    }

    async fn add_numbering_range(&self, range: NumberingRange, audit: &AuditEntry) -> Result<(), DomainError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        // Serializa las altas de rangos del mismo tenant para que la comprobación de solape
        // no compita con otra alta concurrente.
        sqlx::query("SELECT id FROM tenants WHERE id = $1 FOR UPDATE")
            .bind(range.tenant_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?
            .ok_or_else(|| DomainError::NotFound(format!("Tenant {} no encontrado", range.tenant_id)))?;
        let inserted = sqlx::query(
            "INSERT INTO numbering_ranges (id, tenant_id, document_type, prefix, from_number, to_number, technical_key,
                                           valid_date_from, valid_date_to, current_counter)
             SELECT $1, $2, $3::document_type, $4, $5, $6, $7, $8, $9, $10
             WHERE NOT EXISTS (
                 SELECT 1 FROM numbering_ranges
                 WHERE tenant_id = $2 AND document_type = $3::document_type AND prefix = $4
                   AND valid_date_from < $9 AND $8 < valid_date_to)",
        )
        .bind(range.id)
        .bind(range.tenant_id)
        .bind(range.document_type.as_str())
        .bind(&range.prefix)
        .bind(range.from_number)
        .bind(range.to_number)
        .bind(&range.technical_key)
        .bind(range.valid_date_from)
        .bind(range.valid_date_to)
        .bind(range.current_counter)
        .execute(&mut *tx)
        .await
        .map_err(db)?
        .rows_affected();
        if inserted == 0 {
            return Err(DomainError::Conflict(
                "El rango se solapa en vigencia con otro del mismo tipo y prefijo".to_string(),
            ));
        }
        insert_audit(&mut tx, audit).await?;
        tx.commit().await.map_err(db)
    }

    async fn list_dead_jobs(&self, limit: i64) -> Result<Vec<DeadJob>, DomainError> {
        let rows = sqlx::query(
            "SELECT j.id, j.kind::text AS kind, j.document_id, d.tenant_id, j.attempts, j.last_error, j.updated_at
             FROM jobs j JOIN documents d ON d.id = j.document_id
             WHERE j.status = 'dead' ORDER BY j.updated_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter()
            .map(|row| {
                Ok(DeadJob {
                    id: row.try_get("id").map_err(db)?,
                    kind: parse(row, "kind")?,
                    document_id: row.try_get("document_id").map_err(db)?,
                    tenant_id: row.try_get("tenant_id").map_err(db)?,
                    attempts: row.try_get("attempts").map_err(db)?,
                    last_error: row.try_get("last_error").map_err(db)?,
                    failed_at: row.try_get("updated_at").map_err(db)?,
                })
            })
            .collect()
    }

    async fn retry_dead_job(&self, job_id: Uuid, audit: &AuditEntry) -> Result<bool, DomainError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let Some(row) = sqlx::query(
            "SELECT j.kind::text AS kind, j.document_id, d.status::text AS status
             FROM jobs j JOIN documents d ON d.id = j.document_id
             WHERE j.id = $1 AND j.status = 'dead'
             FOR UPDATE OF j, d",
        )
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        else {
            tx.rollback().await.map_err(db)?;
            return Ok(false);
        };
        let kind: JobKind = parse(&row, "kind")?;
        let status: DocumentStatus = parse(&row, "status")?;
        let document_id: Uuid = row.try_get("document_id").map_err(db)?;
        let allowed = match kind {
            JobKind::Transmit => matches!(status, DocumentStatus::Pending | DocumentStatus::ContingencyPending),
            JobKind::PollStatus => status == DocumentStatus::SentDian,
        };
        if !allowed {
            return Err(DomainError::Conflict(format!(
                "El documento esta en estado {}: el trabajo {} ya no aplica",
                status, kind
            )));
        }
        if kind == JobKind::Transmit && status == DocumentStatus::ContingencyPending {
            sqlx::query("UPDATE documents SET status = 'pending' WHERE id = $1")
                .bind(document_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        sqlx::query(
            "UPDATE jobs SET status = 'queued', attempts = 0, run_at = now(), locked_until = NULL, locked_by = NULL
             WHERE id = $1",
        )
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| conflict_on_unique(e, "El documento ya tiene un trabajo activo"))?;
        insert_audit(&mut tx, audit).await?;
        tx.commit().await.map_err(db)?;
        Ok(true)
    }

    async fn list_dead_deliveries(&self, limit: i64) -> Result<Vec<DeadDelivery>, DomainError> {
        let rows = sqlx::query(
            "SELECT d.id, w.tenant_id, d.webhook_id, d.event_type, d.attempts, d.last_error, d.last_status_code, d.updated_at
             FROM webhook_deliveries d JOIN webhooks w ON w.id = d.webhook_id
             WHERE d.status = 'dead' ORDER BY d.updated_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter()
            .map(|row| {
                Ok(DeadDelivery {
                    id: row.try_get("id").map_err(db)?,
                    tenant_id: row.try_get("tenant_id").map_err(db)?,
                    webhook_id: row.try_get("webhook_id").map_err(db)?,
                    event_type: row.try_get("event_type").map_err(db)?,
                    attempts: row.try_get("attempts").map_err(db)?,
                    last_error: row.try_get("last_error").map_err(db)?,
                    last_status_code: row.try_get("last_status_code").map_err(db)?,
                    failed_at: row.try_get("updated_at").map_err(db)?,
                })
            })
            .collect()
    }

    async fn retry_dead_delivery(&self, delivery_id: Uuid, audit: &AuditEntry) -> Result<bool, DomainError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let retried = sqlx::query(
            "UPDATE webhook_deliveries SET status = 'queued', attempts = 0, run_at = now(), locked_until = NULL
             WHERE id = $1 AND status = 'dead'",
        )
        .bind(delivery_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?
        .rows_affected()
            == 1;
        if retried {
            insert_audit(&mut tx, audit).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(retried)
    }
}

#[async_trait]
impl HealthProbe for PgStore {
    async fn ping(&self) -> Result<(), DomainError> {
        sqlx::query("SELECT 1").execute(&self.pool).await.map_err(db)?;
        Ok(())
    }
}

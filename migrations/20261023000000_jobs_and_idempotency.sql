-- 20261023000000_jobs_and_idempotency.sql
-- Fiabilidad del procesamiento (auditoría F2/F3):
--   * Cola persistente de trabajos en PostgreSQL, escrita en la misma transacción que el
--     documento (sustituye la cola Redis y sus reintentos en memoria).
--   * Claves de idempotencia por tenant para la emisión.
--   * Trazabilidad de la resolución de numeración usada por cada documento.

-- 1. Resolución usada por cada documento emitido (de ella sale la clave técnica del CUFE).
ALTER TABLE documents
    ADD COLUMN numbering_range_id UUID REFERENCES numbering_ranges(id) ON DELETE RESTRICT;

-- 2. Cola de trabajos.
CREATE TYPE job_kind AS ENUM ('transmit', 'poll_status');
CREATE TYPE job_status AS ENUM ('queued', 'running', 'done', 'dead');

CREATE TABLE jobs (
    id UUID PRIMARY KEY,
    kind job_kind NOT NULL,
    document_id UUID NOT NULL REFERENCES documents(id) ON DELETE RESTRICT,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    status job_status NOT NULL DEFAULT 'queued',
    attempts INT NOT NULL DEFAULT 0,
    max_attempts INT NOT NULL,
    run_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    locked_until TIMESTAMPTZ,
    locked_by VARCHAR(255),
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT chk_jobs_attempts CHECK (attempts >= 0 AND max_attempts > 0)
);

-- Trabajos listos para reclamar.
CREATE INDEX idx_jobs_ready ON jobs (run_at) WHERE status = 'queued';
-- Trabajos con lease (para recuperar los de un worker caído).
CREATE INDEX idx_jobs_running ON jobs (locked_until) WHERE status = 'running';
-- Como máximo un trabajo activo de cada tipo por documento: impide transmisiones duplicadas.
CREATE UNIQUE INDEX uq_jobs_active_per_document
    ON jobs (document_id, kind) WHERE status IN ('queued', 'running');

CREATE TRIGGER trigger_update_jobs_updated_at
BEFORE UPDATE ON jobs
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

-- 3. Idempotencia de la emisión.
CREATE TABLE idempotency_keys (
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    idempotency_key VARCHAR(255) NOT NULL,
    request_hash CHAR(64) NOT NULL,
    document_id UUID NOT NULL REFERENCES documents(id) ON DELETE RESTRICT,
    document_number INT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (tenant_id, idempotency_key)
);

-- 4. Migración desde la cola Redis: los documentos que estaban en curso reciben su trabajo.
INSERT INTO jobs (id, kind, document_id, max_attempts)
SELECT gen_random_uuid(), 'transmit', d.id, 5
FROM documents d
WHERE d.direction = 'outbound' AND d.status IN ('pending', 'contingency_pending');

UPDATE documents SET status = 'pending'
WHERE direction = 'outbound' AND status = 'contingency_pending';

INSERT INTO jobs (id, kind, document_id, payload, max_attempts)
SELECT gen_random_uuid(), 'poll_status', d.id, jsonb_build_object('zip_key', e.soap_trace_id), 30
FROM documents d
JOIN LATERAL (
    SELECT soap_trace_id
    FROM dian_events
    WHERE document_id = d.id AND event_status = 'sent' AND soap_trace_id IS NOT NULL
    ORDER BY created_at DESC
    LIMIT 1
) e ON TRUE
WHERE d.direction = 'outbound' AND d.status = 'sent_dian';

-- 20261021000000_harden_documents.sql
-- Contención de riesgos críticos (auditoría F1):
--   * Estado explícito para fallos locales de procesamiento (firma, generación XML).
--   * La clave única de documentos distingue dirección (emitido/recibido) y NIT emisor,
--     para que una factura recibida no pueda sobrescribir otra ni una factura propia.
--   * Borrar un tenant ya no elimina en cascada documentos ni eventos DIAN.

-- 1. Nuevo estado de documento.
--    (ADD VALUE no puede usarse dentro de la misma transacción que lo crea; aquí no se usa.)
ALTER TYPE document_status ADD VALUE IF NOT EXISTS 'processing_failed';

-- 2. Dirección y emisor del documento.
ALTER TABLE documents
    ADD COLUMN direction VARCHAR(10) NOT NULL DEFAULT 'outbound',
    ADD COLUMN issuer_nit VARCHAR(50) NOT NULL DEFAULT '';

ALTER TABLE documents
    ADD CONSTRAINT chk_documents_direction CHECK (direction IN ('outbound', 'inbound'));

-- Los documentos recibidos existentes se identifican por la marca del payload.
UPDATE documents
SET direction = 'inbound',
    issuer_nit = COALESCE(payload->>'sender_nit', '')
WHERE payload->>'inbound' = 'true';

ALTER TABLE documents DROP CONSTRAINT uq_tenant_document;
ALTER TABLE documents
    ADD CONSTRAINT uq_tenant_document
    UNIQUE (tenant_id, direction, issuer_nit, document_type, prefix, document_number);

-- 3. Conservación de registros fiscales: sin borrado en cascada.
ALTER TABLE documents DROP CONSTRAINT documents_tenant_id_fkey;
ALTER TABLE documents
    ADD CONSTRAINT documents_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE RESTRICT;

ALTER TABLE dian_events DROP CONSTRAINT dian_events_document_id_fkey;
ALTER TABLE dian_events
    ADD CONSTRAINT dian_events_document_id_fkey
    FOREIGN KEY (document_id) REFERENCES documents(id) ON DELETE RESTRICT;

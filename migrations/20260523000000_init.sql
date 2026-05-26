-- 20260523000000_init.sql
-- Migración inicial para el Motor de Facturación Electrónica DIAN

-- ============================================================================
-- 1. CREACIÓN DE TIPOS ENUM
-- ============================================================================

CREATE TYPE certificate_status AS ENUM (
    'active',
    'expired',
    'revoked'
);

CREATE TYPE document_type AS ENUM (
    'invoice',
    'credit_note',
    'debit_note',
    'support_document',
    'payroll'
);

CREATE TYPE document_status AS ENUM (
    'draft',
    'pending',
    'sent_dian',
    'dian_accepted',
    'dian_rejected',
    'contingency_pending',
    'contingency_sent'
);

CREATE TYPE event_status AS ENUM (
    'sent',
    'accepted',
    'rejected',
    'error'
);

CREATE TYPE subscription_status AS ENUM (
    'active',
    'inactive'
);

-- ============================================================================
-- 2. CREACIÓN DE TABLAS
-- ============================================================================

-- Tabla: tenants
CREATE TABLE tenants (
    id UUID PRIMARY KEY,
    tax_id VARCHAR(50) NOT NULL UNIQUE,
    registration_name VARCHAR(255) NOT NULL,
    tax_regime VARCHAR(100) NOT NULL,
    address_info JSONB NOT NULL DEFAULT '{}'::jsonb,
    reception_email VARCHAR(255) NOT NULL,
    api_keys JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Tabla: certificates
CREATE TABLE certificates (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    vault_reference_path VARCHAR(500) NOT NULL,
    thumbprint VARCHAR(255) NOT NULL,
    expiration_date TIMESTAMPTZ NOT NULL,
    status certificate_status NOT NULL DEFAULT 'active'
);

-- Tabla: numbering_ranges
CREATE TABLE numbering_ranges (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    document_type document_type NOT NULL,
    prefix VARCHAR(50) NOT NULL,
    from_number INT NOT NULL,
    to_number INT NOT NULL,
    technical_key VARCHAR(255) NOT NULL,
    valid_date_from TIMESTAMPTZ NOT NULL,
    valid_date_to TIMESTAMPTZ NOT NULL,
    current_counter INT NOT NULL DEFAULT 0,
    CONSTRAINT chk_range_limits CHECK (from_number <= to_number),
    CONSTRAINT chk_counter CHECK (current_counter >= 0)
);

-- Tabla: documents
CREATE TABLE documents (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    document_type document_type NOT NULL,
    prefix VARCHAR(50) NOT NULL,
    document_number INT NOT NULL,
    cufe_cude VARCHAR(255) NOT NULL,
    payload JSONB NOT NULL,
    original_xml TEXT,
    signed_xml TEXT,
    pdf_url VARCHAR(1024),
    status document_status NOT NULL DEFAULT 'draft',
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT uq_tenant_document UNIQUE (tenant_id, document_type, prefix, document_number)
);

-- Tabla: dian_events
CREATE TABLE dian_events (
    id UUID PRIMARY KEY,
    document_id UUID NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    event_status event_status NOT NULL,
    dian_response_code VARCHAR(50),
    dian_response_message TEXT,
    soap_trace_id VARCHAR(255),
    xml_response TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Tabla: webhooks
CREATE TABLE webhooks (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    target_url VARCHAR(1024) NOT NULL,
    webhook_secret VARCHAR(255) NOT NULL,
    event_type VARCHAR(100) NOT NULL,
    subscription_status subscription_status NOT NULL DEFAULT 'active'
);

-- ============================================================================
-- 3. CREACIÓN DE ÍNDICES OPTIMIZADOS
-- ============================================================================

-- Búsquedas por CUFE/CUDE (Operación de reconciliación y consultas del ERP)
CREATE INDEX idx_documents_cufe_cude ON documents (cufe_cude);

-- Búsqueda de documentos por tenant y estado (Worker de Redis/Trazabilidad)
CREATE INDEX idx_documents_tenant_status ON documents (tenant_id, status);

-- Búsqueda de certificados activos por tenant (Firma criptográfica)
CREATE INDEX idx_certificates_tenant_status ON certificates (tenant_id, status);

-- Búsqueda de rangos de numeración por tenant y tipo de documento
CREATE INDEX idx_numbering_ranges_lookup ON numbering_ranges (tenant_id, document_type);

-- Trazabilidad de eventos de la DIAN por documento
CREATE INDEX idx_dian_events_doc_id ON dian_events (document_id);

-- Despacho de webhooks por tenant
CREATE INDEX idx_webhooks_tenant ON webhooks (tenant_id);

-- ============================================================================
-- 4. TRIGGERS PARA ACTUALIZAR TIMESTAMPS
-- ============================================================================

CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_tenants_updated_at
BEFORE UPDATE ON tenants
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

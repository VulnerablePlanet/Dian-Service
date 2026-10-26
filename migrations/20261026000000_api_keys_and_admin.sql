-- API keys con hash y soporte para la administración.

-- 1. API keys: solo se guarda el SHA-256 (hex) de la clave; la clave se muestra una vez.
CREATE TABLE api_keys (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    name VARCHAR(100) NOT NULL,
    -- Inicio visible de la clave, para identificarla sin revelarla.
    prefix VARCHAR(20) NOT NULL,
    key_hash CHAR(64) NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    revoked_at TIMESTAMPTZ
);
CREATE INDEX idx_api_keys_tenant ON api_keys (tenant_id);

-- Migra las claves heredadas (texto claro en tenants.api_keys->>'secret'). Una clave que
-- comparten varios tenants no identifica a ninguno: no se migra y esos tenants necesitan
-- una clave nueva (POST /admin/v1/tenants/:id/api-keys).
WITH legacy AS (
    SELECT id, api_keys->>'secret' AS secret
    FROM tenants
    WHERE COALESCE(api_keys->>'secret', '') <> ''
),
unique_secrets AS (
    SELECT secret FROM legacy GROUP BY secret HAVING count(*) = 1
)
INSERT INTO api_keys (id, tenant_id, name, prefix, key_hash)
SELECT gen_random_uuid(), legacy.id, 'legacy', 'legacy', encode(sha256(convert_to(legacy.secret, 'UTF8')), 'hex')
FROM legacy JOIN unique_secrets USING (secret);

-- Ningún secreto queda en claro.
UPDATE tenants SET api_keys = '{}'::jsonb WHERE api_keys <> '{}'::jsonb;

-- 2. Un único certificado activo por tenant. Se conserva el que ya se usaba para firmar
-- (el de vencimiento más lejano); los demás activos pasan a revocados.
UPDATE certificates c SET status = 'revoked'
WHERE status = 'active'
  AND id <> (
      SELECT c2.id FROM certificates c2
      WHERE c2.tenant_id = c.tenant_id AND c2.status = 'active'
      ORDER BY c2.expiration_date DESC, c2.id
      LIMIT 1
  );
CREATE UNIQUE INDEX uq_certificates_active ON certificates (tenant_id) WHERE status = 'active';

-- 3. Bitácora de acciones administrativas (solo inserciones).
CREATE TABLE admin_audit_log (
    id UUID PRIMARY KEY,
    action VARCHAR(100) NOT NULL,
    tenant_id UUID,
    subject_id UUID,
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_admin_audit_log_tenant ON admin_audit_log (tenant_id, created_at);

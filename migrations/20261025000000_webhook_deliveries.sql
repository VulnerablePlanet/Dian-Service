-- Entregas de webhooks: cola persistente de notificaciones a los sistemas de los tenants.
-- Las filas se crean en la misma transacción que el cambio de estado del documento que
-- notifican (outbox), así que ninguna notificación se pierde ni se envía sin ese cambio.

CREATE TYPE delivery_status AS ENUM ('queued', 'running', 'delivered', 'dead');

CREATE TABLE webhook_deliveries (
    id UUID PRIMARY KEY,
    webhook_id UUID NOT NULL REFERENCES webhooks(id) ON DELETE CASCADE,
    document_id UUID NOT NULL REFERENCES documents(id) ON DELETE RESTRICT,
    event_type VARCHAR(100) NOT NULL,
    data JSONB NOT NULL,
    status delivery_status NOT NULL DEFAULT 'queued',
    attempts INT NOT NULL DEFAULT 0,
    max_attempts INT NOT NULL,
    run_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    locked_until TIMESTAMPTZ,
    last_error TEXT,
    last_status_code INT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT chk_delivery_attempts CHECK (attempts >= 0 AND max_attempts > 0)
);

CREATE INDEX idx_webhook_deliveries_ready ON webhook_deliveries (run_at) WHERE status = 'queued';
CREATE INDEX idx_webhook_deliveries_running ON webhook_deliveries (locked_until) WHERE status = 'running';
CREATE INDEX idx_webhook_deliveries_dead ON webhook_deliveries (updated_at) WHERE status = 'dead';
CREATE INDEX idx_webhook_deliveries_webhook ON webhook_deliveries (webhook_id);

CREATE TRIGGER update_webhook_deliveries_updated_at
    BEFORE UPDATE ON webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- Eventos admitidos. NOT VALID: no se revalidan suscripciones creadas antes (nunca se
-- despacharon), pero toda fila nueva o modificada debe cumplirlo.
ALTER TABLE webhooks
    ADD CONSTRAINT chk_webhook_event_type CHECK (event_type IN (
        '*', 'document.accepted', 'document.rejected', 'document.contingency', 'document.failed'
    )) NOT VALID;

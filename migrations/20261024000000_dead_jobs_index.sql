-- Índice parcial de la cola de errores (trabajos que agotaron sus intentos): la métrica de
-- profundidad de cola y la administración de trabajos muertos los consultan sin recorrer
-- el histórico de trabajos terminados.
CREATE INDEX idx_jobs_dead ON jobs (updated_at) WHERE status = 'dead';

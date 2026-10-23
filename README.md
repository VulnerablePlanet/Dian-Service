# Dian_Service - Motor de Cumplimiento Tributario Open-Source para la DIAN

Dian_Service es un motor de facturación electrónica y cumplimiento tributario para Colombia (DIAN) bajo el estándar **UBL 2.1**, desarrollado en **Rust** siguiendo los principios de la **Arquitectura Hexagonal**.

Este proyecto está diseñado para funcionar como un microservicio independiente (Open Core) que cualquier software, ERP o POS (independientemente del lenguaje de programación: Laravel, Django, .NET, Node.js) puede integrar para delegar la firma criptográfica XAdES-EPES, el cálculo de CUFE/CUDE, la comunicación SOAP y el cumplimiento normativo exigido por la DIAN (Resolución 165 de 2023, 227 de 2025 y concordantes).

---

## 1. Arquitectura Hexagonal y Estructura

El repositorio es un **Cargo Workspace**. Las dependencias apuntan hacia el dominio: los binarios solo traducen HTTP o la cola a casos de uso, y la infraestructura implementa los puertos del dominio.

```
api-server ─┐                 ┌─► infra-postgres   (puertos de persistencia y cola)
            ├─► application ──┤─► crypto-signer    (puerto CryptoSigner)
worker ─────┘        │        └─► dian-soap-client (puerto DianSoapClient)
                     ▼
               core-domain  ◄── ubl-engine
```

*   **`core-domain`**: modelos, reglas de negocio puras y puertos (traits). No depende de base de datos ni de HTTP. Contiene la máquina de estados del documento (`lifecycle`), las reglas de numeración (`numbering`) y los montos en `Decimal` (`money`).
*   **`application`**: casos de uso: emisión (`IssueService`, con idempotencia), procesamiento de la cola (`DocumentProcessor`: firma, transmisión, consulta de estado, contingencia), facturas recibidas y eventos RADIAN. También el circuit breaker por ambiente y el backoff.
*   **`infra-postgres`**: adaptador PostgreSQL de los puertos: emisión atómica, transiciones de estado condicionales y la cola de trabajos (`FOR UPDATE SKIP LOCKED`, leases). Embebe las migraciones.
*   **`ubl-engine`**: plantillas XML UBL 2.1 (MiniJinja, con autoescape), cálculo de CUFE/CUDE/CUDS/CUNE y empaquetado ZIP.
*   **`crypto-signer`**: canonicalización C14N (inclusiva y exclusiva), firma **XAdES-EPES** (RSA-SHA256) y WS-Security, verificación de firmas con `xmlsec1`; firma con Vault Transit por tenant o con un par local para desarrollo.
*   **`dian-soap-client`**: cliente SOAP 1.2 (WS-Addressing, WS-Security firmado con el certificado del tenant, timeouts) para los Web Services de la DIAN.
*   **`api-server`**: servidor REST (Axum) con autenticación por tenant.
*   **`worker`**: proceso que reclama y procesa los trabajos de la cola.
*   **`observability`**: logs JSON, métricas Prometheus y sondas `/livez`/`/readyz` comunes a ambos binarios.

---

## 2. Requisitos Previos

Asegúrate de contar con los siguientes elementos instalados en tu entorno:

*   **Rust Compiler** (Edición 2021 o superior)
*   **PostgreSQL 13 o superior** (datos y cola de trabajos; ya no se requiere Redis)

---

## 3. Instalación y Configuración rápida

### Paso 1: Configurar la Base de Datos
Crea una base de datos en tu servidor PostgreSQL y aplica **todas** las migraciones de `migrations/` en orden de nombre:

```bash
for f in $(ls migrations/*.sql | sort); do psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f "$f"; done
```

*   `20260523000000_init.sql` crea las tablas (`tenants`, `certificates`, `numbering_ranges`, `documents`, `dian_events`, `webhooks`) y sus índices.
*   `20261021000000_harden_documents.sql` añade el estado `processing_failed`, distingue documentos emitidos y recibidos (`direction`, `issuer_nit`) en la clave única, y cambia el borrado en cascada de documentos y eventos por `ON DELETE RESTRICT`.
*   `20261023000000_jobs_and_idempotency.sql` crea la cola de trabajos (`jobs`) y las claves de idempotencia (`idempotency_keys`), y asocia cada documento a su resolución de numeración. Si actualizas desde la versión con Redis, encola automáticamente los documentos que estaban en curso.
*   `20261024000000_dead_jobs_index.sql` indexa la cola de errores (trabajos `dead`) para las métricas y la administración.
*   `20261025000000_webhook_deliveries.sql` crea la cola de entregas de webhooks (`webhook_deliveries`) y restringe los tipos de evento suscribibles.

### Paso 2 (producción): llave de firma del tenant en Vault Transit
La política del token debe permitir `read` en `<kv>/data/<ruta del tenant>` y `update` en `<transit>/sign/<llave del tenant>`; nada más. Para cada tenant:

1. Importa su llave RSA (la del certificado ONAC) como llave no exportable: `vault write transit/keys/<llave>/import type=rsa-2048 ciphertext=...` (BYOK, ver la documentación de Vault sobre *Transit key import*), o genera la llave en Transit y solicita el certificado a partir de ella.
2. Guarda el certificado y el nombre de la llave: `vault kv put secret/tenants/<tenant> certificate=@cert.pem transit_key=<llave>`.
3. Registra la fila en `certificates` con `vault_reference_path = 'tenants/<tenant>'`.

### Paso 3: Configurar las Variables de Entorno
Los binarios leen la configuración del **entorno del proceso** (no cargan un archivo `.env` por sí mismos; usa tu orquestador, `env $(cat .env | xargs) cargo run ...` o similar). Las variables obligatorias no tienen valor por defecto: si falta alguna, el proceso termina al arrancar con un mensaje explícito.

```env
# --- Obligatorias (api-server y worker) ---
DATABASE_URL=postgres://tu_usuario:tu_contraseña@127.0.0.1:5432/dian

# --- Firma (XAdES de documentos y WS-Security del SOAP): elige UNA opción (obligatorio) ---
# El mismo certificado del tenant firma sus documentos y la cabecera WS-Security de cada
# llamada a la DIAN (BinarySecurityToken + firma del Timestamp y wsa:To).
# a) Producción: certificado por tenant en HashiCorp Vault. Cada tenant necesita una fila activa
#    y vigente en `certificates`; su `vault_reference_path` es la ruta de un secreto KV v2.
#    - VAULT_SIGNING_MODE=transit (por defecto): la llave RSA vive en Vault Transit (no
#      exportable) y firma Vault; el secreto KV contiene `certificate` (PEM) y `transit_key`
#      (nombre de la llave en Transit). La llave privada nunca llega a este proceso.
#    - VAULT_SIGNING_MODE=kv (legado): el secreto KV contiene `certificate` y `private_key`;
#      la llave se descarga y se firma en el proceso. Solo para migrar instalaciones existentes.
VAULT_ADDR=https://vault.example.com
VAULT_TOKEN=...
# VAULT_SIGNING_MODE=transit
# VAULT_KV_MOUNT=secret
# VAULT_TRANSIT_MOUNT=transit
# b) Desarrollo: un único par local compartido por todos los tenants.
# SIGNER_CERT_PATH=/ruta/cert.pem
# SIGNER_KEY_PATH=/ruta/key.pem

# --- Opcionales ---
PORT=8080
RUST_LOG=info
LOG_FORMAT=json                          # json (por defecto) o text
METRICS_ADDR=0.0.0.0:9090                # métricas (api-server: 9090, worker: 9091); off las desactiva
DATABASE_MAX_CONNECTIONS=10
# Worker
MAX_CONCURRENT_JOBS=5                    # trabajos procesados en paralelo
TRANSMIT_MAX_ATTEMPTS=5                  # intentos antes de pasar a contingencia (api-server y worker)
POLL_MAX_ATTEMPTS=30                     # consultas de estado de un envío asíncrono
JOB_LEASE_SECS=600                       # tras este tiempo, un trabajo de un worker caído se retoma
JOB_TIMEOUT_SECS=540                     # debe ser menor que JOB_LEASE_SECS
CONTINGENCY_REQUEUE_INTERVAL_SECS=300    # reproceso automático de documentos en contingencia
BREAKER_FAILURE_THRESHOLD=3
BREAKER_COOLDOWN_SECS=30
SHUTDOWN_GRACE_SECS=120
POLL_INTERVAL_MS=1000
# Endpoints SOAP alternativos (por defecto, los oficiales de la DIAN)
# DIAN_HAB_URL=https://vpfe-hab.dian.gov.co/WcfDianCustomerServices.svc
# DIAN_PROD_URL=https://vpfe.dian.gov.co/WcfDianCustomerServices.svc
# Facturas recibidas: bundle PEM de CAs de confianza (CAs acreditadas ONAC + CA de la DIAN).
# Habilita la verificación criptográfica (requiere `xmlsec1` instalado; se comprueba al arrancar).
# INBOUND_TRUSTED_CA_PEM=/etc/dian/trusted-cas.pem
# INBOUND_INTERMEDIATES_PEM=/etc/dian/intermediates.pem
# INBOUND_DIAN_SIGNER_SUBJECT=800197268   # texto del sujeto del certificado de la DIAN
# Solo para pruebas y sin bundle: registra facturas recibidas SIN verificar su firma
# INBOUND_ALLOW_UNVERIFIED_SIGNATURES=false
```

Si la firma de un documento falla (por ejemplo, el tenant no tiene un certificado activo y vigente), el documento pasa a `processing_failed` y el motivo queda en `dian_events`.

### Paso 4: Compilar y Ejecutar

El proyecto se divide en dos binarios independientes:

1.  **Ejecutar la API REST (api-server)**:
    ```bash
    cargo run --bin api-server
    ```
2.  **Ejecutar el worker** (se pueden levantar varias instancias; la cola reparte los trabajos sin duplicarlos):
    ```bash
    cargo run --bin worker
    ```

---

## 4. Guía de Integración para Desarrolladores (Cómo usar la API)

Toda interacción con `Dian_Service` está protegida por un middleware multi-tenant. Debes registrar primero tu Tenant y configurar la cabecera HTTP en cada request:

`Authorization: Bearer <TU_API_KEY_REGISTRADA>`

---

### A. Emisión de Facturas (Outbound)

Cuando tu ERP o POS genera una venta, debes enviar la información estructurada a la API de `Dian_Service`.

*   **Endpoint:** `POST /api/v1/invoices`
*   **Payload JSON de ejemplo:**

```json
{
  "prefix": "FE",
  "number": 1002,
  "issue_date": "2026-05-23",
  "issue_time": "10:42:00-05:00",
  "company_nit": "900123456",
  "company_name": "Tu Empresa SAS",
  "customer_id_type": "31",
  "customer_id": "900999888",
  "customer_name": "Adquirente SAS",
  "customer_email": "facturas@adquirente.com",
  "net_amount": 100000.00,
  "tax_amount": 19000.00,
  "total_amount": 119000.00,
  "environment": "2",
  "document_type": "invoice",
  "test_set_id": null
}
```

*   **Reglas de validación:**
    *   `number` debe ser **exactamente el siguiente consecutivo** de la resolución vigente del prefijo (no se admiten saltos). La resolución debe estar vigente y no agotada.
    *   Los montos admiten como máximo dos decimales y deben cuadrar: `total_amount = net_amount + tax_amount`.
    *   La clave técnica del CUFE se toma de la resolución de numeración (`numbering_ranges.technical_key`). El campo `technical_key` del cuerpo es obsoleto: se acepta, se ignora y no se guarda.
*   **Idempotencia (recomendado):** envía la cabecera `Idempotency-Key: <identificador único de la venta>`. Si el ERP reintenta con la misma clave y el mismo cuerpo, recibe la misma respuesta (con la cabecera `Idempotent-Replayed: true`) sin crear otro documento ni consumir otro consecutivo. La misma clave con un cuerpo distinto responde `422`.
*   **Respuesta de la API (HTTP 202 Accepted):**

En una sola transacción de PostgreSQL se asigna el consecutivo, se guarda el documento en estado `pending` y se encola su trabajo de transmisión; después se responde al ERP:

```json
{
  "document_id": "fe9b0d4e-b5c1-4b2a-8cfa-d0ad28c46001",
  "document_number": 1002,
  "status": "QUEUED",
  "message": "Factura encolada correctamente para su firmado y transmision"
}
```

*   **Códigos de error:** `401` API key inválida · `403` el NIT emisor no es el del tenant · `404` documento inexistente · `409` conflicto · `422` validación · `501` funcionalidad deshabilitada · `502` error de la DIAN · `503` base de datos no disponible (reintentable). Los errores `5xx` no incluyen detalles internos.

Posteriormente, el worker asíncrono se encarga del cálculo de CUFE, firmado XAdES-EPES, empaquetado ZIP y transmisión a la DIAN.

*   **Líneas de detalle (recomendado):** `lines` es una lista de `{ "description", "quantity", "unit_price", "tax_percent", "unit_code"? }`. Cada línea se calcula en decimal (`quantity × unit_price`, IVA por línea, redondeo a 2 decimales "half away from zero") y la suma debe coincidir exactamente con `net_amount` y `tax_amount`; el IVA del documento se agrupa por tarifa. Si se omite `lines`, se genera una única línea equivalente a los totales (compatibilidad).

#### Notas crédito y débito

El mismo endpoint emite `CreditNote` y `DebitNote` (UBL 2.1) con `document_type` `"credit_note"` o `"debit_note"`. Además de los campos de la factura requieren:

```json
{
  "billing_reference": { "number": "FE1002", "cufe": "<CUFE de 96 caracteres>", "issue_date": "2026-05-23" },
  "discrepancy": { "code": "2", "description": "Anulación de factura" },
  "software_pin": "<PIN del software DIAN>"
}
```

*   `discrepancy.code`: concepto de corrección, `1`-`6` en notas crédito y `1`-`4` en notas débito.
*   El CUDE se calcula con el `software_pin` (no con la clave técnica) y el XML incluye `DiscrepancyResponse` y `BillingReference`.
*   La numeración de cada tipo de nota se controla con su propio rango en `numbering_ranges` (`document_type` `credit_note`/`debit_note`, prefijo de la nota); el `technical_key` de ese rango no interviene.

> **Validación:** el XML generado de facturas, notas, documento soporte, ApplicationResponse y AttachedDocument se valida en las pruebas contra los esquemas oficiales OASIS UBL 2.1 (`xmllint`). Las reglas propias del Anexo Técnico de la DIAN (extensiones `sts:DianExtensions`, listas de códigos) no están cubiertas por esa validación.

---

### B. Notificaciones de Estado mediante Webhooks

En lugar de consultar el estado de forma continua (*polling*), registra una URL y `Dian_Service` te enviará un `POST` cuando un documento llegue a un estado final.

*   **Gestión (con API key):**
    *   `POST /api/v1/webhooks` con `{ "target_url": "https://erp.example.com/hooks/dian", "event_type": "*" }` → `201` con el `secret` de firma (**solo se muestra en esta respuesta**).
    *   `GET /api/v1/webhooks` lista las suscripciones (sin secretos). `DELETE /api/v1/webhooks/:id` borra una (y sus entregas pendientes).
    *   `event_type`: `*` o uno de `document.accepted`, `document.rejected`, `document.contingency`, `document.failed`. Máximo 10 suscripciones por tenant.
*   **Entrega:** las notificaciones se encolan en la misma transacción que el cambio de estado (no se pierden ni se adelantan) y las envía el worker. Cualquier respuesta distinta de `2xx`, un timeout (10 s) o un error de red se reintenta con espera exponencial (de 30 s hasta 1 h) hasta 10 intentos; después la entrega queda en estado `dead`.
*   **Cuerpo:**
```json
{
  "id": "6c1f...",
  "event": "document.accepted",
  "created_at": "2026-10-23T15:04:05Z",
  "data": { "document_id": "fe9b...", "document_type": "invoice", "prefix": "FE", "number": 1002, "status": "dian_accepted", "cufe": "..." }
}
```
*   **Verificación (obligatoria en tu ERP):** cada petición trae `X-Dian-Webhook-Id`, `X-Dian-Webhook-Event`, `X-Dian-Webhook-Timestamp` (segundos Unix) y `X-Dian-Webhook-Signature: v1=<hex>`, donde `<hex>` es `HMAC-SHA256(secret, "{timestamp}.{cuerpo}")` sobre el cuerpo tal como llega. Rechaza firmas que no coincidan (compáralas en tiempo constante) y timestamps con más de 5 minutos de diferencia; usa `id` para ignorar reintentos ya procesados.
*   **Destinos permitidos (SSRF):** solo `https://` a hosts con direcciones públicas. La URL se valida al registrarla y el host se vuelve a resolver en cada envío: se rechaza si alguna dirección es privada, loopback, de enlace local (incluida `169.254.169.254`) o reservada, y la conexión se fija a la dirección verificada, sin proxy ni redirecciones. Solo para desarrollo: `WEBHOOK_ALLOW_HTTP=true` y `WEBHOOK_ALLOW_PRIVATE_NETWORKS=true` (en api-server y worker).
*   **Pendiente:** el `secret` se guarda en claro en `webhooks.webhook_secret` (es necesario para firmar); protégelo con el cifrado en reposo de la base de datos.

---

### C. Recepción de Facturas de Proveedores (Inbound)

Para registrar las facturas que te emiten tus proveedores y habilitar el RADIAN, debes alimentar la API con el AttachedDocument ZIP recibido.

*   **Endpoint:** `POST /api/v1/inbound/invoices`
*   **Payload JSON:**
```json
{
  "zip_content": "<CONTENIDO_DEL_ARCHIVO_ZIP_DEL_PROVEEDOR_EN_BASE64>"
}
```

> **Requiere `INBOUND_TRUSTED_CA_PEM`.** Sin bundle de CAs configurado el endpoint responde `501 Not Implemented` (o, solo en pruebas con `INBOUND_ALLOW_UNVERIFIED_SIGNATURES=true`, registra el documento como **no verificado**, sin derecho a eventos RADIAN).

*   **Qué hace Dian_Service internamente (verificación configurada):**
    1.  Descomprime el ZIP en memoria, con un límite de 10 MiB descomprimidos y 20 entradas.
    2.  Extrae con un parser XML (sin DTD) la factura embebida y el `ApplicationResponse` de la DIAN.
    3.  Verifica con `xmlsec1` tres firmas: la del `AttachedDocument`, la de la factura y la del `ApplicationResponse`. Cada una debe ser la única firma del documento, cubrirlo completo (`URI=""` + transformación enveloped) y estar emitida por una CA del bundle. Nunca se descargan referencias externas.
    4.  Exige que el `ApplicationResponse` lo firme la DIAN (el sujeto del certificado debe contener `INBOUND_DIAN_SIGNER_SUBJECT`, por defecto su NIT `800197268`), que su `ResponseCode` sea `02` y que se refiera al CUFE de esa factura.
    5.  Toma número, CUFE, emisor y adquirente **de la factura verificada** (no del contenedor) y exige que el adquirente sea el tenant autenticado (`403` si no).
    6.  Inserta el documento como recibido (`direction = inbound`, `issuer_nit` = NIT del emisor, `signature_verified: true`). Si esa factura del mismo emisor ya estaba registrada, responde `409 Conflict`; nunca sobrescribe otro documento.

*   Una firma inválida, no confiable o inconsistente responde `422` con el motivo.
*   **Pendiente de confirmar con documentos reales de la DIAN:** el texto que identifica el certificado de la DIAN en su `ApplicationResponse` y el bundle exacto de CAs acreditadas ante la ONAC. La validez del certificado se evalúa en el momento de la verificación (no en la fecha de firma).

---

### D. Registro de Eventos RADIAN (Título Valor)

Una vez ingresada la factura del proveedor (inbound), puedes formalizar eventos legales ante el RADIAN para su circulación.

> Solo se admiten eventos sobre facturas recibidas cuya firma fue verificada (sección C); sobre las registradas sin verificar responde `422`.

*   **Endpoint:** `POST /api/v1/documents/:id/events`
*   **Payload JSON:**
```json
{
  "event_code": "030",
  "software_pin": "tu_pin_de_software_dian",
  "environment": "2"
}
```

*   **Códigos de Eventos Soportados:**
    *   `030`: Acuse de recibo de Factura Electrónica de Venta.
    *   `032`: Recibo de las mercancías o servicios.
    *   `033`: Aceptación expresa.
    *   `034`: Aceptación tácita.

El sistema generará la ApplicationResponse (UBL 2.1), calculará el hash CUDE (SHA-384), firmará criptográficamente con XAdES-EPES, generará el ZIP y lo transmitirá en caliente de forma síncrona a la DIAN mediante la acción SOAP `SendEventUpdateStatus`.

---

### E. Emisión de Nómina Electrónica

Envía reportes de nómina individual a la DIAN.

*   **Endpoint:** `POST /api/v1/payroll`
*   **Payload JSON:**
```json
{
  "prefix": "NOM",
  "number": 101,
  "issue_date": "2026-05-26",
  "issue_time": "08:00:00",
  "employer_nit": "900123456",
  "employer_name": "Tu Empresa SAS",
  "employee_id_type": "13",
  "employee_id": "10203040",
  "employee_name": "Trabajador Ejemplar",
  "devengado": 1500000.00,
  "deducido": 60000.00,
  "total": 1440000.00,
  "software_pin": "pin_test",
  "environment": "2"
}
```

*   **Respuesta (HTTP 202 Accepted):** Retorna `document_id` y `document_number`; el worker calcula el CUNE (SHA-384), firma y transmite en segundo plano. Al igual que en facturas, `number` debe ser el siguiente consecutivo, `total` debe ser `devengado - deducido` y se admite `Idempotency-Key`.

---

### F. Emisión de Documento Soporte

Registra transacciones de compras realizadas a sujetos no obligados a facturar.

*   **Endpoint:** `POST /api/v1/support-documents`
*   **Payload JSON:**
```json
{
  "prefix": "DS",
  "number": 0,
  "issue_date": "2026-05-26",
  "issue_time": "10:30:00-05:00",
  "seller_nit": "10203040",
  "seller_name": "Proveedor No Obligado",
  "buyer_nit": "900123456",
  "buyer_name": "Tu Empresa SAS",
  "net_amount": 100000.00,
  "tax_amount": 19000.00,
  "total_amount": 119000.00,
  "software_pin": "pin_test",
  "environment": "2"
}
```

*   **Respuesta (HTTP 202 Accepted):** El servidor **asigna** el consecutivo (el `number` del cuerpo se ignora; el primer documento de un rango que empieza en 5000 recibe 5000) y responde:
```json
{
  "document_id": "673f4e2b-f119-4a94-87cf-45de09bc8720",
  "document_number": 200,
  "status": "QUEUED",
  "message": "Documento soporte reservado y encolado correctamente"
}
```

---

### G. Monitoreo, sondas y métricas

*   **Sondas (sin API key):**
    *   `GET /livez`: `200` mientras el proceso responde (no consulta dependencias).
    *   `GET /readyz`: `200` si PostgreSQL responde, `503` si no.
    *   `GET /api/v1/health`: compatibilidad (`{"status": "UP"|"DOWN"}`, `200`/`503`).
    *   El worker expone `/livez`, `/readyz` y `/metrics` en `METRICS_ADDR` (por defecto `0.0.0.0:9091`).
*   **Métricas Prometheus (formato OpenMetrics):** la API las sirve en un listener aparte, `METRICS_ADDR` (por defecto `0.0.0.0:9090`; `off` lo desactiva), nunca en el puerto público.
    *   API: `dian_api_http_requests_total{method,route,status}` y `dian_api_http_request_duration_seconds{method,route}`; `route` es la plantilla (`/api/v1/documents/:id`), no la URL.
    *   Worker: `dian_worker_webhook_deliveries_total{outcome}` (`delivered`, `retried`, `dead`), `dian_worker_jobs_total{kind,outcome}` (`handled`, `error`, `timeout`), `dian_worker_job_duration_seconds{kind}`, `dian_worker_jobs_in_flight`, `dian_worker_queue_jobs{state}` (`ready`, `scheduled`, `running`, `dead`) y `dian_worker_dian_breaker_open{environment}`.
    *   Alertas sugeridas: `dian_worker_queue_jobs{state="dead"} > 0` (documentos en contingencia que requieren intervención), `dian_worker_queue_jobs{state="ready"}` creciendo sostenidamente (backlog), `dian_worker_dian_breaker_open == 1` durante más de unos minutos, y tasa de `5xx` en la API.
*   **Logs:** una línea JSON por evento (`LOG_FORMAT=text` para formato legible; nivel con `RUST_LOG`). Cada petición HTTP registra método, ruta, estado, latencia, `request_id` y `tenant_id`; la cabecera `x-request-id` del cliente se propaga (o se genera) y se devuelve en la respuesta. Cada trabajo del worker registra `job_id`, `document_id`, `kind` y `attempt`.

---

## 5. Numeración, cola de trabajos y resiliencia

1.  **Emisión atómica.** Asignar el consecutivo (bloqueo `SELECT ... FOR UPDATE` corto sobre la resolución vigente), insertar el documento, encolar su trabajo y registrar la clave de idempotencia ocurren en **una sola transacción**. No puede existir un documento sin trabajo ni un consecutivo consumido por una solicitud que falló.
2.  **Cola persistente en PostgreSQL (tabla `jobs`).** Los workers reclaman trabajos con `FOR UPDATE SKIP LOCKED`, de modo que varias instancias se reparten la carga sin procesar dos veces el mismo trabajo. Un índice único impide que un documento tenga dos trabajos activos del mismo tipo.
3.  **Leases.** Cada trabajo reclamado tiene un lease (`JOB_LEASE_SECS`). Si un worker muere a mitad de un envío, otro retoma el trabajo cuando el lease vence. Los reintentos se programan en la base de datos (`run_at`) con backoff exponencial y jitter, así que sobreviven a reinicios.
4.  **Sin reenvíos duplicados.** El XML firmado se guarda tras el primer intento y se reutiliza. Antes de reenviar en un reintento se consulta `GetStatus` con el CUFE: si la DIAN ya aceptó el documento, se marca como aceptado sin volver a transmitirlo.
5.  **Máquina de estados.** Cada cambio de estado se aplica solo si el estado actual lo permite, junto con su evento y el cierre del trabajo, en la misma transacción. Así, por ejemplo, un documento ya enviado no puede retroceder a contingencia.
6.  **Contingencia.** Si un documento agota `TRANSMIT_MAX_ATTEMPTS`, pasa a `contingency_pending` y su trabajo queda en estado `dead` para revisión. El worker vuelve a encolar periódicamente esos documentos (`CONTINGENCY_REQUEUE_INTERVAL_SECS`).
7.  **Circuit breaker por ambiente.** Habilitación y producción tienen breakers independientes. Mientras un breaker está abierto, los trabajos se aplazan sin consumir intentos. En estado medio abierto solo pasa una petición de prueba.
8.  **Consulta de estado acotada.** Los envíos asíncronos (set de pruebas) se consultan con `GetStatusZip` hasta `POLL_MAX_ATTEMPTS`. Al agotarse, el trabajo queda `dead` y se registra un evento `POLLING_EXHAUSTED`.
9.  **Apagado ordenado.** Ante SIGTERM, el worker deja de reclamar y espera a que terminen los trabajos en curso (`SHUTDOWN_GRACE_SECS`). El API deja de aceptar conexiones y termina las solicitudes en curso.

Los trabajos en estado `dead` forman la cola de revisión manual:

```sql
SELECT j.kind, j.attempts, j.last_error, d.prefix, d.document_number, d.status
FROM jobs j JOIN documents d ON d.id = j.document_id
WHERE j.status = 'dead' ORDER BY j.updated_at DESC;
```

---

## 6. Licencia

Este proyecto está bajo los términos de la licencia **Apache License 2.0**. Para más detalles, consulta el archivo [LICENSE](LICENSE).

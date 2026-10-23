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
*   **`crypto-signer`**: canonicalización C14N y firma **XAdES-EPES** (RSA + SHA-384), con certificado por tenant desde HashiCorp Vault o un par local para desarrollo.
*   **`dian-soap-client`**: cliente SOAP 1.2 (WS-Addressing, WS-Security firmado con el certificado del tenant, timeouts) para los Web Services de la DIAN.
*   **`api-server`**: servidor REST (Axum) con autenticación por tenant.
*   **`worker`**: proceso que reclama y procesa los trabajos de la cola.

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

### Paso 2: Configurar las Variables de Entorno
Los binarios leen la configuración del **entorno del proceso** (no cargan un archivo `.env` por sí mismos; usa tu orquestador, `env $(cat .env | xargs) cargo run ...` o similar). Las variables obligatorias no tienen valor por defecto: si falta alguna, el proceso termina al arrancar con un mensaje explícito.

```env
# --- Obligatorias (api-server y worker) ---
DATABASE_URL=postgres://tu_usuario:tu_contraseña@127.0.0.1:5432/dian

# --- Firma (XAdES de documentos y WS-Security del SOAP): elige UNA opción (obligatorio) ---
# El mismo certificado del tenant firma sus documentos y la cabecera WS-Security de cada
# llamada a la DIAN (BinarySecurityToken + firma del Timestamp y wsa:To).
# a) Producción: certificado por tenant desde HashiCorp Vault (KV v2). Cada tenant necesita
#    una fila activa y vigente en `certificates`; su `vault_reference_path` es la ruta del secreto
#    (campos `certificate` y `private_key`).
VAULT_ADDR=https://vault.example.com
VAULT_TOKEN=...
# b) Desarrollo: un único par local compartido por todos los tenants.
# SIGNER_CERT_PATH=/ruta/cert.pem
# SIGNER_KEY_PATH=/ruta/key.pem

# --- Opcionales ---
PORT=8080
RUST_LOG=info
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

### Paso 3: Compilar y Ejecutar

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

> **Estado: aún no implementado.** La firma HMAC descrita abajo existe en el código (`api-server/src/webhook.rs`), pero todavía no se despacha ninguna notificación. Mientras tanto, consulta el estado con `GET /api/v1/documents/:id`.

El objetivo es que, en lugar de forzar a tu ERP a hacer peticiones continuas (*polling*), `Dian_Service` envíe una petición HTTP POST a la URL de Webhook registrada en tu Tenant.

#### Seguridad del Webhook (Verificación de Integridad):
Para certificar que la notificación proviene de tu servidor y no de un tercero malintencionado, la API calcula una firma **HMAC-SHA256** del payload y la inyecta en la cabecera:

`X-Hub-Signature-256: sha256={hash_hexadecimal_de_firma}`

*Tu ERP debe calcular el hash del body recibido usando el `webhook_secret` compartido del Tenant y confirmar que coincide con la firma de la cabecera.*

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

### G. Monitoreo y Diagnóstico (Health Check)

Permite consultar el estado operativo de las dependencias clave.

*   **Endpoint:** `GET /api/v1/health` (Acceso público, no requiere API Key)
*   **Respuesta:** `200 OK` con `{"status": "UP", "database": "CONNECTED"}`, o `503 Service Unavailable` con `{"status": "DOWN", "database": "DISCONNECTED"}` si PostgreSQL no responde.

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

# Dian_Service - Motor de Cumplimiento Tributario Open-Source para la DIAN

Dian_Service es un motor de facturación electrónica y cumplimiento tributario para Colombia (DIAN) bajo el estándar **UBL 2.1**, desarrollado en **Rust** siguiendo los principios de la **Arquitectura Hexagonal**.

Este proyecto está diseñado para funcionar como un microservicio independiente (Open Core) que cualquier software, ERP o POS (independientemente del lenguaje de programación: Laravel, Django, .NET, Node.js) puede integrar para delegar la firma criptográfica XAdES-EPES, el cálculo de CUFE/CUDE, la comunicación SOAP y el cumplimiento normativo exigido por la DIAN (Resolución 165 de 2023, 227 de 2025 y concordantes).

---

## 1. Arquitectura Hexagonal y Estructura

El repositorio está organizado como un **Cargo Workspace** compuesto por sub-crates altamente desacoplados:

*   **`core-domain`**: Contiene los modelos de datos puros (Factura, Certificados, Rangos de Numeración, Eventos) y los Puertos (Traits en Rust) que definen los enchufes de salida del sistema.
*   **`ubl-engine`**: Motor de plantillas XML UBL 2.1 (basado en MiniJinja con plantillas compiladas en el binario mediante `include_str!`) y lógicas matemáticas para el cálculo determinista de CUFE/CUDE/CUDS/CUNE.
*   **`crypto-signer`**: Módulo criptográfico encargado de la canonicalización XML exclusiva (C14N) y firma avanzada **XAdES-EPES** utilizando firmas envolventes RSA + SHA-256. Soporta HashiCorp Vault e implementación offline local.
*   **`dian-soap-client`**: Adaptador cliente SOAP v1.2 con WS-Security y WS-Addressing para interactuar de forma segura con los Web Services oficiales de la DIAN.
*   **`worker-redis`**: Background worker asíncrono y resiliente que gestiona las colas de transmisión confiables (BRPOPLPUSH), backoff exponencial con jitter para fallos de red y disyuntores (*Circuit Breakers*) atómicos para contingencias tecnológicas.
*   **`api-server`**: Servidor REST expuesto en HTTP (basado en Axum) que gestiona la autenticación multi-tenant y expone las interfaces de consumo e ingesta.

---

## 2. Requisitos Previos

Asegúrate de contar con los siguientes elementos instalados en tu entorno:

*   **Rust Compiler** (Edición 2021 o superior)
*   **PostgreSQL** (Esquema relacional con soporte JSONB)
*   **Redis** (Para orquestación y colas asíncronas)

---

## 3. Instalación y Configuración rápida

### Paso 1: Configurar la Base de Datos
Crea una base de datos en tu servidor PostgreSQL y aplica **todas** las migraciones de `migrations/` en orden de nombre:

```bash
for f in $(ls migrations/*.sql | sort); do psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f "$f"; done
```

*   `20260523000000_init.sql` crea las tablas (`tenants`, `certificates`, `numbering_ranges`, `documents`, `dian_events`, `webhooks`) y sus índices.
*   `20261021000000_harden_documents.sql` añade el estado `processing_failed`, distingue documentos emitidos y recibidos (`direction`, `issuer_nit`) en la clave única, y cambia el borrado en cascada de documentos y eventos por `ON DELETE RESTRICT`.

### Paso 2: Configurar las Variables de Entorno
Los binarios leen la configuración del **entorno del proceso** (no cargan un archivo `.env` por sí mismos; usa tu orquestador, `env $(cat .env | xargs) cargo run ...` o similar). Las variables obligatorias no tienen valor por defecto: si falta alguna, el proceso termina al arrancar con un mensaje explícito.

```env
# --- Obligatorias (api-server y worker-redis) ---
DATABASE_URL=postgres://tu_usuario:tu_contraseña@127.0.0.1:5432/dian
REDIS_URL=redis://127.0.0.1:6379
DIAN_BINARY_SECURITY_TOKEN=tu_certificado_x509_en_base64

# --- Firma XAdES: elige UNA de las dos opciones (obligatorio) ---
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
MAX_CONCURRENT_JOBS=5
# Endpoints SOAP alternativos (por defecto, los oficiales de la DIAN)
# DIAN_HAB_URL=https://vpfe-hab.dian.gov.co/WcfDianCustomerServices.svc
# DIAN_PROD_URL=https://vpfe.dian.gov.co/WcfDianCustomerServices.svc
# Solo para pruebas: habilita el registro de facturas recibidas SIN verificar su firma
# INBOUND_ALLOW_UNVERIFIED_SIGNATURES=false
```

Si la firma de un documento falla (por ejemplo, el tenant no tiene un certificado activo y vigente), el documento pasa a `processing_failed` y el motivo queda en `dian_events`.

### Paso 3: Compilar y Ejecutar

El proyecto se divide en dos binarios independientes:

1.  **Ejecutar la API REST (api-server)**:
    ```bash
    cargo run --bin api-server
    ```
2.  **Ejecutar el Background Worker (worker-redis)**:
    ```bash
    cargo run --bin worker-redis
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
  "technical_key": "clave_tecnica_dian_o_pin",
  "document_type": "invoice",
  "test_set_id": null
}
```

*   **Respuesta de la API (HTTP 202 Accepted):**

El servidor REST valida el JSON localmente en microsegundos, lo guarda en PostgreSQL en estado `Pending`, inyecta el trabajo en la cola de Redis y responde de inmediato al ERP:

```json
{
  "document_id": "fe9b0d4e-b5c1-4b2a-8cfa-d0ad28c46001",
  "status": "QUEUED",
  "message": "Factura encolada correctamente para su firmado y transmision"
}
```

Posteriormente, el worker asíncrono se encarga del cálculo de CUFE, firmado XAdES-EPES, empaquetado ZIP y transmisión a la DIAN.

> **Limitación actual:** `document_type` solo admite `"invoice"`. Las notas crédito y débito (`credit_note`, `debit_note`) se rechazan con `422` hasta que tengan su propia plantilla UBL (con `BillingReference`) y su CUDE.

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

> **Estado: deshabilitado por defecto (`501 Not Implemented`).** La verificación criptográfica de la firma del `AttachedDocument` todavía no está implementada; sin ella no es posible distinguir una factura legítima de una fabricada. Para pruebas se puede habilitar con `INBOUND_ALLOW_UNVERIFIED_SIGNATURES=true`: los documentos quedan marcados como no verificados (`signature_verified: false`) y **no admiten eventos RADIAN**.

*   **Qué hace Dian_Service internamente (cuando está habilitado):**
    1.  Descomprime el ZIP en memoria, con un límite de 10 MiB descomprimidos y 20 entradas.
    2.  Comprueba que el XML contenga una firma (solo presencia, **no** verificación criptográfica).
    3.  Verifica que la respuesta DIAN embebida tenga un `ResponseCode` de `"02"` (Aceptación DIAN).
    4.  Inserta el documento como recibido (`direction = inbound`, `issuer_nit` = NIT del emisor). Si esa factura del mismo emisor ya estaba registrada, responde `409 Conflict`; nunca sobrescribe otro documento.

---

### D. Registro de Eventos RADIAN (Título Valor)

Una vez ingresada la factura del proveedor (inbound), puedes formalizar eventos legales ante el RADIAN para su circulación.

> Solo se admiten eventos sobre facturas recibidas cuya firma fue verificada. Mientras la verificación no esté implementada (ver sección C), este endpoint responde `422` para todas las facturas recibidas.

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

*   **Respuesta (HTTP 202 Accepted):** Retorna `document_id` y encola el trabajo en Redis, calculando el CUNE (SHA-384) y firmando el reporte en segundo plano.

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

*   **Respuesta (HTTP 202 Accepted):** El servidor reserva el consecutivo atómico (ej: 200) y responde:
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
*   **Respuesta (HTTP 200 OK):**
```json
{
  "status": "UP",
  "database": "CONNECTED",
  "redis": "CONNECTED"
}
```

---

## 5. Arquitectura de Numeración Concurrente y Resiliencia

El motor incluye optimizaciones avanzadas para prevenir fallos bajo alta concurrencia y garantizar la secuencia de numeración exigida por la DIAN:

1.  **Bloqueo Pesimista Corto (`SELECT ... FOR UPDATE`)**:
    Los endpoints `POST /api/v1/invoices`, `POST /api/v1/payroll` y `POST /api/v1/support-documents` inician una transacción corta de base de datos y bloquean de forma exclusiva la fila del rango de numeración del Tenant. El consecutivo es validado e incrementado de forma segura, y la transacción se confirma (`commit`) inmediatamente. Esto libera los locks en menos de `10ms`, mucho antes de iniciar operaciones de red lentas con Redis o SOAP de la DIAN.
2.  **Control de Concurrencia de Transmisión**:
    El background worker (`worker-redis`) utiliza un semáforo asíncrono (`tokio::sync::Semaphore`) limitado por `MAX_CONCURRENT_JOBS` para controlar el número de peticiones SOAP enviadas en paralelo a la DIAN. Esto evita sobrecargar la red y previene timeouts.
3.  **Monitoreo Proactivo**:
    El worker chequea la salud de PostgreSQL y Redis antes de de-encolar nuevos trabajos, transicionando a esperas con backoff si alguna dependencia crítica se desconecta.

---

## 6. Licencia

Este proyecto está bajo los términos de la licencia **Apache License 2.0**. Para más detalles, consulta el archivo [LICENSE](LICENSE).

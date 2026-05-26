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
Crea una base de datos en tu servidor PostgreSQL y corre el script de migración inicial ubicado en:
`migrations/20260523000000_init.sql`.

Este script creará las tablas obligatorias (`tenants`, `certificates`, `numbering_ranges`, `documents`, `dian_events`, `webhooks`) e índices optimizados de búsqueda por CUFE y multi-tenant.

### Paso 2: Configurar las Variables de Entorno
Crea un archivo `.env` en la raíz del proyecto con la siguiente configuración:

```env
# Base de Datos
DATABASE_URL=postgres://tu_usuario:tu_contraseña@127.0.0.1:5432/dian

# Cola de Mensajería
REDIS_URL=redis://127.0.0.1:6379

# Credenciales de Seguridad SOAP DIAN
DIAN_BINARY_SECURITY_TOKEN=tu_certificado_x509_en_base64

# Configuración de Servidor
PORT=8080
RUST_LOG=info
```

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

---

### B. Notificaciones de Estado mediante Webhooks

En lugar de forzar a tu ERP a hacer peticiones continuas (*polling*) para saber si la DIAN aceptó o rechazó la factura, `Dian_Service` enviará una petición asíncrona HTTP POST a la URL de Webhook registrada en tu Tenant.

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

*   **Qué hace Dian_Service internamente:**
    1.  Descomprime el ZIP en memoria.
    2.  Valida criptográficamente la firma digital del `AttachedDocument`.
    3.  Verifica que la respuesta DIAN embebida tenga un `ResponseCode` de `"02"` (Aceptación DIAN).
    4.  Extrae los metadatos e introduce el documento en Postgres con estado `dian_accepted`.

---

### D. Registro de Eventos RADIAN (Título Valor)

Una vez ingresada la factura del proveedor (inbound), puedes formalizar eventos legales ante el RADIAN para su circulación.

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

## 5. Licencia

Este proyecto está bajo los términos de la licencia **Apache License 2.0**. Para más detalles, consulta el archivo [LICENSE](LICENSE).

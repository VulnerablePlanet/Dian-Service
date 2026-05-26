# Arquitectura open source universal para facturación electrónica DIAN en Colombia

## Resumen ejecutivo

Sí es técnicamente posible construir una API open source universal que reemplace **la capa técnica** de servicios pagos como Factus, Alegra, Carvajal o Siigo API, pero no es realista pensar que vas a reemplazar de inmediato **todo** lo que esos proveedores resuelven alrededor del núcleo DIAN: soporte operativo, monitoreo 24/7, acompañamiento de habilitación, manejo de incidencias ambiguas, actualizaciones normativas y operación tributaria diaria. La DIAN hoy concentra el sistema de facturación en un ecosistema más amplio que no es solo factura: incluye factura electrónica de venta, documento soporte de nómina electrónica, documento soporte en adquisiciones con no obligados, documentos equivalentes y RADIAN. Además, la regulación vigente está compilada en la Resolución 227 de 2025, cuyo bloque funcional de facturación viene principalmente de la Resolución 165 de 2023, con cambios posteriores como la Resolución 8 de 2024, la Resolución 202 de 2025 y una contingencia especial introducida por la Resolución 11 de 2026. citeturn38search10turn13view1turn13view0turn13view2turn13view3

La conclusión fuerte de esta investigación es esta: **vale la pena construir el open core**, pero conviene enfocarlo como una **plataforma de cumplimiento DIAN** y no como “otro ERP”. El corazón reusable debe encargarse de UBL 2.1, reglas DIAN, CUFE/CUDE/CUDS/CUNE, firma XAdES-EPES, WS-Security, SOAP, validación local, numeración, almacenamiento de XML/PDF/AttachedDocument/ApplicationResponse, trazabilidad y eventos. Encima de eso sí conviene exponer REST, gRPC, webhooks, SDKs y CLI desacoplados del lenguaje del sistema principal. Lo que **no** conviene meter en el núcleo desde el día uno es contabilidad, CRM, inventario, cartera o UX de usuario final; esos son dominios de aplicación, no del motor DIAN. Esta separación reduce lock-in, facilita mantenimiento y permite integrar el motor con Laravel, Django, Spring, .NET, Node, Go o desktop systems sin rehacer la parte tributaria en cada stack. citeturn33view1turn40search2turn27search3turn23search0turn24search1

## Cómo funciona oficialmente la DIAN

La DIAN publica hoy dos endpoints oficiales principales del servicio SOAP `WcfDianCustomerServices`: uno para **habilitación** y otro para **producción**. Las páginas del servicio exponen la WSDL para generación de cliente con `svcutil.exe`, y dejan claro que existe una descripción `?wsdl` y otra `?singleWsdl`. En términos prácticos, tu plataforma debe tratar estos endpoints como **adaptadores externos** y nunca acoplar la lógica tributaria al cliente SOAP generado, porque ese acoplamiento complica las migraciones y rompe pruebas. citeturn4view0turn4view2turn6search11

```text
Habilitación:
https://vpfe-hab.dian.gov.co/WcfDianCustomerServices.svc?wsdl
https://vpfe-hab.dian.gov.co/WcfDianCustomerServices.svc?singleWsdl

Producción:
https://vpfe.dian.gov.co/WcfDianCustomerServices.svc?wsdl
https://vpfe.dian.gov.co/WcfDianCustomerServices.svc?singleWsdl
```

La DIAN, en la compilación vigente, exige que antes de expedir documentos se realice un procedimiento de **habilitación**: inscripción en el servicio, selección del modo de operación, registro del software, ejecución de pruebas y, una vez aprobadas, actualización del estado a “habilitado”; además, se debe tramitar la autorización de numeración correspondiente. La norma vigente distingue explícitamente tres modos de operación: **desarrollo informático propio o adquirido**, **servicio gratuito DIAN** y **software suministrado a través de proveedor tecnológico** para factura electrónica de venta. citeturn33view0turn3search9turn3search11

### Modos de operación y su efecto arquitectónico

| Modo | Qué significa en la práctica | Implicación técnica para tu API universal | Evidencia |
|---|---|---|---|
| Software propio o adquirido | El obligado integra y opera su propio software ante la DIAN | Tu API debe poder actuar como “compliance engine” standalone | citeturn33view0 |
| Facturación Gratuita DIAN | La DIAN provee un software gratuito y hasta certificado gratuito para quienes usan esa modalidad | No compites por precio base; compites por automatización, integración, multiempresa y operación avanzada | citeturn39search8turn38search0turn34search5 |
| Proveedor tecnológico | Tercero autorizado por DIAN para prestar servicios inherentes a la generación y transmisión | Tu proyecto puede nacer como motor open source y luego operar como servicio gestionado si busca esa habilitación | citeturn33view0turn11search14turn12search4 |

La secuencia operativa real no es “JSON entra y factura sale”. El flujo oficial es, en esencia, este:

```text
Dominio negocio
   -> mapeo a UBL 2.1 + extensiones DIAN
      -> firma XML XAdES-EPES
         -> compresión ZIP
            -> SOAP 1.2 + WS-Security + WS-Addressing
               -> validación DIAN
                  -> respuesta síncrona o TrackId
                     -> consulta de estado
                        -> almacenamiento de XML / ApplicationResponse / AttachedDocument / PDF
                           -> entrega al adquirente
```

Ese patrón aparece repartido entre la compilación normativa, el anexo técnico y las guías de consumo del web service. Las guías DIAN muestran operaciones de consulta y envío; los snippets públicos de la WSDL exponen, al menos, operaciones como `SendBillAsync`, `SendBillSync`, `SendEventUpdateStatus` y `SendNominaSync`, y la guía oficial de completado de adquirientes documenta `GetAcquirer`. En versiones previas del anexo, todavía útiles para entender la mecánica del WS, también se documenta `GetStatus` y la carga de ZIP para `SendBillAsync`. Como la WSDL pública no se dejó inspeccionar completa en esta sesión, tomo el inventario mínimo desde los snippets accesibles y lo marco como parcialmente incompleto. citeturn40search0turn40search1turn40search2turn19search2

Un inventario de alto valor, con buen nivel de confianza, queda así:

| Operación / familia | Rol |
|---|---|
| `SendTestSetAsync` | envío de set de pruebas en habilitación |
| `SendBillSync` / `SendBillAsync` | envío de factura |
| `GetStatus` / `GetStatusZip` | consulta de respuesta / TrackId |
| `GetNumberingRange` | consulta de rangos de numeración |
| `GetXmlByDocumentKey` | recuperación de XML por clave documental |
| `SendNominaSync` | nómina electrónica |
| `SendEventUpdateStatus` | actualización / eventos asociados |
| `GetAcquirer` | completar datos del adquirente |
| `GetExchangeEmails` | consulta del correo de intercambio del adquirente/facturador en el modelo de recepción |
| `SendBillAttachmentAsync` | envío de documentos adjuntos / contenedores según implementaciones y WSDL snippets |

La DIAN también cambió el modelo de recepción. En el flujo publicado para recepción de facturas, el intercambio incluye un `.ZIP` con `AttachedDocument` como **contenedor electrónico** y un PDF de representación gráfica como adjunto opcional; además, el correo electrónico del adquirente es parte operativa del proceso, y la DIAN publica métodos como `GetExchangeEmails` y, más recientemente, un servicio para completar datos del adquirente (`GetAcquirer`). Esto cambia mucho el diseño: tu API no solo debe emitir, también debe **recibir, conservar, relacionar y evidenciar entrega**. citeturn34search11turn11search4turn40search2

## Marco legal y documental vigente

La base jurídica más importante hoy es la **Resolución 165 de 2023**, que desarrolló el sistema de facturación, adoptó la **versión 1.9 del Anexo Técnico de Factura Electrónica de Venta** y expidió la versión 1.0 del Anexo Técnico de Documento Equivalente Electrónico. Después, la **Resolución 227 de 2025** compiló estas reglas en un cuerpo único, y la **Resolución 8 de 2024** movió al **1 de mayo de 2024** la adopción de la versión 1.9 del anexo de factura electrónica. Más recientemente, la **Resolución 202 de 2025** ajustó el tratamiento de servicios públicos y precisó los datos exigibles al adquirente, y la **Resolución 11 de 2026** creó una contingencia especial de regularización voluntaria para un caso extraordinario derivado del Decreto Legislativo 0240 de 2026. citeturn13view0turn12search7turn12search15turn13view2turn13view3

### Requisitos normativos que tu software no puede delegar

| Requisito | Qué significa para el diseño | Evidencia |
|---|---|---|
| Habilitación previa | No puedes emitir “legalmente” solo porque tu API arme XML; debes pasar el procedimiento DIAN por software y modo de operación | citeturn33view0 |
| Numeración autorizada | Debes persistir resolución, prefijo, rango, vigencia y control de consumo de consecutivos | citeturn33view0turn33view2 |
| Vigencia de numeración | La autorización tiene vigencia máxima de dos años | citeturn33view2 |
| XML firmado y anexo técnico | El cumplimiento técnico está en el Anexo Técnico de Factura Electrónica de Venta 1.9 | citeturn13view0turn33view1 |
| CUFE / CUDE y QR | Son parte central del esquema documental y de validación | citeturn19search0turn19search3turn21search6 |
| Representación gráfica | Debe contener, como mínimo, información fiscalmente relevante; el PDF es una representación, no el documento fuente | citeturn34search17turn34search11 |
| Conservación y entrega | La factura se genera, valida, expide, recibe y conserva electrónicamente; además debe poder entregarse al adquirente por mensaje de datos o representación gráfica | citeturn38search2turn34search2turn34search11 |
| RADIAN y eventos | Si vas a cubrir título valor, necesitas registrar eventos posteriores a la factura | citeturn33view3turn20search12 |

En la compilación vigente, los documentos del sistema de facturación no se limitan a la factura electrónica de venta. La propia DIAN describe el sistema como un conjunto que incluye factura electrónica, nómina electrónica, documento soporte en adquisiciones con no obligados, documentos equivalentes y RADIAN. Para una API universal seria, esto implica que la frontera correcta del producto no es “invoice service”, sino **document compliance platform**. citeturn38search10turn20search11turn21search0turn21search3

Sobre UBL, la DIAN sigue anclada en **UBL 2.1**, y su anexo 1.9 cubre explícitamente elementos como **CUFE/CUDE**, **XAdES-EPES**, `AttachedDocument` y `ApplicationResponse`. En otras palabras: tu modelo interno no debería ser un simple DTO plano; debería ser un **modelo canónico de documento electrónico** capaz de proyectarse a `Invoice`, `CreditNote`, `DebitNote`, `AttachedDocument` y `ApplicationResponse`. Si no construyes ese modelo canónico desde el principio, terminarás duplicando lógica por tipo documental y por sector. citeturn12search7turn19search0turn19search2turn19search7

En documento soporte con no obligados, la regla operativa vigente exige generación y transmisión electrónica por parte de los sujetos obligados definidos por la Resolución 167 de 2021, y este documento usa **CUDS** y notas de ajuste; además, la propia compilación deja claro que el número de un documento anulado o corregido no puede reutilizarse. En nómina electrónica, la DIAN exige transmisión uno a uno por beneficiario y, bajo contingencia tecnológica, prevé un plazo de 48 horas desde el día siguiente al restablecimiento para transmitir nómina y notas de ajuste. citeturn21search3turn21search4turn21search6turn32view1

En RADIAN, una factura solo entra a circular como título valor cuando ha cumplido y validado requisitos adicionales: fecha de vencimiento, acuse de recibo, recibo del bien o del servicio y aceptación expresa o tácita. Luego vienen eventos como inscripción, endoso, aval, mandato, informe para el pago, pago total o parcial, limitación de circulación, protesto y transferencia de derechos económicos. Esta parte no es opcional si tu idea es reemplazar técnicamente a proveedores enterprise. citeturn33view3turn20search12

### Firma digital, XAdES y WS-Security

La DIAN exige firma basada en certificado digital vigente X.509 expedido por entidad certificadora autorizada y define el uso de **XAdES-EPES** en su documentación técnica. Además, su guía oficial para consumo de web services obliga a configurar keystore, **WS-Security Signature**, **Timestamp** en milisegundos, `WS-Addressing`, autenticación y el `action` correcto en el `Content-Type` para el método consumido. Esto confirma que el mayor dolor técnico no está en generar JSON ni en “hacer POST”, sino en ejecutar correctamente **XML canonicalization + XMLDSig/XAdES + SOAP security headers**. citeturn19search8turn40search2

La consecuencia práctica es dura: la parte más sensible de la plataforma debe tratarse como **motor criptográfico y de interoperabilidad**, no como simple integration layer. Mi recomendación es encapsularla en un servicio propio, con tests de snapshot XML, validación XSD, fixtures de firma y regresión por cambios normativos. Reescribir esa capa muchas veces en cada aplicación cliente es la receta perfecta para terminar con errores de canonicalización, namespaces y firmas intermitentes. Esa conclusión está alineada con cómo las librerías comunitarias que sí avanzaron se concentran justamente en firma XAdES, token binario SOAP, envío y consulta de estado. citeturn36view4turn35search4turn36view3turn40search2

## Repositorios open source y mercado actual

El ecosistema open source colombiano para DIAN existe, pero está fragmentado. Lo más importante no es “si hay repos”, sino **qué tan reutilizables son para un producto universal**. Mi lectura crítica es que hoy sí hay material valioso para reutilizar, pero casi nada está listo para ser “el reemplazo total” sin una capa arquitectónica más seria encima. citeturn36view1turn36view2turn36view3turn36view4

### Repositorios open source relevantes

| Proyecto | Stack | Estado observable | Qué sirve | Riesgo principal | Evidencia |
|---|---|---|---|---|---|
| `soenac/api-dian` | PHP / Laravel | **Archivado** el 25-jun-2023; 67 stars y 67 forks; Packagist lo marca como abandonado | Buen referente histórico de API UBL 2.1 y Swagger | Demasiado riesgo de obsolescencia normativa y de seguridad para usarlo como núcleo vivo | citeturn36view1turn35search12 |
| `bit4bit/facho` | Python | Repositorio movido a Codeberg; 12 stars, 5 forks | Útil por su CLI y por abstraer creación XML, firma y cliente DIAN | Señales de mantenimiento fragmentado y documentación apoyada en guías viejas | citeturn36view3 |
| `Crispancho93/facturacion-electronica-colombia` | Python / FastAPI | 4 commits, 11 stars, 11 forks, sin releases; opera en habilitación | Buena base didáctica para una API mínima | Todavía parece **POC / laboratorio** más que motor universal listo para producción | citeturn37view3 |
| `diegoasencio96/django-dian` | Python / Django | 8 stars, 1 fork; instala vía `pip`; README con base normativa antigua | Reutilizable como integración Django y modelo de librería empaquetable | Mezcla referencias normativas previas a la regulación actual; requiere actualización fuerte | citeturn37view1turn36view2 |
| `co-ubl21dian` | PHP / GitLab | Proyecto histórico; forks públicos actualizados hasta mar-2025 | Muy valioso como referencia de SOAP, firma y pruebas | Licenciamiento y mantenimiento actual no quedaron claros en las fuentes accesibles | citeturn35search4turn35search9turn35search14 |
| `lopezsoft/ubl21dian` | Node / TypeScript con herencia PHP | 80 commits, licencia MIT, 6 releases; última release visible nov-2025 | Es el candidato más interesante como **core reusable** moderno | Tamaño del proyecto todavía moderado; hay que revisar profundidad de cobertura y seguridad antes de adoptarlo como base maestra | citeturn36view4turn37view2 |
| `vixark/SimpleOps` | .NET | ERP open source con DIAN incluida | Sirve como referencia de producto completo | No es el mejor punto de partida para un **motor universal desacoplado**; mezcla demasiados dominios | citeturn39search0turn39search2 |

Mi recomendación clara aquí es: **no forks a ciegas un ERP ni un API archivado**. Si quieres construir algo con visión de 20 años, lo más sensato es levantar un **open core nuevo**, pero reutilizando piezas donde ya hay valor probado: especialmente ideas, tests y quizá componentes de `lopezsoft/ubl21dian`, `facho` y `co-ubl21dian`. `soenac/api-dian` sirve más como archivo histórico que como foundation actual. citeturn36view1turn36view3turn36view4

### Comparación técnica con proveedores pagos

| Proveedor | Lo que sí muestra públicamente | Lectura crítica |
|---|---|---|
| Factus | API pública amplia, autenticación por token, rate limit de 80 req/min por usuario, suscripciones/webhooks, documentos soporte, recepción, rangos, adquirientes, facturación para salud, mandato y SLA público | Muy orientado a developers e integradores; es probablemente el benchmark más cercano a la API universal que quieres construir | citeturn27search0turn27search1turn27search7turn27search13turn27search4turn31search9turn31search13 |
| Alegra | API pública general, webhooks, numeraciones, e-provider específico para Colombia con endpoint de emisión a DIAN | Más maduro como suite contable/administrativa que como motor compliance desacoplado puro; el API existe, pero su centro de gravedad no es solo DIAN | citeturn23search0turn23search2turn39search20turn29search20 |
| Siigo API | JWT/OAuth, webhooks, batch de facturas, secciones de idempotencia y seguimiento documental; Siigo comercializa FE cloud con certificado incluido por un año en ciertos planes | Muy fuerte en ecosistema empresarial; útil para integrar con Siigo, pero con más lock-in funcional que una capa compliance independiente | citeturn28search7turn28search0turn28search6turn28search16turn24search2turn24search15 |
| Carvajal | Portafolio público de emisión, recepción, eventos, documentos equivalentes, RADIAN y nómina; reporte anual con 99,93% de disponibilidad y 584,3 millones de transacciones en Colombia en 2024 | Claramente enterprise. Mucha operación, poca documentación pública detallada de API. Más difícil de sustituir en acompañamiento, no tanto en protocolo | citeturn26search0turn26search1 |

La ventaja competitiva real de los proveedores pagos no es que “sepan más XML”, sino que ya resolvieron cinco cosas difíciles al mismo tiempo: cambios normativos frecuentes, observabilidad operacional, soporte a clientes no técnicos, contingencias, y una taxonomía práctica para sectores especiales. Factus, por ejemplo, ya publica abstracciones de salud, transporte, mandato, recepción y rangos; Siigo resuelve batch, autenticación y webhooks; Alegra ya separa un `e-provider` colombiano; Carvajal se mueve con escala y disponibilidad enterprise. Eso no invalida el open source; al contrario, te dice exactamente **qué partes vale la pena copiar arquitectónicamente** y cuáles no deberías intentar rehacer con la misma UX desde el sprint uno. citeturn27search3turn27search5turn31search9turn23search0turn39search20turn28search6turn28search0turn26search0turn26search1

## Diseño propuesto de la plataforma universal

Mi propuesta es una arquitectura **hexagonal, orientada a eventos y multi-tenant**, donde el centro sea un **Compliance Kernel DIAN**. Ese kernel concentra todo lo inestable y críticamente normativo: catálogos DIAN, mapeo UBL 2.1, CUFE/CUDE/CUDS/CUNE, firma XAdES, validación XSD/Schematron si la implementas, construcción del ZIP, cliente SOAP, parser de ApplicationResponse, control de numeración y machine state de documentos. Todo lo demás debe ser adaptador. Esta es una inferencia arquitectónica apoyada en la fuerte separación que la DIAN hace entre modos de operación, habilitación, interoperabilidad y anexos técnicos, y en cómo los proveedores comerciales abstraen exactamente esas mismas capas para integrarse desde cualquier lenguaje. citeturn33view0turn33view1turn27search2turn23search0turn24search1

```text
                   +------------------------------+
                   |          Client Apps         |
                   | Laravel | Django | ERP | POS|
                   +---------------+--------------+
                                   |
                          REST / gRPC / CLI
                                   |
                     +-------------v--------------+
                     |       API Gateway          |
                     | auth | RBAC | rate-limit   |
                     +-------------+--------------+
                                   |
              +--------------------+--------------------+
              |                                         |
   +----------v-----------+                 +-----------v----------+
   |   Command Service    |                 |  Query / Status API  |
   | create/send/cancel   |                 | tracking/files/audit |
   +----------+-----------+                 +-----------+----------+
              |                                         |
              +--------------------+--------------------+
                                   |
                      +------------v-------------+
                      |  Compliance Kernel DIAN  |
                      | UBL | CUFE/CUDE | XAdES  |
                      | ZIP | SOAP | states      |
                      +------+-----------+--------+
                             |           |
                    +--------v--+     +--v---------+
                    | Workers    |     | Webhooks   |
                    | retries    |     | outbound   |
                    | queues      |     | signatures |
                    +--------+----+     +------+-----+
                             |                 |
                    +--------v-----------------v------+
                    | PostgreSQL | Redis | ObjectStore |
                    | Audit Log  | XML/PDF/ZIP/AR      |
                    +----------------------------------+
```

### Tecnologías con visión larga

Si el criterio de verdad es **durabilidad, rendimiento, mantenibilidad y oferta de talento**, mi selección sería esta:

| Capa | Recomendación principal | Alternativas razonables | Motivo |
|---|---|---|---|
| Núcleo de cumplimiento y firma | **Java/Kotlin** o **Rust** | Go | Java/Kotlin te da ecosistema XML/WS maduro; Rust te da control, seguridad y performance para la parte más delicada |
| Gateway y APIs | **Go** o **Kotlin/Spring Boot** | .NET | Excelente para concurrencia, middleware, gRPC y operaciones cloud-native |
| SDK rápidos / herramientas | TypeScript / Python | PHP | Aceleran adopción por equipos heterogéneos |
| Base de datos relacional | PostgreSQL | SQL Server | Transacciones, JSONB, particionado, auditoría |
| Cache / locks / rate coordination | Redis | KeyDB | Numeración segura, idempotencia y desacople |
| Colas / streaming | NATS o RabbitMQ para inicio; Kafka si el volumen lo exige | SQS / SNS en cloud | NATS/Rabbit reducen complejidad temprana |
| Orquestación de workflows | Temporal | Cadence | Ideal para reintentos, días festivos, timeouts, compensaciones y trazabilidad |
| Almacenamiento de artefactos | S3 / MinIO | Azure Blob | XML, PDF, ZIP, ApplicationResponse, evidencias |
| Seguridad de secretos | Vault o KMS cloud | HSM si escala regulada | Certificados y contraseñas no deben quedar en la BD en claro |
| Observabilidad | OpenTelemetry + Prometheus + Grafana + Loki | ELK | Necesitas trazas, métricas y logs por document flow |

No hace falta usar todas desde el día uno. Mi opinión: empieza con **PostgreSQL + Redis + RabbitMQ/NATS + MinIO + OTel** y deja Kafka para cuando el throughput y la analítica lo justifiquen. Temporal sí me parece muy buena inversión temprana, porque DIAN y los procesos tributarios están llenos de reintentos, timeouts, ventanas normativas y pasos humanos. Esta parte es recomendación técnica, no mandato normativo; la justificación práctica sale del volumen de estados y eventos que la propia DIAN y los proveedores publican como obligatorios o habituales. citeturn33view1turn33view3turn27search1turn28search0turn24search15

### Modelo de datos recomendado

El modelo debe separar tres niveles: **tenant**, **empresa tributaria** y **documento electrónico**. Si no los separas, el día que metas multiempresa, multi certificados o proveedores híbridos, vas a romper el dominio.

| Tabla / agregado | Campos esenciales |
|---|---|
| `tenants` | `id`, `slug`, `name`, `status`, `plan`, `region`, `created_at` |
| `tenant_users` | `tenant_id`, `user_id`, `role`, `permissions`, `mfa_enforced` |
| `companies` | `tenant_id`, `nit`, `dv`, `legal_name`, `email`, `phone`, `address`, `tax_regime`, `environment`, `dian_mode` |
| `software_registrations` | `company_id`, `software_id`, `software_pin`, `software_name`, `mode`, `status`, `dian_test_set_id` |
| `certificates` | `company_id`, `alias`, `issuer`, `serial_number`, `subject`, `valid_from`, `valid_to`, `storage_ref`, `kms_key_ref`, `status` |
| `numbering_ranges` | `company_id`, `document_type`, `prefix`, `from_number`, `to_number`, `current_number`, `resolution_number`, `technical_key`, `valid_from`, `valid_to`, `status` |
| `parties` | `company_id`, `role`, `document_type`, `document_number`, `name`, `email`, `tax_scheme`, `municipality_code`, `country_code` |
| `documents` | `company_id`, `tenant_id`, `document_kind`, `operation_type`, `profile_execution_id`, `issue_date`, `issue_time`, `currency`, `numbering_range_id`, `sequence`, `uuid`, `cufe_cude_cuds_cune`, `status` |
| `document_lines` | `document_id`, `sku`, `description`, `quantity`, `unit_code`, `unit_price`, `base_amount`, `discount_amount`, `charge_amount`, `taxable_amount`, `line_total` |
| `document_taxes` | `document_id`, `line_id`, `tax_code`, `percent`, `base_amount`, `tax_amount` |
| `payments` | `document_id`, `means_code`, `payment_due_date`, `payment_id`, `terms` |
| `dian_submissions` | `document_id`, `environment`, `operation`, `soap_action`, `request_hash`, `track_id`, `http_status`, `soap_status`, `sent_at`, `responded_at` |
| `artifacts` | `document_id`, `kind`, `storage_url`, `content_hash`, `mime_type`, `size_bytes` |
| `events` | `document_id`, `event_type`, `source`, `payload`, `validated_by_dian`, `registered_in_radian`, `created_at` |
| `webhook_deliveries` | `tenant_id`, `event_id`, `target_url`, `signature`, `attempt`, `status`, `next_retry_at` |
| `audit_logs` | `tenant_id`, `company_id`, `actor`, `action`, `entity_type`, `entity_id`, `ip`, `user_agent`, `before`, `after`, `created_at` |

### Diseño de endpoints

La interfaz pública sí debería ser moderna, pero **sin esconder demasiado el dominio tributario**. Si abstraes tanto que borras conceptos como numeración, tipo de operación, validación DIAN o AttachedDocument, terminas fabricando una API cómoda pero inútil cuando aparezcan contingencias o eventos RADIAN.

| Endpoint | Uso |
|---|---|
| `POST /v1/companies` | registrar empresa y modo DIAN |
| `POST /v1/companies/{companyId}/certificates` | cargar/rotar certificado |
| `POST /v1/companies/{companyId}/numbering-ranges/sync` | sincronizar rangos/resoluciones |
| `POST /v1/invoices` | crear factura en estado draft |
| `POST /v1/invoices/{id}/validate-and-send` | generar UBL, firmar y transmitir |
| `GET /v1/invoices/{id}` | consultar estado interno |
| `GET /v1/invoices/{id}/xml` | descargar XML firmado |
| `GET /v1/invoices/{id}/pdf` | descargar representación gráfica |
| `GET /v1/invoices/{id}/dian-status` | consultar estado DIAN / TrackId |
| `POST /v1/credit-notes` | emitir nota crédito |
| `POST /v1/debit-notes` | emitir nota débito |
| `POST /v1/support-documents` | documento soporte no obligados |
| `POST /v1/payroll-documents` | nómina electrónica |
| `POST /v1/radian/events` | registrar eventos RADIAN |
| `POST /v1/webhooks/subscriptions` | suscribir eventos |
| `POST /v1/retries/{submissionId}` | forzar reproceso controlado |

Ejemplo de creación de factura en tu API propuesta:

```json
{
  "tenant_id": "tn_01",
  "company_id": "cmp_01",
  "document_type": "invoice",
  "operation_type": "standard",
  "numbering_range_id": "nr_2026_01",
  "issue_date": "2026-05-17",
  "issue_time": "10:42:00-05:00",
  "customer": {
    "identification_type": "31",
    "identification_number": "900123456",
    "name": "Cliente SAS",
    "email": "facturas@cliente.com"
  },
  "payment": {
    "means_code": "1",
    "due_date": "2026-05-17"
  },
  "items": [
    {
      "code": "SKU-001",
      "description": "Servicio de desarrollo",
      "quantity": "1",
      "unit_price": "1000000.00",
      "taxes": [
        {
          "code": "01",
          "percent": "19.00"
        }
      ]
    }
  ],
  "idempotency_key": "inv-cmp_01-20260517-0001"
}
```

Ejemplo de respuesta de estado consolidado:

```json
{
  "id": "doc_01",
  "status": "accepted",
  "dian": {
    "environment": "production",
    "operation": "SendBillSync",
    "track_id": "fe9b0d4e-xxxx",
    "validation_status": "Documento validado por la DIAN",
    "last_checked_at": "2026-05-17T10:42:12-05:00"
  },
  "document": {
    "number": "FEV-10234",
    "cufe": "9f2b...sha384...",
    "xml_url": "/v1/invoices/doc_01/xml",
    "pdf_url": "/v1/invoices/doc_01/pdf",
    "attached_document_url": "/v1/invoices/doc_01/attached-document"
  }
}
```

## Riesgos, roadmap y conclusión crítica

El principal riesgo no es programar SOAP. El principal riesgo es **operar cumplimiento tributario vivo**. La DIAN compila y modifica reglas, cambia anexos, ajusta plazos, introduce casos especiales y deja parte del dolor operativo en guías, micrositios, FAQs y flujos de habilitación. Basta ver que entre 2023 y 2026 hay adopción del anexo 1.9, modificación de datos exigibles al adquirente, reglas especiales para servicios públicos y una contingencia extraordinaria de 2026; eso te obliga a diseñar un motor que soporte versionado normativo, feature flags y migraciones por fecha efectiva. citeturn13view0turn13view2turn13view3

En lo técnico, los problemas reales que más castigan son: canonicalización XML, namespaces, certificados vencidos o mal cargados, diferencias entre estado interno y estado DIAN, numeración concurrente, reintentos sin idempotencia, y errores ambiguos del web service. La propia DIAN obliga a manejar autenticación, WS-Security, timestamp, WS-Addressing y acciones SOAP bien formadas; además, la numeración tiene restricciones legales y el consecutivo consumido en una factura no validada puede obligar a inhabilitar rangos o justificar anulaciones. Por eso tu estrategia de performance no puede ser “más hilos”; tiene que ser **colas, workers, backpressure, idempotencia, locks de numeración y retry orchestration**. citeturn40search2turn33view2

La estrategia de seguridad también debe ser tratada como parte del producto, no como hardening tardío. Los certificados `.p12/.pfx` deben almacenarse cifrados, idealmente con `storage_ref` y claves en Vault/KMS; necesitas RBAC por tenant/empresa, OAuth2/OIDC o JWT para consumidores, firma de webhooks salientes, auditoría completa, backups verificables, y una disciplina de supply-chain security con SBOM, SAST y DAST. Esa recomendación no sale de una norma DIAN puntual, pero es directamente consistente con el hecho de que el sistema maneja identidad digital, firma, información tributaria y evidencia documental. citeturn19search8turn40search2turn38search2

### Roadmap recomendado

| Fase | Alcance |
|---|---|
| Fase inicial | Core UBL 2.1 para factura, nota crédito, nota débito; firma XAdES; envío `SendBillSync`/`GetStatus`; almacenamiento de XML/PDF/AR; numeración |
| Fase operativa | `SendBillAsync`, `GetStatusZip`, AttachedDocument, entrega al adquirente, webhooks, retries, observabilidad completa |
| Fase documental | documento soporte no obligados, nómina electrónica, catálogos DIAN sincronizados |
| Fase título valor | eventos RADIAN, acuse/aceptación, consulta y tracking jurídico-operativo |
| Fase ecosistema | SDKs por lenguaje, CLI, Helm charts, Terraform, multi-cloud, portal admin |
| Fase enterprise open-source | fallback provider adapters, analytics, policy engine, migradores desde Factus/Alegra/Siigo/Carvajal |

### Conclusión crítica

**Sí vale la pena construirlo**, pero solo si tu objetivo es crear una **infraestructura open source de cumplimiento DIAN** reusable por muchas aplicaciones, y no si lo que buscas es únicamente “dejar de pagar proveedor mañana”. La parte que más conviene construir es el **núcleo universal**: mapeo documental, firma, SOAP, estados, numeración, archivos, auditoría, recepción y eventos. La parte que más conviene **reutilizar** son librerías o ideas ya maduras alrededor de firma, transporte SOAP y casos DIAN específicos, especialmente donde ya exista evidencia de releases recientes o de pruebas funcionales. En la muestra revisada, `lopezsoft/ubl21dian` se ve hoy como el mejor candidato comunitario para estudiar y extraer patrones; `facho` y `co-ubl21dian` aportan valor como referencias técnicas; `soenac/api-dian` ya debería tratarse como legado; y proyectos tipo ERP como `SimpleOps` sirven más como producto final que como core universal. citeturn36view1turn36view3turn36view4turn39search0

Si yo tuviera que decidir como arquitecto, no haría “otro Factus completo” desde cero. Haría esto: **open core DIAN + adaptadores + operación opcional administrada**. Ese enfoque te da soberanía técnica, reduce lock-in, permite self-hosting, y deja abierta una monetización sana en soporte, hosting, certificación operativa y tooling, sin encerrar a los usuarios en un black box tributario. La DIAN ya define el protocolo; el valor open source está en volverlo **portable, verificable, testeable y mantenible**. citeturn33view1turn27search2turn23search0turn24search1

## Preguntas abiertas y limitaciones

La principal limitación de esta investigación es que la WSDL pública de DIAN no permitió una inspección completa del texto en esta sesión; por eso el inventario de operaciones SOAP quedó armado con **snippets públicos del WSDL** y con la **guía oficial de `GetAcquirer`**, más evidencia de librerías comunitarias activas. Además, no pude citar aquí una sección oficial puntual del anexo 1.9 sobre todos los campos del **sector salud**; sí encontré evidencia fuerte de soporte comercial actual para salud en proveedores como Factus y Siigo, pero prefiero no presentar esa parte como si estuviera trazada exhaustivamente a una sección oficial concreta del PDF del anexo en esta sesión. citeturn40search0turn40search1turn40search2turn31search9turn28search18

Las fuentes comunitarias más útiles revisadas fueron: `Crispancho93/facturacion-electronica-colombia`, `soenac/api-dian`, `diegoasencio96/django-dian`, `bit4bit/facho`, `co-ubl21dian`, `lopezsoft/ubl21dian` y `vixark/SimpleOps`. Las fuentes oficiales más relevantes fueron el micrositio DIAN del sistema de facturación, la compilación de la Resolución 227 de 2025, la Resolución 165 de 2023, la guía oficial de web services para completar adquirientes, y la documentación/micrositios de documento soporte, nómina, RADIAN y facturación gratuita. citeturn38search10turn13view1turn13view0turn40search2turn20search5turn21search0turn20search6turn39search8
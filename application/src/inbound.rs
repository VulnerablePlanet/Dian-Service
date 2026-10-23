//! Caso de uso: registro de facturas recibidas de proveedores (AttachedDocument).
//!
//! El registro verificado ([`InboundService::register_verified`]) comprueba
//! criptográficamente tres firmas antes de aceptar nada: la del contenedor, la de la factura
//! embebida y la del ApplicationResponse con que la DIAN la validó. Los datos se toman de la
//! factura verificada, nunca del contenedor.

use crate::xml::{split_document_id, tag_content, tag_in_section, XmlDoc};
use core_domain::models::{DocumentStatus, DocumentType, Tenant};
use core_domain::ports::{DocumentRepository, DomainError, NewInboundDocument, SignatureVerifier};
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct InboundRegistered {
    pub document_id: Uuid,
    pub parent_document_id: String,
}

/// Verificación de documentos recibidos.
pub struct InboundVerification {
    pub verifier: Arc<dyn SignatureVerifier>,
    /// Texto que debe contener el sujeto del certificado que firma el ApplicationResponse
    /// (identifica a la DIAN; por defecto su NIT).
    pub dian_signer_subject: String,
}

pub struct InboundService {
    documents: Arc<dyn DocumentRepository>,
    verification: Option<InboundVerification>,
}

fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::Validation(message.into())
}

const ATTACHED_INVOICE: &[&str] = &["AttachedDocument", "Attachment", "ExternalReference", "Description"];
const ATTACHED_RESPONSE: &[&str] = &[
    "AttachedDocument",
    "ParentDocumentLineReference",
    "DocumentReference",
    "Attachment",
    "ExternalReference",
    "Description",
];
const SUPPLIER: &[&str] = &["Invoice", "AccountingSupplierParty", "Party", "PartyTaxScheme"];
const CUSTOMER: &[&str] = &["Invoice", "AccountingCustomerParty", "Party", "PartyTaxScheme"];

fn path(base: &[&'static str], leaf: &'static str) -> Vec<&'static str> {
    base.iter().copied().chain(std::iter::once(leaf)).collect()
}

fn required(doc: &XmlDoc, path: &[&str], what: &str) -> Result<String, DomainError> {
    doc.text(path)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| invalid(format!("No se encontro {}", what)))
}

impl InboundService {
    /// Sin `verification` solo está disponible el registro no verificado.
    pub fn new(documents: Arc<dyn DocumentRepository>, verification: Option<InboundVerification>) -> Self {
        Self { documents, verification }
    }

    pub fn can_verify(&self) -> bool {
        self.verification.is_some()
    }

    /// Registra la factura de un AttachedDocument tras verificar criptográficamente:
    /// 1. la firma del AttachedDocument;
    /// 2. la firma de la factura embebida;
    /// 3. la firma del ApplicationResponse embebido, que debe ser de la DIAN, validar la
    ///    factura (`ResponseCode` 02) y referirse a su CUFE.
    pub async fn register_verified(&self, tenant: &Tenant, attached_xml: String) -> Result<InboundRegistered, DomainError> {
        let verification = self
            .verification
            .as_ref()
            .ok_or_else(|| DomainError::Internal("Inbound signature verification is not configured".to_string()))?;
        let verify = |what: &'static str, xml: String| async move {
            verification
                .verifier
                .verify(&xml)
                .await
                .map_err(|e| match e {
                    DomainError::Validation(reason) => invalid(format!("Firma invalida en {}: {}", what, reason)),
                    other => other,
                })
        };

        let container = XmlDoc::parse(&attached_xml).map_err(invalid)?;
        if container.root() != "AttachedDocument" {
            return Err(invalid("El documento recibido no es un AttachedDocument"));
        }
        let invoice_xml = required(&container, ATTACHED_INVOICE, "la factura embebida en el AttachedDocument")?;
        let response_xml = required(&container, ATTACHED_RESPONSE, "el ApplicationResponse de la DIAN en el AttachedDocument")?;

        verify("el AttachedDocument", attached_xml.clone()).await?;
        let invoice_signature = verify("la factura", invoice_xml.clone()).await?;
        let response_signature = verify("el ApplicationResponse", response_xml.clone()).await?;
        if !response_signature.signer_subject.contains(&verification.dian_signer_subject) {
            return Err(invalid(format!(
                "El ApplicationResponse no esta firmado por la DIAN (firmante: {})",
                response_signature.signer_subject
            )));
        }

        let invoice = XmlDoc::parse(&invoice_xml).map_err(|e| invalid(format!("Factura embebida: {}", e)))?;
        if invoice.root() != "Invoice" {
            return Err(invalid("El documento embebido no es una factura (Invoice)"));
        }
        let response = XmlDoc::parse(&response_xml).map_err(|e| invalid(format!("ApplicationResponse: {}", e)))?;
        if response.root() != "ApplicationResponse" {
            return Err(invalid("La respuesta embebida no es un ApplicationResponse"));
        }

        let invoice_id = required(&invoice, &["Invoice", "ID"], "el numero de la factura")?;
        let cufe = required(&invoice, &["Invoice", "UUID"], "el CUFE de la factura")?;
        let sender_nit = required(&invoice, &path(SUPPLIER, "CompanyID"), "el NIT del emisor en la factura")?;
        let sender_name = required(&invoice, &path(SUPPLIER, "RegistrationName"), "el nombre del emisor en la factura")?;
        let receiver_nit = required(&invoice, &path(CUSTOMER, "CompanyID"), "el NIT del adquirente en la factura")?;
        let receiver_name = invoice.text(&path(CUSTOMER, "RegistrationName")).unwrap_or_default();

        let response_code = required(
            &response,
            &["ApplicationResponse", "DocumentResponse", "Response", "ResponseCode"],
            "el codigo de respuesta de la DIAN",
        )?;
        let validated_cufe = required(
            &response,
            &["ApplicationResponse", "DocumentResponse", "DocumentReference", "UUID"],
            "el CUFE validado por la DIAN",
        )?;
        if response_code != "02" {
            return Err(invalid(format!(
                "La DIAN no valido la factura (ResponseCode {}; se esperaba 02)",
                response_code
            )));
        }
        if !validated_cufe.eq_ignore_ascii_case(&cufe) {
            return Err(invalid("El ApplicationResponse de la DIAN corresponde a otra factura (CUFE distinto)"));
        }
        if container.text(&["AttachedDocument", "ParentDocumentID"]).as_deref() != Some(invoice_id.as_str()) {
            return Err(invalid("El ParentDocumentID del contenedor no coincide con el numero de la factura"));
        }
        if receiver_nit != tenant.tax_id {
            return Err(DomainError::Forbidden(format!(
                "El adquirente de la factura ({}) no es el tenant autenticado",
                receiver_nit
            )));
        }
        let (prefix, number) = split_document_id(&invoice_id).ok_or_else(|| {
            invalid(format!("El numero de factura '{}' no tiene un consecutivo numerico valido", invoice_id))
        })?;

        let id = Uuid::new_v4();
        let inserted = self
            .documents
            .insert_inbound(NewInboundDocument {
                id,
                tenant_id: tenant.id,
                issuer_nit: sender_nit.clone(),
                document_type: DocumentType::Invoice,
                prefix,
                document_number: number,
                cufe: cufe.clone(),
                payload: json!({
                    "inbound": true,
                    "signature_verified": true,
                    "parent_document_id": invoice_id,
                    "cufe": cufe,
                    "issue_date": invoice.text(&["Invoice", "IssueDate"]),
                    "payable_amount": invoice.text(&["Invoice", "LegalMonetaryTotal", "PayableAmount"]),
                    "sender_nit": sender_nit,
                    "sender_name": sender_name,
                    "receiver_nit": receiver_nit,
                    "receiver_name": receiver_name,
                    "invoice_signer": invoice_signature.signer_subject,
                    "dian_signer": response_signature.signer_subject
                }),
                signed_xml: attached_xml,
                status: DocumentStatus::DianAccepted,
            })
            .await?;
        if !inserted {
            return Err(DomainError::Conflict(format!(
                "La factura {} del emisor {} ya estaba registrada",
                invoice_id, sender_nit
            )));
        }
        Ok(InboundRegistered {
            document_id: id,
            parent_document_id: invoice_id,
        })
    }

    /// Registra el AttachedDocument como documento recibido **no verificado**: la firma solo
    /// se comprueba por presencia, por lo que no admite eventos RADIAN.
    pub async fn register_unverified(&self, tenant: &Tenant, xml: String) -> Result<InboundRegistered, DomainError> {
        if !xml.contains("<ds:Signature") && !xml.contains("<Signature") {
            return Err(invalid("El AttachedDocument XML no esta firmado digitalmente"));
        }
        if tag_content(&xml, "ResponseCode").as_deref() != Some("02") {
            return Err(invalid(
                "El AttachedDocument no contiene el codigo de aceptacion reglamentario de la DIAN ('02')",
            ));
        }

        let party = |section: &str, tag: &str| {
            tag_in_section(&xml, &format!("<cac:{}>", section), &format!("</cac:{}>", section), tag)
        };
        let sender_nit = party("SenderParty", "CompanyID")
            .ok_or_else(|| invalid("No se encontro el NIT del emisor en el AttachedDocument"))?;
        let sender_name = party("SenderParty", "RegistrationName")
            .or_else(|| party("SenderParty", "Name"))
            .unwrap_or_else(|| "Proveedor Inbound".to_string());
        let receiver_nit = party("ReceiverParty", "CompanyID")
            .ok_or_else(|| invalid("No se encontro el NIT del receptor en el AttachedDocument"))?;
        let receiver_name = party("ReceiverParty", "RegistrationName")
            .or_else(|| party("ReceiverParty", "Name"))
            .unwrap_or_else(|| "Tenant Receptor".to_string());
        let parent_document_id = tag_content(&xml, "ParentDocumentID")
            .ok_or_else(|| invalid("No se encontro el ParentDocumentID (Numero de factura)"))?;
        let cufe = tag_content(&xml, "UUID")
            .ok_or_else(|| invalid("No se encontro el CUFE/UUID de la factura original"))?;

        if receiver_nit != tenant.tax_id {
            return Err(DomainError::Forbidden(format!(
                "El NIT receptor del documento ({}) no coincide con el del tenant autenticado",
                receiver_nit
            )));
        }
        let (prefix, number) = split_document_id(&parent_document_id).ok_or_else(|| {
            invalid(format!(
                "El ParentDocumentID '{}' no tiene un consecutivo numerico valido",
                parent_document_id
            ))
        })?;

        let id = Uuid::new_v4();
        let inserted = self
            .documents
            .insert_inbound(NewInboundDocument {
                id,
                tenant_id: tenant.id,
                issuer_nit: sender_nit.clone(),
                document_type: DocumentType::Invoice,
                prefix,
                document_number: number,
                cufe: cufe.clone(),
                payload: json!({
                    "inbound": true,
                    "signature_verified": false,
                    "parent_document_id": parent_document_id,
                    "cufe": cufe,
                    "sender_nit": sender_nit,
                    "sender_name": sender_name,
                    "receiver_nit": receiver_nit,
                    "receiver_name": receiver_name
                }),
                signed_xml: xml,
                status: DocumentStatus::DianAccepted,
            })
            .await?;
        if !inserted {
            return Err(DomainError::Conflict(format!(
                "La factura {} del emisor {} ya estaba registrada",
                parent_document_id, sender_nit
            )));
        }
        Ok(InboundRegistered {
            document_id: id,
            parent_document_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    fn attached_document(sender: &str, receiver: &str, parent_id: &str) -> String {
        format!(
            "<AttachedDocument><ds:Signature/><cac:SenderParty><cbc:RegistrationName>Prov</cbc:RegistrationName>\
             <cbc:CompanyID>{sender}</cbc:CompanyID></cac:SenderParty><cac:ReceiverParty>\
             <cbc:CompanyID>{receiver}</cbc:CompanyID></cac:ReceiverParty><cbc:ParentDocumentID>{parent_id}</cbc:ParentDocumentID>\
             <cbc:UUID>cufe-1</cbc:UUID><cbc:ResponseCode>02</cbc:ResponseCode></AttachedDocument>"
        )
    }

    #[tokio::test]
    async fn registers_unverified_and_rejects_duplicates() {
        let (store, tenant) = store_with_tenant();
        let service = InboundService::new(store.clone(), None);
        let xml = attached_document("800111222", &tenant.tax_id, "FEV-7");

        let registered = service.register_unverified(&tenant, xml.clone()).await.unwrap();
        let doc = store.document(registered.document_id).unwrap();
        assert_eq!(doc.issuer_nit, "800111222");
        assert_eq!(doc.payload["signature_verified"], false);

        assert!(matches!(service.register_unverified(&tenant, xml).await, Err(DomainError::Conflict(_))));
        let other_supplier = attached_document("811222333", &tenant.tax_id, "FEV-7");
        assert!(service.register_unverified(&tenant, other_supplier).await.is_ok());
    }

    #[tokio::test]
    async fn rejects_documents_for_another_receiver_or_without_number() {
        let (store, tenant) = store_with_tenant();
        let service = InboundService::new(store, None);
        assert!(matches!(
            service.register_unverified(&tenant, attached_document("8001", "999999999", "FEV-7")).await,
            Err(DomainError::Forbidden(_))
        ));
        assert!(matches!(
            service.register_unverified(&tenant, attached_document("8001", &tenant.tax_id, "FEV-ABC")).await,
            Err(DomainError::Validation(_))
        ));
    }
}

/// Verificación con firmas reales: los documentos se generan con `ubl-engine`, se firman con
/// los certificados de prueba de `crypto-signer` y se verifican con `xmlsec1`.
#[cfg(test)]
mod verified_tests {
    use super::*;
    use crate::test_support::*;
    use crypto_signer::provider::LocalCertificateProvider;
    use crypto_signer::sign_document;
    use crypto_signer::verify::XmlSignatureVerifier;
    use ubl_engine::payload::InvoicePayload;

    const SUPPLIER_NIT: &str = "800111222";

    fn fixture(name: &str) -> String {
        format!("{}/../crypto-signer/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name)
    }

    fn provider(name: &str) -> LocalCertificateProvider {
        let read = |suffix: &str| std::fs::read_to_string(fixture(&format!("{}_{}.pem", name, suffix))).unwrap();
        LocalCertificateProvider::from_pem(read("cert"), read("key")).unwrap()
    }

    fn xmlsec1_available() -> bool {
        if std::process::Command::new("xmlsec1").arg("--version").output().is_ok() {
            return true;
        }
        assert!(std::env::var_os("CI").is_none(), "xmlsec1 is required in CI");
        eprintln!("xmlsec1 not installed: skipping verified inbound tests");
        false
    }

    fn supplier_invoice(customer_nit: &str) -> InvoicePayload {
        InvoicePayload {
            company_nit: SUPPLIER_NIT.into(),
            company_name: "Proveedor SAS".into(),
            customer_id: customer_nit.into(),
            customer_name: "Tenant SAS".into(),
            prefix: "FEV".into(),
            number: 7,
            ..invoice_payload(7)
        }
    }

    /// Partes del contenedor que cada caso puede alterar.
    struct Scenario {
        customer_nit: String,
        invoice_signer: &'static str,
        response_signer: &'static str,
        response_code: &'static str,
        response_cufe: Option<String>,
        tamper_invoice: bool,
        sign_container: bool,
    }

    impl Default for Scenario {
        fn default() -> Self {
            Self {
                customer_nit: "900123456".into(),
                invoice_signer: "test",
                response_signer: "dian",
                response_code: "02",
                response_cufe: None,
                tamper_invoice: false,
                sign_container: true,
            }
        }
    }

    async fn attached_document(s: Scenario) -> String {
        let payload = supplier_invoice(&s.customer_nit);
        let (invoice, cufe) = ubl_engine::generate_invoice_xml(&payload, "tk").unwrap();
        let mut signed_invoice = sign_document(&invoice, &provider(s.invoice_signer), "local").await.unwrap();
        if s.tamper_invoice {
            signed_invoice = signed_invoice.replace("119000.00", "1190.00");
        }
        let (response, _) = ubl_engine::generate_application_response_xml(
            "RESP-1", "2026-10-23", "10:50:00-05:00", "800197268", "DIAN", "31", SUPPLIER_NIT,
            "Proveedor SAS", "31", s.response_code, "Documento validado por la DIAN", "FEV7",
            s.response_cufe.as_deref().unwrap_or(&cufe), "pin", "2",
        )
        .unwrap();
        let signed_response = sign_document(&response, &provider(s.response_signer), "local").await.unwrap();
        let container = ubl_engine::generate_attached_document_xml(
            &payload, "ATT-1", "2026-10-23", "11:00:00-05:00", &cufe, &signed_invoice, &signed_response,
        )
        .unwrap();
        if s.sign_container {
            sign_document(&container, &provider("test"), "local").await.unwrap()
        } else {
            container.replace("<!-- PLACEHOLDER_FOR_SIGNATURE -->", "")
        }
    }

    fn service(store: Arc<core_domain::testing::InMemoryStore>) -> InboundService {
        InboundService::new(
            store,
            Some(InboundVerification {
                verifier: Arc::new(XmlSignatureVerifier::new(fixture("test_ca.pem"))),
                dian_signer_subject: "800197268".into(),
            }),
        )
    }

    async fn rejection(s: Scenario) -> DomainError {
        let (store, tenant) = store_with_tenant();
        service(store).register_verified(&tenant, attached_document(s).await).await.unwrap_err()
    }

    #[tokio::test]
    async fn registers_a_genuine_invoice_with_data_from_the_signed_invoice() {
        if !xmlsec1_available() {
            return;
        }
        let (store, tenant) = store_with_tenant();
        let registered = service(store.clone())
            .register_verified(&tenant, attached_document(Scenario::default()).await)
            .await
            .unwrap();

        assert_eq!(registered.parent_document_id, "FEV7");
        let doc = store.document(registered.document_id).unwrap();
        assert_eq!(doc.issuer_nit, SUPPLIER_NIT);
        assert_eq!((doc.prefix.as_str(), doc.document_number), ("FEV", 7));
        assert_eq!(doc.payload["signature_verified"], true);
        assert_eq!(doc.payload["payable_amount"], "119000.00");
        assert!(doc.payload["dian_signer"].as_str().unwrap().contains("800197268"));
    }

    #[tokio::test]
    async fn rejects_forged_or_inconsistent_containers() {
        if !xmlsec1_available() {
            return;
        }
        let cases: Vec<(&str, Scenario)> = vec![
            ("response not signed by the DIAN", Scenario { response_signer: "test", ..Default::default() }),
            ("response signed by an untrusted CA", Scenario { response_signer: "untrusted", ..Default::default() }),
            ("invoice signed by an untrusted CA", Scenario { invoice_signer: "untrusted", ..Default::default() }),
            ("invoice altered after signing", Scenario { tamper_invoice: true, ..Default::default() }),
            ("DIAN rejected the invoice", Scenario { response_code: "04", ..Default::default() }),
            ("response for another invoice", Scenario { response_cufe: Some("ab".repeat(48)), ..Default::default() }),
            ("unsigned container", Scenario { sign_container: false, ..Default::default() }),
        ];
        for (name, scenario) in cases {
            let error = rejection(scenario).await;
            assert!(matches!(error, DomainError::Validation(_)), "{}: {:?}", name, error);
        }
        let other_customer = rejection(Scenario { customer_nit: "811000111".into(), ..Default::default() }).await;
        assert!(matches!(other_customer, DomainError::Forbidden(_)), "{:?}", other_customer);
    }
}

//! Firma los documentos que generan las plantillas reales de `ubl-engine` y los verifica con
//! `xmlsec1` (implementación independiente) contra la CA de prueba.

use crypto_signer::provider::LocalCertificateProvider;
use crypto_signer::sign_document;
use crypto_signer::verify::XmlSignatureVerifier;
use ubl_engine::payload::{InvoicePayload, PayrollPayload, SupportDocumentPayload};

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn provider() -> LocalCertificateProvider {
    LocalCertificateProvider::from_pem(
        std::fs::read_to_string(fixture("test_cert.pem")).unwrap(),
        std::fs::read_to_string(fixture("test_key.pem")).unwrap(),
    )
    .unwrap()
}

fn invoice() -> InvoicePayload {
    InvoicePayload {
        prefix: "FE".into(),
        number: 1002,
        issue_date: "2026-10-23".into(),
        issue_time: "10:42:00-05:00".into(),
        company_nit: "900123456".into(),
        company_name: "Empresa & Cia <SAS>".into(),
        customer_id_type: "31".into(),
        customer_id: "900999888".into(),
        customer_name: "Cliente Ñandú SAS".into(),
        customer_email: "c@x.co".into(),
        net_amount: 100000.0,
        tax_amount: 19000.0,
        total_amount: 119000.0,
        environment: "2".into(),
        technical_key: None,
        document_type: "invoice".into(),
        test_set_id: None,
    }
}

async fn sign_and_verify(name: &str, xml: &str) {
    let signed = sign_document(xml, &provider(), "local").await.unwrap();
    if std::process::Command::new("xmlsec1").arg("--version").output().is_err() {
        eprintln!("xmlsec1 not installed: skipping verification of {}", name);
        return;
    }
    XmlSignatureVerifier::new(fixture("test_ca.pem"))
        .verify(&signed)
        .unwrap_or_else(|e| panic!("{} does not verify: {}", name, e));
}

#[tokio::test]
async fn every_document_type_verifies_independently() {
    let (invoice_xml, _) = ubl_engine::generate_invoice_xml(&invoice(), "tk").unwrap();
    sign_and_verify("invoice", &invoice_xml).await;

    let (support_xml, _) = ubl_engine::generate_support_document_xml(&SupportDocumentPayload {
        prefix: "DS".into(),
        number: 5000,
        issue_date: "2026-10-23".into(),
        issue_time: "10:30:00-05:00".into(),
        seller_nit: "10203040".into(),
        seller_name: "No Obligado".into(),
        buyer_nit: "900123456".into(),
        buyer_name: "Comprador SAS".into(),
        net_amount: 100000.0,
        tax_amount: 19000.0,
        total_amount: 119000.0,
        software_pin: "pin".into(),
        environment: "2".into(),
    })
    .unwrap();
    sign_and_verify("support document", &support_xml).await;

    let (payroll_xml, _) = ubl_engine::generate_payroll_xml(&PayrollPayload {
        prefix: "NOM".into(),
        number: 1,
        issue_date: "2026-10-23".into(),
        issue_time: "08:00:00".into(),
        employer_nit: "900123456".into(),
        employer_name: "Empresa".into(),
        employee_id_type: "13".into(),
        employee_id: "10203040".into(),
        employee_name: "Trabajador".into(),
        devengado: 1500000.0,
        deducido: 60000.0,
        total: 1440000.0,
        software_pin: "pin".into(),
        environment: "2".into(),
    })
    .unwrap();
    sign_and_verify("payroll", &payroll_xml).await;

    let (event_xml, _) = ubl_engine::generate_application_response_xml(
        "EV-1", "2026-10-23", "12:00:00-05:00", "900999888", "Cliente", "31", "900123456",
        "Emisor", "31", "030", "Acuse de recibo", "FE1002", "cufe", "pin", "2",
    )
    .unwrap();
    sign_and_verify("application response", &event_xml).await;

    let signed_invoice = sign_document(&invoice_xml, &provider(), "local").await.unwrap();
    let attached = ubl_engine::generate_attached_document_xml(
        &invoice(), "ATT-1", "2026-10-23", "11:00:00-05:00", "cufe", &signed_invoice, "<ApplicationResponse/>",
    )
    .unwrap();
    sign_and_verify("attached document (with embedded signed invoice)", &attached).await;
}

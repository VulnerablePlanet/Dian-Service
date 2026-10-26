//! Valida el XML generado contra los esquemas oficiales UBL 2.1 (tests/xsd) con `xmllint`.
//!
//! Se omite con un aviso si `xmllint` no está instalado (en CI sí lo está). La firma se
//! sustituye por un elemento de prueba: `ext:ExtensionContent` exige exactamente un
//! elemento de otro espacio de nombres y su contenido no lo valida UBL.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use ubl_engine::payload::{
    BillingReferencePayload, DiscrepancyPayload, InvoicePayload, LinePayload, SupportDocumentPayload,
};

fn xsd(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/xsd/ubl21/maindoc").join(name)
}

fn xmllint_available() -> bool {
    Command::new("xmllint").arg("--version").output().is_ok()
}

/// Devuelve `Err(salida de xmllint)` si el documento no es válido.
fn validate(xml: &str, schema: &str) -> Result<(), String> {
    let xml = xml.replace(
        "<!-- PLACEHOLDER_FOR_SIGNATURE -->",
        r#"<test:Signature xmlns:test="urn:test:signature"/>"#,
    );
    let dir = std::env::temp_dir().join(format!("ubl-xsd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let file = dir.join(format!("{}-{}.xml", schema, COUNTER.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&file, xml).unwrap();
    let output = Command::new("xmllint")
        .arg("--noout")
        .arg("--schema")
        .arg(xsd(schema))
        .arg(&file)
        .output()
        .expect("run xmllint");
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

macro_rules! require_xmllint {
    () => {
        if !xmllint_available() {
            assert!(std::env::var_os("CI").is_none(), "xmllint is required in CI");
            eprintln!("xmllint not installed: skipping UBL schema validation");
            return;
        }
    };
}

pub fn invoice() -> InvoicePayload {
    InvoicePayload {
        prefix: "FE".into(),
        number: 1002,
        issue_date: "2026-10-23".into(),
        issue_time: "10:42:00-05:00".into(),
        company_nit: "900123456".into(),
        company_name: "Empresa Emisora & Cia <SAS>".into(),
        customer_id_type: "31".into(),
        customer_id: "900999888".into(),
        customer_name: "Cliente SAS".into(),
        customer_email: "c@x.co".into(),
        net_amount: 100000.0,
        tax_amount: 19000.0,
        total_amount: 119000.0,
        environment: "2".into(),
        technical_key: None,
        document_type: "invoice".into(),
        test_set_id: None,
        lines: None,
        billing_reference: None,
        discrepancy: None,
        software_pin: None,
    }
}

#[test]
fn invoice_is_valid_ubl() {
    require_xmllint!();
    let (xml, _) = ubl_engine::generate_invoice_xml(&invoice(), "tk").unwrap();
    validate(&xml, "UBL-Invoice-2.1.xsd").unwrap();
}

#[test]
fn support_document_is_valid_ubl() {
    require_xmllint!();
    let payload = SupportDocumentPayload {
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
        lines: None,
    };
    let (xml, _) = ubl_engine::generate_support_document_xml(&payload).unwrap();
    validate(&xml, "UBL-Invoice-2.1.xsd").unwrap();
}

#[test]
fn application_response_is_valid_ubl() {
    require_xmllint!();
    let (xml, _) = ubl_engine::generate_application_response_xml(
        "EV-1", "2026-10-23", "12:00:00-05:00", "900999888", "Cliente SAS", "31", "900123456",
        "Empresa Emisora SAS", "31", "030", "Acuse de recibo", "FE1002", "cufe", "pin", "2",
    )
    .unwrap();
    validate(&xml, "UBL-ApplicationResponse-2.1.xsd").unwrap();
}

#[test]
fn attached_document_is_valid_ubl() {
    require_xmllint!();
    let xml = ubl_engine::generate_attached_document_xml(
        &invoice(), "ATT-1", "2026-10-23", "11:00:00-05:00", "cufe", "<Invoice/>", "<ApplicationResponse/>",
    )
    .unwrap();
    validate(&xml, "UBL-AttachedDocument-2.1.xsd").unwrap();
}


fn line(description: &str, quantity: f64, unit_price: f64, tax_percent: f64) -> LinePayload {
    LinePayload { description: description.into(), quantity, unit_price, tax_percent, unit_code: None }
}

/// Factura con varias líneas y tarifas: 2 x 50.000 al 19 % + 1 x 10.000 exento + 0,5 x 3.000 al 5 %.
fn multi_line_invoice() -> InvoicePayload {
    InvoicePayload {
        net_amount: 111500.0,
        tax_amount: 19075.0,
        total_amount: 130575.0,
        lines: Some(vec![
            line("Servicio <A> & B", 2.0, 50000.0, 19.0),
            line("Libro exento", 1.0, 10000.0, 0.0),
            line("Producto al 5 %", 0.5, 3000.0, 5.0),
        ]),
        ..invoice()
    }
}

fn note(document_type: &str) -> InvoicePayload {
    InvoicePayload {
        document_type: document_type.into(),
        prefix: if document_type == "credit_note" { "NC".into() } else { "ND".into() },
        number: 7,
        net_amount: 10000.0,
        tax_amount: 1900.0,
        total_amount: 11900.0,
        lines: Some(vec![line("Ajuste", 1.0, 10000.0, 19.0)]),
        billing_reference: Some(BillingReferencePayload {
            number: "FE1002".into(),
            cufe: "a".repeat(96),
            issue_date: "2026-10-20".into(),
        }),
        discrepancy: Some(DiscrepancyPayload { code: "1".into(), description: "Devolucion parcial".into() }),
        software_pin: Some("12345".into()),
        ..invoice()
    }
}

#[test]
fn multi_line_invoice_is_valid_ubl() {
    require_xmllint!();
    let (xml, _) = ubl_engine::generate_invoice_xml(&multi_line_invoice(), "tk").unwrap();
    assert_eq!(xml.matches("<cac:InvoiceLine>").count(), 3);
    // Un subtotal por tarifa (0 %, 5 %, 19 %) en el total del documento, más uno por línea.
    assert_eq!(xml.matches("<cac:TaxSubtotal>").count(), 6);
    assert!(xml.contains("<cbc:LineCountNumeric>3</cbc:LineCountNumeric>"));
    validate(&xml, "UBL-Invoice-2.1.xsd").unwrap();
}

#[test]
fn credit_note_is_valid_ubl() {
    require_xmllint!();
    let (xml, cude) = ubl_engine::generate_invoice_xml(&note("credit_note"), "tk").unwrap();
    assert!(xml.contains("<CreditNote"));
    assert!(xml.contains("<cbc:CreditNoteTypeCode>91</cbc:CreditNoteTypeCode>"));
    assert!(xml.contains(&format!("schemeName=\"CUDE-SHA384\">{}", cude)));
    assert!(xml.contains("<cbc:ID>FE1002</cbc:ID>"));
    validate(&xml, "UBL-CreditNote-2.1.xsd").unwrap();
}

#[test]
fn debit_note_is_valid_ubl() {
    require_xmllint!();
    let (xml, _) = ubl_engine::generate_invoice_xml(&note("debit_note"), "tk").unwrap();
    assert!(xml.contains("<DebitNote"));
    assert!(xml.contains("<cac:RequestedMonetaryTotal>"));
    validate(&xml, "UBL-DebitNote-2.1.xsd").unwrap();
}

#[test]
fn note_cude_uses_the_software_pin_not_the_technical_key() {
    let (_, a) = ubl_engine::generate_invoice_xml(&note("credit_note"), "tk-1").unwrap();
    let (_, b) = ubl_engine::generate_invoice_xml(&note("credit_note"), "tk-2").unwrap();
    assert_eq!(a, b);
    let mut other_pin = note("credit_note");
    other_pin.software_pin = Some("99999".into());
    let (_, c) = ubl_engine::generate_invoice_xml(&other_pin, "tk-1").unwrap();
    assert_ne!(a, c);
}

/// Control del propio validador: un documento sin líneas debe ser rechazado.
#[test]
fn the_validator_rejects_invalid_documents() {
    require_xmllint!();
    let (xml, _) = ubl_engine::generate_invoice_xml(&invoice(), "tk").unwrap();
    let start = xml.find("<cac:InvoiceLine>").unwrap();
    let end = xml.rfind("</cac:InvoiceLine>").unwrap() + "</cac:InvoiceLine>".len();
    let broken = format!("{}{}", &xml[..start], &xml[end..]);
    let error = validate(&broken, "UBL-Invoice-2.1.xsd").unwrap_err();
    assert!(error.contains("InvoiceLine"), "{}", error);
}


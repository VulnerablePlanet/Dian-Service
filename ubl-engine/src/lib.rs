pub mod cufe;
pub mod error;
pub mod model;
pub mod packaging;
pub mod payload;
pub mod templates;

use cufe::{calculate_cufe_cude, calculate_cune, calculate_cuds};
use error::UblError;
use minijinja::context;
use model::{compute_lines, tax_subtotals, CommercialDocument, PartyData};
use payload::{InvoicePayload, LinePayload, PayrollPayload, SupportDocumentPayload};
use rust_decimal::Decimal;
use templates::engine;

/// Formatea un monto con dos decimales exactos a partir de su valor decimal (no del binario
/// aproximado del `f64`). Los montos ya fueron validados por `validate()`.
pub(crate) fn format_amount(amount: f64) -> String {
    core_domain::money::to_amount("amount", amount)
        .map(core_domain::money::format_amount)
        .unwrap_or_else(|_| format!("{:.2}", amount))
}

fn decimal_amount(field: &str, value: f64) -> Result<Decimal, UblError> {
    core_domain::money::to_amount(field, value).map_err(|e| UblError::Validation(e.to_string()))
}

/// Datos comunes de un documento comercial (factura, nota o documento soporte).
struct CommercialInput<'a> {
    customization_id: &'a str,
    type_code: &'a str,
    uuid: String,
    uuid_scheme: &'a str,
    id: String,
    issue_date: &'a str,
    issue_time: &'a str,
    environment: &'a str,
    supplier: PartyData,
    customer: PartyData,
    lines: Option<&'a [LinePayload]>,
    net_amount: f64,
    tax_amount: f64,
    total_amount: f64,
    billing_reference: Option<payload::BillingReferencePayload>,
    discrepancy: Option<payload::DiscrepancyPayload>,
}

fn commercial_document(input: CommercialInput<'_>) -> Result<CommercialDocument, UblError> {
    let net = decimal_amount("net_amount", input.net_amount)?;
    let tax = decimal_amount("tax_amount", input.tax_amount)?;
    let total = decimal_amount("total_amount", input.total_amount)?;
    Ok(CommercialDocument {
        customization_id: input.customization_id.to_string(),
        profile_execution_id: input.environment.to_string(),
        id: input.id,
        uuid: input.uuid,
        uuid_scheme: input.uuid_scheme.to_string(),
        issue_date: input.issue_date.to_string(),
        issue_time: input.issue_time.to_string(),
        type_code: input.type_code.to_string(),
        supplier: input.supplier,
        customer: input.customer,
        lines: compute_lines(input.lines, net, tax)?,
        tax_subtotals: tax_subtotals(input.lines, net, tax)?,
        net: core_domain::money::format_amount(net),
        tax: core_domain::money::format_amount(tax),
        total: core_domain::money::format_amount(total),
        billing_reference: input.billing_reference,
        discrepancy: input.discrepancy,
    })
}

/// Genera el XML UBL 2.1 sin firmar de una factura o de una nota crédito/débito y calcula su
/// código único, según `payload.document_type`:
/// - `invoice`: `Invoice` con CUFE (clave técnica `technical_key` de la resolución).
/// - `credit_note` / `debit_note`: `CreditNote` / `DebitNote` con CUDE (PIN del software) y
///   referencia a la factura que corrigen; `technical_key` no interviene.
///
/// Retorna `(xml, cufe_o_cude)`.
pub fn generate_invoice_xml(payload: &InvoicePayload, technical_key: &str) -> Result<(String, String), UblError> {
    payload.validate()?;

    let (template, customization_id, type_code, uuid_scheme, key) = match payload.document_type.as_str() {
        "invoice" => ("invoice.xml", "10", "01", "CUFE-SHA384", technical_key),
        "credit_note" => ("credit_note.xml", "20", "91", "CUDE-SHA384", payload.software_pin.as_deref().unwrap_or_default()),
        "debit_note" => ("debit_note.xml", "30", "92", "CUDE-SHA384", payload.software_pin.as_deref().unwrap_or_default()),
        other => return Err(UblError::Validation(format!("Tipo de documento no soportado: {}", other))),
    };

    // CUFE y CUDE comparten fórmula (SHA-384); cambia la clave: técnica o PIN del software.
    let id = format!("{}{}", payload.prefix, payload.number);
    let code = calculate_cufe_cude(
        &id,
        &payload.issue_date,
        &payload.issue_time,
        payload.net_amount,
        "01", // IVA
        payload.tax_amount,
        "04", // INC
        0.00,
        "03", // ICA
        0.00,
        payload.total_amount,
        &payload.company_nit,
        &payload.customer_id,
        key,
        &payload.environment,
    );

    let document = commercial_document(CommercialInput {
        customization_id,
        type_code,
        uuid: code.clone(),
        uuid_scheme,
        id,
        issue_date: &payload.issue_date,
        issue_time: &payload.issue_time,
        environment: &payload.environment,
        supplier: PartyData::new("31", &payload.company_nit, &payload.company_name),
        customer: PartyData::new(&payload.customer_id_type, &payload.customer_id, &payload.customer_name),
        lines: payload.lines.as_deref(),
        net_amount: payload.net_amount,
        tax_amount: payload.tax_amount,
        total_amount: payload.total_amount,
        billing_reference: payload.billing_reference.clone(),
        discrepancy: payload.discrepancy.clone(),
    })?;

    let xml = engine().render(template, context! { doc => document })?;
    Ok((xml, code))
}

/// Genera el XML de AttachedDocument para el contenedor electrónico.
#[allow(clippy::too_many_arguments)]
pub fn generate_attached_document_xml(
    payload: &InvoicePayload,
    unique_id: &str,
    issue_date: &str,
    issue_time: &str,
    cufe: &str,
    signed_invoice_xml: &str,
    dian_response_xml: &str,
) -> Result<String, UblError> {
    engine().render(
        "attached_document.xml",
        context! {
            unique_id,
            issue_date,
            issue_time,
            parent_document_id => format!("{}{}", payload.prefix, payload.number),
            parent_document_uuid => cufe,
            parent_document_issue_date => &payload.issue_date,
            sender => PartyData::new("31", &payload.company_nit, &payload.company_name),
            receiver => PartyData::new(&payload.customer_id_type, &payload.customer_id, &payload.customer_name),
            signed_invoice_xml,
            dian_response_xml,
        },
    )
}

/// Genera el XML de ApplicationResponse para eventos RADIAN.
#[allow(clippy::too_many_arguments)]
pub fn generate_application_response_xml(
    id: &str,
    issue_date: &str,
    issue_time: &str,
    sender_nit: &str,
    sender_name: &str,
    sender_id_type: &str,
    receiver_nit: &str,
    receiver_name: &str,
    receiver_id_type: &str,
    event_code: &str,
    event_description: &str,
    parent_document_id: &str,
    parent_document_uuid: &str,
    software_pin: &str,
    environment: &str,
) -> Result<(String, String), UblError> {
    let cude = cufe::calculate_cude_event(
        id,
        issue_date,
        issue_time,
        sender_nit,
        receiver_nit,
        software_pin,
        environment,
    );

    let xml = engine().render(
        "application_response.xml",
        context! {
            id,
            cude => &cude,
            issue_date,
            issue_time,
            environment,
            sender => PartyData::new(sender_id_type, sender_nit, sender_name),
            receiver => PartyData::new(receiver_id_type, receiver_nit, receiver_name),
            event_code,
            event_description,
            parent_document_id,
            parent_document_uuid,
        },
    )?;

    Ok((xml, cude))
}

/// Genera el XML de Nomina Electronica y calcula el CUNE.
pub fn generate_payroll_xml(payload: &PayrollPayload) -> Result<(String, String), UblError> {
    payload.validate()?;

    // Por defecto Nomina Individual con TipoXML = "102"
    let cune = calculate_cune(
        &format!("{}{}", payload.prefix, payload.number),
        &payload.issue_date,
        &payload.issue_time,
        payload.devengado,
        payload.deducido,
        payload.total,
        &payload.employer_nit,
        &payload.employee_id,
        "102",
        &payload.software_pin,
        &payload.environment,
    );

    let xml = engine().render(
        "payroll.xml",
        context! {
            Prefix => &payload.prefix,
            Number => payload.number,
            CUNE => &cune,
            IssueDate => &payload.issue_date,
            IssueTime => &payload.issue_time,
            EmployerNIT => &payload.employer_nit,
            EmployerName => &payload.employer_name,
            EmployeeID => &payload.employee_id,
            EmployeeName => &payload.employee_name,
            Devengado => format_amount(payload.devengado),
            Deducido => format_amount(payload.deducido),
            Total => format_amount(payload.total),
        },
    )?;

    Ok((xml, cune))
}

/// Genera el XML de Documento Soporte (un `Invoice` con tipo 05) y calcula el CUDS.
pub fn generate_support_document_xml(payload: &SupportDocumentPayload) -> Result<(String, String), UblError> {
    payload.validate()?;

    // Codigo de impuesto por defecto IVA = "01" (con valor tax_amount) o "00" (sin impuesto)
    let cod_imp = if payload.tax_amount > 0.0 { "01" } else { "00" };
    let id = format!("{}{}", payload.prefix, payload.number);

    let cuds = calculate_cuds(
        &id,
        &payload.issue_date,
        &payload.issue_time,
        payload.net_amount,
        cod_imp,
        payload.tax_amount,
        payload.total_amount,
        &payload.seller_nit,
        &payload.buyer_nit,
        &payload.software_pin,
        &payload.environment,
    );

    let document = commercial_document(CommercialInput {
        customization_id: "10", // vendedor residente
        type_code: "05",
        uuid: cuds.clone(),
        uuid_scheme: "CUDS-SHA384",
        id,
        issue_date: &payload.issue_date,
        issue_time: &payload.issue_time,
        environment: &payload.environment,
        supplier: PartyData::new("31", &payload.seller_nit, &payload.seller_name),
        customer: PartyData::new("31", &payload.buyer_nit, &payload.buyer_name),
        lines: payload.lines.as_deref(),
        net_amount: payload.net_amount,
        tax_amount: payload.tax_amount,
        total_amount: payload.total_amount,
        billing_reference: None,
        discrepancy: None,
    })?;

    let xml = engine().render("invoice.xml", context! { doc => document })?;
    Ok((xml, cuds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_invoice_xml_success() {
        let payload = InvoicePayload {
            prefix: "FE".to_string(),
            number: 1002,
            issue_date: "2026-05-23".to_string(),
            issue_time: "10:42:00-05:00".to_string(),
            company_nit: "900123456".to_string(),
            company_name: "Empresa Emisora SAS".to_string(),
            customer_id_type: "31".to_string(),
            customer_id: "900999888".to_string(),
            customer_name: "Cliente Receptor SAS".to_string(),
            customer_email: "cliente@receptor.com".to_string(),
            net_amount: 100000.00,
            tax_amount: 19000.00,
            total_amount: 119000.00,
            environment: "2".to_string(),
            technical_key: None,
            document_type: "invoice".to_string(),
            test_set_id: None,
            lines: None,
            billing_reference: None,
            discrepancy: None,
            software_pin: None,
        };

        let result = generate_invoice_xml(&payload, "clave_tecnica_dian");
        assert!(result.is_ok());

        let (xml, cufe) = result.unwrap();
        assert_eq!(cufe.len(), 96);
        assert!(xml.contains("<cbc:UUID"));
        assert!(xml.contains(&cufe));
        assert!(xml.contains("<cbc:ID>FE1002</cbc:ID>"));
    }

    #[test]
    fn test_generate_attached_document_success() {
        let payload = InvoicePayload {
            prefix: "FE".to_string(),
            number: 1002,
            issue_date: "2026-05-23".to_string(),
            issue_time: "10:42:00-05:00".to_string(),
            company_nit: "900123456".to_string(),
            company_name: "Empresa Emisora SAS".to_string(),
            customer_id_type: "31".to_string(),
            customer_id: "900999888".to_string(),
            customer_name: "Cliente Receptor SAS".to_string(),
            customer_email: "cliente@receptor.com".to_string(),
            net_amount: 100000.00,
            tax_amount: 19000.00,
            total_amount: 119000.00,
            environment: "2".to_string(),
            technical_key: None,
            document_type: "invoice".to_string(),
            test_set_id: None,
            lines: None,
            billing_reference: None,
            discrepancy: None,
            software_pin: None,
        };

        let signed_invoice = "<Invoice>signed</Invoice>";
        let dian_response = "<DianResponse>accepted</DianResponse>";

        let att_doc_result = generate_attached_document_xml(
            &payload,
            "ATT-DOC-001",
            "2026-05-23",
            "11:00:00-05:00",
            "my_cufe_hash",
            signed_invoice,
            dian_response,
        );

        assert!(att_doc_result.is_ok());
        let xml = att_doc_result.unwrap();

        assert!(xml.contains("<AttachedDocument"));
        assert!(xml.contains("Factura Electrónica de Venta"));
        assert!(xml.contains("ATT-DOC-001"));
        assert!(xml.contains("&lt;Invoice&gt;signed&lt;&#x2f;Invoice&gt;"));
        assert!(xml.contains("&lt;DianResponse&gt;accepted&lt;&#x2f;DianResponse&gt;"));
        assert!(xml.contains("my_cufe_hash"));
    }

    #[test]
    fn test_generate_application_response_success() {
        let result = generate_application_response_xml(
            "EV1234",
            "2026-05-24",
            "12:00:00-05:00",
            "900999888", // Sender NIT
            "Cliente Receptor SAS", // Sender Name
            "31", // Sender ID Type
            "900123456", // Receiver NIT
            "Empresa Emisora SAS", // Receiver Name
            "31", // Receiver ID Type
            "030", // Event Code
            "Acuse de recibo de Factura Electronica", // Event Desc
            "FE1002", // Parent ID
            "my_invoice_cufe", // Parent CUFE
            "pin_test",
            "2",
        );

        assert!(result.is_ok());
        let (xml, cude) = result.unwrap();
        assert_eq!(cude.len(), 96);
        assert!(xml.contains("<ApplicationResponse"));
        assert!(xml.contains("<cbc:ID>EV1234</cbc:ID>"));
        assert!(xml.contains(&cude));
        assert!(xml.contains("030"));
        assert!(xml.contains("Acuse de recibo de Factura Electronica"));
        assert!(xml.contains("FE1002"));
        assert!(xml.contains("my_invoice_cufe"));
    }

    #[test]
    fn test_generate_payroll_xml_success() {
        let p = PayrollPayload {
            prefix: "NOM".to_string(),
            number: 101,
            issue_date: "2026-05-26".to_string(),
            issue_time: "08:00:00".to_string(),
            employer_nit: "900123456".to_string(),
            employer_name: "Empresa Emisora".to_string(),
            employee_id_type: "13".to_string(),
            employee_id: "10203040".to_string(),
            employee_name: "Trabajador".to_string(),
            devengado: 1500000.00,
            deducido: 60000.00,
            total: 1440000.00,
            software_pin: "pin_test".to_string(),
            environment: "2".to_string(),
        };

        let result = generate_payroll_xml(&p);
        assert!(result.is_ok());
        let (xml, cune) = result.unwrap();
        assert_eq!(cune.len(), 96);
        assert!(xml.contains("<NominaIndividual"));
        assert!(xml.contains(&cune));
        assert!(xml.contains("1500000.00"));
        assert!(xml.contains("60000.00"));
    }

    #[test]
    fn test_generate_support_document_xml_success() {
        let sd = SupportDocumentPayload {
            prefix: "DS".to_string(),
            number: 200,
            issue_date: "2026-05-26".to_string(),
            issue_time: "10:30:00-05:00".to_string(),
            seller_nit: "10203040".to_string(),
            seller_name: "Vendedor No Obligado".to_string(),
            buyer_nit: "900123456".to_string(),
            buyer_name: "Comprador SAS".to_string(),
            net_amount: 100000.00,
            tax_amount: 19000.00,
            total_amount: 119000.00,
            software_pin: "pin_test".to_string(),
            environment: "2".to_string(),
            lines: None,
        };

        let result = generate_support_document_xml(&sd);
        assert!(result.is_ok());
        let (xml, cuds) = result.unwrap();
        assert_eq!(cuds.len(), 96);
        assert!(xml.contains("<Invoice"));
        assert!(xml.contains("<cbc:InvoiceTypeCode>05</cbc:InvoiceTypeCode>"));
        assert!(xml.contains("schemeName=\"CUDS-SHA384\""));
        assert!(xml.contains(&cuds));
        assert!(xml.contains("100000.00"));
        assert!(xml.contains("19000.00"));
    }

    #[test]
    fn test_interpolated_values_are_xml_escaped() {
        let payload = InvoicePayload {
            prefix: "FE".to_string(),
            number: 1002,
            issue_date: "2026-05-23".to_string(),
            issue_time: "10:42:00-05:00".to_string(),
            company_nit: "900123456".to_string(),
            company_name: "A & B <Ltda>".to_string(),
            customer_id_type: "31".to_string(),
            customer_id: "900999888".to_string(),
            customer_name: "</cbc:Name><cbc:Injected>x</cbc:Injected>".to_string(),
            customer_email: "cliente@receptor.com".to_string(),
            net_amount: 100000.00,
            tax_amount: 19000.00,
            total_amount: 119000.00,
            environment: "2".to_string(),
            technical_key: None,
            document_type: "invoice".to_string(),
            test_set_id: None,
            lines: None,
            billing_reference: None,
            discrepancy: None,
            software_pin: None,
        };

        let (xml, _) = generate_invoice_xml(&payload, "clave_tecnica_dian").unwrap();
        assert!(xml.contains("A &amp; B &lt;Ltda&gt;"));
        assert!(!xml.contains("<cbc:Injected>"));
        assert!(!xml.contains("A & B <Ltda>"));
    }
}

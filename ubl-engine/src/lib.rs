pub mod cufe;
pub mod error;
pub mod payload;
pub mod templates;

use cufe::calculate_cufe_cude;
use error::UblError;
use payload::InvoicePayload;
use templates::TemplateEngine;

/// Genera el XML UBL 2.1 base y calcula el CUFE para una factura.
/// Retorna una tupla con `(xml_renderizado, cufe_calculado)`.
pub fn generate_invoice_xml(payload: &InvoicePayload) -> Result<(String, String), UblError> {
    // 1. Validar el payload localmente antes del mapeo
    payload.validate()?;

    // 2. Calcular el CUFE/CUDE usando SHA-384
    // Para simplificar bajo el Anexo 1.9, asumimos por defecto IVA ("01") con el tax_amount,
    // y dejamos INC ("04") e ICA ("03") en 0.00.
    let cufe = calculate_cufe_cude(
        &format!("{}{}", payload.prefix, payload.number),
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
        &payload.technical_key,
        &payload.environment,
    );

    // 3. Renderizar plantilla XML con MiniJinja
    let engine = TemplateEngine::new();
    let xml = engine.render_invoice(
        "10", // CustomizationID: "10" estándar DIAN para factura de venta
        &payload.prefix,
        payload.number,
        &cufe,
        &payload.issue_date,
        &payload.issue_time,
        "01", // InvoiceTypeCode: "01" factura de venta nacional
        &payload.company_nit,
        &payload.company_name,
        "31", // SupplierIDType: "31" = NIT
        &payload.customer_id,
        &payload.customer_name,
        &payload.customer_id_type,
        payload.net_amount,
        payload.tax_amount,
        payload.total_amount,
    )?;

    Ok((xml, cufe))
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
    let engine = TemplateEngine::new();
    let xml = engine.render_attached_document(
        unique_id,
        issue_date,
        issue_time,
        &format!("{}{}", payload.prefix, payload.number),
        cufe,
        &payload.issue_date,
        &payload.company_nit,
        &payload.company_name,
        "31",
        &payload.customer_id,
        &payload.customer_name,
        &payload.customer_id_type,
        signed_invoice_xml,
        dian_response_xml,
    )?;
    Ok(xml)
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
    // 1. Calcular el CUDE del evento usando SHA-384
    let cude = cufe::calculate_cude_event(
        id,
        issue_date,
        issue_time,
        sender_nit,
        receiver_nit,
        software_pin,
        environment,
    );

    // 2. Renderizar plantilla XML
    let engine = TemplateEngine::new();
    let xml = engine.render_application_response(
        id,
        &cude,
        issue_date,
        issue_time,
        sender_nit,
        sender_name,
        sender_id_type,
        receiver_nit,
        receiver_name,
        receiver_id_type,
        event_code,
        event_description,
        parent_document_id,
        parent_document_uuid,
    )?;

    Ok((xml, cude))
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
            technical_key: "clave_tecnica_dian".to_string(),
            document_type: "invoice".to_string(),
            test_set_id: None,
        };

        let result = generate_invoice_xml(&payload);
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
            technical_key: "clave_tecnica_dian".to_string(),
            document_type: "invoice".to_string(),
            test_set_id: None,
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
        assert!(xml.contains("<![CDATA[<Invoice>signed</Invoice>]]>"));
        assert!(xml.contains("<![CDATA[<DianResponse>accepted</DianResponse>]]>"));
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
}

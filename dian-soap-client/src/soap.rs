use core_domain::ports::WSSE_SECURITY_PLACEHOLDER;

/// Escapa texto para insertarlo como contenido de un elemento XML.
pub fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Identificador `wsu:Id` de `wsa:To`: el firmador cubre con la firma WS-Security todo
/// elemento que lleve `wsu:Id`.
pub const TO_ID: &str = "id-To";

/// Construye un SOAP Envelope v1.2 con WS-Addressing y una cabecera `wsse:Security` vacía
/// (con [`WSSE_SECURITY_PLACEHOLDER`]) que el firmador completa con el Timestamp, el
/// certificado del tenant y la firma.
pub fn build_soap_envelope(action: &str, to_url: &str, body_content: &str) -> String {
    format!(
        concat!(
            r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://www.w3.org/2005/08/addressing" "#,
            r#"xmlns:wcf="http://wcf.dian.colombia" xmlns:wsse="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd" "#,
            r#"xmlns:wsu="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd">"#,
            r#"<soap:Header><wsse:Security soap:mustUnderstand="true">{security}</wsse:Security>"#,
            r#"<wsa:Action>{action}</wsa:Action><wsa:To wsu:Id="{to_id}">{to}</wsa:To></soap:Header>"#,
            r#"<soap:Body>{body}</soap:Body></soap:Envelope>"#
        ),
        security = WSSE_SECURITY_PLACEHOLDER,
        action = xml_escape(action),
        to_id = TO_ID,
        to = xml_escape(to_url),
        body = body_content,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_soap_envelope_structure() {
        let envelope = build_soap_envelope(
            "http://wcf.dian.colombia/IWcfDianCustomerServices/SendBillSync",
            "https://vpfe-hab.dian.gov.co/WcfDianCustomerServices.svc",
            "<wcf:SendBillSync><wcf:fileName>test.zip</wcf:fileName></wcf:SendBillSync>",
        );

        assert!(envelope.contains("<soap:Envelope"));
        assert!(envelope.contains("<wsa:Action>http://wcf.dian.colombia/IWcfDianCustomerServices/SendBillSync</wsa:Action>"));
        assert!(envelope.contains(r#"<wsa:To wsu:Id="id-To">https://vpfe-hab.dian.gov.co/WcfDianCustomerServices.svc</wsa:To>"#));
        assert!(envelope.contains(&format!("<wsse:Security soap:mustUnderstand=\"true\">{}</wsse:Security>", WSSE_SECURITY_PLACEHOLDER)));
        assert!(envelope.contains("<soap:Body><wcf:SendBillSync>"));
    }

    #[test]
    fn test_xml_escape_neutralizes_markup() {
        assert_eq!(
            xml_escape(r#"a</wcf:testSetId><x a="1">&'"#),
            "a&lt;/wcf:testSetId&gt;&lt;x a=&quot;1&quot;&gt;&amp;&apos;"
        );
        assert_eq!(xml_escape("0f3c-11aa"), "0f3c-11aa");
    }
}

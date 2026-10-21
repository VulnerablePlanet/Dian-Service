use chrono::Utc;

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

/// Construye un SOAP Envelope v1.2 con WS-Security (BinarySecurityToken, Timestamp) y WS-Addressing.
pub fn build_soap_envelope(
    action: &str,
    to_url: &str,
    binary_security_token: &str,
    body_content: &str,
) -> String {
    let now = Utc::now();
    let created = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    // Ventana de expiración estándar de 10 minutos
    let expires = (now + chrono::Duration::minutes(10)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    format!(
        r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://www.w3.org/2005/08/addressing" xmlns:wcf="http://wcf.dian.colombia" xmlns:wsse="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd" xmlns:wsu="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd"><soap:Header><wsa:Action>{}</wsa:Action><wsa:To>{}</wsa:To><wsse:Security><wsu:Timestamp wsu:Id="TS-1"><wsu:Created>{}</wsu:Created><wsu:Expires>{}</wsu:Expires></wsu:Timestamp><wsse:BinarySecurityToken EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary" ValueType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-x509-token-profile-1.0#X509v3" wsu:Id="X509-Token">{}</wsse:BinarySecurityToken></wsse:Security></soap:Header><soap:Body>{}</soap:Body></soap:Envelope>"#,
        xml_escape(action), xml_escape(to_url), created, expires, xml_escape(binary_security_token), body_content
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
            "cert_b64_dummy",
            "<wcf:SendBillSync><wcf:fileName>test.zip</wcf:fileName></wcf:SendBillSync>",
        );

        assert!(envelope.contains("<soap:Envelope"));
        assert!(envelope.contains("<wsa:Action>http://wcf.dian.colombia/IWcfDianCustomerServices/SendBillSync</wsa:Action>"));
        assert!(envelope.contains("<wsse:BinarySecurityToken"));
        assert!(envelope.contains("cert_b64_dummy"));
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

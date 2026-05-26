pub mod c14n;
pub mod error;
pub mod provider;
pub mod vault;
pub mod xades;

use error::CryptoError;
use provider::CertificateProvider;
use xades::XadesSigner;

/// Firma un documento XML utilizando el proveedor de certificados y una referencia de llave.
pub async fn sign_document(
    xml_content: &str,
    provider: &dyn CertificateProvider,
    key_ref: &str,
) -> Result<String, CryptoError> {
    let signer = XadesSigner::new(provider);
    signer.sign(xml_content, key_ref).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider::LocalCertificateProvider;

    // A mock RSA private key in PKCS#8 PEM format for testing
    const TEST_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQC6N2Hsh5q/Z24G
eJ+jP0U40j1W8o19W9Y2o4uC33v2/2L7Yvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbAgMBAAECggEBAJ5T1N6X9t39k23Xm+m74v6+M2376m3P2r6t279s6773
t279s6773t279s6773t279s6773t279s6773t279s6773t279s6773t279s6773
t279s6773t279s6773t279s6773t279s6773t279s6773t279s6773t279s6773
t279s6773t279s6773t279s6773t279s6773t279s6773t279s6773t279s6773
t279s6773t279s6773t279s6773t279s6773t279s6773t279s6773t279s6773
t279s6773gQKBgQDg73t279s6773t279s6773t279s6773t279s6773t279s6773
t279s6773t279s6773t279s6773t279s6773t279s6773t279s6773t279s6773
t279s6773t279s6773t279s6773t279s6773t279s6773gQKBgQDa97du/bOu+9
7du/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97
du/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97d
u/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97gQKBgQDZ97du/bOu+97d
u/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97du
/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97du/bOu+97du/
bOu+97du/bOu+97du/bOu+97du/bOu+97gQKBgQC6N2Hsh5q/Z24GeJ+jP0U40j1
W8o19W9Y2o4uC33v2/2L7Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvt
j22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvt
j22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvt
j22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvt
j22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvt
Yvtj22LbAgECgYEAtjdt7Ieav2duBnifoz9FONI9VvKNfVvWNqOLgt979v9i+2L7
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22Lb
-----END PRIVATE KEY-----
"#;

    // A mock certificate PEM (contains public key matching private key above)
    // For simplicity, we just use a placeholder certificate PEM since RSA local signing only uses the private key.
    const TEST_CERT_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIBpzCCAU+gAwIBAgIBADANBgkqhkiG9w0BAQsFADAKMQswCQYDVQQGEwJDTzAe
Fw0yNjA1MjMwNTEzMjZaFw0yNzA1MjMwNTEzMjZaMAoxCzAJBgNVBAYTAmNPMIGf
MA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQC6N2Hsh5q/Z24GeJ+jP0U40j1W8o19
W9Y2o4uC33v2/2L7Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbAgMBAAEwDQYJKoZIhvcNAQELBQADgYEALjdt7Ieav2duBnifoz9FONI9
VvKNfVvWNqOLgt979v9i+2L7Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
Yvtj22LbYvtj22LbYvtj22LbYvtj22LbYvtj22Lb
-----END CERTIFICATE-----
"#;

    #[tokio::test]
    async fn test_sign_document_success() {
        let provider = LocalCertificateProvider {
            certificate_pem: TEST_CERT_PEM.to_string(),
            private_key_pem: TEST_KEY_PEM.to_string(),
        };

        let xml_input = r#"<?xml version="1.0" encoding="UTF-8"?><Invoice><cbc:ID>FE1002</cbc:ID><!-- PLACEHOLDER_FOR_SIGNATURE --></Invoice>"#;

        let result = sign_document(xml_input, &provider, "local_cert").await;
        assert!(result.is_ok());

        let signed_xml = result.unwrap();
        assert!(signed_xml.contains("<ds:Signature"));
        assert!(signed_xml.contains("<ds:SignatureValue>"));
        assert!(signed_xml.contains("<xades:SignedProperties"));
        assert!(!signed_xml.contains("<!-- PLACEHOLDER_FOR_SIGNATURE -->"));
    }
}

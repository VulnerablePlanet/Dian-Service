pub mod c14n;
pub mod error;
pub mod provider;
pub mod signer;
pub mod vault;
pub mod verify;
pub mod xades;
pub mod xmlutil;

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
    use crate::verify::{check_signature_structure, XmlSignatureVerifier};
    use provider::LocalCertificateProvider;

    // Pares de prueba desechables (ver tests/fixtures/README.md). Nunca usar en produccion.
    const TEST_KEY_PEM: &str = include_str!("../tests/fixtures/test_key.pem");
    const TEST_CERT_PEM: &str = include_str!("../tests/fixtures/test_cert.pem");
    const UNTRUSTED_KEY_PEM: &str = include_str!("../tests/fixtures/untrusted_key.pem");
    const UNTRUSTED_CERT_PEM: &str = include_str!("../tests/fixtures/untrusted_cert.pem");

    /// Documento con namespace por defecto y prefijos declarados en la raíz, como las
    /// plantillas UBL: los nodos firmados heredan esos namespaces.
    const XML_INPUT: &str = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>"#,
        "\n",
        r#"<Invoice xmlns="urn:oasis:names:specification:ubl:schema:xsd:Invoice-2" "#,
        r#"xmlns:cbc="urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2" "#,
        r#"xmlns:ds="http://www.w3.org/2000/09/xmldsig#" "#,
        r#"xmlns:ext="urn:oasis:names:specification:ubl:schema:xsd:CommonExtensionComponents-2">"#,
        "\n    <ext:UBLExtensions><ext:UBLExtension><ext:ExtensionContent>\n        ",
        "<!-- PLACEHOLDER_FOR_SIGNATURE -->",
        "\n    </ext:ExtensionContent></ext:UBLExtension></ext:UBLExtensions>\n",
        r#"    <cbc:ID>FE1002</cbc:ID><cbc:Note>Caf&#233; &amp; t&#233; &lt;ok&gt;</cbc:Note>"#,
        "\n</Invoice>\n"
    );

    fn provider(cert: &str, key: &str) -> LocalCertificateProvider {
        LocalCertificateProvider::from_pem(cert.to_string(), key.to_string()).unwrap()
    }

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name)
    }

    fn xmlsec1_available() -> bool {
        std::process::Command::new("xmlsec1").arg("--version").output().is_ok()
    }

    async fn signed(cert: &str, key: &str) -> String {
        sign_document(XML_INPUT, &provider(cert, key), "local").await.unwrap()
    }

    #[tokio::test]
    async fn signature_has_xades_structure_and_real_certificate_data() {
        let xml = signed(TEST_CERT_PEM, TEST_KEY_PEM).await;
        assert!(!xml.contains("PLACEHOLDER_FOR_SIGNATURE"));
        assert!(xml.contains(xades::ALG_RSA_SHA256));
        assert!(xml.contains(xades::ALG_C14N));
        assert!(!xml.contains("sha384"), "algorithms must be consistent (SHA-256)");
        assert!(xml.contains("CN=Dian-Service Test Root CA"), "real issuer name");
        assert!(xml.contains("<xades:ClaimedRole>supplier</xades:ClaimedRole>"));
        assert!(xml.contains(xades::POLICY_DIGEST_SHA256));
        check_signature_structure(&xml).unwrap();
    }

    #[tokio::test]
    async fn signature_verifies_with_xmlsec1_against_the_trusted_ca() {
        if !xmlsec1_available() {
            assert!(std::env::var_os("CI").is_none(), "xmlsec1 is required in CI");
            eprintln!("xmlsec1 not installed: skipping independent signature verification");
            return;
        }
        let xml = signed(TEST_CERT_PEM, TEST_KEY_PEM).await;
        XmlSignatureVerifier::new(fixture("test_ca.pem")).verify(&xml).unwrap();
    }

    #[tokio::test]
    async fn xmlsec1_rejects_tampering_and_untrusted_signers() {
        if !xmlsec1_available() {
            assert!(std::env::var_os("CI").is_none(), "xmlsec1 is required in CI");
            eprintln!("xmlsec1 not installed: skipping independent signature verification");
            return;
        }
        let verifier = XmlSignatureVerifier::new(fixture("test_ca.pem"));
        let xml = signed(TEST_CERT_PEM, TEST_KEY_PEM).await;

        let tampered = xml.replace("<cbc:ID>FE1002</cbc:ID>", "<cbc:ID>FE9999</cbc:ID>");
        assert!(verifier.verify(&tampered).is_err(), "modified content must fail");

        let untrusted = signed(UNTRUSTED_CERT_PEM, UNTRUSTED_KEY_PEM).await;
        assert!(verifier.verify(&untrusted).is_err(), "signer outside the trusted CA must fail");
        XmlSignatureVerifier::new(fixture("untrusted_ca.pem")).verify(&untrusted).unwrap();
    }

    #[test]
    fn structure_check_rejects_unsigned_partial_and_multiple_signatures() {
        assert!(check_signature_structure("<a/>").is_err());
        let partial = r##"<a xmlns:ds="http://www.w3.org/2000/09/xmldsig#"><b Id="x"/><ds:Signature><ds:SignedInfo><ds:Reference URI="#x"/></ds:SignedInfo></ds:Signature></a>"##;
        assert!(check_signature_structure(partial).unwrap_err().to_string().contains("whole document"));
        let whole = r#"<ds:Signature><ds:SignedInfo><ds:Reference URI=""><ds:Transforms><ds:Transform Algorithm="http://www.w3.org/2000/09/xmldsig#enveloped-signature"/></ds:Transforms></ds:Reference></ds:SignedInfo></ds:Signature>"#;
        let double = format!(r#"<a xmlns:ds="http://www.w3.org/2000/09/xmldsig#">{whole}{whole}</a>"#);
        assert!(check_signature_structure(&double).unwrap_err().to_string().contains("2 signatures"));
    }

    #[tokio::test]
    async fn sign_document_rejects_invalid_certificate_or_missing_placeholder() {
        let provider = LocalCertificateProvider {
            certificate_pem: "cert_dummy".to_string(),
            private_key_pem: "key_dummy".to_string(),
        };
        assert!(sign_document(XML_INPUT, &provider, "local").await.is_err());
        assert!(sign_document("<a/>", &self::provider(TEST_CERT_PEM, TEST_KEY_PEM), "local").await.is_err());
    }
}

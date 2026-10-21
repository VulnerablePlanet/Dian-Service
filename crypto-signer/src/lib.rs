pub mod c14n;
pub mod error;
pub mod provider;
pub mod signer;
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
    use rsa::{pkcs8::DecodePrivateKey, Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
    use sha2::{Digest, Sha384};

    // Par de prueba desechable (ver tests/fixtures/README.md). Nunca usar en produccion.
    const TEST_KEY_PEM: &str = include_str!("../tests/fixtures/test_key.pem");
    const TEST_CERT_PEM: &str = include_str!("../tests/fixtures/test_cert.pem");

    const XML_INPUT: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Invoice xmlns="urn:oasis:names:specification:ubl:schema:xsd:Invoice-2" xmlns:cbc="urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2"><cbc:ID>FE1002</cbc:ID><!-- PLACEHOLDER_FOR_SIGNATURE --></Invoice>"#;

    fn test_provider() -> LocalCertificateProvider {
        LocalCertificateProvider {
            certificate_pem: TEST_CERT_PEM.to_string(),
            private_key_pem: TEST_KEY_PEM.to_string(),
        }
    }

    #[tokio::test]
    async fn test_sign_document_success() {
        let signed_xml = sign_document(XML_INPUT, &test_provider(), "local_cert")
            .await
            .expect("signing with a valid key pair must succeed");

        assert!(signed_xml.contains("<ds:Signature"));
        assert!(signed_xml.contains("<ds:SignatureValue>"));
        assert!(signed_xml.contains("<xades:SignedProperties"));
        assert!(!signed_xml.contains("<!-- PLACEHOLDER_FOR_SIGNATURE -->"));
    }

    #[tokio::test]
    async fn test_signature_value_verifies_with_public_key() {
        let signed_xml = sign_document(XML_INPUT, &test_provider(), "local_cert")
            .await
            .unwrap();

        let start = signed_xml.find("<ds:SignedInfo").unwrap();
        let end = signed_xml.find("</ds:SignedInfo>").unwrap() + "</ds:SignedInfo>".len();
        let signed_info_c14n = c14n::canonicalize_xml(&signed_xml[start..end]).unwrap();
        let digest = Sha384::digest(signed_info_c14n.as_bytes());

        let sv_start = signed_xml.find("<ds:SignatureValue>").unwrap() + "<ds:SignatureValue>".len();
        let sv_end = signed_xml.find("</ds:SignatureValue>").unwrap();
        let signature = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &signed_xml[sv_start..sv_end],
        )
        .unwrap();

        let public_key = RsaPublicKey::from(RsaPrivateKey::from_pkcs8_pem(TEST_KEY_PEM).unwrap());
        public_key
            .verify(Pkcs1v15Sign::new::<Sha384>(), &digest, &signature)
            .expect("SignatureValue must verify against the SignedInfo digest");
    }

    #[tokio::test]
    async fn test_sign_document_rejects_invalid_certificate() {
        let provider = LocalCertificateProvider {
            certificate_pem: "cert_dummy".to_string(),
            private_key_pem: "key_dummy".to_string(),
        };
        assert!(sign_document(XML_INPUT, &provider, "local_cert").await.is_err());
    }
}

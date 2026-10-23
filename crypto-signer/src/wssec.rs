//! Cabecera WS-Security (X.509 Token Profile 1.0) de los envelopes SOAP enviados a la DIAN.
//!
//! - El certificado del firmante viaja como `wsse:BinarySecurityToken` y la firma lo
//!   referencia con `wsse:SecurityTokenReference`.
//! - Se firman el `wsu:Timestamp` (anti-replay) y cada elemento del envelope con `wsu:Id`
//!   (el cliente SOAP marca así `wsa:To`).
//! - Canonicalización C14N exclusiva, RSA-SHA256 y SHA-256: es la suite que exige WCF, el
//!   framework del servicio de la DIAN.
//!
//! La firma resultante se verifica en las pruebas con `xmlsec1`.

use crate::c14n::canonicalize_xml_exclusive;
use crate::error::CryptoError;
use crate::provider::CertificateProvider;
use crate::xades::{ALG_RSA_SHA256, ALG_SHA256, NS_DS};
use crate::xmlutil::{elements_with_prefixed_id, namespaces_in_scope_at, namespaces_in_scope_before, with_namespaces};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{Duration, SecondsFormat, Utc};
use core_domain::ports::WSSE_SECURITY_PLACEHOLDER;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const ALG_EXC_C14N: &str = "http://www.w3.org/2001/10/xml-exc-c14n#";
pub const NS_WSSE: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
pub const NS_WSU: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd";
const X509_TOKEN_TYPE: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-x509-token-profile-1.0#X509v3";
const BASE64_ENCODING: &str =
    "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary";

/// Validez del Timestamp: la DIAN rechaza mensajes vencidos.
const TIMESTAMP_TTL: Duration = Duration::minutes(5);

/// Marcador interno: posición de la firma mientras se calculan los digests.
const SIGNATURE_MARK: &str = "<!-- WSSE_SIGNATURE -->";

fn sha256_b64(data: &str) -> String {
    B64.encode(Sha256::digest(data.as_bytes()))
}

/// Firma el envelope sustituyendo [`WSSE_SECURITY_PLACEHOLDER`] (dentro de `wsse:Security`)
/// por el Timestamp, el BinarySecurityToken y la firma. El envelope debe declarar los
/// prefijos `wsse` y `wsu`.
pub async fn sign_soap_envelope(
    envelope: &str,
    provider: &dyn CertificateProvider,
    key_ref: &str,
) -> Result<String, CryptoError> {
    if envelope.matches(WSSE_SECURITY_PLACEHOLDER).count() != 1 {
        return Err(CryptoError::Xml(
            "The SOAP envelope must contain exactly one WS-Security placeholder".to_string(),
        ));
    }
    let scope = namespaces_in_scope_at(envelope, WSSE_SECURITY_PLACEHOLDER)?;
    for (prefix, namespace) in [("wsse", NS_WSSE), ("wsu", NS_WSU)] {
        if scope.get(prefix).map(String::as_str) != Some(namespace) {
            return Err(CryptoError::Xml(format!(
                "The SOAP envelope must declare xmlns:{}=\"{}\"",
                prefix, namespace
            )));
        }
    }

    let certificate = provider.get_certificate(key_ref).await?;
    let suffix = Uuid::new_v4().simple().to_string();
    let token_id = format!("X509-{}", suffix);
    let now = Utc::now();
    let timestamp = format!(
        r#"<wsu:Timestamp wsu:Id="TS-{suffix}"><wsu:Created>{created}</wsu:Created><wsu:Expires>{expires}</wsu:Expires></wsu:Timestamp>"#,
        created = now.to_rfc3339_opts(SecondsFormat::Millis, true),
        expires = (now + TIMESTAMP_TTL).to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    let token = format!(
        r#"<wsse:BinarySecurityToken EncodingType="{BASE64_ENCODING}" ValueType="{X509_TOKEN_TYPE}" wsu:Id="{token_id}">{certificate}</wsse:BinarySecurityToken>"#,
    );

    // Las referencias se calculan sobre el envelope sin el token ni la firma: el token no se
    // firma y ninguno de los dos altera los elementos firmados.
    let with_timestamp = envelope.replace(WSSE_SECURITY_PLACEHOLDER, &timestamp);
    let mut references = String::new();
    for (id, start, end) in elements_with_prefixed_id(&with_timestamp)? {
        let element = with_namespaces(
            &with_timestamp[start..end],
            &namespaces_in_scope_before(&with_timestamp, start)?,
        );
        references.push_str(&format!(
            concat!(
                r##"<ds:Reference URI="#{id}"><ds:Transforms><ds:Transform Algorithm="{c14n}"/></ds:Transforms>"##,
                r##"<ds:DigestMethod Algorithm="{sha256}"/><ds:DigestValue>{digest}</ds:DigestValue></ds:Reference>"##
            ),
            id = id,
            c14n = ALG_EXC_C14N,
            sha256 = ALG_SHA256,
            digest = sha256_b64(&canonicalize_xml_exclusive(&element, &[])?),
        ));
    }

    let signed_info = format!(
        r#"<ds:SignedInfo><ds:CanonicalizationMethod Algorithm="{ALG_EXC_C14N}"/><ds:SignatureMethod Algorithm="{ALG_RSA_SHA256}"/>{references}</ds:SignedInfo>"#,
    );
    let mut signature_scope = scope.clone();
    signature_scope.insert("ds".to_string(), NS_DS.to_string());
    let signed_info_c14n = canonicalize_xml_exclusive(&with_namespaces(&signed_info, &signature_scope), &[])?;
    let signature_value = provider.sign_rsa_sha256(key_ref, signed_info_c14n.as_bytes()).await?;

    let signature = format!(
        concat!(
            r##"<ds:Signature xmlns:ds="{ns_ds}" Id="SIG-{suffix}">{signed_info}"##,
            r##"<ds:SignatureValue>{value}</ds:SignatureValue><ds:KeyInfo Id="KI-{suffix}">"##,
            r##"<wsse:SecurityTokenReference wsu:Id="STR-{suffix}"><wsse:Reference URI="#{token_id}" ValueType="{token_type}"/>"##,
            r##"</wsse:SecurityTokenReference></ds:KeyInfo></ds:Signature>"##
        ),
        ns_ds = NS_DS,
        suffix = suffix,
        signed_info = signed_info,
        value = B64.encode(signature_value),
        token_id = token_id,
        token_type = X509_TOKEN_TYPE,
    );

    let header = format!("{}{}{}", timestamp, token, SIGNATURE_MARK);
    Ok(envelope
        .replace(WSSE_SECURITY_PLACEHOLDER, &header)
        .replace(SIGNATURE_MARK, &signature))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::LocalCertificateProvider;
    use std::process::Command;

    const TEST_KEY_PEM: &str = include_str!("../tests/fixtures/test_key.pem");
    const TEST_CERT_PEM: &str = include_str!("../tests/fixtures/test_cert.pem");
    const UNTRUSTED_CERT_PEM: &str = include_str!("../tests/fixtures/untrusted_cert.pem");

    fn envelope() -> String {
        format!(
            concat!(
                r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://www.w3.org/2005/08/addressing" "#,
                r#"xmlns:wcf="http://wcf.dian.colombia" xmlns:wsse="{}" xmlns:wsu="{}"><soap:Header>"#,
                r#"<wsse:Security soap:mustUnderstand="true">{}</wsse:Security>"#,
                r#"<wsa:Action>http://wcf.dian.colombia/IWcfDianCustomerServices/GetStatus</wsa:Action>"#,
                r#"<wsa:To wsu:Id="id-To">https://vpfe-hab.dian.gov.co/WcfDianCustomerServices.svc</wsa:To>"#,
                r#"</soap:Header><soap:Body><wcf:GetStatus><wcf:trackId>abc &amp; def</wcf:trackId></wcf:GetStatus></soap:Body></soap:Envelope>"#
            ),
            NS_WSSE, NS_WSU, WSSE_SECURITY_PLACEHOLDER
        )
    }

    async fn signed() -> String {
        let provider = LocalCertificateProvider::from_pem(TEST_CERT_PEM.into(), TEST_KEY_PEM.into()).unwrap();
        sign_soap_envelope(&envelope(), &provider, "local").await.unwrap()
    }

    /// Verifica con xmlsec1 usando `cert` como clave pública (la firma referencia el token
    /// por `SecurityTokenReference`, que xmlsec1 no resuelve por sí mismo).
    fn xmlsec1_verify(xml: &str, cert: &str) -> Option<bool> {
        if Command::new("xmlsec1").arg("--version").output().is_err() {
            assert!(std::env::var_os("CI").is_none(), "xmlsec1 is required in CI");
            eprintln!("xmlsec1 not installed: skipping WS-Security verification");
            return None;
        }
        let dir = std::env::temp_dir().join(format!("wssec-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("envelope.xml"), xml).unwrap();
        std::fs::write(dir.join("cert.pem"), cert).unwrap();
        let output = Command::new("xmlsec1")
            .arg("--verify")
            .arg("--enabled-reference-uris")
            .arg("same-doc")
            .arg("--pubkey-cert-pem")
            .arg(dir.join("cert.pem"))
            .arg("--id-attr:Id")
            .arg("http://www.w3.org/2005/08/addressing:To")
            .arg("--id-attr:Id")
            .arg(format!("{}:Timestamp", NS_WSU))
            .arg(dir.join("envelope.xml"))
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        }
        Some(output.status.success())
    }

    #[tokio::test]
    async fn header_contains_timestamp_token_and_signature_over_to_and_timestamp() {
        let xml = signed().await;
        assert!(!xml.contains(WSSE_SECURITY_PLACEHOLDER) && !xml.contains(SIGNATURE_MARK));
        assert!(xml.contains("<wsu:Timestamp wsu:Id=\"TS-"));
        assert!(xml.contains("<wsse:BinarySecurityToken"));
        assert!(xml.contains(r##"<ds:Reference URI="#id-To">"##));
        assert!(xml.contains(r##"<ds:Reference URI="#TS-"##));
        assert_eq!(xml.matches("<ds:Reference ").count(), 2, "the token itself is not signed");
        assert!(xml.contains(ALG_EXC_C14N) && xml.contains(ALG_RSA_SHA256));
        let certificate: String = TEST_CERT_PEM.lines().filter(|l| !l.starts_with("-----")).collect();
        assert!(xml.contains(&certificate), "the tenant certificate travels as the token");
    }

    #[tokio::test]
    async fn signature_verifies_with_xmlsec1() {
        assert_ne!(xmlsec1_verify(&signed().await, TEST_CERT_PEM), Some(false));
    }

    #[tokio::test]
    async fn tampering_with_signed_elements_is_detected() {
        let xml = signed().await;
        let redirected = xml.replace("https://vpfe-hab.dian.gov.co/", "https://attacker.example/");
        assert_ne!(xmlsec1_verify(&redirected, TEST_CERT_PEM), Some(true));
        let created_at = xml.find("<wsu:Created>").unwrap() + "<wsu:Created>".len();
        let mut replayed = xml.clone();
        replayed.replace_range(created_at..created_at + 4, "2001");
        assert_ne!(xmlsec1_verify(&replayed, TEST_CERT_PEM), Some(true));
    }

    #[tokio::test]
    async fn signature_does_not_verify_with_another_key() {
        assert_ne!(xmlsec1_verify(&signed().await, UNTRUSTED_CERT_PEM), Some(true));
    }

    #[tokio::test]
    async fn rejects_envelopes_without_placeholder_or_namespaces() {
        let provider = LocalCertificateProvider::from_pem(TEST_CERT_PEM.into(), TEST_KEY_PEM.into()).unwrap();
        let no_placeholder = envelope().replace(WSSE_SECURITY_PLACEHOLDER, "");
        assert!(sign_soap_envelope(&no_placeholder, &provider, "local").await.is_err());
        let no_wsu = envelope().replace(&format!(r#" xmlns:wsu="{}""#, NS_WSU), "");
        assert!(sign_soap_envelope(&no_wsu, &provider, "local").await.is_err());
    }
}

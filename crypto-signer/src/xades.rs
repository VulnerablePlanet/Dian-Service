//! Firma XAdES-EPES envolvente (enveloped) de documentos UBL.
//!
//! - Canonicalización: C14N 1.0 inclusiva sin comentarios, declarada y aplicada igual.
//! - Digests y firma: SHA-256 / RSA-SHA256 en todas las referencias.
//! - Cada nodo firmado (`SignedInfo`, `KeyInfo`, `SignedProperties`) se canonicaliza con los
//!   namespaces que hereda en el documento final, como lo hace cualquier verificador.
//! - `SigningCertificate` usa el digest, emisor y serial reales del certificado.
//!
//! La firma resultante se verifica en las pruebas con `xmlsec1`, una implementación
//! independiente de XML-DSig.

use crate::c14n::canonicalize_xml;
use crate::error::CryptoError;
use crate::provider::CertificateProvider;
use crate::xmlutil::{escape_text, namespaces_in_scope_at, with_namespaces};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{FixedOffset, Utc};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;
use x509_cert::der::Decode;

/// Marcador que las plantillas UBL colocan dentro de `ext:ExtensionContent`.
pub const SIGNATURE_PLACEHOLDER: &str = "<!-- PLACEHOLDER_FOR_SIGNATURE -->";

pub const NS_DS: &str = "http://www.w3.org/2000/09/xmldsig#";
pub const NS_XADES: &str = "http://uri.etsi.org/01903/v1.3.2#";
pub const ALG_C14N: &str = "http://www.w3.org/TR/2001/REC-xml-c14n-20010315";
pub const ALG_ENVELOPED: &str = "http://www.w3.org/2000/09/xmldsig#enveloped-signature";
pub const ALG_RSA_SHA256: &str = "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256";
pub const ALG_SHA256: &str = "http://www.w3.org/2001/04/xmlenc#sha256";
const SIGNED_PROPERTIES_TYPE: &str = "http://uri.etsi.org/01903#SignedProperties";

/// Política de firma DIAN v2.
pub const POLICY_URL: &str = "https://facturaelectronica.dian.gov.co/politicadefirma/v2/politicadefirmav2.pdf";
/// SHA-256 (Base64) del PDF de la política de firma DIAN v2.
///
/// No se pudo comprobar contra el PDF oficial desde el entorno de desarrollo (sin acceso al
/// dominio de la DIAN). Verificación recomendada antes de producción:
/// `curl -s POLICY_URL | openssl dgst -sha256 -binary | base64`.
pub const POLICY_DIGEST_SHA256: &str = "dMoMvtcG5aIzgYo0tIsSQeVJBDnUnfSOfBpxXrmor0Y=";
const POLICY_DESCRIPTION: &str = "Política de firma para facturas electrónicas de la República de Colombia";

/// Datos del certificado del firmante necesarios para `SigningCertificate`.
pub struct CertificateInfo {
    pub der_base64: String,
    pub digest_sha256: String,
    /// Nombre del emisor en formato RFC 4514.
    pub issuer_name: String,
    /// Número de serie en decimal.
    pub serial_number: String,
}

impl CertificateInfo {
    pub fn from_base64_der(der_base64: &str) -> Result<Self, CryptoError> {
        let der = B64
            .decode(der_base64)
            .map_err(|e| CryptoError::Signing(format!("Certificate is not valid Base64: {}", e)))?;
        let certificate = x509_cert::Certificate::from_der(&der)
            .map_err(|e| CryptoError::Signing(format!("Certificate is not valid X.509 DER: {}", e)))?;
        let tbs = &certificate.tbs_certificate;
        Ok(Self {
            der_base64: der_base64.to_string(),
            digest_sha256: B64.encode(Sha256::digest(&der)),
            issuer_name: tbs.issuer.to_string(),
            serial_number: decimal_from_be_bytes(tbs.serial_number.as_bytes()),
        })
    }
}

/// Entero sin signo big-endian en decimal (los números de serie X.509 llegan a 20 bytes).
fn decimal_from_be_bytes(bytes: &[u8]) -> String {
    let mut digits: Vec<u8> = vec![0]; // base 10, menos significativo primero
    for &byte in bytes {
        let mut carry = u32::from(byte);
        for digit in digits.iter_mut() {
            let value = u32::from(*digit) * 256 + carry;
            *digit = (value % 10) as u8;
            carry = value / 10;
        }
        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
    }
    digits.iter().rev().map(|d| char::from(b'0' + d)).collect()
}

fn sha256_b64(data: &str) -> String {
    B64.encode(Sha256::digest(data.as_bytes()))
}

fn signing_time() -> String {
    let colombia = FixedOffset::west_opt(5 * 3600).expect("valid offset");
    Utc::now()
        .with_timezone(&colombia)
        .format("%Y-%m-%dT%H:%M:%S%.3f%:z")
        .to_string()
}

pub struct XadesSigner<'a> {
    provider: &'a dyn CertificateProvider,
}

impl<'a> XadesSigner<'a> {
    pub fn new(provider: &'a dyn CertificateProvider) -> Self {
        Self { provider }
    }

    /// Firma el documento sustituyendo `SIGNATURE_PLACEHOLDER` por el bloque `ds:Signature`.
    pub async fn sign(&self, xml_content: &str, key_ref: &str) -> Result<String, CryptoError> {
        if xml_content.matches(SIGNATURE_PLACEHOLDER).count() != 1 {
            return Err(CryptoError::Xml(
                "The document must contain exactly one signature placeholder".to_string(),
            ));
        }

        let certificate = CertificateInfo::from_base64_der(&self.provider.get_certificate(key_ref).await?)?;

        // Namespaces vigentes en el punto donde se insertará la firma.
        let mut signature_scope = namespaces_in_scope_at(xml_content, SIGNATURE_PLACEHOLDER)?;
        signature_scope.insert("ds".to_string(), NS_DS.to_string());
        let mut properties_scope: BTreeMap<String, String> = signature_scope.clone();
        properties_scope.insert("xades".to_string(), NS_XADES.to_string());

        let id = format!("xmldsig-{}", Uuid::new_v4());
        let key_info_id = format!("{}-keyinfo", id);
        let signed_props_id = format!("{}-signedprops", id);

        let key_info = format!(
            r##"<ds:KeyInfo Id="{key_info_id}"><ds:X509Data><ds:X509Certificate>{cert}</ds:X509Certificate></ds:X509Data></ds:KeyInfo>"##,
            cert = certificate.der_base64,
        );

        let signed_properties = format!(
            concat!(
                r##"<xades:SignedProperties Id="{sp}"><xades:SignedSignatureProperties>"##,
                r##"<xades:SigningTime>{time}</xades:SigningTime>"##,
                r##"<xades:SigningCertificate><xades:Cert><xades:CertDigest>"##,
                r##"<ds:DigestMethod Algorithm="{sha256}"/><ds:DigestValue>{cert_digest}</ds:DigestValue>"##,
                r##"</xades:CertDigest><xades:IssuerSerial><ds:X509IssuerName>{issuer}</ds:X509IssuerName>"##,
                r##"<ds:X509SerialNumber>{serial}</ds:X509SerialNumber></xades:IssuerSerial></xades:Cert>"##,
                r##"</xades:SigningCertificate><xades:SignaturePolicyIdentifier><xades:SignaturePolicyId>"##,
                r##"<xades:SigPolicyId><xades:Identifier>{policy_url}</xades:Identifier>"##,
                r##"<xades:Description>{policy_description}</xades:Description></xades:SigPolicyId>"##,
                r##"<xades:SigPolicyHash><ds:DigestMethod Algorithm="{sha256}"/>"##,
                r##"<ds:DigestValue>{policy_digest}</ds:DigestValue></xades:SigPolicyHash>"##,
                r##"</xades:SignaturePolicyId></xades:SignaturePolicyIdentifier>"##,
                r##"<xades:SignerRole><xades:ClaimedRoles><xades:ClaimedRole>supplier</xades:ClaimedRole>"##,
                r##"</xades:ClaimedRoles></xades:SignerRole></xades:SignedSignatureProperties></xades:SignedProperties>"##
            ),
            sp = signed_props_id,
            time = signing_time(),
            sha256 = ALG_SHA256,
            cert_digest = certificate.digest_sha256,
            issuer = escape_text(&certificate.issuer_name),
            serial = certificate.serial_number,
            policy_url = POLICY_URL,
            policy_description = POLICY_DESCRIPTION,
            policy_digest = POLICY_DIGEST_SHA256,
        );

        // Referencia al documento: la transformación enveloped elimina el ds:Signature, que
        // ocupa exactamente el lugar del marcador; el resto del documento queda idéntico.
        let document_digest = sha256_b64(&canonicalize_xml(&xml_content.replace(SIGNATURE_PLACEHOLDER, ""))?);
        let key_info_digest = sha256_b64(&canonicalize_xml(&with_namespaces(&key_info, &signature_scope))?);
        let properties_digest =
            sha256_b64(&canonicalize_xml(&with_namespaces(&signed_properties, &properties_scope))?);

        let signed_info = format!(
            concat!(
                r##"<ds:SignedInfo><ds:CanonicalizationMethod Algorithm="{c14n}"/>"##,
                r##"<ds:SignatureMethod Algorithm="{rsa}"/>"##,
                r##"<ds:Reference Id="{id}-ref0" URI=""><ds:Transforms><ds:Transform Algorithm="{enveloped}"/></ds:Transforms>"##,
                r##"<ds:DigestMethod Algorithm="{sha256}"/><ds:DigestValue>{doc}</ds:DigestValue></ds:Reference>"##,
                r##"<ds:Reference URI="#{ki}"><ds:DigestMethod Algorithm="{sha256}"/><ds:DigestValue>{kid}</ds:DigestValue></ds:Reference>"##,
                r##"<ds:Reference Type="{sp_type}" URI="#{sp}"><ds:DigestMethod Algorithm="{sha256}"/>"##,
                r##"<ds:DigestValue>{spd}</ds:DigestValue></ds:Reference></ds:SignedInfo>"##
            ),
            c14n = ALG_C14N,
            rsa = ALG_RSA_SHA256,
            id = id,
            enveloped = ALG_ENVELOPED,
            sha256 = ALG_SHA256,
            doc = document_digest,
            ki = key_info_id,
            kid = key_info_digest,
            sp_type = SIGNED_PROPERTIES_TYPE,
            sp = signed_props_id,
            spd = properties_digest,
        );

        let signed_info_c14n = canonicalize_xml(&with_namespaces(&signed_info, &signature_scope))?;
        let signature_value = self.provider.sign_rsa_sha256(key_ref, signed_info_c14n.as_bytes()).await?;

        let signature = format!(
            concat!(
                r##"<ds:Signature xmlns:ds="{ns_ds}" Id="{id}">{signed_info}"##,
                r##"<ds:SignatureValue Id="{id}-sigvalue">{value}</ds:SignatureValue>{key_info}"##,
                r##"<ds:Object><xades:QualifyingProperties xmlns:xades="{ns_xades}" Target="#{id}">"##,
                r##"{signed_properties}</xades:QualifyingProperties></ds:Object></ds:Signature>"##
            ),
            ns_ds = NS_DS,
            id = id,
            signed_info = signed_info,
            value = B64.encode(signature_value),
            key_info = key_info,
            ns_xades = NS_XADES,
            signed_properties = signed_properties,
        );

        Ok(xml_content.replace(SIGNATURE_PLACEHOLDER, &signature))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_numbers_are_rendered_in_decimal() {
        assert_eq!(decimal_from_be_bytes(&[0]), "0");
        assert_eq!(decimal_from_be_bytes(&[1, 0]), "256");
        assert_eq!(decimal_from_be_bytes(&[0, 0xff]), "255");
        // 2^128 - 1
        assert_eq!(decimal_from_be_bytes(&[0xff; 16]), "340282366920938463463374607431768211455");
    }
}

use crate::c14n::canonicalize_xml;
use crate::error::CryptoError;
use crate::provider::CertificateProvider;
use sha2::{Digest, Sha384};
use uuid::Uuid;
use chrono::Utc;

pub struct XadesSigner<'a> {
    provider: &'a dyn CertificateProvider,
}

impl<'a> XadesSigner<'a> {
    pub fn new(provider: &'a dyn CertificateProvider) -> Self {
        Self { provider }
    }

    /// Firma un XML UBL 2.1 con el formato XAdES-EPES.
    /// Reemplaza el tag `<!-- PLACEHOLDER_FOR_SIGNATURE -->` con la firma XML.
    pub async fn sign(
        &self,
        xml_content: &str,
        key_ref: &str,
    ) -> Result<String, CryptoError> {
        // 1. Obtener certificado Base64 limpio
        let cert_b64 = self.provider.get_certificate(key_ref).await?;
        
        // Decodificar el certificado para calcular su hash (CertDigest)
        let cert_der = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &cert_b64)
            .map_err(|e| CryptoError::Signing(format!("Failed to decode certificate base64: {}", e)))?;
        
        let mut cert_hasher = Sha384::new();
        cert_hasher.update(&cert_der);
        let cert_hash = cert_hasher.finalize();
        let cert_digest_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, cert_hash);

        // Generar identificadores únicos estables para los elementos de firma
        let uuid = Uuid::new_v4().to_string();
        let sig_id = format!("Signature-{}", uuid);
        let key_info_id = format!("KeyInfo-{}", uuid);
        let signed_props_id = format!("SignedProperties-{}", uuid);

        // Metadata de firma
        let signing_time = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true); // Formato ISO 8601 UTC

        // 2. Construir QualifyingProperties / SignedProperties
        // Para simplificar, usamos valores genéricos/estándar para Issuer y Serial en este motor de ejemplo,
        // o los extraemos. Dejamos marcadores estables.
        let signed_properties_xml = format!(
            r#"<xades:SignedProperties xmlns:xades="http://uri.etsi.org/01903/v1.3.2#" Id="{}"><xades:SignedSignatureProperties><xades:SigningTime>{}</xades:SigningTime><xades:SigningCertificate><xades:Cert><xades:CertDigest><ds:DigestMethod xmlns:ds="http://www.w3.org/2000/09/xmldsig#" Algorithm="http://www.w3.org/2001/04/xmldsig-more#sha384"/><ds:DigestValue xmlns:ds="http://www.w3.org/2000/09/xmldsig#">{}</ds:DigestValue></xades:CertDigest><xades:IssuerSerial><ds:X509IssuerName xmlns:ds="http://www.w3.org/2000/09/xmldsig#">C=CO,L=Bogota,O=DIAN,CN=Subca de Facturacion</ds:X509IssuerName><ds:X509SerialNumber xmlns:ds="http://www.w3.org/2000/09/xmldsig#">123456789</ds:X509SerialNumber></xades:IssuerSerial></xades:Cert></xades:SigningCertificate><xades:SignaturePolicyIdentifier><xades:SignaturePolicyId><xades:SigPolicyId><xades:Identifier>https://facturaelectronica.dian.gov.co/politicadefirma/v2/politicadefirmav2.pdf</xades:Identifier><xades:Description>Politica de firma para facturas electronicas de la Republica de Colombia</xades:Description></xades:SigPolicyId><xades:SigPolicyHash><ds:DigestMethod xmlns:ds="http://www.w3.org/2000/09/xmldsig#" Algorithm="http://www.w3.org/2001/04/xmldsig-more#sha384"/><ds:DigestValue xmlns:ds="http://www.w3.org/2000/09/xmldsig#">dMoQAPcKeQZssgOD5Wgd5CCaaZssgOD5Wgd5CCaaZss=</ds:DigestValue></xades:SigPolicyHash></xades:SignaturePolicyId></xades:SignaturePolicyIdentifier></xades:SignedSignatureProperties></xades:SignedProperties>"#,
            signed_props_id, signing_time, cert_digest_b64
        );

        // Canonicalizar SignedProperties
        let signed_properties_c14n = canonicalize_xml(&signed_properties_xml)?;
        let mut sp_hasher = Sha384::new();
        sp_hasher.update(signed_properties_c14n.as_bytes());
        let sp_hash = sp_hasher.finalize();
        let signed_properties_digest_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, sp_hash);

        // 3. Construir KeyInfo
        let key_info_xml = format!(
            r#"<ds:KeyInfo xmlns:ds="http://www.w3.org/2000/09/xmldsig#" Id="{}"><ds:X509Data><ds:X509Certificate>{}</ds:X509Certificate></ds:X509Data></ds:KeyInfo>"#,
            key_info_id, cert_b64
        );

        // Canonicalizar KeyInfo
        let key_info_c14n = canonicalize_xml(&key_info_xml)?;
        let mut ki_hasher = Sha384::new();
        ki_hasher.update(key_info_c14n.as_bytes());
        let ki_hash = ki_hasher.finalize();
        let key_info_digest_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, ki_hash);

        // 4. Calcular el digest del documento XML crudo (sin la firma)
        let xml_c14n = canonicalize_xml(xml_content)?;
        let mut doc_hasher = Sha384::new();
        doc_hasher.update(xml_c14n.as_bytes());
        let doc_hash = doc_hasher.finalize();
        let doc_digest_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, doc_hash);

        // 5. Construir SignedInfo
        let signed_info_xml = format!(
            r#"<ds:SignedInfo xmlns:ds="http://www.w3.org/2000/09/xmldsig#"><ds:CanonicalizationMethod Algorithm="http://www.w3.org/2006/12/xml-c14n11"/><ds:SignatureMethod Algorithm="http://www.w3.org/2001/04/xmldsig-more#rsa-sha384"/><ds:Reference URI=""><ds:Transforms><ds:Transform Algorithm="http://www.w3.org/2000/09/xmldsig#enveloped-signature"/></ds:Transforms><ds:DigestMethod Algorithm="http://www.w3.org/2001/04/xmldsig-more#sha384"/><ds:DigestValue>{}</ds:DigestValue></ds:Reference><ds:Reference URI="#{}"><ds:DigestMethod Algorithm="http://www.w3.org/2001/04/xmldsig-more#sha384"/><ds:DigestValue>{}</ds:DigestValue></ds:Reference><ds:Reference Type="http://uri.etsi.org/01903#SignedProperties" URI="#{}"><ds:DigestMethod Algorithm="http://www.w3.org/2001/04/xmldsig-more#sha384"/><ds:DigestValue>{}</ds:DigestValue></ds:Reference></ds:SignedInfo>"#,
            doc_digest_b64, key_info_id, key_info_digest_b64, signed_props_id, signed_properties_digest_b64
        );

        // Canonicalizar SignedInfo
        let signed_info_c14n = canonicalize_xml(&signed_info_xml)?;

        // Calcular hash de SignedInfo para firmar
        let mut si_hasher = Sha384::new();
        si_hasher.update(signed_info_c14n.as_bytes());
        let si_hash = si_hasher.finalize();

        // 6. Firmar el hash con el proveedor (Vault o Local)
        let signature_bytes = self.provider.sign_hash(key_ref, &si_hash).await?;
        let signature_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, signature_bytes);

        // 7. Ensamble final del bloque de firma XML
        let signature_block = format!(
            r#"<ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#" Id="{}">{}<ds:SignatureValue>{}</ds:SignatureValue>{}<ds:Object><xades:QualifyingProperties xmlns:xades="http://uri.etsi.org/01903/v1.3.2#" Target="#{}">{}</xades:QualifyingProperties></ds:Object></ds:Signature>"#,
            sig_id, signed_info_xml, signature_b64, key_info_xml, sig_id, signed_properties_xml
        );

        // 8. Reemplazar el placeholder de la firma en el XML de entrada
        let signed_xml = xml_content.replace("<!-- PLACEHOLDER_FOR_SIGNATURE -->", &signature_block);
        
        Ok(signed_xml)
    }
}

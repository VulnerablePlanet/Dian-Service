//! Verificación de firmas XML-DSig/XAdES con `xmlsec1`, la implementación de referencia.
//!
//! Además de delegar la verificación criptográfica y de la cadena de certificados en
//! `xmlsec1`, aquí se imponen reglas propias:
//! - El documento contiene exactamente un `ds:Signature` (no se admiten firmas múltiples
//!   que permitan ataques de "signature wrapping").
//! - Esa firma referencia el documento completo (`URI=""` con transformación enveloped).
//! - Solo se resuelven referencias al mismo documento: nunca se descarga nada externo.

use crate::error::CryptoError;
use crate::xades::{ALG_ENVELOPED, NS_DS};
use async_trait::async_trait;
use core_domain::ports::{DomainError, SignatureVerifier, VerifiedSignature};
use base64::Engine;
use x509_cert::der::Decode;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::NsReader;
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

/// Tamaño máximo del documento a verificar.
pub const MAX_DOCUMENT_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct XmlSignatureVerifier {
    binary: PathBuf,
    trusted_ca_pem: PathBuf,
    intermediates_pem: Option<PathBuf>,
}

impl XmlSignatureVerifier {
    pub fn new(trusted_ca_pem: impl Into<PathBuf>) -> Self {
        Self {
            binary: PathBuf::from("xmlsec1"),
            trusted_ca_pem: trusted_ca_pem.into(),
            intermediates_pem: None,
        }
    }

    pub fn with_intermediates(mut self, intermediates_pem: impl Into<PathBuf>) -> Self {
        self.intermediates_pem = Some(intermediates_pem.into());
        self
    }

    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Comprueba que `xmlsec1` y el bundle de CAs estén disponibles (para fallar al arrancar).
    pub fn check_ready(&self) -> Result<(), CryptoError> {
        Command::new(&self.binary)
            .arg("--version")
            .output()
            .map_err(|e| CryptoError::Provider(format!("xmlsec1 not available: {}", e)))?;
        if !self.trusted_ca_pem.is_file() {
            return Err(CryptoError::Provider(format!(
                "Trusted CA bundle not found: {}",
                self.trusted_ca_pem.display()
            )));
        }
        Ok(())
    }

    /// Verifica la firma del documento. Es una operación bloqueante (lanza un proceso): en
    /// código asíncrono debe ejecutarse con `spawn_blocking`.
    pub fn verify(&self, xml: &str) -> Result<(), CryptoError> {
        if xml.len() > MAX_DOCUMENT_BYTES {
            return Err(CryptoError::Signing("Document too large to verify".to_string()));
        }
        check_signature_structure(xml)?;

        let dir = std::env::temp_dir().join(format!("dian-verify-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).map_err(|e| CryptoError::Provider(format!("temp dir: {}", e)))?;
        let file = dir.join("document.xml");
        let result = std::fs::write(&file, xml)
            .map_err(|e| CryptoError::Provider(format!("temp file: {}", e)))
            .and_then(|_| self.run_xmlsec1(&file));
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    fn run_xmlsec1(&self, file: &std::path::Path) -> Result<(), CryptoError> {
        let mut command = Command::new(&self.binary);
        command
            .arg("--verify")
            .arg("--enabled-reference-uris")
            .arg("empty,same-doc")
            .arg("--enabled-key-data")
            .arg("x509")
            .arg("--trusted-pem")
            .arg(&self.trusted_ca_pem)
            .arg("--id-attr:Id")
            .arg(format!("{}:KeyInfo", NS_DS))
            .arg("--id-attr:Id")
            .arg("http://uri.etsi.org/01903/v1.3.2#:SignedProperties")
            .arg("--id-attr:Id")
            .arg("http://uri.etsi.org/01903/v1.4.1#:SignedProperties");
        if let Some(intermediates) = &self.intermediates_pem {
            command.arg("--untrusted-pem").arg(intermediates);
        }
        let output = command
            .arg(file)
            .output()
            .map_err(|e| CryptoError::Provider(format!("could not run xmlsec1: {}", e)))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .filter(|l| l.contains("error") || l.contains("FAIL") || l.contains("failed"))
            .take(3)
            .collect::<Vec<_>>()
            .join(" | ");
        Err(CryptoError::Signing(format!(
            "Signature verification failed{}",
            if reason.is_empty() { String::new() } else { format!(": {}", reason) }
        )))
    }
}

#[async_trait]
impl SignatureVerifier for XmlSignatureVerifier {
    async fn verify(&self, xml: &str) -> Result<VerifiedSignature, DomainError> {
        let verifier = self.clone();
        let xml = xml.to_string();
        tokio::task::spawn_blocking(move || {
            XmlSignatureVerifier::verify(&verifier, &xml)?;
            Ok(VerifiedSignature {
                signer_subject: signer_subject(&xml)?,
            })
        })
            .await
            .map_err(|e| DomainError::Internal(format!("signature verification task failed: {}", e)))?
            .map_err(|e| match e {
                // Firma ausente, inválida, no confiable o estructura no admitida: culpa del documento.
                CryptoError::Signing(message) | CryptoError::Xml(message) => DomainError::Validation(message),
                other => DomainError::Crypto(other.to_string()),
            })
    }
}

/// Sujeto del certificado firmante: el certificado de `ds:KeyInfo` que no emitió a ningún
/// otro de los incluidos (el de entidad final cuando viaja la cadena).
pub fn signer_subject(xml: &str) -> Result<String, CryptoError> {
    let mut reader = NsReader::from_str(xml);
    let mut in_key_info = false;
    let mut in_certificate = false;
    let mut encoded: Vec<String> = Vec::new();
    loop {
        let (resolved, event) = reader
            .read_resolved_event()
            .map_err(|e| CryptoError::Xml(format!("Invalid XML: {}", e)))?;
        let is_ds = matches!(resolved, ResolveResult::Bound(ns) if ns.as_ref() == NS_DS);
        match event {
            Event::Start(ref e) if is_ds && e.local_name().as_ref() == "KeyInfo" => in_key_info = true,
            Event::End(ref e) if is_ds && e.local_name().as_ref() == "KeyInfo" => in_key_info = false,
            Event::Start(ref e) if in_key_info && is_ds && e.local_name().as_ref() == "X509Certificate" => {
                in_certificate = true;
                encoded.push(String::new());
            }
            Event::End(ref e) if is_ds && e.local_name().as_ref() == "X509Certificate" => in_certificate = false,
            Event::Text(ref t) if in_certificate => {
                let text: &str = t.as_ref();
                if let Some(last) = encoded.last_mut() {
                    last.push_str(text);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    let mut names = Vec::new();
    for b64 in &encoded {
        let compact: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(compact)
            .map_err(|e| CryptoError::Signing(format!("KeyInfo certificate is not valid Base64: {}", e)))?;
        let certificate = x509_cert::Certificate::from_der(&der)
            .map_err(|e| CryptoError::Signing(format!("KeyInfo certificate is not valid X.509: {}", e)))?;
        names.push((
            certificate.tbs_certificate.subject.to_string(),
            certificate.tbs_certificate.issuer.to_string(),
        ));
    }
    let leaves: Vec<&String> = names
        .iter()
        .filter(|(subject, _)| !names.iter().any(|(other, issuer)| issuer == subject && other != subject))
        .map(|(subject, _)| subject)
        .collect();
    match leaves.as_slice() {
        [leaf] => Ok((*leaf).clone()),
        [] => Err(CryptoError::Signing("The signature does not include the signer certificate".to_string())),
        _ => Err(CryptoError::Signing("The signature includes more than one end-entity certificate".to_string())),
    }
}

/// Exige exactamente un `ds:Signature` y que una de sus referencias cubra el documento
/// completo con la transformación enveloped.
pub fn check_signature_structure(xml: &str) -> Result<(), CryptoError> {
    let mut reader = NsReader::from_str(xml);
    let mut signatures = 0;
    let mut path: Vec<String> = Vec::new();
    let mut whole_document_reference = false;
    let mut in_whole_reference = false;
    let mut enveloped = false;

    loop {
        let (resolved, event) = reader
            .read_resolved_event()
            .map_err(|e| CryptoError::Xml(format!("Invalid XML: {}", e)))?;
        let is_ds = matches!(resolved, ResolveResult::Bound(ns) if ns.as_ref() == NS_DS);
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let local = e.local_name().as_ref().to_string();
                if is_ds && local == "Signature" {
                    signatures += 1;
                }
                if is_ds && local == "Reference" && path.last().map(String::as_str) == Some("ds:SignedInfo") {
                    let uri = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == "URI")
                        .map(|a| a.value.into_owned());
                    in_whole_reference = uri.as_deref() == Some("");
                }
                if is_ds && local == "Transform" && in_whole_reference {
                    let algorithm = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == "Algorithm")
                        .map(|a| a.value.into_owned());
                    if algorithm.as_deref() == Some(ALG_ENVELOPED) {
                        enveloped = true;
                    }
                }
                if matches!(event, Event::Start(_)) {
                    path.push(if is_ds { format!("ds:{}", local) } else { local });
                }
            }
            Event::End(_) => {
                if path.last().map(String::as_str) == Some("ds:Reference") {
                    if in_whole_reference && enveloped {
                        whole_document_reference = true;
                    }
                    in_whole_reference = false;
                    enveloped = false;
                }
                path.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }

    match signatures {
        0 => Err(CryptoError::Signing("The document is not signed".to_string())),
        1 if whole_document_reference => Ok(()),
        1 => Err(CryptoError::Signing(
            "The signature does not cover the whole document (URI=\"\" with enveloped transform)".to_string(),
        )),
        n => Err(CryptoError::Signing(format!("The document contains {} signatures; exactly one is required", n))),
    }
}

use async_trait::async_trait;
use core_domain::ports::{DianResponse, DianSoapClient, DomainError};
use reqwest::Client;
use std::io::Cursor;
use crate::error::SoapError;
use crate::soap::build_soap_envelope;

pub struct DianSoapClientImpl {
    client: Client,
    hab_url: String,
    prod_url: String,
    binary_security_token: String,
}

impl DianSoapClientImpl {
    pub fn new(
        binary_security_token: String,
        hab_url: Option<String>,
        prod_url: Option<String>,
    ) -> Self {
        let client = Client::builder()
            .build()
            .expect("Failed to initialize HTTP client for SOAP");

        let hab_url = hab_url.unwrap_or_else(|| "https://vpfe-hab.dian.gov.co/WcfDianCustomerServices.svc".to_string());
        let prod_url = prod_url.unwrap_or_else(|| "https://vpfe.dian.gov.co/WcfDianCustomerServices.svc".to_string());

        Self {
            client,
            hab_url,
            prod_url,
            binary_security_token,
        }
    }

    fn get_url(&self, is_hab: bool) -> &str {
        if is_hab {
            &self.hab_url
        } else {
            &self.prod_url
        }
    }

    /// Extrae el nombre del archivo XML dentro del ZIP para reconstruir el nombre del archivo ZIP.
    fn extract_zip_filename(&self, zip_bytes: &[u8]) -> Result<String, SoapError> {
        let cursor = Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(cursor)
            .map_err(|e| SoapError::Zip(format!("Failed to read ZIP in memory: {}", e)))?;
        
        if archive.is_empty() {
            return Err(SoapError::Zip("ZIP archive is empty".to_string()));
        }

        let file = archive
            .by_index(0)
            .map_err(|e| SoapError::Zip(format!("Failed to get ZIP file element: {}", e)))?;
        
        let xml_name = file.name();
        let zip_name = xml_name.replace(".xml", ".zip");
        Ok(zip_name)
    }
}

#[async_trait]
impl DianSoapClient for DianSoapClientImpl {
    async fn send_bill_sync(
        &self,
        signed_xml_zip: &[u8],
        is_hab: bool,
    ) -> Result<DianResponse, DomainError> {
        let to_url = self.get_url(is_hab);
        let zip_filename = self.extract_zip_filename(signed_xml_zip)?;
        
        // Codificar el ZIP en Base64 para el SOAP body
        let zip_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, signed_xml_zip);
        
        let action = "http://wcf.dian.colombia/IWcfDianCustomerServices/SendBillSync";
        let body_content = format!(
            r#"<wcf:SendBillSync><wcf:fileName>{}</wcf:fileName><wcf:contentData>{}</wcf:contentData></wcf:SendBillSync>"#,
            zip_filename, zip_b64
        );

        let soap_envelope = build_soap_envelope(action, to_url, &self.binary_security_token, &body_content);

        // Enviar petición HTTP POST
        let response = self.client
            .post(to_url)
            .header("Content-Type", "application/soap+xml;charset=utf-8")
            .body(soap_envelope)
            .send()
            .await
            .map_err(SoapError::from)?;

        let status = response.status();
        let resp_xml = response.text().await.map_err(SoapError::from)?;

        if !status.is_success() {
            if let Some(fault_reason) = extract_tag_content(&resp_xml, "Text") {
                let fault_code = extract_tag_content(&resp_xml, "Value").unwrap_or_else(|| "SoapFault".to_string());
                return Err(SoapError::Fault { code: fault_code, message: fault_reason }.into());
            }
            return Err(SoapError::Dian(format!("HTTP error {}: {}", status, resp_xml)).into());
        }

        // Parsear los resultados del XML retornado por la DIAN
        let status_code = extract_tag_content(&resp_xml, "StatusCode")
            .unwrap_or_else(|| "unknown".to_string());
        
        let status_message = extract_tag_content(&resp_xml, "StatusMessage")
            .unwrap_or_else(|| "No status message provided".to_string());

        let soap_trace_id = extract_tag_content(&resp_xml, "XmlDocumentKey");

        Ok(DianResponse {
            status_code,
            message: status_message,
            xml_response: Some(resp_xml),
            soap_trace_id,
        })
    }

    async fn send_bill_async(
        &self,
        signed_xml_zip: &[u8],
        is_hab: bool,
    ) -> Result<String, DomainError> {
        let to_url = self.get_url(is_hab);
        let zip_filename = self.extract_zip_filename(signed_xml_zip)?;
        
        let zip_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, signed_xml_zip);
        
        let action = "http://wcf.dian.colombia/IWcfDianCustomerServices/SendBillAsync";
        let body_content = format!(
            r#"<wcf:SendBillAsync><wcf:fileName>{}</wcf:fileName><wcf:contentData>{}</wcf:contentData></wcf:SendBillAsync>"#,
            zip_filename, zip_b64
        );

        let soap_envelope = build_soap_envelope(action, to_url, &self.binary_security_token, &body_content);

        let response = self.client
            .post(to_url)
            .header("Content-Type", "application/soap+xml;charset=utf-8")
            .body(soap_envelope)
            .send()
            .await
            .map_err(SoapError::from)?;

        let resp_xml = response.text().await.map_err(SoapError::from)?;

        let zip_key = extract_tag_content(&resp_xml, "ZipKey")
            .ok_or_else(|| SoapError::Dian("Response did not contain ZipKey/TrackId".to_string()))?;

        Ok(zip_key)
    }

    async fn send_test_set_async(
        &self,
        signed_xml_zip: &[u8],
        test_set_id: &str,
        is_hab: bool,
    ) -> Result<String, DomainError> {
        let to_url = self.get_url(is_hab);
        let zip_filename = self.extract_zip_filename(signed_xml_zip)?;
        
        let zip_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, signed_xml_zip);
        
        let action = "http://wcf.dian.colombia/IWcfDianCustomerServices/SendTestSetAsync";
        let body_content = format!(
            r#"<wcf:SendTestSetAsync><wcf:fileName>{}</wcf:fileName><wcf:contentData>{}</wcf:contentData><wcf:testSetId>{}</wcf:testSetId></wcf:SendTestSetAsync>"#,
            zip_filename, zip_b64, test_set_id
        );

        let soap_envelope = build_soap_envelope(action, to_url, &self.binary_security_token, &body_content);

        let response = self.client
            .post(to_url)
            .header("Content-Type", "application/soap+xml;charset=utf-8")
            .body(soap_envelope)
            .send()
            .await
            .map_err(SoapError::from)?;

        let resp_xml = response.text().await.map_err(SoapError::from)?;

        let zip_key = extract_tag_content(&resp_xml, "ZipKey")
            .ok_or_else(|| SoapError::Dian("Response did not contain ZipKey/TrackId".to_string()))?;

        Ok(zip_key)
    }

    async fn get_status(
        &self,
        track_id: &str,
        is_hab: bool,
    ) -> Result<DianResponse, DomainError> {
        let to_url = self.get_url(is_hab);
        
        let action = "http://wcf.dian.colombia/IWcfDianCustomerServices/GetStatus";
        let body_content = format!(
            r#"<wcf:GetStatus><wcf:trackId>{}</wcf:trackId></wcf:GetStatus>"#,
            track_id
        );

        let soap_envelope = build_soap_envelope(action, to_url, &self.binary_security_token, &body_content);

        let response = self.client
            .post(to_url)
            .header("Content-Type", "application/soap+xml;charset=utf-8")
            .body(soap_envelope)
            .send()
            .await
            .map_err(SoapError::from)?;

        let resp_xml = response.text().await.map_err(SoapError::from)?;

        let status_code = extract_tag_content(&resp_xml, "StatusCode")
            .unwrap_or_else(|| "unknown".to_string());
        
        let status_message = extract_tag_content(&resp_xml, "StatusMessage")
            .unwrap_or_else(|| "No status message provided".to_string());

        Ok(DianResponse {
            status_code,
            message: status_message,
            xml_response: Some(resp_xml),
            soap_trace_id: Some(track_id.to_string()),
        })
    }

    async fn get_status_zip(
        &self,
        track_id: &str,
        is_hab: bool,
    ) -> Result<DianResponse, DomainError> {
        let to_url = self.get_url(is_hab);
        
        let action = "http://wcf.dian.colombia/IWcfDianCustomerServices/GetStatusZip";
        let body_content = format!(
            r#"<wcf:GetStatusZip><wcf:trackId>{}</wcf:trackId></wcf:GetStatusZip>"#,
            track_id
        );

        let soap_envelope = build_soap_envelope(action, to_url, &self.binary_security_token, &body_content);

        let response = self.client
            .post(to_url)
            .header("Content-Type", "application/soap+xml;charset=utf-8")
            .body(soap_envelope)
            .send()
            .await
            .map_err(SoapError::from)?;

        let resp_xml = response.text().await.map_err(SoapError::from)?;

        let status_code = extract_tag_content(&resp_xml, "StatusCode")
            .unwrap_or_else(|| "unknown".to_string());
        
        let status_message = extract_tag_content(&resp_xml, "StatusMessage")
            .unwrap_or_else(|| "No status message provided".to_string());

        Ok(DianResponse {
            status_code,
            message: status_message,
            xml_response: Some(resp_xml),
            soap_trace_id: Some(track_id.to_string()),
        })
    }

    async fn send_event_update_status(
        &self,
        signed_event_zip: &[u8],
        is_hab: bool,
    ) -> Result<DianResponse, DomainError> {
        let to_url = self.get_url(is_hab);
        
        let zip_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, signed_event_zip);
        
        let action = "http://wcf.dian.colombia/IWcfDianCustomerServices/SendEventUpdateStatus";
        let body_content = format!(
            r#"<wcf:SendEventUpdateStatus><wcf:contentData>{}</wcf:contentData></wcf:SendEventUpdateStatus>"#,
            zip_b64
        );

        let soap_envelope = build_soap_envelope(action, to_url, &self.binary_security_token, &body_content);

        let response = self.client
            .post(to_url)
            .header("Content-Type", "application/soap+xml;charset=utf-8")
            .body(soap_envelope)
            .send()
            .await
            .map_err(SoapError::from)?;

        let status = response.status();
        let resp_xml = response.text().await.map_err(SoapError::from)?;

        if !status.is_success() {
            if let Some(fault_reason) = extract_tag_content(&resp_xml, "Text") {
                let fault_code = extract_tag_content(&resp_xml, "Value").unwrap_or_else(|| "SoapFault".to_string());
                return Err(SoapError::Fault { code: fault_code, message: fault_reason }.into());
            }
            return Err(SoapError::Dian(format!("HTTP error {}: {}", status, resp_xml)).into());
        }

        let status_code = extract_tag_content(&resp_xml, "StatusCode")
            .unwrap_or_else(|| "unknown".to_string());
        
        let status_message = extract_tag_content(&resp_xml, "StatusMessage")
            .unwrap_or_else(|| "No status message provided".to_string());

        let soap_trace_id = extract_tag_content(&resp_xml, "XmlDocumentKey");

        Ok(DianResponse {
            status_code,
            message: status_message,
            xml_response: Some(resp_xml),
            soap_trace_id,
        })
    }
}

/// Extrae de forma robusta y rápida el contenido de una etiqueta XML sin importar su prefijo.
fn extract_tag_content(xml: &str, tag_name: &str) -> Option<String> {
    let mut cursor = 0;
    while let Some(pos) = xml[cursor..].find('<') {
        let tag_start = cursor + pos + 1;
        let tag_rest = &xml[tag_start..];
        
        let space_or_close = tag_rest.find(|c| c == ' ' || c == '>')?;
        let full_tag = &tag_rest[..space_or_close];
        
        let matches = if full_tag.contains(':') {
            full_tag.split(':').last() == Some(tag_name)
        } else {
            full_tag == tag_name
        };

        if matches {
            let content_start = tag_start + tag_rest.find('>')? + 1;
            let rest_content = &xml[content_start..];
            let end_pos = rest_content.find("</")?;
            let content = &rest_content[..end_pos];
            return Some(content.trim().to_string());
        }
        cursor = tag_start;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_tag_content() {
        let xml = r#"<soap:Envelope><soap:Body><wcf:SendBillSyncResponse><wcf:SendBillSyncResult><b:StatusCode>00</b:StatusCode><b:StatusMessage>Aceptado</b:StatusMessage></wcf:SendBillSyncResult></wcf:SendBillSyncResponse></soap:Body></soap:Envelope>"#;
        
        assert_eq!(extract_tag_content(xml, "StatusCode").unwrap(), "00");
        assert_eq!(extract_tag_content(xml, "StatusMessage").unwrap(), "Aceptado");
        assert!(extract_tag_content(xml, "NonExistentTag").is_none());
    }
}

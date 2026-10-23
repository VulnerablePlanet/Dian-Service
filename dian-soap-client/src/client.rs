use async_trait::async_trait;
use core_domain::ports::{CryptoSigner, DianResponse, DianSoapClient, DomainError};
use reqwest::Client;
use std::io::Cursor;
use std::sync::Arc;
use crate::error::SoapError;
use crate::soap::{build_soap_envelope, xml_escape};
use std::time::Duration;
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
const ACTION_PREFIX: &str = "http://wcf.dian.colombia/IWcfDianCustomerServices/";

/// Cliente del servicio web de la DIAN. Cada envelope lleva una cabecera WS-Security firmada
/// con el certificado del tenant que emite (ver `CryptoSigner::sign_soap_for_tenant`).
pub struct DianSoapClientImpl {
    client: Client,
    hab_url: String,
    prod_url: String,
    signer: Arc<dyn CryptoSigner>,
}

impl DianSoapClientImpl {
    pub fn new(signer: Arc<dyn CryptoSigner>, hab_url: Option<String>, prod_url: Option<String>) -> Self {
        // Sin timeout, una DIAN que acepta la conexión y no responde retiene indefinidamente
        // el permiso de concurrencia del worker.
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("Failed to initialize HTTP client for SOAP");

        let hab_url = hab_url.unwrap_or_else(|| "https://vpfe-hab.dian.gov.co/WcfDianCustomerServices.svc".to_string());
        let prod_url = prod_url.unwrap_or_else(|| "https://vpfe.dian.gov.co/WcfDianCustomerServices.svc".to_string());

        Self {
            client,
            hab_url,
            prod_url,
            signer,
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

        let xml_name = file
            .name()
            .map_err(|e| SoapError::Zip(format!("Invalid ZIP entry name: {}", e)))?;
        let zip_name = xml_name.replace(".xml", ".zip");
        Ok(zip_name)
    }

    /// Construye el envelope, lo firma con el certificado del tenant, lo envía y devuelve el
    /// XML de respuesta. Una respuesta HTTP no exitosa se convierte en error.
    async fn call(&self, tenant_id: Uuid, operation: &str, is_hab: bool, body_content: &str) -> Result<String, DomainError> {
        let to_url = self.get_url(is_hab);
        let action = format!("{}{}", ACTION_PREFIX, operation);
        let envelope = build_soap_envelope(&action, to_url, body_content);
        let signed_envelope = self.signer.sign_soap_for_tenant(tenant_id, &envelope).await?;

        let response = self
            .client
            .post(to_url)
            .header("Content-Type", format!("application/soap+xml;charset=utf-8;action=\"{}\"", action))
            .body(signed_envelope)
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
            return Err(SoapError::Dian(format!("HTTP error {}", status)).into());
        }
        Ok(resp_xml)
    }

    /// Envía un ZIP (`fileName` + `contentData` en Base64), con elementos adicionales opcionales.
    async fn send_zip(
        &self,
        tenant_id: Uuid,
        operation: &str,
        zip: &[u8],
        is_hab: bool,
        extra: &str,
    ) -> Result<String, DomainError> {
        let zip_filename = self.extract_zip_filename(zip)?;
        let zip_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, zip);
        let body_content = format!(
            "<wcf:{op}><wcf:fileName>{}</wcf:fileName><wcf:contentData>{}</wcf:contentData>{}</wcf:{op}>",
            xml_escape(&zip_filename),
            zip_b64,
            extra,
            op = operation,
        );
        self.call(tenant_id, operation, is_hab, &body_content).await
    }
}

/// Respuesta de procesamiento (`StatusCode`, `StatusMessage`...).
fn dian_response(resp_xml: String, soap_trace_id: Option<String>) -> DianResponse {
    let status_code = extract_tag_content(&resp_xml, "StatusCode").unwrap_or_else(|| "unknown".to_string());
    let status_message =
        extract_tag_content(&resp_xml, "StatusMessage").unwrap_or_else(|| "No status message provided".to_string());
    let soap_trace_id = soap_trace_id.or_else(|| extract_tag_content(&resp_xml, "XmlDocumentKey"));
    DianResponse {
        status_code,
        message: status_message,
        xml_response: Some(resp_xml),
        soap_trace_id,
    }
}

fn zip_key(resp_xml: &str) -> Result<String, DomainError> {
    extract_tag_content(resp_xml, "ZipKey")
        .ok_or_else(|| SoapError::Dian("Response did not contain ZipKey/TrackId".to_string()).into())
}

#[async_trait]
impl DianSoapClient for DianSoapClientImpl {
    async fn send_bill_sync(&self, tenant_id: Uuid, signed_xml_zip: &[u8], is_hab: bool) -> Result<DianResponse, DomainError> {
        let resp_xml = self.send_zip(tenant_id, "SendBillSync", signed_xml_zip, is_hab, "").await?;
        Ok(dian_response(resp_xml, None))
    }

    async fn send_bill_async(&self, tenant_id: Uuid, signed_xml_zip: &[u8], is_hab: bool) -> Result<String, DomainError> {
        zip_key(&self.send_zip(tenant_id, "SendBillAsync", signed_xml_zip, is_hab, "").await?)
    }

    async fn send_test_set_async(
        &self,
        tenant_id: Uuid,
        signed_xml_zip: &[u8],
        test_set_id: &str,
        is_hab: bool,
    ) -> Result<String, DomainError> {
        let extra = format!("<wcf:testSetId>{}</wcf:testSetId>", xml_escape(test_set_id));
        zip_key(&self.send_zip(tenant_id, "SendTestSetAsync", signed_xml_zip, is_hab, &extra).await?)
    }

    async fn get_status(&self, tenant_id: Uuid, track_id: &str, is_hab: bool) -> Result<DianResponse, DomainError> {
        let body = format!("<wcf:GetStatus><wcf:trackId>{}</wcf:trackId></wcf:GetStatus>", xml_escape(track_id));
        let resp_xml = self.call(tenant_id, "GetStatus", is_hab, &body).await?;
        Ok(dian_response(resp_xml, Some(track_id.to_string())))
    }

    async fn get_status_zip(&self, tenant_id: Uuid, track_id: &str, is_hab: bool) -> Result<DianResponse, DomainError> {
        let body = format!("<wcf:GetStatusZip><wcf:trackId>{}</wcf:trackId></wcf:GetStatusZip>", xml_escape(track_id));
        let resp_xml = self.call(tenant_id, "GetStatusZip", is_hab, &body).await?;
        Ok(dian_response(resp_xml, Some(track_id.to_string())))
    }

    async fn send_event_update_status(
        &self,
        tenant_id: Uuid,
        signed_event_zip: &[u8],
        is_hab: bool,
    ) -> Result<DianResponse, DomainError> {
        let zip_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, signed_event_zip);
        let body = format!(
            "<wcf:SendEventUpdateStatus><wcf:contentData>{}</wcf:contentData></wcf:SendEventUpdateStatus>",
            zip_b64
        );
        let resp_xml = self.call(tenant_id, "SendEventUpdateStatus", is_hab, &body).await?;
        Ok(dian_response(resp_xml, None))
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
    use core_domain::ports::WSSE_SECURITY_PLACEHOLDER;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Firmador de prueba: deja constancia del tenant en la cabecera.
    struct TenantMarker;

    #[async_trait]
    impl CryptoSigner for TenantMarker {
        async fn sign_for_tenant(&self, _: Uuid, _: &str) -> Result<String, DomainError> {
            unreachable!()
        }
        async fn sign_soap_for_tenant(&self, tenant_id: Uuid, envelope: &str) -> Result<String, DomainError> {
            Ok(envelope.replace(WSSE_SECURITY_PLACEHOLDER, &format!("<signed-for>{}</signed-for>", tenant_id)))
        }
    }

    /// Servidor HTTP de un solo uso que responde `status` + `body` y guarda la petición.
    async fn one_shot_server(status: &'static str, body: &'static str) -> (String, Arc<Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/WcfDianCustomerServices.svc", listener.local_addr().unwrap());
        let request = Arc::new(Mutex::new(String::new()));
        let captured = request.clone();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut data = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                data.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&data);
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let length = text[..head_end]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    if data.len() >= head_end + 4 + length {
                        break;
                    }
                }
            }
            *captured.lock().unwrap() = String::from_utf8_lossy(&data).into_owned();
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/soap+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        (url, request)
    }

    #[tokio::test]
    async fn requests_are_signed_for_the_tenant_and_responses_parsed() {
        let (url, request) = one_shot_server(
            "200 OK",
            "<s:Envelope><s:Body><b:StatusCode>00</b:StatusCode><b:StatusMessage>Procesado</b:StatusMessage></s:Body></s:Envelope>",
        )
        .await;
        let client = DianSoapClientImpl::new(Arc::new(TenantMarker), Some(url), None);
        let tenant = Uuid::new_v4();

        let response = client.get_status(tenant, "track<1>", true).await.unwrap();

        assert_eq!(response.status_code, "00");
        assert_eq!(response.soap_trace_id.as_deref(), Some("track<1>"));
        let request = request.lock().unwrap().clone();
        assert!(request.contains(&format!("<signed-for>{}</signed-for>", tenant)));
        assert!(request.contains("<wsa:Action>http://wcf.dian.colombia/IWcfDianCustomerServices/GetStatus</wsa:Action>"));
        assert!(request.contains("<wcf:trackId>track&lt;1&gt;</wcf:trackId>"));
        assert!(!request.contains(WSSE_SECURITY_PLACEHOLDER));
    }

    #[tokio::test]
    async fn soap_faults_become_errors() {
        let (url, _) = one_shot_server(
            "500 Internal Server Error",
            "<s:Envelope><s:Body><s:Fault><s:Code><s:Value>s:Sender</s:Value></s:Code><s:Reason><s:Text>Invalid security</s:Text></s:Reason></s:Fault></s:Body></s:Envelope>",
        )
        .await;
        let client = DianSoapClientImpl::new(Arc::new(TenantMarker), None, Some(url));
        let error = client.get_status_zip(Uuid::new_v4(), "t", false).await.unwrap_err();
        assert!(error.to_string().contains("Invalid security"), "{}", error);
    }

    #[test]
    fn test_extract_tag_content() {
        let xml = r#"<soap:Envelope><soap:Body><wcf:SendBillSyncResponse><wcf:SendBillSyncResult><b:StatusCode>00</b:StatusCode><b:StatusMessage>Aceptado</b:StatusMessage></wcf:SendBillSyncResult></wcf:SendBillSyncResponse></soap:Body></soap:Envelope>"#;
        
        assert_eq!(extract_tag_content(xml, "StatusCode").unwrap(), "00");
        assert_eq!(extract_tag_content(xml, "StatusMessage").unwrap(), "Aceptado");
        assert!(extract_tag_content(xml, "NonExistentTag").is_none());
    }
}

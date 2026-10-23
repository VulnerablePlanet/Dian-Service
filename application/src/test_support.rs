//! Dobles de prueba compartidos por los tests de los casos de uso.

use async_trait::async_trait;
use chrono::{Duration, Utc};
use core_domain::models::{DocumentType, NumberingRange, Tenant};
use core_domain::ports::{CryptoSigner, DianResponse, DianSoapClient, DomainError};
use core_domain::testing::InMemoryStore;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use ubl_engine::payload::{InvoicePayload, PayrollPayload, SupportDocumentPayload};
use uuid::Uuid;

pub fn store_with_tenant() -> (Arc<InMemoryStore>, Tenant) {
    let tenant = Tenant {
        id: Uuid::new_v4(),
        tax_id: "900123456".into(),
        registration_name: "Tenant SAS".into(),
        tax_regime: "48".into(),
        address_info: serde_json::json!({}),
        reception_email: "f@t.co".into(),
        api_keys: serde_json::json!({ "secret": "k" }),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let store = Arc::new(InMemoryStore::default());
    store.add_tenant(tenant.clone());
    (store, tenant)
}

pub fn range(tenant: &Tenant, document_type: DocumentType, prefix: &str, from: i32) -> NumberingRange {
    NumberingRange {
        id: Uuid::new_v4(),
        tenant_id: tenant.id,
        document_type,
        prefix: prefix.into(),
        from_number: from,
        to_number: from + 1000,
        technical_key: "tk-from-range".into(),
        valid_date_from: Utc::now() - Duration::days(1),
        valid_date_to: Utc::now() + Duration::days(1),
        current_counter: 0,
    }
}

pub fn invoice_payload(number: i32) -> InvoicePayload {
    InvoicePayload {
        prefix: "FE".into(),
        number,
        issue_date: "2026-10-23".into(),
        issue_time: "10:42:00-05:00".into(),
        company_nit: "900123456".into(),
        company_name: "Tenant SAS".into(),
        customer_id_type: "31".into(),
        customer_id: "900999888".into(),
        customer_name: "Cliente SAS".into(),
        customer_email: "c@x.co".into(),
        net_amount: 100000.0,
        tax_amount: 19000.0,
        total_amount: 119000.0,
        environment: "2".into(),
        technical_key: None,
        document_type: "invoice".into(),
        test_set_id: None,
        lines: None,
        billing_reference: None,
        discrepancy: None,
        software_pin: None,
    }
}

pub fn payroll_payload(number: i32) -> PayrollPayload {
    PayrollPayload {
        prefix: "NOM".into(),
        number,
        issue_date: "2026-10-23".into(),
        issue_time: "08:00:00".into(),
        employer_nit: "900123456".into(),
        employer_name: "Tenant SAS".into(),
        employee_id_type: "13".into(),
        employee_id: "10203040".into(),
        employee_name: "Trabajador".into(),
        devengado: 1500000.0,
        deducido: 60000.0,
        total: 1440000.0,
        software_pin: "pin".into(),
        environment: "2".into(),
    }
}

pub fn support_payload() -> SupportDocumentPayload {
    SupportDocumentPayload {
        prefix: "DS".into(),
        number: 0,
        issue_date: "2026-10-23".into(),
        issue_time: "10:30:00-05:00".into(),
        seller_nit: "10203040".into(),
        seller_name: "No Obligado".into(),
        buyer_nit: "900123456".into(),
        buyer_name: "Tenant SAS".into(),
        net_amount: 100000.0,
        tax_amount: 19000.0,
        total_amount: 119000.0,
        software_pin: "pin".into(),
        environment: "2".into(),
        lines: None,
    }
}

/// Firmador falso: inserta un bloque de firma reconocible o falla si se le indica.
#[derive(Default)]
pub struct FakeSigner {
    pub fail: bool,
    pub calls: AtomicUsize,
}

#[async_trait]
impl CryptoSigner for FakeSigner {
    async fn sign_for_tenant(&self, tenant_id: Uuid, xml: &str) -> Result<String, DomainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(DomainError::Crypto(format!("Tenant {} has no active certificate", tenant_id)));
        }
        Ok(xml.replace(
            "<!-- PLACEHOLDER_FOR_SIGNATURE -->",
            "<ds:Signature><ds:SignatureValue>fake</ds:SignatureValue></ds:Signature>",
        ))
    }

    async fn sign_soap_for_tenant(&self, tenant_id: Uuid, envelope: &str) -> Result<String, DomainError> {
        Ok(envelope.replace(
            core_domain::ports::WSSE_SECURITY_PLACEHOLDER,
            &format!("<fake-wsse tenant=\"{}\"/>", tenant_id),
        ))
    }
}

pub fn ok(code: &str) -> Result<DianResponse, String> {
    Ok(DianResponse {
        status_code: code.into(),
        message: format!("status {}", code),
        xml_response: Some(format!("<resp code=\"{}\"/>", code)),
        soap_trace_id: Some("trace".into()),
    })
}

/// Cliente SOAP con respuestas programadas por operación. Sin respuesta programada:
/// `send_bill_sync` acepta, `get_status` responde "66" (no encontrado) y `get_status_zip`
/// responde "00".
#[derive(Default)]
pub struct ScriptedSoap {
    pub send: Mutex<VecDeque<Result<DianResponse, String>>>,
    pub status: Mutex<VecDeque<Result<DianResponse, String>>>,
    pub status_zip: Mutex<VecDeque<Result<DianResponse, String>>>,
    pub test_set: Mutex<VecDeque<Result<String, String>>>,
    pub send_calls: AtomicUsize,
    pub status_calls: AtomicUsize,
    pub status_zip_calls: AtomicUsize,
    pub last_zip: Mutex<Vec<u8>>,
    /// Tenant con cuya identidad se hizo la última llamada.
    pub last_tenant: Mutex<Option<Uuid>>,
}

impl ScriptedSoap {
    pub fn script_send(&self, responses: Vec<Result<DianResponse, String>>) {
        self.send.lock().unwrap().extend(responses);
    }
    pub fn script_status(&self, responses: Vec<Result<DianResponse, String>>) {
        self.status.lock().unwrap().extend(responses);
    }
    pub fn script_status_zip(&self, responses: Vec<Result<DianResponse, String>>) {
        self.status_zip.lock().unwrap().extend(responses);
    }
    pub fn calls(&self, counter: &AtomicUsize) -> usize {
        counter.load(Ordering::SeqCst)
    }
    /// XML del último documento transmitido (descomprimido del ZIP).
    pub fn last_xml(&self) -> String {
        use std::io::Read;
        let bytes = self.last_zip.lock().unwrap().clone();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        archive.by_index(0).unwrap().read_to_string(&mut xml).unwrap();
        xml
    }
}

fn next<T>(queue: &Mutex<VecDeque<Result<T, String>>>, default: Result<T, String>) -> Result<T, DomainError> {
    queue
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or(default)
        .map_err(DomainError::Soap)
}

#[async_trait]
impl DianSoapClient for ScriptedSoap {
    async fn send_bill_sync(&self, tenant_id: Uuid, zip: &[u8], _is_hab: bool) -> Result<DianResponse, DomainError> {
        self.send_calls.fetch_add(1, Ordering::SeqCst);
        *self.last_tenant.lock().unwrap() = Some(tenant_id);
        *self.last_zip.lock().unwrap() = zip.to_vec();
        next(&self.send, ok("00"))
    }
    async fn send_bill_async(&self, _tenant_id: Uuid, _zip: &[u8], _is_hab: bool) -> Result<String, DomainError> {
        Ok("track".into())
    }
    async fn send_test_set_async(&self, _tenant_id: Uuid, zip: &[u8], _id: &str, _is_hab: bool) -> Result<String, DomainError> {
        *self.last_zip.lock().unwrap() = zip.to_vec();
        next(&self.test_set, Ok("zip-key-1".into()))
    }
    async fn get_status(&self, _tenant_id: Uuid, _track_id: &str, _is_hab: bool) -> Result<DianResponse, DomainError> {
        self.status_calls.fetch_add(1, Ordering::SeqCst);
        next(&self.status, ok("66"))
    }
    async fn get_status_zip(&self, _tenant_id: Uuid, _track_id: &str, _is_hab: bool) -> Result<DianResponse, DomainError> {
        self.status_zip_calls.fetch_add(1, Ordering::SeqCst);
        next(&self.status_zip, ok("00"))
    }
    async fn send_event_update_status(&self, tenant_id: Uuid, _zip: &[u8], _is_hab: bool) -> Result<DianResponse, DomainError> {
        *self.last_tenant.lock().unwrap() = Some(tenant_id);
        next(&self.send, ok("00"))
    }
}

use chrono::Utc;
use core_domain::models::{
    Certificate, CertificateStatus, DianEvent, Document, DocumentStatus, DocumentType, EventStatus,
    NumberingRange, Tenant, Webhook, WebhookSubscriptionStatus,
};
use uuid::Uuid;

#[test]
fn test_tenant_serialization() {
    let tenant = Tenant {
        id: Uuid::new_v4(),
        tax_id: "900123456-7".to_string(),
        registration_name: "Empresa de Habilitación SAS".to_string(),
        tax_regime: "Regimen Comun".to_string(),
        address_info: serde_json::json!({
            "city": "Bogota",
            "address": "Calle 45 # 12-34"
        }),
        reception_email: "facturas@empresa.com".to_string(),
        api_keys: serde_json::json!({
            "prod": "key_abc123"
        }),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let serialized = serde_json::to_string(&tenant).expect("Failed to serialize Tenant");
    let deserialized: Tenant =
        serde_json::from_str(&serialized).expect("Failed to deserialize Tenant");

    assert_eq!(tenant.id, deserialized.id);
    assert_eq!(tenant.tax_id, deserialized.tax_id);
    assert_eq!(tenant.registration_name, deserialized.registration_name);
    assert_eq!(
        tenant.address_info.get("city").unwrap().as_str().unwrap(),
        "Bogota"
    );
}

#[test]
fn test_enums_serde_naming() {
    // CertStatus
    let cert_status = CertificateStatus::Active;
    let serialized_cert = serde_json::to_string(&cert_status).unwrap();
    assert_eq!(serialized_cert, "\"active\"");

    // DocType
    let doc_type = DocumentType::CreditNote;
    let serialized_type = serde_json::to_string(&doc_type).unwrap();
    assert_eq!(serialized_type, "\"credit_note\"");

    // DocStatus
    let doc_status = DocumentStatus::DianAccepted;
    let serialized_status = serde_json::to_string(&doc_status).unwrap();
    assert_eq!(serialized_status, "\"dian_accepted\"");

    // EventStatus
    let event_status = EventStatus::Error;
    let serialized_event = serde_json::to_string(&event_status).unwrap();
    assert_eq!(serialized_event, "\"error\"");

    // SubStatus
    let sub_status = WebhookSubscriptionStatus::Inactive;
    let serialized_sub = serde_json::to_string(&sub_status).unwrap();
    assert_eq!(serialized_sub, "\"inactive\"");
}

#[test]
fn test_document_serialization() {
    let document = Document {
        id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        document_type: DocumentType::Invoice,
        prefix: "SETP".to_string(),
        document_number: 99482,
        cufe_cude: "dian_hash_sha384_value".to_string(),
        payload: serde_json::json!({
            "items": []
        }),
        original_xml: Some("<xml>original</xml>".to_string()),
        signed_xml: None,
        pdf_url: None,
        status: DocumentStatus::Draft,
        created_at: Utc::now(),
    };

    let serialized = serde_json::to_string(&document).expect("Failed to serialize Document");
    let deserialized: Document =
        serde_json::from_str(&serialized).expect("Failed to deserialize Document");

    assert_eq!(document.id, deserialized.id);
    assert_eq!(document.document_number, deserialized.document_number);
    assert!(deserialized.signed_xml.is_none());
}

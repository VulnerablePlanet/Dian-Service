use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "certificate_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum CertificateStatus {
    Active,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "document_type", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DocumentType {
    Invoice,
    CreditNote,
    DebitNote,
    SupportDocument,
    Payroll,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "document_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus {
    Draft,
    Pending,
    SentDian,
    DianAccepted,
    DianRejected,
    ContingencyPending,
    ContingencySent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "event_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum EventStatus {
    Sent,
    Accepted,
    Rejected,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "subscription_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum WebhookSubscriptionStatus {
    Active,
    Inactive,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Tenant {
    pub id: Uuid,
    pub tax_id: String,
    pub registration_name: String,
    pub tax_regime: String,
    pub address_info: serde_json::Value,
    pub reception_email: String,
    pub api_keys: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Certificate {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub vault_reference_path: String,
    pub thumbprint: String,
    pub expiration_date: DateTime<Utc>,
    pub status: CertificateStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct NumberingRange {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub document_type: DocumentType,
    pub prefix: String,
    pub from_number: i32,
    pub to_number: i32,
    pub technical_key: String,
    pub valid_date_from: DateTime<Utc>,
    pub valid_date_to: DateTime<Utc>,
    pub current_counter: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Document {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub document_type: DocumentType,
    pub prefix: String,
    pub document_number: i32,
    pub cufe_cude: String,
    pub payload: serde_json::Value,
    pub original_xml: Option<String>,
    pub signed_xml: Option<String>,
    pub pdf_url: Option<String>,
    pub status: DocumentStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DianEvent {
    pub id: Uuid,
    pub document_id: Uuid,
    pub event_status: EventStatus,
    pub dian_response_code: Option<String>,
    pub dian_response_message: Option<String>,
    pub soap_trace_id: Option<String>,
    pub xml_response: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Webhook {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub target_url: String,
    pub webhook_secret: String,
    pub event_type: String,
    pub subscription_status: WebhookSubscriptionStatus,
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

/// Error al convertir un texto en un enum del dominio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownVariant {
    pub type_name: &'static str,
    pub value: String,
}

impl fmt::Display for UnknownVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown {} '{}'", self.type_name, self.value)
    }
}

impl std::error::Error for UnknownVariant {}

/// Define un enum con su representación textual. El texto coincide con la etiqueta del
/// tipo enum de PostgreSQL y con la serialización JSON, de modo que los adaptadores pueden
/// persistirlo sin que el dominio dependa de la base de datos.
macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $text)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(&self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }
        }

        impl FromStr for $name {
            type Err = UnknownVariant;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($text => Ok($name::$variant),)+
                    _ => Err(UnknownVariant { type_name: stringify!($name), value: value.to_string() }),
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

text_enum!(CertificateStatus {
    Active => "active",
    Expired => "expired",
    Revoked => "revoked",
});

text_enum!(DocumentType {
    Invoice => "invoice",
    CreditNote => "credit_note",
    DebitNote => "debit_note",
    SupportDocument => "support_document",
    Payroll => "payroll",
});

text_enum!(DocumentStatus {
    Draft => "draft",
    Pending => "pending",
    SentDian => "sent_dian",
    DianAccepted => "dian_accepted",
    DianRejected => "dian_rejected",
    ContingencyPending => "contingency_pending",
    ContingencySent => "contingency_sent",
    /// Fallo local antes de transmitir (firma, generación XML, tipo no soportado).
    /// El motivo queda registrado en `dian_events`.
    ProcessingFailed => "processing_failed",
});

text_enum!(EventStatus {
    Sent => "sent",
    Accepted => "accepted",
    Rejected => "rejected",
    Error => "error",
});

text_enum!(WebhookSubscriptionStatus {
    Active => "active",
    Inactive => "inactive",
});

text_enum!(
    /// Si el documento lo emitió el tenant o lo recibió de un proveedor.
    Direction {
        Outbound => "outbound",
        Inbound => "inbound",
    }
);

text_enum!(
    /// Tipo de trabajo en la cola persistente.
    JobKind {
        /// Generar, firmar y transmitir el documento.
        Transmit => "transmit",
        /// Consultar el estado de un envío asíncrono (`GetStatusZip`).
        PollStatus => "poll_status",
    }
);

text_enum!(JobStatus {
    Queued => "queued",
    Running => "running",
    Done => "done",
    /// Agotó sus intentos: queda como cola de mensajes muertos para revisión.
    Dead => "dead",
});

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Certificate {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub vault_reference_path: String,
    pub thumbprint: String,
    pub expiration_date: DateTime<Utc>,
    pub status: CertificateStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Último consecutivo asignado (0 = ninguno todavía).
    pub current_counter: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub direction: Direction,
    /// NIT del emisor para documentos recibidos; vacío para los emitidos por el tenant.
    pub issuer_nit: String,
    pub document_type: DocumentType,
    pub prefix: String,
    pub document_number: i32,
    /// Resolución de numeración con la que se emitió (solo documentos emitidos).
    pub numbering_range_id: Option<Uuid>,
    pub cufe_cude: String,
    pub payload: serde_json::Value,
    pub original_xml: Option<String>,
    pub signed_xml: Option<String>,
    pub pdf_url: Option<String>,
    pub status: DocumentStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl DianEvent {
    pub fn new(document_id: Uuid, event_status: EventStatus, code: &str, message: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            document_id,
            event_status,
            dian_response_code: Some(code.to_string()),
            dian_response_message: Some(message.into()),
            soap_trace_id: None,
            xml_response: None,
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Webhook {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub target_url: String,
    pub webhook_secret: String,
    pub event_type: String,
    pub subscription_status: WebhookSubscriptionStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_roundtrip_for_every_variant() {
        for status in DocumentStatus::ALL {
            assert_eq!(status.as_str().parse::<DocumentStatus>().unwrap(), *status);
            assert_eq!(serde_json::to_string(status).unwrap(), format!("\"{}\"", status.as_str()));
        }
        for t in DocumentType::ALL {
            assert_eq!(t.as_str().parse::<DocumentType>().unwrap(), *t);
        }
        assert!("nope".parse::<DocumentStatus>().is_err());
    }
}

//! Notificaciones a los sistemas de los tenants (webhooks): qué cambios de estado se
//! notifican, con qué datos y a qué destinos se permite enviarlos.

use crate::models::DocumentStatus;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use uuid::Uuid;

/// Suscripción a todos los eventos.
pub const EVENT_ALL: &str = "*";

/// Eventos que se pueden suscribir.
pub const EVENT_TYPES: &[&str] = &[
    "document.accepted",
    "document.rejected",
    "document.contingency",
    "document.failed",
];

/// Intentos de entrega antes de pasar la notificación a la cola de errores.
pub const MAX_DELIVERY_ATTEMPTS: i32 = 10;

/// Evento que dispara la llegada a `status`, si se notifica.
pub fn event_for(status: DocumentStatus) -> Option<&'static str> {
    match status {
        DocumentStatus::DianAccepted => Some("document.accepted"),
        DocumentStatus::DianRejected => Some("document.rejected"),
        DocumentStatus::ContingencyPending => Some("document.contingency"),
        DocumentStatus::ProcessingFailed => Some("document.failed"),
        _ => None,
    }
}

pub fn is_valid_subscription(event_type: &str) -> bool {
    event_type == EVENT_ALL || EVENT_TYPES.contains(&event_type)
}

/// Datos del documento que viajan en la notificación (nunca el XML ni secretos).
pub struct NotifiedDocument<'a> {
    pub document_id: Uuid,
    pub document_type: &'a str,
    pub prefix: &'a str,
    pub number: i32,
    pub status: DocumentStatus,
    pub cufe: Option<&'a str>,
}

pub fn notification_data(document: &NotifiedDocument) -> Value {
    json!({
        "document_id": document.document_id,
        "document_type": document.document_type,
        "prefix": document.prefix,
        "number": document.number,
        "status": document.status.as_str(),
        "cufe": document.cufe.filter(|c| !c.is_empty()),
    })
}

/// Dirección enrutable en Internet: excluye loopback, redes privadas, enlace local (incluida
/// la de metadatos de la nube, 169.254.169.254), CGNAT, multicast, documentación y
/// reservadas. Los webhooks solo se entregan a direcciones públicas (protección SSRF).
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // CGNAT 100.64.0.0/10
        || (a == 192 && b == 0 && c == 0) // 192.0.0.0/24 (IETF)
        || (a == 198 && (b == 18 || b == 19)) // pruebas de rendimiento
        || a >= 240) // reservadas
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00 // ULA fc00::/7
        || (first & 0xffc0) == 0xfe80 // enlace local fe80::/10
        || first == 0x2001 && ip.segments()[1] == 0x0db8 // documentación
        || first == 0x0064 && ip.segments()[1] == 0xff9b) // NAT64 (traduce a IPv4 arbitrarias)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_final_states_are_notified() {
        assert_eq!(event_for(DocumentStatus::DianAccepted), Some("document.accepted"));
        assert_eq!(event_for(DocumentStatus::ContingencyPending), Some("document.contingency"));
        assert_eq!(event_for(DocumentStatus::SentDian), None);
        assert_eq!(event_for(DocumentStatus::Pending), None);
        for status in DocumentStatus::ALL {
            if let Some(event) = event_for(*status) {
                assert!(is_valid_subscription(event));
            }
        }
        assert!(is_valid_subscription("*"));
        assert!(!is_valid_subscription("document.created"));
    }

    #[test]
    fn private_and_special_addresses_are_not_public() {
        for ip in [
            "127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1",
            "0.0.0.0", "255.255.255.255", "224.0.0.1", "192.0.2.10", "::1", "fd00::1", "fe80::1",
            "::ffff:10.0.0.1", "::ffff:127.0.0.1", "64:ff9b::a00:1", "2001:db8::1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{} must not be public", ip);
        }
        for ip in ["8.8.8.8", "200.69.105.1", "2800:3f0:4005::200e", "::ffff:8.8.4.4"] {
            assert!(is_public_ip(ip.parse().unwrap()), "{} is public", ip);
        }
    }
}

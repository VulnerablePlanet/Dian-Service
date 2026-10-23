//! Caso de uso: registro de facturas recibidas de proveedores (AttachedDocument).

use crate::xml::{split_document_id, tag_content, tag_in_section};
use core_domain::models::{DocumentStatus, DocumentType, Tenant};
use core_domain::ports::{DocumentRepository, DomainError, NewInboundDocument};
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct InboundRegistered {
    pub document_id: Uuid,
    pub parent_document_id: String,
}

pub struct InboundService {
    documents: Arc<dyn DocumentRepository>,
}

fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::Validation(message.into())
}

impl InboundService {
    pub fn new(documents: Arc<dyn DocumentRepository>) -> Self {
        Self { documents }
    }

    /// Registra el AttachedDocument como documento recibido **no verificado**: la firma solo
    /// se comprueba por presencia, por lo que no admite eventos RADIAN.
    pub async fn register_unverified(&self, tenant: &Tenant, xml: String) -> Result<InboundRegistered, DomainError> {
        if !xml.contains("<ds:Signature") && !xml.contains("<Signature") {
            return Err(invalid("El AttachedDocument XML no esta firmado digitalmente"));
        }
        if tag_content(&xml, "ResponseCode").as_deref() != Some("02") {
            return Err(invalid(
                "El AttachedDocument no contiene el codigo de aceptacion reglamentario de la DIAN ('02')",
            ));
        }

        let party = |section: &str, tag: &str| {
            tag_in_section(&xml, &format!("<cac:{}>", section), &format!("</cac:{}>", section), tag)
        };
        let sender_nit = party("SenderParty", "CompanyID")
            .ok_or_else(|| invalid("No se encontro el NIT del emisor en el AttachedDocument"))?;
        let sender_name = party("SenderParty", "RegistrationName")
            .or_else(|| party("SenderParty", "Name"))
            .unwrap_or_else(|| "Proveedor Inbound".to_string());
        let receiver_nit = party("ReceiverParty", "CompanyID")
            .ok_or_else(|| invalid("No se encontro el NIT del receptor en el AttachedDocument"))?;
        let receiver_name = party("ReceiverParty", "RegistrationName")
            .or_else(|| party("ReceiverParty", "Name"))
            .unwrap_or_else(|| "Tenant Receptor".to_string());
        let parent_document_id = tag_content(&xml, "ParentDocumentID")
            .ok_or_else(|| invalid("No se encontro el ParentDocumentID (Numero de factura)"))?;
        let cufe = tag_content(&xml, "UUID")
            .ok_or_else(|| invalid("No se encontro el CUFE/UUID de la factura original"))?;

        if receiver_nit != tenant.tax_id {
            return Err(DomainError::Forbidden(format!(
                "El NIT receptor del documento ({}) no coincide con el del tenant autenticado",
                receiver_nit
            )));
        }
        let (prefix, number) = split_document_id(&parent_document_id).ok_or_else(|| {
            invalid(format!(
                "El ParentDocumentID '{}' no tiene un consecutivo numerico valido",
                parent_document_id
            ))
        })?;

        let id = Uuid::new_v4();
        let inserted = self
            .documents
            .insert_inbound(NewInboundDocument {
                id,
                tenant_id: tenant.id,
                issuer_nit: sender_nit.clone(),
                document_type: DocumentType::Invoice,
                prefix,
                document_number: number,
                cufe: cufe.clone(),
                payload: json!({
                    "inbound": true,
                    "signature_verified": false,
                    "parent_document_id": parent_document_id,
                    "cufe": cufe,
                    "sender_nit": sender_nit,
                    "sender_name": sender_name,
                    "receiver_nit": receiver_nit,
                    "receiver_name": receiver_name
                }),
                signed_xml: xml,
                status: DocumentStatus::DianAccepted,
            })
            .await?;
        if !inserted {
            return Err(DomainError::Conflict(format!(
                "La factura {} del emisor {} ya estaba registrada",
                parent_document_id, sender_nit
            )));
        }
        Ok(InboundRegistered {
            document_id: id,
            parent_document_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    fn attached_document(sender: &str, receiver: &str, parent_id: &str) -> String {
        format!(
            "<AttachedDocument><ds:Signature/><cac:SenderParty><cbc:RegistrationName>Prov</cbc:RegistrationName>\
             <cbc:CompanyID>{sender}</cbc:CompanyID></cac:SenderParty><cac:ReceiverParty>\
             <cbc:CompanyID>{receiver}</cbc:CompanyID></cac:ReceiverParty><cbc:ParentDocumentID>{parent_id}</cbc:ParentDocumentID>\
             <cbc:UUID>cufe-1</cbc:UUID><cbc:ResponseCode>02</cbc:ResponseCode></AttachedDocument>"
        )
    }

    #[tokio::test]
    async fn registers_unverified_and_rejects_duplicates() {
        let (store, tenant) = store_with_tenant();
        let service = InboundService::new(store.clone());
        let xml = attached_document("800111222", &tenant.tax_id, "FEV-7");

        let registered = service.register_unverified(&tenant, xml.clone()).await.unwrap();
        let doc = store.document(registered.document_id).unwrap();
        assert_eq!(doc.issuer_nit, "800111222");
        assert_eq!(doc.payload["signature_verified"], false);

        assert!(matches!(service.register_unverified(&tenant, xml).await, Err(DomainError::Conflict(_))));
        let other_supplier = attached_document("811222333", &tenant.tax_id, "FEV-7");
        assert!(service.register_unverified(&tenant, other_supplier).await.is_ok());
    }

    #[tokio::test]
    async fn rejects_documents_for_another_receiver_or_without_number() {
        let (store, tenant) = store_with_tenant();
        let service = InboundService::new(store);
        assert!(matches!(
            service.register_unverified(&tenant, attached_document("8001", "999999999", "FEV-7")).await,
            Err(DomainError::Forbidden(_))
        ));
        assert!(matches!(
            service.register_unverified(&tenant, attached_document("8001", &tenant.tax_id, "FEV-ABC")).await,
            Err(DomainError::Validation(_))
        ));
    }
}

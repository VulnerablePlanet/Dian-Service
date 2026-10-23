//! Extracción mínima de valores de un XML UBL por nombre de etiqueta.
//!
//! No es un parser XML: solo localiza el contenido de texto de la primera etiqueta con ese
//! nombre local (ignorando el prefijo). No se usa para nada que dependa de la estructura de
//! firma; la verificación criptográfica de documentos recibidos sigue pendiente.

/// Contenido de la primera etiqueta con nombre local `tag_name`.
pub fn tag_content(xml: &str, tag_name: &str) -> Option<String> {
    let mut cursor = 0;
    while let Some(pos) = xml[cursor..].find('<') {
        let tag_start = cursor + pos + 1;
        let tag_rest = &xml[tag_start..];
        let name_end = tag_rest.find(|c: char| c == ' ' || c == '>' || c == '/' || c.is_whitespace())?;
        let full_tag = &tag_rest[..name_end];
        let local_name = full_tag.rsplit(':').next().unwrap_or(full_tag);

        if local_name == tag_name {
            let content_start = tag_start + tag_rest.find('>')? + 1;
            let rest = &xml[content_start..];
            let end = rest.find("</")?;
            return Some(rest[..end].trim().to_string());
        }
        cursor = tag_start;
    }
    None
}

/// Contenido de `tag_name` dentro de la primera sección delimitada por `section_start`/`section_end`.
pub fn tag_in_section(xml: &str, section_start: &str, section_end: &str, tag_name: &str) -> Option<String> {
    let start = xml.find(section_start)?;
    let end = xml[start..].find(section_end)? + start;
    tag_content(&xml[start..end], tag_name)
}

/// Separa un identificador como `FEV-1002` o `SETP990001` en prefijo y consecutivo.
pub fn split_document_id(document_id: &str) -> Option<(String, i32)> {
    let (prefix, digits) = match document_id.rfind('-') {
        Some(pos) => (&document_id[..pos], &document_id[pos + 1..]),
        None => {
            let idx = document_id.find(|c: char| c.is_ascii_digit())?;
            (&document_id[..idx], &document_id[idx..])
        }
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse::<i32>().ok().map(|n| (prefix.to_string(), n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tags_regardless_of_prefix_and_attributes() {
        let xml = r#"<a:Root><b:ResponseCode listID="x">02</b:ResponseCode><UUID>abc</UUID></a:Root>"#;
        assert_eq!(tag_content(xml, "ResponseCode").as_deref(), Some("02"));
        assert_eq!(tag_content(xml, "UUID").as_deref(), Some("abc"));
        assert_eq!(tag_content(xml, "Missing"), None);
    }

    #[test]
    fn section_lookup_is_scoped() {
        let xml = "<cac:SenderParty><cbc:CompanyID>1</cbc:CompanyID></cac:SenderParty>\
                   <cac:ReceiverParty><cbc:CompanyID>2</cbc:CompanyID></cac:ReceiverParty>";
        assert_eq!(tag_in_section(xml, "<cac:ReceiverParty>", "</cac:ReceiverParty>", "CompanyID").as_deref(), Some("2"));
    }

    #[test]
    fn splits_document_ids() {
        assert_eq!(split_document_id("FEV-1002"), Some(("FEV".to_string(), 1002)));
        assert_eq!(split_document_id("SETP990001"), Some(("SETP".to_string(), 990001)));
        assert_eq!(split_document_id("1234"), Some(("".to_string(), 1234)));
        assert_eq!(split_document_id("FEV-ABC"), None);
        assert_eq!(split_document_id("FEV"), None);
        assert_eq!(split_document_id("FE12A"), None);
    }
}

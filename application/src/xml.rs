//! Extracción de valores de documentos UBL.
//!
//! - [`XmlDoc`]: lectura con un parser XML real (`quick-xml`) por ruta de nombres locales,
//!   con el texto desescapado. Es la que se usa sobre documentos verificados.
//! - [`tag_content`] / [`tag_in_section`]: búsqueda textual mínima de la primera etiqueta
//!   con un nombre local; solo para el registro de documentos no verificados.

use quick_xml::events::Event;
use quick_xml::Reader;

/// Documento XML bien formado, sin DTD.
pub struct XmlDoc<'a> {
    xml: &'a str,
    root: String,
}

fn resolve_reference(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => match name.strip_prefix("#x") {
            Some(hex) => u32::from_str_radix(hex, 16).ok().and_then(char::from_u32),
            None => name.strip_prefix('#').and_then(|d| d.parse().ok()).and_then(char::from_u32),
        },
    }
}

fn local_name(qname: &str) -> String {
    qname.rsplit(':').next().unwrap_or_default().to_string()
}

impl<'a> XmlDoc<'a> {
    /// Comprueba que el documento sea XML bien formado (sin DTD) y anota su elemento raíz.
    pub fn parse(xml: &'a str) -> Result<Self, String> {
        let mut reader = Reader::from_str(xml);
        let mut root = None;
        let mut depth = 0usize;
        loop {
            match reader.read_event().map_err(|e| format!("XML invalido: {}", e))? {
                Event::Start(e) => {
                    if depth == 0 && root.replace(local_name(e.name().as_ref())).is_some() {
                        return Err("XML invalido: mas de un elemento raiz".to_string());
                    }
                    depth += 1;
                }
                Event::Empty(e) if depth == 0 => {
                    if root.replace(local_name(e.name().as_ref())).is_some() {
                        return Err("XML invalido: mas de un elemento raiz".to_string());
                    }
                }
                Event::End(_) => depth -= 1,
                Event::DocType(_) => return Err("XML invalido: no se admiten DTD".to_string()),
                Event::GeneralRef(r) => {
                    let name: &str = r.as_ref();
                    if resolve_reference(name).is_none() {
                        return Err(format!("XML invalido: entidad no admitida &{};", name));
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        match (root, depth) {
            (Some(root), 0) => Ok(Self { xml, root }),
            _ => Err("XML invalido: documento incompleto".to_string()),
        }
    }

    /// Nombre local del elemento raíz.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Texto (desescapado y sin espacios en los extremos) del primer elemento cuya ruta de
    /// nombres locales desde la raíz es `path` (la raíz incluida).
    pub fn text(&self, path: &[&str]) -> Option<String> {
        let mut reader = Reader::from_str(self.xml);
        let mut stack: Vec<String> = Vec::new();
        let mut capture: Option<(usize, String)> = None;
        loop {
            match reader.read_event().ok()? {
                Event::Start(e) => {
                    stack.push(local_name(e.name().as_ref()));
                    if capture.is_none() && stack == path {
                        capture = Some((stack.len(), String::new()));
                    }
                }
                Event::Empty(e) => {
                    stack.push(local_name(e.name().as_ref()));
                    if capture.is_none() && stack == path {
                        return Some(String::new());
                    }
                    stack.pop();
                }
                Event::End(_) => {
                    if let Some((depth, text)) = &capture {
                        if *depth == stack.len() {
                            return Some(text.trim().to_string());
                        }
                    }
                    stack.pop();
                }
                Event::Text(t) => {
                    if let Some((_, text)) = capture.as_mut() {
                        let raw: &str = t.as_ref();
                        text.push_str(raw);
                    }
                }
                Event::CData(t) => {
                    if let Some((_, text)) = capture.as_mut() {
                        let raw: &str = t.as_ref();
                        text.push_str(raw);
                    }
                }
                Event::GeneralRef(r) => {
                    if let Some((_, text)) = capture.as_mut() {
                        text.push(resolve_reference(r.as_ref())?);
                    }
                }
                Event::Eof => return None,
                _ => {}
            }
        }
    }
}

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
    fn reads_unescaped_text_by_path() {
        let xml = r#"<a:Root xmlns:a="urn:a" xmlns:b="urn:b"><b:X><b:Y>1</b:Y></b:X><b:Y>top</b:Y><b:X><b:Z>&lt;Inner a=&quot;1&quot;&gt;&#233;&lt;&#x2f;Inner&gt;<![CDATA[<raw/>]]></b:Z></b:X></a:Root>"#;
        let doc = XmlDoc::parse(xml).unwrap();
        assert_eq!(doc.root(), "Root");
        assert_eq!(doc.text(&["Root", "Y"]).as_deref(), Some("top"), "path is exact, not a descendant search");
        assert_eq!(doc.text(&["Root", "X", "Y"]).as_deref(), Some("1"));
        assert_eq!(doc.text(&["Root", "X", "Z"]).as_deref(), Some(r#"<Inner a="1">é</Inner><raw/>"#));
        assert_eq!(doc.text(&["Root", "Missing"]), None);
    }

    #[test]
    fn rejects_malformed_documents_and_dtds() {
        assert!(XmlDoc::parse("<a><b></a>").is_err());
        assert!(XmlDoc::parse("<a/><b/>").is_err());
        assert!(XmlDoc::parse(r#"<!DOCTYPE a [<!ENTITY x "y">]><a>&x;</a>"#).is_err());
        assert!(XmlDoc::parse("<a>&nbsp;</a>").is_err());
    }

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

//! Utilidades XML para firmar nodos "en contexto".
//!
//! La canonicalización inclusiva (C14N 1.0) de un subárbol incluye todas las declaraciones
//! de namespace heredadas de sus ancestros. Un verificador canonicaliza los nodos firmados
//! dentro del documento final, así que el firmador debe reproducir ese contexto: aquí se
//! calcula el ámbito de namespaces en un punto del documento y se inyecta en el fragmento
//! antes de canonicalizarlo de forma aislada.

use crate::error::CryptoError;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::BTreeMap;

/// Declaraciones de namespace (`prefijo -> URI`; el prefijo vacío es el namespace por
/// defecto) vigentes en la posición donde aparece `marker`.
pub fn namespaces_in_scope_at(xml: &str, marker: &str) -> Result<BTreeMap<String, String>, CryptoError> {
    let position = xml
        .find(marker)
        .ok_or_else(|| CryptoError::Xml(format!("marker '{}' not found", marker)))?;
    let mut reader = Reader::from_str(&xml[..position]);
    let mut stack: Vec<Vec<(String, String)>> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let mut declarations = Vec::new();
                for attribute in start.attributes() {
                    let attribute = attribute.map_err(|e| CryptoError::Xml(e.to_string()))?;
                    let key: &str = attribute.key.as_ref();
                    let prefix = match key {
                        "xmlns" => String::new(),
                        other => match other.strip_prefix("xmlns:") {
                            Some(prefix) => prefix.to_string(),
                            None => continue,
                        },
                    };
                    let value = attribute
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| CryptoError::Xml(e.to_string()))?
                        .into_owned();
                    declarations.push((prefix, value));
                }
                stack.push(declarations);
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(CryptoError::Xml(format!("Invalid XML before signature: {}", e))),
        }
    }

    let mut scope = BTreeMap::new();
    for declarations in stack {
        for (prefix, uri) in declarations {
            scope.insert(prefix, uri);
        }
    }
    Ok(scope)
}

/// Inserta en el elemento raíz de `fragment` las declaraciones de `namespaces` que el
/// propio elemento no declare ya.
pub fn with_namespaces(fragment: &str, namespaces: &BTreeMap<String, String>) -> String {
    let name_end = fragment[1..]
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .map(|i| i + 1)
        .unwrap_or(fragment.len());
    let tag_end = fragment.find('>').unwrap_or(fragment.len());
    let start_tag = &fragment[..tag_end];

    let mut declarations = String::new();
    for (prefix, uri) in namespaces {
        let attribute = if prefix.is_empty() {
            "xmlns".to_string()
        } else {
            format!("xmlns:{}", prefix)
        };
        if start_tag.contains(&format!("{}=", attribute)) {
            continue;
        }
        declarations.push_str(&format!(" {}=\"{}\"", attribute, escape_attribute(uri)));
    }
    format!("{}{}{}", &fragment[..name_end], declarations, &fragment[name_end..])
}

/// Escapa texto para el contenido de un elemento.
pub fn escape_text(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Escapa texto para el valor de un atributo entre comillas dobles.
pub fn escape_attribute(value: &str) -> String {
    escape_text(value).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_merges_ancestors_and_ignores_closed_elements() {
        let xml = r#"<a xmlns="urn:a" xmlns:x="urn:x1"><closed xmlns:y="urn:y"/><b xmlns:x="urn:x2"><c>MARK</c></b></a>"#;
        let scope = namespaces_in_scope_at(xml, "MARK").unwrap();
        assert_eq!(scope.get(""), Some(&"urn:a".to_string()));
        assert_eq!(scope.get("x"), Some(&"urn:x2".to_string()), "inner declaration wins");
        assert!(!scope.contains_key("y"), "sibling scope is closed");
    }

    #[test]
    fn injects_only_missing_declarations() {
        let mut ns = BTreeMap::new();
        ns.insert("ds".to_string(), "urn:ds".to_string());
        ns.insert(String::new(), "urn:default".to_string());
        assert_eq!(
            with_namespaces(r#"<ds:KeyInfo Id="k"><x/></ds:KeyInfo>"#, &ns),
            r#"<ds:KeyInfo xmlns="urn:default" xmlns:ds="urn:ds" Id="k"><x/></ds:KeyInfo>"#
        );
        assert_eq!(
            with_namespaces(r#"<ds:A xmlns:ds="urn:other"/>"#, &ns),
            r#"<ds:A xmlns="urn:default" xmlns:ds="urn:other"/>"#
        );
    }
}

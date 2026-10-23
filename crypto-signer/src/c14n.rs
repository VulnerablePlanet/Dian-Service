//! Canonical XML 1.0 inclusivo, sin comentarios (W3C REC-xml-c14n-20010315).
//!
//! Implementación propia sobre `quick-xml`. Reemplaza al crate `xml-canonicalization`, que
//! ordenaba mal la declaración del namespace por defecto (la especificación la exige
//! primero) y por eso producía firmas que ningún verificador aceptaba.
//!
//! Alcance: documentos y fragmentos completos (no "document subsets"); los namespaces que un
//! fragmento hereda se inyectan antes de llamar aquí (ver `xmlutil::with_namespaces`). Se
//! rechazan DTDs y entidades no predefinidas. La conformidad se comprueba en las pruebas
//! comparando contra `xmllint --c14n` (libxml2).

use crate::error::CryptoError;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use std::collections::BTreeMap;

const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

fn err(message: impl Into<String>) -> CryptoError {
    CryptoError::C14n(message.into())
}

/// Normaliza fines de línea como lo exige XML (`\r\n` y `\r` → `\n`).
fn normalize_eol(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn escape_text(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#xD;"),
            _ => out.push(c),
        }
    }
}

fn escape_attribute(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            _ => out.push(c),
        }
    }
}

/// Resuelve una referencia (`amp`, `#10`, `#x41`...). Las entidades no predefinidas se
/// rechazan: requerirían un DTD.
fn resolve_reference(name: &str) -> Result<char, CryptoError> {
    let resolved = match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            if let Some(hex) = name.strip_prefix("#x") {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = name.strip_prefix('#') {
                dec.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                None
            }
        }
    };
    resolved.ok_or_else(|| err(format!("Unsupported entity reference &{};", name)))
}

type Scope = BTreeMap<String, String>;

struct Canonicalizer {
    out: String,
    scopes: Vec<Scope>,
    depth: usize,
    seen_root: bool,
}

impl Canonicalizer {
    fn start_element(&mut self, start: &BytesStart) -> Result<String, CryptoError> {
        let name = std::str::from_utf8(start.name().as_ref().as_bytes())
            .map_err(|e| err(e.to_string()))?
            .to_string();
        let parent = self.scopes.last().cloned().unwrap_or_default();
        let mut scope = parent.clone();
        let mut attributes: Vec<(String, String, String, String)> = Vec::new(); // (ns, local, qname, value)

        let mut raw_attributes = Vec::new();
        for attribute in start.attributes() {
            let attribute = attribute.map_err(|e| err(e.to_string()))?;
            let key: &str = attribute.key.as_ref();
            let value = attribute
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|e| err(e.to_string()))?
                .into_owned();
            if key == "xmlns" {
                scope.insert(String::new(), value);
            } else if let Some(prefix) = key.strip_prefix("xmlns:") {
                if value.is_empty() {
                    return Err(err("Namespace undeclaration (XML 1.1) is not supported"));
                }
                scope.insert(prefix.to_string(), value);
            } else {
                raw_attributes.push((key.to_string(), value));
            }
        }

        for (qname, value) in raw_attributes {
            let (namespace, local) = match qname.split_once(':') {
                Some(("xml", local)) => (XML_NAMESPACE.to_string(), local.to_string()),
                Some((prefix, local)) => (
                    scope
                        .get(prefix)
                        .cloned()
                        .ok_or_else(|| err(format!("Undeclared prefix '{}'", prefix)))?,
                    local.to_string(),
                ),
                None => (String::new(), qname.clone()),
            };
            attributes.push((namespace, local, qname, value));
        }
        attributes.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));

        // Declaraciones que cambian respecto al ancestro (un default ausente equivale a "").
        // BTreeMap ordena por prefijo y el prefijo vacío (default) queda primero.
        let mut rendered = String::new();
        for (prefix, uri) in &scope {
            let inherited = parent.get(prefix).map(String::as_str).unwrap_or("");
            if uri == inherited {
                continue;
            }
            if prefix.is_empty() {
                rendered.push_str(" xmlns=\"");
            } else {
                rendered.push_str(" xmlns:");
                rendered.push_str(prefix);
                rendered.push_str("=\"");
            }
            escape_attribute(uri, &mut rendered);
            rendered.push('"');
        }

        self.out.push('<');
        self.out.push_str(&name);
        self.out.push_str(&rendered);
        for (_, _, qname, value) in attributes {
            self.out.push(' ');
            self.out.push_str(&qname);
            self.out.push_str("=\"");
            escape_attribute(&value, &mut self.out);
            self.out.push('"');
        }
        self.out.push('>');
        self.scopes.push(scope);
        self.depth += 1;
        self.seen_root = true;
        Ok(name)
    }

    fn end_element(&mut self, name: &str) {
        self.out.push_str("</");
        self.out.push_str(name);
        self.out.push('>');
        self.scopes.pop();
        self.depth -= 1;
    }
}

/// Canonicaliza un documento (o fragmento completo) con C14N 1.0 inclusivo sin comentarios.
pub fn canonicalize_xml(xml_content: &str) -> Result<String, CryptoError> {
    let mut reader = Reader::from_str(xml_content);
    reader.config_mut().trim_text(false);
    let mut c = Canonicalizer {
        out: String::with_capacity(xml_content.len()),
        scopes: Vec::new(),
        depth: 0,
        seen_root: false,
    };

    loop {
        let event = reader.read_event().map_err(|e| err(format!("Invalid XML: {}", e)))?;
        match event {
            Event::Start(start) => {
                c.start_element(&start)?;
            }
            Event::Empty(start) => {
                let name = c.start_element(&start)?;
                c.end_element(&name);
            }
            Event::End(end) => {
                let name = std::str::from_utf8(end.name().as_ref().as_bytes())
                    .map_err(|e| err(e.to_string()))?
                    .to_string();
                c.end_element(&name);
            }
            Event::Text(text) if c.depth > 0 => {
                let raw = { let s: &str = text.as_ref(); s };
                escape_text(&normalize_eol(raw), &mut c.out);
            }
            Event::CData(data) if c.depth > 0 => {
                let raw = { let s: &str = data.as_ref(); s };
                escape_text(&normalize_eol(raw), &mut c.out);
            }
            Event::GeneralRef(reference) if c.depth > 0 => {
                let name = { let s: &str = reference.as_ref(); s };
                let mut buffer = [0u8; 4];
                escape_text(resolve_reference(name)?.encode_utf8(&mut buffer), &mut c.out);
            }
            Event::PI(pi) => {
                let raw = { let s: &str = pi.as_ref(); s };
                let (target, data) = match raw.find(|ch: char| ch.is_whitespace()) {
                    Some(i) => (&raw[..i], raw[i..].trim_start()),
                    None => (raw, ""),
                };
                if c.depth == 0 && c.seen_root {
                    c.out.push('\n');
                }
                c.out.push_str("<?");
                c.out.push_str(target);
                if !data.is_empty() {
                    c.out.push(' ');
                    c.out.push_str(&normalize_eol(data));
                }
                c.out.push_str("?>");
                if c.depth == 0 && !c.seen_root {
                    c.out.push('\n');
                }
            }
            Event::DocType(_) => return Err(err("DTDs are not allowed")),
            Event::Eof => break,
            // Declaración XML, comentarios y texto fuera del elemento raíz no se emiten.
            _ => {}
        }
    }
    if c.depth != 0 {
        return Err(err("Unclosed elements"));
    }
    Ok(c.out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn test_canonicalize_simple_xml() {
        let input = "<doc>  <!-- comment -->  <el attr=\"val\">text</el></doc>";
        let canonicalized = canonicalize_xml(input).unwrap();
        assert!(!canonicalized.contains("comment"));
        assert_eq!(canonicalized, "<doc>    <el attr=\"val\">text</el></doc>");
    }

    #[test]
    fn default_namespace_is_rendered_first() {
        assert_eq!(
            canonicalize_xml(r#"<a xmlns:z="urn:z" xmlns="urn:d" xmlns:b="urn:b"/>"#).unwrap(),
            r#"<a xmlns="urn:d" xmlns:b="urn:b" xmlns:z="urn:z"></a>"#
        );
    }

    #[test]
    fn rejects_dtd_and_unknown_entities() {
        assert!(canonicalize_xml(r#"<!DOCTYPE a [<!ENTITY x "y">]><a>&x;</a>"#).is_err());
        assert!(canonicalize_xml("<a>&nbsp;</a>").is_err());
    }

    /// Casos de la especificación y casos límite, comparados contra libxml2. `xmllint --c14n`
    /// es la variante con comentarios, así que el corpus no contiene comentarios (su
    /// eliminación se prueba en `test_canonicalize_simple_xml`).
    const CORPUS: &[&str] = &[
        r#"<?xml version="1.0" encoding="UTF-8"?><doc/>"#,
        "<?xml version=\"1.0\"?>\n<doc>\n  <e1   />\n  <e2   ></e2>\n  <e3   name = \"elem3\"   id=\"elem3\"   />\n</doc>\n",
        r#"<doc xmlns="http://www.ietf.org" xmlns:w3c="http://www.w3.org"><e1 xmlns="http://www.ietf.org"><e2 xmlns=""><e3 xmlns:w3c="http://www.w3.org"/></e2></e1></doc>"#,
        r#"<a xmlns:b="urn:b" xmlns:a="urn:a" b:attr="2" a:attr="1" attr="0" xml:lang="es"><c xmlns:b="urn:b2" b:x="y"/></a>"#,
        "<a attr=\"line1&#10;line2&#9;tab&#13;cr\" other=\"x\ny\tz\">t&#13;ext\r\nnext\rlast</a>",
        r#"<a>&lt;&gt;&amp;&quot;&apos; &#233; &#x1F600; <![CDATA[ <raw> & ]]></a>"#,
        r#"<a b="&quot;q&quot; &amp; &lt;"/>"#,
        "<ns0:root xmlns:ns0=\"urn:x\" xmlns=\"urn:y\"><child xmlns=\"urn:y\"><g xmlns=\"\"/></child></ns0:root>",
        "<?pi-before data ?>\n<a><?inner  some data?></a>\n<?pi-after?>",
        r#"<Invoice xmlns="urn:oasis:names:specification:ubl:schema:xsd:Invoice-2" xmlns:cac="urn:cac" xmlns:cbc="urn:cbc"><cbc:ID schemeID="31" schemeAgencyID="195">FE1</cbc:ID><cac:Party><cbc:Name>A &amp; B</cbc:Name></cac:Party></Invoice>"#,
    ];

    #[test]
    fn matches_libxml2_on_the_corpus() {
        if Command::new("xmllint").arg("--version").output().is_err() {
            eprintln!("xmllint not installed: skipping C14N differential test");
            return;
        }
        let dir = std::env::temp_dir().join(format!("c14n-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (i, input) in CORPUS.iter().enumerate() {
            let file = dir.join(format!("case{}.xml", i));
            std::fs::write(&file, input).unwrap();
            let output = Command::new("xmllint").arg("--c14n").arg(&file).output().unwrap();
            assert!(output.status.success(), "xmllint failed on case {}", i);
            let expected = String::from_utf8(output.stdout).unwrap();
            let actual = canonicalize_xml(input).unwrap();
            assert_eq!(actual, expected, "case {} differs from libxml2:\n{}", i, input);
        }
    }
}

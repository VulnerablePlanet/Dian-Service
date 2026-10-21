use crate::error::CryptoError;
use std::io::Cursor;
use xml_canonicalization::Canonicalizer;

/// Aplica Canonical XML (C14N inclusivo, sin comentarios) sobre el XML de entrada.
pub fn canonicalize_xml(xml_content: &str) -> Result<String, CryptoError> {
    let mut result = Vec::new();
    
    // El argumento de `canonicalize` es `retain_comments`: con `false` se descartan los
    // comentarios, de modo que el marcador de firma no entra en el digest.
    Canonicalizer::read_from_str(xml_content)
        .write_to_writer(Cursor::new(&mut result))
        .canonicalize(false)
        .map_err(|e| CryptoError::C14n(format!("Failed to canonicalize: {:?}", e)))?;

    String::from_utf8(result)
        .map_err(|e| CryptoError::C14n(format!("Utf8 conversion error after C14n: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonicalize_simple_xml() {
        let input = "<doc>  <!-- comment -->  <el attr=\"val\">text</el></doc>";
        let canonicalized = canonicalize_xml(input).unwrap();
        
        // C14N exclusive without comments should omit the comment and sort attributes / format tags
        assert!(!canonicalized.contains("comment"));
        assert_eq!(canonicalized, "<doc>    <el attr=\"val\">text</el></doc>");
    }
}

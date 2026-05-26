use crate::error::SoapError;
use std::io::{Cursor, Write};
use zip::write::FileOptions;
use zip::ZipWriter;

/// Comprime un XML firmado en formato ZIP en memoria.
/// Retorna la tupla `(bytes_del_zip, nombre_del_archivo_zip)`.
pub fn compress_xml_to_zip(
    xml_content: &str,
    nit_ofe: &str,
    prefix: &str,
    number: i32,
) -> Result<(Vec<u8>, String), SoapError> {
    // Formato de nombre estándar DIAN: z{nit_ofe}{prefix}{consecutivo}.zip
    let file_base = format!("z{}{}{:010}", nit_ofe, prefix, number);
    let xml_filename = format!("{}.xml", file_base);
    let zip_filename = format!("{}.zip", file_base);

    let mut buf = Vec::new();
    {
        let cursor = Cursor::new(&mut buf);
        let mut zip = ZipWriter::new(cursor);

        let options = FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        zip.start_file(&xml_filename, options)
            .map_err(|e| SoapError::Zip(format!("Failed to start ZIP file element: {}", e)))?;

        zip.write_all(xml_content.as_bytes())
            .map_err(|e| SoapError::Zip(format!("Failed to write XML into ZIP: {}", e)))?;

        zip.finish()
            .map_err(|e| SoapError::Zip(format!("Failed to finalize ZIP archive: {}", e)))?;
    }

    Ok((buf, zip_filename))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn test_compress_xml_success() {
        let xml = "<test>content</test>";
        let result = compress_xml_to_zip(xml, "900123456", "FE", 102);
        assert!(result.is_ok());

        let (zip_bytes, filename) = result.unwrap();
        assert!(filename.starts_with("z900123456FE0000000102"));
        assert!(filename.ends_with(".zip"));
        assert!(!zip_bytes.is_empty());

        // Validate that we can read it back
        let cursor = Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(cursor).unwrap();
        assert_eq!(archive.len(), 1);

        let mut file = archive.by_index(0).unwrap();
        assert!(file.name().ends_with(".xml"));

        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, xml);
    }
}

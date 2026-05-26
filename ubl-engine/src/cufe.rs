use sha2::{Digest, Sha384};

pub fn calculate_cufe_cude(
    num_fac: &str,
    fec_fac: &str,
    hor_fac: &str,
    val_fac: f64,
    cod_imp1: &str,
    val_imp1: f64,
    cod_imp2: &str,
    val_imp2: f64,
    cod_imp3: &str,
    val_imp3: f64,
    val_tol_fac: f64,
    nit_ofe: &str,
    num_adq: &str,
    key: &str,
    tipo_ambiente: &str,
) -> String {
    let val_fac_str = format_amount(val_fac);
    let val_imp1_str = format_amount(val_imp1);
    let val_imp2_str = format_amount(val_imp2);
    let val_imp3_str = format_amount(val_imp3);
    let val_tol_fac_str = format_amount(val_tol_fac);

    // Concatenate order:
    // NumFac + FecFac + HorFac + ValFac + CodImp1 + ValImp1 + CodImp2 + ValImp2 + CodImp3 + ValImp3 + ValTolFac + NitOFE + NumAdq + ClTec/PIN + TipoAmbiente
    let input = format!(
        "{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}",
        num_fac,
        fec_fac,
        hor_fac,
        val_fac_str,
        cod_imp1,
        val_imp1_str,
        cod_imp2,
        val_imp2_str,
        cod_imp3,
        val_imp3_str,
        val_tol_fac_str,
        nit_ofe,
        num_adq,
        key,
        tipo_ambiente
    );

    let mut hasher = Sha384::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();

    format!("{:x}", result)
}

fn format_amount(amount: f64) -> String {
    format!("{:.2}", amount)
}

pub fn calculate_cude_event(
    num_documento_event: &str,
    fec_event: &str,
    hor_event: &str,
    nit_emisor_event: &str,
    num_receptor_event: &str,
    software_pin: &str,
    tipo_ambiente: &str,
) -> String {
    // Concatena: NumDocumentoEvent + FecEvent + HorEvent + ValEvent (0.00) + CodImp (01) + ValImp (0.00) + ValTot (0.00) + NitEmisorEvent + NumReceptorEvent + PIN + TipoAmbiente
    let input = format!(
        "{}{}{}0.00010.000.00{}{}{}{}",
        num_documento_event,
        fec_event,
        hor_event,
        nit_emisor_event,
        num_receptor_event,
        software_pin,
        tipo_ambiente
    );

    let mut hasher = Sha384::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();

    format!("{:x}", result)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha384_cufe_calculation() {
        // Test values resembling a real DIAN scenario
        let cufe = calculate_cufe_cude(
            "FEV1002",
            "2026-05-23",
            "10:42:00-05:00",
            1000000.00,
            "01", // IVA
            190000.00,
            "04", // INC
            0.00,
            "03", // ICA
            0.00,
            1190000.00,
            "900123456",
            "900999888",
            "clave_tecnica_dian_test",
            "2", // Ambiente de Habilitación
        );

        assert_eq!(cufe.len(), 96); // SHA-384 hex size is 96 chars
        
        // Ensure it's lowercase hex
        assert!(cufe.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn test_sha384_cude_event_calculation() {
        let cude = calculate_cude_event(
            "EV1234",
            "2026-05-24",
            "12:00:00-05:00",
            "900123456",
            "900999888",
            "pin_dummy",
            "2",
        );
        assert_eq!(cude.len(), 96);
        assert!(cude.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }
}

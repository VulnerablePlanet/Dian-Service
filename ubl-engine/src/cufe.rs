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

    #[test]
    fn test_sha384_cune_calculation() {
        let cune = calculate_cune(
            "NOM123",
            "2026-05-26",
            "08:00:00",
            1500000.00,
            60000.00,
            1440000.00,
            "900123456",
            "10203040",
            "102",
            "pin_test",
            "2",
        );
        assert_eq!(cune.len(), 96);
        assert!(cune.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn test_sha384_cuds_calculation() {
        let cuds = calculate_cuds(
            "DS102",
            "2026-05-26",
            "09:30:00-05:00",
            100000.00,
            "01",
            19000.00,
            119000.00,
            "900999888",
            "900123456",
            "pin_test",
            "2",
        );
        assert_eq!(cuds.len(), 96);
        assert!(cuds.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }
}

pub fn calculate_cune(
    num_ne: &str,
    fec_ne: &str,
    hor_ne: &str,
    val_dev: f64,
    val_ded: f64,
    val_tol_ne: f64,
    nit_ne: &str,
    doc_emp: &str,
    tipo_xml: &str,
    software_pin: &str,
    tipo_ambiente: &str,
) -> String {
    let val_dev_str = format_amount(val_dev);
    let val_ded_str = format_amount(val_ded);
    let val_tol_ne_str = format_amount(val_tol_ne);

    // Concatena: NumNE + FecNE + HorNE + ValDev + ValDed + ValTolNE + NitNE + DocEmp + TipoXML + SoftwarePin + TipoAmbiente
    let input = format!(
        "{}{}{}{}{}{}{}{}{}{}{}",
        num_ne,
        fec_ne,
        hor_ne,
        val_dev_str,
        val_ded_str,
        val_tol_ne_str,
        nit_ne,
        doc_emp,
        tipo_xml,
        software_pin,
        tipo_ambiente
    );

    let mut hasher = Sha384::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();

    format!("{:x}", result)
}

pub fn calculate_cuds(
    num_ds: &str,
    fec_ds: &str,
    hor_ds: &str,
    val_ds: f64,
    cod_imp: &str,
    val_imp: f64,
    val_tot: f64,
    nit_ofe: &str,
    num_adq: &str,
    software_pin: &str,
    tipo_ambiente: &str,
) -> String {
    let val_ds_str = format_amount(val_ds);
    let val_imp_str = format_amount(val_imp);
    let val_tot_str = format_amount(val_tot);

    // Concatena: NumDS + FecDS + HorDS + ValDS + CodImp + ValImp + ValTot + NitOFE + NumAdq + SoftwarePin + TipoAmbiente
    let input = format!(
        "{}{}{}{}{}{}{}{}{}{}{}",
        num_ds,
        fec_ds,
        hor_ds,
        val_ds_str,
        cod_imp,
        val_imp_str,
        val_tot_str,
        nit_ofe,
        num_adq,
        software_pin,
        tipo_ambiente
    );

    let mut hasher = Sha384::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();

    format!("{:x}", result)
}

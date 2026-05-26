use crate::error::UblError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoicePayload {
    pub prefix: String,
    pub number: i32,
    pub issue_date: String,      // YYYY-MM-DD
    pub issue_time: String,      // HH:MM:SS-05:00
    pub company_nit: String,     // NIT emisor sin puntos ni DV (ej: 900123456)
    pub company_name: String,
    pub customer_id_type: String, // Tipo de identificación del adquirente (ej: 31 = NIT, 13 = Cédula)
    pub customer_id: String,     // Identificación del adquirente
    pub customer_name: String,
    pub customer_email: String,
    pub net_amount: f64,
    pub tax_amount: f64,
    pub total_amount: f64,
    pub environment: String,      // "1" = Producción, "2" = Habilitación/Pruebas
    pub technical_key: String,    // Clave técnica (para facturas) o Software PIN (para notas)
    pub document_type: String,    // "invoice", "credit_note", "debit_note"
    #[serde(default)]
    pub test_set_id: Option<String>,
}

impl InvoicePayload {
    pub fn validate(&self) -> Result<(), UblError> {
        // 1. Validar formato de fecha (YYYY-MM-DD)
        if self.issue_date.len() != 10
            || !self.issue_date.chars().nth(4).map_or(false, |c| c == '-')
            || !self.issue_date.chars().nth(7).map_or(false, |c| c == '-')
        {
            return Err(UblError::Validation(
                "La fecha debe tener el formato YYYY-MM-DD".to_string(),
            ));
        }

        // 2. Validar formato de hora con zona horaria (HH:MM:SS-05:00)
        // Ejemplo: 10:42:00-05:00
        if self.issue_time.len() != 14
            || !self.issue_time.chars().nth(2).map_or(false, |c| c == ':')
            || !self.issue_time.chars().nth(5).map_or(false, |c| c == ':')
            || !self.issue_time.chars().nth(8).map_or(false, |c| c == '-')
        {
            return Err(UblError::Validation(
                "La hora debe tener el formato HH:MM:SS-05:00".to_string(),
            ));
        }

        // 3. Validar NIT de emisor (numérico)
        if self.company_nit.is_empty() || !self.company_nit.chars().all(|c| c.is_ascii_digit()) {
            return Err(UblError::Validation(
                "El NIT de la empresa debe ser únicamente numérico (sin puntos ni dígito de verificación)".to_string(),
            ));
        }

        // 4. Validar adquirente
        if self.customer_id.is_empty() {
            return Err(UblError::Validation(
                "La identificación del adquiriente no puede estar vacía".to_string(),
            ));
        }

        // 5. Validar correo electrónico básico
        if !self.customer_email.contains('@') {
            return Err(UblError::Validation(
                "El correo del adquiriente debe ser válido".to_string(),
            ));
        }

        // 6. Validar montos positivos
        if self.net_amount < 0.0 || self.tax_amount < 0.0 || self.total_amount < 0.0 {
            return Err(UblError::Validation(
                "Los montos del documento no pueden ser negativos".to_string(),
            ));
        }

        // 7. Validar tipo de documento
        match self.document_type.as_str() {
            "invoice" | "credit_note" | "debit_note" => {}
            _ => {
                return Err(UblError::Validation(
                    "Tipo de documento inválido. Debe ser 'invoice', 'credit_note' o 'debit_note'".to_string(),
                ));
            }
        }

        // 8. Validar ambiente
        if self.environment != "1" && self.environment != "2" {
            return Err(UblError::Validation(
                "El ambiente debe ser '1' (producción) o '2' (habilitación)".to_string(),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_payload() -> InvoicePayload {
        InvoicePayload {
            prefix: "FE".to_string(),
            number: 1002,
            issue_date: "2026-05-23".to_string(),
            issue_time: "10:42:00-05:00".to_string(),
            company_nit: "900123456".to_string(),
            company_name: "Empresa Emisora".to_string(),
            customer_id_type: "31".to_string(),
            customer_id: "900999888".to_string(),
            customer_name: "Cliente Receptor".to_string(),
            customer_email: "cliente@receptor.com".to_string(),
            net_amount: 1000.0,
            tax_amount: 190.0,
            total_amount: 1190.0,
            environment: "2".to_string(),
            technical_key: "clave_tecnica".to_string(),
            document_type: "invoice".to_string(),
            test_set_id: None,
        }
    }

    #[test]
    fn test_valid_payload() {
        let payload = mock_payload();
        assert!(payload.validate().is_ok());
    }

    #[test]
    fn test_invalid_date() {
        let mut payload = mock_payload();
        payload.issue_date = "23-05-2026".to_string();
        assert!(payload.validate().is_err());
    }

    #[test]
    fn test_invalid_time() {
        let mut payload = mock_payload();
        payload.issue_time = "10:42:00".to_string();
        assert!(payload.validate().is_err());
    }

    #[test]
    fn test_invalid_nit() {
        let mut payload = mock_payload();
        payload.company_nit = "900.123.456-7".to_string();
        assert!(payload.validate().is_err());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayrollPayload {
    pub prefix: String,
    pub number: i32,
    pub issue_date: String,      // YYYY-MM-DD
    pub issue_time: String,      // HH:MM:SS
    pub employer_nit: String,    // NIT del empleador sin puntos ni DV
    pub employer_name: String,
    pub employee_id_type: String, // Tipo de identificacion (ej: 13 = Cedula)
    pub employee_id: String,     // Identificacion del empleado
    pub employee_name: String,
    pub devengado: f64,
    pub deducido: f64,
    pub total: f64,
    pub software_pin: String,
    pub environment: String,      // "1" = Produccion, "2" = Habilitacion
}

impl PayrollPayload {
    pub fn validate(&self) -> Result<(), UblError> {
        if self.issue_date.len() != 10
            || !self.issue_date.chars().nth(4).map_or(false, |c| c == '-')
            || !self.issue_date.chars().nth(7).map_or(false, |c| c == '-')
        {
            return Err(UblError::Validation("La fecha de nomina debe tener el formato YYYY-MM-DD".to_string()));
        }

        if self.issue_time.is_empty() {
            return Err(UblError::Validation("La hora de nomina no puede estar vacia".to_string()));
        }

        if self.employer_nit.is_empty() || !self.employer_nit.chars().all(|c| c.is_ascii_digit()) {
            return Err(UblError::Validation("El NIT del empleador debe ser numerico".to_string()));
        }

        if self.employee_id.is_empty() {
            return Err(UblError::Validation("La identificacion del empleado no puede estar vacia".to_string()));
        }

        if self.devengado < 0.0 || self.deducido < 0.0 || self.total < 0.0 {
            return Err(UblError::Validation("Los montos de nomina no pueden ser negativos".to_string()));
        }

        if self.environment != "1" && self.environment != "2" {
            return Err(UblError::Validation("El ambiente debe ser '1' (produccion) o '2' (habilitacion)".to_string()));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportDocumentPayload {
    pub prefix: String,
    pub number: i32,
    pub issue_date: String,      // YYYY-MM-DD
    pub issue_time: String,      // HH:MM:SS-05:00
    pub seller_nit: String,      // NIT/Cedula del vendedor no obligado
    pub seller_name: String,
    pub buyer_nit: String,       // NIT del comprador (nosotros) sin puntos ni DV
    pub buyer_name: String,
    pub net_amount: f64,
    pub tax_amount: f64,
    pub total_amount: f64,
    pub software_pin: String,
    pub environment: String,      // "1" = Produccion, "2" = Habilitacion
}

impl SupportDocumentPayload {
    pub fn validate(&self) -> Result<(), UblError> {
        if self.issue_date.len() != 10
            || !self.issue_date.chars().nth(4).map_or(false, |c| c == '-')
            || !self.issue_date.chars().nth(7).map_or(false, |c| c == '-')
        {
            return Err(UblError::Validation("La fecha del documento soporte debe tener el formato YYYY-MM-DD".to_string()));
        }

        if self.issue_time.len() != 14
            || !self.issue_time.chars().nth(2).map_or(false, |c| c == ':')
            || !self.issue_time.chars().nth(5).map_or(false, |c| c == ':')
            || !self.issue_time.chars().nth(8).map_or(false, |c| c == '-')
        {
            return Err(UblError::Validation("La hora del documento soporte debe tener el formato HH:MM:SS-05:00".to_string()));
        }

        if self.seller_nit.is_empty() || !self.seller_nit.chars().all(|c| c.is_ascii_digit()) {
            return Err(UblError::Validation("La identificacion del vendedor debe ser numerica".to_string()));
        }

        if self.buyer_nit.is_empty() || !self.buyer_nit.chars().all(|c| c.is_ascii_digit()) {
            return Err(UblError::Validation("El NIT del comprador debe ser numerico".to_string()));
        }

        if self.net_amount < 0.0 || self.tax_amount < 0.0 || self.total_amount < 0.0 {
            return Err(UblError::Validation("Los montos del documento soporte no pueden ser negativos".to_string()));
        }

        if self.environment != "1" && self.environment != "2" {
            return Err(UblError::Validation("El ambiente debe ser '1' (produccion) o '2' (habilitacion)".to_string()));
        }

        Ok(())
    }
}

#[cfg(test)]
mod additional_payload_tests {
    use super::*;

    #[test]
    fn test_valid_payroll_payload() {
        let p = PayrollPayload {
            prefix: "NOM".to_string(),
            number: 1,
            issue_date: "2026-05-26".to_string(),
            issue_time: "08:00:00".to_string(),
            employer_nit: "900123456".to_string(),
            employer_name: "Empresa".to_string(),
            employee_id_type: "13".to_string(),
            employee_id: "102030".to_string(),
            employee_name: "Empleado".to_string(),
            devengado: 1000.0,
            deducido: 100.0,
            total: 900.0,
            software_pin: "pin".to_string(),
            environment: "2".to_string(),
        };
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_valid_support_doc_payload() {
        let sd = SupportDocumentPayload {
            prefix: "DS".to_string(),
            number: 1,
            issue_date: "2026-05-26".to_string(),
            issue_time: "10:00:00-05:00".to_string(),
            seller_nit: "10203040".to_string(),
            seller_name: "No Obligado".to_string(),
            buyer_nit: "900123456".to_string(),
            buyer_name: "Comprador".to_string(),
            net_amount: 100.0,
            tax_amount: 0.0,
            total_amount: 100.0,
            software_pin: "pin".to_string(),
            environment: "2".to_string(),
        };
        assert!(sd.validate().is_ok());
    }
}

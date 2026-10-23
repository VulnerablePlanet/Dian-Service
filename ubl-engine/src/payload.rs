use crate::error::UblError;
use core_domain::money::{require_equal, to_amount};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

fn amount(field: &str, value: f64) -> Result<Decimal, UblError> {
    to_amount(field, value).map_err(|e| UblError::Validation(e.to_string()))
}

fn equal(description: &str, expected: Decimal, actual: Decimal) -> Result<(), UblError> {
    require_equal(description, expected, actual).map_err(|e| UblError::Validation(e.to_string()))
}

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
    /// Obsoleto: la clave técnica se toma de la resolución de numeración. Se acepta para no
    /// romper clientes existentes, pero se ignora y nunca se persiste.
    #[serde(default, skip_serializing)]
    pub technical_key: Option<String>,
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
        let net = amount("net_amount", self.net_amount)?;
        let tax = amount("tax_amount", self.tax_amount)?;
        let total = amount("total_amount", self.total_amount)?;
        equal("total_amount debe ser net_amount + tax_amount", net + tax, total)?;

        // 7. Validar tipo de documento
        // Las notas crédito/débito requieren su propia plantilla UBL, BillingReference y CUDE;
        // hasta implementarlas se rechazan para no emitirlas como factura de venta.
        match self.document_type.as_str() {
            "invoice" => {}
            "credit_note" | "debit_note" => {
                return Err(UblError::Validation(
                    "Las notas crédito y débito aún no están soportadas".to_string(),
                ));
            }
            _ => {
                return Err(UblError::Validation(
                    "Tipo de documento inválido. Debe ser 'invoice'".to_string(),
                ));
            }
        }

        // 7b. Validar el identificador del set de pruebas (se inserta en el cuerpo SOAP)
        if let Some(test_set_id) = &self.test_set_id {
            let valid = !test_set_id.is_empty()
                && test_set_id.len() <= 64
                && test_set_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            if !valid {
                return Err(UblError::Validation(
                    "El test_set_id solo puede contener letras, números y guiones (máximo 64)".to_string(),
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
            technical_key: None,
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
    fn test_credit_and_debit_notes_are_rejected() {
        for doc_type in ["credit_note", "debit_note"] {
            let mut payload = mock_payload();
            payload.document_type = doc_type.to_string();
            assert!(payload.validate().is_err(), "{} must be rejected", doc_type);
        }
    }

    #[test]
    fn test_test_set_id_format() {
        let mut payload = mock_payload();
        payload.test_set_id = Some("8a7b6c5d-1234-4f00-9abc-0123456789ab".to_string());
        assert!(payload.validate().is_ok());
        payload.test_set_id = Some("abc</wcf:testSetId><x>".to_string());
        assert!(payload.validate().is_err());
        payload.test_set_id = Some(String::new());
        assert!(payload.validate().is_err());
    }

    #[test]
    fn test_totals_must_add_up() {
        let mut payload = mock_payload();
        payload.total_amount = 5.0;
        assert!(payload.validate().is_err());
        payload.total_amount = 1190.0;
        payload.tax_amount = 190.005;
        assert!(payload.validate().is_err(), "more than two decimals must be rejected");
    }

    #[test]
    fn test_legacy_technical_key_is_accepted_but_never_serialized() {
        let mut payload = mock_payload();
        payload.technical_key = Some("secreto".to_string());
        assert!(payload.validate().is_ok());
        let value = serde_json::to_value(&payload).unwrap();
        assert!(value.get("technical_key").is_none());
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

        let devengado = amount("devengado", self.devengado)?;
        let deducido = amount("deducido", self.deducido)?;
        let total = amount("total", self.total)?;
        equal("total debe ser devengado - deducido", devengado - deducido, total)?;

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

        let net = amount("net_amount", self.net_amount)?;
        let tax = amount("tax_amount", self.tax_amount)?;
        let total = amount("total_amount", self.total_amount)?;
        equal("total_amount debe ser net_amount + tax_amount", net + tax, total)?;

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

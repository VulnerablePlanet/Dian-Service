//! Montos monetarios en COP.
//!
//! Los DTO de la API reciben `f64` por compatibilidad, pero toda validación y todo cálculo se
//! hacen en `Decimal`. La conversión usa la representación decimal más corta del `f64`
//! (la misma que escribió el cliente en el JSON), no su valor binario aproximado.

use crate::ports::DomainError;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Máximo de decimales admitidos para montos en COP.
pub const MAX_SCALE: u32 = 2;

/// Convierte un monto del DTO a `Decimal`, exigiendo que sea finito, no negativo y con a lo
/// sumo dos decimales.
pub fn to_amount(field: &str, value: f64) -> Result<Decimal, DomainError> {
    if !value.is_finite() {
        return Err(DomainError::Validation(format!("El monto '{}' no es un numero valido", field)));
    }
    let amount = Decimal::from_str(&value.to_string())
        .map_err(|_| DomainError::Validation(format!("El monto '{}' no es representable", field)))?
        .normalize();
    if amount.is_sign_negative() {
        return Err(DomainError::Validation(format!("El monto '{}' no puede ser negativo", field)));
    }
    if amount.scale() > MAX_SCALE {
        return Err(DomainError::Validation(format!(
            "El monto '{}' admite como maximo {} decimales",
            field, MAX_SCALE
        )));
    }
    Ok(amount)
}

/// Exige que `expected` sea exactamente igual a `actual`.
pub fn require_equal(description: &str, expected: Decimal, actual: Decimal) -> Result<(), DomainError> {
    if expected != actual {
        return Err(DomainError::Validation(format!(
            "{}: se esperaba {} pero se recibio {}",
            description,
            format_amount(expected),
            format_amount(actual)
        )));
    }
    Ok(())
}

/// Formato con dos decimales exactos, como lo exigen el XML UBL y el CUFE.
pub fn format_amount(amount: Decimal) -> String {
    format!("{:.2}", amount.round_dp(MAX_SCALE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_what_the_client_wrote() {
        assert_eq!(format_amount(to_amount("x", 0.1).unwrap()), "0.10");
        assert_eq!(format_amount(to_amount("x", 119000.0).unwrap()), "119000.00");
        assert_eq!(format_amount(to_amount("x", 1234567.89).unwrap()), "1234567.89");
    }

    #[test]
    fn decimal_sum_is_exact_where_f64_is_not() {
        let a = to_amount("a", 0.1).unwrap();
        let b = to_amount("b", 0.2).unwrap();
        assert!(require_equal("suma", to_amount("t", 0.3).unwrap(), a + b).is_ok());
        assert_ne!(0.1_f64 + 0.2_f64, 0.3_f64);
    }

    #[test]
    fn rejects_invalid_amounts() {
        assert!(to_amount("x", -1.0).is_err());
        assert!(to_amount("x", 10.005).is_err());
        assert!(to_amount("x", f64::NAN).is_err());
        assert!(to_amount("x", f64::INFINITY).is_err());
    }

    #[test]
    fn require_equal_reports_mismatch() {
        let err = require_equal("Total", to_amount("t", 119.0).unwrap(), to_amount("t", 5.0).unwrap()).unwrap_err();
        assert!(err.to_string().contains("119.00"));
    }
}

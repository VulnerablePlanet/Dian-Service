//! Reglas de numeración de una resolución DIAN.

use crate::models::NumberingRange;
use crate::ports::DomainError;
use chrono::{DateTime, Utc};

/// Calcula el siguiente consecutivo de la resolución.
///
/// - La resolución debe estar vigente en `now`.
/// - El siguiente número es `max(current_counter, from_number - 1) + 1`, de modo que el
///   primer documento de un rango que empieza en 5000 recibe 5000.
/// - Si el cliente propone un número, debe ser exactamente el siguiente: no se admiten
///   saltos que dejen consecutivos inutilizables.
pub fn next_number(
    range: &NumberingRange,
    requested: Option<i32>,
    now: DateTime<Utc>,
) -> Result<i32, DomainError> {
    if now < range.valid_date_from || now > range.valid_date_to {
        return Err(DomainError::Validation(format!(
            "La resolucion de numeracion del prefijo '{}' no esta vigente (vigencia {} a {})",
            range.prefix,
            range.valid_date_from.date_naive(),
            range.valid_date_to.date_naive()
        )));
    }

    let next = range.current_counter.max(range.from_number - 1) + 1;
    if next > range.to_number {
        return Err(DomainError::Validation(format!(
            "El rango de numeracion del prefijo '{}' esta agotado (limite {})",
            range.prefix, range.to_number
        )));
    }

    match requested {
        Some(number) if number != next => Err(DomainError::Validation(format!(
            "El numero {} no es el siguiente consecutivo del prefijo '{}'; se esperaba {}",
            number, range.prefix, next
        ))),
        _ => Ok(next),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DocumentType;
    use chrono::Duration;
    use uuid::Uuid;

    fn range(from: i32, to: i32, counter: i32) -> NumberingRange {
        NumberingRange {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            document_type: DocumentType::Invoice,
            prefix: "FE".into(),
            from_number: from,
            to_number: to,
            technical_key: "tk".into(),
            valid_date_from: Utc::now() - Duration::days(1),
            valid_date_to: Utc::now() + Duration::days(1),
            current_counter: counter,
        }
    }

    #[test]
    fn first_number_starts_at_from_number() {
        assert_eq!(next_number(&range(5000, 6000, 0), None, Utc::now()).unwrap(), 5000);
    }

    #[test]
    fn continues_after_counter() {
        assert_eq!(next_number(&range(1, 100, 41), None, Utc::now()).unwrap(), 42);
        assert_eq!(next_number(&range(1, 100, 41), Some(42), Utc::now()).unwrap(), 42);
    }

    #[test]
    fn rejects_gaps_and_reused_numbers() {
        assert!(next_number(&range(1, 100, 41), Some(45), Utc::now()).is_err());
        assert!(next_number(&range(1, 100, 41), Some(41), Utc::now()).is_err());
    }

    #[test]
    fn rejects_exhausted_range() {
        assert!(next_number(&range(1, 100, 100), None, Utc::now()).is_err());
    }

    #[test]
    fn rejects_resolution_outside_validity() {
        let r = range(1, 100, 0);
        assert!(next_number(&r, None, r.valid_date_to + Duration::seconds(1)).is_err());
        assert!(next_number(&r, None, r.valid_date_from - Duration::seconds(1)).is_err());
    }
}

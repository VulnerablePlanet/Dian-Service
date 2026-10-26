//! NIT colombiano: dígito de verificación (módulo 11 de la DIAN).

/// Pesos aplicados de derecha a izquierda a los dígitos del NIT.
const WEIGHTS: [u32; 15] = [3, 7, 13, 17, 19, 23, 29, 37, 41, 43, 47, 53, 59, 67, 71];

/// Calcula el dígito de verificación de un NIT (solo dígitos, sin DV). `None` si el valor no
/// es numérico o tiene más de 15 dígitos.
pub fn check_digit(nit: &str) -> Option<u32> {
    if nit.is_empty() || nit.len() > WEIGHTS.len() || !nit.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let sum: u32 = nit
        .chars()
        .rev()
        .zip(WEIGHTS)
        .map(|(c, weight)| c.to_digit(10).unwrap() * weight)
        .sum();
    let remainder = sum % 11;
    Some(if remainder >= 2 { 11 - remainder } else { remainder })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_published_nits() {
        assert_eq!(check_digit("890903938"), Some(8)); // Bancolombia
        assert_eq!(check_digit("800197268"), Some(4)); // DIAN
        assert_eq!(check_digit("860034313"), Some(7)); // Davivienda
    }

    #[test]
    fn rejects_invalid_input() {
        assert_eq!(check_digit(""), None);
        assert_eq!(check_digit("90012345-6"), None);
        assert_eq!(check_digit("1234567890123456"), None);
    }
}

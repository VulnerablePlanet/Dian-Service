//! API keys de los tenants: solo se persiste su huella SHA-256.
//!
//! Las claves generadas tienen 256 bits aleatorios, así que un hash rápido sin sal basta (no
//! hay diccionario que probar) y permite buscarlas por índice único.

use sha2::{Digest, Sha256};

/// Prefijo de las claves generadas por el servicio.
pub const KEY_PREFIX: &str = "dsk_";
/// Caracteres visibles que identifican una clave sin revelarla (`dsk_` + 8).
pub const VISIBLE_PREFIX_LEN: usize = 12;

/// SHA-256 (hex en minúsculas) de la clave presentada por el cliente.
pub fn hash(api_key: &str) -> String {
    Sha256::digest(api_key.as_bytes()).iter().map(|b| format!("{:02x}", b)).collect()
}

/// Comparación en tiempo constante (para secretos de igual longitud conocida).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_sha256_hex() {
        // printf 'abc' | sha256sum
        assert_eq!(hash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn constant_time_comparison() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"sane"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }
}

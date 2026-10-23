//! Datos que reciben las plantillas UBL, calculados en `Decimal`.

use crate::error::UblError;
use crate::payload::LinePayload;
use core_domain::money::format_amount;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::Serialize;
use std::collections::BTreeMap;
use std::str::FromStr;

const MAX_LINES: usize = 500;
const DEFAULT_UNIT_CODE: &str = "94";
const SYNTHETIC_LINE_DESCRIPTION: &str = "Venta de bienes y/o servicios";

fn decimal(field: &str, value: f64, max_scale: u32, allow_zero: bool) -> Result<Decimal, UblError> {
    if !value.is_finite() {
        return Err(UblError::Validation(format!("{} no es un numero valido", field)));
    }
    let d = Decimal::from_str(&value.to_string())
        .map_err(|_| UblError::Validation(format!("{} no es representable", field)))?
        .normalize();
    if d.is_sign_negative() || (!allow_zero && d.is_zero()) {
        return Err(UblError::Validation(format!(
            "{} debe ser {}",
            field,
            if allow_zero { "mayor o igual a cero" } else { "mayor que cero" }
        )));
    }
    if d.scale() > max_scale {
        return Err(UblError::Validation(format!("{} admite como maximo {} decimales", field, max_scale)));
    }
    Ok(d)
}

fn round2(d: Decimal) -> Decimal {
    d.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

/// Representación con al menos dos decimales, sin perder los que tenga el valor.
fn format_decimal(d: Decimal) -> String {
    let d = d.normalize();
    if d.scale() < 2 {
        format_amount(d)
    } else {
        d.to_string()
    }
}

/// Línea de detalle con importes calculados.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LineData {
    pub id: usize,
    pub description: String,
    pub quantity: String,
    pub unit_code: String,
    pub unit_price: String,
    pub line_extension: String,
    pub tax_percent: String,
    pub tax_amount: String,
}

/// Subtotal de IVA de una tarifa.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaxSubtotalData {
    pub taxable: String,
    pub tax: String,
    pub percent: String,
}

struct ComputedLine {
    data: LineData,
    line_extension: Decimal,
    tax_amount: Decimal,
    tax_percent: Decimal,
}

/// Calcula las líneas y comprueba que cuadren con los totales: la suma de los valores de
/// línea debe ser `net` y la suma de los IVA de línea debe ser `tax`. Sin líneas, genera una
/// única línea equivalente a los totales.
pub fn compute_lines(lines: Option<&[LinePayload]>, net: Decimal, tax: Decimal) -> Result<Vec<LineData>, UblError> {
    Ok(compute(lines, net, tax)?.into_iter().map(|l| l.data).collect())
}

fn compute(lines: Option<&[LinePayload]>, net: Decimal, tax: Decimal) -> Result<Vec<ComputedLine>, UblError> {
    let Some(lines) = lines else {
        let percent = if net.is_zero() { Decimal::ZERO } else { round2(tax * Decimal::from(100) / net) };
        return Ok(vec![ComputedLine {
            data: LineData {
                id: 1,
                description: SYNTHETIC_LINE_DESCRIPTION.to_string(),
                quantity: "1".to_string(),
                unit_code: DEFAULT_UNIT_CODE.to_string(),
                unit_price: format_amount(net),
                line_extension: format_amount(net),
                tax_percent: format_amount(percent),
                tax_amount: format_amount(tax),
            },
            line_extension: net,
            tax_amount: tax,
            tax_percent: percent,
        }]);
    };

    if lines.is_empty() || lines.len() > MAX_LINES {
        return Err(UblError::Validation(format!("lines debe tener entre 1 y {} elementos", MAX_LINES)));
    }

    let mut computed = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let n = index + 1;
        if line.description.trim().is_empty() || line.description.chars().count() > 300 {
            return Err(UblError::Validation(format!("lines[{}].description debe tener entre 1 y 300 caracteres", n)));
        }
        let quantity = decimal(&format!("lines[{}].quantity", n), line.quantity, 6, false)?;
        let unit_price = decimal(&format!("lines[{}].unit_price", n), line.unit_price, 4, true)?;
        let percent = decimal(&format!("lines[{}].tax_percent", n), line.tax_percent, 2, true)?;
        if percent > Decimal::from(100) {
            return Err(UblError::Validation(format!("lines[{}].tax_percent no puede superar 100", n)));
        }
        let unit_code = line.unit_code.clone().unwrap_or_else(|| DEFAULT_UNIT_CODE.to_string());
        if unit_code.is_empty() || unit_code.len() > 3 || !unit_code.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(UblError::Validation(format!("lines[{}].unit_code no es un codigo valido", n)));
        }
        let line_extension = round2(quantity * unit_price);
        let tax_amount = round2(line_extension * percent / Decimal::from(100));
        computed.push(ComputedLine {
            data: LineData {
                id: n,
                description: line.description.clone(),
                quantity: quantity.normalize().to_string(),
                unit_code,
                unit_price: format_decimal(unit_price),
                line_extension: format_amount(line_extension),
                tax_percent: format_amount(percent),
                tax_amount: format_amount(tax_amount),
            },
            line_extension,
            tax_amount,
            tax_percent: percent,
        });
    }

    let lines_net: Decimal = computed.iter().map(|l| l.line_extension).sum();
    let lines_tax: Decimal = computed.iter().map(|l| l.tax_amount).sum();
    if lines_net != net {
        return Err(UblError::Validation(format!(
            "La suma de las lineas ({}) no coincide con net_amount ({})",
            format_amount(lines_net),
            format_amount(net)
        )));
    }
    if lines_tax != tax {
        return Err(UblError::Validation(format!(
            "La suma del IVA de las lineas ({}) no coincide con tax_amount ({})",
            format_amount(lines_tax),
            format_amount(tax)
        )));
    }
    Ok(computed)
}

/// Subtotales de IVA agrupados por tarifa.
pub fn tax_subtotals(lines: Option<&[LinePayload]>, net: Decimal, tax: Decimal) -> Result<Vec<TaxSubtotalData>, UblError> {
    let mut groups: BTreeMap<Decimal, (Decimal, Decimal)> = BTreeMap::new();
    for line in compute(lines, net, tax)? {
        let entry = groups.entry(line.tax_percent).or_default();
        entry.0 += line.line_extension;
        entry.1 += line.tax_amount;
    }
    Ok(groups
        .into_iter()
        .map(|(percent, (taxable, tax))| TaxSubtotalData {
            taxable: format_amount(taxable),
            tax: format_amount(tax),
            percent: format_amount(percent),
        })
        .collect())
}

/// Parte (emisor o adquirente) de un documento.
#[derive(Debug, Clone, Serialize)]
pub struct PartyData {
    /// Tipo de identificación (31 = NIT, 13 = cédula...).
    pub id_type: String,
    pub id: String,
    /// Dígito de verificación (solo para NIT).
    pub check_digit: String,
    pub name: String,
}

impl PartyData {
    pub fn new(id_type: &str, id: &str, name: &str) -> Self {
        let check_digit = if id_type == "31" {
            core_domain::nit::check_digit(id).map(|d| d.to_string()).unwrap_or_default()
        } else {
            String::new()
        };
        Self {
            id_type: id_type.to_string(),
            id: id.to_string(),
            check_digit,
            name: name.to_string(),
        }
    }
}

/// Datos de una factura, documento soporte o nota.
#[derive(Debug, Clone, Serialize)]
pub struct CommercialDocument {
    pub customization_id: String,
    pub profile_execution_id: String,
    pub id: String,
    pub uuid: String,
    pub uuid_scheme: String,
    pub issue_date: String,
    pub issue_time: String,
    pub type_code: String,
    pub supplier: PartyData,
    pub customer: PartyData,
    pub lines: Vec<LineData>,
    pub tax_subtotals: Vec<TaxSubtotalData>,
    pub net: String,
    pub tax: String,
    pub total: String,
    pub billing_reference: Option<crate::payload::BillingReferencePayload>,
    pub discrepancy: Option<crate::payload::DiscrepancyPayload>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(quantity: f64, unit_price: f64, tax_percent: f64) -> LinePayload {
        LinePayload {
            description: "Item".into(),
            quantity,
            unit_price,
            tax_percent,
            unit_code: None,
        }
    }

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn lines_must_add_up_to_the_totals() {
        let lines = vec![line(2.0, 50000.0, 19.0), line(1.0, 10000.0, 0.0)];
        let data = compute_lines(Some(&lines), d("110000"), d("19000")).unwrap();
        assert_eq!(data[0].line_extension, "100000.00");
        assert_eq!(data[0].tax_amount, "19000.00");
        assert_eq!(data[1].tax_amount, "0.00");
        assert!(compute_lines(Some(&lines), d("110000"), d("19001")).is_err());
        assert!(compute_lines(Some(&lines), d("100000"), d("19000")).is_err());
    }

    #[test]
    fn rounding_is_half_away_from_zero_per_line() {
        let lines = vec![line(3.0, 0.335, 19.0)];
        let data = compute_lines(Some(&lines), d("1.01"), d("0.19")).unwrap();
        assert_eq!(data[0].line_extension, "1.01");
        assert_eq!(data[0].unit_price, "0.335");
    }

    #[test]
    fn missing_lines_produce_one_equivalent_line() {
        let data = compute_lines(None, d("100000"), d("19000")).unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0].tax_percent, "19.00");
    }

    #[test]
    fn subtotals_are_grouped_by_rate() {
        let lines = vec![line(1.0, 100.0, 19.0), line(1.0, 200.0, 19.0), line(1.0, 50.0, 5.0)];
        let subtotals = tax_subtotals(Some(&lines), d("350"), d("59.5")).unwrap();
        assert_eq!(subtotals.len(), 2);
        assert_eq!((subtotals[1].taxable.as_str(), subtotals[1].tax.as_str()), ("300.00", "57.00"));
    }

    #[test]
    fn rejects_invalid_lines() {
        assert!(compute_lines(Some(&[]), d("0"), d("0")).is_err());
        assert!(compute_lines(Some(&[line(0.0, 1.0, 19.0)]), d("0"), d("0")).is_err());
        assert!(compute_lines(Some(&[line(1.0, 1.0, 101.0)]), d("1"), d("1.01")).is_err());
    }

    #[test]
    fn nit_parties_get_their_check_digit() {
        assert_eq!(PartyData::new("31", "800197268", "DIAN").check_digit, "4");
        assert_eq!(PartyData::new("13", "10203040", "Persona").check_digit, "");
    }
}

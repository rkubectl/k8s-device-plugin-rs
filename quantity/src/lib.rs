//! Exact quantities for device capacities and shared counters.
//!
//! Syntax, nano rounding and BinarySI saturation follow Kubernetes v1.36
//! apimachinery/pkg/api/resource/{quantity,suffix}.go. BigDecimal owns the
//! decimal representation; this module adds Kubernetes units and wire forms.

use std::fmt;
use std::str::FromStr;

use bigdecimal::BigDecimal;
use bigdecimal::RoundingMode;
use bigdecimal::Signed as _;
use bigdecimal::ToPrimitive as _;
use bigdecimal::Zero as _;

#[cfg(feature = "k8s-openapi")]
mod openapi;

const DECIMAL_SUFFIXES: [(i64, &str); 10] = [
    (-9, "n"),
    (-6, "u"),
    (-3, "m"),
    (0, ""),
    (3, "k"),
    (6, "M"),
    (9, "G"),
    (12, "T"),
    (15, "P"),
    (18, "E"),
];
const BINARY_SUFFIXES: [&str; 6] = ["Ki", "Mi", "Gi", "Ti", "Pi", "Ei"];

#[derive(Clone, Copy, Debug)]
enum Format {
    DecimalSI,
    BinarySI,
    DecimalExponent,
}

/// An exact Kubernetes resource quantity, for example `"0.5"` or `"1.5Gi"`.
///
/// Parse strings with [`FromStr`] or construct integers with [`From`]. Values
/// are rounded away from zero to nano precision, as in Kubernetes. Equality
/// compares the numeric value, independently of units or spelling.
///
/// ```
/// use k8s_quantity::Quantity;
/// let half: Quantity = "0.5".parse()?;
/// assert_eq!(half, "500m".parse()?);
/// assert_eq!(half.to_string(), "500m");
/// assert_eq!(Quantity::from(2).to_string(), "2");
/// # Ok::<(), k8s_quantity::QuantityParseError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Quantity {
    value: BigDecimal,
    format: Format,
}

/// A quantity that does not match Kubernetes' numeric/unit syntax.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid Kubernetes quantity {0:?}")]
pub struct QuantityParseError(String);

impl Quantity {
    /// Consumes this quantity and returns its Kubernetes wire representation.
    #[must_use]
    pub fn into_string(self) -> String {
        self.to_string()
    }

    /// A common decimal representation for comparing wire values with
    /// different notation families, without floating-point conversion.
    #[must_use]
    pub fn canonical_decimal(&self) -> String {
        self.format_decimal(Format::DecimalSI)
    }

    fn format_decimal(&self, format: Format) -> String {
        if self.value.is_zero() {
            return "0".to_string();
        }
        let (coefficient, scale) = self.value.normalized().as_bigint_and_exponent();
        let exponent = -scale;
        let group = exponent.div_euclid(3) * 3;
        let mut digits = coefficient.to_string();
        for _ in 0..exponent.rem_euclid(3) {
            digits.push('0');
        }
        // Values beyond Exa use exponent notation. This preserves their value
        // instead of dropping an unsupported SI suffix in older API servers.
        if !matches!(format, Format::DecimalExponent)
            && let Some((_, suffix)) = DECIMAL_SUFFIXES
                .iter()
                .find(|(exponent, _)| *exponent == group)
        {
            digits.push_str(suffix);
        } else if group != 0 {
            return format!("{digits}e{group}");
        }
        digits
    }
}

impl FromStr for Quantity {
    type Err = QuantityParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let invalid = || QuantityParseError(input.to_string());
        let bytes = input.as_bytes();
        let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
        let start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        let integer_digits = end - start;
        let mut fractional_digits = 0;
        if bytes.get(end) == Some(&b'.') {
            end += 1;
            let start = end;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            fractional_digits = end - start;
        }
        if integer_digits + fractional_digits == 0 {
            return Err(invalid());
        }
        let mut value = input[..end].parse::<BigDecimal>().map_err(|_| invalid())?;
        let suffix = &input[end..];
        let mut format = if let Some(&(exponent, _)) =
            DECIMAL_SUFFIXES.iter().find(|(_, unit)| *unit == suffix)
        {
            let (coefficient, scale) = value.as_bigint_and_exponent();
            value = BigDecimal::new(coefficient, scale - exponent);
            Format::DecimalSI
        } else if let Some(index) = BINARY_SUFFIXES.iter().position(|unit| *unit == suffix) {
            value *= 1_u64 << ((index + 1) * 10);
            Format::BinarySI
        } else if let Some(exponent) = suffix.strip_prefix(['e', 'E']) {
            let exponent = exponent.parse::<i32>().map_err(|_| invalid())?;
            let (coefficient, scale) = value.as_bigint_and_exponent();
            value = BigDecimal::new(coefficient, scale - i64::from(exponent));
            Format::DecimalExponent
        } else {
            return Err(invalid());
        };

        // with_scale_round's reduction path operates on coefficient digits;
        // even extreme small exponents do not allocate a power of ten.
        if value.as_bigint_and_exponent().1 > 9 {
            value = value.with_scale_round(9, RoundingMode::Up);
        }
        if matches!(format, Format::BinarySI) {
            let max = BigDecimal::from(i64::MAX);
            if value.abs() > max {
                value = if value.is_negative() { -max } else { max };
            }
            if value.abs() < 1 {
                format = Format::DecimalSI;
            }
        }
        Ok(Self {
            value: value.normalized(),
            format,
        })
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Binary quantities were bounded to signed-64-bit magnitude when
        // parsed. Use exact decimal notation when there is no integer suffix.
        if matches!(self.format, Format::BinarySI)
            && self.value.is_integer()
            && let Some(value) = self.value.to_i64()
        {
            for (index, suffix) in BINARY_SUFFIXES.iter().enumerate().rev() {
                let factor = 1_i64 << ((index + 1) * 10);
                if value != 0 && value % factor == 0 {
                    return write!(f, "{}{suffix}", value / factor);
                }
            }
        }
        f.write_str(&self.format_decimal(self.format))
    }
}

impl PartialEq for Quantity {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl Eq for Quantity {}

impl Default for Quantity {
    fn default() -> Self {
        Self::from(0)
    }
}

macro_rules! from_integer {
    ($($type:ty),* $(,)?) => {$(
        impl From<$type> for Quantity {
            fn from(value: $type) -> Self {
                Self { value: BigDecimal::from(value), format: Format::DecimalSI }
            }
        }
    )*};
}

from_integer!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);

#[cfg(test)]
mod tests;

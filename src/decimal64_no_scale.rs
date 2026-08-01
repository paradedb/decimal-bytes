//! Fixed-precision 64-bit decimal type with external scale.
//!
//! `Decimal64NoScale` provides maximum precision by using all 64 bits for the
//! mantissa, with scale stored externally (e.g., in schema metadata).
//!
//! ## When to Use
//!
//! - **Decimal64NoScale**: For columnar storage where scale is in metadata
//! - **Decimal64**: For self-contained decimals where scale must be embedded
//! - **Decimal**: For arbitrary precision or when precision exceeds 18 digits
//!
//! ## Comparison with Decimal64
//!
//! |       Type         |   Scale Storage   | Mantissa Bits | Max Digits |           Aggregates               |
//! |--------------------|-------------------|---------------|------------|------------------------------------|
//! | `Decimal64`        | Embedded (8 bits) | 56 bits       | 16         | **Wrong** (scale bits corrupt sum) |
//! | `Decimal64NoScale` | External          | 64 bits       | **18**     | **Correct** (raw integer math)     |
//!
//! ## Storage Layout
//!
//! ```text
//! 64-bit representation:
//! ┌─────────────────────────────────────────────────────────────────┐
//! │ Value (64 bits, signed) - represents value * 10^scale           │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Special values use sentinel i64 values (PostgreSQL sort order):
//! - `i64::MIN`: -Infinity (sorts lowest)
//! - `i64::MAX - 1`: +Infinity
//! - `i64::MAX`: NaN (sorts highest, per PostgreSQL semantics)
//!
//! ## Example
//!
//! ```
//! use decimal_bytes::Decimal64NoScale;
//!
//! // Create with external scale
//! let scale = 2;
//! let price = Decimal64NoScale::new("123.45", scale).unwrap();
//! assert_eq!(price.value(), 12345);  // Raw scaled value
//! assert_eq!(price.to_string_with_scale(scale), "123.45");
//!
//! // Aggregates work correctly!
//! let a = Decimal64NoScale::new("100.50", scale).unwrap();
//! let b = Decimal64NoScale::new("200.25", scale).unwrap();
//! let sum = a.value() + b.value();  // 30075
//! // Interpret: 30075 / 10^2 = 300.75 ✓
//! ```

use std::cmp::Ordering;
use std::fmt;
use std::hash::Hash;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Decimal;
use crate::encoding::DecimalError;

/// Maximum precision that fits in signed i64 (18 digits).
/// i64::MAX = 9,223,372,036,854,775,807 ≈ 9.2 × 10^18
pub const MAX_DECIMAL64_NO_SCALE_PRECISION: u32 = 18;

/// Maximum scale supported.
pub const MAX_DECIMAL64_NO_SCALE_SCALE: i32 = 18;

// Sentinel values for special cases (PostgreSQL sort order: -Inf < numbers < +Inf < NaN)
const SENTINEL_NEG_INFINITY: i64 = i64::MIN;
const SENTINEL_POS_INFINITY: i64 = i64::MAX - 1;
const SENTINEL_NAN: i64 = i64::MAX;

// 18-digit precision limits (matching MAX_DECIMAL64_NO_SCALE_PRECISION)
const MIN_VALUE: i64 = -999_999_999_999_999_999i64;
const MAX_VALUE: i64 = 999_999_999_999_999_999i64;

/// A fixed-precision decimal stored as a raw 64-bit integer.
///
/// ## Design
///
/// Unlike `Decimal64` which embeds scale, `Decimal64NoScale` stores only the
/// mantissa. This provides:
/// - **2 more digits of precision** (18 vs 16)
/// - **Correct aggregates** (sum/min/max work with raw integer math)
/// - **Columnar storage compatibility** (scale in metadata, not per-value)
///
/// ## Aggregates
///
/// ```text
/// stored_values = [a*10^s, b*10^s, c*10^s]
/// SUM(stored_values) = (a+b+c) * 10^s
/// actual_sum = SUM / 10^s = a+b+c  ✓
/// ```
///
/// ## Special Values
///
/// Special values use sentinel i64 values that sort correctly in standard i64 ordering:
/// - `i64::MIN`: -Infinity (sorts lowest)
/// - `i64::MAX - 1`: +Infinity
/// - `i64::MAX`: NaN (sorts highest, per PostgreSQL semantics)
#[derive(Clone, Copy, Default, Eq, PartialEq, Hash)]
pub struct Decimal64NoScale {
    /// Raw value: actual_value * 10^scale (scale stored externally)
    value: i64,
}

impl Decimal64NoScale {
    // ==================== Constructors ====================

    /// Creates a Decimal64NoScale from a string with the given scale.
    ///
    /// # Arguments
    /// * `s` - The decimal string (e.g., "123.45")
    /// * `scale` - The number of decimal places to store
    ///
    /// # Examples
    ///
    /// ```
    /// use decimal_bytes::Decimal64NoScale;
    ///
    /// let d = Decimal64NoScale::new("123.45", 2).unwrap();
    /// assert_eq!(d.value(), 12345);
    /// assert_eq!(d.to_string_with_scale(2), "123.45");
    /// ```
    pub fn new(s: &str, scale: i32) -> Result<Self, DecimalError> {
        let s = s.trim();

        // Handle special values (case-insensitive)
        let lower = s.to_lowercase();
        match lower.as_str() {
            "nan" | "-nan" | "+nan" => return Ok(Self::nan()),
            "infinity" | "inf" | "+infinity" | "+inf" => return Ok(Self::infinity()),
            "-infinity" | "-inf" => return Ok(Self::neg_infinity()),
            _ => {}
        }

        // Validate scale
        if scale.abs() > MAX_DECIMAL64_NO_SCALE_SCALE {
            return Err(DecimalError::InvalidFormat(format!(
                "Scale {} exceeds maximum {} for Decimal64NoScale",
                scale.abs(),
                MAX_DECIMAL64_NO_SCALE_SCALE
            )));
        }

        // Parse sign
        let (is_negative, s) = if let Some(rest) = s.strip_prefix('-') {
            (true, rest)
        } else if let Some(rest) = s.strip_prefix('+') {
            (false, rest)
        } else {
            (false, s)
        };

        // Split into integer and fractional parts
        let (int_part, frac_part) = if let Some(dot_pos) = s.find('.') {
            (&s[..dot_pos], &s[dot_pos + 1..])
        } else {
            (s, "")
        };

        // Trim leading zeros from integer part
        let int_part = int_part.trim_start_matches('0');
        let int_part = if int_part.is_empty() { "0" } else { int_part };

        // Convert to scaled integer
        let value = Self::compute_scaled_value(int_part, frac_part, is_negative, scale)?;

        if !(MIN_VALUE..=MAX_VALUE).contains(&value) {
            return Err(DecimalError::InvalidFormat(
                "Value too large for Decimal64NoScale".to_string(),
            ));
        }

        Ok(Self { value })
    }

    /// Creates a Decimal64NoScale from a raw i64 value.
    ///
    /// Use this when you already have the scaled integer value.
    ///
    /// # Examples
    ///
    /// ```
    /// use decimal_bytes::Decimal64NoScale;
    ///
    /// let d = Decimal64NoScale::from_raw(12345);
    /// assert_eq!(d.value(), 12345);
    /// assert_eq!(d.to_string_with_scale(2), "123.45");
    /// ```
    #[inline]
    pub const fn from_raw(value: i64) -> Self {
        Self { value }
    }

    /// Creates a Decimal64NoScale from an i64 with the given scale.
    ///
    /// Multiplies the value by 10^scale. Returns an error if the result overflows.
    ///
    /// # Examples
    ///
    /// ```
    /// use decimal_bytes::Decimal64NoScale;
    ///
    /// let d = Decimal64NoScale::from_i64(123, 2).unwrap();
    /// assert_eq!(d.value(), 12300);  // 123 * 10^2
    /// assert_eq!(d.to_string_with_scale(2), "123.00");  // Preserves trailing zeros
    /// ```
    pub fn from_i64(value: i64, scale: i32) -> Result<Self, DecimalError> {
        if scale < 0 {
            // Negative scale: divide (rounds toward zero)
            let divisor = 10i64.pow((-scale) as u32);
            return Ok(Self {
                value: value / divisor,
            });
        }

        if scale == 0 {
            return Ok(Self { value });
        }

        let scale_factor = 10i64.pow(scale as u32);
        let scaled = value.checked_mul(scale_factor).ok_or_else(|| {
            DecimalError::InvalidFormat(format!(
                "Overflow: {} * 10^{} exceeds i64 range",
                value, scale
            ))
        })?;

        if !(MIN_VALUE..=MAX_VALUE).contains(&scaled) {
            return Err(DecimalError::InvalidFormat(
                "Value too large for Decimal64NoScale".to_string(),
            ));
        }

        Ok(Self { value: scaled })
    }

    /// Creates a Decimal64NoScale from a u64 with the given scale.
    ///
    /// Multiplies the value by 10^scale. Returns an error if the result overflows.
    ///
    /// # Examples
    ///
    /// ```
    /// use decimal_bytes::Decimal64NoScale;
    ///
    /// let d = Decimal64NoScale::from_u64(123, 2).unwrap();
    /// assert_eq!(d.value(), 12300);  // 123 * 10^2
    /// ```
    pub fn from_u64(value: u64, scale: i32) -> Result<Self, DecimalError> {
        if value > i64::MAX as u64 {
            return Err(DecimalError::InvalidFormat(format!(
                "Value {} exceeds i64::MAX",
                value
            )));
        }
        Self::from_i64(value as i64, scale)
    }

    /// Creates a Decimal64NoScale from an f64 with the given scale.
    ///
    /// Converts via string to avoid floating-point precision loss during scaling.
    /// Returns an error for NaN/Infinity or if the result overflows.
    ///
    /// # Examples
    ///
    /// ```
    /// use decimal_bytes::Decimal64NoScale;
    ///
    /// let d = Decimal64NoScale::from_f64(123.45, 2).unwrap();
    /// assert_eq!(d.value(), 12345);
    /// assert_eq!(d.to_string_with_scale(2), "123.45");
    /// ```
    pub fn from_f64(value: f64, scale: i32) -> Result<Self, DecimalError> {
        if value.is_nan() {
            return Ok(Self::nan());
        }
        if value.is_infinite() {
            return Ok(if value.is_sign_positive() {
                Self::infinity()
            } else {
                Self::neg_infinity()
            });
        }
        // Use string conversion to avoid precision loss
        Self::new(&value.to_string(), scale)
    }

    // ==================== Special Value Constructors ====================

    /// Creates positive infinity.
    #[inline]
    pub const fn infinity() -> Self {
        Self {
            value: SENTINEL_POS_INFINITY,
        }
    }

    /// Creates negative infinity.
    #[inline]
    pub const fn neg_infinity() -> Self {
        Self {
            value: SENTINEL_NEG_INFINITY,
        }
    }

    /// Creates NaN (Not a Number).
    ///
    /// Follows PostgreSQL semantics: `NaN == NaN` is `true`.
    #[inline]
    pub const fn nan() -> Self {
        Self {
            value: SENTINEL_NAN,
        }
    }

    // ==================== Accessors ====================

    /// Returns the raw i64 value.
    ///
    /// For normal values, this is `actual_value * 10^scale`.
    /// For special values, this returns the sentinel value.
    #[inline]
    pub const fn value(&self) -> i64 {
        self.value
    }

    /// Returns the raw i64 value (alias for columnar storage compatibility).
    #[inline]
    pub const fn raw(&self) -> i64 {
        self.value
    }

    /// Returns true if this value is zero.
    #[inline]
    pub fn is_zero(&self) -> bool {
        !self.is_special() && self.value == 0
    }

    /// Returns true if this value is negative (excluding special values).
    #[inline]
    pub fn is_negative(&self) -> bool {
        !self.is_special() && self.value < 0
    }

    /// Returns true if this value is positive (excluding special values).
    #[inline]
    pub fn is_positive(&self) -> bool {
        !self.is_special() && self.value > 0
    }

    /// Returns true if this value is positive infinity.
    #[inline]
    pub fn is_pos_infinity(&self) -> bool {
        self.value == SENTINEL_POS_INFINITY
    }

    /// Returns true if this value is negative infinity.
    #[inline]
    pub fn is_neg_infinity(&self) -> bool {
        self.value == SENTINEL_NEG_INFINITY
    }

    /// Returns true if this value is positive or negative infinity.
    #[inline]
    pub fn is_infinity(&self) -> bool {
        self.is_pos_infinity() || self.is_neg_infinity()
    }

    /// Returns true if this value is NaN (Not a Number).
    #[inline]
    pub fn is_nan(&self) -> bool {
        self.value == SENTINEL_NAN
    }

    /// Returns true if this is a special value (Infinity or NaN).
    #[inline]
    pub fn is_special(&self) -> bool {
        self.value == SENTINEL_NAN
            || self.value == SENTINEL_NEG_INFINITY
            || self.value == SENTINEL_POS_INFINITY
    }

    /// Returns true if this is a finite number (not Infinity or NaN).
    #[inline]
    pub fn is_finite(&self) -> bool {
        !self.is_special()
    }

    // ==================== Conversions ====================

    /// Formats the value as a decimal string using the given scale.
    ///
    /// This preserves trailing zeros to match the specified scale, which is
    /// important for PostgreSQL NUMERIC display formatting.
    ///
    /// - Positive scale: adds decimal point with trailing zeros as needed
    /// - Zero scale: integer output, no decimal point
    /// - Negative scale: multiplies by power of 10, no decimal point
    ///
    /// # Arguments
    /// * `scale` - The scale to use for formatting
    ///
    /// # Examples
    ///
    /// ```
    /// use decimal_bytes::Decimal64NoScale;
    ///
    /// // Positive scale: includes decimal point and trailing zeros
    /// let d = Decimal64NoScale::from_raw(12345);
    /// assert_eq!(d.to_string_with_scale(2), "123.45");
    /// assert_eq!(d.to_string_with_scale(3), "12.345");
    ///
    /// // Trailing zeros are preserved for positive scale
    /// let d = Decimal64NoScale::from_raw(300);  // Represents 3.00 with scale 2
    /// assert_eq!(d.to_string_with_scale(2), "3.00");
    ///
    /// // Zero also includes trailing zeros for positive scale
    /// let zero = Decimal64NoScale::from_raw(0);
    /// assert_eq!(zero.to_string_with_scale(2), "0.00");
    ///
    /// // Zero scale: no decimal point
    /// let d = Decimal64NoScale::from_raw(12345);
    /// assert_eq!(d.to_string_with_scale(0), "12345");
    /// assert_eq!(zero.to_string_with_scale(0), "0");
    ///
    /// // Negative scale: multiplies, no decimal point
    /// let d = Decimal64NoScale::from_raw(123);
    /// assert_eq!(d.to_string_with_scale(-2), "12300");
    /// ```
    pub fn to_string_with_scale(&self, scale: i32) -> String {
        // Handle special values
        if self.is_neg_infinity() {
            return "-Infinity".to_string();
        }
        if self.is_pos_infinity() {
            return "Infinity".to_string();
        }
        if self.is_nan() {
            return "NaN".to_string();
        }

        let value = self.value;

        if value == 0 {
            return if scale > 0 {
                format!("0.{}", "0".repeat(scale as usize))
            } else {
                "0".to_string()
            };
        }

        let is_negative = value < 0;
        let abs_value = value.unsigned_abs();

        if scale <= 0 {
            // Negative or zero scale: multiply
            let result = if scale < 0 {
                abs_value * 10u64.pow((-scale) as u32)
            } else {
                abs_value
            };
            return if is_negative {
                format!("-{}", result)
            } else {
                result.to_string()
            };
        }

        let scale_factor = 10u64.pow(scale as u32);
        let int_part = abs_value / scale_factor;
        let frac_part = abs_value % scale_factor;

        // Always include the decimal point and full scale digits (with trailing zeros)
        let frac_str = format!("{:0>width$}", frac_part, width = scale as usize);
        let result = format!("{}.{}", int_part, frac_str);

        if is_negative {
            format!("-{}", result)
        } else {
            result
        }
    }

    /// Returns the 8-byte big-endian representation.
    #[inline]
    pub fn to_be_bytes(&self) -> [u8; 8] {
        self.value.to_be_bytes()
    }

    /// Creates a Decimal64NoScale from big-endian bytes.
    #[inline]
    pub fn from_be_bytes(bytes: [u8; 8]) -> Self {
        Self {
            value: i64::from_be_bytes(bytes),
        }
    }

    /// Converts to the variable-length `Decimal` type.
    ///
    /// Note: This requires a scale to format correctly.
    pub fn to_decimal(&self, scale: i32) -> Decimal {
        if self.is_neg_infinity() {
            return Decimal::neg_infinity();
        }
        if self.is_pos_infinity() {
            return Decimal::infinity();
        }
        if self.is_nan() {
            return Decimal::nan();
        }

        Decimal::from_str(&self.to_string_with_scale(scale))
            .expect("Decimal64NoScale string is always valid")
    }

    /// Creates a Decimal64NoScale from a Decimal with the specified scale.
    pub fn from_decimal(decimal: &Decimal, scale: i32) -> Result<Self, DecimalError> {
        if decimal.is_nan() {
            return Ok(Self::nan());
        }
        if decimal.is_pos_infinity() {
            return Ok(Self::infinity());
        }
        if decimal.is_neg_infinity() {
            return Ok(Self::neg_infinity());
        }

        Self::new(&decimal.to_string(), scale)
    }

    /// Returns the minimum finite value.
    #[inline]
    pub const fn min_value() -> Self {
        Self { value: MIN_VALUE }
    }

    /// Returns the maximum finite value.
    #[inline]
    pub const fn max_value() -> Self {
        Self { value: MAX_VALUE }
    }

    // ==================== Comparison with Scale ====================

    /// Compares two values, normalizing scales if different.
    ///
    /// This is needed when comparing values that might have been stored
    /// with different scales in different columns.
    pub fn cmp_with_scale(&self, other: &Self, self_scale: i32, other_scale: i32) -> Ordering {
        // Handle special values
        match (self.is_special(), other.is_special()) {
            (true, true) => {
                // Both special: NaN > Infinity > -Infinity
                if self.is_nan() && other.is_nan() {
                    return Ordering::Equal;
                }
                if self.is_nan() {
                    return Ordering::Greater;
                }
                if other.is_nan() {
                    return Ordering::Less;
                }
                if self.is_pos_infinity() && other.is_pos_infinity() {
                    return Ordering::Equal;
                }
                if self.is_neg_infinity() && other.is_neg_infinity() {
                    return Ordering::Equal;
                }
                if self.is_pos_infinity() {
                    return Ordering::Greater;
                }
                if self.is_neg_infinity() {
                    return Ordering::Less;
                }
                if other.is_pos_infinity() {
                    return Ordering::Less;
                }
                Ordering::Greater // other is -Infinity
            }
            (true, false) => {
                if self.is_neg_infinity() {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, true) => {
                if other.is_neg_infinity() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, false) => {
                // Both normal: normalize to common scale
                if self_scale == other_scale {
                    self.value.cmp(&other.value)
                } else {
                    let max_scale = self_scale.max(other_scale);

                    let self_normalized = if self_scale < max_scale {
                        self.value
                            .saturating_mul(10i64.pow((max_scale - self_scale) as u32))
                    } else {
                        self.value
                    };

                    let other_normalized = if other_scale < max_scale {
                        other
                            .value
                            .saturating_mul(10i64.pow((max_scale - other_scale) as u32))
                    } else {
                        other.value
                    };

                    self_normalized.cmp(&other_normalized)
                }
            }
        }
    }

    // ==================== Internal Helpers ====================

    fn compute_scaled_value(
        int_part: &str,
        frac_part: &str,
        is_negative: bool,
        scale: i32,
    ) -> Result<i64, DecimalError> {
        if scale < 0 {
            // Negative scale: round to powers of 10, store the quotient
            // stored = actual_value / 10^(-scale)
            // e.g., 12345 with scale=-2 → store 123 (rounds to 12300, then /100)
            let round_digits = (-scale) as usize;
            let int_value: i64 = int_part.parse().unwrap_or(0);

            if int_part.len() <= round_digits {
                return Ok(0);
            }

            let divisor = 10i64.pow(round_digits as u32);
            // Round and divide (don't multiply back)
            let rounded = (int_value + divisor / 2) / divisor;
            return Ok(if is_negative { -rounded } else { rounded });
        }

        let scale_u = scale as usize;

        // Parse integer part
        let int_value: i64 = if int_part == "0" || int_part.is_empty() {
            0
        } else {
            int_part.parse().map_err(|_| {
                DecimalError::InvalidFormat(format!("Invalid integer part: {}", int_part))
            })?
        };

        // Apply scale to get scaled integer part
        let scale_factor = 10i64.pow(scale as u32);
        let scaled_int = int_value.checked_mul(scale_factor).ok_or_else(|| {
            DecimalError::InvalidFormat("Value too large for Decimal64NoScale".to_string())
        })?;

        // Parse fractional part (truncate or pad to scale)
        let frac_value: i64 = if frac_part.is_empty() {
            0
        } else if frac_part.len() <= scale_u {
            // Pad with zeros
            let padded = format!("{:0<width$}", frac_part, width = scale_u);
            padded.parse().unwrap_or(0)
        } else {
            // Truncate (with rounding)
            let truncated = &frac_part[..scale_u];
            let next_digit = frac_part.chars().nth(scale_u).unwrap_or('0');
            let mut value: i64 = truncated.parse().unwrap_or(0);
            if next_digit >= '5' {
                value += 1;
            }
            value
        };

        let result = scaled_int + frac_value;
        Ok(if is_negative && result != 0 {
            -result
        } else {
            result
        })
    }
}

// ==================== Trait Implementations ====================

impl PartialOrd for Decimal64NoScale {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal64NoScale {
    /// Compares values assuming same scale.
    ///
    /// For cross-scale comparison, use `cmp_with_scale()`.
    ///
    /// The sentinel values are chosen so that standard i64 comparison gives
    /// PostgreSQL ordering: -Infinity < numbers < +Infinity < NaN
    fn cmp(&self, other: &Self) -> Ordering {
        self.value.cmp(&other.value)
    }
}

impl fmt::Debug for Decimal64NoScale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_nan() {
            write!(f, "Decimal64NoScale(NaN)")
        } else if self.is_pos_infinity() {
            write!(f, "Decimal64NoScale(Infinity)")
        } else if self.is_neg_infinity() {
            write!(f, "Decimal64NoScale(-Infinity)")
        } else {
            f.debug_struct("Decimal64NoScale")
                .field("value", &self.value)
                .finish()
        }
    }
}

impl fmt::Display for Decimal64NoScale {
    /// Display without scale (shows raw value).
    ///
    /// For formatted output, use `to_string_with_scale()`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_nan() {
            write!(f, "NaN")
        } else if self.is_pos_infinity() {
            write!(f, "Infinity")
        } else if self.is_neg_infinity() {
            write!(f, "-Infinity")
        } else {
            write!(f, "{}", self.value)
        }
    }
}

impl From<i64> for Decimal64NoScale {
    fn from(value: i64) -> Self {
        Self { value }
    }
}

impl From<i32> for Decimal64NoScale {
    fn from(value: i32) -> Self {
        Self {
            value: value as i64,
        }
    }
}

// Serde support
impl Serialize for Decimal64NoScale {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i64(self.value)
    }
}

impl<'de> Deserialize<'de> for Decimal64NoScale {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = i64::deserialize(deserializer)?;
        Ok(Self::from_raw(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_basic() {
        let d = Decimal64NoScale::new("123.45", 2).unwrap();
        assert_eq!(d.value(), 12345);
        assert_eq!(d.to_string_with_scale(2), "123.45");

        let d = Decimal64NoScale::new("100", 0).unwrap();
        assert_eq!(d.value(), 100);
        assert_eq!(d.to_string_with_scale(0), "100");

        let d = Decimal64NoScale::new("-50.5", 1).unwrap();
        assert_eq!(d.value(), -505);
        assert_eq!(d.to_string_with_scale(1), "-50.5");
    }

    #[test]
    fn test_18_digit_precision() {
        // This should work with Decimal64NoScale but NOT with Decimal64
        let d = Decimal64NoScale::new("123456789012345678", 0).unwrap();
        assert_eq!(d.value(), 123456789012345678);
        assert_eq!(d.to_string_with_scale(0), "123456789012345678");

        // 18 digits with 2 decimal places
        let d = Decimal64NoScale::new("1234567890123456.78", 2).unwrap();
        assert_eq!(d.value(), 123456789012345678);
        assert_eq!(d.to_string_with_scale(2), "1234567890123456.78");
    }

    #[test]
    fn test_aggregates_work() {
        // This is the key test: aggregates on raw i64 values should work
        let scale = 2;
        let a = Decimal64NoScale::new("100.50", scale).unwrap();
        let b = Decimal64NoScale::new("200.25", scale).unwrap();
        let c = Decimal64NoScale::new("300.75", scale).unwrap();

        // Sum the raw values
        let sum = a.value() + b.value() + c.value();
        assert_eq!(sum, 60150); // 601.50 * 100

        // Interpret with scale - preserves trailing zeros
        let result = Decimal64NoScale::from_raw(sum);
        assert_eq!(result.to_string_with_scale(scale), "601.50");

        // Min/Max - preserves trailing zeros
        let values = [a.value(), b.value(), c.value()];
        let min = *values.iter().min().unwrap();
        let max = *values.iter().max().unwrap();
        assert_eq!(
            Decimal64NoScale::from_raw(min).to_string_with_scale(scale),
            "100.50"
        );
        assert_eq!(
            Decimal64NoScale::from_raw(max).to_string_with_scale(scale),
            "300.75"
        );
    }

    #[test]
    fn test_special_values() {
        let nan = Decimal64NoScale::nan();
        assert!(nan.is_nan());
        assert!(nan.is_special());
        assert_eq!(nan.to_string_with_scale(2), "NaN");

        let inf = Decimal64NoScale::infinity();
        assert!(inf.is_pos_infinity());
        assert_eq!(inf.to_string_with_scale(2), "Infinity");

        let neg_inf = Decimal64NoScale::neg_infinity();
        assert!(neg_inf.is_neg_infinity());
        assert_eq!(neg_inf.to_string_with_scale(2), "-Infinity");
    }

    #[test]
    fn test_ordering() {
        let neg_inf = Decimal64NoScale::neg_infinity();
        let neg = Decimal64NoScale::from_raw(-1000);
        let zero = Decimal64NoScale::from_raw(0);
        let pos = Decimal64NoScale::from_raw(1000);
        let inf = Decimal64NoScale::infinity();
        let nan = Decimal64NoScale::nan();

        assert!(neg_inf < neg);
        assert!(neg < zero);
        assert!(zero < pos);
        assert!(pos < inf);
        assert!(inf < nan);
    }

    #[test]
    fn test_from_str_special() {
        assert!(
            Decimal64NoScale::new("Infinity", 0)
                .unwrap()
                .is_pos_infinity()
        );
        assert!(
            Decimal64NoScale::new("-Infinity", 0)
                .unwrap()
                .is_neg_infinity()
        );
        assert!(Decimal64NoScale::new("NaN", 0).unwrap().is_nan());
    }

    #[test]
    fn test_roundtrip() {
        let scale = 4;
        let values = ["0", "123.4567", "-99.9999", "1000000", "-1"];

        for s in values {
            let d = Decimal64NoScale::new(s, scale).unwrap();
            let raw = d.value();
            let restored = Decimal64NoScale::from_raw(raw);
            assert_eq!(d.value(), restored.value(), "Roundtrip failed for {}", s);
        }
    }

    #[test]
    fn test_byte_roundtrip() {
        let d = Decimal64NoScale::new("123.45", 2).unwrap();
        let bytes = d.to_be_bytes();
        let restored = Decimal64NoScale::from_be_bytes(bytes);
        assert_eq!(d, restored);
    }

    #[test]
    fn test_zero() {
        let d = Decimal64NoScale::new("0", 0).unwrap();
        assert!(d.is_zero());
        assert!(!d.is_negative());
        assert!(!d.is_positive());
        assert!(d.is_finite());
    }

    #[test]
    fn test_negative_scale() {
        // Negative scale: rounds to left of decimal point, no decimal in output
        let d = Decimal64NoScale::new("12345", -2).unwrap();
        assert_eq!(d.to_string_with_scale(-2), "12300");

        // More negative scale cases - should NOT have decimal points
        let d = Decimal64NoScale::from_raw(123);
        assert_eq!(d.to_string_with_scale(-1), "1230"); // 123 * 10
        assert_eq!(d.to_string_with_scale(-2), "12300"); // 123 * 100

        // Zero scale - should NOT have decimal point
        let d = Decimal64NoScale::from_raw(12345);
        assert_eq!(d.to_string_with_scale(0), "12345");

        // Zero value with negative/zero scale - no decimal point
        let zero = Decimal64NoScale::from_raw(0);
        assert_eq!(zero.to_string_with_scale(0), "0");
        assert_eq!(zero.to_string_with_scale(-2), "0");
    }

    #[test]
    fn test_max_precision() {
        // Max value that fits
        let max = Decimal64NoScale::max_value();
        assert!(max.is_finite());
        assert!(max.value() > 0);

        // Min value that fits
        let min = Decimal64NoScale::min_value();
        assert!(min.is_finite());
        assert!(min.value() < 0);
    }

    #[test]
    fn test_from_i64() {
        // Basic scaling - preserves trailing zeros
        let d = Decimal64NoScale::from_i64(123, 2).unwrap();
        assert_eq!(d.value(), 12300);
        assert_eq!(d.to_string_with_scale(2), "123.00");

        // Zero scale
        let d = Decimal64NoScale::from_i64(123, 0).unwrap();
        assert_eq!(d.value(), 123);

        // Negative value
        let d = Decimal64NoScale::from_i64(-50, 2).unwrap();
        assert_eq!(d.value(), -5000);

        // Negative scale (divides)
        let d = Decimal64NoScale::from_i64(12345, -2).unwrap();
        assert_eq!(d.value(), 123);
    }

    #[test]
    fn test_from_u64() {
        let d = Decimal64NoScale::from_u64(123, 2).unwrap();
        assert_eq!(d.value(), 12300);

        // Value too large
        assert!(Decimal64NoScale::from_u64(u64::MAX, 0).is_err());
    }

    #[test]
    fn test_from_f64() {
        // Basic conversion
        let d = Decimal64NoScale::from_f64(123.45, 2).unwrap();
        assert_eq!(d.value(), 12345);
        assert_eq!(d.to_string_with_scale(2), "123.45");

        // Special values
        assert!(Decimal64NoScale::from_f64(f64::NAN, 2).unwrap().is_nan());
        assert!(
            Decimal64NoScale::from_f64(f64::INFINITY, 2)
                .unwrap()
                .is_pos_infinity()
        );
        assert!(
            Decimal64NoScale::from_f64(f64::NEG_INFINITY, 2)
                .unwrap()
                .is_neg_infinity()
        );

        // Negative value
        let d = Decimal64NoScale::from_f64(-99.99, 2).unwrap();
        assert_eq!(d.value(), -9999);
    }
}

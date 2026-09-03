//! # decimal-bytes
//!
//! Arbitrary precision decimals with lexicographically sortable byte encoding.
//!
//! This crate provides three decimal types optimized for database storage:
//!
//! - **[`Decimal`]**: Variable-length arbitrary precision with an exponent
//!   range of -16,383 through 131,072
//! - **[`Decimal64`]**: Fixed 8-byte representation with embedded scale (precision ≤ 16 digits)
//! - **[`Decimal64NoScale`]**: Fixed 8-byte representation with external scale (precision ≤ 18 digits)
//!
//! All types support PostgreSQL special values (NaN, ±Infinity) with correct sort ordering.
//!
//! ## When to Use Which
//!
//! | Type | Precision | Scale / exponent range | Storage | Best For |
//! |------|-----------|-----------------------|---------|----------|
//! | `Decimal64NoScale` | ≤ 18 digits | External scale | 8 bytes | **Columnar storage**, aggregates |
//! | `Decimal64` | ≤ 16 digits | Embedded scale | 8 bytes | Self-contained values |
//! | `Decimal` | Unlimited mantissa | -16,383 through 131,072 | Variable | Scientific, very large numbers |
//!
//! ## Decimal64NoScale (Recommended for Columnar Storage)
//!
//! ```
//! use decimal_bytes::Decimal64NoScale;
//!
//! // Scale is provided externally (e.g., from schema metadata)
//! let scale = 2;
//! let a = Decimal64NoScale::new("100.50", scale).unwrap();
//! let b = Decimal64NoScale::new("200.25", scale).unwrap();
//!
//! // Aggregates work correctly - just sum the raw i64 values!
//! let sum = a.value() + b.value();  // 30075
//! let result = Decimal64NoScale::from_raw(sum);
//! assert_eq!(result.to_string_with_scale(scale), "300.75");
//! ```
//!
//! ## Decimal64 Example
//!
//! ```
//! use decimal_bytes::Decimal64;
//!
//! // Create with embedded scale
//! let price = Decimal64::new("99.99", 2).unwrap();
//! assert_eq!(price.to_string(), "99.99");
//! assert_eq!(price.scale(), 2);  // Scale is embedded!
//!
//! // With precision and scale (PostgreSQL NUMERIC semantics)
//! let d = Decimal64::with_precision_scale("123.456", Some(5), Some(2)).unwrap();
//! assert_eq!(d.to_string(), "123.46"); // Rounded
//!
//! // Parse with automatic scale detection
//! let d: Decimal64 = "123.456".parse().unwrap();
//! assert_eq!(d.scale(), 3);
//!
//! // Special values
//! let inf = Decimal64::infinity();
//! let nan = Decimal64::nan();
//! assert!(price < inf);
//! assert!(inf < nan);
//! ```
//!
//! ## Decimal Example (Arbitrary Precision)
//!
//! ```
//! use decimal_bytes::Decimal;
//! use std::str::FromStr;
//!
//! // Create decimals
//! let a = Decimal::from_str("123.456").unwrap();
//! let b = Decimal::from_str("123.457").unwrap();
//!
//! // Byte comparison matches numerical comparison
//! assert!(a.as_bytes() < b.as_bytes());
//! assert!(a < b);
//!
//! // Special values (PostgreSQL compatible)
//! let inf = Decimal::infinity();
//! let nan = Decimal::nan();
//! assert!(a < inf);
//! assert!(inf < nan);
//! ```
//!
//! ## Sort Order
//!
//! The lexicographic byte order matches PostgreSQL NUMERIC:
//!
//! ```text
//! -Infinity < negative numbers < zero < positive numbers < +Infinity < NaN
//! ```
//!
//! Both `Decimal` and `Decimal64` support this sort order including special values.
//!
//! ## Special Value Semantics (PostgreSQL vs IEEE 754)
//!
//! The `Decimal` type follows **PostgreSQL semantics** for special values:
//!
//! | Behavior | PostgreSQL / decimal-bytes | IEEE 754 float |
//! |----------|---------------------------|----------------|
//! | `NaN == NaN` | `true` | `false` |
//! | `NaN` ordering | Greatest value (> Infinity) | Unordered |
//! | `Infinity == Infinity` | `true` | `true` |
//!
//! ```
//! use decimal_bytes::Decimal;
//!
//! let nan1 = Decimal::nan();
//! let nan2 = Decimal::nan();
//! let inf = Decimal::infinity();
//!
//! // NaN equals itself (PostgreSQL behavior, unlike IEEE 754)
//! assert_eq!(nan1, nan2);
//!
//! // NaN is greater than everything, including Infinity
//! assert!(nan1 > inf);
//! ```
//!
//! This makes `Decimal` suitable for use in indexes, sorting, and deduplication
//! where consistent ordering and equality semantics are required.

mod decimal64;
mod decimal64_no_scale;
mod encoding;

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use decimal64::{Decimal64, MAX_DECIMAL64_PRECISION, MAX_DECIMAL64_SCALE};
pub use decimal64_no_scale::{
    Decimal64NoScale, MAX_DECIMAL64_NO_SCALE_PRECISION, MAX_DECIMAL64_NO_SCALE_SCALE,
};
pub use encoding::DecimalError;
pub use encoding::SpecialValue;
use encoding::{
    ENCODING_NAN, ENCODING_NEG_INFINITY, ENCODING_POS_INFINITY, decode_to_string,
    decode_to_string_with_scale, encode_decimal, encode_decimal_with_constraints,
    encode_special_value,
};
pub use encoding::{decode_parts_into, decode_special_value};

/// An arbitrary precision decimal number stored as sortable bytes.
///
/// The internal byte representation is designed to be lexicographically sortable,
/// meaning that comparing the bytes directly yields the same result as comparing
/// the numerical values. This enables efficient range queries in databases.
///
/// # Storage Efficiency
///
/// The encoding uses:
/// - 1 byte for the sign
/// - 2 bytes for an inline exponent, or 6 bytes for an escaped exponent
/// - 4 bits per decimal digit (BCD encoding, 2 digits per byte)
#[derive(Clone)]
pub struct Decimal {
    bytes: Vec<u8>,
}

impl Decimal {
    /// Creates a new Decimal with precision and scale constraints.
    ///
    /// Values that exceed the constraints are truncated/rounded to fit.
    /// This is compatible with SQL NUMERIC(precision, scale) semantics.
    ///
    /// - `precision`: Maximum total number of significant digits (None = unlimited)
    /// - `scale`: Digits after decimal point; negative values round to left of decimal
    ///
    /// # PostgreSQL Compatibility
    ///
    /// Supports negative scale (rounds to powers of 10):
    /// - `scale = -3` rounds to nearest 1000
    /// - `NUMERIC(2, -3)` allows values like -99000 to 99000
    ///
    /// # Examples
    ///
    /// ```
    /// use decimal_bytes::Decimal;
    ///
    /// // NUMERIC(5, 2) - up to 5 digits total, 2 after decimal
    /// let d = Decimal::with_precision_scale("123.456", Some(5), Some(2)).unwrap();
    /// assert_eq!(d.to_string(), "123.46"); // Rounded to 2 decimal places
    ///
    /// // NUMERIC(2, -3) - rounds to nearest 1000, max 2 significant digits
    /// let d = Decimal::with_precision_scale("12345", Some(2), Some(-3)).unwrap();
    /// assert_eq!(d.to_string(), "12000"); // Rounded to nearest 1000
    /// ```
    pub fn with_precision_scale(
        s: &str,
        precision: Option<u32>,
        scale: Option<i32>,
    ) -> Result<Self, DecimalError> {
        let bytes = encode_decimal_with_constraints(s, precision, scale)?;
        Ok(Self { bytes })
    }

    /// Creates a Decimal from raw bytes.
    ///
    /// The bytes must be a valid encoding produced by `as_bytes()`.
    /// Returns an error if the bytes are invalid.
    ///
    /// # Examples
    ///
    /// ```
    /// use decimal_bytes::Decimal;
    /// use std::str::FromStr;
    ///
    /// let original = Decimal::from_str("123.456").unwrap();
    /// let bytes = original.as_bytes();
    /// let restored = Decimal::from_bytes(bytes).unwrap();
    /// assert_eq!(original, restored);
    /// ```
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecimalError> {
        // Validate by attempting to decode
        let _ = decode_to_string(bytes)?;
        // A negative mantissa in the terminator-less layout is accepted but
        // rewritten, so that `Ord` and `Eq` stay byte comparisons.
        Ok(Self {
            bytes: encoding::canonicalize(bytes)?,
        })
    }

    /// Creates a Decimal from raw bytes without validation.
    ///
    /// # Safety
    ///
    /// The caller must ensure the bytes are a valid encoding.
    /// Using invalid bytes may cause panics or incorrect results.
    #[inline]
    pub fn from_bytes_unchecked(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Returns the raw byte representation.
    ///
    /// These bytes are lexicographically sortable - comparing them directly
    /// yields the same result as comparing the numerical values.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the Decimal and returns the underlying bytes.
    #[inline]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns this value in the layout that stored negative mantissas as
    /// bitwise-inverted BCD without a terminator. Non-negative and special values
    /// are identical in both layouts.
    ///
    /// That layout does not order negative numbers correctly and does not sort
    /// together with [`as_bytes`](Self::as_bytes), so this is only for writing
    /// into a store that already holds values in it and has not been rebuilt.
    /// [`from_bytes`](Self::from_bytes) reads either layout.
    ///
    /// # Example
    ///
    /// ```
    /// use decimal_bytes::Decimal;
    /// use std::str::FromStr;
    ///
    /// let d = Decimal::from_str("-1").unwrap();
    /// assert_eq!(d.to_legacy_bytes(), vec![0x00, 0xBF, 0xFE, 0xEF]);
    /// assert_eq!(Decimal::from_bytes(&d.to_legacy_bytes()).unwrap(), d);
    /// ```
    pub fn to_legacy_bytes(&self) -> Vec<u8> {
        encoding::to_legacy_encoding(&self.bytes).expect("Decimal contains valid bytes")
    }

    /// Returns true if this decimal represents zero.
    pub fn is_zero(&self) -> bool {
        self.bytes.len() == 1 && self.bytes[0] == encoding::SIGN_ZERO
    }

    /// Returns true if this decimal is negative.
    pub fn is_negative(&self) -> bool {
        !self.bytes.is_empty() && self.bytes[0] == encoding::SIGN_NEGATIVE
    }

    /// Returns true if this decimal is positive (and not zero).
    pub fn is_positive(&self) -> bool {
        !self.bytes.is_empty() && self.bytes[0] == encoding::SIGN_POSITIVE
    }

    /// Returns true if this decimal represents positive infinity.
    pub fn is_pos_infinity(&self) -> bool {
        self.bytes.as_slice() == ENCODING_POS_INFINITY
    }

    /// Returns true if this decimal represents negative infinity.
    pub fn is_neg_infinity(&self) -> bool {
        self.bytes.as_slice() == ENCODING_NEG_INFINITY
    }

    /// Returns true if this decimal represents positive or negative infinity.
    pub fn is_infinity(&self) -> bool {
        self.is_pos_infinity() || self.is_neg_infinity()
    }

    /// Returns true if this decimal represents NaN (Not a Number).
    pub fn is_nan(&self) -> bool {
        self.bytes.as_slice() == ENCODING_NAN
    }

    /// Returns true if this decimal is a special value (Infinity or NaN).
    pub fn is_special(&self) -> bool {
        decode_special_value(&self.bytes).is_some()
    }

    /// Returns true if this decimal is a finite number (not Infinity or NaN).
    pub fn is_finite(&self) -> bool {
        !self.is_special()
    }

    /// Returns the number of bytes used to store this decimal.
    #[inline]
    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    /// Creates positive infinity.
    ///
    /// Infinity is greater than all finite numbers but less than NaN.
    /// Two positive infinities are equal to each other.
    ///
    /// # Example
    ///
    /// ```
    /// use decimal_bytes::Decimal;
    /// use std::str::FromStr;
    ///
    /// let inf = Decimal::infinity();
    /// let big = Decimal::from_str("999999999999").unwrap();
    /// assert!(inf > big);
    /// assert_eq!(inf, Decimal::infinity());
    /// ```
    pub fn infinity() -> Self {
        Self {
            bytes: encode_special_value(SpecialValue::Infinity),
        }
    }

    /// Creates negative infinity.
    ///
    /// Negative infinity is less than all finite numbers and positive infinity.
    /// Two negative infinities are equal to each other.
    ///
    /// # Example
    ///
    /// ```
    /// use decimal_bytes::Decimal;
    /// use std::str::FromStr;
    ///
    /// let neg_inf = Decimal::neg_infinity();
    /// let small = Decimal::from_str("-999999999999").unwrap();
    /// assert!(neg_inf < small);
    /// assert_eq!(neg_inf, Decimal::neg_infinity());
    /// ```
    pub fn neg_infinity() -> Self {
        Self {
            bytes: encode_special_value(SpecialValue::NegInfinity),
        }
    }

    /// Creates NaN (Not a Number).
    ///
    /// # PostgreSQL Semantics
    ///
    /// Unlike IEEE 754 floating-point where `NaN != NaN`, this follows PostgreSQL
    /// semantics where:
    /// - `NaN == NaN` is `true`
    /// - `NaN` is the greatest value (greater than positive infinity)
    /// - All NaN values are equal regardless of how they were created
    ///
    /// This makes NaN usable in sorting, indexing, and deduplication.
    ///
    /// # Example
    ///
    /// ```
    /// use decimal_bytes::Decimal;
    /// use std::str::FromStr;
    ///
    /// let nan1 = Decimal::nan();
    /// let nan2 = Decimal::from_str("NaN").unwrap();
    /// let inf = Decimal::infinity();
    ///
    /// // NaN equals itself (PostgreSQL behavior)
    /// assert_eq!(nan1, nan2);
    ///
    /// // NaN is greater than everything
    /// assert!(nan1 > inf);
    /// ```
    pub fn nan() -> Self {
        Self {
            bytes: encode_special_value(SpecialValue::NaN),
        }
    }

    /// Converts this decimal to a string with a specific scale (number of decimal places).
    ///
    /// This ensures the output has exactly `scale` decimal places, adding trailing
    /// zeros if needed. Useful for PostgreSQL NUMERIC display formatting where
    /// the scale defines the display format.
    ///
    /// - Positive scale: adds decimal point with trailing zeros as needed
    /// - Zero scale: no decimal point added
    /// - Negative scale: no decimal point (value is already an integer)
    ///
    /// Special values (NaN, Infinity, -Infinity) are returned as-is without scale formatting.
    ///
    /// # Arguments
    /// * `scale` - Number of decimal places to ensure in the output
    ///
    /// # Examples
    ///
    /// ```
    /// use decimal_bytes::Decimal;
    /// use std::str::FromStr;
    ///
    /// // Positive scale: includes decimal point and trailing zeros
    /// let d = Decimal::from_str("1").unwrap();
    /// assert_eq!(d.to_string_with_scale(18), "1.000000000000000000");
    ///
    /// let d = Decimal::from_str("1.5").unwrap();
    /// assert_eq!(d.to_string_with_scale(3), "1.500");
    ///
    /// // Zero scale: no decimal point
    /// let d = Decimal::from_str("123").unwrap();
    /// assert_eq!(d.to_string_with_scale(0), "123");
    ///
    /// // Negative scale: no decimal point
    /// let d = Decimal::from_str("100").unwrap();
    /// assert_eq!(d.to_string_with_scale(-2), "100");
    ///
    /// // Special values are unchanged
    /// let nan = Decimal::nan();
    /// assert_eq!(nan.to_string_with_scale(10), "NaN");
    /// ```
    pub fn to_string_with_scale(&self, scale: i32) -> String {
        decode_to_string_with_scale(&self.bytes, scale).expect("Decimal contains valid bytes")
    }

    /// Decomposes a finite value for arithmetic without a string round-trip:
    /// `value = sign * 0.digits * 10^exponent`, digit values 0-9, most
    /// significant first, trailing zeros stripped. Clears and refills
    /// `digits` so a caller can reuse one buffer across many decodes.
    ///
    /// Returns `None` for NaN and +/-Infinity. Zero decodes to an empty
    /// digit buffer with exponent 0.
    ///
    /// # Example
    /// ```
    /// use decimal_bytes::Decimal;
    /// use std::str::FromStr;
    ///
    /// let d = Decimal::from_str("-12.5").unwrap();
    /// let mut digits = Vec::new();
    /// let (negative, exponent) = d.read_parts_into(&mut digits).unwrap();
    /// assert!(negative);
    /// assert_eq!(exponent, 2); // -0.125 * 10^2
    /// assert_eq!(digits, vec![1, 2, 5]);
    ///
    /// assert_eq!(Decimal::nan().read_parts_into(&mut digits), None);
    /// ```
    pub fn read_parts_into(&self, digits: &mut Vec<u8>) -> Option<(bool, i32)> {
        decode_parts_into(&self.bytes, digits).expect("Decimal contains valid bytes")
    }
}

impl FromStr for Decimal {
    type Err = DecimalError;

    /// Creates a new Decimal from a string representation.
    ///
    /// # Examples
    ///
    /// ```
    /// use decimal_bytes::Decimal;
    /// use std::str::FromStr;
    ///
    /// let d = Decimal::from_str("123.456").unwrap();
    /// let d = Decimal::from_str("-0.001").unwrap();
    /// let d = Decimal::from_str("1e10").unwrap();
    /// // Or use parse:
    /// let d: Decimal = "42.5".parse().unwrap();
    /// ```
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = encode_decimal(s)?;
        Ok(Self { bytes })
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = decode_to_string(&self.bytes).expect("Decimal contains valid bytes");
        write!(f, "{}", s)
    }
}

impl fmt::Debug for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = decode_to_string(&self.bytes).expect("Decimal contains valid bytes");
        f.debug_struct("Decimal")
            .field("value", &value)
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for Decimal {}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        // Byte comparison is equivalent to numerical comparison
        self.bytes.cmp(&other.bytes)
    }
}

impl Hash for Decimal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}

impl Serialize for Decimal {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Serialize as string for human readability in JSON
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Decimal::from_str(&s).map_err(serde::de::Error::custom)
    }
}

// Conversion from integer types
macro_rules! impl_from_int {
    ($($t:ty),*) => {
        $(
            impl From<$t> for Decimal {
                fn from(val: $t) -> Self {
                    // Integer conversion is infallible - always produces valid decimal bytes.
                    // Uses string conversion internally since the byte encoding is designed
                    // around decimal string parsing (BCD encoding of digit pairs).
                    Decimal::from_str(&val.to_string()).expect("Integer is always valid")
                }
            }
        )*
    };
}

impl_from_int!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);

impl TryFrom<f64> for Decimal {
    type Error = DecimalError;

    /// Converts an f64 to a Decimal.
    ///
    /// Special values are handled:
    /// - `f64::NAN` → `Decimal::nan()`
    /// - `f64::INFINITY` → `Decimal::infinity()`
    /// - `f64::NEG_INFINITY` → `Decimal::neg_infinity()`
    ///
    /// Note: Due to f64's limited precision (~15-17 significant digits),
    /// very precise decimal values may lose precision when converted from f64.
    /// For exact decimal representation, use `Decimal::from_str()` instead.
    ///
    /// # Example
    ///
    /// ```
    /// use decimal_bytes::Decimal;
    ///
    /// let d: Decimal = 123.456f64.try_into().unwrap();
    /// assert_eq!(d.to_string(), "123.456");
    ///
    /// let inf: Decimal = f64::INFINITY.try_into().unwrap();
    /// assert!(inf.is_pos_infinity());
    ///
    /// let nan: Decimal = f64::NAN.try_into().unwrap();
    /// assert!(nan.is_nan());
    /// ```
    fn try_from(val: f64) -> Result<Self, Self::Error> {
        if val.is_nan() {
            return Ok(Decimal::nan());
        }
        if val.is_infinite() {
            return Ok(if val.is_sign_positive() {
                Decimal::infinity()
            } else {
                Decimal::neg_infinity()
            });
        }
        Decimal::from_str(&val.to_string())
    }
}

impl Default for Decimal {
    fn default() -> Self {
        Decimal {
            bytes: vec![encoding::SIGN_ZERO],
        }
    }
}

// ============================================================================
// Optional: rust_decimal interop (enabled with "rust_decimal" feature)
// ============================================================================

#[cfg(feature = "rust_decimal")]
mod rust_decimal_interop {
    use super::{Decimal, DecimalError};
    use std::str::FromStr;

    impl TryFrom<rust_decimal::Decimal> for Decimal {
        type Error = DecimalError;

        /// Converts a `rust_decimal::Decimal` to a `decimal_bytes::Decimal`.
        ///
        /// # Example
        ///
        /// ```ignore
        /// use rust_decimal::Decimal as RustDecimal;
        /// use decimal_bytes::Decimal;
        ///
        /// let rd = RustDecimal::new(12345, 2); // 123.45
        /// let d: Decimal = rd.try_into().unwrap();
        /// assert_eq!(d.to_string(), "123.45");
        /// ```
        fn try_from(value: rust_decimal::Decimal) -> Result<Self, Self::Error> {
            Decimal::from_str(&value.to_string())
        }
    }

    impl TryFrom<&Decimal> for rust_decimal::Decimal {
        type Error = rust_decimal::Error;

        /// Converts a `decimal_bytes::Decimal` to a `rust_decimal::Decimal`.
        ///
        /// Note: This may fail if the decimal exceeds rust_decimal's precision limits
        /// (28-29 significant digits) or if it's a special value (Infinity, NaN).
        ///
        /// # Example
        ///
        /// ```ignore
        /// use rust_decimal::Decimal as RustDecimal;
        /// use decimal_bytes::Decimal;
        ///
        /// let d = Decimal::from_str("123.45").unwrap();
        /// let rd: RustDecimal = (&d).try_into().unwrap();
        /// assert_eq!(rd.to_string(), "123.45");
        /// ```
        fn try_from(value: &Decimal) -> Result<Self, Self::Error> {
            use std::str::FromStr;
            rust_decimal::Decimal::from_str(&value.to_string())
        }
    }

    impl TryFrom<Decimal> for rust_decimal::Decimal {
        type Error = rust_decimal::Error;

        fn try_from(value: Decimal) -> Result<Self, Self::Error> {
            rust_decimal::Decimal::try_from(&value)
        }
    }
}

// The rust_decimal_interop module only contains trait implementations,
// so there's nothing to re-export. The TryFrom impls are automatically available.

// ============================================================================
// Optional: bigdecimal interop (enabled with "bigdecimal" feature)
// ============================================================================

#[cfg(feature = "bigdecimal")]
mod bigdecimal_interop {
    use super::{Decimal, DecimalError};
    use std::str::FromStr;

    impl TryFrom<bigdecimal::BigDecimal> for Decimal {
        type Error = DecimalError;

        /// Converts a `bigdecimal::BigDecimal` to a `decimal_bytes::Decimal`.
        fn try_from(value: bigdecimal::BigDecimal) -> Result<Self, Self::Error> {
            Decimal::from_str(&value.to_string())
        }
    }

    impl TryFrom<&bigdecimal::BigDecimal> for Decimal {
        type Error = DecimalError;

        fn try_from(value: &bigdecimal::BigDecimal) -> Result<Self, Self::Error> {
            Decimal::from_str(&value.to_string())
        }
    }

    impl TryFrom<&Decimal> for bigdecimal::BigDecimal {
        type Error = bigdecimal::ParseBigDecimalError;

        /// Converts a `decimal_bytes::Decimal` to a `bigdecimal::BigDecimal`.
        ///
        /// Note: This may fail for special values (Infinity, NaN) which
        /// bigdecimal doesn't support.
        fn try_from(value: &Decimal) -> Result<Self, Self::Error> {
            use std::str::FromStr;
            bigdecimal::BigDecimal::from_str(&value.to_string())
        }
    }

    impl TryFrom<Decimal> for bigdecimal::BigDecimal {
        type Error = bigdecimal::ParseBigDecimalError;

        fn try_from(value: Decimal) -> Result<Self, Self::Error> {
            bigdecimal::BigDecimal::try_from(&value)
        }
    }
}

// The bigdecimal_interop module only contains trait implementations,
// so there's nothing to re-export. The TryFrom impls are automatically available.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_str() {
        let d = Decimal::from_str("123.456").unwrap();
        assert_eq!(d.to_string(), "123.456");
    }

    #[test]
    fn test_zero() {
        let d = Decimal::from_str("0").unwrap();
        assert!(d.is_zero());
        assert!(!d.is_negative());
        assert!(!d.is_positive());
        assert!(d.is_finite());
        assert!(!d.is_special());
    }

    #[test]
    fn test_negative() {
        let d = Decimal::from_str("-123.456").unwrap();
        assert!(d.is_negative());
        assert!(!d.is_zero());
        assert!(!d.is_positive());
        assert!(d.is_finite());
    }

    #[test]
    fn test_positive() {
        let d = Decimal::from_str("123.456").unwrap();
        assert!(d.is_positive());
        assert!(!d.is_zero());
        assert!(!d.is_negative());
        assert!(d.is_finite());
    }

    #[test]
    fn test_ordering() {
        let values = vec!["-100", "-10", "-1", "-0.1", "0", "0.1", "1", "10", "100"];
        let decimals: Vec<Decimal> = values
            .iter()
            .map(|s| Decimal::from_str(s).unwrap())
            .collect();

        // Check that ordering is correct
        for i in 0..decimals.len() - 1 {
            assert!(
                decimals[i] < decimals[i + 1],
                "{} should be < {}",
                values[i],
                values[i + 1]
            );
        }

        // Check that byte ordering matches
        for i in 0..decimals.len() - 1 {
            assert!(
                decimals[i].as_bytes() < decimals[i + 1].as_bytes(),
                "bytes of {} should be < bytes of {}",
                values[i],
                values[i + 1]
            );
        }
    }

    #[test]
    fn test_roundtrip() {
        let values = vec![
            "0", "1", "-1", "123.456", "-123.456", "0.001", "0.1", "10", "100", "1000000",
            "-1000000",
        ];

        for s in values {
            let d = Decimal::from_str(s).unwrap();
            let bytes = d.as_bytes();
            let restored = Decimal::from_bytes(bytes).unwrap();
            assert_eq!(d, restored, "Roundtrip failed for {}", s);
        }
    }

    #[test]
    fn test_precision_scale() {
        // Round to 2 decimal places
        let d = Decimal::with_precision_scale("123.456", Some(10), Some(2)).unwrap();
        assert_eq!(d.to_string(), "123.46");

        // When precision is exceeded, least significant integer digits are kept
        let d = Decimal::with_precision_scale("12345.67", Some(5), Some(2)).unwrap();
        assert_eq!(d.to_string(), "345.67"); // 5 digits total, 2 after decimal = 3 integer digits max

        // Rounding within precision limits
        let d = Decimal::with_precision_scale("99.999", Some(5), Some(2)).unwrap();
        assert_eq!(d.to_string(), "100"); // Rounds up, fits in precision
    }

    #[test]
    fn test_from_integer() {
        let d = Decimal::from(42i64);
        assert_eq!(d.to_string(), "42");

        let d = Decimal::from(-100i32);
        assert_eq!(d.to_string(), "-100");
    }

    #[test]
    fn test_from_f64() {
        // Normal f64 values
        let d: Decimal = 123.456f64.try_into().unwrap();
        assert_eq!(d.to_string(), "123.456");

        let d: Decimal = (-99.5f64).try_into().unwrap();
        assert_eq!(d.to_string(), "-99.5");

        let d: Decimal = 0.0f64.try_into().unwrap();
        assert!(d.is_zero());

        // Special values
        let inf: Decimal = f64::INFINITY.try_into().unwrap();
        assert!(inf.is_pos_infinity());

        let neg_inf: Decimal = f64::NEG_INFINITY.try_into().unwrap();
        assert!(neg_inf.is_neg_infinity());

        let nan: Decimal = f64::NAN.try_into().unwrap();
        assert!(nan.is_nan());
    }

    #[test]
    fn test_serialization() {
        let d = Decimal::from_str("123.456").unwrap();
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "\"123.456\"");

        let restored: Decimal = serde_json::from_str(&json).unwrap();
        assert_eq!(d, restored);
    }

    #[test]
    fn test_byte_efficiency() {
        // Check that storage is reasonably efficient
        let d = Decimal::from_str("123456789").unwrap();
        // 1 byte sign + ~2 bytes exponent + ~5 bytes mantissa (9 digits / 2)
        assert!(
            d.byte_len() <= 10,
            "Expected <= 10 bytes, got {}",
            d.byte_len()
        );

        let d = Decimal::from_str("0.000001").unwrap();
        // Should be compact for small numbers too
        assert!(
            d.byte_len() <= 6,
            "Expected <= 6 bytes, got {}",
            d.byte_len()
        );
    }

    // ==================== Special Values Tests ====================

    #[test]
    fn test_infinity_creation() {
        let pos_inf = Decimal::infinity();
        assert!(pos_inf.is_pos_infinity());
        assert!(pos_inf.is_infinity());
        assert!(!pos_inf.is_neg_infinity());
        assert!(!pos_inf.is_nan());
        assert!(pos_inf.is_special());
        assert!(!pos_inf.is_finite());
        assert_eq!(pos_inf.to_string(), "Infinity");

        let neg_inf = Decimal::neg_infinity();
        assert!(neg_inf.is_neg_infinity());
        assert!(neg_inf.is_infinity());
        assert!(!neg_inf.is_pos_infinity());
        assert!(!neg_inf.is_nan());
        assert!(neg_inf.is_special());
        assert!(!neg_inf.is_finite());
        assert_eq!(neg_inf.to_string(), "-Infinity");
    }

    #[test]
    fn test_nan_creation() {
        let nan = Decimal::nan();
        assert!(nan.is_nan());
        assert!(nan.is_special());
        assert!(!nan.is_finite());
        assert!(!nan.is_infinity());
        assert!(!nan.is_zero());
        assert_eq!(nan.to_string(), "NaN");
    }

    #[test]
    fn test_special_value_from_str() {
        let pos_inf = Decimal::from_str("Infinity").unwrap();
        assert!(pos_inf.is_pos_infinity());

        let neg_inf = Decimal::from_str("-Infinity").unwrap();
        assert!(neg_inf.is_neg_infinity());

        let nan = Decimal::from_str("NaN").unwrap();
        assert!(nan.is_nan());

        // Case-insensitive
        let inf = Decimal::from_str("infinity").unwrap();
        assert!(inf.is_pos_infinity());

        let inf = Decimal::from_str("INF").unwrap();
        assert!(inf.is_pos_infinity());
    }

    #[test]
    fn test_special_value_ordering() {
        // PostgreSQL order: -Infinity < negatives < zero < positives < Infinity < NaN
        let neg_inf = Decimal::neg_infinity();
        let neg_num = Decimal::from_str("-1000").unwrap();
        let zero = Decimal::from_str("0").unwrap();
        let pos_num = Decimal::from_str("1000").unwrap();
        let pos_inf = Decimal::infinity();
        let nan = Decimal::nan();

        assert!(neg_inf < neg_num);
        assert!(neg_num < zero);
        assert!(zero < pos_num);
        assert!(pos_num < pos_inf);
        assert!(pos_inf < nan);

        // Verify byte ordering matches
        assert!(neg_inf.as_bytes() < neg_num.as_bytes());
        assert!(neg_num.as_bytes() < zero.as_bytes());
        assert!(zero.as_bytes() < pos_num.as_bytes());
        assert!(pos_num.as_bytes() < pos_inf.as_bytes());
        assert!(pos_inf.as_bytes() < nan.as_bytes());
    }

    #[test]
    fn test_special_value_equality() {
        // All NaNs are equal (PostgreSQL semantics)
        let nan1 = Decimal::from_str("NaN").unwrap();
        let nan2 = Decimal::from_str("nan").unwrap();
        let nan3 = Decimal::nan();
        assert_eq!(nan1, nan2);
        assert_eq!(nan2, nan3);

        // Infinities are equal to themselves
        let inf1 = Decimal::infinity();
        let inf2 = Decimal::from_str("Infinity").unwrap();
        assert_eq!(inf1, inf2);

        let neg_inf1 = Decimal::neg_infinity();
        let neg_inf2 = Decimal::from_str("-Infinity").unwrap();
        assert_eq!(neg_inf1, neg_inf2);
    }

    #[test]
    fn test_special_value_serialization() {
        let inf = Decimal::infinity();
        let json = serde_json::to_string(&inf).unwrap();
        assert_eq!(json, "\"Infinity\"");
        let restored: Decimal = serde_json::from_str(&json).unwrap();
        assert_eq!(inf, restored);

        let nan = Decimal::nan();
        let json = serde_json::to_string(&nan).unwrap();
        assert_eq!(json, "\"NaN\"");
        let restored: Decimal = serde_json::from_str(&json).unwrap();
        assert_eq!(nan, restored);
    }

    #[test]
    fn test_special_value_byte_efficiency() {
        // Special values should be compact (3 bytes each)
        assert_eq!(Decimal::infinity().byte_len(), 3);
        assert_eq!(Decimal::neg_infinity().byte_len(), 3);
        assert_eq!(Decimal::nan().byte_len(), 3);
    }

    // ==================== Negative Scale Tests ====================

    #[test]
    fn test_negative_scale() {
        // Round to nearest 1000
        let d = Decimal::with_precision_scale("12345", Some(10), Some(-3)).unwrap();
        assert_eq!(d.to_string(), "12000");

        // Round up
        let d = Decimal::with_precision_scale("12500", Some(10), Some(-3)).unwrap();
        assert_eq!(d.to_string(), "13000");

        // Round to nearest 100
        let d = Decimal::with_precision_scale("1234", Some(10), Some(-2)).unwrap();
        assert_eq!(d.to_string(), "1200");
    }

    #[test]
    fn test_negative_scale_with_precision() {
        // NUMERIC(2, -3): 2 significant digits, round to nearest 1000
        let d = Decimal::with_precision_scale("12345", Some(2), Some(-3)).unwrap();
        assert_eq!(d.to_string(), "12000");
    }

    // ==================== Error Handling Tests ====================

    #[test]
    fn test_invalid_format_errors() {
        // Multiple decimal points
        let result = Decimal::from_str("1.2.3");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DecimalError::InvalidFormat(_)
        ));

        // Invalid characters
        let result = Decimal::from_str("12abc");
        assert!(result.is_err());

        // Missing exponent after 'e'
        let result = Decimal::from_str("1e");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DecimalError::InvalidFormat(_)
        ));

        // Invalid exponent (non-numeric after 'e')
        let result = Decimal::from_str("1eabc");
        assert!(result.is_err());

        // Empty string gives zero
        let d = Decimal::from_str("").unwrap();
        assert!(d.is_zero());

        // Just a sign with no digits
        let result = Decimal::from_str("-");
        assert!(result.is_ok()); // Parses as zero
    }

    #[test]
    fn test_leading_plus_sign() {
        let d = Decimal::from_str("+123.456").unwrap();
        assert_eq!(d.to_string(), "123.456");
        assert!(d.is_positive());
    }

    #[test]
    fn test_scientific_notation() {
        let d = Decimal::from_str("1.5e10").unwrap();
        assert_eq!(d.to_string(), "15000000000");

        let d = Decimal::from_str("1.5E-3").unwrap();
        assert_eq!(d.to_string(), "0.0015");

        let d = Decimal::from_str("1e+5").unwrap();
        assert_eq!(d.to_string(), "100000");
    }

    #[test]
    fn test_leading_decimal_point() {
        // ".5" should be parsed as "0.5"
        let d = Decimal::from_str(".5").unwrap();
        assert_eq!(d.to_string(), "0.5");

        let d = Decimal::from_str("-.25").unwrap();
        assert_eq!(d.to_string(), "-0.25");
    }

    #[test]
    fn test_trailing_zeros() {
        let d = Decimal::from_str("100").unwrap();
        assert_eq!(d.to_string(), "100");

        let d = Decimal::from_str("1.500").unwrap();
        assert_eq!(d.to_string(), "1.5");
    }

    #[test]
    fn test_leading_zeros() {
        let d = Decimal::from_str("007").unwrap();
        assert_eq!(d.to_string(), "7");

        let d = Decimal::from_str("00.123").unwrap();
        assert_eq!(d.to_string(), "0.123");
    }

    // ==================== Additional Trait Tests ====================

    #[test]
    fn test_into_bytes() {
        let d = Decimal::from_str("123.456").unwrap();
        let bytes_ref = d.as_bytes().to_vec();
        let bytes_owned = d.into_bytes();
        assert_eq!(bytes_ref, bytes_owned);
    }

    #[test]
    fn test_clone() {
        let d1 = Decimal::from_str("123.456").unwrap();
        let d2 = d1.clone();
        assert_eq!(d1, d2);
        assert_eq!(d1.as_bytes(), d2.as_bytes());
    }

    #[test]
    fn test_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(Decimal::from_str("123.456").unwrap());
        set.insert(Decimal::from_str("123.456").unwrap()); // Duplicate
        set.insert(Decimal::from_str("789.012").unwrap());

        assert_eq!(set.len(), 2);
        assert!(set.contains(&Decimal::from_str("123.456").unwrap()));
    }

    #[test]
    fn test_debug_format() {
        let d = Decimal::from_str("123.456").unwrap();
        let debug_str = format!("{:?}", d);
        assert!(debug_str.contains("Decimal"));
        assert!(debug_str.contains("123.456"));
    }

    #[test]
    fn test_ord_trait() {
        use std::cmp::Ordering;

        let a = Decimal::from_str("1").unwrap();
        let b = Decimal::from_str("2").unwrap();
        let c = Decimal::from_str("1").unwrap();

        assert_eq!(a.cmp(&b), Ordering::Less);
        assert_eq!(b.cmp(&a), Ordering::Greater);
        assert_eq!(a.cmp(&c), Ordering::Equal);
    }

    #[test]
    fn test_from_bytes_invalid() {
        // Empty bytes should fail
        let result = Decimal::from_bytes(&[]);
        assert!(result.is_err());

        // Single invalid sign byte
        let result = Decimal::from_bytes(&[0x00]);
        assert!(result.is_err());
    }

    #[test]
    fn test_deserialize_from_string_number() {
        // JSON string numbers deserialize correctly
        let d: Decimal = serde_json::from_str("\"42\"").unwrap();
        assert_eq!(d.to_string(), "42");

        let d: Decimal = serde_json::from_str("\"-100\"").unwrap();
        assert_eq!(d.to_string(), "-100");

        let d: Decimal = serde_json::from_str("\"1.5e10\"").unwrap();
        assert_eq!(d.to_string(), "15000000000");
    }

    #[test]
    fn test_from_various_integer_types() {
        assert_eq!(Decimal::from(0i32).to_string(), "0");
        assert_eq!(Decimal::from(i32::MAX).to_string(), "2147483647");
        assert_eq!(Decimal::from(i32::MIN).to_string(), "-2147483648");
        assert_eq!(Decimal::from(i64::MAX).to_string(), "9223372036854775807");
        assert_eq!(Decimal::from(i64::MIN).to_string(), "-9223372036854775808");
    }

    #[test]
    fn test_precision_overflow() {
        let result = Decimal::from_str("1e200000");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DecimalError::PrecisionOverflow
        ));

        // Exponent too small (negative)
        let result = Decimal::from_str("1e-20000");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DecimalError::PrecisionOverflow
        ));
    }

    #[test]
    fn test_all_zeros_variations() {
        let d = Decimal::from_str("0").unwrap();
        assert!(d.is_zero());

        let d = Decimal::from_str("0.0").unwrap();
        assert!(d.is_zero());

        let d = Decimal::from_str("00.00").unwrap();
        assert!(d.is_zero());

        let d = Decimal::from_str("-0").unwrap();
        assert!(d.is_zero());
        // Note: -0 normalizes to 0
    }

    // ==================== Additional Edge Case Tests ====================

    #[test]
    fn test_rounding_all_nines() {
        // Rounding 99.999 with scale 2 should become 100.00 -> 100
        let d = Decimal::with_precision_scale("99.999", Some(10), Some(2)).unwrap();
        assert_eq!(d.to_string(), "100");

        // Rounding 9.99 with scale 1 should become 10.0 -> 10
        let d = Decimal::with_precision_scale("9.99", Some(10), Some(1)).unwrap();
        assert_eq!(d.to_string(), "10");

        // 999 rounding to nearest 10 (scale -1) should become 1000
        let d = Decimal::with_precision_scale("999", Some(10), Some(-1)).unwrap();
        assert_eq!(d.to_string(), "1000");
    }

    #[test]
    fn test_negative_scale_small_number() {
        // Number smaller than rounding unit, rounds to 0
        let d = Decimal::with_precision_scale("4", Some(10), Some(-1)).unwrap();
        assert_eq!(d.to_string(), "0");

        // Number >= half the rounding unit, rounds up
        let d = Decimal::with_precision_scale("5", Some(10), Some(-1)).unwrap();
        assert_eq!(d.to_string(), "10");

        // Negative number smaller than rounding unit
        let d = Decimal::with_precision_scale("-4", Some(10), Some(-1)).unwrap();
        assert_eq!(d.to_string(), "0");

        // Negative number >= half unit
        let d = Decimal::with_precision_scale("-5", Some(10), Some(-1)).unwrap();
        assert_eq!(d.to_string(), "-10");
    }

    #[test]
    fn test_precision_truncation() {
        // When precision is exceeded, truncate from left
        let d = Decimal::with_precision_scale("123456", Some(3), Some(0)).unwrap();
        assert_eq!(d.to_string(), "456");

        // With decimal places
        let d = Decimal::with_precision_scale("12345.67", Some(4), Some(2)).unwrap();
        assert_eq!(d.to_string(), "45.67");
    }

    #[test]
    fn test_very_small_numbers() {
        let d = Decimal::from_str("0.000000001").unwrap();
        assert_eq!(d.to_string(), "0.000000001");
        assert!(d.is_positive());

        let d = Decimal::from_str("-0.000000001").unwrap();
        assert_eq!(d.to_string(), "-0.000000001");
        assert!(d.is_negative());
    }

    #[test]
    fn test_very_large_numbers() {
        let d = Decimal::from_str("999999999999999999999999999999").unwrap();
        assert_eq!(d.to_string(), "999999999999999999999999999999");

        let d = Decimal::from_str("-999999999999999999999999999999").unwrap();
        assert_eq!(d.to_string(), "-999999999999999999999999999999");
    }

    #[test]
    fn test_max_exponent_boundary() {
        let d = Decimal::from_str("1e131071").unwrap();
        assert!(d.is_positive());

        let result = Decimal::from_str("1e131072");
        assert!(result.is_err());
    }

    #[test]
    fn test_min_exponent_boundary() {
        // Just above min exponent should work
        let d = Decimal::from_str("1e-16000").unwrap();
        assert!(d.is_positive());

        // Just below should fail
        let result = Decimal::from_str("1e-17000");
        assert!(result.is_err());
    }

    #[test]
    fn test_odd_digit_count() {
        // Odd number of digits (tests BCD padding)
        let d = Decimal::from_str("12345").unwrap();
        assert_eq!(d.to_string(), "12345");

        let d = Decimal::from_str("1").unwrap();
        assert_eq!(d.to_string(), "1");

        let d = Decimal::from_str("123").unwrap();
        assert_eq!(d.to_string(), "123");
    }

    #[test]
    fn test_negative_number_ordering() {
        // Verify negative number byte ordering is correct
        let a = Decimal::from_str("-100").unwrap();
        let b = Decimal::from_str("-10").unwrap();
        let c = Decimal::from_str("-1").unwrap();

        // -100 < -10 < -1 numerically
        assert!(a < b);
        assert!(b < c);

        // Byte ordering should match
        assert!(a.as_bytes() < b.as_bytes());
        assert!(b.as_bytes() < c.as_bytes());
    }

    #[test]
    fn test_from_bytes_unchecked_roundtrip() {
        let original = Decimal::from_str("123.456").unwrap();
        let bytes = original.as_bytes().to_vec();
        let restored = Decimal::from_bytes_unchecked(bytes);
        assert_eq!(original, restored);
    }

    #[test]
    fn test_special_value_checks() {
        let d = Decimal::from_str("123.456").unwrap();
        assert!(!d.is_nan());
        assert!(!d.is_infinity());
        assert!(!d.is_pos_infinity());
        assert!(!d.is_neg_infinity());
        assert!(!d.is_special());
        assert!(d.is_finite());
    }

    #[test]
    fn test_equality_and_hash_consistency() {
        use std::collections::HashMap;

        let d1 = Decimal::from_str("123.456").unwrap();
        let d2 = Decimal::from_str("123.456").unwrap();
        let d3 = Decimal::from_str("123.457").unwrap();

        // Equal values should be equal
        assert_eq!(d1, d2);
        assert_ne!(d1, d3);

        // Equal values should have same hash (can be used as map keys)
        let mut map = HashMap::new();
        map.insert(d1.clone(), "first");
        map.insert(d2.clone(), "second"); // Should overwrite
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(&d1), Some(&"second"));
    }

    #[test]
    fn test_scale_zero() {
        // Scale 0 should keep integer part only
        let d = Decimal::with_precision_scale("123.999", Some(10), Some(0)).unwrap();
        assert_eq!(d.to_string(), "124"); // Rounds up
    }

    #[test]
    fn test_only_fractional_with_precision_scale() {
        let d = Decimal::with_precision_scale(".5", Some(10), Some(2)).unwrap();
        assert_eq!(d.to_string(), "0.5");
    }

    #[test]
    fn test_default_impl() {
        let d = Decimal::default();
        assert!(d.is_zero());
        assert_eq!(d.to_string(), "0");
    }

    #[test]
    fn test_precision_zero_integer_digits() {
        // When precision equals scale, no integer digits are allowed
        // This triggers the max_integer_digits == 0 branch
        let d = Decimal::with_precision_scale("123.456", Some(2), Some(2)).unwrap();
        assert_eq!(d.to_string(), "0.46");
    }

    #[test]
    fn test_negative_with_precision_truncation() {
        // Negative number that gets truncated by precision constraints
        let d = Decimal::with_precision_scale("-123.456", Some(3), Some(2)).unwrap();
        assert_eq!(d.to_string(), "-3.46");
    }

    #[test]
    fn test_invalid_sign_byte() {
        // Sign bytes: SIGN_NEGATIVE=0x00, SIGN_ZERO=0x80, SIGN_POSITIVE=0xFF
        // Any other sign byte is invalid

        // Sign byte 0x01 is invalid
        let result = Decimal::from_bytes(&[0x01, 0x40, 0x00, 0x12]);
        assert!(result.is_err());

        // Sign byte 0x7F is also invalid
        let result = Decimal::from_bytes(&[0x7F, 0x40, 0x00, 0x12]);
        assert!(result.is_err());

        // Sign byte 0xFE is also invalid
        let result = Decimal::from_bytes(&[0xFE, 0x40, 0x00, 0x12]);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_bcd_encoding() {
        // Construct bytes that would decode to invalid BCD (digit > 9)
        // SIGN_POSITIVE = 0xFF, with valid exponent, but invalid BCD mantissa
        // A valid BCD digit must have each nibble 0-9. 0xAB has A=10, B=11 (both > 9)
        let invalid_bytes = vec![
            0xFF, // SIGN_POSITIVE
            0x80, 0x00, // Valid exponent (middle of range)
            0xAB, // Invalid BCD: high nibble = 10, low nibble = 11
        ];
        let result = Decimal::from_bytes(&invalid_bytes);
        assert!(result.is_err());

        // Also test with just high nibble invalid
        let invalid_bytes = vec![
            0xFF, // SIGN_POSITIVE
            0x80, 0x00, // Valid exponent
            0xA1, // Invalid BCD: high nibble = 10, low nibble = 1
        ];
        let result = Decimal::from_bytes(&invalid_bytes);
        assert!(result.is_err());

        // Also test with just low nibble invalid
        let invalid_bytes = vec![
            0xFF, // SIGN_POSITIVE
            0x80, 0x00, // Valid exponent
            0x1B, // Invalid BCD: high nibble = 1, low nibble = 11
        ];
        let result = Decimal::from_bytes(&invalid_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_reserved_exponent_positive() {
        // SIGN_POSITIVE = 0xFF
        // Reserved exponents: 0xFFFE (Infinity), 0xFFFF (NaN)
        // If we have more than 3 bytes, special value check is skipped
        // but decode_exponent will catch the reserved value

        // Reserved NaN exponent (0xFFFF) with extra mantissa byte
        let bytes_with_reserved_exp = vec![
            0xFF, // SIGN_POSITIVE
            0xFF, 0xFF, // Reserved for NaN
            0x12, // Some mantissa (makes it 4 bytes, not 3)
        ];
        let result = Decimal::from_bytes(&bytes_with_reserved_exp);
        assert!(result.is_err());

        // Reserved Infinity exponent (0xFFFE) with extra mantissa byte
        let bytes_with_reserved_exp = vec![
            0xFF, // SIGN_POSITIVE
            0xFF, 0xFE, // Reserved for Infinity
            0x12, // Some mantissa (makes it 4 bytes, not 3)
        ];
        let result = Decimal::from_bytes(&bytes_with_reserved_exp);
        assert!(result.is_err());
    }

    #[test]
    fn test_reserved_exponent_negative() {
        // SIGN_NEGATIVE = 0x00
        // Reserved exponent for -Infinity: 0x0000
        // If we have more than 3 bytes, special value check is skipped

        let bytes_with_reserved_exp = vec![
            0x00, // SIGN_NEGATIVE
            0x00, 0x00, // Reserved for -Infinity
            0x12, // Some mantissa (makes it 4 bytes, not 3)
        ];
        let result = Decimal::from_bytes(&bytes_with_reserved_exp);
        assert!(result.is_err());
    }

    mod ordering_props {
        use super::*;
        use bigdecimal::BigDecimal;
        use proptest::prelude::*;

        /// Decimal strings with enough shape variety to hit shared-prefix mantissas,
        /// odd digit counts, and exponent boundaries on both sides of zero.
        fn decimal_string() -> impl Strategy<Value = String> {
            (
                any::<bool>(),
                "[0-9]{0,24}",
                proptest::option::of("[0-9]{1,24}"),
                proptest::option::of(-40i32..40),
            )
                .prop_filter_map("needs a digit", |(negative, int, frac, exp)| {
                    if int.is_empty() && frac.is_none() {
                        return None;
                    }
                    let mut s = String::new();
                    if negative {
                        s.push('-');
                    }
                    s.push_str(if int.is_empty() { "0" } else { &int });
                    if let Some(frac) = frac {
                        s.push('.');
                        s.push_str(&frac);
                    }
                    if let Some(exp) = exp {
                        s.push_str(&format!("e{exp}"));
                    }
                    Some(s)
                })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(4000))]

            #[test]
            fn byte_order_matches_numeric_order(a in decimal_string(), b in decimal_string()) {
                let (da, db) = (Decimal::from_str(&a).unwrap(), Decimal::from_str(&b).unwrap());
                let (ba, bb) = (BigDecimal::from_str(&a).unwrap(), BigDecimal::from_str(&b).unwrap());
                prop_assert_eq!(da.as_bytes().cmp(db.as_bytes()), ba.cmp(&bb), "{} vs {}", a, b);
            }

            #[test]
            fn roundtrip_through_both_layouts(a in decimal_string()) {
                let d = Decimal::from_str(&a).unwrap();
                let expected = BigDecimal::from_str(&a).unwrap();
                prop_assert_eq!(BigDecimal::from_str(&d.to_string()).unwrap(), expected.clone());
                let legacy = Decimal::from_bytes(&d.to_legacy_bytes()).unwrap();
                prop_assert_eq!(&legacy, &d);
                prop_assert_eq!(BigDecimal::from_str(&legacy.to_string()).unwrap(), expected);
            }
        }
    }

    #[test]
    fn test_empty_mantissa_bytes() {
        // Construct bytes with valid sign and exponent but no mantissa
        // This should decode to 0 via the empty digits path in format_decimal
        let bytes_no_mantissa = vec![
            0xFF, // SIGN_POSITIVE
            0x80, 0x00, // Valid exponent
                  // No mantissa bytes
        ];
        let d = Decimal::from_bytes(&bytes_no_mantissa).unwrap();
        assert_eq!(d.to_string(), "0");
    }

    // ==================== rust_decimal Interop Tests ====================

    #[cfg(feature = "rust_decimal")]
    mod rust_decimal_tests {
        use super::*;

        #[test]
        fn test_from_rust_decimal() {
            use rust_decimal::Decimal as RustDecimal;

            let rd = RustDecimal::new(12345, 2); // 123.45
            let d: Decimal = rd.try_into().unwrap();
            assert_eq!(d.to_string(), "123.45");
        }

        #[test]
        fn test_to_rust_decimal() {
            use rust_decimal::Decimal as RustDecimal;

            let d = Decimal::from_str("123.45").unwrap();
            let rd: RustDecimal = (&d).try_into().unwrap();
            assert_eq!(rd.to_string(), "123.45");
        }

        #[test]
        fn test_rust_decimal_roundtrip() {
            use rust_decimal::Decimal as RustDecimal;

            let values = vec!["0", "1", "-1", "123.456", "-999.999", "0.001"];

            for s in values {
                let d = Decimal::from_str(s).unwrap();
                let rd: RustDecimal = (&d).try_into().unwrap();
                let d2: Decimal = rd.try_into().unwrap();
                assert_eq!(d, d2, "Roundtrip failed for {}", s);
            }
        }

        #[test]
        fn test_rust_decimal_arithmetic() {
            use rust_decimal::Decimal as RustDecimal;

            // Start with decimal-bytes values
            let a = Decimal::from_str("100.50").unwrap();
            let b = Decimal::from_str("25.25").unwrap();

            // Convert to rust_decimal for arithmetic
            let ra: RustDecimal = (&a).try_into().unwrap();
            let rb: RustDecimal = (&b).try_into().unwrap();
            let sum = ra + rb;

            // Convert back to decimal-bytes for storage
            let result: Decimal = sum.try_into().unwrap();
            assert_eq!(result.to_string(), "125.75");
        }

        #[test]
        fn test_rust_decimal_from_owned() {
            use rust_decimal::Decimal as RustDecimal;

            // Test TryFrom<Decimal> (owned) for RustDecimal
            let d = Decimal::from_str("456.789").unwrap();
            let rd: RustDecimal = d.try_into().unwrap();
            assert_eq!(rd.to_string(), "456.789");
        }

        #[test]
        fn test_rust_decimal_special_values_fail() {
            use rust_decimal::Decimal as RustDecimal;

            // Infinity cannot convert to rust_decimal
            let inf = Decimal::infinity();
            let result: Result<RustDecimal, _> = (&inf).try_into();
            assert!(result.is_err());

            // NaN cannot convert to rust_decimal
            let nan = Decimal::nan();
            let result: Result<RustDecimal, _> = (&nan).try_into();
            assert!(result.is_err());
        }
    }

    // ==================== bigdecimal Interop Tests ====================

    #[cfg(feature = "bigdecimal")]
    mod bigdecimal_tests {
        use super::*;

        #[test]
        fn test_from_bigdecimal() {
            use bigdecimal::BigDecimal;
            use std::str::FromStr;

            let bd = BigDecimal::from_str("123.45").unwrap();
            let d: Decimal = bd.try_into().unwrap();
            assert_eq!(d.to_string(), "123.45");
        }

        #[test]
        fn test_to_bigdecimal() {
            use bigdecimal::BigDecimal;

            let d = Decimal::from_str("123.45").unwrap();
            let bd: BigDecimal = (&d).try_into().unwrap();
            assert_eq!(bd.to_string(), "123.45");
        }

        #[test]
        fn test_bigdecimal_roundtrip() {
            use bigdecimal::BigDecimal;

            let values = vec!["0", "1", "-1", "123.456", "-999.999", "0.001"];

            for s in values {
                let d = Decimal::from_str(s).unwrap();
                let bd: BigDecimal = (&d).try_into().unwrap();
                let d2: Decimal = bd.try_into().unwrap();
                assert_eq!(d, d2, "Roundtrip failed for {}", s);
            }
        }

        #[test]
        fn test_bigdecimal_from_owned() {
            use bigdecimal::BigDecimal;

            // Test TryFrom<Decimal> (owned) for BigDecimal
            let d = Decimal::from_str("456.789").unwrap();
            let bd: BigDecimal = d.try_into().unwrap();
            assert_eq!(bd.to_string(), "456.789");
        }

        #[test]
        fn test_bigdecimal_from_ref() {
            use bigdecimal::BigDecimal;
            use std::str::FromStr;

            // Test TryFrom<&BigDecimal> for Decimal
            let bd = BigDecimal::from_str("789.012").unwrap();
            let d: Decimal = (&bd).try_into().unwrap();
            assert_eq!(d.to_string(), "789.012");
        }

        #[test]
        fn test_bigdecimal_special_values_fail() {
            use bigdecimal::BigDecimal;

            // Infinity cannot convert to BigDecimal
            let inf = Decimal::infinity();
            let result: Result<BigDecimal, _> = (&inf).try_into();
            assert!(result.is_err());

            // NaN cannot convert to BigDecimal
            let nan = Decimal::nan();
            let result: Result<BigDecimal, _> = (&nan).try_into();
            assert!(result.is_err());
        }
    }
}

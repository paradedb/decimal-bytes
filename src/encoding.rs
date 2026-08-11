//! Byte encoding for decimal values.
//!
//! This module implements a lexicographically sortable encoding for decimal numbers.
//! The encoding ensures that byte-wise comparison yields the same result as numerical comparison.
//!
//! ## Encoding Format
//!
//! ```text
//! [sign byte] [exponent bytes] [mantissa bytes]
//! ```
//!
//! - **Sign byte**: 0x00 for negative, 0x80 for zero, 0xFF for positive
//! - **Exponent**: Variable-length, biased encoding (inverted for negative numbers)
//! - **Mantissa**: BCD-encoded digits, 2 per byte (inverted for negative numbers)
//!
//! ## Special Values (PostgreSQL compatible)
//!
//! - **-Infinity**: Sorts less than all negative numbers
//! - **+Infinity**: Sorts greater than all positive numbers
//! - **NaN**: Sorts greater than +Infinity (per PostgreSQL semantics)
//!
//! ## Sort Order
//!
//! ```text
//! -Infinity < negatives < zero < positives < +Infinity < NaN
//! ```

use thiserror::Error;

/// Sign byte values for regular numbers
pub(crate) const SIGN_NEGATIVE: u8 = 0x00;
pub(crate) const SIGN_ZERO: u8 = 0x80;
pub(crate) const SIGN_POSITIVE: u8 = 0xFF;

/// Special value encodings (designed for correct lexicographic ordering)
/// -Infinity: [0x00, 0x00, 0x00] - sorts before all negative numbers
pub const ENCODING_NEG_INFINITY: [u8; 3] = [0x00, 0x00, 0x00];
/// +Infinity: [0xFF, 0xFF, 0xFE] - sorts after all positive numbers
pub const ENCODING_POS_INFINITY: [u8; 3] = [0xFF, 0xFF, 0xFE];
/// NaN: [0xFF, 0xFF, 0xFF] - sorts after +Infinity (PostgreSQL semantics)
pub const ENCODING_NAN: [u8; 3] = [0xFF, 0xFF, 0xFF];

/// Reserved exponent values (to distinguish special values from regular numbers)
const RESERVED_NEG_INFINITY_EXP: u16 = 0x0000; // For negative sign byte
const RESERVED_POS_INFINITY_EXP: u16 = 0xFFFE; // For positive sign byte
const RESERVED_NAN_EXP: u16 = 0xFFFF; // For positive sign byte

/// Exponent bias to make all exponents positive for encoding
const EXPONENT_BIAS: i32 = 16384;
const MAX_EXPONENT: i32 = 32767 - EXPONENT_BIAS - 2; // Reserve top 2 values for Infinity/NaN
const MIN_EXPONENT: i32 = -EXPONENT_BIAS + 1; // Reserve 0x0000 for -Infinity

/// Errors that can occur during decimal encoding/decoding.
#[derive(Error, Debug, Clone, PartialEq)]
pub enum DecimalError {
    /// The input string format is invalid.
    #[error("Invalid format: {0}")]
    InvalidFormat(String),

    /// The number exceeds the supported precision range.
    #[error("Precision overflow: exponent out of range")]
    PrecisionOverflow,

    /// The encoded bytes are invalid.
    #[error("Invalid encoding")]
    InvalidEncoding,
}

/// Special decimal values (IEEE 754 / PostgreSQL compatible)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialValue {
    /// Positive infinity
    Infinity,
    /// Negative infinity
    NegInfinity,
    /// Not a Number
    NaN,
}

/// Encodes a decimal string to sortable bytes.
pub fn encode_decimal(value: &str) -> Result<Vec<u8>, DecimalError> {
    // Check for special values first
    if let Some(special) = parse_special_value(value) {
        return Ok(encode_special_value(special));
    }

    let (is_negative, digits, exponent) = parse_decimal(value)?;

    // Handle zero
    if digits.is_empty() {
        return Ok(vec![SIGN_ZERO]);
    }

    let mut result = Vec::with_capacity(1 + 2 + digits.len().div_ceil(2));

    // Sign byte
    result.push(if is_negative {
        SIGN_NEGATIVE
    } else {
        SIGN_POSITIVE
    });

    // Encode exponent
    encode_exponent(&mut result, exponent, is_negative);

    // Encode mantissa (BCD, 2 digits per byte)
    encode_mantissa(&mut result, &digits, is_negative);

    Ok(result)
}

/// Parses special value strings (case-insensitive).
fn parse_special_value(value: &str) -> Option<SpecialValue> {
    let trimmed = value.trim();
    let lower = trimmed.to_lowercase();

    match lower.as_str() {
        "infinity" | "inf" | "+infinity" | "+inf" => Some(SpecialValue::Infinity),
        "-infinity" | "-inf" => Some(SpecialValue::NegInfinity),
        "nan" | "-nan" | "+nan" => Some(SpecialValue::NaN), // PostgreSQL treats all NaN as equal
        _ => None,
    }
}

/// Encodes a special value to bytes.
pub fn encode_special_value(special: SpecialValue) -> Vec<u8> {
    match special {
        SpecialValue::NegInfinity => ENCODING_NEG_INFINITY.to_vec(),
        SpecialValue::Infinity => ENCODING_POS_INFINITY.to_vec(),
        SpecialValue::NaN => ENCODING_NAN.to_vec(),
    }
}

/// Checks if bytes represent a special value.
pub fn decode_special_value(bytes: &[u8]) -> Option<SpecialValue> {
    if bytes.len() == 3 {
        if bytes == ENCODING_NEG_INFINITY {
            return Some(SpecialValue::NegInfinity);
        }
        if bytes == ENCODING_POS_INFINITY {
            return Some(SpecialValue::Infinity);
        }
        if bytes == ENCODING_NAN {
            return Some(SpecialValue::NaN);
        }
    }
    None
}

/// Encodes a decimal string with precision and scale constraints.
///
/// # Arguments
/// * `value` - The decimal string to encode
/// * `precision` - Maximum total significant digits (None = unlimited)
/// * `scale` - Number of digits after decimal point; negative values round to left of decimal
///
/// # PostgreSQL Compatibility
/// Supports negative scale (rounds to powers of 10):
/// - scale = -3 rounds to nearest 1000
/// - NUMERIC(2, -3) allows values like -99000 to 99000
pub fn encode_decimal_with_constraints(
    value: &str,
    precision: Option<u32>,
    scale: Option<i32>,
) -> Result<Vec<u8>, DecimalError> {
    // Handle special values - they ignore precision/scale
    if parse_special_value(value).is_some() {
        return encode_decimal(value);
    }

    let truncated = truncate_decimal(value, precision, scale)?;
    encode_decimal(&truncated)
}

/// Decodes bytes back to a decimal string.
pub fn decode_to_string(bytes: &[u8]) -> Result<String, DecimalError> {
    if bytes.is_empty() {
        return Err(DecimalError::InvalidEncoding);
    }

    // Check for special values first
    if let Some(special) = decode_special_value(bytes) {
        return Ok(match special {
            SpecialValue::NegInfinity => "-Infinity".to_string(),
            SpecialValue::Infinity => "Infinity".to_string(),
            SpecialValue::NaN => "NaN".to_string(),
        });
    }

    let sign_byte = bytes[0];

    // Handle zero
    if sign_byte == SIGN_ZERO {
        return Ok("0".to_string());
    }

    let is_negative = sign_byte == SIGN_NEGATIVE;

    if sign_byte != SIGN_NEGATIVE && sign_byte != SIGN_POSITIVE {
        return Err(DecimalError::InvalidEncoding);
    }

    // Decode exponent (also validates it's not a reserved value)
    let (exponent, mantissa_start) = decode_exponent(&bytes[1..], is_negative)?;

    // Decode mantissa
    let mantissa_bytes = &bytes[1 + mantissa_start..];
    let digits = decode_mantissa(mantissa_bytes, is_negative)?;

    // Format as string
    format_decimal(is_negative, &digits, exponent)
}

/// Decodes bytes back to a decimal string with a specific scale.
///
/// This ensures the output has exactly `scale` decimal places, adding trailing
/// zeros if needed. This is useful for PostgreSQL NUMERIC display formatting.
///
/// # Arguments
/// * `bytes` - The encoded decimal bytes
/// * `scale` - Number of decimal places to ensure in the output
///
/// # Examples
/// ```ignore
/// // If bytes represent "1", with scale 18, returns "1.000000000000000000"
/// // If bytes represent "1.5", with scale 18, returns "1.500000000000000000"
/// ```
pub fn decode_to_string_with_scale(bytes: &[u8], scale: i32) -> Result<String, DecimalError> {
    // First decode to normalized string
    let normalized = decode_to_string(bytes)?;

    // Handle special values - they don't get scale formatting
    if normalized == "NaN" || normalized == "Infinity" || normalized == "-Infinity" {
        return Ok(normalized);
    }

    // If scale <= 0, no decimal places needed
    if scale <= 0 {
        return Ok(normalized);
    }

    let scale = scale as usize;

    // Find the decimal point position
    if let Some(dot_pos) = normalized.find('.') {
        let current_decimals = normalized.len() - dot_pos - 1;
        if current_decimals >= scale {
            // Already has enough decimal places
            Ok(normalized)
        } else {
            // Need to add trailing zeros
            let zeros_needed = scale - current_decimals;
            Ok(format!("{}{}", normalized, "0".repeat(zeros_needed)))
        }
    } else {
        // No decimal point - add one with the required zeros
        Ok(format!("{}.{}", normalized, "0".repeat(scale)))
    }
}

/// Parses a decimal string into sign, digits, and exponent.
fn parse_decimal(value: &str) -> Result<(bool, Vec<u8>, i32), DecimalError> {
    let value = value.trim();
    let mut chars = value.chars().peekable();

    // Handle sign
    let is_negative = if chars.peek() == Some(&'-') {
        chars.next();
        true
    } else if chars.peek() == Some(&'+') {
        chars.next();
        false
    } else {
        false
    };

    // Collect the numeric part (before 'e' or 'E')
    let mut integer_part = String::new();
    let mut fractional_part = String::new();
    let mut seen_decimal = false;
    let mut seen_exponent_marker = false;

    while let Some(&c) = chars.peek() {
        if c == '.' {
            if seen_decimal {
                return Err(DecimalError::InvalidFormat(
                    "Multiple decimal points".to_string(),
                ));
            }
            seen_decimal = true;
            chars.next();
        } else if c.is_ascii_digit() {
            if seen_decimal {
                fractional_part.push(c);
            } else {
                integer_part.push(c);
            }
            chars.next();
        } else if c == 'e' || c == 'E' {
            seen_exponent_marker = true;
            chars.next();
            break;
        } else {
            return Err(DecimalError::InvalidFormat(format!(
                "Invalid character: {}",
                c
            )));
        }
    }

    // Parse exponent (required if 'e' or 'E' was seen)
    let mut exp_offset: i32 = 0;
    if seen_exponent_marker {
        if chars.peek().is_none() {
            return Err(DecimalError::InvalidFormat(
                "Missing exponent after 'e'".to_string(),
            ));
        }
        let exp_str: String = chars.collect();
        exp_offset = exp_str
            .parse()
            .map_err(|_| DecimalError::InvalidFormat(format!("Invalid exponent: {}", exp_str)))?;
    }

    // Handle empty input
    if integer_part.is_empty() && fractional_part.is_empty() {
        return Ok((false, vec![], 0));
    }

    // If only fractional part, integer part is "0"
    if integer_part.is_empty() {
        integer_part.push('0');
    }

    // Remember where the decimal point was before combining
    let decimal_position = integer_part.len();

    // Combine all digits by appending fractional part (avoids extra allocation)
    integer_part.push_str(&fractional_part);
    let all_digits = integer_part;

    // Find the first and last non-zero digit positions
    let first_nonzero = all_digits.chars().position(|c| c != '0');
    let last_nonzero = all_digits.chars().rev().position(|c| c != '0');

    // If all zeros, return zero
    if first_nonzero.is_none() {
        return Ok((false, vec![], 0));
    }

    let first_nonzero = first_nonzero.unwrap();
    let last_nonzero = all_digits.len() - 1 - last_nonzero.unwrap();

    // Extract the significant digits
    let significant = &all_digits[first_nonzero..=last_nonzero];

    // Calculate the exponent
    let exponent = (decimal_position as i32) - (first_nonzero as i32) + exp_offset;

    // Convert significant digits to bytes
    let digits: Vec<u8> = significant
        .chars()
        .map(|c| c.to_digit(10).unwrap() as u8)
        .collect();

    // Validate exponent range
    if !(MIN_EXPONENT..=MAX_EXPONENT).contains(&exponent) {
        return Err(DecimalError::PrecisionOverflow);
    }

    Ok((is_negative, digits, exponent))
}

/// Encodes the exponent as variable-length bytes.
fn encode_exponent(result: &mut Vec<u8>, exponent: i32, is_negative: bool) {
    // Bias the exponent to make it always positive
    // Note: We add 1 to reserve 0x0000 for -Infinity on negative side
    let biased = (exponent + EXPONENT_BIAS) as u16;

    // For negative numbers, invert the exponent so larger negative numbers sort first
    let encoded = if is_negative { !biased } else { biased };

    // Use 2 bytes for the exponent (big-endian)
    result.push((encoded >> 8) as u8);
    result.push((encoded & 0xFF) as u8);
}

/// Decodes the exponent from bytes.
fn decode_exponent(bytes: &[u8], is_negative: bool) -> Result<(i32, usize), DecimalError> {
    if bytes.len() < 2 {
        return Err(DecimalError::InvalidEncoding);
    }

    let encoded = ((bytes[0] as u16) << 8) | (bytes[1] as u16);

    // Check for reserved values (should have been caught by decode_special_value)
    if is_negative && encoded == RESERVED_NEG_INFINITY_EXP {
        return Err(DecimalError::InvalidEncoding);
    }
    if !is_negative && (encoded == RESERVED_POS_INFINITY_EXP || encoded == RESERVED_NAN_EXP) {
        return Err(DecimalError::InvalidEncoding);
    }

    let biased = if is_negative { !encoded } else { encoded };
    let exponent = (biased as i32) - EXPONENT_BIAS;

    Ok((exponent, 2))
}

/// Encodes the mantissa as BCD (2 digits per byte).
fn encode_mantissa(result: &mut Vec<u8>, digits: &[u8], is_negative: bool) {
    // Pack 2 digits per byte
    let mut i = 0;
    while i < digits.len() {
        let high = digits[i];
        let low = if i + 1 < digits.len() {
            digits[i + 1]
        } else {
            0 // Pad with 0 if odd number of digits
        };

        let byte = (high << 4) | low;

        // For negative numbers, invert to reverse the sort order
        result.push(if is_negative { !byte } else { byte });

        i += 2;
    }
}

/// Decodes the mantissa from BCD bytes.
fn decode_mantissa(bytes: &[u8], is_negative: bool) -> Result<Vec<u8>, DecimalError> {
    let mut digits = Vec::with_capacity(bytes.len() * 2);
    decode_mantissa_into(bytes, is_negative, &mut digits)?;
    Ok(digits)
}

/// Decodes the mantissa from BCD bytes into a caller-provided buffer, so hot
/// loops can reuse one allocation across many decodes.
fn decode_mantissa_into(
    bytes: &[u8],
    is_negative: bool,
    digits: &mut Vec<u8>,
) -> Result<(), DecimalError> {
    digits.clear();
    digits.reserve(bytes.len() * 2);

    for &byte in bytes {
        let byte = if is_negative { !byte } else { byte };
        let high = (byte >> 4) & 0x0F;
        let low = byte & 0x0F;

        if high > 9 || low > 9 {
            return Err(DecimalError::InvalidEncoding);
        }

        digits.push(high);
        digits.push(low);
    }

    // Remove trailing zeros (padding)
    while digits.last() == Some(&0) && digits.len() > 1 {
        digits.pop();
    }

    Ok(())
}

/// Decodes a finite value's parts without formatting a string:
/// `value = sign * 0.digits * 10^exponent`, digit values 0-9, most
/// significant first, trailing zeros stripped. Clears and refills `digits`
/// so a caller can reuse one buffer across many decodes.
///
/// Returns `Ok(None)` for NaN and +/-Infinity. Zero decodes to an empty
/// digit buffer with exponent 0.
pub fn decode_parts_into(
    bytes: &[u8],
    digits: &mut Vec<u8>,
) -> Result<Option<(bool, i32)>, DecimalError> {
    if bytes.is_empty() {
        return Err(DecimalError::InvalidEncoding);
    }

    if decode_special_value(bytes).is_some() {
        return Ok(None);
    }

    let sign_byte = bytes[0];

    if sign_byte == SIGN_ZERO {
        digits.clear();
        return Ok(Some((false, 0)));
    }

    let is_negative = sign_byte == SIGN_NEGATIVE;

    if sign_byte != SIGN_NEGATIVE && sign_byte != SIGN_POSITIVE {
        return Err(DecimalError::InvalidEncoding);
    }

    let (exponent, mantissa_start) = decode_exponent(&bytes[1..], is_negative)?;
    decode_mantissa_into(&bytes[1 + mantissa_start..], is_negative, digits)?;

    Ok(Some((is_negative, exponent)))
}

/// Formats digits and exponent back to a decimal string.
fn format_decimal(is_negative: bool, digits: &[u8], exponent: i32) -> Result<String, DecimalError> {
    if digits.is_empty() {
        return Ok("0".to_string());
    }

    let mut result = String::new();

    if is_negative {
        result.push('-');
    }

    let num_digits = digits.len() as i32;

    if exponent >= num_digits {
        // All digits are before the decimal point (integer part)
        for d in digits {
            result.push(char::from_digit(*d as u32, 10).unwrap());
        }
        // Add trailing zeros if needed
        for _ in 0..(exponent - num_digits) {
            result.push('0');
        }
    } else if exponent <= 0 {
        // All digits are after the decimal point
        result.push('0');
        result.push('.');
        for _ in 0..(-exponent) {
            result.push('0');
        }
        for d in digits {
            result.push(char::from_digit(*d as u32, 10).unwrap());
        }
    } else {
        // Some digits before decimal, some after
        let decimal_pos = exponent as usize;
        for (i, d) in digits.iter().enumerate() {
            if i == decimal_pos {
                result.push('.');
            }
            result.push(char::from_digit(*d as u32, 10).unwrap());
        }
    }

    Ok(result)
}

/// Truncates a decimal string to fit precision and scale constraints.
///
/// # PostgreSQL Compatibility
/// - Positive scale: digits after decimal point
/// - Negative scale: rounds to left of decimal (e.g., -3 rounds to nearest 1000)
/// - Precision: total significant (non-rounded) digits
fn truncate_decimal(
    value: &str,
    precision: Option<u32>,
    scale: Option<i32>,
) -> Result<String, DecimalError> {
    // Parse to get sign and parts
    let value = value.trim();
    let is_negative = value.starts_with('-');
    let value = value.trim_start_matches(['-', '+']);

    // Split into integer and fractional parts
    let (integer_part, fractional_part) = if let Some(dot_pos) = value.find('.') {
        (&value[..dot_pos], &value[dot_pos + 1..])
    } else {
        (value, "")
    };

    // Trim leading zeros from integer part (but keep at least one digit)
    let integer_part = integer_part.trim_start_matches('0');
    let integer_part = if integer_part.is_empty() {
        "0"
    } else {
        integer_part
    };

    let scale_val = scale.unwrap_or(0);

    // Handle negative scale (round to left of decimal point)
    if scale_val < 0 {
        let round_digits = (-scale_val) as usize;

        // Remove all fractional digits when scale is negative
        let mut int_str = integer_part.to_string();

        if int_str.len() <= round_digits {
            // Number is smaller than the rounding unit
            // Round: if the number >= half the rounding unit, round up to the unit
            let num_val: u64 = int_str.parse().unwrap_or(0);
            let rounding_unit = 10u64.pow(round_digits as u32);
            let half_unit = rounding_unit / 2;

            let result = if num_val >= half_unit {
                rounding_unit.to_string()
            } else {
                "0".to_string()
            };

            return if is_negative && result != "0" {
                Ok(format!("-{}", result))
            } else {
                Ok(result)
            };
        }

        // Round the integer part
        let keep_len = int_str.len() - round_digits;
        let keep_part = &int_str[..keep_len];
        let round_part = &int_str[keep_len..];

        // Check if we need to round up
        let first_rounded_digit = round_part.chars().next().unwrap_or('0');
        let mut result_int = keep_part.to_string();

        if first_rounded_digit >= '5' {
            result_int = add_one_to_integer(&result_int);
        }

        // Add trailing zeros
        int_str = format!("{}{}", result_int, "0".repeat(round_digits));

        // Apply precision constraint for negative scale
        if let Some(p) = precision {
            let max_significant = p as usize;
            let significant_len = result_int.trim_start_matches('0').len();
            if significant_len > max_significant && max_significant > 0 {
                // Truncate from left (keep least significant digits)
                let trimmed = &result_int[result_int.len().saturating_sub(max_significant)..];
                int_str = format!("{}{}", trimmed, "0".repeat(round_digits));
            }
        }

        return if is_negative && int_str != "0" {
            Ok(format!("-{}", int_str))
        } else {
            Ok(int_str)
        };
    }

    // Handle positive scale (normal case - digits after decimal)
    let scale_usize = scale_val as usize;

    // Apply scale constraint (truncate/round fractional part)
    let (mut integer_part, fractional_part) = if fractional_part.len() > scale_usize {
        // Round the last digit
        let truncated = &fractional_part[..scale_usize];
        let next_digit = fractional_part.chars().nth(scale_usize).unwrap_or('0');

        if next_digit >= '5' {
            // Round up - this may carry into integer part
            if scale_usize == 0 {
                // Rounding to integer
                (add_one_to_integer(integer_part), String::new())
            } else {
                let rounded = round_up(truncated);
                if rounded.len() > scale_usize {
                    // Carry into integer part
                    let new_int = add_one_to_integer(integer_part);
                    (new_int, "0".repeat(scale_usize))
                } else {
                    (integer_part.to_string(), rounded)
                }
            }
        } else {
            (integer_part.to_string(), truncated.to_string())
        }
    } else {
        (integer_part.to_string(), fractional_part.to_string())
    };

    // Apply precision constraint
    if let Some(p) = precision {
        let max_integer_digits = if (p as i32) > scale_val {
            (p as i32 - scale_val) as usize
        } else {
            0
        };

        if integer_part.len() > max_integer_digits && max_integer_digits > 0 {
            // Truncate from the left (keep least significant digits)
            integer_part = integer_part[integer_part.len() - max_integer_digits..].to_string();
        } else if max_integer_digits == 0 {
            integer_part = "0".to_string();
        }
    }

    // Reconstruct
    let result = if fractional_part.is_empty() || fractional_part.chars().all(|c| c == '0') {
        integer_part
    } else {
        format!("{}.{}", integer_part, fractional_part.trim_end_matches('0'))
    };

    if is_negative && result != "0" {
        Ok(format!("-{}", result))
    } else {
        Ok(result)
    }
}

/// Adds 1 to an integer string.
fn add_one_to_integer(s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    let mut carry = true;

    for c in chars.iter_mut().rev() {
        if carry {
            if *c == '9' {
                *c = '0';
            } else {
                *c = char::from_digit(c.to_digit(10).unwrap() + 1, 10).unwrap();
                carry = false;
            }
        }
    }

    if carry {
        format!("1{}", chars.iter().collect::<String>())
    } else {
        chars.iter().collect()
    }
}

/// Rounds up a digit string by adding 1 to the last digit.
fn round_up(s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    let mut carry = true;

    for c in chars.iter_mut().rev() {
        if carry {
            if *c == '9' {
                *c = '0';
            } else {
                *c = char::from_digit(c.to_digit(10).unwrap() + 1, 10).unwrap();
                carry = false;
            }
        }
    }

    if carry {
        // All 9s became 0s, prepend 1
        format!("1{}", chars.iter().collect::<String>())
    } else {
        chars.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let values = vec![
            "0",
            "1",
            "-1",
            "123.456",
            "-123.456",
            "0.001",
            "0.1",
            "10",
            "100",
            "1000",
            "-0.001",
            "999999999999999999",
        ];

        for s in values {
            let encoded = encode_decimal(s).unwrap();
            let decoded = decode_to_string(&encoded).unwrap();
            // Re-encode to normalize
            let re_encoded = encode_decimal(&decoded).unwrap();
            assert_eq!(encoded, re_encoded, "Roundtrip failed for {}", s);
        }
    }

    #[test]
    fn test_lexicographic_ordering() {
        let values = vec![
            "-1000", "-100", "-10", "-1", "-0.1", "-0.01", "0", "0.01", "0.1", "1", "10", "100",
            "1000",
        ];

        let encoded: Vec<Vec<u8>> = values.iter().map(|s| encode_decimal(s).unwrap()).collect();

        // Verify encoding preserves order
        for i in 0..encoded.len() - 1 {
            assert!(
                encoded[i] < encoded[i + 1],
                "Ordering failed: {} should be < {}",
                values[i],
                values[i + 1]
            );
        }
    }

    #[test]
    fn test_zero_encoding() {
        let encoded = encode_decimal("0").unwrap();
        assert_eq!(encoded, vec![SIGN_ZERO]);

        let encoded = encode_decimal("0.0").unwrap();
        assert_eq!(encoded, vec![SIGN_ZERO]);

        let encoded = encode_decimal("-0").unwrap();
        assert_eq!(encoded, vec![SIGN_ZERO]);
    }

    #[test]
    fn test_truncate_scale() {
        assert_eq!(
            truncate_decimal("123.456", None, Some(2)).unwrap(),
            "123.46"
        );
        assert_eq!(
            truncate_decimal("123.454", None, Some(2)).unwrap(),
            "123.45"
        );
        assert_eq!(truncate_decimal("123.995", None, Some(2)).unwrap(), "124");
        assert_eq!(truncate_decimal("9.999", None, Some(2)).unwrap(), "10");
    }

    #[test]
    fn test_storage_efficiency() {
        // 9 digit number: should be ~1 sign + 2 exp + 5 mantissa = 8 bytes
        let encoded = encode_decimal("123456789").unwrap();
        assert!(
            encoded.len() <= 8,
            "Expected <= 8 bytes, got {}",
            encoded.len()
        );

        // Small decimal
        let encoded = encode_decimal("0.1").unwrap();
        assert!(
            encoded.len() <= 4,
            "Expected <= 4 bytes, got {}",
            encoded.len()
        );
    }

    // ==================== Special Values Tests ====================

    #[test]
    fn test_special_value_encoding() {
        // Test encoding special values
        let pos_inf = encode_decimal("Infinity").unwrap();
        assert_eq!(pos_inf, ENCODING_POS_INFINITY.to_vec());

        let neg_inf = encode_decimal("-Infinity").unwrap();
        assert_eq!(neg_inf, ENCODING_NEG_INFINITY.to_vec());

        let nan = encode_decimal("NaN").unwrap();
        assert_eq!(nan, ENCODING_NAN.to_vec());
    }

    #[test]
    fn test_special_value_decoding() {
        // Test decoding special values
        assert_eq!(
            decode_to_string(&ENCODING_POS_INFINITY).unwrap(),
            "Infinity"
        );
        assert_eq!(
            decode_to_string(&ENCODING_NEG_INFINITY).unwrap(),
            "-Infinity"
        );
        assert_eq!(decode_to_string(&ENCODING_NAN).unwrap(), "NaN");
    }

    #[test]
    fn test_special_value_parsing_variants() {
        // Test various ways to write special values (case-insensitive)
        let variants = vec![
            ("infinity", "Infinity"),
            ("Infinity", "Infinity"),
            ("INFINITY", "Infinity"),
            ("inf", "Infinity"),
            ("Inf", "Infinity"),
            ("+infinity", "Infinity"),
            ("+inf", "Infinity"),
            ("-infinity", "-Infinity"),
            ("-inf", "-Infinity"),
            ("-Infinity", "-Infinity"),
            ("nan", "NaN"),
            ("NaN", "NaN"),
            ("NAN", "NaN"),
            ("-nan", "NaN"), // PostgreSQL treats -NaN as NaN
            ("+nan", "NaN"),
        ];

        for (input, expected) in variants {
            let encoded = encode_decimal(input).unwrap();
            let decoded = decode_to_string(&encoded).unwrap();
            assert_eq!(decoded, expected, "Failed for input: {}", input);
        }
    }

    #[test]
    fn test_special_value_ordering() {
        // PostgreSQL order: -Infinity < negatives < zero < positives < Infinity < NaN
        let values = vec![
            "-Infinity",
            "-1000000",
            "-1",
            "-0.001",
            "0",
            "0.001",
            "1",
            "1000000",
            "Infinity",
            "NaN",
        ];

        let encoded: Vec<Vec<u8>> = values.iter().map(|s| encode_decimal(s).unwrap()).collect();

        // Verify ordering
        for i in 0..encoded.len() - 1 {
            assert!(
                encoded[i] < encoded[i + 1],
                "Special value ordering failed: {} should be < {} (bytes: {:?} < {:?})",
                values[i],
                values[i + 1],
                encoded[i],
                encoded[i + 1]
            );
        }
    }

    #[test]
    fn test_special_value_roundtrip() {
        let values = vec!["Infinity", "-Infinity", "NaN"];

        for s in values {
            let encoded = encode_decimal(s).unwrap();
            let decoded = decode_to_string(&encoded).unwrap();
            let re_encoded = encode_decimal(&decoded).unwrap();
            assert_eq!(
                encoded, re_encoded,
                "Special value roundtrip failed for {}",
                s
            );
        }
    }

    #[test]
    fn test_decode_special_value_helper() {
        assert_eq!(
            decode_special_value(&ENCODING_POS_INFINITY),
            Some(SpecialValue::Infinity)
        );
        assert_eq!(
            decode_special_value(&ENCODING_NEG_INFINITY),
            Some(SpecialValue::NegInfinity)
        );
        assert_eq!(decode_special_value(&ENCODING_NAN), Some(SpecialValue::NaN));

        // Regular values should return None
        let regular = encode_decimal("123.456").unwrap();
        assert_eq!(decode_special_value(&regular), None);

        let zero = encode_decimal("0").unwrap();
        assert_eq!(decode_special_value(&zero), None);
    }

    // ==================== Negative Scale Tests ====================

    #[test]
    fn test_negative_scale_basic() {
        // Round to nearest 10
        assert_eq!(truncate_decimal("123", None, Some(-1)).unwrap(), "120");
        assert_eq!(truncate_decimal("125", None, Some(-1)).unwrap(), "130");
        assert_eq!(truncate_decimal("124", None, Some(-1)).unwrap(), "120");

        // Round to nearest 100
        assert_eq!(truncate_decimal("1234", None, Some(-2)).unwrap(), "1200");
        assert_eq!(truncate_decimal("1250", None, Some(-2)).unwrap(), "1300");
        assert_eq!(truncate_decimal("1249", None, Some(-2)).unwrap(), "1200");

        // Round to nearest 1000
        assert_eq!(truncate_decimal("12345", None, Some(-3)).unwrap(), "12000");
        assert_eq!(truncate_decimal("12500", None, Some(-3)).unwrap(), "13000");
    }

    #[test]
    fn test_negative_scale_small_numbers() {
        // When number is smaller than rounding unit
        assert_eq!(truncate_decimal("499", None, Some(-3)).unwrap(), "0");
        assert_eq!(truncate_decimal("500", None, Some(-3)).unwrap(), "1000");
        assert_eq!(truncate_decimal("999", None, Some(-3)).unwrap(), "1000");

        assert_eq!(truncate_decimal("49", None, Some(-2)).unwrap(), "0");
        assert_eq!(truncate_decimal("50", None, Some(-2)).unwrap(), "100");
    }

    #[test]
    fn test_negative_scale_with_precision() {
        // NUMERIC(2, -3): max 2 significant digits, round to nearest 1000
        assert_eq!(
            truncate_decimal("12345", Some(2), Some(-3)).unwrap(),
            "12000"
        );
        // 99999 rounded to nearest 1000 = 100000
        // "100" significant part exceeds precision 2, truncated from left to "00"
        // Final: "00" + "000" trailing zeros = "00000"
        // Note: PostgreSQL would error here; we truncate instead
        assert_eq!(
            truncate_decimal("99999", Some(2), Some(-3)).unwrap(),
            "00000"
        );
    }

    #[test]
    fn test_negative_scale_negative_numbers() {
        assert_eq!(truncate_decimal("-123", None, Some(-1)).unwrap(), "-120");
        assert_eq!(truncate_decimal("-125", None, Some(-1)).unwrap(), "-130");
        assert_eq!(truncate_decimal("-1234", None, Some(-2)).unwrap(), "-1200");
    }

    #[test]
    fn test_negative_scale_with_decimal_input() {
        // Fractional part is ignored with negative scale
        assert_eq!(truncate_decimal("123.456", None, Some(-1)).unwrap(), "120");
        assert_eq!(
            truncate_decimal("1234.999", None, Some(-2)).unwrap(),
            "1200"
        );
    }

    #[test]
    fn test_negative_scale_encoding_ordering() {
        // Verify ordering is preserved with negative scale rounding
        let values = vec!["-1000", "-100", "0", "100", "1000"];

        let encoded: Vec<Vec<u8>> = values
            .iter()
            .map(|s| encode_decimal_with_constraints(s, None, Some(-2)).unwrap())
            .collect();

        for i in 0..encoded.len() - 1 {
            assert!(
                encoded[i] < encoded[i + 1],
                "Negative scale ordering failed: {} should be < {}",
                values[i],
                values[i + 1]
            );
        }
    }

    #[test]
    fn test_special_values_ignore_precision_scale() {
        // Special values should pass through unchanged regardless of precision/scale
        let inf = encode_decimal_with_constraints("Infinity", Some(5), Some(2)).unwrap();
        assert_eq!(inf, ENCODING_POS_INFINITY.to_vec());

        let neg_inf = encode_decimal_with_constraints("-Infinity", Some(5), Some(2)).unwrap();
        assert_eq!(neg_inf, ENCODING_NEG_INFINITY.to_vec());

        let nan = encode_decimal_with_constraints("NaN", Some(5), Some(2)).unwrap();
        assert_eq!(nan, ENCODING_NAN.to_vec());
    }

    #[test]
    fn test_decode_parts_into_matches_decode_to_string() {
        // Reconstructing the string from the parts must match the string
        // decoder, so both stay pinned to the same encoding semantics.
        let values = [
            "0",
            "1",
            "-1",
            "0.1",
            "-0.125",
            "3.30",
            "12000",
            "123.456",
            "-987654321.123456789",
            "0.000001",
            "999999999999999999999999999999999999999999999999999999999999999999999999999999",
            "10000000000000000000000000000000000000000000000000000000000000000000123",
        ];
        let mut digits = Vec::new();
        for value in values {
            let bytes = encode_decimal(value).unwrap();
            let (is_negative, exponent) = decode_parts_into(&bytes, &mut digits)
                .unwrap()
                .unwrap_or_else(|| panic!("finite value {value} decoded as special"));
            let formatted = format_decimal(is_negative, &digits, exponent).unwrap();
            assert_eq!(
                formatted,
                decode_to_string(&bytes).unwrap(),
                "value {value}"
            );
        }
    }

    #[test]
    fn test_decode_parts_into_reuses_buffer() {
        let mut digits = vec![9, 9, 9, 9, 9, 9, 9, 9];
        let bytes = encode_decimal("12.5").unwrap();
        let (is_negative, exponent) = decode_parts_into(&bytes, &mut digits).unwrap().unwrap();
        assert!(!is_negative);
        assert_eq!(exponent, 2);
        assert_eq!(digits, vec![1, 2, 5]);
    }

    #[test]
    fn test_decode_parts_into_zero_and_specials() {
        let mut digits = vec![7];
        let zero = encode_decimal("0").unwrap();
        assert_eq!(
            decode_parts_into(&zero, &mut digits).unwrap(),
            Some((false, 0))
        );
        assert!(digits.is_empty());

        for special in [ENCODING_NAN, ENCODING_POS_INFINITY, ENCODING_NEG_INFINITY] {
            assert_eq!(decode_parts_into(&special, &mut digits).unwrap(), None);
        }
    }
}

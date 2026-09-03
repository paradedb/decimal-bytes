//! Regression and property tests for escaped exponent encoding.

use decimal_bytes::{Decimal, DecimalError};
use proptest::prelude::*;
use std::cmp::Ordering;
use std::str::FromStr;

const MIN_NORMALIZED_EXPONENT: i64 = -16_383;
const LAST_INLINE_EXPONENT: i64 = 49_148;
const MAX_NORMALIZED_EXPONENT: i64 = 131_072;
const LAST_INLINE_ZEROS: usize = 49_147;
const MAX_PG_ZEROS: usize = 131_071;

fn pow10(zeros: usize, negative: bool) -> String {
    let mut value = String::with_capacity(zeros + usize::from(negative) + 1);
    if negative {
        value.push('-');
    }
    value.push('1');
    value.extend(std::iter::repeat_n('0', zeros));
    value
}

fn encode(value: &str) -> Vec<u8> {
    Decimal::from_str(value)
        .unwrap_or_else(|error| panic!("failed to encode {value:?}: {error:?}"))
        .into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Value {
    negative: bool,
    digits: String,
    exponent: i64,
}

impl Value {
    fn scientific(&self) -> String {
        format!(
            "{}0.{}e{}",
            if self.negative { "-" } else { "" },
            self.digits,
            self.exponent
        )
    }

    fn encode(&self) -> Vec<u8> {
        encode(&self.scientific())
    }

    fn cmp_mathematically(&self, other: &Self) -> Ordering {
        let magnitude = self
            .exponent
            .cmp(&other.exponent)
            .then_with(|| self.digits.cmp(&other.digits));

        match (self.negative, other.negative) {
            (false, false) => magnitude,
            (true, true) => magnitude.reverse(),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
        }
    }
}

fn digits_strategy(max_len: usize) -> impl Strategy<Value = String> {
    (1..=max_len)
        .prop_flat_map(|length| {
            (
                1u8..=9,
                prop::collection::vec(0u8..=9, length.saturating_sub(2)),
                1u8..=9,
            )
                .prop_map(move |(first, middle, last)| {
                    let mut digits = vec![first];
                    digits.extend(middle);
                    if length > 1 {
                        digits.push(last);
                    }
                    digits
                })
        })
        .prop_map(|digits| {
            digits
                .into_iter()
                .map(|digit| (b'0' + digit) as char)
                .collect()
        })
}

fn value_strategy() -> impl Strategy<Value = Value> {
    (
        any::<bool>(),
        digits_strategy(40),
        MIN_NORMALIZED_EXPONENT..=MAX_NORMALIZED_EXPONENT,
    )
        .prop_map(|(negative, digits, exponent)| Value {
            negative,
            digits,
            exponent,
        })
}

#[test]
fn issue_6107_value_is_accepted() {
    let value = pow10(20_000, false);
    assert_eq!(Decimal::from_str(&value).unwrap().to_string(), value);
}

#[test]
fn representative_values_round_trip_across_the_supported_range() {
    for zeros in [
        0,
        1,
        100,
        16_380,
        16_381,
        LAST_INLINE_ZEROS,
        LAST_INLINE_ZEROS + 1,
        100_000,
        MAX_PG_ZEROS,
    ] {
        for negative in [false, true] {
            let value = pow10(zeros, negative);
            assert_eq!(
                Decimal::from_str(&value).unwrap().to_string(),
                value,
                "round-trip failed for 1e{zeros}, negative={negative}"
            );
        }
    }

    for value in ["0.123456789e49148", "-0.123456789e49148"] {
        let decimal = Decimal::from_str(value).unwrap();
        let reparsed = Decimal::from_str(&decimal.to_string()).unwrap();
        assert_eq!(decimal.into_bytes(), reparsed.into_bytes());
    }
}

#[test]
fn values_outside_the_supported_range_are_rejected() {
    for value in [
        format!("0.1e{}", MAX_NORMALIZED_EXPONENT + 1),
        format!("-0.1e{}", MAX_NORMALIZED_EXPONENT + 1),
        format!("0.1e{}", MIN_NORMALIZED_EXPONENT - 1),
        format!("-0.1e{}", MIN_NORMALIZED_EXPONENT - 1),
        "1e2147483647".to_string(),
        "-1e2147483647".to_string(),
    ] {
        assert!(Decimal::from_str(&value).is_err(), "{value} should fail");
    }

    for value in [
        "1e2147483647",
        "-1e2147483647",
        "1e2147483648",
        "-1e-2147483648",
    ] {
        let result = Decimal::from_str(value);
        assert!(
            matches!(result, Err(DecimalError::PrecisionOverflow)),
            "{value}: {result:?}"
        );
    }
}

#[test]
fn encodings_are_byte_for_byte_stable() {
    // Inline encodings must stay byte-identical for on-disk compatibility.
    // Escaped rows pin the post-inline form that also lands in indexes.
    let golden = [
        ("0", "80"),
        ("1", "ff400110"),
        ("-1", "00bffe89ff"),
        ("0.0001", "ff3ffd10"),
        ("-0.0001", "00c00289ff"),
        ("123.456", "ff4003123456"),
        ("-123.456", "00bffc876543ff"),
        ("9223372036854775807", "ff401392233720368547758070"),
        ("-9223372036854775808", "00bfec07766279631452241919ff"),
        ("1e100", "ff406510"),
        ("-1e100", "00bf9a89ff"),
        ("1e-16000", "ff018110"),
        ("-1e-16000", "00fe7e89ff"),
        ("1e16000", "ff7e8110"),
        ("-1e16000", "00817e89ff"),
        ("1e16380", "ff7ffd10"),
        ("-1e16380", "00800289ff"),
        ("1e49148", "fffffd0000bffd10"),
        ("-1e49148", "000002ffff400289ff"),
        ("Infinity", "fffffe"),
        ("-Infinity", "000000"),
        ("NaN", "ffffff"),
    ];

    for (value, expected) in golden {
        let actual: String = encode(value)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(actual, expected, "encoding of {value} changed");
    }
}

#[test]
fn escaped_encoding_starts_after_inline_range_and_before_infinity() {
    let inline = encode(&pow10(LAST_INLINE_ZEROS, false));
    let escaped = encode(&pow10(LAST_INLINE_ZEROS + 1, false));
    assert_eq!(escaped.len(), inline.len() + 4);
    assert_eq!(&escaped[1..3], &[0xff, 0xfd]);
    assert!(inline < escaped);

    let inline_negative = encode(&pow10(LAST_INLINE_ZEROS, true));
    let escaped_negative = encode(&pow10(LAST_INLINE_ZEROS + 1, true));
    assert_eq!(escaped_negative.len(), inline_negative.len() + 4);
    assert!(escaped_negative < inline_negative);

    assert!(escaped < encode("Infinity"));
    assert!(encode("-Infinity") < escaped_negative);
}

#[test]
fn malformed_and_noncanonical_exponents_are_rejected() {
    let malformed = [
        vec![0xff, 0xff, 0xfd],
        vec![0xff, 0xff, 0xfd, 0, 0, 0, 1, 0x10],
        vec![0xff, 0xff, 0xfd, 0, 2, 0, 1, 0x10],
        vec![0xff, 0, 0, 0x10],
        vec![0, 0xff, 0xff, 0x89, 0xff],
        vec![0, 0, 1, 0x89, 0xff],
        vec![0, 0, 0x02, 0xff, 0xff, 0xff, 0xff, 0xef],
    ];

    for bytes in malformed {
        assert!(
            Decimal::from_bytes(&bytes).is_err(),
            "{bytes:02x?} should fail"
        );
    }
}

#[test]
fn boundary_sweep_and_special_values_preserve_sort_order() {
    for negative in [false, true] {
        let values: Vec<Value> = (LAST_INLINE_EXPONENT - 40..=LAST_INLINE_EXPONENT + 40)
            .map(|exponent| Value {
                negative,
                digits: "1234567890123456789".to_string(),
                exponent,
            })
            .collect();

        let mut by_bytes = values.clone();
        by_bytes.sort_by_key(Value::encode);
        let mut mathematically = values;
        mathematically.sort_by(Value::cmp_mathematically);
        assert_eq!(by_bytes, mathematically);
    }

    let negative_infinity = encode("-Infinity");
    let positive_infinity = encode("Infinity");
    let nan = encode("NaN");
    let smallest = encode(&format!("-0.9e{MAX_NORMALIZED_EXPONENT}"));
    let biggest = encode(&format!("0.9e{MAX_NORMALIZED_EXPONENT}"));

    assert!(negative_infinity < smallest);
    assert!(biggest < positive_infinity);
    assert!(positive_infinity < nan);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_048))]

    #[test]
    fn byte_order_matches_mathematical_order(a in value_strategy(), b in value_strategy()) {
        prop_assert_eq!(a.encode().cmp(&b.encode()), a.cmp_mathematically(&b));
    }
}

#[test]
fn widest_postgres_values_round_trip() {
    let integer: String = (0..131_072)
        .map(|index| (b'1' + (index % 9) as u8) as char)
        .collect();
    let mut full = integer.clone();
    full.push('.');
    full.extend((0..16_383).map(|index| (b'1' + (index % 9) as u8) as char));

    for value in [&integer, &full] {
        assert_eq!(Decimal::from_str(value).unwrap().to_string(), *value);
    }
}

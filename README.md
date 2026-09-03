# decimal-bytes

[![Crates.io](https://img.shields.io/crates/v/decimal-bytes.svg)](https://crates.io/crates/decimal-bytes)
[![codecov](https://codecov.io/gh/paradedb/decimal-bytes/graph/badge.svg)](https://codecov.io/gh/paradedb/decimal-bytes)
[![CI](https://github.com/paradedb/decimal-bytes/actions/workflows/ci.yml/badge.svg)](https://github.com/paradedb/decimal-bytes/actions/workflows/ci.yml)
[![Documentation](https://docs.rs/decimal-bytes/badge.svg)](https://docs.rs/decimal-bytes)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)

Arbitrary precision decimals with lexicographically sortable byte encoding.

## Overview

This crate provides three decimal types optimized for database storage:

- **`Decimal`**: Variable-length arbitrary precision with an exponent range of
  -16,383 through 131,072
- **`Decimal64`**: Fixed 8-byte representation with embedded scale (precision ≤ 16 digits)
- **`Decimal64NoScale`**: Fixed 8-byte representation with external scale (precision ≤ 18 digits)

All types support PostgreSQL special values (NaN, ±Infinity) with correct sort ordering.

**Why not use `rust_decimal` or `bigdecimal`?** Those libraries are excellent for arithmetic, but their byte representations are not lexicographically sortable. You cannot compare their serialized bytes to determine numerical order - you must deserialize first. `decimal-bytes` solves this by providing a byte encoding where `bytes(a) < bytes(b)` if and only if `a < b` numerically.

## When to Use Which

| Type | Precision | Scale / exponent range | Storage | Best For |
|------|-----------|-----------------------|---------|----------|
| `Decimal64NoScale` | ≤ **18** digits | External scale | 8 bytes | **Columnar storage, aggregates** |
| `Decimal64` | ≤ 16 digits | Embedded scale | 8 bytes | Self-contained values |
| `Decimal` | Unlimited mantissa | -16,383 through 131,072 | Variable | Scientific, very large numbers |

## Features

- **Three storage options**: Fixed 8-byte (`Decimal64`, `Decimal64NoScale`) or variable-length (`Decimal`)
- **Columnar-friendly**: `Decimal64NoScale` enables correct aggregates with external scale
- **Lexicographic ordering**: Byte comparison matches numerical comparison
- **PostgreSQL NUMERIC compatibility**: Full support for precision, scale (including negative), and special values
- **Special values**: Infinity, -Infinity, and NaN with correct PostgreSQL sort order

## Decimal64 Usage

For most financial and business applications where precision ≤ 16 digits:

```rust
use decimal_bytes::Decimal64;

// Create with scale
let price = Decimal64::new("99.99", 2).unwrap();
assert_eq!(price.to_string(), "99.99");
assert_eq!(price.scale(), 2);

// Parse with automatic scale detection
let d: Decimal64 = "123.456".parse().unwrap();
assert_eq!(d.scale(), 3);

// Access raw components
let value = price.value();  // 9999 (scaled integer)
let scale = price.scale();  // 2

// Special values (PostgreSQL compatible)
let inf = Decimal64::infinity();
let neg_inf = Decimal64::neg_infinity();
let nan = Decimal64::nan();

// Correct sort order: -Infinity < numbers < +Infinity < NaN
assert!(neg_inf < price);
assert!(price < inf);
assert!(inf < nan);

// NaN equals NaN (PostgreSQL semantics)
assert_eq!(nan, Decimal64::nan());
```

### Decimal64 with Precision and Scale (PostgreSQL NUMERIC)

`Decimal64` fully supports PostgreSQL's `NUMERIC(precision, scale)` semantics:

```rust
use decimal_bytes::Decimal64;

// NUMERIC(5, 2) - up to 5 digits total, 2 after decimal
let d = Decimal64::with_precision_scale("123.456", Some(5), Some(2)).unwrap();
assert_eq!(d.to_string(), "123.46"); // Rounded to 2 decimal places

// Precision overflow - truncates from left (PostgreSQL behavior)
let d = Decimal64::with_precision_scale("12345.67", Some(5), Some(2)).unwrap();
assert_eq!(d.to_string(), "345.67"); // Keeps rightmost 5 digits

// NUMERIC(2, -3) - negative scale rounds to powers of 10
let d = Decimal64::with_precision_scale("12345", Some(2), Some(-3)).unwrap();
assert_eq!(d.to_string(), "12000"); // Rounded to nearest 1000
```

### Decimal64 Storage Layout

```text
64-bit packed representation:
┌──────────────────┬─────────────────────────────────────────────────────┐
│ Scale (8 bits)   │ Value (56 bits, signed)                             │
│ Byte 0           │ Bytes 1-7                                           │
└──────────────────┴─────────────────────────────────────────────────────┘
```

- **Scale byte**: 0-18 for normal values, 253/254/255 for -Infinity/+Infinity/NaN
- **Value**: 56-bit signed integer (-2^55 to 2^55-1, ~16 significant digits)

### Decimal64 Benefits

- **Fixed 8 bytes**: Predictable storage, no heap allocation, cache-friendly
- **PostgreSQL compatible**: Full NUMERIC(p,s) semantics including NaN, ±Infinity
- **Fast operations**: Single i64 comparison and serialization

## Decimal64NoScale Usage (Recommended for Columnar Storage)

`Decimal64NoScale` stores the raw scaled value without embedding the scale, enabling:
- **18 digits of precision** (vs 16 for Decimal64)
- **Correct aggregates** (SUM, MIN, MAX work directly on raw i64 values)
- **Columnar storage compatibility** (scale stored once in schema metadata)

```rust
use decimal_bytes::Decimal64NoScale;

// Scale is provided externally (e.g., from schema metadata)
let scale = 2;
let a = Decimal64NoScale::new("100.50", scale).unwrap();
let b = Decimal64NoScale::new("200.25", scale).unwrap();

// Raw values can be summed directly!
let sum = a.value() + b.value();  // 30075
assert_eq!(sum, 30075);

// Interpret result with scale
let result = Decimal64NoScale::from_raw(sum);
assert_eq!(result.to_string_with_scale(scale), "300.75");

// 18 digits supported (more than Decimal64's 16)
let big = Decimal64NoScale::new("123456789012345678", 0).unwrap();
assert_eq!(big.value(), 123456789012345678);
```

### Why Decimal64NoScale for Aggregates?

`Decimal64` embeds scale in the i64, which **corrupts aggregate results**:

```text
Decimal64:        packed = (scale << 56) | mantissa
                  SUM(a, b) = adds scale bits → WRONG!

Decimal64NoScale: stored = value * 10^scale
                  SUM(a, b) = (a+b)*scale → divide by scale → CORRECT!
```

### Decimal64NoScale Storage Layout

```text
64-bit representation:
┌─────────────────────────────────────────────────────────────────┐
│ Value (64 bits, signed) - represents value * 10^scale           │
└─────────────────────────────────────────────────────────────────┘
```

- **Value**: Full 64-bit signed integer (±9.99×10^17, ~18 significant digits)
- **Scale**: Stored externally (e.g., in database schema)
- **Special values**: `i64::MIN` (NaN), `i64::MIN+1` (-Infinity), `i64::MAX` (+Infinity)

## Decimal Usage (Arbitrary Precision)

```rust
use decimal_bytes::Decimal;

// Create decimals from strings
let a = Decimal::from_str("123.456").unwrap();
let b = Decimal::from_str("123.457").unwrap();

// Byte comparison matches numerical comparison
assert!(a.as_bytes() < b.as_bytes());
assert!(a < b);

// With precision and scale constraints (SQL NUMERIC semantics)
let d = Decimal::with_precision_scale("123.456", Some(10), Some(2)).unwrap();
assert_eq!(d.to_string(), "123.46"); // Rounded to 2 decimal places

// Negative scale (rounds to left of decimal point)
let d = Decimal::with_precision_scale("12345", Some(10), Some(-3)).unwrap();
assert_eq!(d.to_string(), "12000"); // Rounded to nearest 1000

// Efficient byte access (primary representation)
let bytes: &[u8] = d.as_bytes();

// Reconstruct from bytes
let restored = Decimal::from_bytes(bytes).unwrap();
assert_eq!(d, restored);
```

## Special Values

PostgreSQL-compatible special values with correct sort ordering:

```rust
use decimal_bytes::Decimal;

// Create special values
let pos_inf = Decimal::infinity();
let neg_inf = Decimal::neg_infinity();
let nan = Decimal::nan();

// Or parse from strings (case-insensitive)
let inf = Decimal::from_str("Infinity").unwrap();
let inf = Decimal::from_str("inf").unwrap();
let nan = Decimal::from_str("NaN").unwrap();

// Check for special values
assert!(pos_inf.is_infinity());
assert!(pos_inf.is_pos_infinity());
assert!(neg_inf.is_neg_infinity());
assert!(nan.is_nan());
assert!(!pos_inf.is_finite());

// Sort order: -Infinity < negatives < zero < positives < Infinity < NaN
assert!(neg_inf < Decimal::from_str("-1000000").unwrap());
assert!(Decimal::from_str("1000000").unwrap() < pos_inf);
assert!(pos_inf < nan);
```

### PostgreSQL vs IEEE 754 Semantics

This library follows **PostgreSQL semantics** for special values, which differ from IEEE 754 floating-point:

| Behavior | PostgreSQL / decimal-bytes | IEEE 754 float |
|----------|---------------------------|----------------|
| `NaN == NaN` | `true` | `false` |
| `NaN` ordering | Greatest value (> Infinity) | Unordered |
| `Infinity == Infinity` | `true` | `true` |

```rust
use decimal_bytes::Decimal;

let nan1 = Decimal::nan();
let nan2 = Decimal::nan();
let inf = Decimal::infinity();

// NaN equals itself (PostgreSQL behavior, unlike IEEE 754)
assert_eq!(nan1, nan2);

// NaN is greater than everything, including Infinity
assert!(nan1 > inf);
```

This makes `Decimal` suitable for use in indexes, sorting, and deduplication where consistent ordering and equality semantics are required.

## PostgreSQL Compatibility

This crate implements the PostgreSQL NUMERIC specification:

| Feature | Support |
|---------|---------|
| Max digits before decimal | 131,072 |
| Max digits after decimal | 16,383 |
| Precision constraint | ✓ |
| Scale constraint (positive) | ✓ |
| Scale constraint (negative) | ✓ |
| Infinity | ✓ |
| -Infinity | ✓ |
| NaN | ✓ |
| Rounding (ties away from zero) | ✓ |

## Storage Efficiency

The encoding matches PostgreSQL's storage efficiency (2 bytes per 4 decimal digits):

- 1 byte for sign
- 2 bytes for an inline exponent, or 6 bytes for an escaped exponent
- ~N/2 bytes for N-digit mantissa (BCD encoding: 2 digits per byte), plus 1 terminator byte for negative numbers
- Special values: 3 bytes each

Inline exponents use a biased 2-byte field. Exponents above `49,148` use the
reserved `0xFFFD` escape marker followed by a 4-byte exponent; `0xFFFE` and
`0xFFFF` are reserved for `+Infinity` and `NaN`.

Example: A 9-digit number like `123456789` requires only ~8 bytes total.

## Sort Order

The lexicographic byte order matches the PostgreSQL NUMERIC sort order:

```
-Infinity < negative numbers < zero < positive numbers < +Infinity < NaN
```

This enables efficient range queries in sorted key-value stores without decoding.

## Performance

### Type Comparison Summary

| Type | Max Precision | Parse | Aggregates | Best For |
|------|---------------|-------|------------|----------|
| `Decimal64NoScale` | **18 digits** | ~85 µs/1000 | **✓ Correct, 17 Gelem/s** | Columnar storage |
| `Decimal64` | 16 digits | ~136 µs/1000 | ✗ Wrong (scale corrupts) | Self-contained values |
| `Decimal` | Unlimited | ~134 µs/1000 | N/A | Arbitrary precision |

### Memory Usage

| Type | Stack | Heap | Total |
|------|-------|------|-------|
| Decimal64NoScale | 8 bytes | 0 | **8 bytes** |
| Decimal64 | 8 bytes | 0 | **8 bytes** |
| Decimal | 24 bytes | ~9 bytes | ~33 bytes |

### Decimal64NoScale Operations (Recommended for Columnar)

| Operation | Time | Notes |
|-----------|------|-------|
| Parse (`new`) | 60-85 ns | Scales with digit count |
| `to_string_with_scale()` | 18-25 ns | Scales with digit count |
| `from_raw()` | **<1 ns** | Trivial (just wrap i64) |
| Equality (`==`) | **<1 ns** | Direct i64 comparison |
| SUM 1000 values | **~59 ns** | 17 Gelem/s - just sum raw i64s |
| MIN/MAX 1000 values | **~230 ns** | 4.3 Gelem/s - direct comparison |
| `to_be_bytes()` | <1 ns | Trivial conversion |
| `from_be_bytes()` | <1 ns | Trivial conversion |

### Decimal64 Operations

| Operation | Time | Notes |
|-----------|------|-------|
| Parse (`new`) | 64-71 ns | Scales with digit count |
| `to_string()` | 19-88 ns | Scales with digit count |
| Equality (`==`) | 0.5 ns | Single i64 comparison |
| Comparison (same scale) | 1.6 ns | Direct value comparison |
| Comparison (diff scale) | 2 ns | Requires normalization |
| `to_be_bytes()` | 0.9 ns | Trivial conversion |
| `from_be_bytes()` | 0.8 ns | Trivial conversion |
| `is_nan()` / `is_infinity()` | 0.3 ns | Fast special value checks |

### Decimal Operations (Arbitrary Precision)

| Operation | Time | Notes |
|-----------|------|-------|
| Byte comparison | ~4 ns | The key use case - compare without decoding |
| `from_str` (parse) | 84-312 ns | Scales with digit count |
| `to_string` | 61-89 ns | Scales with digit count |
| `from_bytes` | 58-261 ns | With validation |
| `from_bytes_unchecked` | ~15 ns | Skip validation if bytes are trusted |
| `is_nan()` / `is_infinity()` | ~1.3 ns | Fast special value checks |

### Aggregate Performance (Key Differentiator)

For columnar storage where aggregates are important:

| Operation | Decimal64NoScale | Decimal64 | Speedup |
|-----------|------------------|-----------|---------|
| SUM 1000 values | **59 ns** (17 Gelem/s) | 275 ns (3.6 Gelem/s) | **4.7x** |
| MIN/MAX 1000 values | **230 ns** (4.3 Gelem/s) | 1001 ns (1 Gelem/s) | **4.3x** |
| Create 1000 values | **85 µs** | 136 µs | **1.6x** |
| Results correct? | **✓ Yes** | **✗ No** | - |

**Why is Decimal64NoScale faster?**
- `Decimal64NoScale.value()` returns raw i64 directly
- `Decimal64.value()` must unpack/mask the 56-bit value from the packed format

Run `cargo bench` locally to reproduce benchmarks on your hardware.

## Arithmetic Operations

This library focuses on storage and comparison, not arithmetic. Existing Rust decimal libraries (`rust_decimal`, `bigdecimal`) provide arithmetic but their byte representations are **not lexicographically sortable** - you cannot compare their serialized bytes to determine numerical order. That's the gap `decimal-bytes` fills: efficient storage with byte-level ordering for databases and search engines.

For calculations, use an established decimal library and convert:

### With `rust_decimal` (recommended for most use cases)

```toml
[dependencies]
decimal-bytes = { version = "0.1", features = ["rust_decimal"] }
```

```rust
use rust_decimal::Decimal as RustDecimal;
use decimal_bytes::Decimal;

// Convert from rust_decimal for storage
let rd = RustDecimal::new(12345, 2); // 123.45
let stored: Decimal = rd.try_into().unwrap();

// Do arithmetic with rust_decimal
let a: RustDecimal = (&stored).try_into().unwrap();
let b = RustDecimal::new(1000, 2); // 10.00
let sum = a + b; // 133.45

// Convert back for storage
let result: Decimal = sum.try_into().unwrap();
```

### With `bigdecimal` (for arbitrary precision arithmetic)

```toml
[dependencies]
decimal-bytes = { version = "0.1", features = ["bigdecimal"] }
```

```rust
use bigdecimal::BigDecimal;
use decimal_bytes::Decimal;
use std::str::FromStr;

// Convert between types
let bd = BigDecimal::from_str("123.456789012345678901234567890").unwrap();
let stored: Decimal = bd.try_into().unwrap();
let restored: BigDecimal = (&stored).try_into().unwrap();
```

## License

MIT License - see [LICENSE](LICENSE) for details.

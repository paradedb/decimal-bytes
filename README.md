# decimal-bytes

[![Crates.io](https://img.shields.io/crates/v/decimal-bytes.svg)](https://crates.io/crates/decimal-bytes)
[![codecov](https://codecov.io/gh/paradedb/decimal-bytes/graph/badge.svg)](https://codecov.io/gh/paradedb/decimal-bytes)
[![CI](https://github.com/paradedb/decimal-bytes/actions/workflows/ci.yml/badge.svg)](https://github.com/paradedb/decimal-bytes/actions/workflows/ci.yml)
[![Documentation](https://docs.rs/decimal-bytes/badge.svg)](https://docs.rs/decimal-bytes)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)

Arbitrary precision decimals with lexicographically sortable byte encoding for
database indexes and sorted key-value stores. Compare encoded `Decimal` values
without decoding them first.

[API documentation](https://docs.rs/decimal-bytes)

## Usage

Add the crate to your `Cargo.toml` (requires Rust 1.85 or later):

```toml
[dependencies]
decimal-bytes = "0.6"
```

```rust
use decimal_bytes::Decimal;

let a: Decimal = "123.456".parse().unwrap();
let b: Decimal = "123.457".parse().unwrap();

assert!(a.as_bytes() < b.as_bytes());

let restored = Decimal::from_bytes(a.as_bytes()).unwrap();
assert_eq!(a, restored);
```

## Decimal types

| Type | Precision | Storage | Scale |
| --- | --- | --- | --- |
| `Decimal` | Arbitrary | Variable length | Encoded in the value |
| `Decimal64` | Up to 16 digits | 8 bytes | Embedded |
| `Decimal64NoScale` | Up to 18 digits | 8 bytes | Supplied externally |

Use `Decimal` for byte-level ordering. The fixed-width types provide compact
storage for bounded precision; `Decimal64NoScale` lets a column share one scale.

All three types support NaN and positive and negative infinity. `Decimal` and
`Decimal64` follow PostgreSQL ordering: NaN compares equal to itself and sorts
after positive infinity.

For arithmetic, the optional `rust_decimal` and `bigdecimal` features provide
conversions to and from those libraries. See the [API documentation](https://docs.rs/decimal-bytes)
for precision and scale constraints, encoding details, and conversion examples.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and checks.

## License

[MIT](LICENSE).

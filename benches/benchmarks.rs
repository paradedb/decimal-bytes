use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use decimal_bytes::{Decimal, Decimal64, Decimal64NoScale};
use std::str::FromStr;

/// Sample decimal strings of varying complexity
const SMALL_INT: &str = "42";
const MEDIUM_INT: &str = "123456789";
const LARGE_INT: &str = "123456789012345678901234567890";
const SMALL_DECIMAL: &str = "3.14";
const MEDIUM_DECIMAL: &str = "123456.789012";
const LARGE_DECIMAL: &str = "123456789.012345678901234567890123456789";
const SCIENTIFIC: &str = "1.23456789e15";
const NEGATIVE: &str = "-987654321.123456789";

// Values that fit in Decimal64 (≤16 digits)
const D64_SMALL_INT: &str = "42";
const D64_MEDIUM_INT: &str = "123456789";
const D64_SMALL_DECIMAL: &str = "3.14";
const D64_MEDIUM_DECIMAL: &str = "123456.789012";
const D64_FINANCIAL: &str = "9999999999.99"; // 12 digits, typical financial
const D64_MAX_PRECISION: &str = "1234567890123456"; // 16 digits (max for Decimal64)

// Values that fit in Decimal64NoScale (≤18 digits)
const D64NS_18_DIGITS: &str = "123456789012345678"; // 18 digits (max for Decimal64NoScale)
const D64NS_17_DIGITS: &str = "12345678901234567.8"; // 17+1 digits with decimal

fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse");

    let cases = [
        ("small_int", SMALL_INT),
        ("medium_int", MEDIUM_INT),
        ("large_int", LARGE_INT),
        ("small_decimal", SMALL_DECIMAL),
        ("medium_decimal", MEDIUM_DECIMAL),
        ("large_decimal", LARGE_DECIMAL),
        ("scientific", SCIENTIFIC),
        ("negative", NEGATIVE),
    ];

    for (name, input) in cases {
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(BenchmarkId::new("from_str", name), input, |b, s| {
            b.iter(|| Decimal::from_str(black_box(s)).unwrap())
        });
    }

    group.finish();
}

fn bench_to_string(c: &mut Criterion) {
    let mut group = c.benchmark_group("to_string");

    let cases = [
        ("small_int", SMALL_INT),
        ("medium_int", MEDIUM_INT),
        ("large_int", LARGE_INT),
        ("small_decimal", SMALL_DECIMAL),
        ("medium_decimal", MEDIUM_DECIMAL),
        ("large_decimal", LARGE_DECIMAL),
        ("negative", NEGATIVE),
    ];

    for (name, input) in cases {
        let decimal = Decimal::from_str(input).unwrap();
        group.bench_with_input(BenchmarkId::new("to_string", name), &decimal, |b, d| {
            b.iter(|| black_box(d).to_string())
        });
    }

    group.finish();
}

fn bench_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("comparison");

    let a = Decimal::from_str("123456.789").unwrap();
    let b = Decimal::from_str("123456.790").unwrap();
    let c_val = Decimal::from_str("123456.789").unwrap();

    group.bench_function("cmp_less", |bench| {
        bench.iter(|| black_box(&a) < black_box(&b))
    });

    group.bench_function("cmp_equal", |bench| {
        bench.iter(|| black_box(&a) == black_box(&c_val))
    });

    // Compare bytes directly (the key use case)
    group.bench_function("cmp_bytes", |bench| {
        bench.iter(|| black_box(a.as_bytes()) < black_box(b.as_bytes()))
    });

    group.finish();
}

fn bench_special_values(c: &mut Criterion) {
    let mut group = c.benchmark_group("special_values");

    group.bench_function("create_infinity", |b| b.iter(|| Decimal::infinity()));

    group.bench_function("create_nan", |b| b.iter(|| Decimal::nan()));

    group.bench_function("parse_infinity", |b| {
        b.iter(|| Decimal::from_str(black_box("Infinity")).unwrap())
    });

    group.bench_function("parse_nan", |b| {
        b.iter(|| Decimal::from_str(black_box("NaN")).unwrap())
    });

    let inf = Decimal::infinity();
    let nan = Decimal::nan();

    group.bench_function("is_infinity", |b| b.iter(|| black_box(&inf).is_infinity()));

    group.bench_function("is_nan", |b| b.iter(|| black_box(&nan).is_nan()));

    group.finish();
}

fn bench_precision_scale(c: &mut Criterion) {
    let mut group = c.benchmark_group("precision_scale");

    group.bench_function("with_precision_scale", |b| {
        b.iter(|| {
            Decimal::with_precision_scale(black_box("123.456789"), Some(10), Some(2)).unwrap()
        })
    });

    group.bench_function("negative_scale", |b| {
        b.iter(|| Decimal::with_precision_scale(black_box("123456"), Some(10), Some(-3)).unwrap())
    });

    group.finish();
}

fn bench_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialization");

    let decimal = Decimal::from_str("123456.789012345").unwrap();
    let json = serde_json::to_string(&decimal).unwrap();

    group.bench_function("serialize_json", |b| {
        b.iter(|| serde_json::to_string(black_box(&decimal)).unwrap())
    });

    group.bench_function("deserialize_json", |b| {
        b.iter(|| serde_json::from_str::<Decimal>(black_box(&json)).unwrap())
    });

    group.finish();
}

fn bench_from_bytes(c: &mut Criterion) {
    let mut group = c.benchmark_group("from_bytes");

    let cases = [
        ("small", SMALL_INT),
        ("medium", MEDIUM_DECIMAL),
        ("large", LARGE_DECIMAL),
    ];

    for (name, input) in cases {
        let decimal = Decimal::from_str(input).unwrap();
        let bytes = decimal.as_bytes().to_vec();

        group.bench_with_input(BenchmarkId::new("from_bytes", name), &bytes, |b, bytes| {
            b.iter(|| Decimal::from_bytes(black_box(bytes)).unwrap())
        });

        group.bench_with_input(
            BenchmarkId::new("from_bytes_unchecked", name),
            &bytes,
            |b, bytes| b.iter(|| Decimal::from_bytes_unchecked(black_box(bytes.clone()))),
        );
    }

    group.finish();
}

fn bench_batch_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch");

    // Simulate sorting a batch of decimals (common database operation)
    let inputs: Vec<&str> = vec![
        "100.5", "-50.25", "0", "999.999", "-0.001", "1e10", "42", "-1e5", "3.14159", "2.71828",
    ];

    let decimals: Vec<Decimal> = inputs
        .iter()
        .map(|s| Decimal::from_str(s).unwrap())
        .collect();

    group.bench_function("sort_10_decimals", |b| {
        b.iter(|| {
            let mut d = decimals.clone();
            d.sort();
            black_box(d)
        })
    });

    // Batch parsing
    group.throughput(Throughput::Elements(inputs.len() as u64));
    group.bench_function("parse_10_decimals", |b| {
        b.iter(|| {
            inputs
                .iter()
                .map(|s| Decimal::from_str(black_box(s)).unwrap())
                .collect::<Vec<_>>()
        })
    });

    group.finish();
}

// ==================== Decimal64 Benchmarks ====================

fn bench_decimal64_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64_parse");

    let cases = [
        ("small_int", D64_SMALL_INT, 0u8),
        ("medium_int", D64_MEDIUM_INT, 0),
        ("small_decimal", D64_SMALL_DECIMAL, 2),
        ("medium_decimal", D64_MEDIUM_DECIMAL, 6),
        ("financial", D64_FINANCIAL, 2),
        ("max_precision", D64_MAX_PRECISION, 0),
    ];

    for (name, input, scale) in cases {
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("new", name),
            &(input, scale),
            |b, (s, sc)| b.iter(|| Decimal64::new(black_box(s), *sc).unwrap()),
        );
    }

    // Auto-detect scale
    group.bench_function("from_str_auto", |b| {
        b.iter(|| Decimal64::from_str(black_box("123456.789012")).unwrap())
    });

    group.finish();
}

fn bench_decimal64_to_string(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64_to_string");

    let cases = [
        ("small_int", D64_SMALL_INT, 0u8),
        ("medium_int", D64_MEDIUM_INT, 0),
        ("small_decimal", D64_SMALL_DECIMAL, 2),
        ("medium_decimal", D64_MEDIUM_DECIMAL, 6),
        ("financial", D64_FINANCIAL, 2),
    ];

    for (name, input, scale) in cases {
        let d64 = Decimal64::new(input, scale).unwrap();
        group.bench_with_input(BenchmarkId::new("to_string", name), &d64, |b, d| {
            b.iter(|| black_box(d).to_string())
        });
    }

    group.finish();
}

fn bench_decimal64_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64_comparison");

    let a = Decimal64::new("123456.789", 3).unwrap();
    let b = Decimal64::new("123456.790", 3).unwrap();
    let c_val = Decimal64::new("123456.789", 3).unwrap();

    group.bench_function("cmp_less", |bench| {
        bench.iter(|| black_box(a) < black_box(b))
    });

    group.bench_function("cmp_equal", |bench| {
        bench.iter(|| black_box(a) == black_box(c_val))
    });

    // Compare raw packed values (single i64 comparison)
    group.bench_function("cmp_raw", |bench| {
        bench.iter(|| black_box(a.raw()) < black_box(b.raw()))
    });

    // Different scales (requires normalization)
    let d1 = Decimal64::new("1.5", 1).unwrap();
    let d2 = Decimal64::new("1.50", 2).unwrap();
    group.bench_function("cmp_diff_scale", |bench| {
        bench.iter(|| black_box(d1).cmp(&black_box(d2)))
    });

    group.finish();
}

fn bench_decimal64_special(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64_special");

    group.bench_function("create_infinity", |b| b.iter(|| Decimal64::infinity()));
    group.bench_function("create_nan", |b| b.iter(|| Decimal64::nan()));

    group.bench_function("parse_infinity", |b| {
        b.iter(|| Decimal64::from_str(black_box("Infinity")).unwrap())
    });

    let inf = Decimal64::infinity();
    let nan = Decimal64::nan();

    group.bench_function("is_infinity", |b| b.iter(|| black_box(inf).is_infinity()));
    group.bench_function("is_nan", |b| b.iter(|| black_box(nan).is_nan()));
    group.bench_function("is_finite", |b| {
        let d = Decimal64::new("123.45", 2).unwrap();
        b.iter(|| black_box(d).is_finite())
    });

    group.finish();
}

fn bench_decimal64_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64_serialization");

    let d64 = Decimal64::new("123456.789012", 6).unwrap();

    group.bench_function("to_be_bytes", |b| b.iter(|| black_box(d64).to_be_bytes()));

    let bytes = d64.to_be_bytes();
    group.bench_function("from_be_bytes", |b| {
        b.iter(|| Decimal64::from_be_bytes(black_box(bytes)))
    });

    // JSON serialization
    let json = serde_json::to_string(&d64).unwrap();
    group.bench_function("serialize_json", |b| {
        b.iter(|| serde_json::to_string(black_box(&d64)).unwrap())
    });

    group.bench_function("deserialize_json", |b| {
        b.iter(|| serde_json::from_str::<Decimal64>(black_box(&json)).unwrap())
    });

    group.finish();
}

fn bench_decimal64_precision_scale(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64_precision_scale");

    group.bench_function("with_precision_scale", |b| {
        b.iter(|| {
            Decimal64::with_precision_scale(black_box("123.456789"), Some(10), Some(2)).unwrap()
        })
    });

    group.bench_function("negative_scale", |b| {
        b.iter(|| Decimal64::with_precision_scale(black_box("123456"), Some(10), Some(-3)).unwrap())
    });

    group.bench_function("from_parts", |b| {
        b.iter(|| Decimal64::from_parts(black_box(12345678), black_box(2)).unwrap())
    });

    group.finish();
}

// ==================== Decimal vs Decimal64 Comparison ====================

fn bench_comparison_decimal_vs_decimal64(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal_vs_decimal64");

    // Values that fit in both types (≤16 digits)
    let test_values = [
        ("small_int", "42", 0u8),
        ("medium_decimal", "123456.789012", 6),
        ("financial", "9999999999.99", 2),
        ("max_d64_precision", "1234567890123456", 0),
    ];

    for (name, value, scale) in test_values {
        // Parse benchmarks
        group.bench_with_input(BenchmarkId::new("parse/Decimal", name), value, |b, s| {
            b.iter(|| Decimal::from_str(black_box(s)).unwrap())
        });

        group.bench_with_input(
            BenchmarkId::new("parse/Decimal64", name),
            &(value, scale),
            |b, (s, sc)| b.iter(|| Decimal64::new(black_box(s), *sc).unwrap()),
        );

        // to_string benchmarks
        let decimal = Decimal::from_str(value).unwrap();
        let decimal64 = Decimal64::new(value, scale).unwrap();

        group.bench_with_input(
            BenchmarkId::new("to_string/Decimal", name),
            &decimal,
            |b, d| b.iter(|| black_box(d).to_string()),
        );

        group.bench_with_input(
            BenchmarkId::new("to_string/Decimal64", name),
            &decimal64,
            |b, d| b.iter(|| black_box(d).to_string()),
        );
    }

    group.finish();
}

fn bench_batch_decimal_vs_decimal64(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_decimal_vs_decimal64");

    // Financial data simulation (all values fit in Decimal64)
    let financial_inputs: Vec<(&str, u8)> = vec![
        ("100.50", 2),
        ("-50.25", 2),
        ("0.00", 2),
        ("999.99", 2),
        ("-0.01", 2),
        ("10000.00", 2),
        ("42.00", 2),
        ("-100.00", 2),
        ("3.14", 2),
        ("2.71", 2),
    ];

    let financial_strs: Vec<&str> = financial_inputs.iter().map(|(s, _)| *s).collect();

    // Batch parsing
    group.throughput(Throughput::Elements(10));

    group.bench_function("parse_10/Decimal", |b| {
        b.iter(|| {
            financial_strs
                .iter()
                .map(|s| Decimal::from_str(black_box(s)).unwrap())
                .collect::<Vec<_>>()
        })
    });

    group.bench_function("parse_10/Decimal64", |b| {
        b.iter(|| {
            financial_inputs
                .iter()
                .map(|(s, scale)| Decimal64::new(black_box(s), *scale).unwrap())
                .collect::<Vec<_>>()
        })
    });

    // Sorting
    let decimals: Vec<Decimal> = financial_strs
        .iter()
        .map(|s| Decimal::from_str(s).unwrap())
        .collect();

    let decimal64s: Vec<Decimal64> = financial_inputs
        .iter()
        .map(|(s, scale)| Decimal64::new(s, *scale).unwrap())
        .collect();

    group.bench_function("sort_10/Decimal", |b| {
        b.iter(|| {
            let mut d = decimals.clone();
            d.sort();
            black_box(d)
        })
    });

    group.bench_function("sort_10/Decimal64", |b| {
        b.iter(|| {
            let mut d = decimal64s.clone();
            d.sort();
            black_box(d)
        })
    });

    group.finish();
}

// ==================== Decimal64NoScale Benchmarks ====================

fn bench_decimal64ns_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64ns_parse");

    let cases = [
        ("small_int", D64_SMALL_INT, 0i32),
        ("medium_int", D64_MEDIUM_INT, 0),
        ("small_decimal", D64_SMALL_DECIMAL, 2),
        ("medium_decimal", D64_MEDIUM_DECIMAL, 6),
        ("financial", D64_FINANCIAL, 2),
        ("16_digits", D64_MAX_PRECISION, 0),
        ("18_digits", D64NS_18_DIGITS, 0),
        ("17+1_decimal", D64NS_17_DIGITS, 1),
    ];

    for (name, input, scale) in cases {
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("new", name),
            &(input, scale),
            |b, (s, sc)| b.iter(|| Decimal64NoScale::new(black_box(s), *sc).unwrap()),
        );
    }

    group.finish();
}

fn bench_decimal64ns_to_string(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64ns_to_string");

    let cases = [
        ("small_int", D64_SMALL_INT, 0i32),
        ("medium_int", D64_MEDIUM_INT, 0),
        ("small_decimal", D64_SMALL_DECIMAL, 2),
        ("medium_decimal", D64_MEDIUM_DECIMAL, 6),
        ("financial", D64_FINANCIAL, 2),
        ("18_digits", D64NS_18_DIGITS, 0),
    ];

    for (name, input, scale) in cases {
        let d64ns = Decimal64NoScale::new(input, scale).unwrap();
        group.bench_with_input(
            BenchmarkId::new("to_string_with_scale", name),
            &(d64ns, scale),
            |b, (d, s)| b.iter(|| black_box(d).to_string_with_scale(*s)),
        );
    }

    group.finish();
}

fn bench_decimal64ns_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64ns_operations");

    let a = Decimal64NoScale::new("123456.789", 3).unwrap();
    let b = Decimal64NoScale::new("123456.790", 3).unwrap();
    let c_val = Decimal64NoScale::new("123456.789", 3).unwrap();

    group.bench_function("cmp_less", |bench| {
        bench.iter(|| black_box(a) < black_box(b))
    });

    group.bench_function("cmp_equal", |bench| {
        bench.iter(|| black_box(a) == black_box(c_val))
    });

    // Compare raw values (key use case for aggregates)
    group.bench_function("cmp_raw", |bench| {
        bench.iter(|| black_box(a.value()) < black_box(b.value()))
    });

    // Aggregate simulation (SUM)
    let values: Vec<Decimal64NoScale> = (0..100)
        .map(|i| Decimal64NoScale::new(&format!("{}.99", i), 2).unwrap())
        .collect();

    group.bench_function("sum_100_values", |bench| {
        bench.iter(|| {
            let sum: i64 = values.iter().map(|d| d.value()).sum();
            black_box(sum)
        })
    });

    group.bench_function("min_100_values", |bench| {
        bench.iter(|| {
            let min = values.iter().map(|d| d.value()).min().unwrap();
            black_box(min)
        })
    });

    group.bench_function("max_100_values", |bench| {
        bench.iter(|| {
            let max = values.iter().map(|d| d.value()).max().unwrap();
            black_box(max)
        })
    });

    group.finish();
}

fn bench_decimal64ns_special(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64ns_special");

    group.bench_function("create_infinity", |b| {
        b.iter(|| Decimal64NoScale::infinity())
    });
    group.bench_function("create_nan", |b| b.iter(|| Decimal64NoScale::nan()));

    let inf = Decimal64NoScale::infinity();
    let nan = Decimal64NoScale::nan();

    group.bench_function("is_infinity", |b| b.iter(|| black_box(inf).is_infinity()));
    group.bench_function("is_nan", |b| b.iter(|| black_box(nan).is_nan()));
    group.bench_function("is_finite", |b| {
        let d = Decimal64NoScale::new("123.45", 2).unwrap();
        b.iter(|| black_box(d).is_finite())
    });

    group.finish();
}

fn bench_decimal64ns_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64ns_serialization");

    let d64ns = Decimal64NoScale::new("123456.789012", 6).unwrap();

    group.bench_function("to_be_bytes", |b| b.iter(|| black_box(d64ns).to_be_bytes()));

    let bytes = d64ns.to_be_bytes();
    group.bench_function("from_be_bytes", |b| {
        b.iter(|| Decimal64NoScale::from_be_bytes(black_box(bytes)))
    });

    // From raw (common for columnar storage)
    let raw = d64ns.value();
    group.bench_function("from_raw", |b| {
        b.iter(|| Decimal64NoScale::from_raw(black_box(raw)))
    });

    // JSON serialization
    let json = serde_json::to_string(&d64ns).unwrap();
    group.bench_function("serialize_json", |b| {
        b.iter(|| serde_json::to_string(black_box(&d64ns)).unwrap())
    });

    group.bench_function("deserialize_json", |b| {
        b.iter(|| serde_json::from_str::<Decimal64NoScale>(black_box(&json)).unwrap())
    });

    group.finish();
}

// ==================== Decimal64 vs Decimal64NoScale Comparison ====================

fn bench_decimal64_vs_decimal64ns(c: &mut Criterion) {
    let mut group = c.benchmark_group("decimal64_vs_decimal64ns");

    // Values that fit in both types (≤16 digits)
    let test_values = [
        ("small_int", "42", 0i32),
        ("medium_decimal", "123456.789012", 6),
        ("financial", "9999999999.99", 2),
        ("16_digits", "1234567890123456", 0),
    ];

    for (name, value, scale) in test_values {
        // Parse benchmarks
        group.bench_with_input(
            BenchmarkId::new("parse/Decimal64", name),
            &(value, scale as u8),
            |b, (s, sc)| b.iter(|| Decimal64::new(black_box(s), *sc).unwrap()),
        );

        group.bench_with_input(
            BenchmarkId::new("parse/Decimal64NoScale", name),
            &(value, scale),
            |b, (s, sc)| b.iter(|| Decimal64NoScale::new(black_box(s), *sc).unwrap()),
        );

        // to_string benchmarks
        let d64 = Decimal64::new(value, scale as u8).unwrap();
        let d64ns = Decimal64NoScale::new(value, scale).unwrap();

        group.bench_with_input(
            BenchmarkId::new("to_string/Decimal64", name),
            &d64,
            |b, d| b.iter(|| black_box(d).to_string()),
        );

        group.bench_with_input(
            BenchmarkId::new("to_string/Decimal64NoScale", name),
            &(d64ns, scale),
            |b, (d, s)| b.iter(|| black_box(d).to_string_with_scale(*s)),
        );
    }

    // Equality check (key difference: Decimal64NoScale is direct i64 compare)
    let d64_a = Decimal64::new("123456.78", 2).unwrap();
    let d64_b = Decimal64::new("123456.78", 2).unwrap();
    let d64ns_a = Decimal64NoScale::new("123456.78", 2).unwrap();
    let d64ns_b = Decimal64NoScale::new("123456.78", 2).unwrap();

    group.bench_function("equality/Decimal64", |b| {
        b.iter(|| black_box(d64_a) == black_box(d64_b))
    });

    group.bench_function("equality/Decimal64NoScale", |b| {
        b.iter(|| black_box(d64ns_a) == black_box(d64ns_b))
    });

    group.finish();
}

fn bench_decimal64ns_18_digit_precision(c: &mut Criterion) {
    let mut group = c.benchmark_group("18_digit_precision");

    // 18 digits - only Decimal64NoScale can handle this
    let value_18 = "123456789012345678";

    group.bench_function("Decimal64NoScale/18_digits", |b| {
        b.iter(|| Decimal64NoScale::new(black_box(value_18), 0).unwrap())
    });

    group.bench_function("Decimal/18_digits", |b| {
        b.iter(|| Decimal::from_str(black_box(value_18)).unwrap())
    });

    // 16 digits with 2 decimal places (16+2 = 18 scaled digits, max for Decimal64NoScale)
    let value_16_decimal = "1234567890123456.78";

    group.bench_function("Decimal64NoScale/16+2_decimal", |b| {
        b.iter(|| Decimal64NoScale::new(black_box(value_16_decimal), 2).unwrap())
    });

    group.bench_function("Decimal/16+2_decimal", |b| {
        b.iter(|| Decimal::from_str(black_box(value_16_decimal)).unwrap())
    });

    group.finish();
}

fn bench_aggregate_simulation(c: &mut Criterion) {
    let mut group = c.benchmark_group("aggregate_simulation");

    // Create 1000 financial values
    let n = 1000;

    let decimal64_values: Vec<Decimal64> = (0..n)
        .map(|i| Decimal64::new(&format!("{}.99", i % 10000), 2).unwrap())
        .collect();

    let decimal64ns_values: Vec<Decimal64NoScale> = (0..n)
        .map(|i| Decimal64NoScale::new(&format!("{}.99", i % 10000), 2).unwrap())
        .collect();

    // SUM aggregate
    group.throughput(Throughput::Elements(n as u64));

    group.bench_function("sum_1000/Decimal64NoScale", |b| {
        b.iter(|| {
            let sum: i64 = decimal64ns_values.iter().map(|d| d.value()).sum();
            black_box(sum)
        })
    });

    // For Decimal64, we need to unpack each value (slower)
    group.bench_function("sum_1000/Decimal64", |b| {
        b.iter(|| {
            let sum: i64 = decimal64_values.iter().map(|d| d.value()).sum();
            black_box(sum)
        })
    });

    // MIN/MAX aggregate
    group.bench_function("min_max_1000/Decimal64NoScale", |b| {
        b.iter(|| {
            let min = decimal64ns_values.iter().map(|d| d.value()).min().unwrap();
            let max = decimal64ns_values.iter().map(|d| d.value()).max().unwrap();
            black_box((min, max))
        })
    });

    group.bench_function("min_max_1000/Decimal64", |b| {
        b.iter(|| {
            let min = decimal64_values.iter().map(|d| d.value()).min().unwrap();
            let max = decimal64_values.iter().map(|d| d.value()).max().unwrap();
            black_box((min, max))
        })
    });

    group.finish();
}

fn bench_memory_size_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_size_all");

    let decimal = Decimal::from_str("123456.789012").unwrap();
    let decimal64 = Decimal64::new("123456.789012", 6).unwrap();
    let decimal64ns = Decimal64NoScale::new("123456.789012", 6).unwrap();

    println!("\nMemory sizes:");
    println!(
        "  Decimal:         {} bytes (stack) + {} bytes (heap)",
        std::mem::size_of_val(&decimal),
        decimal.as_bytes().len()
    );
    println!(
        "  Decimal64:       {} bytes (total, no heap)",
        std::mem::size_of_val(&decimal64)
    );
    println!(
        "  Decimal64NoScale: {} bytes (total, no heap)",
        std::mem::size_of_val(&decimal64ns)
    );

    // Simulate creating many values
    group.throughput(Throughput::Elements(1000));

    group.bench_function("create_1000/Decimal", |b| {
        b.iter(|| {
            (0..1000)
                .map(|i| Decimal::from_str(&format!("{}.99", i)).unwrap())
                .collect::<Vec<_>>()
        })
    });

    group.bench_function("create_1000/Decimal64", |b| {
        b.iter(|| {
            (0..1000)
                .map(|i| Decimal64::new(&format!("{}.99", i), 2).unwrap())
                .collect::<Vec<_>>()
        })
    });

    group.bench_function("create_1000/Decimal64NoScale", |b| {
        b.iter(|| {
            (0..1000)
                .map(|i| Decimal64NoScale::new(&format!("{}.99", i), 2).unwrap())
                .collect::<Vec<_>>()
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_parse,
    bench_to_string,
    bench_comparison,
    bench_special_values,
    bench_precision_scale,
    bench_serialization,
    bench_from_bytes,
    bench_batch_operations,
    // Decimal64 specific
    bench_decimal64_parse,
    bench_decimal64_to_string,
    bench_decimal64_comparison,
    bench_decimal64_special,
    bench_decimal64_serialization,
    bench_decimal64_precision_scale,
    // Decimal64NoScale specific
    bench_decimal64ns_parse,
    bench_decimal64ns_to_string,
    bench_decimal64ns_operations,
    bench_decimal64ns_special,
    bench_decimal64ns_serialization,
    // Comparison benchmarks
    bench_comparison_decimal_vs_decimal64,
    bench_batch_decimal_vs_decimal64,
    bench_decimal64_vs_decimal64ns,
    bench_decimal64ns_18_digit_precision,
    bench_aggregate_simulation,
    bench_memory_size_all,
);

criterion_main!(benches);

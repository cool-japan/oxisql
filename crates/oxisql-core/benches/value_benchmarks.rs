use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use oxisql_core::{Row, Value};

fn bench_row_get_by_name(c: &mut Criterion) {
    let row = Row::new(
        vec![
            "id".to_string(),
            "name".to_string(),
            "score".to_string(),
            "active".to_string(),
        ],
        vec![
            Value::I64(42),
            Value::Text("hello".to_string()),
            Value::F64(std::f64::consts::PI),
            Value::Bool(true),
        ],
    );

    c.bench_function("row_get_by_name_hit", |b| {
        b.iter(|| {
            let _ = black_box(row.get("name"));
        })
    });

    c.bench_function("row_get_by_name_miss", |b| {
        b.iter(|| {
            let _ = black_box(row.get("nonexistent"));
        })
    });

    c.bench_function("row_get_by_index_hit", |b| {
        b.iter(|| {
            let _ = black_box(row.get_by_index(1));
        })
    });
}

fn bench_value_clone(c: &mut Criterion) {
    let text_val = Value::Text("a very long text value that requires heap allocation".to_string());
    let blob_val = Value::Blob(vec![0u8; 1024]);
    let i64_val = Value::I64(i64::MAX);
    let null_val = Value::Null;

    c.bench_function("value_text_clone", |b| {
        b.iter(|| black_box(text_val.clone()))
    });

    c.bench_function("value_blob_clone", |b| {
        b.iter(|| black_box(blob_val.clone()))
    });

    c.bench_function("value_i64_clone", |b| b.iter(|| black_box(i64_val.clone())));

    c.bench_function("value_null_clone", |b| {
        b.iter(|| black_box(null_val.clone()))
    });
}

fn bench_row_construction(c: &mut Criterion) {
    c.bench_function("row_new_4_columns", |b| {
        b.iter(|| {
            black_box(Row::new(
                vec![
                    "col1".to_string(),
                    "col2".to_string(),
                    "col3".to_string(),
                    "col4".to_string(),
                ],
                vec![
                    Value::I64(1),
                    Value::Text("test".to_string()),
                    Value::F64(1.5),
                    Value::Bool(true),
                ],
            ))
        })
    });

    c.bench_function("row_new_16_columns", |b| {
        b.iter(|| {
            let cols: Vec<String> = (0..16).map(|i| format!("col{i}")).collect();
            let vals: Vec<Value> = (0..16).map(|i| Value::I64(i as i64)).collect();
            black_box(Row::new(cols, vals))
        })
    });
}

criterion_group!(
    benches,
    bench_row_get_by_name,
    bench_value_clone,
    bench_row_construction
);
criterion_main!(benches);

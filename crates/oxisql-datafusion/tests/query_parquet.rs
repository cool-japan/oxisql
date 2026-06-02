//! Integration tests for `ParquetTableProvider` (requires `columnar` feature).

#[cfg(feature = "columnar")]
mod parquet_tests {
    use std::sync::Arc;

    use arrow::array::{Float64Array, Int64Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use datafusion::datasource::TableProvider;
    use oxisql_datafusion::{OxiSqlContext, ParquetTableProvider};

    /// Write a small Parquet file via oxistore-columnar and read it back through
    /// DataFusion, verifying that row count and column values are correct.
    #[tokio::test]
    async fn test_parquet_provider_write_and_read() {
        let path = std::env::temp_dir().join("oxisql_datafusion_parquet_roundtrip.parquet");

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, false),
            Field::new("score", DataType::Float64, false),
        ]));

        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec!["alice", "bob", "carol"])),
                Arc::new(Float64Array::from(vec![9.1, 8.2, 7.3])),
            ],
        )
        .expect("RecordBatch construction failed");

        // Write via oxistore-columnar free function.
        oxistore_columnar::write_batches(&path, Arc::clone(&schema), &[batch])
            .expect("write_batches failed");

        // Open via ParquetTableProvider — schema should be inferred.
        let provider =
            ParquetTableProvider::open(&path).expect("ParquetTableProvider::open failed");
        assert_eq!(provider.schema().fields().len(), 3);

        // Register in a DataFusion context and run a query.
        let ctx = OxiSqlContext::new();
        ctx.register_parquet("events", &path)
            .expect("register_parquet failed");

        let batches = ctx
            .execute_sql("SELECT id, name FROM events ORDER BY id")
            .await
            .expect("execute_sql failed");

        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 3, "expected 3 rows");

        // Verify first batch values.
        let first = &batches[0];
        let ids = first
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("id column should be Int64Array");
        assert_eq!(ids.value(0), 1);
        assert_eq!(ids.value(1), 2);
        assert_eq!(ids.value(2), 3);

        let _ = std::fs::remove_file(&path);
    }

    /// Verify that column projection is applied: requesting only one column
    /// should return a schema with a single field.
    #[tokio::test]
    async fn test_parquet_provider_projection() {
        let path = std::env::temp_dir().join("oxisql_datafusion_parquet_projection.parquet");

        let schema = Arc::new(Schema::new(vec![
            Field::new("x", DataType::Int64, false),
            Field::new("y", DataType::Int64, false),
        ]));

        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![10, 20, 30])),
                Arc::new(Int64Array::from(vec![100, 200, 300])),
            ],
        )
        .expect("RecordBatch construction failed");

        oxistore_columnar::write_batches(&path, Arc::clone(&schema), &[batch])
            .expect("write_batches failed");

        let ctx = OxiSqlContext::new();
        ctx.register_parquet("xy", &path)
            .expect("register_parquet failed");

        // Select only the `x` column — DataFusion will request projection [0].
        let batches = ctx
            .execute_sql("SELECT x FROM xy")
            .await
            .expect("execute_sql failed");

        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 3);

        // Schema returned by the batch should contain only `x`.
        assert_eq!(batches[0].schema().fields().len(), 1);
        assert_eq!(batches[0].schema().field(0).name(), "x");

        let _ = std::fs::remove_file(&path);
    }

    /// Verify that projection works on non-first columns (e.g. SELECT y FROM schema [x, y]).
    ///
    /// This catches a double-projection bug where `read_batches_with_projection`
    /// already narrows the schema and `MemorySourceConfig::try_new_exec` would
    /// then receive out-of-bounds column indices.
    #[tokio::test]
    async fn test_parquet_provider_projection_non_first_column() {
        let path = std::env::temp_dir().join("oxisql_datafusion_parquet_proj_nonfirst.parquet");

        let schema = Arc::new(Schema::new(vec![
            Field::new("a", DataType::Int64, false),
            Field::new("b", DataType::Int64, false),
            Field::new("c", DataType::Int64, false),
        ]));

        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![1, 2])),
                Arc::new(Int64Array::from(vec![10, 20])),
                Arc::new(Int64Array::from(vec![100, 200])),
            ],
        )
        .expect("RecordBatch construction failed");

        oxistore_columnar::write_batches(&path, Arc::clone(&schema), &[batch])
            .expect("write_batches failed");

        let ctx = OxiSqlContext::new();
        ctx.register_parquet("abc", &path)
            .expect("register_parquet failed");

        // SELECT c — DataFusion will request projection [2] (third column).
        let batches = ctx
            .execute_sql("SELECT c FROM abc")
            .await
            .expect("execute_sql failed");

        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 2);

        // Should contain only `c`, with values 100 and 200.
        assert_eq!(batches[0].schema().fields().len(), 1);
        assert_eq!(batches[0].schema().field(0).name(), "c");
        let vals = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("c column should be Int64Array");
        assert_eq!(vals.value(0), 100);
        assert_eq!(vals.value(1), 200);

        let _ = std::fs::remove_file(&path);
    }

    /// Verify that a WHERE predicate applied by DataFusion after the scan
    /// returns the expected subset of rows.
    #[tokio::test]
    async fn test_parquet_provider_filter() {
        let path = std::env::temp_dir().join("oxisql_datafusion_parquet_filter.parquet");

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("val", DataType::Float64, false),
        ]));

        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3, 4, 5])),
                Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0, 4.0, 5.0])),
            ],
        )
        .expect("RecordBatch construction failed");

        oxistore_columnar::write_batches(&path, Arc::clone(&schema), &[batch])
            .expect("write_batches failed");

        let ctx = OxiSqlContext::new();
        ctx.register_parquet("vals", &path)
            .expect("register_parquet failed");

        let batches = ctx
            .execute_sql("SELECT id FROM vals WHERE id > 3")
            .await
            .expect("execute_sql failed");

        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 2, "expected ids 4 and 5");

        let _ = std::fs::remove_file(&path);
    }
}

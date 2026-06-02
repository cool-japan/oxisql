// Tests have been split into logical modules:
//   memory_basic.rs       — core CRUD, connections, temporal types, query_stream, transactions
//   memory_fts.rs         — full-text search tests
//   memory_prepare.rs     — prepared statements, schema introspection
//   memory_udf.rs         — UDF registry, aggregate UDFs, savepoints
//   memory_complex.rs     — DDL/complex queries, EXPLAIN, JSON, PRAGMA, ATTACH, import/export
//   memory_vtable.rs      — virtual table registration, B-tree index
//   memory_persistent.rs  — fjall-storage and redb-storage backend tests

//! Integration tests for PostgreSQL logical replication (the `pgoutput`
//! protocol), gated behind both the `integration-postgres` and `replication`
//! Cargo features.
//!
//! Unlike the rest of this crate's integration suite, these tests require a
//! server explicitly configured for logical replication (`wal_level=logical`,
//! with enough replication slots/senders for a handful of short-lived test
//! slots):
//!
//! ```bash
//! docker run --rm -e POSTGRES_PASSWORD=test -p 5432:5432 postgres -c wal_level=logical -c max_replication_slots=4 -c max_wal_senders=4
//! POSTGRES_URL=postgres://postgres:test@localhost/postgres cargo test -p oxisql-postgres --features replication,integration-postgres -- --ignored replication
//! ```
//!
//! Every test below is additionally gated on the `POSTGRES_URL` environment
//! variable via the same `match ... => return` graceful-skip idiom used by
//! `tests/integration.rs`'s more robust tests, so a bare
//! `cargo test --features replication,integration-postgres -- --ignored`
//! without a configured server exits cleanly instead of failing. Each `mod`
//! below is gated with `#[cfg(all(feature = "integration-postgres", feature =
//! "replication"))]` (rather than a single file-level `#![cfg(...)]`) so that
//! this file still compiles cleanly under any subset of the two features —
//! including just one of them, which is a normal configuration this crate
//! supports.

// ── identify_system ───────────────────────────────────────────────────────────

#[cfg(all(feature = "integration-postgres", feature = "replication"))]
mod pg_replication_identify {
    use oxisql_postgres::{parse_pg_conn_str, PgReplicationConnection, TlsMode};

    /// Connects in replication mode and runs `IDENTIFY_SYSTEM`, asserting the
    /// returned fields are sane: a non-empty decimal `systemid`, a `timeline`
    /// of at least 1, a nonzero `xlogpos`, and a `dbname` that echoes back the
    /// database named in `POSTGRES_URL`.
    #[tokio::test]
    #[ignore = "requires a live PostgreSQL server with wal_level=logical"]
    async fn identify_system_returns_sane_fields() {
        let url = match std::env::var("POSTGRES_URL") {
            Ok(u) => u,
            Err(_) => return,
        };

        let expected_dbname = parse_pg_conn_str(&url).expect("parse POSTGRES_URL").dbname;

        let mut conn = PgReplicationConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("replication connect");

        let identity = conn.identify_system().await.expect("IDENTIFY_SYSTEM");

        assert!(
            !identity.systemid.is_empty() && identity.systemid.chars().all(|c| c.is_ascii_digit()),
            "systemid should be a non-empty decimal string, got {:?}",
            identity.systemid
        );
        assert!(identity.timeline >= 1, "timeline should be >= 1");
        assert!(
            identity.xlogpos.as_u64() > 0,
            "xlogpos should be a nonzero WAL position"
        );
        assert_eq!(
            identity.dbname, expected_dbname,
            "IDENTIFY_SYSTEM should echo back the connected database"
        );
    }
}

// ── create/drop replication slot ──────────────────────────────────────────────

#[cfg(all(feature = "integration-postgres", feature = "replication"))]
mod pg_replication_slots {
    use oxisql_postgres::{PgError, PgReplicationConnection, TlsMode};

    const SLOT: &str = "oxisql_repl_temp_slot_test";

    /// Creates a temporary replication slot, verifies the returned metadata,
    /// then explicitly drops it and confirms the drop took effect server-side
    /// (a second drop of the same, now-gone, slot must fail).
    ///
    /// Uses `temporary: true`, so even if this test panicked before reaching
    /// the explicit drop, PostgreSQL would still clean the slot up itself
    /// once this session's connection closes — no separate cleanup step is
    /// needed here the way the other test modules in this file need one for
    /// their (necessarily non-temporary) tables/publications.
    #[tokio::test]
    #[ignore = "requires a live PostgreSQL server with wal_level=logical"]
    async fn create_and_drop_temporary_slot() {
        let url = match std::env::var("POSTGRES_URL") {
            Ok(u) => u,
            Err(_) => return,
        };

        let mut conn = PgReplicationConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("replication connect");

        let created = conn
            .create_replication_slot(SLOT, true)
            .await
            .expect("CREATE_REPLICATION_SLOT");

        assert_eq!(created.slot_name, SLOT);
        assert_eq!(created.output_plugin.as_deref(), Some("pgoutput"));
        assert!(
            created.consistent_point.as_u64() > 0,
            "consistent_point should be a real WAL position"
        );

        conn.drop_replication_slot(SLOT)
            .await
            .expect("DROP_REPLICATION_SLOT");

        // Dropping an already-dropped slot must fail — this confirms the
        // drop above actually took effect server-side, not merely that the
        // round trip succeeded.
        let err = conn
            .drop_replication_slot(SLOT)
            .await
            .expect_err("dropping an already-dropped slot should fail");
        assert!(
            matches!(err, PgError::Replication(_)),
            "expected PgError::Replication, got {err:?}"
        );
    }
}

// ── full streaming round trip: INSERT / UPDATE / DELETE, and TRUNCATE ────────

#[cfg(all(feature = "integration-postgres", feature = "replication"))]
mod pg_replication_streaming {
    use futures::StreamExt;
    use oxisql_core::{Connection, Value};
    use oxisql_postgres::{
        CellValue, LogicalReplicationMessage, Lsn, PgConnection, PgReplicationConnection,
        ReplicationEvent, ReplicationStream, TlsMode,
    };

    /// Reads events off `stream`, skipping `KeepAlive`s, until the next
    /// `Logical` event arrives. Returns its WAL range and decoded message.
    ///
    /// Panics with a descriptive message if the stream ends or yields an
    /// error first — for these tests that always indicates a bug or an
    /// unexpectedly-configured server, never an expected outcome.
    async fn next_logical_message(
        stream: &mut ReplicationStream,
    ) -> (Lsn, Lsn, LogicalReplicationMessage) {
        loop {
            let event = stream
                .next()
                .await
                .expect("replication stream ended before the expected event arrived")
                .expect("replication stream yielded an error");
            if let ReplicationEvent::Logical {
                wal_start,
                wal_end,
                message,
            } = event
            {
                return (wal_start, wal_end, message);
            }
            // A keepalive interleaved between logical messages is normal
            // server behavior — skip it and keep waiting.
        }
    }

    const TABLE: &str = "oxisql_repl_stream_test";
    const PUBLICATION: &str = "oxisql_repl_stream_pub";
    const SLOT: &str = "oxisql_repl_stream_slot";

    /// Full round trip: `CREATE TABLE` + `CREATE PUBLICATION` on a normal
    /// connection, `CREATE_REPLICATION_SLOT` + `START_REPLICATION` on a
    /// replication connection, then a single `INSERT`/`INSERT`/`UPDATE`/
    /// `DELETE` transaction on the normal connection.
    ///
    /// Asserts the decoded `Begin` -> `Relation` -> `Insert` -> `Insert` ->
    /// `Update` -> `Delete` -> `Commit` sequence, decodes each tuple's values
    /// via [`ReplicationStream::decode_tuple`] and asserts they are correct,
    /// and finally acks the commit.
    #[tokio::test]
    #[ignore = "requires a live PostgreSQL server with wal_level=logical"]
    async fn insert_update_delete_end_to_end() {
        let url = match std::env::var("POSTGRES_URL") {
            Ok(u) => u,
            Err(_) => return,
        };

        let ddl_conn = PgConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("ddl connect");

        ddl_conn
            .execute_batch(&format!(
                "DROP PUBLICATION IF EXISTS {PUBLICATION};
                 DROP TABLE IF EXISTS {TABLE};
                 CREATE TABLE {TABLE} (id BIGINT PRIMARY KEY, name TEXT);
                 CREATE PUBLICATION {PUBLICATION} FOR TABLE {TABLE};"
            ))
            .await
            .expect("setup DDL");

        let mut repl_conn = PgReplicationConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("replication connect");
        let slot = repl_conn
            .create_replication_slot(SLOT, true)
            .await
            .expect("CREATE_REPLICATION_SLOT");

        let mut stream = repl_conn
            .start_logical_replication(SLOT, &[PUBLICATION], slot.consistent_point)
            .await
            .expect("START_REPLICATION");

        ddl_conn
            .execute_batch(&format!(
                "BEGIN;
                 INSERT INTO {TABLE} (id, name) VALUES (1, 'Alice');
                 INSERT INTO {TABLE} (id, name) VALUES (2, 'Bob');
                 UPDATE {TABLE} SET name = 'Alicia' WHERE id = 1;
                 DELETE FROM {TABLE} WHERE id = 2;
                 COMMIT;"
            ))
            .await
            .expect("DML transaction");

        // Begin
        let (_, _, begin) = next_logical_message(&mut stream).await;
        assert!(
            matches!(begin, LogicalReplicationMessage::Begin { .. }),
            "expected Begin, got {begin:?}"
        );

        // Relation: schema for our table, sent once before the first DML
        // message that references it.
        let (_, _, relation) = next_logical_message(&mut stream).await;
        let rel_id = match relation {
            LogicalReplicationMessage::Relation(body) => {
                assert_eq!(body.name, TABLE);
                assert_eq!(body.columns.len(), 2);
                assert!(body.columns[0].key, "id should be the replica identity key");
                assert!(!body.columns[1].key, "name should not be a key column");
                body.rel_id
            }
            other => panic!("expected Relation, got {other:?}"),
        };

        // Insert id=1 'Alice'.
        let (_, _, insert1) = next_logical_message(&mut stream).await;
        match insert1 {
            LogicalReplicationMessage::Insert {
                rel_id: r,
                new_tuple,
            } => {
                assert_eq!(r, rel_id);
                let values = stream.decode_tuple(r, &new_tuple).expect("decode insert 1");
                assert_eq!(values[0], CellValue::Value(Value::I64(1)));
                assert_eq!(
                    values[1],
                    CellValue::Value(Value::Text("Alice".to_string()))
                );
            }
            other => panic!("expected Insert, got {other:?}"),
        }

        // Insert id=2 'Bob'.
        let (_, _, insert2) = next_logical_message(&mut stream).await;
        match insert2 {
            LogicalReplicationMessage::Insert {
                rel_id: r,
                new_tuple,
            } => {
                assert_eq!(r, rel_id);
                let values = stream.decode_tuple(r, &new_tuple).expect("decode insert 2");
                assert_eq!(values[0], CellValue::Value(Value::I64(2)));
                assert_eq!(values[1], CellValue::Value(Value::Text("Bob".to_string())));
            }
            other => panic!("expected Insert, got {other:?}"),
        }

        // Update id=1 -> name='Alicia'.
        let (_, _, update) = next_logical_message(&mut stream).await;
        match update {
            LogicalReplicationMessage::Update {
                rel_id: r,
                old_tuple,
                new_tuple,
                ..
            } => {
                assert_eq!(r, rel_id);
                let new_values = stream
                    .decode_tuple(r, &new_tuple)
                    .expect("decode update new");
                assert_eq!(new_values[0], CellValue::Value(Value::I64(1)));
                assert_eq!(
                    new_values[1],
                    CellValue::Value(Value::Text("Alicia".to_string()))
                );
                // REPLICA IDENTITY DEFAULT (backed by the primary key) sends
                // a key-only old-row image: the key column (id) carries its
                // real value, but the non-key column (name) carries no real
                // data (PostgreSQL only ever guarantees the *key* survives
                // in a 'K'-tagged tuple — exactly which non-data marker it
                // uses for the rest is not part of the documented contract
                // this test should pin down).
                if let Some(old_tuple) = old_tuple {
                    let old_values = stream
                        .decode_tuple(r, &old_tuple)
                        .expect("decode update old");
                    assert_eq!(old_values[0], CellValue::Value(Value::I64(1)));
                    assert!(
                        matches!(
                            old_values[1],
                            CellValue::Value(Value::Null) | CellValue::UnchangedToast
                        ),
                        "non-key column in a key-only old-row image should carry no real data, got {:?}",
                        old_values[1]
                    );
                }
            }
            other => panic!("expected Update, got {other:?}"),
        }

        // Delete id=2.
        let (_, _, delete) = next_logical_message(&mut stream).await;
        match delete {
            LogicalReplicationMessage::Delete {
                rel_id: r,
                old_tuple,
                ..
            } => {
                assert_eq!(r, rel_id);
                let values = stream.decode_tuple(r, &old_tuple).expect("decode delete");
                assert_eq!(values[0], CellValue::Value(Value::I64(2)));
                assert!(
                    matches!(
                        values[1],
                        CellValue::Value(Value::Null) | CellValue::UnchangedToast
                    ),
                    "non-key column in a key-only DELETE old-row image should carry no real data, got {:?}",
                    values[1]
                );
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        // Commit.
        let (_, commit_end_lsn, commit) = next_logical_message(&mut stream).await;
        assert!(
            matches!(commit, LogicalReplicationMessage::Commit { .. }),
            "expected Commit, got {commit:?}"
        );

        stream.ack(commit_end_lsn).await.expect("ack");

        drop(stream);
        ddl_conn
            .execute_batch(&format!(
                "DROP PUBLICATION IF EXISTS {PUBLICATION}; DROP TABLE IF EXISTS {TABLE};"
            ))
            .await
            .expect("cleanup");
    }

    const TRUNCATE_TABLE: &str = "oxisql_repl_truncate_test";
    const TRUNCATE_PUBLICATION: &str = "oxisql_repl_truncate_pub";
    const TRUNCATE_SLOT: &str = "oxisql_repl_truncate_slot";

    /// Verifies that a `TRUNCATE` statement is decoded as a `Truncate`
    /// message naming the truncated relation.
    #[tokio::test]
    #[ignore = "requires a live PostgreSQL server with wal_level=logical"]
    async fn truncate_is_decoded() {
        let url = match std::env::var("POSTGRES_URL") {
            Ok(u) => u,
            Err(_) => return,
        };

        let ddl_conn = PgConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("ddl connect");

        ddl_conn
            .execute_batch(&format!(
                "DROP PUBLICATION IF EXISTS {TRUNCATE_PUBLICATION};
                 DROP TABLE IF EXISTS {TRUNCATE_TABLE};
                 CREATE TABLE {TRUNCATE_TABLE} (id BIGINT PRIMARY KEY);
                 INSERT INTO {TRUNCATE_TABLE} VALUES (1), (2), (3);
                 CREATE PUBLICATION {TRUNCATE_PUBLICATION} FOR TABLE {TRUNCATE_TABLE};"
            ))
            .await
            .expect("setup DDL");

        let mut repl_conn = PgReplicationConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("replication connect");
        let slot = repl_conn
            .create_replication_slot(TRUNCATE_SLOT, true)
            .await
            .expect("CREATE_REPLICATION_SLOT");
        let mut stream = repl_conn
            .start_logical_replication(
                TRUNCATE_SLOT,
                &[TRUNCATE_PUBLICATION],
                slot.consistent_point,
            )
            .await
            .expect("START_REPLICATION");

        ddl_conn
            .execute(&format!("TRUNCATE {TRUNCATE_TABLE}"), &[])
            .await
            .expect("truncate");

        let (_, _, begin) = next_logical_message(&mut stream).await;
        assert!(
            matches!(begin, LogicalReplicationMessage::Begin { .. }),
            "expected Begin, got {begin:?}"
        );

        let (_, _, relation) = next_logical_message(&mut stream).await;
        let rel_id = match relation {
            LogicalReplicationMessage::Relation(body) => body.rel_id,
            other => panic!("expected Relation, got {other:?}"),
        };

        let (_, _, truncate) = next_logical_message(&mut stream).await;
        match truncate {
            LogicalReplicationMessage::Truncate {
                cascade,
                restart_identity,
                rel_ids,
            } => {
                assert!(!cascade, "no CASCADE was specified");
                assert!(!restart_identity, "no RESTART IDENTITY was specified");
                assert_eq!(rel_ids, vec![rel_id]);
            }
            other => panic!("expected Truncate, got {other:?}"),
        }

        let (_, commit_end_lsn, commit) = next_logical_message(&mut stream).await;
        assert!(
            matches!(commit, LogicalReplicationMessage::Commit { .. }),
            "expected Commit, got {commit:?}"
        );
        stream.ack(commit_end_lsn).await.expect("ack");

        drop(stream);
        ddl_conn
            .execute_batch(&format!(
                "DROP PUBLICATION IF EXISTS {TRUNCATE_PUBLICATION}; \
                 DROP TABLE IF EXISTS {TRUNCATE_TABLE};"
            ))
            .await
            .expect("cleanup");
    }
}

// ── reconnect and resume from an acked LSN ────────────────────────────────────

#[cfg(all(feature = "integration-postgres", feature = "replication"))]
mod pg_replication_resume {
    use std::time::Duration;

    use futures::StreamExt;
    use oxisql_core::{Connection, Value};
    use oxisql_postgres::{
        CellValue, LogicalReplicationMessage, Lsn, PgConnection, PgReplicationConnection,
        ReplicationEvent, ReplicationStream, TlsMode,
    };

    async fn next_logical_message(
        stream: &mut ReplicationStream,
    ) -> (Lsn, Lsn, LogicalReplicationMessage) {
        loop {
            let event = stream
                .next()
                .await
                .expect("replication stream ended before the expected event arrived")
                .expect("replication stream yielded an error");
            if let ReplicationEvent::Logical {
                wal_start,
                wal_end,
                message,
            } = event
            {
                return (wal_start, wal_end, message);
            }
        }
    }

    const TABLE: &str = "oxisql_repl_resume_test";
    const PUBLICATION: &str = "oxisql_repl_resume_pub";
    const SLOT: &str = "oxisql_repl_resume_slot";

    /// Streams one transaction to completion, acks its commit LSN, drops the
    /// stream (closing that connection), inserts a second row while nobody
    /// is streaming, then opens a brand-new replication connection and
    /// resumes from the acked LSN on the same (necessarily non-temporary)
    /// slot. Confirms the first logical event observed after resuming
    /// belongs to the second row's transaction, not a replay of the first.
    ///
    /// This uses the `Commit` message's `end_lsn` — documented as "the
    /// position a client should resume streaming from" — so precise
    /// resumption (no replay of already-acked data) is exactly what this
    /// scenario is expected to guarantee; this test does not attempt to
    /// characterize the protocol's at-least-once semantics more generally
    /// (e.g. resuming from a partially-acked point mid-transaction).
    #[tokio::test]
    #[ignore = "requires a live PostgreSQL server with wal_level=logical"]
    async fn resume_from_acked_lsn_skips_already_processed_transaction() {
        let url = match std::env::var("POSTGRES_URL") {
            Ok(u) => u,
            Err(_) => return,
        };

        let ddl_conn = PgConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("ddl connect");

        // Defensive cleanup: a non-temporary slot from a prior aborted run
        // of this test may still exist. `pg_drop_replication_slot` is a
        // regular SQL-callable function, usable from a normal connection —
        // the `SELECT ... FROM pg_replication_slots WHERE ...` guard makes
        // this a no-op (zero rows, function never called) when the slot
        // does not exist, rather than erroring.
        let _ = ddl_conn
            .execute_batch(&format!(
                "SELECT pg_drop_replication_slot('{SLOT}') FROM pg_replication_slots \
                 WHERE slot_name = '{SLOT}';"
            ))
            .await;

        ddl_conn
            .execute_batch(&format!(
                "DROP PUBLICATION IF EXISTS {PUBLICATION};
                 DROP TABLE IF EXISTS {TABLE};
                 CREATE TABLE {TABLE} (id BIGINT PRIMARY KEY);
                 CREATE PUBLICATION {PUBLICATION} FOR TABLE {TABLE};"
            ))
            .await
            .expect("setup DDL");

        let mut repl_conn = PgReplicationConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("replication connect 1");
        // Non-temporary: must survive connection 1 disconnecting so
        // connection 2 can resume from the same slot.
        let slot = repl_conn
            .create_replication_slot(SLOT, false)
            .await
            .expect("CREATE_REPLICATION_SLOT");

        let mut stream = repl_conn
            .start_logical_replication(SLOT, &[PUBLICATION], slot.consistent_point)
            .await
            .expect("START_REPLICATION 1");

        ddl_conn
            .execute(&format!("INSERT INTO {TABLE} (id) VALUES (1)"), &[])
            .await
            .expect("insert row 1");

        let (_, _, begin1) = next_logical_message(&mut stream).await;
        assert!(matches!(begin1, LogicalReplicationMessage::Begin { .. }));
        let (_, _, relation1) = next_logical_message(&mut stream).await;
        assert!(matches!(relation1, LogicalReplicationMessage::Relation(_)));
        let (_, _, insert1) = next_logical_message(&mut stream).await;
        assert!(matches!(insert1, LogicalReplicationMessage::Insert { .. }));
        let (_, resume_lsn, commit1) = next_logical_message(&mut stream).await;
        assert!(matches!(commit1, LogicalReplicationMessage::Commit { .. }));

        stream.ack(resume_lsn).await.expect("ack");
        drop(stream); // Closes connection 1 and aborts its background tasks.

        // Give the server a moment to notice the socket closed before
        // reconnecting and resuming from the same (non-temporary) slot.
        tokio::time::sleep(Duration::from_millis(200)).await;

        ddl_conn
            .execute(&format!("INSERT INTO {TABLE} (id) VALUES (2)"), &[])
            .await
            .expect("insert row 2");

        let repl_conn2 = PgReplicationConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("replication connect 2");
        let mut stream2 = repl_conn2
            .start_logical_replication(SLOT, &[PUBLICATION], resume_lsn)
            .await
            .expect("START_REPLICATION 2");

        // The first logical message observed after resuming must belong to
        // row 2's transaction, not a replay of row 1's already-acked one.
        let (_, _, begin2) = next_logical_message(&mut stream2).await;
        assert!(matches!(begin2, LogicalReplicationMessage::Begin { .. }));

        // A fresh ReplicationStream starts with an empty schema cache, so
        // the server resends Relation before the Insert even though we
        // already observed it once on connection 1.
        let (_, _, relation2) = next_logical_message(&mut stream2).await;
        let rel_id2 = match relation2 {
            LogicalReplicationMessage::Relation(body) => body.rel_id,
            other => panic!("expected Relation, got {other:?}"),
        };

        let (_, _, insert2) = next_logical_message(&mut stream2).await;
        match insert2 {
            LogicalReplicationMessage::Insert { rel_id, new_tuple } => {
                assert_eq!(rel_id, rel_id2);
                let values = stream2
                    .decode_tuple(rel_id, &new_tuple)
                    .expect("decode insert 2");
                assert_eq!(
                    values[0],
                    CellValue::Value(Value::I64(2)),
                    "the first event after resuming must be row 2, not a replay of row 1"
                );
            }
            other => panic!("expected Insert, got {other:?}"),
        }

        let (_, _, commit2) = next_logical_message(&mut stream2).await;
        assert!(matches!(commit2, LogicalReplicationMessage::Commit { .. }));

        drop(stream2);

        // Cleanup: the slot is non-temporary, so it must be dropped
        // explicitly via a fresh replication connection (once streaming has
        // started, a connection cannot go back to issuing ordinary
        // replication commands).
        let mut cleanup_conn = PgReplicationConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("replication connect for cleanup");
        cleanup_conn
            .drop_replication_slot(SLOT)
            .await
            .expect("DROP_REPLICATION_SLOT");
        ddl_conn
            .execute_batch(&format!(
                "DROP PUBLICATION IF EXISTS {PUBLICATION}; DROP TABLE IF EXISTS {TABLE};"
            ))
            .await
            .expect("cleanup");
    }
}

// ── synthetic tuple decoding (no live server needed) ──────────────────────────

/// Exercises the public [`oxisql_postgres::tuple_to_values`] entry point
/// directly against a synthetic [`RelationBody`] + [`TupleData`] — unlike
/// every other module in this file, these tests need neither a live
/// PostgreSQL server nor the `integration-postgres` feature: they build the
/// already-decoded `pgoutput` structures by hand and check what
/// `tuple_to_values` does with them, exactly as
/// [`oxisql_postgres::ReplicationStream::decode_tuple`] does internally
/// once a live stream has cached a real `Relation` message.
#[cfg(feature = "replication")]
mod pg_replication_tuple_decode {
    use oxisql_core::{ArrayElementType, Value};
    use oxisql_postgres::{
        tuple_to_values, CellValue, ColumnSpec, RelationBody, ReplicaIdentity, TupleColumn,
        TupleData,
    };
    use tokio_postgres::types::Type;

    fn col(name: &str, type_oid: u32) -> ColumnSpec {
        ColumnSpec {
            key: false,
            name: name.to_string(),
            type_oid,
            type_modifier: -1,
        }
    }

    /// A `TupleColumn::Binary` cell decodes to a real typed [`Value`]
    /// through the public `tuple_to_values` path.
    #[test]
    fn binary_cell_decodes_through_tuple_to_values() {
        let rel = RelationBody {
            rel_id: 1,
            namespace: "public".to_string(),
            name: "widgets".to_string(),
            replica_identity: ReplicaIdentity::Default,
            columns: vec![col("count", Type::INT4.oid())],
        };
        // Binary-format INT4 42: a big-endian i32.
        let tuple = TupleData {
            columns: vec![TupleColumn::Binary(bytes::Bytes::copy_from_slice(
                &42i32.to_be_bytes(),
            ))],
        };

        let values = tuple_to_values(&rel, &tuple).expect("binary cell should decode");
        assert_eq!(values, vec![CellValue::Value(Value::I64(42))]);
    }

    /// An array-typed `TupleColumn::Text` cell decodes to a
    /// [`Value::TypedArray`] through the same public `tuple_to_values`
    /// path.
    #[test]
    fn array_text_cell_decodes_through_tuple_to_values() {
        let rel = RelationBody {
            rel_id: 2,
            namespace: "public".to_string(),
            name: "matrices".to_string(),
            replica_identity: ReplicaIdentity::Default,
            columns: vec![col("row", Type::INT4_ARRAY.oid())],
        };
        let tuple = TupleData {
            columns: vec![TupleColumn::Text("{1,2,3}".to_string())],
        };

        let values = tuple_to_values(&rel, &tuple).expect("array text cell should decode");
        assert_eq!(
            values,
            vec![CellValue::Value(Value::TypedArray {
                element_type: ArrayElementType::Int4,
                values: vec![Value::I64(1), Value::I64(2), Value::I64(3)],
            })]
        );
    }
}

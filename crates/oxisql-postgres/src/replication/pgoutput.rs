//! Decoder for PostgreSQL's `pgoutput` logical-decoding output-plugin
//! message format.
//!
//! `pgoutput` is the wire format PostgreSQL's built-in logical replication
//! mechanism (`CREATE PUBLICATION` / `CREATE SUBSCRIPTION`) uses to describe
//! row-level changes. During logical replication, each `pgoutput` message is
//! carried as the payload of an `XLogData` (`CopyData` tag `w`) frame; a
//! sibling module (`copyboth`) is responsible for stripping that framing and
//! handing this module a complete, single message's raw bytes. This module
//! doesn't need to know anything about `XLogData`/CopyBoth framing — it only
//! ever sees a `&[u8]` payload that IS one complete `pgoutput` message.
//!
//! # Scope
//!
//! This module is a **pure, stateless decoder**: [`decode_message`] takes a
//! byte payload and returns one [`LogicalReplicationMessage`], with no side
//! effects and no memory of previous calls. In particular:
//!
//! - It does **not** cache [`RelationBody`] schema information across calls.
//!   `Insert`/`Update`/`Delete`/`Truncate` messages carry only a `rel_id` and
//!   tagged column bytes — no column names or types — so a correct consumer
//!   MUST keep its own `rel_id -> RelationBody` cache (populated from
//!   [`LogicalReplicationMessage::Relation`] messages) and join subsequent
//!   DML messages against it. That stateful cache belongs to
//!   `ReplicationStream`, built in a later wave — not here.
//! - It handles only the pgoutput **v1** message set (tags `B`/`C`/`O`/`R`/
//!   `Y`/`I`/`U`/`D`/`T`/`M`). Streaming (`streaming 'on'`) and two-phase-commit
//!   (`two_phase 'on'`) subscription options introduce additional
//!   xid-prefixed message variants (`Stream Start`/`Stop`/`Commit`/`Abort`,
//!   `Begin Prepare`/`Prepare`/`Commit Prepared`/`Rollback Prepared`) that
//!   this decoder does not recognize. Since this MVP never negotiates those
//!   options, they should not appear on the wire — but if one did, its tag
//!   byte is reported as [`PgError::Protocol`] rather than silently
//!   misinterpreted as a v1 message. Support for them is a Phase 2 follow-up.
//! - Binary-format tuple columns (wire tag `'b'`) are recognized structurally
//!   (tag + length + raw bytes), so the byte stream stays in sync, but are
//!   not decoded into typed values — see [`TupleColumn::Binary`]. This MVP
//!   never negotiates `binary 'true'` with the server, so live traffic
//!   should only ever contain text-format (`'t'`) columns; interpreting
//!   `Binary` payloads is a Phase 2 follow-up.
//!
//! # Wire format conventions
//!
//! - All multi-byte integers are big-endian.
//! - `String` fields are null-terminated C strings: bytes up to (excluding)
//!   the next `0x00`, interpreted as UTF-8. A missing terminator or invalid
//!   UTF-8 is [`PgError::Protocol`].
//! - Timestamps are `i64` microseconds since the PostgreSQL epoch
//!   (`2000-01-01 00:00:00 UTC`) and are **not** converted to Unix epoch by
//!   this module — see [`super::lsn::pg_micros_to_unix_micros`] for that
//!   conversion, which callers can apply once they've decided how to
//!   represent the value.
//! - LSNs are transmitted as 8-byte big-endian values holding the raw WAL
//!   bit pattern (read via `get_u64`, not `get_i64`, then wrapped with
//!   [`Lsn::from_u64`]) — see [`Lsn`].
//!
//! Every reader in this module bounds-checks the remaining buffer length
//! *before* consuming bytes, so a short, truncated, or otherwise hostile
//! payload always yields [`PgError::Protocol`] rather than panicking. This
//! mirrors the decoding discipline used by
//! [`crate::types`]'s `decode_pg_numeric`/`decode_pg_interval`.

use bytes::{Buf, Bytes};

use super::lsn::Lsn;
use crate::error::PgError;

// ── Public types ────────────────────────────────────────────────────────────────

/// A cell in a decoded row's replica-identity/old-image or new-image tuple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TupleColumn {
    /// The `'n'` tag: a SQL `NULL` value.
    Null,
    /// The `'u'` tag: an unchanged, out-of-line `TOASTed` value — **not** the
    /// same as [`TupleColumn::Null`]. The value was omitted because it is
    /// unchanged from the previous row version; a consumer that needs it
    /// must carry forward the last known value from an earlier tuple image.
    UnchangedToast,
    /// The `'t'` tag: a text-format value. This MVP always uses text format
    /// for values it decodes (it never negotiates `binary 'true'`).
    Text(String),
    /// The `'b'` tag: a binary-format value, still in its raw
    /// wire-encoded form here (this type only carries the bytes — the
    /// sibling `tuple` module's `tuple_to_values`/`binary_to_value` decode
    /// it into a typed value). Recognized structurally (tag, length, raw
    /// bytes) so the byte stream stays in sync, even though live traffic
    /// should never actually produce one under this MVP's current
    /// negotiation.
    Binary(Bytes),
}

/// A decoded row image: the ordered column values of one `TupleData` span,
/// as they appear in an `Insert`/`Update`/`Delete` message.
///
/// The columns correspond positionally to the [`ColumnSpec`]s of the
/// [`RelationBody`] previously announced for the same `rel_id` — this type
/// carries no column names or types itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TupleData {
    /// The row's column values, in relation column order.
    pub columns: Vec<TupleColumn>,
}

/// How the source table's `REPLICA IDENTITY` is configured. Determines which
/// old-row-image tag (`'K'` vs `'O'`) appears on `Update`/`Delete` messages
/// for the table, and therefore how much of the previous row a consumer can
/// expect to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaIdentity {
    /// Wire byte `'d'`: the table's primary key (the `PostgreSQL` default).
    Default,
    /// Wire byte `'n'`: no replica identity — `Update`/`Delete` messages
    /// carry no old-row image at all.
    Nothing,
    /// Wire byte `'f'`: the entire row is treated as the identity
    /// (`REPLICA IDENTITY FULL`) — `Update`/`Delete` old-row images are
    /// full-row copies.
    Full,
    /// Wire byte `'i'`: a specific unique index is used as the identity.
    Index,
}

/// One column's schema, as announced in a [`RelationBody`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSpec {
    /// `true` if this column is part of the table's replica identity key.
    pub key: bool,
    /// The column's name.
    pub name: String,
    /// The column's `PostgreSQL` type OID.
    pub type_oid: u32,
    /// The column type's `atttypmod` (e.g. a `VARCHAR(n)`'s `n`, encoded
    /// however that type defines; `-1` conventionally means "unspecified").
    pub type_modifier: i32,
}

/// Schema-cache entry for one relation (table), as decoded from a `'R'`
/// message.
///
/// The server sends this **before** the first `Insert`/`Update`/`Delete`/
/// `Truncate` message referencing a given `rel_id` (and again if the
/// relation's schema changes), because DML messages carry no column names
/// or types of their own — only a `rel_id` and tagged column bytes. A
/// correct client must cache this by `rel_id` and join subsequent DML
/// against it; see the module-level "Scope" section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationBody {
    /// The relation's OID, referenced by later DML messages.
    pub rel_id: u32,
    /// The relation's schema (namespace) name.
    pub namespace: String,
    /// The relation's (table's) name.
    pub name: String,
    /// How the table's replica identity is configured.
    pub replica_identity: ReplicaIdentity,
    /// The relation's columns, in wire/table order.
    pub columns: Vec<ColumnSpec>,
}

/// One decoded pgoutput logical-replication message (v1 message set).
///
/// See the module-level "Scope" section for what this MVP
/// does and does not handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogicalReplicationMessage {
    /// Marks the start of a transaction's message stream. Always the first
    /// message for a transaction, eventually followed by a matching
    /// [`Self::Commit`]. Because the server only streams a transaction once
    /// it has fully committed, the commit LSN is already known up front.
    Begin {
        /// The LSN of the transaction's `COMMIT` record.
        final_lsn: Lsn,
        /// Commit timestamp, in microseconds since the `PostgreSQL` epoch
        /// (`2000-01-01 00:00:00 UTC`).
        commit_time: i64,
        /// The transaction's XID.
        xid: u32,
    },
    /// Marks the end of a transaction's message stream.
    Commit {
        /// Reserved flags byte; always `0` in the current protocol version.
        /// Consumed but not interpreted by this decoder.
        flags: u8,
        /// The LSN of the commit record itself.
        commit_lsn: Lsn,
        /// The LSN of the record immediately following the commit record —
        /// the position a client should resume streaming from.
        end_lsn: Lsn,
        /// Commit timestamp, in microseconds since the `PostgreSQL` epoch.
        commit_time: i64,
    },
    /// Identifies the origin of a replicated change, for cascading /
    /// multi-source replication topologies. Only sent when origin
    /// information exists and was requested for the slot.
    Origin {
        /// The LSN of the commit on the origin server.
        commit_lsn: Lsn,
        /// The replication origin's name.
        name: String,
    },
    /// Schema-cache entry for a relation. See [`RelationBody`] and the
    /// module-level "Scope" section.
    Relation(RelationBody),
    /// Schema-cache entry for a non-built-in column type (e.g. an enum or
    /// domain). Sent before a [`Self::Relation`] that references it, if the
    /// type is not already known to the client.
    Type {
        /// The type's OID.
        id: u32,
        /// The type's schema (namespace) name.
        namespace: String,
        /// The type's name.
        name: String,
    },
    /// A single-row `INSERT`.
    Insert {
        /// The relation the row was inserted into; resolve column names/
        /// types via a prior [`Self::Relation`] message with a matching
        /// `rel_id`.
        rel_id: u32,
        /// The inserted row's column values.
        new_tuple: TupleData,
    },
    /// A single-row `UPDATE`.
    Update {
        /// The relation the row belongs to.
        rel_id: u32,
        /// The row's previous values, if the server sent one. Whether one is
        /// sent at all depends on the table's `REPLICA IDENTITY` and
        /// (for `DEFAULT`/`INDEX`) whether any identity-key column actually
        /// changed value. See `old_tuple_is_full` for how much of the row it
        /// covers.
        old_tuple: Option<TupleData>,
        /// `true` if `old_tuple` is a full-row image (wire tag `'O'`,
        /// `REPLICA IDENTITY FULL`); `false` if it covers only the replica
        /// identity key columns (wire tag `'K'`). Meaningless when
        /// `old_tuple` is `None`.
        old_tuple_is_full: bool,
        /// The row's new values.
        new_tuple: TupleData,
    },
    /// A single-row `DELETE`.
    Delete {
        /// The relation the row was deleted from.
        rel_id: u32,
        /// The deleted row's previous values — always present for `Delete`
        /// (unlike `Update`, this is the only image the server has to send).
        old_tuple: TupleData,
        /// `true` if `old_tuple` is a full-row image (wire tag `'O'`,
        /// `REPLICA IDENTITY FULL`); `false` if it covers only the replica
        /// identity key columns (wire tag `'K'`).
        old_tuple_is_full: bool,
    },
    /// A `TRUNCATE` statement, possibly covering multiple relations sent
    /// together in one message (e.g. `TRUNCATE a, b, c`).
    Truncate {
        /// `true` if `TRUNCATE ... CASCADE` was specified.
        cascade: bool,
        /// `true` if `TRUNCATE ... RESTART IDENTITY` was specified.
        restart_identity: bool,
        /// The truncated relations' OIDs.
        rel_ids: Vec<u32>,
    },
    /// An arbitrary application message emitted via
    /// `pg_logical_emit_message`.
    Message {
        /// `true` if the message was emitted with `transactional := true`
        /// (delivered tied to its enclosing transaction's commit); `false`
        /// if emitted non-transactionally (delivered immediately,
        /// independent of any transaction's outcome).
        transactional: bool,
        /// The LSN of the message itself (for a transactional message, the
        /// LSN of the `pg_logical_emit_message` call — not the commit LSN).
        lsn: Lsn,
        /// The application-defined namespacing prefix passed to
        /// `pg_logical_emit_message`.
        prefix: String,
        /// The message's opaque payload bytes. A zero-length payload is
        /// valid and is *not* treated as an error.
        content: Bytes,
    },
}

// ── Message dispatch ───────────────────────────────────────────────────────────

/// Decodes exactly one pgoutput logical-replication message from its
/// complete byte payload, as already extracted from an `XLogData` frame's
/// WAL-data field.
///
/// This is a pure, stateless decode step: it performs no I/O and does not
/// cache [`RelationBody`] schema information between calls (see the
/// module-level "Scope" section). Bytes beyond the decoded
/// message, if any remain in `payload`, are not inspected — the caller is
/// expected to have already isolated exactly one message's bytes.
///
/// # Errors
///
/// Returns [`PgError::Protocol`] if `payload` is empty, starts with an
/// unrecognized tag byte, or is truncated/malformed anywhere while decoding
/// the message body — every multi-byte integer, C-string, and
/// length-prefixed span is bounds-checked against the remaining buffer
/// length before it is read, so malformed or hostile input never panics.
pub fn decode_message(payload: &[u8]) -> Result<LogicalReplicationMessage, PgError> {
    let mut buf = payload;
    let tag = read_u8(&mut buf)?;
    match tag {
        b'B' => decode_begin(&mut buf),
        b'C' => decode_commit(&mut buf),
        b'O' => decode_origin(&mut buf),
        b'R' => decode_relation(&mut buf),
        b'Y' => decode_type(&mut buf),
        b'I' => decode_insert(&mut buf),
        b'U' => decode_update(&mut buf),
        b'D' => decode_delete(&mut buf),
        b'T' => decode_truncate(&mut buf),
        b'M' => decode_logical_message(&mut buf),
        other => Err(protocol_err(format!(
            "unknown pgoutput message tag: 0x{other:02X} ({:?})",
            other as char
        ))),
    }
}

// ── Per-message decoders ───────────────────────────────────────────────────────

/// Decodes a `'B'` (Begin) message body (tag byte already consumed).
fn decode_begin(buf: &mut &[u8]) -> Result<LogicalReplicationMessage, PgError> {
    let final_lsn = Lsn::from_u64(read_u64(buf)?);
    let commit_time = read_i64(buf)?;
    let xid = read_u32(buf)?;
    Ok(LogicalReplicationMessage::Begin {
        final_lsn,
        commit_time,
        xid,
    })
}

/// Decodes a `'C'` (Commit) message body (tag byte already consumed).
fn decode_commit(buf: &mut &[u8]) -> Result<LogicalReplicationMessage, PgError> {
    let flags = read_u8(buf)?;
    let commit_lsn = Lsn::from_u64(read_u64(buf)?);
    let end_lsn = Lsn::from_u64(read_u64(buf)?);
    let commit_time = read_i64(buf)?;
    Ok(LogicalReplicationMessage::Commit {
        flags,
        commit_lsn,
        end_lsn,
        commit_time,
    })
}

/// Decodes an `'O'` (Origin) message body (tag byte already consumed).
fn decode_origin(buf: &mut &[u8]) -> Result<LogicalReplicationMessage, PgError> {
    let commit_lsn = Lsn::from_u64(read_u64(buf)?);
    let name = read_cstring(buf)?;
    Ok(LogicalReplicationMessage::Origin { commit_lsn, name })
}

/// Decodes an `'R'` (Relation) message body (tag byte already consumed).
fn decode_relation(buf: &mut &[u8]) -> Result<LogicalReplicationMessage, PgError> {
    let rel_id = read_u32(buf)?;
    let namespace = read_cstring(buf)?;
    let name = read_cstring(buf)?;

    let replica_identity_byte = read_u8(buf)?;
    let replica_identity = match replica_identity_byte {
        b'd' => ReplicaIdentity::Default,
        b'n' => ReplicaIdentity::Nothing,
        b'f' => ReplicaIdentity::Full,
        b'i' => ReplicaIdentity::Index,
        other => {
            return Err(protocol_err(format!(
                "Relation: unknown REPLICA IDENTITY byte: 0x{other:02X} ({:?})",
                other as char
            )))
        }
    };

    let raw_num_cols = read_i16(buf)?;
    let num_cols = non_negative_usize_16(raw_num_cols, "Relation column count")?;
    // `num_cols` is i16-bounded (<= 32767), so pre-sizing the Vec cannot be
    // used as a denial-of-service vector.
    let mut columns = Vec::with_capacity(num_cols);
    for _ in 0..num_cols {
        let col_flags = read_u8(buf)?;
        let key = col_flags & 0x01 != 0;
        let col_name = read_cstring(buf)?;
        let type_oid = read_u32(buf)?;
        let type_modifier = read_i32(buf)?;
        columns.push(ColumnSpec {
            key,
            name: col_name,
            type_oid,
            type_modifier,
        });
    }

    Ok(LogicalReplicationMessage::Relation(RelationBody {
        rel_id,
        namespace,
        name,
        replica_identity,
        columns,
    }))
}

/// Decodes a `'Y'` (Type) message body (tag byte already consumed).
fn decode_type(buf: &mut &[u8]) -> Result<LogicalReplicationMessage, PgError> {
    let id = read_u32(buf)?;
    let namespace = read_cstring(buf)?;
    let name = read_cstring(buf)?;
    Ok(LogicalReplicationMessage::Type {
        id,
        namespace,
        name,
    })
}

/// Decodes an `'I'` (Insert) message body (tag byte already consumed).
fn decode_insert(buf: &mut &[u8]) -> Result<LogicalReplicationMessage, PgError> {
    let rel_id = read_u32(buf)?;
    let marker = read_u8(buf)?;
    if marker != b'N' {
        return Err(protocol_err(format!(
            "Insert: expected 'N' tuple marker, found 0x{marker:02X} ({:?})",
            marker as char
        )));
    }
    let new_tuple = decode_tuple_data(buf)?;
    Ok(LogicalReplicationMessage::Insert { rel_id, new_tuple })
}

/// Decodes a `'U'` (Update) message body (tag byte already consumed).
///
/// The wire format has no fixed "old tuple present" flag: the byte
/// immediately after `rel_id` is peeked (by reading it) to disambiguate.
/// `'K'`/`'O'` mean an old-row `TupleData` follows, itself followed by a
/// mandatory `'N'` marker and the new-row `TupleData`; `'N'` directly means
/// there is no old-row image and what follows is the new-row `TupleData`.
fn decode_update(buf: &mut &[u8]) -> Result<LogicalReplicationMessage, PgError> {
    let rel_id = read_u32(buf)?;
    let marker = read_u8(buf)?;

    let (old_tuple, old_tuple_is_full, new_tuple) = match marker {
        b'K' | b'O' => {
            let old_tuple = decode_tuple_data(buf)?;
            let new_marker = read_u8(buf)?;
            if new_marker != b'N' {
                return Err(protocol_err(format!(
                    "Update: expected 'N' new-tuple marker after old-row image, found 0x{new_marker:02X} ({:?})",
                    new_marker as char
                )));
            }
            let new_tuple = decode_tuple_data(buf)?;
            (Some(old_tuple), marker == b'O', new_tuple)
        }
        b'N' => {
            let new_tuple = decode_tuple_data(buf)?;
            (None, false, new_tuple)
        }
        other => {
            return Err(protocol_err(format!(
                "Update: expected 'K', 'O', or 'N' marker, found 0x{other:02X} ({:?})",
                other as char
            )))
        }
    };

    Ok(LogicalReplicationMessage::Update {
        rel_id,
        old_tuple,
        old_tuple_is_full,
        new_tuple,
    })
}

/// Decodes a `'D'` (Delete) message body (tag byte already consumed).
fn decode_delete(buf: &mut &[u8]) -> Result<LogicalReplicationMessage, PgError> {
    let rel_id = read_u32(buf)?;
    let marker = read_u8(buf)?;
    let old_tuple_is_full = match marker {
        b'K' => false,
        b'O' => true,
        other => {
            return Err(protocol_err(format!(
                "Delete: expected 'K' or 'O' marker, found 0x{other:02X} ({:?})",
                other as char
            )))
        }
    };
    let old_tuple = decode_tuple_data(buf)?;
    Ok(LogicalReplicationMessage::Delete {
        rel_id,
        old_tuple,
        old_tuple_is_full,
    })
}

/// Decodes a `'T'` (Truncate) message body (tag byte already consumed).
fn decode_truncate(buf: &mut &[u8]) -> Result<LogicalReplicationMessage, PgError> {
    let num_rels = read_u32(buf)?;
    let opt_flags = read_u8(buf)?;
    let cascade = opt_flags & 0x01 != 0;
    let restart_identity = opt_flags & 0x02 != 0;

    // `num_rels` is a wire-supplied u32 (up to ~4 billion) and is NOT used
    // as a `Vec::with_capacity` hint: a hostile payload claiming a huge
    // count backed by a short buffer must fail via `read_u32`'s bounds
    // check on the first missing element, not trigger a multi-gigabyte
    // up-front allocation.
    let mut rel_ids = Vec::new();
    for _ in 0..num_rels {
        rel_ids.push(read_u32(buf)?);
    }

    Ok(LogicalReplicationMessage::Truncate {
        cascade,
        restart_identity,
        rel_ids,
    })
}

/// Decodes an `'M'` (Message) message body (tag byte already consumed).
fn decode_logical_message(buf: &mut &[u8]) -> Result<LogicalReplicationMessage, PgError> {
    let raw_flags = read_u8(buf)?;
    let transactional = raw_flags & 0x01 != 0;
    let lsn = Lsn::from_u64(read_u64(buf)?);
    let prefix = read_cstring(buf)?;

    let raw_content_len = read_u32(buf)?;
    let content_len = usize::try_from(raw_content_len)
        .map_err(|e| protocol_err(format!("Message: content length out of range: {e}")))?;
    // A zero-length payload is valid (empty `content`), not an error;
    // `read_slice` with `len == 0` simply returns an empty slice.
    let content = read_slice(buf, content_len)?;

    Ok(LogicalReplicationMessage::Message {
        transactional,
        lsn,
        prefix,
        content: Bytes::copy_from_slice(content),
    })
}

// ── TupleData decoding ─────────────────────────────────────────────────────────

/// Decodes one `TupleData` span: `i16 num_cols` followed by that many tagged
/// column values. Used by `Insert`/`Update`/`Delete` for both old- and
/// new-row images.
fn decode_tuple_data(buf: &mut &[u8]) -> Result<TupleData, PgError> {
    let raw_num_cols = read_i16(buf)?;
    let num_cols = non_negative_usize_16(raw_num_cols, "TupleData column count")?;
    // `num_cols` is i16-bounded (<= 32767): safe to use as a capacity hint.
    let mut columns = Vec::with_capacity(num_cols);

    for _ in 0..num_cols {
        let tag = read_u8(buf)?;
        let column = match tag {
            b'n' => TupleColumn::Null,
            b'u' => TupleColumn::UnchangedToast,
            b't' => {
                let raw_len = read_i32(buf)?;
                let len = non_negative_usize_32(raw_len, "TupleData Text column length")?;
                let raw = read_slice(buf, len)?;
                let text = std::str::from_utf8(raw).map_err(|e| {
                    protocol_err(format!("TupleData: invalid UTF-8 in Text column: {e}"))
                })?;
                TupleColumn::Text(text.to_string())
            }
            b'b' => {
                let raw_len = read_i32(buf)?;
                let len = non_negative_usize_32(raw_len, "TupleData Binary column length")?;
                let raw = read_slice(buf, len)?;
                TupleColumn::Binary(Bytes::copy_from_slice(raw))
            }
            other => {
                return Err(protocol_err(format!(
                    "TupleData: unknown column tag: 0x{other:02X} ({:?})",
                    other as char
                )))
            }
        };
        columns.push(column);
    }

    Ok(TupleData { columns })
}

// ── Low-level byte-cursor helpers ──────────────────────────────────────────────
//
// Every reader below checks the remaining buffer length *before* calling the
// corresponding accessor, so a short or hostile buffer always yields
// `PgError::Protocol` rather than panicking (`bytes::Buf`'s `get_*` methods
// panic on underflow, which is exactly why the check must come first). This
// mirrors the bounds-checking discipline in `crate::types::decode_pg_numeric`
// / `decode_pg_interval`.

/// Reads one big-endian `u8`.
fn read_u8(buf: &mut &[u8]) -> Result<u8, PgError> {
    if !buf.has_remaining() {
        return Err(protocol_err(
            "truncated message: expected 1 byte, found end of buffer",
        ));
    }
    Ok(buf.get_u8())
}

/// Reads one big-endian `i16`.
fn read_i16(buf: &mut &[u8]) -> Result<i16, PgError> {
    if buf.remaining() < 2 {
        return Err(protocol_err(format!(
            "truncated message: expected 2 bytes (i16), found {} remaining",
            buf.remaining()
        )));
    }
    Ok(buf.get_i16())
}

/// Reads one big-endian `i32`.
fn read_i32(buf: &mut &[u8]) -> Result<i32, PgError> {
    if buf.remaining() < 4 {
        return Err(protocol_err(format!(
            "truncated message: expected 4 bytes (i32), found {} remaining",
            buf.remaining()
        )));
    }
    Ok(buf.get_i32())
}

/// Reads one big-endian `u32`.
fn read_u32(buf: &mut &[u8]) -> Result<u32, PgError> {
    if buf.remaining() < 4 {
        return Err(protocol_err(format!(
            "truncated message: expected 4 bytes (u32), found {} remaining",
            buf.remaining()
        )));
    }
    Ok(buf.get_u32())
}

/// Reads one big-endian `i64`.
fn read_i64(buf: &mut &[u8]) -> Result<i64, PgError> {
    if buf.remaining() < 8 {
        return Err(protocol_err(format!(
            "truncated message: expected 8 bytes (i64), found {} remaining",
            buf.remaining()
        )));
    }
    Ok(buf.get_i64())
}

/// Reads one big-endian `u64` (used for raw LSN bit patterns; wrap the
/// result with [`Lsn::from_u64`]).
fn read_u64(buf: &mut &[u8]) -> Result<u64, PgError> {
    if buf.remaining() < 8 {
        return Err(protocol_err(format!(
            "truncated message: expected 8 bytes (u64), found {} remaining",
            buf.remaining()
        )));
    }
    Ok(buf.get_u64())
}

/// Reads and returns a borrowed slice of exactly `len` bytes, advancing the
/// cursor past it. Returns [`PgError::Protocol`] if fewer than `len` bytes
/// remain — checked before any slicing occurs, so a huge claimed `len`
/// backed by a short buffer fails cleanly instead of ever being used to
/// size an allocation.
fn read_slice<'a>(buf: &mut &'a [u8], len: usize) -> Result<&'a [u8], PgError> {
    if buf.len() < len {
        return Err(protocol_err(format!(
            "truncated message: expected {len} bytes, found {} remaining",
            buf.len()
        )));
    }
    let (taken, rest) = buf.split_at(len);
    *buf = rest;
    Ok(taken)
}

/// Reads a null-terminated C-string: bytes up to (excluding) the next
/// `0x00`, interpreted as UTF-8. Advances the cursor past the terminator.
///
/// Returns [`PgError::Protocol`] if no `0x00` byte appears before the buffer
/// ends, or if the bytes preceding it are not valid UTF-8.
fn read_cstring(buf: &mut &[u8]) -> Result<String, PgError> {
    let Some(nul_pos) = buf.iter().position(|&b| b == 0) else {
        return Err(protocol_err(
            "unterminated string: no NUL terminator before end of buffer",
        ));
    };
    let raw = &buf[..nul_pos];
    let s = std::str::from_utf8(raw)
        .map_err(|e| protocol_err(format!("invalid UTF-8 in string: {e}")))?
        .to_string();
    *buf = &buf[nul_pos + 1..];
    Ok(s)
}

/// Converts a wire-format signed 16-bit count/length to `usize`, rejecting
/// negative values as [`PgError::Protocol`]. This wire format never uses a
/// negative count/length as a sentinel (e.g. `NULL` is exclusively signaled
/// by the `TupleData` `'n'` tag, not by a negative length).
fn non_negative_usize_16(raw: i16, what: &str) -> Result<usize, PgError> {
    if raw < 0 {
        return Err(protocol_err(format!(
            "{what}: negative count/length ({raw})"
        )));
    }
    usize::try_from(raw).map_err(|e| protocol_err(format!("{what}: out of range: {e}")))
}

/// As [`non_negative_usize_16`], for wire-format signed 32-bit
/// counts/lengths.
fn non_negative_usize_32(raw: i32, what: &str) -> Result<usize, PgError> {
    if raw < 0 {
        return Err(protocol_err(format!(
            "{what}: negative count/length ({raw})"
        )));
    }
    usize::try_from(raw).map_err(|e| protocol_err(format!("{what}: out of range: {e}")))
}

/// Builds a [`PgError::Protocol`] from any string-like message.
fn protocol_err(msg: impl Into<String>) -> PgError {
    PgError::Protocol(msg.into())
}

// ── Tests ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use bytes::{BufMut, BytesMut};

    use super::*;

    // ── Fixture builders ────────────────────────────────────────────────────

    /// Appends a null-terminated C-string.
    fn push_cstring(buf: &mut BytesMut, s: &str) {
        buf.put_slice(s.as_bytes());
        buf.put_u8(0);
    }

    /// Appends a `TupleData` span with two `Text` columns: `i16 num_cols=2`
    /// followed by `'t' i32 len bytes` for each of `a` and `b`.
    fn push_tuple_data_2_text(buf: &mut BytesMut, a: &str, b: &str) {
        buf.put_i16(2);
        for s in [a, b] {
            buf.put_u8(b't');
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            buf.put_i32(s.len() as i32);
            buf.put_slice(s.as_bytes());
        }
    }

    // ── Begin ────────────────────────────────────────────────────────────────

    #[test]
    fn decode_begin_message() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'B');
        buf.put_u64(0x0000_0016_B374_D848);
        buf.put_i64(123_456_789_012);
        buf.put_u32(4242);

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Begin {
                final_lsn: Lsn::from_u64(0x0000_0016_B374_D848),
                commit_time: 123_456_789_012,
                xid: 4242,
            }
        );
    }

    // ── Commit ───────────────────────────────────────────────────────────────

    #[test]
    fn decode_commit_message() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'C');
        buf.put_u8(0);
        buf.put_u64(0x1000);
        buf.put_u64(0x2000);
        buf.put_i64(999);

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Commit {
                flags: 0,
                commit_lsn: Lsn::from_u64(0x1000),
                end_lsn: Lsn::from_u64(0x2000),
                commit_time: 999,
            }
        );
    }

    // ── Origin ───────────────────────────────────────────────────────────────

    #[test]
    fn decode_origin_message() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'O');
        buf.put_u64(0xABCD);
        push_cstring(&mut buf, "my_origin");

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Origin {
                commit_lsn: Lsn::from_u64(0xABCD),
                name: "my_origin".to_string(),
            }
        );
    }

    // ── Type ─────────────────────────────────────────────────────────────────

    #[test]
    fn decode_type_message() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'Y');
        buf.put_u32(16384);
        push_cstring(&mut buf, "public");
        push_cstring(&mut buf, "my_enum");

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Type {
                id: 16384,
                namespace: "public".to_string(),
                name: "my_enum".to_string(),
            }
        );
    }

    // ── Relation ─────────────────────────────────────────────────────────────

    #[test]
    fn decode_relation_zero_columns() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'R');
        buf.put_u32(16385);
        push_cstring(&mut buf, "public");
        push_cstring(&mut buf, "empty_table");
        buf.put_u8(b'd');
        buf.put_i16(0);

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Relation(RelationBody {
                rel_id: 16385,
                namespace: "public".to_string(),
                name: "empty_table".to_string(),
                replica_identity: ReplicaIdentity::Default,
                columns: vec![],
            })
        );
    }

    #[test]
    fn decode_relation_three_columns_mixed_keys() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'R');
        buf.put_u32(16400);
        push_cstring(&mut buf, "app");
        push_cstring(&mut buf, "users");
        buf.put_u8(b'i');
        buf.put_i16(3);
        // col 1: key, "id", oid 23 (int4), atttypmod -1
        buf.put_u8(0x01);
        push_cstring(&mut buf, "id");
        buf.put_u32(23);
        buf.put_i32(-1);
        // col 2: non-key, "name", oid 25 (text), atttypmod -1
        buf.put_u8(0x00);
        push_cstring(&mut buf, "name");
        buf.put_u32(25);
        buf.put_i32(-1);
        // col 3: key, "tenant_id", oid 23, atttypmod -1
        buf.put_u8(0x01);
        push_cstring(&mut buf, "tenant_id");
        buf.put_u32(23);
        buf.put_i32(-1);

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Relation(RelationBody {
                rel_id: 16400,
                namespace: "app".to_string(),
                name: "users".to_string(),
                replica_identity: ReplicaIdentity::Index,
                columns: vec![
                    ColumnSpec {
                        key: true,
                        name: "id".to_string(),
                        type_oid: 23,
                        type_modifier: -1,
                    },
                    ColumnSpec {
                        key: false,
                        name: "name".to_string(),
                        type_oid: 25,
                        type_modifier: -1,
                    },
                    ColumnSpec {
                        key: true,
                        name: "tenant_id".to_string(),
                        type_oid: 23,
                        type_modifier: -1,
                    },
                ],
            })
        );
    }

    #[test]
    fn decode_relation_replica_identity_nothing() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'R');
        buf.put_u32(1);
        push_cstring(&mut buf, "s");
        push_cstring(&mut buf, "t");
        buf.put_u8(b'n');
        buf.put_i16(0);

        let msg = decode_message(&buf).unwrap();
        let LogicalReplicationMessage::Relation(body) = msg else {
            panic!("expected Relation, got {msg:?}");
        };
        assert_eq!(body.replica_identity, ReplicaIdentity::Nothing);
    }

    #[test]
    fn decode_relation_replica_identity_full() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'R');
        buf.put_u32(1);
        push_cstring(&mut buf, "s");
        push_cstring(&mut buf, "t");
        buf.put_u8(b'f');
        buf.put_i16(0);

        let msg = decode_message(&buf).unwrap();
        let LogicalReplicationMessage::Relation(body) = msg else {
            panic!("expected Relation, got {msg:?}");
        };
        assert_eq!(body.replica_identity, ReplicaIdentity::Full);
    }

    // ── Insert ───────────────────────────────────────────────────────────────

    #[test]
    fn decode_insert_message() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'I');
        buf.put_u32(16400);
        buf.put_u8(b'N');
        push_tuple_data_2_text(&mut buf, "42", "alice");

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Insert {
                rel_id: 16400,
                new_tuple: TupleData {
                    columns: vec![
                        TupleColumn::Text("42".to_string()),
                        TupleColumn::Text("alice".to_string()),
                    ],
                },
            }
        );
    }

    #[test]
    fn decode_insert_tuple_data_all_tag_kinds() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'I');
        buf.put_u32(1);
        buf.put_u8(b'N');
        buf.put_i16(4);
        buf.put_u8(b'n'); // Null
        buf.put_u8(b'u'); // UnchangedToast
        buf.put_u8(b't');
        buf.put_i32(5);
        buf.put_slice(b"hello");
        buf.put_u8(b'b');
        buf.put_i32(3);
        buf.put_slice(&[0xDE, 0xAD, 0xBE]);

        let msg = decode_message(&buf).unwrap();
        let LogicalReplicationMessage::Insert { new_tuple, .. } = msg else {
            panic!("expected Insert, got {msg:?}");
        };
        assert_eq!(new_tuple.columns.len(), 4);
        assert_eq!(new_tuple.columns[0], TupleColumn::Null);
        assert_eq!(new_tuple.columns[1], TupleColumn::UnchangedToast);
        // `UnchangedToast` must be a distinct variant from `Null`, not
        // collapsed into it.
        assert_ne!(new_tuple.columns[0], new_tuple.columns[1]);
        assert_eq!(new_tuple.columns[2], TupleColumn::Text("hello".to_string()));
        assert_eq!(
            new_tuple.columns[3],
            TupleColumn::Binary(Bytes::copy_from_slice(&[0xDE, 0xAD, 0xBE]))
        );
    }

    // ── Update ───────────────────────────────────────────────────────────────

    #[test]
    fn decode_update_no_old_tuple() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'U');
        buf.put_u32(1);
        buf.put_u8(b'N');
        push_tuple_data_2_text(&mut buf, "1", "new_value");

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Update {
                rel_id: 1,
                old_tuple: None,
                old_tuple_is_full: false,
                new_tuple: TupleData {
                    columns: vec![
                        TupleColumn::Text("1".to_string()),
                        TupleColumn::Text("new_value".to_string()),
                    ],
                },
            }
        );
    }

    #[test]
    fn decode_update_key_only_old_tuple() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'U');
        buf.put_u32(1);
        buf.put_u8(b'K');
        push_tuple_data_2_text(&mut buf, "1", "old_key_value");
        buf.put_u8(b'N');
        push_tuple_data_2_text(&mut buf, "1", "new_value");

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Update {
                rel_id: 1,
                old_tuple: Some(TupleData {
                    columns: vec![
                        TupleColumn::Text("1".to_string()),
                        TupleColumn::Text("old_key_value".to_string()),
                    ],
                }),
                old_tuple_is_full: false,
                new_tuple: TupleData {
                    columns: vec![
                        TupleColumn::Text("1".to_string()),
                        TupleColumn::Text("new_value".to_string()),
                    ],
                },
            }
        );
    }

    #[test]
    fn decode_update_full_old_tuple() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'U');
        buf.put_u32(1);
        buf.put_u8(b'O');
        push_tuple_data_2_text(&mut buf, "1", "old_full_value");
        buf.put_u8(b'N');
        push_tuple_data_2_text(&mut buf, "1", "new_value");

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Update {
                rel_id: 1,
                old_tuple: Some(TupleData {
                    columns: vec![
                        TupleColumn::Text("1".to_string()),
                        TupleColumn::Text("old_full_value".to_string()),
                    ],
                }),
                old_tuple_is_full: true,
                new_tuple: TupleData {
                    columns: vec![
                        TupleColumn::Text("1".to_string()),
                        TupleColumn::Text("new_value".to_string()),
                    ],
                },
            }
        );
    }

    // ── Delete ───────────────────────────────────────────────────────────────

    #[test]
    fn decode_delete_key_only() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'D');
        buf.put_u32(1);
        buf.put_u8(b'K');
        push_tuple_data_2_text(&mut buf, "1", "x");

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Delete {
                rel_id: 1,
                old_tuple: TupleData {
                    columns: vec![
                        TupleColumn::Text("1".to_string()),
                        TupleColumn::Text("x".to_string()),
                    ],
                },
                old_tuple_is_full: false,
            }
        );
    }

    #[test]
    fn decode_delete_full() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'D');
        buf.put_u32(1);
        buf.put_u8(b'O');
        push_tuple_data_2_text(&mut buf, "1", "x");

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Delete {
                rel_id: 1,
                old_tuple: TupleData {
                    columns: vec![
                        TupleColumn::Text("1".to_string()),
                        TupleColumn::Text("x".to_string()),
                    ],
                },
                old_tuple_is_full: true,
            }
        );
    }

    // ── Truncate ─────────────────────────────────────────────────────────────

    #[test]
    fn decode_truncate_no_flags_single_rel() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'T');
        buf.put_u32(1);
        buf.put_u8(0x00);
        buf.put_u32(100);

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Truncate {
                cascade: false,
                restart_identity: false,
                rel_ids: vec![100],
            }
        );
    }

    #[test]
    fn decode_truncate_cascade_only() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'T');
        buf.put_u32(1);
        buf.put_u8(0x01);
        buf.put_u32(200);

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Truncate {
                cascade: true,
                restart_identity: false,
                rel_ids: vec![200],
            }
        );
    }

    #[test]
    fn decode_truncate_restart_identity_only() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'T');
        buf.put_u32(1);
        buf.put_u8(0x02);
        buf.put_u32(300);

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Truncate {
                cascade: false,
                restart_identity: true,
                rel_ids: vec![300],
            }
        );
    }

    #[test]
    fn decode_truncate_both_flags_multiple_rels() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'T');
        buf.put_u32(3);
        buf.put_u8(0x03);
        buf.put_u32(10);
        buf.put_u32(20);
        buf.put_u32(30);

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Truncate {
                cascade: true,
                restart_identity: true,
                rel_ids: vec![10, 20, 30],
            }
        );
    }

    // ── Message ──────────────────────────────────────────────────────────────

    #[test]
    fn decode_logical_message_transactional() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'M');
        buf.put_u8(0x01);
        buf.put_u64(0x5000);
        push_cstring(&mut buf, "my_app");
        let content = b"hello world";
        buf.put_u32(u32::try_from(content.len()).unwrap());
        buf.put_slice(content);

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Message {
                transactional: true,
                lsn: Lsn::from_u64(0x5000),
                prefix: "my_app".to_string(),
                content: Bytes::copy_from_slice(content),
            }
        );
    }

    #[test]
    fn decode_logical_message_non_transactional() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'M');
        buf.put_u8(0x00);
        buf.put_u64(0x6000);
        push_cstring(&mut buf, "my_app");
        let content = b"non-tx";
        buf.put_u32(u32::try_from(content.len()).unwrap());
        buf.put_slice(content);

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Message {
                transactional: false,
                lsn: Lsn::from_u64(0x6000),
                prefix: "my_app".to_string(),
                content: Bytes::copy_from_slice(content),
            }
        );
    }

    #[test]
    fn decode_logical_message_empty_content() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'M');
        buf.put_u8(0x01);
        buf.put_u64(0x7000);
        push_cstring(&mut buf, "empty");
        buf.put_u32(0);

        let msg = decode_message(&buf).unwrap();
        assert_eq!(
            msg,
            LogicalReplicationMessage::Message {
                transactional: true,
                lsn: Lsn::from_u64(0x7000),
                prefix: "empty".to_string(),
                content: Bytes::new(),
            }
        );
    }

    // ── Error paths ──────────────────────────────────────────────────────────

    #[test]
    fn decode_err_unknown_top_level_tag() {
        let buf = [b'Z', 0, 0, 0, 0];
        let err = decode_message(&buf).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)));
    }

    #[test]
    fn decode_err_unknown_replica_identity_byte() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'R');
        buf.put_u32(1);
        push_cstring(&mut buf, "s");
        push_cstring(&mut buf, "t");
        buf.put_u8(b'x'); // not one of 'd'/'n'/'f'/'i'
        buf.put_i16(0);

        let err = decode_message(&buf).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)));
    }

    #[test]
    fn decode_err_unknown_tuple_data_tag() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'I');
        buf.put_u32(1);
        buf.put_u8(b'N');
        buf.put_i16(1);
        buf.put_u8(b'z'); // not one of 'n'/'u'/'t'/'b'

        let err = decode_message(&buf).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)));
    }

    #[test]
    fn decode_err_empty_payload() {
        let err = decode_message(&[]).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)));
    }

    #[test]
    fn decode_err_cstring_missing_terminator() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'O');
        buf.put_u64(1);
        buf.put_slice(b"no_terminator"); // no trailing 0x00

        let err = decode_message(&buf).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)));
    }

    #[test]
    fn decode_err_truncated_mid_integer() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'B');
        buf.put_slice(&[0, 0, 0]); // final_lsn needs 8 bytes, only 3 given

        let err = decode_message(&buf).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)));
    }

    #[test]
    fn decode_err_negative_tuple_column_length() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'I');
        buf.put_u32(1);
        buf.put_u8(b'N');
        buf.put_i16(1);
        buf.put_u8(b't');
        buf.put_i32(-1); // negative length: Protocol error, not a NULL sentinel

        let err = decode_message(&buf).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)));
    }

    #[test]
    fn decode_err_update_invalid_marker_byte() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'U');
        buf.put_u32(1);
        buf.put_u8(b'X'); // not 'K', 'O', or 'N'

        let err = decode_message(&buf).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)));
    }

    #[test]
    fn decode_err_insert_wrong_tuple_marker() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'I');
        buf.put_u32(1);
        buf.put_u8(b'X'); // must be 'N'

        let err = decode_message(&buf).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)));
    }

    #[test]
    fn decode_err_message_content_length_exceeds_buffer() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'M');
        buf.put_u8(0x00);
        buf.put_u64(1);
        push_cstring(&mut buf, "p");
        buf.put_u32(1000); // claims 1000 bytes of content; none follow

        let err = decode_message(&buf).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)));
    }

    #[test]
    fn decode_err_truncate_rel_ids_cut_short() {
        let mut buf = BytesMut::new();
        buf.put_u8(b'T');
        buf.put_u32(5); // claims 5 rel_ids
        buf.put_u8(0x00);
        buf.put_u32(1); // but only 1 is actually provided

        let err = decode_message(&buf).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)));
    }
}

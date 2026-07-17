//! CopyBoth wire-protocol layer for PostgreSQL streaming/logical replication.
//!
//! PostgreSQL's replication protocol runs inside a `CopyBoth` session: after
//! the client issues `START_REPLICATION`, the server switches the connection
//! into `CopyBoth` mode (`Byte1('W')` response) and both sides exchange
//! `CopyData` (`Byte1('d')`) frames until the session ends.  Each `CopyData`
//! frame carries one of a handful of replication sub-messages, distinguished
//! by their first content byte:
//!
//! - `'w'` -- [`XLogData`]: a chunk of WAL data (for logical replication,
//!   exactly one `pgoutput` message) plus the WAL positions that bracket it.
//! - `'k'` -- [`PrimaryKeepAlive`]: the server's periodic liveness/flush-lag
//!   probe, optionally requesting an immediate standby reply.
//! - `'r'` -- a Standby Status Update, sent client -> server (this module
//!   only encodes it, via [`encode_standby_status_update`]; the server never
//!   sends one, so there is no corresponding parser here).
//!
//! This module is a pure decode/encode layer: it operates on raw bytes and
//! has no knowledge of sockets, TLS, or connection state.  Callers are
//! expected to:
//!
//! 1. Read the raw backend byte stream and use
//!    [`postgres_protocol::message::backend::Header::parse`] to frame
//!    messages (1-byte tag + 4-byte big-endian length, where the length
//!    counts its own 4 bytes but not the tag byte).
//! 2. On tag `'W'`, hand the message body (everything after the tag and
//!    length prefix) to [`parse_copy_both_response`] to complete the
//!    `CopyBoth` handshake.
//! 3. On tag `'d'` thereafter, hand the message body -- e.g. via
//!    [`postgres_protocol::message::backend::CopyDataBody::data`] once the
//!    frame has been decoded with
//!    [`postgres_protocol::message::backend::Message::parse`] -- to
//!    [`parse_copy_both_message`], which dispatches on the sub-message's
//!    leading byte to [`parse_xlog_data`] or [`parse_primary_keepalive`].
//! 4. For the `'w'` case, hand the resulting [`XLogData`]'s `data` bytes on
//!    to the `pgoutput` logical-decoding layer (built separately in a
//!    sibling module) -- this module's responsibility ends at producing the
//!    raw `XLogData` payload.
//!
//! See `copy.rs` for the analogous (non-replication) `CopyIn`/`CopyOut`
//! protocol support, whose house style (bytes handling, no-`unwrap()`
//! discipline) this module follows.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use postgres_protocol::message::frontend;

use super::lsn::Lsn;
use crate::error::PgError;

// ── CopyBothResponse ─────────────────────────────────────────────────────────

/// The parsed body of a `CopyBothResponse` (`Byte1('W')`) backend message.
///
/// Sent once, immediately after `START_REPLICATION` succeeds, to switch the
/// connection into `CopyBoth` mode.  Its body has the same layout as
/// `CopyOutResponse`'s (see `CopyOutResponseBody` in `postgres-protocol`'s
/// `backend` module): an overall format code followed by a per-column format
/// code list.  In practice, replication always uses `format == 0` (text) at
/// this framing level -- the WAL data itself is opaque binary regardless --
/// and `column_formats` is empty, since replication rows have no column list
/// at the protocol level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyBothResponse {
    /// The overall copy format: `0` for text, `1` for binary. Always `0` for
    /// replication sessions.
    pub format: i8,
    /// Per-column format codes. Typically empty for replication.
    pub column_formats: Vec<i16>,
}

/// Parses a `CopyBothResponse` (`Byte1('W')`) message body.
///
/// `buf` is the message body *after* the caller has already stripped the
/// `'W'` tag byte and the 4-byte length prefix (per
/// [`postgres_protocol::message::backend::Header::parse`]).
///
/// Wire layout: `Int8 format`, `Int16 num_columns`, then `num_columns *
/// Int16` per-column format codes -- identical to `CopyOutResponse`'s body,
/// only the outer tag byte differs.
///
/// # Errors
///
/// Returns [`PgError::Protocol`] if `buf` is truncated at any field
/// boundary, or if it has unexpected trailing bytes after the last column
/// format code (the body's length is fully determined by its own fields, so
/// anything left over indicates a malformed message).
pub fn parse_copy_both_response(buf: &[u8]) -> Result<CopyBothResponse, PgError> {
    let mut cursor = buf;

    if cursor.remaining() < 1 {
        return Err(PgError::Protocol(
            "CopyBothResponse truncated: missing format byte".to_string(),
        ));
    }
    let format = cursor.get_i8();

    if cursor.remaining() < 2 {
        return Err(PgError::Protocol(
            "CopyBothResponse truncated: missing column count".to_string(),
        ));
    }
    let num_columns = cursor.get_u16() as usize;

    let mut column_formats = Vec::with_capacity(num_columns);
    for i in 0..num_columns {
        if cursor.remaining() < 2 {
            return Err(PgError::Protocol(format!(
                "CopyBothResponse truncated: missing format code for column {i} of {num_columns}"
            )));
        }
        column_formats.push(cursor.get_i16());
    }

    if cursor.has_remaining() {
        return Err(PgError::Protocol(format!(
            "CopyBothResponse has {} unexpected trailing byte(s) after {num_columns} column \
             format code(s)",
            cursor.remaining()
        )));
    }

    Ok(CopyBothResponse {
        format,
        column_formats,
    })
}

// ── XLogData ──────────────────────────────────────────────────────────────────

/// The parsed body of an `XLogData` (`CopyData` sub-message tag `'w'`)
/// replication message.
///
/// Carries one chunk of WAL data -- for logical replication, exactly one
/// `pgoutput` message -- bracketed by the WAL positions it spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XLogData {
    /// The starting WAL position of this chunk of data.
    pub wal_start: Lsn,
    /// The current end-of-WAL position on the server (not necessarily the
    /// end of this specific chunk).
    pub wal_end: Lsn,
    /// The server's clock reading at the time this message was sent, in
    /// PostgreSQL-epoch microseconds (microseconds since `2000-01-01
    /// 00:00:00 UTC`). This is left unconverted; use `pg_micros_to_unix_micros`
    /// (in `lsn.rs`) to convert to Unix-epoch microseconds if needed.
    pub server_time: i64,
    /// The WAL payload itself. For logical replication this is exactly one
    /// `pgoutput` message.
    pub data: Bytes,
}

/// Parses an `XLogData` (`CopyData` sub-message tag `'w'`) payload.
///
/// `payload` is the full `CopyData` sub-message body, starting with the
/// `'w'` tag byte (i.e. the outer `CopyData` `'d'` tag and its 4-byte length
/// prefix have already been stripped by the caller).
///
/// Wire layout: `Byte1('w')`, `Int64 wal_start`, `Int64 wal_end`, `Int64
/// server_time`, then all remaining bytes are `data`. A zero-length `data`
/// tail is legal.
///
/// # Errors
///
/// Returns [`PgError::Protocol`] if the leading tag byte is not `'w'`, or if
/// `payload` is truncated before the fixed-size header (tag + 3 `Int64`s) is
/// complete.
pub fn parse_xlog_data(payload: &[u8]) -> Result<XLogData, PgError> {
    let mut cursor = payload;

    if cursor.remaining() < 1 {
        return Err(PgError::Protocol(
            "XLogData truncated: missing sub-message tag byte".to_string(),
        ));
    }
    let tag = cursor.get_u8();
    if tag != b'w' {
        return Err(PgError::Protocol(format!(
            "XLogData: expected sub-message tag 'w' (0x77), found {tag:#04x}"
        )));
    }

    if cursor.remaining() < 8 {
        return Err(PgError::Protocol(
            "XLogData truncated: missing wal_start".to_string(),
        ));
    }
    let wal_start = Lsn::from_u64(cursor.get_u64());

    if cursor.remaining() < 8 {
        return Err(PgError::Protocol(
            "XLogData truncated: missing wal_end".to_string(),
        ));
    }
    let wal_end = Lsn::from_u64(cursor.get_u64());

    if cursor.remaining() < 8 {
        return Err(PgError::Protocol(
            "XLogData truncated: missing server_time".to_string(),
        ));
    }
    let server_time = cursor.get_i64();

    // Everything left is the WAL payload; zero-length data is legal (e.g. it
    // cannot occur for real pgoutput messages, but nothing here assumes
    // otherwise).
    let data = Bytes::copy_from_slice(cursor);

    Ok(XLogData {
        wal_start,
        wal_end,
        server_time,
        data,
    })
}

// ── PrimaryKeepAlive ─────────────────────────────────────────────────────────

/// The parsed body of a Primary Keepalive (`CopyData` sub-message tag `'k'`)
/// message.
///
/// Sent periodically by the server (and immediately upon request) to report
/// its current WAL end position and probe whether the standby is still
/// connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryKeepAlive {
    /// The current end-of-WAL position on the server.
    pub wal_end: Lsn,
    /// The server's clock reading at the time this message was sent, in
    /// PostgreSQL-epoch microseconds.
    pub server_time: i64,
    /// Whether the server is requesting an immediate Standby Status Update
    /// reply.
    pub reply_requested: bool,
}

/// Parses a Primary Keepalive (`CopyData` sub-message tag `'k'`) payload.
///
/// `payload` is the full `CopyData` sub-message body, starting with the
/// `'k'` tag byte.
///
/// Wire layout: `Byte1('k')`, `Int64 wal_end`, `Int64 server_time`, `Byte1
/// reply_requested`.
///
/// Per PostgreSQL's documented convention `0` means false and `1` means
/// true, but this parser treats *any* nonzero byte as `true` rather than
/// requiring an exact match against `1`, in case some server implementation
/// ever sets a different nonzero value; only `0x00` is treated as `false`.
///
/// # Errors
///
/// Returns [`PgError::Protocol`] if the leading tag byte is not `'k'`, if
/// `payload` is truncated at any field boundary, or if it has unexpected
/// trailing bytes after `reply_requested` (this sub-message has a fixed
/// length).
pub fn parse_primary_keepalive(payload: &[u8]) -> Result<PrimaryKeepAlive, PgError> {
    let mut cursor = payload;

    if cursor.remaining() < 1 {
        return Err(PgError::Protocol(
            "PrimaryKeepAlive truncated: missing sub-message tag byte".to_string(),
        ));
    }
    let tag = cursor.get_u8();
    if tag != b'k' {
        return Err(PgError::Protocol(format!(
            "PrimaryKeepAlive: expected sub-message tag 'k' (0x6b), found {tag:#04x}"
        )));
    }

    if cursor.remaining() < 8 {
        return Err(PgError::Protocol(
            "PrimaryKeepAlive truncated: missing wal_end".to_string(),
        ));
    }
    let wal_end = Lsn::from_u64(cursor.get_u64());

    if cursor.remaining() < 8 {
        return Err(PgError::Protocol(
            "PrimaryKeepAlive truncated: missing server_time".to_string(),
        ));
    }
    let server_time = cursor.get_i64();

    if cursor.remaining() < 1 {
        return Err(PgError::Protocol(
            "PrimaryKeepAlive truncated: missing reply_requested byte".to_string(),
        ));
    }
    let reply_requested = cursor.get_u8() != 0;

    if cursor.has_remaining() {
        return Err(PgError::Protocol(format!(
            "PrimaryKeepAlive has {} unexpected trailing byte(s)",
            cursor.remaining()
        )));
    }

    Ok(PrimaryKeepAlive {
        wal_end,
        server_time,
        reply_requested,
    })
}

// ── CopyBothMessage dispatcher ───────────────────────────────────────────────

/// A decoded `CopyData` sub-message received during `CopyBoth` replication
/// streaming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyBothMessage {
    /// A chunk of WAL data (sub-message tag `'w'`).
    XLogData(XLogData),
    /// A server liveness probe (sub-message tag `'k'`).
    KeepAlive(PrimaryKeepAlive),
}

/// Parses a `CopyData` sub-message body received during `CopyBoth`
/// replication streaming, dispatching on its leading sub-message tag byte.
///
/// `payload` is the full `CopyData` message body (i.e. what
/// [`postgres_protocol::message::backend::CopyDataBody::data`] returns).
///
/// # Errors
///
/// Returns [`PgError::Protocol`] if `payload` is empty, or if its leading
/// byte is not a recognized sub-message tag (`'w'` or `'k'`). Propagates any
/// error from [`parse_xlog_data`] or [`parse_primary_keepalive`].
pub fn parse_copy_both_message(payload: &[u8]) -> Result<CopyBothMessage, PgError> {
    match payload.first() {
        None => Err(PgError::Protocol(
            "CopyData sub-message payload is empty".to_string(),
        )),
        Some(b'w') => parse_xlog_data(payload).map(CopyBothMessage::XLogData),
        Some(b'k') => parse_primary_keepalive(payload).map(CopyBothMessage::KeepAlive),
        Some(other) => Err(PgError::Protocol(format!(
            "unexpected CopyData sub-message tag: {other:#04x}"
        ))),
    }
}

// ── Standby Status Update (client -> server) ────────────────────────────────

/// Byte length of the Standby Status Update sub-message body (excluding the
/// outer `CopyData` envelope): `1` (tag) + `8` (written) + `8` (flushed) +
/// `8` (applied) + `8` (client_time) + `1` (reply_requested) = `34`.
const STANDBY_STATUS_UPDATE_BODY_LEN: usize = 34;

/// Value of the outer `CopyData` frame's `Int32` length field for a Standby
/// Status Update: the message length counts its own 4 bytes plus the
/// [`STANDBY_STATUS_UPDATE_BODY_LEN`]-byte body (34 + 4 = 38). Defined as a
/// literal (rather than derived via an `as i32` cast at runtime) so the
/// value is fixed at compile time and no numeric cast is needed.
const STANDBY_STATUS_UPDATE_FRAME_LEN: i32 = 34 + 4;

/// Encodes a Standby Status Update (`CopyData` sub-message tag `'r'`),
/// wrapped in its outer `CopyData` (`Byte1('d')`) envelope, ready to write to
/// the socket.
///
/// This is the client -> server counterpart to [`PrimaryKeepAlive`]: it
/// reports how far the standby has written, flushed, and applied WAL, and
/// optionally answers a keepalive's reply request.
///
/// Wire layout of the sub-message body: `Byte1('r')`, `Int64 written_lsn`,
/// `Int64 flushed_lsn`, `Int64 applied_lsn`, `Int64 client_time`, `Byte1
/// reply_requested` (`1` or `0`) -- 34 bytes, wrapped in a `CopyData` frame
/// via [`postgres_protocol::message::frontend::CopyData`].
pub fn encode_standby_status_update(
    written_lsn: Lsn,
    flushed_lsn: Lsn,
    applied_lsn: Lsn,
    client_time: i64,
    reply_requested: bool,
) -> Bytes {
    let mut body_buf = BytesMut::with_capacity(STANDBY_STATUS_UPDATE_BODY_LEN);
    body_buf.put_u8(b'r');
    body_buf.put_u64(written_lsn.as_u64());
    body_buf.put_u64(flushed_lsn.as_u64());
    body_buf.put_u64(applied_lsn.as_u64());
    body_buf.put_i64(client_time);
    body_buf.put_u8(u8::from(reply_requested));
    // `Bytes::freeze` is an O(1) conversion (no copy): it just hands over
    // ownership of the same underlying allocation, refcounted from here on.
    let body = body_buf.freeze();

    let mut framed = BytesMut::with_capacity(5 + STANDBY_STATUS_UPDATE_BODY_LEN);
    // `body.clone()` is an O(1) refcount bump (`Bytes` is refcounted), not a
    // memory copy, so trying the library helper first and keeping a fallback
    // copy of `body` around is effectively free.
    if let Ok(copy_data) = frontend::CopyData::new(body.clone()) {
        copy_data.write(&mut framed);
    } else {
        // `CopyData::new` only errors if the body length overflows `i32`;
        // our body is a fixed 34 bytes, so this branch is unreachable in
        // practice. Frame by hand instead of relying on that invariant
        // (e.g. via `.unwrap()`), so this function stays infallible and
        // panic-free.
        framed.put_u8(b'd');
        framed.put_i32(STANDBY_STATUS_UPDATE_FRAME_LEN);
        framed.put_slice(&body);
    }
    framed.freeze()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_copy_both_response ─────────────────────────────────────────────

    #[test]
    fn copy_both_response_valid_empty_columns() {
        let buf = [0x00, 0x00, 0x00]; // format=text, num_columns=0
        let result = parse_copy_both_response(&buf).unwrap();
        assert_eq!(
            result,
            CopyBothResponse {
                format: 0,
                column_formats: vec![],
            }
        );
    }

    #[test]
    fn copy_both_response_valid_with_columns() {
        let buf = [
            0x00, // format = text
            0x00, 0x02, // num_columns = 2
            0x00, 0x00, // column format 0 = 0
            0x00, 0x01, // column format 1 = 1
        ];
        let result = parse_copy_both_response(&buf).unwrap();
        assert_eq!(
            result,
            CopyBothResponse {
                format: 0,
                column_formats: vec![0, 1],
            }
        );
    }

    #[test]
    fn copy_both_response_binary_format() {
        let buf = [0x01, 0x00, 0x00]; // format=binary, num_columns=0
        let result = parse_copy_both_response(&buf).unwrap();
        assert_eq!(result.format, 1);
    }

    #[test]
    fn copy_both_response_truncated_missing_format() {
        let buf: [u8; 0] = [];
        assert!(matches!(
            parse_copy_both_response(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn copy_both_response_truncated_missing_column_count() {
        let buf = [0x00]; // format only
        assert!(matches!(
            parse_copy_both_response(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn copy_both_response_truncated_partial_column_count() {
        let buf = [0x00, 0x00]; // format + 1 of 2 count bytes
        assert!(matches!(
            parse_copy_both_response(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn copy_both_response_truncated_missing_column_formats() {
        let buf = [0x00, 0x00, 0x02]; // format + count=2, no format codes
        assert!(matches!(
            parse_copy_both_response(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn copy_both_response_truncated_partial_column_formats() {
        let buf = [0x00, 0x00, 0x02, 0x00, 0x00, 0x00]; // count=2, only 1.5 codes present
        assert!(matches!(
            parse_copy_both_response(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn copy_both_response_trailing_bytes_errors() {
        let buf = [0x00, 0x00, 0x00, 0xFF]; // valid empty-columns body + 1 stray byte
        assert!(matches!(
            parse_copy_both_response(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    // ── parse_xlog_data ───────────────────────────────────────────────────────

    #[test]
    fn xlog_data_valid_fixture() {
        let mut buf = vec![b'w'];
        buf.extend_from_slice(&0x1000_u64.to_be_bytes());
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        buf.extend_from_slice(&12345_i64.to_be_bytes());
        buf.extend_from_slice(b"hello");

        let result = parse_xlog_data(&buf).unwrap();
        assert_eq!(result.wal_start, Lsn::from_u64(0x1000));
        assert_eq!(result.wal_end, Lsn::from_u64(0x2000));
        assert_eq!(result.server_time, 12345);
        assert_eq!(result.data.as_ref(), b"hello");
    }

    #[test]
    fn xlog_data_wrong_tag() {
        let mut buf = vec![b'k']; // wrong tag; 'w' expected
        buf.extend_from_slice(&0_u64.to_be_bytes());
        buf.extend_from_slice(&0_u64.to_be_bytes());
        buf.extend_from_slice(&0_i64.to_be_bytes());
        assert!(matches!(parse_xlog_data(&buf), Err(PgError::Protocol(_))));
    }

    #[test]
    fn xlog_data_truncated_missing_tag() {
        let buf: [u8; 0] = [];
        assert!(matches!(parse_xlog_data(&buf), Err(PgError::Protocol(_))));
    }

    #[test]
    fn xlog_data_truncated_missing_wal_start() {
        let buf = [b'w'];
        assert!(matches!(parse_xlog_data(&buf), Err(PgError::Protocol(_))));
    }

    #[test]
    fn xlog_data_truncated_partial_wal_start() {
        let mut buf = vec![b'w'];
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // 4 of 8 bytes
        assert!(matches!(parse_xlog_data(&buf), Err(PgError::Protocol(_))));
    }

    #[test]
    fn xlog_data_truncated_missing_wal_end() {
        let mut buf = vec![b'w'];
        buf.extend_from_slice(&0x1000_u64.to_be_bytes());
        assert!(matches!(parse_xlog_data(&buf), Err(PgError::Protocol(_))));
    }

    #[test]
    fn xlog_data_truncated_partial_wal_end() {
        let mut buf = vec![b'w'];
        buf.extend_from_slice(&0x1000_u64.to_be_bytes());
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // 4 of 8 bytes
        assert!(matches!(parse_xlog_data(&buf), Err(PgError::Protocol(_))));
    }

    #[test]
    fn xlog_data_truncated_missing_server_time() {
        let mut buf = vec![b'w'];
        buf.extend_from_slice(&0x1000_u64.to_be_bytes());
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        assert!(matches!(parse_xlog_data(&buf), Err(PgError::Protocol(_))));
    }

    #[test]
    fn xlog_data_truncated_partial_server_time() {
        let mut buf = vec![b'w'];
        buf.extend_from_slice(&0x1000_u64.to_be_bytes());
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // 4 of 8 bytes
        assert!(matches!(parse_xlog_data(&buf), Err(PgError::Protocol(_))));
    }

    #[test]
    fn xlog_data_empty_data_payload_is_valid() {
        let mut buf = vec![b'w'];
        buf.extend_from_slice(&0x1000_u64.to_be_bytes());
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        buf.extend_from_slice(&12345_i64.to_be_bytes());
        // no trailing data bytes -- zero-length data is legal.

        let result = parse_xlog_data(&buf).unwrap();
        assert_eq!(result.wal_start, Lsn::from_u64(0x1000));
        assert_eq!(result.wal_end, Lsn::from_u64(0x2000));
        assert_eq!(result.server_time, 12345);
        assert_eq!(result.data.len(), 0);
    }

    // ── parse_primary_keepalive ───────────────────────────────────────────────

    #[test]
    fn primary_keepalive_valid_reply_true() {
        let mut buf = vec![b'k'];
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        buf.extend_from_slice(&54321_i64.to_be_bytes());
        buf.push(0x01);

        let result = parse_primary_keepalive(&buf).unwrap();
        assert_eq!(result.wal_end, Lsn::from_u64(0x2000));
        assert_eq!(result.server_time, 54321);
        assert!(result.reply_requested);
    }

    #[test]
    fn primary_keepalive_valid_reply_false() {
        let mut buf = vec![b'k'];
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        buf.extend_from_slice(&54321_i64.to_be_bytes());
        buf.push(0x00);

        let result = parse_primary_keepalive(&buf).unwrap();
        assert!(!result.reply_requested);
    }

    #[test]
    fn primary_keepalive_reply_nonzero_non_one_is_treated_as_true() {
        let mut buf = vec![b'k'];
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        buf.extend_from_slice(&54321_i64.to_be_bytes());
        buf.push(0x42); // some other nonzero byte

        let result = parse_primary_keepalive(&buf).unwrap();
        assert!(result.reply_requested);
    }

    #[test]
    fn primary_keepalive_wrong_tag() {
        let mut buf = vec![b'w']; // wrong tag; 'k' expected
        buf.extend_from_slice(&0_u64.to_be_bytes());
        buf.extend_from_slice(&0_i64.to_be_bytes());
        buf.push(0x00);
        assert!(matches!(
            parse_primary_keepalive(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn primary_keepalive_truncated_missing_tag() {
        let buf: [u8; 0] = [];
        assert!(matches!(
            parse_primary_keepalive(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn primary_keepalive_truncated_missing_wal_end() {
        let buf = [b'k'];
        assert!(matches!(
            parse_primary_keepalive(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn primary_keepalive_truncated_partial_wal_end() {
        let mut buf = vec![b'k'];
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // 4 of 8 bytes
        assert!(matches!(
            parse_primary_keepalive(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn primary_keepalive_truncated_missing_server_time() {
        let mut buf = vec![b'k'];
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        assert!(matches!(
            parse_primary_keepalive(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn primary_keepalive_truncated_partial_server_time() {
        let mut buf = vec![b'k'];
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // 4 of 8 bytes
        assert!(matches!(
            parse_primary_keepalive(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn primary_keepalive_truncated_missing_reply_requested() {
        let mut buf = vec![b'k'];
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        buf.extend_from_slice(&54321_i64.to_be_bytes());
        // missing final byte
        assert!(matches!(
            parse_primary_keepalive(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn primary_keepalive_trailing_bytes_errors() {
        let mut buf = vec![b'k'];
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        buf.extend_from_slice(&54321_i64.to_be_bytes());
        buf.push(0x01);
        buf.push(0xFF); // stray trailing byte
        assert!(matches!(
            parse_primary_keepalive(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    // ── parse_copy_both_message ──────────────────────────────────────────────

    #[test]
    fn copy_both_message_dispatches_xlog_data() {
        let mut buf = vec![b'w'];
        buf.extend_from_slice(&0x1000_u64.to_be_bytes());
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        buf.extend_from_slice(&12345_i64.to_be_bytes());
        buf.extend_from_slice(b"hi");

        let Ok(CopyBothMessage::XLogData(x)) = parse_copy_both_message(&buf) else {
            panic!("expected Ok(CopyBothMessage::XLogData(_))");
        };
        assert_eq!(x.wal_start, Lsn::from_u64(0x1000));
        assert_eq!(x.data.as_ref(), b"hi");
    }

    #[test]
    fn copy_both_message_dispatches_keepalive() {
        let mut buf = vec![b'k'];
        buf.extend_from_slice(&0x2000_u64.to_be_bytes());
        buf.extend_from_slice(&54321_i64.to_be_bytes());
        buf.push(0x01);

        let Ok(CopyBothMessage::KeepAlive(k)) = parse_copy_both_message(&buf) else {
            panic!("expected Ok(CopyBothMessage::KeepAlive(_))");
        };
        assert_eq!(k.wal_end, Lsn::from_u64(0x2000));
        assert!(k.reply_requested);
    }

    #[test]
    fn copy_both_message_unknown_tag_errors() {
        let buf = [b'X', 0x00, 0x00];
        assert!(matches!(
            parse_copy_both_message(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn copy_both_message_empty_payload_errors() {
        let buf: [u8; 0] = [];
        assert!(matches!(
            parse_copy_both_message(&buf),
            Err(PgError::Protocol(_))
        ));
    }

    // ── encode_standby_status_update ─────────────────────────────────────────

    #[test]
    fn encode_standby_status_update_golden_bytes() {
        let actual = encode_standby_status_update(
            Lsn::from_u64(0x1000),
            Lsn::from_u64(0x2000),
            Lsn::from_u64(0x3000),
            999,
            true,
        );

        // Hand-computed byte-by-byte, per the CopyData + Standby Status
        // Update wire layout:
        //   'd' | len:i32=38 | 'r' | written:u64 | flushed:u64 | applied:u64 | client_time:i64 | reply:u8
        let expected: &[u8] = &[
            b'd', // CopyData tag
            0x00, 0x00, 0x00, 0x26, // length = 38 (4 self + 34 body)
            b'r', // Standby Status Update sub-message tag
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, // written_lsn = 0x1000
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, // flushed_lsn = 0x2000
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x00, // applied_lsn = 0x3000
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xE7, // client_time = 999
            0x01, // reply_requested = true
        ];

        assert_eq!(actual.len(), 39);
        assert_eq!(actual.as_ref(), expected);
    }

    #[test]
    fn encode_standby_status_update_reply_not_requested() {
        let actual = encode_standby_status_update(
            Lsn::from_u64(0x1000),
            Lsn::from_u64(0x1000),
            Lsn::from_u64(0x1000),
            0,
            false,
        );
        assert_eq!(actual.len(), 39);
        assert_eq!(actual.last(), Some(&0x00));
        assert_eq!(&actual[0..5], &[b'd', 0x00, 0x00, 0x00, 0x26]);
    }

    #[test]
    fn encode_standby_status_update_body_round_trips_via_manual_reparse() {
        // The encoder output is a CLIENT -> server frame, while parse_* in
        // this module targets SERVER -> client payloads, so there is no
        // `parse_standby_status_update` to round-trip through. Instead,
        // manually strip the CopyData envelope and re-parse the 'r'
        // sub-message body by hand to confirm the encoder placed every
        // field correctly.
        let written = Lsn::from_u64(0x1234_5678);
        let flushed = Lsn::from_u64(0x1234_5680);
        let applied = Lsn::from_u64(0x1234_5690);
        let client_time = 1_700_000_000_123_456_i64;

        let framed = encode_standby_status_update(written, flushed, applied, client_time, false);

        // Strip the CopyData envelope by hand: Byte1('d') + Int32 length.
        assert_eq!(framed[0], b'd');
        let frame_len = i32::from_be_bytes([framed[1], framed[2], framed[3], framed[4]]);
        assert_eq!(frame_len, STANDBY_STATUS_UPDATE_FRAME_LEN);
        let body = &framed[5..];
        assert_eq!(body.len(), STANDBY_STATUS_UPDATE_BODY_LEN);

        let mut cursor = body;
        assert_eq!(cursor.get_u8(), b'r');
        assert_eq!(Lsn::from_u64(cursor.get_u64()), written);
        assert_eq!(Lsn::from_u64(cursor.get_u64()), flushed);
        assert_eq!(Lsn::from_u64(cursor.get_u64()), applied);
        assert_eq!(cursor.get_i64(), client_time);
        assert_eq!(cursor.get_u8(), 0x00); // reply_requested = false
        assert!(!cursor.has_remaining());
    }
}

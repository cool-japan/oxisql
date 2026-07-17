//! High-level PostgreSQL logical-replication control connection.
//!
//! This module ties together the transport-level connection setup in [`auth`]
//! and the transport-agnostic command builders/parsers in [`commands`] to
//! expose [`PgReplicationConnection`] — a thin, ergonomic wrapper over the
//! **non-streaming** parts of the PostgreSQL Streaming Replication Protocol:
//!
//! - [`PgReplicationConnection::connect`] establishes a replication-mode
//!   connection (TCP connect, optional TLS upgrade, a `replication=database`
//!   `StartupMessage`, and authentication), delegating to
//!   [`auth::connect_replication`].
//! - [`PgReplicationConnection::identify_system`] runs `IDENTIFY_SYSTEM`.
//! - [`PgReplicationConnection::create_replication_slot`] runs
//!   `CREATE_REPLICATION_SLOT ... LOGICAL pgoutput`.
//! - [`PgReplicationConnection::drop_replication_slot`] runs
//!   `DROP_REPLICATION_SLOT`.
//!
//! All three commands travel over the ordinary simple-query (`'Q'`) protocol: a
//! `replication=database` connection accepts them just like SQL statements, and
//! the server answers with the usual
//! `RowDescription`/`DataRow`/`CommandComplete`/`ReadyForQuery` sequence (or an
//! `ErrorResponse`). That shared request/response shape is factored into the
//! private [`execute_simple_query`] helper that every command builds on.
//!
//! # Streaming replication
//!
//! [`PgReplicationConnection::start_logical_replication`] issues
//! `START_REPLICATION` and, on success, switches the connection into
//! `CopyBoth` streaming mode, returning a [`ReplicationStream`] — implemented
//! in the sibling [`stream`] module, which decodes `XLogData`/keepalive
//! frames (via [`copyboth`]) and `pgoutput` messages (via [`pgoutput`] and
//! [`tuple`]) on a background task, while a second background task sends
//! periodic Standby Status Updates back. See [`stream`]'s module
//! documentation for the full background-task architecture and its own
//! "Testing status" section.
//!
//! # Testability
//!
//! [`execute_simple_query`] and the per-command `run_*` helpers are generic
//! over the stream type (`S: AsyncRead + AsyncWrite + Unpin`) rather than
//! hard-wired to [`auth::MaybeTlsStream`]. The public methods instantiate them
//! with the connection's real `MaybeTlsStream`; the unit tests instantiate them
//! with an in-memory `tokio::io::duplex` pipe driven by a scripted fake server,
//! exactly as [`auth`]'s own tests exercise its message-handling helpers. (A
//! `MaybeTlsStream` can only wrap a real `TcpStream`/`TlsStream`, never an
//! arbitrary in-memory stream, so this indirection is what makes the command
//! flows unit-testable without a live PostgreSQL server.)

#![forbid(unsafe_code)]

use bytes::BytesMut;
use fallible_iterator_02::FallibleIterator;
use postgres_protocol::message::backend::{DataRowBody, Message};
use postgres_protocol::message::frontend;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::builder::TlsMode;
use crate::error::PgError;

use auth::MaybeTlsStream;

mod auth;
mod commands;
mod copyboth;
mod lsn;
mod pgoutput;
mod stream;
mod tuple;

pub use commands::{CreatedSlot, IdentifySystem};
pub use lsn::{pg_micros_to_unix_micros, unix_micros_to_pg_micros, Lsn};
pub use pgoutput::{
    ColumnSpec, LogicalReplicationMessage, RelationBody, ReplicaIdentity, TupleColumn, TupleData,
};
pub use stream::{ReplicationEvent, ReplicationStream};
pub use tuple::{tuple_to_values, CellValue};

// ── PgReplicationConnection ───────────────────────────────────────────────────

/// An authenticated PostgreSQL connection in replication mode, ready to run the
/// non-streaming replication control commands.
///
/// Created with [`PgReplicationConnection::connect`], which performs the full
/// replication handshake (TCP connect, optional TLS upgrade, a
/// `replication=database` `StartupMessage`, and authentication) and leaves the
/// connection positioned right after the initial `ReadyForQuery`.
///
/// The connection then accepts [`identify_system`](Self::identify_system),
/// [`create_replication_slot`](Self::create_replication_slot), and
/// [`drop_replication_slot`](Self::drop_replication_slot). Each borrows the
/// connection mutably (`&mut self`) because a single wire carries both the
/// request and its response, so commands cannot overlap.
#[derive(Debug)]
pub struct PgReplicationConnection {
    /// The authenticated transport, positioned after `ReadyForQuery`.
    stream: MaybeTlsStream,
    /// Read buffer reused across commands; may retain bytes read past one
    /// message's boundary for the next [`execute_simple_query`] call.
    read_buf: BytesMut,
}

impl PgReplicationConnection {
    /// Establishes a replication-mode connection to PostgreSQL.
    ///
    /// `conn_str` accepts both the libpq `key=value` and `postgres://` URL forms
    /// (parsed exactly as `auth::connect_replication` documents); it must
    /// specify a non-empty user, and — for logical replication — a database
    /// name. `tls` selects a plain-text connection or a `rustls`-backed TLS
    /// upgrade.
    ///
    /// On success the returned connection is authenticated and sitting at
    /// `ReadyForQuery`, ready for the command methods below.
    ///
    /// # Errors
    ///
    /// Forwards every failure mode of `auth::connect_replication`:
    /// [`PgError::Replication`] for connection-string, authentication, or
    /// server-`ErrorResponse` problems; [`PgError::Tls`] for TLS negotiation
    /// failures; [`PgError::Protocol`] for malformed or unexpected server
    /// messages; and [`PgError::Connection`] for socket I/O errors.
    pub async fn connect(conn_str: &str, tls: TlsMode) -> Result<Self, PgError> {
        let stream = auth::connect_replication(conn_str, tls).await?;
        Ok(Self {
            stream,
            read_buf: BytesMut::new(),
        })
    }

    /// Runs `IDENTIFY_SYSTEM` and returns the server's identity row.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Replication`] if the server answers with an
    /// `ErrorResponse`, or [`PgError::Protocol`] if the response is malformed,
    /// carries no row, or cannot be parsed into an [`IdentifySystem`] (see
    /// `commands::parse_identify_system_row`).
    pub async fn identify_system(&mut self) -> Result<IdentifySystem, PgError> {
        run_identify_system(&mut self.stream, &mut self.read_buf).await
    }

    /// Runs `CREATE_REPLICATION_SLOT <slot_name> [TEMPORARY] LOGICAL pgoutput`
    /// and returns the created slot's details.
    ///
    /// When `temporary` is `true` the slot is dropped automatically at the end
    /// of the session; otherwise it persists until explicitly dropped. The slot
    /// always uses the `pgoutput` logical-decoding plugin.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Replication`] if `slot_name` is not a valid slot name
    /// (see `commands::build_create_replication_slot`) or if the server
    /// answers with an `ErrorResponse` (e.g. the slot already exists), or
    /// [`PgError::Protocol`] if the response is malformed, carries no row, or
    /// cannot be parsed into a [`CreatedSlot`].
    pub async fn create_replication_slot(
        &mut self,
        slot_name: &str,
        temporary: bool,
    ) -> Result<CreatedSlot, PgError> {
        run_create_replication_slot(&mut self.stream, &mut self.read_buf, slot_name, temporary)
            .await
    }

    /// Runs `DROP_REPLICATION_SLOT <slot_name>`.
    ///
    /// `DROP_REPLICATION_SLOT` returns no result rows, so a successful call
    /// yields `()`.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Replication`] if `slot_name` is not a valid slot name
    /// (see `commands::build_drop_replication_slot`) or if the server answers
    /// with an `ErrorResponse` (e.g. the slot does not exist), or
    /// [`PgError::Protocol`] if the response is malformed.
    pub async fn drop_replication_slot(&mut self, slot_name: &str) -> Result<(), PgError> {
        run_drop_replication_slot(&mut self.stream, &mut self.read_buf, slot_name).await
    }

    // `start_logical_replication` is implemented in the sibling `stream`
    // module (an additional `impl PgReplicationConnection` block there),
    // since it introduces a substantial amount of supporting machinery
    // (`ReplicationStream`, its background tasks) that would otherwise push
    // this file well past a comfortable size. See `stream.rs`.
}

// ── Command runners (generic over the stream, for testability) ────────────────

/// Runs `IDENTIFY_SYSTEM` over `stream` and parses the single result row.
async fn run_identify_system<S>(
    stream: &mut S,
    read_buf: &mut BytesMut,
) -> Result<IdentifySystem, PgError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let rows = execute_simple_query(stream, read_buf, &commands::build_identify_system()).await?;
    let fields = first_row_fields(&rows, "IDENTIFY_SYSTEM")?;
    commands::parse_identify_system_row(&fields)
}

/// Runs `CREATE_REPLICATION_SLOT` over `stream` and parses the single result
/// row.
async fn run_create_replication_slot<S>(
    stream: &mut S,
    read_buf: &mut BytesMut,
    slot_name: &str,
    temporary: bool,
) -> Result<CreatedSlot, PgError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let command = commands::build_create_replication_slot(slot_name, temporary)?;
    let rows = execute_simple_query(stream, read_buf, &command).await?;
    let fields = first_row_fields(&rows, "CREATE_REPLICATION_SLOT")?;
    commands::parse_create_replication_slot_row(&fields)
}

/// Runs `DROP_REPLICATION_SLOT` over `stream`.
///
/// `DROP_REPLICATION_SLOT` produces no result rows, so any rows are ignored and
/// success maps to `()`.
async fn run_drop_replication_slot<S>(
    stream: &mut S,
    read_buf: &mut BytesMut,
    slot_name: &str,
) -> Result<(), PgError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let command = commands::build_drop_replication_slot(slot_name)?;
    execute_simple_query(stream, read_buf, &command).await?;
    Ok(())
}

/// Borrows the first row of `rows` as a `Vec<Option<&str>>` suitable for the
/// positional parsers in [`commands`], or returns [`PgError::Protocol`] naming
/// `command` when the server returned no rows.
fn first_row_fields<'a>(
    rows: &'a [Vec<Option<String>>],
    command: &str,
) -> Result<Vec<Option<&'a str>>, PgError> {
    let row = rows.first().ok_or_else(|| {
        PgError::Protocol(format!("{command} returned no rows, expected exactly one"))
    })?;
    Ok(row.iter().map(Option::as_deref).collect())
}

// ── Simple-query execution ────────────────────────────────────────────────────

/// Encodes `query_text` as a PostgreSQL simple-query (`'Q'`) message and
/// writes it to `stream`.
///
/// Factored out of [`execute_simple_query`] so
/// [`PgReplicationConnection::start_logical_replication`] can send
/// `START_REPLICATION` the same way, without going through
/// `execute_simple_query` itself — that function's response-reading loop
/// assumes an ordinary `RowDescription`/`DataRow`/`ReadyForQuery` shape,
/// which `START_REPLICATION`'s `CopyBothResponse` reply does not have (see
/// `stream.rs`).
async fn send_query_message<S>(stream: &mut S, query_text: &str) -> Result<(), PgError>
where
    S: AsyncWrite + Unpin,
{
    let mut write_buf = BytesMut::new();
    frontend::query(query_text, &mut write_buf).map_err(|e| {
        PgError::Protocol(format!("failed to encode simple-query ('Q') message: {e}"))
    })?;
    stream.write_all(&write_buf).await?;
    Ok(())
}

/// Sends `query_text` as a PostgreSQL simple-query (`'Q'`) message and collects
/// every `DataRow`'s field values — as owned, UTF-8-decoded, nullable text
/// strings — until `ReadyForQuery`, returning one `Vec<Option<String>>` per row.
///
/// Replication commands always run in text format, so each present field is
/// decoded as a UTF-8 string (and SQL `NULL` maps to `None`).
/// `RowDescription`, `CommandComplete`, `NoticeResponse`, and
/// `EmptyQueryResponse` messages are accepted and ignored; an `ErrorResponse` is
/// converted (via [`auth::server_error_response`]) and returned immediately.
///
/// # Errors
///
/// Returns [`PgError::Replication`] for a server `ErrorResponse`;
/// [`PgError::Protocol`] for an invalid-UTF-8 field, a malformed or undecodable
/// message, or any message type not valid mid–simple-query; and
/// [`PgError::Connection`] if the connection closes before `ReadyForQuery`.
async fn execute_simple_query<S>(
    stream: &mut S,
    read_buf: &mut BytesMut,
    query_text: &str,
) -> Result<Vec<Vec<Option<String>>>, PgError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    send_query_message(stream, query_text).await?;

    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    loop {
        match auth::read_message(stream, read_buf).await? {
            Message::ReadyForQuery(_) => return Ok(rows),
            Message::DataRow(body) => rows.push(decode_data_row(&body)?),
            Message::RowDescription(_)
            | Message::CommandComplete(_)
            | Message::NoticeResponse(_)
            | Message::EmptyQueryResponse => {}
            Message::ErrorResponse(body) => return Err(auth::server_error_response(&body)),
            other => {
                return Err(PgError::Protocol(format!(
                    "unexpected {} message during simple query",
                    backend_message_name(&other)
                )));
            }
        }
    }
}

/// Decodes one `DataRow` body into owned, nullable, UTF-8 text field values,
/// preserving field order.
///
/// A negative field length in the wire format denotes SQL `NULL` and maps to
/// `None`; every present field is decoded as UTF-8.
///
/// # Errors
///
/// Returns [`PgError::Protocol`] if the row is structurally malformed (a decoder
/// bounds-check failure) or if any present field is not valid UTF-8.
fn decode_data_row(body: &DataRowBody) -> Result<Vec<Option<String>>, PgError> {
    let buffer = body.buffer();
    let mut ranges = body.ranges();
    let mut fields: Vec<Option<String>> = Vec::new();
    loop {
        match ranges.next() {
            Ok(Some(Some(range))) => {
                let raw = &buffer[range];
                let text = std::str::from_utf8(raw).map_err(|e| {
                    PgError::Protocol(format!(
                        "simple-query DataRow field {} is not valid UTF-8: {e}",
                        fields.len()
                    ))
                })?;
                fields.push(Some(text.to_string()));
            }
            Ok(Some(None)) => fields.push(None),
            Ok(None) => return Ok(fields),
            Err(e) => {
                return Err(PgError::Protocol(format!(
                    "malformed simple-query DataRow: {e}"
                )));
            }
        }
    }
}

/// Returns a short, human-readable name for a backend message variant, used to
/// describe an unexpected message that arrives mid–simple-query.
///
/// The trailing wildcard is required because
/// [`postgres_protocol::message::backend::Message`] is `#[non_exhaustive]`.
fn backend_message_name(message: &Message) -> &'static str {
    match message {
        Message::AuthenticationCleartextPassword => "AuthenticationCleartextPassword",
        Message::AuthenticationGss => "AuthenticationGSS",
        Message::AuthenticationKerberosV5 => "AuthenticationKerberosV5",
        Message::AuthenticationMd5Password(_) => "AuthenticationMD5Password",
        Message::AuthenticationOk => "AuthenticationOk",
        Message::AuthenticationScmCredential => "AuthenticationSCMCredential",
        Message::AuthenticationSspi => "AuthenticationSSPI",
        Message::AuthenticationGssContinue(_) => "AuthenticationGSSContinue",
        Message::AuthenticationSasl(_) => "AuthenticationSASL",
        Message::AuthenticationSaslContinue(_) => "AuthenticationSASLContinue",
        Message::AuthenticationSaslFinal(_) => "AuthenticationSASLFinal",
        Message::BackendKeyData(_) => "BackendKeyData",
        Message::BindComplete => "BindComplete",
        Message::CloseComplete => "CloseComplete",
        Message::CommandComplete(_) => "CommandComplete",
        Message::CopyData(_) => "CopyData",
        Message::CopyDone => "CopyDone",
        Message::CopyInResponse(_) => "CopyInResponse",
        Message::CopyOutResponse(_) => "CopyOutResponse",
        Message::DataRow(_) => "DataRow",
        Message::EmptyQueryResponse => "EmptyQueryResponse",
        Message::ErrorResponse(_) => "ErrorResponse",
        Message::NoData => "NoData",
        Message::NoticeResponse(_) => "NoticeResponse",
        Message::NotificationResponse(_) => "NotificationResponse",
        Message::ParameterDescription(_) => "ParameterDescription",
        Message::ParameterStatus(_) => "ParameterStatus",
        Message::ParseComplete => "ParseComplete",
        Message::PortalSuspended => "PortalSuspended",
        Message::ReadyForQuery(_) => "ReadyForQuery",
        Message::RowDescription(_) => "RowDescription",
        _ => "unrecognized backend",
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use bytes::{BufMut, BytesMut};
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

    use super::*;

    // ── Wire-format helpers ─────────────────────────────────────────────────

    /// Encodes one length-prefixed backend message: `tag`, a big-endian `u32`
    /// length (covering itself and `body`, per the PostgreSQL wire format), then
    /// `body`. Mirrors `auth.rs`'s test helper of the same name.
    fn encode_message(tag: u8, body: &[u8]) -> BytesMut {
        let mut buf = BytesMut::new();
        buf.put_u8(tag);
        let len = u32::try_from(body.len() + 4).expect("test message body fits in u32");
        buf.put_u32(len);
        buf.put_slice(body);
        buf
    }

    /// Encodes a `ReadyForQuery` (`'Z'`) message with transaction status `I`
    /// (idle).
    fn encode_ready_for_query() -> BytesMut {
        encode_message(b'Z', b"I")
    }

    /// Encodes a `CommandComplete` (`'C'`) message carrying the NUL-terminated
    /// command tag.
    fn encode_command_complete(tag: &str) -> BytesMut {
        let mut body = BytesMut::new();
        body.put_slice(tag.as_bytes());
        body.put_u8(0);
        encode_message(b'C', &body)
    }

    /// Encodes a `RowDescription` (`'T'`) message declaring `field_names`, each
    /// a text-format `TEXT` column. The contents are irrelevant to the code
    /// under test (which ignores `RowDescription`); this exists to make scripts
    /// realistic and to exercise the "ignore RowDescription" path.
    fn encode_row_description(field_names: &[&str]) -> BytesMut {
        let mut body = BytesMut::new();
        let count = u16::try_from(field_names.len()).expect("field count fits in u16");
        body.put_u16(count);
        for name in field_names {
            body.put_slice(name.as_bytes());
            body.put_u8(0); // name cstr terminator
            body.put_u32(0); // table_oid
            body.put_i16(0); // column_id
            body.put_u32(25); // type_oid: TEXT
            body.put_i16(-1); // type_size
            body.put_i32(-1); // type_modifier
            body.put_i16(0); // format code: text
        }
        encode_message(b'T', &body)
    }

    /// Encodes a `DataRow` (`'D'`) message. Each field is either `Some(bytes)`
    /// (a present field of the given raw bytes) or `None` (SQL `NULL`, encoded
    /// as the length `-1`). Raw bytes are used — rather than `&str` — so tests
    /// can hand-build fields that are not valid UTF-8.
    fn encode_data_row(fields: &[Option<&[u8]>]) -> BytesMut {
        let mut body = BytesMut::new();
        let count = u16::try_from(fields.len()).expect("field count fits in u16");
        body.put_u16(count);
        for field in fields {
            match field {
                Some(bytes) => {
                    let len = i32::try_from(bytes.len()).expect("field length fits in i32");
                    body.put_i32(len);
                    body.put_slice(bytes);
                }
                None => body.put_i32(-1),
            }
        }
        encode_message(b'D', &body)
    }

    /// Encodes an `ErrorResponse`/`NoticeResponse` field list: each
    /// `(type_byte, value)` pair as `type_byte` then a NUL-terminated `value`,
    /// then the mandatory terminating `0x00` byte. Returns just the body, to be
    /// wrapped with [`encode_message`]. Mirrors `auth.rs`'s helper.
    fn encode_error_fields(fields: &[(u8, &str)]) -> BytesMut {
        let mut body = BytesMut::new();
        for (type_, value) in fields {
            body.put_u8(*type_);
            body.put_slice(value.as_bytes());
            body.put_u8(0);
        }
        body.put_u8(0); // terminator
        body
    }

    /// Reads one length-prefixed *frontend* message (tag + big-endian `u32`
    /// length + body) directly. A test harness playing "the server" uses this to
    /// read what the client under test sends. Mirrors `auth.rs`'s helper.
    async fn read_raw_frontend_message<S: AsyncRead + Unpin>(stream: &mut S) -> (u8, Vec<u8>) {
        let mut header = [0_u8; 5];
        stream
            .read_exact(&mut header)
            .await
            .expect("read frontend message header");
        let tag = header[0];
        let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let mut body = vec![0_u8; len - 4];
        stream
            .read_exact(&mut body)
            .await
            .expect("read frontend message body");
        (tag, body)
    }

    // ── execute_simple_query ────────────────────────────────────────────────

    #[tokio::test]
    async fn execute_simple_query_returns_single_row_and_sends_query() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let (tag, body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(tag, b'Q', "client should send a simple-query message");
            assert_eq!(&body[..body.len() - 1], b"SELECT 1");
            assert_eq!(
                body.last(),
                Some(&0_u8),
                "query text must be NUL-terminated"
            );

            server
                .write_all(&encode_row_description(&["col"]))
                .await
                .expect("write RowDescription");
            server
                .write_all(&encode_data_row(&[Some(b"hello")]))
                .await
                .expect("write DataRow");
            server
                .write_all(&encode_command_complete("SELECT 1"))
                .await
                .expect("write CommandComplete");
            server
                .write_all(&encode_ready_for_query())
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let rows = execute_simple_query(&mut client, &mut buf, "SELECT 1")
            .await
            .expect("query failed");
        assert_eq!(rows, vec![vec![Some("hello".to_string())]]);
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn execute_simple_query_returns_multiple_rows() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let (tag, _body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(tag, b'Q');
            server
                .write_all(&encode_row_description(&["n"]))
                .await
                .expect("write RowDescription");
            for value in [b"1".as_slice(), b"2", b"3"] {
                server
                    .write_all(&encode_data_row(&[Some(value)]))
                    .await
                    .expect("write DataRow");
            }
            server
                .write_all(&encode_command_complete("SELECT 3"))
                .await
                .expect("write CommandComplete");
            server
                .write_all(&encode_ready_for_query())
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let rows = execute_simple_query(&mut client, &mut buf, "SELECT n")
            .await
            .expect("query failed");
        assert_eq!(
            rows,
            vec![
                vec![Some("1".to_string())],
                vec![Some("2".to_string())],
                vec![Some("3".to_string())],
            ]
        );
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn execute_simple_query_returns_zero_rows() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let _request = read_raw_frontend_message(&mut server).await;
            // A command that yields no rows: CommandComplete then ReadyForQuery,
            // with no DataRow in between.
            server
                .write_all(&encode_command_complete("DROP_REPLICATION_SLOT"))
                .await
                .expect("write CommandComplete");
            server
                .write_all(&encode_ready_for_query())
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let rows = execute_simple_query(&mut client, &mut buf, "DROP_REPLICATION_SLOT \"s\"")
            .await
            .expect("query failed");
        assert!(rows.is_empty());
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn execute_simple_query_decodes_null_field() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let _request = read_raw_frontend_message(&mut server).await;
            server
                .write_all(&encode_row_description(&["a", "b"]))
                .await
                .expect("write RowDescription");
            server
                .write_all(&encode_data_row(&[Some(b"x"), None]))
                .await
                .expect("write DataRow");
            server
                .write_all(&encode_command_complete("SELECT 1"))
                .await
                .expect("write CommandComplete");
            server
                .write_all(&encode_ready_for_query())
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let rows = execute_simple_query(&mut client, &mut buf, "SELECT a, b")
            .await
            .expect("query failed");
        assert_eq!(rows, vec![vec![Some("x".to_string()), None]]);
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn execute_simple_query_surfaces_error_response() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let _request = read_raw_frontend_message(&mut server).await;
            let body = encode_error_fields(&[
                (b'S', "ERROR"),
                (b'C', "42704"),
                (b'M', "replication slot \"nope\" does not exist"),
            ]);
            server
                .write_all(&encode_message(b'E', &body))
                .await
                .expect("write ErrorResponse");
        });

        let mut buf = BytesMut::new();
        let result =
            execute_simple_query(&mut client, &mut buf, "DROP_REPLICATION_SLOT \"nope\"").await;
        let Err(PgError::Replication(text)) = result else {
            panic!("expected Err(PgError::Replication(_)), got {result:?}");
        };
        assert!(text.contains("42704"), "should surface SQLSTATE: {text}");
        assert!(
            text.contains("does not exist"),
            "should surface message: {text}"
        );
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn execute_simple_query_rejects_invalid_utf8_field() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let _request = read_raw_frontend_message(&mut server).await;
            server
                .write_all(&encode_row_description(&["x"]))
                .await
                .expect("write RowDescription");
            // 0xFF is never a valid UTF-8 byte.
            let invalid: &[u8] = &[0xFF, 0xFE];
            server
                .write_all(&encode_data_row(&[Some(invalid)]))
                .await
                .expect("write DataRow");
            server
                .write_all(&encode_command_complete("SELECT 1"))
                .await
                .expect("write CommandComplete");
            server
                .write_all(&encode_ready_for_query())
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let result = execute_simple_query(&mut client, &mut buf, "SELECT x").await;
        assert!(
            matches!(result, Err(PgError::Protocol(_))),
            "expected a protocol error, got {result:?}"
        );
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn execute_simple_query_rejects_unexpected_message() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let _request = read_raw_frontend_message(&mut server).await;
            // BindComplete ('2') never appears in a simple-query response.
            server
                .write_all(&encode_message(b'2', &[]))
                .await
                .expect("write BindComplete");
        });

        let mut buf = BytesMut::new();
        let result = execute_simple_query(&mut client, &mut buf, "SELECT 1").await;
        let Err(PgError::Protocol(text)) = result else {
            panic!("expected Err(PgError::Protocol(_)), got {result:?}");
        };
        assert!(
            text.contains("BindComplete"),
            "error should name the unexpected message: {text}"
        );
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn execute_simple_query_ignores_notice_response() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let _request = read_raw_frontend_message(&mut server).await;
            // A NoticeResponse before the row must be skipped, not surfaced.
            server
                .write_all(&encode_message(
                    b'N',
                    &encode_error_fields(&[(b'S', "NOTICE"), (b'M', "advisory")]),
                ))
                .await
                .expect("write NoticeResponse");
            server
                .write_all(&encode_row_description(&["x"]))
                .await
                .expect("write RowDescription");
            server
                .write_all(&encode_data_row(&[Some(b"ok")]))
                .await
                .expect("write DataRow");
            server
                .write_all(&encode_command_complete("SELECT 1"))
                .await
                .expect("write CommandComplete");
            server
                .write_all(&encode_ready_for_query())
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let rows = execute_simple_query(&mut client, &mut buf, "SELECT x")
            .await
            .expect("query failed");
        assert_eq!(rows, vec![vec![Some("ok".to_string())]]);
        server_task.await.expect("server task panicked");
    }

    // ── run_identify_system ─────────────────────────────────────────────────

    #[tokio::test]
    async fn run_identify_system_parses_identity_row() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let (tag, body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(tag, b'Q');
            assert_eq!(&body[..body.len() - 1], b"IDENTIFY_SYSTEM");

            server
                .write_all(&encode_row_description(&[
                    "systemid", "timeline", "xlogpos", "dbname",
                ]))
                .await
                .expect("write RowDescription");
            server
                .write_all(&encode_data_row(&[
                    Some(b"6821810470617038336"),
                    Some(b"1"),
                    Some(b"16/B374D848"),
                    Some(b"postgres"),
                ]))
                .await
                .expect("write DataRow");
            server
                .write_all(&encode_command_complete("IDENTIFY_SYSTEM"))
                .await
                .expect("write CommandComplete");
            server
                .write_all(&encode_ready_for_query())
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let identity = run_identify_system(&mut client, &mut buf)
            .await
            .expect("IDENTIFY_SYSTEM failed");
        assert_eq!(identity.systemid, "6821810470617038336");
        assert_eq!(identity.timeline, 1);
        assert_eq!(
            identity.xlogpos,
            "16/B374D848".parse::<Lsn>().expect("valid LSN")
        );
        assert_eq!(identity.dbname, Some("postgres".to_string()));
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn run_identify_system_errors_when_no_rows() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let _request = read_raw_frontend_message(&mut server).await;
            // No DataRow at all before ReadyForQuery.
            server
                .write_all(&encode_command_complete("IDENTIFY_SYSTEM"))
                .await
                .expect("write CommandComplete");
            server
                .write_all(&encode_ready_for_query())
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let result = run_identify_system(&mut client, &mut buf).await;
        assert!(
            matches!(result, Err(PgError::Protocol(_))),
            "expected a protocol error, got {result:?}"
        );
        server_task.await.expect("server task panicked");
    }

    // ── run_create_replication_slot ─────────────────────────────────────────

    #[tokio::test]
    async fn run_create_replication_slot_temporary_builds_command_and_parses_row() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let (tag, body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(tag, b'Q');
            assert_eq!(
                &body[..body.len() - 1],
                b"CREATE_REPLICATION_SLOT \"myslot\" TEMPORARY LOGICAL pgoutput"
            );

            server
                .write_all(&encode_row_description(&[
                    "slot_name",
                    "consistent_point",
                    "snapshot_name",
                    "output_plugin",
                ]))
                .await
                .expect("write RowDescription");
            server
                .write_all(&encode_data_row(&[
                    Some(b"myslot"),
                    Some(b"0/1600060"),
                    None,
                    Some(b"pgoutput"),
                ]))
                .await
                .expect("write DataRow");
            server
                .write_all(&encode_command_complete("CREATE_REPLICATION_SLOT"))
                .await
                .expect("write CommandComplete");
            server
                .write_all(&encode_ready_for_query())
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let slot = run_create_replication_slot(&mut client, &mut buf, "myslot", true)
            .await
            .expect("CREATE_REPLICATION_SLOT failed");
        assert_eq!(slot.slot_name, "myslot");
        assert_eq!(
            slot.consistent_point,
            "0/1600060".parse::<Lsn>().expect("valid LSN")
        );
        assert_eq!(slot.snapshot_name, None);
        assert_eq!(slot.output_plugin, Some("pgoutput".to_string()));
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn run_create_replication_slot_permanent_builds_command() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let (tag, body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(tag, b'Q');
            assert_eq!(
                &body[..body.len() - 1],
                b"CREATE_REPLICATION_SLOT \"myslot\" LOGICAL pgoutput"
            );

            server
                .write_all(&encode_row_description(&[
                    "slot_name",
                    "consistent_point",
                    "snapshot_name",
                    "output_plugin",
                ]))
                .await
                .expect("write RowDescription");
            server
                .write_all(&encode_data_row(&[
                    Some(b"myslot"),
                    Some(b"0/1600060"),
                    Some(b"0000000A-0000000B-1"),
                    Some(b"pgoutput"),
                ]))
                .await
                .expect("write DataRow");
            server
                .write_all(&encode_command_complete("CREATE_REPLICATION_SLOT"))
                .await
                .expect("write CommandComplete");
            server
                .write_all(&encode_ready_for_query())
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let slot = run_create_replication_slot(&mut client, &mut buf, "myslot", false)
            .await
            .expect("CREATE_REPLICATION_SLOT failed");
        assert_eq!(slot.slot_name, "myslot");
        assert_eq!(slot.snapshot_name, Some("0000000A-0000000B-1".to_string()));
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn run_create_replication_slot_surfaces_already_exists_error() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let _request = read_raw_frontend_message(&mut server).await;
            let body = encode_error_fields(&[
                (b'S', "ERROR"),
                (b'C', "42710"),
                (b'M', "replication slot \"myslot\" already exists"),
            ]);
            server
                .write_all(&encode_message(b'E', &body))
                .await
                .expect("write ErrorResponse");
        });

        let mut buf = BytesMut::new();
        let result = run_create_replication_slot(&mut client, &mut buf, "myslot", false).await;
        let Err(PgError::Replication(text)) = result else {
            panic!("expected Err(PgError::Replication(_)), got {result:?}");
        };
        assert!(text.contains("42710"), "should surface SQLSTATE: {text}");
        assert!(
            text.contains("already exists"),
            "should surface message: {text}"
        );
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn run_create_replication_slot_rejects_invalid_name_before_io() {
        // An invalid slot name is rejected during command construction, before
        // any bytes are written; the server side is never touched.
        let (mut client, _server) = duplex(4096);
        let mut buf = BytesMut::new();
        let result =
            run_create_replication_slot(&mut client, &mut buf, "Invalid-Name", false).await;
        assert!(
            matches!(result, Err(PgError::Replication(_))),
            "expected a replication error, got {result:?}"
        );
    }

    // ── run_drop_replication_slot ───────────────────────────────────────────

    #[tokio::test]
    async fn run_drop_replication_slot_happy_path() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let (tag, body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(tag, b'Q');
            assert_eq!(&body[..body.len() - 1], b"DROP_REPLICATION_SLOT \"myslot\"");

            server
                .write_all(&encode_command_complete("DROP_REPLICATION_SLOT"))
                .await
                .expect("write CommandComplete");
            server
                .write_all(&encode_ready_for_query())
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let result = run_drop_replication_slot(&mut client, &mut buf, "myslot").await;
        assert!(result.is_ok(), "DROP_REPLICATION_SLOT failed: {result:?}");
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn run_drop_replication_slot_surfaces_error() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let _request = read_raw_frontend_message(&mut server).await;
            let body = encode_error_fields(&[
                (b'S', "ERROR"),
                (b'C', "42704"),
                (b'M', "replication slot \"nope\" does not exist"),
            ]);
            server
                .write_all(&encode_message(b'E', &body))
                .await
                .expect("write ErrorResponse");
        });

        let mut buf = BytesMut::new();
        let result = run_drop_replication_slot(&mut client, &mut buf, "nope").await;
        assert!(
            matches!(result, Err(PgError::Replication(_))),
            "expected a replication error, got {result:?}"
        );
        server_task.await.expect("server task panicked");
    }
}

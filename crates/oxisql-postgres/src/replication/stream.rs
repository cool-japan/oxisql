//! `CopyBoth` logical-replication streaming: [`PgReplicationConnection::start_logical_replication`],
//! [`ReplicationStream`], and [`ReplicationEvent`].
//!
//! # Architecture
//!
//! Once `START_REPLICATION` succeeds, the connection is in `CopyBoth` mode:
//! the server pushes `XLogData`/keepalive frames continuously, and the
//! client must periodically reply with Standby Status Updates. Driving that
//! from a single `&mut self`-style API (like [`super::execute_simple_query`]
//! uses for the non-streaming commands) does not work here, since decoding
//! and acknowledging need to happen concurrently with whatever the consumer
//! is doing with each event.
//!
//! Instead, this module follows the same shape as [`crate::notify`]'s
//! `spawn_connection_driver`: a background task owns the actual connection
//! (here, split into a read half and a write half) and does **all** the
//! async I/O and decoding, forwarding results through a
//! `tokio::sync::mpsc::Receiver<Result<ReplicationEvent, PgError>>` that
//! [`ReplicationStream`] wraps. [`futures::Stream::poll_next`] then just
//! delegates to `Receiver::poll_recv`, sidestepping a hand-written
//! `AsyncRead`-based poll state machine. Compared to `notify.rs`'s single
//! driver task, this is scaled up in two ways:
//!
//! - A **second** background task ([`run_keepalive_task`]) proactively
//!   sends a Standby Status Update every [`KEEPALIVE_INTERVAL`], so the
//!   server does not time the connection out during quiet periods with no
//!   WAL traffic.
//! - The reader task itself sometimes needs to *write* back to the
//!   connection (replying to a server keepalive that requested one), so
//!   the write half is shared (`Arc<tokio::sync::Mutex<_>>`) between both
//!   tasks rather than owned outright by one of them.
//!
//! The channel is bounded (capacity [`REPLICATION_EVENT_CHANNEL_CAPACITY`])
//! so a consumer that falls behind applies backpressure to the reader task
//! (which simply blocks on `Sender::send`) rather than letting buffered
//! events grow without bound.
//!
//! # `std::sync::Mutex` discipline
//!
//! [`ReplicationStream::confirmed`] and [`ReplicationStream::relations`] use
//! a plain `std::sync::Mutex`, not the async `tokio::sync::Mutex` --
//! matching the exact precedent and rationale documented on
//! `PgConnection`'s `stmt_cache` field in `connection.rs`: every critical
//! section here is a quick, synchronous operation (copy three [`Lsn`]s out
//! of a tuple, or read/write one `HashMap` entry) that is **never held
//! across an `.await` point**. [`ReplicationStream::write_half`] uses the
//! async `tokio::sync::Mutex` instead, since sending a Standby Status Update
//! genuinely does an `.await`ing socket write while holding it.
//!
//! # Testing status
//!
//! [`try_parse_start_replication_response`] (the `CopyBothResponse`/
//! `ErrorResponse` dispatch for the `START_REPLICATION` reply) is pure and
//! unit-tested directly against hand-built byte fixtures, no I/O involved.
//!
//! [`run_reader_task`] and [`run_keepalive_task`] -- where essentially all
//! of this module's interesting behavior lives -- are generic over the
//! read/write half types (`R: AsyncRead`, `W: AsyncWrite`) specifically so
//! they can be exercised directly against an in-memory `tokio::io::duplex`
//! pipe driven by a scripted fake server, exactly as [`auth`]'s and
//! [`super`]'s own tests do for their generic command helpers. This is what
//! makes it possible to test the actual concurrent-task logic (event
//! decoding, schema-cache population, replying to keepalives, the periodic
//! keepalive loop, clean-vs-error termination) without a live server.
//!
//! [`PgReplicationConnection::start_logical_replication`] itself is
//! *concrete* over [`auth::MaybeTlsStream`], which -- as
//! [`super`]'s module documentation explains -- can only wrap a real
//! `TcpStream`/`TlsStream`, never an arbitrary in-memory stream. So the one
//! piece this module's generic tests cannot reach is the thin, concrete
//! wrapper itself (building the `START_REPLICATION` command, sending it,
//! splitting the stream, spawning the two tasks). For that, this module's
//! tests use a real loopback TCP socket pair (`TcpListener` bound to
//! `127.0.0.1:0`) instead of `tokio::io::duplex`, which -- unlike a duplex
//! pipe -- can be wrapped in a real `MaybeTlsStream::Plain`. This covers the
//! full public method end to end (`START_REPLICATION` handshake, `CopyBoth`
//! entry, a few [`ReplicationEvent`]s, and an [`ReplicationStream::ack`]
//! call), all over a hermetic local socket with no real PostgreSQL server or
//! external network involved.
//!
//! What is **not** covered by any automated test in this pass: everything
//! that already wasn't covered upstream of this module (the actual
//! `TcpStream`/`TlsStream` I/O in `auth::connect_replication`, and the
//! SCRAM-SHA-256 happy path -- see `auth.rs`'s own "Testing status"
//! section), plus, specific to this module, real PostgreSQL server
//! behavior that a hand-scripted fake server cannot fully stand in for
//! (e.g. real WAL contents, real slot/publication semantics, TLS-mode
//! replication streaming, and the streaming/binary/messages protocol
//! options this MVP never negotiates -- see
//! [`super::commands::StartReplicationOptions`]). Both are left for
//! live-server integration testing in a later wave.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::BytesMut;
use postgres_protocol::message::backend::{Header, Message};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, WriteHalf};
use tokio::sync::{mpsc, Mutex as TokioMutex};
use tokio::task::JoinHandle;

use crate::error::PgError;

use super::Lsn;
use super::PgReplicationConnection;
use super::{auth, commands, copyboth, lsn, pgoutput, tuple};
use auth::MaybeTlsStream;

// ── Tunables ───────────────────────────────────────────────────────────────────

/// Capacity of the bounded channel [`ReplicationStream`] reads from.
///
/// A consumer that falls behind applies backpressure to the reader task
/// (which blocks on `Sender::send`) once this many undelivered events have
/// accumulated, rather than letting memory use grow without bound.
const REPLICATION_EVENT_CHANNEL_CAPACITY: usize = 256;

/// How often [`run_keepalive_task`] proactively sends a Standby Status
/// Update.
///
/// PostgreSQL's own `wal_sender_timeout` defaults to 60 seconds; sending
/// well inside that (every ~10 seconds, matching common client
/// implementations such as `pg_recvlogical`'s default `-s`/`--status-interval`)
/// leaves comfortable margin.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// Minimum bytes needed to hold a backend message header: a 1-byte tag plus
/// a 4-byte big-endian length (which counts itself but not the tag byte).
const MESSAGE_HEADER_LEN: usize = 5;

// ── ReplicationEvent ─────────────────────────────────────────────────────────

/// One event produced by a [`ReplicationStream`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicationEvent {
    /// A decoded `pgoutput` logical-replication message, together with the
    /// WAL position range of the `XLogData` frame that carried it.
    Logical {
        /// The starting WAL position of the `XLogData` frame.
        wal_start: Lsn,
        /// The server's current end-of-WAL position at the time this frame
        /// was sent (not necessarily the end of this specific message).
        wal_end: Lsn,
        /// The decoded message.
        message: pgoutput::LogicalReplicationMessage,
    },
    /// A server liveness probe (Primary Keepalive).
    KeepAlive {
        /// The server's current end-of-WAL position.
        wal_end: Lsn,
        /// Whether the server explicitly requested an immediate Standby
        /// Status Update reply. When `true`, [`ReplicationStream`]'s
        /// background reader task has already sent one automatically by
        /// the time this event is observed on the stream -- this field is
        /// informational only.
        reply_requested: bool,
    },
}

// ── ReplicationStream ─────────────────────────────────────────────────────────

/// A live PostgreSQL logical-replication `CopyBoth` stream.
///
/// Produced by [`PgReplicationConnection::start_logical_replication`]. See
/// the module documentation for the background-task architecture.
///
/// # Acknowledging progress
///
/// Call [`ack`](Self::ack) (or the more general
/// [`standby_status_update`](Self::standby_status_update)) after durably
/// handling everything up to and including a given LSN (typically: after a
/// [`ReplicationEvent::Logical`] carrying
/// [`LogicalReplicationMessage::Commit`](pgoutput::LogicalReplicationMessage::Commit)
/// has been fully applied). Until acknowledged, a server restart or
/// connection loss will replay WAL starting from the last acknowledged
/// position -- acknowledging progress past data that was not durably
/// persisted can lose it.
///
/// # Dropping
///
/// Dropping a `ReplicationStream` aborts its background tasks (see the
/// [`Drop`](#impl-Drop-for-ReplicationStream) impl below) but does not send
/// any explicit termination message to the server; the server will notice
/// the closed socket on its next write attempt.
pub struct ReplicationStream {
    rx: mpsc::Receiver<Result<ReplicationEvent, PgError>>,
    write_half: Arc<TokioMutex<WriteHalf<MaybeTlsStream>>>,
    /// `(written, flushed, applied)` LSNs most recently acknowledged via
    /// [`ack`](Self::ack)/[`standby_status_update`](Self::standby_status_update),
    /// or -- before the first acknowledgment -- the `start_lsn` streaming
    /// began at. Read by [`run_keepalive_task`] for its proactive updates
    /// and by the reader task when replying to a server keepalive that
    /// requested one.
    confirmed: Arc<StdMutex<(Lsn, Lsn, Lsn)>>,
    /// Schema cache, keyed by relation OID, populated from `Relation`
    /// messages observed on the stream. See [`Self::relation`] and
    /// [`Self::decode_tuple`].
    relations: Arc<StdMutex<HashMap<u32, pgoutput::RelationBody>>>,
    reader_task: JoinHandle<()>,
    keepalive_task: JoinHandle<()>,
}

impl Drop for ReplicationStream {
    /// Aborts the background reader and keepalive tasks.
    ///
    /// Both tasks hold `Arc` clones of `write_half`/`confirmed`/`relations`
    /// rather than being their sole owner, so neither can detect on its own
    /// that every `ReplicationStream` handle referencing them has gone
    /// away -- without this, they would otherwise run forever (the
    /// keepalive task on its own timer; the reader task until the
    /// connection happens to close on the server side). Aborting here is
    /// the standard, simple way to guarantee no orphaned background tasks
    /// outlive the stream.
    fn drop(&mut self) {
        self.reader_task.abort();
        self.keepalive_task.abort();
    }
}

impl futures::Stream for ReplicationStream {
    type Item = Result<ReplicationEvent, PgError>;

    /// Delegates directly to `mpsc::Receiver::poll_recv`. `ReplicationStream`
    /// needs no self-referential pinning of its own -- `rx` (an
    /// `mpsc::Receiver`) is `Unpin`, and every other field is either an
    /// `Arc`/`JoinHandle` (also `Unpin`) -- so `self` as a whole is `Unpin`
    /// and `Pin<&mut Self>` derefs to `&mut Self` directly.
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

impl ReplicationStream {
    /// Marks `lsn` as written, flushed, and applied, and immediately sends a
    /// Standby Status Update.
    ///
    /// Call this after durably handling everything up to and including a
    /// `COMMIT` at this LSN -- advancing past data you have not durably
    /// persisted can lose it on reconnect. Equivalent to
    /// `standby_status_update(lsn, lsn, lsn)`.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Connection`] if the write fails (the connection
    /// has closed).
    pub async fn ack(&self, lsn: Lsn) -> Result<(), PgError> {
        self.standby_status_update(lsn, lsn, lsn).await
    }

    /// Sends an explicit Standby Status Update with independently specified
    /// written/flushed/applied LSNs, and updates the stored confirmed-LSN
    /// state `run_keepalive_task` uses for its own proactive updates.
    ///
    /// PostgreSQL's replication protocol tracks these three positions
    /// separately (a standby may have *written* WAL to a not-yet-`fsync`ed
    /// buffer, *flushed* it durably, and *applied* it to its own state, at
    /// three different positions) -- most consumers that process and
    /// durably persist each transaction atomically will want [`Self::ack`],
    /// which sets all three to the same value.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Connection`] if the write fails (the connection
    /// has closed).
    pub async fn standby_status_update(
        &self,
        written: Lsn,
        flushed: Lsn,
        applied: Lsn,
    ) -> Result<(), PgError> {
        set_confirmed(&self.confirmed, written, flushed, applied);
        let payload = copyboth::encode_standby_status_update(
            written,
            flushed,
            applied,
            now_pg_micros(),
            false,
        );
        let mut write_half = self.write_half.lock().await;
        write_half.write_all(&payload).await?;
        Ok(())
    }

    /// Returns the cached schema for `rel_id`, if a `Relation` message
    /// announcing it has been observed on this stream.
    pub fn relation(&self, rel_id: u32) -> Option<pgoutput::RelationBody> {
        lookup_relation(&self.relations, rel_id)
    }

    /// Decodes `tuple`'s cells to OxiSQL values using the cached schema for
    /// `rel_id`.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Protocol`] if no `Relation` message for `rel_id`
    /// has been observed yet on this stream (a protocol desync, not a
    /// normal condition -- PostgreSQL always sends a relation's schema
    /// before the first DML message that references it). Returns
    /// [`PgError::Protocol`] or [`PgError::TypeConversion`] if
    /// `tuple::tuple_to_values` itself fails (a column-count mismatch
    /// against the cached schema, or a value that cannot be parsed per its
    /// column's PostgreSQL type).
    pub fn decode_tuple(
        &self,
        rel_id: u32,
        tuple: &pgoutput::TupleData,
    ) -> Result<Vec<tuple::CellValue>, PgError> {
        decode_tuple_with(&self.relations, rel_id, tuple)
    }
}

// ── PgReplicationConnection::start_logical_replication ──────────────────────

impl PgReplicationConnection {
    /// Issues `START_REPLICATION` and, on success, switches the connection
    /// into `CopyBoth` streaming mode, returning a [`ReplicationStream`].
    ///
    /// Consumes `self`: once the connection enters `CopyBoth` mode there is
    /// no way back to the simple-query command surface
    /// ([`identify_system`](Self::identify_system),
    /// [`create_replication_slot`](Self::create_replication_slot), ...) --
    /// PostgreSQL does not allow interleaving ordinary queries with
    /// replication streaming on the same connection.
    ///
    /// This is an MVP-scoped call: it always requests logical-decoding
    /// protocol version 2 with `streaming`, `binary`, and `messages` all
    /// off (text-format tuples, no in-progress-transaction streaming, no
    /// `pg_logical_emit_message` delivery) -- see
    /// `commands::StartReplicationOptions` for what each option means and
    /// `pgoutput`'s module documentation for what this decoder supports.
    ///
    /// `publication_names` must name at least one publication already
    /// created on the server (e.g. via `CREATE PUBLICATION`); `start_lsn`
    /// is typically the `consistent_point` returned by
    /// [`create_replication_slot`](Self::create_replication_slot) for a
    /// freshly created slot, or the last LSN acknowledged via
    /// [`ReplicationStream::ack`] when resuming an existing slot.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Replication`] if `slot_name` or any
    /// `publication_names` entry is invalid (see
    /// `commands::build_start_replication`), or if the server answers
    /// with an `ErrorResponse` (e.g. the slot or publication does not
    /// exist, or the connection lacks replication privileges). Returns
    /// [`PgError::Protocol`] if the response cannot be parsed or is an
    /// unexpected message type, and [`PgError::Connection`] on a socket I/O
    /// error or if the connection closes before a complete response
    /// arrives.
    pub async fn start_logical_replication(
        mut self,
        slot_name: &str,
        publication_names: &[&str],
        start_lsn: Lsn,
    ) -> Result<ReplicationStream, PgError> {
        let options = commands::StartReplicationOptions {
            proto_version: 2,
            publication_names: publication_names.iter().map(|s| s.to_string()).collect(),
            streaming: false,
            binary: false,
            messages: false,
        };
        let command = commands::build_start_replication(slot_name, start_lsn, &options)?;

        super::send_query_message(&mut self.stream, &command).await?;
        read_start_replication_response(&mut self.stream, &mut self.read_buf).await?;

        let PgReplicationConnection { stream, read_buf } = self;
        let (read_half, write_half) = tokio::io::split(stream);
        let write_half = Arc::new(TokioMutex::new(write_half));
        let confirmed = Arc::new(StdMutex::new((start_lsn, start_lsn, start_lsn)));
        let relations = Arc::new(StdMutex::new(HashMap::new()));
        let (tx, rx) = mpsc::channel(REPLICATION_EVENT_CHANNEL_CAPACITY);

        let reader_task = tokio::spawn(run_reader_task(
            read_half,
            Arc::clone(&write_half),
            Arc::clone(&confirmed),
            Arc::clone(&relations),
            tx,
            read_buf,
        ));
        let keepalive_task = tokio::spawn(run_keepalive_task(
            Arc::clone(&write_half),
            Arc::clone(&confirmed),
            KEEPALIVE_INTERVAL,
        ));

        Ok(ReplicationStream {
            rx,
            write_half,
            confirmed,
            relations,
            reader_task,
            keepalive_task,
        })
    }
}

// ── START_REPLICATION response parsing ───────────────────────────────────────

/// Reads and parses the server's response to a `START_REPLICATION` command,
/// growing `buf` with additional reads from `stream` as needed.
///
/// Mirrors [`auth::read_message`]'s own incremental-parse loop, delegating
/// the actual per-message dispatch to
/// [`try_parse_start_replication_response`] so that function -- the only
/// part with any real branching logic -- stays unit-testable with
/// hand-built byte fixtures and no I/O.
///
/// # Errors
///
/// See [`try_parse_start_replication_response`]. Additionally returns
/// [`PgError::Connection`] if the connection closes before a complete
/// message arrives.
async fn read_start_replication_response<S>(
    stream: &mut S,
    buf: &mut BytesMut,
) -> Result<copyboth::CopyBothResponse, PgError>
where
    S: AsyncRead + Unpin,
{
    loop {
        if let Some(response) = try_parse_start_replication_response(buf)? {
            return Ok(response);
        }
        let n = stream.read_buf(buf).await?;
        if n == 0 {
            return Err(PgError::Connection(
                "server closed the connection while waiting for a response to \
                 START_REPLICATION"
                    .to_string(),
            ));
        }
    }
}

/// Parses the server's response to a `START_REPLICATION` command from
/// already-buffered bytes, performing no I/O.
///
/// `START_REPLICATION`'s success response is `CopyBothResponse` (tag
/// `'W'`), a message shape `postgres_protocol::message::backend::Message::parse`
/// cannot decode (it has no matching enum variant -- see [`super`]'s module
/// documentation and [`auth::read_message`]'s doc comment). This function
/// therefore inspects the raw header via
/// `postgres_protocol::message::backend::Header::parse`, which -- unlike
/// `Message::parse` -- only peeks the tag and declared length without
/// needing to recognize the tag at all.
///
/// Returns `Ok(None)` if `buf` does not yet contain a complete message,
/// matching `Header::parse`'s/`Message::parse`'s own "not enough bytes yet"
/// convention -- callers such as [`read_start_replication_response`] read
/// more bytes and retry, and `buf` is left completely untouched in this
/// case. Once a complete message is buffered, this function always
/// resolves definitively (`Ok(Some(_))` or `Err(_)`) and removes exactly
/// that one message's bytes from the front of `buf` via
/// `BytesMut::split_to`, leaving any bytes after it -- the start of
/// `CopyBoth`-mode traffic -- untouched in `buf` for the caller to hand off
/// to [`run_reader_task`].
///
/// # Errors
///
/// Returns [`PgError::Protocol`] if the header or a `'W'` body is
/// malformed, or if the tag is anything other than `'W'` or `'E'`. Returns
/// [`PgError::Replication`] (via [`auth::server_error_response`]) if the
/// tag is `'E'` (`ErrorResponse`).
fn try_parse_start_replication_response(
    buf: &mut BytesMut,
) -> Result<Option<copyboth::CopyBothResponse>, PgError> {
    let Some(header) = Header::parse(&buf[..])
        .map_err(|e| PgError::Protocol(format!("malformed response to START_REPLICATION: {e}")))?
    else {
        return Ok(None);
    };

    let declared_len = usize::try_from(header.len()).map_err(|e| {
        PgError::Protocol(format!(
            "START_REPLICATION response declared an out-of-range message length: {e}"
        ))
    })?;
    // The header's declared length counts itself (4 bytes) but not the
    // leading tag byte, so the total on-wire message length is 1 + len.
    let total_len = 1 + declared_len;
    if buf.len() < total_len {
        return Ok(None);
    }

    let tag = header.tag();
    let mut message = buf.split_to(total_len);

    let result = match tag {
        b'W' => copyboth::parse_copy_both_response(&message[MESSAGE_HEADER_LEN..]),
        b'E' => match Message::parse(&mut message) {
            Ok(Some(Message::ErrorResponse(body))) => Err(auth::server_error_response(&body)),
            Ok(_) => Err(PgError::Protocol(
                "inconsistent tag while re-parsing an ErrorResponse in reply to \
                 START_REPLICATION"
                    .to_string(),
            )),
            Err(e) => Err(PgError::Protocol(format!(
                "malformed ErrorResponse in reply to START_REPLICATION: {e}"
            ))),
        },
        other => Err(PgError::Protocol(format!(
            "unexpected response to START_REPLICATION: backend message tag {other:#04x} \
             (expected 'W' CopyBothResponse or 'E' ErrorResponse)"
        ))),
    };
    result.map(Some)
}

// ── Background reader task ───────────────────────────────────────────────────

/// Background task that owns the `CopyBoth`-mode read half of a replication
/// connection, decoding server traffic and forwarding it into `tx`.
///
/// Spawned once by [`PgReplicationConnection::start_logical_replication`]
/// (with `R = tokio::io::ReadHalf<auth::MaybeTlsStream>`, `W =
/// tokio::io::WriteHalf<auth::MaybeTlsStream>`) and exercised directly by
/// this module's tests with `R`/`W` instantiated over an in-memory
/// `tokio::io::duplex` pair instead -- see the [module documentation](self).
///
/// `buf` carries forward any bytes already read past the `START_REPLICATION`
/// response's boundary (see [`try_parse_start_replication_response`]); it is
/// intentionally *not* a fresh buffer, so no already-buffered `CopyBoth`
/// traffic is ever discarded.
///
/// Terminates (returns) when:
/// - the consumer drops the [`ReplicationStream`]/its `rx` (a `tx.send`
///   call returns `Err`) -- nothing more to do;
/// - the server sends `CopyDone` (a graceful, server-initiated end of
///   streaming) or the connection closes cleanly (EOF, surfaced by
///   [`auth::read_message`] as [`PgError::Connection`]) -- both are a
///   normal terminal, so the task simply returns, dropping `tx` and
///   causing the consumer's `poll_next` to yield `Poll::Ready(None)`;
/// - the server sends an `ErrorResponse`, a malformed/undecodable message
///   is received, or an unexpected message type appears -- all genuine
///   failures, reported via one `tx.send(Err(_))` call before returning. A
///   malformed message in particular means the byte stream is desynced;
///   this task deliberately does not attempt to resync (unsafe -- there is
///   no reliable resynchronization point in this protocol).
async fn run_reader_task<R, W>(
    mut read_half: R,
    write_half: Arc<TokioMutex<W>>,
    confirmed: Arc<StdMutex<(Lsn, Lsn, Lsn)>>,
    relations: Arc<StdMutex<HashMap<u32, pgoutput::RelationBody>>>,
    tx: mpsc::Sender<Result<ReplicationEvent, PgError>>,
    mut buf: BytesMut,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    loop {
        let message = match auth::read_message(&mut read_half, &mut buf).await {
            Ok(message) => message,
            // A clean EOF (or any other connection-level I/O error) means
            // there is nothing more to read; treat it the same as a
            // graceful `CopyDone` rather than surfacing it as an error --
            // see this function's doc comment.
            Err(PgError::Connection(_)) => return,
            Err(e) => {
                let _ = tx.send(Err(e)).await;
                return;
            }
        };

        match message {
            Message::CopyDone => return,
            Message::ErrorResponse(body) => {
                let _ = tx.send(Err(auth::server_error_response(&body))).await;
                return;
            }
            Message::CopyData(body) => {
                let should_continue =
                    handle_copy_data(body.data(), &write_half, &confirmed, &relations, &tx).await;
                if !should_continue {
                    return;
                }
            }
            other => {
                let _ = tx
                    .send(Err(PgError::Protocol(format!(
                        "unexpected {} message during CopyBoth replication streaming",
                        super::backend_message_name(&other)
                    ))))
                    .await;
                return;
            }
        }
    }
}

/// Decodes one `CopyData` payload received during `CopyBoth` streaming and
/// forwards the resulting event (or error) into `tx`.
///
/// Returns `true` if [`run_reader_task`]'s loop should keep going, `false`
/// if it should stop (either because `tx.send` failed -- the consumer is
/// gone -- or because a terminal error was already reported).
async fn handle_copy_data<W>(
    payload: &[u8],
    write_half: &Arc<TokioMutex<W>>,
    confirmed: &Arc<StdMutex<(Lsn, Lsn, Lsn)>>,
    relations: &Arc<StdMutex<HashMap<u32, pgoutput::RelationBody>>>,
    tx: &mpsc::Sender<Result<ReplicationEvent, PgError>>,
) -> bool
where
    W: AsyncWrite + Unpin,
{
    let copy_message = match copyboth::parse_copy_both_message(payload) {
        Ok(m) => m,
        Err(e) => {
            let _ = tx.send(Err(e)).await;
            return false;
        }
    };

    match copy_message {
        copyboth::CopyBothMessage::XLogData(xlog) => {
            let decoded = match pgoutput::decode_message(&xlog.data) {
                Ok(m) => m,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return false;
                }
            };
            // Update the schema cache *before* sending the event, so a
            // consumer that reacts to the event by immediately calling
            // `ReplicationStream::relation`/`decode_tuple` always sees it.
            if let pgoutput::LogicalReplicationMessage::Relation(ref rel_body) = decoded {
                lock_recover(relations).insert(rel_body.rel_id, rel_body.clone());
            }
            tx.send(Ok(ReplicationEvent::Logical {
                wal_start: xlog.wal_start,
                wal_end: xlog.wal_end,
                message: decoded,
            }))
            .await
            .is_ok()
        }
        copyboth::CopyBothMessage::KeepAlive(ka) => {
            if tx
                .send(Ok(ReplicationEvent::KeepAlive {
                    wal_end: ka.wal_end,
                    reply_requested: ka.reply_requested,
                }))
                .await
                .is_err()
            {
                return false;
            }
            if ka.reply_requested {
                // Best-effort: if this write fails, the connection is dead
                // and the next `read_message` call at the top of the loop
                // will detect that (EOF/I-O error) and stop cleanly; no
                // need to special-case a write failure here too.
                send_standby_status_update(write_half, confirmed).await;
            }
            true
        }
    }
}

// ── Background keepalive task ────────────────────────────────────────────────

/// Background task that proactively sends a Standby Status Update every
/// `interval`, so the server does not time out the connection during quiet
/// periods with no incoming WAL traffic.
///
/// Spawned alongside [`run_reader_task`] by
/// [`PgReplicationConnection::start_logical_replication`]. Holds `Arc`
/// clones (not the primary owner) of `write_half`/`confirmed`, so it has no
/// way to detect on its own that a [`ReplicationStream`] was dropped --
/// [`ReplicationStream`]'s `Drop` impl aborts this task explicitly instead.
///
/// Stops quietly (no panic, no log spam) the first time a write fails,
/// since that means the connection has already closed.
async fn run_keepalive_task<W>(
    write_half: Arc<TokioMutex<W>>,
    confirmed: Arc<StdMutex<(Lsn, Lsn, Lsn)>>,
    interval: Duration,
) where
    W: AsyncWrite + Unpin,
{
    loop {
        tokio::time::sleep(interval).await;
        let wrote_ok = send_standby_status_update(&write_half, &confirmed).await;
        if !wrote_ok {
            return;
        }
    }
}

/// Builds a Standby Status Update from the current `confirmed` LSNs and
/// writes it to `write_half`.
///
/// `reply_requested` in the outgoing message is always `false`: from this
/// module's two call sites, the update is either a proactive keepalive
/// ([`run_keepalive_task`]) or an answer to a server-initiated keepalive
/// that already asked for a reply ([`handle_copy_data`]) -- in neither case
/// is this client asking the server for anything in return.
///
/// Returns `true` if the write succeeded, `false` if it failed (meaning the
/// connection is no longer usable). The `confirmed` lock is always released
/// before the `.await` on the write -- never held across an await point.
async fn send_standby_status_update<W>(
    write_half: &Arc<TokioMutex<W>>,
    confirmed: &Arc<StdMutex<(Lsn, Lsn, Lsn)>>,
) -> bool
where
    W: AsyncWrite + Unpin,
{
    let (written, flushed, applied) = *lock_recover(confirmed);
    let payload =
        copyboth::encode_standby_status_update(written, flushed, applied, now_pg_micros(), false);
    let mut guard = write_half.lock().await;
    guard.write_all(&payload).await.is_ok()
}

// ── Small pure/synchronous helpers (directly unit-testable, no I/O) ─────────

/// Locks `mutex`, recovering from poisoning rather than propagating it.
///
/// Every critical section guarded by a `std::sync::Mutex` in this module is
/// a trivial, non-panicking operation (a `HashMap` insert/lookup, or
/// reading/overwriting a `(Lsn, Lsn, Lsn)` tuple) that is never held across
/// an `.await` point -- see the [module documentation](self). Since nothing
/// in these critical sections can itself panic, poisoning can only happen
/// if some unrelated code elsewhere panicked while holding the *same*
/// guard, which cannot occur here; recovering via
/// [`std::sync::PoisonError::into_inner`] is therefore safe and keeps an
/// unrelated panic from spuriously breaking this stream.
fn lock_recover<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Returns the current time as PostgreSQL replication-protocol microseconds
/// (microseconds since `2000-01-01 00:00:00 UTC`), for the `client_time`
/// field of an outgoing Standby Status Update.
///
/// Falls back to `0` (the PostgreSQL epoch itself) if the system clock
/// reports a time before the Unix epoch, or the elapsed-microseconds count
/// overflows `i64` -- both practically unreachable on any real system
/// clock, but handled without panicking rather than assumed away.
fn now_pg_micros() -> i64 {
    let unix_micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_micros()).ok())
        .unwrap_or(0);
    lsn::unix_micros_to_pg_micros(unix_micros)
}

/// Overwrites the stored confirmed-LSN state. Factored out of
/// [`ReplicationStream::standby_status_update`] so the state-update logic
/// is unit-testable in isolation from the actual socket write.
fn set_confirmed(confirmed: &StdMutex<(Lsn, Lsn, Lsn)>, written: Lsn, flushed: Lsn, applied: Lsn) {
    *lock_recover(confirmed) = (written, flushed, applied);
}

/// Looks up the cached schema for `rel_id`. Factored out of
/// [`ReplicationStream::relation`] so the lookup is unit-testable against a
/// manually populated map, no I/O needed.
fn lookup_relation(
    relations: &StdMutex<HashMap<u32, pgoutput::RelationBody>>,
    rel_id: u32,
) -> Option<pgoutput::RelationBody> {
    lock_recover(relations).get(&rel_id).cloned()
}

/// Looks up `rel_id`'s cached schema and decodes `tuple` against it.
/// Factored out of [`ReplicationStream::decode_tuple`] so the lookup +
/// decode is unit-testable against a manually populated map, no I/O needed.
fn decode_tuple_with(
    relations: &StdMutex<HashMap<u32, pgoutput::RelationBody>>,
    rel_id: u32,
    tuple_data: &pgoutput::TupleData,
) -> Result<Vec<tuple::CellValue>, PgError> {
    let guard = lock_recover(relations);
    let rel = guard.get(&rel_id).ok_or_else(|| {
        PgError::Protocol(format!(
            "no Relation message observed yet for rel_id {rel_id}; cannot decode tuple \
             (likely a protocol desync)"
        ))
    })?;
    tuple::tuple_to_values(rel, tuple_data)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use bytes::BufMut;
    use futures::StreamExt;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    // ── Wire-format encoding helpers ─────────────────────────────────────────

    /// Encodes one length-prefixed backend message: `tag`, a big-endian `u32`
    /// length (covering itself and `body`), then `body`. Mirrors the
    /// identically-named helper in `mod.rs`'s and `auth.rs`'s own tests.
    fn encode_message(tag: u8, body: &[u8]) -> BytesMut {
        let mut buf = BytesMut::new();
        buf.put_u8(tag);
        let len = u32::try_from(body.len() + 4).expect("test message body fits in u32");
        buf.put_u32(len);
        buf.put_slice(body);
        buf
    }

    /// Encodes a `CopyBothResponse` (`'W'`) message with the given format
    /// and per-column format codes.
    fn encode_copy_both_response(format: i8, column_formats: &[i16]) -> BytesMut {
        let mut body = BytesMut::new();
        body.put_i8(format);
        let count = u16::try_from(column_formats.len()).expect("column count fits in u16");
        body.put_u16(count);
        for f in column_formats {
            body.put_i16(*f);
        }
        encode_message(b'W', &body)
    }

    /// Encodes an `ErrorResponse`/`NoticeResponse` field list, mirroring the
    /// identically-named helper in `mod.rs`'s/`auth.rs`'s own tests.
    fn encode_error_fields(fields: &[(u8, &str)]) -> BytesMut {
        let mut body = BytesMut::new();
        for (type_, value) in fields {
            body.put_u8(*type_);
            body.put_slice(value.as_bytes());
            body.put_u8(0);
        }
        body.put_u8(0);
        body
    }

    /// Wraps `payload` in a `CopyData` (`'d'`) envelope.
    fn encode_copy_data(payload: &[u8]) -> BytesMut {
        encode_message(b'd', payload)
    }

    /// Encodes an `XLogData` (`CopyData` sub-message tag `'w'`) payload:
    /// `Byte1('w')`, `Int64 wal_start`, `Int64 wal_end`, `Int64 server_time`,
    /// then `data`. Does **not** wrap it in the outer `CopyData` envelope --
    /// pass the result to [`encode_copy_data`] for that.
    fn encode_xlog_data_payload(
        wal_start: u64,
        wal_end: u64,
        server_time: i64,
        data: &[u8],
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(b'w');
        buf.extend_from_slice(&wal_start.to_be_bytes());
        buf.extend_from_slice(&wal_end.to_be_bytes());
        buf.extend_from_slice(&server_time.to_be_bytes());
        buf.extend_from_slice(data);
        buf
    }

    /// Encodes a Primary Keepalive (`CopyData` sub-message tag `'k'`)
    /// payload. Does **not** wrap it in the outer `CopyData` envelope.
    fn encode_keepalive_payload(wal_end: u64, server_time: i64, reply_requested: bool) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(b'k');
        buf.extend_from_slice(&wal_end.to_be_bytes());
        buf.extend_from_slice(&server_time.to_be_bytes());
        buf.push(u8::from(reply_requested));
        buf
    }

    /// Appends a null-terminated C-string, mirroring `pgoutput.rs`'s own
    /// test helper of the same name.
    fn push_cstring(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
    }

    /// Encodes a pgoutput `'B'` (Begin) message.
    fn encode_pgoutput_begin(final_lsn: u64, commit_time: i64, xid: u32) -> Vec<u8> {
        let mut buf = vec![b'B'];
        buf.extend_from_slice(&final_lsn.to_be_bytes());
        buf.extend_from_slice(&commit_time.to_be_bytes());
        buf.extend_from_slice(&xid.to_be_bytes());
        buf
    }

    /// Encodes a pgoutput `'C'` (Commit) message.
    fn encode_pgoutput_commit(commit_lsn: u64, end_lsn: u64, commit_time: i64) -> Vec<u8> {
        let mut buf = vec![b'C', 0];
        buf.extend_from_slice(&commit_lsn.to_be_bytes());
        buf.extend_from_slice(&end_lsn.to_be_bytes());
        buf.extend_from_slice(&commit_time.to_be_bytes());
        buf
    }

    /// Encodes a pgoutput `'R'` (Relation) message describing a table with
    /// two `TEXT`-typed (OID 25) columns, `id` (key) and `name`.
    fn encode_pgoutput_relation(rel_id: u32, namespace: &str, name: &str) -> Vec<u8> {
        let mut buf = vec![b'R'];
        buf.extend_from_slice(&rel_id.to_be_bytes());
        push_cstring(&mut buf, namespace);
        push_cstring(&mut buf, name);
        buf.push(b'd'); // REPLICA IDENTITY DEFAULT
        buf.extend_from_slice(&2_i16.to_be_bytes());
        // col 1: key, "id", TEXT (oid 25), atttypmod -1
        buf.push(0x01);
        push_cstring(&mut buf, "id");
        buf.extend_from_slice(&25_u32.to_be_bytes());
        buf.extend_from_slice(&(-1_i32).to_be_bytes());
        // col 2: non-key, "name", TEXT (oid 25), atttypmod -1
        buf.push(0x00);
        push_cstring(&mut buf, "name");
        buf.extend_from_slice(&25_u32.to_be_bytes());
        buf.extend_from_slice(&(-1_i32).to_be_bytes());
        buf
    }

    /// Encodes a pgoutput `'I'` (Insert) message with two text columns,
    /// matching [`encode_pgoutput_relation`]'s two-column schema.
    fn encode_pgoutput_insert(rel_id: u32, id_value: &str, name_value: &str) -> Vec<u8> {
        let mut buf = vec![b'I'];
        buf.extend_from_slice(&rel_id.to_be_bytes());
        buf.push(b'N');
        buf.extend_from_slice(&2_i16.to_be_bytes());
        for value in [id_value, name_value] {
            buf.push(b't');
            let len = i32::try_from(value.len()).expect("test value fits in i32");
            buf.extend_from_slice(&len.to_be_bytes());
            buf.extend_from_slice(value.as_bytes());
        }
        buf
    }

    /// Reads one length-prefixed *frontend* message (tag + big-endian `u32`
    /// length + body) directly, mirroring `mod.rs`'s/`auth.rs`'s own tests.
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

    /// Reads one `CopyData`-wrapped Standby Status Update sent by the
    /// client and returns `(written, flushed, applied, reply_requested)`.
    async fn read_standby_status_update<S: AsyncRead + Unpin>(
        stream: &mut S,
    ) -> (Lsn, Lsn, Lsn, bool) {
        let (tag, body) = read_raw_frontend_message(stream).await;
        assert_eq!(tag, b'd', "expected a CopyData-wrapped message");
        assert_eq!(body[0], b'r', "expected a Standby Status Update");
        let field = |range: std::ops::Range<usize>| -> u64 {
            let bytes: [u8; 8] = body[range]
                .try_into()
                .expect("standby status update field is 8 bytes");
            u64::from_be_bytes(bytes)
        };
        let written = Lsn::from_u64(field(1..9));
        let flushed = Lsn::from_u64(field(9..17));
        let applied = Lsn::from_u64(field(17..25));
        let reply_requested = body[33] != 0;
        (written, flushed, applied, reply_requested)
    }

    // ── try_parse_start_replication_response ─────────────────────────────────

    #[test]
    fn try_parse_valid_w_response_empty_columns() {
        let mut buf = BytesMut::from(&encode_copy_both_response(0, &[])[..]);
        let result = try_parse_start_replication_response(&mut buf)
            .expect("should parse")
            .expect("should be complete");
        assert_eq!(result.format, 0);
        assert!(result.column_formats.is_empty());
        assert!(buf.is_empty(), "the whole message should be consumed");
    }

    #[test]
    fn try_parse_valid_w_response_with_columns() {
        let mut buf = BytesMut::from(&encode_copy_both_response(0, &[0, 1])[..]);
        let result = try_parse_start_replication_response(&mut buf)
            .expect("should parse")
            .expect("should be complete");
        assert_eq!(result.column_formats, vec![0, 1]);
        assert!(buf.is_empty());
    }

    #[test]
    fn try_parse_valid_e_error_response() {
        let body = encode_error_fields(&[
            (b'S', "ERROR"),
            (b'C', "55006"),
            (b'M', "replication slot already in use"),
        ]);
        let mut buf = BytesMut::from(&encode_message(b'E', &body)[..]);
        let result = try_parse_start_replication_response(&mut buf);
        let Err(PgError::Replication(text)) = result else {
            panic!("expected Err(PgError::Replication(_)), got {result:?}");
        };
        assert!(text.contains("55006"), "should surface SQLSTATE: {text}");
        assert!(
            text.contains("already in use"),
            "should surface message: {text}"
        );
        assert!(buf.is_empty(), "the whole message should be consumed");
    }

    #[test]
    fn try_parse_unexpected_tag_is_protocol_error() {
        // 'Z' (ReadyForQuery) is never a valid response to START_REPLICATION.
        let mut buf = BytesMut::from(&encode_message(b'Z', b"I")[..]);
        let result = try_parse_start_replication_response(&mut buf);
        assert!(
            matches!(result, Err(PgError::Protocol(_))),
            "expected a protocol error, got {result:?}"
        );
    }

    #[test]
    fn try_parse_incomplete_header_returns_none_and_buf_untouched() {
        let mut buf = BytesMut::from(&b"\x57\x00\x00"[..]); // 'W' + 2 of 4 length bytes
        let original = buf.clone();
        let result = try_parse_start_replication_response(&mut buf).expect("should not error");
        assert!(result.is_none());
        assert_eq!(buf, original, "an incomplete header must not consume bytes");
    }

    #[test]
    fn try_parse_incomplete_body_returns_none_and_buf_untouched() {
        let full = encode_copy_both_response(0, &[0, 1]);
        // Buffer only the header plus one byte of body.
        let mut buf = BytesMut::from(&full[..MESSAGE_HEADER_LEN + 1]);
        let original = buf.clone();
        let result = try_parse_start_replication_response(&mut buf).expect("should not error");
        assert!(result.is_none());
        assert_eq!(buf, original, "an incomplete body must not consume bytes");
    }

    #[test]
    fn try_parse_leftover_bytes_after_w_response_are_preserved() {
        let mut buf = BytesMut::from(&encode_copy_both_response(0, &[])[..]);
        let leftover = b"the-start-of-copyboth-traffic";
        buf.extend_from_slice(leftover);

        let result = try_parse_start_replication_response(&mut buf)
            .expect("should parse")
            .expect("should be complete");
        assert_eq!(result.format, 0);
        assert_eq!(
            &buf[..],
            leftover,
            "bytes after the CopyBothResponse must be preserved, not discarded"
        );
    }

    #[test]
    fn try_parse_malformed_w_body_is_protocol_error() {
        // format=text, num_columns=2, but zero column-format bytes actually
        // present in the (correctly length-prefixed) body.
        let mut body = BytesMut::new();
        body.put_i8(0);
        body.put_u16(2);
        let mut buf = BytesMut::from(&encode_message(b'W', &body)[..]);
        let result = try_parse_start_replication_response(&mut buf);
        assert!(
            matches!(result, Err(PgError::Protocol(_))),
            "expected a protocol error, got {result:?}"
        );
    }

    // ── read_start_replication_response (async wrapper, duplex) ──────────────

    #[tokio::test]
    async fn read_start_replication_response_across_partial_writes() {
        let (mut client, mut server) = duplex(4096);
        let full = encode_copy_both_response(0, &[0]);
        let split_at = 3; // split mid-header, well before a full message is buffered
        let (first, second) = full.split_at(split_at);
        let first = first.to_vec();
        let second = second.to_vec();

        let server_task = tokio::spawn(async move {
            server.write_all(&first).await.expect("write first half");
            // Give the reader a chance to observe "not enough bytes yet"
            // before the rest arrives.
            tokio::task::yield_now().await;
            server.write_all(&second).await.expect("write second half");
        });

        let mut buf = BytesMut::new();
        let response = read_start_replication_response(&mut client, &mut buf)
            .await
            .expect("should eventually parse the full response");
        assert_eq!(response.column_formats, vec![0]);
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn read_start_replication_response_eof_before_complete_is_connection_error() {
        let (mut client, mut server) = duplex(4096);
        let server_task = tokio::spawn(async move {
            server
                .write_all(b"\x57\x00")
                .await
                .expect("write partial header");
            drop(server);
        });

        let mut buf = BytesMut::new();
        let result = read_start_replication_response(&mut client, &mut buf).await;
        assert!(
            matches!(result, Err(PgError::Connection(_))),
            "expected a connection error, got {result:?}"
        );
        server_task.await.expect("server task panicked");
    }

    // ── run_reader_task (generic logic, duplex-based) ─────────────────────────

    /// Spawns [`run_reader_task`] against one end of a fresh
    /// `tokio::io::duplex` pair, returning the "server" end (for a test to
    /// script), the event receiver, the shared `confirmed`/`relations`
    /// state, and the task's `JoinHandle` (so a test can await it to
    /// confirm clean termination).
    #[allow(clippy::type_complexity)]
    fn spawn_reader_task_over_duplex() -> (
        DuplexStream,
        mpsc::Receiver<Result<ReplicationEvent, PgError>>,
        Arc<StdMutex<(Lsn, Lsn, Lsn)>>,
        Arc<StdMutex<HashMap<u32, pgoutput::RelationBody>>>,
        JoinHandle<()>,
    ) {
        let (client, server) = duplex(65536);
        let (read_half, write_half) = tokio::io::split(client);
        let write_half = Arc::new(TokioMutex::new(write_half));
        let confirmed = Arc::new(StdMutex::new((
            Lsn::from_u64(0),
            Lsn::from_u64(0),
            Lsn::from_u64(0),
        )));
        let relations = Arc::new(StdMutex::new(HashMap::new()));
        let (tx, rx) = mpsc::channel(16);
        let task = tokio::spawn(run_reader_task(
            read_half,
            write_half,
            Arc::clone(&confirmed),
            Arc::clone(&relations),
            tx,
            BytesMut::new(),
        ));
        (server, rx, confirmed, relations, task)
    }

    /// Receives the next event, panicking (with a descriptive message) if
    /// the channel closed instead.
    async fn expect_event(
        rx: &mut mpsc::Receiver<Result<ReplicationEvent, PgError>>,
    ) -> Result<ReplicationEvent, PgError> {
        rx.recv()
            .await
            .expect("expected an event, but the channel closed with no error")
    }

    #[tokio::test]
    async fn reader_task_full_flow_xlogdata_then_keepalive_with_reply() {
        let (mut server, mut rx, confirmed, _relations, task) = spawn_reader_task_over_duplex();

        let begin_payload = encode_pgoutput_begin(0x2000, 12345, 42);
        let xlog = encode_xlog_data_payload(0x1000, 0x2000, 12345, &begin_payload);
        server
            .write_all(&encode_copy_data(&xlog))
            .await
            .expect("write XLogData");

        let event = expect_event(&mut rx).await.expect("expected Ok event");
        let ReplicationEvent::Logical {
            wal_start,
            wal_end,
            message,
        } = event
        else {
            panic!("expected ReplicationEvent::Logical, got {event:?}");
        };
        assert_eq!(wal_start, Lsn::from_u64(0x1000));
        assert_eq!(wal_end, Lsn::from_u64(0x2000));
        assert_eq!(
            message,
            pgoutput::LogicalReplicationMessage::Begin {
                final_lsn: Lsn::from_u64(0x2000),
                commit_time: 12345,
                xid: 42,
            }
        );

        // Set a distinctive `confirmed` value so the assertion below proves
        // the reply actually reads current state, not some hardcoded value.
        set_confirmed(
            &confirmed,
            Lsn::from_u64(0x1234),
            Lsn::from_u64(0x1234),
            Lsn::from_u64(0x1234),
        );

        let ka_payload = encode_keepalive_payload(0x3000, 99_999, true);
        server
            .write_all(&encode_copy_data(&ka_payload))
            .await
            .expect("write PrimaryKeepalive");

        let event = expect_event(&mut rx).await.expect("expected Ok event");
        let ReplicationEvent::KeepAlive {
            wal_end,
            reply_requested,
        } = event
        else {
            panic!("expected ReplicationEvent::KeepAlive, got {event:?}");
        };
        assert_eq!(wal_end, Lsn::from_u64(0x3000));
        assert!(reply_requested);

        // The reader task must have immediately replied with a Standby
        // Status Update carrying the `confirmed` value set above.
        let (written, flushed, applied, reply_req) = read_standby_status_update(&mut server).await;
        assert_eq!(written, Lsn::from_u64(0x1234));
        assert_eq!(flushed, Lsn::from_u64(0x1234));
        assert_eq!(applied, Lsn::from_u64(0x1234));
        assert!(
            !reply_req,
            "the client's own reply should not itself request a reply"
        );

        drop(server);
        drop(rx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn reader_task_stops_cleanly_on_copy_done() {
        let (mut server, mut rx, _confirmed, _relations, task) = spawn_reader_task_over_duplex();
        server
            .write_all(&encode_message(b'c', &[]))
            .await
            .expect("write CopyDone");

        let event = rx.recv().await;
        assert!(
            event.is_none(),
            "expected the stream to end with no error, got {event:?}"
        );
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("reader task should finish promptly")
            .expect("reader task panicked");
    }

    #[tokio::test]
    async fn reader_task_stops_cleanly_on_eof() {
        let (server, mut rx, _confirmed, _relations, task) = spawn_reader_task_over_duplex();
        drop(server); // close the connection without a CopyDone

        let event = rx.recv().await;
        assert!(
            event.is_none(),
            "expected the stream to end with no error, got {event:?}"
        );
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("reader task should finish promptly")
            .expect("reader task panicked");
    }

    #[tokio::test]
    async fn reader_task_reports_error_on_error_response() {
        let (mut server, mut rx, _confirmed, _relations, task) = spawn_reader_task_over_duplex();
        let body = encode_error_fields(&[
            (b'S', "FATAL"),
            (b'C', "57P01"),
            (b'M', "terminating connection due to administrator command"),
        ]);
        server
            .write_all(&encode_message(b'E', &body))
            .await
            .expect("write ErrorResponse");

        let event = expect_event(&mut rx).await;
        let Err(PgError::Replication(text)) = event else {
            panic!("expected Err(PgError::Replication(_)), got {event:?}");
        };
        assert!(text.contains("57P01"), "should surface SQLSTATE: {text}");

        let next = rx.recv().await;
        assert!(next.is_none(), "no more events should follow the error");
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("reader task should finish promptly")
            .expect("reader task panicked");
    }

    #[tokio::test]
    async fn reader_task_reports_error_on_malformed_copy_data_submessage() {
        let (mut server, mut rx, _confirmed, _relations, task) = spawn_reader_task_over_duplex();
        // 'X' is not a recognized CopyData sub-message tag ('w' or 'k').
        server
            .write_all(&encode_copy_data(b"X"))
            .await
            .expect("write malformed CopyData");

        let event = expect_event(&mut rx).await;
        assert!(
            matches!(event, Err(PgError::Protocol(_))),
            "expected a protocol error, got {event:?}"
        );
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("reader task should finish promptly")
            .expect("reader task panicked");
    }

    #[tokio::test]
    async fn reader_task_reports_error_on_malformed_pgoutput_payload_and_does_not_resync() {
        let (mut server, mut rx, _confirmed, _relations, task) = spawn_reader_task_over_duplex();
        // 'Z' is not a recognized top-level pgoutput message tag.
        let garbage = vec![b'Z'];
        let xlog = encode_xlog_data_payload(0x1000, 0x2000, 0, &garbage);
        server
            .write_all(&encode_copy_data(&xlog))
            .await
            .expect("write XLogData with undecodable pgoutput payload");

        let event = expect_event(&mut rx).await;
        assert!(
            matches!(event, Err(PgError::Protocol(_))),
            "expected a protocol error, got {event:?}"
        );

        // Send a perfectly valid message afterward: the task must NOT
        // attempt to resync and emit it, since a malformed message means
        // the byte stream is desynced.
        let begin_payload = encode_pgoutput_begin(0x3000, 0, 1);
        let valid_xlog = encode_xlog_data_payload(0x2000, 0x3000, 0, &begin_payload);
        let _ = server.write_all(&encode_copy_data(&valid_xlog)).await;

        let next = rx.recv().await;
        assert!(
            next.is_none(),
            "task must stop after a malformed message, not resync: got {next:?}"
        );
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("reader task should finish promptly")
            .expect("reader task panicked");
    }

    #[tokio::test]
    async fn reader_task_decodes_commit_message() {
        let (mut server, mut rx, _confirmed, _relations, task) = spawn_reader_task_over_duplex();
        let commit_payload = encode_pgoutput_commit(0x2000, 0x2008, 12345);
        let xlog = encode_xlog_data_payload(0x2000, 0x2008, 12345, &commit_payload);
        server
            .write_all(&encode_copy_data(&xlog))
            .await
            .expect("write Commit XLogData");

        let event = expect_event(&mut rx).await.expect("expected Ok event");
        let ReplicationEvent::Logical { message, .. } = event else {
            panic!("expected ReplicationEvent::Logical, got {event:?}");
        };
        assert_eq!(
            message,
            pgoutput::LogicalReplicationMessage::Commit {
                flags: 0,
                commit_lsn: Lsn::from_u64(0x2000),
                end_lsn: Lsn::from_u64(0x2008),
                commit_time: 12345,
            }
        );

        drop(server);
        drop(rx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn reader_task_populates_relations_before_sending_event() {
        let (mut server, mut rx, _confirmed, relations, task) = spawn_reader_task_over_duplex();
        let rel_payload = encode_pgoutput_relation(777, "public", "users");
        let xlog = encode_xlog_data_payload(0x1000, 0x2000, 0, &rel_payload);
        server
            .write_all(&encode_copy_data(&xlog))
            .await
            .expect("write Relation XLogData");

        let event = expect_event(&mut rx).await.expect("expected Ok event");

        // By the time the event is observed, the schema cache must already
        // reflect it.
        let cached = lookup_relation(&relations, 777).expect("relation should be cached");
        assert_eq!(cached.name, "users");
        assert_eq!(cached.namespace, "public");
        assert_eq!(cached.columns.len(), 2);

        let ReplicationEvent::Logical {
            message: pgoutput::LogicalReplicationMessage::Relation(body),
            ..
        } = event
        else {
            panic!("expected a Relation event, got {event:?}");
        };
        assert_eq!(body.rel_id, 777);

        drop(server);
        drop(rx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn reader_task_relation_then_insert_decodes_end_to_end() {
        // Exercises the full pipeline through the *actual* wire-decode path
        // (`pgoutput::decode_message` via `run_reader_task`, not a
        // hand-built `RelationBody`): a `Relation` message populates the
        // schema cache, then an `Insert` message referencing it is decoded
        // via `decode_tuple_with` against that cache -- proving `Relation`
        // and `Insert` messages interoperate correctly end to end, matching
        // real `pgoutput` traffic (a `Relation` message always precedes the
        // first DML message that references it).
        let (mut server, mut rx, _confirmed, relations, task) = spawn_reader_task_over_duplex();

        let rel_payload = encode_pgoutput_relation(42, "public", "widgets");
        let rel_xlog = encode_xlog_data_payload(0x1000, 0x1000, 0, &rel_payload);
        server
            .write_all(&encode_copy_data(&rel_xlog))
            .await
            .expect("write Relation XLogData");
        let _relation_event = expect_event(&mut rx).await.expect("expected Ok event");

        let insert_payload = encode_pgoutput_insert(42, "7", "gizmo");
        let insert_xlog = encode_xlog_data_payload(0x1000, 0x2000, 0, &insert_payload);
        server
            .write_all(&encode_copy_data(&insert_xlog))
            .await
            .expect("write Insert XLogData");
        let event = expect_event(&mut rx).await.expect("expected Ok event");

        let ReplicationEvent::Logical {
            message: pgoutput::LogicalReplicationMessage::Insert { rel_id, new_tuple },
            ..
        } = event
        else {
            panic!("expected an Insert event, got {event:?}");
        };
        assert_eq!(rel_id, 42);

        let values = decode_tuple_with(&relations, rel_id, &new_tuple).expect("should decode");
        // `encode_pgoutput_relation` types both columns as TEXT (OID 25, see
        // its own doc comment), so both decode as `Value::Text`, not
        // `Value::I64` -- unlike `decode_tuple_with_present_relation`'s
        // hand-built schema above, which does use an INT4-typed column.
        assert_eq!(
            values,
            vec![
                tuple::CellValue::Value(oxisql_core::Value::Text("7".to_string())),
                tuple::CellValue::Value(oxisql_core::Value::Text("gizmo".to_string())),
            ]
        );

        drop(server);
        drop(rx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn reader_task_stops_when_receiver_dropped() {
        let (mut server, rx, _confirmed, _relations, task) = spawn_reader_task_over_duplex();
        drop(rx);

        // Send something that would normally produce an event; the task
        // should notice the `send` fails and stop instead of hanging or
        // panicking.
        let begin_payload = encode_pgoutput_begin(0x1000, 0, 1);
        let xlog = encode_xlog_data_payload(0x1000, 0x1000, 0, &begin_payload);
        let _ = server.write_all(&encode_copy_data(&xlog)).await;

        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("reader task should stop promptly once the receiver is dropped")
            .expect("reader task panicked");
    }

    #[tokio::test]
    async fn reader_task_reports_error_on_unexpected_message_type() {
        let (mut server, mut rx, _confirmed, _relations, task) = spawn_reader_task_over_duplex();
        // BindComplete ('2') never appears during CopyBoth streaming.
        server
            .write_all(&encode_message(b'2', &[]))
            .await
            .expect("write BindComplete");

        let event = expect_event(&mut rx).await;
        let Err(PgError::Protocol(text)) = event else {
            panic!("expected Err(PgError::Protocol(_)), got {event:?}");
        };
        assert!(
            text.contains("BindComplete"),
            "error should name the unexpected message: {text}"
        );
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("reader task should finish promptly")
            .expect("reader task panicked");
    }

    // ── run_keepalive_task (generic logic, duplex-based) ──────────────────────

    #[tokio::test]
    async fn keepalive_task_sends_updates_using_latest_confirmed_value() {
        let (client, mut server) = duplex(4096);
        let (_read_half, write_half) = tokio::io::split(client);
        let write_half = Arc::new(TokioMutex::new(write_half));
        let confirmed = Arc::new(StdMutex::new((
            Lsn::from_u64(0x1000),
            Lsn::from_u64(0x1000),
            Lsn::from_u64(0x1000),
        )));

        let short_interval = Duration::from_millis(20);
        let task = tokio::spawn(run_keepalive_task(
            Arc::clone(&write_half),
            Arc::clone(&confirmed),
            short_interval,
        ));

        let (written, ..) = read_standby_status_update(&mut server).await;
        assert_eq!(written, Lsn::from_u64(0x1000));

        set_confirmed(
            &confirmed,
            Lsn::from_u64(0x9999),
            Lsn::from_u64(0x9999),
            Lsn::from_u64(0x9999),
        );
        let (written, ..) = read_standby_status_update(&mut server).await;
        assert_eq!(
            written,
            Lsn::from_u64(0x9999),
            "the second update must reflect the newly set confirmed value, not a stale copy"
        );

        task.abort();
    }

    #[tokio::test]
    async fn keepalive_task_stops_quietly_when_write_fails() {
        let (client, server) = duplex(4096);
        let (_read_half, write_half) = tokio::io::split(client);
        let write_half = Arc::new(TokioMutex::new(write_half));
        let confirmed = Arc::new(StdMutex::new((
            Lsn::from_u64(0),
            Lsn::from_u64(0),
            Lsn::from_u64(0),
        )));
        drop(server); // the write half can now never succeed

        let task = tokio::spawn(run_keepalive_task(
            write_half,
            confirmed,
            Duration::from_millis(10),
        ));
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("keepalive task should stop promptly once writes fail")
            .expect("keepalive task panicked");
    }

    // ── Small pure/synchronous helpers ────────────────────────────────────────

    #[test]
    fn set_confirmed_updates_the_tuple() {
        let confirmed = StdMutex::new((Lsn::from_u64(0), Lsn::from_u64(0), Lsn::from_u64(0)));
        set_confirmed(
            &confirmed,
            Lsn::from_u64(1),
            Lsn::from_u64(2),
            Lsn::from_u64(3),
        );
        assert_eq!(
            *confirmed.lock().expect("lock"),
            (Lsn::from_u64(1), Lsn::from_u64(2), Lsn::from_u64(3))
        );
    }

    #[test]
    fn lock_recover_survives_poisoning() {
        let mutex = Arc::new(StdMutex::new(41));
        let mutex_clone = Arc::clone(&mutex);
        let handle = std::thread::spawn(move || {
            let _guard = mutex_clone.lock().expect("lock");
            panic!("intentional panic to poison the mutex");
        });
        let _ = handle.join(); // the panic is expected; ignore the Err

        // A plain `.lock()` would now return `Err(PoisonError)`; confirm
        // `lock_recover` still succeeds and yields the last-written value.
        let guard = lock_recover(&mutex);
        assert_eq!(*guard, 41);
    }

    #[test]
    fn lookup_relation_present() {
        let mut map = HashMap::new();
        let rel = pgoutput::RelationBody {
            rel_id: 5,
            namespace: "public".to_string(),
            name: "widgets".to_string(),
            replica_identity: pgoutput::ReplicaIdentity::Default,
            columns: vec![],
        };
        map.insert(5, rel.clone());
        let relations = StdMutex::new(map);
        assert_eq!(lookup_relation(&relations, 5), Some(rel));
    }

    #[test]
    fn lookup_relation_absent() {
        let relations: StdMutex<HashMap<u32, pgoutput::RelationBody>> =
            StdMutex::new(HashMap::new());
        assert_eq!(lookup_relation(&relations, 999), None);
    }

    #[test]
    fn decode_tuple_with_present_relation() {
        let rel = pgoutput::RelationBody {
            rel_id: 1,
            namespace: "public".to_string(),
            name: "users".to_string(),
            replica_identity: pgoutput::ReplicaIdentity::Default,
            columns: vec![
                pgoutput::ColumnSpec {
                    key: true,
                    name: "id".to_string(),
                    type_oid: 23, // int4
                    type_modifier: -1,
                },
                pgoutput::ColumnSpec {
                    key: false,
                    name: "name".to_string(),
                    type_oid: 25, // text
                    type_modifier: -1,
                },
            ],
        };
        let mut map = HashMap::new();
        map.insert(1, rel);
        let relations = StdMutex::new(map);

        let tuple_data = pgoutput::TupleData {
            columns: vec![
                pgoutput::TupleColumn::Text("42".to_string()),
                pgoutput::TupleColumn::Text("alice".to_string()),
            ],
        };
        let values = decode_tuple_with(&relations, 1, &tuple_data).expect("should decode");
        assert_eq!(
            values,
            vec![
                tuple::CellValue::Value(oxisql_core::Value::I64(42)),
                tuple::CellValue::Value(oxisql_core::Value::Text("alice".to_string())),
            ]
        );
    }

    #[test]
    fn decode_tuple_with_absent_relation_is_protocol_error() {
        let relations: StdMutex<HashMap<u32, pgoutput::RelationBody>> =
            StdMutex::new(HashMap::new());
        let tuple_data = pgoutput::TupleData { columns: vec![] };
        let result = decode_tuple_with(&relations, 42, &tuple_data);
        let Err(PgError::Protocol(text)) = result else {
            panic!("expected Err(PgError::Protocol(_)), got {result:?}");
        };
        assert!(text.contains("42"), "error should name the rel_id: {text}");
    }

    // ── start_logical_replication (concrete, end to end over loopback TCP) ───
    //
    // `MaybeTlsStream` can only wrap a real `TcpStream`/`TlsStream`, never an
    // arbitrary in-memory stream (see this module's and `super`'s own module
    // documentation) — so unlike every test above, these two use a real
    // loopback TCP socket pair rather than `tokio::io::duplex`, in order to
    // exercise the actual public, concrete `start_logical_replication`
    // method end to end. Both wrap the client-side script in
    // `tokio::time::timeout` so a regression that causes a hang fails the
    // test promptly instead of hanging the test binary.

    #[tokio::test]
    async fn start_logical_replication_end_to_end_over_loopback_tcp() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr: SocketAddr = listener.local_addr().expect("read local_addr");

        let server_task = tokio::spawn(async move {
            let (mut server, _peer) = listener.accept().await.expect("accept");

            let (tag, body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(
                tag, b'Q',
                "expected a simple-query message for START_REPLICATION"
            );
            let command_text = String::from_utf8(body[..body.len() - 1].to_vec())
                .expect("command text should be valid UTF-8");
            assert!(
                command_text.starts_with("START_REPLICATION SLOT \"e2e_slot\" LOGICAL 0/1000 ("),
                "unexpected command text: {command_text}"
            );
            assert!(command_text.contains("\"e2e_pub\""), "{command_text}");

            server
                .write_all(&encode_copy_both_response(0, &[]))
                .await
                .expect("write CopyBothResponse");

            let begin_payload = encode_pgoutput_begin(0x2000, 555, 7);
            let xlog = encode_xlog_data_payload(0x1000, 0x2000, 555, &begin_payload);
            server
                .write_all(&encode_copy_data(&xlog))
                .await
                .expect("write XLogData");

            let ka_payload = encode_keepalive_payload(0x2000, 777, true);
            server
                .write_all(&encode_copy_data(&ka_payload))
                .await
                .expect("write PrimaryKeepalive");

            // The reader task should reply to the keepalive promptly.
            let (_written, _flushed, _applied, reply_req) =
                read_standby_status_update(&mut server).await;
            assert!(
                !reply_req,
                "the client's reply should not itself request a reply"
            );

            // The client calls `.ack(0x3000)` next; expect that update too.
            let (written, flushed, applied, _reply_req) =
                read_standby_status_update(&mut server).await;
            assert_eq!(written, Lsn::from_u64(0x3000));
            assert_eq!(flushed, Lsn::from_u64(0x3000));
            assert_eq!(applied, Lsn::from_u64(0x3000));
        });

        let tcp = TcpStream::connect(addr)
            .await
            .expect("connect to loopback listener");
        let conn = PgReplicationConnection {
            stream: MaybeTlsStream::Plain(tcp),
            read_buf: BytesMut::new(),
        };

        let client_script = async {
            let mut replication_stream = conn
                .start_logical_replication("e2e_slot", &["e2e_pub"], Lsn::from_u64(0x1000))
                .await
                .expect("start_logical_replication should succeed");

            let event = replication_stream
                .next()
                .await
                .expect("expected a Logical event")
                .expect("expected Ok event");
            assert!(
                matches!(
                    event,
                    ReplicationEvent::Logical {
                        message: pgoutput::LogicalReplicationMessage::Begin { .. },
                        ..
                    }
                ),
                "expected a Begin event, got {event:?}"
            );

            let event = replication_stream
                .next()
                .await
                .expect("expected a KeepAlive event")
                .expect("expected Ok event");
            assert!(
                matches!(
                    event,
                    ReplicationEvent::KeepAlive {
                        reply_requested: true,
                        ..
                    }
                ),
                "expected a KeepAlive event, got {event:?}"
            );

            replication_stream
                .ack(Lsn::from_u64(0x3000))
                .await
                .expect("ack should succeed");
        };
        tokio::time::timeout(Duration::from_secs(10), client_script)
            .await
            .expect("client script timed out");

        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn start_logical_replication_surfaces_server_error_response() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr: SocketAddr = listener.local_addr().expect("read local_addr");

        let server_task = tokio::spawn(async move {
            let (mut server, _peer) = listener.accept().await.expect("accept");
            let _request = read_raw_frontend_message(&mut server).await;
            let body = encode_error_fields(&[
                (b'S', "ERROR"),
                (b'C', "42704"),
                (b'M', "publication \"nope\" does not exist"),
            ]);
            server
                .write_all(&encode_message(b'E', &body))
                .await
                .expect("write ErrorResponse");
        });

        let tcp = TcpStream::connect(addr)
            .await
            .expect("connect to loopback listener");
        let conn = PgReplicationConnection {
            stream: MaybeTlsStream::Plain(tcp),
            read_buf: BytesMut::new(),
        };

        let client_script = async {
            conn.start_logical_replication("e2e_slot", &["nope"], Lsn::from_u64(0))
                .await
        };
        let result = tokio::time::timeout(Duration::from_secs(10), client_script)
            .await
            .expect("client script timed out");
        let Err(PgError::Replication(text)) = result else {
            panic!(
                "expected Err(PgError::Replication(_)); \
                 note ReplicationStream has no Debug impl to print the Ok case"
            );
        };
        assert!(text.contains("42704"), "should surface SQLSTATE: {text}");

        server_task.await.expect("server task panicked");
    }
}

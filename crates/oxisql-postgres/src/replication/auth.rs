//! Standalone connection establishment for PostgreSQL **logical replication**
//! connections.
//!
//! # Why this duplicates `tokio-postgres`'s own connection setup
//!
//! Everywhere else in this crate, connections are established through
//! `tokio_postgres::connect` (see `crate::connection::PgConnection::connect`).
//! That path cannot be reused here: PostgreSQL only accepts replication
//! protocol commands (`IDENTIFY_SYSTEM`, `START_REPLICATION`, ...) on a
//! connection whose *StartupMessage* included a `replication` parameter set
//! to `"database"` (logical replication) or `"true"`/`"on"`/`"1"` (physical
//! replication) — and `tokio-postgres` has no public API to add arbitrary
//! startup parameters; it only ever sends `user`, `dbname`→`database`, and a
//! fixed handful of others it chooses itself. So this module drives the
//! PostgreSQL frontend/backend wire protocol by hand, using the same
//! low-level primitives (`postgres-protocol`) that `tokio-postgres` itself is
//! built on internally, up through the point where the connection is
//! authenticated and ready for query traffic (`ReadyForQuery`). Everything
//! after that — issuing `IDENTIFY_SYSTEM`, `START_REPLICATION`, and switching
//! to `CopyBoth` mode — is out of scope for this file and is the
//! responsibility of sibling modules.
//!
//! # Known limitations
//!
//! - **`NoticeResponse` fields are drained but not parsed.**
//!   `ErrorResponse` messages *are* fully parsed — see
//!   [`server_error_response`], which walks `ErrorResponseBody::fields()` (a
//!   `fallible_iterator_02::FallibleIterator`, from the `fallible-iterator`
//!   **0.2.x** line that `postgres-protocol` 0.6.12 is compiled against — see
//!   the `fallible-iterator-02` alias in the workspace root `Cargo.toml`) to
//!   extract the `SQLSTATE` (`'C'`) and message (`'M'`) fields. Sibling
//!   `NoticeResponseBody::fields()` has the identical shape but is not
//!   inspected: notices are drained and ignored by [`drain_until_ready`]
//!   because they are purely advisory (e.g. replication slot creation
//!   progress) and never fail a connection attempt. Wiring up notice-field
//!   parsing, if a caller ever needs the text, is a small mechanical
//!   follow-up that can reuse the same field-walking pattern.
//! - **SASL mechanism negotiation checks for plain `SCRAM-SHA-256` only.**
//!   [`authenticate`] enumerates `AuthenticationSaslBody::mechanisms()` (via
//!   [`select_scram_mechanism`]) and fails clearly with
//!   [`PgError::Replication`] when the server's advertised list does not
//!   include unsuffixed `SCRAM-SHA-256`, rather than blindly assuming the
//!   server offers it. It still never *selects* `SCRAM-SHA-256-PLUS`, even
//!   when a server offers only that — see the channel-binding bullet below.
//! - **Channel binding (`SCRAM-SHA-256-PLUS`) is explicitly out of scope**
//!   for this pass, per the implementation plan.
//!   `postgres_protocol::authentication::sasl::ChannelBinding::unsupported`
//!   is used unconditionally, so a server that offers only
//!   `SCRAM-SHA-256-PLUS` (no unsuffixed `SCRAM-SHA-256`) is rejected by
//!   [`select_scram_mechanism`] rather than attempted. Adding `-PLUS` support
//!   (binding to the `tls-server-end-point` channel-binding data of the
//!   upgraded TLS connection) is a documented future improvement.
//!
//! # Testing status
//!
//! This module's pure helpers ([`build_password_response`],
//! [`build_startup_params`], [`extract_password`], [`classify_ssl_response`],
//! [`select_scram_mechanism`]) are unit-tested directly, as is
//! [`server_error_response`] (fed a real `ErrorResponseBody` produced by
//! round-tripping hand-built wire bytes through `Message::parse`, the same
//! way the `AuthenticationMd5Password` tests below build a real
//! `AuthenticationMd5PasswordBody`). The message-framing/authentication
//! *state machine* ([`authenticate`], [`run_scram_exchange`] error paths,
//! [`drain_until_ready`]) is exercised end-to-end against an in-memory
//! `tokio::io::duplex` stream standing in for the network, driven by a fake
//! "server" task that speaks just enough of the wire protocol to observe what
//! the code under test sends and script what it receives — including
//! structured `ErrorResponse` field extraction and SASL mechanism-list
//! negotiation (both single- and multi-mechanism lists) — see the `tests`
//! module at the bottom of this file.
//!
//! What is **not** covered by any automated test in this pass: the actual
//! `TcpStream`/`TlsStream` I/O in [`connect_replication`] and [`upgrade_tls`]
//! (needs a real socket or a real TLS handshake peer), and the SCRAM-SHA-256
//! *happy path* specifically (`run_scram_exchange`'s success path needs a
//! peer that performs real server-side SCRAM math against whatever random
//! nonce `ScramSha256::new` generates — `postgres-protocol`'s own crate tests
//! use a fixed nonce via a `pub(crate)`-only constructor this crate cannot
//! reach). Both require either a live PostgreSQL server configured for
//! replication or a considerably more elaborate mock SCRAM server; both are
//! left for live-server integration testing in a later wave.

#![forbid(unsafe_code)]

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BytesMut;
use fallible_iterator_02::FallibleIterator;
use postgres_protocol::authentication::md5_hash;
use postgres_protocol::authentication::sasl::{ChannelBinding, ScramSha256, SCRAM_SHA_256};
use postgres_protocol::message::backend::{ErrorResponseBody, Message};
use postgres_protocol::message::frontend;
use rustls_pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

use crate::builder::TlsMode;
use crate::connection::parse_pg_conn_str;
use crate::error::PgError;

// ── MaybeTlsStream ───────────────────────────────────────────────────────────

/// A raw, authenticated TCP/TLS stream ready for replication-mode command
/// traffic.
///
/// Produced by [`connect_replication`]. Not yet in `CopyBoth` mode — that
/// transition happens after a `START_REPLICATION` command is issued, which is
/// the responsibility of a sibling module, not this one.
#[derive(Debug)]
pub enum MaybeTlsStream {
    /// An unencrypted TCP connection.
    Plain(TcpStream),
    /// A TLS-encrypted connection over TCP.
    ///
    /// Boxed because [`TlsStream`] embeds the full `rustls` session state and
    /// is considerably larger than [`TcpStream`]; boxing keeps that size off
    /// the `Plain` variant for callers that never use TLS.
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for MaybeTlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // Neither `TcpStream` nor `TlsStream<TcpStream>` is self-referential,
        // so both are `Unpin`, and so is this enum (an auto trait derived
        // from its fields) — `get_mut` is therefore safe without any
        // `unsafe` pin-projection machinery.
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            MaybeTlsStream::Tls(s) => Pin::new(&mut **s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTlsStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            MaybeTlsStream::Tls(s) => Pin::new(&mut **s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_flush(cx),
            MaybeTlsStream::Tls(s) => Pin::new(&mut **s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            MaybeTlsStream::Tls(s) => Pin::new(&mut **s).poll_shutdown(cx),
        }
    }
}

// ── connect_replication ──────────────────────────────────────────────────────

/// Establishes a raw replication-mode connection: TCP connect, optional TLS
/// upgrade, PostgreSQL startup with `replication=database`, and full
/// authentication.
///
/// Returns the stream positioned right after authentication completes (i.e.
/// after consuming `BackendKeyData`/`ParameterStatus`/`NoticeResponse` and
/// reaching `ReadyForQuery`), ready for a caller to issue `IDENTIFY_SYSTEM` or
/// other replication commands on it.
///
/// `conn_str` is parsed with [`parse_pg_conn_str`] (the same parser
/// `PgConnection::connect` uses), so both the libpq `key=value` and
/// `postgres://` URL forms are accepted. Unlike `PgConnection::connect`, a
/// password embedded in the connection string *is* read out here (needed
/// because this module drives authentication itself rather than delegating
/// to `tokio-postgres`) — see [`extract_password`].
///
/// # Errors
///
/// Returns:
/// - [`PgError::Replication`] if `conn_str` cannot be parsed, does not
///   specify a non-empty user, if the server demands a password that was not
///   supplied, if the server's `AuthenticationSASL` mechanism list does not
///   include unsuffixed `SCRAM-SHA-256`, if a SCRAM-SHA-256 exchange fails
///   cryptographic verification, or if the server sends an `ErrorResponse` at
///   any point during startup or authentication (see [`server_error_response`]
///   for the server-reported `SQLSTATE`/message text this surfaces).
/// - [`PgError::Tls`] if `tls` requests TLS and the server replies that it
///   does not support TLS, if the server name cannot be parsed for SNI, or if
///   the TLS handshake itself fails.
/// - [`PgError::Protocol`] if the server sends a malformed message or a
///   message that is unexpected at the current point in the handshake.
/// - [`PgError::Connection`] on any underlying socket I/O error (via
///   `From<std::io::Error>`), including the server closing the connection
///   before authentication completes.
pub async fn connect_replication(conn_str: &str, tls: TlsMode) -> Result<MaybeTlsStream, PgError> {
    let parts = parse_pg_conn_str(conn_str)
        .map_err(|e| PgError::Replication(format!("invalid replication connection string: {e}")))?;
    let user = match parts.user.as_deref() {
        Some(u) if !u.is_empty() => u,
        _ => {
            return Err(PgError::Replication(
                "connection string must specify a non-empty user for a replication connection"
                    .to_string(),
            ));
        }
    };
    let password = extract_password(conn_str);

    let tcp = TcpStream::connect((parts.host.as_str(), parts.port)).await?;
    // Replication connections are long-lived and latency-sensitive (keepalive
    // round trips, status updates) — matches `tokio-postgres`'s own
    // `connect_socket.rs`, which sets this for the same reason.
    tcp.set_nodelay(true)?;

    let mut stream = upgrade_tls(tcp, &parts.host, tls).await?;

    let params = build_startup_params(user, parts.dbname.as_deref());
    let mut out = BytesMut::new();
    frontend::startup_message(params, &mut out)
        .map_err(|e| PgError::Protocol(format!("failed to build StartupMessage: {e}")))?;
    stream.write_all(&out).await?;

    let mut in_buf = BytesMut::new();
    authenticate(&mut stream, &mut in_buf, user, password.as_deref()).await?;
    drain_until_ready(&mut stream, &mut in_buf).await?;

    Ok(stream)
}

// ── TLS upgrade ──────────────────────────────────────────────────────────────

/// Optionally upgrades a freshly-connected TCP socket to TLS.
///
/// When `tls` is [`TlsMode::Disabled`], returns [`MaybeTlsStream::Plain`]
/// immediately without sending anything. Otherwise sends the `SSLRequest`
/// packet, reads the server's single-byte reply, and upgrades via
/// `tokio_rustls` when that reply is `'S'`.
async fn upgrade_tls(
    mut tcp: TcpStream,
    host: &str,
    tls: TlsMode,
) -> Result<MaybeTlsStream, PgError> {
    let cfg = match tls {
        TlsMode::Disabled => return Ok(MaybeTlsStream::Plain(tcp)),
        TlsMode::Rustls(cfg) => cfg,
    };

    let mut out = BytesMut::new();
    frontend::ssl_request(&mut out);
    tcp.write_all(&out).await?;

    // The SSLRequest reply is a single raw byte, sent *before* the normal
    // tagged-message framing applies — it predates that part of the protocol
    // and is not parsed via `Message::parse`.
    let resp = tcp.read_u8().await?;
    classify_ssl_response(resp)?;

    let domain = ServerName::try_from(host.to_string())
        .map_err(|e| PgError::Tls(format!("invalid server name {host:?} for TLS SNI: {e}")))?;
    let tls_stream = TlsConnector::from(cfg)
        .connect(domain, tcp)
        .await
        .map_err(|e| PgError::Tls(format!("TLS handshake failed: {e}")))?;
    Ok(MaybeTlsStream::Tls(Box::new(tls_stream)))
}

/// Classifies the server's single-byte reply to an `SSLRequest` packet.
///
/// Returns `Ok(())` when the reply is `'S'` (server will proceed with the TLS
/// handshake); otherwise returns the [`PgError`] that reply implies.
fn classify_ssl_response(byte: u8) -> Result<(), PgError> {
    match byte {
        b'S' => Ok(()),
        b'N' => Err(PgError::Tls(
            "server does not support TLS (replied 'N' to SSLRequest)".to_string(),
        )),
        other => Err(PgError::Protocol(format!(
            "unexpected byte {other:#04x} in response to SSLRequest (expected 'S' or 'N')"
        ))),
    }
}

// ── Authentication ───────────────────────────────────────────────────────────

/// Drives the authentication message loop until `AuthenticationOk`.
///
/// Handles cleartext, MD5, and SCRAM-SHA-256 challenges; see the module
/// documentation for the channel-binding limitation that still applies to the
/// SCRAM-SHA-256 case.
async fn authenticate<S>(
    stream: &mut S,
    buf: &mut BytesMut,
    user: &str,
    password: Option<&str>,
) -> Result<(), PgError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        match read_message(stream, buf).await? {
            Message::AuthenticationOk => return Ok(()),
            Message::AuthenticationCleartextPassword => {
                let response = build_password_response(user, require_password(password)?, None);
                send_password(stream, &response).await?;
            }
            Message::AuthenticationMd5Password(body) => {
                let response =
                    build_password_response(user, require_password(password)?, Some(body.salt()));
                send_password(stream, &response).await?;
            }
            Message::AuthenticationSasl(body) => {
                let mechanisms: Vec<&str> = body.mechanisms().collect().map_err(|e| {
                    PgError::Protocol(format!("malformed AuthenticationSASL mechanism list: {e}"))
                })?;
                select_scram_mechanism(&mechanisms)?;
                run_scram_exchange(stream, buf, require_password(password)?).await?;
                // Falls through to the next loop iteration, which reads the
                // `AuthenticationOk` that follows a verified SCRAM exchange
                // (or an `ErrorResponse`/unexpected message, both handled
                // generically by this same loop).
            }
            Message::ErrorResponse(body) => return Err(server_error_response(&body)),
            _ => {
                return Err(PgError::Protocol(
                    "unexpected message during replication authentication".to_string(),
                ));
            }
        }
    }
}

/// Drives one full client side of a SCRAM-SHA-256 exchange, from
/// `SASLInitialResponse` through verifying `AuthenticationSASLFinal`.
///
/// Channel binding is not implemented (`SCRAM-SHA-256-PLUS` is out of scope
/// for this pass — see the module documentation): this always offers plain
/// `SCRAM-SHA-256` via [`ChannelBinding::unsupported`].
///
/// On success, the caller still owes one more [`read_message`] call for the
/// `AuthenticationOk` that follows — this function only covers the
/// `SASLInitialResponse`/`SASLContinue`/`SASLResponse`/`SASLFinal` round trip.
async fn run_scram_exchange<S>(
    stream: &mut S,
    buf: &mut BytesMut,
    password: &str,
) -> Result<(), PgError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut scram = ScramSha256::new(password.as_bytes(), ChannelBinding::unsupported());

    let mut out = BytesMut::new();
    frontend::sasl_initial_response(SCRAM_SHA_256, scram.message(), &mut out)
        .map_err(|e| PgError::Protocol(format!("failed to build SASLInitialResponse: {e}")))?;
    stream.write_all(&out).await?;

    match read_message(stream, buf).await? {
        Message::AuthenticationSaslContinue(body) => scram.update(body.data()).map_err(|e| {
            PgError::Replication(format!(
                "SCRAM-SHA-256 exchange failed while processing the server's first message: {e}"
            ))
        })?,
        Message::ErrorResponse(body) => return Err(server_error_response(&body)),
        _ => {
            return Err(PgError::Protocol(
                "expected AuthenticationSASLContinue after SASLInitialResponse".to_string(),
            ));
        }
    }

    let mut out = BytesMut::new();
    frontend::sasl_response(scram.message(), &mut out)
        .map_err(|e| PgError::Protocol(format!("failed to build SASLResponse: {e}")))?;
    stream.write_all(&out).await?;

    match read_message(stream, buf).await? {
        Message::AuthenticationSaslFinal(body) => scram.finish(body.data()).map_err(|e| {
            PgError::Replication(format!(
                "SCRAM-SHA-256 exchange failed while verifying the server's final message: {e}"
            ))
        }),
        Message::ErrorResponse(body) => Err(server_error_response(&body)),
        _ => Err(PgError::Protocol(
            "expected AuthenticationSASLFinal after SASLResponse".to_string(),
        )),
    }
}

/// Consumes `BackendKeyData`, `ParameterStatus`, and `NoticeResponse`
/// messages until `ReadyForQuery`, which marks the end of connection setup.
///
/// These are the only message types PostgreSQL sends between
/// `AuthenticationOk` and `ReadyForQuery` other than `ErrorResponse` (a
/// startup-time failure, e.g. `FATAL: database "..." does not exist` or
/// `FATAL: must be superuser or replication role to start walsender`).
async fn drain_until_ready<S>(stream: &mut S, buf: &mut BytesMut) -> Result<(), PgError>
where
    S: AsyncRead + Unpin,
{
    loop {
        match read_message(stream, buf).await? {
            Message::ReadyForQuery(_) => return Ok(()),
            Message::BackendKeyData(_)
            | Message::ParameterStatus(_)
            | Message::NoticeResponse(_) => {}
            Message::ErrorResponse(body) => return Err(server_error_response(&body)),
            _ => {
                return Err(PgError::Protocol(
                    "unexpected message while waiting for ReadyForQuery after authentication"
                        .to_string(),
                ));
            }
        }
    }
}

/// Builds the [`PgError`] returned when the server sends an `ErrorResponse`
/// during startup or authentication.
///
/// Walks `body.fields()` (a `fallible_iterator_02::FallibleIterator` —
/// [`FallibleIterator`], imported at the top of this module, is what supplies
/// `.next()`) to extract the `SQLSTATE` (`'C'`) and primary message (`'M'`)
/// fields PostgreSQL always includes in a well-formed `ErrorResponse`; see
/// `tokio-postgres`'s own `DbError::parse` (`tokio-postgres`'s
/// `src/error/mod.rs`) for the same field-tag conventions this mirrors, on a
/// much smaller scale (just the two fields most useful for a log line/error
/// message, not the full `DbError` struct).
///
/// Returns [`PgError::Protocol`] instead if the field list itself is
/// malformed (a [`FallibleIterator::next`] call returning `Err`) — that is a
/// decoder bounds-check failure, not a server-reported error condition, so it
/// gets the same variant [`read_message`] uses for other malformed-message
/// cases.
pub(crate) fn server_error_response(body: &ErrorResponseBody) -> PgError {
    let mut sqlstate: Option<String> = None;
    let mut message: Option<String> = None;

    let mut fields = body.fields();
    loop {
        match fields.next() {
            Ok(Some(field)) => {
                let value = String::from_utf8_lossy(field.value_bytes()).into_owned();
                match field.type_() {
                    b'C' => sqlstate = Some(value),
                    b'M' => message = Some(value),
                    _ => {}
                }
            }
            Ok(None) => break,
            Err(e) => {
                return PgError::Protocol(format!(
                    "malformed ErrorResponse field during replication connection setup: {e}"
                ));
            }
        }
    }

    match (sqlstate, message) {
        (Some(sqlstate), Some(message)) => {
            PgError::Replication(format!("server error [{sqlstate}]: {message}"))
        }
        (Some(sqlstate), None) => PgError::Replication(format!(
            "server error [{sqlstate}] (ErrorResponse had no message ('M') field)"
        )),
        (None, Some(message)) => PgError::Replication(format!(
            "server error: {message} (ErrorResponse had no SQLSTATE ('C') field)"
        )),
        (None, None) => PgError::Replication(
            "server sent an ErrorResponse during replication connection startup or \
             authentication with neither a SQLSTATE nor a message field"
                .to_string(),
        ),
    }
}

/// Selects `SCRAM-SHA-256` from the mechanism names a server advertised in
/// `AuthenticationSASL`, rather than assuming it is offered.
///
/// Returns the matching entry from `mechanisms` on success. Returns
/// [`PgError::Replication`] when `mechanisms` does not contain unsuffixed
/// `SCRAM-SHA-256` — including when the server offers only
/// `SCRAM-SHA-256-PLUS` (channel binding, which this module does not
/// implement — see the module documentation).
fn select_scram_mechanism<'a>(mechanisms: &[&'a str]) -> Result<&'a str, PgError> {
    mechanisms
        .iter()
        .find(|&&mechanism| mechanism == SCRAM_SHA_256)
        .copied()
        .ok_or_else(|| {
            PgError::Replication(format!(
                "server does not support SCRAM-SHA-256 authentication; offered: {mechanisms:?}"
            ))
        })
}

/// Requires that a password was supplied, for challenges that need one.
fn require_password(password: Option<&str>) -> Result<&str, PgError> {
    password.ok_or_else(|| {
        PgError::Replication(
            "server requires a password but none was supplied in the connection string".to_string(),
        )
    })
}

/// Sends a `PasswordMessage` carrying `response` (already MD5-hashed by the
/// caller when responding to an MD5 challenge; sent as-is for cleartext).
async fn send_password<S>(stream: &mut S, response: &str) -> Result<(), PgError>
where
    S: AsyncWrite + Unpin,
{
    let mut out = BytesMut::new();
    frontend::password_message(response.as_bytes(), &mut out)
        .map_err(|e| PgError::Protocol(format!("failed to build PasswordMessage: {e}")))?;
    stream.write_all(&out).await?;
    Ok(())
}

// ── Wire-message read loop ───────────────────────────────────────────────────

/// Reads and returns the next complete backend message, growing `buf` with
/// additional socket reads as needed.
///
/// Mirrors the incremental-parse loop `tokio-postgres`'s own codec uses (see
/// `tokio-postgres`'s `src/codec.rs`, `Decoder::decode`): `Message::parse`
/// returns `Ok(None)` when `buf` does not yet hold a complete message, after
/// first calling `buf.reserve(..)` for (at least) the additional bytes it
/// knows it still needs, so the following `read_buf` call is never a wasted,
/// tiny read.
pub(crate) async fn read_message<S>(stream: &mut S, buf: &mut BytesMut) -> Result<Message, PgError>
where
    S: AsyncRead + Unpin,
{
    loop {
        if let Some(msg) = Message::parse(buf).map_err(|e| {
            PgError::Protocol(format!(
                "malformed backend message during replication connection setup: {e}"
            ))
        })? {
            return Ok(msg);
        }
        let n = stream.read_buf(buf).await?;
        if n == 0 {
            return Err(PgError::Connection(
                "server closed the connection during replication connection setup".to_string(),
            ));
        }
    }
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

/// Builds the `(key, value)` parameter list for the replication-mode
/// PostgreSQL `StartupMessage`.
///
/// Always includes `user` and the literal `replication = "database"`
/// parameter — the entire reason this module exists instead of delegating to
/// `tokio_postgres::connect` (see the module documentation). `database` is
/// included only when `dbname` is `Some`; PostgreSQL requires a database name
/// for *logical* replication connections, but rejecting its absence here
/// (rather than letting the server reject it with a clear `FATAL`
/// `ErrorResponse`) is left to the server, matching this function's narrow
/// job of building the parameter list, not validating it.
fn build_startup_params<'a>(user: &'a str, dbname: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut params = vec![("user", user), ("replication", "database")];
    if let Some(db) = dbname {
        params.push(("database", db));
    }
    params
}

/// Computes the value to send in a `PasswordMessage` in response to an
/// authentication challenge.
///
/// `salt` is `Some` for `AuthenticationMD5Password` (hash the password with
/// [`md5_hash`], which already returns the `"md5"`-prefixed wire format — see
/// the unit tests below) and `None` for `AuthenticationCleartextPassword`
/// (send the password unmodified).
fn build_password_response(user: &str, password: &str, salt: Option<[u8; 4]>) -> String {
    match salt {
        Some(salt) => md5_hash(user.as_bytes(), password.as_bytes(), salt),
        None => password.to_string(),
    }
}

/// Extracts the `password` field from a PostgreSQL connection string.
///
/// `crate::connection::PgConnParts` (returned by [`parse_pg_conn_str`])
/// intentionally does not carry a password: ordinary connections hand the raw
/// connection string straight to `tokio_postgres::connect`, which does its
/// own parsing and never needs the password surfaced separately. This module
/// drives authentication itself, so it needs the value — this function
/// duplicates just the password-extraction half of `parse_pg_conn_str`'s two
/// accepted formats (`key=value` and `postgres://` URLs) rather than
/// extending the shared `PgConnParts` type, to avoid touching that
/// widely-used shared file while sibling `replication/` modules are being
/// built concurrently.
///
/// **Recommended follow-up for the integration wave:** add a
/// `password: Option<String>` field to `PgConnParts` and delete this function
/// in favor of it.
fn extract_password(conn_str: &str) -> Option<String> {
    let trimmed = conn_str.trim();
    if let Some(rest) = trimmed
        .strip_prefix("postgresql://")
        .or_else(|| trimmed.strip_prefix("postgres://"))
    {
        let (rest, _query) = rest.split_once('?').unwrap_or((rest, ""));
        let user_info = &rest[..rest.rfind('@')?];
        let (_user, password) = user_info.split_once(':')?;
        if password.is_empty() {
            None
        } else {
            Some(password.to_string())
        }
    } else {
        trimmed.split_whitespace().find_map(|token| {
            let (k, v) = token.split_once('=')?;
            (k == "password" && !v.is_empty()).then(|| v.to_string())
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use bytes::BufMut;
    use tokio::io::duplex;

    use super::*;

    // ── build_password_response ─────────────────────────────────────────────

    #[test]
    fn cleartext_password_response_is_unmodified() {
        assert_eq!(build_password_response("alice", "hunter2", None), "hunter2");
    }

    #[test]
    fn md5_password_response_matches_postgres_protocol_reference_vector() {
        // Same (username, password, salt) triple as `postgres_protocol`'s own
        // `authentication::test::md5` doctest, so this also pins down that
        // `md5_hash`'s return value already carries the `"md5"` wire-format
        // prefix (no extra prefixing needed by callers of this helper).
        let salt = [0x2a, 0x3d, 0x8f, 0xe0];
        let response = build_password_response("md5_user", "password", Some(salt));
        assert_eq!(response, "md562af4dd09bbb41884907a838a3233294");
        assert!(response.starts_with("md5"));
    }

    #[test]
    fn md5_password_response_changes_with_salt() {
        let a = build_password_response("bob", "s3cret", Some([0, 0, 0, 0]));
        let b = build_password_response("bob", "s3cret", Some([1, 2, 3, 4]));
        assert_ne!(a, b);
    }

    #[test]
    fn md5_password_response_changes_with_user() {
        let a = build_password_response("alice", "s3cret", Some([1, 2, 3, 4]));
        let b = build_password_response("bob", "s3cret", Some([1, 2, 3, 4]));
        assert_ne!(a, b);
    }

    // ── build_startup_params ────────────────────────────────────────────────

    #[test]
    fn startup_params_include_replication_database_and_dbname_when_present() {
        let params = build_startup_params("alice", Some("mydb"));
        assert_eq!(
            params,
            vec![
                ("user", "alice"),
                ("replication", "database"),
                ("database", "mydb"),
            ]
        );
    }

    #[test]
    fn startup_params_omit_database_key_when_dbname_absent() {
        let params = build_startup_params("alice", None);
        assert_eq!(params, vec![("user", "alice"), ("replication", "database")]);
        assert!(!params.iter().any(|(k, _)| *k == "database"));
    }

    // ── extract_password ────────────────────────────────────────────────────

    #[test]
    fn extract_password_from_kv_form() {
        assert_eq!(
            extract_password("host=x port=5432 user=bob password=hunter2"),
            Some("hunter2".to_string())
        );
    }

    #[test]
    fn extract_password_missing_in_kv_form() {
        assert_eq!(extract_password("host=x user=bob"), None);
    }

    #[test]
    fn extract_password_empty_value_in_kv_form_is_none() {
        assert_eq!(extract_password("host=x user=bob password="), None);
    }

    #[test]
    fn extract_password_from_uri_form() {
        assert_eq!(
            extract_password("postgres://bob:hunter2@host/db"),
            Some("hunter2".to_string())
        );
    }

    #[test]
    fn extract_password_from_postgresql_scheme_uri_form() {
        assert_eq!(
            extract_password("postgresql://bob:hunter2@host:5433/db"),
            Some("hunter2".to_string())
        );
    }

    #[test]
    fn extract_password_absent_in_uri_form_without_colon() {
        assert_eq!(extract_password("postgres://bob@host/db"), None);
    }

    #[test]
    fn extract_password_absent_in_uri_form_without_userinfo() {
        assert_eq!(extract_password("postgres://host/db"), None);
    }

    #[test]
    fn extract_password_ignores_query_string() {
        assert_eq!(
            extract_password("postgres://bob:hunter2@host/db?sslmode=require"),
            Some("hunter2".to_string())
        );
    }

    // ── require_password ────────────────────────────────────────────────────

    #[test]
    fn require_password_ok_when_present() {
        assert_eq!(require_password(Some("x")).unwrap(), "x");
    }

    #[test]
    fn require_password_err_when_absent() {
        assert!(matches!(
            require_password(None),
            Err(PgError::Replication(_))
        ));
    }

    // ── classify_ssl_response ────────────────────────────────────────────────

    #[test]
    fn classify_ssl_response_s_means_proceed() {
        assert!(classify_ssl_response(b'S').is_ok());
    }

    #[test]
    fn classify_ssl_response_n_means_tls_unsupported() {
        assert!(matches!(classify_ssl_response(b'N'), Err(PgError::Tls(_))));
    }

    #[test]
    fn classify_ssl_response_other_byte_is_protocol_error() {
        assert!(matches!(
            classify_ssl_response(b'X'),
            Err(PgError::Protocol(_))
        ));
    }

    // ── select_scram_mechanism ───────────────────────────────────────────────

    #[test]
    fn select_scram_mechanism_picks_scram_sha_256_when_only_option() {
        let mechanisms = ["SCRAM-SHA-256"];
        assert_eq!(
            select_scram_mechanism(&mechanisms).unwrap(),
            "SCRAM-SHA-256"
        );
    }

    #[test]
    fn select_scram_mechanism_picks_scram_sha_256_among_multiple() {
        let mechanisms = ["SCRAM-SHA-256-PLUS", "SCRAM-SHA-256"];
        assert_eq!(
            select_scram_mechanism(&mechanisms).unwrap(),
            "SCRAM-SHA-256"
        );
    }

    #[test]
    fn select_scram_mechanism_rejects_list_without_scram_sha_256() {
        let mechanisms = ["SCRAM-SHA-256-PLUS"];
        let err = select_scram_mechanism(&mechanisms).unwrap_err();
        let PgError::Replication(text) = err else {
            panic!("expected PgError::Replication, got a different variant");
        };
        assert!(
            text.contains("SCRAM-SHA-256"),
            "error text should name the missing mechanism: {text}"
        );
        assert!(
            text.contains("SCRAM-SHA-256-PLUS"),
            "error text should list what the server did offer: {text}"
        );
    }

    #[test]
    fn select_scram_mechanism_rejects_empty_list() {
        let mechanisms: [&str; 0] = [];
        assert!(matches!(
            select_scram_mechanism(&mechanisms),
            Err(PgError::Replication(_))
        ));
    }

    // ── Wire-format helpers for the tests below ─────────────────────────────

    /// Encodes one length-prefixed backend-style message: `tag`, then a
    /// big-endian `u32` length (covering itself and `body`, per the PostgreSQL
    /// wire format), then `body`. Mirrors exactly what `Message::parse`
    /// expects, letting these tests hand-build realistic server traffic.
    fn encode_message(tag: u8, body: &[u8]) -> BytesMut {
        let mut buf = BytesMut::new();
        buf.put_u8(tag);
        let len = u32::try_from(body.len() + 4).expect("test message body fits in u32");
        buf.put_u32(len);
        buf.put_slice(body);
        buf
    }

    /// Encodes an `ErrorResponse`/`NoticeResponse` field list: each
    /// `(type_byte, value)` pair in `fields` as `type_byte` followed by a
    /// NUL-terminated `value`, followed by the mandatory terminating `0x00`
    /// byte `ErrorFields::next` (in `postgres-protocol`) requires to signal
    /// end-of-list — see `server_error_response`'s doc comment. An empty
    /// `fields` slice still produces that lone terminator byte, i.e. a
    /// well-formed *zero-field* body, which is different from a genuinely
    /// empty (zero-byte) body: the latter is malformed per the wire format
    /// (`ErrorFields::next` hits EOF before it can even check for the
    /// terminator).
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

    /// Encodes an `AuthenticationSASL` body's mechanism-name list: each name
    /// in `mechanisms` NUL-terminated, followed by the mandatory terminating
    /// `0x00` byte `SaslMechanisms::next` (in `postgres-protocol`) requires.
    ///
    /// Does **not** include the leading big-endian `i32` auth-type code
    /// (`10`, for SASL) that a full `AuthenticationSASL` wire message needs —
    /// `Message::parse` strips that itself before constructing
    /// `AuthenticationSaslBody`, so callers building a full `'R'`-tagged
    /// message via [`encode_message`] must still `put_i32(10)` before this
    /// function's bytes (see the `authenticate_*_scram_sha_256*` tests below
    /// for the full pattern).
    fn encode_sasl_mechanisms(mechanisms: &[&str]) -> BytesMut {
        let mut body = BytesMut::new();
        for mechanism in mechanisms {
            body.put_slice(mechanism.as_bytes());
            body.put_u8(0);
        }
        body.put_u8(0); // terminator
        body
    }

    /// Reads one length-prefixed *frontend* message (tag + `u32` big-endian
    /// length + body) directly, without going through
    /// `postgres_protocol::message::backend::Message::parse` (which only
    /// knows about *backend* tags). A test harness playing "the server" needs
    /// to read what the client — the code under test — sends, so this small
    /// helper exists purely for that side of these tests.
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

    // ── Wire-format integration: Message::parse ─────────────────────────────

    #[test]
    fn authentication_ok_wire_bytes_parse_to_the_expected_variant() {
        let mut body = BytesMut::new();
        body.put_i32(0);
        let mut buf = encode_message(b'R', &body);
        let msg = Message::parse(&mut buf).unwrap().unwrap();
        assert!(matches!(msg, Message::AuthenticationOk));
    }

    #[test]
    fn authentication_md5_password_wire_bytes_feed_build_password_response() {
        let salt = [0x2a, 0x3d, 0x8f, 0xe0];
        let mut body = BytesMut::new();
        body.put_i32(5);
        body.put_slice(&salt);
        let mut buf = encode_message(b'R', &body);

        let msg = Message::parse(&mut buf).unwrap().unwrap();
        let Message::AuthenticationMd5Password(md5_body) = msg else {
            panic!("expected Message::AuthenticationMd5Password, got a different variant");
        };
        assert_eq!(md5_body.salt(), salt);

        let response = build_password_response("md5_user", "password", Some(md5_body.salt()));
        assert_eq!(response, "md562af4dd09bbb41884907a838a3233294");
    }

    #[test]
    fn error_response_wire_bytes_feed_server_error_response_with_sqlstate_and_message() {
        let body = encode_error_fields(&[
            (b'S', "FATAL"),
            (b'C', "28000"),
            (b'M', "no pg_hba.conf entry for replication connection"),
        ]);
        let mut buf = encode_message(b'E', &body);
        let msg = Message::parse(&mut buf).unwrap().unwrap();
        let Message::ErrorResponse(error_body) = msg else {
            panic!("expected Message::ErrorResponse, got a different variant");
        };

        let err = server_error_response(&error_body);
        let PgError::Replication(text) = err else {
            panic!("expected PgError::Replication, got a different variant");
        };
        assert!(
            text.contains("28000"),
            "error text missing SQLSTATE: {text}"
        );
        assert!(
            text.contains("no pg_hba.conf entry for replication connection"),
            "error text missing message: {text}"
        );
    }

    #[test]
    fn error_response_with_malformed_field_list_yields_protocol_error() {
        // A completely empty (zero-byte) body is malformed per the wire
        // format: `ErrorFields::next` hits EOF trying to read even the first
        // field's type byte, before it can recognize a well-formed *empty*
        // list (which needs at least the lone terminating `0x00` — see
        // `encode_error_fields` called with an empty field list, as the
        // `*_stops_cleanly_on_error_response` tests below do).
        let mut buf = encode_message(b'E', &[]);
        let msg = Message::parse(&mut buf).unwrap().unwrap();
        let Message::ErrorResponse(error_body) = msg else {
            panic!("expected Message::ErrorResponse, got a different variant");
        };

        let err = server_error_response(&error_body);
        assert!(matches!(err, PgError::Protocol(_)));
    }

    #[test]
    fn message_parse_returns_none_when_header_incomplete() {
        // Only the tag plus a partial length prefix buffered so far — mirrors
        // the "not enough bytes yet, read more" branch `read_message` relies
        // on when even the 5-byte header hasn't fully arrived.
        let mut buf = BytesMut::new();
        buf.put_u8(b'R');
        buf.put_slice(&[0, 0]);
        assert!(Message::parse(&mut buf).unwrap().is_none());
    }

    #[test]
    fn message_parse_returns_none_when_body_incomplete() {
        // A full 5-byte header claiming an 8-byte body, but only 2 of those
        // bytes are actually present yet.
        let mut buf = BytesMut::new();
        buf.put_u8(b'R');
        buf.put_u32(8);
        buf.put_slice(&[0, 0]);
        assert!(Message::parse(&mut buf).unwrap().is_none());
    }

    // ── authenticate: end-to-end over an in-memory duplex stream ────────────

    #[tokio::test]
    async fn authenticate_cleartext_password_flow_end_to_end() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let mut body = BytesMut::new();
            body.put_i32(3); // AuthenticationCleartextPassword
            server
                .write_all(&encode_message(b'R', &body))
                .await
                .expect("write AuthenticationCleartextPassword");

            let (tag, body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(tag, b'p');
            assert_eq!(&body[..body.len() - 1], b"hunter2");
            assert_eq!(
                body.last(),
                Some(&0_u8),
                "PasswordMessage must be NUL-terminated"
            );

            let mut ok_body = BytesMut::new();
            ok_body.put_i32(0);
            server
                .write_all(&encode_message(b'R', &ok_body))
                .await
                .expect("write AuthenticationOk");
        });

        let mut buf = BytesMut::new();
        let result = authenticate(&mut client, &mut buf, "alice", Some("hunter2")).await;
        assert!(result.is_ok(), "authenticate() failed: {result:?}");
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn authenticate_md5_password_flow_end_to_end() {
        let (mut client, mut server) = duplex(4096);
        let salt: [u8; 4] = [0x2a, 0x3d, 0x8f, 0xe0];

        let server_task = tokio::spawn(async move {
            let mut body = BytesMut::new();
            body.put_i32(5); // AuthenticationMD5Password
            body.put_slice(&salt);
            server
                .write_all(&encode_message(b'R', &body))
                .await
                .expect("write AuthenticationMD5Password");

            let (tag, body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(tag, b'p');
            let expected = build_password_response("md5_user", "password", Some(salt));
            assert_eq!(&body[..body.len() - 1], expected.as_bytes());

            let mut ok_body = BytesMut::new();
            ok_body.put_i32(0);
            server
                .write_all(&encode_message(b'R', &ok_body))
                .await
                .expect("write AuthenticationOk");
        });

        let mut buf = BytesMut::new();
        let result = authenticate(&mut client, &mut buf, "md5_user", Some("password")).await;
        assert!(result.is_ok(), "authenticate() failed: {result:?}");
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn authenticate_fails_clearly_when_password_required_but_missing() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let mut body = BytesMut::new();
            body.put_i32(3); // AuthenticationCleartextPassword
            server
                .write_all(&encode_message(b'R', &body))
                .await
                .expect("write AuthenticationCleartextPassword");
            // The client should error out locally without writing anything
            // back; nothing further to script here.
        });

        let mut buf = BytesMut::new();
        let result = authenticate(&mut client, &mut buf, "alice", None).await;
        assert!(matches!(result, Err(PgError::Replication(_))));
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn authenticate_stops_cleanly_on_error_response() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            // A minimal but well-formed ErrorResponse: zero fields, just the
            // mandatory terminating `0x00` byte (see `encode_error_fields`
            // with an empty field list) — this test only cares that
            // `authenticate` stops cleanly on *any* ErrorResponse, not on the
            // specific fields; `authenticate_surfaces_structured_error_response_fields`
            // below covers real SQLSTATE/message extraction. An actually
            // empty (zero-byte) body would be malformed per the wire format
            // (missing that terminator) and would instead surface as
            // `PgError::Protocol` — not what this test exercises.
            server
                .write_all(&encode_message(b'E', &encode_error_fields(&[])))
                .await
                .expect("write ErrorResponse");
        });

        let mut buf = BytesMut::new();
        let result = authenticate(&mut client, &mut buf, "alice", Some("hunter2")).await;
        assert!(matches!(result, Err(PgError::Replication(_))));
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn authenticate_surfaces_structured_error_response_fields() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let body = encode_error_fields(&[
                (b'S', "FATAL"),
                (b'C', "28000"),
                (b'M', "no pg_hba.conf entry for replication connection"),
            ]);
            server
                .write_all(&encode_message(b'E', &body))
                .await
                .expect("write ErrorResponse");
        });

        let mut buf = BytesMut::new();
        let result = authenticate(&mut client, &mut buf, "alice", Some("hunter2")).await;
        let Err(PgError::Replication(text)) = result else {
            panic!("expected Err(PgError::Replication(_)), got a different result");
        };
        assert!(
            text.contains("28000"),
            "error text missing SQLSTATE: {text}"
        );
        assert!(
            text.contains("no pg_hba.conf entry for replication connection"),
            "error text missing message: {text}"
        );
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn authenticate_fails_clearly_when_server_does_not_offer_scram_sha_256() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let mut body = BytesMut::new();
            body.put_i32(10); // AuthenticationSASL
            body.put_slice(&encode_sasl_mechanisms(&["SCRAM-SHA-256-PLUS"]));
            server
                .write_all(&encode_message(b'R', &body))
                .await
                .expect("write AuthenticationSASL");
            // The client should error out locally, without attempting a SASL
            // exchange — nothing further to script here.
        });

        let mut buf = BytesMut::new();
        let result = authenticate(&mut client, &mut buf, "alice", Some("hunter2")).await;
        let Err(PgError::Replication(text)) = result else {
            panic!("expected Err(PgError::Replication(_)), got a different result");
        };
        assert!(
            text.contains("SCRAM-SHA-256"),
            "error text should name the required mechanism: {text}"
        );
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn authenticate_proceeds_with_scram_sha_256_when_offered_among_multiple_mechanisms() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let mut body = BytesMut::new();
            body.put_i32(10); // AuthenticationSASL
            body.put_slice(&encode_sasl_mechanisms(&[
                "SCRAM-SHA-256-PLUS",
                "SCRAM-SHA-256",
            ]));
            server
                .write_all(&encode_message(b'R', &body))
                .await
                .expect("write AuthenticationSASL");

            // The client should proceed into the SCRAM exchange (rather than
            // rejecting the mechanism list, which only contains SCRAM-SHA-256
            // as its *second* entry — proving selection, not just presence
            // checking on a single-entry list) and send a
            // SASLInitialResponse naming plain SCRAM-SHA-256.
            let (tag, req_body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(tag, b'p', "expected a SASLInitialResponse");
            assert!(
                req_body.starts_with(b"SCRAM-SHA-256\0"),
                "expected the initial response to name SCRAM-SHA-256"
            );

            // End the exchange cleanly via the already-covered ErrorResponse
            // path — a full SCRAM handshake needs real server-side crypto
            // this test harness does not implement (see the module
            // documentation's Testing status section).
            server
                .write_all(&encode_message(b'E', &encode_error_fields(&[])))
                .await
                .expect("write ErrorResponse");
        });

        let mut buf = BytesMut::new();
        let result = authenticate(&mut client, &mut buf, "alice", Some("hunter2")).await;
        assert!(
            matches!(result, Err(PgError::Replication(_))),
            "authenticate() should reach the SCRAM exchange and stop on ErrorResponse, got {result:?}"
        );
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn authenticate_fails_on_unexpected_message() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            // ReadyForQuery is never valid as the *first* message of an
            // authentication exchange.
            server
                .write_all(&encode_message(b'Z', b"I"))
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let result = authenticate(&mut client, &mut buf, "alice", Some("hunter2")).await;
        assert!(matches!(result, Err(PgError::Protocol(_))));
        server_task.await.expect("server task panicked");
    }

    // ── run_scram_exchange: error paths over an in-memory duplex stream ─────

    #[tokio::test]
    async fn scram_exchange_stops_cleanly_when_server_sends_error_after_initial_response() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let (tag, _body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(tag, b'p', "expected a SASLInitialResponse");
            // Zero fields, just the mandatory terminator — see the comment in
            // `authenticate_stops_cleanly_on_error_response` for why an
            // actually empty (zero-byte) body would not work here.
            server
                .write_all(&encode_message(b'E', &encode_error_fields(&[])))
                .await
                .expect("write ErrorResponse");
        });

        let mut buf = BytesMut::new();
        let result = run_scram_exchange(&mut client, &mut buf, "password").await;
        assert!(matches!(result, Err(PgError::Replication(_))));
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn scram_exchange_fails_on_unexpected_message_after_initial_response() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let (tag, _body) = read_raw_frontend_message(&mut server).await;
            assert_eq!(tag, b'p');
            let mut ok_body = BytesMut::new();
            ok_body.put_i32(0);
            server
                .write_all(&encode_message(b'R', &ok_body))
                .await
                .expect("write AuthenticationOk (unexpected here)");
        });

        let mut buf = BytesMut::new();
        let result = run_scram_exchange(&mut client, &mut buf, "password").await;
        assert!(matches!(result, Err(PgError::Protocol(_))));
        server_task.await.expect("server task panicked");
    }

    // ── drain_until_ready: end-to-end over an in-memory duplex stream ───────

    #[tokio::test]
    async fn drain_until_ready_consumes_backend_key_data_and_parameter_status() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            let mut key_body = BytesMut::new();
            key_body.put_i32(1234);
            key_body.put_i32(5678);
            server
                .write_all(&encode_message(b'K', &key_body))
                .await
                .expect("write BackendKeyData");

            let mut ps_body = BytesMut::new();
            ps_body.put_slice(b"server_version\0");
            ps_body.put_slice(b"16.0\0");
            server
                .write_all(&encode_message(b'S', &ps_body))
                .await
                .expect("write ParameterStatus");

            server
                .write_all(&encode_message(b'Z', b"I"))
                .await
                .expect("write ReadyForQuery");
        });

        let mut buf = BytesMut::new();
        let result = drain_until_ready(&mut client, &mut buf).await;
        assert!(result.is_ok(), "drain_until_ready() failed: {result:?}");
        server_task.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn drain_until_ready_stops_cleanly_on_error_response() {
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            // Zero fields, just the mandatory terminator — see the comment in
            // `authenticate_stops_cleanly_on_error_response` for why an
            // actually empty (zero-byte) body would not work here.
            server
                .write_all(&encode_message(b'E', &encode_error_fields(&[])))
                .await
                .expect("write ErrorResponse");
        });

        let mut buf = BytesMut::new();
        let result = drain_until_ready(&mut client, &mut buf).await;
        assert!(matches!(result, Err(PgError::Replication(_))));
        server_task.await.expect("server task panicked");
    }

    // ── MaybeTlsStream ───────────────────────────────────────────────────────

    #[test]
    fn maybe_tls_stream_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<MaybeTlsStream>();
    }
}

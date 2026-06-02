//! PostgreSQL LISTEN/NOTIFY async notification support.
//!
//! Provides [`PgNotification`], [`NotificationStream`], and the methods
//! [`notify`] and [`listen`] for [`PgConnection`].
//!
//! # Architecture
//!
//! `tokio-postgres` delivers async messages (including `NOTIFY` payloads) via
//! the `Connection` object's `poll_message` method, **not** via the `Client`.
//! When [`PgConnection::connect`] is called, the `Connection` driver is spawned
//! as a background task.  To intercept notifications, that task forwards
//! `AsyncMessage::Notification` items into a
//! `tokio::sync::broadcast::Sender<tokio_postgres::Notification>`.
//!
//! `PgConnection` stores an `Option<broadcast::Sender<_>>`.  When the
//! connection was created via [`PgConnection::from_client`] (where no
//! `Connection` object is available), this field is `None` and `listen()`
//! returns [`PgError::Notify`] with a descriptive message.
//!
//! Multiple [`NotificationStream`] values can coexist — each holds its own
//! `broadcast::Receiver` — and all receive every notification (filtered to
//! the relevant channel by `recv_timeout`).
//!
//! # Channel identifier safety
//!
//! Channel names are validated to contain only ASCII alphanumeric characters
//! and underscores, preventing SQL injection.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, Mutex};

use crate::error::PgError;

/// Capacity of the broadcast channel used to buffer notifications.
///
/// If more than this many notifications arrive without being consumed, the
/// oldest are silently dropped (broadcast semantics).
const NOTIFICATION_CAPACITY: usize = 64;

// ── PgNotification ────────────────────────────────────────────────────────────

/// A notification received via the PostgreSQL `LISTEN/NOTIFY` mechanism.
#[derive(Debug, Clone)]
pub struct PgNotification {
    /// The channel name on which the notification was raised.
    pub channel: String,
    /// The payload string passed by the notifying backend.
    pub payload: String,
    /// The process ID of the notifying backend process.
    pub process_id: i32,
}

impl From<tokio_postgres::Notification> for PgNotification {
    fn from(n: tokio_postgres::Notification) -> Self {
        PgNotification {
            process_id: n.process_id(),
            channel: n.channel().to_string(),
            payload: n.payload().to_string(),
        }
    }
}

// ── NotificationStream ────────────────────────────────────────────────────────

/// A live subscription to a PostgreSQL notification channel.
///
/// Obtained from `PgConnection::listen`.  Drive the stream by calling
/// [`recv_timeout`] in a loop.  Call [`unlisten`] when done to deregister the
/// channel on the server.
///
/// [`recv_timeout`]: NotificationStream::recv_timeout
/// [`unlisten`]: NotificationStream::unlisten
pub struct NotificationStream {
    /// Broadcast receiver from the connection's notification forwarder.
    pub(crate) rx: broadcast::Receiver<PgNotification>,
    /// The channel name this stream is subscribed to.
    pub(crate) channel: String,
    /// The client needed for `UNLISTEN`.
    pub(crate) inner: Arc<Mutex<tokio_postgres::Client>>,
}

impl NotificationStream {
    /// Wait up to `timeout` for the next notification on this channel.
    ///
    /// Returns `Some(notification)` if a matching notification arrives within
    /// the timeout, or `None` if the timeout elapses.  Notifications for
    /// other channels are silently discarded.
    pub async fn recv_timeout(&mut self, timeout: Duration) -> Option<PgNotification> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match tokio::time::timeout(remaining, self.rx.recv()).await {
                // Timed out.
                Err(_elapsed) => return None,
                // Channel lagged — the oldest notification(s) were dropped.
                // Continue waiting for the next one.
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                // Channel closed — the connection was dropped.
                Ok(Err(broadcast::error::RecvError::Closed)) => return None,
                Ok(Ok(n)) if n.channel == self.channel => return Some(n),
                // Notification for a different channel — skip and keep waiting.
                Ok(Ok(_other)) => continue,
            }
        }
    }

    /// Send `UNLISTEN` for this channel and consume the stream.
    ///
    /// After this call the connection will no longer receive notifications
    /// for this channel (unless another `PgConnection::listen` call
    /// re-registers it).
    pub async fn unlisten(self) -> Result<(), PgError> {
        validate_channel_name(&self.channel)?;
        let client = self.inner.lock().await;
        client
            .batch_execute(&format!("UNLISTEN {}", self.channel))
            .await
            .map_err(|e| PgError::Notify(e.to_string()))
    }
}

// ── Connection driver with notification forwarding ────────────────────────────

/// Spawn the tokio-postgres `Connection` driver and forward
/// `AsyncMessage::Notification` items to a broadcast channel.
///
/// Returns the `broadcast::Sender` that the `PgConnection` should store.
/// The spawned task exits when the connection closes.
pub(crate) fn spawn_connection_driver<S, T>(
    mut connection: tokio_postgres::Connection<S, T>,
) -> broadcast::Sender<PgNotification>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (tx, _rx) = broadcast::channel::<PgNotification>(NOTIFICATION_CAPACITY);
    let tx_clone = tx.clone();

    tokio::spawn(async move {
        loop {
            match futures::future::poll_fn(|cx| connection.poll_message(cx)).await {
                None => break,
                Some(Err(_e)) => break,
                Some(Ok(tokio_postgres::AsyncMessage::Notification(n))) => {
                    // If no receivers are listening, the send is a no-op.
                    let _ = tx_clone.send(PgNotification::from(n));
                }
                Some(Ok(_)) => {
                    // Notices and other async messages are ignored.
                }
            }
        }
    });

    tx
}

// ── Notify (NOTIFY channel, payload) ─────────────────────────────────────────

/// Send a `NOTIFY` to the given channel with an optional payload.
///
/// The channel name must consist only of ASCII alphanumeric characters and
/// underscores.  The payload is single-quote–escaped before inclusion in the
/// SQL command string.
pub(crate) async fn notify(
    inner: &Arc<Mutex<tokio_postgres::Client>>,
    channel: &str,
    payload: &str,
) -> Result<(), PgError> {
    validate_channel_name(channel)?;
    let escaped_payload = payload.replace('\'', "''");
    let sql = format!("NOTIFY {channel}, '{escaped_payload}'");
    let client = inner.lock().await;
    client
        .batch_execute(&sql)
        .await
        .map_err(|e| PgError::Notify(e.to_string()))
}

// ── Listen (LISTEN channel) ───────────────────────────────────────────────────

/// Register `LISTEN` on the given channel and return a [`NotificationStream`].
///
/// Requires that the `PgConnection` was created via [`PgConnection::connect`]
/// (not `from_client`); otherwise returns [`PgError::Notify`].
pub(crate) async fn listen(
    inner: &Arc<Mutex<tokio_postgres::Client>>,
    notif_tx: &Option<broadcast::Sender<PgNotification>>,
    channel: &str,
) -> Result<NotificationStream, PgError> {
    validate_channel_name(channel)?;

    let tx = notif_tx.as_ref().ok_or_else(|| {
        PgError::Notify(
            "LISTEN requires a connection created via PgConnection::connect; \
             connections created via from_client cannot receive notifications"
                .to_string(),
        )
    })?;

    let sql = format!("LISTEN {channel}");
    {
        let client = inner.lock().await;
        client
            .batch_execute(&sql)
            .await
            .map_err(|e| PgError::Notify(e.to_string()))?;
    }

    Ok(NotificationStream {
        rx: tx.subscribe(),
        channel: channel.to_string(),
        inner: Arc::clone(inner),
    })
}

// ── Identifier validation ─────────────────────────────────────────────────────

/// Validate a PostgreSQL channel (or identifier) name.
///
/// Accepts only ASCII alphanumeric characters and underscores to prevent SQL
/// injection.  Returns [`PgError::Notify`] if the name is invalid.
pub(crate) fn validate_channel_name(name: &str) -> Result<(), PgError> {
    if name.is_empty() {
        return Err(PgError::Notify(
            "channel name must not be empty".to_string(),
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(PgError::Notify(format!(
            "invalid channel name {name:?}: only ASCII alphanumeric characters and underscores are allowed"
        )));
    }
    Ok(())
}

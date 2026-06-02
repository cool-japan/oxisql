//! TLS integration for `oxisql-postgres`.
//!
//! Wraps [`tokio_postgres_rustls::MakeRustlsConnect`] to bridge the
//! `Arc<rustls::ClientConfig>` stored in [`TlsMode::Rustls`] with the
//! value-taking constructor.

use std::sync::Arc;

use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres_rustls::MakeRustlsConnect;

/// Thin newtype over [`MakeRustlsConnect`] that implements
/// [`MakeTlsConnect`] for any async read+write stream.
///
/// # Why a newtype?
///
/// `MakeRustlsConnect::new` takes `ClientConfig` by value, but our
/// `TlsMode::Rustls` holds an `Arc<ClientConfig>`.  This wrapper converts
/// via `Arc::unwrap_or_clone`, avoiding an extra heap allocation when the
/// Arc has exactly one strong reference.
pub struct PgTls(MakeRustlsConnect);

impl PgTls {
    /// Create from an `Arc<rustls::ClientConfig>`.
    ///
    /// If this is the only strong reference, the config is moved out of the
    /// Arc; otherwise it is cloned.  Either way, no `unsafe` code is used.
    pub fn new(cfg: Arc<rustls::ClientConfig>) -> Self {
        let owned = Arc::try_unwrap(cfg).unwrap_or_else(|arc| (*arc).clone());
        Self(MakeRustlsConnect::new(owned))
    }
}

impl<S> MakeTlsConnect<S> for PgTls
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    type Stream = <MakeRustlsConnect as MakeTlsConnect<S>>::Stream;
    type TlsConnect = <MakeRustlsConnect as MakeTlsConnect<S>>::TlsConnect;
    type Error = <MakeRustlsConnect as MakeTlsConnect<S>>::Error;

    fn make_tls_connect(&mut self, domain: &str) -> Result<Self::TlsConnect, Self::Error> {
        <MakeRustlsConnect as MakeTlsConnect<S>>::make_tls_connect(&mut self.0, domain)
    }
}

//! Builder types for configuring PostgreSQL connections.
//!
//! This module provides:
//!
//! - [`TlsMode`] — selects between plain-text and TLS-encrypted connections.
//! - [`PgConnectionBuilder`] — fluent builder API for [`crate::PgConnection`].

use std::sync::Arc;

use rustls_pki_types::CertificateDer;

use crate::connection::PgConnection;
use crate::error::PgError;

// ── TLS mode ──────────────────────────────────────────────────────────────────

/// TLS mode to use when connecting to PostgreSQL.
#[derive(Clone, Default)]
#[allow(missing_debug_implementations)]
pub enum TlsMode {
    /// Plain-text connection — no encryption.
    #[default]
    Disabled,
    /// TLS via a pre-built `rustls` `ClientConfig` sourced from OxiTLS.
    ///
    /// Build the config using, e.g.:
    /// ```rust,no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let cfg = oxitls::client_config(oxitls::webpki_root_certs())?;
    /// let tls = oxisql_postgres::TlsMode::Rustls(cfg);
    /// # Ok(())
    /// # }
    /// ```
    Rustls(Arc<rustls::ClientConfig>),
}

impl TlsMode {
    /// Build a `TlsMode::Rustls` that skips server certificate verification.
    ///
    /// # Security Warning
    ///
    /// This mode accepts **any** server certificate, including self-signed and
    /// expired ones.  It is vulnerable to man-in-the-middle attacks.
    /// **Only use this in development or testing environments.**
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Tls`] if the `rustls` `ClientConfig` cannot be built.
    pub fn skip_verify() -> Result<Self, PgError> {
        let provider = oxitls::pure_provider();
        let verifier = Arc::new(NoCertVerifier);
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| PgError::Tls(e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        Ok(TlsMode::Rustls(Arc::new(cfg)))
    }

    /// Build a `TlsMode::Rustls` that trusts a custom CA certificate in PEM
    /// format, in addition to the system/WebPKI root store.
    ///
    /// `ca_pem` must contain one or more PEM-encoded X.509 certificates.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Tls`] if the PEM cannot be parsed or the
    /// `rustls` `ClientConfig` cannot be built.
    pub fn with_ca_pem(ca_pem: Vec<u8>) -> Result<Self, PgError> {
        use std::io::BufReader;

        let mut root_store = rustls::RootCertStore::empty();
        // Trust the Mozilla WebPKI bundle first.
        root_store.extend(oxitls::webpki_root_certs().roots);

        // Parse and add the caller-supplied CA.
        let mut reader = BufReader::new(ca_pem.as_slice());
        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| PgError::Tls(format!("PEM parse error: {e}")))?;
        if certs.is_empty() {
            return Err(PgError::Tls("no certificates found in ca_pem".to_string()));
        }
        for cert in certs {
            root_store
                .add(cert)
                .map_err(|e| PgError::Tls(format!("bad CA cert: {e}")))?;
        }

        let provider = oxitls::pure_provider();
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| PgError::Tls(e.to_string()))?
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Ok(TlsMode::Rustls(Arc::new(cfg)))
    }
}

// ── NoCertVerifier ────────────────────────────────────────────────────────────

/// A certificate verifier that accepts all server certificates without any
/// validation.
///
/// **For development and testing only.** Using this in production makes TLS
/// connections trivially vulnerable to man-in-the-middle attacks.
#[derive(Debug)]
struct NoCertVerifier;

impl rustls::client::danger::ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        oxitls::pure_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ── PgConnectionBuilder ───────────────────────────────────────────────────────

/// Builder for configuring and establishing a PostgreSQL connection.
///
/// # Example
///
/// ```rust,no_run
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use oxisql_postgres::{PgConnectionBuilder, TlsMode};
///
/// let conn = PgConnectionBuilder::new()
///     .host("localhost")
///     .port(5432)
///     .dbname("mydb")
///     .user("postgres")
///     .password("secret")
///     .connect()
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct PgConnectionBuilder {
    host: Option<String>,
    port: Option<u16>,
    dbname: Option<String>,
    user: Option<String>,
    password: Option<String>,
    connect_timeout_secs: Option<u64>,
    tls_mode: TlsMode,
}

impl PgConnectionBuilder {
    /// Create a new builder with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the host.
    pub fn host(mut self, h: impl Into<String>) -> Self {
        self.host = Some(h.into());
        self
    }

    /// Set the port.
    pub fn port(mut self, p: u16) -> Self {
        self.port = Some(p);
        self
    }

    /// Set the database name.
    pub fn dbname(mut self, db: impl Into<String>) -> Self {
        self.dbname = Some(db.into());
        self
    }

    /// Set the username.
    pub fn user(mut self, u: impl Into<String>) -> Self {
        self.user = Some(u.into());
        self
    }

    /// Set the password.
    pub fn password(mut self, pw: impl Into<String>) -> Self {
        self.password = Some(pw.into());
        self
    }

    /// Set the connection timeout in seconds.
    pub fn connect_timeout_secs(mut self, secs: u64) -> Self {
        self.connect_timeout_secs = Some(secs);
        self
    }

    /// Set the TLS mode.
    pub fn tls_mode(mut self, mode: TlsMode) -> Self {
        self.tls_mode = mode;
        self
    }

    /// Require TLS but skip server certificate verification.
    ///
    /// **INSECURE** — intended for development and testing environments only.
    /// Production code should use [`tls_with_ca_pem`][Self::tls_with_ca_pem] or
    /// supply a trusted root certificate store via
    /// [`tls_mode`][Self::tls_mode] with a fully validated
    /// [`TlsMode::Rustls`] config.
    ///
    /// Internally calls [`TlsMode::skip_verify`] to build a `ClientConfig`
    /// that accepts all server certificates without validation.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Tls`] if the `rustls` `ClientConfig` cannot be built.
    pub fn tls_skip_verify(mut self) -> Result<Self, PgError> {
        self.tls_mode = TlsMode::skip_verify()?;
        Ok(self)
    }

    /// Require TLS and verify the server certificate against a custom CA.
    ///
    /// `ca_pem` must be a PEM-encoded certificate authority certificate.
    /// Multiple certificates may be concatenated in a single `Vec<u8>`.
    ///
    /// Internally calls [`TlsMode::with_ca_pem`] to build a `ClientConfig`
    /// that trusts the supplied CA in addition to the Mozilla WebPKI bundle.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Tls`] if the PEM cannot be parsed or is empty.
    pub fn tls_with_ca_pem(mut self, ca_pem: Vec<u8>) -> Result<Self, PgError> {
        self.tls_mode = TlsMode::with_ca_pem(ca_pem)?;
        Ok(self)
    }

    /// Establish the connection.
    pub async fn connect(self) -> Result<PgConnection, PgError> {
        let mut parts = Vec::new();
        if let Some(h) = &self.host {
            parts.push(format!("host={h}"));
        }
        if let Some(p) = self.port {
            parts.push(format!("port={p}"));
        }
        if let Some(db) = &self.dbname {
            parts.push(format!("dbname={db}"));
        }
        if let Some(u) = &self.user {
            parts.push(format!("user={u}"));
        }
        if let Some(pw) = &self.password {
            parts.push(format!("password={pw}"));
        }
        if let Some(t) = self.connect_timeout_secs {
            parts.push(format!("connect_timeout={t}"));
        }
        let conn_str = parts.join(" ");
        PgConnection::connect(&conn_str, self.tls_mode).await
    }
}

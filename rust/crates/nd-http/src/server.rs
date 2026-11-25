//! TCP listener lifecycle for the HTTP shell (BOOT-03 shell scope).
//!
//! Contract implemented here (`subplans/boot-api.md:249,255`):
//! * bind happens **before** anything reports ready, and the bound listener —
//!   not the configured one — is the source of the reported address (port 0
//!   must expose the real port; Go's `server.go:117-123` `time.After(50ms)`
//!   proof is deliberately not reproduced);
//! * shutdown is signalled from outside, the accept task is **joined**, and the
//!   join outcome is reported honestly;
//! * configured TLS or Unix modes are refused before the bind rather than
//!   silently served as insecure TCP.
//!
//! Not claimed: in-flight request drain and its observability (API-04),
//! DB-close ordering, signal handling (`SIGINT`/`SIGTERM` selection belongs to
//! `nd-cli`, BOOT-04), proxy trust, and any protected mount.

use std::io;
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::time::Duration;

use axum::Router;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::peer::record_peer;
use crate::shell::build_shell;
use crate::shutdown::{Shutdown, ShutdownOutcome};
use crate::{BindError, UnsupportedTransport};

/// Recommended graceful deadline, matching Go's
/// `server.Shutdown` timeout (`server/server.go:135` → `3*time.Second`).
pub const DEFAULT_GRACEFUL_DEADLINE: Duration = Duration::from_millis(3_000);

/// A socket that is already bound and therefore safe to report as ready.
#[derive(Debug)]
pub struct BoundListener {
    listener: StdTcpListener,
    local_addr: SocketAddr,
}

impl BoundListener {
    /// Bind a TCP socket from committed configuration.
    ///
    /// Rejected **before** any bind attempt:
    /// * `TLSKey`/`TLSCert` set → [`UnsupportedTransport::TlsConfigured`];
    /// * `Address` prefixed `unix:` → [`UnsupportedTransport::UnixConfigured`];
    /// * `Address`/`Port` that do not form a numeric socket address.
    ///
    /// Only numeric address literals are resolved here; Go's
    /// `net.Listen("tcp", …)` also resolves hostnames, which this shell does
    /// not claim (recorded as a gap).
    ///
    /// `port == 0` asks the OS for an ephemeral port; [`Self::local_addr`] then
    /// reports the real one.
    ///
    /// # Errors
    ///
    /// [`BindError`] describing which precondition failed, or the OS refusal.
    pub fn bind(address: &str, port: i64) -> Result<Self, BindError> {
        if address.starts_with("unix:") {
            return Err(BindError::Unsupported(
                UnsupportedTransport::UnixConfigured {
                    address: address.to_string(),
                },
            ));
        }
        let port_u16 = u16::try_from(port).map_err(|_| BindError::InvalidAddress {
            address: address.to_string(),
            port,
        })?;
        // Numeric literal only: no DNS resolution in this increment.
        let requested: SocketAddr =
            format!("{address}:{port}")
                .parse()
                .map_err(|_| BindError::InvalidAddress {
                    address: address.to_string(),
                    port,
                })?;
        debug_assert_eq!(requested.port(), port_u16);

        let listener = StdTcpListener::bind(requested).map_err(|err| BindError::Io {
            address: requested.to_string(),
            reason: err.to_string(),
        })?;
        let local_addr = listener.local_addr().map_err(|err| BindError::Io {
            address: requested.to_string(),
            reason: err.to_string(),
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|err| BindError::Io {
                address: requested.to_string(),
                reason: err.to_string(),
            })?;
        Ok(Self {
            listener,
            local_addr,
        })
    }

    /// Bind using the configured `Address`/`Port`, refusing TLS/Unix modes.
    ///
    /// # Errors
    ///
    /// See [`Self::bind`].
    pub fn bind_from_config(config: &nd_conf::Config) -> Result<Self, BindError> {
        if !config.tls_cert.is_empty() || !config.tls_key.is_empty() {
            return Err(BindError::Unsupported(
                UnsupportedTransport::TlsConfigured {
                    cert: config.tls_cert.clone(),
                    key: config.tls_key.clone(),
                },
            ));
        }
        Self::bind(&config.address, config.port)
    }

    /// The address the OS actually bound. Safe to report as ready: the socket is
    /// listening, so this is not a promise about a future bind.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Hand the bound socket to the runtime for serving.
    ///
    /// # Errors
    ///
    /// [`BindError::Io`] if the std listener cannot be registered with tokio.
    fn into_tokio(self) -> Result<TcpListener, BindError> {
        let local_addr = self.local_addr;
        TcpListener::from_std(self.listener).map_err(|err| BindError::Io {
            address: local_addr.to_string(),
            reason: err.to_string(),
        })
    }
}

/// A running accept loop plus the handle that must be joined before shutdown is
/// reported as successful.
#[derive(Debug)]
pub struct Running {
    local_addr: SocketAddr,
    shutdown: Shutdown,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl Running {
    /// Serve `router` over a bound listener and start accepting immediately.
    ///
    /// The router gains [`record_peer`] so `nd-auth` reads the transport peer
    /// from `ConnectInfo`; the router itself is otherwise untouched, which is
    /// what keeps `nd-http` free of an `nd-api` edge (`decisions.md:521`, D1).
    ///
    /// # Errors
    ///
    /// [`BindError::Io`] if the listener cannot be registered with the runtime.
    pub fn start(
        listener: BoundListener,
        router: Router,
        shutdown: Shutdown,
        base_path: &str,
    ) -> Result<Self, BindError> {
        let local_addr = listener.local_addr();
        let tokio_listener = listener.into_tokio()?;
        let app = build_shell(base_path, router).layer(axum::middleware::from_fn(record_peer));
        let signal = shutdown.clone();
        let task = tokio::spawn(async move {
            // The shutdown future must be `'static`, so it owns its own clone of
            // the signal rather than borrowing one.
            axum::serve(
                tokio_listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move { signal.wait().await })
            .await
        });
        Ok(Self {
            local_addr,
            shutdown,
            task: Some(task),
        })
    }

    /// The address this server is accepting on. Only reachable after a real
    /// bind, so "ready" is never claimed before the socket exists.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The shutdown signal driving this accept loop.
    #[must_use]
    pub fn shutdown(&self) -> &Shutdown {
        &self.shutdown
    }

    /// Fire the shutdown signal, then join the accept task within `deadline`.
    ///
    /// Only an observed successful join returns [`ShutdownOutcome::Drained`].
    /// On timeout the task is detached; its completion is not established.
    /// A panicked or cancelled task returns [`ShutdownOutcome::NotJoined`];
    /// a serve I/O error returns [`ShutdownOutcome::ServeFailed`].
    pub async fn stop_and_join(self, deadline: Duration) -> ShutdownOutcome {
        self.shutdown.signal();
        let task = match self.task {
            Some(task) => task,
            None => return ShutdownOutcome::NotJoined,
        };
        let joined = tokio::time::timeout(deadline, task).await;
        match joined {
            Err(_elapsed) => ShutdownOutcome::DeadlineElapsed {
                deadline_ms: deadline.as_millis().try_into().unwrap_or(u64::MAX),
            },
            Ok(Err(_)) => ShutdownOutcome::NotJoined,
            Ok(Ok(Err(io_err))) => ShutdownOutcome::ServeFailed {
                reason: io_err.to_string(),
            },
            Ok(Ok(Ok(()))) => ShutdownOutcome::Drained,
        }
    }
}

impl Running {
    /// Convenience: serve from configuration with the recommended deadline
    /// available as [`DEFAULT_GRACEFUL_DEADLINE`].
    ///
    /// # Errors
    ///
    /// [`BindError`] from the bind or runtime registration.
    pub fn serve_from_config(
        config: &nd_conf::Config,
        router: Router,
        shutdown: Shutdown,
    ) -> Result<Self, BindError> {
        let listener = BoundListener::bind_from_config(config)?;
        Self::start(listener, router, shutdown, &config.base_path)
    }
}

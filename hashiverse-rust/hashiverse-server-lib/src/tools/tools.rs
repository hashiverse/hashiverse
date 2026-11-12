//! # Server-only helpers
//!
//! A small grab bag of utilities the server needs that don't belong in
//! `hashiverse-lib`:
//!
//! - [`get_public_ipv4`] — asks ipify.org for the node's externally-visible IPv4 so
//!   the node's advertised peer address matches reality (behind NAT / firewalls).
//!   Falls back to loopback when explicitly running in a local-only test network.
//! - [`is_ssrf_protected_ip`] — the deny-list for every server-initiated outbound
//!   fetch (URL previews, Let's Encrypt challenges, ipify). Rejects loopback,
//!   RFC-1918 private ranges, link-local, CGNAT, cloud-metadata endpoints
//!   (169.254.169.254 friends), multicast, broadcast, the unspecified address,
//!   IPv6 ULA/link-local, and IPv4-mapped IPv6. This is the primary defence against
//!   SSRF — a malicious URL in a post must not let a client coerce our server into
//!   probing its own internal network.
//! - [`spawn_shutdown_signal_handler`] — wires SIGINT (Ctrl+C) and SIGTERM (or the
//!   Windows console equivalents) into a `CancellationToken` so the server can shut
//!   down gracefully; a second signal exits immediately.

use log::warn;
use tokio_util::sync::CancellationToken;

pub async fn get_public_ipv4(force_local_network: bool) -> anyhow::Result<String> {
    match force_local_network {
        true => Ok("127.0.0.1".to_string()),
        false => {
            let client = reqwest::ClientBuilder::new().danger_accept_invalid_certs(true).build()?; // we are willing to accept invalid certs as we do application level signature checking
            let response = client.get("https://api4.ipify.org").send().await?.text().await?;
            Ok(response)
        }
    }
}

/// Cancels `cancellation_token` on the first shutdown request, and exits the process on a second one
/// (for when graceful shutdown is stuck, e.g. during the identity PoW, which doesn't watch the token).
///
/// Unix: SIGINT (Ctrl+C) and SIGTERM (systemd, Docker, `kill`). Windows: Ctrl+C, Ctrl+Break, console close
/// and system shutdown.
///
/// The OS handlers are registered before this returns, so a signal arriving straight after the call is
/// never lost, and a SIGTERM never takes its default action of killing the process outright.
pub fn spawn_shutdown_signal_handler(cancellation_token: CancellationToken) -> anyhow::Result<()> {
    spawn_shutdown_signal_handler_with_force_exit(cancellation_token, || std::process::exit(130))
}

fn spawn_shutdown_signal_handler_with_force_exit(cancellation_token: CancellationToken, force_exit: impl FnOnce() + Send + 'static) -> anyhow::Result<()> {
    let mut shutdown_signals = ShutdownSignals::register()?;
    tokio::spawn(async move {
        let signal_name = shutdown_signals.recv().await;
        warn!("{} received, shutting down gracefully (send it again to exit immediately)...", signal_name);
        cancellation_token.cancel();

        let signal_name = shutdown_signals.recv().await;
        warn!("{} received again, exiting immediately", signal_name);
        force_exit();
    });
    Ok(())
}

#[cfg(unix)]
struct ShutdownSignals {
    sigint: tokio::signal::unix::Signal,
    sigterm: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl ShutdownSignals {
    fn register() -> anyhow::Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(Self { sigint: signal(SignalKind::interrupt())?, sigterm: signal(SignalKind::terminate())? })
    }

    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.sigint.recv() => "SIGINT",
            _ = self.sigterm.recv() => "SIGTERM",
        }
    }
}

#[cfg(windows)]
struct ShutdownSignals {
    ctrl_c: tokio::signal::windows::CtrlC,
    ctrl_break: tokio::signal::windows::CtrlBreak,
    ctrl_close: tokio::signal::windows::CtrlClose,
    ctrl_shutdown: tokio::signal::windows::CtrlShutdown,
}

#[cfg(windows)]
impl ShutdownSignals {
    fn register() -> anyhow::Result<Self> {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_shutdown};
        Ok(Self { ctrl_c: ctrl_c()?, ctrl_break: ctrl_break()?, ctrl_close: ctrl_close()?, ctrl_shutdown: ctrl_shutdown()? })
    }

    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.ctrl_c.recv() => "Ctrl+C",
            _ = self.ctrl_break.recv() => "Ctrl+Break",
            _ = self.ctrl_close.recv() => "Console close",
            _ = self.ctrl_shutdown.recv() => "System shutdown",
        }
    }
}

/// Returns true for any IP that must never be the target of a server-initiated fetch.
/// Covers: loopback, RFC-1918 private, link-local (incl. cloud metadata 169.254.169.254),
/// CGNAT (100.64.0.0/10), multicast, unspecified, broadcast, IPv6 ULA (fc00::/7),
/// IPv6 link-local (fe80::/10), and IPv4-mapped IPv6 addresses whose embedded v4 is protected.
pub fn is_ssrf_protected_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()                               // 127.0.0.0/8
                || v4.is_private()                         // 10/8, 172.16/12, 192.168/16
                || v4.is_link_local()                      // 169.254.0.0/16 — cloud metadata
                || v4.is_multicast()                       // 224.0.0.0/4
                || v4.is_unspecified()                     // 0.0.0.0
                || v4.is_broadcast()                       // 255.255.255.255
                || (o[0] == 100 && (o[1] & 0xC0) == 64)   // CGNAT 100.64.0.0/10
        }
        std::net::IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback()                               // ::1
                || v6.is_multicast()                       // ff00::/8
                || v6.is_unspecified()                     // ::
                || (s[0] & 0xFFC0) == 0xFE80              // link-local fe80::/10
                || (s[0] & 0xFE00) == 0xFC00              // unique local fc00::/7
                // IPv4-mapped (::ffff:0:0/96) — recurse on the embedded v4 address
                || matches!(v6.to_ipv4_mapped(), Some(v4) if is_ssrf_protected_ip(std::net::IpAddr::V4(v4)))
        }
    }
}

// Unix only: these signal this test process itself, which is safe under nextest (one process per test).
#[cfg(all(test, unix))]
mod tests {
    use crate::tools::tools::spawn_shutdown_signal_handler_with_force_exit;
    use std::time::Duration;
    use tokio::sync::oneshot;
    use tokio_util::sync::CancellationToken;

    fn send_signal_to_self(signal_flag: &str) {
        let status = std::process::Command::new("kill").args([signal_flag, &std::process::id().to_string()]).status().expect("failed to run kill");
        assert!(status.success(), "kill {} failed", signal_flag);
    }

    fn spawn_handler_with_recorded_force_exit(cancellation_token: &CancellationToken) -> oneshot::Receiver<()> {
        let (force_exit_sender, force_exit_receiver) = oneshot::channel::<()>();
        spawn_shutdown_signal_handler_with_force_exit(cancellation_token.clone(), move || {
            let _ = force_exit_sender.send(());
        })
        .expect("failed to register signal handlers");
        force_exit_receiver
    }

    async fn assert_signal_cancels_token_without_force_exit(signal_flag: &str) {
        let cancellation_token = CancellationToken::new();
        let mut force_exit_receiver = spawn_handler_with_recorded_force_exit(&cancellation_token);

        send_signal_to_self(signal_flag);

        tokio::time::timeout(Duration::from_secs(5), cancellation_token.cancelled()).await.unwrap_or_else(|_| panic!("{} did not cancel the token", signal_flag));
        assert!(tokio::time::timeout(Duration::from_millis(200), &mut force_exit_receiver).await.is_err(), "a single {} forced an exit", signal_flag);
    }

    #[tokio::test]
    async fn sigterm_cancels_token() {
        assert_signal_cancels_token_without_force_exit("-TERM").await;
    }

    #[tokio::test]
    async fn sigint_cancels_token() {
        assert_signal_cancels_token_without_force_exit("-INT").await;
    }

    #[tokio::test]
    async fn second_signal_forces_exit() {
        let cancellation_token = CancellationToken::new();
        let force_exit_receiver = spawn_handler_with_recorded_force_exit(&cancellation_token);

        send_signal_to_self("-TERM");
        tokio::time::timeout(Duration::from_secs(5), cancellation_token.cancelled()).await.expect("SIGTERM did not cancel the token");

        send_signal_to_self("-INT");
        tokio::time::timeout(Duration::from_secs(5), force_exit_receiver).await.expect("second signal did not force an exit").expect("force_exit sender dropped");
    }
}

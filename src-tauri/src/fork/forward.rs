//! A client's road to its library host. A release window's page is
//! `tauri://localhost`, which WebKit counts as secure, so it blocks `http://`
//! and `ws://` to another machine as mixed content and allows them to
//! `127.0.0.1`. Each host gets a port on 127.0.0.1 that pipes bytes to it
//! unchanged, so the token, range requests and WebSocket upgrades pass as sent.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, TcpListener as StdTcpListener};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};

/// A host behind a firewall that drops packets never refuses, and the
/// system's own connect timeout is over a minute. The client shows the host
/// unreachable after one of these, so it stays short.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Why a connection to the host failed, as the client's notice names it.
#[derive(Debug, Serialize)]
pub struct Unreachable {
    /// `refused`, `timedOut` or `other`.
    pub reason: &'static str,
    pub message: String,
}

/// Host `name:port` to the local port that reaches it.
#[derive(Default)]
pub struct Forwards(Mutex<HashMap<String, u16>>);

/// Only a `MagicDNS` name, which the CSP allowed before this existed, so a
/// script in the page cannot use the forward to reach anything else.
fn tailnet_name(host: &str) -> bool {
    host.len() > ".ts.net".len()
        && host.ends_with(".ts.net")
        && host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

async fn connect(target: &str) -> Result<TcpStream, Unreachable> {
    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(target)).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(e)) => {
            let reason = match e.kind() {
                ErrorKind::ConnectionRefused => "refused",
                ErrorKind::TimedOut => "timedOut",
                _ => "other",
            };
            Err(Unreachable { reason, message: e.to_string() })
        }
        Err(_) => Err(Unreachable { reason: "timedOut", message: format!("No answer in {} seconds.", CONNECT_TIMEOUT.as_secs()) }),
    }
}

/// Opens and closes one connection to `host:port`, for the client's check.
pub async fn reach(host: &str, port: u16) -> Result<(), Unreachable> {
    if !tailnet_name(host) {
        return Err(Unreachable { reason: "other", message: format!("{host} is not a Tailscale name.") });
    }
    connect(&format!("{host}:{port}")).await.map(drop)
}

/// The local port for `host:port`, listening from the first call on.
pub fn local_port(forwards: &Forwards, host: &str, port: u16) -> Result<u16, String> {
    if !tailnet_name(host) {
        return Err(format!("{host} is not a Tailscale name. Enter the host's address as Preferences › Server shows it there."));
    }
    let target = format!("{host}:{port}");
    let mut held = forwards.0.lock().map_err(|e| e.to_string())?;
    if let Some(&local) = held.get(&target) {
        return Ok(local);
    }
    let listener = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let local = listener.local_addr().map_err(|e| e.to_string())?.port();
    held.insert(target.clone(), local);
    tracing::info!(local, %target, "forwarding to the library host");
    tauri::async_runtime::spawn(serve(listener, target));
    Ok(local)
}

async fn serve(listener: StdTcpListener, target: String) {
    let listener = match TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(e) => {
            tracing::warn!(error = %e, "could not forward to the library host");
            return;
        }
    };
    loop {
        let mut page = match listener.accept().await {
            Ok((page, _)) => page,
            Err(e) => {
                tracing::warn!(error = %e, "forward accept failed");
                continue;
            }
        };
        let target = target.clone();
        tauri::async_runtime::spawn(async move {
            match connect(&target).await {
                Ok(mut host) => {
                    let _ = tokio::io::copy_bidirectional(&mut page, &mut host).await;
                }
                Err(e) => tracing::warn!(reason = e.reason, error = %e.message, %target, "could not reach the library host"),
            }
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn forwards_only_to_a_tailnet_name() {
        assert!(tailnet_name("milads-mac-mini.taild31e9a.ts.net"));
        for host in ["example.com", "127.0.0.1", "100.82.222.93", ".ts.net", "evil.ts.net.example.com", "a b.ts.net"] {
            assert!(!tailnet_name(host), "{host}");
            assert!(local_port(&Forwards::default(), host, 80).is_err(), "{host}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_closed_port_reads_as_refused() {
        let port = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
        let failed = connect(&format!("127.0.0.1:{port}")).await.unwrap_err();
        assert_eq!(failed.reason, "refused", "{}", failed.message);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_that_drops_packets_times_out_in_seconds() {
        // TEST-NET-1 is never routed, so the connect hangs as it does behind a dropping firewall.
        let started = std::time::Instant::now();
        let failed = connect("192.0.2.1:48650").await.unwrap_err();
        assert!(matches!(failed.reason, "timedOut" | "other"), "{failed:?}");
        assert!(started.elapsed() < CONNECT_TIMEOUT + Duration::from_secs(1));
    }
}

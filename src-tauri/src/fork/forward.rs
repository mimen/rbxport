//! A client's road to its library host. A release window's page is
//! `tauri://localhost`, which WebKit counts as secure, so it blocks `http://`
//! and `ws://` to another machine as mixed content and allows them to
//! `127.0.0.1`. Each host gets a port on 127.0.0.1 that pipes bytes to it
//! unchanged, so the token, range requests and WebSocket upgrades pass as sent.

use std::collections::HashMap;
use std::net::{Ipv4Addr, TcpListener as StdTcpListener};
use std::sync::Mutex;

use tokio::net::{TcpListener, TcpStream};

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
            match TcpStream::connect(&target).await {
                Ok(mut host) => {
                    let _ = tokio::io::copy_bidirectional(&mut page, &mut host).await;
                }
                Err(e) => tracing::warn!(error = %e, %target, "could not reach the library host"),
            }
        });
    }
}

#[cfg(test)]
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
}

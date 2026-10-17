//! Server mode: one machine serves its library over Tailscale and the fork
//! app on other machines browses and edits it. See FORK.md.
//!
//! Upstream reaches this through one plugin, registered in `run`. The plugin
//! starts the server when Serve library is on, and answers the Preferences
//! pane and a client's forward to its host on the `rbxfork://` scheme.

pub mod commands;
pub mod forward;
pub mod server;
pub mod settings;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};
use tauri::http::{Request, Response, StatusCode};
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Runtime};

/// What the Preferences pane shows.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub serve: bool,
    pub port: u16,
    pub token: String,
    pub version: String,
    /// What a client enters: `http://<Tailscale name>:<port>`.
    pub address: Option<String>,
    /// `ip:port` for each address that is listening.
    pub listening: Vec<String>,
    pub problems: Vec<String>,
}

#[derive(Default)]
struct Running {
    stop: Option<tokio::sync::watch::Sender<()>>,
    status: Status,
}

/// One at a time: a toggle waits for the previous one's sockets.
#[derive(Default)]
pub struct ForkServer(tokio::sync::Mutex<Running>);

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    tauri::plugin::Builder::new("rbxfork")
        .setup(|app, _api| {
            app.manage(ForkServer::default());
            app.manage(forward::Forwards::default());
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                apply(&app).await;
            });
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("rbxfork", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                let response = if request.uri().path() == "/forward" {
                    forward_to(&app, &request)
                } else {
                    preferences(&app, request).await
                };
                responder.respond(response);
            });
        })
        .build()
}

/// Tailscale gives each machine one address in 100.64.0.0/10.
fn tailscale_ipv4() -> Option<Ipv4Addr> {
    if_addrs::get_if_addrs().ok()?.into_iter().find_map(|interface| match interface.ip() {
        IpAddr::V4(ip) if ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1]) => Some(ip),
        _ => None,
    })
}

/// This machine's `MagicDNS` name, which the client's CSP allows (`*.ts.net`).
fn tailscale_name() -> Option<String> {
    ["tailscale", "/opt/homebrew/bin/tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"].iter().find_map(|cli| {
        let out = std::process::Command::new(cli).args(["status", "--self", "--peers=false", "--json"]).output().ok()?;
        let status: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let name = status.pointer("/Self/DNSName")?.as_str()?.trim_end_matches('.');
        (!name.is_empty()).then(|| name.to_owned())
    })
}

/// Stops whatever is running and starts what the settings say. Idempotent.
pub async fn apply<R: Runtime>(app: &AppHandle<R>) -> Status {
    let held = app.state::<ForkServer>();
    let mut running = held.0.lock().await;
    running.stop = None;
    let saved = settings::load();
    let mut status = Status {
        serve: saved.serve,
        port: saved.port,
        token: saved.token.clone(),
        version: app.package_info().version.to_string(),
        ..Status::default()
    };
    if saved.serve {
        let (stop, stopped) = tokio::sync::watch::channel(());
        let ctx = server::Ctx {
            app: app.clone(),
            token: saved.token.into(),
            version: status.version.clone().into(),
            stop: stopped.clone(),
        };
        let mut addresses = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
        match tailscale_ipv4() {
            Some(ip) => {
                addresses.push(IpAddr::V4(ip));
                status.address = tailscale_name().map(|name| format!("http://{name}:{}", saved.port));
            }
            None => status.problems.push("No Tailscale address on this machine, so only this machine can connect.".into()),
        }
        for ip in addresses {
            let address = SocketAddr::new(ip, saved.port);
            match tokio::net::TcpListener::bind(address).await {
                Ok(listener) => {
                    status.listening.push(address.to_string());
                    let service = server::router(ctx.clone()).into_make_service_with_connect_info::<SocketAddr>();
                    let mut stopped = stopped.clone();
                    tauri::async_runtime::spawn(async move {
                        let served = axum::serve(listener, service)
                            .with_graceful_shutdown(async move {
                                let _ = stopped.changed().await;
                            })
                            .await;
                        if let Err(e) = served {
                            tracing::warn!(error = %e, %address, "library server stopped");
                        }
                    });
                }
                Err(e) => status.problems.push(format!("Could not listen on {address}: {e}")),
            }
        }
        tracing::info!(listening = ?status.listening, problems = ?status.problems, "library server");
        running.stop = Some(stop);
    }
    running.status = status.clone();
    status
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Change {
    serve: Option<bool>,
    new_token: bool,
}

/// `rbxfork://localhost/server`: GET the status, POST a `Change`.
async fn preferences<R: Runtime>(app: &AppHandle<R>, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let status = if request.method() == tauri::http::Method::POST {
        let change: Change = serde_json::from_slice(request.body()).unwrap_or_default();
        let mut saved = settings::load();
        if let Some(serve) = change.serve {
            saved.serve = serve;
        }
        if change.new_token {
            saved.token = settings::new_token();
        }
        if let Err(e) = settings::save(&saved) {
            tracing::warn!(error = %e, "could not save the server settings");
        }
        apply(app).await
    } else {
        app.state::<ForkServer>().0.lock().await.status.clone()
    };
    json(StatusCode::OK, &status)
}

#[derive(Deserialize)]
struct Target {
    host: String,
    port: u16,
}

/// `rbxfork://localhost/forward`: POST a `Target`, get the local port that reaches it.
fn forward_to<R: Runtime>(app: &AppHandle<R>, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let answer = serde_json::from_slice::<Target>(request.body())
        .map_err(|e| e.to_string())
        .and_then(|target| forward::local_port(&app.state::<forward::Forwards>(), &target.host, target.port));
    match answer {
        Ok(port) => json(StatusCode::OK, &serde_json::json!({ "port": port })),
        Err(message) => json(StatusCode::BAD_REQUEST, &serde_json::json!({ "kind": "remote", "message": message })),
    }
}

fn json(status: StatusCode, body: &impl Serialize) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_vec(body).unwrap_or_default())
        .unwrap_or_default()
}

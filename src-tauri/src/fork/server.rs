//! The HTTP side of server mode: a client app's `invoke`, its event
//! subscriptions, and the artwork and audio its `<img>` and `<audio>` load.
//!
//! A command is not re-implemented here. The request becomes the same
//! `InvokeRequest` the webview's IPC would have sent, delivered to the main
//! webview, so it runs through upstream's `generate_handler!` with the
//! argument parsing, state and ACL it always has.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponse, InvokeResponseBody};
use tauri::webview::InvokeRequest;
use tauri::{AppHandle, Listener, Manager, Runtime};
use tower::ServiceExt as _;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeFile;

use crate::state::AppState;

/// The header a client names its build in; the WebSocket, which cannot set
/// headers, sends it as `?v=`.
pub const VERSION_HEADER: &str = "x-rbxport-version";

pub struct Ctx<R: Runtime> {
    pub app: AppHandle<R>,
    pub token: Arc<str>,
    pub version: Arc<str>,
    /// Changes when the server stops, so open `WebSocket`s close with it.
    pub stop: tokio::sync::watch::Receiver<()>,
}

// By hand: a derive would ask for `R: Clone`, which no runtime is.
impl<R: Runtime> Clone for Ctx<R> {
    fn clone(&self) -> Self {
        Self { app: self.app.clone(), token: Arc::clone(&self.token), version: Arc::clone(&self.version), stop: self.stop.clone() }
    }
}

pub fn router<R: Runtime>(ctx: Ctx<R>) -> Router {
    let versioned = Router::new()
        .route("/invoke/{command}", post(invoke::<R>))
        .route("/events", get(events::<R>))
        .route_layer(middleware::from_fn_with_state(ctx.clone(), same_version::<R>));
    Router::new()
        .merge(versioned)
        .route("/version", get(version::<R>))
        .route("/artwork/{track}", get(artwork::<R>))
        .route("/audio/{track}", get(audio::<R>))
        .route_layer(middleware::from_fn_with_state(ctx.clone(), authorized::<R>))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(ctx)
}

/// A refusal in the shape of upstream's `AppErrorDto`, which the client's
/// `IpcError` already reads.
fn refuse(status: StatusCode, kind: &str, message: impl Into<String>) -> Response {
    (status, Json(json!({ "kind": kind, "message": message.into() }))).into_response()
}

fn query_value(uri: &Uri, key: &str) -> Option<String> {
    Query::<HashMap<String, String>>::try_from_uri(uri).ok()?.0.remove(key)
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(str::to_owned)
}

fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Tools on this machine (curl, the host's own scripts) need no token. A
/// page in a browser here does: it always sends `Origin`, and without this
/// rule any website could drive the library through 127.0.0.1.
fn trusted_local(peer: SocketAddr, headers: &HeaderMap) -> bool {
    peer.ip().is_loopback() && !headers.contains_key(header::ORIGIN)
}

async fn authorized<R: Runtime>(
    State(ctx): State<Ctx<R>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let presented = bearer(request.headers()).or_else(|| query_value(request.uri(), "token"));
    let allowed = match presented {
        Some(token) => same_secret(token.as_bytes(), ctx.token.as_bytes()),
        None => trusted_local(peer, request.headers()),
    };
    if allowed {
        next.run(request).await
    } else {
        refuse(StatusCode::UNAUTHORIZED, "unauthorized", "The host refused the token. Copy it again from the host's Preferences › Server.")
    }
}

async fn same_version<R: Runtime>(
    State(ctx): State<Ctx<R>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let theirs = request
        .headers()
        .get(VERSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| query_value(request.uri(), "v"));
    match theirs {
        Some(v) if *v == *ctx.version => next.run(request).await,
        None if trusted_local(peer, request.headers()) => next.run(request).await,
        theirs => refuse(
            StatusCode::CONFLICT,
            "versionMismatch",
            format!(
                "The host runs rbxport {}, and this app is {}. Install the same version on both machines.",
                ctx.version,
                theirs.as_deref().unwrap_or("an unknown version"),
            ),
        ),
    }
}

async fn version<R: Runtime>(State(ctx): State<Ctx<R>>) -> Response {
    Json(json!({ "version": &*ctx.version })).into_response()
}

async fn invoke<R: Runtime>(State(ctx): State<Ctx<R>>, Path(command): Path<String>, body: Bytes) -> Response {
    if !super::commands::exists(&command) {
        return refuse(StatusCode::NOT_FOUND, "notFound", format!("The host has no command {command}."));
    }
    let args = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(args) => args,
            Err(e) => return refuse(StatusCode::BAD_REQUEST, "badRequest", e.to_string()),
        }
    };
    match dispatch(&ctx.app, command, args).await {
        Ok(InvokeResponseBody::Json(json)) => ([(header::CONTENT_TYPE, "application/json")], json).into_response(),
        Ok(InvokeResponseBody::Raw(bytes)) => ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        // The command's own rejection: 422 keeps it apart from the transport's.
        Err(error) => (StatusCode::UNPROCESSABLE_ENTITY, Json(error)).into_response(),
    }
}

/// Runs one command as the main webview would have, and waits for its answer.
pub async fn dispatch<R: Runtime>(app: &AppHandle<R>, command: String, args: Value) -> Result<InvokeResponseBody, Value> {
    let internal = |message: &str| json!({ "kind": "internal", "message": message });
    let Some(window) = app.get_webview_window(crate::MAIN_WINDOW) else {
        return Err(internal("The host has no main window."));
    };
    let webview: tauri::Webview<R> = window.as_ref().clone();
    let url = webview.url().map_err(|e| internal(&e.to_string()))?;
    let request = InvokeRequest {
        cmd: command,
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url,
        body: InvokeBody::Json(args),
        headers: HeaderMap::new(),
        invoke_key: app.invoke_key().to_owned(),
    };
    let (answer, answered) = tokio::sync::oneshot::channel();
    // A synchronous command runs on the calling thread, so not a runtime worker.
    tauri::async_runtime::spawn_blocking(move || {
        webview.on_message(
            request,
            Box::new(move |_, _, response, _, _| {
                let _ = answer.send(response);
            }),
        );
    });
    match answered.await {
        Ok(InvokeResponse::Ok(body)) => Ok(body),
        Ok(InvokeResponse::Err(error)) => Err(error.0),
        Err(_) => Err(internal("The command ended without answering.")),
    }
}

async fn events<R: Runtime>(State(ctx): State<Ctx<R>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| forward(ctx, socket))
}

/// The client names the events it wants, `{"listen":"library:changed"}` and
/// `{"unlisten":…}`; each arrives as `{"event":…,"payload":…}`.
async fn forward<R: Runtime>(mut ctx: Ctx<R>, mut socket: WebSocket) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut listening: HashMap<String, tauri::EventId> = HashMap::new();
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break };
                let Message::Text(text) = message else { continue };
                let Ok(request) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                if let Some(name) = request.get("listen").and_then(Value::as_str) {
                    if listening.contains_key(name) { continue; }
                    let tx = tx.clone();
                    let event = serde_json::to_string(name).unwrap_or_default();
                    let id = ctx.app.listen_any(name.to_owned(), move |e| {
                        let payload = if e.payload().is_empty() { "null" } else { e.payload() };
                        let _ = tx.send(format!(r#"{{"event":{event},"payload":{payload}}}"#));
                    });
                    listening.insert(name.to_owned(), id);
                } else if let Some(name) = request.get("unlisten").and_then(Value::as_str) {
                    if let Some(id) = listening.remove(name) { ctx.app.unlisten(id); }
                }
            }
            Some(text) = rx.recv() => {
                if socket.send(Message::Text(text.into())).await.is_err() { break; }
            }
            _ = ctx.stop.changed() => break,
        }
    }
    for id in listening.into_values() {
        ctx.app.unlisten(id);
    }
}

/// Library ids are digits. A stick's `file:` ids name paths on the host, so
/// they are not served.
fn library_track(track: &str) -> bool {
    !track.is_empty() && track.bytes().all(|b| b.is_ascii_digit())
}

async fn artwork<R: Runtime>(State(ctx): State<Ctx<R>>, Path(track): Path<String>) -> Response {
    if !library_track(&track) {
        return refuse(StatusCode::BAD_REQUEST, "badRequest", "Not a library track id.");
    }
    let state = Arc::clone(ctx.app.state::<Arc<AppState>>().inner());
    let served = tauri::async_runtime::spawn_blocking(move || {
        let request = tauri::http::Request::builder().uri(format!("rbl://localhost/artwork/{track}")).body(Vec::new()).ok()?;
        Some(crate::protocol::handle(&state, &request))
    })
    .await;
    let Ok(Some(mut response)) = served else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    // The CORS layer adds its own.
    response.headers_mut().remove(header::ACCESS_CONTROL_ALLOW_ORIGIN);
    response.map(Body::from)
}

async fn audio<R: Runtime>(State(ctx): State<Ctx<R>>, Path(track): Path<String>, request: Request) -> Response {
    if !library_track(&track) {
        return refuse(StatusCode::BAD_REQUEST, "badRequest", "Not a library track id.");
    }
    let path = match ctx.app.state::<Arc<AppState>>().library() {
        Ok(library) => library.audio_path_of(&track).map(PathBuf::from),
        Err(_) => return refuse(StatusCode::SERVICE_UNAVAILABLE, "notReady", "The host's library is not open."),
    };
    let Some(path) = path else {
        return refuse(StatusCode::NOT_FOUND, "notFound", "No such track.");
    };
    match ServeFile::new(path).oneshot(request).await {
        Ok(response) => response.map(Body::new),
        Err(never) => match never {},
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;
    use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};

    const TOKEN: &str = "secret-token";
    const VERSION: &str = "1.2.3-fork.test";
    const LOOPBACK: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50_000);
    const TAILNET: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 88, 209, 66)), 50_000);

    /// An app whose only command handler echoes the name and arguments it
    /// was given, so every name in the table can be sent without running it.
    fn echo_app() -> (tauri::App<MockRuntime>, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let app = mock_builder()
            .invoke_handler(move |invoke| {
                let command = invoke.message.command().to_owned();
                if let Ok(mut seen) = record.lock() {
                    seen.push(command.clone());
                }
                let args = invoke.message.payload().clone();
                let args = match args { InvokeBody::Json(v) => v, InvokeBody::Raw(_) => Value::Null };
                invoke.resolver.resolve(json!({ "command": command, "args": args }));
                true
            })
            .build(mock_context(noop_assets()))
            .unwrap();
        tauri::WebviewWindowBuilder::new(&app, crate::MAIN_WINDOW, tauri::WebviewUrl::default()).build().unwrap();
        (app, seen)
    }

    fn ctx(app: &tauri::App<MockRuntime>) -> Ctx<MockRuntime> {
        let (_stop, stop) = tokio::sync::watch::channel(());
        Ctx { app: app.handle().clone(), token: TOKEN.into(), version: VERSION.into(), stop }
    }

    fn post(path: &str, peer: SocketAddr, headers: &[(&str, &str)]) -> Request {
        let mut builder = Request::builder().method("POST").uri(path);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let mut request = builder.body(Body::from(r#"{"probe":1}"#)).unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    }

    async fn send(router: &Router, request: Request) -> (StatusCode, Value) {
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn client() -> [(&'static str, &'static str); 2] {
        [("authorization", "Bearer secret-token"), (VERSION_HEADER, VERSION)]
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_generated_command_reaches_the_handler() {
        let (app, seen) = echo_app();
        let router = router(ctx(&app));
        for name in super::super::commands::names() {
            let (status, body) = send(&router, post(&format!("/invoke/{name}"), TAILNET, &client())).await;
            assert_eq!(status, StatusCode::OK, "{name}: {body}");
            assert_eq!(body, json!({ "command": name, "args": { "probe": 1 } }), "{name}");
        }
        assert_eq!(seen.lock().unwrap().len(), super::super::commands::names().len());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_name_outside_the_table_is_not_dispatched() {
        let (app, seen) = echo_app();
        let (status, body) = send(&router(ctx(&app)), post("/invoke/no_such_command", TAILNET, &client())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["kind"], "notFound");
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_or_wrong_token_is_refused() {
        let (app, seen) = echo_app();
        let router = router(ctx(&app));
        let version = (VERSION_HEADER, VERSION);
        let cases = [
            ("no token from the tailnet", TAILNET, vec![version]),
            ("wrong token", TAILNET, vec![("authorization", "Bearer secret-tokeX"), version]),
            ("wrong token on loopback", LOOPBACK, vec![("authorization", "Bearer nope"), version]),
            ("a web page on loopback", LOOPBACK, vec![("origin", "https://example.com"), version]),
        ];
        for (case, peer, headers) in cases {
            let (status, body) = send(&router, post("/invoke/playlist_tree", peer, &headers)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{case}");
            assert_eq!(body["kind"], "unauthorized", "{case}");
        }
        let mut get = Request::builder().uri("/version").body(Body::empty()).unwrap();
        get.extensions_mut().insert(ConnectInfo(TAILNET));
        assert_eq!(send(&router, get).await.0, StatusCode::UNAUTHORIZED, "version without a token");
        assert!(seen.lock().unwrap().is_empty(), "nothing refused reached a command");

        assert_eq!(send(&router, post("/invoke/playlist_tree", LOOPBACK, &[])).await.0, StatusCode::OK, "local tools");
        let query = format!("/invoke/playlist_tree?token={TOKEN}&v={VERSION}");
        assert_eq!(send(&router, post(&query, TAILNET, &[])).await.0, StatusCode::OK, "token in the query");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn another_version_is_refused() {
        let (app, seen) = echo_app();
        let router = router(ctx(&app));
        let auth = ("authorization", "Bearer secret-token");
        let (status, body) = send(&router, post("/invoke/playlist_tree", TAILNET, &[auth, (VERSION_HEADER, "1.2.2")])).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["kind"], "versionMismatch");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains(VERSION) && message.contains("1.2.2"), "{message}");
        let (status, _) = send(&router, post("/invoke/playlist_tree", TAILNET, &[auth])).await;
        assert_eq!(status, StatusCode::CONFLICT, "no version at all");
        assert!(seen.lock().unwrap().is_empty());
    }
}

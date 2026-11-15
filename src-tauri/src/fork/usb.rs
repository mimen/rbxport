//! A USB stick on a client, filled from the host's library.
//!
//! The stick is here and the library is there. The client sends the host
//! the few small files on the stick that upstream's selection builders read.
//! The host lays them out in a temporary directory, runs those builders with
//! it as the stick, and answers with the selection and the host files it
//! names. The client copies those files into a cache, points the selection
//! at the copies, and runs upstream's exporter against its own stick.

use std::collections::HashSet;
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter as _, Manager as _, Runtime};
use tower::ServiceExt as _;
use tower_http::services::ServeFile;

use crate::commands::{
    reserve_export, set_export_failure, set_export_stage, write_export_registered, ExportReservation, ExportSelection,
};
use crate::device_settings::StickDefaultsDto;
use crate::dto::{ExportProgressDto, ExportReportDto, SyncDeviceReportDto, SyncProgressDto};
use crate::error::{AppError, AppResult, ErrorKind};
use crate::state::AppState;

use super::server::{query_value, refuse, Ctx, VERSION_HEADER};

const ROOTS: [&str; 2] = ["PIONEER", ".PIONEER"];
/// Read whole: the manifest and rekordbox's sync record.
const RECORDS: [&str; 3] = ["rbxport/manifest.json", "rekordbox/playlists3.sync", "rekordbox/playlists3Plus.sync"];
/// Any of these makes its root the stick's export root (`export_root_name`).
const MARKERS: [&str; 3] = ["rekordbox/export.pdb", "rekordbox/exportLibrary.db", "DEVSETTING.DAT"];
/// What stands for a root's markers on the host. An empty `export.pdb` would
/// fail `device_sync_state`'s parse; an empty `DEVSETTING.DAT` is read by none
/// of the code the host runs.
const MARKER: &str = "DEVSETTING.DAT";

/// One stick file as sent to the host, its path relative to the stick root.
#[derive(Debug, Serialize, Deserialize)]
struct StickFile {
    path: String,
    base64: String,
}

fn allowed_stick_file(path: &str) -> bool {
    ROOTS.iter().any(|root| {
        path.strip_prefix(root)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|rest| rest == MARKER || RECORDS.contains(&rest))
    })
}

/// What the host needs from the stick at `stick`.
fn snapshot(stick: &Path) -> Vec<StickFile> {
    let mut files = Vec::new();
    for root in ROOTS {
        for record in RECORDS {
            if let Ok(bytes) = std::fs::read(stick.join(root).join(record)) {
                files.push(StickFile { path: format!("{root}/{record}"), base64: base64::engine::general_purpose::STANDARD.encode(bytes) });
            }
        }
        if MARKERS.iter().any(|marker| stick.join(root).join(marker).exists()) {
            files.push(StickFile { path: format!("{root}/{MARKER}"), base64: String::new() });
        }
    }
    files
}

/// The client's stick files laid out as a stick, for upstream's builders.
fn unpack(files: &[StickFile]) -> AppResult<tempfile::TempDir> {
    let dir = tempfile::tempdir().map_err(|e| AppError::internal(e.to_string()))?;
    for file in files {
        if !allowed_stick_file(&file.path) {
            return Err(AppError::new(ErrorKind::Malformed, format!("{} is not a stick file the host reads.", file.path)));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&file.base64)
            .map_err(|e| AppError::new(ErrorKind::Malformed, e.to_string()))?;
        let path = dir.path().join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AppError::internal(e.to_string()))?;
        }
        std::fs::write(&path, bytes).map_err(|e| AppError::internal(e.to_string()))?;
    }
    Ok(dir)
}

/// `ExportSelection` over the wire: upstream's `SyncSource` has no serde and
/// `SourcePlaylist` only serializes.
#[derive(Serialize, Deserialize)]
struct Selection {
    tracks: Vec<rbl_export::SourceTrack>,
    playlists: Vec<Playlist>,
    my_tags: Vec<rbl_export::SourceMyTag>,
    db_id: u64,
    /// `(id, parent, attribute)` per node.
    tree: Vec<(u64, u64, u8)>,
    automatic: bool,
    cloud_db_id: u32,
    keep_unlisted: bool,
}

#[derive(Serialize, Deserialize)]
struct Playlist {
    device_id: u32,
    device_only: bool,
    id: u64,
    name: String,
    parent_id: u64,
    folder: bool,
    track_indices: Vec<usize>,
    entries_changed_ms: Option<u64>,
}

impl From<ExportSelection> for Selection {
    fn from(s: ExportSelection) -> Self {
        Self {
            tracks: s.tracks,
            playlists: s
                .playlists
                .into_iter()
                .map(|p| Playlist {
                    device_id: p.device_id,
                    device_only: p.device_only,
                    id: p.id,
                    name: p.name,
                    parent_id: p.parent_id,
                    folder: p.folder,
                    track_indices: p.track_indices,
                    entries_changed_ms: p.entries_changed_ms,
                })
                .collect(),
            my_tags: s.my_tags,
            db_id: s.sync.db_id,
            tree: s.sync.tree.into_iter().map(|n| (n.id, n.parent, n.attribute)).collect(),
            automatic: s.sync.automatic,
            cloud_db_id: s.sync.cloud_db_id,
            keep_unlisted: s.keep_unlisted,
        }
    }
}

impl From<Selection> for ExportSelection {
    fn from(s: Selection) -> Self {
        Self {
            tracks: s.tracks,
            playlists: s
                .playlists
                .into_iter()
                .map(|p| rbl_export::SourcePlaylist {
                    device_id: p.device_id,
                    device_only: p.device_only,
                    id: p.id,
                    name: p.name,
                    parent_id: p.parent_id,
                    folder: p.folder,
                    track_indices: p.track_indices,
                    entries_changed_ms: p.entries_changed_ms,
                })
                .collect(),
            my_tags: s.my_tags,
            sync: rbl_export::SyncSource {
                db_id: s.db_id,
                tree: s.tree.into_iter().map(|(id, parent, attribute)| rbl_export::SyncNode { id, parent, attribute }).collect(),
                automatic: s.automatic,
                cloud_db_id: s.cloud_db_id,
            },
            keep_unlisted: s.keep_unlisted,
        }
    }
}

/// Which upstream builder the host runs.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Op {
    Sync { playlists: Vec<String>, automatic: bool },
    Playlist { playlist: String },
    Tracks { tracks: Vec<String> },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectionRequest {
    op: Op,
    delete_unlisted_music: bool,
    stick: Vec<StickFile>,
}

/// A host file the selection names, as it is on the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostFile {
    path: PathBuf,
    size: u64,
    modified_ns: i64,
}

#[derive(Serialize, Deserialize)]
struct Answer {
    selection: Selection,
    files: Vec<HostFile>,
}

/// What the exporter opens for a track (`input_paths` in rbl-export's
/// incremental.rs).
fn input_paths(track: &rbl_export::SourceTrack) -> Vec<PathBuf> {
    let mut paths = vec![track.source_path.clone()];
    if let Some(base) = &track.analysis_source {
        paths.extend(["DAT", "EXT", "2EX"].map(|e| base.with_extension(e)));
    }
    if let Some(image) = &track.artwork {
        paths.extend([image.clone(), image.with_file_name("artwork_s.jpg"), image.with_file_name("artwork_m.jpg")]);
    }
    paths
}

fn modified_ns(meta: &std::fs::Metadata) -> i64 {
    let time = filetime::FileTime::from_last_modification_time(meta);
    time.unix_seconds().saturating_mul(1_000_000_000).saturating_add(i64::from(time.nanoseconds()))
}

/// The host files a client may fetch, for as long as the host runs.
static SERVED: LazyLock<parking_lot::Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

pub(crate) fn serve(path: PathBuf) {
    SERVED.lock().insert(path);
}

fn host_files(tracks: &[rbl_export::SourceTrack]) -> Vec<HostFile> {
    let mut seen = HashSet::new();
    let files: Vec<HostFile> = tracks
        .iter()
        .flat_map(input_paths)
        .filter(|path| seen.insert(path.clone()))
        .filter_map(|path| {
            let meta = std::fs::metadata(&path).ok().filter(std::fs::Metadata::is_file)?;
            Some(HostFile { size: meta.len(), modified_ns: modified_ns(&meta), path })
        })
        .collect();
    for file in &files {
        serve(file.path.clone());
    }
    files
}

fn build(state: &AppState, request: &SelectionRequest) -> AppResult<Answer> {
    let library = state.library()?;
    let share = state.share_root();
    let stick = unpack(&request.stick)?;
    let stick = stick.path();
    let delete = request.delete_unlisted_music;
    let selection = match &request.op {
        Op::Sync { playlists, automatic } => {
            let chosen = ExportSelection::from_playlists(state, &library, &share, playlists, *automatic)?;
            chosen.for_stick(state, &library, &share, stick, delete)?.into_owned()
        }
        Op::Playlist { playlist } => crate::commands::playlist_export_selection(state, &library, &share, stick, playlist, delete)?,
        Op::Tracks { tracks } => crate::commands::track_export_selection(state, &library, &share, stick, tracks)?,
    };
    let files = host_files(&selection.tracks);
    Ok(Answer { selection: selection.into(), files })
}

fn answer<T: Serialize>(result: AppResult<T>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (StatusCode::UNPROCESSABLE_ENTITY, Json(error)).into_response(),
    }
}

/// Host: `POST /usb/selection`.
pub async fn selection<R: Runtime>(State(ctx): State<Ctx<R>>, body: Bytes) -> Response {
    let request: SelectionRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, "badRequest", e.to_string()),
    };
    let state = Arc::clone(ctx.app.state::<Arc<AppState>>().inner());
    answer(crate::commands::blocking("remote usb selection", move || build(&state, &request)).await)
}

#[derive(Deserialize)]
struct SyncStateRequest {
    stick: Vec<StickFile>,
}

/// Host: `POST /usb/sync-state`, upstream's `device_sync_state` on the stick files.
pub async fn sync_state<R: Runtime>(State(ctx): State<Ctx<R>>, body: Bytes) -> Response {
    let request: SyncStateRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, "badRequest", e.to_string()),
    };
    let stick = match unpack(&request.stick) {
        Ok(stick) => stick,
        Err(error) => return answer::<()>(Err(error)),
    };
    let path = stick.path().to_string_lossy().into_owned();
    let result = crate::commands::device_sync_state(ctx.app.state::<Arc<AppState>>(), path).await;
    drop(stick);
    answer(result)
}

/// Host: `GET /usb/file?path=`, only what a selection listed.
pub async fn file(request: Request) -> Response {
    let path = query_value(request.uri(), "path").map(PathBuf::from);
    let Some(path) = path.filter(|path| SERVED.lock().contains(path)) else {
        return refuse(StatusCode::NOT_FOUND, "notFound", "The host does not serve that file.");
    };
    match ServeFile::new(path).oneshot(request).await {
        Ok(response) => response.map(Body::new),
        Err(never) => match never {},
    }
}

/// The library host as a client reaches it from Rust.
struct Host {
    base: String,
    name: String,
    token: String,
    version: String,
}

#[derive(Deserialize)]
struct HostAddress {
    url: String,
    token: String,
}

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(5)))
        // ponytail: no idle timeout on a body, and cancel is seen between reads, so a host that stalls mid-file holds the export until the socket drops.
        .timeout_recv_response(Some(Duration::from_secs(300)))
        .build()
        .into()
});

impl Host {
    fn new(address: &HostAddress, version: String) -> AppResult<Self> {
        let url = tauri::Url::parse(&address.url).map_err(|e| AppError::new(ErrorKind::Malformed, e.to_string()))?;
        let name = url.host_str().unwrap_or_default().to_owned();
        if url.scheme() != "http" || !super::forward::tailnet_name(&name) {
            return Err(AppError::new(ErrorKind::Malformed, format!("{} is not a Tailscale address.", address.url)));
        }
        let base = format!("http://{name}:{}", url.port().unwrap_or(80));
        Ok(Self { base, name, token: address.token.clone(), version })
    }

    fn unreachable(&self, error: &ureq::Error) -> AppError {
        AppError::new(ErrorKind::NotFound, format!("Could not reach the library host {}.", self.name)).with_detail(error.to_string())
    }

    fn post<T: DeserializeOwned>(&self, path: &str, body: &impl Serialize) -> AppResult<T> {
        let body = serde_json::to_vec(body).map_err(|e| AppError::internal(e.to_string()))?;
        let response = AGENT
            .post(format!("{}{path}", self.base))
            .header("authorization", format!("Bearer {}", self.token))
            .header(VERSION_HEADER, &self.version)
            .header("content-type", "application/json")
            .send(&body[..])
            .map_err(|e| self.unreachable(&e))?;
        let ok = response.status().is_success();
        let reader = response.into_body().into_reader();
        if !ok {
            return Err(refusal(&serde_json::from_reader(reader).unwrap_or(Value::Null), &self.name));
        }
        serde_json::from_reader(reader).map_err(|e| AppError::internal(format!("The library host's answer did not read: {e}")))
    }

    fn get_file(&self, path: &Path) -> AppResult<ureq::http::Response<ureq::Body>> {
        AGENT
            .get(format!("{}/usb/file", self.base))
            .query("path", path.to_string_lossy())
            .header("authorization", format!("Bearer {}", self.token))
            .header(VERSION_HEADER, &self.version)
            .call()
            .map_err(|e| self.unreachable(&e))
    }
}

/// The host's refusal or its command's error, as an `AppError`.
fn refusal(body: &Value, host: &str) -> AppError {
    let kind = match body.get("kind").and_then(Value::as_str) {
        Some("readOnly") => ErrorKind::ReadOnly,
        Some("notFound") => ErrorKind::NotFound,
        Some("malformed") => ErrorKind::Malformed,
        Some("cancelled") => ErrorKind::Cancelled,
        Some("conflict") => ErrorKind::Conflict,
        _ => ErrorKind::Internal,
    };
    let message = body.get("message").and_then(Value::as_str).map_or_else(|| format!("The library host {host} refused the request."), str::to_owned);
    let error = AppError::new(kind, message);
    match body.get("detail").and_then(Value::as_str) {
        Some(detail) => error.with_detail(detail),
        None => error,
    }
}

/// Where a host file is kept on this machine: its host path under `cache`,
/// so the analysis and artwork siblings the exporter derives still line up.
/// Only plain components are kept, so no host path reaches outside `cache`.
fn cache_path(cache: &Path, host_path: &Path) -> PathBuf {
    if host_path.as_os_str().is_empty() {
        return PathBuf::new();
    }
    host_path.components().fold(cache.to_path_buf(), |path, part| match part {
        Component::Normal(part) => path.join(part),
        _ => path,
    })
}

/// The cached copy is the host's file as listed.
fn fresh(local: &Path, file: &HostFile) -> bool {
    std::fs::metadata(local).is_ok_and(|meta| meta.is_file() && meta.len() == file.size && modified_ns(&meta) == file.modified_ns)
}

fn io(error: &std::io::Error) -> AppError {
    AppError::internal(format!("Could not cache a file from the library host: {error}"))
}

fn cancelled() -> AppError {
    AppError::new(ErrorKind::Cancelled, "Export cancelled.")
}

/// Copies one host file to `local`: a temporary name beside it, given the
/// host's modification time, then renamed over it.
fn download(host: &Host, file: &HostFile, local: &Path, cancel: &AtomicBool, moved: &mut dyn FnMut(u64)) -> AppResult<()> {
    let response = host.get_file(&file.path)?;
    if response.status() == StatusCode::NOT_FOUND {
        // Gone since it was listed: missing here too, as it is there.
        let _ = std::fs::remove_file(local);
        return Ok(());
    }
    if !response.status().is_success() {
        return Err(refusal(&serde_json::from_reader(response.into_body().into_reader()).unwrap_or(Value::Null), &host.name));
    }
    let dir = local.parent().ok_or_else(|| AppError::internal("A cache path has no folder."))?;
    std::fs::create_dir_all(dir).map_err(|e| io(&e))?;
    let name = local.file_name().unwrap_or_default().to_string_lossy();
    let part = dir.join(format!(".{name}.{}.part", uuid::Uuid::new_v4()));
    let written = (|| {
        let mut out = std::fs::File::create(&part).map_err(|e| io(&e))?;
        let mut body = response.into_body().into_reader();
        let mut buffer = vec![0_u8; 256 * 1024];
        let mut total = 0_u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(cancelled());
            }
            let read = body.read(&mut buffer).map_err(|e| io(&e))?;
            if read == 0 {
                break;
            }
            out.write_all(&buffer[..read]).map_err(|e| io(&e))?;
            total += read as u64;
            moved(read as u64);
        }
        if total != file.size {
            return Err(AppError::internal(format!("{} changed on the library host while it was copied.", file.path.display())));
        }
        drop(out);
        let mtime = filetime::FileTime::from_unix_time(file.modified_ns.div_euclid(1_000_000_000), u32::try_from(file.modified_ns.rem_euclid(1_000_000_000)).unwrap_or(0));
        filetime::set_file_mtime(&part, mtime).map_err(|e| io(&e))?;
        std::fs::rename(&part, local).map_err(|e| io(&e))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    written
}

/// One download at a time, so sticks synced together fetch each file once.
static FETCH: parking_lot::Mutex<()> = parking_lot::const_mutex(());

/// Brings the cache up to the host's files, saying so on the stick's
/// `export:progress` as its preparing stage.
fn fetch<R: Runtime>(app: &AppHandle<R>, host: &Host, stick: &Path, cache: &Path, files: &[HostFile], cancel: &AtomicBool) -> AppResult<()> {
    // ponytail: one stream, and the cache grows to everything ever synced from the host; prune by age or size if it matters.
    let _one = FETCH.lock();
    let wanted: Vec<(&HostFile, PathBuf)> = files.iter().map(|f| (f, cache_path(cache, &f.path))).filter(|(f, local)| !fresh(local, f)).collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let bytes_total: u64 = wanted.iter().map(|(f, _)| f.size).sum();
    let started = Instant::now();
    let mut last = started.checked_sub(Duration::from_secs(1)).unwrap_or(started);
    let mut bytes_done = 0_u64;
    for (index, (file, local)) in wanted.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let title = format!("Fetching from {}: file {} of {}", host.name, index + 1, wanted.len());
        download(host, file, local, cancel, &mut |moved| {
            bytes_done += moved;
            if last.elapsed() < Duration::from_millis(250) {
                return;
            }
            last = Instant::now();
            let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX).max(1);
            // No job id: the front end orders only events that carry one, so
            // the export's own, which start again at sequence 1, still land.
            let _ = app.emit("export:progress", ExportProgressDto {
                path: stick.to_string_lossy().into_owned(),
                state: "preparing",
                unit: "bytes",
                bytes_done,
                bytes_total,
                bytes_per_second: bytes_done.saturating_mul(1000) / ms,
                elapsed_ms: ms,
                can_cancel: true,
                title: title.clone(),
                ..Default::default()
            });
        })?;
    }
    Ok(())
}

/// Points a track at the cached copies, and drops a cached copy of anything
/// the host no longer has, so the exporter finds it missing as the host would.
fn localize(track: &mut rbl_export::SourceTrack, cache: &Path, listed: &HashSet<&Path>) {
    for path in input_paths(track) {
        if !listed.contains(path.as_path()) {
            let _ = std::fs::remove_file(cache_path(cache, &path));
        }
    }
    track.source_path = cache_path(cache, &track.source_path);
    track.analysis_source = track.analysis_source.as_deref().map(|p| cache_path(cache, p));
    track.artwork = track.artwork.as_deref().map(|p| cache_path(cache, p));
}

/// The selection for `stick`, built by the host, its files cached here.
fn remote_selection<R: Runtime>(
    app: &AppHandle<R>,
    host: &Host,
    stick: &Path,
    op: Op,
    delete_unlisted_music: bool,
    reservation: &ExportReservation<R>,
) -> AppResult<ExportSelection> {
    reservation.check(stick)?;
    rbl_export::recover(stick).map_err(|e| AppError::internal(e.to_string()))?;
    let request = SelectionRequest { op, delete_unlisted_music, stick: snapshot(stick) };
    let answer: Answer = host.post("/usb/selection", &request)?;
    let cache = rbl_backup::state_dir().join("cache/fork-remote").join(&host.name);
    fetch(app, host, stick, &cache, &answer.files, &reservation.cancel)?;
    let listed: HashSet<&Path> = answer.files.iter().map(|f| f.path.as_path()).collect();
    let mut selection = answer.selection;
    for track in &mut selection.tracks {
        localize(track, &cache, &listed);
    }
    Ok(selection.into())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncArgs {
    playlists: Vec<String>,
    destinations: Vec<String>,
    defaults: Option<StickDefaultsDto>,
    automatic: Option<bool>,
    eject_after_sync: Option<bool>,
    delete_unlisted_music: Option<bool>,
    compatibility_format: Option<rbl_export::CompatibilityFormat>,
    verify_all: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaylistArgs {
    playlist: String,
    destination: String,
    defaults: Option<StickDefaultsDto>,
    delete_unlisted_music: Option<bool>,
    compatibility_format: Option<rbl_export::CompatibilityFormat>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TracksArgs {
    tracks: Vec<String>,
    destination: String,
    defaults: Option<StickDefaultsDto>,
    compatibility_format: Option<rbl_export::CompatibilityFormat>,
}

#[derive(Deserialize)]
struct SyncStateArgs {
    path: String,
}

#[derive(Deserialize)]
struct Call<A> {
    host: HostAddress,
    args: A,
}

/// Why a sync that wrote is not clean enough to eject, as `sync_one_device` says it.
fn not_ejectable(report: &ExportReportDto) -> Option<String> {
    let mut causes = Vec::new();
    if !report.skipped.is_empty() {
        causes.push(format!("{} missing or unreadable", report.skipped.len()));
    }
    if !report.failed.is_empty() {
        causes.push(format!("{} could not be written", report.failed.len()));
    }
    if !report.verified {
        causes.push("the written library did not read back correctly".to_owned());
    }
    if !report.warnings.is_empty() {
        causes.push(format!("{} updates need attention", report.warnings.len()));
    }
    (!causes.is_empty()).then(|| format!("The sync was incomplete or could not be verified ({}). Review it before ejecting.", causes.join(", ")))
}

/// Upstream's `sync_one_device` with the selection from the host.
fn sync_one<R: Runtime>(app: &AppHandle<R>, host: &Host, args: &SyncArgs, destination: String, reservation: &mut ExportReservation<R>) -> SyncDeviceReportDto {
    let progress = |state: &'static str| {
        let _ = app.emit("sync:progress", SyncProgressDto { path: destination.clone(), state });
    };
    progress("writing");
    let stick = Path::new(&destination);
    let eject = args.eject_after_sync.unwrap_or(false);
    let op = Op::Sync { playlists: args.playlists.clone(), automatic: args.automatic.unwrap_or(false) };
    let written = remote_selection(app, host, stick, op, args.delete_unlisted_music.unwrap_or(false), reservation)
        .and_then(|selection| write_export_registered(app, stick, &selection, args.defaults.as_ref(), args.compatibility_format, reservation));
    match written {
        Ok(report) => {
            let mut result = SyncDeviceReportDto { path: destination.clone(), report: None, error: None, ejected: false, eject_error: None };
            if eject {
                result.eject_error = not_ejectable(&report);
                if result.eject_error.is_none() {
                    progress("ejecting");
                    set_export_stage(app, stick, "ejecting");
                    drop(reservation.volume.take());
                    match rbl_devices::eject::eject(stick) {
                        Ok(()) => result.ejected = true,
                        Err(e) => result.eject_error = Some(e.to_string()),
                    }
                }
            }
            result.report = Some(report);
            progress("done");
            if eject {
                set_export_stage(app, stick, "done");
            }
            result
        }
        Err(e) => {
            progress("failed");
            tracing::warn!(destination, error = %e, detail = e.detail.as_deref().unwrap_or(""), "remote sync to one device failed");
            let message = e.full_message();
            set_export_failure(app, stick, message.clone());
            SyncDeviceReportDto { path: destination, report: None, error: Some(message), ejected: false, eject_error: None }
        }
    }
}

/// Upstream's `sync_devices`, each stick's selection from the host.
fn sync_devices<R: Runtime>(app: &AppHandle<R>, host: &Host, args: &SyncArgs) -> AppResult<Vec<SyncDeviceReportDto>> {
    let mut unique: Vec<&str> = Vec::new();
    for destination in &args.destinations {
        if unique.iter().any(|prior| rbl_core::durable::same_root(Path::new(prior), Path::new(destination)).unwrap_or(false)) {
            return Err(AppError::internal("The same USB device was selected more than once. Select it once and retry."));
        }
        unique.push(destination);
    }
    let reservations: Vec<_> = args
        .destinations
        .iter()
        .map(|path| {
            reserve_export(app, Path::new(path)).map(|mut ticket| {
                ticket.verify_all = args.verify_all.unwrap_or(false);
                ticket
            })
        })
        .collect();
    Ok(std::thread::scope(|scope| {
        let workers: Vec<_> = args
            .destinations
            .iter()
            .cloned()
            .zip(reservations)
            .map(|(destination, reservation)| {
                scope.spawn(move || match reservation {
                    Err(error) => {
                        for state in ["writing", "failed"] {
                            let _ = app.emit("sync:progress", SyncProgressDto { path: destination.clone(), state });
                        }
                        SyncDeviceReportDto { path: destination, report: None, error: Some(error.full_message()), ejected: false, eject_error: None }
                    }
                    Ok(mut reservation) => sync_one(app, host, args, destination, &mut reservation),
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| {
                worker.join().unwrap_or_else(|_| SyncDeviceReportDto {
                    path: "Unknown device".to_owned(),
                    report: None,
                    error: Some("The sync worker stopped unexpectedly.".to_owned()),
                    ejected: false,
                    eject_error: None,
                })
            })
            .collect()
    }))
}

fn export_to<R: Runtime>(app: &AppHandle<R>, host: &Host, destination: &str, op: Op, delete: bool, defaults: Option<&StickDefaultsDto>, format: Option<rbl_export::CompatibilityFormat>) -> AppResult<ExportReportDto> {
    let stick = Path::new(destination);
    let reservation = reserve_export(app, stick)?;
    let selection = remote_selection(app, host, stick, op, delete, &reservation)?;
    write_export_registered(app, stick, &selection, defaults, format, &reservation)
}

fn parse_call<A: DeserializeOwned, R: Runtime>(app: &AppHandle<R>, body: &[u8]) -> AppResult<(Host, A)> {
    let call: Call<A> = serde_json::from_slice(body).map_err(|e| AppError::new(ErrorKind::Malformed, e.to_string()))?;
    Ok((Host::new(&call.host, app.package_info().version.to_string())?, call.args))
}

fn to_value(value: impl Serialize) -> AppResult<Value> {
    serde_json::to_value(value).map_err(|e| AppError::internal(e.to_string()))
}

async fn run<R: Runtime>(app: &AppHandle<R>, command: &str, body: &[u8]) -> AppResult<Value> {
    let app = app.clone();
    match command {
        "sync_devices" => {
            let (host, args) = parse_call::<SyncArgs, R>(&app, body)?;
            to_value(crate::commands::blocking("sync_devices", move || sync_devices(&app, &host, &args)).await?)
        }
        "export_playlist" | "export_tracks_to_device" => {
            let (host, op, destination, delete, defaults, format) = if command == "export_playlist" {
                let (host, a) = parse_call::<PlaylistArgs, R>(&app, body)?;
                (host, Op::Playlist { playlist: a.playlist }, a.destination, a.delete_unlisted_music.unwrap_or(false), a.defaults, a.compatibility_format)
            } else {
                let (host, a) = parse_call::<TracksArgs, R>(&app, body)?;
                (host, Op::Tracks { tracks: a.tracks }, a.destination, false, a.defaults, a.compatibility_format)
            };
            let worker = app.clone();
            let report = crate::commands::blocking("remote usb export", move || export_to(&worker, &host, &destination, op, delete, defaults.as_ref(), format)).await?;
            let _ = app.emit("export:done", &report);
            to_value(report)
        }
        "device_sync_state" => {
            let (host, args) = parse_call::<SyncStateArgs, R>(&app, body)?;
            // The stick's own side (what it holds, recovery, the lease) here.
            let mut state = to_value(crate::commands::device_sync_state(app.state::<Arc<AppState>>(), args.path.clone()).await?)?;
            let stick = PathBuf::from(&args.path);
            let theirs: Value = crate::commands::blocking("remote usb sync state", move || {
                host.post("/usb/sync-state", &serde_json::json!({ "stick": snapshot(&stick) }))
            })
            .await?;
            for key in ["selected", "automatic"] {
                if let (Some(ours), Some(value)) = (state.get_mut(key), theirs.get(key)) {
                    ours.clone_from(value);
                }
            }
            Ok(state)
        }
        _ => Err(AppError::new(ErrorKind::NotFound, format!("No remote USB command {command}."))),
    }
}

/// Client: `rbxfork://localhost/usb/<command>`, body `{ host, args }`, args
/// as the front end sends them to upstream's command of that name.
pub async fn handle<R: Runtime>(app: &AppHandle<R>, request: &tauri::http::Request<Vec<u8>>) -> tauri::http::Response<Vec<u8>> {
    let command = request.uri().path().trim_start_matches("/usb/");
    match run(app, command, request.body()).await {
        Ok(value) => super::json(StatusCode::OK, &value),
        Err(error) => super::json(StatusCode::UNPROCESSABLE_ENTITY, &error),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_host_takes_only_the_stick_files_it_reads() {
        for path in ["PIONEER/rbxport/manifest.json", ".PIONEER/rekordbox/playlists3.sync", "PIONEER/DEVSETTING.DAT"] {
            assert!(unpack(&[StickFile { path: path.into(), base64: String::new() }]).is_ok(), "{path}");
        }
        for path in [
            "../PIONEER/rbxport/manifest.json",
            "PIONEER/../../etc/passwd",
            "/PIONEER/rbxport/manifest.json",
            "PIONEER/rekordbox/export.pdb",
            "PIONEERX/rbxport/manifest.json",
            "PIONEER/rbxport/manifest.json/../../../x",
            "",
        ] {
            assert!(unpack(&[StickFile { path: path.into(), base64: String::new() }]).is_err(), "{path}");
        }
    }

    #[test]
    fn a_hidden_root_stick_resolves_to_the_same_root_on_the_host() {
        let stick = tempfile::tempdir().unwrap();
        let root = stick.path().join(".PIONEER");
        std::fs::create_dir_all(root.join("rekordbox")).unwrap();
        std::fs::create_dir_all(root.join("rbxport")).unwrap();
        std::fs::write(root.join("rekordbox/export.pdb"), b"not empty").unwrap();
        std::fs::write(root.join("rbxport/manifest.json"), b"{\"m\":1}").unwrap();
        std::fs::write(stick.path().join("unrelated.mp3"), b"audio").unwrap();

        let host = unpack(&snapshot(stick.path())).unwrap();
        assert!(rbl_export::export_root(host.path()).ends_with(".PIONEER"));
        assert_eq!(std::fs::read(rbl_export::Manifest::path(host.path())).unwrap(), b"{\"m\":1}");
        assert!(!host.path().join("unrelated.mp3").exists());
        assert!(!host.path().join(".PIONEER/rekordbox/export.pdb").exists(), "a marker, not the database");
    }

    #[test]
    fn a_cached_file_sits_under_its_host_path_and_never_outside_the_cache() {
        let cache = Path::new("/cache/host.ts.net");
        let audio = Path::new("/Users/dj/Music/a.mp3");
        assert_eq!(cache_path(cache, audio), Path::new("/cache/host.ts.net/Users/dj/Music/a.mp3"));
        let analysis = Path::new("/Users/dj/Library/Pioneer/rekordbox/share/PIONEER/USBANLZ/ab/cd/ANLZ0000.DAT");
        assert_eq!(cache_path(cache, &analysis.with_extension("EXT")), cache_path(cache, analysis).with_extension("EXT"));
        let artwork = Path::new("/Users/dj/share/PIONEER/Artwork/00/a1/artwork.jpg");
        assert_eq!(cache_path(cache, &artwork.with_file_name("artwork_s.jpg")), cache_path(cache, artwork).with_file_name("artwork_s.jpg"));
        for sneaky in ["/../../etc/passwd", "../x", "/a/../../../../b"] {
            assert!(cache_path(cache, Path::new(sneaky)).starts_with(cache), "{sneaky}");
            assert!(!cache_path(cache, Path::new(sneaky)).components().any(|c| c == Component::ParentDir), "{sneaky}");
        }
        assert_eq!(cache_path(cache, Path::new("")), PathBuf::new(), "no audio stays no audio");
    }

    #[test]
    fn a_cached_copy_is_reused_only_at_the_hosts_size_and_time() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("a.mp3");
        std::fs::write(&local, b"12345").unwrap();
        let time = filetime::FileTime::from_unix_time(1_700_000_000, 123_456_789);
        filetime::set_file_mtime(&local, time).unwrap();
        let listed = HostFile { path: "/x/a.mp3".into(), size: 5, modified_ns: 1_700_000_000_123_456_789 };
        assert!(fresh(&local, &listed));
        assert!(!fresh(&local, &HostFile { size: 6, ..listed.clone() }), "another size");
        assert!(!fresh(&local, &HostFile { modified_ns: listed.modified_ns + 1, ..listed.clone() }), "another time");
        assert!(!fresh(&dir.path().join("b.mp3"), &listed), "not cached");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetching_copies_a_listed_file_once_with_the_hosts_time() {
        let app = tauri::test::mock_app();
        let (_stop, stop) = tokio::sync::watch::channel(());
        let ctx = Ctx { app: app.handle().clone(), token: "t".into(), version: "v".into(), stop };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let service = super::super::server::router(ctx).into_make_service_with_connect_info::<std::net::SocketAddr>();
        tokio::spawn(async move { axum::serve(listener, service).await });

        let host_dir = tempfile::tempdir().unwrap();
        let source = host_dir.path().join("Music/a.mp3");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"first").unwrap();
        filetime::set_file_mtime(&source, filetime::FileTime::from_unix_time(1_700_000_000, 5)).unwrap();
        let listed = host_files(&[rbl_export::SourceTrack { source_path: source.clone(), ..Default::default() }]);
        assert_eq!(listed.len(), 1);

        let cache = tempfile::tempdir().unwrap();
        let cached = cache_path(cache.path(), &source);
        let run = |files: Vec<HostFile>, cancel: bool| {
            let (handle, base, cache) = (app.handle().clone(), base.clone(), cache.path().to_path_buf());
            tokio::task::spawn_blocking(move || {
                let host = Host { base, name: "test".into(), token: "t".into(), version: "v".into() };
                fetch(&handle, &host, Path::new("/Volumes/STICK"), &cache, &files, &AtomicBool::new(cancel))
            })
        };

        run(listed.clone(), false).await.unwrap().unwrap();
        assert_eq!(std::fs::read(&cached).unwrap(), b"first");
        assert!(fresh(&cached, &listed[0]), "the host's size and time");

        std::fs::write(&source, b"other").unwrap();
        run(listed.clone(), false).await.unwrap().unwrap();
        assert_eq!(std::fs::read(&cached).unwrap(), b"first", "same size and time: not fetched again");

        let wrong_size = vec![HostFile { size: 99, modified_ns: 1, ..listed[0].clone() }];
        assert!(run(wrong_size, false).await.unwrap().is_err());
        assert_eq!(std::fs::read(&cached).unwrap(), b"first", "a short copy never replaces the cache");
        assert_eq!(std::fs::read_dir(cached.parent().unwrap()).unwrap().count(), 1, "no partial file left");

        let changed = vec![HostFile { modified_ns: 1, ..listed[0].clone() }];
        let stopped = run(changed.clone(), true).await.unwrap().unwrap_err();
        assert_eq!(stopped.kind, ErrorKind::Cancelled);
        run(changed, false).await.unwrap().unwrap();
        assert_eq!(std::fs::read(&cached).unwrap(), b"other", "a new time fetches again");
    }

    #[test]
    fn the_wire_selection_is_the_selection() {
        let selection = ExportSelection {
            tracks: vec![rbl_export::SourceTrack { id: 7, title: "T".into(), source_path: "/a.mp3".into(), ..Default::default() }],
            playlists: vec![rbl_export::SourcePlaylist { id: 3, name: "P".into(), parent_id: 2, track_indices: vec![0], entries_changed_ms: Some(9), ..Default::default() }],
            my_tags: vec![rbl_export::SourceMyTag { id: 1, seq: 1, name: "Tag".into(), attribute: 0, parent: 0 }],
            sync: rbl_export::SyncSource { db_id: 42, tree: vec![rbl_export::SyncNode { id: 3, parent: 2, attribute: 0 }], automatic: true, cloud_db_id: 5 },
            keep_unlisted: true,
        };
        let wire = serde_json::to_vec(&Selection::from(selection.clone())).unwrap();
        let back: ExportSelection = serde_json::from_slice::<Selection>(&wire).unwrap().into();
        assert_eq!(back.sync, selection.sync);
        assert_eq!(back.my_tags, selection.my_tags);
        assert!(back.keep_unlisted);
        assert_eq!((back.playlists[0].id, back.playlists[0].parent_id, &back.playlists[0].track_indices), (3, 2, &vec![0]));
        assert_eq!((back.tracks[0].id, back.tracks[0].source_path.as_path()), (7, Path::new("/a.mp3")));
    }
}

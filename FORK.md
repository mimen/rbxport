---
upstream: chrisle/rbxport
sync_tags: v*
check: scripts/fork/check.sh
release: scripts/fork/release.sh
---

# rbxport fork

Milad's fork of [chrisle/rbxport](https://github.com/chrisle/rbxport), started from upstream `v1.8.2` on 2026-10-16. It exists to ship its own signed builds, to follow upstream releases, and to carry server mode. Rules for changing anything here are in the `fork` skill.

Upstream releases from `dev`, tags each release `vX.Y.Z`, and fast-forwards `main` to the tag. The fork tracks those tags, never `dev`, so `main` only ever holds released upstream code. The hub's nightly Updates job merges each new tag and runs the release.

**Identity.** The app ships as "rbxport (Fork)", bundle id `com.mimen.rbxport.fork`. All identity values live in [`src-tauri/tauri.fork.conf.json`](src-tauri/tauri.fork.conf.json), which `tauri build --config` merges over upstream's `tauri.conf.json`, so no upstream file is edited.

**Releases.** [`scripts/fork/release.sh`](scripts/fork/release.sh) builds on the Mac Mini, signs with the "T3 Code Fork Signing" certificate (team `458KZD965T`), and publishes the dmg, the updater archive, its signature, and `latest.json` as the latest release on mimen/rbxport. Versions are `<upstream version>-fork.<yyyymmdd>.<HHMM UTC>`, set in the build worktree only. Installed apps check `releases/latest/download/latest.json` and take updates signed by the key in the 1Password item "rbxport Fork Updater Key". Losing that key strands every installed copy.

**Switched off in fork builds.** Sentry stays off because the release builds with no DSN. Bug reports go to upstream's `report.rbxport.com`, so the release drops that origin from the CSP it passes to `tauri build`, and the Report a Bug window cannot reach it.

**Shared with upstream's app.** rbxport names its data directory `~/Library/Application Support/rbxport` in code rather than from the bundle id, so the fork and upstream's app share preferences, logs, and backups, and they act on the same rekordbox library and USB sticks. Install one or the other on a machine, never both.

**Checks.** [`scripts/fork/check.sh`](scripts/fork/check.sh) runs upstream's frontend lint, typecheck, and unit tests and the Rust workspace tests under `RB_LITE_TEST=1`, which refuses writes to an installed rekordbox library. It skips two wall-clock deck tests that upstream CI runs but the hub's headless sessions fail on timer coalescing; their reasons are in the script.

**Server mode.** One machine serves its library and the fork app on other machines browses and edits it over Tailscale. On the host, Preferences › Server › Serve this library starts a server on port 48650, on 127.0.0.1 and the machine's Tailscale address, saved in `~/Library/Application Support/rbxport/fork-server.json`. Off by default. A client enters the host's MagicDNS address and token under Remote host.

- Commands are not re-implemented. [`src-tauri/src/fork/server.rs`](src-tauri/src/fork/server.rs) turns `POST /invoke/<command>` into the `InvokeRequest` the webview would have sent and delivers it to the main webview, so upstream's `generate_handler!` runs it. The accepted names are that list, read from `lib.rs` at compile time ([`commands.rs`](src-tauri/src/fork/commands.rs)), so upstream adding a command needs no fork edit.
- `GET /events` is a WebSocket that forwards whichever app events the client names. `GET /artwork/<id>` runs `protocol.rs`. `GET /audio/<id>` serves the file with range requests. `GET /version` names the build.
- Every request from another machine needs the token. Loopback needs none unless it carries an `Origin` header, so a web page on the host cannot drive it. `/invoke` and `/events` refuse a client built from a different version with a `versionMismatch` error.
- On a client, [`src/ipc/fork/remote.ts`](src/ipc/fork/remote.ts) routes each command: window, menu, update, audio-engine and stick-only commands run locally, USB exports go through Remote USB below, file dialogs, importing from a stick, LINK and the decks are refused with a note, and everything else goes to the host. [`src/fork/remote.css`](src/fork/remote.css) hides the controls for what only the host can do.
- A client's page never talks to the host itself. A release window's page is `tauri://localhost`, which WebKit treats as secure, so it blocks `http://` and `ws://` to another machine as mixed content (a debug window, at `http://localhost:1420`, does not). [`src-tauri/src/fork/forward.rs`](src-tauri/src/fork/forward.rs) holds a port on 127.0.0.1 for each host, asked for over `rbxfork://localhost/forward`, and pipes its bytes to the host unchanged. It forwards only to a `*.ts.net` name.
- When the host does not answer, the client's main window says so instead of loading forever. [`src/ipc/fork/link.ts`](src/ipc/fork/link.ts) holds connecting, connected or unreachable, with why: refused, timed out, token refused, version mismatch. A check is the forward's own connection to the host (`rbxfork://localhost/reach`, three seconds at most) and then `/version`. A failed request or a dropped events socket runs one; while unreachable it reruns every 1 to 15 seconds. [`src/fork/unreachable.ts`](src/fork/unreachable.ts) shows the notice, logs `Can't reach the library on <host> (<reason>)`, and reloads the window once the host answers. A preview that errors or gets no audio for eight seconds stops and says so.
- The CSP allows that forward, `127.0.0.1`, through the overlay [`scripts/fork/csp.mjs`](scripts/fork/csp.mjs) prints for `tauri build --config`.
- **Remote USB.** A stick plugged into the client takes the host's playlists and tracks through the Sync Manager, Export Playlist and Export Track. The host builds the selection and the client writes it. The client recovers the stick and sends the host its manifest, its sync record and which export root it uses. [`src-tauri/src/fork/usb.rs`](src-tauri/src/fork/usb.rs) on the host lays those out in a temporary directory, runs upstream's own selection builders on it at `POST /usb/selection`, and answers with the selection and every host file it names. The client fetches each file from `GET /usb/file`, which serves only files a selection listed, into `cache/fork-remote/<host>/<host path>` under the state directory, skipping one whose size and time match. It points the selection at the copies and runs upstream's exporter on its own stick. `device_sync_state` reads the stick here and asks `POST /usb/sync-state` for the last selection. The space estimate is not available on a client.
- The check covers it: every name in the table dispatches through the HTTP route, every command the client sends exists on the server, and wrong tokens and other versions are refused. Before publishing, `release.sh` runs [`scripts/fork/e2e-client.sh`](scripts/fork/e2e-client.sh), which launches the built release app as a client of a stopped stub host, fails unless it logs the host unreachable, then starts the stub and fails unless the client recovers and the host sees its invoke and its events socket.

| Path | Why |
| ---- | --- |
| `src-tauri/src/lib.rs` | Server mode: `mod fork;` and `.plugin(crate::fork::plugin())` |
| `src-tauri/Cargo.toml` | Server mode: axum, tokio, tower, tower-http and if-addrs for the library server; ureq and filetime for Remote USB's downloads |
| `Cargo.lock` | The server's and Remote USB's dependencies |
| `src-tauri/src/commands.rs` | Remote USB: `pub(crate)` on the selection builders, `reserve_export`, `ExportReservation` and the fields and check it uses, `write_export_registered`, `set_export_stage`, `set_export_failure` |
| `src/ipc/invoke.ts` | Server mode: `tauriInvoke` wraps its function in `withRemote` |
| `src/ipc/client.ts` | Server mode: `subscribe` takes host events, `getBackend` wraps the backend, native drag-out off on a client |
| `src/ipc/artwork.ts` | Server mode: artwork from the host on a client |
| `src/main.tsx` | Server mode: `import "./fork/boot"` |
| `src/views/settings/Preferences.tsx` | Server mode: the Server pane |

---
upstream: chrisle/rbxport
sync_tags: v*
check: scripts/fork/check.sh
release: scripts/fork/release.sh
---

# rbxport fork

Milad's fork of [chrisle/rbxport](https://github.com/chrisle/rbxport), started from upstream `v1.8.2` on 2026-10-16. It carries no features of its own yet. It exists to ship its own signed builds and to follow upstream releases. Rules for changing anything here are in the `fork` skill.

Upstream releases from `dev`, tags each release `vX.Y.Z`, and fast-forwards `main` to the tag. The fork tracks those tags, never `dev`, so `main` only ever holds released upstream code. The hub's nightly Updates job merges each new tag and runs the release.

**Identity.** The app ships as "rbxport (Fork)", bundle id `com.mimen.rbxport.fork`. All identity values live in [`src-tauri/tauri.fork.conf.json`](src-tauri/tauri.fork.conf.json), which `tauri build --config` merges over upstream's `tauri.conf.json`, so no upstream file is edited.

**Releases.** [`scripts/fork/release.sh`](scripts/fork/release.sh) builds on the Mac Mini, signs with the "T3 Code Fork Signing" certificate (team `458KZD965T`), and publishes the dmg, the updater archive, its signature, and `latest.json` as the latest release on mimen/rbxport. Versions are `<upstream version>-fork.<yyyymmdd>.<HHMM UTC>`, set in the build worktree only. Installed apps check `releases/latest/download/latest.json` and take updates signed by the key in the 1Password item "rbxport Fork Updater Key". Losing that key strands every installed copy.

**Switched off in fork builds.** Sentry stays off because the release builds with no DSN. Bug reports go to upstream's `report.rbxport.com`, so the release drops that origin from the CSP it passes to `tauri build`, and the Report a Bug window cannot reach it.

**Shared with upstream's app.** rbxport names its data directory `~/Library/Application Support/rbxport` in code rather than from the bundle id, so the fork and upstream's app share preferences, logs, and backups, and they act on the same rekordbox library and USB sticks. Install one or the other on a machine, never both.

**Checks.** [`scripts/fork/check.sh`](scripts/fork/check.sh) runs upstream's frontend lint, typecheck, and unit tests and the Rust workspace tests under `RB_LITE_TEST=1`, which refuses writes to an installed rekordbox library.

| Path | Why |
| ---- | --- |

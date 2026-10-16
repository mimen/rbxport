#!/usr/bin/env bash
# Build "rbxport (Fork)" for Apple silicon on the Mac Mini, sign it, and publish it as the
# latest release on mimen/rbxport. Installed fork apps read
# releases/latest/download/latest.json, which resolves only to the newest non-prerelease.
#
#   scripts/fork/release.sh [--dry-run] [<ref>]      default ref: origin/main
#
# Idempotent: exits 0 without building when the newest release already targets <ref>.
set -euo pipefail

REPO=mimen/rbxport
TARGET=aarch64-apple-darwin
BUNDLE_ID=com.mimen.rbxport.fork
KEYCHAIN="$HOME/Library/Keychains/fork-signing.keychain-db"
SIGNING_ITEM="op://Sol/T3 Code Fork Signing"
UPDATER_ITEM="op://Sol/rbxport Fork Updater Key"
# Upstream CI builds with Node 24; the Mini's default node is older.
[[ -d /opt/homebrew/opt/node@24/bin ]] && PATH="/opt/homebrew/opt/node@24/bin:$PATH"
PATH="$HOME/.cargo/bin:$PATH"

dry_run=false
[[ "${1:-}" == "--dry-run" ]] && { dry_run=true; shift; }
ref="${1:-origin/main}"

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
git -C "$root" fetch -q origin
git -C "$root" fetch -q --tags upstream
sha="$(git -C "$root" rev-parse --verify "$ref^{commit}")"

latest="$(gh release list -R "$REPO" --limit 1 --json tagName --jq '.[0].tagName // ""')"
if [[ -n "$latest" && "$(gh release view "$latest" -R "$REPO" --json targetCommitish --jq .targetCommitish)" == "$sha" ]]; then
  echo "release: $latest already targets $sha"
  exit 0
fi

# The fork's own release tags (v1.8.2-fork.…) also match v*; only upstream's count here.
upstream_tag="$(git -C "$root" tag --merged "$sha" -l 'v*' --sort=-v:refname | grep -m 1 -v -- '-fork\.' || true)"
core="$(sed -E 's/^v([0-9]+\.[0-9]+\.[0-9]+).*/\1/' <<<"$upstream_tag")"
[[ "$core" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "release: no upstream v* tag merged into $sha" >&2; exit 1; }
# Semver numeric identifiers take no leading zeros, so HHMM 0905 becomes 905.
version="$core-fork.$(date -u +%Y%m%d).$((10#$(date -u +%H%M)))"
tag="v$version"
name="rbxport-fork_${version}_aarch64"

cache="$HOME/Library/Caches/rbxport-fork-release"
work="$cache/src-$sha"
out="$cache/out-$sha"
export CARGO_TARGET_DIR="$cache/target"
bundle="$CARGO_TARGET_DIR/$TARGET/release/bundle"
if [[ ! -d "$work" ]]; then
  git -C "$root" worktree add -q --detach "$work" "$sha"
fi
trap 'git -C "$root" worktree remove --force "$work" >/dev/null 2>&1 || true' EXIT

cd "$work"
pnpm install --frozen-lockfile
node scripts/sync-version.mjs --version "$version"

# The CSP overlay: upstream's, less its bug-report origin, plus server mode's host.
csp_config="$(node scripts/fork/csp.mjs)"

# No DSN means Sentry stays off in both the renderer and the native app.
unset SENTRY_DSN VITE_SENTRY_DSN SENTRY_AUTH_TOKEN
security unlock-keychain -p "$(op read "$SIGNING_ITEM/keychain_password")" "$KEYCHAIN"
TAURI_SIGNING_PRIVATE_KEY="$(op read "$UPDATER_ITEM/private_key")"
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(op read "$UPDATER_ITEM/password")"
export TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD

rm -rf "$bundle" "$out"
# CI=true skips the dmg's Finder AppleScript, which fails without a GUI session (ssh, launchd).
CI=true pnpm tauri build --target "$TARGET" --config src-tauri/tauri.fork.conf.json --config "$csp_config"

app="$(ls -d "$bundle"/macos/*.app)"
plist="$app/Contents/Info.plist"
[[ "$(plutil -extract CFBundleIdentifier raw "$plist")" == "$BUNDLE_ID" ]] || { echo "release: $app is not $BUNDLE_ID" >&2; exit 1; }
[[ "$(plutil -extract CFBundleShortVersionString raw "$plist")" == "$version" ]] || { echo "release: $app is not version $version" >&2; exit 1; }
grep -aq "github.com/mimen/rbxport/releases/latest/download/latest.json" "$app/Contents/MacOS/"* || { echo "release: $app does not read the fork's update feed" >&2; exit 1; }
codesign --verify --deep --strict "$app"
signature="$(codesign -dv "$app" 2>&1)"
[[ "$signature" == *"TeamIdentifier=458KZD965T"* ]] || { echo "release: app is not signed by team 458KZD965T" >&2; exit 1; }

mkdir -p "$out"
cp "$(ls "$bundle"/dmg/*.dmg)" "$out/$name.dmg"
cp "$app.tar.gz" "$out/$name.app.tar.gz"
cp "$app.tar.gz.sig" "$out/$name.app.tar.gz.sig"
node -e '
const [version, tag, name, sig, sha, upstream] = process.argv.slice(1);
const date = new Date().toISOString().replace(/\.\d+Z$/, "Z");
process.stdout.write(JSON.stringify({
  version,
  notes: `## [${version}] — ${date.slice(0, 10)}\n\nFork build of ${sha}, upstream ${upstream}.`,
  pub_date: date,
  platforms: { "darwin-aarch64": {
    signature: sig,
    url: `https://github.com/mimen/rbxport/releases/download/${tag}/${name}.app.tar.gz`,
  } },
}, null, 2) + "\n");
' "$version" "$tag" "$name" "$(cat "$out/$name.app.tar.gz.sig")" "$sha" "$upstream_tag" > "$out/latest.json"

if $dry_run; then
  echo "release: dry run, built $version from $sha into $out"
  exit 0
fi

gh release create "$tag" -R "$REPO" --target "$sha" --latest \
  --title "rbxport (Fork) $version" \
  --notes "Fork build of $sha, upstream $upstream_tag." \
  "$out/$name.dmg" "$out/$name.app.tar.gz" "$out/$name.app.tar.gz.sig" "$out/latest.json"
for _ in 1 2 3 4 5 6; do
  curl -fsSL "https://github.com/$REPO/releases/latest/download/latest.json" | grep -q "\"$version\"" && {
    echo "release: https://github.com/$REPO/releases/tag/$tag"
    exit 0
  }
  sleep 10
done
echo "release: published $tag, but releases/latest/download/latest.json does not name $version" >&2
exit 1

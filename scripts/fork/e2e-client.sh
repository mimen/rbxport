#!/usr/bin/env bash
# Server mode end to end with a RELEASE client: launches the built app as a client of a stub
# host on this machine's Tailscale name. With the stub stopped, the client must log that it
# cannot reach the host within seconds. Once the stub starts, without a restart, the client
# must say it reached the host again and the stub must receive the client's invoke (with its
# token) and its events WebSocket. A debug client runs at
# http://localhost:1420, where WebKit allows what a release page at tauri://localhost
# blocks as mixed content, so only a release build proves the client reaches a host.
#
#   scripts/fork/e2e-client.sh <path to "rbxport (Fork).app">
#
# Runs on the Mac Mini, which has no rekordbox library, in a logged-in GUI session.
set -euo pipefail

app="${1:?usage: e2e-client.sh <app>}"
version="$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")"
BUNDLE_ID=com.mimen.rbxport.fork
PORT=48659
[[ -e "$HOME/Library/Pioneer/rekordbox" ]] && { echo "e2e: this machine has a rekordbox library; run it on the Mini" >&2; exit 1; }

tailscale=$(command -v tailscale || echo /Applications/Tailscale.app/Contents/MacOS/Tailscale)
self="$("$tailscale" status --self --peers=false --json 2>/dev/null)"
name="$(jq -r '.Self.DNSName | rtrimstr(".")' <<<"$self")"
ip="$(jq -r '.Self.TailscaleIPs[] | select(test("^100\\."))' <<<"$self")"
[[ "$name" == *.ts.net && -n "$ip" ]] || { echo "e2e: no Tailscale name on this machine" >&2; exit 1; }

dir="$(mktemp -d)"
token="e2e$(uuidgen | tr -d -)"
cat >"$dir/host.py" <<'EOF'
import http.server, json, sys
class Host(http.server.BaseHTTPRequestHandler):
    def answer(self, status, body=b""):
        self.send_response(status)
        for k in ("Origin", "Methods", "Headers"): self.send_header(f"Access-Control-Allow-{k}", "*")
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_OPTIONS(self): self.answer(204)
    def do_GET(self):
        if self.path == "/version": self.answer(200, json.dumps({"version": sys.argv[3]}).encode())
        else: self.answer(404)
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length") or 0))
        self.answer(200, b"null")
    def log_message(self, *_):
        print(self.command, self.path.split("?")[0], self.headers.get("Authorization"), self.headers.get("Upgrade"), flush=True)
http.server.ThreadingHTTPServer((sys.argv[1], int(sys.argv[2])), Host).serve_forever()
EOF
cat >"$dir/seed.py" <<'EOF'
import glob, os, sqlite3, sys
paths = glob.glob(os.path.expanduser(f"~/Library/WebKit/{sys.argv[1]}/WebsiteData/Default/*/*/LocalStorage/localstorage.sqlite3"))
if not paths: sys.exit("no localStorage yet")
db = sqlite3.connect(paths[0])
key = "rbxfork.remoteHost"
if sys.argv[2] == "get":
    row = db.execute("select value from ItemTable where key=?", (key,)).fetchone()
    print(bytes(row[0]).decode("utf-16-le") if row else "")
elif sys.argv[2] == "set" and sys.argv[3]:
    db.execute("insert into ItemTable values(?, ?)", (key, sys.argv[3].encode("utf-16-le")))
else:
    db.execute("delete from ItemTable where key=?", (key,))
db.commit()
EOF
seed() { python3 -I "$dir/seed.py" "$BUNDLE_ID" "$@"; }
quit() {
  osascript -e "quit app id \"$BUNDLE_ID\"" >/dev/null 2>&1 || true
  for _ in $(seq 20); do pgrep -qf 'rbxport \(Fork\)\.app/Contents/MacOS/' || return 0; sleep 0.5; done
  echo "e2e: the app did not quit" >&2
  exit 1
}

quit
if ! seed get >/dev/null 2>&1; then
  # WebKit creates the store on the first launch.
  open -n "$app"; sleep 8; quit
fi
saved="$(seed get)"
host_pid=
trap 'quit; seed set "$saved"; if [[ -n "$host_pid" ]]; then kill $host_pid; wait $host_pid 2>/dev/null || true; fi; rm -rf "$dir"' EXIT

# Within a few seconds of `seconds`, or fail with `what`.
wait_for() {
  local seconds="$1" what="$2"; shift 2
  for _ in $(seq $((seconds * 2))); do "$@" && return 0; sleep 0.5; done
  echo "e2e: FAIL, $what" >&2
  exit 1
}
logged() { cat "$dir"/logs/rbxport* 2>/dev/null | grep -q "$1"; }

seed set "{\"url\":\"http://$name:$PORT\",\"token\":\"$token\"}"
started=$SECONDS
open -n --env RBXPORT_LOG_DIR="$dir/logs" "$app"
wait_for 12 "the client never said it could not reach the stopped host" logged "Can't reach the library on $name (refused)"
echo "e2e: with the host stopped, the client showed it unreachable after $((SECONDS - started)) s"

python3 -I "$dir/host.py" "$ip" "$PORT" "$version" >"$dir/host.log" 2>&1 &
host_pid=$!
started=$SECONDS
wait_for 30 "the client did not recover once the host started" logged "Reached the library on $name again."
echo "e2e: the client recovered $((SECONDS - started)) s after the host started"
reached() {
  grep -q "^POST /invoke/[a-z_]* Bearer $token " "$dir/host.log" && grep -q "^GET /events .* websocket" "$dir/host.log"
}
wait_for 30 "no invoke with the token and events WebSocket reached the host" reached
echo "e2e: the stub host at http://$name:$PORT saw:"
sort "$dir/host.log" | uniq -c | sed "s/$token/<token>/" | head -20
echo "e2e: pass, a release client showed the host unreachable and then reached it"

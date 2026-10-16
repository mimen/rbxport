/**
 * Runs before the first render in every window. With a remote host set, it
 * marks the document so `remote.css` hides what only the host can do, and
 * opens the main window on the browser layout, since the decks play there.
 */
import { remoteHost } from "@/ipc/fork/remote";
import "./remote.css";

const SESSION_KEY = "rbl.session";

const host = remoteHost();
if (host) {
  const name = new URL(host.url).hostname;
  document.documentElement.dataset.rbxforkRemote = name;
  document.documentElement.style.setProperty("--rbxfork-badge", JSON.stringify(`Library on ${name.split(".")[0] ?? name}`));
  try {
    const session = JSON.parse(localStorage.getItem(SESSION_KEY) ?? "{}") as Record<string, unknown>;
    if (session.layout !== "browser") localStorage.setItem(SESSION_KEY, JSON.stringify({ ...session, layout: "browser" }));
  } catch {
    // An unreadable session falls back to upstream's defaults.
  }
}

if ("__TAURI_INTERNALS__" in window) {
  void import("@tauri-apps/api/event").then(({ listen }) => listen("rbxfork:reload", () => window.location.reload()));
}

/**
 * Runs before the first render in every window. With a remote host set, it
 * marks the document so `remote.css` hides what only the host can do, and
 * opens the main window on the browser layout, since the decks play there.
 * It holds the render until the forward to the host is up, and in the main
 * window shows `unreachable.ts`'s notice while the host does not answer.
 */
import { getCurrentWindow } from "@tauri-apps/api/window";

import { reachHost, remoteHost, watchHost } from "@/ipc/fork/remote";
import { logStatus, showUnreachable } from "./unreachable";
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
  // The first render asks for artwork URLs, which name the local forward's port.
  await reachHost(host).catch(() => undefined);
  if ("__TAURI_INTERNALS__" in window && getCurrentWindow().label === "main") {
    // The host answers again: the library view starts over, as after a change of host.
    const link = watchHost(() => void logStatus(false, `Reached the library on ${name} again.`).finally(() => window.location.reload()));
    if (link) showUnreachable(link, name);
  }
}

if ("__TAURI_INTERNALS__" in window) {
  void import("@tauri-apps/api/event").then(({ listen }) => listen("rbxfork:reload", () => window.location.reload()));
}

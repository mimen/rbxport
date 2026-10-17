/**
 * Server mode's client side: with a remote host set, commands and events go
 * to the host's library server instead of this machine's backend.
 *
 * Upstream reaches this through `tauriInvoke` (invoke.ts), `subscribe`
 * (client.ts) and `artworkUrl` (artwork.ts). With no host set every hook
 * hands back upstream's own behaviour.
 */
import type { InvokeArgs } from "@tauri-apps/api/core";
import { convertFileSrc } from "@tauri-apps/api/core";
import { IpcError } from "../invoke";

export interface RemoteHost {
  /** `http://<tailscale name>:<port>`, no trailing slash. The page reaches it through `reachHost`. */
  url: string;
  token: string;
}

const HOST_KEY = "rbxfork.remoteHost";

/** The header the host checks the build against; must match `server.rs`. */
const VERSION_HEADER = "x-rbxport-version";

export const BUILD_VERSION = typeof __APP_VERSION__ === "string" ? __APP_VERSION__ : "dev";

function parseHost(raw: unknown): RemoteHost | null {
  if (typeof raw !== "object" || raw === null) return null;
  const { url, token } = raw as Record<string, unknown>;
  if (typeof url !== "string" || typeof token !== "string") return null;
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "http:") return null;
    return { url: parsed.origin, token };
  } catch {
    return null;
  }
}

export function remoteHost(): RemoteHost | null {
  try {
    const raw = localStorage.getItem(HOST_KEY);
    return raw === null ? null : parseHost(JSON.parse(raw));
  } catch {
    return null;
  }
}

export function setRemoteHost(host: RemoteHost | null): void {
  if (host === null) localStorage.removeItem(HOST_KEY);
  else localStorage.setItem(HOST_KEY, JSON.stringify(host));
}

/**
 * Where a command runs on a client. Unlisted commands go to the host.
 *
 * - `local`: this machine's own window, menus, updates, scripting, and the
 *   audio engine that the hidden decks would use.
 * - `hostOnly`: needs a file on this machine, a stick or a CDJ at the host,
 *   or plays through the host's speakers. Refused with `HOST_ONLY_NOTE`.
 */
export const ROUTES: Readonly<Record<string, "local" | "hostOnly">> = {
  app_diagnostics: "local",
  app_version: "local",
  check_for_update: "local",
  ready_update: "local",
  download_update: "local",
  restart_to_update: "local",
  open_log: "local",
  open_log_window: "local",
  log_lines: "local",
  log_status_message: "local",
  open_log_folder: "local",
  save_log_text: "local",
  open_url: "local",
  open_preferences: "local",
  open_report_window: "local",
  report_attachment: "local",
  open_report_attachment: "local",
  script_ready: "local",
  script_reply: "local",
  script_preferences: "local",
  set_history_menu: "local",
  set_menu_labels: "local",
  set_menu_enabled: "local",
  remember_screen_assets: "local",
  audio_devices: "local",
  set_audio_device: "local",
  set_audio_config: "local",
  set_master_level: "local",
  master_limiter: "local",
  set_master_limiter: "local",
  set_metronome: "local",
  set_crossfade: "local",
  set_channel_band: "local",
  set_channel_kill: "local",
  set_channel_trim: "local",
  set_eq_curve: "local",
  deck_state: "local",
  deck_unload: "local",
  deck_play: "local",
  deck_play_after: "local",
  deck_pause: "local",
  deck_seek: "local",
  deck_move: "local",
  deck_set_loop: "local",
  deck_loop_active: "local",
  deck_clear_loop: "local",
  deck_scrub_begin: "local",
  deck_scrub_to: "local",
  deck_scrub_end: "local",
  deck_tempo: "local",
  deck_master_tempo: "local",
  deck_metronome: "local",
  deck_metronome_grid: "local",
  deck_key_shift: "local",

  deck_load: "hostOnly",
  drag_tracks: "hostOnly",
  dropped_file_paths: "hostOnly",
  reveal_track: "hostOnly",
  open_sync_window: "hostOnly",
  open_backup_directory: "hostOnly",
  set_backup_directory: "hostOnly",
  start_link_export: "hostOnly",
  link_load_track: "hostOnly",
  link_set_master: "hostOnly",
  link_nudge_master: "hostOnly",
  link_take_master_tempo: "hostOnly",
  export_playlist: "hostOnly",
  export_tracks_to_device: "hostOnly",
  sync_devices: "hostOnly",
  eject_device: "hostOnly",
  format_device: "hostOnly",
  import_usb: "hostOnly",
  device_playlist_edit: "hostOnly",
  device_playlist_import: "hostOnly",
  ensure_device_library: "hostOnly",
  write_device_defaults: "hostOnly",
  save_device_settings: "hostOnly",
  import_files: "hostOnly",
  import_files_slice: "hostOnly",
  list_import_files: "hostOnly",
  import_folder_playlist: "hostOnly",
  import_xml: "hostOnly",
  import_itunes: "hostOnly",
  import_itunes_selected: "hostOnly",
  itunes_library_at: "hostOnly",
  export_xml: "hostOnly",
  export_playlist_file: "hostOnly",
  export_loop_wav: "hostOnly",
  relocate_track: "hostOnly",
  auto_relocate: "hostOnly",
  add_artwork: "hostOnly",
  add_playlist_artwork: "hostOnly",
};

/** Events this machine raises for itself; every other event is the host's. */
const LOCAL_EVENTS: ReadonlySet<string> = new Set([
  "menu", "preferences:reset", "script:request", "update:progress", "rbxfork:reload",
  "deck:tick", "deck:meters", "deck:loaded", "deck:error", "deck:reset",
]);

export const HOST_ONLY_NOTE =
  "With a remote host set, the decks, USB sticks, PRO DJ LINK, and importing or exporting files run on the host machine.";

type Invoke = <T>(command: string, args?: InvokeArgs) => Promise<T>;

function hostName(host: RemoteHost): string {
  return new URL(host.url).hostname;
}

const forwards = new Map<string, Promise<string>>();
const forwarded = new Map<string, string>();

/**
 * Where this page reaches `host`: `http://127.0.0.1:<port>`, which the fork's
 * Rust side pipes to the host (`forward.rs`). A release window's page is
 * `tauri://localhost`, a secure origin, so WebKit blocks `http://` and `ws://`
 * to another machine as mixed content and allows them to 127.0.0.1.
 */
export function reachHost(host: RemoteHost): Promise<string> {
  let base = forwards.get(host.url);
  if (base === undefined) {
    const { hostname, port } = new URL(host.url);
    base = fetch(`${convertFileSrc("", "rbxfork")}forward`, {
      method: "POST",
      headers: { "content-type": "text/plain" },
      body: JSON.stringify({ host: hostname, port: Number(port || 80) }),
    }).then(async (response) => {
      const body: unknown = await response.json();
      if (!response.ok) throw new IpcError(body);
      const local = `http://127.0.0.1:${(body as { port: number }).port}`;
      forwarded.set(host.url, local);
      return local;
    });
    forwards.set(host.url, base);
    base.catch(() => forwards.delete(host.url));
  }
  return base;
}

async function hostFetch(host: RemoteHost, path: string, init: RequestInit = {}): Promise<Response> {
  try {
    return await fetch(`${await reachHost(host)}${path}`, {
      ...init,
      headers: {
        authorization: `Bearer ${host.token}`,
        "content-type": "application/json",
        [VERSION_HEADER]: BUILD_VERSION,
      },
    });
  } catch (error) {
    if (error instanceof IpcError) throw error;
    throw new IpcError({ kind: "remote", message: `Could not reach the library host ${hostName(host)}.` });
  }
}

async function refusal(response: Response): Promise<IpcError> {
  const body: unknown = await response.json().catch(() => null);
  return new IpcError(body ?? { kind: "remote", message: `The library host answered ${response.status}.` });
}

async function callHost<T>(host: RemoteHost, command: string, args?: InvokeArgs): Promise<T> {
  const response = await hostFetch(host, `/invoke/${command}`, { method: "POST", body: JSON.stringify(args ?? {}) });
  if (!response.ok) throw await refusal(response);
  if (response.headers.get("content-type")?.includes("json")) return (await response.json()) as T;
  return (await response.arrayBuffer()) as T;
}

/** `tauriInvoke`'s function, routed per `ROUTES` when a host is set. */
export function withRemote(local: Invoke): Invoke {
  const host = remoteHost();
  if (host === null) return local;
  return async <T>(command: string, args?: InvokeArgs): Promise<T> => {
    switch (ROUTES[command]) {
      case "local": return local<T>(command, args);
      case "hostOnly": throw new IpcError({ kind: "hostOnly", message: HOST_ONLY_NOTE });
      default: return callHost<T>(host, command, args);
    }
  };
}

/** Whether `subscribe` should take this event from the host. */
export function isHostEvent(event: string): boolean {
  return remoteHost() !== null && !LOCAL_EVENTS.has(event);
}

type Listener = (payload: unknown) => void;

/** One WebSocket per window, reconnecting, re-sending its subscriptions. */
const hostEvents = (() => {
  const listeners = new Map<string, Set<Listener>>();
  let socket: WebSocket | null = null;
  let started = false;
  let retryMs = 500;

  const send = (message: object) => {
    if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(message));
  };

  const retry = (host: RemoteHost) => {
    socket = null;
    setTimeout(() => connect(host), retryMs);
    retryMs = Math.min(retryMs * 2, 10_000);
  };

  const connect = (host: RemoteHost) => {
    void reachHost(host).then((base) => {
      const url = new URL("/events", base);
      url.protocol = "ws:";
      url.searchParams.set("token", host.token);
      url.searchParams.set("v", BUILD_VERSION);
      const opened = new WebSocket(url);
      socket = opened;
      opened.onopen = () => {
        retryMs = 500;
        for (const event of listeners.keys()) send({ listen: event });
      };
      opened.onmessage = (message) => {
        const { event, payload } = JSON.parse(String(message.data)) as { event: string; payload: unknown };
        for (const listener of listeners.get(event) ?? []) listener(payload);
      };
      opened.onclose = () => retry(host);
    }).catch(() => retry(host));
  };

  return (host: RemoteHost, event: string, listener: Listener): (() => void) => {
    let set = listeners.get(event);
    if (!set) {
      set = new Set();
      listeners.set(event, set);
      send({ listen: event });
    }
    set.add(listener);
    if (!started) {
      started = true;
      connect(host);
    }
    return () => {
      set.delete(listener);
      if (set.size === 0 && listeners.get(event) === set) {
        listeners.delete(event);
        send({ unlisten: event });
      }
    };
  };
})();

export function remoteSubscribe<T>(event: string, listener: (payload: T) => void): () => void {
  const host = remoteHost();
  if (host === null) return () => {};
  return hostEvents(host, event, listener as Listener);
}

/** Synchronous for `<img src>`; `boot.ts` waits for `reachHost` before the first render. */
function mediaUrl(kind: "artwork" | "audio", trackId: string): string | undefined {
  const host = remoteHost();
  const base = host && forwarded.get(host.url);
  if (!host || !base || !/^\d+$/.test(trackId)) return undefined;
  return `${base}/${kind}/${trackId}?token=${encodeURIComponent(host.token)}`;
}

export const remoteArtworkUrl = (trackId: string) => mediaUrl("artwork", trackId);
export const remoteAudioUrl = (trackId: string) => mediaUrl("audio", trackId);

/** Checks a host before it is saved: reachable, the token accepted, the same build. */
export async function probeHost(host: RemoteHost): Promise<void> {
  const response = await hostFetch(host, "/version");
  if (!response.ok) throw await refusal(response);
  const { version } = (await response.json()) as { version: string };
  if (version !== BUILD_VERSION) {
    throw new IpcError({
      kind: "versionMismatch",
      message: `The host runs rbxport ${version}, and this app is ${BUILD_VERSION}. Install the same version on both machines.`,
    });
  }
}

/** This machine's own server, as the host's Preferences pane shows it. */
export interface ServerStatus {
  serve: boolean;
  port: number;
  token: string;
  version: string;
  /** What a client enters: `http://<Tailscale name>:<port>`. */
  address: string | null;
  listening: string[];
  problems: string[];
}

async function serverRequest(init?: RequestInit): Promise<ServerStatus> {
  const response = await fetch(`${convertFileSrc("", "rbxfork")}server`, init);
  return (await response.json()) as ServerStatus;
}

export const serverStatus = () => serverRequest();

export const changeServer = (change: { serve?: boolean; newToken?: boolean }) =>
  serverRequest({ method: "POST", headers: { "content-type": "text/plain" }, body: JSON.stringify(change) });

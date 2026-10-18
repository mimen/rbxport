import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { IpcError } from "../invoke";
import { BUILD_VERSION, checkHost, isHostEvent, reachHost, remoteArtworkUrl, remoteHost, setRemoteHost, watchHost, withRemote } from "./remote";

const store = new Map<string, string>();
vi.stubGlobal("localStorage", {
  getItem: (key: string) => store.get(key) ?? null,
  setItem: (key: string, value: string) => store.set(key, value),
  removeItem: (key: string) => store.delete(key),
});
vi.stubGlobal("window", { __TAURI_INTERNALS__: { convertFileSrc: (path: string, scheme: string) => `${scheme}://localhost/${path}` } });
const FORWARD = "rbxfork://localhost/forward";
const REACH = "rbxfork://localhost/reach";
const fetch = vi.fn<(url: string, init?: RequestInit) => Promise<Response>>();
const forward = (url: string, init?: RequestInit) =>
  url === FORWARD ? Promise.resolve(Response.json({ port: 50123 })) : Promise.reject(new Error(`unexpected fetch ${url} ${init?.method}`));
fetch.mockImplementation(forward);
vi.stubGlobal("fetch", fetch);
/** The host's next answer; the forward is answered by `forward`. */
const hostAnswers = (answer: () => Promise<Response>) =>
  fetch.mockImplementation((url, init) => (url === FORWARD ? forward(url, init) : answer()));
const ran: string[] = [];
const local = <T,>(command: string): Promise<T> => {
  ran.push(command);
  return Promise.resolve(`local ${command}` as T);
};

beforeEach(() => setRemoteHost({ url: "http://host.example.ts.net:48650/", token: "t0k" }));
afterEach(() => {
  store.clear();
  fetch.mockReset();
  fetch.mockImplementation(forward);
  ran.length = 0;
});

describe("withRemote", () => {
  it("is upstream's own invoke with no host set", () => {
    setRemoteHost(null);
    expect(withRemote(local)).toBe(local);
    expect(isHostEvent("library:changed")).toBe(false);
  });

  it("sends a library command to the host with the token and build", async () => {
    hostAnswers(() => Promise.resolve(Response.json([{ id: "1" }])));
    await expect(withRemote(local)("playlist_tree", { a: 1 })).resolves.toEqual([{ id: "1" }]);
    const [url, init] = fetch.mock.calls.find(([called]) => called !== FORWARD) ?? [];
    expect(url).toBe("http://127.0.0.1:50123/invoke/playlist_tree");
    expect(init?.body).toBe('{"a":1}');
    expect(init?.headers).toMatchObject({ authorization: "Bearer t0k", "x-rbxport-version": BUILD_VERSION });
    expect(ran).toEqual([]);
  });

  it("hands back raw bytes as an ArrayBuffer", async () => {
    hostAnswers(() => Promise.resolve(new Response(new Uint8Array([1, 2, 3]), { headers: { "content-type": "application/octet-stream" } })));
    const bytes = await withRemote(local)<ArrayBuffer>("track_waveform", {});
    expect([...new Uint8Array(bytes)]).toEqual([1, 2, 3]);
  });

  it("keeps the host's own refusal, kind and all", async () => {
    hostAnswers(() => Promise.resolve(Response.json({ kind: "readOnly", message: "rekordbox is running." }, { status: 422 })));
    const call = withRemote(local)("create_playlist", { name: "x" });
    await expect(call).rejects.toBeInstanceOf(IpcError);
    await expect(call).rejects.toMatchObject({ kind: "readOnly", message: "rekordbox is running." });
  });

  it("runs window and update commands here, and refuses what only the host can do", async () => {
    await expect(withRemote(local)("check_for_update")).resolves.toBe("local check_for_update");
    await expect(withRemote(local)("eject_device", { path: "/Volumes/X" })).rejects.toMatchObject({ kind: "hostOnly" });
    expect(fetch).not.toHaveBeenCalled();
  });

  it("says when the host cannot be reached", async () => {
    hostAnswers(() => Promise.reject(new TypeError("Load failed")));
    await expect(withRemote(local)("playlist_tree")).rejects.toMatchObject({ kind: "remote", message: expect.stringContaining("host.example.ts.net") });
  });
});

describe("the forward", () => {
  it("asks for one local port per host, by its Tailscale name and port", async () => {
    const host = remoteHost();
    if (!host) throw new Error("no host set");
    await expect(reachHost(host)).resolves.toBe("http://127.0.0.1:50123");
    await reachHost(host);
    const asked = fetch.mock.calls.filter(([url]) => url === FORWARD);
    expect(asked.length).toBeLessThanOrEqual(1);
    expect(asked.every(([, init]) => init?.body === '{"host":"host.example.ts.net","port":48650}')).toBe(true);
  });

  it("passes on the forward's refusal", async () => {
    const host = { url: "http://not-a-tailnet.example:48650", token: "t" };
    fetch.mockImplementation(() => Promise.resolve(Response.json({ kind: "remote", message: "not a Tailscale name" }, { status: 400 })));
    await expect(reachHost(host)).rejects.toMatchObject({ kind: "remote", message: "not a Tailscale name" });
  });
});

describe("media and events", () => {
  it("loads artwork from the host, and only for library ids", async () => {
    const host = remoteHost();
    if (host) await reachHost(host);
    expect(remoteArtworkUrl("42")).toBe("http://127.0.0.1:50123/artwork/42?token=t0k");
    expect(remoteArtworkUrl("file:/etc/passwd")).toBeUndefined();
  });

  it("takes library events from the host and window events from here", () => {
    expect(isHostEvent("library:changed")).toBe(true);
    expect(isHostEvent("menu")).toBe(false);
  });
});

describe("checking the host", () => {
  const host = { url: "http://host.example.ts.net:48650", token: "t0k" };
  /** The forward's check answers `reached`, and the host's `/version` answers `version`. */
  const answers = (reached: unknown, version?: () => Promise<Response>) =>
    fetch.mockImplementation((url, init) => {
      if (url === FORWARD) return forward(url, init);
      if (url === REACH) return Promise.resolve(Response.json(reached));
      return version ? version() : Promise.reject(new Error(`unexpected fetch ${url}`));
    });

  it("passes when the host answers with this build", async () => {
    answers(null, () => Promise.resolve(Response.json({ version: BUILD_VERSION })));
    await expect(checkHost(host)).resolves.toEqual({ ok: true });
    const reach = fetch.mock.calls.find(([url]) => url === REACH);
    expect(reach?.[1]?.body).toBe('{"host":"host.example.ts.net","port":48650}');
  });

  it("names a refused or silent connection as the forward saw it", async () => {
    answers({ reason: "timedOut", message: "No answer in 3 seconds." });
    await expect(checkHost(host)).resolves.toEqual({ ok: false, reason: "timedOut", message: "No answer in 3 seconds." });
    expect(fetch.mock.calls.some(([url]) => String(url).includes("/version"))).toBe(false);
  });

  it("names a refused token and another build", async () => {
    answers(null, () => Promise.resolve(Response.json({ kind: "unauthorized", message: "The host refused the token." }, { status: 401 })));
    await expect(checkHost(host)).resolves.toMatchObject({ ok: false, reason: "tokenRefused" });
    answers(null, () => Promise.resolve(Response.json({ version: "0.0.1" })));
    await expect(checkHost(host)).resolves.toMatchObject({ ok: false, reason: "versionMismatch", message: expect.stringContaining("0.0.1") });
  });

  it("checks when a request to the host fails", async () => {
    answers(null, () => Promise.resolve(Response.json({ version: BUILD_VERSION })));
    const link = watchHost(() => {});
    await vi.waitFor(() => expect(link?.state().status).toBe("connected"));
    answers({ reason: "refused", message: "Connection refused" });
    await expect(withRemote(local)("playlist_tree")).rejects.toMatchObject({ kind: "remote" });
    await vi.waitFor(() => expect(link?.state()).toMatchObject({ status: "unreachable", reason: "refused" }));
  });
});

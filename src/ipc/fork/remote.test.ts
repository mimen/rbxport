import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { IpcError } from "../invoke";
import { BUILD_VERSION, isHostEvent, remoteArtworkUrl, setRemoteHost, withRemote } from "./remote";

const store = new Map<string, string>();
vi.stubGlobal("localStorage", {
  getItem: (key: string) => store.get(key) ?? null,
  setItem: (key: string, value: string) => store.set(key, value),
  removeItem: (key: string) => store.delete(key),
});
const fetch = vi.fn<(url: string, init?: RequestInit) => Promise<Response>>();
vi.stubGlobal("fetch", fetch);
const ran: string[] = [];
const local = <T,>(command: string): Promise<T> => {
  ran.push(command);
  return Promise.resolve(`local ${command}` as T);
};

beforeEach(() => setRemoteHost({ url: "http://host.example.ts.net:48650/", token: "t0k" }));
afterEach(() => {
  store.clear();
  fetch.mockReset();
  ran.length = 0;
});

describe("withRemote", () => {
  it("is upstream's own invoke with no host set", () => {
    setRemoteHost(null);
    expect(withRemote(local)).toBe(local);
    expect(isHostEvent("library:changed")).toBe(false);
  });

  it("sends a library command to the host with the token and build", async () => {
    fetch.mockResolvedValueOnce(Response.json([{ id: "1" }]));
    await expect(withRemote(local)("playlist_tree", { a: 1 })).resolves.toEqual([{ id: "1" }]);
    const [url, init] = fetch.mock.calls[0] ?? [];
    expect(url).toBe("http://host.example.ts.net:48650/invoke/playlist_tree");
    expect(init?.body).toBe('{"a":1}');
    expect(init?.headers).toMatchObject({ authorization: "Bearer t0k", "x-rbxport-version": BUILD_VERSION });
    expect(ran).toEqual([]);
  });

  it("hands back raw bytes as an ArrayBuffer", async () => {
    fetch.mockResolvedValueOnce(new Response(new Uint8Array([1, 2, 3]), { headers: { "content-type": "application/octet-stream" } }));
    const bytes = await withRemote(local)<ArrayBuffer>("track_waveform", {});
    expect([...new Uint8Array(bytes)]).toEqual([1, 2, 3]);
  });

  it("keeps the host's own refusal, kind and all", async () => {
    fetch.mockResolvedValueOnce(Response.json({ kind: "readOnly", message: "rekordbox is running." }, { status: 422 }));
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
    fetch.mockRejectedValueOnce(new TypeError("Load failed"));
    await expect(withRemote(local)("playlist_tree")).rejects.toMatchObject({ kind: "remote", message: expect.stringContaining("host.example.ts.net") });
  });
});

describe("media and events", () => {
  it("loads artwork from the host, and only for library ids", () => {
    expect(remoteArtworkUrl("42")).toBe("http://host.example.ts.net:48650/artwork/42?token=t0k");
    expect(remoteArtworkUrl("file:/etc/passwd")).toBeUndefined();
  });

  it("takes library events from the host and window events from here", () => {
    expect(isHostEvent("library:changed")).toBe(true);
    expect(isHostEvent("menu")).toBe(false);
  });
});

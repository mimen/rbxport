/**
 * Server mode's contract: every command a client sends exists on the host.
 * The host serves exactly the `generate_handler!` list in lib.rs
 * (`src-tauri/src/fork/commands.rs`), so that list is the server side here.
 */
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { ROUTES } from "@/ipc/fork/remote";

const root = join(import.meta.dirname, "..", "..");

function serverCommands(): Set<string> {
  const lib = readFileSync(join(root, "src-tauri/src/lib.rs"), "utf8");
  const start = lib.indexOf("generate_handler![") + "generate_handler![".length;
  const body = lib.slice(start, lib.indexOf("]", start));
  return new Set(body.split("\n")
    .map((line) => (line.split("//")[0] ?? "").trim().replace(/,$/, ""))
    .filter(Boolean)
    .map((entry) => entry.split("::").pop() ?? entry));
}

function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sources(path);
    return /\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name) && !entry.name.startsWith("backend-mock") ? [path] : [];
  });
}

function clientCommands(): Set<string> {
  const names = new Set<string>();
  for (const file of sources(join(root, "src"))) {
    const text = readFileSync(file, "utf8");
    for (const match of text.matchAll(/\b(?:invoke|importReplacing)(?:<[^()]*>)?\(\s*"([a-z0-9_]+)"/g)) names.add(match[1] ?? "");
  }
  return names;
}

describe("server mode contract", () => {
  const server = serverCommands();
  const client = clientCommands();

  it("reads both sides", () => {
    expect(server.size).toBeGreaterThan(150);
    expect(client.size).toBeGreaterThan(150);
    expect(client).toContain("create_playlist");
  });

  it("serves every command the client sends", () => {
    expect([...client].filter((name) => !server.has(name))).toEqual([]);
  });

  it("routes only commands that exist", () => {
    expect(Object.keys(ROUTES).filter((name) => !server.has(name))).toEqual([]);
  });
});

#!/usr/bin/env node
// Prints the fork's CSP as a `tauri build --config` overlay, derived from upstream's
// tauri.conf.json at build time so upstream's own CSP changes carry over:
//   - report.rbxport.com dropped: bug reports go to upstream's worker.
//   - server mode: the rbxfork: scheme (Preferences › Server and the forward), and
//     127.0.0.1, where the forward to a library host listens, for invoke, events,
//     artwork and audio. The page never reaches the host directly: from
//     tauri://localhost WebKit blocks http:// and ws:// to another machine.
import { readFileSync } from "node:fs";

const FORWARD = ["http://127.0.0.1:*"];
const ADD = {
  "connect-src": ["rbxfork:", "http://rbxfork.localhost", ...FORWARD, "ws://127.0.0.1:*"],
  "img-src": FORWARD,
  "media-src": FORWARD,
};

const conf = new URL("../../src-tauri/tauri.conf.json", import.meta.url);
const upstream = JSON.parse(readFileSync(conf, "utf8")).app.security.csp;
const csp = upstream
  .replaceAll(" https://report.rbxport.com", "")
  .replace(/frame-src\s*;/, "frame-src 'none';")
  .split(";")
  .map((directive) => {
    const name = directive.trim().split(/\s+/)[0];
    return ADD[name] ? `${directive.trimEnd()} ${ADD[name].join(" ")}` : directive;
  })
  .join(";");
if (csp.includes("report.rbxport.com")) throw new Error("report.rbxport.com is still in the CSP");
for (const name of Object.keys(ADD)) if (!csp.includes(`${name} `)) throw new Error(`upstream's CSP has no ${name}`);
process.stdout.write(JSON.stringify({ app: { security: { csp } } }));

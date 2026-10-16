#!/usr/bin/env node
// Prints the fork's CSP as a `tauri build --config` overlay, derived from upstream's
// tauri.conf.json at build time so upstream's own CSP changes carry over:
//   - report.rbxport.com dropped: bug reports go to upstream's worker.
//   - server mode: the rbxfork: scheme (Preferences › Server), and a library host
//     on the tailnet by its MagicDNS name, for invoke, events, artwork and audio.
import { readFileSync } from "node:fs";

const TAILNET = ["http://*.ts.net:*", "https://*.ts.net:*"];
const ADD = {
  "connect-src": ["rbxfork:", "http://rbxfork.localhost", ...TAILNET, "ws://*.ts.net:*", "wss://*.ts.net:*"],
  "img-src": TAILNET,
  "media-src": TAILNET,
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

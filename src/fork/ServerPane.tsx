/**
 * Preferences › Server: serve this machine's library to the fork app on
 * other machines, or browse another machine's library from here.
 */
import { useEffect, useState } from "react";
import { Server } from "lucide-react";

import {
  BUILD_VERSION, changeServer, probeHost, remoteHost, serverStatus, setRemoteHost, type RemoteHost, type ServerStatus,
} from "@/ipc/fork/remote";
import { errorMessage } from "@/lib/errorMessage";
import { Button, Note, Section, Toggle } from "@/views/settings/controls";
import layout from "@/views/settings/PaneLayout.module.css";
import styles from "./ServerPane.module.css";

export const ServerIcon = Server;

/** Every window reloads, so the backend each one holds is made again. */
async function reloadWindows(): Promise<void> {
  const { emit } = await import("@tauri-apps/api/event");
  await emit("rbxfork:reload");
}

function ServeSection() {
  const [status, setStatus] = useState<ServerStatus | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const run = (request: Promise<ServerStatus>) => {
    request.then(setStatus, (e: unknown) => setFailed(errorMessage(e)));
  };
  useEffect(() => run(serverStatus()), []);
  if (status === null) return <Section title="Serve library">{failed ? <Note failed>{failed}</Note> : null}</Section>;
  return (
    <Section title="Serve library">
      <p className={layout.help}>
        Lets the fork app on your other machines browse and edit this library over Tailscale. They need the token below.
      </p>
      <Toggle label="Serve this library" checked={status.serve} onChange={(serve) => run(changeServer({ serve }))} />
      {status.serve && (
        <div className={styles.fields}>
          <label>
            Address
            <input readOnly value={status.address ?? `http://127.0.0.1:${status.port}`} onFocus={(e) => e.currentTarget.select()} />
          </label>
          <label>
            Token
            <input readOnly value={status.token} onFocus={(e) => e.currentTarget.select()} />
          </label>
          <div className={styles.actions}>
            <Button onClick={() => void navigator.clipboard.writeText(status.token)}>Copy token</Button>
            <Button onClick={() => run(changeServer({ newToken: true }))}>New token</Button>
          </div>
          <Note>Listening on {status.listening.join(", ") || "nothing"}. Version {status.version}.</Note>
        </div>
      )}
      {status.problems.map((problem) => <Note key={problem} failed>{problem}</Note>)}
    </Section>
  );
}

function RemoteSection() {
  const saved = remoteHost();
  const [url, setUrl] = useState(saved?.url ?? "");
  const [token, setToken] = useState(saved?.token ?? "");
  const [state, setState] = useState<{ busy: boolean; message: string | null; failed: boolean }>(
    { busy: false, message: null, failed: false },
  );
  const connect = () => {
    let host: RemoteHost;
    try {
      host = { url: new URL(url.trim()).origin, token: token.trim() };
    } catch {
      setState({ busy: false, message: "Enter the host's address, for example http://milads-mac-mini.taild31e9a.ts.net:48650.", failed: true });
      return;
    }
    setState({ busy: true, message: "Connecting…", failed: false });
    probeHost(host)
      .then(() => {
        setRemoteHost(host);
        return reloadWindows();
      })
      .catch((e: unknown) => setState({ busy: false, message: errorMessage(e), failed: true }));
  };
  const disconnect = () => {
    setRemoteHost(null);
    void reloadWindows();
  };
  return (
    <Section title="Remote host">
      <p className={layout.help}>
        Browse and edit the library on another machine that serves it. The decks, USB sticks, PRO DJ LINK, and importing
        or exporting files stay with that machine. Both machines must run the same version ({BUILD_VERSION}).
      </p>
      <div className={styles.fields}>
        <label>
          Address
          <input value={url} placeholder="http://host.tailnet.ts.net:48650" spellCheck={false}
            onChange={(e) => setUrl(e.currentTarget.value)} />
        </label>
        <label>
          Token
          <input value={token} type="password" spellCheck={false} onChange={(e) => setToken(e.currentTarget.value)} />
        </label>
        <div className={styles.actions}>
          <Button onClick={connect} disabled={state.busy || url.trim() === "" || token.trim() === ""}>
            {saved ? "Reconnect" : "Connect"}
          </Button>
          {saved && <Button onClick={disconnect}>Use this machine&apos;s library</Button>}
        </div>
        {saved && !state.message && <Note>Using the library on {new URL(saved.url).hostname}.</Note>}
        {state.message && <Note failed={state.failed}>{state.message}</Note>}
      </div>
    </Section>
  );
}

export function ServerPane() {
  return (
    <>
      <ServeSection />
      <RemoteSection />
    </>
  );
}

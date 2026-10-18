/**
 * The notice over a client's main window while its library host cannot be
 * reached. It covers upstream's window rather than changing it, and logs
 * each change, so a client's log says why it shows no library.
 * `html[data-rbxfork-link]` names the state for `remote.css` and for tests.
 */
import type { Link, LinkState, Reason } from "@/ipc/fork/link";

const NETWORK_HINT =
  "rbxport may be closed there, the Mac may be asleep, or its firewall may be blocking rbxport " +
  "(System Settings › Network › Firewall › Options).";

function detail(reason: Reason, message: string, name: string): string {
  switch (reason) {
    case "refused": return `${name} refused the connection.`;
    case "timedOut": return `${name} did not answer.`;
    case "tokenRefused": return `${name} refused this Mac's token. Copy it again from Preferences › Server there.`;
    case "versionMismatch": return message;
    case "other": return message;
  }
}

const backend = () => import("@/ipc/client").then(({ getBackend }) => getBackend());

/** A line in this machine's log, beside upstream's status messages. */
export const logStatus = (failed: boolean, text: string) => backend().then((b) => b.logStatusMessage(failed, text));

export function showUnreachable(link: Link, host: string): void {
  const name = host.split(".")[0] ?? host;
  const notice = document.createElement("div");
  notice.id = "rbxfork-unreachable";
  notice.setAttribute("role", "alertdialog");
  notice.setAttribute("aria-labelledby", "rbxfork-unreachable-title");
  notice.innerHTML = `
    <div>
      <h2 id="rbxfork-unreachable-title"></h2>
      <p data-detail></p>
      <p data-hint></p>
      <p data-status></p>
      <div>
        <button type="button" data-retry>Retry now</button>
        <button type="button" data-server>Preferences › Server</button>
      </div>
    </div>`;
  const part = <T extends HTMLElement>(selector: string) => notice.querySelector(selector) as T;
  part("h2").textContent = `Can't reach the library on ${name}.`;
  const retry = part<HTMLButtonElement>("[data-retry]");
  retry.addEventListener("click", () => link.check());
  part("[data-server]").addEventListener("click", () => void backend().then((b) => b.openPreferences("server")));
  document.body.append(notice);

  let logged: Reason | null = null;
  const render = (state: LinkState) => {
    document.documentElement.dataset.rbxforkLink = state.status;
    if (state.status !== "unreachable") {
      logged = null;
      return;
    }
    part("[data-detail]").textContent = detail(state.reason, state.message, name);
    part("[data-hint]").textContent = state.reason === "refused" || state.reason === "timedOut" ? NETWORK_HINT : "";
    part("[data-status]").textContent = state.checking ? "Checking…" : "Trying again in the background.";
    retry.disabled = state.checking;
    if (logged !== state.reason) {
      logged = state.reason;
      void logStatus(true, `Can't reach the library on ${host} (${state.reason}): ${state.message}`);
    }
  };
  render(link.state());
  link.subscribe(render);
}

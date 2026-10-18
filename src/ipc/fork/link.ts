/**
 * Whether a client can reach its library host, for the notice `boot.ts`
 * shows over the main window when it cannot.
 *
 * Only a check decides: the forward's own connection to the host
 * (`rbxfork://localhost/reach`), then the host's `/version` with the token.
 * Traffic only asks for one. A failed request or a dropped events socket
 * runs a check, and an answered request counts as one that passed, so a
 * host that answers checks but fails requests cannot reload the window in
 * a loop.
 */
export type Reason = "refused" | "timedOut" | "tokenRefused" | "versionMismatch" | "other";

export type Outcome = { ok: true } | { ok: false; reason: Reason; message: string };

export type LinkState =
  | { status: "connecting" }
  | { status: "connected" }
  | { status: "unreachable"; reason: Reason; message: string; checking: boolean };

export interface Link {
  state(): LinkState;
  subscribe(listener: (state: LinkState) => void): () => void;
  /** Checks now, unless a check is already running. */
  check(): void;
  /** The host answered a request. */
  answered(): void;
  /** A request to the host failed, so check unless already known unreachable. */
  failed(): void;
}

/** 1 s, 2 s, 4 s and so on between checks, at most 15 s. */
export const retryDelay = (failures: number) => Math.min(1000 * 2 ** (failures - 1), 15_000);

export function createLink(deps: { check: () => Promise<Outcome>; recovered: () => void }): Link {
  let state: LinkState = { status: "connecting" };
  let failures = 0;
  let checking = false;
  let retry: ReturnType<typeof setTimeout> | undefined;
  const listeners = new Set<(state: LinkState) => void>();

  const set = (next: LinkState) => {
    state = next;
    for (const listener of listeners) listener(state);
  };

  const settle = (outcome: Outcome) => {
    clearTimeout(retry);
    if (outcome.ok) {
      failures = 0;
      if (state.status === "connected") return;
      const was = state.status;
      set({ status: "connected" });
      if (was === "unreachable") deps.recovered();
      return;
    }
    failures += 1;
    set({ status: "unreachable", reason: outcome.reason, message: outcome.message, checking: false });
    retry = setTimeout(check, retryDelay(failures));
  };

  const check = () => {
    if (checking) return;
    checking = true;
    if (state.status === "unreachable") set({ ...state, checking: true });
    void deps.check()
      .catch((error: unknown): Outcome => ({ ok: false, reason: "other", message: String(error) }))
      .then((outcome) => {
        checking = false;
        settle(outcome);
      });
  };

  return {
    state: () => state,
    subscribe: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    check,
    answered: () => {
      if (state.status !== "connected") settle({ ok: true });
    },
    // While unreachable, the backoff decides when the next check runs.
    failed: () => {
      if (state.status !== "unreachable") check();
    },
  };
}

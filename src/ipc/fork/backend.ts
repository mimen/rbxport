/**
 * The backend a client sees with a remote host set: upstream's, with the
 * pieces that only make sense at the host taken out, and preview played
 * here from the host's `/audio` rather than through the host's speakers.
 */
import type { Backend, PreviewState } from "../types";
import { HOST_ONLY_NOTE, remoteAudioUrl, remoteHost, remoteSubscribe } from "./remote";

/** Each opens a file dialog on this machine for a path the host cannot read. */
const DIALOGS = [
  "importFiles", "importFolder", "importXml", "importItunes", "chooseItunesLibrary", "exportXml",
  "exportLoopWav", "exportPlaylistFile", "chooseRelocateFile", "pickImage", "pickFolder",
] as const satisfies readonly (keyof Backend)[];

/** How long a preview may wait for audio from the host before it is called failed. */
const STALL_MS = 8000;

function preview(tell: Backend["tell"]): Pick<Backend, "previewPlay" | "previewStop" | "previewState"> {
  const audio = new Audio();
  let track: string | null = null;
  let stall: ReturnType<typeof setTimeout> | undefined;
  let refuseStart: ((error: Error) => void) | null = null;

  // A failure pauses the element, so `previewState` reads not playing. A
  // start still waiting rejects, which the row shows as the preview's error;
  // one already playing has nowhere to show it but a dialog.
  const fail = (message: string) => {
    clearTimeout(stall);
    if (track === null) return;
    audio.pause();
    if (refuseStart) refuseStart(new Error(message));
    else void tell(message, "Preview stopped");
    refuseStart = null;
  };
  const waitForAudio = () => {
    clearTimeout(stall);
    stall = setTimeout(() => fail(`The host sent no audio for ${STALL_MS / 1000} seconds.`), STALL_MS);
  };
  audio.addEventListener("error", () => {
    fail(`The preview could not load from the host${audio.error?.message ? `: ${audio.error.message}` : "."}`);
  });
  audio.addEventListener("waiting", waitForAudio);
  audio.addEventListener("playing", () => clearTimeout(stall));
  audio.addEventListener("pause", () => clearTimeout(stall));

  return {
    previewPlay: async (trackId, positionMs) => {
      const src = remoteAudioUrl(trackId);
      if (src === undefined) throw new Error("This track cannot be previewed from the host.");
      track = trackId;
      audio.src = src;
      audio.currentTime = positionMs / 1000;
      const refused = new Promise<never>((_, reject) => {
        refuseStart = reject;
      });
      waitForAudio();
      try {
        await Promise.race([audio.play(), refused]);
      } finally {
        refuseStart = null;
      }
    },
    previewStop: () => {
      track = null;
      audio.pause();
      return Promise.resolve();
    },
    previewState: (): Promise<PreviewState> => Promise.resolve({
      track,
      playing: track !== null && !audio.paused && !audio.ended,
      positionMs: audio.currentTime * 1000,
      durationMs: Number.isFinite(audio.duration) ? audio.duration * 1000 : 0,
    }),
  };
}

export function remoteBackend(backend: Backend): Backend {
  if (remoteHost() === null) return backend;
  const refuse = async () => {
    await backend.tell(HOST_ONLY_NOTE, "Runs on the host");
    return null;
  };
  return {
    ...backend,
    ...Object.fromEntries(DIALOGS.map((name) => [name, refuse])),
    ...preview((message, title) => backend.tell(message, title)),
    listDevices: () => Promise.resolve([]),
    onDevicesChanged: () => () => {},
    explorerRoots: () => Promise.resolve([]),
    linkPeers: () => Promise.resolve([]),
    onLinkPeers: () => () => {},
    // Upstream listens for this one without `subscribe`.
    onLibraryChanged: (listener) => remoteSubscribe<number>("library:changed", listener),
  };
}

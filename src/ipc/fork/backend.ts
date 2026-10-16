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

function preview(): Pick<Backend, "previewPlay" | "previewStop" | "previewState"> {
  const audio = new Audio();
  let track: string | null = null;
  return {
    previewPlay: async (trackId, positionMs) => {
      const src = remoteAudioUrl(trackId);
      if (src === undefined) throw new Error("This track cannot be previewed from the host.");
      track = trackId;
      audio.src = src;
      audio.currentTime = positionMs / 1000;
      await audio.play();
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
    ...preview(),
    listDevices: () => Promise.resolve([]),
    onDevicesChanged: () => () => {},
    explorerRoots: () => Promise.resolve([]),
    linkPeers: () => Promise.resolve([]),
    onLinkPeers: () => () => {},
    // Upstream listens for this one without `subscribe`.
    onLibraryChanged: (listener) => remoteSubscribe<number>("library:changed", listener),
  };
}

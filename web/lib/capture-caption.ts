export type CaptionKey = "captionReleased";

export type CaptureFrameRef = {
  id?: string;
  provenance: "released";
  shows?: {
    testRun: boolean;
    askPosture: boolean;
    modelName: boolean;
    modelConnected: boolean;
    taskSession: boolean;
  };
};

export type CaptureFrameNote = {
  id: string | null;
  lacksAskPosture: boolean;
  lacksModelName: boolean;
  needsPostureNote: boolean;
};

export type CaptureCaption = {
  caption: CaptionKey | null;
  reason: string | null;
  frames: CaptureFrameNote[];
};

export function frameNote(frame: CaptureFrameRef): CaptureFrameNote {
  const shows = frame.shows;
  const lacksAskPosture = shows ? !shows.askPosture : false;
  const lacksModelName = shows ? shows.modelConnected && !shows.modelName : false;
  return {
    id: frame.id ?? null,
    lacksAskPosture,
    lacksModelName,
    needsPostureNote: lacksAskPosture || lacksModelName,
  };
}

export function captureCaption(frames: readonly CaptureFrameRef[]): CaptureCaption {
  const notes = frames.map(frameNote);
  const none = (reason: string): CaptureCaption => ({ caption: null, reason, frames: notes });
  if (frames.length === 0) return none("no frames");
  const released = frames.map((frame) => frame.shows);
  if (released.some((shows) => !shows)) return none("a released frame carries no record of what it shows");
  if (released.some((shows) => !shows?.taskSession)) {
    return none("a frame from before the task is shown, and the caption is about the task");
  }
  if (!released.some((shows) => shows?.testRun)) {
    return none("no frame shown has a test run in it, and the caption says the tests ran");
  }
  return { caption: "captionReleased", reason: null, frames: notes };
}

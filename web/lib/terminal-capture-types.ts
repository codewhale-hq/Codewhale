export type TerminalCaptureStyle = {
  fg: string;
  bg: string;
  bold?: boolean;
  italic?: boolean;
  dim?: boolean;
  underline?: boolean;
  strike?: boolean;
};

export type TerminalCaptureFrame = {
  file: string;
  state: string;
  provenance: "released";
  cols: number;
  rows: number;
  styles: TerminalCaptureStyle[];
  /** Each row is a list of [text, style index] runs. */
  lines: [string, number][][];
};

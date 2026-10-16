// Types shared by the server and the dashboard. No runtime code, no Node imports.

export type EncoderChoice = 'auto' | 'h264_videotoolbox' | 'libx264';

export interface Destination {
  name: string;
  /** rtmp(s):// server URL, or a local file path (handy for testing) */
  url: string;
  key: string;
  enabled: boolean;
}

export interface Config {
  httpHost: string;
  httpPort: number;
  ingest: { host: string; port: number; app: string; key: string };
  output: {
    width: number;
    height: number;
    fps: number;
    /** kbps */
    videoBitrate: number;
    /** kbps */
    audioBitrate: number;
    encoder: EncoderChoice;
  };
  destinations: Destination[];
  bufferMinutes: number;
  /** default delay used by "start delay" */
  delaySeconds: number;
  transition: {
    sound: boolean;
    soundVolume: number;
  };
  reconnectGraceSeconds: number;
}

export type TransitionKind = 'rewind' | 'forward';

export interface IngestInfo {
  codec: string;
  width: number;
  height: number;
  fps: number;
  audio?: string;
}

export type Mode = 'idle' | 'live' | 'delayed';

export interface CommandResult {
  ok: boolean;
  error?: string;
  delay?: number;
  clamped?: boolean;
  queued?: boolean;
}

export interface TransitionEvent {
  kind: TransitionKind;
  from: number;
  to: number;
  /** seconds; the dashboard mirrors these to stay in sync with the stream */
  phases: { intro: number; scrub: number; land: number };
}

export interface PlayoutState {
  mode: Mode;
  targetDelay: number;
  effectiveDelay: number;
  maxDelay: number;
  transition: {
    kind: TransitionKind;
    from: number;
    to: number;
    progress: number;
  } | null;
  output: {
    running: boolean;
    codec: string | null;
    destinations: string[];
    previewOnly: boolean;
    since: number | null;
    dropped: number;
    error: string | null;
  };
  underruns: number;
}

export interface AppState {
  ingest: {
    connected: boolean;
    server: string;
    key: string;
    info: IngestInfo | null;
  };
  buffer: { recorded: number; capacity: number };
  playout: PlayoutState;
  delaySeconds: number;
  restartPending: boolean;
}

export interface ConfigResponse {
  config: Config;
  restartPending: boolean;
}

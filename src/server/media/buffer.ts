import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export interface Segment {
  index: number;
  /** session-local seconds */
  start: number;
  end: number;
}

export interface Session {
  id: number;
  dir: string;
  /** global time at which this session starts */
  base: number;
  /** completed segments still on disk */
  segments: Segment[];
  /** number of completed segments ever (also the index of the one being written) */
  completed: number;
  liveFrames: number;
  ended: boolean;
}

export interface Thumb {
  t: number;
  frame: Buffer;
}

export interface Location {
  session: Session;
  index: number;
  /** session-local start of the segment */
  start: number;
  /** seconds into the segment */
  offset: number;
}

/**
 * The rolling recording: MPEG-TS segments on disk (one directory per OBS connection)
 * plus small in-memory thumbnails used for the on-stream scrub animation.
 *
 * Times are "media seconds" on one global timeline: each ingest session starts where the
 * previous one ended, so disconnect gaps collapse instead of leaving holes.
 */
export class BufferStore {
  readonly fps: number;
  bufferSeconds: number;
  readonly root: string;
  sessions: Session[] = [];
  thumbs: Thumb[] = [];
  private nextSessionId = 1;

  constructor({
    fps,
    bufferSeconds,
    root,
  }: {
    fps: number;
    bufferSeconds: number;
    root?: string;
  }) {
    this.fps = fps;
    this.bufferSeconds = bufferSeconds;
    this.root = root ?? path.join(os.tmpdir(), 'nicedelay-buffer');
    fs.rmSync(this.root, { recursive: true, force: true });
    fs.mkdirSync(this.root, { recursive: true });
  }

  /**
   * Sessions are created when ffmpeg starts listening, but only join the timeline
   * (register) once OBS actually connects.
   */
  createSession(): Session {
    const id = this.nextSessionId++;
    const session: Session = {
      id,
      dir: path.join(this.root, `session-${id}`),
      base: 0,
      segments: [],
      completed: 0,
      liveFrames: 0,
      ended: false,
    };
    fs.mkdirSync(session.dir, { recursive: true });
    return session;
  }

  register(session: Session): void {
    const prev = this.sessions.at(-1);
    session.base = prev ? sessionEnd(prev, this.fps) : 0;
    this.sessions.push(session);
  }

  discard(session: Session): void {
    if (!this.sessions.includes(session))
      fs.rm(session.dir, { recursive: true, force: true }, () => {});
  }

  segmentPath(session: Session, index: number): string {
    return path.join(session.dir, `s${String(index).padStart(8, '0')}.ts`);
  }

  addSegment(session: Session, start: number, end: number): void {
    session.segments.push({ index: session.completed, start, end });
    session.completed++;
  }

  /** Live edge on the global timeline. */
  get liveEdge(): number {
    const s = this.sessions.at(-1);
    return s ? sessionEnd(s, this.fps) : 0;
  }

  get oldest(): number {
    for (const s of this.sessions) {
      if (s.segments.length) return s.base + s.segments[0].start;
      if (!s.ended) return s.base;
    }
    return this.liveEdge;
  }

  get recorded(): number {
    return Math.max(0, this.liveEdge - this.oldest);
  }

  /**
   * Where a reader must start to play global time `t`: the latest segment (a keyframe)
   * starting at or before `t`. The segment currently being written also qualifies,
   * since readers tail files that are still growing.
   */
  locate(t: number): Location | null {
    for (let i = this.sessions.length - 1; i >= 0; i--) {
      const s = this.sessions[i];
      if (t < s.base && i > 0) continue;
      const local = Math.max(0, t - s.base);
      const last = s.segments.at(-1);
      if (!s.ended && (!last || local >= last.end)) {
        const start = last ? last.end : 0;
        return { session: s, index: s.completed, start, offset: local - start };
      }
      for (let j = s.segments.length - 1; j >= 0; j--) {
        const seg = s.segments[j];
        if (seg.start <= local || j === 0) {
          return {
            session: s,
            index: seg.index,
            start: seg.start,
            offset: Math.max(0, local - seg.start),
          };
        }
      }
    }
    return null;
  }

  nextSession(session: Session): Session | null {
    const i = this.sessions.indexOf(session);
    return i >= 0 ? (this.sessions[i + 1] ?? null) : null;
  }

  addThumb(t: number, frame: Buffer): void {
    this.thumbs.push({ t, frame });
  }

  /** Nearest thumbnail to global time `t`. */
  thumbAt(t: number): Thumb | null {
    const th = this.thumbs;
    if (!th.length) return null;
    let lo = 0;
    let hi = th.length - 1;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (th[mid].t < t) lo = mid + 1;
      else hi = mid;
    }
    if (lo > 0 && Math.abs(th[lo - 1].t - t) < Math.abs(th[lo].t - t)) lo--;
    return th[lo];
  }

  /** Drop everything older than the buffer window, but never what a reader still needs. */
  trim(protectFrom = Infinity): void {
    const cutoff = Math.min(
      this.liveEdge - this.bufferSeconds - 5,
      protectFrom - 5,
    );
    while (this.thumbs.length && this.thumbs[0].t < cutoff) this.thumbs.shift();
    for (const s of this.sessions) {
      while (s.segments.length && s.base + s.segments[0].end < cutoff) {
        const seg = s.segments.shift()!;
        fs.rm(this.segmentPath(s, seg.index), { force: true }, () => {});
      }
    }
    while (
      this.sessions.length > 1 &&
      this.sessions[0].ended &&
      !this.sessions[0].segments.length
    ) {
      const s = this.sessions.shift()!;
      fs.rm(s.dir, { recursive: true, force: true }, () => {});
    }
  }

  dispose(): void {
    fs.rmSync(this.root, { recursive: true, force: true });
  }
}

function sessionEnd(s: Session, fps: number): number {
  const last = s.segments.at(-1);
  return s.base + Math.max(s.liveFrames / fps, s.ended && last ? last.end : 0);
}

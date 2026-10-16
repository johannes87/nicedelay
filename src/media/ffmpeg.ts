import { type ChildProcess, execFileSync, spawn } from 'node:child_process';
import type { Readable, Writable } from 'node:stream';

export const FFMPEG = process.env.FFMPEG || 'ffmpeg';

/** Normalize any input picture to the output canvas (letterboxed, square pixels). */
export function scaleChain(w: number, h: number): string {
  return (
    `scale=${w}:${h}:force_original_aspect_ratio=decrease:flags=bicubic,` +
    `pad=${w}:${h}:-1:-1:color=black,setsar=1,format=yuv420p`
  );
}

export const AUDIO_CHAIN = 'aresample=48000:async=1:first_pts=0,aformat=sample_fmts=s16:channel_layouts=stereo';

export interface FfmpegProcess extends ChildProcess {
  stdout: Readable;
  stderr: Readable;
}

/** Spawns ffmpeg with pipes on fds 1..fds-1 (fd 2 is stderr). */
export function spawnFfmpeg(
  args: string[],
  { fds = 3, stdin = 'ignore' }: { fds?: number; stdin?: 'ignore' | 'pipe' } = {},
): FfmpegProcess {
  const stdio: ('ignore' | 'pipe')[] = [stdin, 'pipe', 'pipe'];
  for (let i = 3; i < fds; i++) stdio.push('pipe');
  const proc = spawn(FFMPEG, ['-hide_banner', '-nostdin', ...args], { stdio }) as FfmpegProcess;
  // EPIPE and friends surface through 'close'; don't let them crash the process.
  for (const s of proc.stdio) s?.on('error', () => {});
  return proc;
}

export function pipe(proc: ChildProcess, fd: number): Readable {
  return proc.stdio[fd] as Readable;
}

export function writablePipe(proc: ChildProcess, fd: number): Writable {
  return proc.stdio[fd] as Writable;
}

/** Keeps the last few stderr lines for error reporting. */
export function tailStderr(proc: FfmpegProcess, onLine?: (line: string) => void, keep = 20): string[] {
  const lines: string[] = [];
  let partial = '';
  proc.stderr.setEncoding('utf8');
  proc.stderr.on('data', (d: string) => {
    const parts = (partial + d).split(/\r?\n|\r/);
    partial = parts.pop() ?? '';
    for (const line of parts) {
      if (!line.trim()) continue;
      lines.push(line);
      if (lines.length > keep) lines.shift();
      onLine?.(line);
    }
  });
  return lines;
}

let encoderList: string | null = null;
export function hasEncoder(name: string): boolean {
  if (encoderList === null) {
    try {
      encoderList = execFileSync(FFMPEG, ['-hide_banner', '-encoders'], { encoding: 'utf8' });
    } catch {
      encoderList = '';
    }
  }
  return new RegExp(`\\s${name}\\s`).test(encoderList);
}

export function killGently(proc: ChildProcess | null | undefined, ms = 2000): void {
  if (proc?.exitCode !== null || proc.signalCode) return;
  proc.kill('SIGTERM');
  setTimeout(() => {
    if (proc.exitCode === null) proc.kill('SIGKILL');
  }, ms).unref();
}

export function drained(stream: Writable): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  const done = () => {
    stream.off('drain', done).off('close', done);
    resolve();
  };
  stream.on('drain', done).on('close', done);
  return promise;
}

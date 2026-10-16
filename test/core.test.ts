import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { describe, it } from 'node:test';
import { validate } from '../src/server/config.ts';
import { BufferStore } from '../src/server/media/buffer.ts';
import { parseStreamInfo } from '../src/server/media/ingest.ts';
import {
  AVQueue,
  ByteQueue,
  FrameAssembler,
} from '../src/server/media/queues.ts';
import { StopDetector } from '../src/server/media/relay.ts';
import { formatDelay, formatDuration } from '../src/server/render/overlay.ts';
import {
  crossfade,
  frameBytes,
  scaleFrame,
  toneFrame,
} from '../src/server/render/yuv.ts';

const FPS = 30;
const BPF = (48_000 / FPS) * 4; // audio bytes per video frame

/** Audio bytes whose value encodes the frame they belong to. */
function audioFor(frame: number, frames = 1): Buffer {
  const b = Buffer.alloc(BPF * frames);
  for (let f = 0; f < frames; f++)
    b.fill((frame + f) % 256, f * BPF, (f + 1) * BPF);
  return b;
}

function store(): BufferStore {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nicedelay-test-'));
  return new BufferStore({ fps: FPS, bufferSeconds: 60, root });
}

describe('ByteQueue', () => {
  it('reads across chunk boundaries', () => {
    const q = new ByteQueue();
    q.push(Buffer.from([1, 2, 3]));
    q.push(Buffer.from([4, 5]));
    const out = Buffer.alloc(4);
    assert.equal(q.readInto(out, 0, 4), 4);
    assert.deepEqual([...out], [1, 2, 3, 4]);
    assert.equal(q.length, 1);
    assert.equal(q.skip(10), 1);
    assert.equal(q.length, 0);
  });
});

describe('FrameAssembler', () => {
  it('cuts arbitrary chunks into whole frames', () => {
    const frames: Buffer[] = [];
    const asm = new FrameAssembler(4, (f) => frames.push(f));
    asm.push(Buffer.from([1, 2, 3]));
    asm.push(Buffer.from([4, 5, 6, 7, 8, 9]));
    assert.equal(frames.length, 2);
    assert.deepEqual([...frames[1]], [5, 6, 7, 8]);
  });
});

describe('AVQueue', () => {
  it('pairs each frame with its own audio', () => {
    const q = new AVQueue(FPS);
    for (let i = 0; i < 3; i++) q.pushVideo(Buffer.from([i]));
    q.pushAudio(audioFor(0, 3));
    for (let i = 0; i < 3; i++) {
      const out = q.pull();
      assert.ok(out);
      assert.equal(out.video[0], i);
      assert.equal(out.audio[0], i);
    }
  });

  it('waits for late audio unless forced', () => {
    const q = new AVQueue(FPS);
    q.pushVideo(Buffer.from([0]));
    assert.equal(q.pull(), null);
    const forced = q.pull(true);
    assert.ok(forced);
    assert.equal(
      forced.audio.every((b) => b === 0),
      true,
    );
  });

  it('keeps sync after dropping frames', () => {
    const q = new AVQueue(FPS);
    for (let i = 0; i < 10; i++) q.pushVideo(Buffer.from([i]));
    q.pushAudio(audioFor(0, 10));
    assert.equal(q.trimTo(3), 7);
    const out = q.pull();
    assert.ok(out);
    assert.equal(out.video[0], 7);
    assert.equal(out.audio[0], 7);
  });

  it('discards audio for frames dropped before the audio arrived', () => {
    const q = new AVQueue(FPS);
    for (let i = 0; i < 5; i++) q.pushVideo(Buffer.from([i]));
    q.trimTo(1); // drop frames 0..3 while no audio is queued yet
    q.pushAudio(audioFor(0, 5));
    const out = q.pull();
    assert.ok(out);
    assert.equal(out.video[0], 4);
    assert.equal(out.audio[0], 4);
  });
});

describe('BufferStore', () => {
  it('finds the keyframe segment at or before a time', () => {
    const s = store();
    const session = s.createSession();
    s.register(session);
    s.addSegment(session, 0, 2);
    s.addSegment(session, 2, 4);
    session.liveFrames = 5 * FPS;
    const loc = s.locate(3.5);
    assert.ok(loc);
    assert.equal(loc.index, 1);
    assert.equal(loc.offset, 1.5);
    // Past the last finished segment: the one still being written.
    const tail = s.locate(4.6);
    assert.ok(tail);
    assert.equal(tail.index, 2);
    assert.ok(Math.abs(tail.offset - 0.6) < 1e-9);
    s.dispose();
  });

  it('puts reconnects on one continuous timeline', () => {
    const s = store();
    const a = s.createSession();
    s.register(a);
    s.addSegment(a, 0, 3);
    a.liveFrames = 3 * FPS;
    a.ended = true;
    const b = s.createSession();
    s.register(b);
    assert.equal(b.base, 3);
    b.liveFrames = 2 * FPS;
    assert.equal(s.liveEdge, 5);
    assert.equal(s.recorded, 5);
    assert.equal(s.locate(4)?.session, b);
    assert.equal(s.locate(1)?.session, a);
    s.dispose();
  });

  it('returns the nearest thumbnail', () => {
    const s = store();
    for (const t of [0, 1, 2, 3]) s.addThumb(t, Buffer.from([t]));
    assert.equal(s.thumbAt(1.4)?.t, 1);
    assert.equal(s.thumbAt(1.6)?.t, 2);
    assert.equal(s.thumbAt(99)?.t, 3);
    s.dispose();
  });
});

describe('parseStreamInfo', () => {
  it('reads video and audio details from ffmpeg output', () => {
    const video =
      '  Stream #0:0: Video: h264 (High), yuv420p(tv, bt709, progressive), 1920x1080 [SAR 1:1 DAR 16:9], 6000 kb/s, 29.97 fps, 1k tbn';
    const info = parseStreamInfo(video, null);
    assert.deepEqual(info, {
      codec: 'h264',
      width: 1920,
      height: 1080,
      fps: 29.97,
    });
    const withAudio = parseStreamInfo(
      '  Stream #0:1: Audio: aac (LC), 48000 Hz, stereo, fltp, 160 kb/s',
      info,
    );
    assert.equal(withAudio?.audio, 'aac');
    assert.equal(parseStreamInfo('frame=  100 fps= 30 q=-1.0', info), info);
  });
});

/** An RTMP command name as it appears on the wire (AMF0 string). */
const amf = (name: string) =>
  Buffer.concat([Buffer.from([0x02, 0x00, name.length]), Buffer.from(name)]);

describe('StopDetector', () => {
  it('spots the stop command', () => {
    const d = new StopDetector();
    assert.equal(d.push(Buffer.alloc(5000, 7)), false);
    assert.equal(
      d.push(
        Buffer.concat([
          Buffer.alloc(100),
          amf('deleteStream'),
          Buffer.alloc(10),
        ]),
      ),
      true,
    );
  });

  it('spots a command split across chunks', () => {
    const d = new StopDetector();
    const cmd = amf('FCUnpublish');
    assert.equal(
      d.push(Buffer.concat([Buffer.alloc(300), cmd.subarray(0, 6)])),
      false,
    );
    assert.equal(
      d.push(Buffer.concat([cmd.subarray(6), Buffer.alloc(300)])),
      true,
    );
  });

  it('ignores the start commands', () => {
    const d = new StopDetector();
    assert.equal(
      d.push(
        Buffer.concat([amf('FCPublish'), amf('createStream'), amf('publish')]),
      ),
      false,
    );
  });
});

describe('validate', () => {
  it('fills defaults and rejects junk', () => {
    const c = validate({
      output: { width: 1919, fps: 29, encoder: 'nope', videoBitrate: '4500' },
      destinations: [{ url: '  rtmps://x/app ' }, { url: '' }, 'garbage'],
      ingest: { key: 'a b/c' },
      delaySeconds: 99_999,
      bufferMinutes: 2,
    });
    assert.equal(c.output.width, 1920);
    assert.equal(c.output.fps, 30);
    assert.equal(c.output.encoder, 'auto');
    assert.equal(c.output.videoBitrate, 4500);
    assert.deepEqual(c.destinations, [
      { name: '', url: 'rtmps://x/app', key: '', enabled: true },
    ]);
    assert.equal(c.ingest.key, 'abc');
    assert.equal(c.delaySeconds, 120);
  });
});

describe('render helpers', () => {
  it('formats delays like a clock', () => {
    assert.equal(formatDelay(10), '−0:10');
    assert.equal(formatDelay(75, { sign: false }), '1:15');
    assert.equal(formatDelay(3723), '−1:02:03');
  });

  it('formats delays as short durations', () => {
    assert.equal(formatDuration(10), '10s');
    assert.equal(formatDuration(90.4), '90s');
    assert.equal(formatDuration(120), '2m');
    assert.equal(formatDuration(150), '150s');
  });

  it('scales, tones and crossfades yuv frames', () => {
    const src = Buffer.alloc(frameBytes(4, 4), 128);
    src.fill(200, 0, 16);
    const big = Buffer.alloc(frameBytes(8, 8));
    scaleFrame(src, 4, 4, big, 8, 8);
    assert.equal(big[0], 200);
    assert.equal(big[64], 128);

    const dark = Buffer.alloc(big.length);
    toneFrame(big, dark, 8, 8, { luma: 0.5 });
    assert.equal(dark[0], Math.round(16 + (200 - 16) * 0.5));

    const mid = Buffer.alloc(big.length);
    crossfade(big, dark, mid, 0.5);
    assert.ok(mid[0] > dark[0] && mid[0] < big[0]);
  });
});

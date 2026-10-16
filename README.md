# nicedelay

Stream delay for OBS that starts **instantly**.

nicedelay sits between OBS and Twitch/YouTube and keeps recording the last few minutes of your stream. Because it's always recording, turning a delay on doesn't make viewers wait while a buffer fills: the stream jumps back right away, with a rewind animation and sound so viewers know what happened. Going live again fast-forwards to the present the same way.

## Requirements

- Node.js 24 or newer
- ffmpeg (`brew install ffmpeg`)

## Setup

```sh
npm install
npm run build   # builds the dashboard
npm start
```

Open the dashboard at <http://127.0.0.1:8080>, then:

1. **Point OBS at nicedelay**: in OBS, Settings → Stream → Service **Custom**. Paste the server and stream key from nicedelay's settings (by default `rtmp://127.0.0.1:1935/live` and `nicedelay`).
2. **Check the keyframe interval**: in OBS, Settings → Output. In **Simple** output mode there's nothing to do. In **Advanced** mode, open the **Streaming** tab and set **Keyframe Interval** to 1 or 2 s (the default `0` means "auto" and can be much longer). nicedelay jumps between keyframes, so short intervals make jumps fast and precise.
3. **Tell nicedelay where to stream**: in nicedelay, Settings → Stream to. Add Twitch or YouTube (both connect over encrypted RTMPS) or any RTMP/RTMPS server, with your stream key.
4. Start streaming in OBS. Press **Space** in the dashboard (or click the button) to delay; press it again to go live.

The dashboard also works as an OBS dock (Docks → Custom Browser Docks).

### Controlling it from elsewhere

Stream Deck, scripts and similar tools can use the HTTP API:

```sh
curl -X POST localhost:8080/api/toggle                                    # delay ↔ live
curl -X POST localhost:8080/api/delay -d '{"seconds":30}' -H 'content-type: application/json'
curl -X POST localhost:8080/api/live
```

## How it works

```
OBS ──RTMP──▶ ingest (ffmpeg) ──┬─ rolling buffer: MPEG-TS segments on disk
                                ├─ decoded live feed ────────────┐
                                └─ thumbnails (for the scrub)    ▼
                                    buffer reader (ffmpeg) ─▶ playout clock ─▶ encoder (ffmpeg) ──▶ Twitch, YouTube, …
                                                                  │
                                                     rewind / fast-forward animation
```

- **Ingest** listens for OBS and writes 1-second segments to a rolling buffer (stream copy, no quality loss). It also decodes a low-latency live feed.
- **Playout** emits one frame per tick to a single long-lived encoder, so destinations see one continuous stream. When live, it plays the live feed. When delayed, a reader decodes the buffer from any past moment, tailing the segment still being written.
- **Transitions** are rendered into the stream: the picture dims, VHS-style tracking noise rolls in, real footage scrubs from the old position to the new one, and an on-screen icon, timecode and timeline show what's happening. Audio and video stay paired frame by frame throughout.
- If OBS disconnects while delayed, viewers keep watching until the buffered footage runs out. If OBS reconnects within the grace period, the stream never drops.

Config lives in `config.json` (created on first save). Changes to quality or the OBS connection apply when the stream ends.

## Development

```sh
npm run dev        # server with auto-restart
npm run dev:web    # dashboard with hot reload (proxies to the server)
npm run fake-obs   # stream a test pattern (counter + beeps) into the ingest
npm run check      # typecheck (tsc 7) · lint (oxlint, type-aware) · format (oxfmt) · tests
```

The server runs TypeScript directly on Node (type stripping). `tsc` only type-checks.

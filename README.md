# nicedelay

Stream delay for OBS that starts **instantly**.

> **Status: prototype.** nicedelay works, but it's early. It's developed and tested on macOS only; Windows and Linux are untested. Expect rough edges, and don't rely on it for an important stream yet.
>
> nicedelay was largely written by Claude, Anthropic's AI coding assistant. I directed the design and reviewed some of the changes, but the code hasn't had a full review.

nicedelay sits between OBS and Twitch/YouTube and keeps recording the last few minutes of your stream. Because it's always recording, turning a delay on doesn't make viewers wait while a buffer fills: the stream jumps back right away, with a rewind animation and sound so viewers know what happened. A badge in the corner always tells viewers where they are: "LIVE", or "10s delay" while the delay is on. Drag it in the dashboard's preview to move it. Going live again fast-forwards to the present the same way.

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
2. **Set the keyframe interval**: in OBS, Settings → Output, set **Output Mode** to **Advanced**, open the **Streaming** tab and set **Keyframe Interval** to 1 or 2 s. Simple mode has no such setting, and with a Custom server OBS leaves it to the encoder, which can mean keyframes only every 4 s or more. nicedelay jumps between keyframes and takes the rewind animation's thumbnails from them, so short intervals make jumps fast, precise and cheap, and the animation smooth.
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
                                │            │
                                │            ▼
                                │  buffer reader (ffmpeg) ─▶ playout clock ─▶ encoder (ffmpeg) ──▶ Twitch, YouTube, …
                                │                                  ▲
                                └─ thumbnails (keyframes only) ─▶ rewind / fast-forward animation
```

- **Ingest** listens for OBS and writes 1-second segments to a rolling buffer (stream copy, no quality loss), packet by packet, so the newest footage is on disk right away. For the rewind animation's thumbnails it decodes only the keyframes, which is cheap.
- **Playout** emits one frame per tick to a single long-lived encoder, so destinations see one continuous stream. A reader decodes the buffer, tailing the segment still being written: at the live edge when live, or from any past moment when delayed.
- **Transitions** are rendered into the stream: the picture dims, VHS-style tracking noise rolls in, real footage scrubs from the old position to the new one, and an on-screen icon, timecode and timeline show what's happening. Audio and video stay paired frame by frame throughout.
- When you stop streaming in OBS, viewers still see any delayed footage, then the stream ends. If OBS drops out instead (a crash or lost connection), nicedelay keeps the stream up for 15 seconds, showing "RECONNECTING", so a restarted OBS can continue the same stream.

Config lives in `config.json` (created on first save). Changes to quality or the OBS connection apply when the stream ends.

## Performance

nicedelay re-encodes your stream: the badge and rewind animations are drawn into the picture, so it decodes what OBS sends and encodes it again. That's one decode and a second encode on top of OBS's own encode.

- **macOS** encodes on the GPU (VideoToolbox), so it costs little.
- **Windows and Linux** currently encode on the CPU (x264). At 1080p60 that takes several cores, which can lower your fps in CPU-heavy games.
- Decoding currently runs on the CPU on all platforms.

For demanding games, run nicedelay on a second machine: it's just a hop between OBS and Twitch, so any computer on your network works. By default it only accepts connections from the same computer; set `ingest.host` and `httpHost` to `0.0.0.0` in `config.json` so OBS and your browser can reach it. The dashboard has no login, so only do this on a network you trust. Hardware encoding on Windows and Linux is planned.

## Development

```sh
npm run dev        # server with auto-restart
npm run dev:web    # dashboard with hot reload (proxies to the server)
npm run fake-obs   # stream a test pattern (counter + beeps) into the ingest
npm run check      # typecheck (tsc 7) · lint (oxlint, type-aware + SonarQube rules) · format (oxfmt) · tests
```

The SonarQube rules (`npm run lint:sonar`) come from `eslint-plugin-sonarjs`, which still needs the TypeScript 6 compiler API. They live in `tools/sonar` with their own dependencies, installed automatically by `npm install`.

The server runs TypeScript directly on Node (type stripping). `tsc` only type-checks.

## License

MIT. See [LICENSE](LICENSE).

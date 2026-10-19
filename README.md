# nicedelay

Stream delay for OBS that starts **instantly**.

> **Status: prototype.** nicedelay works, but it's early. It's developed and tested on macOS only; Windows and Linux are built but untested. Expect rough edges, and don't rely on it for an important stream yet.
>
> nicedelay was largely written by Claude, Anthropic's AI coding assistant. I directed the design and reviewed some of the changes, but the code hasn't had a full review.

nicedelay sits between OBS and Twitch/YouTube and keeps recording the last few minutes of your stream. Because it's always recording, turning a delay on doesn't make viewers wait while a buffer fills: the stream jumps back right away, with a rewind animation and sound so viewers know what happened. A badge in the corner always tells viewers where they are: "LIVE", or "10s delay" while the delay is on. Drag it in the app's preview to move it. Going live again fast-forwards to the present the same way.

## Requirements

A graphics card (or Apple chip) that can decode and encode H.264 in hardware. nicedelay never falls back to the CPU for video:

- **macOS**: any Mac (VideoToolbox).
- **Windows**: NVIDIA (NVENC), AMD (AMF) or Intel (Quick Sync) graphics, or Media Foundation's hardware encoder.
- **Linux**: NVIDIA (NVENC) or Intel/AMD with VA-API. Some distributions, for example Fedora, build Mesa without H.264 support because of patents; there you need a Mesa build that has it (on Fedora, from RPM Fusion).

If nothing suitable is found, the app says so in its status line instead of streaming.

## Setup

Download nicedelay for your system from the [releases](https://github.com/johannes87/nicedelay/releases) and open it. Nothing else to install: FFmpeg's libraries come with it.

1. **Point OBS at nicedelay**: in OBS, Settings → Stream → Service **Custom**. Paste the server and stream key from nicedelay's settings (by default `rtmp://127.0.0.1:1935/live` and `nicedelay`).
2. **Set the keyframe interval**: in OBS, Settings → Output, set **Output Mode** to **Advanced**, open the **Streaming** tab and set **Keyframe Interval** to 1 or 2 s. Simple mode has no such setting, and with a Custom server OBS leaves it to the encoder, which can mean keyframes only every 4 s or more. nicedelay jumps between keyframes and takes the rewind animation's thumbnails from them, so short intervals make jumps fast, precise and cheap, and the animation smooth.
3. **Keep OBS's colour format at NV12** (Settings → Advanced, the default). GPUs can't decode the 10-bit or 4:4:4 H.264 some other formats produce; nicedelay tells you if OBS sends one.
4. **Tell nicedelay where to stream**: in nicedelay, Settings → Stream to. Add Twitch or YouTube (both connect over encrypted RTMPS) or any RTMP/RTMPS server, with your stream key.
5. Start streaming in OBS. Press **Space** in nicedelay's window (or click the button) to delay; press it again to go live. **L** always goes live.

### Controlling it from elsewhere

Stream Deck, scripts and similar tools can use the HTTP API:

```sh
curl -X POST localhost:8080/api/toggle                                    # delay ↔ live
curl -X POST localhost:8080/api/delay -d '{"seconds":30}' -H 'content-type: application/json'
curl -X POST localhost:8080/api/live
curl localhost:8080/api/state
```

`nicedelay --headless` runs without a window, controlled only through the API, for example on a second computer.

## How it works

```
OBS ──RTMP──▶ relay ──▶ libavformat ──┬─ rolling buffer: OBS's packets, untouched, in chunk files on disk
                                      └─ keyframes ──GPU decode──▶ thumbnails (for the scrub)

buffer ──▶ reader (GPU decode) ──▶ playout clock ──▶ GPU encode + AAC ──▶ Twitch, YouTube, …
                                        │
                           rewind / fast-forward animation (Skia)
```

nicedelay is one program. It uses FFmpeg's libraries for everything to do with video: RTMP in and out, decoding and encoding on the graphics card, scaling and audio resampling. Skia draws the on-stream graphics; the window is [egui](https://github.com/emilk/egui).

- **Ingest** keeps OBS's packets exactly as sent, so recording costs almost nothing. A small relay in front of FFmpeg's RTMP listener notices when OBS stops streaming on purpose, as opposed to crashing or losing the connection.
- **Playout** emits one frame per tick to a single long-lived encoder, so destinations see one continuous stream. A reader decodes the buffer from any moment, live or in the past, following the newest packets as they arrive.
- **Transitions** are rendered into the stream: the picture dims, VHS-style tracking noise rolls in, real footage scrubs from the old position to the new one, and an on-screen icon, timecode and timeline show what's happening. Audio and video stay paired frame by frame throughout.
- When you stop streaming in OBS, viewers still see any delayed footage, then the stream ends. If OBS drops out instead (a crash or lost connection), nicedelay keeps the stream up for 15 seconds, showing "RECONNECTING", so a restarted OBS can continue the same stream.

Settings live in `config.json` in your system's config folder (on macOS `~/Library/Application Support/nicedelay/`); `NICEDELAY_CONFIG` points elsewhere. Changes to quality or the OBS connection apply when the stream ends.

## Performance

nicedelay re-encodes your stream: the badge and rewind animations are drawn into the picture. Decoding and encoding run on the graphics card. Measured on an M2 MacBook at 1080p60 (CPU use of one core):

| | nicedelay |
| --- | --- |
| Live | 8% |
| Delayed | 9% |
| During a rewind or fast-forward (about 2 s) | 51% |

The animation is drawn on the CPU; moving it to the GPU is planned.

## Development

You need Rust and the tools to build FFmpeg (a C compiler, `pkg-config`, and on Linux `nasm`, `autoconf` and the VA-API headers; see `.github/workflows/build.yml`).

```sh
scripts/build-ffmpeg.sh                    # builds the LGPL FFmpeg libraries with vcpkg (once, a few minutes)
eval "$(scripts/build-ffmpeg.sh --env)"    # tells cargo where they are
cargo run                                  # the app
cargo run -- --headless                    # without a window
cargo test
scripts/fake-obs.sh                        # stream a test pattern (counter + beeps); needs the ffmpeg command
scripts/package.sh                         # after cargo build --release: the download for this system, in dist/
                                           # (needs cargo-about: cargo install cargo-about --locked --features cli)
```

`scripts/build-ffmpeg.sh` builds FFmpeg 9 from `vcpkg.json` with only LGPL parts and the hardware codecs for each platform, and refuses GPL or nonfree builds. `NICEDELAY_SCREENSHOT=window.png` saves a screenshot of the window after a few seconds (`NICEDELAY_SCREENSHOT_AFTER` sets how many).

## License

MIT. See [LICENSE](LICENSE).

The downloads include FFmpeg's libraries, licensed under the LGPL (version 3 or later), as separate files you can replace with your own build. The FFmpeg source with the build recipe and patches is attached to each release. The on-stream text uses the [Inter](https://rsms.me/inter/) font (SIL Open Font License). The licences of everything else compiled into nicedelay (the Rust crates, and Skia with the libraries it includes) are in `THIRD-PARTY-LICENSES.txt` in the download; `scripts/third-party-licenses.sh` generates it.

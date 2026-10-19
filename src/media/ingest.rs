//! Listens for OBS over RTMP. OBS connects to the relay, which forwards the stream to
//! libavformat's RTMP listener on an internal port and notices when OBS stops streaming on
//! purpose. Every packet goes into the store unchanged; only keyframes are decoded here,
//! for thumbnails. The picture viewers see, live or delayed, comes from buffer readers.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ffmpeg_next::codec::Id;
use ffmpeg_next::ffi::AVPixelFormat;
use ffmpeg_next::media::Type;
use ffmpeg_next::{Dictionary, Packet, Rational, format};

use crate::config::Config;
use crate::media::reader::KeyframeDecoder;
use crate::media::relay::{Relay, free_port};
use crate::media::store::{Kind, PacketMeta, Session, Store, Streams};
use crate::state::IngestInfo;

/// OBS is considered gone after this long without a packet (e.g. the network dropped).
const STALL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
pub enum IngestEvent {
    Connected,
    Disconnected,
}

pub type Listener = Arc<dyn Fn(IngestEvent) + Send + Sync>;

#[derive(Default)]
pub struct IngestShared {
    pub connected: AtomicBool,
    pub info: Mutex<Option<IngestInfo>>,
    pub error: Mutex<Option<String>>,
    stopped: AtomicBool,
    /// milliseconds since `epoch` of the last packet
    last_packet: AtomicU64,
}

pub struct Ingest {
    pub shared: Arc<IngestShared>,
    relay: Option<Relay>,
    thread: Option<JoinHandle<()>>,
    pub url: String,
    job: Option<Job>,
}

/// What the playout needs to know about the ingest.
#[derive(Clone)]
pub struct IngestView {
    shared: Arc<IngestShared>,
    ended_by_obs: Arc<AtomicBool>,
}

impl IngestView {
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::Acquire)
    }

    /// OBS sent its "stop streaming" commands before the current/last connection closed.
    pub fn ended_by_obs(&self) -> bool {
        self.ended_by_obs.load(Ordering::Relaxed)
    }
}

fn set<T>(m: &Mutex<T>, v: T) {
    *m.lock().unwrap_or_else(|e| e.into_inner()) = v;
}

impl Ingest {
    /// Opens the OBS port; [`Ingest::run`] then starts accepting streams.
    pub fn new(config: &Config, store: Arc<Store>) -> Ingest {
        let c = &config.ingest;
        let host = if c.host == "0.0.0.0" { "127.0.0.1" } else { c.host.as_str() };
        let url = format!("rtmp://{host}:{}/{}", c.port, c.app);
        let shared = Arc::new(IngestShared::default());
        let relay = match Relay::start(&c.host, c.port) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("can't listen on {}:{}: {e}", c.host, c.port);
                tracing::warn!("ingest: {msg}");
                set(&shared.error, Some(msg));
                return Ingest { shared, relay: None, thread: None, url, job: None };
            }
        };
        let job = Job {
            shared: shared.clone(),
            store,
            listener: Arc::new(|_| {}),
            internal: relay.internal_port.clone(),
            app: c.app.clone(),
            key: c.key.clone(),
            thumb: thumb_geometry(config),
            epoch: Instant::now(),
        };
        Ingest { shared, relay: Some(relay), thread: None, url, job: Some(job) }
    }

    pub fn view(&self) -> IngestView {
        let ended_by_obs = self.relay.as_ref().map_or_else(Default::default, |r| r.ended_by_obs.clone());
        IngestView { shared: self.shared.clone(), ended_by_obs }
    }

    pub fn run(&mut self, listener: Listener) {
        if let Some(mut job) = self.job.take() {
            job.listener = listener;
            self.thread = thread::Builder::new().name("ingest".into()).spawn(move || job.run()).ok();
        }
    }

    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::Acquire)
    }

    pub fn info(&self) -> Option<IngestInfo> {
        self.shared.info.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn stop(&mut self) {
        // Interrupting the RTMP listener makes FFmpeg log an error; it's expected here.
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Fatal);
        self.shared.stopped.store(true, Ordering::Release);
        if let Some(r) = &self.relay {
            r.stop();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
    }
}

impl Drop for Ingest {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Thumbnail size (a tenth of the output) and how many per second to keep: about 900
/// across the whole buffer, so long buffers don't use too much memory.
struct ThumbGeometry {
    w: u32,
    h: u32,
    rate: f64,
}

fn thumb_geometry(c: &Config) -> ThumbGeometry {
    ThumbGeometry {
        w: ((f64::from(c.output.width) / 10.0).round() as u32) * 2,
        h: ((f64::from(c.output.height) / 10.0).round() as u32) * 2,
        rate: (900.0 / (c.buffer_minutes * 60.0)).clamp(0.5, 5.0),
    }
}

struct Job {
    shared: Arc<IngestShared>,
    store: Arc<Store>,
    listener: Listener,
    internal: Arc<std::sync::atomic::AtomicU16>,
    app: String,
    key: String,
    thumb: ThumbGeometry,
    epoch: Instant,
}

impl Job {
    fn stopped(&self) -> bool {
        self.shared.stopped.load(Ordering::Acquire)
    }

    fn run(self) {
        while !self.stopped() {
            let pause = match self.session() {
                Ok(true) => Duration::from_millis(200),
                Ok(false) => Duration::from_millis(1000),
                Err(e) => {
                    if !self.stopped() {
                        tracing::warn!("ingest: {e}");
                        set(&self.shared.error, Some(e));
                    }
                    Duration::from_millis(1000)
                }
            };
            thread::sleep(pause);
        }
    }

    /// Waits for OBS and records one connection. Returns whether OBS was connected.
    fn session(&self) -> Result<bool, String> {
        let port = free_port().map_err(|e| e.to_string())?;
        self.internal.store(port, Ordering::Relaxed);
        let url = format!("rtmp://127.0.0.1:{port}/{}/{}", self.app, self.key);
        let mut opts = Dictionary::new();
        opts.set("listen", "1");
        let shared = self.shared.clone();
        let epoch = self.epoch;
        let interrupt = move || {
            if shared.stopped.load(Ordering::Acquire) {
                return true;
            }
            let last = shared.last_packet.load(Ordering::Relaxed);
            shared.connected.load(Ordering::Acquire)
                && epoch.elapsed().as_millis() as u64 > last + STALL.as_millis() as u64
        };
        let mut input = match format::input_with_interrupt_and_dictionary(&url, interrupt, opts) {
            Ok(i) => i,
            Err(e) if self.stopped() => return Err(e.to_string()),
            Err(e) => return Err(format!("couldn't accept OBS's stream: {e}")),
        };

        let video = input.streams().best(Type::Video).ok_or("OBS sent no video")?;
        let (video_index, video_tb, video_params) = (video.index(), video.time_base(), video.parameters());
        check_decodable(&video_params)?;
        let audio = input.streams().best(Type::Audio).map(|s| (s.index(), s.time_base(), s.parameters()));
        let info = stream_info(&video, audio.as_ref().map(|a| &a.2));
        set(&self.shared.error, None);
        set(&self.shared.info, Some(info.clone()));
        tracing::info!("OBS stream: {} {}x{} {:.3} fps", info.codec, info.width, info.height, info.fps);

        let streams =
            Streams { video: video_params, video_tb, audio: audio.as_ref().map(|(_, tb, p)| (p.clone(), *tb)) };
        let session = self.store.create_session(streams).map_err(|e| format!("can't record: {e}"))?;
        let result = self.record(&mut input, &session, video_index, audio.map(|a| (a.0, a.1)));
        session.finish();
        let was_connected = self.shared.connected.swap(false, Ordering::AcqRel);
        if was_connected {
            (self.listener)(IngestEvent::Disconnected);
        } else {
            self.store.discard(&session);
        }
        result.map(|()| was_connected)
    }

    fn record(
        &self,
        input: &mut format::context::Input,
        session: &Arc<Session>,
        video_index: usize,
        audio: Option<(usize, Rational)>,
    ) -> Result<(), String> {
        let video_tb = session.streams.video_tb;
        let mut thumbs: Option<KeyframeDecoder> = None;
        let mut last_thumb = f64::NEG_INFINITY;
        // Session-local time starts at the first packet.
        let mut t0: Option<f64> = None;
        let mut packet = Packet::empty();
        loop {
            match packet.read(input) {
                Ok(()) => {}
                Err(ffmpeg_next::Error::Eof) => return Ok(()),
                Err(_) if self.stopped() => return Ok(()),
                Err(e) => {
                    tracing::info!("ingest: connection ended: {e}");
                    return Ok(());
                }
            }
            let (kind, tb) = if packet.stream() == video_index {
                (Kind::Video, video_tb)
            } else if let Some((i, tb)) = audio
                && packet.stream() == i
            {
                (Kind::Audio, tb)
            } else {
                continue;
            };
            let tbf = f64::from(tb);
            let dts = packet.dts().or(packet.pts()).unwrap_or(0);
            let t0 = *t0.get_or_insert(dts as f64 * tbf);
            let offset = (t0 / tbf).round() as i64;
            let (pts, dts) = (packet.pts().unwrap_or(dts) - offset, dts - offset);
            let time = pts as f64 * tbf;
            let meta = PacketMeta::new(kind, pts, dts, packet.duration(), packet.is_key(), time);

            if !self.shared.connected.load(Ordering::Acquire) {
                self.store.register(session);
                self.shared.connected.store(true, Ordering::Release);
                (self.listener)(IngestEvent::Connected);
            }
            self.shared.last_packet.store(self.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
            let data = packet.data().unwrap_or(&[]);
            session.append(meta, data).map_err(|e| format!("can't record: {e}"))?;
            session.advance(time + packet.duration() as f64 * tbf);

            if kind == Kind::Video && meta.key && time - last_thumb >= 1.0 / self.thumb.rate {
                if thumbs.is_none() {
                    match KeyframeDecoder::new(session, self.thumb.w, self.thumb.h) {
                        Ok(d) => thumbs = Some(d),
                        Err(e) => set(&self.shared.error, Some(e)),
                    }
                }
                if let Some(d) = thumbs.as_mut() {
                    match d.push(&meta, data) {
                        Ok(Some(pic)) => {
                            self.store.add_thumb(session.base() + time, pic);
                            last_thumb = time;
                        }
                        Ok(None) => {}
                        Err(e) => tracing::debug!("ingest: thumbnail: {e}"),
                    }
                }
            }
        }
    }
}

/// Rejects streams no GPU decoder handles, with what to change in OBS.
fn check_decodable(params: &ffmpeg_next::codec::Parameters) -> Result<(), String> {
    let id = params.id();
    if !matches!(id, Id::H264 | Id::HEVC | Id::AV1) {
        return Err(format!(
            "OBS sends {id:?} video, which nicedelay can't decode. In OBS, choose an H.264 encoder (Settings → Output)."
        ));
    }
    let format = unsafe { (*params.as_ptr()).format };
    let ok_420_8bit =
        [AVPixelFormat::AV_PIX_FMT_YUV420P, AVPixelFormat::AV_PIX_FMT_YUVJ420P, AVPixelFormat::AV_PIX_FMT_NV12]
            .iter()
            .any(|f| *f as i32 == format);
    let ok_420_10bit =
        [AVPixelFormat::AV_PIX_FMT_YUV420P10LE, AVPixelFormat::AV_PIX_FMT_P010LE].iter().any(|f| *f as i32 == format);
    let unknown = format < 0;
    if id == Id::H264 && !(ok_420_8bit || unknown) {
        return Err("OBS sends 10-bit or 4:4:4 H.264, which GPUs can't decode. In OBS, set Settings → Advanced → \
                    Color Format to NV12."
            .into());
    }
    if !(ok_420_8bit || ok_420_10bit || unknown) {
        return Err(
            "OBS sends 4:4:4 video, which GPUs can't decode. In OBS, set Settings → Advanced → Color Format to NV12 \
                    (or P010 for 10-bit HEVC)."
                .into(),
        );
    }
    Ok(())
}

fn stream_info(video: &format::stream::Stream, audio: Option<&ffmpeg_next::codec::Parameters>) -> IngestInfo {
    let p = video.parameters();
    let (w, h) = unsafe { ((*p.as_ptr()).width as u32, (*p.as_ptr()).height as u32) };
    let rate = video.avg_frame_rate();
    let rate = if rate.denominator() != 0 && rate.numerator() != 0 { rate } else { video.rate() };
    let fps = if rate.denominator() != 0 { f64::from(rate) } else { 0.0 };
    let name = |id: Id| format!("{id:?}").to_lowercase();
    IngestInfo {
        codec: name(p.id()),
        width: w,
        height: h,
        fps: (fps * 1000.0).round() / 1000.0,
        audio: audio.map(|a| name(a.id())),
    }
}

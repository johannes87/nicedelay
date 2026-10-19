//! The single, long-lived output: frames from the playout in, H.264 (on the GPU) and AAC
//! out to every destination at once through libavformat's tee muxer, plus a small preview
//! picture for the app window.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use ffmpeg_next::format::sample::Type as SampleType;
use ffmpeg_next::format::{Pixel, Sample};
use ffmpeg_next::software::resampling;
use ffmpeg_next::{ChannelLayout, Dictionary, Packet, Rational, codec, encoder, ffi, format, frame};

use crate::config::{Config, Destination, destination_url};
use crate::media::hw;
use crate::media::picture::{Picture, Sws};
use crate::media::queue::{AUDIO_RATE, AvFrame};

pub const PREVIEW_WIDTH: u32 = 640;
const PREVIEW_FPS: u32 = 15;

/// A downscaled RGBA copy of what viewers see.
pub struct PreviewImage {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

pub type PreviewSink = Arc<dyn Fn(PreviewImage) + Send + Sync>;

#[derive(Clone)]
pub struct OutputSpec {
    pub w: u32,
    pub h: u32,
    pub fps: u32,
    pub video_kbps: u32,
    pub audio_kbps: u32,
    pub encoder: String,
    pub destinations: Vec<Destination>,
}

impl OutputSpec {
    pub fn from_config(c: &Config) -> Self {
        OutputSpec {
            w: c.output.width,
            h: c.output.height,
            fps: c.output.fps,
            video_kbps: c.output.video_bitrate,
            audio_kbps: c.output.audio_bitrate,
            encoder: c.output.encoder.clone(),
            destinations: c.destinations.iter().filter(|d| d.enabled).cloned().collect(),
        }
    }
}

#[derive(Default)]
struct Shared {
    running: AtomicBool,
    /// the encoder failed while streaming (the playout restarts the output)
    failed: AtomicBool,
    dropped: AtomicU64,
    codec: Mutex<Option<String>>,
    error: Mutex<Option<String>>,
}

pub struct Output {
    tx: Option<SyncSender<AvFrame>>,
    shared: Arc<Shared>,
    pub destinations: Vec<String>,
    /// Unix milliseconds
    pub started_at: u64,
}

fn lock<T: Clone>(m: &Mutex<T>) -> T {
    m.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn put<T>(m: &Mutex<T>, v: T) {
    *m.lock().unwrap_or_else(|e| e.into_inner()) = v;
}

impl Output {
    pub fn start(spec: OutputSpec, preview: PreviewSink) -> Output {
        // Short hiccups (up to a quarter second of frames) are absorbed. If the encoder really
        // can't keep up, whole frames (with their audio) are dropped rather than buffered.
        let (tx, rx) = sync_channel(((spec.fps / 4) as usize).max(4));
        let shared = Arc::new(Shared::default());
        shared.running.store(true, Ordering::Release);
        let destinations =
            spec.destinations.iter().map(|d| if d.name.is_empty() { d.url.clone() } else { d.name.clone() }).collect();
        let s = shared.clone();
        let _ = thread::Builder::new().name("output".into()).spawn(move || run(spec, rx, s, preview));
        let started_at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        Output { tx: Some(tx), shared, destinations, started_at }
    }

    pub fn write(&self, frame: AvFrame) {
        let Some(tx) = &self.tx else { return };
        match tx.try_send(frame) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    pub fn running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }

    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }

    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    pub fn codec(&self) -> Option<String> {
        lock(&self.shared.codec)
    }

    pub fn error(&self) -> Option<String> {
        lock(&self.shared.error)
    }
}

impl Drop for Output {
    /// Closing the channel lets the encoder flush and end the streams cleanly.
    fn drop(&mut self) {
        self.tx = None;
    }
}

fn run(spec: OutputSpec, rx: Receiver<AvFrame>, shared: Arc<Shared>, preview: PreviewSink) {
    let mut enc = None;
    if !spec.destinations.is_empty() {
        match Encoder::open(&spec) {
            Ok(e) => {
                tracing::info!("output: encoding with {}", e.name);
                put(&shared.codec, Some(e.name.clone()));
                enc = Some(e);
            }
            Err(e) => {
                tracing::warn!("output: {e}");
                put(&shared.error, Some(e));
            }
        }
    }
    let every = (spec.fps / PREVIEW_FPS).max(1);
    let (pw, ph) =
        (PREVIEW_WIDTH, ((f64::from(PREVIEW_WIDTH) * f64::from(spec.h) / f64::from(spec.w) / 2.0).round() as u32) * 2);
    let mut sws = Sws::default();
    let mut n = 0u32;
    for frame in rx.iter() {
        if n.is_multiple_of(every) {
            let mut rgba = vec![0u8; (pw * ph * 4) as usize];
            sws.picture_to_rgba(&frame.video, pw, ph, &mut rgba);
            preview(PreviewImage { w: pw, h: ph, rgba });
        }
        n = n.wrapping_add(1);
        if let Some(e) = enc.as_mut()
            && let Err(err) = e.write(&frame)
        {
            tracing::warn!("output: stopped unexpectedly: {err}");
            put(&shared.error, Some(err));
            shared.failed.store(true, Ordering::Release);
            enc = None;
            break;
        }
    }
    if let Some(mut e) = enc
        && let Err(err) = e.finish()
    {
        tracing::debug!("output: finishing: {err}");
    }
    shared.running.store(false, Ordering::Release);
}

/// tee muxer slave spec: escape the characters tee treats specially.
fn tee_escape(s: &str) -> String {
    s.chars().fold(String::new(), |mut out, c| {
        if matches!(c, '\\' | '|' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

struct Encoder {
    name: String,
    out: format::context::Output,
    video: encoder::Video,
    audio: encoder::Audio,
    video_tb: Rational,
    audio_tb: Rational,
    pool: Option<hw::FramePool>,
    resampler: resampling::Context,
    frame_size: usize,
    pending: Vec<i16>,
    frames: i64,
    samples: i64,
    w: u32,
    h: u32,
}

impl Encoder {
    fn open(spec: &OutputSpec) -> Result<Encoder, String> {
        let tee = spec
            .destinations
            .iter()
            .map(|d| format!("[f=flv:onfail=ignore]{}", tee_escape(&destination_url(d))))
            .collect::<Vec<_>>()
            .join("|");
        let mut out = open_tee(&tee)?;

        let candidates: Vec<&str> =
            if spec.encoder == "auto" { hw::encoder_candidates().to_vec() } else { vec![spec.encoder.as_str()] };
        let mut errors = vec![];
        let mut opened = None;
        for name in candidates {
            match open_video(name, spec) {
                Ok(v) => {
                    opened = Some((name.to_string(), v));
                    break;
                }
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        let Some((name, (video, pool))) = opened else {
            return Err(format!("no hardware H.264 encoder works on this computer ({})", errors.join("; ")));
        };
        let audio = open_audio(spec)?;

        let vcodec = encoder::find_by_name(&name).ok_or("encoder vanished")?;
        let mut vs = out.add_stream(vcodec).map_err(|e| e.to_string())?;
        vs.set_parameters(&video);
        vs.set_time_base(Rational(1, spec.fps as i32));
        let acodec = encoder::find(codec::Id::AAC).ok_or("no AAC encoder")?;
        let mut aus = out.add_stream(acodec).map_err(|e| e.to_string())?;
        aus.set_parameters(&audio);
        aus.set_time_base(Rational(1, AUDIO_RATE as i32));
        out.write_header().map_err(|e| format!("can't reach any destination: {e}"))?;

        let resampler = resampling::Context::get(
            Sample::I16(SampleType::Packed),
            ChannelLayout::STEREO,
            AUDIO_RATE,
            Sample::F32(SampleType::Planar),
            ChannelLayout::STEREO,
            AUDIO_RATE,
        )
        .map_err(|e| e.to_string())?;
        let frame_size = (audio.frame_size() as usize).max(1024);
        Ok(Encoder {
            name,
            out,
            video,
            audio,
            video_tb: Rational(1, spec.fps as i32),
            audio_tb: Rational(1, AUDIO_RATE as i32),
            pool,
            resampler,
            frame_size,
            pending: vec![],
            frames: 0,
            samples: 0,
            w: spec.w,
            h: spec.h,
        })
    }

    fn write(&mut self, f: &AvFrame) -> Result<(), String> {
        let mut vf = frame::Video::new(Pixel::NV12, self.w, self.h);
        copy_nv12(&f.video, &mut vf);
        vf.set_pts(Some(self.frames));
        self.frames += 1;
        let vf = match &self.pool {
            Some(pool) => pool.upload(&vf)?,
            None => vf,
        };
        self.video.send_frame(&vf).map_err(|e| format!("video encoder: {e}"))?;
        self.drain_video()?;

        self.pending.extend_from_slice(&f.audio);
        while self.pending.len() >= self.frame_size * 2 {
            let chunk: Vec<i16> = self.pending.drain(..self.frame_size * 2).collect();
            self.encode_audio(&chunk)?;
        }
        Ok(())
    }

    fn encode_audio(&mut self, chunk: &[i16]) -> Result<(), String> {
        let n = chunk.len() / 2;
        let mut input = frame::Audio::new(Sample::I16(SampleType::Packed), n, ChannelLayout::STEREO);
        input.set_rate(AUDIO_RATE);
        let (pairs, _) = chunk.as_chunks::<2>();
        input.plane_mut::<(i16, i16)>(0).iter_mut().zip(pairs).for_each(|(d, s)| *d = (s[0], s[1]));
        let mut planar = frame::Audio::new(Sample::F32(SampleType::Planar), n, ChannelLayout::STEREO);
        planar.set_rate(AUDIO_RATE);
        self.resampler.run(&input, &mut planar).map_err(|e| format!("audio conversion: {e}"))?;
        planar.set_pts(Some(self.samples));
        self.samples += n as i64;
        self.audio.send_frame(&planar).map_err(|e| format!("audio encoder: {e}"))?;
        self.drain_audio()
    }

    fn drain_video(&mut self) -> Result<(), String> {
        let mut p = Packet::empty();
        while self.video.receive_packet(&mut p).is_ok() {
            p.set_stream(0);
            p.rescale_ts(self.video_tb, self.out.stream(0).map_or(self.video_tb, |s| s.time_base()));
            p.write_interleaved(&mut self.out).map_err(|e| format!("sending to the destinations failed: {e}"))?;
        }
        Ok(())
    }

    fn drain_audio(&mut self) -> Result<(), String> {
        let mut p = Packet::empty();
        while self.audio.receive_packet(&mut p).is_ok() {
            p.set_stream(1);
            p.rescale_ts(self.audio_tb, self.out.stream(1).map_or(self.audio_tb, |s| s.time_base()));
            p.write_interleaved(&mut self.out).map_err(|e| format!("sending to the destinations failed: {e}"))?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        let _ = self.video.send_eof();
        self.drain_video()?;
        let _ = self.audio.send_eof();
        self.drain_audio()?;
        self.out.write_trailer().map_err(|e| e.to_string())
    }
}

/// The tee muxer opens each destination itself, so unlike `format::output_as` this
/// doesn't open the spec as a file.
fn open_tee(spec: &str) -> Result<format::context::Output, String> {
    let fmt = std::ffi::CString::new("tee").expect("no NUL");
    let url = std::ffi::CString::new(spec).map_err(|_| "a destination URL contains a NUL byte")?;
    unsafe {
        let mut ps = std::ptr::null_mut();
        let r = ffi::avformat_alloc_output_context2(&mut ps, std::ptr::null_mut(), fmt.as_ptr(), url.as_ptr());
        if r < 0 || ps.is_null() {
            return Err(format!("can't open the destinations: {}", ffmpeg_next::Error::from(r)));
        }
        Ok(format::context::Output::wrap(ps))
    }
}

fn open_video(name: &str, spec: &OutputSpec) -> Result<(encoder::Video, Option<hw::FramePool>), String> {
    let codec = encoder::find_by_name(name).ok_or("not in this FFmpeg build")?;
    let mut v = codec::context::Context::new_with_codec(codec).encoder().video().map_err(|e| e.to_string())?;
    let kbps = spec.video_kbps as usize * 1000;
    v.set_width(spec.w);
    v.set_height(spec.h);
    v.set_time_base(Rational(1, spec.fps as i32));
    v.set_frame_rate(Some(Rational(spec.fps as i32, 1)));
    v.set_bit_rate(kbps);
    v.set_max_bit_rate(kbps);
    v.set_gop(spec.fps * 2);
    v.set_max_b_frames(0);
    v.set_flags(codec::Flags::GLOBAL_HEADER);
    v.set_format(Pixel::NV12);
    let mut pool = None;
    unsafe {
        let c = v.as_mut_ptr();
        (*c).rc_buffer_size = (kbps * 2) as i32;
        (*c).color_primaries = ffi::AVColorPrimaries::AVCOL_PRI_BT709;
        (*c).color_trc = ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
        (*c).colorspace = ffi::AVColorSpace::AVCOL_SPC_BT709;
        (*c).color_range = ffi::AVColorRange::AVCOL_RANGE_MPEG;
        if let Some(kind) = hw::encoder_device(name) {
            let dev = hw::device(kind)?;
            let p = hw::FramePool::new(&dev, spec.w, spec.h)?;
            (*c).hw_frames_ctx = p.new_ref();
            (*c).pix_fmt = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
            pool = Some(p);
        }
    }
    let mut opts = Dictionary::new();
    for (k, val) in hw::encoder_options(name) {
        opts.set(k, val);
    }
    let enc = v.open_as_with(codec, opts).map_err(|e| e.to_string())?;
    Ok((enc, pool))
}

fn open_audio(spec: &OutputSpec) -> Result<encoder::Audio, String> {
    let codec = encoder::find(codec::Id::AAC).ok_or("no AAC encoder")?;
    let mut a = codec::context::Context::new_with_codec(codec).encoder().audio().map_err(|e| e.to_string())?;
    a.set_rate(AUDIO_RATE as i32);
    a.set_channel_layout(ChannelLayout::STEREO);
    a.set_format(Sample::F32(SampleType::Planar));
    a.set_bit_rate(spec.audio_kbps as usize * 1000);
    a.set_time_base(Rational(1, AUDIO_RATE as i32));
    a.set_flags(codec::Flags::GLOBAL_HEADER);
    a.open_as(codec).map_err(|e| format!("AAC encoder: {e}"))
}

/// Copies an NV12 picture into a frame whose rows may be padded.
fn copy_nv12(pic: &Picture, f: &mut frame::Video) {
    let (w, h) = (pic.w as usize, pic.h as usize);
    let src = &pic.data;
    for plane in 0..2 {
        let stride = f.stride(plane);
        let rows = if plane == 0 { h } else { h / 2 };
        let offset = if plane == 0 { 0 } else { w * h };
        let dst = f.data_mut(plane);
        for y in 0..rows {
            dst[y * stride..y * stride + w].copy_from_slice(&src[offset + y * w..offset + (y + 1) * w]);
        }
    }
}

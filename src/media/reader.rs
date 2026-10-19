//! Plays the recording from a global time: reads packets from the store (following the
//! newest ones as they arrive), decodes them, and queues frames at the output's fixed
//! frame rate, each paired with its audio. One reader covers one ingest session; the
//! playout chains readers across sessions.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use ffmpeg_next::format::Sample;
use ffmpeg_next::format::sample::Type as SampleType;
use ffmpeg_next::software::resampling;
use ffmpeg_next::{ChannelLayout, Packet, codec, decoder, ffi, frame};

use crate::media::hw;
use crate::media::picture::{Picture, SWS_BICUBIC, Sws};
use crate::media::queue::{AUDIO_RATE, AvFrame, AvQueue};
use crate::media::store::{ChunkReader, Kind, Next, PacketMeta, Session, Store};

/// Output picture size and frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub w: u32,
    pub h: u32,
    pub fps: u32,
}

struct Shared {
    queue: Mutex<AvQueue>,
    cond: Condvar,
    destroyed: AtomicBool,
    closed: AtomicBool,
    error: Mutex<Option<String>>,
}

impl Shared {
    fn queue(&self) -> MutexGuard<'_, AvQueue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Reader {
    pub session: Arc<Session>,
    /// global time of the first frame this reader outputs
    pub start_time: f64,
    pub fps: u32,
    pub max_queue: usize,
    shared: Arc<Shared>,
}

impl Reader {
    pub fn new(store: &Store, format: Format, t: f64) -> Result<Reader, String> {
        let loc = store.locate(t).ok_or("nothing recorded at that time")?;
        let max_queue = (f64::from(format.fps) * 0.6).round().max(8.0) as usize;
        let shared = Arc::new(Shared {
            queue: Mutex::new(AvQueue::new(format.fps)),
            cond: Condvar::new(),
            destroyed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        let reader = Reader { session: loc.session.clone(), start_time: loc.start, fps: format.fps, max_queue, shared };
        let (shared, session) = (reader.shared.clone(), loc.session.clone());
        let start = loc.start;
        thread::Builder::new()
            .name("reader".into())
            .spawn(move || {
                let mut d = Decode::new(shared.clone(), session, format, start, max_queue);
                if let Err(e) = d.run(loc.index)
                    && !shared.destroyed.load(Ordering::Relaxed)
                {
                    tracing::warn!("reader: {e}");
                    *shared.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                }
                shared.closed.store(true, Ordering::Release);
                shared.cond.notify_all();
            })
            .map_err(|e| e.to_string())?;
        Ok(reader)
    }

    /// Global time of the next frame to be pulled.
    pub fn position(&self) -> f64 {
        self.start_time + self.shared.queue().next as f64 / f64::from(self.fps)
    }

    pub fn queue_len(&self) -> usize {
        self.shared.queue().len()
    }

    pub fn ready(&self) -> bool {
        let mut q = self.shared.queue();
        !q.is_empty() && (q.has_audio_for_next() || q.len() >= self.max_queue - 1)
    }

    /// True once everything was decoded and consumed.
    pub fn exhausted(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire) && self.shared.queue().is_empty()
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn pull(&self, force: bool) -> Option<AvFrame> {
        let out = self.shared.queue().pull(force);
        self.shared.cond.notify_all();
        out
    }

    /// Drop up to `n` frames (keeping at least one queued) to catch up.
    pub fn skip(&self, n: usize) -> usize {
        let mut q = self.shared.queue();
        let mut skipped = 0;
        while skipped < n && q.len() > 1 {
            q.pull(true);
            skipped += 1;
        }
        drop(q);
        self.shared.cond.notify_all();
        skipped
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.shared.destroyed.store(true, Ordering::Release);
        self.shared.queue().clear();
        self.shared.cond.notify_all();
        self.session.wake();
    }
}

/// The decoding thread's state.
struct Decode {
    shared: Arc<Shared>,
    session: Arc<Session>,
    format: Format,
    /// global time of output frame 0
    start: f64,
    base: f64,
    max_queue: usize,
    sws: Sws,
    /// next output frame to fill
    next_slot: i64,
    /// the newest decoded picture, scaled on demand
    last: Option<(frame::Video, Option<Picture>)>,
    resampler: Option<resampling::Context>,
    /// audio samples (per channel) queued so far
    written: i64,
}

impl Decode {
    fn new(shared: Arc<Shared>, session: Arc<Session>, format: Format, start: f64, max_queue: usize) -> Self {
        let base = session.base();
        Decode {
            shared,
            session,
            format,
            start,
            base,
            max_queue,
            sws: Sws::default(),
            next_slot: 0,
            last: None,
            resampler: None,
            written: 0,
        }
    }

    fn destroyed(&self) -> bool {
        self.shared.destroyed.load(Ordering::Relaxed)
    }

    fn run(&mut self, mut index: u64) -> Result<(), String> {
        let streams = self.session.streams.clone();
        let (mut video, _) = hw::open_video_decoder(&streams.video, streams.video_tb, false)?;
        let mut audio = match &streams.audio {
            Some((params, tb)) => {
                let ctx = codec::context::Context::from_parameters(params.clone()).map_err(|e| e.to_string())?;
                let mut dec = ctx.decoder();
                dec.set_packet_time_base(*tb);
                Some((dec.audio().map_err(|e| format!("audio decoder: {e}"))?, *tb))
            }
            None => None,
        };
        let vtb = f64::from(streams.video_tb);
        let mut chunks = ChunkReader::default();
        let mut failures = 0;
        while !self.destroyed() {
            let meta = match self.session.wait_packet(index, Duration::from_millis(100)) {
                Next::Packet(m) => m,
                Next::Wait => continue,
                Next::Gone => break,
            };
            index += 1;
            let data = chunks.read(&self.session, &meta).map_err(|e| format!("reading the buffer: {e}"))?;
            let packet = to_packet(&meta, &data);
            match meta.kind {
                Kind::Video => {
                    if video.send_packet(&packet).is_err() {
                        failures += 1;
                    }
                    let mut f = frame::Video::empty();
                    while video.receive_frame(&mut f).is_ok() {
                        failures = 0;
                        let pts = f.timestamp().unwrap_or(0) as f64 * vtb;
                        let sw = hw::download(&f)?;
                        self.on_video(sw, self.base + pts);
                    }
                    // A GPU that can't decode this stream fails every frame.
                    if failures > 30 {
                        return Err("the GPU can't decode this stream".into());
                    }
                }
                Kind::Audio => {
                    let Some((dec, tb)) = audio.as_mut() else { continue };
                    let atb = f64::from(*tb);
                    if dec.send_packet(&packet).is_err() {
                        continue;
                    }
                    let mut f = frame::Audio::empty();
                    while dec.receive_frame(&mut f).is_ok() {
                        let pts = f.timestamp().unwrap_or(0) as f64 * atb;
                        self.on_audio(&f, self.base + pts);
                    }
                }
            }
            if streams.audio.is_none() {
                self.fill_silence();
            }
        }
        // Drain what's still inside the decoders.
        if !self.destroyed() {
            let _ = video.send_eof();
            let mut f = frame::Video::empty();
            while video.receive_frame(&mut f).is_ok() {
                let pts = f.timestamp().unwrap_or(0) as f64 * vtb;
                self.on_video(hw::download(&f)?, self.base + pts);
            }
        }
        Ok(())
    }

    /// Maps a decoded picture onto output frame slots: repeated to fill gaps (lower input
    /// frame rate), dropped when two land in the same slot (higher), and frames before the
    /// start only remembered, so the first slot shows the latest picture at or before it.
    fn on_video(&mut self, frame: frame::Video, t: f64) {
        let fps = f64::from(self.format.fps);
        let k = ((t - self.start) * fps).round() as i64;
        if k < self.next_slot {
            self.last = Some((frame, None));
            return;
        }
        // A jump in the timestamps (not a gap a repeat should cover): just carry on from here.
        if k - self.next_slot > (fps * 5.0) as i64 {
            self.next_slot = k;
        }
        while self.next_slot < k && !self.destroyed() {
            let Some(pic) = self.last_picture() else { break };
            self.push_video(pic);
            self.next_slot += 1;
        }
        self.last = Some((frame, None));
        if let Some(pic) = self.last_picture() {
            self.push_video(pic);
        }
        self.next_slot = k + 1;
    }

    fn last_picture(&mut self) -> Option<Picture> {
        let Format { w, h, .. } = self.format;
        let (frame, scaled) = self.last.as_mut()?;
        if scaled.is_none() {
            *scaled = self.sws.fit_frame(unsafe { &*frame.as_ptr() }, w, h, SWS_BICUBIC);
        }
        scaled.clone()
    }

    /// Queues a picture, waiting while the queue is full.
    fn push_video(&self, pic: Picture) {
        let mut q = self.shared.queue();
        while q.len() >= self.max_queue && !self.destroyed() {
            q = self.shared.cond.wait_timeout(q, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
        }
        if !self.destroyed() {
            q.push_video(pic);
        }
    }

    /// Converts decoded audio to 48 kHz s16 stereo and queues it where its timestamp says
    /// it belongs: gaps are filled with silence and overlaps dropped, so audio stays on the
    /// same timeline as the video slots.
    fn on_audio(&mut self, f: &frame::Audio, t: f64) {
        if self.resampler.is_none() {
            self.resampler = resampling::Context::get(
                f.format(),
                f.channel_layout(),
                f.rate(),
                Sample::I16(SampleType::Packed),
                ChannelLayout::STEREO,
                AUDIO_RATE,
            )
            .map_err(|e| tracing::warn!("reader: audio resampler: {e}"))
            .ok();
        }
        let Some(rs) = self.resampler.as_mut() else { return };
        let mut samples = unsafe {
            let ctx = rs.as_mut_ptr();
            let cap = ffi::swr_get_out_samples(ctx, f.samples() as i32).max(0) as usize;
            let mut out = vec![0i16; cap * 2];
            let out_ptr = out.as_mut_ptr().cast::<u8>();
            let n = ffi::swr_convert(
                ctx,
                &out_ptr,
                cap as i32,
                (*f.as_ptr()).extended_data as *const *const u8,
                f.samples() as i32,
            );
            out.truncate(n.max(0) as usize * 2);
            out
        };
        let expected = ((t - self.start) * f64::from(AUDIO_RATE)).round() as i64;
        let drift = expected - self.written;
        // Small jitter is left alone; the first chunk is placed exactly.
        let tolerance = if self.written == 0 { 0 } else { i64::from(AUDIO_RATE) / 50 };
        if drift > tolerance {
            let pad = drift.min(i64::from(AUDIO_RATE) * 3) as usize;
            self.shared.queue().push_audio(&vec![0; pad * 2]);
            self.written += pad as i64;
        } else if drift < -tolerance {
            let cut = ((-drift) as usize * 2).min(samples.len());
            samples.drain(..cut);
        }
        self.written += (samples.len() / 2) as i64;
        self.shared.queue().push_audio(&samples);
    }

    /// Without an audio stream, every frame gets silence.
    fn fill_silence(&mut self) {
        let want = self.next_slot * i64::from(AUDIO_RATE / self.format.fps);
        if want > self.written {
            self.shared.queue().push_audio(&vec![0; (want - self.written) as usize * 2]);
            self.written = want;
        }
    }
}

fn to_packet(meta: &PacketMeta, data: &[u8]) -> Packet {
    let mut p = Packet::copy(data);
    p.set_pts(Some(meta.pts));
    p.set_dts(Some(meta.dts));
    p.set_duration(meta.duration);
    if meta.key {
        p.set_flags(codec::packet::Flags::KEY);
    }
    p
}

/// Decodes just the keyframes of a live stream, for the scrub animation's thumbnails.
pub struct KeyframeDecoder {
    decoder: decoder::Video,
    sws: Sws,
    w: u32,
    h: u32,
}

impl KeyframeDecoder {
    pub fn new(session: &Session, w: u32, h: u32) -> Result<KeyframeDecoder, String> {
        let s = &session.streams;
        let (decoder, _) = hw::open_video_decoder(&s.video, s.video_tb, true)?;
        Ok(KeyframeDecoder { decoder, sws: Sws::default(), w, h })
    }

    /// Feeds one video packet; returns a thumbnail if it decoded to a picture.
    pub fn push(&mut self, meta: &PacketMeta, data: &[u8]) -> Result<Option<Picture>, String> {
        if !meta.key {
            return Ok(None);
        }
        self.decoder.send_packet(&to_packet(meta, data)).map_err(|e| e.to_string())?;
        let mut f = frame::Video::empty();
        let mut out = None;
        while self.decoder.receive_frame(&mut f).is_ok() {
            let sw = hw::download(&f)?;
            out = self.sws.fit_frame(unsafe { &*sw.as_ptr() }, self.w, self.h, crate::media::picture::SWS_AREA);
        }
        Ok(out)
    }
}

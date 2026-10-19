//! Drives the output: one frame per tick, from a buffer reader (at the live edge or behind
//! it), or a transition that renders the rewind / fast-forward animation between two.
//!
//! The playout runs on its own thread with a precise clock and owns everything it plays;
//! the GUI and the HTTP API talk to it through messages and read a published snapshot.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::format;
use crate::media::ingest::IngestView;
use crate::media::output::{Output, OutputSpec, PreviewSink};
use crate::media::picture::Picture;
use crate::media::queue::{AvFrame, samples_per_frame};
use crate::media::reader::{Format, Reader};
use crate::media::store::Store;
use crate::render::compositor::Compositor;
use crate::render::effects::{draw_toned, tape};
use crate::render::overlay::{CenterParams, Overlay, PillKind, PillParams, TimelineParams};
use crate::render::sfx::{mix_into, tape_whoosh};
use crate::state::{
    CommandResult, Mode, OutputState, Phases, PlayoutState, TransitionEvent, TransitionKind, TransitionState,
};

const PILL_HOLD: f64 = 3.0; // seconds the corner pill is highlighted after a transition
const PILL_FADE: f64 = 0.6;
const READY_TIMEOUT: f64 = 3.0; // max seconds to wait for a reader at landing
const MAX_DRAW_FPS: u32 = 30; // transition animation frame rate cap (keeps 60 fps output within budget)
const LIVE_LEAD: f64 = 1.0; // seconds a transition's live reader starts early, covering decoder latency
const LIVE_CUSHION: f64 = 0.2; // seconds it keeps queued after landing; the surplus is skipped

fn ease_in_out(t: f64) -> f64 {
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}
fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}
fn back_out(t: f64) -> f64 {
    1.0 + 2.9 * (t - 1.0).powi(3) + 1.9 * (t - 1.0).powi(2)
}
fn clamp01(t: f64) -> f64 {
    t.clamp(0.0, 1.0)
}

/// Corner pill: fade in quickly, hold, then fade out.
fn pill_alpha(age: f64) -> f64 {
    if age < 0.3 {
        ease_out(age / 0.3)
    } else if age < PILL_HOLD {
        1.0
    } else {
        1.0 - (age - PILL_HOLD) / PILL_FADE
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    /// keeps its reader at the live edge
    Live,
    /// keeps it `target_delay` behind
    Delayed,
}

struct Source {
    kind: SourceKind,
    reader: Reader,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Correction {
    None,
    Steady,
    Fast,
}

pub enum Command {
    /// None: the configured default delay
    SetDelay(Option<f64>),
    GoLive,
    Toggle,
}

enum Msg {
    Command(Command, Sender<CommandResult>),
    Connected,
    Config(Box<Config>),
    Shutdown,
}

/// What the GUI and the API read about the playout.
#[derive(Clone)]
pub struct Snapshot {
    pub state: PlayoutState,
    /// the latest transition, numbered so the GUI can tell a new one apart
    pub event: Option<(u64, TransitionEvent)>,
}

/// The app's side of the playout thread.
pub struct PlayoutHandle {
    tx: Sender<Msg>,
    snapshot: Arc<Mutex<Snapshot>>,
    thread: Option<JoinHandle<()>>,
}

impl PlayoutHandle {
    pub fn start(config: Config, store: Arc<Store>, ingest: IngestView, preview: PreviewSink) -> PlayoutHandle {
        let (tx, rx) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(Snapshot { state: PlayoutState::idle(&config), event: None }));
        let snap = snapshot.clone();
        let thread = thread::Builder::new()
            .name("playout".into())
            .spawn(move || Playout::new(config, store, ingest, preview, snap).run(rx))
            .ok();
        PlayoutHandle { tx, snapshot, thread }
    }

    pub fn command(&self, cmd: Command) -> CommandResult {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Msg::Command(cmd, tx)).is_err() {
            return CommandResult::error("nicedelay is shutting down.");
        }
        rx.recv_timeout(Duration::from_secs(2)).unwrap_or_else(|_| CommandResult::error("nicedelay didn't respond."))
    }

    /// Tells the playout that OBS connected (callable from any thread).
    pub fn notifier(&self) -> impl Fn() + Send + Sync + 'static {
        let tx = self.tx.clone();
        move || {
            let _ = tx.send(Msg::Connected);
        }
    }

    pub fn set_config(&self, config: Config) {
        let _ = self.tx.send(Msg::Config(Box::new(config)));
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn shutdown(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for PlayoutHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Playout {
    config: Config,
    store: Arc<Store>,
    ingest: IngestView,
    preview: PreviewSink,
    published: Arc<Mutex<Snapshot>>,
    mode: Mode,
    target_delay: f64,
    source: Option<Source>,
    transition: Option<Transition>,
    output: Option<Output>,
    /// when to start the output again after it failed
    restart_at: Option<Instant>,
    last_frame: Option<Picture>,
    overlay: Overlay,
    compositor: Compositor,
    format: Format,
    audio_samples: usize,
    live_target: usize,
    live_max: usize,
    pending: Option<f64>,
    pill: Option<(PillKind, u64)>,
    standby: Option<(Picture, Picture)>,
    ticks: u64,
    underruns: u64,
    starved_since: Option<f64>,
    live_rebuffer: bool,
    last_correction: f64,
    event: Option<(u64, TransitionEvent)>,
    events: u64,
    running: bool,
}

impl Playout {
    fn new(
        config: Config,
        store: Arc<Store>,
        ingest: IngestView,
        preview: PreviewSink,
        published: Arc<Mutex<Snapshot>>,
    ) -> Self {
        let (w, h, fps) = (config.output.width, config.output.height, config.output.fps);
        let f = f64::from(fps);
        Playout {
            store,
            ingest,
            preview,
            published,
            mode: Mode::Idle,
            target_delay: 0.0,
            source: None,
            transition: None,
            output: None,
            restart_at: None,
            last_frame: None,
            overlay: Overlay::new(w, h),
            compositor: Compositor::new(w, h),
            format: Format { w, h, fps },
            audio_samples: samples_per_frame(fps),
            live_target: (f * 0.1).round().max(2.0) as usize,
            live_max: (f * 0.3).round().max(6.0) as usize,
            pending: None,
            pill: None,
            standby: None,
            ticks: 0,
            underruns: 0,
            starved_since: None,
            live_rebuffer: true,
            last_correction: 0.0,
            event: None,
            events: 0,
            running: false,
            config,
        }
    }

    fn fps(&self) -> f64 {
        f64::from(self.format.fps)
    }

    // ---------------------------------------------------------------- thread

    fn run(mut self, rx: Receiver<Msg>) {
        loop {
            if !self.running {
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(msg) => {
                        if !self.handle(msg) {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                // OBS may have connected before the playout was listening.
                if self.mode == Mode::Idle && self.ingest.connected() {
                    self.start_output();
                }
                self.publish();
                continue;
            }
            if !self.clock(&rx) {
                break;
            }
        }
        self.stop_output();
    }

    /// Runs the frame clock until the output stops. Returns false to quit the thread.
    fn clock(&mut self, rx: &Receiver<Msg>) -> bool {
        let period = Duration::from_secs_f64(1.0 / self.fps());
        let mut t0 = Instant::now();
        let mut n: u32 = 0;
        while self.running {
            while let Ok(msg) = rx.try_recv() {
                if !self.handle(msg) {
                    return false;
                }
            }
            let now = Instant::now();
            let mut due =
                (now.duration_since(t0).as_secs_f64() / period.as_secs_f64()).floor() as i64 + 1 - i64::from(n);
            if due > i64::from(self.format.fps) {
                // Fell more than a second behind (machine stalled): resync instead of bursting.
                t0 = now.checked_sub(period * n).unwrap_or(now);
                due = 1;
            }
            // While catching up, animation frames repeat their picture instead of drawing a new
            // one: a slow machine gets a choppier animation instead of dropped output frames.
            for i in 0..due.max(0) {
                if !self.running {
                    break;
                }
                self.tick(i < due - 1);
                n += 1;
            }
            let next = t0 + period * n;
            let now = Instant::now();
            if next > now {
                thread::sleep(next - now);
            }
        }
        true
    }

    fn handle(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Command(cmd, reply) => {
                let r = match cmd {
                    Command::SetDelay(d) => self.set_delay(d.unwrap_or(self.config.delay_seconds)),
                    Command::GoLive => self.go_live(),
                    Command::Toggle => self.toggle(),
                };
                let _ = reply.send(r);
                self.publish();
            }
            Msg::Connected => {
                if self.mode == Mode::Idle {
                    self.start_output();
                }
                self.starved_since = None;
            }
            Msg::Config(c) => {
                // Size and frame rate stay until the pipeline is rebuilt; the rest applies live.
                let (output, ingest) = (self.config.output.clone(), self.config.ingest.clone());
                self.config = *c;
                self.config.output = output;
                self.config.ingest = ingest;
            }
            Msg::Shutdown => return false,
        }
        true
    }

    // ---------------------------------------------------------------- lifecycle

    fn start_output(&mut self) {
        let Some(source) = self.live_source(self.store.live_edge()) else { return };
        self.mode = Mode::Live;
        self.target_delay = 0.0;
        self.source = Some(source);
        self.live_rebuffer = true;
        self.last_frame = None;
        self.starved_since = None;
        self.output = Some(Output::start(OutputSpec::from_config(&self.config), self.preview.clone()));
        self.running = true;
        tracing::info!("output started");
        self.publish();
    }

    fn stop_output(&mut self) {
        self.running = false;
        self.output = None;
        self.restart_at = None;
        self.transition = None;
        self.pending = None;
        self.source = None;
        self.mode = Mode::Idle;
        self.target_delay = 0.0;
        self.pill = None;
        self.publish();
    }

    // ---------------------------------------------------------------- commands

    /// Max delay that can be applied right now (limited by what's recorded).
    fn max_delay(&self) -> f64 {
        (self.config.buffer_minutes * 60.0 - 2.0).min(self.store.recorded() - 0.5).max(0.0)
    }

    fn set_delay(&mut self, seconds: f64) -> CommandResult {
        if self.mode == Mode::Idle {
            return CommandResult::error("Not streaming — connect OBS first.");
        }
        let mut d = seconds;
        if !d.is_finite() || d <= 0.0 {
            return self.go_live();
        }
        let mut clamped = false;
        if d > self.max_delay() {
            d = self.max_delay().floor();
            clamped = true;
        }
        if d < 1.0 {
            return CommandResult::error("Not enough recorded yet.");
        }
        if self.transition.is_some() {
            self.pending = Some(d);
            return CommandResult { queued: true, clamped, ..CommandResult::delay(d) };
        }
        if self.mode == Mode::Delayed && (d - self.target_delay).abs() < 0.5 {
            return CommandResult { clamped, ..CommandResult::delay(d) };
        }
        self.begin_transition(d);
        CommandResult { clamped, ..CommandResult::delay(d) }
    }

    fn go_live(&mut self) -> CommandResult {
        if self.mode == Mode::Idle {
            return CommandResult::error("Not streaming.");
        }
        if self.transition.is_some() {
            self.pending = Some(0.0);
            return CommandResult { queued: true, ..CommandResult::delay(0.0) };
        }
        if self.mode == Mode::Delayed {
            self.begin_transition(0.0);
        }
        CommandResult::delay(0.0)
    }

    fn toggle(&mut self) -> CommandResult {
        let target = self.pending.unwrap_or(self.target_delay);
        if target > 0.0 { self.go_live() } else { self.set_delay(self.config.delay_seconds) }
    }

    fn begin_transition(&mut self, to_delay: f64) {
        let from_delay = if self.mode == Mode::Delayed { self.target_delay } else { 0.0 };
        let t = match Transition::new(self, from_delay, to_delay) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("playout: cannot start transition: {e}");
                return;
            }
        };
        self.mode = if to_delay > 0.0 { Mode::Delayed } else { Mode::Live };
        self.target_delay = to_delay;
        self.pill = None;
        self.events += 1;
        self.event = Some((
            self.events,
            TransitionEvent {
                kind: t.kind,
                from: from_delay,
                to: to_delay,
                phases: Phases { intro: t.intro, scrub: t.scrub, land: t.land },
            },
        ));
        if to_delay > 0.0 {
            tracing::info!("delay → {to_delay}s");
        } else {
            tracing::info!("back to live");
        }
        self.transition = Some(t);
    }

    // ---------------------------------------------------------------- per frame

    /// `late`: more frames are already due after this one.
    fn tick(&mut self, late: bool) {
        self.ticks += 1;
        let mut out = if let Some(mut t) = self.transition.take() {
            let out = t.render(self, late); // may repeat the previous frame: not ours to draw on
            if t.done {
                self.finish_transition(t);
            } else {
                self.transition = Some(t);
            }
            Some(out)
        } else {
            let mut src = self.source.take();
            let out = src.as_mut().and_then(|s| self.pull_from(s, Correction::Steady));
            self.source = src;
            out
        };

        match &out {
            Some(f) => {
                self.starved_since = None;
                self.last_frame = Some(f.video.clone());
            }
            None => {
                self.underruns += 1;
                if self.check_starved() {
                    return;
                }
                out = Some(AvFrame { video: self.standby_frame(), audio: vec![0; self.audio_samples] });
            }
        }
        let Some(mut out) = out else { return };

        if let Some(pill) = self.current_pill() {
            // pixels_mut copies a picture that's shared (repeated, cached) before drawing.
            self.compositor.pill(&mut out.video, &self.overlay, &pill, self.config.badge);
        }

        if let Some(o) = &self.output {
            o.write(out);
            if o.failed() && self.restart_at.is_none() {
                self.restart_at = Some(Instant::now() + Duration::from_secs(2));
            }
        }
        if self.restart_at.is_some_and(|t| Instant::now() >= t) {
            self.restart_at = None;
            self.output = Some(Output::start(OutputSpec::from_config(&self.config), self.preview.clone()));
        }
        if self.ticks.is_multiple_of(3) {
            self.publish();
        }
        if self.ticks.is_multiple_of(u64::from(self.format.fps)) {
            self.housekeeping();
        }
    }

    /// A source that follows the live edge, starting at global time `t`.
    fn live_source(&self, t: f64) -> Option<Source> {
        match Reader::new(&self.store, self.format, t) {
            Ok(reader) => Some(Source { kind: SourceKind::Live, reader }),
            Err(e) => {
                tracing::warn!("playout: {e}");
                None
            }
        }
    }

    /// Moves a source on to the next ingest session once its reader has played this one out.
    /// Returns false if there's nothing to play.
    fn advance_reader(&self, src: &mut Source) -> bool {
        if !src.reader.exhausted() {
            return true;
        }
        let Some(next) = self.store.next_session(&src.reader.session) else { return false };
        // Live jumps to the new live edge; a delay plays the new session from its start.
        let t = if src.kind == SourceKind::Live { self.store.live_edge() } else { next.base() };
        match Reader::new(&self.store, self.format, t) {
            Ok(r) => {
                src.reader = r;
                true
            }
            Err(_) => false,
        }
    }

    fn pull_live(&mut self, src: &mut Source, correct: Correction) -> Option<AvFrame> {
        if !self.advance_reader(src) {
            return None;
        }
        let r = &src.reader;
        let n = r.queue_len();
        if n == 0 {
            self.live_rebuffer = true;
        }
        if self.live_rebuffer {
            if n < self.live_target {
                return None;
            }
            self.live_rebuffer = false;
        }
        // Landing on live: jump to a small cushion right away, hidden by the crossfade.
        // A backlog (e.g. right after OBS connects, or after the machine stalled) fills the
        // queue, with more waiting in the decoder: it's skipped in jumps, as playing through it
        // faster would be visible. Small drift (the feed's clock slightly ahead of ours) is
        // absorbed by dropping single frames.
        let cushion = (self.fps() * LIVE_CUSHION).round() as usize;
        if correct == Correction::Fast && n > cushion {
            r.skip(n - cushion);
        } else if n + 1 >= r.max_queue {
            r.skip(n.saturating_sub(self.live_target));
        } else if n > self.live_max {
            r.skip(1);
        }
        r.pull(r.queue_len() > self.live_max)
    }

    fn pull_delayed(&mut self, src: &mut Source, correct: Correction) -> Option<AvFrame> {
        if !self.advance_reader(src) {
            return None;
        }
        let r = &src.reader;
        // Keep the real delay honest: drift, underruns and slow starts accumulate.
        // Without OBS the live edge stands still, so there is nothing to track: just play out.
        if correct != Correction::None && self.ingest.connected() {
            let err = self.store.live_edge() - r.position() - self.target_delay;
            let now = self.ticks as f64 / self.fps();
            if correct == Correction::Fast && err > 2.0 / self.fps() {
                r.skip(1);
            } else if err.abs() > 0.2 && now - self.last_correction > 0.5 {
                self.last_correction = now;
                if err < 0.0 {
                    return None; // repeat a frame
                }
                r.skip(1);
            }
        }
        r.pull(r.queue_len() + 1 >= r.max_queue)
    }

    fn pull_from(&mut self, src: &mut Source, correct: Correction) -> Option<AvFrame> {
        match src.kind {
            SourceKind::Live => self.pull_live(src, correct),
            SourceKind::Delayed => self.pull_delayed(src, correct),
        }
    }

    fn finish_transition(&mut self, t: Transition) {
        let to = t.to_delay;
        self.source = Some(t.target);
        self.pill = Some((if to > 0.0 { PillKind::Replay } else { PillKind::Live }, self.ticks));
        if let Some(d) = self.pending.take() {
            if d > 0.0 {
                self.set_delay(d);
            } else {
                self.go_live();
            }
        }
        self.publish();
    }

    fn current_pill(&mut self) -> Option<PillParams> {
        if self.starved_since.is_some_and(|s| self.ticks as f64 / self.fps() - s > 1.0) {
            return Some(PillParams { kind: PillKind::Live, text: "RECONNECTING".into(), alpha: 0.95 });
        }
        if self.transition.is_some() {
            return None;
        }
        // Viewers always see whether they are watching live or the past.
        let mut alpha = if self.mode == Mode::Idle { 0.0 } else { 0.8 };
        if let Some((_, start)) = self.pill {
            let age = (self.ticks - start) as f64 / self.fps();
            if age > PILL_HOLD + PILL_FADE {
                self.pill = None;
            }
            alpha = f64::max(alpha, clamp01(pill_alpha(age)));
        }
        if alpha <= 0.0 {
            return None;
        }
        Some(if self.mode == Mode::Delayed {
            PillParams {
                kind: PillKind::Replay,
                text: format!("{} delay", format::duration(self.target_delay)),
                alpha: alpha as f32,
            }
        } else {
            PillParams { kind: PillKind::Live, text: "LIVE".into(), alpha: alpha as f32 }
        })
    }

    fn standby_frame(&mut self) -> Picture {
        let last = self.last_frame.get_or_insert_with(|| Picture::black(self.format.w, self.format.h)).clone();
        if self.starved_since.is_none() {
            return last;
        }
        if self.standby.as_ref().is_none_or(|(from, _)| !from.same_pixels(&last)) {
            let (w, h) = (self.format.w as f32, self.format.h as f32);
            if let Some(img) = self.compositor.image(&last) {
                draw_toned(self.compositor.canvas(), &img, w, h, 0.45, 0.3, 1.0);
            }
            let dim = self.compositor.finish();
            self.standby = Some((last.clone(), dim));
        }
        self.standby.as_ref().map(|(_, d)| d.clone()).unwrap_or(last)
    }

    /// A momentary underrun just repeats the last frame. Once OBS is gone and the buffer has
    /// played out, the stream ends: right away if OBS stopped streaming on purpose, otherwise
    /// after the grace period (showing standby), in case OBS reconnects. Returns true if the
    /// output was stopped.
    fn check_starved(&mut self) -> bool {
        if self.ingest.connected() {
            return false;
        }
        let now = self.ticks as f64 / self.fps();
        let since = *self.starved_since.get_or_insert(now);
        let grace = if self.ingest.ended_by_obs() { 0.0 } else { self.config.reconnect_grace_seconds };
        if now - since < grace {
            return false;
        }
        tracing::info!(
            "{}",
            if self.ingest.ended_by_obs() { "OBS stopped streaming, ending output" } else { "OBS gone, ending output" }
        );
        self.stop_output();
        true
    }

    fn housekeeping(&mut self) {
        // Readers only move forward, so what lies behind their position can go.
        let mut protect = f64::INFINITY;
        for r in self.readers() {
            protect = protect.min(r.position());
        }
        self.store.trim(protect);
    }

    fn readers(&self) -> Vec<&Reader> {
        let mut v: Vec<&Reader> = self.source.iter().map(|s| &s.reader).collect();
        if let Some(t) = &self.transition {
            v.push(&t.target.reader);
            if let Some(o) = &t.old {
                v.push(&o.reader);
            }
        }
        v
    }

    // ---------------------------------------------------------------- state

    fn position(&self) -> f64 {
        match &self.source {
            Some(s) if s.kind == SourceKind::Delayed => s.reader.position(),
            _ => self.store.live_edge(),
        }
    }

    fn effective_delay(&self) -> f64 {
        if self.mode != Mode::Delayed || self.transition.is_some() {
            return self.target_delay;
        }
        (self.store.live_edge() - self.position()).max(0.0)
    }

    fn publish(&self) {
        let out = self.output.as_ref();
        let error = out.and_then(Output::error).or_else(|| self.readers().iter().find_map(|r| r.error()));
        let state = PlayoutState {
            mode: self.mode,
            target_delay: self.target_delay,
            effective_delay: (self.effective_delay() * 10.0).round() / 10.0,
            max_delay: self.max_delay().floor(),
            transition: self.transition.as_ref().map(|t| TransitionState {
                kind: t.kind,
                from: t.from_delay,
                to: t.to_delay,
                progress: t.progress(),
            }),
            output: OutputState {
                width: self.format.w,
                height: self.format.h,
                running: out.is_some_and(Output::running),
                codec: out.and_then(Output::codec),
                destinations: out.map(|o| o.destinations.clone()).unwrap_or_default(),
                preview_only: out.is_some_and(|o| o.destinations.is_empty()),
                since: out.map(|o| o.started_at),
                dropped: out.map_or(0, Output::dropped),
                error,
            },
            underruns: self.underruns,
        };
        let mut s = self.published.lock().unwrap_or_else(|e| e.into_inner());
        s.state = state;
        s.event = self.event;
    }
}

/// What one animation phase contributes to a frame besides the picture.
struct PhaseOutput {
    /// source audio for this frame (already faded), or None for silence
    audio: Option<Vec<i16>>,
    center: (f64, f64, f64), // alpha, pop, progress
    /// delay shown by the timecode and the timeline knob
    delay: f64,
    line_alpha: f64,
}

/// One rewind / fast-forward animation, in three phases:
///   intro — the current picture keeps playing, dims, the icon pops in
///   scrub — thumbnails race from the old position to the new one
///   land  — crossfade into the new source while the graphics fade out
struct Transition {
    kind: TransitionKind,
    from_delay: f64,
    to_delay: f64,
    intro: f64,
    /// longer for bigger jumps, see `new`
    scrub: f64,
    land: f64,
    target: Source,
    old: Option<Source>,
    done: bool,
    fps: f64,
    scrub_from: f64,
    scrub_to: f64,
    span: f64,
    sfx: Option<Vec<i16>>,
    i: u64,
    waited: u64,
    /// Animation frames are drawn at most this often; in between the last one repeats.
    draw_every: u64,
    drawn: Option<Picture>,
    held: Option<skia_safe::Image>,
    last_scrub: Option<skia_safe::Image>,
    incoming: Option<Picture>,
}

impl Transition {
    fn new(p: &mut Playout, from_delay: f64, to_delay: f64) -> Result<Transition, String> {
        let kind = if to_delay > from_delay { TransitionKind::Rewind } else { TransitionKind::Forward };
        let intro = 0.22;
        // Bigger jumps scrub a little longer: 0.78 s for 5 s, 1.15 s for 5 min, at most 1.35 s.
        let scrub = f64::min(1.35, 0.7 + 0.25 * (1.0 + (to_delay - from_delay).abs() / 5.0).log10());
        let land = 0.5;
        let l0 = p.store.live_edge();
        let lands_at = intro + scrub;
        // Start decoding now so the picture is ready exactly when the scrub lands. Live starts
        // early enough that its frames, which arrive late by the decoder's latency, are queued.
        let (target, scrub_to) = if to_delay == 0.0 {
            let reader = Reader::new(&p.store, p.format, l0 + lands_at - LIVE_LEAD)?;
            (Source { kind: SourceKind::Live, reader }, l0 + lands_at)
        } else {
            let reader = Reader::new(&p.store, p.format, l0 + lands_at - to_delay)?;
            let start = reader.start_time;
            (Source { kind: SourceKind::Delayed, reader }, start)
        };
        let t = &p.config.transition;
        let sfx = t.sound.then(|| tape_whoosh(kind, lands_at + 0.25, t.sound_volume));
        Ok(Transition {
            kind,
            from_delay,
            to_delay,
            intro,
            scrub,
            land,
            target,
            old: p.source.take(),
            done: false,
            fps: p.fps(),
            scrub_from: l0 - from_delay + intro,
            scrub_to,
            // The track's left end is the larger of the two delays, so the knob travels the full width.
            span: from_delay.max(to_delay),
            sfx,
            i: 0,
            waited: 0,
            draw_every: u64::from((p.format.fps as f64 / f64::from(MAX_DRAW_FPS)).round().max(1.0) as u32),
            drawn: None,
            held: None,
            last_scrub: None,
            incoming: None,
        })
    }

    fn progress(&self) -> f64 {
        clamp01(self.i as f64 / self.fps / (self.intro + self.scrub + self.land))
    }

    fn target_ready(&self) -> bool {
        self.target.reader.ready()
    }

    /// `late`: the clock is catching up, so this frame repeats the last picture if it can.
    fn render(&mut self, p: &mut Playout, late: bool) -> AvFrame {
        let t = self.i as f64 / self.fps;
        let audio_off = self.i as usize * p.audio_samples;
        // Sources and audio advance every frame; the picture is redrawn only every few frames.
        let draw = (self.i.is_multiple_of(self.draw_every) && !late) || self.drawn.is_none();
        self.i += 1;
        let out = if t < self.intro {
            self.intro_phase(p, draw, t)
        } else if self.scrubbing(t) {
            self.scrub_phase(p, draw, t)
        } else {
            self.land_phase(p, draw, t)
        };
        if draw {
            self.draw_graphics(p, &out);
            self.drawn = Some(p.compositor.finish());
        }
        let mut audio = out.audio.unwrap_or_else(|| vec![0; p.audio_samples]);
        if let Some(sfx) = &self.sfx {
            mix_into(&mut audio, 1.0, Some(sfx), audio_off);
        }
        let video = self.drawn.clone().unwrap_or_else(|| p.standby_frame());
        AvFrame { video, audio }
    }

    /// Scrub until its time is up — and hold there while the target decoder warms up.
    fn scrubbing(&self, t: f64) -> bool {
        t < self.intro + self.scrub || (!self.target_ready() && (self.waited as f64) < READY_TIMEOUT * self.fps)
    }

    /// Intro: keep the old picture moving while it dims.
    fn intro_phase(&mut self, p: &mut Playout, draw: bool, t: f64) -> PhaseOutput {
        let e = ease_out(t / self.intro);
        let mut src = None;
        if let Some(mut old) = self.old.take() {
            src = p.pull_from(&mut old, Correction::None);
            self.old = Some(old);
        }
        if let Some(f) = src.as_mut() {
            p.last_frame = Some(f.video.clone());
            mix_into(&mut f.audio, 1.0 - e, None, 0);
        }
        if draw {
            let base = src
                .as_ref()
                .map(|f| f.video.clone())
                .or_else(|| p.last_frame.clone())
                .unwrap_or_else(|| p.standby_frame());
            let (w, h) = (p.format.w as f32, p.format.h as f32);
            if let Some(img) = p.compositor.image(&base) {
                draw_toned(p.compositor.canvas(), &img, w, h, (1.0 - 0.4 * e) as f32, (1.0 - 0.55 * e) as f32, 1.0);
            }
            let snap = p.compositor.snapshot();
            tape(
                p.compositor.canvas(),
                &snap,
                w,
                h,
                (0.45 * e) as f32,
                (self.i as u32).wrapping_mul(7919),
                ((t * 1.7) % 1.0) as f32,
            );
            self.held = Some(p.compositor.snapshot());
        }
        PhaseOutput {
            audio: src.map(|f| f.audio),
            center: (e, 0.55 + 0.45 * back_out(t / self.intro), 0.0),
            delay: self.from_delay,
            line_alpha: e,
        }
    }

    /// Scrub: race through thumbnails from the old position to the new one.
    fn scrub_phase(&mut self, p: &mut Playout, draw: bool, t: f64) -> PhaseOutput {
        if t >= self.intro + self.scrub {
            self.waited += 1;
        }
        // The old reader is no longer needed once the scrub starts.
        self.old = None;
        let u = ease_in_out(clamp01((t - self.intro) / self.scrub));
        if draw {
            let (w, h) = (p.format.w as f32, p.format.h as f32);
            let pos = self.scrub_from + (self.scrub_to - self.scrub_from) * u;
            let thumb = p.store.thumb_at(pos).and_then(|th| p.compositor.image(&th.pic));
            match thumb.or_else(|| self.held.clone()) {
                Some(img) => draw_toned(p.compositor.canvas(), &img, w, h, 0.62, 0.42, 1.0),
                None => {
                    p.compositor.canvas().clear(skia_safe::Color::BLACK);
                }
            }
            let snap = p.compositor.snapshot();
            tape(
                p.compositor.canvas(),
                &snap,
                w,
                h,
                0.95,
                (self.i as u32).wrapping_mul(7919),
                ((t * 1.9) % 1.0) as f32,
            );
            self.last_scrub = Some(p.compositor.snapshot());
        }
        PhaseOutput {
            audio: None,
            center: (1.0, 1.0 + 0.03 * (t * 18.0).sin(), u),
            delay: self.from_delay + (self.to_delay - self.from_delay) * u,
            line_alpha: 1.0,
        }
    }

    /// Land: crossfade into the new source while the graphics fade out.
    fn land_phase(&mut self, p: &mut Playout, draw: bool, t: f64) -> PhaseOutput {
        let e = ease_out(clamp01((t - self.intro - self.scrub - self.waited as f64 / self.fps) / self.land));
        let mut src = p.pull_from(&mut self.target, Correction::Fast);
        if let Some(f) = src.as_mut() {
            self.incoming = Some(f.video.clone());
            mix_into(&mut f.audio, e, None, 0);
        }
        if draw {
            let (w, h) = (p.format.w as f32, p.format.h as f32);
            let from = self.last_scrub.clone().or_else(|| self.held.clone());
            if let Some(img) = &from {
                draw_toned(p.compositor.canvas(), img, w, h, 1.0, 1.0, 1.0);
            }
            if let Some(incoming) = self.incoming.clone()
                && let Some(img) = p.compositor.image(&incoming)
            {
                draw_toned(p.compositor.canvas(), &img, w, h, 1.0, 1.0, e as f32);
                let snap = p.compositor.snapshot();
                tape(
                    p.compositor.canvas(),
                    &snap,
                    w,
                    h,
                    (0.6 * (1.0 - e)) as f32,
                    (self.i as u32).wrapping_mul(7919),
                    ((t * 1.9) % 1.0) as f32,
                );
            }
        }
        if e >= 1.0 {
            self.done = true;
        }
        PhaseOutput {
            audio: src.map(|f| f.audio),
            center: (1.0 - e, 1.0 + 0.18 * e, 1.0),
            delay: self.to_delay,
            line_alpha: 1.0 - e,
        }
    }

    fn draw_graphics(&self, p: &mut Playout, out: &PhaseOutput) {
        let (alpha, pop, progress) = out.center;
        let show_live = self.to_delay == 0.0 && progress > 0.97;
        let center = CenterParams {
            kind: self.kind,
            alpha: alpha as f32,
            pop: pop as f32,
            progress: progress as f32,
            timecode: if show_live { "LIVE".into() } else { format::clock(out.delay, true) },
            label: self.label(),
        };
        let timeline = TimelineParams { alpha: out.line_alpha as f32, span: self.span, knob: out.delay };
        let canvas = p.compositor.canvas();
        p.overlay.draw_center(canvas, &center);
        p.overlay.draw_timeline(canvas, &timeline);
    }

    fn label(&self) -> &'static str {
        match self.kind {
            TransitionKind::Rewind => "REWINDING",
            TransitionKind::Forward if self.to_delay == 0.0 => "BACK TO LIVE",
            TransitionKind::Forward => "SKIPPING AHEAD",
        }
    }
}

//! The app: config, and the pipeline built from it (buffer, ingest, playout). Settings that
//! change the pipeline's shape (picture size, frame rate, the OBS connection) wait until
//! nothing is streaming, then the pipeline is rebuilt.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::config::{self, Config};
use crate::media::ingest::{Ingest, IngestEvent};
use crate::media::output::PreviewImage;
use crate::media::store::Store;
use crate::playout::{Command, PlayoutHandle};
use crate::state::{AppState, BufferState, CommandResult, ConfigResponse, IngestState, Mode, PlayoutState};

struct Pipeline {
    store: Arc<Store>,
    ingest: Ingest,
    playout: PlayoutHandle,
}

impl Pipeline {
    fn shutdown(mut self) {
        self.ingest.stop();
        self.playout.shutdown();
        self.store.dispose();
    }
}

struct Inner {
    config: Config,
    pending: Option<Config>,
    pipeline: Option<Pipeline>,
}

pub struct App {
    inner: Mutex<Inner>,
    preview: Mutex<Option<Arc<PreviewImage>>>,
    preview_seq: AtomicU64,
    repaint: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl App {
    pub fn start(config: Config) -> Arc<App> {
        let app = Arc::new(App {
            inner: Mutex::new(Inner { config: config.clone(), pending: None, pipeline: None }),
            preview: Mutex::new(None),
            preview_seq: AtomicU64::new(0),
            repaint: Mutex::new(None),
        });
        let pipeline = build(&app, &config);
        app.lock().pipeline = Some(pipeline);
        let weak = Arc::downgrade(&app);
        let _ = thread::Builder::new().name("supervisor".into()).spawn(move || supervise(&weak));
        app
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Called whenever there's something new to show (preview frames).
    pub fn on_repaint(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.repaint.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
    }

    fn set_preview(&self, img: PreviewImage) {
        *self.preview.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(img));
        self.preview_seq.fetch_add(1, Ordering::Release);
        if let Some(f) = self.repaint.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            f();
        }
    }

    /// The latest preview picture and its sequence number.
    pub fn preview(&self) -> (u64, Option<Arc<PreviewImage>>) {
        let seq = self.preview_seq.load(Ordering::Acquire);
        (seq, self.preview.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    pub fn config(&self) -> ConfigResponse {
        let inner = self.lock();
        ConfigResponse {
            config: inner.pending.clone().unwrap_or_else(|| inner.config.clone()),
            restart_pending: inner.pending.is_some(),
        }
    }

    pub fn state(&self) -> AppState {
        let inner = self.lock();
        let c = &inner.config;
        let (ingest, buffer, playout) = match &inner.pipeline {
            Some(p) => (
                IngestState {
                    connected: p.ingest.connected(),
                    server: p.ingest.url.clone(),
                    key: c.ingest.key.clone(),
                    info: if p.ingest.connected() { p.ingest.info() } else { None },
                    error: p.ingest.error(),
                },
                BufferState { recorded: p.store.recorded().floor(), capacity: c.buffer_minutes * 60.0 },
                p.playout.snapshot().state,
            ),
            None => (
                IngestState {
                    connected: false,
                    server: String::new(),
                    key: c.ingest.key.clone(),
                    info: None,
                    error: None,
                },
                BufferState { recorded: 0.0, capacity: c.buffer_minutes * 60.0 },
                PlayoutState::idle(c),
            ),
        };
        AppState {
            ingest,
            buffer,
            playout,
            delay_seconds: c.delay_seconds,
            badge: c.badge,
            restart_pending: inner.pending.is_some(),
        }
    }

    /// The latest transition, numbered so a new one can be told apart.
    pub fn transition_event(&self) -> Option<(u64, crate::state::TransitionEvent)> {
        self.lock().pipeline.as_ref().and_then(|p| p.playout.snapshot().event)
    }

    pub fn command(&self, cmd: Command) -> CommandResult {
        let inner = self.lock();
        match &inner.pipeline {
            Some(p) => p.playout.command(cmd),
            None => CommandResult::error("nicedelay is restarting with new settings."),
        }
    }

    /// Applies a partial config (JSON with any of the config's fields) and saves it.
    pub fn update_config(&self, patch: &Value) -> ConfigResponse {
        let mut inner = self.lock();
        let next = config::merge(&inner.config, patch);
        if let Err(e) = config::save(&next) {
            tracing::warn!("couldn't save the config: {e}");
        }
        let structural = next.output != inner.config.output || next.ingest != inner.config.ingest;
        // Everything else applies right away.
        let (output, ingest) = (inner.config.output.clone(), inner.config.ingest.clone());
        inner.config = Config { output, ingest, ..next.clone() };
        if let Some(p) = &inner.pipeline {
            p.store.set_buffer_seconds(inner.config.buffer_minutes * 60.0);
            p.playout.set_config(inner.config.clone());
        }
        inner.pending = structural.then_some(next);
        ConfigResponse {
            config: inner.pending.clone().unwrap_or_else(|| inner.config.clone()),
            restart_pending: inner.pending.is_some(),
        }
    }

    pub fn shutdown(&self) {
        let pipeline = self.lock().pipeline.take();
        if let Some(p) = pipeline {
            p.shutdown();
        }
    }
}

fn build(app: &Arc<App>, config: &Config) -> Pipeline {
    let store = Arc::new(
        Store::new(config.buffer_minutes * 60.0, None)
            .unwrap_or_else(|e| panic!("can't create the buffer directory: {e}")),
    );
    let mut ingest = Ingest::new(config, store.clone());
    let weak = Arc::downgrade(app);
    let preview = Arc::new(move |img| {
        if let Some(app) = weak.upgrade() {
            app.set_preview(img);
        }
    });
    let playout = PlayoutHandle::start(config.clone(), store.clone(), ingest.view(), preview);
    let notify = playout.notifier();
    ingest.run(Arc::new(move |e| match e {
        IngestEvent::Connected => {
            tracing::info!("OBS connected");
            notify();
        }
        IngestEvent::Disconnected => tracing::info!("OBS disconnected"),
    }));
    Pipeline { store, ingest, playout }
}

/// Rebuilds the pipeline with pending settings once nothing is streaming.
fn supervise(app: &Weak<App>) {
    loop {
        thread::sleep(Duration::from_millis(250));
        let Some(app) = app.upgrade() else { return };
        let ready = {
            let inner = app.lock();
            inner.pending.is_some()
                && inner
                    .pipeline
                    .as_ref()
                    .is_some_and(|p| !p.ingest.connected() && p.playout.snapshot().state.mode == Mode::Idle)
        };
        if !ready {
            continue;
        }
        tracing::info!("applying new output/ingest settings");
        let (old, config) = {
            let mut inner = app.lock();
            let Some(next) = inner.pending.take() else { continue };
            inner.config = next.clone();
            (inner.pipeline.take(), next)
        };
        if let Some(p) = old {
            p.shutdown();
        }
        let pipeline = build(&app, &config);
        app.lock().pipeline = Some(pipeline);
    }
}

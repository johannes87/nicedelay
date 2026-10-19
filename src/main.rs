mod app;
mod config;
mod format;
mod gui;
mod http;
mod media;
mod playout;
mod render;
mod state;

use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("nicedelay=info")))
        .with_target(false)
        .init();
    if let Err(e) = ffmpeg_next::init() {
        eprintln!("FFmpeg didn't start: {e}");
        std::process::exit(1);
    }
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
    let headless = std::env::args().any(|a| a == "--headless");

    let cfg = config::load();
    let app = app::App::start(cfg.clone());
    match http::serve(app.clone(), &cfg.http_host, cfg.http_port) {
        Ok(()) => tracing::info!("API at http://{}:{}/api/state", cfg.http_host, cfg.http_port),
        Err(e) => tracing::warn!("{e}"),
    }
    let ingest = app.state().ingest;
    tracing::info!("OBS server {}   stream key {}", ingest.server, ingest.key);
    tracing::info!("config: {}", config::config_path().display());

    if headless {
        wait_for_ctrl_c();
    } else if let Err(e) = gui::run(app.clone()) {
        eprintln!("the window couldn't open ({e}); run with --headless to use nicedelay without it");
    }
    app.shutdown();
}

/// Blocks until Ctrl+C or SIGTERM.
pub fn wait_for_ctrl_c() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    rt.block_on(async {
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
    });
}

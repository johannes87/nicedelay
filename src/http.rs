//! The HTTP API, for Stream Deck, scripts and similar tools:
//!
//!   GET  /api/state            what's happening
//!   POST /api/toggle           delay ↔ live
//!   POST /api/delay            {"seconds": 30} (optional; default: the configured delay)
//!   POST /api/live
//!   GET  /api/config, PUT /api/config (any subset of the config's fields)

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::Value;

use crate::app::App;
use crate::playout::Command;
use crate::state::CommandResult;

/// Lenient JSON body: anything that isn't a JSON object counts as empty.
fn body(bytes: &Bytes) -> Value {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(v @ Value::Object(_)) => v,
        _ => Value::Object(Default::default()),
    }
}

/// Runs a command off the async runtime (it waits for the playout thread).
async fn command(app: Arc<App>, cmd: Command) -> Response {
    let result = tokio::task::spawn_blocking(move || app.command(cmd))
        .await
        .unwrap_or_else(|_| CommandResult::error("internal error"));
    let status = if result.ok { StatusCode::OK } else { StatusCode::CONFLICT };
    (status, Json(result)).into_response()
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/state", get(|State(app): State<Arc<App>>| async move { Json(app.state()) }))
        .route("/api/toggle", post(|State(app): State<Arc<App>>| command(app, Command::Toggle)))
        .route("/api/live", post(|State(app): State<Arc<App>>| command(app, Command::GoLive)))
        .route(
            "/api/delay",
            post(|State(app): State<Arc<App>>, bytes: Bytes| async move {
                let seconds = body(&bytes).get("seconds").and_then(Value::as_f64);
                command(app, Command::SetDelay(seconds)).await
            }),
        )
        .route(
            "/api/config",
            get(|State(app): State<Arc<App>>| async move { Json(app.config()) }).put(
                |State(app): State<Arc<App>>, bytes: Bytes| async move {
                    let patch = body(&bytes);
                    let res = tokio::task::spawn_blocking(move || app.update_config(&patch)).await;
                    match res {
                        Ok(r) => Json(r).into_response(),
                        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                },
            ),
        )
        .with_state(app)
}

/// Serves the API on its own thread. Returns an error message if the port is taken.
pub fn serve(app: Arc<App>, host: &str, port: u16) -> Result<(), String> {
    let addr = format!("{host}:{port}");
    let listener = std::net::TcpListener::bind(&addr).map_err(|e| format!("can't serve the API on {addr}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    std::thread::Builder::new()
        .name("http".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(rt) => rt,
                Err(e) => return tracing::warn!("http: {e}"),
            };
            rt.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
                if let Err(e) = axum::serve(listener, router(app)).await {
                    tracing::warn!("http: {e}");
                }
            });
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

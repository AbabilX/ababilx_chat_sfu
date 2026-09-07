use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::config::Config;
use crate::sfu::EngineHandle;

#[derive(Clone)]
pub struct AppState {
    pub engine: EngineHandle,
    pub config: Arc<Config>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(upgrade))
        .route("/diag", get(diag))
        .layer(middleware::from_fn(trace_request))
        .with_state(state)
}

/// Liveness only. It deliberately reports nothing about rooms: a call in
/// trouble must not make an orchestrator restart the process and drop every
/// other call with it.
async fn healthz() -> Response {
    Json(json!({"status": "ok", "service": env!("CARGO_PKG_NAME"), "version": env!("CARGO_PKG_VERSION")}))
        .into_response()
}

/// A page for a human to open in the browser that is failing.
///
/// A client-side join dies entirely inside the browser and leaves no trace on
/// any server, and reading the console is not always available (Safari hides it
/// behind the Develop menu). Loading this page proves the browser can reach the
/// service over TCP, and the script then opens a real websocket back and puts
/// the outcome on screen — so the answer is visible without any tooling, and
/// the attempt shows up in this log either way.
async fn diag(headers: HeaderMap) -> Response {
    tracing::info!(agent = %header_or_dash(&headers, "user-agent"), "diagnostics page opened");
    Html(include_str!("diag.html")).into_response()
}

fn header_or_dash(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-")
        .to_string()
}

/// Logs every request that reaches the process.
///
/// This is the only thing that separates "the browser never connected" from
/// "it connected and the upgrade was rejected": in both cases the websocket
/// handler never runs, so from inside the session the two are identical. The
/// health probe is excluded or it would be the only thing in the log.
async fn trace_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let quiet = path == "/healthz";
    let response = next.run(request).await;
    if !quiet {
        tracing::info!(%method, %path, status = response.status().as_u16(), "http");
    }
    response
}

/// The one signaling entry point. Authentication happens inside, on the first
/// frame, so an unauthenticated socket never reaches the media loop.
async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Origin and user-agent name WHICH browser this is, which is the missing
    // half whenever one client works and another does not.
    tracing::info!(
        origin = %header_or_dash(&headers, "origin"),
        agent = %header_or_dash(&headers, "user-agent"),
        "websocket upgrade requested"
    );
    ws.on_upgrade(move |socket| session::run(socket, state))
}

use super::session;

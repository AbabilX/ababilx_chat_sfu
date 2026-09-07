use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};

use crate::auth;
use crate::proto::{ClientMsg, ServerMsg};
use crate::sfu::{Command, JoinRequest, PeerId};

use super::http::AppState;

/// A browser has this long to send its join frame before the socket is closed.
const JOIN_DEADLINE: Duration = Duration::from_secs(10);

pub async fn run(socket: WebSocket, state: AppState) {
    // Logged before anything can go wrong, so "the browser never reached us" is
    // always distinguishable from "we refused it". Every refusal below is a
    // warn: a socket that opens and then vanishes with no line at all used to
    // be indistinguishable from one that never opened.
    tracing::info!("signaling socket open");
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerMsg>();

    let Some(peer) = admit(&mut stream, &out_tx, &state).await else {
        // The refusal has to reach the browser BEFORE the close, or all it
        // learns is that the socket went away.
        while let Ok(message) = out_rx.try_recv() {
            let _ = sink.send(encode(&message)).await;
        }
        let _ = sink.send(close_frame()).await;
        return;
    };

    let writer = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            if sink.send(encode(&message)).await.is_err() {
                break;
            }
        }
        let _ = sink.send(close_frame()).await;
    });

    while let Some(Ok(message)) = stream.next().await {
        let text = match message {
            Message::Text(text) => text.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        if !forward(&text, peer, &state, &out_tx) {
            break;
        }
    }

    tracing::debug!(peer = peer.0, "signaling socket closed");
    state.engine.send(Command::Leave { peer });
    writer.abort();
}

/// Reads frames until the join arrives, verifies the token, and hands the offer
/// to the media loop. Returns the assigned peer id.
async fn admit(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    out_tx: &mpsc::UnboundedSender<ServerMsg>,
    state: &AppState,
) -> Option<PeerId> {
    let first = match tokio::time::timeout(JOIN_DEADLINE, stream.next()).await {
        Err(_) => {
            tracing::warn!(seconds = JOIN_DEADLINE.as_secs(), "join refused: no join frame before deadline");
            return None;
        }
        Ok(None) => {
            tracing::warn!("join refused: socket closed before any frame");
            return None;
        }
        Ok(Some(Err(error))) => {
            tracing::warn!(%error, "join refused: socket error before join");
            return None;
        }
        Ok(Some(Ok(message))) => message,
    };
    let Message::Text(text) = first else {
        tracing::warn!("join refused: first frame was not text");
        return None;
    };
    let Ok(ClientMsg::Join { token, sdp }) = serde_json::from_str::<ClientMsg>(&text) else {
        tracing::warn!(bytes = text.len(), "join refused: first frame is not a join");
        let _ = out_tx.send(ServerMsg::error("expected_join", "First frame must be a join."));
        return None;
    };
    let claims = match auth::verify(&token, &state.config.shared_secret) {
        Ok(claims) => claims,
        Err(error) => {
            // The reason matters: an expired token, a wrong shared secret and a
            // truncated token are three different operator mistakes.
            tracing::warn!(%error, "join refused: token rejected");
            let _ = out_tx.send(ServerMsg::error("unauthorized", "Join token rejected."));
            return None;
        }
    };
    tracing::info!(user = %claims.sub, room = %claims.room, "join token accepted");
    let (assigned_tx, assigned_rx) = oneshot::channel();
    let request = JoinRequest {
        claims,
        sdp,
        out: out_tx.clone(),
        assigned: assigned_tx,
    };
    if !state.engine.send(Command::Join(Box::new(request))) {
        tracing::error!("join refused: media loop is gone");
        let _ = out_tx.send(ServerMsg::error("unavailable", "The call service is restarting."));
        return None;
    }
    // A None here means the media loop refused it (room full, unparseable
    // offer); those log their own reason. An Err means the loop dropped the
    // channel, which nothing else would report.
    match assigned_rx.await {
        Ok(assigned) => assigned,
        Err(_) => {
            tracing::error!("join refused: media loop dropped the request");
            None
        }
    }
}

/// Applies one client frame. Returns false when the socket should close.
fn forward(
    text: &str,
    peer: PeerId,
    state: &AppState,
    out_tx: &mpsc::UnboundedSender<ServerMsg>,
) -> bool {
    let Ok(message) = serde_json::from_str::<ClientMsg>(text) else {
        let _ = out_tx.send(ServerMsg::error("bad_message", "Unrecognised frame."));
        return true;
    };
    match message {
        ClientMsg::Answer { sdp } => state.engine.send(Command::Answer { peer, sdp }),
        ClientMsg::Mute { muted } => state.engine.send(Command::Mute { peer, muted }),
        ClientMsg::Screen { active } => state.engine.send(Command::Screen { peer, active }),
        ClientMsg::Ping => out_tx.send(ServerMsg::Pong).is_ok(),
        ClientMsg::Leave => false,
        // Only the SFU offers once a peer is admitted, so a second join or a
        // client offer is a protocol error rather than something to merge.
        ClientMsg::Join { .. } => {
            let _ = out_tx.send(ServerMsg::error("already_joined", "Already in a call."));
            true
        }
    }
}

fn encode(message: &ServerMsg) -> Message {
    Message::Text(
        serde_json::to_string(message)
            .unwrap_or_else(|_| "{\"t\":\"error\",\"code\":\"encode\"}".to_string())
            .into(),
    )
}

fn close_frame() -> Message {
    Message::Close(None)
}

//! `GET /relay`: the embedded Nostr relay over a websocket.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;

use crate::api::App;
use crate::state::UiEvent;

pub async fn handler(State(s): State<App>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(256 * 1024).on_upgrade(move |socket| serve(socket, s))
}

async fn serve(mut socket: WebSocket, s: App) {
    let mut conn = s.relay.connect();
    s.emit(UiEvent::Relay { connections: s.relay.stats().connections });
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        for reply in conn.handle(&text) {
                            if socket.send(Message::Text(reply)).await.is_err() { break; }
                        }
                    }
                    Some(Ok(Message::Binary(b))) => {
                        if let Ok(text) = String::from_utf8(b) {
                            for reply in conn.handle(&text) {
                                if socket.send(Message::Text(reply)).await.is_err() { break; }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(p))) => { let _ = socket.send(Message::Pong(p)).await; }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                }
            }
            push = conn.next_push() => {
                match push {
                    Some(text) => { if socket.send(Message::Text(text)).await.is_err() { break; } }
                    None => break,
                }
            }
        }
    }
    drop(conn);
    s.emit(UiEvent::Relay { connections: s.relay.stats().connections });
}

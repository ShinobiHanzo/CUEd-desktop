//! A real websocket server built on the `Connection` API, driven by two
//! tungstenite clients. The `serve_socket` function is the loop `cuedd` runs
//! per `/relay` socket (with axum's `WebSocket` in place of tungstenite's stream).

use std::sync::Arc;
use std::time::Duration;

use cued_proto::nostr::{self, parse_relay_message, RelayMessage};
use cued_proto::Identity;
use cued_relay::Relay;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// One websocket, one relay connection, until the socket closes.
async fn serve_socket(relay: Arc<Relay>, stream: TcpStream) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else { return };
    let (mut sink, mut source) = ws.split();
    let mut conn = relay.connect();
    loop {
        tokio::select! {
            incoming = source.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    for reply in conn.handle(&text) {
                        if sink.send(Message::Text(reply)).await.is_err() {
                            return;
                        }
                    }
                }
                Some(Ok(Message::Ping(p))) => {
                    if sink.send(Message::Pong(p)).await.is_err() {
                        return;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {} // binary, pong, frame: ignored
            },
            push = conn.next_push() => match push {
                Some(frame) => {
                    if sink.send(Message::Text(frame)).await.is_err() {
                        return;
                    }
                }
                None => return,
            },
        }
    }
    // `conn` drops here: subscriptions and the connection count go with it.
}

async fn start_server(relay: Arc<Relay>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_socket(Arc::clone(&relay), stream));
        }
    });
    format!("ws://{addr}/relay")
}

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn recv(ws: &mut Client) -> RelayMessage {
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.expect("timed out").unwrap().unwrap();
    match msg {
        Message::Text(t) => parse_relay_message(&t),
        other => panic!("unexpected frame {other:?}"),
    }
}

#[tokio::test]
async fn publish_on_one_socket_receive_on_another() {
    let relay = Relay::open(None).unwrap();
    let url = start_server(Arc::clone(&relay)).await;

    let (mut listener, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut publisher, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    listener.send(Message::Text(nostr::req_message("stations", &[serde_json::json!({"kinds":[30777],"#d":["cued"]})]))).await.unwrap();
    assert_eq!(recv(&mut listener).await, RelayMessage::Eose("stations".into()));

    let phone = Identity::generate();
    let state = phone.sign_now(30777, vec![vec!["d".into(), "cued".into()]], r#"{"track":"x"}"#.into());
    publisher.send(Message::Text(nostr::event_message(&state))).await.unwrap();
    assert_eq!(recv(&mut publisher).await, RelayMessage::Ok { event_id: state.id.clone(), accepted: true, message: String::new() });
    assert_eq!(recv(&mut listener).await, RelayMessage::Event { sub_id: "stations".into(), event: state.clone() });

    // a rejected event comes back as OK false
    let mut forged = state.clone();
    forged.content = "forged".into();
    publisher.send(Message::Text(nostr::event_message(&forged))).await.unwrap();
    assert_eq!(recv(&mut publisher).await, RelayMessage::Ok { event_id: forged.id, accepted: false, message: "invalid: id mismatch".into() });

    // a third client reads it from storage
    let (mut late, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    late.send(Message::Text(nostr::req_message("l", &[serde_json::json!({"authors":[phone.pubkey_hex()]})]))).await.unwrap();
    assert_eq!(recv(&mut late).await, RelayMessage::Event { sub_id: "l".into(), event: state });
    assert_eq!(recv(&mut late).await, RelayMessage::Eose("l".into()));

    assert_eq!(relay.stats().connections, 3);
    assert_eq!(relay.stats().subscriptions, 2);
    listener.close(None).await.unwrap();
    drop(listener);
    tokio::time::timeout(Duration::from_secs(5), async {
        while relay.stats().connections != 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("closed socket releases its connection");
    assert_eq!(relay.stats().subscriptions, 1);
}

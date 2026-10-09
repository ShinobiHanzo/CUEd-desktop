//! In-process edge + client + echo server, driven over real TCP sockets.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use cued_funnel::tls::{self, PinnedFingerprintVerifier, TlsForwarder};
use cued_funnel::{run_client, CancellationToken, ClientConfig, Edge, EdgeConfig, FunnelStatus};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const PSK: &[u8] = b"0123456789abcdef0123456789abcdef";

/// A TCP echo server; returns its address. Connections are served until the
/// client closes its write side.
async fn echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            });
        }
    });
    addr
}

struct RunningEdge {
    public: SocketAddr,
    control: SocketAddr,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn start_edge(control: SocketAddr, psk: &[u8]) -> RunningEdge {
    let edge = Edge::bind(EdgeConfig { listen: "127.0.0.1:0".parse().unwrap(), control, psk: psk.to_vec() }).await.unwrap();
    let (public, control) = (edge.listen_addr(), edge.control_addr());
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(edge.run(shutdown.clone()));
    RunningEdge { public, control, shutdown, task }
}

fn start_client(control: SocketAddr, psk: &[u8], forward_to: SocketAddr) -> (Arc<FunnelStatus>, CancellationToken) {
    let status = Arc::new(FunnelStatus::new());
    let shutdown = CancellationToken::new();
    let cfg = ClientConfig { control: control.to_string(), psk: psk.to_vec(), forward_to };
    tokio::spawn(run_client(cfg, Arc::clone(&status), shutdown.clone()));
    (status, shutdown)
}

async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

async fn roundtrip(public: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let mut sock = TcpStream::connect(public).await.unwrap();
    sock.write_all(payload).await.unwrap();
    let mut got = vec![0u8; payload.len()];
    tokio::time::timeout(Duration::from_secs(5), sock.read_exact(&mut got)).await.expect("echo timed out").unwrap();
    sock.shutdown().await.unwrap();
    got
}

#[tokio::test]
async fn echo_through_edge_and_client() {
    let echo = echo_server().await;
    let edge = start_edge("127.0.0.1:0".parse().unwrap(), PSK).await;
    let (status, client_stop) = start_client(edge.control, PSK, echo);
    wait_for("client to connect", || status.is_connected()).await;

    assert_eq!(roundtrip(edge.public, b"hello through the funnel").await, b"hello through the funnel");
    // a larger-than-one-frame payload
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(roundtrip(edge.public, &big).await, big);

    wait_for("streams to close", || status.snapshot().streams_open == 0).await;
    let snap = status.snapshot();
    assert_eq!(snap.streams_total, 2);
    assert!(snap.bytes_in >= big.len() as u64 + 24);
    assert!(snap.bytes_out >= big.len() as u64 + 24);
    assert_eq!(snap.last_error, None);

    client_stop.cancel();
    wait_for("client to disconnect", || !status.is_connected()).await;
    edge.shutdown.cancel();
    edge.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn two_public_connections_interleaved() {
    let echo = echo_server().await;
    let edge = start_edge("127.0.0.1:0".parse().unwrap(), PSK).await;
    let (status, _client_stop) = start_client(edge.control, PSK, echo);
    wait_for("client to connect", || status.is_connected()).await;

    let mut a = TcpStream::connect(edge.public).await.unwrap();
    let mut b = TcpStream::connect(edge.public).await.unwrap();
    for i in 0..20u8 {
        a.write_all(&[b'a', i]).await.unwrap();
        b.write_all(&[b'b', i]).await.unwrap();
        let mut got = [0u8; 2];
        b.read_exact(&mut got).await.unwrap();
        assert_eq!(got, [b'b', i]);
        a.read_exact(&mut got).await.unwrap();
        assert_eq!(got, [b'a', i]);
    }
    wait_for("two streams open", || status.snapshot().streams_open == 2).await;
    // closing one leaves the other working
    a.shutdown().await.unwrap();
    drop(a);
    wait_for("one stream left", || status.snapshot().streams_open == 1).await;
    b.write_all(b"still here").await.unwrap();
    let mut got = [0u8; 10];
    b.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"still here");
    // the echo server closing its side closes the public socket too
    b.shutdown().await.unwrap();
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), b.read_to_end(&mut rest)).await.unwrap().unwrap();
    assert!(rest.is_empty());
    wait_for("no streams", || status.snapshot().streams_open == 0).await;
    edge.shutdown.cancel();
}

#[tokio::test]
async fn wrong_psk_is_refused_and_public_connections_closed_without_client() {
    let echo = echo_server().await;
    let edge = start_edge("127.0.0.1:0".parse().unwrap(), PSK).await;

    // no client yet: public connections are closed at once
    let mut sock = TcpStream::connect(edge.public).await.unwrap();
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf)).await.unwrap().unwrap();
    assert_eq!(n, 0);

    // manual handshake with a wrong proof: the edge closes without the OK byte
    let mut control = TcpStream::connect(edge.control).await.unwrap();
    let mut nonce = [0u8; 32];
    control.read_exact(&mut nonce).await.unwrap();
    control.write_all(&cued_funnel::frame::handshake_proof(b"wrong", &nonce)).await.unwrap();
    let n = tokio::time::timeout(Duration::from_secs(5), control.read(&mut buf)).await.unwrap().unwrap();
    assert_eq!(n, 0, "edge must close instead of replying");

    // a client with the wrong key never becomes connected and reports why
    let (status, client_stop) = start_client(edge.control, b"not the key", echo);
    wait_for("client to record the refusal", || status.snapshot().last_error.is_some()).await;
    assert!(!status.is_connected());
    assert!(status.snapshot().last_error.unwrap().contains("pre-shared key"));
    let mut sock = TcpStream::connect(edge.public).await.unwrap();
    let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf)).await.unwrap().unwrap();
    assert_eq!(n, 0);
    client_stop.cancel();
    edge.shutdown.cancel();
}

#[tokio::test]
async fn client_reconnects_after_edge_restart_and_new_client_replaces_old() {
    let echo = echo_server().await;
    let first = start_edge("127.0.0.1:0".parse().unwrap(), PSK).await;
    let control = first.control;
    let (status, _client_stop) = start_client(control, PSK, echo);
    wait_for("client to connect", || status.is_connected()).await;
    assert_eq!(roundtrip(first.public, b"one").await, b"one");

    first.shutdown.cancel();
    first.task.await.unwrap().unwrap();
    wait_for("client to notice the drop", || !status.is_connected()).await;

    let second = start_edge(control, PSK).await;
    assert_eq!(second.control, control);
    wait_for("client to reconnect", || status.is_connected()).await;
    assert_eq!(roundtrip(second.public, b"two").await, b"two");

    // a second client attaching replaces the first one
    let (status2, client2_stop) = start_client(control, PSK, echo);
    wait_for("second client to connect", || status2.is_connected()).await;
    wait_for("first client to be replaced", || !status.is_connected()).await;
    assert_eq!(roundtrip(second.public, b"three").await, b"three");
    assert_eq!(status2.snapshot().streams_total, 1);
    client2_stop.cancel();
    // and the first one comes back on its own
    wait_for("first client to return", || status.is_connected()).await;
    assert_eq!(roundtrip(second.public, b"four").await, b"four");
    second.shutdown.cancel();
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("cued-funnel-{tag}-{}-{}", std::process::id(), rand_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn rand_suffix() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new().build_hasher().finish()
}

#[tokio::test]
async fn tls_forwarder_with_pinned_fingerprint() {
    let dir = temp_dir("tls");
    let (config, fp) = tls::load_or_create_self_signed(&dir, "Idris's laptop").unwrap();
    assert_eq!(fp.len(), 64);
    assert!(dir.join(tls::CERT_FILE).exists() && dir.join(tls::KEY_FILE).exists());
    let (_again, fp_again) = tls::load_or_create_self_signed(&dir, "whatever").unwrap();
    assert_eq!(fp, fp_again, "existing certificate is reused");

    let echo = echo_server().await;
    let forwarder = TlsForwarder::bind("127.0.0.1:0".parse().unwrap(), config, echo).await.unwrap();
    let tls_addr = forwarder.local_addr();
    let stop = CancellationToken::new();
    tokio::spawn(forwarder.run(stop.clone()));

    let connector = PinnedFingerprintVerifier::connector(&fp).unwrap();
    let name = rustls::pki_types::ServerName::try_from("whatever.example").unwrap();
    let tcp = TcpStream::connect(tls_addr).await.unwrap();
    let mut tls_stream = connector.connect(name.clone(), tcp).await.unwrap();
    tls_stream.write_all(b"encrypted hello").await.unwrap();
    let mut got = [0u8; 15];
    tls_stream.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"encrypted hello");
    tls_stream.shutdown().await.unwrap();

    // a different pin is refused during the handshake
    let other = tls::fingerprint_hex(b"some other certificate");
    let bad = PinnedFingerprintVerifier::connector(&other).unwrap();
    let tcp = TcpStream::connect(tls_addr).await.unwrap();
    let err = bad.connect(name, tcp).await.expect_err("handshake must fail");
    assert!(err.to_string().contains("fingerprint"), "{err}");

    stop.cancel();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The whole picture from protocol.md §5: phone ─TLS─▶ edge ─mux─▶ cuedd's TLS listener ─▶ plain HTTP port.
#[tokio::test]
async fn end_to_end_tls_through_the_funnel() {
    let dir = temp_dir("e2e");
    let plain = echo_server().await; // stands in for cuedd :8770
    let (config, fp) = tls::load_or_create_self_signed(&dir, "desk").unwrap();
    let forwarder = TlsForwarder::bind("127.0.0.1:0".parse().unwrap(), config, plain).await.unwrap();
    let tls_addr = forwarder.local_addr(); // stands in for cuedd :8771
    let stop = CancellationToken::new();
    tokio::spawn(forwarder.run(stop.clone()));

    let edge = start_edge("127.0.0.1:0".parse().unwrap(), PSK).await;
    let (status, client_stop) = start_client(edge.control, PSK, tls_addr);
    wait_for("client to connect", || status.is_connected()).await;

    let connector = PinnedFingerprintVerifier::connector(&fp).unwrap();
    let name = rustls::pki_types::ServerName::try_from("edge.example").unwrap();
    let tcp = TcpStream::connect(edge.public).await.unwrap();
    let mut phone = connector.connect(name, tcp).await.unwrap();
    phone.write_all(b"GET /relay").await.unwrap();
    let mut got = [0u8; 10];
    phone.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"GET /relay");
    phone.shutdown().await.unwrap();

    client_stop.cancel();
    edge.shutdown.cancel();
    stop.cancel();
    let _ = std::fs::remove_dir_all(&dir);
}

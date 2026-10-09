//! Mux framing and the PSK handshake.
//!
//! Frame: `type u8 | stream u32 BE | len u32 BE | payload`. Types: `1 OPEN`
//! (payload: the remote address as text), `2 DATA`, `3 CLOSE`, `4 PING`,
//! `5 PONG`. Payloads are at most 64 KiB.
//!
//! Handshake, right after the TCP connect: the edge sends a 32-byte random
//! nonce, the client answers `HMAC-SHA256(psk, nonce)` (32 bytes), the edge
//! replies the single byte `0x01` or closes the connection.

use std::io;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest payload one frame may carry.
pub const MAX_PAYLOAD: usize = 64 * 1024;
/// Size of the frame header.
pub const HEADER_LEN: usize = 9;
/// Nonce and HMAC length in the handshake.
pub const NONCE_LEN: usize = 32;
/// The edge's "authenticated" reply.
pub const HANDSHAKE_OK: u8 = 0x01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    Open = 1,
    Data = 2,
    Close = 3,
    Ping = 4,
    Pong = 5,
}

impl FrameType {
    pub fn from_u8(b: u8) -> Option<FrameType> {
        Some(match b {
            1 => FrameType::Open,
            2 => FrameType::Data,
            3 => FrameType::Close,
            4 => FrameType::Ping,
            5 => FrameType::Pong,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameType,
    pub stream: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn open(stream: u32, remote: &str) -> Frame {
        Frame { kind: FrameType::Open, stream, payload: remote.as_bytes().to_vec() }
    }
    pub fn data(stream: u32, payload: Vec<u8>) -> Frame {
        Frame { kind: FrameType::Data, stream, payload }
    }
    pub fn close(stream: u32) -> Frame {
        Frame { kind: FrameType::Close, stream, payload: Vec::new() }
    }
    pub fn ping() -> Frame {
        Frame { kind: FrameType::Ping, stream: 0, payload: Vec::new() }
    }
    pub fn pong() -> Frame {
        Frame { kind: FrameType::Pong, stream: 0, payload: Vec::new() }
    }

    /// Header plus payload, ready for the wire.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.push(self.kind as u8);
        out.extend_from_slice(&self.stream.to_be_bytes());
        out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.payload);
        out
    }
}

/// Reads one frame; `UnexpectedEof` at a clean close.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Frame> {
    let mut header = [0u8; HEADER_LEN];
    r.read_exact(&mut header).await?;
    let kind = FrameType::from_u8(header[0])
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("unknown frame type {}", header[0])))?;
    let stream = u32::from_be_bytes([header[1], header[2], header[3], header[4]]);
    let len = u32::from_be_bytes([header[5], header[6], header[7], header[8]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame payload {len} exceeds {MAX_PAYLOAD}")));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).await?;
    Ok(Frame { kind, stream, payload })
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> io::Result<()> {
    if frame.payload.len() > MAX_PAYLOAD {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame payload too large"));
    }
    w.write_all(&frame.encode()).await?;
    w.flush().await
}

/// `HMAC-SHA256(psk, nonce)`.
pub fn handshake_proof(psk: &[u8], nonce: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(psk).expect("hmac accepts any key length");
    mac.update(nonce);
    mac.finalize().into_bytes().into()
}

/// Constant-time check of a client's proof.
pub fn verify_proof(psk: &[u8], nonce: &[u8], proof: &[u8]) -> bool {
    let mut mac = Hmac::<Sha256>::new_from_slice(psk).expect("hmac accepts any key length");
    mac.update(nonce);
    mac.verify_slice(proof).is_ok()
}

/// Edge side of the handshake. On a bad proof the error is returned and the
/// caller drops the socket (no reply is sent).
pub async fn handshake_edge<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, psk: &[u8]) -> io::Result<()> {
    let nonce: [u8; NONCE_LEN] = rand::random();
    s.write_all(&nonce).await?;
    s.flush().await?;
    let mut proof = [0u8; NONCE_LEN];
    s.read_exact(&mut proof).await?;
    if !verify_proof(psk, &nonce, &proof) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "client presented a wrong pre-shared key"));
    }
    s.write_all(&[HANDSHAKE_OK]).await?;
    s.flush().await
}

/// Client side of the handshake.
pub async fn handshake_client<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, psk: &[u8]) -> io::Result<()> {
    let mut nonce = [0u8; NONCE_LEN];
    s.read_exact(&mut nonce).await?;
    s.write_all(&handshake_proof(psk, &nonce)).await?;
    s.flush().await?;
    let mut ok = [0u8; 1];
    match s.read_exact(&mut ok).await {
        Ok(_) if ok[0] == HANDSHAKE_OK => Ok(()),
        Ok(_) => Err(io::Error::new(io::ErrorKind::PermissionDenied, "edge refused the handshake")),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "edge closed the connection: wrong pre-shared key?"))
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_and_limits() {
        let frames = vec![Frame::open(7, "1.2.3.4:5"), Frame::data(7, vec![1, 2, 3]), Frame::close(7), Frame::ping(), Frame::pong()];
        let mut wire = Vec::new();
        for f in &frames {
            write_frame(&mut wire, f).await.unwrap();
        }
        assert_eq!(wire.len(), frames.iter().map(|f| HEADER_LEN + f.payload.len()).sum::<usize>());
        let mut cursor = std::io::Cursor::new(wire);
        for f in &frames {
            assert_eq!(&read_frame(&mut cursor).await.unwrap(), f);
        }
        assert_eq!(read_frame(&mut cursor).await.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);

        let too_big = Frame::data(1, vec![0; MAX_PAYLOAD + 1]);
        let mut sink = Vec::new();
        assert!(write_frame(&mut sink, &too_big).await.is_err());
        let mut bad_len = vec![2u8, 0, 0, 0, 1];
        bad_len.extend_from_slice(&((MAX_PAYLOAD as u32) + 1).to_be_bytes());
        assert_eq!(read_frame(&mut std::io::Cursor::new(bad_len)).await.unwrap_err().kind(), io::ErrorKind::InvalidData);
        let bad_type = [9u8, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(read_frame(&mut std::io::Cursor::new(bad_type.to_vec())).await.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn handshake_over_duplex() {
        let psk = b"secret".to_vec();
        let (mut a, mut b) = tokio::io::duplex(1024);
        let edge = tokio::spawn({
            let psk = psk.clone();
            async move { handshake_edge(&mut a, &psk).await }
        });
        handshake_client(&mut b, &psk).await.unwrap();
        edge.await.unwrap().unwrap();

        let (mut a, mut b) = tokio::io::duplex(1024);
        let edge = tokio::spawn(async move {
            let r = handshake_edge(&mut a, b"right").await;
            drop(a); // the edge closes on failure
            r
        });
        let err = handshake_client(&mut b, b"wrong").await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(edge.await.unwrap().unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn proof_is_hmac() {
        let p = handshake_proof(b"k", b"n");
        assert!(verify_proof(b"k", b"n", &p));
        assert!(!verify_proof(b"k", b"m", &p));
        assert!(!verify_proof(b"j", b"n", &p));
    }
}

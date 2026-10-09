//! TLS terminates on the desktop, never at the edge. This module holds the
//! daemon's self-signed certificate, the listener that decrypts and hands
//! bytes to the plain HTTP port, and the verifier phones use to pin that
//! certificate by SHA-256 fingerprint instead of trusting a name.

use std::io::BufReader;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::sync::CancellationToken;

pub const CERT_FILE: &str = "funnel-cert.pem";
pub const KEY_FILE: &str = "funnel-key.pem";

/// Lowercase hex SHA-256 of a DER certificate: the `c` of the pair link.
pub fn fingerprint_hex(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

/// Loads `funnel-cert.pem` / `funnel-key.pem` from `dir`, creating a
/// self-signed pair (SAN: `name` when it is a valid DNS name or IP, plus
/// `localhost`) when they are missing. Returns the server config and the
/// certificate's fingerprint to put in the pair QR.
pub fn load_or_create_self_signed(dir: &Path, name: &str) -> anyhow::Result<(Arc<ServerConfig>, String)> {
    let (cert, key) = match load_pem(dir)? {
        Some(pair) => pair,
        None => create_pem(dir, name)?,
    };
    let fingerprint = fingerprint_hex(&cert);
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .context("building TLS server config from the funnel certificate")?;
    Ok((Arc::new(config), fingerprint))
}

fn load_pem(dir: &Path) -> anyhow::Result<Option<(CertificateDer<'static>, PrivateKeyDer<'static>)>> {
    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);
    if !cert_path.exists() || !key_path.exists() {
        return Ok(None);
    }
    let cert_pem = std::fs::read(&cert_path).with_context(|| format!("reading {}", cert_path.display()))?;
    let key_pem = std::fs::read(&key_path).with_context(|| format!("reading {}", key_path.display()))?;
    let cert = rustls_pemfile::certs(&mut BufReader::new(cert_pem.as_slice()))
        .next()
        .ok_or_else(|| anyhow::anyhow!("{} holds no certificate", cert_path.display()))?
        .with_context(|| format!("parsing {}", cert_path.display()))?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key_pem.as_slice()))
        .with_context(|| format!("parsing {}", key_path.display()))?
        .ok_or_else(|| anyhow::anyhow!("{} holds no private key", key_path.display()))?;
    Ok(Some((cert, key)))
}

fn san_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && (name.parse::<std::net::IpAddr>().is_ok()
            || name.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            }))
}

fn create_pem(dir: &Path, name: &str) -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut sans = vec!["localhost".to_string()];
    if san_ok(name) && name != "localhost" {
        sans.insert(0, name.to_string());
    }
    let mut params = rcgen::CertificateParams::new(sans).context("certificate parameters")?;
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, if name.is_empty() { "CUEd desktop" } else { name });
    dn.push(rcgen::DnType::OrganizationName, "CUEd");
    params.distinguished_name = dn;
    let key_pair = rcgen::KeyPair::generate().context("generating the TLS key")?;
    let cert = params.self_signed(&key_pair).context("signing the TLS certificate")?;

    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);
    std::fs::write(&cert_path, cert.pem()).with_context(|| format!("writing {}", cert_path.display()))?;
    write_private(&key_path, key_pair.serialize_pem().as_bytes()).with_context(|| format!("writing {}", key_path.display()))?;
    tracing::info!(cert = %cert_path.display(), "created self-signed funnel certificate");

    let cert_der = cert.der().clone();
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));
    Ok((cert_der, key_der))
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(bytes)
}

/// A TLS-terminating listener forwarding decrypted bytes to a plain TCP port.
pub struct TlsForwarder {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    to_plain: SocketAddr,
}

/// Binds `listen`, terminates TLS with `config` and forwards each connection
/// to `to_plain` (the daemon's HTTP port) until `shutdown` is cancelled.
pub async fn serve_tls_forwarder(listen: SocketAddr, config: Arc<ServerConfig>, to_plain: SocketAddr, shutdown: CancellationToken) -> anyhow::Result<()> {
    TlsForwarder::bind(listen, config, to_plain).await?.run(shutdown).await
}

impl TlsForwarder {
    pub async fn bind(listen: SocketAddr, config: Arc<ServerConfig>, to_plain: SocketAddr) -> anyhow::Result<TlsForwarder> {
        let listener = TcpListener::bind(listen).await.with_context(|| format!("binding TLS listener {listen}"))?;
        Ok(TlsForwarder { listener, acceptor: TlsAcceptor::from(config), to_plain })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr().expect("bound listener has an address")
    }

    pub async fn run(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        tracing::info!(listen = %self.local_addr(), to = %self.to_plain, "TLS forwarder listening");
        loop {
            let (stream, peer) = tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                accepted = self.listener.accept() => match accepted {
                    Ok(pair) => pair,
                    Err(e) => { tracing::warn!("TLS accept failed: {e}"); continue; }
                },
            };
            let acceptor = self.acceptor.clone();
            let to_plain = self.to_plain;
            let cancel = shutdown.clone();
            tokio::spawn(async move {
                let _ = stream.set_nodelay(true);
                let mut tls = match acceptor.accept(stream).await {
                    Ok(tls) => tls,
                    Err(e) => {
                        tracing::debug!(%peer, "TLS handshake failed: {e}");
                        return;
                    }
                };
                let mut plain = match TcpStream::connect(to_plain).await {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("TLS forwarder cannot reach {to_plain}: {e}");
                        return;
                    }
                };
                let _ = plain.set_nodelay(true);
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    r = tokio::io::copy_bidirectional(&mut tls, &mut plain) => {
                        if let Err(e) = r {
                            tracing::debug!(%peer, "forwarded connection ended: {e}");
                        }
                    }
                }
            });
        }
    }
}

/// Accepts exactly one server certificate: the one whose SHA-256 fingerprint
/// was pinned. Names, chains and expiry are ignored, which is the point: the
/// phone learned the fingerprint from the pair QR and nothing else can vouch
/// for a self-signed certificate. Signatures over the handshake are still
/// verified with the certificate's key.
#[derive(Debug)]
pub struct PinnedFingerprintVerifier {
    fingerprint: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl PinnedFingerprintVerifier {
    /// `fingerprint_hex` as produced by [`fingerprint_hex`] (case and `:` separators tolerated).
    pub fn new(fingerprint_hex: &str) -> anyhow::Result<Self> {
        let clean: String = fingerprint_hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let bytes = hex::decode(clean.to_ascii_lowercase()).context("fingerprint is not hex")?;
        let fingerprint: [u8; 32] = bytes.try_into().map_err(|_| anyhow::anyhow!("fingerprint must be 32 bytes (SHA-256)"))?;
        Ok(Self { fingerprint, provider: Arc::new(rustls::crypto::ring::default_provider()) })
    }

    /// A client config that trusts only the pinned certificate.
    pub fn client_config(fingerprint_hex: &str) -> anyhow::Result<Arc<ClientConfig>> {
        let verifier = Arc::new(Self::new(fingerprint_hex)?);
        let config = ClientConfig::builder_with_provider(Arc::clone(&verifier.provider))
            .with_safe_default_protocol_versions()
            .context("TLS protocol versions")?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        Ok(Arc::new(config))
    }

    /// A connector that trusts only the pinned certificate.
    pub fn connector(fingerprint_hex: &str) -> anyhow::Result<TlsConnector> {
        Ok(TlsConnector::from(Self::client_config(fingerprint_hex)?))
    }
}

impl ServerCertVerifier for PinnedFingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let got: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        if got == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "server certificate fingerprint {} does not match the pinned {}",
                hex::encode(got),
                hex::encode(self.fingerprint)
            )))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn san_rules() {
        assert!(san_ok("idris-laptop"));
        assert!(san_ok("desk.local"));
        assert!(san_ok("192.168.1.10"));
        assert!(!san_ok("Idris's laptop"));
        assert!(!san_ok(""));
        assert!(!san_ok("-bad"));
    }

    #[test]
    fn creates_once_and_reuses() {
        let dir = std::env::temp_dir().join(format!("cued-funnel-tls-unit-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let (_cfg, fp) = load_or_create_self_signed(&dir, "Idris's laptop").unwrap();
        assert_eq!(fp.len(), 64);
        let cert_pem = std::fs::read_to_string(dir.join(CERT_FILE)).unwrap();
        assert!(cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join(KEY_FILE)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "private key must be owner-only");
        }
        let (_cfg2, fp2) = load_or_create_self_signed(&dir, "another-name").unwrap();
        assert_eq!(fp, fp2);
        // a corrupt certificate file is an error, never silently replaced
        std::fs::write(dir.join(CERT_FILE), "garbage").unwrap();
        assert!(load_or_create_self_signed(&dir, "x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verifier_rejects_bad_fingerprints() {
        assert!(PinnedFingerprintVerifier::new("abc").is_err());
        assert!(PinnedFingerprintVerifier::new("zz".repeat(32).as_str()).is_err());
        let fp = fingerprint_hex(b"cert");
        let v = PinnedFingerprintVerifier::new(&fp.to_uppercase()).unwrap();
        assert_eq!(hex::encode(v.fingerprint), fp);
        let with_colons: String = fp.as_bytes().chunks(2).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(":");
        assert!(PinnedFingerprintVerifier::new(&with_colons).is_ok());
    }
}

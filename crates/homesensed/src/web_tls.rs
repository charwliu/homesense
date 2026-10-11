//! Explicit LAN TLS settings. Credentials are loaded before binding a listener.
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    io::Read,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
};
#[derive(Clone)]
pub struct Settings {
    pub origin: String,
    pub(crate) acceptor: tokio_rustls::TlsAcceptor,
}
impl Settings {
    pub fn load(origin: String, certificate: &Path, key: &Path) -> Result<Self, String> {
        let cert = read(certificate, 256 * 1024, false)?;
        let key = read(key, 64 * 1024, true)?;
        let certificates = CertificateDer::pem_slice_iter(&cert)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "invalid TLS certificate PEM")?;
        if certificates.is_empty() || certificates.len() > 8 {
            return Err("TLS certificate chain must contain 1..8 certificates".into());
        }
        let key = PrivateKeyDer::from_pem_slice(&key).map_err(|_| "invalid TLS private key PEM")?;
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|_| "TLS certificate/key mismatch or invalid key")?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            origin,
            acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(config)),
        })
    }
    pub fn validate_address(&self, address: SocketAddr) -> Result<(), String> {
        validate_origin(&self.origin, address)
    }
}
pub fn validate_origin(origin: &str, address: SocketAddr) -> Result<(), String> {
    let local = match address.ip() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback(),
    };
    let uri: hyper::Uri = origin.parse().map_err(|_| "invalid HTTPS origin")?;
    let authority = uri.authority().ok_or("HTTPS authority required")?;
    let port = address.port().to_string();
    let canonical_port = if address.port() == 443 {
        authority.port().is_none()
    } else {
        authority.port().is_some_and(|p| p.as_str() == port)
    };
    if !local
        || authority.host().is_empty()
        || !authority
            .host()
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ".-:[]".contains(c))
        || !canonical_port
        || address.port() == 0
        || uri.scheme_str() != Some("https")
        || authority.as_str().contains('@')
        || authority.port_u16().unwrap_or(443) != address.port()
        || origin != format!("https://{authority}")
    {
        return Err(
            "explicit LAN/loopback address and exact HTTPS origin with matching port required"
                .into(),
        );
    }
    Ok(())
}
fn read(path: &Path, limit: u64, secret: bool) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err("invalid TLS file type or size".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if secret && metadata.permissions().mode() & 0o077 != 0 {
            return Err("TLS private key must not be accessible by group or others".into());
        }
    }
    #[cfg(not(unix))]
    {
        if secret {
            return Err("private-key ACL validation is not implemented on this platform".into());
        }
    }
    let mut bytes = vec![];
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("TLS file exceeds size limit".into());
    }
    Ok(bytes)
}

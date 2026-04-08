use std::io::{BufReader, Cursor};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::QuicClientConfig;
use quinn::{
    ClientConfig, ConnectError, Connection, ConnectionError, Endpoint, IdleTimeout, ReadToEndError,
    SendDatagramError,
};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use thiserror::Error;
use tokio::time::timeout;

const QUIC_SUBMIT_ALPN: &str = "solana-trader-submit-v1";
const QUIC_MAX_PAYLOAD_BYTES: usize = 1232;
const DEFAULT_ENDPOINT_PORT: u16 = 443;

#[derive(Debug)]
pub struct TraderApiQuicClientConfig {
    pub endpoint: String,
    pub connect_timeout: Duration,
    pub keep_alive_interval: Option<Duration>,
    pub idle_timeout: Option<Duration>,
    pub client_certificate_chain: Vec<CertificateDer<'static>>,
    pub client_private_key: PrivateKeyDer<'static>,
}

impl TraderApiQuicClientConfig {
    pub fn new(
        endpoint: impl Into<String>,
        client_certificate_chain: Vec<CertificateDer<'static>>,
        client_private_key: PrivateKeyDer<'static>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            connect_timeout: Duration::from_secs(10),
            keep_alive_interval: Some(Duration::from_secs(5)),
            idle_timeout: Some(Duration::from_secs(30)),
            client_certificate_chain,
            client_private_key,
        }
    }

    pub fn new_from_pem(
        endpoint: &str,
        client_certificate_chain_pem: impl AsRef<[u8]>,
        client_private_key_pem: impl AsRef<[u8]>,
    ) -> Result<Self, TraderApiQuicClientError> {
        Ok(Self::new(
            endpoint,
            certificates_from_pem(client_certificate_chain_pem)?,
            private_key_from_pem(client_private_key_pem)?,
        ))
    }

    pub fn new_from_pem_files(
        endpoint: &str,
        client_cert: impl Into<String>,
        client_key: impl Into<String>,
    ) -> Result<Self, TraderApiQuicClientError> {
        Self::new_from_pem(
            endpoint,
            &std::fs::read(client_cert.into())?,
            &std::fs::read(client_key.into())?,
        )
    }

    pub fn with_connect_timeout(mut self, connect_timeout: Duration) -> Self {
        self.connect_timeout = connect_timeout;
        self
    }

    pub fn with_keep_alive_interval(mut self, keep_alive_interval: Option<Duration>) -> Self {
        self.keep_alive_interval = keep_alive_interval;
        self
    }

    pub fn with_idle_timeout(mut self, idle_timeout: Option<Duration>) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }

    pub fn with_client_identity(
        mut self,
        client_certificate_chain: Vec<CertificateDer<'static>>,
        client_private_key: PrivateKeyDer<'static>,
    ) -> Self {
        self.client_certificate_chain = client_certificate_chain;
        self.client_private_key = client_private_key;
        self
    }
}

pub struct TraderApiQuicClient {
    connection: Connection,
}

impl TraderApiQuicClient {
    pub async fn connect(
        config: TraderApiQuicClientConfig,
    ) -> Result<Self, TraderApiQuicClientError> {
        if config.client_certificate_chain.is_empty() {
            return Err(TraderApiQuicClientError::MissingClientCertificate);
        }
        let (server_address, server_name) = resolve_endpoint(&config.endpoint)?;

        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let mut rustls_config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(config.client_certificate_chain, config.client_private_key)?;
        rustls_config.alpn_protocols = vec![QUIC_SUBMIT_ALPN.as_bytes().to_vec()];

        let mut transport_config = quinn::TransportConfig::default();
        transport_config.keep_alive_interval(config.keep_alive_interval);
        if let Some(idle_timeout) = config.idle_timeout {
            transport_config.max_idle_timeout(Some(IdleTimeout::try_from(idle_timeout)?));
        }

        let mut client_config =
            ClientConfig::new(Arc::new(QuicClientConfig::try_from(rustls_config)?));
        client_config.transport_config(Arc::new(transport_config));

        let mut endpoint = Endpoint::client("[::]:0".parse().expect("valid default bind address"))?;
        endpoint.set_default_client_config(client_config);

        let connecting = endpoint.connect(server_address, &server_name)?;
        let connection = timeout(config.connect_timeout, connecting)
            .await
            .map_err(|_| TraderApiQuicClientError::ConnectTimeout(config.connect_timeout))??;

        Ok(Self {
            connection: connection,
        })
    }

    pub async fn send_transaction_uni(
        &self,
        transaction: &[u8],
    ) -> Result<(), TraderApiQuicClientError> {
        self.validate_transaction(transaction)?;

        let mut stream = self.connection.open_uni().await?;
        stream.write_all(transaction).await?;
        stream.finish()?;

        Ok(())
    }

    pub async fn send_transaction_bi(
        &self,
        transaction: &[u8],
    ) -> Result<Vec<u8>, TraderApiQuicClientError> {
        self.validate_transaction(transaction)?;

        let (mut send, mut recv) = self.connection.open_bi().await?;
        send.write_all(transaction).await?;
        send.finish()?;

        Ok(recv.read_to_end(128).await?)
    }

    pub fn send_transaction_datagram(
        &self,
        transaction: &[u8],
    ) -> Result<(), TraderApiQuicClientError> {
        self.validate_transaction(transaction)?;
        self.connection.send_datagram(transaction.to_vec().into())?;
        Ok(())
    }

    pub fn close_reason(&self) -> Option<ConnectionError> {
        self.connection.close_reason()
    }

    pub async fn wait_for_close(&self) -> ConnectionError {
        self.connection.closed().await
    }

    pub fn close(&self, error_code: u32, reason: &[u8]) {
        self.connection.close(error_code.into(), reason);
    }

    fn validate_transaction(&self, transaction: &[u8]) -> Result<(), TraderApiQuicClientError> {
        if transaction.len() > QUIC_MAX_PAYLOAD_BYTES {
            return Err(TraderApiQuicClientError::TransactionTooLarge {
                actual: transaction.len(),
                maximum: QUIC_MAX_PAYLOAD_BYTES,
            });
        }

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum TraderApiQuicClientError {
    #[error("missing client certificate chain")]
    MissingClientCertificate,
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("transaction payload is too large: got {actual} bytes, max {maximum} bytes")]
    TransactionTooLarge { actual: usize, maximum: usize },
    #[error("timed out while connecting after {0:?}")]
    ConnectTimeout(Duration),
    #[error("failed to parse PEM data: {0}")]
    Pem(String),
    #[error(transparent)]
    Rustls(#[from] rustls::Error),
    #[error(transparent)]
    Endpoint(#[from] std::io::Error),
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Write(#[from] quinn::WriteError),
    #[error(transparent)]
    Finish(#[from] quinn::ClosedStream),
    #[error(transparent)]
    ReadToEnd(#[from] ReadToEndError),
    #[error(transparent)]
    Datagram(#[from] SendDatagramError),
    #[error("invalid idle timeout: {0}")]
    InvalidIdleTimeout(String),
    #[error("invalid QUIC crypto configuration: {0}")]
    InvalidCrypto(String),
}

impl From<quinn::VarIntBoundsExceeded> for TraderApiQuicClientError {
    fn from(error: quinn::VarIntBoundsExceeded) -> Self {
        Self::InvalidIdleTimeout(error.to_string())
    }
}

impl From<quinn::crypto::rustls::NoInitialCipherSuite> for TraderApiQuicClientError {
    fn from(error: quinn::crypto::rustls::NoInitialCipherSuite) -> Self {
        Self::InvalidCrypto(error.to_string())
    }
}

fn resolve_endpoint(endpoint: &str) -> Result<(SocketAddr, String), TraderApiQuicClientError> {
    if let Ok(addr) = endpoint.parse::<SocketAddr>() {
        return Ok((addr, addr.ip().to_string()));
    }

    if let Ok(ip) = endpoint.parse::<IpAddr>() {
        return Ok((SocketAddr::new(ip, DEFAULT_ENDPOINT_PORT), ip.to_string()));
    }

    if let Some(ip) = endpoint
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .and_then(|value| value.parse::<IpAddr>().ok())
    {
        return Ok((SocketAddr::new(ip, DEFAULT_ENDPOINT_PORT), ip.to_string()));
    }

    let (host, port) = if endpoint.contains(':') {
        let (host, port) = endpoint.rsplit_once(':').ok_or_else(|| {
            TraderApiQuicClientError::InvalidEndpoint(format!("{endpoint}: expected host:port"))
        })?;

        if host.is_empty() {
            return Err(TraderApiQuicClientError::InvalidEndpoint(format!(
                "{endpoint}: missing host"
            )));
        }

        let port = port.parse::<u16>().map_err(|error| {
            TraderApiQuicClientError::InvalidEndpoint(format!("{endpoint}: invalid port: {error}"))
        })?;

        (host, port)
    } else {
        (endpoint, DEFAULT_ENDPOINT_PORT)
    };

    let mut resolved = (host, port).to_socket_addrs().map_err(|error| {
        TraderApiQuicClientError::InvalidEndpoint(format!("{endpoint}: failed to resolve: {error}"))
    })?;
    let addr = resolved.next().ok_or_else(|| {
        TraderApiQuicClientError::InvalidEndpoint(format!("{endpoint}: no addresses found"))
    })?;

    Ok((addr, host.to_string()))
}

fn certificates_from_pem(
    pem_bytes: impl AsRef<[u8]>,
) -> Result<Vec<CertificateDer<'static>>, TraderApiQuicClientError> {
    let mut reader = BufReader::new(Cursor::new(pem_bytes.as_ref()));
    let certificates = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| TraderApiQuicClientError::Pem(error.to_string()))?;

    if certificates.is_empty() {
        return Err(TraderApiQuicClientError::Pem(
            "no certificates found in PEM input".to_string(),
        ));
    }

    Ok(certificates)
}

fn private_key_from_pem(
    pem_bytes: impl AsRef<[u8]>,
) -> Result<PrivateKeyDer<'static>, TraderApiQuicClientError> {
    let mut reader = BufReader::new(Cursor::new(pem_bytes.as_ref()));
    while let Some(item) = rustls_pemfile::read_one(&mut reader)
        .map_err(|error| TraderApiQuicClientError::Pem(error.to_string()))?
    {
        match item {
            rustls_pemfile::Item::Pkcs1Key(key) => return Ok(PrivateKeyDer::Pkcs1(key)),
            rustls_pemfile::Item::Pkcs8Key(key) => return Ok(PrivateKeyDer::Pkcs8(key)),
            rustls_pemfile::Item::Sec1Key(key) => return Ok(PrivateKeyDer::Sec1(key)),
            _ => continue,
        }
    }

    Err(TraderApiQuicClientError::Pem(
        "no private key found in PEM input".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_uses_solana_transaction_limit() {
        let config = TraderApiQuicClientConfig::new(
            "localhost:4433",
            vec![CertificateDer::from(vec![1, 2, 3])],
            PrivateKeyDer::Pkcs8(vec![4, 5, 6].into()),
        );

        assert_eq!(config.endpoint, "localhost:4433");
        assert_eq!(QUIC_MAX_PAYLOAD_BYTES, 1232);
        assert_eq!(QUIC_SUBMIT_ALPN, "solana-trader-submit-v1");
    }
}

//! A single owned TLS/HTTP connection for an Airdrop-compatible transfer.
//! No proxy, DNS rewrite, redirect, pool, POST replay, or TCP keepalive timer.
use anyhow::{ensure, Context, Result};
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full};
use hyper::{body::Bytes, client::conn::http1::SendRequest, Request};
use hyper_util::rt::TokioIo;
use openssl::pkey::PKey;
use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode, SslVersion};
use openssl::x509::X509;
use std::{io, net::SocketAddr, path::Path, pin::Pin, time::Duration};
use tokio::net::TcpStream;

pub type Body = UnsyncBoxBody<Bytes, io::Error>;
pub fn bytes(body: Vec<u8>) -> Body {
    Full::new(Bytes::from(body))
        .map_err(|never| match never {})
        .boxed_unsync()
}
pub fn connector(identity: &Path) -> Result<SslConnector> {
    let bundle = super::identity_bundle(identity)?;
    let key = PKey::private_key_from_pkcs8(bundle.key.secret_der())?;
    // identity_bundle() only keeps the leaf (re-encoded from the first PEM
    // block) to verify it matches the key; a chain file with an intermediate
    // after the leaf needs the raw file re-read so that cert reaches the peer.
    let mut chain = X509::stack_from_pem(
        &std::fs::read(&bundle.cert_path)
            .with_context(|| format!("reading {:?}", bundle.cert_path))?,
    )?;
    ensure!(
        !chain.is_empty(),
        "{:?} has no certificates",
        bundle.cert_path
    );
    let leaf = chain.remove(0);
    ensure!(
        leaf.to_der()?.as_slice() == bundle.cert.as_ref(),
        "{:?}'s first certificate no longer matches the verified identity",
        bundle.cert_path
    );
    let mut tls = SslConnector::builder(SslMethod::tls_client())?;
    tls.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    // AirDrop peers present Apple/self-signed certs over AWDL, so TLS does not authenticate the peer.
    tls.set_verify(SslVerifyMode::NONE);
    tls.set_certificate(&leaf)?;
    for intermediate in chain {
        tls.add_extra_chain_cert(intermediate)?;
    }
    tls.set_private_key(&key)?;
    tls.check_private_key()?;
    Ok(tls.build())
}
pub struct Session {
    sender: SendRequest<Body>,
    driver: tokio::task::JoinHandle<()>,
    authority: String,
    id: u64,
    reusable: bool,
}
impl Drop for Session {
    fn drop(&mut self) {
        // Cancellation/early error cannot leave a pooled socket or driver task.
        self.driver.abort();
    }
}
impl Session {
    pub async fn connect(address: SocketAddr, tls: &SslConnector) -> Result<Self> {
        let id = rand::random::<u64>();
        let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(address))
            .await
            .context("Airdrop-compatible TCP connect timed out")??;
        stream.set_nodelay(true)?;
        let local = stream.local_addr()?;
        let mut config = tls.configure()?;
        config.set_verify_hostname(false);
        config.set_use_server_name_indication(false);
        let ssl = config.into_ssl(&address.ip().to_string())?;
        let mut stream = tokio_openssl::SslStream::new(ssl, stream)?;
        tokio::time::timeout(Duration::from_secs(15), Pin::new(&mut stream).connect())
            .await
            .context("Airdrop-compatible TLS handshake timed out")??;
        tracing::info!(session=id, %local, peer=%address, tls=stream.ssl().version_str(), "Airdrop-compatible transfer connection established");
        let (sender, connection) = hyper::client::conn::http1::Builder::new()
            .title_case_headers(true)
            .max_buf_size(64 * 1024)
            .handshake(TokioIo::new(stream))
            .await?;
        let driver = tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::debug!(session=id, %error, "Airdrop-compatible HTTP connection ended");
            }
        });
        // IPv6 scope is a socket routing parameter, not part of the HTTP Host.
        let authority = match address {
            SocketAddr::V6(a) => format!("[{}]:{}", a.ip(), a.port()),
            SocketAddr::V4(a) => a.to_string(),
        };
        Ok(Self {
            sender,
            driver,
            authority,
            id,
            reusable: true,
        })
    }
    pub fn is_closed(&self) -> bool {
        !self.reusable || self.sender.is_closed()
    }
    pub async fn post(
        &mut self,
        endpoint: &str,
        ty: &str,
        body: Body,
        size: u64,
    ) -> Result<Vec<u8>> {
        self.post_with_headers(endpoint, ty, body, size, &[]).await
    }
    pub async fn post_with_headers(
        &mut self,
        endpoint: &str,
        ty: &str,
        body: Body,
        size: u64,
        headers: &[(&'static str, String)],
    ) -> Result<Vec<u8>> {
        let started = std::time::Instant::now();
        tracing::info!(
            session = self.id,
            endpoint,
            bytes = size,
            "Airdrop-compatible request started"
        );
        let limit = Duration::from_secs(if endpoint == "/Upload" { 3600 } else { 120 });
        let result = tokio::time::timeout(limit, self.exchange(endpoint, ty, body, headers))
            .await
            .context("HTTP response deadline expired")
            .and_then(|r| r)
            .with_context(|| format!("Airdrop-compatible {endpoint} ({size} bytes)"));
        if let Err(error) = &result {
            tracing::warn!(session=self.id,endpoint,elapsed_ms=started.elapsed().as_millis() as u64,error=%format!("{error:#}"),"Airdrop-compatible request failed");
        }
        result
    }
    async fn exchange(
        &mut self,
        endpoint: &str,
        ty: &str,
        body: Body,
        headers: &[(&'static str, String)],
    ) -> Result<Vec<u8>> {
        let mut request = Request::post(endpoint)
            .header("Host", &self.authority)
            .header("Content-Type", ty)
            .header("Connection", "keep-alive")
            .header("User-Agent", "Airdrop-compatible/1.0")
            .header("Accept", "*/*")
            .header("Accept-Language", "en-us")
            // Do not offer encodings we do not decode in control responses.
            .header("Accept-Encoding", "identity");
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let request = request.body(body)?;
        let response = self.sender.send_request(request).await?;
        let status = response.status();
        self.reusable = !response.headers().get_all("connection").iter().any(|v| {
            v.to_str()
                .is_ok_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("close")))
        }) && response.version() == hyper::Version::HTTP_11;
        if let Some(n) = response.headers().get("content-length") {
            ensure!(
                n.to_str()?.parse::<u64>()? <= 1024 * 1024,
                "Airdrop-compatible response exceeds limit"
            );
        }
        tracing::info!(session=self.id,endpoint,status=status.as_u16(),connection=?response.headers().get("connection"),"Airdrop-compatible response received");
        ensure!(
            status.as_u16() == 200,
            "Airdrop-compatible {endpoint}: {status} (declined or unavailable)"
        );
        let encoding = response.headers().get("content-encoding");
        ensure!(
            encoding.is_none_or(|v| v.as_bytes().eq_ignore_ascii_case(b"identity")),
            "unsupported Airdrop-compatible response encoding"
        );
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame?.into_data() {
                ensure!(
                    data.len() <= 1024 * 1024 - bytes.len(),
                    "Airdrop-compatible response exceeds limit"
                );
                bytes.extend_from_slice(&data);
            }
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    async fn reply_fixture(reply: &'static [u8]) -> Result<Vec<u8>> {
        let root = tempfile::tempdir()?;
        let acceptor = crate::airdrop::tls(&root.path().join("server"))?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        crate::airdrop::identity(&root.path().join("client"))?;
        let tls = connector(&root.path().join("client"))?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(acceptor.accept(stream).await.unwrap());
            crate::airdrop::http::read(&mut stream).await.unwrap();
            stream.get_mut().write_all(reply).await.unwrap();
            stream.get_mut().shutdown().await.unwrap();
        });
        let mut session = Session::connect(addr, &tls).await?;
        let result = session
            .post("/Upload", "application/x-cpio", bytes(vec![]), 0)
            .await;
        server.await?;
        result
    }
    #[tokio::test]
    async fn accepts_empty_upload_response_followed_by_tls_shutdown() {
        assert!(reply_fixture(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .await
        .unwrap()
        .is_empty());
    }
    #[tokio::test]
    async fn handles_interim_then_chunked_response() {
        let reply = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n";
        assert_eq!(reply_fixture(reply).await.unwrap(), b"abc");
    }
    #[tokio::test]
    async fn accepts_close_delimited_response() {
        assert_eq!(
            reply_fixture(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nabc")
                .await
                .unwrap(),
            b"abc"
        );
    }
    #[tokio::test]
    async fn refuses_truncated_success_body() {
        assert!(
            reply_fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc")
                .await
                .is_err()
        );
    }

    // A real Mac's AirDrop Discover handshake requests the client's certificate
    // and, like OpenDrop, can require the full chain rather than just the leaf
    // (`upload_wire_tests::sender_fixture` exercises that request-and-verify
    // path with rustls). This test only checks what reaches the wire: that a
    // chain file with an intermediate after the leaf still presents that
    // intermediate, independent of whether a peer chooses to verify it.
    #[tokio::test]
    async fn presents_intermediate_certificates_from_the_chain_file() {
        use openssl::pkey::PKey as OpenSslPKey;
        use openssl::ssl::{Ssl, SslContext, SslMethod, SslVerifyMode, SslVersion};
        use openssl::x509::X509;
        use std::pin::Pin;

        let root = tempfile::tempdir().unwrap();
        let client_dir = root.path().join("client");
        // Resolve the actual on-disk cert path (self-signed identities use
        // `certificate.self-signed.pem`, not `certificate.pem`) instead of
        // guessing the filename.
        let cert_path = crate::airdrop::identity_bundle(&client_dir)
            .unwrap()
            .cert_path;
        // Simulate a chain file with an intermediate after the leaf by
        // appending an unrelated self-signed cert; its chain validity does
        // not matter here, only whether it is transmitted.
        let intermediate_dir = root.path().join("intermediate");
        let intermediate_pem = std::fs::read(
            crate::airdrop::identity_bundle(&intermediate_dir)
                .unwrap()
                .cert_path,
        )
        .unwrap();
        let mut combined = std::fs::read(&cert_path).unwrap();
        combined.extend_from_slice(&intermediate_pem);
        std::fs::write(&cert_path, combined).unwrap();

        let tls = connector(&client_dir).unwrap();

        let (server_cert, server_key) =
            crate::airdrop::identity(&root.path().join("server")).unwrap();
        let server_cert = X509::from_der(server_cert.as_ref()).unwrap();
        let server_key = OpenSslPKey::private_key_from_pkcs8(server_key.secret_der()).unwrap();
        let mut sctx = SslContext::builder(SslMethod::tls_server()).unwrap();
        sctx.set_min_proto_version(Some(SslVersion::TLS1_2))
            .unwrap();
        sctx.set_certificate(&server_cert).unwrap();
        sctx.set_private_key(&server_key).unwrap();
        // Request the client certificate but accept it regardless of trust:
        // this test checks what the client sends, not chain validity.
        sctx.set_verify_callback(SslVerifyMode::PEER, |_, _| true);
        let sctx = sctx.build();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ssl = Ssl::new(&sctx).unwrap();
            let mut stream = tokio_openssl::SslStream::new(ssl, stream).unwrap();
            Pin::new(&mut stream).accept().await.unwrap();
            // peer_cert_chain(), seen from the server/acceptor side, excludes
            // the peer's leaf and holds only the certs sent after it.
            stream.ssl().peer_cert_chain().map(|c| c.len()).unwrap_or(0)
        });

        let _session = Session::connect(addr, &tls).await.unwrap();
        assert_eq!(server.await.unwrap(), 1);
    }
}

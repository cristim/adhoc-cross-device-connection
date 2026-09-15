//! Explicit, bounded AirDrop receive sessions. Independent of same-account Universal Clipboard.
mod archive;
mod http;
use crate::companion_client::{Pasteboard, PasteboardItem};
use anyhow::{ensure, Context, Result};
use std::{
    io::Cursor,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_rustls::{
    rustls::{
        self,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
    TlsAcceptor,
};

fn tls(dir: &Path) -> Result<TlsAcceptor> {
    use openssl::{
        asn1::Asn1Time,
        bn::{BigNum, MsbOption},
        hash::MessageDigest,
        pkey::PKey,
        rsa::Rsa,
        x509::{X509NameBuilder, X509},
    };
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let cert_path = dir.join("certificate.pem");
    let key_path = dir.join("key.pem");
    if !cert_path.exists() && !key_path.exists() {
        let key = PKey::from_rsa(Rsa::generate(2048)?)?;
        let mut name = X509NameBuilder::new()?;
        name.append_entry_by_text("CN", "ac-dc AirDrop")?;
        let name = name.build();
        let mut x = X509::builder()?;
        x.set_version(2)?;
        let mut bn = BigNum::new()?;
        bn.rand(128, MsbOption::MAYBE_ZERO, false)?;
        let serial = bn.to_asn1_integer()?;
        x.set_serial_number(&serial)?;
        x.set_subject_name(&name)?;
        x.set_issuer_name(&name)?;
        x.set_pubkey(&key)?;
        let start = Asn1Time::days_from_now(0)?;
        let end = Asn1Time::days_from_now(365)?;
        x.set_not_before(&start)?;
        x.set_not_after(&end)?;
        let san = openssl::x509::extension::SubjectAlternativeName::new()
            .dns("ac-dc.local")
            .build(&x.x509v3_context(None, None))?;
        x.append_extension(san)?;
        x.sign(&key, MessageDigest::sha256())?;
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&key_path)?;
        f.write_all(&key.private_key_to_pem_pkcs8()?)?;
        f.sync_all()?;
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&cert_path)?;
        f.write_all(&x.build().to_pem()?)?;
        f.sync_all()?;
    }
    let cert = X509::from_pem(&fs::read(cert_path)?)?;
    let key = PKey::private_key_from_pem(&fs::read(key_path)?)?;
    ensure!(
        cert.public_key()?.public_eq(&key),
        "AirDrop certificate/key do not match"
    );
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert.to_der()?)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.private_key_to_pkcs8()?)),
        )?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}
fn response(name: &str) -> Result<Vec<u8>> {
    let mut d = plist::Dictionary::new();
    d.insert("ReceiverComputerName".into(), name.into());
    d.insert("ReceiverModelName".into(), "MacBookPro18,3".into());
    d.insert(
        "ReceiverMediaCapabilities".into(),
        plist::Value::Data(br#"{"Version":1}"#.to_vec()),
    );
    let mut bytes = Vec::new();
    plist::Value::Dictionary(d).to_writer_binary(&mut bytes)?;
    Ok(bytes)
}
fn board(entries: &[archive::Entry]) -> Pasteboard {
    Pasteboard {
        items: entries
            .iter()
            .filter_map(|e| {
                let ext = Path::new(&e.name)
                    .extension()?
                    .to_str()?
                    .to_ascii_lowercase();
                let uti = match ext.as_str() {
                    "txt" if std::str::from_utf8(&e.bytes).is_ok() => "public.utf8-plain-text",
                    "png" => "public.png",
                    "jpg" | "jpeg" => "public.jpeg",
                    "gif" => "com.compuserve.gif",
                    "webp" => "public.webp",
                    _ => return None,
                };
                Some(PasteboardItem {
                    uti: uti.into(),
                    data: e.bytes.clone(),
                })
            })
            .collect(),
    }
}
async fn deliver(entries: Vec<archive::Entry>, dest: PathBuf, notify: bool) -> Result<()> {
    ensure!(
        !entries.is_empty(),
        "transfer contains no supported regular files"
    );
    let paths = archive::store(&dest, &entries)?;
    let pb = board(&entries);
    let copied = match crate::clipboard_out::copy_pasteboard(&pb) {
        Ok(out) => out.is_some_and(|o| o.spawned),
        Err(e) => {
            tracing::warn!(error=%e,"files saved but clipboard unavailable");
            false
        }
    };
    tracing::info!(files = paths.len(), copied, "AirDrop transfer received");
    for p in paths {
        println!("{}", serde_json::json!({"received":p,"copied":copied}));
    }
    if notify {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::process::Command::new("notify-send")
                .args([
                    "AirDrop received",
                    if copied {
                        "Saved and copied to clipboard"
                    } else {
                        "Saved to the receive folder"
                    },
                ])
                .kill_on_drop(true)
                .status(),
        )
        .await;
    }
    Ok(())
}
fn is_link_transfer(value: Option<&plist::Value>) -> bool {
    match value {
        Some(plist::Value::Dictionary(d)) => d.contains_key("links"),
        Some(plist::Value::Array(a)) => a.iter().any(|v| v.as_string() == Some("links")),
        _ => false,
    }
}
async fn connection(
    stream: tokio::net::TcpStream,
    acceptor: TlsAcceptor,
    name: String,
    dest: PathBuf,
    notify: bool,
    completed: tokio::sync::mpsc::Sender<()>,
) -> Result<()> {
    let tls = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream))
        .await
        .context("AirDrop TLS timed out")??;
    let mut io = tokio::io::BufReader::new(tls);
    let mut asked = false;
    for _ in 0..32 {
        let req = tokio::time::timeout(Duration::from_secs(30), http::read(&mut io))
            .await
            .context("AirDrop request timed out")??;
        let result = match req.path.as_str() {
            "/Discover" => {
                plist::Value::from_reader(Cursor::new(&req.body))
                    .context("invalid Discover plist")?;
                Ok((response(&name)?, false))
            }
            "/Ask" => {
                let value = plist::Value::from_reader(Cursor::new(&req.body))?;
                let dict = value.as_dictionary().context("Ask must be a dictionary")?;
                let links = is_link_transfer(dict.get("TransferType"));
                if links {
                    let mut entries = Vec::new();
                    for url in dict
                        .get("Items")
                        .and_then(plist::Value::as_array)
                        .context("link Items missing")?
                    {
                        let url = url.as_string().context("link is not text")?;
                        ensure!(
                            (url.starts_with("https://") || url.starts_with("http://"))
                                && !url.chars().any(char::is_control),
                            "unsupported link"
                        );
                        entries.push(archive::Entry {
                            name: "shared-link.txt".into(),
                            bytes: url.as_bytes().to_vec(),
                        });
                    }
                    deliver(entries, dest.clone(), notify).await?;
                }
                asked = true;
                Ok((response(&name)?, links))
            }
            "/Upload" if asked => {
                let ty = req
                    .headers
                    .get("content-type")
                    .map(|s| s.split(';').next().unwrap_or(""))
                    .unwrap_or("");
                let archive = match ty {
                    "application/x-dvzip" => archive::dvzip(&req.body)?,
                    "application/x-cpio" => req.body,
                    _ => {
                        http::respond(io.get_mut(), 415, b"").await?;
                        continue;
                    }
                };
                let entries = archive::cpio(&archive)?;
                deliver(entries, dest.clone(), notify).await?;
                asked = false;
                Ok((Vec::new(), true))
            }
            _ => Err(anyhow::anyhow!(
                "Upload requires Ask on the same TLS connection"
            )),
        };
        match result {
            Ok((body, done)) => {
                http::respond(io.get_mut(), 200, &body).await?;
                if done {
                    let _ = completed.try_send(());
                }
            }
            Err(e) => {
                http::respond(io.get_mut(), 400, b"").await?;
                return Err(e);
            }
        }
    }
    Ok(())
}
pub struct Config {
    pub iface: String,
    pub directory: PathBuf,
    pub identity: PathBuf,
    pub name: String,
    pub port: u16,
    pub seconds: u64,
    pub once: bool,
    pub notify: bool,
}
pub async fn run(c: Config) -> Result<()> {
    ensure!(
        (1..=3600).contains(&c.seconds),
        "receive duration must be 1..3600 seconds"
    );
    ensure!(c.port > 0, "port must be nonzero");
    ensure!(
        !c.name.is_empty() && c.name.len() <= 63 && !c.name.chars().any(char::is_control),
        "invalid receiver name"
    );
    let r = crate::health::radio().await?;
    ensure!(
        r["ready"] == true && r["interface"] == c.iface,
        "AWDL radio is not ready on {}",
        c.iface
    );
    let ip: IpAddr = r["link_local"]
        .as_str()
        .context("AWDL link-local missing")?
        .parse()?;
    let mut addr = crate::discover::scope_address(ip, &c.iface)?;
    addr.set_port(c.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    std::fs::create_dir_all(&c.directory)?;
    let dest = c.directory.canonicalize()?;
    let acceptor = tls(&c.identity)?;
    let daemon = mdns_sd::ServiceDaemon::new()?;
    daemon.disable_interface(mdns_sd::IfKind::All)?;
    daemon.enable_interface(c.iface.as_str())?;
    let id = std::fs::read_to_string(format!("/sys/class/net/{}/address", c.iface))?
        .trim()
        .replace(':', "");
    let hostname = format!("ac-dc-{id}.local.");
    let info = mdns_sd::ServiceInfo::new(
        "_airdrop._tcp.local.",
        &id,
        &hostname,
        &ip.to_string()[..],
        c.port,
        &[("flags", "137")][..],
    )?;
    struct Announce(mdns_sd::ServiceDaemon, String);
    impl Drop for Announce {
        fn drop(&mut self) {
            let _ = self.0.unregister(&self.1);
            let _ = self.0.shutdown();
        }
    }
    let fullname = info.get_fullname().to_string();
    daemon.register(info)?;
    let _announce = Announce(daemon, fullname);
    tracing::info!(address=%addr,seconds=c.seconds,"AirDrop receive window open (Everyone; experimental)");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let end = tokio::time::sleep(Duration::from_secs(c.seconds));
    tokio::pin!(end);
    let shutdown = crate::shutdown();
    tokio::pin!(shutdown);
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _=&mut end=>break,_=&mut shutdown=>break,
            Some(())=rx.recv()=>{if c.once{break;}},
            Some(result)=tasks.join_next(),if !tasks.is_empty()=>{if let Ok(Err(e))=result{tracing::debug!(error=%format!("{e:#}"),"AirDrop connection ended");}},
            result=listener.accept()=>{let (s,_)=result?;if tasks.len()>=4{drop(s);continue;}tasks.spawn(connection(s,acceptor.clone(),c.name.clone(),dest.clone(),c.notify,tx.clone()));}
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[tokio::test]
    async fn tls_ask_upload_stores_file_without_touching_clipboard() {
        let tmp = Temp(std::env::temp_dir().join(format!(
            "ac-dc-airdrop-test-{:032x}",
            rand::random::<u128>()
        )));
        std::fs::create_dir(&tmp.0).unwrap();
        let dest = tmp.0.join("received");
        std::fs::create_dir(&dest).unwrap();
        let id = tmp.0.join("identity");
        let acceptor = tls(&id).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let server_dest = dest.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            connection(stream, acceptor, "Test".into(), server_dest, false, tx).await
        });
        let cert =
            openssl::x509::X509::from_pem(&std::fs::read(id.join("certificate.pem")).unwrap())
                .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(cert.to_der().unwrap()))
            .unwrap();
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let client = tokio_rustls::TlsConnector::from(Arc::new(config));
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut stream = client
            .connect("ac-dc.local".try_into().unwrap(), stream)
            .await
            .unwrap();
        let mut ask = Vec::new();
        plist::Value::Dictionary(plist::Dictionary::new())
            .to_writer_binary(&mut ask)
            .unwrap();
        stream
            .write_all(
                format!(
                    "POST /Ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                    ask.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&ask).await.unwrap();
        // Keep the same TLS connection and reader for the next request.
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        let mut io = tokio::io::BufReader::new(stream);
        let mut line = String::new();
        io.read_line(&mut line).await.unwrap();
        assert!(line.contains("200"));
        let mut size = 0;
        loop {
            line.clear();
            io.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                size = v.trim().parse::<usize>().unwrap();
            }
        }
        let mut response = vec![0; size];
        io.read_exact(&mut response).await.unwrap();
        let mut archive = archive_fixture("../../blob.bin", b"test bytes");
        archive.extend(archive_fixture("TRAILER!!!", b""));
        let mut dvzip = ((archive.len() as u32) | 0x8000_0000)
            .to_be_bytes()
            .to_vec();
        dvzip.extend(archive);
        io.get_mut().write_all(format!("POST /Upload HTTP/1.1\r\nContent-Type: application/x-dvzip\r\nContent-Length: {}\r\n\r\n",dvzip.len()).as_bytes()).await.unwrap();
        io.get_mut().write_all(&dvzip).await.unwrap();
        line.clear();
        io.read_line(&mut line).await.unwrap();
        assert!(line.contains("200"), "{line}");
        rx.recv().await.unwrap();
        assert_eq!(std::fs::read(dest.join("blob.bin")).unwrap(), b"test bytes");
        drop(io);
        server.abort();
        let _ = server.await;
    }
    fn archive_fixture(name: &str, data: &[u8]) -> Vec<u8> {
        let header = format!(
            "070707{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:011o}{:06o}{:011o}",
            0,
            1,
            0o100600,
            0,
            0,
            1,
            0,
            0,
            name.len() + 1,
            data.len()
        );
        assert_eq!(header.len(), 76);
        let mut b = header.into_bytes();
        b.extend(name.as_bytes());
        b.push(0);
        b.extend(data);
        b
    }
    #[test]
    fn cpio_flattens_paths_and_preserves_payload() {
        let mut b = archive_fixture("../../file.txt", b"hello");
        b.extend(archive_fixture("TRAILER!!!", b""));
        let e = archive::cpio(&b).unwrap();
        assert_eq!(e[0].name, "file.txt");
        assert_eq!(e[0].bytes, b"hello");
    }
    #[test]
    fn filename_collisions_preserve_both_files() {
        let tmp =
            Temp(std::env::temp_dir().join(format!("ac-dc-files-{:032x}", rand::random::<u128>())));
        std::fs::create_dir(&tmp.0).unwrap();
        let entries = vec![
            archive::Entry {
                name: "one.txt".into(),
                bytes: b"first".to_vec(),
            },
            archive::Entry {
                name: "one.txt".into(),
                bytes: b"second".to_vec(),
            },
        ];
        let files = archive::store(&tmp.0, &entries).unwrap();
        assert_ne!(files[0], files[1]);
        assert_eq!(std::fs::read(&files[0]).unwrap(), b"first");
        assert_eq!(std::fs::read(&files[1]).unwrap(), b"second");
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;
    #[test]
    fn supports_both_transfer_type_containers() {
        let mut d = plist::Dictionary::new();
        d.insert("links".into(), true.into());
        assert!(is_link_transfer(Some(&plist::Value::Dictionary(d))));
        assert!(is_link_transfer(Some(&plist::Value::Array(vec![
            "links".into()
        ]))));
        assert!(!is_link_transfer(Some(&plist::Value::Array(vec![
            "files".into()
        ]))));
    }
}

//! Small HTTP/1.1 subset for AirDrop. Keep one reader across Ask and Upload.
use anyhow::{ensure, Context, Result};
use std::collections::BTreeMap;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
pub struct Request {
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
async fn line<S: AsyncRead + Unpin>(io: &mut BufReader<S>) -> Result<String> {
    let mut b = Vec::new();
    loop {
        let chunk = io.fill_buf().await?;
        ensure!(!chunk.is_empty(), "connection closed");
        let n = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(chunk.len());
        ensure!(b.len() + n <= 8192, "HTTP line exceeds limit");
        b.extend(&chunk[..n]);
        io.consume(n);
        if b.ends_with(b"\n") {
            break;
        }
    }
    ensure!(b.ends_with(b"\r\n"), "invalid HTTP line ending");
    b.truncate(b.len() - 2);
    Ok(String::from_utf8(b)?)
}
pub async fn read<S: AsyncRead + AsyncWrite + Unpin>(io: &mut BufReader<S>) -> Result<Request> {
    let first = line(io).await?;
    let parts = first.split_whitespace().collect::<Vec<_>>();
    ensure!(
        parts.len() == 3 && parts[0] == "POST" && parts[2] == "HTTP/1.1",
        "unsupported HTTP request"
    );
    let path = parts[1].to_string();
    ensure!(
        matches!(path.as_str(), "/Discover" | "/Ask" | "/Upload"),
        "unknown AirDrop endpoint"
    );
    let mut headers = BTreeMap::new();
    let mut total = 0;
    loop {
        let l = line(io).await?;
        total += l.len();
        ensure!(total <= 32768, "headers too large");
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':').context("malformed HTTP header")?;
        ensure!(
            headers
                .insert(k.to_ascii_lowercase(), v.trim().to_string())
                .is_none(),
            "duplicate HTTP header"
        );
    }
    let limit = if path == "/Upload" {
        32 * 1024 * 1024
    } else {
        1024 * 1024
    };
    let te = headers.get("transfer-encoding");
    let cl = headers.get("content-length");
    ensure!(
        !(te.is_some() && cl.is_some()),
        "ambiguous HTTP body framing"
    );
    if let Some(v) = te {
        ensure!(
            v.eq_ignore_ascii_case("chunked"),
            "unsupported transfer encoding"
        );
    }
    let length = cl.map(|s| s.parse::<usize>()).transpose()?.unwrap_or(0);
    ensure!(length <= limit, "request body exceeds limit");
    if headers
        .get("expect")
        .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
    {
        io.get_mut()
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .await?;
        io.get_mut().flush().await?;
    }
    let mut body = Vec::new();
    if te.is_some() {
        loop {
            let chunk = line(io).await?;
            let n =
                usize::from_str_radix(chunk.split(';').next().context("chunk size")?.trim(), 16)?;
            if n == 0 {
                let mut trailers = 0;
                loop {
                    let l = line(io).await?;
                    trailers += l.len();
                    ensure!(trailers <= 8192, "trailers too large");
                    if l.is_empty() {
                        break;
                    }
                }
                break;
            }
            ensure!(n <= limit - body.len(), "chunked body exceeds limit");
            let old = body.len();
            body.resize(old + n, 0);
            io.read_exact(&mut body[old..]).await?;
            let mut end = [0; 2];
            io.read_exact(&mut end).await?;
            ensure!(end == *b"\r\n", "invalid chunk terminator");
        }
    } else {
        body.resize(length, 0);
        io.read_exact(&mut body).await?;
    }
    Ok(Request {
        path,
        headers,
        body,
    })
}
pub async fn respond<S: AsyncWrite + Unpin>(io: &mut S, status: u16, body: &[u8]) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        415 => "Unsupported Media Type",
        _ => "Internal Server Error",
    };
    io.write_all(format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",body.len()).as_bytes()).await?;
    io.write_all(body).await?;
    io.flush().await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn chunked_then_keepalive() {
        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"POST /Ask HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\nPOST /Upload HTTP/1.1\r\nContent-Length: 1\r\n\r\nz").await.unwrap();
        let mut io = BufReader::new(server);
        assert_eq!(read(&mut io).await.unwrap().body, b"abc");
        assert_eq!(read(&mut io).await.unwrap().body, b"z");
    }
    #[tokio::test]
    async fn ambiguous_length() {
        let (mut c, s) = tokio::io::duplex(1024);
        c.write_all(
            b"POST /Ask HTTP/1.1\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n",
        )
        .await
        .unwrap();
        assert!(read(&mut BufReader::new(s)).await.is_err());
    }
}

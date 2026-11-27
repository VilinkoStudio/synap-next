//! 命令行访问正在跑的节点。这里不打开卷。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub fn get_json(base: &str, path: &str) -> Result<serde_json::Value, ClientError> {
    send_json(base, "GET", path, None)
}

pub fn send_json(
    base: &str,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, ClientError> {
    let (host, port) = parse_base(base)?;
    let addr = (host.as_str(), port)
        .to_socket_addrs_first()
        .map_err(|err| ClientError::Unreachable {
            url: base.to_owned(),
            detail: err.to_string(),
        })?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|err| {
        ClientError::Unreachable {
            url: base.to_owned(),
            detail: err.to_string(),
        }
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|err| ClientError::Io(err.to_string()))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|err| ClientError::Io(err.to_string()))?;
    let payload = body
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|err| ClientError::BadResponse(err.to_string()))?
        .unwrap_or_default();
    let headers = if body.is_some() {
        format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        )
    } else {
        String::new()
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: application/json\r\n{headers}Connection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.write_all(&payload))
        .map_err(|err| ClientError::Io(err.to_string()))?;
    let mut bytes = Vec::new();
    stream
        .read_to_end(&mut bytes)
        .map_err(|err| ClientError::Io(err.to_string()))?;
    let text =
        String::from_utf8(bytes).map_err(|_| ClientError::BadResponse("not utf-8".into()))?;
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| ClientError::BadResponse("missing header break".into()))?;
    let status = head
        .lines()
        .next()
        .ok_or_else(|| ClientError::BadResponse("missing status line".into()))?;
    let code = status
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| ClientError::BadResponse(status.to_owned()))?;
    if !code.starts_with('2') {
        return Err(ClientError::Status {
            code: code.to_owned(),
            body: body.to_owned(),
        });
    }
    let payload = decode_body(head, body)?;
    serde_json::from_str(&payload).map_err(|err| ClientError::BadResponse(err.to_string()))
}

fn decode_body(head: &str, body: &str) -> Result<String, ClientError> {
    let chunked = head.lines().any(|line| {
        let (name, value) = line.split_once(':').unwrap_or(("", ""));
        name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
    });
    if !chunked {
        return Ok(body.to_owned());
    }
    let mut rest = body;
    let mut out = String::new();
    loop {
        let (size_line, after) = rest
            .split_once("\r\n")
            .ok_or_else(|| ClientError::BadResponse("short chunk".into()))?;
        let size = usize::from_str_radix(size_line.trim(), 16)
            .map_err(|_| ClientError::BadResponse(format!("bad chunk size {size_line}")))?;
        if size == 0 {
            return Ok(out);
        }
        if after.len() < size + 2 {
            return Err(ClientError::BadResponse("short chunk body".into()));
        }
        out.push_str(&after[..size]);
        rest = &after[size + 2..];
    }
}

fn parse_base(base: &str) -> Result<(String, u16), ClientError> {
    let Some(rest) = base.strip_prefix("http://") else {
        return Err(ClientError::Url(base.to_owned()));
    };
    let rest = rest.trim_end_matches('/');
    let Some((host, port)) = rest.rsplit_once(':') else {
        return Err(ClientError::Url(base.to_owned()));
    };
    if host.is_empty() {
        return Err(ClientError::Url(base.to_owned()));
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| ClientError::Url(base.to_owned()))?;
    Ok((host.to_owned(), port))
}

trait FirstAddr {
    fn to_socket_addrs_first(&self) -> std::io::Result<std::net::SocketAddr>;
}

impl FirstAddr for (&str, u16) {
    fn to_socket_addrs_first(&self) -> std::io::Result<std::net::SocketAddr> {
        use std::net::ToSocketAddrs;
        self.to_socket_addrs()?
            .next()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no address"))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("url must look like http://127.0.0.1:8790, got {0}")]
    Url(String),
    #[error(
        "cannot reach {url} ({detail}). Start the node with `synap-node serve --data-dir <dir>`"
    )]
    Unreachable { url: String, detail: String },
    #[error("io: {0}")]
    Io(String),
    #[error("node returned {code}: {body}")]
    Status { code: String, body: String },
    #[error("bad response: {0}")]
    BadResponse(String),
}

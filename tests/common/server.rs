//! A scripted HTTP server on loopback, playing the model server for the transport cases.

use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Default)]
pub struct Log {
    pub requests: Vec<Recorded>,
    pub closed: Vec<Instant>,
}

pub struct Server {
    pub base: String,
    pub log: Arc<Mutex<Log>>,
}

struct Reply {
    status: Option<u16>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    stall: bool,
}

fn replies(dir: &Path, script: &[Value], base: &str) -> Vec<Reply> {
    script
        .iter()
        .map(|r| {
            if r.get("stall").and_then(Value::as_str) == Some("headers") {
                return Reply {
                    status: None,
                    headers: vec![],
                    body: vec![],
                    stall: true,
                };
            }

            let mut body = match (r.get("body"), r.get("body_file")) {
                (Some(b), _) => b.as_str().unwrap().to_owned(),
                (None, Some(f)) => std::fs::read_to_string(dir.join(f.as_str().unwrap())).unwrap(),
                _ => String::new(),
            };

            if let Some(cut) = r.get("cut_before").and_then(Value::as_str) {
                let at = body.find(cut).expect("cut_before not found");
                body.truncate(at);
            }

            let headers = r["headers"]
                .as_object()
                .map(|h| {
                    h.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap().replace("{server}", base)))
                        .collect()
                })
                .unwrap_or_default();

            Reply {
                status: Some(r["status"].as_u64().unwrap() as u16),
                headers,
                body: body.into_bytes(),
                stall: r.get("then").and_then(Value::as_str) == Some("stall"),
            }
        })
        .collect()
}

impl Server {
    pub async fn start(dir: &Path, script: &[Value]) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let replies = Arc::new(Mutex::new(replies(dir, script, &base).into_iter()));
        let log = Arc::new(Mutex::new(Log::default()));
        let task_log = log.clone();

        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let replies = replies.clone();
                let log = task_log.clone();
                tokio::spawn(async move { serve(socket, replies, log).await });
            }
        });

        Server { base, log }
    }

    /// Waits up to `limit` for the server to see a stalled connection closed by the client.
    pub async fn closed_within(&self, limit: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if !self.log.lock().unwrap().closed.is_empty() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().requests.clone()
    }
}

async fn read_request(socket: &mut TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let n = socket.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();

    let mut body = buf[head_end..].to_vec();
    let length = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .map(|(_, v)| v.parse::<usize>().unwrap());

    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.contains("chunked"));

    if let Some(length) = length {
        while body.len() < length {
            let n = socket.read(&mut tmp).await.ok()?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }
    } else if chunked {
        while !body.windows(5).any(|w| w == b"0\r\n\r\n") {
            let n = socket.read(&mut tmp).await.ok()?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }
        body = dechunk(&body);
    }

    Some(Recorded {
        method,
        path,
        headers,
        body,
    })
}

fn dechunk(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = raw;
    loop {
        let Some(at) = rest.windows(2).position(|w| w == b"\r\n") else {
            return out;
        };

        let size = usize::from_str_radix(std::str::from_utf8(&rest[..at]).unwrap().trim(), 16)
            .unwrap_or(0);

        if size == 0 {
            return out;
        }

        out.extend_from_slice(&rest[at + 2..at + 2 + size]);
        rest = &rest[at + 2 + size + 2..];
    }
}

async fn serve(
    mut socket: TcpStream,
    replies: Arc<Mutex<std::vec::IntoIter<Reply>>>,
    log: Arc<Mutex<Log>>,
) {
    let Some(request) = read_request(&mut socket).await else {
        return;
    };

    log.lock().unwrap().requests.push(request);

    let reply = replies.lock().unwrap().next();
    let Some(reply) = reply else {
        let _ = socket
            .write_all(b"HTTP/1.1 500 Unexpected\r\nconnection: close\r\ncontent-length: 0\r\n\r\n")
            .await;
        return;
    };

    if let Some(status) = reply.status {
        let mut head = format!("HTTP/1.1 {status} Scripted\r\n");
        for (k, v) in &reply.headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("connection: close\r\n\r\n");
        if socket.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        let _ = socket.write_all(&reply.body).await;
        let _ = socket.flush().await;
    }
    if reply.stall {
        let mut tmp = [0u8; 256];
        loop {
            match tokio::time::timeout(Duration::from_secs(15), socket.read(&mut tmp)).await {
                Ok(Ok(0)) | Ok(Err(_)) => {
                    log.lock().unwrap().closed.push(Instant::now());
                    return;
                }
                Ok(Ok(_)) => continue,
                Err(_) => return,
            }
        }
    }
    let _ = socket.shutdown().await;
}

/// Checks the recorded requests against `expect_requests`.
pub fn check_requests(expected: &Value, got: &[Recorded]) -> Vec<String> {
    let mut diffs = Vec::new();
    let Some(list) = expected.as_array() else {
        return diffs;
    };
    if list.len() != got.len() {
        diffs.push(format!("{} requests, expected {}", got.len(), list.len()));
    }
    for (i, (want, got)) in list.iter().zip(got).enumerate() {
        let header = |name: &str| {
            got.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        if let Some(m) = want.get("method") {
            if m.as_str() != Some(&got.method) {
                diffs.push(format!("#{i} method {} != {m}", got.method));
            }
        }
        if let Some(p) = want.get("path") {
            if p.as_str() != Some(&got.path) {
                diffs.push(format!("#{i} path {} != {p}", got.path));
            }
        }
        for (k, v) in want
            .get("headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if header(k).as_deref() != v.as_str() {
                diffs.push(format!("#{i} header {k}: {:?} != {v}", header(k)));
            }
        }
        for k in want
            .get("absent_headers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(v) = header(k.as_str().unwrap()) {
                diffs.push(format!("#{i} header {k} present: {v:?}"));
            }
        }
        if got.method == "POST" {
            // Invariants of every POST, whatever the case lists.
            if header("content-type").as_deref() != Some("application/json") {
                diffs.push(format!("#{i} content-type {:?}", header("content-type")));
            }
            match header("content-length").map(|len| len.parse::<usize>()) {
                Some(Ok(len)) if len == got.body.len() => {}
                other => diffs.push(format!(
                    "#{i} content-length {other:?}, body {}",
                    got.body.len()
                )),
            }
            if let Some(encoding) = header("transfer-encoding") {
                diffs.push(format!("#{i} transfer-encoding {encoding}"));
            }
            let body: Value = serde_json::from_slice(&got.body).unwrap_or(json!(null));
            for (k, v) in want
                .get("body_includes")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                if &body[k] != v {
                    diffs.push(format!("#{i} body.{k} {} != {v}", body[k]));
                }
            }
            for k in want
                .get("body_excludes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if body.get(k.as_str().unwrap()).is_some() {
                    diffs.push(format!("#{i} body has {k}"));
                }
            }
        }
    }
    diffs
}

use c6_bank::Client;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Semaphore,
    task::JoinHandle,
};

pub const RESOURCE_LIMIT: usize = 10 * 1024 * 1024;
pub const AUTH_LIMIT: usize = 64 * 1024;
pub const TOKEN: &str = r#"{"access_token":"synthetic-secret-token","expires_in":3600}"#;
pub const CHARGE: &str = r#"{"txid":"charge","status":"ATIVA"}"#;

pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub declared: Option<usize>,
    pub chunked: bool,
    pub before_headers: Duration,
    pub chunk_delay: Duration,
    pub gate: Option<Arc<Semaphore>>,
    pub retry_after: Option<&'static str>,
}
impl Reply {
    pub fn json(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            declared: None,
            chunked: false,
            before_headers: Duration::ZERO,
            chunk_delay: Duration::ZERO,
            gate: None,
            retry_after: None,
        }
    }
}
pub struct Server {
    pub url: String,
    pub requests: Arc<Mutex<Vec<String>>>,
    pub observed: Arc<Semaphore>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    pub async fn new(handler: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(vec![]));
        let observed = Arc::new(Semaphore::new(0));
        let (records, events, handler) = (requests.clone(), observed.clone(), Arc::new(handler));
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let (records, events, handler) = (records.clone(), events.clone(), handler.clone());
                tasks.spawn(async move {
                    let mut bytes = vec![];
                    let header_end = loop {
                        let mut byte = [0];
                        if socket.read_exact(&mut byte).await.is_err() { return; }
                        bytes.push(byte[0]);
                        if bytes.ends_with(b"\r\n\r\n") { break bytes.len(); }
                    };
                    let headers = String::from_utf8(bytes.clone()).unwrap();
                    let length = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(|value| value.parse::<usize>().unwrap())).unwrap_or(0);
                    bytes.resize(header_end + length, 0);
                    if socket.read_exact(&mut bytes[header_end..]).await.is_err() { return; }
                    let request = String::from_utf8(bytes).unwrap();
                    let reply = handler(&request);
                    records.lock().unwrap().push(request);
                    events.add_permits(1);
                    if let Some(gate) = reply.gate { gate.acquire().await.unwrap().forget(); }
                    if !reply.before_headers.is_zero() { tokio::time::sleep(reply.before_headers).await; }
                    let framing = if reply.chunked { "Transfer-Encoding: chunked\r\n".to_owned() } else { format!("Content-Length: {}\r\n", reply.declared.unwrap_or(reply.body.len())) };
                    let retry = reply.retry_after.map(|v| format!("Retry-After: {v}\r\n")).unwrap_or_default();
                    let headers = format!("HTTP/1.1 {} fixture\r\nConnection: close\r\nContent-Type: application/json\r\n{framing}{retry}\r\n", reply.status);
                    if socket.write_all(headers.as_bytes()).await.is_err() { return; }
                    // Empty oversized declared bodies deliberately remain unfinished.
                    if reply.declared.is_some_and(|n| n > reply.body.len()) {
                        std::future::pending::<()>().await;
                    }
                    for chunk in reply.body.chunks(8192) {
                        if !reply.chunk_delay.is_zero() { tokio::time::sleep(reply.chunk_delay).await; }
                        if reply.chunked && socket.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await.is_err() { return; }
                        if socket.write_all(chunk).await.is_err() { return; }
                        if reply.chunked && socket.write_all(b"\r\n").await.is_err() { return; }
                    }
                    if reply.chunked { let _ = socket.write_all(b"0\r\n\r\n").await; }
                });
            }
        });
        Self {
            url,
            requests,
            observed,
            task,
        }
    }
    pub fn builder(&self) -> c6_bank::ClientBuilder {
        Client::builder("synthetic-client", "synthetic-secret")
            .http_client(
                reqwest::Client::builder()
                    .retry(reqwest::retry::never())
                    .build()
                    .unwrap(),
            )
            .base_url(&self.url)
            .unwrap()
    }
    pub async fn observed(&self) {
        self.observed.acquire().await.unwrap().forget();
    }
}
pub fn padded_json(base: &str, length: usize) -> Vec<u8> {
    let mut body = base.as_bytes().to_vec();
    body.resize(length, b' ');
    body
}

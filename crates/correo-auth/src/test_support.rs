//! Servidor HTTP falso (tokio) para probar intercambio y refresco sin red.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub(crate) struct FakeServer {
    pub url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeServer {
    /// Responde a cada petición con la siguiente entrada de `responses` (status, cuerpo JSON).
    /// Si se agotan, responde 500 para que el test falle de forma visible.
    pub(crate) async fn start(responses: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
        let log = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let request = read_request(&mut stream).await;
                log.lock().unwrap().push(request);
                let (status, body) = queue
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or((500, "{\"error\":\"sin respuesta preparada\"}".into()));
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        Self { url, requests }
    }

    /// Peticiones recibidas: línea de inicio, cabeceras y cuerpo.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            return String::from_utf8_lossy(&buf).into_owned();
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let content_length: usize = head
        .lines()
        .find_map(|l| {
            l.strip_prefix("content-length: ")
                .or_else(|| l.strip_prefix("Content-Length: "))
        })
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + content_length {
        let n = stream.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Cuerpo JSON de una respuesta de token.
pub(crate) fn token_json(access: &str, refresh: Option<&str>, expires_in: u64) -> String {
    match refresh {
        Some(r) => format!(
            "{{\"access_token\":\"{access}\",\"token_type\":\"Bearer\",\"expires_in\":{expires_in},\"refresh_token\":\"{r}\"}}"
        ),
        None => format!(
            "{{\"access_token\":\"{access}\",\"token_type\":\"Bearer\",\"expires_in\":{expires_in}}}"
        ),
    }
}

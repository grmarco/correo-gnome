//! Servidor HTTP mínimo en 127.0.0.1 que recibe la redirección OAuth con el código.

use std::sync::Arc;
use std::time::Duration;

use correo_core::{Error, Result};
use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

const MAX_REQUEST_BYTES: usize = 16 * 1024;
/// Una conexión que no completa su petición en este tiempo se descarta.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
/// Conexiones atendidas a la vez; las que llegan con el cupo lleno se cierran al momento.
const MAX_CONCURRENT_CONNECTIONS: usize = 8;

const SUCCESS_PAGE: &str = "<!doctype html><html lang=\"es\"><head><meta charset=\"utf-8\">\
<title>Autorización completada</title></head><body style=\"font-family:sans-serif;\
margin:3rem auto;max-width:32rem;text-align:center\"><h1>Autorización completada</h1>\
<p>Ya puedes cerrar esta ventana y volver a la aplicación de correo.</p></body></html>";

const FAILURE_PAGE: &str = "<!doctype html><html lang=\"es\"><head><meta charset=\"utf-8\">\
<title>Autorización fallida</title></head><body style=\"font-family:sans-serif;\
margin:3rem auto;max-width:32rem;text-align:center\"><h1>Autorización fallida</h1>\
<p>No se pudo completar la autorización. Vuelve a intentarlo desde la aplicación.</p>\
</body></html>";

/// Enlaza a un puerto aleatorio de loopback.
pub(crate) async fn bind() -> Result<TcpListener> {
    TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| Error::Network(format!("no se pudo abrir el servidor loopback: {e}")))
}

/// Acepta conexiones hasta recibir un callback válido y devuelve el código.
///
/// Cada conexión se atiende en su propia tarea, con su propio timeout y un máximo de
/// `MAX_CONCURRENT_CONNECTIONS` a la vez: una conexión especulativa que no envía nada
/// no impide procesar la redirección real.
///
/// Las peticiones con `state` incorrecto se rechazan (400) y el servidor sigue esperando;
/// un `error` válido del proveedor (p. ej. el usuario canceló) termina con `Error::Auth`.
pub(crate) async fn serve(listener: TcpListener, expected_state: &str) -> Result<String> {
    let expected: Arc<str> = Arc::from(expected_state);
    let slots = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    let (outcomes_tx, mut outcomes) = mpsc::channel::<Outcome>(MAX_CONCURRENT_CONNECTIONS);
    // Al salir de la función, `tasks` aborta las conexiones que sigan abiertas.
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(conn) => conn,
                    Err(e) => return Err(Error::Network(format!("error en loopback: {e}"))),
                };
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let outcomes_tx = outcomes_tx.clone();
                let expected = Arc::clone(&expected);
                tasks.spawn(async move {
                    let _permit = permit;
                    let outcome = attend(stream, &expected).await;
                    let _ = outcomes_tx.send(outcome).await;
                });
            }
            Some(outcome) = outcomes.recv() => match outcome {
                Outcome::Code(code) => return Ok(code),
                Outcome::Denied(description) => {
                    return Err(Error::Auth(format!(
                        "autorización rechazada: {description}"
                    )));
                }
                Outcome::Ignored => {}
            },
        }
    }
}

/// Atiende una conexión con su timeout y la cierra al terminar.
async fn attend(mut stream: TcpStream, expected_state: &str) -> Outcome {
    let outcome = match tokio::time::timeout(
        CONNECTION_TIMEOUT,
        handle_connection(&mut stream, expected_state),
    )
    .await
    {
        Ok(Ok(outcome)) => outcome,
        _ => Outcome::Ignored,
    };
    let _ = stream.shutdown().await;
    outcome
}

enum Outcome {
    Code(String),
    Denied(String),
    Ignored,
}

async fn handle_connection(
    stream: &mut TcpStream,
    expected_state: &str,
) -> std::io::Result<Outcome> {
    let request = read_head(stream).await?;
    let Some(target) = parse_get_target(&request) else {
        write_response(stream, "405 Method Not Allowed", FAILURE_PAGE).await?;
        return Ok(Outcome::Ignored);
    };

    let url = Url::parse("http://127.0.0.1/")
        .and_then(|base| base.join(target))
        .ok();
    let params: Vec<(String, String)> = url
        .as_ref()
        .map(|u| {
            u.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let get = |name: &str| {
        params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };

    let (code, error) = (get("code"), get("error"));
    if code.is_none() && error.is_none() {
        write_response(stream, "404 Not Found", FAILURE_PAGE).await?;
        return Ok(Outcome::Ignored);
    }
    if get("state").as_deref() != Some(expected_state) {
        write_response(stream, "400 Bad Request", FAILURE_PAGE).await?;
        return Ok(Outcome::Ignored);
    }
    if let Some(error) = error {
        let description = get("error_description").unwrap_or(error);
        write_response(stream, "200 OK", FAILURE_PAGE).await?;
        return Ok(Outcome::Denied(description));
    }
    write_response(stream, "200 OK", SUCCESS_PAGE).await?;
    Ok(Outcome::Code(code.unwrap_or_default()))
}

/// Lee hasta la línea vacía que termina las cabeceras (la petición no tiene cuerpo útil).
async fn read_head(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < MAX_REQUEST_BYTES {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn parse_get_target(request: &str) -> Option<&str> {
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();
    match (parts.next()?, parts.next()?) {
        ("GET", target) if target.starts_with('/') => Some(target),
        _ => None,
    }
}

async fn write_response(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = "estado-esperado";

    async fn listener_and_server() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<String>>,
    ) {
        let listener = bind().await.unwrap();
        let addr = listener.local_addr().unwrap();
        (
            addr,
            tokio::spawn(async move { serve(listener, STATE).await }),
        )
    }

    fn callback(code: &str) -> String {
        format!("GET /?code={code}&state={STATE} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
    }

    async fn send(addr: std::net::SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        String::from_utf8_lossy(&response).into_owned()
    }

    #[tokio::test]
    async fn silent_connection_does_not_block_a_later_valid_redirect() {
        let (addr, server) = listener_and_server().await;
        // Conexión especulativa: se abre y no se envía nada.
        let _silent = TcpStream::connect(addr).await.unwrap();

        let response = send(addr, &callback("abc")).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let code = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("la redirección real no se procesó a tiempo")
            .unwrap()
            .unwrap();
        assert_eq!(code, "abc");
    }

    #[tokio::test]
    async fn connections_over_the_limit_are_closed_immediately() {
        let (addr, server) = listener_and_server().await;
        let mut silent = Vec::new();
        for _ in 0..MAX_CONCURRENT_CONNECTIONS {
            silent.push(TcpStream::connect(addr).await.unwrap());
        }
        // El cupo está lleno: la conexión extra se cierra sin esperar al timeout.
        let mut extra = TcpStream::connect(addr).await.unwrap();
        let mut buf = [0u8; 16];
        let read = tokio::time::timeout(Duration::from_secs(2), extra.read(&mut buf))
            .await
            .expect("la conexión extra debía cerrarse de inmediato")
            .unwrap();
        assert_eq!(read, 0);
        server.abort();
    }

    #[tokio::test]
    async fn wrong_state_is_rejected_and_server_keeps_waiting() {
        let (addr, server) = listener_and_server().await;
        let bad = "GET /?code=malo&state=otro HTTP/1.1\r\n\r\n";
        assert!(send(addr, bad).await.starts_with("HTTP/1.1 400"));
        let response = send(addr, &callback("bueno")).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(server.await.unwrap().unwrap(), "bueno");
    }
}

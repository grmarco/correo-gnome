//! Servidor HTTP mínimo en 127.0.0.1 que recibe la redirección OAuth con el código.

use std::time::Duration;

use correo_core::{Error, Result};
use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

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
/// Las peticiones con `state` incorrecto se rechazan (400) y el servidor sigue esperando;
/// un `error` válido del proveedor (p. ej. el usuario canceló) termina con `Error::Auth`.
pub(crate) async fn serve(listener: TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => return Err(Error::Network(format!("error en loopback: {e}"))),
        };
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
        match outcome {
            Outcome::Code(code) => return Ok(code),
            Outcome::Denied(description) => {
                return Err(Error::Auth(format!(
                    "autorización rechazada: {description}"
                )));
            }
            Outcome::Ignored => {}
        }
    }
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

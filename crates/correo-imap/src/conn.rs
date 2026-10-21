//! Conexión IMAP sobre TLS (rustls + webpki-roots) y autenticación XOAUTH2.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_imap::{Authenticator, Client, Session};
use correo_core::{Error, Result};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use socket2::{SockRef, TcpKeepalive};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::parse::xoauth2_initial_response;

pub type ImapSession = Session<TlsStream<TcpStream>>;

/// Límite para TCP + TLS + saludo.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Límite para el login XOAUTH2.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);
/// Límite para cada operación IMAP (en los FETCH de sincronización, por lote).
pub const OP_TIMEOUT: Duration = Duration::from_secs(60);

/// Keepalive TCP: primera sonda tras 60 s de inactividad, luego cada 20 s.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(60);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

/// Capacidades del servidor relevantes para este backend.
#[derive(Debug, Clone, Copy, Default)]
pub struct Caps {
    pub mov: bool,
    pub uidplus: bool,
    pub gmail_labels: bool,
    pub idle: bool,
}

/// Sesión IMAP autenticada con sus capacidades.
pub struct Conn {
    pub session: ImapSession,
    pub caps: Caps,
}

/// Error de red por tiempo agotado. El llamador debe descartar la sesión.
pub fn timeout_error(limit: Duration) -> Error {
    Error::Network(format!("tiempo de espera agotado ({} s)", limit.as_secs()))
}

/// Acota una operación IMAP con `limit`. Al expirar devuelve `Error::Network`.
pub async fn imap<T>(
    limit: Duration,
    fut: impl Future<Output = async_imap::error::Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(limit, fut).await {
        Ok(r) => r.map_err(map_imap_error),
        Err(_) => Err(timeout_error(limit)),
    }
}

/// Activa TCP keepalive para detectar conexiones muertas sin esperar al timeout.
fn set_keepalive(tcp: &TcpStream) -> Result<()> {
    let params = TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL);
    SockRef::from(tcp)
        .set_tcp_keepalive(&params)
        .map_err(|e| Error::Network(format!("keepalive TCP: {e}")))
}

/// TCP (con keepalive) + TLS + saludo IMAP.
async fn open(host: &str, port: u16) -> Result<Client<TlsStream<TcpStream>>> {
    let tcp = TcpStream::connect((host, port))
        .await
        .map_err(|e| Error::Network(format!("conectando a {host}:{port}: {e}")))?;
    set_keepalive(&tcp)?;

    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|e| Error::Network(format!("nombre de servidor no válido: {e}")))?;
    let tls = TlsConnector::from(tls_config()?)
        .connect(server_name, tcp)
        .await
        .map_err(|e| Error::Network(format!("TLS con {host}: {e}")))?;

    let mut client = Client::new(tls);
    client
        .read_response()
        .await
        .map_err(|e| Error::Network(format!("saludo IMAP: {e}")))?
        .ok_or_else(|| Error::Network("el servidor cerró antes del saludo".into()))?;
    Ok(client)
}

/// Abre TCP + TLS (con los límites de tiempo), autentica con XOAUTH2 y consulta CAPABILITY.
pub async fn connect(host: &str, port: u16, email: &str, access_token: &str) -> Result<Conn> {
    let client = tokio::time::timeout(CONNECT_TIMEOUT, open(host, port))
        .await
        .map_err(|_| timeout_error(CONNECT_TIMEOUT))??;

    let auth = XOAuth2::new(xoauth2_initial_response(email, access_token));
    let login = tokio::time::timeout(LOGIN_TIMEOUT, client.authenticate("XOAUTH2", auth))
        .await
        .map_err(|_| timeout_error(LOGIN_TIMEOUT))?;
    let mut session = login.map_err(|(e, _client)| match e {
        async_imap::error::Error::No(m) | async_imap::error::Error::Bad(m) => {
            Error::Auth(format!("XOAUTH2 rechazado: {m}"))
        }
        other => map_imap_error(other),
    })?;

    let c = imap(OP_TIMEOUT, session.capabilities()).await?;
    let caps = Caps {
        mov: c.has_str("MOVE"),
        uidplus: c.has_str("UIDPLUS"),
        gmail_labels: c.has_str("X-GM-EXT-1"),
        idle: c.has_str("IDLE"),
    };
    Ok(Conn { session, caps })
}

/// Configuración TLS con el proveedor `ring` explícito (en el grafo también hay
/// aws-lc-rs, así que no se usa el proveedor por defecto) y raíces de webpki.
fn tls_config() -> Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Network(format!("configuración TLS: {e}")))?
            .with_root_certificates(roots)
            .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Mecanismo SASL XOAUTH2: envía la respuesta inicial y, si el servidor
/// responde con un desafío (error JSON), una línea vacía para que cierre.
pub struct XOAuth2 {
    initial: Vec<u8>,
    sent: bool,
}

impl XOAuth2 {
    pub fn new(initial: Vec<u8>) -> Self {
        Self {
            initial,
            sent: false,
        }
    }
}

impl Authenticator for XOAuth2 {
    type Response = Vec<u8>;

    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        if self.sent {
            Vec::new()
        } else {
            self.sent = true;
            self.initial.clone()
        }
    }
}

/// Traduce errores de async-imap a los de correo-core.
pub fn map_imap_error(e: async_imap::error::Error) -> Error {
    use async_imap::error::Error as E;
    match e {
        E::Io(_) | E::ConnectionLost => Error::Network(e.to_string()),
        E::No(m) | E::Bad(m) => Error::Protocol(m),
        other => Error::Protocol(other.to_string()),
    }
}

/// Errores que justifican reabrir la sesión y reintentar una vez.
pub fn is_network(e: &Error) -> bool {
    matches!(e, Error::Network(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xoauth2_authenticator_sends_token_then_empty_line() {
        let mut auth = XOAuth2::new(b"user=a\x01auth=Bearer t\x01\x01".to_vec());
        assert_eq!(
            auth.process(b""),
            b"user=a\x01auth=Bearer t\x01\x01".to_vec()
        );
        // Desafío de error de Google: se responde vacío para que el servidor cierre.
        assert!(auth.process(b"{\"status\":\"400\"}").is_empty());
    }

    #[test]
    fn network_errors_are_retryable() {
        assert!(is_network(&Error::Network("x".into())));
        assert!(!is_network(&Error::Auth("x".into())));
        assert!(!is_network(&Error::Protocol("x".into())));
    }

    #[test]
    fn tls_config_builds_with_ring_provider() {
        assert!(tls_config().is_ok());
    }

    #[tokio::test]
    async fn imap_helper_turns_expired_limit_into_network_error() {
        let limit = Duration::from_millis(5);
        let got: Result<()> = imap(
            limit,
            std::future::pending::<async_imap::error::Result<()>>(),
        )
        .await;
        assert!(is_network(&got.unwrap_err()));
    }

    #[tokio::test]
    async fn imap_helper_keeps_server_results_and_maps_errors() {
        let ok = imap(OP_TIMEOUT, async { Ok::<_, async_imap::error::Error>(7) }).await;
        assert_eq!(ok.unwrap(), 7);
        let no = imap(OP_TIMEOUT, async {
            Err::<(), _>(async_imap::error::Error::No("cuota".into()))
        })
        .await;
        assert!(matches!(no, Err(Error::Protocol(m)) if m == "cuota"));
    }

    #[tokio::test]
    async fn keepalive_is_set_on_a_real_socket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tcp, _peer) = tokio::join!(TcpStream::connect(addr), listener.accept());
        let tcp = tcp.unwrap();
        set_keepalive(&tcp).unwrap();
        assert!(SockRef::from(&tcp).keepalive().unwrap());
    }
}

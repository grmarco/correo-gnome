//! Cliente OAuth2 de escritorio: autorización con PKCE, intercambio de código y refresco.

use std::fmt;
use std::time::{Duration, Instant};

use correo_core::{Error, Result};
use reqwest::Url;
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::config::OAuthProvider;
use crate::loopback;
use crate::pkce::{Pkce, random_state};

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Vida supuesta del access token si el proveedor no informa `expires_in`.
/// Se usa 0 para forzar un refresco en la siguiente petición (lo más seguro).
const MISSING_EXPIRES_IN: u64 = 0;

/// Tokens obtenidos de un intercambio de código o de un refresco.
#[derive(Clone)]
pub struct TokenSet {
    pub access_token: String,
    /// Presente en el intercambio inicial (si se pidió `offline_access`/`access_type=offline`).
    pub refresh_token: Option<String>,
    pub expires_at: Instant,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Cliente OAuth2 para un proveedor. Barato de clonar (comparte el pool HTTP).
#[derive(Clone)]
pub struct OAuthClient {
    provider: OAuthProvider,
    http: reqwest::Client,
}

impl fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthClient")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

impl OAuthClient {
    pub fn new(provider: OAuthProvider) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::Network(format!("cliente HTTP: {e}")))?;
        Ok(Self { provider, http })
    }

    pub fn provider(&self) -> &OAuthProvider {
        &self.provider
    }

    /// Prepara una autorización: abre el servidor loopback y construye la URL para el navegador.
    ///
    /// El servidor queda escuchando hasta recibir el callback o agotar `callback_timeout`.
    /// Hay que llamar a [`AuthorizationRequest::finish`] para obtener los tokens.
    pub async fn begin_authorization(&self) -> Result<AuthorizationRequest> {
        let pkce = Pkce::generate();
        let state = random_state();
        let listener = loopback::bind().await?;
        let port = listener
            .local_addr()
            .map_err(|e| Error::Network(format!("dirección loopback: {e}")))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/");

        let mut url = Url::parse(&self.provider.authorization_url)
            .map_err(|e| Error::Protocol(format!("URL de autorización inválida: {e}")))?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", &self.provider.client_id)
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("scope", &self.provider.scopes.join(" "))
                .append_pair("state", &state)
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256");
            for (k, v) in &self.provider.extra_authorization_params {
                query.append_pair(k, v);
            }
        }

        let (tx, rx) = oneshot::channel();
        let timeout = self.provider.callback_timeout;
        let expected_state = state.clone();
        tokio::spawn(async move {
            let result =
                match tokio::time::timeout(timeout, loopback::serve(listener, &expected_state))
                    .await
                {
                    Ok(result) => result,
                    Err(_) => Err(Error::Auth(
                        "tiempo agotado esperando la autorización en el navegador".into(),
                    )),
                };
            let _ = tx.send(result);
        });

        Ok(AuthorizationRequest {
            url: url.into(),
            state,
            redirect_uri,
            pkce_verifier: pkce.verifier,
            callback: rx,
            client: self.clone(),
        })
    }

    /// Intercambia un código de autorización por tokens.
    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<TokenSet> {
        self.post_token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ])
        .await
    }

    /// Obtiene un access token nuevo a partir de un refresh token.
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenSet> {
        self.post_token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .await
    }

    /// Como `refresh`, pero pidiendo un conjunto concreto de scopes (un único recurso).
    pub async fn refresh_scoped(&self, refresh_token: &str, scopes: &[String]) -> Result<TokenSet> {
        let scope = scopes.join(" ");
        self.post_token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", &scope),
        ])
        .await
    }

    async fn post_token(&self, params: &[(&str, &str)]) -> Result<TokenSet> {
        let mut form: Vec<(&str, &str)> = params.to_vec();
        form.push(("client_id", &self.provider.client_id));
        if let Some(secret) = &self.provider.client_secret {
            form.push(("client_secret", secret));
        }

        let response = self
            .http
            .post(&self.provider.token_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| Error::Network(format!("petición de tokens: {e}")))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| Error::Network(format!("respuesta de tokens: {e}")))?;

        if !status.is_success() {
            // No incluimos el cuerpo completo: en errores podría haber datos sensibles.
            return Err(Error::Auth(
                match serde_json::from_str::<ErrorBody>(&body) {
                    Ok(e) => format!(
                        "el proveedor rechazó la petición ({status}): {}",
                        e.error_description.unwrap_or(e.error)
                    ),
                    Err(_) => format!("el proveedor rechazó la petición ({status})"),
                },
            ));
        }

        let parsed: TokenBody = serde_json::from_str(&body)
            .map_err(|_| Error::Protocol("respuesta de tokens no válida".into()))?;
        Ok(TokenSet {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token,
            expires_at: Instant::now()
                + Duration::from_secs(parsed.expires_in.unwrap_or(MISSING_EXPIRES_IN)),
        })
    }
}

/// Autorización en curso: la URL para el navegador y el estado esperado.
pub struct AuthorizationRequest {
    url: String,
    state: String,
    redirect_uri: String,
    pkce_verifier: String,
    callback: oneshot::Receiver<Result<String>>,
    client: OAuthClient,
}

impl fmt::Debug for AuthorizationRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizationRequest")
            .field("url", &self.url)
            .field("state", &self.state)
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

impl AuthorizationRequest {
    /// URL que hay que abrir en el navegador del usuario.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Valor `state` generado para esta autorización.
    pub fn state(&self) -> &str {
        &self.state
    }

    /// URI de redirección loopback registrada en la petición.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Espera el callback del navegador (validado por `state`) e intercambia el código por tokens.
    pub async fn finish(self) -> Result<TokenSet> {
        let code = self
            .callback
            .await
            .map_err(|_| Error::Auth("el servidor de autorización se interrumpió".into()))??;
        self.client
            .exchange_code(&code, &self.redirect_uri, &self.pkce_verifier)
            .await
    }
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
    error_description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkce::s256_challenge;
    use crate::test_support::{FakeServer, token_json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    /// GET crudo a una URL local; devuelve la línea de estado y el cuerpo.
    async fn raw_get(url: &str) -> (String, String) {
        let parsed = Url::parse(url).unwrap();
        let host = format!("{}:{}", parsed.host_str().unwrap(), parsed.port().unwrap());
        let target = match parsed.query() {
            Some(q) => format!("{}?{q}", parsed.path()),
            None => parsed.path().to_owned(),
        };
        let mut stream = TcpStream::connect(&host).await.unwrap();
        let request = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let status = response.lines().next().unwrap_or_default().to_owned();
        let body = response
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or_default()
            .to_owned();
        (status, body)
    }

    fn query_of(url: &str) -> Vec<(String, String)> {
        Url::parse(url)
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn param<'a>(params: &'a [(String, String)], name: &str) -> &'a str {
        params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap()
    }

    fn provider_for(server: &FakeServer) -> OAuthProvider {
        let mut provider = OAuthProvider::google("cid-123");
        provider.token_url = server.url.clone();
        provider
    }

    #[tokio::test]
    async fn authorization_url_contains_pkce_state_and_scopes() {
        let client = OAuthClient::new(OAuthProvider::google("cid-123")).unwrap();
        let request = client.begin_authorization().await.unwrap();
        let params = query_of(request.url());

        assert_eq!(param(&params, "client_id"), "cid-123");
        assert_eq!(param(&params, "response_type"), "code");
        assert_eq!(param(&params, "code_challenge_method"), "S256");
        assert_eq!(param(&params, "state"), request.state());
        assert_eq!(param(&params, "access_type"), "offline");
        assert_eq!(param(&params, "redirect_uri"), request.redirect_uri());
        assert!(param(&params, "scope").contains("https://mail.google.com/"));
        assert!(request.redirect_uri().starts_with("http://127.0.0.1:"));
        assert!(!param(&params, "code_challenge").is_empty());
    }

    #[tokio::test]
    async fn microsoft_provider_uses_common_endpoints() {
        let provider = OAuthProvider::microsoft("ms-cid");
        assert!(
            provider
                .authorization_url
                .contains("common/oauth2/v2.0/authorize")
        );
        assert!(provider.token_url.contains("common/oauth2/v2.0/token"));
        assert!(provider.scopes.iter().any(|s| s == "offline_access"));
        assert!(provider.client_secret.is_none());
    }

    #[tokio::test]
    async fn full_flow_validates_state_and_exchanges_code_with_pkce() {
        let server =
            FakeServer::start(vec![(200, token_json("access-1", Some("refresh-1"), 3600))]).await;
        let client = OAuthClient::new(provider_for(&server)).unwrap();
        let request = client.begin_authorization().await.unwrap();
        let challenge = param(&query_of(request.url()), "code_challenge").to_owned();
        let redirect = request.redirect_uri().to_owned();
        let state = request.state().to_owned();

        // Un state incorrecto se rechaza y no termina el flujo.
        let (status, _) = raw_get(&format!("{redirect}?code=abc&state=otro")).await;
        assert!(status.contains("400"), "{status}");

        // Peticiones sin código se ignoran.
        let (status, _) = raw_get(&format!("{redirect}favicon.ico")).await;
        assert!(status.contains("404"), "{status}");

        let (status, body) = raw_get(&format!("{redirect}?code=the-code&state={state}")).await;
        assert!(status.contains("200"), "{status}");
        assert!(body.contains("Autorización completada"));

        let tokens = request.finish().await.unwrap();
        assert_eq!(tokens.access_token, "access-1");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-1"));

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let body = requests[0].split("\r\n\r\n").nth(1).unwrap();
        let form = query_of(&format!("http://x/?{body}"));
        assert_eq!(param(&form, "grant_type"), "authorization_code");
        assert_eq!(param(&form, "code"), "the-code");
        assert_eq!(param(&form, "client_id"), "cid-123");
        assert_eq!(param(&form, "redirect_uri"), redirect);
        // El verificador enviado debe corresponder al challenge de la URL de autorización.
        assert_eq!(s256_challenge(param(&form, "code_verifier")), challenge);
        assert!(form.iter().all(|(k, _)| k != "client_secret"));
    }

    #[tokio::test]
    async fn client_secret_is_sent_only_when_configured() {
        let server = FakeServer::start(vec![(200, token_json("a", None, 3600))]).await;
        let provider = provider_for(&server).with_client_secret("s3cr3t");
        let client = OAuthClient::new(provider).unwrap();
        let refreshed = client.refresh("rt").await.unwrap();
        assert_eq!(refreshed.refresh_token, None);
        let body = server.requests()[0]
            .split("\r\n\r\n")
            .nth(1)
            .unwrap()
            .to_owned();
        let form = query_of(&format!("http://x/?{body}"));
        assert_eq!(param(&form, "client_secret"), "s3cr3t");
        assert_eq!(param(&form, "refresh_token"), "rt");
    }

    #[tokio::test]
    async fn provider_error_is_reported_as_auth_error() {
        let server = FakeServer::start(vec![(
            400,
            "{\"error\":\"invalid_grant\",\"error_description\":\"Token has been revoked\"}".into(),
        )])
        .await;
        let client = OAuthClient::new(provider_for(&server)).unwrap();
        let err = client.refresh("rt").await.unwrap_err();
        match err {
            Error::Auth(msg) => assert!(msg.contains("Token has been revoked"), "{msg}"),
            other => panic!("error inesperado: {other:?}"),
        }
    }

    #[tokio::test]
    async fn user_denial_is_reported_as_auth_error() {
        let client = OAuthClient::new(OAuthProvider::google("cid")).unwrap();
        let request = client.begin_authorization().await.unwrap();
        let redirect = request.redirect_uri().to_owned();
        let state = request.state().to_owned();
        let (status, _) = raw_get(&format!(
            "{redirect}?error=access_denied&error_description=cancelado&state={state}"
        ))
        .await;
        assert!(status.contains("200"), "{status}");
        match request.finish().await.unwrap_err() {
            Error::Auth(msg) => assert!(msg.contains("cancelado"), "{msg}"),
            other => panic!("error inesperado: {other:?}"),
        }
    }

    #[tokio::test]
    async fn callback_timeout_fails_finish() {
        let mut provider = OAuthProvider::google("cid");
        provider.callback_timeout = std::time::Duration::from_millis(100);
        let client = OAuthClient::new(provider).unwrap();
        let request = client.begin_authorization().await.unwrap();
        assert!(matches!(request.finish().await, Err(Error::Auth(_))));
    }
}

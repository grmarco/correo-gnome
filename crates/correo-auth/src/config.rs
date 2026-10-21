use std::fmt;
use std::time::Duration;

const GOOGLE_AUTHORIZATION_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const MICROSOFT_AUTHORIZATION_URL: &str =
    "https://login.microsoftonline.com/common/oauth2/v2.0/authorize";
const MICROSOFT_TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

/// Configuración de un proveedor OAuth2 para una aplicación de escritorio (cliente público).
#[derive(Clone)]
pub struct OAuthProvider {
    pub authorization_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub client_id: String,
    /// Opcional: solo si la aplicación registrada es confidencial.
    pub client_secret: Option<String>,
    /// Parámetros extra de la URL de autorización (p. ej. `access_type=offline` en Google).
    pub extra_authorization_params: Vec<(String, String)>,
    /// Tiempo máximo que el servidor loopback espera la redirección del navegador.
    pub callback_timeout: Duration,
}

impl OAuthProvider {
    /// Google (Gmail, Contacts, userinfo). Pide refresh token siempre.
    pub fn google(client_id: impl Into<String>) -> Self {
        Self {
            authorization_url: GOOGLE_AUTHORIZATION_URL.into(),
            token_url: GOOGLE_TOKEN_URL.into(),
            scopes: vec![
                "https://mail.google.com/".into(),
                "https://www.googleapis.com/auth/contacts.readonly".into(),
                "https://www.googleapis.com/auth/userinfo.email".into(),
            ],
            client_id: client_id.into(),
            client_secret: None,
            extra_authorization_params: vec![
                ("access_type".into(), "offline".into()),
                ("prompt".into(), "consent".into()),
            ],
            callback_timeout: Duration::from_secs(300),
        }
    }

    /// Microsoft (Outlook.com y Microsoft 365, endpoint `common`).
    pub fn microsoft(client_id: impl Into<String>) -> Self {
        Self {
            authorization_url: MICROSOFT_AUTHORIZATION_URL.into(),
            token_url: MICROSOFT_TOKEN_URL.into(),
            scopes: vec![
                "https://outlook.office.com/IMAP.AccessAsUser.All".into(),
                "https://outlook.office.com/SMTP.Send".into(),
                "offline_access".into(),
                "Contacts.Read".into(),
                "User.Read".into(),
            ],
            client_id: client_id.into(),
            client_secret: None,
            extra_authorization_params: Vec::new(),
            callback_timeout: Duration::from_secs(300),
        }
    }

    /// Añade un client secret (solo para aplicaciones confidenciales).
    pub fn with_client_secret(mut self, secret: impl Into<String>) -> Self {
        self.client_secret = Some(secret.into());
        self
    }
}

impl fmt::Debug for OAuthProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthProvider")
            .field("authorization_url", &self.authorization_url)
            .field("token_url", &self.token_url)
            .field("scopes", &self.scopes)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "extra_authorization_params",
                &self.extra_authorization_params,
            )
            .field("callback_timeout", &self.callback_timeout)
            .finish()
    }
}

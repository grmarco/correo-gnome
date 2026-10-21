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
    /// Scopes del refresco por audiencia. Vacío = el token es válido para todo (Google).
    pub mail_scopes: Vec<String>,
    pub directory_scopes: Vec<String>,
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
            mail_scopes: Vec::new(),
            directory_scopes: Vec::new(),
            client_id: client_id.into(),
            client_secret: None,
            // `select_account`: el navegador pregunta qué cuenta usar en vez de reutilizar la
            // última sesión; así la segunda cuenta de Google no sustituye a la primera.
            extra_authorization_params: vec![
                ("access_type".into(), "offline".into()),
                ("prompt".into(), "select_account consent".into()),
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
            mail_scopes: vec![
                "https://outlook.office.com/IMAP.AccessAsUser.All".into(),
                "https://outlook.office.com/SMTP.Send".into(),
                "offline_access".into(),
            ],
            directory_scopes: vec![
                "https://graph.microsoft.com/Contacts.Read".into(),
                "https://graph.microsoft.com/User.Read".into(),
                "offline_access".into(),
            ],
            client_id: client_id.into(),
            client_secret: None,
            // Igual que en Google: elegir cuenta explícitamente para poder añadir varias
            // cuentas de Microsoft sin que el navegador reutilice la primera.
            extra_authorization_params: vec![("prompt".into(), "select_account".into())],
            callback_timeout: Duration::from_secs(300),
        }
    }

    /// Usa un tenant concreto en vez de `common` (GUID, dominio, `organizations` o
    /// `consumers`). Hace falta si la app está registrada como de un solo inquilino, p. ej.
    /// dentro del tenant de una organización. Un valor con caracteres no válidos se ignora.
    pub fn with_tenant(mut self, tenant: &str) -> Self {
        let tenant = tenant.trim();
        let valid = !tenant.is_empty()
            && tenant
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
        if valid {
            self.authorization_url = self
                .authorization_url
                .replace("/common/", &format!("/{tenant}/"));
            self.token_url = self.token_url.replace("/common/", &format!("/{tenant}/"));
        }
        self
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

#[cfg(test)]
mod tenant_tests {
    use super::OAuthProvider;

    #[test]
    fn tenant_replaces_common_in_both_endpoints() {
        let p = OAuthProvider::microsoft("cid").with_tenant("contoso.onmicrosoft.com");
        assert!(
            p.authorization_url
                .contains("/contoso.onmicrosoft.com/oauth2/v2.0/authorize")
        );
        assert!(
            p.token_url
                .contains("/contoso.onmicrosoft.com/oauth2/v2.0/token")
        );
    }

    #[test]
    fn invalid_tenant_is_ignored() {
        let p = OAuthProvider::microsoft("cid").with_tenant("evil.com/../x?y=1");
        assert!(p.token_url.contains("/common/"));
        let p = OAuthProvider::microsoft("cid").with_tenant("  ");
        assert!(p.token_url.contains("/common/"));
    }
}

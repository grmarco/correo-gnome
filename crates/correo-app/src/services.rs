//! Capa de servicios compartida por la interfaz: caché, tokens y backends por cuenta.
//! La UI vive en el hilo principal de GTK; el trabajo de red va al runtime de tokio.

use std::future::Future;
use std::sync::{Arc, Mutex};

use correo_auth::{MemoryTokenStore, OAuthClient, OAuthProvider, OAuthTokenProvider};
use correo_core::{Account, ContactSource, MailBackend, Provider, Result, Sender, TokenProvider};
use correo_imap::ImapBackend;
use correo_store::Store;

pub struct Services {
    pub store: Arc<Mutex<Store>>,
    pub tokens: Arc<dyn TokenProvider>,
    runtime: tokio::runtime::Runtime,
}

impl Services {
    /// Abre la BD en `~/.local/share/correo/correo.db`. `tokens` lo construye la app
    /// (OAuth con los client IDs de la configuración).
    pub fn new(store: Store, tokens: Arc<dyn TokenProvider>) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            tokens,
            runtime,
        })
    }

    /// Ejecuta un future en el runtime de tokio y devuelve su resultado.
    /// Es awaitable desde `glib::MainContext::spawn_local`.
    pub fn spawn<F, T>(&self, fut: F) -> impl Future<Output = T> + 'static
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let handle = self.runtime.spawn(fut);
        async move { handle.await.expect("tarea de tokio cancelada") }
    }

    pub fn backend(&self, account: &Account) -> Arc<dyn MailBackend> {
        let (tokens, store) = (self.tokens.clone(), self.store.clone());
        let email = account.email.clone();
        Arc::new(match account.provider {
            Provider::Gmail => ImapBackend::gmail(account.id, email, tokens, store),
            _ => ImapBackend::outlook(account.id, email, tokens, store),
        })
    }

    pub fn sender(&self, account: &Account) -> Arc<dyn Sender> {
        let (tokens, email) = (self.tokens.clone(), account.email.clone());
        Arc::new(match account.provider {
            Provider::Gmail => correo_smtp::SmtpSender::gmail(account.id, email, tokens),
            _ => correo_smtp::SmtpSender::outlook(account.id, email, tokens),
        })
    }

    pub fn contacts(&self, account: &Account) -> Arc<dyn ContactSource> {
        let tokens = self.tokens.clone();
        match account.provider {
            Provider::Gmail => Arc::new(correo_contacts::GoogleContacts::new(account.id, tokens)),
            _ => Arc::new(correo_contacts::MicrosoftContacts::new(account.id, tokens)),
        }
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        self.store.lock().expect("store envenenado").list_accounts()
    }
}

/// Construye el proveedor de tokens a partir de los client IDs (variables de entorno
/// `CORREO_GOOGLE_CLIENT_ID` / `CORREO_MICROSOFT_CLIENT_ID`, o fichero de config en T12).
pub fn oauth_provider(provider: Provider, client_id: &str) -> OAuthProvider {
    match provider {
        Provider::Gmail => OAuthProvider::google(client_id),
        _ => OAuthProvider::microsoft(client_id),
    }
}

#[allow(dead_code)]
type _Used = (OAuthClient, OAuthTokenProvider, MemoryTokenStore);

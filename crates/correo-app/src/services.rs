//! Capa de servicios compartida por la interfaz: caché, tokens y backends por cuenta.
//! La UI vive en el hilo principal de GTK; el trabajo de red va al runtime de tokio.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use correo_core::{
    Account, AccountId, ContactSource, MailBackend, Provider, Result, Sender, TokenProvider,
};
use correo_imap::ImapBackend;
use correo_store::Store;

/// Future que borra los tokens de una cuenta (memoria y almacén seguro).
pub type ForgetTokensFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
/// Hook que aporta la app para olvidar los tokens de una cuenta.
pub type ForgetTokens = Arc<dyn Fn(AccountId) -> ForgetTokensFuture + Send + Sync>;

pub struct Services {
    pub store: Arc<Mutex<Store>>,
    pub tokens: Arc<dyn TokenProvider>,
    runtime: tokio::runtime::Runtime,
    /// Backend IMAP por cuenta: conserva su conexión TLS+sesión entre operaciones.
    backends: Mutex<HashMap<AccountId, Arc<ImapBackend>>>,
    forget_tokens: Option<ForgetTokens>,
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
            backends: Mutex::new(HashMap::new()),
            forget_tokens: None,
        })
    }

    /// Hook para borrar los tokens de una cuenta al eliminarla (lo aporta `setup`).
    pub fn with_forget_tokens(mut self, forget: ForgetTokens) -> Self {
        self.forget_tokens = Some(forget);
        self
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

    /// Backend de correo de la cuenta, reutilizado entre llamadas. Tras cambiar el proveedor
    /// o el correo de una cuenta, llamar antes a `forget_backend`.
    pub fn backend(&self, account: &Account) -> Arc<dyn MailBackend> {
        let mut backends = self.backends.lock().unwrap_or_else(|p| p.into_inner());
        let backend = backends
            .entry(account.id)
            .or_insert_with(|| Arc::new(self.imap(account)));
        Arc::clone(backend) as Arc<dyn MailBackend>
    }

    /// Descarta el backend cacheado de la cuenta (cierra su conexión al liberarse).
    #[allow(dead_code)] // lo usa `remove_account`, aún sin llamador en la UI.
    pub fn forget_backend(&self, account: AccountId) {
        self.backends
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&account);
    }

    /// Backend IMAP concreto (para operaciones fuera de `MailBackend`, p. ej. IDLE o
    /// guardar una copia en Enviados).
    pub fn imap(&self, account: &Account) -> ImapBackend {
        let (tokens, store) = (self.tokens.clone(), self.store.clone());
        let email = account.email.clone();
        match account.provider {
            Provider::Gmail => ImapBackend::gmail(account.id, email, tokens, store),
            _ => ImapBackend::outlook(account.id, email, tokens, store),
        }
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

    /// Elimina la cuenta de la caché y sus tokens. La BD se borra primero (si la cuenta no
    /// existe, no se toca ningún token). El borrado de tokens va en segundo plano y sus
    /// errores se registran en stderr.
    #[allow(dead_code)] // pendiente de que la UI de eliminar cuenta lo llame.
    pub fn remove_account(&self, account: AccountId) -> Result<()> {
        self.store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .delete_account(account)?;
        self.forget_backend(account);
        if let Some(forget) = self.forget_tokens.clone() {
            self.runtime.spawn(async move {
                if let Err(e) = forget(account).await {
                    eprintln!(
                        "correo: no se pudieron borrar los tokens de la cuenta {account}: {e}"
                    );
                }
            });
        }
        Ok(())
    }
}

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

/// Aplana el resultado de un future de `Services::spawn` cuyo valor ya era un `Result`.
pub async fn flatten<T>(fut: impl Future<Output = Result<Result<T>>>) -> Result<T> {
    fut.await.and_then(|r| r)
}

/// Convierte el `JoinHandle` de una tarea en un `Result`: un pánico o una cancelación se
/// registran en stderr y se devuelven como error, sin propagar el pánico.
async fn join_result<T>(handle: tokio::task::JoinHandle<T>) -> Result<T> {
    handle.await.map_err(|e| {
        if e.is_panic() {
            eprintln!("correo: tarea interna en pánico: {e}");
        }
        correo_core::Error::Network(format!("tarea interna fallida: {e}"))
    })
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

    /// Ejecuta un future en el runtime de tokio y devuelve su resultado. Si la tarea entra
    /// en pánico o se cancela, el resultado es `Err` (el pánico no se propaga a la UI).
    /// Es awaitable desde `glib::MainContext::spawn_local`.
    pub fn spawn<F, T>(&self, fut: F) -> impl Future<Output = Result<T>> + 'static
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        join_result(self.runtime.spawn(fut))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn un_panico_devuelve_error_sin_propagarse() {
        let handle = tokio::spawn(async { panic!("fallo a propósito") });
        let r: Result<()> = join_result(handle).await;
        assert!(
            matches!(r, Err(correo_core::Error::Network(ref m)) if m.contains("tarea interna"))
        );
    }

    #[tokio::test]
    async fn una_tarea_correcta_devuelve_su_valor() {
        let r = join_result(tokio::spawn(async { 7 })).await;
        assert_eq!(r.unwrap(), 7);
    }

    #[test]
    fn backends_are_cached_per_account_and_never_shared() {
        let services = Services::new(Store::open_in_memory().unwrap(), Arc::new(NoTokens)).unwrap();
        let gmail_1 = account(1, Provider::Gmail, "uno@gmail.com");
        let gmail_2 = account(2, Provider::Gmail, "dos@gmail.com");
        let outlook_3 = account(3, Provider::Outlook, "tres@outlook.com");

        let first = services.backend(&gmail_1);
        assert!(
            Arc::ptr_eq(&first, &services.backend(&gmail_1)),
            "misma cuenta, misma sesión"
        );
        assert!(!Arc::ptr_eq(&first, &services.backend(&gmail_2)));
        assert!(!Arc::ptr_eq(&first, &services.backend(&outlook_3)));
        assert!(!Arc::ptr_eq(
            &services.backend(&gmail_2),
            &services.backend(&outlook_3)
        ));

        services.forget_backend(1);
        assert!(
            !Arc::ptr_eq(&first, &services.backend(&gmail_1)),
            "tras olvidarla, una nueva"
        );
    }

    #[test]
    fn remove_account_deletes_cache_and_keeps_the_others() {
        let services = Services::new(Store::open_in_memory().unwrap(), Arc::new(NoTokens)).unwrap();
        let a = services
            .store
            .lock()
            .unwrap()
            .create_account(&account(0, Provider::Gmail, "a@gmail.com"))
            .unwrap();
        let b = services
            .store
            .lock()
            .unwrap()
            .create_account(&account(0, Provider::Gmail, "b@gmail.com"))
            .unwrap();
        services.remove_account(a).unwrap();
        let left = services.accounts().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, b);
        assert!(services.remove_account(a).is_err(), "ya no existe");
    }

    fn account(id: AccountId, provider: Provider, email: &str) -> Account {
        Account {
            id,
            provider,
            email: email.into(),
            display_name: String::new(),
            sync_days: 30,
        }
    }

    /// Sin tokens: los backends se crean pero no se usan en estos tests.
    struct NoTokens;

    #[async_trait::async_trait]
    impl TokenProvider for NoTokens {
        async fn access_token(&self, _account: AccountId) -> Result<correo_core::AccessToken> {
            Err(correo_core::Error::Auth("sin tokens en este test".into()))
        }
    }
}

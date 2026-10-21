//! T12: arranque de servicios (BD, configuración, tokens) y alta de cuentas.
//!
//! Este fichero declara también los módulos de su área (`config`, `power`,
//! `account_dialog`, `background`) para no tocar `main.rs`.

#[path = "account_dialog.rs"]
mod account_dialog;
#[path = "background.rs"]
pub mod background;
#[path = "config.rs"]
pub mod config;
#[path = "power.rs"]
pub mod power;

pub use account_dialog::{add_account_dialog, AddOutcome};
pub use background::{start_sync, MailboxSync, SyncHandle};

use std::error::Error as StdError;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use correo_auth::{
    MemoryTokenStore, OAuthClient, OAuthProvider, OAuthTokenProvider, SecretServiceStore, TokenSet,
    TokenStore,
};
use correo_core::{AccessToken, AccountId, Error, Provider, Result, TokenAudience, TokenProvider};
use correo_store::Store;
use gtk::glib;

use crate::services::{ForgetTokens, Services};
use config::Config;

/// Identificador de la aplicación en el keyring (atributo `application`).
const KEYRING_APP: &str = "io.github.gmarco.Correo";

static CONFIG: OnceLock<Config> = OnceLock::new();
/// Enrutador de tokens compartido: la misma instancia (y por tanto la misma caché de access
/// tokens) la usan los servicios y el alta/reautorización de cuentas.
static ROUTER: OnceLock<Arc<TokenRouter>> = OnceLock::new();

/// Configuración cargada en `init_services`. Si no se ha inicializado, carga la de disco.
pub fn app_config() -> &'static Config {
    CONFIG.get_or_init(|| config::load(&config::config_path()).unwrap_or_default())
}

/// Configuración OAuth del proveedor, o `None` si falta su client ID. Si hay client secret
/// configurado (Google lo suele exigir), se añade a las peticiones de token.
pub fn oauth_config(provider: Provider) -> Option<OAuthProvider> {
    let cfg = app_config();
    match provider {
        Provider::Gmail => cfg.google_client_id().map(|id| {
            let provider = OAuthProvider::google(id);
            match cfg.google_client_secret() {
                Some(secret) => provider.with_client_secret(secret),
                None => provider,
            }
        }),
        Provider::Outlook | Provider::Microsoft365 => cfg.microsoft_client_id().map(|id| {
            let provider = match cfg.microsoft_tenant() {
                Some(tenant) => OAuthProvider::microsoft(id).with_tenant(tenant),
                None => OAuthProvider::microsoft(id),
            };
            match cfg.microsoft_client_secret() {
                Some(secret) => provider.with_client_secret(secret),
                None => provider,
            }
        }),
        Provider::Imap => None,
    }
}

/// Crea los servicios: BD en `~/.local/share/correo/correo.db`, configuración y tokens.
pub fn init_services() -> std::result::Result<Services, Box<dyn StdError>> {
    let db_path = db_path()?;
    let store = Store::open(&db_path)?;

    let cfg = config::load(&config::config_path())?;
    let _ = CONFIG.set(cfg);
    let cfg = app_config();
    if cfg.google_client_id().is_none() && cfg.microsoft_client_id().is_none() {
        eprintln!(
            "correo: aviso: no hay client ID de OAuth configurado; ver docs/OAUTH.md \
             (config.toml o CORREO_GOOGLE_CLIENT_ID / CORREO_MICROSOFT_CLIENT_ID)"
        );
    }

    let token_store = choose_token_store();

    // Segunda conexión a la misma BD: el enrutador de tokens necesita el proveedor de
    // cada cuenta y `Services` toma la suya por valor.
    let google = oauth_provider_for(oauth_config(Provider::Gmail), &token_store)?;
    let microsoft = oauth_provider_for(oauth_config(Provider::Microsoft365), &token_store)?;
    let forget = forget_tokens_hook(Arc::clone(&token_store), google.clone(), microsoft.clone());
    let router = Arc::new(TokenRouter {
        accounts: Mutex::new(Store::open(&db_path)?),
        google,
        microsoft,
    });
    let _ = ROUTER.set(Arc::clone(&router));

    Ok(Services::new(store, router)?.with_forget_tokens(forget))
}

/// Guarda los tokens de una cuenta (alta o reautorización) en el almacén y en la caché del
/// enrutador que usan los servicios, para que el siguiente access token sea el nuevo.
pub async fn save_account_tokens(
    provider: Provider,
    account: AccountId,
    tokens: &TokenSet,
) -> Result<()> {
    let router = ROUTER
        .get()
        .ok_or_else(|| Error::Store("los servicios no están iniciados".into()))?;
    router
        .for_provider(provider)?
        .save_tokens(account, tokens)
        .await
}

/// Borra el refresh token de la cuenta en el almacén y su caché en memoria (si hay proveedor
/// OAuth configurado, el proveedor la limpia; si no, solo el almacén).
fn forget_tokens_hook(
    store: Arc<dyn TokenStore>,
    google: Option<Arc<OAuthTokenProvider>>,
    microsoft: Option<Arc<OAuthTokenProvider>>,
) -> ForgetTokens {
    Arc::new(move |account| {
        let store = Arc::clone(&store);
        // Ambos proveedores comparten el almacén; la caché en memoria solo existe en el suyo.
        let provider = google.clone().or_else(|| microsoft.clone());
        Box::pin(async move {
            match provider {
                Some(p) => p.remove_account(account).await,
                None => store.delete_refresh_token(account).await,
            }
        })
    })
}

fn db_path() -> std::io::Result<PathBuf> {
    let dir = glib::user_data_dir().join("correo");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("correo.db"))
}

fn oauth_provider_for(
    provider: Option<OAuthProvider>,
    store: &Arc<dyn TokenStore>,
) -> Result<Option<Arc<OAuthTokenProvider>>> {
    let Some(provider) = provider else {
        return Ok(None);
    };
    let client = OAuthClient::new(provider)?;
    Ok(Some(Arc::new(OAuthTokenProvider::new(
        client,
        Arc::clone(store),
    ))))
}

/// Keyring si responde; si no, memoria (los tokens se pierden al cerrar la app).
fn choose_token_store() -> Arc<dyn TokenStore> {
    let secret = SecretServiceStore::new(KEYRING_APP);
    match probe_keyring(&secret) {
        Ok(()) => Arc::new(secret),
        Err(e) => {
            eprintln!(
                "correo: aviso: el keyring no está disponible ({e}); \
                 los tokens solo se guardan en memoria durante esta sesión"
            );
            Arc::new(MemoryTokenStore::new())
        }
    }
}

fn probe_keyring(store: &SecretServiceStore) -> std::result::Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    // Cuenta 0 no existe nunca: solo se comprueba que el servicio responde.
    rt.block_on(store.load_refresh_token(0))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Enruta cada petición de token al proveedor OAuth de la cuenta (Google o Microsoft).
pub struct TokenRouter {
    accounts: Mutex<Store>,
    google: Option<Arc<OAuthTokenProvider>>,
    microsoft: Option<Arc<OAuthTokenProvider>>,
}

impl TokenRouter {
    fn route(&self, account: AccountId) -> Result<&OAuthTokenProvider> {
        let provider = {
            let store = self.accounts.lock().unwrap_or_else(|p| p.into_inner());
            store.get_account(account)?.provider
        };
        self.for_provider(provider)
    }

    /// Proveedor OAuth de un tipo de cuenta (todas las cuentas de un mismo proveedor
    /// comparten instancia; la caché interna está indexada por cuenta).
    fn for_provider(&self, provider: Provider) -> Result<&OAuthTokenProvider> {
        let (slot, name) = match provider {
            Provider::Gmail => (&self.google, "Google"),
            Provider::Outlook | Provider::Microsoft365 => (&self.microsoft, "Microsoft"),
            Provider::Imap => {
                return Err(Error::Auth(
                    "las cuentas IMAP genéricas no usan OAuth".into(),
                ));
            }
        };
        slot.as_deref()
            .ok_or_else(|| Error::Auth(format!("falta el client ID de {name}; ver docs/OAUTH.md")))
    }
}

#[async_trait]
impl TokenProvider for TokenRouter {
    async fn access_token(&self, account: AccountId) -> Result<AccessToken> {
        self.route(account)?.access_token(account).await
    }

    async fn access_token_for(
        &self,
        account: AccountId,
        audience: TokenAudience,
    ) -> Result<AccessToken> {
        self.route(account)?
            .access_token_for(account, audience)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cuatro cuentas (dos Gmail, dos Outlook/M365) con refresh tokens distintos contra un
    /// servidor OAuth falso que responde con un access token derivado de cada refresh token.
    /// Cada cuenta debe recibir SU token, también en el recurso de contactos de Microsoft.
    #[tokio::test]
    async fn accounts_of_the_same_provider_get_their_own_tokens() {
        use correo_auth::test_support::{token_json, FakeServer};
        use correo_core::{Account, TokenAudience};

        let server = FakeServer::start_with(|request| {
            let rt: String = request
                .split("refresh_token=")
                .nth(1)
                .unwrap_or("")
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            (200, token_json(&format!("access-{rt}"), None, 3600))
        })
        .await;

        let accounts = Store::open_in_memory().unwrap();
        let mut ids = Vec::new();
        for (provider, email) in [
            (Provider::Gmail, "uno@gmail.com"),
            (Provider::Gmail, "dos@gmail.com"),
            (Provider::Outlook, "tres@outlook.com"),
            (Provider::Microsoft365, "cuatro@empresa.example"),
        ] {
            ids.push(
                accounts
                    .create_account(&Account {
                        id: 0,
                        provider,
                        email: email.into(),
                        display_name: String::new(),
                        sync_days: 30,
                    })
                    .unwrap(),
            );
        }

        let store: Arc<dyn TokenStore> = Arc::new(MemoryTokenStore::new());
        for id in &ids {
            store
                .save_refresh_token(*id, &format!("rt-{id}"))
                .await
                .unwrap();
        }
        let mut google_cfg = OAuthProvider::google("cid-google");
        google_cfg.token_url = server.url.clone();
        let mut ms_cfg = OAuthProvider::microsoft("cid-ms");
        ms_cfg.token_url = server.url.clone();
        let router = TokenRouter {
            accounts: Mutex::new(accounts),
            google: Some(Arc::new(OAuthTokenProvider::new(
                OAuthClient::new(google_cfg).unwrap(),
                Arc::clone(&store),
            ))),
            microsoft: Some(Arc::new(OAuthTokenProvider::new(
                OAuthClient::new(ms_cfg).unwrap(),
                store,
            ))),
        };

        for id in &ids {
            let mail = router.access_token(*id).await.unwrap();
            assert_eq!(
                mail.0,
                format!("access-rt-{id}"),
                "correo de la cuenta {id}"
            );
        }
        for id in &ids[2..] {
            let graph = router
                .access_token_for(*id, TokenAudience::Directory)
                .await
                .unwrap();
            assert_eq!(
                graph.0,
                format!("access-rt-{id}"),
                "contactos de la cuenta {id}"
            );
        }
        // Cada cuenta se refrescó con su propio refresh token: cuatro peticiones de correo y
        // dos de contactos, cada una con el refresh token de su cuenta.
        let requests = server.requests();
        assert_eq!(requests.len(), 6);
        for id in &ids {
            assert!(requests
                .iter()
                .any(|r| r.contains(&format!("refresh_token=rt-{id}&"))
                    || r.ends_with(&format!("refresh_token=rt-{id}"))));
        }
    }

    #[test]
    fn router_rejects_unknown_account() {
        let router = TokenRouter {
            accounts: Mutex::new(Store::open_in_memory().unwrap()),
            google: None,
            microsoft: None,
        };
        assert!(matches!(router.route(42), Err(Error::NotFound(_))));
    }
}

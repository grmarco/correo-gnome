//! T12: arranque de servicios (BD, configuración, tokens) y alta de cuentas.
//!
//! Este fichero declara también los módulos de su área (`config`, `power`,
//! `account_dialog`, `background`) para no tocar `main.rs`.

#[allow(dead_code)]
#[path = "account_dialog.rs"]
mod account_dialog;
#[allow(dead_code)]
#[path = "background.rs"]
pub mod background;
#[allow(dead_code)]
#[path = "config.rs"]
pub mod config;
#[allow(dead_code)]
#[path = "power.rs"]
pub mod power;

#[allow(unused_imports)]
pub use account_dialog::add_account_dialog;
#[allow(unused_imports)]
pub use background::{start_sync, SyncHandle};

use std::error::Error as StdError;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use correo_auth::{
    MemoryTokenStore, OAuthClient, OAuthProvider, OAuthTokenProvider, SecretServiceStore,
    TokenStore,
};
use correo_core::{AccessToken, AccountId, Error, Provider, Result, TokenProvider};
use correo_store::Store;
use gtk::glib;

use crate::services::Services;
use config::Config;

/// Identificador de la aplicación en el keyring (atributo `application`).
const KEYRING_APP: &str = "io.github.gmarco.Correo";

static CONFIG: OnceLock<Config> = OnceLock::new();
static TOKEN_STORE: OnceLock<Arc<dyn TokenStore>> = OnceLock::new();

/// Configuración cargada en `init_services`. Si no se ha inicializado, carga la de disco.
pub fn app_config() -> &'static Config {
    CONFIG.get_or_init(|| config::load(&config::config_path()).unwrap_or_default())
}

/// Almacén de refresh tokens compartido (keyring, o memoria si el keyring no responde).
pub fn token_store() -> Result<Arc<dyn TokenStore>> {
    TOKEN_STORE
        .get()
        .cloned()
        .ok_or_else(|| Error::Store("los servicios no están iniciados".into()))
}

/// Configuración OAuth del proveedor, o `None` si falta su client ID.
pub fn oauth_config(provider: Provider) -> Option<OAuthProvider> {
    let cfg = app_config();
    match provider {
        Provider::Gmail => cfg.google_client_id().map(OAuthProvider::google),
        Provider::Outlook | Provider::Microsoft365 => {
            cfg.microsoft_client_id().map(OAuthProvider::microsoft)
        }
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
    let _ = TOKEN_STORE.set(Arc::clone(&token_store));

    // Segunda conexión a la misma BD: el enrutador de tokens necesita el proveedor de
    // cada cuenta y `Services` toma la suya por valor.
    let router = TokenRouter {
        accounts: Mutex::new(Store::open(&db_path)?),
        google: oauth_provider_for(
            cfg.google_client_id().map(OAuthProvider::google),
            &token_store,
        )?,
        microsoft: oauth_provider_for(
            cfg.microsoft_client_id().map(OAuthProvider::microsoft),
            &token_store,
        )?,
    };

    Ok(Services::new(store, Arc::new(router))?)
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
}

#[cfg(test)]
mod tests {
    use super::*;

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

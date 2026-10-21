//! Almacenamiento de refresh tokens por cuenta.

use std::collections::HashMap;
use std::sync::Mutex;

use correo_core::{AccountId, Result};

/// Guarda el refresh token de cada cuenta. Nunca se guarda en disco en claro.
#[async_trait::async_trait]
pub trait TokenStore: Send + Sync {
    async fn load_refresh_token(&self, account: AccountId) -> Result<Option<String>>;
    async fn save_refresh_token(&self, account: AccountId, token: &str) -> Result<()>;
    async fn delete_refresh_token(&self, account: AccountId) -> Result<()>;
}

/// Almacén en memoria (tests y uso sin keyring; los tokens se pierden al cerrar).
#[derive(Debug, Default)]
pub struct MemoryTokenStore {
    tokens: Mutex<HashMap<AccountId, String>>,
}

impl MemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl TokenStore for MemoryTokenStore {
    async fn load_refresh_token(&self, account: AccountId) -> Result<Option<String>> {
        Ok(lock(&self.tokens).get(&account).cloned())
    }

    async fn save_refresh_token(&self, account: AccountId, token: &str) -> Result<()> {
        lock(&self.tokens).insert(account, token.to_owned());
        Ok(())
    }

    async fn delete_refresh_token(&self, account: AccountId) -> Result<()> {
        lock(&self.tokens).remove(&account);
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Un envenenamiento no deja datos inconsistentes aquí: cada operación es un único insert/get/remove.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Almacén en el Secret Service (GNOME Keyring / KWallet) mediante `oo7`.
#[cfg(feature = "libsecret")]
#[derive(Debug, Clone)]
pub struct SecretServiceStore {
    application: String,
}

#[cfg(feature = "libsecret")]
impl SecretServiceStore {
    /// `application` identifica la app en el keyring (p. ej. `"es.gmarco.correo"`).
    pub fn new(application: impl Into<String>) -> Self {
        Self {
            application: application.into(),
        }
    }

    fn attributes(&self, account: AccountId) -> [(&'static str, String); 3] {
        [
            ("application", self.application.clone()),
            ("kind", "oauth-refresh-token".to_owned()),
            ("account_id", account.to_string()),
        ]
    }
}

#[cfg(feature = "libsecret")]
fn store_error(context: &str, e: impl std::fmt::Display) -> correo_core::Error {
    correo_core::Error::Store(format!("{context}: {e}"))
}

#[cfg(feature = "libsecret")]
#[async_trait::async_trait]
impl TokenStore for SecretServiceStore {
    async fn load_refresh_token(&self, account: AccountId) -> Result<Option<String>> {
        let keyring = oo7::Keyring::new()
            .await
            .map_err(|e| store_error("secret service", e))?;
        let attributes = self.attributes(account);
        let attrs: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let items = keyring
            .search_items(&attrs)
            .await
            .map_err(|e| store_error("buscar token", e))?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        let secret = item
            .secret()
            .await
            .map_err(|e| store_error("leer token", e))?;
        String::from_utf8(secret.to_vec())
            .map(Some)
            .map_err(|_| correo_core::Error::Store("token almacenado no es UTF-8".into()))
    }

    async fn save_refresh_token(&self, account: AccountId, token: &str) -> Result<()> {
        let keyring = oo7::Keyring::new()
            .await
            .map_err(|e| store_error("secret service", e))?;
        let attributes = self.attributes(account);
        let attrs: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        keyring
            .create_item(
                &format!("Correo: cuenta {account}"),
                &attrs,
                token.as_bytes(),
                true,
            )
            .await
            .map_err(|e| store_error("guardar token", e))
    }

    async fn delete_refresh_token(&self, account: AccountId) -> Result<()> {
        let keyring = oo7::Keyring::new()
            .await
            .map_err(|e| store_error("secret service", e))?;
        let attributes = self.attributes(account);
        let attrs: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        keyring
            .delete(&attrs)
            .await
            .map_err(|e| store_error("borrar token", e))
    }
}

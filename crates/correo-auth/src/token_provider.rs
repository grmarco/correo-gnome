//! `TokenProvider` sobre OAuth2: cachea el access token y lo refresca con margen.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use correo_core::{AccessToken, AccountId, Error, Result, TokenProvider};

use crate::oauth::{OAuthClient, TokenSet};
use crate::store::TokenStore;

/// Un access token se considera caducado este tiempo antes de su expiración real.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct CachedToken {
    access: String,
    expires_at: Instant,
}

impl CachedToken {
    fn from_tokens(tokens: &TokenSet) -> Self {
        Self {
            access: tokens.access_token.clone(),
            expires_at: tokens.expires_at,
        }
    }

    fn is_fresh(&self) -> bool {
        Instant::now() + REFRESH_MARGIN < self.expires_at
    }
}

/// Un slot por cuenta. Su mutex async serializa los refrescos concurrentes de esa cuenta.
type Slot = Arc<tokio::sync::Mutex<Option<CachedToken>>>;

pub struct OAuthTokenProvider {
    client: OAuthClient,
    store: Arc<dyn TokenStore>,
    slots: Mutex<HashMap<AccountId, Slot>>,
}

impl fmt::Debug for OAuthTokenProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthTokenProvider")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

impl OAuthTokenProvider {
    pub fn new(client: OAuthClient, store: Arc<dyn TokenStore>) -> Self {
        Self {
            client,
            store,
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Tras una autorización completada: guarda el refresh token de la cuenta y cachea el access token.
    pub async fn save_tokens(&self, account: AccountId, tokens: &TokenSet) -> Result<()> {
        let refresh = tokens.refresh_token.as_deref().ok_or_else(|| {
            Error::Auth(
                "el proveedor no devolvió refresh token; hay que repetir la autorización".into(),
            )
        })?;
        self.store.save_refresh_token(account, refresh).await?;
        let slot = self.slot(account);
        *slot.lock().await = Some(CachedToken::from_tokens(tokens));
        Ok(())
    }

    /// Olvida una cuenta: borra el refresh token y la caché en memoria.
    pub async fn remove_account(&self, account: AccountId) -> Result<()> {
        self.store.delete_refresh_token(account).await?;
        let slot = self.slot(account);
        *slot.lock().await = None;
        Ok(())
    }

    fn slot(&self, account: AccountId) -> Slot {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        Arc::clone(slots.entry(account).or_default())
    }
}

#[async_trait::async_trait]
impl TokenProvider for OAuthTokenProvider {
    async fn access_token(&self, account: AccountId) -> Result<AccessToken> {
        let slot = self.slot(account);
        // Mantener el lock durante el refresco: las demás peticiones de la cuenta esperan y
        // reutilizan el token nuevo en lugar de refrescar de nuevo.
        let mut cached = slot.lock().await;
        if let Some(token) = cached.as_ref().filter(|t| t.is_fresh()) {
            return Ok(AccessToken(token.access.clone()));
        }

        let refresh_token = self
            .store
            .load_refresh_token(account)
            .await?
            .ok_or_else(|| {
                Error::Auth(format!(
                    "no hay credenciales guardadas para la cuenta {account}"
                ))
            })?;
        let tokens = self.client.refresh(&refresh_token).await?;
        if let Some(rotated) = tokens.refresh_token.as_deref()
            && rotated != refresh_token
        {
            self.store.save_refresh_token(account, rotated).await?;
        }

        let fresh = CachedToken::from_tokens(&tokens);
        let access = fresh.access.clone();
        *cached = Some(fresh);
        Ok(AccessToken(access))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OAuthProvider;
    use crate::store::MemoryTokenStore;
    use crate::test_support::{FakeServer, token_json};

    const ACCOUNT: AccountId = 7;

    fn provider_with(server: &FakeServer, store: Arc<MemoryTokenStore>) -> OAuthTokenProvider {
        let mut config = OAuthProvider::google("cid");
        config.token_url = server.url.clone();
        let client = OAuthClient::new(config).unwrap();
        OAuthTokenProvider::new(client, store)
    }

    fn tokens(access: &str, refresh: Option<&str>, expires_in: u64) -> TokenSet {
        TokenSet {
            access_token: access.into(),
            refresh_token: refresh.map(Into::into),
            expires_at: Instant::now() + Duration::from_secs(expires_in),
        }
    }

    #[tokio::test]
    async fn saved_tokens_are_served_from_cache_without_network() {
        let server = FakeServer::start(vec![]).await;
        let store = Arc::new(MemoryTokenStore::new());
        let provider = provider_with(&server, Arc::clone(&store));
        provider
            .save_tokens(ACCOUNT, &tokens("cached", Some("rt-1"), 3600))
            .await
            .unwrap();

        let token = provider.access_token(ACCOUNT).await.unwrap();
        assert_eq!(token.0, "cached");
        assert!(server.requests().is_empty());
        assert_eq!(
            store.load_refresh_token(ACCOUNT).await.unwrap().as_deref(),
            Some("rt-1")
        );
    }

    #[tokio::test]
    async fn fresh_cache_is_reused_and_expired_one_is_refreshed() {
        let server = FakeServer::start(vec![
            (200, token_json("access-1", None, 3600)),
            (200, token_json("access-2", None, 3600)),
        ])
        .await;
        let store = Arc::new(MemoryTokenStore::new());
        store.save_refresh_token(ACCOUNT, "rt").await.unwrap();
        let provider = provider_with(&server, store);

        assert_eq!(provider.access_token(ACCOUNT).await.unwrap().0, "access-1");
        assert_eq!(provider.access_token(ACCOUNT).await.unwrap().0, "access-1");
        assert_eq!(server.requests().len(), 1);

        // Un token que caduca dentro del margen de 60 s se considera caducado.
        provider
            .save_tokens(ACCOUNT, &tokens("almost-expired", Some("rt"), 30))
            .await
            .unwrap();
        assert_eq!(provider.access_token(ACCOUNT).await.unwrap().0, "access-2");
        assert_eq!(server.requests().len(), 2);
        assert!(server.requests()[1].contains("grant_type=refresh_token"));
        assert!(server.requests()[1].contains("refresh_token=rt"));
    }

    #[tokio::test]
    async fn rotated_refresh_token_is_persisted() {
        let server =
            FakeServer::start(vec![(200, token_json("new-access", Some("rt-2"), 3600))]).await;
        let store = Arc::new(MemoryTokenStore::new());
        store.save_refresh_token(ACCOUNT, "rt-1").await.unwrap();
        let provider = provider_with(&server, Arc::clone(&store));

        assert_eq!(
            provider.access_token(ACCOUNT).await.unwrap().0,
            "new-access"
        );
        assert_eq!(
            store.load_refresh_token(ACCOUNT).await.unwrap().as_deref(),
            Some("rt-2")
        );
    }

    #[tokio::test]
    async fn concurrent_requests_trigger_a_single_refresh() {
        // Solo hay una respuesta preparada: si se refrescara más de una vez, fallaría con 500.
        let server = FakeServer::start(vec![(200, token_json("shared", None, 3600))]).await;
        let store = Arc::new(MemoryTokenStore::new());
        store.save_refresh_token(ACCOUNT, "rt").await.unwrap();
        let provider = provider_with(&server, store);

        let results = join_five(&provider).await;
        for result in results {
            assert_eq!(result.unwrap().0, "shared");
        }
        assert_eq!(server.requests().len(), 1);
    }

    async fn join_five(provider: &OAuthTokenProvider) -> Vec<Result<AccessToken>> {
        let (a, b, c, d, e) = tokio::join!(
            provider.access_token(ACCOUNT),
            provider.access_token(ACCOUNT),
            provider.access_token(ACCOUNT),
            provider.access_token(ACCOUNT),
            provider.access_token(ACCOUNT),
        );
        vec![a, b, c, d, e]
    }

    #[tokio::test]
    async fn missing_credentials_is_an_auth_error_without_network() {
        let server = FakeServer::start(vec![]).await;
        let provider = provider_with(&server, Arc::new(MemoryTokenStore::new()));
        assert!(matches!(
            provider.access_token(ACCOUNT).await,
            Err(Error::Auth(_))
        ));
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn refresh_failure_is_propagated() {
        let server = FakeServer::start(vec![(
            400,
            "{\"error\":\"invalid_grant\",\"error_description\":\"expired\"}".into(),
        )])
        .await;
        let store = Arc::new(MemoryTokenStore::new());
        store.save_refresh_token(ACCOUNT, "rt").await.unwrap();
        let provider = provider_with(&server, store);
        match provider.access_token(ACCOUNT).await {
            Err(Error::Auth(msg)) => assert!(msg.contains("expired"), "{msg}"),
            other => panic!("resultado inesperado: {other:?}"),
        }
    }

    #[tokio::test]
    async fn save_tokens_requires_refresh_token() {
        let server = FakeServer::start(vec![]).await;
        let provider = provider_with(&server, Arc::new(MemoryTokenStore::new()));
        assert!(matches!(
            provider
                .save_tokens(ACCOUNT, &tokens("a", None, 3600))
                .await,
            Err(Error::Auth(_))
        ));
    }

    #[tokio::test]
    async fn remove_account_forgets_tokens() {
        let server = FakeServer::start(vec![]).await;
        let store = Arc::new(MemoryTokenStore::new());
        let provider = provider_with(&server, Arc::clone(&store));
        provider
            .save_tokens(ACCOUNT, &tokens("a", Some("rt"), 3600))
            .await
            .unwrap();
        provider.remove_account(ACCOUNT).await.unwrap();
        assert!(store.load_refresh_token(ACCOUNT).await.unwrap().is_none());
        assert!(matches!(
            provider.access_token(ACCOUNT).await,
            Err(Error::Auth(_))
        ));
    }
}

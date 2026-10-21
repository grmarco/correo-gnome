//! OAuth2 para correo: autorización de escritorio con PKCE y redirección loopback,
//! almacenamiento de refresh tokens y proveedor de access tokens con refresco.

mod config;
mod loopback;
mod oauth;
mod pkce;
mod store;
mod token_provider;

#[cfg(test)]
mod test_support;

pub use config::OAuthProvider;
pub use oauth::{AuthorizationRequest, OAuthClient, TokenSet};
pub use pkce::{Pkce, random_state, s256_challenge};
pub use store::{MemoryTokenStore, TokenStore};
pub use token_provider::{OAuthTokenProvider, REFRESH_MARGIN};

#[cfg(feature = "libsecret")]
pub use store::SecretServiceStore;

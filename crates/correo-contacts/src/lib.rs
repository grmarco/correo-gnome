//! Sincronización de contactos desde Google People API y Microsoft Graph.
//!
//! Cada fuente produce `Contact`s (uno por email). `sync_into_store` los
//! vuelca en la caché SQLite de `correo-store` para el autocompletado.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use correo_core::{AccountId, Contact, ContactSource, Error, Result, TokenProvider};
use correo_store::Store;
use serde::Deserialize;
use serde::de::DeserializeOwned;

const GOOGLE_BASE: &str = "https://people.googleapis.com";
const MICROSOFT_BASE: &str = "https://graph.microsoft.com";

/// Contactos de Google: `people/me/connections` y `otherContacts`.
pub struct GoogleContacts {
    pub account_id: AccountId,
    pub tokens: Arc<dyn TokenProvider>,
    /// URL base sin barra final; por defecto `https://people.googleapis.com`.
    pub base_url: String,
}

/// Contactos de Microsoft (Graph `me/contacts`).
pub struct MicrosoftContacts {
    pub account_id: AccountId,
    pub tokens: Arc<dyn TokenProvider>,
    /// URL base sin barra final; por defecto `https://graph.microsoft.com`.
    pub base_url: String,
}

impl GoogleContacts {
    pub fn new(account_id: AccountId, tokens: Arc<dyn TokenProvider>) -> Self {
        Self {
            account_id,
            tokens,
            base_url: GOOGLE_BASE.to_owned(),
        }
    }
}

impl MicrosoftContacts {
    pub fn new(account_id: AccountId, tokens: Arc<dyn TokenProvider>) -> Self {
        Self {
            account_id,
            tokens,
            base_url: MICROSOFT_BASE.to_owned(),
        }
    }
}

#[async_trait::async_trait]
impl ContactSource for GoogleContacts {
    async fn sync(&self) -> Result<Vec<Contact>> {
        let token = self.tokens.access_token(self.account_id).await?;
        let http = http_client()?;
        let base = self.base_url.trim_end_matches('/');
        let mut out = Vec::new();

        let sources = [
            (
                format!("{base}/v1/people/me/connections"),
                vec![
                    ("personFields", "names,emailAddresses"),
                    ("pageSize", "1000"),
                ],
            ),
            (
                format!("{base}/v1/otherContacts"),
                vec![("readMask", "names,emailAddresses"), ("pageSize", "1000")],
            ),
        ];

        for (url, params) in sources {
            let mut page_token: Option<String> = None;
            loop {
                let mut query = params.clone();
                if let Some(t) = page_token.as_deref() {
                    query.push(("pageToken", t));
                }
                let mut page: GooglePage = get_json(&http, &token.0, &url, &query).await?;
                let next = next_token(page_token.as_deref(), page.next_page_token.take());
                out.extend(page.into_contacts());
                match next {
                    Some(t) => page_token = Some(t),
                    None => break,
                }
            }
        }

        Ok(dedupe(out))
    }
}

#[async_trait::async_trait]
impl ContactSource for MicrosoftContacts {
    async fn sync(&self) -> Result<Vec<Contact>> {
        let token = self.tokens.access_token(self.account_id).await?;
        let http = http_client()?;
        let base = self.base_url.trim_end_matches('/');
        let mut out = Vec::new();

        let first_url = format!("{base}/v1.0/me/contacts");
        let first_query = [("$select", "displayName,emailAddresses"), ("$top", "999")];
        let mut page: GraphPage = get_json(&http, &token.0, &first_url, &first_query).await?;
        loop {
            let next = page.next_link.take();
            out.extend(page.into_contacts());
            let Some(next) = next else {
                break;
            };
            // El enlace de paginación es absoluto; nunca enviamos el token a otro host.
            if !next.starts_with(&format!("{base}/")) {
                return Err(Error::Protocol(
                    "enlace de paginación fuera del servidor configurado".into(),
                ));
            }
            page = get_json(&http, &token.0, &next, &[]).await?;
        }

        Ok(dedupe(out))
    }
}

/// Sincroniza una fuente y guarda los contactos en la caché. Devuelve cuántos se guardaron.
pub async fn sync_into_store(source: &dyn ContactSource, store: &Mutex<Store>) -> Result<usize> {
    let contacts = source.sync().await?;
    let count = contacts.len();
    let guard = store
        .lock()
        .map_err(|_| Error::Store("bloqueo de la base de datos envenenado".into()))?;
    guard.upsert_contacts(&contacts)?;
    Ok(count)
}

// ----- Google --------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GooglePage {
    #[serde(default)]
    connections: Vec<GooglePerson>,
    #[serde(default)]
    other_contacts: Vec<GooglePerson>,
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GooglePerson {
    #[serde(default)]
    names: Vec<GoogleName>,
    #[serde(default)]
    email_addresses: Vec<GoogleEmail>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleName {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleEmail {
    value: Option<String>,
}

impl GooglePage {
    fn into_contacts(self) -> Vec<Contact> {
        self.connections
            .into_iter()
            .chain(self.other_contacts)
            .flat_map(|p| {
                let name = p.names.into_iter().find_map(|n| clean(n.display_name));
                let emails = p.email_addresses.into_iter().filter_map(|e| e.value);
                expand(name, emails, "google")
            })
            .collect()
    }
}

// ----- Microsoft Graph -----------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphPage {
    #[serde(default)]
    value: Vec<GraphContact>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphContact {
    display_name: Option<String>,
    #[serde(default)]
    email_addresses: Vec<GraphEmail>,
}

#[derive(Debug, Deserialize)]
struct GraphEmail {
    address: Option<String>,
}

impl GraphPage {
    fn into_contacts(self) -> Vec<Contact> {
        self.value
            .into_iter()
            .flat_map(|c| {
                let name = clean(c.display_name);
                let emails = c.email_addresses.into_iter().filter_map(|e| e.address);
                expand(name, emails, "microsoft")
            })
            .collect()
    }
}

// ----- Utilidades ----------------------------------------------------------

/// Un `Contact` por email no vacío, todos con el mismo nombre.
fn expand(
    name: Option<String>,
    emails: impl Iterator<Item = String>,
    source: &str,
) -> Vec<Contact> {
    emails
        .filter_map(|e| {
            let email = e.trim().to_owned();
            (!email.is_empty()).then(|| Contact {
                name: name.clone(),
                email,
                source: source.to_owned(),
            })
        })
        .collect()
}

/// Quita espacios y convierte cadenas vacías en `None`.
fn clean(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Elimina duplicados por email (sin distinguir mayúsculas). Conserva el primero
/// y rellena su nombre si estaba vacío.
fn dedupe(contacts: Vec<Contact>) -> Vec<Contact> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<Contact> = Vec::new();
    for c in contacts {
        let key = c.email.to_lowercase();
        match index.get(&key) {
            Some(&i) => {
                if out[i].name.is_none() {
                    out[i].name = c.name;
                }
            }
            None => {
                index.insert(key, out.len());
                out.push(c);
            }
        }
    }
    out
}

/// Siguiente `pageToken`; corta el bucle si el servidor repite el anterior.
fn next_token(current: Option<&str>, next: Option<String>) -> Option<String> {
    next.filter(|t| !t.is_empty() && Some(t.as_str()) != current)
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| Error::Network(format!("cliente HTTP: {e}")))
}

async fn get_json<T: DeserializeOwned>(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    query: &[(&str, &str)],
) -> Result<T> {
    let full = reqwest::Url::parse_with_params(url, query)
        .map_err(|e| Error::Protocol(format!("URL de contactos no válida: {e}")))?;
    let response = http
        .get(full)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| Error::Network(format!("petición de contactos: {e}")))?;

    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Error::Auth(
            "el proveedor rechazó el token de acceso".into(),
        ));
    }
    if !status.is_success() {
        return Err(Error::Network(format!(
            "el proveedor respondió {status} al pedir contactos"
        )));
    }

    let body = response
        .bytes()
        .await
        .map_err(|e| Error::Network(format!("respuesta de contactos: {e}")))?;
    serde_json::from_slice(&body)
        .map_err(|e| Error::Protocol(format!("respuesta de contactos no válida: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const GOOGLE_CONNECTIONS: &str = r#"{
        "connections": [
            {
                "resourceName": "people/c1",
                "names": [{"displayName": "Ana Pérez", "givenName": "Ana"}],
                "emailAddresses": [
                    {"value": "ana@example.com", "type": "home"},
                    {"value": "ana.perez@work.example", "type": "work"}
                ]
            },
            {
                "resourceName": "people/c2",
                "emailAddresses": [{"value": "sin-nombre@example.com"}]
            },
            {
                "resourceName": "people/c3",
                "names": [{"displayName": "Solo telefono"}]
            }
        ],
        "nextPageToken": "page-2",
        "totalItems": 4
    }"#;

    const GOOGLE_CONNECTIONS_PAGE_2: &str = r#"{
        "connections": [
            {"names": [{"displayName": "Luis"}], "emailAddresses": [{"value": "luis@example.com"}]}
        ]
    }"#;

    const GOOGLE_OTHER: &str = r#"{
        "otherContacts": [
            {"names": [{"displayName": "Marta"}], "emailAddresses": [{"value": "marta@example.com"}]}
        ],
        "totalSize": 1
    }"#;

    const GRAPH_PAGE_1: &str = r#"{
        "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#users('x')/contacts",
        "value": [
            {
                "id": "AAMk1",
                "displayName": "Carlos Gómez",
                "emailAddresses": [
                    {"name": "Carlos", "address": "carlos@example.com"},
                    {"name": "Carlos work", "address": "cgomez@work.example"}
                ]
            },
            {"id": "AAMk2", "displayName": null, "emailAddresses": [{"name": "", "address": "anon@example.com"}]},
            {"id": "AAMk3", "displayName": "Sin email", "emailAddresses": []}
        ],
        "@odata.nextLink": "__BASE__/v1.0/me/contacts?$skiptoken=abc"
    }"#;

    const GRAPH_PAGE_2: &str = r#"{
        "value": [
            {"id": "AAMk4", "displayName": "Dolores", "emailAddresses": [{"address": "dolores@example.com"}]}
        ]
    }"#;

    struct StaticToken(&'static str);

    #[async_trait::async_trait]
    impl TokenProvider for StaticToken {
        async fn access_token(&self, _account: AccountId) -> Result<correo_core::AccessToken> {
            Ok(correo_core::AccessToken(self.0.to_owned()))
        }
    }

    fn good_token() -> Arc<dyn TokenProvider> {
        Arc::new(StaticToken("test-token"))
    }

    /// Servidor HTTP falso. El handler recibe (base, path+query) y devuelve (estado, cuerpo).
    /// Sin `Authorization: Bearer test-token` responde 401.
    async fn fake_server<F>(handler: F) -> String
    where
        F: Fn(&str, &str) -> (u16, String) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handler = Arc::new(handler);
        let server_base = base.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let handler = handler.clone();
                let base = server_base.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let request = String::from_utf8_lossy(&buf).to_string();
                    let target = request
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_owned();
                    // Se decodifica la query para que los tests comparen valores legibles.
                    let target = {
                        let url = reqwest::Url::parse(&format!("{base}{target}")).unwrap();
                        let query: Vec<String> =
                            url.query_pairs().map(|(k, v)| format!("{k}={v}")).collect();
                        if query.is_empty() {
                            url.path().to_owned()
                        } else {
                            format!("{}?{}", url.path(), query.join("&"))
                        }
                    };
                    let authorized = request
                        .to_lowercase()
                        .contains("authorization: bearer test-token");
                    let (status, body) = if authorized {
                        handler(&base, &target)
                    } else {
                        (401, r#"{"error":"unauthorized"}"#.to_owned())
                    };
                    let reason = match status {
                        200 => "OK",
                        401 => "Unauthorized",
                        500 => "Internal Server Error",
                        _ => "Error",
                    };
                    let response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(response.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        base
    }

    fn find<'a>(contacts: &'a [Contact], email: &str) -> Option<&'a Contact> {
        contacts.iter().find(|c| c.email == email)
    }

    #[test]
    fn parses_google_connections_with_multiple_emails_and_gaps() {
        let page: GooglePage = serde_json::from_str(GOOGLE_CONNECTIONS).unwrap();
        assert_eq!(page.next_page_token.as_deref(), Some("page-2"));
        let contacts = page.into_contacts();
        assert_eq!(contacts.len(), 3);

        let ana: Vec<&Contact> = contacts.iter().filter(|c| c.name.is_some()).collect();
        assert_eq!(ana.len(), 2);
        assert_eq!(ana[0].name.as_deref(), Some("Ana Pérez"));
        assert_eq!(ana[0].email, "ana@example.com");
        assert_eq!(ana[0].source, "google");
        assert_eq!(ana[1].email, "ana.perez@work.example");
        assert_eq!(ana[1].name.as_deref(), Some("Ana Pérez"));

        let anon = find(&contacts, "sin-nombre@example.com").unwrap();
        assert!(anon.name.is_none());
    }

    #[test]
    fn parses_google_other_contacts() {
        let page: GooglePage = serde_json::from_str(GOOGLE_OTHER).unwrap();
        assert!(page.next_page_token.is_none());
        let contacts = page.into_contacts();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].name.as_deref(), Some("Marta"));
        assert_eq!(contacts[0].source, "google");
    }

    #[test]
    fn parses_empty_google_page() {
        let page: GooglePage = serde_json::from_str(r#"{"totalItems": 0}"#).unwrap();
        assert!(page.into_contacts().is_empty());
    }

    #[test]
    fn parses_graph_page_with_multiple_emails_and_gaps() {
        let page: GraphPage = serde_json::from_str(&GRAPH_PAGE_1.replace("__BASE__", "")).unwrap();
        assert_eq!(
            page.next_link.as_deref(),
            Some("/v1.0/me/contacts?$skiptoken=abc")
        );
        let contacts = page.into_contacts();
        assert_eq!(contacts.len(), 3);
        assert_eq!(contacts[0].name.as_deref(), Some("Carlos Gómez"));
        assert_eq!(contacts[0].email, "carlos@example.com");
        assert_eq!(contacts[0].source, "microsoft");
        assert_eq!(contacts[1].email, "cgomez@work.example");
        assert_eq!(contacts[1].name.as_deref(), Some("Carlos Gómez"));
        // Sin displayName y con un nombre vacío en la dirección: sin nombre, sigue con email.
        assert!(contacts[2].name.is_none());
        assert_eq!(contacts[2].email, "anon@example.com");
    }

    #[test]
    fn dedupes_by_email_case_insensitively_and_fills_name() {
        let contacts = dedupe(vec![
            Contact {
                name: None,
                email: "Ana@Example.com".into(),
                source: "google".into(),
            },
            Contact {
                name: Some("Ana".into()),
                email: "ana@example.com".into(),
                source: "google".into(),
            },
        ]);
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].name.as_deref(), Some("Ana"));
    }

    #[test]
    fn repeated_page_token_stops_pagination() {
        assert_eq!(next_token(None, Some("a".into())).as_deref(), Some("a"));
        assert_eq!(next_token(Some("a"), Some("a".into())), None);
        assert_eq!(next_token(Some("a"), Some(String::new())), None);
        assert_eq!(next_token(Some("a"), None), None);
    }

    #[tokio::test]
    async fn google_sync_follows_pagination_and_other_contacts() {
        let base = fake_server(|_, target| {
            if target.starts_with("/v1/people/me/connections") {
                if target.contains("pageToken=page-2") {
                    (200, GOOGLE_CONNECTIONS_PAGE_2.to_owned())
                } else {
                    assert!(target.contains("personFields=names,emailAddresses"));
                    assert!(target.contains("pageSize=1000"));
                    (200, GOOGLE_CONNECTIONS.to_owned())
                }
            } else if target.starts_with("/v1/otherContacts") {
                assert!(target.contains("readMask=names,emailAddresses"));
                (200, GOOGLE_OTHER.to_owned())
            } else {
                (500, "{}".to_owned())
            }
        })
        .await;

        let source = GoogleContacts {
            base_url: base,
            ..GoogleContacts::new(1, good_token())
        };
        let contacts = source.sync().await.unwrap();
        // Primera página: 2 emails de Ana y 1 sin nombre; segunda página: Luis; otherContacts: Marta.
        assert_eq!(contacts.len(), 5);
        assert!(find(&contacts, "luis@example.com").is_some());
        assert!(find(&contacts, "marta@example.com").is_some());
        assert!(contacts.iter().all(|c| c.source == "google"));
    }

    #[tokio::test]
    async fn microsoft_sync_follows_odata_next_link() {
        let base = fake_server(|base, target| {
            if target.starts_with("/v1.0/me/contacts?$skiptoken=abc") {
                (200, GRAPH_PAGE_2.to_owned())
            } else if target.starts_with("/v1.0/me/contacts") {
                assert!(target.contains("$select=displayName,emailAddresses"));
                assert!(target.contains("$top=999"));
                (200, GRAPH_PAGE_1.replace("__BASE__", base))
            } else {
                (500, "{}".to_owned())
            }
        })
        .await;

        let source = MicrosoftContacts {
            base_url: base,
            ..MicrosoftContacts::new(1, good_token())
        };
        let contacts = source.sync().await.unwrap();
        assert_eq!(contacts.len(), 4);
        assert!(find(&contacts, "dolores@example.com").is_some());
        assert!(contacts.iter().all(|c| c.source == "microsoft"));
    }

    #[tokio::test]
    async fn unauthorized_maps_to_auth_error() {
        let base = fake_server(|_, _| (200, "{}".to_owned())).await;
        let bad = Arc::new(StaticToken("wrong-token"));

        let google = GoogleContacts {
            base_url: base.clone(),
            ..GoogleContacts::new(1, bad.clone())
        };
        assert!(matches!(google.sync().await, Err(Error::Auth(_))));

        let microsoft = MicrosoftContacts {
            base_url: base,
            ..MicrosoftContacts::new(1, bad)
        };
        assert!(matches!(microsoft.sync().await, Err(Error::Auth(_))));
    }

    #[tokio::test]
    async fn server_errors_map_to_network_error() {
        let base = fake_server(|_, _| (500, "{}".to_owned())).await;

        let google = GoogleContacts {
            base_url: base.clone(),
            ..GoogleContacts::new(1, good_token())
        };
        assert!(matches!(google.sync().await, Err(Error::Network(_))));

        let microsoft = MicrosoftContacts {
            base_url: base,
            ..MicrosoftContacts::new(1, good_token())
        };
        assert!(matches!(microsoft.sync().await, Err(Error::Network(_))));
    }

    #[tokio::test]
    async fn malformed_body_maps_to_protocol_error() {
        let base = fake_server(|_, _| (200, "<html>no json</html>".to_owned())).await;
        let source = MicrosoftContacts {
            base_url: base,
            ..MicrosoftContacts::new(1, good_token())
        };
        assert!(matches!(source.sync().await, Err(Error::Protocol(_))));
    }

    #[tokio::test]
    async fn next_link_outside_base_is_rejected_without_following_it() {
        let base = fake_server(|_, _| {
            (
                200,
                r#"{"value": [], "@odata.nextLink": "https://evil.example/steal"}"#.to_owned(),
            )
        })
        .await;
        let source = MicrosoftContacts {
            base_url: base,
            ..MicrosoftContacts::new(1, good_token())
        };
        assert!(matches!(source.sync().await, Err(Error::Protocol(_))));
    }

    struct Fixed(Vec<Contact>);

    #[async_trait::async_trait]
    impl ContactSource for Fixed {
        async fn sync(&self) -> Result<Vec<Contact>> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn sync_into_store_upserts_contacts() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let source = Fixed(vec![
            Contact {
                name: Some("Ana Pérez".into()),
                email: "ana@example.com".into(),
                source: "google".into(),
            },
            Contact {
                name: None,
                email: "bob@example.com".into(),
                source: "microsoft".into(),
            },
        ]);
        assert_eq!(sync_into_store(&source, &store).await.unwrap(), 2);

        let found = store.lock().unwrap().search_contacts("ana", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name.as_deref(), Some("Ana Pérez"));
    }
}

//! Diálogo de alta de cuenta: elección de proveedor, autorización OAuth en el navegador,
//! descubrimiento de email y nombre, y guardado de cuenta y tokens.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adw::prelude::*;
use correo_auth::{OAuthClient, OAuthProvider, OAuthTokenProvider, TokenSet};
use correo_core::{Account, Error, Provider, Result};
use correo_store::Store;
use gtk::glib;
use serde::Deserialize;

use super::{app_config, oauth_config, token_store};
use crate::services::Services;

const GOOGLE_USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const GRAPH_ME_URL: &str =
    "https://graph.microsoft.com/v1.0/me?$select=mail,userPrincipalName,displayName";
const PROFILE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
enum Choice {
    Gmail,
    Outlook,
    Microsoft365,
}

impl Choice {
    const ALL: [Choice; 3] = [Choice::Gmail, Choice::Outlook, Choice::Microsoft365];

    fn provider(self) -> Provider {
        match self {
            Choice::Gmail => Provider::Gmail,
            Choice::Outlook => Provider::Outlook,
            Choice::Microsoft365 => Provider::Microsoft365,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Choice::Gmail => "Gmail",
            Choice::Outlook => "Outlook.com",
            Choice::Microsoft365 => "Microsoft 365",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Choice::Gmail => "Cuentas de Google y Google Workspace",
            Choice::Outlook => "Cuentas personales @outlook.com, @hotmail.com, @live.com",
            Choice::Microsoft365 => "Cuentas de trabajo o centro educativo",
        }
    }
}

/// Estado compartido por los callbacks del diálogo.
struct Flow {
    services: Rc<Services>,
    dialog: adw::Dialog,
    parent: gtk::Window,
    list: gtk::ListBox,
    status: gtk::Label,
    on_done: Rc<dyn Fn(Account)>,
}

/// Muestra el diálogo de alta. `on_done` recibe la cuenta ya guardada en la BD.
pub fn add_account_dialog(
    parent: &impl IsA<gtk::Window>,
    services: Rc<Services>,
    on_done: impl Fn(Account) + 'static,
) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    for choice in Choice::ALL {
        let row = adw::ActionRow::builder()
            .title(choice.title())
            .subtitle(choice.subtitle())
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        list.append(&row);
    }

    let status = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();

    let docs = gtk::Button::builder()
        .label("Guía para registrar la aplicación (docs/OAUTH.md)")
        .css_classes(["flat"])
        .halign(gtk::Align::Start)
        .build();

    let intro = gtk::Label::builder()
        .label(
            "Elige el tipo de cuenta. Se abrirá el navegador para que inicies sesión; \
             correo no ve tu contraseña.",
        )
        .wrap(true)
        .xalign(0.0)
        .build();

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(16)
        .margin_end(16)
        .build();
    body.append(&intro);
    body.append(&list);
    body.append(&status);
    body.append(&docs);

    let header = adw::HeaderBar::new();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&body));

    let dialog = adw::Dialog::builder()
        .title("Añadir cuenta")
        .content_width(420)
        .child(&view)
        .build();

    let flow = Rc::new(Flow {
        services,
        dialog: dialog.clone(),
        parent: parent.as_ref().clone(),
        list: list.clone(),
        status: status.clone(),
        on_done: Rc::new(on_done),
    });

    {
        let flow = Rc::clone(&flow);
        list.connect_row_activated(move |_, row| {
            let index = row.index();
            if let Some(choice) = usize::try_from(index).ok().and_then(|i| Choice::ALL.get(i)) {
                start(Rc::clone(&flow), *choice);
            }
        });
    }
    docs.connect_clicked(move |_| open_docs());

    let window: &gtk::Window = parent.as_ref();
    dialog.present(Some(window));
}

/// Ruta de la guía, si la compilación conserva la ubicación del repositorio.
fn docs_uri() -> Option<String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/OAUTH.md");
    glib::filename_to_uri(path, None)
        .ok()
        .map(|u| u.to_string())
}

fn open_docs() {
    if let Some(uri) = docs_uri() {
        gtk::UriLauncher::new(&uri).launch(None::<&gtk::Window>, None::<&gio::Cancellable>, |_| {});
    }
}

fn start(flow: Rc<Flow>, choice: Choice) {
    let provider = choice.provider();
    let Some(oauth) = oauth_config(provider) else {
        flow.show_missing_client_id(choice);
        return;
    };
    flow.set_busy(true, "Preparando la autorización…");

    glib::MainContext::default().spawn_local(async move {
        let services = Rc::clone(&flow.services);
        let oauth_for_begin = oauth.clone();
        let begun = services
            .spawn(async move {
                OAuthClient::new(oauth_for_begin)?
                    .begin_authorization()
                    .await
            })
            .await;
        let request = match begun {
            Ok(request) => request,
            Err(e) => return flow.fail(e),
        };

        flow.set_busy(true, "Abriendo el navegador para iniciar sesión…");
        let url = request.url().to_owned();
        let launched = gtk::UriLauncher::new(&url)
            .launch_future(Some(&flow.parent))
            .await;
        if let Err(e) = launched {
            return flow.fail(Error::Auth(format!("no se pudo abrir el navegador: {e}")));
        }
        flow.set_busy(
            true,
            "Esperando a que completes el inicio de sesión en el navegador…",
        );

        let authorized = services
            .spawn(async move {
                let tokens = request.finish().await?;
                let profile = fetch_profile(provider, &tokens.access_token).await?;
                Ok::<_, Error>((tokens, profile))
            })
            .await;
        let (tokens, profile) = match authorized {
            Ok(v) => v,
            Err(e) => return flow.fail(e),
        };

        flow.set_busy(true, "Guardando la cuenta…");
        let persisted = services
            .spawn(persist_account(
                Arc::clone(&services.store),
                provider,
                profile,
                tokens,
                oauth,
            ))
            .await;
        match persisted {
            Ok(account) => {
                flow.dialog.close();
                (flow.on_done)(account);
            }
            Err(e) => flow.fail(e),
        }
    });
}

impl Flow {
    fn set_busy(&self, busy: bool, message: &str) {
        self.list.set_sensitive(!busy);
        self.status.remove_css_class("error");
        self.status.set_label(message);
        self.status.set_visible(true);
    }

    fn fail(&self, error: Error) {
        self.list.set_sensitive(true);
        self.status.add_css_class("error");
        self.status
            .set_label(&format!("No se pudo añadir la cuenta: {error}"));
        self.status.set_visible(true);
    }

    fn show_missing_client_id(&self, choice: Choice) {
        let (name, var, file) = match choice.provider() {
            Provider::Gmail => ("Google", "CORREO_GOOGLE_CLIENT_ID", "[google] client_id"),
            _ => (
                "Microsoft",
                "CORREO_MICROSOFT_CLIENT_ID",
                "[microsoft] client_id",
            ),
        };
        self.status.remove_css_class("error");
        self.status.set_label(&format!(
            "Falta el client ID de {name}. Regístrala como indica docs/OAUTH.md y añade el \
             identificador en ~/.config/correo/config.toml ({file}) o en la variable {var}. \
             Config actual: {}",
            crate::setup::config::config_path().display()
        ));
        self.status.set_visible(true);
    }
}

/// Perfil mínimo que necesita la app de la cuenta.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Profile {
    email: String,
    name: Option<String>,
}

#[derive(Deserialize)]
struct GoogleUserinfo {
    email: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct GraphMe {
    mail: Option<String>,
    #[serde(rename = "userPrincipalName")]
    user_principal_name: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

/// Email y nombre de la cuenta autorizada: userinfo de Google o `/me` de Graph.
async fn fetch_profile(provider: Provider, access_token: &str) -> Result<Profile> {
    let http = reqwest::Client::builder()
        .timeout(PROFILE_TIMEOUT)
        .build()
        .map_err(|e| Error::Network(format!("cliente HTTP: {e}")))?;
    let url = match provider {
        Provider::Gmail => GOOGLE_USERINFO_URL,
        _ => GRAPH_ME_URL,
    };
    let response = http
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| Error::Network(format!("perfil de la cuenta: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Auth(format!(
            "no se pudo leer el perfil de la cuenta ({status})"
        )));
    }
    let body = response
        .text()
        .await
        .map_err(|e| Error::Network(format!("perfil de la cuenta: {e}")))?;

    match provider {
        Provider::Gmail => {
            let info: GoogleUserinfo = serde_json::from_str(&body)
                .map_err(|_| Error::Protocol("perfil de Google no válido".into()))?;
            let email = info
                .email
                .ok_or_else(|| Error::Protocol("Google no devolvió el correo".into()))?;
            Ok(Profile {
                email,
                name: info.name,
            })
        }
        _ => {
            let me: GraphMe = serde_json::from_str(&body)
                .map_err(|_| Error::Protocol("perfil de Microsoft Graph no válido".into()))?;
            let email = me
                .mail
                .or(me.user_principal_name)
                .ok_or_else(|| Error::Protocol("Microsoft Graph no devolvió el correo".into()))?;
            Ok(Profile {
                email,
                name: me.display_name,
            })
        }
    }
}

/// Crea la cuenta en la BD y guarda su refresh token. Si el guardado de tokens falla,
/// la cuenta se borra para no dejar una cuenta sin credenciales.
async fn persist_account(
    store: Arc<Mutex<Store>>,
    provider: Provider,
    profile: Profile,
    tokens: TokenSet,
    oauth: OAuthProvider,
) -> Result<Account> {
    let account_id = {
        let guard = store.lock().unwrap_or_else(|p| p.into_inner());
        let duplicated = guard
            .list_accounts()?
            .iter()
            .any(|a| a.email.eq_ignore_ascii_case(&profile.email));
        if duplicated {
            return Err(Error::Store(format!(
                "ya existe una cuenta para {}",
                profile.email
            )));
        }
        guard.create_account(&Account {
            id: 0,
            provider,
            display_name: profile
                .name
                .clone()
                .unwrap_or_else(|| profile.email.clone()),
            email: profile.email,
            sync_days: app_config().sync_days,
        })?
    };

    let tokens_store = OAuthTokenProvider::new(OAuthClient::new(oauth)?, token_store()?);
    if let Err(e) = tokens_store.save_tokens(account_id, &tokens).await {
        let guard = store.lock().unwrap_or_else(|p| p.into_inner());
        let _ = guard.delete_account(account_id);
        return Err(e);
    }

    let guard = store.lock().unwrap_or_else(|p| p.into_inner());
    guard.get_account(account_id)
}

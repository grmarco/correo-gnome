//! Vista «Ver original» con WebKitGTK (T10b, feature `webkit`).
//! Se crea solo al pulsar el botón y se destruye al volver al texto o al cambiar de mensaje.
//! Restricciones: JavaScript, plugins, media, WebGL/WebAudio, WebRTC y acceso a ficheros
//! desactivados; sesión de red efímera (sin caché ni cookies); todo recurso de red bloqueado
//! por un filtro de contenido (salvo imágenes https si el usuario lo pide) y toda navegación
//! de enlaces delegada al sistema con `gtk::UriLauncher`.

use gtk::{gio, glib};
use webkit6::prelude::*;

use super::reader_web_policy::{
    content_filter_json, navigation_decision, response_allowed, NavDecision,
};

/// Directorio (en tmpfs de `$XDG_RUNTIME_DIR`) donde WebKit compila los filtros de contenido.
/// Solo contiene reglas estáticas, nunca contenido del correo.
const FILTER_STORE_DIR: &str = "correo-webkit-filtros";

pub struct OriginalView {
    view: webkit6::WebView,
    ucm: webkit6::UserContentManager,
    store: webkit6::UserContentFilterStore,
    /// Se mantiene viva mientras viva la vista (sesión efímera de la que depende).
    _session: webkit6::NetworkSession,
    html: String,
}

impl OriginalView {
    /// Crea el WebView sin cargar nada. Falla si no se puede preparar el filtro de contenido
    /// (en ese caso no se muestra el original: fallo cerrado).
    pub fn new(html: String) -> Result<Self, String> {
        let dir = glib::user_runtime_dir().join(FILTER_STORE_DIR);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("no se pudo preparar el filtro de contenido: {e}"))?;
        let dir = dir
            .to_str()
            .ok_or_else(|| "ruta del filtro no UTF-8".to_string())?;
        let store = webkit6::UserContentFilterStore::new(dir);

        let session = webkit6::NetworkSession::new_ephemeral();
        let ucm = webkit6::UserContentManager::new();
        let view = webkit6::WebView::builder()
            .settings(&settings())
            .network_session(&session)
            .user_content_manager(&ucm)
            .build();
        view.connect_decide_policy(decide);

        Ok(Self {
            view,
            ucm,
            store,
            _session: session,
            html,
        })
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.view.upcast_ref()
    }

    /// Carga el HTML original con el filtro estricto (sin imágenes remotas).
    pub fn load(&self) {
        self.install_filter(false);
    }

    /// Permite solo imágenes https y vuelve a cargar el HTML original.
    pub fn allow_images(&self) {
        self.install_filter(true);
    }

    /// Compila el filtro y, cuando está listo, lo sustituye y carga el HTML. Nunca se carga
    /// el documento sin filtro: si la compilación falla, no se muestra nada.
    fn install_filter(&self, remote_images: bool) {
        let id = if remote_images {
            "correo-imagenes"
        } else {
            "correo-estricto"
        };
        let source = glib::Bytes::from_owned(content_filter_json(remote_images).into_bytes());
        let view = self.view.downgrade();
        let ucm = self.ucm.clone();
        let html = self.html.clone();
        self.store.save(
            id,
            &source,
            None::<&gio::Cancellable>,
            move |res| match res {
                Ok(filter) => {
                    ucm.remove_all_filters();
                    ucm.add_filter(&filter);
                    if let Some(view) = view.upgrade() {
                        view.load_html(&html, Some("about:blank"));
                    }
                }
                Err(e) => eprintln!("filtro de contenido no disponible: {e}"),
            },
        );
    }
}

/// Ajustes del WebView: nada activo salvo el renderizado HTML estático.
fn settings() -> webkit6::Settings {
    let s = webkit6::Settings::new();
    s.set_enable_javascript(false);
    s.set_enable_media(false);
    s.set_enable_mediasource(false);
    s.set_enable_media_stream(false);
    s.set_enable_encrypted_media(false);
    s.set_enable_webgl(false);
    s.set_enable_webaudio(false);
    s.set_enable_webrtc(false);
    s.set_allow_file_access_from_file_urls(false);
    s.set_allow_universal_access_from_file_urls(false);
    s.set_allow_modal_dialogs(false);
    s.set_enable_html5_local_storage(false);
    s.set_enable_html5_database(false);
    s.set_enable_offline_web_application_cache(false);
    s.set_enable_page_cache(false);
    s.set_enable_developer_extras(false);
    s.set_enable_dns_prefetching(false);
    s.set_enable_hyperlink_auditing(false);
    s
}

/// Política de navegación y respuestas (los recursos secundarios los bloquea el filtro).
fn decide(
    view: &webkit6::WebView,
    decision: &webkit6::PolicyDecision,
    kind: webkit6::PolicyDecisionType,
) -> bool {
    use webkit6::PolicyDecisionType as T;
    let action = match kind {
        // Las nuevas ventanas (target=_blank) llegan como decisiones de navegación.
        T::NavigationAction | T::NewWindowAction => decision
            .downcast_ref::<webkit6::NavigationPolicyDecision>()
            .and_then(|d| d.navigation_action()),
        T::Response => {
            let uri = decision
                .downcast_ref::<webkit6::ResponsePolicyDecision>()
                .and_then(|d| d.request())
                .and_then(|r| r.uri())
                .unwrap_or_default();
            if response_allowed(&uri) {
                decision.use_();
            } else {
                decision.ignore();
            }
            return true;
        }
        _ => return false,
    };

    let Some(action) = action else {
        decision.ignore();
        return true;
    };
    let uri = action
        .request()
        .and_then(|r| r.uri())
        .map(|u| u.to_string())
        .unwrap_or_default();
    let clicked = action.navigation_type() == webkit6::NavigationType::LinkClicked;
    match navigation_decision(&uri, clicked) {
        NavDecision::Allow => decision.use_(),
        NavDecision::OpenExternal => {
            decision.ignore();
            open_external(view, &uri);
        }
        NavDecision::Block => decision.ignore(),
    }
    true
}

fn open_external(view: &webkit6::WebView, uri: &str) {
    let parent = view.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    gtk::UriLauncher::new(uri).launch(parent.as_ref(), None::<&gio::Cancellable>, |res| {
        if let Err(e) = res {
            eprintln!("no se pudo abrir el enlace: {e}");
        }
    });
}

//! Vista completa de mensajes HTML con WebKitGTK (feature `webkit`).
//!
//! Un único `HtmlView` se reutiliza mientras el usuario navega entre mensajes con HTML y lo
//! destruye `reader_web_ui` tras un rato sin HTML visible. Restricciones (todo lo de
//! `reader_web_policy.rs` se hace cumplir aquí): JavaScript, plugins, media, WebGL/WebAudio,
//! WebRTC, almacenamiento y caché desactivados; sesión de red efímera; todo recurso de red
//! bloqueado por un filtro de contenido (salvo imágenes https si el usuario lo pide); toda
//! navegación de enlaces delegada al lector, que confirma si el texto aparenta otro dominio;
//! sin ventanas emergentes ni menú contextual.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::{gio, glib};
use webkit6::prelude::*;

use super::reader_web_dark::{card_color, user_stylesheet, Look};
use super::reader_web_policy::{
    content_filter_json, navigation_decision, response_allowed, NavDecision,
};
use super::reader_web_sanitize::build_document;

/// Documento saneado listo para mostrar.
pub struct WebDoc {
    /// Fragmento HTML saneado.
    pub clean: String,
    /// Estilo (depurado) del `<body>` original.
    pub body_style: String,
    /// Ancho fijo de tabla más grande (px), 0 si no hay: para encoger en ventanas estrechas.
    pub width_hint: u32,
    /// Aspecto (claro, oscuro nativo, oscurización automática…).
    pub look: Look,
}

/// Directorio (en tmpfs de `$XDG_RUNTIME_DIR`) donde WebKit compila los filtros de contenido.
/// Solo contiene reglas estáticas, nunca contenido del correo.
const FILTER_STORE_DIR: &str = "correo-webkit-filtros";

/// Avisos del WebView al lector.
pub enum WebEvent {
    /// El documento con este token terminó de cargarse y ya se puede mostrar.
    Loaded(u64),
    /// No se pudo preparar o cargar la vista (filtro, proceso web caído…): usar la ligera.
    Failed(String),
    /// El usuario pulsó un enlace; `label` es el texto visible bajo el puntero, si se conoce.
    OpenLink { uri: String, label: Option<String> },
}

type Events = Rc<dyn Fn(WebEvent)>;

/// Estado compartido con los manejadores de señales del WebView.
struct Shared {
    events: Events,
    /// Token del documento actual; los resultados asíncronos de otros se descartan.
    token: Cell<u64>,
    /// Documento saneado del mensaje actual.
    doc: RefCell<WebDoc>,
    /// Último enlace bajo el puntero: (uri, texto visible).
    hover: RefCell<Option<(String, String)>>,
    /// Último token cuya carga ha terminado y último del que ya se avisó con `Loaded`.
    finished: Cell<u64>,
    /// Filtro instalado en el gestor de contenido (`false` estricto, `true` con imágenes).
    installed: Cell<Option<bool>>,
    /// Ancho actual de la tarjeta (px).
    width: Cell<i32>,
    /// Aspecto aplicado ahora mismo y si el documento cargado admite imágenes https.
    look: Cell<Look>,
    remote: Cell<bool>,
    notified: Cell<u64>,
    /// La vista se está cerrando: el fin del proceso web ya no es un fallo.
    closing: Cell<bool>,
}

pub struct HtmlView {
    /// Tarjeta (blanca o, en tema oscuro, oscura) con esquinas redondeadas que contiene el
    /// WebView.
    card: gtk::Box,
    view: webkit6::WebView,
    ucm: webkit6::UserContentManager,
    store: webkit6::UserContentFilterStore,
    /// Sesión efímera de la que depende el WebView: se libera al soltar la vista.
    _session: webkit6::NetworkSession,
    /// Filtros ya compilados: `false` = estricto, `true` = permite imágenes https.
    filters: Rc<RefCell<HashMap<bool, webkit6::UserContentFilter>>>,
    shared: Rc<Shared>,
}

impl HtmlView {
    /// Crea el WebView sin cargar nada. Falla si no se puede preparar el almacén de filtros
    /// (fallo cerrado: no se muestra HTML sin filtro).
    pub fn new(events: Events) -> Result<Self, String> {
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
        view.set_hexpand(true);
        view.set_vexpand(true);

        let shared = Rc::new(Shared {
            events,
            token: Cell::new(0),
            doc: RefCell::new(WebDoc {
                clean: String::new(),
                body_style: String::new(),
                width_hint: 0,
                look: Look::Light,
            }),
            width: Cell::new(0),
            look: Cell::new(Look::Light),
            remote: Cell::new(false),
            hover: RefCell::new(None),
            finished: Cell::new(0),
            installed: Cell::new(None),
            notified: Cell::new(0),
            closing: Cell::new(false),
        });

        {
            let shared = shared.clone();
            view.connect_decide_policy(move |view, decision, kind| {
                decide(view, decision, kind, &shared)
            });
        }
        {
            let on_load = shared.clone();
            view.connect_load_changed(move |view, event| {
                if event == webkit6::LoadEvent::Finished {
                    on_load.finished.set(on_load.token.get());
                    check_loaded(view, &on_load);
                }
            });
            let shared = shared.clone();
            // El título (que lleva el token) puede llegar después de `Finished`.
            view.connect_title_notify(move |view| check_loaded(view, &shared));
        }
        {
            let shared = shared.clone();
            view.connect_load_failed(move |_, _, uri, error| {
                // Los recursos bloqueados no llegan aquí; esto es la carga del documento.
                if uri.starts_with("about:") {
                    (shared.events)(WebEvent::Failed(error.to_string()));
                }
                false
            });
        }
        {
            let shared = shared.clone();
            view.connect_web_process_terminated(move |_, reason| {
                if shared.closing.get() {
                    return;
                }
                (shared.events)(WebEvent::Failed(format!(
                    "el proceso web terminó ({reason:?})"
                )));
            });
        }
        // Sin menú contextual (su «Abrir enlace» saltaría la confirmación del lector) ni
        // ventanas emergentes (`create` sin manejar no abre nada).
        view.connect_context_menu(|_, _, _| true);
        view.connect_create(|_, _| None);
        {
            let shared = shared.clone();
            view.connect_mouse_target_changed(move |view, hit, _| {
                if hit.context_is_link() {
                    let uri = hit.link_uri().map(|u| u.to_string()).unwrap_or_default();
                    view.set_tooltip_text(Some(&uri));
                    *shared.hover.borrow_mut() = Some((
                        uri,
                        hit.link_label().map(|l| l.to_string()).unwrap_or_default(),
                    ));
                } else {
                    view.set_tooltip_text(None);
                    *shared.hover.borrow_mut() = None;
                }
            });
        }

        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("correo-mail-card");
        card.set_overflow(gtk::Overflow::Hidden);
        card.set_vexpand(true);
        // Sensor de ancho de altura 0: `resize` avisa solo cuando cambia la asignación.
        let sensor = gtk::DrawingArea::new();
        sensor.set_content_height(0);
        sensor.set_hexpand(true);
        {
            let shared = shared.clone();
            let view = view.clone();
            sensor.connect_resize(move |_, w, _| {
                shared.width.set(w);
                fit_zoom(&view, &shared);
            });
        }
        card.append(&sensor);
        card.append(&view);

        let this = Self {
            card,
            view,
            ucm,
            store,
            _session: session,
            filters: Rc::new(RefCell::new(HashMap::new())),
            shared,
        };
        this.apply_chrome(Look::Light);
        Ok(this)
    }

    /// Color de fondo del WebView (visible antes de que cargue el contenido: sin destello
    /// blanco en tema oscuro), hoja de estilos de usuario del aspecto y clase de la tarjeta.
    /// No toca el documento: se puede cambiar en caliente.
    fn apply_chrome(&self, look: Look) {
        self.shared.look.set(look);
        let (r, g, b) = card_color(look);
        self.view.set_background_color(&gtk::gdk::RGBA::new(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        ));
        self.ucm.remove_all_style_sheets();
        self.ucm.add_style_sheet(&webkit6::UserStyleSheet::new(
            &user_stylesheet(look),
            webkit6::UserContentInjectedFrames::AllFrames,
            webkit6::UserStyleLevel::User,
            &[],
            &[],
        ));
        if look.is_dark() {
            self.card.add_css_class("correo-mail-card-dark");
        } else {
            self.card.remove_css_class("correo-mail-card-dark");
        }
    }

    /// Cambia el aspecto del documento actual. Normalmente es instantáneo; solo si el correo
    /// usa `prefers-color-scheme` y cambia lo que valen sus consultas hay que recargar el
    /// documento (ya saneado, en memoria): entonces devuelve el token de la nueva carga.
    pub fn set_look(&self, look: Look) -> Option<u64> {
        let old = self.shared.look.get();
        if old == look {
            return None;
        }
        self.apply_chrome(look);
        let reload = {
            let mut doc = self.shared.doc.borrow_mut();
            doc.look = look;
            !doc.clean.is_empty()
                && old.dark_media() != look.dark_media()
                && doc.clean.contains("prefers-color-scheme")
        };
        reload.then(|| self.reload_with(self.shared.remote.get()))
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.card.upcast_ref()
    }

    /// Carga el fragmento (ya saneado) con el filtro estricto o, con `remote_images`, con la
    /// excepción para imágenes https. Devuelve el token de la carga: llegará `Loaded(token)`.
    pub fn show(&self, doc: WebDoc, remote_images: bool) -> u64 {
        let token = self.shared.token.get() + 1;
        self.shared.token.set(token);
        self.shared.remote.set(remote_images);
        if self.shared.look.get() != doc.look {
            self.apply_chrome(doc.look);
        }
        *self.shared.doc.borrow_mut() = doc;
        fit_zoom(&self.view, &self.shared);
        if let Some(filter) = self.filters.borrow().get(&remote_images).cloned() {
            apply(
                &self.view,
                &self.ucm,
                &self.shared,
                &filter,
                remote_images,
                token,
            );
            return token;
        }
        self.compile_and_load(remote_images, token);
        token
    }

    /// Recarga el mensaje actual permitiendo imágenes https.
    pub fn reload_with_images(&self) -> u64 {
        self.reload_with(true)
    }

    fn reload_with(&self, remote_images: bool) -> u64 {
        let doc = {
            let d = self.shared.doc.borrow();
            WebDoc {
                clean: d.clean.clone(),
                body_style: d.body_style.clone(),
                width_hint: d.width_hint,
                look: d.look,
            }
        };
        self.show(doc, remote_images)
    }

    /// Descarta el documento actual (libera su memoria) sin destruir el WebView.
    pub fn blank(&self) {
        self.shared.token.set(self.shared.token.get() + 1);
        self.shared.doc.borrow_mut().clean.clear();
        self.view.load_uri("about:blank");
    }

    /// Cierra la vista: pide a WebKit terminar el proceso web antes de soltarla.
    pub fn shutdown(&self) {
        self.shared.closing.set(true);
        self.view.terminate_web_process();
    }

    /// Compila el filtro y, cuando está listo, lo instala y carga el HTML. Nunca se carga el
    /// documento sin filtro: si la compilación falla se avisa y no se muestra nada.
    fn compile_and_load(&self, remote_images: bool, token: u64) {
        let id = if remote_images {
            "correo-imagenes"
        } else {
            "correo-estricto"
        };
        let source = glib::Bytes::from_owned(content_filter_json(remote_images).into_bytes());
        let view = self.view.downgrade();
        let ucm = self.ucm.clone();
        let shared = self.shared.clone();
        let filters = self.filters.clone();
        self.store.save(
            id,
            &source,
            None::<&gio::Cancellable>,
            move |res| match res {
                Ok(filter) => {
                    filters.borrow_mut().insert(remote_images, filter.clone());
                    if shared.token.get() != token {
                        return;
                    }
                    if let Some(view) = view.upgrade() {
                        apply(&view, &ucm, &shared, &filter, remote_images, token);
                    }
                }
                Err(e) => (shared.events)(WebEvent::Failed(format!(
                    "filtro de contenido no disponible: {e}"
                ))),
            },
        );
    }
}

/// Encoge el contenido (zoom) si el correo fija un ancho de tabla mayor que el disponible.
fn fit_zoom(view: &webkit6::WebView, shared: &Shared) {
    let hint = shared.doc.borrow().width_hint;
    let width = shared.width.get();
    let zoom = if hint == 0 || width <= 0 {
        1.0
    } else {
        (f64::from(width) / (f64::from(hint) + 40.0)).clamp(0.55, 1.0)
    };
    if (view.zoom_level() - zoom).abs() > 0.01 {
        view.set_zoom_level(zoom);
    }
}

/// Avisa `Loaded` una sola vez por documento: carga terminada y título con su token.
fn check_loaded(view: &webkit6::WebView, shared: &Shared) {
    let token = shared.token.get();
    if shared.finished.get() == token
        && shared.notified.get() != token
        && view.title().as_deref() == Some(format!("correo-{token}").as_str())
    {
        shared.notified.set(token);
        (shared.events)(WebEvent::Loaded(token));
    }
}

/// Sustituye el filtro activo y carga el documento envuelto.
fn apply(
    view: &webkit6::WebView,
    ucm: &webkit6::UserContentManager,
    shared: &Shared,
    filter: &webkit6::UserContentFilter,
    remote_images: bool,
    token: u64,
) {
    if shared.installed.get() != Some(remote_images) {
        ucm.remove_all_filters();
        ucm.add_filter(filter);
        shared.installed.set(Some(remote_images));
    }
    let doc = {
        let d = shared.doc.borrow();
        build_document(
            &d.clean,
            &d.body_style,
            remote_images,
            token,
            d.look.dark_media(),
        )
    };
    view.load_html(&doc, Some("about:blank"));
}

/// Ajustes del WebView: nada activo salvo el renderizado HTML estático.
fn settings() -> webkit6::Settings {
    let s = webkit6::Settings::new();
    s.set_enable_javascript(false);
    s.set_enable_javascript_markup(false);
    s.set_javascript_can_open_windows_automatically(false);
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
    s.set_enable_back_forward_navigation_gestures(false);
    s.set_enable_fullscreen(false);
    s.set_enable_site_specific_quirks(false);
    s.set_enable_write_console_messages_to_stdout(false);
    // Un correo no necesita aceleración: evita el proceso/GPU extra y ahorra memoria.
    s.set_hardware_acceleration_policy(webkit6::HardwareAccelerationPolicy::Never);
    s.set_minimum_font_size(10);
    s.set_default_charset("utf-8");
    s
}

/// Política de navegación y respuestas (los recursos secundarios los bloquea el filtro).
fn decide(
    _view: &webkit6::WebView,
    decision: &webkit6::PolicyDecision,
    kind: webkit6::PolicyDecisionType,
    shared: &Shared,
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
            let label = shared
                .hover
                .borrow()
                .as_ref()
                .filter(|(u, _)| *u == uri)
                .map(|(_, l)| l.clone());
            (shared.events)(WebEvent::OpenLink { uri, label });
        }
        NavDecision::Block => decision.ignore(),
    }
    true
}

//! T10: lector de mensaje.
//! Cabecera, barra de acciones, cuerpo (HTML saneado o texto plano) sin WebKit y adjuntos.
//! Toda la red va por `services.spawn`; los resultados vuelven al hilo de GTK con `spawn_local`.

#[path = "reader_html.rs"]
mod reader_html;
#[cfg(feature = "webkit")]
#[path = "reader_web.rs"]
mod reader_web;
#[cfg(any(feature = "webkit", test))]
#[path = "reader_web_policy.rs"]
mod reader_web_policy;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use correo_core::{Account, Address, Flags, Mailbox, MailboxRole, MessageBody, MessageHeader};
use gtk::{gio, glib};

use crate::services::{flatten, Services};
use crate::ui_style;
use crate::ui_types::ComposeMode;
use reader_html::{
    count_images, html_to_segments, link_looks_deceptive, plain_to_segments, Segment, Style,
    MAX_LEVEL,
};

type ComposeFn = Box<dyn Fn(ComposeMode)>;

const BANNER_NORMAL: &str = "Imágenes bloqueadas para proteger tu privacidad";
/// Cuerpos mayores que esto se truncan: la conversión corre en el hilo de GTK.
const BODY_LIMIT: usize = 200 * 1024;
#[cfg(feature = "webkit")]
const TRUNCATED_NOTICE: &str = "Mensaje truncado, usa Ver original";
#[cfg(not(feature = "webkit"))]
const TRUNCATED_NOTICE: &str = "Mensaje truncado: no se muestra el contenido completo";
/// Longitud máxima (en caracteres) de un nombre de adjunto al guardarlo.
const MAX_NAME_CHARS: usize = 200;
/// Carpeta bajo `$XDG_RUNTIME_DIR` donde se abren los adjuntos (0700).
const ATTACHMENTS_DIR: &str = "correo-adjuntos";
/// Extensiones que pueden ejecutar código o cargar contenido activo: se pide confirmación.
const RISKY_EXTENSIONS: &[&str] = &[
    "html", "htm", "shtml", "xhtml", "xht", "svg", "svgz", "js", "mjs", "vbs", "wsf", "hta", "sh",
    "bash", "zsh", "desktop", "exe", "msi", "bat", "cmd", "com", "scr", "ps1", "reg", "lnk", "jar",
    "py", "pl", "rb", "php", "appimage", "run", "bin", "deb", "rpm", "flatpak", "dll", "so",
    "docm", "xlsm", "pptm", "dotm", "xltm", "ppsm",
];
#[cfg(feature = "webkit")]
const ORIGINAL_LABEL: &str = "Ver original";

/// Mensaje mostrado actualmente.
#[derive(Clone, Default)]
struct Current {
    account: Option<Account>,
    mailbox: Option<Mailbox>,
    header: Option<MessageHeader>,
    body: Option<MessageBody>,
}

/// Enlace del cuerpo: destino real y texto visible.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LinkTarget {
    href: String,
    shown: String,
}

/// Acciones que modifican el mensaje en el servidor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Archive,
    Delete,
    MarkUnread,
}

struct Inner {
    services: Rc<Services>,
    /// Se incrementa en cada `show`/`clear`: descarta respuestas de peticiones obsoletas.
    generation: Cell<u64>,
    current: RefCell<Current>,
    /// Enlaces del cuerpo actual; el tag `link-N` apunta a `links[N]`.
    links: RefCell<Vec<LinkTarget>>,
    compose: RefCell<Option<ComposeFn>>,
    /// Se avisa tras cambios que afectan a la lista (archivar, borrar, leer o no leer).
    changed: RefCell<Option<Rc<dyn Fn()>>>,

    root: gtk::Box,
    avatar: adw::Avatar,
    subject: gtk::Label,
    from: gtk::Label,
    to: gtk::Label,
    cc: gtk::Label,
    date: gtk::Label,
    reply: gtk::Button,
    reply_all: gtk::Button,
    forward: gtk::Button,
    archive: gtk::Button,
    delete: gtk::Button,
    mark_unread: gtk::Button,
    banner: adw::Banner,
    attachments_scroll: gtk::ScrolledWindow,
    attachments: gtk::ListBox,
    stack: gtk::Stack,
    text_view: gtk::TextView,
    error_page: adw::StatusPage,
    error_detail: gtk::Label,
    /// Alterna entre el texto y la vista original (solo con la feature `webkit`).
    #[cfg(feature = "webkit")]
    original: gtk::Button,
    /// Vista original existente; `None` salvo mientras está visible.
    #[cfg(feature = "webkit")]
    web: RefCell<Option<reader_web::OriginalView>>,
}

pub struct MessageView {
    widget: gtk::Widget,
    /// Acciones del mensaje: viven en la cabecera de la ventana, no en el lector.
    actions: gtk::Box,
    inner: Rc<Inner>,
}

impl MessageView {
    pub fn new(services: Rc<Services>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Cabecera: avatar del remitente, remitente, destinatarios y fecha.
        let avatar = adw::Avatar::new(44, None, true);
        avatar.set_valign(gtk::Align::Start);
        let from = gtk::Label::new(None);
        from.set_xalign(0.0);
        from.set_hexpand(true);
        from.set_ellipsize(gtk::pango::EllipsizeMode::End);
        from.set_selectable(true);
        from.add_css_class("correo-reader-from");
        let date = gtk::Label::new(None);
        date.set_valign(gtk::Align::Start);
        date.add_css_class("correo-reader-date");
        let first_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        first_line.append(&from);
        first_line.append(&date);
        let meta = gtk::Box::new(gtk::Orientation::Vertical, 1);
        meta.set_hexpand(true);
        meta.append(&first_line);
        let to = meta_label(&meta);
        let cc = meta_label(&meta);
        let sender_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        sender_row.append(&avatar);
        sender_row.append(&meta);

        let subject = gtk::Label::new(None);
        subject.add_css_class("correo-reader-subject");
        subject.set_halign(gtk::Align::Start);
        subject.set_xalign(0.0);
        subject.set_wrap(true);
        subject.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        subject.set_selectable(true);

        let header_box = gtk::Box::new(gtk::Orientation::Vertical, 14);
        header_box.add_css_class("correo-reader-header");
        header_box.append(&subject);
        header_box.append(&sender_row);
        root.append(&header_box);

        let reply = action_button("Responder", "mail-reply-sender-symbolic", "Responder");
        let reply_all = action_button(
            "Responder a todos",
            "mail-reply-all-symbolic",
            "Responder a todos",
        );
        let forward = action_button("Reenviar", "mail-forward-symbolic", "Reenviar");
        let archive = action_button("Archivar", "mail-archive-symbolic", "Archivar (Ctrl+E)");
        let delete = action_button("Borrar", "user-trash-symbolic", "Borrar (Ctrl+Supr)");
        delete.add_css_class("correo-danger");
        // Acciones secundarias dentro de un menú «Más».
        let mark_unread = menu_item("Marcar como no leído");
        #[cfg(feature = "webkit")]
        let original = menu_item(ORIGINAL_LABEL);
        let more_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        more_box.append(&mark_unread);
        #[cfg(feature = "webkit")]
        {
            original.set_visible(false);
            more_box.append(&original);
        }
        let more_popover = gtk::Popover::new();
        more_popover.add_css_class("menu");
        more_popover.set_child(Some(&more_box));
        let more = gtk::MenuButton::new();
        more.set_icon_name("view-more-symbolic");
        more.set_popover(Some(&more_popover));
        more.set_tooltip_text(Some("Más acciones"));
        more.update_property(&[gtk::accessible::Property::Label("Más acciones")]);
        {
            let popover = more_popover.clone();
            let buttons: Vec<gtk::Button> = {
                #[allow(unused_mut)]
                let mut v = vec![mark_unread.clone()];
                #[cfg(feature = "webkit")]
                v.push(original.clone());
                v
            };
            for b in buttons {
                let popover = popover.clone();
                b.connect_clicked(move |_| popover.popdown());
            }
        }
        let reply_group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        reply_group.add_css_class("linked");
        for b in [&reply, &reply_all, &forward] {
            reply_group.append(b);
        }
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.add_css_class("correo-actions");
        actions.set_visible(false);
        actions.append(&reply_group);
        actions.append(&archive);
        actions.append(&delete);
        actions.append(&more);

        let banner = adw::Banner::new(BANNER_NORMAL);
        banner.set_revealed(false);
        banner.add_css_class("correo-privacy");
        root.append(&banner);

        let attachments = gtk::ListBox::new();
        attachments.add_css_class("boxed-list");
        attachments.add_css_class("correo-attachments");
        attachments.set_selection_mode(gtk::SelectionMode::None);
        let attachments_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(180)
            .propagate_natural_height(true)
            .visible(false)
            .child(&attachments)
            .build();
        attachments_scroll.add_css_class("correo-attachments-scroll");
        root.append(&attachments_scroll);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);

        let empty = adw::StatusPage::builder()
            .icon_name("mail-read-symbolic")
            .title("Ningún mensaje seleccionado")
            .description("Elige un mensaje de la lista para leerlo.")
            .build();
        empty.add_css_class("correo-empty");
        stack.add_named(&empty, Some("empty"));

        stack.add_named(&skeleton(), Some("loading"));

        let error_page = adw::StatusPage::builder()
            .icon_name("dialog-error-symbolic")
            .title("No se pudo abrir el mensaje")
            .build();
        error_page.add_css_class("correo-empty");
        error_page.add_css_class("correo-error");
        stack.add_named(&error_page, Some("error"));

        let text_view = gtk::TextView::new();
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        text_view.set_wrap_mode(gtk::WrapMode::WordChar);
        text_view.add_css_class("correo-reader-body");
        text_view.set_left_margin(0);
        text_view.set_right_margin(0);
        text_view.set_top_margin(4);
        text_view.set_bottom_margin(24);
        text_view.set_pixels_inside_wrap(0);
        text_view.set_pixels_below_lines(8);
        let body_scroll = gtk::ScrolledWindow::builder()
            .child(&text_view)
            .vexpand(true)
            .build();
        stack.add_named(&body_scroll, Some("content"));
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(160);
        stack.set_visible_child_name("empty");
        root.append(&stack);

        for b in [
            &reply,
            &reply_all,
            &forward,
            &archive,
            &delete,
            &mark_unread,
        ] {
            b.set_sensitive(false);
        }
        let error_detail = gtk::Label::new(None);

        let inner = Rc::new(Inner {
            services,
            generation: Cell::new(0),
            current: RefCell::new(Current::default()),
            links: RefCell::new(Vec::new()),
            compose: RefCell::new(None),
            changed: RefCell::new(None),
            root: root.clone(),
            avatar,
            subject,
            from,
            to,
            cc,
            date,
            reply: reply.clone(),
            reply_all: reply_all.clone(),
            forward: forward.clone(),
            archive: archive.clone(),
            delete: delete.clone(),
            mark_unread: mark_unread.clone(),
            banner,
            attachments_scroll,
            attachments,
            stack,
            text_view,
            error_page,
            error_detail: error_detail.clone(),
            #[cfg(feature = "webkit")]
            original: original.clone(),
            #[cfg(feature = "webkit")]
            web: RefCell::new(None),
        });

        #[cfg(feature = "webkit")]
        {
            let weak = Rc::downgrade(&inner);
            original.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    toggle_original(&inner);
                }
            });
            let weak = Rc::downgrade(&inner);
            inner.banner.connect_button_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    allow_remote_images(&inner);
                }
            });
        }

        connect_compose_button(&inner, &reply, ComposeMode::Reply);
        connect_compose_button(&inner, &reply_all, ComposeMode::ReplyAll);
        connect_compose_button(&inner, &forward, ComposeMode::Forward);
        connect_action_button(&inner, &archive, Action::Archive);
        connect_action_button(&inner, &delete, Action::Delete);
        connect_action_button(&inner, &mark_unread, Action::MarkUnread);
        connect_link_clicks(&inner);
        connect_link_tooltips(&inner);

        let retry = gtk::Button::with_label("Reintentar");
        retry.set_halign(gtk::Align::Center);
        retry.add_css_class("pill");
        let weak = Rc::downgrade(&inner);
        retry.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                let cur = inner.current.borrow().clone();
                if let (Some(a), Some(m), Some(h)) = (cur.account, cur.mailbox, cur.header) {
                    show_inner(&inner, a, m, h);
                }
            }
        });
        // Detalle técnico plegado: el mensaje principal es legible para cualquiera.
        error_detail.set_wrap(true);
        error_detail.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        error_detail.set_selectable(true);
        error_detail.set_xalign(0.0);
        error_detail.add_css_class("monospace");
        error_detail.add_css_class("caption");
        let expander = gtk::Expander::new(Some("Detalles"));
        expander.set_child(Some(&error_detail));
        let error_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        error_box.append(&retry);
        error_box.append(&expander);
        inner.error_page.set_child(Some(&error_box));

        // Ancho de lectura cómodo en pantallas anchas.
        let clamp = adw::Clamp::builder()
            .maximum_size(780)
            .tightening_threshold(560)
            .child(&root)
            .build();
        clamp.set_vexpand(true);

        Self {
            widget: clamp.upcast(),
            actions,
            inner,
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.widget
    }

    /// Botones de acción del mensaje, para colocarlos en la cabecera de la ventana.
    pub fn actions(&self) -> &gtk::Box {
        &self.actions
    }

    /// Muestra el mensaje: descarga el cuerpo bajo demanda y marca como leído.
    pub fn show(&self, account: Account, mailbox: Mailbox, header: MessageHeader) {
        self.actions.set_visible(true);
        show_inner(&self.inner, account, mailbox, header);
    }

    pub fn clear(&self) {
        self.actions.set_visible(false);
        clear_view(&self.inner);
    }

    /// Se invoca al pulsar Responder/Responder a todos/Reenviar.
    pub fn connect_compose(&self, f: impl Fn(ComposeMode) + 'static) {
        *self.inner.compose.borrow_mut() = Some(Box::new(f));
    }

    /// Se invoca cuando el mensaje cambia de estado o desaparece de su carpeta
    /// (leído al abrirlo, no leído, archivado o borrado). El llamador refresca la lista.
    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        *self.inner.changed.borrow_mut() = Some(Rc::new(f));
    }
}

fn meta_label(parent: &gtk::Box) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_selectable(true);
    label.add_css_class("correo-reader-meta");
    parent.append(&label);
    label
}

/// Botón solo de icono, con tooltip y etiqueta accesible (el lector de pantalla dice `label`).
fn action_button(label: &str, icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

/// Entrada de texto de un menú emergente.
fn menu_item(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("flat");
    if let Some(child) = button.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
        child.set_xalign(0.0);
    }
    button
}

/// Esqueleto del mensaje mientras se descarga el cuerpo.
fn skeleton() -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_margin_top(8);
    column.set_valign(gtk::Align::Start);
    column.set_accessible_role(gtk::AccessibleRole::Presentation);
    for width in [-1, 520, -1, 440, 300] {
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("correo-skeleton");
        bar.set_size_request(width.max(0), 14);
        bar.set_halign(if width < 0 {
            gtk::Align::Fill
        } else {
            gtk::Align::Start
        });
        column.append(&bar);
    }
    column
}

fn connect_compose_button(
    inner: &Rc<Inner>,
    button: &gtk::Button,
    make: fn(MessageHeader, MessageBody) -> ComposeMode,
) {
    let weak = Rc::downgrade(inner);
    button.connect_clicked(move |_| {
        let Some(inner) = weak.upgrade() else { return };
        let cur = inner.current.borrow().clone();
        let (Some(header), Some(body)) = (cur.header, cur.body) else {
            return;
        };
        let compose = inner.compose.borrow();
        if let Some(f) = compose.as_ref() {
            f(make(header, body));
        }
    });
}

fn connect_action_button(inner: &Rc<Inner>, button: &gtk::Button, action: Action) {
    let weak = Rc::downgrade(inner);
    button.connect_clicked(move |_| {
        if let Some(inner) = weak.upgrade() {
            run_action(&inner, action);
        }
    });
}

/// Enlace bajo la posición (coordenadas del widget), si la hay.
fn link_at(inner: &Inner, view: &gtk::TextView, x: i32, y: i32) -> Option<LinkTarget> {
    let (bx, by) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x, y);
    let iter = view.iter_at_location(bx, by)?;
    let links = inner.links.borrow();
    iter.tags().iter().find_map(|tag| {
        let name = tag.name()?;
        let idx: usize = name.strip_prefix("link-")?.parse().ok()?;
        links.get(idx).cloned()
    })
}

/// Clic en un enlace del cuerpo: si el texto parece otro dominio, pide confirmación.
fn connect_link_clicks(inner: &Rc<Inner>) {
    let click = gtk::GestureClick::new();
    let weak = Rc::downgrade(inner);
    click.connect_released(move |gesture, _, x, y| {
        let Some(inner) = weak.upgrade() else { return };
        let Some(view) = gesture
            .widget()
            .and_then(|w| w.downcast::<gtk::TextView>().ok())
        else {
            return;
        };
        if let Some(link) = link_at(&inner, &view, x as i32, y as i32) {
            open_link(&view, &link);
        }
    });
    inner.text_view.add_controller(click);
}

/// Muestra el destino real del enlace al pasar el ratón por encima.
fn connect_link_tooltips(inner: &Rc<Inner>) {
    inner.text_view.set_has_tooltip(true);
    let weak = Rc::downgrade(inner);
    inner
        .text_view
        .connect_query_tooltip(move |view, x, y, _keyboard, tooltip| {
            let Some(inner) = weak.upgrade() else {
                return false;
            };
            match link_at(&inner, view, x, y) {
                Some(link) => {
                    tooltip.set_text(Some(&link.href));
                    true
                }
                None => false,
            }
        });
}

/// Abre el enlace con el lanzador de GTK. Si el texto visible aparenta otro dominio
/// (posible suplantación), antes pide confirmación.
fn open_link(widget: &impl IsA<gtk::Widget>, link: &LinkTarget) {
    if !link_looks_deceptive(&link.shown, &link.href) {
        launch_url(widget, &link.href);
        return;
    }
    let body = format!(
        "El texto del enlace parece «{}», pero apunta a {}. Comprueba la dirección antes de abrirla.",
        link.shown, link.href
    );
    let dialog = adw::AlertDialog::new(Some("¿Abrir este enlace?"), Some(&body));
    dialog.add_response("cancel", "Cancelar");
    dialog.add_response("open", "Abrir");
    dialog.set_response_appearance("open", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let target = widget.clone().upcast::<gtk::Widget>();
    let href = link.href.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "open" {
            launch_url(&target, &href);
        }
    });
    dialog.present(Some(widget));
}

fn launch_url(widget: &impl IsA<gtk::Widget>, url: &str) {
    let parent = widget.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    gtk::UriLauncher::new(url).launch(parent.as_ref(), None::<&gio::Cancellable>, |res| {
        if let Err(e) = res {
            eprintln!("no se pudo abrir el enlace: {e}");
        }
    });
}

fn show_inner(inner: &Rc<Inner>, account: Account, mailbox: Mailbox, header: MessageHeader) {
    let gen = inner.generation.get() + 1;
    inner.generation.set(gen);
    destroy_original(inner);

    set_header_labels(inner, &header);
    set_actions_enabled(inner, true, false);
    inner.banner.set_revealed(false);
    clear_attachments(inner);
    inner.links.borrow_mut().clear();
    *inner.current.borrow_mut() = Current {
        account: Some(account.clone()),
        mailbox: Some(mailbox.clone()),
        header: Some(header.clone()),
        body: None,
    };

    // Caché local: si el cuerpo ya está en la BD no hace falta la red.
    let cached = {
        let store = inner.services.store.lock().expect("store envenenado");
        store
            .message_id(mailbox.id, header.uid)
            .ok()
            .flatten()
            .and_then(|mid| store.get_body(mid).ok().flatten())
    };
    if let Some(body) = cached {
        finish_body(inner, body);
        return;
    }

    inner.stack.set_visible_child_name("loading");
    let backend = inner.services.backend(&account);
    let (m, uid) = (mailbox.clone(), header.uid);
    let handle = flatten(
        inner
            .services
            .spawn(async move { backend.fetch_body(&m, uid).await }),
    );
    let weak = Rc::downgrade(inner);
    glib::MainContext::default().spawn_local(async move {
        let result = handle.await;
        let Some(inner) = weak.upgrade() else { return };
        if inner.generation.get() != gen {
            return;
        }
        match result {
            Ok(body) => finish_body(&inner, body),
            Err(e) => show_error(&inner, &e.to_string()),
        }
    });
}

/// Pinta el cuerpo ya disponible y marca el mensaje como leído si hace falta.
fn finish_body(inner: &Rc<Inner>, body: MessageBody) {
    render_body(inner, &body);
    inner.current.borrow_mut().body = Some(body);
    set_actions_enabled(inner, true, true);

    let cur = inner.current.borrow().clone();
    if let (Some(account), Some(mailbox), Some(header)) = (cur.account, cur.mailbox, cur.header) {
        if !header.flags.seen {
            mark_seen(inner, account, mailbox, header);
        }
    }
}

fn mark_seen(inner: &Rc<Inner>, account: Account, mailbox: Mailbox, header: MessageHeader) {
    let backend = inner.services.backend(&account);
    let (m, uid) = (mailbox.clone(), header.uid);
    let handle = flatten(
        inner
            .services
            .spawn(async move { backend.set_seen(&m, uid, true).await }),
    );
    let weak = Rc::downgrade(inner);
    glib::MainContext::default().spawn_local(async move {
        if handle.await.is_err() {
            return;
        }
        let Some(inner) = weak.upgrade() else { return };
        let mut flags = header.flags.clone();
        flags.seen = true;
        update_flags(&inner, &mailbox, header.uid, header.id, &flags);
        notify_changed(&inner);
    });
}

/// Avisa al llamador de que la lista puede haber cambiado. Clona el callback antes de
/// llamarlo para no retener el `RefCell` durante la llamada.
fn notify_changed(inner: &Inner) {
    let callback = inner.changed.borrow().clone();
    if let Some(callback) = callback {
        callback();
    }
}

/// Guarda flags en la BD local y en el mensaje mostrado (si sigue siendo el mismo).
fn update_flags(inner: &Inner, mailbox: &Mailbox, uid: u32, message_id: i64, flags: &Flags) {
    if let Ok(store) = inner.services.store.lock() {
        let _ = store.set_flags(mailbox.id, uid, flags);
    }
    let mut cur = inner.current.borrow_mut();
    if let Some(h) = cur.header.as_mut() {
        if h.id == message_id {
            h.flags = flags.clone();
        }
    }
}

fn run_action(inner: &Rc<Inner>, action: Action) {
    let cur = inner.current.borrow().clone();
    let (Some(account), Some(mailbox), Some(header)) = (cur.account, cur.mailbox, cur.header)
    else {
        return;
    };

    let target = match action {
        Action::Archive => {
            let archive = inner
                .services
                .store
                .lock()
                .expect("store envenenado")
                .list_mailboxes(account.id)
                .ok()
                .and_then(|list| list.into_iter().find(|m| m.role == MailboxRole::Archive));
            match archive {
                Some(target) => Some(target),
                None => {
                    alert(
                        &inner.root,
                        "No se puede archivar",
                        "No se encontró la carpeta de archivo de esta cuenta.",
                    );
                    return;
                }
            }
        }
        _ => None,
    };

    let backend = inner.services.backend(&account);
    let (m, uid) = (mailbox.clone(), header.uid);
    let handle = flatten(inner.services.spawn(async move {
        match (action, target) {
            (Action::Archive, Some(target)) => backend.move_to(&m, uid, &target).await,
            (Action::Delete, _) => backend.delete(&m, uid).await,
            (Action::MarkUnread, _) => backend.set_seen(&m, uid, false).await,
            _ => Ok(()),
        }
    }));
    let weak = Rc::downgrade(inner);
    glib::MainContext::default().spawn_local(async move {
        let result = handle.await;
        let Some(inner) = weak.upgrade() else { return };
        match result {
            Ok(()) => match action {
                Action::Archive | Action::Delete => {
                    if let Ok(store) = inner.services.store.lock() {
                        let _ = store.delete_uid(mailbox.id, header.uid);
                    }
                    clear_view(&inner);
                    notify_changed(&inner);
                }
                Action::MarkUnread => {
                    let mut flags = header.flags.clone();
                    flags.seen = false;
                    update_flags(&inner, &mailbox, header.uid, header.id, &flags);
                    notify_changed(&inner);
                }
            },
            Err(e) => alert(
                &inner.root,
                "No se pudo completar la acción",
                &e.to_string(),
            ),
        }
    });
}

fn clear_view(inner: &Rc<Inner>) {
    inner.generation.set(inner.generation.get() + 1);
    destroy_original(inner);
    *inner.current.borrow_mut() = Current::default();
    inner.links.borrow_mut().clear();
    inner.subject.set_text("");
    inner.from.set_text("");
    inner.to.set_text("");
    inner.cc.set_text("");
    inner.date.set_text("");
    inner.banner.set_revealed(false);
    clear_attachments(inner);
    set_actions_enabled(inner, false, false);
    inner.stack.set_visible_child_name("empty");
}

fn show_error(inner: &Rc<Inner>, detail: &str) {
    inner
        .error_page
        .set_description(Some("Comprueba la conexión e inténtalo de nuevo."));
    inner.error_detail.set_text(detail);
    inner.stack.set_visible_child_name("error");
}

fn set_actions_enabled(inner: &Inner, message: bool, body: bool) {
    for b in [&inner.archive, &inner.delete, &inner.mark_unread] {
        b.set_sensitive(message);
    }
    for b in [&inner.reply, &inner.reply_all, &inner.forward] {
        b.set_sensitive(body);
    }
}

fn set_header_labels(inner: &Inner, header: &MessageHeader) {
    inner.subject.set_text(if header.subject.is_empty() {
        "(sin asunto)"
    } else {
        &header.subject
    });
    let sender = header.from.first();
    let name = sender.map_or_else(|| "(sin remitente)".to_string(), short_name);
    let email = sender.map_or("", |a| a.email.as_str());
    inner.from.set_markup(&format!(
        "{}  <span alpha=\"60%\" weight=\"normal\" size=\"small\">{}</span>",
        glib::markup_escape_text(&name),
        glib::markup_escape_text(email)
    ));
    inner
        .from
        .set_tooltip_text(Some(&format_addresses(&header.from)));
    inner.avatar.set_text(Some(&name));
    ui_style::set_tone(&inner.avatar, &ui_style::sender_class(email));

    let recipients = |list: &[Address]| list.iter().map(short_name).collect::<Vec<_>>().join(", ");
    inner.to.set_text(&format!(
        "Para: {}",
        if header.to.is_empty() {
            "(nadie)".to_string()
        } else {
            recipients(&header.to)
        }
    ));
    inner
        .to
        .set_tooltip_text(Some(&format_addresses(&header.to)));
    inner.cc.set_visible(!header.cc.is_empty());
    inner
        .cc
        .set_text(&format!("Cc: {}", recipients(&header.cc)));
    inner
        .cc
        .set_tooltip_text(Some(&format_addresses(&header.cc)));
    let date = glib::DateTime::from_unix_local(header.date)
        .ok()
        .and_then(|d| d.format("%-d %b %Y, %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    inner.date.set_text(&date);
}

/// Nombre visible de una dirección: su nombre, o el email si no lo tiene.
fn short_name(a: &Address) -> String {
    a.name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map_or_else(|| a.email.clone(), str::to_string)
}

fn format_addresses(addresses: &[Address]) -> String {
    if addresses.is_empty() {
        return "(nadie)".into();
    }
    addresses
        .iter()
        .map(
            |a| match a.name.as_deref().filter(|n| !n.trim().is_empty()) {
                Some(name) => format!("{name} <{}>", a.email),
                None => a.email.clone(),
            },
        )
        .collect::<Vec<_>>()
        .join(", ")
}

/// Recorta `text` a `BODY_LIMIT` bytes en un límite de carácter. Devuelve si hubo recorte.
fn limit_body(text: &str) -> (&str, bool) {
    if text.len() <= BODY_LIMIT {
        return (text, false);
    }
    let mut end = BODY_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

fn render_body(inner: &Rc<Inner>, body: &MessageBody) {
    let html = body.html.as_deref().filter(|h| !h.trim().is_empty());
    let (segments, remote_images, truncated) = match html {
        Some(html) => {
            let (html, truncated) = limit_body(html);
            (html_to_segments(html), count_images(html), truncated)
        }
        None => match body.text.as_deref() {
            Some(text) => {
                let (text, truncated) = limit_body(text);
                (plain_to_segments(text), 0, truncated)
            }
            None => (Vec::new(), 0, false),
        },
    };
    if truncated {
        inner.banner.set_title(TRUNCATED_NOTICE);
        inner.banner.set_revealed(true);
    } else {
        inner.banner.set_revealed(remote_images > 0);
    }

    let mut links: Vec<LinkTarget> = Vec::new();
    let buffer = if segments.is_empty() {
        let b = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
        b.set_text("Este mensaje no tiene contenido.");
        b
    } else {
        build_buffer(&segments, &mut links)
    };
    *inner.links.borrow_mut() = links;
    inner.text_view.set_buffer(Some(&buffer));
    inner.stack.set_visible_child_name("content");
    #[cfg(feature = "webkit")]
    inner.original.set_visible(html.is_some());

    populate_attachments(inner, body);
}

/// Pulsa «Ver original»: crea la vista bajo demanda y carga el HTML sin sanear.
/// Si ya existe, vuelve al texto y la destruye para liberar memoria.
#[cfg(feature = "webkit")]
fn toggle_original(inner: &Rc<Inner>) {
    if inner.web.borrow().is_some() {
        destroy_original(inner);
        inner.stack.set_visible_child_name("content");
        return;
    }
    let html = inner
        .current
        .borrow()
        .body
        .as_ref()
        .and_then(|b| b.html.clone())
        .filter(|h| !h.trim().is_empty());
    let Some(html) = html else { return };
    match reader_web::OriginalView::new(html.clone()) {
        Ok(view) => {
            inner.stack.add_named(view.widget(), Some("original"));
            view.load();
            inner.stack.set_visible_child_name("original");
            inner.original.set_label("Volver al texto");
            inner
                .banner
                .set_title("Imágenes remotas bloqueadas en la vista original");
            inner
                .banner
                .set_button_label(Some("Cargar imágenes remotas"));
            inner
                .banner
                .set_revealed(reader_html::count_images(&html) > 0);
            *inner.web.borrow_mut() = Some(view);
        }
        Err(e) => alert(&inner.root, "No se pudo abrir la vista original", &e),
    }
}

/// El usuario acepta imágenes remotas: solo se relaja la carga de imágenes https.
#[cfg(feature = "webkit")]
fn allow_remote_images(inner: &Inner) {
    if let Some(view) = inner.web.borrow().as_ref() {
        view.allow_images();
    }
    inner.banner.set_revealed(false);
}

/// Destruye la vista original (si existe) y restaura el banner del modo texto.
#[cfg(feature = "webkit")]
fn destroy_original(inner: &Inner) {
    let Some(view) = inner.web.borrow_mut().take() else {
        return;
    };
    if inner.stack.visible_child_name().as_deref() == Some("original") {
        inner.stack.set_visible_child_name("content");
    }
    inner.stack.remove(view.widget());
    drop(view);
    inner.original.set_label(ORIGINAL_LABEL);
    inner.banner.set_title(BANNER_NORMAL);
    inner.banner.set_button_label(None);
    let remote = inner
        .current
        .borrow()
        .body
        .as_ref()
        .and_then(|b| b.html.as_deref())
        .map_or(0, reader_html::count_images);
    inner.banner.set_revealed(remote > 0);
}

#[cfg(not(feature = "webkit"))]
fn destroy_original(_inner: &Inner) {}

fn clear_attachments(inner: &Inner) {
    while let Some(child) = inner.attachments.first_child() {
        inner.attachments.remove(&child);
    }
    inner.attachments_scroll.set_visible(false);
}

fn populate_attachments(inner: &Rc<Inner>, body: &MessageBody) {
    clear_attachments(inner);
    if body.attachments.is_empty() {
        return;
    }
    let cur = inner.current.borrow().clone();
    let (Some(account), Some(mailbox), Some(header)) = (cur.account, cur.mailbox, cur.header)
    else {
        return;
    };
    for (index, att) in body.attachments.iter().enumerate() {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&att.filename).as_str())
            .subtitle(glib::markup_escape_text(&format!(
                "{} · {}",
                att.mime_type,
                human_size(att.size)
            )))
            .build();
        row.add_prefix(&{
            let icon = gtk::Image::from_icon_name("mail-attachment-symbolic");
            icon.add_css_class("dim-label");
            icon
        });
        let save = gtk::Button::from_icon_name("document-save-symbolic");
        save.set_tooltip_text(Some("Guardar"));
        save.update_property(&[gtk::accessible::Property::Label("Guardar adjunto")]);
        save.set_valign(gtk::Align::Center);
        save.add_css_class("flat");
        let open = gtk::Button::from_icon_name("document-open-symbolic");
        open.set_tooltip_text(Some("Abrir"));
        open.update_property(&[gtk::accessible::Property::Label("Abrir adjunto")]);
        open.set_valign(gtk::Align::Center);
        open.add_css_class("flat");

        let target = AttachmentRef {
            account: account.clone(),
            mailbox: mailbox.clone(),
            uid: header.uid,
            index,
            filename: att.filename.clone(),
        };
        let weak = Rc::downgrade(inner);
        let t = target.clone();
        save.connect_clicked(move |btn| {
            let Some(inner) = weak.upgrade() else { return };
            save_attachment(&inner, btn, t.clone());
        });
        let weak = Rc::downgrade(inner);
        open.connect_clicked(move |btn| {
            let Some(inner) = weak.upgrade() else { return };
            open_attachment(&inner, btn, target.clone());
        });
        row.add_suffix(&save);
        row.add_suffix(&open);
        inner.attachments.append(&row);
    }
    inner.attachments_scroll.set_visible(true);
}

#[derive(Clone)]
struct AttachmentRef {
    account: Account,
    mailbox: Mailbox,
    uid: u32,
    index: usize,
    filename: String,
}

fn save_attachment(inner: &Rc<Inner>, btn: &gtk::Button, att: AttachmentRef) {
    let parent = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    let dialog = gtk::FileDialog::new();
    dialog.set_initial_name(Some(&safe_file_name(&att.filename)));
    let weak = Rc::downgrade(inner);
    dialog.save(parent.as_ref(), None::<&gio::Cancellable>, move |res| {
        // Cancelar el diálogo devuelve error: no es un fallo.
        let Ok(file) = res else { return };
        let Some(path) = file.path() else {
            if let Some(inner) = weak.upgrade() {
                alert(
                    &inner.root,
                    "No se puede guardar",
                    "La ruta elegida no es válida.",
                );
            }
            return;
        };
        let Some(inner) = weak.upgrade() else { return };
        download_to(&inner, att, path);
    });
}

/// Abre el adjunto desde una carpeta privada por apertura. Los tipos que pueden ejecutar
/// código o cargar contenido activo piden confirmación antes.
fn open_attachment(inner: &Rc<Inner>, _btn: &gtk::Button, att: AttachmentRef) {
    let name = safe_file_name(&att.filename);
    let risky = is_risky_extension(&name);
    let weak = Rc::downgrade(inner);
    fetch_attachment_to(
        inner,
        att,
        move |bytes| {
            write_private_attachment(&attachments_dir(), &name, &bytes).map_err(|e| e.to_string())
        },
        move |result| {
            let Some(inner) = weak.upgrade() else { return };
            match result {
                Ok(path) if risky => confirm_open(&inner.root, path),
                Ok(path) => launch_file(&inner.root, &path),
                Err(e) => alert(&inner.root, "No se pudo descargar el adjunto", &e),
            }
        },
    );
}

/// Pregunta antes de abrir un adjunto de tipo activo o ejecutable.
fn confirm_open(widget: &gtk::Box, path: PathBuf) {
    let dialog = adw::AlertDialog::new(
        Some("¿Abrir este archivo?"),
        Some("Puede ejecutar código o cargar contenido activo. Ábrelo solo si confías en el remitente."),
    );
    dialog.add_response("cancel", "Cancelar");
    dialog.add_response("open", "Abrir");
    dialog.set_response_appearance("open", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let target = widget.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "open" {
            launch_file(&target, &path);
        }
    });
    dialog.present(Some(widget));
}

fn launch_file(widget: &impl IsA<gtk::Widget>, path: &Path) {
    let parent = widget.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    gtk::FileLauncher::new(Some(&gio::File::for_path(path))).launch(
        parent.as_ref(),
        None::<&gio::Cancellable>,
        |res| {
            if let Err(e) = res {
                eprintln!("no se pudo abrir el adjunto: {e}");
            }
        },
    );
}

/// Descarga el adjunto en `dest` (solo guardar; errores en un diálogo).
fn download_to(inner: &Rc<Inner>, att: AttachmentRef, dest: PathBuf) {
    let weak = Rc::downgrade(inner);
    fetch_attachment_to(
        inner,
        att,
        move |bytes| std::fs::write(&dest, bytes).map_err(|e| e.to_string()),
        move |result| {
            if let (Err(e), Some(inner)) = (result, weak.upgrade()) {
                alert(&inner.root, "No se pudo guardar el adjunto", &e);
            }
        },
    );
}

/// Pide los bytes al backend y los entrega a `write` fuera del hilo de GTK; `on_done` se
/// ejecuta en el hilo de GTK con el resultado.
fn fetch_attachment_to<T: Send + 'static>(
    inner: &Rc<Inner>,
    att: AttachmentRef,
    write: impl FnOnce(Vec<u8>) -> Result<T, String> + Send + 'static,
    on_done: impl FnOnce(Result<T, String>) + 'static,
) {
    let backend = inner.services.backend(&att.account);
    let handle = inner.services.spawn(async move {
        let bytes = backend
            .fetch_attachment(&att.mailbox, att.uid, att.index)
            .await
            .map_err(|e| e.to_string())?;
        write(bytes)
    });
    glib::MainContext::default().spawn_local(async move {
        on_done(match handle.await {
            Ok(done) => done,
            Err(e) => Err(e.to_string()),
        });
    });
}

/// Carpeta de los adjuntos abiertos: `$XDG_RUNTIME_DIR/correo-adjuntos` (o la equivalente de
/// GLib). Es del usuario y de uso temporal.
fn attachments_dir() -> PathBuf {
    glib::user_runtime_dir().join(ATTACHMENTS_DIR)
}

/// Escribe el adjunto en un subdirectorio nuevo y aleatorio de `base` (0700), con el fichero
/// creado en exclusiva con modo 0600 y sin seguir enlaces simbólicos. Si el nombre ya existe
/// en ese subdirectorio, añade un sufijo `(n)`.
fn write_private_attachment(base: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(base)?;
    std::fs::set_permissions(base, std::fs::Permissions::from_mode(0o700))?;
    let dir = base.join(glib::uuid_string_random().as_str());
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;

    let mut n = 1;
    loop {
        let path = dir.join(numbered_name(name, n));
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path);
        match opened {
            Ok(mut file) => {
                file.write_all(bytes)?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e),
        }
    }
}

/// Nombre para el intento `n` (1 = el original): `informe (2).pdf`, `archivo (2)`.
fn numbered_name(name: &str, n: u32) -> String {
    if n == 1 {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// Borra la carpeta de adjuntos abiertos (best-effort).
fn cleanup_attachments() {
    let _ = std::fs::remove_dir_all(attachments_dir());
}

impl Drop for Inner {
    fn drop(&mut self) {
        cleanup_attachments();
    }
}

/// `true` si la extensión del nombre puede ejecutar código o cargar contenido activo.
fn is_risky_extension(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => RISKY_EXTENSIONS
            .iter()
            .any(|risky| ext.eq_ignore_ascii_case(risky)),
        _ => false,
    }
}

fn alert(parent: &impl IsA<gtk::Widget>, heading: &str, body: &str) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("ok", "Aceptar");
    dialog.present(Some(parent));
}

/// Caracteres de control de dirección bidi (RLO, LRI…): disfrazan la extensión real.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Nombre de fichero seguro: sin rutas, caracteres de control ni bidi, nunca vacío ni oculto.
fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || is_bidi_control(c) || matches!(c, '/' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned: String = cleaned
        .trim()
        .trim_start_matches('.')
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "adjunto".into()
    } else {
        cleaned.to_string()
    }
}

fn human_size(bytes: u64) -> String {
    if bytes < 1_000 {
        format!("{bytes} B")
    } else if bytes < 1_000_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

/// Construye el `TextBuffer` con etiquetas a partir de los segmentos.
/// Las URL de los enlaces se guardan en `links` (tag `link-N` -> `links[N]`).
fn build_buffer(segments: &[Segment], links: &mut Vec<LinkTarget>) -> gtk::TextBuffer {
    let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
    let mut style_tags: HashMap<String, gtk::TextTag> = HashMap::new();
    let mut para_tags: HashMap<String, gtk::TextTag> = HashMap::new();
    let mut bar_tag: Option<gtk::TextTag> = None;

    // Inicio (offset en caracteres) y niveles del párrafo en curso.
    let mut para: Option<(i32, u32, u32)> = None;

    for segment in segments {
        match segment {
            Segment::Text { text, style } => {
                if para.is_none() {
                    let start = buffer.char_count();
                    para = Some((start, style.quote, style.indent));
                    if style.quote > 0 {
                        let tag = bar_tag.get_or_insert_with(|| {
                            let t = gtk::TextTag::new(Some("bar"));
                            t.set_foreground(Some("#77767b"));
                            buffer.tag_table().add(&t);
                            t
                        });
                        let mut end = buffer.end_iter();
                        let bar = format!("{} ", "▎".repeat(style.quote as usize));
                        buffer.insert_with_tags(&mut end, &bar, &[tag]);
                    }
                }
                let mut tags = Vec::new();
                let char_style = Style {
                    quote: 0,
                    indent: 0,
                    link: None,
                    ..style.clone()
                };
                if style.link.is_none() {
                    tags.push(style_tag(&buffer, &mut style_tags, &char_style));
                }
                if let Some(url) = &style.link {
                    let tag = gtk::TextTag::new(Some(&format!("link-{}", links.len())));
                    tag.set_foreground(Some("#1c71d8"));
                    tag.set_underline(gtk::pango::Underline::Single);
                    buffer.tag_table().add(&tag);
                    links.push(LinkTarget {
                        href: url.clone(),
                        shown: text.clone(),
                    });
                    tags.push(tag);
                }
                let mut end = buffer.end_iter();
                let refs: Vec<&gtk::TextTag> = tags.iter().collect();
                buffer.insert_with_tags(&mut end, text, &refs);
            }
            Segment::Paragraph => {
                if let Some((start, quote, indent)) = para.take() {
                    let mut end = buffer.end_iter();
                    buffer.insert(&mut end, "\n");
                    let tag = para_tag(&buffer, &mut para_tags, quote, indent);
                    let from = buffer.iter_at_offset(start);
                    let to = buffer.end_iter();
                    buffer.apply_tag(&tag, &from, &to);
                }
            }
        }
    }
    if let Some((start, quote, indent)) = para.take() {
        let mut end = buffer.end_iter();
        buffer.insert(&mut end, "\n");
        let tag = para_tag(&buffer, &mut para_tags, quote, indent);
        let from = buffer.iter_at_offset(start);
        let to = buffer.end_iter();
        buffer.apply_tag(&tag, &from, &to);
    }
    buffer
}

fn style_tag(
    buffer: &gtk::TextBuffer,
    cache: &mut HashMap<String, gtk::TextTag>,
    style: &Style,
) -> gtk::TextTag {
    let key = format!("{style:?}");
    if let Some(tag) = cache.get(&key) {
        return tag.clone();
    }
    let tag = gtk::TextTag::new(None);
    if style.bold {
        tag.set_weight(700);
    }
    if style.italic {
        tag.set_style(gtk::pango::Style::Italic);
    }
    if style.underline {
        tag.set_underline(gtk::pango::Underline::Single);
    }
    if style.strike {
        tag.set_strikethrough(true);
    }
    if style.mono {
        tag.set_family(Some("monospace"));
    }
    if style.heading {
        tag.set_scale(1.25);
    }
    buffer.tag_table().add(&tag);
    cache.insert(key, tag.clone());
    tag
}

fn para_tag(
    buffer: &gtk::TextBuffer,
    cache: &mut HashMap<String, gtk::TextTag>,
    quote: u32,
    indent: u32,
) -> gtk::TextTag {
    let key = format!("para-{quote}-{indent}");
    if let Some(tag) = cache.get(&key) {
        return tag.clone();
    }
    let tag = gtk::TextTag::new(None);
    // Acotado por `MAX_LEVEL`: la margen nunca desborda.
    let depth = quote.min(MAX_LEVEL) + indent.min(MAX_LEVEL);
    tag.set_left_margin(16 * depth as i32);
    tag.set_pixels_below_lines(8);
    buffer.tag_table().add(&tag);
    cache.insert(key, tag.clone());
    tag
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_file_name_strips_paths_and_hidden_names() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("C:\\docs\\informe.pdf"), "informe.pdf");
        assert_eq!(safe_file_name(".bashrc"), "bashrc");
        assert_eq!(safe_file_name(""), "adjunto");
        assert_eq!(safe_file_name("a\nb.txt"), "a_b.txt");
    }

    #[test]
    fn safe_file_name_removes_bidi_controls_and_limits_length() {
        // RLO (U+202E) oculta la extensión real: se sustituye.
        assert_eq!(safe_file_name("invoice\u{202E}fdp.exe"), "invoice_fdp.exe");
        assert_eq!(safe_file_name("a\u{2066}b\u{2069}.txt"), "a_b_.txt");
        assert_eq!(safe_file_name("..\\..\\x"), "x");
        assert_eq!(
            safe_file_name(&"a".repeat(500)).chars().count(),
            MAX_NAME_CHARS
        );
    }

    #[test]
    fn risky_extensions_are_detected() {
        assert!(is_risky_extension("pagina.HTML"));
        assert!(is_risky_extension("setup.exe"));
        assert!(is_risky_extension("script.tar.sh"));
        assert!(is_risky_extension("app.AppImage"));
        assert!(!is_risky_extension("foto.jpg"));
        assert!(!is_risky_extension("informe.pdf"));
        assert!(!is_risky_extension("sin_extension"));
        assert!(
            !is_risky_extension(".html"),
            "un fichero oculto sin nombre no cuenta"
        );
    }

    #[test]
    fn numbered_names_keep_extension() {
        assert_eq!(numbered_name("informe.pdf", 1), "informe.pdf");
        assert_eq!(numbered_name("informe.pdf", 2), "informe (2).pdf");
        assert_eq!(numbered_name("a.tar.gz", 3), "a.tar (3).gz");
        assert_eq!(numbered_name("LEEME", 2), "LEEME (2)");
    }

    #[test]
    fn private_attachment_is_0600_inside_0700_dir_and_dedupes() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("correo-test-{}", std::process::id()));
        let first = write_private_attachment(&base, "nota.txt", b"uno").unwrap();
        let second = write_private_attachment(&base, "nota.txt", b"dos").unwrap();
        assert_ne!(
            first.parent(),
            second.parent(),
            "cada apertura tiene su carpeta"
        );
        let mode = std::fs::metadata(&first).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = std::fs::metadata(first.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        // Dentro de una misma carpeta, un nombre repetido recibe sufijo.
        let again = write_private_attachment(first.parent().unwrap(), "nota.txt", b"x");
        assert!(again.is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn oversized_bodies_are_cut_on_char_boundary() {
        let text = "ñ".repeat(BODY_LIMIT);
        let (cut, truncated) = limit_body(&text);
        assert!(truncated);
        assert!(cut.len() <= BODY_LIMIT);
        assert!(text.starts_with(cut));
        assert_eq!(limit_body("corto"), ("corto", false));
    }

    #[test]
    fn human_size_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1_500), "1.5 KB");
        assert_eq!(human_size(2_500_000), "2.5 MB");
    }
}

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
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use correo_core::{Account, Address, Flags, Mailbox, MailboxRole, MessageBody, MessageHeader};
use gtk::{gio, glib};

use crate::services::Services;
use crate::ui_types::ComposeMode;
use reader_html::{count_images, html_to_segments, plain_to_segments, Segment, Style};

type ComposeFn = Box<dyn Fn(ComposeMode)>;

const BANNER_NORMAL: &str = "Imágenes bloqueadas para proteger tu privacidad";
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
    /// URLs de los enlaces del cuerpo actual; el tag `link-N` apunta a `links[N]`.
    links: RefCell<Vec<String>>,
    compose: RefCell<Option<ComposeFn>>,
    /// Se avisa tras cambios que afectan a la lista (archivar, borrar, leer o no leer).
    changed: RefCell<Option<Rc<dyn Fn()>>>,

    root: gtk::Box,
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
    /// Alterna entre el texto y la vista original (solo con la feature `webkit`).
    #[cfg(feature = "webkit")]
    original: gtk::Button,
    /// Vista original existente; `None` salvo mientras está visible.
    #[cfg(feature = "webkit")]
    web: RefCell<Option<reader_web::OriginalView>>,
}

pub struct MessageView {
    widget: gtk::Widget,
    inner: Rc<Inner>,
}

impl MessageView {
    pub fn new(services: Rc<Services>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(16);
        root.set_margin_end(16);

        let subject = gtk::Label::new(None);
        subject.add_css_class("title-2");
        subject.set_halign(gtk::Align::Start);
        subject.set_xalign(0.0);
        subject.set_wrap(true);
        subject.set_selectable(true);
        root.append(&subject);

        let meta = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let from = meta_label(&meta);
        let to = meta_label(&meta);
        let cc = meta_label(&meta);
        let date = meta_label(&meta);
        date.add_css_class("dim-label");
        root.append(&meta);

        let reply = action_button(
            "Responder",
            "mail-reply-sender-symbolic",
            "Responder al remitente",
        );
        let reply_all = action_button(
            "Responder a todos",
            "mail-reply-all-symbolic",
            "Responder a todos",
        );
        let forward = action_button("Reenviar", "mail-forward-symbolic", "Reenviar el mensaje");
        let archive = action_button(
            "Archivar",
            "mail-archive-symbolic",
            "Mover a la carpeta de archivo",
        );
        let delete = action_button("Borrar", "user-trash-symbolic", "Borrar el mensaje");
        let mark_unread = action_button(
            "Marcar no leído",
            "mail-unread-symbolic",
            "Marcar como no leído",
        );
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for b in [
            &reply,
            &reply_all,
            &forward,
            &archive,
            &delete,
            &mark_unread,
        ] {
            actions.append(b);
        }
        #[cfg(feature = "webkit")]
        let original = action_button(
            ORIGINAL_LABEL,
            "web-browser-symbolic",
            "Ver el HTML original en una vista aislada (sin scripts ni red)",
        );
        #[cfg(feature = "webkit")]
        {
            original.set_visible(false);
            actions.append(&original);
        }
        let actions_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&actions)
            .build();
        root.append(&actions_scroll);

        let banner = adw::Banner::new(BANNER_NORMAL);
        banner.set_revealed(false);
        root.append(&banner);

        let attachments = gtk::ListBox::new();
        attachments.add_css_class("boxed-list");
        attachments.set_selection_mode(gtk::SelectionMode::None);
        let attachments_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(180)
            .propagate_natural_height(true)
            .visible(false)
            .child(&attachments)
            .build();
        root.append(&attachments_scroll);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);

        let empty = adw::StatusPage::builder()
            .icon_name("mail-read-symbolic")
            .title("Selecciona un mensaje")
            .build();
        stack.add_named(&empty, Some("empty"));

        let spinner = gtk::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_spinning(true);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        spinner.set_hexpand(true);
        spinner.set_vexpand(true);
        stack.add_named(&spinner, Some("loading"));

        let error_page = adw::StatusPage::builder()
            .icon_name("dialog-error-symbolic")
            .title("No se pudo cargar el mensaje")
            .build();
        stack.add_named(&error_page, Some("error"));

        let text_view = gtk::TextView::new();
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        text_view.set_wrap_mode(gtk::WrapMode::WordChar);
        text_view.set_left_margin(4);
        text_view.set_right_margin(4);
        let body_scroll = gtk::ScrolledWindow::builder()
            .child(&text_view)
            .vexpand(true)
            .build();
        stack.add_named(&body_scroll, Some("content"));
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

        let inner = Rc::new(Inner {
            services,
            generation: Cell::new(0),
            current: RefCell::new(Current::default()),
            links: RefCell::new(Vec::new()),
            compose: RefCell::new(None),
            changed: RefCell::new(None),
            root: root.clone(),
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
        inner.error_page.set_child(Some(&retry));

        Self {
            widget: root.upcast(),
            inner,
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.widget
    }

    /// Muestra el mensaje: descarga el cuerpo bajo demanda y marca como leído.
    pub fn show(&self, account: Account, mailbox: Mailbox, header: MessageHeader) {
        show_inner(&self.inner, account, mailbox, header);
    }

    pub fn clear(&self) {
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
    label.set_wrap(true);
    label.set_selectable(true);
    label.add_css_class("caption");
    parent.append(&label);
    label
}

fn action_button(label: &str, icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(label)
        .icon_name(icon)
        .tooltip_text(tooltip)
        .build()
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

/// Clic en un enlace del cuerpo: abre la URL con el lanzador de GTK.
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
        let (bx, by) =
            view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let Some(iter) = view.iter_at_location(bx, by) else {
            return;
        };
        let url = iter.tags().iter().find_map(|tag| {
            let name = tag.name()?;
            let idx: usize = name.strip_prefix("link-")?.parse().ok()?;
            inner.links.borrow().get(idx).cloned()
        });
        if let Some(url) = url {
            let parent = view.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            gtk::UriLauncher::new(&url).launch(parent.as_ref(), None::<&gio::Cancellable>, |res| {
                if let Err(e) = res {
                    eprintln!("no se pudo abrir el enlace: {e}");
                }
            });
        }
    });
    inner.text_view.add_controller(click);
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
    let handle = inner
        .services
        .spawn(async move { backend.fetch_body(&m, uid).await });
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
    let handle = inner
        .services
        .spawn(async move { backend.set_seen(&m, uid, true).await });
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
    let handle = inner.services.spawn(async move {
        match (action, target) {
            (Action::Archive, Some(target)) => backend.move_to(&m, uid, &target).await,
            (Action::Delete, _) => backend.delete(&m, uid).await,
            (Action::MarkUnread, _) => backend.set_seen(&m, uid, false).await,
            _ => Ok(()),
        }
    });
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
    inner.error_page.set_description(Some(&format!(
        "Comprueba la conexión e inténtalo de nuevo.\n{detail}"
    )));
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
    inner
        .from
        .set_text(&format!("De: {}", format_addresses(&header.from)));
    inner
        .to
        .set_text(&format!("Para: {}", format_addresses(&header.to)));
    inner.cc.set_visible(!header.cc.is_empty());
    inner
        .cc
        .set_text(&format!("CC: {}", format_addresses(&header.cc)));
    let date = glib::DateTime::from_unix_local(header.date)
        .ok()
        .and_then(|d| d.format("%d/%m/%Y %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    inner.date.set_text(&date);
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

fn render_body(inner: &Rc<Inner>, body: &MessageBody) {
    let (segments, remote_images) = match body.html.as_deref().filter(|h| !h.trim().is_empty()) {
        Some(html) => (html_to_segments(html), count_images(html)),
        None => match body.text.as_deref() {
            Some(text) => (plain_to_segments(text), 0),
            None => (Vec::new(), 0),
        },
    };
    inner.banner.set_revealed(remote_images > 0);

    let mut links = Vec::new();
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
    inner
        .original
        .set_visible(body.html.as_deref().is_some_and(|h| !h.trim().is_empty()));

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
            .subtitle(format!("{} · {}", att.mime_type, human_size(att.size)))
            .build();
        let save = gtk::Button::from_icon_name("document-save-symbolic");
        save.set_tooltip_text(Some("Guardar"));
        save.set_valign(gtk::Align::Center);
        save.add_css_class("flat");
        let open = gtk::Button::from_icon_name("document-open-symbolic");
        open.set_tooltip_text(Some("Abrir"));
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

fn open_attachment(inner: &Rc<Inner>, btn: &gtk::Button, att: AttachmentRef) {
    let dir = std::env::temp_dir().join("correo-adjuntos");
    let path = dir.join(safe_file_name(&att.filename));
    let parent = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    let weak = Rc::downgrade(inner);
    let p = path.clone();
    let on_done = move |result: Result<(), String>| {
        let Some(inner) = weak.upgrade() else { return };
        match result {
            Ok(()) => {
                let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&p)));
                launcher.launch(parent.as_ref(), None::<&gio::Cancellable>, |res| {
                    if let Err(e) = res {
                        eprintln!("no se pudo abrir el adjunto: {e}");
                    }
                });
            }
            Err(e) => alert(&inner.root, "No se pudo descargar el adjunto", &e),
        }
    };
    fetch_attachment_to(inner, att, path, on_done);
}

/// Descarga el adjunto en `dest` (solo guardar; errores en un diálogo).
fn download_to(inner: &Rc<Inner>, att: AttachmentRef, dest: PathBuf) {
    let weak = Rc::downgrade(inner);
    fetch_attachment_to(inner, att, dest, move |result| {
        if let (Err(e), Some(inner)) = (result, weak.upgrade()) {
            alert(&inner.root, "No se pudo guardar el adjunto", &e);
        }
    });
}

/// Pide los bytes al backend y los escribe en `dest` fuera del hilo de GTK.
fn fetch_attachment_to(
    inner: &Rc<Inner>,
    att: AttachmentRef,
    dest: PathBuf,
    on_done: impl FnOnce(Result<(), String>) + 'static,
) {
    let backend = inner.services.backend(&att.account);
    let handle = inner.services.spawn(async move {
        let bytes = backend
            .fetch_attachment(&att.mailbox, att.uid, att.index)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&dest, bytes).map_err(|e| e.to_string())
    });
    glib::MainContext::default().spawn_local(async move {
        on_done(handle.await);
    });
}

fn alert(parent: &impl IsA<gtk::Widget>, heading: &str, body: &str) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("ok", "Aceptar");
    dialog.present(Some(parent));
}

/// Nombre de fichero seguro: sin rutas ni caracteres de control, nunca vacío ni oculto.
fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.');
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
fn build_buffer(segments: &[Segment], links: &mut Vec<String>) -> gtk::TextBuffer {
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
                    links.push(url.clone());
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
    tag.set_left_margin(16 * (quote as i32 + indent as i32));
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
    fn human_size_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1_500), "1.5 KB");
        assert_eq!(human_size(2_500_000), "2.5 MB");
    }
}

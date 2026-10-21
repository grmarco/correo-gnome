//! T11: compositor (ventana independiente).
//!
//! Módulos hermanos: `composer_format` (modelo y conversión a texto/HTML),
//! `composer_mail` (reglas de asunto, destinatarios e hilos), `composer_editor`
//! (editor con formato), `composer_recipients` (chips de destinatarios) y
//! `composer_attach` (adjuntos).

#[path = "composer_attach.rs"]
mod composer_attach;
#[path = "composer_editor.rs"]
mod composer_editor;
#[path = "composer_format.rs"]
mod composer_format;
#[path = "composer_mail.rs"]
mod composer_mail;
#[path = "composer_recipients.rs"]
mod composer_recipients;

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use correo_core::{Account, Address, Draft, Provider};
use gtk::{gdk, glib};

use crate::services::{flatten, Services};
use crate::ui_types::ComposeMode;
use composer_attach::Attachments;
use composer_editor::Editor;
use composer_format::to_plain_and_html;
use composer_mail::{initial_paragraphs, initial_recipients, subject_for, threading};
use composer_recipients::RecipientField;

// TODO: autoguardado de borradores. Pendiente: guardar el `Draft` en la caché
// (tabla de borradores de `correo-store`) cada N segundos cuando `dirty` esté activo,
// y restaurarlo al abrir un «Nuevo mensaje» vacío.

/// Estado compartido de la ventana. Los widgets y las señales lo referencian por `Rc`.
struct Ctx {
    app: adw::Application,
    window: glib::WeakRef<adw::Window>,
    services: Rc<Services>,
    accounts: Vec<Account>,
    from: Option<gtk::DropDown>,
    mode: ComposeMode,
    subject: gtk::Entry,
    to: RecipientField,
    cc: RecipientField,
    bcc: RecipientField,
    editor: Editor,
    attachments: Attachments,
    banner: adw::Banner,
    toasts: adw::ToastOverlay,
    spinner: adw::Spinner,
    send_button: gtk::Button,
    title: adw::WindowTitle,
    sending: Cell<bool>,
    dirty: Rc<Cell<bool>>,
    allow_close: Cell<bool>,
}

/// Abre una ventana de redacción para `account`.
pub fn open(app: &adw::Application, services: Rc<Services>, account: Account, mode: ComposeMode) {
    let accounts = accounts_for(&services, &account);
    let initial_account = accounts
        .iter()
        .position(|a| a.id == account.id)
        .unwrap_or(0);
    let title_text = match mode {
        ComposeMode::New => "Nuevo mensaje",
        ComposeMode::Reply(..) => "Responder",
        ComposeMode::ReplyAll(..) => "Responder a todos",
        ComposeMode::Forward(..) => "Reenviar",
    };

    let dirty = Rc::new(Cell::new(false));
    let on_change: Rc<dyn Fn()> = {
        let dirty = dirty.clone();
        Rc::new(move || dirty.set(true))
    };

    let toasts = adw::ToastOverlay::new();
    let notify: Rc<dyn Fn(String)> = {
        let toasts = toasts.clone();
        Rc::new(move |msg: String| toasts.add_toast(adw::Toast::new(&msg)))
    };

    let window = adw::Window::builder()
        .application(app)
        .title(title_text)
        .default_width(680)
        .default_height(640)
        .build();
    if let Some(parent) = app.active_window() {
        window.set_transient_for(Some(&parent));
    }

    // Remitente: selector solo si hay varias cuentas.
    let from_widget: gtk::Widget;
    let from = if accounts.len() > 1 {
        let labels: Vec<String> = accounts.iter().map(|a| a.email.clone()).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let dropdown = gtk::DropDown::from_strings(&refs);
        dropdown.set_selected(initial_account as u32);
        dropdown.set_hexpand(true);
        from_widget = dropdown.clone().upcast();
        Some(dropdown)
    } else {
        let label = gtk::Label::new(Some(&account.email));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_selectable(true);
        from_widget = label.upcast();
        None
    };

    let store = services.store.clone();
    let to = RecipientField::new(store.clone(), on_change.clone());
    let cc = RecipientField::new(store.clone(), on_change.clone());
    let bcc = RecipientField::new(store, on_change.clone());
    // Las sugerencias priorizan los contactos de la cuenta remitente; al cambiar De, se
    // reordenan.
    let sender_ids: Vec<correo_core::AccountId> = accounts.iter().map(|a| a.id).collect();
    for field in [&to, &cc, &bcc] {
        field.set_preferred_account(sender_ids.get(initial_account).copied());
    }
    if let Some(dropdown) = &from {
        let fields = [to.clone(), cc.clone(), bcc.clone()];
        dropdown.connect_selected_notify(move |d| {
            let id = sender_ids.get(d.selected() as usize).copied();
            for field in &fields {
                field.set_preferred_account(id);
            }
        });
    }

    let cc_button = gtk::Button::with_label("Cc");
    let bcc_button = gtk::Button::with_label("Cco");
    for (b, tip) in [
        (&cc_button, "Añadir copia"),
        (&bcc_button, "Añadir copia oculta"),
    ] {
        b.add_css_class("flat");
        b.add_css_class("correo-compose-link");
        b.set_valign(gtk::Align::Start);
        b.set_tooltip_text(Some(tip));
    }
    let to_line = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    to_line.append(&to.widget());
    to_line.append(&cc_button);
    to_line.append(&bcc_button);
    // Cc y Cco aparecen con una transición corta de altura.
    let cc_row = reveal(labeled_row("Cc", &cc.widget()));
    let bcc_row = reveal(labeled_row("Cco", &bcc.widget()));

    let subject = gtk::Entry::new();
    subject.set_placeholder_text(Some("Asunto"));
    subject.set_hexpand(true);
    subject.set_has_frame(false);
    subject.add_css_class("correo-compose-entry");
    subject.add_css_class("correo-compose-subject");
    subject.set_text(&subject_for(&mode));
    let subject_row = labeled_row("Asunto", &subject);

    // Destinatarios iniciales (Responder / Responder a todos).
    let recipients = initial_recipients(&mode, &account.email);
    for addr in recipients.to {
        to.add_address(addr);
    }
    for addr in recipients.cc {
        cc.add_address(addr);
    }
    if !cc.addresses().is_empty() {
        cc_row.set_reveal_child(true);
        cc_button.set_visible(false);
    }

    let date_text = format_date(&mode);
    let signature = load_signature();
    let editor = Editor::new();
    editor.set_paragraphs(&initial_paragraphs(&mode, signature.as_deref(), &date_text));
    editor.widget.set_vexpand(true);

    let attachments = Attachments::new(on_change.clone(), notify.clone());
    let attach_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    attach_bar.add_css_class("correo-attach-bar");
    attach_bar.append(&attachments.attach_button(window.upcast_ref(), services.clone()));

    editor.buffer().connect_changed({
        let on_change = on_change.clone();
        move |_| on_change()
    });
    subject.connect_changed({
        let on_change = on_change.clone();
        move |_| on_change()
    });

    let form = gtk::Box::new(gtk::Orientation::Vertical, 0);
    form.add_css_class("correo-compose-form");
    form.append(&labeled_row("De", &from_widget));
    form.append(&labeled_row_box("Para", &to_line));
    form.append(&cc_row);
    form.append(&bcc_row);
    form.append(&subject_row);

    let banner = adw::Banner::new("");
    banner.set_revealed(false);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&banner);
    body.append(&form);
    body.append(&editor.widget);
    body.append(&attach_bar);
    body.append(&attachments.widget);
    attachments.install_drop_target(&body, services.clone());
    toasts.set_child(Some(&body));

    let title = adw::WindowTitle::new(title_text, "");
    let spinner = adw::Spinner::new();
    spinner.set_visible(false);
    let send_button = gtk::Button::new();
    send_button.set_child(Some(
        &adw::ButtonContent::builder()
            .icon_name("mail-send-symbolic")
            .label("Enviar")
            .build(),
    ));
    send_button.add_css_class("suggested-action");
    send_button.add_css_class("correo-compose-button");
    send_button.set_tooltip_text(Some("Enviar (Ctrl+Intro)"));
    send_button.update_property(&[gtk::accessible::Property::Label("Enviar")]);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    header.pack_end(&send_button);
    header.pack_end(&spinner);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&toasts));
    window.set_content(Some(&view));

    let ctx = Rc::new(Ctx {
        app: app.clone(),
        window: window.downgrade(),
        services,
        accounts,
        from,
        mode,
        subject,
        to,
        cc,
        bcc,
        editor,
        attachments,
        banner,
        toasts,
        spinner,
        send_button: send_button.clone(),
        title,
        sending: Cell::new(false),
        dirty: dirty.clone(),
        allow_close: Cell::new(false),
    });

    // Mostrar y ocultar Cc / Cco bajo demanda.
    // Mostrar Cc / Cco bajo demanda. Con `@weak` la señal no retiene sus propios widgets.
    cc_button.connect_clicked(glib::clone!(
        #[weak]
        cc_row,
        #[weak]
        cc_button,
        move |_| {
            cc_row.set_reveal_child(true);
            cc_button.set_visible(false);
        }
    ));
    bcc_button.connect_clicked(glib::clone!(
        #[weak]
        bcc_row,
        #[weak]
        bcc_button,
        move |_| {
            bcc_row.set_reveal_child(true);
            bcc_button.set_visible(false);
        }
    ));

    // Los widgets que pertenecen a `Ctx` y las señales que les conectamos no deben
    // retener `Ctx` con un `Rc`: eso forma un ciclo y la ventana nunca se libera. Por
    // eso los manejadores usan `Weak`. La propia ventana retiene `Ctx` a través de
    // `close_request`, así que `Ctx` vive lo que la ventana.
    {
        let weak = Rc::downgrade(&ctx);
        send_button.connect_clicked(move |_| {
            if let Some(ctx) = weak.upgrade() {
                send(&ctx);
            }
        });
    }
    {
        // Ctrl+Intro envía; en fase de captura para que el editor no inserte salto de línea.
        let weak = Rc::downgrade(&ctx);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, state| {
            let enter = matches!(key, gdk::Key::Return | gdk::Key::KP_Enter);
            if enter && state.contains(gdk::ModifierType::CONTROL_MASK) {
                if let Some(ctx) = weak.upgrade() {
                    send(&ctx);
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(keys);
    }
    {
        let ctx = ctx.clone();
        window.connect_close_request(move |win| close_requested(&ctx, win));
    }

    // Lo escrito durante la preparación no cuenta como cambio.
    dirty.set(false);
    window.present();
    if ctx.mode_is_new() {
        ctx.to.entry().grab_focus();
    } else {
        ctx.editor.view().grab_focus();
    }
}

fn accounts_for(services: &Services, account: &Account) -> Vec<Account> {
    let mut list = services.accounts().unwrap_or_default();
    if !list.iter().any(|a| a.id == account.id) {
        list.insert(0, account.clone());
    }
    list
}

/// Fila «etiqueta: campo» con un widget.
fn labeled_row(label: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("correo-compose-row");
    row.append(&row_label(label));
    row.append(child);
    row
}

/// Envuelve una fila en un `Revealer` oculto que la despliega hacia abajo.
fn reveal(row: gtk::Box) -> gtk::Revealer {
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    revealer.set_transition_duration(160);
    revealer.set_child(Some(&row));
    revealer
}

fn labeled_row_box(label: &str, child: &gtk::Box) -> gtk::Box {
    labeled_row(label, child)
}

fn row_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("correo-compose-label");
    label.set_valign(gtk::Align::Start);
    label.set_xalign(0.0);
    label
}

impl Ctx {
    fn mode_is_new(&self) -> bool {
        matches!(self.mode, ComposeMode::New)
    }

    fn selected_account(&self) -> Account {
        match &self.from {
            Some(dropdown) => self
                .accounts
                .get(dropdown.selected() as usize)
                .unwrap_or(&self.accounts[0])
                .clone(),
            None => self.accounts[0].clone(),
        }
    }

    fn set_sending(&self, on: bool) {
        self.sending.set(on);
        self.spinner.set_visible(on);
        self.send_button.set_sensitive(!on);
        self.title.set_subtitle(if on { "Enviando…" } else { "" });
        if on {
            self.banner.set_revealed(false);
        }
    }

    fn show_error(&self, msg: &str) {
        self.banner.set_title(msg);
        self.banner.set_revealed(true);
    }

    fn build_draft(&self) -> Result<(Account, Draft), String> {
        for (field, label) in [(&self.to, "Para"), (&self.cc, "Cc"), (&self.bcc, "Cco")] {
            if let Err(text) = field.commit_pending() {
                return Err(format!("«{text}» no es una dirección válida ({label})"));
            }
        }
        let (to, cc, bcc) = (
            self.to.addresses(),
            self.cc.addresses(),
            self.bcc.addresses(),
        );
        if to.is_empty() && cc.is_empty() && bcc.is_empty() {
            return Err("Añade al menos un destinatario".to_owned());
        }
        let account = self.selected_account();
        let (text, html) = to_plain_and_html(&self.editor.paragraphs());
        let (in_reply_to, references) = threading(&self.mode);
        let from_name = account.display_name.trim();
        let draft = Draft {
            from: Address {
                name: (!from_name.is_empty()).then(|| from_name.to_owned()),
                email: account.email.clone(),
            },
            to,
            cc,
            bcc,
            subject: self.subject.text().trim().to_owned(),
            text,
            html: Some(html),
            in_reply_to,
            references,
            attachments: self.attachments.snapshot(),
        };
        Ok((account, draft))
    }

    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }
}

/// Envía el mensaje en el runtime de tokio y actualiza la UI al terminar.
fn send(ctx: &Rc<Ctx>) {
    if ctx.sending.get() {
        return;
    }
    let (account, draft) = match ctx.build_draft() {
        Ok(built) => built,
        Err(msg) => {
            ctx.show_error(&msg);
            return;
        }
    };
    ctx.set_sending(true);
    // Gmail guarda la copia en Enviados al enviar por SMTP; Outlook y Microsoft 365 no,
    // así que la copia se añade por IMAP después del envío.
    let sent_copy = needs_sent_copy(&account).then(|| {
        correo_smtp::raw_message(&draft).map_err(|e| format!("no se pudo preparar la copia: {e}"))
    });
    let sender = ctx.services.sender(&account);
    let sent = flatten(ctx.services.spawn(async move { sender.send(&draft).await }));
    let ctx = ctx.clone();
    glib::MainContext::default().spawn_local(async move {
        let result = sent.await;
        ctx.set_sending(false);
        match result {
            Ok(()) => {
                ctx.allow_close.set(true);
                if let Some(window) = ctx.window.upgrade() {
                    window.close();
                }
                toast_in_other_window(&ctx.app, &ctx.window, "Mensaje enviado");
                if let Some(raw) = sent_copy {
                    save_sent_copy(&ctx, &account, raw).await;
                }
            }
            Err(msg) => ctx.show_error(&format!("No se pudo enviar el mensaje: {msg}")),
        }
    });
}

/// Outlook.com y Microsoft 365 no guardan una copia de lo enviado por SMTP.
fn needs_sent_copy(account: &Account) -> bool {
    matches!(account.provider, Provider::Outlook | Provider::Microsoft365)
}

/// Añade la copia del mensaje enviado a Enviados. Un fallo no revierte el envío: solo
/// avisa con un toast en la ventana principal.
async fn save_sent_copy(ctx: &Ctx, account: &Account, raw: Result<Vec<u8>, String>) {
    let saved = match raw {
        Ok(raw) => {
            let backend = ctx.services.imap(account);
            flatten(
                ctx.services
                    .spawn(async move { backend.append_to_sent(&raw).await }),
            )
            .await
            .map_err(|e| e.to_string())
        }
        Err(e) => Err(e),
    };
    if let Err(e) = saved {
        toast_in_other_window(
            &ctx.app,
            &ctx.window,
            &format!("Mensaje enviado, pero no se guardó la copia en Enviados: {e}"),
        );
    }
}

/// Pregunta antes de cerrar con cambios sin enviar. Durante el envío no se cierra.
fn close_requested(ctx: &Rc<Ctx>, win: &adw::Window) -> glib::Propagation {
    if ctx.allow_close.get() || (!ctx.dirty.get() && !ctx.sending.get()) {
        return glib::Propagation::Proceed;
    }
    if ctx.sending.get() {
        ctx.toast("Espera a que termine el envío");
        return glib::Propagation::Stop;
    }
    let ctx = ctx.clone();
    let win = win.clone();
    glib::MainContext::default().spawn_local(async move {
        let dialog = adw::AlertDialog::new(
            Some("¿Descartar el mensaje?"),
            Some("Los cambios que no hayas enviado se perderán."),
        );
        dialog.add_response("cancel", "Seguir editando");
        dialog.add_response("discard", "Descartar");
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let response = dialog.choose_future(Some(&win)).await;
        if response.as_str() == "discard" {
            ctx.allow_close.set(true);
            win.close();
        }
    });
    glib::Propagation::Stop
}

/// Muestra un toast en otra ventana de la aplicación (la principal), si tiene uno.
fn toast_in_other_window(app: &adw::Application, me: &glib::WeakRef<adw::Window>, msg: &str) {
    let me = me.upgrade();
    for win in app.windows() {
        if me
            .as_ref()
            .is_some_and(|m| m.upcast_ref::<gtk::Window>() == &win)
        {
            continue;
        }
        if let Some(overlay) = find_toast_overlay(win.upcast_ref()) {
            overlay.add_toast(adw::Toast::new(msg));
            return;
        }
    }
}

fn find_toast_overlay(widget: &gtk::Widget) -> Option<adw::ToastOverlay> {
    if let Some(overlay) = widget.downcast_ref::<adw::ToastOverlay>() {
        return Some(overlay.clone());
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(found) = find_toast_overlay(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

/// Firma del usuario en `~/.config/correo/signature.txt`, si existe y no está vacía.
fn load_signature() -> Option<String> {
    let path = glib::user_config_dir().join("correo").join("signature.txt");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim_end().to_owned())
        .filter(|s| !s.trim().is_empty())
}

/// Fecha del mensaje citado, en hora local (vacía en mensaje nuevo).
fn format_date(mode: &ComposeMode) -> String {
    let date = match mode {
        ComposeMode::New => return String::new(),
        ComposeMode::Reply(h, _) | ComposeMode::ReplyAll(h, _) | ComposeMode::Forward(h, _) => {
            h.date
        }
    };
    glib::DateTime::from_unix_local(date)
        .ok()
        .and_then(|d| d.format("%d/%m/%Y %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

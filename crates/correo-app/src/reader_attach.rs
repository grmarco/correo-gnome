//! Adjuntos del lector: cada fila tiene estados (reposo → descargando → abriendo / guardado /
//! error / cancelado) y su descarga corre por partes en segundo plano
//! (`MailBackend::fetch_attachment_to`) con progreso y cancelación. Ver `docs/ATTACHMENTS.md`.
//!
//! - Las descargas viven en un registro (`Inner::transfers`): cambiar de mensaje no las
//!   interrumpe y al volver a él la fila retoma su estado.
//! - El progreso llega por un contador atómico que un temporizador de 100 ms (10 Hz) vuelca
//!   a la interfaz; la barra se anima entre lecturas si las animaciones están activas.
//! - Seguridad: los ficheros se crean en una carpeta privada (0700) con modo 0600 y sin
//!   seguir enlaces; las extensiones peligrosas piden confirmación antes de descargar.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use correo_core::{CancelToken, Error, MailBackend, MailboxId, MessageBody, Progress, ProgressFn};
use gtk::{gio, glib};

use super::{
    alert, attachments_dir, create_private_file, is_risky_extension, launch_error_is_cancel,
    safe_file_name, AttachmentRef, Inner,
};

/// Cada cuánto se vuelca el progreso a la interfaz (10 Hz).
const TICK: Duration = Duration::from_millis(100);
/// Duración de la animación de la barra entre dos lecturas.
const BAR_ANIMATION_MS: u32 = 140;
/// Cuánto se muestra «Abriendo…» tras lanzar el fichero.
const OPENING_MS: u64 = 900;

/// Identidad de un adjunto: (carpeta, uid, índice).
pub(super) type AttKey = (MailboxId, u32, usize);

// ----- Texto de progreso (lógica pura) ------------------------------------------------------

/// Tamaño legible con unidades decimales y coma decimal: `512 B`, `1,5 KB`, `3,2 MB`, `12 MB`.
pub(super) fn fmt_size(bytes: u64) -> String {
    let (value, unit) = if bytes < 1_000 {
        return format!("{bytes} B");
    } else if bytes < 1_000_000 {
        (bytes as f64 / 1_000.0, "KB")
    } else if bytes < 1_000_000_000 {
        (bytes as f64 / 1_000_000.0, "MB")
    } else {
        (bytes as f64 / 1_000_000_000.0, "GB")
    };
    let number = if value >= 100.0 {
        format!("{value:.0}")
    } else {
        let one = format!("{value:.1}");
        one.strip_suffix(".0").map_or(one.clone(), str::to_string)
    };
    format!("{} {unit}", number.replace('.', ","))
}

/// Texto de la fila mientras descarga: `3,2 MB de 12 MB · 27 %`; sin total conocido,
/// solo lo recibido (`3,2 MB`).
pub(super) fn progress_text(received: u64, total: Option<u64>) -> String {
    match total.filter(|t| *t > 0) {
        Some(total) => {
            let received = received.min(total);
            // Redondeado, pero «100 %» solo cuando de verdad ha terminado.
            let percent = if received >= total {
                100
            } else {
                ((received as f64 * 100.0 / total as f64).round() as u64).min(99)
            };
            format!(
                "{} de {} · {percent} %",
                fmt_size(received),
                fmt_size(total)
            )
        }
        None => fmt_size(received),
    }
}

/// Cifras que se enseñan: el progreso real va en bytes codificados (base64 pesa ~4/3), pero
/// el usuario ve el tamaño del adjunto decodificado, así que se reescala la fracción a ese
/// tamaño. Sin total conocido se enseña lo recibido tal cual.
pub(super) fn shown_progress(
    decoded_size: u64,
    received: u64,
    total: Option<u64>,
) -> (u64, Option<u64>) {
    match total.filter(|t| *t > 0) {
        Some(total) => {
            let shown_total = if decoded_size > 0 {
                decoded_size
            } else {
                total
            };
            let fraction = (received as f64 / total as f64).clamp(0.0, 1.0);
            ((fraction * shown_total as f64) as u64, Some(shown_total))
        }
        None => (received, None),
    }
}

/// Icono simbólico según el tipo del adjunto.
fn icon_for(mime: &str, name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
    match (mime, ext.as_str()) {
        (_, "pdf" | "doc" | "docx" | "odt" | "rtf") => "x-office-document-symbolic",
        (_, "xls" | "xlsx" | "ods" | "csv") => "x-office-spreadsheet-symbolic",
        (_, "ppt" | "pptx" | "odp") => "x-office-presentation-symbolic",
        (_, "zip" | "gz" | "tar" | "7z" | "rar" | "xz" | "bz2") => "package-x-generic-symbolic",
        (_, "sh" | "exe" | "bat" | "appimage" | "run") => "application-x-executable-symbolic",
        (m, _) if m.starts_with("image/") => "image-x-generic-symbolic",
        (m, _) if m.starts_with("audio/") => "audio-x-generic-symbolic",
        (m, _) if m.starts_with("video/") => "video-x-generic-symbolic",
        (m, _) if m.starts_with("text/") => "text-x-generic-symbolic",
        _ => "mail-attachment-symbolic",
    }
}

// ----- Descargas en curso --------------------------------------------------------------------

/// Qué hacer con el adjunto cuando termine de bajar.
#[derive(Clone)]
enum Kind {
    Open,
    /// Copiar al destino elegido por el usuario.
    Save(PathBuf),
}

enum TState {
    Running,
    /// Terminada: el fichero abierto (temporal) o el guardado.
    Opened(PathBuf),
    Saved(PathBuf),
    Failed(String),
    Cancelled,
}

/// Progreso compartido con la tarea de descarga (que corre en otro hilo).
#[derive(Default)]
struct ProgressCell {
    received: AtomicU64,
    /// 0 = total desconocido.
    total: AtomicU64,
}

impl ProgressCell {
    fn set(&self, p: Progress) {
        self.received.store(p.received, Ordering::Relaxed);
        self.total.store(p.total.unwrap_or(0), Ordering::Relaxed);
    }

    fn get(&self) -> (u64, Option<u64>) {
        let total = self.total.load(Ordering::Relaxed);
        (
            self.received.load(Ordering::Relaxed),
            (total > 0).then_some(total),
        )
    }
}

pub(super) struct Transfer {
    kind: Kind,
    cancel: CancelToken,
    cell: Arc<ProgressCell>,
    state: RefCell<TState>,
    /// Fila que la muestra ahora (puede haberse destruido al cambiar de mensaje).
    ui: RefCell<Weak<RowUi>>,
}

impl Transfer {
    fn running(&self) -> bool {
        matches!(*self.state.borrow(), TState::Running)
    }

    pub(super) fn cancel_all(&self) {
        if self.running() {
            self.cancel.cancel();
        }
    }
}

// ----- Fila ------------------------------------------------------------------------------------

/// Páginas de la pila de acciones de la fila.
const PAGE_IDLE: &str = "idle";
const PAGE_BUSY: &str = "busy";
const PAGE_OPENING: &str = "opening";
const PAGE_SAVED: &str = "saved";
const PAGE_FAILED: &str = "failed";

pub(super) struct RowUi {
    inner: Weak<Inner>,
    key: AttKey,
    target: AttachmentRef,
    decoded_size: u64,
    base_subtitle: String,
    risky: bool,
    row: gtk::ListBoxRow,
    subtitle: gtk::Label,
    stack: gtk::Stack,
    bar: gtk::ProgressBar,
    /// Se está lanzando el fichero («Abriendo…»).
    opening: Cell<bool>,
    animation: RefCell<Option<adw::TimedAnimation>>,
    last_fraction: Cell<f64>,
}

/// Rellena la lista de adjuntos del mensaje actual.
pub(super) fn populate(inner: &Rc<Inner>, body: &MessageBody) {
    clear(inner);
    if body.attachments.is_empty() {
        return;
    }
    let cur = inner.current.borrow().clone();
    let (Some(account), Some(mailbox), Some(header)) = (cur.account, cur.mailbox, cur.header)
    else {
        return;
    };
    for (index, att) in body.attachments.iter().enumerate() {
        let target = AttachmentRef {
            account: account.clone(),
            mailbox: mailbox.clone(),
            uid: header.uid,
            index,
            filename: att.filename.clone(),
        };
        let ui = RowUi::build(
            inner,
            target,
            &att.mime_type,
            att.size,
            (mailbox.id, header.uid, index),
        );
        inner.attachments.append(&ui.row);
        // Si hay una descarga de este adjunto (de una visita anterior al mensaje), la fila la retoma.
        if let Some(t) = inner.transfers.borrow().get(&ui.key) {
            *t.ui.borrow_mut() = Rc::downgrade(&ui);
        }
        ui.render();
        inner.attachment_rows.borrow_mut().push(ui);
    }
    inner.attachments_scroll.set_visible(true);
}

/// Vacía la lista (las descargas en curso siguen en el registro).
pub(super) fn clear(inner: &Inner) {
    while let Some(child) = inner.attachments.first_child() {
        inner.attachments.remove(&child);
    }
    inner.attachment_rows.borrow_mut().clear();
    inner.attachments_scroll.set_visible(false);
}

fn flat_icon_button(icon: &str, tooltip: &str, label: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.update_property(&[gtk::accessible::Property::Label(label)]);
    b.set_valign(gtk::Align::Center);
    b.add_css_class("flat");
    b
}

fn text_button(label: &str, icon: Option<&str>) -> gtk::Button {
    let b = gtk::Button::new();
    let content = adw::ButtonContent::new();
    content.set_label(label);
    if let Some(icon) = icon {
        content.set_icon_name(icon);
    }
    b.set_child(Some(&content));
    b.set_valign(gtk::Align::Center);
    b.add_css_class("flat");
    b.add_css_class("correo-attach-action");
    b
}

impl RowUi {
    fn build(
        inner: &Rc<Inner>,
        target: AttachmentRef,
        mime: &str,
        size: u64,
        key: AttKey,
    ) -> Rc<Self> {
        let name = safe_file_name(&target.filename);
        let base_subtitle = format!("{mime} · {}", fmt_size(size));

        let tile = gtk::Image::from_icon_name(icon_for(mime, &name));
        tile.add_css_class("correo-attach-tile");
        tile.set_pixel_size(18);
        tile.set_valign(gtk::Align::Center);

        let title = gtk::Label::new(Some(&target.filename));
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        title.add_css_class("correo-attach-title");
        let subtitle = gtk::Label::new(Some(&base_subtitle));
        subtitle.set_xalign(0.0);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subtitle.add_css_class("correo-attach-sub");
        subtitle.add_css_class("correo-tabular");
        let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        text.append(&title);
        text.append(&subtitle);

        // Acciones de la derecha, una página por estado.
        let save = flat_icon_button("document-save-symbolic", "Guardar", "Guardar adjunto");
        let open = flat_icon_button("document-open-symbolic", "Abrir", "Abrir adjunto");
        let idle = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        idle.append(&save);
        idle.append(&open);

        let cancel = flat_icon_button(
            "window-close-symbolic",
            "Cancelar la descarga",
            "Cancelar la descarga",
        );
        cancel.add_css_class("circular");

        let spinner = adw::Spinner::new();
        spinner.set_size_request(16, 16);
        spinner.set_valign(gtk::Align::Center);
        spinner.set_margin_end(10);

        let show_folder = text_button("Mostrar en carpeta", Some("folder-open-symbolic"));
        let ok = gtk::Image::from_icon_name("object-select-symbolic");
        ok.add_css_class("correo-attach-ok");
        let saved = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        saved.append(&ok);
        saved.append(&show_folder);

        let retry = text_button("Reintentar", Some("view-refresh-symbolic"));
        retry.add_css_class("correo-attach-retry");

        let stack = gtk::Stack::new();
        stack.set_vhomogeneous(false);
        stack.set_hhomogeneous(false);
        stack.set_valign(gtk::Align::Center);
        stack.add_named(&idle, Some(PAGE_IDLE));
        stack.add_named(&cancel, Some(PAGE_BUSY));
        stack.add_named(&spinner, Some(PAGE_OPENING));
        stack.add_named(&saved, Some(PAGE_SAVED));
        stack.add_named(&retry, Some(PAGE_FAILED));
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(120);

        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        line.add_css_class("correo-attach-line");
        line.append(&tile);
        line.append(&text);
        line.append(&stack);

        let bar = gtk::ProgressBar::new();
        bar.add_css_class("correo-attach-bar");
        bar.set_opacity(0.0);
        bar.update_property(&[gtk::accessible::Property::Label(&format!(
            "Descarga de {}",
            target.filename
        ))]);

        // La barra se superpone al borde inferior: no añade altura a la fila.
        bar.set_valign(gtk::Align::End);
        bar.set_can_target(false);
        let column = gtk::Overlay::new();
        column.set_child(Some(&line));
        column.add_overlay(&bar);
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        row.set_selectable(false);
        row.set_focusable(false);
        row.add_css_class("correo-attach-row");
        row.set_child(Some(&column));

        let ui = Rc::new(RowUi {
            inner: Rc::downgrade(inner),
            key,
            risky: is_risky_extension(&name),
            target,
            decoded_size: size,
            base_subtitle,
            row,
            subtitle,
            stack,
            bar,
            opening: Cell::new(false),
            animation: RefCell::new(None),
            last_fraction: Cell::new(0.0),
        });

        let weak = Rc::downgrade(&ui);
        open.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                RowUi::open(&ui, false);
            }
        });
        let weak = Rc::downgrade(&ui);
        save.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                RowUi::choose_destination(&ui);
            }
        });
        let weak = Rc::downgrade(&ui);
        cancel.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.cancel();
            }
        });
        let weak = Rc::downgrade(&ui);
        retry.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                RowUi::retry(&ui);
            }
        });
        let weak = Rc::downgrade(&ui);
        show_folder.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.reveal_saved();
            }
        });
        ui
    }

    fn transfer(&self) -> Option<Rc<Transfer>> {
        self.inner
            .upgrade()
            .and_then(|i| i.transfers.borrow().get(&self.key).cloned())
    }

    /// Pinta el estado actual de la descarga de este adjunto.
    fn render(&self) {
        let transfer = self.transfer();
        self.row.remove_css_class("correo-attach-failed");
        self.subtitle.remove_css_class("error");
        self.subtitle.set_tooltip_text(None);
        let mut subtitle = self.base_subtitle.clone();
        let mut page = PAGE_IDLE;
        let mut bar = false;
        if self.opening.get() {
            page = PAGE_OPENING;
            bar = true;
            subtitle = "Abriendo…".into();
            self.set_bar(1.0, false);
        } else if let Some(t) = &transfer {
            match &*t.state.borrow() {
                TState::Running => {
                    page = PAGE_BUSY;
                    bar = true;
                    subtitle = self.progress_subtitle(t);
                }
                TState::Saved(path) => {
                    page = PAGE_SAVED;
                    let folder = path
                        .parent()
                        .and_then(Path::file_name)
                        .map(|n| n.to_string_lossy().into_owned());
                    subtitle = match folder {
                        Some(f) => format!("Guardado en {f}"),
                        None => "Guardado".into(),
                    };
                }
                TState::Failed(message) => {
                    page = PAGE_FAILED;
                    subtitle = message.clone();
                    self.row.add_css_class("correo-attach-failed");
                    self.subtitle.add_css_class("error");
                    self.subtitle.set_tooltip_text(Some(message));
                }
                TState::Cancelled => subtitle = "Descarga cancelada".into(),
                TState::Opened(_) => {}
            }
        }
        self.stack.set_visible_child_name(page);
        self.bar.set_opacity(if bar { 1.0 } else { 0.0 });
        if page == PAGE_BUSY {
            self.update_bar(transfer.as_deref());
        }
        self.subtitle.set_text(&subtitle);
    }

    fn progress_subtitle(&self, t: &Transfer) -> String {
        if t.cancel.is_cancelled() {
            return "Cancelando…".into();
        }
        let (received, total) = t.cell.get();
        let (shown, total) = shown_progress(self.decoded_size, received, total);
        match total {
            Some(_) => progress_text(shown, total),
            None if received == 0 => "Descargando…".into(),
            None => format!("Descargando · {}", progress_text(shown, None)),
        }
    }

    /// Fija la fracción de la barra, animándola si las animaciones están activas.
    fn set_bar(&self, fraction: f64, animate: bool) {
        let from = self.last_fraction.replace(fraction);
        if let Some(a) = self.animation.borrow_mut().take() {
            a.skip();
        }
        if !animate || !adw::is_animations_enabled(&self.bar) || (fraction - from).abs() < 0.001 {
            self.bar.set_fraction(fraction);
            return;
        }
        let bar = self.bar.clone();
        let target = adw::CallbackAnimationTarget::new(move |v| bar.set_fraction(v));
        let animation =
            adw::TimedAnimation::new(&self.bar, from, fraction, BAR_ANIMATION_MS, target);
        animation.set_easing(adw::Easing::EaseOutCubic);
        animation.play();
        *self.animation.borrow_mut() = Some(animation);
    }

    fn update_bar(&self, t: Option<&Transfer>) {
        let Some(t) = t else { return };
        let (received, total) = t.cell.get();
        match total {
            Some(total) => self.set_bar(received as f64 / total as f64, true),
            None => self.bar.pulse(),
        }
    }

    /// Un paso del temporizador de progreso: actualiza barra y texto (10 Hz).
    fn tick(&self) {
        let Some(t) = self.transfer() else { return };
        if !t.running() || self.opening.get() {
            return;
        }
        self.subtitle.set_text(&self.progress_subtitle(&t));
        self.update_bar(Some(&t));
    }

    // ----- Acciones -----------------------------------------------------------------------

    fn parent_window(&self) -> Option<gtk::Window> {
        self.row
            .root()
            .and_then(|r| r.downcast::<gtk::Window>().ok())
    }

    /// «Abrir»: reutiliza el fichero ya descargado en esta sesión o lo baja. Las extensiones
    /// peligrosas piden confirmación antes de descargar (`confirmed` la omite en reintentos).
    fn open(this: &Rc<Self>, confirmed: bool) {
        if let Some(t) = this.transfer() {
            if t.running() {
                return;
            }
        }
        if this.risky && !confirmed {
            let weak = Rc::downgrade(this);
            this.confirm_open(move || {
                if let Some(ui) = weak.upgrade() {
                    RowUi::open(&ui, true);
                }
            });
            return;
        }
        if let Some(path) = this.cached_file() {
            this.launch(&path);
            return;
        }
        Self::run(this, Kind::Open);
    }

    /// Fichero temporal ya descargado de este adjunto, si sigue existiendo (y es un fichero
    /// normal, no un enlace).
    fn cached_file(&self) -> Option<PathBuf> {
        let t = self.transfer()?;
        let state = t.state.borrow();
        match &*state {
            TState::Opened(path)
                if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file()) =>
            {
                Some(path.clone())
            }
            _ => None,
        }
    }

    fn confirm_open(&self, then: impl Fn() + 'static) {
        let dialog = adw::AlertDialog::new(
            Some("¿Abrir este archivo?"),
            Some(&format!(
                "«{}» puede ejecutar código o cargar contenido activo. Ábrelo solo si confías en el remitente.",
                safe_file_name(&self.target.filename)
            )),
        );
        dialog.add_response("cancel", "Cancelar");
        dialog.add_response("open", "Abrir");
        dialog.set_response_appearance("open", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.connect_response(None, move |_, response| {
            if response == "open" {
                then();
            }
        });
        dialog.present(Some(&self.row));
    }

    /// «Guardar»: pide el destino y baja el adjunto hasta allí.
    fn choose_destination(this: &Rc<Self>) {
        if this.transfer().is_some_and(|t| t.running()) {
            return;
        }
        // Modo demo (solo depuración): sin diálogo, se guarda en `CORREO_DEMO_SAVE_DIR`.
        #[cfg(debug_assertions)]
        if let Some(dir) = std::env::var_os("CORREO_DEMO_SAVE_DIR") {
            let dest = PathBuf::from(dir).join(safe_file_name(&this.target.filename));
            RowUi::run(this, Kind::Save(dest));
            return;
        }
        let dialog = gtk::FileDialog::new();
        dialog.set_initial_name(Some(&safe_file_name(&this.target.filename)));
        let weak = Rc::downgrade(this);
        let parent = this.parent_window();
        dialog.save(parent.as_ref(), None::<&gio::Cancellable>, move |res| {
            // Cerrar el diálogo devuelve error: no es un fallo.
            let Ok(file) = res else { return };
            let Some(ui) = weak.upgrade() else { return };
            match file.path() {
                Some(path) => RowUi::run(&ui, Kind::Save(path)),
                None => alert(
                    &ui.row,
                    "No se puede guardar",
                    "La ruta elegida no es válida.",
                ),
            }
        });
    }

    fn retry(this: &Rc<Self>) {
        let kind = this.transfer().map_or(Kind::Open, |t| t.kind.clone());
        match kind {
            Kind::Open => RowUi::open(this, true),
            Kind::Save(dest) => RowUi::run(this, Kind::Save(dest)),
        }
    }

    fn cancel(&self) {
        if let Some(t) = self.transfer() {
            // Se atiende entre trozos: mientras tanto la fila dice «Cancelando…».
            t.cancel.cancel();
            self.render();
        }
    }

    fn reveal_saved(&self) {
        let Some(t) = self.transfer() else { return };
        let TState::Saved(path) = &*t.state.borrow() else {
            return;
        };
        let parent = self.parent_window();
        gtk::FileLauncher::new(Some(&gio::File::for_path(path))).open_containing_folder(
            parent.as_ref(),
            None::<&gio::Cancellable>,
            |res| {
                if let Err(e) = res {
                    if !launch_error_is_cancel(&e) {
                        eprintln!("no se pudo mostrar la carpeta: {e}");
                    }
                }
            },
        );
    }

    /// Lanza la descarga en segundo plano y engancha su temporizador de progreso.
    fn run(this: &Rc<Self>, kind: Kind) {
        let Some(inner) = this.inner.upgrade() else {
            return;
        };
        let transfer = Rc::new(Transfer {
            kind: kind.clone(),
            cancel: CancelToken::new(),
            cell: Arc::new(ProgressCell::default()),
            state: RefCell::new(TState::Running),
            ui: RefCell::new(Rc::downgrade(this)),
        });
        inner
            .transfers
            .borrow_mut()
            .insert(this.key, transfer.clone());
        this.opening.set(false);
        this.last_fraction.set(0.0);
        this.bar.set_fraction(0.0);

        let backend = inner.services.backend(&this.target.account);
        let att = this.target.clone();
        let name = safe_file_name(&att.filename);
        let dest = match &kind {
            Kind::Open => None,
            Kind::Save(path) => Some(path.clone()),
        };
        let cell = transfer.cell.clone();
        let progress: ProgressFn = Arc::new(move |p| cell.set(p));
        let cancel = transfer.cancel.clone();
        let handle = inner
            .services
            .spawn(async move { download(backend, att, name, dest, progress, cancel).await });

        this.render();

        // Temporizador de progreso (10 Hz) mientras la descarga corre.
        {
            let weak = Rc::downgrade(this);
            let t = transfer.clone();
            glib::timeout_add_local(TICK, move || {
                if !t.running() {
                    return glib::ControlFlow::Break;
                }
                if let Some(ui) = t.ui.borrow().upgrade().or_else(|| weak.upgrade()) {
                    ui.tick();
                }
                glib::ControlFlow::Continue
            });
        }

        let key = this.key;
        let inner_weak = Rc::downgrade(&inner);
        glib::MainContext::default().spawn_local(async move {
            let result = match handle.await {
                Ok(r) => r,
                Err(e) => Err(Error::Network(e.to_string())),
            };
            RowUi::finish(&inner_weak, key, &transfer, result);
        });
    }

    fn finish(
        inner: &Weak<Inner>,
        key: AttKey,
        transfer: &Rc<Transfer>,
        result: Result<PathBuf, Error>,
    ) {
        let mut open_now = None;
        *transfer.state.borrow_mut() = match result {
            Ok(path) => match transfer.kind {
                Kind::Open => {
                    open_now = Some(path.clone());
                    TState::Opened(path)
                }
                Kind::Save(_) => TState::Saved(path),
            },
            Err(Error::Cancelled) => TState::Cancelled,
            Err(_) if transfer.cancel.is_cancelled() => TState::Cancelled,
            Err(e) => TState::Failed(failure_text(&e)),
        };
        // ¿Sigue ese registro vigente? (un reintento pudo sustituirlo).
        let current = inner.upgrade().is_some_and(|i| {
            i.transfers
                .borrow()
                .get(&key)
                .is_some_and(|t| Rc::ptr_eq(t, transfer))
        });
        let ui = transfer.ui.borrow().upgrade();
        match (current, ui, open_now) {
            (true, Some(ui), Some(path)) => {
                ui.set_bar(1.0, false);
                ui.launch(&path);
            }
            (true, Some(ui), None) => ui.render(),
            _ => {}
        }
    }

    /// Abre el fichero con la aplicación predeterminada y muestra «Abriendo…» un instante.
    fn launch(&self, path: &Path) {
        self.opening.set(true);
        self.render();
        // Modo demo (solo depuración): no se lanza nada y «Abriendo…» dura más, para verlo.
        #[cfg(debug_assertions)]
        let (skip_launch, opening_ms) = if std::env::var_os("CORREO_DEMO_NO_LAUNCH").is_some() {
            (true, 4000)
        } else {
            (false, OPENING_MS)
        };
        #[cfg(not(debug_assertions))]
        let (skip_launch, opening_ms) = (false, OPENING_MS);
        let weak_inner = self.inner.clone();
        let key = self.key;
        let parent = self.parent_window();
        let path_owned = path.to_path_buf();
        if !skip_launch {
            gtk::FileLauncher::new(Some(&gio::File::for_path(path))).launch(
                parent.as_ref(),
                None::<&gio::Cancellable>,
                move |res| {
                    let Err(e) = res else { return };
                    // Nadie ha abierto el fichero: se borra la copia temporal y se olvida.
                    let _ = std::fs::remove_file(&path_owned);
                    let Some(inner) = weak_inner.upgrade() else {
                        return;
                    };
                    let failure = if launch_error_is_cancel(&e) {
                        // Cerrar el diálogo del portal es una cancelación, no un fallo.
                        inner.transfers.borrow_mut().remove(&key);
                        None
                    } else {
                        eprintln!("no se pudo abrir el adjunto: {e}");
                        Some("No se pudo abrir el archivo con ninguna aplicación".to_string())
                    };
                    if let Some(t) = inner.transfers.borrow().get(&key) {
                        if let Some(message) = failure {
                            *t.state.borrow_mut() = TState::Failed(message);
                        } else {
                            *t.state.borrow_mut() = TState::Cancelled;
                        }
                    }
                    let ui = inner
                        .attachment_rows
                        .borrow()
                        .iter()
                        .find(|u| u.key == key)
                        .cloned();
                    if let Some(ui) = ui {
                        ui.opening.set(false);
                        ui.render();
                    }
                },
            );
        }
        let weak = self.row.downgrade();
        let inner = self.inner.clone();
        glib::timeout_add_local_once(Duration::from_millis(opening_ms), move || {
            if weak.upgrade().is_none() {
                return;
            }
            let Some(inner) = inner.upgrade() else { return };
            let ui = inner
                .attachment_rows
                .borrow()
                .iter()
                .find(|u| u.key == key)
                .cloned();
            if let Some(ui) = ui {
                ui.opening.set(false);
                ui.render();
            }
        });
    }
}

/// Texto breve de un fallo de descarga para la fila (el detalle completo va en el tooltip).
fn failure_text(e: &Error) -> String {
    format!("No se pudo descargar · {e}")
}

/// Baja el adjunto a un fichero privado (0700/0600) y, si es «Guardar», lo copia al destino
/// elegido: así un fallo o una cancelación no dejan a medias un fichero del usuario.
async fn download(
    backend: Arc<dyn MailBackend>,
    att: AttachmentRef,
    name: String,
    dest: Option<PathBuf>,
    progress: ProgressFn,
    cancel: CancelToken,
) -> Result<PathBuf, Error> {
    let temp = create_private_file(&attachments_dir(), &name)
        .map_err(|e| Error::Store(format!("preparando la descarga: {e}")))?;
    let result = backend
        .fetch_attachment_to(&att.mailbox, att.uid, att.index, &temp, progress, cancel)
        .await;
    if let Err(e) = result {
        remove_temp(&temp);
        return Err(e);
    }
    let Some(dest) = dest else { return Ok(temp) };
    let copied = copy_to(&temp, &dest);
    remove_temp(&temp);
    copied
        .map(|()| dest.clone())
        .map_err(|e| Error::Store(format!("guardando en {}: {e}", dest.display())))
}

/// Copia `from` en `to` (que se crea o se trunca, con los permisos habituales del usuario).
fn copy_to(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut src = std::fs::File::open(from)?;
    let mut dst = std::fs::File::create(to)?;
    std::io::copy(&mut src, &mut dst)?;
    Ok(())
}

/// Borra el fichero temporal y su carpeta privada (best-effort).
fn remove_temp(temp: &Path) {
    let _ = std::fs::remove_file(temp);
    if let Some(dir) = temp.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_decimal_units_and_a_decimal_comma() {
        assert_eq!(fmt_size(0), "0 B");
        assert_eq!(fmt_size(512), "512 B");
        assert_eq!(fmt_size(1_500), "1,5 KB");
        assert_eq!(fmt_size(56_400), "56,4 KB");
        assert_eq!(fmt_size(248_000), "248 KB");
        assert_eq!(fmt_size(3_200_000), "3,2 MB");
        assert_eq!(fmt_size(12_000_000), "12 MB");
        assert_eq!(fmt_size(2_500_000_000), "2,5 GB");
    }

    #[test]
    fn progress_text_shows_received_total_and_percent() {
        assert_eq!(
            progress_text(3_200_000, Some(12_000_000)),
            "3,2 MB de 12 MB · 27 %"
        );
        assert_eq!(progress_text(0, Some(12_000_000)), "0 B de 12 MB · 0 %");
        assert_eq!(
            progress_text(12_000_000, Some(12_000_000)),
            "12 MB de 12 MB · 100 %"
        );
        // Nunca pasa del 100 % aunque lo recibido supere al total estimado.
        assert_eq!(
            progress_text(13_000_000, Some(12_000_000)),
            "12 MB de 12 MB · 100 %"
        );
        assert_eq!(
            progress_text(11_990_000, Some(12_000_000)),
            "12 MB de 12 MB · 99 %"
        );
    }

    #[test]
    fn unknown_total_shows_only_what_was_received() {
        assert_eq!(progress_text(3_200_000, None), "3,2 MB");
        assert_eq!(progress_text(3_200_000, Some(0)), "3,2 MB");
    }

    #[test]
    fn shown_figures_are_rescaled_to_the_decoded_size() {
        // base64: 16 MB en el cable para un adjunto de 12 MB; a mitad, 6 de 12.
        assert_eq!(
            shown_progress(12_000_000, 8_000_000, Some(16_000_000)),
            (6_000_000, Some(12_000_000))
        );
        // Sin tamaño decodificado se usa el del cable.
        assert_eq!(
            shown_progress(0, 4_000_000, Some(16_000_000)),
            (4_000_000, Some(16_000_000))
        );
        assert_eq!(shown_progress(12_000_000, 700, None), (700, None));
    }

    #[test]
    fn icons_follow_the_file_type() {
        assert_eq!(
            icon_for("application/pdf", "a.PDF"),
            "x-office-document-symbolic"
        );
        assert_eq!(icon_for("image/png", "foto"), "image-x-generic-symbolic");
        assert_eq!(
            icon_for("application/zip", "x.zip"),
            "package-x-generic-symbolic"
        );
        assert_eq!(
            icon_for("application/octet-stream", "raro"),
            "mail-attachment-symbolic"
        );
    }
}

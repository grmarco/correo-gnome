//! Adjuntos del compositor: lista con quitar, selector de archivos y arrastrar y soltar.
//! Los archivos se leen en el runtime de tokio y el límite total es de 25 MB.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::composer_mail::{fits_attachment_limit, MAX_ATTACHMENTS_BYTES};
use crate::services::Services;

#[derive(Clone)]
pub struct Item {
    pub id: u64,
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

struct Inner {
    items: RefCell<Vec<Item>>,
    next_id: Cell<u64>,
    summary: gtk::Label,
    list: gtk::Box,
    on_change: Rc<dyn Fn()>,
    notify: Rc<dyn Fn(String)>,
}

/// Lista de adjuntos. Clonable: los clones comparten estado.
#[derive(Clone)]
pub struct Attachments {
    inner: Rc<Inner>,
    pub widget: gtk::Box,
}

impl Attachments {
    /// `on_change` tras cualquier cambio; `notify` muestra avisos (límite, lectura fallida).
    pub fn new(on_change: Rc<dyn Fn()>, notify: Rc<dyn Fn(String)>) -> Self {
        let summary = gtk::Label::new(None);
        summary.add_css_class("dim-label");
        summary.set_xalign(0.0);
        summary.set_hexpand(true);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        list.set_visible(false);

        let inner = Rc::new(Inner {
            items: RefCell::new(Vec::new()),
            next_id: Cell::new(0),
            summary: summary.clone(),
            list: list.clone(),
            on_change,
            notify,
        });
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 6);
        widget.set_margin_start(12);
        widget.set_margin_end(12);
        widget.set_margin_bottom(8);
        widget.append(&summary);
        widget.append(&list);
        Self { inner, widget }
    }

    /// Botón «Adjuntar…» que abre el selector de archivos nativo.
    pub fn attach_button(&self, window: &gtk::Window, services: Rc<Services>) -> gtk::Button {
        let button = gtk::Button::with_label("Adjuntar…");
        button.set_icon_name("mail-attachment-symbolic");
        button.add_css_class("flat");
        let me = self.clone();
        let window = window.clone();
        button.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Adjuntar archivos");
            let me = me.clone();
            let services = services.clone();
            let window = window.clone();
            glib::MainContext::default().spawn_local(async move {
                let Ok(files) = dialog.open_multiple_future(Some(&window)).await else {
                    return;
                };
                let paths = files_to_paths(&files);
                me.add_paths(services, paths);
            });
        });
        button
    }

    /// Acepta archivos arrastrados desde el gestor de archivos sobre `target`.
    pub fn install_drop_target(&self, target: &impl IsA<gtk::Widget>, services: Rc<Services>) {
        let drop = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
        drop.set_types(&[gio::File::static_type(), gdk::FileList::static_type()]);
        let me = self.clone();
        drop.connect_drop(move |_, value, _, _| {
            let paths: Vec<PathBuf> = if let Ok(list) = value.get::<gdk::FileList>() {
                list.files().iter().filter_map(|f| f.path()).collect()
            } else if let Ok(file) = value.get::<gio::File>() {
                file.path().into_iter().collect()
            } else {
                return false;
            };
            if paths.is_empty() {
                (me.inner.notify)("Solo se pueden adjuntar archivos locales".to_owned());
                return false;
            }
            me.add_paths(services.clone(), paths);
            true
        });
        target.add_controller(drop);
    }

    /// Lee los archivos en segundo plano y los añade cuando están listos.
    pub fn add_paths(&self, services: Rc<Services>, paths: Vec<PathBuf>) {
        for path in paths {
            let read = services.spawn(async move { read_file(path) });
            let me = self.clone();
            glib::MainContext::default().spawn_local(async move {
                match read.await {
                    Ok(Ok(item)) => {
                        if let Err(msg) = me.add(item) {
                            (me.inner.notify)(msg);
                        }
                    }
                    Ok(Err(msg)) => (me.inner.notify)(msg),
                    Err(e) => (me.inner.notify)(e.to_string()),
                }
            });
        }
    }

    /// Añade un adjunto si cabe en el límite total.
    pub fn add(&self, mut item: Item) -> Result<(), String> {
        if !fits_attachment_limit(self.total(), item.data.len() as u64) {
            return Err(format!(
                "No se añade «{}»: los adjuntos superarían el límite de {}",
                item.name,
                glib::format_size(MAX_ATTACHMENTS_BYTES)
            ));
        }
        item.id = self.inner.next_id.get();
        self.inner.next_id.set(item.id + 1);
        self.inner.items.borrow_mut().push(item);
        self.inner.rebuild();
        (self.inner.on_change)();
        Ok(())
    }

    pub fn total(&self) -> u64 {
        self.inner
            .items
            .borrow()
            .iter()
            .map(|i| i.data.len() as u64)
            .sum()
    }

    /// Copia de los adjuntos como `(nombre, tipo MIME, contenido)` para el `Draft`.
    pub fn snapshot(&self) -> Vec<(String, String, Vec<u8>)> {
        self.inner
            .items
            .borrow()
            .iter()
            .map(|i| (i.name.clone(), i.mime.clone(), i.data.clone()))
            .collect()
    }
}

impl Inner {
    fn remove(self: &Rc<Self>, id: u64) {
        self.items.borrow_mut().retain(|i| i.id != id);
        self.rebuild();
        (self.on_change)();
    }

    fn rebuild(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let items = self.items.borrow();
        for item in items.iter() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.set_margin_top(2);
            row.set_margin_bottom(2);
            let icon = gtk::Image::from_icon_name("mail-attachment-symbolic");
            let name = gtk::Label::new(Some(&item.name));
            name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            name.set_hexpand(true);
            name.set_xalign(0.0);
            name.set_tooltip_text(Some(&item.name));
            let size = gtk::Label::new(Some(&glib::format_size(item.data.len() as u64)));
            size.add_css_class("dim-label");
            let remove = gtk::Button::from_icon_name("window-close-symbolic");
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some("Quitar adjunto"));
            // `Weak`: la fila pertenece a `Inner`; un `Rc` aquí sería un ciclo.
            let weak = Rc::downgrade(self);
            let id = item.id;
            remove.connect_clicked(move |_| {
                if let Some(me) = weak.upgrade() {
                    me.remove(id);
                }
            });
            row.append(&icon);
            row.append(&name);
            row.append(&size);
            row.append(&remove);
            self.list.append(&row);
        }
        self.list.set_visible(!items.is_empty());
        let total: u64 = items.iter().map(|i| i.data.len() as u64).sum();
        self.summary.set_text(&if items.is_empty() {
            String::new()
        } else {
            format!(
                "{} adjunto(s), {} de {}",
                items.len(),
                glib::format_size(total),
                glib::format_size(MAX_ATTACHMENTS_BYTES)
            )
        });
    }
}

fn files_to_paths(files: &gio::ListModel) -> Vec<PathBuf> {
    (0..files.n_items())
        .filter_map(|i| files.item(i))
        .filter_map(|o| o.downcast::<gio::File>().ok())
        .filter_map(|f| f.path())
        .collect()
}

/// Lectura bloqueante de un archivo; se ejecuta en el runtime de tokio.
fn read_file(path: PathBuf) -> Result<Item, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "adjunto".to_owned());
    let size = std::fs::metadata(&path)
        .map_err(|e| format!("«{name}»: {e}"))?
        .len();
    if size > MAX_ATTACHMENTS_BYTES {
        return Err(format!(
            "«{name}» supera el límite de {}",
            glib::format_size(MAX_ATTACHMENTS_BYTES)
        ));
    }
    let data = std::fs::read(&path).map_err(|e| format!("«{name}»: {e}"))?;
    let mime = mime_for(&path);
    Ok(Item {
        id: 0,
        name,
        mime,
        data,
    })
}

fn mime_for(path: &Path) -> String {
    let (content_type, _) = gio::content_type_guess(Some(path), None::<&[u8]>);
    gio::content_type_get_mime_type(&content_type)
        .map(|m| m.to_string())
        .unwrap_or_else(|| "application/octet-stream".to_owned())
}

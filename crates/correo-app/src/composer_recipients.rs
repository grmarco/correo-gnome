//! Campo de destinatarios: «chips» de direcciones y una entrada que muestra
//! sugerencias de contactos (SQLite local, consultada en el hilo principal).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use correo_core::{Address, Contact};
use correo_store::Store;
use gtk::{gdk, glib};

use super::composer_mail::{display_name, parse_recipient};

const SUGGESTIONS: usize = 8;
const SEPARATORS: [char; 2] = [',', ';'];

struct Chip {
    id: u64,
    addr: Address,
    widget: gtk::Box,
}

struct Inner {
    flow: gtk::FlowBox,
    entry: gtk::Entry,
    chips: RefCell<Vec<Chip>>,
    next_id: Cell<u64>,
    /// Evita re-entrar en `changed` al reescribir el texto de la entrada.
    busy: Cell<bool>,
    popover: gtk::Popover,
    list: gtk::ListBox,
    suggestions: RefCell<Vec<Contact>>,
    store: Arc<Mutex<Store>>,
    on_change: Rc<dyn Fn()>,
}

/// Campo de destinatarios. Clonable: los clones comparten estado.
#[derive(Clone)]
pub struct RecipientField {
    inner: Rc<Inner>,
}

/// Recupera el campo mientras siga vivo (para los manejadores de señal con `Weak`).
fn upgrade(weak: &Weak<Inner>) -> Option<RecipientField> {
    weak.upgrade().map(|inner| RecipientField { inner })
}

impl RecipientField {
    /// `on_change` se llama cuando cambian los destinatarios o el texto escrito.
    pub fn new(store: Arc<Mutex<Store>>, on_change: Rc<dyn Fn()>) -> Self {
        let flow = gtk::FlowBox::new();
        flow.set_selection_mode(gtk::SelectionMode::None);
        flow.set_activate_on_single_click(false);
        flow.set_homogeneous(false);
        flow.set_hexpand(true);
        flow.set_column_spacing(4);
        flow.set_row_spacing(4);

        let entry = gtk::Entry::new();
        entry.set_has_frame(false);
        entry.set_hexpand(true);
        entry.set_width_chars(16);
        flow.append(&entry);

        // Las sugerencias no toman el foco: así la entrada no pierde el foco al pulsar una.
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_focusable(false);
        let popover = gtk::Popover::new();
        popover.set_autohide(false);
        popover.set_has_arrow(false);
        popover.set_position(gtk::PositionType::Bottom);
        popover.set_child(Some(&list));
        popover.set_parent(&entry);

        let field = Self {
            inner: Rc::new(Inner {
                flow,
                entry: entry.clone(),
                chips: RefCell::new(Vec::new()),
                next_id: Cell::new(0),
                busy: Cell::new(false),
                popover,
                list: list.clone(),
                suggestions: RefCell::new(Vec::new()),
                store,
                on_change,
            }),
        };

        // Las señales usan `Weak`: los widgets pertenecen a `Inner`, y un `Rc` aquí
        // formaría un ciclo que impediría liberar el campo al cerrar el compositor.
        let weak = Rc::downgrade(&field.inner);
        entry.connect_changed({
            let weak = weak.clone();
            move |_| {
                if let Some(me) = upgrade(&weak) {
                    me.on_text_changed();
                }
            }
        });

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let weak = weak.clone();
            move |_, key, _, _| match upgrade(&weak) {
                Some(me) => me.on_key(key),
                None => glib::Propagation::Proceed,
            }
        });
        entry.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave({
            let weak = weak.clone();
            move |_| {
                if let Some(me) = upgrade(&weak) {
                    me.commit_pending_quietly();
                    me.inner.popover.popdown();
                }
            }
        });
        entry.add_controller(focus);

        list.connect_row_activated(move |_, row| {
            if let Some(me) = upgrade(&weak) {
                me.pick_suggestion(row.index() as usize);
            }
        });

        field
    }

    pub fn widget(&self) -> gtk::FlowBox {
        self.inner.flow.clone()
    }

    pub fn entry(&self) -> gtk::Entry {
        self.inner.entry.clone()
    }

    pub fn addresses(&self) -> Vec<Address> {
        self.inner
            .chips
            .borrow()
            .iter()
            .map(|c| c.addr.clone())
            .collect()
    }

    pub fn add_address(&self, addr: Address) {
        self.inner.add_chip(addr);
    }

    /// Convierte el texto pendiente en chip. `Err` con el texto si no es una dirección válida.
    pub fn commit_pending(&self) -> Result<(), String> {
        let text = self.inner.entry.text().to_string();
        if text.trim().is_empty() {
            return Ok(());
        }
        if self.inner.commit_str(&text) {
            self.inner.set_entry_text("");
            self.inner.set_error(false);
            Ok(())
        } else {
            self.inner.set_error(true);
            Err(text.trim().to_owned())
        }
    }

    fn commit_pending_quietly(&self) {
        let _ = self.commit_pending();
    }

    fn on_text_changed(&self) {
        if self.inner.busy.get() {
            return;
        }
        self.inner.set_error(false);
        // Un separador (coma o punto y coma) cierra el destinatario escrito antes de él.
        while let Some(pos) = self.inner.entry.text().find(SEPARATORS) {
            let text = self.inner.entry.text().to_string();
            let (before, after) = (&text[..pos], &text[pos + 1..]);
            if !self.inner.commit_str(before) {
                self.inner.set_error(true);
                break;
            }
            self.inner.set_entry_text(after);
        }
        (self.inner.on_change)();
        self.update_suggestions();
    }

    fn on_key(&self, key: gdk::Key) -> glib::Propagation {
        let inner = &self.inner;
        let handled = match key {
            gdk::Key::Return | gdk::Key::KP_Enter => {
                match self.selected_suggestion() {
                    Some(index) => self.pick_suggestion(index),
                    None => self.commit_pending_quietly(),
                }
                true
            }
            gdk::Key::Tab | gdk::Key::ISO_Left_Tab => match self.selected_suggestion() {
                Some(index) => {
                    self.pick_suggestion(index);
                    true
                }
                None => {
                    self.commit_pending_quietly();
                    false
                }
            },
            gdk::Key::Down | gdk::Key::Up if inner.popover.is_visible() => {
                self.move_selection(key == gdk::Key::Down);
                true
            }
            gdk::Key::Escape if inner.popover.is_visible() => {
                inner.popover.popdown();
                true
            }
            gdk::Key::BackSpace if inner.entry.text().is_empty() => {
                let last = inner.chips.borrow().last().map(|c| c.id);
                if let Some(id) = last {
                    inner.remove_chip(id);
                }
                true
            }
            _ => false,
        };
        if handled {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    }

    fn selected_suggestion(&self) -> Option<usize> {
        if !self.inner.popover.is_visible() {
            return None;
        }
        self.inner.list.selected_row().map(|r| r.index() as usize)
    }

    fn move_selection(&self, down: bool) {
        let count = self.inner.suggestions.borrow().len() as i32;
        if count == 0 {
            return;
        }
        let current = self.inner.list.selected_row().map_or(-1, |r| r.index());
        let next = if down {
            (current + 1).min(count - 1)
        } else {
            (current - 1).max(0)
        };
        if let Some(row) = self.inner.list.row_at_index(next) {
            self.inner.list.select_row(Some(&row));
        }
    }

    fn pick_suggestion(&self, index: usize) {
        let contact = self.inner.suggestions.borrow().get(index).cloned();
        if let Some(c) = contact {
            self.inner.add_chip(Address {
                name: c.name,
                email: c.email,
            });
            self.inner.set_entry_text("");
            self.inner.popover.popdown();
            self.inner.entry.grab_focus();
        }
    }

    /// Consulta contactos que empiezan por lo escrito (síncrono: SQLite local).
    fn update_suggestions(&self) {
        let inner = &self.inner;
        let text = inner.entry.text().to_string();
        let query = text.trim();
        if query.is_empty() || query.contains(SEPARATORS) {
            inner.popover.popdown();
            return;
        }
        let found = match inner.store.lock() {
            Ok(store) => store
                .search_contacts(query, SUGGESTIONS)
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let chosen: Vec<String> = inner
            .chips
            .borrow()
            .iter()
            .map(|c| c.addr.email.to_lowercase())
            .collect();
        let found: Vec<Contact> = found
            .into_iter()
            .filter(|c| !chosen.contains(&c.email.to_lowercase()))
            .collect();

        while let Some(row) = inner.list.row_at_index(0) {
            inner.list.remove(&row);
        }
        if found.is_empty() {
            inner.suggestions.borrow_mut().clear();
            inner.popover.popdown();
            return;
        }
        for c in &found {
            let label = match c.name.as_deref().filter(|n| !n.trim().is_empty()) {
                Some(name) => format!("{name} <{}>", c.email),
                None => c.email.clone(),
            };
            let text = gtk::Label::new(Some(&label));
            text.set_xalign(0.0);
            text.set_margin_start(8);
            text.set_margin_end(8);
            text.set_margin_top(4);
            text.set_margin_bottom(4);
            let row = gtk::ListBoxRow::new();
            row.set_focusable(false);
            row.set_child(Some(&text));
            inner.list.append(&row);
        }
        *inner.suggestions.borrow_mut() = found;
        if !inner.popover.is_visible() {
            inner.popover.popup();
        }
    }
}

impl Inner {
    fn set_entry_text(&self, text: &str) {
        self.busy.set(true);
        self.entry.set_text(text);
        self.busy.set(false);
    }

    fn set_error(&self, on: bool) {
        if on {
            self.entry.add_css_class("error");
        } else {
            self.entry.remove_css_class("error");
        }
    }

    /// Convierte `text` en chip si es una dirección válida (o está vacío).
    fn commit_str(self: &Rc<Self>, text: &str) -> bool {
        let t = text.trim();
        if t.is_empty() {
            return true;
        }
        match parse_recipient(t) {
            Some(addr) => {
                self.add_chip(addr);
                true
            }
            None => false,
        }
    }

    fn add_chip(self: &Rc<Self>, addr: Address) {
        let id = self.next_id.get();
        self.next_id.set(id + 1);

        let label = gtk::Label::new(Some(display_name(&addr)));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(30);
        let remove = gtk::Button::from_icon_name("window-close-symbolic");
        remove.add_css_class("flat");
        remove.add_css_class("circular");
        remove.set_tooltip_text(Some("Quitar destinatario"));
        let weak = Rc::downgrade(self);
        remove.connect_clicked(move |_| {
            if let Some(me) = weak.upgrade() {
                me.remove_chip(id);
            }
        });

        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        chip.add_css_class("card");
        chip.set_margin_start(2);
        chip.set_margin_end(2);
        chip.set_margin_top(2);
        chip.set_margin_bottom(2);
        chip.set_tooltip_text(Some(&addr.email));
        chip.append(&label);
        chip.append(&remove);

        // La entrada es el último hijo del flujo; los chips van justo antes.
        let position = self.chips.borrow().len() as i32;
        self.flow.insert(&chip, position);
        self.chips.borrow_mut().push(Chip {
            id,
            addr,
            widget: chip,
        });
        (self.on_change)();
    }

    fn remove_chip(&self, id: u64) {
        let mut chips = self.chips.borrow_mut();
        if let Some(pos) = chips.iter().position(|c| c.id == id) {
            let chip = chips.remove(pos);
            if let Some(child) = chip.widget.parent() {
                self.flow.remove(&child);
            }
            drop(chips);
            (self.on_change)();
        }
    }
}

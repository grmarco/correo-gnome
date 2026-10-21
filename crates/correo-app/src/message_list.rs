//! T9: lista central de hilos. `gtk::ListView` virtualizada sobre un `gio::ListStore`
//! de `glib::BoxedAnyObject` que envuelven `ThreadRow`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use correo_core::{Address, MessageHeader, MessageId};
use correo_threads::build_threads;
use gtk::{gio, glib};

use crate::ui_style;

/// Un hilo tal como se muestra: su último mensaje y el resumen del conjunto.
#[derive(Debug, Clone)]
pub struct ThreadRow {
    pub last: MessageHeader,
    pub subject: String,
    pub count: usize,
    pub unread: usize,
    pub has_attachments: bool,
    /// Todos los mensajes del hilo que hay en la lista cargada (orden cronológico).
    pub message_ids: Vec<MessageId>,
}

/// ¿Dos filas se muestran igual y apuntan a los mismos mensajes? Sirve para actualizar la
/// lista sin tocar las filas que no cambian (y así conservar el desplazamiento).
fn same_row(a: &ThreadRow, b: &ThreadRow) -> bool {
    let (x, y) = (&a.last, &b.last);
    x.id == y.id
        && x.date == y.date
        && x.flags.seen == y.flags.seen
        && x.flags.flagged == y.flags.flagged
        && x.snippet == y.snippet
        && x.labels == y.labels
        && x.from == y.from
        && a.subject == b.subject
        && a.count == b.count
        && a.unread == b.unread
        && a.has_attachments == b.has_attachments
        && a.message_ids == b.message_ids
}

/// Agrupa cabeceras en hilos (más recientes primero).
pub fn thread_rows(headers: &[MessageHeader]) -> Vec<ThreadRow> {
    let by_id: HashMap<MessageId, &MessageHeader> = headers.iter().map(|h| (h.id, h)).collect();
    build_threads(headers)
        .into_iter()
        .filter_map(|t| {
            let last = by_id.get(t.message_ids.last()?)?;
            let has_attachments = t
                .message_ids
                .iter()
                .any(|id| by_id.get(id).is_some_and(|h| h.has_attachments));
            Some(ThreadRow {
                last: (*last).clone(),
                subject: t.root_subject,
                count: t.message_ids.len(),
                unread: t.unread,
                has_attachments,
                message_ids: t.message_ids,
            })
        })
        .collect()
}

/// Qué hay seleccionado en la lista.
#[derive(Debug, Clone)]
pub enum Selection {
    None,
    /// Exactamente una fila: se abre en el lector (cabecera del último mensaje del hilo).
    One(Box<MessageHeader>),
    /// Dos o más filas: acciones en lote, sin abrir ningún mensaje.
    Many(usize),
}

/// Identidad de una selección, para no avisar dos veces de lo mismo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelKey {
    None,
    One(MessageId),
    Many(usize),
}

impl Selection {
    fn key(&self) -> SelKey {
        match self {
            Selection::None => SelKey::None,
            Selection::One(h) => SelKey::One(h.id),
            Selection::Many(n) => SelKey::Many(*n),
        }
    }
}

type SelectFn = dyn Fn(Selection);

/// Estado compartido entre la lista y los cierres de sus filas y señales.
struct Shared {
    rows: RefCell<Vec<ThreadRow>>,
    /// Mientras se actualiza la lista no se notifica cada cambio intermedio.
    updating: Cell<bool>,
    /// Modo «Seleccionar»: casillas visibles y un clic alterna la fila.
    select_mode: Cell<bool>,
    last: Cell<SelKey>,
}

pub struct MessageList {
    stack: gtk::Stack,
    empty: adw::StatusPage,
    scroller: gtk::ScrolledWindow,
    view: gtk::ListView,
    store: gio::ListStore,
    selection: gtk::MultiSelection,
    shared: Rc<Shared>,
    views: Rc<RefCell<HashMap<gtk::ListItem, RowView>>>,
    on_select: Rc<SelectFn>,
}

impl MessageList {
    /// `on_select` avisa de cada cambio de la selección.
    pub fn new(on_select: impl Fn(Selection) + 'static) -> Self {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::MultiSelection::new(Some(store.clone()));
        let shared = Rc::new(Shared {
            rows: RefCell::new(Vec::new()),
            updating: Cell::new(false),
            select_mode: Cell::new(false),
            last: Cell::new(SelKey::None),
        });

        let factory = gtk::SignalListItemFactory::new();
        // Vista de cada fila reciclada, por `ListItem`.
        let views: Rc<RefCell<HashMap<gtk::ListItem, RowView>>> = Rc::default();
        {
            let views = views.clone();
            let selection = selection.clone();
            let shared = shared.clone();
            factory.connect_setup(move |_, obj| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let view = build_row();
                item.set_child(Some(&view.root));
                // En modo «Seleccionar» un clic en cualquier punto de la fila la alterna
                // (apto para táctil); fuera de ese modo el ListView gestiona el clic.
                let gesture = gtk::GestureClick::new();
                gesture.set_button(1);
                let weak = item.downgrade();
                let (selection, shared) = (selection.clone(), shared.clone());
                gesture.connect_pressed(move |g, _, _, _| {
                    if !shared.select_mode.get() {
                        return;
                    }
                    let Some(item) = weak.upgrade() else { return };
                    let pos = item.position();
                    if selection.is_selected(pos) {
                        selection.unselect_item(pos);
                    } else {
                        selection.select_item(pos, false);
                    }
                    g.set_state(gtk::EventSequenceState::Claimed);
                });
                view.root.add_controller(gesture);
                views.borrow_mut().insert(item.clone(), view);
            });
        }
        {
            let views = views.clone();
            factory.connect_teardown(move |_, obj| {
                if let Some(item) = obj.downcast_ref::<gtk::ListItem>() {
                    views.borrow_mut().remove(item);
                }
            });
        }
        {
            let views = views.clone();
            let shared = shared.clone();
            factory.connect_bind(move |_, obj| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let Some(any) = item
                    .item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else {
                    return;
                };
                let view = views.borrow().get(item).cloned();
                if let Some(view) = view {
                    bind_row(&view, &any.borrow::<ThreadRow>());
                    // Casilla: refleja el estado de selección de la fila y lo sigue.
                    view.check.set_active(item.is_selected());
                    let check = view.check.clone();
                    let id = item.connect_selected_notify(move |it| {
                        check.set_active(it.is_selected());
                    });
                    *view.handler.borrow_mut() = Some(id);
                    view.reveal.set_transition_duration(0);
                    view.reveal.set_reveal_child(shared.select_mode.get());
                }
            });
        }
        {
            let views = views.clone();
            factory.connect_unbind(move |_, obj| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let view = views.borrow().get(item).cloned();
                if let Some(id) = view.and_then(|v| v.handler.borrow_mut().take()) {
                    item.disconnect(id);
                }
            });
        }

        let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        view.set_single_click_activate(false);
        view.add_css_class("correo-thread-list");
        view.update_property(&[gtk::accessible::Property::Label("Conversaciones")]);

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&view));

        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("mail-read-symbolic"));
        empty.add_css_class("correo-empty");
        empty.add_css_class("compact");
        empty.set_title("Todo al día");
        empty.set_description(Some("No hay mensajes en esta carpeta."));

        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&empty, Some("empty"));
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(160);
        stack.set_visible_child_name("empty");
        stack.set_vexpand(true);

        let on_select: Rc<SelectFn> = Rc::new(on_select);
        {
            let shared = shared.clone();
            let on_select = on_select.clone();
            let selection = selection.clone();
            selection.clone().connect_selection_changed(move |_, _, _| {
                if shared.updating.get() {
                    return;
                }
                let state = selection_state(&selection, &shared.rows.borrow());
                if state.key() != shared.last.get() {
                    shared.last.set(state.key());
                    on_select(state);
                }
            });
        }

        Self {
            stack,
            empty,
            scroller,
            view,
            store,
            selection,
            shared,
            views,
            on_select,
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.stack.upcast_ref()
    }

    /// Desplazamiento vertical de la lista, para la carga bajo demanda.
    pub fn scroller(&self) -> &gtk::ScrolledWindow {
        &self.scroller
    }

    /// Texto del estado vacío: con una búsqueda activa se indica que no hay coincidencias.
    pub fn set_empty_for_query(&self, query: &str) {
        if query.is_empty() {
            self.empty.set_icon_name(Some("mail-read-symbolic"));
            self.empty.set_title("Todo al día");
            self.empty
                .set_description(Some("No hay mensajes en esta carpeta."));
        } else {
            self.empty.set_icon_name(Some("edit-find-symbolic"));
            self.empty.set_title("Sin resultados");
            self.empty
                .set_description(Some(&format!("No hay nada que coincida con «{query}».")));
        }
    }

    /// Número de filas seleccionadas.
    pub fn selection_len(&self) -> usize {
        self.selection.selection().size() as usize
    }

    /// Posiciones seleccionadas, en orden de lista.
    pub fn selected_positions(&self) -> Vec<u32> {
        let n = self.store.n_items();
        (0..n).filter(|&i| self.selection.is_selected(i)).collect()
    }

    /// Rangos `(inicio, longitud)` de las filas seleccionadas.
    pub fn selected_ranges(&self) -> Vec<(u32, u32)> {
        let n = self.store.n_items();
        crate::batch::ranges_from_flags((0..n).map(|i| self.selection.is_selected(i)))
    }

    /// Filas (hilos) seleccionadas, en orden de lista.
    pub fn selected_rows(&self) -> Vec<ThreadRow> {
        let rows = self.shared.rows.borrow();
        let ranges = self.selected_ranges();
        crate::batch::positions_in(&ranges)
            .filter_map(|p| rows.get(p as usize).cloned())
            .collect()
    }

    pub fn row_count(&self) -> usize {
        self.shared.rows.borrow().len()
    }

    /// Id del último mensaje de la fila en `position`.
    pub fn row_id_at(&self, position: u32) -> Option<MessageId> {
        self.shared
            .rows
            .borrow()
            .get(position as usize)
            .map(|r| r.last.id)
    }

    /// Reserva (o no) un hueco al final de la lista para la barra de lote flotante.
    pub fn set_bottom_inset(&self, on: bool) {
        self.view.set_margin_bottom(if on { 52 } else { 0 });
    }

    pub fn select_all(&self) {
        self.selection.select_all();
    }

    pub fn unselect_all(&self) {
        self.selection.unselect_all();
    }

    /// Selecciona solo la fila cuyo último mensaje es `id` y la hace visible.
    pub fn select_by_id(&self, id: MessageId) -> bool {
        let position = self
            .shared
            .rows
            .borrow()
            .iter()
            .position(|r| r.last.id == id);
        let Some(p) = position else { return false };
        self.selection.select_item(p as u32, true);
        self.view
            .scroll_to(p as u32, gtk::ListScrollFlags::FOCUS, None);
        true
    }

    /// Lleva el foco de teclado a la fila de `id` sin seleccionarla.
    pub fn focus_id(&self, id: MessageId) {
        let position = self
            .shared
            .rows
            .borrow()
            .iter()
            .position(|r| r.last.id == id);
        if let Some(p) = position {
            self.view
                .scroll_to(p as u32, gtk::ListScrollFlags::FOCUS, None);
        }
    }

    pub fn select_mode(&self) -> bool {
        self.shared.select_mode.get()
    }

    /// Activa o desactiva el modo «Seleccionar» (casillas en cada fila).
    pub fn set_select_mode(&self, on: bool) {
        if self.shared.select_mode.replace(on) == on {
            return;
        }
        let duration = if adw::is_animations_enabled(&self.view) {
            180
        } else {
            0
        };
        if on {
            self.view.add_css_class("correo-select-mode");
        } else {
            self.view.remove_css_class("correo-select-mode");
        }
        for view in self.views.borrow().values() {
            view.reveal.set_transition_duration(duration);
            view.reveal.set_reveal_child(on);
        }
    }

    /// Sustituye las filas. Solo se tocan las que cambian (así la lista no salta ni pierde
    /// el desplazamiento) y se conserva la selección de las filas que siguen estando. Si la
    /// selección cambia, se avisa con `on_select`.
    pub fn set_rows(&self, rows: &[ThreadRow]) {
        let prev_key = self.shared.last.get();
        let prev_selected: HashSet<MessageId> =
            self.selected_rows().iter().map(|r| r.last.id).collect();

        self.shared.updating.set(true);
        let (prefix, suffix, old_len) = {
            let old = self.shared.rows.borrow();
            let max = old.len().min(rows.len());
            let mut prefix = 0;
            while prefix < max && same_row(&old[prefix], &rows[prefix]) {
                prefix += 1;
            }
            let mut suffix = 0;
            while suffix < max - prefix
                && same_row(&old[old.len() - 1 - suffix], &rows[rows.len() - 1 - suffix])
            {
                suffix += 1;
            }
            (prefix, suffix, old.len())
        };
        let removed = old_len - prefix - suffix;
        let added: Vec<glib::BoxedAnyObject> = rows[prefix..rows.len() - suffix]
            .iter()
            .cloned()
            .map(glib::BoxedAnyObject::new)
            .collect();
        let replaced_all = old_len > 0 && !rows.is_empty() && prefix == 0 && suffix == 0;
        if removed > 0 || !added.is_empty() {
            self.store.splice(prefix as u32, removed as u32, &added);
        }
        *self.shared.rows.borrow_mut() = rows.to_vec();
        if replaced_all {
            self.scroller.vadjustment().set_value(0.0);
        }
        if !prev_selected.is_empty() {
            let selected = gtk::Bitset::new_empty();
            for (i, r) in rows.iter().enumerate() {
                if prev_selected.contains(&r.last.id) {
                    selected.add(i as u32);
                }
            }
            let mask = gtk::Bitset::new_range(0, rows.len() as u32);
            self.selection.set_selection(&selected, &mask);
        }
        self.shared.updating.set(false);

        self.stack
            .set_visible_child_name(if rows.is_empty() { "empty" } else { "list" });

        let state = selection_state(&self.selection, &self.shared.rows.borrow());
        if state.key() != prev_key {
            self.shared.last.set(state.key());
            (self.on_select)(state);
        }
    }
}

fn selection_state(selection: &gtk::MultiSelection, rows: &[ThreadRow]) -> Selection {
    let bitset = selection.selection();
    match bitset.size() {
        0 => Selection::None,
        1 => match rows.get(bitset.minimum() as usize) {
            Some(row) => Selection::One(Box::new(row.last.clone())),
            None => Selection::None,
        },
        n => Selection::Many(n as usize),
    }
}

/// Referencias a los widgets de una fila, creadas junto a ella en `build_row`.
#[derive(Clone)]
struct RowView {
    root: gtk::Box,
    /// Casilla del modo «Seleccionar» (solo indicadora: el clic lo recoge la fila).
    check: gtk::CheckButton,
    reveal: gtk::Revealer,
    /// Manejador de `selected` del `ListItem` enlazado, para soltarlo al desenlazar.
    handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    avatar: adw::Avatar,
    sender: gtk::Label,
    count: gtk::Label,
    date: gtk::Label,
    clip: gtk::Image,
    subject: gtk::Label,
    snippet: gtk::Label,
    label_chip: gtk::Label,
}

fn build_row() -> RowView {
    // Punto de no leído: columna fija a la izquierda del avatar (no cambia la altura).
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("correo-unread-dot");
    dot.set_valign(gtk::Align::Center);
    dot.set_accessible_role(gtk::AccessibleRole::Presentation);

    let avatar = adw::Avatar::new(40, None, true);
    avatar.set_valign(gtk::Align::Start);
    avatar.add_css_class("correo-avatar");

    let sender = plain_label("correo-sender");
    sender.set_hexpand(true);
    let date = plain_label("correo-date");
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&sender);
    top.append(&date);

    let subject = plain_label("correo-subject");
    subject.set_hexpand(true);
    let label_chip = plain_label("correo-label-chip");
    label_chip.set_visible(false);
    let clip = gtk::Image::from_icon_name("mail-attachment-symbolic");
    clip.add_css_class("correo-clip");
    clip.set_pixel_size(14);
    let count = plain_label("correo-count");
    count.add_css_class("correo-tabular");
    let middle = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    middle.append(&subject);
    middle.append(&label_chip);
    middle.append(&clip);
    middle.append(&count);

    let snippet = plain_label("correo-snippet");
    snippet.set_single_line_mode(true);

    let col = gtk::Box::new(gtk::Orientation::Vertical, 1);
    col.set_hexpand(true);
    col.append(&top);
    col.append(&middle);
    col.append(&snippet);

    let check = gtk::CheckButton::new();
    check.add_css_class("selection-mode");
    check.add_css_class("correo-select-check");
    check.set_valign(gtk::Align::Center);
    check.set_can_target(false);
    check.set_can_focus(false);
    check.set_accessible_role(gtk::AccessibleRole::Presentation);
    let reveal = gtk::Revealer::new();
    reveal.set_transition_type(gtk::RevealerTransitionType::SlideRight);
    reveal.set_child(Some(&check));
    reveal.set_reveal_child(false);

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    root.add_css_class("correo-thread-row");
    root.append(&reveal);
    root.append(&dot);
    root.append(&avatar);
    root.append(&col);
    RowView {
        root,
        check,
        reveal,
        handler: Rc::default(),
        avatar,
        sender,
        count,
        date,
        clip,
        subject,
        snippet,
        label_chip,
    }
}

fn plain_label(class: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.add_css_class(class);
    label
}

fn bind_row(view: &RowView, row: &ThreadRow) {
    let m = &row.last;
    if row.unread > 0 {
        view.root.add_css_class("correo-unread");
    } else {
        view.root.remove_css_class("correo-unread");
    }

    let from = m
        .from
        .first()
        .map_or_else(|| "(sin remitente)".to_string(), display_name);
    view.sender.set_label(&from);
    view.avatar.set_text(Some(&from));
    let email = m.from.first().map_or("", |a| a.email.as_str());
    ui_style::set_tone(&view.avatar, &ui_style::sender_class(email));

    view.count.set_visible(row.count > 1);
    view.count.set_label(&row.count.to_string());

    view.date.set_label(&format_date(m.date));
    view.clip.set_visible(row.has_attachments);

    let subject: &str = if row.subject.trim().is_empty() {
        "(sin asunto)"
    } else {
        &row.subject
    };
    view.subject.set_label(subject);

    match m.labels.first() {
        Some(label) => {
            view.label_chip.set_label(label);
            view.label_chip.set_visible(true);
        }
        None => view.label_chip.set_visible(false),
    }

    view.snippet.set_label(m.snippet.as_deref().unwrap_or(""));

    // Descripción accesible de la fila completa (el lector de pantalla no recorre los hijos).
    let state = if row.unread > 0 { "No leído. " } else { "" };
    let attach = if row.has_attachments {
        " Con adjuntos."
    } else {
        ""
    };
    view.root
        .update_property(&[gtk::accessible::Property::Label(&format!(
            "{state}{from}, {subject}, {}.{attach}",
            format_date(m.date)
        ))]);
}

fn display_name(a: &Address) -> String {
    a.name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| a.email.clone())
}

/// Fecha relativa: hora si es hoy, día de la semana si es de esta semana, fecha si no.
fn format_date(ts: i64) -> String {
    let (Ok(then), Ok(now)) = (
        glib::DateTime::from_unix_local(ts),
        glib::DateTime::now_local(),
    ) else {
        return String::new();
    };
    let fmt = if then.year() == now.year() && then.day_of_year() == now.day_of_year() {
        "%H:%M"
    } else if now.to_unix() - ts < 6 * 86_400 {
        "%a"
    } else if then.year() == now.year() {
        "%d %b"
    } else {
        "%d/%m/%Y"
    };
    then.format(fmt).map(|s| s.to_string()).unwrap_or_default()
}

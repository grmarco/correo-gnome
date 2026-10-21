//! T9: lista central de hilos. `gtk::ListView` virtualizada sobre un `gio::ListStore`
//! de `glib::BoxedAnyObject` que envuelven `ThreadRow`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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
            })
        })
        .collect()
}

type SelectFn = dyn Fn(Option<MessageHeader>);

pub struct MessageList {
    stack: gtk::Stack,
    empty: adw::StatusPage,
    scroller: gtk::ScrolledWindow,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    /// Mientras se reconstruye la lista no se notifica cada cambio intermedio.
    updating: Rc<Cell<bool>>,
    on_select: Rc<SelectFn>,
}

impl MessageList {
    /// `on_select` recibe la cabecera del último mensaje del hilo seleccionado, o `None`.
    pub fn new(on_select: impl Fn(Option<MessageHeader>) + 'static) -> Self {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();

        let factory = gtk::SignalListItemFactory::new();
        // Vista de cada fila reciclada, por `ListItem`.
        let views: Rc<RefCell<HashMap<gtk::ListItem, RowView>>> = Rc::default();
        {
            let views = views.clone();
            factory.connect_setup(move |_, obj| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let view = build_row();
                item.set_child(Some(&view.root));
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
            }
        });

        let selection = gtk::SingleSelection::new(Some(store.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        view.set_single_click_activate(false);
        view.add_css_class("correo-thread-list");

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

        let updating = Rc::new(Cell::new(false));
        let on_select: Rc<SelectFn> = Rc::new(on_select);
        {
            let updating = updating.clone();
            let on_select = on_select.clone();
            selection.connect_selected_item_notify(move |sel| {
                if !updating.get() {
                    on_select(header_of(sel.selected_item()));
                }
            });
        }

        Self {
            stack,
            empty,
            scroller,
            store,
            selection,
            updating,
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

    pub fn selected_header(&self) -> Option<MessageHeader> {
        header_of(self.selection.selected_item())
    }

    /// Sustituye todas las filas. Conserva la selección si el último mensaje
    /// seleccionado sigue en la lista; si no, avisa con `on_select(None)`.
    pub fn set_rows(&self, rows: &[ThreadRow]) {
        let prev = self.selected_header().map(|h| h.id);

        self.updating.set(true);
        let items: Vec<glib::BoxedAnyObject> = rows
            .iter()
            .cloned()
            .map(glib::BoxedAnyObject::new)
            .collect();
        self.store.splice(0, self.store.n_items(), &items);
        let position = prev.and_then(|id| rows.iter().position(|r| r.last.id == id));
        self.selection
            .set_selected(position.map_or(gtk::INVALID_LIST_POSITION, |p| p as u32));
        self.updating.set(false);

        self.stack
            .set_visible_child_name(if rows.is_empty() { "empty" } else { "list" });

        let now = self.selected_header().map(|h| h.id);
        if now != prev {
            (self.on_select)(self.selected_header());
        }
    }
}

fn header_of(item: Option<glib::Object>) -> Option<MessageHeader> {
    let any = item?.downcast::<glib::BoxedAnyObject>().ok()?;
    let row = any.borrow::<ThreadRow>();
    Some(row.last.clone())
}

/// Referencias a los widgets de una fila, creadas junto a ella en `build_row`.
#[derive(Clone)]
struct RowView {
    root: gtk::Box,
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

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    root.add_css_class("correo-thread-row");
    root.append(&dot);
    root.append(&avatar);
    root.append(&col);
    RowView {
        root,
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

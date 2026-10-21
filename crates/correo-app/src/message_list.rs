//! T9: lista central de hilos. `gtk::ListView` virtualizada sobre un `gio::ListStore`
//! de `glib::BoxedAnyObject` que envuelven `ThreadRow`.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use correo_core::{Address, MessageHeader, MessageId};
use correo_threads::build_threads;
use gtk::{gio, glib};

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
        factory.connect_setup(|_, obj| {
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            item.set_child(Some(&build_row()));
        });
        factory.connect_bind(|_, obj| {
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(any) = item
                .item()
                .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };
            let (Some(child), row) = (item.child(), any.borrow::<ThreadRow>()) else {
                return;
            };
            if let Some(view) = row_view(&child) {
                bind_row(&view, &row);
            }
        });

        let selection = gtk::SingleSelection::new(Some(store.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        view.set_single_click_activate(false);

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&view));

        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("mail-read-symbolic"));
        empty.set_title("Sin mensajes");
        empty.set_description(Some(
            "Sincroniza para descargar los correos de esta carpeta.",
        ));

        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&empty, Some("empty"));
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

/// Referencias a los widgets de una fila, obtenidas recorriendo el árbol creado en `build_row`.
struct RowView {
    sender: gtk::Label,
    count: gtk::Label,
    date: gtk::Label,
    clip: gtk::Image,
    subject: gtk::Label,
    snippet: gtk::Label,
}

fn build_row() -> gtk::Box {
    let sender = plain_label(false);
    let count = plain_label(true);
    let date = plain_label(true);
    let clip = gtk::Image::from_icon_name("mail-attachment-symbolic");
    clip.add_css_class("dim-label");

    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    top.append(&sender);
    top.append(&count);
    top.append(&date);
    top.append(&clip);

    let subject = plain_label(false);
    let snippet = plain_label(true);
    snippet.set_single_line_mode(true);

    let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
    col.set_hexpand(true);
    col.append(&top);
    col.append(&subject);
    col.append(&snippet);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_top(8);
    row.set_margin_bottom(8);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.append(&col);
    row
}

fn plain_label(dim: bool) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    if dim {
        label.add_css_class("dim-label");
        label.add_css_class("caption");
    }
    label
}

/// Recupera los widgets de la fila construida por `build_row`.
fn row_view(root: &gtk::Widget) -> Option<RowView> {
    let col = root.first_child()?;
    let top = col.first_child()?;
    let sender = top.first_child()?;
    let count = sender.next_sibling()?;
    let date = count.next_sibling()?;
    let clip = date.next_sibling()?;
    let subject = top.next_sibling()?;
    let snippet = col.last_child()?;
    Some(RowView {
        sender: sender.downcast().ok()?,
        count: count.downcast().ok()?,
        date: date.downcast().ok()?,
        clip: clip.downcast().ok()?,
        subject: subject.downcast().ok()?,
        snippet: snippet.downcast().ok()?,
    })
}

fn bind_row(view: &RowView, row: &ThreadRow) {
    let m = &row.last;
    let emphasis: &[&str] = if row.unread > 0 { &["heading"] } else { &[] };

    let from = m
        .from
        .first()
        .map_or_else(|| "(sin remitente)".to_string(), display_name);
    view.sender.set_label(&from);
    view.sender.set_css_classes(emphasis);

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
    view.subject.set_css_classes(emphasis);

    view.snippet.set_label(m.snippet.as_deref().unwrap_or(""));
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

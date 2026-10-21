//! T9: lista central de hilos. `gtk::ListView` virtualizada sobre un `gio::ListStore`
//! de `glib::BoxedAnyObject` que envuelven `ThreadRow`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use correo_core::{Address, MessageHeader, MessageId};
use correo_threads::build_threads;
use gtk::{gio, glib};

use crate::notice::{Notice, Tone};
use crate::older::Footer;
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

/// Callback de «Reintentar», compartido con los botones del aviso y de la página de error.
type RetrySlot = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// Estado compartido entre la lista y los cierres de sus filas y señales.
struct Shared {
    rows: RefCell<Vec<ThreadRow>>,
    /// Mientras se actualiza la lista no se notifica cada cambio intermedio.
    updating: Cell<bool>,
    /// Modo «Seleccionar»: casillas visibles y un clic alterna la fila.
    select_mode: Cell<bool>,
    last: Cell<SelKey>,
}

/// Estado de carga de la carpeta (o carpetas) que muestra la lista.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LoadState {
    /// Sincronizada: una lista vacía significa «todo al día».
    #[default]
    Idle,
    /// Se está sincronizando por primera vez o refrescando.
    Loading,
    /// La última sincronización falló, con el motivo.
    Failed(String),
}

/// Texto de «Cargando» accesible, también usado por el esqueleto.
const LOADING_LABEL: &str = "Cargando mensajes…";

/// Fila del final de la lista: «Cargar mensajes anteriores» y sus estados.
struct OlderRow {
    root: gtk::Box,
    spinner: gtk::Spinner,
    label: gtk::Label,
    button: gtk::Button,
    /// Callback de «Cargar mensajes anteriores» / «Reintentar».
    on_load: RetrySlot,
}

impl OlderRow {
    fn new() -> Self {
        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        let label = gtk::Label::new(None);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.add_css_class("caption");
        label.set_visible(false);
        let button = gtk::Button::with_label("Cargar mensajes anteriores");
        button.add_css_class("flat");
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.add_css_class("correo-older-row");
        root.set_halign(gtk::Align::Center);
        root.set_margin_top(6);
        root.set_margin_bottom(6);
        root.set_margin_start(12);
        root.set_margin_end(12);
        root.append(&spinner);
        root.append(&label);
        root.append(&button);
        root.set_visible(false);
        let on_load: RetrySlot = Rc::default();
        {
            let on_load = on_load.clone();
            button.connect_clicked(move |_| {
                let f = on_load.borrow().clone();
                if let Some(f) = f {
                    f();
                }
            });
        }
        Self {
            root,
            spinner,
            label,
            button,
            on_load,
        }
    }

    fn set(&self, footer: &Footer) {
        let (spin, text, button, error) = match footer {
            Footer::Hidden => (false, None, None, false),
            Footer::Available => (false, None, Some("Cargar mensajes anteriores"), false),
            Footer::Loading => (true, Some("Cargando mensajes anteriores…"), None, false),
            Footer::Exhausted => (false, Some("No hay mensajes más antiguos"), None, false),
            Footer::Failed(e) => (false, Some(e.as_str()), Some("Reintentar"), true),
        };
        self.root.set_visible(*footer != Footer::Hidden);
        self.spinner.set_visible(spin);
        self.spinner
            .set_spinning(spin && adw::is_animations_enabled(&self.root));
        self.label.set_visible(text.is_some());
        self.label.set_label(text.unwrap_or(""));
        self.label.set_tooltip_text(error.then_some(text).flatten());
        if error {
            self.label.add_css_class("error");
            self.label.remove_css_class("dim-label");
        } else {
            self.label.remove_css_class("error");
            self.label.add_css_class("dim-label");
        }
        self.button.set_visible(button.is_some());
        if let Some(b) = button {
            self.button.set_label(b);
        }
        let description = match footer {
            Footer::Failed(e) => format!("No se pudieron cargar mensajes anteriores: {e}"),
            _ => text.or(button).unwrap_or("").to_string(),
        };
        self.root
            .update_property(&[gtk::accessible::Property::Label(&description)]);
    }
}

pub struct MessageList {
    root: gtk::Box,
    older: OlderRow,
    stack: gtk::Stack,
    empty: adw::StatusPage,
    /// Barra fina indeterminada sobre el borde superior mientras se sincroniza.
    progress: gtk::Box,
    /// Aviso con «Reintentar» cuando falla la sincronización y hay filas en caché.
    failed_banner: Notice,
    failed_page: adw::StatusPage,
    retry: RetrySlot,
    load: RefCell<LoadState>,
    has_query: Cell<bool>,
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

        let failed_page = adw::StatusPage::new();
        failed_page.set_icon_name(Some("network-error-symbolic"));
        failed_page.add_css_class("correo-empty");
        failed_page.add_css_class("correo-error");
        failed_page.add_css_class("compact");
        failed_page.set_title("No se pudo cargar la carpeta");
        let retry: RetrySlot = Rc::default();
        let retry_btn = gtk::Button::with_label("Reintentar");
        retry_btn.add_css_class("pill");
        retry_btn.set_halign(gtk::Align::Center);
        failed_page.set_child(Some(&retry_btn));
        let failed_banner = Notice::new("dialog-warning-symbolic", Tone::Error, false);
        failed_banner.set_title("No se pudo actualizar la carpeta");
        failed_banner.set_compact();
        failed_banner.set_button_label(Some("Reintentar"));
        let on_retry: Rc<dyn Fn()> = {
            let retry = retry.clone();
            Rc::new(move || fire_retry(&retry))
        };
        {
            let f = on_retry.clone();
            retry_btn.connect_clicked(move |_| f());
        }
        failed_banner.connect_button_clicked(move || on_retry());

        let older = OlderRow::new();
        let list_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list_page.append(&scroller);
        list_page.append(&older.root);
        let stack = gtk::Stack::new();
        stack.add_named(&list_page, Some("list"));
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&loading_page(), Some("loading"));
        stack.add_named(&failed_page, Some("failed"));
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

        // Barra indeterminada solo con CSS (un bloque que se desliza): sin temporizadores y
        // la animación se detiene sola cuando el widget no se ve.
        let progress = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        progress.add_css_class("correo-loading-bar");
        progress.set_valign(gtk::Align::Start);
        progress.set_visible(false);
        progress.set_can_target(false);
        progress.set_accessible_role(gtk::AccessibleRole::Presentation);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        overlay.add_overlay(&progress);
        overlay.set_vexpand(true);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(failed_banner.widget());
        root.append(&overlay);

        Self {
            root,
            older,
            stack,
            empty,
            progress,
            failed_banner,
            failed_page,
            retry,
            load: RefCell::new(LoadState::Idle),
            has_query: Cell::new(false),
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
        self.root.upcast_ref()
    }

    /// Se invoca al pulsar «Reintentar» tras un fallo de sincronización.
    pub fn connect_retry(&self, f: impl Fn() + 'static) {
        *self.retry.borrow_mut() = Some(Rc::new(f));
    }

    /// Se invoca al pulsar «Cargar mensajes anteriores» (o «Reintentar» tras un fallo).
    pub fn connect_load_older(&self, f: impl Fn() + 'static) {
        *self.older.on_load.borrow_mut() = Some(Rc::new(f));
    }

    /// Fila del final de la lista.
    pub fn set_older_footer(&self, footer: &Footer) {
        self.older.set(footer);
    }

    /// Estado de sincronización de lo que se muestra: barra fina mientras carga, esqueleto
    /// si además no hay filas, y aviso o página de error si falló.
    pub fn set_load_state(&self, state: LoadState) {
        if *self.load.borrow() == state {
            return;
        }
        *self.load.borrow_mut() = state;
        self.update_page();
    }

    /// Elige qué página muestra la pila según las filas, la búsqueda y el estado de carga.
    fn update_page(&self) {
        let has_rows = !self.shared.rows.borrow().is_empty();
        // Una búsqueda local no depende de la sincronización: su vacío es «sin resultados».
        let state = if self.has_query.get() {
            LoadState::Idle
        } else {
            self.load.borrow().clone()
        };
        let page = match (&state, has_rows) {
            (_, true) => "list",
            (LoadState::Loading, false) => "loading",
            (LoadState::Failed(_), false) => "failed",
            (LoadState::Idle, false) => "empty",
        };
        self.stack.set_visible_child_name(page);

        let loading = matches!(*self.load.borrow(), LoadState::Loading);
        self.progress.set_visible(loading);
        // Sin animaciones del sistema, una barra fija y tenue en lugar del bloque deslizante.
        if adw::is_animations_enabled(&self.progress) {
            self.progress.remove_css_class("correo-static");
        } else {
            self.progress.add_css_class("correo-static");
        }
        match &state {
            LoadState::Failed(reason) => {
                self.failed_page.set_description(Some(reason));
                self.failed_banner.set_detail(Some(reason));
                self.failed_banner.set_revealed(has_rows);
            }
            _ => self.failed_banner.set_revealed(false),
        }
    }

    /// Desplazamiento vertical de la lista, para la carga bajo demanda.
    pub fn scroller(&self) -> &gtk::ScrolledWindow {
        &self.scroller
    }

    /// Texto del estado vacío: con una búsqueda activa se indica que no hay coincidencias.
    pub fn set_empty_for_query(&self, query: &str) {
        self.has_query.set(!query.is_empty());
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

        self.update_page();

        let state = selection_state(&self.selection, &self.shared.rows.borrow());
        if state.key() != prev_key {
            self.shared.last.set(state.key());
            (self.on_select)(state);
        }
    }
}

fn fire_retry(retry: &RetrySlot) {
    let callback = retry.borrow().clone();
    if let Some(callback) = callback {
        callback();
    }
}

/// Esqueleto de filas de la lista mientras una carpeta se carga por primera vez.
fn loading_page() -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.add_css_class("correo-list-skeleton");
    page.set_accessible_role(gtk::AccessibleRole::Group);
    page.update_property(&[gtk::accessible::Property::Label(LOADING_LABEL)]);
    let caption = gtk::Label::new(Some(LOADING_LABEL));
    caption.add_css_class("caption");
    caption.add_css_class("dim-label");
    caption.set_xalign(0.0);
    caption.set_margin_start(18);
    caption.set_margin_top(14);
    caption.set_margin_bottom(6);
    page.append(&caption);
    // Anchos variados para que no parezca una rejilla; cada fila algo más tenue.
    let widths = [
        (150, 230),
        (120, 190),
        (170, 250),
        (110, 210),
        (140, 170),
        (160, 240),
    ];
    for (i, (name, text)) in widths.iter().enumerate() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("correo-skeleton-row");
        row.set_opacity(1.0 - i as f64 * 0.13);
        let avatar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        avatar.add_css_class("correo-skeleton");
        avatar.add_css_class("correo-skeleton-circle");
        avatar.set_size_request(40, 40);
        avatar.set_valign(gtk::Align::Start);
        let lines = gtk::Box::new(gtk::Orientation::Vertical, 8);
        lines.set_hexpand(true);
        for (w, h) in [(*name, 12), (*text, 10)] {
            let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            bar.add_css_class("correo-skeleton");
            bar.set_size_request(w, h);
            bar.set_halign(gtk::Align::Start);
            lines.append(&bar);
        }
        row.append(&avatar);
        row.append(&lines);
        page.append(&row);
    }
    page.set_valign(gtk::Align::Start);
    page
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

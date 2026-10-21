//! T9: barra lateral con la bandeja unificada y las carpetas de cada cuenta,
//! ordenadas por rol (Entrada, Borradores, Enviados, Archivo, Spam, Papelera, resto).
//! Cada cuenta se identifica por su email (varias cuentas del mismo proveedor pueden
//! tener el mismo nombre). Con el botón derecho sobre la cuenta se ofrece «Eliminar cuenta».
//!
//! Cada cuenta es una sección plegable: cabecera enfocable (avatar, email, insignia de no
//! leídos si está plegada, opciones y chevron) y, debajo, una lista de carpetas dentro de
//! un `gtk::Revealer`. La selección es única entre todas las listas. El estado de plegado
//! se guarda en `ui_state` y se conserva al reconstruir la barra.
//!
//! Las carpetas forman un árbol (`folder_tree`): la lista de la cuenta sigue siendo un
//! `ListBox` plano con una fila por nodo, sangrada por nivel y con el contenido dentro de
//! un `Revealer`; plegar un nodo anima (y luego oculta) las filas de sus descendientes.
//! El plegado de cada nodo se guarda por (cuenta, ruta).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use correo_core::{Account, AccountId, Mailbox, MailboxId, MailboxRole};
use gtk::gio;

use crate::folder_tree::{self, Node};
use crate::ui_state::UiState;
use crate::ui_style;

/// Duración de la transición de plegado, en milisegundos.
const FOLD_MS: u32 = 200;

/// Destino seleccionable en la barra lateral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Entradas de todas las cuentas.
    Unified,
    /// Una carpeta concreta.
    Mailbox(MailboxId),
}

/// Lista de filas seleccionables con el destino de cada una, por posición.
#[derive(Clone)]
struct TargetList {
    list: gtk::ListBox,
    /// `None` en los contenedores (no seleccionables).
    targets: Rc<Vec<Option<Target>>>,
}

/// Fila de un nodo del árbol de carpetas de una cuenta.
#[derive(Clone)]
struct TreeRow {
    /// Clave de plegado (ruta normalizada).
    key: String,
    default_collapsed: bool,
    /// Índice del padre en la misma lista.
    parent: Option<usize>,
    has_children: bool,
    row: gtk::ListBoxRow,
    revealer: gtk::Revealer,
    chevron: gtk::Image,
    badge: gtk::Label,
    name: String,
    own_unread: u32,
    total_unread: u32,
}

/// Árbol de carpetas de una cuenta, con una fila por nodo en orden de recorrido.
#[derive(Clone)]
struct Tree {
    account: AccountId,
    list: gtk::ListBox,
    rows: Rc<Vec<TreeRow>>,
}

/// Sección plegable de una cuenta.
#[derive(Clone)]
struct Section {
    id: AccountId,
    email: String,
    unread: u32,
    /// Botón de la cabecera (recibe el foco; Enter y Espacio lo activan de serie).
    header: gtk::Button,
    /// Cabecera completa (se inserta en la barra).
    row: gtk::Box,
    revealer: gtk::Revealer,
    chevron: gtk::Image,
    badge: gtk::Label,
}

type Shared<T> = Rc<RefCell<T>>;

pub struct Sidebar {
    page: adw::NavigationPage,
    add_button: gtk::Button,
    /// Contenedor de todas las secciones (dentro del desplazador).
    content: gtk::Box,
    /// Listas de destinos (la primera es la de «Todas las entradas»).
    lists: Shared<Vec<TargetList>>,
    sections: Shared<Vec<Section>>,
    trees: Shared<Vec<Tree>>,
    /// Verdadero mientras se reconstruyen las listas: las selecciones que GTK hace por su
    /// cuenta (el foco salta a otra fila al destruir la enfocada) no cambian de destino.
    rebuilding: Rc<Cell<bool>>,
    state: Shared<UiState>,
    on_select: Rc<dyn Fn(Target)>,
    on_activate: Rc<dyn Fn()>,
    /// Menú «Eliminar cuenta», con un único popover reutilizado para todas las cuentas.
    menu: gtk::Popover,
    /// Cuenta a la que apunta el menú abierto.
    menu_account: Rc<Cell<Option<AccountId>>>,
}

impl Sidebar {
    /// `on_select` recibe el destino elegido; `on_remove` la cuenta que el usuario quiere
    /// eliminar desde el menú contextual (la confirmación la hace quien recibe el aviso).
    pub fn new(
        on_select: impl Fn(Target) + 'static,
        on_remove: impl Fn(AccountId) + 'static,
        on_activate: impl Fn() + 'static,
    ) -> Self {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.add_css_class("correo-sidebar-box");

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_vexpand(true);
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_child(Some(&content));

        // Menú contextual de la cabecera de una cuenta. Su padre es el desplazador.
        let menu_account: Rc<Cell<Option<AccountId>>> = Rc::new(Cell::new(None));
        let menu = gtk::Popover::new();
        menu.set_has_arrow(false);
        menu.set_parent(&scroller);
        menu.add_css_class("menu");
        let remove = gtk::Button::new();
        remove.set_child(Some(
            &adw::ButtonContent::builder()
                .icon_name("user-trash-symbolic")
                .label("Eliminar cuenta…")
                .build(),
        ));
        remove.add_css_class("flat");
        remove.add_css_class("correo-danger");
        menu.set_child(Some(&remove));
        {
            let menu = menu.clone();
            let menu_account = Rc::clone(&menu_account);
            remove.connect_clicked(move |_| {
                menu.popdown();
                if let Some(id) = menu_account.take() {
                    on_remove(id);
                }
            });
        }

        let add_button = gtk::Button::from_icon_name("list-add-symbolic");
        add_button.set_tooltip_text(Some("Añadir cuenta"));
        add_button.update_property(&[gtk::accessible::Property::Label("Añadir cuenta")]);
        let main_menu = gio::Menu::new();
        let fold_section = gio::Menu::new();
        fold_section.append(Some("Plegar todas las cuentas"), Some("sidebar.fold-all"));
        fold_section.append(
            Some("Desplegar todas las cuentas"),
            Some("sidebar.unfold-all"),
        );
        main_menu.append_section(None, &fold_section);
        let app_section = gio::Menu::new();
        app_section.append(Some("Acerca de Correo"), Some("app.about"));
        app_section.append(Some("Salir"), Some("app.quit"));
        main_menu.append_section(None, &app_section);
        let menu_button = gtk::MenuButton::new();
        menu_button.set_icon_name("open-menu-symbolic");
        menu_button.set_menu_model(Some(&main_menu));
        menu_button.set_primary(true);
        menu_button.set_tooltip_text(Some("Menú principal (F10)"));
        menu_button.update_property(&[gtk::accessible::Property::Label("Menú principal")]);
        let header = adw::HeaderBar::new();
        header.pack_start(&add_button);
        header.pack_end(&menu_button);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroller));

        let page = adw::NavigationPage::new(&toolbar, "Carpetas");
        let sidebar = Self {
            page,
            add_button,
            content,
            lists: Rc::new(RefCell::new(Vec::new())),
            sections: Rc::new(RefCell::new(Vec::new())),
            trees: Rc::new(RefCell::new(Vec::new())),
            rebuilding: Rc::new(Cell::new(false)),
            state: Rc::new(RefCell::new(UiState::load())),
            on_select: Rc::new(on_select),
            on_activate: Rc::new(on_activate),
            menu,
            menu_account,
        };
        sidebar.install_fold_actions();
        sidebar
    }

    /// Acciones «Plegar todas» / «Desplegar todas» del menú principal.
    fn install_fold_actions(&self) {
        let group = gio::SimpleActionGroup::new();
        for (name, collapsed) in [("fold-all", true), ("unfold-all", false)] {
            let action = gio::SimpleAction::new(name, None);
            let sections = Rc::clone(&self.sections);
            let state = Rc::clone(&self.state);
            action.connect_activate(move |_, _| {
                for section in sections.borrow().iter() {
                    set_collapsed(&state, section, collapsed, true);
                }
            });
            group.add_action(&action);
        }
        self.page.insert_action_group("sidebar", Some(&group));
    }

    pub fn page(&self) -> &adw::NavigationPage {
        &self.page
    }

    /// Se invoca al pulsar «Añadir cuenta» en la cabecera de la barra lateral.
    pub fn connect_add_account(&self, f: impl Fn() + 'static) {
        self.add_button.connect_clicked(move |_| f());
    }

    /// Despliega la cuenta indicada (p. ej. al abrir una carpeta suya desde una notificación).
    pub fn expand_account(&self, id: AccountId) {
        let section = self.sections.borrow().iter().find(|s| s.id == id).cloned();
        match section {
            Some(section) => set_collapsed(&self.state, &section, false, true),
            None => {
                let changed = {
                    let mut st = self.state.borrow_mut();
                    let was = st.is_collapsed(id);
                    st.set_collapsed(id, false);
                    was
                };
                if changed {
                    self.state.borrow().save();
                }
            }
        }
    }

    /// Despliega la cuenta y los ancestros plegados de `mailbox` para que su fila sea visible
    /// (selección programática: notificaciones, búsqueda).
    pub fn reveal_mailbox(&self, mailbox: &Mailbox) {
        self.expand_account(mailbox.account_id);
        let tree = self
            .trees
            .borrow()
            .iter()
            .find(|t| t.account == mailbox.account_id)
            .cloned();
        let Some(tree) = tree else { return };
        let mut changed = false;
        for key in folder_tree::ancestor_keys(mailbox) {
            let Some(row) = tree.rows.iter().find(|r| r.key == key) else {
                continue;
            };
            let mut st = self.state.borrow_mut();
            if st.folder_collapsed(tree.account, &key, row.default_collapsed) {
                st.set_folder_collapsed(tree.account, &key, false, row.default_collapsed);
                changed = true;
            }
        }
        if changed {
            self.state.borrow().save();
            apply_tree(&self.state, &tree, true);
        }
    }

    /// Reconstruye la barra con las cuentas y sus carpetas, y selecciona `current`
    /// sin que ello cambie de destino (el llamador comprueba si ya estaba activo).
    /// El plegado de cada cuenta se conserva.
    pub fn reload(&self, accounts: &[(Account, Vec<Mailbox>)], current: Target) {
        // Recuerda si el foco estaba en una cabecera para devolvérselo a la nueva.
        let focused_folder = self.trees.borrow().iter().find_map(|t| {
            let i = t.rows.iter().position(|r| r.row.is_focus())?;
            Some((t.account, t.rows[i].key.clone()))
        });
        let focused_unified = self
            .lists
            .borrow()
            .first()
            .and_then(|l| l.list.row_at_index(0))
            .is_some_and(|r| r.is_focus());
        self.rebuilding.set(true);
        let focused_header = self
            .sections
            .borrow()
            .iter()
            .find(|s| s.header.is_focus())
            .map(|s| s.id);

        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        self.lists.borrow_mut().clear();
        self.sections.borrow_mut().clear();
        self.trees.borrow_mut().clear();

        if !accounts.is_empty() {
            let ids: Vec<AccountId> = accounts.iter().map(|(a, _)| a.id).collect();
            let before = self.state.borrow().clone();
            self.state.borrow_mut().retain_accounts(&ids);
            if *self.state.borrow() != before {
                self.state.borrow().save();
            }
        }

        let unified_unread: u32 = accounts
            .iter()
            .flat_map(|(_, boxes)| boxes)
            .filter(|m| m.role == MailboxRole::Inbox)
            .map(|m| m.unread)
            .sum();
        let unified = vec![(
            target_row(
                "mail-unread-symbolic",
                "Todas las entradas",
                unified_unread,
                true,
            ),
            Some(Target::Unified),
        )];
        let list = self.target_list(unified);
        self.content.append(&list.list);
        self.lists.borrow_mut().push(list);

        for (index, (account, boxes)) in accounts.iter().enumerate() {
            let nodes = folder_tree::folder_tree(boxes);
            let (list, tree) = self.build_tree(account.id, &nodes);

            let unread: u32 = boxes.iter().map(|m| m.unread).sum();
            let inbox_unread = boxes
                .iter()
                .any(|m| m.role == MailboxRole::Inbox && m.unread > 0);
            let revealer = gtk::Revealer::new();
            revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
            revealer.set_child(Some(&list.list));
            let section = self.header_section(account, index, unread, inbox_unread, revealer);
            // Estado inicial sin animación.
            let collapsed = self.state.borrow().is_collapsed(account.id);
            apply_fold(&section, collapsed, false);

            let block = gtk::Box::new(gtk::Orientation::Vertical, 0);
            block.add_css_class("correo-account-section");
            block.append(&section.row);
            block.append(&section.revealer);
            self.content.append(&block);
            self.lists.borrow_mut().push(list);
            self.trees.borrow_mut().push(tree);
            self.sections.borrow_mut().push(section);
        }

        // Si el menú sigue abierto sobre una cuenta que ya no existe, se cierra.
        self.menu.popdown();

        let found = self.lists.borrow().iter().find_map(|l| {
            let pos = l.targets.iter().position(|t| *t == Some(current))?;
            l.list.row_at_index(pos as i32)
        });
        if let Some(row) = found {
            if let Some(list) = row.parent().and_then(|p| p.downcast::<gtk::ListBox>().ok()) {
                list.select_row(Some(&row));
            }
        }
        self.rebuilding.set(false);
        if focused_unified {
            if let Some(row) = self
                .lists
                .borrow()
                .first()
                .and_then(|l| l.list.row_at_index(0))
            {
                row.grab_focus();
            }
        }
        if let Some((account, key)) = focused_folder {
            let row = self.trees.borrow().iter().find_map(|t| {
                (t.account == account)
                    .then(|| t.rows.iter().find(|r| r.key == key).map(|r| r.row.clone()))
                    .flatten()
            });
            if let Some(row) = row.filter(|r| r.is_visible()) {
                row.grab_focus();
            }
        }
        if let Some(id) = focused_header {
            let header = self
                .sections
                .borrow()
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.header.clone());
            if let Some(header) = header {
                header.grab_focus();
            }
        }
    }

    /// Crea una lista de filas seleccionables y conecta selección única entre listas.
    fn target_list(&self, rows: Vec<(gtk::ListBoxRow, Option<Target>)>) -> TargetList {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.add_css_class("correo-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);
        let mut targets = Vec::with_capacity(rows.len());
        for (row, target) in rows {
            list.append(&row);
            targets.push(target);
        }
        let targets = Rc::new(targets);

        {
            let targets = Rc::clone(&targets);
            let on_select = Rc::clone(&self.on_select);
            let lists = Rc::clone(&self.lists);
            let rebuilding = Rc::clone(&self.rebuilding);
            list.connect_row_selected(move |this, row| {
                let Some(row) = row else { return };
                // Selección única entre listas.
                if let Ok(others) = lists.try_borrow() {
                    for other in others.iter().filter(|o| o.list != *this) {
                        other.list.unselect_all();
                    }
                }
                if rebuilding.get() {
                    return;
                }
                let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
                if let Some(Some(target)) = targets.get(index).copied() {
                    // Elegir destino reconstruye la barra (y esta fila deja de tener raíz).
                    // Si se hiciera aquí, GTK seguiría procesando el clic o el movimiento de
                    // foco sobre una fila ya desmontada (`gtk_root_get_focus`: GTK_IS_ROOT).
                    let on_select = Rc::clone(&on_select);
                    gtk::glib::idle_add_local_once(move || on_select(target));
                }
            });
        }
        {
            // Pulsar una carpeta (aunque ya estuviera elegida) abre su lista en modo estrecho.
            // Se usa el clic en la lista y no `row-activated`: elegir una carpeta reconstruye
            // las filas, y la fila pulsada ya no existiría al soltar el botón.
            let on_activate = Rc::clone(&self.on_activate);
            list.connect_row_activated({
                let on_activate = Rc::clone(&on_activate);
                let targets = Rc::clone(&targets);
                move |_, row| {
                    let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
                    if matches!(targets.get(index), Some(Some(_))) {
                        on_activate();
                    }
                }
            });
            let click = gtk::GestureClick::new();
            click.set_button(1);
            let list_for_rows = list.clone();
            let targets = Rc::clone(&targets);
            click.connect_released(move |_, _, _, y| {
                let selectable = list_for_rows
                    .row_at_y(y as i32)
                    .and_then(|row| usize::try_from(row.index()).ok())
                    .is_some_and(|i| matches!(targets.get(i), Some(Some(_))));
                if selectable {
                    on_activate();
                }
            });
            list.add_controller(click);
        }
        TargetList { list, targets }
    }
}

fn target_row(icon: &str, label: &str, unread: u32, accent: bool) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);
    hbox.set_margin_top(4);
    hbox.set_margin_bottom(4);

    hbox.append(&gtk::Image::from_icon_name(icon));
    let name = gtk::Label::new(Some(label));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    hbox.append(&name);

    let mut description = label.to_string();
    if unread > 0 {
        let badge = gtk::Label::new(Some(&unread.to_string()));
        badge.add_css_class("correo-badge");
        if accent {
            badge.add_css_class("correo-badge-accent");
        }
        hbox.append(&badge);
        description = format!("{label}, {unread} sin leer");
    }
    row.set_child(Some(&hbox));
    row.update_property(&[gtk::accessible::Property::Label(&description)]);
    row
}

/// Cambia el plegado de una sección, lo guarda y lo aplica con animación.
fn set_collapsed(state: &Shared<UiState>, section: &Section, collapsed: bool, animate: bool) {
    let changed = state.borrow().is_collapsed(section.id) != collapsed;
    if !changed {
        return;
    }
    state.borrow_mut().set_collapsed(section.id, collapsed);
    state.borrow().save();
    apply_fold(section, collapsed, animate);
}

/// Alterna el plegado de una sección, lo guarda y lo anima.
fn toggle_collapsed(state: &Shared<UiState>, section: &Section) {
    let collapsed = state.borrow_mut().toggle(section.id);
    state.borrow().save();
    apply_fold(section, collapsed, true);
}

/// Refleja el estado de plegado en los widgets de la sección.
fn apply_fold(section: &Section, collapsed: bool, animate: bool) {
    let duration = if animate && ui_style::animations_enabled(&section.revealer) {
        FOLD_MS
    } else {
        0
    };
    section.revealer.set_transition_duration(duration);
    section.revealer.set_reveal_child(!collapsed);
    if collapsed {
        section.chevron.add_css_class("correo-collapsed");
    } else {
        section.chevron.remove_css_class("correo-collapsed");
    }
    // La insignia sustituye a los contadores de las carpetas mientras están ocultas.
    section.badge.set_visible(collapsed && section.unread > 0);
    let tip = if collapsed { "Desplegar" } else { "Plegar" };
    section.header.set_tooltip_text(Some(tip));
    section.chevron.set_tooltip_text(Some(tip));
    section.header.update_property(&[
        gtk::accessible::Property::Label(&section.email),
        gtk::accessible::Property::Description(&format!("{tip} cuenta")),
    ]);
    section
        .header
        .update_state(&[gtk::accessible::State::Expanded(Some(!collapsed))]);
}

impl Sidebar {
    /// Cabecera de una cuenta: avatar de su color, email, insignia de no leídos (solo plegada),
    /// botón de menú (alternativa por teclado y táctil al clic derecho) y chevron. Pulsarla,
    /// o Enter/Espacio con foco, alterna el plegado; izquierda pliega y derecha despliega.
    fn header_section(
        &self,
        account: &Account,
        index: usize,
        unread: u32,
        accent: bool,
        revealer: gtk::Revealer,
    ) -> Section {
        let avatar = adw::Avatar::new(24, Some(&account.email), true);
        ui_style::set_tone(&avatar, &ui_style::account_class(index));

        let label = gtk::Label::new(Some(&account.email));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.add_css_class("heading");

        let badge = gtk::Label::new(Some(&unread.to_string()));
        badge.add_css_class("correo-badge");
        if accent {
            badge.add_css_class("correo-badge-accent");
        }
        badge.set_visible(false);

        let more = gtk::Button::from_icon_name("view-more-symbolic");
        more.add_css_class("flat");
        more.add_css_class("circular");
        more.add_css_class("correo-account-more");
        more.set_tooltip_text(Some("Opciones de la cuenta"));
        more.update_property(&[gtk::accessible::Property::Label("Opciones de la cuenta")]);
        {
            let menu = self.menu.clone();
            let menu_account = Rc::clone(&self.menu_account);
            let id = account.id;
            more.connect_clicked(move |button| {
                let Some(parent) = menu.parent() else { return };
                let Some(bounds) = button.compute_bounds(&parent) else {
                    return;
                };
                menu_account.set(Some(id));
                menu.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                    bounds.x() as i32,
                    bounds.y() as i32,
                    bounds.width() as i32,
                    bounds.height() as i32,
                )));
                menu.popup();
            });
        }

        let chevron = gtk::Image::from_icon_name("pan-down-symbolic");
        chevron.add_css_class("correo-chevron");

        // El botón plano ocupa todo el ancho útil; el de opciones y el chevron van aparte
        // para que sus clics no se confundan con los del plegado.
        let inner = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        inner.append(&avatar);
        inner.append(&label);
        inner.append(&badge);
        let toggle = gtk::Button::new();
        toggle.set_child(Some(&inner));
        toggle.add_css_class("flat");
        toggle.add_css_class("correo-account-toggle");
        toggle.set_hexpand(true);
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        hbox.add_css_class("correo-account-header");
        hbox.append(&toggle);
        hbox.append(&more);
        hbox.append(&chevron);

        let section = Section {
            id: account.id,
            email: account.email.clone(),
            unread,
            header: toggle.clone(),
            row: hbox.clone(),
            revealer,
            chevron,
            badge,
        };

        // Clic primario en el botón (o Enter/Espacio con foco) o en el chevron: alterna.
        {
            let (state_b, section_b) = (Rc::clone(&self.state), section.clone());
            toggle.connect_clicked(move |_| toggle_collapsed(&state_b, &section_b));
            let click = gtk::GestureClick::new();
            click.set_button(1);
            let (state_c, section_c) = (Rc::clone(&self.state), section.clone());
            click.connect_released(move |_, _, _, _| {
                section_c.header.grab_focus();
                toggle_collapsed(&state_c, &section_c);
            });
            section.chevron.add_controller(click);
        }
        // Clic derecho: menú contextual de la cuenta (no alterna el plegado).
        {
            let click = gtk::GestureClick::new();
            click.set_button(3);
            let menu = self.menu.clone();
            let menu_account = Rc::clone(&self.menu_account);
            let header = hbox.clone();
            let id = account.id;
            click.connect_pressed(move |_, _, x, y| {
                let Some(parent) = menu.parent() else { return };
                let point = gtk::graphene::Point::new(x as f32, y as f32);
                let Some(point) = header.compute_point(&parent, &point) else {
                    return;
                };
                menu_account.set(Some(id));
                menu.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                    point.x() as i32,
                    point.y() as i32,
                    1,
                    1,
                )));
                menu.popup();
            });
            hbox.add_controller(click);
        }
        // Teclado, solo con el foco en la propia cabecera (no en el botón de opciones).
        {
            let keys = gtk::EventControllerKey::new();
            let state = Rc::clone(&self.state);
            let section = section.clone();
            keys.connect_key_pressed(move |_, key, _, _| {
                use gtk::gdk::Key;
                // Enter y Espacio ya los gestiona el botón.
                match key {
                    Key::Left | Key::KP_Left => set_collapsed(&state, &section, true, true),
                    Key::Right | Key::KP_Right => set_collapsed(&state, &section, false, true),
                    _ => return gtk::glib::Propagation::Proceed,
                }
                gtk::glib::Propagation::Stop
            });
            toggle.add_controller(keys);
        }
        section
    }
}

fn role_icon(role: MailboxRole) -> &'static str {
    match role {
        MailboxRole::Inbox => ui_style::icon_or("mail-inbox-symbolic", "mail-unread-symbolic"),
        MailboxRole::Drafts => "document-edit-symbolic",
        MailboxRole::Sent => "mail-send-symbolic",
        MailboxRole::Archive => ui_style::icon_or("mail-archive-symbolic", "folder-symbolic"),
        MailboxRole::Spam => "mail-mark-junk-symbolic",
        MailboxRole::Trash => "user-trash-symbolic",
        MailboxRole::Other => "folder-symbolic",
    }
}

// ----- Árbol de carpetas ----------------------------------------------------

/// Sangría por nivel, en píxeles.
const INDENT: i32 = 16;
/// Ancho de la columna del chevron de plegado, en píxeles.
const CHEVRON_W: i32 = 22;

/// Aplana el árbol en orden de recorrido: (nodo, nivel, índice del padre).
fn flatten<'a>(
    nodes: &'a [Node],
    level: usize,
    parent: Option<usize>,
    out: &mut Vec<(&'a Node, usize, Option<usize>)>,
) {
    for node in nodes {
        let index = out.len();
        out.push((node, level, parent));
        flatten(&node.children, level + 1, Some(index), out);
    }
}

impl Sidebar {
    /// Crea la lista de una cuenta con una fila por nodo del árbol y conecta el plegado
    /// (chevron, contenedores y teclado).
    fn build_tree(&self, account: AccountId, nodes: &[Node]) -> (TargetList, Tree) {
        let mut flat = Vec::new();
        flatten(nodes, 0, None, &mut flat);
        let with_chevrons = flat.iter().any(|(n, ..)| !n.children.is_empty());

        let mut rows = Vec::with_capacity(flat.len());
        let mut targets = Vec::with_capacity(flat.len());
        for (node, level, parent) in &flat {
            rows.push(folder_row(node, *level, *parent, with_chevrons));
            targets.push(node.mailbox.map(Target::Mailbox));
        }
        let list = self.target_list(
            rows.iter()
                .map(|r| r.row.clone())
                .zip(targets.iter().copied())
                .collect(),
        );
        let tree = Tree {
            account,
            list: list.list.clone(),
            rows: Rc::new(rows),
        };
        // Estado inicial sin animación.
        apply_tree(&self.state, &tree, false);
        let present: std::collections::BTreeSet<String> =
            tree.rows.iter().map(|r| r.key.clone()).collect();
        let before = self.state.borrow().clone();
        self.state.borrow_mut().retain_folders(account, &present);
        if *self.state.borrow() != before {
            self.state.borrow().save();
        }

        for (i, tr) in tree.rows.iter().enumerate() {
            if !tr.has_children {
                continue;
            }
            // El chevron pliega sin seleccionar: reclama el clic para que la lista no lo vea.
            let click = gtk::GestureClick::new();
            click.set_button(1);
            let (state, tree) = (Rc::clone(&self.state), tree.clone());
            click.connect_pressed(move |gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                toggle_node(&state, &tree, i, None);
            });
            tr.chevron.set_cursor_from_name(Some("pointer"));
            tr.chevron.add_controller(click);
        }
        {
            // Pulsar un contenedor (no seleccionable) o pulsar Enter sobre él lo pliega.
            let (state, tree) = (Rc::clone(&self.state), tree.clone());
            let targets = Rc::new(targets);
            tree.list.clone().connect_row_activated(move |_, row| {
                let Ok(i) = usize::try_from(row.index()) else {
                    return;
                };
                if matches!(targets.get(i), Some(None)) && tree.rows[i].has_children {
                    toggle_node(&state, &tree, i, None);
                }
            });
        }
        {
            // Derecha despliega (o baja al primer hijo); izquierda pliega (o sube al padre).
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let list_widget = tree.list.clone();
            let (state, tree) = (Rc::clone(&self.state), tree.clone());
            keys.connect_key_pressed(move |_, key, _, modifiers| {
                use gtk::gdk::Key;
                if modifiers.intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK
                        | gtk::gdk::ModifierType::ALT_MASK
                        | gtk::gdk::ModifierType::SHIFT_MASK,
                ) {
                    return gtk::glib::Propagation::Proceed;
                }
                let Some(i) = tree.rows.iter().position(|r| r.row.is_focus()) else {
                    return gtk::glib::Propagation::Proceed;
                };
                let tr = &tree.rows[i];
                let collapsed = tr.has_children && node_collapsed(&state, &tree, i);
                match key {
                    Key::Right | Key::KP_Right if tr.has_children => {
                        if collapsed {
                            toggle_node(&state, &tree, i, Some(false));
                        } else if let Some(child) = tree.rows.get(i + 1) {
                            child.row.grab_focus();
                        }
                    }
                    Key::Left | Key::KP_Left => {
                        if tr.has_children && !collapsed {
                            toggle_node(&state, &tree, i, Some(true));
                        } else if let Some(parent) = tr.parent {
                            tree.rows[parent].row.grab_focus();
                        } else {
                            return gtk::glib::Propagation::Proceed;
                        }
                    }
                    _ => return gtk::glib::Propagation::Proceed,
                }
                gtk::glib::Propagation::Stop
            });
            list_widget.add_controller(keys);
        }
        (list, tree)
    }
}

/// Fila de un nodo: sangría, chevron (si hay árbol), icono, nombre e insignia.
fn folder_row(node: &Node, level: usize, parent: Option<usize>, with_chevrons: bool) -> TreeRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("correo-folder-row");
    row.set_selectable(node.mailbox.is_some());

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    content.add_css_class("correo-folder-content");
    content.set_margin_start(4 + INDENT * i32::try_from(level).unwrap_or(0));
    content.set_margin_end(8);

    let has_children = !node.children.is_empty();
    let chevron = gtk::Image::from_icon_name("pan-down-symbolic");
    chevron.add_css_class("correo-chevron");
    if with_chevrons {
        if has_children {
            chevron.set_size_request(CHEVRON_W, -1);
            content.append(&chevron);
        } else {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_size_request(CHEVRON_W, -1);
            content.append(&spacer);
        }
    }
    let icon = gtk::Image::from_icon_name(role_icon(node.role));
    icon.set_margin_end(6);
    content.append(&icon);
    let name = gtk::Label::new(Some(&node.name));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&name);
    let badge = gtk::Label::new(None);
    badge.add_css_class("correo-badge");
    if matches!(node.role, MailboxRole::Inbox | MailboxRole::Drafts) {
        badge.add_css_class("correo-badge-accent");
    }
    badge.set_visible(false);
    content.append(&badge);

    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    revealer.set_transition_duration(0);
    revealer.set_reveal_child(true);
    revealer.set_child(Some(&content));
    row.set_child(Some(&revealer));
    {
        // Al terminar de plegarse, la fila se oculta del todo (el teclado la salta).
        let row = row.clone();
        revealer.connect_child_revealed_notify(move |rev| {
            if !rev.is_child_revealed() && !rev.reveals_child() {
                row.set_visible(false);
            }
        });
    }
    TreeRow {
        key: node.key.clone(),
        default_collapsed: node.default_collapsed,
        parent,
        has_children,
        row,
        revealer,
        chevron,
        badge,
        name: node.name.clone(),
        own_unread: node.unread,
        total_unread: node.total_unread(),
    }
}

fn node_collapsed(state: &Shared<UiState>, tree: &Tree, i: usize) -> bool {
    let tr = &tree.rows[i];
    state
        .borrow()
        .folder_collapsed(tree.account, &tr.key, tr.default_collapsed)
}

/// Pliega (`Some(true)`), despliega (`Some(false)`) o alterna (`None`) un nodo; lo guarda
/// y anima los cambios.
fn toggle_node(state: &Shared<UiState>, tree: &Tree, i: usize, collapsed: Option<bool>) {
    let tr = &tree.rows[i];
    let current = node_collapsed(state, tree, i);
    let wanted = collapsed.unwrap_or(!current);
    if wanted == current {
        return;
    }
    state
        .borrow_mut()
        .set_folder_collapsed(tree.account, &tr.key, wanted, tr.default_collapsed);
    state.borrow().save();
    apply_tree(state, tree, true);
}

/// Refleja el plegado en todas las filas: visibilidad (con animación), chevron, insignia
/// (la suma de los hijos solo se ve con el nodo plegado) y estado accesible.
fn apply_tree(state: &Shared<UiState>, tree: &Tree, animate: bool) {
    let mut collapsed = vec![false; tree.rows.len()];
    let mut visible = vec![true; tree.rows.len()];
    for (i, tr) in tree.rows.iter().enumerate() {
        collapsed[i] = tr.has_children && node_collapsed(state, tree, i);
        visible[i] = tr.parent.is_none_or(|p| visible[p] && !collapsed[p]);
    }
    for (i, tr) in tree.rows.iter().enumerate() {
        let duration = if animate && ui_style::animations_enabled(&tr.revealer) {
            FOLD_MS
        } else {
            0
        };
        tr.revealer.set_transition_duration(duration);
        if visible[i] {
            tr.row.set_visible(true);
            tr.revealer.set_reveal_child(true);
        } else {
            tr.revealer.set_reveal_child(false);
            if duration == 0 || !tr.revealer.is_mapped() {
                tr.row.set_visible(false);
            }
        }
        let unread = if collapsed[i] {
            tr.total_unread
        } else {
            tr.own_unread
        };
        tr.badge.set_label(&unread.to_string());
        tr.badge.set_visible(unread > 0);
        let mut label = tr.name.clone();
        if unread > 0 {
            label = format!("{label}, {unread} sin leer");
        }
        tr.row
            .update_property(&[gtk::accessible::Property::Label(&label)]);
        if tr.has_children {
            if collapsed[i] {
                tr.chevron.add_css_class("correo-collapsed");
            } else {
                tr.chevron.remove_css_class("correo-collapsed");
            }
            let tip = if collapsed[i] { "Desplegar" } else { "Plegar" };
            tr.chevron.set_tooltip_text(Some(tip));
            tr.row
                .update_state(&[gtk::accessible::State::Expanded(Some(!collapsed[i]))]);
        }
    }
}

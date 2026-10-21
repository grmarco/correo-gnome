//! T9: barra lateral con la bandeja unificada y las carpetas de cada cuenta,
//! ordenadas por rol (Entrada, Borradores, Enviados, Archivo, Spam, Papelera, resto).
//! Cada cuenta se identifica por su email (varias cuentas del mismo proveedor pueden
//! tener el mismo nombre). Con el botón derecho sobre la cuenta se ofrece «Eliminar cuenta».
//!
//! Cada cuenta es una sección plegable: cabecera enfocable (avatar, email, insignia de no
//! leídos si está plegada, opciones y chevron) y, debajo, una lista de carpetas dentro de
//! un `gtk::Revealer`. La selección es única entre todas las listas. El estado de plegado
//! se guarda en `ui_state` y se conserva al reconstruir la barra.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use correo_core::{Account, AccountId, Mailbox, MailboxId, MailboxRole};
use gtk::gio;

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
    targets: Rc<Vec<Target>>,
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

    /// Reconstruye la barra con las cuentas y sus carpetas, y selecciona `current`
    /// sin que ello cambie de destino (el llamador comprueba si ya estaba activo).
    /// El plegado de cada cuenta se conserva.
    pub fn reload(&self, accounts: &[(Account, Vec<Mailbox>)], current: Target) {
        // Recuerda si el foco estaba en una cabecera para devolvérselo a la nueva.
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
            Target::Unified,
        )];
        let list = self.target_list(unified);
        self.content.append(&list.list);
        self.lists.borrow_mut().push(list);

        for (index, (account, boxes)) in accounts.iter().enumerate() {
            let mut sorted: Vec<&Mailbox> = boxes.iter().collect();
            sorted.sort_by(|a, b| {
                role_rank(a.role)
                    .cmp(&role_rank(b.role))
                    .then_with(|| a.path.cmp(&b.path))
            });
            let rows = sorted
                .iter()
                .map(|m| {
                    (
                        target_row(
                            role_icon(m.role),
                            &mailbox_label(m),
                            m.unread,
                            matches!(m.role, MailboxRole::Inbox | MailboxRole::Drafts),
                        ),
                        Target::Mailbox(m.id),
                    )
                })
                .collect();
            let list = self.target_list(rows);

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
            self.sections.borrow_mut().push(section);
        }

        // Si el menú sigue abierto sobre una cuenta que ya no existe, se cierra.
        self.menu.popdown();

        let found = self.lists.borrow().iter().find_map(|l| {
            let pos = l.targets.iter().position(|t| *t == current)?;
            l.list.row_at_index(pos as i32)
        });
        if let Some(row) = found {
            if let Some(list) = row.parent().and_then(|p| p.downcast::<gtk::ListBox>().ok()) {
                list.select_row(Some(&row));
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
    fn target_list(&self, rows: Vec<(gtk::ListBoxRow, Target)>) -> TargetList {
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
            list.connect_row_selected(move |this, row| {
                let Some(row) = row else { return };
                // Selección única entre listas.
                if let Ok(others) = lists.try_borrow() {
                    for other in others.iter().filter(|o| o.list != *this) {
                        other.list.unselect_all();
                    }
                }
                let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
                if let Some(target) = targets.get(index).copied() {
                    on_select(target);
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
                move |_, _| on_activate()
            });
            let click = gtk::GestureClick::new();
            click.set_button(1);
            let list_for_rows = list.clone();
            click.connect_released(move |_, _, _, y| {
                if list_for_rows.row_at_y(y as i32).is_some() {
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

fn role_rank(role: MailboxRole) -> u8 {
    match role {
        MailboxRole::Inbox => 0,
        MailboxRole::Drafts => 1,
        MailboxRole::Sent => 2,
        MailboxRole::Archive => 3,
        MailboxRole::Spam => 4,
        MailboxRole::Trash => 5,
        MailboxRole::Other => 6,
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

/// Nombre visible de una carpeta: los roles tienen nombre fijo en castellano y el
/// resto usa el último segmento de su ruta IMAP.
fn mailbox_label(m: &Mailbox) -> String {
    match m.role {
        MailboxRole::Inbox => "Entrada".to_string(),
        MailboxRole::Drafts => "Borradores".to_string(),
        MailboxRole::Sent => "Enviados".to_string(),
        MailboxRole::Archive => "Archivo".to_string(),
        MailboxRole::Spam => "Spam".to_string(),
        MailboxRole::Trash => "Papelera".to_string(),
        MailboxRole::Other => m.path.rsplit('/').next().unwrap_or(&m.path).to_string(),
    }
}

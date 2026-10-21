//! T9: barra lateral con la bandeja unificada y las carpetas de cada cuenta,
//! ordenadas por rol (Entrada, Borradores, Enviados, Archivo, Spam, Papelera, resto).
//! Cada cuenta se identifica por su email (varias cuentas del mismo proveedor pueden
//! tener el mismo nombre). Con el botón derecho sobre la cuenta se ofrece «Eliminar cuenta».

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use correo_core::{Account, AccountId, Mailbox, MailboxId, MailboxRole};

/// Destino seleccionable en la barra lateral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Entradas de todas las cuentas.
    Unified,
    /// Una carpeta concreta.
    Mailbox(MailboxId),
}

pub struct Sidebar {
    page: adw::NavigationPage,
    add_button: gtk::Button,
    list: gtk::ListBox,
    /// Destino de cada fila, por posición (las cabeceras de cuenta no tienen destino).
    targets: Rc<RefCell<Vec<Option<Target>>>>,
    /// Cuenta de cada cabecera, por posición (`None` en el resto de filas).
    owners: Rc<RefCell<Vec<Option<AccountId>>>>,
    /// Menú «Eliminar cuenta», con un único popover reutilizado para todas las cuentas.
    menu: gtk::Popover,
}

impl Sidebar {
    /// `on_select` recibe el destino elegido; `on_remove` la cuenta que el usuario quiere
    /// eliminar desde el menú contextual (la confirmación la hace quien recibe el aviso).
    pub fn new(
        on_select: impl Fn(Target) + 'static,
        on_remove: impl Fn(AccountId) + 'static,
    ) -> Self {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);

        let targets: Rc<RefCell<Vec<Option<Target>>>> = Rc::new(RefCell::new(Vec::new()));
        let owners: Rc<RefCell<Vec<Option<AccountId>>>> = Rc::new(RefCell::new(Vec::new()));
        let on_select = Rc::new(on_select);
        {
            let targets = targets.clone();
            list.connect_row_selected(move |_, row| {
                let Some(row) = row else { return };
                let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
                let target = targets.borrow().get(index).copied().flatten();
                if let Some(target) = target {
                    on_select(target);
                }
            });
        }

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_vexpand(true);
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_child(Some(&list));

        // Menú contextual de la cabecera de una cuenta. Su padre es el desplazador, no la
        // lista: `ListBox` no admite hijos que no sean filas.
        let menu_account: Rc<Cell<Option<AccountId>>> = Rc::new(Cell::new(None));
        let menu = gtk::Popover::new();
        menu.set_has_arrow(false);
        menu.set_parent(&scroller);
        let remove = gtk::Button::with_label("Eliminar cuenta");
        remove.add_css_class("flat");
        remove.add_css_class("destructive-action");
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
        {
            let click = gtk::GestureClick::new();
            // Botón secundario (menú contextual).
            click.set_button(3);
            let owners = Rc::clone(&owners);
            let menu = menu.clone();
            let menu_account = Rc::clone(&menu_account);
            let list_for_rows = list.clone();
            let scroller = scroller.clone();
            click.connect_pressed(move |_, _, x, y| {
                let Some(row) = list_for_rows.row_at_y(y as i32) else {
                    return;
                };
                let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
                let Some(Some(id)) = owners.borrow().get(index).copied() else {
                    return;
                };
                let point = gtk::graphene::Point::new(x as f32, y as f32);
                let Some(point) = list_for_rows.compute_point(&scroller, &point) else {
                    return;
                };
                let (x, y) = (point.x() as f64, point.y() as f64);
                menu_account.set(Some(id));
                menu.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                menu.popup();
            });
            list.add_controller(click);
        }

        let add_button = gtk::Button::from_icon_name("list-add-symbolic");
        add_button.set_tooltip_text(Some("Añadir cuenta"));
        let header = adw::HeaderBar::new();
        header.pack_start(&add_button);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroller));

        let page = adw::NavigationPage::new(&toolbar, "Carpetas");
        Self {
            page,
            add_button,
            list,
            targets,
            owners,
            menu,
        }
    }

    pub fn page(&self) -> &adw::NavigationPage {
        &self.page
    }

    /// Se invoca al pulsar «Añadir cuenta» en la cabecera de la barra lateral.
    pub fn connect_add_account(&self, f: impl Fn() + 'static) {
        self.add_button.connect_clicked(move |_| f());
    }

    /// Reconstruye la barra con las cuentas y sus carpetas, y selecciona `current`
    /// sin que ello cambie de destino (el llamador comprueba si ya estaba activo).
    pub fn reload(&self, accounts: &[(Account, Vec<Mailbox>)], current: Target) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }

        let mut targets: Vec<Option<Target>> = Vec::new();
        let mut owners: Vec<Option<AccountId>> = Vec::new();
        let mut rows: Vec<gtk::ListBoxRow> = Vec::new();

        let unified_unread: u32 = accounts
            .iter()
            .flat_map(|(_, boxes)| boxes)
            .filter(|m| m.role == MailboxRole::Inbox)
            .map(|m| m.unread)
            .sum();
        rows.push(target_row(
            "mail-unread-symbolic",
            "Todas las bandejas de entrada",
            unified_unread,
        ));
        targets.push(Some(Target::Unified));
        owners.push(None);

        for (account, boxes) in accounts {
            // El email es el título: dos cuentas pueden tener el mismo nombre visible.
            rows.push(header_row(&account.email));
            targets.push(None);
            owners.push(Some(account.id));

            let mut sorted: Vec<&Mailbox> = boxes.iter().collect();
            sorted.sort_by(|a, b| {
                role_rank(a.role)
                    .cmp(&role_rank(b.role))
                    .then_with(|| a.path.cmp(&b.path))
            });
            for m in sorted {
                rows.push(target_row(role_icon(m.role), &mailbox_label(m), m.unread));
                targets.push(Some(Target::Mailbox(m.id)));
                owners.push(None);
            }
        }

        let mut selected_row = None;
        for (i, row) in rows.iter().enumerate() {
            self.list.append(row);
            if targets[i] == Some(current) {
                selected_row = Some(row.clone());
            }
        }
        *self.targets.borrow_mut() = targets;
        *self.owners.borrow_mut() = owners;
        // Si el menú sigue abierto sobre una cuenta que ya no existe, se cierra.
        self.menu.popdown();

        if let Some(row) = selected_row {
            self.list.select_row(Some(&row));
        }
    }
}

fn target_row(icon: &str, label: &str, unread: u32) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);
    hbox.set_margin_top(6);
    hbox.set_margin_bottom(6);

    hbox.append(&gtk::Image::from_icon_name(icon));
    let name = gtk::Label::new(Some(label));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    hbox.append(&name);

    if unread > 0 {
        let badge = gtk::Label::new(Some(&unread.to_string()));
        badge.add_css_class("numeric");
        badge.add_css_class("accent");
        hbox.append(&badge);
    }
    row.set_child(Some(&hbox));
    row
}

fn header_row(title: &str) -> gtk::ListBoxRow {
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_margin_start(8);
    label.set_margin_top(12);
    label.set_margin_bottom(4);
    label.add_css_class("heading");
    label.add_css_class("dim-label");

    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    row.set_child(Some(&label));
    row
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
        MailboxRole::Inbox => "mail-unread-symbolic",
        MailboxRole::Drafts => "document-edit-symbolic",
        MailboxRole::Sent => "mail-send-symbolic",
        MailboxRole::Archive => "folder-symbolic",
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

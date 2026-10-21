//! T9: barra lateral con la bandeja unificada y las carpetas de cada cuenta,
//! ordenadas por rol (Entrada, Borradores, Enviados, Archivo, Spam, Papelera, resto).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use correo_core::{Account, Mailbox, MailboxId, MailboxRole};

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
    list: gtk::ListBox,
    /// Destino de cada fila, por posición (las cabeceras de cuenta no tienen destino).
    targets: Rc<RefCell<Vec<Option<Target>>>>,
}

impl Sidebar {
    pub fn new(on_select: impl Fn(Target) + 'static) -> Self {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);

        let targets: Rc<RefCell<Vec<Option<Target>>>> = Rc::new(RefCell::new(Vec::new()));
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

        let header = adw::HeaderBar::new();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroller));

        let page = adw::NavigationPage::new(&toolbar, "Carpetas");
        Self {
            page,
            list,
            targets,
        }
    }

    pub fn page(&self) -> &adw::NavigationPage {
        &self.page
    }

    /// Reconstruye la barra con las cuentas y sus carpetas, y selecciona `current`
    /// sin que ello cambie de destino (el llamador comprueba si ya estaba activo).
    pub fn reload(&self, accounts: &[(Account, Vec<Mailbox>)], current: Target) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }

        let mut targets: Vec<Option<Target>> = Vec::new();
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

        for (account, boxes) in accounts {
            let title = if account.display_name.trim().is_empty() {
                account.email.clone()
            } else {
                format!("{} · {}", account.display_name, account.email)
            };
            rows.push(header_row(&title));
            targets.push(None);

            let mut sorted: Vec<&Mailbox> = boxes.iter().collect();
            sorted.sort_by(|a, b| {
                role_rank(a.role)
                    .cmp(&role_rank(b.role))
                    .then_with(|| a.path.cmp(&b.path))
            });
            for m in sorted {
                rows.push(target_row(role_icon(m.role), &mailbox_label(m), m.unread));
                targets.push(Some(Target::Mailbox(m.id)));
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

//! Interfaz de las acciones en lote: la barra contextual sobre la lista (contador, archivar,
//! borrar, leído/no leído, mover a…) y el estado «N conversaciones seleccionadas» del panel
//! derecho. Las acciones son acciones de ventana (`win.archive`, `win.delete`,
//! `win.toggle-read`, `win.move-to`, `win.select-all`, `win.clear-selection`), así que los
//! botones y los atajos de teclado comparten el mismo camino.

use adw::prelude::*;

use crate::batch;
use crate::ui_style;

pub struct BatchBar {
    revealer: gtk::Revealer,
    count: gtk::Label,
    read_btn: gtk::Button,
    /// Botones que necesitan una selección no vacía.
    needs_selection: Vec<gtk::Button>,
    move_btn: gtk::MenuButton,
    move_list: gtk::Box,
    popover: gtk::Popover,
    status: adw::StatusPage,
    status_read: adw::ButtonContent,
    announced: std::cell::RefCell<String>,
}

/// Botón plano solo de icono con tooltip y etiqueta accesible, ligado a una acción.
fn icon_button(icon: &str, label: &str, tooltip: &str, action: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tooltip));
    b.update_property(&[gtk::accessible::Property::Label(label)]);
    b.set_action_name(Some(action));
    b
}

impl BatchBar {
    pub fn new() -> Self {
        let clear = icon_button(
            "window-close-symbolic",
            "Cancelar la selección",
            "Cancelar la selección (Esc)",
            "win.clear-selection",
        );
        clear.add_css_class("circular");
        let count = gtk::Label::new(None);
        count.set_xalign(0.0);
        count.set_hexpand(true);
        count.set_ellipsize(gtk::pango::EllipsizeMode::End);
        count.add_css_class("correo-batch-count");
        count.add_css_class("correo-tabular");

        let archive = icon_button(
            ui_style::icon_or("mail-archive-symbolic", "folder-symbolic"),
            "Archivar la selección",
            "Archivar (Ctrl+E)",
            "win.archive",
        );
        let read_btn = icon_button(
            "mail-read-symbolic",
            "Marcar como leídos",
            "Marcar como leídos (Ctrl+U)",
            "win.toggle-read",
        );
        let popover = gtk::Popover::new();
        let move_list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        move_list.set_margin_top(6);
        move_list.set_margin_bottom(6);
        move_list.set_margin_start(6);
        move_list.set_margin_end(6);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(320);
        scroll.set_min_content_width(240);
        scroll.set_width_request(240);
        scroll.set_child(Some(&move_list));
        popover.set_child(Some(&scroll));
        let move_btn = gtk::MenuButton::new();
        move_btn.set_icon_name(ui_style::icon_or(
            "send-to-symbolic",
            "folder-open-symbolic",
        ));
        move_btn.add_css_class("flat");
        move_btn.set_popover(Some(&popover));
        move_btn.set_tooltip_text(Some("Mover a una carpeta…"));
        move_btn.update_property(&[gtk::accessible::Property::Label("Mover a una carpeta")]);
        let delete = icon_button(
            "user-trash-symbolic",
            "Borrar la selección",
            "Borrar (Supr)",
            "win.delete",
        );

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        row.add_css_class("correo-batch-bar");
        row.append(&clear);
        row.append(&count);
        row.append(&archive);
        row.append(&read_btn);
        row.append(&move_btn);
        row.append(&delete);
        row.set_accessible_role(gtk::AccessibleRole::Toolbar);
        row.update_property(&[gtk::accessible::Property::Label(
            "Acciones sobre la selección",
        )]);

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
        revealer.set_transition_duration(180);
        revealer.set_child(Some(&row));

        // Panel derecho con dos o más conversaciones seleccionadas.
        let status = adw::StatusPage::new();
        status.add_css_class("correo-empty");
        status.set_icon_name(Some(ui_style::icon_or(
            "selection-mode-symbolic",
            "object-select-symbolic",
        )));
        let pill = |icon: &str, label: &str, action: &str| {
            let content = adw::ButtonContent::builder()
                .icon_name(icon)
                .label(label)
                .build();
            let b = gtk::Button::new();
            b.set_child(Some(&content));
            b.add_css_class("pill");
            b.set_action_name(Some(action));
            (b, content)
        };
        let (a, _) = pill(
            ui_style::icon_or("mail-archive-symbolic", "folder-symbolic"),
            "Archivar",
            "win.archive",
        );
        let (r, status_read) = pill(
            "mail-read-symbolic",
            "Marcar como leídas",
            "win.toggle-read",
        );
        let (d, _) = pill("user-trash-symbolic", "Borrar", "win.delete");
        d.add_css_class("destructive-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::Center);
        actions.append(&a);
        actions.append(&r);
        actions.append(&d);
        status.set_child(Some(&actions));

        Self {
            revealer,
            count,
            needs_selection: vec![archive, delete],
            read_btn,
            move_btn,
            move_list,
            popover,
            status,
            status_read,
            announced: std::cell::RefCell::new(String::new()),
        }
    }

    /// Barra contextual para colocar sobre la lista.
    pub fn widget(&self) -> &gtk::Widget {
        self.revealer.upcast_ref()
    }

    /// Estado «N seleccionadas» del panel derecho.
    pub fn status_page(&self) -> &adw::StatusPage {
        &self.status
    }

    /// Contenedor de las carpetas del popover «Mover a…».
    pub fn move_list(&self) -> &gtk::Box {
        &self.move_list
    }

    pub fn popover(&self) -> &gtk::Popover {
        &self.popover
    }

    /// Actualiza la barra y el panel.
    ///
    /// * `n`: filas (conversaciones) seleccionadas.
    /// * `show`: barra visible.
    /// * `mark_read`: la acción de alternar marcará como leídos (si no, como no leídos).
    /// * `move_blocked`: motivo por el que no se puede mover (selección de varias cuentas).
    pub fn update(&self, n: usize, show: bool, mark_read: bool, move_blocked: Option<&str>) {
        let animate = adw::is_animations_enabled(&self.revealer);
        self.revealer
            .set_transition_duration(if animate { 180 } else { 0 });
        self.revealer.set_reveal_child(show);

        let text = batch::count_label(n);
        self.count.set_label(&text);
        if show && *self.announced.borrow() != text {
            // El lector de pantalla anuncia el número sin quitar el foco de la lista.
            self.count
                .announce(&text, gtk::AccessibleAnnouncementPriority::Low);
            *self.announced.borrow_mut() = text;
        } else if !show {
            self.announced.borrow_mut().clear();
        }

        let (icon, label, tip) = if mark_read {
            (
                "mail-read-symbolic",
                "Marcar como leídos",
                "Marcar como leídos (Ctrl+U)",
            )
        } else {
            (
                "mail-unread-symbolic",
                "Marcar como no leídos",
                "Marcar como no leídos (Ctrl+U)",
            )
        };
        self.read_btn.set_icon_name(icon);
        self.read_btn.set_tooltip_text(Some(tip));
        self.read_btn
            .update_property(&[gtk::accessible::Property::Label(label)]);
        self.status_read.set_icon_name(icon);
        self.status_read.set_label(if mark_read {
            "Marcar como leídas"
        } else {
            "Marcar como no leídas"
        });

        for b in self.needs_selection.iter().chain([&self.read_btn]) {
            b.set_sensitive(n > 0);
        }
        self.move_btn.set_sensitive(move_blocked.is_none() && n > 0);
        self.move_btn.set_tooltip_text(Some(match move_blocked {
            Some(reason) => reason,
            None => "Mover a una carpeta…",
        }));

        if n >= 2 {
            self.status
                .set_title(&format!("{n} conversaciones seleccionadas"));
            self.status.set_description(Some(
                "Elige una acción para aplicarla a todas, o pulsa Esc para cancelar la selección.",
            ));
        }
    }
}

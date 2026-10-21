//! Aviso en línea, discreto: icono, texto, botón de acción opcional y cierre opcional.
//! Sustituye a `adw::Banner` (franja a todo el ancho) en el lector y en la lista. Se
//! muestra y oculta con un `gtk::Revealer` que respeta «reducir animaciones».

use std::rc::Rc;

use adw::prelude::*;

/// Variante visual del aviso.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Informativo (privacidad, recortes): fondo neutro.
    Info,
    /// Algo falló: matiz de error discreto.
    Error,
}

#[derive(Clone)]
pub struct Notice {
    content: gtk::Box,
    revealer: gtk::Revealer,
    icon: gtk::Image,
    label: gtk::Label,
    button: gtk::Button,
}

impl Notice {
    /// `dismissable`: añade una ✕ que oculta el aviso (solo hasta el siguiente mensaje).
    pub fn new(icon: &str, tone: Tone, dismissable: bool) -> Self {
        let icon = gtk::Image::from_icon_name(icon);
        icon.add_css_class("correo-notice-icon");
        icon.set_valign(gtk::Align::Center);

        let label = gtk::Label::new(None);
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_valign(gtk::Align::Center);
        label.add_css_class("correo-notice-text");

        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("correo-notice-action");
        button.set_valign(gtk::Align::Center);
        button.set_visible(false);

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        content.add_css_class("correo-notice");
        if tone == Tone::Error {
            content.add_css_class("correo-notice-error");
        }
        content.set_accessible_role(gtk::AccessibleRole::Group);
        content.append(&icon);
        content.append(&label);
        content.append(&button);

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(180);
        revealer.set_child(Some(&content));
        revealer.set_reveal_child(false);

        if dismissable {
            let close = gtk::Button::from_icon_name("window-close-symbolic");
            close.add_css_class("flat");
            close.add_css_class("circular");
            close.add_css_class("correo-notice-close");
            close.set_valign(gtk::Align::Center);
            close.set_tooltip_text(Some("Descartar"));
            close.update_property(&[gtk::accessible::Property::Label("Descartar aviso")]);
            let r = revealer.clone();
            close.connect_clicked(move |_| r.set_reveal_child(false));
            content.append(&close);
        }
        Self {
            content,
            revealer,
            icon,
            label,
            button,
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.revealer.upcast_ref()
    }

    /// Márgenes pequeños, para avisos dentro de un panel estrecho (la lista).
    pub fn set_compact(&self) {
        self.content.add_css_class("correo-notice-compact");
    }

    /// Detalle que no cabe en el texto (se ve al pasar el ratón).
    pub fn set_detail(&self, text: Option<&str>) {
        self.content.set_tooltip_text(text);
    }

    pub fn set_title(&self, text: &str) {
        self.label.set_text(text);
    }

    pub fn set_icon(&self, icon: &str) {
        self.icon.set_icon_name(Some(icon));
    }

    /// `None` oculta el botón de acción.
    pub fn set_button_label(&self, label: Option<&str>) {
        match label {
            Some(text) => {
                self.button.set_label(text);
                self.button.set_visible(true);
            }
            None => self.button.set_visible(false),
        }
    }

    pub fn connect_button_clicked(&self, f: impl Fn() + 'static) {
        let f = Rc::new(f);
        self.button.connect_clicked(move |_| f());
    }

    /// Muestra u oculta; sin animaciones del sistema el cambio es inmediato.
    pub fn set_revealed(&self, revealed: bool) {
        let duration = if adw::is_animations_enabled(&self.revealer) {
            180
        } else {
            0
        };
        self.revealer.set_transition_duration(duration);
        self.revealer.set_reveal_child(revealed);
    }
}

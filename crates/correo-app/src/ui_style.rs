//! Estilo común de la interfaz: carga de `data/style.css`, colores por cuenta y
//! remitente, y utilidades de animación. Ver `docs/DESIGN.md`.

use adw::prelude::*;
use gtk::glib;

/// Hoja de estilos propia, incrustada en el binario.
const STYLE: &str = include_str!("../../../data/style.css");

/// Número de tonos de la paleta de avatares (`correo-acct-1..8`).
const TONES: usize = 8;

/// Carga la hoja de estilos para toda la aplicación. Se llama una vez, en `startup`.
pub fn load_css() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    #[cfg(debug_assertions)]
    provider.connect_parsing_error(|_, section, error| eprintln!("CSS: {section}: {error}"));
    provider.load_from_string(STYLE);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Clase de color de la cuenta en la posición `index` (rota cada 8 cuentas).
pub fn account_class(index: usize) -> String {
    format!("correo-acct-{}", index % TONES + 1)
}

/// Clase de color estable para un remitente: depende solo de su dirección (FNV-1a),
/// de modo que la misma persona tiene siempre el mismo color.
pub fn sender_class(email: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for b in email.trim().to_lowercase().bytes() {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    // Pliega los bits altos sobre los bajos: FNV varía poco en los bajos con textos cortos.
    account_class(((hash >> 16) ^ hash) as usize)
}

/// Sustituye la clase `correo-acct-N` de `widget` por `class`.
pub fn set_tone(widget: &impl IsA<gtk::Widget>, class: &str) {
    for n in 1..=TONES {
        widget.remove_css_class(&format!("correo-acct-{n}"));
    }
    widget.add_css_class("correo-avatar");
    widget.add_css_class(class);
}

/// Devuelve `name` si el tema de iconos actual lo trae, y `fallback` si no.
pub fn icon_or(name: &'static str, fallback: &'static str) -> &'static str {
    match gtk::gdk::Display::default() {
        Some(display) if gtk::IconTheme::for_display(&display).has_icon(name) => name,
        _ => fallback,
    }
}

/// `true` si las animaciones están activas (ajuste del sistema o de GTK).
pub fn animations_enabled(widget: &impl IsA<gtk::Widget>) -> bool {
    adw::is_animations_enabled(widget)
}

/// Aplica `class_enter`, y en el siguiente ciclo de reposo la cambia por `class_active`
/// (la transición CSS hace el resto). Sin animaciones, aplica el estado final directamente.
pub fn enter(
    widget: &impl IsA<gtk::Widget>,
    class_enter: &'static str,
    class_active: &'static str,
) {
    if !animations_enabled(widget) {
        return;
    }
    widget.add_css_class(class_enter);
    let weak = widget.downgrade();
    glib::idle_add_local_once(move || {
        if let Some(w) = weak.upgrade() {
            w.add_css_class(class_active);
            w.remove_css_class(class_enter);
            // Retira la clase activa cuando acaba la transición.
            glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                w.remove_css_class(class_active);
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_classes_cycle() {
        assert_eq!(account_class(0), "correo-acct-1");
        assert_eq!(account_class(7), "correo-acct-8");
        assert_eq!(account_class(8), "correo-acct-1");
    }

    #[test]
    fn sender_class_is_stable_and_case_insensitive() {
        assert_eq!(
            sender_class("Ana@Ejemplo.com"),
            sender_class(" ana@ejemplo.com")
        );
        assert!(sender_class("x@y.z").starts_with("correo-acct-"));
    }
}

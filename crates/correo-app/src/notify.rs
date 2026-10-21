//! T13: notificaciones nativas agrupadas por cuenta.
//!
//! Cada cuenta usa ids estables (`correo-new-<cuenta>-<n>`) para que un lote nuevo
//! reemplace al anterior en lugar de apilarse. Las acciones (`app.open-message`,
//! `app.mark-read`, `app.archive`) las registra window.rs.

use adw::prelude::*;
use correo_core::{Account, MessageHeader};

/// Notificaciones individuales máximas por lote; el resto se resume.
const MAX_INDIVIDUAL: usize = 3;

/// Muestra las notificaciones de un lote de mensajes nuevos de una cuenta.
/// Con más de `MAX_INDIVIDUAL` mensajes muestra las primeras individualmente y un
/// resumen «N mensajes nuevos». Con `show_account` (varias cuentas), el email de la cuenta
/// va en el título para distinguirlas.
pub fn notify_new(
    app: &adw::Application,
    account: &Account,
    msgs: &[MessageHeader],
    show_account: bool,
) {
    for (index, msg) in msgs.iter().take(MAX_INDIVIDUAL).enumerate() {
        let id = format!("correo-new-{}-{index}", account.id);
        app.send_notification(
            Some(&id),
            &message_notification(msg, show_account.then_some(account.email.as_str())),
        );
    }
    if msgs.len() > MAX_INDIVIDUAL {
        let id = format!("correo-new-{}-summary", account.id);
        let title = format!("{} mensajes nuevos", msgs.len());
        let heading = if show_account {
            format!("{title} · {}", account.email)
        } else {
            title
        };
        let notification = gtk::gio::Notification::new(&heading);
        notification.set_body(Some(&account.email));
        app.send_notification(Some(&id), &notification);
    }
}

/// Como `notify_new`, pero no hace nada si la ventana principal está activa (el usuario
/// ya ve la lista).
pub fn notify_new_if_inactive(
    app: &adw::Application,
    account: &Account,
    msgs: &[MessageHeader],
    window_active: bool,
    show_account: bool,
) {
    if window_active || msgs.is_empty() {
        return;
    }
    notify_new(app, account, msgs, show_account);
}

/// Notificación de un mensaje: título con el remitente (y la cuenta, si `account` es
/// `Some`), cuerpo con el asunto y los botones de acción. El destino `(xu)` es
/// (mailbox_id, uid).
fn message_notification(msg: &MessageHeader, account: Option<&str>) -> gtk::gio::Notification {
    let sender = msg
        .from
        .first()
        .map(|a| {
            a.name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| a.email.clone())
        })
        .unwrap_or_else(|| "Remitente desconocido".to_owned());
    let subject = if msg.subject.trim().is_empty() {
        "(sin asunto)"
    } else {
        msg.subject.as_str()
    };

    let title = match account {
        Some(email) => format!("{sender} · {email}"),
        None => sender,
    };
    let notification = gtk::gio::Notification::new(&title);
    notification.set_body(Some(subject));
    let target = (msg.mailbox_id, msg.uid).to_variant();
    notification.set_default_action_and_target_value("app.open-message", Some(&target));
    notification.add_button_with_target_value("Abrir", "app.open-message", Some(&target));
    notification.add_button_with_target_value("Marcar como leído", "app.mark-read", Some(&target));
    notification.add_button_with_target_value("Archivar", "app.archive", Some(&target));
    notification
}

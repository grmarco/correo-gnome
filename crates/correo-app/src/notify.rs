//! T13: notificaciones nativas.
use correo_core::{Account, MessageHeader};

/// Muestra una notificación de GNotification agrupada por cuenta.
/// `action` "app.open-message" recibe (mailbox_id, uid) como GVariant "(xu)".
pub fn notify_new(_app: &adw::Application, _account: &Account, _msgs: &[MessageHeader]) {
    todo!("T13")
}

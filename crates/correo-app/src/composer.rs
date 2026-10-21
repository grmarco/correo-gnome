//! T11: compositor (ventana independiente).
use std::rc::Rc;

use correo_core::Account;

use crate::services::Services;
use crate::ui_types::ComposeMode;

/// Abre una ventana de redacción para `account`.
pub fn open(_app: &adw::Application, _services: Rc<Services>, _account: Account, _mode: ComposeMode) {
    todo!("T11")
}

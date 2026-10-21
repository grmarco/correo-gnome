//! T10: lector de mensaje.
use std::rc::Rc;

use correo_core::{Account, Mailbox, MessageHeader};

use crate::services::Services;
use crate::ui_types::ComposeMode;

pub struct MessageView {
    widget: gtk::Widget,
}

impl MessageView {
    pub fn new(_services: Rc<Services>) -> Self {
        todo!("T10")
    }
    pub fn widget(&self) -> &gtk::Widget {
        &self.widget
    }
    /// Muestra el mensaje: descarga el cuerpo bajo demanda y marca como leído.
    pub fn show(&self, _account: Account, _mailbox: Mailbox, _header: MessageHeader) {
        todo!("T10")
    }
    pub fn clear(&self) {
        todo!("T10")
    }
    /// Se invoca al pulsar Responder/Responder a todos/Reenviar.
    pub fn connect_compose(&self, _f: impl Fn(ComposeMode) + 'static) {
        todo!("T10")
    }
}

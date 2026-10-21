//! Tipos compartidos entre módulos de la interfaz (fijados por el orquestador; no cambiar sin avisar).
use correo_core::{MessageBody, MessageHeader};

#[derive(Debug, Clone)]
pub enum ComposeMode {
    New,
    Reply(MessageHeader, MessageBody),
    ReplyAll(MessageHeader, MessageBody),
    Forward(MessageHeader, MessageBody),
}

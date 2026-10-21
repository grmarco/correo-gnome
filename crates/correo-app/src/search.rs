//! T13: búsqueda.
use correo_core::{MailboxId, MessageHeader};

use crate::services::Services;

/// Búsqueda local FTS5 (síncrona, rápida). Para búsqueda en servidor, ver T13.
pub fn search_local(_services: &Services, _query: &str, _limit: usize) -> Vec<MessageHeader> {
    todo!("T13")
}

#[allow(dead_code)]
type _M = MailboxId;

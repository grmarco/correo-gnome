//! T13: búsqueda.
//!
//! TODO (T13, búsqueda en servidor): `UID SEARCH TEXT` en IMAP no está expuesto por
//! `MailBackend` ni por `ImapBackend` (la sesión `conn` es privada en correo-imap).
//! Añadirlo requiere un método nuevo en correo-imap/correo-core, que este módulo no
//! toca. Cuando exista, `search_server` debe llamar a ese método solo si el backend
//! de la cuenta lo anuncia, y devolver cabeceras que se guarden en la caché.
#![allow(dead_code)]

use correo_core::MessageHeader;

use crate::services::Services;

/// Búsqueda local FTS5 (síncrona, rápida): asunto, remitente, extracto y cuerpo cacheado.
/// Una BD bloqueada o un error de consulta dan una lista vacía (la UI no debe caerse).
pub fn search_local(services: &Services, query: &str, limit: usize) -> Vec<MessageHeader> {
    let store = services.store.lock().unwrap_or_else(|p| p.into_inner());
    match store.search_messages(query, limit) {
        Ok(headers) => headers,
        Err(e) => {
            eprintln!("correo: búsqueda local fallida: {e}");
            Vec::new()
        }
    }
}

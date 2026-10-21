//! Cliente IMAP para correo: autenticación XOAUTH2 sobre TLS, listado de
//! carpetas, sincronización de cabeceras por ventana de días y acciones
//! (leído, mover, borrar), todo volcado a la caché de `correo-store`.

pub mod attach;
mod backend;
mod conn;
mod paging;
mod parse;
mod unread;

pub use backend::{GMAIL_HOST, IDLE_MAX, IMAPS_PORT, IdleEvent, ImapBackend, OUTLOOK_HOST};
pub use parse::{DEFAULT_MIN_MESSAGES, effective_min_messages};

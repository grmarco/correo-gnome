//! Cliente IMAP para correo: autenticación XOAUTH2 sobre TLS, listado de
//! carpetas, sincronización de cabeceras por ventana de días y acciones
//! (leído, mover, borrar), todo volcado a la caché de `correo-store`.

mod backend;
mod conn;
mod parse;

pub use backend::{GMAIL_HOST, IDLE_MAX, IMAPS_PORT, IdleEvent, ImapBackend, OUTLOOK_HOST};

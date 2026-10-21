//! Estado de «Cargar mensajes anteriores» por carpeta durante la sesión. Lógica pura (sin GTK):
//! qué carpetas hay que pedir y qué muestra la fila del final de la lista.

use std::collections::HashMap;

use correo_core::{MailboxId, OlderPage};

/// Mensajes que se piden por carga.
pub const OLDER_PAGE: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Loading,
    /// El servidor no tiene nada más antiguo: no se vuelve a pedir en esta sesión.
    Exhausted,
    Failed(String),
}

/// Qué muestra la fila del final de la lista.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Footer {
    /// Sin fila (hay filas locales por mostrar, búsqueda activa, carpeta cargándose…).
    #[default]
    Hidden,
    /// Botón «Cargar mensajes anteriores».
    Available,
    /// Spinner mientras se piden.
    Loading,
    /// Texto discreto «No hay mensajes más antiguos».
    Exhausted,
    /// Error con «Reintentar».
    Failed(String),
}

#[derive(Default)]
pub struct OlderTracker {
    states: HashMap<MailboxId, State>,
}

impl OlderTracker {
    /// Resumen para las carpetas de la vista (la unificada junta varias).
    pub fn footer(&self, mailboxes: &[MailboxId]) -> Footer {
        if mailboxes.is_empty() {
            return Footer::Hidden;
        }
        let mut failure = None;
        let mut all_exhausted = true;
        for id in mailboxes {
            match self.states.get(id) {
                Some(State::Loading) => return Footer::Loading,
                Some(State::Failed(e)) => {
                    failure.get_or_insert_with(|| e.clone());
                    all_exhausted = false;
                }
                Some(State::Exhausted) => {}
                None => all_exhausted = false,
            }
        }
        match failure {
            Some(e) => Footer::Failed(e),
            None if all_exhausted => Footer::Exhausted,
            None => Footer::Available,
        }
    }

    /// Carpetas a pedir ahora. Con `retry_failed == false` (carga automática al llegar al
    /// final) las que fallaron se saltan: solo el botón «Reintentar» las vuelve a pedir.
    pub fn to_request(&self, mailboxes: &[MailboxId], retry_failed: bool) -> Vec<MailboxId> {
        mailboxes
            .iter()
            .copied()
            .filter(|id| match self.states.get(id) {
                None => true,
                Some(State::Failed(_)) => retry_failed,
                Some(State::Loading | State::Exhausted) => false,
            })
            .collect()
    }

    pub fn start(&mut self, id: MailboxId) {
        self.states.insert(id, State::Loading);
    }

    /// Anota el resultado. Devuelve `true` si llegaron mensajes nuevos a la caché.
    pub fn finish(&mut self, id: MailboxId, result: Result<OlderPage, String>) -> bool {
        let new_messages = matches!(&result, Ok(page) if page.fetched > 0);
        match result {
            Ok(page) if page.exhausted => {
                self.states.insert(id, State::Exhausted);
            }
            Ok(page) if page.fetched == 0 => {
                // Ni mensajes ni fin: evita bucles de carga automática.
                self.states.insert(
                    id,
                    State::Failed("El servidor no devolvió mensajes anteriores.".into()),
                );
            }
            Ok(_) => {
                self.states.remove(&id);
            }
            Err(e) => {
                self.states.insert(id, State::Failed(e));
            }
        }
        new_messages
    }
}

/// ¿Se puede mostrar la fila? Solo cuando no queda nada local por enseñar, no hay búsqueda
/// y la carpeta no se está sincronizando (ni falló: entonces manda el aviso de error).
pub fn footer_allowed(has_query: bool, local_rows_left: bool, folder_idle: bool) -> bool {
    !has_query && !local_rows_left && folder_idle
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(fetched: usize, exhausted: bool) -> OlderPage {
        OlderPage { fetched, exhausted }
    }

    #[test]
    fn fresh_folders_offer_the_button() {
        let t = OlderTracker::default();
        assert_eq!(t.footer(&[1]), Footer::Available);
        assert_eq!(t.footer(&[]), Footer::Hidden);
        assert_eq!(t.to_request(&[1, 2], false), vec![1, 2]);
    }

    #[test]
    fn loading_then_more_available() {
        let mut t = OlderTracker::default();
        t.start(1);
        assert_eq!(t.footer(&[1]), Footer::Loading);
        assert!(t.to_request(&[1], true).is_empty(), "no se pide dos veces");
        assert!(t.finish(1, Ok(page(100, false))));
        assert_eq!(t.footer(&[1]), Footer::Available);
    }

    #[test]
    fn exhausted_is_remembered_and_not_requested_again() {
        let mut t = OlderTracker::default();
        t.start(1);
        assert!(t.finish(1, Ok(page(40, true))));
        assert_eq!(t.footer(&[1]), Footer::Exhausted);
        assert!(t.to_request(&[1], true).is_empty());
        // Unificada: otra cuenta aún tiene historial.
        assert_eq!(t.footer(&[1, 2]), Footer::Available);
        assert_eq!(t.to_request(&[1, 2], false), vec![2]);
    }

    #[test]
    fn failure_shows_error_and_only_manual_retry_asks_again() {
        let mut t = OlderTracker::default();
        t.start(1);
        assert!(!t.finish(1, Err("sin red".into())));
        assert_eq!(t.footer(&[1]), Footer::Failed("sin red".into()));
        assert!(t.to_request(&[1], false).is_empty(), "no automática");
        assert_eq!(t.to_request(&[1], true), vec![1]);
        t.start(1);
        assert_eq!(t.footer(&[1]), Footer::Loading);
    }

    #[test]
    fn empty_non_final_page_does_not_loop() {
        let mut t = OlderTracker::default();
        assert!(!t.finish(1, Ok(page(0, false))));
        assert!(matches!(t.footer(&[1]), Footer::Failed(_)));
        assert!(t.to_request(&[1], false).is_empty());
    }

    #[test]
    fn any_loading_or_failure_wins_over_exhausted() {
        let mut t = OlderTracker::default();
        t.finish(1, Ok(page(0, true)));
        t.start(2);
        assert_eq!(t.footer(&[1, 2]), Footer::Loading);
        t.finish(2, Err("x".into()));
        assert_eq!(t.footer(&[1, 2]), Footer::Failed("x".into()));
    }

    #[test]
    fn footer_visibility_rules() {
        assert!(footer_allowed(false, false, true));
        assert!(!footer_allowed(true, false, true), "búsqueda local");
        assert!(!footer_allowed(false, true, true), "quedan filas locales");
        assert!(
            !footer_allowed(false, false, false),
            "carpeta cargando o fallida"
        );
    }
}

//! Estado de sincronización por carpeta, tal como lo informa el runner (`MailboxSyncing` /
//! `MailboxSynced`), y su resumen para lo que muestra la lista. Es lógica pura, sin GTK.

use std::collections::HashMap;

use correo_core::MailboxId;

use crate::message_list::LoadState;
use crate::setup::MailboxSync;

#[derive(Default)]
pub struct LoadTracker {
    states: HashMap<MailboxId, MailboxSync>,
}

impl LoadTracker {
    pub fn set(&mut self, mailbox: MailboxId, state: MailboxSync) {
        self.states.insert(mailbox, state);
    }

    /// Marca la carpeta como «cargando» sin esperar el aviso del runner (se acaba de pedir).
    pub fn mark_pending(&mut self, mailbox: MailboxId) {
        self.states.insert(mailbox, MailboxSync::Syncing);
    }

    /// Resumen para varias carpetas (la bandeja unificada junta varias): cargando si alguna
    /// carga; si no, el primer fallo; si no, sincronizada o desconocida (= sin novedades).
    pub fn summary(&self, mailboxes: &[MailboxId]) -> LoadState {
        let mut failure = None;
        for id in mailboxes {
            match self.states.get(id) {
                Some(MailboxSync::Syncing) => return LoadState::Loading,
                Some(MailboxSync::Done(Some(reason))) if failure.is_none() => {
                    failure = Some(reason.clone());
                }
                _ => {}
            }
        }
        failure.map_or(LoadState::Idle, LoadState::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(reason: &str) -> MailboxSync {
        MailboxSync::Done(Some(reason.into()))
    }

    #[test]
    fn unknown_and_finished_folders_are_idle() {
        let mut t = LoadTracker::default();
        assert_eq!(t.summary(&[1, 2]), LoadState::Idle);
        t.set(1, MailboxSync::Done(None));
        assert_eq!(t.summary(&[1, 2]), LoadState::Idle);
    }

    #[test]
    fn any_syncing_folder_means_loading_even_if_another_failed() {
        let mut t = LoadTracker::default();
        t.set(1, failed("sin red"));
        t.set(2, MailboxSync::Syncing);
        assert_eq!(t.summary(&[1, 2]), LoadState::Loading);
        assert_eq!(t.summary(&[1]), LoadState::Failed("sin red".into()));
    }

    #[test]
    fn a_new_attempt_replaces_the_failure() {
        let mut t = LoadTracker::default();
        t.set(7, failed("tiempo agotado"));
        t.mark_pending(7);
        assert_eq!(t.summary(&[7]), LoadState::Loading);
        t.set(7, MailboxSync::Done(None));
        assert_eq!(t.summary(&[7]), LoadState::Idle);
    }

    #[test]
    fn only_the_listed_folders_count() {
        let mut t = LoadTracker::default();
        t.set(1, MailboxSync::Syncing);
        assert_eq!(t.summary(&[2]), LoadState::Idle);
    }
}

//! Refresco de contadores de no leídos fuera del camino crítico.
//!
//! `STATUS` de cada carpeta cuesta ~0,7 s en Gmail, así que no se hace en `list_mailboxes`.
//! Aquí se recorren las carpetas conocidas de una en una: la fuente (`UnseenSource`) toma
//! la sesión solo durante cada `STATUS` y la suelta entre carpetas, de modo que sincronizar,
//! marcar leído o abrir un mensaje se intercalan. El bucle trabaja sobre el trait para
//! poder probarse sin servidor.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use correo_core::{Error, Mailbox, MailboxRole, Result};

/// Presupuesto total del recorrido.
pub const BUDGET: Duration = Duration::from_secs(60);
/// Tope por cada `STATUS`.
pub const PER_STATUS: Duration = Duration::from_secs(10);

/// Origen de contadores `STATUS (UNSEEN)`.
#[async_trait]
pub trait UnseenSource: Send {
    /// No leídos de `path`, adquiriendo la sesión solo durante la consulta. `Ok(None)` si
    /// el servidor rechaza la orden (NO/BAD) o no informa; `Err` si la sesión no es
    /// fiable (red, conexión, credenciales).
    async fn unseen(&mut self, path: &str, limit: Duration) -> Result<Option<u32>>;

    /// Descarta la sesión tras un error de red.
    async fn discard(&mut self);
}

/// Resultado del recorrido.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Carpetas cuyo contador difiere del guardado, con el contador nuevo en `unread`.
    pub changed: Vec<Mailbox>,
    /// Error que detuvo el recorrido, si lo hubo.
    pub error: Option<Error>,
}

/// Orden de consulta: bandeja de entrada, luego las ya sincronizadas alguna vez (tienen
/// `uid_validity`) y por último el resto. Estable dentro de cada grupo.
pub fn priority_order(mut boxes: Vec<Mailbox>) -> Vec<Mailbox> {
    boxes.sort_by_key(|m| {
        if m.role == MailboxRole::Inbox {
            0
        } else if m.uid_validity.is_some() {
            1
        } else {
            2
        }
    });
    boxes
}

/// Recorre `boxes` (ya ordenadas) pidiendo sus no leídos. No escribe nada: el llamador
/// guarda `changed` de una vez, así que cancelar el futuro no deja cambios a medias.
pub async fn collect_unread<S: UnseenSource>(
    src: &mut S,
    boxes: Vec<Mailbox>,
    budget: Duration,
    per_status: Duration,
) -> Outcome {
    let started = Instant::now();
    let mut out = Outcome::default();
    for mut mailbox in boxes {
        let left = budget.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        match src.unseen(&mailbox.path, per_status.min(left)).await {
            Ok(Some(n)) if n != mailbox.unread => {
                mailbox.unread = n;
                out.changed.push(mailbox);
            }
            Ok(_) => {}
            Err(e) => {
                src.discard().await;
                out.error = Some(e);
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::*;

    fn mb(id: i64, path: &str, role: MailboxRole, validity: Option<u32>, unread: u32) -> Mailbox {
        Mailbox {
            id,
            account_id: 1,
            path: path.into(),
            role,
            uid_validity: validity,
            unread,
            delimiter: None,
        }
    }

    /// Fuente falsa: la «sesión» es un mutex asíncrono compartido con el test.
    struct Fake {
        session: Arc<Mutex<Vec<String>>>,
        answers: Vec<(&'static str, Result<Option<u32>>)>,
        discarded: bool,
    }

    #[async_trait]
    impl UnseenSource for Fake {
        async fn unseen(&mut self, path: &str, _limit: Duration) -> Result<Option<u32>> {
            let mut log = self.session.lock().await;
            log.push(format!("status {path}"));
            tokio::task::yield_now().await;
            match self.answers.iter().find(|(p, _)| *p == path) {
                Some((_, Ok(v))) => Ok(*v),
                Some((_, Err(e))) => Err(Error::Network(e.to_string())),
                None => Ok(None),
            }
        }
        async fn discard(&mut self) {
            self.discarded = true;
        }
    }

    fn fake(answers: Vec<(&'static str, Result<Option<u32>>)>) -> Fake {
        Fake {
            session: Arc::new(Mutex::new(Vec::new())),
            answers,
            discarded: false,
        }
    }

    #[test]
    fn inbox_first_then_synced_then_rest() {
        let ordered = priority_order(vec![
            mb(1, "a", MailboxRole::Other, None, 0),
            mb(2, "b", MailboxRole::Other, Some(5), 0),
            mb(3, "INBOX", MailboxRole::Inbox, None, 0),
            mb(4, "c", MailboxRole::Other, Some(7), 0),
        ]);
        let ids: Vec<i64> = ordered.iter().map(|m| m.id).collect();
        assert_eq!(ids, vec![3, 2, 4, 1]);
    }

    #[tokio::test]
    async fn reports_only_changed_counters_and_skips_rejected_folders() {
        let mut src = fake(vec![
            ("a", Ok(Some(3))),
            ("b", Ok(Some(2))),
            ("c", Ok(None)),
        ]);
        let boxes = vec![
            mb(1, "a", MailboxRole::Other, None, 1),
            mb(2, "b", MailboxRole::Other, None, 2),
            mb(3, "c", MailboxRole::Other, None, 9),
            mb(4, "d", MailboxRole::Other, None, 4),
        ];
        let out = collect_unread(&mut src, boxes, BUDGET, PER_STATUS).await;
        assert!(out.error.is_none());
        assert_eq!(out.changed.len(), 1);
        assert_eq!((out.changed[0].id, out.changed[0].unread), (1, 3));
        assert_eq!(src.session.lock().await.len(), 4, "se consultaron todas");
    }

    #[tokio::test]
    async fn network_error_stops_the_walk_and_discards_the_session() {
        let mut src = fake(vec![
            ("a", Ok(Some(5))),
            ("b", Err(Error::Network("caída".into()))),
        ]);
        let boxes = vec![
            mb(1, "a", MailboxRole::Other, None, 0),
            mb(2, "b", MailboxRole::Other, None, 0),
            mb(3, "c", MailboxRole::Other, None, 0),
        ];
        let out = collect_unread(&mut src, boxes, BUDGET, PER_STATUS).await;
        assert!(src.discarded);
        assert!(matches!(out.error, Some(Error::Network(_))));
        assert_eq!(out.changed.len(), 1, "lo ya leído se conserva");
        assert_eq!(src.session.lock().await.len(), 2, "c no se consultó");
    }

    #[tokio::test]
    async fn zero_budget_queries_nothing() {
        let mut src = fake(vec![]);
        let boxes = vec![mb(1, "a", MailboxRole::Other, None, 0)];
        let out = collect_unread(&mut src, boxes, Duration::ZERO, PER_STATUS).await;
        assert!(out.changed.is_empty());
        assert!(src.session.lock().await.is_empty());
    }

    #[tokio::test]
    async fn the_session_is_released_between_folders() {
        let mut src = fake(vec![]);
        let session = Arc::clone(&src.session);
        let boxes: Vec<Mailbox> = (0..6)
            .map(|i| mb(i, &format!("f{i}"), MailboxRole::Other, None, 0))
            .collect();
        // Operación concurrente (p. ej. marcar leído): pide la misma sesión mientras
        // el recorrido está en marcha y debe poder entrar antes de que termine.
        let op = {
            let session = Arc::clone(&session);
            async move {
                tokio::task::yield_now().await;
                let mut log = session.lock().await;
                log.push("operación concurrente".into());
            }
        };
        let (_, _) = tokio::join!(collect_unread(&mut src, boxes, BUDGET, PER_STATUS), op);
        let log = session.lock().await;
        let pos = log
            .iter()
            .position(|l| l == "operación concurrente")
            .expect("la operación se ejecutó");
        assert!(pos < log.len() - 1, "se intercaló antes del final: {log:?}");
        assert!(pos > 0, "no antes de la primera consulta: {log:?}");
    }
}

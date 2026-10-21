//! `SyncRunner`: ejecuta las acciones del planificador contra un `MailBackend`
//! y devuelve los resultados como eventos. Los mensajes nuevos de la bandeja
//! salen por un canal de `SyncReport` para las notificaciones.

use std::collections::{HashMap, VecDeque};
use std::future::pending;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use correo_core::{AccountId, Error, MailBackend, Mailbox, MailboxId, MessageId, Result};
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{Instant as TokioInstant, sleep_until};

use crate::clock::SystemClock;
use crate::scheduler::{Action, Event, IDLE_MAX, SyncScheduler};

/// Mensaje de error con el que un `IdleBackend` indica que el servidor no soporta IDLE.
pub const SIN_IDLE: &str = "sin IDLE";

/// Espera IDLE sobre la bandeja de entrada. Devuelve `Ok(())` cuando el servidor
/// anuncia cambios o expira `max`; `Err(Error::Protocol(SIN_IDLE))` si no hay IDLE.
#[async_trait]
pub trait IdleBackend: Send + Sync {
    async fn idle_inbox(&self, mailbox: &Mailbox, max: Duration) -> Result<()>;
}

/// Ids de los mensajes llegados a una carpeta desde la última consulta.
/// `MailBackend::sync_mailbox` solo devuelve un recuento; esto da los ids para notificar.
#[async_trait]
pub trait NewMessageIds: Send + Sync {
    async fn take_new(&self, mailbox: MailboxId) -> Result<Vec<MessageId>>;
}

/// Mensajes nuevos de la bandeja de entrada de una cuenta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub account: AccountId,
    pub new_messages: Vec<MessageId>,
}

pub struct SyncChannels {
    /// Eventos externos: `MailboxOpened`, `ConditionsChanged`, `Resumed`...
    pub events: mpsc::Sender<Event>,
    pub reports: mpsc::Receiver<SyncReport>,
    /// Termina cuando se cierra `events`.
    pub task: JoinHandle<()>,
}

pub struct SyncRunner {
    scheduler: SyncScheduler<SystemClock>,
    backend: Arc<dyn MailBackend>,
    idle: Option<Arc<dyn IdleBackend>>,
    news: Arc<dyn NewMessageIds>,
    sync_days: u32,
    mailboxes: HashMap<MailboxId, Mailbox>,
    idle_task: Option<JoinHandle<Result<()>>>,
    deadline: Option<Instant>,
}

impl SyncRunner {
    /// Sin `idle` el planificador usa solo polling.
    pub fn new(
        scheduler: SyncScheduler<SystemClock>,
        backend: Arc<dyn MailBackend>,
        idle: Option<Arc<dyn IdleBackend>>,
        news: Arc<dyn NewMessageIds>,
        sync_days: u32,
    ) -> Self {
        Self {
            scheduler,
            backend,
            idle,
            news,
            sync_days,
            mailboxes: HashMap::new(),
            idle_task: None,
            deadline: None,
        }
    }

    /// Arranca el bucle en una tarea de tokio.
    pub fn spawn(self) -> SyncChannels {
        let (events_tx, events_rx) = mpsc::channel(64);
        let (reports_tx, reports_rx) = mpsc::channel(64);
        let task = tokio::spawn(self.run(events_rx, reports_tx));
        SyncChannels {
            events: events_tx,
            reports: reports_rx,
            task,
        }
    }

    async fn run(mut self, mut events: mpsc::Receiver<Event>, reports: mpsc::Sender<SyncReport>) {
        let actions = self.scheduler.start();
        self.execute(actions, &reports).await;
        loop {
            let deadline = self.deadline;
            let mut idle_finished = false;
            let event = tokio::select! {
                biased;
                ev = events.recv() => match ev {
                    Some(ev) => ev,
                    None => break,
                },
                joined = join_idle(&mut self.idle_task) => {
                    idle_finished = true;
                    match joined {
                        Ok(Ok(())) => Event::IdleWake,
                        Ok(Err(Error::Protocol(m))) if m == SIN_IDLE => Event::IdleUnsupported,
                        _ => Event::SyncFailed,
                    }
                }
                _ = wait_until(deadline) => Event::Tick,
            };
            if idle_finished {
                self.idle_task = None;
            }
            let actions = self.scheduler.handle(event);
            self.execute(actions, &reports).await;
        }
        self.abort_idle();
    }

    /// Ejecuta acciones; los resultados que producen se reinyectan al planificador.
    async fn execute(&mut self, actions: Vec<Action>, reports: &mpsc::Sender<SyncReport>) {
        let mut queue = VecDeque::from(actions);
        while let Some(action) = queue.pop_front() {
            let follow_up = match action {
                Action::SyncInbox(_) => Some(self.sync_inbox(reports).await),
                Action::SyncMailbox(_, id) => {
                    // Un fallo en carpetas secundarias no altera el backoff de la bandeja:
                    // se reintenta en el siguiente barrido.
                    let _ = self.sync_folder(id).await;
                    None
                }
                Action::StartIdle(_) => self.start_idle().await,
                Action::Sleep(at) => {
                    self.deadline = Some(at);
                    None
                }
                Action::Pause => {
                    self.deadline = None;
                    self.abort_idle();
                    None
                }
            };
            if let Some(event) = follow_up {
                queue.extend(self.scheduler.handle(event));
            }
        }
    }

    async fn sync_inbox(&mut self, reports: &mpsc::Sender<SyncReport>) -> Event {
        let inbox = self.scheduler.inbox();
        let account = self.scheduler.account();
        let Ok(mailbox) = self.mailbox(inbox).await else {
            return Event::SyncFailed;
        };
        let Ok(changed) = self.backend.sync_mailbox(&mailbox, self.sync_days).await else {
            return Event::SyncFailed;
        };
        if changed > 0
            && let Ok(ids) = self.news.take_new(inbox).await
            && !ids.is_empty()
        {
            let _ = reports
                .send(SyncReport {
                    account,
                    new_messages: ids,
                })
                .await;
        }
        Event::SyncOk {
            new_messages: changed,
        }
    }

    async fn sync_folder(&mut self, id: MailboxId) -> Result<usize> {
        let mailbox = self.mailbox(id).await?;
        self.backend.sync_mailbox(&mailbox, self.sync_days).await
    }

    /// Relanza la espera IDLE sobre la bandeja en una tarea propia.
    async fn start_idle(&mut self) -> Option<Event> {
        self.abort_idle();
        let Some(idle) = self.idle.clone() else {
            return Some(Event::IdleUnsupported);
        };
        let inbox = self.scheduler.inbox();
        let Ok(mailbox) = self.mailbox(inbox).await else {
            return Some(Event::SyncFailed);
        };
        self.idle_task = Some(tokio::spawn(async move {
            idle.idle_inbox(&mailbox, IDLE_MAX).await
        }));
        None
    }

    fn abort_idle(&mut self) {
        if let Some(task) = self.idle_task.take() {
            task.abort();
        }
    }

    /// Carpeta por id; si no está en caché, recarga la lista de carpetas.
    async fn mailbox(&mut self, id: MailboxId) -> Result<Mailbox> {
        if let Some(mb) = self.mailboxes.get(&id) {
            return Ok(mb.clone());
        }
        for mb in self.backend.list_mailboxes().await? {
            self.mailboxes.insert(mb.id, mb);
        }
        self.mailboxes
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("carpeta {id}")))
    }
}

async fn join_idle(
    slot: &mut Option<JoinHandle<Result<()>>>,
) -> std::result::Result<Result<()>, JoinError> {
    match slot {
        Some(task) => task.await,
        None => pending().await,
    }
}

async fn wait_until(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(TokioInstant::from_std(at)).await,
        None => pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use correo_core::{MailboxRole, MessageBody};
    use tokio::sync::Notify;

    use super::*;

    const ACCOUNT: AccountId = 1;
    const INBOX: MailboxId = 10;
    const SECONDARY: MailboxId = 20;

    fn mailbox(id: MailboxId, role: MailboxRole) -> Mailbox {
        Mailbox {
            id,
            account_id: ACCOUNT,
            path: format!("box{id}"),
            role,
            uid_validity: None,
            unread: 0,
        }
    }

    /// Backend falso: cuenta sincronizaciones y devuelve un recuento configurable.
    struct FakeBackend {
        changed: Mutex<usize>,
        syncs: Mutex<Vec<MailboxId>>,
    }

    impl FakeBackend {
        fn new(changed: usize) -> Arc<Self> {
            Arc::new(Self {
                changed: Mutex::new(changed),
                syncs: Mutex::new(Vec::new()),
            })
        }

        fn sync_count(&self, id: MailboxId) -> usize {
            self.syncs
                .lock()
                .unwrap()
                .iter()
                .filter(|m| **m == id)
                .count()
        }
    }

    fn unused() -> Error {
        Error::Protocol("no usado en el test".into())
    }

    #[async_trait]
    impl MailBackend for FakeBackend {
        async fn list_mailboxes(&self) -> Result<Vec<Mailbox>> {
            Ok(vec![
                mailbox(INBOX, MailboxRole::Inbox),
                mailbox(SECONDARY, MailboxRole::Other),
            ])
        }
        async fn sync_mailbox(&self, mailbox: &Mailbox, _days: u32) -> Result<usize> {
            self.syncs.lock().unwrap().push(mailbox.id);
            Ok(*self.changed.lock().unwrap())
        }
        async fn fetch_body(&self, _: &Mailbox, _: u32) -> Result<MessageBody> {
            Err(unused())
        }
        async fn fetch_attachment(&self, _: &Mailbox, _: u32, _: usize) -> Result<Vec<u8>> {
            Err(unused())
        }
        async fn set_seen(&self, _: &Mailbox, _: u32, _: bool) -> Result<()> {
            Err(unused())
        }
        async fn move_to(&self, _: &Mailbox, _: u32, _: &Mailbox) -> Result<()> {
            Err(unused())
        }
        async fn delete(&self, _: &Mailbox, _: u32) -> Result<()> {
            Err(unused())
        }
    }

    /// IDLE falso: sin IDLE, o bloqueado hasta que el test lo libera con `release`.
    struct FakeIdle {
        supported: bool,
        calls: AtomicUsize,
        release: Notify,
    }

    impl FakeIdle {
        fn new(supported: bool) -> Arc<Self> {
            Arc::new(Self {
                supported,
                calls: AtomicUsize::new(0),
                release: Notify::new(),
            })
        }
    }

    #[async_trait]
    impl IdleBackend for FakeIdle {
        async fn idle_inbox(&self, _mailbox: &Mailbox, _max: Duration) -> Result<()> {
            if !self.supported {
                return Err(Error::Protocol(SIN_IDLE.into()));
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.release.notified().await;
            Ok(())
        }
    }

    struct FakeNews(Vec<MessageId>);

    #[async_trait]
    impl NewMessageIds for FakeNews {
        async fn take_new(&self, _mailbox: MailboxId) -> Result<Vec<MessageId>> {
            Ok(self.0.clone())
        }
    }

    fn runner(
        backend: Arc<FakeBackend>,
        idle: Option<Arc<FakeIdle>>,
        news: Vec<MessageId>,
    ) -> SyncRunner {
        let scheduler = SyncScheduler::new(SystemClock, ACCOUNT, INBOX, 1);
        SyncRunner::new(
            scheduler,
            backend,
            idle.map(|i| i as Arc<dyn IdleBackend>),
            Arc::new(FakeNews(news)),
            30,
        )
    }

    async fn eventually(mut check: impl FnMut() -> bool) {
        for _ in 0..500 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("la condición no se cumplió a tiempo");
    }

    #[tokio::test]
    async fn new_inbox_messages_are_reported_once_per_sync() {
        let backend = FakeBackend::new(2);
        let idle = FakeIdle::new(true);
        let mut ch = runner(backend.clone(), Some(idle.clone()), vec![101, 102]).spawn();

        let report = tokio::time::timeout(Duration::from_secs(5), ch.reports.recv())
            .await
            .expect("llegó el informe")
            .expect("canal abierto");
        assert_eq!(
            report,
            SyncReport {
                account: ACCOUNT,
                new_messages: vec![101, 102]
            }
        );
        eventually(|| idle.calls.load(Ordering::SeqCst) == 1).await;
        drop(ch.events);
        ch.task.await.unwrap();
    }

    #[tokio::test]
    async fn no_report_when_the_sync_finds_nothing_new() {
        let backend = FakeBackend::new(0);
        let idle = FakeIdle::new(true);
        let mut ch = runner(backend.clone(), Some(idle), vec![101]).spawn();
        eventually(|| backend.sync_count(INBOX) == 1).await;
        let got = tokio::time::timeout(Duration::from_millis(200), ch.reports.recv()).await;
        assert!(got.is_err(), "no debía haber informe");
        drop(ch.events);
        ch.task.await.unwrap();
    }

    #[tokio::test]
    async fn idle_wake_resyncs_inbox_and_restarts_idle() {
        let backend = FakeBackend::new(0);
        let idle = FakeIdle::new(true);
        let ch = runner(backend.clone(), Some(idle.clone()), vec![]).spawn();
        eventually(|| idle.calls.load(Ordering::SeqCst) == 1).await;
        assert_eq!(backend.sync_count(INBOX), 1);

        idle.release.notify_one(); // el servidor anuncia cambios
        eventually(|| idle.calls.load(Ordering::SeqCst) == 2).await;
        assert_eq!(backend.sync_count(INBOX), 2);
        drop(ch.events);
        ch.task.await.unwrap();
    }

    #[tokio::test]
    async fn without_idle_support_it_polls_instead_of_looping() {
        let backend = FakeBackend::new(0);
        let idle = FakeIdle::new(false);
        let ch = runner(backend.clone(), Some(idle.clone()), vec![]).spawn();
        eventually(|| backend.sync_count(INBOX) == 1).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        // La siguiente comprobación está a 2 min: no hay bucle ni reintentos.
        assert_eq!(backend.sync_count(INBOX), 1);
        assert_eq!(idle.calls.load(Ordering::SeqCst), 0);
        drop(ch.events);
        ch.task.await.unwrap();
    }

    #[tokio::test]
    async fn opening_a_secondary_folder_syncs_it() {
        let backend = FakeBackend::new(0);
        let ch = runner(backend.clone(), None, vec![]).spawn();
        ch.events
            .send(Event::MailboxOpened(SECONDARY))
            .await
            .unwrap();
        eventually(|| backend.sync_count(SECONDARY) == 1).await;
        drop(ch.events);
        ch.task.await.unwrap();
    }

    #[tokio::test]
    async fn going_offline_cancels_idle_and_stops_folder_syncs() {
        let backend = FakeBackend::new(0);
        let idle = FakeIdle::new(true);
        let ch = runner(backend.clone(), Some(idle.clone()), vec![]).spawn();
        eventually(|| idle.calls.load(Ordering::SeqCst) == 1).await;

        ch.events
            .send(Event::ConditionsChanged(crate::Conditions {
                network: crate::scheduler::Network::Offline,
                ..Default::default()
            }))
            .await
            .unwrap();
        ch.events
            .send(Event::MailboxOpened(SECONDARY))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(backend.sync_count(SECONDARY), 0);
        assert_eq!(idle.calls.load(Ordering::SeqCst), 1);
        drop(ch.events);
        ch.task.await.unwrap();
    }
}

//! `SyncRunner`: ejecuta las acciones del planificador contra un `MailBackend`
//! y devuelve los resultados como eventos. Las sincronizaciones corren en tareas
//! cancelables: un `Pause` o un `Resumed` las interrumpe aunque el servidor no responda.
//! Las novedades y los avisos que necesita el usuario salen por un canal de `SyncNotice`.

use std::collections::{HashMap, VecDeque};
use std::future::pending;
use std::sync::{Arc, Mutex, MutexGuard};
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

/// Avisos que el runner entrega a la aplicación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncNotice {
    /// Mensajes nuevos en la bandeja de entrada.
    NewMessages(SyncReport),
    /// La cuenta necesita volver a autorizarse. No se reintenta hasta enviar `Event::Reauthorized`.
    AuthRequired { account: AccountId },
}

pub struct SyncChannels {
    /// Eventos externos: `MailboxOpened`, `ConditionsChanged`, `Resumed`, `Reauthorized`...
    pub events: mpsc::Sender<Event>,
    pub reports: mpsc::Receiver<SyncNotice>,
    /// Termina cuando se cierra `events`.
    pub task: JoinHandle<()>,
}

/// Estado compartido con las tareas de sincronización.
struct Shared {
    backend: Arc<dyn MailBackend>,
    news: Arc<dyn NewMessageIds>,
    account: AccountId,
    inbox: MailboxId,
    sync_days: u32,
    mailboxes: Mutex<HashMap<MailboxId, Mailbox>>,
}

impl Shared {
    fn cache(&self) -> MutexGuard<'_, HashMap<MailboxId, Mailbox>> {
        self.mailboxes.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Carpeta por id; si no está en caché, recarga la lista de carpetas.
    async fn mailbox(&self, id: MailboxId) -> Result<Mailbox> {
        if let Some(mb) = self.cache().get(&id) {
            return Ok(mb.clone());
        }
        let listed = self.backend.list_mailboxes().await?;
        let mut cache = self.cache();
        for mb in listed {
            cache.insert(mb.id, mb);
        }
        cache
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("carpeta {id}")))
    }

    async fn sync_inbox(&self, notices: &mpsc::Sender<SyncNotice>) -> Event {
        match self.sync_mailbox(self.inbox).await {
            Ok(changed) => {
                if changed > 0
                    && let Ok(ids) = self.news.take_new(self.inbox).await
                    && !ids.is_empty()
                {
                    let _ = notices
                        .send(SyncNotice::NewMessages(SyncReport {
                            account: self.account,
                            new_messages: ids,
                        }))
                        .await;
                }
                Event::SyncOk {
                    new_messages: changed,
                }
            }
            Err(Error::Auth(_)) => Event::AuthFailed,
            Err(_) => Event::SyncFailed,
        }
    }

    async fn sync_mailbox(&self, id: MailboxId) -> Result<usize> {
        let mailbox = self.mailbox(id).await?;
        self.backend.sync_mailbox(&mailbox, self.sync_days).await
    }

    /// Ejecuta una acción de sincronización. Devuelve el evento que produce, si lo hay.
    async fn run(&self, action: Action, notices: &mpsc::Sender<SyncNotice>) -> Option<Event> {
        match action {
            Action::SyncInbox(_) => Some(self.sync_inbox(notices).await),
            // Un fallo en carpetas secundarias no altera el backoff de la bandeja:
            // se reintenta en el siguiente barrido. Sí cuenta un rechazo de credenciales.
            Action::SyncMailbox(_, id) => match self.sync_mailbox(id).await {
                Err(Error::Auth(_)) => Some(Event::AuthFailed),
                _ => None,
            },
            _ => None,
        }
    }
}

pub struct SyncRunner {
    scheduler: SyncScheduler<SystemClock>,
    shared: Arc<Shared>,
    idle: Option<Arc<dyn IdleBackend>>,
    /// Sincronizaciones pendientes; se ejecutan de una en una.
    pending_sync: VecDeque<Action>,
    sync_task: Option<JoinHandle<Option<Event>>>,
    /// Resultado: cuánto duró la espera y si terminó bien.
    idle_task: Option<JoinHandle<(Duration, Result<()>)>>,
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
        let shared = Shared {
            backend,
            news,
            account: scheduler.account(),
            inbox: scheduler.inbox(),
            sync_days,
            mailboxes: Mutex::new(HashMap::new()),
        };
        Self {
            scheduler,
            shared: Arc::new(shared),
            idle,
            pending_sync: VecDeque::new(),
            sync_task: None,
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

    async fn run(mut self, mut events: mpsc::Receiver<Event>, notices: mpsc::Sender<SyncNotice>) {
        let actions = self.scheduler.start();
        self.execute(actions, &notices).await;
        loop {
            self.start_next_sync(&notices);
            let deadline = self.deadline;
            let mut sync_done = false;
            let mut idle_done = false;
            let event = tokio::select! {
                biased;
                ev = events.recv() => match ev {
                    Some(ev) => Some(ev),
                    None => break,
                },
                joined = join_sync(&mut self.sync_task) => {
                    sync_done = true;
                    // Una tarea que terminó en pánico cuenta como fallo de sincronización.
                    joined.unwrap_or(Some(Event::SyncFailed))
                }
                joined = join_idle(&mut self.idle_task) => {
                    idle_done = true;
                    Some(idle_event(joined))
                }
                _ = wait_until(deadline) => Some(Event::Tick),
            };
            if sync_done {
                self.sync_task = None;
            }
            if idle_done {
                self.idle_task = None;
            }
            let Some(event) = event else {
                continue;
            };
            if event == Event::Resumed {
                // Vuelta del equipo o de la red: lo que estaba en curso ya no sirve.
                self.cancel_sync();
            }
            let actions = self.scheduler.handle(event);
            self.execute(actions, &notices).await;
        }
        self.abort_idle();
        self.cancel_sync();
    }

    /// Ejecuta acciones; los resultados que producen se reinyectan al planificador.
    async fn execute(&mut self, actions: Vec<Action>, notices: &mpsc::Sender<SyncNotice>) {
        let mut queue = VecDeque::from(actions);
        while let Some(action) = queue.pop_front() {
            let follow_up = match action {
                Action::SyncInbox(_) | Action::SyncMailbox(..) => {
                    self.pending_sync.push_back(action);
                    None
                }
                Action::StartIdle(_) => self.start_idle(),
                Action::Sleep(at) => {
                    self.deadline = Some(at);
                    None
                }
                Action::Pause => {
                    self.deadline = None;
                    self.abort_idle();
                    self.cancel_sync();
                    None
                }
                Action::AuthRequired(account) => {
                    let _ = notices.send(SyncNotice::AuthRequired { account }).await;
                    None
                }
            };
            if let Some(event) = follow_up {
                queue.extend(self.scheduler.handle(event));
            }
        }
    }

    /// Lanza la siguiente sincronización pendiente si no hay otra en curso.
    fn start_next_sync(&mut self, notices: &mpsc::Sender<SyncNotice>) {
        if self.sync_task.is_some() {
            return;
        }
        let Some(action) = self.pending_sync.pop_front() else {
            return;
        };
        let shared = Arc::clone(&self.shared);
        let notices = notices.clone();
        self.sync_task = Some(tokio::spawn(
            async move { shared.run(action, &notices).await },
        ));
    }

    /// Cancela la sincronización en curso y descarta las pendientes.
    fn cancel_sync(&mut self) {
        self.pending_sync.clear();
        if let Some(task) = self.sync_task.take() {
            task.abort();
        }
    }

    /// Relanza la espera IDLE sobre la bandeja en una tarea propia.
    /// Devuelve un evento solo si no hay forma de intentarlo.
    fn start_idle(&mut self) -> Option<Event> {
        self.abort_idle();
        let Some(idle) = self.idle.clone() else {
            return Some(Event::IdleUnsupported);
        };
        let shared = Arc::clone(&self.shared);
        self.idle_task = Some(tokio::spawn(async move {
            let started = Instant::now();
            let result = match shared.mailbox(shared.inbox).await {
                Ok(mailbox) => idle.idle_inbox(&mailbox, IDLE_MAX).await,
                Err(e) => Err(e),
            };
            (started.elapsed(), result)
        }));
        None
    }

    fn abort_idle(&mut self) {
        if let Some(task) = self.idle_task.take() {
            task.abort();
        }
    }
}

/// Traduce el final de una espera IDLE a un evento del planificador.
fn idle_event(joined: std::result::Result<(Duration, Result<()>), JoinError>) -> Event {
    match joined {
        Ok((lasted, Ok(()))) => Event::IdleWake { lasted },
        Ok((_, Err(Error::Protocol(m)))) if m == SIN_IDLE => Event::IdleUnsupported,
        Ok((_, Err(Error::Auth(_)))) => Event::AuthFailed,
        _ => Event::IdleFailed,
    }
}

async fn join_sync(
    slot: &mut Option<JoinHandle<Option<Event>>>,
) -> std::result::Result<Option<Event>, JoinError> {
    match slot {
        Some(task) => task.await,
        None => pending().await,
    }
}

async fn join_idle(
    slot: &mut Option<JoinHandle<(Duration, Result<()>)>>,
) -> std::result::Result<(Duration, Result<()>), JoinError> {
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

    /// Cuenta las sincronizaciones canceladas: al descartarse la future, el contador sube.
    struct CancelFlag(Arc<AtomicUsize>);

    impl Drop for CancelFlag {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Backend falso: cuenta sincronizaciones y devuelve un recuento configurable.
    /// `hang_next` hace que la siguiente sincronización se cuelgue; `auth_error` que falle con `Auth`.
    struct FakeBackend {
        changed: Mutex<usize>,
        syncs: Mutex<Vec<MailboxId>>,
        hang_next: AtomicBool,
        auth_error: AtomicBool,
        cancelled: Arc<AtomicUsize>,
    }

    impl FakeBackend {
        fn new(changed: usize) -> Arc<Self> {
            Arc::new(Self {
                changed: Mutex::new(changed),
                syncs: Mutex::new(Vec::new()),
                hang_next: AtomicBool::new(false),
                auth_error: AtomicBool::new(false),
                cancelled: Arc::new(AtomicUsize::new(0)),
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
            if self.hang_next.swap(false, Ordering::SeqCst) {
                let _flag = CancelFlag(Arc::clone(&self.cancelled));
                pending::<()>().await;
            }
            if self.auth_error.load(Ordering::SeqCst) {
                return Err(Error::Auth("token revocado".into()));
            }
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

    async fn eventually(check: impl FnMut() -> bool) {
        eventually_within(Duration::from_secs(5), check).await;
    }

    async fn eventually_within(limit: Duration, mut check: impl FnMut() -> bool) {
        let step = Duration::from_millis(10);
        let mut waited = Duration::ZERO;
        while waited < limit {
            if check() {
                return;
            }
            tokio::time::sleep(step).await;
            waited += step;
        }
        panic!("la condición no se cumplió a tiempo");
    }

    #[tokio::test]
    async fn new_inbox_messages_are_reported_once_per_sync() {
        let backend = FakeBackend::new(2);
        let idle = FakeIdle::new(true);
        let mut ch = runner(backend.clone(), Some(idle.clone()), vec![101, 102]).spawn();

        let notice = tokio::time::timeout(Duration::from_secs(5), ch.reports.recv())
            .await
            .expect("llegó el aviso")
            .expect("canal abierto");
        assert_eq!(
            notice,
            SyncNotice::NewMessages(SyncReport {
                account: ACCOUNT,
                new_messages: vec![101, 102]
            })
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

        // Un IDLE que vuelve al instante no cuenta como espera real: el reintento llega
        // tras el backoff de IDLE (5 s con jitter), no de inmediato.
        idle.release.notify_one();
        eventually_within(Duration::from_secs(10), || backend.sync_count(INBOX) == 2).await;
        eventually(|| idle.calls.load(Ordering::SeqCst) == 2).await;
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

    #[tokio::test]
    async fn pause_cancels_a_sync_that_hangs_on_the_server() {
        let backend = FakeBackend::new(0);
        backend.hang_next.store(true, Ordering::SeqCst);
        let ch = runner(backend.clone(), None, vec![]).spawn();
        eventually(|| backend.sync_count(INBOX) == 1).await;

        // Con la sincronización colgada, la red cae: el runner debe cancelarla y seguir atendiendo.
        ch.events
            .send(Event::ConditionsChanged(crate::Conditions {
                network: crate::scheduler::Network::Offline,
                ..Default::default()
            }))
            .await
            .unwrap();
        eventually(|| backend.cancelled.load(Ordering::SeqCst) == 1).await;
        drop(ch.events);
        tokio::time::timeout(Duration::from_secs(5), ch.task)
            .await
            .expect("el runner no terminó tras cerrar eventos")
            .unwrap();
    }

    #[tokio::test]
    async fn resumed_cancels_a_hung_sync_and_restarts_it() {
        let backend = FakeBackend::new(0);
        backend.hang_next.store(true, Ordering::SeqCst);
        let ch = runner(backend.clone(), None, vec![]).spawn();
        eventually(|| backend.sync_count(INBOX) == 1).await;

        ch.events.send(Event::Resumed).await.unwrap();
        eventually(|| backend.cancelled.load(Ordering::SeqCst) == 1).await;
        // La sincronización nueva ya no se cuelga y completa.
        eventually(|| backend.sync_count(INBOX) == 2).await;
        drop(ch.events);
        ch.task.await.unwrap();
    }

    #[tokio::test]
    async fn auth_error_notifies_once_and_stops_until_reauthorized() {
        let backend = FakeBackend::new(0);
        backend.auth_error.store(true, Ordering::SeqCst);
        let idle = FakeIdle::new(true);
        let mut ch = runner(backend.clone(), Some(idle.clone()), vec![]).spawn();

        let notice = tokio::time::timeout(Duration::from_secs(5), ch.reports.recv())
            .await
            .expect("llegó el aviso de autorización")
            .expect("canal abierto");
        assert_eq!(notice, SyncNotice::AuthRequired { account: ACCOUNT });

        // Ni reintentos de sincronización ni IDLE mientras no se reautorice.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let idle_before = idle.calls.load(Ordering::SeqCst);
        assert_eq!(backend.sync_count(INBOX), 1);

        backend.auth_error.store(false, Ordering::SeqCst);
        ch.events.send(Event::Reauthorized).await.unwrap();
        eventually(|| backend.sync_count(INBOX) == 2).await;
        eventually(|| idle.calls.load(Ordering::SeqCst) > idle_before).await;
        drop(ch.events);
        ch.task.await.unwrap();
    }
}

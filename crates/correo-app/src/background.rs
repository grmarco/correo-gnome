//! Sincronización en segundo plano: un `SyncRunner` por cuenta (IDLE sobre la bandeja de
//! entrada y polling de respaldo), conectado a la política de batería y red, a la
//! reanudación tras suspensión y al callback de mensajes nuevos en el hilo principal.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adw::prelude::*;
use async_trait::async_trait;
use correo_core::{
    Account, AccountId, Error, MailBackend, Mailbox, MailboxId, MailboxRole, MessageId, Result,
};
use correo_imap::ImapBackend;
use correo_store::Store;
use correo_sync::{
    backoff_delay, Event, IdleBackend, Jitter, NewMessageIds, SyncChannels, SyncReport, SyncRunner,
    SyncScheduler, SystemClock,
};
use gtk::glib;
use tokio::sync::mpsc;

use super::power::{watch_resume, PowerWatcher, ResumeWatch};
use crate::services::Services;

/// Cabeceras de la bandeja que se consultan para detectar mensajes nuevos.
const NEW_SCAN_LIMIT: usize = 10_000;
/// Reintentos al arrancar una cuenta sin conexión: backoff estándar de correo-sync.
const START_RETRY_LIMIT: u32 = 10_000;

type Senders = Rc<RefCell<HashMap<AccountId, mpsc::Sender<Event>>>>;

/// Control de la sincronización desde la interfaz. Mantenlo vivo mientras la app
/// necesite sincronizar: al soltarlo, los `Rc` de los callbacks se liberan.
pub struct SyncHandle {
    services: Rc<Services>,
    on_new: Rc<dyn Fn(Account, Vec<MessageId>)>,
    senders: Senders,
    power: Rc<PowerWatcher>,
    _resume: Option<ResumeWatch>,
}

impl SyncHandle {
    /// La ventana principal visible habilita el barrido de carpetas suscritas.
    pub fn set_window_visible(&self, visible: bool) {
        self.power.set_window_visible(visible);
    }

    /// Una carpeta se ha abierto: se sincroniza ya si no es la bandeja de entrada.
    pub fn mailbox_opened(&self, account: AccountId, mailbox: MailboxId) {
        self.send(account, Event::MailboxOpened(mailbox));
    }

    /// Empieza a sincronizar una cuenta (las cuentas existentes al arrancar y las nuevas
    /// tras `add_account_dialog`).
    pub fn start_account(&self, account: Account) {
        let backend = Arc::new(self.services.imap(&account));
        let store = Arc::clone(&self.services.store);
        let job = self.services.spawn({
            let account = account.clone();
            async move { start_runner(account, backend, store).await }
        });
        let senders = Rc::clone(&self.senders);
        let on_new = Rc::clone(&self.on_new);
        let power = Rc::clone(&self.power);
        glib::MainContext::default().spawn_local(async move {
            match job.await {
                Ok(channels) => attach(account, channels, &senders, &on_new, power.conditions()),
                Err(e) => eprintln!("correo: no se pudo iniciar la sincronización: {e}"),
            }
        });
    }

    fn send(&self, account: AccountId, event: Event) {
        if let Some(tx) = self.senders.borrow().get(&account) {
            if let Err(e) = tx.try_send(event) {
                eprintln!("correo: evento de sincronización descartado: {e}");
            }
        }
    }
}

/// Arranca la sincronización de todas las cuentas guardadas. `on_new` recibe los ids
/// de los mensajes nuevos y no leídos de la bandeja de entrada, en el hilo principal.
/// Con `run_in_background` la app se mantiene activa sin ventanas (`app.hold()`).
pub fn start_sync(
    services: Rc<Services>,
    app: &adw::Application,
    on_new: impl Fn(Account, Vec<MessageId>) + 'static,
) -> SyncHandle {
    if super::app_config().run_in_background {
        // Modo residente: el guard vive lo que la app. Se olvida a propósito para que
        // quien llama no tenga que guardarlo.
        std::mem::forget(app.hold());
    }

    let senders: Senders = Rc::default();
    let power_senders = Rc::clone(&senders);
    let power = PowerWatcher::new(move |conditions| {
        for tx in power_senders.borrow().values() {
            let _ = tx.try_send(Event::ConditionsChanged(conditions));
        }
    });
    let resume_senders = Rc::clone(&senders);
    let resume = watch_resume(move || {
        for tx in resume_senders.borrow().values() {
            let _ = tx.try_send(Event::Resumed);
        }
    });
    if resume.is_none() {
        eprintln!("correo: aviso: sin logind; no se resincronizará al reanudar");
    }

    let handle = SyncHandle {
        services: Rc::clone(&services),
        on_new: Rc::new(on_new),
        senders,
        power: Rc::new(power),
        _resume: resume,
    };
    match services.accounts() {
        Ok(accounts) => accounts
            .into_iter()
            .for_each(|account| handle.start_account(account)),
        Err(e) => eprintln!("correo: no se pudieron leer las cuentas: {e}"),
    }
    handle
}

/// Registra el canal del runner, le envía las condiciones actuales y reenvía sus
/// informes de mensajes nuevos al callback (en el hilo principal).
fn attach(
    account: Account,
    channels: SyncChannels,
    senders: &Senders,
    on_new: &Rc<dyn Fn(Account, Vec<MessageId>)>,
    conditions: correo_sync::Conditions,
) {
    let SyncChannels {
        events,
        mut reports,
        task: _task,
    } = channels;
    let _ = events.try_send(Event::ConditionsChanged(conditions));
    senders.borrow_mut().insert(account.id, events);

    let on_new = Rc::clone(on_new);
    glib::MainContext::default().spawn_local(async move {
        while let Some(SyncReport { new_messages, .. }) = reports.recv().await {
            on_new(account.clone(), new_messages);
        }
    });
}

/// Crea el runner de una cuenta y lo arranca. Debe ejecutarse dentro del runtime de tokio.
async fn start_runner(
    account: Account,
    backend: Arc<ImapBackend>,
    store: Arc<Mutex<Store>>,
) -> Result<SyncChannels> {
    let inbox = inbox_with_retry(&backend, &account).await?;
    let news = Arc::new(StoreNews::new(store, inbox.id)?);
    let idle: Arc<dyn IdleBackend> = Arc::new(ImapIdle(Arc::clone(&backend)));
    let scheduler = SyncScheduler::new(SystemClock, account.id, inbox.id, account.id as u64);
    let mail: Arc<dyn MailBackend> = backend;
    let runner = SyncRunner::new(scheduler, mail, Some(idle), news, account.sync_days);
    Ok(runner.spawn())
}

/// Localiza la bandeja de entrada (y registra las carpetas en la BD). Sin conexión
/// reintenta con backoff hasta conseguirlo.
async fn inbox_with_retry(backend: &ImapBackend, account: &Account) -> Result<Mailbox> {
    let mut jitter = Jitter::new(account.id as u64);
    for attempt in 0..START_RETRY_LIMIT {
        match backend.list_mailboxes().await {
            Ok(mailboxes) => {
                return mailboxes
                    .into_iter()
                    .find(|m| m.role == MailboxRole::Inbox)
                    .ok_or_else(|| {
                        Error::NotFound(format!("bandeja de entrada de {}", account.email))
                    });
            }
            Err(e) => {
                let delay = backoff_delay(attempt, &mut jitter);
                eprintln!(
                    "correo: {}: sin carpetas ({e}); reintento en {:?}",
                    account.email, delay
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
    Err(Error::Network(format!(
        "sin carpetas para {}",
        account.email
    )))
}

/// IDLE sobre la bandeja de entrada: `IdleEvent` se reduce a `Ok(())` (hubo aviso o
/// expiró el plazo; el planificador decide en ambos casos).
struct ImapIdle(Arc<ImapBackend>);

#[async_trait]
impl IdleBackend for ImapIdle {
    async fn idle_inbox(&self, mailbox: &Mailbox, max: Duration) -> Result<()> {
        self.0.idle_inbox(mailbox, max).await.map(|_| ())
    }
}

/// Ids de mensajes nuevos y no leídos de la bandeja. Se compara con el conjunto
/// de ids conocidos (sembrado desde la BD al arrancar).
///
/// Si la caché estaba vacía al arrancar (primera ejecución), el primer escaneo solo
/// establece la base: no notifica todo el histórico de golpe.
struct StoreNews {
    store: Arc<Mutex<Store>>,
    inbox: MailboxId,
    state: Mutex<NewsState>,
}

struct NewsState {
    known: HashSet<MessageId>,
    armed: bool,
}

impl StoreNews {
    fn new(store: Arc<Mutex<Store>>, inbox: MailboxId) -> Result<Self> {
        let known: HashSet<MessageId> = {
            let guard = store.lock().unwrap_or_else(|p| p.into_inner());
            guard
                .list_headers(inbox, NEW_SCAN_LIMIT, 0)?
                .into_iter()
                .map(|h| h.id)
                .collect()
        };
        let armed = !known.is_empty();
        Ok(Self {
            store,
            inbox,
            state: Mutex::new(NewsState { known, armed }),
        })
    }
}

#[async_trait]
impl NewMessageIds for StoreNews {
    async fn take_new(&self, mailbox: MailboxId) -> Result<Vec<MessageId>> {
        if mailbox != self.inbox {
            return Ok(Vec::new());
        }
        let headers = {
            let guard = self.store.lock().unwrap_or_else(|p| p.into_inner());
            guard.list_headers(mailbox, NEW_SCAN_LIMIT, 0)?
        };
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let notify = state.armed;
        state.armed = true;
        let mut fresh = Vec::new();
        for header in headers {
            let is_new = state.known.insert(header.id);
            if is_new && notify && !header.flags.seen {
                fresh.push(header.id);
            }
        }
        Ok(fresh)
    }
}

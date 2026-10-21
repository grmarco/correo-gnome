//! Planificador de sincronización: máquina de estados pura. Recibe eventos y
//! devuelve acciones; el tiempo sale únicamente de un `Clock` inyectado.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use correo_core::{AccountId, MailboxId};

use crate::clock::Clock;

/// Duración máxima de una espera IDLE antes de reiniciarla (los servidores cortan hacia 29 min).
pub const IDLE_MAX: Duration = Duration::from_secs(25 * 60);
/// Intervalos de polling cuando IDLE no está disponible: 2 → 4 → 8 → 15 min.
const POLL_STEPS: [Duration; 4] = [
    Duration::from_secs(2 * 60),
    Duration::from_secs(4 * 60),
    Duration::from_secs(8 * 60),
    Duration::from_secs(15 * 60),
];
/// Un IDLE que dura al menos esto cuenta como sano y reinicia el contador de fallos de IDLE.
const IDLE_HEALTHY: Duration = Duration::from_secs(60);
/// Fallos de IDLE seguidos antes de degradar a polling.
const IDLE_FAILURES_TO_DEGRADE: u32 = 3;
/// Tiempo en polling antes de volver a intentar IDLE.
const IDLE_RETRY: Duration = Duration::from_secs(60 * 60);
/// Barrido de carpetas secundarias.
const SWEEP: Duration = Duration::from_secs(30 * 60);
const SWEEP_LOW_POWER: Duration = Duration::from_secs(2 * 60 * 60);
/// Factor sobre los intervalos con ahorro de energía o batería baja.
const LOW_POWER_FACTOR: u32 = 3;
/// Backoff de reconexión: 5 s doblando hasta 10 min.
const BACKOFF_BASE_SECS: u64 = 5;
const BACKOFF_MAX: Duration = Duration::from_secs(10 * 60);
/// Jitter en milésimas: factor en [800, 1200] (±20 %).
const JITTER_MIN_PERMILLE: u64 = 800;
const JITTER_SPAN_PERMILLE: u64 = 401;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Network {
    #[default]
    Online,
    Offline,
}

/// Estado del entorno que condiciona la sincronización.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Conditions {
    pub network: Network,
    /// Red de pago: no se descargan adjuntos ni cuerpos automáticamente.
    pub metered: bool,
    pub power_saver: bool,
    pub low_battery: bool,
    /// La ventana principal está visible (habilita el barrido de carpetas suscritas).
    pub window_visible: bool,
}

impl Conditions {
    /// Ahorro de energía o batería baja: intervalos ×3 y sin prefetch.
    pub fn low_power(&self) -> bool {
        self.power_saver || self.low_battery
    }
}

/// Qué trabajo de fondo está permitido ahora.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub allow_prefetch_bodies: bool,
    pub allow_auto_attachments: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// IDLE terminó sin error (cambios del servidor o expiró `IDLE_MAX`).
    /// `lasted` es cuánto duró la espera; una espera corta se trata como fallo.
    IdleWake {
        lasted: Duration,
    },
    /// La espera IDLE falló (error de red, sesión, carpeta...). No es un fallo de sincronización.
    IdleFailed,
    /// El backend rechazó las credenciales: la cuenta no se reintenta hasta `Reauthorized`.
    AuthFailed,
    /// La cuenta se ha vuelto a autorizar.
    Reauthorized,
    /// Venció un deadline devuelto en `Action::Sleep`.
    Tick,
    MailboxOpened(MailboxId),
    /// El equipo vuelve de una suspensión.
    Resumed,
    ConditionsChanged(Conditions),
    /// Falló una sincronización o la espera IDLE (red, sesión...).
    SyncFailed,
    /// Sincronización de la bandeja completada; `new_messages` cuenta cambios (nuevos o modificados).
    SyncOk {
        new_messages: usize,
    },
    /// El servidor no anuncia IDLE.
    IdleUnsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SyncInbox(AccountId),
    SyncMailbox(AccountId, MailboxId),
    StartIdle(AccountId),
    /// Despertar no antes de este instante (el último `Sleep` recibido manda).
    Sleep(Instant),
    /// Sin red: cancelar IDLE y no programar nada hasta volver a estar online.
    Pause,
    /// Hace falta volver a autorizar la cuenta; la aplicación debe avisar al usuario.
    AuthRequired(AccountId),
}

/// Jitter determinista (splitmix64) con semilla inyectable.
#[derive(Debug, Clone)]
pub struct Jitter {
    state: u64,
}

impl Jitter {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Siguiente factor en milésimas, en [800, 1200].
    fn next_permille(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        JITTER_MIN_PERMILLE + z % JITTER_SPAN_PERMILLE
    }
}

/// Retraso antes del reintento número `attempt` (0 = primer fallo): 5 s · 2^attempt,
/// con jitter, acotado a 10 min.
pub fn backoff_delay(attempt: u32, jitter: &mut Jitter) -> Duration {
    let base_secs = (BACKOFF_BASE_SECS << attempt.min(7)).min(BACKOFF_MAX.as_secs());
    let millis = base_secs * 1000 * jitter.next_permille() / 1000;
    Duration::from_millis(millis).min(BACKOFF_MAX)
}

/// Planificador de una cuenta. Ver `handle` para las reglas.
pub struct SyncScheduler<C: Clock> {
    clock: C,
    account: AccountId,
    inbox: MailboxId,
    cond: Conditions,
    jitter: Jitter,
    idle_unsupported: bool,
    /// Hay una espera IDLE activa (o pendiente de lanzarse) en el runner.
    idle_running: bool,
    /// Fallos de IDLE seguidos. Solo se reinicia con un IDLE sano, nunca con `SyncOk`.
    idle_failures: u32,
    /// Cuándo reintentar IDLE tras haberlo degradado a polling por fallos.
    idle_retry_at: Option<Instant>,
    paused: bool,
    /// Estado terminal: credenciales inválidas hasta `Reauthorized`.
    auth_required: bool,
    /// Índice en `POLL_STEPS` del intervalo de polling siguiente.
    poll_step: usize,
    /// Fallos de sincronización seguidos (backoff de reintentos).
    failures: u32,
    next_inbox_at: Option<Instant>,
    next_sweep_at: Option<Instant>,
    /// Carpetas secundarias abiertas alguna vez.
    subscribed: BTreeSet<MailboxId>,
    /// Última carpeta secundaria abierta.
    open: Option<MailboxId>,
}

impl<C: Clock> SyncScheduler<C> {
    pub fn new(clock: C, account: AccountId, inbox: MailboxId, jitter_seed: u64) -> Self {
        Self {
            clock,
            account,
            inbox,
            cond: Conditions::default(),
            jitter: Jitter::new(jitter_seed),
            idle_unsupported: false,
            idle_running: false,
            idle_failures: 0,
            idle_retry_at: None,
            paused: false,
            auth_required: false,
            poll_step: 0,
            failures: 0,
            next_inbox_at: None,
            next_sweep_at: None,
            subscribed: BTreeSet::new(),
            open: None,
        }
    }

    pub fn account(&self) -> AccountId {
        self.account
    }

    pub fn inbox(&self) -> MailboxId {
        self.inbox
    }

    pub fn conditions(&self) -> Conditions {
        self.cond
    }

    /// Permisos de descarga según las condiciones actuales.
    pub fn policy(&self) -> Policy {
        Policy {
            allow_prefetch_bodies: !self.cond.metered && !self.cond.low_power(),
            allow_auto_attachments: !self.cond.metered,
        }
    }

    /// Arranque: IDLE + sincronización de la bandeja (o Pause si no hay red).
    pub fn start(&mut self) -> Vec<Action> {
        let now = self.clock.now();
        let mut out = Vec::new();
        if self.online() {
            self.resume(now, &mut out);
        } else {
            self.go_offline(&mut out);
        }
        self.finish(out)
    }

    /// Procesa un evento y devuelve las acciones a ejecutar.
    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        let now = self.clock.now();
        let mut out = Vec::new();
        if self.auth_required {
            // Estado terminal: solo la reautorización reanuda el trabajo.
            match event {
                Event::Reauthorized => {
                    self.auth_required = false;
                    if self.online() {
                        self.resume(now, &mut out);
                    } else {
                        self.go_offline(&mut out);
                    }
                }
                Event::ConditionsChanged(cond) => self.cond = cond,
                _ => {}
            }
            return self.finish(out);
        }
        match event {
            Event::ConditionsChanged(cond) => {
                self.cond = cond;
                if !self.online() {
                    self.go_offline(&mut out);
                } else if self.paused {
                    self.paused = false;
                    self.resume(now, &mut out);
                }
            }
            Event::Resumed => {
                if !self.paused {
                    self.resume(now, &mut out);
                }
            }
            Event::IdleWake { lasted } => {
                if !self.paused {
                    self.idle_running = false;
                    if lasted >= IDLE_HEALTHY {
                        self.idle_failures = 0;
                        self.inbox_wake(&mut out);
                    } else {
                        // Un IDLE que termina al instante no es una espera real: cuenta como fallo
                        // para no entrar en un bucle sin pausas.
                        self.idle_failed(now);
                    }
                }
            }
            Event::IdleFailed => {
                if !self.paused {
                    self.idle_failed(now);
                }
            }
            Event::AuthFailed => {
                if !self.paused {
                    self.auth_required = true;
                    self.idle_running = false;
                    self.next_inbox_at = None;
                    self.next_sweep_at = None;
                    self.idle_retry_at = None;
                    out.push(Action::AuthRequired(self.account));
                    out.push(Action::Pause);
                }
            }
            Event::Reauthorized => {}
            Event::MailboxOpened(id) => {
                if !self.paused {
                    if id == self.inbox {
                        self.inbox_wake(&mut out);
                    } else {
                        self.subscribed.insert(id);
                        self.open = Some(id);
                        out.push(Action::SyncMailbox(self.account, id));
                    }
                }
            }
            Event::Tick => {
                if !self.paused {
                    if self.idle_retry_at.is_some_and(|at| at <= now) {
                        // Reintento de IDLE tras la degradación: se da una nueva oportunidad.
                        self.idle_retry_at = None;
                        self.idle_unsupported = false;
                        self.idle_failures = 0;
                        self.ensure_idle(&mut out);
                    }
                    if self.next_inbox_at.is_some_and(|at| at <= now) {
                        self.inbox_wake(&mut out);
                    }
                    if self.next_sweep_at.is_some_and(|at| at <= now) {
                        self.sweep(now, &mut out);
                    }
                }
            }
            Event::SyncFailed => {
                if !self.paused {
                    let delay = backoff_delay(self.failures, &mut self.jitter);
                    self.failures = self.failures.saturating_add(1);
                    // La sesión IDLE probablemente murió con la conexión: se relanza tras el reintento.
                    self.idle_running = false;
                    self.next_inbox_at = Some(now + delay);
                }
            }
            Event::SyncOk { new_messages } => {
                if !self.paused {
                    // Solo reinicia el backoff de sincronización; los fallos de IDLE son independientes.
                    self.failures = 0;
                    if self.idle_unsupported {
                        if new_messages > 0 {
                            self.poll_step = 0;
                        } else {
                            self.poll_step = (self.poll_step + 1).min(POLL_STEPS.len() - 1);
                        }
                        self.next_inbox_at = Some(now + self.poll_interval());
                    }
                    self.ensure_idle(&mut out);
                }
            }
            Event::IdleUnsupported => {
                if !self.paused {
                    self.idle_unsupported = true;
                    self.idle_running = false;
                    if self.next_inbox_at.is_none() {
                        self.next_inbox_at = Some(now + self.poll_interval());
                    }
                }
            }
        }
        self.finish(out)
    }

    fn online(&self) -> bool {
        self.cond.network == Network::Online
    }

    fn poll_interval(&self) -> Duration {
        let base = POLL_STEPS[self.poll_step.min(POLL_STEPS.len() - 1)];
        if self.cond.low_power() {
            base * LOW_POWER_FACTOR
        } else {
            base
        }
    }

    fn sweep_interval(&self) -> Duration {
        if self.cond.low_power() {
            SWEEP_LOW_POWER
        } else {
            SWEEP
        }
    }

    fn go_offline(&mut self, out: &mut Vec<Action>) {
        if self.paused {
            return;
        }
        self.paused = true;
        self.idle_running = false;
        self.next_inbox_at = None;
        self.next_sweep_at = None;
        out.push(Action::Pause);
    }

    /// Fallo de IDLE: reintento con backoff y, tras varios seguidos, degradación a polling.
    fn idle_failed(&mut self, now: Instant) {
        self.idle_running = false;
        self.idle_failures = self.idle_failures.saturating_add(1);
        if self.idle_failures >= IDLE_FAILURES_TO_DEGRADE {
            self.idle_unsupported = true;
            self.idle_retry_at = Some(now + IDLE_RETRY);
            self.poll_step = 0;
            self.next_inbox_at = Some(now + self.poll_interval());
        } else {
            let delay = backoff_delay(self.idle_failures - 1, &mut self.jitter);
            self.next_inbox_at = Some(now + delay);
        }
    }

    /// Vuelta a la red o al foreground: sincronización inmediata, sin esperar backoff.
    fn resume(&mut self, now: Instant, out: &mut Vec<Action>) {
        self.failures = 0;
        // Cualquier IDLE anterior murió con la sesión; el runner descarta la tarea antigua.
        self.idle_running = false;
        if self.next_sweep_at.is_none() {
            self.next_sweep_at = Some(now + self.sweep_interval());
        }
        self.inbox_wake(out);
    }

    fn ensure_idle(&mut self, out: &mut Vec<Action>) {
        if self.online() && !self.idle_unsupported && !self.idle_running {
            self.idle_running = true;
            out.push(Action::StartIdle(self.account));
        }
    }

    /// Sincroniza la bandeja y relanza IDLE si hace falta.
    fn inbox_wake(&mut self, out: &mut Vec<Action>) {
        self.ensure_idle(out);
        self.next_inbox_at = None;
        out.push(Action::SyncInbox(self.account));
    }

    /// Barrido de carpetas secundarias: todas las suscritas con la ventana visible;
    /// sin ventana, solo la carpeta abierta.
    fn sweep(&mut self, now: Instant, out: &mut Vec<Action>) {
        let targets: Vec<MailboxId> = if self.cond.window_visible {
            self.subscribed.iter().copied().collect()
        } else {
            self.open.into_iter().collect()
        };
        for id in targets {
            out.push(Action::SyncMailbox(self.account, id));
        }
        self.next_sweep_at = Some(now + self.sweep_interval());
    }

    /// Añade el `Sleep` con el próximo deadline (si no hay pausa).
    fn finish(&self, mut out: Vec<Action>) -> Vec<Action> {
        if self.paused || self.auth_required {
            return out;
        }
        let next = [self.next_inbox_at, self.next_sweep_at, self.idle_retry_at]
            .into_iter()
            .flatten()
            .min();
        if let Some(at) = next {
            out.push(Action::Sleep(at));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::clock::FakeClock;

    const ACCOUNT: AccountId = 1;
    const INBOX: MailboxId = 10;
    const SECONDARY: MailboxId = 20;
    const OTHER: MailboxId = 21;
    const SEED: u64 = 42;
    const MIN: Duration = Duration::from_secs(60);

    fn setup() -> (FakeClock, SyncScheduler<FakeClock>) {
        let clock = FakeClock::new();
        let sched = SyncScheduler::new(clock.clone(), ACCOUNT, INBOX, SEED);
        (clock, sched)
    }

    fn sleep_of(actions: &[Action]) -> Option<Instant> {
        actions.iter().find_map(|a| match a {
            Action::Sleep(at) => Some(*at),
            _ => None,
        })
    }

    fn without_sleep(actions: Vec<Action>) -> Vec<Action> {
        actions
            .into_iter()
            .filter(|a| !matches!(a, Action::Sleep(_)))
            .collect()
    }

    fn online_with(f: impl FnOnce(&mut Conditions)) -> Conditions {
        let mut c = Conditions::default();
        f(&mut c);
        c
    }

    fn offline() -> Conditions {
        Conditions {
            network: Network::Offline,
            ..Conditions::default()
        }
    }

    /// Avanza el reloj en pasos de 100 ms hasta que llega `SyncInbox` tras un `Tick`,
    /// y devuelve cuánto se esperó.
    fn wait_for_inbox_retry(clock: &FakeClock, sched: &mut SyncScheduler<FakeClock>) -> Duration {
        let step = Duration::from_millis(100);
        let mut waited = Duration::ZERO;
        loop {
            clock.advance(step);
            waited += step;
            if sched
                .handle(Event::Tick)
                .contains(&Action::SyncInbox(ACCOUNT))
            {
                return waited;
            }
            assert!(
                waited < Duration::from_secs(601),
                "el reintento nunca llegó"
            );
        }
    }

    /// Retrasos hasta cada reintento tras `n` fallos consecutivos.
    fn failure_delays(seed: u64, n: usize) -> Vec<Duration> {
        let clock = FakeClock::new();
        let mut sched = SyncScheduler::new(clock.clone(), ACCOUNT, INBOX, seed);
        sched.start();
        (0..n)
            .map(|_| {
                sched.handle(Event::SyncFailed);
                wait_for_inbox_retry(&clock, &mut sched)
            })
            .collect()
    }

    #[test]
    fn start_online_runs_idle_and_syncs_inbox_then_sleeps_until_sweep() {
        let (clock, mut s) = setup();
        let t0 = clock.now();
        assert_eq!(
            s.start(),
            vec![
                Action::StartIdle(ACCOUNT),
                Action::SyncInbox(ACCOUNT),
                Action::Sleep(t0 + 30 * MIN),
            ]
        );
    }

    #[test]
    fn start_offline_pauses_and_never_retries() {
        let (clock, mut s) = setup();
        assert_eq!(
            s.handle(Event::ConditionsChanged(offline())),
            vec![Action::Pause]
        );
        assert!(s.start().is_empty());
        for _ in 0..10 {
            clock.advance(Duration::from_secs(3600));
            assert!(s.handle(Event::Tick).is_empty());
        }
    }

    #[test]
    fn going_offline_pauses_once_and_ignores_all_events_until_online() {
        let (_clock, mut s) = setup();
        s.start();
        assert_eq!(
            s.handle(Event::ConditionsChanged(offline())),
            vec![Action::Pause]
        );
        // Cambios de condiciones sin volver a la red: sin Pause duplicado.
        let metered_offline = Conditions {
            metered: true,
            ..offline()
        };
        assert!(
            s.handle(Event::ConditionsChanged(metered_offline))
                .is_empty()
        );
        assert!(s.handle(Event::IdleWake { lasted: MIN }).is_empty());
        assert!(s.handle(Event::Tick).is_empty());
        assert!(s.handle(Event::SyncFailed).is_empty());
        assert!(s.handle(Event::SyncOk { new_messages: 4 }).is_empty());
        assert!(s.handle(Event::MailboxOpened(SECONDARY)).is_empty());
        assert!(s.handle(Event::MailboxOpened(INBOX)).is_empty());
        assert!(s.handle(Event::Resumed).is_empty());
        assert!(s.handle(Event::IdleUnsupported).is_empty());
    }

    #[test]
    fn back_online_syncs_immediately_without_waiting_for_backoff() {
        let (clock, mut s) = setup();
        s.start();
        s.handle(Event::SyncFailed); // deja un reintento pendiente de unos segundos
        s.handle(Event::ConditionsChanged(offline()));
        let now = clock.now();
        let actions = s.handle(Event::ConditionsChanged(Conditions::default()));
        assert_eq!(
            actions,
            vec![
                Action::StartIdle(ACCOUNT),
                Action::SyncInbox(ACCOUNT),
                Action::Sleep(now + 30 * MIN),
            ]
        );
    }

    #[test]
    fn resumed_after_suspend_syncs_inbox_immediately_and_restarts_idle() {
        let (clock, mut s) = setup();
        s.start();
        clock.advance(Duration::from_secs(3 * 3600));
        let actions = s.handle(Event::Resumed);
        assert_eq!(
            without_sleep(actions),
            vec![Action::StartIdle(ACCOUNT), Action::SyncInbox(ACCOUNT)]
        );
    }

    #[test]
    fn resumed_while_offline_is_ignored() {
        let (_clock, mut s) = setup();
        s.start();
        s.handle(Event::ConditionsChanged(offline()));
        assert!(s.handle(Event::Resumed).is_empty());
    }

    #[test]
    fn idle_wake_syncs_inbox_and_restarts_idle() {
        let (clock, mut s) = setup();
        let t0 = clock.now();
        s.start();
        clock.advance(25 * MIN);
        assert_eq!(
            s.handle(Event::IdleWake { lasted: 25 * MIN }),
            vec![
                Action::StartIdle(ACCOUNT),
                Action::SyncInbox(ACCOUNT),
                Action::Sleep(t0 + 30 * MIN),
            ]
        );
    }

    #[test]
    fn sync_ok_with_idle_running_does_not_resync() {
        let (clock, mut s) = setup();
        let t0 = clock.now();
        s.start();
        assert_eq!(
            s.handle(Event::SyncOk { new_messages: 3 }),
            vec![Action::Sleep(t0 + 30 * MIN)]
        );
    }

    #[test]
    fn mailbox_opened_on_inbox_syncs_inbox_and_on_other_folder_syncs_it() {
        let (clock, mut s) = setup();
        let t0 = clock.now();
        s.start();
        assert_eq!(
            s.handle(Event::MailboxOpened(INBOX)),
            vec![Action::SyncInbox(ACCOUNT), Action::Sleep(t0 + 30 * MIN)]
        );
        assert_eq!(
            s.handle(Event::MailboxOpened(SECONDARY)),
            vec![
                Action::SyncMailbox(ACCOUNT, SECONDARY),
                Action::Sleep(t0 + 30 * MIN)
            ]
        );
    }

    #[test]
    fn idle_unsupported_switches_to_polling_starting_at_two_minutes() {
        let (clock, mut s) = setup();
        let t0 = clock.now();
        s.start();
        assert_eq!(
            s.handle(Event::IdleUnsupported),
            vec![Action::Sleep(t0 + 2 * MIN)]
        );
        clock.advance(MIN);
        assert_eq!(s.handle(Event::Tick), vec![Action::Sleep(t0 + 2 * MIN)]);
        clock.advance(MIN);
        // Sin StartIdle: el servidor no lo soporta.
        assert_eq!(
            s.handle(Event::Tick),
            vec![Action::SyncInbox(ACCOUNT), Action::Sleep(t0 + 30 * MIN)]
        );
    }

    #[test]
    fn polling_intervals_grow_2_4_8_15_and_stay_at_15_without_news() {
        let (clock, mut s) = setup();
        s.start();
        s.handle(Event::IdleUnsupported);
        // Tras cada sincronización sin novedades, el siguiente intervalo crece.
        // Se comprueba con tres saltos para no cruzar el barrido de 30 min.
        let mut gaps = Vec::new();
        for _ in 0..3 {
            let due = sleep_of(&s.handle(Event::Tick)).unwrap();
            clock.advance(due - clock.now());
            s.handle(Event::Tick);
            let now = clock.now();
            let next = sleep_of(&s.handle(Event::SyncOk { new_messages: 0 })).unwrap();
            gaps.push(next - now);
        }
        assert_eq!(gaps, vec![4 * MIN, 8 * MIN, 15 * MIN]);
        // Y se queda en 15 min sin novedades.
        for _ in 0..3 {
            s.handle(Event::SyncOk { new_messages: 0 });
        }
        assert_eq!(s.poll_interval(), 15 * MIN);
        assert_eq!(s.poll_step, POLL_STEPS.len() - 1);
    }

    #[test]
    fn new_mail_resets_polling_interval_to_two_minutes() {
        let (clock, mut s) = setup();
        s.start();
        s.handle(Event::IdleUnsupported);
        s.handle(Event::SyncOk { new_messages: 0 }); // 4 min
        s.handle(Event::SyncOk { new_messages: 0 }); // 8 min
        let now = clock.now();
        let next = sleep_of(&s.handle(Event::SyncOk { new_messages: 2 })).unwrap();
        assert_eq!(next - now, 2 * MIN);
    }

    #[test]
    fn power_saver_and_low_battery_triple_polling_intervals() {
        for cond in [
            online_with(|c| c.power_saver = true),
            online_with(|c| c.low_battery = true),
        ] {
            let (clock, mut s) = setup();
            s.handle(Event::ConditionsChanged(cond));
            s.start();
            let t0 = clock.now();
            assert_eq!(
                sleep_of(&s.handle(Event::IdleUnsupported)),
                Some(t0 + 6 * MIN)
            );
            let now = clock.now();
            let next = sleep_of(&s.handle(Event::SyncOk { new_messages: 0 })).unwrap();
            assert_eq!(next - now, 12 * MIN);
            let next = sleep_of(&s.handle(Event::SyncOk { new_messages: 0 })).unwrap();
            assert_eq!(next - now, 24 * MIN);
            let next = sleep_of(&s.handle(Event::SyncOk { new_messages: 0 })).unwrap();
            assert_eq!(next - now, 45 * MIN);
        }
    }

    #[test]
    fn failure_retries_with_backoff_and_restarts_idle_on_retry() {
        let (clock, mut s) = setup();
        s.start();
        s.handle(Event::SyncFailed);
        clock.advance(Duration::from_secs(10));
        let actions = s.handle(Event::Tick);
        let idle = actions
            .iter()
            .position(|a| *a == Action::StartIdle(ACCOUNT));
        let sync = actions
            .iter()
            .position(|a| *a == Action::SyncInbox(ACCOUNT));
        assert!(idle.is_some() && sync.is_some(), "{actions:?}");
        assert!(idle < sync, "IDLE antes que la sincronización");
    }

    #[test]
    fn tick_before_retry_deadline_does_nothing() {
        let (clock, mut s) = setup();
        s.start();
        s.handle(Event::SyncFailed);
        let deadline = sleep_of(&s.handle(Event::Tick)).unwrap();
        clock.advance(Duration::from_secs(3));
        let actions = s.handle(Event::Tick);
        assert!(!actions.contains(&Action::SyncInbox(ACCOUNT)));
        assert_eq!(sleep_of(&actions), Some(deadline));
    }

    #[test]
    fn backoff_starts_at_five_seconds_and_doubles_within_jitter_bounds() {
        let delays = failure_delays(SEED, 12);
        for (attempt, waited) in delays.iter().enumerate() {
            let base = ((5u64 << attempt.min(7)).min(600)) as f64;
            let lo = base * 0.8;
            let hi = (base * 1.2).min(600.0);
            let w = waited.as_secs_f64();
            assert!(
                w >= lo - 1e-6 && w <= hi + 0.1 + 1e-6,
                "intento {attempt}: {w} s fuera de [{lo}, {hi}]"
            );
        }
    }

    #[test]
    fn backoff_never_exceeds_ten_minutes() {
        let mut jitter = Jitter::new(7);
        for attempt in 0..200 {
            assert!(backoff_delay(attempt, &mut jitter) <= BACKOFF_MAX);
        }
        let late = failure_delays(SEED, 12);
        assert!(
            late.iter()
                .all(|d| *d <= BACKOFF_MAX + Duration::from_millis(100))
        );
    }

    #[test]
    fn jitter_is_deterministic_for_a_seed_and_differs_between_seeds() {
        assert_eq!(failure_delays(7, 8), failure_delays(7, 8));
        assert_ne!(failure_delays(7, 12), failure_delays(8, 12));
        let mut j = Jitter::new(1);
        for _ in 0..1000 {
            let p = j.next_permille();
            assert!((800..=1200).contains(&p));
        }
    }

    #[test]
    fn successful_sync_resets_backoff() {
        let (clock, mut s) = setup();
        s.start();
        for _ in 0..5 {
            s.handle(Event::SyncFailed);
            wait_for_inbox_retry(&clock, &mut s);
        }
        s.handle(Event::SyncOk { new_messages: 0 });
        s.handle(Event::SyncFailed);
        let waited = wait_for_inbox_retry(&clock, &mut s);
        assert!(waited <= Duration::from_millis(6100), "{waited:?}");
    }

    #[test]
    fn failure_while_offline_is_ignored() {
        let (_clock, mut s) = setup();
        s.handle(Event::ConditionsChanged(offline()));
        assert!(s.handle(Event::SyncFailed).is_empty());
    }

    #[test]
    fn opened_secondary_folder_syncs_immediately() {
        let (clock, mut s) = setup();
        let t0 = clock.now();
        s.start();
        assert_eq!(
            s.handle(Event::MailboxOpened(SECONDARY)),
            vec![
                Action::SyncMailbox(ACCOUNT, SECONDARY),
                Action::Sleep(t0 + 30 * MIN)
            ]
        );
    }

    #[test]
    fn sweep_every_30_min_syncs_all_subscribed_folders_when_window_visible() {
        let (clock, mut s) = setup();
        s.handle(Event::ConditionsChanged(online_with(|c| {
            c.window_visible = true
        })));
        s.start();
        s.handle(Event::MailboxOpened(SECONDARY));
        s.handle(Event::MailboxOpened(OTHER));
        clock.advance(30 * MIN);
        let t = clock.now();
        assert_eq!(
            s.handle(Event::Tick),
            vec![
                Action::SyncMailbox(ACCOUNT, SECONDARY),
                Action::SyncMailbox(ACCOUNT, OTHER),
                Action::Sleep(t + 30 * MIN),
            ]
        );
    }

    #[test]
    fn sweep_without_visible_window_only_syncs_the_open_folder() {
        let (clock, mut s) = setup();
        s.start();
        s.handle(Event::MailboxOpened(SECONDARY));
        s.handle(Event::MailboxOpened(OTHER)); // OTHER queda abierta
        clock.advance(30 * MIN);
        let actions = without_sleep(s.handle(Event::Tick));
        assert_eq!(actions, vec![Action::SyncMailbox(ACCOUNT, OTHER)]);
    }

    #[test]
    fn sweep_without_any_open_folder_does_no_network_work() {
        let (clock, mut s) = setup();
        s.start();
        clock.advance(30 * MIN);
        assert!(without_sleep(s.handle(Event::Tick)).is_empty());
    }

    #[test]
    fn sweep_never_includes_the_inbox() {
        let (clock, mut s) = setup();
        s.handle(Event::ConditionsChanged(online_with(|c| {
            c.window_visible = true
        })));
        s.start();
        s.handle(Event::MailboxOpened(INBOX));
        clock.advance(30 * MIN);
        let actions = without_sleep(s.handle(Event::Tick));
        assert!(
            actions
                .iter()
                .all(|a| !matches!(a, Action::SyncMailbox(_, INBOX)))
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn low_power_sweeps_every_two_hours_instead_of_thirty_minutes() {
        let (clock, mut s) = setup();
        s.handle(Event::ConditionsChanged(online_with(|c| {
            c.window_visible = true;
            c.power_saver = true;
        })));
        s.start();
        s.handle(Event::MailboxOpened(SECONDARY));
        clock.advance(30 * MIN);
        assert!(without_sleep(s.handle(Event::Tick)).is_empty());
        clock.advance(90 * MIN);
        assert_eq!(
            without_sleep(s.handle(Event::Tick)),
            vec![Action::SyncMailbox(ACCOUNT, SECONDARY)]
        );
    }

    #[test]
    fn policy_disables_prefetch_on_metered_or_low_power() {
        let (_clock, mut s) = setup();
        let policy = |s: &SyncScheduler<FakeClock>| {
            (
                s.policy().allow_prefetch_bodies,
                s.policy().allow_auto_attachments,
            )
        };
        assert_eq!(policy(&s), (true, true));

        s.handle(Event::ConditionsChanged(online_with(|c| c.metered = true)));
        assert_eq!(policy(&s), (false, false));

        s.handle(Event::ConditionsChanged(online_with(|c| {
            c.power_saver = true
        })));
        assert_eq!(policy(&s), (false, true));

        s.handle(Event::ConditionsChanged(online_with(|c| {
            c.low_battery = true
        })));
        assert_eq!(policy(&s), (false, true));

        s.handle(Event::ConditionsChanged(online_with(|c| {
            c.metered = true;
            c.low_battery = true;
        })));
        assert_eq!(policy(&s), (false, false));

        s.handle(Event::ConditionsChanged(Conditions::default()));
        assert_eq!(policy(&s), (true, true));
    }

    #[test]
    fn idle_stays_on_inbox_only_and_is_not_restarted_while_running() {
        let (clock, mut s) = setup();
        s.start();
        // Varios eventos de estado estable no relanzan IDLE ni sincronizan.
        for _ in 0..3 {
            clock.advance(MIN);
            let actions = without_sleep(s.handle(Event::Tick));
            assert!(actions.is_empty(), "{actions:?}");
            assert!(without_sleep(s.handle(Event::SyncOk { new_messages: 0 })).is_empty());
        }
    }

    /// Simula el runner con un IDLE que falla al instante y una sincronización que va bien.
    /// Avanza el reloj hasta el siguiente `Sleep` mientras no pase de `window`.
    /// Devuelve (arranques de IDLE, sincronizaciones de la bandeja).
    fn simulate_failing_idle(window: Duration) -> (usize, usize) {
        let (clock, mut s) = setup();
        let end = clock.now() + window;
        let (mut idle_starts, mut inbox_syncs) = (0, 0);
        let mut pending = s.start();
        for _ in 0..10_000 {
            let mut deadline = None;
            let mut queue = VecDeque::from(pending);
            while let Some(action) = queue.pop_front() {
                match action {
                    Action::StartIdle(_) => {
                        idle_starts += 1;
                        queue.extend(s.handle(Event::IdleFailed));
                    }
                    Action::SyncInbox(_) => {
                        inbox_syncs += 1;
                        queue.extend(s.handle(Event::SyncOk { new_messages: 0 }));
                    }
                    Action::Sleep(at) => deadline = Some(at),
                    _ => {}
                }
            }
            match deadline {
                Some(at) if at <= end => {
                    clock.advance(at.saturating_duration_since(clock.now()));
                    pending = s.handle(Event::Tick);
                }
                _ => break,
            }
        }
        (idle_starts, inbox_syncs)
    }

    #[test]
    fn failing_idle_with_healthy_sync_is_bounded_in_ten_minutes() {
        // Sin contador separado (el bug original) habría ~120 arranques de IDLE a 5 s.
        let (idle_starts, inbox_syncs) = simulate_failing_idle(10 * MIN);
        assert!(idle_starts <= 3, "arranques de IDLE: {idle_starts}");
        assert!(
            inbox_syncs <= 5,
            "sincronizaciones de bandeja: {inbox_syncs}"
        );
    }

    #[test]
    fn sync_ok_does_not_reset_idle_failures() {
        let (_clock, mut s) = setup();
        s.start();
        s.handle(Event::IdleFailed);
        s.handle(Event::SyncOk { new_messages: 0 });
        s.handle(Event::IdleFailed);
        s.handle(Event::SyncOk { new_messages: 0 });
        assert!(!s.idle_unsupported);
        assert_eq!(s.idle_failures, 2);
        s.handle(Event::IdleFailed);
        assert!(
            s.idle_unsupported,
            "tres fallos seguidos degradan a polling"
        );
    }

    #[test]
    fn healthy_idle_resets_the_failure_counter() {
        let (_clock, mut s) = setup();
        s.start();
        s.handle(Event::IdleFailed);
        s.handle(Event::IdleFailed);
        s.handle(Event::IdleWake { lasted: IDLE_MAX });
        s.handle(Event::IdleFailed);
        s.handle(Event::IdleFailed);
        assert!(!s.idle_unsupported);
        s.handle(Event::IdleFailed);
        assert!(s.idle_unsupported);
    }

    #[test]
    fn idle_that_returns_immediately_counts_as_failure() {
        let (_clock, mut s) = setup();
        s.start();
        for _ in 0..3 {
            s.handle(Event::IdleWake {
                lasted: Duration::ZERO,
            });
        }
        assert!(s.idle_unsupported);
    }

    #[test]
    fn degraded_idle_is_retried_exactly_one_hour_later() {
        let (clock, mut s) = setup();
        s.start();
        let mut actions = Vec::new();
        for _ in 0..IDLE_FAILURES_TO_DEGRADE {
            actions = s.handle(Event::IdleFailed);
        }
        let degraded_at = clock.now();
        for _ in 0..1000 {
            let due = sleep_of(&actions).expect("debe haber un despertar programado");
            clock.advance(due.saturating_duration_since(clock.now()));
            actions = s.handle(Event::Tick);
            if actions.contains(&Action::StartIdle(ACCOUNT)) {
                break;
            }
        }
        assert!(actions.contains(&Action::StartIdle(ACCOUNT)));
        assert_eq!(clock.now() - degraded_at, IDLE_RETRY);
    }

    #[test]
    fn auth_failure_is_terminal_until_reauthorized() {
        let (clock, mut s) = setup();
        s.start();
        assert_eq!(
            s.handle(Event::AuthFailed),
            vec![Action::AuthRequired(ACCOUNT), Action::Pause]
        );
        clock.advance(10 * 3600 * Duration::from_secs(1));
        let noise = [
            Event::Tick,
            Event::SyncFailed,
            Event::SyncOk { new_messages: 3 },
            Event::IdleWake { lasted: IDLE_MAX },
            Event::IdleFailed,
            Event::IdleUnsupported,
            Event::MailboxOpened(SECONDARY),
            Event::Resumed,
            Event::ConditionsChanged(Conditions::default()),
        ];
        for event in noise {
            assert!(
                s.handle(event).is_empty(),
                "{event:?} no debía producir acciones"
            );
        }
        assert_eq!(
            without_sleep(s.handle(Event::Reauthorized)),
            vec![Action::StartIdle(ACCOUNT), Action::SyncInbox(ACCOUNT)]
        );
    }

    #[test]
    fn reauthorized_while_offline_stays_paused() {
        let (_clock, mut s) = setup();
        s.start();
        s.handle(Event::AuthFailed);
        assert!(s.handle(Event::ConditionsChanged(offline())).is_empty());
        assert_eq!(s.handle(Event::Reauthorized), vec![Action::Pause]);
        assert!(s.handle(Event::Tick).is_empty());
    }
}

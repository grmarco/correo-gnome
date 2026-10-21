//! Planificación de la sincronización: IDLE, polling con intervalos crecientes,
//! reconexión con backoff, barrido de carpetas y política según batería y red.
//!
//! `SyncScheduler` es puro (reloj inyectado). `SyncRunner` conecta el planificador
//! con un `MailBackend` y expone un canal de `SyncReport` para las notificaciones.

mod clock;
mod runner;
mod scheduler;

pub use clock::{Clock, FakeClock, SystemClock};
pub use runner::{
    INTERRUPTED_REASON, IdleBackend, NewMessageIds, PAUSED_REASON, SIN_IDLE, SyncChannels,
    SyncNotice, SyncReport, SyncRunner,
};
pub use scheduler::{
    Action, Conditions, Event, IDLE_MAX, Jitter, Network, Policy, SyncScheduler, backoff_delay,
};

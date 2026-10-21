//! Fuente de tiempo inyectable: el planificador nunca lee el reloj directamente.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Reloj real (monotónico).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Reloj simulado. Los clones comparten el mismo instante, así que un test
/// puede conservar uno y avanzar el tiempo que ve el planificador.
#[derive(Debug, Clone)]
pub struct FakeClock {
    now: Arc<Mutex<Instant>>,
}

impl FakeClock {
    pub fn new() -> Self {
        Self {
            now: Arc::new(Mutex::new(Instant::now())),
        }
    }

    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().expect("reloj simulado envenenado");
        *now += by;
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        *self.now.lock().expect("reloj simulado envenenado")
    }
}

//! Vigilancia de energía y red: traduce `gio::PowerProfileMonitor`, `gio::NetworkMonitor`,
//! UPower (batería baja) y la señal `PrepareForSleep` de logind a `correo_sync::Conditions`.

use std::cell::Cell;
use std::rc::Rc;

use correo_sync::{Conditions, Network};
use gio::prelude::*;

const UPOWER_SERVICE: &str = "org.freedesktop.UPower";
const UPOWER_DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const UPOWER_DEVICE_IFACE: &str = "org.freedesktop.UPower.Device";
/// Niveles de aviso de UPower (`UpDeviceLevel`): LOW=3, CRITICAL=4, ACTION=5.
const WARNING_LEVEL_LOW: u32 = 3;
const WARNING_LEVEL_ACTION: u32 = 5;

const LOGIND_SERVICE: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

/// Lecturas crudas del entorno.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PowerInputs {
    pub online: bool,
    /// Red de pago (`NetworkMonitor::is_network_metered`).
    pub metered: bool,
    /// Modo ahorro de energía del sistema.
    pub power_saver: bool,
    /// Batería baja o crítica según UPower.
    pub low_battery: bool,
}

/// Combina las lecturas en las condiciones que entiende el planificador.
/// Función pura: `low_battery` y `power_saver` se pasan tal cual; el planificador ya
/// trata ambos como "low power" (`Conditions::low_power`).
pub fn combine(inputs: PowerInputs, window_visible: bool) -> Conditions {
    Conditions {
        network: if inputs.online {
            Network::Online
        } else {
            Network::Offline
        },
        metered: inputs.metered,
        power_saver: inputs.power_saver,
        low_battery: inputs.low_battery,
        window_visible,
    }
}

/// `true` si el nivel de aviso de UPower corresponde a batería baja o peor.
pub fn is_low_warning_level(level: u32) -> bool {
    (WARNING_LEVEL_LOW..=WARNING_LEVEL_ACTION).contains(&level)
}

struct Inner {
    inputs: Cell<PowerInputs>,
    window_visible: Cell<bool>,
    last: Cell<Option<Conditions>>,
    on_change: Box<dyn Fn(Conditions)>,
}

impl Inner {
    /// Emite si las condiciones cambiaron respecto a la última emisión.
    fn refresh(&self) {
        let next = combine(self.inputs.get(), self.window_visible.get());
        if self.last.get() != Some(next) {
            self.last.set(Some(next));
            (self.on_change)(next);
        }
    }
}

/// Mantiene las condiciones actuales y llama a `on_change` cada vez que cambian.
/// Los monitores son singletons de GIO, así que el watcher vive mientras viva la app.
pub struct PowerWatcher {
    inner: Rc<Inner>,
    _battery: Option<gio::DBusProxy>,
}

impl PowerWatcher {
    /// Conecta los monitores. No emite nada hasta `emit_current` o el primer cambio.
    pub fn new(on_change: impl Fn(Conditions) + 'static) -> Self {
        let monitor = gio::PowerProfileMonitor::get_default();
        let network = gio::NetworkMonitor::default();
        let inner = Rc::new(Inner {
            inputs: Cell::new(read_inputs(&monitor, &network, false)),
            window_visible: Cell::new(false),
            last: Cell::new(None),
            on_change: Box::new(on_change),
        });

        // Los closures de señal retienen solo un Weak para no formar ciclos con `inner`.
        let weak = Rc::downgrade(&inner);
        monitor.connect_power_saver_enabled_notify(move |m| {
            if let Some(inner) = weak.upgrade() {
                update(&inner, |i| i.power_saver = m.is_power_saver_enabled());
            }
        });
        let weak = Rc::downgrade(&inner);
        network.connect_network_changed(move |n, available| {
            if let Some(inner) = weak.upgrade() {
                update(&inner, |i| {
                    i.online = available;
                    i.metered = n.is_network_metered();
                });
            }
        });
        let weak = Rc::downgrade(&inner);
        network.connect_network_metered_notify(move |n| {
            if let Some(inner) = weak.upgrade() {
                update(&inner, |i| i.metered = n.is_network_metered());
            }
        });

        let battery = connect_battery(&inner);
        Self {
            inner,
            _battery: battery,
        }
    }

    /// La ventana principal visible habilita el barrido de carpetas suscritas.
    pub fn set_window_visible(&self, visible: bool) {
        self.inner.window_visible.set(visible);
        self.inner.refresh();
    }

    pub fn conditions(&self) -> Conditions {
        combine(self.inner.inputs.get(), self.inner.window_visible.get())
    }
}

/// Aplica un cambio a las lecturas y emite si procede.
fn update(inner: &Inner, change: impl FnOnce(&mut PowerInputs)) {
    let mut inputs = inner.inputs.get();
    change(&mut inputs);
    inner.inputs.set(inputs);
    inner.refresh();
}

fn read_inputs(
    monitor: &gio::PowerProfileMonitor,
    network: &gio::NetworkMonitor,
    low_battery: bool,
) -> PowerInputs {
    PowerInputs {
        online: network.is_network_available(),
        metered: network.is_network_metered(),
        power_saver: monitor.is_power_saver_enabled(),
        low_battery,
    }
}

/// Suscribe a UPower (DisplayDevice). Sin UPower (escritorio sin batería o D-Bus
/// no disponible) `low_battery` se queda en `false`.
fn connect_battery(inner: &Rc<Inner>) -> Option<gio::DBusProxy> {
    let bus = gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>).ok()?;
    let proxy = gio::DBusProxy::new_sync(
        &bus,
        gio::DBusProxyFlags::NONE,
        None,
        Some(UPOWER_SERVICE),
        UPOWER_DISPLAY_DEVICE,
        UPOWER_DEVICE_IFACE,
        None::<&gio::Cancellable>,
    )
    .ok()?;

    let weak = Rc::downgrade(inner);
    let watched = proxy.clone();
    proxy.connect_local("g-properties-changed", false, move |_| {
        if let Some(inner) = weak.upgrade() {
            apply_battery(&inner, &watched);
        }
        None
    });
    apply_battery(inner, &proxy);
    Some(proxy)
}

fn apply_battery(inner: &Inner, proxy: &gio::DBusProxy) {
    let low = proxy
        .cached_property("WarningLevel")
        .and_then(|v| v.get::<u32>())
        .is_some_and(is_low_warning_level);
    update(inner, |i| i.low_battery = low);
}

/// Suscripción a la reanudación tras suspensión (logind `PrepareForSleep(false)`).
/// Mientras viva, llama a `on_resume`. Si no hay logind, devuelve `None`.
pub struct ResumeWatch {
    _subscription: gio::SignalSubscription,
}

pub fn watch_resume(on_resume: impl Fn() + 'static) -> Option<ResumeWatch> {
    let bus = gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>).ok()?;
    let subscription = bus.subscribe_to_signal(
        Some(LOGIND_SERVICE),
        Some(LOGIND_MANAGER),
        Some("PrepareForSleep"),
        Some(LOGIND_PATH),
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            // El argumento es `true` al entrar en suspensión y `false` al reanudar.
            if signal.parameters.child_value(0).get::<bool>() == Some(false) {
                on_resume();
            }
        },
    );
    Some(ResumeWatch {
        _subscription: subscription,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(online: bool, metered: bool, saver: bool, low: bool) -> PowerInputs {
        PowerInputs {
            online,
            metered,
            power_saver: saver,
            low_battery: low,
        }
    }

    #[test]
    fn online_unmetered_plugged_in_is_plain_online() {
        let c = combine(inputs(true, false, false, false), true);
        assert_eq!(c.network, Network::Online);
        assert!(!c.metered && !c.power_saver && !c.low_battery);
        assert!(c.window_visible);
        assert!(!c.low_power());
    }

    #[test]
    fn offline_maps_to_offline_network() {
        let c = combine(inputs(false, false, false, false), false);
        assert_eq!(c.network, Network::Offline);
    }

    #[test]
    fn metered_is_passed_through() {
        let c = combine(inputs(true, true, false, false), false);
        assert!(c.metered);
        assert!(!c.low_power());
    }

    #[test]
    fn power_saver_or_low_battery_means_low_power() {
        assert!(combine(inputs(true, false, true, false), false).low_power());
        assert!(combine(inputs(true, false, false, true), false).low_power());
        assert!(!combine(inputs(true, false, false, false), false).low_power());
    }

    #[test]
    fn warning_levels_low_critical_action_are_low_battery() {
        assert!(!is_low_warning_level(0)); // unknown
        assert!(!is_low_warning_level(1)); // none
        assert!(!is_low_warning_level(2)); // discharging
        assert!(is_low_warning_level(3)); // low
        assert!(is_low_warning_level(4)); // critical
        assert!(is_low_warning_level(5)); // action
        assert!(!is_low_warning_level(6)); // normal
        assert!(!is_low_warning_level(8)); // full
    }
}

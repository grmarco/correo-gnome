// Temporal: varios módulos de la interfaz aún no están conectados desde main.
#![allow(dead_code)]

mod composer;
mod message_list;
mod notify;
mod reader;
mod search;
mod services;
mod setup;
mod sidebar;
mod ui_types;
mod window;

use std::rc::Rc;

use adw::prelude::*;

use services::Services;

const APP_ID: &str = "io.github.gmarco.Correo";

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        let services = Rc::new(build_services());
        window::present(app, services);
    });
    app.run()
}

/// Con `CORREO_DEMO=1` (solo compilaciones de depuración) arranca con una base en
/// memoria y datos ficticios, sin tocar la configuración real.
fn build_services() -> Services {
    #[cfg(debug_assertions)]
    {
        if std::env::var_os("CORREO_DEMO").is_some() {
            return window::demo_services().expect("no se pudo crear la demostración");
        }
    }
    setup::init_services().expect("no se pudo iniciar los servicios")
}

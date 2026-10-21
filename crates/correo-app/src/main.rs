mod composer;
mod notify;
mod reader;
mod search;
mod services;
mod setup;
mod ui_types;
mod window;

use std::rc::Rc;

use adw::prelude::*;

const APP_ID: &str = "io.github.gmarco.Correo";

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        let services = Rc::new(setup::init_services().expect("no se pudo iniciar los servicios"));
        window::present(app, services);
    });
    app.run()
}

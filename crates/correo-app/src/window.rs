//! T9: ventana principal. `NavigationSplitView` externo (cuentas y carpetas) que
//! contiene otro (lista de hilos | lector). Toda la E/S de red va por `Services::spawn`.

use std::cell::RefCell;
use std::future::Future;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use correo_core::{Account, Mailbox, MailboxId, MailboxRole, MessageHeader, Result as CoreResult};
use correo_store::Store;
use gtk::{gio, glib};

use crate::composer;
use crate::message_list::{thread_rows, MessageList};
use crate::notify;
use crate::reader::MessageView;
use crate::search;
use crate::services::Services;
use crate::setup::{self, SyncHandle};
use crate::sidebar::{Sidebar, Target};
use crate::ui_types::ComposeMode;

/// Cabeceras por página al desplazarse.
const PAGE: usize = 50;
/// Resultados máximos de la búsqueda local.
const SEARCH_LIMIT: usize = 200;
/// Tipo de las acciones de notificación: (id de carpeta, uid).
const MESSAGE_TARGET: &str = "(xu)";

thread_local! {
    /// Ventana única de la aplicación. Vive mientras dure el proceso: así las acciones
    /// de las notificaciones funcionan aunque la ventana esté oculta (modo residente).
    static SHELL: RefCell<Option<Rc<Shell>>> = const { RefCell::new(None) };
}

/// Muestra la ventana principal, o la vuelve a mostrar si ya existe.
pub fn present(app: &adw::Application, services: Rc<Services>) {
    if let Some(shell) = SHELL.with(|s| s.borrow().clone()) {
        shell.window.present();
        return;
    }

    let shell = Shell::new(app, services);
    shell.install_actions();
    shell.install_app_actions(app);
    shell.start();

    // Con CORREO_DEMO no se toca la red: la base es ficticia.
    if !demo_mode() {
        let weak = shell.weak.clone();
        let weak_auth = shell.weak.clone();
        let handle = setup::start_sync(
            shell.services.clone(),
            app,
            move |account, ids| {
                if let Some(s) = weak.upgrade() {
                    s.on_new_messages(account, &ids);
                }
            },
            move |account| {
                if let Some(s) = weak_auth.upgrade() {
                    s.on_auth_required(&account);
                }
            },
        );
        *shell.sync.borrow_mut() = Some(handle);
        for account in shell.services.accounts().unwrap_or_default() {
            shell.sync_contacts(&account);
        }
    }
    shell.watch_focus();

    shell.window.present();
    shell.update_visibility();
    SHELL.with(|s| *s.borrow_mut() = Some(shell));
}

/// Modo de demostración (solo compilaciones de depuración con `CORREO_DEMO`).
fn demo_mode() -> bool {
    cfg!(debug_assertions) && std::env::var_os("CORREO_DEMO").is_some()
}

/// Con modo residente, cerrar la ventana solo la oculta: la app sigue activa en segundo
/// plano y las notificaciones pueden volver a abrirla.
fn resident() -> bool {
    setup::app_config().run_in_background
}

/// Fuente de cabeceras paginada para la vista actual.
#[derive(Clone)]
struct Source {
    mailbox: Mailbox,
    offset: usize,
    exhausted: bool,
}

struct State {
    accounts: Vec<Account>,
    mailboxes: Vec<Mailbox>,
    target: Target,
    sources: Vec<Source>,
    headers: Vec<MessageHeader>,
    /// Texto de búsqueda activo; vacío = listado normal.
    query: String,
    selected: Option<MessageHeader>,
    loading: bool,
}

pub struct Shell {
    app: adw::Application,
    window: adw::ApplicationWindow,
    services: Rc<Services>,
    weak: Weak<Shell>,
    toasts: adw::ToastOverlay,
    root: gtk::Stack,
    sidebar: Sidebar,
    list: MessageList,
    search: gtk::SearchEntry,
    sync_btn: gtk::Button,
    /// Sincronización en segundo plano; `None` en modo demostración.
    sync: RefCell<Option<SyncHandle>>,
    /// Cuentas recién añadidas cuyo primer informe de sincronización aún no ha llegado.
    fresh_accounts: RefCell<Vec<correo_core::AccountId>>,
    inner: adw::NavigationSplitView,
    reader_stack: gtk::Stack,
    reader_box: gtk::Box,
    /// Se crea al primer mensaje abierto, para no construir el lector sin uso.
    reader: RefCell<Option<MessageView>>,
    state: RefCell<State>,
}

impl Shell {
    fn new(app: &adw::Application, services: Rc<Services>) -> Rc<Self> {
        Rc::new_cyclic(|weak: &Weak<Shell>| {
            let sidebar = {
                let w = weak.clone();
                Sidebar::new(move |t| {
                    if let Some(s) = w.upgrade() {
                        s.set_target(t);
                    }
                })
            };
            {
                let w = weak.clone();
                sidebar.connect_add_account(move || {
                    if let Some(s) = w.upgrade() {
                        s.add_account();
                    }
                });
            }
            let list = {
                let w = weak.clone();
                MessageList::new(move |h| {
                    if let Some(s) = w.upgrade() {
                        s.on_select(h);
                    }
                })
            };

            let window = adw::ApplicationWindow::builder()
                .application(app)
                .title("Correo")
                .default_width(1100)
                .default_height(720)
                .build();
            window.connect_close_request(|win| {
                if resident() {
                    win.set_visible(false);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });

            // Lista de mensajes: cabecera con sincronizar, búsqueda y redactar.
            let search = gtk::SearchEntry::builder()
                .placeholder_text("Buscar correo")
                .hexpand(true)
                .width_request(240)
                .build();
            let sync_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
            sync_btn.set_tooltip_text(Some("Sincronizar"));
            let compose_btn = gtk::Button::with_label("Redactar");
            compose_btn.add_css_class("suggested-action");
            let list_header = adw::HeaderBar::new();
            list_header.pack_start(&sync_btn);
            list_header.pack_end(&compose_btn);
            list_header.set_title_widget(Some(&search));
            let list_toolbar = adw::ToolbarView::new();
            list_toolbar.add_top_bar(&list_header);
            list_toolbar.set_content(Some(list.widget()));
            let list_page = adw::NavigationPage::new(&list_toolbar, "Mensajes");

            // Lector: placeholder hasta que se abre un mensaje.
            let reader_empty = adw::StatusPage::new();
            reader_empty.set_icon_name(Some("mail-read-symbolic"));
            reader_empty.set_title("Selecciona un mensaje");
            let reader_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
            reader_box.set_hexpand(true);
            reader_box.set_vexpand(true);
            let reader_stack = gtk::Stack::new();
            reader_stack.add_named(&reader_empty, Some("empty"));
            reader_stack.add_named(&reader_box, Some("reader"));
            reader_stack.set_visible_child_name("empty");
            let reader_toolbar = adw::ToolbarView::new();
            reader_toolbar.add_top_bar(&adw::HeaderBar::new());
            reader_toolbar.set_content(Some(&reader_stack));
            let reader_page = adw::NavigationPage::new(&reader_toolbar, "Mensaje");

            let inner = adw::NavigationSplitView::new();
            inner.set_min_sidebar_width(320.0);
            inner.set_sidebar(Some(&list_page));
            inner.set_content(Some(&reader_page));

            let outer = adw::NavigationSplitView::new();
            outer.set_min_sidebar_width(220.0);
            outer.set_sidebar(Some(sidebar.page()));
            let inner_page = adw::NavigationPage::new(&inner, "Correo");
            outer.set_content(Some(&inner_page));

            // Estado vacío: sin cuentas.
            let empty_header = adw::HeaderBar::new();
            let empty_status = adw::StatusPage::new();
            empty_status.set_icon_name(Some("mail-unread-symbolic"));
            empty_status.set_title("Añade tu primera cuenta");
            empty_status.set_description(Some(
                "Correo funciona con Gmail, Outlook.com y Microsoft 365.",
            ));
            let add_btn = gtk::Button::with_label("Añadir cuenta");
            add_btn.add_css_class("pill");
            add_btn.add_css_class("suggested-action");
            {
                let w = weak.clone();
                add_btn.connect_clicked(move |_| {
                    if let Some(s) = w.upgrade() {
                        s.add_account();
                    }
                });
            }
            empty_status.set_child(Some(&add_btn));
            let empty_toolbar = adw::ToolbarView::new();
            empty_toolbar.add_top_bar(&empty_header);
            empty_toolbar.set_content(Some(&empty_status));

            let root = gtk::Stack::new();
            root.add_named(&outer, Some("main"));
            root.add_named(&empty_toolbar, Some("empty"));

            let toasts = adw::ToastOverlay::new();
            toasts.set_child(Some(&root));
            window.set_content(Some(&toasts));

            // Puntos de corte: a 720 px se colapsa la barra lateral; a 1000 px, la lista.
            for (condition, split) in [("max-width: 720sp", &outer), ("max-width: 1000sp", &inner)]
            {
                let bp = adw::Breakpoint::new(
                    adw::BreakpointCondition::parse(condition).expect("condición de breakpoint"),
                );
                bp.add_setter(split, "collapsed", Some(&true.to_value()));
                window.add_breakpoint(bp);
            }

            // Sincronizar, redactar y búsqueda.
            {
                let w = weak.clone();
                sync_btn.connect_clicked(move |_| {
                    if let Some(s) = w.upgrade() {
                        s.sync();
                    }
                });
            }
            {
                let w = weak.clone();
                compose_btn.connect_clicked(move |_| {
                    if let Some(s) = w.upgrade() {
                        s.compose(ComposeMode::New);
                    }
                });
            }
            {
                let w = weak.clone();
                search.connect_search_changed(move |_| {
                    if let Some(s) = w.upgrade() {
                        s.on_search();
                    }
                });
            }

            // Carga bajo demanda al acercarse al final de la lista.
            let adj = list.scroller().vadjustment();
            {
                let w = weak.clone();
                adj.connect_changed(move |a| {
                    if let Some(s) = w.upgrade() {
                        s.maybe_load_more(a);
                    }
                });
            }
            {
                let w = weak.clone();
                adj.connect_value_changed(move |a| {
                    if let Some(s) = w.upgrade() {
                        s.maybe_load_more(a);
                    }
                });
            }

            Shell {
                app: app.clone(),
                window,
                services,
                weak: weak.clone(),
                toasts,
                root,
                sidebar,
                list,
                search,
                sync_btn,
                inner,
                reader_stack,
                reader_box,
                reader: RefCell::new(None),
                sync: RefCell::new(None),
                fresh_accounts: RefCell::new(Vec::new()),
                state: RefCell::new(State {
                    accounts: Vec::new(),
                    mailboxes: Vec::new(),
                    target: Target::Unified,
                    sources: Vec::new(),
                    headers: Vec::new(),
                    query: String::new(),
                    selected: None,
                    loading: false,
                }),
            }
        })
    }

    /// Acciones de ventana y atajos de teclado.
    fn install_actions(self: &Rc<Self>) {
        self.action("compose", &["<Primary>n"], |s| s.compose(ComposeMode::New));
        self.action("sync", &["<Primary>r"], |s| s.sync());
        self.action("archive", &["<Primary>e"], |s| s.archive());
        self.action("delete", &["<Primary>Delete"], |s| s.delete());
        self.action("toggle-read", &["<Primary>u"], |s| s.toggle_read());
        self.action("search", &["<Primary>f"], |s| {
            s.search.grab_focus();
        });
    }

    /// Acciones de aplicación: las usan las notificaciones (`app.open-message`,
    /// `app.mark-read`, `app.archive`) y funcionan aunque la ventana esté oculta.
    fn install_app_actions(self: &Rc<Self>, app: &adw::Application) {
        let ty = glib::VariantTy::new(MESSAGE_TARGET).expect("tipo de acción válido");
        type MessageAction = fn(&Rc<Shell>, MailboxId, u32);
        let entries: [(&str, MessageAction); 3] = [
            ("open-message", |s, m, uid| s.open_message(m, uid)),
            ("mark-read", |s, m, uid| s.mark_read_message(m, uid)),
            ("archive", |s, m, uid| s.archive_message_by_id(m, uid)),
        ];
        for (name, f) in entries {
            let action = gio::SimpleAction::new(name, Some(ty));
            let w = self.weak.clone();
            action.connect_activate(move |_, param| {
                let Some((mailbox, uid)) = param.and_then(|p| p.get::<(i64, u32)>()) else {
                    return;
                };
                if let Some(s) = w.upgrade() {
                    f(&s, mailbox, uid);
                }
            });
            app.add_action(&action);
        }
    }

    /// Visibilidad efectiva de la ventana: visible y con el foco. Alimenta el barrido
    /// de sincronización y la decisión de notificar.
    fn is_in_front(&self) -> bool {
        self.window.is_visible() && self.window.is_active()
    }

    fn update_visibility(&self) {
        let visible = self.is_in_front();
        if let Some(sync) = self.sync.borrow().as_ref() {
            sync.set_window_visible(visible);
        }
    }

    fn watch_focus(self: &Rc<Self>) {
        let w = self.weak.clone();
        self.window.connect_is_active_notify(move |_| {
            if let Some(s) = w.upgrade() {
                s.update_visibility();
            }
        });
    }

    fn action(self: &Rc<Self>, name: &str, accels: &[&str], f: fn(&Rc<Shell>)) {
        let action = gio::SimpleAction::new(name, None);
        let w = self.weak.clone();
        action.connect_activate(move |_, _| {
            if let Some(s) = w.upgrade() {
                f(&s);
            }
        });
        self.window.add_action(&action);
        self.app
            .set_accels_for_action(&format!("win.{name}"), accels);
    }

    /// Carga cuentas y carpetas, y muestra la vista inicial o el estado vacío.
    fn start(self: &Rc<Self>) {
        if let Err(e) = self.reload_model() {
            self.toast(format!("No se pudo leer la base de datos: {e}"));
        }
        let has_accounts = !self.state.borrow().accounts.is_empty();
        if has_accounts {
            self.root.set_visible_child_name("main");
            self.apply_target();
        } else {
            self.root.set_visible_child_name("empty");
        }
    }

    // ----- Modelo ----------------------------------------------------------

    fn with_store<T>(&self, f: impl FnOnce(&Store) -> CoreResult<T>) -> CoreResult<T> {
        let store = self.services.store.lock().expect("store envenenado");
        f(&store)
    }

    /// Relee cuentas y carpetas de la caché.
    fn reload_model(&self) -> CoreResult<()> {
        let accounts = self.with_store(|s| s.list_accounts())?;
        let mut mailboxes = Vec::new();
        for a in &accounts {
            mailboxes.extend(self.with_store(|s| s.list_mailboxes(a.id))?);
        }
        let mut st = self.state.borrow_mut();
        st.accounts = accounts;
        st.mailboxes = mailboxes;
        Ok(())
    }

    fn account(&self, id: i64) -> Option<Account> {
        self.state
            .borrow()
            .accounts
            .iter()
            .find(|a| a.id == id)
            .cloned()
    }

    fn mailbox(&self, id: MailboxId) -> Option<Mailbox> {
        self.state
            .borrow()
            .mailboxes
            .iter()
            .find(|m| m.id == id)
            .cloned()
    }

    /// Carpetas que cubre el destino actual.
    fn target_mailboxes(&self) -> Vec<Mailbox> {
        let st = self.state.borrow();
        match st.target {
            Target::Unified => st
                .mailboxes
                .iter()
                .filter(|m| m.role == MailboxRole::Inbox)
                .cloned()
                .collect(),
            Target::Mailbox(id) => st
                .mailboxes
                .iter()
                .filter(|m| m.id == id)
                .cloned()
                .collect(),
        }
    }

    fn reload_sidebar(&self) {
        let (groups, target) = {
            let st = self.state.borrow();
            let groups: Vec<(Account, Vec<Mailbox>)> = st
                .accounts
                .iter()
                .map(|a| {
                    let boxes = st
                        .mailboxes
                        .iter()
                        .filter(|m| m.account_id == a.id)
                        .cloned()
                        .collect();
                    (a.clone(), boxes)
                })
                .collect();
            (groups, st.target)
        };
        self.sidebar.reload(&groups, target);
    }

    // ----- Listado ---------------------------------------------------------

    fn set_target(self: &Rc<Self>, target: Target) {
        if self.state.borrow().target == target {
            return;
        }
        self.state.borrow_mut().target = target;
        if let Target::Mailbox(id) = target {
            if let (Some(mailbox), Some(sync)) = (self.mailbox(id), self.sync.borrow().as_ref()) {
                sync.mailbox_opened(mailbox.account_id, id);
            }
        }
        self.apply_target();
    }

    /// Reinicia la vista para el destino actual (sin búsqueda).
    fn apply_target(self: &Rc<Self>) {
        let sources: Vec<Source> = self
            .target_mailboxes()
            .into_iter()
            .map(|mailbox| Source {
                mailbox,
                offset: 0,
                exhausted: false,
            })
            .collect();
        {
            let mut st = self.state.borrow_mut();
            st.sources = sources;
            st.query.clear();
            st.selected = None;
            st.headers.clear();
        }
        // Vacía el campo sin disparar una búsqueda (query ya es "").
        self.search.set_text("");
        self.reader_stack.set_visible_child_name("empty");
        self.reload_sidebar();
        self.reset_list();
    }

    /// Descarta las cabeceras cargadas y vuelve a cargar la primera página.
    fn reset_list(self: &Rc<Self>) {
        {
            let mut st = self.state.borrow_mut();
            st.headers.clear();
            for s in &mut st.sources {
                s.offset = 0;
                s.exhausted = false;
            }
        }
        self.load_more();
    }

    /// Carga la siguiente página de cada fuente (fuera de búsqueda).
    fn load_more(self: &Rc<Self>) {
        let plan: Vec<(usize, Mailbox, usize)> = {
            let mut st = self.state.borrow_mut();
            if st.loading || !st.query.is_empty() {
                return;
            }
            let plan: Vec<_> = st
                .sources
                .iter()
                .enumerate()
                .filter(|(_, s)| !s.exhausted)
                .map(|(i, s)| (i, s.mailbox.clone(), s.offset))
                .collect();
            if plan.is_empty() {
                return;
            }
            st.loading = true;
            plan
        };

        let mut fetched: Vec<(usize, Vec<MessageHeader>)> = Vec::new();
        let result = self.with_store(|s| {
            for (i, mailbox, offset) in &plan {
                fetched.push((*i, s.list_headers(mailbox.id, PAGE, *offset)?));
            }
            Ok(())
        });

        {
            let mut st = self.state.borrow_mut();
            st.loading = false;
            if result.is_ok() {
                for (i, headers) in fetched {
                    let n = headers.len();
                    let source = &mut st.sources[i];
                    source.offset += n;
                    source.exhausted = n < PAGE;
                    st.headers.extend(headers);
                }
                st.headers.sort_by_key(|m| std::cmp::Reverse(m.date));
            }
        }
        if let Err(e) = result {
            self.toast(format!("Error al leer la caché: {e}"));
        }
        self.refresh_rows();
    }

    fn maybe_load_more(self: &Rc<Self>, adj: &gtk::Adjustment) {
        if adj.value() + adj.page_size() >= adj.upper() - 300.0 {
            self.load_more();
        }
    }

    fn refresh_rows(&self) {
        let rows = thread_rows(&self.state.borrow().headers);
        self.list.set_rows(&rows);
    }

    fn on_search(self: &Rc<Self>) {
        let text = self.search.text().trim().to_string();
        if self.state.borrow().query == text {
            return;
        }
        if text.is_empty() {
            self.state.borrow_mut().query.clear();
            self.reset_list();
            return;
        }
        let hits = search::search_local(&self.services, &text, SEARCH_LIMIT);
        {
            let mut st = self.state.borrow_mut();
            st.query = text;
            st.headers = hits;
        }
        self.refresh_rows();
    }

    // ----- Selección y lector ----------------------------------------------

    fn on_select(self: &Rc<Self>, header: Option<MessageHeader>) {
        self.state.borrow_mut().selected = header.clone();
        let Some(h) = header else {
            self.reader_stack.set_visible_child_name("empty");
            if let Some(reader) = self.reader.borrow().as_ref() {
                reader.clear();
            }
            return;
        };
        let Some(mailbox) = self.mailbox(h.mailbox_id) else {
            return;
        };
        let Some(account) = self.account(mailbox.account_id) else {
            return;
        };
        self.ensure_reader();
        if let Some(reader) = self.reader.borrow().as_ref() {
            reader.show(account, mailbox, h);
        }
        self.reader_stack.set_visible_child_name("reader");
        self.inner.set_show_content(true);
    }

    fn ensure_reader(self: &Rc<Self>) {
        if self.reader.borrow().is_some() {
            return;
        }
        let view = MessageView::new(self.services.clone());
        let w = self.weak.clone();
        view.connect_compose(move |mode| {
            if let Some(s) = w.upgrade() {
                s.compose(mode);
            }
        });
        let w = self.weak.clone();
        view.connect_changed(move || {
            if let Some(s) = w.upgrade() {
                s.on_reader_changed();
            }
        });
        self.reader_box.append(view.widget());
        *self.reader.borrow_mut() = Some(view);
    }

    /// Abre el compositor. Para respuestas y reenvíos usa la cuenta del mensaje.
    fn compose(self: &Rc<Self>, mode: ComposeMode) {
        let account = match &mode {
            ComposeMode::New => self.state.borrow().accounts.first().cloned(),
            ComposeMode::Reply(h, _) | ComposeMode::ReplyAll(h, _) | ComposeMode::Forward(h, _) => {
                self.mailbox(h.mailbox_id)
                    .and_then(|m| self.account(m.account_id))
            }
        };
        match account {
            Some(account) => composer::open(&self.app, self.services.clone(), account, mode),
            None => self.toast("Añade una cuenta antes de redactar"),
        }
    }

    // ----- Cuentas y avisos del sincronizador -------------------------------

    /// Abre el alta de cuenta; al terminar, la añade a la barra y la sincroniza.
    fn add_account(self: &Rc<Self>) {
        let w = self.weak.clone();
        setup::add_account_dialog(&self.window, self.services.clone(), move |account| {
            if let Some(s) = w.upgrade() {
                s.on_account_added(account);
            }
        });
    }

    fn on_account_added(self: &Rc<Self>, account: Account) {
        if let Err(e) = self.reload_model() {
            self.toast(format!("No se pudo leer la base de datos: {e}"));
        }
        self.root.set_visible_child_name("main");
        self.apply_target();

        self.fresh_accounts.borrow_mut().push(account.id);
        if let Some(sync) = self.sync.borrow().as_ref() {
            sync.account_reauthorized(account.clone());
        }
        self.sync_contacts(&account);
        self.toast(format!("Cuenta añadida: {}", account.email));
    }

    /// El proveedor rechazó las credenciales (token revocado o caducado): la
    /// sincronización de esa cuenta queda parada hasta que se vuelva a autorizar.
    fn on_auth_required(self: &Rc<Self>, account: &Account) {
        let toast = adw::Toast::new(&format!(
            "{}: hay que volver a iniciar sesión",
            account.email
        ));
        toast.set_button_label(Some("Reautorizar"));
        toast.set_timeout(0);
        let w = self.weak.clone();
        toast.connect_button_clicked(move |_| {
            if let Some(s) = w.upgrade() {
                s.add_account();
            }
        });
        self.toasts.add_toast(toast);
    }

    /// Descarga los contactos de la cuenta para el autocompletado del compositor. Un
    /// fallo no afecta al correo: se registra en la salida de error.
    fn sync_contacts(self: &Rc<Self>, account: &Account) {
        let source = self.services.contacts(account);
        let store = self.services.store.clone();
        let job = self.services.spawn(async move {
            correo_contacts::sync_into_store(source.as_ref(), &store)
                .await
                .map_err(|e| e.to_string())
        });
        let email = account.email.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Err(e) = job.await {
                eprintln!("correo: {email}: contactos no sincronizados: {e}");
            }
        });
    }

    /// Llega un informe de sincronización de una cuenta, con los ids de sus mensajes
    /// nuevos y no leídos (puede estar vacío). Avisa si la ventana no tiene el foco y
    /// refresca la lista si se muestra la bandeja de esa cuenta.
    fn on_new_messages(self: &Rc<Self>, account: Account, ids: &[i64]) {
        let headers: Vec<MessageHeader> = ids
            .iter()
            .filter_map(|id| self.with_store(|s| s.get_header(*id)).ok().flatten())
            .collect();
        notify::notify_new_if_inactive(&self.app, &account, &headers, self.is_in_front());

        // La primera sincronización de una cuenta recién añadida no trae ids nuevos
        // (la caché estaba vacía), pero sí debe verse en la lista.
        let first_report = {
            let mut fresh = self.fresh_accounts.borrow_mut();
            let before = fresh.len();
            fresh.retain(|id| *id != account.id);
            before != fresh.len()
        };
        if headers.is_empty() && !first_report {
            return;
        }
        if let Err(e) = self.reload_model() {
            self.toast(format!("Error al leer la caché: {e}"));
            return;
        }
        self.reload_sidebar();
        let shows_inbox = self
            .target_mailboxes()
            .iter()
            .any(|m| m.account_id == account.id && m.role == MailboxRole::Inbox);
        if shows_inbox {
            self.refresh_list();
        }
    }

    /// El lector cambió un mensaje (leído, no leído, archivado o borrado).
    fn on_reader_changed(self: &Rc<Self>) {
        if let Err(e) = self.reload_model() {
            self.toast(format!("Error al leer la caché: {e}"));
        }
        self.reload_sidebar();
        // Si el mensaje seleccionado ya no está en la caché, se limpia el lector.
        let selected = self.state.borrow().selected.clone();
        if let Some(h) = selected {
            match self.with_store(|s| s.get_header(h.id)) {
                Ok(Some(fresh)) => self.state.borrow_mut().selected = Some(fresh),
                Ok(None) => {
                    self.state.borrow_mut().selected = None;
                    self.reader_stack.set_visible_child_name("empty");
                }
                Err(_) => {}
            }
        }
        self.refresh_list();
    }

    /// Relee la lista de la vista actual: la búsqueda se repite; el listado normal,
    /// desde la primera página.
    fn refresh_list(self: &Rc<Self>) {
        let query = self.state.borrow().query.clone();
        if query.is_empty() {
            self.reset_list();
        } else {
            let hits = search::search_local(&self.services, &query, SEARCH_LIMIT);
            self.state.borrow_mut().headers = hits;
            self.refresh_rows();
        }
    }

    // ----- Sincronización --------------------------------------------------

    fn sync(self: &Rc<Self>) {
        let pairs: Vec<(Account, Mailbox)> = self
            .target_mailboxes()
            .into_iter()
            .filter_map(|m| self.account(m.account_id).map(|a| (a, m)))
            .collect();
        if pairs.is_empty() {
            self.toast("No hay carpetas que sincronizar");
            return;
        }

        // Todas las peticiones salen ya; la espera es secuencial en el hilo de GTK.
        let futures: Vec<_> = pairs
            .into_iter()
            .map(|(account, mailbox)| {
                let backend = self.services.backend(&account);
                let days = account.sync_days;
                self.services
                    .spawn(async move { backend.sync_mailbox(&mailbox, days).await })
            })
            .collect();

        self.sync_btn.set_sensitive(false);
        let w = self.weak.clone();
        glib::MainContext::default().spawn_local(async move {
            let mut new_messages = 0;
            let mut first_error = None;
            for f in futures {
                match f.await {
                    Ok(n) => new_messages += n,
                    Err(e) => {
                        first_error.get_or_insert(e.to_string());
                    }
                }
            }
            let Some(s) = w.upgrade() else { return };
            s.sync_btn.set_sensitive(true);
            if let Err(e) = s.reload_model() {
                s.toast(format!("Error al leer la caché: {e}"));
            }
            s.reload_sidebar();
            s.reset_list();
            match first_error {
                Some(e) => s.toast(format!("Sincronización incompleta: {e}")),
                None => s.toast(format!(
                    "Sincronizado: {new_messages} mensajes nuevos o cambiados"
                )),
            }
        });
    }

    // ----- Acciones sobre el mensaje seleccionado --------------------------

    fn selected_context(&self) -> Option<(Account, Mailbox, MessageHeader)> {
        let h = self.state.borrow().selected.clone()?;
        let mailbox = self.mailbox(h.mailbox_id)?;
        let account = self.account(mailbox.account_id)?;
        Some((account, mailbox, h))
    }

    /// Localiza un mensaje por (carpeta, uid) en la caché, con su cuenta y carpeta.
    /// Recarga el modelo antes: una notificación puede llegar de una cuenta recién añadida.
    fn context_for(
        &self,
        mailbox_id: MailboxId,
        uid: u32,
    ) -> Option<(Account, Mailbox, MessageHeader)> {
        if let Err(e) = self.reload_model() {
            self.toast(format!("Error al leer la caché: {e}"));
            return None;
        }
        let id = self
            .with_store(|s| s.message_id(mailbox_id, uid))
            .ok()
            .flatten()?;
        let h = self.with_store(|s| s.get_header(id)).ok().flatten()?;
        let mailbox = self.mailbox(mailbox_id)?;
        let account = self.account(mailbox.account_id)?;
        Some((account, mailbox, h))
    }

    /// Ejecuta una operación remota y, si sale bien, aplica `on_ok` en el hilo de GTK.
    fn run<T: 'static>(
        self: &Rc<Self>,
        fut: impl Future<Output = CoreResult<T>> + 'static,
        on_ok: impl FnOnce(&Rc<Shell>, T) + 'static,
    ) {
        let w = self.weak.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = fut.await;
            let Some(s) = w.upgrade() else { return };
            match result {
                Ok(v) => on_ok(&s, v),
                Err(e) => s.toast(format!("Error: {e}")),
            }
        });
    }

    fn toggle_read(self: &Rc<Self>) {
        let Some((account, mailbox, h)) = self.selected_context() else {
            return;
        };
        let seen = !h.flags.seen;
        self.set_seen_remote(&account, mailbox, h, seen);
    }

    fn set_seen_remote(
        self: &Rc<Self>,
        account: &Account,
        mailbox: Mailbox,
        h: MessageHeader,
        seen: bool,
    ) {
        let backend = self.services.backend(account);
        let uid = h.uid;
        let fut = self
            .services
            .spawn(async move { backend.set_seen(&mailbox, uid, seen).await });
        self.run(fut, move |s, ()| s.apply_seen(&h, seen));
    }

    fn archive(self: &Rc<Self>) {
        if let Some((account, mailbox, h)) = self.selected_context() {
            self.archive_remote(&account, mailbox, h);
        }
    }

    fn archive_remote(self: &Rc<Self>, account: &Account, mailbox: Mailbox, h: MessageHeader) {
        let target = self
            .state
            .borrow()
            .mailboxes
            .iter()
            .find(|m| m.account_id == account.id && m.role == MailboxRole::Archive)
            .cloned();
        let Some(target) = target else {
            self.toast("La cuenta no tiene carpeta de archivo");
            return;
        };
        if target.id == mailbox.id {
            return;
        }
        let backend = self.services.backend(account);
        let uid = h.uid;
        let fut = self
            .services
            .spawn(async move { backend.move_to(&mailbox, uid, &target).await });
        self.run(fut, move |s, ()| s.drop_local(&h));
    }

    fn delete(self: &Rc<Self>) {
        let Some((account, mailbox, h)) = self.selected_context() else {
            return;
        };
        let backend = self.services.backend(&account);
        let uid = h.uid;
        let fut = self
            .services
            .spawn(async move { backend.delete(&mailbox, uid).await });
        self.run(fut, move |s, ()| s.drop_local(&h));
    }

    // ----- Acciones de notificación (funcionan con la ventana oculta) ---------

    /// Presenta la ventana, selecciona la carpeta y abre el mensaje.
    fn open_message(self: &Rc<Self>, mailbox_id: MailboxId, uid: u32) {
        self.window.present();
        let Some((_, _, h)) = self.context_for(mailbox_id, uid) else {
            self.toast("El mensaje ya no está en la caché");
            return;
        };
        self.set_target(Target::Mailbox(mailbox_id));
        self.on_select(Some(h));
    }

    fn mark_read_message(self: &Rc<Self>, mailbox_id: MailboxId, uid: u32) {
        if let Some((account, mailbox, h)) = self.context_for(mailbox_id, uid) {
            self.set_seen_remote(&account, mailbox, h, true);
        }
    }

    fn archive_message_by_id(self: &Rc<Self>, mailbox_id: MailboxId, uid: u32) {
        if let Some((account, mailbox, h)) = self.context_for(mailbox_id, uid) {
            self.archive_remote(&account, mailbox, h);
        }
    }

    /// Refleja en la caché y en pantalla un cambio de estado de lectura.
    fn apply_seen(self: &Rc<Self>, h: &MessageHeader, seen: bool) {
        let mut flags = h.flags.clone();
        flags.seen = seen;
        let result = self.with_store(|s| {
            s.set_flags(h.mailbox_id, h.uid, &flags)?;
            Ok(())
        });
        if let Err(e) = result {
            self.toast(format!("Error al guardar el estado: {e}"));
        }
        let current = self.mailbox(h.mailbox_id).map_or(0, |m| m.unread);
        let unread = if seen {
            current.saturating_sub(1)
        } else {
            current + 1
        };
        self.set_unread(h.mailbox_id, unread);
        {
            let mut st = self.state.borrow_mut();
            for m in st.headers.iter_mut().filter(|m| m.id == h.id) {
                m.flags.seen = seen;
            }
            if let Some(sel) = st.selected.as_mut().filter(|m| m.id == h.id) {
                sel.flags.seen = seen;
            }
        }
        self.refresh_rows();
        self.reload_sidebar();
    }

    /// Quita un mensaje que ya no está en su carpeta (archivado o borrado).
    fn drop_local(self: &Rc<Self>, h: &MessageHeader) {
        if let Err(e) = self.with_store(|s| s.delete_uid(h.mailbox_id, h.uid)) {
            self.toast(format!("Error al actualizar la caché: {e}"));
        }
        if !h.flags.seen {
            let current = self.mailbox(h.mailbox_id).map_or(0, |m| m.unread);
            self.set_unread(h.mailbox_id, current.saturating_sub(1));
        }
        {
            let mut st = self.state.borrow_mut();
            st.headers.retain(|m| m.id != h.id);
            st.selected = None;
        }
        self.reader_stack.set_visible_child_name("empty");
        self.refresh_rows();
        self.reload_sidebar();
    }

    fn set_unread(&self, mailbox_id: MailboxId, unread: u32) {
        let result = self.with_store(|s| {
            let mut mb = s.get_mailbox(mailbox_id)?;
            mb.unread = unread;
            s.upsert_mailbox(&mb)?;
            Ok(mb)
        });
        match result {
            Ok(mb) => {
                let mut st = self.state.borrow_mut();
                if let Some(m) = st.mailboxes.iter_mut().find(|m| m.id == mailbox_id) {
                    *m = mb;
                }
            }
            Err(e) => self.toast(format!("Error al guardar el contador: {e}")),
        }
    }

    fn toast(&self, text: impl AsRef<str>) {
        self.toasts.add_toast(adw::Toast::new(text.as_ref()));
    }
}

/// Punto de entrada para la base de datos y los servicios de demostración (solo debug).
#[cfg(debug_assertions)]
pub fn demo_services() -> Result<Services, Box<dyn std::error::Error>> {
    use std::sync::Arc;

    use correo_core::{AccessToken, AccountId, Error, TokenProvider};

    struct NoTokens;

    #[async_trait::async_trait]
    impl TokenProvider for NoTokens {
        async fn access_token(&self, _account: AccountId) -> CoreResult<AccessToken> {
            Err(Error::Auth("modo demostración: sin cuentas reales".into()))
        }
    }

    let store = Store::open_in_memory()?;
    seed_demo(&store);
    Ok(Services::new(store, Arc::new(NoTokens))?)
}

/// Inserta una cuenta ficticia con dos carpetas y 30 mensajes en hilos de 3.
#[cfg(debug_assertions)]
pub fn seed_demo(store: &Store) {
    use std::time::{SystemTime, UNIX_EPOCH};

    use correo_core::{Account, Address, Flags, Mailbox, MailboxRole, MessageHeader, Provider};

    let account_id = store
        .create_account(&Account {
            id: 0,
            provider: Provider::Gmail,
            email: "demo@ejemplo.com".into(),
            display_name: "Demo".into(),
            sync_days: 30,
        })
        .expect("cuenta demo");
    let mailbox = |path: &str, role, unread| Mailbox {
        id: 0,
        account_id,
        path: path.into(),
        role,
        uid_validity: Some(1),
        unread,
    };
    let inbox = store
        .upsert_mailbox(&mailbox("INBOX", MailboxRole::Inbox, 0))
        .expect("carpeta demo");
    store
        .upsert_mailbox(&mailbox("[Gmail]/Enviados", MailboxRole::Sent, 0))
        .expect("carpeta demo");

    let people = [
        ("Ana Ruiz", "ana@ejemplo.com"),
        ("Luis Gómez", "luis@ejemplo.org"),
        ("Soporte", "soporte@ejemplo.net"),
        ("Marta Solé", "marta@ejemplo.com"),
    ];
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));

    let mut headers = Vec::new();
    let mut unread = 0;
    for i in 0..30u32 {
        let thread = i / 3;
        let root_id = format!("<demo-{}@ejemplo.com>", thread * 3);
        let mid = format!("<demo-{i}@ejemplo.com>");
        let (in_reply_to, references, subject) = if i % 3 == 0 {
            (
                None,
                Vec::new(),
                format!("Tema {}: presupuesto del trimestre", thread + 1),
            )
        } else {
            let prev = format!("<demo-{}@ejemplo.com>", i - 1);
            (
                Some(prev.clone()),
                vec![root_id.clone(), prev],
                format!("Re: Tema {}: presupuesto del trimestre", thread + 1),
            )
        };
        let (name, email) = people[(thread as usize) % people.len()];
        let seen = i % 4 != 0;
        if !seen {
            unread += 1;
        }
        headers.push(MessageHeader {
            id: 0,
            mailbox_id: inbox,
            uid: i + 1,
            message_id: Some(mid),
            in_reply_to,
            references,
            subject,
            from: vec![Address {
                name: Some(name.into()),
                email: email.into(),
            }],
            to: vec![Address {
                name: None,
                email: "demo@ejemplo.com".into(),
            }],
            cc: Vec::new(),
            date: now - i64::from(30 - i) * 7_200,
            flags: Flags {
                seen,
                ..Flags::default()
            },
            has_attachments: i % 5 == 0,
            snippet: Some(format!(
                "Hola, te envío la revisión número {i} del documento."
            )),
            labels: Vec::new(),
        });
    }
    store
        .upsert_headers(inbox, &headers)
        .expect("cabeceras demo");
    store
        .upsert_mailbox(&mailbox("INBOX", MailboxRole::Inbox, unread))
        .expect("contador demo");
}

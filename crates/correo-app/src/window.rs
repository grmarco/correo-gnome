//! T9: ventana principal. `NavigationSplitView` externo (cuentas y carpetas) que
//! contiene otro (lista de hilos | lector). Toda la E/S de red va por `Services::spawn`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use correo_core::{
    Account, AccountId, Address, BatchReport, Mailbox, MailboxId, MailboxRole, MessageHeader,
    MessageId, Result as CoreResult,
};
use correo_store::Store;
use gtk::{gio, glib};

use crate::batch::{self, Phrase};
use crate::batch_bar::BatchBar;
use crate::composer;
use crate::load_status::LoadTracker;
use crate::message_list::{thread_rows, MessageList, Selection};
use crate::notify;
use crate::reader::MessageView;
use crate::search;
use crate::services::{flatten, Services};
use crate::setup::{self, AddOutcome, MailboxSync, SyncHandle};
use crate::sidebar::{Sidebar, Target};
use crate::ui_style;
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
        let weak_status = shell.weak.clone();
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
            move |account, mailbox, state| {
                if let Some(s) = weak_status.upgrade() {
                    s.on_mailbox_status(&account, mailbox, state);
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
    /// Botón «Seleccionar» de la cabecera de la lista (casillas en cada fila).
    select_btn: gtk::ToggleButton,
    /// Barra de acciones en lote y estado del panel derecho.
    batch_bar: BatchBar,
    /// Sincronización en segundo plano; `None` en modo demostración.
    sync: RefCell<Option<SyncHandle>>,
    /// Ventana estrecha (≤ 560sp): el lector compacta su barra de acciones.
    compact: Cell<bool>,
    /// Estado de sincronización de cada carpeta (avisos del runner).
    load: RefCell<LoadTracker>,
    /// Cuentas recién añadidas cuyo primer informe de sincronización aún no ha llegado.
    fresh_accounts: RefCell<Vec<correo_core::AccountId>>,
    inner: adw::NavigationSplitView,
    outer: adw::NavigationSplitView,
    reader_stack: gtk::Stack,
    /// Cabecera del panel del lector: aloja las acciones del mensaje.
    reader_header: adw::HeaderBar,
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
                let w_remove = weak.clone();
                Sidebar::new(
                    move |t| {
                        if let Some(s) = w.upgrade() {
                            s.set_target(t);
                        }
                    },
                    move |id| {
                        if let Some(s) = w_remove.upgrade() {
                            s.confirm_remove_account(id);
                        }
                    },
                    {
                        let w = weak.clone();
                        move || {
                            if let Some(s) = w.upgrade() {
                                s.outer.set_show_content(true);
                            }
                        }
                    },
                )
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
                MessageList::new(move |sel| {
                    if let Some(s) = w.upgrade() {
                        s.on_selection(sel);
                    }
                })
            };

            let window = adw::ApplicationWindow::builder()
                .application(app)
                .title("Correo")
                .default_width(1180)
                .default_height(760)
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
                .build();
            search.add_css_class("correo-search");
            let search_clamp = adw::Clamp::builder()
                .maximum_size(340)
                .tightening_threshold(240)
                .child(&search)
                .build();
            let sync_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
            sync_btn.add_css_class("correo-sync");
            sync_btn.set_tooltip_text(Some("Sincronizar (Ctrl+R)"));
            sync_btn.update_property(&[gtk::accessible::Property::Label("Sincronizar")]);
            // Botón con icono y texto; en ventanas estrechas solo queda el icono.
            let compose_content = adw::ButtonContent::builder()
                .icon_name("mail-message-new-symbolic")
                .label("Redactar")
                .build();
            let compose_btn = gtk::Button::new();
            compose_btn.set_child(Some(&compose_content));
            compose_btn.add_css_class("suggested-action");
            compose_btn.add_css_class("correo-compose-button");
            compose_btn.set_tooltip_text(Some("Redactar un mensaje nuevo (Ctrl+N)"));
            compose_btn.update_property(&[gtk::accessible::Property::Label("Redactar")]);
            let select_btn = gtk::ToggleButton::new();
            select_btn.set_icon_name(ui_style::icon_or(
                "selection-mode-symbolic",
                "object-select-symbolic",
            ));
            select_btn.add_css_class("correo-select-toggle");
            select_btn.set_tooltip_text(Some("Seleccionar varios mensajes"));
            select_btn.update_property(&[gtk::accessible::Property::Label("Seleccionar varios")]);
            let batch_bar = BatchBar::new();
            let list_header = adw::HeaderBar::new();
            list_header.pack_start(&sync_btn);
            list_header.pack_start(&select_btn);
            list_header.pack_end(&compose_btn);
            list_header.set_title_widget(Some(&search_clamp));
            let list_toolbar = adw::ToolbarView::new();
            list_toolbar.add_top_bar(&list_header);
            // La barra de lote flota sobre el borde inferior de la lista: al aparecer no
            // redimensiona la lista (las filas no se mueven y el siguiente Ctrl+clic cae donde
            // se espera) y queda al alcance del pulgar.
            let list_overlay = gtk::Overlay::new();
            list_overlay.set_child(Some(list.widget()));
            batch_bar.widget().set_valign(gtk::Align::End);
            list_overlay.add_overlay(batch_bar.widget());
            list_toolbar.set_content(Some(&list_overlay));
            // Teclado con el foco en la lista o en la barra: Supr borra y Esc cancela la
            // selección (Ctrl+Supr, Ctrl+E y Ctrl+U son atajos de ventana).
            {
                let keys = gtk::EventControllerKey::new();
                let w = weak.clone();
                keys.connect_key_pressed(move |_, key, _, state| {
                    let Some(s) = w.upgrade() else {
                        return glib::Propagation::Proceed;
                    };
                    s.on_list_key(key, state)
                });
                list_toolbar.add_controller(keys);
            }
            {
                let w = weak.clone();
                select_btn.connect_toggled(move |b| {
                    if let Some(s) = w.upgrade() {
                        s.set_select_mode(b.is_active());
                    }
                });
            }
            {
                let w = weak.clone();
                batch_bar.popover().connect_show(move |_| {
                    if let Some(s) = w.upgrade() {
                        s.fill_move_menu();
                    }
                });
            }
            let list_page = adw::NavigationPage::new(&list_toolbar, "Mensajes");

            // Lector: placeholder hasta que se abre un mensaje.
            let reader_empty = adw::StatusPage::new();
            reader_empty.add_css_class("correo-empty");
            reader_empty.set_icon_name(Some("mail-read-symbolic"));
            reader_empty.set_title("Ningún mensaje seleccionado");
            reader_empty.set_description(Some("Elige un mensaje de la lista para leerlo."));
            let reader_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
            reader_box.set_hexpand(true);
            reader_box.set_vexpand(true);
            let reader_stack = gtk::Stack::new();
            reader_stack.add_named(&reader_empty, Some("empty"));
            reader_stack.add_named(&reader_box, Some("reader"));
            reader_stack.add_named(batch_bar.status_page(), Some("batch"));
            reader_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
            reader_stack.set_transition_duration(160);
            reader_stack.set_visible_child_name("empty");
            let reader_toolbar = adw::ToolbarView::new();
            let reader_header = adw::HeaderBar::new();
            reader_header.set_title_widget(Some(&gtk::Label::new(None)));
            reader_toolbar.add_top_bar(&reader_header);
            reader_toolbar.set_content(Some(&reader_stack));
            let reader_page = adw::NavigationPage::new(&reader_toolbar, "Mensaje");

            let inner = adw::NavigationSplitView::new();
            inner.set_min_sidebar_width(340.0);
            inner.set_max_sidebar_width(440.0);
            inner.set_sidebar_width_fraction(0.32);
            inner.set_sidebar(Some(&list_page));
            inner.set_content(Some(&reader_page));

            let outer = adw::NavigationSplitView::new();
            outer.set_min_sidebar_width(250.0);
            outer.set_max_sidebar_width(300.0);
            outer.set_sidebar_width_fraction(0.21);
            outer.set_sidebar(Some(sidebar.page()));
            let inner_page = adw::NavigationPage::new(&inner, "Correo");
            outer.set_content(Some(&inner_page));

            // Estado vacío: sin cuentas.
            let empty_header = adw::HeaderBar::new();
            let empty_status = adw::StatusPage::new();
            empty_status.add_css_class("correo-empty");
            empty_status.set_icon_name(Some("mail-unread-symbolic"));
            empty_status.set_title("Bienvenido a Correo");
            empty_status.set_description(Some(
                "Añade una cuenta de Gmail, Outlook.com o Microsoft 365 para empezar.",
            ));
            let add_btn = gtk::Button::with_label("Añadir cuenta");
            add_btn.set_halign(gtk::Align::Center);
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
            root.set_transition_type(gtk::StackTransitionType::Crossfade);
            root.set_transition_duration(200);
            root.add_named(&outer, Some("main"));
            root.add_named(&empty_toolbar, Some("empty"));

            let toasts = adw::ToastOverlay::new();
            toasts.set_child(Some(&root));
            window.set_content(Some(&toasts));

            // Puntos de corte. libadwaita aplica solo el último que cumple su condición, así
            // que cada uno repite los ajustes de los anteriores: a 1000 px se colapsa la lista;
            // a 720 px, también la barra lateral; a 560 px, además se compacta la cabecera.
            let compact_window = |max_width: &str, outer_collapsed: bool, compact: bool| {
                let bp = adw::Breakpoint::new(
                    adw::BreakpointCondition::parse(max_width).expect("condición de breakpoint"),
                );
                bp.add_setter(&inner, "collapsed", Some(&true.to_value()));
                if outer_collapsed {
                    bp.add_setter(&outer, "collapsed", Some(&true.to_value()));
                }
                if compact {
                    // El botón Redactar se reduce a su icono.
                    bp.add_setter(&compose_content, "label", Some(&"".to_value()));
                    let win = window.clone();
                    let w = weak.clone();
                    bp.connect_apply(move |_| {
                        win.add_css_class("correo-compact");
                        if let Some(s) = w.upgrade() {
                            s.set_compact(true);
                        }
                    });
                    let win = window.clone();
                    let w = weak.clone();
                    bp.connect_unapply(move |_| {
                        win.remove_css_class("correo-compact");
                        if let Some(s) = w.upgrade() {
                            s.set_compact(false);
                        }
                    });
                }
                window.add_breakpoint(bp);
            };
            compact_window("max-width: 1000sp", false, false);
            compact_window("max-width: 720sp", true, false);
            compact_window("max-width: 560sp", true, true);

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
                select_btn,
                batch_bar,
                inner,
                outer,
                reader_stack,
                reader_header,
                reader_box,
                reader: RefCell::new(None),
                sync: RefCell::new(None),
                compact: Cell::new(false),
                load: RefCell::new(LoadTracker::default()),
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

    fn set_compact(&self, compact: bool) {
        self.compact.set(compact);
        if let Some(reader) = self.reader.borrow().as_ref() {
            reader.set_compact(compact);
        }
    }

    /// Acciones de ventana y atajos de teclado.
    fn install_actions(self: &Rc<Self>) {
        self.action("compose", &["<Primary>n"], |s| s.compose(ComposeMode::New));
        self.action("sync", &["<Primary>r"], |s| s.sync());
        self.action("archive", &["<Primary>e"], |s| s.batch_archive());
        self.action("delete", &["<Primary>Delete"], |s| s.batch_delete());
        self.action("toggle-read", &["<Primary>u"], |s| s.batch_toggle_read());
        self.action("select-all", &[], |s| s.list.select_all());
        self.action("clear-selection", &[], |s| s.clear_selection());
        let move_to = gio::SimpleAction::new("move-to", Some(glib::VariantTy::INT64));
        let w = self.weak.clone();
        move_to.connect_activate(move |_, param| {
            let Some(id) = param.and_then(|p| p.get::<i64>()) else {
                return;
            };
            if let Some(s) = w.upgrade() {
                s.batch_move(id);
            }
        });
        self.window.add_action(&move_to);
        self.action("search", &["<Primary>f"], |s| {
            s.search.grab_focus();
        });
    }

    /// Acciones de aplicación: las usan las notificaciones (`app.open-message`,
    /// `app.mark-read`, `app.archive`) y funcionan aunque la ventana esté oculta.
    fn install_app_actions(self: &Rc<Self>, app: &adw::Application) {
        let about = gio::SimpleAction::new("about", None);
        let w = self.weak.clone();
        about.connect_activate(move |_, _| {
            if let Some(s) = w.upgrade() {
                let dialog = adw::AboutDialog::builder()
                    .application_name("Correo")
                    .application_icon(crate::APP_ID)
                    .developer_name("Guillermo Marco Remón")
                    .version(env!("CARGO_PKG_VERSION"))
                    .comments("Cliente de correo nativo para GNOME")
                    .build();
                dialog.present(Some(&s.window));
            }
        });
        app.add_action(&about);
        let quit = gio::SimpleAction::new("quit", None);
        let a = app.clone();
        quit.connect_activate(move |_, _| a.quit());
        app.add_action(&quit);
        app.set_accels_for_action("app.quit", &["<Primary>q"]);

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
        let w = self.weak.clone();
        self.list.connect_retry(move || {
            if let Some(s) = w.upgrade() {
                s.retry_sync();
            }
        });
        if demo_mode() {
            let ids: Vec<MailboxId> = self.state.borrow().mailboxes.iter().map(|m| m.id).collect();
            for id in ids {
                self.demo_sync(id);
            }
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
        // Una carpeta de una cuenta plegada la despliega (notificaciones, bandejas).
        if let Target::Mailbox(id) = target {
            if let Some(mailbox) = self.mailbox(id) {
                self.sidebar.reveal_mailbox(&mailbox);
            }
        }
        if self.state.borrow().target == target {
            return;
        }
        self.state.borrow_mut().target = target;
        if let Target::Mailbox(id) = target {
            self.request_mailbox_sync(id);
        }
        self.apply_target();
    }

    /// Pide al sincronizador que traiga ya una carpeta y la marca «cargando» hasta que
    /// avise (así el indicador aparece al instante, sin esperar al runner).
    fn request_mailbox_sync(self: &Rc<Self>, id: MailboxId) {
        let Some(mailbox) = self.mailbox(id) else {
            return;
        };
        let asked = self
            .sync
            .borrow()
            .as_ref()
            .is_some_and(|sync| sync.mailbox_opened(mailbox.account_id, id));
        if asked {
            self.load.borrow_mut().mark_pending(id);
        } else if demo_mode() {
            self.demo_sync(id);
        }
    }

    /// «Reintentar» tras un fallo: vuelve a pedir las carpetas de la vista.
    fn retry_sync(self: &Rc<Self>) {
        for mailbox in self.target_mailboxes() {
            self.request_mailbox_sync(mailbox.id);
        }
        self.update_load_ui();
    }

    /// Un aviso del runner sobre una carpeta: actualiza el indicador si está a la vista.
    fn on_mailbox_status(
        self: &Rc<Self>,
        _account: &Account,
        mailbox: MailboxId,
        state: MailboxSync,
    ) {
        self.load.borrow_mut().set(mailbox, state);
        if self.target_mailboxes().iter().any(|m| m.id == mailbox) {
            self.update_load_ui();
        }
    }

    /// Traslada el estado de sincronización de las carpetas a la lista y al botón de
    /// sincronizar (el icono gira mientras algo de lo que se ve está cargando).
    fn update_load_ui(&self) {
        let ids: Vec<MailboxId> = self.target_mailboxes().iter().map(|m| m.id).collect();
        let mut state = self.load.borrow().summary(&ids);
        // Cuenta recién añadida: su bandeja aún no ha informado, pero se está sincronizando.
        if state == crate::message_list::LoadState::Idle {
            let fresh = self.fresh_accounts.borrow();
            let waiting = self
                .target_mailboxes()
                .iter()
                .any(|m| m.role == MailboxRole::Inbox && fresh.contains(&m.account_id));
            if waiting {
                state = crate::message_list::LoadState::Loading;
            }
        }
        let loading = state == crate::message_list::LoadState::Loading;
        self.list.set_load_state(state);
        // Una sincronización manual en curso (botón desactivado) gestiona su propio giro.
        if self.sync_btn.is_sensitive() {
            if loading {
                self.sync_btn.add_css_class("correo-syncing");
            } else {
                self.sync_btn.remove_css_class("correo-syncing");
            }
        }
    }

    /// Simula la sincronización de una carpeta en el modo demo (`CORREO_DEMO_SYNC=loading`
    /// no termina nunca; `failed` termina con error; sin la variable no hace nada).
    fn demo_sync(self: &Rc<Self>, id: MailboxId) {
        let mode = std::env::var("CORREO_DEMO_SYNC").unwrap_or_default();
        let result = match mode.as_str() {
            "loading" => {
                self.load.borrow_mut().mark_pending(id);
                return;
            }
            "failed" => Some("No se pudo conectar con imap.ejemplo.com: tiempo de espera agotado."),
            _ => return,
        };
        self.load.borrow_mut().mark_pending(id);
        let w = self.weak.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
            if let Some(s) = w.upgrade() {
                let state = MailboxSync::Done(result.map(str::to_string));
                s.load.borrow_mut().set(id, state);
                s.update_load_ui();
            }
        });
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
        self.update_load_ui();
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
        self.list.set_empty_for_query(&self.state.borrow().query);
        let rows = thread_rows(&self.state.borrow().headers);
        self.list.set_rows(&rows);
        self.update_batch_ui();
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

    /// Cambió la selección de la lista: una fila abre el lector; varias, el estado de lote.
    fn on_selection(self: &Rc<Self>, selection: Selection) {
        self.update_batch_ui();
        match selection {
            // En el modo «Seleccionar» marcar una casilla no abre el mensaje (y, con la lista
            // a pantalla completa, no navega al lector).
            Selection::None => self.on_select(None),
            Selection::One(_) if self.list.select_mode() => self.on_select(None),
            Selection::One(h) => self.on_select(Some(*h)),
            Selection::Many(_) => {
                self.state.borrow_mut().selected = None;
                if let Some(reader) = self.reader.borrow().as_ref() {
                    reader.clear();
                }
                self.reader_stack.set_visible_child_name("batch");
            }
        }
    }

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
        view.set_compact(self.compact.get());
        self.reader_header.pack_end(view.actions());
        self.reader_box.append(view.widget());
        *self.reader.borrow_mut() = Some(view);
    }

    /// Abre el compositor. Un mensaje nuevo sale de la cuenta de la carpeta que se ve (o del
    /// mensaje seleccionado en la bandeja unificada); respuestas y reenvíos, de la cuenta que
    /// recibió el mensaje original.
    fn compose(self: &Rc<Self>, mode: ComposeMode) {
        let account = match &mode {
            ComposeMode::New => self.default_sender(),
            ComposeMode::Reply(h, _) | ComposeMode::ReplyAll(h, _) => self.reply_account(h),
            ComposeMode::Forward(h, _) => self
                .mailbox(h.mailbox_id)
                .and_then(|m| self.account(m.account_id)),
        };
        match account {
            Some(account) => composer::open(&self.app, self.services.clone(), account, mode),
            None => self.toast("Añade una cuenta antes de redactar"),
        }
    }

    /// Cuenta remitente por defecto de un mensaje nuevo.
    fn default_sender(&self) -> Option<Account> {
        let account_of = |mailbox_id: MailboxId| self.mailbox(mailbox_id).map(|m| m.account_id);
        let from_view = match self.state.borrow().target {
            Target::Mailbox(id) => account_of(id),
            Target::Unified => None,
        };
        let from_selection = self
            .state
            .borrow()
            .selected
            .as_ref()
            .and_then(|h| account_of(h.mailbox_id));
        from_view
            .or(from_selection)
            .and_then(|id| self.account(id))
            .or_else(|| self.state.borrow().accounts.first().cloned())
    }

    /// Cuenta desde la que responder: la que recibió el mensaje. Si esa cuenta no aparece en
    /// De/Para/Cc y otra cuenta sí (p. ej. un mensaje guardado en una carpeta compartida),
    /// se usa esa otra.
    fn reply_account(&self, h: &MessageHeader) -> Option<Account> {
        let owner = self
            .mailbox(h.mailbox_id)
            .and_then(|m| self.account(m.account_id));
        let st = self.state.borrow();
        let addressed = |account: &Account, list: &[Address]| {
            list.iter()
                .any(|a| a.email.eq_ignore_ascii_case(&account.email))
        };
        if let Some(owner) = &owner {
            let mentioned = [&h.from, &h.to, &h.cc]
                .iter()
                .any(|list| addressed(owner, list));
            if mentioned {
                return Some(owner.clone());
            }
        }
        st.accounts
            .iter()
            .find(|a| addressed(a, &h.to) || addressed(a, &h.cc))
            .cloned()
            .or(owner)
    }

    // ----- Cuentas y avisos del sincronizador -------------------------------

    /// Abre el alta de cuenta; al terminar, la añade a la barra y la sincroniza.
    fn add_account(self: &Rc<Self>) {
        let w = self.weak.clone();
        setup::add_account_dialog(
            &self.window,
            self.services.clone(),
            move |account, outcome| {
                if let Some(s) = w.upgrade() {
                    s.on_account_added(account, outcome);
                }
            },
        );
    }

    fn on_account_added(self: &Rc<Self>, account: Account, outcome: AddOutcome) {
        if let Err(e) = self.reload_model() {
            self.toast(format!("No se pudo leer la base de datos: {e}"));
        }
        self.root.set_visible_child_name("main");
        self.apply_target();

        let message = match outcome {
            AddOutcome::Added => {
                self.fresh_accounts.borrow_mut().push(account.id);
                format!("Cuenta añadida: {}", account.email)
            }
            AddOutcome::Reauthorized => {
                // La conexión IMAP pudo abrirse con el token anterior: se descarta para que
                // la siguiente operación use el nuevo.
                self.services.forget_backend(account.id);
                format!("Cuenta reautorizada: {}", account.email)
            }
        };
        // La bandeja de la cuenta nueva se está sincronizando: lo indica la lista.
        self.update_load_ui();
        if let Some(sync) = self.sync.borrow().as_ref() {
            sync.account_reauthorized(account.clone());
        }
        self.sync_contacts(&account);
        self.toast(message);
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
        let account_id = account.id;
        let job = flatten(self.services.spawn(async move {
            correo_contacts::sync_into_store(account_id, source.as_ref(), &store).await
        }));
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
        let several = self.state.borrow().accounts.len() > 1;
        notify::notify_new_if_inactive(&self.app, &account, &headers, self.is_in_front(), several);

        // Un informe llega siempre que la sincronización cambió algo, aunque no haya ids
        // nuevos (p. ej. la primera sincronización de una cuenta): refresca barra y lista.
        self.fresh_accounts
            .borrow_mut()
            .retain(|id| *id != account.id);
        if let Err(e) = self.reload_model() {
            self.toast(format!("Error al leer la caché: {e}"));
            return;
        }
        self.reload_sidebar();
        self.update_load_ui();
        // Se relee la lista si muestra alguna carpeta de la cuenta: la bandeja (informe de
        // mensajes nuevos) o una carpeta secundaria que acaba de sincronizarse al abrirla.
        let shows_account = self
            .target_mailboxes()
            .iter()
            .any(|m| m.account_id == account.id);
        if shows_account {
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

    // ----- Eliminar cuenta -------------------------------------------------

    /// Pide confirmación antes de eliminar una cuenta.
    fn confirm_remove_account(self: &Rc<Self>, account_id: AccountId) {
        let Some(account) = self.account(account_id) else {
            return;
        };
        let body = format!(
            "Se borrarán de este equipo los mensajes guardados, los contactos y las credenciales \
             de {}. El correo que está en el servidor no se modifica.",
            account.email
        );
        let dialog = adw::AlertDialog::new(Some("¿Eliminar la cuenta?"), Some(&body));
        dialog.add_response("cancel", "Cancelar");
        dialog.add_response("delete", "Eliminar");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let w = self.weak.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            if let Some(s) = w.upgrade() {
                s.remove_account(account_id);
            }
        });
        dialog.present(Some(&self.window));
    }

    /// Elimina una cuenta: para su sincronización, borra la caché, los contactos y los
    /// tokens, y recarga la barra y la lista.
    fn remove_account(self: &Rc<Self>, account_id: AccountId) {
        let Some(account) = self.account(account_id) else {
            return;
        };
        if let Some(sync) = self.sync.borrow().as_ref() {
            sync.stop_account(account_id);
        }
        if let Err(e) = self.services.remove_account(account_id) {
            self.toast(format!("No se pudo eliminar la cuenta: {e}"));
            return;
        }
        self.fresh_accounts
            .borrow_mut()
            .retain(|id| *id != account_id);

        // ¿La vista o el mensaje abierto pertenecían a la cuenta? Se comprueba con el modelo
        // anterior, que aún contiene sus carpetas.
        let (view_in_account, selected_in_account) = {
            let st = self.state.borrow();
            let in_account = |mailbox_id: MailboxId| {
                st.mailboxes
                    .iter()
                    .any(|m| m.id == mailbox_id && m.account_id == account_id)
            };
            let view = match st.target {
                Target::Mailbox(id) => in_account(id),
                Target::Unified => false,
            };
            let selected = st
                .selected
                .as_ref()
                .is_some_and(|h| in_account(h.mailbox_id));
            (view, selected)
        };
        if selected_in_account {
            self.state.borrow_mut().selected = None;
            if let Some(reader) = self.reader.borrow().as_ref() {
                reader.clear();
            }
            self.reader_stack.set_visible_child_name("empty");
        }
        if let Err(e) = self.reload_model() {
            self.toast(format!("Error al leer la caché: {e}"));
        }
        if self.state.borrow().accounts.is_empty() {
            self.root.set_visible_child_name("empty");
        } else if view_in_account {
            // La carpeta que se veía ya no existe: se vuelve a la bandeja unificada.
            self.state.borrow_mut().target = Target::Unified;
            self.apply_target();
        } else {
            self.reload_sidebar();
            self.refresh_list();
        }
        self.toast(format!("Cuenta eliminada: {}", account.email));
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
                flatten(
                    self.services
                        .spawn(async move { backend.sync_mailbox(&mailbox, days).await }),
                )
            })
            .collect();

        self.sync_btn.set_sensitive(false);
        self.sync_btn.add_css_class("correo-syncing");
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
            s.sync_btn.remove_css_class("correo-syncing");
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

    fn set_seen_remote(
        self: &Rc<Self>,
        account: &Account,
        mailbox: Mailbox,
        h: MessageHeader,
        seen: bool,
    ) {
        let backend = self.services.backend(account);
        let uid = h.uid;
        let fut = flatten(
            self.services
                .spawn(async move { backend.set_seen(&mailbox, uid, seen).await }),
        );
        self.run(fut, move |s, ()| s.apply_seen(&h, seen));
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
        let fut = flatten(
            self.services
                .spawn(async move { backend.move_to(&mailbox, uid, &target).await }),
        );
        self.run(fut, move |s, ()| s.drop_local(&h));
    }

    // ----- Selección múltiple y acciones en lote ---------------------------------

    fn clear_selection(&self) {
        if self.list.selection_len() > 0 {
            self.list.unselect_all();
        } else if self.list.select_mode() {
            self.set_select_mode(false);
        }
    }

    /// Activa o desactiva el modo «Seleccionar» (casillas en cada fila).
    fn set_select_mode(&self, on: bool) {
        self.list.set_select_mode(on);
        // Con una sola fila seleccionada se conserva (el mensaje sigue abierto).
        if !on && self.list.selection_len() >= 2 {
            self.list.unselect_all();
        }
        if self.select_btn.is_active() != on {
            self.select_btn.set_active(on);
        }
        self.select_btn.set_tooltip_text(Some(if on {
            "Terminar la selección"
        } else {
            "Seleccionar varios mensajes"
        }));
        self.update_batch_ui();
    }

    /// Supr borra la selección y Esc la cancela, salvo que se esté escribiendo en un campo.
    fn on_list_key(
        self: &Rc<Self>,
        key: gtk::gdk::Key,
        state: gtk::gdk::ModifierType,
    ) -> glib::Propagation {
        use gtk::gdk::{Key, ModifierType};
        if GtkWindowExt::focus(&self.window).is_some_and(|w| w.is::<gtk::Editable>()) {
            return glib::Propagation::Proceed;
        }
        let plain = !state.intersects(
            ModifierType::CONTROL_MASK | ModifierType::ALT_MASK | ModifierType::SHIFT_MASK,
        );
        let busy = self.list.selection_len() > 0;
        match key {
            Key::Escape if busy || self.list.select_mode() => {
                self.clear_selection();
                glib::Propagation::Stop
            }
            Key::Delete | Key::KP_Delete if plain && busy => {
                self.batch_delete();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    }

    /// Refleja la selección en la barra de lote y en el panel derecho.
    fn update_batch_ui(&self) {
        let n = self.list.selection_len();
        let mode = self.list.select_mode();
        let (any_unread, accounts) = if n == 0 {
            (true, HashSet::new())
        } else {
            let account_of: HashMap<MailboxId, AccountId> = self
                .state
                .borrow()
                .mailboxes
                .iter()
                .map(|m| (m.id, m.account_id))
                .collect();
            let rows = self.list.selected_rows();
            (
                rows.iter().any(|r| r.unread > 0),
                rows.iter()
                    .filter_map(|r| account_of.get(&r.last.mailbox_id).copied())
                    .collect::<HashSet<_>>(),
            )
        };
        let blocked = (accounts.len() > 1).then_some(
            "La selección mezcla mensajes de varias cuentas: no se pueden mover a una carpeta",
        );
        let show = n >= 2 || mode;
        self.batch_bar.update(n, show, any_unread, blocked);
        // Hueco al final de la lista para que la barra flotante no tape las últimas filas.
        self.list.set_bottom_inset(show);
    }

    /// Mensajes de la selección: todos los de cada hilo seleccionado que hay en la lista.
    fn selection_headers(&self) -> Vec<MessageHeader> {
        let selected = self.list.selected_rows();
        if selected.is_empty() {
            return Vec::new();
        }
        let st = self.state.borrow();
        let by_id: HashMap<MessageId, &MessageHeader> =
            st.headers.iter().map(|h| (h.id, h)).collect();
        batch::selected_messages(&selected, &[(0, selected.len() as u32)], &by_id)
    }

    fn batch_archive(self: &Rc<Self>) {
        self.start_batch(BatchOp::Archive);
    }

    fn batch_delete(self: &Rc<Self>) {
        self.start_batch(BatchOp::Delete);
    }

    fn batch_toggle_read(self: &Rc<Self>) {
        self.start_batch(BatchOp::ToggleRead);
    }

    fn batch_move(self: &Rc<Self>, target: MailboxId) {
        self.start_batch(BatchOp::Move(target));
    }

    /// Resuelve la selección a grupos (cuenta, carpeta) y los ejecuta; pide confirmación
    /// antes de un borrado definitivo grande o desde la Papelera.
    fn start_batch(self: &Rc<Self>, op: BatchOp) {
        let headers = self.selection_headers();
        if headers.is_empty() {
            return;
        }
        let op = match op {
            BatchOp::ToggleRead => BatchOp::Seen(batch::target_seen(&headers)),
            other => other,
        };
        let plan = self.plan_batch(op, headers);
        for problem in &plan.problems {
            self.toast(problem);
        }
        if plan.groups.is_empty() {
            return;
        }
        let permanent: usize = plan
            .groups
            .iter()
            .filter(|g| matches!(g.dest, Dest::Expunge))
            .map(|g| g.headers.len())
            .sum();
        let from_trash = plan
            .groups
            .iter()
            .any(|g| matches!(g.dest, Dest::Expunge) && g.mailbox.role == MailboxRole::Trash);
        if !batch::needs_delete_confirmation(permanent, from_trash) {
            self.execute_batch(plan);
            return;
        }
        let body = format!(
            "Se borrarán {permanent} mensajes de forma permanente. Esta acción no se puede deshacer."
        );
        let dialog = adw::AlertDialog::new(Some("¿Borrar definitivamente?"), Some(&body));
        dialog.add_response("cancel", "Cancelar");
        dialog.add_response("delete", "Borrar");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let w = self.weak.clone();
        let plan = RefCell::new(Some(plan));
        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            if let (Some(s), Some(plan)) = (w.upgrade(), plan.borrow_mut().take()) {
                s.execute_batch(plan);
            }
        });
        dialog.present(Some(&self.window));
    }

    fn plan_batch(&self, op: BatchOp, headers: Vec<MessageHeader>) -> Plan {
        let (mailboxes, accounts) = {
            let st = self.state.borrow();
            (st.mailboxes.clone(), st.accounts.clone())
        };
        let mut groups = Vec::new();
        let mut problems = Vec::new();
        for (mailbox_id, mut hs) in batch::group_by_mailbox(headers) {
            let Some(mailbox) = mailboxes.iter().find(|m| m.id == mailbox_id).cloned() else {
                continue;
            };
            let Some(account) = accounts
                .iter()
                .find(|a| a.id == mailbox.account_id)
                .cloned()
            else {
                continue;
            };
            let with_role = |role: MailboxRole| {
                mailboxes
                    .iter()
                    .find(|m| m.account_id == account.id && m.role == role)
                    .cloned()
            };
            let dest = match op {
                BatchOp::Seen(seen) => {
                    hs.retain(|h| h.flags.seen != seen);
                    Dest::Seen(seen)
                }
                BatchOp::ToggleRead => continue,
                BatchOp::Archive => match with_role(MailboxRole::Archive) {
                    Some(t) if t.id == mailbox.id => continue,
                    Some(t) => Dest::Move(t),
                    None => {
                        problems.push(format!("{} no tiene carpeta de archivo", account.email));
                        continue;
                    }
                },
                BatchOp::Delete => match with_role(MailboxRole::Trash) {
                    Some(t) if mailbox.role != MailboxRole::Trash && t.id != mailbox.id => {
                        Dest::Move(t)
                    }
                    _ => Dest::Expunge,
                },
                BatchOp::Move(target) => match mailboxes.iter().find(|m| m.id == target) {
                    Some(t) if t.account_id == account.id => {
                        if t.id == mailbox.id {
                            continue;
                        }
                        Dest::Move(t.clone())
                    }
                    _ => {
                        problems.push("No se puede mover mensajes entre cuentas".to_string());
                        continue;
                    }
                },
            };
            if hs.is_empty() {
                continue;
            }
            groups.push(Group {
                account,
                mailbox,
                headers: hs,
                dest,
            });
        }
        Plan {
            op,
            groups,
            problems,
        }
    }

    /// Aplica el lote: primero la lista y la caché en pantalla (optimista), luego un trabajo
    /// por (cuenta, carpeta) en tokio; la interfaz solo vuelve a tocarse al final.
    fn execute_batch(self: &Rc<Self>, plan: Plan) {
        let Plan { op, groups, .. } = plan;
        let removal = !matches!(op, BatchOp::Seen(_));
        // Fila a la que ir tras quitar la selección.
        let one_selected = self.list.selection_len() == 1;
        let next = if removal {
            let positions = self.list.selected_positions();
            batch::next_position(&positions, self.list.row_count() as u32)
                .and_then(|p| self.list.row_id_at(p))
        } else {
            None
        };

        let jobs: Vec<_> = groups.iter().map(|g| self.spawn_group(g)).collect();
        for g in &groups {
            self.apply_local(g.mailbox.id, &g.headers, g.local(), false);
        }
        self.refresh_rows();
        self.reload_sidebar();
        if let Some(id) = next {
            if self.list.selection_len() == 0 {
                if one_selected {
                    self.list.select_by_id(id);
                } else {
                    self.list.focus_id(id);
                }
            }
        }

        let w = self.weak.clone();
        glib::MainContext::default().spawn_local(async move {
            let mut reports = Vec::new();
            for job in jobs {
                reports.push(job.await);
            }
            if let Some(s) = w.upgrade() {
                s.finish_batch(op, groups, reports);
            }
        });
    }

    /// Lanza en tokio el trabajo de un grupo: cambia la caché, ejecuta el lote en el servidor
    /// y, si algo falla, devuelve a la caché lo que no se pudo aplicar.
    fn spawn_group(&self, g: &Group) -> impl Future<Output = BatchReport> + 'static {
        let backend = self.services.backend(&g.account);
        let store = self.services.store.clone();
        let (mailbox, headers, dest) = (g.mailbox.clone(), g.headers.clone(), g.dest.clone());
        let uids = batch::uids(&headers);
        let job = self.services.spawn({
            let uids = uids.clone();
            async move {
                cache_apply(&store, &mailbox, &headers, &dest);
                let report = match &dest {
                    Dest::Seen(seen) => backend.set_seen_many(&mailbox, &uids, *seen).await,
                    Dest::Move(target) => backend.move_many(&mailbox, &uids, target).await,
                    Dest::Expunge => backend.delete_many(&mailbox, &uids).await,
                };
                if !report.failed.is_empty() {
                    cache_revert(&store, &mailbox, &headers, &dest, &report.failed);
                }
                report
            }
        });
        async move {
            job.await
                .unwrap_or_else(|e| BatchReport::all_failed(&uids, &e))
        }
    }

    /// Ajusta la lista en memoria y el contador de no leídos. Con `revert` solo deshace el
    /// contador: la lista se relee de la caché, que ya recuperó lo que falló.
    fn apply_local(
        &self,
        mailbox_id: MailboxId,
        headers: &[MessageHeader],
        kind: Local,
        revert: bool,
    ) {
        let delta: i64 = match kind {
            Local::Seen(seen) => {
                let changed = headers.iter().filter(|h| h.flags.seen != seen).count() as i64;
                if !revert {
                    let ids: HashSet<MessageId> = headers.iter().map(|h| h.id).collect();
                    let mut st = self.state.borrow_mut();
                    for m in st.headers.iter_mut().filter(|m| ids.contains(&m.id)) {
                        m.flags.seen = seen;
                    }
                    if let Some(sel) = st.selected.as_mut().filter(|m| ids.contains(&m.id)) {
                        sel.flags.seen = seen;
                    }
                }
                if seen {
                    -changed
                } else {
                    changed
                }
            }
            Local::Remove => {
                if !revert {
                    let ids: HashSet<MessageId> = headers.iter().map(|h| h.id).collect();
                    self.state
                        .borrow_mut()
                        .headers
                        .retain(|m| !ids.contains(&m.id));
                }
                -(headers.iter().filter(|h| !h.flags.seen).count() as i64)
            }
        };
        self.adjust_unread(mailbox_id, if revert { -delta } else { delta });
    }

    fn adjust_unread(&self, mailbox_id: MailboxId, delta: i64) {
        if delta == 0 {
            return;
        }
        let current = self.mailbox(mailbox_id).map_or(0, |m| i64::from(m.unread));
        let unread = u32::try_from((current + delta).max(0)).unwrap_or(u32::MAX);
        self.set_unread(mailbox_id, unread);
    }

    /// Resultado del lote: revierte lo que falló, resume con un aviso y ofrece deshacer.
    fn finish_batch(self: &Rc<Self>, op: BatchOp, groups: Vec<Group>, reports: Vec<BatchReport>) {
        let (mut ok, mut total) = (0usize, 0usize);
        let mut error: Option<String> = None;
        let mut any_failed = false;
        let mut undo: Vec<UndoItem> = Vec::new();
        for (g, report) in groups.iter().zip(&reports) {
            let failed: HashSet<u32> = report.failed.iter().copied().collect();
            let (bad, good): (Vec<&MessageHeader>, Vec<&MessageHeader>) =
                g.headers.iter().partition(|h| failed.contains(&h.uid));
            total += g.headers.len();
            ok += good.len();
            if error.is_none() {
                error.clone_from(&report.error);
            }
            if !bad.is_empty() {
                any_failed = true;
                let bad: Vec<MessageHeader> = bad.into_iter().cloned().collect();
                self.apply_local(g.mailbox.id, &bad, g.local(), true);
            }
            if let Dest::Move(to) = &g.dest {
                let message_ids: HashSet<String> =
                    good.iter().filter_map(|h| h.message_id.clone()).collect();
                if !message_ids.is_empty() {
                    undo.push(UndoItem {
                        account: g.account.clone(),
                        from: g.mailbox.clone(),
                        to: to.clone(),
                        message_ids,
                    });
                }
            }
        }
        if any_failed {
            // La caché ya recuperó lo que falló: se relee todo de ella.
            if let Err(e) = self.reload_model() {
                self.toast(format!("Error al leer la caché: {e}"));
            }
            self.refresh_list();
            self.reload_sidebar();
        }

        let seen_op = matches!(op, BatchOp::Seen(_));
        if seen_op && ok == total {
            return;
        }
        let phrase = phrase_for(op, &groups, |id| {
            self.mailbox(id)
                .map(|m| folder_label(&m))
                .unwrap_or_default()
        });
        let text = batch::summary(&phrase, ok, total, error.as_deref());
        let undoable =
            ok > 0 && !undo.is_empty() && matches!(op, BatchOp::Archive | BatchOp::Delete);
        if !undoable {
            self.toast(text);
            return;
        }
        let toast = adw::Toast::new(&text);
        toast.set_button_label(Some("Deshacer"));
        toast.set_timeout(10);
        let undo = Rc::new(RefCell::new(Some(undo)));
        let w = self.weak.clone();
        toast.connect_button_clicked(move |_| {
            if let (Some(s), Some(items)) = (w.upgrade(), undo.borrow_mut().take()) {
                s.undo_batch(items);
            }
        });
        self.toasts.add_toast(toast);
    }

    /// Deshace un archivado o un borrado a la papelera: sincroniza la carpeta de destino para
    /// ver los mensajes (su UID cambió al moverlos), los localiza por `Message-ID` y los
    /// devuelve a su carpeta de origen.
    fn undo_batch(self: &Rc<Self>, items: Vec<UndoItem>) {
        let jobs: Vec<_> = items
            .into_iter()
            .map(|it| {
                let backend = self.services.backend(&it.account);
                let store = self.services.store.clone();
                let days = it.account.sync_days;
                flatten(self.services.spawn(async move {
                    backend.sync_mailbox(&it.to, days).await?;
                    let uids = find_moved(&store, &it.to, &it.message_ids)?;
                    if uids.is_empty() {
                        return Err(correo_core::Error::NotFound(
                            "los mensajes ya no están en la carpeta de destino".into(),
                        ));
                    }
                    let report = backend.move_many(&it.to, &uids, &it.from).await;
                    // Trae de vuelta a la caché de origen lo que acaba de regresar.
                    let _ = backend.sync_mailbox(&it.from, days).await;
                    Ok(report)
                }))
            })
            .collect();
        let w = self.weak.clone();
        glib::MainContext::default().spawn_local(async move {
            let mut restored = 0usize;
            let mut failed = 0usize;
            let mut error: Option<String> = None;
            for job in jobs {
                match job.await {
                    Ok(report) => {
                        restored += report.done.len();
                        failed += report.failed.len();
                        if error.is_none() {
                            error = report.error;
                        }
                    }
                    Err(e) => {
                        failed += 1;
                        error.get_or_insert(e.to_string());
                    }
                }
            }
            let Some(s) = w.upgrade() else { return };
            if let Err(e) = s.reload_model() {
                s.toast(format!("Error al leer la caché: {e}"));
            }
            s.reload_sidebar();
            s.refresh_list();
            let text = match (restored, failed) {
                (0, _) => format!(
                    "No se pudo deshacer: {}",
                    error.unwrap_or_else(|| "no se encontraron los mensajes".into())
                ),
                (n, 0) => format!(
                    "Se {} {n} {}",
                    if n == 1 { "restauró" } else { "restauraron" },
                    if n == 1 { "mensaje" } else { "mensajes" }
                ),
                (n, f) => format!(
                    "Se restauraron {n}; {f} no se pudieron deshacer: {}",
                    error.unwrap_or_default()
                ),
            };
            s.toast(text);
        });
    }

    /// Rellena el popover «Mover a…» con las carpetas de la cuenta de la selección.
    fn fill_move_menu(self: &Rc<Self>) {
        let holder = self.batch_bar.move_list();
        while let Some(child) = holder.first_child() {
            holder.remove(&child);
        }
        let rows = self.list.selected_rows();
        let (boxes, sources) = {
            let st = self.state.borrow();
            let sources: HashSet<MailboxId> = rows.iter().map(|r| r.last.mailbox_id).collect();
            let account = sources
                .iter()
                .find_map(|id| st.mailboxes.iter().find(|m| m.id == *id))
                .map(|m| m.account_id);
            let boxes: Vec<Mailbox> = st
                .mailboxes
                .iter()
                .filter(|m| Some(m.account_id) == account)
                .cloned()
                .collect();
            (boxes, sources)
        };
        if boxes.is_empty() {
            let empty = gtk::Label::new(Some("No hay carpetas"));
            empty.add_css_class("dim-label");
            holder.append(&empty);
            return;
        }
        let popover = self.batch_bar.popover().clone();
        add_move_nodes(
            holder,
            &crate::folder_tree::folder_tree(&boxes),
            0,
            &sources,
            &popover,
        );
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

/// Acción en lote pedida por el usuario.
#[derive(Clone, Copy)]
enum BatchOp {
    Archive,
    Delete,
    ToggleRead,
    /// Marcar como leído (`true`) o no leído (`false`).
    Seen(bool),
    Move(MailboxId),
}

/// Qué se hace con los mensajes de un grupo.
#[derive(Clone)]
enum Dest {
    Seen(bool),
    Move(Mailbox),
    /// Borrado definitivo (carpeta Papelera o cuenta sin ella): `UID EXPUNGE`.
    Expunge,
}

/// Cambio de la lista en memoria que implica una acción.
#[derive(Clone, Copy)]
enum Local {
    Seen(bool),
    Remove,
}

/// Mensajes de una misma (cuenta, carpeta) sobre los que se aplica la acción.
struct Group {
    account: Account,
    mailbox: Mailbox,
    headers: Vec<MessageHeader>,
    dest: Dest,
}

impl Group {
    fn local(&self) -> Local {
        match self.dest {
            Dest::Seen(seen) => Local::Seen(seen),
            Dest::Move(_) | Dest::Expunge => Local::Remove,
        }
    }
}

struct Plan {
    op: BatchOp,
    groups: Vec<Group>,
    /// Avisos de lo que no se puede hacer (cuentas sin carpeta de archivo, etc.).
    problems: Vec<String>,
}

/// Datos para deshacer un movimiento: los mensajes están ahora en `to`.
struct UndoItem {
    account: Account,
    from: Mailbox,
    to: Mailbox,
    message_ids: HashSet<String>,
}

/// Aplica el cambio a la caché local, por tandas para no retener el almacén mucho tiempo.
fn cache_apply(
    store: &std::sync::Mutex<Store>,
    mailbox: &Mailbox,
    headers: &[MessageHeader],
    dest: &Dest,
) {
    for chunk in headers.chunks(100) {
        let Ok(store) = store.lock() else { return };
        for h in chunk {
            let _ = match dest {
                Dest::Seen(seen) => {
                    let mut flags = h.flags.clone();
                    flags.seen = *seen;
                    store.set_flags(mailbox.id, h.uid, &flags)
                }
                Dest::Move(_) | Dest::Expunge => store.delete_uid(mailbox.id, h.uid),
            };
        }
    }
}

/// Devuelve a la caché los mensajes de `failed` (UIDs) que el servidor no aplicó.
fn cache_revert(
    store: &std::sync::Mutex<Store>,
    mailbox: &Mailbox,
    headers: &[MessageHeader],
    dest: &Dest,
    failed: &[u32],
) {
    let failed: HashSet<u32> = failed.iter().copied().collect();
    let bad: Vec<MessageHeader> = headers
        .iter()
        .filter(|h| failed.contains(&h.uid))
        .cloned()
        .collect();
    let Ok(store) = store.lock() else { return };
    match dest {
        Dest::Seen(_) => {
            for h in &bad {
                let _ = store.set_flags(mailbox.id, h.uid, &h.flags);
            }
        }
        Dest::Move(_) | Dest::Expunge => {
            let _ = store.upsert_headers(mailbox.id, &bad);
        }
    }
}

/// UIDs, en la carpeta `to`, de los mensajes con esos `Message-ID` (recorre la caché por
/// páginas hasta encontrarlos todos o agotarla).
fn find_moved(
    store: &std::sync::Mutex<Store>,
    to: &Mailbox,
    wanted: &HashSet<String>,
) -> CoreResult<Vec<u32>> {
    const PAGE: usize = 500;
    const MAX_SCAN: usize = 50_000;
    let mut found = Vec::new();
    let mut offset = 0;
    while offset < MAX_SCAN && found.len() < wanted.len() {
        let page = {
            let store = store.lock().unwrap_or_else(|p| p.into_inner());
            store.list_headers(to.id, PAGE, offset)?
        };
        found.extend(batch::pick_uids(&page, wanted));
        if page.len() < PAGE {
            break;
        }
        offset += PAGE;
    }
    Ok(found)
}

/// Frase del resultado de una acción para el aviso.
fn phrase_for(op: BatchOp, groups: &[Group], label_of: impl Fn(MailboxId) -> String) -> Phrase {
    let (singular, plural, suffix) = match op {
        BatchOp::Archive => ("archivó", "archivaron", String::new()),
        BatchOp::Delete => {
            if groups.iter().all(|g| matches!(g.dest, Dest::Expunge)) {
                ("borró", "borraron", String::new())
            } else {
                ("movió", "movieron", " a la papelera".to_string())
            }
        }
        BatchOp::Move(target) => ("movió", "movieron", format!(" a «{}»", label_of(target))),
        BatchOp::Seen(true) => ("marcó como leído", "marcaron como leídos", String::new()),
        BatchOp::Seen(false) | BatchOp::ToggleRead => (
            "marcó como no leído",
            "marcaron como no leídos",
            String::new(),
        ),
    };
    Phrase {
        singular,
        plural,
        suffix,
    }
}

/// Nombre visible de una carpeta en avisos.
fn folder_label(m: &Mailbox) -> String {
    crate::folder_tree::role_name(m.role).map_or_else(
        || {
            let sep = m.delimiter.unwrap_or('/');
            m.path.rsplit(sep).next().unwrap_or(&m.path).to_string()
        },
        str::to_string,
    )
}

/// Añade al popover «Mover a…» los nodos del árbol de carpetas, con sangría por nivel.
fn add_move_nodes(
    holder: &gtk::Box,
    nodes: &[crate::folder_tree::Node],
    depth: i32,
    sources: &HashSet<MailboxId>,
    popover: &gtk::Popover,
) {
    for node in nodes {
        let label = gtk::Label::new(Some(&node.name));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Fill);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_margin_start(6 + 14 * depth);
        match node.mailbox {
            Some(id) => {
                let button = gtk::Button::new();
                button.add_css_class("flat");
                button.set_child(Some(&label));
                button.set_action_name(Some("win.move-to"));
                button.set_action_target_value(Some(&id.to_variant()));
                button.set_sensitive(!sources.contains(&id));
                let popover = popover.clone();
                button.connect_clicked(move |_| popover.popdown());
                holder.append(&button);
            }
            None => {
                label.add_css_class("dim-label");
                label.add_css_class("caption");
                label.set_margin_top(4);
                holder.append(&label);
            }
        }
        add_move_nodes(holder, &node.children, depth + 1, sources, popover);
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
    // `CORREO_DEMO_EMPTY`: base vacía, para ver el estado «sin cuentas».
    if std::env::var_os("CORREO_DEMO_EMPTY").is_none() {
        seed_demo(&store);
    }
    Ok(Services::new(store, Arc::new(NoTokens))?
        .with_backend_override(crate::demo_backend::DemoBackend::shared()))
}

/// Inserta cuatro cuentas ficticias (dos Gmail y dos Outlook / Microsoft 365), cada una con
/// Entrada y Enviados y 12 mensajes en hilos de 3. Dos cuentas comparten nombre visible para
/// comprobar que la barra lateral, el selector «De» y las notificaciones distinguen por email.
#[cfg(debug_assertions)]
pub fn seed_demo(store: &Store) {
    use std::time::{SystemTime, UNIX_EPOCH};

    use correo_core::{Account, Address, Flags, Mailbox, MailboxRole, MessageHeader, Provider};

    let accounts = [
        (
            Provider::Gmail,
            "demo.uno@gmail.com",
            "Demo",
            "INBOX",
            "[Gmail]/Enviados",
        ),
        (
            Provider::Gmail,
            "demo.dos@gmail.com",
            "Demo",
            "INBOX",
            "[Gmail]/Enviados",
        ),
        (
            Provider::Outlook,
            "demo.outlook@outlook.com",
            "Demo Outlook",
            "INBOX",
            "Elementos enviados",
        ),
        (
            Provider::Microsoft365,
            "demo.trabajo@empresa.example",
            "Demo Trabajo",
            "INBOX",
            "Elementos enviados",
        ),
    ];
    let people = [
        ("Ana Ruiz", "ana@ejemplo.com"),
        ("Luis Gómez", "luis@ejemplo.org"),
        ("Soporte técnico", "soporte@ejemplo.net"),
        ("Marta Solé", "marta@ejemplo.com"),
        ("Javier Ortega", "javier@ejemplo.es"),
        ("Carmen Vidal", "carmen@ejemplo.org"),
        ("Equipo de Facturación", "facturas@ejemplo.net"),
        ("Pablo", "pablo@ejemplo.com"),
    ];
    // Un asunto, un extracto y una antigüedad (en horas) por hilo, para tener variedad.
    let topics = [
        (
            "Presupuesto del trimestre",
            "Hola, te envío la revisión del documento con las cifras actualizadas.",
            0.4,
        ),
        (
            "Reunión del jueves: orden del día",
            "Os dejo los puntos que quiero tratar; avisadme si falta algo importante.",
            5.0,
        ),
        (
            "Tu factura de octubre ya está disponible",
            "Adjuntamos el PDF con el detalle de tu consumo del último periodo.",
            30.0,
        ),
        (
            "Fotos del fin de semana",
            "Por fin he conseguido pasar las fotos, ¡hay alguna que te va a encantar!",
            100.0,
        ),
    ];
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));

    for (index, (provider, email, display_name, inbox_path, sent_path)) in
        accounts.into_iter().enumerate()
    {
        let account_id = store
            .create_account(&Account {
                id: 0,
                provider,
                email: email.into(),
                display_name: display_name.into(),
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
            // Gmail y Outlook usan «/»; Microsoft 365 usa «.» para probar ambos.
            delimiter: Some(if provider == Provider::Microsoft365 {
                '.'
            } else {
                '/'
            }),
        };
        let inbox = store
            .upsert_mailbox(&mailbox(inbox_path, MailboxRole::Inbox, 0))
            .expect("carpeta demo");
        store
            .upsert_mailbox(&mailbox(sent_path, MailboxRole::Sent, 0))
            .expect("carpeta demo");
        // Jerarquía de carpetas (sin mensajes) para probar el árbol de la barra lateral.
        let extra: Vec<(String, MailboxRole, u32)> = match index {
            0 => [
                ("[Gmail]/Borradores", MailboxRole::Drafts, 1),
                ("[Gmail]/Spam", MailboxRole::Spam, 2),
                ("[Gmail]/Papelera", MailboxRole::Trash, 0),
                ("[Gmail]/Todos", MailboxRole::Archive, 0),
                ("[Gmail]/Destacados", MailboxRole::Other, 0),
                ("Proyectos", MailboxRole::Other, 1),
                ("Proyectos/Casa", MailboxRole::Other, 0),
                ("Proyectos/Casa/Obra", MailboxRole::Other, 3),
                ("Proyectos/Casa/Obra/Facturas", MailboxRole::Other, 0),
                ("Proyectos/Viaje", MailboxRole::Other, 0),
            ]
            .into_iter()
            .map(|(p, r, u)| (p.to_string(), r, u))
            .collect(),
            2 => std::iter::once(("Archives".to_string(), MailboxRole::Archive, 0))
                .chain((2014..=2023).map(|y| {
                    (
                        format!("Archives/{y}"),
                        MailboxRole::Other,
                        u32::from(y == 2017),
                    )
                }))
                .chain([
                    ("Drafts".to_string(), MailboxRole::Drafts, 0),
                    ("Deleted Items".to_string(), MailboxRole::Trash, 0),
                ])
                .collect(),
            3 => [
                ("Clientes.Acme", MailboxRole::Other, 0),
                ("Clientes.Acme.2024", MailboxRole::Other, 2),
                ("Clientes.Acme.2024.T1", MailboxRole::Other, 0),
                ("Clientes.Zeta", MailboxRole::Other, 0),
            ]
            .into_iter()
            .map(|(p, r, u)| (p.to_string(), r, u))
            .collect(),
            _ => Vec::new(),
        };
        for (path, role, unread) in extra {
            store
                .upsert_mailbox(&mailbox(&path, role, unread))
                .expect("carpeta demo");
        }

        let mut headers = Vec::new();
        let mut unread = 0;
        for i in 0..12u32 {
            let thread = i / 3;
            let root_id = format!("<demo{index}-{}@ejemplo.com>", thread * 3);
            let mid = format!("<demo{index}-{i}@ejemplo.com>");
            let topic = topics[thread as usize % topics.len()];
            let (in_reply_to, references, subject) = if i % 3 == 0 {
                (None, Vec::new(), topic.0.to_string())
            } else {
                let prev = format!("<demo{index}-{}@ejemplo.com>", i - 1);
                (
                    Some(prev.clone()),
                    vec![root_id.clone(), prev],
                    format!("Re: {}", topic.0),
                )
            };
            let (name, sender) = people[(thread as usize + index) % people.len()];
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
                    email: sender.into(),
                }],
                to: vec![Address {
                    name: None,
                    email: email.into(),
                }],
                cc: Vec::new(),
                date: now
                    - (topic.2 * 3_600.0) as i64
                    - i64::from(2 - i % 3) * 900
                    - index as i64 * 600,
                flags: Flags {
                    seen,
                    ..Flags::default()
                },
                has_attachments: (i + index as u32).is_multiple_of(5),
                snippet: Some(topic.1.to_string()),
                labels: if i % 6 == 1 {
                    vec!["Trabajo".to_string()]
                } else {
                    Vec::new()
                },
            });
        }
        store
            .upsert_headers(inbox, &headers)
            .expect("cabeceras demo");
        // Cuerpos en la caché, para que el lector se pueda ver sin red.
        for h in &headers {
            let Ok(Some(message_id)) = store.message_id(inbox, h.uid) else {
                continue;
            };
            let sender = h
                .from
                .first()
                .and_then(|a| a.name.clone())
                .unwrap_or_default();
            let text = format!(
                "Hola:\n\n{}\n\nQuedo pendiente de tus comentarios; si te viene bien, lo \
                 repasamos juntos esta semana y lo cerramos antes del viernes.\n\n\
                 Un abrazo,\n{sender}\n\n> El mensaje anterior decía:\n> Gracias por la \
                 revisión, me parece bien el enfoque.\n> Un saludo.\n",
                h.snippet.clone().unwrap_or_default()
            );
            let attachments = if h.uid % 6 == 3 {
                crate::demo_backend::demo_attachments()
            } else {
                Vec::new()
            };
            let _ = store.save_body(
                message_id,
                &correo_core::MessageBody {
                    text: Some(text),
                    html: None,
                    attachments,
                },
            );
        }
        if index == 0 {
            // Correos HTML (boletín, factura, citas, tema oscuro, hostil) para la vista completa.
            crate::demo_html::seed(store, inbox, email, now);
        }
        store
            .upsert_mailbox(&mailbox(inbox_path, MailboxRole::Inbox, unread))
            .expect("contador demo");
    }
}

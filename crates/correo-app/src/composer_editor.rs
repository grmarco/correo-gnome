//! Editor con formato mínimo sobre `gtk::TextView`: negrita, cursiva, subrayado,
//! viñetas, citas y enlaces. Traduce su `TextBuffer` al modelo de `composer_format`.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib, pango};

use super::composer_format::{normalize_link, ParaKind, Paragraph, Run, Style};
use super::composer_mail::BULLET;

const BOLD: &str = "bold";
const ITALIC: &str = "italic";
const UNDERLINE: &str = "underline";
const QUOTE: &str = "quote";
const LINK_PREFIX: &str = "link:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Inline {
    Bold,
    Italic,
    Underline,
}

impl Inline {
    fn tag(self) -> &'static str {
        match self {
            Inline::Bold => BOLD,
            Inline::Italic => ITALIC,
            Inline::Underline => UNDERLINE,
        }
    }
}

struct Inner {
    view: gtk::TextView,
    /// Formatos que se aplican al texto que se escriba sin selección.
    pending: RefCell<BTreeSet<Inline>>,
    /// Posición del cursor tras el último cambio (-1 si no se conoce).
    last_cursor: Cell<i32>,
}

/// Editor de cuerpo. Clonable: los clones comparten el mismo estado.
#[derive(Clone)]
pub struct Editor {
    pub widget: gtk::Box,
    inner: Rc<Inner>,
}

impl Editor {
    pub fn new() -> Self {
        let view = gtk::TextView::new();
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_left_margin(12);
        view.set_right_margin(12);
        view.set_top_margin(8);
        view.set_bottom_margin(8);
        view.set_vexpand(true);
        view.add_css_class("composer-body");

        let inner = Rc::new(Inner {
            view: view.clone(),
            pending: RefCell::new(BTreeSet::new()),
            last_cursor: Cell::new(-1),
        });

        // Texto escrito con un formato pendiente: se aplica a lo insertado.
        let buf = view.buffer();
        {
            let inner = inner.clone();
            buf.connect_changed(move |buf| {
                let cursor = buf.cursor_position();
                let last = inner.last_cursor.get();
                if last >= 0 && cursor > last && !inner.pending.borrow().is_empty() {
                    let (start, end) = (buf.iter_at_offset(last), buf.iter_at_offset(cursor));
                    for fmt in inner.pending.borrow().iter() {
                        let tag = ensure_tag(buf, fmt.tag());
                        buf.apply_tag(&tag, &start, &end);
                    }
                }
                inner.last_cursor.set(cursor);
            });
        }

        // Mover el cursor con el ratón o el teclado cancela los formatos pendientes.
        let click = gtk::GestureClick::new();
        {
            let inner = inner.clone();
            click.connect_pressed(move |_, _, _, _| inner.clear_pending());
        }
        view.add_controller(click);

        let keys = gtk::EventControllerKey::new();
        {
            let inner = inner.clone();
            keys.connect_key_pressed(move |_, keyval, _, state| {
                if state.contains(gdk::ModifierType::CONTROL_MASK) {
                    let handled = match keyval.to_unicode().map(|c| c.to_ascii_lowercase()) {
                        Some('b') => inner.toggle_inline(Inline::Bold),
                        Some('i') => inner.toggle_inline(Inline::Italic),
                        Some('u') => inner.toggle_inline(Inline::Underline),
                        Some('k') => {
                            inner.insert_link();
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        return glib::Propagation::Stop;
                    }
                } else if matches!(keyval, gdk::Key::Return | gdk::Key::KP_Enter)
                    && inner.continue_bullet()
                {
                    return glib::Propagation::Stop;
                } else if is_cursor_key(keyval) {
                    inner.clear_pending();
                }
                glib::Propagation::Proceed
            });
        }
        view.add_controller(keys);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.append(&toolbar(&inner));
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_vexpand(true);
        scroll.set_child(Some(&view));
        widget.append(&scroll);

        Self { widget, inner }
    }

    pub fn view(&self) -> &gtk::TextView {
        &self.inner.view
    }

    pub fn buffer(&self) -> gtk::TextBuffer {
        self.inner.view.buffer()
    }

    /// Sustituye todo el contenido por `paras` y sitúa el cursor al principio.
    pub fn set_paragraphs(&self, paras: &[Paragraph]) {
        let buf = self.buffer();
        let mut text = String::new();
        // (inicio, fin, nombres de etiqueta) en desplazamientos de carácter.
        let mut spans: Vec<(i32, i32, Vec<String>)> = Vec::new();
        let mut pos: i32 = 0;
        for (idx, p) in paras.iter().enumerate() {
            let line_start = pos;
            if p.kind == ParaKind::Bullet {
                text.push_str(BULLET);
                pos += BULLET.chars().count() as i32;
            }
            for run in &p.runs {
                let n = run.text.chars().count() as i32;
                if n == 0 {
                    continue;
                }
                spans.push((pos, pos + n, style_tags(&run.style)));
                text.push_str(&run.text);
                pos += n;
            }
            if idx + 1 < paras.len() {
                text.push('\n');
                pos += 1;
            }
            if p.kind == ParaKind::Quote {
                spans.push((line_start, pos, vec![QUOTE.to_owned()]));
            }
        }
        buf.set_text(&text);
        for (start, end, names) in spans {
            let (s, e) = (buf.iter_at_offset(start), buf.iter_at_offset(end));
            for name in names {
                let tag = ensure_tag(&buf, &name);
                buf.apply_tag(&tag, &s, &e);
            }
        }
        self.inner.clear_pending();
        buf.place_cursor(&buf.start_iter());
    }

    /// Lee el contenido del editor como párrafos.
    pub fn paragraphs(&self) -> Vec<Paragraph> {
        read_paragraphs(&self.buffer())
    }
}

impl Inner {
    fn clear_pending(&self) {
        self.pending.borrow_mut().clear();
        self.last_cursor.set(-1);
    }

    /// Con selección: aplica o quita el formato. Sin selección: alterna el formato pendiente.
    fn toggle_inline(&self, fmt: Inline) -> bool {
        let buf = self.view.buffer();
        let tag = ensure_tag(&buf, fmt.tag());
        if let Some((start, end)) = buf.selection_bounds() {
            if start.has_tag(&tag) {
                buf.remove_tag(&tag, &start, &end);
            } else {
                buf.apply_tag(&tag, &start, &end);
            }
        } else {
            let mut pending = self.pending.borrow_mut();
            if !pending.remove(&fmt) {
                pending.insert(fmt);
            }
            self.last_cursor.set(buf.cursor_position());
        }
        self.view.grab_focus();
        true
    }

    /// Enter en una línea con viñeta: nueva viñeta debajo. Si la viñeta está vacía,
    /// quita la viñeta y deja que Enter termine la lista (devuelve `false`).
    fn continue_bullet(&self) -> bool {
        let buf = self.view.buffer();
        let start = line_start(&buf, buf.iter_at_offset(buf.cursor_position()).line());
        let mut content = start;
        content.forward_chars(2);
        if buf.text(&start, &content, false).as_str() != BULLET {
            return false;
        }
        let mut line_end = content;
        line_end.forward_to_line_end();
        if buf.text(&content, &line_end, false).is_empty() {
            let (mut s, mut e) = (start, content);
            buf.delete(&mut s, &mut e);
            return false;
        }
        buf.insert_at_cursor(&format!("\n{BULLET}"));
        true
    }

    /// Alterna viñetas en las líneas seleccionadas (o en la del cursor).
    pub fn toggle_bullets(&self) {
        let buf = self.view.buffer();
        let (first, last) = selected_lines(&buf);
        let all_bullets = (first..=last).all(|line| {
            let (start, mut end) = (line_start(&buf, line), line_start(&buf, line));
            end.forward_chars(2);
            buf.text(&start, &end, false).starts_with(BULLET)
        });
        for line in (first..=last).rev() {
            let mut start = line_start(&buf, line);
            if all_bullets {
                let mut end = start;
                end.forward_chars(2);
                buf.delete(&mut start, &mut end);
            } else {
                buf.insert(&mut start, BULLET);
            }
        }
        self.view.grab_focus();
    }

    /// Alterna la cita (sangría y color) en las líneas seleccionadas.
    pub fn toggle_quote(&self) {
        let buf = self.view.buffer();
        let (first, last) = selected_lines(&buf);
        let tag = ensure_tag(&buf, QUOTE);
        let quoted = line_start(&buf, first)
            .tags()
            .iter()
            .any(|t| tag_name(t) == QUOTE);
        for line in first..=last {
            let (start, end) = line_range(&buf, line);
            if quoted {
                buf.remove_tag(&tag, &start, &end);
            } else {
                buf.apply_tag(&tag, &start, &end);
            }
        }
        self.view.grab_focus();
    }

    /// Diálogo para el enlace: envuelve la selección o inserta la URL como texto.
    fn insert_link(self: &Rc<Self>) {
        let buf = self.view.buffer();
        let selected = buf
            .selection_bounds()
            .map(|(s, e)| (s.offset(), e.offset()));
        let dialog = adw::AlertDialog::new(Some("Insertar enlace"), None);
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("https://… o correo@dominio"));
        entry.set_activates_default(true);
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancelar");
        dialog.add_response("insert", "Insertar");
        dialog.set_response_appearance("insert", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("insert"));
        dialog.set_close_response("cancel");

        let parent = self.view.root().and_downcast::<gtk::Window>();
        let me = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let response = dialog.choose_future(parent.as_ref()).await;
            if response != "insert" {
                return;
            }
            let Some(url) = normalize_link(&entry.text()) else {
                return;
            };
            me.apply_link(&url, selected);
        });
    }

    fn apply_link(&self, url: &str, selected: Option<(i32, i32)>) {
        let buf = self.view.buffer();
        let tag = ensure_tag(&buf, &format!("{LINK_PREFIX}{url}"));
        match selected {
            Some((s, e)) => {
                let (start, end) = (buf.iter_at_offset(s), buf.iter_at_offset(e));
                buf.apply_tag(&tag, &start, &end);
            }
            None => {
                let offset = buf.cursor_position();
                let mut at = buf.iter_at_offset(offset);
                buf.insert(&mut at, url);
                let n = url.chars().count() as i32;
                let (start, end) = (buf.iter_at_offset(offset), buf.iter_at_offset(offset + n));
                buf.apply_tag(&tag, &start, &end);
            }
        }
        self.view.grab_focus();
    }
}

fn toolbar(inner: &Rc<Inner>) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    bar.set_margin_start(6);
    bar.set_margin_end(6);
    bar.set_margin_top(4);
    bar.set_margin_bottom(4);
    bar.add_css_class("toolbar");

    let i = inner.clone();
    tool_button(
        &bar,
        "format-text-bold-symbolic",
        "Negrita (Ctrl+B)",
        move || {
            i.toggle_inline(Inline::Bold);
        },
    );
    let i = inner.clone();
    tool_button(
        &bar,
        "format-text-italic-symbolic",
        "Cursiva (Ctrl+I)",
        move || {
            i.toggle_inline(Inline::Italic);
        },
    );
    let i = inner.clone();
    tool_button(
        &bar,
        "format-text-underline-symbolic",
        "Subrayado (Ctrl+U)",
        move || {
            i.toggle_inline(Inline::Underline);
        },
    );
    let i = inner.clone();
    tool_button(
        &bar,
        "view-list-bullet-symbolic",
        "Lista con viñetas",
        move || {
            i.toggle_bullets();
        },
    );
    let i = inner.clone();
    tool_button(&bar, "format-indent-more-symbolic", "Cita", move || {
        i.toggle_quote();
    });
    let i = inner.clone();
    tool_button(&bar, "insert-link-symbolic", "Enlace (Ctrl+K)", move || {
        i.insert_link();
    });
    bar
}

fn tool_button(bar: &gtk::Box, icon: &str, tip: &str, action: impl Fn() + 'static) {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tip));
    button.add_css_class("flat");
    button.connect_clicked(move |_| action());
    bar.append(&button);
}

fn is_cursor_key(key: gdk::Key) -> bool {
    matches!(
        key,
        gdk::Key::Left
            | gdk::Key::Right
            | gdk::Key::Up
            | gdk::Key::Down
            | gdk::Key::Home
            | gdk::Key::End
            | gdk::Key::Page_Up
            | gdk::Key::Page_Down
    )
}

/// Primera y última línea (inclusive) afectadas por la selección o el cursor.
fn selected_lines(buf: &gtk::TextBuffer) -> (i32, i32) {
    match buf.selection_bounds() {
        Some((s, e)) => {
            let mut last = e.line();
            if e.starts_line() && last > s.line() {
                last -= 1;
            }
            (s.line(), last)
        }
        None => {
            let line = buf.iter_at_offset(buf.cursor_position()).line();
            (line, line)
        }
    }
}

/// Inicio de una línea (el final del buffer si la línea no existe).
fn line_start(buf: &gtk::TextBuffer, line: i32) -> gtk::TextIter {
    buf.iter_at_line(line).unwrap_or_else(|| buf.end_iter())
}

/// Rango de una línea incluido su salto de línea final.
fn line_range(buf: &gtk::TextBuffer, line: i32) -> (gtk::TextIter, gtk::TextIter) {
    let start = line_start(buf, line);
    let end = if line + 1 < buf.line_count() {
        line_start(buf, line + 1)
    } else {
        buf.end_iter()
    };
    (start, end)
}

fn tag_name(tag: &gtk::TextTag) -> String {
    tag.name().map(|n| n.to_string()).unwrap_or_default()
}

fn style_tags(style: &Style) -> Vec<String> {
    let mut names = Vec::new();
    if style.bold {
        names.push(BOLD.to_owned());
    }
    if style.italic {
        names.push(ITALIC.to_owned());
    }
    if style.underline {
        names.push(UNDERLINE.to_owned());
    }
    if let Some(url) = &style.link {
        names.push(format!("{LINK_PREFIX}{url}"));
    }
    names
}

/// Devuelve la etiqueta `name`, creándola con su aspecto si no existe.
fn ensure_tag(buf: &gtk::TextBuffer, name: &str) -> gtk::TextTag {
    let table = buf.tag_table();
    if let Some(tag) = table.lookup(name) {
        return tag;
    }
    let tag = gtk::TextTag::new(Some(name));
    if name == BOLD {
        tag.set_weight(700);
    } else if name == ITALIC {
        tag.set_style(pango::Style::Italic);
    } else if name == UNDERLINE {
        tag.set_underline(pango::Underline::Single);
    } else if name == QUOTE {
        tag.set_left_margin(24);
        tag.set_foreground(Some("#5e5c64"));
    } else {
        tag.set_underline(pango::Underline::Single);
        tag.set_foreground(Some("#1c71d8"));
    }
    table.add(&tag);
    tag
}

/// Traduce el contenido del buffer a párrafos.
pub fn read_paragraphs(buf: &gtk::TextBuffer) -> Vec<Paragraph> {
    (0..buf.line_count())
        .map(|line| {
            let start = line_start(buf, line);
            let mut end = start;
            end.forward_to_line_end();
            let mut bullet_end = start;
            bullet_end.forward_chars(2);
            let bullet = buf.text(&start, &bullet_end, false).starts_with(BULLET);
            let quoted = start.tags().iter().any(|t| tag_name(t) == QUOTE);
            let (kind, mut it) = if bullet {
                (ParaKind::Bullet, bullet_end)
            } else if quoted {
                (ParaKind::Quote, start)
            } else {
                (ParaKind::Normal, start)
            };
            let mut runs: Vec<Run> = Vec::new();
            while it.offset() < end.offset() {
                let style = style_at(&it);
                let ch = it.char();
                match runs.last_mut() {
                    Some(run) if run.style == style => run.text.push(ch),
                    _ => runs.push(Run {
                        text: ch.to_string(),
                        style,
                    }),
                }
                if !it.forward_chars(1) {
                    break;
                }
            }
            Paragraph { kind, runs }
        })
        .collect()
}

fn style_at(it: &gtk::TextIter) -> Style {
    let mut style = Style::default();
    for tag in it.tags() {
        let name = tag_name(&tag);
        match name.as_str() {
            BOLD => style.bold = true,
            ITALIC => style.italic = true,
            UNDERLINE => style.underline = true,
            n if n.starts_with(LINK_PREFIX) => style.link = Some(n[LINK_PREFIX.len()..].to_owned()),
            _ => {}
        }
    }
    style
}

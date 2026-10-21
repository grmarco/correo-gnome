//! Pegamento entre el lector y la vista completa (`reader_web.rs`): decide el renderizador,
//! crea el WebView de forma perezosa, lo reutiliza entre mensajes y lo destruye tras un rato
//! sin HTML visible. Es un módulo hijo de `reader`, así que ve sus tipos privados.

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use super::reader_html::{count_images, html_to_segments, Segment};
use super::reader_web::{HtmlView, WebDoc, WebEvent};
use super::reader_web_sanitize::{
    body_style, needs_rich_rendering, sanitize_for_web, table_width_hint,
};
use super::{limit_body, open_link, Inner, LinkTarget, BANNER_NORMAL};
use crate::setup::config::HtmlRenderer;

/// Segundos sin HTML visible tras los que se destruye el WebView (un solo disparo).
pub const WEB_IDLE_SECS: u32 = 30;
/// Tope del HTML que se sanea y se entrega a WebKit.
const WEB_LIMIT: usize = 2 * 1024 * 1024;
pub const LABEL_FULL: &str = "Vista completa";
pub const LABEL_LIGHT: &str = "Vista ligera";
const BANNER_BUTTON: &str = "Cargar imágenes remotas";
const FALLBACK_NOTICE: &str = "La vista completa no está disponible; se muestra la vista ligera";
const TRUNCATED_WEB: &str = "Mensaje truncado: no se muestra el contenido completo";

/// ¿Se muestra este HTML con WebKit? Prima la última elección del usuario en la sesión.
pub fn should_use(inner: &Inner, html: &str) -> bool {
    if inner.web_failed.get() {
        return false;
    }
    match inner.web_pref.get() {
        Some(full) => full,
        None => decide_mode(inner.renderer, html),
    }
}

/// Decisión según la configuración (sin preferencia del usuario).
pub fn decide_mode(renderer: HtmlRenderer, html: &str) -> bool {
    match renderer {
        HtmlRenderer::Webkit => true,
        HtmlRenderer::Light => false,
        HtmlRenderer::Auto => needs_rich_rendering(html),
    }
}

/// Muestra `html` en el WebView (lo crea si hace falta). Si falla, el lector usa la vista
/// ligera.
pub fn show(inner: &Rc<Inner>, html: &str) -> Result<(), String> {
    cancel_idle(inner);
    ensure_view(inner)?;

    let (html, truncated) = cut(html);
    let images = count_images(html);
    let doc = WebDoc {
        clean: sanitize_for_web(html),
        body_style: body_style(html),
        width_hint: table_width_hint(html),
    };
    // Enlaces para la comprobación de «texto que aparenta otro dominio».
    let (segments_src, _) = limit_body(html);
    *inner.links.borrow_mut() = links_of(&html_to_segments(segments_src));
    inner
        .text_view
        .set_buffer(Some(&gtk::TextBuffer::new(None::<&gtk::TextTagTable>)));

    if truncated {
        inner.banner.set_title(TRUNCATED_WEB);
        inner.banner.set_icon("dialog-information-symbolic");
        inner.banner.set_button_label(None);
        inner.banner.set_revealed(true);
    } else {
        inner.banner.set_title(BANNER_NORMAL);
        inner.banner.set_icon("view-conceal-symbolic");
        inner.banner.set_button_label(Some(BANNER_BUTTON));
        inner.banner.set_revealed(images > 0);
    }

    inner.web_active.set(true);
    inner.toggle.set_label(LABEL_LIGHT);
    inner.toggle.set_visible(true);
    // Esqueleto mientras carga; el fundido llega cuando el documento está listo.
    inner
        .stack
        .set_visible_child_full("loading", gtk::StackTransitionType::None);
    let token = {
        let web = inner.web.borrow();
        let view = web.as_ref().ok_or("la vista web no existe")?;
        view.show(doc, false)
    };
    inner.web_token.set(token);
    Ok(())
}

/// Recorta a `WEB_LIMIT` bytes en un límite de carácter.
fn cut(html: &str) -> (&str, bool) {
    if html.len() <= WEB_LIMIT {
        return (html, false);
    }
    let mut end = WEB_LIMIT;
    while !html.is_char_boundary(end) {
        end -= 1;
    }
    (&html[..end], true)
}

/// El usuario acepta imágenes remotas: solo se relaja la carga de imágenes https.
pub fn allow_remote_images(inner: &Rc<Inner>) {
    if !inner.web_active.get() {
        return;
    }
    let token = inner.web.borrow().as_ref().map(|v| v.reload_with_images());
    if let Some(token) = token {
        inner.web_token.set(token);
        inner
            .stack
            .set_visible_child_full("loading", gtk::StackTransitionType::None);
    }
    inner.banner.set_revealed(false);
}

/// Alterna «Vista ligera» ↔ «Vista completa» y lo recuerda el resto de la sesión.
pub fn toggle(inner: &Rc<Inner>) {
    inner.web_pref.set(Some(!inner.web_active.get()));
    let body = inner.current.borrow().body.clone();
    if let Some(body) = body {
        super::render_body(inner, &body);
    }
}

/// El mensaje actual ya no usa WebKit (o no hay mensaje): se descarta el documento y se
/// programa la destrucción del WebView.
pub fn release(inner: &Rc<Inner>) {
    inner.web_active.set(false);
    let Some(()) = inner.web.borrow().as_ref().map(HtmlView::blank) else {
        return;
    };
    if inner.web_idle.borrow().is_some() {
        return;
    }
    let weak = Rc::downgrade(inner);
    let id =
        glib::timeout_add_local_once(Duration::from_secs(u64::from(WEB_IDLE_SECS)), move || {
            let Some(inner) = weak.upgrade() else { return };
            // La fuente ya se ha disparado: se descarta el id sin `remove` (no hay nada que quitar).
            inner.web_idle.borrow_mut().take();
            if !inner.web_active.get() {
                destroy(&inner);
            }
        });
    *inner.web_idle.borrow_mut() = Some(id);
}

fn cancel_idle(inner: &Inner) {
    if let Some(id) = inner.web_idle.borrow_mut().take() {
        id.remove();
    }
}

/// Destruye el WebView (si existe) y, con él, sus procesos auxiliares.
pub fn destroy(inner: &Inner) {
    cancel_idle(inner);
    inner.web_active.set(false);
    let Some(view) = inner.web.borrow_mut().take() else {
        return;
    };
    if inner.stack.visible_child_name().as_deref() == Some("web") {
        inner
            .stack
            .set_visible_child_full("empty", gtk::StackTransitionType::None);
    }
    inner.stack.remove(view.widget());
    view.shutdown();
}

fn ensure_view(inner: &Rc<Inner>) -> Result<(), String> {
    if inner.web.borrow().is_some() {
        return Ok(());
    }
    let weak = Rc::downgrade(inner);
    let view = HtmlView::new(Rc::new(move |event| {
        if let Some(inner) = weak.upgrade() {
            handle_event(&inner, event);
        }
    }))?;
    inner.stack.add_named(view.widget(), Some("web"));
    *inner.web.borrow_mut() = Some(view);
    Ok(())
}

fn handle_event(inner: &Rc<Inner>, event: WebEvent) {
    match event {
        WebEvent::Loaded(token) => {
            if token != inner.web_token.get() || !inner.web_active.get() {
                return;
            }
            let duration = if crate::ui_style::animations_enabled(&inner.stack) {
                160
            } else {
                0
            };
            inner.stack.set_transition_duration(duration);
            inner
                .stack
                .set_visible_child_full("web", gtk::StackTransitionType::Crossfade);
        }
        WebEvent::Failed(reason) => {
            // Se difiere: puede llegar dentro de una señal del propio WebView.
            let weak = Rc::downgrade(inner);
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade() {
                    fall_back(&inner, &reason);
                }
            });
        }
        WebEvent::OpenLink { uri, label } => {
            let link = link_for_open(&inner.links.borrow(), &uri, label.as_deref());
            open_link(&inner.root, &link);
        }
    }
}

/// La vista completa falla: se destruye, no se vuelve a intentar en la sesión y el mensaje
/// actual se muestra con el visor ligero y un aviso discreto.
fn fall_back(inner: &Rc<Inner>, reason: &str) {
    if inner.web_failed.replace(true) {
        return;
    }
    eprintln!("vista completa no disponible: {reason}");
    destroy(inner);
    let body = inner.current.borrow().body.clone();
    if let Some(body) = body {
        super::render_body(inner, &body);
    }
    show_fallback_notice(inner);
}

pub fn show_fallback_notice(inner: &Inner) {
    inner.banner.set_title(FALLBACK_NOTICE);
    inner.banner.set_icon("dialog-information-symbolic");
    inner.banner.set_button_label(None);
    inner.banner.set_revealed(true);
}

/// Enlaces (destino y texto visible) de los segmentos convertidos.
pub fn links_of(segments: &[Segment]) -> Vec<LinkTarget> {
    segments
        .iter()
        .filter_map(|s| match s {
            Segment::Text { text, style } => style.link.as_ref().map(|href| LinkTarget {
                href: href.clone(),
                shown: text.clone(),
            }),
            Segment::Paragraph => None,
        })
        .collect()
}

/// Enlace a abrir para la URI que pulsó el usuario: se toma como texto visible el que
/// parezca otro dominio (si lo hay) entre los textos de los enlaces con ese destino y la
/// etiqueta bajo el puntero.
pub fn link_for_open(links: &[LinkTarget], uri: &str, label: Option<&str>) -> LinkTarget {
    let norm = |u: &str| u.trim().trim_end_matches('/').to_ascii_lowercase();
    let wanted = norm(uri);
    let mut shown: Vec<&str> = links
        .iter()
        .filter(|l| norm(&l.href) == wanted)
        .map(|l| l.shown.as_str())
        .collect();
    shown.extend(label);
    let pick = shown
        .iter()
        .find(|s| super::link_looks_deceptive(s, uri))
        .or(shown.first())
        .copied()
        .unwrap_or_default();
    LinkTarget {
        href: uri.to_owned(),
        shown: pick.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(href: &str, shown: &str) -> LinkTarget {
        LinkTarget {
            href: href.into(),
            shown: shown.into(),
        }
    }

    #[test]
    fn mode_follows_configuration() {
        let plain = "<p>hola</p>";
        let rich = "<table><tr><td>x</td></tr></table>";
        assert!(decide_mode(HtmlRenderer::Webkit, plain));
        assert!(!decide_mode(HtmlRenderer::Light, rich));
        assert!(!decide_mode(HtmlRenderer::Auto, plain));
        assert!(decide_mode(HtmlRenderer::Auto, rich));
    }

    #[test]
    fn deceptive_text_is_found_through_the_normalised_uri() {
        let links = vec![
            link("https://evil.example", "https://paypal.com"),
            link("https://otro.example/", "otro"),
        ];
        // WebKit normaliza añadiendo la barra final.
        let l = link_for_open(&links, "https://evil.example/", None);
        assert_eq!(l.shown, "https://paypal.com");
        assert!(super::super::link_looks_deceptive(&l.shown, &l.href));
    }

    #[test]
    fn hover_label_is_used_when_the_link_is_unknown() {
        let l = link_for_open(&[], "https://evil.example/", Some("www.banco.es"));
        assert_eq!(l.shown, "www.banco.es");
        let none = link_for_open(&[], "https://x.example/", None);
        assert_eq!(none.shown, "");
        assert!(!super::super::link_looks_deceptive(&none.shown, &none.href));
    }
}

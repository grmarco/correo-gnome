//! Modo oscuro de la vista completa: decisiones puras (sin GTK ni WebKit).
//!
//! Un correo HTML suele asumir fondo blanco. En tema oscuro se elige un `Look`:
//! - `Native`: el correo declara que sabe de modo oscuro (`<meta name="color-scheme">` con
//!   `dark`, `color-scheme: dark` en CSS o reglas `@media (prefers-color-scheme: dark)`); se le
//!   ofrece el esquema oscuro y sus propias reglas.
//! - `Dark`: ya trae un fondo oscuro explícito; se deja tal cual (sin invertir otra vez).
//! - `Invert`: oscurización automática («smart invert») con un filtro CSS, re-invirtiendo
//!   imágenes y vídeo.
//! - `Light`: colores originales sobre tarjeta blanca (tema claro, o elección del usuario).
//!
//! Los `@media (prefers-color-scheme: ...)` del correo se reescriben según el aspecto elegido
//! (ver `rewrite_scheme_queries`), de modo que el resultado no dependa de lo que WebKit crea
//! que es el esquema del sistema.

use super::reader_web_sanitize::attr_value;
use crate::setup::config::HtmlDarkMode;

/// Aspecto con el que se muestra un correo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Light,
    Native,
    Dark,
    Invert,
}

impl Look {
    /// ¿La tarjeta es oscura?
    pub fn is_dark(self) -> bool {
        self != Self::Light
    }

    /// ¿Se evalúan como ciertas las consultas `prefers-color-scheme: dark`?
    pub fn dark_media(self) -> bool {
        matches!(self, Self::Native | Self::Dark)
    }
}

/// Elige el aspecto. `pref` es la elección del menú en la sesión (`Some(true)` = adaptar al
/// tema oscuro, `Some(false)` = colores originales).
pub fn choose_look(mode: HtmlDarkMode, pref: Option<bool>, dark_theme: bool, html: &str) -> Look {
    if !dark_theme {
        return Look::Light;
    }
    let mode = match (pref, mode) {
        (Some(false), _) => HtmlDarkMode::Original,
        (Some(true), HtmlDarkMode::Invert) => HtmlDarkMode::Invert,
        (Some(true), _) => HtmlDarkMode::Auto,
        (None, mode) => mode,
    };
    match mode {
        HtmlDarkMode::Original => Look::Light,
        HtmlDarkMode::Auto if supports_dark(html) => Look::Native,
        HtmlDarkMode::Auto | HtmlDarkMode::Invert => {
            if has_dark_background(html) {
                Look::Dark
            } else {
                Look::Invert
            }
        }
    }
}

/// ¿Declara el correo que sabe de modo oscuro? Busca (sobre el HTML original, antes del
/// saneado, que elimina `<meta>`) `color-scheme` / `supported-color-schemes` con `dark` y
/// `@media (prefers-color-scheme: dark)`.
pub fn supports_dark(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("color-scheme") {
        let at = from + i;
        let after = at + "color-scheme".len();
        from = after;
        let tail = &lower[after..];
        let stop = if lower[..at].ends_with("prefers-") {
            tail.find(')')
        } else {
            tail.find([';', '}', '>', ')'])
        };
        let window = &tail[..stop.unwrap_or(tail.len())];
        let window = match window.char_indices().nth(120) {
            Some((end, _)) => &window[..end],
            None => window,
        };
        if window.contains("dark") {
            return true;
        }
    }
    false
}

/// ¿Trae el correo un fondo oscuro explícito? Se mira, por este orden, el `<body>` (estilo en
/// línea, reglas `body`/`html` de `<style>`, `bgcolor`) y, si no declara nada, los primeros
/// contenedores (`table`/`div`) en el orden del documento.
pub fn has_dark_background(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    if let Some(color) = body_background(html, &lower) {
        return is_dark_color(color);
    }
    let mut rest = lower.as_str();
    let mut offset = 0;
    let mut seen = 0;
    while seen < 4 {
        let next = [rest.find("<table"), rest.find("<div")]
            .into_iter()
            .flatten()
            .min();
        let Some(i) = next else { break };
        let tag_src = &rest[i..];
        let end = tag_src.find('>').unwrap_or(tag_src.len());
        let (from, to) = (offset + i, offset + i + end);
        let (tag, tag_lower) = (&html[from..to], &lower[from..to]);
        if let Some(color) = tag_background(tag, tag_lower) {
            return is_dark_color(color);
        }
        seen += 1;
        offset += i + end;
        rest = &lower[offset..];
    }
    false
}

/// Fondo declarado en una etiqueta: estilo en línea y luego `bgcolor`.
fn tag_background(tag: &str, tag_lower: &str) -> Option<(u8, u8, u8)> {
    attr_value(tag, tag_lower, "style")
        .and_then(|style| declared_background(&style))
        .or_else(|| attr_value(tag, tag_lower, "bgcolor").and_then(|c| parse_color(&c)))
}

fn body_background(html: &str, lower: &str) -> Option<(u8, u8, u8)> {
    let start = lower.find("<body");
    let tag = start.map(|s| {
        let end = lower[s..].find('>').map_or(lower.len(), |i| s + i);
        (&html[s..end], &lower[s..end])
    });
    if let Some((tag, tag_lower)) = tag {
        if let Some(color) =
            attr_value(tag, tag_lower, "style").and_then(|s| declared_background(&s))
        {
            return Some(color);
        }
    }
    if let Some(color) = style_rules_background(lower) {
        return Some(color);
    }
    tag.and_then(|(tag, tag_lower)| attr_value(tag, tag_lower, "bgcolor"))
        .and_then(|c| parse_color(&c))
}

/// Último fondo que las reglas de los `<style>` dan a `body`, `html` o `:root`.
fn style_rules_background(lower_html: &str) -> Option<(u8, u8, u8)> {
    let mut found = None;
    let mut rest = lower_html;
    while let Some(open) = rest.find("<style") {
        let Some(gt) = rest[open..].find('>') else {
            break;
        };
        let body = &rest[open + gt + 1..];
        let close = body.find("</style").unwrap_or(body.len());
        if let Some(color) = rules_background(&body[..close]) {
            found = Some(color);
        }
        rest = &body[close..];
    }
    found
}

/// Recorre las reglas de primer nivel de una hoja (se saltan los bloques `@...`).
fn rules_background(css: &str) -> Option<(u8, u8, u8)> {
    let css = strip_comments(css);
    let bytes = css.as_bytes();
    let mut found = None;
    let mut i = 0;
    while let Some(open) = css[i..].find('{').map(|j| i + j) {
        let selector = css[i..open].trim();
        // Cierre que equilibra la llave.
        let mut depth = 0usize;
        let mut close = css.len();
        for (j, b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = j;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !selector.starts_with('@')
            && selector
                .split(',')
                .map(str::trim)
                .any(|s| matches!(s, "body" | "html" | ":root"))
        {
            if let Some(color) = declared_background(&css[open + 1..close]) {
                found = Some(color);
            }
        }
        i = (close + 1).min(css.len());
    }
    found
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = match rest[i + 2..].find("*/") {
            Some(j) => &rest[i + 2 + j + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Color de fondo que fijan unas declaraciones CSS (`background-color` o color dentro de
/// `background`); gana la última.
fn declared_background(decls: &str) -> Option<(u8, u8, u8)> {
    let mut found = None;
    for decl in decls.split(';') {
        let Some((prop, value)) = decl.split_once(':') else {
            continue;
        };
        match prop.trim().to_ascii_lowercase().as_str() {
            "background-color" => found = parse_color(value),
            "background" => found = color_in(value),
            _ => {}
        }
    }
    found
}

/// Primer color dentro de un valor (por ejemplo, el de `background: #111 url(...) no-repeat`).
fn color_in(value: &str) -> Option<(u8, u8, u8)> {
    let mut depth = 0;
    let mut token = String::new();
    let mut tokens = Vec::new();
    for c in value.chars() {
        match c {
            '(' => {
                depth += 1;
                token.push(c);
            }
            ')' => {
                depth -= 1;
                token.push(c);
            }
            c if c.is_whitespace() && depth == 0 => tokens.push(std::mem::take(&mut token)),
            c => token.push(c),
        }
    }
    tokens.push(token);
    tokens.iter().find_map(|t| parse_color(t))
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb()/rgba()` y unos pocos nombres. `None` si es
/// transparente o no se entiende.
fn parse_color(value: &str) -> Option<(u8, u8, u8)> {
    let v = value
        .trim()
        .trim_end_matches("!important")
        .trim()
        .to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let nib = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok().map(|n| n * 17);
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return match hex.len() {
            3 => Some((nib(0)?, nib(1)?, nib(2)?)),
            6 => Some((byte(0)?, byte(2)?, byte(4)?)),
            8 if byte(6)? >= 128 => Some((byte(0)?, byte(2)?, byte(4)?)),
            _ => None,
        };
    }
    if let Some(args) = v
        .strip_prefix("rgba(")
        .or_else(|| v.strip_prefix("rgb("))
        .and_then(|a| a.strip_suffix(')'))
    {
        let parts: Vec<&str> = args
            .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .collect();
        let chan = |p: &str| -> Option<u8> {
            match p.strip_suffix('%') {
                Some(pc) => pc
                    .parse::<f32>()
                    .ok()
                    .map(|n| (n * 2.55).round().clamp(0.0, 255.0) as u8),
                None => p
                    .parse::<f32>()
                    .ok()
                    .map(|n| n.round().clamp(0.0, 255.0) as u8),
            }
        };
        if parts.len() < 3 {
            return None;
        }
        if let Some(alpha) = parts.get(3) {
            let a = match alpha.strip_suffix('%') {
                Some(pc) => pc.parse::<f32>().ok()? / 100.0,
                None => alpha.parse::<f32>().ok()?,
            };
            if a < 0.5 {
                return None;
            }
        }
        return Some((chan(parts[0])?, chan(parts[1])?, chan(parts[2])?));
    }
    match v.as_str() {
        "black" => Some((0, 0, 0)),
        "white" => Some((255, 255, 255)),
        "navy" => Some((0, 0, 128)),
        "darkblue" => Some((0, 0, 139)),
        "midnightblue" => Some((25, 25, 112)),
        "darkslategray" | "darkslategrey" => Some((47, 79, 79)),
        "dimgray" | "dimgrey" => Some((105, 105, 105)),
        "gray" | "grey" => Some((128, 128, 128)),
        "silver" => Some((192, 192, 192)),
        "lightgray" | "lightgrey" => Some((211, 211, 211)),
        "whitesmoke" => Some((245, 245, 245)),
        "ivory" => Some((255, 255, 240)),
        "beige" => Some((245, 245, 220)),
        _ => None,
    }
}

/// Luminancia percibida (0..1) por debajo de la que un fondo se considera oscuro.
const DARK_LUMINANCE: f32 = 0.4;

fn is_dark_color((r, g, b): (u8, u8, u8)) -> bool {
    let lum = (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0;
    lum < DARK_LUMINANCE
}

/// Reescribe las consultas `prefers-color-scheme: dark|light` de un `<style>` para que valgan
/// lo que corresponde al aspecto (`dark`: el correo se pinta en oscuro). Una consulta que
/// debe cumplirse pasa a `(min-width: 0px)` y la que no, a `(min-width: 99999px)`. Quita la
/// dependencia de lo que WebKit crea que es el esquema del sistema.
pub fn rewrite_scheme_queries(css: &str, dark: bool) -> String {
    const KEY: &str = "prefers-color-scheme";
    let lower = css.to_ascii_lowercase();
    if !lower.contains(KEY) {
        return css.to_owned();
    }
    let mut out = String::with_capacity(css.len());
    let mut at = 0;
    while let Some(i) = lower[at..].find(KEY).map(|j| at + j) {
        // Debe estar dentro de unos paréntesis: «(prefers-color-scheme : dark)».
        let after = &lower[i + KEY.len()..];
        let skip = after.len() - after.trim_start().len();
        let value = after.trim_start().strip_prefix(':').map(str::trim_start);
        let word = value.and_then(|v| {
            ["dark", "light"].into_iter().find(|w| {
                v.strip_prefix(w).is_some_and(|r| {
                    !r.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '-')
                })
            })
        });
        let open = lower[..i].rfind('(');
        let (Some(word), Some(open)) = (word, open) else {
            out.push_str(&css[at..i + KEY.len()]);
            at = i + KEY.len();
            continue;
        };
        // Fin de la consulta: tras el valor y los espacios, el paréntesis de cierre.
        let value_start = i + KEY.len() + skip + 1;
        let v = &lower[value_start..];
        let ws = v.len() - v.trim_start().len();
        let word_end = value_start + ws + word.len();
        let tail = &lower[word_end..];
        let Some(close) = tail.find(')').filter(|c| tail[..*c].trim().is_empty()) else {
            out.push_str(&css[at..i + KEY.len()]);
            at = i + KEY.len();
            continue;
        };
        let matches = (word == "dark") == dark;
        out.push_str(&css[at..open]);
        out.push_str(if matches {
            "(min-width: 0px)"
        } else {
            "(min-width: 99999px)"
        });
        at = word_end + close + 1;
    }
    out.push_str(&css[at..]);
    out
}

/// Parámetros de la oscurización automática. `brightness` y `contrast` son los de la
/// compensación (ajustables).
const BRIGHTNESS: f32 = 0.95;
const CONTRAST: f32 = 0.85;

/// Color (0..255) de la tarjeta oscura para los aspectos que no filtran.
pub const CARD_DARK: (u8, u8, u8) = (0x1e, 0x1e, 0x1e);

/// Color con el que queda el blanco tras la oscurización (el fondo de la tarjeta en `Invert`).
pub fn inverted_white() -> (u8, u8, u8) {
    // invert(1) deja el blanco en 0; brightness no lo cambia; contrast lo lleva a 0.5-0.5*c.
    let v = ((0.5 - 0.5 * CONTRAST) * 255.0).round() as u8;
    (v, v, v)
}

/// Color de fondo de la tarjeta/WebView para el aspecto.
pub fn card_color(look: Look) -> (u8, u8, u8) {
    match look {
        Look::Light => (255, 255, 255),
        Look::Native | Look::Dark => CARD_DARK,
        Look::Invert => inverted_white(),
    }
}

/// Hoja de estilos de usuario que inyecta WebKit según el aspecto. No depende del documento,
/// así que se cambia en caliente sin recargarlo.
pub fn user_stylesheet(look: Look) -> String {
    match look {
        Look::Light => ":root{color-scheme:light !important}".into(),
        Look::Native | Look::Dark => ":root{color-scheme:dark !important;\
             --correo-fg:#e8e8e8;--correo-link:#78aeed;--correo-quote-line:#5e5c64;\
             --correo-quote-fg:#aeacb5}"
            .into(),
        Look::Invert => {
            let forward = format!(
                "invert(1) hue-rotate(180deg) brightness({BRIGHTNESS}) contrast({CONTRAST})"
            );
            // Lo contrario, para que imágenes y vídeo se vean como son tras el filtro del
            // documento: deshace el contraste y el brillo, y vuelve a invertir.
            let restore = format!(
                "contrast({:.3}) brightness({:.3}) hue-rotate(180deg) invert(1)",
                1.0 / CONTRAST,
                1.0 / BRIGHTNESS
            );
            format!(
                ":root{{color-scheme:light !important}}\
                 html{{filter:{forward} !important;min-height:100%}}\
                 img,picture,video,[style*=\"background-image\" i],[style*=\"url(\" i]\
                 {{filter:{restore} !important}}\
                 body{{min-height:100vh}}\
                 html{{background-color:#fff}}"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_dark_awareness() {
        assert!(supports_dark(
            "<head><meta name=\"color-scheme\" content=\"light dark\"></head>"
        ));
        assert!(supports_dark(
            "<META NAME='supported-color-schemes' CONTENT='light dark'>"
        ));
        assert!(supports_dark(
            "<style>:root{color-scheme: light dark;}</style>"
        ));
        assert!(supports_dark(
            "<style>@media (prefers-color-scheme:dark){body{background:#000}}</style>"
        ));
        assert!(supports_dark(
            "<style>@media screen and ( prefers-color-scheme : dark ) {a{color:red}}</style>"
        ));
        assert!(!supports_dark(
            "<meta name=\"color-scheme\" content=\"light\">"
        ));
        assert!(!supports_dark(
            "<style>@media (prefers-color-scheme: light){.dark-card{x:y}}</style>"
        ));
        assert!(!supports_dark("<p>Hola, modo dark</p>"));
        assert!(!supports_dark(""));
    }

    #[test]
    fn awareness_survives_sanitising_of_style_elements() {
        let html = "<style>:root{color-scheme: light dark} \
                    @media (prefers-color-scheme: dark){body{background:#111;color:#eee}}</style><p>x</p>";
        let clean = super::super::reader_web_sanitize::sanitize_for_web(html);
        assert!(clean.contains("color-scheme: light dark"));
        assert!(clean.contains("@media (prefers-color-scheme: dark)"));
        assert!(clean.contains("background:#111"));
        assert!(supports_dark(&clean));
    }

    #[test]
    fn detects_explicit_dark_backgrounds() {
        assert!(has_dark_background(
            "<body style=\"background:#0f1419\"><p>x"
        ));
        assert!(has_dark_background("<body bgcolor=\"#101010\"><p>x"));
        assert!(has_dark_background(
            "<style>body { background-color: rgb(20,20,30); color:#eee }</style><p>x"
        ));
        assert!(has_dark_background(
            "<style>.a{background:#fff} html, body {background: #111 url(x.png)}</style>"
        ));
        assert!(has_dark_background(
            "<table bgcolor=\"black\"><tr><td>x</td></tr></table>"
        ));
        assert!(has_dark_background(
            "<div><table style=\"background-color:#222\"><tr><td>x"
        ));
        assert!(!has_dark_background(
            "<body style=\"background:#f2f4f7\"><p>x"
        ));
        assert!(!has_dark_background("<body bgcolor=\"white\"><p>x"));
        assert!(!has_dark_background(
            "<body><table bgcolor=\"#ffffff\"><tr><td bgcolor=\"#000\">x"
        ));
        assert!(!has_dark_background("<p>sin fondos</p>"));
        // Las reglas dentro de @media no cuentan como fondo base.
        assert!(!has_dark_background(
            "<style>@media (prefers-color-scheme: dark){body{background:#000}}</style><p>x"
        ));
        // Transparente o con alfa bajo: sin fondo.
        assert!(!has_dark_background(
            "<body style=\"background-color:rgba(0,0,0,0.1)\"><p>x"
        ));
    }

    #[test]
    fn color_parsing() {
        assert_eq!(parse_color("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_color(" #0F1419 "), Some((15, 20, 25)));
        assert_eq!(parse_color("rgb(10, 20, 30)"), Some((10, 20, 30)));
        assert_eq!(parse_color("rgb(10 20 30 / 0.9)"), Some((10, 20, 30)));
        assert_eq!(parse_color("rgba(0,0,0,0)"), None);
        assert_eq!(parse_color("#00000000"), None);
        assert_eq!(parse_color("transparent"), None);
        assert_eq!(parse_color("#12"), None);
        assert_eq!(parse_color("#zzz"), None);
        assert_eq!(color_in("url(a.png) no-repeat #123456"), Some((18, 52, 86)));
        assert_eq!(color_in("rgb(1, 2, 3) url(x)"), Some((1, 2, 3)));
    }

    #[test]
    fn look_selection() {
        let plain = "<p>hola</p>";
        let aware = "<meta name=\"color-scheme\" content=\"light dark\"><p>x</p>";
        let dark = "<body style=\"background:#111\"><p>x</p>";
        use HtmlDarkMode::{Auto, Invert, Original};
        // Tema claro: siempre tarjeta clara.
        assert_eq!(choose_look(Auto, None, false, aware), Look::Light);
        assert_eq!(choose_look(Invert, Some(true), false, plain), Look::Light);
        // Tema oscuro, automático.
        assert_eq!(choose_look(Auto, None, true, plain), Look::Invert);
        assert_eq!(choose_look(Auto, None, true, aware), Look::Native);
        assert_eq!(choose_look(Auto, None, true, dark), Look::Dark);
        // Originales.
        assert_eq!(choose_look(Original, None, true, plain), Look::Light);
        // Invertir siempre (menos lo que ya es oscuro).
        assert_eq!(choose_look(Invert, None, true, aware), Look::Invert);
        assert_eq!(choose_look(Invert, None, true, dark), Look::Dark);
        // Elección del menú.
        assert_eq!(choose_look(Auto, Some(false), true, aware), Look::Light);
        assert_eq!(choose_look(Original, Some(true), true, plain), Look::Invert);
        assert_eq!(choose_look(Original, Some(true), true, aware), Look::Native);
        assert_eq!(choose_look(Invert, Some(true), true, aware), Look::Invert);
    }

    #[test]
    fn scheme_queries_follow_the_look() {
        let css = "@media (prefers-color-scheme: dark) { a{x:y} }\
                   @media screen and (PREFERS-COLOR-SCHEME : light){b{x:y}}";
        let dark = rewrite_scheme_queries(css, true);
        assert_eq!(
            dark,
            "@media (min-width: 0px) { a{x:y} }\
             @media screen and (min-width: 99999px){b{x:y}}"
        );
        let light = rewrite_scheme_queries(css, false);
        assert!(light.starts_with("@media (min-width: 99999px)"));
        assert!(light.contains("and (min-width: 0px){b"));
        // Sin consultas, o con otras características, no se toca nada.
        let other = "@media (prefers-reduced-motion: reduce){a{x:y}} p{color:red}";
        assert_eq!(rewrite_scheme_queries(other, true), other);
        assert_eq!(
            rewrite_scheme_queries("@media (prefers-color-scheme: darker){}", true),
            "@media (prefers-color-scheme: darker){}"
        );
    }

    #[test]
    fn stylesheets_per_look() {
        assert!(user_stylesheet(Look::Light).contains("color-scheme:light"));
        let native = user_stylesheet(Look::Native);
        assert!(native.contains("color-scheme:dark") && !native.contains("filter"));
        assert_eq!(native, user_stylesheet(Look::Dark));
        let inv = user_stylesheet(Look::Invert);
        assert!(inv.contains("invert(1) hue-rotate(180deg)"));
        assert!(inv.contains("img,picture,video,[style*=\"background-image\" i]"));
        assert!(Look::Native.dark_media() && Look::Dark.dark_media());
        assert!(!Look::Invert.dark_media() && !Look::Light.dark_media());
        assert!(!Look::Light.is_dark() && Look::Invert.is_dark() && Look::Dark.is_dark());
        assert_eq!(card_color(Look::Light), (255, 255, 255));
        assert_eq!(card_color(Look::Native), CARD_DARK);
        assert!(card_color(Look::Invert).0 < 40);
    }
}

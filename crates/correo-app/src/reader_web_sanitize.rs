//! Saneado previo del HTML para la vista completa (WebKit) y heurística de «¿hace falta WebKit?».
//! Funciones puras, sin GTK ni WebKit. El saneado es una capa más: además, el WebView no
//! ejecuta JavaScript, bloquea la red con un filtro de contenido y la propia página lleva una
//! política CSP restrictiva.
//!
//! Se conserva la maquetación (tablas, estilos en línea, `<style>`, imágenes) y se eliminan
//! scripts, formularios, marcos, `<meta>`, `<base>`, `<link>`, SVG y manejadores de eventos.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use super::reader_web_policy::scheme_of;

/// Etiquetas de maquetación permitidas. Sin script, form, input, button, iframe, object, embed,
/// meta, link, base, svg ni video/audio.
const TAGS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "article",
    "aside",
    "b",
    "bdi",
    "bdo",
    "big",
    "blockquote",
    "br",
    "caption",
    "center",
    "cite",
    "code",
    "col",
    "colgroup",
    "dd",
    "del",
    "dfn",
    "div",
    "dl",
    "dt",
    "em",
    "figcaption",
    "figure",
    "font",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "i",
    "img",
    "ins",
    "kbd",
    "li",
    "main",
    "mark",
    "nav",
    "ol",
    "p",
    "pre",
    "q",
    "s",
    "samp",
    "section",
    "small",
    "span",
    "strike",
    "strong",
    "style",
    "sub",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "tt",
    "u",
    "ul",
    "var",
    "wbr",
];

/// Etiquetas cuyo contenido se descarta (si no, su texto quedaría visible en el cuerpo).
const DROP_CONTENT: &[&str] = &[
    "script", "title", "template", "noscript", "iframe", "object", "embed", "svg", "math",
    "noembed", "noframes", "textarea", "applet", "select", "option",
];

const GENERIC_ATTRS: &[&str] = &[
    "class", "id", "style", "title", "lang", "dir", "align", "valign", "bgcolor", "width", "height",
];

/// Imagen que sustituye a las `cid:` (adjuntos en línea) hasta que se resuelvan.
/// TODO(cid): `MessageBody.attachments` solo trae metadatos (nombre, tipo, tamaño, content-id),
/// no los bytes; resolverlas exigiría otra descarga IMAP del adjunto. Cuando el cuerpo
/// cacheado incluya los bytes, sustituir aquí `cid:` por un `data:` con la imagen.
const CID_PLACEHOLDER: &str = "data:image/svg+xml;utf8,%3Csvg xmlns='http://www.w3.org/2000/svg' width='48' height='48'%3E%3Crect width='48' height='48' fill='%23e5e5e5'/%3E%3C/svg%3E";

/// Estilos base de la página. El `html` no lleva fondo para que el del `body` del correo
/// (si lo declara) cubra todo el lienzo; el WebView es blanco por defecto.
const BASE_CSS: &str = "body{margin:0;color:#1e1e1e;\
font:15px/1.5 Cantarell,'Adwaita Sans',system-ui,sans-serif;overflow-wrap:anywhere;\
-webkit-text-size-adjust:100%}\
#correo-pad{padding:16px 20px 28px}\
img{max-width:100%;height:auto}\
table{max-width:100%}pre{white-space:pre-wrap}\
blockquote{margin:.6em 0 .6em .4em;padding-left:.9em;border-left:3px solid #c0bfbc;color:#5e5c64}\
a[href]{color:#1c71d8}";

/// Sanea el HTML del correo para mostrarlo en el WebView. Devuelve un fragmento (sin
/// `<html>`/`<head>`/`<body>`); `build_document` lo envuelve.
pub fn sanitize_for_web(html: &str) -> String {
    let mut builder = ammonia::Builder::empty();
    builder
        .tags(TAGS.iter().copied().collect::<HashSet<_>>())
        .clean_content_tags(DROP_CONTENT.iter().copied().collect::<HashSet<_>>())
        .generic_attributes(GENERIC_ATTRS.iter().copied().collect::<HashSet<_>>())
        .tag_attributes(tag_attributes())
        .url_schemes(
            ["http", "https", "mailto", "data", "cid"]
                .into_iter()
                .collect::<HashSet<_>>(),
        )
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true)
        .attribute_filter(filter_attribute);
    let clean = builder.clean(html).to_string();
    scrub_style_elements(&clean)
}

fn tag_attributes() -> HashMap<&'static str, HashSet<&'static str>> {
    let set = |names: &[&'static str]| names.iter().copied().collect::<HashSet<_>>();
    HashMap::from([
        ("a", set(&["href", "name"])),
        ("img", set(&["src", "alt", "border", "hspace", "vspace"])),
        (
            "table",
            set(&["border", "cellpadding", "cellspacing", "summary"]),
        ),
        (
            "td",
            set(&["colspan", "rowspan", "nowrap", "scope", "headers"]),
        ),
        (
            "th",
            set(&["colspan", "rowspan", "nowrap", "scope", "headers"]),
        ),
        ("col", set(&["span"])),
        ("colgroup", set(&["span"])),
        ("font", set(&["color", "face", "size"])),
        ("ol", set(&["start", "type", "reversed"])),
        ("ul", set(&["type"])),
        ("li", set(&["value"])),
        ("hr", set(&["noshade", "size"])),
        ("time", set(&["datetime"])),
    ])
}

/// Filtro de valores de atributos: enlaces solo http/https/mailto, imágenes solo http(s) o
/// `data:image` raster (nunca SVG), `cid:` sustituido, y CSS en línea depurado.
fn filter_attribute<'a>(element: &str, attr: &str, value: &'a str) -> Option<Cow<'a, str>> {
    match (element, attr) {
        (_, "style") => Some(Cow::Owned(scrub_css(value))),
        ("a", "href") => matches!(scheme_of(value).as_str(), "http" | "https" | "mailto")
            .then(|| Cow::Borrowed(value.trim())),
        ("img", "src") => match scheme_of(value).as_str() {
            "http" | "https" => Some(Cow::Borrowed(value.trim())),
            "data" if is_raster_data_uri(value) => Some(Cow::Borrowed(value.trim())),
            "cid" => Some(Cow::Borrowed(CID_PLACEHOLDER)),
            _ => None,
        },
        _ => Some(Cow::Borrowed(value)),
    }
}

fn is_raster_data_uri(value: &str) -> bool {
    let lower = value.trim_start().to_ascii_lowercase();
    ["png", "jpeg", "jpg", "gif", "webp", "bmp"]
        .iter()
        .any(|t| lower.starts_with(&format!("data:image/{t}")))
}

/// Depura un fragmento de CSS (atributo `style` o contenido de `<style>`): quita comentarios y
/// escapes, `@import`, `url()` que no sean `data:image` raster, `image-set()` y similares,
/// `expression()`, `behavior` y `-moz-binding`. Defensa en profundidad: la red ya está
/// bloqueada por el filtro de contenido y la CSP.
pub fn scrub_css(css: &str) -> String {
    // Sin comentarios ni escapes (`\75rl(` no puede esquivar la búsqueda de `url(`).
    let mut text = String::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut prev = '\0';
            for d in chars.by_ref() {
                if prev == '*' && d == '/' {
                    break;
                }
                prev = d;
            }
        } else if c != '\\' && c != '\0' {
            text.push(c);
        }
    }
    let text = drop_at_rules(&text);
    let text = replace_functions(&text);
    let mut out = String::with_capacity(text.len());
    for decl in split_keeping(&text, ';') {
        let lower = decl.to_ascii_lowercase();
        let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("expression(")
            || compact.contains("-moz-binding")
            || compact.contains("behavior:")
            || compact.contains("javascript:")
        {
            continue;
        }
        out.push_str(decl);
    }
    out
}

/// Parte `text` por `sep` conservando el separador al final de cada trozo.
fn split_keeping(text: &str, sep: char) -> Vec<&str> {
    text.split_inclusive(sep).collect()
}

/// Elimina las reglas `@import ...;` y `@namespace ...;`.
fn drop_at_rules(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    loop {
        let lower = rest.to_ascii_lowercase();
        let hit = ["@import", "@namespace"]
            .iter()
            .filter_map(|k| lower.find(k))
            .min();
        let Some(i) = hit else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        match tail.find([';', '{', '}']) {
            Some(j) if tail.as_bytes()[j] == b';' => rest = &tail[j + 1..],
            // `@import` sin `;` hasta el final del bloque: se descarta hasta ahí.
            Some(j) => rest = &tail[j..],
            None => return out,
        }
    }
}

/// Neutraliza `url(...)` no raster-data y las funciones de imagen que aceptan URLs.
fn replace_functions(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    loop {
        let lower = rest.to_ascii_lowercase();
        let hit = ["url(", "image-set(", "image(", "cross-fade(", "element("]
            .iter()
            .filter_map(|k| lower.find(k).map(|i| (i, *k)))
            .min_by_key(|(i, _)| *i);
        let Some((i, key)) = hit else {
            out.push_str(rest);
            return out;
        };
        // «image(» también casa dentro de «-webkit-image-set(»: se evalúa lo más temprano.
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find(')').map_or(tail.len(), |j| j + 1);
        let call = &tail[..end];
        if key == "url(" && {
            let inner = call[4..]
                .trim_end_matches(')')
                .trim()
                .trim_matches(['"', '\''])
                .trim();
            is_raster_data_uri(inner)
        } {
            out.push_str(call);
        } else {
            out.push_str("none");
        }
        rest = &tail[end..];
    }
}

/// Aplica `scrub_css` al contenido de cada `<style>` del HTML ya saneado.
fn scrub_style_elements(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(open) = lower.find("<style") else {
            out.push_str(rest);
            return out;
        };
        let Some(gt) = rest[open..].find('>') else {
            out.push_str(&rest[..open]);
            return out;
        };
        let body_start = open + gt + 1;
        out.push_str(&rest[..body_start]);
        match lower[body_start..].find("</style") {
            Some(close) => {
                out.push_str(&scrub_css(&rest[body_start..body_start + close]));
                rest = &rest[body_start + close..];
            }
            None => {
                out.push_str(&scrub_css(&rest[body_start..]));
                out.push_str("</style>");
                return out;
            }
        }
    }
}

/// Política de contenido de la página: sin scripts, marcos, objetos, formularios ni `<base>`;
/// solo estilos en línea e imágenes `data:` (y `https:` si el usuario lo ha pedido).
pub fn csp(remote_images: bool) -> String {
    let img = if remote_images {
        "data: https:"
    } else {
        "data:"
    };
    format!(
        "default-src 'none'; img-src {img}; style-src 'unsafe-inline'; script-src 'none'; \
         frame-src 'none'; object-src 'none'; form-action 'none'; base-uri 'none'; \
         connect-src 'none'; font-src 'none'; media-src 'none'"
    )
}

/// Estilo del `<body>` original (atributos `style`, `bgcolor` y `text`), depurado. Como
/// `sanitize_for_web` descarta `<body>`, se reaplica a nuestro propio `<body>` para que el
/// fondo del correo cubra todo el lienzo.
pub fn body_style(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(start) = lower.find("<body") else {
        return String::new();
    };
    let tag_end = lower[start..].find('>').map_or(lower.len(), |i| start + i);
    let (tag, tag_lower) = (&html[start..tag_end], &lower[start..tag_end]);
    let mut out = String::new();
    if let Some(color) = attr_value(tag, tag_lower, "bgcolor").filter(|c| is_color(c)) {
        out.push_str(&format!("background-color:{color};"));
    }
    if let Some(color) = attr_value(tag, tag_lower, "text").filter(|c| is_color(c)) {
        out.push_str(&format!("color:{color};"));
    }
    if let Some(style) = attr_value(tag, tag_lower, "style") {
        out.push_str(&scrub_css(&style));
    }
    // Sin comillas ni `<`: el valor va entre comillas dobles en un atributo.
    out.retain(|c| !matches!(c, '"' | '<' | '>'));
    out
}

/// Valor de `name="..."`/`name='...'` en una etiqueta (`tag_lower` es `tag` en minúsculas).
fn attr_value(tag: &str, tag_lower: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = tag_lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let boundary = tag_lower[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_whitespace());
        let rest = tag_lower[from..].trim_start();
        if !boundary || !rest.starts_with('=') {
            continue;
        }
        let eq = tag_lower[from..].find('=')? + from + 1;
        let value = tag[eq..].trim_start();
        let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'');
        return Some(match quote {
            Some(q) => value[1..].split(q).next()?.to_owned(),
            None => value.split(char::is_whitespace).next()?.to_owned(),
        });
    }
    None
}

fn is_color(value: &str) -> bool {
    let v = value.trim();
    let hex = v.strip_prefix('#').unwrap_or(v);
    !v.is_empty() && v.len() <= 20 && hex.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Ancho fijo (px) más grande declarado en una etiqueta `<table>` (atributo `width` o
/// `width:Npx` en `style`), acotado a 1000. 0 si no hay ninguno. Sirve para encoger el
/// contenido (zoom) en ventanas estrechas en vez de obligar a desplazar en horizontal.
pub fn table_width_hint(html: &str) -> u32 {
    let lower = html.to_ascii_lowercase();
    let mut best = 0;
    let mut rest = lower.as_str();
    while let Some(i) = rest.find("<table") {
        let tag_src = &rest[i..];
        let end = tag_src.find('>').unwrap_or(tag_src.len());
        let tag = &tag_src[..end];
        for key in ["width=\"", "width='", "width:"] {
            let mut t = tag;
            while let Some(k) = t.find(key) {
                let digits: String = t[k + key.len()..]
                    .trim_start()
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                let after = t[k + key.len()..].trim_start()[digits.len()..].trim_start();
                // `width:100%` o `width="100%"` no son anchos fijos.
                if !after.starts_with('%') {
                    if let Ok(n) = digits.parse::<u32>() {
                        best = best.max(n.min(1000));
                    }
                }
                t = &t[k + key.len()..];
            }
        }
        rest = &tag_src[end..];
    }
    best
}

/// Documento completo que se carga en el WebView. `token` va en el `<title>` y permite
/// distinguir la carga de este documento de otras (p. ej. `about:blank`).
pub fn build_document(
    clean_fragment: &str,
    body_style: &str,
    remote_images: bool,
    token: u64,
) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\
         <meta name=\"color-scheme\" content=\"light\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>correo-{token}</title><style>{BASE_CSS}</style></head>\
         <body style=\"{body_style}\"><div id=\"correo-pad\">{clean_fragment}</div></body></html>",
        csp = csp(remote_images)
    )
}

/// Propiedades de estilo en línea que indican maquetación que el visor ligero no pinta.
const LAYOUT_STYLE_HINTS: &[&str] = &[
    "background",
    "border",
    "padding",
    "margin",
    "width",
    "height",
    "float",
    "display",
    "position",
    "line-height",
    "font-size",
    "font-family",
    "text-align",
    "vertical-align",
];

/// `true` si el HTML tiene elementos que el visor ligero no maneja bien (tablas, imágenes,
/// hojas de estilo, maquetación en línea, atributos de color/ancho heredados, SVG o vídeo).
/// Los estilos en línea triviales (solo `color`, `font-weight`…) no cuentan.
pub fn needs_rich_rendering(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    const TAG_HINTS: &[&str] = &[
        "<table", "<img", "<style", "<center", "<font", "<svg", "<video", "<picture", "<hr ",
        "<button", "<iframe", "@media",
    ];
    if TAG_HINTS.iter().any(|t| lower.contains(t)) {
        return true;
    }
    if [" bgcolor=", " background=", " width=\"", " cellpadding="]
        .iter()
        .any(|a| lower.contains(a))
    {
        return true;
    }
    // Atributos style="..." / style='...' con propiedades de maquetación.
    let mut rest = lower.as_str();
    while let Some(i) = rest.find("style=") {
        let after = &rest[i + 6..];
        let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            rest = after;
            continue;
        };
        let value = &after[1..];
        let end = value.find(quote).unwrap_or(value.len());
        if LAYOUT_STYLE_HINTS.iter().any(|h| value[..end].contains(h)) {
            return true;
        }
        rest = &value[end..];
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTILE: &str = r##"<html><head><title>Texto del titulo</title>
<meta http-equiv="refresh" content="0;url=https://evil.example/">
<base href="https://evil.example/">
<link rel="stylesheet" href="https://evil.example/a.css">
<script>alert(1)</script></head>
<body onload="x()" bgcolor="#000">
<iframe src="https://evil.example/f"></iframe><object data="x"></object><embed src="x">
<form action="https://evil.example/post"><input name="p"><button>Enviar</button></form>
<svg onload="x()"><script>1</script></svg>
<a href="javascript:alert(1)" onclick="x()" target="_blank">js</a>
<a href="data:text/html,hola">data</a>
<a href="https://evil.example">https://paypal.com</a>
<img src="https://t.example/p.gif" onerror="x()" srcset="a 1x">
<img src="cid:logo@x"><img src="file:///etc/passwd">
<div style="background:url(https://evil.example/x.png);color:red">hola</div>
<table width="600"><tr><td style="padding:4px">celda</td></tr></table>
</body></html>"##;

    #[test]
    fn hostile_html_is_made_inert() {
        let clean = sanitize_for_web(HOSTILE).to_ascii_lowercase();
        for bad in [
            "<script",
            "<iframe",
            "<object",
            "<embed",
            "<form",
            "<input",
            "<button",
            "<meta",
            "<base",
            "<link",
            "<svg",
            "http-equiv",
            "javascript:",
            "onclick",
            "onerror",
            "onload",
            "target=",
            "srcset",
            "file:",
            "data:text",
            "alert(1)",
            "texto del titulo",
            "evil.example/x.png",
            "evil.example/f",
            "evil.example/post",
        ] {
            assert!(!clean.contains(bad), "quedó «{bad}» en: {clean}");
        }
        // Lo legítimo se conserva.
        assert!(clean.contains("<table"));
        assert!(clean.contains("padding:4px"));
        assert!(clean.contains("color:red"));
        assert!(clean.contains("href=\"https://evil.example\""));
        assert!(clean.contains("https://paypal.com"));
        assert!(clean.contains("rel=\"noopener noreferrer\""));
    }

    #[test]
    fn cid_images_get_a_placeholder() {
        let clean = sanitize_for_web("<img src=\"cid:logo@x\" alt=\"logo\">");
        assert!(!clean.contains("cid:"));
        assert!(clean.contains("data:image/svg+xml"));
    }

    #[test]
    fn only_raster_data_images_survive() {
        let ok = sanitize_for_web("<img src=\"data:image/png;base64,AAAA\">");
        assert!(ok.contains("data:image/png"));
        let svg = sanitize_for_web("<img src=\"data:image/svg+xml;base64,AAAA\">");
        assert!(!svg.contains("svg+xml"));
    }

    #[test]
    fn style_elements_are_kept_but_scrubbed() {
        let clean = sanitize_for_web(
            "<style>@import url(https://e.example/a.css);\
             body{background:#111;color:#eee} .x{background:url('https://e.example/i.png')}\
             .y{b\\61ckground:\\75rl(https://e.example/z)}</style><p class=x>hola</p>",
        );
        assert!(clean.contains("background:#111"));
        assert!(clean.contains("color:#eee"));
        assert!(!clean.to_ascii_lowercase().contains("@import"));
        // Con los escapes eliminados, `\75rl(` queda como un token inerte, nunca como `url(`.
        assert!(!clean.to_ascii_lowercase().contains("url("));
        assert!(!clean.contains("a.css") && !clean.contains("i.png"));
        assert!(clean.contains("<p class=\"x\">"));
    }

    #[test]
    fn scrub_css_handles_tricks() {
        assert_eq!(scrub_css("color:red"), "color:red");
        assert!(!scrub_css("background:URL( 'http://x/y' )").contains("http"));
        assert!(!scrub_css("background:u/**/rl(http://x/y)").contains("http"));
        assert!(!scrub_css("background:-webkit-image-set(\"http://x/y\" 1x)").contains("http"));
        assert!(!scrub_css("width:expression(alert(1));color:red").contains("expression"));
        assert!(scrub_css("width:expression(alert(1));color:red").contains("color:red"));
        assert!(scrub_css("background:url(data:image/png;base64,AAAA)").contains("data:image/png"));
        assert!(!scrub_css("background:url(data:image/svg+xml;base64,AAAA)").contains("svg"));
        assert!(!scrub_css("-moz-binding:url(x)").contains("binding"));
    }

    #[test]
    fn document_has_strict_csp_and_token() {
        let doc = build_document("<p>hola</p>", "color:red;", false, 7);
        assert!(doc.contains("default-src 'none'"));
        assert!(doc.contains("script-src 'none'"));
        assert!(doc.contains("frame-src 'none'"));
        assert!(doc.contains("form-action 'none'"));
        assert!(doc.contains("base-uri 'none'"));
        assert!(doc.contains("img-src data:;"));
        assert!(doc.contains("<title>correo-7</title>"));
        assert!(doc.contains(
            "<body style=\"color:red;\"><div id=\"correo-pad\"><p>hola</p></div></body>"
        ));
        assert!(build_document("", "", true, 1).contains("img-src data: https:;"));
        // Los cuerpos con `</style>` o scripts quedan ya saneados antes de envolverse.
        assert!(!csp(false).contains("https:"));
    }

    #[test]
    fn body_style_is_kept_and_scrubbed() {
        let html = "<html><body bgcolor=\"#101010\" text=#eeeeee style=\"background:url(http://x/y);margin:0\"><p>x</p>";
        let style = body_style(html);
        assert!(style.contains("background-color:#101010;"));
        assert!(style.contains("color:#eeeeee;"));
        assert!(style.contains("margin:0"));
        assert!(!style.contains("http"));
        assert_eq!(body_style("<p>sin body</p>"), "");
        // Sin comillas que rompan el atributo.
        assert!(!body_style("<body bgcolor='red\"onload=x'>").contains('"'));
    }

    #[test]
    fn table_width_hint_ignores_percentages_and_caps() {
        assert_eq!(
            table_width_hint("<table width=\"600\"><tr><td width=\"900\">"),
            600
        );
        assert_eq!(
            table_width_hint("<table style=\"width: 640px;max-width:100%\"><table width=\"100%\">"),
            640
        );
        assert_eq!(table_width_hint("<table width=\"100%\"><p>"), 0);
        assert_eq!(table_width_hint("<table width=\"5000\">"), 1000);
        assert_eq!(table_width_hint("<p style=\"width:300px\">"), 0);
    }

    #[test]
    fn rich_rendering_heuristic() {
        assert!(needs_rich_rendering("<table><tr><td>x</td></tr></table>"));
        assert!(needs_rich_rendering("<p>Hola <IMG src=x></p>"));
        assert!(needs_rich_rendering("<style>p{color:red}</style><p>x</p>"));
        assert!(needs_rich_rendering(
            "<div style=\"padding: 10px\">hola</div>"
        ));
        assert!(needs_rich_rendering("<p style='background:#fff'>x</p>"));
        assert!(needs_rich_rendering("<td bgcolor=\"#fff\">x</td>"));
        assert!(!needs_rich_rendering("<p>Hola <b>mundo</b></p><br>"));
        assert!(!needs_rich_rendering(
            "<p style=\"color:red;font-weight:bold\">hola</p>"
        ));
        assert!(!needs_rich_rendering(
            "<blockquote><p>cita</p></blockquote><a href=\"https://x.org\">x</a>"
        ));
        assert!(!needs_rich_rendering(""));
    }
}

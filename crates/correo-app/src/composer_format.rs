//! Modelo intermedio del cuerpo de un mensaje y conversión pura a texto plano y HTML.
//!
//! No depende de GTK: el editor (`composer_editor.rs`) traduce su `TextBuffer` a
//! este modelo, y de aquí salen el `text` y el `html` del `Draft`.

/// Tipo de párrafo (una línea del editor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParaKind {
    #[default]
    Normal,
    Bullet,
    Quote,
}

/// Formato de un tramo de texto.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    pub text: String,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Paragraph {
    pub kind: ParaKind,
    pub runs: Vec<Run>,
}

impl Paragraph {
    /// Párrafo de texto sin formato.
    pub fn plain(kind: ParaKind, text: &str) -> Self {
        let runs = if text.is_empty() {
            Vec::new()
        } else {
            vec![Run {
                text: text.to_owned(),
                style: Style::default(),
            }]
        };
        Self { kind, runs }
    }
}

/// Acepta solo enlaces de navegador o de correo; el resto se descarta por seguridad.
pub fn is_safe_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let scheme_ok = lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:");
    let has_bad_char = url
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''));
    scheme_ok && !has_bad_char && url.len() > "https://".len()
}

/// Normaliza lo que escribe el usuario en el diálogo de enlace.
/// Sin esquema: una dirección con `@` se convierte en `mailto:`, lo demás en `https://`.
pub fn normalize_link(input: &str) -> Option<String> {
    let t = input.trim();
    if t.is_empty() {
        return None;
    }
    let url = if t.contains("://") || t.to_ascii_lowercase().starts_with("mailto:") {
        t.to_owned()
    } else if t.contains('@') && !t.contains('/') && !t.contains(' ') {
        format!("mailto:{t}")
    } else {
        format!("https://{t}")
    };
    is_safe_link(&url).then_some(url)
}

/// Convierte el cuerpo a (texto plano, HTML).
pub fn to_plain_and_html(paras: &[Paragraph]) -> (String, String) {
    (to_plain(paras), to_html(paras))
}

/// Texto plano: viñetas con "- ", citas con "> ", enlaces como `texto (url)`.
pub fn to_plain(paras: &[Paragraph]) -> String {
    let lines: Vec<String> = paras
        .iter()
        .map(|p| {
            let body: String = p.runs.iter().map(run_plain).collect();
            match p.kind {
                ParaKind::Normal => body,
                ParaKind::Bullet => format!("- {body}"),
                ParaKind::Quote if body.is_empty() => ">".to_owned(),
                ParaKind::Quote => format!("> {body}"),
            }
        })
        .collect();
    lines.join("\n").trim_end().to_owned()
}

fn run_plain(run: &Run) -> String {
    match &run.style.link {
        Some(url) if !run.text.is_empty() && run.text != *url => {
            format!("{} ({url})", run.text)
        }
        _ => run.text.clone(),
    }
}

/// HTML: párrafos `<p>`, listas `<ul>` y citas `<blockquote>` agrupadas.
pub fn to_html(paras: &[Paragraph]) -> String {
    let mut out = String::from("<div>");
    let mut i = 0;
    while i < paras.len() {
        match paras[i].kind {
            ParaKind::Normal => {
                out.push_str(&format!("<p>{}</p>", line_html(&paras[i])));
                i += 1;
            }
            ParaKind::Bullet => {
                out.push_str("<ul>");
                while i < paras.len() && paras[i].kind == ParaKind::Bullet {
                    out.push_str(&format!("<li>{}</li>", line_html(&paras[i])));
                    i += 1;
                }
                out.push_str("</ul>");
            }
            ParaKind::Quote => {
                out.push_str("<blockquote type=\"cite\">");
                while i < paras.len() && paras[i].kind == ParaKind::Quote {
                    out.push_str(&format!("<p>{}</p>", line_html(&paras[i])));
                    i += 1;
                }
                out.push_str("</blockquote>");
            }
        }
    }
    out.push_str("</div>");
    out
}

/// Contenido de un párrafo; vacío se representa con `<br>` para que conserve la línea.
fn line_html(p: &Paragraph) -> String {
    let inner: String = p.runs.iter().map(run_html).collect();
    if inner.is_empty() {
        "<br>".to_owned()
    } else {
        inner
    }
}

fn run_html(run: &Run) -> String {
    if run.text.is_empty() {
        return String::new();
    }
    let mut s = escape_html(&run.text);
    let st = &run.style;
    if st.underline {
        s = format!("<u>{s}</u>");
    }
    if st.italic {
        s = format!("<i>{s}</i>");
    }
    if st.bold {
        s = format!("<b>{s}</b>");
    }
    if let Some(url) = &st.link {
        s = format!("<a href=\"{}\">{s}</a>", escape_html(url));
    }
    s
}

pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Cabecera de cita y líneas citadas (cada línea como párrafo `Quote`).
pub fn quote_paragraphs(header: &str, body: &str) -> Vec<Paragraph> {
    let mut paras = vec![Paragraph::plain(ParaKind::Normal, header)];
    paras.extend(body.lines().map(|l| Paragraph::plain(ParaKind::Quote, l)));
    paras
}

/// Convierte HTML a texto legible (para citar mensajes que solo tienen HTML).
pub fn html_to_text(html: &str) -> String {
    let cleaned = strip_between(html, "<!--", "-->");
    let cleaned = strip_between(&cleaned, "<script", "</script>");
    let cleaned = strip_between(&cleaned, "<style", "</style>");
    let chars: Vec<char> = cleaned.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            if let Some(len) = chars[i..].iter().position(|&x| x == '>') {
                let inner: String = chars[i + 1..i + len].iter().collect();
                push_tag(&mut out, &inner);
                i += len + 1;
                continue;
            }
        } else if c == '&' {
            if let Some((decoded, len)) = decode_entity(&chars[i..]) {
                out.push_str(&decoded);
                i += len;
                continue;
            }
        }
        out.push(if c.is_whitespace() { ' ' } else { c });
        i += 1;
    }
    normalize_text(&out)
}

/// Elimina bloques `open`..`close` (ASCII sin distinguir mayúsculas).
fn strip_between(html: &str, open: &str, close: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;
    while let Some(s) = lower[pos..].find(open) {
        let start = pos + s;
        out.push_str(&html[pos..start]);
        match lower[start..].find(close) {
            Some(e) => pos = start + e + close.len(),
            None => {
                pos = html.len();
                break;
            }
        }
    }
    out.push_str(&html[pos..]);
    out
}

fn push_tag(out: &mut String, inner: &str) {
    let closing = inner.trim_start().starts_with('/');
    let name: String = inner
        .trim_start()
        .trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    match name.as_str() {
        "br" => out.push('\n'),
        "li" if !closing => out.push_str("\n- "),
        "li" => {}
        "p" | "div" | "tr" | "blockquote" | "ul" | "ol" | "table" | "pre" | "h1" | "h2" | "h3"
        | "h4" | "h5" | "h6" => out.push('\n'),
        _ => {}
    }
}

fn decode_entity(chars: &[char]) -> Option<(String, usize)> {
    let semi = chars.iter().take(12).position(|&c| c == ';')?;
    let name: String = chars[1..semi].iter().collect();
    let decoded = match name.as_str() {
        "amp" => "&".to_owned(),
        "lt" => "<".to_owned(),
        "gt" => ">".to_owned(),
        "quot" => "\"".to_owned(),
        "apos" => "'".to_owned(),
        "nbsp" => " ".to_owned(),
        n if n.starts_with("#x") || n.starts_with("#X") => {
            char::from_u32(u32::from_str_radix(&n[2..], 16).ok()?)?.to_string()
        }
        n if n.starts_with('#') => char::from_u32(n[1..].parse().ok()?)?.to_string(),
        _ => return None,
    };
    Some((decoded, semi + 1))
}

/// Colapsa espacios, recorta líneas y reduce líneas vacías consecutivas a una.
fn normalize_text(s: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in s.split('\n') {
        let collapsed = line
            .split(' ')
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if collapsed.is_empty() && lines.last().is_some_and(|l| l.is_empty()) {
            continue;
        }
        lines.push(collapsed);
    }
    lines.join("\n").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bold(text: &str) -> Run {
        Run {
            text: text.into(),
            style: Style {
                bold: true,
                ..Style::default()
            },
        }
    }

    fn plain_run(text: &str) -> Run {
        Run {
            text: text.into(),
            style: Style::default(),
        }
    }

    #[test]
    fn plano_sin_formato_no_cambia() {
        let paras = vec![
            Paragraph::plain(ParaKind::Normal, "Hola"),
            Paragraph::plain(ParaKind::Normal, ""),
            Paragraph::plain(ParaKind::Normal, "Adiós"),
        ];
        assert_eq!(to_plain(&paras), "Hola\n\nAdiós");
    }

    #[test]
    fn html_escapa_y_aplica_formato() {
        let paras = vec![Paragraph {
            kind: ParaKind::Normal,
            runs: vec![plain_run("a < b & "), bold("\"c\"")],
        }];
        let html = to_html(&paras);
        assert!(
            html.contains("<p>a &lt; b &amp; <b>&quot;c&quot;</b></p>"),
            "{html}"
        );
    }

    #[test]
    fn html_agrupa_listas_y_citas() {
        let paras = vec![
            Paragraph::plain(ParaKind::Bullet, "uno"),
            Paragraph::plain(ParaKind::Bullet, "dos"),
            Paragraph::plain(ParaKind::Quote, "cita"),
        ];
        let html = to_html(&paras);
        assert!(html.contains("<ul><li>uno</li><li>dos</li></ul>"), "{html}");
        assert!(
            html.contains("<blockquote type=\"cite\"><p>cita</p></blockquote>"),
            "{html}"
        );
    }

    #[test]
    fn plano_de_listas_y_enlaces() {
        let paras = vec![
            Paragraph::plain(ParaKind::Bullet, "item"),
            Paragraph {
                kind: ParaKind::Normal,
                runs: vec![Run {
                    text: "web".into(),
                    style: Style {
                        link: Some("https://ejemplo.org".into()),
                        ..Style::default()
                    },
                }],
            },
        ];
        assert_eq!(to_plain(&paras), "- item\nweb (https://ejemplo.org)");
    }

    #[test]
    fn enlace_no_seguro_se_descarta() {
        assert!(!is_safe_link("javascript:alert(1)"));
        assert!(is_safe_link("https://ejemplo.org/a?b=c"));
        assert_eq!(
            normalize_link("ejemplo.org").as_deref(),
            Some("https://ejemplo.org")
        );
        assert_eq!(
            normalize_link("ana@ejemplo.org").as_deref(),
            Some("mailto:ana@ejemplo.org")
        );
        assert_eq!(normalize_link("   "), None);
    }

    #[test]
    fn cita_de_texto_plano() {
        let paras = quote_paragraphs("El 1/1/2026, Ana escribió:", "Línea 1\n\nLínea 2");
        assert_eq!(
            to_plain(&paras),
            "El 1/1/2026, Ana escribió:\n> Línea 1\n>\n> Línea 2"
        );
    }

    #[test]
    fn html_a_texto() {
        let html = "<html><head><style>p{color:red}</style></head><body>\
            <p>Hola&nbsp;<b>Ana</b> &amp; Luis</p><!-- oculto -->\
            <ul><li>uno</li><li>dos</li></ul><br>Fin &#x41;&#66;</body></html>";
        let text = html_to_text(html);
        assert_eq!(text, "Hola Ana & Luis\n\n- uno\n- dos\n\nFin AB");
    }
}

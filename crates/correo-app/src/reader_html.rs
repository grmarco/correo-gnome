//! Conversión pura de HTML de correo a segmentos de texto con formato (sin GTK, testeable).
//! El HTML pasa primero por `ammonia` (sin scripts, estilos, imágenes ni esquemas peligrosos)
//! y después se convierte a una lista plana de `Segment` que la UI pinta en un `TextBuffer`.

use std::collections::HashSet;

/// Profundidad máxima de cita y de sangría. Acota el número de marcas de cita, la margen del
/// párrafo y el coste de construir el `TextBuffer` ante mensajes con anidamiento absurdo.
pub const MAX_LEVEL: u32 = 8;

/// Marcas de formato aplicadas a un fragmento de texto.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    pub heading: bool,
    /// URL del enlace (si el texto es un enlace).
    pub link: Option<String>,
    /// Nivel de cita (`<blockquote>` anidados o líneas que empiezan por `>`).
    pub quote: u32,
    /// Nivel de sangría de lista.
    pub indent: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Texto con formato. Puede contener `\n` para saltos de línea dentro de un párrafo.
    Text { text: String, style: Style },
    /// Fin de párrafo o bloque.
    Paragraph,
}

const ALLOWED_TAGS: &[&str] = &[
    "a",
    "b",
    "strong",
    "i",
    "em",
    "u",
    "s",
    "del",
    "strike",
    "ins",
    "cite",
    "code",
    "pre",
    "tt",
    "kbd",
    "p",
    "br",
    "hr",
    "div",
    "span",
    "ul",
    "ol",
    "li",
    "blockquote",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "td",
    "th",
    "caption",
    "section",
    "article",
    "header",
    "footer",
    "dl",
    "dt",
    "dd",
];

const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "blockquote",
    "pre",
    "ul",
    "ol",
    "table",
    "tr",
    "caption",
    "section",
    "article",
    "header",
    "footer",
    "dl",
    "dt",
    "dd",
];

/// Convierte HTML de correo en segmentos de texto con formato. Scripts, estilos, imágenes y
/// atributos peligrosos se descartan antes de convertir.
pub fn html_to_segments(html: &str) -> Vec<Segment> {
    let clean = sanitizer().clean(html).to_string();
    convert(&clean)
}

/// Cuenta las etiquetas `<img` del HTML original (para el aviso «Imágenes bloqueadas»).
pub fn count_images(html: &str) -> usize {
    html.to_ascii_lowercase().matches("<img").count()
}

/// Convierte texto plano en segmentos: párrafos separados por líneas vacías, líneas con `>`
/// como citas y enlaces detectados (`http://`, `https://`, `mailto:`).
pub fn plain_to_segments(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut group: Vec<String> = Vec::new();
    let mut group_quote = 0;

    fn flush(out: &mut Vec<Segment>, group: &mut Vec<String>, quote: u32) {
        if group.is_empty() {
            return;
        }
        let style = Style {
            quote,
            ..Style::default()
        };
        for (i, line) in group.iter().enumerate() {
            if i > 0 {
                out.push(Segment::Text {
                    text: "\n".into(),
                    style: style.clone(),
                });
            }
            out.extend(linkify(line, &style));
        }
        out.push(Segment::Paragraph);
        group.clear();
    }

    for line in text.lines() {
        let (quote, body) = strip_quote(line);
        if body.trim().is_empty() {
            flush(&mut out, &mut group, group_quote);
            continue;
        }
        if quote != group_quote {
            flush(&mut out, &mut group, group_quote);
            group_quote = quote;
        }
        group.push(body.trim_end().to_string());
    }
    flush(&mut out, &mut group, group_quote);
    out
}

fn sanitizer() -> ammonia::Builder<'static> {
    let mut builder = ammonia::Builder::default();
    builder
        .tags(ALLOWED_TAGS.iter().copied().collect::<HashSet<_>>())
        .clean_content_tags(
            [
                "script", "style", "head", "title", "template", "noscript", "iframe", "object",
                "embed", "svg", "math",
            ]
            .into_iter()
            .collect::<HashSet<_>>(),
        )
        .url_schemes(
            ["http", "https", "mailto"]
                .into_iter()
                .collect::<HashSet<_>>(),
        );
    builder
}

/// Quita los prefijos `>` de una línea y devuelve (nivel de cita, resto).
fn strip_quote(line: &str) -> (u32, &str) {
    let mut rest = line;
    let mut level = 0;
    loop {
        let trimmed = rest.trim_start();
        match trimmed.strip_prefix('>') {
            Some(after) => {
                level += 1;
                rest = after;
            }
            None => break,
        }
    }
    if level == 0 {
        return (0, line);
    }
    (level.min(MAX_LEVEL), rest.strip_prefix(' ').unwrap_or(rest))
}

/// Separa el texto en segmentos, convirtiendo las URL en enlaces.
fn linkify(text: &str, base: &Style) -> Vec<Segment> {
    const PREFIXES: [&str; 3] = ["https://", "http://", "mailto:"];
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let found = PREFIXES
            .iter()
            .filter_map(|p| rest.find(p).map(|i| (i, *p)))
            .min_by_key(|(i, _)| *i);
        let Some((start, prefix)) = found else { break };
        let url_len = rest[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"'))
            .unwrap_or(rest.len() - start);
        let raw = &rest[start..start + url_len];
        let url = raw.trim_end_matches(|c: char| ".,;:!?)]'".contains(c));
        if url.len() <= prefix.len() {
            // Solo el prefijo: se trata como texto normal.
            out.push(text_segment(&rest[..start + prefix.len()], base));
            rest = &rest[start + prefix.len()..];
            continue;
        }
        if start > 0 {
            out.push(text_segment(&rest[..start], base));
        }
        let style = Style {
            link: Some(url.to_string()),
            ..base.clone()
        };
        out.push(Segment::Text {
            text: url.to_string(),
            style,
        });
        rest = &rest[start + url.len()..];
    }
    if !rest.is_empty() {
        out.push(text_segment(rest, base));
    }
    out
}

fn text_segment(text: &str, style: &Style) -> Segment {
    Segment::Text {
        text: text.to_string(),
        style: style.clone(),
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Open {
        name: String,
        href: Option<String>,
        void: bool,
    },
    Close(String),
    Text(String),
}

fn tokenize(mut rest: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            tokens.push(Token::Text(rest.to_string()));
            break;
        };
        if lt > 0 {
            tokens.push(Token::Text(rest[..lt].to_string()));
            rest = &rest[lt..];
        }
        if rest.starts_with("<!--") {
            rest = match rest.find("-->") {
                Some(end) => &rest[end + 3..],
                None => "",
            };
            continue;
        }
        let Some(gt) = rest.find('>') else {
            // '<' sin cierre: texto literal.
            tokens.push(Token::Text(rest.to_string()));
            break;
        };
        let body = &rest[1..gt];
        rest = &rest[gt + 1..];
        if body.starts_with('!') || body.starts_with('?') {
            continue;
        }
        if let Some(name) = body.strip_prefix('/') {
            tokens.push(Token::Close(name.trim().to_ascii_lowercase()));
            continue;
        }
        let (name, attrs) = parse_tag(body);
        let href = attrs
            .iter()
            .find(|(k, _)| k == "href")
            .map(|(_, v)| v.clone());
        let void =
            body.trim_end().ends_with('/') || matches!(name.as_str(), "br" | "hr" | "img" | "wbr");
        tokens.push(Token::Open { name, href, void });
    }
    tokens
}

/// Separa nombre y atributos de la parte interior de una etiqueta de apertura.
fn parse_tag(body: &str) -> (String, Vec<(String, String)>) {
    let body = body.trim_end_matches('/');
    let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
    let name = body[..name_end].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut s = body[name_end..].trim_start();
    while !s.is_empty() {
        let key_end = s
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(s.len());
        let key = s[..key_end].to_ascii_lowercase();
        s = s[key_end..].trim_start();
        let value = if let Some(after_eq) = s.strip_prefix('=') {
            let after_eq = after_eq.trim_start();
            let (raw, remain) = match after_eq.chars().next() {
                Some(q @ ('"' | '\'')) => match after_eq[1..].find(q) {
                    Some(end) => (&after_eq[1..1 + end], &after_eq[end + 2..]),
                    None => (&after_eq[1..], ""),
                },
                _ => {
                    let end = after_eq.find(char::is_whitespace).unwrap_or(after_eq.len());
                    (&after_eq[..end], &after_eq[end..])
                }
            };
            s = remain.trim_start();
            decode_entities(raw)
        } else {
            String::new()
        };
        if !key.is_empty() {
            attrs.push((key, value));
        }
    }
    (name, attrs)
}

/// Decodifica entidades HTML habituales (con nombre y numéricas). Las desconocidas se dejan tal cual.
fn decode_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&end| end <= 12).and_then(|end| {
            let entity = &rest[1..end];
            let ch = match entity {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => '\u{a0}',
                _ => {
                    let num = entity.strip_prefix('#')?;
                    let code = match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => num.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((ch, end + 1))
        });
        match decoded {
            Some((ch, used)) => {
                out.push(ch);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Colapsa espacios ASCII (como hace HTML) a un único espacio.
fn collapse_spaces(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_space = false;
    for c in input.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r') {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            in_space = false;
            out.push(c);
        }
    }
    out
}

struct Open {
    name: String,
    href: Option<String>,
}

#[derive(Default)]
struct Converter {
    out: Vec<Segment>,
    para: Vec<Segment>,
    stack: Vec<Open>,
    /// Listas abiertas: (ordenada, último número usado).
    lists: Vec<(bool, usize)>,
    pre: u32,
}

impl Converter {
    fn style(&self) -> Style {
        let mut s = Style {
            indent: depth(self.lists.len()),
            ..Style::default()
        };
        for open in &self.stack {
            match open.name.as_str() {
                "b" | "strong" => s.bold = true,
                "i" | "em" | "cite" => s.italic = true,
                "u" | "ins" => s.underline = true,
                "s" | "del" | "strike" => s.strike = true,
                "code" | "pre" | "tt" | "kbd" => s.mono = true,
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    s.heading = true;
                    s.bold = true;
                }
                "blockquote" => s.quote = (s.quote + 1).min(MAX_LEVEL),
                "a" if open.href.is_some() => s.link = open.href.clone(),
                _ => {}
            }
        }
        s
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let style = self.style();
        let text = if self.pre > 0 {
            text.to_string()
        } else {
            // Un espacio al principio de párrafo o tras otro espacio sobra.
            let ends_with_space = match self.para.last() {
                Some(Segment::Text { text, .. }) => text.ends_with(' ') || text.ends_with('\n'),
                _ => true,
            };
            let collapsed = collapse_spaces(text);
            if ends_with_space {
                collapsed.trim_start_matches(' ').to_string()
            } else {
                collapsed
            }
        };
        if !text.is_empty() {
            self.para.push(Segment::Text { text, style });
        }
    }

    /// Cierra el párrafo actual quitando espacios sobrantes en los extremos.
    fn end_para(&mut self) {
        let mut para = std::mem::take(&mut self.para);
        let is_blank = |s: &Segment| matches!(s, Segment::Text { text, .. } if text.trim_matches([' ', '\n']).is_empty());
        while para.last().is_some_and(is_blank) {
            para.pop();
        }
        if let Some(Segment::Text { text, .. }) = para.last_mut() {
            *text = text.trim_end_matches([' ', '\n']).to_string();
        }
        while para.first().is_some_and(is_blank) {
            para.remove(0);
        }
        if let Some(Segment::Text { text, .. }) = para.first_mut() {
            *text = text.trim_start_matches([' ', '\n']).to_string();
        }
        if para.is_empty() {
            return;
        }
        self.out.append(&mut para);
        self.out.push(Segment::Paragraph);
    }

    fn open(&mut self, name: &str, href: Option<String>, void: bool) {
        match name {
            "br" => self.para.push(Segment::Text {
                text: "\n".into(),
                style: self.style(),
            }),
            "hr" => {
                self.end_para();
                self.para.push(Segment::Text {
                    text: "─".repeat(24),
                    style: self.style(),
                });
                self.end_para();
            }
            _ => {
                if BLOCK_TAGS.contains(&name) {
                    self.end_para();
                }
                match name {
                    "ul" => self.lists.push((false, 0)),
                    "ol" => self.lists.push((true, 0)),
                    "li" => self.list_marker(),
                    "pre" => self.pre += 1,
                    _ => {}
                }
                if !void {
                    self.stack.push(Open {
                        name: name.to_string(),
                        href,
                    });
                }
            }
        }
    }

    fn list_marker(&mut self) {
        let Some((ordered, count)) = self.lists.last_mut() else {
            return;
        };
        let marker = if *ordered {
            *count += 1;
            format!("{}. ", count)
        } else {
            "• ".to_string()
        };
        let style = Style {
            indent: depth(self.lists.len()),
            ..Style::default()
        };
        self.para.push(Segment::Text {
            text: marker,
            style,
        });
    }

    fn close(&mut self, name: &str) {
        if let Some(pos) = self.stack.iter().rposition(|o| o.name == name) {
            self.stack.truncate(pos);
        }
        match name {
            "td" | "th" => self.para.push(Segment::Text {
                text: "   ".into(),
                style: self.style(),
            }),
            "pre" => self.pre = self.pre.saturating_sub(1),
            "ul" | "ol" => {
                self.lists.pop();
            }
            _ => {}
        }
        if BLOCK_TAGS.contains(&name) {
            self.end_para();
        }
    }
}

fn depth(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX).min(MAX_LEVEL)
}

/// Host de una URL `http`/`https` en minúsculas (sin usuario, puerto ni ruta).
pub fn host_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = host_port.split(':').next()?.trim_end_matches('.');
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Dominio que aparenta el texto visible de un enlace: una URL o un dominio desnudo
/// (`www.banco.es/x`). Las palabras sueltas, versiones («v2.0») y otros textos dan `None`.
pub fn shown_host(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    if text.contains("://") {
        return host_of(text);
    }
    let host = text.split(['/', '?', '#']).next()?.trim_end_matches('.');
    let labels: Vec<&str> = host.split('.').collect();
    let tld = labels.last()?;
    let valid = labels.len() >= 2
        && labels
            .iter()
            .all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && tld.len() >= 2
        && tld.chars().all(|c| c.is_ascii_alphabetic());
    valid.then(|| host.to_ascii_lowercase())
}

/// `true` si el texto visible aparenta un dominio distinto del destino real del enlace.
/// Un destino sin host (p. ej. `mailto:`) con texto que parece web también se marca.
pub fn link_looks_deceptive(shown: &str, href: &str) -> bool {
    let Some(shown) = shown_host(shown) else {
        return false;
    };
    let Some(target) = host_of(href) else {
        return true;
    };
    !(target == shown
        || target.ends_with(&format!(".{shown}"))
        || shown.ends_with(&format!(".{target}")))
}

fn convert(clean_html: &str) -> Vec<Segment> {
    let mut conv = Converter::default();
    for token in tokenize(clean_html) {
        match token {
            Token::Text(raw) => conv.push_text(&decode_entities(&raw)),
            Token::Open { name, href, void } => conv.open(&name, href, void),
            Token::Close(name) => conv.close(&name),
        }
    }
    conv.end_para();
    conv.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Segment {
        Segment::Text {
            text: s.into(),
            style: Style::default(),
        }
    }

    fn styled(s: &str, style: Style) -> Segment {
        Segment::Text {
            text: s.into(),
            style,
        }
    }

    fn bold() -> Style {
        Style {
            bold: true,
            ..Style::default()
        }
    }

    #[test]
    fn bold_and_paragraphs() {
        let segs = html_to_segments("<p>Hola <b>mundo</b></p><p>Adiós</p>");
        assert_eq!(
            segs,
            vec![
                text("Hola "),
                styled("mundo", bold()),
                Segment::Paragraph,
                text("Adiós"),
                Segment::Paragraph,
            ]
        );
    }

    #[test]
    fn strong_and_italic_nest() {
        let segs = html_to_segments("<strong>a <em>b</em></strong>");
        assert_eq!(segs[0], styled("a ", bold()));
        assert_eq!(
            segs[1],
            styled(
                "b",
                Style {
                    bold: true,
                    italic: true,
                    ..Style::default()
                }
            )
        );
    }

    #[test]
    fn links_keep_their_url() {
        let segs =
            html_to_segments(r#"<p>Mira <a href="https://example.org/x?a=1&amp;b=2">aquí</a></p>"#);
        let link = Style {
            link: Some("https://example.org/x?a=1&b=2".into()),
            ..Style::default()
        };
        assert_eq!(segs[0], text("Mira "));
        assert_eq!(segs[1], styled("aquí", link));
    }

    #[test]
    fn javascript_links_are_dropped() {
        let segs = html_to_segments(r#"<a href="javascript:alert(1)">pulsa</a>"#);
        assert_eq!(segs, vec![text("pulsa"), Segment::Paragraph]);
    }

    #[test]
    fn lists_get_markers_and_indent() {
        let segs = html_to_segments("<ul><li>uno</li><li>dos</li></ul><ol><li>primero</li></ol>");
        let indented = Style {
            indent: 1,
            ..Style::default()
        };
        assert_eq!(segs[0], styled("• ", indented.clone()));
        assert_eq!(segs[1], styled("uno", indented.clone()));
        assert_eq!(segs[2], Segment::Paragraph);
        assert_eq!(segs[3], styled("• ", indented.clone()));
        assert_eq!(segs[4], styled("dos", indented.clone()));
        assert_eq!(segs[5], Segment::Paragraph);
        assert_eq!(segs[6], styled("1. ", indented.clone()));
        assert_eq!(segs[7], styled("primero", indented));
    }

    #[test]
    fn blockquotes_are_quoted() {
        let segs = html_to_segments("<blockquote><p>cita</p></blockquote><p>fuera</p>");
        let quoted = Style {
            quote: 1,
            ..Style::default()
        };
        assert_eq!(segs[0], styled("cita", quoted));
        assert_eq!(segs[2], text("fuera"));
    }

    #[test]
    fn scripts_and_styles_are_discarded() {
        let segs = html_to_segments(
            "<style>p{color:red}</style><p>visible</p><script>alert('x')</script><p onclick='y()'>ok</p>",
        );
        let all: String = segs
            .iter()
            .filter_map(|s| match s {
                Segment::Text { text, .. } => Some(text.as_str()),
                Segment::Paragraph => None,
            })
            .collect();
        assert_eq!(all, "visibleok");
    }

    #[test]
    fn images_are_removed() {
        let html = r#"<p>a<img src="https://tracker.example/p.gif">b</p>"#;
        assert_eq!(count_images(html), 1);
        let segs = html_to_segments(html);
        assert_eq!(segs, vec![text("ab"), Segment::Paragraph]);
    }

    #[test]
    fn entities_are_decoded() {
        let segs = html_to_segments("<p>&lt;tag&gt; &amp; &quot;q&quot; &#65;&#x42; &nbsp;fin</p>");
        assert_eq!(segs[0], text("<tag> & \"q\" AB \u{a0}fin"));
    }

    #[test]
    fn whitespace_collapses_and_trims() {
        let segs = html_to_segments("<p>  uno \n\n   dos  </p>");
        assert_eq!(segs, vec![text("uno dos"), Segment::Paragraph]);
    }

    #[test]
    fn line_breaks_and_rules() {
        let segs = html_to_segments("<p>a<br>b</p><hr><p>c</p>");
        assert_eq!(segs[0], text("a"));
        assert_eq!(segs[1], text("\n"));
        assert_eq!(segs[2], text("b"));
        assert_eq!(segs[3], Segment::Paragraph);
        assert_eq!(segs[4], text("─".repeat(24).as_str()));
    }

    #[test]
    fn plain_text_detects_links_and_quotes() {
        let segs = plain_to_segments("Visita https://example.org/a.\n\n> citado\n> más");
        assert_eq!(segs[0], text("Visita "));
        assert_eq!(
            segs[1],
            styled(
                "https://example.org/a",
                Style {
                    link: Some("https://example.org/a".into()),
                    ..Style::default()
                }
            )
        );
        assert_eq!(segs[2], text("."));
        assert_eq!(segs[3], Segment::Paragraph);
        assert_eq!(
            segs[4],
            styled(
                "citado",
                Style {
                    quote: 1,
                    ..Style::default()
                }
            )
        );
        assert_eq!(
            segs[5],
            styled(
                "\n",
                Style {
                    quote: 1,
                    ..Style::default()
                }
            )
        );
        assert_eq!(
            segs[6],
            styled(
                "más",
                Style {
                    quote: 1,
                    ..Style::default()
                }
            )
        );
    }

    #[test]
    fn plain_text_without_links_is_one_paragraph_per_block() {
        let segs = plain_to_segments("uno\ndos\n\ntres");
        assert_eq!(
            segs,
            vec![
                text("uno"),
                text("\n"),
                text("dos"),
                Segment::Paragraph,
                text("tres"),
                Segment::Paragraph,
            ]
        );
    }
}

#[cfg(test)]
mod depth_and_link_tests {
    use super::*;

    #[test]
    fn plain_quote_depth_is_capped() {
        let line = format!("{} texto", ">".repeat(100_000));
        let segs = plain_to_segments(&line);
        let Segment::Text { style, .. } = &segs[0] else {
            panic!("se esperaba texto");
        };
        assert_eq!(style.quote, MAX_LEVEL);
    }

    #[test]
    fn nested_blockquotes_and_lists_are_capped() {
        let html = "<blockquote>".repeat(50) + "hondo" + &"</blockquote>".repeat(50);
        let segs = html_to_segments(&html);
        let quoted = segs
            .iter()
            .find_map(|s| match s {
                Segment::Text { text, style } if text == "hondo" => Some(style.clone()),
                _ => None,
            })
            .expect("texto presente");
        assert_eq!(quoted.quote, MAX_LEVEL);

        let lists = "<ul><li>".repeat(40) + "x";
        let segs = html_to_segments(&lists);
        assert!(segs.iter().all(|s| match s {
            Segment::Text { style, .. } => style.indent <= MAX_LEVEL,
            Segment::Paragraph => true,
        }));
    }

    #[test]
    fn host_of_extracts_real_destination() {
        assert_eq!(
            host_of("https://User@Evil.Example:8443/x?y=1").as_deref(),
            Some("evil.example")
        );
        assert_eq!(
            host_of("HTTP://example.org").as_deref(),
            Some("example.org")
        );
        assert_eq!(host_of("mailto:ana@example.org"), None);
        assert_eq!(host_of("javascript://x"), None);
    }

    #[test]
    fn shown_host_recognises_domains_only() {
        assert_eq!(
            shown_host("www.banco.es/login").as_deref(),
            Some("www.banco.es")
        );
        assert_eq!(
            shown_host("https://banco.es/x").as_deref(),
            Some("banco.es")
        );
        assert_eq!(shown_host("aquí"), None);
        assert_eq!(shown_host("pulsa aquí"), None);
        assert_eq!(shown_host("v2.0"), None);
        assert_eq!(shown_host("ana@example.org"), None);
    }

    #[test]
    fn deceptive_links_are_detected() {
        // Texto con dominio distinto del destino.
        assert!(link_looks_deceptive(
            "https://www.paypal.com",
            "https://paypal.com.evil.example/login"
        ));
        assert!(link_looks_deceptive("banco.es", "http://phish.example/"));
        assert!(link_looks_deceptive("https://banco.es", "mailto:x@y.z"));
        // Mismo dominio o subdominio: no se pregunta.
        assert!(!link_looks_deceptive(
            "https://example.org/a",
            "https://example.org/b"
        ));
        assert!(!link_looks_deceptive(
            "example.org",
            "https://www.example.org/"
        ));
        assert!(!link_looks_deceptive(
            "secure.example.org",
            "https://example.org/"
        ));
        // Texto normal no es un dominio.
        assert!(!link_looks_deceptive(
            "pulsa aquí",
            "https://phish.example/"
        ));
    }
}

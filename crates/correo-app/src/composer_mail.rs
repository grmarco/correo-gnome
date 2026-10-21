//! Reglas de correo para el compositor, sin GTK: asunto, destinatarios de respuesta,
//! cabeceras de hilo, validación de direcciones y contenido inicial.

use correo_core::{Address, MessageBody, MessageHeader};

use super::composer_format::{html_to_text, quote_paragraphs, ParaKind, Paragraph};
use crate::ui_types::ComposeMode;

/// Límite total de adjuntos por mensaje.
pub const MAX_ATTACHMENTS_BYTES: u64 = 25 * 1024 * 1024;

pub const BULLET: &str = "• ";

/// Asunto según el modo. No duplica los prefijos ya presentes.
pub fn subject_for(mode: &ComposeMode) -> String {
    match mode {
        ComposeMode::New => String::new(),
        ComposeMode::Reply(h, _) | ComposeMode::ReplyAll(h, _) => {
            with_prefix(&h.subject, "Re:", &["re:"])
        }
        ComposeMode::Forward(h, _) => with_prefix(&h.subject, "Fwd:", &["fwd:", "fw:"]),
    }
}

fn with_prefix(subject: &str, prefix: &str, known: &[&str]) -> String {
    let s = subject.trim();
    let lower = s.to_lowercase();
    if known.iter().any(|k| lower.starts_with(k)) {
        s.to_owned()
    } else {
        format!("{prefix} {s}").trim_end().to_owned()
    }
}

/// Destinatarios iniciales (Para y Cc) según el modo.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recipients {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
}

/// Responder: al remitente (o a `to` si el remitente somos nosotros).
/// Responder a todos: remitente + `to` + `cc`, sin nuestra dirección ni duplicados.
pub fn initial_recipients(mode: &ComposeMode, own_email: &str) -> Recipients {
    let own = own_email.to_lowercase();
    match mode {
        ComposeMode::New | ComposeMode::Forward(..) => Recipients::default(),
        ComposeMode::Reply(h, _) => {
            let mut excluded = vec![own.clone()];
            let mut to = collect(&[&h.from], &mut excluded);
            if to.is_empty() {
                excluded = vec![own];
                to = collect(&[&h.to], &mut excluded);
            }
            Recipients { to, cc: Vec::new() }
        }
        ComposeMode::ReplyAll(h, _) => {
            let mut excluded = vec![own];
            let to = collect(&[&h.from, &h.to], &mut excluded);
            let cc = collect(&[&h.cc], &mut excluded);
            Recipients { to, cc }
        }
    }
}

/// Une listas sin duplicados (sin distinguir mayúsculas), saltando `excluded`.
fn collect(sources: &[&[Address]], excluded: &mut Vec<String>) -> Vec<Address> {
    let mut out = Vec::new();
    for src in sources {
        for addr in src.iter() {
            let key = addr.email.to_lowercase();
            if excluded.contains(&key) {
                continue;
            }
            excluded.push(key);
            out.push(addr.clone());
        }
    }
    out
}

/// (In-Reply-To, References) para el mensaje saliente.
/// Responder: In-Reply-To = Message-ID del original y References = las anteriores + ese.
/// Reenviar: no hay In-Reply-To, pero References sí encadena con el original.
pub fn threading(mode: &ComposeMode) -> (Option<String>, Vec<String>) {
    let (header, reply) = match mode {
        ComposeMode::New => return (None, Vec::new()),
        ComposeMode::Reply(h, _) | ComposeMode::ReplyAll(h, _) => (h, true),
        ComposeMode::Forward(h, _) => (h, false),
    };
    let mut refs = header.references.clone();
    let mut in_reply_to = None;
    if let Some(mid) = header.message_id.as_ref().filter(|m| !m.trim().is_empty()) {
        if reply {
            in_reply_to = Some(mid.clone());
        }
        if !refs.iter().any(|r| r == mid) {
            refs.push(mid.clone());
        }
    }
    (in_reply_to, refs)
}

/// Comprueba si `email` tiene formato razonable de buzón (local@dominio.tld).
pub fn is_valid_email(email: &str) -> bool {
    let s = email.trim();
    if s.len() > 254
        || s.chars()
            .any(|c| c.is_whitespace() || "<>,;\"()[]".contains(c))
    {
        return false;
    }
    let Some((local, domain)) = s.rsplit_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-/=?^_`{|}~.".contains(c));
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && labels.last().is_some_and(|tld| tld.len() >= 2);
    local_ok && domain_ok
}

/// Interpreta un destinatario escrito: `ana@ejemplo.org` o `Ana Pérez <ana@ejemplo.org>`.
pub fn parse_recipient(input: &str) -> Option<Address> {
    let t = input.trim();
    if t.ends_with('>') {
        let lt = t.rfind('<')?;
        let email = t[lt + 1..t.len() - 1].trim();
        if !is_valid_email(email) {
            return None;
        }
        let name = t[..lt].trim().trim_matches('"').trim();
        Some(Address {
            name: (!name.is_empty()).then(|| name.to_owned()),
            email: email.to_owned(),
        })
    } else {
        is_valid_email(t).then(|| Address {
            name: None,
            email: t.to_owned(),
        })
    }
}

/// Nombre a mostrar de una dirección: el nombre si lo hay, si no el correo.
pub fn display_name(addr: &Address) -> &str {
    addr.name
        .as_deref()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(&addr.email)
}

/// "El {fecha}, {nombre} escribió:"
pub fn quote_header(date_text: &str, from: &Address) -> String {
    format!("El {date_text}, {} escribió:", display_name(from))
}

/// Texto citable de un mensaje: texto plano o HTML convertido.
pub fn quotable_text(body: &MessageBody) -> String {
    body.text
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| body.html.as_deref().map(html_to_text))
        .unwrap_or_default()
}

/// Cabecera de reenvío y líneas de metadatos del original.
fn forward_block(h: &MessageHeader, date_text: &str) -> Vec<Paragraph> {
    let from = h.from.first().map(display_name).unwrap_or("").to_owned();
    let to: Vec<&str> = h.to.iter().map(|a| a.email.as_str()).collect();
    let mut lines = vec![
        "---------- Mensaje reenviado ----------".to_owned(),
        format!("De: {from}"),
        format!("Fecha: {date_text}"),
        format!("Asunto: {}", h.subject),
        format!("Para: {}", to.join(", ")),
    ];
    lines.push(String::new());
    lines
        .iter()
        .map(|l| Paragraph::plain(ParaKind::Normal, l))
        .collect()
}

/// Contenido inicial del editor: línea de escritura, firma y, si procede, el mensaje citado.
pub fn initial_paragraphs(
    mode: &ComposeMode,
    signature: Option<&str>,
    date_text: &str,
) -> Vec<Paragraph> {
    let mut paras = vec![Paragraph::plain(ParaKind::Normal, "")];
    if let Some(sig) = signature.map(str::trim_end).filter(|s| !s.is_empty()) {
        paras.push(Paragraph::plain(ParaKind::Normal, ""));
        paras.extend(sig.lines().map(|l| Paragraph::plain(ParaKind::Normal, l)));
    }
    match mode {
        ComposeMode::New => {}
        ComposeMode::Reply(h, body) | ComposeMode::ReplyAll(h, body) => {
            let header = h
                .from
                .first()
                .map(|from| quote_header(date_text, from))
                .unwrap_or_else(|| format!("El {date_text} escribió:"));
            paras.push(Paragraph::plain(ParaKind::Normal, ""));
            paras.extend(quote_paragraphs(&header, &quotable_text(body)));
        }
        ComposeMode::Forward(h, body) => {
            paras.push(Paragraph::plain(ParaKind::Normal, ""));
            paras.extend(forward_block(h, date_text));
            paras.extend(
                quote_paragraphs("", &quotable_text(body))
                    .into_iter()
                    .skip(1),
            );
        }
    }
    paras
}

/// ¿Cabe `extra` bytes más en el límite de adjuntos, con `current` ya usados?
pub fn fits_attachment_limit(current: u64, extra: u64) -> bool {
    current.saturating_add(extra) <= MAX_ATTACHMENTS_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    use correo_core::Flags;

    fn addr(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_owned),
            email: email.to_owned(),
        }
    }

    fn header(from: Address, to: Vec<Address>, cc: Vec<Address>) -> MessageHeader {
        MessageHeader {
            id: 1,
            mailbox_id: 1,
            uid: 7,
            message_id: Some("<orig@ejemplo.org>".into()),
            in_reply_to: None,
            references: vec!["<raiz@ejemplo.org>".into()],
            subject: "Presupuesto Q3".into(),
            from: vec![from],
            to,
            cc,
            date: 0,
            flags: Flags::default(),
            has_attachments: false,
            snippet: None,
            labels: vec![],
        }
    }

    #[test]
    fn asunto_sin_duplicar_prefijos() {
        let h = header(addr(None, "a@b.es"), vec![], vec![]);
        let reply = ComposeMode::Reply(h.clone(), MessageBody::default());
        assert_eq!(subject_for(&reply), "Re: Presupuesto Q3");

        let mut re = h.clone();
        re.subject = "RE: Presupuesto Q3".into();
        let reply = ComposeMode::Reply(re, MessageBody::default());
        assert_eq!(subject_for(&reply), "RE: Presupuesto Q3");

        let mut fwd = h.clone();
        fwd.subject = "Fwd: Presupuesto Q3".into();
        assert_eq!(
            subject_for(&ComposeMode::Forward(fwd, MessageBody::default())),
            "Fwd: Presupuesto Q3"
        );
        assert_eq!(
            subject_for(&ComposeMode::Forward(h, MessageBody::default())),
            "Fwd: Presupuesto Q3"
        );
        assert_eq!(subject_for(&ComposeMode::New), "");
    }

    #[test]
    fn reply_all_excluye_propia_y_duplicados() {
        let me = "yo@mio.es";
        let h = header(
            addr(Some("Ana"), "ana@ejemplo.org"),
            vec![addr(None, "YO@mio.es"), addr(None, "bea@ejemplo.org")],
            vec![
                addr(None, "carlos@ejemplo.org"),
                addr(None, "bea@ejemplo.org"),
                addr(None, "ana@ejemplo.org"),
            ],
        );
        let r = initial_recipients(&ComposeMode::ReplyAll(h, MessageBody::default()), me);
        let to: Vec<&str> = r.to.iter().map(|a| a.email.as_str()).collect();
        let cc: Vec<&str> = r.cc.iter().map(|a| a.email.as_str()).collect();
        assert_eq!(to, ["ana@ejemplo.org", "bea@ejemplo.org"]);
        assert_eq!(cc, ["carlos@ejemplo.org"]);
    }

    #[test]
    fn reply_va_al_remitente_y_forward_no_tiene_destinatarios() {
        let h = header(
            addr(None, "ana@ejemplo.org"),
            vec![addr(None, "yo@mio.es")],
            vec![addr(None, "carlos@ejemplo.org")],
        );
        let r = initial_recipients(
            &ComposeMode::Reply(h.clone(), MessageBody::default()),
            "yo@mio.es",
        );
        assert_eq!(r.to, vec![addr(None, "ana@ejemplo.org")]);
        assert!(r.cc.is_empty());
        let f = initial_recipients(
            &ComposeMode::Forward(h, MessageBody::default()),
            "yo@mio.es",
        );
        assert_eq!(f, Recipients::default());
    }

    #[test]
    fn hilos_in_reply_to_y_references() {
        let h = header(addr(None, "ana@ejemplo.org"), vec![], vec![]);
        let (irt, refs) = threading(&ComposeMode::Reply(h.clone(), MessageBody::default()));
        assert_eq!(irt.as_deref(), Some("<orig@ejemplo.org>"));
        assert_eq!(refs, ["<raiz@ejemplo.org>", "<orig@ejemplo.org>"]);

        let (irt, refs) = threading(&ComposeMode::Forward(h, MessageBody::default()));
        assert_eq!(irt, None);
        assert_eq!(refs.len(), 2);
        assert_eq!(threading(&ComposeMode::New), (None, vec![]));
    }

    #[test]
    fn cita_de_respuesta_con_firma() {
        let h = header(addr(Some("Ana Pérez"), "ana@ejemplo.org"), vec![], vec![]);
        let mode = ComposeMode::Reply(
            h,
            MessageBody {
                text: Some("Hola\nTodo bien".into()),
                ..MessageBody::default()
            },
        );
        let paras = initial_paragraphs(&mode, Some("Un saludo\n"), "21/10/2026 09:30");
        let plain = crate::composer::composer_format::to_plain(&paras);
        assert_eq!(
            plain,
            "\n\nUn saludo\n\nEl 21/10/2026 09:30, Ana Pérez escribió:\n> Hola\n> Todo bien"
        );
    }

    #[test]
    fn cita_solo_html_se_convierte() {
        let body = MessageBody {
            html: Some("<p>Hola <b>Luis</b></p>".into()),
            ..MessageBody::default()
        };
        assert_eq!(quotable_text(&body), "Hola Luis");
    }

    #[test]
    fn validacion_de_email() {
        assert!(is_valid_email("ana.perez+tag@ejemplo.org"));
        assert!(is_valid_email("  ana@sub.ejemplo.es "));
        assert!(!is_valid_email("ana@ejemplo"));
        assert!(!is_valid_email("ana@@ejemplo.org"));
        assert!(!is_valid_email("ana perez@ejemplo.org"));
        assert!(!is_valid_email("@ejemplo.org"));
        assert!(!is_valid_email("ana..b@ejemplo.org"));
        assert!(!is_valid_email("ana@-ejemplo.org"));
        assert!(!is_valid_email("ana@ejemplo.o"));
        assert!(!is_valid_email(""));
    }

    #[test]
    fn parseo_de_destinatario() {
        assert_eq!(
            parse_recipient("Ana Pérez <ana@ejemplo.org>"),
            Some(addr(Some("Ana Pérez"), "ana@ejemplo.org"))
        );
        assert_eq!(
            parse_recipient("\"Ana\" <ana@ejemplo.org>"),
            Some(addr(Some("Ana"), "ana@ejemplo.org"))
        );
        assert_eq!(
            parse_recipient("<ana@ejemplo.org>"),
            Some(addr(None, "ana@ejemplo.org"))
        );
        assert_eq!(parse_recipient("Ana <no-valido>"), None);
        assert_eq!(parse_recipient("<>"), None);
    }

    #[test]
    fn limite_de_adjuntos() {
        assert!(fits_attachment_limit(0, MAX_ATTACHMENTS_BYTES));
        assert!(!fits_attachment_limit(MAX_ATTACHMENTS_BYTES, 1));
    }
}

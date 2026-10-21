//! Funciones puras (sin red ni estado): roles de carpeta, conversión de
//! cabeceras y BODYSTRUCTURE, parseo de cuerpos, cadena XOAUTH2 y planificación
//! de sincronización. Todo aquí es testeable sin servidor.

use std::collections::BTreeSet;

use async_imap::imap_proto::{self, BodyStructure, Envelope, NameAttribute};
use async_imap::types::Flag;
use correo_core::{Address, Attachment, Flags, MailboxId, MailboxRole, MessageBody, MessageHeader};
use mail_parser::{Addr, HeaderValue, MessageParser, MessagePart, MimeHeaders};

/// Tamaño de lote para UID FETCH / UID STORE.
pub const BATCH_SIZE: usize = 200;

const SECONDS_PER_DAY: i64 = 86_400;

// ----- Carpetas ----------------------------------------------------------

/// Convierte los atributos de un `NameAttribute` de LIST a su forma textual IMAP.
pub fn attribute_strings(attrs: &[NameAttribute<'_>]) -> Vec<String> {
    attrs
        .iter()
        .map(|a| match a {
            NameAttribute::NoInferiors => "\\Noinferiors".to_owned(),
            NameAttribute::NoSelect => "\\Noselect".to_owned(),
            NameAttribute::Marked => "\\Marked".to_owned(),
            NameAttribute::Unmarked => "\\Unmarked".to_owned(),
            NameAttribute::All => "\\All".to_owned(),
            NameAttribute::Archive => "\\Archive".to_owned(),
            NameAttribute::Drafts => "\\Drafts".to_owned(),
            NameAttribute::Flagged => "\\Flagged".to_owned(),
            NameAttribute::Junk => "\\Junk".to_owned(),
            NameAttribute::Sent => "\\Sent".to_owned(),
            NameAttribute::Trash => "\\Trash".to_owned(),
            NameAttribute::Extension(s) => s.to_string(),
            _ => String::new(),
        })
        .collect()
}

/// Rol de una carpeta: primero por atributos especiales (RFC 6154), y si no
/// hay ninguno, por el nombre de su último segmento.
pub fn mailbox_role(attributes: &[String], name: &str) -> MailboxRole {
    for attr in attributes {
        let role = match attr.to_ascii_lowercase().as_str() {
            "\\inbox" => Some(MailboxRole::Inbox),
            "\\sent" => Some(MailboxRole::Sent),
            "\\drafts" => Some(MailboxRole::Drafts),
            "\\trash" => Some(MailboxRole::Trash),
            "\\junk" => Some(MailboxRole::Spam),
            "\\archive" => Some(MailboxRole::Archive),
            _ => None,
        };
        if let Some(role) = role {
            return role;
        }
    }
    role_from_name(name)
}

/// Respaldo por nombre (en inglés y español) para servidores sin atributos.
pub fn role_from_name(name: &str) -> MailboxRole {
    if name.eq_ignore_ascii_case("INBOX") {
        return MailboxRole::Inbox;
    }
    let leaf = name
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .trim()
        .to_lowercase();
    match leaf.as_str() {
        "sent" | "sent items" | "sent mail" | "enviados" | "enviado" | "elementos enviados"
        | "correo enviado" => MailboxRole::Sent,
        "drafts" | "draft" | "borradores" => MailboxRole::Drafts,
        "trash"
        | "deleted"
        | "deleted items"
        | "deleted messages"
        | "bin"
        | "papelera"
        | "elementos eliminados"
        | "correo eliminado" => MailboxRole::Trash,
        "spam" | "junk" | "junk e-mail" | "bulk mail" | "correo no deseado" => MailboxRole::Spam,
        "archive" | "archives" | "archivo" | "archivado" | "archivados" => MailboxRole::Archive,
        _ => MailboxRole::Other,
    }
}

/// Indica si una carpeta puede seleccionarse (no tiene \Noselect).
pub fn is_selectable(attributes: &[String]) -> bool {
    !attributes
        .iter()
        .any(|a| a.eq_ignore_ascii_case("\\Noselect"))
}

// ----- Autenticación -----------------------------------------------------

/// Respuesta inicial XOAUTH2 (RFC de Google/Microsoft), sin codificar en base64.
pub fn xoauth2_initial_response(email: &str, access_token: &str) -> Vec<u8> {
    format!("user={email}\x01auth=Bearer {access_token}\x01\x01").into_bytes()
}

// ----- Flags -------------------------------------------------------------

pub fn flags_from<'a>(flags: impl IntoIterator<Item = Flag<'a>>) -> Flags {
    let mut out = Flags::default();
    for flag in flags {
        match flag {
            Flag::Seen => out.seen = true,
            Flag::Flagged => out.flagged = true,
            Flag::Answered => out.answered = true,
            Flag::Draft => out.draft = true,
            _ => {}
        }
    }
    out
}

pub fn same_flags(a: &Flags, b: &Flags) -> bool {
    a.seen == b.seen && a.flagged == b.flagged && a.answered == b.answered && a.draft == b.draft
}

// ----- Conjuntos de UID y planificación de sync -------------------------

/// Convierte UIDs en un conjunto IMAP compacto, p. ej. `1:3,7,9:10`.
pub fn uid_set(uids: &[u32]) -> String {
    let mut sorted: Vec<u32> = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut parts = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        if start == end {
            parts.push(start.to_string());
        } else {
            parts.push(format!("{start}:{end}"));
        }
        i += 1;
    }
    parts.join(",")
}

/// Resultado de comparar el servidor con la caché.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncPlan {
    /// En el servidor (dentro de la ventana) y no en caché: descargar cabeceras.
    pub new_uids: Vec<u32>,
    /// En el servidor y en caché: refrescar FLAGS.
    pub known_uids: Vec<u32>,
    /// En caché dentro de la ventana y ya no en el servidor: borrar de caché.
    pub removed_uids: Vec<u32>,
}

/// `server`: UIDs que devuelve SEARCH SINCE. `cached_all`: todos los UIDs en
/// caché de la carpeta. `cached_in_window`: los de caché dentro de la ventana.
pub fn plan_sync(
    server: &BTreeSet<u32>,
    cached_all: &BTreeSet<u32>,
    cached_in_window: &BTreeSet<u32>,
) -> SyncPlan {
    SyncPlan {
        new_uids: server.difference(cached_all).copied().collect(),
        known_uids: server.intersection(cached_all).copied().collect(),
        removed_uids: cached_in_window.difference(server).copied().collect(),
    }
}

// ----- Fechas ------------------------------------------------------------

/// Días desde 1970-01-01 a (año, mes, día). Algoritmo de H. Hinnant.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Fecha en formato IMAP (`dd-Mon-yyyy`) para SEARCH SINCE.
pub fn imap_date(days: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, m, d) = civil_from_days(days);
    format!("{d:02}-{}-{y}", MONTHS[(m - 1) as usize])
}

/// Días desde epoch de hoy (UTC).
pub fn today_days(now_unix: i64) -> i64 {
    now_unix.div_euclid(SECONDS_PER_DAY)
}

// ----- Cabeceras ---------------------------------------------------------

/// Datos de un mensaje que no están en la cabecera RFC 5322.
#[derive(Debug, Clone)]
pub struct FetchMeta {
    pub mailbox_id: MailboxId,
    pub uid: u32,
    pub flags: Flags,
    /// INTERNALDATE en segundos Unix (se usa como fecha del mensaje).
    pub internal_date: i64,
    pub has_attachments: bool,
    pub labels: Vec<String>,
}

/// Construye la cabecera a partir de `BODY.PEEK[HEADER.FIELDS (...)]`. Si no
/// vino ese bloque, usa ENVELOPE como respaldo.
pub fn build_header(
    raw_headers: Option<&[u8]>,
    envelope: Option<&Envelope<'_>>,
    meta: FetchMeta,
) -> MessageHeader {
    let mut h = MessageHeader {
        id: 0,
        mailbox_id: meta.mailbox_id,
        uid: meta.uid,
        message_id: None,
        in_reply_to: None,
        references: Vec::new(),
        subject: String::new(),
        from: Vec::new(),
        to: Vec::new(),
        cc: Vec::new(),
        date: meta.internal_date,
        flags: meta.flags,
        has_attachments: meta.has_attachments,
        snippet: None,
        labels: meta.labels,
    };
    if let Some(raw) = raw_headers {
        fill_from_raw_headers(&mut h, raw);
    } else if let Some(env) = envelope {
        fill_from_envelope(&mut h, env);
    }
    h
}

fn fill_from_raw_headers(h: &mut MessageHeader, raw: &[u8]) {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return;
    };
    h.message_id = msg.message_id().map(str::to_owned);
    h.in_reply_to = header_first_id(msg.in_reply_to());
    h.references = header_id_list(msg.references());
    h.subject = msg.subject().unwrap_or_default().to_owned();
    h.from = msg.from().map(mail_addresses).unwrap_or_default();
    h.to = msg.to().map(mail_addresses).unwrap_or_default();
    h.cc = msg.cc().map(mail_addresses).unwrap_or_default();
}

fn header_first_id(value: &HeaderValue<'_>) -> Option<String> {
    match value {
        HeaderValue::Text(t) => first_token(t),
        HeaderValue::TextList(list) => list.first().and_then(|t| first_token(t)),
        _ => None,
    }
}

fn header_id_list(value: &HeaderValue<'_>) -> Vec<String> {
    match value {
        HeaderValue::Text(t) => t
            .split_whitespace()
            .map(|s| s.trim_matches(['<', '>']).to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
        HeaderValue::TextList(list) => list
            .iter()
            .map(|s| s.trim_matches(['<', '>']).to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn first_token(text: &str) -> Option<String> {
    text.split_whitespace()
        .next()
        .map(|s| s.trim_matches(['<', '>']).to_owned())
        .filter(|s| !s.is_empty())
}

fn mail_addresses(addr: &mail_parser::Address<'_>) -> Vec<Address> {
    addr.iter().filter_map(mail_addr).collect()
}

fn mail_addr(a: &Addr<'_>) -> Option<Address> {
    let email = a.address.as_deref()?.trim();
    if email.is_empty() {
        return None;
    }
    Some(Address {
        name: a
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned),
        email: email.to_owned(),
    })
}

fn fill_from_envelope(h: &mut MessageHeader, env: &Envelope<'_>) {
    let text = |b: &Option<std::borrow::Cow<'_, [u8]>>| {
        b.as_ref().map(|v| String::from_utf8_lossy(v).into_owned())
    };
    h.message_id = text(&env.message_id).map(|s| s.trim_matches(['<', '>']).to_owned());
    h.in_reply_to = text(&env.in_reply_to).map(|s| s.trim_matches(['<', '>']).to_owned());
    h.subject = text(&env.subject).unwrap_or_default();
    h.from = envelope_addresses(env.from.as_deref());
    h.to = envelope_addresses(env.to.as_deref());
    h.cc = envelope_addresses(env.cc.as_deref());
}

fn envelope_addresses(list: Option<&[imap_proto::Address<'_>]>) -> Vec<Address> {
    list.unwrap_or_default()
        .iter()
        .filter_map(|a| {
            // Los grupos llegan con host NIL; se ignoran.
            let mailbox = String::from_utf8_lossy(a.mailbox.as_deref()?);
            let host = String::from_utf8_lossy(a.host.as_deref()?);
            let name = a
                .name
                .as_deref()
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .filter(|n| !n.trim().is_empty());
            Some(Address {
                name,
                email: format!("{mailbox}@{host}"),
            })
        })
        .collect()
}

// ----- BODYSTRUCTURE -----------------------------------------------------

/// Indica si la estructura MIME contiene algún adjunto.
///
/// Una hoja cuenta como adjunto si su disposición es `attachment`, o si no es
/// `inline`, no es `text/*` ni `multipart/*` y tiene parámetro `name`/`filename`.
pub fn has_attachments(bs: &BodyStructure<'_>) -> bool {
    match bs {
        BodyStructure::Multipart { bodies, .. } => bodies.iter().any(has_attachments),
        BodyStructure::Basic { common, .. }
        | BodyStructure::Text { common, .. }
        | BodyStructure::Message { common, .. } => {
            let disposition = common.disposition.as_ref();
            let disp_ty = disposition.map(|d| d.ty.to_ascii_lowercase());
            if disp_ty.as_deref() == Some("attachment") {
                return true;
            }
            if disp_ty.as_deref() == Some("inline") {
                return false;
            }
            let ty = common.ty.ty.to_ascii_lowercase();
            if ty == "text" || ty == "multipart" {
                return false;
            }
            let named = |params: &imap_proto::BodyParams<'_>| {
                params.as_ref().is_some_and(|ps| {
                    ps.iter().any(|(k, _)| {
                        k.eq_ignore_ascii_case("name") || k.eq_ignore_ascii_case("filename")
                    })
                })
            };
            named(&common.ty.params) || disposition.is_some_and(|d| named(&d.params))
        }
    }
}

// ----- Cuerpos -----------------------------------------------------------

/// Parsea un mensaje completo (`BODY[]`): texto, HTML y lista de adjuntos.
pub fn parse_body(raw: &[u8]) -> MessageBody {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return MessageBody::default();
    };
    let text = msg.body_text(0).map(|t| t.into_owned());
    let html = msg.body_html(0).map(|t| t.into_owned());
    let attachments = msg
        .attachments()
        .enumerate()
        .map(|(i, part)| attachment_info(part, i))
        .collect();
    MessageBody {
        text,
        html,
        attachments,
    }
}

fn attachment_info(part: &MessagePart<'_>, index: usize) -> Attachment {
    let mime_type = part
        .content_type()
        .map(|ct| match ct.c_subtype.as_deref() {
            Some(sub) => format!("{}/{}", ct.c_type, sub).to_ascii_lowercase(),
            None => ct.c_type.to_ascii_lowercase(),
        })
        .unwrap_or_else(|| "application/octet-stream".to_owned());
    Attachment {
        filename: part
            .attachment_name()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("adjunto-{}", index + 1)),
        mime_type,
        size: part.contents().len() as u64,
        content_id: part
            .content_id()
            .map(|c| c.trim_matches(['<', '>']).to_owned()),
    }
}

/// Bytes del adjunto `index` (mismo orden que `parse_body`).
pub fn attachment_contents(raw: &[u8], index: usize) -> Option<Vec<u8>> {
    let msg = MessageParser::default().parse(raw)?;
    msg.attachments()
        .nth(index)
        .map(|part| part.contents().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_imap::imap_proto::{AttributeValue, Response};

    const MAIL_WITH_PDF: &[u8] = b"From: Ana <ana@example.com>\r\n\
To: Guillermo <guille@example.com>, otro@example.com\r\n\
Cc: equipo@example.com\r\n\
Subject: Informe trimestral\r\n\
Date: Mon, 19 Oct 2026 10:00:00 +0200\r\n\
Message-ID: <abc123@example.com>\r\n\
In-Reply-To: <prev@example.com>\r\n\
References: <root@example.com> <prev@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"XX\"\r\n\
\r\n\
--XX\r\n\
Content-Type: multipart/alternative; boundary=\"YY\"\r\n\
\r\n\
--YY\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hola, adjunto el informe.\r\n\
--YY\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Hola, adjunto el <b>informe</b>.</p>\r\n\
--YY--\r\n\
--XX\r\n\
Content-Type: application/pdf; name=\"informe.pdf\"\r\n\
Content-Disposition: attachment; filename=\"informe.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--XX--\r\n";

    fn meta() -> FetchMeta {
        FetchMeta {
            mailbox_id: 7,
            uid: 42,
            flags: Flags {
                seen: true,
                ..Flags::default()
            },
            internal_date: 1_792_400_000,
            has_attachments: true,
            labels: vec!["Trabajo".to_owned()],
        }
    }

    #[test]
    fn role_from_special_use_attributes() {
        let attrs = vec!["\\Sent".to_owned(), "\\HasNoChildren".to_owned()];
        assert_eq!(mailbox_role(&attrs, "Enviados"), MailboxRole::Sent);
        assert_eq!(
            mailbox_role(&["\\Trash".to_owned()], "Whatever"),
            MailboxRole::Trash
        );
        assert_eq!(
            mailbox_role(&["\\junk".to_owned()], "Basura"),
            MailboxRole::Spam
        );
        assert_eq!(
            mailbox_role(&["\\Drafts".to_owned()], "x"),
            MailboxRole::Drafts
        );
        assert_eq!(
            mailbox_role(&["\\Archive".to_owned()], "x"),
            MailboxRole::Archive
        );
    }

    #[test]
    fn role_falls_back_to_name() {
        assert_eq!(mailbox_role(&[], "INBOX"), MailboxRole::Inbox);
        assert_eq!(mailbox_role(&[], "[Gmail]/Papelera"), MailboxRole::Trash);
        assert_eq!(mailbox_role(&[], "[Gmail]/Enviados"), MailboxRole::Sent);
        assert_eq!(mailbox_role(&[], "Borradores"), MailboxRole::Drafts);
        assert_eq!(mailbox_role(&[], "Deleted Items"), MailboxRole::Trash);
        assert_eq!(mailbox_role(&[], "Correo no deseado"), MailboxRole::Spam);
        assert_eq!(mailbox_role(&[], "Proyectos"), MailboxRole::Other);
    }

    #[test]
    fn special_use_wins_over_name() {
        // Una carpeta llamada "Papelera" pero marcada como \Archive gana el atributo.
        assert_eq!(
            mailbox_role(&["\\Archive".to_owned()], "Papelera"),
            MailboxRole::Archive
        );
    }

    #[test]
    fn noselect_is_not_selectable() {
        assert!(!is_selectable(&["\\Noselect".to_owned()]));
        assert!(is_selectable(&["\\HasChildren".to_owned()]));
    }

    #[test]
    fn name_attributes_are_stringified() {
        let attrs = [
            NameAttribute::Trash,
            NameAttribute::Extension("\\Inbox".into()),
        ];
        assert_eq!(attribute_strings(&attrs), vec!["\\Trash", "\\Inbox"]);
    }

    #[test]
    fn xoauth2_format() {
        let resp = xoauth2_initial_response("user@gmail.com", "ya29.TOKEN");
        assert_eq!(
            resp,
            b"user=user@gmail.com\x01auth=Bearer ya29.TOKEN\x01\x01".to_vec()
        );
    }

    #[test]
    fn uid_set_compacts_ranges() {
        assert_eq!(uid_set(&[]), "");
        assert_eq!(uid_set(&[5]), "5");
        assert_eq!(uid_set(&[9, 1, 2, 3, 7, 10, 3]), "1:3,7,9:10");
    }

    #[test]
    fn sync_plan_classifies_uids() {
        let server: BTreeSet<u32> = [1, 2, 3, 5].into();
        let cached_all: BTreeSet<u32> = [2, 3, 4, 9].into();
        let in_window: BTreeSet<u32> = [2, 3, 4].into();
        let plan = plan_sync(&server, &cached_all, &in_window);
        assert_eq!(plan.new_uids, vec![1, 5]);
        assert_eq!(plan.known_uids, vec![2, 3]);
        // 9 está en caché pero fuera de la ventana: no se borra.
        assert_eq!(plan.removed_uids, vec![4]);
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_747), (2026, 10, 21));
        assert_eq!(civil_from_days(20_790), (2026, 12, 3));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(imap_date(20_747), "21-Oct-2026");
        assert_eq!(imap_date(0), "01-Jan-1970");
        assert_eq!(today_days(1_792_540_800), 20_747);
        assert_eq!(today_days(1_792_540_799), 20_746);
    }

    #[test]
    fn flags_are_mapped() {
        let f = flags_from([Flag::Seen, Flag::Flagged, Flag::Deleted]);
        assert!(f.seen && f.flagged && !f.answered && !f.draft);
        let g = flags_from(std::iter::empty());
        assert!(!same_flags(&f, &g));
        assert!(same_flags(&f, &flags_from([Flag::Seen, Flag::Flagged])));
    }

    #[test]
    fn header_from_raw_fields() {
        let raw = b"Subject: Hola\r\nFrom: Ana <ana@example.com>\r\nTo: a@x.com\r\n\
Message-ID: <m1@x>\r\nIn-Reply-To: <m0@x>\r\nReferences: <r@x> <m0@x>\r\n\r\n";
        let h = build_header(Some(raw), None, meta());
        assert_eq!(h.subject, "Hola");
        assert_eq!(
            h.from,
            vec![Address {
                name: Some("Ana".into()),
                email: "ana@example.com".into()
            }]
        );
        assert_eq!(h.to.len(), 1);
        assert_eq!(h.message_id.as_deref(), Some("m1@x"));
        assert_eq!(h.in_reply_to.as_deref(), Some("m0@x"));
        assert_eq!(h.references, vec!["r@x", "m0@x"]);
        assert_eq!(h.mailbox_id, 7);
        assert_eq!(h.uid, 42);
        assert_eq!(h.date, 1_792_400_000);
        assert!(h.flags.seen);
        assert!(h.has_attachments);
        assert_eq!(h.labels, vec!["Trabajo"]);
    }

    #[test]
    fn header_from_full_message_parses_multiple_recipients() {
        let end = MAIL_WITH_PDF
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap()
            + 4;
        let h = build_header(Some(&MAIL_WITH_PDF[..end]), None, meta());
        assert_eq!(h.subject, "Informe trimestral");
        assert_eq!(h.to.len(), 2);
        assert_eq!(h.to[1].email, "otro@example.com");
        assert_eq!(h.cc[0].email, "equipo@example.com");
        assert_eq!(h.references.len(), 2);
    }

    #[test]
    fn header_falls_back_to_envelope() {
        let line = b"* 3 FETCH (UID 42 ENVELOPE (\"Mon, 19 Oct 2026 10:00:00 +0200\" \"Hola\" \
((\"Ana\" NIL \"ana\" \"example.com\")) NIL NIL ((NIL NIL \"guille\" \"example.com\")) NIL NIL NIL \"<m9@x>\"))\r\n";
        let (_, resp) = Response::parse(line).expect("parse");
        let Response::Fetch(_, attrs) = resp else {
            panic!("no fetch")
        };
        let env = attrs
            .iter()
            .find_map(|a| match a {
                AttributeValue::Envelope(e) => Some(e),
                _ => None,
            })
            .expect("envelope");
        let h = build_header(None, Some(env), meta());
        assert_eq!(h.subject, "Hola");
        assert_eq!(h.from[0].name.as_deref(), Some("Ana"));
        assert_eq!(h.from[0].email, "ana@example.com");
        assert_eq!(h.to[0].email, "guille@example.com");
        assert_eq!(h.message_id.as_deref(), Some("m9@x"));
    }

    fn bodystructure_of(line: &[u8]) -> bool {
        let (_, resp) = Response::parse(line).expect("parse");
        let Response::Fetch(_, attrs) = resp else {
            panic!("no fetch")
        };
        let bs = attrs
            .iter()
            .find_map(|a| match a {
                AttributeValue::BodyStructure(bs) => Some(bs),
                _ => None,
            })
            .expect("bodystructure");
        has_attachments(bs)
    }

    #[test]
    fn bodystructure_detects_attachment() {
        let line = b"* 1 FETCH (BODYSTRUCTURE (((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1)\
(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 40 1) \"ALTERNATIVE\" (\"BOUNDARY\" \"b1\") NIL NIL)\
(\"APPLICATION\" \"PDF\" (\"NAME\" \"informe.pdf\") NIL NIL \"BASE64\" 5000 NIL (\"ATTACHMENT\" (\"FILENAME\" \"informe.pdf\")) NIL) \"MIXED\" (\"BOUNDARY\" \"b0\") NIL NIL))\r\n";
        assert!(bodystructure_of(line));
    }

    #[test]
    fn bodystructure_without_attachment() {
        let line = b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1)\
(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 40 1) \"ALTERNATIVE\" (\"BOUNDARY\" \"b1\") NIL NIL))\r\n";
        assert!(!bodystructure_of(line));
    }

    #[test]
    fn bodystructure_inline_image_is_not_attachment() {
        let line = b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 40 1)\
(\"IMAGE\" \"PNG\" (\"NAME\" \"logo.png\") \"<logo@x>\" NIL \"BASE64\" 900 NIL (\"INLINE\" (\"FILENAME\" \"logo.png\")) NIL) \"RELATED\" (\"BOUNDARY\" \"b2\") NIL NIL))\r\n";
        assert!(!bodystructure_of(line));
    }

    #[test]
    fn parse_body_extracts_text_html_and_attachments() {
        let body = parse_body(MAIL_WITH_PDF);
        assert!(body.text.unwrap().contains("Hola, adjunto el informe."));
        assert!(body.html.unwrap().contains("<b>informe</b>"));
        assert_eq!(body.attachments.len(), 1);
        let a = &body.attachments[0];
        assert_eq!(a.filename, "informe.pdf");
        assert_eq!(a.mime_type, "application/pdf");
        assert!(a.size > 0);
        assert!(a.content_id.is_none());
    }

    #[test]
    fn attachment_bytes_match_index() {
        // El adjunto va en base64 ("JVBERi0xLjQK" = "%PDF-1.4\n"); se devuelven bytes decodificados.
        let bytes = attachment_contents(MAIL_WITH_PDF, 0).expect("adjunto 0");
        assert_eq!(bytes, b"%PDF-1.4\n".to_vec());
        assert!(attachment_contents(MAIL_WITH_PDF, 1).is_none());
    }

    #[test]
    fn parse_body_of_empty_input_has_no_content() {
        let body = parse_body(b"");
        assert!(body.text.as_deref().unwrap_or("").trim().is_empty());
        assert!(body.attachments.is_empty());
    }
}

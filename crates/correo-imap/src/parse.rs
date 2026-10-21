//! Funciones puras (sin red ni estado): roles de carpeta, conversión de
//! cabeceras y BODYSTRUCTURE, parseo de cuerpos, cadena XOAUTH2 y planificación
//! de sincronización. Todo aquí es testeable sin servidor.

use std::collections::BTreeSet;

use async_imap::imap_proto::{self, BodyStructure, Envelope, NameAttribute};
use async_imap::types::Flag;
use correo_core::{Address, Attachment, Flags, MailboxId, MailboxRole, MessageBody, MessageHeader};
use mail_parser::{Addr, HeaderValue, Message, MessageParser, MessagePart, MimeHeaders, PartType};

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

/// Delimitador de jerarquía declarado en LIST (`None` si el servidor no tiene jerarquía).
pub fn delimiter_char(delimiter: Option<&str>) -> Option<char> {
    delimiter.and_then(|d| d.chars().next())
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

// ----- UIDVALIDITY -------------------------------------------------------

/// Decide si la UIDVALIDITY del servidor invalida la caché de la carpeta.
///
/// Solo hay cambio cuando ambos valores existen y difieren: sin valor guardado
/// no hay nada que invalidar, y sin valor del servidor no se puede comparar.
/// Si cambió, los UID cacheados ya no apuntan a los mismos mensajes.
pub fn uid_validity_changed(stored: Option<u32>, server: Option<u32>) -> bool {
    matches!((stored, server), (Some(old), Some(new)) if old != new)
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
        while i + 1 < sorted.len() && end.checked_add(1) == Some(sorted[i + 1]) {
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

/// Conjuntos de UID para órdenes por lotes: ordena, deduplica, trocea en lotes de a lo
/// sumo `max` UIDs y formatea cada lote (`1:3,7`). Devuelve cada conjunto con sus UIDs.
pub fn uid_sets(uids: &[u32], max: usize) -> Vec<(String, Vec<u32>)> {
    correo_core::chunk_uids(uids, max)
        .into_iter()
        .map(|chunk| (uid_set(&chunk), chunk))
        .collect()
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
        name: a.name.as_deref().and_then(clean_name),
        email: email.to_owned(),
    })
}

/// Nombre para mostrar sin caracteres de control (CR/LF incluidos), recortado.
/// Los caracteres de control se sustituyen por espacio; si queda vacío, `None`.
pub fn clean_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
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
                .and_then(|n| clean_name(&String::from_utf8_lossy(n)));
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
        | BodyStructure::Message { common, .. } => leaf_is_attachment(common),
    }
}

/// Regla de `has_attachments` para una hoja de la estructura MIME.
pub(crate) fn leaf_is_attachment(common: &imap_proto::BodyContentCommon<'_>) -> bool {
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
            ps.iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("name") || k.eq_ignore_ascii_case("filename"))
        })
    };
    named(&common.ty.params) || disposition.is_some_and(|d| named(&d.params))
}

// ----- Cuerpos -----------------------------------------------------------

/// Parsea un mensaje completo (`BODY[]`): texto, HTML y lista de adjuntos.
pub fn parse_body(raw: &[u8]) -> MessageBody {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return MessageBody::default();
    };
    let text = msg.body_text(0).map(|t| t.into_owned());
    let html = msg.body_html(0).map(|t| t.into_owned());
    let paths = part_paths(&msg);
    let attachments = msg
        .attachments()
        .enumerate()
        .map(|(i, part)| {
            let id = msg.attachments.get(i).copied();
            let path = id.and_then(|id| paths.get(id as usize)).cloned().flatten();
            attachment_info(part, i, path)
        })
        .collect();
    MessageBody {
        text,
        html,
        attachments,
    }
}

/// Número de parte MIME estilo IMAP de cada parte de `msg` (índice = id de parte de
/// mail-parser). Las partes multipart y las que no se encuentran valen `None`.
fn part_paths(msg: &Message<'_>) -> Vec<Option<String>> {
    fn walk(msg: &Message<'_>, id: usize, path: String, out: &mut Vec<Option<String>>) {
        let Some(part) = msg.parts.get(id) else {
            return;
        };
        match &part.body {
            PartType::Multipart(children) => {
                for (i, child) in children.iter().enumerate() {
                    let child_path = if path.is_empty() {
                        (i + 1).to_string()
                    } else {
                        format!("{path}.{}", i + 1)
                    };
                    walk(msg, *child as usize, child_path, out);
                }
            }
            _ => out[id] = Some(path),
        }
    }
    let mut out = vec![None; msg.parts.len()];
    match msg.parts.first().map(|p| &p.body) {
        None => {}
        // Una raíz que no es multipart es la parte "1".
        Some(PartType::Multipart(_)) => walk(msg, 0, String::new(), &mut out),
        Some(_) => out[0] = Some("1".to_owned()),
    }
    out
}

fn attachment_info(part: &MessagePart<'_>, index: usize, path: Option<String>) -> Attachment {
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
        encoding: path.is_some().then(|| {
            part.content_transfer_encoding()
                .map(|e| e.trim().to_ascii_lowercase())
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| "7bit".to_owned())
        }),
        encoded_size: path
            .is_some()
            .then(|| u64::from(part.offset_end.saturating_sub(part.offset_body))),
        part: path,
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
    fn delimiter_comes_from_the_server() {
        assert_eq!(delimiter_char(Some("/")), Some('/'));
        assert_eq!(delimiter_char(Some(".")), Some('.'));
        assert_eq!(delimiter_char(Some("")), None);
        assert_eq!(delimiter_char(None), None);
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
    fn uid_sets_are_chunked_sorted_and_compact() {
        assert!(uid_sets(&[], 500).is_empty());
        let sets = uid_sets(&[9, 1, 3, 2, 5, 9], 500);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].0, "1:3,5,9");
        assert_eq!(sets[0].1, vec![1, 2, 3, 5, 9]);

        // 1201 UIDs contiguos: tres órdenes, ninguna con más de 500, y rangos compactos.
        let many: Vec<u32> = (1..=1201).collect();
        let sets = uid_sets(&many, correo_core::BATCH_MAX_UIDS);
        let texts: Vec<&str> = sets.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["1:500", "501:1000", "1001:1201"]);

        // UIDs alternos: el troceo respeta el tope aunque no haya rangos.
        let odd: Vec<u32> = (0..1000).map(|i| i * 2 + 1).collect();
        let sets = uid_sets(&odd, 500);
        assert_eq!(sets.len(), 2);
        assert!(sets.iter().all(|(_, u)| u.len() == 500));
        assert!(sets[0].0.starts_with("1,3,5,"));
        assert!(!sets[0].0.contains(':'));
    }

    #[test]
    fn uid_set_handles_max_uid_without_overflow() {
        assert_eq!(uid_set(&[u32::MAX]), "4294967295");
        assert_eq!(uid_set(&[u32::MAX - 1, u32::MAX]), "4294967294:4294967295");
        assert_eq!(uid_set(&[0, u32::MAX]), "0,4294967295");
    }

    #[test]
    fn uid_validity_change_is_detected_only_when_both_known() {
        assert!(uid_validity_changed(Some(1), Some(2)));
        assert!(!uid_validity_changed(Some(7), Some(7)));
        // Sin valor guardado no hay caché que invalidar.
        assert!(!uid_validity_changed(None, Some(2)));
        // Sin valor del servidor no hay nada que comparar.
        assert!(!uid_validity_changed(Some(1), None));
        assert!(!uid_validity_changed(None, None));
    }

    #[test]
    fn control_chars_in_addresses_are_replaced_and_trimmed() {
        let a = Addr {
            name: Some("  Ana\r\nPérez\t".into()),
            address: Some("ana@example.com".into()),
        };
        let got = mail_addr(&a).unwrap();
        assert_eq!(got.name.as_deref(), Some("Ana  Pérez"));
        let blank = Addr {
            name: Some("\r\n ".into()),
            address: Some("x@example.com".into()),
        };
        assert_eq!(mail_addr(&blank).unwrap().name, None);
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
    fn parse_body_records_part_number_encoding_and_size() {
        let body = parse_body(MAIL_WITH_PDF);
        let a = &body.attachments[0];
        // multipart/mixed: [0] alternative, [1] pdf.
        assert_eq!(a.part.as_deref(), Some("2"));
        assert_eq!(a.encoding.as_deref(), Some("base64"));
        let size = a.encoded_size.unwrap();
        assert!((12..=16).contains(&size), "tamaño codificado {size}");
    }

    const NESTED: &[u8] = b"Subject: x\r\n\
Content-Type: multipart/mixed; boundary=\"A\"\r\n\
\r\n\
--A\r\n\
Content-Type: text/plain\r\n\
\r\n\
hola\r\n\
--A\r\n\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>x</p>\r\n\
--B\r\n\
Content-Type: image/png; name=\"a.png\"\r\n\
Content-Disposition: attachment; filename=\"a.png\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
AAEC\r\n\
--B--\r\n\
--A\r\n\
Content-Type: message/rfc822\r\n\
Content-Disposition: attachment; filename=\"fwd.eml\"\r\n\
\r\n\
Subject: interno\r\n\
Content-Type: text/plain\r\n\
\r\n\
dentro\r\n\
--A\r\n\
Content-Type: application/octet-stream\r\n\
Content-Disposition: attachment; filename*=utf-8''caf%C3%A9.bin\r\n\
Content-Transfer-Encoding: Quoted-Printable\r\n\
\r\n\
a=3Db\r\n\
--A--\r\n";

    #[test]
    fn part_numbers_in_nested_multipart_and_rfc822() {
        let body = parse_body(NESTED);
        let got: Vec<_> = body
            .attachments
            .iter()
            .map(|a| (a.filename.as_str(), a.part.as_deref().unwrap()))
            .collect();
        assert_eq!(
            got,
            vec![("a.png", "2.2"), ("fwd.eml", "3"), ("café.bin", "4")]
        );
        assert_eq!(
            body.attachments[2].encoding.as_deref(),
            Some("quoted-printable")
        );
        assert_eq!(body.attachments[1].encoding.as_deref(), Some("7bit"));
    }

    #[test]
    fn single_part_root_is_part_one() {
        let raw = b"Content-Type: application/pdf; name=\"x.pdf\"\r\n\
Content-Disposition: attachment; filename=\"x.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n";
        let body = parse_body(raw);
        assert_eq!(body.attachments[0].part.as_deref(), Some("1"));
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

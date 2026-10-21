//! Construcción MIME y envío SMTP con autenticación XOAUTH2.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use correo_core::{AccountId, Address, Draft, Error, Result, Sender, TokenProvider};
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, Message, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

/// Dominio de respaldo para Message-ID si la dirección no tiene dominio.
const FALLBACK_DOMAIN: &str = "localhost";

static MESSAGE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Sustituye los caracteres de control (CR, LF, tabulador...) por un espacio y
/// recorta. Evita que un valor de usuario rompa las cabeceras MIME o haga
/// fallar el formateo del mensaje.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn mailbox(address: &Address) -> Result<Mailbox> {
    let email = address
        .email
        .parse::<lettre::Address>()
        .map_err(|e| Error::Protocol(format!("dirección inválida '{}': {e}", address.email)))?;
    let name = address.name.as_deref().map(clean).filter(|n| !n.is_empty());
    Ok(Mailbox::new(name, email))
}

fn mailboxes(addresses: &[Address]) -> Result<Vec<Mailbox>> {
    addresses.iter().map(mailbox).collect()
}

/// Message-ID único de la forma `<id@dominio>`, con el dominio del remitente.
fn message_id(from: &Address) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let counter = MESSAGE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let domain = from
        .email
        .rsplit_once('@')
        .map(|(_, d)| d)
        .filter(|d| !d.is_empty())
        .unwrap_or(FALLBACK_DOMAIN);
    format!("<{nanos:x}.{counter:x}.{}@{domain}>", std::process::id())
}

/// Normaliza un identificador de mensaje a la forma `<...>`.
fn angle(id: &str) -> String {
    let id = clean(id);
    if id.starts_with('<') && id.ends_with('>') {
        id
    } else {
        format!("<{id}>")
    }
}

fn content_type(mime: &str) -> ContentType {
    ContentType::parse(&clean(mime))
        .unwrap_or_else(|_| ContentType::parse("application/octet-stream").expect("MIME válido"))
}

fn assemble(draft: &Draft, keep_bcc: bool) -> Result<Message> {
    let mut builder = Message::builder()
        .from(mailbox(&draft.from)?)
        .subject(clean(&draft.subject))
        .message_id(Some(message_id(&draft.from)))
        .date_now();

    for to in mailboxes(&draft.to)? {
        builder = builder.to(to);
    }
    for cc in mailboxes(&draft.cc)? {
        builder = builder.cc(cc);
    }
    for bcc in mailboxes(&draft.bcc)? {
        builder = builder.bcc(bcc);
    }
    if let Some(parent) = draft
        .in_reply_to
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        builder = builder.in_reply_to(angle(parent));
    }
    if !draft.references.is_empty() {
        let refs = draft
            .references
            .iter()
            .map(|r| angle(r))
            .collect::<Vec<_>>()
            .join(" ");
        builder = builder.references(refs);
    }
    if keep_bcc {
        builder = builder.keep_bcc();
    }

    // Cuerpo: text/plain, o multipart/alternative si hay HTML; con adjuntos, multipart/mixed.
    let has_attachments = !draft.attachments.is_empty();
    let body_error =
        |e: lettre::error::Error| Error::Protocol(format!("construyendo mensaje: {e}"));

    if !has_attachments {
        return match draft.html.as_deref() {
            Some(html) => builder
                .multipart(MultiPart::alternative_plain_html(
                    draft.text.clone(),
                    html.to_owned(),
                ))
                .map_err(body_error),
            None => builder
                .singlepart(SinglePart::plain(draft.text.clone()))
                .map_err(body_error),
        };
    }

    let mut mixed = match draft.html.as_deref() {
        Some(html) => MultiPart::mixed().multipart(MultiPart::alternative_plain_html(
            draft.text.clone(),
            html.to_owned(),
        )),
        None => MultiPart::mixed().singlepart(SinglePart::plain(draft.text.clone())),
    };
    for (name, mime, data) in &draft.attachments {
        let part = Attachment::new(clean(name)).body(data.clone(), content_type(mime));
        mixed = mixed.singlepart(part);
    }
    builder.multipart(mixed).map_err(body_error)
}

/// Construye el mensaje MIME listo para enviar.
///
/// Las cabeceras `Bcc` se eliminan (los destinatarios siguen en el sobre SMTP).
pub fn build_message(draft: &Draft) -> Result<Message> {
    assemble(draft, false)
}

/// Mensaje en bruto (RFC 5322) para guardar en Enviados o Borradores vía IMAP.
///
/// Conserva la cabecera `Bcc`, porque la copia guardada es la del propio usuario.
pub fn raw_message(draft: &Draft) -> Result<Vec<u8>> {
    Ok(assemble(draft, true)?.formatted())
}

/// Modo de cifrado de la conexión SMTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// Conexión TLS implícita desde el primer byte (p. ej. puerto 465).
    Implicit,
    /// Conexión en claro que pasa a TLS con STARTTLS (p. ej. puerto 587). Obligatorio.
    Starttls,
}

/// Remitente SMTP que autentica con XOAUTH2.
#[derive(Clone)]
pub struct SmtpSender {
    pub host: String,
    pub port: u16,
    pub starttls_or_tls: TlsMode,
    pub account_id: AccountId,
    pub email: String,
    pub tokens: Arc<dyn TokenProvider>,
}

impl SmtpSender {
    /// Preset de Gmail: `smtp.gmail.com:465` con TLS implícito.
    pub fn gmail(account_id: AccountId, email: String, tokens: Arc<dyn TokenProvider>) -> Self {
        Self {
            host: "smtp.gmail.com".into(),
            port: 465,
            starttls_or_tls: TlsMode::Implicit,
            account_id,
            email,
            tokens,
        }
    }

    /// Preset de Outlook / Microsoft 365: `smtp.office365.com:587` con STARTTLS.
    pub fn outlook(account_id: AccountId, email: String, tokens: Arc<dyn TokenProvider>) -> Self {
        Self {
            host: "smtp.office365.com".into(),
            port: 587,
            starttls_or_tls: TlsMode::Starttls,
            account_id,
            email,
            tokens,
        }
    }

    fn transport(&self, token: &str) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
        let params = TlsParameters::new_rustls(self.host.clone())
            .map_err(|e| Error::Network(format!("configuración TLS: {e}")))?;
        let tls = match self.starttls_or_tls {
            TlsMode::Implicit => Tls::Wrapper(params),
            TlsMode::Starttls => Tls::Required(params),
        };
        Ok(
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(self.host.clone())
                .port(self.port)
                .tls(tls)
                .credentials(Credentials::new(self.email.clone(), token.to_owned()))
                .authentication(vec![Mechanism::Xoauth2])
                .build(),
        )
    }
}

#[async_trait::async_trait]
impl Sender for SmtpSender {
    async fn send(&self, draft: &Draft) -> Result<()> {
        let message = build_message(draft)?;
        let token = self.tokens.access_token(self.account_id).await?;
        let transport = self.transport(&token.0)?;
        transport.send(message).await.map_err(|e| {
            let text = e.to_string();
            if e.is_permanent()
                && (text.contains("535") || text.contains("534") || text.contains("530"))
            {
                Error::Auth(text)
            } else if e.is_transient() || e.is_timeout() || e.is_tls() {
                Error::Network(text)
            } else {
                Error::Protocol(text)
            }
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_owned),
            email: email.to_owned(),
        }
    }

    fn base_draft() -> Draft {
        Draft {
            from: addr(Some("Guillermo Marco"), "yo@example.com"),
            to: vec![addr(Some("Ana Pérez"), "ana@example.org")],
            subject: "Hola".into(),
            text: "Texto plano".into(),
            ..Default::default()
        }
    }

    fn text(msg: &Message) -> String {
        String::from_utf8_lossy(&msg.formatted()).into_owned()
    }

    #[test]
    fn plain_message_has_basic_headers() {
        let msg = build_message(&base_draft()).unwrap();
        let raw = text(&msg);
        assert!(raw.contains("From: "));
        assert!(raw.contains("yo@example.com"));
        assert!(raw.contains("To: "));
        assert!(raw.contains("ana@example.org"));
        assert!(raw.contains("Subject: Hola"));
        assert!(raw.contains("Date: "));
        assert!(raw.contains("Message-ID: <"));
        assert!(raw.contains("text/plain"));
        assert!(!raw.contains("multipart"));
    }

    #[test]
    fn message_id_uses_sender_domain_and_is_unique() {
        let a = text(&build_message(&base_draft()).unwrap());
        let b = text(&build_message(&base_draft()).unwrap());
        assert!(a.contains("@example.com>"));
        let id = |raw: &str| {
            raw.lines()
                .find(|l| l.starts_with("Message-ID:"))
                .unwrap()
                .to_owned()
        };
        assert_ne!(id(&a), id(&b));
    }

    #[test]
    fn html_produces_alternative_with_both_parts() {
        let mut d = base_draft();
        d.html = Some("<p>Hola</p>".into());
        let raw = text(&build_message(&d).unwrap());
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("text/plain"));
        assert!(raw.contains("text/html"));
        assert!(!raw.contains("multipart/mixed"));
    }

    #[test]
    fn attachments_produce_mixed() {
        let mut d = base_draft();
        d.html = Some("<p>Hola</p>".into());
        let binary = vec![0x25, 0x50, 0xFF, 0x00, 0xEE, 0x80];
        d.attachments
            .push(("informe.pdf".into(), "application/pdf".into(), binary));
        let raw = text(&build_message(&d).unwrap());
        assert!(raw.contains("multipart/mixed"));
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("application/pdf"));
        assert!(raw.contains("filename=\"informe.pdf\""));
        assert!(raw.contains("Content-Transfer-Encoding: base64"));
    }

    #[test]
    fn attachment_without_html_is_mixed_with_plain() {
        let mut d = base_draft();
        d.attachments
            .push(("a.txt".into(), "text/plain".into(), b"x".to_vec()));
        let raw = text(&build_message(&d).unwrap());
        assert!(raw.contains("multipart/mixed"));
        assert!(!raw.contains("multipart/alternative"));
        assert!(raw.contains("filename=\"a.txt\""));
    }

    #[test]
    fn invalid_attachment_mime_falls_back_to_octet_stream() {
        let mut d = base_draft();
        d.attachments
            .push(("x.bin".into(), "no es mime".into(), vec![1, 2, 3]));
        let raw = text(&build_message(&d).unwrap());
        assert!(raw.contains("application/octet-stream"));
    }

    #[test]
    fn reply_headers_are_set_and_normalized() {
        let mut d = base_draft();
        d.in_reply_to = Some("abc@example.org".into());
        d.references = vec!["<root@example.org>".into(), "abc@example.org".into()];
        let raw = text(&build_message(&d).unwrap());
        assert!(raw.contains("In-Reply-To: <abc@example.org>"));
        assert!(raw.contains("References: <root@example.org> <abc@example.org>"));
    }

    #[test]
    fn no_reply_headers_when_not_replying() {
        let raw = text(&build_message(&base_draft()).unwrap());
        assert!(!raw.contains("In-Reply-To:"));
        assert!(!raw.contains("References:"));
    }

    #[test]
    fn cc_is_in_header_and_bcc_is_not_in_sent_message() {
        let mut d = base_draft();
        d.cc = vec![addr(None, "cc@example.org")];
        d.bcc = vec![addr(None, "secreto@example.net")];
        let raw = text(&build_message(&d).unwrap());
        assert!(raw.contains("Cc: cc@example.org"));
        assert!(!raw.contains("Bcc"));
        assert!(!raw.contains("secreto@example.net"));
        // Los destinatarios ocultos siguen en el sobre SMTP.
        let msg = build_message(&d).unwrap();
        let envelope = msg.envelope();
        assert!(
            envelope
                .to()
                .iter()
                .any(|a| a.to_string() == "secreto@example.net")
        );
    }

    #[test]
    fn raw_message_keeps_bcc() {
        let mut d = base_draft();
        d.bcc = vec![addr(None, "secreto@example.net")];
        let raw = String::from_utf8(raw_message(&d).unwrap()).unwrap();
        assert!(raw.contains("Bcc: secreto@example.net"));
        assert!(raw.contains("Subject: Hola"));
    }

    #[test]
    fn non_ascii_subject_is_encoded() {
        let mut d = base_draft();
        d.subject = "Reunión 🚀 niño".into();
        let raw = text(&build_message(&d).unwrap());
        assert!(raw.to_ascii_lowercase().contains("subject: =?utf-8?b?"));
        assert!(!raw.contains("Reunión"));
        // La cabecera decodificada debe recuperar el asunto original.
        let encoded = raw
            .lines()
            .find(|l| l.starts_with("Subject:"))
            .unwrap()
            .trim_start_matches("Subject: ")
            .to_owned();
        let decoded = decode_rfc2047(&encoded);
        assert_eq!(decoded, "Reunión 🚀 niño");
    }

    #[test]
    fn non_ascii_display_names_are_encoded() {
        let mut d = base_draft();
        d.from = addr(Some("José Ñandú 😀"), "yo@example.com");
        d.to = vec![addr(Some("Zoë Müller"), "zoe@example.org")];
        let raw = text(&build_message(&d).unwrap());
        assert!(!raw.contains("José"));
        assert!(!raw.contains("Zoë"));
        assert!(raw.to_ascii_lowercase().contains("=?utf-8?b?"));
        assert!(raw.contains("zoe@example.org"));
    }

    #[test]
    fn non_ascii_attachment_name_is_encoded() {
        let mut d = base_draft();
        d.attachments.push((
            "presupuesto año 🚀.pdf".into(),
            "application/pdf".into(),
            vec![0],
        ));
        let raw = text(&build_message(&d).unwrap());
        assert!(!raw.contains("presupuesto año"));
        assert!(raw.contains("application/pdf"));
    }

    /// Cabeceras (antes de la línea vacía) de un mensaje formateado.
    fn header_section(raw: &str) -> &str {
        raw.split_once("\r\n\r\n").map(|(h, _)| h).unwrap_or(raw)
    }

    /// Ninguna línea de cabecera puede quedar huérfana (sin `Nombre:` ni plegado).
    fn assert_no_stray_header_lines(raw: &str) {
        for line in header_section(raw).split("\r\n") {
            assert!(
                line.starts_with(' ') || line.starts_with('\t') || line.contains(':'),
                "línea de cabecera suelta: {line:?}"
            );
        }
    }

    #[test]
    fn control_chars_in_names_and_subject_do_not_panic_or_break_headers() {
        let mut d = base_draft();
        d.from = addr(Some("A\r\nB"), "yo@example.com");
        d.to = vec![addr(Some("C\r\nD"), "ana@example.org")];
        d.cc = vec![addr(Some("E\nF"), "cc@example.org")];
        d.subject = "Hola\r\nAdios".into();
        d.in_reply_to = Some("abc@example.org\r\nBcc: x@example.net".into());
        d.attachments.push((
            "informe\r\n.pdf".into(),
            "application/pdf\r\nX-Evil: 1".into(),
            vec![1, 2, 3],
        ));
        d.html = Some("<p>Hola</p>".into());

        let built = text(&build_message(&d).expect("build_message no debe fallar"));
        let stored =
            String::from_utf8(raw_message(&d).expect("raw_message no debe fallar")).expect("UTF-8");

        for raw in [&built, &stored] {
            assert_no_stray_header_lines(raw);
            assert!(raw.contains("Subject: Hola  Adios"), "{raw}");
            assert!(!raw.contains("X-Evil"), "{raw}");
            assert!(!header_section(raw).contains("\r\nBcc: x@example.net"));
        }
    }

    #[test]
    fn mailbox_names_are_sanitized() {
        let mb = mailbox(&addr(Some("  A\r\nB\t "), "yo@example.com")).unwrap();
        assert_eq!(mb.name.as_deref(), Some("A  B"));
        let blank = mailbox(&addr(Some("\r\n"), "yo@example.com")).unwrap();
        assert_eq!(blank.name, None);
    }

    #[test]
    fn invalid_address_is_protocol_error() {
        let mut d = base_draft();
        d.to = vec![addr(None, "no-es-un-correo")];
        assert!(matches!(build_message(&d), Err(Error::Protocol(_))));
    }

    #[test]
    fn message_without_recipients_fails() {
        let mut d = base_draft();
        d.to.clear();
        assert!(build_message(&d).is_err());
    }

    /// Decodificador mínimo de RFC 2047 (solo `=?utf-8?B?...?=`) para los tests.
    fn decode_rfc2047(s: &str) -> String {
        use base64_helper::decode;
        let prefix = "=?utf-8?b?";
        assert!(s.len() > prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix));
        let inner = s[prefix.len()..].strip_suffix("?=").expect("cierre ?=");
        String::from_utf8(decode(inner)).unwrap()
    }

    mod base64_helper {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

        pub fn decode(input: &str) -> Vec<u8> {
            let mut out = Vec::new();
            let mut buf = 0u32;
            let mut bits = 0;
            for c in input.bytes().filter(|&c| c != b'=') {
                let v = ALPHABET.iter().position(|&a| a == c).expect("base64") as u32;
                buf = (buf << 6) | v;
                bits += 6;
                if bits >= 8 {
                    bits -= 8;
                    out.push((buf >> bits) as u8);
                }
            }
            out
        }
    }
}

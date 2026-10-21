//! `ImapBackend`: implementación de `MailBackend` sobre IMAP.
//!
//! Una sesión por backend, abierta de forma perezosa. Si una operación falla
//! por red, la sesión se descarta y la operación se reintenta una vez con una
//! sesión nueva.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_imap::extensions::idle::IdleResponse;
use async_imap::types::{Fetch, Name};
use async_trait::async_trait;
use correo_core::{
    AccountId, Attachment, BATCH_MAX_UIDS, BatchReport, CancelToken, Error, Flags, Mailbox,
    MailboxId, MailboxRole, MessageBody, MessageHeader, MessageId, ProgressFn, Result,
    TokenProvider,
};
use correo_store::Store;
use futures::TryStreamExt;
use tokio::sync::{MappedMutexGuard, Mutex as AsyncMutex, MutexGuard};

use crate::attach::{self, CHUNK_SIZE, ChunkSource};
use crate::conn::{self, Conn, OP_TIMEOUT, imap, is_network, map_imap_error, timeout_error};
use crate::parse::{
    self, BATCH_SIZE, FetchMeta, attachment_contents, attribute_strings, flags_from, imap_date,
    mailbox_role, parse_body, plan_sync, same_flags, today_days, uid_set, uid_sets,
    uid_validity_changed,
};

pub const GMAIL_HOST: &str = "imap.gmail.com";
pub const OUTLOOK_HOST: &str = "outlook.office365.com";
pub const IMAPS_PORT: u16 = 993;

/// Duración máxima de una espera IDLE; los servidores suelen cortarla hacia los 29 min.
pub const IDLE_MAX: Duration = Duration::from_secs(25 * 60);

/// Margen sobre `IDLE_MAX` para el tope externo de toda la espera IDLE
/// (conexión propia incluida).
pub const IDLE_GRACE: Duration = Duration::from_secs(30);

/// Ventana máxima de sincronización en días; evita SEARCH con fechas absurdas.
pub const MAX_SYNC_DAYS: u32 = 3650;

/// Días de ventana efectivos para una cuenta.
pub fn effective_sync_days(days: u32) -> u32 {
    days.min(MAX_SYNC_DAYS)
}

/// Resultado de una espera IDLE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleEvent {
    /// El servidor anunció cambios en la carpeta (EXISTS, EXPUNGE, FETCH...).
    NewData,
    /// Expiró `max` sin cambios (o el servidor envió un keepalive hasta el límite).
    Timeout,
}

/// Tamaño de página al recorrer la caché.
const CACHE_PAGE: usize = 500;

/// Cabeceras que se piden en cada FETCH de sincronización.
const HEADER_FIELDS: &str =
    "BODY.PEEK[HEADER.FIELDS (MESSAGE-ID IN-REPLY-TO REFERENCES SUBJECT FROM TO CC)]";

pub struct ImapBackend {
    host: String,
    port: u16,
    account_id: AccountId,
    email: String,
    tokens: Arc<dyn TokenProvider>,
    store: Arc<Mutex<Store>>,
    conn: AsyncMutex<Option<Conn>>,
}

impl ImapBackend {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        account_id: AccountId,
        email: impl Into<String>,
        tokens: Arc<dyn TokenProvider>,
        store: Arc<Mutex<Store>>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            account_id,
            email: email.into(),
            tokens,
            store,
            conn: AsyncMutex::new(None),
        }
    }

    /// Preset para Gmail (imap.gmail.com:993).
    pub fn gmail(
        account_id: AccountId,
        email: impl Into<String>,
        tokens: Arc<dyn TokenProvider>,
        store: Arc<Mutex<Store>>,
    ) -> Self {
        Self::new(GMAIL_HOST, IMAPS_PORT, account_id, email, tokens, store)
    }

    /// Preset para Outlook.com / Microsoft 365 (outlook.office365.com:993).
    pub fn outlook(
        account_id: AccountId,
        email: impl Into<String>,
        tokens: Arc<dyn TokenProvider>,
        store: Arc<Mutex<Store>>,
    ) -> Self {
        Self::new(OUTLOOK_HOST, IMAPS_PORT, account_id, email, tokens, store)
    }

    // ----- Infraestructura ---------------------------------------------

    fn with_store<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let guard = self
            .store
            .lock()
            .map_err(|_| Error::Store("el almacén quedó bloqueado por un pánico".into()))?;
        f(&guard)
    }

    async fn connect(&self) -> Result<Conn> {
        let token = self.tokens.access_token(self.account_id).await?;
        conn::connect(&self.host, self.port, &self.email, &token.0).await
    }

    /// Devuelve la conexión, abriéndola si no existe.
    async fn conn(&self) -> Result<MappedMutexGuard<'_, Conn>> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.connect().await?);
        }
        Ok(MutexGuard::map(guard, |c| {
            c.as_mut().expect("conexión inicializada arriba")
        }))
    }

    async fn reset(&self) {
        *self.conn.lock().await = None;
    }

    /// Ejecuta `op`; si falla por red, descarta la sesión y reintenta una vez.
    async fn retry<T, F, Fut>(&self, mut op: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        match op().await {
            Err(e) if is_network(&e) => {
                self.reset().await;
                op().await
            }
            other => other,
        }
    }

    /// Ejecuta una operación que no se reintenta (APPEND, COPY): si falla por
    /// red descarta la sesión y devuelve el error tal cual.
    async fn attempt<T>(&self, op: impl Future<Output = Result<T>>) -> Result<T> {
        let res = op.await;
        if matches!(&res, Err(e) if is_network(e)) {
            self.reset().await;
        }
        res
    }

    /// SELECT de `mailbox` en `conn` con la comprobación de UIDVALIDITY: si el
    /// servidor cambió la carpeta, la caché se borra y la acción no se ejecuta.
    async fn select_checked(&self, mailbox: &Mailbox, conn: &mut Conn) -> Result<()> {
        let selected = imap(OP_TIMEOUT, conn.session.select(&mailbox.path)).await?;
        self.guard_uid_validity(mailbox, selected.uid_validity)
    }

    fn guard_uid_validity(&self, mailbox: &Mailbox, server: Option<u32>) -> Result<()> {
        let stored = self.with_store(|s| s.get_mailbox(mailbox.id))?;
        if !uid_validity_changed(stored.uid_validity, server) {
            return Ok(());
        }
        self.with_store(|s| s.wipe_mailbox(mailbox.id))?;
        let state = Mailbox {
            uid_validity: server,
            ..stored
        };
        self.with_store(|s| s.upsert_mailbox(&state))?;
        Err(Error::Protocol(
            "la carpeta cambió; vuelve a sincronizar".into(),
        ))
    }

    /// Espera IDLE sobre `mailbox` en una conexión propia (IDLE bloquea la sesión
    /// de operaciones). Devuelve al haber cambios o al expirar `max`. La
    /// conexión se descarta al terminar; el planificador decide cuándo reiniciar.
    ///
    /// Si el servidor no anuncia IDLE devuelve `Error::Protocol("sin IDLE")`.
    pub async fn idle_inbox(&self, mailbox: &Mailbox, max: Duration) -> Result<IdleEvent> {
        let limit = max + IDLE_GRACE;
        match tokio::time::timeout(limit, self.idle_once(mailbox, max)).await {
            Ok(outcome) => outcome,
            Err(_) => {
                self.reset().await;
                Err(timeout_error(limit))
            }
        }
    }

    async fn idle_once(&self, mailbox: &Mailbox, max: Duration) -> Result<IdleEvent> {
        let mut conn = self.connect().await?;
        if !conn.caps.idle {
            return Err(Error::Protocol("sin IDLE".into()));
        }
        imap(OP_TIMEOUT, conn.session.select(&mailbox.path)).await?;

        let mut handle = conn.session.idle();
        imap(OP_TIMEOUT, handle.init()).await?;

        let outcome = {
            let (wait, _stop) = handle.wait_with_timeout(max);
            tokio::time::timeout(max, wait).await
        };
        let event = match outcome {
            Err(_elapsed) => IdleEvent::Timeout,
            Ok(Ok(IdleResponse::NewData(_))) => IdleEvent::NewData,
            Ok(Ok(IdleResponse::Timeout | IdleResponse::ManualInterrupt)) => IdleEvent::Timeout,
            Ok(Err(e)) => return Err(map_imap_error(e)),
        };

        // DONE para cerrar limpio; acotado en el tiempo por si el servidor no contesta.
        let _ = tokio::time::timeout(Duration::from_secs(5), handle.done()).await;
        Ok(event)
    }

    /// Guarda `raw` (RFC 5322) en la carpeta de rol Enviados, marcada como leída.
    /// Lo usan los servidores que no guardan la copia al enviar por SMTP (Outlook).
    pub async fn append_to_sent(&self, raw: &[u8]) -> Result<()> {
        let sent = correo_core::MailBackend::list_mailboxes(self)
            .await?
            .into_iter()
            .find(|m| m.role == MailboxRole::Sent)
            .ok_or_else(|| Error::NotFound(format!("carpeta de Enviados de {}", self.email)))?;
        // APPEND no se reintenta: tras un corte no se sabe si el mensaje llegó.
        self.attempt(self.append_once(&sent, raw)).await
    }

    async fn append_once(&self, sent: &Mailbox, raw: &[u8]) -> Result<()> {
        let mut guard = self.conn().await?;
        imap(
            OP_TIMEOUT,
            guard
                .session
                .append(&sent.path, Some("(\\Seen)"), None, raw),
        )
        .await
    }

    /// Cabeceras cacheadas de una carpeta, paginando.
    fn cached_headers(&self, mailbox_id: MailboxId) -> Result<Vec<MessageHeader>> {
        let mut all = Vec::new();
        let mut offset = 0;
        loop {
            let page = self.with_store(|s| s.list_headers(mailbox_id, CACHE_PAGE, offset))?;
            let n = page.len();
            all.extend(page);
            if n < CACHE_PAGE {
                return Ok(all);
            }
            offset += CACHE_PAGE;
        }
    }

    /// Id de caché de (carpeta, uid), o `None` si el mensaje no está cacheado.
    fn message_id(&self, mailbox_id: MailboxId, uid: u32) -> Result<Option<MessageId>> {
        self.with_store(|s| s.message_id(mailbox_id, uid))
    }

    /// Actualiza la caché de flags; si el mensaje no está cacheado no es error.
    fn cache_flags(&self, mailbox_id: MailboxId, uid: u32, flags: &Flags) -> Result<()> {
        match self.with_store(|s| s.set_flags(mailbox_id, uid, flags)) {
            Err(Error::NotFound(_)) => Ok(()),
            other => other,
        }
    }

    fn trash_mailbox(&self) -> Result<Option<Mailbox>> {
        let boxes = self.with_store(|s| s.list_mailboxes(self.account_id))?;
        Ok(boxes.into_iter().find(|m| m.role == MailboxRole::Trash))
    }
}

/// Fuente de trozos sobre la sesión del backend. Ante un error de red descarta la sesión
/// (el siguiente trozo abre una nueva).
struct ImapChunkSource<'a> {
    backend: &'a ImapBackend,
    mailbox: &'a Mailbox,
    uid: u32,
    part: String,
}

#[async_trait]
impl ChunkSource for ImapChunkSource<'_> {
    async fn fetch(&mut self, offset: u64, len: u32) -> Result<Vec<u8>> {
        let r = self
            .backend
            .fetch_chunk_once(self.mailbox, self.uid, &self.part, offset, len)
            .await;
        if matches!(&r, Err(e) if is_network(e)) {
            self.backend.reset().await;
        }
        r
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[async_trait]
impl correo_core::MailBackend for ImapBackend {
    async fn list_mailboxes(&self) -> Result<Vec<Mailbox>> {
        self.retry(|| self.list_mailboxes_once()).await
    }

    async fn sync_mailbox(&self, mailbox: &Mailbox, days: u32) -> Result<usize> {
        let days = effective_sync_days(days);
        self.retry(|| self.sync_mailbox_once(mailbox, days)).await
    }

    async fn fetch_body(&self, mailbox: &Mailbox, uid: u32) -> Result<MessageBody> {
        // Si el cuerpo ya está en caché, no hace falta la red.
        if let Some(mid) = self.message_id(mailbox.id, uid)?
            && let Some(body) = self.with_store(|s| s.get_body(mid))?
        {
            return Ok(body);
        }
        let raw = self.retry(|| self.fetch_raw_once(mailbox, uid)).await?;
        let body = parse_body(&raw);
        if let Some(mid) = self.message_id(mailbox.id, uid)? {
            self.with_store(|s| s.save_body(mid, &body))?;
        }
        Ok(body)
    }

    async fn fetch_attachment(&self, mailbox: &Mailbox, uid: u32, index: usize) -> Result<Vec<u8>> {
        // No se cachean bytes de adjuntos: se descarga el mensaje completo.
        let raw = self.retry(|| self.fetch_raw_once(mailbox, uid)).await?;
        attachment_contents(&raw, index)
            .ok_or_else(|| Error::NotFound(format!("adjunto {index} del mensaje uid {uid}")))
    }

    async fn fetch_attachment_to(
        &self,
        mailbox: &Mailbox,
        uid: u32,
        index: usize,
        dest: &Path,
        progress: ProgressFn,
        cancel: CancelToken,
    ) -> Result<u64> {
        let meta = self.cached_attachment(mailbox, uid, index)?;
        // Sin número de parte (cuerpo cacheado antes de existir) o sin cuerpo en caché:
        // camino antiguo, mensaje completo.
        let Some((att, part)) = attach::chunked_plan(meta) else {
            return correo_core::fetch_attachment_to_via_whole(
                self, mailbox, uid, index, dest, &progress, &cancel,
            )
            .await;
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let file = std::fs::File::create(dest)
            .map_err(|e| Error::Store(format!("creando {}: {e}", dest.display())))?;
        let mut sink = BufWriter::new(file);
        let mut src = ImapChunkSource {
            backend: self,
            mailbox,
            uid,
            part,
        };
        let result = attach::download(
            &mut src,
            att.encoding.as_deref(),
            att.encoded_size,
            CHUNK_SIZE,
            &mut sink,
            &*progress,
            &cancel,
        )
        .await;
        let result = result.and_then(|n| {
            sink.flush()
                .map_err(|e| Error::Store(format!("escribiendo el adjunto: {e}")))
                .map(|()| n)
        });
        drop(sink);
        if result.is_err() {
            let _ = std::fs::remove_file(dest);
        }
        result
    }

    async fn set_seen(&self, mailbox: &Mailbox, uid: u32, seen: bool) -> Result<()> {
        self.retry(|| self.set_seen_once(mailbox, uid, seen)).await
    }

    async fn move_to(&self, mailbox: &Mailbox, uid: u32, target: &Mailbox) -> Result<()> {
        // MOVE es reintentable; el fallback COPY+STORE+EXPUNGE no, porque un
        // reintento tras un corte duplicaría el mensaje en el destino.
        if self.retry(|| self.supports_move()).await? {
            self.retry(|| self.uid_move_once(mailbox, uid, target))
                .await
        } else {
            self.attempt(self.copy_move_once(mailbox, uid, target))
                .await
        }
    }

    async fn delete(&self, mailbox: &Mailbox, uid: u32) -> Result<()> {
        match self.trash_mailbox()? {
            Some(trash) if mailbox.role != MailboxRole::Trash && trash.id != mailbox.id => {
                self.move_to(mailbox, uid, &trash).await
            }
            _ => self.retry(|| self.expunge_uid_once(mailbox, uid)).await,
        }
    }

    async fn set_seen_many(&self, mailbox: &Mailbox, uids: &[u32], seen: bool) -> BatchReport {
        let mut report = BatchReport::default();
        for (set, chunk) in uid_sets(uids, BATCH_MAX_UIDS) {
            let r = self
                .retry(|| self.set_seen_set_once(mailbox, &set, seen))
                .await;
            report.record(&chunk, r);
        }
        report
    }

    async fn move_many(&self, mailbox: &Mailbox, uids: &[u32], target: &Mailbox) -> BatchReport {
        let native = match self.retry(|| self.supports_move()).await {
            Ok(native) => native,
            Err(e) => return BatchReport::all_failed(uids, &e),
        };
        let mut report = BatchReport::default();
        for (set, chunk) in uid_sets(uids, BATCH_MAX_UIDS) {
            // Igual que `move_to`: MOVE se reintenta; COPY + marcar + expurgar, no.
            let r = if native {
                self.retry(|| self.uid_move_set_once(mailbox, &set, &chunk, target))
                    .await
            } else {
                self.attempt(self.copy_move_set_once(mailbox, &set, &chunk, target))
                    .await
            };
            report.record(&chunk, r);
        }
        report
    }

    async fn delete_many(&self, mailbox: &Mailbox, uids: &[u32]) -> BatchReport {
        let trash = match self.trash_mailbox() {
            Ok(trash) => trash,
            Err(e) => return BatchReport::all_failed(uids, &e),
        };
        match trash {
            Some(trash) if mailbox.role != MailboxRole::Trash && trash.id != mailbox.id => {
                self.move_many(mailbox, uids, &trash).await
            }
            _ => {
                let mut report = BatchReport::default();
                for (set, chunk) in uid_sets(uids, BATCH_MAX_UIDS) {
                    let r = self
                        .retry(|| self.expunge_set_once(mailbox, &set, &chunk))
                        .await;
                    report.record(&chunk, r);
                }
                report
            }
        }
    }
}

// ----- Operaciones contra el servidor (una tentativa cada una) ----------

impl ImapBackend {
    async fn supports_move(&self) -> Result<bool> {
        Ok(self.conn().await?.caps.mov)
    }

    async fn list_mailboxes_once(&self) -> Result<Vec<Mailbox>> {
        let names: Vec<Name> = {
            let mut guard = self.conn().await?;
            let stream = imap(OP_TIMEOUT, guard.session.list(Some(""), Some("*"))).await?;
            imap(OP_TIMEOUT, stream.try_collect()).await?
        };

        let existing = self.with_store(|s| s.list_mailboxes(self.account_id))?;
        let mut out = Vec::new();
        for name in names {
            let attrs = attribute_strings(name.attributes());
            if !parse::is_selectable(&attrs) {
                continue;
            }
            let path = name.name().to_owned();
            let delimiter = parse::delimiter_char(name.delimiter());
            let role = mailbox_role(&attrs, &path);
            // upsert_mailbox sobrescribe uid_validity y unread: se conservan los guardados.
            let prev = existing.iter().find(|m| m.path == path);
            let mailbox = Mailbox {
                id: 0,
                account_id: self.account_id,
                path,
                role,
                uid_validity: prev.and_then(|m| m.uid_validity),
                unread: prev.map(|m| m.unread).unwrap_or(0),
                delimiter,
            };
            let id = self.with_store(|s| s.upsert_mailbox(&mailbox))?;
            out.push(Mailbox { id, ..mailbox });
        }
        Ok(out)
    }

    async fn sync_mailbox_once(&self, mailbox: &Mailbox, days: u32) -> Result<usize> {
        let today = today_days(now_unix());
        let cutoff_day = today - i64::from(days);
        let cutoff_ts = cutoff_day * 86_400;
        let since = imap_date(cutoff_day);

        let mut guard = self.conn().await?;
        let conn = &mut *guard;

        let selected = imap(OP_TIMEOUT, conn.session.select(&mailbox.path)).await?;

        // UIDVALIDITY: si cambió, la caché de la carpeta ya no sirve.
        let stored = self.with_store(|s| s.get_mailbox(mailbox.id))?;
        let mut wiped = false;
        if let (Some(new), Some(old)) = (selected.uid_validity, stored.uid_validity)
            && new != old
        {
            self.with_store(|s| s.wipe_mailbox(mailbox.id))?;
            wiped = true;
        }
        let mut state = stored.clone();
        state.uid_validity = selected.uid_validity.or(stored.uid_validity);
        state.unread = selected.unseen.unwrap_or(0);
        self.with_store(|s| s.upsert_mailbox(&state))?;

        // Servidor: UIDs dentro de la ventana.
        let server: BTreeSet<u32> = imap(
            OP_TIMEOUT,
            conn.session.uid_search(format!("SINCE {since}")),
        )
        .await?
        .into_iter()
        .collect();

        // Caché: estado actual de la carpeta.
        let cached: Vec<MessageHeader> = if wiped {
            Vec::new()
        } else {
            self.cached_headers(mailbox.id)?
        };
        let cached_all: BTreeSet<u32> = cached.iter().map(|h| h.uid).collect();
        let cached_in_window: BTreeSet<u32> = cached
            .iter()
            .filter(|h| h.date >= cutoff_ts)
            .map(|h| h.uid)
            .collect();
        let cached_flags: BTreeMap<u32, Flags> =
            cached.iter().map(|h| (h.uid, h.flags.clone())).collect();

        let plan = plan_sync(&server, &cached_all, &cached_in_window);
        let mut changed = 0usize;

        // Borrados en servidor dentro de la ventana.
        for uid in &plan.removed_uids {
            self.with_store(|s| s.delete_uid(mailbox.id, *uid))?;
        }

        // Flags de los ya conocidos (límite por lote).
        for chunk in plan.known_uids.chunks(BATCH_SIZE) {
            let stream = imap(OP_TIMEOUT, conn.session.uid_fetch(uid_set(chunk), "FLAGS")).await?;
            let fetches: Vec<Fetch> = imap(OP_TIMEOUT, stream.try_collect()).await?;
            for f in &fetches {
                let Some(uid) = f.uid else { continue };
                let Some(old) = cached_flags.get(&uid) else {
                    continue;
                };
                let new = flags_from(f.flags());
                if !same_flags(old, &new) {
                    self.cache_flags(mailbox.id, uid, &new)?;
                    changed += 1;
                }
            }
        }

        // Cabeceras nuevas.
        let gmail = conn.caps.gmail_labels;
        let mut query = format!("(UID FLAGS INTERNALDATE ENVELOPE BODYSTRUCTURE {HEADER_FIELDS}");
        if gmail {
            query.push_str(" X-GM-LABELS");
        }
        query.push(')');

        for chunk in plan.new_uids.chunks(BATCH_SIZE) {
            let stream = imap(OP_TIMEOUT, conn.session.uid_fetch(uid_set(chunk), &query)).await?;
            let fetches: Vec<Fetch> = imap(OP_TIMEOUT, stream.try_collect()).await?;

            let mut headers = Vec::with_capacity(fetches.len());
            for f in &fetches {
                let Some(uid) = f.uid else { continue };
                let meta = FetchMeta {
                    mailbox_id: mailbox.id,
                    uid,
                    flags: flags_from(f.flags()),
                    internal_date: f.internal_date().map(|d| d.timestamp()).unwrap_or(0),
                    has_attachments: f.bodystructure().is_some_and(parse::has_attachments),
                    labels: f
                        .gmail_labels()
                        .map(|l| l.iter().map(|s| s.to_string()).collect())
                        .unwrap_or_default(),
                };
                headers.push(parse::build_header(f.header(), f.envelope(), meta));
            }
            if !headers.is_empty() {
                changed += headers.len();
                self.with_store(|s| s.upsert_headers(mailbox.id, &headers))?;
            }
        }

        Ok(changed)
    }

    /// Descarga el mensaje completo (BODY.PEEK[]) y devuelve sus bytes.
    async fn fetch_raw_once(&self, mailbox: &Mailbox, uid: u32) -> Result<Vec<u8>> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        self.select_checked(mailbox, conn).await?;
        let stream = imap(
            OP_TIMEOUT,
            conn.session.uid_fetch(uid.to_string(), "BODY.PEEK[]"),
        )
        .await?;
        let fetches: Vec<Fetch> = imap(OP_TIMEOUT, stream.try_collect()).await?;
        fetches
            .iter()
            .find_map(|f| f.body().map(<[u8]>::to_vec))
            .ok_or_else(|| Error::NotFound(format!("mensaje uid {uid} en {}", mailbox.path)))
    }

    /// Metadatos del adjunto `index` según el cuerpo cacheado, si lo hay.
    fn cached_attachment(
        &self,
        mailbox: &Mailbox,
        uid: u32,
        index: usize,
    ) -> Result<Option<Attachment>> {
        let Some(mid) = self.message_id(mailbox.id, uid)? else {
            return Ok(None);
        };
        Ok(self
            .with_store(|s| s.get_body(mid))?
            .and_then(|b| b.attachments.into_iter().nth(index)))
    }

    /// Un trozo de una parte: la sesión solo se retiene durante este FETCH, de modo que
    /// otras operaciones se intercalan entre trozos. El límite de tiempo es por trozo.
    async fn fetch_chunk_once(
        &self,
        mailbox: &Mailbox,
        uid: u32,
        part: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>> {
        let path = attach::parse_part_path(part)
            .ok_or_else(|| Error::Protocol(format!("número de parte no válido: {part}")))?;
        let query = attach::fetch_query(part, offset, len);
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        let fut = async {
            let selected = conn
                .session
                .select(&mailbox.path)
                .await
                .map_err(map_imap_error)?;
            self.guard_uid_validity(mailbox, selected.uid_validity)?;
            let stream = conn
                .session
                .uid_fetch(uid.to_string(), &query)
                .await
                .map_err(map_imap_error)?;
            let fetches: Vec<Fetch> = stream.try_collect().await.map_err(map_imap_error)?;
            let section = async_imap::imap_proto::SectionPath::Part(path, None);
            let data = fetches
                .iter()
                .find_map(|f| f.section(&section).map(<[u8]>::to_vec));
            match data {
                Some(d) => Ok(d),
                // Sin sección: fuera de rango (vacío) o mensaje inexistente.
                None if fetches.iter().any(|f| f.uid == Some(uid)) => Ok(Vec::new()),
                None => Err(Error::NotFound(format!(
                    "mensaje uid {uid} en {}",
                    mailbox.path
                ))),
            }
        };
        match tokio::time::timeout(OP_TIMEOUT, fut).await {
            Ok(r) => r,
            Err(_) => Err(timeout_error(OP_TIMEOUT)),
        }
    }

    async fn set_seen_once(&self, mailbox: &Mailbox, uid: u32, seen: bool) -> Result<()> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        self.select_checked(mailbox, conn).await?;
        // Sin .SILENT: el servidor devuelve los FLAGS completos, que van a la caché.
        let op = if seen {
            "+FLAGS (\\Seen)"
        } else {
            "-FLAGS (\\Seen)"
        };
        let stream = imap(OP_TIMEOUT, conn.session.uid_store(uid.to_string(), op)).await?;
        let fetches: Vec<Fetch> = imap(OP_TIMEOUT, stream.try_collect()).await?;
        drop(guard);
        if let Some(f) = fetches.iter().find(|f| f.uid == Some(uid)) {
            let flags = flags_from(f.flags());
            self.cache_flags(mailbox.id, uid, &flags)?;
        }
        Ok(())
    }

    /// UID MOVE (RFC 6851). Solo se llama si el servidor anuncia MOVE.
    async fn uid_move_once(&self, mailbox: &Mailbox, uid: u32, target: &Mailbox) -> Result<()> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        self.select_checked(mailbox, conn).await?;
        imap(
            OP_TIMEOUT,
            conn.session.uid_mv(uid.to_string(), &target.path),
        )
        .await?;
        drop(guard);
        self.with_store(|s| s.delete_uid(mailbox.id, uid))
    }

    /// Fallback sin MOVE: UID COPY al destino y luego se marca \Deleted el original.
    ///
    /// Nunca hace EXPUNGE global: solo se expurga el UID si hay UIDPLUS. Sin él
    /// el original queda marcado \Deleted y se devuelve error.
    async fn copy_move_once(&self, mailbox: &Mailbox, uid: u32, target: &Mailbox) -> Result<()> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        self.select_checked(mailbox, conn).await?;
        let uid_str = uid.to_string();
        imap(OP_TIMEOUT, conn.session.uid_copy(&uid_str, &target.path)).await?;
        store_deleted(conn, &uid_str).await?;
        if !conn.caps.uidplus {
            return Err(Error::Protocol(format!(
                "copiado a '{}', pero el servidor no admite UIDPLUS: el original quedó marcado como borrado sin expurgar",
                target.path
            )));
        }
        uid_expunge(conn, &uid_str).await?;
        drop(guard);
        self.with_store(|s| s.delete_uid(mailbox.id, uid))
    }

    /// Marca \Deleted y expurga solo ese UID (UIDPLUS). Sin UIDPLUS no expurga
    /// nada: devuelve error y el mensaje queda marcado.
    async fn expunge_uid_once(&self, mailbox: &Mailbox, uid: u32) -> Result<()> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        self.select_checked(mailbox, conn).await?;
        let uid_str = uid.to_string();
        store_deleted(conn, &uid_str).await?;
        if !conn.caps.uidplus {
            return Err(Error::Protocol(
                "el servidor no admite UIDPLUS: el mensaje quedó marcado como borrado y no se expurgó".into(),
            ));
        }
        uid_expunge(conn, &uid_str).await?;
        drop(guard);
        self.with_store(|s| s.delete_uid(mailbox.id, uid))
    }
}

// ----- Variantes por lotes: un conjunto de UID por orden ----------------

impl ImapBackend {
    /// `UID STORE <set> +/-FLAGS (\Seen)` y actualización de la caché con los flags devueltos.
    async fn set_seen_set_once(&self, mailbox: &Mailbox, set: &str, seen: bool) -> Result<()> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        self.select_checked(mailbox, conn).await?;
        let op = if seen {
            "+FLAGS (\\Seen)"
        } else {
            "-FLAGS (\\Seen)"
        };
        let stream = imap(OP_TIMEOUT, conn.session.uid_store(set, op)).await?;
        let fetches: Vec<Fetch> = imap(OP_TIMEOUT, stream.try_collect()).await?;
        drop(guard);
        for f in &fetches {
            if let Some(uid) = f.uid {
                self.cache_flags(mailbox.id, uid, &flags_from(f.flags()))?;
            }
        }
        Ok(())
    }

    /// `UID MOVE <set>` (solo si el servidor anuncia MOVE).
    async fn uid_move_set_once(
        &self,
        mailbox: &Mailbox,
        set: &str,
        uids: &[u32],
        target: &Mailbox,
    ) -> Result<()> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        self.select_checked(mailbox, conn).await?;
        imap(OP_TIMEOUT, conn.session.uid_mv(set, &target.path)).await?;
        drop(guard);
        self.forget_cached(mailbox.id, uids)
    }

    /// Sin MOVE: `UID COPY <set>`, marcar \Deleted y `UID EXPUNGE <set>` (solo con UIDPLUS;
    /// jamás un EXPUNGE global).
    async fn copy_move_set_once(
        &self,
        mailbox: &Mailbox,
        set: &str,
        uids: &[u32],
        target: &Mailbox,
    ) -> Result<()> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        self.select_checked(mailbox, conn).await?;
        imap(OP_TIMEOUT, conn.session.uid_copy(set, &target.path)).await?;
        store_deleted(conn, set).await?;
        if !conn.caps.uidplus {
            return Err(Error::Protocol(format!(
                "copiados a '{}', pero el servidor no admite UIDPLUS: los originales quedaron marcados como borrados sin expurgar",
                target.path
            )));
        }
        uid_expunge(conn, set).await?;
        drop(guard);
        self.forget_cached(mailbox.id, uids)
    }

    /// Marca \Deleted y expurga solo ese conjunto (UIDPLUS).
    async fn expunge_set_once(&self, mailbox: &Mailbox, set: &str, uids: &[u32]) -> Result<()> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        self.select_checked(mailbox, conn).await?;
        store_deleted(conn, set).await?;
        if !conn.caps.uidplus {
            return Err(Error::Protocol(
                "el servidor no admite UIDPLUS: los mensajes quedaron marcados como borrados y no se expurgaron".into(),
            ));
        }
        uid_expunge(conn, set).await?;
        drop(guard);
        self.forget_cached(mailbox.id, uids)
    }

    fn forget_cached(&self, mailbox_id: MailboxId, uids: &[u32]) -> Result<()> {
        for &uid in uids {
            self.with_store(|s| s.delete_uid(mailbox_id, uid))?;
        }
        Ok(())
    }
}

/// STORE +FLAGS.SILENT \Deleted sobre un UID.
async fn store_deleted(conn: &mut Conn, uid_str: &str) -> Result<()> {
    let stream = imap(
        OP_TIMEOUT,
        conn.session.uid_store(uid_str, "+FLAGS.SILENT (\\Deleted)"),
    )
    .await?;
    let _: Vec<Fetch> = imap(OP_TIMEOUT, stream.try_collect()).await?;
    Ok(())
}

/// UID EXPUNGE (UIDPLUS): expurga solo ese UID.
async fn uid_expunge(conn: &mut Conn, uid_str: &str) -> Result<()> {
    let stream = imap(OP_TIMEOUT, conn.session.uid_expunge(uid_str)).await?;
    let _: Vec<u32> = imap(OP_TIMEOUT, stream.try_collect()).await?;
    Ok(())
}

#[cfg(test)]
mod idle_tests {
    use super::*;

    #[test]
    fn idle_outer_limit_is_25_min_plus_30_s() {
        assert_eq!(IDLE_MAX + IDLE_GRACE, Duration::from_secs(25 * 60 + 30));
    }

    /// Compilación: la espera IDLE debe poder lanzarse con `tokio::spawn`.
    #[allow(dead_code)]
    fn idle_future_is_send<'a>(
        backend: &'a ImapBackend,
        mailbox: &'a Mailbox,
    ) -> impl Future<Output = Result<IdleEvent>> + Send + 'a {
        backend.idle_inbox(mailbox, IDLE_MAX)
    }

    #[test]
    fn idle_max_is_25_minutes() {
        assert_eq!(IDLE_MAX, Duration::from_secs(25 * 60));
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;
    use correo_core::{AccessToken, Account, Provider};
    use correo_store::Store;

    struct NoTokens;

    #[async_trait]
    impl TokenProvider for NoTokens {
        async fn access_token(&self, _account: AccountId) -> Result<AccessToken> {
            Err(Error::Auth("sin token en las pruebas".into()))
        }
    }

    fn backend_with_inbox(stored_validity: Option<u32>) -> (ImapBackend, Mailbox) {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .create_account(&Account {
                id: 0,
                provider: Provider::Gmail,
                email: "yo@example.com".into(),
                display_name: "Yo".into(),
                sync_days: 30,
            })
            .unwrap();
        let id = store
            .upsert_mailbox(&Mailbox {
                id: 0,
                account_id,
                path: "INBOX".into(),
                role: MailboxRole::Inbox,
                uid_validity: stored_validity,
                unread: 0,
                delimiter: None,
            })
            .unwrap();
        let mailbox = store.get_mailbox(id).unwrap();
        // Un mensaje cacheado: debe desaparecer si la carpeta cambió.
        let meta = parse::FetchMeta {
            mailbox_id: id,
            uid: 1,
            flags: Default::default(),
            internal_date: 0,
            has_attachments: false,
            labels: vec![],
        };
        let header = parse::build_header(None, None, meta);
        store.upsert_headers(id, &[header]).unwrap();

        let backend = ImapBackend::new(
            "imap.example.com",
            IMAPS_PORT,
            account_id,
            "yo@example.com",
            Arc::new(NoTokens),
            Arc::new(Mutex::new(store)),
        );
        (backend, mailbox)
    }

    #[test]
    fn uidvalidity_change_wipes_cache_and_refuses_the_action() {
        let (backend, mailbox) = backend_with_inbox(Some(1));
        let err = backend
            .guard_uid_validity(&mailbox, Some(2))
            .expect_err("debe rechazar la acción");
        assert!(
            matches!(&err, Error::Protocol(m) if m == "la carpeta cambió; vuelve a sincronizar"),
            "{err:?}"
        );
        let cached = backend
            .with_store(|s| s.list_headers(mailbox.id, 10, 0))
            .unwrap();
        assert!(cached.is_empty(), "la caché debe vaciarse");
        let stored = backend.with_store(|s| s.get_mailbox(mailbox.id)).unwrap();
        assert_eq!(stored.uid_validity, Some(2));
    }

    #[test]
    fn same_uidvalidity_keeps_cache_and_allows_the_action() {
        let (backend, mailbox) = backend_with_inbox(Some(5));
        backend.guard_uid_validity(&mailbox, Some(5)).unwrap();
        let cached = backend
            .with_store(|s| s.list_headers(mailbox.id, 10, 0))
            .unwrap();
        assert_eq!(cached.len(), 1);
    }

    #[test]
    fn sync_days_are_capped() {
        assert_eq!(effective_sync_days(30), 30);
        assert_eq!(effective_sync_days(MAX_SYNC_DAYS), 3650);
        assert_eq!(effective_sync_days(u32::MAX), 3650);
    }
}

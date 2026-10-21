//! `ImapBackend`: implementación de `MailBackend` sobre IMAP.
//!
//! Una sesión por backend, abierta de forma perezosa. Si una operación falla
//! por red, la sesión se descarta y la operación se reintenta una vez con una
//! sesión nueva.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_imap::extensions::idle::IdleResponse;
use async_imap::types::{Fetch, Name};
use async_trait::async_trait;
use correo_core::{
    AccountId, Error, Flags, Mailbox, MailboxId, MailboxRole, MessageBody, MessageHeader,
    MessageId, Result, TokenProvider,
};
use correo_store::Store;
use futures::TryStreamExt;
use tokio::sync::{MappedMutexGuard, Mutex as AsyncMutex, MutexGuard};

use crate::conn::{self, Conn, is_network, map_imap_error};
use crate::parse::{
    self, BATCH_SIZE, FetchMeta, attachment_contents, attribute_strings, flags_from, imap_date,
    mailbox_role, parse_body, plan_sync, same_flags, today_days, uid_set,
};

pub const GMAIL_HOST: &str = "imap.gmail.com";
pub const OUTLOOK_HOST: &str = "outlook.office365.com";
pub const IMAPS_PORT: u16 = 993;

/// Duración máxima de una espera IDLE; los servidores suelen cortarla hacia los 29 min.
pub const IDLE_MAX: Duration = Duration::from_secs(25 * 60);

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

    /// Espera IDLE sobre `mailbox` en una conexión propia (IDLE bloquea la sesión
    /// de operaciones). Devuelve al haber cambios o al expirar `max`. La
    /// conexión se descarta al terminar; el planificador decide cuándo reiniciar.
    ///
    /// Si el servidor no anuncia IDLE devuelve `Error::Protocol("sin IDLE")`.
    pub async fn idle_inbox(&self, mailbox: &Mailbox, max: Duration) -> Result<IdleEvent> {
        let mut conn = self.connect().await?;
        if !conn.caps.idle {
            return Err(Error::Protocol("sin IDLE".into()));
        }
        conn.session
            .select(&mailbox.path)
            .await
            .map_err(map_imap_error)?;

        let mut handle = conn.session.idle();
        handle.init().await.map_err(map_imap_error)?;

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

    async fn set_seen(&self, mailbox: &Mailbox, uid: u32, seen: bool) -> Result<()> {
        self.retry(|| self.set_seen_once(mailbox, uid, seen)).await
    }

    async fn move_to(&self, mailbox: &Mailbox, uid: u32, target: &Mailbox) -> Result<()> {
        self.retry(|| self.move_to_once(mailbox, uid, target)).await
    }

    async fn delete(&self, mailbox: &Mailbox, uid: u32) -> Result<()> {
        match self.trash_mailbox()? {
            Some(trash) if mailbox.role != MailboxRole::Trash && trash.id != mailbox.id => {
                self.move_to(mailbox, uid, &trash).await
            }
            _ => self.retry(|| self.expunge_uid_once(mailbox, uid)).await,
        }
    }
}

// ----- Operaciones contra el servidor (una tentativa cada una) ----------

impl ImapBackend {
    async fn list_mailboxes_once(&self) -> Result<Vec<Mailbox>> {
        let names: Vec<Name> = {
            let mut guard = self.conn().await?;
            let stream = guard
                .session
                .list(Some(""), Some("*"))
                .await
                .map_err(map_imap_error)?;
            stream.try_collect().await.map_err(map_imap_error)?
        };

        let existing = self.with_store(|s| s.list_mailboxes(self.account_id))?;
        let mut out = Vec::new();
        for name in names {
            let attrs = attribute_strings(name.attributes());
            if !parse::is_selectable(&attrs) {
                continue;
            }
            let path = name.name().to_owned();
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

        let selected = conn
            .session
            .select(&mailbox.path)
            .await
            .map_err(map_imap_error)?;

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
        let server: BTreeSet<u32> = conn
            .session
            .uid_search(format!("SINCE {since}"))
            .await
            .map_err(map_imap_error)?
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

        // Flags de los ya conocidos.
        for chunk in plan.known_uids.chunks(BATCH_SIZE) {
            let fetches: Vec<Fetch> = conn
                .session
                .uid_fetch(uid_set(chunk), "FLAGS")
                .await
                .map_err(map_imap_error)?
                .try_collect()
                .await
                .map_err(map_imap_error)?;
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
            let fetches: Vec<Fetch> = conn
                .session
                .uid_fetch(uid_set(chunk), &query)
                .await
                .map_err(map_imap_error)?
                .try_collect()
                .await
                .map_err(map_imap_error)?;

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
        conn.session
            .select(&mailbox.path)
            .await
            .map_err(map_imap_error)?;
        let fetches: Vec<Fetch> = conn
            .session
            .uid_fetch(uid.to_string(), "BODY.PEEK[]")
            .await
            .map_err(map_imap_error)?
            .try_collect()
            .await
            .map_err(map_imap_error)?;
        fetches
            .iter()
            .find_map(|f| f.body().map(<[u8]>::to_vec))
            .ok_or_else(|| Error::NotFound(format!("mensaje uid {uid} en {}", mailbox.path)))
    }

    async fn set_seen_once(&self, mailbox: &Mailbox, uid: u32, seen: bool) -> Result<()> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        conn.session
            .select(&mailbox.path)
            .await
            .map_err(map_imap_error)?;
        // Sin .SILENT: el servidor devuelve los FLAGS completos, que van a la caché.
        let op = if seen {
            "+FLAGS (\\Seen)"
        } else {
            "-FLAGS (\\Seen)"
        };
        let fetches: Vec<Fetch> = conn
            .session
            .uid_store(uid.to_string(), op)
            .await
            .map_err(map_imap_error)?
            .try_collect()
            .await
            .map_err(map_imap_error)?;
        drop(guard);
        if let Some(f) = fetches.iter().find(|f| f.uid == Some(uid)) {
            let flags = flags_from(f.flags());
            self.cache_flags(mailbox.id, uid, &flags)?;
        }
        Ok(())
    }

    async fn move_to_once(&self, mailbox: &Mailbox, uid: u32, target: &Mailbox) -> Result<()> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        conn.session
            .select(&mailbox.path)
            .await
            .map_err(map_imap_error)?;
        let uid_str = uid.to_string();

        if conn.caps.mov {
            conn.session
                .uid_mv(&uid_str, &target.path)
                .await
                .map_err(map_imap_error)?;
        } else {
            conn.session
                .uid_copy(&uid_str, &target.path)
                .await
                .map_err(map_imap_error)?;
            mark_deleted_and_expunge(conn, &uid_str).await?;
        }
        drop(guard);
        self.with_store(|s| s.delete_uid(mailbox.id, uid))
    }

    /// Marca \Deleted y expunge (solo el uid si hay UIDPLUS) y quita la caché.
    async fn expunge_uid_once(&self, mailbox: &Mailbox, uid: u32) -> Result<()> {
        let mut guard = self.conn().await?;
        let conn = &mut *guard;
        conn.session
            .select(&mailbox.path)
            .await
            .map_err(map_imap_error)?;
        mark_deleted_and_expunge(conn, &uid.to_string()).await?;
        drop(guard);
        self.with_store(|s| s.delete_uid(mailbox.id, uid))
    }
}

/// STORE +FLAGS.SILENT \Deleted seguido de expunge; con UIDPLUS solo ese UID.
async fn mark_deleted_and_expunge(conn: &mut Conn, uid_str: &str) -> Result<()> {
    let _: Vec<Fetch> = conn
        .session
        .uid_store(uid_str, "+FLAGS.SILENT (\\Deleted)")
        .await
        .map_err(map_imap_error)?
        .try_collect()
        .await
        .map_err(map_imap_error)?;
    if conn.caps.uidplus {
        let _: Vec<u32> = conn
            .session
            .uid_expunge(uid_str)
            .await
            .map_err(map_imap_error)?
            .try_collect()
            .await
            .map_err(map_imap_error)?;
    } else {
        let _: Vec<u32> = conn
            .session
            .expunge()
            .await
            .map_err(map_imap_error)?
            .try_collect()
            .await
            .map_err(map_imap_error)?;
    }
    Ok(())
}

#[cfg(test)]
mod idle_tests {
    use super::*;

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

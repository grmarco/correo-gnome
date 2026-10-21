//! Caché local en SQLite: cuentas, carpetas, cabeceras, cuerpos, contactos
//! y búsqueda de texto completo (FTS5).
//!
//! `Store` envuelve una `rusqlite::Connection`: es `Send` pero no `Sync`.
//! Para usarlo desde varios hilos, envolverlo en un `Mutex` o abrir una
//! instancia por hilo.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use correo_core::{
    Account, AccountId, Address, Contact, Error, Flags, Mailbox, MailboxId, MailboxRole,
    MessageBody, MessageHeader, MessageId, Provider, Result,
};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::de::DeserializeOwned;

/// Migraciones en orden. La versión de esquema es `user_version` = número
/// de migraciones aplicadas. Nunca editar una migración ya publicada: añadir
/// una nueva al final.
const MIGRATIONS: &[&str] = &[r#"
    CREATE TABLE accounts (
        id           INTEGER PRIMARY KEY,
        provider     TEXT NOT NULL,
        email        TEXT NOT NULL UNIQUE COLLATE NOCASE,
        display_name TEXT NOT NULL DEFAULT '',
        sync_days    INTEGER NOT NULL DEFAULT 30
    );

    CREATE TABLE mailboxes (
        id           INTEGER PRIMARY KEY,
        account_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        path         TEXT NOT NULL,
        role         TEXT NOT NULL,
        uid_validity INTEGER,
        unread       INTEGER NOT NULL DEFAULT 0,
        UNIQUE (account_id, path)
    );

    CREATE TABLE messages (
        id              INTEGER PRIMARY KEY,
        mailbox_id      INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
        uid             INTEGER NOT NULL,
        message_id      TEXT,
        in_reply_to     TEXT,
        references_json TEXT NOT NULL DEFAULT '[]',
        subject         TEXT NOT NULL DEFAULT '',
        from_json       TEXT NOT NULL DEFAULT '[]',
        to_json         TEXT NOT NULL DEFAULT '[]',
        cc_json         TEXT NOT NULL DEFAULT '[]',
        date            INTEGER NOT NULL,
        seen            INTEGER NOT NULL DEFAULT 0,
        flagged         INTEGER NOT NULL DEFAULT 0,
        answered        INTEGER NOT NULL DEFAULT 0,
        draft           INTEGER NOT NULL DEFAULT 0,
        has_attachments INTEGER NOT NULL DEFAULT 0,
        snippet         TEXT,
        labels_json     TEXT NOT NULL DEFAULT '[]',
        UNIQUE (mailbox_id, uid)
    );
    CREATE INDEX messages_by_date ON messages (mailbox_id, date DESC);
    CREATE INDEX messages_by_message_id ON messages (message_id);

    -- Cuerpo completo (MessageBody en JSON) y texto plano derivado para FTS.
    CREATE TABLE bodies (
        message_id  INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
        body_json   TEXT NOT NULL,
        search_text TEXT NOT NULL DEFAULT ''
    );

    CREATE TABLE contacts (
        email  TEXT PRIMARY KEY COLLATE NOCASE,
        name   TEXT,
        source TEXT NOT NULL
    );
    CREATE INDEX contacts_by_name ON contacts (name COLLATE NOCASE);

    -- Tabla FTS5 independiente; su rowid es messages.id. Se mantiene desde
    -- este módulo (fts_refresh / fts_remove) en cada escritura.
    CREATE VIRTUAL TABLE messages_fts USING fts5(
        subject, sender, snippet, body,
        tokenize = 'unicode61 remove_diacritics 2'
    );
"#];

/// Columnas de `messages` en el orden que espera `header_from_row`.
const HEADER_COLS: &str = "id, mailbox_id, uid, message_id, in_reply_to, references_json, \
    subject, from_json, to_json, cc_json, date, seen, flagged, answered, draft, \
    has_attachments, snippet, labels_json";

/// Columnas de `mailboxes` en el orden que espera `mailbox_from_row`.
const MAILBOX_COLS: &str = "id, account_id, path, role, uid_validity, unread";

/// Columnas de `accounts` en el orden que espera `account_from_row`.
const ACCOUNT_COLS: &str = "id, provider, email, display_name, sync_days";

const SECONDS_PER_DAY: i64 = 86_400;

/// Caché SQLite de la aplicación.
pub struct Store {
    conn: Connection,
}

fn se(e: impl std::fmt::Display) -> Error {
    Error::Store(e.to_string())
}

impl Store {
    /// Abre (o crea) la base de datos en `path`, con WAL y claves foráneas.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(se)?;
        Self::init(conn)
    }

    /// Base de datos temporal en memoria, sin persistencia. Útil para tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(se)?;
        Self::init(conn)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5)).map_err(se)?;
        conn.pragma_update(None, "foreign_keys", true).map_err(se)?;
        // Devuelve una fila con el modo resultante; en memoria responde "memory".
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .map_err(se)?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    // ----- Cuentas -------------------------------------------------------

    /// Inserta una cuenta nueva. `account.id` se ignora; devuelve el id asignado.
    pub fn create_account(&self, account: &Account) -> Result<AccountId> {
        self.conn
            .execute(
                "INSERT INTO accounts (provider, email, display_name, sync_days) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    provider_to_str(account.provider),
                    account.email,
                    account.display_name,
                    account.sync_days
                ],
            )
            .map_err(se)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_account(&self, id: AccountId) -> Result<Account> {
        self.conn
            .query_row(
                &format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE id = ?1"),
                [id],
                account_from_row,
            )
            .optional()
            .map_err(se)?
            .ok_or_else(|| not_found("cuenta", id))
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {ACCOUNT_COLS} FROM accounts ORDER BY id"))
            .map_err(se)?;
        let rows = stmt.query_map([], account_from_row).map_err(se)?;
        rows.collect::<rusqlite::Result<_>>().map_err(se)
    }

    /// Actualiza provider, email, nombre y días de sincronización de `account.id`.
    pub fn update_account(&self, account: &Account) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE accounts SET provider = ?2, email = ?3, display_name = ?4, \
                 sync_days = ?5 WHERE id = ?1",
                params![
                    account.id,
                    provider_to_str(account.provider),
                    account.email,
                    account.display_name,
                    account.sync_days
                ],
            )
            .map_err(se)?;
        if changed == 0 {
            return Err(not_found("cuenta", account.id));
        }
        Ok(())
    }

    /// Borra la cuenta con sus carpetas, cabeceras, cuerpos y entradas FTS.
    pub fn delete_account(&self, id: AccountId) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(se)?;
        fts_remove_where(
            &tx,
            "rowid IN (SELECT m.id FROM messages m JOIN mailboxes mb \
             ON mb.id = m.mailbox_id WHERE mb.account_id = ?1)",
            [id],
        )?;
        let changed = tx
            .execute("DELETE FROM accounts WHERE id = ?1", [id])
            .map_err(se)?;
        if changed == 0 {
            return Err(not_found("cuenta", id));
        }
        tx.commit().map_err(se)
    }

    // ----- Carpetas ------------------------------------------------------

    /// Inserta o actualiza una carpeta por (account_id, path) y devuelve su id.
    /// Si `uid_validity` cambia, el llamador debe invocar `wipe_mailbox`.
    pub fn upsert_mailbox(&self, mailbox: &Mailbox) -> Result<MailboxId> {
        self.conn
            .query_row(
                "INSERT INTO mailboxes (account_id, path, role, uid_validity, unread) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT (account_id, path) DO UPDATE SET \
                     role = excluded.role, \
                     uid_validity = excluded.uid_validity, \
                     unread = excluded.unread \
                 RETURNING id",
                params![
                    mailbox.account_id,
                    mailbox.path,
                    role_to_str(mailbox.role),
                    mailbox.uid_validity,
                    mailbox.unread
                ],
                |row| row.get(0),
            )
            .map_err(se)
    }

    pub fn get_mailbox(&self, id: MailboxId) -> Result<Mailbox> {
        self.conn
            .query_row(
                &format!("SELECT {MAILBOX_COLS} FROM mailboxes WHERE id = ?1"),
                [id],
                mailbox_from_row,
            )
            .optional()
            .map_err(se)?
            .ok_or_else(|| not_found("carpeta", id))
    }

    pub fn list_mailboxes(&self, account_id: AccountId) -> Result<Vec<Mailbox>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {MAILBOX_COLS} FROM mailboxes WHERE account_id = ?1 ORDER BY path"
            ))
            .map_err(se)?;
        let rows = stmt.query_map([account_id], mailbox_from_row).map_err(se)?;
        rows.collect::<rusqlite::Result<_>>().map_err(se)
    }

    /// Borra una carpeta con sus mensajes y entradas FTS.
    pub fn delete_mailbox(&self, id: MailboxId) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(se)?;
        fts_remove_where(
            &tx,
            "rowid IN (SELECT id FROM messages WHERE mailbox_id = ?1)",
            [id],
        )?;
        let changed = tx
            .execute("DELETE FROM mailboxes WHERE id = ?1", [id])
            .map_err(se)?;
        if changed == 0 {
            return Err(not_found("carpeta", id));
        }
        tx.commit().map_err(se)
    }

    // ----- Mensajes (cabeceras) -----------------------------------------

    /// Inserta o actualiza cabeceras por (mailbox_id, uid). `mailbox_id` del
    /// argumento prevalece sobre `h.mailbox_id`; `h.id` se ignora. No toca
    /// los cuerpos ya guardados.
    pub fn upsert_headers(&self, mailbox_id: MailboxId, headers: &[MessageHeader]) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(se)?;
        for h in headers {
            let references = to_json(&h.references)?;
            let from = to_json(&h.from)?;
            let to = to_json(&h.to)?;
            let cc = to_json(&h.cc)?;
            let labels = to_json(&h.labels)?;
            let id: MessageId = tx
                .query_row(
                    "INSERT INTO messages (mailbox_id, uid, message_id, in_reply_to, \
                         references_json, subject, from_json, to_json, cc_json, date, \
                         seen, flagged, answered, draft, has_attachments, snippet, labels_json) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                         ?15, ?16, ?17) \
                     ON CONFLICT (mailbox_id, uid) DO UPDATE SET \
                         message_id = excluded.message_id, \
                         in_reply_to = excluded.in_reply_to, \
                         references_json = excluded.references_json, \
                         subject = excluded.subject, \
                         from_json = excluded.from_json, \
                         to_json = excluded.to_json, \
                         cc_json = excluded.cc_json, \
                         date = excluded.date, \
                         seen = excluded.seen, \
                         flagged = excluded.flagged, \
                         answered = excluded.answered, \
                         draft = excluded.draft, \
                         has_attachments = excluded.has_attachments, \
                         snippet = excluded.snippet, \
                         labels_json = excluded.labels_json \
                     RETURNING id",
                    params![
                        mailbox_id,
                        h.uid,
                        h.message_id,
                        h.in_reply_to,
                        references,
                        h.subject,
                        from,
                        to,
                        cc,
                        h.date,
                        h.flags.seen,
                        h.flags.flagged,
                        h.flags.answered,
                        h.flags.draft,
                        h.has_attachments,
                        h.snippet,
                        labels
                    ],
                    |row| row.get(0),
                )
                .map_err(se)?;
            fts_refresh(&tx, id)?;
        }
        tx.commit().map_err(se)
    }

    /// Cabeceras de una carpeta, de la más reciente a la más antigua.
    pub fn list_headers(
        &self,
        mailbox_id: MailboxId,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<MessageHeader>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {HEADER_COLS} FROM messages WHERE mailbox_id = ?1 \
                 ORDER BY date DESC, id DESC LIMIT ?2 OFFSET ?3"
            ))
            .map_err(se)?;
        let rows = stmt
            .query_map(
                params![mailbox_id, to_i64(limit), to_i64(offset)],
                header_from_row,
            )
            .map_err(se)?;
        rows.collect::<rusqlite::Result<_>>().map_err(se)
    }

    /// Cabecera por id interno.
    pub fn get_header(&self, id: MessageId) -> Result<Option<MessageHeader>> {
        self.conn
            .query_row(
                &format!("SELECT {HEADER_COLS} FROM messages WHERE id = ?1"),
                params![id],
                header_from_row,
            )
            .optional()
            .map_err(se)
    }

    /// Actualiza los flags de un mensaje identificado por (mailbox_id, uid).
    pub fn set_flags(&self, mailbox_id: MailboxId, uid: u32, flags: &Flags) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE messages SET seen = ?3, flagged = ?4, answered = ?5, draft = ?6 \
                 WHERE mailbox_id = ?1 AND uid = ?2",
                params![
                    mailbox_id,
                    uid,
                    flags.seen,
                    flags.flagged,
                    flags.answered,
                    flags.draft
                ],
            )
            .map_err(se)?;
        if changed == 0 {
            return Err(Error::NotFound(format!(
                "mensaje uid {uid} en carpeta {mailbox_id}"
            )));
        }
        Ok(())
    }

    /// Borra un mensaje por uid. Si no existe, no hace nada.
    /// Id interno de (carpeta, uid), o `None` si el mensaje no está cacheado.
    pub fn message_id(&self, mailbox_id: MailboxId, uid: u32) -> Result<Option<MessageId>> {
        self.conn
            .query_row(
                "SELECT id FROM messages WHERE mailbox_id = ?1 AND uid = ?2",
                params![mailbox_id, uid],
                |r| r.get(0),
            )
            .optional()
            .map_err(se)
    }

    pub fn delete_uid(&self, mailbox_id: MailboxId, uid: u32) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(se)?;
        fts_remove_where(
            &tx,
            "rowid IN (SELECT id FROM messages WHERE mailbox_id = ?1 AND uid = ?2)",
            params![mailbox_id, uid],
        )?;
        tx.execute(
            "DELETE FROM messages WHERE mailbox_id = ?1 AND uid = ?2",
            params![mailbox_id, uid],
        )
        .map_err(se)?;
        tx.commit().map_err(se)
    }

    /// Borra todos los mensajes de una carpeta (p. ej. tras cambiar UIDVALIDITY).
    pub fn wipe_mailbox(&self, mailbox_id: MailboxId) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(se)?;
        fts_remove_where(
            &tx,
            "rowid IN (SELECT id FROM messages WHERE mailbox_id = ?1)",
            [mailbox_id],
        )?;
        tx.execute("DELETE FROM messages WHERE mailbox_id = ?1", [mailbox_id])
            .map_err(se)?;
        tx.commit().map_err(se)
    }

    // ----- Cuerpos -------------------------------------------------------

    /// Guarda (o reemplaza) el cuerpo de un mensaje ya cacheado.
    pub fn save_body(&self, message_id: MessageId, body: &MessageBody) -> Result<()> {
        let body_json = to_json(body)?;
        let search_text = match (&body.text, &body.html) {
            (Some(text), _) if !text.trim().is_empty() => text.clone(),
            (_, Some(html)) => strip_tags(html),
            _ => String::new(),
        };
        let tx = self.conn.unchecked_transaction().map_err(se)?;
        tx.execute(
            "INSERT INTO bodies (message_id, body_json, search_text) VALUES (?1, ?2, ?3) \
             ON CONFLICT (message_id) DO UPDATE SET \
                 body_json = excluded.body_json, search_text = excluded.search_text",
            params![message_id, body_json, search_text],
        )
        .map_err(se)?;
        fts_refresh(&tx, message_id)?;
        tx.commit().map_err(se)
    }

    pub fn get_body(&self, message_id: MessageId) -> Result<Option<MessageBody>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT body_json FROM bodies WHERE message_id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(se)?;
        raw.map(|json| serde_json::from_str(&json).map_err(se))
            .transpose()
    }

    // ----- Contactos -----------------------------------------------------

    /// Inserta o actualiza contactos por email. Si el nuevo nombre es `None`,
    /// se conserva el anterior. Las entradas con email vacío se ignoran.
    pub fn upsert_contacts(&self, contacts: &[Contact]) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(se)?;
        for c in contacts.iter().filter(|c| !c.email.trim().is_empty()) {
            tx.execute(
                "INSERT INTO contacts (email, name, source) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (email) DO UPDATE SET \
                     name = COALESCE(excluded.name, contacts.name), \
                     source = excluded.source",
                params![c.email.trim(), c.name, c.source],
            )
            .map_err(se)?;
        }
        tx.commit().map_err(se)
    }

    /// Contactos cuyo email o nombre empieza por `prefix` (o alguna palabra del
    /// nombre empieza por él). Sin distinguir mayúsculas.
    pub fn search_contacts(&self, prefix: &str, limit: usize) -> Result<Vec<Contact>> {
        let prefix = prefix.trim();
        let escaped = escape_like(prefix);
        let starts = format!("{escaped}%");
        let word_starts = format!("% {escaped}%");
        let mut stmt = self
            .conn
            .prepare(
                "SELECT name, email, source FROM contacts \
                 WHERE email LIKE ?1 ESCAPE '\\' OR name LIKE ?1 ESCAPE '\\' \
                    OR name LIKE ?2 ESCAPE '\\' \
                 ORDER BY name IS NULL, name COLLATE NOCASE, email \
                 LIMIT ?3",
            )
            .map_err(se)?;
        let rows = stmt
            .query_map(params![starts, word_starts, to_i64(limit)], |row| {
                Ok(Contact {
                    name: row.get(0)?,
                    email: row.get(1)?,
                    source: row.get(2)?,
                })
            })
            .map_err(se)?;
        rows.collect::<rusqlite::Result<_>>().map_err(se)
    }

    // ----- Búsqueda ------------------------------------------------------

    /// Busca en asunto, remitente, extracto y cuerpo cacheado (FTS5). Cada
    /// palabra de `query` se trata como prefijo; las palabras se combinan con
    /// AND. Devuelve hasta `limit` cabeceras ordenadas por relevancia.
    pub fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<MessageHeader>> {
        let Some(fts) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {HEADER_COLS} FROM messages \
                 JOIN (SELECT rowid AS mid, rank FROM messages_fts \
                       WHERE messages_fts MATCH ?1 ORDER BY rank LIMIT ?2) f \
                 ON f.mid = messages.id \
                 ORDER BY f.rank"
            ))
            .map_err(se)?;
        let rows = stmt
            .query_map(params![fts, to_i64(limit)], header_from_row)
            .map_err(se)?;
        rows.collect::<rusqlite::Result<_>>().map_err(se)
    }

    // ----- Mantenimiento -------------------------------------------------

    /// Borra cabeceras (y cuerpos) con fecha anterior a hace `days` días.
    /// Devuelve cuántos mensajes se borraron.
    pub fn purge_older_than(&self, days: u32) -> Result<usize> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(se)?
            .as_secs();
        let now = i64::try_from(now).map_err(se)?;
        self.purge_before(now - i64::from(days) * SECONDS_PER_DAY)
    }

    /// Borra mensajes con `date < cutoff` (segundos Unix UTC).
    fn purge_before(&self, cutoff: i64) -> Result<usize> {
        let tx = self.conn.unchecked_transaction().map_err(se)?;
        fts_remove_where(
            &tx,
            "rowid IN (SELECT id FROM messages WHERE date < ?1)",
            [cutoff],
        )?;
        let removed = tx
            .execute("DELETE FROM messages WHERE date < ?1", [cutoff])
            .map_err(se)?;
        tx.commit().map_err(se)?;
        Ok(removed)
    }
}

// ----- Migraciones -------------------------------------------------------

fn migrate(conn: &mut Connection) -> Result<()> {
    let current: usize = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(se)?;
    if current > MIGRATIONS.len() {
        return Err(Error::Store(format!(
            "esquema v{current} más nuevo que el soportado (v{})",
            MIGRATIONS.len()
        )));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction().map_err(se)?;
        tx.execute_batch(sql).map_err(se)?;
        tx.pragma_update(None, "user_version", i + 1).map_err(se)?;
        tx.commit().map_err(se)?;
    }
    Ok(())
}

// ----- FTS5 --------------------------------------------------------------

/// Reconstruye la fila FTS de `id` a partir de `messages` y `bodies`.
fn fts_refresh(conn: &Connection, id: MessageId) -> Result<()> {
    let row = conn
        .query_row(
            "SELECT m.subject, m.from_json, m.snippet, b.search_text \
             FROM messages m LEFT JOIN bodies b ON b.message_id = m.id \
             WHERE m.id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(se)?;
    conn.execute("DELETE FROM messages_fts WHERE rowid = ?1", [id])
        .map_err(se)?;
    if let Some((subject, from_json, snippet, body)) = row {
        let from: Vec<Address> = serde_json::from_str(&from_json).map_err(se)?;
        let sender = from
            .iter()
            .flat_map(|a| [a.name.as_deref().unwrap_or(""), a.email.as_str()])
            .collect::<Vec<_>>()
            .join(" ");
        conn.execute(
            "INSERT INTO messages_fts (rowid, subject, sender, snippet, body) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                subject,
                sender,
                snippet.unwrap_or_default(),
                body.unwrap_or_default()
            ],
        )
        .map_err(se)?;
    }
    Ok(())
}

/// Elimina de la FTS las filas de mensajes que cumplen `cond` (sobre `messages`).
/// Debe ejecutarse antes de borrar los mensajes.
fn fts_remove_where(conn: &Connection, cond: &str, p: impl rusqlite::Params) -> Result<()> {
    conn.execute(&format!("DELETE FROM messages_fts WHERE {cond}"), p)
        .map_err(se)?;
    Ok(())
}

/// Convierte la entrada del usuario en una consulta FTS5 segura: cada palabra
/// va entre comillas (así `-`, `:` o `"` no rompen la sintaxis) como prefijo.
fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

/// Quita etiquetas HTML de forma simple (suficiente para indexar texto).
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Escapa `%`, `_` y `\` para usarlos en un LIKE con `ESCAPE '\'`.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ----- Conversiones de fila y valores -----------------------------------

fn not_found(what: &str, id: i64) -> Error {
    Error::NotFound(format!("{what} {id}"))
}

fn to_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(se)
}

fn bad_value(idx: usize, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        idx,
        Type::Text,
        format!("valor no válido: {value}").into(),
    )
}

fn json_column<T: DeserializeOwned>(row: &Row, idx: usize) -> rusqlite::Result<T> {
    let raw: String = row.get(idx)?;
    serde_json::from_str(&raw)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e)))
}

fn provider_to_str(p: Provider) -> &'static str {
    match p {
        Provider::Gmail => "gmail",
        Provider::Outlook => "outlook",
        Provider::Microsoft365 => "microsoft365",
        Provider::Imap => "imap",
    }
}

fn provider_from_str(s: &str) -> Option<Provider> {
    match s {
        "gmail" => Some(Provider::Gmail),
        "outlook" => Some(Provider::Outlook),
        "microsoft365" => Some(Provider::Microsoft365),
        "imap" => Some(Provider::Imap),
        _ => None,
    }
}

fn role_to_str(r: MailboxRole) -> &'static str {
    match r {
        MailboxRole::Inbox => "inbox",
        MailboxRole::Sent => "sent",
        MailboxRole::Drafts => "drafts",
        MailboxRole::Trash => "trash",
        MailboxRole::Spam => "spam",
        MailboxRole::Archive => "archive",
        MailboxRole::Other => "other",
    }
}

fn role_from_str(s: &str) -> Option<MailboxRole> {
    match s {
        "inbox" => Some(MailboxRole::Inbox),
        "sent" => Some(MailboxRole::Sent),
        "drafts" => Some(MailboxRole::Drafts),
        "trash" => Some(MailboxRole::Trash),
        "spam" => Some(MailboxRole::Spam),
        "archive" => Some(MailboxRole::Archive),
        "other" => Some(MailboxRole::Other),
        _ => None,
    }
}

fn account_from_row(row: &Row) -> rusqlite::Result<Account> {
    let provider: String = row.get(1)?;
    Ok(Account {
        id: row.get(0)?,
        provider: provider_from_str(&provider).ok_or_else(|| bad_value(1, &provider))?,
        email: row.get(2)?,
        display_name: row.get(3)?,
        sync_days: row.get(4)?,
    })
}

fn mailbox_from_row(row: &Row) -> rusqlite::Result<Mailbox> {
    let role: String = row.get(3)?;
    Ok(Mailbox {
        id: row.get(0)?,
        account_id: row.get(1)?,
        path: row.get(2)?,
        role: role_from_str(&role).ok_or_else(|| bad_value(3, &role))?,
        uid_validity: row.get(4)?,
        unread: row.get(5)?,
    })
}

fn header_from_row(row: &Row) -> rusqlite::Result<MessageHeader> {
    Ok(MessageHeader {
        id: row.get(0)?,
        mailbox_id: row.get(1)?,
        uid: row.get(2)?,
        message_id: row.get(3)?,
        in_reply_to: row.get(4)?,
        references: json_column(row, 5)?,
        subject: row.get(6)?,
        from: json_column(row, 7)?,
        to: json_column(row, 8)?,
        cc: json_column(row, 9)?,
        date: row.get(10)?,
        flags: Flags {
            seen: row.get(11)?,
            flagged: row.get(12)?,
            answered: row.get(13)?,
            draft: row.get(14)?,
        },
        has_attachments: row.get(15)?,
        snippet: row.get(16)?,
        labels: json_column(row, 17)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use correo_core::Attachment;

    fn store() -> Store {
        Store::open_in_memory().expect("bd en memoria")
    }

    fn new_account(store: &Store, email: &str) -> AccountId {
        store
            .create_account(&Account {
                id: 0,
                provider: Provider::Gmail,
                email: email.into(),
                display_name: "Test".into(),
                sync_days: 30,
            })
            .unwrap()
    }

    fn new_mailbox(store: &Store, account_id: AccountId, path: &str) -> MailboxId {
        store
            .upsert_mailbox(&Mailbox {
                id: 0,
                account_id,
                path: path.into(),
                role: MailboxRole::Inbox,
                uid_validity: Some(1),
                unread: 0,
            })
            .unwrap()
    }

    fn header(mailbox_id: MailboxId, uid: u32, subject: &str, date: i64) -> MessageHeader {
        MessageHeader {
            id: 0,
            mailbox_id,
            uid,
            message_id: Some(format!("<{uid}@test>")),
            in_reply_to: None,
            references: vec![],
            subject: subject.into(),
            from: vec![Address {
                name: Some("Ana Pérez".into()),
                email: "ana@example.com".into(),
            }],
            to: vec![Address {
                name: None,
                email: "yo@example.com".into(),
            }],
            cc: vec![],
            date,
            flags: Flags::default(),
            has_attachments: false,
            snippet: Some(format!("extracto {subject}")),
            labels: vec![],
        }
    }

    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[test]
    fn schema_is_migrated_and_foreign_keys_on() {
        let s = store();
        let version: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        let fk: i64 = s
            .conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn file_database_uses_wal_and_persists() {
        let path = std::env::temp_dir().join(format!(
            "correo-store-test-{}-{}.db",
            std::process::id(),
            now_nanos()
        ));
        {
            let s = Store::open(&path).unwrap();
            let mode: String = s
                .conn
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
            new_account(&s, "persist@example.com");
        }
        {
            // Reabrir no vuelve a aplicar migraciones y conserva los datos.
            let s = Store::open(&path).unwrap();
            let accounts = s.list_accounts().unwrap();
            assert_eq!(accounts.len(), 1);
            assert_eq!(accounts[0].email, "persist@example.com");
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    fn now_nanos() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn account_crud() {
        let s = store();
        let id = new_account(&s, "a@example.com");
        let got = s.get_account(id).unwrap();
        assert_eq!(got.email, "a@example.com");
        assert_eq!(got.provider, Provider::Gmail);
        assert_eq!(got.sync_days, 30);

        let mut updated = got.clone();
        updated.provider = Provider::Microsoft365;
        updated.display_name = "Nuevo".into();
        updated.sync_days = 7;
        s.update_account(&updated).unwrap();
        let got = s.get_account(id).unwrap();
        assert_eq!(got.provider, Provider::Microsoft365);
        assert_eq!(got.display_name, "Nuevo");
        assert_eq!(got.sync_days, 7);

        new_account(&s, "b@example.com");
        assert_eq!(s.list_accounts().unwrap().len(), 2);

        s.delete_account(id).unwrap();
        assert!(matches!(s.get_account(id), Err(Error::NotFound(_))));
        assert_eq!(s.list_accounts().unwrap().len(), 1);
    }

    #[test]
    fn account_errors() {
        let s = store();
        let id = new_account(&s, "dup@example.com");
        // Email duplicado (sin distinguir mayúsculas).
        let dup = s.create_account(&Account {
            id: 0,
            provider: Provider::Imap,
            email: "DUP@example.com".into(),
            display_name: String::new(),
            sync_days: 1,
        });
        assert!(matches!(dup, Err(Error::Store(_))));
        // Actualizar o borrar una cuenta inexistente.
        let ghost = Account {
            id: 9999,
            provider: Provider::Imap,
            email: "x@example.com".into(),
            display_name: String::new(),
            sync_days: 1,
        };
        assert!(matches!(s.update_account(&ghost), Err(Error::NotFound(_))));
        assert!(matches!(s.delete_account(9999), Err(Error::NotFound(_))));
        assert!(s.get_account(id).is_ok());
    }

    #[test]
    fn mailbox_upsert_is_keyed_by_account_and_path() {
        let s = store();
        let acc = new_account(&s, "m@example.com");
        let first = new_mailbox(&s, acc, "INBOX");
        let again = s
            .upsert_mailbox(&Mailbox {
                id: 0,
                account_id: acc,
                path: "INBOX".into(),
                role: MailboxRole::Inbox,
                uid_validity: Some(42),
                unread: 5,
            })
            .unwrap();
        assert_eq!(first, again);
        let got = s.get_mailbox(first).unwrap();
        assert_eq!(got.uid_validity, Some(42));
        assert_eq!(got.unread, 5);

        let sent = new_mailbox(&s, acc, "Sent");
        let boxes = s.list_mailboxes(acc).unwrap();
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[1].id, sent);
        assert_eq!(boxes[1].role, MailboxRole::Inbox);

        s.delete_mailbox(sent).unwrap();
        assert_eq!(s.list_mailboxes(acc).unwrap().len(), 1);
        assert!(matches!(s.delete_mailbox(sent), Err(Error::NotFound(_))));
        assert!(matches!(s.get_mailbox(sent), Err(Error::NotFound(_))));
    }

    #[test]
    fn upsert_headers_inserts_then_updates_same_uid() {
        let s = store();
        let acc = new_account(&s, "h@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        s.upsert_headers(
            mb,
            &[header(mb, 1, "Primero", 100), header(mb, 2, "Otro", 200)],
        )
        .unwrap();
        assert_eq!(s.list_headers(mb, 10, 0).unwrap().len(), 2);

        let mut changed = header(mb, 1, "Primero editado", 100);
        changed.flags.seen = true;
        changed.labels = vec!["Trabajo".into()];
        changed.references = vec!["<root@test>".into()];
        changed.in_reply_to = Some("<root@test>".into());
        s.upsert_headers(mb, &[changed]).unwrap();

        let rows = s.list_headers(mb, 10, 0).unwrap();
        assert_eq!(rows.len(), 2, "no debe duplicar el uid existente");
        let row = rows.iter().find(|h| h.uid == 1).unwrap();
        assert_eq!(row.subject, "Primero editado");
        assert!(row.flags.seen);
        assert_eq!(row.labels, vec!["Trabajo".to_string()]);
        assert_eq!(row.references, vec!["<root@test>".to_string()]);
        assert_eq!(row.in_reply_to.as_deref(), Some("<root@test>"));
        assert_eq!(row.from[0].name.as_deref(), Some("Ana Pérez"));
        assert_eq!(row.to[0].email, "yo@example.com");
        assert_eq!(row.snippet.as_deref(), Some("extracto Primero editado"));
    }

    #[test]
    fn list_headers_orders_by_date_desc_and_paginates() {
        let s = store();
        let acc = new_account(&s, "l@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        let headers: Vec<_> = (1..=5)
            .map(|i| header(mb, i, &format!("m{i}"), i as i64 * 10))
            .collect();
        s.upsert_headers(mb, &headers).unwrap();

        let page1 = s.list_headers(mb, 2, 0).unwrap();
        let subjects: Vec<_> = page1.iter().map(|h| h.subject.as_str()).collect();
        assert_eq!(subjects, ["m5", "m4"]);
        let page2 = s.list_headers(mb, 2, 2).unwrap();
        let subjects: Vec<_> = page2.iter().map(|h| h.subject.as_str()).collect();
        assert_eq!(subjects, ["m3", "m2"]);
        assert!(s.list_headers(mb, 10, 50).unwrap().is_empty());
    }

    #[test]
    fn get_header_by_id() {
        let s = store();
        let acc = new_account(&s, "g@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        s.upsert_headers(mb, &[header(mb, 3, "Uno", 1)]).unwrap();
        let id = s.message_id(mb, 3).unwrap().unwrap();
        assert_eq!(s.get_header(id).unwrap().unwrap().subject, "Uno");
        assert!(s.get_header(id + 99).unwrap().is_none());
    }

    #[test]
    fn set_flags_updates_and_reports_missing() {
        let s = store();
        let acc = new_account(&s, "f@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        s.upsert_headers(mb, &[header(mb, 7, "Flags", 1)]).unwrap();
        let flags = Flags {
            seen: true,
            flagged: true,
            answered: false,
            draft: true,
        };
        s.set_flags(mb, 7, &flags).unwrap();
        let row = &s.list_headers(mb, 1, 0).unwrap()[0];
        assert!(row.flags.seen && row.flags.flagged && row.flags.draft);
        assert!(!row.flags.answered);

        assert!(matches!(
            s.set_flags(mb, 999, &flags),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn delete_uid_removes_message_and_index_entry() {
        let s = store();
        let acc = new_account(&s, "d@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        s.upsert_headers(
            mb,
            &[
                header(mb, 1, "borrable zanahoria", 1),
                header(mb, 2, "otro", 2),
            ],
        )
        .unwrap();
        s.delete_uid(mb, 1).unwrap();
        let rows = s.list_headers(mb, 10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].uid, 2);
        assert!(s.search_messages("zanahoria", 10).unwrap().is_empty());
        // Idempotente: borrar un uid inexistente no falla.
        s.delete_uid(mb, 1).unwrap();
    }

    #[test]
    fn wipe_mailbox_clears_only_that_mailbox() {
        let s = store();
        let acc = new_account(&s, "w@example.com");
        let inbox = new_mailbox(&s, acc, "INBOX");
        let sent = new_mailbox(&s, acc, "Sent");
        s.upsert_headers(
            inbox,
            &[header(inbox, 1, "uno", 1), header(inbox, 2, "dos", 2)],
        )
        .unwrap();
        s.upsert_headers(sent, &[header(sent, 1, "enviado", 3)])
            .unwrap();

        s.wipe_mailbox(inbox).unwrap();
        assert!(s.list_headers(inbox, 10, 0).unwrap().is_empty());
        assert_eq!(s.list_headers(sent, 10, 0).unwrap().len(), 1);
        assert!(s.search_messages("uno", 10).unwrap().is_empty());
        assert_eq!(s.search_messages("enviado", 10).unwrap().len(), 1);
    }

    #[test]
    fn body_roundtrip_and_replacement() {
        let s = store();
        let acc = new_account(&s, "b@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        s.upsert_headers(mb, &[header(mb, 1, "con cuerpo", 1)])
            .unwrap();
        let id = s.list_headers(mb, 1, 0).unwrap()[0].id;

        assert!(s.get_body(id).unwrap().is_none());

        let body = MessageBody {
            text: Some("Hola, adjunto el informe trimestral".into()),
            html: Some("<p>Hola</p>".into()),
            attachments: vec![Attachment {
                filename: "informe.pdf".into(),
                mime_type: "application/pdf".into(),
                size: 1234,
                content_id: None,
            }],
        };
        s.save_body(id, &body).unwrap();
        let got = s.get_body(id).unwrap().unwrap();
        assert_eq!(got.text, body.text);
        assert_eq!(got.html, body.html);
        assert_eq!(got.attachments.len(), 1);
        assert_eq!(got.attachments[0].filename, "informe.pdf");
        assert_eq!(s.search_messages("trimestral", 10).unwrap().len(), 1);

        // Reemplazo: el texto antiguo deja de ser buscable.
        let replaced = MessageBody {
            text: None,
            html: Some("<div>Contenido <b>nuevo</b></div>".into()),
            attachments: vec![],
        };
        s.save_body(id, &replaced).unwrap();
        assert!(s.search_messages("trimestral", 10).unwrap().is_empty());
        assert_eq!(s.search_messages("nuevo", 10).unwrap().len(), 1);
        assert!(s.get_body(id).unwrap().unwrap().text.is_none());
    }

    #[test]
    fn save_body_requires_cached_message() {
        let s = store();
        let body = MessageBody::default();
        assert!(matches!(s.save_body(12345, &body), Err(Error::Store(_))));
    }

    #[test]
    fn upsert_headers_keeps_existing_body() {
        let s = store();
        let acc = new_account(&s, "k@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        s.upsert_headers(mb, &[header(mb, 1, "asunto", 1)]).unwrap();
        let id = s.list_headers(mb, 1, 0).unwrap()[0].id;
        s.save_body(
            id,
            &MessageBody {
                text: Some("cuerpo persistente".into()),
                ..Default::default()
            },
        )
        .unwrap();
        s.upsert_headers(mb, &[header(mb, 1, "asunto nuevo", 1)])
            .unwrap();
        assert!(s.get_body(id).unwrap().is_some());
        assert_eq!(s.search_messages("persistente", 10).unwrap().len(), 1);
        assert_eq!(s.search_messages("nuevo", 10).unwrap().len(), 1);
    }

    #[test]
    fn contacts_upsert_and_keep_name_when_missing() {
        let s = store();
        s.upsert_contacts(&[
            Contact {
                name: Some("Ana Pérez".into()),
                email: "ana@example.com".into(),
                source: "google".into(),
            },
            Contact {
                name: None,
                email: "  ".into(),
                source: "google".into(),
            },
        ])
        .unwrap();
        s.upsert_contacts(&[Contact {
            name: None,
            email: "ANA@example.com".into(),
            source: "graph".into(),
        }])
        .unwrap();
        let found = s.search_contacts("ana", 10).unwrap();
        assert_eq!(found.len(), 1, "email único sin distinguir mayúsculas");
        assert_eq!(found[0].name.as_deref(), Some("Ana Pérez"));
        assert_eq!(found[0].source, "graph");
    }

    #[test]
    fn contacts_search_by_email_name_word_and_limit() {
        let s = store();
        let c = |name: Option<&str>, email: &str| Contact {
            name: name.map(String::from),
            email: email.into(),
            source: "test".into(),
        };
        s.upsert_contacts(&[
            c(Some("Juan Pérez"), "juan@example.com"),
            c(Some("María Juana"), "maria@example.com"),
            c(None, "juanita@example.com"),
            c(Some("100% Real"), "real@example.com"),
            c(Some("Under_score"), "under@example.com"),
        ])
        .unwrap();

        // Prefijo de email.
        let emails: Vec<_> = s
            .search_contacts("juan", 10)
            .unwrap()
            .into_iter()
            .map(|c| c.email)
            .collect();
        assert!(emails.contains(&"juan@example.com".to_string()));
        assert!(emails.contains(&"juanita@example.com".to_string()));
        // Prefijo de nombre y de palabra dentro del nombre.
        let names: Vec<_> = s
            .search_contacts("pér", 10)
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, vec![Some("Juan Pérez".to_string())]);
        assert_eq!(s.search_contacts("perez", 10).unwrap().len(), 0);
        let word: Vec<_> = s
            .search_contacts("juana", 10)
            .unwrap()
            .into_iter()
            .map(|c| c.email)
            .collect();
        assert!(word.contains(&"maria@example.com".to_string()));
        // Los comodines se tratan literalmente.
        assert_eq!(s.search_contacts("100%", 10).unwrap().len(), 1);
        assert!(s.search_contacts("%", 10).unwrap().is_empty());
        assert_eq!(s.search_contacts("under_", 10).unwrap().len(), 1);
        assert_eq!(s.search_contacts("under", 10).unwrap().len(), 1);
        assert_eq!(s.search_contacts("_", 10).unwrap().len(), 0);
        // Límite.
        assert_eq!(s.search_contacts("", 2).unwrap().len(), 2);
    }

    #[test]
    fn search_messages_matches_subject_sender_snippet() {
        let s = store();
        let acc = new_account(&s, "s@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        s.upsert_headers(
            mb,
            &[
                header(mb, 1, "Presupuesto anual", 1),
                header(mb, 2, "Reunión mañana", 2),
            ],
        )
        .unwrap();

        // Asunto, prefijo de palabra y sin tildes.
        let hits = s.search_messages("presupuesto", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].uid, 1);
        assert_eq!(s.search_messages("pres ANU", 10).unwrap().len(), 1);
        assert_eq!(s.search_messages("reunion", 10).unwrap().len(), 1);
        // Remitente (nombre y email).
        assert_eq!(s.search_messages("Ana", 10).unwrap().len(), 2);
        assert_eq!(s.search_messages("ana@example", 10).unwrap().len(), 2);
        // Extracto.
        assert_eq!(s.search_messages("extracto", 10).unwrap().len(), 2);
        // Sin coincidencias.
        assert!(s.search_messages("inexistente", 10).unwrap().is_empty());
        // Entrada con caracteres de sintaxis FTS no rompe la consulta.
        assert!(s.search_messages("\"-:( ) AND", 10).is_ok());
        assert!(s.search_messages("   ", 10).unwrap().is_empty());
        // Límite.
        assert_eq!(s.search_messages("extracto", 1).unwrap().len(), 1);
    }

    #[test]
    fn purge_removes_only_old_messages() {
        let s = store();
        let acc = new_account(&s, "p@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        let old = now() - 100 * SECONDS_PER_DAY;
        s.upsert_headers(
            mb,
            &[
                header(mb, 1, "antiguo caduco", old),
                header(mb, 2, "reciente vigente", now()),
            ],
        )
        .unwrap();
        let removed = s.purge_older_than(30).unwrap();
        assert_eq!(removed, 1);
        let rows = s.list_headers(mb, 10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].uid, 2);
        assert!(s.search_messages("caduco", 10).unwrap().is_empty());
        assert_eq!(s.search_messages("vigente", 10).unwrap().len(), 1);
    }

    #[test]
    fn purge_before_removes_bodies_and_index_entries() {
        let s = store();
        let acc = new_account(&s, "q@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        s.upsert_headers(mb, &[header(mb, 1, "viejo", 10)]).unwrap();
        let id = s.list_headers(mb, 1, 0).unwrap()[0].id;
        s.save_body(
            id,
            &MessageBody {
                text: Some("cuerpo viejo".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.purge_before(100).unwrap(), 1);
        assert!(s.get_body(id).unwrap().is_none());
        assert!(s.search_messages("viejo", 10).unwrap().is_empty());
    }

    #[test]
    fn deleting_account_cleans_index() {
        let s = store();
        let acc = new_account(&s, "del@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        s.upsert_headers(mb, &[header(mb, 1, "desaparece", 1)])
            .unwrap();
        s.delete_account(acc).unwrap();
        assert!(s.search_messages("desaparece", 10).unwrap().is_empty());
        let left: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn deleting_mailbox_cleans_index() {
        let s = store();
        let acc = new_account(&s, "dm@example.com");
        let mb = new_mailbox(&s, acc, "INBOX");
        s.upsert_headers(mb, &[header(mb, 1, "carpeta borrada", 1)])
            .unwrap();
        s.delete_mailbox(mb).unwrap();
        assert!(s.search_messages("borrada", 10).unwrap().is_empty());
    }
}

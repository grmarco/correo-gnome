//! Tipos y contratos compartidos por todos los crates.

use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("autenticación: {0}")]
    Auth(String),
    #[error("red: {0}")]
    Network(String),
    #[error("protocolo: {0}")]
    Protocol(String),
    #[error("almacenamiento: {0}")]
    Store(String),
    #[error("no encontrado: {0}")]
    NotFound(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    Gmail,
    Outlook,
    Microsoft365,
    Imap,
}

pub type AccountId = i64;
pub type MailboxId = i64;
pub type MessageId = i64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub provider: Provider,
    pub email: String,
    pub display_name: String,
    /// Días de cabeceras a sincronizar.
    pub sync_days: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MailboxRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Spam,
    Archive,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mailbox {
    pub id: MailboxId,
    pub account_id: AccountId,
    /// Ruta IMAP completa.
    pub path: String,
    pub role: MailboxRole,
    pub uid_validity: Option<u32>,
    pub unread: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Flags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
}

/// Cabecera de mensaje: lo único que se sincroniza por defecto.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageHeader {
    pub id: MessageId,
    pub mailbox_id: MailboxId,
    pub uid: u32,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Segundos Unix UTC.
    pub date: i64,
    pub flags: Flags,
    pub has_attachments: bool,
    pub snippet: Option<String>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub mime_type: String,
    pub size: u64,
    pub content_id: Option<String>,
}

/// Cuerpo descargado bajo demanda.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageBody {
    pub text: Option<String>,
    pub html: Option<String>,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Contact {
    pub name: Option<String>,
    pub email: String,
    pub source: String,
}

/// Mensaje saliente.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// (nombre, tipo MIME, contenido)
    pub attachments: Vec<(String, String, Vec<u8>)>,
}

/// Credencial para IMAP/SMTP XOAUTH2.
#[derive(Debug, Clone)]
pub struct AccessToken(pub String);

#[async_trait::async_trait]
pub trait TokenProvider: Send + Sync {
    /// Devuelve un token válido, refrescándolo si hace falta.
    async fn access_token(&self, account: AccountId) -> Result<AccessToken>;
}

#[async_trait::async_trait]
pub trait MailBackend: Send + Sync {
    async fn list_mailboxes(&self) -> Result<Vec<Mailbox>>;
    /// Sincroniza cabeceras de los últimos `days` días; devuelve cuántas son nuevas o cambiadas.
    async fn sync_mailbox(&self, mailbox: &Mailbox, days: u32) -> Result<usize>;
    async fn fetch_body(&self, mailbox: &Mailbox, uid: u32) -> Result<MessageBody>;
    async fn fetch_attachment(&self, mailbox: &Mailbox, uid: u32, index: usize) -> Result<Vec<u8>>;
    async fn set_seen(&self, mailbox: &Mailbox, uid: u32, seen: bool) -> Result<()>;
    async fn move_to(&self, mailbox: &Mailbox, uid: u32, target: &Mailbox) -> Result<()>;
    async fn delete(&self, mailbox: &Mailbox, uid: u32) -> Result<()>;
}

#[async_trait::async_trait]
pub trait ContactSource: Send + Sync {
    async fn sync(&self) -> Result<Vec<Contact>>;
}

#[async_trait::async_trait]
pub trait Sender: Send + Sync {
    async fn send(&self, draft: &Draft) -> Result<()>;
}

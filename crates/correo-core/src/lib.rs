//! Tipos y contratos compartidos por todos los crates.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    #[error("operación cancelada")]
    Cancelled,
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
    /// Delimitador de jerarquía que declara el servidor en LIST (`None` si no hay
    /// jerarquía o aún no se conoce). Nunca se adivina.
    #[serde(default)]
    pub delimiter: Option<char>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub mime_type: String,
    /// Tamaño decodificado (el del contenido real del adjunto).
    pub size: u64,
    pub content_id: Option<String>,
    /// Número de parte MIME según IMAP (p. ej. `"2"`, `"1.2"`). `None` en cuerpos
    /// cacheados antes de existir este campo: se descarga el mensaje entero.
    #[serde(default)]
    pub part: Option<String>,
    /// Content-Transfer-Encoding en minúsculas (`base64`, `quoted-printable`, `7bit`...).
    #[serde(default)]
    pub encoding: Option<String>,
    /// Tamaño en el cable (codificado), base del cálculo del progreso.
    #[serde(default)]
    pub encoded_size: Option<u64>,
}

/// Progreso de una descarga. `received` y `total` van en bytes codificados (los que
/// viajan por la red); `total` es `None` si no se conoce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub received: u64,
    pub total: Option<u64>,
}

impl Progress {
    /// Fracción 0.0..=1.0 si se conoce el total.
    pub fn fraction(&self) -> Option<f64> {
        self.total
            .filter(|t| *t > 0)
            .map(|t| (self.received as f64 / t as f64).clamp(0.0, 1.0))
    }
}

/// Callback de progreso. Se invoca desde la tarea que descarga (no desde el hilo de GTK).
pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// Señal de cancelación compartida y barata de clonar.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Respaldo: descarga el adjunto con `fetch_attachment` (mensaje completo), lo escribe en
/// `dest` y notifica 0 % y 100 %. Es lo que hace la implementación por defecto de
/// [`MailBackend::fetch_attachment_to`].
pub async fn fetch_attachment_to_via_whole<B: MailBackend + ?Sized>(
    backend: &B,
    mailbox: &Mailbox,
    uid: u32,
    index: usize,
    dest: &Path,
    progress: &ProgressFn,
    cancel: &CancelToken,
) -> Result<u64> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    progress(Progress {
        received: 0,
        total: None,
    });
    let data = backend.fetch_attachment(mailbox, uid, index).await?;
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if let Err(e) = std::fs::write(dest, &data) {
        let _ = std::fs::remove_file(dest);
        return Err(Error::Store(format!("escribiendo el adjunto: {e}")));
    }
    let n = data.len() as u64;
    progress(Progress {
        received: n,
        total: Some(n),
    });
    Ok(n)
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

/// Credencial para IMAP/SMTP XOAUTH2. No implementa `Debug` con el valor: un `{:?}` en un
/// log no debe filtrar el token.
#[derive(Clone)]
pub struct AccessToken(pub String);

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

/// Recurso para el que se pide el token. En Microsoft un token solo vale para un recurso:
/// el correo (IMAP/SMTP) y Graph (contactos, perfil) necesitan tokens distintos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenAudience {
    Mail,
    Directory,
}

#[async_trait::async_trait]
pub trait TokenProvider: Send + Sync {
    /// Devuelve un token válido para el correo, refrescándolo si hace falta.
    async fn access_token(&self, account: AccountId) -> Result<AccessToken>;

    /// Token para otro recurso del mismo proveedor. Por defecto es el mismo token.
    async fn access_token_for(
        &self,
        account: AccountId,
        _audience: TokenAudience,
    ) -> Result<AccessToken> {
        self.access_token(account).await
    }
}

#[async_trait::async_trait]
pub trait MailBackend: Send + Sync {
    async fn list_mailboxes(&self) -> Result<Vec<Mailbox>>;
    /// Sincroniza cabeceras de los últimos `days` días; devuelve cuántas son nuevas o cambiadas.
    async fn sync_mailbox(&self, mailbox: &Mailbox, days: u32) -> Result<usize>;
    async fn fetch_body(&self, mailbox: &Mailbox, uid: u32) -> Result<MessageBody>;
    async fn fetch_attachment(&self, mailbox: &Mailbox, uid: u32, index: usize) -> Result<Vec<u8>>;

    /// Descarga el adjunto `index` directamente al fichero `dest` (que crea o trunca el
    /// llamador/implementación) y devuelve los bytes decodificados escritos.
    ///
    /// `progress` se invoca tras cada trozo con bytes codificados recibidos y total
    /// (y una última vez con `received == total`). Al cancelar con `cancel` devuelve
    /// `Error::Cancelled`; si falla o se cancela, `dest` no se deja a medias (se borra).
    /// La implementación por defecto usa `fetch_attachment` y notifica solo 0 % y 100 %.
    async fn fetch_attachment_to(
        &self,
        mailbox: &Mailbox,
        uid: u32,
        index: usize,
        dest: &Path,
        progress: ProgressFn,
        cancel: CancelToken,
    ) -> Result<u64> {
        fetch_attachment_to_via_whole(self, mailbox, uid, index, dest, &progress, &cancel).await
    }

    async fn set_seen(&self, mailbox: &Mailbox, uid: u32, seen: bool) -> Result<()>;
    async fn move_to(&self, mailbox: &Mailbox, uid: u32, target: &Mailbox) -> Result<()>;
    async fn delete(&self, mailbox: &Mailbox, uid: u32) -> Result<()>;

    // ----- Operaciones por lotes ---------------------------------------------------------
    // Todos los UIDs pertenecen a `mailbox`. Un fallo no aborta el resto: el resultado
    // indica qué UIDs se aplicaron y cuáles no. La implementación por defecto itera; los
    // backends pueden sobrescribirla con conjuntos de UID reales.

    async fn set_seen_many(&self, mailbox: &Mailbox, uids: &[u32], seen: bool) -> BatchReport {
        let mut report = BatchReport::default();
        for &uid in uids {
            let r = self.set_seen(mailbox, uid, seen).await;
            report.record(&[uid], r);
        }
        report
    }

    async fn move_many(&self, mailbox: &Mailbox, uids: &[u32], target: &Mailbox) -> BatchReport {
        let mut report = BatchReport::default();
        for &uid in uids {
            let r = self.move_to(mailbox, uid, target).await;
            report.record(&[uid], r);
        }
        report
    }

    async fn delete_many(&self, mailbox: &Mailbox, uids: &[u32]) -> BatchReport {
        let mut report = BatchReport::default();
        for &uid in uids {
            let r = self.delete(mailbox, uid).await;
            report.record(&[uid], r);
        }
        report
    }
}

/// Máximo de UIDs por conjunto en una orden IMAP por lotes.
pub const BATCH_MAX_UIDS: usize = 500;

/// Ordena, deduplica y trocea `uids` en lotes de como máximo `max` elementos
/// (`max == 0` se trata como 1).
pub fn chunk_uids(uids: &[u32], max: usize) -> Vec<Vec<u32>> {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted.chunks(max.max(1)).map(<[u32]>::to_vec).collect()
}

/// Resultado de una operación por lotes sobre una carpeta.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchReport {
    /// UIDs aplicados.
    pub done: Vec<u32>,
    /// UIDs que fallaron.
    pub failed: Vec<u32>,
    /// Primer error observado (texto para el usuario).
    pub error: Option<String>,
}

impl BatchReport {
    /// Anota el resultado de una operación sobre `uids`.
    pub fn record(&mut self, uids: &[u32], result: Result<()>) {
        match result {
            Ok(()) => self.done.extend_from_slice(uids),
            Err(e) => {
                self.failed.extend_from_slice(uids);
                self.error.get_or_insert_with(|| e.to_string());
            }
        }
    }

    /// Informe en el que todos los UIDs fallaron por el mismo motivo.
    pub fn all_failed(uids: &[u32], error: &Error) -> Self {
        Self {
            done: Vec::new(),
            failed: uids.to_vec(),
            error: Some(error.to_string()),
        }
    }

    /// Une otro informe al actual.
    pub fn merge(&mut self, other: BatchReport) {
        self.done.extend(other.done);
        self.failed.extend(other.failed);
        if self.error.is_none() {
            self.error = other.error;
        }
    }
}

#[async_trait::async_trait]
pub trait ContactSource: Send + Sync {
    async fn sync(&self) -> Result<Vec<Contact>>;
}

#[async_trait::async_trait]
pub trait Sender: Send + Sync {
    async fn send(&self, draft: &Draft) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::{AccessToken, BATCH_MAX_UIDS, BatchReport, Error, chunk_uids};

    #[test]
    fn chunk_uids_sorts_dedups_and_splits() {
        assert!(chunk_uids(&[], 500).is_empty());
        assert_eq!(chunk_uids(&[5, 1, 5, 3], 2), vec![vec![1, 3], vec![5]]);
        let many: Vec<u32> = (1..=1201).collect();
        let chunks = chunk_uids(&many, BATCH_MAX_UIDS);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.len() <= BATCH_MAX_UIDS));
        assert_eq!(chunks.concat(), many);
        // Un tope de 0 no debe provocar un pánico.
        assert_eq!(chunk_uids(&[1, 2], 0), vec![vec![1], vec![2]]);
    }

    #[test]
    fn batch_report_records_and_merges() {
        let mut a = BatchReport::default();
        a.record(&[1, 2], Ok(()));
        a.record(&[3], Err(Error::Network("corte".into())));
        a.record(&[4], Err(Error::Network("otro".into())));
        assert_eq!(a.done, vec![1, 2]);
        assert_eq!(a.failed, vec![3, 4]);
        assert_eq!(a.error.as_deref(), Some("red: corte"));
        let mut b = BatchReport::default();
        b.merge(a);
        b.merge(BatchReport::all_failed(&[9], &Error::Auth("x".into())));
        assert_eq!(b.failed, vec![3, 4, 9]);
        assert_eq!(b.error.as_deref(), Some("red: corte"));
    }

    struct Whole(Vec<u8>);

    #[async_trait::async_trait]
    impl crate::MailBackend for Whole {
        async fn list_mailboxes(&self) -> crate::Result<Vec<crate::Mailbox>> {
            Ok(vec![])
        }
        async fn sync_mailbox(&self, _: &crate::Mailbox, _: u32) -> crate::Result<usize> {
            Ok(0)
        }
        async fn fetch_body(
            &self,
            _: &crate::Mailbox,
            _: u32,
        ) -> crate::Result<crate::MessageBody> {
            Ok(Default::default())
        }
        async fn fetch_attachment(
            &self,
            _: &crate::Mailbox,
            _: u32,
            _: usize,
        ) -> crate::Result<Vec<u8>> {
            Ok(self.0.clone())
        }
        async fn set_seen(&self, _: &crate::Mailbox, _: u32, _: bool) -> crate::Result<()> {
            Ok(())
        }
        async fn move_to(
            &self,
            _: &crate::Mailbox,
            _: u32,
            _: &crate::Mailbox,
        ) -> crate::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &crate::Mailbox, _: u32) -> crate::Result<()> {
            Ok(())
        }
    }

    fn mailbox() -> crate::Mailbox {
        crate::Mailbox {
            id: 1,
            account_id: 1,
            path: "INBOX".into(),
            role: crate::MailboxRole::Inbox,
            uid_validity: None,
            unread: 0,
            delimiter: None,
        }
    }

    #[tokio::test]
    async fn default_fetch_attachment_to_writes_file_and_reports_0_and_100() {
        use std::sync::{Arc, Mutex};
        let dir = std::env::temp_dir().join(format!("correo-core-att-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.bin");
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        let progress: crate::ProgressFn = Arc::new(move |p| l2.lock().unwrap().push(p));
        let b = Whole(vec![1, 2, 3]);
        use crate::MailBackend;
        let n = b
            .fetch_attachment_to(
                &mailbox(),
                1,
                0,
                &dest,
                progress.clone(),
                crate::CancelToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(n, 3);
        assert_eq!(std::fs::read(&dest).unwrap(), vec![1, 2, 3]);
        let log = log.lock().unwrap().clone();
        assert_eq!(log.first().unwrap().received, 0);
        assert_eq!(log.last().unwrap().total, Some(3));

        let cancel = crate::CancelToken::new();
        cancel.cancel();
        let other = dir.join("b.bin");
        let r = b
            .fetch_attachment_to(&mailbox(), 1, 0, &other, progress, cancel)
            .await;
        assert!(matches!(r, Err(crate::Error::Cancelled)));
        assert!(!other.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_token_is_shared_between_clones() {
        let a = crate::CancelToken::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled());
    }

    #[test]
    fn progress_fraction() {
        let p = crate::Progress {
            received: 5,
            total: Some(10),
        };
        assert_eq!(p.fraction(), Some(0.5));
        let q = crate::Progress {
            received: 5,
            total: None,
        };
        assert_eq!(q.fraction(), None);
    }

    #[test]
    fn access_token_debug_redacts_secret() {
        let token = AccessToken("ya29.supersecreto".into());
        let shown = format!("{token:?}");
        assert!(!shown.contains("supersecreto"));
        assert!(shown.contains("<redacted>"));
        // El valor sigue accesible para XOAUTH2.
        assert_eq!(token.0, "ya29.supersecreto");
    }
}

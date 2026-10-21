//! Backend falso del modo demo (solo depuración): no toca la red y simula descargas de
//! adjuntos lentas, con progreso, éxito, error y cancelación, para probar la interfaz.
//!
//! El comportamiento depende del nombre del adjunto:
//! - `Fotos-fin-de-semana.zip`: avanza hasta el 40 % y se queda esperando (se puede cancelar);
//! - `Balance-2026.xlsx`: falla al llegar al 60 %;
//! - cualquier otro: baja a ~4 MB/s (codificados) y termina bien.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use correo_core::{
    Attachment, CancelToken, Error, MailBackend, Mailbox, MessageBody, Progress, ProgressFn, Result,
};

/// Adjuntos de demostración (nombre, tipo, tamaño decodificado) de los mensajes que los llevan.
pub fn demo_attachments() -> Vec<Attachment> {
    let make = |name: &str, mime: &str, size: u64| Attachment {
        filename: name.into(),
        mime_type: mime.into(),
        size,
        content_id: None,
        part: Some("2".into()),
        encoding: Some("base64".into()),
        // base64: ~4/3 del tamaño real en el cable.
        encoded_size: Some(size / 3 * 4),
    };
    vec![
        make("Presupuesto-T4.pdf", "application/pdf", 12_000_000),
        make("Fotos-fin-de-semana.zip", "application/zip", 48_000_000),
        make("Balance-2026.xlsx", "application/vnd.ms-excel", 3_400_000),
        make("instalar.sh", "application/x-sh", 2_100),
    ]
}

/// Bytes por segundo (codificados) de la descarga simulada.
const SPEED: u64 = 4_000_000;
const STEP: Duration = Duration::from_millis(50);

pub struct DemoBackend;

impl DemoBackend {
    pub fn shared() -> Arc<dyn MailBackend> {
        Arc::new(DemoBackend)
    }
}

#[async_trait]
impl MailBackend for DemoBackend {
    async fn list_mailboxes(&self) -> Result<Vec<Mailbox>> {
        Ok(Vec::new())
    }
    async fn sync_mailbox(&self, _: &Mailbox, _: u32) -> Result<usize> {
        Ok(0)
    }
    async fn fetch_body(&self, _: &Mailbox, _: u32) -> Result<MessageBody> {
        Err(Error::Network("modo demostración: sin red".into()))
    }
    async fn fetch_attachment(&self, _: &Mailbox, _: u32, _: usize) -> Result<Vec<u8>> {
        Ok(b"demo".to_vec())
    }

    async fn fetch_attachment_to(
        &self,
        _mailbox: &Mailbox,
        _uid: u32,
        index: usize,
        dest: &Path,
        progress: ProgressFn,
        cancel: CancelToken,
    ) -> Result<u64> {
        let att = demo_attachments()
            .into_iter()
            .nth(index)
            .ok_or_else(|| Error::NotFound("adjunto".into()))?;
        let total = att.encoded_size.unwrap_or(att.size).max(1);
        let fail = |e| {
            let _ = std::fs::remove_file(dest);
            Err(e)
        };
        std::fs::write(dest, b"").map_err(|e| Error::Store(e.to_string()))?;
        let (stall_at, fail_at) = match att.filename.as_str() {
            "Fotos-fin-de-semana.zip" => (Some(total * 4 / 10), None),
            "Balance-2026.xlsx" => (None, Some(total * 6 / 10)),
            _ => (None, None),
        };
        let per_step = (SPEED as f64 * STEP.as_secs_f64()) as u64;
        let mut received = 0;
        progress(Progress {
            received,
            total: Some(total),
        });
        while received < total {
            tokio::time::sleep(STEP).await;
            if cancel.is_cancelled() {
                return fail(Error::Cancelled);
            }
            if stall_at.is_some_and(|s| received >= s) {
                // Esperando datos del servidor: solo la cancelación saca de aquí.
                continue;
            }
            received = (received + per_step).min(total);
            if let Some(limit) = stall_at {
                received = received.min(limit);
            }
            if fail_at.is_some_and(|f| received >= f) {
                return fail(Error::Network("conexión cerrada por el servidor".into()));
            }
            progress(Progress {
                received,
                total: Some(total),
            });
        }
        std::fs::write(dest, b"contenido de demostracion\n")
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(att.size)
    }

    async fn set_seen(&self, _: &Mailbox, _: u32, _: bool) -> Result<()> {
        Ok(())
    }
    async fn move_to(&self, _: &Mailbox, _: u32, _: &Mailbox) -> Result<()> {
        Ok(())
    }
    async fn delete(&self, _: &Mailbox, _: u32) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_attachments_cover_the_scenarios() {
        let names: Vec<_> = demo_attachments().into_iter().map(|a| a.filename).collect();
        assert!(names.contains(&"Fotos-fin-de-semana.zip".to_string()));
        assert!(names.contains(&"Balance-2026.xlsx".to_string()));
    }

    #[tokio::test]
    async fn a_stalled_download_ends_when_cancelled() {
        let dir = std::env::temp_dir().join(format!("correo-demo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("zip");
        let cancel = CancelToken::new();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let progress: ProgressFn = Arc::new(move |p| s2.lock().unwrap().push(p.received));
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            c2.cancel();
        });
        let mailbox = Mailbox {
            id: 1,
            account_id: 1,
            path: "INBOX".into(),
            role: correo_core::MailboxRole::Inbox,
            uid_validity: None,
            unread: 0,
            delimiter: None,
        };
        let r = DemoBackend
            .fetch_attachment_to(&mailbox, 1, 1, &dest, progress, cancel)
            .await;
        assert!(matches!(r, Err(Error::Cancelled)));
        assert!(!dest.exists(), "al cancelar no queda fichero");
        let seen = seen.lock().unwrap();
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "progreso monótono");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

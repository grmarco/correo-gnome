//! Backend falso del modo demo (solo depuración): no toca la red y simula descargas de
//! adjuntos lentas, con progreso, éxito, error y cancelación, para probar la interfaz.
//!
//! El comportamiento depende del nombre del adjunto:
//! - `Fotos-fin-de-semana.zip`: avanza hasta el 40 % y se queda esperando (se puede cancelar);
//! - `Balance-2026.xlsx`: falla al llegar al 60 %;
//! - cualquier otro: baja a ~4 MB/s (codificados) y termina bien.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use correo_core::{
    Address, Attachment, CancelToken, Error, Flags, MailBackend, Mailbox, MessageBody,
    MessageHeader, OlderPage, Progress, ProgressFn, Result,
};
use correo_store::Store;

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

/// Carpetas de archivo de la demo con historial simulado en el «servidor»:
/// (sufijo de la ruta, UID más alto, comportamiento de «Cargar mensajes anteriores»).
pub const DEMO_ARCHIVES: [(&str, u32, &str); 4] = [
    ("2014", 500, "cuatro páginas y se agota"),
    ("2015", 130, "una página y se agota"),
    ("2016", 300, "no termina nunca (cargando)"),
    ("2019", 300, "falla"),
];

/// UID más alto del historial simulado de una carpeta `Archives/<año>`.
pub fn demo_archive_top(path: &str) -> Option<u32> {
    let year = path.rsplit(['/', '.']).next()?;
    DEMO_ARCHIVES
        .iter()
        .find(|(y, _, _)| *y == year)
        .map(|(_, top, _)| *top)
}

/// Cabeceras sintéticas de una carpeta de archivo: un mensaje por día desde el 1 de enero
/// del año de la carpeta, con el UID como número de orden.
pub fn demo_archive_headers(
    mailbox_id: correo_core::MailboxId,
    path: &str,
    uids: std::ops::RangeInclusive<u32>,
) -> Vec<MessageHeader> {
    let year: i64 = path
        .rsplit(['/', '.'])
        .next()
        .and_then(|y| y.parse().ok())
        .unwrap_or(2014);
    let base = (year - 1970) * 365 * 86_400 + ((year - 1969) / 4) * 86_400;
    let people = [
        ("Ana Ruiz", "ana@ejemplo.com"),
        ("Luis Gómez", "luis@ejemplo.org"),
        ("Marta Solé", "marta@ejemplo.com"),
    ];
    uids.map(|uid| {
        let (name, email) = people[uid as usize % people.len()];
        MessageHeader {
            id: 0,
            mailbox_id,
            uid,
            message_id: Some(format!("<archivo-{year}-{uid}@ejemplo.com>")),
            in_reply_to: None,
            references: Vec::new(),
            subject: format!("Archivo {year}: mensaje {uid}"),
            from: vec![Address {
                name: Some(name.into()),
                email: email.into(),
            }],
            to: Vec::new(),
            cc: Vec::new(),
            date: base + i64::from(uid) * 86_400,
            flags: Flags {
                seen: true,
                ..Flags::default()
            },
            has_attachments: uid % 7 == 0,
            snippet: Some("Mensaje antiguo del archivo de demostración.".into()),
            labels: Vec::new(),
        }
    })
    .collect()
}

#[derive(Default)]
pub struct DemoBackend {
    /// Caché de la demo: `fetch_older` le añade las cabeceras «traídas del servidor».
    store: Option<Arc<Mutex<Store>>>,
}

impl DemoBackend {
    pub fn shared(store: Arc<Mutex<Store>>) -> Arc<dyn MailBackend> {
        Arc::new(DemoBackend { store: Some(store) })
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
    /// Historial simulado: `Archives/2014` tiene 500 mensajes en el servidor, 2015 solo 130,
    /// 2016 se queda cargando para siempre y 2019 falla (ver `DEMO_ARCHIVES`).
    async fn fetch_older(&self, mailbox: &Mailbox, count: usize) -> Result<OlderPage> {
        tokio::time::sleep(Duration::from_millis(900)).await;
        let year = mailbox.path.rsplit(['/', '.']).next().unwrap_or_default();
        match year {
            "2016" => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
            "2019" => {
                return Err(Error::Network("tiempo de espera agotado (60 s)".into()));
            }
            _ => {}
        }
        let (Some(store), Some(top)) = (&self.store, demo_archive_top(&mailbox.path)) else {
            return Ok(OlderPage {
                fetched: 0,
                exhausted: true,
            });
        };
        let store = store.lock().map_err(|_| Error::Store("bloqueado".into()))?;
        let min = store.min_uid(mailbox.id)?.unwrap_or(top + 1);
        if min <= 1 {
            return Ok(OlderPage {
                fetched: 0,
                exhausted: true,
            });
        }
        let hi = min - 1;
        let lo = hi.saturating_sub(count as u32 - 1).max(1);
        let headers = demo_archive_headers(mailbox.id, &mailbox.path, lo..=hi);
        store.upsert_headers(mailbox.id, &headers)?;
        Ok(OlderPage {
            fetched: headers.len(),
            exhausted: lo <= 1,
        })
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
        let r = DemoBackend::default()
            .fetch_attachment_to(&mailbox, 1, 1, &dest, progress, cancel)
            .await;
        assert!(matches!(r, Err(Error::Cancelled)));
        assert!(!dest.exists(), "al cancelar no queda fichero");
        let seen = seen.lock().unwrap();
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "progreso monótono");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

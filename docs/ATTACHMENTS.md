# Descarga de adjuntos por partes

## Diseño

- `Attachment` (correo-core) lleva, además de nombre/tipo/tamaño decodificado, `part`
  (número de parte MIME IMAP, p. ej. `"2"`, `"1.2"`), `encoding` (Content-Transfer-Encoding
  en minúsculas) y `encoded_size` (bytes en el cable). Los tres son `Option` con
  `#[serde(default)]`: los cuerpos cacheados antes no los tienen.
- `fetch_body` (correo-imap, `parse::parse_body`) los rellena al parsear el mensaje completo:
  el número de parte se calcula recorriendo el árbol MIME con la numeración IMAP, de modo que
  coincide con el índice `index` de `attachments()`. `save_body` guarda el JSON completo.
- `attach::bodystructure_attachments` extrae lo mismo desde un BODYSTRUCTURE (con `filename*`
  RFC 2231); hoy solo lo usan los tests (útil para validar o para backends futuros).
- `MailBackend::fetch_attachment_to(mailbox, uid, index, dest, progress, cancel)`:
  - Por defecto: `fetch_attachment` + escritura en `dest`, progreso 0 % y 100 %.
  - IMAP: si el adjunto cacheado tiene `part`, pide `UID FETCH uid (BODY.PEEK[part]<off.256KiB>)`
    (`attach::CHUNK_SIZE`), decodifica al vuelo (base64 / quoted-printable con estado entre
    trozos; 7bit/8bit/binary tal cual) y escribe en `dest`. Si no hay `part`, usa el camino
    antiguo (mensaje completo).
  - Cada trozo: bloquea la sesión solo durante su `SELECT` + `FETCH`, con tope de 60 s por
    trozo (no por adjunto); comprueba UIDVALIDITY; un error de red se reintenta UNA vez con
    sesión nueva (solo se escribe un trozo completo, sin duplicados). Así sincronizar o marcar
    como leído se intercalan entre trozos.
  - El final se detecta por un trozo más corto que el pedido; `encoded_size` solo alimenta
    el progreso.

## Contrato para la UI

- Llamar desde una tarea de fondo (tokio), nunca desde el hilo de GTK. `dest` debe estar en
  un directorio existente y escribible; la función crea/trunca el fichero. Para «Abrir»,
  usar una ruta temporal; para «Guardar», la elegida por el usuario.
- `progress: ProgressFn = Arc<dyn Fn(Progress)+Send+Sync>` se llama desde la tarea de
  descarga: reenviar a GTK con un canal/`glib::MainContext::invoke`. `Progress { received,
  total }` va en bytes codificados; `total` puede ser `None`; `Progress::fraction()` da 0..1.
  Siempre hay un primer evento con `received == 0` y un último con `received == total`.
- Cancelar: `CancelToken::cancel()` (clonable). Se atiende entre trozos (a lo sumo tarda lo
  que un trozo); devuelve `Error::Cancelled`, que la UI no debe mostrar como fallo.
- Si falla o se cancela, `dest` se borra. Si tiene éxito devuelve los bytes decodificados
  escritos. Las firmas antiguas (`fetch_attachment`) siguen existiendo.
- Cuerpos cacheados antiguos: sin `part`, se usa el mensaje completo (sin progreso real).
  Volver a bajar el cuerpo (p. ej. tras `wipe`) los actualiza.

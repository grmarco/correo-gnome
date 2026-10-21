# Desarrollo

## Sincronización de cabeceras: ventana de días O mínimo de mensajes

`sync_mailbox` no trae carpetas enteras. Garantiza, por carpeta, **los mensajes de los últimos `days` días
(`UID SEARCH SINCE`) o, si son menos de `N`, los `N` de UID más alto** (`N = min_messages_per_folder` de
`config.toml`, por defecto 100, validado a 20..2000; `ImapBackend::with_min_messages`). Así `Archives/2014`
no sale «sincronizada» y vacía.

* **Rango cubierto**: `uid_min` = el más bajo entre la ventana de días y los `N` UID más altos. Los `N` más
  altos se buscan **sin `SEARCH ALL`**: `UID SEARCH UID lo:hi` por tramos descendentes desde `UIDNEXT-1`
  (primer tramo de 256 UID, que se duplica si los UID están dispersos, tope de 40 búsquedas). Si la ventana
  ya trae `N` o más, no hay búsquedas extra.
* **Borrados**: se compara la caché con `UID SEARCH UID lo:*`, donde `lo = min(uid_min, UID más bajo en
  caché)`. Todo UID cacheado `>= lo` que el servidor ya no devuelve se borra de la caché, esté o no dentro
  de la ventana de días (la decide el UID, no la fecha). Los UID `< lo` del resultado se descartan (el RFC 3501
  hace que `n:*` con `n` > máximo devuelva el último mensaje). Funciones puras: `plan_range_sync`,
  `synced_floor`, `compare_floor` (`correo-imap/src/parse.rs`) y `newest_uids`/`plan_older`
  (`paging.rs`, con una fuente falsa en los tests).
* **Cabeceras nuevas y flags** solo desde `uid_min`; por debajo (páginas antiguas ya cargadas) únicamente se
  detectan borrados, sin refrescar flags.
* **Coste**: 1 `SEARCH SINCE` + 0..n búsquedas de tramo + 1 `SEARCH UID lo:*` (devuelve tantos UID como
  mensajes cacheados o recientes: unos KB incluso con miles) + FETCH en lotes de 200 + 1 `STATUS` para los
  no leídos de esa carpeta. UIDVALIDITY, timeouts por operación, X-GM-LABELS y la ausencia de EXPUNGE global no cambian.
* **No leídos**: al sincronizar una carpeta se pide su `STATUS (UNSEEN)` (una sola). `list_mailboxes` ya
  NO consulta `STATUS`: hace solo LIST + upsert y devuelve los contadores guardados (en Gmail, con 18
  carpetas, el `STATUS` de todas tardaba ~12 s con la sesión bloqueada). El resto de carpetas se
  actualiza con `MailBackend::refresh_unread` (ver abajo).

### Refresco de no leídos en segundo plano

* `MailBackend::refresh_unread() -> Result<Vec<MailboxId>>` (por defecto `Ok(vec![])`) devuelve los ids de
  carpetas cuyo contador cambió. En `correo-imap` (`unread.rs`) recorre las carpetas SELECCIONABLES ya
  guardadas (sin otro LIST), con la bandeja de entrada primero, luego las ya sincronizadas
  (`uid_validity` conocido) y al final el resto, pidiendo `STATUS (UNSEEN)` de una en una.
* El mutex de la sesión se toma SOLO durante cada `STATUS` (`UnseenSource`), de modo que sincronizar,
  marcar leído o abrir un mensaje se intercalan entre carpetas. Presupuesto total 60 s y 10 s por
  `STATUS`; NO/BAD salta la carpeta y conserva su contador; un error de red detiene el recorrido y
  descarta la sesión. Los cambios se guardan de una vez al final (cancelar el futuro no deja nada a
  medias; si se cancela en mitad de un `STATUS` la sesión se descarta) conservando `uid_validity`.
* Política (`correo-sync/src/scheduler.rs`, pura y con tests con `FakeClock`): `Action::RefreshUnread` se
  emite (a) tras la PRIMERA sincronización correcta de la bandeja, (b) tras cada barrido de carpetas
  secundarias y (c) con `Event::Resumed`; nunca en pausa/sin red/sin autorización, uno a la vez
  (`Event::UnreadRefreshDone` libera el hueco) y, con ahorro de energía, batería baja o red de pago, como
  mucho uno cada 30 min (las peticiones rechazadas se descartan).
* `SyncRunner` lo ejecuta en una tarea propia y cancelable (Pause y Resumed la abortan; el bucle de
  eventos no se bloquea), y la lanza cuando no hay sincronizaciones en curso ni pendientes. Si cambió
  algún contador envía `SyncNotice::CountsChanged { account }`; la interfaz (`window.rs`
  `on_counts_changed`) relee solo la barra lateral, sin recargar la lista de mensajes.
* Para medir contra un servidor real: tiempo de `list_mailboxes` (objetivo < ~1,5 s en Gmail) y duración
  de `refresh_unread`.

### Cargar mensajes anteriores

`MailBackend::fetch_older(mailbox, count)` (por defecto: nada que cargar) trae las `count` cabeceras
anteriores al UID más bajo en caché con el mismo recorrido por tramos, y devuelve `OlderPage { fetched,
exhausted }`; con el UID 1 en caché responde sin red. En la interfaz (`older.rs`, `message_list.rs`,
`window.rs`) la fila del final aparece solo cuando no quedan filas locales (la paginación local es de 50 en
50), sin búsqueda activa y con la carpeta sin sincronizar/fallar: botón «Cargar mensajes anteriores», spinner
al cargar, «No hay mensajes más antiguos» (no se vuelve a pedir en la sesión) o error con «Reintentar».
También se pide sola al llegar al final de una lista desplazable. En la bandeja unificada se pide una carga
por cuenta (cada carpeta Entrada visible) y el estado se agrega.

### Nombres de carpeta en UTF-7 modificado

`correo_core::{decode_modified_utf7, encode_modified_utf7}` (RFC 3501 §5.1.3). Solo para mostrar: la ruta
cruda `Mailbox.path` no se modifica nunca (SELECT/COPY, claves de plegado de `ui-state`). El árbol de
carpetas, «Mover a…», la barra lateral y los avisos usan el nombre decodificado; la demo trae carpetas como
`Problemas de sincronizaci&APM-n`.

### Demo con historial largo

`Archives/2014` (500 mensajes simulados en el «servidor», solo 100 en caché; cuatro cargas y se agota),
`2015` (130; una carga y se agota), `2016` (la carga no termina: spinner) y `2019` (falla a los ~1 s con
«Reintentar»). Tests: `cargo test -p correo-imap paging`, `plan_range_sync`, `cargo test -p correo-app older`.

## Avisos de GTK

La interfaz no debe emitir ningún `Gtk-CRITICAL`, `Gtk-WARNING` ni `Adwaita-WARNING`. Para comprobarlo
(arranca la demo en Xvfb, aislada con `dbus-run-session` y XDG temporales, y recorre escenarios con
clics y teclas): `scripts/check-gtk-warnings.sh` (requiere `Xvfb`, `dbus-run-session` y `python3`;
no forma parte de `cargo test` porque necesita display).

## Correos HTML de demo (vista completa con WebKit)

Con `CORREO_DEMO=1` (solo compilaciones de depuración) la primera cuenta trae ocho correos HTML al principio de la Entrada (`crates/correo-app/src/demo_html.rs`):

| Correo | Qué prueba |
|---|---|
| Boletín nº 42 | tablas anidadas, estilos en línea, imagen remota https (bloqueada), botón, zoom en ventana estrecha |
| Factura F-2026-1042 | tabla con cabecera, pie y botón |
| Re: Re: Re: Reunión del jueves | citas anidadas (`blockquote`) y lista |
| Tu resumen semanal | `<style>` con colores oscuros propios |
| Tu agenda de la semana (soporta modo oscuro) | `<meta name="color-scheme" content="light dark">` y `@media (prefers-color-scheme: dark)`: en tema oscuro usa su propia paleta, sin filtro |
| Fotos del mes: sendero de la sierra | texto negro sobre blanco con un PNG a color (`data:`), iconos con transparencia y un `background-image`: la oscurización automática no debe invertir las imágenes |
| Pedido #A-10293 confirmado | transaccional: cabecera de color, tabla con etiquetas de estado y cuatro botones de colores |
| URGENTE: verifica tu cuenta | hostil: script, iframe, object, embed, form, `meta refresh`, `<base>`, enlace engañoso (`https://paypal.com` que apunta a `evil.example`), imágenes de seguimiento y fondo remoto |

```sh
CORREO_DEMO=1 cargo run -p correo-app                              # vista completa (por defecto)
CORREO_DEMO=1 CORREO_HTML_RENDERER=light cargo run -p correo-app   # visor ligero
CORREO_DEMO=1 CORREO_HTML_RENDERER=auto cargo run -p correo-app    # WebKit solo si hace falta
cargo run -p correo-app --no-default-features                      # sin compilar WebKit
```

Tema oscuro: `ADW_DEBUG_COLOR_SCHEME=prefer-dark` (o `prefer-light`). El menú «Más» alterna «Vista ligera» / «Vista completa» (se recuerda en la sesión) y, en tema oscuro, «Colores originales» / «Adaptar al tema oscuro».

Modo oscuro de la vista completa (ver `docs/DESIGN.md` §6.4): `html_dark_mode = "auto" | "original" | "invert"` en `config.toml` o `CORREO_HTML_DARK_MODE`. Para comprobar el cambio de tema en caliente (la app lee `org.gnome.desktop.interface color-scheme`): dentro de `dbus-run-session` con XDG temporales, arrancar la app sin `ADW_DEBUG_COLOR_SCHEME` y lanzar `gsettings set org.gnome.desktop.interface color-scheme prefer-dark` / `prefer-light`; el mensaje abierto debe cambiar sin recargarse (solo los correos con `prefers-color-scheme` muestran un instante el esqueleto). Tests puros: `cargo test -p correo-app dark`.

Comprobar que el correo hostil no hace red: `python3 -m http.server 8099 --bind 127.0.0.1` en otra terminal, abrir ese correo (y pulsar «Cargar imágenes remotas»); el registro del servidor debe quedar vacío. Al pulsar el enlace `https://paypal.com` debe aparecer el diálogo «¿Abrir este enlace?». Procesos auxiliares: `ps -o pid,rss,comm --ppid $(pgrep -x correo-app)`; el proceso web desaparece unos 30 s después de dejar de ver HTML.

Tests puros: `cargo test -p correo-app sanitize` (saneado, CSP, heurística), `policy` (navegación) y `config`.

## Pruebas de interfaz sin servidor (solo depuración)

Con `CORREO_DEMO=1` la app usa un `MailBackend` falso (`demo_backend.rs`) y datos ficticios:

| Variable | Efecto |
|---|---|
| `CORREO_DEMO_SYNC=loading` | las carpetas se quedan «cargando» (barra fina; esqueleto si no hay caché, p. ej. «Proyectos») |
| `CORREO_DEMO_SYNC=failed` | las carpetas fallan a los 1,2 s (aviso o página de error con «Reintentar») |
| `CORREO_DEMO_SAVE_DIR=<dir>` | «Guardar» un adjunto no abre el diálogo: guarda en ese directorio |
| `CORREO_DEMO_NO_LAUNCH=1` | «Abrir» un adjunto no lanza nada y mantiene «Abriendo…» 4 s |

Los mensajes «Presupuesto del trimestre» con clip (p. ej. el de Ana Ruiz) traen cuatro adjuntos de prueba: uno que termina
(~4 s), uno que se detiene al 40 % hasta cancelarlo, uno que falla al 60 % y un `.sh` que pide confirmación antes de descargar.

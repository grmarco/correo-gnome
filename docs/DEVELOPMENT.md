# Desarrollo

## Avisos de GTK

La interfaz no debe emitir ningún `Gtk-CRITICAL`, `Gtk-WARNING` ni `Adwaita-WARNING`. Para comprobarlo
(arranca la demo en Xvfb, aislada con `dbus-run-session` y XDG temporales, y recorre escenarios con
clics y teclas): `scripts/check-gtk-warnings.sh` (requiere `Xvfb`, `dbus-run-session` y `python3`;
no forma parte de `cargo test` porque necesita display).

## Correos HTML de demo (vista completa con WebKit)

Con `CORREO_DEMO=1` (solo compilaciones de depuración) la primera cuenta trae cinco correos HTML al principio de la Entrada (`crates/correo-app/src/demo_html.rs`):

| Correo | Qué prueba |
|---|---|
| Boletín nº 42 | tablas anidadas, estilos en línea, imagen remota https (bloqueada), botón, zoom en ventana estrecha |
| Factura F-2026-1042 | tabla con cabecera, pie y botón |
| Re: Re: Re: Reunión del jueves | citas anidadas (`blockquote`) y lista |
| Tu resumen semanal | `<style>` con colores oscuros propios |
| URGENTE: verifica tu cuenta | hostil: script, iframe, object, embed, form, `meta refresh`, `<base>`, enlace engañoso (`https://paypal.com` que apunta a `evil.example`), imágenes de seguimiento y fondo remoto |

```sh
CORREO_DEMO=1 cargo run -p correo-app                              # vista completa (por defecto)
CORREO_DEMO=1 CORREO_HTML_RENDERER=light cargo run -p correo-app   # visor ligero
CORREO_DEMO=1 CORREO_HTML_RENDERER=auto cargo run -p correo-app    # WebKit solo si hace falta
cargo run -p correo-app --no-default-features                      # sin compilar WebKit
```

Tema oscuro: `ADW_DEBUG_COLOR_SCHEME=prefer-dark`. El menú «Más» alterna «Vista ligera» / «Vista completa» (se recuerda en la sesión).

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

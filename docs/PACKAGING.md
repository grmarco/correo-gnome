# Empaquetado de Correo

Identificador de aplicación: `io.github.gmarco.Correo`. Binario: `correo-app`.

## Ficheros

| Ruta | Contenido |
|---|---|
| `data/io.github.gmarco.Correo.desktop` | Entrada de escritorio (`mailto:`, `Exec=correo-app %u`) |
| `data/io.github.gmarco.Correo.metainfo.xml` | Metadatos AppStream (descripción, licencia, OARS, releases) |
| `data/icons/hicolor/scalable/apps/io.github.gmarco.Correo.svg` | Icono a color |
| `data/icons/hicolor/symbolic/apps/io.github.gmarco.Correo-symbolic.svg` | Icono simbólico |
| `Makefile` | `build`, `install`, `uninstall`, `clean` |
| `flatpak/io.github.gmarco.Correo.yaml` | Manifiesto Flatpak |
| `flatpak/cargo-sources.json` | Generado, no versionado (ver más abajo) |

## Compilación e instalación nativas

```sh
make build                          # cargo build --release -p correo-app (sin root)
sudo make install PREFIX=/usr       # binario, .desktop, metainfo y iconos
sudo make uninstall PREFIX=/usr
```

`make install` no compila por sí mismo si ya existe `target/release/correo-app`; compila solo si falta. Con `DESTDIR=...` (empaquetado) no se actualiza la caché de iconos del host.

## Perfil de compilación

`[profile.release]` en el `Cargo.toml` raíz: `lto = "thin"`, `codegen-units = 1`, `strip = true`, `panic = "abort"`.

`panic = "abort"` compila con todas las dependencias. Efecto a tener en cuenta: un pánico en una tarea de tokio ya no se aísla como `JoinError`; aborta el proceso entero. Si se prefiere resiliencia frente a tamaño, quitar esa línea.

## Flatpak

Requisitos: `flatpak`, `flatpak-builder`, y los runtimes `org.gnome.Platform` / `org.gnome.Sdk` 50 con la extensión `org.freedesktop.Sdk.Extension.rust-stable` (de Flathub).

1. Generar las fuentes de Cargo a partir del lockfile. Con `flatpak-cargo-generator.py` (repositorio `flatpak/flatpak-builder-tools`, carpeta `cargo`):

   ```sh
   python3 flatpak-cargo-generator.py Cargo.lock -o flatpak/cargo-sources.json
   ```

   Alternativa: `cargo vendor` y un `[source]` apuntando al directorio vendorizado. El manifiesto actual espera el JSON del generador.

2. Construir e instalar en el usuario:

   ```sh
   flatpak-builder --user --install --force-clean build-dir flatpak/io.github.gmarco.Correo.yaml
   flatpak run io.github.gmarco.Correo
   ```

   El módulo compila con `make build CARGO='cargo --offline'` e instala con `make install PREFIX=/app`.

### Permisos (`finish-args`)

| Permiso | Motivo |
|---|---|
| `--share=network` | IMAP, SMTP, OAuth2, People y Graph |
| `--socket=wayland`, `--socket=fallback-x11` | Ventana |
| `--device=dri` | Render por GPU; sin él GTK usa software y gasta más CPU |
| `--talk-name=org.freedesktop.secrets` | Tokens OAuth2 en GNOME Keyring (`oo7`) |
| `--talk-name=org.freedesktop.Notifications` | Notificaciones |
| `--talk-name=org.freedesktop.UPower` | Estado de batería (`power.rs`) |
| `--system-talk-name=org.freedesktop.login1` | Suspender/reanudar (`PrepareForSleep`, `power.rs`) |
| `--filesystem=xdg-download` | Guardar adjuntos en Descargas |

No hay acceso al home. Si se quiere eliminar `xdg-download`, guardar adjuntos con el portal `FileChooser` sería la vía adecuada.

## Validación

| Comprobación | Resultado |
|---|---|
| `desktop-file-validate data/io.github.gmarco.Correo.desktop` | Correcto, sin avisos |
| `appstreamcli validate --no-net data/io.github.gmarco.Correo.metainfo.xml` | Correcto |
| `appstreamcli validate --no-net --pedantic ...` | Un aviso: `cid-contains-uppercase-letter`. El ID con mayúsculas viene fijado por `APP_ID`; no se cambia |
| XML de los dos SVG | Bien formados |
| Manifiesto Flatpak | Sintaxis YAML correcta. **No construido**: `flatpak` no está instalado en esta máquina |

## Medidas (release, 2026-10-21)

Máquina de desarrollo, `CORREO_DEMO=1 timeout 15 xvfb-run -a ./target/release/correo-app`, medido a los 10 s de arranque.

| Métrica | Valor |
|---|---|
| Tamaño de `target/release/correo-app` | 13 785 808 bytes (13,8 MB), ya sin símbolos |
| VmRSS en reposo | 122 MB (121 880 kB en una ejecución, 121 992 kB en la segunda) |
| RssAnon (memoria privada) | 20 MB |
| RssFile (bibliotecas compartidas mapeadas) | 102 MB |
| Hilos | 23 |
| CPU en reposo | 0 ticks en 3 s (menos de 0,4 %) |

Notas sobre la medida:

- `CORREO_DEMO` solo tiene efecto en compilaciones de depuración (`cfg!(debug_assertions)`). En release la app arranca sin datos de demostración, sin cuentas, y muestra el asistente de alta.
- El RSS está dominado por las bibliotecas compartidas de GTK4 y libadwaita (`RssFile`). Son páginas compartidas con el resto del sistema. La memoria privada (`RssAnon`, 20 MB) es la cifra más representativa del coste propio de la app.
- El objetivo del `PLAN.md` (menos de 60 MB de RSS con dos cuentas) no se verifica con esta medida: se hizo sin cuentas ni sincronización. Repetir la medida con dos cuentas reales o con datos de demo en depuración.

## Pendiente

- Verificar la versión del runtime (`50`) en Flathub y construir el manifiesto con `flatpak-builder`.
- Sustituir la URL de `homepage` y `bugtracker` del metainfo (`https://github.com/gmarco/correo`) por la real.
- Añadir capturas de pantalla al metainfo antes de publicar.

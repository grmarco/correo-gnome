# Correo: guía de diseño de interfaz

Estado: propuesta (fase 1). Implementación en la fase 2, tras terminar los cambios en curso en `window.rs`, `sidebar.rs`, `message_list.rs`, `composer*.rs` y `reader.rs`. Hoja de estilos asociada: `data/style.css` (clases con prefijo `correo-`, aún sin cargar). Versiones objetivo: GTK 4.22, libadwaita 1.9, gtk4-rs 0.11, libadwaita-rs 0.9.

## 0. Auditoría de la interfaz actual

Capturas hechas con la compilación de depuración en modo demo (oscuro y claro, 1100x720 y 560x720, lector y redactor).

Lo que funciona: estructura de dos `NavigationSplitView` anidados con breakpoints (720sp y 1000sp), `ListView` virtualizada, `ToastOverlay`, `StatusPage` para vacíos.

Lo que lo hace «feíllo»:

1. **Sin identidad visual.** No hay CSS propio; todo es el aspecto por defecto. No se carga ninguna hoja de estilos.
2. **Lista de hilos densa y plana.** Remitente, contador, fecha y clip en una sola línea de `caption`; sin avatar, sin indicador de no leído (solo negrita), sin separación ni redondeo de la selección; la fila seleccionada ocupa todo el ancho en un azul chillón (modo claro).
3. **Barra lateral.** Las cabeceras de cuenta son el email en `heading dim-label` (visualmente débiles pero ruidosas), sin color ni avatar por cuenta; todas las cuentas usan el mismo icono por carpeta; «Todas las bandejas de entrada» se trunca a 220px; el badge de no leído es solo número azul.
4. **Cabecera del lector.** Asunto `title-2` correcto, pero metadatos (`De`, `Para`, fecha) en `caption` apretados, sin avatar del remitente; seis botones-icono sueltos con fondo propio (parecen una fila de fichas), sin agrupar, sin separar acciones destructivas; el icono de archivar se ve como un bloque.
5. **Estados.** El vacío del lector es un `StatusPage` enorme con icono gris; el error repite texto técnico («autenticación: modo demostración…») sin jerarquía. La carga es un `Spinner` suelto, sin esqueleto.
6. **Redactor.** Etiquetas «De:/Para:/Asunto:» a la izquierda con campos con marco; el campo Para ocupa 200px con `Cc` y `Cco` a la derecha como botones; chips = `.card` con botón de cerrar (anchos y toscos); barra de formato con botones planos sin estado `checked`, sin separadores de grupo; cuerpo sin tipografía de lectura; botón de adjuntar suelto abajo.
7. **Cabeceras.** `Redactar` es `suggested-action` rectangular junto a un buscador estrecho; ventana secundaria del redactor con los tres botones de ventana y sin ancho mínimo cómodo.
8. **Sin animaciones propias.** Ni aparición de filas, ni transición del panel lector, ni feedback de sincronización.

Utilidades de captura (en el scratchpad de la sesión, no en el repo): `snap3.sh` y `act.py`.

```
cd <scratchpad>; xvfb-run -a -s "-screen 0 1400x900x24" ./snap3.sh shots/x.png ACCIONES...
# Acciones: click:x:y  rclick:x:y  key:Control_L+n  resize:w:h  wait:s
# SCHEME=light|dark (usa ADW_DEBUG_COLOR_SCHEME). Ejemplo: lector claro estrecho:
SCHEME=light xvfb-run -a -s "-screen 0 1400x900x24" ./snap3.sh shots/r.png resize:560:720 click:300:90
```

(`csscheck`, también en el scratchpad, valida un CSS con el parser real de GTK: `xvfb-run -a ./csscheck data/style.css`.)

## 1. Principios

1. **Idiomático antes que original.** Usar widgets y clases de libadwaita; el CSS propio solo refina (radio, tinte, peso, espaciado). Nada de rehacer controles.
2. **Jerarquía por peso y opacidad, no por tamaño.** Tres niveles de texto: primario (100 %), secundario (~62 % de opacidad), terciario/fecha (~62 % y `0.8rem`). El no leído se marca con peso 700 + punto de acento.
3. **El acento es escaso.** Acento (`@accent_bg_color`) solo para: selección, punto y contador de no leído, botón primario de la pantalla (Redactar/Enviar), foco, estados `checked`. Nunca para texto largo.
4. **Color de cuenta como pista, no como decoración.** Cada cuenta tiene uno de 8 tonos fijos (avatares, punto en la barra lateral, chips de etiqueta). Los demás colores salen de libadwaita para respetar claro/oscuro/alto contraste.
5. **Densidad cómoda.** Filas de lista de ~72px; rejilla de 4px (4/8/12/16/24); radios 8 (controles), 12 (filas, tarjetas), 99 (píldoras).
6. **Movimiento con propósito y barato.** Solo opacidad, transform y color; 120-250ms; sin animaciones continuas salvo spinner/esqueleto mientras se carga. Todo respeta `gtk-enable-animations`.
7. **Accesible por defecto.** Contraste >= 4,5:1 en texto, `accessible-role/label` en iconos-botón, foco visible, nada transmitido solo por color (no leído = punto + peso), objetivos >= 32px, navegación completa por teclado.
8. **Ligera.** Sin sombras difusas, sin `filter`, sin blur, sin imágenes de fondo. `ListView` reciclada: las filas no crean widgets nuevos al enlazar.

## 2. Color

Todo con nombres de libadwaita (se adaptan solos a claro/oscuro y al acento del sistema, libadwaita 1.6+):

| Uso | Color |
|---|---|
| Fondo de ventana / lector / lista | `@window_bg_color` (lista y lector), `@view_bg_color` solo para el cuerpo editable |
| Barra lateral | `@sidebar_bg_color` (lo aplica `NavigationSplitView`; no sobrescribir) |
| Cabeceras | `@headerbar_bg_color` (por `ToolbarView`) |
| Tarjetas, adjuntos | `@card_bg_color` + borde `alpha(currentColor, .08)` |
| Selección de fila | `color-mix(in srgb, @accent_bg_color 16%, transparent)` (22 % en hover); texto sigue `@window_fg_color` |
| Hover de fila | `alpha(currentColor, .05)` |
| Punto, contador de no leídos, botón primario | `@accent_bg_color` / `@accent_fg_color` |
| Fecha de hilo no leído | `@accent_color` |
| Errores | `@error_color`, `@error_bg_color` (chips inválidos, StatusPage de error) |
| Destructivo en hover (Borrar) | `@destructive_color` con fondo `alpha(@destructive_bg_color, .18)` |
| Separadores | `@borders` |

Paleta de avatares por cuenta (`@define-color correo_acct_N`, en `style.css`; mismos valores en claro y oscuro; texto blanco):

| N | Tono | Hex |
|---|---|---|
| 1 | azul | `#1c71d8` |
| 2 | turquesa | `#0f7f8f` |
| 3 | verde | `#2b8a3e` |
| 4 | ámbar | `#b85c00` |
| 5 | rojo | `#c01c28` |
| 6 | rosa | `#b8318a` |
| 7 | violeta | `#7d4cc0` |
| 8 | pizarra | `#5e6b7a` |

Asignación: el color de una cuenta es `index_en_config % 8`, estable (no depende del orden de carga). En la lista de hilos el avatar del **remitente** usa un tono derivado de un hash del email del remitente (`hash % 8`) para que la misma persona tenga siempre el mismo color; el color de **cuenta** solo aparece en la vista unificada (punto pequeño junto a la fecha o `correo-label-chip` con el nombre corto de la cuenta) y en la barra lateral.

Modo claro/oscuro: se sigue `adw::StyleManager` (por defecto `ColorScheme::Default`); no se fuerza nada. Ofrecer en el menú principal «Sistema / Claro / Oscuro» es opcional (fase 3).

## 3. Tipografía

La fuente es la del sistema (Adwaita Sans / Cantarell / Inter): no se fija familia. Escala (clases de libadwaita primero, CSS propio cuando falta):

| Rol | Clase | Tamaño aprox. | Peso |
|---|---|---|---|
| Asunto en el lector | `correo-reader-subject` (propia; base `title-2`) | 1.45rem | 800 |
| Título de StatusPage | `title-1` (por defecto en StatusPage) con `correo-empty` | 1.8rem | 700 |
| Remitente en el lector | `correo-reader-from` (base `heading`) | 1rem | 700 |
| Remitente en lista | `correo-sender` | 1rem | 500 / 700 no leído |
| Asunto en lista | `correo-subject` | 1rem | 400 / 600 no leído |
| Extracto, metadatos | `correo-snippet`, `correo-reader-meta` (base `caption` + `dim-label`) | 0.9rem / 0.86rem | 400 |
| Fecha, contadores | `correo-date`, `.numeric`, `correo-tabular` | 0.8rem | 400 / 600 |
| Títulos de sección (sidebar, grupos) | `correo-overline` / `correo-group-header` | 0.78rem, versalitas no, `letter-spacing: .06em` | 700 |
| Cuerpo en lector/editor | `correo-reader-body` | 1.04rem, `line-height` 1.55 | 400 |

Números tabulares (`font-feature-settings: "tnum"`) en fechas, horas, contadores y tamaños de adjunto, para que las columnas no «bailen» al cambiar de dígitos. `.dim-label` solo para textos cortos; para el resto, `opacity` en las clases propias (evita el truco de `.dim-label` que reduce también el contraste de iconos).

## 4. Espaciado y forma

- Rejilla de 4px. Márgenes de contenido 16 (24 en el lector a >= 700sp). Separación entre bloques 12; entre líneas de texto relacionadas 2-4.
- Fila de hilo: padding 10/10/10/8, avatar 40px, ~72px de alto. Fila de barra lateral: `min-height: 34px`.
- Radios: 8 (botones de barra, filas de sidebar), 12 (filas de hilo, tarjetas de adjunto), 99 (chips, badges, buscador, `Redactar`).
- Selección en lista y sidebar: **redondeada con margen de 6px** a ambos lados (como Geary/Files), no a todo el ancho.
- Sin separadores entre filas de hilo (el espacio y el redondeo bastan); sí una línea `@borders` entre bloques del redactor y sobre la barra de formato.

## 5. Iconografía

Solo iconos simbólicos de Adwaita (`*-symbolic`), tamaño 16 (por defecto), 12 dentro de chips, 32-48 en estados vacíos con el modificador `.icon` de StatusPage. Nombres reales, estables en el tema Adwaita (los que no estén en el tema base de Adwaita, se incluirán en `data/icons/hicolor/symbolic` si el entorno no los trae):

| Función | Icono |
|---|---|
| Entrada | `mail-inbox-symbolic` (en vez de `mail-unread-symbolic`; este queda para «Todas las entradas» o no leídos) |
| Todas las entradas (unificada) | `mail-unread-symbolic` |
| Borradores | `document-edit-symbolic` |
| Enviados | `mail-send-symbolic` |
| Archivo | `mail-archive-symbolic` (carpeta archivo) |
| Spam | `mail-mark-junk-symbolic` |
| Papelera | `user-trash-symbolic` |
| Carpeta genérica | `folder-symbolic` |
| Etiqueta/subcarpeta | `tag-symbolic` |
| Responder / a todos / reenviar | `mail-reply-sender-symbolic`, `mail-reply-all-symbolic`, `mail-forward-symbolic` |
| Marcar no leído / leído | `mail-unread-symbolic` / `mail-read-symbolic` |
| Adjunto | `mail-attachment-symbolic` |
| Sincronizar | `view-refresh-symbolic` |
| Buscar | `system-search-symbolic` (SearchEntry lo trae) |
| Añadir cuenta | `list-add-symbolic`; cuenta: `avatar-default-symbolic` |
| Menú principal | `open-menu-symbolic` |
| Quitar chip | `window-close-symbolic` (12px) |
| Formato | `format-text-bold-symbolic`, `format-text-italic-symbolic`, `format-text-underline-symbolic`, `view-list-bullet-symbolic`, `format-indent-more-symbolic`, `insert-link-symbolic` |
| Estados vacíos | `mail-read-symbolic` (sin mensajes), `mail-unread-symbolic` (sin cuentas), `edit-find-symbolic` (sin resultados), `network-offline-symbolic` (offline), `dialog-error-symbolic` (error) |
| Vista completa / ligera | entrada de texto en el menú «Más» (sin icono) |

Cada botón de icono: `tooltip-text` y `accessible-label` (con `gtk::AccessibleExt`/`update_property`) explícitos.

## 6. Diseño pantalla por pantalla

### 6.1 Ventana principal

Estructura actual (se mantiene): `AdwApplicationWindow` > `ToastOverlay` > `Stack{main, empty}`; `main` = `NavigationSplitView` externo (cuentas) con el interno (lista, lector). Cambios:

- **Anchos**: barra lateral `min-sidebar-width: 230`, `max-sidebar-width: 280`, `sidebar-width-fraction: 0.2`; lista `min 340 / max 440 / fraction 0.3`. Ventana por defecto 1180x760.
- **Breakpoints** (ver §8): se mantienen 720sp y 1000sp.
- Separación visual entre paneles: la que dan `ToolbarView`/`NavigationSplitView`; no añadir bordes propios.

### 6.2 Barra lateral (`sidebar.rs`)

- `ListBox.navigation-sidebar` + `correo-sidebar` (selección redondeada tintada).
- **Fila superior fija «Todas las entradas»** (icono `mail-unread-symbolic`, texto «Entradas unificadas» para no truncar), badge de acento si hay no leídos.
- **Cabecera de cuenta**: `gtk::Box` horizontal con `adw::Avatar` de 24px (`size`, iniciales de la cuenta, clase `correo-acct-N`), etiqueta con el email en `heading` ellipsizada, más a la derecha un botón `flat` `view-more-symbolic` (aparece con hover/foco) que abre el mismo menú que el clic derecho. Fila no seleccionable, clase `correo-account-header`.
- **Carpetas**: icono + nombre + `correo-badge` (acento solo en Entrada/Borradores; el resto, tono neutro). Sangrado de 4px respecto a la cabecera.
- Plegado de cuenta (opcional, fase 3): chevron `pan-down-symbolic` ↔ `pan-end-symbolic` con `gtk::Revealer` (`SlideDown`, 200ms).
- Pie: botón `Añadir cuenta` como fila final `flat` con `list-add-symbolic` (sustituye al botón de la cabecera); la cabecera lleva el menú principal (`open-menu-symbolic` con `gtk::MenuButton`: Preferencias, Atajos, Acerca de).
- Menú contextual de cuenta: `gtk::PopoverMenu` desde `gio::Menu` (idiomático) en lugar del botón rojo suelto: «Sincronizar ahora», «Eliminar cuenta…» (destructivo, con `adw::AlertDialog` de confirmación).

### 6.3 Lista de hilos (`message_list.rs`)

Cabecera (`HeaderBar` de la lista): izquierda `correo-sync` (refrescar); título `SearchEntry` redondeado (`correo-search`) en `adw::Clamp` (máx 320); derecha `Redactar` con icono `mail-message-new-symbolic` + etiqueta (`correo-compose-button`, `suggested-action`). Con la ventana estrecha, el botón de redactar pasa a solo icono (setter de breakpoint sobre `label`/ `icon-name`).

Fila (`gtk::Box` horizontal, `correo-thread-row`, clase `correo-unread` si `unread > 0`):

```
[avatar 40] [ remitente (500/700)               fecha ]
            [ asunto (400/600)           [clip] [n]   ]
            [ extracto (0.9rem, 62 %), 1 línea        ]
```

- Avatar: `adw::Avatar` size 40, `text` = nombre (iniciales automáticas), `show-initials: true`, clase `correo-acct-N` (hash del remitente). Sobre el avatar, el **punto de no leído** (`correo-unread-dot`, 8px) en una `gtk::Overlay` esquina superior izquierda; alternativa de bajo coste: punto a la izquierda del remitente (columna de 12px, `valign=center`).
- Fecha: `correo-date`, alineada a la derecha y arriba; no leído en acento.
- Contador del hilo `correo-count` (solo `count > 1`), clip `mail-attachment-symbolic` a 12px, chips de etiqueta `correo-label-chip` (máx. 1 visible + `+n`).
- Dos líneas de texto bajo el remitente (asunto y extracto) para ~72px de alto. Con `compact` (ventana estrecha, breakpoint) se oculta el extracto.
- Selección: redondeada 12px tintada (`row:selected`), hover sutil; el color del texto no cambia de contraste.
- **Cabeceras de grupo** («Hoy», «Ayer», «Esta semana», «Más antiguos»): `gtk::ListHeaderFactory` en `ListView` con `GtkSectionModel` (`gtk::SortListModel` con `Sorter`/`set_section_sorter`) y `correo-group-header`. Alto impacto, riesgo medio (requiere modelo seccionado); es fase 3.
- Hilo con varios no leídos: el contador del sidebar se actualiza con una transición de opacidad corta (140ms).
- Carga de más páginas: fila final con `adw::Spinner` (1.6+) de 16px, centrada.
- Esqueletos: al cambiar de carpeta sin datos en caché se muestran 6 filas con `correo-skeleton` (avatar circular + 3 barras de anchos 40/70/55 %) en un `gtk::Stack` («skeleton» → «list»), con transición `Crossfade` 160ms.

### 6.4 Lector (`reader.rs`)

Contenedor: `adw::Clamp` (`maximum-size: 760`, `tightening-threshold: 560`) dentro del `ScrolledWindow`, así el texto tiene un ancho de lectura cómodo en pantallas anchas. Las cabeceras de panel `HeaderBar` del lector: título vacío; a la derecha, el grupo de acciones (ver abajo) y `view-more-symbolic`.

Cabecera del mensaje (`correo-reader-header`):

```
[avatar 48]  Marta Solé                                  21 oct, 15:03
             marta@ejemplo.com · para ti            [v detalles]
Asunto del mensaje (correo-reader-subject, wrap)
[chips de adjunto]  (tarjetas `correo-attachment`)
```

- Línea de remitente con `adw::Avatar` 48px + nombre (`correo-reader-from`) + email y «para ti / para n más» en `correo-reader-meta`. Los campos Para/Cc/Cco completos se muestran plegados detrás de un botón «Detalles» (`gtk::Revealer` con `SlideDown` 200ms) para quitar ruido.
- **Acciones agrupadas** en la `HeaderBar` (no bajo el asunto): `box.linked` con `correo-actions`: Responder (`mail-reply-sender-symbolic`), Responder a todos (`mail-reply-all-symbolic`), Reenviar (`mail-forward-symbolic`); separado: Archivar (`mail-archive-symbolic`), Borrar (`user-trash-symbolic`, clase `correo-danger`), y un `MenuButton` «Más» con «Marcar como no leído» y «Vista completa» / «Vista ligera». Atajos en tooltips (`Responder (R)`, `Borrar (Supr)`). Con ventana estrecha solo se muestran Responder, Archivar y Borrar; el resto va al menú.
- Adjuntos: en lugar de `boxed-list`, `adw::WrapBox` (1.7) con tarjetas `correo-attachment` (icono de tipo en cuadro tintado 36px, nombre en `heading`, tamaño en `caption` tabular, botones «Guardar»/«Abrir» planos que aparecen con hover).
- Cuerpo: `correo-reader-body`, 1.04rem, interlineado 1.55, selección de texto permitida. Enlaces con el acento del sistema (`link`).
- **Citas plegables**: el texto citado (`>`) se agrupa en un bloque; por defecto plegado tras un botón-píldora «···» (`correo-quote-toggle`) y al pulsar se despliega con `gtk::Revealer` (`SlideDown`, 200ms, `ease-out`); la cita desplegada usa `correo-quote` (barra izquierda de acento tenue, opacidad 0.8). Con `gtk::TextView`, se implementa con una `gtk::TextChildAnchor` que aloja el botón y una etiqueta de texto oculta/visible mediante `gtk::TextTag` `invisible`; la animación es el fundido del `Revealer` en un `TextChildAnchor`; si resulta compleja, solo conmutar la etiqueta `invisible` (sin animación) en la primera iteración.
- Banner de privacidad (imágenes remotas): `adw::Banner` actual, con botón «Cargar imágenes»; se muestra con la animación propia de `Banner` (revealer).
- Estado «Selecciona un mensaje»: ver §6.7.

#### Vista completa de mensajes HTML (WebKit, `reader_web*.rs`)

Los mensajes con HTML se muestran por defecto con WebKitGTK (feature `webkit`, activa por defecto). Los de solo texto plano usan siempre el visor ligero: nunca se crea un `WebView` para ellos.

- **Modo** (`html_renderer` en `config.toml` o `CORREO_HTML_RENDERER`): `"webkit"` (por defecto), `"light"` (visor ligero: ammonia + `TextView`) o `"auto"` (WebKit solo si `needs_rich_rendering(html)`: tablas, imágenes, `<style>`, estilos en línea de maquetación, `bgcolor`, etc.). La entrada «Vista ligera» / «Vista completa» del menú «Más» alterna el modo del mensaje y la elección se recuerda el resto de la sesión (no por mensaje).
- **Tarjeta**: el `WebView` va dentro de `correo-mail-card`: fondo blanco, radio 12, anillo de 1px (`alpha(currentColor, .10)`) y sombra suave, también en tema oscuro, porque los correos asumen fondo blanco. Si el correo declara su propio fondo (`<body style>`, `bgcolor` o `<style>`), este cubre el lienzo entero y manda sobre el blanco. Márgenes 24px a los lados, como el cuerpo de texto.
- **Tipografía y ancho**: 15px/1.5 en Cantarell, `adw::Clamp` del lector (780). El scroll es el propio del `WebView` (una sola barra, el resto del lector no se desplaza). Si una tabla fija un ancho mayor que el disponible (p. ej. boletines de 600px en una ventana estrecha) se reduce el zoom (hasta 0,55) en vez de forzar scroll horizontal.
- **Carga**: el esqueleto existente (`loading`) se muestra hasta que el documento está listo y entonces se hace el fundido (`Stack` `Crossfade`, 160ms; 0 si `adw::is_animations_enabled` es falso).
- **Banner**: «Imágenes bloqueadas para proteger tu privacidad» con botón «Cargar imágenes remotas» (solo `https` y solo imágenes).
- **Fallo**: si no se puede crear el `WebView`, compilar el filtro de contenido o el proceso web muere, se vuelve al visor ligero para el resto de la sesión con un aviso discreto en el banner («La vista completa no está disponible; se muestra la vista ligera»).
- **Ciclo de vida**: un único `WebView` creado al abrir el primer mensaje con HTML y reutilizado al navegar entre ellos. Al limpiar la vista, abrir un mensaje sin HTML, seleccionar varios o esperar una descarga, se descarta el documento (`about:blank`) y se arranca un temporizador de un solo disparo (30 s); si no vuelve a haber HTML visible, se destruye el `WebView` y se termina su proceso web.
- **Seguridad** (ver `reader_web_policy.rs` y `reader_web_sanitize.rs`): JavaScript, WebGL, media, plugins, almacenamiento y caché desactivados; sesión efímera; filtro de contenido que bloquea toda la red; CSP en el propio documento (`default-src 'none'`, sin scripts, marcos, objetos, formularios ni `<base>`); saneado previo con ammonia y lista blanca de maquetación (sin `script`, `form`, `iframe`, `object`, `embed`, `meta`, `base`, `link`, `svg`; `href` solo http/https/mailto; imágenes solo http(s) o `data:` raster; CSS depurado de `url()`, `@import`, `expression()`…); navegación bloqueada dentro del `WebView`, sin ventanas emergentes ni menú contextual; los enlaces se abren fuera y, si el texto visible aparenta otro dominio, piden confirmación (`link_looks_deceptive`, igual que el visor ligero).
- **Imágenes `cid:`**: se sustituyen por un marcador gris. `MessageBody.attachments` solo guarda metadatos (no los bytes) y resolverlas exigiría otra descarga IMAP. TODO documentado en `reader_web_sanitize.rs` (`CID_PLACEHOLDER`).

### 6.5 Redactor (`composer*.rs`)

Ventana `adw::Window` (o `adw::Dialog` flotante en 1.5+, preferible en escritorio estrecho) 720x640, mínimo 480x400.

- **Formulario como filas planas** (`correo-compose-row`, `min-height: 40`), etiqueta a la izquierda en gris (`correo-compose-label`, ancho fijo 56) y campo de ancho completo sin marco (`correo-compose-entry`), separadas por líneas de 1px `@borders`. Filas: `De` (selector), `Para` (con `Cc`/`Cco` como enlaces `flat` pequeños al final de la fila; aparecen filas extra con `Revealer` 160ms), `Asunto` (negrita 1.05rem).
- **Campo Para a ancho completo**: `adw::WrapBox` (1.7, sustituye al `FlowBox`) con `child-spacing: 4`, `line-spacing: 4` que contiene los chips y la `Entry` (`hexpand`, `min-width: 120`).
- **Chips**: `correo-chip` píldora tintada de acento al 14 %; contenido: `adw::Avatar` 20px con iniciales + nombre ellipsizado (máx. 28 caracteres) + botón `circular flat` 20px con `window-close-symbolic` a 12px. Chip inválido: `correo-chip-invalid`. Entrada: clase `correo-chip-enter` → `-active` en el siguiente tick (opacidad + escala 0.85→1, 140ms); salida: `correo-chip-leave` 110ms y retirar al terminar (`glib::timeout_add_local_once`). Sustituye `card` (demasiado cuadrado).
- **Selector De**: con una sola cuenta, texto del email en `correo-reader-meta` con avatar de 20px; con varias, `gtk::DropDown` con `factory` personalizada (avatar de cuenta + email), flecha estándar; no etiqueta «De:» en negrita.
- **Barra de formato** (`correo-format-bar`, tras el cuerpo o encima, en `.toolbar`): botones `toggle flat` con estado `checked` real (negrita/cursiva/subrayado/lista/cita sincronizados con la posición del cursor), separadores `gtk::Separator` verticales entre grupos [B I U] [lista, cita] [enlace]; tooltips con atajo («Negrita (Ctrl+B)»). Adjuntar se mueve aquí, al final, con `mail-attachment-symbolic`.
- **Cuerpo** (`correo-editor`): `TextView` con `left/right margin` 20, `top/bottom` 12, tipografía de lectura.
- **Adjuntos del redactor**: `adw::WrapBox` de `correo-attach-chip` bajo el cuerpo (icono + nombre + tamaño + quitar), con la misma entrada/salida que los chips. Zona de soltar: al arrastrar, un `correo-drop-hint` (overlay con borde discontinuo de acento) con fundido de 120ms.
- **Cabecera**: botón `Enviar` (`suggested-action` con `mail-send-symbolic` y etiqueta), a la izquierda `Descartar` (texto, `destructive-action` solo en el diálogo de confirmación). Mientras se envía: `adw::Spinner` y el botón se desactiva.
- Cerrar con borrador modificado: `adw::AlertDialog` («Guardar borrador / Descartar / Cancelar»).

### 6.6 Diálogo de alta de cuenta (`account_dialog.rs`)

`adw::Dialog` (ya adaptativo) con `adw::NavigationView` en tres páginas: (1) elegir proveedor como `adw::ActionRow` activables con icono grande y avatar de marca (Gmail, Outlook.com, Microsoft 365) dentro de `boxed-list`; (2) «Esperando al navegador…» con `adw::Spinner` y botón Cancelar; (3) éxito con `StatusPage` compacta (`compact`, `emblem-ok-symbolic`) y botón `Listo` `pill suggested-action`. Errores: `adw::Banner`/`StatusPage` con `error_color` (no etiqueta roja suelta). Los campos (si los hay) en `adw::EntryRow`/`PreferencesGroup`. Transición entre páginas: la propia de `NavigationView` (por defecto, 250ms).

### 6.7 Estados vacíos, carga y errores

Todos con `adw::StatusPage` + clase `correo-empty`; icono envuelto en `correo-empty-badge` (círculo tintado de 96px) cuando se usa `set_child`/paintable. Usar `compact` si el espacio es < 480px.

| Situación | Icono | Título | Descripción | Acción |
|---|---|---|---|---|
| Sin cuentas | `mail-unread-symbolic` | «Bienvenido a Correo» | «Añade una cuenta de Gmail, Outlook.com o Microsoft 365 para empezar.» | `Añadir cuenta` (`pill suggested-action`) |
| Carpeta vacía | `mail-read-symbolic` | «Todo al día» | «No hay mensajes en esta carpeta.» (Entrada) / «Esta carpeta está vacía.» | `Sincronizar` (`pill`) si procede |
| Primera sincronización | skeleton | – | – | – |
| Sin resultados | `edit-find-symbolic` | «Sin resultados» | «No hay nada que coincida con "…".» | `Borrar búsqueda` |
| Sin mensaje seleccionado | `mail-read-symbolic` | (sin título grande) | «Selecciona un mensaje para leerlo» en `dim-label` | – |
| Sin conexión | `network-offline-symbolic` | «Sin conexión» | «Se mostrarán los mensajes guardados.» | `Reintentar` |
| Error al cargar el mensaje | `dialog-error-symbolic` + `correo-error` | «No se pudo abrir el mensaje» | texto humano; detalles técnicos plegados en `gtk::Expander` («Detalles») en `monospace caption` | `Reintentar` |
| Reautorización necesaria | `dialog-password-symbolic` | «Hay que volver a conectar la cuenta» | nombre de la cuenta | `Reconectar` |

Esqueletos: lista (ver 6.3) y lector (avatar circular + barras: asunto 60 %, línea 35 %, tres líneas de cuerpo de 100/95/70 %), con `correo-skeleton`; la clase se quita al llegar el contenido y el `Stack` hace fundido de 160ms. El `Spinner` central del lector se sustituye por el esqueleto si la carga tarda > 250ms (evitar parpadeo en cargas rápidas).

Avisos efímeros: `adw::Toast` (timeout 4s, 6s si lleva botón `Deshacer`, con `set_priority`). «Mensaje archivado / borrado» con `Deshacer` (acción real: mover de vuelta). «Enviado» con botón «Ver». Errores de red persistentes: `adw::Banner` bajo la cabecera de la lista (`Sin conexión. Reintentando…`).

## 7. Catálogo de animaciones

Todas derivan de dos primitivas baratas: transiciones/keyframes CSS (cuyos tiempos GTK anula con `gtk-enable-animations = false`) y `adw::Animation`. Para las segundas, se crean con `adw::TimedAnimation` o `adw::SpringAnimation` y, antes de reproducirlas, se comprueba `adw::is_animations_enabled(&widget)`: si es `false`, se aplica el valor final al instante. Curvas: `ease-out` (`adw::Easing::EaseOutCubic`) para entradas; `ease-in` (`EaseInCubic`) para salidas; muelle (`SpringParams::new(1.0, 1.0, 400.0)`, sin rebote) solo para desplazamientos; el rebote suave `cubic-bezier(.2,.9,.3,1.2)` solo en el chip.

| Elemento | Qué anima | Duración | Curva | Implementación |
|---|---|---|---|---|
| Aparición de la lista (cambio de carpeta) | Fundido del `Stack` skeleton/lista/vacío | 160ms | ease | `Stack::set_transition_type(Crossfade)`, `set_transition_duration(160)` |
| Filas nuevas (al sincronizar) | opacidad 0→1 + `translateY(6px)→0` | 180ms | ease-out | clases `correo-row-enter` → `-active` al enlazar solo filas que no estaban (no animar el rebind por reciclaje) |
| Selección de hilo | fondo de la fila (tinte de acento) | 120ms | ease-out | `transition: background-color` en `row` (ya en CSS) |
| Cambio de mensaje en el lector | fundido del contenido | 160ms | ease-out | `Stack` interno `Crossfade` 160ms o clase `correo-fade-in` sobre el contenedor al terminar la carga |
| Punto/contador de no leído | opacidad al marcar leído | 150ms | ease-out | `transition: opacity` en `.correo-unread-dot`; el badge de sidebar cambia con `Label::set_label` (sin animar números) |
| Apertura del redactor | aparición de la ventana/diálogo | 250ms | ease-out | `adw::Dialog` (animación nativa) con `present(parent)`; si ventana, `set_opacity` de 0→1 con `TimedAnimation` 160ms |
| Chips que entran/salen | opacidad + escala 0.85→1 / 1→0.85 | 140ms / 110ms | ease-out con leve overshoot / ease-in | clases `correo-chip-enter(-active)` / `correo-chip-leave`, retirar el widget con `timeout_add_local_once(110ms)` |
| Cc / Cco | altura de la fila | 160ms | ease-out | `gtk::Revealer(SlideDown)` envolviendo la fila |
| Toasts | entrada/salida nativas de `ToastOverlay` | ~200ms | nativa | nada que hacer; usar `adw::Toast` |
| Transición lista → lector en móvil | deslizamiento horizontal con arrastre | ~250ms | nativa (muelle) | `NavigationSplitView` colapsado ya lo hace; `navigation-view` gesto de volver |
| Spinner de sincronización | giro del icono mientras sincroniza | 900ms/vuelta (continuo mientras dura) | linear | `button.correo-sync.correo-syncing image` con `@keyframes correo-spin`; la clase se añade en `sync()` y se quita al terminar. Sin sincronización no hay animación |
| Plegado de citas | altura del bloque | 200ms | ease-out | `gtk::Revealer(SlideDown/SlideUp)` |
| Esqueletos | pulso de opacidad 0.45→1 | 900ms alternate | ease-in-out | `.correo-skeleton`; solo mientras `stack == skeleton` |
| Barra de formato (estado `checked`) | color de fondo | 120ms | ease-out | CSS `transition` |
| Banner / aviso de red | revelado | ~250ms | nativa | `adw::Banner::set_revealed` |
| Hover de botones, chips, tarjetas | color de fondo | 120ms | ease-out | CSS `transition` |

Reglas: no animar más de dos propiedades a la vez; nada de animaciones con `width/height` o `margin` en listas largas; no animar durante un `splice` completo (por ejemplo al cambiar de carpeta): usar solo el fundido del `Stack`.

## 8. Comportamiento adaptativo

| Ancho (sp) | Modo | Cambios |
|---|---|---|
| > 1000 | 3 paneles | Sidebar, lista, lector visibles. Lector con `Clamp` 760. |
| 720-1000 | 2 paneles | `inner.collapsed = true`: la lista ocupa el hueco del lector; al elegir hilo se navega a «Mensaje» con botón de volver. La barra lateral sigue fija (`outer` no colapsado). |
| < 720 | 1 panel | `outer.collapsed = true` y `inner.collapsed = true`; jerarquía Carpetas → Mensajes → Mensaje con botón atrás en cada cabecera. |
| < 560 | compacto | Setters de breakpoint: ocultar extracto en la fila; `Redactar` solo con icono; acciones del lector reducidas a Responder/Archivar/Borrar + menú; márgenes del lector 16. |

Breakpoints (se mantienen los actuales y se añade `max-width: 560sp` con setters sobre `add_setter(&widget, "visible"/"css-classes"/…)`; para clases, un único `adw::Breakpoint` + `connect_apply`/`connect_unapply` que añade/quita `correo-compact` en la ventana y el CSS usa `window.correo-compact .correo-snippet { … }`.) Altura baja (< 500px): `ToolbarView` con `top-bar-style: raised` sin cambios.

Otros: el redactor sigue las mismas reglas (el `Dialog` de libadwaita pasa a bottom sheet en móvil). Tamaños de objetivo táctil >= 32px (botones de barra 34px) y de fila >= 56px. Navegación por teclado: `Ctrl+N` redactar, `R`/`Shift+R` responder, `Supr` borrar, `E` archivar, `Ctrl+F` buscar, `F5` sincronizar; documentadas en un `adw::ShortcutsDialog` (1.8). Texto de tamaño grande: todo en `rem`/`em`, filas con `min-height` (no `height`) para crecer con la escala.

## 9. Cambios de código priorizados (fase 2)

Leyenda: **[A]** alto impacto visual y bajo riesgo (empezar por aquí); **[M]** medio; **[B]** bajo o con riesgo.

### 9.0 Infraestructura (previa, [A])
1. **`crates/correo-app/src/main.rs` / `window.rs::present`**: cargar `data/style.css` con `gtk::CssProvider::load_from_string(include_str!("../../../data/style.css"))` en `connect_startup` (no en `activate`, que puede repetirse) con `STYLE_PROVIDER_PRIORITY_APPLICATION`. Añadir también el fichero al `Makefile`/Flatpak (no hace falta si se incrusta con `include_str!`; preferible).
2. **Nuevo `crates/correo-app/src/ui_style.rs`** (módulo pequeño): `account_class(index) -> &'static str` (`correo-acct-1..8`), `sender_class(email) -> &'static str` (hash estable FNV), `animations_enabled(widget) -> bool` (envuelve `adw::is_animations_enabled`) y `fade_in(widget)` (añade `correo-fade-in`). Sin dependencias nuevas.
3. **`Cargo.toml` de `correo-app`**: confirmar que `libadwaita` tiene `features = ["v1_7"]` (para `WrapBox`, `Spinner` es 1.6, `ShortcutsDialog` requiere `v1_8`, `Dialog` 1.5). Con libadwaita 1.9 instalada se puede subir a `v1_8`/`v1_9`; si el Flatpak usa un runtime anterior, comprobar el runtime GNOME 49/50.

### 9.1 `message_list.rs` (mayor impacto)
1. **[A]** Añadir `list.add_css_class("correo-thread-list")` al `ListView` (`view`) y `correo-thread-row` a `build_row`; la clase `correo-unread` se alterna en `bind_row` según `row.unread > 0` (sustituye `emphasis=["heading"]`) y se ajustan clases `correo-sender`/`correo-subject`/`correo-snippet`/`correo-date`/`correo-count`.
2. **[A]** Avatar: añadir a `RowView` un `adw::Avatar` (size 40) como primer hijo de `row`; en `bind_row`, `avatar.set_text(Some(&from))`, `set_show_initials(true)` y la clase `correo-acct-N` (`ui_style::sender_class`), quitando la anterior en cada bind (reutilización de filas). Cuidado: `row_view` recorre por orden de hijos: actualizar la función (o, mejor, guardar un `struct RowWidgets` con `glib::object::ObjectExt::set_data`/`BoxedAnyObject` para no depender del orden).
3. **[A]** Punto de no leído: `gtk::Box` de 8px con `correo-unread-dot` en una columna fija de 12px antes del avatar o en overlay; siempre presente (no cambia la altura).
4. **[A]** Reorganizar la fila a 3 líneas: arriba remitente + fecha (`hexpand` en remitente, fecha a la derecha, `correo-date`); en medio asunto + clip + contador; abajo extracto. Márgenes 10/10/10/8.
5. **[A]** `StatusPage` vacía: `add_css_class("correo-empty")`, icono `mail-read-symbolic`, título «Todo al día», descripción según carpeta.
6. **[M]** `Stack` con tercer hijo `skeleton` (6 filas `correo-skeleton`); mostrar si `rows.is_empty()` y hay sincronización en curso; `set_transition_type(Crossfade)`, 160ms.
7. **[M]** Animación de entrada de filas nuevas (sync): marcar IDs nuevos en `set_rows` y aplicar `correo-row-enter(-active)` en `bind_row` solo a esas.
8. **[B]** Cabeceras de sección por fecha (`ListHeaderFactory` + `SectionModel`); retrasar a fase 3.

### 9.2 `sidebar.rs`
1. **[A]** `list.add_css_class("correo-sidebar")`; `header_row` pasa a `Box` con `adw::Avatar` 24px (`correo-acct-N`) + email (`heading`, ellipsize) y clase `correo-account-header` en la fila; texto de la fila unificada «Entradas unificadas».
2. **[A]** `target_row`: sustituir `Label` `numeric accent` por `correo-badge` (+ `correo-badge-accent` solo para Entrada y Borradores); icono `mail-inbox-symbolic` para Entrada.
3. **[A]** Menú contextual con `gio::Menu` + `gtk::PopoverMenu` («Sincronizar ahora», «Eliminar cuenta…»); elimina el botón rojo suelto. Acciones con `gio::SimpleAction`.
4. **[M]** Mover «Añadir cuenta» a la parte inferior de la lista y poner el menú principal en la cabecera.
5. **[B]** Cuentas plegables con `Revealer`.

### 9.3 `window.rs`
1. **[A]** Cabecera de la lista: `SearchEntry` con `correo-search` dentro de `adw::Clamp(maximum_size=320)`; `Redactar` con icono + `correo-compose-button`; `sync_btn` con `correo-sync` y `correo-syncing` mientras corre `sync()` (añadir/quitar la clase en inicio/fin).
2. **[A]** Estado vacío y «Selecciona un mensaje»: `correo-empty`, textos nuevos de §6.7, botón `pill suggested-action`.
3. **[A]** Anchos de `NavigationSplitView` (`max_sidebar_width`, `sidebar_width_fraction`) y tamaño por defecto 1180x760.
4. **[M]** Breakpoint `max-width: 560sp` que añada/quite `correo-compact` en la ventana (extracto oculto, `Redactar` solo icono).
5. **[M]** Toasts con `Deshacer` para archivar/borrar (`adw::Toast::set_button_label` + `connect_button_clicked`), y banner de red.
6. **[M]** `adw::ShortcutsDialog` y menú principal (`MenuButton` con `open-menu-symbolic`): Preferencias, Atajos, Acerca de (`adw::AboutDialog`).

### 9.4 `reader.rs`
1. **[A]** Subject: sustituir `title-2` por `correo-reader-subject`; metas: de `caption` a `correo-reader-meta`; fecha `correo-reader-date`.
2. **[A]** Cabecera con `adw::Avatar` 48px + remitente en dos líneas (`from` negrita, `para: …` secundaria) y fecha a la derecha; Para/Cc plegados en `Revealer` con botón «Detalles».
3. **[A]** Acciones: pasar de seis botones con etiqueta/icono a iconos solo (`.flat`) en un `box.linked.correo-actions` con tooltips con atajo; Borrar con `correo-danger`; mover «Marcar no leído» y «Vista completa» a `MenuButton`. Poner el grupo en la `HeaderBar` del lector (requiere exponer un `widget` desde `MessageView` o poner el grupo como `pack_end` del header en `window.rs`).
4. **[A]** `adw::Clamp` (760/560) alrededor del contenido, `correo-reader-body` en el `TextView` y márgenes 24.
5. **[A]** Estado de error: título/descr humanas, detalle técnico en `Expander` con `monospace caption`; `correo-empty`/`correo-error`.
6. **[M]** Esqueleto de lector tras 250ms de carga (sustituye el spinner); `Stack` con `Crossfade` 160ms.
7. **[M]** Adjuntos en `adw::WrapBox` con tarjetas `correo-attachment` (reemplaza `boxed-list`).
8. **[M]** Citas plegables (`TextTag` `invisible` + `TextChildAnchor` con botón `correo-quote-toggle`); empezar sin animación.
9. **[B]** Cabecera sticky al desplazar.

### 9.5 `composer.rs`, `composer_recipients.rs`, `composer_editor.rs`, `composer_attach.rs`
1. **[A]** `labeled_row`: filas `correo-compose-row` con `correo-compose-label`; entradas `correo-compose-entry`; asunto `correo-compose-subject`; `form.add_css_class("correo-compose-form")`. Separadores entre filas por CSS.
2. **[A]** Chip (`add_chip`): quitar `card`, usar `correo-chip`; añadir `adw::Avatar` 20px con iniciales antes de la etiqueta; entrada con `correo-chip-enter` → `-active` (`glib::idle_add_local_once`); salida con `correo-chip-leave` y retirada tras 110ms.
3. **[A]** Campo Para: `adw::WrapBox` en lugar de `FlowBox`, `hexpand`, `Cc`/`Cco` como botones `flat` pequeños al final de la fila de Para.
4. **[A]** Barra de formato: `box.correo-format-bar`, botones `ToggleButton` (`flat`) con `checked` sincronizado al cursor, separadores entre grupos; botón de adjuntar al final de la barra (mueve `attach_bar`).
5. **[A]** Cabecera del redactor: `Enviar` con icono + texto; `Descartar` a la izquierda; `adw::Spinner` en lugar de `gtk::Spinner`.
6. **[M]** Selector De con avatar (factory de `DropDown`).
7. **[M]** Cc/Cco en `Revealer(SlideDown)` 160ms.
8. **[M]** Adjuntos en `WrapBox` con `correo-attach-chip` y entrada/salida; `correo-drop-hint` al arrastrar.
9. **[B]** Migrar la ventana a `adw::Dialog`.

### 9.6 `account_dialog.rs`
1. **[M]** Reestructurar como `NavigationView` de 3 páginas (proveedor, esperando navegador, éxito) con `ActionRow`s de proveedor; errores con `Banner`.
2. **[A]** Cambios menores: `adw::Spinner`, botones `pill`, textos.

### 9.7 `setup.rs` / `background.rs` / `notify.rs`
1. **[B]** Sin cambios visuales; `notify.rs`: icono de la app y `Priority::Normal`, agrupar por cuenta con `set_category("email.arrived")`. `background.rs`: exponer eventos de «sincronizando» para que `window.rs` alterne `correo-syncing`.

### 9.8 Orden recomendado de implementación
1. Infraestructura (9.0) + lista de hilos (9.1.1-5) + barra lateral (9.2.1-2): el aspecto cambia por completo con poco riesgo.
2. Lector (9.4.1-5) y cabecera/vacíos (9.3.1-3).
3. Redactor (9.5.1-5).
4. Animaciones de entrada/salida, esqueletos, Deshacer, atajos (los **[M]**).
5. Secciones por fecha, cuentas plegables, citas animadas, `Dialog` para el redactor (los **[B]**).

Verificación en cada paso: capturas con `snap3.sh` en claro/oscuro y 560px; `GTK_DEBUG=interactive` para inspeccionar clases; `cargo clippy`; comprobar rendimiento (la lista de 5000 hilos no debe crear widgets nuevos al desplazar) y `GTK_ENABLE_ANIMATIONS=0`/`gtk-enable-animations=false` para el modo sin animaciones.

## 10. Estado de implementación (fase 2)

Hecho (todos los [A] y los [M] de bajo riesgo):

- `data/style.css` se carga en `startup` (`ui_style::load_css`, incrustado con `include_str!`; en depuración se imprimen los errores de parseo del CSS). `ui_style.rs` aporta colores por cuenta y remitente, `icon_or`, `animations_enabled` y `enter` (entrada de chips por clases CSS).
- Lista de hilos: avatar `adw::Avatar` de 40 px con color por remitente, punto de no leído, fila en 3 líneas, chip de etiqueta, contador con tooltip accesible, estado vacío («Todo al día» / «Sin resultados»), fundido de 160 ms.
- Barra lateral: cabeceras de cuenta con avatar de su color y botón de opciones (teclado/táctil), badges `correo-badge`, menú principal (Acerca de, Salir), menú contextual de eliminar cuenta restilizado.
- Ventana: buscador en `adw::Clamp`, `Redactar` con icono (solo icono a <= 560sp), `correo-syncing` en el botón de sincronizar solo mientras sincroniza, tres breakpoints (1000/720/560sp; libadwaita aplica solo el último que se cumple, así que cada uno repite los ajustes anteriores), anchos de panel, estados vacíos nuevos.
- Lector: `adw::Clamp` 780, cabecera con avatar de 44 px, asunto, remitente (nombre y email), destinatarios abreviados con tooltip completo, acciones solo-icono en la cabecera de la ventana (`linked` para responder, archivar, borrar y menú «Más» con «Marcar como no leído» y «Vista completa» / «Vista ligera»), esqueleto de carga, error con detalle plegado.
- Redactor: filas planas con etiqueta, `adw::WrapBox` para destinatarios, chips con avatar y animación de entrada/salida, Cc/Cco en `Revealer`, barra de formato con separadores y etiquetas accesibles, `Enviar` con icono, `adw::Spinner`.
- Datos de demostración variados (8 remitentes, adjuntos, etiquetas, no leídos, fechas dispersas); `CORREO_DEMO_EMPTY=1` arranca sin cuentas.

Pendiente (marcados [B] o fuera del riesgo asumido), como TODO:

- TODO: cabeceras de grupo por fecha en la lista (`ListHeaderFactory` + modelo seccionado).
- TODO: citas plegables en el lector (`TextTag` `invisible` + `TextChildAnchor` con botón `correo-quote-toggle`).
- TODO: adjuntos del lector como tarjetas en `adw::WrapBox` (hoy: `boxed-list` con icono y botones con etiqueta accesible).
- TODO: estado `checked` de la barra de formato sincronizado con el cursor.
- TODO: selector De con avatar (factory de `DropDown`), diálogo de alta de cuenta con `NavigationView`, redactor como `adw::Dialog`.
- TODO: toasts con «Deshacer» al archivar/borrar (requiere mover de vuelta en el servidor), banner de red, `adw::ShortcutsDialog`.
- Hecho: cuentas plegables en la barra lateral (`Revealer` 200 ms, cabecera enfocable con Enter/Espacio/flechas, insignia de no leídos al plegar, «Plegar/Desplegar todas» en el menú, estado en `ui-state.json` vía `ui_state.rs`).
- TODO: entrada animada de filas nuevas de la lista (las filas se reciclan; se necesita marcar ids nuevos).
- TODO: modo compacto real de la lista (ocultar extracto a <= 560sp): GTK CSS no tiene `display: none`; requiere una propiedad en la fila o un `connect_apply` que recorra las filas.
- Los iconos `mail-inbox-symbolic` y `mail-archive-symbolic` se piden con `ui_style::icon_or` y caen a `mail-unread-symbolic` / `folder-symbolic` si el tema no los trae.
- Nota de avatares: el color de `adw::Avatar` vive en un nodo `avatar` hijo, por lo que las reglas son `.correo-avatar.correo-acct-N avatar`.

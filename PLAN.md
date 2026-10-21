# PLAN: cliente de correo nativo para GNOME

## Decisiones (acordadas)

| Tema | Decisión |
|---|---|
| Lenguaje / UI | Rust + GTK4 + libadwaita (UI con Relm4 si simplifica; si no, gtk4-rs directo) |
| Protocolo | IMAP/SMTP + OAuth2 para Gmail, Outlook.com y Microsoft 365 |
| Contactos | Google People API y Microsoft Graph (solo contactos) |
| Sincronización | Caché SQLite: cabeceras de los últimos N días; cuerpo y adjuntos bajo demanda |
| MVP | Lectura, carpetas/etiquetas, responder/reenviar, editor con formato mínimo, autocompletado, hilos, búsqueda, notificaciones, adjuntos, cuentas unificadas |

## Riesgos conocidos

1. **Microsoft 365 empresarial** puede tener IMAP/SMTP AUTH deshabilitado por el administrador del tenant. Hay que probarlo pronto con la cuenta real. Plan B: backend Graph para correo (tarea T12, post-MVP).
2. **OAuth2**: Google y Microsoft exigen registrar una aplicación (client ID). Eso lo tiene que hacer el usuario. Google marca los scopes de correo como "restringidos"; para uso personal vale el modo "testing" (límite de 100 usuarios, tokens de 7 días en testing; conviene publicar la app como "interna/personal").
3. **Almacenamiento de tokens**: libsecret (Secret Service / GNOME Keyring), nunca en disco en claro.
4. **HTML de correo**: renderizar con WebKitGTK es pesado. Alternativa ligera: sanitizar con `ammonia` y renderizar en un WebView mínimo con JS desactivado y sin recursos remotos por defecto.

## Reutilización (a evaluar en T0, no se clona nada sin revisar licencia)

- Crates: `async-imap` o `imap-next`, `lettre`, `mail-parser`, `mail-builder`, `oauth2`, `rusqlite`/`sqlx`, `ammonia`, `secret-service`/`oo7`, `tokio`.
- Referencia de arquitectura: **Himalaya / pimalaya** (Rust, MIT), **Geary** (Vala, LGPL: UX y modelo de hilos), **Delta Chat** (Rust, IMAP robusto).

## Arquitectura

Workspace Cargo con núcleo separado de la UI, para poder testear sin GTK y delegar piezas independientes:

```
correo/
  Cargo.toml                 # workspace
  crates/
    correo-core/             # modelos: Account, Mailbox, Message, Thread, Contact
    correo-auth/             # OAuth2 (PKCE, loopback), refresco, libsecret
    correo-imap/             # cliente IMAP, sync incremental (UIDVALIDITY/CONDSTORE), IDLE
    correo-smtp/             # envío (XOAUTH2), construcción MIME
    correo-store/            # SQLite: esquema, migraciones, FTS5, caché de cuerpos
    correo-contacts/         # Google People + Graph, caché y búsqueda para autocompletado
    correo-threads/          # agrupación en hilos (References/In-Reply-To + asunto)
    correo-app/              # GTK4/libadwaita: ventanas, listas, editor, notificaciones
  data/                      # .desktop, icono, metainfo, gschema
  docs/
```

Principios de ligereza: carga perezosa, listas virtualizadas (`gtk::ListView`), tokio con pocos hilos, sin descargar cuerpos hasta abrirlos, una conexión IMAP por cuenta (más una para IDLE).

## Tareas y reparto de agentes

Yo (orquestador, Sonnet) defino interfaces (traits y tipos en `correo-core`), reparto, integro y reviso. Los agentes Haiku implementan tareas acotadas con criterios de aceptación y tests. Las tareas de la misma fase sin dependencias entre sí se lanzan en paralelo.

### Fase 0: cimientos (yo)
- **T0** Esqueleto del workspace, CI local (`cargo fmt/clippy/test`), `correo-core` con tipos y traits (`MailBackend`, `ContactSource`, `Store`). Evaluación de crates. Verificar toolchain GTK4/libadwaita instalada.

### Fase 1: núcleo (Haiku, en paralelo)
- **T1** `correo-store`: esquema SQLite, migraciones, CRUD de cuentas/carpetas/mensajes, FTS5. *Acepta:* tests con BD en memoria.
- **T2** `correo-auth`: flujo OAuth2 PKCE con redirección loopback para Google y Microsoft, refresco de tokens, guardado en libsecret. *Acepta:* tests con servidor OAuth simulado.
- **T3** `correo-threads`: algoritmo de hilos (estilo JWZ). *Acepta:* tests con corpus de ejemplo.
- **T4** `correo-smtp`: construcción MIME (texto + HTML + adjuntos) y envío con XOAUTH2.

### Fase 2: sincronización (Haiku; revisión mía)
- **T5** `correo-imap`: login XOAUTH2, listado de carpetas (con atributos especiales), sync de cabeceras por ventana de N días, fetch de cuerpo/adjuntos bajo demanda. Depende de T1 y T2.
- **T6** Sync incremental y estado: flags, borrados, UIDVALIDITY, IDLE, reconexión con backoff. Depende de T5.
- **T7** Acciones: marcar leído, mover, archivar, borrar, etiquetas Gmail (X-GM-LABELS). Depende de T5.
- **T8** `correo-contacts`: People API y Graph, caché local, búsqueda por prefijo. Depende de T1 y T2.

### Fase 3: interfaz (Haiku por pantallas; yo defino la navegación)
- **T9** Ventana principal: barra lateral de cuentas y carpetas (con bandeja unificada), lista de mensajes virtualizada, vista de hilo. Responsive (`AdwNavigationSplitView`).
- **T10** Lector de mensaje: HTML saneado, imágenes remotas bloqueadas por defecto, adjuntos (abrir/guardar).
- **T11** Compositor: editor con negrita/cursiva/listas/enlaces/firma, adjuntos con arrastrar y soltar, autocompletado de destinatarios, responder/responder a todos/reenviar con cita.
- **T12** Asistente de alta de cuenta (OAuth en navegador) y preferencias mínimas (días a sincronizar, firma).
- **T13** Búsqueda (local FTS5 + búsqueda en servidor) y notificaciones de escritorio.

### Fase 4: pulido
- **T14** Empaquetado (Flatpak o `.deb`), icono, metainfo, accesibilidad, atajos de teclado.
- **T15** (post-MVP) Backend Microsoft Graph para correo si IMAP está bloqueado en el tenant; calendario fuera de alcance.

## Notificaciones y consumo de batería (requisito añadido)

- **Notificaciones nativas**: `GNotification` vía `GApplication` (libadwaita), con acciones "Abrir", "Marcar como leído" y "Archivar". Se agrupan por cuenta, no se notifica la carpeta de spam y se respeta No molestar de GNOME. Solo notifican mensajes nuevos de la bandeja de entrada y no leídos.
- **Servicio en segundo plano**: la app puede quedarse residente sin ventana (portal Background de Flatpak / `--gapplication-service`) y arrancar con la sesión, para notificar con la ventana cerrada.
- **Sincronización ligera**:
  - IMAP IDLE sobre la bandeja de entrada (una sola conexión por cuenta). Fuera de IDLE no se hace polling agresivo.
  - Si IDLE no está disponible o la conexión cae: polling con intervalo creciente (2 a 15 min) y reconexión con backoff exponencial más jitter. IDLE se reinicia cada 25 min (límite de los servidores).
  - Las demás carpetas se sincronizan solo al abrirlas o con un barrido lento (cada 30 min) y únicamente las suscritas o visibles.
  - Sin ventana visible y sin cambios: ningún trabajo de CPU ni de red salvo IDLE.
- **Batería y red** (`gio::PowerProfileMonitor` + `gio::NetworkMonitor`):
  - Con ahorro de energía o batería baja: se alargan los intervalos, se pausa el barrido de carpetas secundarias y no se descargan cuerpos por adelantado.
  - Red de pago (`NetworkMonitor::is_network_metered`): no se descargan adjuntos ni imágenes automáticamente.
  - Sin red: se suspende todo y se reanuda al volver (señal `network-changed`), sin reintentos en bucle.
  - Al suspender y reanudar el equipo, resincronización inmediata de la bandeja de entrada.
- **Objetivos medibles** (se verifican en T14): menos de 60 MB de RSS en reposo con 2 cuentas, 0 % de CPU en reposo, y sin despertares periódicos por debajo de 1 por minuto.

Impacto en tareas: **T6** pasa a incluir IDLE, backoff y política de intervalos (módulo `SyncScheduler` testeable con reloj simulado); **T13** incluye las notificaciones y el modo residente; nueva **T13b** integra `PowerProfileMonitor`/`NetworkMonitor` con el planificador; **T14** incluye la medición de consumo (`powertop`/`perf`).

## Control de calidad
- Cada tarea termina con tests y `cargo clippy -D warnings` en verde.
- Haiku trabaja en ramas o worktrees aislados por tarea; yo reviso el diff antes de fusionar.
- Prueba de integración manual con cuentas reales (Gmail, Outlook.com, M365) al final de las fases 2 y 3.

## Qué necesito del usuario
1. Aprobación de este plan.
2. Registrar las apps OAuth (Google Cloud Console y Azure/Entra) y darme los client IDs. Te daré una guía paso a paso.
3. Confirmar si el tenant de M365 permite IMAP/SMTP AUTH (lo comprobamos con una cuenta de prueba en la Fase 2).
4. Confirmar que la máquina de desarrollo tiene `libgtk-4-dev`, `libadwaita-1-dev` y Rust estable (lo verifico en T0).

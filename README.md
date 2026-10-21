# Correo GNOME

Cliente de correo nativo para GNOME (Rust, GTK4 y libadwaita), ligero y pensado para el bajo consumo.
Soporta Gmail, Outlook.com y Microsoft 365 con varias cuentas a la vez, contactos para el autocompletado,
notificaciones nativas y sincronización por IMAP IDLE. Solo descarga cabeceras recientes; los cuerpos y
adjuntos se piden al abrirlos.

- Plan y arquitectura: [PLAN.md](PLAN.md)
- Registrar las aplicaciones OAuth: [docs/OAUTH.md](docs/OAUTH.md)
- Empaquetado: [docs/PACKAGING.md](docs/PACKAGING.md)
- Política de privacidad: [PRIVACY.md](PRIVACY.md)

Licencia: GPL-3.0-or-later. Estado: en desarrollo.

## Entorno remoto de Claude

    ./remoto.sh            # nueva sesión Remote Control, visible en claude.ai/code y en la app móvil
    ./remoto.sh --continue # reengancha la última sesión de esta carpeta

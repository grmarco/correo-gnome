# Registrar la aplicación OAuth

Correo se conecta a Gmail, Outlook.com y Microsoft 365 con OAuth2 (flujo de código con PKCE y
redirección loopback). Cada proveedor exige que registres una aplicación propia y te dé un
**client ID**.

El **client secret** es opcional y depende del proveedor:

- **Google (Gmail): normalmente hace falta.** Su token endpoint pide `client_secret` también para
  las aplicaciones de escritorio aunque se use PKCE; sin él el intercambio del código o el
  refresco del token fallan. El secreto de una aplicación de escritorio **no es confidencial**
  (va incrustado en cualquier binario o configuración instalada), así que guardarlo en
  `config.toml` con permisos restrictivos no compromete nada que no esté ya expuesto.
- **Microsoft (Outlook / Microsoft 365): no hace falta.** Se registra como cliente público
  («permitir flujos de cliente público»), sin secreto. Deja el campo vacío.

Después de registrar las apps, configura los identificadores en
`~/.config/correo/config.toml`:

```toml
sync_days = 14            # días de cabeceras a sincronizar al crear una cuenta
run_in_background = false # true: la app sigue activa sin ventanas (notificaciones)

[google]
client_id = "123456789-abc.apps.googleusercontent.com"
client_secret = "GOCSPX-..."   # Google: normalmente necesario (ver arriba)

[microsoft]
client_id = "00000000-0000-0000-0000-000000000000"
# client_secret: no usar (cliente público)
```

También puedes usar las variables de entorno `CORREO_GOOGLE_CLIENT_ID`,
`CORREO_GOOGLE_CLIENT_SECRET`, `CORREO_MICROSOFT_CLIENT_ID` y `CORREO_MICROSOFT_CLIENT_SECRET`;
tienen prioridad sobre el fichero. Un valor vacío equivale a ausente.

Permisos recomendados del fichero, ya que contiene el secreto: `chmod 600 ~/.config/correo/config.toml`.

Los tokens de acceso y de refresco se guardan en el llavero del sistema (GNOME Keyring o
KWallet, vía Secret Service). Si el llavero no está disponible, los tokens solo viven en memoria
durante la sesión y tendrás que volver a iniciar sesión al reiniciar.

---

## 1. Google (Gmail)

Necesitas una cuenta de Google y un proyecto en Google Cloud.

1. Abre <https://console.cloud.google.com/> y crea un proyecto nuevo (por ejemplo, `correo`).
2. **APIs y servicios → Biblioteca**: habilita **Gmail API** y **Google People API**.
3. **APIs y servicios → Pantalla de consentimiento de OAuth** (en versiones nuevas,
   **Google Auth Platform**):
   - Tipo de usuario: **Externo** (para una cuenta personal de Gmail) o **Interno** si usas
     Google Workspace de tu organización.
   - Nombre de la aplicación y correo de contacto.
   - **Ámbitos** (scopes) — añade estos:
     - `https://mail.google.com/` (IMAP y SMTP; Google lo marca como ámbito restringido)
     - `https://www.googleapis.com/auth/contacts.readonly` (contactos, People API)
     - `https://www.googleapis.com/auth/userinfo.email` (saber qué cuenta se ha autorizado)
   - Estado de publicación: mientras la app esté en **Pruebas** solo pueden iniciar sesión los
     **usuarios de prueba** que añadas (hasta 100), y los refresh tokens caducan a los
     **7 días**: tendrás que volver a autorizar cada semana. Para uso personal, lo estable es
     **Publicar la aplicación** (sin verificación, Google mostrará una advertencia de
     aplicación no verificada que puedes aceptar tú mismo).
4. **APIs y servicios → Credenciales → Crear credenciales → ID de cliente de OAuth**:
   - Tipo de aplicación: **Aplicación de escritorio**.
   - Nombre: `correo (escritorio)`.
   - Copia el **ID de cliente** (termina en `.apps.googleusercontent.com`) y también el
     **secreto de cliente** (empieza por `GOCSPX-`). Como es una aplicación de escritorio, el
     secreto no es confidencial; Google lo necesita en la práctica para el intercambio de tokens.
5. Pon el ID en `[google] client_id` y el secreto en `[google] client_secret` de `config.toml`
   (o en `CORREO_GOOGLE_CLIENT_SECRET`).

Notas:
- No hay que registrar URIs de redirección: los clientes de escritorio aceptan
  `http://127.0.0.1:<puerto>/` con puerto aleatorio.
- IMAP de Gmail debe estar activado en la cuenta (en Gmail, Ajustes → Reenvío y POP/IMAP).

---

## 2. Microsoft (Outlook.com y Microsoft 365)

Necesitas acceso a <https://entra.microsoft.com/> (o <https://portal.azure.com/>, sección
Microsoft Entra ID).

1. **Registros de aplicaciones → Nuevo registro**:
   - Nombre: `correo`.
   - Tipos de cuenta admitidos: **Cuentas en cualquier directorio de Microsoft Entra y
     cuentas personales de Microsoft** (equivale a `common`, sirve para Outlook.com y para
     Microsoft 365).
   - URI de redirección: plataforma **Aplicaciones móviles y de escritorio**, valor
     `http://localhost`.
2. En la aplicación creada, **Autenticación**:
   - En **Configuración avanzada**, pon **Permitir flujos de cliente público** en **Sí**
     (cliente público: no hay secreto).
   - Guarda los cambios.
3. **Permisos de API → Agregar un permiso**:
   - **Office 365 Exchange Online** → Permisos delegados → `IMAP.AccessAsUser.All` y
     `SMTP.Send`.
   - **Microsoft Graph** → Permisos delegados → `offline_access`, `Contacts.Read`, `User.Read`.
   - Si el administrador del tenant debe aprobar los permisos, pídele que haga clic en
     **Conceder consentimiento de administrador**. En cuentas personales, cada usuario
     consiente al iniciar sesión.
4. En **Información general** copia el **Id. de aplicación (cliente)**. No hace falta ningún
   secreto de cliente.
5. Pon ese valor en `[microsoft] client_id` de `config.toml`.

### Nota para Microsoft 365 empresarial

Muchos tenants de empresa **bloquean IMAP y SMTP AUTH** por defecto. Si el alta falla con un
error de autenticación al abrir IMAP, o el administrador no te permite consentir los permisos,
el problema está en el tenant, no en la app. Para que funcione, un administrador debe:

- Permitir la autenticación SMTP y el acceso IMAP para tu buzón (en Exchange Online, el
  buzón debe tener IMAP habilitado y la autenticación SMTP de cliente permitida; revisa la
  configuración de autenticación del tenant y las directivas de acceso condicional que
  bloqueen clientes heredados).
- Consentir los permisos de la aplicación para la organización.

Si el tenant no lo permite, la alternativa es el backend de Microsoft Graph para correo
(prevista como tarea posterior al MVP). Comprueba primero con una cuenta de prueba.

---

## Solución de problemas

- **«Falta el client ID»** en el diálogo de alta: el fichero o la variable de entorno no
  contiene el ID del proveedor. Revisa `config.toml` (sección `[google]` o `[microsoft]`).
- **Google responde con `invalid_request` o «client_secret is missing» al autorizar**: falta
  `client_secret` en `[google]` (o `CORREO_GOOGLE_CLIENT_SECRET`). Añádelo y repite el alta.
- **Pantalla «aplicación no verificada» de Google**: es esperable para una app personal; pulsa
  en «Avanzado» y continúa. Solo lo verás si la app no está publicada.
- **«Esperando la autorización» y nada pasa**: el navegador debe poder llegar a `127.0.0.1` en
  el mismo equipo. La autorización caduca a los 5 minutos.
- **Error de consentimiento en Microsoft**: revisa que el tipo de cuenta admitido incluye tu
  tipo de cuenta y que los permisos están añadidos.

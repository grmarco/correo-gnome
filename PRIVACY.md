# Política de privacidad / Privacy policy

**Correo GNOME** es un cliente de correo de escritorio para Linux. Esta política explica qué datos toca la aplicación.
*Correo GNOME is a desktop email client for Linux. This policy explains what data the application handles.*

## Español

- **Todo ocurre en tu equipo.** La aplicación se conecta directamente desde tu ordenador a tus proveedores de correo (Gmail, Outlook, Microsoft 365) mediante IMAP/SMTP y a sus APIs de contactos. No existe ningún servidor del desarrollador entre tú y tu proveedor.
- **No recopilamos tus datos.** El desarrollador no recibe, almacena, vende ni comparte tu correo, tus contactos, tus tokens ni ningún dato de uso. No hay analítica ni telemetría.
- **Datos a los que accede la app**, con tu autorización explícita (OAuth 2.0): tus mensajes y carpetas (para leerlos, enviarlos y gestionarlos), tus contactos (solo lectura, para el autocompletado) y tu dirección de correo y nombre (para identificar la cuenta).
- **Almacenamiento local.** Las cabeceras de los mensajes recientes y los contactos se guardan en una base de datos local en tu equipo (`~/.local/share/correo`). Los tokens de acceso se guardan en el llavero del sistema (GNOME Keyring / Secret Service). Puedes borrar una cuenta desde la aplicación, lo que elimina sus datos y sus tokens locales.
- **Uso de datos de Google.** El uso y la transferencia de la información recibida de las APIs de Google se ajustará a la [Política de datos de usuario de los servicios de API de Google](https://developers.google.com/terms/api-services-user-data-policy), incluidos los requisitos de uso limitado. Los datos de Google solo se usan para ofrecer las funciones de cliente de correo que ves en la aplicación; no se usan para publicidad ni para entrenar modelos, y ninguna persona los lee.
- **Revocar el acceso.** Puedes revocar el acceso en cualquier momento desde la configuración de seguridad de tu cuenta de Google (https://myaccount.google.com/permissions) o de Microsoft (https://account.microsoft.com/privacy/app-access).
- **Contacto.** Abre una incidencia en el repositorio del proyecto.

## English

- **Everything happens on your computer.** The application connects directly from your machine to your mail providers (Gmail, Outlook, Microsoft 365) over IMAP/SMTP and to their contacts APIs. There is no developer-operated server between you and your provider.
- **We do not collect your data.** The developer does not receive, store, sell or share your email, contacts, tokens or any usage data. There is no analytics or telemetry.
- **Data the app accesses**, with your explicit OAuth 2.0 consent: your messages and folders (to read, send and manage them), your contacts (read-only, for autocomplete), and your email address and name (to identify the account).
- **Local storage.** Recent message headers and contacts are stored in a local database on your machine (`~/.local/share/correo`). Access tokens are stored in the system keyring (GNOME Keyring / Secret Service). Removing an account in the app deletes its local data and tokens.
- **Google user data.** The use and transfer of information received from Google APIs adheres to the [Google API Services User Data Policy](https://developers.google.com/terms/api-services-user-data-policy), including the Limited Use requirements. Google user data is used only to provide the email-client features visible in the app; it is not used for advertising or to train models, and no human reads it.
- **Revoking access.** You can revoke access at any time from your Google (https://myaccount.google.com/permissions) or Microsoft (https://account.microsoft.com/privacy/app-access) account settings.
- **Contact.** Open an issue in the project repository.

_Última actualización / Last updated: 2026-10-21_

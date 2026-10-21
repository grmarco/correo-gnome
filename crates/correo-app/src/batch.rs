//! Lógica pura (sin GTK) de la selección múltiple y las acciones en lote: de posiciones
//! seleccionadas a mensajes agrupados por carpeta, siguiente elemento tras borrar y textos
//! de resumen. La interfaz vive en `window.rs` y `message_list.rs`.

use std::collections::{BTreeMap, HashMap, HashSet};

use correo_core::{MailboxId, MessageHeader, MessageId};

use crate::message_list::ThreadRow;

/// Convierte una secuencia de «¿está seleccionada la fila i?» en rangos `(inicio, longitud)`.
pub fn ranges_from_flags(flags: impl IntoIterator<Item = bool>) -> Vec<(u32, u32)> {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    let mut start: Option<u32> = None;
    let mut pos = 0u32;
    for selected in flags {
        match (selected, start) {
            (true, None) => start = Some(pos),
            (false, Some(s)) => {
                ranges.push((s, pos - s));
                start = None;
            }
            _ => {}
        }
        pos += 1;
    }
    if let Some(s) = start {
        ranges.push((s, pos - s));
    }
    ranges
}

/// Posiciones que cubren los rangos `(inicio, longitud)`.
pub fn positions_in(ranges: &[(u32, u32)]) -> impl Iterator<Item = u32> + '_ {
    ranges
        .iter()
        .flat_map(|&(start, len)| start..start.saturating_add(len))
}

/// Mensajes de un hilo que están en la lista cargada (en el orden del hilo).
pub fn thread_messages<'a>(
    row: &ThreadRow,
    by_id: &HashMap<MessageId, &'a MessageHeader>,
) -> Vec<&'a MessageHeader> {
    row.message_ids
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .collect()
}

/// Mensajes de las filas de `ranges`, sin repetidos y en orden de lista. Seleccionar un hilo
/// equivale a seleccionar todos sus mensajes de la carpeta que se muestra.
pub fn selected_messages(
    rows: &[ThreadRow],
    ranges: &[(u32, u32)],
    by_id: &HashMap<MessageId, &MessageHeader>,
) -> Vec<MessageHeader> {
    let mut seen: HashSet<MessageId> = HashSet::new();
    let mut out = Vec::new();
    for pos in positions_in(ranges) {
        let Some(row) = rows.get(pos as usize) else {
            continue;
        };
        for h in thread_messages(row, by_id) {
            if seen.insert(h.id) {
                out.push(h.clone());
            }
        }
    }
    out
}

/// Agrupa mensajes por carpeta, conservando el orden de cada grupo.
pub fn group_by_mailbox(
    headers: impl IntoIterator<Item = MessageHeader>,
) -> BTreeMap<MailboxId, Vec<MessageHeader>> {
    let mut groups: BTreeMap<MailboxId, Vec<MessageHeader>> = BTreeMap::new();
    for h in headers {
        groups.entry(h.mailbox_id).or_default().push(h);
    }
    groups
}

/// UIDs de un grupo de mensajes.
pub fn uids(headers: &[MessageHeader]) -> Vec<u32> {
    headers.iter().map(|h| h.uid).collect()
}

/// Valor de «leído» que aplica el atajo de alternar: si algún mensaje está sin leer, todos
/// pasan a leídos; si todos están leídos, todos pasan a no leídos.
pub fn target_seen(headers: &[MessageHeader]) -> bool {
    headers.iter().any(|h| !h.flags.seen)
}

/// Fila a la que ir tras quitar las filas seleccionadas (posiciones ordenadas, de una lista
/// de `len` filas): la siguiente a la última seleccionada o, si no hay, la anterior a la
/// primera.
pub fn next_position(selected: &[u32], len: u32) -> Option<u32> {
    let (&first, &last) = (selected.first()?, selected.last()?);
    if last + 1 < len {
        Some(last + 1)
    } else {
        first.checked_sub(1)
    }
}

/// ¿Hay que pedir confirmación antes de un borrado definitivo? Siempre al borrar desde la
/// Papelera y, fuera de ella, si afecta a más de 20 mensajes.
pub fn needs_delete_confirmation(permanent: usize, from_trash: bool) -> bool {
    permanent > 0 && (from_trash || permanent > 20)
}

/// UIDs (en la carpeta de `headers`) de los mensajes cuyo `Message-ID` está en `wanted`. Sirve
/// para localizar en la carpeta de destino los mensajes recién movidos y deshacer el movimiento
/// (el UID cambia al mover).
pub fn pick_uids(headers: &[MessageHeader], wanted: &HashSet<String>) -> Vec<u32> {
    headers
        .iter()
        .filter(|h| h.message_id.as_ref().is_some_and(|m| wanted.contains(m)))
        .map(|h| h.uid)
        .collect()
}

/// Frase de resultado de una acción: «Se archivaron 12 de 15; 3 fallaron: …».
pub struct Phrase {
    pub singular: &'static str,
    pub plural: &'static str,
    /// Complemento tras el número, p. ej. « a la papelera».
    pub suffix: String,
}

pub fn summary(phrase: &Phrase, ok: usize, total: usize, error: Option<&str>) -> String {
    let noun = |n: usize| if n == 1 { "mensaje" } else { "mensajes" };
    let verb = |n: usize| {
        if n == 1 {
            phrase.singular
        } else {
            phrase.plural
        }
    };
    if ok >= total {
        return format!(
            "Se {} {total} {}{}",
            verb(total),
            noun(total),
            phrase.suffix
        );
    }
    let failed = total - ok;
    let failed_verb = if failed == 1 { "falló" } else { "fallaron" };
    let reason = error.map(|e| format!(": {e}")).unwrap_or_default();
    format!(
        "Se {} {ok} de {total}{}; {failed} {failed_verb}{reason}",
        phrase.plural, phrase.suffix
    )
}

/// Texto de «N seleccionados» para el contador y el anuncio accesible.
pub fn count_label(n: usize) -> String {
    match n {
        0 => "0 seleccionados".to_string(),
        1 => "1 seleccionado".to_string(),
        n => format!("{n} seleccionados"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use correo_core::Flags;

    fn header(id: MessageId, mailbox_id: MailboxId, uid: u32, seen: bool) -> MessageHeader {
        MessageHeader {
            id,
            mailbox_id,
            uid,
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            subject: String::new(),
            from: Vec::new(),
            to: Vec::new(),
            cc: Vec::new(),
            date: 0,
            flags: Flags {
                seen,
                ..Flags::default()
            },
            has_attachments: false,
            snippet: None,
            labels: Vec::new(),
        }
    }

    fn row(last: &MessageHeader, ids: &[MessageId]) -> ThreadRow {
        ThreadRow {
            last: last.clone(),
            subject: String::new(),
            count: ids.len(),
            unread: 0,
            has_attachments: false,
            message_ids: ids.to_vec(),
        }
    }

    #[test]
    fn ranges_from_flags_groups_runs() {
        assert!(ranges_from_flags([]).is_empty());
        assert!(ranges_from_flags([false, false]).is_empty());
        assert_eq!(
            ranges_from_flags([true, true, false, true, false, false, true]),
            vec![(0, 2), (3, 1), (6, 1)]
        );
        assert_eq!(ranges_from_flags([true; 5]), vec![(0, 5)]);
    }

    #[test]
    fn positions_expand_ranges() {
        let r = [(0, 2), (5, 3)];
        assert_eq!(positions_in(&r).collect::<Vec<_>>(), vec![0, 1, 5, 6, 7]);
        assert_eq!(positions_in(&[]).count(), 0);
    }

    #[test]
    fn thread_selection_resolves_all_messages_grouped_by_folder() {
        // Mensajes 1,2,3 (carpeta 10) forman el hilo A; 4 (carpeta 20) es el hilo B;
        // 5 (carpeta 10) el hilo C.
        let hs = [
            header(1, 10, 101, true),
            header(2, 10, 102, false),
            header(3, 10, 103, true),
            header(4, 20, 7, true),
            header(5, 10, 105, true),
        ];
        let by_id: HashMap<_, _> = hs.iter().map(|h| (h.id, h)).collect();
        let rows = vec![
            row(&hs[2], &[1, 2, 3]),
            row(&hs[3], &[4]),
            row(&hs[4], &[5]),
        ];
        // Se seleccionan las filas 0 y 1.
        let got = selected_messages(&rows, &[(0, 2)], &by_id);
        assert_eq!(
            got.iter().map(|h| h.id).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        let groups = group_by_mailbox(got);
        assert_eq!(groups.len(), 2);
        assert_eq!(uids(&groups[&10]), vec![101, 102, 103]);
        assert_eq!(uids(&groups[&20]), vec![7]);
    }

    #[test]
    fn selecting_everything_scales_linearly() {
        // Ctrl+A sobre miles de hilos: una sola pasada, sin comparaciones cuadráticas.
        let hs: Vec<MessageHeader> = (1..=30_000)
            .map(|i| header(i, i % 3 + 1, i as u32, i % 2 == 0))
            .collect();
        let by_id: HashMap<_, _> = hs.iter().map(|h| (h.id, h)).collect();
        let rows: Vec<ThreadRow> = hs.iter().map(|h| row(h, &[h.id])).collect();
        let got = selected_messages(&rows, &[(0, rows.len() as u32)], &by_id);
        assert_eq!(got.len(), 30_000);
        let groups = group_by_mailbox(got);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups.values().map(Vec::len).sum::<usize>(), 30_000);
    }

    #[test]
    fn selected_messages_skips_duplicates_and_unknown_ids() {
        let hs = [header(1, 10, 1, true)];
        let by_id: HashMap<_, _> = hs.iter().map(|h| (h.id, h)).collect();
        // Dos filas apuntan al mismo mensaje y una a un id que ya no está cargado.
        let rows = vec![row(&hs[0], &[1, 99]), row(&hs[0], &[1])];
        let got = selected_messages(&rows, &[(0, 2), (5, 1)], &by_id);
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn toggle_read_marks_all_read_unless_all_are_read() {
        let mixed = [header(1, 1, 1, true), header(2, 1, 2, false)];
        assert!(target_seen(&mixed));
        let read = [header(1, 1, 1, true), header(2, 1, 2, true)];
        assert!(!target_seen(&read));
    }

    #[test]
    fn next_position_prefers_the_following_row() {
        assert_eq!(next_position(&[], 5), None);
        assert_eq!(next_position(&[2], 5), Some(3));
        assert_eq!(next_position(&[1, 2, 3], 10), Some(4));
        // Al final de la lista se retrocede.
        assert_eq!(next_position(&[3, 4], 5), Some(2));
        assert_eq!(next_position(&[0, 1, 2], 3), None);
    }

    #[test]
    fn delete_confirmation_rules() {
        assert!(!needs_delete_confirmation(0, true));
        assert!(!needs_delete_confirmation(5, false));
        assert!(!needs_delete_confirmation(20, false));
        assert!(needs_delete_confirmation(21, false));
        assert!(needs_delete_confirmation(1, true));
    }

    #[test]
    fn summaries_report_partial_failures() {
        let archived = Phrase {
            singular: "archivó",
            plural: "archivaron",
            suffix: String::new(),
        };
        assert_eq!(summary(&archived, 1, 1, None), "Se archivó 1 mensaje");
        assert_eq!(
            summary(&archived, 15, 15, None),
            "Se archivaron 15 mensajes"
        );
        assert_eq!(
            summary(&archived, 12, 15, Some("red: corte")),
            "Se archivaron 12 de 15; 3 fallaron: red: corte"
        );
        assert_eq!(
            summary(&archived, 14, 15, None),
            "Se archivaron 14 de 15; 1 falló"
        );
        let trashed = Phrase {
            singular: "movió",
            plural: "movieron",
            suffix: " a la papelera".into(),
        };
        assert_eq!(
            summary(&trashed, 3, 3, None),
            "Se movieron 3 mensajes a la papelera"
        );
    }

    #[test]
    fn pick_uids_matches_by_message_id() {
        let mut a = header(1, 5, 40, true);
        a.message_id = Some("<a@x>".into());
        let mut b = header(2, 5, 41, true);
        b.message_id = Some("<b@x>".into());
        let c = header(3, 5, 42, true);
        let wanted: HashSet<String> = ["<b@x>".to_string(), "<z@x>".to_string()].into();
        assert_eq!(pick_uids(&[a, b, c], &wanted), vec![41]);
    }

    #[test]
    fn count_label_is_pluralised() {
        assert_eq!(count_label(0), "0 seleccionados");
        assert_eq!(count_label(1), "1 seleccionado");
        assert_eq!(count_label(3), "3 seleccionados");
    }
}

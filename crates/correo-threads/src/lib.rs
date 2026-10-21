//! Agrupación de cabeceras de correo en hilos (estilo JWZ).
//!
//! 1. Enlaza mensajes por `Message-ID`, `In-Reply-To` y `References`. Las
//!    referencias a mensajes ausentes crean nodos "fantasma" que mantienen
//!    unido el hilo pero no se devuelven.
//! 2. Los mensajes sin ninguna cabecera de enlace que quedan solos se agrupan
//!    con el hilo cuyo asunto normalizado coincide.
//!
//! Coste aproximado O(n): tablas hash para los identificadores y recorrido
//! iterativo del bosque. La comprobación de ciclos sólo sube por los
//! ascendientes cuando el nodo candidato ya tiene hijos.

use std::collections::{HashMap, HashSet, hash_map::Entry};

use correo_core::{Address, MessageHeader, MessageId};

/// Hilo de conversación construido a partir de cabeceras.
#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    /// Asunto normalizado del mensaje más antiguo (sin Re:/Fwd:…).
    pub root_subject: String,
    /// Mensajes del hilo en orden cronológico.
    pub message_ids: Vec<MessageId>,
    /// Fecha (Unix, segundos) del mensaje más reciente.
    pub last_date: i64,
    /// Mensajes no leídos del hilo.
    pub unread: usize,
    /// Remitentes y destinatarios únicos por email, en orden de aparición.
    pub participants: Vec<Address>,
}

/// Agrupa las cabeceras en hilos, ordenados por `last_date` descendente.
pub fn build_threads(headers: &[MessageHeader]) -> Vec<Thread> {
    let n = headers.len();
    let mut forest = Forest::with_capacity(n);
    let mut ids: HashMap<&str, usize> = HashMap::with_capacity(n);
    let mut node_of = Vec::with_capacity(n);
    let mut is_dup = vec![false; n];

    // Pasada 1: un nodo por cabecera. El primero con un Message-ID ocupa su
    // entrada; los duplicados cuelgan de él para quedar en el mismo hilo.
    for (i, h) in headers.iter().enumerate() {
        let node = forest.add(Some(i));
        if let Some(mid) = h.message_id.as_deref().and_then(clean_id) {
            match ids.entry(mid) {
                Entry::Occupied(e) => {
                    is_dup[i] = true;
                    forest.attach(node, *e.get());
                }
                Entry::Vacant(e) => {
                    e.insert(node);
                }
            }
        }
        node_of.push(node);
    }

    // Pasada 2: enlazar la cadena de referencias y, al final, el propio mensaje.
    for (i, h) in headers.iter().enumerate() {
        if is_dup[i] {
            continue;
        }
        let node = node_of[i];
        let own = h.message_id.as_deref().and_then(clean_id);

        let mut chain: Vec<&str> = h
            .references
            .iter()
            .filter_map(|r| clean_id(r))
            .filter(|r| Some(*r) != own)
            .collect();
        if let Some(irt) = h.in_reply_to.as_deref().and_then(clean_id)
            && Some(irt) != own
            && !chain.contains(&irt)
        {
            chain.push(irt);
        }

        let mut prev: Option<usize> = None;
        for r in chain {
            let c = match ids.entry(r) {
                Entry::Occupied(e) => *e.get(),
                Entry::Vacant(e) => *e.insert(forest.add(None)),
            };
            if let Some(p) = prev
                && forest.nodes[c].parent.is_none()
            {
                forest.attach(c, p);
            }
            prev = Some(c);
        }
        // El último elemento de la cadena es el padre directo del mensaje.
        if let Some(p) = prev {
            forest.attach(node, p);
        }
    }

    // Raíces del bosque -> grupos de cabeceras (ignorando fantasmas).
    let mut seen = vec![false; forest.nodes.len()];
    let mut stack = Vec::new();
    let mut linked_groups: Vec<Vec<usize>> = Vec::new();
    let mut loose: Vec<usize> = Vec::new();
    for root in 0..forest.nodes.len() {
        if forest.nodes[root].parent.is_some() || seen[root] {
            continue;
        }
        let mut members = Vec::new();
        stack.push(root);
        while let Some(x) = stack.pop() {
            if std::mem::replace(&mut seen[x], true) {
                continue;
            }
            if let Some(h) = forest.nodes[x].header {
                members.push(h);
            }
            stack.extend_from_slice(&forest.nodes[x].children);
        }
        if members.is_empty() {
            continue;
        }
        members.sort_by_key(|&i| (headers[i].date, i));
        // Sólo un mensaje suelto y sin cabeceras de enlace es candidato a
        // agruparse por asunto.
        if members.len() == 1 && !has_link_headers(&headers[members[0]]) {
            loose.push(members[0]);
        } else {
            linked_groups.push(members);
        }
    }

    // Agrupación por asunto: primero los hilos enlazados registran su asunto;
    // después los mensajes sueltos sin cabeceras se unen al primer hilo con
    // el mismo asunto normalizado, o forman uno nuevo.
    let mut final_groups: Vec<Vec<usize>> = Vec::with_capacity(linked_groups.len() + loose.len());
    let mut by_subject: HashMap<String, usize> = HashMap::new();
    for members in linked_groups {
        let key = subject_key(&headers[members[0]]);
        final_groups.push(members);
        if !key.is_empty() {
            by_subject.entry(key).or_insert(final_groups.len() - 1);
        }
    }
    for m in loose {
        let key = subject_key(&headers[m]);
        if key.is_empty() {
            final_groups.push(vec![m]);
            continue;
        }
        match by_subject.entry(key) {
            Entry::Occupied(e) => final_groups[*e.get()].push(m),
            Entry::Vacant(e) => {
                final_groups.push(vec![m]);
                e.insert(final_groups.len() - 1);
            }
        }
    }

    let mut threads: Vec<Thread> = final_groups
        .into_iter()
        .map(|mut members| make_thread(headers, &mut members))
        .collect();
    threads.sort_by_key(|t| std::cmp::Reverse(t.last_date));
    threads
}

/// Normaliza un asunto: quita prefijos de respuesta/reenvío repetidos
/// (`Re:`, `RE[2]:`, `Fwd:`, `Fw:`, `RV:`, `ENV:`, y `AW:`/`SV:`/`Antw:`/`R:`)
/// y colapsa los espacios.
pub fn normalize_subject(subject: &str) -> String {
    let mut s = subject.trim();
    loop {
        let next = strip_prefix_once(s);
        if next.len() == s.len() {
            break;
        }
        s = next;
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const PREFIXES: [&str; 9] = ["re", "fwd", "fw", "rv", "env", "aw", "sv", "antw", "r"];

fn strip_prefix_once(s: &str) -> &str {
    for p in PREFIXES {
        let Some(head) = s.get(..p.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(p) {
            continue;
        }
        let mut rest = &s[p.len()..];
        // Contador opcional, como en "Re[2]:".
        if let Some(r) = rest.strip_prefix('[')
            && let Some(end) = r.find(']')
            && end > 0
            && r[..end].bytes().all(|b| b.is_ascii_digit())
        {
            rest = &r[end + 1..];
        }
        if let Some(r) = rest.strip_prefix(':') {
            return r.trim_start();
        }
    }
    s
}

fn subject_key(h: &MessageHeader) -> String {
    normalize_subject(&h.subject).to_lowercase()
}

/// Identificador sin corchetes ni espacios; vacío se trata como ausente.
fn clean_id(s: &str) -> Option<&str> {
    let t = s
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    (!t.is_empty()).then_some(t)
}

fn has_link_headers(h: &MessageHeader) -> bool {
    h.message_id.as_deref().and_then(clean_id).is_some()
        || h.in_reply_to.as_deref().and_then(clean_id).is_some()
        || h.references.iter().any(|r| clean_id(r).is_some())
}

fn make_thread(headers: &[MessageHeader], members: &mut [usize]) -> Thread {
    members.sort_by_key(|&i| (headers[i].date, i));

    let root_subject = members
        .iter()
        .map(|&i| normalize_subject(&headers[i].subject))
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    let last_date = members.iter().map(|&i| headers[i].date).max().unwrap_or(0);
    let unread = members.iter().filter(|&&i| !headers[i].flags.seen).count();

    let mut seen_emails = HashSet::new();
    let mut participants = Vec::new();
    for &i in members.iter() {
        let h = &headers[i];
        for addr in h.from.iter().chain(&h.to).chain(&h.cc) {
            let key = addr.email.trim().to_lowercase();
            if key.is_empty() || !seen_emails.insert(key) {
                continue;
            }
            participants.push(addr.clone());
        }
    }

    Thread {
        root_subject,
        message_ids: members.iter().map(|&i| headers[i].id).collect(),
        last_date,
        unread,
        participants,
    }
}

/// Nodo del bosque: cabecera (si la hay) y enlaces al padre e hijos.
struct Node {
    header: Option<usize>,
    parent: Option<usize>,
    children: Vec<usize>,
}

struct Forest {
    nodes: Vec<Node>,
}

impl Forest {
    fn with_capacity(n: usize) -> Self {
        Self {
            nodes: Vec::with_capacity(n + n / 4),
        }
    }

    fn add(&mut self, header: Option<usize>) -> usize {
        self.nodes.push(Node {
            header,
            parent: None,
            children: Vec::new(),
        });
        self.nodes.len() - 1
    }

    /// ¿Asignar `parent` a `child` crearía un ciclo? Un nodo sin hijos no
    /// puede ser ascendiente de nadie, así que la subida sólo se hace si es
    /// necesaria.
    fn would_cycle(&self, child: usize, parent: usize) -> bool {
        if child == parent {
            return true;
        }
        if self.nodes[child].children.is_empty() {
            return false;
        }
        let mut cur = Some(parent);
        let mut steps = 0;
        while let Some(c) = cur {
            if c == child {
                return true;
            }
            steps += 1;
            if steps > self.nodes.len() {
                // Sólo ocurre si la estructura ya estuviera corrupta: ser conservador.
                return true;
            }
            cur = self.nodes[c].parent;
        }
        false
    }

    /// Pone `parent` como padre de `child`, reemplazando el anterior, salvo
    /// que se cree un ciclo.
    fn attach(&mut self, child: usize, parent: usize) {
        if self.nodes[child].parent == Some(parent) || self.would_cycle(child, parent) {
            return;
        }
        if let Some(old) = self.nodes[child].parent {
            self.nodes[old].children.retain(|&c| c != child);
        }
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use correo_core::Flags;

    fn addr(email: &str) -> Address {
        Address {
            name: None,
            email: email.to_string(),
        }
    }

    fn hdr(
        id: i64,
        mid: Option<&str>,
        irt: Option<&str>,
        refs: &[&str],
        subject: &str,
        date: i64,
    ) -> MessageHeader {
        MessageHeader {
            id,
            mailbox_id: 1,
            uid: id as u32,
            message_id: mid.map(String::from),
            in_reply_to: irt.map(String::from),
            references: refs.iter().map(|s| s.to_string()).collect(),
            subject: subject.to_string(),
            from: vec![addr("ana@x.org")],
            to: vec![],
            cc: vec![],
            date,
            flags: Flags::default(),
            has_attachments: false,
            snippet: None,
            labels: vec![],
        }
    }

    #[test]
    fn respuesta_simple() {
        let mut a = hdr(1, Some("<a@x>"), None, &[], "Hola", 100);
        let mut b = hdr(2, Some("<b@x>"), Some("<a@x>"), &["<a@x>"], "Re: Hola", 200);
        a.flags.seen = true;
        b.to = vec![addr("bob@x.org")];
        b.cc = vec![addr("ANA@x.org")];
        let threads = build_threads(&[a, b]);
        assert_eq!(threads.len(), 1);
        let t = &threads[0];
        assert_eq!(t.root_subject, "Hola");
        assert_eq!(t.message_ids, vec![1, 2]);
        assert_eq!(t.last_date, 200);
        assert_eq!(t.unread, 1);
        let emails: Vec<&str> = t.participants.iter().map(|a| a.email.as_str()).collect();
        assert_eq!(emails, vec!["ana@x.org", "bob@x.org"]);
    }

    #[test]
    fn cadena_larga_ordenada_cronologicamente() {
        let mids: Vec<String> = (0..30).map(|i| format!("<c{i}@x>")).collect();
        let mut headers = Vec::new();
        for i in 0..30 {
            let refs: Vec<&str> = mids[..i].iter().map(String::as_str).collect();
            let irt = (i > 0).then(|| mids[i - 1].as_str());
            headers.push(hdr(
                i as i64,
                Some(&mids[i]),
                irt,
                &refs,
                "Cadena",
                i as i64 * 10,
            ));
        }
        headers.reverse();
        let threads = build_threads(&headers);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].message_ids, (0..30).collect::<Vec<i64>>());
        assert_eq!(threads[0].last_date, 290);
    }

    #[test]
    fn padre_ausente_no_rompe_el_hilo() {
        let headers = vec![
            hdr(
                1,
                Some("<b@x>"),
                Some("<padre@x>"),
                &["<padre@x>"],
                "Propuesta",
                100,
            ),
            hdr(
                2,
                Some("<c@x>"),
                Some("<padre@x>"),
                &["<padre@x>"],
                "Re: Propuesta",
                200,
            ),
            hdr(3, Some("<z@x>"), None, &[], "Otra", 50),
        ];
        let threads = build_threads(&headers);
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].message_ids, vec![1, 2]);
        assert_eq!(threads[0].root_subject, "Propuesta");
        assert_eq!(threads[1].message_ids, vec![3]);
    }

    #[test]
    fn asuntos_con_prefijos_se_agrupan() {
        let headers = vec![
            hdr(1, None, None, &[], "Presupuesto 2026", 100),
            hdr(2, None, None, &[], "Re: Presupuesto 2026", 200),
            hdr(3, None, None, &[], "RE: RE:  presupuesto 2026", 300),
            hdr(4, None, None, &[], "Fwd: Fw: Presupuesto 2026", 400),
            hdr(5, None, None, &[], "Otro asunto", 500),
        ];
        let threads = build_threads(&headers);
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].message_ids, vec![5]);
        assert_eq!(threads[1].message_ids, vec![1, 2, 3, 4]);
        assert_eq!(threads[1].root_subject, "Presupuesto 2026");
    }

    #[test]
    fn normaliza_asuntos() {
        assert_eq!(
            normalize_subject("  Fwd:  RE[2]: Hola   mundo "),
            "Hola mundo"
        );
        assert_eq!(normalize_subject("Reunión"), "Reunión");
        assert_eq!(normalize_subject("Realidad: x"), "Realidad: x");
        assert_eq!(normalize_subject("Re:"), "");
    }

    #[test]
    fn ciclo_no_provoca_panico() {
        let headers = vec![
            hdr(1, Some("<a@x>"), None, &["<b@x>"], "Ciclo", 100),
            hdr(2, Some("<b@x>"), None, &["<a@x>"], "Re: Ciclo", 200),
            hdr(3, Some("<s@x>"), None, &["<s@x>"], "Auto", 300),
        ];
        let threads = build_threads(&headers);
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].message_ids, vec![3]);
        assert_eq!(threads[1].message_ids, vec![1, 2]);
    }

    #[test]
    fn duplicados_quedan_en_el_mismo_hilo() {
        let headers = vec![
            hdr(1, Some("<d@x>"), None, &[], "Hola", 100),
            hdr(2, Some("<d@x>"), None, &[], "Hola", 110),
            hdr(3, Some("<r@x>"), Some("<d@x>"), &["<d@x>"], "Re: Hola", 120),
        ];
        let threads = build_threads(&headers);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].message_ids, vec![1, 2, 3]);
    }

    #[test]
    fn mezcla_de_idiomas() {
        let headers = vec![
            hdr(1, Some("<es@x>"), None, &[], "Reunión de presupuesto", 100),
            hdr(
                2,
                Some("<de@x>"),
                Some("<es@x>"),
                &["<es@x>"],
                "AW: Besprechung zum Budget",
                200,
            ),
            hdr(3, None, None, &[], "SV: Reunión de presupuesto", 300),
            hdr(4, None, None, &[], "Präsentation", 50),
        ];
        let threads = build_threads(&headers);
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].message_ids, vec![1, 2, 3]);
        assert_eq!(threads[0].root_subject, "Reunión de presupuesto");
        assert_eq!(threads[1].message_ids, vec![4]);
    }

    #[test]
    fn humo_20k_cabeceras() {
        let n = 20_000usize;
        let mids: Vec<String> = (0..n).map(|i| format!("<s{i}@x>")).collect();
        let mut headers = Vec::with_capacity(n);
        for i in 0..n {
            let (irt, refs): (Option<&str>, Vec<&str>) = if i % 10 == 0 {
                (None, vec![])
            } else {
                let root = i - i % 10;
                let mut refs = Vec::new();
                if root != i - 1 {
                    refs.push(mids[root].as_str());
                }
                refs.push(mids[i - 1].as_str());
                (Some(mids[i - 1].as_str()), refs)
            };
            headers.push(hdr(i as i64, Some(&mids[i]), irt, &refs, "Tema", i as i64));
        }
        let threads = build_threads(&headers);
        assert_eq!(threads.len(), n / 10);
        assert!(threads.iter().all(|t| t.message_ids.len() == 10));
        assert_eq!(threads[0].last_date, n as i64 - 1);
    }
}

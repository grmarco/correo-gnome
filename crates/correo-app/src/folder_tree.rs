//! Árbol de carpetas de una cuenta, construido por prefijos de ruta con el delimitador que
//! declara el servidor (`Mailbox::delimiter`, nunca adivinado). Es puro (sin GTK) para poder
//! probarlo y reutilizarlo (barra lateral, «Mover a…»).
//!
//! * Los padres que no existen como carpeta (o que el servidor marca `\Noselect`, que la
//!   sincronización no guarda) son nodos contenedor: `mailbox == None`.
//! * Una carpeta con hijos que también es seleccionable (p. ej. `Archives` con
//!   `Archives/2014`) es un nodo con `mailbox` y `children`.
//! * Nombre visible: las carpetas con rol usan su nombre fijo en castellano (la primera de
//!   cada rol; si hay otra con el mismo rol conserva su nombre) y el resto, su último segmento
//!   (así `[Gmail]/Enviados` no repite el prefijo).
//! * Orden entre hermanos: roles especiales (Entrada, Borradores, Enviados, Archivo, Spam,
//!   Papelera) primero y luego el resto sin distinguir mayúsculas y con orden natural
//!   (`2` antes que `10`). Un contenedor se ordena por su mejor descendiente.

use std::cmp::Ordering;

use correo_core::{Mailbox, MailboxId, MailboxRole};

/// Nodo del árbol de carpetas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Nombre visible.
    pub name: String,
    /// Ruta normalizada del nodo (segmentos unidos por el delimitador): clave estable para
    /// recordar el plegado.
    pub key: String,
    /// Carpeta de correo, o `None` si es un simple contenedor (no seleccionable).
    pub mailbox: Option<MailboxId>,
    pub role: MailboxRole,
    /// No leídos de la propia carpeta (0 en un contenedor).
    pub unread: u32,
    pub children: Vec<Node>,
    /// Plegado por defecto: contenedores de primer nivel como `[Gmail]`.
    pub default_collapsed: bool,
    /// Rol con el que se ordena (el de la carpeta o, en un contenedor, el de sus hijos).
    rank: u8,
}

impl Node {
    pub fn is_container(&self) -> bool {
        self.mailbox.is_none()
    }

    /// No leídos de este nodo y de todos sus descendientes.
    pub fn total_unread(&self) -> u32 {
        self.children
            .iter()
            .fold(self.unread, |sum, c| sum.saturating_add(c.total_unread()))
    }
}

/// Nombre fijo en castellano de un rol (`None` para `Other`).
pub fn role_name(role: MailboxRole) -> Option<&'static str> {
    match role {
        MailboxRole::Inbox => Some("Entrada"),
        MailboxRole::Drafts => Some("Borradores"),
        MailboxRole::Sent => Some("Enviados"),
        MailboxRole::Archive => Some("Archivo"),
        MailboxRole::Spam => Some("Spam"),
        MailboxRole::Trash => Some("Papelera"),
        MailboxRole::Other => None,
    }
}

/// Posición de un rol en el orden de la barra lateral.
pub fn role_rank(role: MailboxRole) -> u8 {
    match role {
        MailboxRole::Inbox => 0,
        MailboxRole::Drafts => 1,
        MailboxRole::Sent => 2,
        MailboxRole::Archive => 3,
        MailboxRole::Spam => 4,
        MailboxRole::Trash => 5,
        MailboxRole::Other => 6,
    }
}

/// Segmentos de la ruta según el delimitador del servidor; ignora vacíos (delimitador al
/// final o repetido). Sin delimitador, la ruta entera es un único segmento.
fn segments(path: &str, delimiter: Option<char>) -> Vec<String> {
    let segs: Vec<String> = match delimiter {
        Some(d) => path
            .split(d)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        None => vec![path.to_owned()],
    };
    if segs.is_empty() {
        vec![path.to_owned()]
    } else {
        segs
    }
}

fn join(segs: &[String], delimiter: Option<char>) -> String {
    let mut sep = String::new();
    if let Some(d) = delimiter {
        sep.push(d);
    }
    segs.join(&sep)
}

/// Claves de los ancestros de una carpeta, del más alto al más cercano (sin incluirla).
pub fn ancestor_keys(mailbox: &Mailbox) -> Vec<String> {
    let segs = segments(&mailbox.path, mailbox.delimiter);
    (1..segs.len())
        .map(|n| join(&segs[..n], mailbox.delimiter))
        .collect()
}

/// Minúsculas y sin tildes en las vocales (para que «Álbum» ordene junto a «a…»).
fn fold(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            c => c,
        })
        .collect()
}

/// Comparación «natural» sin distinguir mayúsculas: los números se comparan por valor.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (la, lb) = (fold(a), fold(b));
    let (mut ia, mut ib) = (la.chars().peekable(), lb.chars().peekable());
    loop {
        match (ia.peek().copied(), ib.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ia.next_if(char::is_ascii_digit) {
                    na.push(c);
                }
                let mut nb = String::new();
                while let Some(c) = ib.next_if(char::is_ascii_digit) {
                    nb.push(c);
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(ca), Some(cb)) => {
                if ca != cb {
                    return ca.cmp(&cb);
                }
                ia.next();
                ib.next();
            }
        }
    }
}

fn new_node(name: &str, key: String) -> Node {
    Node {
        name: name.to_owned(),
        key,
        mailbox: None,
        role: MailboxRole::Other,
        unread: 0,
        children: Vec::new(),
        default_collapsed: false,
        rank: role_rank(MailboxRole::Other),
    }
}

fn fill(node: &mut Node, m: &Mailbox) {
    node.mailbox = Some(m.id);
    node.role = m.role;
    node.unread = m.unread;
}

fn insert(
    nodes: &mut Vec<Node>,
    segs: &[String],
    delimiter: Option<char>,
    depth: usize,
    m: &Mailbox,
) {
    let seg = &segs[depth];
    let key = join(&segs[..=depth], delimiter);
    let idx = match nodes.iter().position(|n| n.name == *seg) {
        Some(i) => i,
        None => {
            nodes.push(new_node(seg, key));
            nodes.len() - 1
        }
    };
    if depth + 1 < segs.len() {
        insert(&mut nodes[idx].children, segs, delimiter, depth + 1, m);
    } else if nodes[idx].mailbox.is_none() {
        fill(&mut nodes[idx], m);
    } else {
        // Dos carpetas con la misma ruta normalizada (p. ej. «A» y «A/»): no se pierde ninguna.
        let mut extra = new_node(&m.path, format!("{}\u{0}{}", nodes[idx].key, m.id));
        fill(&mut extra, m);
        nodes.push(extra);
    }
}

/// Aplica el nombre de rol a la carpeta principal de cada rol (la menos profunda y, a igual
/// profundidad, la primera por ruta) y calcula el rango de ordenación.
fn label(nodes: &mut [Node], primary: &[(MailboxRole, String)]) {
    for n in nodes.iter_mut() {
        if n.mailbox.is_some() {
            let is_primary = primary.iter().any(|(r, k)| *r == n.role && *k == n.key);
            if is_primary {
                if let Some(name) = role_name(n.role) {
                    n.name = name.to_owned();
                }
                n.rank = role_rank(n.role);
            } else {
                n.rank = role_rank(MailboxRole::Other);
            }
        }
        label(&mut n.children, primary);
        if n.mailbox.is_none() {
            n.rank = n
                .children
                .iter()
                .map(|c| c.rank)
                .min()
                .unwrap_or(role_rank(MailboxRole::Other));
        }
    }
}

fn collect_roles(nodes: &[Node], depth: usize, out: &mut Vec<(MailboxRole, usize, String)>) {
    for n in nodes {
        if n.mailbox.is_some() && n.role != MailboxRole::Other {
            out.push((n.role, depth, n.key.clone()));
        }
        collect_roles(&n.children, depth + 1, out);
    }
}

fn sort(nodes: &mut [Node]) {
    for n in nodes.iter_mut() {
        sort(&mut n.children);
    }
    nodes.sort_by(|a, b| {
        a.rank
            .cmp(&b.rank)
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
}

/// Construye el árbol de carpetas de una cuenta (ver el comentario del módulo).
pub fn folder_tree(mailboxes: &[Mailbox]) -> Vec<Node> {
    let mut roots: Vec<Node> = Vec::new();
    for m in mailboxes {
        let segs = segments(&m.path, m.delimiter);
        insert(&mut roots, &segs, m.delimiter, 0, m);
    }

    let mut roles = Vec::new();
    collect_roles(&roots, 0, &mut roles);
    roles.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)));
    let mut primary: Vec<(MailboxRole, String)> = Vec::new();
    for (role, _, key) in roles {
        if !primary.iter().any(|(r, _)| *r == role) {
            primary.push((role, key));
        }
    }
    label(&mut roots, &primary);
    for n in roots.iter_mut() {
        n.default_collapsed = n.is_container() && n.name.starts_with('[');
    }
    sort(&mut roots);
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mb(id: i64, path: &str, role: MailboxRole, delimiter: Option<char>) -> Mailbox {
        Mailbox {
            id,
            account_id: 1,
            path: path.into(),
            role,
            uid_validity: Some(1),
            unread: 0,
            delimiter,
        }
    }

    fn other(id: i64, path: &str, d: char) -> Mailbox {
        mb(id, path, MailboxRole::Other, Some(d))
    }

    fn names(nodes: &[Node]) -> Vec<&str> {
        nodes.iter().map(|n| n.name.as_str()).collect()
    }

    #[test]
    fn slash_delimiter_builds_levels() {
        let t = folder_tree(&[
            other(1, "Archives/2017", '/'),
            other(2, "Archives", '/'),
            other(3, "Archives/2014", '/'),
        ]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].mailbox, Some(2));
        assert_eq!(names(&t[0].children), ["2014", "2017"]);
        assert_eq!(t[0].children[0].key, "Archives/2014");
    }

    #[test]
    fn dot_delimiter_does_not_split_on_slash() {
        let t = folder_tree(&[
            other(1, "Trabajo.Cliente A/B", '.'),
            other(2, "Trabajo", '.'),
        ]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].children[0].name, "Cliente A/B");
        // Sin delimitador no se adivina jerarquía.
        let flat = folder_tree(&[mb(1, "a.b/c", MailboxRole::Other, None)]);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].name, "a.b/c");
    }

    #[test]
    fn missing_parents_become_containers() {
        let t = folder_tree(&[other(1, "Proyectos/Casa/Obra", '/')]);
        assert!(t[0].is_container());
        assert!(t[0].children[0].is_container());
        assert_eq!(t[0].children[0].children[0].mailbox, Some(1));
        assert_eq!(t[0].children[0].key, "Proyectos/Casa");
        assert!(!t[0].default_collapsed);
    }

    #[test]
    fn noselect_parent_absent_from_the_list_is_a_container() {
        // La sincronización no guarda las carpetas \Noselect: llegan como padre inexistente.
        let t = folder_tree(&[
            mb(1, "[Gmail]/Enviados", MailboxRole::Sent, Some('/')),
            mb(2, "[Gmail]/Papelera", MailboxRole::Trash, Some('/')),
        ]);
        assert_eq!(t.len(), 1);
        assert!(t[0].is_container());
        assert_eq!(t[0].mailbox, None);
        assert_eq!(t[0].total_unread(), 0);
    }

    #[test]
    fn gmail_group_is_collapsed_and_shows_only_leaves() {
        let t = folder_tree(&[
            mb(1, "INBOX", MailboxRole::Inbox, Some('/')),
            mb(2, "[Gmail]/Papelera", MailboxRole::Trash, Some('/')),
            mb(3, "[Gmail]/Borradores", MailboxRole::Drafts, Some('/')),
            mb(4, "[Gmail]/Enviados", MailboxRole::Sent, Some('/')),
            mb(5, "[Gmail]/Destacados", MailboxRole::Other, Some('/')),
            other(6, "Zeta", '/'),
        ]);
        assert_eq!(names(&t), ["Entrada", "[Gmail]", "Zeta"]);
        let g = &t[1];
        assert!(g.default_collapsed);
        assert_eq!(
            names(&g.children),
            ["Borradores", "Enviados", "Papelera", "Destacados"]
        );
        assert!(g.children.iter().all(|c| !c.name.contains("[Gmail]")));
    }

    #[test]
    fn roles_first_then_alphabetical() {
        let t = folder_tree(&[
            other(1, "zebra", '/'),
            mb(2, "Trash", MailboxRole::Trash, Some('/')),
            other(3, "Álbum", '/'),
            mb(4, "Junk", MailboxRole::Spam, Some('/')),
            other(5, "apple", '/'),
            mb(6, "INBOX", MailboxRole::Inbox, Some('/')),
            mb(7, "Sent", MailboxRole::Sent, Some('/')),
            mb(8, "Drafts", MailboxRole::Drafts, Some('/')),
            mb(9, "Archives", MailboxRole::Archive, Some('/')),
        ]);
        assert_eq!(
            names(&t),
            [
                "Entrada",
                "Borradores",
                "Enviados",
                "Archivo",
                "Spam",
                "Papelera",
                "Álbum",
                "apple",
                "zebra"
            ]
        );
    }

    #[test]
    fn duplicate_role_keeps_its_own_name() {
        let t = folder_tree(&[
            mb(1, "Archive", MailboxRole::Archive, Some('/')),
            mb(2, "Old/Archive", MailboxRole::Archive, Some('/')),
        ]);
        assert_eq!(names(&t), ["Archivo", "Old"]);
        assert_eq!(t[1].children[0].name, "Archive");
    }

    #[test]
    fn natural_order_for_numbers_and_years() {
        let t = folder_tree(&[
            other(1, "Años/2017", '/'),
            other(2, "Años/10", '/'),
            other(3, "Años/2014", '/'),
            other(4, "Años/9", '/'),
            other(5, "Años/009b", '/'),
            other(6, "Años/Beta", '/'),
            other(7, "Años/alfa", '/'),
        ]);
        assert_eq!(
            names(&t[0].children),
            ["9", "009b", "10", "2014", "2017", "alfa", "Beta"]
        );
        assert_eq!(natural_cmp("a2", "a10"), Ordering::Less);
        assert_eq!(natural_cmp("A", "a"), Ordering::Less); // desempate estable
        assert_eq!(natural_cmp("x", "X1"), Ordering::Less);
        assert_eq!(
            natural_cmp("99999999999999999999999", "100000000000000000000000"),
            Ordering::Less
        );
    }

    #[test]
    fn selectable_parent_keeps_own_row_and_sums_unread() {
        let mut parent = mb(1, "Archives", MailboxRole::Archive, Some('/'));
        parent.unread = 2;
        let mut child = other(2, "Archives/2014", '/');
        child.unread = 3;
        let t = folder_tree(&[child, parent]);
        assert_eq!(t[0].mailbox, Some(1));
        assert_eq!(t[0].unread, 2);
        assert_eq!(t[0].total_unread(), 5);
        assert_eq!(t[0].children[0].total_unread(), 3);
    }

    #[test]
    fn trailing_delimiter_is_ignored() {
        let t = folder_tree(&[other(1, "Foo/", '/'), other(2, "Foo/Bar/", '/')]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].name, "Foo");
        assert_eq!(t[0].mailbox, Some(1));
        assert_eq!(t[0].children[0].name, "Bar");
        assert_eq!(t[0].children[0].key, "Foo/Bar");
    }

    #[test]
    fn colliding_paths_lose_no_folder() {
        let t = folder_tree(&[other(1, "Foo", '/'), other(2, "Foo/", '/')]);
        let ids: Vec<_> = t.iter().filter_map(|n| n.mailbox).collect();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn depth_of_four_or_more() {
        let t = folder_tree(&[
            other(1, "a.b.c.d.e", '.'),
            other(2, "a.b.c.d", '.'),
            other(3, "a.b.x", '.'),
        ]);
        let d = &t[0].children[0].children;
        assert_eq!(names(d), ["c", "x"]);
        let c = &d[0];
        assert_eq!(c.children[0].mailbox, Some(2));
        assert_eq!(c.children[0].children[0].mailbox, Some(1));
        assert_eq!(c.children[0].children[0].key, "a.b.c.d.e");
    }

    #[test]
    fn ancestors_of_a_folder() {
        let m = other(1, "Proyectos/Casa/Obra", '/');
        assert_eq!(ancestor_keys(&m), ["Proyectos", "Proyectos/Casa"]);
        assert!(ancestor_keys(&other(2, "Solo", '/')).is_empty());
        assert!(ancestor_keys(&mb(3, "a/b", MailboxRole::Other, None)).is_empty());
    }

    #[test]
    fn container_ranks_by_best_descendant() {
        let t = folder_tree(&[
            other(1, "Aaa", '/'),
            mb(2, "Grupo/Enviados", MailboxRole::Sent, Some('/')),
        ]);
        assert_eq!(names(&t), ["Grupo", "Aaa"]);
    }
}

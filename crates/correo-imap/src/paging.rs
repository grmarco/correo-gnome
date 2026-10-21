//! Búsqueda hacia atrás de los UID más recientes y paginación de mensajes anteriores.
//!
//! Nunca se hace `UID SEARCH ALL` en carpetas grandes: se recorren tramos de UID
//! descendentes (`UID SEARCH UID lo:hi`) con un tamaño que se duplica cuando el tramo
//! no basta (UID dispersos tras muchos borrados), hasta reunir los mensajes pedidos o
//! llegar al UID 1. El bucle trabaja sobre `UidSearch`, que en producción es la sesión
//! IMAP y en los tests una fuente falsa.

use std::collections::BTreeSet;

use async_trait::async_trait;
use correo_core::Result;

use crate::parse::probe_range;

/// Tamaño mínimo del primer tramo de UID.
const MIN_STEP: u32 = 256;
/// Tope del tamaño de tramo (evita respuestas enormes con UID muy dispersos).
const MAX_STEP: u32 = 1 << 22;
/// Máximo de búsquedas por llamada: acota el tiempo en carpetas patológicas.
const MAX_PROBES: usize = 40;

/// Origen de UIDs existentes en un rango.
#[async_trait]
pub trait UidSearch: Send {
    /// UIDs que existen en `lo..=hi` (`UID SEARCH UID lo:hi`).
    async fn search(&mut self, lo: u32, hi: u32) -> Result<BTreeSet<u32>>;
}

/// Hasta `want` UIDs existentes, los más altos que sean `< below` (o `< uid_next` si
/// `below` es `None`), en orden descendente.
#[derive(Debug, PartialEq, Eq)]
pub struct Newest {
    pub uids: Vec<u32>,
    /// Se llegó al UID 1: no hay más mensajes por debajo de lo encontrado.
    pub reached_start: bool,
}

pub async fn newest_uids<S: UidSearch>(
    src: &mut S,
    uid_next: u32,
    below: Option<u32>,
    want: usize,
) -> Result<Newest> {
    let top = below
        .map_or(uid_next, |b| b.min(uid_next))
        .saturating_sub(1);
    if top == 0 || want == 0 {
        return Ok(Newest {
            uids: Vec::new(),
            reached_start: top == 0,
        });
    }
    let mut step = u32::try_from(want.saturating_mul(2))
        .unwrap_or(MAX_STEP)
        .clamp(MIN_STEP, MAX_STEP);
    let mut hi = top;
    let mut found: Vec<u32> = Vec::new();
    let mut reached_start = false;
    for _ in 0..MAX_PROBES {
        let (lo, upper) = probe_range(hi, step);
        found.extend(src.search(lo, upper).await?.into_iter().rev());
        if lo <= 1 {
            reached_start = true;
            break;
        }
        if found.len() >= want {
            break;
        }
        hi = lo - 1;
        step = step.saturating_mul(2).min(MAX_STEP);
    }
    // Si se llegó al UID 1 y no sobra nada, no queda nada por debajo de lo que se devuelve.
    let reached_start = reached_start && found.len() <= want;
    found.truncate(want);
    Ok(Newest {
        uids: found,
        reached_start,
    })
}

/// Qué traer en «Cargar mensajes anteriores».
#[derive(Debug, PartialEq, Eq)]
pub struct OlderPlan {
    pub uids: Vec<u32>,
    /// Tras traer `uids` no queda nada más antiguo.
    pub exhausted: bool,
}

/// `cache_min`: UID más bajo de la caché de la carpeta (`None` si está vacía).
pub async fn plan_older<S: UidSearch>(
    src: &mut S,
    uid_next: u32,
    cache_min: Option<u32>,
    count: usize,
) -> Result<OlderPlan> {
    if cache_min.is_some_and(|m| m <= 1) {
        return Ok(OlderPlan {
            uids: Vec::new(),
            exhausted: true,
        });
    }
    let n = newest_uids(src, uid_next, cache_min, count).await?;
    Ok(OlderPlan {
        exhausted: n.reached_start,
        uids: n.uids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use correo_core::Error;

    /// Servidor falso: un conjunto de UIDs y un registro de las búsquedas.
    struct Fake {
        uids: BTreeSet<u32>,
        calls: Vec<(u32, u32)>,
        fail_on: Option<usize>,
    }

    impl Fake {
        fn new(uids: impl IntoIterator<Item = u32>) -> Self {
            Self {
                uids: uids.into_iter().collect(),
                calls: Vec::new(),
                fail_on: None,
            }
        }
    }

    #[async_trait]
    impl UidSearch for Fake {
        async fn search(&mut self, lo: u32, hi: u32) -> Result<BTreeSet<u32>> {
            let n = self.calls.len();
            self.calls.push((lo, hi));
            if self.fail_on == Some(n) {
                return Err(Error::Network("corte".into()));
            }
            Ok(self.uids.range(lo..=hi).copied().collect())
        }
    }

    #[tokio::test]
    async fn old_folder_with_5000_messages_needs_one_small_search() {
        let mut f = Fake::new(1..=5000);
        let n = newest_uids(&mut f, 5001, None, 100).await.unwrap();
        assert_eq!(n.uids, (4901..=5000).rev().collect::<Vec<_>>());
        assert!(!n.reached_start);
        assert_eq!(f.calls, vec![(4745, 5000)], "sin SEARCH ALL");
    }

    #[tokio::test]
    async fn empty_folder_returns_nothing() {
        let mut f = Fake::new([]);
        let n = newest_uids(&mut f, 1, None, 100).await.unwrap();
        assert!(n.uids.is_empty() && n.reached_start);
        assert!(f.calls.is_empty());
        let mut f = Fake::new([]);
        let n = newest_uids(&mut f, 900, None, 100).await.unwrap();
        assert!(n.uids.is_empty() && n.reached_start);
    }

    #[tokio::test]
    async fn sparse_uids_widen_the_range() {
        // 30 mensajes repartidos en UIDs hasta 1_000_000.
        let uids: Vec<u32> = (1..=30).map(|i| i * 33_000).collect();
        let mut f = Fake::new(uids.clone());
        let n = newest_uids(&mut f, 1_000_000, None, 20).await.unwrap();
        assert_eq!(n.uids.len(), 20);
        assert_eq!(
            n.uids[0],
            *uids.iter().rev().find(|&&u| u < 1_000_000).unwrap()
        );
        assert!(f.calls.len() > 1);
        // Los tramos son contiguos y descendentes.
        for w in f.calls.windows(2) {
            assert_eq!(w[1].1, w[0].0 - 1);
        }
    }

    #[tokio::test]
    async fn short_folder_reaches_start() {
        let mut f = Fake::new([3, 4, 9]);
        let n = newest_uids(&mut f, 10, None, 100).await.unwrap();
        assert_eq!(n.uids, vec![9, 4, 3]);
        assert!(n.reached_start);
    }

    #[tokio::test]
    async fn exactly_want_messages_reaching_start_is_exhausted() {
        let mut f = Fake::new(1..=10);
        let n = newest_uids(&mut f, 11, None, 10).await.unwrap();
        assert_eq!(n.uids.len(), 10);
        assert!(n.reached_start);
        // Con una más, no.
        let mut f = Fake::new(1..=11);
        let n = newest_uids(&mut f, 12, None, 10).await.unwrap();
        assert_eq!(n.uids, (2..=11).rev().collect::<Vec<_>>());
        assert!(!n.reached_start);
    }

    #[tokio::test]
    async fn network_errors_propagate() {
        let mut f = Fake::new(1..=100);
        f.fail_on = Some(0);
        assert!(matches!(
            newest_uids(&mut f, 101, None, 10).await,
            Err(Error::Network(_))
        ));
    }

    #[tokio::test]
    async fn older_pages_walk_back_until_exhausted() {
        // 250 mensajes; la caché empieza en el 201.
        let mut f = Fake::new(1..=250);
        let p = plan_older(&mut f, 251, Some(201), 100).await.unwrap();
        assert_eq!(p.uids, (101..=200).rev().collect::<Vec<_>>());
        assert!(!p.exhausted);
        let p = plan_older(&mut f, 251, Some(101), 100).await.unwrap();
        assert_eq!(p.uids, (1..=100).rev().collect::<Vec<_>>());
        assert!(p.exhausted, "la página llega al UID 1");
        let calls = f.calls.len();
        let p = plan_older(&mut f, 251, Some(1), 100).await.unwrap();
        assert!(p.uids.is_empty() && p.exhausted);
        assert_eq!(
            f.calls.len(),
            calls,
            "con el UID 1 en caché no se pregunta al servidor"
        );
    }

    #[tokio::test]
    async fn older_with_gaps_and_empty_cache() {
        let mut f = Fake::new([5, 6, 50, 51]);
        let p = plan_older(&mut f, 52, None, 3).await.unwrap();
        assert_eq!(p.uids, vec![51, 50, 6]);
        assert!(!p.exhausted);
        let p = plan_older(&mut f, 52, Some(6), 3).await.unwrap();
        assert_eq!(p.uids, vec![5]);
        assert!(p.exhausted);
    }

    #[tokio::test]
    async fn pathological_folder_stops_after_the_probe_cap() {
        let mut f = Fake::new([]);
        let n = newest_uids(&mut f, u32::MAX, None, 100).await.unwrap();
        assert!(f.calls.len() <= MAX_PROBES);
        assert!(n.uids.is_empty());
        assert!(
            !n.reached_start,
            "no se llegó al UID 1: no se da por agotada"
        );
    }
}

//! Boucles automatiques (F4) : « 50 km depuis chez moi », plusieurs propositions différentes.
//!
//! Principe :
//! 1. Pour chaque candidate, on choisit une direction et on place 2 à 4 points
//!    de passage sur un cercle qui passe par le départ.
//! 2. On calcule la boucle départ → points → départ avec les réglages choisis ;
//!    les routes déjà empruntées coûtent plus cher, pour éviter les allers-retours.
//! 3. On ajuste la taille du cercle jusqu'à tomber dans la tolérance de distance.
//! 4. On garde les meilleures candidates qui se ressemblent le moins.
//!
//! Les candidates sont calculées en parallèle sur tous les cœurs.

use crate::geo::haversine_m;
use crate::router::{Leg, RouteOptions, Router, Snap};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Rapport longueur par la route / longueur à vol d'oiseau, pour le premier essai.
const DETOUR: f64 = 1.3;
/// Surcoût d'une route déjà empruntée dans la même boucle.
const REUSE_PENALTY: f32 = 4.0;
/// Distance maximale entre un point de passage idéal et la route où on l'accroche (m).
const WAYPOINT_SNAP_M: f64 = 2_500.0;
/// Fenêtre de part et d'autre d'un point de passage pour repérer un crochet (m).
const EXCURSION_WINDOW_M: f64 = 2_000.0;
/// Crochet si le chemin direct fait moins de cette part du trajet réel (0,55 : 1,8 fois plus court).
const EXCURSION_RATIO: f64 = 0.55;
/// Essais d'ajustement de la taille du cercle.
const CALIBRATION_STEPS: usize = 4;
/// Tirages successifs de formes au maximum.
const MAX_ROUNDS: usize = 3;
/// Écart maximal toléré en dernier recours, en multiple de la tolérance (2 × 5 % = 10 %).
const MAX_ERROR_FACTOR: f64 = 2.0;

fn default_tolerance() -> f64 {
    0.05
}
fn default_count() -> usize {
    4
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopRequest {
    pub start: [f64; 2],
    pub distance_m: f64,
    /// Écart accepté sur la distance (0,05 = ± 5 %).
    #[serde(default = "default_tolerance")]
    pub tolerance: f64,
    /// Nombre de propositions voulues.
    #[serde(default = "default_count")]
    pub count: usize,
    /// Change le tirage (« Autres propositions »).
    #[serde(default)]
    pub seed: u64,
    /// Direction préférée (cap en degrés : 0 = nord, 90 = est), sinon toutes directions.
    #[serde(default)]
    pub heading_deg: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopProposal {
    /// Identifiant stable (empreinte des routes empruntées).
    pub id: u64,
    /// Départ, points de passage, retour au départ (accrochés à la route).
    pub waypoints: Vec<[f64; 2]>,
    pub legs: Vec<Leg>,
    pub length_m: f64,
    /// Dénivelé positif estimé (m).
    pub ascent_m: f64,
    /// Part de la boucle parcourue deux fois (0 à 1) : allers-retours.
    pub retrace: f64,
    /// Mètres parcourus par tronçon, triés par tronçon (comparaison entre boucles).
    #[serde(skip)]
    pub footprint: Vec<(u32, f32)>,
    /// Crochets restants (la boucle part et revient presque au même endroit).
    #[serde(skip)]
    pub hooks: usize,
}

/// Nombre de crochets : endroits où les points 2 km avant et 2 km après sont à moins de 1 km.
pub fn count_hooks(legs: &[Leg]) -> usize {
    let coords: Vec<[f64; 2]> = legs.iter().flat_map(|l| l.coords.iter().copied()).collect();
    if coords.len() < 2 {
        return 0;
    }
    let mut cum = Vec::with_capacity(coords.len());
    cum.push(0.0);
    for w in coords.windows(2) {
        cum.push(cum.last().unwrap() + haversine_m(w[0], w[1]));
    }
    let at = |d: f64| coords[cum.partition_point(|&c| c < d).min(coords.len() - 1)];
    let total = *cum.last().unwrap();
    let (mut n, mut d, mut inside) = (0, 2_000.0, false);
    while d < total - 2_000.0 {
        let hook = haversine_m(at(d - 2_000.0), at(d + 2_000.0)) < 1_000.0;
        if hook && !inside {
            n += 1;
        }
        inside = hook;
        d += 100.0;
    }
    n
}

/// Générateur pseudo-aléatoire minimal et reproductible (splitmix64).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Nombre dans [0, 1[.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Point à `dist` mètres de `p` dans la direction `bearing` (degrés, 0 = nord).
fn offset(p: [f64; 2], bearing_deg: f64, dist: f64) -> [f64; 2] {
    let b = bearing_deg.to_radians();
    let dy = dist * b.cos();
    let dx = dist * b.sin();
    [p[0] + dx / (111_320.0 * p[1].to_radians().cos()), p[1] + dy / 110_574.0]
}

/// Part de `a` (en longueur) qui emprunte aussi des routes de `b`.
pub fn overlap(a: &[(u32, f32)], b: &[(u32, f32)]) -> f64 {
    let (mut i, mut j) = (0, 0);
    let (mut shared, total): (f64, f64) = (0.0, a.iter().map(|x| x.1 as f64).sum());
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                shared += a[i].1 as f64;
                i += 1;
                j += 1;
            }
        }
    }
    if total > 0.0 { shared / total } else { 0.0 }
}

/// Paramètres d'une candidate.
#[derive(Debug, Clone, Copy)]
struct Shape {
    heading: f64,
    points: usize,
    clockwise: bool,
    jitter: [f64; 4],
}

/// Calcule une boucle de la forme donnée, en ajustant sa taille à la distance visée.
fn build_candidate(router: &Router, start: &Snap, req: &LoopRequest, opts: &RouteOptions, shape: Shape) -> Option<LoopProposal> {
    let k = shape.points;
    let n = (k + 1) as f64;
    // Périmètre d'un polygone régulier à k + 1 sommets inscrit dans un cercle de rayon R : n · 2R · sin(π / n)
    let perimeter_per_r = n * 2.0 * (std::f64::consts::PI / n).sin();
    let mut radius = req.distance_m / (DETOUR * perimeter_per_r);
    let mut best: Option<LoopProposal> = None;

    for _ in 0..CALIBRATION_STEPS {
        let center = offset(start.point, shape.heading, radius);
        let back = shape.heading + 180.0; // direction du départ vu du centre
        let step = 360.0 / n * if shape.clockwise { 1.0 } else { -1.0 };

        // Points de passage idéaux, accrochés à la route
        let mut snaps = vec![*start];
        for i in 1..=k {
            let angle = back + step * i as f64 + shape.jitter[i - 1];
            let ideal = offset(center, angle, radius);
            let s = router.snap_within(ideal, opts, WAYPOINT_SNAP_M.min(radius * 0.5).max(800.0)).ok()?;
            if snaps.last().is_some_and(|p| haversine_m(p.point, s.point) < 200.0) {
                return best; // deux points confondus : forme dégénérée
            }
            snaps.push(s);
        }
        snaps.push(*start);

        // Étapes, puis suppression des crochets (trois passes au plus). Inutile tant que
        // la boucle est loin de la bonne taille : elle sera recalculée de toute façon.
        let mut legs = route_legs(router, &snaps, opts)?;
        let rough_len: f64 = legs.iter().map(|l| l.length_m).sum();
        let close = (rough_len / req.distance_m - 1.0).abs() < 0.25;
        for _ in 0..if close { 3 } else { 0 } {
            if !straighten(router, &mut snaps, &legs, opts) {
                break;
            }
            legs = route_legs(router, &snaps, opts)?;
        }
        let candidate = finish(router, &snaps, legs);
        let err = (candidate.length_m - req.distance_m).abs();
        let better = best.as_ref().is_none_or(|b| err < (b.length_m - req.distance_m).abs());
        let ratio = req.distance_m / candidate.length_m.max(1.0);
        if better {
            best = Some(candidate);
        }
        if (ratio - 1.0).abs() <= req.tolerance * 0.8 {
            break;
        }
        radius *= ratio.clamp(0.6, 1.6);
    }
    best
}

/// Calcule les étapes successives, en pénalisant les routes déjà prises.
fn route_legs(router: &Router, snaps: &[Snap], opts: &RouteOptions) -> Option<Vec<Leg>> {
    let mut used: HashMap<u32, f32> = HashMap::new();
    let mut legs = Vec::with_capacity(snaps.len());
    for w in snaps.windows(2) {
        let leg = router.route_with(&w[0], &w[1], opts, Some(&used)).ok()?;
        for &(s, _, _) in &leg.segs {
            used.insert(s, REUSE_PENALTY);
        }
        legs.push(leg);
    }
    Some(legs)
}

/// Point situé à `d` mètres du début de la ligne.
fn point_at(coords: &[[f64; 2]], d: f64) -> [f64; 2] {
    let mut acc = 0.0;
    for w in coords.windows(2) {
        let l = haversine_m(w[0], w[1]);
        if acc + l >= d && l > 0.0 {
            let t = (d - acc) / l;
            return [w[0][0] + (w[1][0] - w[0][0]) * t, w[0][1] + (w[1][1] - w[0][1]) * t];
        }
        acc += l;
    }
    *coords.last().unwrap_or(&[0.0, 0.0])
}

/// Supprime les allers-retours aux points de passage (exacts, ou en « crochet »).
///
/// Repère les « crochets » : la boucle part toucher un point de passage posé à l'écart
/// et revient presque au même endroit. On compare le trajet entre 2 km avant et 2 km
/// après le point au chemin direct entre ces deux positions ; si le direct est bien plus
/// court, le point de passage est déplacé sur ce chemin direct. Vrai si un point a bougé.
fn straighten(router: &Router, snaps: &mut [Snap], legs: &[Leg], opts: &RouteOptions) -> bool {
    let mut changed = false;
    for i in 1..snaps.len() - 1 {
        let (lin, lout) = (&legs[i - 1], &legs[i]);

        // 1. Petit aller-retour exact : l'étape suivante repart par le chemin d'arrivée.
        //    On ramène le point de passage au carrefour où les deux chemins se séparent.
        let (a, b) = (&lin.coords, &lout.coords);
        let (mut k, mut spur) = (0usize, 0.0f64);
        while k + 1 < a.len() && k + 1 < b.len() && haversine_m(a[a.len() - 2 - k], b[k + 1]) < 1.0 {
            spur += haversine_m(b[k], b[k + 1]);
            k += 1;
        }
        if spur > 20.0 {
            if let Ok(m) = router.snap_within(b[k], opts, 30.0) {
                let too_close = |p: &Snap| haversine_m(p.point, m.point) < 200.0;
                if !too_close(&snaps[i - 1]) && !too_close(&snaps[i + 1]) {
                    snaps[i] = m;
                    changed = true;
                    continue;
                }
            }
        }

        // 2. Crochet plus large : la boucle part loin toucher le point et revient près du même endroit.
        let w = EXCURSION_WINDOW_M.min(lin.length_m * 0.45).min(lout.length_m * 0.45);
        if w < 300.0 {
            continue;
        }
        let a = point_at(&lin.coords, lin.length_m - w);
        let b = point_at(&lout.coords, w);
        let (Ok(sa), Ok(sb)) = (router.snap_within(a, opts, 50.0), router.snap_within(b, opts, 50.0)) else {
            continue;
        };
        let Ok(direct) = router.route(&sa, &sb, opts) else { continue };
        if direct.length_m >= EXCURSION_RATIO * 2.0 * w {
            continue;
        }
        let mid = point_at(&direct.coords, direct.length_m / 2.0);
        if let Ok(m) = router.snap_within(mid, opts, 50.0) {
            let too_close = |p: &Snap| haversine_m(p.point, m.point) < 200.0;
            if !too_close(&snaps[i - 1]) && !too_close(&snaps[i + 1]) {
                snaps[i] = m;
                changed = true;
            }
        }
    }
    changed
}

fn finish(router: &Router, snaps: &[Snap], legs: Vec<Leg>) -> LoopProposal {
    let length_m: f64 = legs.iter().map(|l| l.length_m).sum();
    let ascent_m: f64 = legs.iter().map(|l| l.ascent_estimate(&router.graph)).sum();
    // Empreinte et allers-retours
    let mut meters: HashMap<u32, (f32, u32)> = HashMap::new();
    let mut repeated = 0.0f64;
    for leg in &legs {
        for &(s, _, m) in &leg.segs {
            let e = meters.entry(s).or_insert((0.0, 0));
            if e.1 > 0 {
                repeated += m as f64;
            }
            e.0 = e.0.max(m);
            e.1 += 1;
        }
    }
    let mut footprint: Vec<(u32, f32)> = meters.into_iter().map(|(s, (m, _))| (s, m)).collect();
    footprint.sort_unstable_by_key(|x| x.0);
    // Limité à 53 bits : l'identifiant reste exact une fois passé en JavaScript
    let id = footprint.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &(s, _)| (h ^ s as u64).wrapping_mul(0x0100_0000_01b3))
        & ((1u64 << 53) - 1);
    let hooks = count_hooks(&legs);
    LoopProposal {
        hooks,
        id,
        waypoints: snaps.iter().map(|s| s.point).collect(),
        legs,
        length_m,
        ascent_m,
        retrace: if length_m > 0.0 { repeated / length_m } else { 0.0 },
        footprint,
    }
}

/// Propose des boucles variées. `excluded` : empreintes des boucles déjà montrées.
pub fn propose_loops(
    router: &Router,
    req: &LoopRequest,
    opts: &RouteOptions,
    excluded: &[Vec<(u32, f32)>],
) -> Result<Vec<LoopProposal>, String> {
    if !(2_000.0..=400_000.0).contains(&req.distance_m) {
        return Err("Distance de boucle à choisir entre 2 et 400 km.".into());
    }
    let start = router.snap(req.start, opts).map_err(|e| e.to_string())?;
    let count = req.count.clamp(1, 8);

    // Formes candidates : directions réparties (ou autour de la direction voulue), 2 à 4 points.
    // Si trop peu tombent à la bonne distance (bord de mer, réseau clairsemé), on refait des tirages.
    let m = (count * 4).max(12);
    let mut rng = Rng(req.seed.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ 0x5EED);
    let in_tolerance = |c: &LoopProposal| (c.length_m - req.distance_m).abs() / req.distance_m <= req.tolerance;
    let mut candidates: Vec<LoopProposal> = Vec::new();
    for _round in 0..MAX_ROUNDS {
        let base = rng.unit() * 360.0;
        let shapes: Vec<Shape> = (0..m)
            .map(|i| {
                let heading = match req.heading_deg {
                    Some(h) => h + (rng.unit() - 0.5) * 100.0,
                    None => base + i as f64 * 360.0 / m as f64 + (rng.unit() - 0.5) * 20.0,
                };
                Shape {
                    heading,
                    points: [3, 2, 4, 3][i % 4],
                    clockwise: rng.unit() < 0.5,
                    jitter: [0; 4].map(|_| (rng.unit() - 0.5) * 30.0),
                }
            })
            .collect();
        candidates.par_extend(shapes.par_iter().filter_map(|&shape| build_candidate(router, &start, req, opts, shape)));
        if candidates.iter().filter(|c| in_tolerance(c)).count() >= count * 2 {
            break;
        }
    }
    // Jamais de boucle franchement hors distance
    candidates.retain(|c| (c.length_m - req.distance_m).abs() / req.distance_m <= MAX_ERROR_FACTOR * req.tolerance);
    if candidates.is_empty() {
        return Err("Aucune boucle trouvée depuis ce point avec ces réglages : essaie une autre distance ou un autre départ.".into());
    }

    // Classement : dans la tolérance d'abord, puis peu d'allers-retours
    let score = |c: &LoopProposal| {
        let err = (c.length_m - req.distance_m).abs() / req.distance_m;
        let out = if err > req.tolerance { 1.0 + err } else { err };
        out + 1.5 * c.retrace + 0.5 * c.hooks as f64
    };
    candidates.sort_by(|a, b| score(a).total_cmp(&score(b)));

    // Sélection des plus différentes
    let mut chosen: Vec<LoopProposal> = Vec::new();
    for max_overlap in [0.35, 0.55, 0.8] {
        for c in &candidates {
            if chosen.len() >= count {
                break;
            }
            if chosen.iter().any(|x| x.id == c.id) {
                continue;
            }
            let distinct = chosen.iter().all(|x| overlap(&c.footprint, &x.footprint) < max_overlap)
                && excluded.iter().all(|x| overlap(&c.footprint, x) < max_overlap.max(0.5));
            if distinct {
                chosen.push(c.clone());
            }
        }
    }
    if chosen.is_empty() {
        return Err("Plus de nouvelles boucles différentes depuis ce point : change la distance ou la direction.".into());
    }
    Ok(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_measure() {
        let a = [(1, 100.0), (2, 100.0), (5, 200.0)];
        let b = [(2, 50.0), (5, 10.0), (9, 1.0)];
        assert!((overlap(&a, &b) - 0.75).abs() < 1e-9);
        assert_eq!(overlap(&a, &[]), 0.0);
    }

    #[test]
    fn offset_distance() {
        let p = [-1.68, 48.11];
        let q = offset(p, 90.0, 10_000.0);
        assert!((haversine_m(p, q) - 10_000.0).abs() < 50.0);
        assert!(q[0] > p[0]);
    }

    fn leg(coords: Vec<[f64; 2]>) -> Leg {
        Leg { surface: vec![], class: vec![], length_m: 0.0, segs: vec![], coords }
    }

    #[test]
    fn hook_detection() {
        // 10 km tout droit vers l'est : pas de crochet
        let straight: Vec<[f64; 2]> = (0..=100).map(|i| [i as f64 * 0.0009, 0.0]).collect();
        assert_eq!(count_hooks(&[leg(straight.clone())]), 0);
        // Même ligne avec un aller-retour de 3 km vers le nord au milieu
        let mut hooked: Vec<[f64; 2]> = straight[..50].to_vec();
        let x = straight[50][0];
        hooked.extend((0..=30).map(|i| [x, i as f64 * 0.0009]));
        hooked.extend((0..=30).rev().map(|i| [x + 0.00005, i as f64 * 0.0009]));
        hooked.extend(straight[51..].iter().copied());
        assert_eq!(count_hooks(&[leg(hooked)]), 1);
    }

    #[test]
    fn rng_is_reproducible() {
        let (mut a, mut b) = (Rng(7), Rng(7));
        assert_eq!(a.next(), b.next());
        assert!((0.0..1.0).contains(&a.unit()));
    }
}

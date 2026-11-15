//! Guidage du mode navigation : repère, le long d'un itinéraire, les endroits où il faut
//! annoncer un changement de direction (« tourne à gauche », « au rond-point, 2e sortie »…).
//!
//! Avec le réseau routier chargé, la décision se prend **aux carrefours** : une route qui
//! tourne sans croiser personne (lacet, virage) n'est pas annoncée, un carrefour où l'on
//! continue tout droit non plus. Là où le réseau n'est pas chargé (trace importée d'une autre
//! région), on se rabat sur la seule forme du tracé : tout virage franc est annoncé.
//!
//! Le réseau ne connaît pas le nom des rues : les annonces donnent la direction et la distance.

use crate::geo::haversine_m;
use crate::graph::{Graph, FLAG_ROUNDABOUT};
use crate::osm_tags::RoadClass;
use crate::router::Router;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ManeuverKind {
    SlightLeft,
    Left,
    SharpLeft,
    SlightRight,
    Right,
    SharpRight,
    UTurn,
    Roundabout,
    Arrive,
}

/// Un changement de direction à annoncer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Maneuver {
    pub kind: ManeuverKind,
    /// Distance depuis le départ de l'itinéraire (m).
    pub at_m: f64,
    /// Position (lon, lat).
    pub point: [f64; 2],
    /// Angle du changement de direction en degrés : négatif à gauche, positif à droite.
    pub angle: f32,
    /// Rond-point : numéro de la sortie à prendre (les voies interdites aux vélos,
    /// absentes du réseau, ne sont pas comptées).
    pub exit: Option<u8>,
}

/// Distance sur laquelle on mesure le cap avant et après un carrefour (m).
const LOOK_M: f64 = 22.0;
/// Écart maximal entre un sommet du réseau et le tracé : tracé calculé par GoRide…
const EXACT_TOL_M: f64 = 2.0;
/// … ou trace venue d'ailleurs (autre planificateur, GPS), qui longe les routes de plus loin.
const LOOSE_TOL_M: f64 = 12.0;
/// Virage annoncé d'après la seule forme du tracé (sans réseau) à partir de cet angle.
const GEOM_TURN_DEG: f64 = 55.0;

/// Changements de direction d'un itinéraire, dans l'ordre, terminés par l'arrivée.
pub fn maneuvers(router: Option<&Router>, coords: &[[f64; 2]]) -> Vec<Maneuver> {
    if coords.len() < 2 {
        return Vec::new();
    }
    let path = Path::new(coords);
    let total = path.len();
    let mut out = Vec::new();
    if let Some(r) = router {
        junction_maneuvers(r, &path, &mut out);
    }
    let from_shape = shape_maneuvers(&path, router, &out);
    out.extend(from_shape);
    out.sort_by(|a, b| a.at_m.total_cmp(&b.at_m));
    // Deux annonces au même endroit : on garde la plus marquée (un rond-point l'emporte).
    let mut kept: Vec<Maneuver> = Vec::with_capacity(out.len() + 1);
    for m in out {
        match kept.last_mut() {
            Some(last) if m.at_m - last.at_m < 12.0 => {
                let stronger = m.kind == ManeuverKind::Roundabout
                    || (last.kind != ManeuverKind::Roundabout && m.angle.abs() > last.angle.abs());
                if stronger {
                    *last = m;
                }
            }
            _ => kept.push(m),
        }
    }
    kept.push(Maneuver { kind: ManeuverKind::Arrive, at_m: total, point: *coords.last().unwrap(), angle: 0.0, exit: None });
    kept
}

// --- Tracé : distances cumulées, position et cap à une distance donnée -------------------------

struct Path<'a> {
    pts: &'a [[f64; 2]],
    cum: Vec<f64>,
}

impl<'a> Path<'a> {
    fn new(pts: &'a [[f64; 2]]) -> Self {
        let mut cum = Vec::with_capacity(pts.len());
        let mut acc = 0.0;
        cum.push(0.0);
        for w in pts.windows(2) {
            acc += haversine_m(w[0], w[1]);
            cum.push(acc);
        }
        Path { pts, cum }
    }

    fn len(&self) -> f64 {
        *self.cum.last().unwrap()
    }

    /// Position à `d` mètres du départ (bornée aux extrémités).
    fn at(&self, d: f64) -> [f64; 2] {
        let d = d.clamp(0.0, self.len());
        let i = self.cum.partition_point(|&c| c < d).clamp(1, self.pts.len() - 1);
        let span = self.cum[i] - self.cum[i - 1];
        let t = if span > 0.0 { (d - self.cum[i - 1]) / span } else { 0.0 };
        let (a, b) = (self.pts[i - 1], self.pts[i]);
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
    }

    /// Cap suivi entre deux distances, ou rien si les deux positions sont confondues.
    fn bearing(&self, d0: f64, d1: f64) -> Option<f64> {
        let (a, b) = (self.at(d0), self.at(d1));
        (haversine_m(a, b) > 1.5).then(|| bearing(a, b))
    }
}

/// Cap de `a` vers `b` en degrés (0 = nord, 90 = est).
fn bearing(a: [f64; 2], b: [f64; 2]) -> f64 {
    let k = ((a[1] + b[1]) / 2.0).to_radians().cos();
    let deg = ((b[0] - a[0]) * k).atan2(b[1] - a[1]).to_degrees();
    (deg + 360.0) % 360.0
}

/// Angle pour passer du cap `from` au cap `to`, dans ]−180, 180] : positif = vers la droite.
fn turn(from: f64, to: f64) -> f64 {
    let d = (to - from).rem_euclid(360.0);
    if d > 180.0 {
        d - 360.0
    } else {
        d
    }
}

fn kind_for(angle: f64, slight: bool) -> ManeuverKind {
    use ManeuverKind::*;
    let a = angle.abs();
    let left = angle < 0.0;
    if a >= 155.0 {
        UTurn
    } else if a >= 115.0 {
        if left { SharpLeft } else { SharpRight }
    } else if slight {
        if left { SlightLeft } else { SlightRight }
    } else if left {
        Left
    } else {
        Right
    }
}

// --- Avec le réseau : décisions aux carrefours ---------------------------------------------------

/// Passage du tracé par un sommet du réseau.
struct Hit {
    node: u32,
    /// Distance depuis le départ (m).
    d: f64,
}

/// Sommets du réseau par lesquels passe le tracé, dans l'ordre de parcours.
fn node_hits(router: &Router, path: &Path, tol: f64, sample_m: Option<f64>) -> Vec<Hit> {
    // (sommet, distance le long du tracé, écart au tracé)
    let mut raw: Vec<(u32, f64, f64)> = Vec::new();
    let mut buf: Vec<(u32, f64)> = Vec::new();
    let mut probe = |p: [f64; 2], d: f64, raw: &mut Vec<(u32, f64, f64)>| {
        buf.clear();
        router.nodes_near(p, tol, &mut buf);
        raw.extend(buf.iter().map(|&(v, off)| (v, d, off)));
    };
    for i in 0..path.pts.len() {
        probe(path.pts[i], path.cum[i], &mut raw);
        // Trace à points espacés : on sonde aussi entre les points
        if let (Some(step), Some(&next)) = (sample_m, path.cum.get(i + 1)) {
            let mut d = path.cum[i] + step;
            while d < next - step / 2.0 {
                probe(path.at(d), d, &mut raw);
                d += step;
            }
        }
    }
    // Un même sommet vu depuis plusieurs points voisins : on garde le passage le plus proche.
    let gap = if sample_m.is_some() { 40.0 } else { 15.0 };
    raw.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut hits: Vec<Hit> = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let mut best = i;
        let mut j = i + 1;
        while j < raw.len() && raw[j].0 == raw[i].0 && raw[j].1 - raw[j - 1].1 <= gap {
            if raw[j].2 < raw[best].2 {
                best = j;
            }
            j += 1;
        }
        hits.push(Hit { node: raw[best].0, d: raw[best].1 });
        i = j;
    }
    hits.sort_by(|a, b| a.d.total_cmp(&b.d));
    hits
}

/// Tronçon qui relie directement deux sommets, de longueur compatible avec le tracé.
fn seg_between(g: &Graph, u: u32, v: u32, dist: f64) -> Option<u32> {
    if u == v {
        return None;
    }
    g.incident(u)
        .iter()
        .copied()
        .filter(|&s| g.other_end(s, u).0 == v)
        .min_by(|&a, &b| {
            let da = (g.seg_len[a as usize] as f64 - dist).abs();
            let db = (g.seg_len[b as usize] as f64 - dist).abs();
            da.total_cmp(&db)
        })
        .filter(|&s| (g.seg_len[s as usize] as f64 - dist).abs() <= 0.3 * dist + 25.0)
}

/// Vrai si l'on peut quitter le rond-point par ce sommet (voie hors anneau, sens autorisé).
fn has_exit(g: &Graph, v: u32) -> bool {
    g.incident(v).iter().any(|&s| {
        let i = s as usize;
        g.seg_from[i] != g.seg_to[i] && !g.has_flag(s, FLAG_ROUNDABOUT) && g.allows(s, g.seg_from[i] == v)
    })
}

/// Cap d'un tronçon en s'éloignant du sommet `v` (mesuré sur une vingtaine de mètres).
fn departure_bearing(g: &Graph, s: u32, v: u32) -> Option<f64> {
    let pts = g.seg_points(s);
    if pts.len() < 2 {
        return None;
    }
    let to64 = |p: [f32; 2]| [p[0] as f64, p[1] as f64];
    let from_v = g.seg_from[s as usize] == v;
    let order: Vec<[f64; 2]> = if from_v { pts.iter().map(|&p| to64(p)).collect() } else { pts.iter().rev().map(|&p| to64(p)).collect() };
    let origin = order[0];
    let mut acc = 0.0;
    let mut target = order[1];
    for w in order.windows(2) {
        acc += haversine_m(w[0], w[1]);
        target = w[1];
        if acc >= 18.0 {
            break;
        }
    }
    (haversine_m(origin, target) > 0.5).then(|| bearing(origin, target))
}

fn junction_maneuvers(router: &Router, path: &Path, out: &mut Vec<Maneuver>) {
    let g = &router.graph;
    // Tracé calculé par GoRide : il passe exactement par les sommets. Sinon, recherche plus large.
    let exact = node_hits(router, path, EXACT_TOL_M, None);
    let hits = if exact.len() as f64 >= (path.len() / 1000.0 * 2.0).max(2.0) {
        exact
    } else {
        node_hits(router, path, LOOSE_TOL_M, Some(8.0))
    };
    let n = hits.len();
    let seg_out: Vec<Option<u32>> = (0..n)
        .map(|k| if k + 1 < n { seg_between(g, hits[k].node, hits[k + 1].node, hits[k + 1].d - hits[k].d) } else { None })
        .collect();
    let on_ring = |s: Option<u32>| s.is_some_and(|s| g.has_flag(s, FLAG_ROUNDABOUT));

    let mut k = 0;
    while k < n {
        let arriving_on_ring = k > 0 && on_ring(seg_out[k - 1]);
        if !arriving_on_ring && on_ring(seg_out[k]) {
            // Entrée dans un rond-point : on compte les sorties jusqu'à celle que prend le tracé.
            let mut x = k + 1;
            let mut exits = 0u8;
            while x < n {
                if has_exit(g, hits[x].node) {
                    exits = exits.saturating_add(1);
                }
                if !on_ring(seg_out[x]) {
                    break;
                }
                x += 1;
            }
            if x < n {
                let d_in = hits[k].d;
                let d_out = hits[x].d;
                let angle = match (path.bearing(d_in - LOOK_M, d_in - 1.0), path.bearing(d_out + 1.0, d_out + LOOK_M)) {
                    (Some(a), Some(b)) => turn(a, b),
                    _ => 0.0,
                };
                out.push(Maneuver {
                    kind: ManeuverKind::Roundabout,
                    at_m: d_in,
                    point: path.at(d_in),
                    angle: angle as f32,
                    exit: Some(exits.max(1)),
                });
            }
            k = x + 1;
            continue;
        }
        if !arriving_on_ring {
            if let Some(m) = junction(g, path, &hits[k]) {
                out.push(m);
            }
        }
        k += 1;
    }
}

/// Faut-il annoncer quelque chose à ce carrefour ?
fn junction(g: &Graph, path: &Path, hit: &Hit) -> Option<Maneuver> {
    let v = hit.node;
    if g.incident(v).len() < 3 {
        return None; // simple jonction de deux voies : on suit la route
    }
    let d = hit.d;
    if d < 3.0 || d > path.len() - 3.0 {
        return None;
    }
    let inb = path.bearing(d - LOOK_M, d - 1.0)?;
    let outb = path.bearing(d + 1.0, d + LOOK_M)?;
    let theta = turn(inb, outb);

    // Départ de chaque voie du carrefour : (cap, autorisée dans ce sens, voie qui compte)
    let deps: Vec<(f64, bool, bool)> = g
        .incident(v)
        .iter()
        .filter(|&&s| g.seg_from[s as usize] != g.seg_to[s as usize])
        .filter_map(|&s| {
            let b = departure_bearing(g, s, v)?;
            let allowed = g.allows(s, g.seg_from[s as usize] == v);
            let minor = matches!(g.seg_class[s as usize], RoadClass::Service | RoadClass::Track | RoadClass::Path);
            Some((b, allowed, !minor))
        })
        .collect();
    // On écarte la voie d'où l'on vient et celle que l'on prend
    let closest = |target: f64| {
        deps.iter()
            .enumerate()
            .map(|(i, dep)| (turn(dep.0, target).abs(), i))
            .filter(|x| x.0 < 30.0)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|x| x.1)
    };
    let back = closest((inb + 180.0) % 360.0);
    let taken = closest(outb);
    // Écart de chaque autre voie par rapport au « tout droit »
    let alts: Vec<(f64, bool)> = deps
        .iter()
        .enumerate()
        .filter(|&(i, dep)| Some(i) != back && Some(i) != taken && dep.1)
        .map(|(_, dep)| (turn(inb, dep.0), dep.2))
        .collect();

    let a = theta.abs();
    let kind = if a >= 60.0 {
        kind_for(theta, false)
    } else if a >= 28.0 {
        // Virage modéré : annoncé seulement si une autre voie continue plus droit
        if !alts.iter().any(|&(dev, _)| dev.abs() < a - 10.0) {
            return None;
        }
        kind_for(theta, a < 45.0)
    } else if a >= 10.0 {
        // Fourche : deux voies partent vers l'avant, on dit de quel côté rester
        let &(dev, _) = alts.iter().find(|&&(dev, real)| real && dev.abs() < 45.0 && (dev - theta).abs() >= 15.0)?;
        if theta < dev {
            ManeuverKind::SlightLeft
        } else {
            ManeuverKind::SlightRight
        }
    } else {
        return None;
    };
    Some(Maneuver { kind, at_m: d, point: path.at(d), angle: theta as f32, exit: None })
}

// --- D'après la seule forme du tracé --------------------------------------------------------------

/// Virages francs repérés sur la forme du tracé. Là où le réseau est chargé, seuls les
/// demi-tours sont gardés (les carrefours ont déjà été traités, le reste n'est que virages).
fn shape_maneuvers(path: &Path, router: Option<&Router>, found: &[Maneuver]) -> Vec<Maneuver> {
    let total = path.len();
    if total < 60.0 {
        return Vec::new();
    }
    // Angle entre le cap des 18 m d'avant et celui des 18 m d'après, tous les 5 m
    let mut cands: Vec<(f64, f64)> = Vec::new();
    let mut d = 22.0;
    while d <= total - 22.0 {
        if let (Some(a), Some(b)) = (path.bearing(d - 22.0, d - 4.0), path.bearing(d + 4.0, d + 22.0)) {
            let t = turn(a, b);
            if t.abs() >= GEOM_TURN_DEG {
                cands.push((d, t));
            }
        }
        d += 5.0;
    }
    // Le plus marqué d'abord ; un seul par tranche de 30 m
    cands.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()).then(a.0.total_cmp(&b.0)));
    let mut peaks: Vec<(f64, f64)> = Vec::new();
    for c in cands {
        if peaks.iter().all(|p| (p.0 - c.0).abs() >= 30.0) {
            peaks.push(c);
        }
    }
    let mut out = Vec::new();
    for (d, t) in peaks {
        let p = path.at(d);
        // Demi-tour : on repasse au même endroit juste après (un lacet, lui, s'en écarte)
        let before = path.at(d - 15.0);
        let retrace = t.abs() >= 140.0 && (0..=12).any(|j| haversine_m(before, path.at(d + 5.0 + j as f64 * 2.5)) < 4.0);
        let covered = router.is_some_and(|r| r.road_near(p, 30.0));
        if covered && !retrace {
            continue;
        }
        if found.iter().any(|m| (m.at_m - d).abs() < 30.0) {
            continue;
        }
        let kind = if retrace {
            ManeuverKind::UTurn
        } else if t.abs() >= 115.0 {
            if t < 0.0 { ManeuverKind::SharpLeft } else { ManeuverKind::SharpRight }
        } else if t < 0.0 {
            ManeuverKind::Left
        } else {
            ManeuverKind::Right
        };
        out.push(Maneuver { kind, at_m: d, point: p, angle: t as f32, exit: None });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_sign_and_range() {
        assert_eq!(turn(90.0, 0.0), -90.0); // de l'est vers le nord : à gauche
        assert_eq!(turn(90.0, 180.0), 90.0); // de l'est vers le sud : à droite
        assert_eq!(turn(350.0, 10.0), 20.0);
        assert_eq!(turn(10.0, 350.0), -20.0);
        assert_eq!(turn(0.0, 180.0), 180.0);
    }

    #[test]
    fn bearing_cardinal() {
        assert!((bearing([0.0, 0.0], [0.0, 0.001]) - 0.0).abs() < 0.01);
        assert!((bearing([0.0, 0.0], [0.001, 0.0]) - 90.0).abs() < 0.01);
        assert!((bearing([0.0, 0.0], [0.0, -0.001]) - 180.0).abs() < 0.01);
        assert!((bearing([0.0, 0.0], [-0.001, 0.0]) - 270.0).abs() < 0.01);
    }
}

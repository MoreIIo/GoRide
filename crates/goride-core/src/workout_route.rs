//! Itinéraire adapté à une séance (F7.4) : parmi des boucles proposées, garder celles
//! qui offrent le terrain dont la séance a besoin — une montée assez longue pour les
//! efforts en côte, ou une longue portion plate et roulante pour le seuil ou le tempo.

use crate::geo::haversine_m;
use crate::graph::{Graph, FLAG_ROUNDABOUT};
use crate::loops::LoopProposal;
use crate::router::Leg;
use crate::workout::Terrain;
use serde::Serialize;

/// Ce que la séance demande au terrain.
#[derive(Debug, Clone, Copy)]
pub struct Need {
    pub terrain: Terrain,
    /// Plus long effort d'un seul tenant (s).
    pub effort_s: f64,
    /// Vitesse d'élévation en montée de la cycliste (m/h), d'après ses sorties.
    pub vam_m_h: f64,
    /// Vitesse sur le plat (km/h), d'après ses sorties.
    pub flat_speed_kmh: f64,
}

/// Portion clé de la boucle pour la séance.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySection {
    pub terrain: Terrain,
    /// Début et fin depuis le départ de la boucle (m).
    pub start_m: f64,
    pub end_m: f64,
    pub length_m: f64,
    pub gain_m: f64,
    /// Pente moyenne (%).
    pub grade_pct: f64,
    /// Temps estimé pour la parcourir à l'effort (s).
    pub est_s: f64,
    /// Rapport temps estimé / effort demandé (1 = l'effort tient pile dans la portion).
    pub fit: f64,
    pub coords: Vec<[f64; 2]>,
}

/// Profil grossier de la boucle, tronçon par tronçon : (distance cumulée, altitude relative).
fn profile(legs: &[Leg], g: &Graph) -> Vec<(f64, f64)> {
    let mut out = vec![(0.0, 0.0)];
    let (mut d, mut e) = (0.0f64, 0.0f64);
    for leg in legs {
        for &(s, fwd, m) in &leg.segs {
            let (up, down, _) = g.climb(s, fwd);
            let len = g.seg_len[s as usize].max(1.0);
            let f = (m / len).min(1.0) as f64;
            d += m as f64;
            e += (up - down) as f64 * f;
            out.push((d, e));
        }
    }
    out
}

/// Pour le plat : (début, fin, est plat ?) de chaque tronçon parcouru.
fn flatness(legs: &[Leg], g: &Graph) -> Vec<(f64, f64, bool)> {
    let mut out = Vec::new();
    let mut d = 0.0f64;
    for leg in legs {
        for &(s, fwd, m) in &leg.segs {
            let (up, down, _) = g.climb(s, fwd);
            let len = g.seg_len[s as usize].max(1.0);
            let flat = (up - down).abs() / len < 0.015 && (up + down) / len < 0.025 && !g.has_flag(s, FLAG_ROUNDABOUT);
            out.push((d, d + m as f64, flat));
            d += m as f64;
        }
    }
    out
}

/// Montées d'un profil : (indice du pied, indice du sommet). Une montée s'arrête quand on
/// redescend de plus de 15 m sous le sommet ou après 1 km sans reprendre d'altitude.
pub fn climbs(p: &[(f64, f64)]) -> Vec<(usize, usize)> {
    const DROP_M: f64 = 15.0;
    const STALL_M: f64 = 1_000.0;
    const MIN_GAIN_M: f64 = 25.0;
    const MIN_GRADE: f64 = 0.03;
    let mut out = Vec::new();
    if p.len() < 2 {
        return out;
    }
    let (mut lo, mut hi) = (0usize, 0usize);
    let push = |lo: usize, hi: usize, out: &mut Vec<(usize, usize)>| {
        let gain = p[hi].1 - p[lo].1;
        let len = p[hi].0 - p[lo].0;
        if hi > lo && gain >= MIN_GAIN_M && gain / len.max(1.0) >= MIN_GRADE {
            out.push((lo, hi));
        }
    };
    for i in 1..p.len() {
        if p[i].1 > p[hi].1 {
            hi = i;
        }
        if p[hi].1 - p[i].1 > DROP_M || p[i].0 - p[hi].0 > STALL_M {
            push(lo, hi, &mut out);
            lo = i;
            hi = i;
        } else if p[i].1 < p[lo].1 || (p[i].1 <= p[lo].1 + 0.5 && p[hi].1 - p[lo].1 < 2.0) {
            // Nouveau point bas, ou toujours sur le plat avant le pied : la montée commence plus loin
            lo = i;
            hi = i;
        }
    }
    push(lo, hi, &mut out);
    out
}

/// Coordonnées de la boucle entre deux distances depuis le départ (m).
fn slice(legs: &[Leg], from_m: f64, to_m: f64) -> Vec<[f64; 2]> {
    let pts: Vec<[f64; 2]> = legs.iter().flat_map(|l| l.coords.iter().copied()).collect();
    let mut out = Vec::new();
    let mut d = 0.0;
    for (i, &c) in pts.iter().enumerate() {
        if i > 0 {
            d += haversine_m(pts[i - 1], c);
        }
        if d >= from_m && d <= to_m {
            out.push(c);
        }
        if d > to_m {
            break;
        }
    }
    out
}

/// Meilleure portion de la boucle pour la séance (None si la séance n'a pas besoin de terrain
/// particulier, ou si la boucle n'en offre aucune).
pub fn key_section(l: &LoopProposal, g: &Graph, need: &Need) -> Option<KeySection> {
    let effort = need.effort_s.max(60.0);
    match need.terrain {
        Terrain::Any => None,
        Terrain::Climb => {
            if !g.has_elevation {
                return None;
            }
            let p = profile(&l.legs, g);
            let vam = need.vam_m_h.clamp(300.0, 1_800.0);
            climbs(&p)
                .into_iter()
                .map(|(a, b)| {
                    let (gain, len) = (p[b].1 - p[a].1, p[b].0 - p[a].0);
                    let grade = gain / len * 100.0;
                    // En montée, le temps vient de la VAM ; sur les pentes douces la vitesse plafonne
                    let est = (gain / vam * 3600.0).max(len / (28.0 / 3.6));
                    let fit = est / effort;
                    // Idéal : pente de 4 à 8 %, effort qui tient dans la montée, pied après l'échauffement
                    let score = fit.min(1.1) * 100.0
                        - (grade - 6.0).abs().max(2.0) * 4.0
                        - if p[a].0 < 4_000.0 { 15.0 } else { 0.0 };
                    (score, a, b, gain, len, grade, est, fit)
                })
                .max_by(|x, y| x.0.total_cmp(&y.0))
                .map(|(_, a, b, gain, len, grade, est, fit)| KeySection {
                    terrain: Terrain::Climb,
                    start_m: p[a].0,
                    end_m: p[b].0,
                    length_m: len,
                    gain_m: gain,
                    grade_pct: grade,
                    est_s: est,
                    fit,
                    coords: slice(&l.legs, p[a].0, p[b].0),
                })
        }
        Terrain::Flat => {
            // Portions plates d'un seul tenant ; un court passage non plat (< 120 m) ne les coupe pas
            const GAP_M: f64 = 120.0;
            let parts = flatness(&l.legs, g);
            let mut best: Option<(f64, f64)> = None;
            let mut run: Option<(f64, f64)> = None;
            let mut gap = 0.0;
            for &(a, b, flat) in &parts {
                if flat {
                    run = Some(run.map_or((a, b), |(s, _)| (s, b)));
                    gap = 0.0;
                } else if run.is_some() {
                    gap += b - a;
                    if gap > GAP_M {
                        let r = run.take().unwrap();
                        if best.is_none_or(|x| r.1 - r.0 > x.1 - x.0) {
                            best = Some(r);
                        }
                        gap = 0.0;
                    }
                }
            }
            if let Some(r) = run {
                if best.is_none_or(|x| r.1 - r.0 > x.1 - x.0) {
                    best = Some(r);
                }
            }
            let (a, b) = best?;
            let len = b - a;
            if len < 1_000.0 {
                return None;
            }
            let est = len / (need.flat_speed_kmh.clamp(15.0, 45.0) / 3.6);
            Some(KeySection {
                terrain: Terrain::Flat,
                start_m: a,
                end_m: b,
                length_m: len,
                gain_m: 0.0,
                grade_pct: 0.0,
                est_s: est,
                fit: est / effort,
                coords: slice(&l.legs, a, b),
            })
        }
    }
}

/// Classe les boucles pour la séance (les plus adaptées d'abord) et en garde `keep`.
pub fn rank_for_workout(
    loops: Vec<LoopProposal>,
    g: &Graph,
    need: &Need,
    keep: usize,
) -> Vec<(LoopProposal, Option<KeySection>)> {
    let mut scored: Vec<(f64, LoopProposal, Option<KeySection>)> = loops
        .into_iter()
        .map(|l| {
            let k = key_section(&l, g, need);
            let score = match &k {
                Some(k) => k.fit.min(1.2) - l.retrace,
                None if need.terrain == Terrain::Any => 1.0 - l.retrace,
                None => -1.0,
            };
            (score, l, k)
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().take(keep).map(|(_, l, k)| (l, k)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_climbs_and_ignores_small_bumps() {
        // 2 km de plat, 3 km à 5 % (150 m), descente, une bosse de 10 m, 2 km à 4 % (80 m)
        let mut p = vec![(0.0, 0.0)];
        let mut push = |dd: f64, de: f64| {
            let (d, e) = *p.last().unwrap();
            p.push((d + dd, e + de));
        };
        for _ in 0..4 {
            push(500.0, 0.0);
        }
        for _ in 0..6 {
            push(500.0, 25.0);
        }
        for _ in 0..6 {
            push(500.0, -25.0);
        }
        push(300.0, 10.0);
        push(300.0, -10.0);
        for _ in 0..4 {
            push(500.0, 20.0);
        }
        let c = climbs(&p);
        assert_eq!(c.len(), 2, "{c:?}");
        let (a, b) = c[0];
        assert!((p[b].1 - p[a].1 - 150.0).abs() < 1e-6);
        assert!((p[b].0 - p[a].0 - 3_000.0).abs() < 1e-6);
        let (a, b) = c[1];
        assert!((p[b].1 - p[a].1 - 80.0).abs() < 1e-6);
    }

    #[test]
    fn a_short_dip_does_not_split_a_climb() {
        let p = vec![(0.0, 0.0), (1000.0, 50.0), (1200.0, 42.0), (2200.0, 100.0)];
        assert_eq!(climbs(&p), vec![(0, 3)]);
    }
}

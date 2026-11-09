//! Assemble un itinéraire tracé (étapes bout à bout) en trace complète :
//! altitude, D+/D−, répartition des revêtements et des types de route.

use crate::dem::Dem;
use crate::geo::haversine_m;
use crate::osm_tags::{RoadClass, Surface};
use crate::track::Track;
use serde::Serialize;

/// Espacement maximal entre deux points pour l'échantillonnage d'altitude (m).
const SAMPLE_SPACING_M: f64 = 25.0;
/// Demi-largeur de la moyenne glissante appliquée à l'altitude du terrain (m).
const SMOOTH_HALF_WINDOW_M: f64 = 40.0;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Share<T> {
    pub kind: T,
    pub meters: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteSummary {
    pub track: Track,
    pub surfaces: Vec<Share<Surface>>,
    pub classes: Vec<Share<RoadClass>>,
}

/// `surface[i]` et `class[i]` décrivent la portion coords[i] → coords[i + 1].
pub fn summarize(coords: &[[f64; 2]], surface: &[Surface], class: &[RoadClass], dem: Option<&Dem>) -> RouteSummary {
    // Répartition des revêtements et types de route
    let mut surf = [(Surface::Paved, 0.0), (Surface::Assumed, 0.0), (Surface::Unpaved, 0.0)];
    let mut cls: Vec<(RoadClass, f64)> = RoadClass::ALL.iter().map(|&c| (c, 0.0)).collect();
    for (i, w) in coords.windows(2).enumerate() {
        let d = haversine_m(w[0], w[1]);
        if let Some(s) = surface.get(i) {
            surf.iter_mut().find(|x| x.0 == *s).unwrap().1 += d;
        }
        if let Some(c) = class.get(i) {
            cls.iter_mut().find(|x| x.0 == *c).unwrap().1 += d;
        }
    }

    // Points rapprochés pour suivre le relief, puis altitude du terrain lissée
    let dense = densify(coords, SAMPLE_SPACING_M);
    let ele = match dem {
        Some(dem) => smooth(&dense, dem.elevations(&dense)),
        None => vec![None; dense.len()],
    };
    let track = Track::from_points(None, dense.into_iter().zip(ele).collect());

    RouteSummary {
        track,
        surfaces: surf.into_iter().map(|(kind, meters)| Share { kind, meters }).collect(),
        classes: cls.into_iter().filter(|c| c.1 > 0.0).map(|(kind, meters)| Share { kind, meters }).collect(),
    }
}

fn densify(coords: &[[f64; 2]], spacing: f64) -> Vec<[f64; 2]> {
    let mut out = Vec::with_capacity(coords.len() * 2);
    for w in coords.windows(2) {
        out.push(w[0]);
        let d = haversine_m(w[0], w[1]);
        let n = (d / spacing).ceil() as usize;
        for k in 1..n {
            let t = k as f64 / n as f64;
            out.push([w[0][0] + (w[1][0] - w[0][0]) * t, w[0][1] + (w[1][1] - w[0][1]) * t]);
        }
    }
    if let Some(&last) = coords.last() {
        out.push(last);
    }
    out
}

/// Moyenne glissante sur la distance : gomme le bruit du modèle de terrain
/// (pixels de 25 m) qui gonflerait artificiellement le D+.
fn smooth(pts: &[[f64; 2]], ele: Vec<Option<f64>>) -> Vec<Option<f64>> {
    let mut dist = Vec::with_capacity(pts.len());
    let mut acc = 0.0;
    for (i, p) in pts.iter().enumerate() {
        if i > 0 {
            acc += haversine_m(pts[i - 1], *p);
        }
        dist.push(acc);
    }
    let (mut lo, mut hi) = (0usize, 0usize);
    let (mut sum, mut cnt) = (0.0, 0usize);
    let mut out = Vec::with_capacity(pts.len());
    for i in 0..pts.len() {
        while hi < pts.len() && dist[hi] <= dist[i] + SMOOTH_HALF_WINDOW_M {
            if let Some(e) = ele[hi] {
                sum += e;
                cnt += 1;
            }
            hi += 1;
        }
        while dist[lo] < dist[i] - SMOOTH_HALF_WINDOW_M {
            if let Some(e) = ele[lo] {
                sum -= e;
                cnt -= 1;
            }
            lo += 1;
        }
        out.push(if ele[i].is_some() && cnt > 0 { Some(sum / cnt as f64) } else { None });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn densify_spacing() {
        let c = [[0.0, 0.0], [0.0, 0.001]]; // ≈ 111 m
        let d = densify(&c, 25.0);
        assert_eq!(d.len(), 6);
        assert!(d.windows(2).all(|w| haversine_m(w[0], w[1]) <= 25.0));
    }

    #[test]
    fn breakdown_sums_to_length() {
        let c = [[0.0, 0.0], [0.0, 0.001], [0.0, 0.002]];
        let r = summarize(&c, &[Surface::Paved, Surface::Assumed], &[RoadClass::Minor, RoadClass::Minor], None);
        let total: f64 = r.surfaces.iter().map(|s| s.meters).sum();
        assert!((total - r.track.stats.distance_m).abs() < 0.01);
        assert_eq!(r.classes.len(), 1);
        assert!(!r.track.stats.has_elevation);
    }

    #[test]
    fn smoothing_flattens_noise() {
        let pts: Vec<[f64; 2]> = (0..100).map(|i| [0.0, i as f64 * 0.0002]).collect(); // pas de 22 m
        let noisy: Vec<Option<f64>> = (0..100).map(|i| Some(if i % 2 == 0 { 50.0 } else { 54.0 })).collect();
        let s = smooth(&pts, noisy);
        let spread = s[10..90].iter().map(|v| v.unwrap()).fold((f64::MAX, f64::MIN), |a, v| (a.0.min(v), a.1.max(v)));
        assert!(spread.1 - spread.0 < 1.5, "{spread:?}");
    }
}

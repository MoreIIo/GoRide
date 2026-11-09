//! Trace : suite de points avec distance cumulée et altitude, plus ses statistiques.

use crate::geo::haversine_m;
use serde::Serialize;

/// Seuil d'hystérésis pour le D+/D− (m). Les variations plus petites sont
/// considérées comme du bruit GPS/baro. 3 m donne des valeurs proches de
/// Garmin Connect ; il sera calibré sur les vraies sorties en phase 4.
pub const ELEVATION_THRESHOLD_M: f64 = 3.0;

/// Trace prête à afficher. Format « colonnes » (un tableau par grandeur)
/// pour un transfert léger vers l'interface.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub name: Option<String>,
    /// [longitude, latitude] en degrés (ordre GeoJSON).
    pub coords: Vec<[f64; 2]>,
    /// Altitude en mètres, interpolée là où le fichier n'en donne pas.
    /// `None` partout si le fichier n'a aucune altitude.
    pub ele: Vec<Option<f64>>,
    /// Distance cumulée depuis le départ, en mètres.
    pub dist: Vec<f64>,
    pub stats: TrackStats,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrackStats {
    pub distance_m: f64,
    pub ascent_m: f64,
    pub descent_m: f64,
    pub min_ele_m: Option<f64>,
    pub max_ele_m: Option<f64>,
    pub point_count: usize,
    pub has_elevation: bool,
}

impl Track {
    /// Construit une trace à partir de points bruts (lon, lat, altitude éventuelle).
    pub fn from_points(name: Option<String>, raw: Vec<([f64; 2], Option<f64>)>) -> Self {
        let mut coords = Vec::with_capacity(raw.len());
        let mut ele = Vec::with_capacity(raw.len());
        let mut dist = Vec::with_capacity(raw.len());

        let mut total = 0.0;
        for (i, (c, e)) in raw.into_iter().enumerate() {
            if i > 0 {
                total += haversine_m(coords[i - 1], c);
            }
            coords.push(c);
            ele.push(e.filter(|v: &f64| v.is_finite()));
            dist.push(total);
        }

        interpolate_missing(&mut ele, &dist);
        let stats = compute_stats(&ele, total);

        Track { name, coords, ele, dist, stats }
    }
}

/// Remplit les altitudes manquantes par interpolation linéaire sur la distance ;
/// aux extrémités, recopie la valeur connue la plus proche.
fn interpolate_missing(ele: &mut [Option<f64>], dist: &[f64]) {
    let known: Vec<usize> = (0..ele.len()).filter(|&i| ele[i].is_some()).collect();
    let (Some(&first), Some(&last)) = (known.first(), known.last()) else {
        return; // aucune altitude : on laisse tout à None
    };
    for i in 0..first {
        ele[i] = ele[first];
    }
    for i in last + 1..ele.len() {
        ele[i] = ele[last];
    }
    for w in known.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b - a < 2 {
            continue;
        }
        let (ea, eb) = (ele[a].unwrap(), ele[b].unwrap());
        let span = dist[b] - dist[a];
        for i in a + 1..b {
            let t = if span > 0.0 { (dist[i] - dist[a]) / span } else { 0.5 };
            ele[i] = Some(ea + (eb - ea) * t);
        }
    }
}

fn compute_stats(ele: &[Option<f64>], distance_m: f64) -> TrackStats {
    let values: Vec<f64> = ele.iter().flatten().copied().collect();
    let has_elevation = !values.is_empty();

    let (ascent_m, descent_m) = ascent_descent(&values, ELEVATION_THRESHOLD_M);
    let min_ele_m = values.iter().copied().reduce(f64::min);
    let max_ele_m = values.iter().copied().reduce(f64::max);

    TrackStats {
        distance_m,
        ascent_m,
        descent_m,
        min_ele_m,
        max_ele_m,
        point_count: ele.len(),
        has_elevation,
    }
}

/// D+ et D− avec hystérésis : on ne valide un changement qu'une fois
/// l'écart avec la dernière altitude retenue supérieur au seuil.
pub fn ascent_descent(values: &[f64], threshold: f64) -> (f64, f64) {
    let Some(&first) = values.first() else {
        return (0.0, 0.0);
    };
    let (mut up, mut down) = (0.0, 0.0);
    let mut reference = first;
    for &v in &values[1..] {
        let delta = v - reference;
        if delta >= threshold {
            up += delta;
            reference = v;
        } else if delta <= -threshold {
            down += -delta;
            reference = v;
        }
    }
    (up, down)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_below_threshold_is_ignored() {
        let v = [100.0, 101.0, 100.0, 101.5, 100.2, 101.0];
        assert_eq!(ascent_descent(&v, 3.0), (0.0, 0.0));
    }

    #[test]
    fn climb_then_descent() {
        let v = [100.0, 110.0, 120.0, 150.0, 130.0, 100.0];
        assert_eq!(ascent_descent(&v, 3.0), (50.0, 50.0));
    }

    #[test]
    fn missing_elevations_are_interpolated() {
        let raw = vec![
            ([0.0, 0.0], None),
            ([0.0, 0.001], Some(10.0)),
            ([0.0, 0.002], None),
            ([0.0, 0.003], Some(30.0)),
            ([0.0, 0.004], None),
        ];
        let t = Track::from_points(None, raw);
        let e: Vec<f64> = t.ele.iter().map(|x| x.unwrap()).collect();
        assert_eq!(e[0], 10.0);
        assert!((e[2] - 20.0).abs() < 1e-6);
        assert_eq!(e[4], 30.0);
    }

    #[test]
    fn no_elevation_at_all() {
        let t = Track::from_points(None, vec![([0.0, 0.0], None), ([0.0, 0.01], None)]);
        assert!(!t.stats.has_elevation);
        assert_eq!(t.stats.ascent_m, 0.0);
        assert!(t.stats.distance_m > 1_000.0);
    }
}

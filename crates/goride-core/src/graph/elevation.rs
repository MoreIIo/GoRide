//! Altitudes du réseau : pour chaque tronçon, dénivelé et pente maximale
//! dans chaque sens. C'est ce qui permet au routage de chercher ou d'éviter
//! les bosses.

use super::{Graph, FLAG_STRUCTURE};
use rayon::prelude::*;

/// Espacement maximal entre deux échantillons d'altitude sur un tronçon (m).
const SPACING_M: f32 = 30.0;
/// Longueur minimale sur laquelle on mesure une pente (m) : en dessous, le
/// modèle de terrain (pixels de 25 m) donne des pentes fantaisistes.
const GRADE_WINDOW_M: f32 = 80.0;
const MIN_GRADE_LEN_M: f32 = 30.0;
/// Demi-largeur du lissage de l'altitude le long d'un tronçon (m).
const SMOOTH_HALF_M: f32 = 60.0;
/// Rayon autour d'un carrefour sur lequel on moyenne son altitude (m).
const NODE_RADIUS_M: f32 = 90.0;
/// Variations plus petites ignorées dans le calcul du dénivelé (m) : bruit du terrain.
const HYSTERESIS_M: f32 = 2.5;

#[inline]
fn flat_dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    let k = (a[1].to_radians()).cos();
    let dx = (b[0] - a[0]) * k * 111_320.0;
    let dy = (b[1] - a[1]) * 110_574.0;
    (dx * dx + dy * dy).sqrt()
}

/// Calcule les altitudes de tous les tronçons. `sample` renvoie l'altitude de
/// chaque point (NaN si inconnue) ; il est appelé une seule fois, avec tous les points.
pub fn compute_elevation(g: &mut Graph, sample: impl FnOnce(&[[f32; 2]]) -> Vec<f32>) {
    // 1. Points d'échantillonnage le long de chaque tronçon
    let nseg = g.seg_count();
    let mut pts: Vec<[f32; 2]> = Vec::with_capacity(g.geom.len() * 2);
    let mut dist: Vec<f32> = Vec::with_capacity(g.geom.len() * 2);
    let mut start: Vec<u32> = Vec::with_capacity(nseg + 1);
    for s in 0..nseg as u32 {
        start.push(pts.len() as u32);
        let shape = g.seg_points(s);
        let mut acc = 0.0f32;
        pts.push(shape[0]);
        dist.push(0.0);
        for w in shape.windows(2) {
            let d = flat_dist(w[0], w[1]);
            let n = (d / SPACING_M).ceil().max(1.0) as usize;
            for k in 1..=n {
                let t = k as f32 / n as f32;
                pts.push([w[0][0] + (w[1][0] - w[0][0]) * t, w[0][1] + (w[1][1] - w[0][1]) * t]);
                dist.push(acc + d * t);
            }
            acc += d;
        }
    }
    start.push(pts.len() as u32);

    // 2. Altitudes
    let ele = sample(&pts);
    drop(pts);
    let known = ele.iter().filter(|e| e.is_finite()).count();

    // 3. Altitude de chaque carrefour : moyenne des points proches sur toutes les routes qui s'y croisent.
    //    Les tronçons voisins partagent ainsi exactement la même valeur à leur jonction.
    let mut node_sum = vec![0.0f64; g.node_count()];
    let mut node_cnt = vec![0u32; g.node_count()];
    for s in 0..nseg {
        let (a, b) = (start[s] as usize, start[s + 1] as usize);
        let total = dist[b - 1];
        for i in a..b {
            let v = ele[i];
            if !v.is_finite() {
                continue;
            }
            if dist[i] <= NODE_RADIUS_M {
                node_sum[g.seg_from[s] as usize] += v as f64;
                node_cnt[g.seg_from[s] as usize] += 1;
            }
            if total - dist[i] <= NODE_RADIUS_M {
                node_sum[g.seg_to[s] as usize] += v as f64;
                node_cnt[g.seg_to[s] as usize] += 1;
            }
        }
    }
    let node_ele: Vec<f32> =
        (0..g.node_count()).map(|v| if node_cnt[v] > 0 { (node_sum[v] / node_cnt[v] as f64) as f32 } else { f32::NAN }).collect();

    // 4. Dénivelé et pentes par tronçon
    let results: Vec<(f32, f32, i8, i8)> = (0..nseg)
        .into_par_iter()
        .map(|s| {
            let (a, b) = (start[s] as usize, start[s + 1] as usize);
            let ends = [node_ele[g.seg_from[s] as usize], node_ele[g.seg_to[s] as usize]];
            segment_climb(&ele[a..b], &dist[a..b], g.seg_flags[s] & FLAG_STRUCTURE != 0, ends)
        })
        .collect();
    for (s, (up, down, gf, gb)) in results.into_iter().enumerate() {
        g.seg_up[s] = up;
        g.seg_down[s] = down;
        g.seg_grade_fwd[s] = gf;
        g.seg_grade_bwd[s] = gb;
    }
    // Au moins la moitié des points connus : sinon on considère l'altitude indisponible
    g.has_elevation = known * 2 >= ele.len().max(1);
}

/// (montée, descente, pente max de montée aller %, pente max de montée retour %).
/// `ends` : altitudes lissées des deux carrefours (NaN si inconnues).
fn segment_climb(raw: &[f32], dist: &[f32], structure: bool, ends: [f32; 2]) -> (f32, f32, i8, i8) {
    let n = raw.len();
    if n < 2 {
        return (0.0, 0.0, 0, 0);
    }
    // Trous comblés avec la valeur connue la plus proche
    let mut e: Vec<f32> = raw.to_vec();
    let Some(first) = e.iter().position(|v| v.is_finite()) else {
        return (0.0, 0.0, 0, 0);
    };
    for i in 0..first {
        e[i] = e[first];
    }
    for i in first + 1..n {
        if !e[i].is_finite() {
            e[i] = e[i - 1];
        }
    }
    let total = dist[n - 1];

    // Lissage sur la distance (± SMOOTH_HALF_M), puis extrémités = altitude des carrefours
    let mut sm = Vec::with_capacity(n);
    let (mut lo, mut hi, mut sum) = (0usize, 0usize, 0.0f32);
    for i in 0..n {
        while hi < n && dist[hi] <= dist[i] + SMOOTH_HALF_M {
            sum += e[hi];
            hi += 1;
        }
        while dist[lo] < dist[i] - SMOOTH_HALF_M {
            sum -= e[lo];
            lo += 1;
        }
        sm.push(sum / (hi - lo) as f32);
    }
    if ends[0].is_finite() {
        sm[0] = ends[0];
    }
    if ends[1].is_finite() {
        sm[n - 1] = ends[1];
    }
    let mut e = sm;
    if structure && total > 0.0 {
        // Pont ou tunnel : ligne droite entre les deux extrémités
        let (e0, e1) = (e[0], e[n - 1]);
        for i in 0..n {
            e[i] = e0 + (e1 - e0) * dist[i] / total;
        }
    }

    // Dénivelé avec hystérésis : les petites oscillations du terrain ne comptent pas
    let (mut up, mut down) = (0.0f32, 0.0f32);
    let mut reference = e[0];
    for &v in &e[1..] {
        let d = v - reference;
        if d >= HYSTERESIS_M {
            up += d;
            reference = v;
        } else if d <= -HYSTERESIS_M {
            down -= d;
            reference = v;
        }
    }
    // Reste entre le dernier palier et l'arrivée : compté (au-delà d'un demi-mètre) pour qu'une
    // longue pente douce, découpée en courts tronçons, garde tout son dénivelé.
    let rest = e[n - 1] - reference;
    if rest >= 0.5 {
        up += rest;
    } else if rest <= -0.5 {
        down -= rest;
    }

    // Pente maximale sur une fenêtre glissante d'au moins GRADE_WINDOW_M
    let (mut gmax, mut gmin) = (0.0f32, 0.0f32);
    if total >= MIN_GRADE_LEN_M {
        let window = GRADE_WINDOW_M.min(total);
        let mut j = 0;
        for i in 0..n {
            while j < n && dist[j] - dist[i] < window {
                j += 1;
            }
            if j >= n {
                break;
            }
            let grade = (e[j] - e[i]) / (dist[j] - dist[i]) * 100.0;
            gmax = gmax.max(grade);
            gmin = gmin.min(grade);
        }
    }
    let clamp = |v: f32| v.round().clamp(0.0, 60.0) as i8;
    (up, down, clamp(gmax), clamp(-gmin))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_ENDS: [f32; 2] = [f32::NAN, f32::NAN];

    fn line(n: usize, step: f32) -> Vec<f32> {
        (0..n).map(|i| i as f32 * step).collect()
    }

    #[test]
    fn steady_climb() {
        // 300 m à 5 %
        let d = line(11, 30.0);
        let e: Vec<f32> = d.iter().map(|x| 10.0 + x * 0.05).collect();
        let (up, down, gf, gb) = segment_climb(&e, &d, false, [10.0, 25.0]);
        assert!((up - 15.0).abs() < 0.01 && down < 0.01);
        assert_eq!((gf, gb), (5, 0));
    }

    #[test]
    fn bridge_is_flattened() {
        // Pont de 300 m au-dessus d'une vallée de 20 m : le terrain dessous ne compte pas
        let d = line(11, 30.0);
        let e = [30.0, 26.0, 22.0, 18.0, 14.0, 10.0, 14.0, 18.0, 22.0, 26.0, 30.0];
        let (up, down, gf, gb) = segment_climb(&e, &d, true, [30.0, 30.0]);
        assert_eq!((up, down, gf, gb), (0.0, 0.0, 0, 0));
        let (up, down, ..) = segment_climb(&e, &d, false, [30.0, 30.0]);
        assert!(up > 10.0 && down > 10.0, "{up} {down}");
    }

    #[test]
    fn missing_values_filled() {
        let d = line(4, 30.0);
        let e = [f32::NAN, 10.0, f32::NAN, 13.0];
        let (up, ..) = segment_climb(&e, &d, false, NO_ENDS);
        assert!(up > 0.0 && up.is_finite());
        let (up, down, ..) = segment_climb(&[f32::NAN; 3], &line(3, 30.0), false, NO_ENDS);
        assert_eq!((up, down), (0.0, 0.0));
    }

    #[test]
    fn short_segments_have_no_grade() {
        let (_, _, gf, _) = segment_climb(&[0.0, 5.0], &[0.0, 20.0], false, NO_ENDS);
        assert_eq!(gf, 0);
    }

    #[test]
    fn noise_is_not_relief() {
        // 600 m plat avec ± 1,5 m de bruit : pas de dénivelé
        let d = line(21, 30.0);
        let e: Vec<f32> = (0..21).map(|i| 50.0 + if i % 2 == 0 { 1.5 } else { -1.5 }).collect();
        let (up, down, gf, gb) = segment_climb(&e, &d, false, [50.0, 50.0]);
        assert!(up < 1.0 && down < 1.0, "{up} {down}");
        assert!(gf <= 1 && gb <= 1);
    }

    #[test]
    fn junction_altitude_is_used() {
        let d = line(6, 30.0);
        let e = [40.0; 6];
        let (up, down, ..) = segment_climb(&e, &d, false, [40.0, 46.0]);
        assert!((up - 6.0).abs() < 0.01 && down < 0.01);
    }
}

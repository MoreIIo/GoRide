//! Analyse d'une sortie : indicateurs, zones, meilleurs efforts, montées et conseils.
//!
//! Conventions (proches de Strava / TrainingPeaks) :
//! - temps en mouvement : vitesse > 3,6 km/h, pauses de plus de 10 s exclues ;
//! - puissance normalisée (NP) : moyenne glissante sur 30 s, puissance 4, racine 4ᵉ ;
//! - IF = NP / FTP ; TSS = durée (h) × IF² × 100 ;
//! - zones FC en % de la FC max (5 zones), zones de puissance de Coggan en % de la FTP (7 zones).

use super::Activity;
use crate::track::ascent_descent;
use serde::{Deserialize, Serialize};

/// Profil de la cycliste (réglé dans l'application ; tout est facultatif).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Athlete {
    /// Puissance au seuil (W).
    pub ftp: Option<f32>,
    /// Fréquence cardiaque maximale (bpm).
    pub hr_max: Option<f32>,
    /// Poids (kg).
    pub weight: Option<f32>,
}

/// Vitesse en dessous de laquelle on est considéré à l'arrêt (m/s).
const MOVING_SPEED: f32 = 1.0;
/// Trou entre deux points au-delà duquel c'est une pause (s).
const PAUSE_GAP: f32 = 10.0;
/// D+ : lissage de l'altitude (points de part et d'autre) et seuil d'hystérésis (m).
/// Calibré sur 6 sorties d'un Garmin Edge 540 (enregistrement chaque seconde) :
/// 5 sur 6 à moins de 2 % du D+ du compteur, pauses exclues comme le fait le compteur.
pub const ELE_SMOOTH: usize = 2;
pub const ELE_THRESHOLD: f64 = 0.5;
/// Durées des meilleurs efforts (s).
pub const BEST_DURATIONS: [u32; 10] = [5, 10, 30, 60, 120, 300, 600, 1200, 1800, 3600];
/// Bornes des zones de puissance (Coggan), en fraction de la FTP.
const POWER_ZONES: [f32; 6] = [0.55, 0.75, 0.90, 1.05, 1.20, 1.50];
/// Bornes des zones cardiaques, en fraction de la FC max.
const HR_ZONES: [f32; 4] = [0.60, 0.70, 0.80, 0.90];
/// Nombre maximal de points des courbes envoyées à l'interface.
const CHART_POINTS: usize = 1500;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerStats {
    pub avg: f32,
    pub max: f32,
    pub np: f32,
    /// Indice de variabilité NP / moyenne.
    pub vi: f32,
    pub work_kj: f32,
    pub intensity: Option<f32>,
    pub tss: Option<f32>,
    pub w_per_kg: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Climb {
    pub start_km: f32,
    pub end_km: f32,
    pub length_m: f32,
    pub gain_m: f32,
    /// Pente moyenne (%).
    pub grade: f32,
    pub duration_s: f32,
    /// Vitesse ascensionnelle (m/h).
    pub vam: f32,
    /// « HC », « 1 » … « 4 » ou vide (selon longueur × pente, comme Strava).
    pub category: String,
    pub avg_power: Option<f32>,
    pub avg_hr: Option<f32>,
    pub avg_cadence: Option<f32>,
    /// Position (lon, lat) du pied et du sommet : sert à retrouver la même montée dans d'autres sorties.
    pub start: Option<[f32; 2]>,
    pub end: Option<[f32; 2]>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Advice {
    /// « good », « tip », « warning » ou « info ».
    pub kind: &'static str,
    pub title: String,
    pub text: String,
}

/// Courbes réduites pour les graphiques (une valeur par tranche de distance).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    pub dist_km: Vec<f32>,
    pub time_s: Vec<f32>,
    pub ele: Vec<f32>,
    pub speed_kmh: Vec<f32>,
    pub hr: Vec<f32>,
    pub power: Vec<f32>,
    pub cadence: Vec<f32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub distance_m: f64,
    pub elapsed_s: f32,
    pub moving_s: f32,
    pub avg_speed_kmh: f32,
    pub max_speed_kmh: f32,
    /// D+ retenu : celui du compteur s'il l'a calculé, sinon celui de GoRide.
    pub ascent_m: f64,
    pub descent_m: f64,
    /// D+ recalculé par GoRide depuis les altitudes du fichier.
    pub ascent_computed_m: f64,
    /// D+ du compteur (message « session » du FIT).
    pub ascent_device_m: Option<f64>,
    pub ele_min: Option<f32>,
    pub ele_max: Option<f32>,
    pub avg_hr: Option<f32>,
    pub max_hr: Option<f32>,
    pub avg_cadence: Option<f32>,
    pub power: Option<PowerStats>,
    pub avg_temp: Option<f32>,
    /// Secondes passées dans chaque zone (5 zones FC, 7 zones de puissance), si calculable.
    pub hr_zones: Option<Vec<f32>>,
    pub power_zones: Option<Vec<f32>>,
    /// Meilleure puissance moyenne pour chaque durée de `BEST_DURATIONS` (None si sortie trop courte).
    pub best_power: Vec<(u32, Option<f32>)>,
    pub climbs: Vec<Climb>,
    /// Dérive cardiaque (%) entre 1ʳᵉ et 2ᵉ moitié, à puissance égale.
    pub decoupling: Option<f32>,
    /// FTP utilisée pour IF, TSS et zones, et sa provenance (« profil », « compteur »).
    pub ftp_used: Option<f32>,
    pub ftp_source: Option<String>,
    /// Plancher de FTP d'après le meilleur effort de 20 min (sortie d'au moins 40 min).
    pub estimated_ftp: Option<f32>,
    /// Vitesse moyenne sur le plat (pente < 1,5 %) en pédalant (km/h).
    pub flat_speed_kmh: Option<f32>,
    /// Part du temps en roue libre sur le plat (%), si la cadence est enregistrée.
    pub flat_coasting_pct: Option<f32>,
    pub advice: Vec<Advice>,
    pub series: Series,
}

fn mean(v: impl Iterator<Item = f32>) -> Option<f32> {
    let (s, n) = v.filter(|x| x.is_finite()).fold((0.0f64, 0usize), |(s, n), x| (s + x as f64, n + 1));
    (n > 0).then(|| (s / n as f64) as f32)
}

/// Durée représentée par chaque point (s) : écart avec le suivant, 0 si c'est une pause.
fn sample_durations(t: &[f32]) -> Vec<f32> {
    let n = t.len();
    (0..n)
        .map(|i| {
            if i + 1 >= n {
                return 1.0;
            }
            let dt = t[i + 1] - t[i];
            if dt > 0.0 && dt <= PAUSE_GAP {
                dt
            } else {
                0.0
            }
        })
        .collect()
}

/// Série « une valeur par seconde enregistrée » (pauses retirées) : base des calculs de puissance.
fn per_second(t: &[f32], v: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(t.last().copied().unwrap_or(0.0) as usize + 1);
    for i in 0..t.len() {
        let dt = if i + 1 < t.len() { t[i + 1] - t[i] } else { 1.0 };
        if !(dt > 0.0 && dt <= PAUSE_GAP) {
            continue;
        }
        let x = if v[i].is_finite() { v[i] } else { 0.0 };
        for _ in 0..(dt.round().max(1.0) as usize) {
            out.push(x);
        }
    }
    out
}

/// Meilleure moyenne glissante sur `w` secondes.
fn best_mean(s: &[f32], w: usize) -> Option<f32> {
    if w == 0 || s.len() < w {
        return None;
    }
    let mut sum: f64 = s[..w].iter().map(|&x| x as f64).sum();
    let mut best = sum;
    for i in w..s.len() {
        sum += s[i] as f64 - s[i - w] as f64;
        best = best.max(sum);
    }
    Some((best / w as f64) as f32)
}

fn normalized_power(s: &[f32]) -> Option<f32> {
    if s.len() < 30 {
        return None;
    }
    let mut sum: f64 = s[..30].iter().map(|&x| x as f64).sum();
    let mut acc = 0.0f64;
    let mut n = 0usize;
    for i in 30..=s.len() {
        let avg = sum / 30.0;
        acc += avg.powi(4);
        n += 1;
        if i < s.len() {
            sum += s[i] as f64 - s[i - 30] as f64;
        }
    }
    Some((acc / n as f64).powf(0.25) as f32)
}

/// Altitude lissée (moyenne sur ± `k` points, valeurs absentes ignorées).
pub fn smooth_elevation(ele: &[f32], k: usize) -> Vec<f32> {
    let n = ele.len();
    (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(k), (i + k + 1).min(n));
            mean(ele[a..b].iter().copied()).unwrap_or(f32::NAN)
        })
        .collect()
}

/// D+ et D− d'une série d'altitudes du compteur. Comme le compteur, on ne compte pas
/// les variations pendant les pauses (trou de plus de 10 s entre deux points).
pub fn activity_ascent(t: &[f32], ele: &[f32], smooth: usize, threshold: f64) -> (f64, f64) {
    let sm = smooth_elevation(ele, smooth);
    let (mut up, mut down) = (0.0, 0.0);
    let mut part: Vec<f64> = Vec::new();
    for i in 0..sm.len() {
        if i > 0 && t[i] - t[i - 1] > PAUSE_GAP {
            let (u, d) = ascent_descent(&part, threshold);
            up += u;
            down += d;
            part.clear();
        }
        if sm[i].is_finite() {
            part.push(sm[i] as f64);
        }
    }
    let (u, d) = ascent_descent(&part, threshold);
    (up + u, down + d)
}

fn category(length_m: f32, grade: f32) -> String {
    let score = length_m * grade; // longueur (m) × pente (%)
    match score {
        s if s >= 80_000.0 => "HC",
        s if s >= 64_000.0 => "1",
        s if s >= 32_000.0 => "2",
        s if s >= 16_000.0 => "3",
        s if s >= 8_000.0 => "4",
        _ => "",
    }
    .to_string()
}

/// Montées : d'un creux à un sommet, au moins 20 m de gain, 300 m de long et 3 % de moyenne.
fn detect_climbs(a: &Activity, moving: &[bool]) -> Vec<Climb> {
    // Altitude rééchantillonnée tous les 10 m de distance
    const STEP: f32 = 10.0;
    let sm = smooth_elevation(&a.ele, ELE_SMOOTH);
    let total = *a.dist.last().unwrap_or(&0.0);
    if total < 500.0 || !Activity::has(&sm) {
        return Vec::new();
    }
    let mut dk = Vec::new();
    let mut ek = Vec::new();
    let mut j = 0;
    let mut d = 0.0;
    while d <= total {
        while j + 1 < a.len() && a.dist[j + 1] < d {
            j += 1;
        }
        if sm[j].is_finite() {
            dk.push(d);
            ek.push(sm[j]);
        }
        d += STEP;
    }
    if ek.len() < 3 {
        return Vec::new();
    }

    let mut climbs = Vec::new();
    let (mut lo, mut hi) = (0usize, 0usize);
    let close = |lo: usize, hi: usize, out: &mut Vec<(usize, usize)>| {
        // La montée commence là où la route se met vraiment à grimper (pas au début du plat
        // qui précède) et s'arrête au premier point proche du sommet.
        let lo = (lo..=hi).rev().find(|&k| ek[k] <= ek[lo] + 2.0).unwrap_or(lo);
        let hi = (lo..=hi).find(|&k| ek[k] >= ek[hi] - 2.0).unwrap_or(hi);
        let gain = ek[hi] - ek[lo];
        let len = dk[hi] - dk[lo];
        if gain >= 20.0 && len >= 300.0 && gain / len >= 0.03 {
            out.push((lo, hi));
        }
    };
    let mut found = Vec::new();
    for k in 1..ek.len() {
        if ek[k] < ek[lo] && hi == lo {
            lo = k;
            hi = k;
        } else if ek[k] > ek[hi] {
            hi = k;
        } else {
            let gain = ek[hi] - ek[lo];
            let tolerance = (0.2 * gain).max(8.0);
            if ek[hi] - ek[k] > tolerance {
                close(lo, hi, &mut found);
                lo = k;
                hi = k;
            } else if ek[k] < ek[lo] {
                lo = k;
                hi = k;
            }
        }
    }
    close(lo, hi, &mut found);

    // Statistiques de chaque montée sur les points du fichier
    let index_at = |d: f32| a.dist.partition_point(|&x| x < d).min(a.len() - 1);
    for (lo, hi) in found {
        let (i0, i1) = (index_at(dk[lo]), index_at(dk[hi]));
        if i1 <= i0 {
            continue;
        }
        let gain = ek[hi] - ek[lo];
        let length = dk[hi] - dk[lo];
        // Temps de montée sans les arrêts (pauses du compteur exclues)
        let dur = sample_durations(&a.t);
        let duration: f32 = (i0..i1).map(|i| dur[i]).sum();
        let pos = |i: usize| -> Option<[f32; 2]> {
            let near = (0..a.len()).map(|k| (k as isize - i as isize).unsigned_abs()).zip(0..a.len());
            near.filter(|&(_, k)| a.lon[k].is_finite() && a.lat[k].is_finite())
                .min_by_key(|&(d, _)| d)
                .filter(|&(d, _)| d < 30)
                .map(|(_, k)| [a.lon[k], a.lat[k]])
        };
        let grade = gain / length * 100.0;
        let pick = |v: &[f32], positive: bool| {
            mean((i0..=i1).filter(|&i| moving[i]).map(|i| v[i]).filter(|x| !positive || *x > 0.0))
        };
        climbs.push(Climb {
            start_km: dk[lo] / 1000.0,
            end_km: dk[hi] / 1000.0,
            length_m: length,
            gain_m: gain,
            grade,
            duration_s: duration,
            vam: if duration > 0.0 { gain / duration * 3600.0 } else { 0.0 },
            category: category(length, grade),
            avg_power: pick(&a.power, false).filter(|_| Activity::has(&a.power)),
            avg_hr: pick(&a.hr, true),
            avg_cadence: pick(&a.cad, true),
            start: pos(i0),
            end: pos(i1),
        });
    }
    climbs
}

fn zones(values: &[f32], weights: &[f32], bounds: &[f32], scale: f32) -> Vec<f32> {
    let mut z = vec![0.0; bounds.len() + 1];
    for (v, w) in values.iter().zip(weights) {
        if !v.is_finite() || *w <= 0.0 {
            continue;
        }
        let k = bounds.iter().position(|b| *v < b * scale).unwrap_or(bounds.len());
        z[k] += w;
    }
    z
}

fn downsample(a: &Activity) -> Series {
    let total = *a.dist.last().unwrap_or(&0.0);
    let buckets = CHART_POINTS.min(a.len()).max(1);
    let width = (total / buckets as f32).max(1.0);
    let mut s = Series::default();
    let mut i = 0;
    for b in 0..buckets {
        let end = (b + 1) as f32 * width;
        let start = i;
        while i < a.len() && (a.dist[i] < end || b == buckets - 1) {
            i += 1;
        }
        if i == start {
            continue;
        }
        let r = start..i;
        let m = |v: &[f32]| mean(v[r.clone()].iter().copied()).unwrap_or(f32::NAN);
        s.dist_km.push(a.dist[i - 1] / 1000.0);
        s.time_s.push(a.t[i - 1]);
        s.ele.push(m(&a.ele));
        s.speed_kmh.push(m(&a.speed) * 3.6);
        s.hr.push(m(&a.hr));
        s.power.push(m(&a.power));
        s.cadence.push(mean(a.cad[r.clone()].iter().copied().filter(|c| *c > 0.0)).unwrap_or(f32::NAN));
    }
    s
}

/// Pente (fraction) en chaque point, mesurée sur ± 50 m de distance.
fn grades(a: &Activity) -> Vec<f32> {
    let n = a.len();
    let sm = smooth_elevation(&a.ele, ELE_SMOOTH);
    let (mut lo, mut hi) = (0usize, 0usize);
    (0..n)
        .map(|i| {
            while lo < i && a.dist[i] - a.dist[lo] > 50.0 {
                lo += 1;
            }
            hi = hi.max(i);
            while hi + 1 < n && a.dist[hi] - a.dist[i] < 50.0 {
                hi += 1;
            }
            let dd = a.dist[hi] - a.dist[lo];
            if dd > 20.0 && sm[hi].is_finite() && sm[lo].is_finite() {
                (sm[hi] - sm[lo]) / dd
            } else {
                f32::NAN
            }
        })
        .collect()
}

pub fn analyze(a: &Activity, athlete: &Athlete) -> Analysis {
    // FTP : celle du profil, sinon celle réglée sur le compteur
    let (ftp_used, ftp_source) = match (athlete.ftp.filter(|f| *f > 0.0), a.device.ftp.filter(|f| *f > 0.0)) {
        (Some(f), _) => (Some(f), Some("profil".to_string())),
        (None, Some(f)) => (Some(f as f32), Some("compteur".to_string())),
        _ => (None, None),
    };
    let athlete = &Athlete { ftp: ftp_used, ..*athlete };
    let n = a.len();
    let dur = sample_durations(&a.t);
    let moving: Vec<bool> = (0..n).map(|i| dur[i] > 0.0 && a.speed[i].is_finite() && a.speed[i] > MOVING_SPEED).collect();
    let moving_s: f32 = (0..n).filter(|&i| moving[i]).map(|i| dur[i]).sum();
    let distance_m = *a.dist.last().unwrap_or(&0.0) as f64;
    let elapsed_s = *a.t.last().unwrap_or(&0.0);
    let speed5 = smooth_elevation(&a.speed, 2); // même lissage, appliqué à la vitesse
    let max_speed = speed5.iter().copied().filter(|v| v.is_finite()).fold(0.0f32, f32::max);

    // Relief
    let (up, down) = activity_ascent(&a.t, &a.ele, ELE_SMOOTH, ELE_THRESHOLD);
    let ele_ok: Vec<f32> = a.ele.iter().copied().filter(|v| v.is_finite()).collect();
    let device_up = a.device.ascent_m;

    // Fréquence cardiaque, cadence, température (en mouvement)
    let on_move = |v: &[f32], positive: bool| {
        mean((0..n).filter(|&i| moving[i]).map(|i| v[i]).filter(|x| !positive || *x > 0.0))
    };
    let avg_hr = on_move(&a.hr, true);
    let max_hr = a.hr.iter().copied().filter(|v| v.is_finite()).reduce(f32::max);
    let avg_cadence = on_move(&a.cad, true);

    // Puissance
    let has_power = Activity::has(&a.power) && a.power.iter().any(|p| *p > 0.0);
    let p1 = if has_power { per_second(&a.t, &a.power) } else { Vec::new() };
    let power = if has_power && p1.len() >= 30 {
        let avg = p1.iter().map(|&x| x as f64).sum::<f64>() / p1.len() as f64;
        let np = normalized_power(&p1).unwrap_or(avg as f32);
        let intensity = athlete.ftp.filter(|f| *f > 0.0).map(|f| np / f);
        Some(PowerStats {
            avg: avg as f32,
            max: p1.iter().copied().fold(0.0, f32::max),
            np,
            vi: if avg > 0.0 { np / avg as f32 } else { 1.0 },
            work_kj: (p1.iter().map(|&x| x as f64).sum::<f64>() / 1000.0) as f32,
            intensity,
            tss: intensity.map(|i| p1.len() as f32 / 3600.0 * i * i * 100.0),
            w_per_kg: athlete.weight.filter(|w| *w > 0.0).map(|w| avg as f32 / w),
        })
    } else {
        None
    };
    let best_power: Vec<(u32, Option<f32>)> =
        BEST_DURATIONS.iter().map(|&d| (d, if has_power { best_mean(&p1, d as usize) } else { None })).collect();
    let estimated_ftp = (moving_s >= 2400.0)
        .then(|| best_power.iter().find(|x| x.0 == 1200).and_then(|x| x.1).map(|p20| p20 * 0.95))
        .flatten();

    // Plat : vitesse en pédalant et part de roue libre
    let grade = grades(a);
    let flat: Vec<usize> = (0..n).filter(|&i| moving[i] && grade[i].is_finite() && grade[i].abs() < 0.015).collect();
    let has_cad = Activity::has(&a.cad);
    let flat_pedal: Vec<usize> = flat.iter().copied().filter(|&i| !has_cad || a.cad[i] > 0.0).collect();
    let flat_time: f32 = flat_pedal.iter().map(|&i| dur[i]).sum();
    let flat_speed_kmh = (flat_time >= 300.0).then(|| {
        let d: f32 = flat_pedal.iter().map(|&i| a.speed[i] * dur[i]).sum();
        d / flat_time * 3.6
    });
    let flat_coasting_pct = (has_cad && flat.len() > 300).then(|| {
        let total: f32 = flat.iter().map(|&i| dur[i]).sum();
        let coast: f32 = flat.iter().filter(|&&i| a.cad[i] == 0.0).map(|&i| dur[i]).sum();
        coast / total.max(1.0) * 100.0
    });

    // Zones
    let hr_max = athlete.hr_max.or(max_hr);
    let hr_zones = (Activity::has(&a.hr) && hr_max.is_some()).then(|| {
        let w: Vec<f32> = (0..n).map(|i| if moving[i] { dur[i] } else { 0.0 }).collect();
        zones(&a.hr, &w, &HR_ZONES, hr_max.unwrap())
    });
    let power_zones = athlete.ftp.filter(|f| *f > 0.0 && has_power).map(|ftp| zones(&p1, &vec![1.0; p1.len()], &POWER_ZONES, ftp));

    // Dérive cardiaque : rapport puissance / FC, 2ᵉ moitié vs 1ʳᵉ (sorties d'au moins 1 h)
    let decoupling = if has_power && Activity::has(&a.hr) && moving_s >= 3600.0 {
        let idx: Vec<usize> = (0..n).filter(|&i| moving[i] && a.hr[i] > 0.0 && a.power[i].is_finite()).collect();
        let half = idx.len() / 2;
        let ratio = |r: &[usize]| {
            let p = mean(r.iter().map(|&i| a.power[i]))?;
            let h = mean(r.iter().map(|&i| a.hr[i]))?;
            (h > 0.0).then_some(p / h)
        };
        match (ratio(&idx[..half]), ratio(&idx[half..])) {
            (Some(r1), Some(r2)) if r1 > 0.0 => Some((r1 - r2) / r1 * 100.0),
            _ => None,
        }
    } else {
        None
    };

    let climbs = detect_climbs(a, &moving);
    let mut out = Analysis {
        distance_m,
        elapsed_s,
        moving_s,
        avg_speed_kmh: if moving_s > 0.0 { (distance_m / moving_s as f64 * 3.6) as f32 } else { 0.0 },
        max_speed_kmh: max_speed * 3.6,
        ascent_m: device_up.unwrap_or(up),
        descent_m: a.device.descent_m.unwrap_or(down),
        ascent_computed_m: up,
        ascent_device_m: device_up,
        ele_min: ele_ok.iter().copied().reduce(f32::min),
        ele_max: ele_ok.iter().copied().reduce(f32::max),
        avg_hr,
        max_hr,
        avg_cadence,
        power,
        avg_temp: mean(a.temp.iter().copied()),
        hr_zones,
        power_zones,
        best_power,
        climbs,
        decoupling,
        ftp_used,
        ftp_source,
        estimated_ftp,
        flat_speed_kmh,
        flat_coasting_pct,
        advice: Vec::new(),
        series: downsample(a),
    };
    out.advice = advise(a, &out, athlete, &moving, &grade);
    out
}

// --- Conseils par règles ----------------------------------------------------------

fn advise(a: &Activity, r: &Analysis, athlete: &Athlete, moving: &[bool], grade: &[f32]) -> Vec<Advice> {
    let mut v = Vec::new();
    let mut add = |kind, title: &str, text: String| v.push(Advice { kind, title: title.to_string(), text });
    let fmt = |x: f32| format!("{x:.0}");

    // Cadence en montée et sur le plat
    if Activity::has(&a.cad) {
        let long_climbs: Vec<&Climb> = r.climbs.iter().filter(|c| c.duration_s >= 120.0).collect();
        let low: Vec<&&Climb> = long_climbs.iter().filter(|c| c.avg_cadence.is_some_and(|x| x < 70.0)).collect();
        if !low.is_empty() {
            let worst = low.iter().filter_map(|c| c.avg_cadence).fold(f32::MAX, f32::min);
            add(
                "tip",
                "Cadence basse en montée",
                format!(
                    "{} montée(s) à moins de 70 tr/min (jusqu'à {} tr/min). Mouliner plus vite (75 à 90 tr/min), avec un braquet plus petit, épargne les muscles et retarde la fatigue.",
                    low.len(),
                    fmt(worst)
                ),
            );
        } else if !long_climbs.is_empty() && long_climbs.iter().all(|c| c.avg_cadence.is_some()) {
            add("good", "Bonne cadence en montée", "Tu as gardé plus de 70 tr/min dans les montées : c'est efficace.".into());
        }
        if let Some(c) = r.avg_cadence {
            if c < 78.0 {
                add(
                    "tip",
                    "Pour aller plus vite : pédaler plus vite",
                    format!(
                        "{} tr/min en moyenne. Viser 85 à 90 tr/min, avec un braquet un peu plus léger, fatigue moins les muscles : tu tiens une vitesse plus élevée plus longtemps. Habitue-toi par blocs de 5 min à 90 tr/min dans tes sorties.",
                        fmt(c)
                    ),
                );
            }
        }
        if let Some(cp) = r.flat_coasting_pct {
            if cp > 8.0 {
                add(
                    "tip",
                    "Moins de roue libre sur le plat",
                    format!(
                        "Tu es en roue libre {} % du temps sur le plat. Garder un coup de pédale léger mais continu (même 60 % de ton effort) évite de perdre de la vitesse à relancer.",
                        fmt(cp)
                    ),
                );
            }
        }
    }

    // Gestion de l'effort : 1ʳᵉ moitié vs 2ᵉ moitié
    if r.moving_s >= 2400.0 {
        let idx: Vec<usize> = (0..a.len()).filter(|&i| moving[i]).collect();
        let half = idx.len() / 2;
        let series: Option<(&[f32], &str)> = if r.power.is_some() {
            Some((&a.power, "puissance"))
        } else if Activity::has(&a.hr) {
            Some((&a.hr, "fréquence cardiaque"))
        } else {
            None
        };
        if let Some((s, label)) = series {
            let m1 = mean(idx[..half].iter().map(|&i| s[i]));
            let m2 = mean(idx[half..].iter().map(|&i| s[i]));
            if let (Some(m1), Some(m2)) = (m1, m2) {
                if label == "puissance" && m2 < 0.88 * m1 {
                    add(
                        "warning",
                        "Départ trop rapide",
                        format!(
                            "Ta puissance a chuté de {} % en 2ᵉ moitié ({} W → {} W). Partir 5 à 10 % plus doucement permet souvent de finir plus fort.",
                            fmt((1.0 - m2 / m1) * 100.0),
                            fmt(m1),
                            fmt(m2)
                        ),
                    );
                } else if label == "puissance" && m2 >= 0.97 * m1 {
                    add("good", "Effort bien géré", "Ta puissance est restée stable jusqu'au bout : bonne gestion de l'effort.".into());
                }
            }
        }
    }

    // Vitesse sur le plat : 1ʳᵉ moitié vs 2ᵉ moitié (sorties d'au moins 1 h)
    if r.moving_s >= 3600.0 {
        let flat: Vec<usize> = (0..a.len())
            .filter(|&i| moving[i] && grade[i].is_finite() && grade[i].abs() < 0.015 && (!Activity::has(&a.cad) || a.cad[i] > 0.0))
            .collect();
        let half_d = r.distance_m as f32 / 2.0;
        let speed = |first: bool| mean(flat.iter().filter(|&&i| (a.dist[i] < half_d) == first).map(|&i| a.speed[i] * 3.6));
        if let (Some(s1), Some(s2)) = (speed(true), speed(false)) {
            if s2 < 0.93 * s1 {
                add(
                    "warning",
                    "Baisse de régime en fin de sortie",
                    format!(
                        "Sur le plat, tu roulais à {} km/h en 1ʳᵉ moitié et {} km/h en 2ᵉ ({} %). Mange un peu toutes les 45 min, bois régulièrement, et pars légèrement moins vite : la moyenne finale sera meilleure.",
                        format!("{s1:.1}").replace('.', ","),
                        format!("{s2:.1}").replace('.', ","),
                        fmt((s2 / s1 - 1.0) * 100.0)
                    ),
                );
            } else if s2 >= 0.99 * s1 {
                add("good", "Vitesse tenue jusqu'au bout", format!("Ta vitesse sur le plat n'a pas faibli en 2ᵉ moitié ({} km/h) : bonne gestion.", format!("{s2:.1}").replace('.', ",")));
            }
        }
    }

    // Dérive cardiaque
    if let Some(d) = r.decoupling {
        if d > 5.0 {
            add(
                "tip",
                "Endurance de base à développer",
                format!(
                    "À puissance égale, ton cœur a battu {} % plus vite en 2ᵉ moitié. Des sorties longues en zone 2 (aisance respiratoire) réduisent cette dérive.",
                    fmt(d)
                ),
            );
        } else if d >= 0.0 {
            add("good", "Bonne endurance", format!("Dérive cardiaque de {} % seulement : ta base aérobie tient bien la durée.", fmt(d)));
        }
    }

    // Régularité (VI)
    if let Some(p) = &r.power {
        if p.vi > 1.15 && r.moving_s >= 1800.0 {
            add(
                "tip",
                "Effort irrégulier",
                format!(
                    "Indice de variabilité de {:.2} : beaucoup d'à-coups. Sur le plat, lisser l'effort (relances plus douces, anticiper) économise de l'énergie.",
                    p.vi
                ),
            );
        }
    }

    // Zone grise (zone 3 FC)
    if let Some(z) = &r.hr_zones {
        let total: f32 = z.iter().sum();
        if total > 1800.0 && z[2] / total > 0.4 {
            add(
                "info",
                "Beaucoup de temps en zone 3",
                format!(
                    "{} % du temps en zone 3 (« tempo »). Pour progresser, alterne sorties vraiment faciles (zone 2) et séances intenses courtes.",
                    fmt(z[2] / total * 100.0)
                ),
            );
        }
    }

    // Montées trop appuyées
    if let (Some(ftp), true) = (athlete.ftp, r.power.is_some()) {
        if let Some(c) = r.climbs.iter().find(|c| c.duration_s >= 300.0 && c.avg_power.is_some_and(|p| p > 1.05 * ftp)) {
            add(
                "warning",
                "Montée au-dessus du seuil",
                format!(
                    "Montée du km {:.1} : {} W de moyenne pendant {} min, au-delà de ta FTP ({} W). Sur une longue sortie, monter à 85-95 % de la FTP évite de s'entamer.",
                    c.start_km,
                    fmt(c.avg_power.unwrap()),
                    fmt(c.duration_s / 60.0),
                    fmt(ftp)
                ),
            );
        }
    }

    // Séance pour progresser (à partir de la FTP si on la connaît)
    if r.moving_s >= 1800.0 {
        let text = match athlete.ftp {
            Some(f) => format!(
                "Une fois par semaine : 2 × 20 min à {}–{} W (88 à 93 % de ta FTP de {} W), 5 min de récupération entre les deux. C'est la séance la plus efficace pour relever ta vitesse de croisière.",
                fmt(f * 0.88),
                fmt(f * 0.93),
                fmt(f)
            ),
            None => "Une fois par semaine : 2 × 20 min à un effort « soutenu mais tenable » (tu peux dire quelques mots, pas tenir une conversation), 5 min de récupération entre les deux. C'est la séance la plus efficace pour relever ta vitesse de croisière.".into(),
        };
        add("tip", "Séance pour aller plus vite", text);
    }

    // Mise à jour du profil
    if let Some(est) = r.estimated_ftp {
        match athlete.ftp {
            Some(f) if est > f * 1.03 => add(
                "good",
                "FTP en hausse ?",
                format!("Ton meilleur effort de 20 min indique une FTP d'au moins {} W (réglée à {} W). Pense à la mettre à jour.", fmt(est), fmt(f)),
            ),
            None => add(
                "info",
                "FTP estimée",
                format!("D'après ton meilleur effort de 20 min, ta FTP est d'au moins {} W. Renseigne-la dans ton profil pour les zones et le TSS.", fmt(est)),
            ),
            _ => {}
        }
    }
    if let (Some(m), Some(set)) = (r.max_hr, athlete.hr_max) {
        if m > set + 1.0 {
            add("info", "FC max dépassée", format!("FC maximale de {} bpm sur cette sortie, au-dessus de celle du profil ({} bpm) : mets-la à jour.", fmt(m), fmt(set)));
        }
    }

    // Données manquantes
    let missing: Vec<&str> = [(Activity::has(&a.hr), "cardio"), (r.power.is_some(), "puissance"), (Activity::has(&a.cad), "cadence")]
        .iter()
        .filter(|x| !x.0)
        .map(|x| x.1)
        .collect();
    if !missing.is_empty() {
        add("info", "Analyse partielle", format!("Pas de données de {} dans ce fichier : certains conseils ne sont pas disponibles.", missing.join(", ")));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sortie fictive d'une heure à 1 Hz : 30 km/h à plat, puis une montée de 2 km à 5 %.
    fn ride() -> Activity {
        let mut a = Activity { format: "test".into(), ..Default::default() };
        let mut d = 0.0f32;
        for s in 0..3600 {
            let climbing = s >= 2400 && s < 2400 + 480; // 8 min à 15 km/h = 2 km
            let v = if climbing { 15.0 / 3.6 } else { 30.0 / 3.6 };
            d += v;
            let ele = if s < 2400 { 50.0 } else if climbing { 50.0 + (s - 2400) as f32 / 480.0 * 100.0 } else { 150.0 };
            a.t.push(s as f32);
            a.dist.push(d);
            a.speed.push(v);
            a.ele.push(ele + if s % 2 == 0 { 0.3 } else { -0.3 });
            a.hr.push(if climbing { 165.0 } else { 140.0 });
            a.cad.push(if climbing { 65.0 } else { 90.0 });
            a.power.push(if climbing { 280.0 } else { 200.0 });
            a.lon.push(f32::NAN);
            a.lat.push(f32::NAN);
            a.temp.push(20.0);
        }
        a
    }

    #[test]
    fn core_metrics() {
        let r = analyze(&ride(), &Athlete { ftp: Some(250.0), hr_max: Some(185.0), weight: Some(60.0) });
        assert!((r.moving_s - 3599.0).abs() < 2.0);
        assert!((r.ascent_m - 100.0).abs() < 3.0, "D+ {}", r.ascent_m);
        let p = r.power.as_ref().unwrap();
        assert!((p.avg - 210.7).abs() < 1.0, "moyenne {}", p.avg);
        assert!(p.np > p.avg && p.np < 230.0, "NP {}", p.np);
        assert!((p.tss.unwrap() - 100.0 * (p.np / 250.0).powi(2)).abs() < 1.0);
        assert_eq!(r.best_power.iter().find(|x| x.0 == 300).unwrap().1, Some(280.0));
        assert_eq!(r.best_power.iter().find(|x| x.0 == 3600).unwrap().1.is_some(), true);
        // Zones : 8 min de montée à 165 bpm (89 % de 185) → zone 4
        let z = r.hr_zones.unwrap();
        assert!((z[3] - 480.0).abs() < 2.0, "{z:?}");
        // Puissance : 200 W = 80 % de la FTP → zone 3 ; 280 W = 112 % → zone 5 (105-120 %)
        let pz = r.power_zones.unwrap();
        assert!((pz[4] - 480.0).abs() < 2.0 && (pz[2] - 3120.0).abs() < 2.0, "{pz:?}");
    }

    #[test]
    fn climb_detection_and_advice() {
        let r = analyze(&ride(), &Athlete { ftp: Some(250.0), hr_max: Some(185.0), weight: None });
        assert_eq!(r.climbs.len(), 1, "{:?}", r.climbs);
        let c = &r.climbs[0];
        assert!((c.gain_m - 100.0).abs() < 5.0 && (c.grade - 5.0).abs() < 0.6, "{c:?}");
        assert!((c.vam - 750.0).abs() < 60.0, "VAM {}", c.vam);
        assert_eq!(c.avg_cadence.map(|x| x.round()), Some(65.0));
        let titles: Vec<&str> = r.advice.iter().map(|a| a.title.as_str()).collect();
        assert!(titles.contains(&"Cadence basse en montée"), "{titles:?}");
        assert!(titles.contains(&"Montée au-dessus du seuil"), "{titles:?}");
    }

    #[test]
    fn pauses_are_excluded() {
        let mut a = ride();
        for t in a.t.iter_mut().skip(1800) {
            *t += 600.0; // pause café de 10 min au milieu
        }
        let r = analyze(&a, &Athlete::default());
        assert!((r.moving_s - 3599.0).abs() < 2.0);
        assert!((r.elapsed_s - 4199.0).abs() < 1.0);
    }

    #[test]
    fn best_mean_and_np() {
        let s: Vec<f32> = (0..120).map(|i| if i < 60 { 100.0 } else { 300.0 }).collect();
        assert_eq!(best_mean(&s, 60), Some(300.0));
        assert_eq!(best_mean(&s, 200), None);
        let flat = vec![200.0; 600];
        assert!((normalized_power(&flat).unwrap() - 200.0).abs() < 0.01);
    }
}

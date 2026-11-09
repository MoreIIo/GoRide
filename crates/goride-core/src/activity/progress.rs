//! Progression dans le temps : charge d'entraînement (forme, fatigue, fraîcheur),
//! bilans par semaine et tendance de la vitesse sur le plat.
//!
//! Méthode « Performance Manager » (TrainingPeaks) :
//! - TSS de chaque sortie (puissance, sinon cardio, sinon estimation) ;
//! - forme (CTL) = moyenne exponentielle du TSS quotidien sur 42 jours ;
//! - fatigue (ATL) = idem sur 7 jours ;
//! - fraîcheur (TSB) = forme − fatigue de la veille.

use super::analysis::Athlete;
use serde::Serialize;

/// Ce qu'il faut savoir d'une sortie pour la progression (stocké dans la bibliothèque).
#[derive(Debug, Clone)]
pub struct RideLoad {
    pub id: i64,
    pub name: String,
    pub start_time: i64,
    pub moving_s: f64,
    pub distance_m: f64,
    pub ascent_m: f64,
    pub np: Option<f64>,
    pub avg_hr: Option<f64>,
    pub flat_speed_kmh: Option<f64>,
    /// FTP réglée sur le compteur au moment de la sortie.
    pub device_ftp: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RidePoint {
    pub id: i64,
    pub name: String,
    pub start_time: i64,
    pub distance_km: f64,
    pub moving_h: f64,
    pub ascent_m: f64,
    pub flat_speed_kmh: Option<f64>,
    pub tss: f64,
    /// « puissance », « cardio » ou « estimation ».
    pub tss_method: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayLoad {
    /// Jour (secondes Unix à minuit, heure locale).
    pub day: i64,
    pub tss: f64,
    pub fitness: f64,
    pub fatigue: f64,
    pub form: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Week {
    /// Lundi de la semaine (secondes Unix à minuit, heure locale).
    pub start: i64,
    pub distance_km: f64,
    pub moving_h: f64,
    pub ascent_m: f64,
    pub rides: u32,
    pub tss: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub rides: Vec<RidePoint>,
    pub days: Vec<DayLoad>,
    pub weeks: Vec<Week>,
    /// Évolution de la vitesse sur le plat (km/h par mois), d'après la tendance des sorties.
    pub flat_speed_trend_per_month: Option<f64>,
    /// Part des sorties dont le TSS est estimé (ni puissance ni cardio).
    pub estimated_share: f64,
}

const DAY: i64 = 86_400;

/// TSS d'une sortie et méthode employée.
pub fn ride_tss(r: &RideLoad, athlete: &Athlete) -> (f64, &'static str) {
    let h = r.moving_s / 3600.0;
    let ftp = athlete.ftp.map(|f| f as f64).or(r.device_ftp).filter(|f| *f > 0.0);
    if let (Some(np), Some(ftp)) = (r.np, ftp) {
        let i = np / ftp;
        return (h * i * i * 100.0, "puissance");
    }
    if let (Some(hr), Some(max)) = (r.avg_hr, athlete.hr_max) {
        // Seuil cardiaque ≈ 90 % de la FC max
        let i = hr / (0.9 * max as f64);
        return (h * i * i * 100.0, "cardio");
    }
    // Sans capteur : sortie d'endurance (≈ 45 TSS/h), plus le dénivelé
    (h * 45.0 + r.ascent_m / 100.0 * 4.0, "estimation")
}

/// Pente et ordonnée de la droite des moindres carrés.
pub fn linear_trend(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = points.len() as f64;
    if points.len() < 3 {
        return None;
    }
    let (sx, sy) = points.iter().fold((0.0, 0.0), |(a, b), p| (a + p.0, b + p.1));
    let (mx, my) = (sx / n, sy / n);
    let sxx: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    if sxx <= 0.0 {
        return None;
    }
    let sxy: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let slope = sxy / sxx;
    Some((slope, my - slope * mx))
}

/// Calcule la progression. `tz_offset_s` : décalage de l'heure locale (Paris en hiver = 3 600),
/// `now` : instant présent (secondes Unix) pour prolonger les courbes jusqu'à aujourd'hui.
pub fn progress(rides: &[RideLoad], athlete: &Athlete, tz_offset_s: i64, now: i64) -> Progress {
    let local_day = |t: i64| (t + tz_offset_s).div_euclid(DAY) * DAY - tz_offset_s;
    let mut points: Vec<RidePoint> = rides
        .iter()
        .map(|r| {
            let (tss, method) = ride_tss(r, athlete);
            RidePoint {
                id: r.id,
                name: r.name.clone(),
                start_time: r.start_time,
                distance_km: r.distance_m / 1000.0,
                moving_h: r.moving_s / 3600.0,
                ascent_m: r.ascent_m,
                flat_speed_kmh: r.flat_speed_kmh,
                tss,
                tss_method: method,
            }
        })
        .collect();
    points.sort_by_key(|p| p.start_time);

    let mut days = Vec::new();
    let mut weeks: Vec<Week> = Vec::new();
    if let Some(first) = points.first() {
        let start = local_day(first.start_time);
        let end = local_day(now.max(points.last().unwrap().start_time));
        let (mut ctl, mut atl) = (0.0f64, 0.0f64);
        let mut k = 0;
        let mut day = start;
        while day <= end {
            let mut tss = 0.0;
            while k < points.len() && local_day(points[k].start_time) == day {
                tss += points[k].tss;
                k += 1;
            }
            let form = ctl - atl; // fraîcheur du matin : bilan de la veille
            ctl += (tss - ctl) / 42.0;
            atl += (tss - atl) / 7.0;
            days.push(DayLoad { day, tss, fitness: ctl, fatigue: atl, form });
            day += DAY;
        }

        // Semaines du lundi au dimanche (le 1/1/1970 était un jeudi)
        let monday = |d: i64| {
            let days_since_epoch = (d + tz_offset_s).div_euclid(DAY);
            let weekday = (days_since_epoch + 3).rem_euclid(7); // 0 = lundi
            (days_since_epoch - weekday) * DAY - tz_offset_s
        };
        let mut w = monday(start);
        while w <= end {
            weeks.push(Week { start: w, distance_km: 0.0, moving_h: 0.0, ascent_m: 0.0, rides: 0, tss: 0.0 });
            w += 7 * DAY;
        }
        for p in &points {
            let idx = ((monday(local_day(p.start_time)) - weeks[0].start) / (7 * DAY)) as usize;
            if let Some(wk) = weeks.get_mut(idx) {
                wk.distance_km += p.distance_km;
                wk.moving_h += p.moving_h;
                wk.ascent_m += p.ascent_m;
                wk.rides += 1;
                wk.tss += p.tss;
            }
        }
    }

    // Tendance de la vitesse sur le plat (sorties d'au moins 30 min)
    let speed_pts: Vec<(f64, f64)> = points
        .iter()
        .filter(|p| p.moving_h >= 0.5)
        .filter_map(|p| p.flat_speed_kmh.map(|v| (p.start_time as f64 / (30.0 * DAY as f64), v)))
        .collect();
    let flat_speed_trend_per_month = linear_trend(&speed_pts).map(|(slope, _)| slope);
    let estimated_share = if points.is_empty() {
        0.0
    } else {
        points.iter().filter(|p| p.tss_method == "estimation").count() as f64 / points.len() as f64
    };

    Progress { rides: points, days, weeks, flat_speed_trend_per_month, estimated_share }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ride(id: i64, day: i64, hours: f64, np: Option<f64>, flat: f64) -> RideLoad {
        RideLoad {
            id,
            name: format!("Sortie {id}"),
            start_time: day * DAY + 9 * 3600,
            moving_s: hours * 3600.0,
            distance_m: hours * 25_000.0,
            ascent_m: 300.0,
            np,
            avg_hr: None,
            flat_speed_kmh: Some(flat),
            device_ftp: Some(200.0),
        }
    }

    #[test]
    fn tss_methods() {
        let a = Athlete::default();
        let (tss, m) = ride_tss(&ride(1, 0, 1.0, Some(200.0), 25.0), &a);
        assert_eq!(m, "puissance");
        assert!((tss - 100.0).abs() < 1e-9); // 1 h à la FTP = 100 TSS
        let (tss, m) = ride_tss(&ride(1, 0, 2.0, None, 25.0), &a);
        assert_eq!(m, "estimation");
        assert!((tss - 102.0).abs() < 1e-9);
        let hr = RideLoad { avg_hr: Some(144.0), np: None, ..ride(1, 0, 1.0, None, 25.0) };
        let (tss, m) = ride_tss(&hr, &Athlete { hr_max: Some(200.0), ..a });
        assert_eq!(m, "cardio");
        assert!((tss - 64.0).abs() < 1e-6); // 144 / 180 = 0,8 → 64
    }

    #[test]
    fn fitness_fatigue_and_weeks() {
        // Une sortie d'1 h à la FTP chaque jour pendant 10 jours (du jeudi 1/1/1970 au samedi 10/1)
        let rides: Vec<RideLoad> = (0..10).map(|d| ride(d, d, 1.0, Some(200.0), 25.0)).collect();
        let p = progress(&rides, &Athlete::default(), 0, 12 * DAY);
        assert_eq!(p.days.len(), 13);
        let last_ride_day = &p.days[9];
        assert!(last_ride_day.fatigue > last_ride_day.fitness, "la fatigue monte plus vite que la forme");
        assert!(last_ride_day.form < 0.0);
        // Deux jours de repos : la fraîcheur remonte
        assert!(p.days[12].form > p.days[10].form);
        // Semaines : lundi 29/12/1969, puis 5/1 et 12/1
        assert_eq!(p.weeks.len(), 3);
        assert_eq!(p.weeks[0].rides, 4); // jeudi → dimanche
        assert_eq!(p.weeks[1].rides, 6);
        assert_eq!((p.weeks[0].start + 3 * DAY) % (7 * DAY), 0);
    }

    #[test]
    fn flat_speed_trend() {
        // +1 km/h tous les 30 jours
        let rides: Vec<RideLoad> = (0..6).map(|k| ride(k, k * 30, 1.0, None, 24.0 + k as f64)).collect();
        let p = progress(&rides, &Athlete::default(), 0, 200 * DAY);
        assert!((p.flat_speed_trend_per_month.unwrap() - 1.0).abs() < 1e-6);
        assert_eq!(p.estimated_share, 1.0);
    }

    #[test]
    fn local_days() {
        // Sortie à 23 h 30 heure de Paris (22 h 30 UTC) : même jour local
        let mut r = ride(1, 5, 1.0, None, 25.0);
        r.start_time = 5 * DAY + 22 * 3600 + 1800;
        let p = progress(&[r], &Athlete::default(), 3600, 5 * DAY);
        assert_eq!(p.days[0].day, 5 * DAY - 3600);
    }
}

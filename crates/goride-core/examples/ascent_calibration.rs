//! Calibrage du D+ (porte de la phase 4) : compare le D+ calculé par GoRide à celui
//! du compteur sur des fichiers FIT, pour plusieurs réglages de lissage et de seuil.
//! cargo run --release -p goride-core --example ascent_calibration -- data/sorties/*.fit
use goride_core::activity::analysis::{activity_ascent, analyze, Athlete, ELE_SMOOTH, ELE_THRESHOLD};
use goride_core::activity::parse_activity;

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let mut rides = Vec::new();
    for f in &files {
        let bytes = std::fs::read(f).expect("lecture");
        match parse_activity(&bytes, f) {
            Ok(a) => {
                let r = analyze(&a, &Athlete::default());
                println!(
                    "{f}: {:.1} km, {} pts, compteur D+ {:?} m, GoRide {:.0} m, FC {:?}, puissance {:?} W, {} montées",
                    r.distance_m / 1000.0,
                    a.len(),
                    a.device.ascent_m,
                    r.ascent_computed_m,
                    r.avg_hr.map(|x| x.round()),
                    r.power.as_ref().map(|p| p.avg.round()),
                    r.climbs.len()
                );
                if let Some(dev) = a.device.ascent_m {
                    rides.push((a, dev));
                }
            }
            Err(e) => println!("{f}: ERREUR {e}"),
        }
    }
    if rides.is_empty() {
        return;
    }
    println!("\nRéglage actuel : lissage ±{ELE_SMOOTH}, seuil {ELE_THRESHOLD} m");
    let mut best = (f64::MAX, 0, 0.0);
    for smooth in [0usize, 2, 3, 5, 8, 12] {
        for threshold in [0.5, 1.0, 1.5, 2.0, 3.0, 4.0] {
            let errs: Vec<f64> = rides
                .iter()
                .map(|(a, dev)| (activity_ascent(&a.t, &a.ele, smooth, threshold).0 - dev) / dev.max(1.0) * 100.0)
                .collect();
            let worst = errs.iter().fold(0.0f64, |m, e| m.max(e.abs()));
            let txt: Vec<String> = errs.iter().map(|e| format!("{e:+.1} %")).collect();
            println!("lissage ±{smooth:<2} seuil {threshold:.1} m : pire écart {worst:.1} % | {}", txt.join(" "));
            if worst < best.0 {
                best = (worst, smooth, threshold);
            }
        }
    }
    println!("\nMeilleur réglage : lissage ±{}, seuil {} m (pire écart {:.1} %)", best.1, best.2, best.0);
}

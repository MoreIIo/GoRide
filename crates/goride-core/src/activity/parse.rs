//! Lecture des fichiers de sortie : FIT (Garmin, Wahoo…), GPX et TCX.

use super::time::parse_iso8601;
use super::{Activity, DeviceSummary};
use crate::geo::haversine_m;
use fitparser::profile::MesgNum;
use fitparser::Value;

/// Détecte le format (contenu d'abord, extension ensuite) et lit la sortie.
pub fn parse_activity(bytes: &[u8], file_name: &str) -> Result<Activity, String> {
    let lower = file_name.to_ascii_lowercase();
    let is_fit = bytes.len() >= 12 && &bytes[8..12] == b".FIT";
    let mut a = if is_fit || lower.ends_with(".fit") {
        parse_fit(bytes)?
    } else {
        let text = std::str::from_utf8(bytes).map_err(|_| "fichier illisible (ni FIT, ni texte)".to_string())?;
        let text = text.trim_start_matches('\u{feff}');
        if text.contains("<TrainingCenterDatabase") {
            parse_tcx(text)?
        } else if text.contains("<gpx") {
            parse_gpx(text)?
        } else {
            return Err("format non reconnu : seuls les fichiers FIT, GPX et TCX sont acceptés".into());
        }
    };
    finish(&mut a)?;
    if a.name.is_none() {
        let stem = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
        a.name = Some(stem.rsplit_once('.').map_or(stem, |(s, _)| s).to_string());
    }
    Ok(a)
}

/// Point en cours de lecture, avant mise en séries.
#[derive(Default, Clone, Copy)]
struct Sample {
    time: f64,
    lon: f32,
    lat: f32,
    ele: f32,
    dist: f32,
    speed: f32,
    hr: f32,
    cad: f32,
    power: f32,
    temp: f32,
}

impl Sample {
    fn new(time: f64) -> Sample {
        let n = f32::NAN;
        Sample { time, lon: n, lat: n, ele: n, dist: n, speed: n, hr: n, cad: n, power: n, temp: n }
    }
}

fn to_series(samples: Vec<Sample>, a: &mut Activity) {
    let t0 = samples.first().map_or(0.0, |s| s.time);
    a.start_time = a.start_time.or(Some(t0.floor() as i64));
    for s in samples {
        a.t.push((s.time - t0) as f32);
        a.lon.push(s.lon);
        a.lat.push(s.lat);
        a.ele.push(s.ele);
        a.dist.push(s.dist);
        a.speed.push(s.speed);
        a.hr.push(s.hr);
        a.cad.push(s.cad);
        a.power.push(s.power);
        a.temp.push(s.temp);
    }
}

/// Compléments communs : distance et vitesse manquantes, contrôles.
fn finish(a: &mut Activity) -> Result<(), String> {
    if a.len() < 2 {
        return Err("la sortie ne contient pas assez de points".into());
    }
    // Distance : celle du compteur si elle existe, sinon calculée depuis les positions
    if !Activity::has(&a.dist) {
        let mut acc = 0.0f64;
        let mut last: Option<[f64; 2]> = None;
        for i in 0..a.len() {
            if a.lon[i].is_finite() && a.lat[i].is_finite() {
                let p = [a.lon[i] as f64, a.lat[i] as f64];
                if let Some(q) = last {
                    acc += haversine_m(q, p);
                }
                last = Some(p);
            }
            a.dist[i] = acc as f32;
        }
    } else {
        // Trous éventuels : on garde la dernière valeur connue
        let mut lastd = 0.0;
        for d in a.dist.iter_mut() {
            if d.is_finite() {
                lastd = *d;
            } else {
                *d = lastd;
            }
        }
    }
    // Vitesse : dérivée de la distance sur ~5 s si le fichier ne la donne pas
    if !Activity::has(&a.speed) {
        let n = a.len();
        for i in 0..n {
            let (lo, hi) = (i.saturating_sub(2), (i + 2).min(n - 1));
            let dt = a.t[hi] - a.t[lo];
            a.speed[i] = if dt > 0.0 { (a.dist[hi] - a.dist[lo]) / dt } else { 0.0 };
        }
    }
    Ok(())
}

// --- FIT ----------------------------------------------------------------------

fn num(v: &Value) -> Option<f64> {
    let x = match v {
        Value::Float64(x) => *x,
        Value::Float32(x) => *x as f64,
        Value::UInt8(x) | Value::UInt8z(x) | Value::Byte(x) | Value::Enum(x) => *x as f64,
        Value::SInt8(x) => *x as f64,
        Value::UInt16(x) | Value::UInt16z(x) => *x as f64,
        Value::SInt16(x) => *x as f64,
        Value::UInt32(x) | Value::UInt32z(x) => *x as f64,
        Value::SInt32(x) => *x as f64,
        Value::SInt64(x) => *x as f64,
        Value::UInt64(x) | Value::UInt64z(x) => *x as f64,
        _ => return None,
    };
    x.is_finite().then_some(x)
}

fn parse_fit(bytes: &[u8]) -> Result<Activity, String> {
    let msgs = fitparser::from_bytes(bytes).map_err(|e| format!("fichier FIT illisible : {e}"))?;
    let mut a = Activity { format: "fit".into(), ..Default::default() };
    let mut samples: Vec<Sample> = Vec::new();
    let mut dev = DeviceSummary::default();
    let semicircle = 180.0 / 2f64.powi(31);

    for m in &msgs {
        let get = |name: &str| m.fields().iter().find(|f| f.name() == name).map(|f| f.value());
        match m.kind() {
            MesgNum::Record => {
                let Some(Value::Timestamp(ts)) = get("timestamp") else { continue };
                let time = ts.timestamp() as f64;
                if samples.last().is_some_and(|s| time < s.time) {
                    continue; // horodatage qui recule : ignoré
                }
                let mut s = Sample::new(time);
                let f = |name: &str| get(name).and_then(num);
                if let (Some(lat), Some(lon)) = (f("position_lat"), f("position_long")) {
                    s.lat = (lat * semicircle) as f32;
                    s.lon = (lon * semicircle) as f32;
                }
                s.ele = f("enhanced_altitude").or_else(|| f("altitude")).map_or(f32::NAN, |v| v as f32);
                s.dist = f("distance").map_or(f32::NAN, |v| v as f32);
                s.speed = f("enhanced_speed").or_else(|| f("speed")).map_or(f32::NAN, |v| v as f32);
                s.hr = f("heart_rate").map_or(f32::NAN, |v| v as f32);
                s.cad = f("cadence").map_or(f32::NAN, |v| v as f32);
                s.power = f("power").map_or(f32::NAN, |v| v as f32);
                s.temp = f("temperature").map_or(f32::NAN, |v| v as f32);
                if samples.last().is_some_and(|l| l.time == time) {
                    *samples.last_mut().unwrap() = s; // doublon : on garde le plus complet (le dernier)
                } else {
                    samples.push(s);
                }
            }
            MesgNum::Session => {
                let f = |name: &str| get(name).and_then(num);
                let add = |acc: &mut Option<f64>, v: Option<f64>| {
                    if let Some(v) = v {
                        *acc = Some(acc.unwrap_or(0.0) + v);
                    }
                };
                add(&mut dev.distance_m, f("total_distance"));
                add(&mut dev.ascent_m, f("total_ascent"));
                add(&mut dev.descent_m, f("total_descent"));
                add(&mut dev.timer_s, f("total_timer_time"));
                add(&mut dev.elapsed_s, f("total_elapsed_time"));
                add(&mut dev.calories, f("total_calories"));
                dev.avg_power = dev.avg_power.or(f("avg_power"));
                dev.normalized_power = dev.normalized_power.or(f("normalized_power"));
                dev.tss = dev.tss.or(f("training_stress_score"));
                dev.ftp = dev.ftp.or(f("threshold_power"));
                dev.avg_hr = dev.avg_hr.or(f("avg_heart_rate"));
                dev.max_hr = dev.max_hr.or(f("max_heart_rate"));
                if a.sport.is_none() {
                    if let Some(Value::String(s)) = get("sport") {
                        a.sport = Some(s.clone());
                    }
                }
                if let Some(Value::Timestamp(ts)) = get("start_time") {
                    a.start_time = a.start_time.or(Some(ts.timestamp()));
                }
            }
            _ => {}
        }
    }
    if samples.is_empty() {
        return Err("ce fichier FIT ne contient aucun enregistrement de sortie (c'est peut-être un parcours ou des réglages)".into());
    }
    a.device = dev;
    to_series(samples, &mut a);
    Ok(a)
}

// --- GPX ----------------------------------------------------------------------

fn child_text<'a>(n: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    n.children().find(|c| c.tag_name().name() == name).and_then(|c| c.text())
}

fn parse_gpx(text: &str) -> Result<Activity, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| format!("GPX illisible : {e}"))?;
    let mut a = Activity { format: "gpx".into(), ..Default::default() };
    a.name = doc
        .descendants()
        .find(|n| n.tag_name().name() == "trk")
        .and_then(|t| child_text(t, "name"))
        .or_else(|| doc.descendants().find(|n| n.tag_name().name() == "metadata").and_then(|m| child_text(m, "name")))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    a.sport = doc.descendants().find(|n| n.tag_name().name() == "trk").and_then(|t| child_text(t, "type")).map(String::from);

    let mut samples = Vec::new();
    for p in doc.descendants().filter(|n| n.tag_name().name() == "trkpt") {
        let Some(time) = child_text(p, "time").and_then(parse_iso8601) else { continue };
        if samples.last().is_some_and(|s: &Sample| time < s.time) {
            continue;
        }
        let mut s = Sample::new(time);
        s.lat = p.attribute("lat").and_then(|v| v.parse().ok()).unwrap_or(f32::NAN);
        s.lon = p.attribute("lon").and_then(|v| v.parse().ok()).unwrap_or(f32::NAN);
        s.ele = child_text(p, "ele").and_then(|v| v.trim().parse().ok()).unwrap_or(f32::NAN);
        // Extensions Garmin (gpxtpx), Strava, etc. : on cherche par nom, quel que soit le préfixe
        for e in p.descendants().filter(|e| e.is_element()) {
            let v: Option<f32> = e.text().and_then(|t| t.trim().parse().ok());
            match e.tag_name().name() {
                "hr" | "heartrate" => s.hr = v.unwrap_or(s.hr),
                "cad" | "cadence" => s.cad = v.unwrap_or(s.cad),
                "power" | "PowerInWatts" | "watts" => s.power = v.unwrap_or(s.power),
                "atemp" | "temp" => s.temp = v.unwrap_or(s.temp),
                "speed" => s.speed = v.unwrap_or(s.speed),
                _ => {}
            }
        }
        samples.push(s);
    }
    if samples.is_empty() {
        return Err(
            "ce GPX n'a pas d'horodatage : c'est un itinéraire, pas une sortie enregistrée. Ouvre-le plutôt dans l'onglet Itinéraires."
                .into(),
        );
    }
    to_series(samples, &mut a);
    Ok(a)
}

// --- TCX ----------------------------------------------------------------------

fn parse_tcx(text: &str) -> Result<Activity, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| format!("TCX illisible : {e}"))?;
    let mut a = Activity { format: "tcx".into(), ..Default::default() };
    a.sport = doc.descendants().find(|n| n.tag_name().name() == "Activity").and_then(|n| n.attribute("Sport")).map(String::from);
    let mut samples = Vec::new();
    for p in doc.descendants().filter(|n| n.tag_name().name() == "Trackpoint") {
        let Some(time) = child_text(p, "Time").and_then(parse_iso8601) else { continue };
        if samples.last().is_some_and(|s: &Sample| time < s.time) {
            continue;
        }
        let mut s = Sample::new(time);
        let deep = |name: &str| -> Option<f32> {
            p.descendants().find(|n| n.tag_name().name() == name).and_then(|n| n.text()).and_then(|t| t.trim().parse().ok())
        };
        s.lat = deep("LatitudeDegrees").unwrap_or(f32::NAN);
        s.lon = deep("LongitudeDegrees").unwrap_or(f32::NAN);
        s.ele = deep("AltitudeMeters").unwrap_or(f32::NAN);
        s.dist = deep("DistanceMeters").unwrap_or(f32::NAN);
        s.hr = p
            .children()
            .find(|n| n.tag_name().name() == "HeartRateBpm")
            .and_then(|n| child_text(n, "Value"))
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(f32::NAN);
        s.cad = deep("Cadence").unwrap_or(f32::NAN);
        s.power = deep("Watts").unwrap_or(f32::NAN);
        s.speed = deep("Speed").unwrap_or(f32::NAN);
        samples.push(s);
    }
    if samples.is_empty() {
        return Err("ce TCX ne contient aucun point horodaté".into());
    }
    to_series(samples, &mut a);
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GPX: &str = r#"<?xml version="1.0"?>
<gpx version="1.1" creator="Garmin" xmlns="http://www.topografix.com/GPX/1/1"
     xmlns:gpxtpx="http://www.garmin.com/xmlschemas/TrackPointExtension/v1">
 <trk><name>Sortie du matin</name><type>cycling</type><trkseg>
  <trkpt lat="48.10" lon="-1.70"><ele>40</ele><time>2026-08-12T07:00:00Z</time>
   <extensions><power>210</power><gpxtpx:TrackPointExtension><gpxtpx:hr>130</gpxtpx:hr><gpxtpx:cad>88</gpxtpx:cad></gpxtpx:TrackPointExtension></extensions></trkpt>
  <trkpt lat="48.101" lon="-1.70"><ele>41</ele><time>2026-08-12T07:00:10Z</time>
   <extensions><power>220</power><gpxtpx:TrackPointExtension><gpxtpx:hr>134</gpxtpx:hr><gpxtpx:cad>90</gpxtpx:cad></gpxtpx:TrackPointExtension></extensions></trkpt>
 </trkseg></trk></gpx>"#;

    #[test]
    fn gpx_with_extensions() {
        let a = parse_activity(GPX.as_bytes(), "x.gpx").unwrap();
        assert_eq!(a.name.as_deref(), Some("Sortie du matin"));
        assert_eq!(a.t, vec![0.0, 10.0]);
        assert_eq!((a.hr[1], a.cad[1], a.power[1]), (134.0, 90.0, 220.0));
        assert!((a.dist[1] - 111.2).abs() < 1.0);
        assert!((a.speed[1] - 11.1).abs() < 0.2);
        assert_eq!(a.start_time, Some(1_786_518_000));
    }

    #[test]
    fn route_gpx_is_refused() {
        let r = r#"<gpx version="1.1"><trk><trkseg><trkpt lat="48" lon="-1"/><trkpt lat="48.1" lon="-1"/></trkseg></trk></gpx>"#;
        assert!(parse_activity(r.as_bytes(), "x.gpx").unwrap_err().contains("itinéraire"));
    }

    #[test]
    fn tcx() {
        let x = r#"<?xml version="1.0"?><TrainingCenterDatabase xmlns="http://www.garmin.com/xmlschemas/TrainingCenterDatabase/v2">
<Activities><Activity Sport="Biking"><Lap><Track>
<Trackpoint><Time>2026-08-12T07:00:00Z</Time><Position><LatitudeDegrees>48.1</LatitudeDegrees><LongitudeDegrees>-1.7</LongitudeDegrees></Position>
<AltitudeMeters>40</AltitudeMeters><DistanceMeters>0</DistanceMeters><HeartRateBpm><Value>120</Value></HeartRateBpm><Cadence>85</Cadence>
<Extensions><TPX xmlns="http://www.garmin.com/xmlschemas/ActivityExtension/v2"><Watts>200</Watts></TPX></Extensions></Trackpoint>
<Trackpoint><Time>2026-08-12T07:00:01Z</Time><DistanceMeters>8</DistanceMeters><HeartRateBpm><Value>121</Value></HeartRateBpm></Trackpoint>
</Track></Lap></Activity></Activities></TrainingCenterDatabase>"#;
        let a = parse_activity(x.as_bytes(), "x.tcx").unwrap();
        assert_eq!(a.sport.as_deref(), Some("Biking"));
        assert_eq!((a.hr[0], a.cad[0], a.power[0]), (120.0, 85.0, 200.0));
        assert_eq!(a.dist[1], 8.0);
        assert!(a.lat[1].is_nan());
    }

    #[test]
    fn fit_course_is_not_an_activity() {
        // Notre propre export de parcours : pas de sortie dedans… mais des records horodatés.
        // Il est lu sans erreur ; c'est l'interface qui distingue parcours et sorties.
        let t = crate::Track::from_points(None, vec![([-1.7, 48.1], Some(40.0)), ([-1.7, 48.11], Some(50.0))]);
        let bytes = crate::fit_export::to_fit_course("Test", &t, 25.0, 1_786_518_000);
        let a = parse_activity(&bytes, "test.fit").unwrap();
        assert_eq!(a.len(), 2);
        assert!((a.ele[1] - 50.0).abs() < 0.3);
        assert!((a.lat[1] - 48.11).abs() < 1e-5);
    }
}

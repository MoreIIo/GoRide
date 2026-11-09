//! Export d'une trace en GPX 1.1 (lisible par Garmin Connect, Strava, Komoot…).

use crate::track::Track;
use std::fmt::Write;

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn to_gpx(name: &str, track: &Track) -> String {
    let mut out = String::with_capacity(track.coords.len() * 70 + 512);
    let name = escape(name);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(
        "<gpx version=\"1.1\" creator=\"GoRide\" xmlns=\"http://www.topografix.com/GPX/1/1\">\n",
    );
    let _ = writeln!(out, " <metadata><name>{name}</name></metadata>");
    let _ = writeln!(out, " <trk>\n  <name>{name}</name>\n  <type>cycling</type>\n  <trkseg>");
    for (c, e) in track.coords.iter().zip(&track.ele) {
        match e {
            Some(e) => {
                let _ = writeln!(out, "   <trkpt lat=\"{:.6}\" lon=\"{:.6}\"><ele>{e:.1}</ele></trkpt>", c[1], c[0]);
            }
            None => {
                let _ = writeln!(out, "   <trkpt lat=\"{:.6}\" lon=\"{:.6}\"/>", c[1], c[0]);
            }
        }
    }
    out.push_str("  </trkseg>\n </trk>\n</gpx>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let t = Track::from_points(None, vec![([-1.7, 48.1], Some(40.0)), ([-1.7, 48.11], Some(60.0))]);
        let xml = to_gpx("Boucle <test> & co", &t);
        assert!(xml.contains("Boucle &lt;test&gt; &amp; co"));
        let back = crate::parse_gpx(xml.as_bytes()).unwrap();
        assert_eq!(back.stats.point_count, 2);
        assert!((back.stats.distance_m - t.stats.distance_m).abs() < 1.0);
        assert_eq!(back.stats.ascent_m, 20.0);
    }
}

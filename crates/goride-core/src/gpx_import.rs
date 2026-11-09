//! Import d'un fichier GPX (traces `trk` en priorité, sinon itinéraires `rte`).

use crate::{track::Track, CoreError};
use std::io::BufReader;

/// Lit un GPX et renvoie une trace unique : tous les segments de toutes les
/// traces sont mis bout à bout. Si le fichier n'a pas de trace, on prend
/// ses itinéraires (`rte`), fréquents dans les exports de planificateurs.
pub fn parse_gpx(bytes: &[u8]) -> Result<Track, CoreError> {
    let doc = gpx::read(BufReader::new(bytes)).map_err(|e| CoreError::Gpx(e.to_string()))?;

    let mut raw = Vec::new();
    for trk in &doc.tracks {
        for seg in &trk.segments {
            raw.extend(seg.points.iter().map(waypoint));
        }
    }
    if raw.is_empty() {
        for rte in &doc.routes {
            raw.extend(rte.points.iter().map(waypoint));
        }
    }
    if raw.is_empty() {
        return Err(CoreError::Empty);
    }

    let name = doc
        .tracks
        .iter()
        .find_map(|t| t.name.clone())
        .or_else(|| doc.routes.iter().find_map(|r| r.name.clone()))
        .or_else(|| doc.metadata.as_ref().and_then(|m| m.name.clone()))
        .filter(|n| !n.trim().is_empty());

    Ok(Track::from_points(name, raw))
}

fn waypoint(w: &gpx::Waypoint) -> ([f64; 2], Option<f64>) {
    let p = w.point();
    ([p.x(), p.y()], w.elevation)
}

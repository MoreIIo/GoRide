//! Itinéraires enregistrés : un fichier GPX par itinéraire, rangés dans un dossier
//! choisi une fois pour toutes (réglage gardé dans `settings.json`).
//!
//! Le GPX reste lisible partout (Garmin Connect, Komoot, Strava…). GoRide y ajoute,
//! dans une extension, ses propres données : points de passage et réglages du tracé,
//! pour rouvrir l'itinéraire et continuer à le modifier.

use goride_core::Track;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Espace de noms XML de l'extension GoRide.
const NS: &str = "https://goride.app/xmlns/route/1";

/// Données propres à GoRide, enregistrées dans le GPX.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RouteMeta {
    /// Points de passage (vides pour une trace importée : non modifiable).
    pub waypoints: Vec<[f64; 2]>,
    /// Réglages du tracé utilisés (profil, dénivelé…), tels que l'interface les envoie.
    pub options: Option<serde_json::Value>,
    pub distance_m: f64,
    pub ascent_m: f64,
    /// Dates en millisecondes depuis 1970.
    pub created_at: i64,
    pub saved_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedRouteSummary {
    /// Chemin complet du fichier (identifiant de l'itinéraire).
    pub file: String,
    pub name: String,
    pub distance_m: f64,
    pub ascent_m: f64,
    pub saved_at: i64,
    /// Points de passage connus : l'itinéraire se rouvre en mode tracé.
    pub editable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedRoute {
    pub summary: SavedRouteSummary,
    pub track: Track,
    pub waypoints: Vec<[f64; 2]>,
    pub options: Option<serde_json::Value>,
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Nom de fichier sûr sous Windows à partir du nom de l'itinéraire.
pub fn file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '-' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    let short: String = cleaned.chars().take(90).collect();
    if short.is_empty() { "Itinéraire".into() } else { short }
}

pub fn to_gpx(name: &str, coords: &[[f64; 2]], ele: &[Option<f64>], meta: &RouteMeta) -> String {
    let mut out = String::with_capacity(coords.len() * 70 + 1024);
    let name = escape(name);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<gpx version=\"1.1\" creator=\"GoRide\" xmlns=\"http://www.topografix.com/GPX/1/1\">\n");
    let _ = writeln!(out, " <metadata><name>{name}</name></metadata>");
    let _ = writeln!(out, " <trk>\n  <name>{name}</name>\n  <type>cycling</type>\n  <trkseg>");
    for (c, e) in coords.iter().zip(ele) {
        match e {
            Some(e) => {
                let _ = writeln!(out, "   <trkpt lat=\"{:.6}\" lon=\"{:.6}\"><ele>{e:.1}</ele></trkpt>", c[1], c[0]);
            }
            None => {
                let _ = writeln!(out, "   <trkpt lat=\"{:.6}\" lon=\"{:.6}\"/>", c[1], c[0]);
            }
        }
    }
    out.push_str("  </trkseg>\n </trk>\n");
    let json = serde_json::to_string(meta).unwrap_or_default();
    let _ = writeln!(out, " <extensions><goride:route xmlns:goride=\"{NS}\">{}</goride:route></extensions>", escape(&json));
    out.push_str("</gpx>\n");
    out
}

/// Lit un GPX du dossier : nom, trace et données GoRide s'il y en a.
pub fn read(path: &Path) -> Result<(String, Track, Option<RouteMeta>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Lecture impossible : {e}"))?;
    let doc = roxmltree::Document::parse(&text).map_err(|e| format!("Fichier GPX illisible : {e}"))?;
    let meta: Option<RouteMeta> = doc
        .descendants()
        .find(|n| n.has_tag_name((NS, "route")))
        .and_then(|n| n.text())
        .and_then(|t| serde_json::from_str(t).ok());

    let child_text = |n: roxmltree::Node, tag: &str| {
        n.children().find(|c| c.tag_name().name() == tag).and_then(|c| c.text()).map(|s| s.trim().to_string())
    };
    let point = |n: roxmltree::Node| -> Option<([f64; 2], Option<f64>)> {
        let lat: f64 = n.attribute("lat")?.trim().parse().ok()?;
        let lon: f64 = n.attribute("lon")?.trim().parse().ok()?;
        let ele = child_text(n, "ele").and_then(|s| s.parse().ok());
        Some(([lon, lat], ele))
    };
    let mut raw: Vec<_> = doc.descendants().filter(|n| n.tag_name().name() == "trkpt").filter_map(point).collect();
    if raw.is_empty() {
        raw = doc.descendants().filter(|n| n.tag_name().name() == "rtept").filter_map(point).collect();
    }
    if raw.len() < 2 {
        return Err("Aucune trace dans ce fichier.".into());
    }
    let name = ["trk", "rte", "metadata"]
        .iter()
        .find_map(|tag| doc.descendants().find(|n| n.tag_name().name() == *tag).and_then(|n| child_text(n, "name")))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or("Itinéraire").to_string());
    let track = Track::from_points(Some(name.clone()), raw);
    Ok((name, track, meta))
}

fn summary_of(path: &Path, name: String, track: &Track, meta: Option<&RouteMeta>) -> SavedRouteSummary {
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    SavedRouteSummary {
        file: path.to_string_lossy().into_owned(),
        name,
        distance_m: meta.map(|m| m.distance_m).filter(|d| *d > 0.0).unwrap_or(track.stats.distance_m),
        ascent_m: meta.map(|m| m.ascent_m).filter(|d| *d > 0.0).unwrap_or(track.stats.ascent_m),
        saved_at: meta.map(|m| m.saved_at).filter(|t| *t > 0).unwrap_or(mtime),
        editable: meta.is_some_and(|m| m.waypoints.len() >= 2),
    }
}

/// Tous les itinéraires du dossier (fichiers .gpx), du plus récent au plus ancien.
/// Les GPX déposés à la main dans le dossier apparaissent aussi.
pub fn list(dir: &Path) -> Result<Vec<SavedRouteSummary>, String> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(format!("Dossier des itinéraires illisible : {e}")),
    };
    let mut out: Vec<SavedRouteSummary> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("gpx")))
        .filter_map(|p| match read(&p) {
            Ok((name, track, meta)) => Some(summary_of(&p, name, &track, meta.as_ref())),
            Err(e) => {
                eprintln!("Itinéraire ignoré {p:?} : {e}");
                None
            }
        })
        .collect();
    out.sort_by(|a, b| b.saved_at.cmp(&a.saved_at).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// Vérifie qu'un fichier est bien dans le dossier des itinéraires (rien d'autre n'est touché).
pub fn inside(dir: &Path, file: &Path) -> Result<PathBuf, String> {
    let d = dir.canonicalize().map_err(|e| format!("Dossier des itinéraires introuvable : {e}"))?;
    let f = file.canonicalize().map_err(|_| "Cet itinéraire n'existe plus (fichier déplacé ou supprimé ?).".to_string())?;
    if f.parent() == Some(d.as_path()) {
        Ok(f)
    } else {
        Err("Ce fichier n'est pas dans le dossier des itinéraires.".into())
    }
}

pub fn open(dir: &Path, file: &Path) -> Result<SavedRoute, String> {
    let path = inside(dir, file)?;
    let (name, track, meta) = read(&path)?;
    let mut summary = summary_of(&path, name, &track, meta.as_ref());
    summary.file = file.to_string_lossy().into_owned();
    let meta = meta.unwrap_or_default();
    Ok(SavedRoute { summary, track, waypoints: meta.waypoints, options: meta.options })
}

/// Enregistre (ou met à jour) un itinéraire. `previous` : fichier d'origine quand on
/// réenregistre un itinéraire ouvert ; renommé, l'ancien fichier est remplacé.
pub fn save(
    dir: &Path,
    name: &str,
    coords: &[[f64; 2]],
    ele: &[Option<f64>],
    mut meta: RouteMeta,
    previous: Option<&Path>,
) -> Result<SavedRouteSummary, String> {
    if coords.len() < 2 || coords.len() != ele.len() {
        return Err("Itinéraire vide ou incohérent.".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Création du dossier impossible : {e}"))?;
    let previous = previous.and_then(|p| inside(dir, p).ok());

    // Date de création conservée d'un enregistrement à l'autre
    let now = now_ms();
    meta.saved_at = now;
    meta.created_at = previous
        .as_deref()
        .and_then(|p| read(p).ok())
        .and_then(|(_, _, m)| m)
        .map(|m| m.created_at)
        .filter(|t| *t > 0)
        .unwrap_or(now);

    // Nom de fichier libre (« Boucle (2).gpx » si le nom est déjà pris par un autre itinéraire)
    let stem = file_stem(name);
    let mut target = dir.join(format!("{stem}.gpx"));
    let mut k = 2;
    while target.exists() && target.canonicalize().ok() != previous {
        target = dir.join(format!("{stem} ({k}).gpx"));
        k += 1;
    }

    let gpx = to_gpx(name, coords, ele, &meta);
    let tmp = target.with_extension("gpx.tmp");
    std::fs::write(&tmp, gpx).map_err(|e| format!("Écriture impossible : {e}"))?;
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Écriture impossible : {e}")
    })?;
    if let Some(prev) = previous {
        if target.canonicalize().ok().as_deref() != Some(prev.as_path()) {
            let _ = std::fs::remove_file(prev);
        }
    }
    let track = Track::from_points(Some(name.to_string()), coords.iter().copied().zip(ele.iter().copied()).collect());
    Ok(summary_of(&target, name.to_string(), &track, Some(&meta)))
}

pub fn delete(dir: &Path, file: &Path) -> Result<(), String> {
    let path = inside(dir, file)?;
    std::fs::remove_file(path).map_err(|e| format!("Suppression impossible : {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_list_open_rename_delete() {
        let dir = std::env::temp_dir().join(format!("goride-routes-{}", now_ms()));
        let coords = vec![[-1.68, 48.11], [-1.67, 48.12], [-1.66, 48.12]];
        let ele = vec![Some(40.0), Some(52.0), None];
        let meta = RouteMeta {
            waypoints: vec![[-1.68, 48.11], [-1.66, 48.12]],
            options: Some(serde_json::json!({"profile": "Road", "hills": 0})),
            distance_m: 2100.0,
            ascent_m: 12.0,
            ..Default::default()
        };
        let a = save(&dir, "Boucle <du> \"dimanche\" : 50 km", &coords, &ele, meta.clone(), None).unwrap();
        assert!(a.file.ends_with("Boucle <du> -dimanche- - 50 km.gpx") || a.file.contains("Boucle"));
        assert!(a.editable);
        // Même nom, autre itinéraire : pas d'écrasement
        let b = save(&dir, "Boucle <du> \"dimanche\" : 50 km", &coords, &ele, meta.clone(), None).unwrap();
        assert_ne!(a.file, b.file);
        assert_eq!(list(&dir).unwrap().len(), 2);

        // Le GPX reste lisible par le lecteur standard
        let t = goride_core::parse_gpx(&std::fs::read(&a.file).unwrap()).unwrap();
        assert_eq!(t.stats.point_count, 3);

        let r = open(&dir, Path::new(&a.file)).unwrap();
        assert_eq!(r.waypoints.len(), 2);
        assert_eq!(r.summary.name, "Boucle <du> \"dimanche\" : 50 km");
        assert_eq!(r.options.unwrap()["profile"], "Road");
        assert_eq!(r.track.ele[2], Some(52.0)); // altitude manquante interpolée

        // Renommage : l'ancien fichier disparaît
        let c = save(&dir, "Sortie du lac", &coords, &ele, meta, Some(Path::new(&a.file))).unwrap();
        assert!(!Path::new(&a.file).exists());
        assert!(Path::new(&c.file).exists());
        assert_eq!(list(&dir).unwrap().len(), 2);

        assert!(delete(&dir, Path::new("/etc/hosts")).is_err());
        delete(&dir, Path::new(&c.file)).unwrap();
        delete(&dir, Path::new(&b.file)).unwrap();
        assert!(list(&dir).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

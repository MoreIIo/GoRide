//! Régions OpenStreetMap chargées automatiquement selon l'endroit où l'on trace.
//!
//! On s'appuie sur le découpage de la France métropolitaine de Geofabrik (22 régions) :
//! - le contour exact de chaque région (fichier `.poly`, quelques Ko) est téléchargé une
//!   fois puis gardé sur le disque ;
//! - quand on clique hors du réseau chargé, on retient la région du point et les régions
//!   voisines à moins de [`NEAR_KM`] km, on télécharge leurs extraits `.osm.pbf` (une seule
//!   fois) et on construit un réseau unique, mis en cache pour la fois suivante.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BASE: &str = "https://download.geofabrik.de/europe/france";
const USER_AGENT: &str = "GoRide/0.1 (planificateur d'itinéraires vélo, usage personnel)";
/// Les régions dont la frontière passe à moins de cette distance d'un point sont chargées
/// aussi : de quoi faire une sortie autour du point sans buter sur une frontière.
pub const NEAR_KM: f64 = 40.0;

pub struct RegionDef {
    pub slug: &'static str,
    pub name: &'static str,
    /// Taille approximative de l'extrait, pour prévenir avant de le télécharger.
    pub size_mb: u32,
}

const fn r(slug: &'static str, name: &'static str, size_mb: u32) -> RegionDef {
    RegionDef { slug, name, size_mb }
}

/// France métropolitaine, découpage Geofabrik (anciennes régions).
pub const CATALOG: &[RegionDef] = &[
    r("alsace", "Alsace", 122),
    r("aquitaine", "Aquitaine", 274),
    r("auvergne", "Auvergne", 140),
    r("basse-normandie", "Basse-Normandie", 133),
    r("bourgogne", "Bourgogne", 184),
    r("bretagne", "Bretagne", 306),
    r("centre", "Centre", 223),
    r("champagne-ardenne", "Champagne-Ardenne", 98),
    r("corse", "Corse", 32),
    r("franche-comte", "Franche-Comté", 115),
    r("haute-normandie", "Haute-Normandie", 98),
    r("ile-de-france", "Île-de-France", 312),
    r("languedoc-roussillon", "Languedoc-Roussillon", 248),
    r("limousin", "Limousin", 92),
    r("lorraine", "Lorraine", 159),
    r("midi-pyrenees", "Midi-Pyrénées", 333),
    r("nord-pas-de-calais", "Nord-Pas-de-Calais", 221),
    r("pays-de-la-loire", "Pays de la Loire", 345),
    r("picardie", "Picardie", 124),
    r("poitou-charentes", "Poitou-Charentes", 215),
    r("provence-alpes-cote-d-azur", "Provence-Alpes-Côte d'Azur", 359),
    r("rhone-alpes", "Rhône-Alpes", 487),
];

pub fn def(slug: &str) -> Option<&'static RegionDef> {
    CATALOG.iter().find(|d| d.slug == slug)
}

/// Clé du réseau de toute la France métropolitaine (les 22 régions).
pub const FRANCE_KEY: &str = "france";

/// Clé d'un ensemble de régions (nom du réseau en cache) : « aquitaine+poitou-charentes »,
/// ou « france » pour toutes les régions.
pub fn key_of(slugs: &[&str]) -> String {
    let mut v: Vec<&RegionDef> = slugs.iter().filter_map(|s| def(s)).collect();
    v.sort_by_key(|d| CATALOG.iter().position(|c| c.slug == d.slug));
    v.dedup_by_key(|d| d.slug);
    if v.len() == CATALOG.len() {
        return FRANCE_KEY.into();
    }
    v.iter().map(|d| d.slug).collect::<Vec<_>>().join("+")
}

/// Régions d'une clé, si elle ne contient que des régions du catalogue.
pub fn slugs_of_key(key: &str) -> Option<Vec<&'static str>> {
    if key == FRANCE_KEY {
        return Some(CATALOG.iter().map(|d| d.slug).collect());
    }
    let v: Option<Vec<&'static str>> = key.split('+').map(|s| def(s).map(|d| d.slug)).collect();
    v.filter(|v| !v.is_empty())
}

/// Nom lisible : « Aquitaine + Poitou-Charentes », « France entière ».
pub fn display_name(key: &str) -> Option<String> {
    if key == FRANCE_KEY {
        return Some("France entière".into());
    }
    slugs_of_key(key).map(|v| v.iter().filter_map(|s| def(s)).map(|d| d.name).collect::<Vec<_>>().join(" + "))
}

// --- Contours (.poly) -----------------------------------------------------------

/// Contour d'une région : anneaux extérieurs et trous, en (lon, lat).
pub struct Poly {
    rings: Vec<(bool, Vec<[f64; 2]>)>,
    bbox: [f64; 4],
}

impl Poly {
    /// Format « Osmosis polygon » : nom, puis sections (« !x » = trou) de coordonnées, chacune terminée par END.
    pub fn parse(text: &str) -> Result<Poly, String> {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        lines.next().ok_or("contour vide")?; // nom
        let mut rings = Vec::new();
        let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        while let Some(head) = lines.next() {
            if head == "END" {
                break;
            }
            let hole = head.starts_with('!');
            let mut ring = Vec::new();
            for l in lines.by_ref() {
                if l == "END" {
                    break;
                }
                let mut it = l.split_whitespace().map(str::parse::<f64>);
                match (it.next(), it.next()) {
                    (Some(Ok(lon)), Some(Ok(lat))) => {
                        ring.push([lon, lat]);
                        if !hole {
                            bbox = [bbox[0].min(lon), bbox[1].min(lat), bbox[2].max(lon), bbox[3].max(lat)];
                        }
                    }
                    _ => return Err(format!("ligne de contour illisible : « {l} »")),
                }
            }
            if ring.len() >= 3 {
                rings.push((hole, ring));
            }
        }
        if !rings.iter().any(|(h, _)| !h) {
            return Err("contour sans surface".into());
        }
        Ok(Poly { rings, bbox })
    }

    pub fn contains(&self, p: [f64; 2]) -> bool {
        let [w, s, e, n] = self.bbox;
        if p[0] < w || p[0] > e || p[1] < s || p[1] > n {
            return false;
        }
        let inside_outer = self.rings.iter().any(|(h, r)| !h && ring_contains(r, p));
        inside_outer && !self.rings.iter().any(|(h, r)| *h && ring_contains(r, p))
    }

    /// Distance (km) du point à la région : 0 dedans, sinon jusqu'à la frontière la plus proche.
    pub fn distance_km(&self, p: [f64; 2]) -> f64 {
        if self.contains(p) {
            return 0.0;
        }
        // Rejet rapide : plus loin que la boîte englobante + marge confortable
        let k = (p[1].to_radians()).cos();
        let dx = ((self.bbox[0] - p[0]).max(p[0] - self.bbox[2]).max(0.0)) * k * 111.32;
        let dy = (self.bbox[1] - p[1]).max(p[1] - self.bbox[3]).max(0.0) * 110.57;
        let box_d = dx.hypot(dy);
        if box_d > 300.0 {
            return box_d;
        }
        let mut best = f64::MAX;
        for (hole, ring) in &self.rings {
            if *hole {
                continue;
            }
            for i in 0..ring.len() {
                let a = ring[i];
                let b = ring[(i + 1) % ring.len()];
                best = best.min(seg_dist_km(p, a, b));
            }
        }
        best
    }
}

/// Test pair-impair (lancer de rayon).
fn ring_contains(ring: &[[f64; 2]], p: [f64; 2]) -> bool {
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[j]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Distance (km) d'un point à un segment, en projection locale autour du point.
fn seg_dist_km(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let k = p[1].to_radians().cos() * 111.32;
    let to = |q: [f64; 2]| [(q[0] - p[0]) * k, (q[1] - p[1]) * 110.57];
    let (a, b) = (to(a), to(b));
    let (vx, vy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = vx * vx + vy * vy;
    let t = if len2 > 0.0 { (-(a[0] * vx + a[1] * vy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (a[0] + t * vx).hypot(a[1] + t * vy)
}

/// Régions à charger pour tracer autour de `points`, en gardant celles des points `keep`
/// (l'itinéraire en cours, pour qu'il reste calculable).
pub fn needed_regions(polys: &[(&'static RegionDef, Arc<Poly>)], points: &[[f64; 2]], keep: &[[f64; 2]]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for &p in points {
        for (d, poly) in polys {
            if poly.distance_km(p) <= NEAR_KM && !out.contains(&d.slug) {
                out.push(d.slug);
            }
        }
    }
    for &p in keep {
        if let Some((d, _)) = polys.iter().find(|(_, poly)| poly.contains(p)) {
            if !out.contains(&d.slug) {
                out.push(d.slug);
            }
        }
    }
    out.sort_by_key(|s| CATALOG.iter().position(|c| c.slug == *s));
    out
}

// --- Téléchargements et cache ------------------------------------------------------------

pub struct Regions {
    dir: PathBuf,
    polys: Mutex<HashMap<&'static str, Arc<Poly>>>,
    agent: ureq::Agent,
}

impl Regions {
    pub fn new(dir: PathBuf) -> Regions {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(60))
            .user_agent(USER_AGENT)
            .build();
        Regions { dir, polys: Mutex::new(HashMap::new()), agent }
    }

    fn poly_path(&self, slug: &str) -> PathBuf {
        self.dir.join("contours").join(format!("{slug}.poly"))
    }

    /// Extrait OpenStreetMap d'une région, à cet emplacement une fois téléchargé.
    pub fn pbf_path(&self, slug: &str) -> PathBuf {
        self.dir.join(format!("{slug}-latest.osm.pbf"))
    }

    fn poly(&self, d: &'static RegionDef) -> Result<Arc<Poly>, String> {
        if let Some(p) = self.polys.lock().unwrap().get(d.slug) {
            return Ok(p.clone());
        }
        let path = self.poly_path(d.slug);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => {
                let url = format!("{BASE}/{}.poly", d.slug);
                let t = self
                    .agent
                    .get(&url)
                    .call()
                    .map_err(|e| format!("contour de la région {} indisponible ({e})", d.name))?
                    .into_string()
                    .map_err(|e| e.to_string())?;
                let _ = std::fs::create_dir_all(path.parent().unwrap());
                let _ = std::fs::write(&path, &t);
                t
            }
        };
        let poly = Arc::new(Poly::parse(&text).map_err(|e| format!("contour {} : {e}", d.name))?);
        self.polys.lock().unwrap().insert(d.slug, poly.clone());
        Ok(poly)
    }

    /// Contours de toutes les régions (téléchargés en parallèle la première fois).
    pub fn all(&self) -> Result<Vec<(&'static RegionDef, Arc<Poly>)>, String> {
        let results: Vec<Result<(&'static RegionDef, Arc<Poly>), String>> = std::thread::scope(|s| {
            let handles: Vec<_> = CATALOG.iter().map(|d| s.spawn(move || self.poly(d).map(|p| (d, p)))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("erreur interne".into()))).collect()
        });
        let mut out = Vec::with_capacity(results.len());
        let mut errors = Vec::new();
        for r in results {
            match r {
                Ok(x) => out.push(x),
                Err(e) => errors.push(e),
            }
        }
        if out.is_empty() {
            return Err(format!(
                "Impossible de récupérer la carte des régions (pas d'accès à download.geofabrik.de ?) : {}",
                errors.first().cloned().unwrap_or_default()
            ));
        }
        if !errors.is_empty() {
            eprintln!("Contours manquants : {errors:?}");
        }
        Ok(out)
    }

    /// Télécharge l'extrait d'une région s'il n'est pas déjà là.
    /// `progress(octets reçus, taille totale)` est appelé régulièrement ; `cancel` interrompt.
    pub fn ensure_pbf(&self, d: &RegionDef, cancel: &AtomicBool, progress: impl Fn(u64, Option<u64>)) -> Result<PathBuf, String> {
        let path = self.pbf_path(d.slug);
        if path.exists() {
            return Ok(path);
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let url = format!("{BASE}/{}-latest.osm.pbf", d.slug);
        let resp = self
            .agent
            .get(&url)
            .timeout(Duration::from_secs(60 * 60))
            .call()
            .map_err(|e| format!("téléchargement de {} impossible : {e}", d.name))?;
        let total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok());
        let part = path.with_extension("pbf.part");
        let mut file = std::io::BufWriter::new(std::fs::File::create(&part).map_err(|e| e.to_string())?);
        let mut reader = resp.into_reader();
        let mut buf = vec![0u8; 1 << 16];
        let mut done: u64 = 0;
        let mut last_report = 0u64;
        let result: Result<(), String> = (|| {
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err("Téléchargement annulé.".into());
                }
                let n = reader.read(&mut buf).map_err(|e| format!("téléchargement de {} interrompu : {e}", d.name))?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n]).map_err(|e| format!("écriture impossible : {e}"))?;
                done += n as u64;
                if done - last_report >= 4 << 20 {
                    last_report = done;
                    progress(done, total);
                }
            }
            file.flush().map_err(|e| e.to_string())?;
            if total.is_some_and(|t| t != done) {
                return Err(format!("téléchargement de {} incomplet, réessaie", d.name));
            }
            Ok(())
        })();
        drop(file);
        if let Err(e) = result {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
        Ok(path)
    }
}

/// Ne garde que les `keep` réseaux les plus récents (chacun pèse plusieurs centaines de Mo).
pub fn prune_graphs(graphs_dir: &Path, keep: usize, current: &Path) {
    let Ok(rd) = std::fs::read_dir(graphs_dir) else { return };
    let mut graphs: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "graph") && p != current)
        .filter_map(|p| std::fs::metadata(&p).and_then(|m| m.modified()).ok().map(|t| (t, p)))
        .collect();
    graphs.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in graphs.into_iter().skip(keep.saturating_sub(1)) {
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("source"));
    }
}

/// Téléphone : seuls les assemblages de plusieurs régions (« bretagne+pays-de-la-loire ») sont
/// supprimés au-delà de `keep`. Les régions téléchargées restent : sans elles, il faudrait les
/// télécharger à nouveau, alors qu'un assemblage se refait en une seconde à partir d'elles.
pub fn prune_merged(graphs_dir: &Path, keep: usize, current: &Path) {
    let Ok(rd) = std::fs::read_dir(graphs_dir) else { return };
    let mut merged: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "graph") && p != current)
        .filter(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.contains('+')))
        .filter_map(|p| std::fs::metadata(&p).and_then(|m| m.modified()).ok().map(|t| (t, p)))
        .collect();
    merged.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in merged.into_iter().skip(keep) {
        let _ = std::fs::remove_file(&p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_merged_keeps_downloaded_regions() {
        let dir = std::env::temp_dir().join(format!("goride-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let names = ["bretagne", "pays-de-la-loire", "alsace", "corse", "bretagne+pays-de-la-loire", "alsace+lorraine", "centre+limousin"];
        for (i, n) in names.iter().enumerate() {
            std::fs::write(dir.join(format!("{n}.graph")), b"x").unwrap();
            // dates différentes : du plus ancien au plus récent
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + i as u64 * 60);
            std::fs::File::options().write(true).open(dir.join(format!("{n}.graph"))).unwrap().set_modified(t).unwrap();
        }
        prune_merged(&dir, 1, &dir.join("bretagne.graph"));
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["alsace.graph", "bretagne.graph", "centre+limousin.graph", "corse.graph", "pays-de-la-loire.graph"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    const SQUARE: &str = "test\n1\n   0.0E+00   0.0E+00\n   2.0E+00   0.0E+00\n   2.0E+00   2.0E+00\n   0.0E+00   2.0E+00\nEND\n!2\n 0.5 0.5\n 1.0 0.5\n 1.0 1.0\n 0.5 1.0\nEND\nEND\n";

    #[test]
    fn poly_contains_and_distance() {
        let p = Poly::parse(SQUARE).unwrap();
        assert!(p.contains([1.5, 1.5]));
        assert!(!p.contains([0.75, 0.75])); // dans le trou
        assert!(!p.contains([3.0, 1.0]));
        assert_eq!(p.distance_km([1.5, 1.5]), 0.0);
        let d = p.distance_km([2.5, 1.0]); // 0,5° de longitude à 1° de latitude ≈ 55,7 km
        assert!((d - 55.65).abs() < 0.5, "{d}");
    }

    #[test]
    fn keys_and_names() {
        assert_eq!(key_of(&["poitou-charentes", "aquitaine", "aquitaine"]), "aquitaine+poitou-charentes");
        assert_eq!(display_name("aquitaine+poitou-charentes").unwrap(), "Aquitaine + Poitou-Charentes");
        assert_eq!(display_name("bretagne").unwrap(), "Bretagne");
        assert!(display_name("ma-carte").is_none());
        let all: Vec<&str> = CATALOG.iter().rev().map(|d| d.slug).collect();
        assert_eq!(key_of(&all), "france");
        assert_eq!(slugs_of_key("france").unwrap().len(), 22);
        assert_eq!(display_name("france").unwrap(), "France entière");
    }

    #[test]
    fn needed_near_border() {
        let a = Arc::new(Poly::parse("a\n1\n0 0\n1 0\n1 1\n0 1\nEND\nEND\n").unwrap());
        let b = Arc::new(Poly::parse("b\n1\n1 0\n2 0\n2 1\n1 1\nEND\nEND\n").unwrap());
        let polys = vec![(&CATALOG[1], a), (&CATALOG[19], b)];
        // Au milieu de « a », à ~55 km de « b » : seulement « a »
        assert_eq!(needed_regions(&polys, &[[0.5, 0.5]], &[]), vec!["aquitaine"]);
        // À 10 km de la frontière : les deux
        assert_eq!(needed_regions(&polys, &[[0.9, 0.5]], &[]), vec!["aquitaine", "poitou-charentes"]);
        // L'itinéraire en cours (dans « b ») est gardé
        assert_eq!(needed_regions(&polys, &[[0.5, 0.5]], &[[1.5, 0.5]]), vec!["aquitaine", "poitou-charentes"]);
    }
}

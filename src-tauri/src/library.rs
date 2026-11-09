//! Bibliothèque des sorties : base SQLite locale (aucune donnée ne quitte le PC).
//!
//! - Import de fichiers (FIT, GPX, TCX, éventuellement compressés en .gz), d'un dossier
//!   entier (ex. `Garmin/Activity`) ou de l'archive d'export Strava (`activities.csv` pour les noms).
//! - Chaque sortie garde ses données complètes (sérialisées) et un résumé pour les listes,
//!   la progression et la comparaison des montées.

use goride_core::activity::analysis::{analyze, Analysis, Athlete, Climb};
use goride_core::activity::progress::{progress, Progress, RideLoad};
use goride_core::activity::{parse_activity, Activity};
use goride_core::geo::haversine_m;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Version des indicateurs stockés : quand elle change, les sorties existantes sont recalculées.
const METRICS_VERSION: i64 = 2;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySummary {
    pub id: i64,
    pub name: String,
    pub sport: Option<String>,
    pub start_time: Option<i64>,
    pub format: String,
    pub distance_m: f64,
    pub moving_s: f64,
    pub ascent_m: f64,
    pub avg_power: Option<f64>,
    pub avg_hr: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: Vec<ActivitySummary>,
    pub duplicates: Vec<String>,
    /// Fichiers écartés car ce ne sont pas des sorties vélo (course à pied, marche…).
    pub skipped: Vec<String>,
    pub errors: Vec<(String, String)>,
}

/// Points de la trace pour la carte, avec la distance (m) de chaque point.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MapTrack {
    pub coords: Vec<[f64; 2]>,
    pub ele: Vec<Option<f64>>,
    pub dist: Vec<f64>,
}

/// Un passage sur une montée.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClimbEffort {
    pub activity_id: i64,
    pub name: String,
    pub start_time: Option<i64>,
    pub duration_s: f64,
    pub vam: f64,
    pub avg_power: Option<f64>,
    pub avg_hr: Option<f64>,
    pub current: bool,
}

/// Tous les passages sur une montée, du plus rapide au plus lent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClimbHistory {
    pub efforts: Vec<ClimbEffort>,
    /// Rang du passage de cette sortie (1 = record).
    pub rank: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityDetail {
    pub summary: ActivitySummary,
    pub track: MapTrack,
    pub analysis: Analysis,
    /// Historique de chaque montée de la sortie (même ordre que `analysis.climbs`).
    pub climb_history: Vec<ClimbHistory>,
}

pub struct Library {
    db: Connection,
}

/// Empreinte FNV-1a du fichier : reconnaît un fichier déjà importé.
fn file_hash(bytes: &[u8]) -> String {
    let h = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
    format!("{h:016x}-{}", bytes.len())
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Sport de vélo (ou non précisé) ?
pub fn is_cycling(sport: Option<&str>) -> bool {
    let Some(s) = sport else { return true };
    let s = s.to_lowercase();
    !["run", "walk", "hik", "swim", "ski", "row", "course", "marche", "randonn", "natation"]
        .iter()
        .any(|k| s.contains(k))
}

/// Nom du fichier sans « .gz » ni extension : « 1234567.fit.gz » → « 1234567 ».
fn stem(file: &str) -> String {
    let f = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let f = f.strip_suffix(".gz").unwrap_or(f);
    f.rsplit_once('.').map_or(f, |(s, _)| s).to_string()
}

fn is_activity_file(p: &Path) -> bool {
    let n = p.file_name().and_then(|f| f.to_str()).unwrap_or("").to_lowercase();
    let n = n.strip_suffix(".gz").unwrap_or(&n);
    n.ends_with(".fit") || n.ends_with(".gpx") || n.ends_with(".tcx")
}

const SELECT_SUMMARY: &str =
    "SELECT id, name, sport, start_time, format, distance_m, moving_s, ascent_m, avg_power, avg_hr FROM activities";

fn summary_row(r: &rusqlite::Row) -> rusqlite::Result<ActivitySummary> {
    Ok(ActivitySummary {
        id: r.get(0)?,
        name: r.get(1)?,
        sport: r.get(2)?,
        start_time: r.get(3)?,
        format: r.get(4)?,
        distance_m: r.get(5)?,
        moving_s: r.get(6)?,
        ascent_m: r.get(7)?,
        avg_power: r.get(8)?,
        avg_hr: r.get(9)?,
    })
}

/// Lit `activities.csv` de l'export Strava : nom de fichier (sans extension) → (nom, type).
fn strava_names(csv_path: &Path) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let Ok(mut rdr) = csv::Reader::from_path(csv_path) else { return out };
    let Ok(headers) = rdr.headers().cloned() else { return out };
    let col = |names: &[&str]| headers.iter().position(|h| names.iter().any(|n| h.eq_ignore_ascii_case(n)));
    let (Some(fname), Some(name)) = (col(&["Filename", "Nom du fichier"]), col(&["Activity Name", "Nom de l'activité"])) else {
        return out;
    };
    let kind = col(&["Activity Type", "Type d'activité"]);
    for rec in rdr.records().flatten() {
        let f = rec.get(fname).unwrap_or("").trim();
        if f.is_empty() {
            continue;
        }
        out.insert(
            stem(f),
            (rec.get(name).unwrap_or("").trim().to_string(), kind.and_then(|k| rec.get(k)).unwrap_or("").to_string()),
        );
    }
    out
}

impl Library {
    pub fn open(path: &Path) -> Result<Library, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let db = Connection::open(path).map_err(|e| format!("Base des sorties inaccessible : {e}"))?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS activities (
                id INTEGER PRIMARY KEY,
                hash TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                sport TEXT,
                start_time INTEGER,
                file_name TEXT,
                format TEXT NOT NULL,
                imported_at INTEGER NOT NULL,
                distance_m REAL NOT NULL,
                moving_s REAL NOT NULL,
                ascent_m REAL NOT NULL,
                avg_power REAL,
                avg_hr REAL,
                data BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS activities_start ON activities(start_time);
             CREATE TABLE IF NOT EXISTS climbs (
                activity_id INTEGER NOT NULL,
                idx INTEGER NOT NULL,
                start_lon REAL NOT NULL, start_lat REAL NOT NULL,
                end_lon REAL NOT NULL, end_lat REAL NOT NULL,
                length_m REAL NOT NULL,
                gain_m REAL NOT NULL,
                duration_s REAL NOT NULL,
                vam REAL NOT NULL,
                avg_power REAL,
                avg_hr REAL,
                PRIMARY KEY (activity_id, idx)
             );
             CREATE INDEX IF NOT EXISTS climbs_start ON climbs(start_lat, start_lon);
             CREATE TABLE IF NOT EXISTS coach_notes (
                activity_id INTEGER PRIMARY KEY,
                model TEXT NOT NULL,
                text TEXT NOT NULL,
                created_at INTEGER NOT NULL
             );",
        )
        .map_err(|e| e.to_string())?;

        // Colonnes ajoutées au fil des versions
        let cols: Vec<String> = {
            let mut st = db.prepare("PRAGMA table_info(activities)").map_err(|e| e.to_string())?;
            let rows = st.query_map([], |r| r.get::<_, String>(1)).map_err(|e| e.to_string())?;
            rows.flatten().collect()
        };
        for (c, ty) in [("np", "REAL"), ("max_hr", "REAL"), ("flat_speed", "REAL"), ("device_ftp", "REAL"), ("metrics_version", "INTEGER NOT NULL DEFAULT 0")] {
            if !cols.iter().any(|x| x == c) {
                db.execute(&format!("ALTER TABLE activities ADD COLUMN {c} {ty}"), []).map_err(|e| e.to_string())?;
            }
        }
        let lib = Library { db };
        lib.migrate()?;
        Ok(lib)
    }

    /// Recalcule les indicateurs des sorties importées avec une version antérieure.
    fn migrate(&self) -> Result<(), String> {
        let ids: Vec<i64> = {
            let mut st = self.db.prepare("SELECT id FROM activities WHERE metrics_version < ?1").map_err(|e| e.to_string())?;
            let rows = st.query_map(params![METRICS_VERSION], |r| r.get(0)).map_err(|e| e.to_string())?;
            rows.flatten().collect()
        };
        for id in ids {
            let a = self.load(id)?;
            let r = analyze(&a, &Athlete::default());
            self.store_metrics(id, &a, &r)?;
        }
        Ok(())
    }

    fn store_metrics(&self, id: i64, a: &Activity, r: &Analysis) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE activities SET distance_m = ?2, moving_s = ?3, ascent_m = ?4, avg_power = ?5, avg_hr = ?6,
                    np = ?7, max_hr = ?8, flat_speed = ?9, device_ftp = ?10, metrics_version = ?11 WHERE id = ?1",
                params![
                    id,
                    r.distance_m,
                    r.moving_s as f64,
                    r.ascent_m,
                    r.power.as_ref().map(|p| p.avg as f64),
                    r.avg_hr.map(|h| h as f64),
                    r.power.as_ref().map(|p| p.np as f64),
                    r.max_hr.map(|h| h as f64),
                    r.flat_speed_kmh.map(|v| v as f64),
                    a.device.ftp,
                    METRICS_VERSION
                ],
            )
            .map_err(|e| e.to_string())?;
        self.db.execute("DELETE FROM climbs WHERE activity_id = ?1", params![id]).map_err(|e| e.to_string())?;
        for (i, c) in r.climbs.iter().enumerate() {
            let (Some(s), Some(e)) = (c.start, c.end) else { continue };
            self.db
                .execute(
                    "INSERT INTO climbs (activity_id, idx, start_lon, start_lat, end_lon, end_lat, length_m, gain_m, duration_s, vam, avg_power, avg_hr)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        id,
                        i as i64,
                        s[0] as f64,
                        s[1] as f64,
                        e[0] as f64,
                        e[1] as f64,
                        c.length_m as f64,
                        c.gain_m as f64,
                        c.duration_s as f64,
                        c.vam as f64,
                        c.avg_power.map(|x| x as f64),
                        c.avg_hr.map(|x| x as f64)
                    ],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Importe des fichiers. `cycling_only` : écarte les sorties d'autres sports (import de dossier).
    /// `names` : noms à donner aux fichiers (export Strava). `progress(fait, total)` après chaque fichier.
    pub fn import(
        &mut self,
        paths: &[String],
        cycling_only: bool,
        names: &HashMap<String, (String, String)>,
        progress: impl Fn(usize, usize),
    ) -> ImportReport {
        let mut report = ImportReport::default();
        for (k, path) in paths.iter().enumerate() {
            let file = Path::new(path).file_name().and_then(|f| f.to_str()).unwrap_or(path).to_string();
            let named = names.get(&stem(&file));
            if cycling_only && named.is_some_and(|(_, kind)| !kind.is_empty() && !is_cycling(Some(kind))) {
                report.skipped.push(file);
            } else {
                match self.import_one(path, &file, cycling_only, named.map(|n| n.0.as_str())) {
                    Ok(Imported::New(s)) => report.imported.push(s),
                    Ok(Imported::Duplicate) => report.duplicates.push(file),
                    Ok(Imported::NotCycling) => report.skipped.push(file),
                    Err(e) => report.errors.push((file, e)),
                }
            }
            progress(k + 1, paths.len());
        }
        report
    }

    /// Importe tous les fichiers de sortie d'un dossier (et de ses sous-dossiers).
    pub fn import_folder(&mut self, folder: &Path, progress: impl Fn(usize, usize)) -> Result<ImportReport, String> {
        let mut files = Vec::new();
        let mut stack = vec![folder.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let rd = std::fs::read_dir(&dir).map_err(|e| format!("dossier illisible : {e}"))?;
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if is_activity_file(&p) {
                    files.push(p.to_string_lossy().to_string());
                }
            }
        }
        files.sort();
        // Export Strava : activities.csv dans le dossier choisi ou son parent
        let csv: Option<PathBuf> = [folder.join("activities.csv"), folder.parent().map(|p| p.join("activities.csv")).unwrap_or_default()]
            .into_iter()
            .find(|p| p.is_file());
        let names = csv.as_deref().map(strava_names).unwrap_or_default();
        Ok(self.import(&files, true, &names, progress))
    }

    fn import_one(&mut self, path: &str, file: &str, cycling_only: bool, name: Option<&str>) -> Result<Imported, String> {
        let mut bytes = std::fs::read(path).map_err(|e| format!("lecture impossible : {e}"))?;
        let hash = file_hash(&bytes);
        let exists: Option<i64> = self
            .db
            .query_row("SELECT id FROM activities WHERE hash = ?1", params![hash], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        if exists.is_some() {
            return Ok(Imported::Duplicate);
        }
        // Fichiers compressés de l'export Strava
        let inner = file.strip_suffix(".gz").or_else(|| file.strip_suffix(".GZ"));
        if inner.is_some() {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&bytes[..])
                .take(200 << 20)
                .read_to_end(&mut out)
                .map_err(|e| format!("décompression impossible : {e}"))?;
            bytes = out;
        }
        let mut activity = parse_activity(&bytes, inner.unwrap_or(file))?;
        if cycling_only && !is_cycling(activity.sport.as_deref()) {
            return Ok(Imported::NotCycling);
        }
        if let Some(n) = name.filter(|n| !n.is_empty()) {
            activity.name = Some(n.to_string());
        }
        let data = bincode::serialize(&activity).map_err(|e| e.to_string())?;
        let display = activity.name.clone().unwrap_or_else(|| stem(file));
        self.db
            .execute(
                "INSERT INTO activities (hash, name, sport, start_time, file_name, format, imported_at,
                                         distance_m, moving_s, ascent_m, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0, 0, ?8)",
                params![hash, display, activity.sport, activity.start_time, file, activity.format, now(), data],
            )
            .map_err(|e| e.to_string())?;
        let id = self.db.last_insert_rowid();
        let r = analyze(&activity, &Athlete::default());
        self.store_metrics(id, &activity, &r)?;
        self.summary(id).map(Imported::New)
    }

    pub fn list(&self) -> Result<Vec<ActivitySummary>, String> {
        let mut st = self
            .db
            .prepare(&format!("{SELECT_SUMMARY} ORDER BY start_time DESC, id DESC"))
            .map_err(|e| e.to_string())?;
        let rows = st.query_map([], summary_row).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn summary(&self, id: i64) -> Result<ActivitySummary, String> {
        self.db
            .query_row(&format!("{SELECT_SUMMARY} WHERE id = ?1"), params![id], summary_row)
            .map_err(|e| format!("sortie introuvable : {e}"))
    }

    pub fn load(&self, id: i64) -> Result<Activity, String> {
        let data: Vec<u8> = self
            .db
            .query_row("SELECT data FROM activities WHERE id = ?1", params![id], |r| r.get(0))
            .map_err(|e| format!("sortie introuvable : {e}"))?;
        bincode::deserialize(&data).map_err(|e| format!("sortie illisible : {e}"))
    }

    pub fn detail(&self, id: i64, athlete: &Athlete) -> Result<ActivityDetail, String> {
        let activity = self.load(id)?;
        let analysis = analyze(&activity, athlete);
        let climb_history = analysis.climbs.iter().map(|c| self.climb_history(id, c)).collect::<Result<Vec<_>, _>>()?;
        Ok(ActivityDetail { summary: self.summary(id)?, track: map_track(&activity), analysis, climb_history })
    }

    /// Passages sur la même montée dans toutes les sorties : même pied et même sommet
    /// (à 300 m près), longueur comparable.
    fn climb_history(&self, id: i64, c: &Climb) -> Result<ClimbHistory, String> {
        let (Some(s), Some(e)) = (c.start, c.end) else { return Ok(ClimbHistory { efforts: vec![], rank: 0 }) };
        let (s, e) = ([s[0] as f64, s[1] as f64], [e[0] as f64, e[1] as f64]);
        const TOL_M: f64 = 300.0;
        let (dlat, dlon) = (TOL_M / 110_574.0, TOL_M / (111_320.0 * s[1].to_radians().cos()));
        let mut st = self
            .db
            .prepare(
                "SELECT c.activity_id, a.name, a.start_time, c.duration_s, c.vam, c.avg_power, c.avg_hr,
                        c.start_lon, c.start_lat, c.end_lon, c.end_lat, c.length_m
                 FROM climbs c JOIN activities a ON a.id = c.activity_id
                 WHERE c.start_lat BETWEEN ?1 AND ?2 AND c.start_lon BETWEEN ?3 AND ?4",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![s[1] - dlat, s[1] + dlat, s[0] - dlon, s[0] + dlon], |r| {
                Ok((
                    ClimbEffort {
                        activity_id: r.get(0)?,
                        name: r.get(1)?,
                        start_time: r.get(2)?,
                        duration_s: r.get(3)?,
                        vam: r.get(4)?,
                        avg_power: r.get(5)?,
                        avg_hr: r.get(6)?,
                        current: false,
                    },
                    [r.get::<_, f64>(7)?, r.get::<_, f64>(8)?],
                    [r.get::<_, f64>(9)?, r.get::<_, f64>(10)?],
                    r.get::<_, f64>(11)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut efforts: Vec<ClimbEffort> = Vec::new();
        for row in rows.flatten() {
            let (mut eff, rs, re, len) = row;
            let ratio = len / c.length_m.max(1.0) as f64;
            if haversine_m(rs, s) > TOL_M || haversine_m(re, e) > TOL_M || !(0.7..=1.4).contains(&ratio) {
                continue;
            }
            eff.current = eff.activity_id == id;
            // Une seule entrée par sortie (la plus rapide si la montée y figure deux fois)
            match efforts.iter_mut().find(|x| x.activity_id == eff.activity_id) {
                Some(prev) if prev.duration_s <= eff.duration_s => {}
                Some(prev) => *prev = eff,
                None => efforts.push(eff),
            }
        }
        efforts.sort_by(|a, b| a.duration_s.total_cmp(&b.duration_s));
        let rank = efforts.iter().position(|x| x.current).map_or(0, |p| p + 1);
        Ok(ClimbHistory { efforts, rank })
    }

    /// Progression : charge d'entraînement, semaines, vitesse sur le plat (sorties vélo).
    pub fn progress(&self, athlete: &Athlete, tz_offset_s: i64) -> Result<Progress, String> {
        let mut st = self
            .db
            .prepare(
                "SELECT id, name, start_time, moving_s, distance_m, ascent_m, np, avg_hr, flat_speed, device_ftp, sport
                 FROM activities WHERE start_time IS NOT NULL",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    RideLoad {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        start_time: r.get(2)?,
                        moving_s: r.get(3)?,
                        distance_m: r.get(4)?,
                        ascent_m: r.get(5)?,
                        np: r.get(6)?,
                        avg_hr: r.get(7)?,
                        flat_speed_kmh: r.get(8)?,
                        device_ftp: r.get(9)?,
                    },
                    r.get::<_, Option<String>>(10)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let rides: Vec<RideLoad> = rows.flatten().filter(|(_, sport)| is_cycling(sport.as_deref())).map(|(r, _)| r).collect();
        Ok(progress(&rides, athlete, tz_offset_s, now()))
    }

    pub fn rename(&self, id: i64, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("le nom ne peut pas être vide".into());
        }
        self.db.execute("UPDATE activities SET name = ?1 WHERE id = ?2", params![name, id]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<(), String> {
        self.db.execute("DELETE FROM climbs WHERE activity_id = ?1", params![id]).map_err(|e| e.to_string())?;
        self.db.execute("DELETE FROM coach_notes WHERE activity_id = ?1", params![id]).map_err(|e| e.to_string())?;
        self.db.execute("DELETE FROM activities WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
        Ok(())
    }
}

// --- Coach ---------------------------------------------------------------------------

/// Commentaire du coach IA déjà rédigé pour une sortie.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoachNote {
    pub text: String,
    pub model: String,
    pub created_at: i64,
}

/// Repères tirés des sorties pour le coach et les itinéraires de séance.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RiderHabits {
    /// VAM médiane dans les montées d'au moins 3 min (m/h).
    pub vam_m_h: Option<f64>,
    /// Vitesse sur le plat médiane des 10 dernières sorties (km/h).
    pub flat_speed_kmh: Option<f64>,
    /// Vitesse moyenne médiane des 10 dernières sorties (km/h).
    pub avg_speed_kmh: Option<f64>,
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

impl Library {
    pub fn coach_note(&self, id: i64) -> Result<Option<CoachNote>, String> {
        self.db
            .query_row("SELECT text, model, created_at FROM coach_notes WHERE activity_id = ?1", params![id], |r| {
                Ok(CoachNote { text: r.get(0)?, model: r.get(1)?, created_at: r.get(2)? })
            })
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn save_coach_note(&self, id: i64, model: &str, text: &str) -> Result<CoachNote, String> {
        let t = now();
        self.db
            .execute(
                "INSERT OR REPLACE INTO coach_notes (activity_id, model, text, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, model, text, t],
            )
            .map_err(|e| e.to_string())?;
        Ok(CoachNote { text: text.to_string(), model: model.to_string(), created_at: t })
    }

    /// Sorties vélo les plus récentes (identifiants, de la plus récente à la plus ancienne).
    pub fn recent_cycling_ids(&self, n: usize) -> Result<Vec<i64>, String> {
        let mut st = self
            .db
            .prepare("SELECT id, sport FROM activities WHERE start_time IS NOT NULL ORDER BY start_time DESC")
            .map_err(|e| e.to_string())?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))).map_err(|e| e.to_string())?;
        Ok(rows.flatten().filter(|(_, s)| is_cycling(s.as_deref())).map(|(id, _)| id).take(n).collect())
    }

    /// FTP réglée sur le compteur lors de la dernière sortie qui l'indique.
    pub fn latest_device_ftp(&self) -> Result<Option<f64>, String> {
        self.db
            .query_row(
                "SELECT device_ftp FROM activities WHERE device_ftp > 0 ORDER BY start_time DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn habits(&self) -> Result<RiderHabits, String> {
        let vams: Vec<f64> = {
            let mut st = self
                .db
                .prepare("SELECT vam FROM climbs WHERE duration_s >= 180 ORDER BY activity_id DESC LIMIT 60")
                .map_err(|e| e.to_string())?;
            let rows = st.query_map([], |r| r.get::<_, f64>(0)).map_err(|e| e.to_string())?;
            rows.flatten().collect()
        };
        let mut st = self
            .db
            .prepare(
                "SELECT flat_speed, distance_m, moving_s, sport FROM activities
                 WHERE start_time IS NOT NULL ORDER BY start_time DESC LIMIT 30",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(Option<f64>, f64, f64, Option<String>)> = st
            .query_map([], |r| Ok((r.get::<_, Option<f64>>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, Option<String>>(3)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|r| is_cycling(r.3.as_deref()))
            .take(10)
            .collect();
        Ok(RiderHabits {
            vam_m_h: median(vams),
            flat_speed_kmh: median(rows.iter().filter_map(|r| r.0).collect()),
            avg_speed_kmh: median(rows.iter().filter(|r| r.2 > 600.0).map(|r| r.1 / r.2 * 3.6).collect()),
        })
    }
}

enum Imported {
    New(ActivitySummary),
    Duplicate,
    NotCycling,
}

/// Trace pour la carte, réduite à 4 000 points au plus.
fn map_track(a: &Activity) -> MapTrack {
    let idx: Vec<usize> = (0..a.len()).filter(|&i| a.lon[i].is_finite() && a.lat[i].is_finite()).collect();
    let step = (idx.len() / 4000).max(1);
    let mut t = MapTrack { coords: Vec::new(), ele: Vec::new(), dist: Vec::new() };
    for (k, &i) in idx.iter().enumerate() {
        if k % step != 0 && k + 1 != idx.len() {
            continue;
        }
        t.coords.push([a.lon[i] as f64, a.lat[i] as f64]);
        t.ele.push(a.ele[i].is_finite().then_some(a.ele[i] as f64));
        t.dist.push(a.dist[i] as f64);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const GPX: &str = r#"<?xml version="1.0"?><gpx version="1.1" xmlns="http://www.topografix.com/GPX/1/1"><trk><name>Test</name><type>cycling</type><trkseg>
<trkpt lat="48.10" lon="-1.70"><ele>40</ele><time>2026-08-12T07:00:00Z</time></trkpt>
<trkpt lat="48.101" lon="-1.70"><ele>41</ele><time>2026-08-12T07:00:10Z</time></trkpt>
<trkpt lat="48.102" lon="-1.70"><ele>43</ele><time>2026-08-12T07:00:20Z</time></trkpt>
</trkseg></trk></gpx>"#;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("goride-lib-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn import_list_detail_delete() {
        let dir = tmp("a");
        let file = dir.join("sortie.gpx");
        std::fs::write(&file, GPX).unwrap();
        let bad = dir.join("faux.fit");
        std::fs::write(&bad, b"pas un fit").unwrap();

        let mut lib = Library::open(&dir.join("lib.db")).unwrap();
        let paths = vec![file.to_string_lossy().to_string(), bad.to_string_lossy().to_string()];
        let r = lib.import(&paths, false, &HashMap::new(), |_, _| {});
        assert_eq!(r.imported.len(), 1);
        assert_eq!(r.errors.len(), 1);
        assert_eq!(r.imported[0].name, "Test");
        assert!(r.imported[0].distance_m > 200.0, "indicateurs enregistrés");

        // Deuxième import du même fichier : doublon
        let r2 = lib.import(&paths[..1], false, &HashMap::new(), |_, _| {});
        assert!(r2.imported.is_empty() && r2.duplicates.len() == 1);

        let list = lib.list().unwrap();
        assert_eq!(list.len(), 1);
        let d = lib.detail(list[0].id, &Athlete::default()).unwrap();
        assert_eq!(d.track.coords.len(), 3);
        assert!((d.analysis.distance_m - 222.4).abs() < 1.0);

        let p = lib.progress(&Athlete::default(), 3600).unwrap();
        assert_eq!(p.rides.len(), 1);
        assert!(!p.days.is_empty() && !p.weeks.is_empty());

        lib.rename(list[0].id, "Tour du lac").unwrap();
        assert_eq!(lib.list().unwrap()[0].name, "Tour du lac");
        lib.delete(list[0].id).unwrap();
        assert!(lib.list().unwrap().is_empty());
        drop(lib);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strava_archive_folder() {
        let dir = tmp("b");
        let acts = dir.join("activities");
        std::fs::create_dir_all(&acts).unwrap();
        // Une sortie vélo compressée, une course à pied
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(GPX.as_bytes()).unwrap();
        std::fs::write(acts.join("111.gpx.gz"), gz.finish().unwrap()).unwrap();
        std::fs::write(acts.join("222.gpx"), GPX.replace("cycling", "running")).unwrap();
        std::fs::write(
            dir.join("activities.csv"),
            "Activity ID,Activity Date,Activity Name,Activity Type,Filename\n\
             111,\"Aug 12, 2026\",Tour de Bécherel,Ride,activities/111.gpx.gz\n\
             222,\"Aug 13, 2026\",Footing,Run,activities/222.gpx\n",
        )
        .unwrap();

        let mut lib = Library::open(&dir.join("lib.db")).unwrap();
        let r = lib.import_folder(&dir, |_, _| {}).unwrap();
        assert_eq!(r.imported.len(), 1, "{r:?}");
        assert_eq!(r.imported[0].name, "Tour de Bécherel");
        assert_eq!(r.skipped, vec!["222.gpx".to_string()]);
        drop(lib);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sortie GPX fictive : 1 km de plat puis 1,5 km de montée à 5 % (vitesse en montée choisie).
    fn climb_gpx(name: &str, day: u32, climb_kmh: f64) -> String {
        let mut pts = String::new();
        let (mut t, mut d) = (0.0f64, 0.0f64);
        while d <= 3_000.0 {
            let climbing = (1_000.0..2_500.0).contains(&d);
            let ele = if d < 1_000.0 { 50.0 } else if climbing { 50.0 + (d - 1_000.0) * 0.05 } else { 125.0 };
            let lat = 48.1 + d / 111_000.0;
            let (h, m, sec) = ((t as u32) / 3600, (t as u32 / 60) % 60, (t as u32) % 60);
            pts.push_str(&format!(
                "<trkpt lat=\"{lat:.6}\" lon=\"-1.700000\"><ele>{ele:.1}</ele><time>2026-08-{day:02}T{:02}:{m:02}:{sec:02}Z</time></trkpt>\n",
                7 + h
            ));
            let v = if climbing { climb_kmh } else { 30.0 } / 3.6;
            t += 2.0;
            d += v * 2.0;
        }
        format!(r#"<?xml version="1.0"?><gpx version="1.1" xmlns="http://www.topografix.com/GPX/1/1"><trk><name>{name}</name><trkseg>{pts}</trkseg></trk></gpx>"#)
    }

    #[test]
    fn same_climb_across_rides() {
        let dir = tmp("c");
        let files: Vec<String> = [("Lente", 1, 12.0), ("Rapide", 2, 16.0), ("Moyenne", 3, 14.0)]
            .iter()
            .map(|(n, d, v)| {
                let p = dir.join(format!("{n}.gpx"));
                std::fs::write(&p, climb_gpx(n, *d, *v)).unwrap();
                p.to_string_lossy().to_string()
            })
            .collect();
        let mut lib = Library::open(&dir.join("lib.db")).unwrap();
        let r = lib.import(&files, false, &HashMap::new(), |_, _| {});
        assert_eq!(r.imported.len(), 3, "{r:?}");
        let moyenne = r.imported.iter().find(|s| s.name == "Moyenne").unwrap().id;
        let d = lib.detail(moyenne, &Athlete::default()).unwrap();
        assert_eq!(d.analysis.climbs.len(), 1, "{:?}", d.analysis.climbs);
        let h = &d.climb_history[0];
        let order: Vec<&str> = h.efforts.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(order, vec!["Rapide", "Moyenne", "Lente"]);
        assert_eq!(h.rank, 2);
        assert!(h.efforts[1].current);
        drop(lib);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sports() {
        assert!(is_cycling(None));
        assert!(is_cycling(Some("cycling")));
        assert!(is_cycling(Some("Virtual Ride")));
        assert!(!is_cycling(Some("running")));
        assert!(!is_cycling(Some("Hike")));
        assert_eq!(stem("activities/123.fit.gz"), "123");
    }
}

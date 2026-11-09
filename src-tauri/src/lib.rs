//! Pont entre l'interface (carte) et le cœur Rust.
//! Toute la logique vit dans `goride-core` ; ici on expose des commandes
//! et on garde en mémoire le réseau routier chargé.

mod coach;
mod library;
mod regions;
mod routes;

use goride_core::activity::analysis::Athlete;
use goride_core::dem::Dem;
use goride_core::activity::progress::Progress;
use library::{ActivityDetail, ActivitySummary, CoachNote, ImportReport, Library};
use coach::{ChatMessage, CoachStatus};
use goride_core::workout::{templates, Terrain, Workout};
use goride_core::workout_route::{rank_for_workout, KeySection, Need};
use routes::{RouteMeta, SavedRoute, SavedRouteSummary};
use goride_core::graph::elevation::compute_elevation;
use goride_core::graph::pbf::{graph_from_pbfs, BuildStep};
use regions::Regions;
use goride_core::graph::{merge_graphs, Graph, FORMAT_VERSION};
use goride_core::loops::{propose_loops, LoopProposal, LoopRequest};
use goride_core::osm_tags::{RoadClass, Surface};
use goride_core::route_track::{summarize, RouteSummary};
use goride_core::router::{Leg, RouteOptions, Router};
use goride_core::Track;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};

struct AppState {
    router: RwLock<Option<Arc<Router>>>,
    region: Mutex<Option<String>>,
    busy: Mutex<bool>,
    /// Message à afficher quand aucun réseau n'est utilisable (ex. format obsolète).
    notice: Mutex<Option<String>>,
    /// Empreintes des boucles déjà montrées (pour « Autres propositions »).
    loop_footprints: Mutex<HashMap<u64, Vec<(u32, f32)>>>,
    /// Sorties importées (base SQLite locale).
    library: Mutex<Option<Library>>,
    dem: Arc<Dem>,
    graphs_dir: PathBuf,
    /// Clé du réseau actif (« aquitaine+poitou-charentes », ou nom d'un extrait choisi à la main).
    network_key: Mutex<Option<String>>,
    /// Régions Geofabrik : contours et extraits téléchargés.
    regions: Regions,
    /// Demande d'arrêt du téléchargement en cours.
    cancel_download: AtomicBool,
    /// Réglages de l'application (dossier des itinéraires…).
    settings_path: PathBuf,
    /// Demandes au coach en cours : identifiant → drapeau d'arrêt.
    coach_jobs: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    /// Séances enregistrées (« Mes séances »).
    workouts_path: PathBuf,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct NetworkStatus {
    loaded: bool,
    busy: bool,
    region: Option<String>,
    segments: usize,
    bbox: Option<[f32; 4]>,
    /// Le réseau connaît le relief (curseur dénivelé et pente max actifs).
    elevation: bool,
    notice: Option<String>,
}

fn status_of(st: &AppState) -> NetworkStatus {
    let router = st.router.read().unwrap();
    NetworkStatus {
        loaded: router.is_some(),
        busy: *st.busy.lock().unwrap(),
        region: st.region.lock().unwrap().clone(),
        segments: router.as_ref().map_or(0, |r| r.graph.seg_count()),
        bbox: router.as_ref().map(|r| r.graph.bbox()),
        elevation: router.as_ref().is_some_and(|r| r.graph.has_elevation),
        notice: st.notice.lock().unwrap().clone(),
    }
}

fn router_of(st: &AppState) -> Result<Arc<Router>, String> {
    st.router
        .read()
        .unwrap()
        .clone()
        .ok_or_else(|| "Aucun réseau routier chargé : charge d'abord une région (.osm.pbf).".to_string())
}

/// Refuse clairement un point situé hors de la région chargée (sinon le moteur répond
/// seulement « aucune route praticable », ce qui laisse croire à un problème de route).
fn ensure_in_region(st: &AppState, router: &Router, p: [f64; 2]) -> Result<(), String> {
    let [w, s, e, n] = router.graph.bbox().map(f64::from);
    const MARGIN: f64 = 0.02; // ~2 km
    if p[0] >= w - MARGIN && p[0] <= e + MARGIN && p[1] >= s - MARGIN && p[1] <= n + MARGIN {
        return Ok(());
    }
    let region = st.region.lock().unwrap().clone().unwrap_or_else(|| "chargée".into());
    Err(format!(
        "Ce point est hors de la région chargée ({region}). Pour tracer ici, charge l'extrait OpenStreetMap de cette région avec le bouton « Réseau » en haut à droite."
    ))
}

/// Clé d'un réseau en cache : le nom de son fichier sans « .graph ».
fn graph_key(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or("reseau").to_string()
}

fn region_name(path: &Path) -> String {
    let stem = path.file_name().and_then(|s| s.to_str()).unwrap_or("région");
    let stem = stem.trim_end_matches(".graph").trim_end_matches(".osm.pbf").trim_end_matches("-latest");
    // « bretagne-261107 » (date de l'extrait Geofabrik) → « bretagne »
    let stem = match stem.rsplit_once('-') {
        Some((head, tail)) if !head.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => head,
        _ => stem,
    };
    let mut c = stem.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Nom affiché d'un réseau à partir de sa clé (nom du fichier .graph).
fn network_display(key: &str) -> String {
    regions::display_name(key).unwrap_or_else(|| {
        // « france-261108-internal » → « France » : sans date d'extrait ni suffixe Geofabrik
        let words: Vec<&str> = key
            .split(['-', '_'])
            .filter(|w| !w.is_empty() && !w.chars().all(|c| c.is_ascii_digit()) && !["internal", "latest", "osm"].contains(w))
            .collect();
        let name = if words.is_empty() { key.to_string() } else { words.join("-") };
        let mut c = name.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    })
}

/// Charge un graphe déjà construit et l'active (`key` : nom de son fichier .graph).
fn activate(app: &AppHandle, graph: Graph, key: &str) {
    let region = network_display(key);
    let t = Instant::now();
    let router = Router::new(graph);
    eprintln!("Index spatial construit en {:.1} s", t.elapsed().as_secs_f32());
    let st = app.state::<AppState>();
    *st.router.write().unwrap() = Some(Arc::new(router));
    *st.region.lock().unwrap() = Some(region);
    *st.network_key.lock().unwrap() = Some(key.to_string());
    *st.notice.lock().unwrap() = None;
    let _ = app.emit("network-status", status_of(&st));
}

/// Construit le réseau depuis un ou plusieurs .osm.pbf voisins (routes puis altitudes),
/// l'enregistre sous `key` et l'active.
fn build_and_activate(app: &AppHandle, pbfs: &[PathBuf], key: &str) -> Result<(), String> {
    let t = Instant::now();
    let emit = |msg: String| {
        let _ = app.emit("network-progress", msg);
    };
    let mut graph = graph_from_pbfs(pbfs, |step| match step {
        BuildStep::ReadingWays => emit("Lecture des routes…".into()),
        BuildStep::ReadingNodes { ways } => emit(format!("{ways} voies retenues, lecture des positions…")),
        BuildStep::Building { nodes } => emit(format!("{nodes} points, assemblage du réseau…")),
        BuildStep::Done => {}
    })?;
    eprintln!("Réseau construit en {:.1} s : {} tronçons", t.elapsed().as_secs_f32(), graph.seg_count());

    // Altitudes : tuiles de terrain téléchargées une fois, puis gardées en cache
    let st = app.state::<AppState>();
    let dem = st.dem.clone();
    let t = Instant::now();
    compute_elevation(&mut graph, |pts| {
        dem.sample_many(pts, |done, total| {
            if done % 10 == 0 || done == total {
                emit(format!("Relief : {done} / {total} zones…"));
            }
        })
    });
    eprintln!("Altitudes en {:.1} s (disponibles : {})", t.elapsed().as_secs_f32(), graph.has_elevation);

    emit("Enregistrement…".into());
    std::fs::create_dir_all(&st.graphs_dir).map_err(|e| e.to_string())?;
    let out = st.graphs_dir.join(format!("{key}.graph"));
    graph.save(&out).map_err(|e| format!("Enregistrement du réseau impossible : {e}"))?;
    // On note d'où vient le réseau (un fichier par ligne) pour pouvoir le reconstruire seul plus tard
    let sources: Vec<String> = pbfs.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let _ = std::fs::write(st.graphs_dir.join(format!("{key}.source")), sources.join("\n"));
    emit("Préparation de l'index…".into());
    activate(app, graph, key);
    // Les réseaux en cache pèsent lourd : on garde les quatre plus récents
    regions::prune_graphs(&st.graphs_dir, 4, &out);
    if !app.state::<AppState>().router.read().unwrap().as_ref().is_some_and(|r| r.graph.has_elevation) {
        let _ = app.emit(
            "network-warning",
            "Relief indisponible (pas d'accès aux tuiles de terrain) : le curseur dénivelé et la pente max sont sans effet.".to_string(),
        );
    }
    Ok(())
}

// --- Réglages de l'application -----------------------------------------------------

fn read_settings(st: &AppState) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(&st.settings_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_settings(st: &AppState, s: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    if let Some(dir) = st.settings_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(&st.settings_path, text).map_err(|e| format!("Réglages non enregistrés : {e}"))
}

/// Dossier des itinéraires, s'il a déjà été choisi.
fn routes_dir(st: &AppState) -> Option<PathBuf> {
    read_settings(st).get("routesDir").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(PathBuf::from)
}

fn routes_dir_or_err(st: &AppState) -> Result<PathBuf, String> {
    routes_dir(st).ok_or_else(|| "Aucun dossier choisi pour les itinéraires.".to_string())
}

// --- Commandes ----------------------------------------------------------------

/// Lit un fichier GPX sur le disque et renvoie la trace calculée.
#[tauri::command]
async fn load_track(path: String) -> Result<Track, String> {
    let path = PathBuf::from(path);
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if ext != "gpx" {
        return Err(format!(
            "Format « .{ext} » pas encore pris en charge : seuls les fichiers GPX le sont pour l'instant (FIT et TCX arrivent ensuite)."
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("Impossible de lire le fichier : {e}"))?;
    goride_core::parse_gpx(&bytes).map_err(|e| e.to_string())
}

#[tauri::command]
fn network_status(st: State<'_, AppState>) -> NetworkStatus {
    status_of(&st)
}

/// Construit le réseau routier à partir d'un extrait OSM (.osm.pbf), l'enregistre et l'active.
#[tauri::command]
async fn build_network(app: AppHandle, path: String) -> Result<NetworkStatus, String> {
    {
        let st = app.state::<AppState>();
        let mut busy = st.busy.lock().unwrap();
        if *busy {
            return Err("Une construction est déjà en cours.".into());
        }
        *busy = true;
    }
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let key = region_name(Path::new(&path)).to_lowercase().replace('+', "-");
        build_and_activate(&app2, &[PathBuf::from(&path)], &key)
    })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);

    let st = app.state::<AppState>();
    *st.busy.lock().unwrap() = false;
    let status = status_of(&st);
    let _ = app.emit("network-status", status.clone());
    result.map(|_| status)
}

/// Accroche un point cliqué sur la route autorisée la plus proche.
#[tauri::command]
async fn snap_point(st: State<'_, AppState>, point: [f64; 2], options: RouteOptions) -> Result<[f64; 2], String> {
    let router = router_of(&st)?;
    ensure_in_region(&st, &router, point)?;
    router.snap(point, &options).map(|s| s.point).map_err(|e| e.to_string())
}

/// Calcule une étape entre deux points de passage.
#[tauri::command]
async fn route_leg(
    st: State<'_, AppState>,
    from: [f64; 2],
    to: [f64; 2],
    options: RouteOptions,
) -> Result<Leg, String> {
    let router = router_of(&st)?;
    ensure_in_region(&st, &router, from)?;
    ensure_in_region(&st, &router, to)?;
    tauri::async_runtime::spawn_blocking(move || {
        let a = router.snap(from, &options).map_err(|e| e.to_string())?;
        let b = router.snap(to, &options).map_err(|e| e.to_string())?;
        router.route(&a, &b, &options).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Propose des boucles. `exclude` : identifiants des boucles déjà montrées, à ne pas reproposer.
#[tauri::command]
async fn loop_proposals(
    st: State<'_, AppState>,
    request: LoopRequest,
    options: RouteOptions,
    exclude: Vec<u64>,
) -> Result<Vec<LoopProposal>, String> {
    let router = router_of(&st)?;
    ensure_in_region(&st, &router, request.start)?;
    let excluded: Vec<Vec<(u32, f32)>> = {
        let cache = st.loop_footprints.lock().unwrap();
        exclude.iter().filter_map(|id| cache.get(id).cloned()).collect()
    };
    let t = Instant::now();
    let loops = tauri::async_runtime::spawn_blocking(move || propose_loops(&router, &request, &options, &excluded))
        .await
        .map_err(|e| e.to_string())??;
    eprintln!("{} boucles en {} ms", loops.len(), t.elapsed().as_millis());
    let mut cache = st.loop_footprints.lock().unwrap();
    if cache.len() > 500 {
        cache.clear();
    }
    for l in &loops {
        cache.insert(l.id, l.footprint.clone());
    }
    Ok(loops)
}

// --- Sorties -------------------------------------------------------------------------

fn with_library<T>(st: &AppState, f: impl FnOnce(&mut Library) -> Result<T, String>) -> Result<T, String> {
    let mut guard = st.library.lock().unwrap();
    let lib = guard.as_mut().ok_or("La base des sorties n'a pas pu être ouverte.")?;
    f(lib)
}

/// Importe des fichiers (ou un dossier entier) en tâche de fond, avec progression.
#[tauri::command]
async fn import_activities(app: AppHandle, paths: Vec<String>, folder: Option<String>) -> Result<ImportReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let progress = |done: usize, total: usize| {
            if total > 3 && (done % 5 == 0 || done == total) {
                let _ = app.emit("import-progress", (done, total));
            }
        };
        with_library(&st, |l| match &folder {
            Some(f) => l.import_folder(Path::new(f), progress),
            None => Ok(l.import(&paths, false, &HashMap::new(), progress)),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Progression : forme, fatigue, semaines, vitesse sur le plat.
#[tauri::command]
async fn ride_progress(st: State<'_, AppState>, athlete: Athlete, tz_offset_min: i64) -> Result<Progress, String> {
    with_library(&st, |l| l.progress(&athlete, tz_offset_min * 60))
}

#[tauri::command]
async fn list_activities(st: State<'_, AppState>) -> Result<Vec<ActivitySummary>, String> {
    with_library(&st, |l| l.list())
}

#[tauri::command]
async fn activity_detail(st: State<'_, AppState>, id: i64, athlete: Athlete) -> Result<ActivityDetail, String> {
    with_library(&st, |l| l.detail(id, &athlete))
}

#[tauri::command]
async fn rename_activity(st: State<'_, AppState>, id: i64, name: String) -> Result<(), String> {
    with_library(&st, |l| l.rename(id, &name))
}

#[tauri::command]
async fn delete_activity(st: State<'_, AppState>, id: i64) -> Result<(), String> {
    with_library(&st, |l| l.delete(id))
}

/// Bilan de l'itinéraire complet : altitude, D+, revêtements, types de route.
#[tauri::command]
async fn summarize_route(
    st: State<'_, AppState>,
    coords: Vec<[f64; 2]>,
    surface: Vec<Surface>,
    class: Vec<RoadClass>,
) -> Result<RouteSummary, String> {
    let dem = st.dem.clone();
    tauri::async_runtime::spawn_blocking(move || summarize(&coords, &surface, &class, Some(&dem)))
        .await
        .map_err(|e| e.to_string())
}

/// Altitude d'un point de la carte (tuiles de terrain, gardées en cache).
#[tauri::command]
async fn elevation_at(st: State<'_, AppState>, point: [f64; 2]) -> Result<Option<f64>, String> {
    let dem = st.dem.clone();
    tauri::async_runtime::spawn_blocking(move || dem.elevations(&[point]).into_iter().next().flatten())
        .await
        .map_err(|e| e.to_string())
}

/// Enregistre l'itinéraire en GPX ou en parcours FIT Garmin, selon l'extension choisie.
#[tauri::command]
async fn save_route(
    path: String,
    name: String,
    coords: Vec<[f64; 2]>,
    ele: Vec<Option<f64>>,
    speed_kmh: f64,
) -> Result<(), String> {
    if coords.len() != ele.len() || coords.len() < 2 {
        return Err("Itinéraire vide ou incohérent.".into());
    }
    let track = Track::from_points(Some(name.clone()), coords.into_iter().zip(ele).collect());
    let is_fit = Path::new(&path).extension().is_some_and(|e| e.eq_ignore_ascii_case("fit"));
    let bytes = if is_fit {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        goride_core::fit_export::to_fit_course(&name, &track, speed_kmh, now)
    } else {
        goride_core::gpx_export::to_gpx(&name, &track).into_bytes()
    };
    std::fs::write(&path, bytes).map_err(|e| format!("Écriture impossible : {e}"))
}

// --- Régions chargées automatiquement -------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RegionInfo {
    slug: &'static str,
    name: &'static str,
    size_mb: u32,
    /// Extrait déjà sur le disque : pas de téléchargement.
    downloaded: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Coverage {
    /// Tous les points sont dans le réseau actif.
    covered: bool,
    /// Régions à charger sinon (vide : hors de France métropolitaine).
    regions: Vec<RegionInfo>,
    /// Ce réseau a déjà été construit : il se recharge en quelques secondes.
    cached: bool,
    /// Nom lisible de l'ensemble (« Aquitaine + Poitou-Charentes »).
    name: String,
}

/// Extrait OSM d'une région déjà présent sur le disque (téléchargé par GoRide, ou choisi
/// à la main auparavant avec le bouton « Réseau »).
fn local_pbf(st: &AppState, slug: &str) -> Option<PathBuf> {
    let p = st.regions.pbf_path(slug);
    if p.exists() {
        return Some(p);
    }
    std::fs::read_to_string(st.graphs_dir.join(format!("{slug}.source")))
        .ok()
        .and_then(|t| {
            let lines: Vec<&str> = t.lines().filter(|l| !l.trim().is_empty()).collect();
            (lines.len() == 1).then(|| PathBuf::from(lines[0].trim()))
        })
        .filter(|p| p.exists())
}

/// Extraits OSM de ces régions : ceux déjà sur le disque, les autres téléchargés un à un.
/// Ce que coûte une région avant de pouvoir tracer dedans. Sur téléphone : le réseau prêt
/// à l'emploi (compressé, environ six fois plus léger que l'extrait OpenStreetMap).
fn region_info(st: &AppState, d: &'static regions::RegionDef) -> RegionInfo {
    if cfg!(mobile) {
        RegionInfo { slug: d.slug, name: d.name, size_mb: d.size_mb.div_ceil(6), downloaded: st.graphs_dir.join(format!("{}.graph", d.slug)).exists() }
    } else {
        RegionInfo { slug: d.slug, name: d.name, size_mb: d.size_mb, downloaded: local_pbf(st, d.slug).is_some() }
    }
}

/// Réseaux prêts à l'emploi, un par région, fabriqués et publiés chaque mois sur GitHub
/// par `.github/workflows/reseaux.yml`. Le numéro suit le format du graphe : une version
/// de GoRide ne télécharge que des réseaux qu'elle sait lire.
fn prebuilt_url(slug: &str) -> String {
    format!("https://github.com/MoreIIo/GoRide/releases/download/reseaux-v{FORMAT_VERSION}/{slug}.graph.gz")
}

/// Téléphone : récupère le réseau de chaque région (en cache, sinon téléchargé tout prêt).
/// Rien à construire : pas d'extrait OpenStreetMap ni de relief à calculer sur l'appareil.
fn fetch_prebuilt(app: &AppHandle, defs: &[&'static regions::RegionDef]) -> Result<Vec<Graph>, String> {
    use std::io::{Read, Write};
    let st = app.state::<AppState>();
    let emit = |msg: String| {
        let _ = app.emit("network-progress", msg);
    };
    std::fs::create_dir_all(&st.graphs_dir).map_err(|e| e.to_string())?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(20))
        .timeout_read(std::time::Duration::from_secs(60))
        .user_agent("GoRide (planificateur d'itinéraires vélo)")
        .build();
    let n = defs.len();
    let mut out = Vec::with_capacity(n);
    for (i, d) in defs.iter().enumerate() {
        let step = if n > 1 { format!(" ({}/{n})", i + 1) } else { String::new() };
        let path = st.graphs_dir.join(format!("{}.graph", d.slug));
        if path.exists() {
            emit(format!("Chargement {}{step}…", d.name));
            match Graph::load(&path) {
                Ok(g) => {
                    out.push(g);
                    continue;
                }
                Err(e) => eprintln!("Réseau {path:?} inutilisable, nouveau téléchargement : {e}"),
            }
        }
        emit(format!("Téléchargement {}{step}…", d.name));
        let resp = agent.get(&prebuilt_url(d.slug)).call().map_err(|e| match e {
            ureq::Error::Status(404, _) => format!("Le réseau {} n'est pas encore publié pour cette version de GoRide.", d.name),
            e => format!("Téléchargement de {} impossible : {e}", d.name),
        })?;
        let total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok());

        // Octets reçus (compressés) comptés au passage, pour la barre de progression
        struct Counting<'a, R: Read, F: Fn(u64)> {
            inner: R,
            done: u64,
            last: u64,
            cancel: &'a AtomicBool,
            report: F,
        }
        impl<R: Read, F: Fn(u64)> Read for Counting<'_, R, F> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(std::io::Error::other("Téléchargement annulé."));
                }
                let k = self.inner.read(buf)?;
                self.done += k as u64;
                if self.done - self.last >= 1 << 20 {
                    self.last = self.done;
                    (self.report)(self.done);
                }
                Ok(k)
            }
        }
        let mb = |b: u64| b / 1_000_000;
        let reader = Counting {
            inner: resp.into_reader(),
            done: 0,
            last: 0,
            cancel: &st.cancel_download,
            report: |done: u64| {
                emit(match total {
                    Some(t) => format!("Téléchargement {}{step} : {} / {} Mo", d.name, mb(done), mb(t)),
                    None => format!("Téléchargement {}{step} : {} Mo", d.name, mb(done)),
                })
            },
        };
        // Décompressé à la volée et gardé tel quel : il se rechargera vite la fois suivante.
        let part = path.with_extension("graph.part");
        let result: Result<(), String> = (|| {
            let mut gz = flate2::read::GzDecoder::new(reader);
            let mut file = std::io::BufWriter::new(std::fs::File::create(&part).map_err(|e| e.to_string())?);
            std::io::copy(&mut gz, &mut file).map_err(|e| format!("Téléchargement de {} interrompu : {e}", d.name))?;
            file.flush().map_err(|e| e.to_string())
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
        emit(format!("Chargement {}{step}…", d.name));
        out.push(Graph::load(&path).map_err(|e| format!("Réseau {} illisible : {e}", d.name))?);
    }
    Ok(out)
}

/// Téléphone : réseau des régions demandées, assemblé si elles sont plusieurs, puis activé.
fn load_prebuilt(app: &AppHandle, defs: &[&'static regions::RegionDef], key: &str) -> Result<(), String> {
    let st = app.state::<AppState>();
    let parts = fetch_prebuilt(app, defs)?;
    let single = parts.len() == 1;
    if !single {
        let _ = app.emit("network-progress", "Assemblage des régions…".to_string());
    }
    let graph = merge_graphs(parts);
    let out = st.graphs_dir.join(format!("{key}.graph"));
    if !single {
        // L'assemblage est gardé : au prochain démarrage, il se recharge directement.
        graph.save(&out).map_err(|e| format!("Enregistrement du réseau impossible : {e}"))?;
    }
    let _ = app.emit("network-progress", "Préparation de l'index…".to_string());
    activate(app, graph, key);
    // Chaque réseau pèse plusieurs dizaines de Mo : on garde les six plus récents
    regions::prune_graphs(&st.graphs_dir, 6, &out);
    Ok(())
}

fn fetch_pbfs(app: &AppHandle, defs: &[&'static regions::RegionDef]) -> Result<Vec<PathBuf>, String> {
    let st = app.state::<AppState>();
    let emit = |msg: String| {
        let _ = app.emit("network-progress", msg);
    };
    let missing_mb: u64 = defs.iter().filter(|d| local_pbf(&st, d.slug).is_none()).map(|d| d.size_mb as u64).sum();
    let to_get = defs.iter().filter(|d| local_pbf(&st, d.slug).is_none()).count();
    let mut got_mb = 0u64;
    let mut k = 0;
    let mut out = Vec::with_capacity(defs.len());
    for d in defs {
        if let Some(p) = local_pbf(&st, d.slug) {
            out.push(p);
            continue;
        }
        k += 1;
        let step = if to_get > 1 { format!(" ({k}/{to_get})") } else { String::new() };
        emit(format!("Téléchargement {}{step}…", d.name));
        let path = st.regions.ensure_pbf(d, &st.cancel_download, |done, total| {
            let mb = |b: u64| b / 1_000_000;
            let overall = if to_get > 1 {
                format!(" · total {:.1} / {:.1} Go", (got_mb + mb(done)) as f64 / 1000.0, missing_mb as f64 / 1000.0)
            } else {
                String::new()
            };
            emit(match total {
                Some(t) => format!("Téléchargement {}{step} : {} / {} Mo{overall}", d.name, mb(done), mb(t)),
                None => format!("Téléchargement {}{step} : {} Mo{overall}", d.name, mb(done)),
            });
        })?;
        got_mb += d.size_mb as u64;
        out.push(path);
    }
    Ok(out)
}

/// Les points sont-ils dans le réseau actif ? Sinon, quelles régions charger ?
/// `keep` : points de l'itinéraire en cours, à garder calculables.
#[tauri::command]
async fn coverage_for(app: AppHandle, points: Vec<[f64; 2]>, keep: Vec<[f64; 2]>) -> Result<Coverage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let key = st.network_key.lock().unwrap().clone();
        let loaded_slugs = key.as_deref().and_then(regions::slugs_of_key);
        let router = st.router.read().unwrap().clone();

        // Réseau choisi à la main (hors catalogue) : on s'en tient à son emprise
        if let (Some(router), None) = (&router, &loaded_slugs) {
            if points.iter().all(|&p| ensure_in_region(&st, router, p).is_ok()) {
                return Ok(Coverage { covered: true, regions: vec![], cached: true, name: String::new() });
            }
        }
        let polys = st.regions.all()?;
        if let (Some(_), Some(slugs)) = (&router, &loaded_slugs) {
            let inside = |p: [f64; 2]| polys.iter().any(|(d, poly)| slugs.contains(&d.slug) && poly.contains(p));
            if points.iter().all(|&p| inside(p)) {
                return Ok(Coverage { covered: true, regions: vec![], cached: true, name: String::new() });
            }
        }
        let needed = regions::needed_regions(&polys, &points, &keep);
        let key = regions::key_of(&needed);
        let regions = needed.iter().filter_map(|s| regions::def(s)).map(|d| region_info(&st, d)).collect();
        Ok(Coverage {
            covered: false,
            regions,
            cached: !key.is_empty() && st.graphs_dir.join(format!("{key}.graph")).exists(),
            name: regions::display_name(&key).unwrap_or_default(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Charge (téléchargement, construction ou cache) le réseau couvrant ces régions et l'active.
#[tauri::command]
async fn load_regions(app: AppHandle, slugs: Vec<String>) -> Result<NetworkStatus, String> {
    {
        let st = app.state::<AppState>();
        let mut busy = st.busy.lock().unwrap();
        if *busy {
            return Err("Un réseau est déjà en préparation, encore un instant…".into());
        }
        *busy = true;
    }
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let st = app2.state::<AppState>();
        let refs: Vec<&str> = slugs.iter().map(String::as_str).collect();
        let key = regions::key_of(&refs);
        st.cancel_download.store(false, Ordering::Relaxed);
        let defs: Vec<&'static regions::RegionDef> = regions::slugs_of_key(&key)
            .ok_or("Aucune région à charger.")?
            .into_iter()
            .filter_map(regions::def)
            .collect();
        let emit = |msg: String| {
            let _ = app2.emit("network-progress", msg);
        };
        let cached = st.graphs_dir.join(format!("{key}.graph"));
        if cached.exists() {
            emit(format!("Chargement : {}…", network_display(&key)));
            if key == regions::FRANCE_KEY {
                *st.router.write().unwrap() = None;
            }
            match Graph::load(&cached) {
                Ok(g) => {
                    // Réseau utilisé : il devient le plus récent (et sera rechargé au démarrage)
                    let _ = std::fs::File::options().append(true).open(&cached).and_then(|f| f.set_modified(std::time::SystemTime::now()));
                    activate(&app2, g, &key);
                    return Ok(());
                }
                Err(e) => eprintln!("Réseau en cache {cached:?} inutilisable : {e}"),
            }
        }
        if cfg!(mobile) {
            return load_prebuilt(&app2, &defs, &key);
        }
        let pbfs = fetch_pbfs(&app2, &defs)?;
        if key == regions::FRANCE_KEY {
            // Construire la France demande beaucoup de mémoire : on libère d'abord le réseau actuel
            *st.router.write().unwrap() = None;
            let _ = app2.emit("network-status", status_of(&st));
        }
        build_and_activate(&app2, &pbfs, &key)
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);

    let st = app.state::<AppState>();
    *st.busy.lock().unwrap() = false;
    let status = status_of(&st);
    let _ = app.emit("network-status", status.clone());
    result.map(|_| status)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RegionsOverview {
    regions: Vec<RegionInfo>,
    total_mb: u32,
    downloaded_mb: u32,
    /// Réseau « toute la France » déjà construit (se recharge vite).
    france_ready: bool,
    /// Réseau actif = toute la France.
    france_active: bool,
}

/// État des régions : déjà téléchargées ou non, réseau France prêt ou actif.
#[tauri::command]
fn regions_overview(st: State<'_, AppState>) -> RegionsOverview {
    let regions: Vec<RegionInfo> = regions::CATALOG.iter().map(|d| region_info(&st, d)).collect();
    RegionsOverview {
        total_mb: regions.iter().map(|r| r.size_mb).sum(),
        downloaded_mb: regions.iter().filter(|r| r.downloaded).map(|r| r.size_mb).sum(),
        regions,
        france_ready: st.graphs_dir.join(format!("{}.graph", regions::FRANCE_KEY)).exists(),
        france_active: st.network_key.lock().unwrap().as_deref() == Some(regions::FRANCE_KEY),
    }
}

/// Télécharge d'un coup toutes les régions manquantes (sans construire de réseau) :
/// ensuite, chaque région se prépare sans attendre de téléchargement.
#[tauri::command]
async fn download_all_regions(app: AppHandle) -> Result<NetworkStatus, String> {
    {
        let st = app.state::<AppState>();
        let mut busy = st.busy.lock().unwrap();
        if *busy {
            return Err("Une préparation est déjà en cours, encore un instant…".into());
        }
        *busy = true;
        st.cancel_download.store(false, Ordering::Relaxed);
    }
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let all: Vec<&'static regions::RegionDef> = regions::CATALOG.iter().collect();
        fetch_pbfs(&app2, &all).map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    let st = app.state::<AppState>();
    *st.busy.lock().unwrap() = false;
    let status = status_of(&st);
    let _ = app.emit("network-status", status.clone());
    result.map(|_| status)
}

/// Arrête le téléchargement en cours (le fichier partiel est effacé, les régions finies restent).
#[tauri::command]
fn cancel_download(st: State<'_, AppState>) {
    st.cancel_download.store(true, Ordering::Relaxed);
}

// --- Mes itinéraires (dossier choisi une fois) -------------------------------------

#[tauri::command]
fn routes_folder(st: State<'_, AppState>) -> Option<String> {
    routes_dir(&st).map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
fn set_routes_folder(st: State<'_, AppState>, path: String) -> Result<(), String> {
    let dir = PathBuf::from(&path);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Dossier inutilisable : {e}"))?;
    let mut s = read_settings(&st);
    s.insert("routesDir".into(), serde_json::Value::String(path));
    write_settings(&st, &s)
}

#[tauri::command]
async fn list_saved_routes(st: State<'_, AppState>) -> Result<Vec<SavedRouteSummary>, String> {
    let Some(dir) = routes_dir(&st) else { return Ok(vec![]) };
    tauri::async_runtime::spawn_blocking(move || routes::list(&dir)).await.map_err(|e| e.to_string())?
}

/// Enregistre l'itinéraire dans le dossier. `previous` : son fichier s'il a déjà été enregistré.
#[tauri::command]
async fn save_to_library(
    st: State<'_, AppState>,
    name: String,
    coords: Vec<[f64; 2]>,
    ele: Vec<Option<f64>>,
    meta: RouteMeta,
    previous: Option<String>,
) -> Result<SavedRouteSummary, String> {
    let dir = routes_dir_or_err(&st)?;
    tauri::async_runtime::spawn_blocking(move || {
        routes::save(&dir, name.trim(), &coords, &ele, meta, previous.as_deref().map(Path::new))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn open_saved_route(st: State<'_, AppState>, file: String) -> Result<SavedRoute, String> {
    let dir = routes_dir_or_err(&st)?;
    tauri::async_runtime::spawn_blocking(move || routes::open(&dir, Path::new(&file)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn delete_saved_route(st: State<'_, AppState>, file: String) -> Result<(), String> {
    routes::delete(&routes_dir_or_err(&st)?, Path::new(&file))
}

/// Ouvre le dossier des itinéraires dans l'Explorateur.
#[tauri::command]
fn reveal_routes_folder(st: State<'_, AppState>) -> Result<(), String> {
    let dir = routes_dir_or_err(&st)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let program = "xdg-open";
    std::process::Command::new(program).arg(&dir).spawn().map(|_| ()).map_err(|e| format!("Ouverture du dossier impossible : {e}"))
}

// --- Coach IA local (phase 5) ----------------------------------------------------------

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Adresse d'Ollama et modèle choisi (réglages).
fn coach_config(st: &AppState) -> (String, String) {
    let s = read_settings(st);
    let get = |k: &str, d: &str| s.get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).unwrap_or(d).to_string();
    (get("coachUrl", coach::DEFAULT_URL).trim_end_matches('/').to_string(), get("coachModel", coach::DEFAULT_MODEL))
}

/// Inscrit une demande au coach ; renvoie son drapeau d'arrêt.
fn coach_job(st: &AppState, id: u64) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    st.coach_jobs.lock().unwrap().insert(id, flag.clone());
    flag
}

fn coach_done(st: &AppState, id: u64) {
    st.coach_jobs.lock().unwrap().remove(&id);
}

#[derive(Serialize, Clone)]
struct CoachToken {
    id: u64,
    text: String,
}

#[tauri::command]
async fn coach_status(app: AppHandle) -> Result<CoachStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (url, model) = coach_config(&app.state::<AppState>());
        coach::status(&url, &model)
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn coach_set_model(st: State<'_, AppState>, model: String) -> Result<(), String> {
    let model = model.trim();
    if model.is_empty() || model.len() > 80 || model.chars().any(|c| c.is_whitespace()) {
        return Err("Nom de modèle invalide.".into());
    }
    let mut s = read_settings(&st);
    s.insert("coachModel".into(), serde_json::Value::String(model.to_string()));
    write_settings(&st, &s)
}

/// Télécharge le modèle choisi (ou `model`) ; avancement : événement « coach-pull ».
#[tauri::command]
async fn coach_pull(app: AppHandle, id: u64, model: Option<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let (url, chosen) = coach_config(&st);
        let model = model.unwrap_or(chosen);
        let cancel = coach_job(&st, id);
        let r = coach::pull(&url, &model, &cancel, |p| {
            let _ = app.emit("coach-pull", p);
        });
        coach_done(&st, id);
        r
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn coach_stop(st: State<'_, AppState>, id: u64) {
    if let Some(f) = st.coach_jobs.lock().unwrap().get(&id) {
        f.store(true, Ordering::Relaxed);
    }
}

/// Contexte du coach (profil, forme, dernières sorties), calculé sans garder la base verrouillée ensuite.
fn coach_context(st: &AppState, athlete: &Athlete, tz_offset_min: i64) -> Result<coach::Context, String> {
    with_library(st, |l| coach::context(l, athlete, tz_offset_min * 60, now_unix()))
}

/// Discussion libre : réponse au fil de l'eau (événement « coach-token »). Le modèle peut demander
/// des actions dans l'application (`toolCalls`) : l'interface les fait, puis renvoie leurs résultats
/// dans un nouvel appel (messages « tool »). `app_state` : ce que l'application affiche.
#[tauri::command]
async fn coach_chat(
    app: AppHandle,
    id: u64,
    messages: Vec<ChatMessage>,
    athlete: Athlete,
    tz_offset_min: i64,
    app_state: Option<String>,
    use_tools: Option<bool>,
) -> Result<coach::Reply, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let (url, model) = coach_config(&st);
        let ctx = coach_context(&st, &athlete, tz_offset_min)?;
        let system = match &app_state {
            Some(a) => coach::system_prompt_with_tools(&ctx, a),
            None => coach::system_prompt(&ctx),
        };
        let mut all = vec![ChatMessage::new("system", system)];
        // Les derniers messages suffisent (la mémoire du modèle est limitée)
        let keep: Vec<ChatMessage> =
            messages.into_iter().filter(|m| matches!(m.role.as_str(), "user" | "assistant" | "tool")).collect();
        let mut skip = keep.len().saturating_sub(16);
        // Ne pas commencer par un résultat d'outil isolé de sa demande
        while skip < keep.len() && keep[skip].role != "user" {
            skip += 1;
        }
        all.extend(keep.into_iter().skip(skip));
        let cancel = coach_job(&st, id);
        // Sans outils (use_tools = false) : le modèle doit conclure par une réponse écrite
        let tools = app_state.as_ref().filter(|_| use_tools != Some(false)).map(|_| coach::tools());
        let emit = |d: &str| {
            let _ = app.emit("coach-token", CoachToken { id, text: d.to_string() });
        };
        let mut r = coach::chat(&url, &model, &all, None, tools.as_ref(), 0.5, &cancel, emit);
        if matches!(&r, Err(e) if e == coach::NO_TOOLS) {
            // Modèle sans outils : simple discussion
            eprintln!("{model} ne sait pas utiliser d'outils : réponse sans actions");
            r = coach::chat(&url, &model, &all, None, None, 0.5, &cancel, emit);
        }
        coach_done(&st, id);
        r
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Séance au format du coach (minutes, % de FTP) rendue utilisable (action « create_workout »).
#[tauri::command]
fn workout_from_coach(workout: serde_json::Value) -> Result<Workout, String> {
    match workout {
        serde_json::Value::String(text) => coach::parse_workout(&text),
        v => coach::parse_workout(&v.to_string()),
    }
}

#[tauri::command]
async fn coach_ride_note(st: State<'_, AppState>, activity_id: i64) -> Result<Option<CoachNote>, String> {
    with_library(&st, |l| l.coach_note(activity_id))
}

/// Commentaire d'une sortie par le coach (au fil de l'eau), gardé ensuite dans la base.
#[tauri::command]
async fn coach_ride_comment(
    app: AppHandle,
    id: u64,
    activity_id: i64,
    athlete: Athlete,
    tz_offset_min: i64,
) -> Result<CoachNote, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let (url, model) = coach_config(&st);
        let ctx = coach_context(&st, &athlete, tz_offset_min)?;
        let detail = with_library(&st, |l| coach::ride_detail_text(l, activity_id, &athlete, tz_offset_min * 60))?;
        let msgs = vec![
            ChatMessage::new("system", coach::system_prompt(&ctx)),
            ChatMessage::new("user", coach::ride_comment_prompt(&detail)),
        ];
        let cancel = coach_job(&st, id);
        let r = coach::chat_stream(&url, &model, &msgs, None, 0.5, &cancel, |d| {
            let _ = app.emit("coach-token", CoachToken { id, text: d.to_string() });
        });
        let stopped = cancel.load(Ordering::Relaxed);
        coach_done(&st, id);
        let text = r?;
        if stopped || text.is_empty() {
            return Err("Commentaire interrompu.".into());
        }
        with_library(&st, |l| l.save_coach_note(activity_id, &model, &text))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Séance proposée par le coach, au format imposé (schéma JSON).
#[tauri::command]
async fn coach_workout(app: AppHandle, id: u64, wish: String, athlete: Athlete, tz_offset_min: i64) -> Result<Workout, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let (url, model) = coach_config(&st);
        let ctx = coach_context(&st, &athlete, tz_offset_min)?;
        let msgs = vec![
            ChatMessage::new("system", coach::system_prompt(&ctx)),
            ChatMessage::new("user", coach::workout_prompt(&wish)),
        ];
        let cancel = coach_job(&st, id);
        let mut chars = 0usize;
        let r = coach::chat_stream(&url, &model, &msgs, Some(&coach::workout_schema()), 0.4, &cancel, |d| {
            // Pas de texte à montrer (c'est du JSON) : juste l'avancement
            chars += d.chars().count();
            let _ = app.emit("coach-token", CoachToken { id, text: chars.to_string() });
        });
        let stopped = cancel.load(Ordering::Relaxed);
        coach_done(&st, id);
        if stopped {
            return Err("Demande arrêtée.".into());
        }
        coach::parse_workout(&r?)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn workout_templates() -> Vec<Workout> {
    templates()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkoutInfo {
    total_s: u32,
    work_s: u32,
    tss: f32,
    /// FTP utilisée pour afficher les watts, et d'où elle vient.
    ftp: Option<f32>,
    ftp_source: Option<&'static str>,
    hr_max: Option<f32>,
    /// Distance conseillée pour la boucle (km).
    suggested_km: f64,
}

/// FTP (profil, sinon compteur) et FC max pour les cibles d'une séance.
fn workout_targets(st: &AppState, athlete: &Athlete) -> (Option<f32>, Option<&'static str>, Option<f32>) {
    let device = with_library(st, |l| l.latest_device_ftp()).ok().flatten();
    let (ftp, src) = match (athlete.ftp.filter(|f| *f > 0.0), device) {
        (Some(f), _) => (Some(f), Some("profil")),
        (None, Some(f)) => (Some(f as f32), Some("compteur")),
        _ => (None, None),
    };
    (ftp, src, athlete.hr_max.filter(|h| *h > 0.0))
}

fn suggested_km(w: &Workout, habits: &library::RiderHabits) -> f64 {
    let speed = habits.avg_speed_kmh.unwrap_or(25.0).clamp(15.0, 40.0);
    // En côte, les efforts se répètent sur la même montée : la boucle peut être plus courte
    let factor = if w.terrain == Terrain::Climb { 0.75 } else { 1.0 };
    let km = w.total_s() as f64 / 3600.0 * speed * factor;
    ((km / 5.0).round() * 5.0).clamp(15.0, 200.0)
}

#[tauri::command]
async fn workout_info(st: State<'_, AppState>, workout: Workout, athlete: Athlete) -> Result<WorkoutInfo, String> {
    let (ftp, ftp_source, hr_max) = workout_targets(&st, &athlete);
    let habits = with_library(&st, |l| l.habits())?;
    Ok(WorkoutInfo {
        total_s: workout.total_s(),
        work_s: workout.work_s(),
        tss: workout.tss(),
        ftp,
        ftp_source,
        hr_max,
        suggested_km: suggested_km(&workout, &habits),
    })
}

/// Enregistre la séance en fichier FIT « Workout » pour le Garmin.
#[tauri::command]
async fn export_workout(st: State<'_, AppState>, path: String, workout: Workout, athlete: Athlete) -> Result<(), String> {
    let workout = workout.normalized()?;
    let (ftp, _, hr_max) = workout_targets(&st, &athlete);
    let bytes = goride_core::workout::to_fit_workout(&workout, ftp, hr_max, now_unix() as u64);
    std::fs::write(&path, bytes).map_err(|e| format!("Écriture impossible : {e}"))
}

#[derive(Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SavedWorkout {
    id: i64,
    created_at: i64,
    workout: Workout,
}

fn read_workouts(st: &AppState) -> Vec<SavedWorkout> {
    std::fs::read_to_string(&st.workouts_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write_workouts(st: &AppState, list: &[SavedWorkout]) -> Result<(), String> {
    if let Some(dir) = st.workouts_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    std::fs::write(&st.workouts_path, text).map_err(|e| format!("Séance non enregistrée : {e}"))
}

#[tauri::command]
fn list_workouts(st: State<'_, AppState>) -> Vec<SavedWorkout> {
    let mut l = read_workouts(&st);
    l.sort_by_key(|w| -w.created_at);
    l
}

#[tauri::command]
fn keep_workout(st: State<'_, AppState>, workout: Workout) -> Result<SavedWorkout, String> {
    let workout = workout.normalized()?;
    let mut list = read_workouts(&st);
    let now = now_unix();
    let id = list.iter().map(|w| w.id).max().unwrap_or(0) + 1;
    let saved = SavedWorkout { id, created_at: now, workout };
    list.push(saved.clone());
    write_workouts(&st, &list)?;
    Ok(saved)
}

#[tauri::command]
fn delete_workout(st: State<'_, AppState>, id: i64) -> Result<(), String> {
    let mut list = read_workouts(&st);
    list.retain(|w| w.id != id);
    write_workouts(&st, &list)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkoutLoop {
    #[serde(flatten)]
    proposal: LoopProposal,
    key: Option<KeySection>,
}

/// Boucles adaptées à la séance : une montée assez longue pour les efforts en côte, une
/// longue portion plate pour le seuil ou le tempo. Les meilleures d'abord.
#[tauri::command]
async fn workout_loops(
    st: State<'_, AppState>,
    request: LoopRequest,
    options: RouteOptions,
    workout: Workout,
    exclude: Vec<u64>,
) -> Result<Vec<WorkoutLoop>, String> {
    let router = router_of(&st)?;
    ensure_in_region(&st, &router, request.start)?;
    let habits = with_library(&st, |l| l.habits())?;
    let need = Need {
        terrain: workout.terrain,
        effort_s: workout.longest_effort_s() as f64,
        vam_m_h: habits.vam_m_h.unwrap_or(750.0),
        flat_speed_kmh: habits.flat_speed_kmh.unwrap_or(28.0),
    };
    if need.terrain == Terrain::Climb && !router.graph.has_elevation {
        return Err("Le réseau chargé ne connaît pas le relief : impossible de chercher une montée.".into());
    }
    let mut opts = options;
    match need.terrain {
        Terrain::Climb => opts.hills = opts.hills.max(0.7),
        Terrain::Flat => opts.hills = opts.hills.min(-0.6),
        Terrain::Any => {}
    }
    let excluded: Vec<Vec<(u32, f32)>> = {
        let cache = st.loop_footprints.lock().unwrap();
        exclude.iter().filter_map(|id| cache.get(id).cloned()).collect()
    };
    let req = LoopRequest { count: 8, tolerance: request.tolerance.max(0.08), ..request };
    let t = Instant::now();
    let ranked = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<(LoopProposal, Option<KeySection>)>, String> {
        let loops = propose_loops(&router, &req, &opts, &excluded)?;
        Ok(rank_for_workout(loops, &router.graph, &need, 4))
    })
    .await
    .map_err(|e| e.to_string())??;
    eprintln!("{} boucles de séance en {} ms", ranked.len(), t.elapsed().as_millis());
    let mut cache = st.loop_footprints.lock().unwrap();
    for (l, _) in &ranked {
        cache.insert(l.id, l.footprint.clone());
    }
    Ok(ranked.into_iter().map(|(proposal, key)| WorkoutLoop { proposal, key }).collect())
}

/// Ouvre la page de téléchargement d'Ollama dans le navigateur.
#[tauri::command]
fn open_ollama_site() -> Result<(), String> {
    const URL: &str = "https://ollama.com/download";
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", URL]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(URL).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(URL).spawn();
    r.map(|_| ()).map_err(|e| format!("Ouverture du navigateur impossible : {e}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn network_names_are_clean() {
        assert_eq!(super::network_display("france-261108-internal"), "France");
        assert_eq!(super::network_display("ile-de-france-latest"), "Ile-de-france");
        assert_eq!(super::network_display("mon_extrait"), "Mon-extrait");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            let cache = app.path().app_cache_dir()?;
            let graphs_dir = data.join("graphs");
            app.manage(AppState {
                router: RwLock::new(None),
                region: Mutex::new(None),
                busy: Mutex::new(false),
                notice: Mutex::new(None),
                loop_footprints: Mutex::new(HashMap::new()),
                library: Mutex::new(match Library::open(&data.join("sorties.db")) {
                    Ok(l) => Some(l),
                    Err(e) => {
                        eprintln!("{e}");
                        None
                    }
                }),
                dem: Arc::new(Dem::new(Some(cache.join("dem")))),
                graphs_dir: graphs_dir.clone(),
                network_key: Mutex::new(None),
                regions: Regions::new(data.join("osm")),
                cancel_download: AtomicBool::new(false),
                settings_path: data.join("settings.json"),
                coach_jobs: Mutex::new(HashMap::new()),
                workouts_path: data.join("seances.json"),
            });

            // Au démarrage, on recharge en arrière-plan le dernier réseau construit.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let latest = std::fs::read_dir(&graphs_dir).ok().and_then(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().is_some_and(|x| x == "graph"))
                        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
                        .map(|e| e.path())
                });
                let Some(path) = latest else { return };
                *handle.state::<AppState>().busy.lock().unwrap() = true;
                let _ = handle.emit("network-progress", "Chargement du réseau routier…".to_string());
                let t = Instant::now();
                match Graph::load(&path) {
                    Ok(g) => {
                        eprintln!("Réseau chargé en {:.1} s", t.elapsed().as_secs_f32());
                        activate(&handle, g, &graph_key(&path));
                    }
                    Err(e) => {
                        eprintln!("Réseau {path:?} inutilisable : {e}");
                        // Format ancien : on reconstruit à partir du même .osm.pbf s'il est connu
                        let sources: Vec<PathBuf> = std::fs::read_to_string(path.with_extension("source"))
                            .map(|t| t.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect())
                            .unwrap_or_default();
                        match (!sources.is_empty() && sources.iter().all(|p| p.exists())).then_some(sources) {
                            Some(pbfs) => {
                                let _ = handle.emit("network-progress", "Mise à jour du réseau…".to_string());
                                if let Err(e) = build_and_activate(&handle, &pbfs, &graph_key(&path)) {
                                    eprintln!("Reconstruction impossible : {e}");
                                }
                            }
                            None if cfg!(mobile) => {
                                // Téléphone : réseau d'une ancienne version, on le retélécharge à la demande
                                let _ = std::fs::remove_file(&path);
                                *handle.state::<AppState>().notice.lock().unwrap() =
                                    Some("Le réseau routier a été mis à jour : touche la carte pour le télécharger à nouveau.".into());
                            }
                            None => {
                                *handle.state::<AppState>().notice.lock().unwrap() = Some(format!(
                                    "Le réseau {} doit être reconstruit pour cette version : clique ici et choisis à nouveau le fichier .osm.pbf.",
                                    network_display(&graph_key(&path))
                                ));
                            }
                        }
                    }
                }
                let st = handle.state::<AppState>();
                *st.busy.lock().unwrap() = false;
                let _ = handle.emit("network-status", status_of(&st));
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_track,
            network_status,
            build_network,
            snap_point,
            route_leg,
            summarize_route,
            loop_proposals,
            import_activities,
            ride_progress,
            list_activities,
            activity_detail,
            rename_activity,
            delete_activity,
            save_route,
            elevation_at,
            coverage_for,
            load_regions,
            regions_overview,
            download_all_regions,
            cancel_download,
            routes_folder,
            set_routes_folder,
            list_saved_routes,
            save_to_library,
            open_saved_route,
            delete_saved_route,
            reveal_routes_folder,
            coach_status,
            coach_set_model,
            coach_pull,
            coach_stop,
            coach_chat,
            workout_from_coach,
            coach_ride_note,
            coach_ride_comment,
            coach_workout,
            workout_templates,
            workout_info,
            export_workout,
            list_workouts,
            keep_workout,
            delete_workout,
            workout_loops,
            open_ollama_site
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de GoRide");
}

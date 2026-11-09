//! Coach IA local (F7) : un modèle de langage qui tourne sur le PC via Ollama
//! (<https://ollama.com>), interrogé en HTTP sur 127.0.0.1. Rien ne part en ligne.
//!
//! - `context` : ce que le coach sait de la cycliste (profil, forme, dernières sorties),
//!   en texte, recalculé à chaque question à partir de la base des sorties ;
//! - `chat_stream` : réponse au fil de l'eau (événements vers l'interface) ;
//! - `workout_request` : séance structurée, au format imposé par un schéma JSON.

use crate::library::Library;
use goride_core::activity::analysis::{analyze, Analysis, Athlete};
use goride_core::workout::{fmt_duration, templates, Block, Step, StepKind, Terrain, Workout};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const DEFAULT_URL: &str = "http://127.0.0.1:11434";
/// Modèle conseillé : ~5 Go, tient dans 8 Go de mémoire vidéo, bon en français.
pub const DEFAULT_MODEL: &str = "qwen3:8b";

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        // Premier mot : le modèle doit d'abord être chargé en mémoire vidéo (jusqu'à ~30 s)
        .timeout_read(Duration::from_secs(240))
        .build()
}

fn unreachable(e: impl std::fmt::Display) -> String {
    eprintln!("Ollama : {e}");
    "Ollama ne répond pas : vérifie qu'il est installé et lancé (icône du lama près de l'horloge).".to_string()
}

/// Message d'erreur d'Ollama (« model "x" not found… ») traduit au mieux.
fn ollama_error(code: u16, resp: ureq::Response) -> String {
    let body = resp.into_string().unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or(body);
    if msg.contains("does not support tools") {
        return NO_TOOLS.into();
    }
    if msg.contains("not found") {
        return "Ce modèle n'est pas encore installé : télécharge-le depuis le panneau Coach.".into();
    }
    if msg.contains("memory") {
        return format!("Pas assez de mémoire pour ce modèle ({msg}). Choisis un modèle plus petit.");
    }
    format!("Ollama a répondu une erreur {code} : {msg}")
}

fn post(url: &str, body: &Value) -> Result<ureq::Response, String> {
    match agent().post(url).set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(r) => Ok(r),
        Err(ureq::Error::Status(code, resp)) => Err(ollama_error(code, resp)),
        Err(e) => Err(unreachable(e)),
    }
}

// --- État et modèles ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub name: String,
    pub size_gb: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoachStatus {
    /// Ollama répond.
    pub running: bool,
    pub version: Option<String>,
    pub models: Vec<ModelInfo>,
    /// Modèle choisi (réglages), installé ou non.
    pub model: String,
    pub installed: bool,
    pub recommended: &'static str,
}

/// Le nom « qwen3:8b » désigne aussi « qwen3:8b-q4_K_M » ; « qwen3 » désigne « qwen3:latest ».
fn same_model(a: &str, b: &str) -> bool {
    let norm = |s: &str| if s.contains(':') { s.to_string() } else { format!("{s}:latest") };
    norm(a) == norm(b)
}

pub fn status(base: &str, model: &str) -> CoachStatus {
    let a = agent();
    let version = a
        .get(&format!("{base}/api/version"))
        .timeout(Duration::from_secs(3))
        .call()
        .ok()
        .and_then(|r| serde_json::from_reader::<_, Value>(r.into_reader()).ok())
        .and_then(|v| v.get("version").and_then(|x| x.as_str()).map(str::to_string));
    let models: Vec<ModelInfo> = if version.is_some() {
        a.get(&format!("{base}/api/tags"))
            .timeout(Duration::from_secs(5))
            .call()
            .ok()
            .and_then(|r| serde_json::from_reader::<_, Value>(r.into_reader()).ok())
            .and_then(|v| v.get("models").and_then(|m| m.as_array()).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|m| {
                let name = m.get("name").or_else(|| m.get("model"))?.as_str()?.to_string();
                // Les modèles d'« embedding » ne savent pas discuter
                if name.contains("embed") {
                    return None;
                }
                let size = m.get("size").and_then(|s| s.as_f64()).unwrap_or(0.0);
                Some(ModelInfo { name, size_gb: (size / 1e8).round() / 10.0 })
            })
            .collect()
    } else {
        vec![]
    };
    CoachStatus {
        running: version.is_some(),
        installed: models.iter().any(|m| same_model(&m.name, model)),
        version,
        models,
        model: model.to_string(),
        recommended: DEFAULT_MODEL,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullProgress {
    pub status: String,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

/// Télécharge un modèle (plusieurs Go) ; `on` reçoit l'avancement.
pub fn pull(base: &str, model: &str, cancel: &AtomicBool, mut on: impl FnMut(PullProgress)) -> Result<(), String> {
    let resp = post(&format!("{base}/api/pull"), &json!({ "model": model, "stream": true }))?;
    for line in BufReader::new(resp.into_reader()).lines() {
        if cancel.load(Ordering::Relaxed) {
            return Err("Téléchargement arrêté.".into());
        }
        let line = line.map_err(unreachable)?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            return Err(if err.contains("file does not exist") || err.contains("not found") {
                format!("Le modèle « {model} » n'existe pas dans la bibliothèque Ollama.")
            } else {
                format!("Téléchargement impossible : {err}")
            });
        }
        let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let done = status == "success";
        on(PullProgress {
            status,
            completed: v.get("completed").and_then(|c| c.as_u64()),
            total: v.get("total").and_then(|c| c.as_u64()),
        });
        if done {
            return Ok(());
        }
    }
    Ok(())
}

// --- Discussion -----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    /// Actions demandées par le modèle (message « assistant »).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Outil dont ce message donne le résultat (message « tool »).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        ChatMessage { role: role.into(), content: content.into(), tool_calls: None, tool_name: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// Réponse du modèle : texte, et éventuellement des actions à faire dans l'application.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

/// Erreur renvoyée quand le modèle ne sait pas utiliser d'outils (on réessaie sans).
pub const NO_TOOLS: &str = "NO_TOOLS";

/// Actions écrites en texte par le modèle (« <tool_call>{…}</tool_call> ») quand Ollama ne les a pas reconnues.
fn tool_calls_in_text(text: &str) -> (String, Vec<ToolCall>) {
    let mut calls = Vec::new();
    let mut rest = String::new();
    let mut s = text;
    while let Some(i) = s.find("<tool_call>") {
        rest.push_str(&s[..i]);
        let after = &s[i + "<tool_call>".len()..];
        let end = after.find("</tool_call>").unwrap_or(after.len());
        if let Ok(v) = serde_json::from_str::<Value>(after[..end].trim()) {
            if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                calls.push(ToolCall {
                    function: ToolFunction { name: name.into(), arguments: v.get("arguments").cloned().unwrap_or(json!({})) },
                });
            }
        }
        s = after.get(end + "</tool_call>".len()..).unwrap_or("");
    }
    rest.push_str(s);
    (rest, calls)
}

/// Retire les passages de réflexion (« <think>…</think> ») au fil du flux.
#[derive(Default)]
struct ThinkFilter {
    inside: bool,
    pending: String,
}

impl ThinkFilter {
    fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let mut out = String::new();
        loop {
            if self.inside {
                match self.pending.find("</think>") {
                    Some(i) => {
                        self.pending.drain(..i + "</think>".len());
                        self.inside = false;
                    }
                    None => {
                        // On garde la fin au cas où la balise serait coupée en deux
                        let keep = self.pending.len().min(7);
                        let cut = self.pending.len() - keep;
                        let cut = (0..=cut).rev().find(|&c| self.pending.is_char_boundary(c)).unwrap_or(0);
                        self.pending.drain(..cut);
                        return out;
                    }
                }
            } else {
                match self.pending.find("<think>") {
                    Some(i) => {
                        out.push_str(&self.pending[..i]);
                        self.pending.drain(..i + "<think>".len());
                        self.inside = true;
                    }
                    None => {
                        // Une balise peut commencer à la fin du morceau : on attend la suite
                        let tail = (1..="<think>".len().min(self.pending.len()))
                            .rev()
                            .find(|&k| {
                                let start = self.pending.len() - k;
                                self.pending.is_char_boundary(start) && "<think>".starts_with(&self.pending[start..])
                            })
                            .unwrap_or(0);
                        let cut = self.pending.len() - tail;
                        out.push_str(&self.pending[..cut]);
                        self.pending.drain(..cut);
                        return out;
                    }
                }
            }
        }
    }
}

/// Modèles qui « réfléchissent » avant de répondre : on coupe la réflexion (plus rapide).
fn can_think(model: &str) -> bool {
    let m = model.to_lowercase();
    ["qwen3", "deepseek-r1", "magistral"].iter().any(|k| m.starts_with(k))
}

/// Interroge le modèle. `on_delta` reçoit le texte au fur et à mesure ; renvoie le texte complet.
/// `format` : schéma JSON imposé à la réponse (séances).
pub fn chat_stream(
    base: &str,
    model: &str,
    messages: &[ChatMessage],
    format: Option<&Value>,
    temperature: f32,
    cancel: &AtomicBool,
    on_delta: impl FnMut(&str),
) -> Result<String, String> {
    chat(base, model, messages, format, None, temperature, cancel, on_delta).map(|r| r.text)
}

/// Comme `chat_stream`, avec des outils (actions dans l'application) que le modèle peut appeler.
#[allow(clippy::too_many_arguments)]
pub fn chat(
    base: &str,
    model: &str,
    messages: &[ChatMessage],
    format: Option<&Value>,
    tools: Option<&Value>,
    temperature: f32,
    cancel: &AtomicBool,
    mut on_delta: impl FnMut(&str),
) -> Result<Reply, String> {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "keep_alive": "15m",
        "options": { "temperature": temperature, "num_ctx": 8192 },
    });
    if can_think(model) {
        body["think"] = json!(false);
    }
    if let Some(f) = format {
        body["format"] = f.clone();
    }
    if let Some(t) = tools {
        body["tools"] = t.clone();
    }
    let resp = post(&format!("{base}/api/chat"), &body)?;
    let mut filter = ThinkFilter::default();
    let mut full = String::new();
    let mut calls: Vec<ToolCall> = Vec::new();
    for line in BufReader::new(resp.into_reader()).lines() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let line = line.map_err(unreachable)?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).map_err(|e| format!("Réponse d'Ollama illisible : {e}"))?;
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            return Err(format!("Le modèle s'est interrompu : {err}"));
        }
        if let Some(c) = v.pointer("/message/content").and_then(|c| c.as_str()) {
            let shown = filter.push(c);
            if !shown.is_empty() {
                full.push_str(&shown);
                on_delta(&shown);
            }
        }
        if let Some(tc) = v.pointer("/message/tool_calls") {
            if let Ok(mut more) = serde_json::from_value::<Vec<ToolCall>>(tc.clone()) {
                calls.append(&mut more);
            }
        }
        if v.get("done").and_then(|d| d.as_bool()) == Some(true) {
            break;
        }
    }
    if calls.is_empty() && tools.is_some() && full.contains("<tool_call>") {
        let (text, found) = tool_calls_in_text(&full);
        full = text;
        calls = found;
    }
    Ok(Reply { text: full.trim().to_string(), tool_calls: calls })
}

// --- Actions dans l'application (outils) ----------------------------------------------

fn profile_param() -> Value {
    json!({ "type": "string", "enum": ["road", "triathlon", "gravel"], "description": "road = vélo de route (goudron seulement) ; triathlon = contre-la-montre (CLM), triathlon, position aéro : goudron lisse, routes roulantes, peu de carrefours ; gravel = chemins acceptés, SEULEMENT si elle parle de gravel ou de chemins" })
}

fn hills_param() -> Value {
    json!({ "type": "string", "enum": ["flat", "neutral", "hilly"], "description": "flat = le plus plat possible, hilly = chercher les bosses" })
}

/// Outils que le coach peut appeler. Ils sont exécutés par l'interface de GoRide.
pub fn tools() -> Value {
    let template_names: Vec<String> = templates().iter().map(|w| w.name.clone()).collect();
    let f = |name: &str, description: &str, parameters: Value| {
        json!({ "type": "function", "function": { "name": name, "description": description, "parameters": parameters } })
    };
    json!([
        f(
            "propose_loops",
            "Calcule et affiche sur la carte plusieurs boucles qui reviennent au point de départ, d'une distance donnée. À utiliser dès qu'elle donne une distance sans destination différente du départ : « trace-moi un itinéraire de 20 km », « une boucle de 50 km depuis chez moi », « départ et retour au même endroit ».",
            json!({
                "type": "object",
                "properties": {
                    "distance_km": { "type": "number", "description": "Distance de la boucle en km" },
                    "start": { "type": "string", "description": "Lieu ou adresse complète de départ en France, recopiée telle quelle (ex. « 1 place de la Mairie 35000 Rennes », « Bédoin »). Chaîne vide si non précisé ou « n'importe où »." },
                    "profile": profile_param(),
                    "hills": hills_param(),
                    "direction": { "type": "string", "enum": ["any", "north", "northeast", "east", "southeast", "south", "southwest", "west", "northwest"] }
                },
                "required": ["distance_km", "start", "profile"]
            }),
        ),
        f(
            "plan_route",
            "Trace un itinéraire d'un lieu A vers un lieu B DIFFÉRENT (en passant éventuellement par des étapes). Pour revenir au point de départ, n'utilise pas cet outil : utilise propose_loops.",
            json!({
                "type": "object",
                "properties": {
                    "start": { "type": "string", "description": "Lieu de départ en France" },
                    "end": { "type": "string", "description": "Lieu d'arrivée en France" },
                    "via": { "type": "array", "items": { "type": "string" }, "description": "Étapes intermédiaires, dans l'ordre" },
                    "profile": profile_param(),
                    "hills": hills_param()
                },
                "required": ["start", "end", "profile"]
            }),
        ),
        f(
            "set_route_options",
            "Change les réglages du tracé (s'appliquent à l'itinéraire en cours et aux prochaines boucles).",
            json!({
                "type": "object",
                "properties": {
                    "profile": profile_param(),
                    "hills": hills_param(),
                    "avoid_main_roads": { "type": "boolean", "description": "true = fuir les grands axes" },
                    "cycleways": { "type": "string", "enum": ["avoid", "neutral", "prefer"] },
                    "max_grade_pct": { "type": "number", "description": "Pente maximale en %, 0 = pas de limite" }
                }
            }),
        ),
        f(
            "create_workout",
            "Crée une séance d'entraînement structurée et l'affiche dans GoRide (exportable vers le Garmin). Puissances en % de la FTP.",
            workout_schema(),
        ),
        f(
            "show_workout_template",
            "Affiche une des séances types de GoRide.",
            json!({ "type": "object", "properties": { "name": { "type": "string", "enum": template_names } }, "required": ["name"] }),
        ),
        f(
            "route_for_workout",
            "Cherche des boucles adaptées à la séance affichée (montée assez longue pour les efforts en côte, portion plate pour le seuil).",
            json!({ "type": "object", "properties": { "start": { "type": "string", "description": "Lieu de départ ; vide = centre de la carte" } } }),
        ),
        f(
            "open_ride",
            "Ouvre une sortie importée (carte, courbes, analyse).",
            json!({ "type": "object", "properties": { "date": { "type": "string", "description": "Date AAAA-MM-JJ de la sortie, ou « latest » pour la plus récente" } }, "required": ["date"] }),
        ),
        f("show_progress", "Ouvre la vue Progression (forme, fatigue, vitesse sur le plat, kilomètres par semaine).", json!({ "type": "object", "properties": {} })),
        f(
            "show_place",
            "Centre la carte sur un lieu et y place un repère.",
            json!({ "type": "object", "properties": { "query": { "type": "string", "description": "Lieu ou adresse en France" } }, "required": ["query"] }),
        )
    ])
}

// --- Contexte de la cycliste ----------------------------------------------------------

const DAY: i64 = 86_400;
const MONTHS: [&str; 12] = ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc."];
const WEEKDAYS: [&str; 7] = ["jeudi", "vendredi", "samedi", "dimanche", "lundi", "mardi", "mercredi"];

/// (année, mois 1–12, jour) d'un nombre de jours depuis le 1er janvier 1970.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// « lundi 9 nov. 2026 » (heure locale).
pub fn date_fr(t: i64, tz_s: i64, weekday: bool) -> String {
    let days = (t + tz_s).div_euclid(DAY);
    let (y, m, d) = civil(days);
    let wd = WEEKDAYS[days.rem_euclid(7) as usize];
    if weekday {
        format!("{wd} {d} {} {y}", MONTHS[m as usize - 1])
    } else {
        format!("{d} {} {y}", MONTHS[m as usize - 1])
    }
}

fn fr(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$}").replace('.', ",")
}

fn hours(s: f64) -> String {
    let m = (s / 60.0).round() as i64;
    format!("{} h {:02}", m / 60, m % 60)
}

/// Ce que le coach sait de la cycliste.
pub struct Context {
    pub text: String,
}

fn ride_line(lib: &Library, id: i64, a: &Analysis, tz_s: i64) -> String {
    let (name, start) = lib
        .summary(id)
        .map(|s| (s.name, s.start_time.unwrap_or(0)))
        .unwrap_or_else(|_| ("Sortie".into(), 0));
    let mut parts = vec![
        format!("{} km", fr(a.distance_m / 1000.0, 1)),
        hours(a.moving_s as f64),
        format!("D+ {} m", a.ascent_m.round()),
        format!("{} km/h de moyenne", fr(a.avg_speed_kmh as f64, 1)),
    ];
    if let Some(v) = a.flat_speed_kmh {
        parts.push(format!("{} km/h sur le plat", fr(v as f64, 1)));
    }
    if let Some(p) = &a.power {
        parts.push(format!("puissance moyenne {:.0} W, NP {:.0} W", p.avg, p.np));
        if let Some(i) = p.intensity {
            parts.push(format!("IF {}", fr(i as f64, 2)));
        }
        if let Some(t) = p.tss {
            parts.push(format!("TSS {t:.0}"));
        }
    }
    if let Some(h) = a.avg_hr {
        parts.push(format!("FC moyenne {h:.0} bpm"));
    }
    if let Some(c) = a.avg_cadence {
        parts.push(format!("cadence {c:.0} tr/min"));
    }
    format!("- {} « {} » : {}", date_fr(start, tz_s, false), name, parts.join(", "))
}

/// Construit le contexte. `now` en secondes Unix, `tz_s` décalage horaire local.
pub fn context(lib: &Library, athlete: &Athlete, tz_s: i64, now: i64) -> Result<Context, String> {
    let ids = lib.recent_cycling_ids(8)?;
    let analyses: Vec<(i64, Analysis)> =
        ids.iter().filter_map(|&id| lib.load(id).ok().map(|a| (id, analyze(&a, athlete)))).collect();
    let habits = lib.habits()?;

    // FTP : celle du profil, sinon celle du compteur, sinon l'estimation d'après les sorties
    let device_ftp = analyses.iter().find_map(|(_, a)| a.ftp_used.filter(|_| a.ftp_source.as_deref() == Some("compteur")));
    let estimated = analyses.iter().filter_map(|(_, a)| a.estimated_ftp).fold(None, |m: Option<f32>, x| Some(m.map_or(x, |m| m.max(x))));
    let (ftp, ftp_txt) = match (athlete.ftp, device_ftp, estimated) {
        (Some(f), _, _) => (Some(f), format!("FTP {f:.0} W (renseignée dans l'application)")),
        (None, Some(f), _) => (Some(f), format!("FTP {f:.0} W (réglée sur son compteur Garmin, à vérifier)")),
        (None, None, Some(f)) => (Some(f), format!("FTP inconnue, estimée à environ {f:.0} W d'après ses meilleurs efforts")),
        _ => (None, "FTP inconnue".to_string()),
    };
    let max_hr_seen = analyses.iter().filter_map(|(_, a)| a.max_hr).fold(None, |m: Option<f32>, x| Some(m.map_or(x, |m| m.max(x))));
    let hr_txt = match (athlete.hr_max, max_hr_seen) {
        (Some(h), _) => format!("FC max {h:.0} bpm"),
        (None, Some(h)) => format!("FC max inconnue (la plus haute relevée sur les dernières sorties : {h:.0} bpm)"),
        _ => "FC max inconnue".to_string(),
    };

    let mut t = String::new();
    t += &format!("Date du jour : {}.\n", date_fr(now, tz_s, true));
    t += &format!("Profil : {ftp_txt} ; {hr_txt}");
    if let Some(w) = athlete.weight {
        t += &format!(" ; poids {} kg", fr(w as f64, 1));
        if let Some(f) = ftp {
            t += &format!(" ({} W/kg)", fr((f / w) as f64, 2));
        }
    }
    t += ".\nCapteurs : cardio, puissance et cadence. Compteur Garmin Edge 540. Vélo de route et triathlon. Objectif : aller plus vite.\n";

    if analyses.is_empty() {
        t += "Aucune sortie importée pour l'instant : le coach ne connaît pas encore son niveau.\n";
        return Ok(Context { text: t });
    }

    let p = lib.progress(athlete, tz_s)?;
    if let Some(d) = p.days.last() {
        t += &format!(
            "Charge d'entraînement aujourd'hui : forme (CTL, 42 j) {:.0}, fatigue (ATL, 7 j) {:.0}, fraîcheur (TSB) {:.0}.\n",
            d.fitness, d.fatigue, d.form
        );
        if p.estimated_share > 0.3 {
            t += "Attention : une partie des charges est estimée (sorties sans puissance ni cardio).\n";
        }
    }
    let weeks: Vec<String> = p
        .weeks
        .iter()
        .rev()
        .take(6)
        .map(|w| {
            format!(
                "- semaine du {}{} : {} sortie(s), {} km, {}, D+ {:.0} m, TSS {:.0}",
                date_fr(w.start, tz_s, false),
                if w.start + 7 * DAY > now { " (en cours)" } else { "" },
                w.rides,
                w.distance_km.round(),
                hours(w.moving_h * 3600.0),
                w.ascent_m,
                w.tss
            )
        })
        .collect();
    t += &format!("Dernières semaines (de la plus récente) :\n{}\n", weeks.join("\n"));
    if let Some(tr) = p.flat_speed_trend_per_month {
        t += &format!("Tendance de la vitesse sur le plat : {} km/h par mois.\n", fr(tr, 2));
    }
    if let Some(v) = habits.vam_m_h {
        t += &format!("VAM habituelle en montée : {v:.0} m/h.\n");
    }

    // Meilleurs efforts sur les dernières sorties
    let mut best = std::collections::BTreeMap::<u32, f32>::new();
    for (_, a) in &analyses {
        for &(d, w) in &a.best_power {
            if let Some(w) = w {
                let e = best.entry(d).or_insert(0.0);
                *e = e.max(w);
            }
        }
    }
    let shown: Vec<String> = [60u32, 300, 1200, 3600]
        .iter()
        .filter_map(|d| best.get(d).map(|w| format!("{} {w:.0} W", fmt_duration(*d))))
        .collect();
    if !shown.is_empty() {
        t += &format!("Meilleurs efforts sur ses {} dernières sorties : {}.\n", analyses.len(), shown.join(", "));
    }

    // IF > 1 sur une longue sortie : FTP trop basse, ou NP gonflée par un effort très irrégulier
    if let Some((_, a)) = analyses.iter().find(|(_, a)| {
        a.moving_s >= 2700.0 && a.power.as_ref().and_then(|p| p.intensity).is_some_and(|i| i > 1.05)
    }) {
        let p = a.power.as_ref().unwrap();
        t += &format!(
            "Attention : IF de {} sur une sortie de plus de 45 min, ce qui est physiologiquement improbable. Soit la FTP utilisée est trop basse, soit l'effort très irrégulier (VI {}) gonfle la NP. Le meilleur effort de 20 min donne une meilleure idée du seuil (≈ 95 % de cette valeur).\n",
            fr(p.intensity.unwrap_or(0.0) as f64, 2),
            fr(p.vi as f64, 2)
        );
    }

    t += "Dernières sorties (de la plus récente) :\n";
    for (id, a) in &analyses {
        t += &ride_line(lib, *id, a, tz_s);
        t.push('\n');
    }
    Ok(Context { text: t })
}

/// Détail d'une sortie pour son commentaire.
pub fn ride_detail_text(lib: &Library, id: i64, athlete: &Athlete, tz_s: i64) -> Result<String, String> {
    let d = lib.detail(id, athlete)?;
    let a = &d.analysis;
    let mut t = ride_line(lib, id, a, tz_s);
    t += "\n";
    if a.elapsed_s - a.moving_s > 300.0 {
        t += &format!("Temps total (arrêts compris) : {}.\n", hours(a.elapsed_s as f64));
    }
    if let Some(c) = a.flat_coasting_pct {
        t += &format!("Part de roue libre sur le plat : {c:.0} %.\n");
    }
    if let Some(p) = &a.power {
        t += &format!("Puissance : max {:.0} W, VI {} (régularité : 1,00 = parfaitement régulier), travail {:.0} kJ", p.max, fr(p.vi as f64, 2), p.work_kj);
        if let Some(w) = p.w_per_kg {
            t += &format!(", {} W/kg", fr(w as f64, 2));
        }
        t += ".\n";
    }
    if let Some(f) = a.ftp_used {
        t += &format!("FTP utilisée pour les calculs : {f:.0} W ({}).\n", a.ftp_source.as_deref().unwrap_or("?"));
    }
    if let Some(dc) = a.decoupling {
        t += &format!("Dérive cardiaque (découplage puissance/FC entre 1re et 2e moitié) : {} %.\n", fr(dc as f64, 1));
    }
    let zones = |names: &[&str], secs: &[f32]| -> String {
        let total: f32 = secs.iter().sum::<f32>().max(1.0);
        names
            .iter()
            .zip(secs)
            .filter(|(_, s)| **s > 0.0)
            .map(|(n, s)| format!("{n} {:.0} %", s / total * 100.0))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(z) = &a.hr_zones {
        t += &format!("Temps par zone cardiaque : {}.\n", zones(&["Z1", "Z2", "Z3", "Z4", "Z5"], z));
    }
    if let Some(z) = &a.power_zones {
        t += &format!("Temps par zone de puissance : {}.\n", zones(&["Z1", "Z2", "Z3", "Z4", "Z5", "Z6", "Z7"], z));
    }
    let best: Vec<String> = a.best_power.iter().filter_map(|&(d, w)| w.map(|w| format!("{} {w:.0} W", fmt_duration(d)))).collect();
    if !best.is_empty() {
        t += &format!("Meilleurs efforts de la sortie : {}.\n", best.join(", "));
    }
    for (i, c) in a.climbs.iter().enumerate() {
        t += &format!(
            "Montée {} : km {} à {}, {} km à {} %, {:.0} m de D+ en {}, VAM {:.0} m/h",
            i + 1,
            fr(c.start_km as f64, 1),
            fr(c.end_km as f64, 1),
            fr(c.length_m as f64 / 1000.0, 1),
            fr(c.grade as f64, 1),
            c.gain_m,
            fmt_duration(c.duration_s as u32),
            c.vam
        );
        if let Some(p) = c.avg_power {
            t += &format!(", {p:.0} W");
        }
        if let Some(h) = c.avg_hr {
            t += &format!(", {h:.0} bpm");
        }
        if let Some(cad) = c.avg_cadence {
            t += &format!(", cadence {cad:.0}");
        }
        if let Some(h) = d.climb_history.get(i).filter(|h| h.efforts.len() > 1) {
            t += &format!(" — {}e sur {} passages de cette montée", h.rank, h.efforts.len());
        }
        t += ".\n";
    }
    if !a.advice.is_empty() {
        t += "Constats de l'analyse automatique :\n";
        for x in &a.advice {
            t += &format!("- {} : {}\n", x.title, x.text);
        }
    }
    Ok(t)
}

// --- Consignes ------------------------------------------------------------------------

/// Consignes pour la discussion avec actions : `app` décrit ce que l'application affiche.
pub fn system_prompt_with_tools(ctx: &Context, app: &str) -> String {
    format!(
        "{}\n\nTu peux agir directement dans GoRide grâce à des outils (tracer des boucles ou un itinéraire sur la carte, régler le tracé, créer ou afficher une séance, chercher un itinéraire pour une séance, ouvrir une sortie ou la progression, montrer un lieu). \
Quand elle demande une de ces choses, appelle l'outil au lieu d'expliquer comment faire. Ne dis jamais que tu n'as pas accès aux cartes ou à l'application. \
Si le départ n'est pas précisé ou si elle dit « n'importe où », laisse « start » vide : le départ sera là où la carte est centrée. \
Un itinéraire qui revient au point de départ (« retour au même endroit », « boucle », une distance sans destination) se fait avec propose_loops, jamais avec plan_route. \
Choisis le profil d'après la demande : contre-la-montre (CLM), triathlon ou position aéro → triathlon, et de préférence « hills » = flat ; gravel ou chemins → gravel ; sinon road. Ne garde pas le profil gravel d'une demande précédente. \
Une seule action suffit le plus souvent ; ne relance pas une action qui vient d'échouer avec les mêmes paramètres. \
Après une action, base-toi uniquement sur le résultat de l'outil et dis en une ou deux phrases ce qui est affiché et ce qu'elle peut faire ensuite (par exemple cliquer sur « Choisir » pour garder une boucle). \
Si l'outil signale un échec, explique-le simplement.\n\nÉtat de l'application :\n{}",
        system_prompt(ctx),
        app.trim()
    )
}

pub fn system_prompt(ctx: &Context) -> String {
    let names: Vec<String> = templates().iter().map(|w| w.name.clone()).collect();
    format!(
        "Tu es le coach de vélo de route et de triathlon intégré à GoRide, une application qui tourne entièrement sur le PC de la cycliste. \
Tu t'adresses à elle en français, en la tutoyant, avec un ton direct, chaleureux et précis. Son objectif principal : aller plus vite.\n\
Règles :\n\
- Appuie-toi uniquement sur les chiffres fournis ci-dessous ; n'invente aucune donnée. Si une information manque, dis-le et explique comment l'obtenir (par exemple un test FTP de 20 minutes).\n\
- Sois concret : zones, durées, watts ou % de FTP, cadences, nombre de séances par semaine.\n\
- Tiens compte de la fraîcheur (TSB) : très négative (sous −20), propose du repos ou de la récupération.\n\
- Tu n'es pas médecin : en cas de douleur, de malaise ou de symptôme, conseille de consulter.\n\
- Réponses courtes (moins de 200 mots sauf demande contraire), en Markdown simple : paragraphes, listes à puces, **gras**. Pas de tableaux ni de titres.\n\
- Séances types disponibles dans GoRide : {}. Une séance peut être exportée vers son Garmin.\n\n\
Ce que tu sais d'elle :\n{}",
        names.join(", "),
        ctx.text
    )
}

pub fn ride_comment_prompt(detail: &str) -> String {
    format!(
        "Commente cette sortie pour m'aider à progresser :\n{detail}\n\
Réponds en trois parties courtes, avec ces intitulés en gras : **Ce qui va bien**, **À travailler**, **Prochaine séance** \
(une séance précise pour les prochains jours, adaptée à ma fraîcheur). 180 mots au plus."
    )
}

// --- Séance structurée ----------------------------------------------------------------

/// Forme demandée au modèle : des minutes plutôt que des secondes (moins d'erreurs).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LlmStep {
    kind: String,
    minutes: f64,
    power_low: f64,
    power_high: f64,
    #[serde(default)]
    cadence_low: Option<f64>,
    #[serde(default)]
    cadence_high: Option<f64>,
    #[serde(default)]
    note: String,
}

#[derive(Debug, Deserialize)]
struct LlmBlock {
    repeat: f64,
    steps: Vec<LlmStep>,
}

#[derive(Debug, Deserialize)]
struct LlmWorkout {
    name: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    terrain: String,
    blocks: Vec<LlmBlock>,
}

pub fn workout_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "goal": { "type": "string" },
            "terrain": { "type": "string", "enum": ["climb", "flat", "any"] },
            "blocks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "repeat": { "type": "integer" },
                        "steps": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "kind": { "type": "string", "enum": ["warmup", "work", "recovery", "steady", "cooldown"] },
                                    "minutes": { "type": "number" },
                                    "powerLow": { "type": "number" },
                                    "powerHigh": { "type": "number" },
                                    "cadenceLow": { "type": "integer" },
                                    "cadenceHigh": { "type": "integer" },
                                    "note": { "type": "string" }
                                },
                                "required": ["kind", "minutes", "powerLow", "powerHigh", "note"]
                            }
                        }
                    },
                    "required": ["repeat", "steps"]
                }
            }
        },
        "required": ["name", "goal", "terrain", "blocks"]
    })
}

pub fn workout_prompt(wish: &str) -> String {
    let example = json!({
        "name": "Côte 3 × 8 min",
        "goal": "Monter plus vite : effort au seuil en montée, cadence un peu basse.",
        "terrain": "climb",
        "blocks": [
            { "repeat": 1, "steps": [{ "kind": "warmup", "minutes": 15, "powerLow": 50, "powerHigh": 70, "cadenceLow": 85, "cadenceHigh": 95, "note": "Monte progressivement" }] },
            { "repeat": 3, "steps": [
                { "kind": "work", "minutes": 8, "powerLow": 95, "powerHigh": 105, "cadenceLow": 70, "cadenceHigh": 80, "note": "Assise, buste calme" },
                { "kind": "recovery", "minutes": 6, "powerLow": 40, "powerHigh": 55, "note": "Redescends au pied" }
            ] },
            { "repeat": 1, "steps": [{ "kind": "cooldown", "minutes": 10, "powerLow": 40, "powerHigh": 55, "note": "Tout souple" }] }
        ]
    });
    let wish = wish.trim();
    let wish = if wish.is_empty() { "la séance la plus utile pour moi aujourd'hui" } else { wish };
    format!(
        "Crée UNE séance de vélo pour : {wish}.\n\
Adapte-la à ma forme, à ma fraîcheur et à mes dernières sorties. \
Les puissances sont en % de ma FTP (powerLow et powerHigh, par exemple 88 et 94). \
« terrain » vaut « climb » si les efforts se font en montée, « flat » s'ils demandent une longue portion plate, sinon « any ». \
« name » : 40 caractères au plus. « goal » : une ou deux phrases qui expliquent pourquoi cette séance me fera aller plus vite. \
« note » : consigne de 6 mots au plus. Commence par un échauffement et finis par un retour au calme.\n\
Exemple de format (à ne pas recopier) : {example}\n\
Réponds uniquement avec le JSON."
    )
}

/// Lit la séance renvoyée par le modèle et la rend utilisable.
pub fn parse_workout(text: &str) -> Result<Workout, String> {
    let start = text.find('{').ok_or("Le coach n'a pas renvoyé de séance : réessaie.")?;
    let end = text.rfind('}').ok_or("Le coach n'a pas renvoyé de séance : réessaie.")?;
    let w: LlmWorkout = serde_json::from_str(&text[start..=end]).map_err(|e| {
        eprintln!("Séance illisible : {e}\n{text}");
        "La séance proposée est illisible : réessaie.".to_string()
    })?;
    let kind = |k: &str| match k {
        "warmup" => StepKind::Warmup,
        "work" => StepKind::Work,
        "recovery" => StepKind::Recovery,
        "cooldown" => StepKind::Cooldown,
        _ => StepKind::Steady,
    };
    let cad = |c: Option<f64>| c.filter(|c| c.is_finite() && *c > 0.0).map(|c| c.round() as u16);
    Workout {
        name: w.name,
        goal: w.goal,
        terrain: match w.terrain.as_str() {
            "climb" => Terrain::Climb,
            "flat" => Terrain::Flat,
            _ => Terrain::Any,
        },
        blocks: w
            .blocks
            .into_iter()
            .map(|b| Block {
                repeat: b.repeat.round().max(1.0) as u32,
                steps: b
                    .steps
                    .into_iter()
                    .map(|s| Step {
                        kind: kind(&s.kind),
                        duration_s: (s.minutes.max(0.0) * 60.0).round() as u32,
                        power_low: s.power_low as f32,
                        power_high: s.power_high as f32,
                        cadence_low: cad(s.cadence_low),
                        cadence_high: cad(s.cadence_high),
                        note: s.note,
                    })
                    .collect(),
            })
            .collect(),
    }
    .normalized()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn think_filter_removes_reasoning_even_when_split() {
        let mut f = ThinkFilter::default();
        let mut out = String::new();
        for chunk in ["Bon", "jour <thi", "nk>je réfléchis…</th", "ink> à toi", " ! <", "b>ok"] {
            out += &f.push(chunk);
        }
        out += &f.pending;
        assert_eq!(out, "Bonjour  à toi ! <b>ok");
    }

    #[test]
    fn think_filter_keeps_accents_intact() {
        let mut f = ThinkFilter::default();
        let mut out = String::new();
        for chunk in ["Fraîch", "eur ", "très négative <", "think>é</think>é"] {
            out += &f.push(chunk);
        }
        out += &f.pending;
        assert_eq!(out, "Fraîcheur très négative é");
    }

    #[test]
    fn tool_calls_written_as_text_are_recovered() {
        let (text, calls) = tool_calls_in_text(
            "Je lance ça. <tool_call>\n{\"name\": \"propose_loops\", \"arguments\": {\"distance_km\": 30, \"profile\": \"gravel\"}}\n</tool_call>",
        );
        assert_eq!(text.trim(), "Je lance ça.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "propose_loops");
        assert_eq!(calls[0].function.arguments["distance_km"], 30);
    }

    #[test]
    fn tools_are_well_formed() {
        let t = tools();
        let names: Vec<&str> = t.as_array().unwrap().iter().map(|x| x["function"]["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"propose_loops") && names.contains(&"create_workout"));
        for x in t.as_array().unwrap() {
            assert_eq!(x["type"], "function");
            assert_eq!(x["function"]["parameters"]["type"], "object");
            assert!(!x["function"]["description"].as_str().unwrap().is_empty());
        }
        // Messages « tool » envoyés à Ollama : sans champs vides inutiles
        let m = serde_json::to_value(ChatMessage::new("user", "salut")).unwrap();
        assert!(m.get("tool_calls").is_none() && m.get("tool_name").is_none());
        let back: ChatMessage = serde_json::from_value(json!({ "role": "assistant", "tool_calls": [{ "function": { "name": "show_progress", "arguments": {} } }] })).unwrap();
        assert_eq!(back.tool_calls.unwrap()[0].function.name, "show_progress");
    }

    #[test]
    fn dates_in_french() {
        // 9 novembre 2026, 15 h 21 à Paris (UTC+1)
        assert_eq!(date_fr(1_794_234_060, 3600, true), "lundi 9 nov. 2026");
        assert_eq!(date_fr(0, 0, true), "jeudi 1 janv. 1970");
    }

    #[test]
    fn same_model_names() {
        assert!(same_model("qwen3:8b", "qwen3:8b"));
        assert!(same_model("llama3.1", "llama3.1:latest"));
        assert!(!same_model("qwen3:8b", "qwen3:14b"));
    }

    #[test]
    fn parses_a_model_workout() {
        let text = r#"Voici : {"name":"Seuil 2 × 15 min","goal":"Rouler plus vite seule.","terrain":"flat","blocks":[
            {"repeat":1,"steps":[{"kind":"warmup","minutes":15,"powerLow":0.5,"powerHigh":0.7,"note":"Progressif"}]},
            {"repeat":2,"steps":[{"kind":"work","minutes":15,"powerLow":95,"powerHigh":100,"cadenceLow":88,"cadenceHigh":95,"note":"Aéro"},
                                 {"kind":"recovery","minutes":5,"powerLow":50,"powerHigh":60,"note":""}]},
            {"repeat":1,"steps":[{"kind":"cooldown","minutes":10,"powerLow":40,"powerHigh":55,"note":"Souple"}]}]}"#;
        let w = parse_workout(text).unwrap();
        assert_eq!(w.terrain, Terrain::Flat);
        assert_eq!(w.total_s(), (15 + 2 * 20 + 10) * 60);
        assert_eq!(w.longest_effort_s(), 15 * 60);
        assert!((w.blocks[0].steps[0].power_low - 50.0).abs() < 0.01, "fraction convertie en %");
        assert!(parse_workout("désolé").is_err());
    }
}

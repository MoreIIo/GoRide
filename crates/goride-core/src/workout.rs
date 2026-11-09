//! Séances d'entraînement structurées (F7.3) : modèle, séances types, export FIT
//! pour le compteur Garmin (fichier « Workout », à copier dans `Garmin/NewFiles`).
//!
//! Les cibles sont exprimées en % de la FTP. À l'export, elles deviennent des watts
//! si la FTP est connue, sinon des fréquences cardiaques (% de la FC max), sinon
//! l'étape reste « libre » avec la consigne dans son nom.

use crate::fit_export::{crc16, fit_header, Writer, ENUM, STRING, UINT16, UINT32, UINT32Z};
use serde::{Deserialize, Serialize};

/// Terrain dont la séance a besoin, pour lui proposer un itinéraire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Terrain {
    /// Les efforts se font en montée.
    Climb,
    /// Les efforts demandent une portion plate et roulante, sans relance.
    Flat,
    #[default]
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    Warmup,
    Work,
    Recovery,
    Steady,
    Cooldown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub kind: StepKind,
    pub duration_s: u32,
    /// Fourchette de puissance en % de la FTP.
    pub power_low: f32,
    pub power_high: f32,
    /// Fourchette de cadence (tr/min), facultative.
    #[serde(default)]
    pub cadence_low: Option<u16>,
    #[serde(default)]
    pub cadence_high: Option<u16>,
    /// Consigne courte (« assise, mains en haut du cintre »).
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    /// Nombre de répétitions du bloc (1 = une seule fois).
    pub repeat: u32,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workout {
    pub name: String,
    /// À quoi sert la séance, en une ou deux phrases.
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub terrain: Terrain,
    pub blocks: Vec<Block>,
}

/// Correspondance % FTP → % FC max (repères usuels des zones de Coggan).
const POWER_TO_HR: [(f32, f32); 6] = [(40.0, 55.0), (55.0, 62.0), (75.0, 72.0), (90.0, 82.0), (105.0, 90.0), (120.0, 95.0)];

/// % de la FC max correspondant à un % de FTP (interpolation linéaire, plafonnée à 97 %).
pub fn hr_pct_for_power_pct(p: f32) -> f32 {
    let t = &POWER_TO_HR;
    if p <= t[0].0 {
        return t[0].1;
    }
    for w in t.windows(2) {
        let ((p0, h0), (p1, h1)) = (w[0], w[1]);
        if p <= p1 {
            return h0 + (h1 - h0) * (p - p0) / (p1 - p0);
        }
    }
    (t[t.len() - 1].1 + (p - t[t.len() - 1].0) * 0.1).min(97.0)
}

/// Nom d'une zone de puissance (Coggan) pour un % de FTP.
pub fn zone_name(pct: f32) -> &'static str {
    match pct {
        p if p < 56.0 => "Z1 récupération",
        p if p < 76.0 => "Z2 endurance",
        p if p < 88.0 => "Z3 tempo",
        p if p < 95.0 => "Sweet spot",
        p if p < 106.0 => "Z4 seuil",
        p if p < 121.0 => "Z5 VO2max",
        p if p < 151.0 => "Z6 anaérobie",
        _ => "Z7 sprint",
    }
}

impl Step {
    fn mid(&self) -> f32 {
        (self.power_low + self.power_high) / 2.0
    }
}

impl Workout {
    /// Corrige les valeurs aberrantes (séance proposée par le coach IA) et refuse
    /// une séance vide. Renvoie la séance utilisable.
    pub fn normalized(mut self) -> Result<Workout, String> {
        self.name = self.name.trim().chars().take(60).collect();
        if self.name.is_empty() {
            self.name = "Séance".into();
        }
        self.goal = self.goal.trim().chars().take(400).collect();
        self.blocks.retain(|b| !b.steps.is_empty());
        for b in &mut self.blocks {
            b.repeat = b.repeat.clamp(1, 30);
            b.steps.truncate(8);
            for s in &mut b.steps {
                s.duration_s = s.duration_s.clamp(10, 4 * 3600);
                // Un modèle peut donner une fraction (0,9) au lieu d'un pourcentage (90)
                if s.power_low > 0.0 && s.power_low <= 3.0 && s.power_high <= 3.0 {
                    s.power_low *= 100.0;
                    s.power_high *= 100.0;
                }
                s.power_low = s.power_low.clamp(30.0, 250.0);
                s.power_high = s.power_high.clamp(30.0, 250.0);
                if s.power_high < s.power_low {
                    std::mem::swap(&mut s.power_low, &mut s.power_high);
                }
                if let (Some(l), Some(h)) = (s.cadence_low, s.cadence_high) {
                    if h < l {
                        s.cadence_low = Some(h);
                        s.cadence_high = Some(l);
                    }
                }
                s.cadence_low = s.cadence_low.filter(|c| (40..=140).contains(c));
                s.cadence_high = s.cadence_high.filter(|c| (40..=140).contains(c));
                s.note = s.note.trim().chars().take(120).collect();
            }
        }
        if self.blocks.is_empty() {
            return Err("La séance proposée ne contient aucune étape.".into());
        }
        if self.total_s() > 6 * 3600 {
            return Err("La séance proposée dure plus de 6 heures : demande-la à nouveau.".into());
        }
        Ok(self)
    }

    /// Durée totale (s).
    pub fn total_s(&self) -> u32 {
        self.blocks.iter().map(|b| b.repeat * b.steps.iter().map(|s| s.duration_s).sum::<u32>()).sum()
    }

    /// Temps passé sur les efforts (étapes « work ») (s).
    pub fn work_s(&self) -> u32 {
        self.blocks
            .iter()
            .map(|b| b.repeat * b.steps.iter().filter(|s| s.kind == StepKind::Work).map(|s| s.duration_s).sum::<u32>())
            .sum()
    }

    /// Plus long effort d'un seul tenant (s) : ce que l'itinéraire doit permettre.
    pub fn longest_effort_s(&self) -> u32 {
        self.blocks
            .iter()
            .flat_map(|b| b.steps.iter())
            .filter(|s| s.kind == StepKind::Work)
            .map(|s| s.duration_s)
            .max()
            .unwrap_or(0)
    }

    /// Charge estimée (TSS), avec la puissance au milieu de chaque fourchette.
    pub fn tss(&self) -> f32 {
        self.blocks
            .iter()
            .map(|b| {
                b.repeat as f32
                    * b.steps.iter().map(|s| s.duration_s as f32 / 3600.0 * (s.mid() / 100.0).powi(2) * 100.0).sum::<f32>()
            })
            .sum()
    }

    /// Description en texte (contexte du coach, résumé lisible).
    pub fn describe(&self) -> String {
        let mut out = format!("{} ({} min, TSS ≈ {:.0})", self.name, self.total_s() / 60, self.tss());
        for b in &self.blocks {
            let steps: Vec<String> = b
                .steps
                .iter()
                .map(|s| {
                    let mut t = format!("{} à {:.0}–{:.0} % FTP", fmt_duration(s.duration_s), s.power_low, s.power_high);
                    if let (Some(l), Some(h)) = (s.cadence_low, s.cadence_high) {
                        t += &format!(" ({l}–{h} tr/min)");
                    }
                    t
                })
                .collect();
            if b.repeat > 1 {
                out += &format!("\n- {} × [{}]", b.repeat, steps.join(" + "));
            } else {
                out += &format!("\n- {}", steps.join(" + "));
            }
        }
        out
    }
}

/// « 8 min », « 1 h 30 », « 30 s ».
pub fn fmt_duration(s: u32) -> String {
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        if s % 60 == 0 {
            format!("{} min", s / 60)
        } else {
            format!("{} min {:02}", s / 60, s % 60)
        }
    } else {
        let m = (s % 3600) / 60;
        if m == 0 {
            format!("{} h", s / 3600)
        } else {
            format!("{} h {m:02}", s / 3600)
        }
    }
}

// --- Séances types -------------------------------------------------------------------

fn step(kind: StepKind, min: f32, low: f32, high: f32) -> Step {
    Step { kind, duration_s: (min * 60.0).round() as u32, power_low: low, power_high: high, cadence_low: None, cadence_high: None, note: String::new() }
}

fn with(mut s: Step, cad: Option<(u16, u16)>, note: &str) -> Step {
    if let Some((l, h)) = cad {
        s.cadence_low = Some(l);
        s.cadence_high = Some(h);
    }
    s.note = note.into();
    s
}

fn once(steps: Vec<Step>) -> Block {
    Block { repeat: 1, steps }
}

fn warmup(min: f32) -> Block {
    once(vec![with(step(StepKind::Warmup, min, 50.0, 70.0), Some((85, 95)), "Monte progressivement")])
}

fn cooldown(min: f32) -> Block {
    once(vec![with(step(StepKind::Cooldown, min, 40.0, 55.0), Some((85, 100)), "Tout souple")])
}

/// Séances types, utilisables sans l'IA et proposées comme base au coach.
pub fn templates() -> Vec<Workout> {
    use StepKind::*;
    vec![
        Workout {
            name: "Endurance 1 h 30".into(),
            goal: "Construire le moteur aérobie : le socle de la vitesse sur toutes les sorties. Tu dois pouvoir parler en roulant.".into(),
            terrain: Terrain::Any,
            blocks: vec![
                warmup(10.0),
                once(vec![with(step(Steady, 70.0, 65.0, 75.0), Some((85, 95)), "Régulier, même dans les bosses")]),
                cooldown(10.0),
            ],
        },
        Workout {
            name: "Sweet spot 3 × 12 min".into(),
            goal: "Repousser le seuil avec une fatigue modérée : la séance la plus rentable pour rouler plus vite longtemps.".into(),
            terrain: Terrain::Flat,
            blocks: vec![
                warmup(15.0),
                Block {
                    repeat: 3,
                    steps: vec![
                        with(step(Work, 12.0, 88.0, 94.0), Some((85, 95)), "Soutenu mais tenable"),
                        with(step(Recovery, 5.0, 50.0, 60.0), None, "Récupère"),
                    ],
                },
                cooldown(10.0),
            ],
        },
        Workout {
            name: "Seuil 2 × 20 min".into(),
            goal: "Élever la puissance que tu tiens une heure : directement la vitesse en contre-la-montre et en triathlon.".into(),
            terrain: Terrain::Flat,
            blocks: vec![
                warmup(15.0),
                Block {
                    repeat: 2,
                    steps: vec![
                        with(step(Work, 20.0, 95.0, 100.0), Some((85, 95)), "Position aéro si possible"),
                        with(step(Recovery, 6.0, 50.0, 60.0), None, "Récupère"),
                    ],
                },
                cooldown(10.0),
            ],
        },
        Workout {
            name: "Côte 3 × 8 min".into(),
            goal: "Monter plus vite : effort au seuil en montée, cadence un peu basse, redescente en récupération.".into(),
            terrain: Terrain::Climb,
            blocks: vec![
                warmup(15.0),
                Block {
                    repeat: 3,
                    steps: vec![
                        with(step(Work, 8.0, 95.0, 105.0), Some((70, 80)), "Assise, buste calme"),
                        with(step(Recovery, 6.0, 40.0, 55.0), None, "Redescends au pied"),
                    ],
                },
                cooldown(10.0),
            ],
        },
        Workout {
            name: "VO2max 5 × 4 min".into(),
            goal: "Augmenter ta puissance maximale aérobie : relances, bosses courtes, accélérations.".into(),
            terrain: Terrain::Any,
            blocks: vec![
                warmup(15.0),
                Block {
                    repeat: 5,
                    steps: vec![
                        with(step(Work, 4.0, 110.0, 120.0), Some((90, 100)), "Dur, régulier jusqu'au bout"),
                        with(step(Recovery, 4.0, 45.0, 55.0), None, "Récupère"),
                    ],
                },
                cooldown(10.0),
            ],
        },
        Workout {
            name: "Tempo 3 × 15 min".into(),
            goal: "Tenir une allure de course longtemps sans à-coups : le cœur de la préparation triathlon et CLM.".into(),
            terrain: Terrain::Flat,
            blocks: vec![
                warmup(15.0),
                Block {
                    repeat: 3,
                    steps: vec![
                        with(step(Work, 15.0, 80.0, 88.0), Some((85, 95)), "Allure course, position stable"),
                        with(step(Recovery, 5.0, 55.0, 65.0), None, "Souple"),
                    ],
                },
                cooldown(10.0),
            ],
        },
        Workout {
            name: "Force en côte 6 × 2 min".into(),
            goal: "Gagner en force : gros braquet, cadence basse, assise. Améliore les relances et les montées raides.".into(),
            terrain: Terrain::Climb,
            blocks: vec![
                warmup(15.0),
                Block {
                    repeat: 6,
                    steps: vec![
                        with(step(Work, 2.0, 85.0, 95.0), Some((55, 65)), "Assise, gros braquet"),
                        with(step(Recovery, 3.0, 45.0, 55.0), Some((90, 100)), "Moulinez"),
                    ],
                },
                cooldown(10.0),
            ],
        },
        Workout {
            name: "Récupération active 45 min".into(),
            goal: "Faire tourner les jambes après une grosse journée, sans ajouter de fatigue.".into(),
            terrain: Terrain::Flat,
            blocks: vec![once(vec![with(step(Steady, 45.0, 45.0, 55.0), Some((90, 100)), "Très facile")])],
        },
    ]
}

// --- Export FIT ----------------------------------------------------------------------

const MSG_FILE_ID: u16 = 0;
const MSG_WORKOUT: u16 = 26;
const MSG_WORKOUT_STEP: u16 = 27;
const NAME_LEN: usize = 32;
const STEP_NAME_LEN: usize = 32;
const NOTE_LEN: usize = 64;
const FIT_EPOCH_OFFSET: u64 = 631_065_600;

// Valeurs FIT
const DURATION_TIME: u8 = 0;
const DURATION_REPEAT: u8 = 6;
const TARGET_HR: u8 = 1;
const TARGET_OPEN: u8 = 2;
const TARGET_POWER: u8 = 4;

fn intensity(k: StepKind) -> u8 {
    match k {
        StepKind::Work | StepKind::Steady => 0,
        StepKind::Warmup => 2,
        StepKind::Cooldown => 3,
        StepKind::Recovery => 4,
    }
}

fn step_label(k: StepKind) -> &'static str {
    match k {
        StepKind::Warmup => "Échauffement",
        StepKind::Work => "Effort",
        StepKind::Recovery => "Récup",
        StepKind::Steady => "Régulier",
        StepKind::Cooldown => "Retour au calme",
    }
}

/// Encode la séance en fichier FIT « Workout » pour Garmin Edge.
/// `ftp` (W) et `hr_max` (bpm) fixent les cibles ; sans aucun des deux, étapes libres.
pub fn to_fit_workout(w: &Workout, ftp: Option<f32>, hr_max: Option<f32>, now_unix: u64) -> Vec<u8> {
    let t0 = now_unix.saturating_sub(FIT_EPOCH_OFFSET) as u32;
    let ftp = ftp.filter(|f| *f >= 50.0);
    let hr_max = hr_max.filter(|h| *h >= 120.0);

    // Étapes à plat : les blocs répétés finissent par une étape « répéter depuis… »
    enum Out<'a> {
        Step(&'a Step),
        Repeat { from: u16, times: u32 },
    }
    let mut out: Vec<Out> = Vec::new();
    for b in &w.blocks {
        let from = out.len() as u16;
        out.extend(b.steps.iter().map(Out::Step));
        if b.repeat > 1 {
            out.push(Out::Repeat { from, times: b.repeat });
        }
    }

    let mut wr = Writer { data: Vec::with_capacity(out.len() * 160 + 128) };
    // file_id : type = séance (5)
    wr.define(0, MSG_FILE_ID, &[(0, 1, ENUM), (1, 2, UINT16), (2, 2, UINT16), (3, 4, UINT32Z), (4, 4, UINT32)]);
    wr.header(0);
    wr.u8(5);
    wr.u16(255);
    wr.u16(1);
    wr.u32(t0.max(1));
    wr.u32(t0);

    // workout : sport vélo (2), nombre d'étapes, nom
    wr.define(1, MSG_WORKOUT, &[(4, 1, ENUM), (6, 2, UINT16), (8, NAME_LEN as u8, STRING)]);
    wr.header(1);
    wr.u8(2);
    wr.u16(out.len() as u16);
    wr.string(&w.name, NAME_LEN);

    wr.define(
        2,
        MSG_WORKOUT_STEP,
        &[
            (254, 2, UINT16),
            (0, STEP_NAME_LEN as u8, STRING),
            (1, 1, ENUM),
            (2, 4, UINT32),
            (3, 1, ENUM),
            (4, 4, UINT32),
            (5, 4, UINT32),
            (6, 4, UINT32),
            (7, 1, ENUM),
            (8, NOTE_LEN as u8, STRING),
        ],
    );
    for (i, o) in out.iter().enumerate() {
        wr.header(2);
        wr.u16(i as u16);
        match o {
            Out::Step(s) => {
                let (target, low, high, consigne) = match (ftp, hr_max) {
                    (Some(f), _) => {
                        let (l, h) = ((s.power_low / 100.0 * f).round(), (s.power_high / 100.0 * f).round());
                        (TARGET_POWER, l as u32 + 1000, h as u32 + 1000, format!("{l:.0}-{h:.0} W"))
                    }
                    (None, Some(m)) => {
                        let (l, h) = (
                            (hr_pct_for_power_pct(s.power_low) / 100.0 * m).round(),
                            (hr_pct_for_power_pct(s.power_high) / 100.0 * m).round(),
                        );
                        (TARGET_HR, l as u32 + 100, h as u32 + 100, format!("{l:.0}-{h:.0} bpm"))
                    }
                    (None, None) => (TARGET_OPEN, 0, 0, zone_name((s.power_low + s.power_high) / 2.0).to_string()),
                };
                let name = format!("{} {}", step_label(s.kind), consigne);
                wr.string(&name, STEP_NAME_LEN);
                wr.u8(DURATION_TIME);
                wr.u32(s.duration_s * 1000);
                wr.u8(target);
                wr.u32(0);
                wr.u32(low);
                wr.u32(high);
                wr.u8(intensity(s.kind));
                let mut note = s.note.clone();
                if let (Some(l), Some(h)) = (s.cadence_low, s.cadence_high) {
                    note = if note.is_empty() { format!("{l}-{h} tr/min") } else { format!("{l}-{h} tr/min · {note}") };
                }
                wr.string(&note, NOTE_LEN);
            }
            Out::Repeat { from, times } => {
                wr.string("", STEP_NAME_LEN);
                wr.u8(DURATION_REPEAT);
                wr.u32(*from as u32);
                wr.u8(TARGET_OPEN);
                wr.u32(*times);
                wr.u32(0);
                wr.u32(0);
                wr.u8(0);
                wr.string("", NOTE_LEN);
            }
        }
    }

    let mut data = fit_header(wr.data.len());
    data.extend_from_slice(&wr.data);
    let crc = crc16(&data);
    data.extend_from_slice(&crc.to_le_bytes());
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use fitparser::profile::MesgNum;
    use fitparser::Value;

    fn field(m: &fitparser::FitDataRecord, name: &str) -> Option<Value> {
        m.fields().iter().find(|f| f.name() == name).map(|f| f.value().clone())
    }

    #[test]
    fn templates_are_consistent() {
        for w in templates() {
            let n = w.clone().normalized().unwrap();
            assert_eq!(n.total_s(), w.total_s(), "{}", w.name);
            assert!(w.total_s() >= 40 * 60 && w.total_s() <= 2 * 3600, "{} : {} s", w.name, w.total_s());
            assert!(!w.goal.is_empty());
        }
        let cote = templates().into_iter().find(|w| w.name.starts_with("Côte")).unwrap();
        assert_eq!(cote.terrain, Terrain::Climb);
        assert_eq!(cote.longest_effort_s(), 8 * 60);
        assert_eq!(cote.work_s(), 24 * 60);
        // 15 + 3 × (8 + 6) + 10 = 67 min
        assert_eq!(cote.total_s(), 67 * 60);
    }

    #[test]
    fn normalizes_fractions_and_bad_values() {
        let w = Workout {
            name: "  ".into(),
            goal: String::new(),
            terrain: Terrain::Any,
            blocks: vec![
                Block { repeat: 0, steps: vec![Step { kind: StepKind::Work, duration_s: 2, power_low: 1.05, power_high: 0.95, cadence_low: Some(95), cadence_high: Some(80), note: String::new() }] },
                Block { repeat: 3, steps: vec![] },
            ],
        }
        .normalized()
        .unwrap();
        assert_eq!(w.name, "Séance");
        assert_eq!(w.blocks.len(), 1);
        let s = &w.blocks[0].steps[0];
        assert_eq!(w.blocks[0].repeat, 1);
        assert_eq!(s.duration_s, 10);
        assert!((s.power_low - 95.0).abs() < 0.01 && (s.power_high - 105.0).abs() < 0.01);
        assert_eq!((s.cadence_low, s.cadence_high), (Some(80), Some(95)));
        assert!(Workout { name: "x".into(), goal: String::new(), terrain: Terrain::Any, blocks: vec![] }.normalized().is_err());
    }

    #[test]
    fn hr_mapping_is_monotonic() {
        let mut prev = 0.0;
        for p in (30..=200).step_by(5) {
            let h = hr_pct_for_power_pct(p as f32);
            assert!(h >= prev && h <= 97.0, "{p} → {h}");
            prev = h;
        }
        assert!((hr_pct_for_power_pct(90.0) - 82.0).abs() < 0.01);
    }

    #[test]
    fn fit_workout_with_power_targets_and_repeats() {
        let w = templates().into_iter().find(|w| w.name.starts_with("Côte")).unwrap();
        let bytes = to_fit_workout(&w, Some(230.0), Some(190.0), 1_794_170_000);
        let msgs = fitparser::from_bytes(&bytes).expect("FIT illisible");
        assert_eq!(field(&msgs[0], "type"), Some(Value::String("workout".into())));
        let wk = msgs.iter().find(|m| m.kind() == MesgNum::Workout).unwrap();
        assert_eq!(field(wk, "wkt_name"), Some(Value::String("Côte 3 × 8 min".into())));
        assert_eq!(field(wk, "sport"), Some(Value::String("cycling".into())));

        let steps: Vec<_> = msgs.iter().filter(|m| m.kind() == MesgNum::WorkoutStep).collect();
        // échauffement, effort, récup, répéter, retour au calme
        assert_eq!(steps.len(), 5);
        assert_eq!(field(wk, "num_valid_steps"), Some(Value::UInt16(5)));

        let effort = steps[1];
        assert_eq!(field(effort, "duration_type"), Some(Value::String("time".into())));
        match field(effort, "duration_time").or_else(|| field(effort, "duration_value")) {
            Some(Value::Float64(t)) => assert!((t - 480.0).abs() < 0.01, "{t}"),
            Some(Value::UInt32(t)) => assert_eq!(t, 480_000),
            other => panic!("durée inattendue : {other:?}"),
        }
        assert_eq!(field(effort, "target_type"), Some(Value::String("power".into())));
        // 95 % et 105 % de 230 W = 219 et 241 W (+1000 : valeurs personnalisées)
        let watts = |name: &str| match field(effort, name) {
            Some(Value::UInt32(v)) => v as i64,
            Some(Value::SInt64(v)) => v,
            other => panic!("{name} inattendu : {other:?}"),
        };
        assert_eq!(watts("custom_target_power_low"), 1219);
        assert_eq!(watts("custom_target_power_high"), 1241);
        assert_eq!(field(effort, "intensity"), Some(Value::String("active".into())));

        let rep = steps[3];
        assert_eq!(field(rep, "duration_type"), Some(Value::String("repeat_until_steps_cmplt".into())));
        let from = ["duration_step", "duration_value"].iter().find_map(|n| field(rep, n));
        assert_eq!(from, Some(Value::UInt32(1)));
        let times = ["repeat_steps", "target_value"].iter().find_map(|n| field(rep, n));
        assert_eq!(times, Some(Value::UInt32(3)));
    }

    #[test]
    fn fit_workout_falls_back_to_heart_rate_then_open() {
        let w = &templates()[0];
        let hr = fitparser::from_bytes(&to_fit_workout(w, None, Some(190.0), 1_794_170_000)).unwrap();
        let s = hr.iter().find(|m| m.kind() == MesgNum::WorkoutStep).unwrap();
        assert_eq!(field(s, "target_type"), Some(Value::String("heart_rate".into())));

        let open = fitparser::from_bytes(&to_fit_workout(w, None, None, 1_794_170_000)).unwrap();
        let s = open.iter().find(|m| m.kind() == MesgNum::WorkoutStep).unwrap();
        assert_eq!(field(s, "target_type"), Some(Value::String("open".into())));
        match field(s, "wkt_step_name") {
            Some(Value::String(n)) => assert!(n.starts_with("Échauffement"), "{n}"),
            other => panic!("{other:?}"),
        }
    }
}

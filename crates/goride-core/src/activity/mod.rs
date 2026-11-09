//! Sorties réalisées (fichiers du compteur) : lecture et analyse.
//!
//! Une sortie est stockée en séries parallèles, un point par enregistrement
//! du compteur (en général une seconde). Les valeurs absentes valent NaN.

pub mod analysis;
pub mod progress;
mod parse;
mod time;

pub use parse::parse_activity;
use serde::{Deserialize, Serialize};

/// Résumé calculé par le compteur lui-même (message « session » du FIT), quand il existe.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub distance_m: Option<f64>,
    pub ascent_m: Option<f64>,
    pub descent_m: Option<f64>,
    pub timer_s: Option<f64>,
    pub elapsed_s: Option<f64>,
    pub avg_power: Option<f64>,
    pub normalized_power: Option<f64>,
    pub tss: Option<f64>,
    pub ftp: Option<f64>,
    pub calories: Option<f64>,
    pub avg_hr: Option<f64>,
    pub max_hr: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub name: Option<String>,
    /// Sport indiqué par le fichier (« cycling », « running »…).
    pub sport: Option<String>,
    /// Début de la sortie (secondes Unix, UTC).
    pub start_time: Option<i64>,
    /// Format d'origine : « fit », « gpx » ou « tcx ».
    pub format: String,
    /// Temps écoulé depuis le début (s).
    pub t: Vec<f32>,
    pub lon: Vec<f32>,
    pub lat: Vec<f32>,
    /// Altitude (m).
    pub ele: Vec<f32>,
    /// Distance cumulée (m).
    pub dist: Vec<f32>,
    /// Vitesse (m/s).
    pub speed: Vec<f32>,
    /// Fréquence cardiaque (bpm).
    pub hr: Vec<f32>,
    /// Cadence (tr/min).
    pub cad: Vec<f32>,
    /// Puissance (W).
    pub power: Vec<f32>,
    /// Température (°C).
    pub temp: Vec<f32>,
    pub device: DeviceSummary,
}

impl Activity {
    pub fn len(&self) -> usize {
        self.t.len()
    }
    pub fn is_empty(&self) -> bool {
        self.t.is_empty()
    }
    pub fn has(series: &[f32]) -> bool {
        series.iter().any(|v| v.is_finite())
    }
}

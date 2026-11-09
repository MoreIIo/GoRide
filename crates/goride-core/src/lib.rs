//! Cœur de calcul de GoRide, sans aucune dépendance à l'interface.
//!
//! - `track` / `gpx_import` / `gpx_export` : traces GPX et statistiques
//! - `osm_tags` / `graph` : réseau routier vélo construit depuis OpenStreetMap
//! - `router` : accroche au réseau et plus court chemin
//! - `dem` / `route_track` : altitude et bilan d'un itinéraire tracé
//! - `workout` : séances structurées et export FIT pour Garmin
//!
//! Tests : `cargo test -p goride-core`.

pub mod activity;
pub mod cost;
pub mod dem;
pub mod geo;
pub mod fit_export;
pub mod gpx_export;
pub mod gpx_import;
pub mod loops;
pub mod graph;
pub mod osm_tags;
pub mod route_track;
pub mod router;
pub mod track;
pub mod workout;
pub mod workout_route;

pub use gpx_import::parse_gpx;
pub use track::{Track, TrackStats};

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("fichier GPX illisible : {0}")]
    Gpx(String),
    #[error("le fichier ne contient aucun point de trace")]
    Empty,
}

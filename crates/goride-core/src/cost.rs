//! Modèle de coût du routage : c'est ici que se règlent les préférences.
//!
//! Coût d'un tronçon parcouru dans un sens =
//!   longueur × facteur (type de route, revêtement, profil, pistes cyclables)
//!   + terme de relief (selon le curseur dénivelé)
//!   × pénalité de pente si la pente dépasse le maximum choisi.
//! Plus le coût est bas, plus le tronçon est recherché. Un tronçon interdit
//! (ex. chemin en terre en profil Route) n'a pas de coût.

use crate::graph::{Graph, FLAG_ROUGH, FLAG_ROUNDABOUT};
use crate::osm_tags::{RoadClass, Surface};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Profile {
    /// Route : goudron uniquement, petites routes préférées.
    #[default]
    Road,
    /// Triathlon / contre-la-montre : goudron lisse, peu de carrefours et de relances.
    Triathlon,
    /// Gravel : chemins et pistes acceptés, même recherchés.
    Gravel,
}

impl Profile {
    /// Seul le goudron (certain ou présumé) est autorisé.
    pub fn paved_only(self) -> bool {
        !matches!(self, Profile::Gravel)
    }
}

/// Réglages choisis dans l'interface.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RouteOptions {
    pub profile: Profile,
    /// −1 = le plus plat possible, 0 = neutre, +1 = chercher les bosses.
    pub hills: f32,
    /// 0 = indifférent aux grands axes, 1 = les fuir.
    pub traffic: f32,
    /// −1 = éviter les pistes cyclables, 0 = neutre, +1 = les préférer.
    pub cycleways: f32,
    /// Pente maximale souhaitée (%) ; 0 = pas de limite.
    pub max_grade: f32,
}

impl Default for RouteOptions {
    fn default() -> Self {
        RouteOptions { profile: Profile::Road, hills: 0.0, traffic: 0.5, cycleways: 0.0, max_grade: 0.0 }
    }
}

const N_CLASS: usize = RoadClass::ALL.len();

/// Facteurs par type de route quand les grands axes sont indifférents (traffic = 0)…
const NEUTRAL: [f32; N_CLASS] = [1.6, 1.15, 1.05, 1.0, 1.0, 1.1, 1.3, 1.0, 1.2, 1.4];
/// … et quand on veut les fuir (traffic = 1). Ordre : voir `RoadClass::ALL`.
const AVOID: [f32; N_CLASS] = [6.0, 3.0, 1.8, 1.12, 1.0, 1.2, 1.45, 1.0, 1.25, 1.5];

/// Coût (en mètres équivalents) d'un mètre de montée quand on cherche le plat au maximum.
/// Ordre de grandeur physique : monter 1 m (80 kg) ≈ l'énergie de 35 à 40 m à plat à 25 km/h.
const FLAT_UP_COST: f32 = 40.0;
/// Les descentes comptent un peu aussi : freinages, relances, et il a fallu monter avant.
const FLAT_DOWN_COST: f32 = 4.0;
/// Remise maximale sur un tronçon qui monte, quand on cherche les bosses au maximum.
const HILL_BONUS: f32 = 0.55;
/// Pente moyenne (fraction) à partir de laquelle la remise est entière.
const HILL_FULL_GRADE: f32 = 0.04;
/// Multiplicateur d'un tronçon plus raide que la pente maximale choisie.
const TOO_STEEP: f32 = 12.0;

/// Coûts précalculés pour un jeu de réglages.
pub struct Cost {
    opts: RouteOptions,
    class_factor: [f32; N_CLASS],
    use_elevation: bool,
    /// Facteur minimal possible par mètre : sert à garder l'heuristique A* exacte.
    pub min_factor: f64,
}

fn class_index(c: RoadClass) -> usize {
    c as usize
}

impl Cost {
    pub fn new(opts: &RouteOptions, graph_has_elevation: bool) -> Cost {
        let mut o = *opts;
        o.hills = o.hills.clamp(-1.0, 1.0);
        o.traffic = o.traffic.clamp(0.0, 1.0);
        o.cycleways = o.cycleways.clamp(-1.0, 1.0);
        if o.profile == Profile::Gravel {
            o.traffic = o.traffic.max(0.7); // en gravel, la route n'est qu'une liaison
        }
        let mut class_factor = [0.0; N_CLASS];
        for (i, f) in class_factor.iter_mut().enumerate() {
            *f = NEUTRAL[i] + (AVOID[i] - NEUTRAL[i]) * o.traffic;
        }
        let cw = class_index(RoadClass::Cycleway);
        class_factor[cw] *= if o.cycleways >= 0.0 { 1.0 - 0.3 * o.cycleways } else { 1.0 + 1.5 * -o.cycleways };
        match o.profile {
            Profile::Road => {}
            Profile::Triathlon => {
                // Ville et chemins : relances permanentes, à éviter
                for (c, k) in [
                    (RoadClass::Residential, 1.4),
                    (RoadClass::Service, 1.5),
                    (RoadClass::Track, 1.6),
                    (RoadClass::Path, 2.0),
                    (RoadClass::Cycleway, 1.3),
                ] {
                    class_factor[class_index(c)] *= k;
                }
            }
            Profile::Gravel => {
                class_factor[class_index(RoadClass::Track)] *= 0.85;
            }
        }

        let mut cost = Cost { opts: o, class_factor, use_elevation: graph_has_elevation, min_factor: 1.0 };
        // Facteur minimal sur toutes les combinaisons possibles
        let mut min = f32::MAX;
        for c in RoadClass::ALL {
            for s in [Surface::Paved, Surface::Assumed, Surface::Unpaved] {
                if let Some(f) = cost.factor(c, s, false) {
                    min = min.min(f);
                }
            }
        }
        if cost.use_elevation && o.hills > 0.0 {
            min *= 1.0 - HILL_BONUS * o.hills;
        }
        cost.min_factor = min.max(0.05) as f64;
        cost
    }

    pub fn options(&self) -> &RouteOptions {
        &self.opts
    }

    /// Facteur par mètre selon le type de route et le revêtement ; `None` = interdit.
    fn factor(&self, class: RoadClass, surface: Surface, rough: bool) -> Option<f32> {
        let base = self.class_factor[class_index(class)];
        let surf = match (self.opts.profile, surface) {
            (Profile::Road | Profile::Triathlon, Surface::Unpaved) => return None,
            (Profile::Road, Surface::Assumed) => 1.1,
            (Profile::Triathlon, Surface::Assumed) => 1.25,
            (Profile::Gravel, Surface::Unpaved) => 0.8,
            _ => 1.0,
        };
        let rough = match (rough, self.opts.profile) {
            (false, _) | (true, Profile::Gravel) => 1.0,
            (true, Profile::Road) => 1.6,
            (true, Profile::Triathlon) => 3.0,
        };
        Some(base * surf * rough)
    }

    /// Le tronçon est-il praticable avec ces réglages (dans au moins un sens) ?
    #[inline]
    pub fn allowed(&self, g: &Graph, s: u32) -> bool {
        let su = s as usize;
        g.seg_dir[su] != 0 && (!self.opts.profile.paved_only() || g.seg_surface[su] != Surface::Unpaved)
    }

    /// Coût complet du tronçon `s` parcouru dans le sens `forward` ; `None` si interdit.
    pub fn edge(&self, g: &Graph, s: u32, forward: bool) -> Option<f64> {
        if !g.allows(s, forward) {
            return None;
        }
        let su = s as usize;
        let len = g.seg_len[su];
        let f = self.factor(g.seg_class[su], g.seg_surface[su], g.has_flag(s, FLAG_ROUGH))?;
        let mut c = len * f;

        if self.use_elevation {
            let (up, down, grade) = g.climb(s, forward);
            let h = self.opts.hills;
            if h < 0.0 {
                c += -h * (FLAT_UP_COST * up + FLAT_DOWN_COST * down);
            } else if h > 0.0 && len > 0.0 {
                let steepness = (up / len / HILL_FULL_GRADE).clamp(0.0, 1.0);
                c *= 1.0 - HILL_BONUS * h * steepness;
            }
            if self.opts.max_grade > 0.0 && grade > self.opts.max_grade {
                c = c * TOO_STEEP + 500.0;
            }
        }
        if self.opts.profile == Profile::Triathlon && g.has_flag(s, FLAG_ROUNDABOUT) {
            c += 25.0;
        }
        Some(c as f64)
    }

    /// Coût de passage par un sommet (carrefour), ajouté en y entrant.
    #[inline]
    pub fn node(&self, g: &Graph, v: u32) -> f64 {
        if self.opts.profile == Profile::Triathlon && g.incident(v).len() >= 3 {
            35.0
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_factor_is_a_lower_bound() {
        for profile in [Profile::Road, Profile::Triathlon, Profile::Gravel] {
            for hills in [-1.0, 0.0, 1.0] {
                for cycleways in [-1.0, 0.0, 1.0] {
                    let o = RouteOptions { profile, hills, cycleways, ..Default::default() };
                    let c = Cost::new(&o, true);
                    for class in RoadClass::ALL {
                        for s in [Surface::Paved, Surface::Assumed, Surface::Unpaved] {
                            if let Some(f) = c.factor(class, s, false) {
                                let worst = f as f64 * if hills > 0.0 { 1.0 - HILL_BONUS as f64 * hills as f64 } else { 1.0 };
                                assert!(worst >= c.min_factor - 1e-6, "{o:?} {class:?} {s:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn traffic_slider_orders_roads() {
        let calm = Cost::new(&RouteOptions { traffic: 1.0, ..Default::default() }, false);
        let any = Cost::new(&RouteOptions { traffic: 0.0, ..Default::default() }, false);
        let p = |c: &Cost, k| c.factor(k, Surface::Paved, false).unwrap();
        assert!(p(&calm, RoadClass::Primary) > 2.5 * p(&calm, RoadClass::Minor));
        assert!(p(&any, RoadClass::Primary) < 1.2 * p(&any, RoadClass::Minor));
    }

    #[test]
    fn surfaces_per_profile() {
        let road = Cost::new(&RouteOptions::default(), false);
        let gravel = Cost::new(&RouteOptions { profile: Profile::Gravel, ..Default::default() }, false);
        assert!(road.factor(RoadClass::Track, Surface::Unpaved, false).is_none());
        let g_unpaved = gravel.factor(RoadClass::Track, Surface::Unpaved, false).unwrap();
        let g_paved = gravel.factor(RoadClass::Minor, Surface::Paved, false).unwrap();
        assert!(g_unpaved < g_paved);
        let tri = Cost::new(&RouteOptions { profile: Profile::Triathlon, ..Default::default() }, false);
        assert!(
            tri.factor(RoadClass::Minor, Surface::Paved, true).unwrap()
                > 2.0 * tri.factor(RoadClass::Minor, Surface::Paved, false).unwrap()
        );
    }

    #[test]
    fn options_deserialize_with_defaults() {
        let o: RouteOptions = serde_json::from_str(r#"{"profile":"Gravel","hills":-0.5}"#).unwrap();
        assert_eq!(o.profile, Profile::Gravel);
        assert_eq!(o.hills, -0.5);
        assert_eq!(o.traffic, 0.5);
    }
}

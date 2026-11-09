//! Lecture des étiquettes OSM d'une voie : est-elle praticable à vélo,
//! quel type de route, quel revêtement, dans quel(s) sens.
//!
//! Les règles sont volontairement prudentes : en cas de doute sur le
//! revêtement d'une petite route, elle est marquée « présumée goudronnée »
//! (affichée comme telle), jamais « goudronnée » tout court.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum RoadClass {
    /// Voie rapide (trunk) ouverte aux vélos.
    Trunk,
    Primary,
    Secondary,
    Tertiary,
    /// Petite route de campagne (`unclassified`, `road`).
    Minor,
    Residential,
    Service,
    Cycleway,
    Track,
    Path,
}

impl RoadClass {
    pub const ALL: [RoadClass; 10] = [
        RoadClass::Trunk,
        RoadClass::Primary,
        RoadClass::Secondary,
        RoadClass::Tertiary,
        RoadClass::Minor,
        RoadClass::Residential,
        RoadClass::Service,
        RoadClass::Cycleway,
        RoadClass::Track,
        RoadClass::Path,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Surface {
    /// Revêtement indiqué dans OSM (asphalt, concrete…) ou grand axe.
    Paved,
    /// Pas d'étiquette surface : presque toujours goudronné pour une route,
    /// mais non vérifié. Signalé à l'utilisatrice.
    Assumed,
    /// Gravier, terre, herbe… Interdit en profil Route.
    Unpaved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WayInfo {
    pub class: RoadClass,
    pub surface: Surface,
    /// Praticable dans le sens de dessin de la voie.
    pub forward: bool,
    /// Praticable dans le sens inverse.
    pub backward: bool,
    /// Pont ou tunnel : l'altitude du terrain dessous/dessus ne vaut rien, on l'interpole.
    pub structure: bool,
    /// Rond-point.
    pub roundabout: bool,
    /// Revêtement dur mais inconfortable (pavés) ou en mauvais état.
    pub rough: bool,
}

const PAVED: &[&str] = &[
    "asphalt", "paved", "concrete", "concrete:plates", "concrete:lanes", "chipseal", "paving_stones",
    "sett", "cobblestone", "unhewn_cobblestone", "metal", "wood", "bricks", "tartan",
];
const UNPAVED: &[&str] = &[
    "unpaved", "gravel", "fine_gravel", "compacted", "dirt", "earth", "ground", "grass", "grass_paver",
    "sand", "mud", "pebblestone", "rock", "rocks", "woodchips", "stone", "shells", "clay", "soil",
];
const ROUGH: &[&str] = &["sett", "cobblestone", "unhewn_cobblestone", "cobblestone:flattened"];
const BIKE_YES: &[&str] = &["yes", "designated", "permissive", "destination", "official"];

/// Classe une voie à partir de ses étiquettes. `None` = inaccessible à vélo.
pub fn classify<'a>(tag: impl Fn(&str) -> Option<&'a str>) -> Option<WayInfo> {
    let highway = tag("highway")?;
    if tag("area") == Some("yes") {
        return None;
    }
    let bicycle = tag("bicycle");
    let bike_yes = bicycle.is_some_and(|b| BIKE_YES.contains(&b));

    let class = match highway {
        "trunk" | "trunk_link" => {
            if tag("motorroad") == Some("yes") {
                return None;
            }
            RoadClass::Trunk
        }
        "primary" | "primary_link" => RoadClass::Primary,
        "secondary" | "secondary_link" => RoadClass::Secondary,
        "tertiary" | "tertiary_link" => RoadClass::Tertiary,
        "unclassified" | "road" => RoadClass::Minor,
        "residential" | "living_street" => RoadClass::Residential,
        "service" => match tag("service") {
            Some("parking_aisle" | "driveway" | "drive-through" | "emergency_access") => return None,
            _ => RoadClass::Service,
        },
        "cycleway" => RoadClass::Cycleway,
        "track" => RoadClass::Track,
        "path" => RoadClass::Path,
        "footway" | "pedestrian" | "bridleway" if bike_yes => RoadClass::Path,
        _ => return None, // motorway, steps, construction, footway sans vélo…
    };

    // Accès
    if matches!(bicycle, Some("no" | "dismount" | "use_sidepath" | "private")) {
        return None;
    }
    let closed = |k: &str| matches!(tag(k), Some("no" | "private" | "agricultural" | "forestry" | "delivery" | "customers"));
    if (closed("access") || closed("vehicle")) && !bike_yes {
        return None;
    }

    // Revêtement
    let mut surface = match tag("surface") {
        Some(s) if PAVED.contains(&s) => Surface::Paved,
        Some(s) if UNPAVED.contains(&s) || s.starts_with("gravel") || s.starts_with("dirt") => Surface::Unpaved,
        Some(_) => Surface::Assumed, // valeur exotique : on ne conclut pas
        None => match class {
            RoadClass::Trunk | RoadClass::Primary | RoadClass::Secondary | RoadClass::Tertiary => Surface::Paved,
            RoadClass::Minor | RoadClass::Residential | RoadClass::Service | RoadClass::Cycleway => Surface::Assumed,
            RoadClass::Track => match tag("tracktype") {
                Some("grade1") => Surface::Assumed,
                _ => Surface::Unpaved,
            },
            RoadClass::Path => Surface::Unpaved,
        },
    };
    if matches!(tag("smoothness"), Some("very_bad" | "horrible" | "very_horrible" | "impassable")) {
        surface = Surface::Unpaved;
    }

    // Sens de circulation (pour le vélo)
    let roundabout = matches!(tag("junction"), Some("roundabout" | "circular"));
    let (mut forward, mut backward) = match tag("oneway") {
        Some("yes" | "true" | "1") => (true, false),
        Some("-1" | "reverse") => (false, true),
        Some("no" | "false" | "0") => (true, true),
        _ if roundabout => (true, false),
        _ => (true, true),
    };
    let contraflow = tag("oneway:bicycle") == Some("no")
        || tag("cycleway").is_some_and(|c| c.starts_with("opposite"))
        || ["cycleway:left", "cycleway:right", "cycleway:both"]
            .iter()
            .any(|k| tag(k).is_some_and(|c| c.starts_with("opposite")))
        || ["cycleway:left:oneway", "cycleway:right:oneway", "cycleway:both:oneway"]
            .iter()
            .any(|k| matches!(tag(k), Some("-1" | "no")));
    if contraflow {
        forward = true;
        backward = true;
    }
    if tag("oneway:bicycle") == Some("yes") && forward && backward {
        backward = false;
    }

    let structure = tag("bridge").is_some_and(|b| b != "no") || tag("tunnel").is_some_and(|t| t != "no");
    let rough = tag("surface").is_some_and(|s| ROUGH.contains(&s)) || tag("smoothness") == Some("bad");
    Some(WayInfo { class, surface, forward, backward, structure, roundabout, rough })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(tags: &[(&'static str, &'static str)]) -> Option<WayInfo> {
        classify(|k| tags.iter().find(|(kk, _)| *kk == k).map(|(_, v)| *v))
    }

    #[test]
    fn motorway_and_steps_excluded() {
        assert!(c(&[("highway", "motorway")]).is_none());
        assert!(c(&[("highway", "steps")]).is_none());
        assert!(c(&[("highway", "trunk"), ("motorroad", "yes")]).is_none());
    }

    #[test]
    fn small_road_without_surface_is_assumed() {
        let w = c(&[("highway", "unclassified")]).unwrap();
        assert_eq!(w.surface, Surface::Assumed);
        assert_eq!(c(&[("highway", "secondary")]).unwrap().surface, Surface::Paved);
    }

    #[test]
    fn tracks_and_paths_are_unpaved_by_default() {
        assert_eq!(c(&[("highway", "track")]).unwrap().surface, Surface::Unpaved);
        assert_eq!(c(&[("highway", "path")]).unwrap().surface, Surface::Unpaved);
        assert_eq!(c(&[("highway", "track"), ("surface", "asphalt")]).unwrap().surface, Surface::Paved);
        assert_eq!(c(&[("highway", "residential"), ("surface", "gravel")]).unwrap().surface, Surface::Unpaved);
    }

    #[test]
    fn flags() {
        assert!(c(&[("highway", "secondary"), ("bridge", "yes")]).unwrap().structure);
        assert!(c(&[("highway", "secondary"), ("bridge", "viaduct")]).unwrap().structure);
        assert!(!c(&[("highway", "secondary"), ("bridge", "no")]).unwrap().structure);
        assert!(c(&[("highway", "residential"), ("surface", "sett")]).unwrap().rough);
        assert!(!c(&[("highway", "residential"), ("surface", "asphalt")]).unwrap().rough);
    }

    #[test]
    fn access_rules() {
        assert!(c(&[("highway", "residential"), ("access", "private")]).is_none());
        assert!(c(&[("highway", "residential"), ("access", "no"), ("bicycle", "yes")]).is_some());
        assert!(c(&[("highway", "footway")]).is_none());
        assert!(c(&[("highway", "footway"), ("bicycle", "designated")]).is_some());
        assert!(c(&[("highway", "primary"), ("bicycle", "no")]).is_none());
        assert!(c(&[("highway", "service"), ("service", "parking_aisle")]).is_none());
    }

    #[test]
    fn oneway_rules() {
        let w = c(&[("highway", "residential"), ("oneway", "yes")]).unwrap();
        assert!(w.forward && !w.backward);
        let w = c(&[("highway", "residential"), ("oneway", "-1")]).unwrap();
        assert!(!w.forward && w.backward);
        let w = c(&[("highway", "residential"), ("oneway", "yes"), ("oneway:bicycle", "no")]).unwrap();
        assert!(w.forward && w.backward);
        let w = c(&[("highway", "primary"), ("junction", "roundabout")]).unwrap();
        assert!(w.forward && !w.backward && w.roundabout);
    }
}

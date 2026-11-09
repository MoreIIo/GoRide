//! Graphe routier compact.
//!
//! - Les **sommets** sont les intersections (nœuds OSM partagés par plusieurs
//!   voies) et les extrémités de voies.
//! - Les **tronçons** relient deux sommets ; ils gardent la géométrie
//!   complète de la route (tous les points intermédiaires).
//!
//! Tout est stocké en tableaux plats (pas d'objet par nœud) : le graphe de la
//! France tient en mémoire et se charge vite.

mod build;
pub mod elevation;
pub mod pbf;

pub use build::{build_graph, RawWay};

use crate::osm_tags::{RoadClass, Surface};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// À incrémenter dès que le format change : un vieux fichier sera reconstruit.
pub const FORMAT_VERSION: u32 = 3;
const MAGIC: &[u8; 8] = b"GORIDEGR";

pub const DIR_FORWARD: u8 = 1;
pub const DIR_BACKWARD: u8 = 2;

/// Drapeaux d'un tronçon (`seg_flags`).
pub const FLAG_STRUCTURE: u8 = 1;
pub const FLAG_ROUNDABOUT: u8 = 2;
pub const FLAG_ROUGH: u8 = 4;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Graph {
    /// Position (lon, lat) de chaque sommet.
    pub node_pos: Vec<[f32; 2]>,

    // --- Tronçons (non orientés) ---
    pub seg_from: Vec<u32>,
    pub seg_to: Vec<u32>,
    /// Longueur en mètres.
    pub seg_len: Vec<f32>,
    pub seg_class: Vec<RoadClass>,
    pub seg_surface: Vec<Surface>,
    /// Sens autorisés : `DIR_FORWARD` (from → to) et/ou `DIR_BACKWARD`.
    pub seg_dir: Vec<u8>,
    /// `FLAG_STRUCTURE` (pont, tunnel), `FLAG_ROUNDABOUT`, `FLAG_ROUGH` (pavés).
    pub seg_flags: Vec<u8>,
    /// Dénivelé positif en parcourant le tronçon dans son sens (from → to), en mètres.
    /// Dans l'autre sens, montée et descente s'échangent.
    pub seg_up: Vec<f32>,
    pub seg_down: Vec<f32>,
    /// Pente maximale de montée (%), sur au moins 80 m, dans chaque sens.
    pub seg_grade_fwd: Vec<i8>,
    pub seg_grade_bwd: Vec<i8>,
    /// Vrai si les altitudes ont été calculées.
    pub has_elevation: bool,
    /// Géométrie du tronçon i : `geom[seg_geom[i] .. seg_geom[i + 1]]`, extrémités comprises.
    pub seg_geom: Vec<u32>,
    pub geom: Vec<[f32; 2]>,

    // --- Adjacence (format CSR) ---
    /// Tronçons touchant le sommet v : `adj[adj_start[v] .. adj_start[v + 1]]`.
    pub adj_start: Vec<u32>,
    pub adj: Vec<u32>,
}

impl Graph {
    pub fn node_count(&self) -> usize {
        self.node_pos.len()
    }
    pub fn seg_count(&self) -> usize {
        self.seg_from.len()
    }
    pub fn seg_points(&self, s: u32) -> &[[f32; 2]] {
        let s = s as usize;
        &self.geom[self.seg_geom[s] as usize..self.seg_geom[s + 1] as usize]
    }
    pub fn incident(&self, v: u32) -> &[u32] {
        let v = v as usize;
        &self.adj[self.adj_start[v] as usize..self.adj_start[v + 1] as usize]
    }
    /// Autre extrémité du tronçon `s` vue depuis `v`, et vrai si on le parcourt dans son sens.
    #[inline]
    pub fn other_end(&self, s: u32, v: u32) -> (u32, bool) {
        let s = s as usize;
        if self.seg_from[s] == v {
            (self.seg_to[s], true)
        } else {
            (self.seg_from[s], false)
        }
    }
    /// (montée, descente, pente max de montée %) en parcourant `s` dans le sens indiqué.
    #[inline]
    pub fn climb(&self, s: u32, forward: bool) -> (f32, f32, f32) {
        let s = s as usize;
        if forward {
            (self.seg_up[s], self.seg_down[s], self.seg_grade_fwd[s] as f32)
        } else {
            (self.seg_down[s], self.seg_up[s], self.seg_grade_bwd[s] as f32)
        }
    }
    #[inline]
    pub fn has_flag(&self, s: u32, flag: u8) -> bool {
        self.seg_flags[s as usize] & flag != 0
    }
    #[inline]
    pub fn allows(&self, s: u32, forward: bool) -> bool {
        self.seg_dir[s as usize] & if forward { DIR_FORWARD } else { DIR_BACKWARD } != 0
    }

    /// Emprise (ouest, sud, est, nord).
    pub fn bbox(&self) -> [f32; 4] {
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for p in &self.node_pos {
            b[0] = b[0].min(p[0]);
            b[1] = b[1].min(p[1]);
            b[2] = b[2].max(p[0]);
            b[3] = b[3].max(p[1]);
        }
        b
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut w = BufWriter::new(std::fs::File::create(path)?);
        w.write_all(MAGIC)?;
        w.write_all(&FORMAT_VERSION.to_le_bytes())?;
        bincode::serialize_into(&mut w, self).map_err(std::io::Error::other)?;
        w.flush()
    }

    pub fn load(path: &Path) -> std::io::Result<Graph> {
        let mut r = BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
        let mut head = [0u8; 12];
        r.read_exact(&mut head)?;
        if &head[..8] != MAGIC {
            return Err(std::io::Error::other("ce fichier n'est pas un graphe GoRide"));
        }
        let version = u32::from_le_bytes(head[8..12].try_into().unwrap());
        if version != FORMAT_VERSION {
            return Err(std::io::Error::other(format!(
                "graphe au format {version}, format attendu {FORMAT_VERSION} : il faut le reconstruire"
            )));
        }
        bincode::deserialize_from(r).map_err(std::io::Error::other)
    }
}

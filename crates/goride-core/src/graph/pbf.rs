//! Lecture d'un extrait OpenStreetMap (.osm.pbf, ex. Geofabrik) en parallèle.

use super::{build_graph, Graph, RawWay};
use crate::osm_tags::classify;
use osmpbf::{BlobDecode, BlobReader, Element};
use rayon::prelude::*;
use std::path::Path;

/// Étapes signalées pendant la construction (pour la barre de progression).
#[derive(Debug, Clone, Copy)]
pub enum BuildStep {
    ReadingWays,
    ReadingNodes { ways: usize },
    Building { nodes: usize },
    Done,
}

/// Lit le fichier .pbf et construit le graphe routier vélo.
pub fn graph_from_pbf(path: &Path, progress: impl Fn(BuildStep)) -> Result<Graph, String> {
    graph_from_pbfs(&[path], progress)
}

/// Construit un seul graphe à partir de plusieurs extraits voisins (ex. deux régions).
/// Les voies qui franchissent une frontière figurent dans les deux fichiers : elles ne
/// sont gardées qu'une fois, et le réseau est continu d'une région à l'autre.
pub fn graph_from_pbfs<P: AsRef<Path>>(paths: &[P], progress: impl Fn(BuildStep)) -> Result<Graph, String> {
    let err = |e: osmpbf::Error| format!("lecture du fichier OSM impossible : {e}");

    // Passe 1 : les voies praticables à vélo.
    progress(BuildStep::ReadingWays);
    let mut ways: Vec<(i64, RawWay)> = Vec::new();
    for path in paths {
        let part: Vec<(i64, RawWay)> = BlobReader::from_path(path.as_ref())
            .map_err(err)?
            .par_bridge()
            .map(|blob| -> Result<Vec<(i64, RawWay)>, osmpbf::Error> {
                let mut out = Vec::new();
                if let BlobDecode::OsmData(block) = blob?.decode()? {
                    for el in block.elements() {
                        if let Element::Way(w) = el {
                            let tags: Vec<(&str, &str)> = w.tags().collect();
                            let info = classify(|k| tags.iter().find(|(kk, _)| *kk == k).map(|(_, v)| *v));
                            if let Some(info) = info {
                                out.push((w.id(), RawWay { nodes: w.refs().collect(), info }));
                            }
                        }
                    }
                }
                Ok(out)
            })
            .try_reduce(Vec::new, |mut a, b| {
                a.extend(b);
                Ok(a)
            })
            .map_err(err)?;
        ways.extend(part);
    }
    if paths.len() > 1 {
        ways.par_sort_unstable_by_key(|w| w.0);
        ways.dedup_by_key(|w| w.0);
    }
    let ways: Vec<RawWay> = ways.into_iter().map(|(_, w)| w).collect();
    if ways.is_empty() {
        return Err("aucune route praticable à vélo dans ce fichier".into());
    }

    // Passe 2 : coordonnées des seuls nœuds utilisés par ces voies.
    progress(BuildStep::ReadingNodes { ways: ways.len() });
    let mut needed: Vec<i64> = ways.iter().flat_map(|w| w.nodes.iter().copied()).collect();
    needed.par_sort_unstable();
    needed.dedup();

    let mut nodes: Vec<(i64, [f64; 2])> = Vec::new();
    for path in paths {
        let part: Vec<(i64, [f64; 2])> = BlobReader::from_path(path.as_ref())
            .map_err(err)?
            .par_bridge()
            .map(|blob| -> Result<Vec<(i64, [f64; 2])>, osmpbf::Error> {
                let mut out = Vec::new();
                if let BlobDecode::OsmData(block) = blob?.decode()? {
                    for el in block.elements() {
                        let (id, lon, lat) = match el {
                            Element::DenseNode(n) => (n.id(), n.lon(), n.lat()),
                            Element::Node(n) => (n.id(), n.lon(), n.lat()),
                            _ => continue,
                        };
                        if needed.binary_search(&id).is_ok() {
                            out.push((id, [lon, lat]));
                        }
                    }
                }
                Ok(out)
            })
            .try_reduce(Vec::new, |mut a, b| {
                a.extend(b);
                Ok(a)
            })
            .map_err(err)?;
        nodes.extend(part);
    }
    drop(needed);
    nodes.par_sort_unstable_by_key(|n| n.0);
    nodes.dedup_by_key(|n| n.0); // nœuds présents dans deux extraits voisins

    progress(BuildStep::Building { nodes: nodes.len() });
    let coord = |id: i64| nodes.binary_search_by_key(&id, |n| n.0).ok().map(|k| nodes[k].1);
    let graph = build_graph(&ways, coord);
    if graph.seg_count() == 0 {
        return Err("le réseau construit est vide".into());
    }
    progress(BuildStep::Done);
    Ok(graph)
}

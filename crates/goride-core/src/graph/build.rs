//! Construction du graphe à partir des voies OSM retenues.

use super::{Graph, DIR_BACKWARD, DIR_FORWARD, FLAG_ROUGH, FLAG_ROUNDABOUT, FLAG_STRUCTURE};
use crate::geo::haversine_m;
use crate::osm_tags::WayInfo;

/// Voie OSM déjà classée, avec la liste de ses nœuds.
#[derive(Debug, Clone)]
pub struct RawWay {
    pub nodes: Vec<i64>,
    pub info: WayInfo,
}

/// Composantes connexes plus petites que ce nombre de sommets supprimées :
/// ce sont des bouts de route isolés (erreurs de saisie, parkings…) sur
/// lesquels un clic resterait bloqué. Les îles (Belle-Île, Groix…) sont bien plus grandes.
const MIN_COMPONENT_NODES: usize = 50;

/// Construit le graphe. `coord` donne la position (lon, lat) d'un nœud OSM.
pub fn build_graph(ways: &[RawWay], coord: impl Fn(i64) -> Option<[f64; 2]>) -> Graph {
    // 1. Un nœud OSM devient un sommet s'il est partagé par plusieurs voies ou en bout de voie.
    let mut refs: Vec<i64> = ways.iter().flat_map(|w| w.nodes.iter().copied()).collect();
    refs.sort_unstable();
    let mut vertex_ids: Vec<i64> = Vec::new();
    {
        let mut i = 0;
        while i < refs.len() {
            let mut j = i + 1;
            while j < refs.len() && refs[j] == refs[i] {
                j += 1;
            }
            if j - i > 1 {
                vertex_ids.push(refs[i]);
            }
            i = j;
        }
    }
    for w in ways {
        if let (Some(&a), Some(&b)) = (w.nodes.first(), w.nodes.last()) {
            vertex_ids.push(a);
            vertex_ids.push(b);
        }
    }
    drop(refs);
    vertex_ids.sort_unstable();
    vertex_ids.dedup();
    let is_vertex = |id: i64| vertex_ids.binary_search(&id).is_ok();

    // 2. Découpage des voies en tronçons entre sommets.
    let mut g = Graph::default();
    let mut vertex_index: Vec<u32> = vec![u32::MAX; vertex_ids.len()];
    let mut vertex_of = |id: i64, pos: [f64; 2], g: &mut Graph| -> u32 {
        let k = vertex_ids.binary_search(&id).unwrap();
        if vertex_index[k] == u32::MAX {
            vertex_index[k] = g.node_pos.len() as u32;
            g.node_pos.push([pos[0] as f32, pos[1] as f32]);
        }
        vertex_index[k]
    };
    g.seg_geom.push(0);

    for w in ways {
        let dir = (if w.info.forward { DIR_FORWARD } else { 0 }) | (if w.info.backward { DIR_BACKWARD } else { 0 });
        let flags = (if w.info.structure { FLAG_STRUCTURE } else { 0 })
            | (if w.info.roundabout { FLAG_ROUNDABOUT } else { 0 })
            | (if w.info.rough { FLAG_ROUGH } else { 0 });
        if dir == 0 {
            continue;
        }
        // Points avec coordonnées connues (un nœud absent de l'extrait est ignoré).
        let pts: Vec<(i64, [f64; 2])> = w.nodes.iter().filter_map(|&id| coord(id).map(|c| (id, c))).collect();
        if pts.len() < 2 {
            continue;
        }
        let mut start_v = vertex_of(pts[0].0, pts[0].1, &mut g);
        let mut shape: Vec<[f64; 2]> = vec![pts[0].1];
        let mut len = 0.0;
        for (k, &(id, c)) in pts.iter().enumerate().skip(1) {
            let prev = *shape.last().unwrap();
            if c != prev {
                len += haversine_m(prev, c);
                shape.push(c);
            }
            let last = k == pts.len() - 1;
            if is_vertex(id) || last {
                let end_v = vertex_of(id, c, &mut g);
                if shape.len() >= 2 && len > 0.0 {
                    g.seg_from.push(start_v);
                    g.seg_to.push(end_v);
                    g.seg_len.push(len as f32);
                    g.seg_class.push(w.info.class);
                    g.seg_surface.push(w.info.surface);
                    g.seg_dir.push(dir);
                    g.seg_flags.push(flags);
                    g.geom.extend(shape.iter().map(|p| [p[0] as f32, p[1] as f32]));
                    g.seg_geom.push(g.geom.len() as u32);
                }
                start_v = end_v;
                shape.clear();
                shape.push(c);
                len = 0.0;
            }
        }
    }

    let n = g.seg_count();
    g.seg_up = vec![0.0; n];
    g.seg_down = vec![0.0; n];
    g.seg_grade_fwd = vec![0; n];
    g.seg_grade_bwd = vec![0; n];
    build_adjacency(&mut g);
    remove_small_components(g)
}

fn build_adjacency(g: &mut Graph) {
    let n = g.node_pos.len();
    let mut deg = vec![0u32; n + 1];
    for s in 0..g.seg_count() {
        deg[g.seg_from[s] as usize] += 1;
        if g.seg_to[s] != g.seg_from[s] {
            deg[g.seg_to[s] as usize] += 1;
        }
    }
    let mut start = vec![0u32; n + 1];
    for v in 0..n {
        start[v + 1] = start[v] + deg[v];
    }
    let mut fill = start.clone();
    let mut adj = vec![0u32; start[n] as usize];
    for s in 0..g.seg_count() as u32 {
        let (a, b) = (g.seg_from[s as usize] as usize, g.seg_to[s as usize] as usize);
        adj[fill[a] as usize] = s;
        fill[a] += 1;
        if a != b {
            adj[fill[b] as usize] = s;
            fill[b] += 1;
        }
    }
    g.adj_start = start;
    g.adj = adj;
}

/// Supprime les petites composantes connexes (sans tenir compte du sens unique).
fn remove_small_components(g: Graph) -> Graph {
    let n = g.node_count();
    let mut comp = vec![u32::MAX; n];
    let mut sizes: Vec<usize> = Vec::new();
    let mut stack = Vec::new();
    for root in 0..n as u32 {
        if comp[root as usize] != u32::MAX {
            continue;
        }
        let id = sizes.len() as u32;
        let mut size = 0;
        comp[root as usize] = id;
        stack.push(root);
        while let Some(v) = stack.pop() {
            size += 1;
            for &s in g.incident(v) {
                let (w, _) = g.other_end(s, v);
                if comp[w as usize] == u32::MAX {
                    comp[w as usize] = id;
                    stack.push(w);
                }
            }
        }
        sizes.push(size);
    }
    let keep = |v: u32| sizes[comp[v as usize] as usize] >= MIN_COMPONENT_NODES;
    if (0..n as u32).all(keep) {
        return g;
    }

    let mut out = Graph { has_elevation: g.has_elevation, ..Graph::default() };
    let mut remap = vec![u32::MAX; n];
    for v in 0..n as u32 {
        if keep(v) {
            remap[v as usize] = out.node_pos.len() as u32;
            out.node_pos.push(g.node_pos[v as usize]);
        }
    }
    out.seg_geom.push(0);
    for s in 0..g.seg_count() {
        let (a, b) = (g.seg_from[s], g.seg_to[s]);
        if !keep(a) {
            continue;
        }
        out.seg_from.push(remap[a as usize]);
        out.seg_to.push(remap[b as usize]);
        out.seg_len.push(g.seg_len[s]);
        out.seg_class.push(g.seg_class[s]);
        out.seg_surface.push(g.seg_surface[s]);
        out.seg_dir.push(g.seg_dir[s]);
        out.seg_flags.push(g.seg_flags[s]);
        out.seg_up.push(g.seg_up[s]);
        out.seg_down.push(g.seg_down[s]);
        out.seg_grade_fwd.push(g.seg_grade_fwd[s]);
        out.seg_grade_bwd.push(g.seg_grade_bwd[s]);
        out.geom.extend_from_slice(g.seg_points(s as u32));
        out.seg_geom.push(out.geom.len() as u32);
    }
    build_adjacency(&mut out);
    out
}

/// Assemble plusieurs réseaux voisins (une région et ses voisines) en un seul.
///
/// Chaque extrait régional contient en entier les routes qui passent la frontière :
/// elles figurent donc dans les deux réseaux, avec exactement les mêmes points. Les
/// sommets de même position sont fusionnés (le réseau devient continu d'une région à
/// l'autre) et les tronçons identiques ne sont gardés qu'une fois. Une route coupée
/// différemment de part et d'autre reste en double : sans effet sur le calcul.
pub fn merge_graphs(parts: Vec<Graph>) -> Graph {
    if parts.len() == 1 {
        return parts.into_iter().next().unwrap();
    }
    let key = |p: [f32; 2]| ((p[0].to_bits() as u64) << 32) | p[1].to_bits() as u64;
    let mut out = Graph { has_elevation: parts.iter().all(|g| g.has_elevation), ..Graph::default() };
    let nodes: usize = parts.iter().map(|g| g.node_count()).sum();
    let segs: usize = parts.iter().map(|g| g.seg_count()).sum();
    let mut vertex: std::collections::HashMap<u64, u32> = std::collections::HashMap::with_capacity(nodes);
    let mut seen: std::collections::HashSet<(u32, u32, u32)> = std::collections::HashSet::with_capacity(segs);
    out.seg_geom.push(0);
    for g in &parts {
        let remap: Vec<u32> = g
            .node_pos
            .iter()
            .map(|&p| {
                *vertex.entry(key(p)).or_insert_with(|| {
                    out.node_pos.push(p);
                    out.node_pos.len() as u32 - 1
                })
            })
            .collect();
        for s in 0..g.seg_count() {
            let (a, b) = (remap[g.seg_from[s] as usize], remap[g.seg_to[s] as usize]);
            if !seen.insert((a, b, g.seg_len[s].to_bits())) {
                continue; // même tronçon déjà apporté par une région voisine
            }
            out.seg_from.push(a);
            out.seg_to.push(b);
            out.seg_len.push(g.seg_len[s]);
            out.seg_class.push(g.seg_class[s]);
            out.seg_surface.push(g.seg_surface[s]);
            out.seg_dir.push(g.seg_dir[s]);
            out.seg_flags.push(g.seg_flags[s]);
            out.seg_up.push(g.seg_up[s]);
            out.seg_down.push(g.seg_down[s]);
            out.seg_grade_fwd.push(g.seg_grade_fwd[s]);
            out.seg_grade_bwd.push(g.seg_grade_bwd[s]);
            out.geom.extend_from_slice(g.seg_points(s as u32));
            out.seg_geom.push(out.geom.len() as u32);
        }
    }
    build_adjacency(&mut out);
    out
}

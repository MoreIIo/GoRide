//! Calcul d'itinéraire : accroche d'un clic au réseau, puis plus court chemin (A*).
//!
//! Le coût de chaque tronçon vient de `cost.rs` (profil et réglages). Un
//! tronçon sans coût est exclu : c'est ainsi que les profils Route et
//! Triathlon interdisent strictement les surfaces non revêtues.

pub use crate::cost::{Profile, RouteOptions};
use crate::cost::Cost;
use crate::geo::haversine_m;
use crate::graph::Graph;
use crate::osm_tags::{RoadClass, Surface};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

/// Distance maximale entre un clic et la route la plus proche.
pub const MAX_SNAP_M: f64 = 1_000.0;
/// Garde-fou : nombre maximal de sommets explorés pour une étape.
const MAX_SETTLED: usize = 8_000_000;
/// Un point ne s'accroche qu'à un tronçon relié à au moins ce nombre de sommets
/// par des routes autorisées : évite les « îlots » (bout de chemin goudronné entouré
/// de chemins en terre) d'où aucun itinéraire ne peut sortir.
const MIN_SNAP_COMPONENT: usize = 300;

#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    #[error("aucune route praticable à moins de {0:.0} m de ce point")]
    NoRoadNearby(f64),
    #[error("aucun itinéraire praticable avec ces réglages ne relie ces deux points")]
    NoPath,
    #[error("recherche trop longue : rapproche les points de passage")]
    TooFar,
}

/// Point accroché sur un tronçon.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Snap {
    pub seg: u32,
    /// Distance depuis le début du tronçon (m), mesurée le long de sa géométrie.
    pub offset: f64,
    /// Position (lon, lat) sur la route.
    pub point: [f64; 2],
    /// Distance entre le clic et la route (m).
    pub distance_m: f64,
}

/// Une étape calculée entre deux points de passage.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Leg {
    pub coords: Vec<[f64; 2]>,
    /// Revêtement de chaque portion (coords[i] → coords[i + 1]).
    pub surface: Vec<Surface>,
    /// Type de route de chaque portion.
    pub class: Vec<RoadClass>,
    pub length_m: f64,
    /// Tronçons parcourus : (tronçon, dans son sens ?, mètres parcourus). Interne (boucles).
    #[serde(skip)]
    pub segs: Vec<(u32, bool, f32)>,
}

impl Leg {
    /// Dénivelé positif estimé à partir du relief des tronçons parcourus (m).
    pub fn ascent_estimate(&self, g: &Graph) -> f64 {
        self.segs
            .iter()
            .map(|&(s, fwd, m)| {
                let (up, _, _) = g.climb(s, fwd);
                let len = g.seg_len[s as usize].max(1.0);
                (up * (m / len).min(1.0)) as f64
            })
            .sum()
    }
}

/// Index spatial en grille : pour chaque case (~400 m de côté), la liste des tronçons qui
/// la traversent. Il se construit en quelques secondes même pour toute la France et prend
/// peu de mémoire (un arbre avec une entrée par portion de route demandait des minutes
/// et plusieurs Go au chargement d'un grand réseau).
struct SegGrid {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: i64,
    ny: i64,
    /// Tronçons de la case c : `segs[start[c] .. start[c + 1]]`.
    start: Vec<u32>,
    segs: Vec<u32>,
}

impl SegGrid {
    fn build(g: &Graph, kx: f64) -> SegGrid {
        let b = g.bbox();
        let lo = proj(kx, [b[0], b[1]]);
        let hi = proj(kx, [b[2], b[3]]);
        let (w, h) = ((hi[0] - lo[0]).max(1.0), (hi[1] - lo[1]).max(1.0));
        // Cases de 400 m, agrandies si besoin pour rester sous ~16 millions de cases
        let mut cell = 400.0;
        while (w / cell).ceil() * (h / cell).ceil() > 16.0e6 {
            cell *= 1.25;
        }
        let nx = (w / cell).ceil() as i64 + 1;
        let ny = (h / cell).ceil() as i64 + 1;
        let mut grid = SegGrid { x0: lo[0], y0: lo[1], cell, nx, ny, start: Vec::new(), segs: Vec::new() };

        // Cases traversées par un tronçon (boîte de chaque portion), sans doublon
        let mut buf: Vec<u32> = Vec::new();
        let cells_of = |s: u32, buf: &mut Vec<u32>| {
            buf.clear();
            let pts = g.seg_points(s);
            for i in 0..pts.len().saturating_sub(1) {
                let (a, b) = (proj(kx, pts[i]), proj(kx, pts[i + 1]));
                let (ax, ay) = grid.cell_of(a);
                let (bx, by) = grid.cell_of(b);
                for y in ay.min(by)..=ay.max(by) {
                    for x in ax.min(bx)..=ax.max(bx) {
                        buf.push((y * nx + x) as u32);
                    }
                }
            }
            buf.sort_unstable();
            buf.dedup();
        };
        let n_cells = (nx * ny) as usize;
        let mut count = vec![0u32; n_cells + 1];
        for s in 0..g.seg_count() as u32 {
            cells_of(s, &mut buf);
            for &c in &buf {
                count[c as usize + 1] += 1;
            }
        }
        for c in 0..n_cells {
            count[c + 1] += count[c];
        }
        let mut fill = count.clone();
        let mut segs = vec![0u32; count[n_cells] as usize];
        for s in 0..g.seg_count() as u32 {
            cells_of(s, &mut buf);
            for &c in &buf {
                segs[fill[c as usize] as usize] = s;
                fill[c as usize] += 1;
            }
        }
        grid.start = count;
        grid.segs = segs;
        grid
    }

    #[inline]
    fn cell_of(&self, p: [f64; 2]) -> (i64, i64) {
        let x = ((p[0] - self.x0) / self.cell).floor() as i64;
        let y = ((p[1] - self.y0) / self.cell).floor() as i64;
        (x.clamp(0, self.nx - 1), y.clamp(0, self.ny - 1))
    }

    #[inline]
    fn cell_segs(&self, x: i64, y: i64) -> &[u32] {
        if x < 0 || y < 0 || x >= self.nx || y >= self.ny {
            return &[];
        }
        let c = (y * self.nx + x) as usize;
        &self.segs[self.start[c] as usize..self.start[c + 1] as usize]
    }
}

/// Graphe + index spatial : tout ce qu'il faut pour calculer des itinéraires.
pub struct Router {
    pub graph: Graph,
    grid: SegGrid,
    /// Facteur de projection locale (cos de la latitude moyenne).
    kx: f64,
    /// Tronçons où l'on peut accrocher un point : goudron seul (Route, Triathlon)…
    snappable_paved: Vec<bool>,
    /// … ou tous revêtements (Gravel).
    snappable_all: Vec<bool>,
}

const M_PER_DEG_LAT: f64 = 110_574.0;
const M_PER_DEG_LON_EQ: f64 = 111_320.0;

impl Router {
    pub fn new(graph: Graph) -> Router {
        let b = graph.bbox();
        let lat0 = ((b[1] + b[3]) / 2.0) as f64;
        let kx = lat0.to_radians().cos();
        // Index et tronçons accrochables : calculés en parallèle (démarrage plus rapide)
        let (grid, snappable_paved, snappable_all) = std::thread::scope(|sc| {
            let paved = sc.spawn(|| snappable_segments(&graph, true));
            let all = sc.spawn(|| snappable_segments(&graph, false));
            let grid = SegGrid::build(&graph, kx);
            (grid, paved.join().expect("calcul interrompu"), all.join().expect("calcul interrompu"))
        });
        Router { graph, grid, kx, snappable_paved, snappable_all }
    }

    /// Accroche un point (lon, lat) au tronçon autorisé le plus proche.
    pub fn snap(&self, lonlat: [f64; 2], opts: &RouteOptions) -> Result<Snap, RouteError> {
        self.snap_within(lonlat, opts, MAX_SNAP_M)
    }

    /// Comme `snap`, avec une distance maximale choisie (m).
    pub fn snap_within(&self, lonlat: [f64; 2], opts: &RouteOptions, max_m: f64) -> Result<Snap, RouteError> {
        let q = proj(self.kx, [lonlat[0] as f32, lonlat[1] as f32]);
        let g = &self.graph;
        let snappable = if opts.profile.paved_only() { &self.snappable_paved } else { &self.snappable_all };
        let limit = max_m * 1.2;
        let grid = &self.grid;
        let (cx, cy) = grid.cell_of(q);
        // Portion autorisée la plus proche : (distance², tronçon, portion, position sur la portion)
        let mut best: Option<(f64, u32, usize, f64)> = None;
        let max_ring = (limit / grid.cell).ceil() as i64 + 1;
        for r in 0..=max_ring {
            for y in cy - r..=cy + r {
                for x in cx - r..=cx + r {
                    if (x - cx).abs() != r && (y - cy).abs() != r {
                        continue; // case déjà vue (anneau intérieur)
                    }
                    for &s in grid.cell_segs(x, y) {
                        if !snappable[s as usize] {
                            continue;
                        }
                        let pts = g.seg_points(s);
                        for i in 0..pts.len().saturating_sub(1) {
                            let (a, b) = (proj(self.kx, pts[i]), proj(self.kx, pts[i + 1]));
                            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                            let l2 = dx * dx + dy * dy;
                            let t = if l2 > 0.0 { (((q[0] - a[0]) * dx + (q[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                            let (px, py) = (a[0] + dx * t - q[0], a[1] + dy * t - q[1]);
                            let d2 = px * px + py * py;
                            if best.is_none_or(|b| d2 < b.0) {
                                best = Some((d2, s, i, t));
                            }
                        }
                    }
                }
            }
            // Tout ce qui reste est à plus de r cases du clic : inutile d'aller plus loin
            if best.is_some_and(|b| b.0.sqrt() <= r as f64 * grid.cell) {
                break;
            }
        }
        let Some((d2, s, i, t)) = best else { return Err(RouteError::NoRoadNearby(max_m)) };
        if d2 > limit * limit {
            return Err(RouteError::NoRoadNearby(max_m));
        }
        let pts = g.seg_points(s);
        let (pa, pb) = (to64(pts[i]), to64(pts[i + 1]));
        let point = [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t];
        let before: f64 = (0..i).map(|k| haversine_m(to64(pts[k]), to64(pts[k + 1]))).sum();
        let offset = before + haversine_m(pa, pb) * t;
        let distance_m = haversine_m(lonlat, point);
        if distance_m > max_m {
            return Err(RouteError::NoRoadNearby(max_m));
        }
        Ok(Snap { seg: s, offset, point, distance_m })
    }

    /// Meilleur itinéraire (au sens des réglages) entre deux points accrochés.
    pub fn route(&self, a: &Snap, b: &Snap, opts: &RouteOptions) -> Result<Leg, RouteError> {
        self.route_with(a, b, opts, None)
    }

    /// Comme `route`, avec un multiplicateur de coût sur certains tronçons
    /// (ex. ceux déjà empruntés par une boucle, pour éviter les allers-retours).
    pub fn route_with(
        &self,
        a: &Snap,
        b: &Snap,
        opts: &RouteOptions,
        penalty: Option<&HashMap<u32, f32>>,
    ) -> Result<Leg, RouteError> {
        let g = &self.graph;
        let n = g.node_count();
        const START: u32 = u32::MAX - 1;
        const NONE: u32 = u32::MAX;

        let cost = Cost::new(opts, g.has_elevation);
        let (sa, sb) = (a.seg, b.seg);
        let (la, lb) = (seg_geom_len(g, sa), seg_geom_len(g, sb));
        // Coût d'une portion de tronçon : au prorata de sa longueur
        let part = |s: u32, forward: bool, len: f64, frac_len: f64| -> Option<f64> {
            let full = cost.edge(g, s, forward)?;
            Some(if len > 0.0 { full * (frac_len / len).clamp(0.0, 1.0) } else { 0.0 })
        };
        if !cost.allowed(g, sa) || !cost.allowed(g, sb) {
            return Err(RouteError::NoPath);
        }

        // Meilleure solution connue : (coût, sommet d'arrivée sur le tronçon final ou NONE pour le trajet direct).
        let mut best = (f64::INFINITY, NONE);

        // Cas où départ et arrivée sont sur le même tronçon.
        if sa == sb {
            let forward = b.offset >= a.offset;
            if let Some(c) = part(sa, forward, la, (b.offset - a.offset).abs()) {
                best = (c, NONE);
            }
        }

        let min_factor = cost.min_factor;
        let goal = b.point;
        let h = |v: u32| {
            let p = g.node_pos[v as usize];
            haversine_m(to64(p), goal) * min_factor
        };
        let mut dist = vec![f64::INFINITY; n];
        let mut parent = vec![NONE; n];
        let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
        let key = |c: f64| (c.max(0.0) * 1000.0) as u64; // millimètres : ordre exact et entier

        let push_start = |v: u32, c: f64, dist: &mut Vec<f64>, parent: &mut Vec<u32>, heap: &mut BinaryHeap<_>| {
            if c < dist[v as usize] {
                dist[v as usize] = c;
                parent[v as usize] = START;
                heap.push(Reverse((key(c + h(v)), v)));
            }
        };
        if let Some(c) = part(sa, false, la, a.offset) {
            push_start(g.seg_from[sa as usize], c, &mut dist, &mut parent, &mut heap);
        }
        if let Some(c) = part(sa, true, la, la - a.offset) {
            push_start(g.seg_to[sa as usize], c, &mut dist, &mut parent, &mut heap);
        }

        let mut settled = 0usize;
        while let Some(Reverse((k, v))) = heap.pop() {
            let d = dist[v as usize];
            if k > key(d + h(v)) {
                continue; // entrée périmée
            }
            if (k as f64) / 1000.0 >= best.0 {
                break;
            }
            settled += 1;
            if settled > MAX_SETTLED {
                return Err(RouteError::TooFar);
            }
            // Arrivée possible depuis ce sommet ?
            if v == g.seg_from[sb as usize] {
                if let Some(c) = part(sb, true, lb, b.offset) {
                    if d + c < best.0 {
                        best = (d + c, v);
                    }
                }
            }
            if v == g.seg_to[sb as usize] {
                if let Some(c) = part(sb, false, lb, lb - b.offset) {
                    if d + c < best.0 {
                        best = (d + c, v);
                    }
                }
            }
            for &s in g.incident(v) {
                let (w, forward) = g.other_end(s, v);
                if w == v {
                    continue;
                }
                let Some(mut c) = cost.edge(g, s, forward) else { continue };
                if let Some(&k) = penalty.and_then(|p| p.get(&s)) {
                    c *= k as f64;
                }
                let nd = d + c + cost.node(g, w);
                if nd < dist[w as usize] {
                    dist[w as usize] = nd;
                    parent[w as usize] = s;
                    heap.push(Reverse((key(nd + h(w)), w)));
                }
            }
        }

        if !best.0.is_finite() {
            return Err(RouteError::NoPath);
        }

        let mut leg = LegBuilder::default();
        if best.1 == NONE {
            leg.push(g, sa, b.offset >= a.offset, subpath(g, sa, a.offset, b.offset));
            return Ok(leg.finish());
        }

        // Remontée du chemin : liste des (tronçon, sommet de départ).
        let mut chain = Vec::new();
        let mut v = best.1;
        while parent[v as usize] != START {
            let s = parent[v as usize];
            let (u, _) = g.other_end(s, v);
            chain.push((s, u));
            v = u;
        }
        chain.reverse();
        let start_v = v;

        // Début : du point de départ jusqu'au premier sommet.
        let exit_to_end = start_v == g.seg_to[sa as usize] && (g.seg_from[sa as usize] != start_v || g.allows(sa, true));
        leg.push(g, sa, exit_to_end, subpath(g, sa, a.offset, if exit_to_end { la } else { 0.0 }));
        // Tronçons complets
        for &(s, u) in &chain {
            let ls = seg_geom_len(g, s);
            let fwd = g.seg_from[s as usize] == u;
            let pts = if fwd { subpath(g, s, 0.0, ls) } else { subpath(g, s, ls, 0.0) };
            leg.push(g, s, fwd, pts);
        }
        // Fin : du dernier sommet jusqu'au point d'arrivée.
        let end_v = best.1;
        let enter_from_start = end_v == g.seg_from[sb as usize] && (g.seg_to[sb as usize] != end_v || g.allows(sb, true));
        leg.push(g, sb, enter_from_start, subpath(g, sb, if enter_from_start { 0.0 } else { lb }, b.offset));
        Ok(leg.finish())
    }
}

/// Tronçons autorisés appartenant à une composante connexe (routes autorisées seulement) assez grande.
fn snappable_segments(g: &Graph, paved_only: bool) -> Vec<bool> {
    let allowed = |s: u32| {
        let s = s as usize;
        g.seg_dir[s] != 0 && (!paved_only || g.seg_surface[s] != Surface::Unpaved)
    };
    let n = g.node_count();
    let mut comp = vec![u32::MAX; n];
    let mut sizes: Vec<usize> = Vec::new();
    let mut stack = Vec::new();
    for root in 0..n as u32 {
        if comp[root as usize] != u32::MAX {
            continue;
        }
        let id = sizes.len() as u32;
        comp[root as usize] = id;
        stack.push(root);
        let mut size = 0;
        while let Some(v) = stack.pop() {
            size += 1;
            for &s in g.incident(v) {
                if !allowed(s) {
                    continue;
                }
                let (w, _) = g.other_end(s, v);
                if comp[w as usize] == u32::MAX {
                    comp[w as usize] = id;
                    stack.push(w);
                }
            }
        }
        sizes.push(size);
    }
    // Sur un tout petit réseau (tests), le seuil ne doit pas tout exclure.
    let threshold = MIN_SNAP_COMPONENT.min(sizes.iter().copied().max().unwrap_or(0));
    (0..g.seg_count() as u32)
        .map(|s| allowed(s) && sizes[comp[g.seg_from[s as usize] as usize] as usize] >= threshold)
        .collect()
}

#[derive(Default)]
struct LegBuilder {
    coords: Vec<[f64; 2]>,
    surface: Vec<Surface>,
    class: Vec<RoadClass>,
    segs: Vec<(u32, bool, f32)>,
}

impl LegBuilder {
    fn push(&mut self, g: &Graph, s: u32, forward: bool, pts: Vec<[f64; 2]>) {
        let m: f64 = pts.windows(2).map(|w| haversine_m(w[0], w[1])).sum();
        if m > 0.0 {
            self.segs.push((s, forward, m as f32));
        }
        for p in pts {
            if self.coords.last() == Some(&p) {
                continue;
            }
            if !self.coords.is_empty() {
                self.surface.push(g.seg_surface[s as usize]);
                self.class.push(g.seg_class[s as usize]);
            }
            self.coords.push(p);
        }
    }
    fn finish(self) -> Leg {
        let length_m = self.coords.windows(2).map(|w| haversine_m(w[0], w[1])).sum();
        Leg { coords: self.coords, surface: self.surface, class: self.class, length_m, segs: self.segs }
    }
}

#[inline]
fn to64(p: [f32; 2]) -> [f64; 2] {
    [p[0] as f64, p[1] as f64]
}

#[inline]
fn proj(kx: f64, p: [f32; 2]) -> [f64; 2] {
    [p[0] as f64 * kx * M_PER_DEG_LON_EQ, p[1] as f64 * M_PER_DEG_LAT]
}

/// Longueur de la géométrie d'un tronçon, calculée comme les offsets d'accroche.
fn seg_geom_len(g: &Graph, s: u32) -> f64 {
    g.seg_points(s).windows(2).map(|w| haversine_m(to64(w[0]), to64(w[1]))).sum()
}

/// Points de la géométrie du tronçon entre deux distances (sens from → to si `a < b`, inverse sinon).
fn subpath(g: &Graph, s: u32, a: f64, b: f64) -> Vec<[f64; 2]> {
    let pts: Vec<[f64; 2]> = g.seg_points(s).iter().map(|&p| to64(p)).collect();
    let mut cum = Vec::with_capacity(pts.len());
    let mut acc = 0.0;
    cum.push(0.0);
    for w in pts.windows(2) {
        acc += haversine_m(w[0], w[1]);
        cum.push(acc);
    }
    let at = |d: f64| -> [f64; 2] {
        let d = d.clamp(0.0, acc);
        let i = cum.partition_point(|&c| c < d).clamp(1, pts.len() - 1);
        let span = cum[i] - cum[i - 1];
        let t = if span > 0.0 { (d - cum[i - 1]) / span } else { 0.0 };
        [pts[i - 1][0] + (pts[i][0] - pts[i - 1][0]) * t, pts[i - 1][1] + (pts[i][1] - pts[i - 1][1]) * t]
    };
    let mut out = vec![at(a)];
    if a <= b {
        out.extend((0..pts.len()).filter(|&i| cum[i] > a && cum[i] < b).map(|i| pts[i]));
    } else {
        out.extend((0..pts.len()).rev().filter(|&i| cum[i] < a && cum[i] > b).map(|i| pts[i]));
    }
    out.push(at(b));
    out
}

//! Tests du moteur sur un petit réseau fictif (≈ 1 km de côté).
//!
//!   A ──────── B ──────── C        y = 0,01
//!   │                     │
//!   │      (gravier)      │
//!   D ───────────────────  E       y = 0
//!   │                     │
//!   F ──────── G ──────── H        y = -0,01   (G → H en sens unique vers l'ouest)
//!
//! La voie D–E est en gravier : interdite en profil Route.
//! Les voies verticales et A-B-C sont goudronnées.

use goride_core::geo::haversine_m;
use goride_core::graph::{build_graph, Graph, RawWay};
use goride_core::osm_tags::{RoadClass, Surface, WayInfo};
use goride_core::graph::elevation::compute_elevation;
use goride_core::router::{Profile, RouteError, RouteOptions, Router};

fn road() -> RouteOptions {
    RouteOptions::default()
}

const PTS: &[(i64, [f64; 2])] = &[
    (1, [0.00, 0.01]),  // A
    (2, [0.01, 0.01]),  // B
    (3, [0.02, 0.01]),  // C
    (4, [0.00, 0.00]),  // D
    (5, [0.02, 0.00]),  // E
    (6, [0.00, -0.01]), // F
    (7, [0.01, -0.01]), // G
    (8, [0.02, -0.01]), // H
    (9, [0.01, 0.00]),  // milieu de D–E (point de forme)
];

fn way(nodes: &[i64], surface: Surface, forward: bool, backward: bool) -> RawWay {
    RawWay {
        nodes: nodes.to_vec(),
        info: WayInfo { class: RoadClass::Minor, surface, forward, backward, structure: false, roundabout: false, rough: false },
    }
}

fn graph() -> Graph {
    use Surface::*;
    let ways = vec![
        way(&[1, 2, 3], Paved, true, true),
        way(&[4, 9, 5], Unpaved, true, true),
        way(&[1, 4, 6], Paved, true, true),
        way(&[3, 5, 8], Paved, true, true),
        way(&[6, 7], Paved, true, true),
        way(&[8, 7], Paved, true, false), // H → G seulement
    ];
    // Le petit réseau est volontairement gardé (sinon supprimé comme « composante trop petite ») :
    // on le duplique en grille dense éloignée pour dépasser le seuil.
    let mut all = ways;
    let mut coords: Vec<(i64, [f64; 2])> = PTS.to_vec();
    for k in 0..60 {
        let id = 1000 + k;
        coords.push((id, [10.0 + k as f64 * 0.001, 10.0]));
        if k > 0 {
            all.push(way(&[id - 1, id], Paved, true, true));
        }
    }
    build_graph(&all, |id| coords.iter().find(|c| c.0 == id).map(|c| c.1))
}

#[test]
fn small_components_are_removed() {
    // Le réseau principal (8 sommets) est sous le seuil : il disparaît, la grande chaîne reste.
    let g = graph();
    assert!(g.node_pos.iter().all(|p| p[0] >= 9.0));
}

/// Même réseau, mais sans suppression (le seuil ne s'applique qu'aux petites composantes isolées) :
/// on relie le petit réseau à la grande chaîne pour qu'il soit conservé.
fn connected_graph() -> Graph {
    use Surface::*;
    let mut ways = vec![
        way(&[1, 2, 3], Paved, true, true),
        way(&[4, 9, 5], Unpaved, true, true),
        way(&[1, 4, 6], Paved, true, true),
        way(&[3, 5, 8], Paved, true, true),
        way(&[6, 7], Paved, true, true),
        way(&[8, 7], Paved, true, false),
    ];
    let mut coords: Vec<(i64, [f64; 2])> = PTS.to_vec();
    let mut prev = 3; // part de C vers l'est
    for k in 0..60 {
        let id = 1000 + k;
        coords.push((id, [0.03 + k as f64 * 0.001, 0.01]));
        ways.push(way(&[prev, id], Paved, true, true));
        prev = id;
    }
    build_graph(&ways, |id| coords.iter().find(|c| c.0 == id).map(|c| c.1))
}

#[test]
fn avoids_gravel_shortcut() {
    let r = Router::new(connected_graph());
    // De D vers E : le chemin direct (2,2 km) est en gravier → détour par A-B-C (≈ 4,4 km)
    let a = r.snap([0.0005, 0.0], &road()).unwrap();
    let b = r.snap([0.0195, 0.0], &road()).unwrap();
    // Les clics proches de D–E s'accrochent sur les verticales goudronnées, pas sur le gravier
    assert_ne!(r.graph.seg_surface[a.seg as usize], Surface::Unpaved);
    let leg = r.route(&a, &b, &road()).unwrap();
    assert!(leg.surface.iter().all(|s| *s != Surface::Unpaved));
    assert!(leg.length_m > 4_000.0, "{}", leg.length_m);
    // Chemin passant par le nord ou le sud, jamais par y ≈ 0 au milieu
    assert!(leg.coords.iter().all(|c| !(c[1].abs() < 0.001 && c[0] > 0.002 && c[0] < 0.018)));
}

#[test]
fn respects_oneway() {
    let r = Router::new(connected_graph());
    let g = r.snap([0.0015, -0.01], &road()).unwrap(); // près de G, côté F
    let h = r.snap([0.0185, -0.01], &road()).unwrap(); // près de H
    // H → G : autorisé, direct par le bas (≈ 1,9 km)
    let back = r.route(&h, &g, &road()).unwrap();
    assert!(back.length_m < 2_100.0, "{}", back.length_m);
    // G → H : sens interdit, il faut remonter par le nord (≈ 7,5 km)
    let fwd = r.route(&g, &h, &road()).unwrap();
    assert!(fwd.length_m > 6_000.0, "{}", fwd.length_m);
}

#[test]
fn same_segment_and_geometry_continuity() {
    let r = Router::new(connected_graph());
    let a = r.snap([0.002, 0.0101], &road()).unwrap();
    let b = r.snap([0.008, 0.0101], &road()).unwrap();
    assert_eq!(a.seg, b.seg);
    let leg = r.route(&a, &b, &road()).unwrap();
    let expected = haversine_m(a.point, b.point);
    assert!((leg.length_m - expected).abs() < 1.0);
    assert_eq!(leg.coords.first(), Some(&a.point));
    assert_eq!(leg.coords.last(), Some(&b.point));
    assert_eq!(leg.surface.len(), leg.coords.len() - 1);

    // Trajet plus long : départ et arrivée exactement aux points accrochés, sans saut
    let c = r.snap([0.0201, -0.005], &road()).unwrap();
    let leg = r.route(&a, &c, &road()).unwrap();
    assert_eq!(leg.coords.first(), Some(&a.point));
    assert_eq!(leg.coords.last(), Some(&c.point));
    assert!(leg.coords.windows(2).all(|w| haversine_m(w[0], w[1]) < 1_200.0));
    let sum: f64 = leg.coords.windows(2).map(|w| haversine_m(w[0], w[1])).sum();
    assert!((sum - leg.length_m).abs() < 0.01);
}

#[test]
fn reverse_on_same_segment() {
    let r = Router::new(connected_graph());
    let a = r.snap([0.008, 0.0101], &road()).unwrap();
    let b = r.snap([0.002, 0.0101], &road()).unwrap();
    let leg = r.route(&a, &b, &road()).unwrap();
    assert!((leg.length_m - haversine_m(a.point, b.point)).abs() < 1.0);
}

#[test]
fn far_click_is_rejected() {
    let r = Router::new(connected_graph());
    assert!(matches!(r.snap([0.01, 0.5], &road()), Err(RouteError::NoRoadNearby(_))));
}

#[test]
fn graph_save_and_load() {
    let g = connected_graph();
    let dir = std::env::temp_dir().join(format!("goride-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("t.graph");
    g.save(&p).unwrap();
    let back = Graph::load(&p).unwrap();
    assert_eq!(back.seg_count(), g.seg_count());
    assert_eq!(back.geom, g.geom);
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Phase 2 : réglages ------------------------------------------------------

/// Ajoute une longue chaîne raccordée au nœud `from` (le réseau dépasse ainsi les seuils de taille).
fn tail(ways: &mut Vec<RawWay>, coords: &mut Vec<(i64, [f64; 2])>, from: i64, at: [f64; 2]) {
    let mut prev = from;
    for k in 0..400 {
        let id = 50_000 + k;
        coords.push((id, [at[0] + 0.0005 * (k + 1) as f64, at[1] + 0.02]));
        ways.push(way(&[prev, id], Surface::Paved, true, true));
        prev = id;
    }
}

#[test]
fn gravel_takes_the_dirt_shortcut() {
    let r = Router::new(connected_graph());
    let gravel = RouteOptions { profile: Profile::Gravel, ..road() };
    let a = r.snap([0.0005, 0.0], &gravel).unwrap();
    let b = r.snap([0.0195, 0.0], &gravel).unwrap();
    let leg = r.route(&a, &b, &gravel).unwrap();
    assert!(leg.length_m < 2_500.0, "{}", leg.length_m);
    assert!(leg.surface.contains(&Surface::Unpaved));
}

/// Bruit déterministe de ± `amp` m, comme celui du modèle de terrain (pixels de 25 m).
fn noise(p: [f32; 2], amp: f64) -> f64 {
    let h = ((p[0] as f64 * 91_731.0).sin() * 43_758.545 + (p[1] as f64 * 57_221.0).cos() * 12_345.678).fract();
    (h * 2.0 - 1.0) * amp
}

/// Deux routes de P à Q : au nord par une colline de 120 m (plus courte), au sud à plat (+ 15 %).
fn hills_graph() -> Graph {
    hills_graph_with(120.0, 0.0)
}

/// Variante : colline de `hill` m, terrain bruité de ± `amp` m, routes découpées en tronçons courts
/// (des rues transversales tous les 150 m environ, comme en vrai).
fn hills_graph_with(hill: f64, amp: f64) -> Graph {
    let mut coords: Vec<(i64, [f64; 2])> = vec![
        (1, [0.0, 0.0]),     // P
        (2, [0.04, 0.0]),    // Q
        (3, [0.01, 0.006]),  // nord
        (4, [0.02, 0.008]),
        (5, [0.03, 0.006]),
        (6, [0.01, -0.010]), // sud
        (7, [0.02, -0.011]),
        (8, [0.03, -0.010]),
    ];
    // Chaque route est découpée en ~30 tronçons par des impasses transversales
    let mut ways = Vec::new();
    for (k, chain) in [[1i64, 3, 4, 5, 2], [1, 6, 7, 8, 2]].iter().enumerate() {
        let mut nodes = vec![chain[0]];
        for w in chain.windows(2) {
            let (a, b) = (coords.iter().find(|c| c.0 == w[0]).unwrap().1, coords.iter().find(|c| c.0 == w[1]).unwrap().1);
            for i in 1..8 {
                let t = i as f64 / 8.0;
                let id = 10_000 + k as i64 * 1_000 + w[0] * 10 + i;
                let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                coords.push((id, p));
                coords.push((id + 500, [p[0], p[1] + if k == 0 { 0.0005 } else { -0.0005 }]));
                ways.push(way(&[id, id + 500], Surface::Paved, true, true));
                nodes.push(id);
            }
            nodes.push(w[1]);
        }
        ways.push(way(&nodes, Surface::Paved, true, true));
    }
    tail(&mut ways, &mut coords, 1, [0.0, 0.0]);
    let mut g = build_graph(&ways, |id| coords.iter().find(|c| c.0 == id).map(|c| c.1));
    // Colline centrée sur (0,02 ; 0,008)
    compute_elevation(&mut g, |pts| {
        pts.iter()
            .map(|p| {
                let (dx, dy) = ((p[0] as f64 - 0.02) / 0.012, (p[1] as f64 - 0.008) / 0.006);
                (20.0 + hill * (-(dx * dx + dy * dy)).exp() + noise(*p, amp)) as f32
            })
            .collect()
    });
    assert!(g.has_elevation);
    g
}

fn goes_north(leg: &goride_core::router::Leg) -> bool {
    leg.coords.iter().any(|c| c[1] > 0.004)
}

#[test]
fn hills_slider() {
    let r = Router::new(hills_graph());
    let a = r.snap([0.0, 0.0001], &road()).unwrap();
    let b = r.snap([0.04, 0.0001], &road()).unwrap();
    let with = |o: RouteOptions| r.route(&a, &b, &o).unwrap();

    assert!(goes_north(&with(road())), "neutre : la route la plus courte, par la colline");
    assert!(!goes_north(&with(RouteOptions { hills: -1.0, ..road() })), "le plus plat : par le sud");
    assert!(goes_north(&with(RouteOptions { hills: 1.0, ..road() })), "les bosses : par la colline");
}

#[test]
fn max_grade_avoids_steep_road() {
    let r = Router::new(hills_graph());
    let g = &r.graph;
    let steepest = (0..g.seg_count()).map(|s| g.seg_grade_fwd[s].max(g.seg_grade_bwd[s])).max().unwrap();
    assert!(steepest >= 6, "pente max {steepest} %");
    let a = r.snap([0.0, 0.0001], &road()).unwrap();
    let b = r.snap([0.04, 0.0001], &road()).unwrap();
    let limited = RouteOptions { max_grade: (steepest - 2) as f32, ..road() };
    assert!(!goes_north(&r.route(&a, &b, &limited).unwrap()));
}

#[test]
fn triathlon_avoids_intersections() {
    // Route directe hachée de 12 carrefours vs. route continue 2 % plus longue
    let mut coords: Vec<(i64, [f64; 2])> = vec![(1, [0.0, 0.0]), (2, [0.05, 0.0]), (3, [0.025, 0.0035])];
    let mut direct = vec![1];
    let mut ways = Vec::new();
    for k in 1..=12 {
        let (id, stub) = (100 + k, 200 + k);
        let x = 0.05 * k as f64 / 13.0;
        coords.push((id, [x, 0.0]));
        coords.push((stub, [x, -0.002]));
        direct.push(id);
        ways.push(way(&[id, stub], Surface::Paved, true, true)); // rue transversale
    }
    direct.push(2);
    ways.push(way(&direct, Surface::Paved, true, true));
    ways.push(way(&[1, 3, 2], Surface::Paved, true, true));
    tail(&mut ways, &mut coords, 1, [0.0, 0.0]);
    let r = Router::new(build_graph(&ways, |id| coords.iter().find(|c| c.0 == id).map(|c| c.1)));

    let tri = RouteOptions { profile: Profile::Triathlon, ..road() };
    let a = r.snap([0.0, 0.0001], &road()).unwrap();
    let b = r.snap([0.05, 0.0001], &road()).unwrap();
    let road_leg = r.route(&a, &b, &road()).unwrap();
    let tri_leg = r.route(&a, &b, &tri).unwrap();
    assert!(road_leg.coords.iter().all(|c| c[1] < 0.001), "Route : la plus courte");
    assert!(tri_leg.coords.iter().any(|c| c[1] > 0.003), "Triathlon : la route sans carrefours");
}

/// Cas réel signalé : en Bretagne (relief doux, terrain bruité), « le plus plat »
/// donnait plus de D+ que « les bosses ». Le bruit ne doit pas compter comme du relief.
#[test]
fn hills_slider_with_noisy_terrain() {
    let r = Router::new(hills_graph_with(35.0, 1.5));
    let g = &r.graph;
    // Sur la route plate du sud (impasses transversales exclues), le bruit seul ne doit presque rien coûter
    let through = |v: u32| g.incident(v).len() >= 2;
    let south_up: f32 = (0..g.seg_count())
        .filter(|&s| {
            let (a, b) = (g.seg_from[s], g.seg_to[s]);
            g.node_pos[a as usize][1] < -0.005 && g.node_pos[b as usize][1] < -0.005 && through(a) && through(b)
        })
        .map(|s| g.seg_up[s] + g.seg_down[s])
        .sum();
    assert!(south_up < 20.0, "relief fantôme sur la route plate : {south_up:.1} m");

    let a = r.snap([0.0, 0.0001], &road()).unwrap();
    let b = r.snap([0.04, 0.0001], &road()).unwrap();
    let with = |o: RouteOptions| r.route(&a, &b, &o).unwrap();
    assert!(!goes_north(&with(RouteOptions { hills: -1.0, ..road() })), "le plus plat : par le sud");
    assert!(goes_north(&with(RouteOptions { hills: 1.0, ..road() })), "les bosses : par la colline");
}

/// Deux régions voisines qui partagent des routes frontalières : une fois assemblées,
/// on passe de l'une à l'autre, et les routes communes ne comptent qu'une fois.
#[test]
fn merged_regions_are_connected() {
    use goride_core::graph::merge_graphs;
    use goride_core::osm_tags::classify;
    let info = || classify(|k| (k == "highway").then_some("tertiary")).unwrap();
    let pos = |id: i64| Some([-1.70 + id as f64 * 0.002, 48.10 + (id % 3) as f64 * 0.001]);
    // Une route en 98 bouts : l'ouest a les bouts 1→60, l'est 40→99 (40→60 dans les deux)
    let ways = |a: i64, b: i64| -> Vec<RawWay> { (a..b).map(|i| RawWay { nodes: vec![i, i + 1], info: info() }).collect() };
    let west = build_graph(&ways(1, 60), pos);
    let east = build_graph(&ways(40, 99), pos);
    assert_eq!((west.seg_count(), east.seg_count()), (59, 59));
    let merged = merge_graphs(vec![west, east]);
    assert_eq!(merged.seg_count(), 98, "les bouts frontaliers ne sont gardés qu'une fois");
    assert_eq!(merged.node_count(), 99);
    // D'un bout à l'autre : un seul réseau continu
    let r = Router::new(merged);
    let a = r.snap([-1.70 + 2.0 * 0.002, 48.10 + 0.002], &road()).unwrap();
    let b = r.snap([-1.70 + 98.0 * 0.002, 48.10 + 0.002], &road()).unwrap();
    let leg = r.route(&a, &b, &road()).expect("passage d'une région à l'autre");
    assert!(leg.length_m > 10_000.0);
}

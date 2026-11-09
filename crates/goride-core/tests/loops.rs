//! Boucles automatiques sur un quadrillage de routes fictif (≈ 9 km de côté).

use goride_core::graph::{build_graph, Graph, RawWay};
use goride_core::loops::{overlap, propose_loops, LoopRequest};
use goride_core::osm_tags::{RoadClass, Surface, WayInfo};
use goride_core::router::{RouteOptions, Router};

const N: i64 = 41;
const STEP: f64 = 0.002; // ≈ 222 m

fn grid() -> Graph {
    let id = |i: i64, j: i64| i * 1000 + j;
    let info = WayInfo {
        class: RoadClass::Minor,
        surface: Surface::Paved,
        forward: true,
        backward: true,
        structure: false,
        roundabout: false,
        rough: false,
    };
    let mut ways = Vec::new();
    for i in 0..N {
        ways.push(RawWay { nodes: (0..N).map(|j| id(i, j)).collect(), info });
        ways.push(RawWay { nodes: (0..N).map(|j| id(j, i)).collect(), info });
    }
    build_graph(&ways, |n| {
        let (i, j) = (n / 1000, n % 1000);
        (i < N && j < N).then(|| [j as f64 * STEP, i as f64 * STEP])
    })
}

fn request(seed: u64) -> LoopRequest {
    LoopRequest {
        start: [20.0 * STEP, 20.0 * STEP],
        distance_m: 12_000.0,
        tolerance: 0.05,
        count: 4,
        seed,
        heading_deg: None,
    }
}

#[test]
fn proposes_varied_loops_at_the_right_distance() {
    let r = Router::new(grid());
    let opts = RouteOptions::default();
    let loops = propose_loops(&r, &request(1), &opts, &[]).unwrap();

    assert!(loops.len() >= 3, "{} boucles seulement", loops.len());
    for l in &loops {
        let err = (l.length_m - 12_000.0).abs() / 12_000.0;
        assert!(err <= 0.05, "boucle de {:.0} m", l.length_m);
        assert!(l.retrace < 0.1, "allers-retours : {:.0} %", l.retrace * 100.0);
        // Boucle fermée : on revient au départ
        assert_eq!(l.waypoints.first(), l.waypoints.last());
        let first = l.legs.first().unwrap().coords[0];
        let last = *l.legs.last().unwrap().coords.last().unwrap();
        assert_eq!(first, last);
    }
    for (i, a) in loops.iter().enumerate() {
        for b in &loops[i + 1..] {
            assert!(overlap(&a.footprint, &b.footprint) < 0.6, "deux boucles trop semblables");
        }
    }
}

#[test]
fn other_proposals_are_new() {
    let r = Router::new(grid());
    let opts = RouteOptions::default();
    let first = propose_loops(&r, &request(1), &opts, &[]).unwrap();
    let shown: Vec<_> = first.iter().map(|l| l.footprint.clone()).collect();
    let second = propose_loops(&r, &request(2), &opts, &shown).unwrap();
    for l in &second {
        assert!(first.iter().all(|f| f.id != l.id), "boucle déjà proposée");
        assert!(shown.iter().all(|s| overlap(&l.footprint, s) < 0.6));
    }
}

#[test]
fn preferred_direction() {
    let r = Router::new(grid());
    let opts = RouteOptions::default();
    let start = request(3).start;
    // Vers l'est : la boucle s'étend surtout à l'est du départ
    let req = LoopRequest { heading_deg: Some(90.0), ..request(3) };
    for l in propose_loops(&r, &req, &opts, &[]).unwrap() {
        let mean_x: f64 = l.waypoints.iter().map(|p| p[0]).sum::<f64>() / l.waypoints.len() as f64;
        assert!(mean_x > start[0], "boucle pas orientée vers l'est");
    }
}

#[test]
fn rejects_absurd_distance() {
    let r = Router::new(grid());
    let req = LoopRequest { distance_m: 500.0, ..request(1) };
    assert!(propose_loops(&r, &req, &RouteOptions::default(), &[]).is_err());
}

// --- Itinéraire adapté à une séance ----------------------------------------------------

use goride_core::workout::Terrain;
use goride_core::workout_route::{key_section, rank_for_workout, Need};

/// Quadrillage avec une colline : altitude = 120 m × exp(−d²/σ²), d = distance au centre
/// d'une colline placée au nord-est du départ.
fn hilly_grid() -> Graph {
    let mut g = grid();
    let hill = [28.0 * STEP, 28.0 * STEP];
    let ele = |p: [f32; 2]| {
        let (dx, dy) = ((p[0] as f64 - hill[0]) / STEP, (p[1] as f64 - hill[1]) / STEP);
        120.0 * (-(dx * dx + dy * dy) / 60.0).exp()
    };
    for s in 0..g.seg_count() {
        let (a, b) = (g.node_pos[g.seg_from[s] as usize], g.node_pos[g.seg_to[s] as usize]);
        let d = (ele(b) - ele(a)) as f32;
        g.seg_up[s] = d.max(0.0);
        g.seg_down[s] = (-d).max(0.0);
        let grade = (d / g.seg_len[s] * 100.0).round() as i8;
        g.seg_grade_fwd[s] = grade.max(0);
        g.seg_grade_bwd[s] = (-grade).max(0);
    }
    g.has_elevation = true;
    g
}

#[test]
fn workout_loops_find_the_hill_for_climbing_efforts() {
    let r = Router::new(hilly_grid());
    let opts = RouteOptions { hills: 1.0, ..RouteOptions::default() };
    let loops = propose_loops(&r, &LoopRequest { count: 8, ..request(5) }, &opts, &[]).unwrap();
    // 4 min d'effort à 900 m/h ≈ 60 m de montée
    let need = Need { terrain: Terrain::Climb, effort_s: 240.0, vam_m_h: 900.0, flat_speed_kmh: 28.0 };
    let ranked = rank_for_workout(loops, &r.graph, &need, 4);
    assert!(!ranked.is_empty());
    let (best, key) = &ranked[0];
    let k = key.as_ref().expect("la meilleure boucle doit passer par une montée");
    assert!(k.gain_m >= 30.0, "montée de {:.0} m seulement", k.gain_m);
    assert!(k.grade_pct >= 3.0);
    assert!(k.end_m <= best.length_m + 1.0 && k.start_m < k.end_m);
    assert!(k.coords.len() >= 2, "portion à surligner vide");
    // Les boucles sont classées : la première est au moins aussi adaptée que les suivantes
    for (_, other) in &ranked[1..] {
        if let Some(o) = other {
            assert!(o.fit.min(1.2) <= k.fit.min(1.2) + 0.25);
        }
    }
}

#[test]
fn workout_loops_find_flat_stretches() {
    let r = Router::new(hilly_grid());
    let opts = RouteOptions { hills: -1.0, ..RouteOptions::default() };
    let loops = propose_loops(&r, &request(6), &opts, &[]).unwrap();
    let need = Need { terrain: Terrain::Flat, effort_s: 300.0, vam_m_h: 900.0, flat_speed_kmh: 30.0 };
    let k = loops.iter().filter_map(|l| key_section(l, &r.graph, &need)).max_by(|a, b| a.length_m.total_cmp(&b.length_m));
    let k = k.expect("une portion plate");
    assert!(k.length_m >= 1_000.0);
    assert!((k.est_s - k.length_m / (30.0 / 3.6)).abs() < 1.0);
    // Sans besoin de terrain, aucune portion n'est mise en avant
    let any = Need { terrain: Terrain::Any, ..need };
    assert!(key_section(&loops[0], &r.graph, &any).is_none());
}

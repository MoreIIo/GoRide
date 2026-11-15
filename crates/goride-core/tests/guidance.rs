//! Tests du guidage sur un petit réseau fictif (1 millième de degré ≈ 111 m).
//!
//!                 31
//!                  │
//!          32 ── (rond-point) ── 30        rond-point centré en (6 ; 6), sens anti-horaire
//!                  │
//!        N         11
//!        │         │
//!  W ─── X ─── E ──┘ (virage à angle droit, sans carrefour)
//!        │
//!        S

use goride_core::graph::{build_graph, Graph, RawWay};
use goride_core::guidance::{maneuvers, Maneuver, ManeuverKind};
use goride_core::osm_tags::{RoadClass, Surface, WayInfo};
use goride_core::router::{RouteOptions, Router};

const K: f64 = 0.001;

fn way(nodes: &[i64], oneway: bool, roundabout: bool) -> RawWay {
    RawWay {
        nodes: nodes.to_vec(),
        info: WayInfo { class: RoadClass::Minor, surface: Surface::Paved, forward: true, backward: !oneway, structure: false, roundabout, rough: false },
    }
}

fn graph() -> Graph {
    let mut coords: Vec<(i64, [f64; 2])> = vec![
        (1, [0.0, 0.0]),          // X
        (2, [-3.0 * K, 0.0]),     // W
        (3, [3.0 * K, 0.0]),      // E
        (4, [0.0, 3.0 * K]),      // N
        (5, [0.0, -3.0 * K]),     // S
        (10, [6.0 * K, 0.0]),     // virage (point de forme)
        (11, [6.0 * K, 3.0 * K]),
        (20, [6.0 * K, 5.7 * K]), // rond-point : sud
        (21, [6.3 * K, 6.0 * K]), // est
        (22, [6.0 * K, 6.3 * K]), // nord
        (23, [5.7 * K, 6.0 * K]), // ouest
        (30, [9.0 * K, 6.0 * K]),
        (31, [6.0 * K, 9.0 * K]),
        (32, [3.0 * K, 6.0 * K]),
    ];
    let mut ways = vec![
        way(&[2, 1, 3], false, false),
        way(&[4, 1, 5], false, false),
        way(&[3, 10, 11], false, false),
        way(&[11, 20], false, false),
        way(&[20, 21, 22, 23, 20], true, true),
        way(&[21, 30], false, false),
        way(&[22, 31], false, false),
        way(&[23, 32], false, false),
    ];
    // Longue chaîne vers l'ouest : sans elle, ce petit réseau serait écarté comme « îlot »
    let mut prev = 2;
    for k in 0..60 {
        let id = 1000 + k;
        coords.push((id, [-3.0 * K - (k + 1) as f64 * K, 0.0]));
        ways.push(way(&[prev, id], false, false));
        prev = id;
    }
    build_graph(&ways, |id| coords.iter().find(|c| c.0 == id).map(|c| c.1))
}

fn route(r: &Router, from: [f64; 2], to: [f64; 2]) -> Vec<[f64; 2]> {
    let o = RouteOptions::default();
    let (a, b) = (r.snap(from, &o).unwrap(), r.snap(to, &o).unwrap());
    r.route(&a, &b, &o).unwrap().coords
}

fn kinds(m: &[Maneuver]) -> Vec<ManeuverKind> {
    m.iter().map(|x| x.kind).collect()
}

#[test]
fn straight_through_a_crossroads_says_nothing() {
    let r = Router::new(graph());
    let m = maneuvers(Some(&r), &route(&r, [-2.5 * K, 0.0], [2.5 * K, 0.0]));
    assert_eq!(kinds(&m), [ManeuverKind::Arrive]);
    assert!((m[0].at_m - 556.0).abs() < 5.0, "arrivée à {} m", m[0].at_m);
}

#[test]
fn left_and_right_at_a_crossroads() {
    let r = Router::new(graph());
    let m = maneuvers(Some(&r), &route(&r, [-2.5 * K, 0.0], [0.0, 2.5 * K]));
    assert_eq!(kinds(&m), [ManeuverKind::Left, ManeuverKind::Arrive]);
    assert!((m[0].at_m - 278.0).abs() < 3.0, "virage à {} m", m[0].at_m);
    assert!((m[0].angle + 90.0).abs() < 2.0);

    let m = maneuvers(Some(&r), &route(&r, [-2.5 * K, 0.0], [0.0, -2.5 * K]));
    assert_eq!(kinds(&m), [ManeuverKind::Right, ManeuverKind::Arrive]);
}

#[test]
fn a_bend_in_the_road_is_not_a_turn() {
    let r = Router::new(graph());
    let coords = route(&r, [3.5 * K, 0.0], [6.0 * K, 2.5 * K]);
    assert_eq!(kinds(&maneuvers(Some(&r), &coords)), [ManeuverKind::Arrive]);
    // Sans réseau, on ne peut pas savoir : le virage franc est annoncé
    assert_eq!(kinds(&maneuvers(None, &coords)), [ManeuverKind::Left, ManeuverKind::Arrive]);
}

#[test]
fn roundabout_exits_are_counted() {
    let r = Router::new(graph());
    let start = [6.0 * K, 4.0 * K];
    for (to, exit) in [([8.0 * K, 6.0 * K], 1), ([6.0 * K, 8.0 * K], 2), ([4.0 * K, 6.0 * K], 3)] {
        let m = maneuvers(Some(&r), &route(&r, start, to));
        assert_eq!(kinds(&m), [ManeuverKind::Roundabout, ManeuverKind::Arrive], "vers {to:?}");
        assert_eq!(m[0].exit, Some(exit), "vers {to:?}");
        assert!((m[0].at_m - 189.0).abs() < 4.0, "entrée à {} m", m[0].at_m);
    }
}

#[test]
fn turning_back_is_a_u_turn() {
    let r = Router::new(graph());
    let coords = [[-2.5 * K, 0.0], [-1.0 * K, 0.0], [-2.5 * K, 0.0]];
    let m = maneuvers(Some(&r), &coords);
    assert_eq!(kinds(&m), [ManeuverKind::UTurn, ManeuverKind::Arrive]);
    assert!((m[0].at_m - 167.0).abs() < 25.0, "demi-tour à {} m", m[0].at_m);
}

#[test]
fn a_trace_from_elsewhere_still_finds_the_junction() {
    // Trace décalée de quelques mètres par rapport aux routes (autre planificateur, GPS)
    let r = Router::new(graph());
    let off = 0.04 * K;
    let coords = [[-2.5 * K, off], [off, off], [off, 2.5 * K]];
    let m = maneuvers(Some(&r), &coords);
    assert_eq!(kinds(&m), [ManeuverKind::Left, ManeuverKind::Arrive]);
}

#[test]
fn empty_or_single_point() {
    assert!(maneuvers(None, &[]).is_empty());
    assert!(maneuvers(None, &[[0.0, 0.0]]).is_empty());
}

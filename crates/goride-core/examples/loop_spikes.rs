//! Mesure des « crochets » dans les boucles proposées : endroits où la boucle
//! part toucher un point et revient presque au même endroit.
//! cargo run --release -p goride-core --example loop_spikes -- data/bretagne.osm.graph LON LAT KM
use goride_core::geo::haversine_m;
use goride_core::graph::Graph;
use goride_core::loops::{propose_loops, LoopRequest};
use goride_core::router::{RouteOptions, Router};
use std::path::Path;

/// Nombre de crochets : points où 2 km avant et 2 km après sont à moins de 1 km l'un de l'autre.
pub fn spikes(coords: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut cum = vec![0.0];
    for w in coords.windows(2) {
        cum.push(cum.last().unwrap() + haversine_m(w[0], w[1]));
    }
    let at = |d: f64| coords[cum.partition_point(|&c| c < d).min(coords.len() - 1)];
    let total = *cum.last().unwrap();
    let (mut n, mut d, mut in_spike) = (Vec::new(), 2_000.0, false);
    while d < total - 2_000.0 {
        let s = haversine_m(at(d - 2_000.0), at(d + 2_000.0)) < 1_000.0;
        if s && !in_spike {
            n.push(at(d));
        }
        in_spike = s;
        d += 100.0;
    }
    n
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let r = Router::new(Graph::load(Path::new(&a[1])).unwrap());
    let start = [a[2].parse().unwrap(), a[3].parse().unwrap()];
    let km: f64 = a[4].parse().unwrap();
    let (mut total, mut with_spike, mut count, mut spurs) = (0, 0, 0, 0);
    for seed in 0..20 {
        let req = LoopRequest { start, distance_m: km * 1000.0, tolerance: 0.05, count: 5, seed, heading_deg: None };
        for l in propose_loops(&r, &req, &RouteOptions::default(), &[]).unwrap() {
            let coords: Vec<[f64; 2]> = l.legs.iter().flat_map(|g| g.coords.iter().copied()).collect();
            // Petits allers-retours aux points de passage : l'étape suivante repart par le chemin d'arrivée
            for w in l.legs.windows(2) {
                let (a, b) = (&w[0].coords, &w[1].coords);
                let mut len = 0.0;
                let mut k = 0;
                while k + 1 < a.len() && k + 1 < b.len() && haversine_m(a[a.len() - 2 - k], b[k + 1]) < 1.0 {
                    len += haversine_m(b[k], b[k + 1]);
                    k += 1;
                }
                if len > 20.0 {
                    spurs += 1;
                }
            }
            let s = spikes(&coords);
            total += s.len();
            count += 1;
            if !s.is_empty() {
                with_spike += 1;
            }

        }
    }
    println!("{count} boucles : {with_spike} avec au moins un crochet, {total} crochets au total, {spurs} petits allers-retours aux points de passage");
}

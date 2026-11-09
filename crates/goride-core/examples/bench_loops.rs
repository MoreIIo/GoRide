//! Porte de la phase 3 : 5 boucles de 50 km en moins de 3 s.
//! cargo run --release -p goride-core --example bench_loops -- data/bretagne-latest.osm.pbf
use goride_core::graph::{pbf::graph_from_pbf, Graph};
use goride_core::loops::{overlap, propose_loops, LoopRequest};
use goride_core::router::{Profile, RouteOptions, Router};
use std::path::Path;
use std::time::Instant;

fn main() {
    let pbf = std::env::args().nth(1).expect("chemin du .osm.pbf");
    let out = Path::new(&pbf).with_extension("graph");
    let g = Graph::load(&out).unwrap_or_else(|_| {
        let g = graph_from_pbf(Path::new(&pbf), |_| {}).unwrap();
        g.save(&out).unwrap();
        g
    });
    let r = Router::new(g);
    println!("{} cœurs", rayon::current_num_threads());
    let cases: &[(&str, [f64; 2], f64, Option<f64>, Profile)] = &[
        ("Saint-Grégoire, 50 km", [-1.6853, 48.1505], 50_000.0, None, Profile::Road),
        ("Saint-Grégoire, 50 km, vers le nord", [-1.6853, 48.1505], 50_000.0, Some(0.0), Profile::Road),
        ("Saint-Grégoire, 100 km", [-1.6853, 48.1505], 100_000.0, None, Profile::Road),
        ("Saint-Grégoire, 50 km, triathlon", [-1.6853, 48.1505], 50_000.0, None, Profile::Triathlon),
        ("Brest (côte), 50 km", [-4.4861, 48.3904], 50_000.0, None, Profile::Road),
        ("Saint-Malo (côte), 40 km", [-2.0257, 48.6493], 40_000.0, None, Profile::Road),
        ("Saint-Grégoire, 20 km, gravel", [-1.6853, 48.1505], 20_000.0, None, Profile::Gravel),
    ];
    for (name, start, dist, heading, profile) in cases {
        let opts = RouteOptions { profile: *profile, ..Default::default() };
        let req = LoopRequest { start: *start, distance_m: *dist, tolerance: 0.05, count: 5, seed: 1, heading_deg: *heading };
        let t = Instant::now();
        match propose_loops(&r, &req, &opts, &[]) {
            Ok(loops) => {
                let ms = t.elapsed().as_millis();
                let mut max_ov: f64 = 0.0;
                for (i, a) in loops.iter().enumerate() {
                    for b in &loops[i + 1..] {
                        max_ov = max_ov.max(overlap(&a.footprint, &b.footprint));
                    }
                }
                let txt: Vec<String> = loops
                    .iter()
                    .map(|l| format!("{:.1} km/AR {:.0} %", l.length_m / 1000.0, l.retrace * 100.0))
                    .collect();
                println!("{name}: {} boucles en {ms} ms, recouvrement max {:.0} % | {}", loops.len(), max_ov * 100.0, txt.join(" · "));
            }
            Err(e) => println!("{name}: ERREUR {e}"),
        }
    }
}

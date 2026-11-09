//! Banc d'essai sur un vrai extrait OSM, réglage par réglage :
//! cargo run --release -p goride-core --example bench_bretagne -- data/bretagne-latest.osm.pbf
//!
//! Sans accès réseau, les altitudes ne sont pas calculées : les réglages de
//! relief sont alors sans effet (le reste est testé).
use goride_core::geo::haversine_m;
use goride_core::graph::{pbf::graph_from_pbf, Graph};
use goride_core::osm_tags::Surface;
use goride_core::router::{Profile, RouteOptions, Router};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

fn main() {
    let pbf = std::env::args().nth(1).expect("chemin du .osm.pbf");
    let out = Path::new(&pbf).with_extension("graph");
    let g = match Graph::load(&out) {
        Ok(g) => g,
        Err(_) => {
            let t = Instant::now();
            let g = graph_from_pbf(Path::new(&pbf), |_| {}).unwrap();
            println!("construction : {:.1} s, {} tronçons", t.elapsed().as_secs_f32(), g.seg_count());
            g.save(&out).unwrap();
            g
        }
    };
    let t = Instant::now();
    let r = Router::new(g);
    println!("index spatial : {:.2} s (altitudes : {})", t.elapsed().as_secs_f32(), r.graph.has_elevation);

    let trips: &[(&str, [f64; 2], [f64; 2])] = &[
        ("Rennes → Saint-Malo", [-1.6794, 48.1119], [-2.0257, 48.6493]),
        ("Saint-Grégoire → Betton", [-1.6853, 48.1505], [-1.6406, 48.1825]),
        ("Brest → Quimper", [-4.4861, 48.3904], [-4.1024, 47.9960]),
        ("Rennes → Brest", [-1.6794, 48.1119], [-4.4861, 48.3904]),
    ];
    let settings: &[(&str, RouteOptions)] = &[
        ("Route", RouteOptions::default()),
        ("Route, grands axes OK", RouteOptions { traffic: 0.0, ..Default::default() }),
        ("Route, fuir grands axes", RouteOptions { traffic: 1.0, ..Default::default() }),
        ("Route, pistes cyclables +", RouteOptions { cycleways: 1.0, ..Default::default() }),
        ("Triathlon", RouteOptions { profile: Profile::Triathlon, ..Default::default() }),
        ("Gravel", RouteOptions { profile: Profile::Gravel, ..Default::default() }),
    ];
    for (name, a, b) in trips {
        println!("\n{name}");
        for (label, o) in settings {
            let (sa, sb) = (r.snap(*a, o).unwrap(), r.snap(*b, o).unwrap());
            let t = Instant::now();
            match r.route(&sa, &sb, o) {
                Ok(leg) => {
                    let ms = t.elapsed().as_millis();
                    let mut by: BTreeMap<String, f64> = BTreeMap::new();
                    let mut unpaved = 0.0;
                    for ((w, c), s) in leg.coords.windows(2).zip(&leg.class).zip(&leg.surface) {
                        let d = haversine_m(w[0], w[1]) / 1000.0;
                        *by.entry(format!("{c:?}")).or_default() += d;
                        if *s == Surface::Unpaved {
                            unpaved += d;
                        }
                    }
                    let mut v: Vec<_> = by.into_iter().filter(|x| x.1 >= 0.5).collect();
                    v.sort_by(|a, b| b.1.total_cmp(&a.1));
                    let types: Vec<String> = v.iter().map(|(k, d)| format!("{k} {d:.0}")).collect();
                    println!(
                        "  {label:<28} {:>6.1} km  {:>4} ms  non revêtu {:>5.1} km  | {}",
                        leg.length_m / 1000.0,
                        ms,
                        unpaved,
                        types.join(", ")
                    );
                }
                Err(e) => println!("  {label:<28} ERREUR {e}"),
            }
        }
    }
}

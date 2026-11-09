//! Fabrique le réseau prêt à l'emploi d'une région, pour l'appli mobile :
//! routes vélo + relief, compressé. Lancé par GitHub Actions pour chaque région
//! (.github/workflows/reseaux.yml), ou à la main :
//!
//! cargo run --release -p goride-core --example export_region -- bretagne-latest.osm.pbf bretagne.graph.gz [dossier-cache-relief]
use goride_core::dem::Dem;
use goride_core::graph::{elevation::compute_elevation, pbf::graph_from_pbf};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let pbf = args.next().expect("chemin du .osm.pbf");
    let out = args.next().expect("fichier de sortie (.graph.gz)");
    let cache = args.next().map(PathBuf::from);

    let t = Instant::now();
    let mut g = graph_from_pbf(Path::new(&pbf), |step| eprintln!("{step:?}")).unwrap_or_else(|e| {
        eprintln!("Construction impossible : {e}");
        std::process::exit(1);
    });
    eprintln!("Réseau : {} tronçons en {:.0} s", g.seg_count(), t.elapsed().as_secs_f32());

    let t = Instant::now();
    let dem = Dem::new(cache);
    compute_elevation(&mut g, |pts| {
        dem.sample_many(pts, |done, total| {
            if done % 200 == 0 || done == total {
                eprintln!("Relief : {done} / {total} tuiles");
            }
        })
    });
    if !g.has_elevation {
        // Un réseau sans relief serait publié pour un mois : on préfère échouer et relancer.
        eprintln!("Relief indisponible (tuiles de terrain injoignables)");
        std::process::exit(1);
    }
    eprintln!("Relief en {:.0} s", t.elapsed().as_secs_f32());

    g.save_gz(Path::new(&out)).expect("écriture du fichier");
    let mb = std::fs::metadata(&out).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0);
    eprintln!("{out} : {mb:.1} Mo");
}

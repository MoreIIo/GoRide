use goride_core::{parse_gpx, CoreError};

const BOUCLE: &[u8] = include_bytes!("data/petite-boucle.gpx");

#[test]
fn reads_all_segments_and_name() {
    let t = parse_gpx(BOUCLE).unwrap();
    assert_eq!(t.name.as_deref(), Some("Petite boucle de test"));
    assert_eq!(t.stats.point_count, 8);
    assert_eq!(t.coords.len(), t.dist.len());
    assert_eq!(t.coords[0], [-1.7, 48.1]);
}

#[test]
fn distance_is_plausible() {
    // 3 × 111 m vers le nord, 74 m vers l'ouest, 3 × 111 m vers le sud ≈ 741 m
    let t = parse_gpx(BOUCLE).unwrap();
    assert!((t.stats.distance_m - 741.0).abs() < 5.0, "{}", t.stats.distance_m);
    assert!(t.dist.windows(2).all(|w| w[1] >= w[0]));
}

#[test]
fn elevation_stats() {
    let t = parse_gpx(BOUCLE).unwrap();
    // Altitudes : 40, 41, 50, 65, 66, (interpolé), 45, 40.
    // Les variations de 1 m (40→41 et 65→66) restent sous le seuil de 3 m :
    // D+ = 40→65 = 25 m, D− = 65→40 = 25 m.
    assert_eq!(t.stats.ascent_m, 25.0);
    assert_eq!(t.stats.descent_m, 25.0);
    assert_eq!(t.stats.min_ele_m, Some(40.0));
    assert_eq!(t.stats.max_ele_m, Some(66.0));
    // Le point sans <ele> a été interpolé entre 66 et 45
    let e = t.ele[5].unwrap();
    assert!(e < 66.0 && e > 45.0);
}

#[test]
fn route_only_file() {
    let gpx = r#"<?xml version="1.0"?><gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1">
      <rte><name>Itinéraire</name>
        <rtept lat="48.0" lon="-1.0"/><rtept lat="48.01" lon="-1.0"/>
      </rte></gpx>"#;
    let t = parse_gpx(gpx.as_bytes()).unwrap();
    assert_eq!(t.name.as_deref(), Some("Itinéraire"));
    assert_eq!(t.stats.point_count, 2);
    assert!(!t.stats.has_elevation);
}

#[test]
fn empty_and_invalid_files() {
    let empty = br#"<?xml version="1.0"?><gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1"></gpx>"#;
    assert!(matches!(parse_gpx(empty), Err(CoreError::Empty)));
    assert!(matches!(parse_gpx(b"pas du gpx"), Err(CoreError::Gpx(_))));
}

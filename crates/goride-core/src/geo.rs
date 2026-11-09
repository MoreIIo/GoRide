//! Calculs géométriques de base.

/// Rayon moyen de la Terre (m), valeur IUGG.
const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Distance orthodromique (haversine) en mètres entre deux points (lon, lat) en degrés.
pub fn haversine_m(a: [f64; 2], b: [f64; 2]) -> f64 {
    let (lon1, lat1) = (a[0].to_radians(), a[1].to_radians());
    let (lon2, lat2) = (b[0].to_radians(), b[1].to_radians());
    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rennes_saint_malo() {
        // Rennes (place de la Mairie) → Saint-Malo (intra-muros) ≈ 64,6 km à vol d'oiseau
        let d = haversine_m([-1.6794, 48.1119], [-2.0257, 48.6493]);
        assert!((d - 64_600.0).abs() < 1_000.0, "distance = {d}");
    }

    #[test]
    fn same_point_is_zero() {
        assert_eq!(haversine_m([2.35, 48.85], [2.35, 48.85]), 0.0);
    }
}

//! Altitude à partir des tuiles « Terrarium » (modèle de terrain mondial, gratuit).
//!
//! Tuiles au zoom 12 (≈ 25 m par pixel en Bretagne), téléchargées à la
//! demande puis gardées sur le disque : un même secteur n'est téléchargé
//! qu'une fois. Sera complété par l'IGN RGE ALTI (plus précis) en phase 4.

use rayon::prelude::*;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub const DEM_ZOOM: u32 = 12;
const TILE: usize = 256;
const URL: &str = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium";

type Tile = Option<Arc<Vec<f32>>>;

pub struct Dem {
    cache_dir: Option<PathBuf>,
    tiles: Mutex<HashMap<(u32, u32), Tile>>,
}

impl Dem {
    /// `cache_dir` : dossier où garder les tuiles téléchargées (None = mémoire seulement).
    pub fn new(cache_dir: Option<PathBuf>) -> Dem {
        Dem { cache_dir, tiles: Mutex::new(HashMap::new()) }
    }

    /// Altitude (m) de chaque point (lon, lat). `None` si la tuile est indisponible.
    pub fn elevations(&self, pts: &[[f64; 2]]) -> Vec<Option<f64>> {
        // Téléchargement en parallèle des tuiles manquantes
        let mut needed: Vec<(u32, u32)> = pts.iter().map(|p| tile_pos(*p).0).collect();
        needed.sort_unstable();
        needed.dedup();
        let missing: Vec<(u32, u32)> = {
            let tiles = self.tiles.lock().unwrap();
            needed.into_iter().filter(|t| !tiles.contains_key(t)).collect()
        };
        let fetched: Vec<((u32, u32), Tile)> =
            missing.into_par_iter().map(|t| (t, self.load_tile(t).ok().map(Arc::new))).collect();
        let mut tiles = self.tiles.lock().unwrap();
        tiles.extend(fetched);

        pts.iter()
            .map(|&p| {
                let (t, px, py) = tile_pos(p);
                tiles.get(&t)?.as_ref().map(|data| bilinear(data, px, py))
            })
            .collect()
    }

    /// Altitude d'un très grand nombre de points (tout un réseau routier),
    /// NaN si inconnue. Les tuiles sont traitées une à une sans rester en
    /// mémoire ; `progress(faites, total)` est appelé après chaque tuile.
    pub fn sample_many(&self, pts: &[[f32; 2]], progress: impl Fn(usize, usize) + Sync) -> Vec<f32> {
        let mut order: Vec<(u64, u32)> = pts
            .par_iter()
            .enumerate()
            .map(|(i, p)| {
                let ((x, y), _, _) = tile_pos([p[0] as f64, p[1] as f64]);
                (((x as u64) << 32) | y as u64, i as u32)
            })
            .collect();
        order.par_sort_unstable();
        // Groupes de points par tuile
        let mut groups: Vec<(usize, usize)> = Vec::new();
        let mut a = 0;
        for b in 1..=order.len() {
            if b == order.len() || order[b].0 != order[a].0 {
                groups.push((a, b));
                a = b;
            }
        }
        let total = groups.len();
        let done = AtomicUsize::new(0);
        let values: Vec<Vec<(u32, f32)>> = groups
            .par_iter()
            .map(|&(a, b)| {
                let key = order[a].0;
                let tile = self.load_tile(((key >> 32) as u32, key as u32)).ok();
                let out = order[a..b]
                    .iter()
                    .map(|&(_, i)| {
                        let p = pts[i as usize];
                        let v = tile.as_ref().map_or(f32::NAN, |t| {
                            let (_, px, py) = tile_pos([p[0] as f64, p[1] as f64]);
                            bilinear(t, px, py) as f32
                        });
                        (i, v)
                    })
                    .collect();
                progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                out
            })
            .collect();
        let mut out = vec![f32::NAN; pts.len()];
        for group in values {
            for (i, v) in group {
                out[i as usize] = v;
            }
        }
        out
    }

    fn load_tile(&self, (x, y): (u32, u32)) -> Result<Vec<f32>, String> {
        let path = self.cache_dir.as_ref().map(|d| d.join(format!("terrarium/{DEM_ZOOM}/{x}/{y}.png")));
        if let Some(p) = &path {
            if let Ok(bytes) = std::fs::read(p) {
                if let Ok(v) = decode_terrarium(&bytes) {
                    return Ok(v);
                }
            }
        }
        let url = format!("{URL}/{DEM_ZOOM}/{x}/{y}.png");
        let resp = ureq::get(&url).call().map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        resp.into_reader().take(4 << 20).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let v = decode_terrarium(&bytes)?;
        if let Some(p) = &path {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, &bytes);
        }
        Ok(v)
    }
}

/// Tuile contenant le point et position en pixels (flottants) dans cette tuile.
fn tile_pos([lon, lat]: [f64; 2]) -> ((u32, u32), f64, f64) {
    let n = (1u32 << DEM_ZOOM) as f64;
    let lat = lat.clamp(-85.0511, 85.0511).to_radians();
    let xf = (lon + 180.0) / 360.0 * n;
    let yf = (1.0 - lat.tan().asinh() / std::f64::consts::PI) / 2.0 * n;
    let (tx, ty) = (xf.floor().clamp(0.0, n - 1.0), yf.floor().clamp(0.0, n - 1.0));
    ((tx as u32, ty as u32), (xf - tx) * TILE as f64, (yf - ty) * TILE as f64)
}

/// Interpolation bilinéaire entre les centres de pixels.
fn bilinear(data: &[f32], px: f64, py: f64) -> f64 {
    let max = (TILE - 1) as f64;
    let x = (px - 0.5).clamp(0.0, max);
    let y = (py - 0.5).clamp(0.0, max);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(TILE - 1), (y0 + 1).min(TILE - 1));
    let (fx, fy) = (x - x0 as f64, y - y0 as f64);
    let v = |x: usize, y: usize| data[y * TILE + x] as f64;
    let top = v(x0, y0) * (1.0 - fx) + v(x1, y0) * fx;
    let bottom = v(x0, y1) * (1.0 - fx) + v(x1, y1) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// Décode une tuile Terrarium : altitude = R × 256 + G + B / 256 − 32768.
pub fn decode_terrarium(bytes: &[u8]) -> Result<Vec<f32>, String> {
    let mut dec = png::Decoder::new(bytes);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if info.width as usize != TILE || info.height as usize != TILE {
        return Err("taille de tuile inattendue".into());
    }
    let ch = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return Err("format de tuile inattendu".into()),
    };
    Ok(buf[..info.buffer_size()]
        .chunks_exact(ch)
        .map(|p| p[0] as f32 * 256.0 + p[1] as f32 + p[2] as f32 / 256.0 - 32768.0)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile_png(elev: impl Fn(usize, usize) -> f32) -> Vec<u8> {
        let mut raw = Vec::with_capacity(TILE * TILE * 3);
        for y in 0..TILE {
            for x in 0..TILE {
                let v = elev(x, y) + 32768.0;
                let r = (v / 256.0).floor();
                let gg = (v - r * 256.0).floor();
                let b = ((v - r * 256.0 - gg) * 256.0).round();
                raw.extend([r as u8, gg as u8, b as u8]);
            }
        }
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, TILE as u32, TILE as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&raw).unwrap();
        out
    }

    #[test]
    fn decode_and_interpolate() {
        let data = decode_terrarium(&tile_png(|x, _| x as f32 * 2.0 + 0.5)).unwrap();
        assert_eq!(data.len(), TILE * TILE);
        assert!((data[10] - 20.5).abs() < 0.01);
        // Au milieu entre les pixels 10 et 11 (centres 10,5 et 11,5) → 21,5
        assert!((bilinear(&data, 11.0, 5.5) - 21.5).abs() < 0.01);
    }

    #[test]
    fn negative_elevations() {
        let data = decode_terrarium(&tile_png(|_, _| -12.25)).unwrap();
        assert!((data[0] + 12.25).abs() < 0.01);
    }

    #[test]
    fn tile_of_rennes() {
        let ((x, y), px, py) = tile_pos([-1.6794, 48.1119]);
        // Valeurs de référence : formule « slippy map » d'OpenStreetMap
        assert_eq!((x, y), (2028, 1421));
        assert!((0.0..256.0).contains(&px) && (0.0..256.0).contains(&py));
    }
}

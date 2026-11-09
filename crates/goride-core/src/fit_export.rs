//! Export d'un itinéraire en fichier FIT « Course » (parcours Garmin).
//!
//! Format binaire Garmin : en-tête, puis messages (chacun précédé de sa
//! définition), puis CRC. Contenu d'un parcours, comme ceux exportés par
//! Garmin Connect ou Strava :
//! `file_id` → `course` → `lap` → `event` (départ) → `record` × N → `event` (fin).
//!
//! Les points reçoivent des horodatages calculés à la vitesse moyenne
//! choisie : le compteur s'en sert pour le partenaire virtuel.

use crate::track::Track;

// Types de base FIT
pub(crate) const ENUM: u8 = 0x00;
pub(crate) const UINT16: u8 = 0x84;
const SINT32: u8 = 0x85;
pub(crate) const UINT32: u8 = 0x86;
pub(crate) const STRING: u8 = 0x07;
pub(crate) const UINT32Z: u8 = 0x8C;

// Messages globaux
const MSG_FILE_ID: u16 = 0;
const MSG_LAP: u16 = 19;
const MSG_RECORD: u16 = 20;
const MSG_EVENT: u16 = 21;
const MSG_COURSE: u16 = 31;

const PROFILE_VERSION: u16 = 2132;
const NAME_LEN: usize = 32;
/// Secondes entre l'époque Unix (1970) et l'époque FIT (31/12/1989).
const FIT_EPOCH_OFFSET: u64 = 631_065_600;

const CRC_TABLE: [u16; 16] = [
    0x0000, 0xCC01, 0xD801, 0x1400, 0xF001, 0x3C00, 0x2800, 0xE401, 0xA001, 0x6C00, 0x7800, 0xB401, 0x5000, 0x9C01,
    0x8801, 0x4400,
];

pub(crate) fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in bytes {
        let mut tmp = CRC_TABLE[(crc & 0xF) as usize];
        crc = (crc >> 4) & 0x0FFF;
        crc = crc ^ tmp ^ CRC_TABLE[(b & 0xF) as usize];
        tmp = CRC_TABLE[(crc & 0xF) as usize];
        crc = (crc >> 4) & 0x0FFF;
        crc = crc ^ tmp ^ CRC_TABLE[((b >> 4) & 0xF) as usize];
    }
    crc
}

/// Degrés → semi-cercles (unité de position FIT).
fn semicircles(deg: f64) -> i32 {
    (deg * (2f64.powi(31) / 180.0)).round() as i32
}

pub(crate) struct Writer {
    pub(crate) data: Vec<u8>,
}

impl Writer {
    /// Message de définition : `local` = numéro local (0–15), champs = (numéro, taille, type).
    pub(crate) fn define(&mut self, local: u8, global: u16, fields: &[(u8, u8, u8)]) {
        self.data.push(0x40 | local);
        self.data.push(0); // réservé
        self.data.push(0); // petit-boutiste
        self.data.extend_from_slice(&global.to_le_bytes());
        self.data.push(fields.len() as u8);
        for &(num, size, ty) in fields {
            self.data.extend_from_slice(&[num, size, ty]);
        }
    }
    pub(crate) fn header(&mut self, local: u8) {
        self.data.push(local);
    }
    pub(crate) fn u8(&mut self, v: u8) {
        self.data.push(v);
    }
    pub(crate) fn u16(&mut self, v: u16) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn u32(&mut self, v: u32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn i32(&mut self, v: i32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn string(&mut self, s: &str, len: usize) {
        let mut bytes: Vec<u8> = Vec::with_capacity(len);
        for ch in s.chars() {
            let mut buf = [0u8; 4];
            let enc = ch.encode_utf8(&mut buf).as_bytes();
            if bytes.len() + enc.len() > len - 1 {
                break; // tronqué sans couper un caractère accentué
            }
            bytes.extend_from_slice(enc);
        }
        bytes.resize(len, 0);
        self.data.extend_from_slice(&bytes);
    }
}

/// En-tête FIT de 14 octets (avec son CRC) pour `data_len` octets de messages.
pub(crate) fn fit_header(data_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data_len + 16);
    out.push(14);
    out.push(0x20); // protocole 2.0
    out.extend_from_slice(&PROFILE_VERSION.to_le_bytes());
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    out.extend_from_slice(b".FIT");
    let hcrc = crc16(&out);
    out.extend_from_slice(&hcrc.to_le_bytes());
    out
}

/// Altitude FIT : (m + 500) × 5, 0xFFFF si inconnue.
fn altitude(e: Option<f64>) -> u16 {
    match e {
        Some(e) => ((e + 500.0) * 5.0).round().clamp(0.0, 65_534.0) as u16,
        None => 0xFFFF,
    }
}

/// Encode la trace en parcours FIT. `speed_kmh` sert aux horodatages, `now_unix` à la date de création.
pub fn to_fit_course(name: &str, track: &Track, speed_kmh: f64, now_unix: u64) -> Vec<u8> {
    let speed = (speed_kmh.max(1.0)) / 3.6; // m/s
    let t0 = (now_unix.saturating_sub(FIT_EPOCH_OFFSET)) as u32;
    let time_at = |d: f64| t0 + (d / speed).round() as u32;
    let n = track.coords.len();
    let total = track.stats.distance_m;
    let t_end = time_at(total);
    let first = track.coords.first().copied().unwrap_or([0.0, 0.0]);
    let last = track.coords.last().copied().unwrap_or([0.0, 0.0]);

    let mut w = Writer { data: Vec::with_capacity(n * 20 + 256) };

    // file_id : type = parcours (6)
    w.define(0, MSG_FILE_ID, &[(0, 1, ENUM), (1, 2, UINT16), (2, 2, UINT16), (3, 4, UINT32Z), (4, 4, UINT32)]);
    w.header(0);
    w.u8(6);
    w.u16(255); // fabricant « development »
    w.u16(1);
    w.u32(t0.max(1));
    w.u32(t0);

    // course : nom et sport (vélo = 2)
    w.define(1, MSG_COURSE, &[(5, NAME_LEN as u8, STRING), (4, 1, ENUM)]);
    w.header(1);
    w.string(name, NAME_LEN);
    w.u8(2);

    // lap : résumé du parcours
    w.define(
        2,
        MSG_LAP,
        &[
            (253, 4, UINT32),
            (2, 4, UINT32),
            (3, 4, SINT32),
            (4, 4, SINT32),
            (5, 4, SINT32),
            (6, 4, SINT32),
            (7, 4, UINT32),
            (8, 4, UINT32),
            (9, 4, UINT32),
            (21, 2, UINT16),
            (22, 2, UINT16),
        ],
    );
    w.header(2);
    w.u32(t_end);
    w.u32(t0);
    w.i32(semicircles(first[1]));
    w.i32(semicircles(first[0]));
    w.i32(semicircles(last[1]));
    w.i32(semicircles(last[0]));
    let dur_ms = ((t_end - t0) as u64 * 1000).min(u32::MAX as u64) as u32;
    w.u32(dur_ms);
    w.u32(dur_ms);
    w.u32((total * 100.0).round() as u32);
    let (up, down) = if track.stats.has_elevation {
        (track.stats.ascent_m.round() as u16, track.stats.descent_m.round() as u16)
    } else {
        (0xFFFF, 0xFFFF)
    };
    w.u16(up);
    w.u16(down);

    // event : départ du chrono (event = timer 0, type = start 0)
    w.define(3, MSG_EVENT, &[(253, 4, UINT32), (0, 1, ENUM), (1, 1, ENUM)]);
    w.header(3);
    w.u32(t0);
    w.u8(0);
    w.u8(0);

    // record : un par point
    w.define(4, MSG_RECORD, &[(253, 4, UINT32), (0, 4, SINT32), (1, 4, SINT32), (2, 2, UINT16), (5, 4, UINT32)]);
    for i in 0..n {
        let c = track.coords[i];
        let d = track.dist[i];
        w.header(4);
        w.u32(time_at(d));
        w.i32(semicircles(c[1]));
        w.i32(semicircles(c[0]));
        w.u16(altitude(track.ele[i]));
        w.u32((d * 100.0).round() as u32);
    }

    // event : fin (timer, stop_disable_all = 9)
    w.header(3);
    w.u32(t_end);
    w.u8(0);
    w.u8(9);

    // En-tête de 14 octets + CRC final
    let mut out = fit_header(w.data.len());
    out.extend_from_slice(&w.data);
    let crc = crc16(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fitparser::profile::MesgNum;
    use fitparser::Value;

    fn sample() -> Track {
        Track::from_points(
            None,
            vec![
                ([-1.68, 48.11], Some(40.0)),
                ([-1.68, 48.12], Some(60.0)),
                ([-1.67, 48.12], None),
                ([-1.66, 48.12], Some(45.0)),
            ],
        )
    }

    #[test]
    fn crc_of_empty_is_zero() {
        assert_eq!(crc16(&[]), 0);
    }

    #[test]
    fn readable_by_an_independent_fit_parser() {
        let t = sample();
        let bytes = to_fit_course("Boucle de Saint-Grégoire très longue au nom tronqué", &t, 27.0, 1_794_170_000);
        // Le lecteur vérifie l'en-tête et les deux CRC
        let msgs = fitparser::from_bytes(&bytes).expect("FIT illisible");

        let kinds: Vec<MesgNum> = msgs.iter().map(|m| m.kind()).collect();
        assert_eq!(kinds[0], MesgNum::FileId);
        assert_eq!(kinds.iter().filter(|k| **k == MesgNum::Record).count(), 4);
        assert!(kinds.contains(&MesgNum::Course) && kinds.contains(&MesgNum::Lap));

        let field = |m: &fitparser::FitDataRecord, name: &str| {
            m.fields().iter().find(|f| f.name() == name).map(|f| f.value().clone())
        };
        let file_id = &msgs[0];
        assert_eq!(field(file_id, "type"), Some(Value::String("course".into())));

        let course = msgs.iter().find(|m| m.kind() == MesgNum::Course).unwrap();
        match field(course, "name") {
            Some(Value::String(s)) => assert!(s.starts_with("Boucle de Saint-Gr") && s.len() <= 31),
            other => panic!("nom inattendu : {other:?}"),
        }

        let recs: Vec<_> = msgs.iter().filter(|m| m.kind() == MesgNum::Record).collect();
        // Le lecteur convertit le champ altitude en « enhanced_altitude » (même valeur)
        match field(recs[1], "enhanced_altitude").or_else(|| field(recs[1], "altitude")) {
            Some(Value::Float64(a)) => assert!((a - 60.0).abs() < 0.3, "{a}"),
            other => panic!("altitude inattendue : {other:?}"),
        }
        match field(recs[1], "position_lat") {
            Some(Value::SInt32(lat)) => assert!((lat as f64 * 180.0 / 2f64.powi(31) - 48.12).abs() < 1e-6),
            other => panic!("latitude inattendue : {other:?}"),
        }
        match field(recs[3], "distance") {
            Some(Value::Float64(d)) => assert!((d - t.stats.distance_m).abs() < 0.02),
            other => panic!("distance inattendue : {other:?}"),
        }
    }
}

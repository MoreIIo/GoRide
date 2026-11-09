//! Lecture des dates ISO 8601 des fichiers GPX et TCX (« 2026-08-12T07:31:05Z »).

/// Jours depuis le 1er janvier 1970 (calendrier grégorien proleptique).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Secondes Unix (avec fraction) ; gère « Z », « +02:00 » et les fractions de seconde.
pub fn parse_iso8601(s: &str) -> Option<f64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', ' '])?;
    let mut dp = date.split('-');
    let (y, mo, d): (i64, i64, i64) = (dp.next()?.parse().ok()?, dp.next()?.parse().ok()?, dp.next()?.parse().ok()?);
    // Fuseau
    let (clock, offset_s) = if let Some(c) = rest.strip_suffix('Z') {
        (c, 0i64)
    } else if let Some(pos) = rest.rfind(['+', '-']).filter(|&p| p >= 8) {
        let (c, off) = rest.split_at(pos);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let off = &off[1..];
        let (oh, om) = off.split_once(':').unwrap_or((&off[..2.min(off.len())], off.get(2..).unwrap_or("0")));
        (c, sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().unwrap_or(0) * 60))
    } else {
        (rest, 0)
    };
    let mut cp = clock.split(':');
    let h: i64 = cp.next()?.parse().ok()?;
    let mi: i64 = cp.next()?.parse().ok()?;
    let sec: f64 = cp.next().unwrap_or("0").parse().ok()?;
    let days = days_from_civil(y, mo, d);
    Some((days * 86_400 + h * 3600 + mi * 60 - offset_s) as f64 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_iso8601("2026-08-12T07:31:05Z"), Some(1_786_519_865.0));
        assert_eq!(parse_iso8601("2026-08-12T09:31:05+02:00"), Some(1_786_519_865.0));
        assert_eq!(parse_iso8601("2026-08-12T07:31:05.500Z"), Some(1_786_519_865.5));
        assert_eq!(parse_iso8601("pas une date"), None);
    }
}

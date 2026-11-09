/** Miroir de `goride_core::Track` (sérialisé en camelCase côté Rust). */
export interface TrackStats {
  distanceM: number;
  ascentM: number;
  descentM: number;
  minEleM: number | null;
  maxEleM: number | null;
  pointCount: number;
  hasElevation: boolean;
}

export interface Track {
  name: string | null;
  coords: [number, number][];
  ele: (number | null)[];
  dist: number[];
  stats: TrackStats;
}

/** Index du premier point dont la distance cumulée est ≥ d (recherche dichotomique). */
export function indexAtDistance(track: Track, d: number): number {
  const a = track.dist;
  let lo = 0;
  let hi = a.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (a[mid] < d) lo = mid + 1;
    else hi = mid;
  }
  // garde le plus proche des deux voisins
  if (lo > 0 && d - a[lo - 1] < a[lo] - d) return lo - 1;
  return lo;
}

/** Pente moyenne (%) sur une fenêtre de ±`halfWindowM` mètres autour du point i. */
export function slopeAt(track: Track, i: number, halfWindowM = 50): number | null {
  if (!track.stats.hasElevation) return null;
  const a = indexAtDistance(track, track.dist[i] - halfWindowM);
  const b = indexAtDistance(track, track.dist[i] + halfWindowM);
  const run = track.dist[b] - track.dist[a];
  if (run < 10) return null;
  return ((track.ele[b]! - track.ele[a]!) / run) * 100;
}

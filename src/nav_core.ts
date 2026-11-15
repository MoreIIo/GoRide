/**
 * Cœur du mode navigation, sans carte ni interface : où en est-on sur l'itinéraire,
 * quelle est la prochaine direction, et comment le dire.
 */

export type ManeuverKind =
  | "slightLeft" | "left" | "sharpLeft"
  | "slightRight" | "right" | "sharpRight"
  | "uTurn" | "roundabout" | "arrive";

/** Miroir de `goride_core::guidance::Maneuver`. */
export interface Maneuver {
  kind: ManeuverKind;
  /** Distance depuis le départ (m). */
  atM: number;
  point: [number, number];
  /** Angle du changement de direction : négatif à gauche, positif à droite. */
  angle: number;
  /** Rond-point : numéro de la sortie. */
  exit: number | null;
}

type LngLat = [number, number];

const M_PER_DEG_LAT = 110_574;
const M_PER_DEG_LON = 111_320;

/** Position rapportée à l'itinéraire. */
export interface Located {
  /** Distance parcourue depuis le départ (m). */
  m: number;
  /** Écart à l'itinéraire (m). */
  off: number;
  /** Point de l'itinéraire le plus proche. */
  point: LngLat;
}

/** Retrouve où l'on est sur l'itinéraire à partir d'une position GPS. */
export class RouteMatcher {
  private x: Float64Array;
  private y: Float64Array;
  private kx: number;
  private lon0: number;
  private lat0: number;
  readonly total: number;

  constructor(
    readonly coords: LngLat[],
    /** Distance cumulée depuis le départ pour chaque point (m). */
    readonly dist: number[],
  ) {
    const n = coords.length;
    this.lon0 = coords[0][0];
    this.lat0 = coords[0][1];
    // Projection plane locale : largement assez juste à l'échelle d'une sortie
    let [south, north] = [90, -90];
    for (const c of coords) {
      if (c[1] < south) south = c[1];
      if (c[1] > north) north = c[1];
    }
    const midLat = (south + north) / 2;
    this.kx = Math.cos((midLat * Math.PI) / 180);
    this.x = new Float64Array(n);
    this.y = new Float64Array(n);
    for (let i = 0; i < n; i++) [this.x[i], this.y[i]] = this.project(coords[i]);
    this.total = dist[n - 1] ?? 0;
  }

  private project(p: LngLat): [number, number] {
    return [(p[0] - this.lon0) * this.kx * M_PER_DEG_LON, (p[1] - this.lat0) * M_PER_DEG_LAT];
  }

  /** Dernière portion qui commence avant `m`. */
  private segAt(m: number): number {
    let lo = 0;
    let hi = this.dist.length - 2;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (this.dist[mid] <= m) lo = mid;
      else hi = mid - 1;
    }
    return Math.max(0, lo);
  }

  /** Portion la plus proche parmi celles d'indices [i0, i1] : (distance², portion, position dessus). */
  private scan(px: number, py: number, i0: number, i1: number): [number, number, number] | null {
    let best: [number, number, number] | null = null;
    for (let i = i0; i <= i1; i++) {
      const dx = this.x[i + 1] - this.x[i];
      const dy = this.y[i + 1] - this.y[i];
      const l2 = dx * dx + dy * dy;
      const t = l2 > 0 ? Math.min(1, Math.max(0, ((px - this.x[i]) * dx + (py - this.y[i]) * dy) / l2)) : 0;
      const ex = this.x[i] + dx * t - px;
      const ey = this.y[i] + dy * t - py;
      const d2 = ex * ex + ey * ey;
      if (!best || d2 < best[0]) best = [d2, i, t];
    }
    return best;
  }

  /**
   * Où est-on sur l'itinéraire ? `hint` : distance déjà parcourue à la position précédente.
   * On cherche d'abord juste devant (l'itinéraire peut se croiser ou revenir sur ses pas),
   * puis partout si l'on s'en est écarté.
   */
  locate(p: LngLat, hint: number | null): Located {
    const [px, py] = this.project(p);
    const last = this.coords.length - 2;
    let best: [number, number, number] | null = null;
    if (hint !== null) {
      const near = this.scan(px, py, this.segAt(hint - 60), this.segAt(hint + 500));
      if (near && near[0] <= 30 * 30) best = near;
      else {
        const all = this.scan(px, py, 0, last);
        // À écart comparable, on reste sur la suite logique du parcours
        best = near && all && Math.sqrt(near[0]) <= Math.sqrt(all[0]) + 20 ? near : all;
      }
    } else best = this.scan(px, py, 0, last);
    if (!best) return { m: 0, off: Infinity, point: this.coords[0] };
    const [d2, i, t] = best;
    const a = this.coords[i];
    const b = this.coords[i + 1];
    return {
      m: this.dist[i] + (this.dist[i + 1] - this.dist[i]) * t,
      off: Math.sqrt(d2),
      point: [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t],
    };
  }

  /** Point de l'itinéraire à `m` mètres du départ. */
  pointAt(m: number): LngLat {
    const d = Math.min(this.total, Math.max(0, m));
    const i = this.segAt(d);
    const span = this.dist[i + 1] - this.dist[i];
    const t = span > 0 ? Math.min(1, (d - this.dist[i]) / span) : 0;
    const a = this.coords[i];
    const b = this.coords[i + 1];
    return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
  }

  /** Cap de l'itinéraire à `m` mètres du départ (degrés, 0 = nord), mesuré sur `look` mètres. */
  bearingAt(m: number, look = 30): number {
    const from = Math.min(m, Math.max(0, this.total - look));
    return bearing(this.pointAt(from), this.pointAt(from + look));
  }

  /** Début de l'itinéraire jusqu'à `m` mètres (portion parcourue). */
  sliceTo(m: number): LngLat[] {
    if (m <= 0) return [];
    const i = this.segAt(Math.min(m, this.total));
    return [...this.coords.slice(0, i + 1), this.pointAt(m)];
  }

  /** Distance à vol d'oiseau (m) entre un point et le départ. */
  distanceToStart(p: LngLat): number {
    const [px, py] = this.project(p);
    return Math.hypot(px - this.x[0], py - this.y[0]);
  }
}

/** Cap de `a` vers `b` en degrés (0 = nord, 90 = est). */
export function bearing(a: LngLat, b: LngLat): number {
  const k = Math.cos((((a[1] + b[1]) / 2) * Math.PI) / 180);
  const deg = (Math.atan2((b[0] - a[0]) * k, b[1] - a[1]) * 180) / Math.PI;
  return (deg + 360) % 360;
}

/** État du guidage après une position GPS. */
export interface NavUpdate extends Located {
  onRoute: boolean;
  /** Prochaine direction à annoncer (la dernière est toujours l'arrivée). */
  next: Maneuver | null;
  nextIndex: number;
  /** Distance jusqu'à cette direction (m). */
  toNext: number;
  /** Direction suivante, si elle arrive juste après. */
  then: Maneuver | null;
  remaining: number;
  arrived: boolean;
}

/** Écart (m) au-delà duquel on se considère hors itinéraire, et en deçà duquel on y revient. */
const OFF_ROUTE_M = 40;
const BACK_ON_ROUTE_M = 25;

/** Suit la progression le long de l'itinéraire, position après position. */
export class NavTracker {
  private lastM: number | null = null;
  private offCount = 0;
  private off = false;
  private arrived = false;

  constructor(
    readonly matcher: RouteMatcher,
    readonly maneuvers: Maneuver[],
  ) {}

  update(p: LngLat, accuracy = 10): NavUpdate {
    // Boucle : départ et arrivée au même endroit. Tant qu'on n'a pas bougé, on est au départ.
    const hint = this.lastM ?? (this.matcher.distanceToStart(p) < 80 ? 0 : null);
    const loc = this.matcher.locate(p, hint);
    // Un GPS imprécis ne doit pas déclencher l'alerte : on élargit la tolérance (sans excès)
    const tol = Math.max(OFF_ROUTE_M, Math.min(accuracy * 1.5, 90));
    if (loc.off > tol) this.offCount++;
    else this.offCount = 0;
    if (!this.off && this.offCount >= 3) this.off = true;
    else if (this.off && loc.off < BACK_ON_ROUTE_M) this.off = false;
    // Très loin de l'itinéraire (on ne l'a pas encore rejoint) : inutile d'attendre trois positions
    if (loc.off > 150) this.off = true;
    if (!this.off) this.lastM = loc.m;

    const total = this.matcher.total;
    const ms = this.maneuvers;
    let i = ms.findIndex((m) => m.atM >= loc.m - 12);
    if (i < 0) i = ms.length - 1;
    const next = ms[i] ?? null;
    const after = ms[i + 1] ?? null;
    const remaining = Math.max(0, total - loc.m);
    if (!this.off && remaining < 30) this.arrived = true;
    return {
      ...loc,
      onRoute: !this.off,
      next,
      nextIndex: i,
      toNext: next ? Math.max(0, next.atM - loc.m) : remaining,
      then: next && after && next.kind !== "arrive" && after.atM - next.atM < 150 ? after : null,
      remaining,
      arrived: this.arrived,
    };
  }
}

// --- Dire et montrer une direction -------------------------------------------------------------------

const TEXT: Record<Exclude<ManeuverKind, "roundabout">, string> = {
  slightLeft: "Serre à gauche",
  left: "Tourne à gauche",
  sharpLeft: "Virage serré à gauche",
  slightRight: "Serre à droite",
  right: "Tourne à droite",
  sharpRight: "Virage serré à droite",
  uTurn: "Fais demi-tour",
  arrive: "Arrivée",
};

const ordinal = (n: number) => (n === 1 ? "1re" : `${n}e`);
const ordinalSpoken = (n: number) =>
  ["première", "deuxième", "troisième", "quatrième", "cinquième", "sixième", "septième", "huitième"][n - 1] ?? `${n}e`;

export function maneuverText(m: Maneuver): string {
  if (m.kind === "roundabout") return m.exit ? `Rond-point : ${ordinal(m.exit)} sortie` : "Prends le rond-point";
  return TEXT[m.kind];
}

/** Distance pour l'affichage : « 1,2 km », « 350 m », « 40 m ». */
export function navDistance(m: number): string {
  if (m >= 10_000) return `${Math.round(m / 1000)} km`;
  if (m >= 1000) return `${(m / 1000).toFixed(1).replace(".", ",")} km`;
  if (m >= 100) return `${Math.round(m / 10) * 10} m`;
  return `${Math.max(5, Math.round(m / 5) * 5)} m`;
}

/** Phrase à prononcer ; `inM` : distance annoncée, ou null pour « maintenant ». */
export function maneuverSpeech(m: Maneuver, inM: number | null): string {
  let what: string;
  if (m.kind === "roundabout") what = m.exit ? `au rond-point, prends la ${ordinalSpoken(m.exit)} sortie` : "prends le rond-point";
  else if (m.kind === "arrive") what = "tu arrives";
  else what = TEXT[m.kind].toLowerCase();
  if (inM === null) return what.charAt(0).toUpperCase() + what.slice(1);
  const d = inM >= 1000 ? `${(inM / 1000).toFixed(1).replace(".", ",")} kilomètres` : `${Math.round(inM / 10) * 10} mètres`;
  return `Dans ${d}, ${what}`;
}

/** Angle dessiné pour chaque type de virage (degrés, négatif à gauche). */
const ICON_ANGLE: Partial<Record<ManeuverKind, number>> = {
  slightLeft: -40,
  left: -90,
  sharpLeft: -135,
  slightRight: 40,
  right: 90,
  sharpRight: 135,
};

const f = (v: number) => v.toFixed(1);

/** Pointe de flèche au bout d'un trait arrivant en (x, y) dans la direction `a` (radians, 0 = haut). */
function head(x: number, y: number, a: number, size = 8): string {
  const p = (da: number) => `${f(x + size * Math.sin(a + da))} ${f(y - size * Math.cos(a + da))}`;
  return `M${p(2.55)}L${f(x)} ${f(y)}L${p(-2.55)}`;
}

/** Pictogramme SVG d'une direction (trait dessiné en `currentColor`). */
export function maneuverIcon(m: Maneuver): string {
  let d: string;
  if (m.kind === "arrive") d = "M15 43V8M15 10h19l-5 7 5 7H15";
  else if (m.kind === "uTurn") d = `M32 43V19a8.5 8.5 0 0 0-17 0v12${head(15, 31, Math.PI)}`;
  else if (m.kind === "roundabout") {
    // Anneau, entrée par le bas, sortie dans la direction réelle
    const a = (Math.max(-150, Math.min(150, m.angle)) * Math.PI) / 180;
    const [cx, cy, r, len] = [24, 25, 7.5, 19];
    const ex = cx + len * Math.sin(a);
    const ey = cy - len * Math.cos(a);
    d =
      `M${cx} 44V${cy + r}M${f(cx + r)} ${cy}a${r} ${r} 0 1 0 ${-2 * r} 0a${r} ${r} 0 1 0 ${2 * r} 0` +
      `M${f(cx + r * Math.sin(a))} ${f(cy - r * Math.cos(a))}L${f(ex)} ${f(ey)}${head(ex, ey, a, 7)}`;
  } else {
    const a = ((ICON_ANGLE[m.kind] ?? 0) * Math.PI) / 180;
    const [cx, cy, len] = [24, 28, 16];
    const ex = cx + len * Math.sin(a);
    const ey = cy - len * Math.cos(a);
    d = `M${cx} 44V${cy}L${f(ex)} ${f(ey)}${head(ex, ey, a)}`;
  }
  return `<svg viewBox="0 0 48 48" aria-hidden="true"><path d="${d}" /></svg>`;
}

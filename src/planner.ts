import { invoke } from "@tauri-apps/api/core";
import maplibregl from "maplibre-gl";

export type Surface = "Paved" | "Assumed" | "Unpaved";
export type RoadClass =
  | "Trunk" | "Primary" | "Secondary" | "Tertiary" | "Minor"
  | "Residential" | "Service" | "Cycleway" | "Track" | "Path";

/** Miroir de `goride_core::router::Leg`. */
export interface Leg {
  coords: [number, number][];
  surface: Surface[];
  class: RoadClass[];
  lengthM: number;
}

type LngLat = [number, number];

export type Profile = "Road" | "Triathlon" | "Gravel";

/** Miroir de `goride_core::cost::RouteOptions`. */
export interface RouteOptions {
  profile: Profile;
  /** −1 = le plus plat, 0 = neutre, +1 = les bosses */
  hills: number;
  /** 0 = indifférent aux grands axes, 1 = les fuir */
  traffic: number;
  /** −1 = éviter les pistes cyclables, +1 = les préférer */
  cycleways: number;
  /** Pente max (%), 0 = pas de limite */
  maxGrade: number;
}

interface Callbacks {
  /** Les étapes ont changé (`final` = action terminée, pas un glisser en cours). */
  onChange: (legs: Leg[], final: boolean) => void;
  onError: (msg: string) => void;
  /**
   * Vérifie que des points sont dans le réseau routier chargé, et charge la région sinon.
   * `keep` : points déjà tracés, à garder calculables. Faux si on ne peut pas tracer là.
   */
  ensure?: (points: LngLat[], keep: LngLat[]) => Promise<boolean>;
}

const wps_equal = (a?: LngLat, b?: LngLat) => !!a && !!b && a[0] === b[0] && a[1] === b[1];
const key = (a: LngLat, b: LngLat) => `${a[0]},${a[1]}|${b[0]},${b[1]}`;
/** Distance approchée (degrés corrigés de la latitude) : suffisante pour comparer. */
const flatDist = (a: LngLat, b: LngLat) => Math.hypot((a[0] - b[0]) * Math.cos((a[1] * Math.PI) / 180), a[1] - b[1]);
/** Retire les points identiques consécutifs (une étape de longueur nulle n'a pas de sens). */
const dedupe = (wps: LngLat[]) => wps.filter((p, i) => i === 0 || !wps_equal(p, wps[i - 1]));

/**
 * Tracé manuel (F2.1 à F2.4) : points de passage accrochés à la route,
 * étapes recalculées par le moteur Rust, annuler / rétablir illimités.
 */
export class Planner {
  private waypoints: LngLat[] = [];
  private markers: maplibregl.Marker[] = [];
  private legs = new Map<string, Leg>();
  private past: LngLat[][] = [];
  private future: LngLat[][] = [];
  private dragBusy = false;
  private dragPending: (() => void) | null = null;

  constructor(
    private map: maplibregl.Map,
    private cb: Callbacks,
    private options: () => RouteOptions,
  ) {}

  private optKey() {
    return JSON.stringify(this.options());
  }

  get count() {
    return this.waypoints.length;
  }
  get canUndo() {
    return this.past.length > 0;
  }
  get canRedo() {
    return this.future.length > 0;
  }
  /** Points de passage dans l'ordre (copie). */
  get points(): LngLat[] {
    return [...this.waypoints];
  }

  /** Déplace le point `from` à la position `to` dans la liste (réorganisation manuelle). */
  async move(from: number, to: number) {
    const n = this.waypoints.length;
    if (from === to || from < 0 || from >= n || to < 0 || to >= n) return;
    const wps = [...this.waypoints];
    const [p] = wps.splice(from, 1);
    wps.splice(to, 0, p);
    await this.commit(wps);
  }

  /** Inverse le sens de l'itinéraire. */
  async reverse() {
    if (this.waypoints.length < 2) return;
    await this.commit([...this.waypoints].reverse());
  }

  /** Ajoute un point de passage à la fin de l'itinéraire. */
  async add(point: LngLat) {
    const snapped = await this.snap(point);
    if (!snapped) return;
    await this.commit([...this.waypoints, snapped]);
  }

  /** Insère un point de passage dans l'étape `legIndex` (clic sur le tracé). */
  async insert(legIndex: number, point: LngLat) {
    const snapped = await this.snap(point);
    if (!snapped) return;
    const wps = [...this.waypoints];
    wps.splice(legIndex + 1, 0, snapped);
    await this.commit(wps);
  }

  /** Insère un point à la position `index` (0 = nouveau départ, `count` = nouvelle arrivée). */
  async insertAt(index: number, point: LngLat): Promise<boolean> {
    const snapped = await this.snap(point);
    if (!snapped) return false;
    const wps = [...this.waypoints];
    wps.splice(Math.max(0, Math.min(index, wps.length)), 0, snapped);
    return this.commit(dedupe(wps));
  }

  /**
   * Ajoute un point là où il allonge le moins l'itinéraire : entre deux points
   * existants, ou à la fin si c'est plus court (jamais avant le départ).
   */
  async addBest(point: LngLat): Promise<boolean> {
    const snapped = await this.snap(point);
    if (!snapped) return false;
    const wps = [...this.waypoints];
    const n = wps.length;
    let at = n;
    if (n >= 2) {
      const closed = wps_equal(wps[0], wps[n - 1]);
      let best = closed ? Infinity : flatDist(wps[n - 1], snapped);
      for (let i = 0; i + 1 < n; i++) {
        const extra = flatDist(wps[i], snapped) + flatDist(snapped, wps[i + 1]) - flatDist(wps[i], wps[i + 1]);
        if (extra < best) {
          best = extra;
          at = i + 1;
        }
      }
    }
    wps.splice(at, 0, snapped);
    return this.commit(dedupe(wps));
  }

  /**
   * Détour : aller-retour vers le point depuis l'endroit le plus proche du tracé,
   * puis reprise de l'itinéraire là où on l'a quitté.
   */
  async detour(point: LngLat): Promise<boolean> {
    const legs = this.currentLegs(this.waypoints);
    if (this.waypoints.length < 2 || legs.length !== this.waypoints.length - 1) return false;
    const snapped = await this.snap(point);
    if (!snapped) return false;
    // Point du tracé le plus proche du lieu (et l'étape qui le contient)
    let legIndex = 0;
    let from: LngLat = this.waypoints[0];
    let bestD = Infinity;
    legs.forEach((leg, li) => {
      for (const c of leg.coords) {
        const d = flatDist(c, snapped);
        if (d < bestD) {
          bestD = d;
          legIndex = li;
          from = c;
        }
      }
    });
    const wps = [...this.waypoints];
    wps.splice(legIndex + 1, 0, from, snapped, from);
    return this.commit(dedupe(wps));
  }

  /** Remplace tous les points (itinéraire rouvert) ; faux si impossible à recalculer. */
  async setPoints(wps: LngLat[]): Promise<boolean> {
    if (this.cb.ensure && !(await this.cb.ensure(wps, []))) return false;
    return this.commit(dedupe(wps));
  }

  async remove(i: number) {
    const wps = this.waypoints.filter((_, k) => k !== i);
    await this.commit(wps);
  }

  async clear() {
    if (this.waypoints.length === 0) return;
    await this.commit([]);
  }

  async undo() {
    const prev = this.past.pop();
    if (!prev) return;
    this.future.push(this.waypoints);
    await this.apply(prev);
  }

  async redo() {
    const next = this.future.pop();
    if (!next) return;
    this.past.push(this.waypoints);
    await this.apply(next);
  }

  /** Recalcule l'affichage (après un changement de fond de carte par exemple). */
  refresh() {
    this.cb.onChange(this.currentLegs(this.waypoints), true);
  }

  /** Remplace l'itinéraire par un tracé déjà calculé (boucle choisie), modifiable ensuite. */
  load(wps: LngLat[], legs: Leg[]) {
    const ok = this.optKey();
    for (let i = 0; i + 1 < wps.length; i++) {
      if (legs[i]) this.legs.set(ok + key(wps[i], wps[i + 1]), legs[i]);
    }
    this.past.push(this.waypoints);
    this.future = [];
    this.setWaypoints(wps);
    this.cb.onChange(this.currentLegs(wps), true);
  }

  /** Premier point de passage (départ), s'il y en a un. */
  get start(): LngLat | null {
    return this.waypoints[0] ?? null;
  }

  /** Les réglages ont changé : recalcule tout l'itinéraire. Faux si impossible avec ces réglages. */
  async recompute(): Promise<boolean> {
    if (this.waypoints.length < 2) return true;
    const ok = await this.ensureLegs(this.waypoints);
    if (ok) this.cb.onChange(this.currentLegs(this.waypoints), true);
    return ok;
  }

  // --- interne -----------------------------------------------------------------

  private async snap(p: LngLat): Promise<LngLat | null> {
    if (this.cb.ensure && !(await this.cb.ensure([p], this.waypoints))) return null;
    try {
      return await invoke<LngLat>("snap_point", { point: p, options: this.options() });
    } catch (err) {
      this.cb.onError(String(err));
      return null;
    }
  }

  /** Nouvel état validé : enregistré dans l'historique s'il est calculable. */
  private async commit(wps: LngLat[]): Promise<boolean> {
    const ok = await this.ensureLegs(wps);
    if (!ok) return false;
    this.past.push(this.waypoints);
    this.future = [];
    this.setWaypoints(wps);
    this.cb.onChange(this.currentLegs(wps), true);
    return true;
  }

  private async apply(wps: LngLat[]) {
    await this.ensureLegs(wps);
    this.setWaypoints(wps);
    this.cb.onChange(this.currentLegs(wps), true);
  }

  /** Calcule les étapes manquantes (en parallèle). Faux si l'une est impossible. */
  private async ensureLegs(wps: LngLat[], quiet = false): Promise<boolean> {
    const todo: Promise<void>[] = [];
    const options = this.options();
    const ok = this.optKey();
    for (let i = 0; i + 1 < wps.length; i++) {
      const k = ok + key(wps[i], wps[i + 1]);
      if (this.legs.has(k)) continue;
      todo.push(
        invoke<Leg>("route_leg", { from: wps[i], to: wps[i + 1], options }).then((leg) => {
          this.legs.set(k, leg);
        }),
      );
    }
    try {
      await Promise.all(todo);
      return true;
    } catch (err) {
      if (!quiet) this.cb.onError(String(err));
      return false;
    }
  }

  private currentLegs(wps: LngLat[]): Leg[] {
    const out: Leg[] = [];
    const ok = this.optKey();
    for (let i = 0; i + 1 < wps.length; i++) {
      const leg = this.legs.get(ok + key(wps[i], wps[i + 1]));
      if (leg) out.push(leg);
    }
    return out;
  }

  private setWaypoints(wps: LngLat[]) {
    this.waypoints = wps;
    this.markers.forEach((m) => m.remove());
    this.markers = wps.map((p, i) => this.makeMarker(p, i, wps.length));
    if (this.legs.size > 1500) this.prune();
  }

  /** Vide le cache des étapes calculées pendant les glisser, en gardant celles de l'historique. */
  private prune() {
    const keep = new Set<string>();
    const ok = this.optKey();
    for (const wps of [this.waypoints, ...this.past.slice(-50), ...this.future]) {
      for (let i = 0; i + 1 < wps.length; i++) keep.add(ok + key(wps[i], wps[i + 1]));
    }
    for (const k of [...this.legs.keys()]) if (!keep.has(k)) this.legs.delete(k);
  }

  private makeMarker(p: LngLat, i: number, n: number): maplibregl.Marker {
    const el = document.createElement("div");
    const closed = n > 2 && wps_equal(this.waypoints[0], this.waypoints[n - 1]);
    const role = i === 0 ? (closed ? "start-end" : "start") : i === n - 1 ? (closed ? "loop-end" : "end") : "via";
    el.className = `wp ${role}`;
    el.textContent = role === "loop-end" ? "" : String(i + 1);
    el.title = "Glisser pour déplacer · clic droit pour supprimer";
    el.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.remove(i);
    });
    el.addEventListener("click", (e) => e.stopPropagation());

    const marker = new maplibregl.Marker({ element: el, draggable: true }).setLngLat(p).addTo(this.map);

    // Pendant le glisser : recalcul en continu des deux étapes voisines (une requête à la fois)
    marker.on("drag", () => {
      const ll = marker.getLngLat();
      this.dragPending = () => this.previewDrag(i, [ll.lng, ll.lat]);
      if (!this.dragBusy) this.runDrag();
    });
    marker.on("dragend", async () => {
      this.dragPending = null;
      const ll = marker.getLngLat();
      const snapped = await this.snap([ll.lng, ll.lat]);
      const wps = [...this.waypoints];
      if (snapped) wps[i] = snapped;
      const ok = snapped && (await this.ensureLegs(wps));
      if (ok) {
        this.past.push(this.waypoints);
        this.future = [];
        this.setWaypoints(wps);
        this.cb.onChange(this.currentLegs(wps), true);
      } else {
        // Déplacement impossible : on remet le point à sa place
        this.setWaypoints(this.waypoints);
        this.cb.onChange(this.currentLegs(this.waypoints), true);
      }
    });
    return marker;
  }

  private async runDrag() {
    while (this.dragPending) {
      const job = this.dragPending;
      this.dragPending = null;
      this.dragBusy = true;
      await job();
      this.dragBusy = false;
    }
  }

  private async previewDrag(i: number, p: LngLat) {
    const wps = [...this.waypoints];
    wps[i] = p;
    if (await this.ensureLegs(wps, true)) {
      this.cb.onChange(this.currentLegs(wps), false);
    }
  }
}

/** Met bout à bout les étapes en une seule ligne (sans doubler les points de jonction). */
export function joinLegs(legs: Leg[]): Leg {
  const out: Leg = { coords: [], surface: [], class: [], lengthM: 0 };
  for (const leg of legs) {
    if (leg.coords.length === 0) continue;
    let start = 0;
    const last = out.coords[out.coords.length - 1];
    if (last) {
      const first = leg.coords[0];
      if (last[0] === first[0] && last[1] === first[1]) start = 1;
      else {
        out.surface.push(leg.surface[0] ?? "Assumed");
        out.class.push(leg.class[0] ?? "Minor");
      }
    }
    for (let k = start; k < leg.coords.length; k++) out.coords.push(leg.coords[k]);
    out.surface.push(...leg.surface);
    out.class.push(...leg.class);
    out.lengthM += leg.lengthM;
  }
  return out;
}

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { ask, open, save } from "@tauri-apps/plugin-dialog";
import "@fontsource-variable/inter-tight";
import maplibregl from "maplibre-gl";
import "./styles.css";
import type { BasemapId } from "./basemaps";
import { duration, km, meters } from "./format";
import { Coach, TERRAIN_LABEL, fmtDuration, type ToolResult, type Workout, type WorkoutInfo } from "./coach";
import { runIntro } from "./intro";
import { LOOP_COLORS, TrackMap } from "./map";
import { joinLegs, Planner, type Leg, type RoadClass, type RouteOptions, type Surface } from "./planner";
import { placeName, reverseInfo, searchPlaces, type Place } from "./places";
import { ElevationProfile } from "./profile";
import { progressHtml, type Period, type Progress } from "./progress_view";
import {
  METRICS,
  RideChart,
  rideDetailHtml,
  rideListItem,
  type ActivityDetail,
  type ActivitySummary,
  type Athlete,
  type Metric,
} from "./rides";
import type { Track } from "./types";

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const inTauri = "__TAURI_INTERNALS__" in window;

interface Share<T> {
  kind: T;
  meters: number;
}
interface RouteSummary {
  track: Track;
  surfaces: Share<Surface>[];
  classes: Share<RoadClass>[];
}
interface NetworkStatus {
  loaded: boolean;
  busy: boolean;
  region: string | null;
  segments: number;
  bbox: [number, number, number, number] | null;
  elevation: boolean;
  notice: string | null;
}

/** Ce qui est affiché : une trace importée ou un itinéraire tracé. */
let current: { kind: "import" | "route"; track: Track; name: string } | null = null;
let network: NetworkStatus = {
  loaded: false,
  busy: false,
  region: null,
  segments: 0,
  bbox: null,
  elevation: false,
  notice: null,
};

// --- Réglages du tracé (gardés d'une session à l'autre) ----------------------

const DEFAULT_OPTIONS: RouteOptions = { profile: "Road", hills: 0, traffic: 0.5, cycleways: 0, maxGrade: 0 };
const OPTIONS_KEY = "goride.options";

function loadOptions(): RouteOptions {
  try {
    const raw = localStorage.getItem(OPTIONS_KEY);
    if (raw) return { ...DEFAULT_OPTIONS, ...JSON.parse(raw) };
  } catch {
    /* stockage indisponible : réglages par défaut */
  }
  return { ...DEFAULT_OPTIONS };
}

let options: RouteOptions = loadOptions();

// Carte et profil se pilotent mutuellement : survoler l'un place le curseur sur l'autre.
const trackMap = new TrackMap($("map"), (i) => onMapHover(i));
const profile = new ElevationProfile($<HTMLCanvasElement>("profile-canvas"), $("profile-tip"), (i) =>
  trackMap.setCursor(i),
);

// --- Trace importée ----------------------------------------------------------

async function loadFromPath(path: string) {
  try {
    const t0 = performance.now();
    const track = await invoke<Track>("load_track", { path });
    console.info(`GPX chargé en ${(performance.now() - t0).toFixed(0)} ms (${track.stats.pointCount} points)`);
    setDrawing(false);
    await planner.clear();
    const fileName = path.split(/[\\/]/).pop()!.replace(/\.gpx$/i, "");
    current = { kind: "import", track, name: track.name ?? fileName };
    currentFile = null;
    savedSig = null;
    showStats(track, current.name, null);
    profile.setTrack(track);
    profile.setMarks([]);
    trackMap.setTrack(track);
  } catch (err) {
    console.error("load_track a échoué :", err);
    toast(String(err));
  }
}

async function pickFile() {
  if (!inTauri) return toast("L'import fonctionne dans l'application (npm run tauri dev), pas dans un navigateur.");
  try {
    const path = await open({ multiple: false, directory: false, filters: [{ name: "Trace GPX", extensions: ["gpx", "GPX"] }] });
    if (typeof path === "string") await loadFromPath(path);
  } catch (err) {
    toast(`Ouverture du sélecteur impossible : ${err}`);
  }
}

// --- Tracé d'itinéraire ------------------------------------------------------

let summaryTimer = 0;
/** Nom donné à l'itinéraire tracé (« Boucle de 50 km » quand il vient d'une boucle proposée). */
let routeName = "Nouvel itinéraire";
let summarySeq = 0;
/** Étapes affichées (une par paire de points de passage consécutifs). */
let lastLegs: Leg[] = [];

const planner = new Planner(trackMap.map, {
  onChange(legs, final) {
    trackMap.setRoute(legs);
    updateDrawTools();
    lastLegs = legs;
    if (final) renderPoints();
    if (legs.length === 0) {
      routeName = "Nouvel itinéraire";
      currentFile = null;
      savedSig = null;
      if (current?.kind === "route") hideRoute();
      updateEmptyCard();
      return;
    }
    // Distance tout de suite, altitude et bilan complet un instant après (pas pendant un glisser)
    const joined = joinLegs(legs);
    $("st-dist").textContent = km(joined.lengthM);
    if (final) {
      clearTimeout(summaryTimer);
      summaryTimer = window.setTimeout(() => summarize(joined), 120);
    }
    updateEmptyCard();
    updateSaveState();
  },
  onError: (msg) => toast(msg),
  ensure: (points, keep) => ensureCoverage(points, keep),
}, () => options);

async function summarize(joined: Leg) {
  const seq = ++summarySeq;
  try {
    const s = await invoke<RouteSummary>("summarize_route", {
      coords: joined.coords,
      surface: joined.surface,
      class: joined.class,
    });
    if (seq !== summarySeq || planner.count < 2) return; // une version plus récente est en route
    current = { kind: "route", track: s.track, name: routeName };
    trackMap.setRouteTrack(s.track);
    showStats(s.track, current.name, s);
    profile.setTrack(s.track);
    profile.setMarks(waypointDistances(s.track.stats.distanceM));
  } catch (err) {
    toast(`Bilan de l'itinéraire impossible : ${err}`);
  }
}

function hideRoute() {
  current = null;
  trackMap.setRouteTrack(null);
  profile.setTrack(null);
  $("stats").classList.add("hidden");
  $("profile").classList.add("hidden");
}

function setDrawing(on: boolean) {
  if (on && loopMode) closeLoop();
  // Sans réseau chargé, on peut tracer quand même : la région se charge au premier clic.
  if (on && network.busy && !network.loaded) {
    toast("Le réseau routier est en cours de préparation, encore un instant…");
    return;
  }
  if (on && current?.kind === "import") {
    current = null;
    trackMap.clearTrack();
    hideRoute();
  }
  trackMap.setDrawing(on);
  $("btn-draw").classList.toggle("active", on);
  $("draw-tools").classList.toggle("hidden", !on && planner.count === 0);
  updateEmptyCard();
}

function updateDrawTools() {
  $<HTMLButtonElement>("btn-undo").disabled = !planner.canUndo;
  $<HTMLButtonElement>("btn-redo").disabled = !planner.canRedo;
  $<HTMLButtonElement>("btn-clear").disabled = planner.count === 0;
}

/** Aide contextuelle au-dessus de la barre d'outils, seulement pendant le tracé (rien au lancement). */
function updateEmptyCard() {
  const show = trackMap.drawing && planner.count < 2 && !loopMode;
  $("empty").classList.toggle("hidden", !show);
  if (!show) return;
  if (trackMap.drawing) {
    $("empty-title").textContent = planner.count === 0 ? "Clique sur la carte pour placer le départ" : "Clique pour placer le point suivant";
    $("empty-text").textContent =
      "Glisse un point pour le déplacer, clic droit pour le supprimer, clic sur le tracé pour en insérer un.";
  }
}

trackMap.onMapClick = (p) => planner.add(p);
trackMap.onRouteClick = (leg, p) => planner.insert(leg, p);

// --- Panneau de réglages -------------------------------------------------------

/** Valeurs proposées par les boutons « Dénivelé » et « Grands axes ». */
const HILLS_STEPS = [-1, 0, 1];
const TRAFFIC_STEPS = [0, 0.5, 1];
const PROFILE_LABEL: Record<RouteOptions["profile"], string> = { Road: "Route", Triathlon: "Triathlon", Gravel: "Gravel" };
/** Valeur la plus proche parmi les boutons (anciens réglages enregistrés au curseur). */
const nearest = (steps: number[], v: number) => steps.reduce((a, b) => (Math.abs(b - v) < Math.abs(a - v) ? b : a));

function setSegmented(id: string, value: string) {
  $(id)
    .querySelectorAll<HTMLButtonElement>("button")
    .forEach((b) => b.classList.toggle("active", b.dataset.v === value));
}

/** Affiche les réglages courants dans le panneau. */
function renderOptions() {
  setSegmented("opt-profile", options.profile);
  setSegmented("opt-cycle", String(options.cycleways));
  setSegmented("opt-hills", String(nearest(HILLS_STEPS, options.hills)));
  setSegmented("opt-traffic", String(nearest(TRAFFIC_STEPS, options.traffic)));
  $<HTMLSelectElement>("opt-grade").value = String(options.maxGrade);
  $("btn-settings-label").textContent = PROFILE_LABEL[options.profile];
  // En gravel, les grands axes sont toujours évités (réglage indicatif seulement)
  const gravel = options.profile === "Gravel";
  $("opt-traffic").classList.toggle("disabled", gravel);
  $("opt-traffic").querySelectorAll("button").forEach((b) => (b.disabled = gravel));
}

/** Applique un changement de réglage : recalcul du tracé, retour en arrière si impossible. */
async function changeOptions(patch: Partial<RouteOptions>) {
  const previous = options;
  options = { ...options, ...patch };
  renderOptions();
  const panel = $("settings");
  panel.classList.add("busy");
  const ok = await planner.recompute();
  panel.classList.remove("busy");
  if (!ok) {
    options = previous;
    renderOptions();
    return;
  }
  try {
    localStorage.setItem(OPTIONS_KEY, JSON.stringify(options));
  } catch {
    /* rien */
  }
}

$("opt-profile").addEventListener("click", (e) => {
  const v = (e.target as HTMLElement).closest("button")?.dataset.v;
  if (v && v !== options.profile) changeOptions({ profile: v as RouteOptions["profile"] });
});
$("opt-cycle").addEventListener("click", (e) => {
  const v = (e.target as HTMLElement).closest("button")?.dataset.v;
  if (v !== undefined && Number(v) !== options.cycleways) changeOptions({ cycleways: Number(v) });
});
$("opt-hills").addEventListener("click", (e) => {
  const v = (e.target as HTMLElement).closest("button")?.dataset.v;
  if (v !== undefined && Number(v) !== options.hills) changeOptions({ hills: Number(v) });
});
$("opt-traffic").addEventListener("click", (e) => {
  const v = (e.target as HTMLElement).closest("button")?.dataset.v;
  if (v !== undefined && Number(v) !== options.traffic) changeOptions({ traffic: Number(v) });
});
$("opt-grade").addEventListener("change", (e) => changeOptions({ maxGrade: Number((e.target as HTMLSelectElement).value) }));

/** Réglages en fenêtre au-dessus de la barre d'outils. */
function setSettingsOpen(open: boolean) {
  $("settings").classList.toggle("hidden", !open);
  $("btn-settings").classList.toggle("open", open);
  $("btn-settings").setAttribute("aria-expanded", String(open));
}
$("btn-settings").addEventListener("click", () => setSettingsOpen($("settings").classList.contains("hidden")));
$("settings-toggle").addEventListener("click", () => setSettingsOpen(false));
renderOptions();

// --- Boucles automatiques ---------------------------------------------------

interface LoopProposal {
  id: number;
  waypoints: [number, number][];
  legs: Leg[];
  lengthM: number;
  ascentM: number;
  retrace: number;
  /** Portion clé pour la séance (boucles de séance seulement). */
  key?: KeySection | null;
}
interface KeySection {
  terrain: "climb" | "flat" | "any";
  startM: number;
  endM: number;
  lengthM: number;
  gainM: number;
  gradePct: number;
  estS: number;
  fit: number;
  coords: [number, number][];
}
/** Séance pour laquelle on cherche une boucle (bouton « Itinéraire pour cette séance »). */
let loopWorkout: { workout: Workout; info: WorkoutInfo } | null = null;

let loopMode = false;
let loopStart: [number, number] | null = null;
let loops: LoopProposal[] = [];
let loopSelected: number | null = null;
/** Boucles déjà montrées pour ce départ et cette distance (à ne pas reproposer). */
let loopShown: number[] = [];
let loopSeed = 0;

function openLoop() {
  closeLibrary();
  if (network.busy && !network.loaded) {
    toast("Le réseau routier est en cours de préparation, encore un instant…");
    return;
  }
  if (trackMap.drawing) setDrawing(false);
  loopMode = true;
  $("loop-panel").classList.remove("hidden");
  $("stats").classList.add("hidden");
  $("btn-loop").classList.add("active");
  trackMap.setLoopPicking(true);
  // Départ proposé : celui de l'itinéraire en cours, s'il y en a un
  const start = loopStart ?? planner.start;
  if (start) setLoopStart(start);
  updateLoopHint();
  updateEmptyCard();
}

function closeLoop() {
  if (!loopMode) return;
  loopMode = false;
  setLoopWorkout(null);
  $("loop-panel").classList.add("hidden");
  $("btn-loop").classList.remove("active");
  trackMap.setLoopPicking(false);
  loops = [];
  loopSelected = null;
  renderLoopList();
  if (current) $("stats").classList.remove("hidden");
  updateEmptyCard();
}

function setLoopStart(p: [number, number]) {
  loopStart = p;
  trackMap.setLoopStart(p);
  loopShown = [];
  loops = [];
  loopSelected = null;
  trackMap.setLoopPreviews([], null);
  renderLoopList();
  updateLoopHint();
}

function updateLoopHint() {
  const hint = $("loop-hint");
  hint.textContent = loopStart
    ? "Départ choisi. Clique ailleurs sur la carte pour le déplacer."
    : "Clique sur la carte pour choisir le départ.";
  hint.classList.toggle("ok", !!loopStart);
  $<HTMLButtonElement>("loop-go").disabled = !loopStart;
}

function loopKm(): number {
  const v = Number($<HTMLInputElement>("loop-km").value);
  return Number.isFinite(v) ? Math.min(300, Math.max(5, v)) : 50;
}

/** Boucles pour une séance : bandeau en haut du panneau, distance conseillée. */
function setLoopWorkout(w: { workout: Workout; info: WorkoutInfo } | null) {
  loopWorkout = w;
  loopShown = [];
  $("loop-workout").classList.toggle("hidden", !w);
  if (w) {
    $("loop-workout-name").textContent = w.workout.name;
    const effort = fmtDuration(Math.max(...w.workout.blocks.flatMap((b) => b.steps.filter((s) => s.kind === "work").map((s) => s.durationS)), 0));
    $("loop-workout-need").textContent =
      w.workout.terrain === "climb"
        ? `Une montée d'au moins ${effort} d'effort`
        : w.workout.terrain === "flat"
          ? `Une portion plate de ${effort} sans relance`
          : `${fmtDuration(w.info.totalS)} · ${TERRAIN_LABEL.any.toLowerCase()}`;
    $<HTMLInputElement>("loop-km").value = String(w.info.suggestedKm);
  }
}

/** Ouvre les boucles adaptées à une séance (depuis le panneau Coach). */
function routeForWorkout(workout: Workout, info: WorkoutInfo) {
  setMode("routes");
  setMenuOpen(true);
  if (!loopMode) openLoop();
  setLoopWorkout({ workout, info });
  loops = [];
  loopSelected = null;
  showLoops();
  if (loopStart) requestLoops(false);
  else toast("Clique sur la carte pour choisir le départ de ta séance.", "ok");
}

/** Calcule les boucles ; renvoie un message d'erreur, ou null si tout va bien. */
async function requestLoops(more: boolean): Promise<string | null> {
  if (!loopStart) return "Aucun départ choisi.";
  if (!(await ensureCoverage([loopStart], []))) return "Le réseau routier de cette région n'est pas chargé.";
  const go = $<HTMLButtonElement>("loop-go");
  const moreBtn = $<HTMLButtonElement>("loop-more");
  if (!more) loopShown = [];
  loopSeed = more ? loopSeed + 1 : Math.floor(Math.random() * 1e9);
  const dir = $<HTMLSelectElement>("loop-dir").value;
  go.disabled = moreBtn.disabled = true;
  const label = go.textContent;
  go.textContent = "Calcul des boucles…";
  try {
    const t0 = performance.now();
    const request = {
      start: loopStart,
      distanceM: loopKm() * 1000,
      tolerance: 0.05,
      count: 5,
      seed: loopSeed,
      headingDeg: dir === "" ? null : Number(dir),
    };
    const res = loopWorkout
      ? await invoke<LoopProposal[]>("workout_loops", { request, options, workout: loopWorkout.workout, exclude: loopShown })
      : await invoke<LoopProposal[]>("loop_proposals", { request, options, exclude: loopShown });
    console.info(`${res.length} boucles en ${Math.round(performance.now() - t0)} ms`);
    loops = res;
    loopShown.push(...res.map((l) => l.id));
    loopSelected = null;
    showLoops();
    trackMap.fit(loops.flatMap((l) => joinLegs(l.legs).coords));
    return loops.length ? null : "Aucune boucle trouvée avec ces réglages.";
  } catch (err) {
    toast(String(err));
    return String(err);
  } finally {
    go.textContent = label;
    go.disabled = moreBtn.disabled = false;
  }
}

function showLoops() {
  trackMap.setLoopPreviews(
    loops.map((l) => joinLegs(l.legs).coords),
    loopSelected,
  );
  trackMap.setLoopKeys(
    loops.map((l) => l.key?.coords ?? []),
    loopSelected,
  );
  renderLoopList();
}

/** Ce que la boucle offre à la séance : la montée ou la portion plate, et si elle suffit. */
function keyText(k: KeySection | null | undefined): string {
  if (!loopWorkout || loopWorkout.workout.terrain === "any") return "";
  if (!k) return `<span class="key warn">${loopWorkout.workout.terrain === "climb" ? "Pas de vraie montée" : "Pas de longue portion plate"}</span>`;
  const at = `au km ${Math.round(k.startM / 1000)}`;
  const est = fmtDuration(Math.max(60, Math.round(k.estS / 60) * 60));
  const enough = k.fit >= 0.85;
  const what =
    k.terrain === "climb"
      ? `Montée de ${km(k.lengthM)} à ${k.gradePct.toFixed(1).replace(".", ",")} % · ≈ ${est} ${at}`
      : `Plat sur ${km(k.lengthM)} · ≈ ${est} ${at}`;
  const note = enough ? "" : k.terrain === "climb" ? " — un peu courte : finis l'effort au sommet" : " — un peu courte : fais demi-tour au bout";
  return `<span class="key${enough ? "" : " warn"}">${what}${note}</span>`;
}

function renderLoopList() {
  const list = $("loop-list");
  list.innerHTML = loops
    .map((l, i) => {
      const parts = [km(l.lengthM)];
      if (network.elevation) parts.push(`D+ ≈ ${meters(Math.round(l.ascentM / 10) * 10)}`);
      if (l.retrace > 0.03) parts.push(`${Math.round(l.retrace * 100)} % aller-retour`);
      return `<li class="loop-item${loopSelected === i ? " selected" : ""}" data-i="${i}">
        <span class="swatch" style="background:${LOOP_COLORS[i % LOOP_COLORS.length]}"></span>
        <span><span class="title">Boucle ${i + 1}</span><br /><span class="meta">${parts.join(" · ")}</span>${keyText(l.key)}</span>
        <button class="btn small${loopSelected === i ? " primary" : " ghost"}" data-choose="${i}">Choisir</button>
      </li>`;
    })
    .join("");
  $("loop-more").classList.toggle("hidden", loops.length === 0);
}

function selectLoop(i: number) {
  loopSelected = loopSelected === i ? null : i;
  showLoops();
}

function chooseLoop(i: number) {
  const l = loops[i];
  if (!l) return;
  routeName = loopWorkout ? `${loopWorkout.workout.name} · ${Math.round(l.lengthM / 1000)} km` : `Boucle de ${Math.round(l.lengthM / 1000)} km`;
  currentFile = null;
  savedSig = null;
  if (current?.kind === "import") {
    current = null;
    trackMap.clearTrack();
  }
  closeLoop();
  planner.load(l.waypoints, l.legs);
  setDrawing(true); // la boucle reste modifiable : glisser un point pour l'ajuster
  trackMap.fit(joinLegs(l.legs).coords);
}

trackMap.onLoopStart = (p) => setLoopStart(p);
trackMap.onLoopClick = (i) => selectLoop(i);
$("btn-loop").addEventListener("click", () => (loopMode ? closeLoop() : openLoop()));
$("loop-close").addEventListener("click", closeLoop);
$("loop-workout-clear").addEventListener("click", () => {
  setLoopWorkout(null);
  loops = [];
  loopSelected = null;
  showLoops();
});
$("loop-go").addEventListener("click", () => requestLoops(false));
$("loop-more").addEventListener("click", () => requestLoops(true));
$("loop-km").addEventListener("change", () => {
  $<HTMLInputElement>("loop-km").value = String(loopKm());
  loopShown = [];
});
$("loop-dir").addEventListener("change", () => (loopShown = []));
$("loop-list").addEventListener("click", (e) => {
  const target = e.target as HTMLElement;
  const choose = target.closest<HTMLElement>("[data-choose]");
  if (choose) return chooseLoop(Number(choose.dataset.choose));
  const item = target.closest<HTMLElement>("[data-i]");
  if (item) selectLoop(Number(item.dataset.i));
});
$("loop-list").addEventListener("dblclick", (e) => {
  const item = (e.target as HTMLElement).closest<HTMLElement>("[data-i]");
  if (item) chooseLoop(Number(item.dataset.i));
});


// --- Sorties (phase 4) ---------------------------------------------------------

type Mode = "routes" | "rides" | "coach";
let mode: Mode = "routes";
let rides: ActivitySummary[] = [];
let rideDetail: ActivityDetail | null = null;
let rideMetric: Metric = "speedKmh";
const ATHLETE_KEY = "goride.athlete";

function loadAthlete(): Athlete {
  try {
    return JSON.parse(localStorage.getItem(ATHLETE_KEY) ?? "{}");
  } catch {
    return {};
  }
}
let athlete: Athlete = loadAthlete();

const coach = new Coach({
  athlete: () => athlete,
  toast: (m, k) => toast(m, k),
  onRouteForWorkout: routeForWorkout,
  runTool: coachTool,
  appState: coachAppState,
  openCoach: () => {
    setMode("coach");
    setMenuOpen(true);
  },
});

/** Index du point de trace le plus proche d'une distance (m). */
function indexAtDist(dist: number[], d: number): number {
  let lo = 0;
  let hi = dist.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (dist[mid] < d) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

const rideChart = new RideChart($<HTMLCanvasElement>("ride-canvas"), $("ride-tip"), (km) => {
  if (!rideDetail || rideDetail.track.coords.length === 0) return;
  trackMap.setCursor(km === null ? null : indexAtDist(rideDetail.track.dist, km * 1000));
});

/** Survol de la trace sur la carte : profil (itinéraires) ou graphique (sorties). */
function onMapHover(i: number | null) {
  if (mode === "rides") {
    rideChart.setCursor(i === null || !rideDetail ? null : rideDetail.track.dist[i] / 1000);
  } else {
    profile.setCursor(i);
  }
}

function rideTrack(d: ActivityDetail): Track {
  const t = d.track;
  return {
    name: d.summary.name,
    coords: t.coords,
    ele: t.ele,
    dist: t.dist,
    stats: {
      distanceM: d.analysis.distanceM,
      ascentM: d.analysis.ascentM,
      descentM: d.analysis.descentM,
      minEleM: d.analysis.eleMin,
      maxEleM: d.analysis.eleMax,
      pointCount: t.coords.length,
      hasElevation: d.analysis.eleMin !== null,
    },
  };
}

function setMode(m: Mode) {
  if (m === mode) return;
  mode = m;
  document.body.classList.toggle("mode-rides", m === "rides");
  document.body.classList.toggle("mode-coach", m === "coach");
  document.querySelectorAll<HTMLButtonElement>("[data-mode]").forEach((b) => b.classList.toggle("active", b.dataset.mode === m));
  trackMap.setCursor(null);
  if (m === "rides" || m === "coach") {
    if (trackMap.drawing) setDrawing(false);
    closeLoop();
    closeLibrary();
    removeSearchPin();
    trackMap.setRoute([]);
    trackMap.clearTrack();
    if (m === "rides") {
      refreshRides();
      if (rideDetail) showRideOnMap();
    } else {
      coach.hideFloat();
      if (inTauri) coach.open();
    }
  } else {
    trackMap.clearTrack();
    planner.refresh();
    if (current?.kind === "import") trackMap.setTrack(current.track);
    else if (current?.kind === "route") trackMap.setRouteTrack(current.track);
  }
}

async function refreshRides(selectId?: number) {
  if (!inTauri) return;
  try {
    rides = await invoke<ActivitySummary[]>("list_activities");
  } catch (err) {
    toast(String(err));
    return;
  }
  renderRideList();
  if (selectId !== undefined) await selectRide(selectId);
}

function renderRideList() {
  $("rides-empty").classList.toggle("hidden", rides.length > 0);
  $("rides-list").innerHTML = rides.map((r) => rideListItem(r, rideDetail?.summary.id === r.id)).join("");
}

async function selectRide(id: number) {
  hideProgress();
  try {
    rideDetail = await invoke<ActivityDetail>("activity_detail", { id, athlete });
  } catch (err) {
    toast(String(err));
    return;
  }
  renderRideList();
  renderRideDetail();
  showRideOnMap();
}

function renderRideDetail() {
  const panel = $("ride-detail");
  const chart = $("ride-chart");
  if (!rideDetail) {
    panel.classList.add("hidden");
    chart.classList.add("hidden");
    return;
  }
  panel.innerHTML = rideDetailHtml(rideDetail, athlete);
  panel.classList.remove("hidden");
  panel.scrollTop = 0;
  if (inTauri) coach.mountRide($("ride-coach"), rideDetail.summary.id);

  // Onglets du graphique : seulement les données présentes dans la sortie
  const available = RideChart.available(rideDetail.analysis.series);
  if (!available.includes(rideMetric)) rideMetric = available[0] ?? "speedKmh";
  $("chart-tabs").innerHTML = available
    .map((m) => `<button data-metric="${m}" class="${m === rideMetric ? "active" : ""}">${METRICS[m].label}</button>`)
    .join("");
  chart.classList.remove("hidden");
  rideChart.setData(rideDetail.analysis.series, rideMetric);
}

/** Affiche la trace de la sortie et cadre la carte dessus. */
function showRideOnMap() {
  if (!rideDetail || rideDetail.track.coords.length < 2) {
    trackMap.clearTrack();
    return;
  }
  trackMap.setTrack(rideTrack(rideDetail));
}

async function importRides(paths: string[], folder?: string) {
  if (paths.length === 0 && !folder) return;
  $("import-menu").classList.add("hidden");
  const status = $("import-status");
  status.textContent = "Import en cours…";
  status.classList.remove("hidden");
  try {
    const r = await invoke<{
      imported: ActivitySummary[];
      duplicates: string[];
      skipped: string[];
      errors: [string, string][];
    }>("import_activities", { paths, folder: folder ?? null });
    const parts: string[] = [];
    const pl = (n: number, s: string) => `${n} ${s}${n > 1 ? "s" : ""}`;
    if (r.imported.length) parts.push(`${pl(r.imported.length, "sortie")} importée${r.imported.length > 1 ? "s" : ""}`);
    if (r.duplicates.length) parts.push(`${r.duplicates.length} déjà présente${r.duplicates.length > 1 ? "s" : ""}`);
    if (r.skipped.length) parts.push(`${r.skipped.length} autre${r.skipped.length > 1 ? "s" : ""} sport${r.skipped.length > 1 ? "s" : ""} ignoré${r.skipped.length > 1 ? "s" : ""}`);
    if (r.errors.length) {
      parts.push(`${pl(r.errors.length, "erreur")}`);
      console.warn("Fichiers non importés :", r.errors);
    }
    const msg = parts.join(" · ") || "Aucun fichier de sortie trouvé.";
    toast(
      r.errors.length && r.errors.length <= 3 ? `${msg} — ${r.errors.map(([f, e]) => `${f} : ${e}`).join(" ; ")}` : msg,
      r.errors.length && !r.imported.length ? "error" : "ok",
    );
    const newest = [...r.imported].sort((a, b) => (b.startTime ?? 0) - (a.startTime ?? 0))[0];
    await refreshRides(newest?.id);
  } catch (err) {
    toast(`Import impossible : ${err}`);
  } finally {
    status.classList.add("hidden");
  }
}

async function pickRides() {
  if (!inTauri) return toast("L'import fonctionne dans l'application, pas dans un navigateur.");
  const sel = await open({
    multiple: true,
    directory: false,
    title: "Sorties à importer",
    filters: [{ name: "Sorties (FIT, GPX, TCX)", extensions: ["fit", "FIT", "gpx", "GPX", "tcx", "TCX", "gz"] }],
  });
  const paths = Array.isArray(sel) ? sel : sel ? [sel] : [];
  await importRides(paths);
}

async function pickRideFolder() {
  if (!inTauri) return;
  const dir = await open({ directory: true, multiple: false, title: "Dossier de sorties (Garmin/Activity ou archive Strava)" });
  if (typeof dir === "string") await importRides([], dir);
}

// --- Progression -----------------------------------------------------------------

let progressData: Progress | null = null;
let progressPeriod: Period = 90;

async function showProgress() {
  rideDetail = null;
  renderRideList();
  renderRideDetail();
  trackMap.clearTrack();
  const panel = $("progress-panel");
  panel.classList.remove("hidden");
  $("progress-body").innerHTML = `<p class="hint">Calcul…</p>`;
  try {
    progressData = await invoke<Progress>("ride_progress", { athlete, tzOffsetMin: -new Date().getTimezoneOffset() });
    renderProgress();
  } catch (err) {
    toast(String(err));
  }
}

function renderProgress() {
  if (!progressData) return;
  $("progress-body").innerHTML = progressHtml(progressData, progressPeriod);
}

function hideProgress() {
  $("progress-panel").classList.add("hidden");
}

$("btn-progress").addEventListener("click", showProgress);
$("progress-close").addEventListener("click", hideProgress);
$("progress-period").addEventListener("click", (e) => {
  const v = (e.target as HTMLElement).closest<HTMLElement>("[data-v]")?.dataset.v;
  if (v === undefined) return;
  progressPeriod = Number(v) as Period;
  $("progress-period")
    .querySelectorAll("button")
    .forEach((b) => b.classList.toggle("active", b.dataset.v === v));
  renderProgress();
});
$("progress-body").addEventListener("click", (e) => {
  const id = (e.target as Element).closest<SVGElement>("[data-ride]")?.dataset.ride;
  if (id) selectRide(Number(id));
});

function renderAthleteForm() {
  const set = (id: string, v: number | null | undefined) => ($<HTMLInputElement>(id).value = v ? String(v) : "");
  set("ath-ftp", athlete.ftp);
  set("ath-hr", athlete.hrMax);
  set("ath-weight", athlete.weight);
}

function readAthleteForm() {
  const get = (id: string) => {
    const v = Number($<HTMLInputElement>(id).value);
    return Number.isFinite(v) && v > 0 ? v : null;
  };
  athlete = { ftp: get("ath-ftp"), hrMax: get("ath-hr"), weight: get("ath-weight") };
  try {
    localStorage.setItem(ATHLETE_KEY, JSON.stringify(athlete));
  } catch {
    /* rien */
  }
  if (rideDetail) selectRide(rideDetail.summary.id);
  else if (!$("progress-panel").classList.contains("hidden")) showProgress();
}

/** Les deux menus (Itinéraires, Sorties) : un clic sur le menu actif le replie ou le déplie. */
function setMenuOpen(open: boolean) {
  document.body.classList.toggle("menu-closed", !open);
  document.querySelectorAll<HTMLButtonElement>("[data-mode]").forEach((b) =>
    b.setAttribute("aria-expanded", String(open && b.dataset.mode === mode)),
  );
}
document.querySelectorAll<HTMLButtonElement>("[data-mode]").forEach((b) =>
  b.addEventListener("click", () => {
    const m = b.dataset.mode as Mode;
    if (m === mode) setMenuOpen(document.body.classList.contains("menu-closed"));
    else {
      setMode(m);
      setMenuOpen(true);
    }
  }),
);
$("btn-ride-import").addEventListener("click", () => $("import-menu").classList.toggle("hidden"));
$("imp-files").addEventListener("click", pickRides);
$("imp-folder").addEventListener("click", pickRideFolder);
$("btn-athlete").addEventListener("click", () => {
  renderAthleteForm();
  $("athlete-form").classList.toggle("hidden");
});
["ath-ftp", "ath-hr", "ath-weight"].forEach((id) => $(id).addEventListener("change", readAthleteForm));
$("rides-list").addEventListener("click", (e) => {
  const item = (e.target as HTMLElement).closest<HTMLElement>("[data-id]");
  if (item) selectRide(Number(item.dataset.id));
});
$("chart-tabs").addEventListener("click", (e) => {
  const m = (e.target as HTMLElement).closest<HTMLElement>("[data-metric]")?.dataset.metric as Metric | undefined;
  if (!m || !rideDetail) return;
  rideMetric = m;
  $("chart-tabs")
    .querySelectorAll("button")
    .forEach((b) => b.classList.toggle("active", b.dataset.metric === m));
  rideChart.setData(rideDetail.analysis.series, m);
});
$("ride-detail").addEventListener("click", async (e) => {
  const target = e.target as HTMLElement;
  if (!rideDetail) return;
  const id = rideDetail.summary.id;
  if (target.closest("#ride-delete")) {
    const ok = await ask(`Supprimer « ${rideDetail.summary.name} » de GoRide ? Le fichier d'origine n'est pas touché.`, {
      title: "Supprimer la sortie",
      kind: "warning",
      okLabel: "Supprimer",
      cancelLabel: "Annuler",
    });
    if (!ok) return;
    try {
      await invoke("delete_activity", { id });
      rideDetail = null;
      renderRideDetail();
      trackMap.clearTrack();
      await refreshRides();
    } catch (err) {
      toast(String(err));
    }
  } else if (target.closest("#ride-rename")) {
    const title = $("ride-title");
    title.contentEditable = "true";
    title.focus();
    document.getSelection()?.selectAllChildren(title);
    const done = async () => {
      title.contentEditable = "false";
      const name = title.textContent?.trim() ?? "";
      if (!name || name === rideDetail?.summary.name) {
        title.textContent = rideDetail?.summary.name ?? "";
        return;
      }
      try {
        await invoke("rename_activity", { id, name });
        if (rideDetail) rideDetail.summary.name = name;
        await refreshRides();
      } catch (err) {
        toast(String(err));
      }
    };
    title.addEventListener("blur", done, { once: true });
    title.addEventListener("keydown", (k) => {
      if (k.key === "Enter") {
        k.preventDefault();
        title.blur();
      }
    });
  } else {
    // Clic sur un passage d'une autre sortie : on l'ouvre
    const other = target.closest<HTMLElement>("[data-open-ride]");
    if (other && Number(other.dataset.openRide) !== id) {
      selectRide(Number(other.dataset.openRide));
      return;
    }
    // Clic sur une montée : la carte se cadre dessus et ses passages s'affichent
    const li = target.closest<HTMLElement>("[data-climb]");
    if (li) {
      li.querySelector(".climb-efforts")?.classList.toggle("hidden");
      const c = rideDetail.analysis.climbs[Number(li.dataset.climb)];
      const t = rideDetail.track;
      const a = indexAtDist(t.dist, c.startKm * 1000);
      const b = indexAtDist(t.dist, c.endKm * 1000);
      trackMap.fit(t.coords.slice(a, b + 1));
      rideChart.setCursor(c.startKm);
    }
  }
});

// --- Statistiques ------------------------------------------------------------

const SURFACE_LABEL: Record<Surface, string> = {
  Paved: "Goudron",
  Assumed: "Goudron présumé",
  Unpaved: "Non revêtu",
};
const CLASS_LABEL: Record<RoadClass, string> = {
  Trunk: "Voie rapide",
  Primary: "Grand axe",
  Secondary: "Départementale",
  Tertiary: "Route secondaire",
  Minor: "Petite route",
  Residential: "Rue",
  Service: "Voie de desserte",
  Cycleway: "Piste cyclable",
  Track: "Chemin revêtu",
  Path: "Sentier revêtu",
};

function showStats(track: Track, name: string, summary: RouteSummary | null) {
  $("track-name").textContent = name;
  const s = track.stats;
  $("st-dist").textContent = km(s.distanceM);
  $("st-up").textContent = s.hasElevation ? meters(s.ascentM) : "—";
  $("st-down").textContent = s.hasElevation ? meters(s.descentM) : "—";
  $("st-alt").textContent = s.hasElevation ? `${Math.round(s.minEleM!)} – ${meters(s.maxEleM!)}` : "—";
  updateTime();

  const extra = $("route-extra");
  extra.classList.toggle("hidden", !summary);
  if (summary) {
    const total = summary.surfaces.reduce((a, x) => a + x.meters, 0) || 1;
    $("surf-bar").innerHTML = summary.surfaces
      .filter((x) => x.meters > 0)
      .map((x) => `<span class="s-${x.kind}" style="flex:${x.meters}" title="${SURFACE_LABEL[x.kind]}"></span>`)
      .join("");
    $("surf-legend").innerHTML = summary.surfaces
      .filter((x) => x.meters > 0)
      .map(
        (x) =>
          `<li><i class="s-${x.kind}"></i>${SURFACE_LABEL[x.kind]} <b>${Math.round((x.meters / total) * 100)} %</b></li>`,
      )
      .join("");
    $("class-list").innerHTML = [...summary.classes]
      .sort((a, b) => b.meters - a.meters)
      .map((x) => `<li><span>${CLASS_LABEL[x.kind]}</span><b>${km(x.meters)}</b></li>`)
      .join("");
  }

  if (!libraryOpen && !loopMode) $("stats").classList.remove("hidden");
  $("profile").classList.remove("hidden");
  renderPoints();
  updateEmptyCard();
  updateSaveState();
}

// --- Points du tracé : liste réorganisable -----------------------------------------

/** Distances cumulées (m) des points de passage, ramenées à la longueur de la trace. */
function waypointDistances(totalM: number): number[] {
  const n = planner.count;
  if (n < 2 || lastLegs.length !== n - 1) return [];
  const sum = lastLegs.reduce((a, l) => a + l.lengthM, 0) || 1;
  const out = [0];
  for (const l of lastLegs) out.push(out[out.length - 1] + l.lengthM);
  return out.map((d) => (d / sum) * totalM);
}

const esc = (t: string) => t.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);

function renderPoints() {
  const section = $("points");
  const pts = planner.points;
  const show = current?.kind === "route" && pts.length >= 2;
  section.classList.toggle("hidden", !show);
  if (!show) return;
  const legsOk = lastLegs.length === pts.length - 1;
  const cum = [0];
  if (legsOk) for (const l of lastLegs) cum.push(cum[cum.length - 1] + l.lengthM);
  $("points-meta").textContent = `${pts.length} points${legsOk ? ` · ${km(cum[cum.length - 1])}` : ""}`;
  const n = pts.length;
  $("points-list").innerHTML = pts
    .map((_, i) => {
      const role = i === 0 ? "Départ" : i === n - 1 ? "Arrivée" : "";
      const ends = i === 0 || i === n - 1;
      return `<li class="pt" data-i="${i}">
        <span class="pt-handle" title="Glisser pour réorganiser" aria-hidden="true"><svg viewBox="0 0 10 16"><circle cx="2.5" cy="3" r="1.4"/><circle cx="7.5" cy="3" r="1.4"/><circle cx="2.5" cy="8" r="1.4"/><circle cx="7.5" cy="8" r="1.4"/><circle cx="2.5" cy="13" r="1.4"/><circle cx="7.5" cy="13" r="1.4"/></svg></span>
        <span class="pt-rail"><b class="pt-badge${ends ? " end" : ""}">${i + 1}</b>${i < n - 1 ? `<i class="pt-link"></i>` : ""}</span>
        <span class="pt-text">
          <span class="pt-title">Point ${i + 1}${role ? ` · ${role}` : ""}</span>
          <span class="pt-place" data-place="${i}">…</span>
        </span>
        <span class="pt-km">${legsOk ? km(cum[i]) : ""}</span>
        <span class="pt-acts">
          <button class="icon-btn" data-act="up" aria-label="Monter le point ${i + 1}" ${i === 0 ? "disabled" : ""}><svg viewBox="0 0 24 24"><path d="m6 15 6-6 6 6"/></svg></button>
          <button class="icon-btn" data-act="down" aria-label="Descendre le point ${i + 1}" ${i === n - 1 ? "disabled" : ""}><svg viewBox="0 0 24 24"><path d="m6 9 6 6 6-6"/></svg></button>
          <button class="icon-btn" data-act="del" aria-label="Supprimer le point ${i + 1}" ${n <= 2 ? "disabled" : ""}><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6 6 18"/></svg></button>
        </span>
      </li>`;
    })
    .join("");
  pts.forEach((p, i) =>
    placeName(p).then((name) => {
      const el = document.querySelector<HTMLElement>(`[data-place="${i}"]`);
      // la liste a pu changer entre-temps : on vérifie que c'est toujours le même point
      const now = planner.points[i];
      if (el && now && now[0] === p[0] && now[1] === p[1]) {
        el.textContent = name;
        el.title = name;
      }
    }),
  );
}

$("points-toggle").addEventListener("click", () => {
  const open = $("points").classList.toggle("collapsed") === false;
  $("points-toggle").setAttribute("aria-expanded", String(open));
});
$("points-reverse").addEventListener("click", () => planner.reverse());
$("points-list").addEventListener("click", (e) => {
  const target = e.target as HTMLElement;
  const li = target.closest<HTMLElement>(".pt");
  if (!li) return;
  const i = Number(li.dataset.i);
  const act = target.closest<HTMLElement>("[data-act]")?.dataset.act;
  if (act === "up") planner.move(i, i - 1);
  else if (act === "down") planner.move(i, i + 1);
  else if (act === "del") planner.remove(i);
  else if (!target.closest(".pt-handle")) {
    const p = planner.points[i];
    if (p) trackMap.map.easeTo({ center: p, duration: 600 });
  }
});

// Réorganisation à la souris (sans glisser-déposer HTML, intercepté par Tauri sous Windows)
$("points-list").addEventListener("pointerdown", (e) => {
  const handle = (e.target as HTMLElement).closest(".pt-handle");
  const li = handle?.closest<HTMLElement>(".pt");
  if (!li || e.button !== 0) return;
  e.preventDefault();
  const list = $("points-list");
  const rows = [...list.querySelectorAll<HTMLElement>(".pt")];
  const from = Number(li.dataset.i);
  const startY = e.clientY;
  let to = from;
  li.setPointerCapture(e.pointerId);
  li.classList.add("dragging");
  const onMove = (ev: PointerEvent) => {
    li.style.transform = `translateY(${ev.clientY - startY}px)`;
    const others = rows.filter((r) => r !== li);
    to = others.filter((r) => {
      const b = r.getBoundingClientRect();
      return b.top + b.height / 2 < ev.clientY;
    }).length;
    rows.forEach((r) => r.classList.remove("drop-before", "drop-after"));
    const target = others[to] ?? others[others.length - 1];
    target?.classList.add(others[to] ? "drop-before" : "drop-after");
  };
  const onUp = () => {
    li.removeEventListener("pointermove", onMove);
    li.removeEventListener("pointerup", onUp);
    li.removeEventListener("pointercancel", onUp);
    li.classList.remove("dragging");
    li.style.transform = "";
    rows.forEach((r) => r.classList.remove("drop-before", "drop-after"));
    if (to !== from) planner.move(from, to);
  };
  li.addEventListener("pointermove", onMove);
  li.addEventListener("pointerup", onUp);
  li.addEventListener("pointercancel", onUp);
});

// --- Recherche de lieu ------------------------------------------------------------

let searchAbort: AbortController | null = null;
let searchTimer = 0;
let searchHits: Place[] = [];
let searchSel = -1;

function renderSearch() {
  const ul = $("search-results");
  ul.classList.toggle("hidden", searchHits.length === 0);
  ul.innerHTML = searchHits
    .map(
      (p, i) =>
        `<li role="option" data-i="${i}" aria-selected="${i === searchSel}" class="${i === searchSel ? "sel" : ""}"><b>${esc(p.label)}</b>${p.kind ? `<small>${esc(p.kind)}</small>` : ""}</li>`,
    )
    .join("");
}

function goToPlace(p: Place) {
  trackMap.map.flyTo({ center: p.center, zoom: /Commune/.test(p.kind) ? 12 : 15, duration: 1400 });
  $<HTMLInputElement>("search").value = p.label;
  searchHits = [];
  renderSearch();
  $("search").blur();
  placeSearchPin(p);
}

// Repère d'un lieu (trouvé par la recherche ou cliqué sur la carte) : un clic ouvre
// ses infos et les actions (ajouter, détour, départ, arrivée).
let searchPin: maplibregl.Marker | null = null;
type PlaceAction = "add" | "detour" | "start" | "end";
type PinKind = "search" | "map";
interface PinInfo {
  /** Adresse trouvée (clic sur la carte), null si inconnue ou hors ligne. */
  label: string | null;
  kind: string;
  nearM: number | null;
  alt: number | null;
  failed: boolean;
}

/** Infos d'un point : altitude (moteur Rust) et, pour un clic sur la carte, adresse la plus proche. */
function loadPinInfo(at: [number, number], kind: PinKind): Promise<PinInfo> {
  const alt = inTauri ? invoke<number | null>("elevation_at", { point: at }).catch(() => null) : Promise.resolve(null);
  const addr =
    kind === "map"
      ? reverseInfo(at).then(
          (r) => ({ r, failed: false }),
          () => ({ r: null, failed: true }),
        )
      : Promise.resolve({ r: null, failed: false });
  return Promise.all([alt, addr]).then(([a, { r, failed }]) => ({
    label: r?.label ?? null,
    kind: r?.kind ?? "",
    nearM: r?.distanceM ?? null,
    alt: a,
    failed,
  }));
}

// Clic sur la carte hors mode tracé : repère et infos du lieu.
// (petit délai pour ignorer le double-clic, qui sert à zoomer)
let mapInfoTimer = 0;
trackMap.onMapInfo = (p) => {
  clearTimeout(mapInfoTimer);
  if (mode !== "routes") return;
  // Une fenêtre était ouverte : ce clic sert seulement à la fermer
  if (searchPin?.getPopup()?.isOpen()) return;
  mapInfoTimer = window.setTimeout(() => placeSearchPin({ label: "", kind: "", center: p }, "map"), 250);
};
trackMap.map.on("dblclick", () => clearTimeout(mapInfoTimer));
const POPUP_OFFSET: maplibregl.Offset = {
  top: [0, 8],
  "top-left": [0, 8],
  "top-right": [0, 8],
  bottom: [0, -38],
  "bottom-left": [0, -38],
  "bottom-right": [0, -38],
  left: [16, -20],
  right: [-16, -20],
  center: [0, 0],
};

function removeSearchPin() {
  searchPin?.remove();
  searchPin = null;
}

function placeSearchPin(p: Place, kind: PinKind = "search") {
  removeSearchPin();
  const el = document.createElement("div");
  el.className = kind === "map" ? "search-pin map-pin" : "search-pin";
  el.title = kind === "map" ? "Infos sur ce point" : "Cliquer pour l'ajouter à l'itinéraire";
  el.innerHTML =
    kind === "map"
      ? `<svg viewBox="0 0 24 24" aria-hidden="true"><circle class="ring" cx="12" cy="12" r="7.5"/><circle class="core" cx="12" cy="12" r="2.4"/></svg>`
      : `<svg viewBox="0 0 28 38" aria-hidden="true"><path d="M14 1.5C7.1 1.5 1.5 7 1.5 13.9 1.5 23.2 14 36.5 14 36.5S26.5 23.2 26.5 13.9C26.5 7 20.9 1.5 14 1.5Z"/><circle cx="14" cy="14" r="4.6"/></svg>`;
  const popup = new maplibregl.Popup({ offset: kind === "map" ? 16 : POPUP_OFFSET, maxWidth: "300px", className: "place-pop", focusAfterOpen: false });
  const info = loadPinInfo(p.center, kind);
  popup.on("open", () => popup.setDOMContent(placePopup(p, kind, info, () => popup.remove())));
  const marker = new maplibregl.Marker({ element: el, anchor: kind === "map" ? "center" : "bottom" }).setLngLat(p.center).setPopup(popup).addTo(trackMap.map);
  // Le clic est géré ici (et ne remonte pas à la carte, sinon il ajouterait un point au tracé)
  el.addEventListener("click", (e) => {
    e.stopPropagation();
    marker.togglePopup();
  });
  searchPin = marker;
  if (kind === "map") marker.togglePopup(); // clic sur la carte : les infos s'ouvrent tout de suite
}

const fmtCoord = (p: [number, number]) => `${p[1].toFixed(5)}, ${p[0].toFixed(5)}`;

function placePopup(p: Place, kind: PinKind, info: Promise<PinInfo>, close: () => void): HTMLElement {
  const box = document.createElement("div");
  const canDetour = planner.count >= 2 && lastLegs.length === planner.count - 1;
  const empty = planner.count === 0;
  box.className = "pop";
  box.innerHTML = `
    <p class="eyebrow">${kind === "map" ? "Point sur la carte" : "Lieu recherché"}</p>
    <b class="pop-title${kind === "map" ? " loading" : ""}" data-f="title">${kind === "map" ? "Recherche de l'adresse…" : esc(p.label)}</b>
    <small class="pop-kind${p.kind ? "" : " hidden"}" data-f="kind">${esc(p.kind)}</small>
    <div class="pop-facts">
      <span data-f="alt"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 19 9.5 8l4 6.5L16 11l5 8z"/></svg><span>Altitude…</span></span>
      <button class="pop-coords" data-act="copy" title="Copier les coordonnées (latitude, longitude)">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 21s-7-6.3-7-11.5A7 7 0 0 1 19 9.5C19 14.7 12 21 12 21Z"/><circle cx="12" cy="9.5" r="2.4"/></svg><span>${fmtCoord(p.center)}</span>
      </button>
    </div>
    <div class="pop-acts">
      <button class="pop-btn primary" data-act="add">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5v14M5 12h14"/></svg>
        <span>${empty ? "Ajouter comme départ" : "Ajouter à l'itinéraire"}<small>${empty ? "premier point du tracé" : "point de passage, au meilleur endroit"}</small></span>
      </button>
      <button class="pop-btn" data-act="detour" ${canDetour ? "" : "disabled"} title="${canDetour ? "" : "Il faut d'abord un itinéraire d'au moins deux points"}">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 18h6a4 4 0 0 0 4-4V9M10 9l4-4 4 4"/></svg>
        <span>Faire un détour<small>aller-retour depuis le tracé</small></span>
      </button>
      <div class="pop-row">
        <button class="pop-btn" data-act="start">
          <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="7"/><circle cx="12" cy="12" r="2.5"/></svg>
          <span>Partir d'ici</span>
        </button>
        <button class="pop-btn" data-act="end">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 21V4M6 4h11l-2.5 4L17 12H6"/></svg>
          <span>Finir ici</span>
        </button>
      </div>
    </div>
    <button class="pop-link" data-act="remove">Retirer le repère</button>`;
  info.then((i) => {
    const alt = box.querySelector('[data-f="alt"] span')!;
    alt.textContent = i.alt !== null ? `Altitude ${Math.round(i.alt)} m` : "Altitude inconnue";
    if (kind !== "map") return;
    const title = box.querySelector<HTMLElement>('[data-f="title"]')!;
    const sub = box.querySelector<HTMLElement>('[data-f="kind"]')!;
    title.classList.remove("loading");
    title.textContent = i.label ?? (i.failed ? "Adresse indisponible (hors ligne ?)" : "Aucune adresse à proximité");
    const parts = [i.kind];
    if (i.label && i.nearM !== null && i.nearM > 150)
      parts.push(`adresse la plus proche, à ${i.nearM >= 1000 ? km(i.nearM) : `${Math.round(i.nearM / 10) * 10} m`}`);
    sub.textContent = parts.filter(Boolean).join(" · ");
    sub.classList.toggle("hidden", !sub.textContent);
  });
  box.addEventListener("click", async (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLButtonElement>("[data-act]");
    if (!btn || btn.disabled) return;
    const act = btn.dataset.act as PlaceAction | "remove" | "copy";
    if (act === "copy") {
      const label = btn.querySelector("span")!;
      try {
        await navigator.clipboard.writeText(fmtCoord(p.center));
        label.textContent = "Coordonnées copiées";
      } catch {
        label.textContent = "Copie impossible";
      }
      setTimeout(() => (label.textContent = fmtCoord(p.center)), 1400);
      return;
    }
    if (act === "remove") {
      close();
      removeSearchPin();
      return;
    }
    box.classList.add("busy");
    const ok = await placeAction(act, p.center);
    box.classList.remove("busy");
    if (ok) {
      close();
      removeSearchPin();
      if (kind === "search") $<HTMLInputElement>("search").value = "";
    }
  });
  return box;
}

/** Ajoute le lieu à l'itinéraire selon l'action choisie (passe en mode tracé si besoin). */
async function placeAction(act: PlaceAction, at: [number, number]): Promise<boolean> {
  if (mode !== "routes") {
    setMode("routes");
    setMenuOpen(true);
  }
  if (loopMode) closeLoop();
  closeLibrary();
  if (!trackMap.drawing) setDrawing(true);
  if (!trackMap.drawing) return false; // réseau pas prêt : un message l'a déjà dit
  switch (act) {
    case "add":
      return planner.addBest(at);
    case "detour":
      return planner.detour(at);
    case "start":
      return planner.insertAt(0, at);
    case "end":
      return planner.insertAt(planner.count, at);
  }
}

$("search").addEventListener("input", () => {
  clearTimeout(searchTimer);
  const text = $<HTMLInputElement>("search").value;
  searchTimer = window.setTimeout(async () => {
    searchAbort?.abort();
    searchAbort = new AbortController();
    try {
      searchHits = await searchPlaces(text, searchAbort.signal);
      searchSel = searchHits.length ? 0 : -1;
      renderSearch();
    } catch (err) {
      if ((err as Error).name !== "AbortError") toast(`Recherche impossible : ${(err as Error).message ?? err}`);
    }
  }, 250);
});
$("search").addEventListener("search", () => {
  // Croix du champ : on retire aussi le repère
  if (!$<HTMLInputElement>("search").value) removeSearchPin();
});
$("search").addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    if (!searchHits.length) return;
    searchSel = (searchSel + (e.key === "ArrowDown" ? 1 : -1) + searchHits.length) % searchHits.length;
    renderSearch();
  } else if (e.key === "Enter" && searchHits[searchSel]) {
    goToPlace(searchHits[searchSel]);
  } else if (e.key === "Escape") {
    searchHits = [];
    renderSearch();
    $("search").blur();
  }
});
$("search").addEventListener("blur", () => setTimeout(() => ((searchHits = []), renderSearch()), 150));
$("search-results").addEventListener("pointerdown", (e) => {
  const li = (e.target as HTMLElement).closest<HTMLElement>("[data-i]");
  if (li) {
    e.preventDefault();
    goToPlace(searchHits[Number(li.dataset.i)]);
  }
});

function speed(): number {
  const raw = Number($<HTMLInputElement>("in-speed").value);
  // Vitesse moyenne : entre 10 et 150 km/h
  return Number.isFinite(raw) && raw > 0 ? Math.min(150, Math.max(10, raw)) : 27;
}

function updateTime() {
  if (!current) return;
  $("st-time").textContent = duration(current.track.stats.distanceM / 1000 / speed());
}

async function exportRoute(format: "gpx" | "fit") {
  if (!current) return;
  const safe = current.name.replace(/[\\/:*?"<>|]/g, "-");
  const filter =
    format === "fit"
      ? { name: "Parcours Garmin (FIT)", extensions: ["fit"] }
      : { name: "Trace GPX", extensions: ["gpx"] };
  try {
    const path = await save({ defaultPath: `${safe}.${format}`, filters: [filter] });
    if (!path) return;
    await invoke("save_route", {
      path,
      name: current.name,
      coords: current.track.coords,
      ele: current.track.ele,
      speedKmh: speed(),
    });
    const file = path.split(/[\\/]/).pop();
    toast(
      format === "fit"
        ? `${file} enregistré : copie-le dans le dossier Garmin/NewFiles de ton Edge.`
        : `Itinéraire enregistré : ${file}`,
      "ok",
    );
  } catch (err) {
    toast(`Export impossible : ${err}`);
  }
}

// --- Mes itinéraires : enregistrés dans un dossier choisi une fois -----------------

interface SavedRouteSummary {
  file: string;
  name: string;
  distanceM: number;
  ascentM: number;
  savedAt: number;
  editable: boolean;
}
interface SavedRoute {
  summary: SavedRouteSummary;
  track: Track;
  waypoints: [number, number][];
  options: Partial<RouteOptions> | null;
}

/** Fichier de l'itinéraire affiché, s'il a été enregistré ou ouvert depuis la bibliothèque. */
let currentFile: string | null = null;
/** Empreinte (nom + points) au dernier enregistrement : sert à savoir s'il y a des changements. */
let savedSig: string | null = null;
let routesFolder: string | null = null;
let libraryOpen = false;
let library: SavedRouteSummary[] = [];
let libraryShown: SavedRouteSummary[] = [];

const currentName = () => (current?.kind === "import" ? current.name : routeName);
const signature = () => JSON.stringify([currentName(), current?.kind === "import" ? current.track.stats.pointCount : planner.points]);

/** Bouton « Enregistrer » : indique si l'itinéraire affiché a des changements non enregistrés. */
function updateSaveState() {
  const btn = $<HTMLButtonElement>("btn-save");
  const saved = !!currentFile && savedSig === signature();
  btn.classList.toggle("saved", saved);
  btn.disabled = !current;
  $("btn-save-label").textContent = saved ? "Enregistré" : currentFile ? "Enregistrer les modifications" : "Enregistrer";
  btn.title = saved ? "Déjà enregistré dans Mes itinéraires" : "Enregistrer dans Mes itinéraires (Ctrl+S)";
}

async function getRoutesFolder(): Promise<string | null> {
  if (routesFolder) return routesFolder;
  try {
    routesFolder = await invoke<string | null>("routes_folder");
  } catch {
    routesFolder = null;
  }
  return routesFolder;
}

/** Choix du dossier (la première fois, ou pour en changer). */
async function chooseRoutesFolder(first: boolean): Promise<string | null> {
  const dir = await open({
    directory: true,
    multiple: false,
    title: first ? "Où ranger tes itinéraires ? (choix fait une seule fois)" : "Nouveau dossier pour tes itinéraires",
    defaultPath: routesFolder ?? undefined,
  });
  if (typeof dir !== "string") return null;
  try {
    await invoke("set_routes_folder", { path: dir });
    routesFolder = dir;
    if (!first) toast("Dossier changé. Les itinéraires déjà enregistrés restent dans l'ancien dossier.", "ok");
    return dir;
  } catch (err) {
    toast(String(err));
    return null;
  }
}

async function saveToLibrary() {
  if (!current) return toast("Rien à enregistrer : trace ou importe d'abord un itinéraire.");
  if (!inTauri) return toast("L'enregistrement fonctionne dans l'application, pas dans un navigateur.");
  const dir = (await getRoutesFolder()) ?? (await chooseRoutesFolder(true));
  if (!dir) return;
  const isRoute = current.kind === "route";
  const s = current.track.stats;
  const btn = $<HTMLButtonElement>("btn-save");
  btn.disabled = true;
  try {
    const sum = await invoke<SavedRouteSummary>("save_to_library", {
      name: current.name.trim() || "Itinéraire",
      coords: current.track.coords,
      ele: current.track.ele,
      meta: {
        waypoints: isRoute ? planner.points : [],
        options: isRoute ? options : null,
        distanceM: s.distanceM,
        ascentM: s.hasElevation ? s.ascentM : 0,
      },
      previous: currentFile,
    });
    currentFile = sum.file;
    savedSig = signature();
    toast(`« ${sum.name} » enregistré dans Mes itinéraires.`, "ok");
    if (libraryOpen) refreshLibrary();
  } catch (err) {
    toast(`Enregistrement impossible : ${err}`);
  } finally {
    updateSaveState();
  }
}

function openLibrary() {
  if (mode !== "routes") return;
  closeLoop();
  libraryOpen = true;
  $("library-panel").classList.remove("hidden");
  $("stats").classList.add("hidden");
  $("btn-library").classList.add("active");
  setMenuOpen(true);
  refreshLibrary();
}

function closeLibrary() {
  if (!libraryOpen) return;
  libraryOpen = false;
  $("library-panel").classList.add("hidden");
  $("btn-library").classList.remove("active");
  if (current && !loopMode) $("stats").classList.remove("hidden");
}

async function refreshLibrary() {
  if (!inTauri) {
    library = [];
    renderLibrary();
    return;
  }
  await getRoutesFolder();
  try {
    library = await invoke<SavedRouteSummary[]>("list_saved_routes");
  } catch (err) {
    library = [];
    toast(String(err));
  }
  renderLibrary();
}

const dateFmt = new Intl.DateTimeFormat("fr-FR", { day: "numeric", month: "short", year: "numeric" });

function renderLibrary() {
  const q = $<HTMLInputElement>("library-filter")
    .value.trim()
    .toLocaleLowerCase("fr")
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "");
  const norm = (t: string) => t.toLocaleLowerCase("fr").normalize("NFD").replace(/\p{Diacritic}/gu, "");
  libraryShown = q ? library.filter((r) => norm(r.name).includes(q)) : library;
  const empty = $("library-empty");
  empty.classList.toggle("hidden", libraryShown.length > 0);
  empty.innerHTML = !routesFolder
    ? "Aucun dossier choisi pour l'instant. Trace un itinéraire puis clique sur <b>Enregistrer</b> : GoRide te demandera une seule fois où les ranger."
    : library.length === 0
      ? "Aucun itinéraire enregistré. Trace un itinéraire puis clique sur <b>Enregistrer</b> (Ctrl+S)."
      : "Aucun itinéraire ne correspond.";
  $("library-filter-box").classList.toggle("hidden", library.length < 2);
  $("library-count").textContent = library.length ? String(library.length) : "";
  $("library-list").innerHTML = libraryShown
    .map((r, i) => {
      const meta = [km(r.distanceM)];
      if (r.ascentM > 0) meta.push(`D+ ${meters(Math.round(r.ascentM))}`);
      if (r.savedAt > 0) meta.push(dateFmt.format(new Date(r.savedAt)));
      return `<li class="lib-item${r.file === currentFile ? " selected" : ""}" data-i="${i}" title="${r.editable ? "Ouvrir et modifier" : "Ouvrir (trace GPX, non modifiable)"}">
        <span class="lib-icon${r.editable ? "" : " plain"}" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="${
          r.editable
            ? "M6 19.5a2 2 0 1 0 0-4 2 2 0 0 0 0 4ZM18 8.5a2 2 0 1 0 0-4 2 2 0 0 0 0 4ZM8 17.5h7.5a3 3 0 0 0 0-6h-7a3 3 0 0 1 0-6H16"
            : "M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8l-5-5ZM14 3v5h5"
        }"/></svg></span>
        <span class="lib-text"><span class="lib-name">${esc(r.name)}</span><span class="lib-meta">${meta.join(" · ")}</span></span>
        <button class="icon-btn lib-del" data-del="${i}" title="Supprimer" aria-label="Supprimer ${esc(r.name)}"><svg viewBox="0 0 24 24"><path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/></svg></button>
      </li>`;
    })
    .join("");
  const path = $("library-path");
  path.textContent = routesFolder ?? "Dossier non choisi";
  path.title = routesFolder ?? "";
  $<HTMLButtonElement>("library-reveal").disabled = !routesFolder;
  $("library-change").textContent = routesFolder ? "Changer" : "Choisir…";
}

async function openSavedRoute(r: SavedRouteSummary) {
  let sr: SavedRoute;
  try {
    sr = await invoke<SavedRoute>("open_saved_route", { file: r.file });
  } catch (err) {
    toast(String(err));
    refreshLibrary();
    return;
  }
  const name = sr.summary.name;
  closeLibrary();
  const editable = sr.waypoints.length >= 2;
  if (editable && network.loaded) {
    // On retrouve les réglages utilisés pour que le tracé soit identique
    const previousOptions = options;
    if (sr.options) {
      options = { ...DEFAULT_OPTIONS, ...sr.options };
      renderOptions();
    }
    if (current?.kind === "import") {
      current = null;
      trackMap.clearTrack();
      hideRoute();
    }
    routeName = name;
    setDrawing(true);
    const ok = await planner.setPoints(sr.waypoints);
    if (ok) {
      currentFile = sr.summary.file;
      savedSig = signature();
      try {
        localStorage.setItem(OPTIONS_KEY, JSON.stringify(options));
      } catch {
        /* rien */
      }
      updateSaveState();
      trackMap.fit(sr.track.coords);
      return;
    }
    options = previousOptions;
    renderOptions();
  } else if (editable) {
    toast("Réseau routier pas encore chargé : l'itinéraire s'affiche, mais il faudra le rouvrir pour le modifier.");
  }
  // Trace seule (GPX externe, ou réseau indisponible) : affichée sans points modifiables
  setDrawing(false);
  await planner.clear();
  current = { kind: "import", track: sr.track, name };
  currentFile = sr.summary.file;
  savedSig = signature();
  showStats(sr.track, name, null);
  profile.setTrack(sr.track);
  profile.setMarks([]);
  trackMap.setTrack(sr.track);
}

async function deleteSavedRoute(r: SavedRouteSummary) {
  const ok = await ask(`Supprimer « ${r.name} » ? Le fichier sera effacé du dossier des itinéraires.`, {
    title: "Supprimer l'itinéraire",
    kind: "warning",
    okLabel: "Supprimer",
    cancelLabel: "Annuler",
  });
  if (!ok) return;
  try {
    await invoke("delete_saved_route", { file: r.file });
    if (currentFile === r.file) {
      currentFile = null;
      savedSig = null;
      updateSaveState();
    }
  } catch (err) {
    toast(String(err));
  }
  refreshLibrary();
}

$("btn-save").addEventListener("click", saveToLibrary);
$("btn-library").addEventListener("click", () => (libraryOpen ? closeLibrary() : openLibrary()));
$("library-close").addEventListener("click", closeLibrary);
$("library-filter").addEventListener("input", renderLibrary);
$("library-list").addEventListener("click", (e) => {
  const target = e.target as HTMLElement;
  const del = target.closest<HTMLElement>("[data-del]");
  if (del) {
    const r = libraryShown[Number(del.dataset.del)];
    if (r) deleteSavedRoute(r);
    return;
  }
  const li = target.closest<HTMLElement>("[data-i]");
  const r = li ? libraryShown[Number(li.dataset.i)] : undefined;
  if (r) openSavedRoute(r);
});
$("library-reveal").addEventListener("click", () => invoke("reveal_routes_folder").catch((err) => toast(String(err))));
$("library-change").addEventListener("click", async () => {
  if (await chooseRoutesFolder(!routesFolder)) refreshLibrary();
});

// Nom de l'itinéraire : un clic sur le titre permet de le renommer
$("track-name").addEventListener("click", () => {
  const el = $("track-name");
  if (!current || el.isContentEditable) return;
  const before = current.name;
  el.contentEditable = "true";
  el.focus();
  document.getSelection()?.selectAllChildren(el);
  const finish = (keep: boolean) => {
    el.removeEventListener("keydown", onKey);
    el.contentEditable = "false";
    const name = (el.textContent ?? "").replace(/\s+/g, " ").trim();
    if (!keep || !name || !current) {
      el.textContent = before;
      return;
    }
    el.textContent = name;
    current.name = name;
    if (current.kind === "route") routeName = name;
    updateSaveState();
  };
  const onKey = (k: KeyboardEvent) => {
    if (k.key === "Enter") {
      k.preventDefault();
      el.blur();
    } else if (k.key === "Escape") {
      k.preventDefault();
      k.stopPropagation();
      finish(false);
      el.blur();
    }
  };
  el.addEventListener("keydown", onKey);
  el.addEventListener("blur", () => el.isContentEditable && finish(true), { once: true });
});

// --- Réseau routier ----------------------------------------------------------

function renderNetwork(st: NetworkStatus) {
  network = st;
  if (netMenuOpen && !franceJob) refreshNetMenu();
  const el = $("net");
  // Vert dès qu'un réseau est utilisable, même si une autre préparation tourne en fond
  el.classList.toggle("ok", st.loaded);
  el.classList.toggle("busy", st.busy);
  if (!st.busy) {
    $("net-label").textContent = st.loaded
      ? `Réseau : ${st.region}`
      : st.notice
        ? "Réseau à mettre à jour"
        : "Région : automatique";
    el.title = st.loaded
      ? `${st.segments.toLocaleString("fr-FR")} tronçons routiers${st.elevation ? ", relief compris" : ""}. La région change toute seule quand tu traces ailleurs. Clic : choisir un extrait .osm.pbf à la main.`
      : (st.notice ??
        "La région se charge toute seule au premier point tracé. Clic : choisir un extrait OpenStreetMap (.osm.pbf) à la main.");
    if (st.notice && !st.loaded) toast(st.notice);
  }
  $("opt-note").classList.toggle("hidden", !st.loaded || st.elevation);
}

async function buildNetwork() {
  if (network.busy) return;
  try {
    const path = await open({
      multiple: false,
      directory: false,
      title: "Extrait OpenStreetMap à utiliser pour le routage",
      filters: [{ name: "Extrait OpenStreetMap", extensions: ["pbf"] }],
    });
    if (typeof path !== "string") return;
    renderNetwork({ ...network, busy: true });
    $("net-label").textContent = "Préparation…";
    const t0 = performance.now();
    const st = await invoke<NetworkStatus>("build_network", { path });
    renderNetwork(st);
    toast(`Réseau ${st.region} prêt en ${Math.round((performance.now() - t0) / 1000)} s : tu peux tracer.`, "ok");
  } catch (err) {
    renderNetwork({ ...network, busy: false });
    toast(`Construction du réseau impossible : ${err}`);
  }
}

// --- Régions chargées selon l'endroit où l'on trace ---------------------------------

interface RegionInfo {
  slug: string;
  name: string;
  sizeMb: number;
  downloaded: boolean;
}
interface Coverage {
  covered: boolean;
  regions: RegionInfo[];
  cached: boolean;
  name: string;
}

/** Chargement de région en cours (les autres clics l'attendent). */
let regionLoad: Promise<boolean> | null = null;

/**
 * Avant de tracer : les points sont-ils dans le réseau chargé ? Sinon on propose de charger
 * la région du point et ses voisines (téléchargement une seule fois, puis réseau en cache).
 */
async function ensureCoverage(points: [number, number][], keep: [number, number][]): Promise<boolean> {
  if (!inTauri || points.length === 0) return true;
  if (regionLoad) await regionLoad;
  let c: Coverage;
  try {
    c = await invoke<Coverage>("coverage_for", { points, keep });
  } catch (err) {
    // Carte des régions inaccessible (hors ligne) : on tente quand même avec le réseau actuel
    if (network.loaded) return true;
    toast(String(err));
    return false;
  }
  if (c.covered) return true;
  if (c.regions.length === 0) {
    toast("Ce point est hors de France métropolitaine : aucun réseau routier disponible ici.");
    return false;
  }
  if (!c.cached) {
    const toGet = c.regions.filter((r) => !r.downloaded);
    const mb = toGet.reduce((a, r) => a + r.sizeMb, 0);
    const lines = [`Pour tracer ici, GoRide doit charger le réseau routier : ${c.name}.`];
    if (toGet.length)
      lines.push(
        `Téléchargement depuis OpenStreetMap (Geofabrik) : ${toGet.map((r) => r.name).join(", ")}, environ ${mb.toLocaleString("fr-FR")} Mo, une seule fois.`,
      );
    lines.push("Préparation : quelques minutes. Ensuite ce réseau est gardé et se recharge en quelques secondes.");
    const ok = await ask(lines.join("\n\n"), {
      title: "Nouvelle région",
      kind: "info",
      okLabel: "Charger",
      cancelLabel: "Annuler",
    });
    if (!ok) return false;
  }
  regionLoad = loadRegions(c);
  try {
    return await regionLoad;
  } finally {
    regionLoad = null;
  }
}

async function loadRegions(c: Coverage): Promise<boolean> {
  toast(`Chargement du réseau ${c.name}…`, "ok", 0);
  renderNetwork({ ...network, busy: true });
  $("net-label").textContent = "Préparation…";
  try {
    const st = await invoke<NetworkStatus>("load_regions", { slugs: c.regions.map((r) => r.slug) });
    renderNetwork(st);
    toast(`Réseau ${st.region} prêt.`, "ok");
    return true;
  } catch (err) {
    renderNetwork({ ...network, busy: false });
    toast(`Chargement de la région impossible : ${err}`);
    return false;
  }
}

// --- Interface ---------------------------------------------------------------

$("btn-import").addEventListener("click", pickFile);
$("btn-draw").addEventListener("click", () => setDrawing(!trackMap.drawing));
$("btn-undo").addEventListener("click", () => planner.undo());
$("btn-redo").addEventListener("click", () => planner.redo());
$("btn-clear").addEventListener("click", () => planner.clear());
$("btn-fit").addEventListener("click", () => trackMap.fit(current?.track.coords));
$("btn-export").addEventListener("click", () => exportRoute("gpx"));
$("btn-export-fit").addEventListener("click", () => exportRoute("fit"));
// Menu « Réseau » : état, toute la France, fichier à la main
interface RegionsOverview {
  regions: RegionInfo[];
  totalMb: number;
  downloadedMb: number;
  franceReady: boolean;
  franceActive: boolean;
}
let netMenuOpen = false;
let franceJob: "prepare" | "download" | null = null;

const go = (mb: number) => `${(mb / 1000).toLocaleString("fr-FR", { maximumFractionDigits: 1 })} Go`;

function setNetMenu(open: boolean) {
  netMenuOpen = open;
  $("net-menu").classList.toggle("hidden", !open);
  $("net").setAttribute("aria-expanded", String(open));
  if (open) refreshNetMenu();
}

async function refreshNetMenu() {
  $("nm-current").textContent = network.loaded
    ? `Actif : ${network.region} · ${network.segments.toLocaleString("fr-FR")} tronçons${network.elevation ? ", relief compris" : ""}`
    : network.busy
      ? "Préparation en cours…"
      : "Aucun réseau chargé pour l'instant.";
  if (!inTauri) return;
  let o: RegionsOverview;
  try {
    o = await invoke<RegionsOverview>("regions_overview");
  } catch {
    return;
  }
  const n = o.regions.filter((r) => r.downloaded).length;
  $("nm-france-meta").textContent = `${n} / ${o.regions.length} régions téléchargées · ${go(o.downloadedMb)} / ${go(o.totalMb)}`;
  $("nm-bar").style.width = `${(o.downloadedMb / o.totalMb) * 100}%`;
  const prepare = $<HTMLButtonElement>("nm-prepare");
  const download = $<HTMLButtonElement>("nm-download");
  const busy = network.busy || franceJob !== null;
  prepare.textContent = o.franceActive
    ? "Toute la France est chargée"
    : o.franceReady
      ? "Charger toute la France"
      : "Préparer toute la France";
  prepare.disabled = busy || o.franceActive;
  download.textContent = n === o.regions.length ? "Les 22 régions sont téléchargées" : "Seulement télécharger les 22 régions";
  download.disabled = busy || n === o.regions.length;
  $("nm-cancel").classList.toggle("hidden", franceJob === null);
}

async function prepareFrance() {
  let o: RegionsOverview;
  try {
    o = await invoke<RegionsOverview>("regions_overview");
  } catch (err) {
    return toast(String(err));
  }
  if (!o.franceReady) {
    const missing = o.totalMb - o.downloadedMb;
    const ok = await ask(
      [
        "GoRide va préparer le réseau routier de toute la France métropolitaine.",
        missing > 0
          ? `Téléchargement : environ ${go(missing)} depuis OpenStreetMap (Geofabrik), une seule fois.`
          : "Toutes les régions sont déjà téléchargées.",
        "Préparation : un bon moment la première fois (le relief de toute la France est aussi téléchargé), et beaucoup de mémoire : ferme les jeux et logiciels gourmands. Pendant ce temps le tracé est indisponible.",
        "Ensuite tu peux tracer partout, et ce réseau se recharge tout seul au démarrage.",
      ].join("\n\n"),
      { title: "Toute la France", kind: "info", okLabel: "Préparer", cancelLabel: "Annuler" },
    );
    if (!ok) return;
  }
  franceJob = "prepare";
  refreshNetMenu();
  renderNetwork({ ...network, busy: true });
  try {
    const st = await invoke<NetworkStatus>("load_regions", { slugs: o.regions.map((r) => r.slug) });
    renderNetwork(st);
    toast("Toute la France est prête : tu peux tracer partout.", "ok", 6000);
  } catch (err) {
    renderNetwork({ ...network, busy: false });
    toast(String(err).includes("annulé") ? "Téléchargement annulé." : `Préparation impossible : ${err}`);
  } finally {
    franceJob = null;
    $("nm-progress").classList.add("hidden");
    if (netMenuOpen) refreshNetMenu();
  }
}

async function downloadAllRegions() {
  let o: RegionsOverview;
  try {
    o = await invoke<RegionsOverview>("regions_overview");
  } catch (err) {
    return toast(String(err));
  }
  const missing = o.totalMb - o.downloadedMb;
  const ok = await ask(
    `Télécharger les ${o.regions.filter((r) => !r.downloaded).length} régions manquantes (environ ${go(missing)}) ? Tu peux continuer à tracer pendant ce temps.`,
    { title: "Télécharger toute la France", kind: "info", okLabel: "Télécharger", cancelLabel: "Annuler" },
  );
  if (!ok) return;
  franceJob = "download";
  refreshNetMenu();
  try {
    await invoke("download_all_regions");
    toast("Les 22 régions sont téléchargées.", "ok", 6000);
  } catch (err) {
    toast(String(err).includes("annulé") ? "Téléchargement annulé." : `Téléchargement impossible : ${err}`);
  } finally {
    franceJob = null;
    $("nm-progress").classList.add("hidden");
    if (netMenuOpen) refreshNetMenu();
  }
}

$("net").addEventListener("click", (e) => {
  e.stopPropagation();
  setNetMenu(!netMenuOpen);
});
$("net-menu").addEventListener("click", (e) => e.stopPropagation());
document.addEventListener("click", () => netMenuOpen && setNetMenu(false));
$("nm-prepare").addEventListener("click", prepareFrance);
$("nm-download").addEventListener("click", downloadAllRegions);
$("nm-cancel").addEventListener("click", () => {
  invoke("cancel_download");
  $("nm-progress").textContent = "Arrêt du téléchargement…";
});
$("nm-file").addEventListener("click", () => {
  setNetMenu(false);
  buildNetwork();
});
$("in-speed").addEventListener("input", updateTime);
$("in-speed").addEventListener("change", () => {
  // Quand on quitte le champ, on y réécrit la valeur réellement utilisée
  $<HTMLInputElement>("in-speed").value = String(Math.round(speed()));
  updateTime();
});

window.addEventListener("keydown", (e) => {
  const t = e.target as HTMLElement;
  const typing = t.tagName === "INPUT" || t.tagName === "SELECT" || t.isContentEditable;
  const mod = e.ctrlKey || e.metaKey;
  const k = e.key.toLowerCase();
  if (mod && k === "k") {
    e.preventDefault();
    $("search").focus();
    $<HTMLInputElement>("search").select();
  } else if (mod && k === "o") {
    e.preventDefault();
    pickFile();
  } else if (mod && k === "z" && !e.shiftKey && !typing) {
    e.preventDefault();
    planner.undo();
  } else if (mod && (k === "y" || (k === "z" && e.shiftKey)) && !typing) {
    e.preventDefault();
    planner.redo();
  } else if (mod && k === "s") {
    e.preventDefault();
    saveToLibrary();
  } else if (!mod && k === "t" && !typing && mode === "routes") {
    setDrawing(!trackMap.drawing);
  } else if (!mod && k === "l" && !typing && mode === "routes") {
    if (libraryOpen) closeLibrary();
    else openLibrary();
  } else if (!mod && k === "b" && !typing && mode === "routes") {
    if (loopMode) closeLoop();
    else openLoop();
  } else if (!mod && (k === "2" || k === "3") && !typing) {
    trackMap.set3d(k === "3");
  } else if (k === "escape" && netMenuOpen) {
    setNetMenu(false);
  } else if (k === "escape" && !$("settings").classList.contains("hidden")) {
    setSettingsOpen(false);
  } else if (k === "escape" && libraryOpen && !typing) {
    closeLibrary();
  } else if (k === "escape" && loopMode) {
    closeLoop();
  } else if (k === "escape" && trackMap.drawing) {
    setDrawing(false);
  }
});

document.querySelectorAll<HTMLButtonElement>("[data-basemap]").forEach((btn) =>
  btn.addEventListener("click", () => {
    document.querySelectorAll("[data-basemap]").forEach((b) => b.classList.toggle("active", b === btn));
    trackMap.setBasemap(btn.dataset.basemap as BasemapId);
  }),
);
// Dock de vue : 2D / 3D, zoom, boussole, relief
$("tg-hillshade").addEventListener("click", () => {
  const btn = $("tg-hillshade");
  const on = !btn.classList.contains("on");
  btn.classList.toggle("on", on);
  btn.setAttribute("aria-pressed", String(on));
  trackMap.setHillshade(on);
});
document.querySelectorAll<HTMLButtonElement>("[data-view]").forEach((b) =>
  b.addEventListener("click", () => trackMap.set3d(b.dataset.view === "3d")),
);
trackMap.onViewChange = (is3d) => {
  $("view-switch").classList.toggle("is-3d", is3d);
  document.querySelectorAll<HTMLButtonElement>("[data-view]").forEach((b) =>
    b.classList.toggle("active", (b.dataset.view === "3d") === is3d),
  );
};
trackMap.onBearing = (bearing) => {
  $("compass-needle").style.transform = `rotate(${-bearing}deg)`;
};
$("btn-zoom-in").addEventListener("click", () => trackMap.map.zoomIn({ duration: 450 }));
$("btn-zoom-out").addEventListener("click", () => trackMap.map.zoomOut({ duration: 450 }));
$("btn-north").addEventListener("click", () => trackMap.map.easeTo({ bearing: 0, duration: 700 }));

let toastTimer = 0;
/** Message en bas de l'écran ; `ms` = 0 : reste affiché jusqu'au message suivant. */
// --- Actions du coach dans l'application ------------------------------------------------
// Le modèle demande une action (outil) ; on la fait ici avec les mêmes fonctions que les
// boutons, puis on lui renvoie le résultat en texte pour qu'il le commente.

const TOOL_PROFILE: Record<string, RouteOptions["profile"]> = { road: "Road", triathlon: "Triathlon", gravel: "Gravel" };
const TOOL_HILLS: Record<string, number> = { flat: -1, neutral: 0, hilly: 1 };
const TOOL_HEADING: Record<string, string> = {
  north: "0",
  northeast: "45",
  east: "90",
  southeast: "135",
  south: "180",
  southwest: "225",
  west: "270",
  northwest: "315",
};
const HILLS_LABEL: Record<string, string> = { "-1": "le plus plat possible", "0": "dénivelé neutre", "1": "en cherchant les bosses" };
const argStr = (v: unknown) => (typeof v === "string" ? v.trim() : "");
/** Échec d'une action : `text` pour le modèle, `reason` (sinon `text`) pour la discussion. */
const fail = (text: string, label: string, reason = text): ToolResult => ({
  text: `Échec : ${text}`,
  label: `${label} : ${reason.replace(/\.$/, "")}`,
  ok: false,
});
/** Même lieu (à 150 m près) : un « itinéraire » qui revient à son départ est une boucle. */
const samePlace = (a: [number, number], b: [number, number]) => Math.abs(a[0] - b[0]) < 0.002 && Math.abs(a[1] - b[1]) < 0.0014;
const ymd = (t: number) => {
  const d = new Date(t * 1000);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
};

/** Départ « n'importe où » : départ déjà choisi, sinon centre de la carte (ramené dans le réseau chargé). */
function anywhereStart(): { p: [number, number]; label: string } {
  if (loopStart) return { p: loopStart, label: "le point de départ déjà choisi" };
  if (planner.start) return { p: planner.start, label: "le départ de l'itinéraire en cours" };
  const c = trackMap.map.getCenter();
  let p: [number, number] = [c.lng, c.lat];
  const b = network.bbox;
  if (b && (p[0] < b[0] || p[0] > b[2] || p[1] < b[1] || p[1] > b[3])) p = [(b[0] + b[2]) / 2, (b[1] + b[3]) / 2];
  return { p, label: "le centre de la carte" };
}

/** Lieu demandé par le coach (texte) → coordonnées. Texte vide ou « n'importe où » : départ par défaut. */
async function toolPlace(text: string, allowAnywhere: boolean): Promise<{ p: [number, number]; label: string } | string> {
  if (!text || /n'?importe|peu importe|^ici$|centre de la carte|^carte$/i.test(text)) {
    return allowAnywhere ? anywhereStart() : "Lieu non précisé.";
  }
  try {
    const hits = await searchPlaces(text);
    if (!hits.length) return `Lieu « ${text} » introuvable en France.`;
    return { p: hits[0].center, label: hits[0].label };
  } catch (err) {
    return `Recherche du lieu « ${text} » impossible (connexion ?) : ${err}`;
  }
}

/** Réglages du tracé demandés par le coach ; renvoie leur description. */
async function toolOptions(a: Record<string, unknown>): Promise<string> {
  const patch: Partial<RouteOptions> = {};
  const profile = TOOL_PROFILE[argStr(a.profile)];
  if (profile) patch.profile = profile;
  const hills = TOOL_HILLS[argStr(a.hills)];
  if (hills !== undefined) patch.hills = hills;
  if (typeof a.avoid_main_roads === "boolean") patch.traffic = a.avoid_main_roads ? 1 : 0.5;
  const cyc = { avoid: -1, neutral: 0, prefer: 1 }[argStr(a.cycleways)];
  if (cyc !== undefined) patch.cycleways = cyc;
  if (typeof a.max_grade_pct === "number") patch.maxGrade = nearest([0, 6, 8, 10, 12, 15], a.max_grade_pct);
  if (Object.keys(patch).length) await changeOptions(patch);
  return `profil ${PROFILE_LABEL[options.profile].toLowerCase()}, ${HILLS_LABEL[String(nearest(HILLS_STEPS, options.hills))]}`;
}

function goRoutes() {
  if (mode !== "routes") {
    setMode("routes");
    setMenuOpen(true);
  }
}

/** Boucles calculées pour le coach : résultat en texte. */
async function toolLoops(start: { p: [number, number]; label: string }): Promise<string | null> {
  setLoopStart(start.p);
  return requestLoops(false);
}

function loopsText(): string {
  return loops
    .map((l, i) => {
      const parts = [`Boucle ${i + 1} : ${km(l.lengthM)}`];
      if (network.elevation) parts.push(`D+ ≈ ${Math.round(l.ascentM / 10) * 10} m`);
      if (l.retrace > 0.03) parts.push(`${Math.round(l.retrace * 100)} % d'aller-retour`);
      const k = l.key;
      if (loopWorkout && loopWorkout.workout.terrain !== "any")
        parts.push(
          k
            ? `${k.terrain === "climb" ? `montée de ${km(k.lengthM)} à ${k.gradePct.toFixed(1).replace(".", ",")} %` : `portion plate de ${km(k.lengthM)}`} (≈ ${fmtDuration(
                Math.round(k.estS / 60) * 60,
              )} d'effort) au km ${Math.round(k.startM / 1000)}${k.fit < 0.85 ? ", un peu courte pour l'effort demandé" : ""}`
            : "pas de terrain adapté à la séance",
        );
      return parts.join(", ");
    })
    .join("\n");
}

async function toolProposeLoops(a: Record<string, unknown>): Promise<ToolResult> {
  const dist = Math.round(Number(a.distance_km));
  if (!(dist >= 5 && dist <= 300)) return fail("distance à choisir entre 5 et 300 km.", "Distance de boucle invalide");
  const start = await toolPlace(argStr(a.start), true);
  if (typeof start === "string") return fail(start, "Départ introuvable");
  goRoutes();
  if (trackMap.drawing) setDrawing(false);
  const opts = await toolOptions(a);
  if (!loopMode) openLoop();
  if (!loopMode) return fail("le réseau routier est encore en préparation.", "Réseau routier pas prêt");
  setLoopWorkout(null);
  $<HTMLInputElement>("loop-km").value = String(dist);
  $<HTMLSelectElement>("loop-dir").value = TOOL_HEADING[argStr(a.direction)] ?? "";
  const err = await toolLoops(start);
  if (err) return fail(err, `Boucles de ${dist} km impossibles`);
  return {
    ok: true,
    label: `${loops.length} boucles de ${dist} km (${PROFILE_LABEL[options.profile].toLowerCase()}) depuis ${start.label}`,
    text: `${loops.length} boucles affichées sur la carte, départ : ${start.label}, ${opts} :\n${loopsText()}\nElle peut cliquer sur une boucle pour la mettre en avant, puis sur « Choisir » pour la garder et l'ajuster.`,
  };
}

async function toolPlanRoute(a: Record<string, unknown>, names: string[], pts: [number, number][], labels: string[]): Promise<ToolResult> {
  goRoutes();
  if (loopMode) closeLoop();
  closeLibrary();
  const opts = await toolOptions(a);
  if (!trackMap.drawing) setDrawing(true);
  if (!trackMap.drawing) return fail("le réseau routier est encore en préparation.", "Réseau routier pas prêt");
  routeName = `${names[0]} → ${names[names.length - 1]}`;
  currentFile = null;
  savedSig = null;
  if (!(await planner.setPoints(pts)) || lastLegs.length === 0) return fail("aucune route praticable entre ces lieux avec ces réglages.", "Itinéraire impossible");
  const joined = joinLegs(lastLegs);
  trackMap.fit(joined.coords);
  let ascent = "";
  try {
    const sum = await invoke<RouteSummary>("summarize_route", { coords: joined.coords, surface: joined.surface, class: joined.class });
    if (sum.track.stats.hasElevation) ascent = `, D+ ${Math.round(sum.track.stats.ascentM)} m`;
  } catch {
    /* bilan indisponible : la distance suffit */
  }
  return {
    ok: true,
    label: `Itinéraire ${routeName} tracé (${km(joined.lengthM)})`,
    text: `Itinéraire tracé et affiché : ${labels.join(" → ")}, ${km(joined.lengthM)}${ascent}, ${opts}. Elle peut déplacer les points sur la carte, l'enregistrer (Ctrl+S) ou l'exporter vers son Garmin.`,
  };
}

async function coachTool(name: string, a: Record<string, unknown>): Promise<ToolResult> {
  switch (name) {
    case "propose_loops":
      return toolProposeLoops(a);
    case "plan_route": {
      const names = [argStr(a.start), ...(Array.isArray(a.via) ? a.via.map(argStr).filter(Boolean) : []), argStr(a.end)];
      const pts: [number, number][] = [];
      const labels: string[] = [];
      for (const n of names) {
        const r = await toolPlace(n, false);
        if (typeof r === "string") return fail(r, "Lieu introuvable");
        pts.push(r.p);
        labels.push(r.label);
      }
      // Départ = arrivée : c'est une boucle
      if (pts.length === 2 && samePlace(pts[0], pts[1])) {
        if (Number(a.distance_km) > 0) return toolProposeLoops({ ...a, start: names[0] });
        return fail(
          "départ et arrivée identiques. Pour revenir au point de départ, appelle propose_loops avec start et distance_km.",
          "Départ et arrivée identiques",
          "c'est une boucle, le coach passe aux boucles",
        );
      }
      return toolPlanRoute(a, names, pts, labels);
    }
    case "set_route_options": {
      const opts = await toolOptions(a);
      return { ok: true, label: `Réglages du tracé : ${opts}`, text: `Réglages appliqués : ${opts}${planner.count >= 2 ? " ; l'itinéraire en cours a été recalculé" : ""}.` };
    }
    case "create_workout": {
      let w: Workout;
      try {
        w = await invoke<Workout>("workout_from_coach", { workout: a });
      } catch (err) {
        return fail(String(err), "Séance illisible");
      }
      if (mode !== "coach") setMode("coach");
      await coach.showWorkout(w, null, true);
      const total = w.blocks.reduce((t, b) => t + b.repeat * b.steps.reduce((x, st) => x + st.durationS, 0), 0);
      return {
        ok: true,
        label: `Séance « ${w.name} » créée`,
        text: `Séance « ${w.name} » affichée à droite (${fmtDuration(total)}, ${TERRAIN_LABEL[w.terrain].toLowerCase()}). Elle peut l'exporter vers son Garmin, la garder dans ses séances, ou demander un itinéraire adapté.`,
      };
    }
    case "show_workout_template": {
      const want = argStr(a.name).toLowerCase();
      const list = await coach.templateList();
      const w = list.find((x) => x.name.toLowerCase() === want) ?? list.find((x) => x.name.toLowerCase().includes(want.split(" ")[0] ?? ""));
      if (!w) return fail(`séance type « ${a.name} » inconnue.`, "Séance type introuvable");
      if (mode !== "coach") setMode("coach");
      await coach.showWorkout(w, null);
      return { ok: true, label: `Séance type « ${w.name} » affichée`, text: `Séance type « ${w.name} » affichée à droite. Objectif : ${w.goal}` };
    }
    case "route_for_workout": {
      const shown = coach.shownWorkout;
      if (!shown) return fail("aucune séance affichée : crée ou affiche d'abord une séance.", "Aucune séance affichée");
      const start = await toolPlace(argStr(a.start), true);
      if (typeof start === "string") return fail(start, "Départ introuvable");
      const info = shown.info ?? (await invoke<WorkoutInfo>("workout_info", { workout: shown.workout, athlete }));
      goRoutes();
      if (trackMap.drawing) setDrawing(false);
      if (!loopMode) openLoop();
      if (!loopMode) return fail("le réseau routier est encore en préparation.", "Réseau routier pas prêt");
      setLoopWorkout({ workout: shown.workout, info });
      const err = await toolLoops(start);
      if (err) return fail(err, "Boucles pour la séance impossibles");
      return {
        ok: true,
        label: `${loops.length} boucles pour « ${shown.workout.name} »`,
        text: `${loops.length} boucles de ${info.suggestedKm} km affichées, départ : ${start.label}, classées de la plus adaptée à la moins adaptée :\n${loopsText()}`,
      };
    }
    case "open_ride": {
      if (mode !== "rides") setMode("rides");
      setMenuOpen(true);
      await refreshRides();
      const want = argStr(a.date);
      const ride = !want || /latest|dernière|derniere|recent/i.test(want) ? rides[0] : rides.find((r) => r.startTime !== null && ymd(r.startTime) === want);
      if (!ride) return fail(rides.length ? `aucune sortie le ${want}.` : "aucune sortie importée.", "Sortie introuvable");
      await selectRide(ride.id);
      const d = rideDetail;
      const facts = d
        ? [
            km(d.analysis.distanceM),
            fmtDuration(d.analysis.movingS),
            `D+ ${Math.round(d.analysis.ascentM)} m`,
            `${d.analysis.avgSpeedKmh.toFixed(1)} km/h`,
            ...(d.analysis.power ? [`NP ${Math.round(d.analysis.power.np)} W`] : []),
          ].join(", ")
        : "";
      return {
        ok: true,
        label: `Sortie « ${ride.name} » ouverte`,
        text: `Sortie « ${ride.name} » du ${ride.startTime ? ymd(ride.startTime) : "?"} affichée (carte, courbes, analyse) : ${facts}. Elle peut demander l'avis du coach avec le bouton « Avis du coach ».`,
      };
    }
    case "show_progress": {
      if (mode !== "rides") setMode("rides");
      setMenuOpen(true);
      await showProgress();
      return { ok: true, label: "Progression affichée", text: "Vue Progression ouverte : forme, fatigue, fraîcheur, kilomètres par semaine, vitesse sur le plat." };
    }
    case "show_place": {
      const r = await toolPlace(argStr(a.query), false);
      if (typeof r === "string") return fail(r, "Lieu introuvable");
      goToPlace({ label: r.label, kind: "", center: r.p });
      return { ok: true, label: `Carte centrée sur ${r.label}`, text: `Carte centrée sur ${r.label}, avec un repère (un clic dessus permet d'en faire un départ, une arrivée ou une étape).` };
    }
    default:
      return fail(`action « ${name} » inconnue.`, `Action « ${name} » inconnue`);
  }
}

/** Ce que l'application affiche, pour le coach. */
function coachAppState(): string {
  const c = trackMap.map.getCenter();
  const view = { routes: "Itinéraires (carte)", rides: "Sorties", coach: "Coach" }[mode];
  const lines = [
    `Vue affichée : ${view}.`,
    `Réseau routier chargé : ${network.loaded ? network.region : "aucun"}.`,
    `Centre de la carte : ${c.lat.toFixed(3)} N, ${c.lng.toFixed(3)} E.`,
    `Réglages du tracé : ${PROFILE_LABEL[options.profile].toLowerCase()}, ${HILLS_LABEL[String(nearest(HILLS_STEPS, options.hills))]}.`,
  ];
  if (current?.kind === "route") lines.push(`Itinéraire en cours : « ${routeName} », ${km(current.track.stats.distanceM)}.`);
  if (loopMode && loops.length) lines.push(`Boucles proposées à l'écran :\n${loopsText()}`);
  const w = coach.shownWorkout;
  if (w) lines.push(`Séance affichée : « ${w.workout.name} » (${TERRAIN_LABEL[w.workout.terrain].toLowerCase()}).`);
  if (mode === "rides" && rideDetail) lines.push(`Sortie ouverte : « ${rideDetail.summary.name} ».`);
  return lines.join("\n");
}

function toast(msg: string, kind: "error" | "ok" = "error", ms = kind === "ok" ? 3500 : 6000) {
  const el = $("toast");
  el.textContent = msg;
  el.className = `toast ${kind}`;
  clearTimeout(toastTimer);
  if (ms > 0) toastTimer = window.setTimeout(() => el.classList.add("hidden"), ms);
}

// Toute erreur JavaScript non prévue s'affiche à l'écran au lieu de passer inaperçue.
window.addEventListener("error", (e) => toast(`Erreur : ${e.message}`));
window.addEventListener("unhandledrejection", (e) => toast(`Erreur : ${e.reason}`));

if (inTauri) {
  // Glisser-déposer : Tauri intercepte le fichier et nous donne directement son chemin.
  const drop = $("drop");
  getCurrentWebview()
    .onDragDropEvent((event) => {
      const p = event.payload;
      if (p.type === "enter" || p.type === "over") drop.classList.remove("hidden");
      else drop.classList.add("hidden");
      if (p.type === "drop" && p.paths.length > 0) {
        const file = p.paths[0];
        const activityFiles = p.paths.filter((f) => /\.(fit|tcx)$/i.test(f) || (mode === "rides" && /\.gpx$/i.test(f)));
        if (/\.pbf$/i.test(file)) toast("Pour un fichier .osm.pbf, utilise le bouton « Réseau » en haut à droite.");
        else if (activityFiles.length > 0) {
          setMode("rides");
          importRides(activityFiles);
        } else loadFromPath(file);
      }
    })
    .catch((err) => toast(`Glisser-déposer indisponible : ${err}`));

  // État du réseau routier (chargé en arrière-plan au démarrage)
  listen<NetworkStatus>("network-status", (e) => renderNetwork(e.payload));
  listen<string>("network-warning", (e) => toast(e.payload));
  listen<[number, number]>("import-progress", (e) => {
    const [done, total] = e.payload;
    const st = $("import-status");
    st.textContent = `Import : ${done} / ${total} fichiers…`;
    st.classList.remove("hidden");
  });
  listen<string>("network-progress", (e) => {
    $("net").classList.add("busy");
    $("net-label").textContent = e.payload;
    // Région chargée à la demande : la progression s'affiche aussi en bas, bien en vue
    if (regionLoad) toast(e.payload, "ok", 0);
    if (franceJob) {
      const p = $("nm-progress");
      p.textContent = e.payload;
      p.classList.remove("hidden");
      // La barre suit le total téléchargé (« total 1,2 / 4,6 Go »)
      const m = /total ([\d,.]+) \/ ([\d,.]+) Go/.exec(e.payload);
      if (m) {
        const num = (t: string) => Number(t.replace(",", "."));
        $("nm-bar").style.width = `${Math.min(100, (num(m[1]) / num(m[2])) * 100)}%`;
      }
    }
  });
  invoke<NetworkStatus>("network_status").then(renderNetwork);
}

// Écran d'ouverture puis vol jusqu'à la région
runIntro(trackMap);

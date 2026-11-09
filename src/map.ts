import maplibregl, { type GeoJSONSource, type LngLatBoundsLike, type Map as MlMap } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import type { Feature, FeatureCollection } from "geojson";
import { DEM_ATTRIBUTION, DEM_TILES, loadBasemap, PLACEHOLDER_STYLE, type BasemapId } from "./basemaps";
import type { Leg } from "./planner";
import type { Track } from "./types";

const ACCENT = "#fc4c02"; // orange Strava
const UNPAVED = "#9c4a2c";
const CASING = "#0b0b0d";

/** Couleurs des boucles proposées (dans l'ordre de la liste). */
export const LOOP_COLORS = ["#fc4c02", "#3e8bff", "#2bb673", "#a66bff", "#f0b429", "#ff5c8a", "#19c3c3", "#c8b08a"];
const EMPTY: FeatureCollection = { type: "FeatureCollection", features: [] };

/** Point de départ de la caméra après l'ouverture. */
const HOME: [number, number] = [-1.68, 48.11]; // Rennes
const HOME_ZOOM = 10.6;
/** Exagération du relief en 3D. */
const TERRAIN_EXAGGERATION = 1.35;
/** Inclinaison et cap de la caméra quand on bascule en 3D avec le bouton. */
const PITCH_3D = 62;
const BEARING_3D = -18;
/** En dessous de ce seuil (degrés), une inclinaison à la souris repasse en 2D ; au-dessus, en 3D. */
const PITCH_TO_2D = 4;
const PITCH_TO_3D = 12;

const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const easeInOutCubic = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ** 3 / 2);
const easeOutExpo = (t: number) => (t >= 1 ? 1 : 1 - 2 ** (-10 * t));

/** Carte MapLibre : fonds, relief, 2D/3D fluide, trace et curseur synchronisé avec le profil. */
export class TrackMap {
  readonly map: MlMap;
  /** Résolue quand le premier vrai fond de carte est affiché (tuiles comprises). */
  readonly ready: Promise<void>;
  private resolveReady: (() => void) | null = null;
  private basemapToken = 0;
  private track: Track | null = null;
  private hillshade = true;
  private terrain3d = false;
  /** Exagération du relief actuellement appliquée (animée entre 0 et TERRAIN_EXAGGERATION). */
  private exaggeration = 0;
  private exaggerationAnim = 0;
  private markers: maplibregl.Marker[] = [];
  private hoverFrame = 0;
  private routeData: FeatureCollection = EMPTY;
  /** Mode tracé : un clic sur la carte ajoute un point, un clic sur le tracé en insère un. */
  drawing = false;
  onMapClick: ((p: [number, number]) => void) | null = null;
  onRouteClick: ((legIndex: number, p: [number, number]) => void) | null = null;
  /** Clic hors mode tracé : infos sur le lieu cliqué. */
  onMapInfo: ((p: [number, number]) => void) | null = null;
  /** Mode boucle : un clic choisit le départ, un clic sur une proposition la sélectionne. */
  loopPicking = false;
  onLoopStart: ((p: [number, number]) => void) | null = null;
  onLoopClick: ((index: number) => void) | null = null;
  /** Prévient l'interface quand la vue passe en 2D ou en 3D (bouton, clavier ou souris). */
  onViewChange: ((is3d: boolean) => void) | null = null;
  /** Cap de la carte, pour faire tourner la boussole. */
  onBearing: ((bearing: number) => void) | null = null;
  private loopData: FeatureCollection = EMPTY;
  /** Portions clés des boucles de séance (montée ou plat), surlignées. */
  private loopKeyData: FeatureCollection = EMPTY;
  /** Trace dessinée sur la couche « track » (import ou sortie). */
  private shownTrack: Track | null = null;
  private loopStart: maplibregl.Marker | null = null;

  constructor(container: HTMLElement, private onHover: (index: number | null) => void) {
    this.ready = new Promise((res) => (this.resolveReady = res));
    this.map = new maplibregl.Map({
      container,
      style: PLACEHOLDER_STYLE,
      // Ouverture : la Terre entière, avant le vol jusqu'à la maison
      center: [4, 30],
      zoom: 1.15,
      maxPitch: 78,
      // Crédits affichés dans le menu « Réseau » plutôt que par un bouton sur la carte
      attributionControl: false,
      // Garde les tuiles déjà vues en mémoire : changement de fond et retours plus rapides.
      maxTileCacheZoomLevels: 6,
      fadeDuration: 250,
    });
    this.map.addControl(new maplibregl.ScaleControl({ unit: "metric", maxWidth: 80 }), "bottom-left");

    // Chaque changement de fond recharge le style : on remet relief, ciel et trace par-dessus.
    this.map.on("style.load", () => this.installOverlays());
    this.setBasemap("route");

    this.map.on("mousemove", (e) => {
      cancelAnimationFrame(this.hoverFrame);
      this.hoverFrame = requestAnimationFrame(() => this.handleHover(e.point));
    });
    this.map.on("mouseout", () => this.onHover(null));
    this.map.on("rotate", () => this.onBearing?.(this.map.getBearing()));

    // Inclinaison à la souris (clic droit + glisser) : la 3D s'active ou se coupe d'elle-même.
    this.map.on("pitch", (e) => {
      if (!(e as { originalEvent?: Event }).originalEvent) return; // mouvements programmés ignorés
      const pitch = this.map.getPitch();
      if (!this.terrain3d && pitch > PITCH_TO_3D) this.setView3d(true, false);
      else if (this.terrain3d && pitch < PITCH_TO_2D) this.setView3d(false, false);
    });

    this.map.on("click", (e) => {
      if (this.loopPicking) {
        const box: [maplibregl.PointLike, maplibregl.PointLike] = [
          [e.point.x - 6, e.point.y - 6],
          [e.point.x + 6, e.point.y + 6],
        ];
        const hit = this.map.getLayer("loops-casing")
          ? this.map.queryRenderedFeatures(box, { layers: ["loops-casing"] })[0]
          : undefined;
        if (hit) this.onLoopClick?.(hit.properties.i as number);
        else this.onLoopStart?.([e.lngLat.lng, e.lngLat.lat]);
        return;
      }
      const p: [number, number] = [e.lngLat.lng, e.lngLat.lat];
      if (!this.drawing) {
        this.onMapInfo?.(p);
        return;
      }
      const box: [maplibregl.PointLike, maplibregl.PointLike] = [
        [e.point.x - 6, e.point.y - 6],
        [e.point.x + 6, e.point.y + 6],
      ];
      const hit = this.map.getLayer("route-casing")
        ? this.map.queryRenderedFeatures(box, { layers: ["route-casing"] })[0]
        : undefined;
      if (hit && this.onRouteClick) this.onRouteClick(hit.properties.leg as number, p);
      else this.onMapClick?.(p);
    });
  }

  get is3d() {
    return this.terrain3d;
  }

  /** Vol d'ouverture : de la Terre vue de l'espace jusqu'à la région de départ. */
  playIntro() {
    if (this.track) return; // une trace a déjà été ouverte : on ne déplace pas la carte
    if (reducedMotion()) {
      this.map.jumpTo({ center: HOME, zoom: HOME_ZOOM });
      return;
    }
    this.map.flyTo({
      center: HOME,
      zoom: HOME_ZOOM,
      bearing: 0,
      pitch: 0,
      duration: 4600,
      curve: 1.35,
      easing: easeInOutCubic,
      essential: true,
    });
  }

  setDrawing(on: boolean) {
    this.drawing = on;
    this.map.getCanvas().style.cursor = on ? "crosshair" : "";
    this.map.doubleClickZoom[on ? "disable" : "enable"]();
  }

  /** Itinéraire tracé : une ligne par portion de même revêtement, avec l'index de l'étape. */
  setRoute(legs: Leg[]) {
    const features: Feature[] = [];
    legs.forEach((leg, li) => {
      let start = 0;
      for (let k = 1; k <= leg.surface.length; k++) {
        if (k === leg.surface.length || leg.surface[k] !== leg.surface[start]) {
          features.push({
            type: "Feature",
            properties: { leg: li, s: leg.surface[start] },
            geometry: { type: "LineString", coordinates: leg.coords.slice(start, k + 1) },
          });
          start = k;
        }
      }
    });
    this.routeData = { type: "FeatureCollection", features };
    this.map.getSource<GeoJSONSource>("route")?.setData(this.routeData);
    this.updateBuildings();
  }

  /** Mode boucle : curseur et marqueur de départ. */
  setLoopPicking(on: boolean) {
    this.loopPicking = on;
    this.map.getCanvas().style.cursor = on ? "crosshair" : this.drawing ? "crosshair" : "";
    if (on) this.map.doubleClickZoom.disable();
    else if (!this.drawing) this.map.doubleClickZoom.enable();
    if (!on) {
      this.setLoopStart(null);
      this.setLoopPreviews([], null);
      this.setLoopKeys([], null);
    }
  }

  setLoopStart(p: [number, number] | null) {
    if (!p) {
      this.loopStart?.remove();
      this.loopStart = null;
      return;
    }
    if (!this.loopStart) {
      const el = document.createElement("div");
      el.className = "wp start loop-start";
      el.title = "Départ de la boucle";
      this.loopStart = new maplibregl.Marker({ element: el }).setLngLat(p).addTo(this.map);
    } else {
      this.loopStart.setLngLat(p);
    }
  }

  /** Boucles proposées ; la sélectionnée est mise en avant, les autres estompées. */
  setLoopPreviews(lines: [number, number][][], selected: number | null) {
    const features: Feature[] = lines.map((coords, i) => ({
      type: "Feature",
      properties: { i, c: LOOP_COLORS[i % LOOP_COLORS.length], sel: selected === null || selected === i ? 1 : 0 },
      geometry: { type: "LineString", coordinates: coords },
    }));
    // La sélectionnée est dessinée en dernier, donc par-dessus
    features.sort((a, b) => (a.properties!.sel as number) - (b.properties!.sel as number));
    this.loopData = { type: "FeatureCollection", features };
    this.map.getSource<GeoJSONSource>("loops")?.setData(this.loopData);
  }

  /** Portions clés des boucles (séance) : surlignées, celle de la boucle choisie plus fort. */
  setLoopKeys(lines: [number, number][][], selected: number | null) {
    const features: Feature[] = lines
      .map((coords, i) => ({ coords, i }))
      .filter((x) => x.coords.length >= 2)
      .map(({ coords, i }) => ({
        type: "Feature",
        properties: { i, sel: selected === null || selected === i ? 1 : 0 },
        geometry: { type: "LineString", coordinates: coords },
      }));
    this.loopKeyData = { type: "FeatureCollection", features };
    this.map.getSource<GeoJSONSource>("loop-keys")?.setData(this.loopKeyData);
  }

  /** Trace de référence pour le survol et le profil, sans la dessiner (l'itinéraire l'est déjà). */
  setRouteTrack(track: Track | null) {
    this.track = track;
    if (this.shownTrack) {
      this.shownTrack = null;
      this.updateBuildings();
    }
    this.markers.forEach((mk) => mk.remove());
    this.markers = [];
    this.map.getSource<GeoJSONSource>("track")?.setData(EMPTY);
  }

  clearTrack() {
    this.track = null;
    if (this.shownTrack) {
      this.shownTrack = null;
      this.updateBuildings();
    }
    this.markers.forEach((mk) => mk.remove());
    this.markers = [];
    this.map.getSource<GeoJSONSource>("track")?.setData(EMPTY);
  }

  setBasemap(id: BasemapId) {
    const token = ++this.basemapToken;
    loadBasemap(id).then((style) => {
      if (token !== this.basemapToken) return; // un autre fond a été choisi entre-temps
      this.map.setStyle(style, { diff: false });
      this.map.once("idle", () => {
        this.resolveReady?.();
        this.resolveReady = null;
      });
    });
  }

  setHillshade(on: boolean) {
    this.hillshade = on;
    if (this.map.getLayer("hillshade")) {
      this.map.setLayoutProperty("hillshade", "visibility", on ? "visible" : "none");
    }
  }

  /** Bascule 2D ↔ 3D : la caméra s'incline pendant que le relief « pousse » du sol. */
  set3d(on: boolean) {
    this.setView3d(on, true);
  }

  setTrack(track: Track) {
    this.track = track;
    this.shownTrack = track;
    this.updateTrackSource();
    this.updateBuildings();
    this.placeMarkers();
    this.fit();
  }

  fit(coords: [number, number][] | undefined = this.track?.coords) {
    if (!coords || coords.length === 0) return;
    this.map.resize();
    let [w, s, e, n] = [180, 90, -180, -90];
    for (const [lon, lat] of coords) {
      if (lon < w) w = lon;
      if (lon > e) e = lon;
      if (lat < s) s = lat;
      if (lat > n) n = lat;
    }
    this.map.fitBounds([[w, s], [e, n]] as LngLatBoundsLike, {
      padding: this.freeArea(),
      bearing: this.map.getBearing(),
      duration: reducedMotion() ? 0 : 1400,
      easing: easeOutExpo,
      maxZoom: 16,
    });
  }

  /** Affiche (ou cache, si null) le point courant sur la trace. */
  setCursor(index: number | null) {
    const src = this.map.getSource<GeoJSONSource>("cursor");
    if (!src) return;
    if (index === null || !this.track) {
      src.setData(EMPTY);
      return;
    }
    src.setData({ type: "Feature", properties: {}, geometry: { type: "Point", coordinates: this.track.coords[index] } });
  }

  // --- interne -------------------------------------------------------------

  private setView3d(on: boolean, moveCamera: boolean) {
    if (on === this.terrain3d && !moveCamera) return;
    this.terrain3d = on;
    this.onViewChange?.(on);
    this.updateBuildings();
    const duration = reducedMotion() ? 0 : 1700;
    this.animateExaggeration(on ? TERRAIN_EXAGGERATION : 0, moveCamera ? duration : duration * 0.7);
    if (!moveCamera) return;
    const bearing = this.map.getBearing();
    this.map.easeTo({
      pitch: on ? PITCH_3D : 0,
      // On garde le cap choisi par l'utilisateur ; sinon une légère rotation donne de la profondeur.
      bearing: on ? (Math.abs(bearing) > 1 ? bearing : BEARING_3D) : 0,
      duration,
      easing: easeInOutCubic,
      essential: true,
    });
  }

  /** Fait varier l'exagération du relief image par image (0 = plat, donc identique à la 2D). */
  private animateExaggeration(target: number, duration: number) {
    cancelAnimationFrame(this.exaggerationAnim);
    const from = this.exaggeration;
    const t0 = performance.now();
    const step = (now: number) => {
      const t = duration > 0 ? Math.min(1, (now - t0) / duration) : 1;
      this.exaggeration = from + (target - from) * easeInOutCubic(t);
      this.applyTerrain();
      if (t < 1) this.exaggerationAnim = requestAnimationFrame(step);
    };
    this.exaggerationAnim = requestAnimationFrame(step);
  }

  private applyTerrain() {
    if (!this.map.getSource("dem-terrain")) return;
    const terrain = (this.map as unknown as { terrain: { exaggeration: number } | null }).terrain;
    if (this.exaggeration < 0.002) {
      if (terrain) this.map.setTerrain(null);
    } else if (!terrain) {
      this.map.setTerrain({ source: "dem-terrain", exaggeration: this.exaggeration });
    } else {
      // Modifier l'objet existant évite de recréer le maillage du relief à chaque image.
      terrain.exaggeration = this.exaggeration;
      this.map.triggerRepaint();
    }
  }

  /** Marges de la carte laissées libres par les panneaux, pour cadrer une trace. */
  private freeArea() {
    const W = window.innerWidth;
    const H = window.innerHeight;
    const shown = (id: string) => {
      const el = document.getElementById(id);
      return el && el.getClientRects().length > 0 ? el.getBoundingClientRect() : null;
    };
    const left = Math.max(0, ...["stats", "loop-panel", "library-panel", "rides-panel"].map((id) => shown(id)?.right ?? 0));
    const rightEdge = Math.min(W - 64, ...["settings", "ride-detail"].map((id) => shown(id)?.left ?? W));
    const sheet = shown("profile") ?? shown("ride-chart");
    const top = (shown("dock")?.top ?? 90) + 24;
    const pad = { top, left: left + 40, right: W - rightEdge + 40, bottom: sheet ? H - sheet.top + 36 : 60 };
    // Fenêtre étroite : on rogne les marges pour garder au moins 160 px de carte.
    const squeezeX = Math.max(0, pad.left + pad.right - (W - 160));
    pad.left -= squeezeX / 2;
    pad.right -= squeezeX / 2;
    const squeezeY = Math.max(0, pad.top + pad.bottom - (H - 140));
    pad.top -= squeezeY / 2;
    pad.bottom -= squeezeY / 2;
    return pad;
  }

  private installOverlays() {
    const m = this.map;
    // Globe aux petites échelles (vol d'ouverture), Mercator classique dès qu'on zoome.
    m.setProjection({ type: "globe" });
    // Ciel nocturne avec une lueur orangée à l'horizon, visible en 3D.
    m.setSky({
      "sky-color": "#08080a",
      "horizon-color": "#2a160c",
      "fog-color": "#0b0b0d",
      "sky-horizon-blend": 0.55,
      "horizon-fog-blend": 0.7,
      "fog-ground-blend": 0.35,
      "atmosphere-blend": ["interpolate", ["linear"], ["zoom"], 0, 0.9, 5, 0.9, 7, 0],
    });

    m.addSource("dem-hillshade", { type: "raster-dem", tiles: DEM_TILES, encoding: "terrarium", tileSize: 256, maxzoom: 15, attribution: DEM_ATTRIBUTION });
    m.addSource("dem-terrain", { type: "raster-dem", tiles: DEM_TILES, encoding: "terrarium", tileSize: 256, maxzoom: 15 });

    // Relief ombré sous les étiquettes du fond (s'il en a) pour qu'elles restent lisibles.
    const firstSymbol = m.getStyle().layers.find((l) => l.type === "symbol")?.id;
    m.addLayer(
      {
        id: "hillshade",
        type: "hillshade",
        source: "dem-hillshade",
        layout: { visibility: this.hillshade ? "visible" : "none" },
        paint: {
          "hillshade-exaggeration": 0.45,
          "hillshade-shadow-color": "#000000",
          "hillshade-highlight-color": "rgba(255, 236, 220, 0.16)",
          "hillshade-accent-color": "#000000",
        },
      },
      firstSymbol,
    );

    const width = (a: number, b: number) => ["interpolate", ["linear"], ["zoom"], 8, a, 16, b] as maplibregl.ExpressionSpecification;

    // Trace importée : halo lumineux, liseré sombre, ligne orange
    m.addSource("track", { type: "geojson", data: EMPTY, tolerance: 0.2 });
    m.addLayer({
      id: "track-glow",
      type: "line",
      source: "track",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": ACCENT, "line-width": width(12, 26), "line-blur": 10, "line-opacity": 0.3 },
    });
    m.addLayer({
      id: "track-casing",
      type: "line",
      source: "track",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": CASING, "line-opacity": 0.85, "line-width": width(5.5, 11) },
    });
    m.addLayer({
      id: "track-line",
      type: "line",
      source: "track",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": ACCENT, "line-width": width(3, 6.5) },
    });

    // Itinéraire tracé : orange plein, pointillés clairs par-dessus là où le goudron n'est que présumé.
    m.addSource("route", { type: "geojson", data: this.routeData, tolerance: 0.2 });
    m.addLayer({
      id: "route-glow",
      type: "line",
      source: "route",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": ACCENT, "line-width": width(12, 26), "line-blur": 10, "line-opacity": 0.3 },
    });
    m.addLayer({
      id: "route-casing",
      type: "line",
      source: "route",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": CASING, "line-opacity": 0.85, "line-width": width(6.5, 12) },
    });
    m.addLayer({
      id: "route-line",
      type: "line",
      source: "route",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: {
        "line-color": ["match", ["get", "s"], "Unpaved", UNPAVED, ACCENT],
        "line-width": width(3.5, 7),
      },
    });
    m.addLayer({
      id: "route-assumed",
      type: "line",
      source: "route",
      filter: ["==", ["get", "s"], "Assumed"],
      layout: { "line-join": "round" },
      paint: {
        "line-color": "rgba(255, 255, 255, 0.85)",
        "line-width": width(1.2, 2.5),
        "line-dasharray": [2, 2.5],
      },
    });

    // Boucles proposées
    m.addSource("loops", { type: "geojson", data: this.loopData, tolerance: 0.3 });
    m.addLayer({
      id: "loops-casing",
      type: "line",
      source: "loops",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: {
        "line-color": CASING,
        "line-width": ["case", ["==", ["get", "sel"], 1], 10, 7],
        "line-opacity": ["case", ["==", ["get", "sel"], 1], 0.9, 0.5],
      },
    });
    // Portion clé d'une boucle de séance : halo blanc sous la couleur de la boucle
    m.addSource("loop-keys", { type: "geojson", data: this.loopKeyData, tolerance: 0.3 });
    m.addLayer({
      id: "loop-keys-halo",
      type: "line",
      source: "loop-keys",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: {
        "line-color": "#ffffff",
        "line-width": ["case", ["==", ["get", "sel"], 1], 16, 11],
        "line-opacity": ["case", ["==", ["get", "sel"], 1], 0.55, 0.18],
        "line-blur": 2,
      },
    });
    m.addLayer({
      id: "loops-line",
      type: "line",
      source: "loops",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: {
        "line-color": ["get", "c"],
        "line-width": ["case", ["==", ["get", "sel"], 1], 6, 4],
        "line-opacity": ["case", ["==", ["get", "sel"], 1], 1, 0.4],
      },
    });

    // Point courant (survol du profil ou de la trace)
    m.addSource("cursor", { type: "geojson", data: EMPTY });
    m.addLayer({
      id: "cursor-halo",
      type: "circle",
      source: "cursor",
      paint: { "circle-radius": 18, "circle-color": ACCENT, "circle-opacity": 0.22, "circle-blur": 0.4 },
    });
    m.addLayer({
      id: "cursor",
      type: "circle",
      source: "cursor",
      paint: { "circle-radius": 7, "circle-color": ACCENT, "circle-stroke-color": "#fff", "circle-stroke-width": 3 },
    });

    this.applyTerrain();
    this.updateTrackSource();
    this.updateBuildings();
  }

  /**
   * En 3D, les bâtiments en volume masquent la trace dans les rues : quand une trace est
   * affichée, on les rend translucides pour qu'elle reste visible sur tout son parcours.
   */
  private updateBuildings() {
    const style = this.map.getStyle();
    if (!style) return;
    const fade = this.terrain3d && (this.routeData.features.length > 0 || this.shownTrack !== null);
    for (const l of style.layers) {
      if (l.type !== "fill-extrusion") continue;
      this.map.setPaintProperty(l.id, "fill-extrusion-opacity", fade ? 0.28 : 0.85);
    }
  }


  private updateTrackSource() {
    const src = this.map.getSource<GeoJSONSource>("track");
    if (!src || !this.track) return;
    src.setData({ type: "Feature", properties: {}, geometry: { type: "LineString", coordinates: this.track.coords } });
  }

  private placeMarkers() {
    this.markers.forEach((mk) => mk.remove());
    this.markers = [];
    if (!this.track) return;
    const c = this.track.coords;
    const mk = (cls: string, title: string, at: [number, number]) => {
      const el = document.createElement("div");
      el.className = `pin ${cls}`;
      el.title = title;
      this.markers.push(new maplibregl.Marker({ element: el }).setLngLat(at).addTo(this.map));
    };
    const loop = c.length > 1 && distanceSq(c[0], c[c.length - 1]) < 1e-7; // ~30 m
    if (loop) {
      mk("start-end", "Départ et arrivée", c[0]);
    } else {
      mk("end", "Arrivée", c[c.length - 1]);
      mk("start", "Départ", c[0]);
    }
  }

  /** Survol de la carte : si la souris est sur la trace, on trouve le point le plus proche. */
  private handleHover(px: maplibregl.Point) {
    if (!this.track || !this.map.getLayer("track-casing")) return;
    const layers = ["track-casing", "route-casing"];
    const box: [maplibregl.PointLike, maplibregl.PointLike] = [
      [px.x - 8, px.y - 8],
      [px.x + 8, px.y + 8],
    ];
    if (this.map.queryRenderedFeatures(box, { layers }).length === 0) {
      this.map.getCanvas().style.cursor = this.drawing ? "crosshair" : "";
      this.onHover(null);
      return;
    }
    this.map.getCanvas().style.cursor = this.drawing ? "copy" : "crosshair";
    const ll = this.map.unproject(px);
    const k = Math.cos((ll.lat * Math.PI) / 180);
    let best = 0;
    let bestD = Infinity;
    const c = this.track.coords;
    for (let i = 0; i < c.length; i++) {
      const dx = (c[i][0] - ll.lng) * k;
      const dy = c[i][1] - ll.lat;
      const d = dx * dx + dy * dy;
      if (d < bestD) {
        bestD = d;
        best = i;
      }
    }
    this.onHover(best);
  }
}

function distanceSq(a: [number, number], b: [number, number]) {
  const k = Math.cos((a[1] * Math.PI) / 180);
  return ((a[0] - b[0]) * k) ** 2 + (a[1] - b[1]) ** 2;
}

import type { LayerSpecification, StyleSpecification } from "maplibre-gl";

export type BasemapId = "route" | "topo" | "satellite";

/** Fond commun à tous les styles : presque noir, comme l'interface. */
export const MAP_BG = "#0b0b0d";

/** Style raster minimal (une seule couche de tuiles), réglé pour le thème sombre. */
function rasterStyle(
  tiles: string[],
  attribution: string,
  maxzoom: number,
  paint: Record<string, number>,
  tileSize = 256,
): StyleSpecification {
  return {
    version: 8,
    // Polices nécessaires si on ajoute plus tard des étiquettes vectorielles.
    glyphs: "https://tiles.openfreemap.org/fonts/{fontstack}/{range}.pbf",
    sources: { base: { type: "raster", tiles, tileSize, maxzoom, attribution } },
    layers: [
      { id: "background", type: "background", paint: { "background-color": MAP_BG } },
      { id: "base", type: "raster", source: "base", paint },
    ],
  };
}

const IGN_ORTHO =
  "https://data.geopf.fr/wmts?SERVICE=WMTS&REQUEST=GetTile&VERSION=1.0.0" +
  "&LAYER=ORTHOIMAGERY.ORTHOPHOTOS&STYLE=normal&TILEMATRIXSET=PM&FORMAT=image/jpeg" +
  "&TILEMATRIX={z}&TILEROW={y}&TILECOL={x}";

const LIBERTY_URL = "https://tiles.openfreemap.org/styles/liberty";

/**
 * Fonds de carte, tous gratuits et sans clé, assombris pour le thème :
 * - Carte : OpenFreeMap « Liberty » (tuiles vectorielles OSM) recolorée en sombre à la volée.
 * - Topo : OpenTopoMap, luminosité inversée par le GPU (courbes claires sur fond sombre).
 * - Satellite : orthophotos IGN, légèrement assombries.
 */
const RASTERS: Record<Exclude<BasemapId, "route">, StyleSpecification> = {
  topo: rasterStyle(
    ["a", "b", "c"].map((s) => `https://${s}.tile.opentopomap.org/{z}/{x}/{y}.png`),
    '© <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors, SRTM · style © <a href="https://opentopomap.org">OpenTopoMap</a> (CC-BY-SA)',
    17,
    // min > max : la luminosité est inversée ; la rotation de teinte rend aux forêts leur vert.
    { "raster-brightness-min": 0.93, "raster-brightness-max": 0.06, "raster-hue-rotate": 180, "raster-saturation": -0.35, "raster-contrast": 0.08 },
  ),
  satellite: rasterStyle([IGN_ORTHO], '© <a href="https://www.ign.fr">IGN</a> — Géoplateforme', 19, {
    "raster-brightness-max": 0.82,
    "raster-saturation": -0.12,
    "raster-contrast": 0.06,
  }),
};

let darkVector: Promise<StyleSpecification | string> | null = null;

/** Style prêt à passer à `map.setStyle`. Le fond vectoriel est téléchargé une fois puis gardé. */
export function loadBasemap(id: BasemapId): Promise<StyleSpecification | string> {
  if (id !== "route") return Promise.resolve(RASTERS[id]);
  darkVector ??= fetch(LIBERTY_URL)
    .then((r) => {
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      return r.json() as Promise<StyleSpecification>;
    })
    .then(darkify)
    .catch((err) => {
      console.warn("Fond sombre indisponible, style d'origine utilisé :", err);
      darkVector = null;
      return LIBERTY_URL;
    });
  return darkVector;
}

/** Style vide affiché le temps que le vrai fond arrive. */
export const PLACEHOLDER_STYLE: StyleSpecification = {
  version: 8,
  sources: {},
  layers: [{ id: "background", type: "background", paint: { "background-color": MAP_BG } }],
};

/** Modèle numérique de terrain mondial (format Terrarium) pour l'ombrage et la 3D. */
export const DEM_TILES = ["https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png"];
export const DEM_ATTRIBUTION = 'Relief : <a href="https://github.com/tilezen/joerd/blob/master/docs/attribution.md">Mapzen Terrain Tiles</a>';

// --- Recoloration sombre du style vectoriel ----------------------------------

type Rgba = [number, number, number, number];
type Hsla = [number, number, number, number];

const NAMED: Record<string, Rgba> = {
  white: [255, 255, 255, 1],
  black: [0, 0, 0, 1],
  transparent: [0, 0, 0, 0],
};

function parseColor(s: string): Rgba | null {
  const v = s.trim().toLowerCase();
  if (NAMED[v]) return NAMED[v];
  if (v.startsWith("#")) {
    let h = v.slice(1);
    if (h.length === 3 || h.length === 4) h = [...h].map((c) => c + c).join("");
    if (!/^[0-9a-f]{6}([0-9a-f]{2})?$/.test(h)) return null;
    const n = parseInt(h.slice(0, 6), 16);
    const a = h.length === 8 ? parseInt(h.slice(6), 16) / 255 : 1;
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255, a];
  }
  const m = /^(rgba?|hsla?)\(([^)]+)\)$/.exec(v);
  if (!m) return null;
  const parts = m[2].split(/[\s,/]+/).filter(Boolean).map((p) => parseFloat(p));
  if (parts.length < 3 || parts.some(Number.isNaN)) return null;
  const a = parts[3] ?? 1;
  if (m[1].startsWith("rgb")) return [parts[0], parts[1], parts[2], a];
  return [...hslToRgb([parts[0], parts[1] / 100, parts[2] / 100, a])] as Rgba;
}

function rgbToHsl([r, g, b, a]: Rgba): Hsla {
  r /= 255;
  g /= 255;
  b /= 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  let h = 0;
  let s = 0;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h *= 60;
  }
  return [h, s, l, a];
}

function hslToRgb([h, s, l, a]: Hsla): Rgba {
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const x = c * (1 - Math.abs(((h / 60) % 2) - 1));
  const m = l - c / 2;
  const [r, g, b] =
    h < 60 ? [c, x, 0] : h < 120 ? [x, c, 0] : h < 180 ? [0, c, x] : h < 240 ? [0, x, c] : h < 300 ? [x, 0, c] : [c, 0, x];
  return [Math.round((r + m) * 255), Math.round((g + m) * 255), Math.round((b + m) * 255), a];
}

const fmt = ([r, g, b, a]: Rgba) => `rgba(${r}, ${g}, ${b}, ${+a.toFixed(3)})`;

/** Applique `f` à chaque couleur trouvée dans une valeur de style (couleur simple, expression, stops). */
function mapColors(v: unknown, f: (c: Hsla) => Hsla): unknown {
  if (typeof v === "string") {
    const c = parseColor(v);
    return c ? fmt(hslToRgb(f(rgbToHsl(c)))) : v;
  }
  if (Array.isArray(v)) return v.map((x) => mapColors(x, f));
  if (v && typeof v === "object") {
    const o = { ...(v as Record<string, unknown>) };
    if (Array.isArray(o.stops)) o.stops = (o.stops as unknown[][]).map(([z, c]) => [z, mapColors(c, f)]);
    return o;
  }
  return v;
}

const clamp = (x: number, a = 0, b = 1) => Math.min(b, Math.max(a, x));
const fixed = (hex: string) => () => rgbToHsl(parseColor(hex)!);

// Transformations par famille de couches (teinte gardée, saturation et luminosité ramenées dans le noir)
const T = {
  land: ([h, s, l, a]: Hsla): Hsla => [h, clamp(s * 0.32), 0.058 + (1 - l) * 0.11, a],
  road: ([h, s, l, a]: Hsla): Hsla => [h, clamp(s * 0.22), 0.17 + l * 0.13, a],
  casing: ([h, s, , a]: Hsla): Hsla => [h, clamp(s * 0.1), 0.045, a],
  line: ([h, s, l, a]: Hsla): Hsla => [h, clamp(s * 0.25), 0.14 + l * 0.1, a],
  text: ([h, s, l, a]: Hsla): Hsla => [h, clamp(s * 0.28), 0.6 + (1 - l) * 0.2, a],
  halo: ([h, , , a]: Hsla): Hsla => [h, 0.05, 0.04, Math.min(a, 0.9)],
  water: fixed("#0a1219"),
  waterLine: fixed("#122230"),
  building: fixed("#18181c"),
  boundary: fixed("#3a3a44"),
  rail: fixed("#2a2a30"),
};

const has = (id: string, ...keys: string[]) => keys.some((k) => id.includes(k));

function recolor(paint: Record<string, unknown> | undefined, keys: string[], f: (c: Hsla) => Hsla) {
  if (!paint) return;
  for (const k of keys) if (k in paint) paint[k] = mapColors(paint[k], f);
}

/** Transforme le style clair « Liberty » en version sombre, couche par couche. */
function darkify(style: StyleSpecification): StyleSpecification {
  const layers: LayerSpecification[] = [];
  for (const src of style.layers) {
    const layer = structuredClone(src) as LayerSpecification & {
      paint?: Record<string, unknown>;
      layout?: Record<string, unknown>;
    };
    const id = layer.id.toLowerCase();
    const p = layer.paint;
    switch (layer.type) {
      case "background":
        layer.paint = { ...p, "background-color": MAP_BG };
        break;
      case "fill":
        if (has(id, "water", "ocean", "lake", "river")) recolor(p, ["fill-color", "fill-outline-color"], T.water);
        else if (has(id, "building")) recolor(p, ["fill-color", "fill-outline-color"], T.building);
        else recolor(p, ["fill-color", "fill-outline-color"], T.land);
        break;
      case "fill-extrusion":
        layer.paint = { ...p, "fill-extrusion-color": "#1b1b20", "fill-extrusion-opacity": 0.85 };
        break;
      case "line":
        if (has(id, "water", "river", "stream", "canal")) recolor(p, ["line-color"], T.waterLine);
        else if (has(id, "casing")) recolor(p, ["line-color"], T.casing);
        else if (has(id, "rail", "transit")) recolor(p, ["line-color"], T.rail);
        else if (has(id, "boundary", "admin")) recolor(p, ["line-color"], T.boundary);
        else if (has(id, "road", "highway", "bridge", "tunnel", "street", "path", "track", "cycle", "motorway", "transport"))
          recolor(p, ["line-color"], T.road);
        else recolor(p, ["line-color"], T.line);
        break;
      case "symbol": {
        // Points d'intérêt masqués : la carte reste épurée, centrée sur la route.
        if (id.startsWith("poi")) {
          layer.layout = { ...layer.layout, visibility: "none" };
        }
        recolor(p, ["text-color"], T.text);
        recolor(p, ["text-halo-color"], T.halo);
        if (layer.layout && "icon-image" in layer.layout) layer.paint = { ...layer.paint, "icon-opacity": 0.55 };
        break;
      }
      case "raster":
        // Imagerie de très petite échelle (Natural Earth…) : ramenée dans le noir
        layer.paint = { ...p, "raster-brightness-max": 0.28, "raster-saturation": -0.6 };
        break;
      case "circle":
        recolor(p, ["circle-color", "circle-stroke-color"], T.line);
        break;
    }
    layers.push(layer);
  }
  return { ...style, layers };
}

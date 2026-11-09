/**
 * Recherche de lieux et noms des points de passage, via le service de géocodage
 * de l'IGN (Géoplateforme, gratuit, sans clé). Hors ligne : on retombe sur les coordonnées.
 */

type LngLat = [number, number];

const API = "https://data.geopf.fr/geocodage";

export interface Place {
  label: string;
  /** Précision : commune, rue, adresse, lieu-dit… */
  kind: string;
  center: LngLat;
}

interface Feature {
  geometry: { coordinates: LngLat };
  properties: { label?: string; name?: string; city?: string; type?: string; context?: string; distance?: number };
}

const KIND: Record<string, string> = {
  municipality: "Commune",
  street: "Rue",
  housenumber: "Adresse",
  locality: "Lieu-dit",
};

/** Lieux correspondant au texte tapé (France). */
export async function searchPlaces(text: string, signal?: AbortSignal): Promise<Place[]> {
  const q = text.trim();
  if (q.length < 3) return [];
  const res = await fetch(`${API}/search?q=${encodeURIComponent(q)}&limit=6&autocomplete=1`, { signal });
  if (!res.ok) throw new Error(`Recherche indisponible (${res.status})`);
  const data = (await res.json()) as { features?: Feature[] };
  return (data.features ?? []).map((f) => ({
    label: f.properties.label ?? f.properties.name ?? q,
    kind: [KIND[f.properties.type ?? ""] ?? "", f.properties.context ?? ""].filter(Boolean).join(" · "),
    center: f.geometry.coordinates,
  }));
}

export interface ReverseInfo {
  /** Adresse complète (« 12 Rue de la Paix 35000 Rennes »). */
  label: string;
  /** Précision et contexte (« Adresse · 35, Ille-et-Vilaine, Bretagne »). */
  kind: string;
  /** Distance (m) entre le point cliqué et l'adresse trouvée. */
  distanceM: number | null;
}

/** Adresse la plus proche d'un point cliqué, avec son contexte. Null si rien de proche. */
export async function reverseInfo(p: LngLat, signal?: AbortSignal): Promise<ReverseInfo | null> {
  const res = await fetch(`${API}/reverse?lon=${p[0]}&lat=${p[1]}&limit=1`, { signal });
  if (!res.ok) throw new Error(`Adresse indisponible (${res.status})`);
  const data = (await res.json()) as { features?: Feature[] };
  const f = data.features?.[0]?.properties;
  if (!f) return null;
  return {
    label: f.label ?? ([f.name, f.city].filter(Boolean).join(" ") || coordLabel(p)),
    kind: [KIND[f.type ?? ""] ?? "", f.context ?? ""].filter(Boolean).join(" · "),
    distanceM: typeof f.distance === "number" ? f.distance : null,
  };
}

const cache = new Map<string, Promise<string>>();
const keyOf = (p: LngLat) => `${p[0].toFixed(4)},${p[1].toFixed(4)}`;

export const coordLabel = (p: LngLat) => `${p[1].toFixed(4)}, ${p[0].toFixed(4)}`;

/** Nom lisible d'un point (rue et commune), mis en cache. */
export function placeName(p: LngLat): Promise<string> {
  const k = keyOf(p);
  let hit = cache.get(k);
  if (!hit) {
    hit = fetch(`${API}/reverse?lon=${p[0]}&lat=${p[1]}&limit=1`)
      .then((r) => (r.ok ? r.json() : null))
      .then((data: { features?: Feature[] } | null) => {
        const f = data?.features?.[0]?.properties;
        if (!f) return coordLabel(p);
        if (f.type === "municipality") return f.city ?? f.label ?? coordLabel(p);
        return [f.name, f.city].filter(Boolean).join(" · ") || f.label || coordLabel(p);
      })
      .catch(() => {
        cache.delete(k); // on retentera plus tard (réseau revenu)
        return coordLabel(p);
      });
    cache.set(k, hit);
  }
  return hit;
}

import type { TrackMap } from "./map";

/** Durée minimale de l'écran d'ouverture, le temps que le logo se dessine. */
const MIN_INTRO_MS = 2000;
/** Si la carte tarde (hors ligne, réseau lent), on ouvre quand même. */
const MAX_WAIT_MS = 6500;

/**
 * Ouverture : logo animé pendant le chargement du fond de carte, puis le rideau s'ouvre,
 * la caméra vole depuis le globe jusqu'à la position de l'utilisateur (`here`, si elle est
 * connue) ou jusqu'à la France entière, et l'interface entre en scène.
 */
export function runIntro(trackMap: TrackMap, here: Promise<[number, number] | null> = Promise.resolve(null)) {
  const intro = document.getElementById("intro");
  const body = document.body;
  if (!intro) {
    body.classList.remove("booting");
    return;
  }
  // Position connue ? (undefined : pas encore de réponse — GPS lent ou autorisation en attente)
  let known: [number, number] | null | undefined;
  here.then((p) => (known = p));
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const wait = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

  Promise.all([wait(reduced ? 0 : MIN_INTRO_MS), Promise.race([trackMap.ready, wait(MAX_WAIT_MS)])]).then(async () => {
    intro.classList.add("ready"); // la ligne de chargement se complète
    await wait(reduced ? 0 : 420);
    trackMap.playIntro(known ?? null);
    // Réponse arrivée après le départ du vol : on rejoint la position si la carte n'a pas été touchée
    if (known === undefined) here.then((p) => p && trackMap.flyHere(p));
    intro.classList.add("out");
    body.classList.add("revealing");
    body.classList.remove("booting");
    await wait(reduced ? 0 : 2900);
    body.classList.remove("revealing");
    intro.remove();
  });
}

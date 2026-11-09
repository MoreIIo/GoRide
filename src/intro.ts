import type { TrackMap } from "./map";

/** Durée minimale de l'écran d'ouverture, le temps que le logo se dessine. */
const MIN_INTRO_MS = 2000;
/** Si la carte tarde (hors ligne, réseau lent), on ouvre quand même. */
const MAX_WAIT_MS = 6500;

/**
 * Ouverture : logo animé pendant le chargement du fond de carte, puis le rideau s'ouvre,
 * la caméra vole depuis le globe jusqu'à la région et l'interface entre en scène.
 */
export function runIntro(trackMap: TrackMap) {
  const intro = document.getElementById("intro");
  const body = document.body;
  if (!intro) {
    body.classList.remove("booting");
    return;
  }
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const wait = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

  Promise.all([wait(reduced ? 0 : MIN_INTRO_MS), Promise.race([trackMap.ready, wait(MAX_WAIT_MS)])]).then(async () => {
    intro.classList.add("ready"); // la ligne de chargement se complète
    await wait(reduced ? 0 : 420);
    trackMap.playIntro();
    intro.classList.add("out");
    body.classList.add("revealing");
    body.classList.remove("booting");
    await wait(reduced ? 0 : 2900);
    body.classList.remove("revealing");
    intro.remove();
  });
}

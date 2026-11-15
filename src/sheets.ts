/**
 * Tiroirs du téléphone : les panneaux montent du bas de l'écran.
 *
 * Un tiroir a trois tenues : replié (seul le haut dépasse : titre, distance…), à mi-hauteur
 * (là où le doigt l'a laissé) ou grand ouvert. On le déplace en glissant sa poignée — ou
 * n'importe où dessus tant qu'il est replié ; un simple toucher sur la poignée le plie ou
 * le déplie en entier. La poignée reste en haut du tiroir même quand son contenu défile.
 */

const SHEET_IDS = ["stats", "loop-panel", "library-panel", "rides-panel", "ride-detail"];

/** Déplacement du doigt (px) à partir duquel un toucher devient un glisser. */
const DRAG_START_PX = 6;
/** Vitesse (px/ms) au lâcher qui suffit à envoyer le tiroir en haut ou en bas. */
const FLING = 0.45;

type Listener = (id: string, open: boolean) => void;
let listeners: Listener[] = [];
/** Sonde invisible : hauteur réservée en haut de l'écran (barre, fonds de carte), en pixels. */
let topProbe: HTMLElement | null = null;

const el = (id: string) => document.getElementById(id);
const isOpen = (e: HTMLElement) => e.classList.contains("expanded");

/** Prévenu quand un tiroir s'ouvre (en entier ou à mi-hauteur) ou se replie. */
export function onSheetChange(fn: Listener) {
  listeners.push(fn);
}

/** Plie ou déplie un tiroir en entier. */
export function setSheet(id: string, open: boolean) {
  const e = el(id);
  if (!e) return;
  const was = isOpen(e);
  e.style.maxHeight = "";
  e.classList.toggle("expanded", open);
  if (!open) {
    e.scrollTop = 0; // replié, il montre toujours son en-tête
    e.classList.remove("scrolled");
  }
  if (was !== open) listeners.forEach((fn) => fn(id, open));
}

export function collapseSheets() {
  for (const id of SHEET_IDS) setSheet(id, false);
}

/** Hauteur d'un tiroir replié et hauteur maximale (jusque sous la barre du haut), en pixels. */
function limits(e: HTMLElement): { peek: number; full: number } {
  const peek = parseFloat(getComputedStyle(e).getPropertyValue("--peek")) || 92;
  const top = topProbe?.offsetHeight ?? 140;
  return { peek, full: Math.max(peek, e.getBoundingClientRect().bottom - top) };
}

/**
 * Ouvre un tiroir à mi-hauteur : son contenu reste lisible et la carte visible au-dessus
 * (boucles proposées, par exemple). Renvoie une promesse tenue quand il a fini de bouger.
 */
export function openSheetPartly(id: string, share = 0.46): Promise<void> {
  const e = el(id);
  if (!e || e.classList.contains("hidden")) return Promise.resolve();
  const { peek, full } = limits(e);
  const was = isOpen(e);
  const h = Math.round(Math.min(full, Math.max(peek + 80, window.innerHeight * share)));
  e.classList.add("expanded");
  e.style.maxHeight = `${h}px`;
  if (!was) listeners.forEach((fn) => fn(id, true));
  return new Promise((r) => setTimeout(r, 380)); // durée de l'animation CSS
}

function addGrip(e: HTMLElement) {
  if (e.querySelector(":scope > .sheet-grip")) return;
  const grip = document.createElement("div");
  grip.className = "sheet-grip";
  grip.setAttribute("role", "button");
  grip.setAttribute("aria-label", "Agrandir ou réduire le panneau");
  grip.innerHTML = "<i></i>";
  e.prepend(grip);
}

function attachDrag(e: HTMLElement) {
  const id = e.id;
  let pid = -1;
  let onGrip = false;
  let dragging = false;
  let startY = 0;
  let startH = 0;
  let lastY = 0;
  let lastT = 0;
  let speed = 0; // px/ms, positif vers le bas
  let quietUntil = 0;
  let lim = { peek: 92, full: 400 };

  e.addEventListener("pointerdown", (ev) => {
    if (pid !== -1 || (ev.pointerType === "mouse" && ev.button !== 0)) return;
    const target = ev.target as HTMLElement;
    onGrip = !!target.closest(".sheet-grip");
    // Déplié, le contenu défile : seule la poignée déplace le tiroir.
    if (!onGrip && (isOpen(e) || target.closest("input, select, textarea"))) return;
    pid = ev.pointerId;
    dragging = false;
    startY = lastY = ev.clientY;
    lastT = ev.timeStamp;
    speed = 0;
    startH = e.getBoundingClientRect().height;
    lim = limits(e);
    // Sur la poignée, le geste nous appartient d'emblée (même si le pointeur sort du tiroir)
    if (onGrip) {
      try {
        e.setPointerCapture(pid);
      } catch {
        /* pointeur déjà relâché */
      }
    }
  });

  e.addEventListener("pointermove", (ev) => {
    if (ev.pointerId !== pid) return;
    const dy = ev.clientY - startY;
    if (!dragging) {
      if (Math.abs(dy) < DRAG_START_PX) return;
      dragging = true;
      try {
        e.setPointerCapture(pid);
      } catch {
        /* pointeur déjà relâché */
      }
      e.classList.add("dragging", "expanded");
    }
    const dt = ev.timeStamp - lastT;
    if (dt > 0) speed = 0.6 * speed + 0.4 * ((ev.clientY - lastY) / dt);
    lastY = ev.clientY;
    lastT = ev.timeStamp;
    e.style.maxHeight = `${Math.min(lim.full, Math.max(lim.peek, startH - dy))}px`;
    ev.preventDefault();
  });

  const end = (ev: PointerEvent) => {
    if (ev.pointerId !== pid) return;
    pid = -1;
    if (!dragging) {
      // Simple toucher sur la poignée : plie ou déplie en entier
      if (onGrip && ev.type === "pointerup") setSheet(id, !isOpen(e));
      return;
    }
    dragging = false;
    e.classList.remove("dragging");
    quietUntil = performance.now() + 350;
    const h = e.getBoundingClientRect().height;
    if (speed > FLING || h <= lim.peek + 28) {
      setSheet(id, false);
    } else if (speed < -FLING || h >= lim.full - 28) {
      e.style.maxHeight = "";
      listeners.forEach((fn) => fn(id, true));
    } else {
      e.style.maxHeight = `${Math.round(h)}px`; // il reste là où on l'a laissé
      listeners.forEach((fn) => fn(id, true));
    }
  };
  e.addEventListener("pointerup", end);
  e.addEventListener("pointercancel", end);
  // Souris relâchée hors du tiroir avant d'avoir glissé : on oublie l'appui
  e.addEventListener("pointerleave", (ev) => {
    if (ev.pointerId === pid && !dragging && !onGrip) pid = -1;
  });

  // Le « clic » envoyé au lâcher d'un glisser ne doit pas activer le bouton qui se trouvait dessous.
  e.addEventListener(
    "click",
    (ev) => {
      if (performance.now() < quietUntil) {
        ev.stopPropagation();
        ev.preventDefault();
      }
    },
    true,
  );
}

/** Met en place poignées et gestes (téléphone seulement). */
export function initSheets() {
  topProbe = document.createElement("div");
  topProbe.className = "sheet-top-probe";
  document.body.append(topProbe);
  for (const id of SHEET_IDS) {
    const e = el(id);
    if (!e) continue;
    e.classList.add("sheet");
    addGrip(e);
    attachDrag(e);
    // Contenu qui a défilé : la poignée se détache en bandeau opaque (CSS « scrolled »)
    e.addEventListener("scroll", () => e.classList.toggle("scrolled", e.scrollTop > 2), { passive: true });
    // Un panneau dont le contenu est réécrit (fiche d'une sortie) retrouve sa poignée.
    new MutationObserver(() => addGrip(e)).observe(e, { childList: true });
  }
}

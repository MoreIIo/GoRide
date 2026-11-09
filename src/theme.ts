/** Thème de l'interface : sombre (par défaut) ou clair, gardé d'une session à l'autre. */
export type Theme = "dark" | "light";

const KEY = "goride.theme";
/** Événement envoyé sur `window` quand le thème change (carte, graphiques…). */
export const THEME_EVENT = "goride:theme";

export function getTheme(): Theme {
  return document.documentElement.dataset.theme === "light" ? "light" : "dark";
}

export function setTheme(theme: Theme) {
  if (theme === getTheme()) return;
  const root = document.documentElement;
  // Bascule douce : les couleurs glissent pendant un court instant, puis les transitions habituelles reprennent.
  root.classList.add("theme-switching");
  root.dataset.theme = theme;
  document.querySelector('meta[name="color-scheme"]')?.setAttribute("content", theme);
  try {
    localStorage.setItem(KEY, theme);
  } catch {
    /* stockage indisponible : le choix vaut pour cette session */
  }
  window.dispatchEvent(new CustomEvent<Theme>(THEME_EVENT, { detail: theme }));
  window.setTimeout(() => root.classList.remove("theme-switching"), 450);
}

/** Vue « Progression » : forme / fatigue / fraîcheur, semaines, vitesse sur le plat (graphiques SVG). */

export interface RidePoint {
  id: number;
  name: string;
  startTime: number;
  distanceKm: number;
  movingH: number;
  ascentM: number;
  flatSpeedKmh: number | null;
  tss: number;
  tssMethod: "puissance" | "cardio" | "estimation";
}
export interface DayLoad {
  day: number;
  tss: number;
  fitness: number;
  fatigue: number;
  form: number;
}
export interface Week {
  start: number;
  distanceKm: number;
  movingH: number;
  ascentM: number;
  rides: number;
  tss: number;
}
export interface Progress {
  rides: RidePoint[];
  days: DayLoad[];
  weeks: Week[];
  flatSpeedTrendPerMonth: number | null;
  estimatedShare: number;
}

const n0 = new Intl.NumberFormat("fr-FR", { maximumFractionDigits: 0 });
const n1 = new Intl.NumberFormat("fr-FR", { minimumFractionDigits: 1, maximumFractionDigits: 1 });
const monthFmt = new Intl.DateTimeFormat("fr-FR", { month: "short" });
const dayFmt = new Intl.DateTimeFormat("fr-FR", { day: "numeric", month: "short" });
const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

const W = 640;
const PAD = { l: 40, r: 12, t: 12, b: 24 };

interface Line {
  values: number[];
  color: string;
  label: string;
  width?: number;
}

/** Axe du temps : un repère par mois. */
function monthTicks(t0: number, t1: number, x: (t: number) => number, h: number): string {
  let out = "";
  const d = new Date(t0 * 1000);
  d.setDate(1);
  d.setHours(0, 0, 0, 0);
  d.setMonth(d.getMonth() + 1);
  while (d.getTime() / 1000 <= t1) {
    const xx = x(d.getTime() / 1000);
    out += `<line x1="${xx}" x2="${xx}" y1="${PAD.t}" y2="${h - PAD.b}" class="grid" />`;
    out += `<text x="${xx + 3}" y="${h - 8}" class="axis">${monthFmt.format(d)}</text>`;
    d.setMonth(d.getMonth() + 1);
  }
  return out;
}
function yTicks(lo: number, hi: number, y: (v: number) => number, unit = ""): string {
  const span = hi - lo || 1;
  const raw = span / 4;
  const pow = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((k) => k * pow).find((s) => s >= raw) ?? raw;
  let out = "";
  for (let v = Math.ceil(lo / step) * step; v <= hi + 1e-9; v += step) {
    const yy = y(v);
    out += `<line x1="${PAD.l}" x2="${W - PAD.r}" y1="${yy}" y2="${yy}" class="grid${v === 0 ? " zero" : ""}" />`;
    out += `<text x="${PAD.l - 6}" y="${yy + 4}" class="axis" text-anchor="end">${n0.format(v)}${unit}</text>`;
  }
  return out;
}

/** Courbes sur le temps (une valeur par jour). */
function timeChart(days: number[], lines: Line[], h: number, bars?: Line): string {
  if (days.length < 2) return "";
  const t0 = days[0];
  const t1 = days[days.length - 1];
  const x = (t: number) => PAD.l + ((t - t0) / (t1 - t0 || 1)) * (W - PAD.l - PAD.r);
  const all = [...lines.flatMap((l) => l.values), ...(bars?.values ?? []), 0];
  const lo = Math.min(...all);
  const hi = Math.max(...all) * 1.08 || 1;
  const y = (v: number) => PAD.t + (1 - (v - lo) / (hi - lo || 1)) * (h - PAD.t - PAD.b);
  let svg = yTicks(lo, hi, y) + monthTicks(t0, t1, x, h);
  if (bars) {
    const bw = Math.max(1, (W - PAD.l - PAD.r) / days.length - 0.5);
    bars.values.forEach((v, i) => {
      const y0 = y(0);
      const yy = y(v);
      svg += `<rect x="${x(days[i]) - bw / 2}" y="${Math.min(y0, yy)}" width="${bw}" height="${Math.abs(yy - y0)}" fill="${
        v >= 0 ? "#2bb673" : "#e07a00"
      }" opacity="0.45" />`;
    });
  }
  for (const l of lines) {
    const d = l.values.map((v, i) => `${i ? "L" : "M"}${x(days[i]).toFixed(1)},${y(v).toFixed(1)}`).join("");
    svg += `<path d="${d}" fill="none" stroke="${l.color}" stroke-width="${l.width ?? 2}" stroke-linejoin="round" />`;
  }
  return `<svg viewBox="0 0 ${W} ${h}" class="pchart" role="img">${svg}</svg>`;
}

/** Barres par semaine. */
function weekChart(weeks: Week[], h: number): string {
  if (weeks.length === 0) return "";
  const hi = Math.max(10, ...weeks.map((w) => w.distanceKm)) * 1.1;
  const y = (v: number) => PAD.t + (1 - v / hi) * (h - PAD.t - PAD.b);
  const bw = (W - PAD.l - PAD.r) / weeks.length;
  let svg = yTicks(0, hi, y, "");
  weeks.forEach((w, i) => {
    const xx = PAD.l + i * bw;
    if (w.distanceKm > 0) {
      svg += `<rect x="${xx + 1}" y="${y(w.distanceKm)}" width="${Math.max(1, bw - 2)}" height="${y(0) - y(w.distanceKm)}" rx="2" fill="#fc4c02">
        <title>Semaine du ${dayFmt.format(new Date(w.start * 1000))} : ${n0.format(w.distanceKm)} km, ${w.rides} sortie${w.rides > 1 ? "s" : ""}, ${n0.format(w.ascentM)} m D+</title></rect>`;
    }
    if (i % Math.ceil(weeks.length / 8) === 0) {
      svg += `<text x="${xx + 2}" y="${h - 8}" class="axis">${dayFmt.format(new Date(w.start * 1000))}</text>`;
    }
  });
  return `<svg viewBox="0 0 ${W} ${h}" class="pchart" role="img">${svg}</svg>`;
}

/** Points (vitesse sur le plat de chaque sortie) et droite de tendance. */
function speedChart(rides: RidePoint[], h: number): string {
  const pts = rides.filter((r) => r.flatSpeedKmh !== null && r.movingH >= 0.5);
  if (pts.length < 2) return "";
  const t0 = pts[0].startTime - 86400 * 3;
  const t1 = pts[pts.length - 1].startTime + 86400 * 3;
  const x = (t: number) => PAD.l + ((t - t0) / (t1 - t0)) * (W - PAD.l - PAD.r);
  const vs = pts.map((p) => p.flatSpeedKmh!);
  const lo = Math.floor(Math.min(...vs) - 1);
  const hi = Math.ceil(Math.max(...vs) + 1);
  const y = (v: number) => PAD.t + (1 - (v - lo) / (hi - lo)) * (h - PAD.t - PAD.b);
  let svg = yTicks(lo, hi, y) + monthTicks(t0, t1, x, h);
  // Tendance (moindres carrés)
  const n = pts.length;
  const mx = pts.reduce((a, p) => a + p.startTime, 0) / n;
  const my = vs.reduce((a, v) => a + v, 0) / n;
  const sxx = pts.reduce((a, p) => a + (p.startTime - mx) ** 2, 0);
  if (n >= 3 && sxx > 0) {
    const slope = pts.reduce((a, p, i) => a + (p.startTime - mx) * (vs[i] - my), 0) / sxx;
    const f = (t: number) => my + slope * (t - mx);
    svg += `<line x1="${x(t0)}" y1="${y(f(t0))}" x2="${x(t1)}" y2="${y(f(t1))}" stroke="#3e8bff" stroke-width="2" stroke-dasharray="6 4" />`;
  }
  pts.forEach((p) => {
    svg += `<circle cx="${x(p.startTime)}" cy="${y(p.flatSpeedKmh!)}" r="5" fill="#fc4c02" stroke="#fff" stroke-width="1.5" data-ride="${p.id}">
      <title>${esc(p.name)} (${dayFmt.format(new Date(p.startTime * 1000))}) : ${n1.format(p.flatSpeedKmh!)} km/h sur le plat</title></circle>`;
  });
  return `<svg viewBox="0 0 ${W} ${h}" class="pchart" role="img">${svg}</svg>`;
}

/** Lecture de la fraîcheur du jour. */
function formLabel(form: number): [string, string] {
  if (form > 15) return ["Très frais", "Reposé, mais la forme commence à baisser : c'est le moment d'une grosse sortie ou d'un objectif."];
  if (form > 5) return ["Frais", "Bien reposé : idéal pour une sortie rapide ou un objectif."];
  if (form > -10) return ["Équilibré", "Charge et récupération s'équilibrent : continue comme ça."];
  if (form > -30) return ["En construction", "Tu accumules de l'entraînement : c'est ce qui fait progresser. Garde une journée facile par semaine."];
  return ["Fatigue élevée", "La charge des derniers jours est forte : prévois 2 ou 3 jours faciles pour récupérer."];
}

export type Period = 90 | 180 | 365 | 0;

export function progressHtml(p: Progress, period: Period): string {
  if (p.rides.length === 0) {
    return `<p class="hint">Importe quelques sorties pour voir ta progression.</p>`;
  }
  const now = Date.now() / 1000;
  const from = period ? now - period * 86400 : -Infinity;
  const days = p.days.filter((d) => d.day >= from);
  const weeks = p.weeks.filter((w) => w.start + 7 * 86400 >= from);
  const rides = p.rides.filter((r) => r.startTime >= from);
  const today = p.days[p.days.length - 1];
  const [label, text] = formLabel(today.form);

  const thisWeek = p.weeks[p.weeks.length - 1];
  const prev4 = p.weeks.slice(-5, -1);
  const avg4 = prev4.length ? prev4.reduce((a, w) => a + w.distanceKm, 0) / prev4.length : 0;

  const trend = p.flatSpeedTrendPerMonth;
  const speedRides = p.rides.filter((r) => r.flatSpeedKmh !== null && r.movingH >= 0.5);
  let trendText = "";
  if (trend !== null && speedRides.length >= 3) {
    const sign = trend >= 0 ? "+" : "−";
    trendText = `Tendance : <b>${sign}${n1.format(Math.abs(trend))} km/h par mois</b> sur ${speedRides.length} sorties.`;
    if (speedRides.length < 10)
      trendText += ` <span class="hint">Peu de sorties : la tendance est encore indicative (le vent, le parcours et la météo jouent beaucoup).</span>`;
  }

  return `
    <div class="kpis">
      <div class="kpi"><span>Forme</span><b>${n0.format(today.fitness)}</b><small>charge des 6 dernières semaines</small></div>
      <div class="kpi"><span>Fatigue</span><b>${n0.format(today.fatigue)}</b><small>charge des 7 derniers jours</small></div>
      <div class="kpi"><span>Fraîcheur</span><b>${n0.format(today.form)}</b><small>${label}</small></div>
      <div class="kpi"><span>Cette semaine</span><b>${n0.format(thisWeek?.distanceKm ?? 0)} km</b><small>moyenne 4 sem. : ${n0.format(avg4)} km</small></div>
    </div>
    <p class="form-text">${text}</p>
    ${p.estimatedShare > 0.5 ? `<p class="hint small">La plupart de tes sorties n'ont ni capteur de puissance ni cardio : leur charge (TSS) est estimée d'après la durée et le dénivelé. Elle sera plus juste avec un capteur.</p>` : ""}

    <section class="block"><h3>Forme, fatigue, fraîcheur</h3>
      <div class="legend">
        <span><i style="background:#3e8bff"></i>Forme</span>
        <span><i style="background:#d93a6a"></i>Fatigue</span>
        <span><i style="background:#2bb673;opacity:.6"></i>Fraîcheur</span>
      </div>
      ${timeChart(
        days.map((d) => d.day),
        [
          { values: days.map((d) => d.fitness), color: "#3e8bff", label: "Forme", width: 2.5 },
          { values: days.map((d) => d.fatigue), color: "#d93a6a", label: "Fatigue", width: 1.5 },
        ],
        210,
        { values: days.map((d) => d.form), color: "#2bb673", label: "Fraîcheur" },
      )}
    </section>

    <section class="block"><h3>Vitesse sur le plat</h3>
      <p class="hint small">Vitesse moyenne quand tu pédales sur le plat : le meilleur repère pour voir si tu vas plus vite (sorties de plus de 30 min).</p>
      ${speedChart(rides, 190) || `<p class="hint">Pas assez de sorties sur cette période.</p>`}
      ${trendText ? `<p>${trendText}</p>` : ""}
    </section>

    <section class="block"><h3>Kilomètres par semaine</h3>
      ${weekChart(weeks, 170)}
    </section>`;
}

/** Onglet « Sorties » : types renvoyés par le moteur, graphique et panneau de détail. */
import { duration, km, meters } from "./format";
import { THEME_EVENT } from "./theme";

export interface ActivitySummary {
  id: number;
  name: string;
  sport: string | null;
  startTime: number | null;
  format: string;
  distanceM: number;
  movingS: number;
  ascentM: number;
  avgPower: number | null;
  avgHr: number | null;
}

export interface Athlete {
  ftp?: number | null;
  hrMax?: number | null;
  weight?: number | null;
}

interface PowerStats {
  avg: number;
  max: number;
  np: number;
  vi: number;
  workKj: number;
  intensity: number | null;
  tss: number | null;
  wPerKg: number | null;
}

interface Climb {
  startKm: number;
  endKm: number;
  lengthM: number;
  gainM: number;
  grade: number;
  durationS: number;
  vam: number;
  category: string;
  avgPower: number | null;
  avgHr: number | null;
  avgCadence: number | null;
}

interface Advice {
  kind: "good" | "tip" | "warning" | "info";
  title: string;
  text: string;
}

export interface Series {
  distKm: number[];
  timeS: number[];
  ele: (number | null)[];
  speedKmh: (number | null)[];
  hr: (number | null)[];
  power: (number | null)[];
  cadence: (number | null)[];
}

export interface Analysis {
  distanceM: number;
  elapsedS: number;
  movingS: number;
  avgSpeedKmh: number;
  maxSpeedKmh: number;
  ascentM: number;
  descentM: number;
  ascentComputedM: number;
  ascentDeviceM: number | null;
  eleMin: number | null;
  eleMax: number | null;
  avgHr: number | null;
  maxHr: number | null;
  avgCadence: number | null;
  power: PowerStats | null;
  avgTemp: number | null;
  hrZones: number[] | null;
  powerZones: number[] | null;
  bestPower: [number, number | null][];
  climbs: Climb[];
  decoupling: number | null;
  ftpUsed: number | null;
  ftpSource: "profil" | "compteur" | null;
  estimatedFtp: number | null;
  flatSpeedKmh: number | null;
  flatCoastingPct: number | null;
  advice: Advice[];
  series: Series;
}

export interface ClimbEffort {
  activityId: number;
  name: string;
  startTime: number | null;
  durationS: number;
  vam: number;
  avgPower: number | null;
  avgHr: number | null;
  current: boolean;
}

export interface ClimbHistory {
  efforts: ClimbEffort[];
  rank: number;
}

export interface ActivityDetail {
  summary: ActivitySummary;
  track: { coords: [number, number][]; ele: (number | null)[]; dist: number[] };
  analysis: Analysis;
  climbHistory: ClimbHistory[];
}

const shortDate = new Intl.DateTimeFormat("fr-FR", { day: "numeric", month: "short", year: "2-digit" });

/** Ligne « record » ou « 2ᵉ sur 5 passages » et tableau de tous les passages. */
function climbHistoryHtml(h: ClimbHistory | undefined): string {
  if (!h || h.efforts.length < 2) return "";
  const best = h.efforts[0];
  const badge =
    h.rank === 1
      ? `<span class="climb-rank record">Record sur ${h.efforts.length} passages</span>`
      : `<span class="climb-rank">${h.rank}ᵉ sur ${h.efforts.length} · record ${hms(best.durationS)}${
          best.startTime ? " le " + shortDate.format(new Date(best.startTime * 1000)) : ""
        }</span>`;
  const rows = h.efforts
    .map(
      (e, i) => `<tr class="${e.current ? "current" : ""}" data-open-ride="${e.activityId}">
        <td>${i + 1}</td><td>${e.startTime ? shortDate.format(new Date(e.startTime * 1000)) : ""}</td>
        <td>${hms(e.durationS)}</td><td>${n0.format(e.vam)} m/h</td>
        <td>${e.avgPower !== null ? n0.format(e.avgPower) + " W" : ""}</td></tr>`,
    )
    .join("");
  return `<br />${badge}<table class="climb-efforts hidden">${rows}</table>`;
}

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
const n0 = new Intl.NumberFormat("fr-FR", { maximumFractionDigits: 0 });
const n1 = new Intl.NumberFormat("fr-FR", { minimumFractionDigits: 1, maximumFractionDigits: 1 });
const n2 = new Intl.NumberFormat("fr-FR", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
const dateFmt = new Intl.DateTimeFormat("fr-FR", { weekday: "short", day: "numeric", month: "short", year: "numeric" });
const dateLong = new Intl.DateTimeFormat("fr-FR", {
  weekday: "long",
  day: "numeric",
  month: "long",
  year: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

export const rideDate = (s: ActivitySummary, long = false) =>
  s.startTime ? (long ? dateLong : dateFmt).format(new Date(s.startTime * 1000)) : "Date inconnue";

const hms = (s: number) => duration(s / 3600);

/** Élément de la liste des sorties. */
export function rideListItem(s: ActivitySummary, selected: boolean): string {
  const extra = [s.avgPower ? `${n0.format(s.avgPower)} W` : "", s.avgHr ? `${n0.format(s.avgHr)} bpm` : ""].filter(Boolean);
  return `<li class="ride-item${selected ? " selected" : ""}" data-id="${s.id}">
    <span class="ride-date">${esc(rideDate(s))}</span>
    <span class="ride-name">${esc(s.name)}</span>
    <span class="ride-meta">${km(s.distanceM)} · ${meters(s.ascentM)} D+ · ${hms(s.movingS)}${extra.length ? " · " + extra.join(" · ") : ""}</span>
  </li>`;
}

const HR_ZONE_NAMES = ["Z1 Récupération", "Z2 Endurance", "Z3 Tempo", "Z4 Seuil", "Z5 Maximal"];
const POWER_ZONE_NAMES = ["Z1 Récup.", "Z2 Endurance", "Z3 Tempo", "Z4 Seuil", "Z5 VO2max", "Z6 Anaérobie", "Z7 Neuromusc."];
const ZONE_COLORS = ["#9aa0a9", "#3e8bff", "#2bb673", "#e0a100", "#fc4c02", "#d93a3a", "#8a1c1c"];

function zoneBars(title: string, names: string[], secs: number[]): string {
  const total = secs.reduce((a, b) => a + b, 0) || 1;
  const rows = secs
    .map((s, i) => {
      const pct = (s / total) * 100;
      return `<div class="zone"><span class="zname">${names[i]}</span>
        <span class="zbar"><i style="width:${pct.toFixed(1)}%;background:${ZONE_COLORS[i]}"></i></span>
        <span class="zval">${n0.format(pct)} %<small>${hms(s)}</small></span></div>`;
    })
    .join("");
  return `<section class="block"><h3>${title}</h3>${rows}</section>`;
}

const DURATION_LABEL: Record<number, string> = {
  5: "5 s",
  10: "10 s",
  30: "30 s",
  60: "1 min",
  120: "2 min",
  300: "5 min",
  600: "10 min",
  1200: "20 min",
  1800: "30 min",
  3600: "1 h",
};

const ADVICE_ICON = { good: "✓", tip: "➜", warning: "!", info: "i" };

/** Panneau de détail d'une sortie. */
export function rideDetailHtml(d: ActivityDetail, athlete: Athlete): string {
  const a = d.analysis;
  const s = d.summary;
  const stat = (label: string, value: string, sub = "") =>
    `<div><dt>${label}</dt><dd>${value}${sub ? `<small>${sub}</small>` : ""}</dd></div>`;

  // D+ : celui du compteur et celui recalculé par GoRide
  let ascentSub = "";
  if (a.ascentDeviceM !== null) {
    const diff = ((a.ascentComputedM - a.ascentDeviceM) / Math.max(1, a.ascentDeviceM)) * 100;
    ascentSub = `compteur · GoRide ${n0.format(a.ascentComputedM)} m (${diff >= 0 ? "+" : ""}${n0.format(diff)} %)`;
  }

  const stats = [
    stat("Distance", km(a.distanceM)),
    stat("Temps en mouvement", hms(a.movingS), a.elapsedS - a.movingS > 120 ? `${hms(a.elapsedS)} au total` : ""),
    stat("Vitesse moyenne", `${n1.format(a.avgSpeedKmh)} km/h`, `max ${n1.format(a.maxSpeedKmh)} km/h`),
    a.flatSpeedKmh !== null
      ? stat(
          "Vitesse sur le plat",
          `${n1.format(a.flatSpeedKmh)} km/h`,
          a.flatCoastingPct !== null ? `roue libre ${n0.format(a.flatCoastingPct)} %` : "en pédalant",
        )
      : "",
    stat("D+", meters(a.ascentM), ascentSub),
    a.avgHr !== null ? stat("FC moyenne", `${n0.format(a.avgHr)} bpm`, a.maxHr ? `max ${n0.format(a.maxHr)} bpm` : "") : "",
    a.avgCadence !== null ? stat("Cadence moyenne", `${n0.format(a.avgCadence)} tr/min`) : "",
    a.avgTemp !== null ? stat("Température", `${n0.format(a.avgTemp)} °C`) : "",
  ].join("");

  let powerBlock = "";
  if (a.power) {
    const p = a.power;
    powerBlock = `<section class="block"><h3>Puissance</h3><dl class="grid">
      ${stat("Moyenne", `${n0.format(p.avg)} W`, p.wPerKg ? `${n2.format(p.wPerKg)} W/kg` : "")}
      ${stat("Normalisée (NP)", `${n0.format(p.np)} W`, `VI ${n2.format(p.vi)}`)}
      ${stat(
        "Intensité (IF)",
        p.intensity !== null ? n2.format(p.intensity) : "—",
        a.ftpUsed !== null
          ? `FTP ${n0.format(a.ftpUsed)} W${a.ftpSource === "compteur" ? " (réglée sur ton Garmin)" : ""}`
          : "renseigne ta FTP",
      )}
      ${stat("TSS", p.tss !== null ? n0.format(p.tss) : "—")}
      ${stat("Travail", `${n0.format(p.workKj)} kJ`)}
      ${stat("Max", `${n0.format(p.max)} W`)}
    </dl></section>`;
  }

  const best = a.bestPower.filter(([, w]) => w !== null);
  const ftp = a.ftpUsed ?? a.estimatedFtp;
  const bestBlock = best.length
    ? `<section class="block"><h3>Meilleurs efforts</h3><table class="best">
        ${best
          .map(
            ([d, w]) =>
              `<tr><td>${DURATION_LABEL[d] ?? d + " s"}</td><td><b>${n0.format(w!)} W</b></td><td>${
                athlete.weight ? n2.format(w! / athlete.weight) + " W/kg" : ""
              }</td><td>${ftp ? n0.format((w! / ftp) * 100) + " % FTP" : ""}</td></tr>`,
          )
          .join("")}
      </table></section>`
    : "";

  const climbs = a.climbs.length
    ? `<section class="block"><h3>Montées (${a.climbs.length})</h3><ul class="climbs">
        ${a.climbs
          .map(
            (c, i) => `<li data-climb="${i}" title="Cliquer pour voir la montée">
              <span class="cat${c.category ? "" : " none"}">${c.category ? (c.category === "HC" ? "HC" : "Cat " + c.category) : "—"}</span>
              <span><b>km ${n1.format(c.startKm)} → ${n1.format(c.endKm)}</b> · ${n1.format(c.lengthM / 1000)} km à ${n1.format(c.grade)} %<br />
              <small>${n0.format(c.gainM)} m en ${hms(c.durationS)} · VAM ${n0.format(c.vam)} m/h${
                c.avgPower !== null ? ` · ${n0.format(c.avgPower)} W` : ""
              }${c.avgHr !== null ? ` · ${n0.format(c.avgHr)} bpm` : ""}${c.avgCadence !== null ? ` · ${n0.format(c.avgCadence)} tr/min` : ""}</small>${climbHistoryHtml(
                d.climbHistory?.[i],
              )}</span>
            </li>`,
          )
          .join("")}
      </ul></section>`
    : "";

  const advice = a.advice.length
    ? `<section class="block"><h3>Conseils</h3>${a.advice
        .map(
          (x) => `<div class="advice advice-${x.kind}"><span class="aicon">${ADVICE_ICON[x.kind]}</span>
            <div><b>${esc(x.title)}</b><p>${esc(x.text)}</p></div></div>`,
        )
        .join("")}</section>`
    : "";

  return `<header class="ride-head">
      <div><h2 id="ride-title">${esc(s.name)}</h2><p class="hint">${esc(rideDate(s, true))}</p></div>
      <div class="row">
        <button class="icon-btn" id="ride-rename" title="Renommer"><svg viewBox="0 0 24 24"><path d="M4 20l4-1 11-11-3-3L5 16l-1 4z" /></svg></button>
        <button class="icon-btn" id="ride-delete" title="Supprimer cette sortie"><svg viewBox="0 0 24 24"><path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3" /></svg></button>
      </div>
    </header>
    <dl class="grid">${stats}</dl>
    <section class="block ride-coach" id="ride-coach" data-ride-coach="${s.id}"></section>
    ${advice}
    ${powerBlock}
    ${a.hrZones ? zoneBars(`Zones cardiaques${athlete.hrMax ? "" : " (FC max de la sortie)"}`, HR_ZONE_NAMES, a.hrZones) : ""}
    ${a.powerZones ? zoneBars("Zones de puissance", POWER_ZONE_NAMES, a.powerZones) : ""}
    ${bestBlock}
    ${climbs}`;
}

// --- Graphique ------------------------------------------------------------------------

export type Metric = "speedKmh" | "hr" | "power" | "cadence";

export const METRICS: Record<Metric, { label: string; unit: string; color: string }> = {
  speedKmh: { label: "Vitesse", unit: "km/h", color: "#fc4c02" },
  hr: { label: "Cardio", unit: "bpm", color: "#e0443a" },
  power: { label: "Puissance", unit: "W", color: "#8a5cf6" },
  cadence: { label: "Cadence", unit: "tr/min", color: "#14a3a3" },
};

const PAD = { left: 48, right: 46, top: 12, bottom: 24 };

/** Courbe d'une donnée (vitesse, cardio…) sur le relief, en fonction de la distance. */
export class RideChart {
  private ctx: CanvasRenderingContext2D;
  private s: Series | null = null;
  private metric: Metric = "speedKmh";
  private cursorKm: number | null = null;
  private frame = 0;

  constructor(
    private canvas: HTMLCanvasElement,
    private tip: HTMLElement,
    private onHover: (distKm: number | null) => void,
  ) {
    this.ctx = canvas.getContext("2d")!;
    new ResizeObserver(() => this.redraw()).observe(canvas);
    window.addEventListener(THEME_EVENT, () => this.redraw());
    canvas.addEventListener("pointermove", (e) => {
      if (!this.s || this.s.distKm.length === 0) return;
      const total = this.s.distKm[this.s.distKm.length - 1];
      const ratio = (e.offsetX - PAD.left) / (canvas.clientWidth - PAD.left - PAD.right);
      const d = Math.min(1, Math.max(0, ratio)) * total;
      this.onHover(d);
      this.setCursor(d);
    });
    canvas.addEventListener("pointerleave", () => {
      this.onHover(null);
      this.setCursor(null);
    });
  }

  /** Données disponibles dans la sortie (pour n'afficher que les onglets utiles). */
  static available(s: Series): Metric[] {
    return (Object.keys(METRICS) as Metric[]).filter((m) => s[m].some((v) => v !== null && Number.isFinite(v) && v > 0));
  }

  setData(s: Series | null, metric: Metric) {
    this.s = s;
    this.metric = metric;
    this.cursorKm = null;
    this.redraw();
  }

  setCursor(km: number | null) {
    this.cursorKm = km;
    this.redraw();
  }

  private redraw() {
    cancelAnimationFrame(this.frame);
    this.frame = requestAnimationFrame(() => this.draw());
  }

  private draw() {
    const { canvas, ctx } = this;
    const dpr = window.devicePixelRatio || 1;
    const w = canvas.clientWidth;
    const h = canvas.clientHeight;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const s = this.s;
    if (!s || s.distKm.length < 2 || w === 0) return;

    const css = getComputedStyle(document.documentElement);
    const ink = css.getPropertyValue("--text-2").trim();
    const grid = css.getPropertyValue("--grid").trim();
    ctx.font = "500 11px 'Inter Tight Variable', system-ui, sans-serif";
    const total = s.distKm[s.distKm.length - 1];
    const x = (d: number) => PAD.left + (d / total) * (w - PAD.left - PAD.right);
    const plotH = h - PAD.top - PAD.bottom;

    // Relief en fond (échelle à droite)
    const ele = s.ele.map((v) => (v === null || !Number.isFinite(v) ? NaN : v));
    const eOk = ele.filter((v) => !Number.isNaN(v));
    if (eOk.length > 1) {
      const lo = Math.min(...eOk);
      const hi = Math.max(lo + 30, Math.max(...eOk));
      const ye = (e: number) => PAD.top + (1 - (e - lo) / (hi - lo)) * plotH * 0.95 + plotH * 0.05;
      ctx.beginPath();
      ctx.moveTo(x(s.distKm[0]), h - PAD.bottom);
      s.distKm.forEach((d, i) => ctx.lineTo(x(d), Number.isNaN(ele[i]) ? h - PAD.bottom : ye(ele[i])));
      ctx.lineTo(x(total), h - PAD.bottom);
      ctx.closePath();
      ctx.fillStyle = "rgba(140, 140, 150, 0.22)";
      ctx.fill();
      ctx.fillStyle = ink;
      ctx.textAlign = "left";
      ctx.textBaseline = "middle";
      ctx.fillText(`${Math.round(hi)} m`, w - PAD.right + 6, ye(hi));
      ctx.fillText(`${Math.round(lo)} m`, w - PAD.right + 6, ye(lo));
    }

    // Courbe de la donnée choisie (lissée sur 5 points)
    const m = METRICS[this.metric];
    const raw = s[this.metric].map((v) => (v === null || !Number.isFinite(v) ? NaN : v));
    const vals = raw.map((_, i) => {
      let sum = 0;
      let n = 0;
      for (let k = Math.max(0, i - 2); k <= Math.min(raw.length - 1, i + 2); k++) {
        if (!Number.isNaN(raw[k])) {
          sum += raw[k];
          n++;
        }
      }
      return n ? sum / n : NaN;
    });
    const ok = vals.filter((v) => !Number.isNaN(v));
    if (ok.length > 1) {
      const vmax = Math.max(...ok) * 1.08;
      const vmin = this.metric === "hr" ? Math.min(...ok) * 0.92 : 0;
      const y = (v: number) => PAD.top + (1 - (v - vmin) / (vmax - vmin || 1)) * plotH;
      ctx.strokeStyle = grid;
      ctx.fillStyle = ink;
      ctx.textAlign = "right";
      ctx.textBaseline = "middle";
      for (let k = 0; k <= 4; k++) {
        const v = vmin + ((vmax - vmin) * k) / 4;
        const yy = Math.round(y(v)) + 0.5;
        ctx.beginPath();
        ctx.moveTo(PAD.left, yy);
        ctx.lineTo(w - PAD.right, yy);
        ctx.stroke();
        ctx.fillText(String(Math.round(v)), PAD.left - 6, yy);
      }
      ctx.strokeStyle = m.color;
      ctx.lineWidth = 1.6;
      ctx.lineJoin = "round";
      ctx.beginPath();
      let pen = false;
      s.distKm.forEach((d, i) => {
        if (Number.isNaN(vals[i])) {
          pen = false;
          return;
        }
        if (pen) ctx.lineTo(x(d), y(vals[i]));
        else ctx.moveTo(x(d), y(vals[i]));
        pen = true;
      });
      ctx.stroke();
    }

    // Distances
    ctx.fillStyle = ink;
    ctx.textAlign = "center";
    ctx.textBaseline = "top";
    const step = [1, 2, 5, 10, 20, 25, 50, 100].find((st) => total / st <= Math.max(2, (w - 100) / 80)) ?? 200;
    for (let d = 0; d <= total + 0.01; d += step) ctx.fillText(`${d} km`, x(d), h - PAD.bottom + 7);

    // Curseur
    if (this.cursorKm !== null) {
      const i = Math.min(s.distKm.length - 1, s.distKm.findIndex((d) => d >= this.cursorKm!) + 0);
      const k = i < 0 ? s.distKm.length - 1 : i;
      const cx = Math.round(x(s.distKm[k])) + 0.5;
      ctx.strokeStyle = ink;
      ctx.setLineDash([3, 3]);
      ctx.beginPath();
      ctx.moveTo(cx, PAD.top);
      ctx.lineTo(cx, h - PAD.bottom);
      ctx.stroke();
      ctx.setLineDash([]);
      const parts = [`<b>${n1.format(s.distKm[k])} km</b>`, hms(s.timeS[k])];
      for (const key of Object.keys(METRICS) as Metric[]) {
        const v = s[key][k];
        if (v !== null && Number.isFinite(v) && v > 0)
          parts.push(`${key === "speedKmh" ? n1.format(v) : n0.format(v)} ${METRICS[key].unit}`);
      }
      if (s.ele[k] !== null && Number.isFinite(s.ele[k])) parts.push(`${n0.format(s.ele[k]!)} m`);
      this.tip.innerHTML = parts.join(" · ");
      this.tip.classList.remove("hidden");
      const tw = this.tip.offsetWidth;
      this.tip.style.left = `${Math.min(Math.max(cx - tw / 2, 4), w - tw - 4)}px`;
    } else {
      this.tip.classList.add("hidden");
    }
  }
}

import { indexAtDistance, slopeAt, type Track } from "./types";
import { km, meters, percent } from "./format";

const PAD = { left: 52, right: 18, top: 24, bottom: 26 };
/** Pente au-delà de laquelle la zone est surlignée (%). */
const STEEP = 6;

/** "#rrggbb" → "rgba(r, g, b, a)". */
function withAlpha(hex: string, a: number): string {
  const n = parseInt(hex.replace("#", ""), 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${a})`;
}

/** « Jolis » pas de graduation : 1, 2, 5 × 10ⁿ. */
function niceStep(range: number, targetTicks: number): number {
  const raw = range / Math.max(1, targetTicks);
  const pow = 10 ** Math.floor(Math.log10(raw));
  const n = raw / pow;
  return (n < 1.5 ? 1 : n < 3.5 ? 2 : n < 7.5 ? 5 : 10) * pow;
}

/**
 * Profil altimétrique dessiné sur un <canvas> (F2.5).
 * Survol du profil → callback avec l'index du point, pour le curseur sur la carte.
 */
export class ElevationProfile {
  private ctx: CanvasRenderingContext2D;
  private track: Track | null = null;
  private cursor: number | null = null;
  /** Distances (m) des points de passage, affichées en repères numérotés. */
  private marks: number[] = [];
  private yMin = 0;
  private yMax = 1;
  private frame = 0;

  constructor(
    private canvas: HTMLCanvasElement,
    private tip: HTMLElement,
    private onHover: (index: number | null) => void,
  ) {
    this.ctx = canvas.getContext("2d")!;
    new ResizeObserver(() => this.redraw()).observe(canvas);
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => this.redraw());

    canvas.addEventListener("pointermove", (e) => {
      if (!this.track) return;
      const x = e.offsetX;
      const i = this.indexAtX(x);
      this.onHover(i);
      this.setCursor(i);
    });
    canvas.addEventListener("pointerleave", () => {
      this.onHover(null);
      this.setCursor(null);
    });
  }

  setTrack(track: Track | null) {
    this.track = track;
    this.cursor = null;
    if (track?.stats.hasElevation) {
      const lo = track.stats.minEleM!;
      const hi = track.stats.maxEleM!;
      const span = Math.max(hi - lo, 40); // évite d'exagérer un profil quasi plat
      const step = niceStep(span, 4);
      this.yMin = Math.floor((lo - span * 0.08) / step) * step;
      this.yMax = Math.ceil((lo + span * 1.08) / step) * step;
    }
    this.redraw();
  }

  /** Repères des points de passage (distances en mètres depuis le départ). */
  setMarks(distances: number[]) {
    this.marks = distances;
    this.redraw();
  }

  setCursor(index: number | null) {
    this.cursor = index;
    this.redraw();
  }

  private get width() {
    return this.canvas.clientWidth;
  }
  private get height() {
    return this.canvas.clientHeight;
  }
  private x(d: number) {
    const total = this.track!.stats.distanceM || 1;
    return PAD.left + (d / total) * (this.width - PAD.left - PAD.right);
  }
  private y(e: number) {
    return PAD.top + (1 - (e - this.yMin) / (this.yMax - this.yMin)) * (this.height - PAD.top - PAD.bottom);
  }
  private indexAtX(px: number): number {
    const t = this.track!;
    const ratio = (px - PAD.left) / (this.width - PAD.left - PAD.right);
    const d = Math.min(1, Math.max(0, ratio)) * t.stats.distanceM;
    return indexAtDistance(t, d);
  }

  private redraw() {
    cancelAnimationFrame(this.frame);
    this.frame = requestAnimationFrame(() => this.draw());
  }

  private draw() {
    const { canvas, ctx } = this;
    const dpr = window.devicePixelRatio || 1;
    const w = this.width;
    const h = this.height;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const t = this.track;
    if (!t || w === 0) return;

    const css = getComputedStyle(document.documentElement);
    const ink = css.getPropertyValue("--text-2").trim();
    const grid = css.getPropertyValue("--grid").trim();
    const accent = css.getPropertyValue("--accent").trim();
    ctx.font = "500 11px 'Inter Tight Variable', system-ui, sans-serif";

    if (!t.stats.hasElevation) {
      ctx.fillStyle = ink;
      ctx.textAlign = "center";
      ctx.fillText("Ce fichier ne contient pas d'altitudes.", w / 2, h / 2);
      return;
    }

    // Grille horizontale + altitudes
    const yStep = niceStep(this.yMax - this.yMin, 4);
    ctx.strokeStyle = grid;
    ctx.fillStyle = ink;
    ctx.lineWidth = 1;
    ctx.textAlign = "right";
    ctx.textBaseline = "middle";
    for (let e = this.yMin; e <= this.yMax + 0.01; e += yStep) {
      const y = Math.round(this.y(e)) + 0.5;
      ctx.beginPath();
      ctx.moveTo(PAD.left, y);
      ctx.lineTo(w - PAD.right, y);
      ctx.stroke();
      ctx.fillText(meters(e), PAD.left - 8, y);
    }

    // Graduations en km
    const xStep = niceStep(t.stats.distanceM / 1000, Math.floor((w - PAD.left - PAD.right) / 90)) * 1000;
    ctx.textAlign = "center";
    ctx.textBaseline = "top";
    for (let d = 0; d <= t.stats.distanceM + 1; d += xStep) {
      const x = Math.round(this.x(d)) + 0.5;
      ctx.strokeStyle = grid;
      ctx.beginPath();
      ctx.moveTo(x, PAD.top);
      ctx.lineTo(x, h - PAD.bottom);
      ctx.stroke();
      ctx.fillText(km(d).replace(",0 km", " km"), x, h - PAD.bottom + 8);
    }

    // Courbe : on ne garde qu'un point par pixel (min/max) pour rester rapide sur les longues traces
    const path = new Path2D();
    let lastPx = -1;
    let lo = Infinity;
    let hi = -Infinity;
    path.moveTo(this.x(0), this.y(t.ele[0]!));
    for (let i = 0; i < t.dist.length; i++) {
      const px = Math.round(this.x(t.dist[i]));
      const e = t.ele[i]!;
      if (px !== lastPx && lastPx >= 0) {
        path.lineTo(lastPx, this.y(hi));
        if (lo !== hi) path.lineTo(lastPx, this.y(lo));
        lo = Infinity;
        hi = -Infinity;
      }
      lastPx = px;
      lo = Math.min(lo, e);
      hi = Math.max(hi, e);
    }
    path.lineTo(lastPx, this.y(t.ele[t.ele.length - 1]!));

    const area = new Path2D(path);
    area.lineTo(this.x(t.stats.distanceM), h - PAD.bottom);
    area.lineTo(this.x(0), h - PAD.bottom);
    area.closePath();
    ctx.fillStyle = withAlpha(accent, 0.2);
    ctx.fill(area);

    // Zones raides (> STEEP %) : remplissage plus soutenu sous la courbe
    ctx.save();
    ctx.clip(area);
    ctx.fillStyle = withAlpha(accent, 0.38);
    let j = 0;
    let runStart = -1;
    const flush = (endX: number) => {
      if (runStart >= 0) ctx.fillRect(runStart, PAD.top, Math.max(1, endX - runStart), h - PAD.bottom - PAD.top);
      runStart = -1;
    };
    for (let i = 0; i < t.dist.length; i++) {
      while (j < t.dist.length - 1 && t.dist[j] - t.dist[i] < 100) j++;
      const run = t.dist[j] - t.dist[i];
      const g = run > 20 ? ((t.ele[j]! - t.ele[i]!) / run) * 100 : 0;
      const x = this.x(t.dist[i]);
      if (g > STEEP) {
        if (runStart < 0) runStart = x;
      } else flush(x);
    }
    flush(this.x(t.stats.distanceM));
    ctx.restore();
    ctx.strokeStyle = accent;
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.stroke(path);

    // Repères des points de passage : pastilles rondes numérotées en haut, trait pointillé
    if (this.marks.length > 0) {
      ctx.save();
      ctx.font = "700 10px 'Inter Tight Variable', system-ui, sans-serif";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      this.marks.forEach((d, k) => {
        const x = Math.round(this.x(Math.min(d, t.stats.distanceM))) + 0.5;
        ctx.strokeStyle = "rgba(255, 255, 255, 0.35)";
        ctx.setLineDash([2, 3]);
        ctx.beginPath();
        ctx.moveTo(x, PAD.top);
        ctx.lineTo(x, h - PAD.bottom);
        ctx.stroke();
        ctx.setLineDash([]);
        const ends = k === 0 || k === this.marks.length - 1;
        ctx.fillStyle = ends ? "#ffffff" : "#111214";
        ctx.strokeStyle = ends ? "#111214" : "#ffffff";
        ctx.lineWidth = 1.5;
        ctx.beginPath();
        ctx.arc(x, 10, 8, 0, Math.PI * 2);
        ctx.fill();
        ctx.stroke();
        ctx.fillStyle = ends ? "#111214" : "#ffffff";
        ctx.fillText(String(k + 1), x, 10.5);
      });
      ctx.restore();
    }

    // Curseur synchronisé avec la carte
    if (this.cursor !== null) {
      const i = this.cursor;
      const x = this.x(t.dist[i]);
      const y = this.y(t.ele[i]!);
      ctx.strokeStyle = ink;
      ctx.setLineDash([3, 3]);
      ctx.beginPath();
      ctx.moveTo(Math.round(x) + 0.5, PAD.top);
      ctx.lineTo(Math.round(x) + 0.5, h - PAD.bottom);
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.fillStyle = accent;
      ctx.strokeStyle = "#fff";
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.arc(x, y, 5, 0, Math.PI * 2);
      ctx.fill();
      ctx.stroke();

      const slope = slopeAt(t, i);
      this.tip.innerHTML =
        `<b>${km(t.dist[i])}</b> · ${meters(t.ele[i]!)}` + (slope === null ? "" : ` · ${percent(slope)}`);
      this.tip.classList.remove("hidden");
      const tw = this.tip.offsetWidth;
      this.tip.style.left = `${Math.min(Math.max(x - tw / 2, 4), w - tw - 4)}px`;
    } else {
      this.tip.classList.add("hidden");
    }
  }
}

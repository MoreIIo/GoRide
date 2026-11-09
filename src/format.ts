const nf0 = new Intl.NumberFormat("fr-FR", { maximumFractionDigits: 0 });
const nf1 = new Intl.NumberFormat("fr-FR", { minimumFractionDigits: 1, maximumFractionDigits: 1 });

export const km = (m: number) => `${(m >= 100_000 ? nf0 : nf1).format(m / 1000)} km`;
export const meters = (m: number) => `${nf0.format(m)} m`;
export const percent = (p: number) => `${p > 0 ? "+" : ""}${nf1.format(p)} %`;

export function duration(hours: number): string {
  const totalMin = Math.round(hours * 60);
  const h = Math.floor(totalMin / 60);
  const min = totalMin % 60;
  return h === 0 ? `${min} min` : `${h} h ${String(min).padStart(2, "0")}`;
}

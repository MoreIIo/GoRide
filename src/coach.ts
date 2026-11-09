// Coach IA local (phase 5) : installation, discussion, séances, avis sur une sortie.
// Le modèle tourne sur le PC via Ollama ; le cœur Rust lui parle et relaie la réponse
// au fil de l'eau (événement « coach-token »).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import type { Athlete } from "./rides";

export type StepKind = "warmup" | "work" | "recovery" | "steady" | "cooldown";
export interface Step {
  kind: StepKind;
  durationS: number;
  powerLow: number;
  powerHigh: number;
  cadenceLow?: number | null;
  cadenceHigh?: number | null;
  note: string;
}
export interface Block {
  repeat: number;
  steps: Step[];
}
export type Terrain = "climb" | "flat" | "any";
export interface Workout {
  name: string;
  goal: string;
  terrain: Terrain;
  blocks: Block[];
}
export interface WorkoutInfo {
  totalS: number;
  workS: number;
  tss: number;
  ftp: number | null;
  ftpSource: "profil" | "compteur" | null;
  hrMax: number | null;
  suggestedKm: number;
}
interface SavedWorkout {
  id: number;
  createdAt: number;
  workout: Workout;
}
interface ModelInfo {
  name: string;
  sizeGb: number;
}
interface CoachStatus {
  running: boolean;
  version: string | null;
  models: ModelInfo[];
  model: string;
  installed: boolean;
  recommended: string;
}
interface CoachNote {
  text: string;
  model: string;
  createdAt: number;
}
/** Action faite dans l'application à la demande du coach. */
interface Action {
  label: string;
  ok: boolean;
}
/** Message de la discussion (affiché et gardé). */
interface ChatEntry {
  role: "user" | "assistant";
  content: string;
  actions?: Action[];
}
interface ToolCall {
  function: { name: string; arguments: Record<string, unknown> | string };
}
/** Message envoyé au modèle (avec les échanges d'outils du tour en cours). */
interface ChatMessage {
  role: "user" | "assistant" | "tool";
  content: string;
  tool_calls?: ToolCall[];
  tool_name?: string;
}
interface Reply {
  text: string;
  toolCalls: ToolCall[];
}
export interface ToolResult {
  /** Résultat envoyé au modèle. */
  text: string;
  /** Ce qui est montré dans la discussion (« 4 boucles gravel de 30 km affichées »). */
  label: string;
  ok: boolean;
}
interface PullProgress {
  status: string;
  completed: number | null;
  total: number | null;
}

export interface CoachDeps {
  athlete: () => Athlete;
  toast: (msg: string, kind?: "error" | "ok") => void;
  /** « Itinéraire pour cette séance » : ouvre les boucles adaptées. */
  onRouteForWorkout: (w: Workout, info: WorkoutInfo) => void;
  /** Fait une action demandée par le coach dans l'application. */
  runTool: (name: string, args: Record<string, unknown>) => Promise<ToolResult>;
  /** Ce que l'application affiche, décrit pour le coach. */
  appState: () => string;
  /** Revient au menu Coach. */
  openCoach: () => void;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
const n0 = new Intl.NumberFormat("fr-FR", { maximumFractionDigits: 0 });
const n1 = new Intl.NumberFormat("fr-FR", { minimumFractionDigits: 1, maximumFractionDigits: 1 });
const tz = () => -new Date().getTimezoneOffset();
const CHAT_KEY = "goride.coach.chat";

// --- Utilitaires ------------------------------------------------------------------------

/** Markdown minimal du coach : paragraphes, listes, **gras**, *italique*. Le texte est d'abord échappé. */
export function md(text: string): string {
  const inline = (s: string) =>
    esc(s)
      .replace(/\*\*(.+?)\*\*/g, "<b>$1</b>")
      .replace(/(^|[^*])\*(?!\s)(.+?)\*(?!\*)/g, "$1<i>$2</i>")
      .replace(/`(.+?)`/g, "<code>$1</code>");
  const out: string[] = [];
  let list: "ul" | "ol" | null = null;
  let para: string[] = [];
  const flushPara = () => {
    if (para.length) out.push(`<p>${para.map(inline).join("<br />")}</p>`);
    para = [];
  };
  const closeList = () => {
    if (list) out.push(`</${list}>`);
    list = null;
  };
  for (const raw of text.split("\n")) {
    const line = raw.trimEnd();
    const ul = /^\s*[-*•]\s+(.*)$/.exec(line);
    const ol = /^\s*\d+[.)]\s+(.*)$/.exec(line);
    const heading = /^\s*#{1,6}\s+(.*)$/.exec(line);
    if (ul || ol) {
      flushPara();
      const kind = ul ? "ul" : "ol";
      if (list !== kind) {
        closeList();
        out.push(`<${kind}>`);
        list = kind;
      }
      out.push(`<li>${inline((ul ?? ol)![1])}</li>`);
    } else if (heading) {
      flushPara();
      closeList();
      out.push(`<p><b>${inline(heading[1])}</b></p>`);
    } else if (line.trim() === "") {
      flushPara();
      closeList();
    } else {
      closeList();
      para.push(line.trim());
    }
  }
  flushPara();
  closeList();
  return out.join("");
}

function actionsHtml(actions: Action[] | undefined): string {
  if (!actions?.length) return "";
  return `<div class="msg-actions">${actions
    .map((a) => `<span class="act${a.ok ? "" : " fail"}"><i>${a.ok ? "✓" : "!"}</i>${esc(a.label)}</span>`)
    .join("")}</div>`;
}

export function fmtDuration(s: number): string {
  s = Math.round(s);
  if (s < 60) return `${s} s`;
  if (s < 3600) return s % 60 === 0 ? `${s / 60} min` : `${Math.floor(s / 60)} min ${String(s % 60).padStart(2, "0")}`;
  const m = Math.round((s % 3600) / 60);
  return m === 0 ? `${Math.floor(s / 3600)} h` : `${Math.floor(s / 3600)} h ${String(m).padStart(2, "0")}`;
}

const ZONE_BOUNDS = [56, 76, 91, 106, 121, 151];
const ZONE_COLORS = ["#9aa0a9", "#3e8bff", "#2bb673", "#e0a100", "#fc4c02", "#d93a3a", "#8a1c1c"];
const zoneOf = (pct: number) => ZONE_BOUNDS.findIndex((b) => pct < b) === -1 ? 6 : ZONE_BOUNDS.findIndex((b) => pct < b);
const KIND_LABEL: Record<StepKind, string> = {
  warmup: "Échauffement",
  work: "Effort",
  recovery: "Récupération",
  steady: "Régulier",
  cooldown: "Retour au calme",
};
export const TERRAIN_LABEL: Record<Terrain, string> = { climb: "En côte", flat: "Sur le plat", any: "Tout terrain" };

const totalS = (w: Workout) => w.blocks.reduce((t, b) => t + b.repeat * b.steps.reduce((s, x) => s + x.durationS, 0), 0);
const tssOf = (w: Workout) =>
  w.blocks.reduce(
    (t, b) => t + b.repeat * b.steps.reduce((s, x) => s + (x.durationS / 3600) * ((x.powerLow + x.powerHigh) / 200) ** 2 * 100, 0),
    0,
  );

/** Étapes dépliées (répétitions comprises) pour le graphique. */
function flatSteps(w: Workout): Step[] {
  return w.blocks.flatMap((b) => Array.from({ length: b.repeat }, () => b.steps).flat());
}

/** Graphique de la séance : une barre par étape, hauteur = puissance, couleur = zone. */
function workoutChart(w: Workout, height: number, mini = false): string {
  const steps = flatSteps(w);
  const total = steps.reduce((t, s) => t + s.durationS, 0) || 1;
  const maxPct = Math.max(130, ...steps.map((s) => s.powerHigh));
  const W = 1000;
  let x = 0;
  const bars = steps
    .map((s) => {
      const w0 = (s.durationS / total) * W;
      const mid = (s.powerLow + s.powerHigh) / 2;
      const h = Math.max(2, (mid / maxPct) * height);
      const r = `<rect x="${x.toFixed(1)}" y="${(height - h).toFixed(1)}" width="${Math.max(0.5, w0 - (mini ? 0.6 : 1.5)).toFixed(
        1,
      )}" height="${h.toFixed(1)}" fill="${ZONE_COLORS[zoneOf(mid)]}"${mini ? "" : ` opacity="0.92"`}><title>${esc(
        KIND_LABEL[s.kind],
      )} · ${fmtDuration(s.durationS)} · ${n0.format(s.powerLow)}–${n0.format(s.powerHigh)} % FTP</title></rect>`;
      x += w0;
      return r;
    })
    .join("");
  const ftpLine = mini ? "" : `<line x1="0" x2="${W}" y1="${height - (100 / maxPct) * height}" y2="${height - (100 / maxPct) * height}" class="wk-ftp" />`;
  return `<svg class="wk-chart${mini ? " mini" : ""}" viewBox="0 0 ${W} ${height}" preserveAspectRatio="none" aria-hidden="true">${bars}${ftpLine}</svg>`;
}

// --- Coach ------------------------------------------------------------------------------

export class Coach {
  private status: CoachStatus | null = null;
  private checking: Promise<CoachStatus | null> | null = null;
  private streams = new Map<number, (text: string) => void>();
  private nextId = Math.floor(Math.random() * 1e6) * 1000;
  private chat: ChatEntry[] = [];
  /** Actions faites pendant la réponse en cours. */
  private liveActions: Action[] = [];
  private chatJob: number | null = null;
  /** « Arrêter » pendant une réponse (y compris entre deux actions). */
  private stopAsked = false;
  private pullJob: number | null = null;
  private workoutJob: number | null = null;
  private tab: "chat" | "workouts" = "chat";
  private templates: Workout[] = [];
  private mine: SavedWorkout[] = [];
  /** Séance affichée à droite : d'où elle vient (type, enregistrée, proposée). */
  private shown: { workout: Workout; savedId: number | null; info: WorkoutInfo | null } | null = null;
  /** Avis en cours de rédaction, par sortie. */
  private rideJobs = new Map<number, { id: number; text: string }>();

  constructor(private deps: CoachDeps) {
    try {
      this.chat = JSON.parse(localStorage.getItem(CHAT_KEY) ?? "[]");
    } catch {
      this.chat = [];
    }
    if ("__TAURI_INTERNALS__" in window) {
      listen<{ id: number; text: string }>("coach-token", (e) => this.streams.get(e.payload.id)?.(e.payload.text));
      listen<PullProgress>("coach-pull", (e) => this.renderPull(e.payload));
    }
    this.bind();
  }

  private id() {
    return ++this.nextId;
  }

  /** Panneau ouvert (menu Coach). */
  async open() {
    this.renderTab();
    this.renderChat();
    await this.refreshStatus();
    if (this.tab === "workouts") this.loadWorkouts();
  }

  /** Le coach est-il prêt à répondre ? Vérifie au besoin. */
  async ready(): Promise<boolean> {
    const s = this.status ?? (await this.refreshStatus());
    return !!s && s.running && s.installed;
  }

  async refreshStatus(): Promise<CoachStatus | null> {
    if (!this.checking) {
      this.checking = invoke<CoachStatus>("coach_status")
        .then((s) => (this.status = s))
        .catch((err) => {
          this.deps.toast(String(err));
          return null;
        })
        .finally(() => (this.checking = null));
      this.renderSetup(true);
    }
    const s = await this.checking;
    this.renderSetup(false);
    return s;
  }

  // --- Installation -----------------------------------------------------------------

  private renderSetup(checking: boolean) {
    const s = this.status;
    const box = $("coach-setup");
    const chip = $("coach-model");
    const ready = !!s && s.running && s.installed;
    document.body.classList.toggle("coach-ready", ready);
    chip.classList.toggle("ok", ready);
    chip.querySelector("span")!.textContent = checking && !s ? "Vérification…" : ready ? s!.model : s?.running ? "Modèle à installer" : "Hors ligne";
    if (ready) {
      box.classList.add("hidden");
      box.innerHTML = "";
      return;
    }
    box.classList.remove("hidden");
    if (checking && !s) {
      box.innerHTML = `<p class="hint">Recherche d'Ollama sur ton PC…</p>`;
      return;
    }
    const step = (n: number, done: boolean, active: boolean, title: string, body: string) =>
      `<li class="cs-step${done ? " done" : ""}${active ? " active" : ""}"><span class="cs-n">${done ? "✓" : n}</span><div><b>${title}</b>${body}</div></li>`;
    const others = (s?.models ?? []).filter((m) => m.name !== s?.model);
    const pulling = this.pullJob !== null;
    box.innerHTML = `<p class="cs-lead">Le coach est un modèle d'IA qui tourne <b>sur ton PC</b>, sans connexion : tes sorties ne quittent jamais ta machine. Deux étapes, une seule fois.</p>
      <ol class="cs-steps">
        ${step(
          1,
          !!s?.running,
          !s?.running,
          "Installer Ollama",
          s?.running
            ? `<p>Ollama ${esc(s.version ?? "")} est lancé.</p>`
            : `<p>Le moteur qui fait tourner le modèle (gratuit, ~1 Go). Une fois installé, il démarre tout seul avec Windows.</p>
               <div class="row"><button class="btn primary small" id="cs-get">Télécharger Ollama</button><button class="btn ghost small" id="cs-retry">C'est fait, vérifier</button></div>`,
        )}
        ${step(
          2,
          !!s?.installed,
          !!s?.running && !s.installed,
          "Télécharger le modèle du coach",
          `<p><code>${esc(s?.model ?? "qwen3:8b")}</code> · environ 5 Go. Il tient entièrement dans les 8 Go de ta RTX 3070 Ti : réponses rapides.</p>
           ${
             s?.running
               ? `<div id="cs-pull" class="cs-pull${pulling ? "" : " hidden"}"><div class="cs-bar"><i></i></div><small id="cs-pull-txt">Préparation…</small></div>
                  <div class="row">${
                    pulling
                      ? `<button class="btn ghost small" id="cs-pull-stop">Arrêter</button>`
                      : `<button class="btn primary small" id="cs-pull-go">Télécharger le modèle</button>`
                  }</div>
                  ${
                    others.length && !pulling
                      ? `<p class="hint small">Ou utilise un modèle déjà installé : ${others
                          .map((m) => `<button class="cs-model" data-model="${esc(m.name)}">${esc(m.name)}</button>`)
                          .join(" ")}</p>`
                      : ""
                  }`
               : ""
           }`,
        )}
      </ol>`;
    $("cs-get")?.addEventListener("click", () => invoke("open_ollama_site").catch((e) => this.deps.toast(String(e))));
    $("cs-retry")?.addEventListener("click", () => this.refreshStatus());
    $("cs-pull-go")?.addEventListener("click", () => this.pull());
    $("cs-pull-stop")?.addEventListener("click", () => this.pullJob !== null && invoke("coach_stop", { id: this.pullJob }));
    box.querySelectorAll<HTMLButtonElement>("[data-model]").forEach((b) => b.addEventListener("click", () => this.useModel(b.dataset.model!)));
  }

  private async useModel(model: string) {
    try {
      await invoke("coach_set_model", { model });
      await this.refreshStatus();
    } catch (err) {
      this.deps.toast(String(err));
    }
  }

  private async pull() {
    const id = this.id();
    this.pullJob = id;
    this.renderSetup(false);
    try {
      await invoke("coach_pull", { id, model: null });
      this.deps.toast("Modèle installé : le coach est prêt.", "ok");
    } catch (err) {
      this.deps.toast(String(err));
    } finally {
      this.pullJob = null;
      await this.refreshStatus();
    }
  }

  private renderPull(p: PullProgress) {
    const box = $("cs-pull");
    if (!box) return;
    box.classList.remove("hidden");
    const bar = box.querySelector<HTMLElement>(".cs-bar i")!;
    const txt = $("cs-pull-txt");
    const gb = (b: number) => n1.format(b / 1e9);
    if (p.total && p.completed !== null && p.total > 50e6) {
      bar.style.width = `${Math.min(100, (p.completed / p.total) * 100)}%`;
      txt.textContent = `Téléchargement : ${gb(p.completed)} / ${gb(p.total)} Go`;
    } else if (p.status.startsWith("verifying")) txt.textContent = "Vérification du fichier…";
    else if (p.status.startsWith("writing") || p.status === "success") {
      bar.style.width = "100%";
      txt.textContent = "Installation…";
    } else if (p.status.includes("manifest")) txt.textContent = "Préparation…";
  }

  // --- Onglets ------------------------------------------------------------------------

  private bind() {
    document.querySelectorAll<HTMLButtonElement>("#coach-tabs [data-tab]").forEach((b) =>
      b.addEventListener("click", () => {
        this.tab = b.dataset.tab as "chat" | "workouts";
        this.renderTab();
        if (this.tab === "workouts") this.loadWorkouts();
      }),
    );
    $("coach-model").addEventListener("click", () => this.refreshStatus());
    $("coach-float-open").addEventListener("click", () => this.deps.openCoach());
    $("coach-float-close").addEventListener("click", () => this.hideFloat());
    const input = $<HTMLTextAreaElement>("coach-input");
    $("coach-form").addEventListener("submit", (e) => {
      e.preventDefault();
      if (this.chatJob !== null) {
        this.stopAsked = true;
        invoke("coach_stop", { id: this.chatJob });
        return;
      }
      this.ask(input.value);
    });
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        if (this.chatJob === null) this.ask(input.value);
      }
    });
    input.addEventListener("input", () => this.autosize());
    $("coach-clear").addEventListener("click", () => {
      if (this.chatJob !== null) return;
      this.chat = [];
      this.saveChat();
      this.renderChat();
    });
    $("coach-msgs").addEventListener("click", (e) => {
      const chip = (e.target as HTMLElement).closest<HTMLButtonElement>("[data-ask]");
      if (chip) this.ask(chip.dataset.ask!);
    });
    $("wk-form").addEventListener("submit", (e) => {
      e.preventDefault();
      if (this.workoutJob !== null) invoke("coach_stop", { id: this.workoutJob });
      else this.createWorkout($<HTMLInputElement>("wk-wish").value);
    });
    const pick = (e: Event) => {
      const li = (e.target as HTMLElement).closest<HTMLElement>("[data-wk]");
      if (!li) return;
      const [kind, i] = li.dataset.wk!.split(":");
      if (kind === "t") this.showWorkout(this.templates[Number(i)], null);
      else {
        const s = this.mine.find((m) => m.id === Number(i));
        if (s) this.showWorkout(s.workout, s.id);
      }
    };
    $("wk-templates").addEventListener("click", pick);
    $("wk-mine").addEventListener("click", pick);
  }

  private renderTab() {
    document.querySelectorAll<HTMLButtonElement>("#coach-tabs [data-tab]").forEach((b) => b.classList.toggle("active", b.dataset.tab === this.tab));
    $("coach-chat").classList.toggle("hidden", this.tab !== "chat");
    $("coach-workouts").classList.toggle("hidden", this.tab !== "workouts");
  }

  // --- Discussion -------------------------------------------------------------------------

  private saveChat() {
    try {
      localStorage.setItem(CHAT_KEY, JSON.stringify(this.chat.slice(-40)));
    } catch {
      /* rien */
    }
  }

  private autosize() {
    const input = $<HTMLTextAreaElement>("coach-input");
    input.style.height = "auto";
    input.style.height = `${Math.min(140, input.scrollHeight)}px`;
  }

  private renderChat(pending?: string) {
    const box = $("coach-msgs");
    if (this.chat.length === 0 && pending === undefined) {
      const ideas = [
        "Comment aller plus vite sur le plat ?",
        "Que dois-je faire cette semaine ?",
        "Que disent mes dernières sorties de ma forme ?",
        "Comment connaître ma FTP et ma FC max ?",
      ];
      box.innerHTML = `<div class="coach-empty">
          <p class="eyebrow">Ton entraîneur</p>
          <p>Il connaît tes sorties, ta forme et ta fatigue du jour. Pose-lui une question, ou demande-lui une séance dans l'onglet <b>Séances</b>.</p>
          <div class="coach-ideas">${ideas.map((q) => `<button class="coach-idea" data-ask="${esc(q)}">${esc(q)}</button>`).join("")}</div>
        </div>`;
      return;
    }
    const msgs = this.chat
      .map((m) =>
        m.role === "user"
          ? `<div class="msg user">${esc(m.content).replace(/\n/g, "<br />")}</div>`
          : `<div class="msg coach">${actionsHtml(m.actions)}${md(m.content)}</div>`,
      )
      .join("");
    const live =
      pending === undefined
        ? ""
        : `<div class="msg coach waiting" id="coach-live"><span class="dots"><i></i><i></i><i></i></span><small id="coach-wait">Le coach réfléchit…</small></div>`;
    box.innerHTML = msgs + live;
    if (pending) this.renderLive(pending);
    box.scrollTop = box.scrollHeight;
  }

  /** Réponse en cours : seul ce message est redessiné (les autres ne bougent pas). */
  private renderLive(text: string) {
    this.renderFloat(text, true);
    const live = $("coach-live");
    if (!live) return;
    const box = $("coach-msgs");
    const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
    if (text) {
      live.className = "msg coach streaming";
      live.innerHTML = actionsHtml(this.liveActions) + md(text);
    } else {
      live.className = "msg coach waiting";
      live.innerHTML = `${actionsHtml(this.liveActions)}<span class="dots"><i></i><i></i><i></i></span><small id="coach-wait">${
        this.liveActions.length ? "Le coach regarde le résultat…" : "Le coach réfléchit…"
      }</small>`;
    }
    if (atBottom) box.scrollTop = box.scrollHeight;
  }

  /**
   * Réponse du coach hors du menu Coach (une action a ouvert la carte ou une sortie) :
   * petite carte flottante, pour lire la réponse et revenir à la discussion.
   */
  private renderFloat(text: string, live: boolean, actions: Action[] = this.liveActions) {
    const f = $("coach-float");
    if (document.body.classList.contains("mode-coach")) {
      f.classList.add("hidden");
      return;
    }
    if (!text && !actions.length) return;
    f.classList.remove("hidden");
    $("coach-float-body").innerHTML =
      actionsHtml(actions) +
      (text ? `<div class="coach-text${live ? " streaming" : ""}">${md(text)}</div>` : `<p class="hint"><span class="dots"><i></i><i></i><i></i></span> Le coach regarde le résultat…</p>`);
    const b = $("coach-float-body");
    b.scrollTop = b.scrollHeight;
  }

  /** Masque la carte flottante (retour au menu Coach, ou fermeture). */
  hideFloat() {
    $("coach-float").classList.add("hidden");
  }

  private setChatBusy(busy: boolean) {
    const send = $<HTMLButtonElement>("coach-send");
    send.classList.toggle("stop", busy);
    send.title = busy ? "Arrêter" : "Envoyer (Entrée)";
    send.setAttribute("aria-label", send.title);
  }

  /** Historique envoyé au modèle : les actions passées y figurent en clair. */
  private history(): ChatMessage[] {
    return this.chat.map((m) => ({
      role: m.role,
      content:
        m.actions?.length && m.role === "assistant"
          ? `${m.content}\n\n(Actions faites dans GoRide : ${m.actions.map((a) => a.label + (a.ok ? "" : " — échec")).join(" ; ")})`
          : m.content,
    }));
  }

  async ask(question: string) {
    question = question.trim();
    if (!question || this.chatJob !== null) return;
    if (!(await this.ready())) {
      this.deps.toast("Le coach n'est pas encore installé : suis les deux étapes en haut du panneau.");
      return;
    }
    const input = $<HTMLTextAreaElement>("coach-input");
    input.value = "";
    this.autosize();
    const messages = this.history();
    messages.push({ role: "user", content: question });
    this.chat.push({ role: "user", content: question });
    this.saveChat();
    this.liveActions = [];
    this.stopAsked = false;
    this.setChatBusy(true);
    this.renderChat("");
    // Premier lancement : le modèle se charge en mémoire vidéo (jusqu'à ~30 s)
    const slow = window.setTimeout(() => {
      const w = $("coach-wait");
      if (w && !this.liveActions.length) w.textContent = "Chargement du modèle en mémoire… (la première réponse est la plus lente)";
    }, 5000);
    let text = "";
    let failed = false;
    try {
      // Le coach peut enchaîner quelques actions avant de répondre
      // Actions déjà tentées (nom + paramètres) : on ne refait pas deux fois la même
      const tried = new Map<string, ToolResult>();
      const MAX_ROUNDS = 4;
      for (let round = 0; round <= MAX_ROUNDS; round++) {
        // Dernier tour : plus d'actions possibles, le coach doit répondre par écrit
        const last = round === MAX_ROUNDS;
        const id = this.id();
        this.chatJob = id;
        let frame = 0;
        let streamed = "";
        this.streams.set(id, (d) => {
          streamed += d;
          if (!frame)
            frame = requestAnimationFrame(() => {
              frame = 0;
              this.renderLive(streamed);
            });
        });
        let reply: Reply;
        try {
          reply = await invoke<Reply>("coach_chat", {
            id,
            messages,
            athlete: this.deps.athlete(),
            tzOffsetMin: tz(),
            appState: this.deps.appState(),
            useTools: !last,
          });
        } finally {
          cancelAnimationFrame(frame);
          this.streams.delete(id);
        }
        text = reply.text || streamed;
        const calls = last ? [] : reply.toolCalls;
        if (!calls.length || this.stopAsked) break;
        let failures = 0;
        messages.push({ role: "assistant", content: reply.text, tool_calls: calls });
        text = "";
        this.renderLive("");
        for (const c of calls) {
          const name = c.function.name;
          let args: Record<string, unknown> = {};
          try {
            args = typeof c.function.arguments === "string" ? JSON.parse(c.function.arguments) : (c.function.arguments ?? {});
          } catch {
            /* arguments illisibles : l'outil le dira */
          }
          const key = name + JSON.stringify(args);
          let r: ToolResult;
          const before = tried.get(key);
          if (before && !before.ok) {
            // Même action, mêmes paramètres, même échec : on ne la refait pas
            r = { ...before, text: `${before.text} (action déjà tentée avec ces paramètres : ne la relance pas, explique le problème ou essaie autrement)` };
          } else {
            try {
              r = await this.deps.runTool(name, args);
            } catch (err) {
              r = { text: `Échec : ${err}`, label: `Action « ${name} » impossible`, ok: false };
            }
            tried.set(key, r);
            this.liveActions.push({ label: r.label, ok: r.ok });
          }
          if (!r.ok) failures++;
          if (this.stopAsked) break;
          messages.push({ role: "tool", content: r.text, tool_name: name });
          this.renderLive("");
        }
        // Deux échecs dans le même tour : on passe directement à la réponse écrite
        if (failures >= 2) round = MAX_ROUNDS - 1;
      }
    } catch (err) {
      failed = true;
      this.deps.toast(String(err));
    } finally {
      clearTimeout(slow);
      this.chatJob = null;
      this.setChatBusy(false);
      const actions = this.liveActions;
      this.liveActions = [];
      text = text.trim();
      if (text || actions.length) this.chat.push({ role: "assistant", content: text, actions: actions.length ? actions : undefined });
      else if (this.chat[this.chat.length - 1]?.role === "user") this.chat.pop();
      this.saveChat();
      this.renderChat();
      this.renderFloat(text, false, actions);
      if (!text && !actions.length && failed) input.value = question;
    }
  }

  // --- Séances ----------------------------------------------------------------------------

  private async loadWorkouts() {
    try {
      if (this.templates.length === 0) this.templates = await invoke<Workout[]>("workout_templates");
      this.mine = await invoke<SavedWorkout[]>("list_workouts");
    } catch (err) {
      this.deps.toast(String(err));
    }
    this.renderWorkoutLists();
  }

  private renderWorkoutLists() {
    const item = (w: Workout, key: string, selected: boolean) => `<li class="wk-item${selected ? " selected" : ""}" data-wk="${key}">
        ${workoutChart(w, 28, true)}
        <span class="wk-name">${esc(w.name)}</span>
        <span class="wk-meta">${fmtDuration(totalS(w))} · TSS ${n0.format(tssOf(w))} · ${TERRAIN_LABEL[w.terrain]}</span>
      </li>`;
    const sel = this.shown;
    $("wk-mine").innerHTML = this.mine.length
      ? this.mine.map((s) => item(s.workout, `m:${s.id}`, sel?.savedId === s.id)).join("")
      : `<li class="hint small wk-none">Les séances que tu crées avec le coach et que tu gardes apparaissent ici.</li>`;
    $("wk-templates").innerHTML = this.templates.map((w, i) => item(w, `t:${i}`, sel?.savedId === null && sel.workout === w)).join("");
  }

  private async createWorkout(wish: string) {
    if (!(await this.ready())) {
      this.deps.toast("Le coach n'est pas encore installé : suis les deux étapes en haut du panneau.");
      return;
    }
    const id = this.id();
    this.workoutJob = id;
    const btn = $<HTMLButtonElement>("wk-go");
    const status = $("wk-status");
    btn.textContent = "Arrêter";
    status.classList.remove("hidden");
    status.innerHTML = `<span class="dots"><i></i><i></i><i></i></span> Le coach prépare ta séance…`;
    this.streams.set(id, (n) => {
      status.innerHTML = `<span class="dots"><i></i><i></i><i></i></span> Le coach écrit ta séance… (${n} caractères)`;
    });
    try {
      const w = await invoke<Workout>("coach_workout", { id, wish, athlete: this.deps.athlete(), tzOffsetMin: tz() });
      await this.showWorkout(w, null, true);
    } catch (err) {
      this.deps.toast(String(err));
    } finally {
      this.streams.delete(id);
      this.workoutJob = null;
      btn.textContent = "Créer";
      status.classList.add("hidden");
    }
  }

  async showWorkout(w: Workout, savedId: number | null, proposed = false) {
    let info: WorkoutInfo | null = null;
    try {
      info = await invoke<WorkoutInfo>("workout_info", { workout: w, athlete: this.deps.athlete() });
    } catch (err) {
      this.deps.toast(String(err));
    }
    this.shown = { workout: w, savedId, info };
    this.renderWorkoutLists();
    const p = $("workout-panel");
    p.classList.remove("hidden");
    const ftp = info?.ftp ?? null;
    const watts = (pct: number) => (ftp ? `${n0.format((pct / 100) * ftp)}` : "");
    const target = (s: Step) =>
      ftp
        ? `<b>${watts(s.powerLow)}–${watts(s.powerHigh)} W</b><small>${n0.format(s.powerLow)}–${n0.format(s.powerHigh)} % FTP</small>`
        : `<b>${n0.format(s.powerLow)}–${n0.format(s.powerHigh)} %</b><small>de la FTP</small>`;
    const stepRow = (s: Step) => {
      const mid = (s.powerLow + s.powerHigh) / 2;
      const extra = [s.cadenceLow && s.cadenceHigh ? `${s.cadenceLow}–${s.cadenceHigh} tr/min` : "", s.note].filter(Boolean).join(" · ");
      return `<li class="wk-step"><span class="wk-zone" style="background:${ZONE_COLORS[zoneOf(mid)]}"></span>
        <span><b>${KIND_LABEL[s.kind]}</b> · ${fmtDuration(s.durationS)}${extra ? `<small>${esc(extra)}</small>` : ""}</span>
        <span class="wk-target">${target(s)}</span></li>`;
    };
    const blocks = w.blocks
      .map((b) =>
        b.repeat > 1
          ? `<li class="wk-repeat"><span class="wk-times">${b.repeat} ×</span><ul>${b.steps.map(stepRow).join("")}</ul></li>`
          : b.steps.map(stepRow).join(""),
      )
      .join("");
    const ftpNote = !info
      ? ""
      : info.ftp
        ? `Watts calculés avec ta FTP de ${n0.format(info.ftp)} W${info.ftpSource === "compteur" ? " réglée sur ton Garmin" : ""}.`
        : info.hrMax
          ? `FTP inconnue : le fichier Garmin guidera à la fréquence cardiaque (FC max ${n0.format(info.hrMax)} bpm).`
          : `FTP et FC max inconnues : renseigne-les dans Sorties → Mon profil pour avoir des cibles en watts ou en bpm.`;
    const need =
      w.terrain === "climb"
        ? "Les efforts se font en montée : GoRide cherche une boucle avec une côte assez longue."
        : w.terrain === "flat"
          ? "Les efforts demandent une longue portion plate : GoRide cherche une boucle roulante."
          : "";
    p.innerHTML = `<header class="ride-head">
        <div><p class="eyebrow">${proposed ? "Proposée par le coach" : savedId !== null ? "Mes séances" : "Séance type"}</p><h2>${esc(w.name)}</h2></div>
        <button class="icon-btn" id="workout-close2" title="Fermer"><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6 6 18" /></svg></button>
      </header>
      ${w.goal ? `<p class="wk-goal">${esc(w.goal)}</p>` : ""}
      <dl class="grid wk-kpis">
        <div><dt>Durée</dt><dd>${fmtDuration(totalS(w))}</dd></div>
        <div><dt>Charge</dt><dd>TSS ${n0.format(tssOf(w))}</dd></div>
        <div><dt>Efforts</dt><dd>${fmtDuration(info?.workS ?? 0)}</dd></div>
        <div><dt>Terrain</dt><dd>${TERRAIN_LABEL[w.terrain]}</dd></div>
      </dl>
      <div class="wk-chart-box">${workoutChart(w, 110)}<span class="wk-ftp-label">FTP</span></div>
      <ul class="wk-steps">${blocks}</ul>
      <p class="hint small">${ftpNote}</p>
      <div class="wk-actions">
        <button class="btn ${w.terrain === "any" ? "ghost" : "primary"} full" id="wk-route">Itinéraire pour cette séance</button>
        ${need ? `<p class="hint small">${need}</p>` : ""}
        <div class="row">
          <button class="btn ghost small" id="wk-export" title="Fichier .fit à copier dans Garmin/NewFiles du compteur">Exporter pour Garmin</button>
          ${
            savedId !== null
              ? `<button class="btn ghost small" id="wk-delete">Supprimer</button>`
              : `<button class="btn ghost small" id="wk-keep">Garder dans mes séances</button>`
          }
        </div>
        <p class="hint small">Compteur branché en USB : copie le fichier dans <code>Garmin/NewFiles</code>. La séance apparaît dans <b>Entraînement → Séances</b>.</p>
      </div>`;
    $("workout-close2").addEventListener("click", () => this.hideWorkout());
    $("wk-route").addEventListener("click", async () => {
      const i = info ?? (await invoke<WorkoutInfo>("workout_info", { workout: w, athlete: this.deps.athlete() }).catch(() => null));
      if (i) this.deps.onRouteForWorkout(w, i);
    });
    $("wk-export").addEventListener("click", () => this.exportWorkout(w));
    $("wk-keep")?.addEventListener("click", async () => {
      try {
        const s = await invoke<SavedWorkout>("keep_workout", { workout: w });
        this.mine.unshift(s);
        this.deps.toast("Séance gardée dans « Mes séances ».", "ok");
        this.showWorkout(s.workout, s.id);
      } catch (err) {
        this.deps.toast(String(err));
      }
    });
    $("wk-delete")?.addEventListener("click", async () => {
      if (savedId === null) return;
      try {
        await invoke("delete_workout", { id: savedId });
        this.mine = this.mine.filter((m) => m.id !== savedId);
        this.hideWorkout();
      } catch (err) {
        this.deps.toast(String(err));
      }
    });
  }

  /** Séances types (chargées au besoin). */
  async templateList(): Promise<Workout[]> {
    if (this.templates.length === 0) this.templates = await invoke<Workout[]>("workout_templates");
    return this.templates;
  }

  /** Séance affichée à droite, s'il y en a une. */
  get shownWorkout(): { workout: Workout; info: WorkoutInfo | null } | null {
    return this.shown;
  }

  hideWorkout() {
    this.shown = null;
    $("workout-panel").classList.add("hidden");
    this.renderWorkoutLists();
  }

  private async exportWorkout(w: Workout) {
    const safe = w.name.replace(/[\\/:*?"<>|]/g, "-").trim() || "seance";
    const path = await save({ defaultPath: `${safe}.fit`, filters: [{ name: "Séance Garmin (FIT)", extensions: ["fit"] }] });
    if (!path) return;
    try {
      await invoke("export_workout", { path, workout: w, athlete: this.deps.athlete() });
      this.deps.toast("Séance exportée : copie-la dans Garmin/NewFiles du compteur.", "ok");
    } catch (err) {
      this.deps.toast(String(err));
    }
  }

  // --- Avis sur une sortie ----------------------------------------------------------------

  /** Bloc « Avis du coach » dans le détail d'une sortie. */
  async mountRide(container: HTMLElement, activityId: number) {
    const job = this.rideJobs.get(activityId);
    if (job) {
      this.renderRide(container, activityId, null, job.text, true);
      return;
    }
    let note: CoachNote | null = null;
    try {
      note = await invoke<CoachNote | null>("coach_ride_note", { activityId });
    } catch {
      /* base indisponible : le bouton reste proposé */
    }
    if (!container.isConnected) return;
    this.renderRide(container, activityId, note, "", false);
  }

  private renderRide(container: HTMLElement, activityId: number, note: CoachNote | null, live: string, busy: boolean) {
    const date = (t: number) => new Intl.DateTimeFormat("fr-FR", { day: "numeric", month: "short" }).format(new Date(t * 1000));
    if (busy) {
      container.innerHTML = `<h3>Avis du coach</h3>${
        live ? `<div class="coach-text streaming">${md(live)}</div>` : `<p class="hint"><span class="dots"><i></i><i></i><i></i></span> Le coach lit ta sortie…</p>`
      }<button class="btn ghost small" data-coach-stop>Arrêter</button>`;
    } else if (note) {
      container.innerHTML = `<h3>Avis du coach</h3><div class="coach-text">${md(note.text)}</div>
        <p class="coach-meta"><span>${esc(note.model)} · ${date(note.createdAt)}</span><button class="nm-link" data-coach-ride>Nouvel avis</button></p>`;
    } else {
      container.innerHTML = `<button class="coach-cta" data-coach-ride>
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3l1.9 4.6L18.5 9l-4.6 1.9L12 15.5l-1.9-4.6L5.5 9l4.6-1.4L12 3zM18 15l.9 2.1L21 18l-2.1.9L18 21l-.9-2.1L15 18l2.1-.9L18 15z" /></svg>
          <span><b>Avis du coach</b><small>Ce qui va bien, ce qu'il faut travailler, la prochaine séance</small></span>
        </button>`;
    }
    container.querySelector("[data-coach-ride]")?.addEventListener("click", () => this.commentRide(container, activityId));
    container.querySelector("[data-coach-stop]")?.addEventListener("click", () => {
      const j = this.rideJobs.get(activityId);
      if (j) invoke("coach_stop", { id: j.id });
    });
  }

  private async commentRide(container: HTMLElement, activityId: number) {
    if (!(await this.ready())) {
      this.deps.toast("Installe d'abord le coach : menu Coach en haut à gauche.");
      return;
    }
    const id = this.id();
    const job = { id, text: "" };
    this.rideJobs.set(activityId, job);
    // Le panneau peut avoir été redessiné (autre sortie, réglage du profil) : on vise le bloc actuel
    const target = () => (container.isConnected ? container : document.querySelector<HTMLElement>(`[data-ride-coach="${activityId}"]`));
    let frame = 0;
    this.streams.set(id, (d) => {
      job.text += d;
      if (!frame)
        frame = requestAnimationFrame(() => {
          frame = 0;
          const c = target();
          if (c) this.renderRide(c, activityId, null, job.text, true);
        });
    });
    this.renderRide(container, activityId, null, "", true);
    let note: CoachNote | null = null;
    try {
      note = await invoke<CoachNote>("coach_ride_comment", { id, activityId, athlete: this.deps.athlete(), tzOffsetMin: tz() });
    } catch (err) {
      this.deps.toast(String(err));
      note = await invoke<CoachNote | null>("coach_ride_note", { activityId }).catch(() => null);
    } finally {
      cancelAnimationFrame(frame);
      this.streams.delete(id);
      this.rideJobs.delete(activityId);
    }
    const c = target();
    if (c) this.renderRide(c, activityId, note, "", false);
  }
}

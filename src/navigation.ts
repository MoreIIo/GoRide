import { Channel, invoke } from "@tauri-apps/api/core";
import maplibregl from "maplibre-gl";
import { duration, km } from "./format";
import type { TrackMap } from "./map";
import {
  bearing,
  maneuverIcon,
  maneuverSpeech,
  maneuverText,
  navDistance,
  NavTracker,
  RouteMatcher,
  type Maneuver,
  type NavUpdate,
} from "./nav_core";
import type { Track } from "./types";

type LngLat = [number, number];

/** Une position donnée par le GPS (ou par l'aperçu). */
interface GpsFix {
  p: LngLat;
  /** Précision horizontale (m). */
  accuracy: number;
  /** Vitesse (m/s) et cap (degrés), quand le téléphone les donne. */
  speed: number | null;
  heading: number | null;
}

interface Deps {
  trackMap: TrackMap;
  toast: (msg: string, kind?: "error" | "ok", ms?: number) => void;
  /** Vitesse moyenne prévue (km/h), celle du panneau de l'itinéraire. */
  planSpeed: () => number;
  /** Dans l'application (GPS du téléphone) ou dans un simple navigateur. */
  inTauri: boolean;
  /** Appelé quand la navigation se termine (recadrer la carte…). */
  onEnd: () => void;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const SOUND_KEY = "goride.nav.sound";
/** L'aperçu déroule le parcours en accéléré (× la vitesse prévue). */
const PREVIEW_FACTOR = 10;
/** Inclinaison de la carte pendant le guidage (degrés). */
const NAV_PITCH = 52;

const clock = new Intl.DateTimeFormat("fr-FR", { hour: "2-digit", minute: "2-digit" });

/**
 * Mode navigation : la carte suit la position, orientée dans le sens de la marche ; un bandeau
 * annonce la prochaine direction, une barre donne la distance et l'heure d'arrivée.
 * Le téléphone vibre et sonne à l'approche d'un changement de direction.
 */
export class Navigation {
  active = false;
  private matcher: RouteMatcher | null = null;
  private tracker: NavTracker | null = null;
  private stopGps: (() => void) | null = null;
  private preview = false;
  /** La carte suit la position (faux dès que l'utilisateur la déplace lui-même). */
  private following = true;
  /** Un doigt est posé sur la carte : on ne la bouge pas sous lui (son geste serait annulé). */
  private held = false;
  private puck: maplibregl.Marker | null = null;
  private puckAt: LngLat = [0, 0];
  private puckAnim = 0;
  private course = 0;
  private zoom = 16.4;
  private lastFixAt = 0;
  private lastM: number | null = null;
  private doneM = -1e9;
  /** Vitesse moyenne récente (m/s), pour l'heure d'arrivée. */
  private avg = 7;
  private said = new Set<string>();
  private wasOff = false;
  private sound = true;
  private audio: AudioContext | null = null;
  private wake: WakeLockSentinel | null = null;
  private camera = { pitch: 0, bearing: 0 };

  constructor(private deps: Deps) {
    try {
      this.sound = localStorage.getItem(SOUND_KEY) !== "off";
    } catch {
      /* stockage indisponible : son actif */
    }
    this.showSound();
    $("nav-stop").addEventListener("click", () => this.stop());
    $("nav-recenter").addEventListener("click", () => this.setFollowing(true));
    $("nav-sound").addEventListener("click", () => {
      this.sound = !this.sound;
      try {
        localStorage.setItem(SOUND_KEY, this.sound ? "on" : "off");
      } catch {
        /* rien */
      }
      this.showSound();
      if (this.sound) this.beep(1);
      else window.speechSynthesis?.cancel();
    });
    // La carte déplacée à la main cesse de suivre : un bouton permet d'y revenir. Tant qu'un
    // doigt est posé, on suspend le suivi : déplacer la carte à cet instant annulerait son geste.
    const map = deps.trackMap.map;
    for (const ev of ["dragstart", "zoomstart", "rotatestart", "pitchstart", "wheel"] as const) {
      map.on(ev, (e) => {
        if (this.active && (e as { originalEvent?: Event }).originalEvent) this.setFollowing(false);
      });
    }
    map.getCanvasContainer().addEventListener("pointerdown", () => {
      if (!this.active) return;
      this.held = true;
      if (this.following) map.stop();
    });
    for (const ev of ["pointerup", "pointercancel"]) window.addEventListener(ev, () => (this.held = false));
    // L'écran reste allumé ; le verrou saute quand l'appli passe en arrière-plan, on le reprend au retour
    document.addEventListener("visibilitychange", () => {
      if (this.active && document.visibilityState === "visible") this.keepAwake();
    });
  }

  /**
   * Lance le guidage sur un itinéraire. `preview` : pas de GPS, le parcours défile tout seul
   * (pour voir les annonces depuis son canapé, ou sur un PC).
   */
  async start(track: Track, maneuvers: Maneuver[], preview: boolean) {
    if (this.active || track.coords.length < 2) return;
    const map = this.deps.trackMap.map;
    this.matcher = new RouteMatcher(track.coords, track.dist);
    // Sans guidage calculé, il reste au moins l'arrivée (distance et heure restent justes)
    const last = track.coords[track.coords.length - 1];
    const list = maneuvers.length ? maneuvers : [{ kind: "arrive" as const, atM: this.matcher.total, point: last, angle: 0, exit: null }];
    this.tracker = new NavTracker(this.matcher, list);
    this.preview = preview;
    this.active = true;
    this.lastFixAt = 0;
    this.lastM = null;
    this.doneM = -1e9;
    this.avg = this.deps.planSpeed() / 3.6;
    this.said.clear();
    this.wasOff = false;
    this.camera = { pitch: map.getPitch(), bearing: map.getBearing() };
    this.held = false;
    this.deps.trackMap.locked = true;
    document.body.classList.add("navigating");
    $("nav").classList.remove("hidden");
    $("nav-stop").textContent = "Arrêter";
    this.setFollowing(true);
    this.showWaiting();
    this.unlockAudio();
    this.keepAwake();
    try {
      this.stopGps = preview ? this.simulate() : await this.watchGps();
    } catch (err) {
      this.deps.toast(String(err instanceof Error ? err.message : err));
      this.stop();
    }
  }

  stop() {
    if (!this.active) return;
    this.active = false;
    this.stopGps?.();
    this.stopGps = null;
    cancelAnimationFrame(this.puckAnim);
    this.puck?.remove();
    this.puck = null;
    this.wake?.release().catch(() => {});
    this.wake = null;
    window.speechSynthesis?.cancel();
    const { trackMap } = this.deps;
    trackMap.locked = false;
    trackMap.setNavDone(null);
    trackMap.setNavLink(null);
    document.body.classList.remove("navigating");
    $("nav").classList.add("hidden");
    trackMap.map.stop();
    trackMap.map.jumpTo(this.camera);
    this.deps.onEnd();
  }

  // --- Position ----------------------------------------------------------------------------------

  /** GPS du téléphone (extension Tauri), ou celui du navigateur hors de l'application. */
  private async watchGps(): Promise<() => void> {
    const onFix = (f: GpsFix) => this.onFix(f);
    if (!this.deps.inTauri) {
      if (!navigator.geolocation) throw new Error("Position indisponible sur cet appareil.");
      const id = navigator.geolocation.watchPosition(
        (pos) => onFix({ p: [pos.coords.longitude, pos.coords.latitude], accuracy: pos.coords.accuracy, speed: pos.coords.speed, heading: pos.coords.heading }),
        (err) => this.showAlert(`Position indisponible : ${err.message}`),
        { enableHighAccuracy: true, maximumAge: 0 },
      );
      return () => navigator.geolocation.clearWatch(id);
    }
    type Perm = { location: string };
    let perm = await invoke<Perm>("plugin:geolocation|check_permissions");
    if (perm.location !== "granted") perm = await invoke<Perm>("plugin:geolocation|request_permissions", { permissions: ["location"] });
    if (perm.location !== "granted")
      throw new Error("GoRide n'a pas accès à ta position : autorise-la dans les réglages du téléphone (GoRide → Position).");
    type Pos = { coords: { longitude: number; latitude: number; accuracy: number; speed: number | null; heading: number | null } };
    const channel = new Channel<Pos | string>();
    channel.onmessage = (msg) => {
      if (typeof msg === "string") return this.showAlert(`Position indisponible : ${msg}`);
      const c = msg.coords;
      onFix({ p: [c.longitude, c.latitude], accuracy: c.accuracy ?? 10, speed: c.speed ?? null, heading: c.heading ?? null });
    };
    // Sur Android, « timeout » sert aussi d'intervalle entre deux positions : une par seconde.
    await invoke("plugin:geolocation|watch_position", { options: { enableHighAccuracy: true, timeout: 1000, maximumAge: 0 }, channel });
    return () => {
      invoke("plugin:geolocation|clear_watch", { channelId: channel.id }).catch(() => {});
    };
  }

  /** Aperçu : la position avance toute seule le long de l'itinéraire, en accéléré. */
  private simulate(): () => void {
    const matcher = this.matcher!;
    const v = this.deps.planSpeed() / 3.6;
    let m = 0;
    let last = performance.now();
    const id = window.setInterval(() => {
      const now = performance.now();
      m = Math.min(matcher.total, m + v * PREVIEW_FACTOR * ((now - last) / 1000));
      last = now;
      this.onFix({ p: matcher.pointAt(m), accuracy: 5, speed: v, heading: matcher.bearingAt(m) });
      if (m >= matcher.total) clearInterval(id);
    }, 250);
    this.showAlert(`Aperçu du guidage · défilement accéléré ×${PREVIEW_FACTOR}`, "info");
    return () => clearInterval(id);
  }

  private onFix(fix: GpsFix) {
    if (!this.active || !this.tracker || !this.matcher) return;
    const now = performance.now();
    const first = this.lastFixAt === 0;
    const dt = first ? 1200 : Math.min(3000, Math.max(150, now - this.lastFixAt));
    this.lastFixAt = now;
    const u = this.tracker.update(fix.p, fix.accuracy);

    // Vitesse : celle du GPS, sinon déduite de l'avancée sur l'itinéraire
    let v = fix.speed !== null && fix.speed >= 0 ? fix.speed : null;
    if (v === null && this.lastM !== null && !first) v = Math.max(0, (u.m - this.lastM) / (dt / 1000));
    this.lastM = u.m;
    if (v !== null && v > 1.5) this.avg += (v - this.avg) * Math.min(1, dt / 120_000);

    // Sur l'itinéraire, le point colle au tracé et la carte regarde dans son sens : pas de tremblement
    const pos = u.onRoute ? u.point : fix.p;
    if (u.onRoute) this.course = this.matcher.bearingAt(u.m);
    else if (fix.heading !== null && (v ?? 0) > 1.5) this.course = fix.heading;
    this.movePuck(pos, first ? 0 : dt);

    if (this.following && !this.held) {
      // Plus on roule vite, plus on voit loin
      const target = v === null ? 16.4 : Math.min(17.2, Math.max(15.7, 17.3 - (v * 3.6) / 28));
      this.zoom += (target - this.zoom) * (first ? 1 : 0.15);
      const map = this.deps.trackMap.map;
      map.easeTo({
        center: pos,
        bearing: this.course,
        zoom: this.zoom,
        pitch: NAV_PITCH,
        // La position se tient dans le tiers bas de l'écran : on voit surtout ce qui arrive
        offset: [0, Math.round(map.getContainer().clientHeight * 0.2)],
        duration: dt,
        easing: (t) => t,
        essential: true,
      });
    }

    if (Math.abs(u.m - this.doneM) > 25) {
      this.doneM = u.m;
      this.deps.trackMap.setNavDone(this.matcher.sliceTo(u.m));
    }
    this.deps.trackMap.setNavLink(u.onRoute ? null : fix.p, u.point);
    this.render(u, v, fix);
    this.announce(u);
  }

  // --- Carte ---------------------------------------------------------------------------------------

  private movePuck(to: LngLat, ms: number) {
    const map = this.deps.trackMap.map;
    if (!this.puck) {
      const el = document.createElement("div");
      el.className = "nav-puck";
      el.innerHTML = `<svg viewBox="0 0 40 40" aria-hidden="true"><circle cx="20" cy="20" r="17" /><path d="M20 9l8 19-8-5-8 5z" /></svg>`;
      this.puck = new maplibregl.Marker({ element: el, rotationAlignment: "map", pitchAlignment: "map" }).setLngLat(to).addTo(map);
      this.puckAt = to;
    }
    this.puck.setRotation(this.course);
    cancelAnimationFrame(this.puckAnim);
    if (ms <= 0) {
      this.puckAt = to;
      this.puck.setLngLat(to);
      return;
    }
    // Glisse d'une position à la suivante, au même rythme que la carte
    const from = this.puckAt;
    const t0 = performance.now();
    const step = (now: number) => {
      const t = Math.min(1, (now - t0) / ms);
      this.puckAt = [from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t];
      this.puck?.setLngLat(this.puckAt);
      if (t < 1) this.puckAnim = requestAnimationFrame(step);
    };
    this.puckAnim = requestAnimationFrame(step);
  }

  private setFollowing(on: boolean) {
    this.following = on;
    $("nav-recenter").classList.toggle("hidden", on);
    if (on && this.puck) {
      const map = this.deps.trackMap.map;
      map.easeTo({
        center: this.puckAt,
        bearing: this.course,
        zoom: this.zoom,
        pitch: NAV_PITCH,
        offset: [0, Math.round(map.getContainer().clientHeight * 0.2)],
        duration: 700,
        essential: true,
      });
    }
  }

  // --- Bandeau et barre ---------------------------------------------------------------------------

  private showWaiting() {
    $("nav-banner").classList.remove("off", "done");
    $("nav-icon").innerHTML = `<span class="nav-spinner"></span>`;
    $("nav-dist").textContent = "GPS";
    $("nav-text").textContent = "Recherche de ta position…";
    $("nav-then").classList.add("hidden");
    $("nav-remaining").textContent = km(this.matcher?.total ?? 0);
    $("nav-eta").textContent = "—";
    $("nav-eta-label").textContent = "arrivée";
    $("nav-speed").textContent = "—";
    this.showAlert(null);
  }

  private showAlert(text: string | null, kind: "warn" | "info" = "warn") {
    const el = $("nav-alert");
    el.classList.toggle("hidden", !text);
    el.classList.toggle("info", kind === "info");
    if (text) el.textContent = text;
  }

  private render(u: NavUpdate, v: number | null, fix: GpsFix) {
    const banner = $("nav-banner");
    const icon = $("nav-icon");
    banner.classList.toggle("off", !u.onRoute && !u.arrived);
    banner.classList.toggle("done", u.arrived);
    icon.style.transform = "";
    let then: Maneuver | null = null;
    if (u.arrived) {
      icon.innerHTML = maneuverIcon({ kind: "arrive", atM: 0, point: u.point, angle: 0, exit: null });
      $("nav-dist").textContent = "Arrivée";
      $("nav-text").textContent = "Itinéraire terminé, bravo !";
      $("nav-stop").textContent = "Terminer";
    } else if (!u.onRoute) {
      // Flèche tournée vers l'itinéraire, par rapport au haut de la carte
      icon.innerHTML = `<svg viewBox="0 0 48 48" aria-hidden="true"><path d="M24 42V8M12 20 24 8l12 12" /></svg>`;
      icon.style.transform = `rotate(${Math.round(bearing(fix.p, u.point) - this.deps.trackMap.map.getBearing())}deg)`;
      $("nav-dist").textContent = navDistance(u.off);
      $("nav-text").textContent = this.said.has("joined") ? "Hors itinéraire : rejoins le tracé" : "Rejoins l'itinéraire";
    } else if (u.next) {
      icon.innerHTML = maneuverIcon(u.next);
      $("nav-dist").textContent = u.next.kind !== "arrive" && u.toNext < 20 ? "Maintenant" : navDistance(u.toNext);
      $("nav-text").textContent = maneuverText(u.next);
      then = u.then;
    }
    $("nav-then").classList.toggle("hidden", !then);
    if (then) $("nav-then-icon").innerHTML = maneuverIcon(then);

    $("nav-remaining").textContent = km(u.remaining);
    const hours = u.remaining / Math.max(this.avg, 1.5) / 3600;
    $("nav-eta").textContent = clock.format(new Date(Date.now() + hours * 3_600_000));
    $("nav-eta-label").textContent = u.remaining > 50 ? `arrivée · ${duration(hours)}` : "arrivée";
    $("nav-speed").textContent = v === null ? "—" : String(Math.round(v * 3.6));
    if (!this.preview) this.showAlert(fix.accuracy > 45 ? "Signal GPS faible" : null);
  }

  // --- Annonces --------------------------------------------------------------------------------------

  private announce(u: NavUpdate) {
    if (u.arrived) {
      if (!this.said.has("arrived")) {
        this.said.add("arrived");
        this.signal(3, "Tu es à l'arrivée, bravo !");
      }
      return;
    }
    if (!u.onRoute) {
      // On vient de quitter le tracé (pas au tout début, quand on ne l'a pas encore rejoint)
      if (!this.wasOff && this.said.has("joined")) this.signal(3, "Tu as quitté l'itinéraire.");
      this.wasOff = true;
      return;
    }
    this.wasOff = false;
    this.said.add("joined");
    const next = u.next;
    if (!next) return;
    const key = `${u.nextIndex}`;
    // Annonce d'approche plus tôt quand on roule vite, puis rappel au moment de tourner
    const far = this.avg > 9 ? 250 : 160;
    if (u.toNext <= far && u.toNext > 60 && !this.said.has(`${key}:far`)) {
      this.said.add(`${key}:far`);
      this.signal(1, maneuverSpeech(next, u.toNext));
    } else if (u.toNext <= 35 && !this.said.has(`${key}:now`)) {
      this.said.add(`${key}:far`).add(`${key}:now`);
      if (next.kind !== "arrive") this.signal(2, maneuverSpeech(next, null));
    }
  }

  /** Vibration, puis voix si le téléphone en a une, sinon bips. */
  private signal(beeps: number, text: string) {
    if (this.preview && document.visibilityState !== "visible") return;
    navigator.vibrate?.(beeps === 1 ? 180 : beeps === 2 ? [160, 90, 160] : [300, 120, 300]);
    if (!this.sound) return;
    const synth = window.speechSynthesis;
    if (synth && typeof SpeechSynthesisUtterance !== "undefined") {
      const say = new SpeechSynthesisUtterance(text);
      say.lang = "fr-FR";
      say.rate = 1.05;
      synth.cancel();
      synth.speak(say);
    } else this.beep(beeps);
  }

  private unlockAudio() {
    try {
      this.audio ??= new AudioContext();
      this.audio.resume().catch(() => {});
    } catch {
      this.audio = null; // pas de son sur cet appareil : il reste la vibration
    }
  }

  private beep(n: number) {
    const ctx = this.audio;
    if (!ctx) return;
    const t0 = ctx.currentTime + 0.02;
    for (let i = 0; i < n; i++) {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      const at = t0 + i * 0.24;
      osc.frequency.value = i === n - 1 ? 1175 : 880;
      gain.gain.setValueAtTime(0.0001, at);
      gain.gain.exponentialRampToValueAtTime(0.5, at + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.0001, at + 0.2);
      osc.connect(gain).connect(ctx.destination);
      osc.start(at);
      osc.stop(at + 0.22);
    }
  }

  private showSound() {
    const btn = $("nav-sound");
    btn.classList.toggle("off", !this.sound);
    btn.setAttribute("aria-pressed", String(this.sound));
    btn.title = this.sound ? "Annonces sonores activées" : "Annonces sonores coupées (la vibration reste)";
  }

  private async keepAwake() {
    try {
      this.wake = (await navigator.wakeLock?.request("screen")) ?? null;
    } catch {
      /* refusé ou indisponible : l'écran suivra le réglage du téléphone */
    }
  }
}

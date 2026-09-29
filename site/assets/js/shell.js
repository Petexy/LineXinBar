/*
 * What every page of the site shares with the shell.
 *
 * The palette and the three look settings (accent, the wallpaper's material,
 * the particles), kept per visitor; the corner clock; the button legend, which
 * names whichever control is in hand; the shell's own sounds, each spent where
 * the shell spends it; the guide, the one menu reachable from anywhere; and the
 * launch splash a press that opens something answers with.
 *
 * Input arrives as five acts — a direction, select, back, options and guide —
 * from a keyboard, a pad or a pointer, and goes to whichever screen is in front:
 * the guide while it is open, and the page's own screen otherwise.
 */
(function () {
  "use strict";

  const LXB = (window.LXB = window.LXB || {});
  const root = document.documentElement;
  const base = document.currentScript ? new URL("../..", document.currentScript.src).href : "./";
  LXB.base = base;
  // This language's pages: the root for British English, a directory of its
  // own for every other.
  LXB.home = base + (LXB.language && LXB.language.dir ? LXB.language.dir + "/" : "");

  LXB.VERSION = "0.9.3";
  LXB.REPO = "https://github.com/Petexy/LineXinBar";

  // The bar's categories and the pages behind them, left to right.
  LXB.PAGES = [
    { id: "overview", href: "overview.html", title: LXB.t("page-overview"), glyph: "logo" },
    { id: "install", href: "install.html", title: LXB.t("page-install"), glyph: "setting-install-to" },
    { id: "games", href: "games.html", title: LXB.t("page-games"), glyph: "category-games" },
    { id: "media", href: "media.html", title: LXB.t("page-media"), glyph: "category-multimedia" },
    { id: "guide", href: "guide.html", title: LXB.t("page-guide"), glyph: "pad-guide" },
    { id: "displays", href: "displays.html", title: LXB.t("page-displays"), glyph: "setting-display" },
    { id: "settings", href: "settings.html", title: LXB.t("page-settings"), glyph: "category-settings" },
    { id: "controls", href: "controls.html", title: LXB.t("page-controls"), glyph: "setting-input" },
    { id: "desktop", href: "desktop.html", title: LXB.t("page-desktop"), glyph: "category-utilities" },
    { id: "family", href: "family.html", title: LXB.t("page-family"), glyph: "file-home" },
    { id: "develop", href: "develop.html", title: LXB.t("page-develop"), glyph: "category-development" },
  ];

  LXB.glyph = function (name, size) {
    return base + "assets/glyphs/" + (size || "lg") + "/" + name + ".webp";
  };
  LXB.img = function (name, size, alt) {
    const img = document.createElement("img");
    img.className = "glyph";
    img.src = LXB.glyph(name, size);
    img.alt = alt || "";
    img.decoding = "async";
    img.draggable = false;
    return img;
  };

  // --- a site opened straight off the disk -----------------------------------

  /*
   * Firefox gives every file on the disk a storage of its own, so what one
   * page remembered — the accent, the sound, where the music had got to —
   * never reached the next, and each page opened in its own colours. On the
   * disk, then, a page hands everything it keeps over in the address of the
   * page it opens, and that page takes it into its own storage before
   * anything reads it. Served from a web address the pages share one storage
   * and nothing is handed over.
   */
  const onDisk = location.protocol === "file:";
  const HANDED = "lxb";
  function kept(storage) {
    const all = {};
    try {
      for (let i = 0; i < storage.length; i++) {
        const key = storage.key(i);
        if (key.startsWith("lxb-")) all[key.slice(4)] = storage.getItem(key);
      }
    } catch (e) { /* storage refused */ }
    return all;
  }
  if (onDisk && location.search) {
    const params = new URLSearchParams(location.search);
    if (params.has(HANDED)) {
      try {
        const bag = JSON.parse(params.get(HANDED));
        for (const [key, value] of Object.entries(bag.l || {})) localStorage.setItem("lxb-" + key, value);
        for (const [key, value] of Object.entries(bag.s || {})) sessionStorage.setItem("lxb-" + key, value);
      } catch (e) { /* a mangled address: keep what this page had */ }
      params.delete(HANDED);
      const rest = params.toString();
      try { history.replaceState(history.state, "", location.pathname + (rest ? "?" + rest : "") + location.hash); } catch (e) { /* ignore */ }
    }
  }
  // The address to open another of the site's pages at: on the disk, with
  // everything this page keeps; anywhere else, as it was.
  LXB.carry = function (href) {
    if (!onDisk) return href;
    let to;
    try { to = new URL(href, location.href); } catch (e) { return href; }
    if (to.protocol !== "file:" || !/\.html$/.test(to.pathname)) return href;
    if (LXB.music) LXB.music.remember();
    to.searchParams.set(HANDED, JSON.stringify({ l: kept(localStorage), s: kept(sessionStorage) }));
    return to.href;
  };
  // A plain link opens its page with the same hand-over. The ones the site's
  // own scripts open have been prevented and go through LXB.carry themselves.
  if (onDisk) {
    const follow = (ev) => {
      if (ev.defaultPrevented) return;
      const a = ev.target.closest && ev.target.closest("a[href]");
      if (a) a.href = LXB.carry(a.href);
    };
    document.addEventListener("click", follow);
    document.addEventListener("auxclick", follow);
  }

  // --- what the visitor chose, kept in their browser ------------------------

  const store = {
    get(key, fallback) {
      try {
        const v = localStorage.getItem("lxb-" + key);
        return v === null ? fallback : JSON.parse(v);
      } catch (e) { return fallback; }
    },
    set(key, value) {
      try { localStorage.setItem("lxb-" + key, JSON.stringify(value)); } catch (e) { /* private window */ }
    },
  };
  LXB.store = store;

  const prefs = {
    accent: store.get("accent", "Purple"),
    simple: store.get("simple", false),
    particles: store.get("particles", true),
    sound: store.get("sound", true),
    volume: store.get("volume", 0.55),
    music: store.get("music", true),
  };
  if (!LXB.PALETTES || !LXB.PALETTES[prefs.accent]) prefs.accent = "Purple";
  LXB.prefs = prefs;

  function rgb(hex) {
    const n = parseInt(hex, 16);
    return ((n >> 16) & 255) + " " + ((n >> 8) & 255) + " " + (n & 255);
  }
  function applyPalette(name) {
    const p = (LXB.PALETTES || {})[name];
    if (!p) return;
    const s = root.style;
    s.setProperty("--accent", "#" + p.accent);
    s.setProperty("--accent-rgb", rgb(p.accent));
    s.setProperty("--accent-soft", "#" + p.accent_soft);
    s.setProperty("--accent-soft-rgb", rgb(p.accent_soft));
    s.setProperty("--accent-deep", "#" + p.accent_deep);
    s.setProperty("--accent-deep-rgb", rgb(p.accent_deep));
    s.setProperty("--glass-rgb", rgb(p.glass));
    s.setProperty("--glass-raised-rgb", rgb(p.glass_raised));
    s.setProperty("--text-soft", "#" + p.text_soft);
    s.setProperty("--text-soft-rgb", rgb(p.text_soft));
    s.setProperty("--sky-top", "#" + p.sky[0]);
    s.setProperty("--sky-bottom", "#" + p.sky[1]);
    s.setProperty("--glow", "#" + p.glow);
    const meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.content = "#" + p.sky[1];
  }
  applyPalette(prefs.accent);

  LXB.setAccent = function (name, animate) {
    if (!LXB.PALETTES[name]) return;
    prefs.accent = name;
    store.set("accent", name);
    applyPalette(name);
    if (LXB.wallpaper) LXB.wallpaper.setAccent(name, animate !== false);
    document.dispatchEvent(new CustomEvent("lxb:accent", { detail: name }));
  };
  LXB.setLook = function (look) {
    if ("simple" in look) { prefs.simple = look.simple; store.set("simple", look.simple); }
    if ("particles" in look) { prefs.particles = look.particles; store.set("particles", look.particles); }
    if (LXB.wallpaper) LXB.wallpaper.setStyle({ simple: prefs.simple, particles: prefs.particles });
    document.dispatchEvent(new CustomEvent("lxb:look"));
  };

  // --- the wallpaper ---------------------------------------------------------

  function startWallpaper() {
    let canvas = document.getElementById("wallpaper");
    if (!canvas) {
      canvas = document.createElement("canvas");
      canvas.id = "wallpaper";
      canvas.setAttribute("aria-hidden", "true");
      document.body.prepend(canvas);
    }
    if (!LXB.Wallpaper) { root.classList.add("no-wallpaper"); return; }
    const bar = document.body.dataset.screen === "bar";
    try {
      LXB.wallpaper = new LXB.Wallpaper(canvas, { glass: bar, fps: bar ? 0 : 30 });
      LXB.wallpaper.setAccent(prefs.accent, false);
      LXB.wallpaper.setStyle({ simple: prefs.simple, particles: prefs.particles });
      requestAnimationFrame(() => requestAnimationFrame(() => canvas.classList.add("ready")));
    } catch (e) {
      // No WebGL2: the page keeps the theme's own gradient behind it.
      canvas.remove();
      LXB.wallpaper = null;
      root.classList.add("no-wallpaper");
      console.info("LineXinBar site: drawing without the wallpaper —", e.message);
    }
  }

  // --- the corner ------------------------------------------------------------

  const clockDate = new Intl.DateTimeFormat(LXB.lang, { day: "numeric", month: "numeric" });
  const clockTime = new Intl.DateTimeFormat(LXB.lang, { hour: "2-digit", minute: "2-digit" });
  const longDate = new Intl.DateTimeFormat(LXB.lang, { weekday: "short", day: "numeric", month: "short" });
  function corner() {
    let el = document.querySelector(".corner");
    if (!el) {
      el = document.createElement("div");
      el.className = "corner";
      el.setAttribute("aria-hidden", "true");
      el.append(LXB.img("signal-strong", "sm"), document.createElement("span"));
      (LXB.screenEl || document.body).append(el);
    }
    const tick = () => {
      const now = new Date();
      el.lastElementChild.textContent = clockDate.format(now) + " " + clockTime.format(now);
      const t = document.querySelector(".guide-time");
      if (t) {
        t.textContent = clockTime.format(now);
        document.querySelector(".guide-date").textContent = longDate.format(now);
      }
    };
    tick();
    LXB.tick = tick;
    setTimeout(() => { tick(); setInterval(tick, 60000); }, (60 - new Date().getSeconds()) * 1000 + 50);
  }

  // --- sound -----------------------------------------------------------------

  /*
   * The shell's own clips. Two screens with voices of their own — the bar
   * answers a move with `press` and a press with `press-selected`; the guide
   * with `press-guide` and `press-guide-selected` — plus `press-back` for
   * leaving a column, `guide-open` for the guide's own button and nothing
   * else, and `app-launch` for a tile that opens something.
   *
   * Every clip is decoded as the page loads, from sounds.js, because every
   * press on this site opens a new page and a clip fetched at its first press
   * is a press that makes no sound. A browser lets a page make a sound only
   * once it has been touched, so the audio device is woken by the first key,
   * click or tap of any kind — before the one that makes the first sound has
   * finished arriving.
   */
  const CLIPS = ["press", "press-selected", "press-back", "guide-open", "press-guide", "press-guide-selected", "app-launch"];
  const sound = {
    ctx: null,
    gain: null,
    decoder: null,
    buffers: {},
    lead: {},
    last: {},
    waiting: null,
    // The clips, decoded as the page loads by a context that plays nothing —
    // one a browser has no objection to before the page has been touched.
    load() {
      if (this.loading) return;
      this.loading = true;
      const decodeAll = () => {
        const OAC = window.OfflineAudioContext || window.webkitOfflineAudioContext;
        try { this.decoder = OAC ? new OAC(1, 1, 48000) : null; } catch (e) { this.decoder = null; }
        if (this.decoder || this.ctx) this.decodeAll();
      };
      if (window.LXB_SOUNDS) decodeAll();
      else {
        const script = document.createElement("script");
        script.src = base + "assets/js/sounds.js";
        script.onload = decodeAll;
        document.head.append(script);
      }
    },
    decodeAll() {
      const data = window.LXB_SOUNDS || {};
      CLIPS.forEach((name) => data[name] && !this.buffers[name] && this.decode(name, data[name]));
    },
    prepare() {
      if (this.ctx) return;
      const AC = window.AudioContext || window.webkitAudioContext;
      if (!AC) return;
      try {
        this.ctx = new AC({ latencyHint: "interactive" });
      } catch (e) {
        try { this.ctx = new AC(); } catch (e2) { return; }
      }
      this.gain = this.ctx.createGain();
      this.gain.gain.value = curve(prefs.volume);
      this.gain.connect(this.ctx.destination);
      // A browser with nothing to decode offline decodes here instead.
      if (!this.decoder && window.LXB_SOUNDS) this.decodeAll();
    },
    decode(name, b64) {
      const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
      (this.decoder || this.ctx).decodeAudioData(bytes.buffer, (buf) => {
        this.buffers[name] = buf;
        // An MP3 opens on its encoder's padding: start where the sound does.
        const data = buf.getChannelData(0);
        let i = 0;
        while (i < data.length && Math.abs(data[i]) < 0.002) i++;
        this.lead[name] = Math.max(0, i - 24) / buf.sampleRate;
        const w = this.waiting;
        if (w && w.name === name && performance.now() - w.at < 250) this.play(name);
      }, () => {});
    },
    // Called from inside the first input of any kind, which is when a browser
    // lets the device start.
    wake() {
      if (!prefs.sound) return;
      this.prepare();
      if (this.ctx && this.ctx.state === "suspended") {
        const p = this.ctx.resume();
        if (p && p.catch) p.catch(() => {});
      }
    },
    play(name) {
      if (!prefs.sound) return;
      this.wake();
      if (!this.ctx) return;
      // No clip is laid on top of a copy of itself: two in phase are one
      // clip twice as loud, which a spun wheel otherwise makes.
      const now = performance.now();
      if (now - (this.last[name] || 0) < 60) return;
      this.last[name] = now;
      const buf = this.buffers[name];
      if (!buf) { this.waiting = { name, at: now }; return; }
      const src = this.ctx.createBufferSource();
      src.buffer = buf;
      src.connect(this.gain);
      src.start(0, this.lead[name] || 0);
    },
    setVolume(v) {
      prefs.volume = v;
      store.set("volume", v);
      if (this.gain) this.gain.gain.setTargetAtTime(curve(v), this.ctx.currentTime, 0.02);
      music.level();
    },
  };
  // The shell's perceptual curve: a bar halfway along is half as loud to the ear.
  function curve(v) { return Math.pow(Math.max(0, Math.min(1, v)), 2); }
  LXB.sound = sound;
  // Kept for the pages' scripts, which wake the device on a press of their own.
  sound.unlock = sound.wake;

  /*
   * The start screen's music, on by default. In the shell it plays whenever no
   * application is open, and nothing on this site is one — every page is a
   * column of the bar stepped into — so it plays throughout a visit, carried
   * from page to page at the point it had reached rather than starting over.
   * A browser will not start it before the visitor has touched the site, so a
   * refused start waits for the first key, click or tap.
   */
  const MUSIC_LEVEL = 0.7;
  const FADE_FROM = 0.15;
  const music = {
    el: null,
    fade: 0,
    playing: false,
    starting: false,
    start() {
      if (!prefs.music || !prefs.sound || this.starting) return;
      if (!this.el) {
        let at = 0;
        try { at = Number(sessionStorage.getItem("lxb-music-at")) || 0; } catch (e) { /* ignore */ }
        this.el = new Audio(base + "assets/sounds/start-bg-music.mp3" + (at > 0 ? "#t=" + at.toFixed(2) : ""));
        this.el.loop = true;
        this.el.preload = "auto";
        this.el.addEventListener("timeupdate", () => this.remember());
        // Whether it is really playing is the element's to say: a browser
        // may pause it on its own, and a start that has been paused has to
        // be tried again at the next input.
        this.el.addEventListener("pause", () => { this.playing = false; });
      }
      // Never started silent. A browser lets a start at no volume through
      // without asking, then stops it the moment it becomes audible and says
      // nothing — which left the music waiting for somebody to switch it off
      // and on again. Started quiet but audible, a refusal is a refusal.
      this.fade = FADE_FROM;
      this.level();
      this.starting = true;
      const p = this.el.play();
      if (p && p.then) {
        p.then(() => { this.starting = false; this.rise(); }, () => { this.starting = false; this.playing = false; });
      } else {
        this.starting = false;
        this.rise();
      }
    },
    // In over half a second, so a page arriving mid-phrase does not cut in.
    rise() {
      this.playing = true;
      const began = performance.now();
      const step = (now) => {
        if (!this.playing) return;
        this.fade = FADE_FROM + (1 - FADE_FROM) * clamp01((now - began) / 500);
        this.level();
        if (this.fade < 1) requestAnimationFrame(step);
      };
      requestAnimationFrame(step);
    },
    level() {
      if (this.el) this.el.volume = Math.min(1, Math.max(0, curve(prefs.volume) * MUSIC_LEVEL * this.fade));
    },
    stop() {
      this.playing = false;
      if (this.el) this.el.pause();
      this.remember();
    },
    remember() {
      if (!this.el) return;
      try { sessionStorage.setItem("lxb-music-at", String(this.el.currentTime || 0)); } catch (e) { /* ignore */ }
    },
    toggle(on) {
      prefs.music = on;
      store.set("music", on);
      if (on) this.start();
      else this.stop();
    },
    // A start the browser refused, tried again from inside an input.
    retry() {
      if (prefs.music && prefs.sound && !this.starting && (!this.playing || (this.el && this.el.paused))) this.start();
    },
  };
  LXB.music = music;
  window.addEventListener("pagehide", () => music.remember());

  // The first key, click or tap wakes both, and every one after that until
  // they are awake: a browser counts only some inputs, and not the pad's.
  ["pointerdown", "keydown", "touchend", "mousedown"].forEach((type) => window.addEventListener(type, () => {
    sound.wake();
    music.retry();
  }, { capture: true, passive: true }));

  // --- input -----------------------------------------------------------------

  let device = "keys";
  function setDevice(kind) {
    if (device === kind) return;
    device = kind;
    root.dataset.input = kind;
    legend();
  }
  LXB.device = () => device;

  // What is in front: the language menu while it is up, then the guide, and
  // the page's screen otherwise.
  function front() {
    return languages.open ? languages : guide.open ? guide : LXB.screen;
  }
  function dispatch(act, from) {
    sound.unlock();
    const screen = front();
    if (act === "guide" && !guide.open && !languages.open) {
      guide.show(true);
      return true;
    }
    if (screen && typeof screen[act] === "function") {
      return screen[act](from) !== false;
    }
    return false;
  }
  LXB.dispatch = dispatch;

  document.addEventListener("keydown", (e) => {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const t = e.target;
    const typing = t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));
    if (typing && e.key !== "Escape") return;
    // The page stands still in its card while the guide is over it.
    if ((guide.open || languages.open) && /^(PageUp|PageDown|End)$/.test(e.key)) { e.preventDefault(); return; }
    // A screen that takes typing — the bar's search — has the letters before
    // any of them can be a shortcut.
    if (!guide.open && !languages.open && LXB.screen && LXB.screen.type && (e.key.length === 1 || e.key === "Backspace")) {
      if (LXB.screen.type(e.key)) {
        setDevice("keys");
        e.preventDefault();
        return;
      }
    }
    let act = null;
    switch (e.key) {
      case "ArrowUp": act = "up"; break;
      case "ArrowDown": act = "down"; break;
      case "ArrowLeft": act = "left"; break;
      case "ArrowRight": act = "right"; break;
      case "Enter": act = "select"; break;
      case " ": act = document.body.dataset.screen === "bar" || guide.open ? "select" : null; break;
      case "Escape": case "Backspace": act = "back"; break;
      case "g": case "G": case "Home": case "ContextMenu": act = "guide"; break;
      case "F10": act = "options"; break;
      default: return;
    }
    if (!act) return;
    // A link or button with the keyboard's focus answers Enter itself.
    // Only the start screen's rows hand Enter to the bar, which presses what is lit.
    if (act === "select" && !guide.open && !languages.open && t && t.closest && t.closest("a[href], button, summary")
        && !(document.body.dataset.screen === "bar" && t.closest("[data-lxb-row]"))) return;
    setDevice("keys");
    if (dispatch(act, "keys")) e.preventDefault();
  });

  document.addEventListener("pointerdown", (e) => {
    sound.unlock();
    if (e.pointerType === "mouse" || e.pointerType === "pen") setDevice("pointer");
    else setDevice("touch");
  }, { passive: true });

  // Pads are read by polling, only while one is connected. Buttons are read
  // by position — the bottom of the cluster is select on every layout.
  const pad = {
    prev: {},
    held: {},
    polling: false,
    start() {
      if (this.polling) return;
      this.polling = true;
      const loop = (now) => {
        const pads = navigator.getGamepads ? Array.from(navigator.getGamepads()).filter(Boolean) : [];
        if (!pads.length) { this.polling = false; return; }
        this.read(pads, now);
        requestAnimationFrame(loop);
      };
      requestAnimationFrame(loop);
    },
    read(pads, now) {
      const state = {};
      for (const p of pads) {
        const b = (i) => p.buttons[i] && p.buttons[i].pressed;
        const ax = p.axes[0] || 0, ay = p.axes[1] || 0;
        state.up = state.up || b(12) || ay < -0.55;
        state.down = state.down || b(13) || ay > 0.55;
        state.left = state.left || b(14) || ax < -0.55;
        state.right = state.right || b(15) || ax > 0.55;
        state.select = state.select || b(0);
        state.back = state.back || b(1);
        state.options = state.options || b(3);
        state.guide = state.guide || b(16) || b(9);
        state.prev = state.prev || b(4);
        state.next = state.next || b(5);
      }
      for (const act in state) {
        const on = state[act];
        const was = this.prev[act];
        if (on && !was) {
          setDevice("pad");
          this.held[act] = now + 380;
          dispatch(act, "pad");
        } else if (on && was && /^(up|down|left|right)$/.test(act) && now >= this.held[act]) {
          this.held[act] = now + 95;
          dispatch(act, "pad");
        }
        this.prev[act] = on;
      }
    },
  };
  window.addEventListener("gamepadconnected", () => { setDevice("pad"); pad.start(); });
  if (navigator.getGamepads && Array.from(navigator.getGamepads()).some(Boolean)) pad.start();

  // --- the legend --------------------------------------------------------------

  const LEGEND = {
    select: { label: "select", pad: "pad-south", keys: "key-enter" },
    back: { label: "back", pad: "pad-east", keys: "key-escape" },
    options: { label: "options", pad: "pad-north", keys: "mouse-right" },
    guide: { label: "guide", pad: "pad-guide", keys: "key-g" },
  };
  /*
   * The acts the screen in front can answer, named for the control in hand.
   * Guide is always there: it is the way out from everywhere.
   */
  function legend() {
    let el = document.querySelector(".legend");
    if (!el) {
      el = document.createElement("div");
      el.className = "legend";
      el.setAttribute("aria-label", LXB.t("buttons"));
      document.body.append(el);
    }
    const screen = front();
    // Guide is always offered — except while the bar is being typed into,
    // where its key on a keyboard is the one that gives the search up.
    const hideGuide = guide.open || languages.open || (screen && screen.hidesGuide && screen.hidesGuide());
    const acts = (screen && screen.legend ? screen.legend() : ["select"]).concat(hideGuide ? [] : ["guide"]);
    const kind = device === "pad" ? "pad" : "keys";
    el.textContent = "";
    for (const act of acts) {
      const spec = LEGEND[act];
      if (!spec) continue;
      const b = document.createElement("button");
      b.type = "button";
      const label = (screen && screen.label && screen.label(act)) || LXB.t(spec.label);
      const glyph = (screen && screen.glyph && screen.glyph(act, kind)) || spec[kind];
      b.append(document.createTextNode(label), LXB.img(glyph, "sm", ""));
      b.addEventListener("click", () => dispatch(act, "pointer"));
      el.append(b);
    }
  }
  LXB.legend = legend;

  // --- the guide -----------------------------------------------------------------

  /*
   * The guide, as the shell draws it — `build_guide` in ui.rs, laid out by
   * lxb_protocol::overview. A column of glass slides in from the left while the
   * screen flies back into a card beside it, framed and titled there, and the
   * wallpaper round the card softens. Closing takes the column away at once and
   * flies the card back up to fill the display.
   *
   * The column is one pane of the shell's sidebar glass — shallower and clearer
   * than a dialog's, with a faint bow across its face and two pools of the
   * accent under it — drawn by the wallpaper's WebGL pass. The chips and words
   * on it are the page's own, measured in the shell's reference pixels, and the
   * light on the chosen one is a single capsule that glides between them.
   */
  const GUIDE = {
    INSET: 14, RADIUS: 30,
    DEPTH: 15, FROST: 0.46, GLOSS: 0.66, CURVE: 1, STAIN: 0.38,
    HEADER_LIGHT: 0.075, FOOT_LIGHT: 0.04,
    SLIDE: 0.28, FLIGHT: 0.3, CARD_ARRIVAL: 0.3 + 2 / 60, CARD_FADE: 0.14,
    CARD_HEIGHT: 0.54, CARD_RADIUS: 18, LIGHT_RATE: 21, TILE_RADIUS: 0.30,
  };
  const clamp01 = (t) => Math.min(Math.max(t, 0), 1);
  // `ui::ease`, a cubic in and out.
  const ease = (t) => { t = clamp01(t); return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2; };
  const smoothstep = (t) => { t = clamp01(t); return t * t * (3 - 2 * t); };
  const guideScale = (H) => Math.min(Math.max(H / 1080, 0.6), 2.5);
  // `overview::spring`, which the light rides as the shell's does.
  function spring(position, velocity, target, rate, dt) {
    dt = Math.min(Math.max(dt, 0), 0.1);
    const offset = position - target;
    const c = velocity + rate * offset;
    const decay = Math.exp(-rate * dt);
    return [target + (offset + c * dt) * decay, (velocity - c * rate * dt) * decay];
  }
  // `overview::sidebar_width` — and on a phone, never so narrow that the
  // column cannot say what is on it.
  function sidebarWidth(W) {
    const w = Math.min(Math.max(W * 0.22, 280), 520);
    return W < 640 ? Math.min(w, W - 72) : Math.min(w, W * 0.5);
  }
  // `overview::card_slots` for the one card there is, with the display fitted
  // into it at its own shape (`overview::fit`). A phone has no room beside the
  // column, so there the card keeps its size and hangs off the right edge,
  // peeking out past the column the way a cut-off card does in the shell.
  function cardRect(W, H) {
    const margin = H * 0.05;
    const left = sidebarWidth(W) + margin;
    const room = Math.max(0, W - margin - left);
    if (room >= H * 0.3) {
      let h = H * GUIDE.CARD_HEIGHT, w = (h * 16) / 9;
      if (w > room) { w = room; h = (w * 9) / 16; }
      const x = left + (room - w) / 2, y = (H - h) / 2;
      const k = Math.min(w / W, h / H);
      return { x: x + (w - W * k) / 2, y: y + (h - H * k) / 2, w: W * k, h: H * k };
    }
    const w = W * GUIDE.CARD_HEIGHT, h = H * GUIDE.CARD_HEIGHT;
    return { x: left + room / 2 - w / 2, y: (H - h) / 2, w, h };
  }
  const lerpRect = (a, b, t) => ({ x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t, w: a.w + (b.w - a.w) * t, h: a.h + (b.h - a.h) * t });

  const guide = {
    open: false,
    stops: [],
    at: 0,
    tileAt: 0,
    swatchAt: 0,
    pane: "menu",
    home: 0,
    build() {
      const here = document.body.dataset.page || "start";
      const page = LXB.PAGES.find((p) => p.id === here);
      this.here = here;
      this.title = page ? page.title : here === "start" ? LXB.t("start-screen") : document.title.split(" — ")[0];

      const scrim = document.createElement("div");
      scrim.className = "guide-scrim";
      scrim.addEventListener("click", () => this.show(false));
      scrim.addEventListener("wheel", (e) => e.preventDefault(), { passive: false });

      // The card's frame and title. The screen itself is flown into it.
      const card = document.createElement("div");
      card.className = "guide-card";
      card.setAttribute("aria-hidden", "true");
      card.innerHTML = '<div class="guide-card-frame"></div><div class="guide-card-title"></div>';
      card.querySelector(".guide-card-title").textContent = this.title;
      card.addEventListener("click", () => { sound.play("press-guide-selected"); this.show(false); });
      card.addEventListener("pointermove", (e) => { if (e.pointerType === "mouse" && this.pane !== "card") this.focusCard(true, true); });
      card.addEventListener("wheel", (e) => e.preventDefault(), { passive: false });

      const el = document.createElement("aside");
      el.className = "guide";
      el.id = "guide";
      el.tabIndex = -1;
      el.setAttribute("aria-label", LXB.t("guide"));
      el.setAttribute("aria-hidden", "true");
      el.innerHTML =
        '<div class="guide-head"><div class="guide-time"></div><div class="guide-date"></div>' +
        '<div class="guide-where"></div></div>' +
        '<div class="guide-entries"><div class="guide-light" aria-hidden="true"></div>' +
        '<div class="guide-tiles entry" role="group"></div>' +
        '<label class="guide-bar entry chip"><span class="visually-hidden"></span></label>' +
        '<div class="guide-swatches entry" role="group"></div>' +
        '<hr class="guide-rule entry"><div class="guide-rows" data-top></div>' +
        '<hr class="guide-rule entry"><nav class="guide-rows" data-pages></nav>' +
        '<div class="guide-foot entry"></div></div>';
      // What is in front: the page, as an application would be — or, on the
      // start screen, nothing.
      el.querySelector(".guide-where").textContent = here === "start" ? LXB.t("nothing-running") : this.title;
      el.querySelector(".guide-tiles").setAttribute("aria-label", LXB.t("quick-settings"));
      el.querySelector(".guide-bar .visually-hidden").textContent = LXB.t("volume");
      el.querySelector(".guide-swatches").setAttribute("aria-label", LXB.t("accent"));
      el.querySelector("[data-pages]").setAttribute("aria-label", LXB.t("pages"));
      this.entries = el.querySelector(".guide-entries");
      this.light = el.querySelector(".guide-light");

      const tiles = el.querySelector(".guide-tiles");
      const tile = (glyph, label, pressed, act) => {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "guide-tile chip";
        b.title = label;
        b.setAttribute("aria-label", label);
        b.setAttribute("aria-pressed", String(pressed()));
        b.append(LXB.img(glyph, "sm"));
        (this.tileStates = this.tileStates || []).push({ b, pressed });
        b.addEventListener("click", () => { act(); b.setAttribute("aria-pressed", String(pressed())); sound.play("press-guide-selected"); });
        tiles.append(b);
        return b;
      };
      const soundGlyph = () => this.soundTile.querySelector("img").src = LXB.glyph(prefs.sound ? "volume" : "volume-muted", "sm");
      this.soundTile = tile("volume", LXB.t("sounds"), () => prefs.sound, () => {
        prefs.sound = !prefs.sound;
        store.set("sound", prefs.sound);
        soundGlyph();
        if (!prefs.sound) music.stop();
        else music.retry();
        this.sync();
      });
      soundGlyph();
      this.soundGlyph = soundGlyph;
      this.musicTile = tile("media-play", LXB.t("music"), () => prefs.music && prefs.sound, () => {
        if (!prefs.sound) { prefs.sound = true; store.set("sound", true); soundGlyph(); }
        music.toggle(!(prefs.music && music.playing));
        this.sync();
      });
      tile("setting-particles", LXB.t("particles"), () => prefs.particles, () => LXB.setLook({ particles: !prefs.particles }));
      tile("setting-wallpaper", LXB.t("wallpaper-tile"), () => !prefs.simple, () => LXB.setLook({ simple: !prefs.simple }));

      const bar = el.querySelector(".guide-bar");
      bar.prepend(LXB.img("volume", "sm"));
      const range = document.createElement("input");
      range.type = "range";
      range.min = "0";
      range.max = "1";
      range.step = "0.05";
      range.value = String(prefs.volume);
      range.addEventListener("input", () => { sound.setVolume(Number(range.value)); this.sync(); });
      range.addEventListener("change", () => sound.play("press-guide"));
      bar.append(range);
      this.range = range;

      const swatches = el.querySelector(".guide-swatches");
      for (const name of Object.keys(LXB.PALETTES)) {
        const b = document.createElement("button");
        b.type = "button";
        b.title = LXB.t(name);
        b.setAttribute("aria-label", LXB.t(name));
        b.style.setProperty("--swatch", "#" + LXB.PALETTES[name].accent);
        b.setAttribute("aria-pressed", String(name === prefs.accent));
        b.addEventListener("click", () => { LXB.setAccent(name); this.swatchAt = Object.keys(LXB.PALETTES).indexOf(name); this.sync(); sound.play("press-guide-selected"); });
        swatches.append(b);
      }

      const row = (list, title, href, cls) => {
        const a = document.createElement(href ? "a" : "button");
        a.className = "guide-row entry chip" + (cls ? " " + cls : "");
        if (href) a.href = href; else a.type = "button";
        const label = document.createElement("span");
        label.textContent = title;
        a.append(label);
        list.append(a);
        return a;
      };
      const top = el.querySelector("[data-top]");
      const resume = row(top, LXB.t("resume"), null);
      resume.addEventListener("click", () => { sound.play("press-guide-selected"); this.show(false); });
      this.resumeRow = resume;
      // Resume and Start screen would do the same thing on the start screen,
      // so there it is left out, as the shell leaves it out.
      if (here !== "start") row(top, LXB.t("start-screen"), LXB.home + "index.html");
      const language = row(top, LXB.t("language") + " · " + LXB.language.name, null);
      language.addEventListener("click", () => { sound.play("press-guide-selected"); languages.show(true); });
      const pages = el.querySelector("[data-pages]");
      for (const p of LXB.PAGES) row(pages, p.title, LXB.home + p.href, p.id === here ? "here" : "");
      const repo = row(pages, LXB.t("github"), LXB.REPO);
      repo.target = "_blank";
      repo.rel = "noopener";
      el.querySelectorAll(".guide-row[href]").forEach((a) => a.addEventListener("click", (ev) => {
        if (a === repo) { sound.play("press-guide-selected"); return; }
        ev.preventDefault();
        sound.play("press-guide-selected");
        const p = LXB.PAGES.find((q) => a.href === LXB.home + q.href);
        if (p && p.id === here) { this.show(false); return; }
        LXB.launch(a.href, p ? { glyph: p.glyph, name: p.title, quiet: true } : { glyph: "logo", name: LXB.t("start-screen"), quiet: true });
      }));

      el.querySelector(".guide-foot").innerHTML = LXB.t("licence", {
        version: LXB.VERSION,
        licence: '<a href="' + LXB.REPO + '/blob/master/LICENSE" target="_blank" rel="noopener">GPL-3.0</a>',
      });

      // The entries arrive one after another, as the shell's do.
      el.querySelectorAll(".entry").forEach((e, i) => e.style.setProperty("--i", String(i)));

      document.body.append(scrim, card, el);
      this.el = el;
      this.scrim = scrim;
      this.card = card;
      this.stops = [tiles, bar, swatches].concat(Array.from(el.querySelectorAll(".guide-row")));
      this.swatchAt = Math.max(0, Object.keys(LXB.PALETTES).indexOf(prefs.accent));

      // The pointer lights what it is over, and says nothing doing it: a swept
      // mouse is not a stream of moves.
      this.stops.forEach((stop, i) => stop.addEventListener("pointermove", (e) => {
        if (e.pointerType !== "mouse") return;
        let changed = this.at !== i || this.pane !== "menu";
        const child = e.target.closest(".guide-tile, .guide-swatches button");
        if (child && stop.classList.contains("guide-tiles")) { changed = changed || this.tileAt !== Array.from(stop.children).indexOf(child); this.tileAt = Array.from(stop.children).indexOf(child); }
        if (child && stop.classList.contains("guide-swatches")) { changed = changed || this.swatchAt !== Array.from(stop.children).indexOf(child); this.swatchAt = Array.from(stop.children).indexOf(child); }
        if (!changed) return;
        this.at = i;
        this.pane = "menu";
        this.lit();
      }));
      window.addEventListener("resize", () => this.open && this.kick());
    },
    sync() {
      const names = Object.keys(LXB.PALETTES);
      this.el.querySelectorAll(".guide-swatches button").forEach((b, i) => b.setAttribute("aria-pressed", String(names[i] === prefs.accent)));
      this.range.value = String(prefs.volume);
      this.range.style.setProperty("--level", prefs.volume * 100 + "%");
      (this.tileStates || []).forEach((t) => t.b.setAttribute("aria-pressed", String(t.pressed())));
      this.soundGlyph();
    },

    show(on, quiet) {
      if (!this.el) this.build();
      if (on === this.open) return;
      this.open = on;
      root.classList.toggle("guide-open", on);
      this.el.setAttribute("aria-hidden", String(!on));
      const surface = LXB.screenEl;
      if (on) {
        this.sync();
        if (LXB.tick) LXB.tick();
        this.openedAt = performance.now();
        this.pane = "menu";
        this.at = this.stops.indexOf(this.resumeRow);
        this.lightFrom = null;
        this.el.classList.remove("in");
        void this.el.offsetWidth;
        this.el.classList.add("in");
        this.entries.scrollTop = 0;
        this.lit(true);
        this.returnFocus = document.activeElement;
        if (surface) surface.inert = true;
        if (!quiet) sound.play("guide-open");
        this.el.focus({ preventScroll: true });
      } else {
        this.el.classList.remove("in");
        // The keyboard must not stay on a button in a menu that has gone, or
        // the next Enter presses it rather than whatever is lit behind it.
        if (this.el.contains(document.activeElement)) document.activeElement.blur();
        if (surface) surface.inert = false;
        const back = this.returnFocus;
        if (back && back !== document.body && back.isConnected && back.focus) back.focus({ preventScroll: true });
      }
      if (quiet === "instant") this.home = on ? 1 : 0;
      this.kick();
      legend();
    },

    // --- one frame of the flight, the column and the light -------------------

    kick() {
      const wp = LXB.wallpaper;
      if (wp && wp.hooks) {
        if (!this.hooked) {
          this.hooked = (dt, now) => this.frame(dt, now);
          wp.hooks.push(this.hooked);
        }
        if (wp.hurry) wp.hurry(0.8);
        else wp.kick();
      } else if (!this.ticking) {
        // No wallpaper to ride on: the guide keeps its own frames.
        this.ticking = true;
        let last = 0;
        const tick = (now) => {
          const dt = last ? Math.min((now - last) / 1000, 0.1) : 0.016;
          last = now;
          if (this.frame(dt, now)) requestAnimationFrame(tick);
          else this.ticking = false;
        };
        requestAnimationFrame(tick);
      }
    },
    // Returns whether anything is still moving, so a still guide costs no frames.
    frame(dt, now) {
      const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      const W = root.clientWidth, H = root.clientHeight;
      const gs = guideScale(H);
      const target = this.open ? 1 : 0;
      this.home = reduced ? target : this.home + Math.sign(target - this.home) * Math.min(Math.abs(target - this.home), dt / GUIDE.FLIGHT);
      const home = smoothstep(this.home);
      const age = this.open ? (performance.now() - this.openedAt) / 1000 : 0;
      const slide = this.open ? (reduced ? 1 : ease(age / GUIDE.SLIDE)) : 0;

      // The column, floating clear of the display's edges.
      const sw = sidebarWidth(W);
      const inset = GUIDE.INSET * gs;
      const panel = { x: inset + (slide - 1) * sw, y: inset, w: sw - inset * 2, h: H - inset * 2 };
      const el = this.el;
      el.style.setProperty("--gs", gs + "px");
      el.style.left = inset + "px";
      el.style.top = inset + "px";
      el.style.width = panel.w + "px";
      el.style.height = panel.h + "px";
      el.style.transform = "translateX(" + (panel.x - inset) + "px)";
      el.style.opacity = String(slide);
      el.style.visibility = this.open ? "visible" : "hidden";

      // The screen, wherever it has got to between the display and its card.
      const slot = cardRect(W, H);
      const rect = lerpRect({ x: 0, y: 0, w: W, h: H }, slot, home);
      const radius = GUIDE.CARD_RADIUS * gs * home;
      this.place(rect, radius, home, this.open ? panel.x + panel.w : -1, W, H);

      const wp = LXB.wallpaper;
      if (wp && wp.overlay) {
        const th = wp.theme;
        wp.card = home > 0 ? { x: rect.x, y: rect.y, w: rect.w, h: rect.h, radius } : null;
        wp.setSoftenNow(home);
        if (this.open && slide > 0) {
          // `sidebar_surface`: two quiet pools of the accent wholly inside the
          // column, under one pane of its glass, which bends them with the rest.
          const headerH = Math.min(300 * gs, panel.h * 0.36), footH = Math.min(360 * gs, panel.h * 0.38);
          wp.overlay.glows = [
            { x: panel.x + panel.w * 0.5, y: panel.y + 2 * gs + headerH / 2, w: panel.w * 0.88, h: headerH, color: [...th.accent_soft, GUIDE.HEADER_LIGHT * slide] },
            { x: panel.x + panel.w * 0.5, y: panel.y + panel.h - 2 * gs - footH / 2, w: panel.w * 0.8, h: footH, color: [...th.accent, GUIDE.FOOT_LIGHT * slide] },
          ];
          wp.overlay.panes = [{
            x: panel.x, y: panel.y, w: panel.w, h: panel.h,
            color: [...th.glass, GUIDE.STAIN], radius: GUIDE.RADIUS * gs, power: 2,
            slab: GUIDE.DEPTH * gs, frost: GUIDE.FROST, gloss: GUIDE.GLOSS, curve: GUIDE.CURVE, fade: slide,
          }];
        } else {
          wp.overlay.glows = [];
          wp.overlay.panes = [];
        }
      }

      // The card's frame and title, which hold back until it has landed.
      const cardFade = this.open ? (reduced ? 1 : ease((age - GUIDE.CARD_ARRIVAL) / GUIDE.CARD_FADE)) : 0;
      const c = this.card;
      c.style.setProperty("--gs", gs + "px");
      c.style.left = slot.x + "px";
      c.style.top = slot.y + "px";
      c.style.width = slot.w + "px";
      c.style.height = slot.h + "px";
      c.style.opacity = String(cardFade);
      c.style.visibility = cardFade > 0.004 ? "visible" : "hidden";
      c.classList.toggle("lit", this.pane === "card");
      // A card lying partly under the column — on a phone — is framed and
      // named only where it shows.
      const under = Math.max(0, panel.x + panel.w - slot.x);
      c.style.clipPath = under > 0 ? "inset(-60px -60px -60px " + under + "px)" : "";
      c.style.setProperty("--under", under > 0 ? under + 10 * gs + "px" : "0px");

      if (this.open) this.glide(dt, reduced);

      const moving = this.home !== target || (this.open && (slide < 1 || cardFade < 1 || this.lightMoving));
      if (!this.open && this.home === 0 && LXB.wallpaper && LXB.wallpaper.hooks && this.hooked) {
        const hooks = LXB.wallpaper.hooks;
        hooks.splice(hooks.indexOf(this.hooked), 1);
        this.hooked = null;
      }
      if (moving && LXB.wallpaper && LXB.wallpaper.hurry) LXB.wallpaper.hurry(0.1);
      return moving;
    },

    /*
     * The page, flown into the card: scaled about the top of what the display
     * is showing, cut to the card's rounded corners, and cut again where it
     * would run under the column, since the column's glass is drawn beneath
     * the page rather than over it.
     */
    place(rect, radius, home, columnRight, W, H) {
      const s = LXB.screenEl;
      if (!s) return;
      if (home <= 0) {
        if (s.classList.contains("flying")) {
          s.classList.remove("flying");
          s.style.transform = s.style.transformOrigin = s.style.clipPath = s.style.minHeight = "";
          s.style.removeProperty("--screen-y");
          if (this.scrollY !== undefined && Math.abs(window.scrollY - this.scrollY) > 1) window.scrollTo({ top: this.scrollY, behavior: "instant" });
          this.scrollY = undefined;
        }
        return;
      }
      if (!s.classList.contains("flying")) {
        this.scrollY = window.scrollY;
        s.classList.add("flying");
        s.style.minHeight = H + "px";
      }
      const y = this.scrollY;
      const k = rect.w / W;
      const inner = s.offsetHeight;
      const left = columnRight > rect.x ? (columnRight - rect.x) / k : 0;
      s.style.setProperty("--screen-y", y + "px");
      s.style.transformOrigin = "0 " + y + "px";
      s.style.transform = "translate(" + rect.x + "px," + rect.y + "px) scale(" + k + ")";
      s.style.clipPath = "inset(" + y + "px 0 " + Math.max(0, inner - y - H) + "px " + left + "px round " + radius / k + "px)";
    },

    // The light: one lit capsule gliding between entries, which each give their
    // own chip up only once it has arrived over them.
    lit(snap) {
      this.stops.forEach((s) => s.classList.remove("on"));
      this.el.querySelectorAll(".guide-tile.on, .guide-swatches button.on").forEach((x) => x.classList.remove("on"));
      const target = this.target();
      if (target) {
        target.classList.add("on");
        // The column scrolls to keep the light in view when it is longer than
        // the display; only the column, never the page in the card.
        const box = this.entries;
        const [, y, , h] = this.offset(target);
        const pad = 12;
        let to = null;
        if (y - pad < box.scrollTop) to = y - pad;
        else if (y + h + pad > box.scrollTop + box.clientHeight) to = y + h + pad - box.clientHeight;
        if (to !== null) box.scrollTo({ top: Math.max(0, to), behavior: snap ? "instant" : "smooth" });
      }
      if (snap) this.lightFrom = null;
      this.kick();
    },
    // Where `el` is in the column's own scrolling coordinates.
    offset(el) {
      let x = 0, y = 0;
      for (let n = el; n && n !== this.entries; n = n.offsetParent) { x += n.offsetLeft; y += n.offsetTop; }
      return [x, y, el.offsetWidth, el.offsetHeight];
    },
    target() {
      if (this.pane === "card") return null;
      const stop = this.stops[this.at];
      if (!stop) return null;
      if (stop.classList.contains("guide-tiles")) return stop.children[this.tileAt];
      if (stop.classList.contains("guide-swatches")) return stop.children[this.swatchAt];
      return stop;
    },
    glide(dt, reduced) {
      const target = this.target();
      const light = this.light;
      const handedBack = (keep) => this.el.querySelectorAll('[style*="--over"]').forEach((c) => c !== keep && c.style.removeProperty("--over"));
      if (!target) {
        light.style.opacity = "0";
        this.lightMoving = false;
        handedBack(null);
        return;
      }
      // Measured in the column's own scrolling coordinates, so the light
      // scrolls with what it is on.
      const want = this.offset(target);
      const round = target.classList.contains("guide-tile") ? GUIDE.TILE_RADIUS : 0.5;
      if (!this.lightFrom || reduced) {
        this.lightFrom = { at: want.slice(), vel: [0, 0, 0, 0], round, roundVel: 0 };
      } else {
        const l = this.lightFrom;
        for (let i = 0; i < 4; i++) [l.at[i], l.vel[i]] = spring(l.at[i], l.vel[i], want[i], GUIDE.LIGHT_RATE, dt);
        [l.round, l.roundVel] = spring(l.round, l.roundVel, round, GUIDE.LIGHT_RATE, dt);
      }
      const [lx, ly, lw, lh] = this.lightFrom.at;
      light.style.transform = "translate(" + lx + "px," + ly + "px)";
      light.style.width = lw + "px";
      light.style.height = lh + "px";
      light.style.borderRadius = lh * this.lightFrom.round + "px";
      light.style.opacity = "1";
      light.classList.toggle("disc", target.parentElement && target.parentElement.classList.contains("guide-swatches"));
      // `highlight_arrival`: how much of the light is sitting on the entry.
      const apart = Math.abs(lx - want[0]) / Math.max(want[2], 1) + Math.abs(ly - want[1]) / Math.max(want[3], 1)
        + Math.abs(lw - want[2]) / Math.max(want[2], 1) + Math.abs(lh - want[3]) / Math.max(want[3], 1);
      target.style.setProperty("--over", String(1 - clamp01(apart)));
      handedBack(target);
      this.lightMoving = apart > 0.002;
    },

    // --- the five acts ---------------------------------------------------------

    move(step) {
      if (this.pane === "card") return;
      const next = this.at + step;
      if (next < 0 || next >= this.stops.length) return;
      this.at = next;
      this.lit();
      sound.play("press-guide");
    },
    up() { this.move(-1); },
    down() { this.move(1); },
    side(step) {
      if (this.pane === "card") {
        if (step < 0) { this.focusCard(false); sound.play("press-guide"); }
        return;
      }
      const stop = this.stops[this.at];
      if (stop.classList.contains("guide-tiles")) {
        const n = this.tileAt + step;
        if (n < 0) return;
        if (n >= stop.children.length) { this.focusCard(true); sound.play("press-guide"); return; }
        this.tileAt = n;
      } else if (stop.classList.contains("guide-swatches")) {
        const n = this.swatchAt + step;
        if (n < 0) return;
        if (n >= stop.children.length) { this.focusCard(true); sound.play("press-guide"); return; }
        this.swatchAt = n;
        LXB.setAccent(Object.keys(LXB.PALETTES)[n]);
        this.sync();
      } else if (stop.classList.contains("guide-bar")) {
        const v = Math.max(0, Math.min(1, Math.round((prefs.volume + step * 0.05) * 20) / 20));
        if (v === prefs.volume) return;
        sound.setVolume(v);
        this.sync();
      } else {
        // Right leaves the column for the card, and Left comes back.
        if (step > 0) { this.focusCard(true); sound.play("press-guide"); }
        return;
      }
      this.lit();
      sound.play("press-guide");
    },
    focusCard(on, quiet) {
      this.pane = on ? "card" : "menu";
      this.lit();
      if (!quiet) legend();
    },
    left() { this.side(-1); },
    right() { this.side(1); },
    select() {
      if (this.pane === "card") { sound.play("press-guide-selected"); this.show(false); return; }
      const stop = this.stops[this.at];
      if (stop.classList.contains("guide-tiles")) stop.children[this.tileAt].click();
      else if (stop.classList.contains("guide-swatches")) stop.children[this.swatchAt].click();
      else if (stop.classList.contains("guide-bar")) return;
      else stop.click();
    },
    back() { this.show(false); },
    guide() { this.show(false); },
    legend() { return ["select", "back"]; },
  };
  LXB.guide = guide;

  function guideButton() {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "guide-button";
    b.setAttribute("aria-label", LXB.t("open-guide"));
    b.setAttribute("aria-controls", "guide");
    b.title = LXB.t("guide-key");
    b.append(LXB.img("pad-guide", "sm"));
    b.addEventListener("click", () => guide.show(!guide.open));
    document.body.append(b);
  }

  /*
   * Everything a page shows, but for the wallpaper and the shell's own
   * furniture, is gathered into one element, so the guide can fly the screen
   * into its card in one piece.
   */
  function gatherScreen() {
    const screen = document.createElement("div");
    screen.className = "screen";
    const stays = (n) => n.nodeType === 1 && (n.id === "wallpaper" || n.classList.contains("skip") || /^(SCRIPT|NOSCRIPT|TEMPLATE)$/.test(n.tagName));
    Array.from(document.body.childNodes).forEach((n) => { if (!stays(n)) screen.append(n); });
    const canvas = document.getElementById("wallpaper");
    if (canvas && canvas.parentNode === document.body) canvas.after(screen);
    else document.body.prepend(screen);
    LXB.screenEl = screen;
  }

  // --- the language ------------------------------------------------------------

  /*
   * The page in another language: the same page, at the same place on it, in
   * the directory that language's pages are kept in. The menu is the shell's
   * list — each language by its own name, in the order those names sort — and
   * it is raised from the button beside the guide's, or from the guide itself.
   * Like the shell's menus it answers in the bar's voice wherever it is raised
   * from, and like them it takes the pointer's light without a sound.
   */
  LXB.inLanguage = function (language) {
    const last = location.pathname.split("/").pop();
    const file = document.body.dataset.screen !== "lost" && /\.html$/.test(last) ? last : "index.html";
    return base + (language.dir ? language.dir + "/" : "") + file + location.hash;
  };

  const languages = {
    open: false,
    at: 0,
    build() {
      const scrim = document.createElement("div");
      scrim.className = "language-scrim";
      scrim.addEventListener("click", () => this.show(false));
      const el = document.createElement("div");
      el.className = "language-menu glass";
      el.tabIndex = -1;
      el.setAttribute("role", "menu");
      el.setAttribute("aria-label", LXB.t("language"));
      const title = document.createElement("div");
      title.className = "language-title";
      title.append(LXB.img("setting-language", "sm"), document.createTextNode(LXB.t("language")));
      const list = document.createElement("div");
      list.className = "language-list";
      this.light = document.createElement("div");
      this.light.className = "language-light";
      this.light.setAttribute("aria-hidden", "true");
      list.append(this.light);
      this.rows = LXB.LANGUAGES.map((language, i) => {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "language-row";
        b.setAttribute("role", "menuitemradio");
        b.setAttribute("aria-checked", String(language.tag === LXB.lang));
        // Each name in its own language, so it is read and drawn as one.
        b.lang = language.tag;
        b.textContent = language.name;
        if (language.tag === LXB.lang) b.classList.add("here");
        b.addEventListener("click", () => this.choose(i));
        b.addEventListener("pointermove", (e) => {
          if (e.pointerType === "mouse" && this.at !== i) { this.at = i; this.lit(); }
        });
        list.append(b);
        return b;
      });
      el.append(title, list);
      document.body.append(scrim, el);
      this.el = el;
    },
    show(on, from) {
      if (!this.el) this.build();
      if (on === this.open) return;
      this.open = on;
      root.classList.toggle("language-open", on);
      if (on) {
        this.returnFocus = document.activeElement;
        this.at = Math.max(0, LXB.LANGUAGES.findIndex((l) => l.tag === LXB.lang));
        this.place(from);
        this.lit(true);
        this.el.focus({ preventScroll: true });
      } else {
        if (this.el.contains(document.activeElement)) document.activeElement.blur();
        const back = this.returnFocus;
        if (back && back !== document.body && back.isConnected && back.focus) back.focus({ preventScroll: true });
      }
      legend();
    },
    // Out of the button it was raised from, or beside the guide's column.
    place(from) {
      const el = this.el;
      el.style.left = el.style.top = el.style.right = el.style.bottom = "";
      const W = root.clientWidth, H = root.clientHeight, gap = 12;
      el.style.maxHeight = H - gap * 2 + "px";
      const w = el.offsetWidth, h = el.offsetHeight;
      let x, y;
      if (from && from.getBoundingClientRect) {
        const r = from.getBoundingClientRect();
        x = r.left;
        y = r.top > H / 2 ? r.top - h - gap : r.bottom + gap;
      } else if (guide.open && guide.el) {
        const r = guide.el.getBoundingClientRect();
        x = r.right + gap * 2;
        y = (H - h) / 2;
      } else {
        x = (W - w) / 2;
        y = (H - h) / 2;
      }
      el.style.left = Math.max(gap, Math.min(W - w - gap, x)) + "px";
      el.style.top = Math.max(gap, Math.min(H - h - gap, y)) + "px";
    },
    lit(snap) {
      const row = this.rows[this.at];
      this.rows.forEach((r) => r.classList.toggle("on", r === row));
      this.light.classList.toggle("snap", !!snap);
      this.light.style.transform = "translate(" + row.offsetLeft + "px," + row.offsetTop + "px)";
      this.light.style.width = row.offsetWidth + "px";
      this.light.style.height = row.offsetHeight + "px";
      const list = row.parentElement;
      if (row.offsetTop < list.scrollTop) list.scrollTop = row.offsetTop;
      else if (row.offsetTop + row.offsetHeight > list.scrollTop + list.clientHeight) list.scrollTop = row.offsetTop + row.offsetHeight - list.clientHeight;
    },
    move(step) {
      const next = this.at + step;
      if (next < 0 || next >= this.rows.length) return;
      this.at = next;
      this.lit();
      sound.play("press");
    },
    choose(i) {
      const language = LXB.LANGUAGES[i];
      sound.play("press-selected");
      store.set("language", language.tag);
      if (language.tag === LXB.lang) { this.show(false); return; }
      music.remember();
      document.body.classList.add("leaving");
      setTimeout(() => { location.href = LXB.carry(LXB.inLanguage(language)); }, window.matchMedia("(prefers-reduced-motion: reduce)").matches ? 0 : 160);
    },
    up() { this.move(-1); },
    down() { this.move(1); },
    left() {},
    right() {},
    select() { this.choose(this.at); },
    back() { this.show(false); },
    guide() { this.show(false); },
    legend() { return ["select", "back"]; },
  };
  LXB.languages = languages;

  function languageButton() {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "language-button";
    b.setAttribute("aria-label", LXB.t("language"));
    b.setAttribute("aria-haspopup", "menu");
    b.title = LXB.t("language") + " · " + LXB.language.name;
    b.append(LXB.img("setting-language", "sm"));
    b.addEventListener("click", () => {
      if (!languages.open) sound.play("press-selected");
      languages.show(!languages.open, b);
    });
    document.body.append(b);
  }

  // --- the launch splash -------------------------------------------------------------

  // `what.quiet` is for a press that has made its own sound already: the
  // launch sound is the start screen's, and the guide never borrows it.
  LXB.launch = function (href, what) {
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (!what.quiet) sound.play("app-launch");
    music.remember();
    try { sessionStorage.setItem("lxb-arrived-by", "launch"); } catch (e) { /* ignore */ }
    if (reduced) { location.href = LXB.carry(href); return; }
    let splash = document.querySelector(".splash");
    if (!splash) {
      splash = document.createElement("div");
      splash.className = "splash";
      splash.setAttribute("aria-hidden", "true");
      splash.innerHTML = '<div class="splash-body"><div class="splash-disc"><div class="splash-ring"></div></div><div class="splash-name"></div></div>';
      const ring = splash.querySelector(".splash-ring");
      for (let i = 0; i < 12; i++) {
        const dot = document.createElement("i");
        const a = (i / 12) * Math.PI * 2;
        dot.style.transform = "translate(" + Math.cos(a) * 114 + "px," + Math.sin(a) * 114 + "px)";
        dot.style.opacity = String(0.25 + 0.75 * (i / 11));
        ring.append(dot);
      }
      document.body.append(splash);
    }
    const disc = splash.querySelector(".splash-disc");
    disc.querySelectorAll("img").forEach((i) => i.remove());
    disc.append(LXB.img(what.glyph || "logo"));
    splash.querySelector(".splash-name").textContent = what.name || "";
    requestAnimationFrame(() => splash.classList.add("up"));
    setTimeout(() => { location.href = LXB.carry(href); }, 520);
  };

  // Pages the browser keeps in its back-forward cache come back with the
  // splash still up; take it down again.
  window.addEventListener("pageshow", (e) => {
    if (e.persisted) {
      const s = document.querySelector(".splash");
      if (s) s.classList.remove("up");
      if (guide.open) guide.show(false, "instant");
      refresh();
      music.retry();
    }
  });

  // What the visitor chose may have changed while this page was not the one
  // in front: on another page before the browser brought this one back from
  // its cache, or in another tab. Whatever is kept now is what shows.
  function refresh() {
    const accent = store.get("accent", "Purple");
    if (accent !== prefs.accent && LXB.PALETTES[accent]) LXB.setAccent(accent, false);
    const simple = store.get("simple", false);
    const particles = store.get("particles", true);
    if (simple !== prefs.simple || particles !== prefs.particles) LXB.setLook({ simple, particles });
    const volume = store.get("volume", 0.55);
    if (volume !== prefs.volume) sound.setVolume(volume);
    prefs.sound = store.get("sound", true);
    prefs.music = store.get("music", true);
    if (!prefs.sound || !prefs.music) music.stop();
    if (guide.el) guide.sync();
  }
  window.addEventListener("storage", (e) => {
    if (e.key === null || e.key.startsWith("lxb-")) refresh();
  });

  // The Settings page's own controls: the accents, and the wallpaper's two
  // switches, changing this site as the rows change the shell.
  function demos() {
    document.querySelectorAll("[data-accent-picker]").forEach((box) => {
      for (const name of Object.keys(LXB.PALETTES)) {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "swatch";
        b.dataset.accent = name;
        b.style.setProperty("--swatch", "#" + LXB.PALETTES[name].accent);
        const dot = document.createElement("span");
        dot.className = "swatch-dot";
        const label = document.createElement("span");
        label.textContent = LXB.t(name);
        b.append(dot, label);
        b.addEventListener("click", () => { LXB.setAccent(name); sound.play("press-selected"); });
        box.append(b);
      }
    });
    document.querySelectorAll("[data-look]").forEach((b) => b.addEventListener("click", () => {
      LXB.setLook(b.dataset.look === "simple" ? { simple: !prefs.simple } : { particles: !prefs.particles });
      sound.play("press-selected");
    }));
    const sync = () => {
      document.querySelectorAll("[data-accent]").forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.accent === prefs.accent)));
      document.querySelectorAll("[data-look]").forEach((b) => {
        const simple = b.dataset.look === "simple";
        b.setAttribute("aria-pressed", String(simple ? !prefs.simple : prefs.particles));
        b.querySelector("[data-look-value]").textContent = simple ? LXB.t(prefs.simple ? "silk" : "water") : LXB.t(prefs.particles ? "on" : "off");
      });
      if (guide.el) guide.sync();
    };
    document.addEventListener("lxb:accent", sync);
    document.addEventListener("lxb:look", sync);
    sync();
  }

  // Copy buttons on command blocks.
  function copyButtons() {
    document.querySelectorAll("pre > code").forEach((code) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "copy";
      b.textContent = LXB.t("copy");
      b.addEventListener("click", () => {
        navigator.clipboard && navigator.clipboard.writeText(code.textContent.trim()).then(() => {
          b.textContent = LXB.t("copied");
          setTimeout(() => (b.textContent = LXB.t("copy")), 1400);
        });
      });
      code.parentElement.append(b);
    });
  }

  // Tabs, for the four ways of installing.
  function tabs() {
    document.querySelectorAll(".tabs").forEach((list) => {
      const buttons = Array.from(list.querySelectorAll("button"));
      buttons.forEach((b) => b.addEventListener("click", () => {
        buttons.forEach((o) => {
          const on = o === b;
          o.setAttribute("aria-selected", String(on));
          document.getElementById(o.getAttribute("aria-controls")).hidden = !on;
        });
      }));
    });
  }

  function init() {
    root.dataset.input = device;
    gatherScreen();
    sound.load();
    music.start();
    startWallpaper();
    corner();
    guideButton();
    languageButton();
    copyButtons();
    tabs();
    demos();
    legend();
    LXB.ready = true;
    document.dispatchEvent(new CustomEvent("lxb:ready"));
  }
  // After every deferred script has run, so the page's own script is listening.
  if (document.readyState === "complete") init();
  else document.addEventListener("DOMContentLoaded", init);
  LXB.whenReady = function (start) {
    if (LXB.ready) start();
    else document.addEventListener("lxb:ready", start, { once: true });
  };
})();

/*
 * The start screen: the shell's bar, driven by a pad, a keyboard, a mouse or a
 * finger.
 *
 * Laid out with the constants in crates/lxb-desktop/src/ui.rs on a 1080-line
 * reference display — the cross where the arms meet at 22% across and 30%
 * down, categories 200 apart, 84 at rest and 148 chosen on squircles of glass
 * 1.3 times their size, rows 124 apart at 64 and 105, the chosen one on a disc
 * √2·1.04 its size, and the gap the category row opens in the column above and
 * below it. Every glide is the critically damped spring the shell's cursor
 * rides (`overview::spring`, rate 19), so a move has a start rather than a
 * launch and a reversal mid-flight carries on from where it is.
 *
 * Typing searches the whole bar, as it does in the shell: the category row
 * closes up into the button the cursor was on, the button becomes a magnifier
 * called Search, what is typed is written large beside it, and the column under
 * it is everything on the bar the words fit.
 *
 * The tiles, the discs and the bloom behind the chosen one are drawn by the
 * wallpaper's WebGL pass as the shell draws them; the marks and the words are
 * the page's own elements, laid over them on the same frame.
 */
(function () {
  "use strict";
  const LXB = window.LXB;

  const REF = 1080;
  const CROSS_X = 0.22, CROSS_Y = 0.30;
  const CATEGORY_SPACING = 200, CATEGORY_ICON = 84, CATEGORY_ICON_FOCUSED = 148, CATEGORY_DISC = 1.30;
  const ITEM_SPACING = 124, ITEM_ICON = 64, ITEM_ICON_FOCUSED = 105, ITEM_DISC = Math.SQRT2 * 1.04;
  const CATEGORY_LABEL = 26, CATEGORY_LABEL_ABOVE = 3, CATEGORY_LABEL_BELOW = 25;
  const CATEGORY_HALF = (CATEGORY_ICON_FOCUSED * CATEGORY_DISC) / 2;
  const GAP_BELOW = CATEGORY_HALF + CATEGORY_LABEL_ABOVE + CATEGORY_LABEL + CATEGORY_LABEL_BELOW + (ITEM_ICON_FOCUSED * ITEM_DISC) / 2;
  const GAP_ABOVE = 168;
  const SEARCH_FIELD = 46, SEARCH_FIELD_GAP = 28;
  const CATEGORY_MIN_ALPHA = 0.62;
  const DEPTH_CONTROL = 9, FROST_CONTROL = 0.2, GLOSS_FULL = 1, GLOSS_QUIET = 0.45;
  const PULSE_PERIOD = 1.8;
  const EASE_RATE = 19;
  // The row gathering into its button is slower than a step, so the eye can
  // follow the others travelling in.
  const GATHER_RATE = 8;

  // `overview::spring`: critically damped, carrying its velocity between frames.
  function spring(position, velocity, target, rate, dt) {
    dt = Math.min(Math.max(dt, 0), 0.1);
    const offset = position - target;
    const c = velocity + rate * offset;
    const decay = Math.exp(-rate * dt);
    return [target + (offset + c * dt) * decay, (velocity - c * rate * dt) * decay];
  }
  const lerp = (a, b, t) => a + (b - a) * Math.min(Math.max(t, 0), 1);
  const smooth = (a, b, x) => {
    const t = Math.min(Math.max((x - a) / (b - a), 0), 1);
    return t * t * (3 - 2 * t);
  };

  // `item_y`: rows below the cross after the gap the category row opens, rows
  // above it before; the chosen row straddles neither.
  function itemY(offset, crossY, above, below, spacing) {
    if (offset >= 0) return crossY + below + offset * spacing;
    if (offset <= -1) return crossY - above + (offset + 1) * spacing;
    return crossY + below + offset * (below + above);
  }

  function place(el, x, y, w, h, opacity) {
    el.style.transform = "translate(" + x + "px," + y + "px)";
    if (w !== null) {
      el.style.width = w + "px";
      el.style.height = h + "px";
    }
    el.style.opacity = String(opacity);
    el.style.visibility = opacity > 0.004 ? "visible" : "hidden";
  }

  // A row of a column: its link, its mark and its two lines.
  function makeRow(a, glyph, title, comment) {
    const row = { a, glyph, title, comment };
    row.mark = LXB.img(glyph, "lg");
    row.mark.classList.add("bar-mark");
    const words = document.createElement("span");
    words.className = "bar-text";
    words.append(...Array.from(a.childNodes));
    a.append(row.mark, words);
    a.dataset.lxbRow = "";
    a.tabIndex = -1;
    if (a.target === "_blank") a.querySelector(".row-name").classList.add("row-ext");
    return row;
  }

  const bar = {
    cats: [],
    cat: 0,
    catPos: 0,
    catVel: 0,
    arrive: 0,
    // The search: what is typed, the rows it found, and how far the category
    // row has closed up into its button (0 spread out, 1 gathered).
    search: null,
    gather: 0,
    gatherVel: 0,
    results: [],
    found: { row: 0, rowPos: 0, rowVel: 0 },

    build() {
      const nav = document.querySelector(".bar-categories");
      const layer = document.querySelector(".bar-layer");
      nav.querySelectorAll(":scope > li").forEach((li, ci) => {
        const cat = {
          li,
          id: li.dataset.id,
          title: li.querySelector(".cat-name").textContent.trim(),
          glyph: li.dataset.glyph,
          rows: [],
          row: 0,
          rowPos: 0,
          rowVel: 0,
        };
        cat.mark = LXB.img(cat.glyph, "lg");
        cat.mark.classList.add("bar-mark");
        cat.hit = document.createElement("button");
        cat.hit.type = "button";
        cat.hit.className = "bar-hit";
        cat.hit.tabIndex = -1;
        cat.hit.setAttribute("aria-label", cat.title);
        cat.hit.addEventListener("click", () => {
          if (this.search) { this.endSearch(); return; }
          this.goCategory(ci);
        });
        cat.label = document.createElement("div");
        cat.label.className = "bar-label";
        cat.label.textContent = cat.title;
        cat.label.setAttribute("aria-hidden", "true");
        layer.append(cat.hit, cat.mark, cat.label);
        li.querySelectorAll(".cat-rows a").forEach((a, ri) => {
          const row = makeRow(
            a,
            a.dataset.glyph,
            a.querySelector(".row-name").textContent.trim(),
            (a.querySelector(".row-note") || { textContent: "" }).textContent.trim(),
          );
          a.addEventListener("click", (e) => {
            e.preventDefault();
            if (this.cat === ci && cat.row === ri) this.press(row);
            else {
              if (this.cat !== ci) this.goCategory(ci, true);
              this.goRow(ri);
            }
          });
          cat.rows.push(row);
        });
        this.cats.push(cat);
      });

      // The search's own furniture: the magnifier the chosen button turns
      // into, its name, the words being typed, and the column of what they
      // found.
      this.lens = LXB.img("search", "lg");
      this.lens.classList.add("bar-mark");
      this.lensLabel = document.createElement("div");
      this.lensLabel.className = "bar-label";
      this.lensLabel.textContent = LXB.t("search");
      this.lensLabel.setAttribute("aria-hidden", "true");
      this.query = document.createElement("div");
      this.query.className = "bar-query";
      this.query.setAttribute("role", "search");
      this.query.setAttribute("aria-live", "polite");
      this.nothing = document.createElement("div");
      this.nothing.className = "bar-nothing";
      this.nothing.textContent = LXB.t("nothing-on-bar");
      this.foundList = document.createElement("div");
      this.foundList.className = "bar-found cat-rows";
      layer.append(this.lens, this.lensLabel, this.query, this.nothing);
      document.querySelector(".bar nav").append(this.foundList);
      document.body.classList.add("bar-live");
    },

    restore() {
      // Where to stand: what the address names (a page coming back to the bar
      // names itself), or where this visit last stood.
      let want = location.hash.replace(/^#/, "");
      if (!want) {
        try { want = sessionStorage.getItem("lxb-bar") || ""; } catch (e) { want = ""; }
      }
      const [catId, rowHash] = want.split("/");
      const ci = Math.max(0, this.cats.findIndex((c) => c.id === catId));
      this.cat = ci;
      const cat = this.cats[ci];
      if (rowHash) {
        const ri = cat.rows.findIndex((r) => r.a.getAttribute("href").split("#")[1] === rowHash);
        if (ri >= 0) cat.row = ri;
      }
      this.cats.forEach((c) => (c.rowPos = c.row));
      // Arriving, the row glides in from the right onto where it stands, as
      // the shell's bar arrives once its first frame is up.
      const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      this.catPos = reduced ? ci : ci + 0.9;
      this.arrive = reduced ? 1 : 0;
    },

    remember() {
      const cat = this.cats[this.cat];
      const row = cat.rows[cat.row];
      const hash = row ? (row.a.getAttribute("href").split("#")[1] || "") : "";
      const at = cat.id + (hash ? "/" + hash : "");
      try { sessionStorage.setItem("lxb-bar", at); } catch (e) { /* ignore */ }
      if (history.replaceState) history.replaceState(null, "", "#" + at);
      this.lightLinks();
      this.announce(row ? cat.title + ": " + row.title + (row.comment ? ". " + row.comment : "") : "");
    },
    lightLinks() {
      const lit = this.litRow();
      this.cats.forEach((c) => c.rows.forEach((r) => this.mark(r, r === lit)));
      this.results.forEach((r) => this.mark(r, r === lit));
    },
    mark(r, lit) {
      r.a.tabIndex = lit ? 0 : -1;
      if (lit) r.a.setAttribute("aria-current", "true");
      else r.a.removeAttribute("aria-current");
    },
    announce(text) {
      const live = document.querySelector(".bar-live-region");
      if (live) live.textContent = text;
    },
    litRow() {
      if (this.search) return this.results[this.found.row] || null;
      const cat = this.cats[this.cat];
      return cat.rows[cat.row] || null;
    },

    goCategory(ci, quiet) {
      if (ci < 0 || ci >= this.cats.length || ci === this.cat) return false;
      this.cat = ci;
      if (!quiet) LXB.sound.play("press");
      this.remember();
      LXB.legend();
      return true;
    },
    goRow(ri) {
      if (this.search) {
        if (ri < 0 || ri >= this.results.length || ri === this.found.row) return false;
        this.found.row = ri;
        LXB.sound.play("press");
        this.lightLinks();
        const r = this.results[ri];
        this.announce(r.title + ". " + r.comment);
        return true;
      }
      const cat = this.cats[this.cat];
      if (ri < 0 || ri >= cat.rows.length || ri === cat.row) return false;
      cat.row = ri;
      LXB.sound.play("press");
      this.remember();
      return true;
    },
    press(row, from) {
      row = row || this.litRow();
      if (!row) return;
      const href = row.a.href;
      // Another site opens beside this one. Compared by protocol and host
      // rather than by origin, which a page opened straight off the disk does
      // not have — and there every row was taken for another site's.
      const to = new URL(href, location.href);
      if (row.a.target === "_blank" || to.protocol !== location.protocol || to.host !== location.host) {
        LXB.sound.play("press-selected");
        // A pad's press is not one a browser lets open a window with.
        if (from === "pad") location.href = href;
        else window.open(href, "_blank", "noopener");
        return;
      }
      if (this.search) {
        // A result is pressed where it lives: the bar is left standing on it,
        // so coming back finds the column it belongs to.
        const home = row.home;
        if (home) {
          this.cat = home.ci;
          this.cats[home.ci].row = home.ri;
          this.cats[home.ci].rowPos = home.ri;
          this.catPos = home.ci;
          this.remember();
        }
      }
      LXB.launch(href, { glyph: row.glyph, name: row.title });
    },

    /*
     * As in the shell, only the lit row opens. A click on any other lights it
     * — the column slides it to the cross — and a second click, on the row
     * now standing there, opens it. A mouse, a pen and a finger alike.
     */

    // --- the search -----------------------------------------------------------

    type(key) {
      if (LXB.guide.open) return false;
      if (key === "Backspace") {
        if (!this.search) return false;
        this.search.query = this.search.query.slice(0, -1);
        if (!this.search.query) this.endSearch();
        else this.find();
        return true;
      }
      if (key.length !== 1) return false;
      // A space begins nothing — it is Select on the bar — but it is part of
      // what is being typed once something is.
      if (key === " " && !this.search) return false;
      if (!/[\p{L}\p{N}\p{P}\p{S} ]/u.test(key)) return false;
      if (!this.search) {
        this.search = { query: "" };
        this.found = { row: 0, rowPos: 0, rowVel: 0 };
        document.body.classList.add("bar-searching");
      }
      this.search.query += key;
      this.find();
      LXB.legend();
      return true;
    },
    endSearch() {
      if (!this.search) return;
      this.search = null;
      document.body.classList.remove("bar-searching");
      this.foundList.textContent = "";
      this.results = [];
      LXB.sound.play("press-back");
      this.remember();
      LXB.legend();
    },
    // Whole name first, then its start, then the start of one of its words,
    // then anywhere in it, and last what the row says about itself or the
    // column it is in. Ties keep the bar's order, left to right and down.
    find() {
      const q = this.search.query.trim().toLocaleLowerCase();
      const scored = [];
      this.cats.forEach((cat, ci) => cat.rows.forEach((row, ri) => {
        const name = row.title.toLocaleLowerCase();
        let score = -1;
        if (!q) score = -1;
        else if (name === q) score = 0;
        else if (name.startsWith(q)) score = 1;
        else if (name.split(/[\s\-–—&/]+/).some((w) => w.startsWith(q))) score = 2;
        else if (name.includes(q)) score = 3;
        else if (row.comment.toLocaleLowerCase().includes(q)) score = 4;
        else if (cat.title.toLocaleLowerCase().includes(q)) score = 5;
        if (score >= 0) scored.push({ score, order: scored.length, cat, ci, ri, row });
      }));
      scored.sort((a, b) => a.score - b.score || a.order - b.order);
      // Rebuilt rather than kept: a result is a copy of the row it found,
      // carrying the line that says where that row lives.
      this.foundList.textContent = "";
      this.results = scored.map((hit) => {
        const a = document.createElement("a");
        a.href = hit.row.a.getAttribute("href");
        if (hit.row.a.target) { a.target = hit.row.a.target; a.rel = "noopener"; }
        const name = document.createElement("span");
        name.className = "row-name";
        name.textContent = hit.row.title;
        const note = document.createElement("span");
        note.className = "row-note";
        const where = hit.cat.title + " · " + hit.row.comment;
        note.textContent = where;
        a.append(name, note);
        this.foundList.append(a);
        const row = makeRow(a, hit.row.glyph, hit.row.title, where);
        row.home = { ci: hit.ci, ri: hit.ri };
        a.addEventListener("click", (e) => {
          e.preventDefault();
          const ri = this.results.indexOf(row);
          if (ri === this.found.row) this.press(row);
          else {
            this.goRow(ri);
          }
        });
        return row;
      });
      this.found.row = 0;
      this.found.rowPos = Math.min(this.found.rowPos, 0);
      this.results.forEach((r) => (r.a.style.opacity = "0"));
      this.lightLinks();
      this.announce(this.results.length ? LXB.t("found", { count: this.results.length, first: this.results[0].title }) : LXB.t("nothing-found"));
    },

    // --- the five acts --------------------------------------------------------

    left() { if (!this.search) this.goCategory(this.cat - 1); },
    right() { if (!this.search) this.goCategory(this.cat + 1); },
    up() { this.goRow((this.search ? this.found.row : this.cats[this.cat].row) - 1); },
    down() { this.goRow((this.search ? this.found.row : this.cats[this.cat].row) + 1); },
    select(from) { this.press(null, from); },
    // Back gives a search up. At the top of the bar there is nothing to go
    // back from, and — as in the shell — Esc opens the guide there instead.
    back(from) {
      if (this.search) { this.endSearch(); return true; }
      if (from === "keys") { LXB.guide.show(true); return true; }
      return false;
    },
    prev() { this.left(); },
    next() { this.right(); },
    legend() { return this.search ? ["select", "back"] : ["select"]; },
    label(act) { return act === "back" && this.search ? LXB.t("cancel") : null; },
    // On a keyboard the guide is Esc here, since every letter begins a search.
    glyph(act, kind) { return act === "guide" && kind === "keys" ? "key-escape" : null; },
    hidesGuide() { return !!this.search && LXB.device() !== "pad"; },

    // Wheel and swipe: a notch or a flick is one step, as a d-pad press is.
    gestures() {
      // A mouse's notch is one step, however many pixels the browser says it
      // is worth. A touchpad sends a stream of small amounts, which add up to a
      // step, and never more than one step per short while — its momentum
      // would otherwise throw the column to the end. Over the category row the
      // wheel walks the categories, since a plain wheel has no sideways.
      let sum = 0, at = 0, stepped = 0;
      window.addEventListener("wheel", (e) => {
        if (LXB.guide.open) return;
        e.preventDefault();
        const now = performance.now();
        const unit = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 800 : 1;
        const dx = e.deltaX * unit, dy = e.deltaY * unit;
        const band = (CATEGORY_HALF + CATEGORY_LABEL_ABOVE + CATEGORY_LABEL + 10) * (this.scale || 1);
        const overRow = this.crossY !== undefined && Math.abs(e.clientY - this.crossY) < band;
        const sideways = Math.abs(dx) > Math.abs(dy) || e.shiftKey || (overRow && !this.search);
        const d = Math.abs(dx) > Math.abs(dy) ? dx : dy;
        if (!d) return;
        const move = (dir) => {
          stepped = now;
          if (sideways) (dir > 0 ? this.right() : this.left());
          else (dir > 0 ? this.down() : this.up());
        };
        if (now - at > 200) sum = 0;
        at = now;
        if (Math.abs(d) >= 40) {
          sum = 0;
          move(Math.sign(d));
          return;
        }
        sum += d;
        if (Math.abs(sum) >= 50 && now - stepped > 90) {
          move(Math.sign(sum));
          sum = 0;
        }
      }, { passive: false });

      let start = null;
      const surface = document.querySelector(".bar");
      surface.addEventListener("pointerdown", (e) => {
        if (e.pointerType === "mouse") return;
        start = { x: e.clientX, y: e.clientY };
      });
      surface.addEventListener("pointerup", (e) => {
        if (!start) return;
        const dx = e.clientX - start.x, dy = e.clientY - start.y;
        const s = this.scale;
        start = null;
        if (Math.hypot(dx, dy) < 24) return;
        if (Math.abs(dx) > Math.abs(dy)) {
          if (this.search) return;
          const steps = Math.max(1, Math.round(Math.abs(dx) / (CATEGORY_SPACING * s * 0.9)));
          this.goCategory(Math.max(0, Math.min(this.cats.length - 1, this.cat - Math.sign(dx) * steps)));
        } else {
          const count = this.search ? this.results.length : this.cats[this.cat].rows.length;
          const at = this.search ? this.found.row : this.cats[this.cat].row;
          const steps = Math.max(1, Math.round(Math.abs(dy) / (ITEM_SPACING * s * 0.8)));
          this.goRow(Math.max(0, Math.min(count - 1, at - Math.sign(dy) * steps)));
        }
        this.swiped = performance.now();
      });
      surface.addEventListener("pointercancel", () => { start = null; });
      // A swipe that ended over a row must not also press it.
      surface.addEventListener("click", (e) => {
        if (this.swiped && performance.now() - this.swiped < 350) { e.stopPropagation(); e.preventDefault(); }
      }, true);
    },

    // --- drawing ----------------------------------------------------------------

    frame(dt, now) {
      const W = window.innerWidth, H = window.innerHeight;
      // One reference pixel. A display taller than it is wide is laid out by
      // its width too, or the row runs off the side of a phone.
      const s = Math.min(Math.max(Math.min(H / REF, W / 760), 0.55), 2.5);
      this.scale = s;
      const crossX = Math.max(W * CROSS_X, (CATEGORY_ICON_FOCUSED * CATEGORY_DISC * s) / 2 + 16);
      const crossY = H * CROSS_Y;
      this.crossY = crossY;
      const t = now / 1000;
      const pulse = 0.5 + 0.5 * Math.sin((t * 2 * Math.PI) / PULSE_PERIOD);
      const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      const step = (pos, vel, to, rate) => (reduced ? [to, 0] : spring(pos, vel, to, rate || EASE_RATE, dt));

      [this.catPos, this.catVel] = step(this.catPos, this.catVel, this.cat);
      [this.gather, this.gatherVel] = step(this.gather, this.gatherVel, this.search ? 1 : 0, GATHER_RATE);
      const gather = Math.min(Math.max(this.gather, 0), 1);
      if (this.arrive < 1) this.arrive = Math.min(1, this.arrive + dt / 0.7);
      const arrival = smooth(0, 1, this.arrive);
      const th = LXB.wallpaper.theme;
      const panes = [];
      const glows = [];
      const frame = { W, H, s, crossY, pulse, th, panes, glows };

      const catSpacing = CATEGORY_SPACING * s;
      const focalX = crossX;
      this.cats.forEach((cat, ci) => {
        const offset = ci - this.catPos;
        const spread = crossX + offset * catSpacing;
        // Searching, the others travel in and are taken into the chosen one,
        // each as it reaches the edge of its glass.
        const x = lerp(spread, focalX, ci === this.cat ? 0 : gather);
        const distance = Math.abs(offset);
        const focus = Math.max(0, 1 - distance);
        const selected = ci === this.cat;
        const taken = selected ? 1 : 1 - smooth(0.35, 0.85, gather);
        const alpha = Math.min(Math.max(1 - distance * 0.1, CATEGORY_MIN_ALPHA), 1) * arrival * taken;
        const icon = lerp(CATEGORY_ICON, CATEGORY_ICON_FOCUSED, focus) * s;
        const disc = icon * CATEGORY_DISC;
        const onScreen = x + disc > -40 && x - disc < W + 40 && alpha > 0.004;

        if (onScreen) {
          if (selected) {
            const lit = arrival;
            glows.push({ x, y: crossY, size: icon * (2 + 0.2 * pulse), color: [...th.accent, (0.28 + 0.24 * pulse) * lit] });
            panes.push({ x: x - disc / 2, y: crossY - disc / 2, w: disc, h: disc, color: [...th.accent, 0.12], radius: disc / 2, power: 4, slab: DEPTH_CONTROL * s, frost: FROST_CONTROL, gloss: GLOSS_FULL, fade: Math.max(lit, alpha * 0.85) });
          } else {
            panes.push({ x: x - disc / 2, y: crossY - disc / 2, w: disc, h: disc, color: [...th.glass_raised, 0.05], radius: disc / 2, power: 4, slab: DEPTH_CONTROL * s, frost: FROST_CONTROL, gloss: GLOSS_QUIET, fade: alpha * 0.85 });
          }
        }
        // The category's own mark gives way to the magnifier as the button
        // becomes the search's: it shrinks a little as it goes and the other
        // grows into its place.
        const own = selected ? icon * (1 - 0.25 * gather) : icon;
        place(cat.mark, x - own / 2, crossY - own / 2, own, own, onScreen ? alpha * (selected ? 1 - gather : 1) : 0);
        place(cat.hit, x - disc / 2, crossY - disc / 2, disc, disc, 1);
        cat.hit.classList.toggle("lit", selected);
        cat.hit.style.pointerEvents = onScreen ? "auto" : "none";

        // The name under the chosen button, clear of its glass and holding
        // still while the row glides underneath it.
        const labelSize = Math.max(15, CATEGORY_LABEL * s);
        const boxW = catSpacing * 1.7;
        const labelY = crossY + (CATEGORY_HALF + CATEGORY_LABEL_ABOVE) * s;
        cat.label.style.fontSize = labelSize + "px";
        cat.label.style.width = boxW + "px";
        place(cat.label, x - boxW / 2, labelY, null, null, selected ? arrival * (1 - gather) : 0);
        if (selected) {
          const lens = icon * (0.75 + 0.25 * gather);
          place(this.lens, x - lens / 2, crossY - lens / 2, lens, lens, gather * arrival);
          this.lensLabel.style.fontSize = labelSize + "px";
          this.lensLabel.style.width = boxW + "px";
          place(this.lensLabel, x - boxW / 2, labelY, null, null, gather * arrival);
          // What is being typed, large, beside the button.
          const size = Math.max(22, SEARCH_FIELD * s);
          const left = x + (CATEGORY_HALF + SEARCH_FIELD_GAP) * s;
          this.query.style.fontSize = size + "px";
          this.query.style.maxWidth = Math.max(120, W - left - 40) + "px";
          this.query.textContent = this.search ? this.search.query : this.query.textContent;
          place(this.query, left, crossY - size * 0.62, null, null, gather);
          this.nothing.style.fontSize = Math.max(14, 22 * s) + "px";
          place(this.nothing, x + (ITEM_ICON_FOCUSED * ITEM_DISC / 2 + 12) * s, crossY + GAP_BELOW * s - 14 * s,
            null, null, this.search && !this.results.length ? gather : 0);
        }

        // The column under it, which slides with its button and is only there
        // while its button is the one standing at the cross — and gives way to
        // the search's column as the row gathers.
        const shown = Math.max(0, 1 - distance * 1.8) * arrival * (selected ? 1 - gather : 1);
        [cat.rowPos, cat.rowVel] = step(cat.rowPos, cat.rowVel, cat.row);
        this.column(cat.rows, cat.rowPos, selected && !this.search ? cat.row : -1, x, shown, frame);
      });

      // The search's column: everything the words fit.
      if (this.results.length || this.gather > 0.004) {
        [this.found.rowPos, this.found.rowVel] = step(this.found.rowPos, this.found.rowVel, this.found.row);
        this.column(this.results, this.found.rowPos, this.search ? this.found.row : -1, focalX, gather * arrival, frame);
      }

      // Under the guide the bar is not dimmed: it is the card's miniature, and
      // a miniature of a screen is the screen, lit tile and all.
      LXB.wallpaper.glows = glows;
      LXB.wallpaper.panes = panes;
    },

    // One column's rows around `rowPos`, the lit one standing on its disc.
    column(rows, rowPos, litIndex, x, shown, f) {
      const { W, H, s, crossY, pulse, th, panes, glows } = f;
      const textX = x + (ITEM_ICON_FOCUSED * ITEM_DISC / 2 + 12) * s;
      const textMax = Math.max(120, W - textX - 48 * s);
      const bottomClear = H - (ITEM_ICON / 2 + 16) * s;
      rows.forEach((row, ri) => {
        const ro = ri - rowPos;
        const y = itemY(ro, crossY, GAP_ABOVE * s, GAP_BELOW * s, ITEM_SPACING * s);
        const rfocus = Math.max(0, 1 - Math.abs(ro));
        const rsel = ri === litIndex;
        const ricon = lerp(ITEM_ICON, ITEM_ICON_FOCUSED, rfocus) * s;
        // Faded against the bottom clearance, and above the category row
        // only the one row before the chosen one is worth drawing.
        let ralpha = shown * (1 - smooth(bottomClear - 60 * s, bottomClear + 30 * s, y));
        if (ro < 0) ralpha *= smooth(-2.1, -0.6, ro) * 0.9;
        const a = row.a;
        if (ralpha <= 0.004) {
          a.style.opacity = "0";
          a.style.visibility = "hidden";
          a.style.pointerEvents = "none";
          return;
        }
        if (rsel) {
          const d = ricon * ITEM_DISC;
          const lit = shown;
          glows.push({ x, y, size: ricon * (2 + 0.2 * pulse), color: [...th.accent, (0.22 + 0.2 * pulse) * lit] });
          panes.push({ x: x - d / 2, y: y - d / 2, w: d, h: d, color: [...th.accent, 0.13], radius: d / 2, power: 2, slab: DEPTH_CONTROL * s, frost: FROST_CONTROL, gloss: GLOSS_FULL, fade: lit * rfocus });
        }
        a.style.visibility = "visible";
        a.style.pointerEvents = "auto";
        a.classList.toggle("lit", rsel);
        const name = rsel ? Math.max(17, 30 * s) : Math.max(14, 22 * s);
        const note = Math.max(13, 19 * s);
        a.style.setProperty("--name", name + "px");
        a.style.setProperty("--note", note + "px");
        a.style.setProperty("--text-x", textX - x + ricon / 2 + "px");
        a.style.width = textX - x + ricon / 2 + textMax + "px";
        const top = rsel && row.comment ? y - 40 * s * Math.max(1, name / (30 * s)) : y - name * 0.62;
        const box = Math.min(top, y - ricon / 2);
        // The link spans the mark and the words, so a click on either is
        // one on the row.
        a.style.transform = "translate(" + (x - ricon / 2) + "px," + box + "px)";
        a.style.setProperty("--mark", ricon + "px");
        a.style.setProperty("--mark-y", y - ricon / 2 - box + "px");
        a.style.setProperty("--text-y", top - box + "px");
        a.style.opacity = String(ralpha);
      });
    },
  };

  // Without WebGL there is no glass to draw on: the tiles become the page's.
  function withoutWallpaper() {
    document.body.classList.add("bar-plain");
    LXB.wallpaper = {
      theme: LXB.rendered(LXB.prefs.accent),
      glows: [], panes: [], hooks: [],
      kick() { requestAnimationFrame((now) => this.tick(now)); },
      tick(now) {
        const dt = this.last ? Math.min((now - this.last) / 1000, 0.1) : 0.016;
        this.last = now;
        this.hooks.forEach((h) => h(dt, now));
        this.kick();
      },
      setAccent(name) { this.theme = LXB.rendered(name); },
      setStyle() {}, setSoften() {},
    };
    LXB.wallpaper.kick();
  }

  function hint() {
    // Said once a visit, in the corner, and gone again.
    try { if (sessionStorage.getItem("lxb-hinted")) return; sessionStorage.setItem("lxb-hinted", "1"); } catch (e) { return; }
    const toast = document.createElement("div");
    toast.className = "toast glass";
    toast.setAttribute("role", "status");
    const touch = matchMedia("(hover: none)").matches;
    toast.append(LXB.img(touch ? "setting-mouse" : "setting-keyboard", "sm"), document.createTextNode(LXB.t(touch ? "hint-touch" : "hint-mouse")));
    (LXB.screenEl || document.body).append(toast);
    setTimeout(() => toast.classList.add("up"), 1600);
    setTimeout(() => toast.classList.remove("up"), 9500);
    setTimeout(() => toast.remove(), 10200);
  }

  LXB.whenReady(() => {
    if (!LXB.wallpaper) withoutWallpaper();
    bar.build();
    bar.restore();
    bar.remember();
    bar.gestures();
    LXB.screen = bar;
    LXB.legend();
    LXB.wallpaper.hooks.push((dt, now) => bar.frame(dt, now));
    LXB.wallpaper.kick();
    hint();
    window.addEventListener("hashchange", () => {
      if (bar.search) return;
      bar.restore();
      bar.catPos = bar.cat;
      bar.arrive = 1;
      bar.remember();
    });
  });
})();

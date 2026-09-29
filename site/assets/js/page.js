/*
 * A column page: a category of the bar stepped into.
 *
 * Its rows run down the left and one of them is lit; what that row is about
 * stands on the pane of glass beside it. Up and Down walk the rows and bring
 * the pane in; scrolling the page moves the light to whatever is being read.
 * Back — or Left, which is the same exit in the shell — returns to the bar,
 * standing on this category and on the row that was lit.
 */
(function () {
  "use strict";
  const LXB = window.LXB;

  const page = {
    rows: [],
    panes: [],
    at: 0,
    holdSpy: 0,

    build() {
      this.rows = Array.from(document.querySelectorAll("[data-lxb-row]"));
      this.panes = this.rows.map((r) => document.getElementById(r.getAttribute("href").slice(1)));
      this.rowsList = document.querySelector(".rows");
      this.id = document.body.dataset.page;
      this.rows.forEach((row, i) => {
        row.addEventListener("click", (e) => {
          e.preventDefault();
          LXB.sound.unlock();
          if (i !== this.at) LXB.sound.play("press");
          this.go(i, true);
        });
      });
      document.querySelectorAll("[data-back]").forEach((a) => a.addEventListener("click", (e) => {
        e.preventDefault();
        this.back();
      }));
      const wanted = location.hash ? this.panes.findIndex((p) => p && "#" + p.id === location.hash) : -1;
      this.at = wanted >= 0 ? wanted : 0;
      this.light(false);
      window.addEventListener("scroll", () => this.spy(), { passive: true });
      window.addEventListener("resize", () => this.spy(), { passive: true });
    },

    light(scrollRow) {
      this.rows.forEach((r, i) => {
        const on = i === this.at;
        r.classList.toggle("lit", on);
        if (on) r.setAttribute("aria-current", "true");
        else r.removeAttribute("aria-current");
      });
      this.panes.forEach((p, i) => p && p.classList.toggle("lit", i === this.at));
      // On a narrow display the rows are a strip across the top: keep the lit
      // one in view by scrolling the strip, never the page.
      const list = this.rowsList;
      if (scrollRow !== false && list && list.scrollWidth > list.clientWidth + 4) {
        const row = this.rows[this.at];
        const left = row.offsetLeft - (list.clientWidth - row.offsetWidth) / 2;
        list.scrollTo({ left, behavior: "smooth" });
      }
      LXB.legend();
    },

    go(i, scroll) {
      if (i < 0 || i >= this.rows.length) return false;
      this.at = i;
      this.light();
      const pane = this.panes[i];
      if (scroll && pane) {
        this.holdSpy = performance.now() + 900;
        pane.scrollIntoView({ behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "start" });
        if (history.replaceState) history.replaceState(null, "", "#" + pane.id);
      }
      return true;
    },

    // The light follows reading: the last pane whose head has passed a line a
    // third of the way down, and the last pane of all once the page has run out.
    spy() {
      if (this.spyQueued) return;
      this.spyQueued = true;
      requestAnimationFrame(() => {
        this.spyQueued = false;
        if (performance.now() < this.holdSpy) return;
        const line = window.innerHeight * 0.34;
        let at = 0;
        this.panes.forEach((p, i) => { if (p && p.getBoundingClientRect().top <= line) at = i; });
        const end = window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 4;
        if (end) at = this.panes.length - 1;
        if (at !== this.at) {
          this.at = at;
          this.light();
        }
      });
    },

    up() { if (this.go(this.at - 1, true)) LXB.sound.play("press"); },
    down() { if (this.go(this.at + 1, true)) LXB.sound.play("press"); },
    left() { this.back(); },
    right() { return false; },
    select(from) {
      const action = this.primary();
      if (!action) return false;
      LXB.sound.play("press-selected");
      // A pad's press is not one a browser lets open a window with.
      if (action.target === "_blank" && from !== "pad") window.open(action.href, "_blank", "noopener");
      else location.href = LXB.carry(action.href);
    },
    back() {
      LXB.sound.play("press-back");
      const pane = this.panes[this.at];
      const where = this.id + (pane ? "/" + pane.id : "");
      const href = LXB.home + "index.html#" + where;
      try { sessionStorage.setItem("lxb-bar", where); } catch (e) { /* ignore */ }
      document.body.classList.add("leaving");
      setTimeout(() => { location.href = LXB.carry(href); }, window.matchMedia("(prefers-reduced-motion: reduce)").matches ? 0 : 180);
    },
    // What Select does on this row: the pane's own first action, where it has one.
    primary() {
      const pane = this.panes[this.at];
      return pane ? pane.querySelector("[data-select]") : null;
    },
    legend() { return this.primary() ? ["select", "back"] : ["back"]; },
    label(act) {
      if (act === "select") {
        const p = this.primary();
        return p ? p.dataset.select || LXB.t("open") : null;
      }
      return null;
    },

    arrive() {
      const items = document.querySelectorAll(".rows .row, .panes .pane");
      items.forEach((el, i) => {
        el.classList.add("arrive");
        el.style.setProperty("--i", String(Math.min(i, 14)));
      });
      requestAnimationFrame(() => requestAnimationFrame(() => document.body.classList.add("arrived")));
      try { sessionStorage.removeItem("lxb-arrived-by"); } catch (e) { /* ignore */ }
    },
  };

  window.LXB.whenReady(() => {
    page.build();
    page.arrive();
    LXB.screen = page;
    LXB.legend();
  });
})();

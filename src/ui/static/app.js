// terrarium UI glue. Everything is delegated from `document`, so it keeps
// working when htmx swaps the page body during boosted navigation.
(() => {
  "use strict";

  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

  // ── Relative timestamps ────────────────────────────────────────────────
  // The server renders a relative label already; this only keeps it fresh.
  function rel(iso) {
    const t = Date.parse(iso);
    if (Number.isNaN(t)) return null;
    const s = Math.round((Date.now() - t) / 1000);
    if (s < 45) return "just now";
    const m = Math.round(s / 60);
    if (m < 60) return `${m}m ago`;
    const h = Math.round(m / 60);
    if (h < 24) return `${h}h ago`;
    const d = Math.round(h / 24);
    if (d < 30) return `${d}d ago`;
    return null; // older dates keep their absolute server label
  }
  function refreshTimes() {
    for (const el of $$("time[data-rel]")) {
      const label = rel(el.getAttribute("datetime"));
      if (label) el.textContent = label;
    }
  }
  setInterval(refreshTimes, 30_000);

  // ── Command palette ────────────────────────────────────────────────────
  function palette() { return $("#palette"); }
  function openPalette() {
    const dlg = palette();
    if (!dlg || dlg.open) return;
    dlg.showModal();
    const input = $("input", dlg);
    input.value = "";
    htmx.trigger(input, "search");
    input.focus();
  }
  function paletteItems() { return $$("#palette-results a"); }
  function select(idx) {
    const items = paletteItems();
    if (!items.length) return;
    const i = (idx + items.length) % items.length;
    items.forEach((a, n) => a.setAttribute("aria-selected", n === i ? "true" : "false"));
    items[i].scrollIntoView({ block: "nearest" });
  }
  function selectedIndex() {
    return paletteItems().findIndex((a) => a.getAttribute("aria-selected") === "true");
  }

  document.addEventListener("keydown", (e) => {
    const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName) || document.activeElement?.isContentEditable;
    if ((e.key === "k" && (e.metaKey || e.ctrlKey)) || (e.key === "/" && !typing)) {
      e.preventDefault();
      openPalette();
      return;
    }
    const dlg = palette();
    if (!dlg?.open) return;
    if (e.key === "ArrowDown" || (e.key === "n" && e.ctrlKey)) { e.preventDefault(); select(selectedIndex() + 1); }
    else if (e.key === "ArrowUp" || (e.key === "p" && e.ctrlKey)) { e.preventDefault(); select(selectedIndex() - 1); }
    else if (e.key === "Enter") {
      const a = paletteItems()[Math.max(selectedIndex(), 0)];
      if (a) { e.preventDefault(); dlg.close(); a.click(); }
    }
  });

  document.addEventListener("click", (e) => {
    if (e.target.closest("[data-open-palette]")) { e.preventDefault(); openPalette(); return; }
    const dlg = palette();
    if (dlg?.open && e.target === dlg) dlg.close(); // click on backdrop
    if (dlg?.open && e.target.closest("#palette-results a")) dlg.close();
  });

  document.addEventListener("mousemove", (e) => {
    const a = e.target.closest?.("#palette-results a");
    if (a) select(paletteItems().indexOf(a));
  });

  // ── Two-step confirmation ──────────────────────────────────────────────
  // A destructive button arms on the first click and only acts on the
  // second, within a few seconds. No modal, works with the keyboard.
  document.addEventListener("click", (e) => {
    const btn = e.target.closest("button[data-confirm]");
    if (!btn) return;
    if (btn.hasAttribute("data-armed")) return; // second click: let it through
    e.preventDefault();
    e.stopImmediatePropagation();
    const label = btn.querySelector(".label");
    const original = label ? label.textContent : null;
    btn.setAttribute("data-armed", "");
    if (label) label.textContent = btn.dataset.confirm;
    setTimeout(() => {
      btn.removeAttribute("data-armed");
      if (label && original !== null) label.textContent = original;
    }, 4000);
  }, true);

  // ── htmx integration ───────────────────────────────────────────────────
  // CSRF tokens are single-use; every action response hands out the next
  // one, and all forms on the page pick it up.
  document.addEventListener("htmx:afterRequest", (e) => {
    const token = e.detail.xhr?.getResponseHeader("X-CSRF-Token");
    if (token) for (const input of $$("input[name=csrf]")) input.value = token;
  });

  document.addEventListener("toast", (e) => toast(e.detail.msg || e.detail.value, e.detail.kind));
  document.addEventListener("htmx:responseError", (e) => {
    if (e.detail.elt?.closest?.("#palette")) return;
    toast(`Request failed (${e.detail.xhr.status})`, "err");
  });

  function toast(msg, kind) {
    if (!msg) return;
    let box = $(".toasts");
    if (!box) { box = document.createElement("div"); box.className = "toasts"; document.body.append(box); }
    const el = document.createElement("div");
    el.className = "toast" + (kind === "err" ? " err" : "");
    el.setAttribute("role", "status");
    el.innerHTML = kind === "err"
      ? '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="9"/><path d="M12 8v5M12 16h.01"/></svg>'
      : '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12 5 5 9-10"/></svg>';
    el.append(document.createTextNode(msg));
    box.append(el);
    setTimeout(() => el.remove(), 3600);
  }

  // ── Copy buttons ───────────────────────────────────────────────────────
  document.addEventListener("click", async (e) => {
    const btn = e.target.closest("[data-copy]");
    if (!btn) return;
    const text = btn.dataset.copy || btn.closest(".codeblock, .secret")?.querySelector("pre, span")?.innerText || "";
    try {
      await navigator.clipboard.writeText(text.trim());
      btn.setAttribute("data-copied", "");
      const label = btn.querySelector(".label");
      const was = label?.textContent;
      if (label) label.textContent = "Copied";
      setTimeout(() => { btn.removeAttribute("data-copied"); if (label) label.textContent = was; }, 1400);
    } catch {
      toast("Clipboard unavailable", "err");
    }
  });

  // ── Resource filter ────────────────────────────────────────────────────
  document.addEventListener("input", (e) => {
    const input = e.target.closest("input[data-filter]");
    if (!input) return;
    const q = input.value.trim().toLowerCase();
    const root = $(input.dataset.filter);
    if (!root) return;
    for (const group of $$("[data-group]", root)) {
      const groupHit = group.dataset.group.toLowerCase().includes(q);
      let any = false;
      for (const li of $$("li", group)) {
        const hit = !q || groupHit || li.textContent.toLowerCase().includes(q);
        li.hidden = !hit;
        any ||= hit;
      }
      group.hidden = !any;
      if (q) group.open = true;
    }
  });

  // ── Dependency graph focus ─────────────────────────────────────────────
  let pinned = null;
  function focusNode(svg, id) {
    if (!svg) return;
    svg.classList.toggle("focus", !!id);
    for (const el of $$(".hi", svg)) el.classList.remove("hi");
    if (!id) return;
    const node = svg.querySelector(`.node[data-id="${CSS.escape(id)}"]`);
    node?.classList.add("hi");
    for (const edge of $$(".edge", svg)) {
      if (edge.dataset.from === id || edge.dataset.to === id) {
        edge.classList.add("hi");
        svg.querySelector(`.node[data-id="${CSS.escape(edge.dataset.from === id ? edge.dataset.to : edge.dataset.from)}"]`)?.classList.add("hi");
      }
    }
  }
  document.addEventListener("mouseover", (e) => {
    if (pinned) return;
    const node = e.target.closest?.(".graph .node");
    if (node) focusNode(node.closest("svg"), node.dataset.id);
  });
  document.addEventListener("mouseout", (e) => {
    if (pinned) return;
    const node = e.target.closest?.(".graph .node");
    if (node && !node.contains(e.relatedTarget)) focusNode(node.closest("svg"), null);
  });
  document.addEventListener("click", (e) => {
    const svg = e.target.closest?.("svg.graph");
    if (!svg) return;
    const node = e.target.closest(".node");
    pinned = node && pinned !== node.dataset.id ? node.dataset.id : null;
    focusNode(svg, pinned);
  });

  // ── Mobile menu ────────────────────────────────────────────────────────
  document.addEventListener("click", (e) => {
    if (e.target.closest("[data-menu]")) $(".side")?.classList.toggle("open");
  });
  document.addEventListener("htmx:beforeSwap", () => { pinned = null; });
})();

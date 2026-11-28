// The page-side half of the D1 design lint (serve/src/design_lint.rs). It only COLLECTS facts about the rendered page - computed colours, sizes, spacing, rects,
// clipping, interactive elements - and returns them as one JSON string; every verdict is made in Rust over that snapshot. It is the same function as `collect` in
// bench/design/shoot.mjs (the Node reference camera); the fixtures' snapshots were made by it, so change both together. It tags interactive elements with
// data-d1id so the Rust side can hover them; `design_lint::capture` removes the tags again.
(() => {
const collect = function (opts) {
  const vw = window.innerWidth, vh = window.innerHeight;
  const SKIP = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "HEAD", "META", "LINK", "TITLE", "BR", "COL", "COLGROUP"]);
  const num = (s) => { const v = parseFloat(s); return Number.isFinite(v) ? v : 0; };
  const tok = (t) => (t.endsWith("%") ? parseFloat(t) / 100 : parseFloat(t));
  const parseColor = (s) => {
    if (!s) return null; s = s.trim();
    if (s === "transparent") return [0, 0, 0, 0];
    let m = s.match(/^rgba?\(([^)]+)\)$/);
    if (m) { const p = m[1].split(/[\s,/]+/).filter(Boolean).map(tok); return [p[0], p[1], p[2], p.length > 3 ? p[3] : 1]; }
    m = s.match(/^color\(srgb\s+([^)]+)\)$/);
    if (m) { const p = m[1].split(/[\s/]+/).filter(Boolean).map(tok); return [p[0] * 255, p[1] * 255, p[2] * 255, p.length > 3 ? p[3] : 1]; }
    return null; // lab()/oklch()/display-p3: unknown, never guessed
  };
  const over = (c, base) => [0, 1, 2].map((k) => c[k] * c[3] + base[k] * (1 - c[3]));
  const sel = (el) => {
    let s = el.tagName.toLowerCase(); if (el.id) s += "#" + el.id;
    const cl = (typeof el.className === "string" ? el.className : "").trim().split(/\s+/).filter(Boolean).slice(0, 2); if (cl.length) s += "." + cl.join(".");
    return s;
  };
  const txt = (el) => (el.innerText || el.textContent || "").replace(/\s+/g, " ").trim().slice(0, 40);
  const directText = (el) => Array.from(el.childNodes).some((n) => n.nodeType === 3 && n.textContent.trim().length > 0);
  const effOpacity = (el) => { let o = 1; for (let n = el; n && n.nodeType === 1; n = n.parentElement) { const v = parseFloat(getComputedStyle(n).opacity); o *= Number.isFinite(v) ? v : 1; } return o; };
  const bgOf = (el) => {
    const layers = [];
    for (let n = el; n && n.nodeType === 1; n = n.parentElement) {
      const cs = getComputedStyle(n);
      if (cs.backgroundImage && cs.backgroundImage !== "none") return null; // gradient / image: unknown
      const c = parseColor(cs.backgroundColor);
      if (c === null) return null;
      if (c[3] > 0) { layers.push(c); if (c[3] >= 0.999) break; }
    }
    let base = [255, 255, 255];
    for (let i = layers.length - 1; i >= 0; i--) base = over(layers[i], base);
    return base.map(Math.round);
  };
  const shown = (el, cs, r) => {
    if (cs.display === "none" || cs.visibility === "hidden" || cs.visibility === "collapse") return false;
    if (r.width < 1 || r.height < 1) return false;
    if (r.right <= 0 || r.left >= vw || r.bottom <= 0) return false; // off to the side (a closed drawer) or above the page
    if (el.closest('[aria-hidden="true"],[inert]')) return false;
    return true;
  };

  const els = [document.body, ...Array.from(document.body.querySelectorAll("*"))].filter((e) => !SKIP.has(e.tagName) && !(e.closest("svg") && e.tagName.toLowerCase() !== "svg"));
  const out = { viewport: { w: vw, h: vh }, doc: { scrollW: document.documentElement.scrollWidth, scrollH: document.documentElement.scrollHeight, hscroll: document.documentElement.scrollWidth > vw + 1 }, title: document.title, blank: document.body.innerText.trim().length < 5, elements: 0, texts: [], spacing: [], colors: [], containers: [], clipped: [], offscreen: [], tap: [], interactive: [], defaults: [], defaultsOf: 0 };
  const spacing = new Map();
  const bump = (v, selr) => { const k = Math.round(v * 10) / 10; const e = spacing.get(k) || { v: k, n: 0, sel: [] }; e.n++; if (e.sel.length < 3 && !e.sel.includes(selr)) e.sel.push(selr); spacing.set(k, e); };
  const SCROLLERS = (el) => { for (let n = el.parentElement; n; n = n.parentElement) { const cs = getComputedStyle(n); if (/(auto|scroll|hidden|clip)/.test(cs.overflowX)) return true; } return false; };
  const INTER = 'a[href],button,input:not([type="hidden"]),select,textarea,summary,[role="button"],[role="link"],[role="tab"],[role="menuitem"],[role="checkbox"],[role="switch"]';
  const UA_TYPES = new Set(["text", "email", "search", "tel", "url", "password", "number", "submit", "button", "reset", ""]);
  let uid = 0;
  const sigOf = (el) => { const cs = getComputedStyle(el); return ["color", "backgroundColor", "borderTopColor", "borderTopWidth", "borderRightColor", "boxShadow", "outlineStyle", "outlineWidth", "outlineColor", "textDecorationLine", "transform", "opacity", "filter"].map((k) => cs[k]).join("|"); };

  for (const el of els) {
    const cs = getComputedStyle(el), r = el.getBoundingClientRect();
    if (el !== document.body && (cs.display === "none")) continue;
    if (cs.display === "contents") continue;
    const sh = shown(el, cs, r) && effOpacity(el) > 0.05;
    out.elements++;
    const selr = sel(el);
    if (!sh) continue;
    // spacing: margins, paddings, gaps of the boxes the screenshot shows (a horizontal margin > 64px is taken for auto-centring and skipped)
    {
      const m = [num(cs.marginTop), num(cs.marginRight), num(cs.marginBottom), num(cs.marginLeft)], p = [num(cs.paddingTop), num(cs.paddingRight), num(cs.paddingBottom), num(cs.paddingLeft)];
      const auto = m[1] > 64 || m[3] > 64;
      m.forEach((v, i) => { if (v > 0 && !(auto && (i === 1 || i === 3))) bump(v, selr); });
      p.forEach((v) => { if (v > 0) bump(v, selr); });
      if (cs.display.includes("flex") || cs.display.includes("grid")) { [cs.rowGap, cs.columnGap].forEach((g) => { const v = num(g); if (v > 0 && g !== "normal") bump(v, selr); }); }
    }
    // text
    const isField = el.tagName === "INPUT" || el.tagName === "SELECT" || el.tagName === "TEXTAREA";
    if ((directText(el) || (isField && el.type !== "checkbox" && el.type !== "radio" && el.type !== "range" && el.type !== "file")) && !el.disabled && out.texts.length < 900) {
      const fgc = parseColor(cs.color), bg = bgOf(el), eo = effOpacity(el);
      let fg = null;
      if (fgc && bg) fg = over([fgc[0], fgc[1], fgc[2], fgc[3] * eo], bg).map(Math.round);
      out.texts.push({ sel: selr, text: isField ? (el.value || el.placeholder || "[field]").slice(0, 40) : txt(el), size: Math.round(num(cs.fontSize) * 10) / 10, weight: parseInt(cs.fontWeight, 10) || 400, fg, bg, x: Math.round(r.x), y: Math.round(r.y + window.scrollY), w: Math.round(r.width), h: Math.round(r.height) });
      if (fgc && fgc[3] > 0.05 && out.colors.length < 2500) out.colors.push({ rgb: (fg || fgc.slice(0, 3)).map(Math.round), role: "text", sel: selr });
    }
    // colours: backgrounds and drawn borders
    const bgc = parseColor(cs.backgroundColor);
    if (bgc && bgc[3] >= 0.5 && out.colors.length < 2500) out.colors.push({ rgb: bgc.slice(0, 3).map(Math.round), role: "bg", sel: selr });
    if (num(cs.borderTopWidth) > 0 && cs.borderTopStyle !== "none" && out.colors.length < 2500) { const bc = parseColor(cs.borderTopColor); if (bc && bc[3] >= 0.5) out.colors.push({ rgb: bc.slice(0, 3).map(Math.round), role: "border", sel: selr }); }
    // alignment: containers with >= 3 in-flow block children
    if (out.containers.length < 160 && cs.display !== "inline") {
      const kids = Array.from(el.children).filter((c) => { if (SKIP.has(c.tagName)) return false; const k = getComputedStyle(c); if (k.display === "none" || k.display === "inline" || k.display === "contents" || k.position === "absolute" || k.position === "fixed") return false; const kr = c.getBoundingClientRect(); return kr.width >= 1 && kr.height >= 1 && kr.right > 0 && kr.left < vw && !c.closest('[aria-hidden="true"]'); });
      if (kids.length >= 3) out.containers.push({ sel: selr, align: `${cs.alignItems} ${cs.textAlign}`, kids: kids.map((c) => { const kr = c.getBoundingClientRect(); return { x: kr.x, y: kr.y + window.scrollY, w: kr.width, h: kr.height }; }) });
    }
    // overflow / clipping
    if (el !== document.body && el !== document.documentElement && cs.display !== "inline") {
      const cx = cs.overflowX === "hidden" || cs.overflowX === "clip", cy = cs.overflowY === "hidden" || cs.overflowY === "clip";
      if (cx || cy) {
        const ell = cs.textOverflow === "ellipsis";
        let hit = null;
        if (directText(el)) {
          if (cx && !ell && el.scrollWidth > el.clientWidth + 1) hit = { kind: "clip-x", scroll: el.scrollWidth, client: el.clientWidth };
          else if (cy && el.scrollHeight > el.clientHeight + 1) hit = { kind: "clip-y", scroll: el.scrollHeight, client: el.clientHeight };
        }
        if (!hit) {
          let n = 0;
          for (const d of el.querySelectorAll("*")) {
            if (++n > 200) break;
            if (!directText(d)) continue;
            const dr = d.getBoundingClientRect(); if (dr.width < 1) continue;
            if ((cx && !ell && (dr.right > r.right + 1 || dr.left < r.left - 1)) || (cy && (dr.bottom > r.bottom + 1 || dr.top < r.top - 1))) { hit = { kind: "clip-child", child: sel(d) }; break; }
          }
        }
        if (hit && out.clipped.length < 20) out.clipped.push({ sel: selr, text: txt(el), ...hit });
      }
    }
    if (r.right > vw + 1 && cs.position !== "fixed" && !SCROLLERS(el) && !el.closest("svg") && out.offscreen.length < 10) out.offscreen.push({ sel: selr, right: Math.round(r.right), viewportW: vw });
    // interactive: tap targets, state probe list
    if (el.matches(INTER)) {
      let box = r, kind = el.tagName === "A" ? "link" : el.tagName === "BUTTON" || el.getAttribute("role") === "button" || el.getAttribute("role") === "tab" ? "button" : "field";
      if (el.tagName === "INPUT" && (el.type === "checkbox" || el.type === "radio") && el.labels && el.labels[0]) { const lr = el.labels[0].getBoundingClientRect(); if (lr.width >= 1 && lr.height >= 1) box = lr; }
      const inlineInText = cs.display === "inline" && el.tagName === "A" && el.parentElement && el.parentElement.textContent.trim().length > el.textContent.trim().length + 3;
      if (!inlineInText) out.tap.push({ sel: selr, text: txt(el) || el.getAttribute("aria-label") || el.getAttribute("title") || "", w: box.width, h: box.height });
      if (out.interactive.length < 40) { const id = String(uid++); el.setAttribute("data-d1id", id); out.interactive.push({ id, sel: selr, kind, sig: sigOf(el) }); }
    }
    // user-agent leftovers on form controls
    const isCtl = el.tagName === "BUTTON" || el.tagName === "SELECT" || el.tagName === "TEXTAREA" || (el.tagName === "INPUT" && UA_TYPES.has(el.type));
    if (isCtl) {
      out.defaultsOf++;
      const fs13 = cs.fontSize === "13.3333px", bg239 = cs.backgroundColor === "rgb(239, 239, 239)", bd118 = /rgb\((118, 118, 118|133, 133, 133)\)/.test(cs.borderTopColor), bevel = cs.borderTopStyle === "outset" || cs.borderTopStyle === "inset";
      if ((fs13 && (bg239 || bd118 || bevel)) || (bg239 && bevel)) out.defaults.push({ sel: selr, tag: el.tagName.toLowerCase(), fontSize: cs.fontSize, background: cs.backgroundColor, border: `${cs.borderTopWidth} ${cs.borderTopStyle} ${cs.borderTopColor}` });
    }
  }
  out.spacing = Array.from(spacing.values());
  return out;
};
return JSON.stringify(collect({}));
})()

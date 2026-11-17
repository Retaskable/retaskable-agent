// The accessibility sweep's browser (src/jobs/a11y_browser.rs): one long-lived node process that the agent talks
// to in line-delimited JSON on stdin/stdout. Ported from the TypeScript agent's a11yScan.ts, a11yPage.ts and
// a11yChecks.ts (the page collectors); the judging stays in Rust.
//
// Request:  {"id": n, "method": "...", "params": {...}}   one per line
// Response: {"id": n, "result": ...}  or  {"id": n, "error": {"kind": "...", "message": "..."}}
// `kind` is "modules" (playwright-core or axe-core not found), "no_chrome" or "other". stdout carries nothing but
// responses; anything else this process prints goes to stderr. The process closes its browser and exits when stdin
// ends, so it never outlives the agent. `playwright-core` and `axe-core` resolve through NODE_PATH.
"use strict";
const fs = require("node:fs");
const readline = require("node:readline");

const VIEWPORTS = { phone: { width: 390, height: 844 }, desktop: { width: 1440, height: 900 } };
const NO_CHROME = /executable doesn't exist|is not found at|npx playwright install/i;
const PREFS_KEY = "retaskable.expo.ui";

class Failure extends Error {
  constructor(kind, message) {
    super(message);
    this.kind = kind;
  }
}

const state = { browser: null, base: "", prefs: null, axe: "", pages: new Map(), walks: new Map(), nextPage: 1 };

const isSample = (route) => route.includes("?sample");

// ---- The collectors: they run in the page and return plain data (a11yChecks.ts) ----

function collectReflow() {
  return { scrollWidth: document.documentElement.scrollWidth, clientWidth: document.documentElement.clientWidth };
}

function collectOverflows() {
  const ids = (window.__a11yIds = window.__a11yIds || new WeakMap());
  const out = [];
  for (const el of Array.from(document.body.querySelectorAll("*"))) {
    const cs = getComputedStyle(el);
    if (!["hidden", "clip"].includes(cs.overflowX) && !["hidden", "clip"].includes(cs.overflowY)) continue;
    if (el.scrollWidth <= el.clientWidth + 1 && el.scrollHeight <= el.clientHeight + 1) continue;
    const own = Array.from(el.childNodes).some((n) => n.nodeType === 3 && n.textContent.trim() !== "");
    if (!own) continue;
    if (!ids.has(el)) ids.set(el, (window.__a11yN = (window.__a11yN || 0) + 1));
    const clamp = cs.webkitLineClamp !== undefined ? cs.webkitLineClamp : cs.getPropertyValue("-webkit-line-clamp");
    out.push({
      element: ids.get(el),
      role: el.getAttribute("role") || el.tagName.toLowerCase(),
      name: (el.getAttribute("aria-label") || el.textContent || "").trim().slice(0, 60),
      ellipsis: cs.textOverflow === "ellipsis" || (clamp !== undefined && clamp !== "none" && clamp !== ""),
    });
  }
  return out;
}

const SPACING_CSS =
  '* { line-height: 1.5 !important; letter-spacing: 0.12em !important; word-spacing: 0.16em !important } p, [role="paragraph"] { margin-bottom: 2em !important }';

function collectTitle() {
  return document.title;
}

function collectFocused() {
  const w = globalThis;
  const state = (w.__rtWalk = w.__rtWalk || { ids: new WeakMap(), n: 0, prev: null });
  const styles = (el) => {
    const c = w.getComputedStyle(el);
    return { outlineStyle: c.outlineStyle, outlineWidth: c.outlineWidth, outlineColor: c.outlineColor, boxShadow: c.boxShadow, borderColor: c.borderColor, backgroundColor: c.backgroundColor };
  };
  const roleOf = (el) => el.getAttribute("role") || el.tagName.toLowerCase();
  const nameOf = (el) => (el.getAttribute("aria-label") || el.innerText || el.value || el.getAttribute("title") || "").replace(/\s+/g, " ").trim().slice(0, 60);
  const prev = state.prev ? styles(state.prev) : null;
  const el = w.document.activeElement;
  state.prev = el && el !== w.document.body && el !== w.document.documentElement ? el : null;
  const zero = { x: 0, y: 0, width: 0, height: 0 };
  if (!state.prev) {
    const none = styles(w.document.body);
    return { prev, step: { element: null, role: "page", name: "", box: zero, inViewport: true, focused: none, hits: [] } };
  }
  const cur = state.prev;
  if (!state.ids.has(cur)) state.ids.set(cur, ++state.n);
  const r = cur.getBoundingClientRect();
  const vw = w.innerWidth, vh = w.innerHeight;
  const inViewport = r.width > 0 && r.height > 0 && r.right > 0 && r.bottom > 0 && r.left < vw && r.top < vh;
  const clamp = (v, max) => Math.min(Math.max(v, 0), max - 1);
  const points = [[r.x + r.width / 2, r.y + r.height / 2], [r.left + 2, r.top + 2], [r.right - 2, r.top + 2], [r.left + 2, r.bottom - 2], [r.right - 2, r.bottom - 2]];
  const hits = points.map(([x, y]) => {
    const hit = w.document.elementFromPoint(clamp(x, vw), clamp(y, vh));
    if (!hit) return null;
    return { onElement: hit === cur || cur.contains(hit), role: roleOf(hit), name: nameOf(hit) };
  });
  return { prev, step: { element: state.ids.get(cur), role: roleOf(cur), name: nameOf(cur), box: { x: r.x, y: r.y, width: r.width, height: r.height }, inViewport, focused: styles(cur), hits } };
}

function blurAndRead() {
  const w = globalThis;
  const prev = (w.__rtWalk && w.__rtWalk.prev) || null;
  if (w.document.activeElement && w.document.activeElement.blur) w.document.activeElement.blur();
  if (!prev) return null;
  const c = w.getComputedStyle(prev);
  return { outlineStyle: c.outlineStyle, outlineWidth: c.outlineWidth, outlineColor: c.outlineColor, boxShadow: c.boxShadow, borderColor: c.borderColor, backgroundColor: c.backgroundColor };
}

function collectCandidates() {
  const w = globalThis;
  const state = (w.__rtWalk = w.__rtWalk || { ids: new WeakMap(), n: 0, prev: null });
  const roles = "button link checkbox switch radio tab menuitem option combobox".split(" ").map((r) => `[role="${r}"]`);
  const selector = ["a[href]", "button", "input", "select", "textarea", "summary", "[tabindex]", ...roles].join(",");
  const modal = w.document.querySelector('[aria-modal="true"], dialog[open]');
  const out = [];
  for (const el of w.document.querySelectorAll(selector)) {
    const tabindex = el.getAttribute("tabindex");
    const role = el.getAttribute("role");
    if (tabindex !== null && Number(tabindex) < 0 && !role && !/^(A|BUTTON|INPUT|SELECT|TEXTAREA|SUMMARY)$/.test(el.tagName)) continue;
    if (el.type === "hidden" || el.disabled || el.getAttribute("aria-disabled") === "true") continue;
    if (el.closest("[inert], [aria-hidden='true']")) continue;
    if (modal && !modal.contains(el)) continue;
    const c = w.getComputedStyle(el);
    const r = el.getBoundingClientRect();
    if (c.display === "none" || c.visibility === "hidden" || r.width === 0 || r.height === 0) continue;
    if (!state.ids.has(el)) state.ids.set(el, ++state.n);
    const name = (el.getAttribute("aria-label") || el.innerText || el.value || el.getAttribute("title") || "").replace(/\s+/g, " ").trim().slice(0, 60);
    out.push({ element: state.ids.get(el), role: role || el.tagName.toLowerCase(), name });
  }
  return out;
}

function runAxe(runOnly) {
  return globalThis.axe.run(globalThis.document, { runOnly }).then((r) => ({ passed: r.passes.map((p) => p.id), failed: r.violations.map((v) => v.id) }));
}

function blurActive() {
  const a = globalThis.document.activeElement;
  if (a && a.blur) a.blur();
}

function twoFrames() {
  return new Promise((done) => globalThis.requestAnimationFrame(() => globalThis.requestAnimationFrame(() => done())));
}

// ---- Pages (a11yScan.ts's realSweep, a11yPage.ts) ----

async function withPrefs(page, scheme) {
  // `scheme` is forced to match the page's theme: a saved scheme must never win over it.
  if (state.prefs) await page.addInitScript(([key, prefs]) => localStorage.setItem(key, JSON.stringify(prefs)), [PREFS_KEY, { ...state.prefs, scheme }]);
}

async function launch(params) {
  let playwright;
  let axePath;
  try {
    playwright = require("playwright-core");
    axePath = require.resolve("axe-core/axe.min.js");
  } catch (err) {
    throw new Failure("modules", String((err && err.message) || err));
  }
  const chromium = playwright.chromium;
  const exe = process.env.CHROME_PATH || undefined;
  // The installed Chrome first (as the TypeScript agent), then Playwright's own Chromium (the Docker image has it).
  const attempts = exe ? [{ executablePath: exe }] : [{ channel: "chrome" }, {}];
  const errors = [];
  for (const opts of attempts) {
    try {
      state.browser = await chromium.launch({ headless: true, ...opts });
      break;
    } catch (err) {
      errors.push(String((err && err.message) || err));
    }
  }
  if (!state.browser) {
    // No Chrome only when every attempt said so; otherwise the first other error is the real one.
    const other = errors.find((m) => !NO_CHROME.test(m));
    throw new Failure(other === undefined ? "no_chrome" : "other", other === undefined ? errors[0] : other);
  }
  state.base = params.base;
  state.prefs = params.samplePrefs || null;
  state.axe = fs.readFileSync(axePath, "utf8");
  for (const size of Object.keys(VIEWPORTS)) {
    for (const theme of ["light", "dark"]) {
      const page = await state.browser.newPage({ viewport: VIEWPORTS[size], colorScheme: theme });
      await withPrefs(page, theme);
      state.pages.set(`${size} ${theme}`, page);
    }
  }
  return {};
}

async function pass({ route, size, theme, runOnly }) {
  const page = state.pages.get(`${size} ${theme}`);
  if (!page) throw new Failure("other", `no page for ${size} ${theme}`);
  await page.goto(state.base + route, { waitUntil: "load" });
  await page.addScriptTag({ content: state.axe });
  return page.evaluate(runAxe, runOnly);
}

/** A fresh light page of `viewport` (a11yPage.ts's newLightPage); `time` is the Sample's fake clock when one is wanted. */
async function lightPage(viewport, time) {
  const page = await state.browser.newPage({ viewport, colorScheme: "light" });
  try {
    if (time) await page.clock.install({ time: new Date(time) });
    await withPrefs(page, "light");
  } catch (err) {
    await page.close().catch(() => undefined);
    throw err;
  }
  return page;
}

/** Loads a route as section 3.6.2 'The start': a Sample route waits for the network, then runs its clock 3 seconds. */
async function openRoute(page, route, sample) {
  if (sample) {
    await page.goto(state.base + route, { waitUntil: "networkidle" });
    await page.clock.runFor(3000);
  } else {
    await page.goto(state.base + route, { waitUntil: "load" });
  }
}

async function settle(page, sample) {
  if (sample) await page.clock.runFor(50);
  else await page.evaluate(twoFrames);
}

async function withLayoutPage(viewport, route, fn) {
  const sample = isSample(route);
  const page = await lightPage(viewport, sample ? "2026-01-01T09:00:00Z" : undefined);
  try {
    await openRoute(page, route, sample);
    return await fn(page, sample);
  } finally {
    await page.close().catch(() => undefined);
  }
}

const reflow = ({ route }) => withLayoutPage({ width: 320, height: 256 }, route, (page) => page.evaluate(collectReflow));
const title = ({ route }) => withLayoutPage(VIEWPORTS.desktop, route, (page) => page.evaluate(collectTitle));
const spacing = ({ route }) =>
  withLayoutPage(VIEWPORTS.phone, route, async (page, sample) => {
    const before = await page.evaluate(collectOverflows);
    await page.addStyleTag({ content: SPACING_CSS });
    await settle(page, sample);
    return { before, after: await page.evaluate(collectOverflows) };
  });

// The Tab walk's page (walkChecks): a desktop light page of its own, loaded and blurred; the Rust side numbers the
// presses and judges them.
async function walkOpen({ route, sample }) {
  const page = await lightPage(VIEWPORTS.desktop, sample ? "2026-01-15T10:00:00Z" : undefined);
  const id = state.nextPage++;
  state.walks.set(id, { page, sample });
  try {
    await page.goto(state.base + route, { waitUntil: sample ? "networkidle" : "load" });
    if (sample) await page.clock.runFor(3000);
    await page.evaluate(blurActive);
  } catch (err) {
    await walkClose({ page: id });
    throw err;
  }
  return { page: id };
}

function walkPage(id) {
  const w = state.walks.get(id);
  if (!w) throw new Failure("other", `no walk page ${id}`);
  return w;
}

async function walkCandidates({ page }) {
  return walkPage(page).page.evaluate(collectCandidates);
}

async function press(w, key) {
  await w.page.keyboard.press(key);
  await settle(w.page, w.sample);
  return w.page.evaluate(collectFocused);
}

const walkPress = ({ page, key }) => press(walkPage(page), key);

async function walkEscapeTab({ page }) {
  const w = walkPage(page);
  await w.page.keyboard.press("Escape");
  await settle(w.page, w.sample);
  return press(w, "Tab");
}

const walkBlur = ({ page }) => walkPage(page).page.evaluate(blurAndRead);

async function walkClose({ page }) {
  const w = state.walks.get(page);
  state.walks.delete(page);
  if (w) await w.page.close().catch(() => undefined);
  return {};
}

const METHODS = { launch, pass, reflow, title, spacing, walkOpen, walkCandidates, walkPress, walkEscapeTab, walkBlur, walkClose };

// ---- The protocol ----

function send(message) {
  process.stdout.write(JSON.stringify(message) + "\n");
}

async function handle(line) {
  let req;
  try {
    req = JSON.parse(line);
  } catch (err) {
    process.stderr.write(`a11y-browser: unreadable request: ${line.slice(0, 200)}\n`);
    return;
  }
  try {
    const method = METHODS[req.method];
    if (!method) throw new Failure("other", `unknown method ${req.method}`);
    send({ id: req.id, result: await method(req.params || {}) });
  } catch (err) {
    send({ id: req.id, error: { kind: (err && err.kind) || "other", message: String((err && err.message) || err) } });
  }
}

const lines = readline.createInterface({ input: process.stdin });
lines.on("line", (line) => {
  if (line.trim() !== "") void handle(line);
});
lines.on("close", async () => {
  if (state.browser) await state.browser.close().catch(() => undefined);
  process.exit(0);
});

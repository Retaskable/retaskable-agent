// Retaskable flags client (docs/specs/release-flags.md section 3.5, "A customer's app"). One file, no imports:
// copy it into your app's source.
//
// React (web or React Native), with the project id and the project's flag key from its settings:
//
//   const flags = createFlagClient({ project: "my-project", key: "rfk_..." });
//   export const useFlag = (name: string) => useSyncExternalStore(flags.subscribe, () => flags.on(name));
//   // <Checkout /> only when useFlag("new-checkout") is true; a flag is Off until there is an answer.
//
// Web: nothing else. Values are kept in localStorage, and a tester link (`?rf=<token>`) in the page's URL is read
// when the client is created.
//
// Expo / React Native: pass AsyncStorage, and hand tester links to openLink:
//
//   import AsyncStorage from "@react-native-async-storage/async-storage";
//   import * as Linking from "expo-linking";
//   const flags = createFlagClient({ project, key, storage: AsyncStorage });
//   Linking.getInitialURL().then((url) => url && flags.openLink(url));
//   Linking.addEventListener("url", (e) => flags.openLink(e.url));
//
// Signed-in users: call flags.setSubject(user.id), so one person sees the same flags on every device.

export interface FlagStorage {
  get(k: string): string | null | Promise<string | null>;
  set(k: string, v: string): void | Promise<void>;
}

export interface FlagEnv {
  /** The page's URL query (`?rf=…`); default `location.search` where there is one. */
  search?: string;
  setTimeout?: (fn: () => void, ms: number) => unknown;
  clearTimeout?: (id: unknown) => void;
}

export interface FlagClientOptions {
  project: string;
  key: string;
  base?: string;
  storage?: FlagStorage;
  fetch?: (url: string, init: { headers: Record<string, string> }) => Promise<{ ok: boolean; json(): Promise<unknown> }>;
  env?: FlagEnv;
  /** Milliseconds since 1970. */
  now?: () => number;
}

export interface FlagClient {
  /** Resolves once the subject, the saved answer and the saved links are read from storage. */
  ready: Promise<void>;
  on(key: string): boolean;
  subscribe(fn: () => void): () => void;
  /** Changes whenever a flag's value does. */
  version(): number;
  refresh(): Promise<void>;
  setSubject(id: string): Promise<void>;
  openLink(url: string): Promise<void>;
}

const SUBJECT_KEY = "retaskable.flags.subject";
const CACHE_KEY = "retaskable.flags.cache";
const LINKS_KEY = "retaskable.flags.links";
const LINKS_KEPT = 20;
const TOKEN = /^[A-Za-z0-9_-]{16,100}$/;

function tokenOf(url: string): string | null {
  const m = /[?&]rf=([^&#]*)/.exec(url);
  return m && TOKEN.test(m[1]) ? m[1] : null;
}

function randomId(): string {
  const c = (globalThis as { crypto?: { randomUUID?: () => string } }).crypto;
  if (c?.randomUUID) return c.randomUUID();
  return Array.from({ length: 4 }, () => Math.random().toString(36).slice(2, 10)).join("");
}

function defaultStorage(): FlagStorage {
  const ls = (globalThis as { localStorage?: { getItem(k: string): string | null; setItem(k: string, v: string): void } }).localStorage;
  if (ls) return { get: (k) => ls.getItem(k), set: (k, v) => ls.setItem(k, v) };
  const m = new Map<string, string>();
  return { get: (k) => m.get(k) ?? null, set: (k, v) => void m.set(k, v) };
}

export function createFlagClient(opts: FlagClientOptions): FlagClient {
  const base = (opts.base ?? "https://retaskable.com").replace(/\/+$/, "");
  const storage = opts.storage ?? defaultStorage();
  const doFetch = opts.fetch ?? ((url, init) => (globalThis as unknown as { fetch: NonNullable<FlagClientOptions["fetch"]> }).fetch(url, init));
  const now = opts.now ?? Date.now;
  const g = globalThis as { location?: { search?: string }; setTimeout: (fn: () => void, ms: number) => unknown; clearTimeout: (id: unknown) => void };
  const setTimer = opts.env?.setTimeout ?? g.setTimeout.bind(g);
  const clearTimer = opts.env?.clearTimeout ?? g.clearTimeout.bind(g);

  let flags: Record<string, boolean> = {};
  let ttl = 300;
  let fetchedAt = 0;
  let subject = "";
  let links: string[] = [];
  let fresh: string | null = null; // a token to send as `new=` once
  let version = 0;
  let timer: unknown = null;
  const listeners = new Set<() => void>();

  const save = (k: string, v: string) => Promise.resolve(storage.set(k, v)).catch(() => {});
  const notify = () => listeners.forEach((fn) => fn());

  function apply(next: Record<string, boolean>) {
    if (JSON.stringify(next) === JSON.stringify(flags)) return;
    flags = next;
    version++;
    notify();
  }

  function addLink(token: string) {
    if (links.includes(token)) return false;
    links = [...links, token].slice(-LINKS_KEPT);
    void save(LINKS_KEY, JSON.stringify(links));
    return true;
  }

  const ready = (async () => {
    try {
      subject = (await storage.get(SUBJECT_KEY)) ?? "";
      if (!subject) {
        subject = randomId();
        await save(SUBJECT_KEY, subject);
      }
      const saved = JSON.parse((await storage.get(LINKS_KEY)) ?? "[]");
      if (Array.isArray(saved)) links = saved.filter((t) => typeof t === "string" && TOKEN.test(t)).slice(-LINKS_KEPT);
      const cache = JSON.parse((await storage.get(CACHE_KEY)) ?? "null");
      if (cache && typeof cache.flags === "object" && cache.flags && typeof cache.ttl === "number") {
        ttl = cache.ttl;
        fetchedAt = Number(cache.at) || 0;
        apply(cache.flags);
      }
    } catch {
      // Unreadable storage: start with every flag Off.
    }
    if (!subject) subject = randomId();
    const t = tokenOf(opts.env?.search ?? g.location?.search ?? "");
    if (t) {
      addLink(t);
      fresh = t;
    }
  })();

  async function refresh(): Promise<void> {
    await ready;
    const sent = fresh;
    const params = [`subject=${encodeURIComponent(subject)}`, `links=${encodeURIComponent(links.join(","))}`];
    if (sent) params.push(`new=${encodeURIComponent(sent)}`);
    try {
      const res = await doFetch(`${base}/api/flags/${encodeURIComponent(opts.project)}?${params.join("&")}`, {
        headers: { Authorization: `Bearer ${opts.key}` },
      });
      if (!res.ok) return;
      const body = (await res.json()) as { flags?: Record<string, unknown>; ttl?: unknown };
      if (!body || typeof body.flags !== "object" || !body.flags) return;
      const next: Record<string, boolean> = {};
      for (const [k, v] of Object.entries(body.flags)) next[k] = v === true;
      if (typeof body.ttl === "number" && body.ttl > 0) ttl = body.ttl;
      fetchedAt = now();
      if (fresh === sent) fresh = null;
      void save(CACHE_KEY, JSON.stringify({ flags: next, ttl, at: fetchedAt }));
      apply(next);
    } catch {
      // Keep the last answer: Retaskable is never on the app's critical path.
    }
  }

  function schedule(delay: number) {
    if (timer !== null) clearTimer(timer);
    timer = setTimer(() => {
      timer = null;
      void refresh().then(() => {
        if (listeners.size) schedule(ttl * 1000);
      });
    }, delay);
  }

  function subscribe(fn: () => void): () => void {
    listeners.add(fn);
    if (listeners.size === 1) {
      void ready.then(() => {
        if (!listeners.size || timer !== null) return;
        schedule(fetchedAt ? Math.max(0, fetchedAt + ttl * 1000 - now()) : 0);
      });
    }
    return () => {
      listeners.delete(fn);
      if (!listeners.size && timer !== null) {
        clearTimer(timer);
        timer = null;
      }
    };
  }

  return {
    ready,
    on: (key) => flags[key] === true,
    subscribe,
    version: () => version,
    refresh,
    async setSubject(id) {
      await ready;
      if (!id || id === subject) return;
      subject = id;
      await save(SUBJECT_KEY, id);
      await refresh();
    },
    async openLink(url) {
      await ready;
      const t = tokenOf(url);
      if (!t) return;
      if (addLink(t)) fresh = t;
      if (listeners.size) await refresh();
    },
  };
}

// The Scriptorium library, served to every app at /scriptorium.js.
// Plain ES2020, no dependencies. It wraps the app API of the running
// Scriptorium: calls (the agent tools an app is offered), the app's own data,
// and change events. It also shows errors and lost connections on the page in
// plain words, and reloads the page when the app's files change.
//
//   const s = Scriptorium;
//   await s.call("read_scene", { scene: 3 })   // text; throws ScriptoriumError
//   await s.json("list_reviews")               // parsed JSON answer
//   await s.paragraphs({ scene: "KITCHEN" })   // every page of list_paragraphs
//   await s.suggest([{ paragraph_id: 41, options: ["..."] }])
//   await s.data.read("notes.json")            // parsed JSON, or null if missing
//   await s.data.blob("img/a.png")             // a Blob, or null
//   await s.data.write("notes.json", value)    // JSON, or a Blob
//   await s.data.list(); await s.data.remove("notes.json")
//   s.on("script" | "decided" | "writer" | "open", fn)
(function () {
  "use strict";

  // The app and its project come from the address: /p/<key>/<name>/...
  const m = location.pathname.match(/^\/p\/([0-9a-f]{16})\/([a-z0-9][a-z0-9-]{0,39})(\/|$)/);
  const project = m ? m[1] : null;
  const app = m ? m[2] : null;
  const notOpenPage = !!document.querySelector('meta[name="scriptorium-page"][content="not-open"]');
  const api = project && app ? `/api/p/${project}/a/${app}` : null;
  const scriptKey = `scriptorium-script:${project}`;

  class ScriptoriumError extends Error {
    constructor(message, status) {
      super(message);
      this.name = "ScriptoriumError";
      this.status = status;
    }
  }

  // ---- What the page shows: an error bar, and a full-page overlay. ----

  function onBody(fn) {
    if (document.body) fn();
    else document.addEventListener("DOMContentLoaded", fn, { once: true });
  }

  let bar = null;
  function showError(text) {
    onBody(() => {
      if (!bar) {
        bar = document.createElement("div");
        bar.setAttribute("role", "alert");
        bar.style.cssText =
          "position:fixed;top:0;left:0;right:0;z-index:2147483646;padding:8px 36px 8px 12px;" +
          "background:#b3261e;color:#fff;font:14px/1.4 system-ui,sans-serif;white-space:pre-wrap;";
        const close = document.createElement("button");
        close.textContent = "×";
        close.setAttribute("aria-label", "Close");
        close.style.cssText =
          "position:absolute;top:4px;right:8px;border:0;background:none;color:#fff;font-size:20px;cursor:pointer;";
        close.onclick = () => { bar.remove(); bar = null; };
        bar.append(document.createElement("span"), close);
        document.body.append(bar);
      }
      bar.firstChild.textContent = text;
    });
    log({ error: text });
  }

  let overlay = null;
  function showOverlay(text) {
    onBody(() => {
      if (!overlay) {
        overlay = document.createElement("div");
        overlay.style.cssText =
          "position:fixed;inset:0;z-index:2147483647;display:flex;align-items:center;justify-content:center;" +
          "padding:2em;text-align:center;background:rgba(250,250,250,0.96);color:#222;" +
          "font:18px/1.5 system-ui,sans-serif;";
        document.body.append(overlay);
      }
      overlay.textContent = text;
    });
  }

  // ---- Reporting to Scriptorium, so the agent that built the app sees it. ----

  function log(body) {
    if (!api || notOpenPage) return;
    fetch(`${api}/log`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }).catch(() => {});
  }

  function where(file, line) {
    if (!file) return "";
    const short = file.startsWith(location.origin) ? file.slice(location.origin.length) : file;
    return ` (${short}${line ? ":" + line : ""})`;
  }

  // Capturing, so failed loads of <img>, <script>, <link> are seen too:
  // they don't bubble to window.
  window.addEventListener(
    "error",
    (e) => {
      const t = e.target;
      if (t && t !== window && (t.src || t.href)) {
        const url = t.src || t.href;
        // Loads from elsewhere are blocked by the CSP and reported below.
        if (!/^https?:/.test(url) || url.startsWith(location.origin + "/")) {
          showError(`Couldn't load ${url.startsWith(location.origin) ? url.slice(location.origin.length) : url}`);
        }
        return;
      }
      if (e.message) showError(`${e.message}${where(e.filename, e.lineno)}`);
    },
    true
  );
  window.addEventListener("unhandledrejection", (e) => {
    const r = e.reason;
    const text = r && r.message ? r.message : String(r);
    showError(`Unhandled error: ${text}${r && r.stack ? where(firstFrame(r.stack)) : ""}`);
  });
  document.addEventListener("securitypolicyviolation", (e) => showError(blockedText(e)));

  // What Scriptorium's content security policy blocked, in a plain sentence.
  function blockedText(e) {
    const uri = e.blockedURI || "";
    const what = (e.effectiveDirective || e.violatedDirective || "").replace(/-src.*$/, "");
    if (uri === "eval") {
      return "This app ran code built from text (eval, new Function, or setTimeout with a string), " +
        "which Scriptorium blocks. Write the code out in a script instead.";
    }
    if (uri === "wasm-eval") return "This app tried to run WebAssembly, which Scriptorium doesn't support.";
    if (uri === "inline") {
      return `Scriptorium blocked inline code in this app (${e.effectiveDirective || e.violatedDirective}). ` +
        "Move it into a file in the app folder and load that.";
    }
    if (/^(data|blob):/.test(uri) || uri === "data" || uri === "blob") {
      return `This app tried to load a ${uri.replace(/:.*$/, "")}: address as a ${what || "resource"}, which ` +
        "Scriptorium blocks (only images may be data: or blob:). Save it as a file in the app folder and load it from there.";
    }
    if (/^(https?|wss?):/.test(uri)) {
      return `This app tried to load ${uri} from the internet, which Scriptorium blocks. ` +
        "Copy it into the app folder (for example vendor/) and load it from there.";
    }
    return `Scriptorium blocked ${uri || "a load"}${what ? ` (${what})` : ""} in this app. ` +
      "Load files only from the app folder.";
  }

  function firstFrame(stack) {
    const found = String(stack).match(/(https?:\/\/[^\s)]+?)(?::\d+)?\)?$/m);
    return found ? found[0].replace(/\)$/, "") : "";
  }

  // ---- When Scriptorium is gone, or another folder is open. ----

  let lostSince = null;
  function lost() {
    if (lostSince !== null) return;
    lostSince = Date.now();
    showOverlay("Scriptorium isn't running. This page reconnects when it's open again.");
    if (events) { events.close(); events = null; }
    const retry = () => {
      fetch("/api/status", { cache: "no-store" })
        .then(() => location.reload())
        .catch(() => setTimeout(retry, Date.now() - lostSince < 30000 ? 2000 : 10000));
    };
    setTimeout(retry, 2000);
  }

  let notOpenShown = false;
  function notOpen() {
    notOpenShown = true;
    let script = null;
    try { script = localStorage.getItem(scriptKey); } catch (_) { /* storage off */ }
    showOverlay(script ? `Open ${script} in Scriptorium to use this.` : "Open this app's script in Scriptorium.");
  }

  // ---- Requests. ----

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const answers = () => fetch("/api/status", { cache: "no-store" }).then(() => true, () => false);
  const UNAUTHORIZED = "Open this app from Scriptorium's Apps menu, with its script open.";

  async function send(url, init) {
    for (const wait of [500, 1000, 2000, null]) {
      let r;
      try {
        r = await fetch(url, Object.assign({ cache: "no-store" }, init));
      } catch (e) {
        // A failed fetch isn't always Scriptorium gone (a body the browser
        // couldn't send, say): ask it before saying so.
        if (await answers()) throw new ScriptoriumError(`Couldn't reach Scriptorium: ${e.message || e}`, 0);
        lost();
        throw new ScriptoriumError("Scriptorium isn't running.", 0);
      }
      if (r.status === 503 && wait !== null) { await sleep(wait); continue; }
      if (r.status === 503) throw new ScriptoriumError("Scriptorium is busy. Try again in a moment.", 503);
      if (r.status === 409) {
        notOpen();
        throw new ScriptoriumError("This app's script isn't open in Scriptorium.", 409);
      }
      if (r.status === 401) {
        throw new ScriptoriumError(UNAUTHORIZED, 401);
      }
      return r;
    }
  }

  function needApp() {
    if (!api) throw new ScriptoriumError("This page isn't an app: open it from Scriptorium's Apps menu.", 0);
  }

  async function call(name, args) {
    needApp();
    const r = await send(`${api}/call`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name, arguments: args || {}, project }),
    });
    if (!r.ok) throw new ScriptoriumError(`${name} failed: ${(await r.text()) || r.status}`, r.status);
    const answer = await r.json();
    if (answer.is_error) throw new ScriptoriumError(answer.text, r.status);
    return answer.text;
  }

  async function json(name, args) {
    const text = await call(name, args);
    try {
      return JSON.parse(text);
    } catch (_) {
      throw new ScriptoriumError(`${name} doesn't answer JSON; use Scriptorium.call. It said: ${text}`, 0);
    }
  }

  async function paragraphs(filters) {
    const rows = [];
    let offset = 0;
    for (;;) {
      const page = await json("list_paragraphs", Object.assign({}, filters, { offset, limit: 1000 }));
      rows.push(...page.paragraphs);
      if (!page.more || page.paragraphs.length === 0) return rows;
      offset = page.offset + page.paragraphs.length;
    }
  }

  // suggest_changes takes at most MAX_BATCH per call: larger lists go in
  // several calls, and the answers are merged with each `index` counted
  // from the start of the whole list.
  const MAX_BATCH = 200;
  async function suggest(suggestions) {
    const all = { added: 0, suggestions: [], failed: [] };
    for (let start = 0; start < suggestions.length || start === 0; start += MAX_BATCH) {
      const part = await json("suggest_changes", { suggestions: suggestions.slice(start, start + MAX_BATCH) });
      const shift = (row) => (typeof row.index === "number" ? Object.assign({}, row, { index: row.index + start }) : row);
      all.added += part.added || 0;
      all.suggestions.push(...(part.suggestions || []).map(shift));
      all.failed.push(...(part.failed || []).map(shift));
      if (suggestions.length === 0) break;
    }
    return all;
  }

  // A data file's address. An empty path would be the list, not a file.
  function dataUrl(path) {
    const parts = String(path == null ? "" : path).split("/").filter((p) => p !== "");
    if (parts.length === 0) throw new ScriptoriumError("A data file needs a name, like \"notes.json\".", 0);
    return `${api}/data/${parts.map(encodeURIComponent).join("/")}`;
  }

  async function dataGet(path) {
    needApp();
    const r = await send(dataUrl(path));
    if (r.status === 404) return null;
    if (!r.ok) throw new ScriptoriumError(`Couldn't read ${path}: ${(await r.text()) || r.status}`, r.status);
    return r;
  }

  const data = {
    async read(path) {
      const r = await dataGet(path);
      if (!r) return null;
      const text = await r.text();
      try {
        return text === "" ? null : JSON.parse(text);
      } catch (_) {
        throw new ScriptoriumError(`${path} isn't JSON; use Scriptorium.data.blob to read it.`, 0);
      }
    },
    async blob(path) {
      const r = await dataGet(path);
      return r ? r.blob() : null;
    },
    async write(path, value) {
      needApp();
      const isBlob = typeof Blob !== "undefined" && value instanceof Blob;
      const r = await send(dataUrl(path), {
        method: "PUT",
        headers: { "Content-Type": isBlob ? value.type || "application/octet-stream" : "application/json" },
        body: isBlob ? value : JSON.stringify(value),
      });
      if (r.status === 413) throw new ScriptoriumError(`${path} is too large to keep (20 MB at most).`, 413);
      if (!r.ok) throw new ScriptoriumError(`Couldn't write ${path}: ${(await r.text()) || r.status}`, r.status);
    },
    async list() {
      needApp();
      const r = await send(`${api}/data/`);
      if (!r.ok) throw new ScriptoriumError(`Couldn't list the app's data: ${r.status}`, r.status);
      return r.json();
    },
    async remove(path) {
      needApp();
      const r = await send(dataUrl(path), { method: "DELETE" });
      if (!r.ok && r.status !== 404) {
        throw new ScriptoriumError(`Couldn't remove ${path}: ${(await r.text()) || r.status}`, r.status);
      }
    },
  };

  // ---- Events. ----

  const handlers = {};
  function on(name, fn) {
    (handlers[name] = handlers[name] || []).push(fn);
  }
  function emit(name, value) {
    for (const fn of handlers[name] || []) {
      try { fn(value); } catch (e) { showError(`${e.message || e}${e.stack ? where(firstFrame(e.stack)) : ""}`); }
    }
  }

  let events = null;
  function listen() {
    if (typeof EventSource === "undefined") return;
    events = new EventSource("/api/events");
    const read = (e) => { try { return JSON.parse(e.data); } catch (_) { return {}; } };
    events.addEventListener("hello", (e) => {
      const d = read(e);
      if (d.project === project && d.script) {
        try { localStorage.setItem(scriptKey, d.script); } catch (_) { /* storage off */ }
      }
      if (project && d.project !== project) notOpen();
      else if (notOpenShown || notOpenPage) location.reload();
      emit("hello", d);
    });
    events.addEventListener("open", (e) => {
      const d = read(e);
      if (!project) return emit("open", d);
      if (d.project === project) {
        if (notOpenShown || notOpenPage) return location.reload();
      } else {
        notOpen();
      }
      emit("open", d);
    });
    for (const name of ["script", "decided", "writer"]) {
      events.addEventListener(name, (e) => emit(name, read(e)));
    }
    events.addEventListener("reload", (e) => {
      if (read(e).app === app && !notOpenPage) location.reload();
    });
    events.onerror = () => {
      // The browser would retry on its own; instead tell a full listener
      // list (503) from Scriptorium gone, and retry the stream ourselves.
      if (events) { events.close(); events = null; }
      fetch("/api/status", { cache: "no-store" })
        .then((r) => (r.status === 401 ? showOverlay(UNAUTHORIZED) : setTimeout(listen, 2000)))
        .catch(() => lost());
    };
  }

  window.Scriptorium = { app, project, call, json, paragraphs, suggest, data, on, ScriptoriumError };
  window.ScriptoriumError = ScriptoriumError;

  // `loaded` once the page and everything it loads are in: a page whose
  // own script fails still loads, so an error after this is reported too.
  if (document.readyState === "complete") log({ loaded: true });
  else window.addEventListener("load", () => log({ loaded: true }), { once: true });
  listen();
})();

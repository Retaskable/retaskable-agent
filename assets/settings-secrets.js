// The secrets card's editor (agent-settings.md section 3.3), shared by both pages and every program's tab as
// RT.secrets.editor: one row per variable, a name field and a value field side by side, a red bin on the right, and a
// "+ Add variable" button, like Portainer's environment variables; or, in advanced mode, one text box of NAME=value
// lines. A value never comes back from the server (only its fingerprint), so a saved variable shows the mask and
// nothing typed keeps it. The server checks every name and value again and is the authority; this only mirrors its
// rules (`secretRules`) so a mistake shows under the row before Save. Everything is built as text, never as markup.
(function () {
  "use strict";
  var RT = window.RT, h = RT.h;
  var MASK = "••••••";
  var NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
  var CONTROL_RE = /[\u0000-\u0008\u000a-\u001f\u007f-\u009f]/;
  var KNOWN_MAX = 4096;
  var SOURCE_WORDS = { env: "From the environment", saved: "Saved on this computer", unset: "Not set", unreadable: "Unreadable: save it again" };
  var modes = {};

  function byteLength(s) {
    return typeof TextEncoder === "function" ? new TextEncoder().encode(s).length : unescape(encodeURIComponent(s)).length;
  }

  function bin() {
    var ns = "http://www.w3.org/2000/svg";
    var svg = document.createElementNS(ns, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("width", "18");
    svg.setAttribute("height", "18");
    svg.setAttribute("aria-hidden", "true");
    ["M4 7h16", "M9 7V4h6v3", "M6 7l1 13h10l1-13", "M10 11v6", "M14 11v6"].forEach(function (d) {
      var p = document.createElementNS(ns, "path");
      p.setAttribute("d", d);
      p.setAttribute("fill", "none");
      p.setAttribute("stroke", "currentColor");
      p.setAttribute("stroke-width", "2");
      p.setAttribute("stroke-linecap", "round");
      p.setAttribute("stroke-linejoin", "round");
      svg.append(p);
    });
    return svg;
  }

  // ---- The model: one entry per variable ----
  // known: one of the program's own secrets (always a row); saved: the server has a value; fresh: added here.
  function fromRows(rows) {
    return (rows || []).map(function (r) {
      var saved = r.source === "saved" || r.source === "unreadable";
      return { name: r.name, known: !r.custom, label: r.label, use: r.use, saved: saved, source: r.source, envSet: r.envSet === true,
        fingerprint: r.fingerprint, fixed: true, value: "", gone: false, fresh: false, touched: false };
    });
  }

  function refusedWhy(rules, name) {
    var up = name.toUpperCase();
    var list = (rules && rules.refused) || [];
    for (var i = 0; i < list.length; i++) {
      if ((list[i].name && list[i].name === up) || (list[i].prefix && up.indexOf(list[i].prefix) === 0)) return name + " " + list[i].why;
    }
    return "";
  }

  // The message under an entry, or "". `all` is every entry, for duplicates.
  function problem(e, all, rules) {
    if (e.gone) return "";
    var value = e.value.trim(), name = e.name.trim();
    if (!e.fixed && !name && !e.value) return "";
    if (!e.fixed) {
      if (!name) return "Environment variable name is required";
      if (name.length > ((rules && rules.maxName) || 128)) return "A variable name is at most " + ((rules && rules.maxName) || 128) + " characters";
      if (!NAME_RE.test(name)) return "A variable name is letters, digits and underscores, and doesn't start with a digit";
      var twins = all.filter(function (o) { return o !== e && o.name.trim().toLowerCase() === name.toLowerCase(); });
      if (twins.length) return "Duplicate environment variable name";
      var why = refusedWhy(rules, name);
      if (why) return why;
      if (!value) return "Value is required";
    }
    if (!value) return "";
    if (value === MASK && !e.saved) return "Type a value: " + MASK + " stands for one already saved";
    if (e.known) {
      if (/\s/.test(value) || CONTROL_RE.test(value)) return "A token can't hold spaces or line breaks";
      if (value.length > KNOWN_MAX) return "A token is at most " + KNOWN_MAX + " characters";
    } else {
      if (CONTROL_RE.test(value)) return "A value can't hold a NUL, line break or other control character";
      if (byteLength(value) > ((rules && rules.maxValue) || 8192)) return "A value is at most " + ((rules && rules.maxValue) || 8192) + " bytes";
    }
    return "";
  }

  // ---- Advanced mode's text ----
  function toText(entries) {
    var lines = [];
    entries.forEach(function (e) {
      if (e.gone) return;
      var value = e.value.trim();
      if (e.saved && !value) lines.push(e.name + "=" + MASK);
      else if (value || (e.fresh && e.name.trim())) lines.push(e.name.trim() + "=" + value);
    });
    return lines.join("\n");
  }

  // Lines become entries: a name the editor already has keeps its entry (the mask keeps a saved value), another is
  // new, and a saved variable with no line is deleted on Save. Nothing is lost: a bad line becomes an entry with the
  // problem showing.
  function fromText(text, entries) {
    var kept = entries.filter(function (e) { return e.fixed; });
    var used = {};
    var fresh = [];
    kept.forEach(function (e) { e.gone = e.saved; e.value = ""; e.line = 0; });
    text.split(/\r?\n/).forEach(function (raw, i) {
      var line = raw.trim();
      if (!line || line.charAt(0) === "#") return;
      var at = line.indexOf("=");
      var name = at < 0 ? line : line.slice(0, at).trim();
      var value = at < 0 ? "" : line.slice(at + 1).trim();
      var own = kept.filter(function (e) { return e.name === name && !used[e.name]; })[0];
      if (own) {
        used[own.name] = true;
        own.gone = false;
        own.line = i + 1;
        own.value = own.saved && value === MASK ? "" : value;
        own.touched = true;
      } else {
        fresh.push({ name: name, known: false, saved: false, fixed: false, fresh: true, value: value, gone: false, touched: true, line: i + 1 });
      }
    });
    return kept.concat(fresh);
  }

  function editor(opts) {
    var rules = opts.rules || {};
    var entries = fromRows(opts.rows);
    var mode = modes[opts.key] === "advanced" && opts.open ? "advanced" : "simple";
    var shown = false;
    var text = "";
    var el = h("div", "envedit");
    var customCount = function () { return entries.filter(function (e) { return !e.known && !e.gone && (e.saved || e.value.trim() || e.name.trim()); }).length; };

    function paintProblems() {
      entries.forEach(function (e) {
        if (!e.msgEl) return;
        var msg = e.touched || shown ? problem(e, entries, rules) : "";
        e.msgEl.textContent = msg;
        e.msgEl.hidden = !msg;
        e.rowEl.className = "envrow" + (msg ? " bad" : "") + (e.gone ? " gone" : "");
      });
    }

    function row(e) {
      var nameIn = h("input", "");
      nameIn.type = "text";
      nameIn.autocomplete = "off";
      nameIn.spellcheck = false;
      nameIn.placeholder = "e.g. FOO";
      nameIn.value = e.name;
      nameIn.disabled = e.fixed || e.gone;
      nameIn.setAttribute("aria-label", "Variable name");
      var valIn = h("input", "");
      valIn.type = e.known ? "password" : "text";
      valIn.autocomplete = "off";
      valIn.spellcheck = false;
      valIn.placeholder = e.saved ? MASK + " saved" : e.known ? (e.envSet ? "Type to use another value" : "Paste it to save it") : "e.g. bar";
      valIn.value = e.value;
      valIn.disabled = e.gone;
      valIn.setAttribute("aria-label", "Value of " + (e.name || "the variable"));
      var del = h("button", "bin");
      del.type = "button";
      del.append(bin());
      var word = e.saved ? (e.envSet ? "Use the environment's value for " : "Clear ") : "Delete ";
      del.title = word + (e.name || "this variable");
      del.setAttribute("aria-label", word + (e.name || "this variable"));
      del.disabled = e.known && !e.saved;
      if (del.disabled) del.title = "Nothing saved to clear";
      var msg = h("p", "envmsg");
      msg.hidden = true;
      var meta = h("div", "envmeta");
      if (e.known) meta.append(h("strong", "", e.label), h("span", "hint", e.name + " · " + e.use));
      if (e.source) meta.append(h("span", e.source === "env" || e.source === "saved" ? "chip ok" : e.source === "unreadable" ? "chip bad" : "chip", SOURCE_WORDS[e.source] || e.source));
      if (e.fingerprint) meta.append(h("code", "", "sha256 " + e.fingerprint));
      if (e.gone) {
        var undo = h("button", "btn small", "Undo");
        undo.type = "button";
        undo.addEventListener("click", function () { e.gone = false; render(); });
        meta.append(h("span", "hint", e.envSet ? "Goes back to the environment's value when you save." : "Cleared when you save."), undo);
      }
      var rowEl = h("div", "envrow", h("label", "ig", h("span", "ig-l", "name"), nameIn), h("label", "ig", h("span", "ig-l", "value"), valIn), del, meta, msg);
      e.rowEl = rowEl;
      e.msgEl = msg;
      nameIn.addEventListener("input", function () { e.name = nameIn.value; e.touched = true; paintProblems(); });
      valIn.addEventListener("input", function () { e.value = valIn.value; e.touched = true; paintProblems(); });
      del.addEventListener("click", function () {
        if (e.saved) e.gone = true;
        else entries = entries.filter(function (o) { return o !== e; });
        render();
      });
      return rowEl;
    }

    function readOnly(e) {
      var now = h("div", "now", h("span", e.source === "env" || e.source === "saved" ? "chip ok" : e.source === "unreadable" ? "chip bad" : "chip", SOURCE_WORDS[e.source] || e.source));
      if (e.fingerprint) now.append(h("code", "", "sha256 " + e.fingerprint));
      return h("div", "secret", h("div", "who", h("strong", "", e.known ? e.label : e.name), h("span", "hint", e.known ? e.name + " · " + e.use : "Added here")), now);
    }

    function render() {
      var kids = [];
      if (!opts.open) {
        entries.forEach(function (e) { kids.push(readOnly(e)); });
        el.replaceChildren.apply(el, kids);
        return;
      }
      var add = h("button", "btn", "+ Add variable");
      add.type = "button";
      add.hidden = mode === "advanced";
      add.addEventListener("click", function () {
        entries.push({ name: "", known: false, saved: false, fixed: false, fresh: true, value: "", gone: false, touched: false });
        render();
      });
      var toggle = h("button", "btn", mode === "advanced" ? "Simple mode" : "Advanced mode");
      toggle.type = "button";
      toggle.addEventListener("click", function () { setMode(mode === "advanced" ? "simple" : "advanced"); });
      kids.push(h("div", "envbar", add, toggle));
      if (mode === "advanced") {
        var area = h("textarea", "mono");
        area.rows = Math.max(6, text.split("\n").length + 1);
        area.spellcheck = false;
        area.value = text;
        area.setAttribute("aria-label", "Environment variables, one NAME=value per line");
        area.addEventListener("input", function () { text = area.value; });
        var errs = h("ul", "envmsg");
        errs.id = opts.key + "-errors";
        errs.hidden = true;
        el.errorsEl = errs;
        kids.push(area, h("p", "hint", "One NAME=value per line; the value is whatever follows the first =. Lines starting with # and blank lines are ignored. " + MASK +
          " stands for a saved value and keeps it; removing a saved line deletes it when you save."), errs);
      } else {
        entries.forEach(function (e) { if (!e.gone || e.saved) kids.push(row(e)); });
        if (!entries.length) kids.push(h("p", "hint", "No variables yet."));
      }
      el.replaceChildren.apply(el, kids);
      if (mode === "simple") paintProblems();
    }

    function setMode(next) {
      if (next === mode) return;
      if (next === "advanced") {
        text = toText(entries);
        mode = "advanced";
      } else {
        entries = fromText(text, entries);
        mode = "simple";
        shown = true;
      }
      modes[opts.key] = mode;
      render();
    }

    // The changes to send: { NAME: value | null }, and what's wrong (nothing sent while anything is).
    function collect() {
      shown = true;
      if (mode === "advanced") entries = fromText(text, entries);
      var errors = [], changes = {};
      entries.forEach(function (e) {
        var msg = problem(e, entries, rules);
        if (msg) errors.push((e.line ? "Line " + e.line + ": " : "") + msg);
        else if (e.gone && e.saved) changes[e.name] = null;
        else if (e.value.trim() && e.value.trim() !== MASK) changes[e.name.trim()] = e.value.trim();
        else if (e.value.trim() === MASK && !e.saved) errors.push("Type a value for " + e.name);
      });
      if (customCount() > (rules.maxCustom || 64)) errors.push("At most " + (rules.maxCustom || 64) + " variables can be added");
      if (mode === "advanced") {
        var box = el.errorsEl;
        box.replaceChildren.apply(box, errors.map(function (m) { return h("li", "", m); }));
        box.hidden = !errors.length;
      } else paintProblems();
      return { changes: changes, errors: errors, count: Object.keys(changes).length };
    }

    render();
    return { el: el, collect: collect, mode: function () { return mode; }, setMode: setMode };
  }

  RT.secrets = { editor: editor, MASK: MASK };
})();

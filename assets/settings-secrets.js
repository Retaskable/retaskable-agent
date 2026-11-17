// The secrets card's editor (agent-settings.md section 3.3), shared by both pages and every program's tab as
// RT.secrets.editor: one row per variable, a name field and a value field side by side, a red bin on the right, and a
// "+ Add variable" button, like Portainer's environment variables. A value never comes back from the server (only its fingerprint), so a saved variable shows the mask and
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

  function editor(opts) {
    var rules = opts.rules || {};
    var entries = fromRows(opts.rows);
    var shown = false;
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
      add.addEventListener("click", function () {
        entries.push({ name: "", known: false, saved: false, fixed: false, fresh: true, value: "", gone: false, touched: false });
        render();
      });
      kids.push(h("div", "envbar", add));
      entries.forEach(function (e) { if (!e.gone || e.saved) kids.push(row(e)); });
      if (!entries.length) kids.push(h("p", "hint", "No variables yet."));
      el.replaceChildren.apply(el, kids);
      paintProblems();
    }

    // The changes to send: { NAME: value | null }, and what's wrong (nothing sent while anything is).
    function collect() {
      shown = true;
      var errors = [], changes = {};
      entries.forEach(function (e) {
        var msg = problem(e, entries, rules);
        if (msg) errors.push(msg);
        else if (e.gone && e.saved) changes[e.name] = null;
        else if (e.value.trim() && e.value.trim() !== MASK) changes[e.name.trim()] = e.value.trim();
        else if (e.value.trim() === MASK && !e.saved) errors.push("Type a value for " + e.name);
      });
      if (customCount() > (rules.maxCustom || 64)) errors.push("At most " + (rules.maxCustom || 64) + " variables can be added");
      paintProblems();
      return { changes: changes, errors: errors, count: Object.keys(changes).length };
    }

    render();
    return { el: el, collect: collect };
  }

  RT.secrets = { editor: editor, MASK: MASK };
})();

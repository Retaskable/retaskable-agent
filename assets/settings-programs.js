// The programs panel and the Chat Agent's and CI runner's settings (agent-settings.md section 4.6). Part of
// settings.html (the server puts it inside that page's second nonce'd script), sharing the helpers the first script
// publishes as window.RT. It reads GET /api/settings/programs and saves with POST /api/settings/programs/<id>; there
// is no call to a program, no start, stop or link button. An unlinked program shows the command to run in a terminal.
(function () {
  "use strict";
  var RT = window.RT, h = RT.h, $ = RT.$;
  var programs = [];
  var clearing = {};
  var STATE_WORDS = { running: "Running", stopped: "Stopped", unlinked: "Not linked", notInstalled: "Not installed on this computer" };
  var STATE_TONE = { running: "chip ok", stopped: "chip", unlinked: "chip warn", notInstalled: "chip" };
  var SECRET_WORDS = { env: "From the environment", saved: "Saved on this computer", unset: "Not set", unreadable: "Unreadable: save it again" };

  function seen(ms) {
    return ms ? "last seen " + new Date(ms).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }) : "never seen running";
  }

  // What a program reports about itself beyond its state: numbers and words, never a value that is a secret.
  function details(p) {
    var d = p.details || {}, out = [];
    if (p.version) out.push("v" + p.version);
    if (p.id === "ci-runner") {
      if (d.docker) out.push("Docker " + d.docker);
      if (d.slots !== undefined) out.push((d.slotsBusy || 0) + " of " + d.slots + " slots busy");
    } else if (p.id === "chat-agent") {
      if (d.projectsServed !== undefined) out.push(d.projectsServed + " project(s) served");
      if (d.claudeAvailable === false) out.push("no Claude Code found");
    } else if (d.projects !== undefined) {
      out.push(d.projects + " project(s)");
    }
    return out.join(" · ");
  }

  function statusRow(p) {
    var words = STATE_WORDS[p.state] || p.state;
    if (p.state === "stopped") words += ", " + seen(p.lastSeen);
    var row = h("div", "prog", h("strong", "", p.label), h("span", STATE_TONE[p.state] || "chip", words));
    var line = details(p);
    if (line && p.state !== "notInstalled") row.append(h("span", "meta", line));
    if (p.state === "unlinked") {
      row.append(h("span", "meta", "Link it from a terminal on its computer: "), h("code", "mono", p.linkCommand));
    }
    if (p.lastError) row.append(h("span", "prog-error", p.lastError));
    return row;
  }

  function renderStatus() {
    var box = $("programsList");
    box.replaceChildren.apply(box, programs.map(statusRow));
  }

  // One field of a program's settings file: the value the file has, or, when the environment sets it, the value in
  // use, locked.
  function field(p, name, label, kind, hint) {
    var s = p.settings, locked = s.locked.indexOf(name) >= 0;
    var id = "ps-" + p.id + "-" + name;
    var input = h(kind === "list" ? "textarea" : "input", "");
    input.id = id;
    if (kind === "list") input.rows = 3;
    else input.type = kind === "number" ? "number" : "text";
    var shown = locked ? s.effective[name] : s.file[name];
    input.value = Array.isArray(shown) ? shown.join("\n") : shown === null || shown === undefined ? "" : shown;
    var dflt = s.defaults[name];
    if (!locked && dflt !== undefined) input.placeholder = "Default " + dflt;
    input.disabled = locked;
    var box = h("div", "field", h("label", "", label), input);
    if (locked) box.append(h("span", "hint", "Set by " + s.env[name] + ": change it there."));
    else if (hint) box.append(h("span", "hint", hint));
    return box;
  }

  function read(p, name, kind) {
    var el = $("ps-" + p.id + "-" + name);
    if (!el || el.disabled) return undefined;
    var v = el.value.trim();
    if (kind === "list") return v ? v.split("\n").map(function (x) { return x.trim(); }).filter(Boolean) : null;
    if (!v) return null;
    return kind === "number" ? Number(v) : v;
  }

  function secretRows(p) {
    var open = p.canSaveSecrets === true && !p.secretsNote;
    var box = h("div", "");
    box.append(h("h3", "", "Secrets"), h("p", "hint", !p.canSaveSecrets
      ? "Set secrets on the computer itself: from here they'd cross the network."
      : p.secretsNote || "An environment variable wins over one saved here. A saved one reaches checks and sessions started after it."));
    p.secrets.forEach(function (t) {
      var now = h("div", "now", h("span", t.source === "env" || t.source === "saved" ? "chip ok" : t.source === "unreadable" ? "chip bad" : "chip", SECRET_WORDS[t.source] || t.source));
      if (t.fingerprint) now.append(h("code", "", "sha256 " + t.fingerprint));
      if (t.source === "env") {
        now.append(h("span", "hint", "Set by " + t.name + ": change it there."));
      } else if (open) {
        var input = h("input", "");
        input.id = "ps-secret-" + p.id + "-" + t.name;
        input.type = "password";
        input.autocomplete = "off";
        input.placeholder = t.source === "saved" ? "Empty keeps the one saved" : "Paste it to save it";
        input.setAttribute("aria-label", t.label);
        now.append(input);
        if (t.source === "saved" || t.source === "unreadable") {
          var clear = h("button", "btn", "Clear");
          clear.type = "button";
          clear.addEventListener("click", function () {
            clearing[p.id + t.name] = !clearing[p.id + t.name];
            clear.textContent = clearing[p.id + t.name] ? "Clears on Save" : "Clear";
          });
          now.append(clear);
        }
      }
      box.append(h("div", "secret", h("div", "who", h("strong", "", t.label), h("span", "hint", t.name + " · " + t.use)), now));
    });
    return box;
  }

  function card(p, fields) {
    var s = p.settings;
    var c = h("section", "card", h("h2", "", p.label));
    if (s.error) c.append(h("p", "prog-error", "The settings file can't be read, so the values below are not in use: " + s.error));
    fields.forEach(function (f) { c.append(field(p, f[0], f[1], f[2], f[3])); });
    c.append(secretRows(p));
    c.append(h("p", "hint", p.applies));
    var save = h("button", "btn save", "Save");
    save.type = "button";
    save.addEventListener("click", function () { saveProgram(p, fields); });
    c.append(save);
    return c;
  }

  function saveProgram(p, fields) {
    var settings = {}, secrets = {};
    fields.forEach(function (f) {
      var v = read(p, f[0], f[2]);
      if (v !== undefined) settings[f[0]] = v;
    });
    p.secrets.forEach(function (t) {
      if (t.source === "env") return;
      var el = $("ps-secret-" + p.id + "-" + t.name);
      var v = el ? el.value.trim() : "";
      if (v) secrets[t.name] = v;
      else if (clearing[p.id + t.name]) secrets[t.name] = null;
    });
    var body = { settings: settings };
    if (Object.keys(secrets).length) body.secrets = secrets;
    RT.setStatus("Saving…");
    RT.api("/api/settings/programs/" + p.id, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    })
      .then(function () { RT.setStatus("Saved. " + p.applies); return load(true); })
      .catch(function (e) { RT.setStatus("Couldn't save: " + e.message, true); });
  }

  var FIELDS = {
    "ci-runner": [
      ["repos", "Repos to check (one per line: owner/repo, or owner/repo@branch)", "list"],
      ["slots", "Checks at once (0 runs nothing)", "number"],
      ["cpus", "CPUs for each check", "number"],
      ["name", "Name in each status (default: this computer's name)", "text"],
    ],
    "chat-agent": [["onlyRepoId", "Serve only this repository id (empty serves every project the store names)", "text"]],
  };

  function renderCards() {
    clearing = {};
    var box = $("programCards");
    box.replaceChildren.apply(box, programs.filter(function (p) { return p.settings && FIELDS[p.id]; }).map(function (p) { return card(p, FIELDS[p.id]); }));
  }

  // The status rows are cheap to repaint on every poll; the forms only when asked, so a poll never wipes what is typed.
  function load(withCards) {
    return RT.api("/api/settings/programs").then(function (body) {
      programs = body.programs || [];
      renderStatus();
      if (withCards) renderCards();
    });
  }

  RT.programs = { load: load };
})();

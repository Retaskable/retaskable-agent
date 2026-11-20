// The program tabs (Code agent | Chat agent | CI runner) and the Chat Agent's and CI runner's status, settings and
// secrets under them (agent-settings.md sections 4.6 and 4.7). Part of
// settings.html (the server puts it inside that page's second nonce'd script), sharing the helpers the first script
// publishes as window.RT. It reads GET /api/settings/programs and saves with POST /api/settings/programs/<id>; there
// is no call to a program, no start or stop. An unlinked program has a Link button (POST /api/settings/programs/<id>/link,
// from this computer), or, where the page can't link it, the command to run in a terminal.
(function () {
  "use strict";
  var RT = window.RT, h = RT.h, $ = RT.$;
  var programs = [];
  var STATE_WORDS = { running: "Running", stopped: "Stopped", unlinked: "Not linked", notInstalled: "Not installed on this computer" };
  var STATE_TONE = { running: "chip ok", stopped: "chip", unlinked: "chip warn", notInstalled: "chip" };
  var STATE_SHORT = { running: "Running", stopped: "Stopped", unlinked: "Not linked", notInstalled: "Not installed" };

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

  // The Link button of an unlinked program; while its flow waits, the approval address and code; after one that failed,
  // why. Where the page can't link it (another computer, or its own environment holds the address) the command to run.
  var wasWorking = {};

  function linkPart(p, row) {
    var l = p.link;
    if (!l) {
      if (p.state === "unlinked") row.append(h("span", "meta", "Link it from a terminal on its computer: "), h("code", "mono", p.linkCommand));
      return;
    }
    if (l.working) {
      wasWorking[p.id] = true;
      var step = l.step && l.step.of > 1 ? "Step " + l.step.no + " of " + l.step.of + ", " + l.step.label + ". " : "";
      row.append(h("span", "meta", step), RT.approval(l.approve));
      return;
    }
    if (wasWorking[p.id] && l.linked) {
      wasWorking[p.id] = false;
      RT.setStatus("Linked the " + p.label + ". It picks this up within a minute: no restart needed.");
    }
    if (l.linked) {
      if (p.state === "unlinked") row.append(h("span", "meta", "Linked. It picks this up within a minute."));
      return;
    }
    if (l.error) row.append(h("span", "prog-error", l.error));
    if (!l.here) {
      row.append(h("span", "meta", "Link it on this computer itself, or from a terminal on its computer: "), h("code", "mono", p.linkCommand));
    } else if (l.why) {
      row.append(h("span", "meta", l.why + " "), h("code", "mono", p.linkCommand));
    } else {
      var go = h("button", "btn primary small", "Link");
      go.type = "button";
      go.addEventListener("click", function () { startLink(p, go); });
      row.append(go);
    }
  }

  function startLink(p, button) {
    button.disabled = true;
    RT.setStatus("Starting the link…");
    RT.api("/api/settings/programs/" + p.id + "/link", { method: "POST" })
      .then(function () { RT.setStatus(""); return load(false); })
      .catch(function (e) { button.disabled = false; RT.setStatus("Couldn't link: " + e.message, true); });
  }

  // While a link waits for its approval the rows are looked at every 2 seconds, not the page's 5.
  var watching = null;
  function watchLinks() {
    if (watching || !programs.some(function (p) { return p.link && p.link.working; })) return;
    watching = window.setTimeout(function () { watching = null; load(false).catch(function () {}); }, 2000);
  }

  function statusRow(p) {
    var words = STATE_WORDS[p.state] || p.state;
    if (p.state === "stopped") words += ", " + seen(p.lastSeen);
    var row = h("div", "prog", h("strong", "", p.label), h("span", STATE_TONE[p.state] || "chip", words));
    var line = details(p);
    if (line && p.state !== "notInstalled") row.append(h("span", "meta", line));
    linkPart(p, row);
    if (p.lastError) row.append(h("span", "prog-error", p.lastError));
    return row;
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

  // The secrets of a program, in the editor of settings-secrets.js: kept by program, so a repaint after a save starts fresh.
  var editors = {};

  // The CI runner on the same computer as a code agent has no secrets of its own to set: it uses the agent's (GH_TOKEN),
  // which the page's Secrets tab sets once for both. Only a server-only runner (its own trimmed page, no code agent)
  // keeps a way to have its own token.
  function ownSecrets(p) {
    return !(p.id === "ci-runner" && opts.ownView);
  }

  function secretRows(p) {
    var open = p.canSaveSecrets === true && !p.secretsNote;
    var box = h("div", "");
    var lead = p.id === "ci-runner" ? "This runner's own GitHub token, for a runner on a server with no code agent. " : "";
    box.append(h("h3", "", p.id === "ci-runner" ? "Its own token" : "Secrets"), h("p", "hint", lead + (!p.canSaveSecrets
      ? "Set secrets on the computer itself: from here they'd cross the network."
      : p.secretsNote || "Add a variable of any name, or set one the program expects. A variable saved here wins over the same environment variable, which fills it only until one is saved. A saved one reaches checks and sessions started after it, but never a check's container. Values are never shown again.")));
    editors[p.id] = RT.secrets.editor({ key: p.id, rows: p.secrets, rules: p.secretRules, open: open });
    box.append(editors[p.id].el);
    return box;
  }

  function card(p, fields) {
    var s = p.settings;
    var c = h("section", "card", h("h2", "", p.label));
    if (s.error) c.append(h("p", "prog-error", "The settings file can't be read, so the values below are not in use: " + s.error));
    fields.forEach(function (f) { c.append(field(p, f[0], f[1], f[2], f[3])); });
    if (ownSecrets(p)) c.append(secretRows(p));
    else c.append(h("p", "hint", "This runner uses the secrets set on this page's Secrets tab: set once, used by both programs."));
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
    var body = { settings: settings };
    if (ownSecrets(p)) {
      var r = editors[p.id].collect();
      if (r.errors.length) return RT.setStatus("Fix the variables marked before saving.", true);
      secrets = r.changes;
      if (r.count) body.secrets = secrets;
    }
    postProgram(p, body);
  }

  function postProgram(p, body) {
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
      // Its repos come from the agent's projects (RETASKABLE_CI_FROM_AGENT); `repos` stays in its settings file and
      // AGENT_CI_REPO only as a hidden override for a server-only runner, never a field of the page.
      ["slots", "Checks at once (0 runs nothing)", "number"],
      ["cpus", "CPUs for each check", "number"],
      ["name", "Name in each status (default: this computer's name)", "text"],
    ],
    "chat-agent": [["onlyRepoId", "Serve only this repository id (empty serves every project the store names)", "text"]],
  };

  // ---- Tabs ----
  var CODE = "code-agent";
  var opts = { own: CODE, tabs: "progTabs", ownView: null, view: "programView", key: "retaskable-program-tab" };
  var selected = null, tabsSig = null;

  // A tab for the program this page belongs to, and for each other one installed on this computer. The CI runner's
  // trimmed page has no view for the code agent, so none for it there.
  function shown(p) {
    return p.id === opts.own || (p.state !== "notInstalled" && (p.id !== CODE || !!opts.ownView));
  }

  function remembered() {
    try { return window.localStorage.getItem(opts.key); } catch (e) { return null; }
  }

  function remember(id) {
    try { window.localStorage.setItem(opts.key, id); } catch (e) { /* a private window: the tab is just not remembered */ }
  }

  function select(id, focus) {
    selected = id;
    remember(id);
    paintSelection();
    if (focus && $("ptab-" + id)) $("ptab-" + id).focus();
  }

  function paintSelection() {
    var tabs = programs.filter(shown);
    if (!tabs.some(function (p) { return p.id === selected; })) {
      var back = remembered();
      selected = tabs.some(function (p) { return p.id === back; }) ? back : opts.own;
    }
    tabs.forEach(function (p) {
      var t = $("ptab-" + p.id);
      if (t) {
        t.setAttribute("aria-selected", String(p.id === selected));
        t.tabIndex = p.id === selected ? 0 : -1;
      }
      var panel = $("pp-" + p.id);
      if (panel) panel.hidden = p.id !== selected;
    });
    if (opts.ownView) $(opts.ownView).hidden = selected !== CODE;
    $(opts.view).hidden = selected === CODE;
  }

  function renderTabs() {
    var tabs = programs.filter(shown);
    var bar = $(opts.tabs);
    var sig = tabs.map(function (p) { return p.id + ":" + p.state; }).join(",");
    bar.hidden = tabs.length < 2;
    if (sig !== tabsSig) {
      tabsSig = sig;
      bar.replaceChildren.apply(bar, tabs.map(function (p) {
        var t = h("button", "tab", p.label, h("span", STATE_TONE[p.state] || "chip", STATE_SHORT[p.state] || p.state));
        t.type = "button";
        t.id = "ptab-" + p.id;
        t.setAttribute("role", "tab");
        t.setAttribute("aria-controls", "pp-" + p.id);
        t.addEventListener("click", function () { select(p.id); });
        return t;
      }));
    }
    paintSelection();
  }

  function onKey(e) {
    var tabs = programs.filter(shown);
    var at = tabs.findIndex(function (p) { return p.id === selected; });
    if (e.key === "ArrowRight") select(tabs[(at + 1) % tabs.length].id, true);
    else if (e.key === "ArrowLeft") select(tabs[(at + tabs.length - 1) % tabs.length].id, true);
  }

  // ---- Panels: one per other program, its status (repainted on every poll) and its settings and secrets cards ----
  function panelFor(p) {
    var panel = $("pp-" + p.id);
    if (!panel) {
      panel = h("div", "stack", h("div", ""), h("div", "stack"));
      panel.id = "pp-" + p.id;
      panel.setAttribute("role", "tabpanel");
      panel.setAttribute("aria-labelledby", "ptab-" + p.id);
      panel.hidden = true;
      $(opts.view).append(panel);
    }
    return panel;
  }

  function renderPanels(withCards) {
    var view = $(opts.view);
    var live = programs.filter(function (p) { return shown(p) && p.id !== CODE; });
    Array.prototype.slice.call(view.children).forEach(function (el) {
      if (el.id.indexOf("pp-") === 0 && !live.some(function (p) { return "pp-" + p.id === el.id; })) el.remove();
    });
    live.forEach(function (p) {
      var fresh = !$("pp-" + p.id);
      var panel = panelFor(p);
      panel.firstChild.replaceChildren(statusRow(p));
      if ((withCards || fresh) && p.settings && FIELDS[p.id]) panel.lastChild.replaceChildren(card(p, FIELDS[p.id]));
    });
  }

  // The status rows are cheap to repaint on every poll; the forms only when asked (or when a program first shows), so
  // a poll never wipes what is typed.
  function load(withCards) {
    return RT.api("/api/settings/programs").then(function (body) {
      programs = body.programs || [];
      renderPanels(withCards);
      renderTabs();
      watchLinks();
    });
  }

  // `own` is the program this page is of; `ownView` the id of the element holding the code agent's own page (none on
  // the CI runner's trimmed page); `tabs` and `view` the ids of the tab bar and of the other programs' panels.
  function init(o) {
    for (var k in o) opts[k] = o[k];
    $(opts.tabs).addEventListener("keydown", onKey);
  }

  // `show(id)`: open that program's tab, e.g. the code agent's while its setup isn't finished (setup-wizard.md).
  RT.programs = { init: init, load: load, show: function (id) { select(id, false); } };
})();

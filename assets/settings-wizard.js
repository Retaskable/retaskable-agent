// The setup wizard (docs/specs/setup-wizard.md section 3), published as RT.wizard for the code agent's Setup tab. It shows
// what GET /api/wizard says, step by step, and posts only what the page already may: a link (POST /api/link/{target} and
// the CI runner's program link), a saved token (POST /api/wizard/github and /claude), Add project (POST /api/projects, or POST /api/wizard/projects for a retaskable.com project) and
// the CI switch. A step is passed only when its check says done. Built with h(), never markup; no token is ever shown,
// and the one the person pastes is cleared from its field once sent.
(function () {
  "use strict";
  var RT = window.RT, h = RT.h, api = RT.api;
  var view = null;       // the last GET /api/wizard
  var viewing = null;    // the step id on show
  var busy = "";         // what a button is waiting on, so it can't be pressed twice
  var note = {};         // a message under a step: { id: [text, bad] }
  var wantLink = false;  // the person pressed Link: carry on to the next approval by itself
  var repos = { owners: null, owner: "", list: [], picked: {}, error: "" };
  var mine = { data: null, loading: false, picked: {} };  // the Projects step's retaskable.com list
  var timer = null;
  var onChange = function () {};
  var QUEUE = "rtWizardQueue";

  function root() { return RT.$("wizardBox"); }
  function post(path, body) {
    return api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });
  }
  function button(label, cls, fn, disabled) {
    var b = h("button", "btn " + cls, label);
    b.type = "button";
    b.disabled = !!disabled;
    b.addEventListener("click", fn);
    return b;
  }
  function say(text, cls) { return h("p", "wz-say " + (cls || ""), text); }
  function stepOf(id) { return (view.steps || []).filter(function (s) { return s.id === id; })[0]; }
  function setNote(id, text, bad) { note[id] = text ? [text, !!bad] : null; }
  function queue() { try { return JSON.parse(sessionStorage.getItem(QUEUE) || "[]"); } catch (e) { return []; } }
  function setQueue(q) { try { sessionStorage.setItem(QUEUE, JSON.stringify(q)); } catch (e) { /* no storage: one at a time */ } }

  function run(key, fn) {
    busy = key;
    paint();
    return Promise.resolve().then(fn).catch(function (e) { setNote(key.split(":")[0], e.message, true); })
      .then(function () { busy = ""; return load(/:check$/.test(key)); });
  }

  // ---- Step 1: link ----
  function nextLink(s) {
    var d = s.data;
    if (!d.cloud.linked) return "/api/link/" + encodeURIComponent(d.target || "production");
    if (!d.store.linked) return "/api/link/backlog";
    if (d.runner === false) return "/api/settings/programs/ci-runner/link";
    return null;
  }

  function linkBody(s) {
    var d = s.data, box = h("div", "wz-body");
    var rows = h("ul", "wz-list");
    [[d.cloud.name || "retaskable.com", d.cloud.linked], ["The shared backlog", d.store.linked], ["The CI runner", d.runner === null ? null : d.runner]].forEach(function (r) {
      if (r[1] === null) return;
      rows.append(h("li", "", h("span", "wz-title", r[0]), h("span", "chip " + (r[1] ? "ok" : ""), h("span", "dot"), r[1] ? "Linked" : "Not yet")));
    });
    box.append(rows);
    if (d.approve) {
      var url = d.approve.url, a = h("div", "wz-box");
      a.append(h("strong", "", "Approve " + d.approve.label));
      if (url && /^https?:\/\//.test(url)) {
        var link = h("a", "", "Open the approval page");
        link.href = url; link.target = "_blank"; link.rel = "noopener noreferrer";
        a.append(h("span", "", link, " and approve it."));
      }
      if (d.approve.code) a.append(h("span", "", "Check the code: ", h("strong", "mono wz-code", d.approve.code)));
      a.append(h("span", "hint", "Waiting for you to approve. This page carries on by itself."));
      box.append(a);
    }
    var path = nextLink(s);
    if (path) {
      box.append(button(d.approve || d.working ? "Waiting for approval…" : "Link this computer", "primary", function () {
        wantLink = true;
        run("link", function () { return post(path); });
      }, !view.canAct || d.working || busy === "link"));
    }
    return box;
  }

  // After an approval, start the next one without another press (the runner's link follows this computer's and the store's).
  function chainLink() {
    var s = stepOf("link");
    if (!wantLink || !s || !view.canAct || busy || s.data.working || s.data.approve) return;
    var path = nextLink(s);
    if (!path) { wantLink = false; return; }
    run("link", function () { return post(path); });
  }

  // ---- Step 2: GitHub ----
  function githubUrl() {
    return "https://github.com/settings/tokens/new?scopes=repo,workflow,read:org&description=" +
      encodeURIComponent("Retaskable (" + (view.computer || "this computer") + ")");
  }

  function tokenForm(id, placeholder, send, extra) {
    var input = h("input", "");
    input.type = "password"; input.autocomplete = "off"; input.spellcheck = false; input.placeholder = placeholder;
    input.setAttribute("aria-label", placeholder);
    var go = button("Save and check", "primary", function () {
      var value = input.value.trim();
      if (!value) { setNote(id, "Paste it first.", true); return paint(); }
      input.value = "";
      run(id + ":save", function () { return send(value); });
    }, !view.canAct || busy === id + ":save");
    input.addEventListener("keydown", function (e) { if (e.key === "Enter") go.click(); });
    return h("div", "wz-row", extra || null, input, go);
  }

  function githubBody(s) {
    var box = h("div", "wz-body");
    var open = h("a", "btn", "Create a token");
    open.href = githubUrl(); open.target = "_blank"; open.rel = "noopener noreferrer";
    box.append(h("div", "wz-row", open, h("span", "hint", "Opens GitHub with the scopes (repo, workflow, read:org) and a description filled in. Press Generate token there, then copy it.")));
    box.append(tokenForm("github", "Paste the token (ghp_…)", function (value) {
      return post("/api/wizard/github", { token: value }).then(function (r) { setNote("github", r.step && r.step.state === "done" ? "Saved and checked." : "Saved, but the check failed: see above."); });
    }));
    box.append(h("div", "wz-row", button("Check again", "small", function () { run("github:check", function () { return Promise.resolve(); }); })));
    return box;
  }

  // ---- Step 3: Claude ----
  function claudeBody(s) {
    var box = h("div", "wz-body"), kind = "token";
    var pick = h("select"), t = document.createElement("option"), k = document.createElement("option");
    t.value = "token"; t.textContent = "A token from claude setup-token (preferred)";
    k.value = "key"; k.textContent = "An Anthropic API key";
    pick.append(t, k);
    pick.setAttribute("aria-label", "What you are pasting");
    pick.addEventListener("change", function () { kind = pick.value; });
    box.append(h("p", "hint", "On any computer with Claude Code, run `claude setup-token` in a terminal, sign in, and paste the token it prints (it starts sk-ant-oat). It lasts a year and is kept encrypted here."));
    box.append(tokenForm("claude", "Paste the token or key", function (value) {
      return post("/api/wizard/claude", { token: value, kind: kind }).then(function (r) { setNote("claude", r.step && r.step.state === "done" ? "Saved and signed in." : "Saved, but Claude Code didn't accept it: see above."); });
    }, pick));
    box.append(h("div", "wz-row",
      button("Test it", "small", function () {
        run("claude:test", function () {
          return post("/api/wizard/claude/test").then(function (r) { setNote("claude", r.ok ? "Claude answered: " + r.said : r.why, !r.ok); });
        });
      }, !view.canAct || busy === "claude:test"),
      button("Check again", "small", function () { run("claude:check", function () { return Promise.resolve(); }); }),
      h("span", "hint", "Test it sends one tiny message to Claude (a few tokens).")));
    return box;
  }

  // ---- Step 4: projects (Add project, the same change the site's Link project… makes) ----
  function loadOwners() {
    if (repos.owners || repos.loading) return;
    repos.loading = true;
    api("/api/projects/owners").then(function (r) { repos.owners = r.owners || []; repos.owner = repos.owners[0] || ""; repos.error = ""; return loadRepos(); })
      .catch(function (e) { repos.owners = []; repos.error = e.message; })
      .then(function () { repos.loading = false; paint(); });
  }
  function loadRepos() {
    if (!repos.owner) return Promise.resolve();
    return api("/api/projects/repos?owner=" + encodeURIComponent(repos.owner)).then(function (r) { repos.list = r.repos || []; repos.error = ""; })
      .catch(function (e) { repos.list = []; repos.error = e.message; });
  }

  // The person's retaskable.com projects, by organisation (GET /api/wizard/projects); a cloud without the route says "github".
  function loadCloud() {
    if (mine.data || mine.loading) return;
    mine.loading = true;
    api("/api/wizard/projects").then(function (r) { mine.data = r; })
      .catch(function (e) { mine.data = { source: "cloud", error: e.message, groups: [] }; })
      .then(function () { mine.loading = false; paint(); });
  }
  // A queued link is "organisation:owner/repo" for a project, or a bare "owner/repo" for a repo that isn't a project yet.
  function queued(k) { var i = k.indexOf(":"); return i < 0 ? { repo: k, org: "" } : { repo: k.slice(i + 1), org: k.slice(0, i) }; }

  function cloudList(box, d) {
    if (d.error) box.append(say(d.error, "bad"));
    if (!d.error && !d.groups.length) box.append(h("p", "hint", "retaskable.com lists no projects for you yet. Add a repo below."));
    d.groups.forEach(function (g) {
      box.append(h("p", "", h("strong", "", g.name)));
      var ul = h("ul", "wz-list");
      g.projects.forEach(function (p) {
        var key = g.slug + ":" + p.repo, cb = h("input"); cb.type = "checkbox"; cb.checked = !!mine.picked[key];
        cb.disabled = p.onThisComputer || !p.linkable || !view.canAct;
        cb.addEventListener("change", function () { mine.picked[key] = cb.checked; paint(); });
        ul.append(h("li", "", h("label", "", cb, h("span", "", p.name, p.repo ? " (" + p.repo + ")" : "")),
          p.onThisComputer ? h("span", "chip ok", h("span", "dot"), "On this computer") : (p.linkable ? null : h("span", "hint", "Not a GitHub repo"))));
      });
      box.append(ul);
    });
  }

  function githubList(box) {
    loadOwners();
    if (repos.error) box.append(say(repos.error, "bad"));
    if (repos.owners && repos.owners.length > 1) {
      var sel = h("select");
      repos.owners.forEach(function (o) { var op = document.createElement("option"); op.value = o; op.textContent = o; sel.append(op); });
      sel.value = repos.owner;
      sel.setAttribute("aria-label", "Whose repositories");
      sel.addEventListener("change", function () { repos.owner = sel.value; loadRepos().then(paint); });
      box.append(h("div", "wz-row", sel));
    }
    var list = h("ul", "wz-list");
    repos.list.forEach(function (r) {
      var cb = h("input"); cb.type = "checkbox"; cb.checked = !!repos.picked[r.nameWithOwner]; cb.disabled = r.added || !view.canAct;
      cb.addEventListener("change", function () { repos.picked[r.nameWithOwner] = cb.checked; paint(); });
      list.append(h("li", "", h("label", "", cb, h("span", "", r.nameWithOwner, r.private ? " (private)" : "")), r.added ? h("span", "chip ok", h("span", "dot"), "On this computer") : null));
    });
    if (repos.owners && !repos.list.length && !repos.error) list.append(h("li", "hint", "No repositories found."));
    box.append(list);
  }

  function projectsBody(s) {
    var box = h("div", "wz-body"), on = s.data.onComputer || [];
    if (on.length) {
      box.append(h("ul", "wz-list", on.map(function (p) {
        var go = p.paused ? button("Start working on it", "primary small", function () {
          run("projects:resume", function () {
            return post("/api/projects/" + encodeURIComponent(p.id) + "/paused", { paused: false })
              .then(function () { setNote("projects", p.repo + " is working now."); });
          });
        }, !view.canAct || busy === "projects:resume") : null;
        return h("li", "", h("span", "wz-title", p.repo), h("span", "chip ok", h("span", "dot"), "On this computer"),
          p.paused ? h("span", "chip warn", h("span", "dot"), "Paused") : h("span", "chip ok", h("span", "dot"), "Working"), go);
      })));
      if (on.some(function (p) { return p.paused; })) {
        box.append(h("p", "hint", "A project you link starts paused: nothing is worked on until you press Start working on it."));
      }
    }
    // The step's own words (above) already say which repo is being linked and where it is; this is only what follows.
    if (s.data.adding) {
      box.append(h("p", "hint", "The agent restarts when it is done; this page comes back by itself."));
      return box;
    }
    var q = queue();
    if (q.length) box.append(say("Waiting to link: " + q.map(function (k) { return queued(k).repo; }).join(", ") + ".", ""));
    loadCloud();
    var viaGithub = !!mine.data && mine.data.source === "github";
    if (mine.data && !viaGithub) {
      box.append(h("p", "hint", "Your retaskable.com projects, by organisation. Tick the ones this computer should work on: each is cloned here and starts paused; start it above when you are ready."));
      cloudList(box, mine.data);
      var more = h("details", "wz-more", h("summary", "", "Add a repo that isn't a project yet"));
      more.open = !!more.open || Object.keys(repos.picked).some(function (k) { return repos.picked[k]; });
      var inner = h("div", "wz-body");
      inner.append(h("p", "hint", "Your GitHub repositories. A repo you add here becomes a project in your organisation on retaskable.com."));
      githubList(inner);
      more.append(inner);
      box.append(more);
    } else if (mine.data) {
      box.append(h("p", "hint", mine.data.note || ""));
      box.append(h("p", "hint", "Your GitHub repositories. Tick the ones this computer should work on: each is cloned here and starts paused; start it above when you are ready."));
      githubList(box);
    } else {
      box.append(h("p", "hint", "Asking retaskable.com for your projects…"));
    }
    var picked = Object.keys(mine.picked).filter(function (k) { return mine.picked[k]; })
      .concat(Object.keys(repos.picked).filter(function (k) { return repos.picked[k]; }));
    box.append(h("div", "wz-row", button(picked.length ? "Link " + picked.length + " selected" : "Link selected", "primary", function () {
      setQueue(queue().concat(picked)); repos.picked = {}; mine.picked = {};
      run("projects", function () { return Promise.resolve(); });
    }, !view.canAct || !picked.length || busy === "projects"), button("Refresh the list", "small", function () { repos.owners = null; repos.list = []; mine.data = null; paint(); })));
    return box;
  }

  // One repo at a time (each add restarts the agent): the next waits in sessionStorage until the last one is on the computer.
  function pump() {
    var s = stepOf("projects"), q = queue();
    if (!s || !q.length || busy || s.data.adding || !view.canAct) return;
    var have = (s.data.onComputer || []).map(function (p) { return p.repo.toLowerCase(); });
    q = q.filter(function (k) { return have.indexOf(queued(k).repo.toLowerCase()) < 0; });
    setQueue(q);
    if (!q.length) return;
    var next = queued(q.shift());
    setQueue(q);
    run("projects", function () { return next.org ? post("/api/wizard/projects", { repo: next.repo, organisation: next.org }) : post("/api/projects", { repo: next.repo }); });
  }

  // ---- Steps 5 to 7 ----
  // Where a project is on retaskable.com: /<organisation slug>/<project slug> (the slugs come from the Projects step's list,
  // matched by repo), else the site's address.
  function projectPage(cloud, repo) {
    if (!cloud || !/^https?:\/\//.test(cloud)) return "";
    var base = cloud.replace(/\/+$/, ""), want = String(repo || "").toLowerCase(), found = "";
    ((mine.data && mine.data.groups) || []).forEach(function (g) {
      (g.projects || []).forEach(function (p) {
        if (p.repo && p.repo.toLowerCase() === want) found = "/" + encodeURIComponent(g.slug) + "/" + encodeURIComponent(p.slug);
      });
    });
    return base + found;
  }

  var MARK = { "new": "New", changed: "Changed", removed: "Removed" };
  function commandList(lines) {
    var groups = [];
    (lines || []).forEach(function (l) {
      var g = groups.filter(function (x) { return x.label === l.label; })[0];
      if (!g) { g = { label: l.label, lines: [] }; groups.push(g); }
      g.lines.push(l);
    });
    return h("div", "wz-cmds", groups.map(function (g) {
      return h("div", "wz-cmdgroup",
        h("span", "wz-cmdlabel", g.label),
        g.lines.map(function (l) {
          return h("div", "wz-cmd" + (l.status ? " " + l.status : ""),
            h("span", "wz-cmdkind", l.kind || ""),
            h("code", "mono wz-cmdtext", l.command || (l.status === "removed" ? "(removed)" : "")),
            l.status ? h("span", "chip " + (l.status === "removed" ? "bad" : "warn"), MARK[l.status] || l.status) : null);
        }));
    }));
  }

  function commandsBody(s) {
    var box = h("div", "wz-body"), w = s.data.waiting || [];
    var cloud = stepOf("link").data.cloud.url;
    if (!w.length) return box;
    loadCloud();
    w.forEach(function (c) {
      var key = "commands:" + c.id;
      var card = h("div", "wz-box wz-approve",
        h("div", "wz-row", h("strong", "wz-title", c.project), c.by ? h("span", "hint", "changed by " + c.by) : null),
        (c.lines && c.lines.length) ? commandList(c.lines) : h("p", "wz-say", c.text));
      var actions = h("div", "wz-row", button(busy === key ? "Approving…" : "Approve", "primary", function () {
        run(key + ":check", function () {
          return post("/api/wizard/approve", { id: c.id, version: c.version }).then(function () { setNote("commands", "Approved the commands for " + c.project + ".", false); });
        });
      }, !view.canAct || !!busy));
      var url = projectPage(cloud, c.project);
      if (url) {
        var a = h("a", "btn small", "Open on retaskable.com");
        a.href = url; a.target = "_blank"; a.rel = "noopener noreferrer";
        actions.append(a);
      }
      card.append(actions);
      box.append(card);
    });
    box.append(h("p", "hint", "Approving here is you, at this computer, allowing these commands to run on it. They run only after you do."));
    return box;
  }

  function ciBody(s) {
    var box = h("div", "wz-body"), d = s.data;
    if (d.off) {
      box.append(button("Turn CI on", "primary", function () { run("ci", function () { return post("/api/wizard/ci", { off: false }); }); }, !view.canAct || busy === "ci"));
      return box;
    }
    box.append(h("ul", "wz-list",
      [["The runner", d.state === "running" ? "Running" : d.state], ["Its own link", d.linked ? "Linked" : "Not linked"], ["GitHub token", d.token || "None"], ["Docker", d.docker || "Not reported yet"]].map(function (r) {
        return h("li", "", h("span", "wz-title", r[0]), h("span", "wz-say", r[1]));
      })));
    if (d.repos && d.repos.length) box.append(h("p", "hint", "Checks pull requests for: " + d.repos.join(", ") + "."));
    box.append(h("div", "wz-row", button("Turn CI off on this computer", "small", function () { run("ci", function () { return post("/api/wizard/ci", { off: true }); }); }, !view.canAct || busy === "ci")));
    return box;
  }

  function doneBody() {
    var box = h("div", "wz-body");
    box.append(h("ul", "wz-checks", (view.steps || []).filter(function (s) { return s.id !== "done"; }).map(function (s) {
      var ok = s.state === "done";
      return h("li", ok ? "" : "no", h("span", "wz-tick", ok ? "✓" : "✗"), h("span", "wz-checkname", s.title), h("span", "wz-checksay", ok ? (s.summary || "") : "Not yet"));
    })));
    if (view.complete) box.append(h("div", "wz-row", button("Open this computer's page", "primary", function () { onChange("now"); })));
    return box;
  }

  var BODIES = { link: linkBody, github: githubBody, claude: claudeBody, projects: projectsBody, commands: commandsBody, ci: ciBody, done: doneBody };
  var CHIP = { done: ["ok", "Done"], needs: ["warn", "Needs you"], failed: ["bad", "Failed"] };

  // ---- The page ----
  function paint() {
    var el = root();
    if (!el || !view) return;
    var steps = view.steps || [], cur = view.current || "done";
    if (!viewing || !stepOf(viewing)) viewing = cur;
    var kids = [];
    var left = steps.filter(function (s) { return s.state !== "done"; }).length;
    kids.push(h("div", "wz-top",
      h("h2", "", view.complete ? "This computer is set up" : "Set this computer up"),
      h("p", "calm", view.complete ? "Every step is done. You can come back here to check them." : left + " of " + steps.length + " steps left. Each one checks itself; you can move on once it passes."),
      view.canAct ? null : say("Open this page on the computer itself to change anything here.", "bad")));
    if (view.storageProblem) kids.push(say(view.storageProblem, "bad"));
    var curIndex = steps.map(function (s) { return s.id; }).indexOf(cur);
    steps.forEach(function (s, i) {
      var later = curIndex >= 0 && i > curIndex, open = s.id === viewing && !later;
      var chip = CHIP[s.state];
      var head = h("div", "wz-head" + (later ? "" : " click"), h("span", "wz-num", s.state === "done" ? "✓" : String(i + 1)), h("span", "wz-title", s.title),
        later ? h("span", "hint", "Waiting for the steps above") : h("span", "chip " + chip[0], h("span", "dot"), chip[1]));
      if (!later) {
        head.tabIndex = 0;
        head.setAttribute("role", "button");
        head.setAttribute("aria-expanded", String(open));
        var toggle = function () { viewing = open ? null : s.id; paint(); };
        head.addEventListener("click", toggle);
        head.addEventListener("keydown", function (e) { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(); } });
      }
      var card = h("div", "wz-step " + s.state + (s.id === cur ? " now" : "") + (later ? " later" : ""), head);
      if (open) {
        var body = (BODIES[s.id] || function () { return h("div", "wz-body"); })(s);
        var words = h("div", "wz-body");
        if (s.state === "done") words.append(say(s.detail, "good"));
        if (s.state === "failed") words.append(say(s.failed, "bad"));
        if (s.state === "needs" && s.todo) words.append(say(s.todo, ""));
        var n = note[s.id];
        if (n) words.append(say(n[0], n[1] ? "bad" : "good"));
        card.append(words, body);
        var next = steps[i + 1];
        if (next && s.id !== "done") {
          card.append(h("div", "wz-body", h("div", "wz-row", button("Next", "primary", function () { viewing = next.id; paint(); }, s.state !== "done"),
            s.state !== "done" ? h("span", "hint", "Next opens once this step's check passes.") : null)));
        }
      }
      kids.push(card);
    });
    el.replaceChildren.apply(el, kids);
  }

  function load(fresh) {
    return api("/api/wizard" + (fresh ? "?fresh=1" : "")).then(function (v) {
      var firstTime = !view;
      view = v;
      if (firstTime) viewing = v.current || "done";
      paint();
      onChange(null, v);
      chainLink();
      pump();
    }).catch(function (e) { if (e.status === 401) onChange("signedOut"); });
  }

  // Polls while a step waits on the world (an approval, a clone, the runner), slower once everything is done.
  function start() {
    if (timer) return;
    timer = setInterval(function () { if (!document.hidden && !busy) load(false); }, 3000);
    load(false);
  }

  RT.wizard = {
    init: function (opts) { onChange = opts.onChange || onChange; },
    start: start,
    load: load,
    paint: paint,
    view: function () { return view; },
  };
})();

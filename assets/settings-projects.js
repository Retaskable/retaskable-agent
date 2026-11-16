// Add project (agent-settings.md section 4.7), published as RT.projects for the code agent's Settings tab: the projects
// already added, and a picker of the person's GitHub repos (or an owner/repo typed) that adds one. It only shows what
// /api/projects says and posts the choice; the clone, the entry and the paused start are the agent's. Built with h(),
// never markup, and no token is ever in anything it shows.
(function () {
  "use strict";
  var RT = window.RT, $ = RT.$, h = RT.h, api = RT.api;
  var state = { open: false, owners: null, owner: "", repos: [], search: "", listing: "", view: null };
  var timer = null;

  function box() { return $("projectsBox"); }

  function say(text, bad) {
    var el = $("projectsSay");
    if (!el) return;
    el.textContent = text || "";
    el.className = bad ? "error" : "hint";
  }

  function button(label, cls, fn) {
    var b = h("button", cls, label);
    b.type = "button";
    b.addEventListener("click", fn);
    return b;
  }

  function working(add) { return add && add.state === "working"; }

  function post(repo) {
    say("Starting…");
    return api("/api/projects", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ repo: repo }),
    }).then(function () { state.open = false; return load(); })
      .catch(function (err) { say(err.message, true); });
  }

  function progress(add) {
    if (working(add)) {
      return h("p", "", h("strong", "", "Adding " + add.repo + ". "), add.step);
    }
    if (add.state === "done") {
      return h("p", "",
        h("strong", "", "Added " + add.repo + ". "),
        "It's paused: nothing will be worked on until you press Resume on its page on retaskable.com. ",
        "Retaskable is restarting to start using it. If this page asks you to sign in again, open Settings from the tray menu. ",
        "Its folder is " + add.folder + ".");
    }
    if (add.state === "failed") {
      return h("p", "error", "Couldn't add " + add.repo + ": " + add.message);
    }
    return null;
  }

  function repoRows() {
    var q = state.search.trim().toLowerCase();
    var rows = state.repos.filter(function (r) {
      return !q || r.nameWithOwner.toLowerCase().indexOf(q) >= 0 || (r.description || "").toLowerCase().indexOf(q) >= 0;
    });
    if (!rows.length) return [h("li", "hint", state.listing === "loading" ? "Asking GitHub…" : "No repos to show.")];
    return rows.slice(0, 200).map(function (r) {
      var li = h("li", "",
        h("span", "", h("strong", "", r.nameWithOwner), r.private ? h("span", "chip", "Private") : null,
          r.description ? h("span", "hint", " " + r.description) : null));
      li.append(r.added ? h("span", "hint", "Added") : button("Add", "btn small", function () { post(r.nameWithOwner); }));
      return li;
    });
  }

  function loadRepos(owner, fresh) {
    state.owner = owner;
    state.listing = "loading";
    state.repos = [];
    paint();
    return api("/api/projects/repos?owner=" + encodeURIComponent(owner) + (fresh ? "&fresh=1" : ""))
      .then(function (body) { state.repos = body.repos; state.listing = ""; })
      .catch(function (err) { state.listing = ""; say("Couldn't list " + owner + "'s repos: " + err.message, true); })
      .then(paint);
  }

  function openPicker() {
    state.open = true;
    state.search = "";
    paint();
    return api("/api/projects/owners")
      .then(function (body) {
        state.owners = body.owners;
        return body.owners.length ? loadRepos(body.owners[0]) : null;
      })
      .catch(function (err) {
        state.owners = [];
        say("Couldn't reach GitHub: " + err.message + " (Is GitHub linked? Save a GH_TOKEN under Secrets.) You can still type owner/repo below.", true);
        paint();
      });
  }

  function picker() {
    var owner = h("select");
    owner.id = "projectsOwner";
    (state.owners || []).forEach(function (o) {
      var opt = h("option", "", o);
      opt.value = o;
      owner.append(opt);
    });
    owner.value = state.owner;
    owner.addEventListener("change", function () { loadRepos(owner.value); });
    var search = h("input");
    search.type = "text";
    search.id = "projectsSearch";
    search.placeholder = "Search these repos";
    search.value = state.search;
    search.addEventListener("input", function () {
      state.search = search.value;
      var list = $("projectsRepos");
      list.replaceChildren.apply(list, repoRows());
    });
    var typed = h("input");
    typed.type = "text";
    typed.id = "projectsTyped";
    typed.placeholder = "owner/repo, or its GitHub address";
    var list = h("ul", "sub");
    list.id = "projectsRepos";
    repoRows().forEach(function (li) { list.append(li); });
    return h("div", "field group",
      h("label", "", "Pick a repo from GitHub"),
      h("div", "choice", owner, search),
      list,
      h("label", "", "Or type one"),
      h("div", "choice", typed, button("Add", "btn primary small", function () { post(typed.value); })),
      button("Cancel", "btn small", function () { state.open = false; say(""); paint(); }));
  }

  function paint() {
    var v = state.view;
    if (!v || !box()) return;
    var items = v.projects.map(function (p) {
      return h("li", "", h("strong", "", p.id), p.githubRepo ? " " + p.githubRepo : "");
    });
    var list = h("ul", "sub");
    items.forEach(function (li) { list.append(li); });
    var parts = [items.length ? list : h("p", "hint", "No project yet.")];
    var shown = progress(v.add);
    if (shown) parts.push(shown);
    if (v.canAdd && !working(v.add) && v.add.state !== "done") {
      parts.push(state.open ? picker() : button("Add project", "btn primary", openPicker));
    } else if (!v.canAdd) {
      parts.push(h("p", "hint", "A project is added from the computer it runs on."));
    }
    var sayEl = h("p", "hint");
    sayEl.id = "projectsSay";
    sayEl.setAttribute("role", "status");
    parts.push(sayEl);
    var prior = $("projectsSay") ? $("projectsSay").textContent : "";
    var prior_bad = $("projectsSay") && $("projectsSay").className === "error";
    box().replaceChildren.apply(box(), parts);
    if (prior) say(prior, prior_bad);
  }

  // While an add works it is looked at every 2 seconds; the Settings tab's own polling is slower.
  function load() {
    return api("/api/projects").then(function (v) {
      state.view = v;
      if (!state.open || !working(v.add)) paint();
      if (working(v.add) && !timer) {
        timer = window.setTimeout(function () { timer = null; load().catch(function () {}); }, 2000);
      }
    });
  }

  RT.projects = { load: load };
})();

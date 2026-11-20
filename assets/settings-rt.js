// The helpers both settings pages and settings-programs.js use, published as window.RT: $ (by id), h (build an
// element; text goes in as text, never as markup), api (fetch JSON with the session's CSRF token on RT.csrf) and
// setStatus (the status line, in each place the page has one).
(function () {
  "use strict";
  var RT = window.RT = { csrf: null };
  var $ = function (id) { return document.getElementById(id); };

  function setStatus(message, isError) {
    ["status", "pstatus"].forEach(function (id) {
      var el = $(id);
      if (!el) return;
      el.textContent = message || "";
      el.className = isError ? "error" : "";
    });
  }

  function api(path, options) {
    options = options || {};
    options.headers = options.headers || {};
    if (RT.csrf) options.headers["X-CSRF-Token"] = RT.csrf;
    return fetch(path, options).then(function (res) {
      return res.json().catch(function () { return {}; }).then(function (body) {
        if (!res.ok) {
          var err = new Error(body.error || res.statusText);
          err.status = res.status;
          throw err;
        }
        return body;
      });
    });
  }

  function h(tag, cls) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    // A child may be a node, text, or a list of them (a map() result); a list is flattened, never printed.
    var add = function (k) {
      if (Array.isArray(k)) k.forEach(add);
      else if (k !== null && k !== undefined && k !== false) e.append(k);
    };
    for (var i = 2; i < arguments.length; i++) add(arguments[i]);
    return e;
  }

  // What a link that waits shows (a device flow's `approve`/`waiting`: { url, code }, never a token): the store's approval
  // page as a link that opens in a new tab, and the code to check on it. Only an http(s) address becomes a link.
  function approval(a) {
    var box = h("span", "approve");
    if (a && a.url && /^https?:\/\//.test(a.url)) {
      var link = h("a", "", "Open the approval page");
      link.href = a.url;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      box.append(link, " and approve it. ");
    } else {
      box.append("Starting… ");
    }
    if (a && a.code) box.append("The code is ", h("strong", "mono", a.code), ". ");
    box.append("Waiting for you to approve…");
    return box;
  }

  // What a repo's backlog already holds (docs/specs/setup-wizard.md section 3.9), from GET /api/wizard/backlog: asked once
  // per repo, said as a plain sentence. An error is said, never read as "nothing".
  var counts = {}, waiting = {};
  function need(repo, again) {
    var k = repo.toLowerCase();
    if (counts[k] && counts[k] !== "asking") return counts[k];
    if (again) (waiting[k] = waiting[k] || []).push(again);
    if (counts[k]) return null;
    counts[k] = "asking";
    api("/api/wizard/backlog?repo=" + encodeURIComponent(repo)).then(function (r) { counts[k] = r; })
      .catch(function (e) { counts[k] = { repo: repo, error: e.message }; })
      .then(function () { var w = waiting[k] || []; waiting[k] = []; w.forEach(function (f) { f(); }); });
    return null;
  }
  function plural(n, one, many) { return n + " " + (n === 1 ? one : many); }
  function parts(c) {
    var out = [];
    if (c.inProgress) out.push(c.inProgress + " in progress");
    if (c.waiting) out.push(c.waiting + " waiting");
    if (c.needsYou) out.push(c.needsYou + " need you");
    return out.join(", ");
  }
  function words(c) {
    if (!c) return null;
    if (c.error) return "Can't tell what this repo's backlog already holds just now: " + c.error;
    if (!c.total) return null;
    return "This repo already has " + plural(c.total, "item", "items") + " in its backlog from before (" + parts(c) + "). They will start when you press Start working on it.";
  }
  function starts(c) {
    if (!c || c.error) return "";
    if (!c.total) return "Nothing is waiting yet.";
    var rest = c.waiting + c.needsYou;
    if (c.inProgress) return "Starts " + plural(c.inProgress, "item", "items") + " now; " + rest + " more " + (rest === 1 ? "is" : "are") + " waiting in order.";
    return "Nothing is in progress; it starts with the first of the " + rest + " waiting.";
  }
  RT.backlog = { need: need, words: words, starts: starts, parts: parts };

  RT.$ = $; RT.api = api; RT.h = h; RT.setStatus = setStatus; RT.approval = approval;
})();

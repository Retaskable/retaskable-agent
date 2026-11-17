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
    for (var i = 2; i < arguments.length; i++) {
      var k = arguments[i];
      if (k !== null && k !== undefined && k !== false) e.append(k);
    }
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

  RT.$ = $; RT.api = api; RT.h = h; RT.setStatus = setStatus; RT.approval = approval;
})();

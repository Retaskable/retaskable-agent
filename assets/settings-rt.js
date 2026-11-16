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

  RT.$ = $; RT.api = api; RT.h = h; RT.setStatus = setStatus;
})();

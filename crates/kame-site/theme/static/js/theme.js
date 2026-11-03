// Light and dark. The page follows the system until a choice is made here, which is then
// remembered. This runs before the page is drawn, so it never flashes the wrong one.
(function () {
  var key = "kame-theme";
  var root = document.documentElement;

  function saved() {
    try {
      return localStorage.getItem(key);
    } catch (err) {
      return null;
    }
  }

  function current() {
    if (root.dataset.theme) {
      return root.dataset.theme;
    }
    return matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }

  var choice = saved();
  if (choice === "light" || choice === "dark") {
    root.dataset.theme = choice;
  }

  document.addEventListener("DOMContentLoaded", function () {
    var button = document.getElementById("theme-toggle");
    if (!button) {
      return;
    }
    function label() {
      button.textContent = current() === "dark" ? "Light" : "Dark";
    }
    button.addEventListener("click", function () {
      var next = current() === "dark" ? "light" : "dark";
      root.dataset.theme = next;
      try {
        localStorage.setItem(key, next);
      } catch (err) {}
      label();
    });
    label();
    button.hidden = false;
  });
})();

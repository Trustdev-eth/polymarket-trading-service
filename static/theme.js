(function () {
  const KEY = "oq_theme";
  function t(k) {
    return window.OQ_I18N && OQ_I18N.t ? OQ_I18N.t(k) : k;
  }
  function current() {
    return document.documentElement.getAttribute("data-theme") === "light" ? "light" : "dark";
  }
  function apply(tname) {
    const theme = tname === "light" ? "light" : "dark";
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem(KEY, theme);
    document.querySelectorAll("#themeToggle").forEach((btn) => {
      btn.setAttribute("aria-pressed", theme === "light" ? "true" : "false");
      btn.setAttribute("aria-label", t(theme === "dark" ? "theme.toLight" : "theme.toDark"));
      const label = btn.querySelector(".theme-label");
      if (label) label.textContent = t(theme === "dark" ? "theme.light" : "theme.dark");
    });
    window.dispatchEvent(new CustomEvent("oq-theme", { detail: theme }));
  }
  apply(localStorage.getItem(KEY) || "dark");
  window.addEventListener("oq-lang", () => apply(current()));
  document.addEventListener("click", (e) => {
    const btn = e.target.closest("#themeToggle");
    if (!btn) return;
    apply(current() === "dark" ? "light" : "dark");
  });
  const logout = document.getElementById("logout");
  if (logout && !logout.dataset.bound) {
    logout.dataset.bound = "1";
    logout.addEventListener("click", async () => {
      await fetch("/api/auth/logout", { method: "POST", credentials: "same-origin" });
      location.href = "/";
    });
  }
})();

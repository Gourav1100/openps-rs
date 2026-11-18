(() => {
  const { protocol, hostname } = window.location;
  const sonyHosts = ["sony.com", "sonyentertainmentnetwork.com", "playstation.com", "playstation.net"];
  if (window !== window.top || protocol !== "https:" ||
      !sonyHosts.some(host => hostname === host || hostname.endsWith(`.${host}`))) return;

  function showLoading() {
    const overlay = document.createElement("div");
    Object.assign(overlay.style, {
      position: "fixed", inset: "0", zIndex: "2147483647", display: "flex",
      flexDirection: "column", alignItems: "center", justifyContent: "center",
      gap: "18px", background: "#111820", color: "#f5f7fa",
      font: "16px system-ui, sans-serif", textAlign: "center",
    });

    const spinner = document.createElement("div");
    Object.assign(spinner.style, {
      width: "32px", height: "32px", border: "3px solid #536274",
      borderTopColor: "#ffffff", borderRadius: "50%",
    });
    spinner.setAttribute("aria-hidden", "true");
    if (!window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      spinner.animate([{ transform: "rotate(0deg)" }, { transform: "rotate(360deg)" }],
        { duration: 900, iterations: Infinity });
    }

    const message = document.createElement("p");
    message.textContent = "Loading PlayStation sign-in…";
    message.setAttribute("role", "status");
    const show = document.createElement("button");
    show.type = "button";
    show.textContent = "Show page";
    Object.assign(show.style, {
      padding: "8px 14px", color: "#f5f7fa", background: "transparent",
      border: "1px solid #aeb9c5", borderRadius: "6px", cursor: "pointer",
      font: "inherit",
    });
    overlay.append(spinner, message, show);
    document.body.append(overlay);

    const dismiss = () => { clearInterval(timer); overlay.remove(); };
    show.addEventListener("click", dismiss);
    const timer = setInterval(() => {
      if (!overlay.isConnected) document.body.append(overlay);
      const controls = document.querySelectorAll('body input:not([type="hidden"]), body button, body a[href], body select, body [role="button"]');
      if (Array.from(controls).some(control => {
        if (overlay.contains(control)) return false;
        const rect = control.getBoundingClientRect();
        const style = getComputedStyle(control);
        return rect.width > 0 && rect.height > 0 && style.visibility !== "hidden" && style.opacity !== "0";
      })) dismiss();
    }, 250);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", showLoading, { once: true });
  } else {
    showLoading();
  }
})();

(() => {
  "use strict";
  const transition = document.getElementById("transition-stage");
  if (transition) {
    let queued = false;
    const updateSwap = () => {
      const rect = transition.getBoundingClientRect();
      const travel = Math.max(1, transition.offsetHeight - window.innerHeight);
      const progress = Math.min(1, Math.max(0, -rect.top / travel));
      transition.style.setProperty("--swap", progress.toFixed(3));
      queued = false;
    };
    const schedule = () => {
      if (!queued) {
        queued = true;
        requestAnimationFrame(updateSwap);
      }
    };
    window.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule, { passive: true });
    updateSwap();
  }
  const descriptions = {
    workspace: "The Pi workspace: projects, sessions, and the full conversation.",
    sessions: "A clear view of projects, branches, and active sessions.",
    diff: "Review branch changes side by side before you move on."
  };
  const image = document.getElementById("preview-image");
  const description = document.getElementById("preview-description");
  document.querySelectorAll("[data-shot]").forEach(tab => {
    tab.addEventListener("click", () => {
      const shot = tab.dataset.shot;
      if (!image || !Object.hasOwn(descriptions, shot)) return;
      document.querySelectorAll("[data-shot]").forEach(item => {
        item.setAttribute("aria-selected", String(item === tab));
      });
      image.classList.remove("is-ready");
      image.alt = descriptions[shot];
      image.onload = () => image.classList.add("is-ready");
      image.onerror = () => image.classList.add("is-ready");
      image.src = `/assets/app-${shot}.png`;
      if (description) description.textContent = descriptions[shot];
    });
  });
  const hero = document.querySelector(".terminal-hero");
  const session = document.querySelector(".session-section");
  if (hero && session && "IntersectionObserver" in window) {
    const observer = new IntersectionObserver(entries => {
      entries.forEach(entry => session.classList.toggle("in-view", entry.isIntersecting));
    }, { threshold: .14 });
    observer.observe(session);
    window.addEventListener("scroll", () => {
      hero.classList.toggle("scrolled", window.scrollY > window.innerHeight * .18);
    }, { passive: true });
  }
  const base = "https://edge.letscypher.app/releases/";
  fetch(`${base}latest.txt`).then(r => r.ok ? r.text() : Promise.reject()).then(text => {
    const version = text.trim();
    if (!/^\d+\.\d+\.\d+$/.test(version)) return;
    document.querySelectorAll("[data-version]").forEach(n => n.textContent = `v${version}`);
    document.querySelectorAll("[data-download]").forEach(n => n.href = `${base}cypher-${version}-macos-arm64.dmg`);
  }).catch(() => {});
  const button = document.getElementById("copy");
  const status = document.getElementById("copy-status");
  const commandNode = document.getElementById("install-command");
  // The compact terminal layout has its own inline copy handler.
  if (!button || !status || !commandNode) return;
  const command = commandNode.textContent;
  const fallback = () => {
    const area = document.createElement("textarea");
    area.value = command; area.style.cssText = "position:fixed;left:-9999px;opacity:0";
    document.body.append(area); area.select();
    let ok = false; try { ok = document.execCommand("copy"); } catch {}
    area.remove(); return ok;
  };
  button.addEventListener("click", async () => {
    button.disabled = true;
    let ok = false;
    try { if (navigator.clipboard && window.isSecureContext) { await navigator.clipboard.writeText(command); ok = true; } else ok = fallback(); }
    catch { ok = fallback(); }
    button.disabled = false; button.textContent = ok ? "Copied ✓" : "Try again";
    status.textContent = ok ? "Install command copied." : "Copy failed — select the command above.";
    setTimeout(() => { button.textContent = "Copy command ⧉"; status.textContent = ""; }, 2800);
  });
})();

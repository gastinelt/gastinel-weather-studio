/*
 * Couleurs de la carte — interface de réglage du fond de carte.
 *
 * Le panneau fonctionne dans une iframe sans accès direct à l'API Tauri :
 * les appels vers la DLL passent par `postMessage({ gws: "call", ... })`,
 * et les réglages en direct sont diffusés via `postMessage`.
 */

(() => {
  let prochainId = 1;
  const attentes = new Map();
  let timerSauvegarde = null;
  let rafId = null;

  const curseurLum = document.getElementById("curseur-luminosite");
  const curseurSat = document.getElementById("curseur-saturation");
  const curseurNet = document.getElementById("curseur-nettete");

  const valLum = document.getElementById("valeur-luminosite");
  const valSat = document.getElementById("valeur-saturation");
  const valNet = document.getElementById("valeur-nettete");

  const btnReset = document.getElementById("btn-reinitialiser");
  const statutEl = document.getElementById("statut");

  /** Échange avec la DLL de l'extension via le pont postMessage de l'application. */
  function appel(methode, payload = {}) {
    return new Promise((resolve, reject) => {
      const rid = prochainId++;
      attentes.set(rid, { resolve, reject });
      window.parent.postMessage({ gws: "call", rid, method: methode, payload }, "*");
    });
  }

  window.addEventListener("message", (ev) => {
    const m = ev.data || {};
    if (m.gws === "reply" && attentes.has(m.rid)) {
      const { resolve, reject } = attentes.get(m.rid);
      attentes.delete(m.rid);
      if (m.err) reject(new Error(m.err));
      else resolve(m.ok);
    }
  });

  function formaterValeur(v) {
    const n = Number(v);
    if (n > 0) return `+${n}`;
    return String(n);
  }

  function lireValeurs() {
    return {
      luminosite: Number(curseurLum.value),
      saturation: Number(curseurSat.value),
      nettete: Number(curseurNet.value),
    };
  }

  function appliquerValeursDansUI(r) {
    curseurLum.value = r.luminosite ?? 0;
    curseurSat.value = r.saturation ?? 0;
    curseurNet.value = r.nettete ?? 0;

    valLum.textContent = formaterValeur(curseurLum.value);
    valSat.textContent = formaterValeur(curseurSat.value);
    valNet.textContent = formaterValeur(curseurNet.value);
  }

  /**
   * Transmet les réglages à la fenêtre principale pour mise à jour du filtre CSS/SVG.
   * Cadencé par requestAnimationFrame pour préserver la fluidité lors du glissement.
   */
  function diffuserStyle(valeurs) {
    if (rafId) cancelAnimationFrame(rafId);
    rafId = requestAnimationFrame(() => {
      window.parent.postMessage(
        {
          gws: "map_tile_style",
          payload: valeurs,
        },
        "*"
      );
      rafId = null;
    });
  }

  /** Enregistre les réglages sur disque via la DLL (temporisé à 300 ms pendant le glissement). */
  function planifierSauvegarde() {
    if (timerSauvegarde) clearTimeout(timerSauvegarde);
    timerSauvegarde = setTimeout(async () => {
      try {
        await appel("save_settings", lireValeurs());
        indiquerStatut("Enregistré");
      } catch (e) {
        indiquerStatut("Erreur sauvegarde : " + e.message);
      }
    }, 300);
  }

  function indiquerStatut(msg) {
    if (!statutEl) return;
    statutEl.textContent = msg;
    setTimeout(() => {
      if (statutEl.textContent === msg) statutEl.textContent = "";
    }, 2000);
  }

  function onCurseurInput() {
    const vals = lireValeurs();
    valLum.textContent = formaterValeur(vals.luminosite);
    valSat.textContent = formaterValeur(vals.saturation);
    valNet.textContent = formaterValeur(vals.nettete);

    diffuserStyle(vals);
    planifierSauvegarde();
  }

  async function reinitialiser() {
    const defaut = { luminosite: 0, saturation: 0, nettete: 0 };
    appliquerValeursDansUI(defaut);
    diffuserStyle(defaut);
    try {
      await appel("reset_settings");
      indiquerStatut("Réinitialisé");
    } catch (e) {
      indiquerStatut("Erreur : " + e.message);
    }
  }

  // Écouteurs d'événements
  curseurLum.addEventListener("input", onCurseurInput);
  curseurSat.addEventListener("input", onCurseurInput);
  curseurNet.addEventListener("input", onCurseurInput);

  btnReset.addEventListener("click", reinitialiser);

  // Échap ferme la modale
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      window.parent.postMessage({ gws: "escape" }, "*");
    }
  });

  // Chargement initial
  async function initialiser() {
    window.parent.postMessage({ gws: "ready" }, "*");
    try {
      const reglages = await appel("get_settings");
      appliquerValeursDansUI(reglages || {});
      diffuserStyle(lireValeurs());
    } catch {
      // Si la DLL n'est pas encore prête ou absente, on reste aux valeurs par défaut (0)
      appliquerValeursDansUI({ luminosite: 0, saturation: 0, nettete: 0 });
    }
  }

  document.addEventListener("DOMContentLoaded", initialiser);
})();

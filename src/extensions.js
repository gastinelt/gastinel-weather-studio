/*
 * Extensions natives — liste dans les Paramètres, bouton sur la carte, panneau.
 *
 * Le panneau d'une extension est une iframe servie par le protocole `gwsext`
 * (fichiers du dossier `ui/` de l'extension). Elle n'a pas accès à l'API
 * Tauri : ses appels passent par postMessage, que cette page relaie à la DLL
 * via `extension_call`. Seule l'iframe ouverte par nous est écoutée.
 */
const GwsExtensions = (() => {
  const inv = window.__TAURI__ ? window.__TAURI__.core.invoke : null;
  const ORIGINE = /windows/i.test(navigator.userAgent) ? "http://gwsext.localhost" : "gwsext://localhost";
  let extensions = [];
  let panneau = null; // { id, iframe }

  const ETATS = {
    Desactivee: "désactivée",
    Chargement: "chargement…",
    Active: "active",
    Incompatible: "incompatible",
    Erreur: "erreur",
  };
  const PERMS = { db_read: "lecture de la base", db_write: "ÉCRITURE dans la base", network: "accès Internet" };

  async function rafraichir() {
    if (!inv) return;
    try {
      extensions = await inv("list_extensions");
    } catch (e) {
      extensions = [];
      console.error("[extensions]", e);
    }
    rendreParametres();
    rendreBoutons();
  }

  function rendreParametres() {
    const liste = document.getElementById("extensions-list");
    if (!liste) return;
    liste.textContent = "";
    document.getElementById("extensions-empty").hidden = extensions.length > 0;
    for (const x of extensions) {
      const li = document.createElement("li");
      li.className = "extension-item";

      const tete = document.createElement("label");
      tete.className = "extension-head";
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = x.enabled;
      box.disabled = x.state.kind === "Chargement";
      box.addEventListener("change", () => basculer(x, box));
      const titre = document.createElement("strong");
      titre.textContent = `${x.name} ${x.version}`;
      const etat = document.createElement("span");
      etat.className = "extension-state is-" + x.state.kind.toLowerCase();
      etat.textContent = ETATS[x.state.kind] || x.state.kind;
      tete.append(box, titre, etat);
      li.appendChild(tete);

      const lignes = [];
      if (x.description) lignes.push(x.description);
      if (x.permissions && x.permissions.length) {
        lignes.push("Déclare : " + x.permissions.map((p) => PERMS[p] || p).join(", ") + ".");
      }
      if (x.state.detail) lignes.push("Attention : " + x.state.detail);
      if (x.last_backup) lignes.push("Sauvegarde avant écriture : " + x.last_backup);
      for (const t of lignes) {
        const p = document.createElement("p");
        p.className = "hint";
        p.textContent = t;
        li.appendChild(p);
      }
      const retirer = document.createElement("button");
      retirer.type = "button";
      retirer.className = "btn btn-sm";
      retirer.textContent = "Désinstaller";
      retirer.addEventListener("click", () => desinstaller(x, retirer));
      li.appendChild(retirer);
      liste.appendChild(li);
    }
  }

  async function desinstaller(x, bouton) {
    if (!(await confirm(`Désinstaller « ${x.name} » ? Ses fichiers seront supprimés (vos données ne sont pas touchées).`))) return;
    bouton.disabled = true;
    try {
      await inv("extension_uninstall", { id: x.id });
    } catch (e) {
      alert("Extension : " + e);
    }
    await rafraichir();
    await rendreCatalogue();
  }

  /** Catalogue en ligne (gastinel.me) : installer, mettre à jour. */
  async function rendreCatalogue() {
    const liste = document.getElementById("extensions-catalogue");
    const statut = document.getElementById("extensions-catalogue-status");
    if (!liste || !inv) return;
    statut.textContent = "Chargement du catalogue…";
    let offres;
    try {
      offres = await inv("extensions_catalogue");
    } catch (e) {
      liste.textContent = "";
      statut.textContent = "Catalogue indisponible : " + e;
      return;
    }
    liste.textContent = "";
    statut.textContent = offres.length ? "" : "Aucune extension publiée pour le moment.";
    for (const o of offres) {
      const li = document.createElement("li");
      li.className = "extension-item";
      const tete = document.createElement("div");
      tete.className = "extension-head";
      const titre = document.createElement("strong");
      titre.textContent = `${o.name} ${o.version}`;
      const etat = document.createElement("span");
      etat.className = "extension-state";
      const bouton = document.createElement("button");
      bouton.type = "button";
      bouton.className = "btn btn-sm";
      if (o.pending_restart) {
        etat.textContent = "redémarrage nécessaire";
        bouton.hidden = true;
      } else if (!o.compatible) {
        etat.textContent = "demande une version plus récente de l'application";
        bouton.hidden = true;
      } else if (!o.installed_version) {
        etat.textContent = `${(o.size / 1048576).toFixed(1).replace(".", ",")} Mo`;
        bouton.textContent = "Installer";
      } else if (o.installed_version !== o.version) {
        etat.textContent = `installée : ${o.installed_version}`;
        bouton.textContent = "Mettre à jour";
      } else {
        etat.textContent = "installée, à jour";
        bouton.hidden = true;
      }
      bouton.addEventListener("click", () => installer(o, bouton));
      tete.append(titre, etat, bouton);
      li.appendChild(tete);
      if (o.description) {
        const p = document.createElement("p");
        p.className = "hint";
        p.textContent = o.description;
        li.appendChild(p);
      }
      liste.appendChild(li);
    }
  }

  async function installer(o, bouton) {
    bouton.disabled = true;
    bouton.textContent = "Téléchargement…";
    try {
      const r = await inv("extension_install", { id: o.id });
      if (r.restart_required) {
        alert(`« ${o.name} » ${r.version} est prête : elle remplacera la version en cours au prochain démarrage de l'application.`);
      }
    } catch (e) {
      alert("Installation impossible : " + e);
    }
    await rafraichir();
    await rendreCatalogue();
  }

  async function basculer(x, box) {
    const activer = box.checked;
    if (activer && (x.permissions || []).includes("db_write")) {
      const ok = await confirm(
        `« ${x.name} » demande l'écriture dans la base.\n\n` +
          "Une copie complète de la base va être faite avant son chargement (cela peut " +
          "prendre plusieurs minutes et plusieurs Go sur une grosse base). Cette copie " +
          "protège vos données, mais pas contre un plantage de l'application pendant " +
          "que l'extension tourne.\n\nContinuer ?"
      );
      if (!ok) { box.checked = false; return; }
    }
    box.disabled = true;
    try {
      await inv("set_extension_enabled", { id: x.id, enabled: activer });
    } catch (e) {
      alert("Extension : " + e);
    }
    await rafraichir();
  }

  /** Un bouton par extension active accrochée à la carte (`hook: map_view`). */
  function rendreBoutons() {
    const zone = document.getElementById("map-extensions");
    if (!zone) return;
    zone.textContent = "";
    for (const x of extensions) {
      if (x.hook !== "map_view" || x.state.kind !== "Active" || !x.ui_entry) continue;
      const b = document.createElement("button");
      b.type = "button";
      b.className = "btn btn-sm map-ext-btn";
      b.textContent = x.name;
      b.title = x.description || x.name;
      b.addEventListener("click", () => ouvrir(x));
      zone.appendChild(b);
    }
  }

  function ouvrir(x) {
    const dlg = document.getElementById("dlg-extension");
    document.getElementById("dlg-extension-title").textContent = x.name;
    const cadre = document.getElementById("extension-frame");
    const iframe = document.createElement("iframe");
    iframe.src = `${ORIGINE}/${encodeURIComponent(x.id)}/${x.ui_entry}`;
    iframe.title = x.name;
    cadre.textContent = "";
    cadre.appendChild(iframe);
    panneau = { id: x.id, iframe };
    dlg.showModal();
  }

  function fermer() {
    document.getElementById("extension-frame").textContent = "";
    panneau = null;
  }

  window.addEventListener("message", async (ev) => {
    if (!panneau || ev.source !== panneau.iframe.contentWindow) return;
    const m = ev.data || {};
    const repondre = (r) => panneau && panneau.iframe.contentWindow.postMessage({ gws: "reply", rid: m.rid, ...r }, "*");
    if (m.gws === "escape") {
      // Échap pressé dans l'extension : la touche ne sort pas de l'iframe,
      // l'extension la relaie et la fenêtre se ferme comme avec « Fermer ».
      const dlg = document.getElementById("dlg-extension");
      if (dlg.open) dlg.close();
      return;
    }
    if (m.gws === "ready") {
      const projet = typeof state !== "undefined" && state.project ? state.project.id : null;
      panneau.iframe.contentWindow.postMessage({ gws: "context", project_id: projet }, "*");
    } else if (m.gws === "call" && typeof m.method === "string") {
      const projet = typeof state !== "undefined" && state.project ? state.project.id : null;
      try {
        const ok = await inv("extension_call", {
          id: panneau.id,
          method: m.method,
          payload: { ...(m.payload || {}), project_id: projet },
        });
        repondre({ ok });
      } catch (e) {
        repondre({ err: String(e) });
      }
    }
  });

  document.addEventListener("DOMContentLoaded", () => {
    const dlg = document.getElementById("dlg-extension");
    dlg.addEventListener("close", fermer);
    document.getElementById("btn-close-extension").addEventListener("click", () => dlg.close());
    document.getElementById("btn-settings").addEventListener("click", rafraichir);
    // Catalogue : lu seulement quand le groupe Extensions est ouvert (réseau).
    const groupe = document.querySelector('details[data-group="extensions"]');
    if (groupe) groupe.addEventListener("toggle", () => { if (groupe.open) rendreCatalogue(); });
    // Les DLL se chargent en arrière-plan au démarrage : on relit l'état un
    // peu plus tard pour faire apparaître le bouton sans attendre les Paramètres.
    rafraichir();
    setTimeout(rafraichir, 2500);
  });

  return { rafraichir };
})();

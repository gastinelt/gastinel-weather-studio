// Modale Paramètres : groupes repliables (état mémorisé) et recherche en temps réel.
//
// Ce module ne fait que de l'affichage : il ne lit ni n'écrit aucun réglage,
// ne touche à aucun id ni name de champ et n'appelle aucune commande Rust.
// Les handlers de main.js / extensions.js restent seuls maîtres des valeurs.
//
// Unité de recherche : chaque enfant direct de `.settings-group-body`
// (un fieldset, ou le bandeau d'avertissement Infoclimat). Une unité
// correspond si TOUS les mots saisis se retrouvent dans son texte visible,
// ses placeholders, ou son attribut `data-keywords`. Si la requête correspond
// au titre / aux mots-clés du groupe lui-même, tout le groupe est affiché.
// Le texte est lu au moment de la frappe : les éléments ajoutés plus tard
// (liste des extensions, case « bêta » une fois portée...) sont donc
// retrouvables sans rien déclarer ici.
(() => {
  const CLE_STOCKAGE = "gws_settings_groups";

  function lireEtat() {
    try {
      const brut = localStorage.getItem(CLE_STOCKAGE);
      const v = brut ? JSON.parse(brut) : null;
      return v && typeof v === "object" ? v : {};
    } catch (_) {
      return {};
    }
  }

  function ecrireEtat(etat) {
    try {
      localStorage.setItem(CLE_STOCKAGE, JSON.stringify(etat));
    } catch (_) {
      /* stockage indisponible : l'état ne sera simplement pas mémorisé */
    }
  }

  // Minuscules, sans accents, espaces normalisés : « Mises à jour » ≈ « mise a jour ».
  function normaliser(t) {
    return String(t || "")
      .normalize("NFD")
      .replace(/[̀-ͯ]/g, "")
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, " ")
      .trim();
  }

  function texteUnite(n) {
    const morceaux = [n.textContent, n.getAttribute("data-keywords") || ""];
    for (const c of n.querySelectorAll("[placeholder]")) morceaux.push(c.getAttribute("placeholder"));
    for (const c of n.querySelectorAll("[data-keywords]")) morceaux.push(c.getAttribute("data-keywords"));
    return normaliser(morceaux.join(" "));
  }

  // Correspondance en début de mot : « ign » trouve « IGN » mais pas « consigne ».
  function correspond(texte, termes) {
    const t0 = " " + texte;
    return termes.every((t) => t0.includes(" " + t));
  }

  // Surlignage via l'API CSS Custom Highlight : aucun nœud du DOM n'est
  // modifié (pas de <mark> injecté au milieu des libellés que main.js
  // réécrit). Absente, la recherche fonctionne sans surlignage.
  const surlignageDispo = typeof CSS !== "undefined" && CSS.highlights && typeof Highlight === "function";

  function surligner(racines, termesBruts) {
    if (!surlignageDispo) return;
    CSS.highlights.delete("settings-search");
    if (!termesBruts.length) return;
    const plages = [];
    for (const racine of racines) {
      const it = document.createTreeWalker(racine, NodeFilter.SHOW_TEXT);
      let noeud;
      while ((noeud = it.nextNode())) {
        const brut = noeud.nodeValue;
        if (!brut.trim()) continue;
        // Normalisation caractère par caractère pour garder les positions.
        let norm = "";
        const pos = [];
        for (let i = 0; i < brut.length; i++) {
          // Un caractère non alphanumérique devient une espace (comme dans normaliser).
          const c = normaliser(brut[i]) || " ";
          for (let k = 0; k < c.length; k++) pos.push(i);
          norm += c;
        }
        for (const t of termesBruts) {
          let from = 0;
          let idx;
          while (t && (idx = norm.indexOf(t, from)) !== -1) {
            if (idx > 0 && /[a-z0-9]/.test(norm[idx - 1])) {
              from = idx + 1;
              continue;
            }
            const r = new Range();
            r.setStart(noeud, pos[idx]);
            r.setEnd(noeud, pos[idx + t.length - 1] + 1);
            plages.push(r);
            from = idx + t.length;
          }
        }
      }
    }
    if (plages.length) CSS.highlights.set("settings-search", new Highlight(...plages));
  }

  function init() {
    const dlg = document.getElementById("dlg-settings");
    const champ = document.getElementById("settings-search");
    const croix = document.getElementById("settings-search-clear");
    const vide = document.getElementById("settings-search-empty");
    if (!dlg || !champ) return;
    const groupes = [...dlg.querySelectorAll("details.settings-group")];

    // État ouvert / fermé mémorisé. Par défaut, seul le premier groupe est ouvert.
    const etat = lireEtat();
    groupes.forEach((g, i) => {
      const id = g.dataset.group;
      g.open = id in etat ? !!etat[id] : i === 0;
    });

    // Ouverture programmatique : marquée pour que l'événement toggle
    // (asynchrone) ne soit pas pris pour un geste de l'utilisateur.
    function ouvrir(g, v) {
      if (g.open === v) return;
      g.dataset.auto = "1";
      g.open = v;
    }

    let enRecherche = false;
    for (const g of groupes) {
      g.addEventListener("toggle", () => {
        if (g.dataset.auto) {
          delete g.dataset.auto;
          return;
        }
        if (enRecherche) return; // pendant une recherche : rien n'est mémorisé
        const e = lireEtat();
        e[g.dataset.group] = g.open;
        ecrireEtat(e);
      });
    }

    function appliquer() {
      const requete = normaliser(champ.value);
      const termes = requete.split(" ").filter(Boolean);
      if (croix) croix.hidden = !champ.value;

      if (!termes.length) {
        if (enRecherche) {
          enRecherche = false;
          dlg.classList.remove("is-searching");
          const e = lireEtat();
          groupes.forEach((g, i) => {
            g.hidden = false;
            for (const u of g.querySelectorAll(".settings-group-body > *")) u.classList.remove("search-hidden");
            const id = g.dataset.group;
            ouvrir(g, id in e ? !!e[id] : i === 0);
          });
        }
        if (vide) vide.hidden = true;
        surligner([], []);
        return;
      }

      enRecherche = true;
      dlg.classList.add("is-searching");
      let total = 0;
      const visibles = [];
      for (const g of groupes) {
        const titre = g.querySelector("summary");
        const texteGroupe = normaliser((titre ? titre.textContent : "") + " " + (g.dataset.keywords || ""));
        const toutLeGroupe = correspond(texteGroupe, termes);
        let n = 0;
        for (const u of g.querySelectorAll(".settings-group-body > *")) {
          const ok = toutLeGroupe || correspond(texteUnite(u), termes);
          u.classList.toggle("search-hidden", !ok);
          if (ok) {
            n++;
            visibles.push(u);
          }
        }
        g.hidden = n === 0;
        if (n > 0) ouvrir(g, true);
        if (n > 0 && titre) visibles.push(titre);
        total += n;
      }
      if (vide) vide.hidden = total > 0;
      surligner(visibles, termes);
    }

    function effacer() {
      champ.value = "";
      appliquer();
    }

    champ.addEventListener("input", appliquer);
    champ.addEventListener("search", appliquer); // croix native de type=search
    champ.addEventListener("keydown", (e) => {
      // Échap efface d'abord la recherche ; un second Échap ferme la modale.
      if (e.key === "Escape" && champ.value) {
        e.preventDefault();
        e.stopPropagation();
        effacer();
      }
    });
    croix?.addEventListener("click", () => {
      effacer();
      champ.focus();
    });
    // Une modale rouverte repart d'une recherche vide.
    dlg.addEventListener("close", effacer);
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();

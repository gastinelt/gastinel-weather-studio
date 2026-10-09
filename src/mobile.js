/*
 * Gastinel Weather Studio — l'accès au RC-5 sur téléphone.
 *
 * ## Pourquoi un module à part, et pas un réglage de `elitech.js`
 *
 * Le poste fixe et le téléphone ne partagent pas la *forme* de l'accès aux
 * loggers, seulement son *contenu*. Sur poste fixe, une page dédiée
 * (`elitech.js`) tient une colonne d'onglets : plusieurs loggers ouverts en même
 * temps, on passe de l'un à l'autre sans rien fermer. Sur téléphone, il n'y a ni
 * la largeur pour une colonne d'onglets, ni le geste pour en changer : la
 * navigation y est une pile, et le bouton retour du système en est le seul
 * mécanisme (cf. `main.js`, `navigateAndroidBack`).
 *
 * Faire tenir les deux formes dans `elitech.js` aurait voulu dire un `if` mobile
 * à chaque fonction de navigation — `selectionner`, `ouvrirOnglet`,
 * `fermerOnglet`, `rendreOnglets` — pour un module dont c'est justement tout le
 * propos. Les deux formes vivent donc côte à côte, et **une seule des deux est
 * branchée** selon la plateforme (`estMobile()`).
 *
 * Ce qui est partagé, en revanche, l'est réellement :
 *
 *  * `GwsRc5Panel` — le panneau d'un logger, identique au bouton près : export,
 *    effacement sous confirmation stricte, heure, graphique, informations de
 *    l'appareil. Rien n'en a été recopié ni réécrit ici ;
 *  * la détection — une seule source, le fil de surveillance du backend relayé
 *    par `main.js`. Ce module ne sonde rien ;
 *  * l'animation de la loupe — les mêmes classes CSS (`.radar`, `.radar-onde`,
 *    `.detect-card`…) que l'onglet de détection du poste fixe, à l'échelle
 *    près. Un second visuel aurait fait deux applications au lieu d'une.
 *
 * ## Ce que le mobile retire
 *
 * Le RC-5 n'existe plus dans le contexte « projet » sur téléphone : ni la
 * notification de branchement (qui menait à l'import d'un projet ouvert), ni
 * l'onglet « USB direct (RC-5) » de la modale d'import. Un seul chemin, depuis
 * l'accueil, vers un écran par appareil. Deux chemins vers la même chose sur un
 * écran de cette taille, c'est un de trop.
 */

const GwsMobile = (() => {
  "use strict";

  const el = (id) => document.getElementById(id);

  function h(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else if (v === true) node.setAttribute(k, "");
      else node.setAttribute(k, v);
    }
    for (const c of children.flat()) {
      if (c === null || c === undefined || c === false) continue;
      node.appendChild(typeof c === "string" ? document.createTextNode(c) : c);
    }
    return node;
  }

  /**
   * Sommes-nous sur le téléphone ?
   *
   * La seule cible mobile de ce projet est Android, et sa vue web annonce
   * « Android » dans son `userAgent` — c'est vrai de la WebView système comme de
   * Chrome, et faux de la WebView2 de Windows. Pas de test de largeur : une
   * fenêtre de poste fixe rétrécie n'est pas un téléphone, et faire basculer
   * l'application entière sur un redimensionnement serait une surprise.
   *
   * `localStorage["gws.forcer-mobile"]` force la réponse (`"1"` / `"0"`). Ce
   * n'est pas un réglage de l'application — aucun écran ne l'expose : c'est le
   * moyen de regarder la mise en page mobile depuis un navigateur de bureau
   * pendant qu'on la construit.
   */
  function estMobile() {
    try {
      const force = window.localStorage.getItem("gws.forcer-mobile");
      if (force === "1") return true;
      if (force === "0") return false;
    } catch (e) {
      /* localStorage indisponible : on retombe sur la détection normale. */
    }
    return /Android/i.test(window.navigator.userAgent || "");
  }

  /** Dépendances injectées par `main.js` (cf. `init`). */
  let ctx = null;
  /** Vrai une fois `activer()` passé, c'est-à-dire sur téléphone seulement. */
  let actif = false;

  /** Loggers annoncés par la surveillance, par port. Même carte que sur poste fixe. */
  const detectes = new Map();
  /** Panneaux déjà construits, par port. Un panneau survit à un retour à l'accueil. */
  const panneaux = new Map();
  /** Port du logger affiché en plein écran, ou `null` si on est sur l'accueil. */
  let ouvert = null;

  // ------------------------------------------------------------ les widgets

  /**
   * Redessine la liste des loggers détectés, sous la loupe.
   *
   * Même règle que sur poste fixe : les widgets déjà affichés sont conservés
   * tels quels, sinon leur animation d'apparition repartirait à chaque cycle de
   * surveillance (une seconde).
   *
   * Une différence assumée avec le poste fixe : un logger déjà **ouvert** reste
   * dans la liste. Là-bas, la carte disparaissait parce qu'elle promettait
   * « ouvrir dans un nouvel onglet » et que l'onglet existait déjà. Ici, il n'y
   * a pas d'onglet à retrouver ailleurs — le widget *est* le seul chemin vers
   * l'écran du logger, et le faire disparaître le rendrait inatteignable.
   */
  function rendreWidgets() {
    const zone = el("home-rc5-found");
    if (!zone) return;
    const liste = [...detectes.values()];

    const vus = new Set();
    for (const carte of [...zone.children]) {
      if (liste.some((d) => d.portName === carte.dataset.port)) vus.add(carte.dataset.port);
      else carte.remove();
    }

    for (const d of liste) {
      if (vus.has(d.portName)) continue;
      zone.appendChild(
        h(
          "button",
          {
            type: "button",
            class: "detect-card",
            "data-port": d.portName,
            onclick: () => ouvrirLogger(d.portName),
          },
          h("span", {
            class: "detect-card-name",
            text: GwsRc5Panel.libellePour(d.loggerSerial, d.portName, d.modelLabel),
          }),
          h("span", {
            class: "detect-card-meta",
            text:
              `${d.portName}` +
              (d.recCount === null || d.recCount === undefined
                ? ""
                : ` · ${Number(d.recCount).toLocaleString("fr-FR")} mesure(s)`),
          }),
          h("span", {
            class: "detect-card-hint",
            text: "Toucher pour ouvrir cet appareil.",
          })
        )
      );
    }

    const statut = el("home-rc5-status");
    if (!statut) return;
    if (liste.length === 1) statut.textContent = "Un logger RC-5 a été trouvé.";
    else if (liste.length > 1) {
      statut.textContent = `${liste.length} loggers RC-5 ont été trouvés.`;
    } else {
      statut.textContent =
        "Recherche d'un logger RC-5 branché… Branchez le boîtier en USB, il sera reconnu " +
        "tout seul.";
    }
  }

  // ------------------------------------------------------ l'écran d'un logger

  /**
   * Construit — ou retrouve — le panneau d'un logger.
   *
   * Retrouver plutôt que reconstruire n'est pas une optimisation : une lecture
   * de mémoire prend une dizaine de secondes de va-et-vient série, et revenir
   * d'un aller-retour à l'accueil pour la refaire à l'identique serait une
   * punition. Le panneau garde donc son graphique et son rapport.
   */
  function panneauPour(portName) {
    const existant = panneaux.get(portName);
    if (existant) return existant;
    const d = detectes.get(portName) || { portName };
    const panneau = GwsRc5Panel.create({
      portName,
      modelLabel: d.modelLabel,
      loggerSerial: d.loggerSerial,
      invoke: ctx.invoke,
      onToast: ctx.onToast,
      onError: ctx.onError,
      confirmDuplicate: ctx.confirmDuplicate,
      onSaved: ctx.onSaved,
      // Renommer le logger doit se voir tout de suite sur le widget de
      // l'accueil : c'est là qu'on le choisit.
      onIdentity: () => rendreWidgets(),
      // La croix du panneau ferme l'écran comme le bouton retour : il n'y a pas
      // d'onglet derrière à révéler, seulement l'accueil.
      onClose: () => retourAccueil(),
    });
    panneau.element.hidden = true;
    el("rc5-mobile-host").appendChild(panneau.element);
    panneaux.set(portName, panneau);
    return panneau;
  }

  /** Ouvre l'écran plein d'un logger. */
  async function ouvrirLogger(portName) {
    const panneau = panneauPour(portName);
    for (const [port, p] of panneaux) p.element.hidden = port !== portName;
    ouvert = portName;

    el("view-projects").hidden = true;
    el("view-rc5-mobile").hidden = false;

    // La liste des projets est chargée à chaque venue plutôt qu'une fois : un
    // projet créé entre-temps doit pouvoir recevoir la série qu'on vient lire.
    try {
      const projets = await ctx.listProjects();
      await panneau.remplirProjets(projets, null);
    } catch (e) {
      ctx.onError(String(e));
    }

    // Activé **après** l'affichage : Chart.js mesure son conteneur, et un
    // conteneur encore masqué a une largeur nulle.
    panneau.activer();
  }

  /**
   * Revient à l'accueil depuis l'écran d'un logger.
   *
   * @returns {boolean} vrai si un écran a bien été quitté — c'est ce que le
   *   bouton retour Android attend pour savoir s'il doit, lui, ne rien faire.
   */
  function retourAccueil() {
    if (ouvert === null) return false;
    ouvert = null;
    el("view-rc5-mobile").hidden = true;
    el("view-projects").hidden = false;
    rendreWidgets();
    return true;
  }

  // ------------------------------------------- entrées venant de `main.js`

  /** Un logger vient d'être annoncé par la surveillance. */
  function noteDetection(payload) {
    if (!actif) return;
    const portName = payload.port.port_name;
    detectes.set(portName, {
      portName,
      modelLabel: payload.model_label,
      loggerSerial: payload.logger_serial || null,
      recCount: payload.rec_count,
    });
    // Rebranché sur le port d'un écran déjà construit : il reprend vie plutôt
    // que d'en construire un second sur le même appareil.
    const panneau = panneaux.get(portName);
    if (panneau && !panneau.branche) panneau.rebrancher();
    rendreWidgets();
  }

  /** Le logger d'un port a disparu. */
  function noteDebranchement(portName) {
    if (!actif) return;
    detectes.delete(portName);
    const panneau = panneaux.get(portName);
    if (panneau) panneau.debrancher();
    rendreWidgets();
  }

  /** Vrai quand l'écran d'un logger est au premier plan. */
  function ecranLoggerOuvert() {
    return ouvert !== null;
  }

  // ------------------------------------------------------------ cycle de vie

  function init(options) {
    ctx = options;
  }

  /**
   * Bascule l'application en disposition téléphone.
   *
   * Appelée une seule fois au démarrage, et seulement si `estMobile()`. Tout ce
   * qu'elle fait tient dans une classe sur `<body>` — la moitié haute / moitié
   * basse de l'accueil est en CSS — et dans le câblage du bouton retour de
   * l'écran d'un logger.
   */
  function activer() {
    actif = true;
    document.body.classList.add("gws-mobile");
    el("home-rc5").hidden = false;
    const retour = el("rc5-mobile-back");
    if (retour) retour.addEventListener("click", () => retourAccueil());
    rendreWidgets();
  }

  return {
    estMobile,
    init,
    activer,
    estActif: () => actif,
    noteDetection,
    noteDebranchement,
    ecranLoggerOuvert,
    retourAccueil,
    ouvrirLogger,
  };
})();

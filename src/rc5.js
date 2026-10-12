/*
 * Un logger RC-5 branché, et **un seul** — poste fixe et téléphone.
 *
 * Ce module ne connaît pas la page qui l'accueille : il fabrique un panneau
 * complet (graphique, horloge, enregistrement, export, effacement) autour d'un
 * port série donné, et rend une poignée pour le piloter. C'est la page
 * `GwsElitech` qui décide combien de panneaux existent et lequel est visible.
 *
 * ## Pourquoi une fabrique, et non un module à état unique
 *
 * La version précédente parlait au document par des identifiants fixes
 * (`el("rc5-chart")`, `el("rc5-save")`…). Un seul jeu d'éléments, donc un seul
 * logger affichable : brancher deux RC-5 n'en montrait qu'un, et rien dans le
 * code ne le disait. Ici chaque panneau clone `#tpl-rc5-panel` et ne cherche ses
 * éléments que **dans son propre clone** (`q()`), ce qui rend l'affichage
 * simultané possible par construction plutôt que par discipline.
 *
 * ## Ce qui n'est pas refait ici
 *
 * Rien du métier. Le tableau vaut toujours :
 *
 * | Ce qu'on fait | Ce qui le fait réellement |
 * |---|---|
 * | Lire le logger | `read_rc5` — la même commande que l'onglet d'import |
 * | Tracer les données | Chart.js vendorisé, comme le bloc Croisement |
 * | Vérifier l'heure | `check_rc5_clock` → `elitech_serial::compare_clock` |
 * | Remettre à l'heure | `write_rc5` (heure seule) → `elitech_serial::write_device` |
 * | Enregistrer | `import_rc5` → `persist_import`, donc même empreinte SHA-256 |
 * | Démarrer | rien : le RC-5 démarre par son bouton ▶, l'assistant relit l'état (`identify_rc5`) |
 * | Arrêter (sans effacer) | `stop_rc5` → `elitech_serial::stop_recording` |
 * | Modifier les réglages | `write_rc5` → `elitech_serial::write_device` |
 * | Effacer | `reset_rc5` → `elitech_serial::clear_memory_at` |
 * | Exporter | `build_rc5_export` → `export::build_rc5_csv` / `build_rc5_txt` |
 *
 * Toutes ces commandes reçoivent désormais le **port du panneau**, jamais
 * `null` : avec plusieurs loggers branchés, « le premier RC-5 trouvé » ne
 * désigne plus rien de précis.
 *
 * ## Le nom de l'appareil
 *
 * Le protocole transmet un numéro de série (tronqué à dix caractères). Il
 * identifie l'appareil, mais ne dit rien de ce qu'on en fait — « E205004333 »
 * ne se retient pas. Le crayon à côté du titre permet de lui donner un nom
 * libre, rangé dans le `localStorage` sous cette identité : le même boîtier
 * rebranché sur un autre port se retrouve avec son nom. Rien n'est envoyé au
 * backend, et la base n'en sait rien — c'est un repère d'écran, pas une donnée.
 *
 * ## Le graphique
 *
 * Les points arrivent **déjà sous-échantillonnés** du backend (`Rc5Analysis.curve`,
 * 1 500 points au plus, minimum et maximum de chaque seau conservés). Ce module ne
 * calcule aucune statistique — même règle que `cross.js`, et pour la même raison :
 * l'agrégation appartient au Rust, qui a les données sous la main.
 *
 * L'axe porte l'heure **brute du logger**, non corrigée. C'est délibéré : la page
 * montre l'appareil tel qu'il est, y compris quand son horloge dérive. La
 * correction n'intervient qu'à l'enregistrement, comme pour toutes les sources.
 * L'axe est donc étiqueté comme tel, sans jamais dire « UTC ».
 */

const GwsRc5Panel = (() => {
  "use strict";

  const el = (id) => document.getElementById(id);

  function h(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v);
    }
    for (const c of children.flat()) {
      if (c === null || c === undefined || c === false) continue;
      node.appendChild(typeof c === "string" ? document.createTextNode(c) : c);
    }
    return node;
  }

  function clear(node) {
    while (node.firstChild) node.removeChild(node.firstChild);
  }

  /**
   * Au-delà, un écart d'horloge n'est pas une **dérive**.
   *
   * L'horloge d'un RC-5 dérive de quelques minutes par an au plus. Un écart de
   * plusieurs heures ou de plusieurs mois signale une horloge arrêtée ou remise
   * à zéro (pile changée), ou un fuseau mal choisi. Les mesures, elles, sont
   * datées depuis l'instant de départ et restent justes : recopier cet écart
   * dans « Dérive » les décalerait toutes — c'est ce qui faisait finir une
   * série de décembre 2025… le jour de la lecture.
   */
  const DERIVE_PLAUSIBLE_MAX_S = 3600;

  /** « 288 jours », « 5 h », « 12 min » : un écart lisible. */
  function ecartLisible(secondes) {
    const s = Math.abs(secondes);
    if (s >= 172800) return `${Math.round(s / 86400)} jours`;
    if (s >= 7200) return `${Math.round(s / 3600)} h`;
    if (s >= 120) return `${Math.round(s / 60)} min`;
    return `${s} s`;
  }

  /** Phrase dite quand un écart trop grand n'est **pas** repris comme dérive. */
  function ecartNonRepris(secondes) {
    return (
      ` L'écart (${secondes > 0 ? "avance" : "retard"} de ${ecartLisible(secondes)}) est trop grand ` +
      "pour une dérive : l'horloge a sans doute été arrêtée ou remise à zéro, ou le fuseau " +
      "choisi n'est pas le bon. Il n'est pas repris dans le champ « Dérive » : les dates des " +
      "mesures, calculées depuis l'instant de départ, restent celles du logger."
    );
  }

  const nombre = (v) => (v === null || v === undefined ? "—" : Number(v).toLocaleString("fr-FR"));
  const degres = (v) => (v === null || v === undefined ? "—" : `${Number(v).toFixed(1)} °C`);

  /** « 15 min », « 1 h », « 45 s » — même intention que `fmtDuration` de main.js. */
  function duree(secondes) {
    if (!secondes) return "—";
    if (secondes % 3600 === 0) return `${secondes / 3600} h`;
    if (secondes % 60 === 0) return `${secondes / 60} min`;
    return `${secondes} s`;
  }

  /** Horodatage naïf du backend (`2026-05-01T14:07:31`) → millisecondes. */
  function instant(brut) {
    // Le `Z` est ajouté volontairement : ces instants **ne sont pas** de l'UTC,
    // mais l'axe doit les afficher tels quels, sans que le fuseau du PC les
    // déplace. Les lire comme de l'UTC et les rendre en UTC les laisse intacts.
    return Date.parse(`${String(brut).replace(" ", "T")}Z`);
  }

  // ------------------------------------------------------- noms personnalisés

  /**
   * Clé de rangement des noms libres.
   *
   * Le `localStorage` et non la base : ce nom ne décrit pas une mesure, il
   * décrit le boîtier posé sur le bureau. Le mettre en base le ferait voyager
   * avec un export de données auxquelles il n'appartient pas.
   */
  const CLE_NOMS = "gws.rc5.noms";

  function tousLesNoms() {
    try {
      const brut = window.localStorage.getItem(CLE_NOMS);
      const objet = brut ? JSON.parse(brut) : {};
      return objet && typeof objet === "object" ? objet : {};
    } catch (e) {
      // Un stockage indisponible (quota, navigation privée) ne doit pas
      // empêcher de lire un logger : on retombe sur les noms par défaut.
      console.warn("Noms de loggers illisibles :", e);
      return {};
    }
  }

  /**
   * Identité durable d'un appareil.
   *
   * Le numéro de série d'abord : c'est le seul repère qui survive à un
   * changement de port. À défaut (un logger qui ne l'a pas encore transmis), le
   * port fait office d'identité — le nom suivra alors la prise, pas le boîtier,
   * et c'est le mieux qu'on puisse faire sans identifiant.
   */
  function identite(serial, portName) {
    const s = (serial || "").trim();
    return s ? `sn:${s}` : `port:${portName || "?"}`;
  }

  function nomEnregistre(deviceId) {
    const n = tousLesNoms()[deviceId];
    return typeof n === "string" && n.trim() ? n.trim() : null;
  }

  function enregistrerNom(deviceId, nom) {
    const noms = tousLesNoms();
    const propre = (nom || "").trim();
    if (propre) noms[deviceId] = propre;
    else delete noms[deviceId];
    try {
      window.localStorage.setItem(CLE_NOMS, JSON.stringify(noms));
    } catch (e) {
      console.warn("Nom de logger non enregistré :", e);
    }
  }

  /** Le libellé par défaut, quand aucun nom libre n'a été donné. */
  function libelleParDefaut(modele, serial) {
    const m = modele || "Logger RC-5";
    return serial ? `${m} — n° ${serial}` : m;
  }

  /** Nom d'usage d'un appareil qu'aucun panneau n'affiche encore. */
  function libellePour(serial, portName, modele) {
    return nomEnregistre(identite(serial, portName)) || libelleParDefaut(modele, serial);
  }

  // ------------------------------------------- modale d'effacement (unique)

  /**
   * Panneau qui a ouvert la modale d'effacement.
   *
   * Elle est unique dans le document — effacer deux loggers à la fois n'a aucun
   * sens, et deux modales empilées encore moins. Ce champ dit lequel des
   * panneaux la commande reprendra à la confirmation.
   */
  let proprietaireEffacement = null;
  let modaleCablee = false;

  function cablerModaleEffacement() {
    if (modaleCablee) return;
    modaleCablee = true;

    el("rc5-reset-cancel").addEventListener("click", () => el("dlg-rc5-reset").close());
    el("rc5-reset-confirm").addEventListener("click", () => {
      if (proprietaireEffacement) proprietaireEffacement.effacer();
    });
    // Le bouton ne s'active qu'au mot exact : c'est la garde qui empêche le clic
    // réflexe sur un bouton rouge. Le backend refuse de toute façon sans lui.
    el("rc5-reset-word").addEventListener("input", (e) => {
      el("rc5-reset-confirm").disabled = e.target.value.trim() !== "EFFACER";
    });
    el("dlg-rc5-reset").addEventListener("close", () => {
      proprietaireEffacement = null;
    });
  }

  // ------------------------------------------------- états de fonctionnement

  /** `WorkStatus` du backend → texte. `Unknown(n)` arrive comme `{ Unknown: n }`. */
  function libelleEtat(statut) {
    switch (statut) {
      case "NotStarted":
        return "en attente de démarrage (appui long sur ▶)";
      case "Running":
        return "enregistrement en cours";
      case "Stopped":
        return "arrêté";
      case "DelayedStart":
        return "démarrage différé programmé";
      default:
        return statut && typeof statut === "object" && "Unknown" in statut
          ? `inconnu (code ${statut.Unknown})`
          : "inconnu";
    }
  }

  /** « Aucun », « 30 min », « 1 h », « 1 h 30 » — le délai est en demi-heures. */
  function libelleDelai(demiHeures) {
    if (!demiHeures) return "Aucun";
    const h = Math.floor(demiHeures / 2);
    const demi = demiHeures % 2 === 1;
    if (h === 0) return "30 min";
    return demi ? `${h} h 30` : `${h} h`;
  }

  const ouiNon = (v) => (v ? "activé" : "désactivé");

  function libelleAlarme(code) {
    return code === 0 ? "désactivée" : `code ${code}`;
  }

  // ----------------------------------------- modale des réglages (unique)

  /**
   * Panneau qui a ouvert la modale des réglages, et ce que le logger annonçait
   * à l'ouverture (`Rc5Identity`). Le bilan compare le formulaire à cette
   * relecture, jamais à une lecture plus ancienne : c'est elle que l'écriture
   * va remplacer.
   */
  let reglages = null;
  let reglagesCables = false;

  /**
   * Champs de la modale. Une seule source pour lire, remplir et comparer :
   * oublier un champ ici le ferait disparaître des trois à la fois, ce qui se voit.
   */
  function lireFormulaireReglages() {
    const n = (id) => {
      const v = el(id).value.trim();
      return v === "" ? NaN : Number(v.replace(",", "."));
    };
    const zero = (id) => (el(id).value.trim() === "" ? 0 : n(id));
    return {
      sample_interval_seconds: zero("rc5-set-h") * 3600 + zero("rc5-set-m") * 60 + zero("rc5-set-s"),
      start_delay_half_hours: Number(el("rc5-set-delay").value),
      stop_button_enabled: el("rc5-set-stop").value === "1",
      upper_limit_celsius: n("rc5-set-upper"),
      lower_limit_celsius: n("rc5-set-lower"),
      fahrenheit: el("rc5-set-unit").value === "1",
      station_no: n("rc5-set-station"),
      key_tone_enabled: el("rc5-set-tone").value === "1",
      alarm: Number(el("rc5-set-alarm").value),
      temperature_calibration: n("rc5-set-calib"),
    };
  }

  function remplirFormulaireReglages(r) {
    const i = r.sample_interval_seconds;
    el("rc5-set-h").value = String(Math.floor(i / 3600));
    el("rc5-set-m").value = String(Math.floor((i % 3600) / 60));
    el("rc5-set-s").value = String(i % 60);
    el("rc5-set-delay").value = String(Math.min(r.start_delay_half_hours, 12));
    el("rc5-set-stop").value = r.stop_button_enabled ? "1" : "0";
    el("rc5-set-upper").value = r.upper_limit_celsius.toFixed(1);
    el("rc5-set-lower").value = r.lower_limit_celsius.toFixed(1);
    el("rc5-set-unit").value = r.fahrenheit ? "1" : "0";
    el("rc5-set-station").value = String(r.station_no);
    el("rc5-set-tone").value = r.key_tone_enabled ? "1" : "0";
    // Une valeur hors liste (logger réglé ailleurs) resterait invisible dans le
    // menu : on l'y ajoute plutôt que d'afficher « Désactivée » à tort.
    const alarme = el("rc5-set-alarm");
    if (![...alarme.options].some((o) => Number(o.value) === r.alarm)) {
      alarme.appendChild(h("option", { value: String(r.alarm) }, `Code ${r.alarm} (actuel)`));
    }
    alarme.value = String(r.alarm);
    el("rc5-set-calib").value = r.temperature_calibration.toFixed(1);
  }

  /**
   * Ce que l'écriture fera, en clair. Mêmes bornes que `Rc5Settings::validate`
   * côté Rust — qui reste juge : un réglage refusé ici le serait aussi là-bas,
   * mais on le dit avant le clic plutôt qu'après.
   */
  function bilanReglages() {
    const { identite: id } = reglages;
    const avant = id.settings;
    const apres = lireFormulaireReglages();
    const dixiemes = (v) => Math.round(v * 10);

    let erreur = null;
    const i = apres.sample_interval_seconds;
    if (!Number.isFinite(i) || i < 10 || i > 86400) {
      erreur = "L'intervalle doit être compris entre 10 secondes et 24 heures.";
    } else if (![apres.upper_limit_celsius, apres.lower_limit_celsius].every((v) => Number.isFinite(v) && v >= -40 && v <= 90)) {
      erreur = "Les limites doivent être comprises entre −40 °C et 90 °C.";
    } else if (dixiemes(apres.upper_limit_celsius) - dixiemes(apres.lower_limit_celsius) < 5) {
      erreur = "La limite haute doit dépasser la limite basse d'au moins 0,5 °C.";
    } else if (!Number.isInteger(apres.station_no) || apres.station_no < 1 || apres.station_no > 254) {
      erreur = "L'adresse doit être un nombre entier entre 1 et 254.";
    } else if (!Number.isFinite(apres.temperature_calibration) || Math.abs(apres.temperature_calibration) > 5) {
      erreur = "La correction de température doit rester entre −5,0 °C et +5,0 °C.";
    }

    const info = el("rc5-set-info").value.trim();
    const infoAvant = (id.device.user_info || "").trim();
    const serie = el("rc5-set-serial").value.trim();
    const serieAvant = (id.device.device_number || "").trim();
    if (!erreur && !/^[\x20-\x7E]*$/.test(info)) {
      erreur = "La description n'accepte que des lettres sans accent, des chiffres et la ponctuation courante.";
    } else if (!erreur && serie !== serieAvant && !/^[A-Za-z0-9]{1,10}$/.test(serie)) {
      erreur = "Le numéro de série : 1 à 10 lettres sans accent ou chiffres.";
    }

    const changements = [];
    const comparer = (nom, a, b, fmt = (x) => x) => {
      if (a !== b) changements.push(`${nom} : ${fmt(a)} → ${fmt(b)}`);
    };
    if (!erreur) {
      comparer("intervalle", avant.sample_interval_seconds, apres.sample_interval_seconds, duree);
      comparer("délai de démarrage", avant.start_delay_half_hours, apres.start_delay_half_hours, libelleDelai);
      comparer("bouton d'arrêt", avant.stop_button_enabled, apres.stop_button_enabled, ouiNon);
      comparer("limite haute", dixiemes(avant.upper_limit_celsius), dixiemes(apres.upper_limit_celsius), (d) => `${(d / 10).toFixed(1)} °C`);
      comparer("limite basse", dixiemes(avant.lower_limit_celsius), dixiemes(apres.lower_limit_celsius), (d) => `${(d / 10).toFixed(1)} °C`);
      comparer("unité de l'écran", avant.fahrenheit, apres.fahrenheit, (f) => (f ? "°F" : "°C"));
      comparer("adresse", avant.station_no, apres.station_no);
      comparer("tonalité des boutons", avant.key_tone_enabled, apres.key_tone_enabled, (v) => (v ? "activée" : "désactivée"));
      comparer("alarme sonore", avant.alarm, apres.alarm, libelleAlarme);
      comparer("correction de température", dixiemes(avant.temperature_calibration), dixiemes(apres.temperature_calibration), (d) => `${(d / 10).toFixed(1)} °C`);
    }

    const demande = {};
    if (changements.length) demande.settings = apres;
    if (!erreur && info !== infoAvant) demande.user_info = info;
    if (!erreur && serie !== serieAvant) demande.device_number = serie;
    if (el("rc5-set-clock").checked) demande.clock_timezone_offset_seconds = reglages.panneau.fuseau();

    const efface = Boolean(demande.settings);
    return {
      erreur,
      demande,
      changements,
      efface,
      sensible: efface || demande.device_number !== undefined,
      vide: Object.keys(demande).length === 0,
    };
  }

  function majBilanReglages() {
    const b = bilanReglages();
    const { identite: id, panneau } = reglages;
    const boite = el("rc5-set-bilan");
    clear(boite);
    el("rc5-set-error").hidden = true;

    if (b.erreur) {
      boite.className = "banner is-danger";
      boite.textContent = `${b.erreur} Rien ne sera écrit tant que ce n'est pas corrigé.`;
    } else if (b.vide) {
      boite.className = "banner";
      boite.textContent = "Rien à écrire : aucun réglage n'a changé.";
    } else {
      const lignes = [];
      if (b.changements.length) lignes.push(`Réglages modifiés — ${b.changements.join(" ; ")}.`);
      if (b.demande.user_info !== undefined) lignes.push("La description change (n'efface rien).");
      if (b.demande.device_number !== undefined) {
        lignes.push(
          `Le numéro de série change : ${id.device.device_number || "aucun"} → ${b.demande.device_number}. ` +
            "Le nom donné à ce logger le suivra ; les séries déjà en base gardent l'ancien numéro."
        );
      }
      if (b.demande.clock_timezone_offset_seconds !== undefined) lignes.push("Le logger sera remis à l'heure.");
      if (b.efface) {
        lignes.push(
          `La mémoire du logger sera effacée${id.rec_count ? ` (${nombre(id.rec_count)} mesure(s))` : ""}, ` +
            "comme dans ElitechLog : c'est le seul moyen d'écrire ces paramètres."
        );
        if (id.device.work_status === "Running" || id.device.work_status === "DelayedStart") {
          lignes.push("L'enregistrement en cours sera arrêté avant l'écriture.");
        }
        if (id.rec_count && panneau.lectureNonSauvegardee()) {
          lignes.push("Des mesures sont affichées dans ce panneau : sauvegardez-les d'abord si ce n'est pas fait.");
        }
        lignes.push("Ensuite, le logger attendra son démarrage : maintenez son bouton ▶ environ 5 secondes.");
      } else {
        lignes.push("Rien n'est effacé.");
      }
      boite.className = b.efface ? "banner is-danger" : "banner is-ok";
      for (const l of lignes) boite.appendChild(h("p", { text: l }));
    }

    el("rc5-set-word-field").hidden = !b.sensible || Boolean(b.erreur);
    const mot = el("rc5-set-word").value.trim();
    el("rc5-set-confirm").disabled = Boolean(b.erreur) || b.vide || (b.sensible && mot !== "CONFIRMER");
    el("rc5-set-confirm").textContent = b.efface ? "Effacer et écrire sur le logger" : "Écrire sur le logger";
  }

  function cablerModaleReglages() {
    if (reglagesCables) return;
    reglagesCables = true;

    const delai = el("rc5-set-delay");
    for (let d = 0; d <= 12; d += 1) delai.appendChild(h("option", { value: String(d) }, libelleDelai(d)));

    const form = el("form-rc5-settings");
    form.addEventListener("input", () => reglages && majBilanReglages());
    form.addEventListener("change", () => reglages && majBilanReglages());
    el("rc5-set-cancel").addEventListener("click", () => el("dlg-rc5-settings").close());
    el("rc5-set-confirm").addEventListener("click", () => {
      if (!reglages) return;
      const b = bilanReglages();
      if (b.erreur || b.vide) return;
      reglages.panneau.ecrireReglages(b.demande, b.sensible ? el("rc5-set-word").value.trim() : null);
    });
    el("dlg-rc5-settings").addEventListener("close", () => {
      reglages = null;
    });
  }

  function ouvrirModaleReglages(panneau, identiteLue) {
    cablerModaleReglages();
    reglages = { panneau, identite: identiteLue };
    el("rc5-set-who").textContent =
      `« ${panneau.libelle()} » — ${identiteLue.model_label}, ${libelleEtat(identiteLue.device.work_status)}, ` +
      `${nombre(identiteLue.rec_count)} mesure(s) en mémoire. Valeurs relues à l'instant sur l'appareil.`;
    remplirFormulaireReglages(identiteLue.settings);
    el("rc5-set-info").value = identiteLue.device.user_info || "";
    el("rc5-set-serial").value = identiteLue.device.device_number || "";
    el("rc5-set-clock").checked = true;
    el("rc5-set-word").value = "";
    el("rc5-set-error").hidden = true;
    majBilanReglages();
    el("dlg-rc5-settings").showModal();
  }

  // --------------------------------------------------------------- fabrique

  /**
   * Construit le panneau d'un logger.
   *
   * @param {object}   ctx                 dépendances communes (cf. `elitech.js`)
   * @param {string}   ctx.portName        port du logger — jamais nul ici
   * @param {string}   [ctx.modelLabel]    modèle annoncé par la détection
   * @param {string}   [ctx.loggerSerial]  numéro de série annoncé par la détection
   * @param {function} ctx.invoke          pont Tauri
   * @param {function} ctx.onToast         message éphémère
   * @param {function} [ctx.onError]       ignoré : une erreur reste dans le cadre du panneau
   * @param {function} ctx.confirmDuplicate dialogue de ré-import partagé
   * @param {function} ctx.onSaved         rappel après enregistrement en base
   * @param {function} ctx.onIdentity      rappel quand le nom ou l'identité changent
   * @param {function} [ctx.onClose]       rappel au clic sur la croix de fermeture
   */
  function create(ctx) {
    cablerModaleEffacement();

    const racine = el("tpl-rc5-panel").content.firstElementChild.cloneNode(true);
    /** Cherche **dans ce panneau** : c'est ce qui rend deux panneaux possibles. */
    const q = (nom) => racine.querySelector(`[data-r="${nom}"]`);

    const invoke = ctx.invoke;
    const onToast = ctx.onToast || (() => {});

    let portName = ctx.portName;
    let modelLabel = ctx.modelLabel || "Logger RC-5";
    let loggerSerial = ctx.loggerSerial || null;
    let deviceId = identite(loggerSerial, portName);

    let chart = null;
    /** Dernière lecture reçue de `read_rc5`, telle quelle. */
    let lecture = null;
    /** `false` dès que la surveillance annonce le débranchement de ce port. */
    let branche = true;
    /** Une lecture a-t-elle déjà été lancée ? (l'onglet lit à sa première venue) */
    let luUneFois = false;

    const instance = {};

    // ------------------------------------------------------------ graphique

    function detruireGraphique() {
      if (chart) {
        chart.destroy();
        chart = null;
      }
    }

    function messageGraphique(texte) {
      const zone = q("chart");
      clear(zone);
      zone.classList.add("is-empty");
      zone.appendChild(h("p", { class: "placeholder", text: texte }));
    }

    function tracer(analyse) {
      const points = analyse.curve || [];
      if (points.length === 0) {
        detruireGraphique();
        messageGraphique("Le logger ne contient aucune mesure.");
        return;
      }

      const zone = q("chart");
      // Canvas recréé à chaque tracé, comme dans `cross.js` : Chart.js garde des
      // écouteurs sur celui qu'il a instrumenté, et le réutiliser après `destroy()`
      // laisse des infobulles fantômes de la lecture précédente.
      detruireGraphique();
      clear(zone);
      zone.classList.remove("is-empty");
      const canvas = h("canvas");
      zone.appendChild(canvas);

      const donnees = points.map((p) => ({
        x: instant(p.raw_timestamp),
        y: p.average_celsius,
        min: p.min_celsius,
        max: p.max_celsius,
        n: p.count,
      }));
      // La réduction LTTB exige des x croissants ; l'horloge d'un logger n'en
      // garantit rien, le tri ne coûte presque rien sur 1 500 points.
      donnees.sort((a, b) => a.x - b.x);

      chart = new Chart(canvas.getContext("2d"), {
        type: "line",
        data: {
          datasets: [
            {
              label: instance.libelle(),
              data: donnees,
              borderColor: "#0072b2",
              backgroundColor: "#0072b2",
              borderWidth: 1.6,
              // Aucun lissage : une courbe de température ne doit pas être
              // embellie par une interpolation qui inventerait des creux.
              tension: 0,
              pointRadius: 0,
              pointHoverRadius: 3,
            },
          ],
        },
        options: {
          responsive: true,
          maintainAspectRatio: false,
          animation: false,
          normalized: true,
          parsing: false,
          interaction: { mode: "index", intersect: false },
          scales: {
            x: {
              type: "time",
              time: { isoWeekday: 1 },
              grid: { color: "#eef1f5" },
              ticks: { maxRotation: 0, autoSkipPadding: 16, font: { size: 10 } },
              title: {
                display: true,
                // Jamais « UTC » : ces instants sont ceux de l'horloge du logger,
                // non corrigés. Le dire évite de croire à un décalage des données.
                text: "heure du logger (brute, non corrigée)",
                font: { size: 10 },
              },
            },
            y: {
              title: { display: true, text: "°C", font: { size: 10 } },
              grid: {
                // Le zéro est la ligne qui compte dans ce projet.
                color: (c) => (c.tick.value === 0 ? "#b9c2cd" : "#eef1f5"),
                lineWidth: (c) => (c.tick.value === 0 ? 1.5 : 1),
              },
              ticks: { font: { size: 10 } },
            },
          },
          plugins: {
            // N'agit qu'au-delà de 4 points par pixel (graphique très étroit) ;
            // les statistiques affichées viennent du backend, pas de ces points.
            decimation: { enabled: true, algorithm: "lttb" },
            legend: { display: false },
            tooltip: {
              callbacks: {
                label: (item) => {
                  const b = item.raw;
                  if (b.n <= 1) return `${degres(b.y)}`;
                  // Sur un seau agrégé, la moyenne seule masquerait le minimum —
                  // c'est-à-dire précisément ce qu'on vient chercher.
                  return `moy ${degres(b.y)} · min ${degres(b.min)} · max ${degres(b.max)} (${b.n} mes.)`;
                },
              },
            },
          },
        },
      });
    }

    // -------------------------------------------------------------- lecture

    function ligne(dl, cle, valeur) {
      dl.appendChild(h("dt", { text: cle }));
      dl.appendChild(h("dd", { text: valeur }));
    }

    /**
     * Le titre porte le nom **d'usage**, la ligne du dessous porte l'identité.
     *
     * Renommer ne doit jamais faire perdre de vue quel boîtier on regarde : le
     * modèle et le numéro de série restent affichés sous le nom libre, quoi
     * qu'il arrive.
     */
    function majTitre() {
      const nom = instance.libelle();
      const defaut = libelleParDefaut(modelLabel, loggerSerial);
      q("title").textContent = nom;
      // La ligne d'identité n'apparaît que sous un nom libre : tant que le titre
      // *est* l'identité, la répéter en dessous n'apprendrait rien.
      q("identite").textContent = defaut;
      q("identite").hidden = nom === defaut;
    }

    function afficherEntete(a) {
      modelLabel = a.model_label || modelLabel;
      majIdentite(a.logger_serial);
      q("sub").textContent =
        `${nombre(a.measurement_count)} mesure(s) en mémoire · ${a.work_status_label} · ` +
        `cadence ${duree(a.sample_interval_seconds)} · lu sur ${a.port_name}`;
      q("chart-note").textContent =
        a.curve && a.curve.length < a.measurement_count
          ? `${nombre(a.measurement_count)} mesures, regroupées en ${nombre(a.curve.length)} points ` +
            `(minimum et maximum de chaque groupe conservés)`
          : `${nombre(a.measurement_count)} mesure(s), tracées une à une`;
    }

    /**
     * Fixe l'identité de l'appareil à partir de ce qu'il vient de transmettre.
     *
     * La détection ne connaît pas toujours le numéro de série (un port occupé
     * est annoncé sans avoir été lu). Le premier échange complet le donne :
     * c'est le moment de faire suivre le nom libre du port vers le boîtier.
     */
    function majIdentite(serial) {
      const nouveau = identite(serial || loggerSerial, portName);
      if (nouveau !== deviceId) {
        const ancien = nomEnregistre(deviceId);
        // Le nom donné à « ce port » suit l'appareil dès qu'il se nomme : sans
        // ce report, renommer avant la première lecture perdrait le nom.
        if (ancien && !nomEnregistre(nouveau)) enregistrerNom(nouveau, ancien);
        deviceId = nouveau;
      }
      if (serial) loggerSerial = serial;
      majTitre();
      if (ctx.onIdentity) ctx.onIdentity(instance);
    }

    /** Lit toute la mémoire du logger. Ne touche jamais à la base. */
    async function relire() {
      luUneFois = true;
      const bouton = q("reload");
      bouton.disabled = true;
      cacherErreur();
      messageGraphique("Lecture du logger… (une dizaine de secondes pour une mémoire pleine)");
      try {
        const a = await invoke("read_rc5", { port_name: portName });
        lecture = a;
        portName = a.port_name;
        afficherEntete(a);
        tracer(a);
        afficherInformations(a);
        majEtat(a.work_status);
        majBoutonsExport();
        majApercuDecalage();

        // Les avertissements matériels (logger encore en marche, calibration
        // interne, Fahrenheit) valent autant ici qu'à l'import : ils changent la
        // lecture des chiffres affichés juste au-dessus.
        const rapport = q("report");
        clear(rapport);
        rapport.hidden = a.read_warnings.length === 0;
        rapport.className = "report";
        for (const w of a.read_warnings) {
          rapport.appendChild(h("p", { class: "banner", text: w.message }));
        }
      } catch (e) {
        lecture = null;
        detruireGraphique();
        afficherInformations(null);
        majBoutonsExport();
        messageGraphique("La lecture n'a pas abouti.");
        montrerErreur(String(e));
      } finally {
        bouton.disabled = !branche;
      }
    }

    /** Les deux boutons d'export ne valent que par la lecture qu'ils exportent. */
    function majBoutonsExport() {
      q("export-csv").disabled = !lecture;
      q("export-txt").disabled = !lecture;
    }

    // --------------------------------------------------------------- export

    /**
     * Exporte la lecture affichée, dans le format demandé.
     *
     * Le `reading_id` est exigé par le backend : il garantit qu'on exporte le lot
     * qui est sous les yeux de l'utilisateur, et non une relecture qui pourrait
     * différer si le logger enregistre encore. Une lecture ratée laisse donc les
     * deux boutons désactivés — mieux vaut pas de bouton qu'un bouton qui produit
     * un fichier d'origine incertaine.
     */
    async function exporter(format) {
      if (!lecture) return;
      const boutons = [q("export-csv"), q("export-txt")];
      for (const b of boutons) b.disabled = true;
      cacherErreur();
      try {
        const contenu = await invoke("build_rc5_export", {
          reading_id: lecture.reading_id,
          format,
        });
        const chemin = await invoke("save_export_file", {
          content: contenu.content,
          suggested_name: contenu.suggested_name,
        });
        // `null` : l'utilisateur a fermé la boîte d'enregistrement. Ce n'est pas
        // une erreur, et le dire comme telle serait déroutant.
        if (chemin) onToast(`Export enregistré : ${chemin}`);
      } catch (e) {
        montrerErreur(String(e));
      } finally {
        for (const b of boutons) b.disabled = !lecture;
      }
    }

    // ---------------------------------------------- informations de l'appareil

    /** « 15,0 °C », ou « — ». Les seuils et la calibration sont des réglages. */
    const reglageDegres = (v) =>
      v === null || v === undefined ? "—" : `${Number(v).toFixed(1)} °C`;

    /**
     * Remplit le tableau du bas avec tout ce que le protocole a transmis.
     *
     * Rien n'y est calculé : ce sont les champs de `DevInfo` tels que
     * `elitech_serial.rs` les a décodés — y compris le firmware et la pile, que
     * l'on a longtemps crus absents du protocole (ElitechLog les lit aux octets
     * 4 et 28). Une valeur non transmise est **nommée** plutôt que masquée.
     */
    function afficherInformations(a) {
      const bloc = q("infos");
      const dl = q("infos-kv");
      clear(dl);
      if (!a) {
        bloc.hidden = true;
        return;
      }

      ligne(dl, "Nom donné à l'appareil", nomEnregistre(deviceId) || "aucun (nom par défaut)");
      ligne(dl, "Modèle", a.model_label);
      ligne(dl, "Version du firmware", a.firmware_label || "non transmise");
      ligne(
        dl,
        "Pile",
        a.battery_percent === null || a.battery_percent === undefined
          ? "non transmise"
          : `${a.battery_percent} % (valeur annoncée par l'appareil)`
      );
      ligne(
        dl,
        "Numéro de série",
        a.logger_serial ? `${a.logger_serial} (tronqué à 10 caractères)` : "non transmis"
      );
      ligne(dl, "Description du voyage", a.user_info || "—");
      ligne(dl, "État de fonctionnement", a.work_status_label);
      ligne(dl, "Cadence d'enregistrement", duree(a.sample_interval_seconds));
      ligne(dl, "Mesures en mémoire", nombre(a.measurement_count));
      ligne(dl, "Port de lecture", a.port_name);
      ligne(
        dl,
        "Horloge du logger à la lecture",
        a.device_clock ? a.device_clock.replace("T", " ") : "non transmise"
      );
      ligne(dl, "Seuil d'alarme haut", reglageDegres(a.upper_limit_celsius));
      ligne(dl, "Seuil d'alarme bas", reglageDegres(a.lower_limit_celsius));
      const r = a.settings;
      if (r) {
        ligne(dl, "Délai de démarrage", libelleDelai(r.start_delay_half_hours));
        ligne(dl, "Bouton d'arrêt", ouiNon(r.stop_button_enabled));
        ligne(dl, "Unité de l'écran", r.fahrenheit ? "°F (les mesures lues restent en °C)" : "°C");
        ligne(dl, "Adresse", String(r.station_no));
        ligne(dl, "Tonalité des boutons", r.key_tone_enabled ? "activée" : "désactivée");
        ligne(dl, "Alarme sonore", libelleAlarme(r.alarm));
      }
      // Cette ligne n'est pas décorative : une calibration non nulle est déjà
      // comprise dans les températures tracées au-dessus.
      ligne(
        dl,
        "Correction interne de l'appareil",
        `${reglageDegres(a.temperature_calibration)} (déjà comprise dans les valeurs)`
      );

      q("infos-absents").textContent =
        "Le nom donné à l'appareil ne vient pas du protocole : il est rangé sur ce poste " +
        "seulement. Tout le reste est lu sur le logger ; « Modifier les réglages » permet " +
        "de changer ce qui est modifiable.";
      bloc.hidden = false;
    }

    // -------------------------------------------------------------- horloge

    async function verifierHorloge() {
      const bouton = q("check-clock");
      bouton.disabled = true;
      cacherErreur();
      try {
        const r = await invoke("check_rc5_clock", {
          port_name: portName,
          timezone_offset_seconds: Number(q("tz").value) || 0,
        });
        portName = r.port_name;

        const dl = q("clock-kv");
        clear(dl);
        ligne(dl, "Heure du logger", r.check.device_clock || "illisible");
        ligne(dl, "Heure du PC (UTC)", String(r.check.host_utc).replace("T", " ").replace("Z", ""));
        ligne(
          dl,
          "Écart avec l'UTC",
          r.check.offset_vs_utc_seconds === null
            ? "—"
            : `${r.check.offset_vs_utc_seconds >= 0 ? "+" : ""}${r.check.offset_vs_utc_seconds} s`
        );
        ligne(
          dl,
          "Dérive (fuseau retiré)",
          r.check.drift_seconds === null
            ? "—"
            : `${r.check.drift_seconds >= 0 ? "+" : ""}${r.check.drift_seconds} s`
        );
        dl.hidden = false;

        const verdict = q("clock-verdict");
        const d = r.check.drift_seconds;
        const plausible = d !== null && Math.abs(d) <= DERIVE_PLAUSIBLE_MAX_S;
        verdict.textContent = r.check.message + (d !== null && !plausible ? ecartNonRepris(d) : "");
        verdict.className = `banner${r.check.severity === "aligned" ? " is-ok" : ""}${
          r.check.severity === "severe" ? " is-danger" : ""
        }`;
        verdict.hidden = false;

        // La dérive mesurée alimente directement le champ de l'enregistrement :
        // c'est tout l'intérêt d'avoir les deux sur le même écran.
        if (plausible) {
          q("drift").value = String(d);
          majApercuDecalage();
        }
      } catch (e) {
        montrerErreur(String(e));
      } finally {
        bouton.disabled = !branche;
      }
    }

    /**
     * Écrit l'heure du PC dans le logger, sans rien effacer.
     *
     * Les mesures déjà prises gardent l'ancienne heure. La dérive mesurée juste
     * avant passe dans le champ de l'enregistrement : elle vaut pour la lecture
     * affichée, faite avant la remise à l'heure. Une lecture faite **après**
     * mélangerait des mesures décalées et d'autres à l'heure, qu'aucune
     * correction unique ne remet d'aplomb — d'où la question posée avant.
     */
    async function remettreALHeure() {
      if (lecture && lecture.measurement_count > 0) {
        const ok = await window.confirm(
          `« ${instance.libelle()} » contient ${nombre(lecture.measurement_count)} mesure(s). ` +
            "Elles garderont l'heure d'avant la remise à l'heure.\n\n" +
            "Le plus simple : sauvegardez-les d'abord (la dérive mesurée sera reprise " +
            "dans le champ « Dérive »). Remettre à l'heure maintenant ?"
        );
        if (!ok) return;
      }
      const bouton = q("set-clock");
      bouton.disabled = true;
      cacherErreur();
      try {
        const r = await invoke("write_rc5", {
          port_name: portName,
          request: { clock_timezone_offset_seconds: fuseau() },
          confirmation: null,
        });
        portName = r.port_name;
        const avant = r.report.clock ? r.report.clock.drift_before_seconds : null;
        let texte = r.message;
        if (avant !== null && avant !== undefined && avant !== 0) {
          texte += ` L'horloge avait ${avant > 0 ? "+" : ""}${avant} s d'écart.`;
          if (Math.abs(avant) > DERIVE_PLAUSIBLE_MAX_S) {
            texte += ecartNonRepris(avant);
          } else if (r.report.before.rec_count > 0) {
            // Les mesures déjà en mémoire portent cet écart : le garder dans le
            // champ « Dérive » évite de le perdre — « Vérifier l'heure » dira
            // désormais « à l'heure ».
            q("drift").value = String(avant);
            majApercuDecalage();
            texte += lecture
              ? " Cet écart est repris dans le champ « Dérive » pour sauvegarder la lecture affichée."
              : " Cet écart est repris dans le champ « Dérive » : il vaut pour les mesures prises " +
                "avant la remise à l'heure. Relisez puis sauvegardez sans tarder.";
          }
        }
        const verdict = q("clock-verdict");
        verdict.textContent = texte;
        verdict.className = r.report.mismatches.length ? "banner is-danger" : "banner is-ok";
        verdict.hidden = false;
        q("clock-kv").hidden = true;
        majEtat(r.report.after.work_status);
        onToast("Logger remis à l'heure.");
      } catch (e) {
        montrerErreur(String(e));
      } finally {
        bouton.disabled = !branche;
      }
    }

    // ------------------------------------------------- démarrage et arrêt

    function majEtat(statut) {
      q("etat").textContent = `État du logger : ${libelleEtat(statut)}.`;
    }

    /**
     * Assistant de démarrage. Il n'envoie rien : le RC-5 n'a pas de commande de
     * démarrage (ElitechLog n'en a pas non plus pour lui). Il relit l'état, dit
     * quoi faire, et relit de nouveau pour confirmer.
     */
    async function demarrer(verification = false) {
      const bouton = q("start");
      bouton.disabled = true;
      cacherErreur();
      const guide = q("start-guide");
      try {
        const id = await invoke("identify_rc5", { port_name: portName });
        portName = id.port_name;
        const statut = id.device.work_status;
        majEtat(statut);
        clear(guide);
        const p = (texte, cls) => guide.appendChild(h("p", { class: cls || null, text: texte }));
        if (statut === "Running") {
          const depuis = id.device.start_time ? ` depuis ${id.device.start_time.replace("T", " ")} (heure du logger)` : "";
          p(`${verification ? "C'est parti : le" : "Le"} logger enregistre${depuis}, toutes les ${duree(id.sample_interval_seconds)}.`, "banner is-ok");
        } else if (statut === "DelayedStart") {
          p(
            `Démarrage différé en cours : la première mesure viendra après le délai réglé (${libelleDelai(id.settings.start_delay_half_hours)}).`,
            "banner is-ok"
          );
        } else if (statut === "Stopped") {
          p(
            "Ce logger est arrêté, et un RC-5 arrêté ne repart pas. Sauvegardez ses données, " +
              "puis réinitialisez-le (plus bas) : il attendra alors son démarrage par ▶.",
            "banner"
          );
        } else if (statut === "NotStarted") {
          if (verification) p("Le logger n'a pas encore démarré.", "banner");
          p(
            "Maintenez le bouton ▶ du logger environ 5 secondes, jusqu'à ce que le symbole ▶ " +
              "s'affiche à l'écran. Si rien ne se passe, débranchez le câble USB et recommencez."
          );
          if (id.settings.start_delay_half_hours > 0) {
            p(`Délai réglé : ${libelleDelai(id.settings.start_delay_half_hours)} entre l'appui et la première mesure.`);
          }
          p("Pensez à le remettre à l'heure avant (bouton « Remettre à l'heure » plus haut) : rien n'est effacé.", "hint");
          guide.appendChild(
            h("button", { type: "button", class: "btn", onclick: () => demarrer(true) }, "Vérifier le démarrage")
          );
        } else {
          p(`État inattendu (${libelleEtat(statut)}) : relisez le logger avant toute action.`, "banner is-danger");
        }
        guide.hidden = false;
      } catch (e) {
        montrerErreur(String(e));
      } finally {
        bouton.disabled = !branche;
      }
    }

    async function arreterEnregistrement() {
      const ok = await window.confirm(
        `Arrêter l'enregistrement de « ${instance.libelle()} » ?\n\n` +
          "Les mesures déjà prises restent en mémoire. Mais un RC-5 arrêté ne repart pas : " +
          "pour enregistrer de nouveau, il faudra le réinitialiser, ce qui efface sa mémoire."
      );
      if (!ok) return;
      const bouton = q("stop");
      bouton.disabled = true;
      cacherErreur();
      const resultat = q("stop-result");
      try {
        const r = await invoke("stop_rc5", { port_name: portName });
        resultat.textContent =
          `Arrêté : état du logger « ${r.work_status} », ` +
          `${nombre(r.rec_count)} mesure(s) toujours en mémoire.`;
        resultat.className = "banner is-ok";
        resultat.hidden = false;
        q("etat").textContent = `État du logger : ${r.work_status}.`;
        q("start-guide").hidden = true;
      } catch (e) {
        montrerErreur(String(e));
      } finally {
        bouton.disabled = !branche;
      }
    }

    /** Fuseau réglé sur le logger, en secondes, tel que choisi dans « Heure du logger ». */
    const fuseau = () => Number(q("tz").value) || 0;

    /** `clock_offset_seconds` = secondes à AJOUTER au brut pour obtenir l'UTC. */
    function decalage() {
      const tz = Number(q("tz").value) || 0;
      const derive = Number(q("drift").value) || 0;
      return -(tz + derive);
    }

    function majApercuDecalage() {
      const tz = Number(q("tz").value) || 0;
      const derive = Number(q("drift").value) || 0;
      const offset = decalage();
      q("offset-hint").textContent =
        `Chaque mesure sera enregistrée en UTC = heure du logger ${offset >= 0 ? "+" : "−"} ` +
        `${Math.abs(offset)} s (fuseau ${tz === 0 ? "UTC" : `UTC+${tz / 3600}`}, dérive ${derive} s). ` +
        `L'heure brute est conservée : un recalibrage reste possible plus tard.` +
        periodeEnregistree(offset);
    }

    /**
     * « Période enregistrée : du … au … (UTC) », correction comprise. Une dérive
     * aberrante se voit alors avant l'enregistrement, et non après.
     */
    function periodeEnregistree(offset) {
      if (!lecture || !lecture.raw_period_start || !lecture.raw_period_end) return "";
      const fmt = (brut) =>
        new Date(instant(brut) + offset * 1000).toISOString().slice(0, 16).replace("T", " ");
      return ` Période enregistrée : du ${fmt(lecture.raw_period_start)} au ${fmt(lecture.raw_period_end)} (UTC).`;
    }

    // ---------------------------------------------------- projet et points

    /**
     * Remplit le choix du projet, puis celui du point.
     *
     * La page n'est plus atteinte depuis un projet ouvert : elle vit à côté
     * d'eux, et l'enregistrement doit donc dire **où** il enregistre. Le projet
     * ouvert, s'il y en a un, est présélectionné : sans cela le geste habituel
     * demanderait un clic de plus qu'avant.
     */
    async function remplirProjets(projets, projetSuggere) {
      const select = q("project");
      clear(select);
      for (const p of projets || []) {
        select.appendChild(h("option", { value: p.id }, p.name));
      }
      if (projetSuggere && (projets || []).some((p) => p.id === projetSuggere)) {
        select.value = projetSuggere;
      }
      select.disabled = (projets || []).length === 0;
      await chargerPoints(instance.pointSuggere);
    }

    async function chargerPoints(suggere) {
      const select = q("point");
      clear(select);
      const projectId = q("project").value;
      const hint = q("save-hint");
      if (!projectId) {
        q("save").disabled = true;
        hint.textContent =
          "Aucun projet : créez-en un depuis l'accueil pour pouvoir enregistrer cette " +
          "lecture. L'export ci-dessous, lui, reste disponible.";
        hint.hidden = false;
        return;
      }
      try {
        const points = await invoke("list_points", { project_id: projectId });
        for (const p of points) {
          select.appendChild(
            h("option", { value: p.id }, `${p.name} (${p.category}${p.size ? " " + p.size : ""})`)
          );
        }
        if (suggere && points.some((p) => p.id === suggere)) select.value = suggere;
        const vide = points.length === 0;
        q("save").disabled = vide;
        hint.hidden = !vide;
        if (vide) {
          hint.textContent =
            "Ce projet n'a aucun point : une série est toujours rattachée " +
            "à un point. L'export ci-dessous reste disponible.";
        }
      } catch (e) {
        montrerErreur(String(e));
      }
    }

    // -------------------------------------------------------- enregistrement

    async function sauvegarder() {
      cacherErreur();
      if (!lecture) {
        montrerErreur(
          "Lisez d'abord le logger : l'enregistrement reprend la lecture que vous venez " +
            "de voir, il ne relit pas l'appareil à l'aveugle."
        );
        return;
      }
      const pointId = q("point").value;
      if (!pointId) {
        montrerErreur("Choisissez le point auquel rattacher cette série.");
        return;
      }

      const bouton = q("save");
      bouton.disabled = true;
      try {
        const lancer = (force) =>
          invoke("import_rc5", {
            payload: {
              point_id: pointId,
              content: "",
              file_name: null,
              role: q("role").value,
              clock_offset_seconds: decalage(),
              raw_timestamp_was_utc: Number(q("tz").value) === 0,
              // Le numéro transmis par le logger : il figure dans la série, les
              // exports et le bilan, sans champ à remplir.
              logger_serial: lecture.logger_serial || null,
              label: null,
              reading_id: lecture.reading_id,
              port_name: lecture.port_name,
              force_duplicate: force,
            },
          });

        const rapport = await ctx.confirmDuplicate(lancer);
        if (!rapport) {
          onToast("Enregistrement annulé : ce contenu est déjà en base sur ce point.");
          return;
        }

        // Le jeton de lecture a été consommé par le backend : relire le logger
        // avant un second enregistrement, sinon la commande relirait l'appareil.
        const serie = lecture.logger_serial;
        lecture = null;
        majBoutonsExport();
        afficherRapport(rapport, serie);
        onToast(
          `${nombre(rapport.imported_count)} mesures enregistrées sur « ${rapport.point_name} ».`
        );
        if (ctx.onSaved) ctx.onSaved(q("project").value);
      } catch (e) {
        montrerErreur(String(e));
      } finally {
        bouton.disabled = false;
      }
    }

    function afficherRapport(r, serie) {
      const box = q("report");
      clear(box);
      box.className = "report is-ok";
      const dl = h("dl", { class: "kv" });
      ligne(dl, "Point", r.point_name);
      ligne(dl, "Logger", serie ? `n° ${serie}` : "numéro de série non transmis");
      ligne(dl, "Mesures enregistrées", nombre(r.imported_count));
      ligne(
        dl,
        "Période (UTC, corrigée)",
        `${String(r.period_start_utc || "—").replace("T", " ")} → ${String(
          r.period_end_utc || "—"
        ).replace("T", " ")}`
      );
      ligne(dl, "Correction appliquée", `${r.clock_offset_seconds} s`);
      ligne(dl, "Minimum sur la période", degres(r.stats.extreme_min_c));
      box.appendChild(dl);
      box.appendChild(
        h("p", {
          class: "muted",
          style: "margin-top:8px",
          text:
            "Ces mesures sont maintenant en base. Effacer la mémoire du logger ne les " +
            "supprimera pas.",
        })
      );
      box.hidden = false;
    }

    // ------------------------------------------------------------- réglages

    /** Relit l'appareil puis ouvre la modale : jamais de réglages périmés. */
    async function ouvrirReglages() {
      const bouton = q("settings");
      bouton.disabled = true;
      cacherErreur();
      try {
        const id = await invoke("identify_rc5", { port_name: portName });
        portName = id.port_name;
        majEtat(id.device.work_status);
        ouvrirModaleReglages(instance, id);
      } catch (e) {
        montrerErreur(String(e));
      } finally {
        bouton.disabled = !branche;
      }
    }

    /** Appelé par la modale avec la demande prête (`Rc5Write`). */
    async function ecrireReglages(demande, confirmation) {
      const bouton = el("rc5-set-confirm");
      bouton.disabled = true;
      const erreur = el("rc5-set-error");
      erreur.hidden = true;
      try {
        const r = await invoke("write_rc5", { port_name: portName, request: demande, confirmation });
        el("dlg-rc5-settings").close();
        portName = r.port_name;
        if (r.memory_cleared) memoireVidee("Mémoire effacée par l'écriture des réglages.");
        if (demande.device_number !== undefined) majIdentite(r.report.after.device_number);
        majEtat(r.report.after.work_status);

        const box = q("report");
        clear(box);
        box.className = r.report.mismatches.length ? "report" : "report is-ok";
        box.appendChild(h("p", { text: r.message }));
        box.hidden = false;
        onToast(r.memory_cleared ? "Réglages écrits ; la mémoire du logger a été vidée." : "Réglages écrits sur le logger.");
      } catch (e) {
        erreur.textContent = String(e);
        erreur.hidden = false;
        bouton.disabled = false;
      }
    }

    /** La lecture affichée ne décrit plus rien : l'appareil est vide. */
    function memoireVidee(texteGraphique) {
      lecture = null;
      detruireGraphique();
      majBoutonsExport();
      messageGraphique(texteGraphique);
      q("sub").textContent = "0 mesure en mémoire — le logger attend son démarrage (▶).";
    }

    // ------------------------------------------------------ réinitialisation

    function ouvrirConfirmationEffacement() {
      proprietaireEffacement = instance;
      const mesures = lecture ? lecture.measurement_count : null;
      el("rc5-reset-what").textContent = mesures
        ? `« ${instance.libelle()} » contient ${nombre(mesures)} mesure(s).`
        : `Le contenu de « ${instance.libelle()} » n'a pas été relu depuis l'ouverture de ` +
          "cet onglet.";

      // Le seul cas qui mérite d'être signalé avant d'effacer : des données lues,
      // affichées, et jamais enregistrées. L'application ne peut pas savoir si
      // elles l'ont été *ailleurs*, elle ne l'affirme donc pas — elle rappelle.
      const rappel = el("rc5-reset-unsaved");
      rappel.hidden = !lecture;
      rappel.textContent = lecture
        ? "Ces mesures sont affichées à l'écran mais rien ne dit qu'elles ont été " +
          "enregistrées : si ce n'est pas fait, utilisez d'abord « Sauvegarder les données »."
        : "";

      // Effacer un logger qui enregistre encore l'arrête au passage : c'est ce
      // qui rend l'effacement possible (voir `clear_memory`). Sur un RC-5,
      // l'appareil ne redémarrera pas seul — le dire *avant*, pas après.
      const enCours = el("rc5-reset-running");
      const marche = Boolean(lecture && /en cours/i.test(lecture.work_status_label || ""));
      enCours.hidden = !marche;
      enCours.textContent = marche
        ? "Ce logger enregistre actuellement. Il sera arrêté avant l'effacement, puis " +
          "attendra son démarrage : maintenez son bouton ▶ environ 5 secondes pour " +
          "relancer un enregistrement."
        : "";

      const mot = el("rc5-reset-word");
      mot.value = "";
      el("rc5-reset-confirm").disabled = true;
      el("rc5-reset-error").hidden = true;
      el("dlg-rc5-reset").showModal();
      mot.focus();
    }

    async function effacer() {
      const bouton = el("rc5-reset-confirm");
      bouton.disabled = true;
      const erreur = el("rc5-reset-error");
      erreur.hidden = true;
      try {
        const r = await invoke("reset_rc5", {
          port_name: portName,
          confirmation: el("rc5-reset-word").value.trim(),
          timezone_offset_seconds: el("rc5-reset-clock").checked ? fuseau() : null,
        });
        el("dlg-rc5-reset").close();

        memoireVidee("Mémoire effacée : le logger ne contient plus aucune mesure.");
        majEtat("NotStarted");
        q("start-guide").hidden = true;

        const box = q("report");
        clear(box);
        box.className = r.parameters_preserved ? "report is-ok" : "report";
        box.appendChild(h("p", { text: r.message }));
        for (const c of r.changed_parameters) {
          box.appendChild(h("p", { class: "banner is-danger", text: c }));
        }
        box.hidden = false;
        onToast(
          r.recording_stopped
            ? `${nombre(r.records_erased)} mesure(s) effacées ; le logger a été arrêté.`
            : `${nombre(r.records_erased)} mesure(s) effacées du logger.`
        );
      } catch (e) {
        erreur.textContent = String(e);
        erreur.hidden = false;
      } finally {
        bouton.disabled = el("rc5-reset-word").value.trim() !== "EFFACER";
      }
    }

    // --------------------------------------------------------------- erreurs

    /**
     * Une erreur, **un** affichage : le cadre du panneau, amené à l'écran.
     * Avant la 0.6.0, elle partait aussi dans un message rouge en bas de page —
     * le même texte deux fois.
     */
    function montrerErreur(message) {
      const box = q("error");
      box.textContent = message;
      box.hidden = false;
      box.scrollIntoView({ behavior: "smooth", block: "nearest" });
    }

    function cacherErreur() {
      q("error").hidden = true;
    }

    // -------------------------------------------------------------- renommage

    function ouvrirRenommage() {
      q("rename-form").hidden = false;
      q("title").hidden = true;
      q("rename").hidden = true;
      q("rename-input").value = instance.libelle();
      q("rename-input").focus();
      q("rename-input").select();
    }

    function fermerRenommage() {
      q("rename-form").hidden = true;
      q("title").hidden = false;
      q("rename").hidden = false;
    }

    function validerRenommage(event) {
      event.preventDefault();
      const saisi = q("rename-input").value.trim();
      // Le libellé par défaut retapé à l'identique n'est pas un nom libre : le
      // ranger figerait « RC-5 — n° X » et le nom cesserait de suivre l'appareil
      // quand celui-ci finit par transmettre son numéro de série.
      const libre = saisi && saisi !== libelleParDefaut(modelLabel, loggerSerial) ? saisi : "";
      enregistrerNom(deviceId, libre);
      fermerRenommage();
      majTitre();
      if (chart) {
        chart.data.datasets[0].label = instance.libelle();
        chart.update("none");
      }
      if (lecture) afficherInformations(lecture);
      if (ctx.onIdentity) ctx.onIdentity(instance);
      onToast(libre ? `Logger renommé « ${libre} ».` : "Nom par défaut rétabli.");
    }

    // ------------------------------------------------------ redimensionnement

    const surRedimensionnement = () => {
      if (chart) chart.resize();
    };
    window.addEventListener("resize", surRedimensionnement);

    /** Les boutons qui parlent au logger : coupés quand il est débranché. */
    const BOUTONS_MATERIEL = ["reload", "check-clock", "set-clock", "start", "stop", "settings", "reset"];

    // ---------------------------------------------------------------- câblage

    q("reload").addEventListener("click", relire);
    q("check-clock").addEventListener("click", verifierHorloge);
    q("set-clock").addEventListener("click", remettreALHeure);
    q("start").addEventListener("click", () => demarrer(false));
    q("stop").addEventListener("click", arreterEnregistrement);
    q("settings").addEventListener("click", ouvrirReglages);
    q("save").addEventListener("click", sauvegarder);
    q("tz").addEventListener("change", majApercuDecalage);
    q("drift").addEventListener("input", majApercuDecalage);
    q("project").addEventListener("change", () => chargerPoints(null));
    q("export-csv").addEventListener("click", () => exporter("csv"));
    q("export-txt").addEventListener("click", () => exporter("txt"));
    q("reset").addEventListener("click", ouvrirConfirmationEffacement);
    q("rename").addEventListener("click", ouvrirRenommage);
    q("rename-form").addEventListener("submit", validerRenommage);
    q("rename-cancel").addEventListener("click", fermerRenommage);
    // Ferme cet onglet seul, pas le logger : `elitech.js` décide de ce que ça
    // veut dire (retirer le panneau, choisir un autre onglet).
    if (ctx.onClose) q("close").addEventListener("click", () => ctx.onClose(instance));

    // ---------------------------------------------------------------- poignée

    Object.assign(instance, {
      element: racine,
      /** Point présélectionné à la première ouverture, posé par la page. */
      pointSuggere: null,

      get portName() {
        return portName;
      },
      get deviceId() {
        return deviceId;
      },
      get loggerSerial() {
        return loggerSerial;
      },
      get branche() {
        return branche;
      },

      /** Nom d'usage : celui qu'on a donné, sinon celui de l'appareil. */
      libelle() {
        return nomEnregistre(deviceId) || libelleParDefaut(modelLabel, loggerSerial);
      },

      remplirProjets,
      fuseau,
      ecrireReglages,
      /** Une lecture est affichée (et n'a pas été enregistrée depuis). */
      lectureNonSauvegardee() {
        return Boolean(lecture);
      },

      /** Première venue sur l'onglet : on lit, comme le faisait l'ancienne page. */
      activer() {
        if (chart) chart.resize();
        if (!luUneFois && branche) relire();
      },

      /**
       * Le logger vient d'être débranché.
       *
       * Le panneau reste : ce qui y est affiché a bien été lu, et le refermer
       * d'autorité ferait disparaître un rapport d'enregistrement que
       * l'utilisateur est peut-être en train de lire. Seules les actions qui
       * parlent au matériel sont coupées.
       */
      debrancher() {
        branche = false;
        for (const b of BOUTONS_MATERIEL) q(b).disabled = true;
        q("start-guide").hidden = true;
        q("offline").hidden = false;
        if (reglages && reglages.panneau === instance) el("dlg-rc5-settings").close();
      },

      /** Rebranché sur le même port : tout redevient possible. */
      rebrancher() {
        branche = true;
        for (const b of BOUTONS_MATERIEL) q(b).disabled = false;
        q("offline").hidden = true;
      },

      effacer,

      /** Referme proprement : le graphique garde des écouteurs tant qu'il vit. */
      destroy() {
        detruireGraphique();
        window.removeEventListener("resize", surRedimensionnement);
        if (proprietaireEffacement === instance) {
          el("dlg-rc5-reset").close();
          proprietaireEffacement = null;
        }
        if (reglages && reglages.panneau === instance) el("dlg-rc5-settings").close();
        racine.remove();
      },
    });

    // L'état initial vient après la poignée, et non avant : le titre affiche
    // `instance.libelle()`, qui n'existe qu'une fois `instance` complétée.
    majTitre();
    majApercuDecalage();
    majBoutonsExport();
    messageGraphique(
      "Ce logger n'a pas encore été lu. Cliquez sur « Relire le logger » pour voir sa mémoire."
    );

    return instance;
  }

  return { create, identite, nomEnregistre, libelleParDefaut, libellePour };
})();

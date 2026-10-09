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
 * | Enregistrer | `import_rc5` → `persist_import`, donc même empreinte SHA-256 |
 * | Arrêter (sans effacer) | `stop_rc5` → `elitech_serial::stop_recording` |
 * | Effacer | `reset_rc5` → `elitech_serial::clear_memory` |
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
   * @param {function} ctx.onError         message d'erreur global
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
    const onError = ctx.onError || (() => {});

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
        majBoutonsExport();
        q("serial").placeholder = a.logger_serial || "";
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
     * `elitech_serial.rs` les a décodés. Ce que le protocole ne transmet pas est
     * **nommé** plutôt que passé sous silence — chercher une version de firmware
     * absente coûte plus cher que lire une ligne qui dit qu'elle n'existe pas.
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
      // Cette ligne n'est pas décorative : une calibration non nulle est déjà
      // comprise dans les températures tracées au-dessus.
      ligne(
        dl,
        "Correction interne de l'appareil",
        `${reglageDegres(a.temperature_calibration)} (déjà comprise dans les valeurs)`
      );

      q("infos-absents").textContent =
        "Le protocole série du RC-5 ne transmet ni version de firmware, ni niveau de " +
        "batterie : ces deux informations sont absentes de l'appareil, pas de cet écran. " +
        "Le nom donné à l'appareil, à l'inverse, ne vient pas du protocole — il est rangé " +
        "sur ce poste seulement.";
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
        verdict.textContent = r.check.message;
        verdict.className = `banner${r.check.severity === "aligned" ? " is-ok" : ""}${
          r.check.severity === "severe" ? " is-danger" : ""
        }`;
        verdict.hidden = false;

        // La dérive mesurée alimente directement le champ de l'enregistrement :
        // c'est tout l'intérêt d'avoir les deux sur le même écran.
        if (r.check.drift_seconds !== null) {
          q("drift").value = String(r.check.drift_seconds);
          majApercuDecalage();
        }
      } catch (e) {
        montrerErreur(String(e));
      } finally {
        bouton.disabled = !branche;
      }
    }

    // ----------------------------------------------------- arrêt (sans effacer)

    async function arreterEnregistrement() {
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
      } catch (e) {
        montrerErreur(String(e));
      } finally {
        bouton.disabled = !branche;
      }
    }

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
        `L'heure brute est conservée : un recalibrage reste possible plus tard.`;
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
              logger_serial: q("serial").value.trim() || null,
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
        lecture = null;
        majBoutonsExport();
        afficherRapport(rapport);
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

    function afficherRapport(r) {
      const box = q("report");
      clear(box);
      box.className = "report is-ok";
      const dl = h("dl", { class: "kv" });
      ligne(dl, "Point", r.point_name);
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
        ? "Ce logger enregistre actuellement. Il sera arrêté avant l'effacement, et ne " +
          "redémarrera pas tout seul : il faudra le reprogrammer depuis ElitechLog pour " +
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
        });
        el("dlg-rc5-reset").close();

        // La lecture affichée ne décrit plus rien : l'appareil est vide.
        lecture = null;
        detruireGraphique();
        majBoutonsExport();
        messageGraphique("Mémoire effacée : le logger ne contient plus aucune mesure.");
        q("sub").textContent = r.recording_stopped
          ? "0 mesure en mémoire — logger arrêté puis réinitialisé."
          : "0 mesure en mémoire — logger réinitialisé.";

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

    function montrerErreur(message) {
      const box = q("error");
      box.textContent = message;
      box.hidden = false;
      onError(message);
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

    // ---------------------------------------------------------------- câblage

    q("reload").addEventListener("click", relire);
    q("check-clock").addEventListener("click", verifierHorloge);
    q("stop").addEventListener("click", arreterEnregistrement);
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
        q("reload").disabled = true;
        q("check-clock").disabled = true;
        q("stop").disabled = true;
        q("reset").disabled = true;
        q("offline").hidden = false;
      },

      /** Rebranché sur le même port : tout redevient possible. */
      rebrancher() {
        branche = true;
        q("reload").disabled = false;
        q("check-clock").disabled = false;
        q("stop").disabled = false;
        q("reset").disabled = false;
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

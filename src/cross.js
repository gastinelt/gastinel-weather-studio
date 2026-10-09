/*
 * Bloc « Croisement de données » (bas-gauche de la grille 2×2).
 *
 * Deux zones, **visibles en même temps** et jamais paginées : les statistiques
 * de synthèse en haut, les courbes superposées en bas. C'est là qu'est la valeur
 * du bloc : un tableau de moyennes, un tableur le fait très bien ; ce qu'il ne
 * montre pas, c'est la forme des courbes côte à côte dans le temps.
 *
 * ## Une seule fenêtre pour les deux zones
 *
 * Le sélecteur de période commande **les deux** zones. Ce n'est pas un détail de
 * confort : afficher une moyenne calculée sur neuf ans au-dessus d'une courbe
 * qui montre sept jours donnerait une comparaison fausse d'apparence rigoureuse.
 * L'unicité est garantie côté Rust — `crossing::build` lit les échantillons une
 * seule fois par point et en dérive les deux zones (voir `crossing.rs`).
 *
 * ## Ce que ce module ne fait pas
 *
 * Aucun calcul statistique. Pas de moyenne, pas de min/max, pas de rééchantil-
 * lonnage : tout arrive agrégé de `cross_analysis`. Sur l'archive ROMMA
 * (476 912 mesures), c'est ce qui fait la différence entre un bloc réactif et un
 * bloc qui fige la fenêtre à chaque clic sur la carte.
 *
 * ## Bibliothèque de graphique
 *
 * Chart.js 4.4.7, **vendorisé** dans `vendor/chartjs/` comme Leaflet : la CSP de
 * l'application interdit tout script externe (`script-src 'self'`), et l'app doit
 * fonctionner dans un refuge sans réseau. L'adaptateur de dates est écrit à la
 * main (`vendor/chartjs/adapter-utc.js`) pour éviter d'embarquer une seconde
 * bibliothèque — et pour tenir l'axe en UTC strict. Voir README § 11.
 */

const GwsCross = (() => {
  "use strict";

  /**
   * Palette des courbes — **une couleur par point sélectionné**, attribuée dans
   * l'ordre de sélection.
   *
   * ⚠️ Ce n'est délibérément pas la couleur de catégorie de la carte
   * (`COULEURS_CATEGORIE` dans `map.js`). Cette correspondance-là est
   * catégorie → couleur, pas point → couleur : comparer deux dépressions
   * fermées, le cas d'usage central du projet, donnerait deux courbes du même
   * bleu et un graphique illisible.
   *
   * La cohérence demandée est donc établie dans l'autre sens : c'est la carte qui
   * reprend ces couleurs pour l'**anneau de sélection** de ses marqueurs
   * (`GwsMap.setSelectionColors`). Le point coché en vermillon sur la carte est
   * la courbe vermillon du graphique. La pastille de catégorie, elle, reste au
   * centre du marqueur : aucune information n'est perdue.
   *
   * Palette dérivée d'Okabe–Ito (lisible en deutéranopie et protanopie), avec le
   * jaune remplacé par un or foncé — le jaune d'origine disparaît sur fond blanc.
   */
  const PALETTE = [
    "#0072b2", // bleu
    "#d55e00", // vermillon
    "#009e73", // vert
    "#cc79a7", // rose
    "#56b4e9", // bleu ciel
    "#b8860b", // or foncé
    "#6a3d9a", // violet
    "#333333", // graphite
  ];

  /**
   * Temporisation avant rechargement sur changement de sélection.
   *
   * 250 ms, contre 900 ms pour l'agent IA (`AUTO_ANALYSE_DEBOUNCE_MS` dans
   * `main.js`). Les deux protègent de la même rafale — cocher cinq points à la
   * souris produit cinq changements en deux secondes — mais pas du même coût :
   * l'agent consomme un quota d'API distant, ce bloc lit une base SQLite locale.
   * Attendre 900 ms ici ne protégerait rien et se sentirait à l'usage.
   */
  const DEBOUNCE_MS = 250;

  /** Largeur nominale d'un seau, par granularité (ms). Sert à détecter les trous. */
  const LARGEUR_SEAU_MS = {
    Hour: 3600e3,
    Day: 86400e3,
    Week: 604800e3,
    Month: 31 * 86400e3,
  };

  let invoke = null;
  let onError = null;
  /** Fenêtre courante, au format attendu par `crossing::TimeWindow`. */
  let fenetre = { kind: "last30_days" };
  /**
   * Ids sélectionnés, dans l'ordre où la carte les a cochés. C'est tout ce dont
   * ce module a besoin de `main.js` : noms, catégories et tailles arrivent avec
   * l'analyse, il ne relit jamais l'état du projet.
   */
  let selection = [];
  let timer = null;
  /** Numéro d'appel, pour ignorer une réponse doublée par une plus récente. */
  let sequence = 0;
  let chart = null;
  /** Dernière analyse rendue — la base de l'export à venir (cf. README § 11). */
  let derniereAnalyse = null;

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

  function clear(node) {
    while (node.firstChild) node.removeChild(node.firstChild);
  }

  // ------------------------------------------------------------------ formats

  const fmtTemp = (v, digits = 1) =>
    v === null || v === undefined ? "—" : `${v.toFixed(digits).replace(".", ",")} °C`;

  /**
   * Température **sans unité**, pour les colonnes de chiffres.
   *
   * Le « °C » répété vingt fois dans un tableau de cinq colonnes coûte à lui
   * seul une trentaine de pixels par colonne — sur un bloc de 40 % de largeur,
   * c'est ce qui faisait passer la cellule de nom sur trois lignes. L'unité est
   * rappelée une fois, sous le tableau.
   */
  const fmtDegres = (v, digits = 1) =>
    v === null || v === undefined ? "—" : v.toFixed(digits).replace(".", ",");

  // Formateurs créés une seule fois : `toLocaleString("fr-FR", …)` en construit
  // un nouveau à chaque appel, et le tableau en demande une dizaine par ligne.
  // Avec 1 153 points sélectionnés, c'était l'essentiel du temps de rendu.
  const FMT_NOMBRE = new Intl.NumberFormat("fr-FR");
  const FMT_UTC = new Intl.DateTimeFormat("fr-FR", {
    dateStyle: "short",
    timeStyle: "short",
    timeZone: "UTC",
  });
  const FMT_UTC_JOUR = new Intl.DateTimeFormat("fr-FR", { dateStyle: "short", timeZone: "UTC" });

  const fmtNombre = (v) => (v === null || v === undefined ? "—" : FMT_NOMBRE.format(v));

  /** Date calendaire nue (`YYYY-MM-DD`) telle que sérialisée par `NaiveDate`. */
  function fmtJour(iso) {
    if (!iso) return "—";
    const [a, m, j] = iso.split("-");
    return j ? `${j}/${m}/${a}` : iso;
  }

  /** Instant de mesure : UTC explicite, jamais converti en heure locale. */
  function fmtUtc(iso, avecHeure = true) {
    if (!iso) return "—";
    return (avecHeure ? FMT_UTC : FMT_UTC_JOUR).format(new Date(iso));
  }

  // ------------------------------------------------------------- couleurs

  /**
   * Attribution des couleurs, par ordre de sélection.
   *
   * Au-delà de huit points, la palette se répète — c'est exactement ce que
   * l'avertissement de lisibilité renvoyé par le backend annonce.
   */
  function paletteFor(ids) {
    const couleurs = new Map();
    ids.forEach((id, i) => couleurs.set(id, PALETTE[i % PALETTE.length]));
    return couleurs;
  }

  /**
   * Couleurs des lignes et des courbes du rendu en cours.
   *
   * Attribuées d'après l'ordre des points **renvoyés par le backend**, et non
   * d'après `selection`. Les deux coïncident (`crossing::build` parcourt
   * `point_ids` dans l'ordre reçu, et `main.js` pousse à la carte la palette de
   * cette même liste), mais s'appuyer sur la réponse rend le tableau, la légende
   * et le graphique cohérents *par construction* : un décalage ne pourrait plus
   * se manifester par des courbes toutes grises, échec silencieux et crédible.
   */
  function couleursDuRendu(analyse) {
    return paletteFor((analyse ? analyse.points : []).map((p) => p.point_id));
  }

  // --------------------------------------------------------------- cycle de vie

  function init(options) {
    invoke = options.invoke;
    onError = options.onError || (() => {});

    const selecteur = el("cross-window");
    const surChangement = () => {
      if (appliquerChoixFenetre(selecteur.value)) recharger(true);
      else attendreLesDeuxDates();
    };

    selecteur.addEventListener("change", surChangement);
    for (const champ of ["cross-from", "cross-to"]) {
      el(champ).addEventListener("change", surChangement);
    }
  }

  /**
   * Traduit le choix du `<select>` en charge utile `TimeWindow`.
   *
   * Renvoie `false` quand la fenêtre n'est pas exploitable — le seul cas est
   * « plage personnalisée » dont une des deux dates manque encore. On ne
   * recharge alors pas : envoyer la fenêtre précédente laisserait un résumé qui
   * contredit le sélecteur, et une plage à moitié définie n'a pas de sens.
   */
  function appliquerChoixFenetre(valeur) {
    el("cross-range").hidden = valeur !== "custom";

    if (valeur === "custom") {
      const from = el("cross-from").value;
      const to = el("cross-to").value;
      if (!from || !to) return false;
      fenetre = { kind: "custom", from, to };
      return true;
    }
    fenetre = { kind: valeur };
    return true;
  }

  /** Plage personnalisée incomplète : on le dit, sans rien recalculer. */
  function attendreLesDeuxDates() {
    const resume = el("cross-window-summary");
    resume.hidden = false;
    resume.removeAttribute("title");
    resume.textContent = "Saisissez les deux dates de la plage (UTC).";
  }

  /** Changement de projet : on oublie tout, graphique compris. */
  function reset() {
    clearTimeout(timer);
    timer = null;
    sequence++;
    selection = [];
    derniereAnalyse = null;
    detruireGraphique();
    rendre(null);
  }

  function destroy() {
    reset();
  }

  /** Nouvelle sélection venue de la carte : recharge, avec temporisation. */
  function setSelection(ids) {
    selection = [...ids];
    recharger(false);
  }

  /** Force un rechargement (changement de fenêtre, réimport de données). */
  function refresh() {
    recharger(true);
  }

  // ------------------------------------------------------------- chargement

  function recharger(immediat) {
    clearTimeout(timer);

    if (selection.length === 0) {
      sequence++; // annule une réponse encore en vol
      derniereAnalyse = null;
      detruireGraphique();
      rendre(null);
      return;
    }

    const lancer = () => charger();
    if (immediat) lancer();
    else timer = setTimeout(lancer, DEBOUNCE_MS);
  }

  async function charger() {
    if (!invoke) return;

    const appel = ++sequence;
    el("block-cross").classList.add("is-busy");

    try {
      const analyse = await invoke("cross_analysis", {
        point_ids: selection,
        window: fenetre,
      });
      // Une réponse doublée par une sélection plus récente est périmée :
      // l'afficher ferait clignoter le bloc sur des données déjà remplacées.
      if (appel !== sequence) return;

      derniereAnalyse = analyse;
      rendre(analyse);
    } catch (e) {
      if (appel !== sequence) return;
      derniereAnalyse = null;
      detruireGraphique();
      rendre(null, String(e));
      onError(String(e));
    } finally {
      if (appel === sequence) el("block-cross").classList.remove("is-busy");
    }
  }

  // ------------------------------------------------------------------ rendu

  function rendre(analyse, erreur) {
    rendreEnTete(analyse, erreur);
    rendreStats(analyse, erreur);
    rendreGraphique(analyse);
  }

  function rendreEnTete(analyse, erreur) {
    const badge = el("cross-badge");
    badge.hidden = selection.length === 0;
    badge.textContent = selection.length === 1 ? "1 point" : `${selection.length} points`;

    const barre = el("cross-window-bar");
    barre.hidden = selection.length === 0;

    const resume = el("cross-window-summary");
    if (erreur || !analyse || !analyse.window) {
      resume.textContent = "";
      resume.hidden = true;
    } else {
      const w = analyse.window;
      resume.hidden = false;
      // La fenêtre résolue est affichée en toutes lettres : les périodes
      // relatives s'ancrent sur la dernière mesure disponible, pas sur
      // l'horloge du poste (cf. l'en-tête de `crossing.rs`). Sans cette ligne,
      // « 7 derniers jours » serait un libellé trompeur.
      //
      // L'heure n'est montrée que sur les seaux horaires : sur une fenêtre de
      // trente jours ou de neuf ans, « 12:30 » n'apprend rien et coûte la
      // largeur qui manque à la ligne.
      const heure = w.granularity === "Hour";
      resume.textContent =
        `${fmtUtc(w.from_utc, heure)} → ${fmtUtc(w.to_utc, heure)} UTC · ${w.granularity_label}`;
      // Le temps de calcul reste consultable sans encombrer : c'est un chiffre
      // de diagnostic, le seul moyen de voir la réactivité se dégrader avant
      // qu'elle ne gêne.
      resume.title =
        `Fenêtre demandée : ${w.label}\n` +
        `Calculé côté Rust en ${analyse.elapsed_ms} ms`;
    }

    const alerte = el("cross-warning");
    const message = erreur || (analyse && analyse.readability_warning) || null;
    alerte.hidden = !message;
    alerte.textContent = message || "";
    alerte.classList.toggle("is-error", Boolean(erreur));
  }

  function rendreStats(analyse, erreur) {
    const zone = el("cross-stats");
    clear(zone);

    if (erreur) {
      zone.appendChild(h("p", { class: "placeholder", text: erreur }));
      return;
    }

    if (selection.length === 0) {
      zone.appendChild(
        h("p", {
          class: "placeholder",
          text:
            "Sélectionnez au moins deux points sur la carte pour comparer leurs " +
            "statistiques. Un seul point s'affiche aussi, sans comparaison.",
        })
      );
      return;
    }

    if (!analyse) {
      zone.appendChild(h("p", { class: "placeholder", text: "Calcul en cours…" }));
      return;
    }

    if (!analyse.window) {
      zone.appendChild(
        h("p", {
          class: "placeholder",
          text:
            selection.length === 1
              ? "Ce point ne porte encore aucune mesure. Importez un fichier de logger " +
                "depuis sa fiche."
              : "Aucun des points sélectionnés ne porte de mesure : il n'y a rien à croiser.",
        })
      );
      return;
    }

    const couleurs = couleursDuRendu(analyse);
    const annualisee = analyse.window.covers_a_full_year;

    // Les en-têtes sont abrégés, leur sens complet vit dans le `title` : à cette
    // largeur, « Moyenne des minimums quotidiens » en toutes lettres imposerait
    // sa largeur à la colonne et volerait la place de la colonne des noms.
    const table = h(
      "table",
      { class: "data-table cross-table" },
      h(
        "thead",
        {},
        h(
          "tr",
          {},
          h("th", { text: "Point" }),
          h("th", {
            class: "num",
            title: "Minimum extrême — la plus basse valeur relevée sur la fenêtre affichée",
            text: "Min ext.",
          }),
          h("th", {
            class: "num",
            title: annualisee
              ? "Moyenne annuelle des minimums quotidiens (la fenêtre couvre au moins un an)"
              : "Moyenne des minimums quotidiens — la fenêtre couvre moins d'un an, " +
                "ce n'est donc pas une moyenne annuelle",
            text: annualisee ? "Moy. min ᵃⁿ" : "Moy. min",
          }),
          h("th", {
            class: "num",
            title: "Amplitude quotidienne moyenne (max − min du jour, moyenné)",
            text: "Ampl. moy",
          }),
          h("th", {
            class: "num",
            title: "Plus forte amplitude quotidienne de la fenêtre",
            text: "Ampl. max",
          })
        )
      )
    );

    const corps = h("tbody", {});
    for (const p of analyse.points) {
      const vide = p.measurement_count === 0;
      corps.appendChild(
        h(
          "tr",
          {
            class: vide ? "is-empty-row" : null,
            // Le détail complet reste consultable sans encombrer la ligne.
            title: detailPoint(p),
          },
          h(
            "td",
            { class: "cross-name" },
            h("span", {
              class: "cross-swatch",
              style: `background:${couleurs.get(p.point_id)}`,
              "aria-hidden": true,
            }),
            h(
              "span",
              {},
              h("span", { class: "strong", text: p.name }),
              h("span", {
                class: "cross-sub",
                text: descriptionPoint(p, vide),
              })
            )
          ),
          h("td", { class: "num is-cold", text: fmtDegres(p.extreme_min_c) }),
          h("td", { class: "num", text: fmtDegres(p.mean_daily_min_c) }),
          h("td", { class: "num", text: fmtDegres(p.mean_amplitude_c) }),
          h("td", { class: "num", text: fmtDegres(p.max_amplitude_c) })
        )
      );
    }
    table.appendChild(corps);
    zone.appendChild(table);

    // L'unité, la portée des chiffres et le sens de « rec. », dits une fois.
    zone.appendChild(
      h("p", {
        class: "cross-note",
        text:
          "Températures en °C, calculées sur la fenêtre affichée. " +
          "« rec. » = minimum sur tout l'historique du point.",
      })
    );

    if (selection.length === 1) {
      zone.appendChild(
        h("p", {
          class: "cross-note",
          text: "Un seul point : sélectionnez-en un second sur la carte pour comparer.",
        })
      );
    }
    if (analyse.points_without_data > 0) {
      zone.appendChild(
        h("p", {
          class: "cross-note",
          text:
            analyse.points_without_data === 1
              ? "1 point sélectionné ne porte aucune mesure."
              : `${analyse.points_without_data} points sélectionnés ne portent aucune mesure.`,
        })
      );
    }
  }

  /**
   * Deuxième ligne d'une cellule de nom : catégorie, taille, et de quoi juger
   * ce que la ligne vaut — le record de froid *hors fenêtre* et le volume.
   *
   * Le record est là parce que le minimum extrême de la colonne porte, lui, sur
   * la seule fenêtre affichée. Sur « 30 derniers jours », ne montrer que ce
   * dernier ferait disparaître le record de l'hiver précédent sans le dire.
   */
  function descriptionPoint(p, vide) {
    const morceaux = [p.category + (p.size ? ` ${p.size}` : "")];
    if (vide) {
      morceaux.push("rien sur cette période");
    } else {
      morceaux.push(`${fmtNombre(p.measurement_count)} mes.`);
    }
    // Le record n'est montré que s'il **ajoute** quelque chose : quand la
    // fenêtre porte déjà le minimum de tout l'historique, le répéter ne ferait
    // que voler la largeur d'une ligne déjà serrée.
    if (
      p.record_min_c !== null &&
      p.record_min_c !== undefined &&
      (p.extreme_min_c === null ||
        p.extreme_min_c === undefined ||
        p.record_min_c < p.extreme_min_c - 0.05)
    ) {
      morceaux.push(`rec. ${fmtDegres(p.record_min_c)}`);
    }
    return morceaux.join(" · ");
  }

  /** Détail complet d'un point, en infobulle de ligne. */
  function detailPoint(p) {
    const lignes = [
      `${p.name} — ${p.category_label}${p.size ? ` (${p.size})` : ""}`,
      p.altitude_m !== null && p.altitude_m !== undefined
        ? `Altitude : ${fmtNombre(p.altitude_m)} m`
        : null,
      `Sur la fenêtre : ${fmtNombre(p.measurement_count)} mesures, ` +
        `${fmtNombre(p.day_count)} journées`,
      p.first_utc ? `${fmtUtc(p.first_utc)} → ${fmtUtc(p.last_utc)} UTC` : null,
      p.extreme_min_date
        ? `Minimum de la fenêtre atteint le ${fmtJour(p.extreme_min_date)}`
        : null,
      p.max_amplitude_date ? `Amplitude maximale le ${fmtJour(p.max_amplitude_date)}` : null,
      "",
      `Tout l'historique : ${fmtNombre(p.total_measurement_count)} mesures` +
        (p.history_first_utc
          ? `, du ${fmtUtc(p.history_first_utc, false)} au ${fmtUtc(p.history_last_utc, false)}`
          : ""),
      p.record_min_c !== null && p.record_min_c !== undefined
        ? `Record de froid : ${fmtTemp(p.record_min_c)}`
        : null,
    ];
    return lignes.filter((l) => l !== null).join("\n");
  }

  // -------------------------------------------------------------- graphique

  function detruireGraphique() {
    if (chart) {
      chart.destroy();
      chart = null;
    }
  }

  function messageGraphique(texte) {
    const zone = el("cross-chart");
    clear(zone);
    zone.classList.add("is-empty");
    zone.appendChild(h("p", { class: "placeholder", text: texte }));
  }

  /**
   * Insère un point `null` dans chaque trou de la série.
   *
   * Chart.js relie deux points consécutifs quelle que soit la distance qui les
   * sépare sur l'axe : sans cette rupture explicite, un logger arrêté six mois
   * afficherait un segment parfaitement droit d'un hiver à l'autre — une donnée
   * inventée, et la plus crédible qui soit. Le backend, lui, ne comble jamais un
   * seau vide (cf. `stats::aggregate_curve`).
   */
  function avecTrous(curve, granularity) {
    const largeur = LARGEUR_SEAU_MS[granularity] || 86400e3;
    const seuil = largeur * 1.5;
    const points = [];

    for (let i = 0; i < curve.length; i++) {
      const seau = curve[i];
      const x = Date.parse(seau.bucket_start_utc);
      if (i > 0) {
        const precedent = Date.parse(curve[i - 1].bucket_start_utc);
        if (x - precedent > seuil) points.push({ x: precedent + largeur, y: null });
      }
      points.push({
        x,
        y: seau.mean_c,
        min: seau.min_c,
        max: seau.max_c,
        n: seau.sample_count,
      });
    }
    return points;
  }

  function rendreGraphique(analyse) {
    if (selection.length === 0) {
      detruireGraphique();
      messageGraphique(
        "Le graphique superpose les courbes des points sélectionnés sur la même période."
      );
      return;
    }
    if (!analyse || !analyse.window) {
      detruireGraphique();
      messageGraphique(analyse ? "Aucune mesure à tracer." : "Calcul en cours…");
      return;
    }

    const couleurs = couleursDuRendu(analyse);
    // L'heure n'a de sens que sur des seaux horaires : au pas quotidien ou
    // mensuel, « 00:00 » dans chaque infobulle est du bruit qui a l'air d'une
    // précision.
    const seauHoraire = analyse.window.granularity === "Hour";
    const seuilTrou = (LARGEUR_SEAU_MS[analyse.window.granularity] || 86400e3) * 1.5;
    const datasets = analyse.points
      .filter((p) => p.curve.length > 0)
      .map((p) => ({
        label: p.name,
        data: avecTrous(p.curve, analyse.window.granularity),
        borderColor: couleurs.get(p.point_id),
        backgroundColor: couleurs.get(p.point_id),
        borderWidth: 1.6,
        // Aucun lissage : une courbe de température ne doit pas être embellie
        // par une interpolation qui inventerait des creux entre deux seaux.
        tension: 0,
        pointRadius: 0,
        pointHoverRadius: 3,
        // Rupture au-delà d'un seau et demi (voir `ruptures` plus bas), en plus
        // des points `null` d'`avecTrous`, que la réduction peut écarter.
        spanGaps: seuilTrou,
      }));

    if (datasets.length === 0) {
      detruireGraphique();
      messageGraphique("Aucune mesure sur cette période pour les points sélectionnés.");
      return;
    }

    const zone = el("cross-chart");
    // Le canvas est recréé à chaque rendu : Chart.js garde des écouteurs sur
    // celui qu'il a instrumenté, et le réutiliser après `destroy()` laisse des
    // infobulles fantômes de la sélection précédente.
    detruireGraphique();
    clear(zone);
    zone.classList.remove("is-empty");
    const canvas = h("canvas", { id: "cross-canvas" });
    zone.appendChild(canvas);

    // Réduction LTTB : au-delà de 4 seaux par pixel, les points retenus sont
    // espacés d'un à deux pixels, donc de plus d'un seau et demi. Pour ne pas
    // hachurer une courbe continue, le seuil de rupture passe alors à 3 pixels
    // de temps : un trou plus étroit ne se verrait de toute façon pas.
    // Recalculé à chaque mise à jour, puisque la largeur change au
    // redimensionnement. Les statistiques du tableau viennent du backend et
    // les infobulles portent le min/max du seau retenu : rien ne dépend des
    // points écartés.
    const ruptures = {
      id: "gwsRuptures",
      beforeUpdate(c) {
        const largeur = c.width || 1;
        const etendue = Date.parse(analyse.window.to_utc) - Date.parse(analyse.window.from_utc);
        for (const ds of c.data.datasets) {
          const n = (ds._data || ds.data || []).length;
          ds.spanGaps = n > 4 * largeur ? Math.max(seuilTrou, (3 * etendue) / largeur) : seuilTrou;
        }
      },
    };

    chart = new Chart(canvas.getContext("2d"), {
      type: "line",
      data: { datasets },
      plugins: [ruptures],
      options: {
        responsive: true,
        maintainAspectRatio: false,
        // Pas d'animation : jusqu'à huit courbes de quelques centaines de
        // seaux, rejouées à chaque clic sur la carte. L'animation ne se lit pas,
        // elle se subit.
        animation: false,
        normalized: true,
        parsing: false,
        interaction: { mode: "index", intersect: false },
        scales: {
          x: {
            type: "time",
            // Bornes imposées : deux points aux couvertures différentes doivent
            // se lire sur le **même** axe, sinon la superposition ment.
            min: Date.parse(analyse.window.from_utc),
            max: Date.parse(analyse.window.to_utc),
            time: { isoWeekday: 1 },
            grid: { color: "#eef1f5" },
            ticks: { maxRotation: 0, autoSkipPadding: 16, font: { size: 10 } },
          },
          y: {
            title: { display: true, text: "°C", font: { size: 10 } },
            grid: {
              // Le zéro est la ligne qui compte dans ce projet : au-dessus il ne
              // gèle pas, en dessous si.
              color: (ctx) => (ctx.tick.value === 0 ? "#b9c2cd" : "#eef1f5"),
              lineWidth: (ctx) => (ctx.tick.value === 0 ? 1.5 : 1),
            },
            ticks: { font: { size: 10 } },
          },
        },
        plugins: {
          // Données triées (BTreeMap côté Rust), `parsing: false`, axe temporel :
          // les trois conditions de la réduction sont remplies.
          decimation: { enabled: true, algorithm: "lttb" },
          // Pas de légende Chart.js : le tableau du dessus en est une, avec la
          // même pastille de couleur devant chaque nom, et il est toujours
          // visible (le bloc ne pagine pas). En doubler une ici coûtait de 30 à
          // 70 px selon le nombre de points — sur une zone de graphique qui en
          // fait 180, c'est la moitié du tracé échangée contre une redite.
          legend: { display: false },
          tooltip: {
            callbacks: {
              title: (items) =>
                items.length ? `${fmtUtc(items[0].parsed.x, seauHoraire)} UTC` : "",
              label: (item) => {
                const brut = item.raw;
                const moyenne = fmtTemp(brut.y);
                // Min et max du seau : sur une moyenne quotidienne ou mensuelle,
                // c'est le minimum qui caractérise un trou à froid, pas la moyenne.
                if (brut.min === undefined) return `${item.dataset.label} : ${moyenne}`;
                return (
                  `${item.dataset.label} : ${moyenne} ` +
                  `(min ${fmtTemp(brut.min)} · max ${fmtTemp(brut.max)} · ` +
                  `${fmtNombre(brut.n)} mes.)`
                );
              },
            },
          },
        },
      },
    });
  }

  /** Le graphique ne se remesure pas tout seul quand son bloc change de taille. */
  function resize() {
    if (chart) chart.resize();
  }

  /**
   * Dernière analyse rendue, telle qu'elle est arrivée du backend.
   *
   * Point d'entrée de l'export (CSV / TXT / HTML), chantier séparé : il n'aura
   * ni à relire la base ni à recalculer une statistique — le document est déjà
   * complet et autonome (cf. `crossing::CrossAnalysis`).
   */
  function lastAnalysis() {
    return derniereAnalyse;
  }

  /**
   * Fenêtre actuellement choisie dans le sélecteur du bloc, au format attendu
   * par `crossing::TimeWindow`.
   *
   * C'est la fenêtre que l'export (page 2 du bloc Stations) réutilise — voir
   * son en-tête : pas de second sélecteur de période, celui-ci fait référence
   * pour toute l'application.
   */
  function currentWindow() {
    return { ...fenetre };
  }

  return {
    init,
    setSelection,
    refresh,
    reset,
    destroy,
    resize,
    paletteFor,
    lastAnalysis,
    currentWindow,
    PALETTE,
  };
})();

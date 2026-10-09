/*
 * « Croisement en plein écran » : vue ouverte depuis le bouton du bloc
 * Croisement de données.
 *
 * - Panneau à gauche : stations, dates, profil et paramètres du croisement.
 * - Zone 1 (hauteur de la fenêtre moins la barre de 44 px) : graphique coloré,
 *   de janvier à décembre, chaque mois moyenné sur les années de la période :
 *   moyenne des maximales (trait plein), moyenne du mois (pointillés fins),
 *   moyenne des minimales (tirets). Une couleur par station.
 * - Zone 2 (24 px plus bas, aussi longue que nécessaire) : tableau comparatif.
 *   Les paramètres en lignes, le temps en colonnes (janv. → déc., puis toute
 *   la période), avec un filtre pour choisir les paramètres affichés.
 *
 * Aucun calcul statistique ici : tout arrive de `cross_monthly`
 * (`croisement_mensuel.rs`), qui lit les agrégats journaliers.
 */

const GwsCrossPlein = (() => {
  "use strict";

  /**
   * Profils. « Par défaut » est celui demandé pour suivre le réchauffement :
   * du 1er janvier 2020 au 30 septembre 2026, jours ≥ 25 °C et ≤ 0 °C.
   */
  const PROFILS = {
    defaut: {
      label: "Par défaut (évolution du climat)",
      du: "2020-01-01",
      au: "2026-09-30",
      chaud: 25,
      gel: 0,
    },
  };

  /**
   * Lignes du tableau, dans l'ordre, et leur filtre. `champ` : valeur d'un
   * mois moyenné ou de la période (mêmes noms côté Rust).
   */
  const PARAMETRES = [
    { id: "moy", label: "Moyenne du mois", champ: "moy_c", titre: "Moyenne de toutes les mesures" },
    { id: "moy_min", label: "Moy. mini", champ: "moy_min_c", titre: "Moyenne des températures minimales quotidiennes" },
    { id: "moy_max", label: "Moy. maxi", champ: "moy_max_c", titre: "Moyenne des températures maximales quotidiennes" },
    { id: "ext_min", label: "Extrême mini", champ: "ext_min_c", date: "ext_min_date", classe: "is-cold", titre: "Température la plus basse relevée (survolez pour la date)" },
    { id: "ext_max", label: "Extrême maxi", champ: "ext_max_c", date: "ext_max_date", classe: "is-hot", titre: "Température la plus haute relevée (survolez pour la date)" },
    { id: "chauds", label: "Jours ≥ chaud", champ: "jours_chauds_par_mois", jours: true, titre: "Nombre moyen de jours par mois dont la maximale atteint le seuil chaud" },
    { id: "gel", label: "Jours ≤ gel", champ: "jours_gel_par_mois", jours: true, titre: "Nombre moyen de jours par mois dont la minimale descend au seuil de gel" },
  ];

  const MOIS_COURTS = [
    "janv.", "févr.", "mars", "avr.", "mai", "juin",
    "juil.", "août", "sept.", "oct.", "nov.", "déc.",
  ];
  const MOIS = [
    "janvier", "février", "mars", "avril", "mai", "juin",
    "juillet", "août", "septembre", "octobre", "novembre", "décembre",
  ];

  let invoke = null;
  let onError = () => {};
  let lirePoints = () => [];
  let lireSelectionCarte = () => [];

  let ouvert = false;
  /** Stations cochées, dans l'ordre où elles l'ont été (= ordre des couleurs). */
  let choisies = [];
  let reglages = { profil: "defaut", ...PROFILS.defaut };
  let parametresVisibles = new Set(PARAMETRES.map((p) => p.id));
  let bilan = null;
  let chart = null;
  let sequence = 0;
  let timer = null;

  const el = (id) => document.getElementById(id);

  function h(tag, attrs, ...enfants) {
    const n = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k === "class") n.className = v;
      else if (k === "text") n.textContent = v;
      else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
      else if (v === true) n.setAttribute(k, "");
      else n.setAttribute(k, v);
    }
    for (const c of enfants.flat()) {
      if (c === null || c === undefined || c === false) continue;
      n.appendChild(typeof c === "string" ? document.createTextNode(c) : c);
    }
    return n;
  }

  const vider = (n) => {
    while (n.firstChild) n.removeChild(n.firstChild);
  };

  const fmtDeg = (v) => (v === null || v === undefined ? "—" : v.toFixed(1).replace(".", ","));
  function fmtJour(iso) {
    if (!iso) return "";
    const [a, m, j] = iso.split("-");
    return `${j}/${m}/${a}`;
  }

  function couleurs() {
    return GwsCross.paletteFor(choisies);
  }

  // ------------------------------------------------------------ cycle de vie

  function init(options) {
    invoke = options.invoke;
    onError = options.onError || onError;
    lirePoints = options.points || lirePoints;
    lireSelectionCarte = options.selection || lireSelectionCarte;

    el("btn-cross-plein").addEventListener("click", ouvrir);
    el("cp-fermer").addEventListener("click", fermer);
    document.addEventListener("keydown", (e) => {
      if (ouvert && e.key === "Escape" && !document.querySelector("dialog[open]")) fermer();
    });

    el("cp-filtre-stations").addEventListener("input", rendreListeStations);
    el("cp-tout-decocher").addEventListener("click", () => {
      choisies = [];
      rendreListeStations();
      recharger();
    });
    el("cp-reprendre-carte").addEventListener("click", () => {
      choisies = lireSelectionCarte().filter((id) => lirePoints().some((p) => p.id === id));
      rendreListeStations();
      recharger();
    });

    for (const id of ["cp-du", "cp-au"]) {
      el(id).addEventListener("change", () => {
        marquerPersonnalise();
        recharger();
      });
    }
    el("cp-profil").addEventListener("change", () => appliquerProfil(el("cp-profil").value));
    el("cp-btn-parametres").addEventListener("click", () => {
      const zone = el("cp-parametres");
      zone.hidden = !zone.hidden;
      el("cp-btn-parametres").setAttribute("aria-expanded", String(!zone.hidden));
    });
    for (const id of ["cp-seuil-chaud", "cp-seuil-gel"]) {
      el(id).addEventListener("change", () => {
        marquerPersonnalise();
        recharger();
      });
    }

    const filtres = el("cp-filtres-colonnes");
    for (const p of PARAMETRES) {
      filtres.appendChild(
        h(
          "label",
          { class: "cp-filtre", title: p.titre },
          h("input", {
            type: "checkbox",
            checked: true,
            onchange: (e) => {
              if (e.target.checked) parametresVisibles.add(p.id);
              else parametresVisibles.delete(p.id);
              rendreTableau();
            },
          }),
          p.label
        )
      );
    }

    const selProfil = el("cp-profil");
    for (const [cle, p] of Object.entries(PROFILS)) {
      selProfil.appendChild(h("option", { value: cle, text: p.label }));
    }
    selProfil.appendChild(h("option", { value: "perso", text: "Personnalisé" }));
  }

  function ouvrir() {
    if (ouvert) return;
    ouvert = true;
    // Première ouverture, ou stations disparues (autre projet) : on repart de
    // la sélection de la carte.
    const ids = new Set(lirePoints().map((p) => p.id));
    choisies = choisies.filter((id) => ids.has(id));
    if (choisies.length === 0) choisies = lireSelectionCarte().filter((id) => ids.has(id));
    ecrireReglages();
    el("cross-plein").hidden = false;
    document.body.classList.add("cp-ouvert");
    el("cp-fermer").focus();
    rendreListeStations();
    recharger(true);
  }

  function fermer() {
    if (!ouvert) return;
    ouvert = false;
    clearTimeout(timer);
    sequence++;
    detruireGraphique();
    el("cross-plein").hidden = true;
    document.body.classList.remove("cp-ouvert");
    el("btn-cross-plein").focus();
  }

  /** Changement de projet : la vue se ferme et oublie ses stations. */
  function reset() {
    fermer();
    choisies = [];
    bilan = null;
  }

  function resize() {
    if (chart) chart.resize();
  }

  // ---------------------------------------------------------------- réglages

  function ecrireReglages() {
    el("cp-profil").value = reglages.profil;
    el("cp-du").value = reglages.du;
    el("cp-au").value = reglages.au;
    el("cp-seuil-chaud").value = String(reglages.chaud);
    el("cp-seuil-gel").value = String(reglages.gel);
  }

  function lireReglages() {
    const nombre = (id, defaut) => {
      const v = Number.parseFloat(String(el(id).value).replace(",", "."));
      return Number.isFinite(v) ? v : defaut;
    };
    reglages = {
      profil: el("cp-profil").value,
      du: el("cp-du").value,
      au: el("cp-au").value,
      chaud: nombre("cp-seuil-chaud", 25),
      gel: nombre("cp-seuil-gel", 0),
    };
  }

  function appliquerProfil(cle) {
    if (PROFILS[cle]) {
      reglages = { profil: cle, ...PROFILS[cle] };
      ecrireReglages();
      recharger(true);
    }
  }

  function marquerPersonnalise() {
    el("cp-profil").value = "perso";
  }

  // ------------------------------------------------------------- stations

  function rendreListeStations() {
    const liste = el("cp-stations");
    vider(liste);
    const filtre = el("cp-filtre-stations").value.trim().toLowerCase();
    const cols = couleurs();
    const points = [...lirePoints()].sort((a, b) => a.name.localeCompare(b.name, "fr"));
    let affichees = 0;
    for (const p of points) {
      if (filtre && !p.name.toLowerCase().includes(filtre)) continue;
      affichees++;
      const coche = choisies.includes(p.id);
      liste.appendChild(
        h(
          "li",
          {},
          h(
            "label",
            { class: "cp-station" },
            h("input", {
              type: "checkbox",
              checked: coche,
              onchange: (e) => {
                if (e.target.checked) choisies.push(p.id);
                else choisies = choisies.filter((id) => id !== p.id);
                rendreListeStations();
                recharger();
              },
            }),
            h("span", {
              class: "cross-swatch",
              style: coche ? `background:${cols.get(p.id)}` : "background:transparent",
              "aria-hidden": true,
            }),
            h("span", { class: "cp-station-nom", text: p.name }),
            p.altitude_m !== null && p.altitude_m !== undefined
              ? h("span", { class: "cp-station-alt", text: `${Math.round(p.altitude_m)} m` })
              : null
          )
        )
      );
    }
    if (affichees === 0) {
      liste.appendChild(h("li", { class: "placeholder", text: "Aucune station." }));
    }
    el("cp-compte").textContent =
      choisies.length === 0
        ? "Aucune station cochée"
        : choisies.length === 1
          ? "1 station cochée"
          : `${choisies.length} stations cochées`;
  }

  // ------------------------------------------------------------- chargement

  function recharger(immediat) {
    clearTimeout(timer);
    if (immediat) charger();
    else timer = setTimeout(charger, 250);
  }

  async function charger() {
    lireReglages();
    const appel = ++sequence;
    if (choisies.length === 0) {
      bilan = null;
      rendre();
      return;
    }
    if (!reglages.du || !reglages.au) {
      bilan = null;
      rendre("Saisissez les deux dates.");
      return;
    }
    el("cross-plein").classList.add("is-busy");
    try {
      const r = await invoke("cross_monthly", {
        point_ids: choisies,
        du: reglages.du,
        au: reglages.au,
        seuils: { chaud_c: reglages.chaud, gel_c: reglages.gel },
      });
      if (appel !== sequence) return;
      bilan = r;
      rendre();
    } catch (e) {
      if (appel !== sequence) return;
      bilan = null;
      rendre(String(e));
      onError(String(e));
    } finally {
      if (appel === sequence) el("cross-plein").classList.remove("is-busy");
    }
  }

  // ------------------------------------------------------------------ rendu

  let derniereErreur = null;

  function rendre(erreur) {
    derniereErreur = erreur || null;
    const resume = el("cp-resume");
    if (bilan) {
      resume.textContent =
        `${fmtJour(bilan.du)} → ${fmtJour(bilan.au)} · chaque mois moyenné sur les années · ` +
        `jour chaud : maxi ≥ ${fmtDeg(bilan.seuils.chaud_c)} °C · jour de gel : mini ≤ ${fmtDeg(bilan.seuils.gel_c)} °C`;
      resume.title = `Calculé en ${bilan.elapsed_ms} ms`;
    } else {
      resume.textContent = "";
    }
    rendreGraphique(erreur);
    rendreTableau();
  }

  function detruireGraphique() {
    if (chart) {
      chart.destroy();
      chart = null;
    }
  }

  function messageGraphique(texte) {
    detruireGraphique();
    const zone = el("cp-graphique");
    vider(zone);
    zone.appendChild(h("p", { class: "placeholder", text: texte }));
  }

  function rendreGraphique(erreur) {
    if (erreur) return messageGraphique(erreur);
    if (choisies.length === 0) {
      return messageGraphique("Cochez au moins une station dans le panneau de gauche.");
    }
    if (!bilan) return messageGraphique("Calcul en cours…");
    if (bilan.stations.every((s) => s.mois.length === 0)) {
      return messageGraphique("Aucune mesure sur cette période pour les stations cochées.");
    }

    const cols = GwsCross.paletteFor(bilan.stations.map((s) => s.point_id));
    const datasets = [];
    const courbes = [
      { champ: "moy_max_c", nom: "moy. maxi", largeur: 2.2, tirets: [] },
      { champ: "moy_c", nom: "moyenne", largeur: 1.4, tirets: [2, 3] },
      { champ: "moy_min_c", nom: "moy. mini", largeur: 1.8, tirets: [7, 4] },
    ];
    for (const s of bilan.stations) {
      const couleur = cols.get(s.point_id);
      for (const c of courbes) {
        datasets.push({
          label: `${s.name} · ${c.nom}`,
          // Un mois sans aucune mesure donne `null` : la courbe se coupe.
          data: s.normales.map((n) => (n[c.champ] === null ? null : n[c.champ])),
          normales: s.normales,
          borderColor: couleur,
          backgroundColor: couleur,
          borderWidth: c.largeur,
          borderDash: c.tirets,
          tension: 0,
          pointRadius: 3,
          spanGaps: false,
        });
      }
    }

    detruireGraphique();
    const zone = el("cp-graphique");
    vider(zone);
    const canvas = h("canvas", { id: "cp-canvas" });
    zone.appendChild(canvas);

    chart = new Chart(canvas.getContext("2d"), {
      type: "line",
      data: { labels: MOIS_COURTS, datasets },
      options: {
        responsive: true,
        maintainAspectRatio: false,
        animation: false,
        interaction: { mode: "nearest", axis: "x", intersect: false },
        scales: {
          x: { grid: { color: "#eef1f5" } },
          y: {
            title: { display: true, text: "°C" },
            grid: {
              color: (ctx) => (ctx.tick.value === 0 ? "#b9c2cd" : "#eef1f5"),
              lineWidth: (ctx) => (ctx.tick.value === 0 ? 1.5 : 1),
            },
          },
        },
        plugins: {
          legend: { position: "top", labels: { boxWidth: 18 } },
          tooltip: {
            callbacks: {
              title: (items) => (items.length ? MOIS[items[0].dataIndex] : ""),
              label: (item) => {
                const n = item.dataset.normales[item.dataIndex];
                const annees = n.annees === 1 ? "1 année" : `${n.annees} années`;
                return `${item.dataset.label} : ${fmtDeg(item.raw)} °C (${annees})`;
              },
            },
          },
        },
      },
    });
  }

  /** Cellule d'un paramètre pour un mois moyenné ou pour la période. */
  function cellule(p, v, annees) {
    const valeur = v[p.champ];
    const texte = p.jours
      ? valeur === null || valeur === undefined
        ? "—"
        : valeur.toFixed(1).replace(".", ",")
      : fmtDeg(valeur);
    let titre = null;
    if (p.date && v[p.date]) titre = `Le ${fmtJour(v[p.date])}`;
    else if (p.jours) titre = `Moyenne sur ${v.mois_complets} mois complet${v.mois_complets > 1 ? "s" : ""}`;
    else if (annees !== null) titre = annees === 1 ? "Sur 1 année" : `Sur ${annees} années`;
    return h("td", { class: ["num", p.classe || ""].join(" ").trim(), title: titre, text: texte });
  }

  function rendreTableau() {
    const zone = el("cp-tableaux");
    vider(zone);
    if (derniereErreur || !bilan || choisies.length === 0) {
      zone.appendChild(
        h("p", {
          class: "placeholder",
          text: derniereErreur || (choisies.length === 0 ? "Aucune station cochée." : "Calcul en cours…"),
        })
      );
      return;
    }
    const visibles = PARAMETRES.filter((p) => parametresVisibles.has(p.id));
    if (visibles.length === 0) {
      zone.appendChild(h("p", { class: "placeholder", text: "Cochez au moins un paramètre." }));
      return;
    }
    const cols = GwsCross.paletteFor(bilan.stations.map((s) => s.point_id));
    const pourcent = Math.round(bilan.couverture_mois_complet * 100);

    const table = h(
      "table",
      { class: "data-table cross-table cp-table" },
      h(
        "thead",
        {},
        h(
          "tr",
          {},
          h("th", { text: "Station" }),
          h("th", { text: "Paramètre" }),
          MOIS_COURTS.map((m, i) => h("th", { class: "num", title: `${MOIS[i]}, moyenné sur les années`, text: m })),
          h("th", { class: "num cp-col-periode", title: "Toute la période choisie", text: "Période" })
        )
      )
    );
    const corps = h("tbody", {});
    for (const s of bilan.stations) {
      visibles.forEach((p, i) => {
        corps.appendChild(
          h(
            "tr",
            { class: i === 0 ? "cp-debut-station" : null },
            i === 0
              ? h(
                  "td",
                  {
                    class: "cp-station-cellule",
                    rowspan: String(visibles.length),
                    title:
                      `${s.periode.mois_observes} mois observés, dont ${s.periode.mois_complets} complets` +
                      (s.altitude_m !== null && s.altitude_m !== undefined
                        ? `\nAltitude : ${Math.round(s.altitude_m)} m`
                        : ""),
                  },
                  h(
                    "span",
                    { class: "cross-name" },
                    h("span", {
                      class: "cross-swatch",
                      style: `background:${cols.get(s.point_id)}`,
                      "aria-hidden": true,
                    }),
                    h("span", { class: "strong", text: s.name })
                  )
                )
              : null,
            h("td", { class: "cp-parametre", title: p.titre, text: p.label }),
            s.normales.map((n) => cellule(p, n, n.annees)),
            (() => {
              const td = cellule(p, s.periode, null);
              td.classList.add("cp-col-periode");
              return td;
            })()
          )
        );
      });
    }
    table.appendChild(corps);
    zone.appendChild(h("div", { class: "cp-table-defile" }, table));
    zone.appendChild(
      h("p", {
        class: "cross-note",
        text:
          `Températures en °C. Chaque colonne de mois est la moyenne de ce mois sur toutes les années ` +
          `de la période. « Jours ≥ chaud » et « Jours ≤ gel » : nombre moyen de jours par mois, sur les ` +
          `seuls mois observés à ${pourcent} % ou plus. Survolez une valeur pour voir sur combien ` +
          `d'années elle porte, ou la date d'un extrême.`,
      })
    );
  }

  return { init, ouvrir, fermer, reset, resize };
})();

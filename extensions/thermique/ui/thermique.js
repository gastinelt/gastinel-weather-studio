/*
 * Analyse thermique — panneau de l'extension (JS pur, aucune bibliothèque).
 *
 * Un seul panneau à trois onglets plutôt que trois extensions : les trois
 * analyses partagent la même liste de points, la même période et la même
 * DLL ; l'hôte n'ouvre qu'une fenêtre par bouton `map_view`.
 *
 * Tous les calculs sont faits en Rust (méthodes `points`, `gradient`,
 * `episodes`, `courbe`, `profil`) ; ce fichier ne fait qu'afficher.
 * Les heures sont affichées en heure locale du poste.
 */
(() => {
  "use strict";

  // Couleurs des graphiques : lues dans les variables CSS de la page (thème clair).
  const C = (nom) => getComputedStyle(document.documentElement).getPropertyValue(nom).trim();

  // --- Pont avec l'application ---------------------------------------------
  const attentes = new Map();
  let prochain = 1;
  function appeler(method, payload) {
    return new Promise((ok, ko) => {
      const rid = prochain++;
      attentes.set(rid, { ok, ko });
      parent.postMessage({ gws: "call", rid, method, payload: Object.assign({ project_id: etat.projet }, payload) }, "*");
    });
  }
  window.addEventListener("message", (ev) => {
    if (ev.source !== parent) return;
    const m = ev.data || {};
    if (m.gws === "reply" && attentes.has(m.rid)) {
      const a = attentes.get(m.rid);
      attentes.delete(m.rid);
      m.err ? a.ko(new Error(m.err)) : a.ok(m.ok);
    } else if (m.gws === "context") {
      etat.projet = m.project_id;
      charger().catch((e) => { $("liste-points").innerHTML = ""; $("liste-points").append(erreur(e)); });
    }
  });

  const $ = (id) => document.getElementById(id);
  const etat = { projet: null, points: [], onglet: "gradient", sel: new Set(), profil: null, tri: { cle: "coucher", asc: true }, episodes: [] };
  const H = 3600, J = 86400;

  // --- Formats -----------------------------------------------------------------
  const nf = (v, d = 1) => (v == null || !isFinite(v) ? "—" : v.toLocaleString("fr-FR", { minimumFractionDigits: d, maximumFractionDigits: d }));
  const pc = (v) => nf(v * 100, 0) + " %";
  const dt = (t) => new Date(t * 1000);
  const fDate = (t) => dt(t).toLocaleDateString("fr-FR", { day: "2-digit", month: "2-digit", year: "numeric" });
  const fHeure = (t) => dt(t).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" });
  const fDateHeure = (t) => fDate(t) + " " + fHeure(t);
  const versInputDate = (t) => { const d = dt(t); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`; };
  const depuisInputDate = (s) => { const [y, m, d] = s.split("-").map(Number); return new Date(y, m - 1, d).getTime() / 1000; };
  const versInputDH = (t) => versInputDate(t) + "T" + String(dt(t).getHours()).padStart(2, "0") + ":00";
  const depuisInputDH = (s) => new Date(s).getTime() / 1000;
  function erreur(e) { const p = document.createElement("div"); p.className = "erreur"; p.textContent = e.message || String(e); return p; }
  function el(tag, props, ...enfants) { const n = document.createElement(tag); Object.assign(n, props || {}); n.append(...enfants); return n; }

  // --- Canvas : repère et graduations ------------------------------------------
  function preparer(cv) {
    const dpr = window.devicePixelRatio || 1;
    if (!cv.dataset.h) cv.dataset.h = cv.getAttribute("height");
    const w = cv.clientWidth, h = +cv.dataset.h;
    cv.style.height = h + "px";
    cv.width = Math.round(w * dpr); cv.height = Math.round(h * dpr);
    const ctx = cv.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    ctx.font = "11px system-ui, Segoe UI, sans-serif";
    return { ctx, w, h };
  }
  function pasJoli(etendue, n) {
    const brut = etendue / n, p = Math.pow(10, Math.floor(Math.log10(brut))), r = brut / p;
    return (r < 1.5 ? 1 : r < 3 ? 2 : r < 7 ? 5 : 10) * p;
  }
  /** Repère : x et y numériques, `fx`/`fy` formatent les graduations. */
  function repere(ctx, w, h, xr, yr, o) {
    const m = { g: 52, d: 14, h: 12, b: 26 };
    const X = (v) => m.g + (v - xr[0]) / (xr[1] - xr[0] || 1) * (w - m.g - m.d);
    const Y = (v) => h - m.b - (v - yr[0]) / (yr[1] - yr[0] || 1) * (h - m.h - m.b);
    ctx.strokeStyle = C("--bord"); ctx.fillStyle = C("--doux"); ctx.lineWidth = 1;
    const py = pasJoli(yr[1] - yr[0], 6);
    ctx.textAlign = "right"; ctx.textBaseline = "middle";
    for (let v = Math.ceil(yr[0] / py) * py; v <= yr[1] + 1e-9; v += py) {
      ctx.beginPath(); ctx.moveTo(m.g, Y(v)); ctx.lineTo(w - m.d, Y(v)); ctx.stroke();
      ctx.fillText(o.fy(v), m.g - 6, Y(v));
    }
    ctx.textAlign = "center"; ctx.textBaseline = "top";
    for (const v of o.tx || graduationsX(xr, o, w)) ctx.fillText(o.fx(v), X(v), h - m.b + 6);
    if (o.titreY) { ctx.save(); ctx.translate(11, (h - m.b + m.h) / 2); ctx.rotate(-Math.PI / 2); ctx.fillText(o.titreY, 0, -6); ctx.restore(); }
    return { X, Y, m };
  }
  function graduationsX(xr, o, w) {
    const n = Math.max(2, Math.floor(w / 110));
    if (!o.temps) { const p = pasJoli(xr[1] - xr[0], n), r = []; for (let v = Math.ceil(xr[0] / p) * p; v <= xr[1]; v += p) r.push(v); return r; }
    const e = xr[1] - xr[0];
    const pas = [H, 3 * H, 6 * H, 12 * H, J, 2 * J, 7 * J, 14 * J, 30 * J, 61 * J, 91 * J].find((p) => e / p <= n) || 182 * J;
    const r = [];
    // Aligne sur minuit local pour les pas ≥ 1 jour.
    let t = pas >= J ? new Date(dt(xr[0]).setHours(0, 0, 0, 0)).getTime() / 1000 + J : Math.ceil(xr[0] / pas) * pas;
    for (; t <= xr[1]; t += pas) r.push(t);
    return r;
  }
  const bornes = (vals, marge = 0.05) => {
    let a = Math.min(...vals), b = Math.max(...vals);
    if (!isFinite(a)) return [0, 1];
    if (a === b) { a -= 1; b += 1; }
    const d = (b - a) * marge; return [a - d, b + d];
  };

  // --- Chargement des points -----------------------------------------------------
  async function charger() {
    if (!etat.projet) { $("liste-points").textContent = "Aucun projet actif."; return; }
    const r = await appeler("points", {});
    etat.points = r.points.filter((p) => p.altitude_m != null);
    const liste = $("liste-points");
    liste.innerHTML = "";
    $("station").innerHTML = "";
    etat.sel.clear();
    if (!r.points.length) { liste.textContent = "Aucun point de ce projet n'a de mesures."; return; }
    for (const p of r.points) {
      const s = serieParDefaut(p);
      const cb = el("input", { type: "checkbox", value: p.id, disabled: p.altitude_m == null });
      cb.addEventListener("change", () => { cb.checked ? etat.sel.add(p.id) : etat.sel.delete(p.id); periodeParDefaut(); });
      const info = el("small", {}, `${p.altitude_m == null ? "altitude ?" : nf(p.altitude_m, 0) + " m"} · ${fDate(s.t_min)} → ${fDate(s.t_max)}`);
      const lab = el("label", { className: "pt" }, cb, el("span", {}, p.name, info));
      if (p.series.some((x) => x.minimum_horaire)) info.append(el("span", { className: "min", title: "Résumé horaire : la température est le MINIMUM de l'heure" }, " · min. horaire"));
      liste.append(lab);
      $("station").append(el("option", { value: p.id }, `${p.name} (${p.altitude_m == null ? "?" : nf(p.altitude_m, 0)} m)`));
    }
    periodeParDefaut();
  }
  /** Même règle que le Rust (Primary d'abord), sans la plage : pour l'affichage. */
  function serieParDefaut(p) {
    const prim = p.series.filter((s) => s.role === "Primary");
    return (prim.length ? prim : p.series).reduce((a, b) => (b.t_max - b.t_min > a.t_max - a.t_min ? b : a));
  }
  function etendue(ids) {
    const pts = etat.points.filter((p) => ids.includes(p.id)).map(serieParDefaut);
    if (!pts.length) return null;
    return { debut: Math.max(...pts.map((s) => s.t_min)), fin: Math.min(...pts.map((s) => s.t_max)) };
  }
  function periodeParDefaut() {
    const ids = etat.onglet === "episodes" ? [$("station").value] : [...etat.sel];
    const e = etendue(ids);
    if (!e) return;
    const jours = etat.onglet === "episodes" ? 120 : 90;
    const fin = e.fin > e.debut ? e.fin : Math.max(...etat.points.filter((p) => ids.includes(p.id)).map((p) => serieParDefaut(p).t_max));
    $("fin").value = versInputDate(fin);
    $("debut").value = versInputDate(Math.max(fin - jours * J, e.fin > e.debut ? e.debut : fin - jours * J));
    const g = $("g-statut");
    if (etat.onglet !== "episodes" && ids.length >= 2 && e.fin <= e.debut) g.textContent = "Attention : ces séries ne se recouvrent pas dans le temps.";
  }
  function periode() {
    const debut = depuisInputDate($("debut").value), fin = depuisInputDate($("fin").value) + J;
    if (!isFinite(debut) || !isFinite(fin)) throw new Error("période invalide");
    return { debut, fin };
  }

  // --- Onglets -------------------------------------------------------------------
  const AIDES = {
    gradient: "ΔT/Δz par régression sur les points cochés, aux seuls instants où tous ont une mesure (grille commune).",
    episodes: "Nuits claires et calmes détectées d'après la forme de la courbe de température d'une seule station.",
    profil: "Température en fonction de l'altitude à un instant ; le curseur parcourt la période commune.",
  };
  for (const b of document.querySelectorAll("nav button")) b.addEventListener("click", () => ouvrir(b.dataset.onglet));
  function ouvrir(o) {
    etat.onglet = o;
    for (const b of document.querySelectorAll("nav button")) b.setAttribute("aria-selected", b.dataset.onglet === o);
    for (const s of document.querySelectorAll("section.onglet")) s.hidden = s.id !== "onglet-" + o;
    $("bloc-points").hidden = o === "episodes";
    $("bloc-station").hidden = o !== "episodes";
    $("ligne-pas").hidden = o !== "gradient";
    $("bloc-periode").hidden = false;
    $("aide-onglet").textContent = AIDES[o];
    periodeParDefaut();
  }
  $("station").addEventListener("change", periodeParDefaut);
  $("calculer").addEventListener("click", () => {
    const f = { gradient: calculerGradient, episodes: calculerEpisodes, profil: calculerProfil }[etat.onglet];
    f().catch((e) => { const s = $(etat.onglet[0] + "-statut"); s.innerHTML = ""; s.append(erreur(e)); });
  });
  window.addEventListener("resize", () => { dessinerGradient(); dessinerEpisodes(); dessinerNuit(); dessinerProfil(); });

  // --- 1. Gradient -----------------------------------------------------------------
  async function calculerGradient() {
    if (etat.sel.size < 2) throw new Error("Cochez au moins deux points.");
    $("g-statut").textContent = "Calcul…";
    const r = await appeler("gradient", Object.assign(periode(), { points: [...etat.sel], pas: +$("pas").value }));
    etat.gradient = r;
    const s = r.stats;
    $("g-statut").innerHTML = "";
    if (!s.n) { $("g-statut").textContent = "Aucun instant commun : les séries ne se recouvrent pas sur cette période."; $("g-stats").innerHTML = ""; dessinerGradient(); return; }
    $("g-statut").textContent = `Recouvrement utilisé : ${fDateHeure(s.debut)} → ${fDateHeure(s.fin)} · ${s.n} instants communs (pas ${r.pas_s / 60} min, tolérance ±${r.tolerance_s / 60} min) · Δz = ${nf(r.dz_m, 0)} m.`;
    const tuiles = [
      [pc(s.part_inversion), "du temps en inversion"],
      [pc(s.part_inversion_nuit), `en inversion la nuit (${s.n_nuit} h)`],
      [pc(s.part_inversion_jour), `en inversion le jour (${s.n_jour} h)`],
      [nf(s.moyenne, 2), "gradient moyen °C/100 m"],
      [nf(s.moyenne_nuit, 2), "moyen de nuit °C/100 m"],
      [nf(s.moyenne_jour, 2), "moyen de jour °C/100 m"],
      [s.max_inversion == null ? "—" : "+" + nf(s.max_inversion, 2), s.t_max_inversion ? "inversion max, " + fDateHeure(s.t_max_inversion) : "inversion max"],
      [nf(s.min_gradient, 2), s.t_min_gradient ? "gradient le plus fort, " + fDateHeure(s.t_min_gradient) : ""],
    ];
    $("g-stats").replaceChildren(...tuiles.map(([v, l]) => el("div", { className: "stat" }, el("b", {}, v), el("span", {}, l))));
    const infos = $("g-infos");
    infos.replaceChildren(el("div", {}, "Séries utilisées : " + r.points.map((p) => `${p.name} (${nf(p.altitude_m, 0)} m, ${p.role}${p.label ? ", " + p.label : ""})`).join(" · ")),
      el("div", {}, "Jour / nuit : hauteur du soleil au centre des points (> −0,833°). Gradient > 0 : la température augmente avec l'altitude (inversion)." + (r.agrege ? " Série longue : chaque trait est la moyenne d'une tranche, la bande grise son min–max." : "")),
      ...r.avertissements.map((a) => el("div", { className: "avert" }, "⚠ " + a)));
    dessinerGradient();
  }
  function dessinerGradient() {
    const r = etat.gradient, cv = $("g-canvas");
    if (!r || $("onglet-gradient").hidden) return;
    const { ctx, w, h } = preparer(cv);
    const v = r.serie;
    if (!v.length) return;
    const ys = v.flatMap((x) => [x.min, x.max]).concat([0, -0.65]);
    const xr = [v[0].t, v[v.length - 1].t];
    const { X, Y } = repere(ctx, w, h, xr, bornes(ys), { temps: true, fx: xr[1] - xr[0] > 3 * J ? fDate : fHeure, fy: (y) => nf(y, 1), titreY: "°C / 100 m" });
    if (r.agrege) {
      ctx.fillStyle = C("--zone");
      for (const x of v) ctx.fillRect(X(x.t) - 0.5, Y(x.max), 1.5, Math.max(1, Y(x.min) - Y(x.max)));
    }
    ctx.setLineDash([4, 4]); ctx.strokeStyle = C("--repere");
    ctx.beginPath(); ctx.moveTo(X(xr[0]), Y(-0.65)); ctx.lineTo(X(xr[1]), Y(-0.65)); ctx.stroke();
    ctx.setLineDash([]); ctx.strokeStyle = C("--repere");
    ctx.beginPath(); ctx.moveTo(X(xr[0]), Y(0)); ctx.lineTo(X(xr[1]), Y(0)); ctx.stroke();
    // Trait coloré selon le signe ; rupture de trait sur les trous > 3 pas.
    ctx.lineWidth = 1.4;
    const trou = (r.agrege ? (xr[1] - xr[0]) / v.length : r.pas_s) * 3;
    for (let i = 1; i < v.length; i++) {
      if (v[i].t - v[i - 1].t > trou) continue;
      ctx.strokeStyle = (v[i].moy + v[i - 1].moy) / 2 > 0 ? C("--chaud") : C("--froid");
      ctx.beginPath(); ctx.moveTo(X(v[i - 1].t), Y(v[i - 1].moy)); ctx.lineTo(X(v[i].t), Y(v[i].moy)); ctx.stroke();
    }
  }

  // --- 2. Épisodes radiatifs -----------------------------------------------------------
  const COLONNES = [
    ["coucher", "Date (soir)", (e) => fDate(e.coucher)],
    ["debut", "Début", (e) => fHeure(e.debut)],
    ["t_min", "Heure du min.", (e) => fHeure(e.t_min)],
    ["tmin", "Tmin °C", (e) => nf(e.tmin)],
    ["amplitude", "Amplitude °C", (e) => nf(e.amplitude)],
    ["refroidissement_max", "Refroid. max °C/h", (e) => nf(e.refroidissement_max)],
    ["duree_h", "Durée h", (e) => nf(e.duree_h)],
    ["rupture", "Rupture", (e) => fHeure(e.rupture)],
  ];
  async function calculerEpisodes() {
    const point = $("station").value;
    if (!point) throw new Error("Aucune station.");
    $("e-statut").textContent = "Calcul…";
    const r = await appeler("episodes", Object.assign(periode(), { point }));
    etat.ep = r; etat.episodes = r.resultat.episodes; etat.nuit = null;
    const res = r.resultat, rej = res.rejets;
    $("e-statut").replaceChildren(el("div", {}, `${r.point.name} — série ${r.serie.role}${r.serie.label ? " « " + r.serie.label + " »" : ""}, pas d'analyse ${res.pas_s / 60} min. ` +
      `Heures locales du poste.`), ...r.avertissements.map((a) => el("div", { className: "avert" }, "⚠ " + a)));
    const tuiles = [
      [`${res.episodes.length} / ${res.nuits_examinees}`, "nuits radiatives détectées"],
      [nf(moyenne(res.episodes.map((e) => e.tmin))), "Tmin moyenne des épisodes °C"],
      [nf(moyenne(res.episodes.map((e) => e.amplitude))), "amplitude moyenne °C"],
      [`${rej.refroidissement} · ${rej.amplitude} · ${rej.remontee} · ${rej.plateau} · ${rej.rupture} · ${rej.vent}`, "rejets : refroid. · ampl. · remontée · plateau · rupture · vent"],
      [String(rej.donnees_insuffisantes), "nuits sans données suffisantes"],
    ];
    $("e-stats").replaceChildren(...tuiles.map(([v, l]) => el("div", { className: "stat" }, el("b", {}, v), el("span", {}, l))));
    const s = r.seuils;
    $("e-criteres").innerHTML = "";
    $("e-criteres").append(
      el("p", {}, `Une nuit est retenue si : (1) la baisse sur 1 h glissante atteint ${nf(s.refroidissement_min_c_h)} °C/h entre coucher − 3 h et coucher + 3 h (le début est la première heure à ${nf(s.debut_c_h)} °C/h) ; ` +
        `(2) Tmax de l'après-midi − Tmin ≥ ${nf(s.amplitude_min_c)} °C ; (3a) jamais de remontée de plus de ${nf(s.remontee_max_c)} °C avant le minimum ; ` +
        `(3b) plateau : baisse moyenne < ${nf(s.plateau_max_c_h)} °C/h dans les 3 h avant le minimum ; (4) rupture matinale : hausse ≥ ${nf(s.rupture_c_h)} °C/h sur 1 h, pas plus d'1 h avant le lever du soleil ; ` +
        `(5) si le vent est mesuré, vent moyen nocturne ≤ ${nf(s.vent_max_kmh, 0)} km/h. Données requises sur ${nf(s.couverture_min * 100, 0)} % de la nuit.`),
      el("p", {}, "Limites : seuils empiriques non calibrés scientifiquement ; méthode fondée sur la seule température d'une station (pas de nébulosité ni de rayonnement) ; sensible au pas de temps (un pas horaire lisse le refroidissement et décale les heures d'1 h) ; " +
        "les résumés horaires « minimum » accentuent le refroidissement apparent ; une advection froide calme ou un brouillard peuvent être confondus ; en site ventilé (sommet, col) les épisodes réels sont souvent manqués."));
    remplirTable();
    dessinerEpisodes();
    $("e-nuit-cadre")?.remove();
  }
  const moyenne = (a) => (a.length ? a.reduce((x, y) => x + y, 0) / a.length : null);
  function remplirTable() {
    const t = $("e-table"), { cle, asc } = etat.tri;
    t.tHead.replaceChildren(el("tr", {}, ...COLONNES.map(([k, titre]) => {
      const th = el("th", { className: k === cle ? "tri" + (asc ? " asc" : "") : "" }, titre);
      th.addEventListener("click", () => { etat.tri = { cle: k, asc: k === cle ? !asc : true }; remplirTable(); });
      return th;
    })));
    const lignes = [...etat.episodes].sort((a, b) => {
      // Les heures se comparent à l'heure du jour (tri « 23 h » avant « 5 h » sinon absurde).
      const va = k24(a, cle), vb = k24(b, cle);
      return (va - vb) * (asc ? 1 : -1);
    });
    t.tBodies[0].replaceChildren(...lignes.map((e) => {
      const tr = el("tr", {}, ...COLONNES.map(([, , f]) => el("td", {}, f(e))));
      if (etat.nuit === e) tr.className = "sel";
      tr.addEventListener("click", () => afficherNuit(e).catch((x) => $("e-statut").append(erreur(x))));
      return tr;
    }));
    if (!lignes.length) t.tBodies[0].append(el("tr", {}, el("td", { colSpan: COLONNES.length }, "Aucun épisode détecté sur la période.")));
  }
  function k24(e, cle) {
    if (["debut", "t_min", "rupture"].includes(cle)) { const d = dt(e[cle]); return ((d.getHours() + 12) % 24) * 60 + d.getMinutes(); }
    return e[cle];
  }
  function dessinerEpisodes() {
    const r = etat.ep, cv = $("e-canvas");
    if (!r || $("onglet-episodes").hidden) return;
    const { ctx, w, h } = preparer(cv);
    const { debut, fin } = periode();
    const ev = r.resultat.episodes;
    const ys = ev.flatMap((e) => [e.tmin, e.tmax_veille]).concat([0]);
    const { X, Y } = repere(ctx, w, h, [debut, fin], bornes(ys), { temps: true, fx: fDate, fy: (y) => nf(y, 0), titreY: "°C" });
    const lg = Math.max(2, Math.min(8, (w - 70) / ((fin - debut) / J) * 0.6));
    for (const e of ev) {
      ctx.fillStyle = etat.nuit === e ? C("--accent") : "rgba(183,130,42,0.45)";
      ctx.fillRect(X(e.coucher) - lg / 2, Y(e.tmax_veille), lg, Y(e.tmin) - Y(e.tmax_veille));
      ctx.fillStyle = C("--froid");
      ctx.beginPath(); ctx.arc(X(e.coucher), Y(e.tmin), 3, 0, 7); ctx.fill();
    }
  }
  async function afficherNuit(e) {
    etat.nuit = e;
    remplirTable(); dessinerEpisodes();
    const c = await appeler("courbe", { point: etat.ep.point.id, debut: e.coucher - 8 * H, fin: e.lever + 8 * H });
    etat.courbe = c;
    if (!$("e-nuit-cadre")) {
      const cadre = el("div", { className: "cadre", id: "e-nuit-cadre" }, el("canvas", { id: "e-nuit", height: 240 }),
        el("div", { className: "legende" }, el("span", {}, "Nuit sélectionnée — traits : coucher/lever (gris), début (bleu), minimum (blanc), rupture (orange)")));
      $("e-table").parentElement.after(cadre);
    }
    dessinerNuit();
  }
  function dessinerNuit() {
    const e = etat.nuit, c = etat.courbe, cv = $("e-nuit");
    if (!e || !c || !cv || $("onglet-episodes").hidden) return;
    const { ctx, w, h } = preparer(cv);
    const v = c.valeurs;
    if (!v.length) return;
    const xr = [e.coucher - 8 * H, e.lever + 8 * H];
    const { X, Y, m } = repere(ctx, w, h, xr, bornes(v.map((x) => x.moy)), { temps: true, fx: fHeure, fy: (y) => nf(y, 0), titreY: "°C" });
    const trait = (t, coul, dash) => { ctx.strokeStyle = coul; ctx.setLineDash(dash || []); ctx.beginPath(); ctx.moveTo(X(t), m.h); ctx.lineTo(X(t), h - m.b); ctx.stroke(); ctx.setLineDash([]); };
    trait(e.coucher, C("--repere"), [4, 4]); trait(e.lever, C("--repere"), [4, 4]);
    trait(e.debut, C("--froid")); trait(e.t_min, C("--texte")); trait(e.rupture, C("--accent"));
    ctx.strokeStyle = C("--chaud"); ctx.lineWidth = 1.6; ctx.beginPath();
    v.forEach((x, i) => (i ? ctx.lineTo(X(x.t), Y(x.moy)) : ctx.moveTo(X(x.t), Y(x.moy))));
    ctx.stroke(); ctx.lineWidth = 1;
    ctx.fillStyle = C("--texte"); ctx.textAlign = "left"; ctx.textBaseline = "top";
    ctx.fillText(`${fDate(e.coucher)} : Tmin ${nf(e.tmin)} °C à ${fHeure(e.t_min)}, refroidissement max ${nf(e.refroidissement_max)} °C/h`, m.g + 6, m.h + 2);
  }

  // --- 3. Profil vertical ------------------------------------------------------------------
  let minuterie = null;
  async function calculerProfil() {
    if (etat.sel.size < 2) throw new Error("Cochez au moins deux points.");
    const ids = [...etat.sel];
    const e = etendue(ids);
    const { debut, fin } = periode();
    // Plage du curseur : période choisie ∩ étendue des séries (union : un point
    // sans mesure à l'instant affiché est simplement signalé comme manquant).
    const series = etat.points.filter((p) => ids.includes(p.id)).map(serieParDefaut);
    let a = Math.max(debut, Math.min(...series.map((s) => s.t_min)));
    let b = Math.min(fin, Math.max(...series.map((s) => s.t_max)));
    if (b <= a) throw new Error("Aucune mesure des points cochés sur cette période.");
    if (!(e && e.fin > e.debut && e.debut < b && e.fin > a)) $("p-statut").dataset.avert = "1";
    a = Math.ceil(a / H) * H; b = Math.floor(b / H) * H;
    etat.profil = { ids, a, b, xr: null, r: null };
    const cur = $("curseur");
    cur.max = String(Math.round((b - a) / H));
    cur.value = cur.max;
    $("p-statut").textContent = `Curseur : ${fDateHeure(a)} → ${fDateHeure(b)} (pas 1 h).` +
      ($("p-statut").dataset.avert ? " Les séries cochées ne se recouvrent pas toutes sur cette période : certains points manqueront." : "");
    delete $("p-statut").dataset.avert;
    await majProfil(b);
  }
  $("curseur").addEventListener("input", () => {
    if (!etat.profil) return;
    const t = etat.profil.a + +$("curseur").value * H;
    $("p-instant").value = versInputDH(t);
    clearTimeout(minuterie);
    minuterie = setTimeout(() => majProfil(t).catch((e) => $("p-infos").replaceChildren(erreur(e))), 120);
  });
  $("p-instant").addEventListener("change", () => {
    if (!etat.profil) return;
    const t = depuisInputDH($("p-instant").value);
    $("curseur").value = String(Math.round((t - etat.profil.a) / H));
    majProfil(t).catch((e) => $("p-infos").replaceChildren(erreur(e)));
  });
  $("p-fenetre").addEventListener("change", () => { if (etat.profil?.r) majProfil(etat.profil.r.t).catch(() => {}); });
  async function majProfil(t) {
    $("p-instant").value = versInputDH(t);
    const r = await appeler("profil", { points: etat.profil.ids, t, fenetre_s: +$("p-fenetre").value });
    etat.profil.r = r;
    // Échelle de température élargie au fil du curseur, pour que le profil ne « saute » pas.
    const ts = r.points.map((p) => p.temperature).filter((x) => x != null);
    if (ts.length) {
      const [lo, hi] = [Math.min(...ts) - 1, Math.max(...ts) + 1];
      const xr = etat.profil.xr;
      etat.profil.xr = xr ? [Math.min(xr[0], lo), Math.max(xr[1], hi)] : [lo, hi];
    }
    const manq = r.points.filter((p) => p.temperature == null).map((p) => p.name);
    $("p-infos").replaceChildren(
      el("div", {}, `${fDateHeure(r.t)}${r.fenetre_s ? ` (moyenne sur ${r.fenetre_s / 3600} h centrée)` : " (mesure la plus proche, ±15 min ou ½ intervalle)"} — pente de régression : ` +
        (r.pente_100m == null ? "—" : `${nf(r.pente_100m, 2)} °C/100 m${r.pente_100m > 0 ? " (inversion)" : ""}`)),
      ...(manq.length ? [el("div", { className: "avert" }, "Sans mesure à cet instant : " + manq.join(", "))] : []),
      ...r.avertissements.map((a) => el("div", { className: "avert" }, "⚠ " + a)));
    dessinerProfil();
  }
  function dessinerProfil() {
    const p = etat.profil, cv = $("p-canvas");
    if (!p || !p.r || $("onglet-profil").hidden) return;
    const { ctx, w, h } = preparer(cv);
    const pts = p.r.points.filter((x) => x.temperature != null).sort((a, b) => a.altitude_m - b.altitude_m);
    const alts = p.r.points.map((x) => x.altitude_m);
    const { X, Y } = repere(ctx, w, h, p.xr || [-1, 1], bornes(alts, 0.08), { fx: (v) => nf(v, 0) + " °C", fy: (v) => nf(v, 0) + " m", titreY: "altitude" });
    // Régression (droite T = a + pente·z).
    if (pts.length >= 2 && p.r.pente_100m != null) {
      const mz = moyenne(pts.map((x) => x.altitude_m)), mt = moyenne(pts.map((x) => x.temperature));
      const z0 = Math.min(...alts), z1 = Math.max(...alts), f = (z) => mt + (z - mz) * p.r.pente_100m / 100;
      ctx.setLineDash([5, 4]); ctx.strokeStyle = p.r.pente_100m > 0 ? C("--chaud") : C("--froid");
      ctx.beginPath(); ctx.moveTo(X(f(z0)), Y(z0)); ctx.lineTo(X(f(z1)), Y(z1)); ctx.stroke(); ctx.setLineDash([]);
    }
    ctx.strokeStyle = C("--doux"); ctx.lineWidth = 1.5; ctx.beginPath();
    pts.forEach((x, i) => (i ? ctx.lineTo(X(x.temperature), Y(x.altitude_m)) : ctx.moveTo(X(x.temperature), Y(x.altitude_m))));
    ctx.stroke(); ctx.lineWidth = 1;
    ctx.textBaseline = "middle";
    for (const x of pts) {
      ctx.fillStyle = x.minimum_horaire ? C("--accent") : C("--texte");
      ctx.beginPath(); ctx.arc(X(x.temperature), Y(x.altitude_m), 4, 0, 7); ctx.fill();
      const txt = `${x.name} ${nf(x.temperature)} °C${x.minimum_horaire ? " (min. h)" : ""}`;
      const gauche = X(x.temperature) > w * 0.6;
      ctx.textAlign = gauche ? "right" : "left";
      ctx.fillText(txt, X(x.temperature) + (gauche ? -8 : 8), Y(x.altitude_m));
    }
  }

  ouvrir("gradient");
  parent.postMessage({ gws: "ready" }, "*");
})();

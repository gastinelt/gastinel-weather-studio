/*
 * Gastinel Weather Studio — frontend (vanilla JS, aucun framework).
 *
 * L'écran d'un projet est une grille fixe de quatre zones : carte, stations /
 * export, croisement de données, agent IA. Les trois dernières consomment
 * `state.selectedIds`, alimenté par la carte : c'est le seul état de sélection
 * de l'application, et `renderSelection()` est le seul endroit qui le diffuse.
 *
 * Règles respectées ici et à conserver :
 *  - Aucune règle métier en dur côté JS. Les couples catégorie/taille autorisés
 *    viennent de la commande `category_options` : le Rust reste seul juge.
 *  - Les instants venant du backend sont de l'UTC absolu. Les périodes de
 *    mesures sont donc affichées **en UTC**, explicitement étiquetées, pour ne
 *    jamais réintroduire d'ambiguïté de fuseau. Seules les dates « de gestion »
 *    (dernière ouverture d'un projet) sont affichées en heure locale.
 */

const { invoke } = window.__TAURI__ ? window.__TAURI__.core : { invoke: null };

// ---------------------------------------------------------------- utilitaires

const el = (id) => document.getElementById(id);

/** Petit constructeur d'éléments — évite tout innerHTML avec des données. */
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

/** Tracés des icônes au trait (24×24) : remplacent les emojis de l'interface. */
const TRACES_ICONES = {
  dossier: ["M5 4h4l3 3h7a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-11a2 2 0 0 1 2 -2"],
  repere: ["M9 11a3 3 0 1 0 6 0a3 3 0 0 0 -6 0", "M17.657 16.657l-4.243 4.243a2 2 0 0 1 -2.827 0l-4.244 -4.243a8 8 0 1 1 11.314 0z"],
  cible: ["M12 12m-1 0a1 1 0 1 0 2 0a1 1 0 1 0 -2 0", "M12 12m-5 0a5 5 0 1 0 10 0a5 5 0 1 0 -10 0", "M12 12m-9 0a9 9 0 1 0 18 0a9 9 0 1 0 -18 0"],
  antenne: ["M4 18v-3", "M8 18v-6", "M12 18v-9", "M16 18v-12", "M20 18v-15"],
  fleche: ["M5 12h14", "M13 18l6 -6", "M13 6l6 6"],
  fichier: ["M14 3v4a1 1 0 0 0 1 1h4", "M17 21h-10a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2h7l5 5v11a2 2 0 0 1 -2 2z"],
  image: ["M15 8h.01", "M3 6a3 3 0 0 1 3 -3h12a3 3 0 0 1 3 3v12a3 3 0 0 1 -3 3h-12a3 3 0 0 1 -3 -3v-12z", "M3 16l5 -5c.928 -.893 2.072 -.893 3 0l5 5", "M14 14l1 -1c.928 -.893 2.072 -.893 3 0l3 3"],
  calendrier: ["M4 7a2 2 0 0 1 2 -2h12a2 2 0 0 1 2 2v12a2 2 0 0 1 -2 2h-12a2 2 0 0 1 -2 -2v-12z", "M16 3v4", "M8 3v4", "M4 11h16"],
  attention: ["M12 9v4", "M10.363 3.591l-8.106 13.534a1.914 1.914 0 0 0 1.636 2.871h16.214a1.914 1.914 0 0 0 1.636 -2.87l-8.106 -13.536a1.914 1.914 0 0 0 -3.274 0z", "M12 16h.01"],
  ok: ["M5 12l5 5l10 -10"],
  croix: ["M18 6l-12 12", "M6 6l12 12"],
};

/** Icône SVG décorative ; `classe` s'ajoute à « icone ». */
function icone(nom, classe = "") {
  const NS = "http://www.w3.org/2000/svg";
  const s = document.createElementNS(NS, "svg");
  s.setAttribute("class", `icone ${classe}`.trim());
  s.setAttribute("viewBox", "0 0 24 24");
  s.setAttribute("fill", "none");
  s.setAttribute("stroke", "currentColor");
  s.setAttribute("stroke-width", "1.8");
  s.setAttribute("stroke-linecap", "round");
  s.setAttribute("stroke-linejoin", "round");
  s.setAttribute("aria-hidden", "true");
  for (const d of TRACES_ICONES[nom] || []) {
    const p = document.createElementNS(NS, "path");
    p.setAttribute("d", d);
    s.appendChild(p);
  }
  return s;
}

/** Remplace le contenu de `node` par une icône suivie d'un texte. */
function texteAvecIcone(node, nom, texte) {
  clear(node);
  node.append(icone(nom, "icone-texte"), " " + texte);
}

function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
}

let toastTimer = null;
function toast(message, isError = false) {
  const t = el("toast");
  t.textContent = message;
  t.classList.toggle("is-error", isError);
  t.title = "Cliquer pour fermer";
  t.onclick = () => {
    t.hidden = true;
    clearTimeout(toastTimer);
  };
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    t.hidden = true;
  }, isError ? 9000 : 3500);
}

function showError(node, message) {
  node.textContent = message;
  node.hidden = false;
}

function hideError(node) {
  node.textContent = "";
  node.hidden = true;
}

// Formateurs créés une seule fois : `toLocaleString("fr-FR", …)` en construit
// un nouveau à chaque appel, ce qui se paie sur les listes longues (1 153
// stations du projet France, panneau Imports, pages de mesures).
const FMT_LOCAL = new Intl.DateTimeFormat("fr-FR", { dateStyle: "medium", timeStyle: "short" });
const FMT_UTC = new Intl.DateTimeFormat("fr-FR", {
  dateStyle: "short",
  timeStyle: "short",
  timeZone: "UTC",
});
const FMT_NOMBRES = new Map();

/** Date « de gestion » : heure locale, lisible. */
function fmtLocal(iso) {
  if (!iso) return "—";
  return FMT_LOCAL.format(new Date(iso));
}

/** Instant de mesure : UTC explicite, jamais converti en local. */
function fmtUtc(iso) {
  if (!iso) return "—";
  return `${FMT_UTC.format(new Date(iso))} UTC`;
}

function fmtTemp(value, digits = 1) {
  if (value === null || value === undefined) return "—";
  return `${value.toFixed(digits)} °C`;
}

function fmtNumber(value, digits = 0) {
  if (value === null || value === undefined) return "—";
  let fmt = FMT_NOMBRES.get(digits);
  if (!fmt) {
    fmt = new Intl.NumberFormat("fr-FR", {
      minimumFractionDigits: digits,
      maximumFractionDigits: digits,
    });
    FMT_NOMBRES.set(digits, fmt);
  }
  return fmt.format(value);
}

function fmtDuration(seconds) {
  if (seconds === null || seconds === undefined) return "—";
  if (seconds % 3600 === 0) return `${seconds / 3600} h`;
  if (seconds % 60 === 0) return `${seconds / 60} min`;
  return `${seconds} s`;
}

/**
 * Décodage d'un fichier logger.
 *
 * ElitechLog tourne sous Windows : un TXT peut arriver en UTF-16 LE (avec ou
 * sans BOM) ou en Windows-1252. On tranche ici, côté frontend, pour que le Rust
 * ne voie jamais que de l'UTF-8 propre.
 */
async function readFileAsText(file) {
  const bytes = new Uint8Array(await file.arrayBuffer());

  if (bytes[0] === 0xff && bytes[1] === 0xfe)
    return new TextDecoder("utf-16le").decode(bytes.subarray(2));
  if (bytes[0] === 0xfe && bytes[1] === 0xff)
    return new TextDecoder("utf-16be").decode(bytes.subarray(2));
  if (bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf)
    return new TextDecoder("utf-8").decode(bytes.subarray(3));

  // UTF-16 LE sans BOM : un octet sur deux est nul sur du texte latin.
  const probe = Math.min(bytes.length, 2000);
  let nulls = 0;
  for (let i = 1; i < probe; i += 2) if (bytes[i] === 0) nulls++;
  if (probe > 40 && nulls / (probe / 2) > 0.5)
    return new TextDecoder("utf-16le").decode(bytes);

  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return new TextDecoder("windows-1252").decode(bytes);
  }
}

// -------------------------------------------------------------------- état

const state = {
  categories: [],
  project: null,
  points: [],
  /**
   * Réglages Infoclimat (intervalle de rafraîchissement, statut de la clé).
   *
   * Chargés une fois au démarrage et rafraîchis à chaque enregistrement. Ne
   * contient **jamais** la clé elle-même : le backend n'en renvoie que les
   * derniers caractères et la longueur.
   */
  infoclimat: null,
  /** Station dont la fiche détaillée est ouverte (modale), ou `null`. */
  detailPointId: null,
  /**
   * Stations cochées pour le croisement de données.
   *
   * La carte en est la source de vérité (`GwsMap.getSelection()`), cette copie
   * n'existe que pour éviter d'interroger la carte à chaque rendu de ligne.
   * Trois consommateurs : la liste des stations, le bloc « Croisement de
   * données » (`cross.js`) et l'agent IA — tous par `renderSelection()`.
   */
  selectedIds: [],
  /**
   * Sélection propre à la page 2 (Export), utilisée seulement quand la carte
   * n'a aucune sélection en cours (voir `renderExportPage`) — sinon la
   * sélection de la carte fait foi, pas de doublon d'état à synchroniser.
   */
  exportSelection: new Set(),
  /**
   * Prénom de l'utilisateur (`app_settings.user_name`), ou `null` à la toute
   * première ouverture. Alimente les scripts de l'écran d'accueil et le system
   * prompt de l'agent.
   */
  userName: null,
  /**
   * Réglages de l'agent : modèle, statut de la clé Groq, plafond de tokens.
   * Comme pour Infoclimat, **ne contient jamais la clé**.
   */
  agent: null,
  /** Statut du relai SSH vers le serveur distant (prompt 25). */
  sshRelay: null,
  /** Statut de la synchronisation Cloud chiffrée. */
  cloud: null,
  /** Un appel d'agent est en cours : les boutons rapides sont désactivés. */
  agentBusy: false,
};

// --------------------------------------------------------------- paramètres

/**
 * Écran Paramètres. Deux réglages seulement à ce stade, tous deux liés à
 * l'API Infoclimat : le reste de l'écran viendra avec les chantiers concernés.
 */
async function loadSettings() {
  // Tout part en parallèle : lancés l'un après l'autre, ces appels
  // additionnaient leurs attentes (jusqu'à ~750 ms pendant une synchro Cloud).
  const [infoclimat, agent, ssh] = await Promise.all([
    invoke("infoclimat_settings"),
    invoke("agent_settings"),
    invoke("get_ssh_relay_status").catch((e) => {
      console.warn("Impossible de charger le statut du relai SSH :", e);
      return state.sshRelay;
    }),
    // Réglages secondaires : aucun ne doit empêcher l'écran de s'afficher.
    loadTilesCacheInfo().catch((e) => console.warn("Cache de tuiles :", e)),
    loadUpdateSettings(),
    loadIssuedShares(),
    loadCloudStatus(),
  ]);
  state.infoclimat = infoclimat;
  state.agent = agent;
  state.userName = agent.user_name;
  state.sshRelay = ssh;
  renderSettings();
}

// ---------------------------------------------------------- cache de tuiles

/** Octets → texte lisible (Ko/Mo), même échelle que le reste de l'écran. */
function fmtBytes(octets) {
  if (!octets) return "0 Ko";
  const mo = octets / (1024 * 1024);
  if (mo >= 1) return `${mo.toFixed(1)} Mo`;
  return `${Math.max(1, Math.round(octets / 1024))} Ko`;
}

async function loadTilesCacheInfo() {
  state.tilesCache = await invoke("tiles_cache_info");
  renderTilesCacheInfo();
}

function renderTilesCacheInfo() {
  const info = state.tilesCache;
  const bloc = el("setting-tiles-cache");
  if (!info || !bloc) return;
  bloc.textContent =
    `${fmtBytes(info.total_bytes)} sur disque — ` +
    `Photo aérienne : ${fmtNumber(info.photo.tile_count)} tuile(s), ${fmtBytes(info.photo.bytes)} · ` +
    `TOP25/SCAN25 : ${fmtNumber(info.scan25.tile_count)} tuile(s), ${fmtBytes(info.scan25.bytes)}.`;
}

async function clearTilesCache() {
  const confirmed = await confirm(
    "Vider le cache de tuiles ? Les zones déjà téléchargées ne seront plus " +
    "disponibles hors-ligne tant qu'elles n'auront pas été revisitées en ligne."
  );
  if (!confirmed) return;

  try {
    await invoke("clear_tiles_cache");
    await loadTilesCacheInfo();
    toast("Cache de tuiles vidé.");
  } catch (e) {
    showError(el("setting-error"), String(e));
  }
}

/**
 * Bloc « clé configurée / aucune clé », commun aux deux fournisseurs.
 *
 * Les deux clés arrivent sous la même forme (`KeyStatus`, produit par
 * `api_keys.rs`) : un seul rendu, sinon les deux blocs divergeraient au premier
 * ajustement.
 */
function renderKeyStatus(box, key, keyFile) {
  clear(box);

  if (key.configured) {
    box.appendChild(
      h(
        "p",
        { class: "key-line" },
        h("span", { class: "chip chip-primary", text: "Clé configurée" }),
        // La clé n'arrive jamais en clair du backend : on n'affiche que ce
        // qu'il consent à dire, de quoi identifier la clé sans la divulguer.
        h("span", { class: "mono", text: `${key.hint} · ${key.length} caractères` })
      )
    );
    if (key.source_path) {
      box.appendChild(h("p", { class: "hint", text: `Lue dans ${key.source_path}` }));
    }
  } else {
    box.appendChild(
      h(
        "p",
        { class: "key-line" },
        h("span", { class: "chip", text: "Aucune clé" }),
        h("span", { class: "muted", text: keyFile })
      )
    );
    box.appendChild(h("p", { class: "hint", text: key.problem || "" }));
  }
}

function renderSettings() {
  const s = state.infoclimat;
  if (!s) return;

  el("setting-refresh").value = s.refresh_min_minutes;
  el("setting-attribution").textContent =
    `Mention affichée avec ces données : « ${s.attribution} ». ` +
    `Chaque appel couvre au plus ${s.max_days_per_request} jours ; ` +
    `les périodes plus longues sont découpées automatiquement.`;

  renderKeyStatus(el("setting-key"), s.key, s.key_file);
  const inputIc = el("setting-infoclimat-key-input");
  if (inputIc) inputIc.value = "";

  el("setting-user-name").value = state.userName || "";

  // Préférences des onglets Elitech : réglées ici, mais rangées et appliquées
  // par `GwsElitech` lui-même — elles n'ont rien à faire dans l'état applicatif
  // ni dans une commande Rust, ce sont des préférences d'écran locales.
  const prefsElitech = GwsElitech.getPrefs();
  el("setting-elitech-close-on-exit").checked = prefsElitech.closeTabsOnExit;
  for (const bouton of document.querySelectorAll('input[name="elitech-tab-position"]')) {
    bouton.checked = bouton.value === prefsElitech.tabPosition;
  }

  const a = state.agent;
  if (a) {
    renderKeyStatus(el("setting-groq-key"), a.key, a.key_file);
    // Toujours vide : la clé n'arrive jamais en clair du backend, et un champ
    // pré-rempli d'une empreinte donnerait l'illusion qu'on peut la modifier
    // caractère par caractère. On colle une clé entière, ou on ne touche à rien.
    el("setting-groq-key-input").value = "";
    el("setting-groq-model").textContent =
      `Modèle : ${a.model}. Le palier gratuit est plafonné à ` +
      `${fmtNumber(a.tokens_per_minute)} tokens par minute, soit environ six analyses ` +
      `consécutives — au-delà, l'agent répond qu'il faut attendre.`;
  }

  const ssh = state.sshRelay;
  if (el("setting-ssh-enabled")) {
    if (ssh) {
      el("setting-ssh-enabled").checked = !!ssh.enabled;
      el("setting-ssh-host").value = ssh.host || "";
      el("setting-ssh-port").value = ssh.port || 22;
      el("setting-ssh-user").value = ssh.user || "";
      for (const radio of document.querySelectorAll('input[name="ssh-auth-type"]')) {
        radio.checked = radio.value === ssh.auth_type;
      }
      updateSshAuthTypeVisibility(ssh.auth_type);
      el("setting-ssh-key-path").value = ssh.key_path || "";
      el("setting-ssh-passphrase").value = "";
      el("setting-ssh-passphrase").placeholder = ssh.has_passphrase
        ? "Phrase secrète enregistrée (laisser vide pour conserver)"
        : "Laisser vide si la clé n'est pas chiffrée";
      el("setting-ssh-password").value = "";
      el("setting-ssh-password").placeholder = ssh.has_password
        ? "Mot de passe enregistré (laisser vide pour conserver)"
        : "Mot de passe du compte SSH distant";
    } else {
      updateSshAuthTypeVisibility(getSelectedSshAuthType());
    }

    renderSshHostKey();

    const resBox = el("ssh-test-result");
    if (resBox) {
      resBox.style.display = "none";
      resBox.textContent = "";
      resBox.className = "";
    }
  }

  // Partage par code dans les Paramètres : désactivé si aucun projet ouvert
  const btnShareSettings = el("btn-share-settings");
  const hintShareSettings = el("share-settings-hint");
  if (btnShareSettings && hintShareSettings) {
    const hasProject = !!state.project;
    btnShareSettings.disabled = !hasProject;
    hintShareSettings.hidden = hasProject;
  }

  renderCloudStatus();
}


/** Liste des projets dans Paramètres › Gestion des projets. */
async function renderProjetsGestion() {
  const body = el("setting-projects-body");
  if (!body) return;
  let projets;
  try {
    projets = await invoke("list_projects");
  } catch (e) {
    showError(el("setting-error"), String(e));
    return;
  }
  clear(body);
  if (!projets.length) {
    body.appendChild(h("tr", {}, h("td", { class: "muted", text: "Aucun projet." })));
    return;
  }
  for (const p of projets) {
    body.appendChild(
      h(
        "tr",
        {},
        h("td", { class: "strong" }, p.name, p.is_demo ? badgeExemple() : null),
        h("td", { class: "num", text: `${fmtNumber(p.point_count)} points` }),
        h(
          "td",
          { class: "shrink" },
          h("button", {
            type: "button",
            class: "btn btn-sm btn-danger",
            text: "Supprimer",
            onclick: () => supprimerProjet(p),
          })
        )
      )
    );
  }
}

async function supprimerProjet(p) {
  let annulable = true;
  try {
    annulable = (await invoke("project_measurement_count", { project_id: p.id })).undoable;
  } catch (e) {
    console.warn("Taille du projet inconnue :", e);
  }
  const ok = await confirm(
    `Supprimer le projet « ${p.name} » ?\n\n` +
      `${fmtNumber(p.point_count)} points, leurs séries et toutes leurs mesures seront effacés ` +
      "de ce poste, et du Cloud à la prochaine synchronisation.\n\n" +
      (annulable
        ? "Ctrl+Z annule la suppression tant que l'application reste ouverte."
        : "Ce projet est trop volumineux pour être gardé en mémoire : la suppression ne pourra pas être annulée.")
  );
  if (!ok) return;
  try {
    await invoke("delete_project", { project_id: p.id });
    toast(`Projet « ${p.name} » supprimé.${annulable ? " Ctrl+Z pour annuler." : ""}`);
    if (state.project && state.project.id === p.id) {
      el("dlg-settings").close();
      window.location.reload();
      return;
    }
    await renderProjetsGestion();
    await loadProjects();
  } catch (e) {
    showError(el("setting-error"), String(e));
  }
}

async function saveSettings() {
  hideError(el("setting-error"));
  const minutes = Number(el("setting-refresh").value);
  if (!Number.isInteger(minutes) || minutes < 1) {
    showError(el("setting-error"), "Indiquez un nombre entier de minutes (1 au minimum).");
    return;
  }
  try {
    // Le prénom d'abord : c'est le seul champ dont la validation peut échouer
    // pour une raison que l'utilisateur vient de créer (champ vidé).
    const nom = el("setting-user-name").value.trim();
    if (nom !== (state.userName || "")) {
      state.userName = await invoke("set_user_name", { name: nom });
    }
    if (!state.infoclimat || state.infoclimat.refresh_min_minutes !== minutes) {
      state.infoclimat = await invoke("set_infoclimat_refresh_minutes", { minutes });
      toast(`Rafraîchissement automatique limité à un appel toutes les ${minutes} minutes.`);
    }
    renderSettings();
  } catch (e) {
    showError(el("setting-error"), String(e));
  }
}

/**
 * Enregistrement de la clé Groq depuis les Paramètres.
 *
 * Bouton séparé de « Enregistrer » : les autres réglages de cette boîte se
 * valident ensemble, la clé non — on veut voir l'empreinte se mettre à jour
 * immédiatement pour être sûr d'avoir collé la bonne.
 *
 * Même chemin que l'écran de première ouverture (`saveGroqKey`), donc même
 * fichier écrit et même statut relu.
 */
async function saveGroqKeyFromSettings() {
  hideError(el("setting-error"));
  const saisie = el("setting-groq-key-input").value.trim();
  if (!saisie) {
    showError(
      el("setting-error"),
      "Collez une clé dans le champ avant d'enregistrer. Laisser le champ vide ne " +
        "supprime pas la clé déjà en place."
    );
    return;
  }
  try {
    await saveGroqKey(saisie);
    toast("Clé Groq enregistrée.");
  } catch (e) {
    showError(el("setting-error"), String(e));
  }
}

/**
 * Enregistrement de la clé Infoclimat depuis les Paramètres.
 *
 * Même comportement que Groq : validation immédiate, rafraîchissement
 * instantané de l'empreinte et vidage du champ de saisie.
 */
async function saveInfoclimatKeyFromSettings() {
  hideError(el("setting-error"));
  const saisie = el("setting-infoclimat-key-input").value.trim();
  if (!saisie) {
    showError(
      el("setting-error"),
      "Collez une clé dans le champ avant d'enregistrer. Laisser le champ vide ne " +
        "supprime pas la clé déjà en place."
    );
    return;
  }
  try {
    await saveInfoclimatKey(saisie);
    toast("Clé Infoclimat enregistrée.");
  } catch (e) {
    showError(el("setting-error"), String(e));
  }
}

/**
 * Met à jour l'affichage des champs d'authentification SSH (clé privée vs mot de passe).
 */
function updateSshAuthTypeVisibility(authType) {
  const type = authType || getSelectedSshAuthType();
  const keyBox = el("ssh-auth-key-fields");
  const pwdBox = el("ssh-auth-password-fields");
  if (keyBox) keyBox.hidden = type !== "key";
  if (pwdBox) pwdBox.hidden = type !== "password";
}

function getSelectedSshAuthType() {
  const checked = document.querySelector('input[name="ssh-auth-type"]:checked');
  return checked ? checked.value : "key";
}

function getSshRelayFormInput() {
  const enabled = !!el("setting-ssh-enabled")?.checked;
  const host = el("setting-ssh-host")?.value.trim() || "";
  const port = Number(el("setting-ssh-port")?.value) || 22;
  const user = el("setting-ssh-user")?.value.trim() || "";
  const authType = getSelectedSshAuthType();
  const keyPath = el("setting-ssh-key-path")?.value.trim() || "";
  const passphraseRaw = el("setting-ssh-passphrase")?.value;
  const passphrase = passphraseRaw && passphraseRaw.trim() ? passphraseRaw : null;
  const passwordRaw = el("setting-ssh-password")?.value;
  const password = passwordRaw && passwordRaw.trim() ? passwordRaw : null;

  return {
    enabled,
    host,
    port,
    user,
    authType,
    keyPath: keyPath || null,
    passphrase,
    password,
  };
}

async function browseSshKeyFile() {
  try {
    const selected = await invoke("pick_ssh_key_file");
    if (selected) {
      el("setting-ssh-key-path").value = selected;
    }
  } catch (e) {
    showError(el("setting-error"), String(e));
  }
}

async function saveSshRelayFromSettings() {
  hideError(el("setting-error"));
  const input = getSshRelayFormInput();

  if (input.enabled || input.host || input.user) {
    if (!input.host) {
      showError(el("setting-error"), "Veuillez renseigner l'adresse du serveur SSH.");
      return;
    }
    if (!input.user) {
      showError(el("setting-error"), "Veuillez renseigner l'utilisateur SSH.");
      return;
    }
    if (input.authType === "key" && !input.keyPath) {
      showError(el("setting-error"), "Veuillez sélectionner un fichier de clé privée SSH.");
      return;
    }
  }

  try {
    const status = await invoke("save_ssh_relay_config", {
      input: {
        enabled: input.enabled,
        host: input.host,
        port: input.port,
        user: input.user,
        auth_type: input.authType,
        key_path: input.keyPath,
        key_passphrase: input.passphrase,
        password: input.password,
      },
    });
    state.sshRelay = status;
    renderSettings();
    toast("Configuration du relai SSH enregistrée.");
  } catch (e) {
    showError(el("setting-error"), String(e));
  }
}

async function toggleSshRelayEnabled(e) {
  hideError(el("setting-error"));
  const enabled = e.target.checked;
  if (state.sshRelay && state.sshRelay.host) {
    try {
      state.sshRelay = await invoke("set_ssh_relay_enabled", { enabled });
      toast(enabled ? "Relai SSH activé." : "Relai SSH désactivé (appels directs).");
    } catch (err) {
      e.target.checked = !enabled;
      showError(el("setting-error"), String(err));
    }
  }
}

async function testSshRelayConnection() {
  hideError(el("setting-error"));
  const resBox = el("ssh-test-result");
  if (resBox) {
    resBox.style.display = "block";
    resBox.className = "banner";
    resBox.textContent = "Test de connexion en cours…";
  }
  const btn = el("btn-test-ssh");
  if (btn) btn.disabled = true;

  const input = getSshRelayFormInput();
  if (!input.host) {
    if (btn) btn.disabled = false;
    if (resBox) {
      resBox.className = "banner is-danger";
      resBox.textContent = "Veuillez renseigner l'adresse du serveur SSH.";
    }
    return;
  }
  if (!input.user) {
    if (btn) btn.disabled = false;
    if (resBox) {
      resBox.className = "banner is-danger";
      resBox.textContent = "Veuillez renseigner l'utilisateur SSH.";
    }
    return;
  }
  if (input.authType === "key" && !input.keyPath) {
    if (btn) btn.disabled = false;
    if (resBox) {
      resBox.className = "banner is-danger";
      resBox.textContent = "Veuillez sélectionner un fichier de clé privée SSH.";
    }
    return;
  }

  try {
    const resultat = await invoke("test_ssh_relay_connection", {
      input: {
        enabled: input.enabled,
        host: input.host,
        port: input.port,
        user: input.user,
        auth_type: input.authType,
        key_path: input.keyPath,
        key_passphrase: input.passphrase,
        password: input.password,
      },
    });
    if (resBox) afficherResultatSsh(resBox, resultat, input);
  } catch (err) {
    if (resBox) {
      resBox.className = "banner is-danger";
      texteAvecIcone(resBox, "croix", "Échec du test SSH : " + String(err));
    }
  } finally {
    if (btn) btn.disabled = false;
  }
}

async function clearAllData() {
  const confirmed = await confirm(
    "Êtes-vous sûr(e) ? Cette action supprimera :\n" +
    "• Tous les projets\n" +
    "• Tous les points\n" +
    "• Toutes les données importées\n" +
    "• Votre prénom\n\n" +
    "Cette action ne peut pas être annulée."
  );

  if (!confirmed) return;

  try {
    await invoke("clear_all_data");
    toast("Toutes les données ont été supprimées.");
    el("dlg-settings").close();
    setTimeout(() => window.location.reload(), 500);
  } catch (e) {
    showError(el("setting-error"), String(e));
  }
}

// ------------------------------------------------------------------ projets

/** Pastille « Exemple » d'un projet de démonstration. */
function badgeExemple() {
  return h("span", {
    class: "badge-exemple",
    text: "Exemple",
    title: "Données synthétiques, jamais synchronisées vers le Cloud",
  });
}

/** Bouton « Explorer un projet d'exemple » : crée le projet, puis l'ouvre. */
async function creerProjetExemple() {
  const bouton = el("btn-demo-project");
  bouton.disabled = true;
  try {
    const projet = await invoke("create_demo_project");
    toast("Projet d'exemple créé : données fictives, supprimable depuis Paramètres › Gestion des projets.");
    await loadProjects();
    await openProject(projet.id);
  } catch (e) {
    toast(String(e), true);
  } finally {
    bouton.disabled = false;
  }
}

async function loadProjects() {
  const projects = await invoke("list_projects");
  const body = el("projects-body");
  clear(body);

  el("projects-empty").hidden = projects.length > 0;
  el("projects-demo").hidden = projects.length > 0;
  el("projects-table").hidden = projects.length === 0;

  for (const p of projects) {
    body.appendChild(
      h(
        "tr",
        { class: "clickable", onclick: () => openProject(p.id) },
        h("td", { class: "strong" }, p.name, p.is_demo ? badgeExemple() : ""),
        h("td", { class: "num", text: fmtNumber(p.point_count) }),
        h("td", { text: fmtLocal(p.last_opened_at) }),
        h(
          "td",
          { class: "shrink" },
          h("span", { class: "link", text: "Ouvrir →" })
        )
      )
    );
  }
  return projects;
}

/**
 * Ouvre un projet : chargement, puis **écran d'accueil**, puis grille.
 *
 * Le découpage en deux fonctions vient de là. `open_project` est appelé ici (il
 * met à jour la date de dernière ouverture et renvoie la précédente, dont le
 * script d'accueil a besoin) ; l'installation de la grille attend, elle, que
 * l'utilisateur ait choisi un des trois boutons — ou que les sept secondes
 * soient passées sans qu'il interrompe le compte à rebours. Voir `showHomeScreen`.
 */
async function openProject(projectId) {
  const opened = await invoke("open_project", { project_id: projectId });
  state.project = opened;
  state.detailPointId = null;
  state.selectedIds = [];
  state.exportSelection = new Set();
  showHomeScreen(opened);

  // Synchronisation Cloud automatique en tâche de fond, une seule fois par
  // session : si le démarrage l'a déjà faite, on ne la relance pas ici.
  if (state.cloud?.logged_in && state.cloud?.auto_sync && !cloudAutoSyncFaite) {
    cloudAutoSyncFaite = true;
    invoke("cloud_sync").then(res => {
      if (res && (res.uploaded > 0 || res.downloaded > 0)) {
        toast(`Synchronisation Cloud : ${res.uploaded} envoyé(s), ${res.downloaded} reçu(s).`);
        loadCloudStatus().then(renderCloudStatus);
      }
    }).catch(e => console.warn("Synchro Cloud auto :", e));
  }
}

/**
 * Vrai dès qu'une synchronisation Cloud automatique a été lancée dans la
 * session (démarrage ou première ouverture de projet). Les synchronisations
 * demandées (bouton, connexion) ne sont pas concernées.
 */
let cloudAutoSyncFaite = false;

/** Historique minimal de navigation entre la liste et un projet (PC). */
const navState = { projetQuitte: null };

/**
 * Installe la grille 2×2 du projet déjà chargé dans `state.project`.
 *
 * Séparée d'`openProject` pour que l'écran d'accueil puisse s'intercaler sans
 * dupliquer cette mise en place — c'est le même `playReveal` qu'au prompt 3.
 */
async function enterWorkspace() {
  el("view-projects").hidden = true;
  el("view-home").hidden = true;
  GwsElitech.close();
  el("view-project").hidden = false;
  el("topbar-project").textContent = state.project.name;
  el("topbar-sep").hidden = false;
  el("btn-home").disabled = false;

  showStationsPage(0);
  resetAgentBlock();
  GwsCross.reset();
  GwsCrossPlein.reset();
  renderSelection();
  playReveal();

  // L'ordre compte : Leaflet mesure son conteneur à la création, et
  // `#view-project` vient tout juste d'être révélé. Créer la carte avant
  // l'affichage donnerait une carte grise de hauteur nulle.
  GwsMap.ensure("map", {
    onCreateHere: (lat, lon) => openPointDialog("create", { lat, lon }),
    onEditPoint: (id) => openPointDialog("edit", { point: findPoint(id) }),
    onOpenDetail: (id) => openDetail(id),
    onFetchData: (id) => fetchDataFromMap(id),
    onSelectionChange: (ids) => {
      state.selectedIds = ids;
      renderSelection();
    },
  });
  GwsMap.refresh();

  await loadPoints();
}

/**
 * Fondu d'apparition des quatre blocs de la grille.
 *
 * La classe est retirée à la fin de l'animation : la laisser en place ferait
 * rejouer le fondu au moindre recalcul de style, et les blocs clignoteraient
 * pendant l'usage courant.
 */
function playReveal() {
  const grille = el("view-project");
  grille.classList.remove("is-revealing");
  // Forcer un reflow, sans quoi retirer puis remettre la classe dans la même
  // frame ne relance rien : le navigateur ne voit aucun changement net.
  void grille.offsetWidth;
  grille.classList.add("is-revealing");
  setTimeout(() => grille.classList.remove("is-revealing"), 700);
}

/**
 * Logique du bouton Accueil de la barre supérieure : ferme la page Elitech
 * si elle est ouverte, sinon revient à la liste des projets — en un seul
 * geste, quel que soit le nombre d'onglets Elitech ouverts.
 */
function goHome() {
  // Sur téléphone, l'écran d'un logger se referme comme le reste : le bouton
  // d'accueil rend la liste des projets, avec sa zone de détection.
  if (GwsMobile.retourAccueil()) return;
  // La page Elitech se ferme d'abord : elle peut être ouverte par-dessus un
  // projet comme depuis la liste, et le bouton d'accueil doit rendre la même
  // chose dans les deux cas — la liste des projets.
  if (!el("view-elitech").hidden) {
    closeElitechPage();
    if (state.project) backToProjects();
    return;
  }
  if (state.project) backToProjects();
}

/**
 * Un cran « arrière » dans la hiérarchie de l'app, pour le bouton/geste
 * retour Android (cf. MainActivity.kt, qui appelle `window.__gwsAndroidBack`
 * avant de fermer l'activité). Contrairement au bouton Accueil ci-dessus, la
 * page Elitech remonte un onglet à la fois — même logique que sa croix de
 * fermeture — avant de se refermer complètement :
 *
 *  0. (téléphone) écran plein d'un logger RC-5 → revenir à l'accueil, où ses
 *     widgets de détection attendent. C'est le seul chemin de retour de cet
 *     écran : il n'y a pas d'onglets pour passer d'un logger à l'autre, on
 *     remonte et on redescend (cf. `mobile.js`) ;
 *  1. un onglet de logger actif → le fermer (retour à la détection ou à
 *     l'onglet précédent, cf. `GwsElitech.fermerOngletActif`) ;
 *  2. sinon, page Elitech sur l'onglet détection, ou espace de travail d'un
 *     projet ouvert → revenir à la liste des projets ;
 *  3. sinon (déjà sur la liste des projets, écran racine) → rien à faire,
 *     Android applique son comportement par défaut (quitter l'app).
 *
 * Renvoie `true` si un cran a été remonté, `false` au cas 3.
 */
function navigateAndroidBack() {
  // Testé en premier : sur téléphone, cet écran se superpose à la liste des
  // projets et rien d'autre ne peut être ouvert par-dessus.
  if (GwsMobile.retourAccueil()) return true;
  if (!el("view-elitech").hidden) {
    if (GwsElitech.fermerOngletActif()) return true;
    closeElitechPage();
    if (state.project) backToProjects();
    return true;
  }
  if (state.project) {
    backToProjects();
    return true;
  }
  return false;
}

// Pont explicite pour le bouton/geste retour Android : nom stable indépendant
// du nom de la fonction interne.
window.__gwsAndroidBack = navigateAndroidBack;

function backToProjects() {
  // Retenu pour « Avancer » (Alt+→, bouton avant de la souris).
  if (state.project) navState.projetQuitte = state.project.id;
  state.project = null;
  state.selectedIds = [];
  state.detailPointId = null;
  // Un appel d'agent en vol doit être oublié, sinon sa réponse s'afficherait
  // dans le bloc d'un autre projet.
  resetAgentBlock();
  GwsCross.destroy();
  annulerRemplissage();
  GwsTypewriter.cancel(el("home-script"));
  GwsMap.destroy();
  el("dlg-detail").close();
  GwsElitech.close();
  el("view-home").hidden = true;
  el("view-project").hidden = true;
  el("view-projects").hidden = false;
  el("topbar-project").textContent = "";
  el("topbar-sep").hidden = true;
  el("btn-home").disabled = true;
  loadProjects().catch((e) => toast(String(e), true));
}

// ------------------------------------------------ bloc Stations : pagination

/** Page courante du bloc Stations / Export (0 = stations, 1 = export). */
let stationsPage = 0;
const TITRES_PAGES = ["Points", "Export"];

function showStationsPage(index) {
  const total = TITRES_PAGES.length;
  const cible = Math.max(0, Math.min(total - 1, index));

  // Sens de l'animation : la page entrante arrive du côté d'où on vient.
  const pager = document.querySelector(".panel-stations .pager");
  if (pager) pager.dataset.dir = cible < stationsPage ? "back" : "forward";

  stationsPage = cible;
  el("stations-title").textContent = TITRES_PAGES[cible];

  for (const page of document.querySelectorAll(".panel-stations .pager-page"))
    page.classList.toggle("is-active", Number(page.dataset.page) === cible);

  for (const dot of document.querySelectorAll("#stations-dots .dot"))
    dot.classList.toggle("is-active", Number(dot.dataset.page) === cible);

  el("stations-prev").disabled = cible === 0;
  el("stations-next").disabled = cible === total - 1;

  if (cible === 1) renderExportPage();
}

// ------------------------------------------------------------------- points

async function loadPoints() {
  state.points = await invoke("list_points", { project_id: state.project.id });
  const body = el("points-body");
  clear(body);

  el("points-empty").hidden = state.points.length > 0;
  el("points-body").hidden = state.points.length === 0;

  for (const p of state.points) {
    body.appendChild(
      h(
        "li",
        {
          // Clic sur la ligne = sélection, exactement comme un clic sur le
          // marqueur. Le même objet doit répondre au même geste des deux côtés,
          // sinon la sélection devient un jeu de piste.
          class: "station",
          "data-point-id": p.id,
          title: "Cliquer pour sélectionner ce point (croisement de données)",
          onclick: () => GwsMap.toggleSelection(p.id),
        },
        h(
          "div",
          { class: "station-main" },
          h(
            "div",
            { class: "station-line" },
            h("span", { class: "station-name", text: p.name }),
            h("span", { class: "chip chip-cat", text: p.category }),
            p.size ? h("span", { class: "chip", text: p.size }) : null
          ),
          h("div", {
            class: "station-meta",
            text:
              `${p.altitude_m === null ? "altitude —" : fmtNumber(p.altitude_m) + " m"}` +
              ` · ${fmtNumber(p.series_count)} série(s)` +
              ` · ${p.latitude.toFixed(4)}, ${p.longitude.toFixed(4)}`,
          })
        ),
        h(
          "div",
          { class: "station-actions" },
          h("button", {
            class: "btn btn-sm",
            type: "button",
            text: "Détail",
            onclick: (e) => {
              e.stopPropagation();
              openDetail(p.id);
            },
          }),
          // L'édition ne doit pas dépendre de la carte : un point mal placé (ou
          // sans position exploitable) doit rester modifiable depuis la liste.
          h("button", {
            class: "btn btn-sm",
            type: "button",
            text: "Modifier",
            onclick: (e) => {
              e.stopPropagation();
              openPointDialog("edit", { point: p });
            },
          }),
          h("button", {
            class: "btn btn-sm btn-danger",
            type: "button",
            text: "Supprimer",
            onclick: (e) => {
              e.stopPropagation();
              deletePoint(p).catch((error) => toast(String(error), true));
            },
          })
        )
      )
    );
  }

  GwsMap.setPoints(state.points);
  // `setPoints` a pu élaguer des ids devenus caducs : on relit la source de vérité.
  state.selectedIds = GwsMap.getSelection();
  renderSelection();
  if (stationsPage === 1) renderExportPage();

  // Rafraîchit la fiche ouverte, le cas échéant.
  if (state.detailPointId && el("dlg-detail").open) {
    await renderDetail(state.detailPointId);
  }
}

// -------------------------------------------------- sélection / croisement

/**
 * Répercute la sélection courante partout où elle se lit.
 *
 * Quatre endroits, un seul état : les lignes de la liste, le bandeau de la page
 * Stations, le bloc « Croisement de données » et l'agent IA. C'est le seul
 * point de diffusion — un consommateur qui irait interroger `GwsMap` de son
 * côté finirait par se désynchroniser des trois autres.
 *
 * Croisement et agent réagissent tous deux au changement, mais avec des
 * temporisations différentes et assumées : 250 ms pour le croisement (requête
 * SQLite locale), 900 ms pour l'agent (quota d'API distant). Ils restent par
 * ailleurs indépendants — l'un n'attend jamais l'autre.
 */
function renderSelection() {
  const ids = new Set(state.selectedIds);

  for (const ligne of el("points-body").children)
    ligne.classList.toggle("is-picked", ids.has(ligne.dataset.pointId));

  const barre = el("selection-bar");
  barre.hidden = ids.size === 0;
  el("selection-count").textContent =
    ids.size === 1 ? "1 point sélectionné" : `${ids.size} points sélectionnés`;

  // La couleur d'un point est décidée ici, une fois, puis poussée aux deux
  // vues qui l'affichent. La carte ne connaît donc pas `cross.js` (elle reste
  // le module autonome qu'elle était), et les deux ne peuvent pas diverger :
  // le point coché en vermillon sur la carte est la courbe vermillon du
  // graphique. Voir le commentaire de `PALETTE` dans `cross.js`.
  GwsMap.setSelectionColors(GwsCross.paletteFor(state.selectedIds));

  GwsCross.setSelection(state.selectedIds);
  renderAgentActions();
  // Troisième mode de déclenchement de l'agent : automatique dès qu'au moins
  // deux points sont sélectionnés, avec debounce (cf. `planifierAnalyseAuto`).
  planifierAnalyseAuto();

  if (stationsPage === 1) renderExportPage();
}

// -------------------------------------------------------------- export (page 2)

/**
 * Libellé lisible de la fenêtre courante du bloc Croisement de données —
 * l'export n'a pas son propre sélecteur, voir `cross.js::currentWindow`.
 */
function descriptionFenetreExport(fenetre) {
  switch (fenetre.kind) {
    case "last7_days":
      return "7 derniers jours de données";
    case "last30_days":
      return "30 derniers jours de données";
    case "current_year":
      return "Année de la dernière mesure";
    case "all":
      return "Tout l'historique disponible";
    case "custom":
      return fenetre.from && fenetre.to
        ? `Du ${fenetre.from} au ${fenetre.to}`
        : "Plage personnalisée (incomplète)";
    default:
      return "—";
  }
}

/**
 * Dessine la page 2 du bloc Stations : sélection à exporter et période.
 *
 * La sélection de la carte fait foi dès qu'elle n'est pas vide — c'est la
 * sélection multi-points déjà en place pour le croisement de données, pas la
 * peine d'en recréer une seconde. Elle ne redevient éditable ici que quand la
 * carte n'a rien coché : sans quoi l'export serait bloqué tant que l'utilisateur
 * n'a rien sélectionné sur la carte, ce qui n'a pas de raison d'être.
 */
function renderExportPage() {
  const zone = el("export-selection");
  clear(zone);

  if (state.selectedIds.length > 0) {
    zone.appendChild(
      h(
        "p",
        { class: "muted" },
        state.selectedIds.length === 1
          ? "1 point sélectionné sur la carte."
          : `${state.selectedIds.length} points sélectionnés sur la carte.`
      )
    );
  } else if (state.points.length === 0) {
    zone.appendChild(
      h("p", { class: "placeholder" }, "Aucun point dans ce projet à exporter.")
    );
  } else {
    // Repli : la carte n'a rien coché, l'utilisateur choisit directement ici.
    state.exportSelection = new Set(
      [...state.exportSelection].filter((id) => state.points.some((p) => p.id === id))
    );
    const liste = h("ul", { class: "station-list export-pick-list" });
    for (const p of state.points) {
      const checked = state.exportSelection.has(p.id);
      liste.appendChild(
        h(
          "li",
          { class: "station export-pick-row" },
          h(
            "label",
            { class: "export-pick-label" },
            h("input", {
              type: "checkbox",
              checked,
              onchange: (e) => {
                if (e.target.checked) state.exportSelection.add(p.id);
                else state.exportSelection.delete(p.id);
              },
            }),
            h("span", { text: p.name }),
            h("span", { class: "chip chip-cat", text: p.category })
          )
        )
      );
    }
    zone.appendChild(liste);
  }

  el("export-period").textContent = descriptionFenetreExport(GwsCross.currentWindow());
}

/** Points à exporter : la sélection carte si non vide, sinon la case à cocher locale. */
function exportPointIds() {
  return state.selectedIds.length > 0 ? state.selectedIds : [...state.exportSelection];
}

async function lancerExport(format) {
  const point_ids = exportPointIds();
  if (point_ids.length === 0) {
    toast("Sélectionnez au moins un point à exporter.", true);
    return;
  }

  const detail = format === "csv" || el("export-txt-detail").checked;

  try {
    const contenu = await invoke("build_export", {
      point_ids,
      window: GwsCross.currentWindow(),
      format,
      detail,
    });
    const chemin = await invoke("save_export_file", {
      content: contenu.content,
      suggested_name: contenu.suggested_name,
    });
    if (chemin) toast(`Export enregistré : ${chemin}`);
  } catch (e) {
    toast(String(e), true);
  }
}

// ------------------------------------------------------- identité utilisateur

/**
 * Première ouverture : on demande le prénom avant tout le reste.
 *
 * Bloquant volontairement — l'écran d'accueil s'adresse à quelqu'un, et un
 * « {name} » vide y ferait une phrase bancale à chaque ouverture de projet.
 */
async function loadUserName() {
  state.userName = await invoke("get_user_name");
  return state.userName;
}

/**
 * Première ouverture, étape 1 : la clé de l'agent IA.
 *
 * Placée **avant** le prénom et non à sa place. L'ordre n'est pas arbitraire :
 * la clé est ce qu'un nouvel arrivant ne peut pas deviner, et la demander en
 * premier lui laisse le temps de faire l'aller-retour sur le site de Groq avant
 * d'entrer dans l'application. Le prénom, lui, se remplit d'un trait.
 *
 * Contrairement au prénom, cet écran ne bloque rien : « Passer » enchaîne sur
 * l'étape suivante, et l'application reste entièrement utilisable sans clé.
 *
 * Même détection que l'étape suivante — l'absence de prénom enregistré. Il n'y a
 * donc pas de second drapeau à tenir : quelqu'un qui a déjà nommé son
 * installation ne reverra jamais ni l'un ni l'autre de ces écrans.
 */
function showOnboardingKey() {
  el("view-onboarding-key").hidden = false;
  el("view-onboarding").hidden = true;
  el("view-projects").hidden = true;
  el("btn-settings").disabled = true;
  el("onboarding-key-input").focus();
}

/** Étape 1 franchie — avec ou sans clé : on passe au prénom. */
function showOnboarding() {
  el("view-onboarding-key").hidden = true;
  el("view-onboarding").hidden = false;
  el("view-projects").hidden = true;
  el("btn-settings").disabled = true;
  el("onboarding-name").focus();
}

async function submitOnboardingKey(event) {
  event.preventDefault();
  hideError(el("onboarding-key-error"));

  const saisie = el("onboarding-key-input").value.trim();
  // Un champ laissé vide vaut « Passer » : valider sans rien coller ne doit pas
  // renvoyer une erreur pour un écran qu'on a le droit de ne pas remplir.
  if (!saisie) {
    showOnboarding();
    return;
  }

  try {
    await saveGroqKey(saisie);
    toast("Clé enregistrée. L'agent IA est disponible.");
    showOnboarding();
  } catch (e) {
    showError(el("onboarding-key-error"), String(e));
  }
}

/**
 * Enregistrement de la clé Groq — **le seul** du frontend.
 *
 * Appelée par l'écran de première ouverture et par les Paramètres. Elle
 * rafraîchit `state.agent` avec ce que le backend vient de relire sur le disque,
 * pas avec ce qu'on lui a envoyé : les deux écrans affichent donc toujours la
 * même empreinte, et jamais une clé qu'on croirait enregistrée.
 */
async function saveGroqKey(cle) {
  state.agent = await invoke("set_groq_key", { key: cle });
  state.userName = state.agent.user_name;
  renderSettings();
}

/**
 * Enregistrement de la clé Infoclimat.
 * Rafraîchit `state.infoclimat` relu sur le disque par le backend.
 */
async function saveInfoclimatKey(cle) {
  state.infoclimat = await invoke("set_infoclimat_key", { key: cle });
  renderSettings();
}

/**
 * Ouvre une adresse dans le navigateur du système.
 *
 * Passe par le backend : un `<a href>` naviguerait la webview elle-même et
 * ferait disparaître l'application derrière la page, sans moyen de revenir. La
 * liste des adresses permises est tenue côté Rust (`commands.rs`).
 */
/**
 * Ouvre la page des clés d'API de Groq.
 *
 * L'adresse vient de `agent_settings` — le backend la tient dans
 * `groq::KEY_SOURCE`, d'où sortent déjà les messages d'erreur qui la citent.
 * Si les réglages n'ont pas pu être chargés au démarrage (cas où l'écran de
 * première ouverture s'affiche quand même), on les redemande plutôt que d'écrire
 * une seconde fois l'URL ici.
 */
async function openGroqKeysPage() {
  try {
    if (!state.agent) state.agent = await invoke("agent_settings");
    await openExternal(state.agent.key_signup_url);
  } catch (e) {
    toast(String(e), true);
  }
}

async function openInfoclimatKeysPage() {
  try {
    if (!state.infoclimat) state.infoclimat = await invoke("infoclimat_settings");
    await openExternal(state.infoclimat.key_signup_url);
  } catch (e) {
    toast(String(e), true);
  }
}

async function openExternal(url) {
  try {
    await invoke("open_external_url", { url });
  } catch (e) {
    toast(String(e), true);
  }
}

async function submitOnboarding(event) {
  event.preventDefault();
  hideError(el("onboarding-error"));
  try {
    state.userName = await invoke("set_user_name", { name: el("onboarding-name").value });
    el("view-onboarding").hidden = true;
    el("view-projects").hidden = false;
    el("btn-settings").disabled = false;
    await loadProjects();
    await showQuickstartGuide();
  } catch (e) {
    showError(el("onboarding-error"), String(e));
  }
}

/**
 * `force` : ouverture voulue par l'utilisateur (bouton des réglages), sans
 * condition. Ouverture automatique : pour tout le monde depuis la 0.5.0
 * (stable comme bêta), et jamais si le guide a déjà été vu ou fermé.
 */
async function showQuickstartGuide(force = false) {
  const dlg = el("dlg-quickstart");
  if (!dlg) return;
  if (!force) {
    if (localStorage.getItem("gws_quickstart_seen")) return;
  }
  dlg.showModal();
}

function closeQuickstartGuide() {
  const dlg = el("dlg-quickstart");
  if (dlg && dlg.open) dlg.close();
  localStorage.setItem("gws_quickstart_seen", "true");
}

// ------------------------------------------------------ écran d'accueil

/**
 * Date de dernière ouverture, en relatif tant que c'est lisible.
 *
 * « il y a 3 jours » se comprend sans calcul ; au-delà d'une semaine la date
 * absolue redevient plus informative qu'un décompte de jours.
 */
function fmtRelative(iso) {
  if (!iso) return "à la création du projet";

  const instant = new Date(iso);
  const minutes = Math.floor((Date.now() - instant.getTime()) / 60000);

  if (minutes < 2) return "à l'instant";
  if (minutes < 60) return `il y a ${minutes} minutes`;

  const heures = Math.floor(minutes / 60);
  if (heures < 24) return heures === 1 ? "il y a une heure" : `il y a ${heures} heures`;

  const jours = Math.floor(heures / 24);
  if (jours === 1) return "hier";
  if (jours < 8) return `il y a ${jours} jours`;

  return `le ${instant.toLocaleDateString("fr-FR", { day: "numeric", month: "long", year: "numeric" })}`;
}

/** « 12 points », « 1 point », « aucun point ». */
function fmtPointCount(n) {
  if (!n) return "aucun point";
  return n === 1 ? "1 point" : `${n} points`;
}

/** Dernier script joué, pour ne pas le retirer deux fois de suite. */
let dernierScript = null;

/** Minuteries de l'écran d'accueil, à nettoyer si on en sort par un clic. */
const homeTimers = { auto: null };


/**
 * Écran d'accueil d'un projet.
 *
 * Enchaînement : le texte s'écrit → les boutons apparaissent → celui que le
 * script désigne se remplit sur 7 s → au bout des 7 s son action se déclenche.
 * Comme en 0.4.7, seul un clic sur un des boutons (ou un dépôt de fichier)
 * interrompt le remplissage (cf. `wireUp`).
 *
 * @param {object} opened  réponse de `open_project` (projet + ouverture précédente)
 */
function showHomeScreen(opened) {
  const script = GwsHomeScripts.pick(dernierScript);
  dernierScript = script.text;

  const texte = GwsHomeScripts.fill(script.text, {
    name: state.userName || "Bonjour",
    project_name: opened.name,
    point_count: fmtPointCount(opened.point_count),
    last_opened: fmtRelative(opened.previous_opened_at),
  });

  el("view-projects").hidden = true;
  el("view-project").hidden = true;
  el("view-home").hidden = false;

  const actions = el("home-actions");
  actions.hidden = true;
  annulerRemplissage();

  // Le même composant que le bloc Agent IA — une seule implémentation de
  // l'animation dans toute l'application (cf. `typewriter.js`).
  const anim = GwsTypewriter.type(el("home-script"), texte, { speed: 26 });

  // Cliquer pendant que le texte s'écrit le termine d'un coup plutôt que de
  // forcer l'attente : la phrase reste lisible, l'utilisateur pressé passe.
  const passer = () => anim.skip();
  el("home-script").addEventListener("click", passer, { once: true });

  anim.promise.then(() => {
    if (el("view-home").hidden) return; // sorti entre-temps
    actions.hidden = false;
    demarrerRemplissage(script.defaultAction);
  });
}

/**
 * Remplit le bouton par défaut sur 7 secondes, puis déclenche son action.
 *
 * Le remplissage est une transition CSS sur la largeur d'un calque : la durée
 * vient de `GwsHomeScripts.AUTO_DELAY_MS` et la minuterie JS utilise la même
 * constante, pour que le déclenchement coïncide avec la fin visuelle.
 */
function demarrerRemplissage(action) {
  const bouton = el("home-actions").querySelector(`.home-btn[data-action="${action}"]`);
  if (!bouton) return;

  bouton.style.setProperty("--fill-duration", `${GwsHomeScripts.AUTO_DELAY_MS}ms`);
  // Un reflow avant d'ajouter la classe, sinon la transition ne part pas quand
  // on rouvre un projet dans la même session (même piège que `playReveal`).
  void bouton.offsetWidth;
  bouton.classList.add("is-filling");

  homeTimers.auto = setTimeout(() => {
    homeTimers.auto = null;
    if (!el("view-home").hidden) quitterAccueil(action);
  }, GwsHomeScripts.AUTO_DELAY_MS);
}

/** Coupe le compte à rebours et vide la barre ; sans effet s'il n'y en a pas. */
function annulerRemplissage() {
  clearTimeout(homeTimers.auto);
  homeTimers.auto = null;
  for (const b of el("home-actions").querySelectorAll(".home-btn")) {
    b.classList.remove("is-filling");
  }
}

/**
 * Sortie de l'écran d'accueil vers la grille 2×2.
 *
 * Les trois actions mènent toutes à la grille : elles diffèrent seulement par
 * ce qui s'ouvre par-dessus une fois qu'elle est révélée.
 */
async function quitterAccueil(action) {
  annulerRemplissage();
  GwsTypewriter.cancel(el("home-script"));
  el("view-home").hidden = true;

  await enterWorkspace();

  if (action === "import") {
    if (state.points.length === 0) {
      toast("Ajoutez d'abord un point : une série est toujours rattachée à un point.", true);
      openPointDialog("create");
    } else {
      openImportDialog();
    }
  } else if (action === "create") {
    openPointDialog("create");
  }
}

// ---------------------------------------------------------------- agent IA

/**
 * Jeux de boutons rapides, selon ce qui est sélectionné.
 *
 * Le libellé n'est pas qu'un habillage : proposer « Comparer les points
 * sélectionnés » avec un seul point coché produirait une erreur côté Rust
 * (`demande_pour` refuse), donc l'UI n'offre que ce qui a un sens ici.
 */
function agentQuickActions(nbPoints) {
  if (nbPoints >= 2) {
    return [
      { kind: "compare", label: "Comparer les points sélectionnés" },
      { kind: "biases", label: "Biais possibles" },
      { kind: "project", label: "Résumer l'état du projet" },
    ];
  }
  if (nbPoints === 1) {
    return [
      { kind: "analyze", label: "Analyser cette série" },
      { kind: "biases", label: "Biais possibles" },
      { kind: "project", label: "Résumer l'état du projet" },
    ];
  }
  return [{ kind: "project", label: "Résumer l'état du projet" }];
}

function renderAgentActions() {
  const zone = el("agent-actions");
  clear(zone);

  const indisponible = state.agent && !state.agent.key.configured;
  for (const action of agentQuickActions(state.selectedIds.length)) {
    zone.appendChild(
      h("button", {
        type: "button",
        class: "btn btn-sm agent-action",
        text: action.label,
        disabled: indisponible || state.agentBusy,
        onclick: () => runAgent({ kind: action.kind }),
      })
    );
  }

  const badge = el("agent-badge");
  const n = state.selectedIds.length;
  badge.hidden = n === 0;
  badge.textContent = n === 1 ? "1 point" : `${n} points`;
}

/**
 * Affiche un état de travail. Le texte précédent est effacé : laisser l'ancienne
 * réponse sous un indicateur de chargement laisse croire qu'elle est à jour.
 */
function setAgentBusy(busy, message) {
  state.agentBusy = busy;
  el("agent-send").disabled = busy;
  el("block-agent").classList.toggle("is-busy", busy);

  if (busy) {
    hideError(el("agent-error"));
    el("agent-meta").hidden = true;
    GwsTypewriter.cancel(el("agent-text"));
    el("agent-text").hidden = true;
    el("agent-placeholder").hidden = false;
    el("agent-placeholder").textContent = message || "Analyse en cours…";
  }
  renderAgentActions();
}

/**
 * Un seul appel à la fois.
 *
 * Ce n'est pas du confort : le palier gratuit de Groq plafonne à 8 000 tokens
 * par minute (README § 6), soit six ou sept analyses. Laisser partir deux
 * appels concurrents épuise le quota et fait échouer le second avec une erreur
 * que l'utilisateur n'aurait pas provoquée lui-même.
 */
let agentSequence = 0;

async function runAgent(demande) {
  if (!state.project) return;

  const sequence = ++agentSequence;
  setAgentBusy(true, demande.kind === "question" ? "Question envoyée…" : "Analyse en cours…");

  try {
    const reponse = await invoke("agent_analyze", {
      payload: {
        project_id: state.project.id,
        point_ids: state.selectedIds,
        kind: demande.kind,
        question: demande.question || null,
      },
    });

    // Une réponse arrivée après qu'une autre demande soit partie est périmée :
    // l'afficher écraserait un résultat plus récent.
    if (sequence !== agentSequence) return;

    state.agentBusy = false;
    el("agent-send").disabled = false;
    el("block-agent").classList.remove("is-busy");
    el("agent-placeholder").hidden = true;
    el("agent-text").hidden = false;

    // Même composant que l'écran d'accueil, plus rapide : un paragraphe entier
    // à 26 ms par caractère se ferait attendre.
    //
    // `typeRich` et non `type` : le modèle répond en Markdown (puces, gras) même
    // quand on le lui déconseille. Écrit tel quel, ce balisage s'affichait
    // littéralement — des astérisques au milieu des phrases. Il est donc rendu
    // en DOM d'abord, animé ensuite (voir `markdown.js` et `typewriter.js`).
    GwsTypewriter.typeRich(el("agent-text"), GwsMarkdown.toDom(reponse.text), { speed: 9 });

    const meta = el("agent-meta");
    meta.hidden = false;
    meta.textContent = reponse.usage
      ? `${reponse.model} · ${reponse.usage.total_tokens} tokens`
      : reponse.model;

    renderAgentActions();
  } catch (e) {
    if (sequence !== agentSequence) return;
    state.agentBusy = false;
    el("agent-send").disabled = false;
    el("block-agent").classList.remove("is-busy");
    el("agent-placeholder").hidden = true;
    el("agent-text").hidden = true;
    // Les messages du backend sont déjà rédigés pour l'utilisateur (clé
    // refusée, quota de la minute, réponse vide) : on les affiche tels quels.
    showError(el("agent-error"), String(e));
    renderAgentActions();
  }
}

async function submitAgentQuestion(event) {
  event.preventDefault();
  const champ = el("agent-question");
  const question = champ.value.trim();
  if (!question) return;

  // La saisie est vidée tout de suite : la réponse arrive dans le bloc, et
  // relire sa propre question dans le champ n'apporte rien.
  champ.value = "";
  annulerAnalyseAuto();
  await runAgent({ kind: "question", question });
}

/**
 * Déclenchement automatique sur changement de sélection.
 *
 * Le debounce n'est pas cosmétique : cocher cinq points à la souris produit cinq
 * changements en deux secondes, soit cinq appels — de quoi épuiser le quota de
 * la minute avant même que l'utilisateur ait fini de choisir. On attend donc que
 * la sélection se stabilise.
 */
const AUTO_ANALYSE_DEBOUNCE_MS = 900;
let autoTimer = null;
/** Dernière sélection effectivement analysée, pour ne pas la refaire à l'identique. */
let derniereSelectionAnalysee = "";

function annulerAnalyseAuto() {
  clearTimeout(autoTimer);
  autoTimer = null;
}

function planifierAnalyseAuto() {
  annulerAnalyseAuto();

  const ids = state.selectedIds;
  if (ids.length < 2) {
    // En dessous de deux points, il n'y a rien à comparer : on n'appelle pas, et
    // on oublie la dernière signature pour qu'un retour à la même paire relance.
    derniereSelectionAnalysee = "";
    return;
  }
  if (state.agent && !state.agent.key.configured) return;

  // L'ordre de sélection ne change pas l'analyse : on compare des ensembles.
  const signature = [...ids].sort().join("|");
  if (signature === derniereSelectionAnalysee) return;

  autoTimer = setTimeout(() => {
    derniereSelectionAnalysee = signature;
    runAgent({ kind: "compare" });
  }, AUTO_ANALYSE_DEBOUNCE_MS);
}

function resetAgentBlock() {
  annulerAnalyseAuto();
  agentSequence++;
  derniereSelectionAnalysee = "";
  state.agentBusy = false;
  GwsTypewriter.cancel(el("agent-text"));
  el("agent-text").hidden = true;
  el("agent-meta").hidden = true;
  hideError(el("agent-error"));
  el("agent-placeholder").hidden = false;

  const indisponible = state.agent && !state.agent.key.configured;
  el("agent-placeholder").textContent = indisponible
    ? state.agent.key.problem ||
      `Aucune clé Groq : posez-la dans ${state.agent.key_file} pour activer l'agent.`
    : "Sélectionnez des points sur la carte, ou posez une question.";
  el("agent-question").disabled = indisponible;
  el("agent-send").disabled = indisponible;

  renderAgentActions();
}

function findPoint(id) {
  return state.points.find((p) => p.id === id);
}

// ------------------------------------------------- détail d'un point + stats

/**
 * Fiche d'une station : séries, statistiques, résolution de l'ordre jour/mois.
 *
 * En modale et non dans la grille : les quatre zones de la grille sont fixes et
 * attribuées, et cette fiche est un détail consulté ponctuellement, pas un
 * cinquième bloc permanent. Rien n'y a été retiré au passage — la confirmation
 * du format de date, notamment, reste accessible ici.
 */
function openDetail(pointId) {
  const dlg = el("dlg-detail");
  if (!dlg.open) dlg.showModal();
  renderDetail(pointId).catch((e) => toast(String(e), true));
}

/**
 * « Récupérer les données » du menu contextuel de la carte.
 *
 * Seule source d'appel direct existante : l'API Infoclimat, via le même
 * chemin que le bouton « Récupérer les dernières données » de la fiche
 * (`fetchInfoclimat`, avec son contrôle d'intervalle et sa confirmation de
 * doublon). La fiche est ouverte d'abord : c'est elle qui porte le bandeau
 * d'erreur. Sans station Infoclimat rattachée, la fiche explique quoi faire.
 */
async function fetchDataFromMap(pointId) {
  const point = findPoint(pointId);
  if (!point) return;
  const dlg = el("dlg-detail");
  if (!dlg.open) dlg.showModal();
  try {
    await renderDetail(pointId);
  } catch (e) {
    toast(String(e), true);
    return;
  }
  if (!point.infoclimat_station_id) {
    toast("Aucune source de récupération directe : station Infoclimat non rattachée.", true);
    return;
  }
  await fetchInfoclimat(point, false);
}

async function renderDetail(pointId) {
  state.detailPointId = pointId;
  const point = findPoint(pointId);
  if (!point) return;

  // Suit la station sur la carte, sans changer le zoom choisi par l'utilisateur.
  GwsMap.focus(pointId, state.points);

  el("detail-title").textContent = `${point.name} — ${point.category}${point.size ? " " + point.size : ""}`;

  const body = el("detail-body");
  clear(body);
  body.appendChild(h("p", { class: "empty", text: "Chargement…" }));

  const series = await invoke("list_series", { point_id: pointId });
  clear(body);

  body.appendChild(renderInfoclimatBar(point));

  if (series.length === 0) {
    body.appendChild(
      h("p", {
        class: "empty",
        text: "Aucune série sur ce point. Utilisez « Importer un fichier logger ».",
      })
    );
  } else {
    for (const s of series) {
      const block = h("div", { class: "series-block" });

      block.appendChild(
        h(
          "div",
          { class: "series-head" },
          h("span", {
            class: `chip ${s.role === "Primary" ? "chip-primary" : "chip-backup"}`,
            text: s.role === "Primary" ? "Principal" : "Secours",
          }),
          // Libellé lisible, jamais l'identifiant brut du modèle : « DraginoApi »
          // ne veut rien dire pour l'utilisateur, et prête justement à confusion
          // avec la Moucherolle, qui arrive par l'API Infoclimat.
          h("span", { class: "chip", text: sourceLabel(s.source_type) }),
          h("strong", {
            text: s.logger_serial ? `Logger ${s.logger_serial}` : "Logger sans numéro de série",
          }),
          h("span", {
            class: "muted",
            text: `· ${fmtNumber(s.measurement_count)} mesures · importé le ${fmtLocal(s.imported_at)}`,
          }),
          s.raw_file_reference ? h("span", { class: "muted", text: `· ${s.raw_file_reference}` }) : null,
          h("span", {
            class: "muted",
            text: `· décalage horloge ${s.clock_offset_seconds >= 0 ? "+" : ""}${s.clock_offset_seconds} s`,
          }),
          // Une source déjà en UTC n'a aucun ordre jour/mois à afficher : le
          // signaler serait suggérer un doute là où il n'y en a pas.
          isUtcNative(s)
            ? h("span", {
                class: "chip",
                title:
                  "Cette source horodate en UTC absolu (dh_utc) : ni fuseau à corriger, " +
                  "ni ambiguïté jour/mois possible.",
                text: "Dates : UTC d'origine",
              })
            : h("span", {
                class: "chip",
                title: "Ordre jour/mois appliqué à la lecture des dates de cette série",
                text: `Dates : ${s.date_order === "DayFirst" ? "jj/mm" : "mm/jj"}`,
              }),
          s.station_id ? h("span", { class: "chip", text: s.station_id }) : null,
          // Recalibrer et inverser n'ont de sens que sur un logger de terrain.
          // Les proposer sur une série d'API n'offrirait qu'une façon de corrompre
          // des horodatages qui sont justes par construction.
          isUtcNative(s)
            ? null
            : h("button", {
                class: "btn btn-sm",
                type: "button",
                text: "Recalibrer l'horloge",
                onclick: () => recalibrate(s),
              }),
          // Hors doute, l'inversion reste possible mais devient une action à risque :
          // elle réécrit les horodatages bruts d'une série réputée correcte.
          isUtcNative(s) || s.date_format_uncertain
            ? null
            : h("button", {
                class: "btn btn-sm",
                type: "button",
                text: "Inverser jour/mois",
                onclick: () => resolveDateOrder(s, true),
              })
        )
      );

      if (s.date_format_uncertain) {
        block.appendChild(renderDateWarning(s));
      }

      const stats = await invoke("series_stats", { series_id: s.id });
      block.appendChild(renderStats(stats));
      block.appendChild(
        h(
          "div",
          { class: "series-data-actions" },
          h("button", {
            class: "btn btn-sm",
            type: "button",
            text: "Voir et corriger les données",
            onclick: (ev) => basculerDonneesSerie(s, block, ev.currentTarget),
          }),
          h("button", {
            class: "btn btn-sm btn-danger-ghost",
            type: "button",
            text: "Supprimer la série",
            onclick: () => supprimerSerie(s, point),
          })
        )
      );

      // Partie D : l'attribution accompagne la donnée, là où elle est lue.
      if (s.data_license || s.source_type === "InfoclimatApi") {
        block.appendChild(renderAttribution(s));
      }

      body.appendChild(block);
    }
  }

  // Bouton d'action en bas de fiche si la station est rattachée à Infoclimat
  if (point.infoclimat_station_id) {
    body.appendChild(
      h(
        "div",
        { class: "detail-bottom-actions" },
        h("button", {
          class: "btn btn-secondary",
          type: "button",
          id: "btn-open-ic-history",
          text: "Récupération d'anciennes données",
          onclick: () => openInfoclimatHistoryModal(point),
        })
      )
    );
  }
}

// ------------------------------------------- données d'une série (voir / corriger / supprimer)

/** Rafraîchit ce qui peut montrer des données après une annulation. */
async function rafraichirApresAnnulation() {
  if (state.project) {
    await loadPoints();
    if (state.detailPointId && el("dlg-detail").open) await renderDetail(state.detailPointId);
  } else if (!el("view-projects").hidden) {
    await loadProjects();
  }
  if (el("dlg-settings").open && typeof renderProjetsGestion === "function") await renderProjetsGestion();
}

async function annulerOuRetablir(sens) {
  try {
    const libelle = await invoke(sens === "undo" ? "undo_last" : "redo_last");
    if (!libelle) {
      toast(sens === "undo" ? "Rien à annuler." : "Rien à rétablir.");
      return;
    }
    toast(`${sens === "undo" ? "Annulé" : "Rétabli"} : ${libelle}.`);
    await rafraichirApresAnnulation();
  } catch (e) {
    toast(String(e), true);
  }
}

async function supprimerSerie(s, point) {
  const ok = await confirm(
    `Supprimer cette série de « ${point.name} » (${fmtNumber(s.measurement_count)} mesures) ?\n\n` +
      "Ctrl+Z annule la suppression tant que l'application reste ouverte."
  );
  if (!ok) return;
  try {
    await invoke("delete_series", { series_id: s.id });
    toast("Série supprimée. Ctrl+Z pour annuler.");
    await loadPoints();
    await renderDetail(point.id);
  } catch (e) {
    toast(String(e), true);
  }
}

const PAGE_DONNEES = 100;

/** Affiche ou masque le tableau des mesures d'une série, sous son bloc. */
function basculerDonneesSerie(s, block, bouton) {
  const existant = block.querySelector(".series-data");
  if (existant) {
    existant.remove();
    bouton.textContent = "Voir et corriger les données";
    return;
  }
  bouton.textContent = "Masquer les données";
  const zone = h("div", { class: "series-data" });
  block.appendChild(zone);
  remplirDonneesSerie(s, zone, 0);
}

/** `datetime-local` (heure locale) → secondes UTC. */
function secondesDepuisChamp(v) {
  const t = new Date(v).getTime();
  return Number.isFinite(t) ? Math.floor(t / 1000) : null;
}

async function remplirDonneesSerie(s, zone, page) {
  clear(zone);
  zone.appendChild(h("p", { class: "empty", text: "Chargement…" }));
  let mesures, trous;
  try {
    [mesures, trous] = await Promise.all([
      invoke("series_measurements_page", { series_id: s.id, offset: page * PAGE_DONNEES, limit: PAGE_DONNEES }),
      page === 0 ? invoke("series_gaps", { series_id: s.id }) : Promise.resolve(null),
    ]);
  } catch (e) {
    clear(zone);
    zone.appendChild(h("p", { class: "error", text: String(e) }));
    return;
  }
  clear(zone);
  const total = s.measurement_count || 0;
  const pages = Math.max(1, Math.ceil(total / PAGE_DONNEES));
  const recharger = async () => {
    await loadPoints();
    const point = findPoint(s.point_id);
    if (point) {
      const series = await invoke("list_series", { point_id: s.point_id });
      const maj = series.find((x) => x.id === s.id);
      if (maj) s.measurement_count = maj.measurement_count;
    }
    await remplirDonneesSerie(s, zone, page);
  };

  // Trous
  if (trous && trous.length) {
    const fmt = (sec) => fmtLocal(new Date(sec * 1000).toISOString());
    const dureeH = (a, b) => Math.round((b - a) / 3600);
    zone.appendChild(
      h(
        "details",
        { class: "series-gaps" },
        h("summary", { text: `${trous.length} trou(s) dans la série — le plus long : ${dureeH(...trous[0])} h` }),
        h("ul", null, trous.map(([a, b]) => h("li", { text: `${fmt(a)} → ${fmt(b)} (${dureeH(a, b)} h sans mesure)` })))
      )
    );
  } else if (trous) {
    zone.appendChild(h("p", { class: "muted", text: "Aucun trou notable dans la série." }));
  }

  // Suppression d'une plage
  const de = h("input", { type: "datetime-local", "aria-label": "Début de la plage" });
  const a = h("input", { type: "datetime-local", "aria-label": "Fin de la plage" });
  zone.appendChild(
    h(
      "div",
      { class: "series-range" },
      h("span", { text: "Supprimer une plage : du" }),
      de,
      h("span", { text: "au" }),
      a,
      h("button", {
        class: "btn btn-sm btn-danger-ghost",
        type: "button",
        text: "Supprimer la plage",
        onclick: async () => {
          const debut = secondesDepuisChamp(de.value);
          const fin = secondesDepuisChamp(a.value);
          if (debut === null || fin === null) {
            toast("Indiquez le début et la fin de la plage.", true);
            return;
          }
          try {
            const n = await invoke("delete_measurements_range", { series_id: s.id, from_utc: debut, to_utc: fin });
            toast(n ? `${fmtNumber(n)} mesure(s) supprimée(s). Ctrl+Z pour annuler.` : "Aucune mesure dans cette plage.");
            if (n) await recharger();
          } catch (e) {
            toast(String(e), true);
          }
        },
      })
    )
  );

  // Tableau
  const choisies = new Set();
  const btnSupprSel = h("button", {
    class: "btn btn-sm btn-danger-ghost",
    type: "button",
    text: "Supprimer la sélection",
    disabled: true,
    onclick: async () => {
      try {
        const n = await invoke("delete_measurements", { series_id: s.id, ids: [...choisies] });
        toast(`${fmtNumber(n)} mesure(s) supprimée(s). Ctrl+Z pour annuler.`);
        await recharger();
      } catch (e) {
        toast(String(e), true);
      }
    },
  });
  const majSel = () => {
    btnSupprSel.disabled = choisies.size === 0;
    btnSupprSel.textContent = choisies.size ? `Supprimer la sélection (${choisies.size})` : "Supprimer la sélection";
  };
  const opt = (v, d = 1) => (v === null || v === undefined ? "—" : Number(v).toFixed(d).replace(".", ","));
  const lignes = mesures.map((m) => {
    const coche = h("input", {
      type: "checkbox",
      "aria-label": "Sélectionner cette mesure",
      onchange: (ev) => {
        if (ev.target.checked) choisies.add(m.id);
        else choisies.delete(m.id);
        majSel();
      },
    });
    const temp = h("input", {
      type: "number",
      step: "0.1",
      value: String(m.temperature_celsius),
      class: "cell-edit",
      "aria-label": "Température (°C)",
      onchange: async (ev) => {
        const v = Number(String(ev.target.value).replace(",", "."));
        if (!Number.isFinite(v)) {
          ev.target.value = String(m.temperature_celsius);
          return;
        }
        try {
          await invoke("set_measurement_temperature", { series_id: s.id, id: m.id, celsius: v });
          m.temperature_celsius = v;
          toast("Valeur corrigée. Ctrl+Z pour annuler.");
        } catch (e) {
          ev.target.value = String(m.temperature_celsius);
          toast(String(e), true);
        }
      },
    });
    return h(
      "tr",
      null,
      h("td", null, coche),
      h("td", { text: fmtLocal(m.timestamp_utc) }),
      h("td", null, temp),
      h("td", { text: opt(m.humidity_percent, 0) }),
      h("td", { text: opt(m.pressure_hpa) }),
      h("td", { text: opt(m.wind_avg_kmh, 0) }),
      h("td", { text: opt(m.rain_mm) })
    );
  });
  zone.appendChild(
    h(
      "div",
      { class: "series-table-wrap" },
      h(
        "table",
        { class: "series-table" },
        h(
          "thead",
          null,
          h(
            "tr",
            null,
            ["", "Date (heure locale)", "Temp. °C", "Hum. %", "Pression", "Vent km/h", "Pluie mm"].map((t) => h("th", { text: t }))
          )
        ),
        h("tbody", null, lignes)
      )
    )
  );

  // Pagination + suppression de la sélection
  zone.appendChild(
    h(
      "div",
      { class: "series-pager" },
      btnSupprSel,
      h("span", { class: "muted", text: `Page ${page + 1} / ${pages} · plus récentes d'abord` }),
      h("button", {
        class: "btn btn-sm",
        type: "button",
        text: "← Plus récentes",
        disabled: page === 0,
        onclick: () => remplirDonneesSerie(s, zone, page - 1),
      }),
      h("button", {
        class: "btn btn-sm",
        type: "button",
        text: "Plus anciennes →",
        disabled: page + 1 >= pages,
        onclick: () => remplirDonneesSerie(s, zone, page + 1),
      })
    )
  );
}

// ----------------------------------------------------- panneau « Imports » du projet

let importsListe = [];

async function ouvrirImports() {
  if (!state.project) return;
  const dlg = el("dlg-imports");
  el("imports-recherche").value = "";
  clear(el("imports-liste"));
  el("imports-liste").appendChild(h("p", { class: "empty", text: "Chargement…" }));
  if (!dlg.open) dlg.showModal();
  await rechargerImports();
}

async function rechargerImports() {
  try {
    importsListe = await invoke("list_project_imports", { project_id: state.project.id });
  } catch (e) {
    clear(el("imports-liste"));
    el("imports-liste").appendChild(h("p", { class: "error", text: String(e) }));
    return;
  }
  afficherImports();
}

/** État lisible d'un import, d'après l'âge de sa dernière mesure. */
function etatImport(l) {
  if (!l.last_utc) return { texte: "Vide", classe: "chip-warn" };
  const jours = (Date.now() - new Date(l.last_utc).getTime()) / 86400000;
  if (l.refreshable) {
    if (jours <= 1) return { texte: "À jour", classe: "chip-ok" };
    return { texte: `En retard de ${Math.floor(jours)} j`, classe: "chip-warn" };
  }
  return { texte: "Fichier", classe: "" };
}

function afficherImports() {
  const zone = el("imports-liste");
  clear(zone);
  const q = el("imports-recherche").value.trim().toLowerCase();
  const lignes = importsListe.filter((l) =>
    !q ||
    [l.point_name, sourceLabel(l.series.source_type), l.series.raw_file_reference || "", l.series.logger_serial || ""]
      .join(" ")
      .toLowerCase()
      .includes(q)
  );
  el("imports-compte").textContent = `${fmtNumber(lignes.length)} import(s)`;
  if (!lignes.length) {
    zone.appendChild(h("p", { class: "empty", text: importsListe.length ? "Aucun import ne correspond." : "Aucun import dans ce projet." }));
    return;
  }
  // Au-delà de 300 lignes (projet France), on n'affiche que le début : le
  // filtre permet de retrouver le reste.
  const visibles = lignes.slice(0, 300);
  const corps = visibles.map((l) => {
    const s = l.series;
    const etat = etatImport(l);
    const point = findPoint(s.point_id);
    return h(
      "tr",
      null,
      h("td", null, h("strong", { text: l.point_name })),
      h("td", null, h("span", { class: "chip", text: sourceLabel(s.source_type) })),
      h("td", { class: "muted", title: s.raw_file_reference || "", text: s.raw_file_reference || (s.logger_serial ? `Logger ${s.logger_serial}` : "—") }),
      h("td", { text: fmtLocal(s.imported_at) }),
      h("td", { text: l.first_utc ? fmtLocal(l.first_utc) : "—" }),
      h("td", { text: l.last_utc ? fmtLocal(l.last_utc) : "—" }),
      h("td", { text: s.last_api_fetch_at ? fmtLocal(s.last_api_fetch_at) : "—" }),
      h("td", { class: "num", text: fmtNumber(s.measurement_count) }),
      h("td", null, h("span", { class: `chip ${etat.classe}`, text: etat.texte })),
      h(
        "td",
        { class: "imports-actions" },
        h("button", {
          class: "btn btn-sm",
          type: "button",
          text: "Détail",
          title: "Ouvrir la fiche du point (données, corrections)",
          onclick: () => {
            el("dlg-imports").close();
            openDetail(s.point_id);
          },
        }),
        l.refreshable && point
          ? h("button", {
              class: "btn btn-sm",
              type: "button",
              text: "Relancer",
              title: "Récupérer les dernières données depuis l'API",
              onclick: async (ev) => {
                ev.currentTarget.disabled = true;
                await fetchInfoclimat(point, false);
                await rechargerImports();
              },
            })
          : null,
        h("button", {
          class: "btn btn-sm btn-danger-ghost",
          type: "button",
          text: "Supprimer",
          onclick: async () => {
            await supprimerSerie(s, point || { name: l.point_name, id: s.point_id });
            await rechargerImports();
          },
        })
      )
    );
  });
  zone.appendChild(
    h(
      "table",
      { class: "imports-table" },
      h(
        "thead",
        null,
        h(
          "tr",
          null,
          ["Point", "Source", "Fichier", "Importé le", "Anciennes données", "Dernières données", "Dernier appel API", "Mesures", "État", ""].map((t) =>
            h("th", { text: t })
          )
        )
      ),
      h("tbody", null, corps)
    )
  );
  if (lignes.length > visibles.length) {
    zone.appendChild(h("p", { class: "muted", text: `${fmtNumber(lignes.length - visibles.length)} autre(s) import(s) : affinez le filtre.` }));
  }
}

// ------------------------------------------- quota Infoclimat partagé (bandeau des Paramètres)

/** Huit couleurs d'appareil ; l'index est celui enregistré côté Rust. */
const COULEURS_APPAREIL = ["#1a73e8", "#e8711c", "#2fae6b", "#c62828", "#7b5cd6", "#0f9d9a", "#d4a514", "#d0488f"];

async function chargerAppareil() {
  let a;
  try {
    a = await invoke("appareil_identite");
  } catch (e) {
    console.warn("Identité de l'appareil indisponible :", e);
    return;
  }
  const nom = el("setting-appareil-nom");
  nom.value = a.nom;
  nom.onchange = () => enregistrerAppareil(nom.value, a.couleur).then((n) => n && (a = n));
  const zone = el("setting-appareil-couleurs");
  clear(zone);
  COULEURS_APPAREIL.forEach((c, i) => {
    zone.appendChild(
      h("button", {
        type: "button",
        class: "appareil-couleur",
        role: "radio",
        "aria-checked": String(i === a.couleur),
        "aria-label": `Couleur ${i + 1}`,
        style: `background:${c}`,
        onclick: async () => {
          const n = await enregistrerAppareil(nom.value, i);
          if (n) {
            a = n;
            for (const b of zone.children) b.setAttribute("aria-checked", String(b === zone.children[i]));
          }
        },
      })
    );
  });
}

async function enregistrerAppareil(nom, couleur) {
  try {
    const a = await invoke("appareil_changer", { nom, couleur });
    afficherQuotaPartage(true);
    return a;
  } catch (e) {
    showError(el("setting-error"), String(e));
    return null;
  }
}

/**
 * « réinitialisation dans 2 h 15 min » : temps restant avant minuit, heure de
 * Paris (remise à zéro du quota Infoclimat), quel que soit le fuseau du PC.
 */
function avantRemiseQuota(maintenant = new Date()) {
  const p = Object.fromEntries(new Intl.DateTimeFormat("en-GB", {
    timeZone: "Europe/Paris", hour: "numeric", minute: "numeric", second: "numeric", hourCycle: "h23",
  }).formatToParts(maintenant).map((x) => [x.type, Number(x.value)]));
  const min = Math.max(1, Math.ceil((86400 - (p.hour * 3600 + p.minute * 60 + p.second)) / 60));
  const hh = Math.floor(min / 60);
  return `réinitialisation dans ${hh ? `${hh} h ` : ""}${hh ? String(min % 60).padStart(2, "0") : min} min`;
}

/**
 * Bandeau du quota partagé : noir = automatique (serveur), gris = manuel
 * (administrateur), une couleur par appareil. Rien si le relai est éteint.
 */
async function afficherQuotaPartage(forcer) {
  const bandeau = el("quota-bandeau");
  let q;
  try {
    q = await invoke("quota_partage", { forcer });
  } catch (e) {
    bandeau.hidden = false;
    clear(el("quota-barre"));
    el("quota-legende").textContent = "Quota partagé illisible (serveur relai injoignable).";
    el("quota-legende").title = String(e);
    return;
  }
  if (!q) {
    bandeau.hidden = true;
    return;
  }
  bandeau.hidden = false;
  const barre = el("quota-barre");
  clear(barre);
  const pct = (n) => `${Math.min(100, (n / q.plafond) * 100)}%`;
  const segment = (n, couleur, titre) => {
    if (n <= 0) return;
    barre.appendChild(h("span", { class: "quota-seg", style: `width:${pct(n)};background:${couleur}`, title: titre }));
  };
  segment(q.automatiques, "#202124", `Automatiques (serveur) : ${fmtNumber(q.automatiques)}`);
  segment(q.manuelles, "#9aa0a6", `Manuelles (administrateur) : ${fmtNumber(q.manuelles)}`);
  for (const a of q.appareils) {
    segment(a.requetes, COULEURS_APPAREIL[a.couleur] || "#1a73e8", `${a.nom}${a.moi ? " (cet appareil)" : ""} : ${fmtNumber(a.requetes)}`);
  }
  barre.setAttribute("aria-label", `Quota Infoclimat : ${q.total} requêtes sur ${q.plafond} aujourd'hui`);
  el("quota-legende").textContent = `${fmtNumber(q.total)} / ${fmtNumber(q.plafond)} requêtes Infoclimat aujourd'hui · ${avantRemiseQuota()}`;
  el("quota-legende").title = `Remise à zéro à minuit (heure de Paris). Lu à ${fmtLocal(q.lu_le)}.`;
}

/**
 * Modale de récupération par plage personnalisée (historique Infoclimat).
 */
let icHistoryUnlisten = null;

async function openInfoclimatHistoryModal(point) {
  const dlg = el("dlg-infoclimat-history");
  if (!dlg) return;

  const startInput = el("ic-history-start");
  const endInput = el("ic-history-end");
  const chunksSpan = el("ic-history-chunks-count");
  const timeSpan = el("ic-history-estimated-time");
  const quotaSpan = el("ic-history-quota-info");
  const errorP = el("ic-history-error");
  const progressBox = el("ic-history-progress-box");
  const progressBar = el("ic-history-progress-bar");
  const progressPct = el("ic-history-progress-pct");
  const progressLabel = el("ic-history-progress-label");
  const progressDetail = el("ic-history-progress-detail");
  const btnStart = el("btn-ic-history-start");
  const btnClose = el("btn-ic-history-close");
  const btnCloseX = el("btn-ic-history-close-x");

  // Réinitialisation de l'affichage
  hideError(errorP);
  progressBox.hidden = true;
  progressBar.style.width = "0%";
  progressPct.textContent = "0%";
  btnStart.disabled = false;
  btnStart.textContent = "C'est parti !";
  btnClose.disabled = false;
  startInput.disabled = false;
  endInput.disabled = false;

  el("ic-history-title").textContent = `Récupération d'anciennes données — ${point.name}`;
  el("ic-history-station-info").textContent =
    `Station Infoclimat : ${point.infoclimat_station_id}. ` +
    `L'API Open Data d'Infoclimat est interrogée par tranches de 7 jours, avec un délai de sécurité ` +
    `de 1,01 s entre chaque requête pour respecter strictement les règles d'usage de l'API.`;

  // Dates par défaut : fin aujourd'hui, début 1 an auparavant
  const now = new Date();
  const todayStr = now.toISOString().slice(0, 10);
  const defaultStart = new Date(now.getTime() - 365 * 24 * 60 * 60 * 1000);
  const defaultStartStr = defaultStart.toISOString().slice(0, 10);

  endInput.value = todayStr;
  endInput.max = todayStr;
  startInput.value = defaultStartStr;
  startInput.max = todayStr;

  let currentQuota = null;

  async function refreshQuota() {
    try {
      currentQuota = await invoke("get_infoclimat_quota");
      quotaSpan.textContent = `${fmtNumber(currentQuota.requests_today)} / ${fmtNumber(currentQuota.max_daily_requests)} requêtes utilisées aujourd'hui (${currentQuota.date}, fuseau ${currentQuota.timezone_offset}) · ${avantRemiseQuota()}`;
    } catch (e) {
      quotaSpan.textContent = "Information de quota indisponible";
    }
  }

  function updateEstimates() {
    hideError(errorP);
    const startStr = startInput.value;
    const endStr = endInput.value;

    if (!startStr || !endStr) {
      chunksSpan.textContent = "—";
      timeSpan.textContent = "—";
      btnStart.disabled = true;
      return;
    }

    const dStart = new Date(startStr + "T00:00:00");
    const dEnd = new Date(endStr + "T00:00:00");

    if (dStart > dEnd) {
      showError(errorP, "La date de début doit être antérieure ou égale à la date de fin.");
      chunksSpan.textContent = "—";
      timeSpan.textContent = "—";
      btnStart.disabled = true;
      return;
    }

    const diffDays = Math.round((dEnd - dStart) / (1000 * 60 * 60 * 24)) + 1;
    const chunks = Math.ceil(diffDays / 7);

    chunksSpan.textContent = `${chunks} tranche(s) de 7 jours (${fmtNumber(diffDays)} jours au total)`;

    const totalSeconds = chunks * 1.01;
    let timeFormatted = "";
    if (totalSeconds < 60) {
      timeFormatted = `~${Math.ceil(totalSeconds)} s`;
    } else {
      const mins = Math.floor(totalSeconds / 60);
      const secs = Math.ceil(totalSeconds % 60);
      timeFormatted = `~${mins} min ${secs.toString().padStart(2, "0")} s`;
    }
    timeSpan.textContent = `${timeFormatted} (cadence : 1 requêtte / 1,01 s)`;

    if (currentQuota && currentQuota.requests_today + chunks > currentQuota.max_daily_requests) {
      showError(
        errorP,
        `Attention : cette opération (${chunks} requêtes) risque de dépasser la limite recommandée de 5 000 requêtes / jour.`
      );
    }

    btnStart.disabled = false;
  }

  startInput.oninput = updateEstimates;
  endInput.oninput = updateEstimates;

  const closeDialog = () => {
    if (icHistoryUnlisten) {
      icHistoryUnlisten();
      icHistoryUnlisten = null;
    }
    dlg.close();
  };

  btnClose.onclick = closeDialog;
  btnCloseX.onclick = closeDialog;

  btnStart.onclick = async () => {
    const startStr = startInput.value;
    const endStr = endInput.value;
    if (!startStr || !endStr) return;

    btnStart.disabled = true;
    btnClose.disabled = true;
    startInput.disabled = true;
    endInput.disabled = true;
    btnStart.textContent = "Récupération en cours…";
    hideError(errorP);

    progressBox.hidden = false;
    progressBar.style.width = "0%";
    progressPct.textContent = "0%";
    progressLabel.textContent = "Démarrage de la récupération…";
    progressDetail.textContent = "Préparation des tranches de 7 jours…";

    if (icHistoryUnlisten) {
      icHistoryUnlisten();
      icHistoryUnlisten = null;
    }

    const evenements = window.__TAURI__ && window.__TAURI__.event;
    if (evenements) {
      icHistoryUnlisten = await evenements.listen("infoclimat-fetch-progress", ({ payload }) => {
        if (payload.station_id === point.infoclimat_station_id) {
          const pct = Math.round((payload.current / payload.total) * 100);
          progressBar.style.width = `${pct}%`;
          progressPct.textContent = `${pct}%`;
          progressLabel.textContent = `Tranche ${payload.current} / ${payload.total} en cours…`;
          progressDetail.textContent = `Période interrogée : du ${payload.start} au ${payload.end}`;
        }
      });
    }

    try {
      const r = await avecConfirmationDeReimport((forceDoublon) =>
        invoke("fetch_infoclimat", {
          payload: {
            point_id: point.id,
            station_id: point.infoclimat_station_id,
            start_date: startStr,
            end_date: endStr,
            force: true,
            force_duplicate: forceDoublon,
          },
        })
      );

      if (!r) {
        progressLabel.textContent = "Récupération annulée";
        progressDetail.textContent = "Ces données sont déjà enregistrées sur ce point.";
        toast("Récupération annulée : données déjà enregistrées.");
        return;
      }

      progressBar.style.width = "100%";
      progressPct.textContent = "100%";
      texteAvecIcone(progressLabel, "ok", "Récupération terminée");
      progressDetail.textContent =
        `${fmtNumber(r.inserted)} nouvelle(s) mesure(s) (${fmtNumber(r.unchanged)} déjà connues, ` +
        `${fmtNumber(r.updated)} mises à jour) — ${r.window_count} tranche(s) du ${r.requested_start} au ${r.requested_end}.`;

      toast(
        `${fmtNumber(r.inserted)} nouvelle(s) mesure(s) ajoutée(s) sur « ${r.point_name} » !`
      );

      await refreshQuota();
      await loadPoints();
      if (state.detailPointId === point.id) {
        await renderDetail(point.id);
      }
    } catch (e) {
      const msg = String(e);
      showError(errorP, msg);
      texteAvecIcone(progressLabel, "croix", "Échec de la récupération");
      progressDetail.textContent = "Une erreur est survenue lors de l'appel à l'API.";
      toast("Échec de la récupération historique : " + msg, true);
    } finally {
      if (icHistoryUnlisten) {
        icHistoryUnlisten();
        icHistoryUnlisten = null;
      }
      btnStart.disabled = false;
      btnStart.textContent = "Relancer un import";
      btnClose.disabled = false;
      startInput.disabled = false;
      endInput.disabled = false;
    }
  };

  await refreshQuota();
  updateEstimates();
  dlg.showModal();
}

/** Libellés des types de source — le Rust ne les expose pas encore autrement. */
const LIBELLES_SOURCE = {
  ElitechImport: "Fichier Elitech",
  ElitechDirect: "Elitech (USB direct)",
  InfoclimatApi: "API Infoclimat",
  DraginoApi: "Orange Live Objects (réservé)",
  RommaImport: "Fichier ROMMA (archives)",
  Manual: "Fichier à colonnes mappées",
  ManualEntry: "Saisie manuelle sur site",
};

function sourceLabel(sourceType) {
  return LIBELLES_SOURCE[sourceType] || sourceType;
}

/** `true` si la série vient d'une source horodatée en UTC absolu à l'origine. */
function isUtcNative(s) {
  // ROMMA en fait partie : sa colonne `timestamp` est un epoch Unix, donc de
  // l'UTC absolu au même titre que le `dh_utc` d'Infoclimat. Un export Elitech,
  // lui, n'en est jamais — même au format xlsx, où l'horodatage est en ISO mais
  // reste l'heure naïve de l'horloge du logger.
  return (
    s.source_type === "InfoclimatApi" ||
    s.source_type === "DraginoApi" ||
    s.source_type === "RommaImport"
  );
}

/**
 * Mention d'attribution en pied de série.
 *
 * La Moucherolle est diffusée en CC BY-NC : l'attribution n'est pas une
 * politesse, c'est la condition de la licence. Elle doit donc apparaître partout
 * où la donnée se lit — ici pour la consultation, et dans l'export HTML quand
 * celui-ci sera écrit (chantier séparé).
 */
function renderAttribution(s) {
  const mention =
    (state.infoclimat && state.infoclimat.attribution) ||
    "Données Infoclimat (infoclimat.fr)";
  return h(
    "p",
    { class: "attribution" },
    h("span", { text: mention }),
    s.data_license ? h("span", { class: "muted", text: ` · licence : ${s.data_license}` }) : null
  );
}

/**
 * Bandeau de tête de fiche : rattachement Infoclimat et bouton de récupération.
 *
 * Sans identifiant de station renseigné, on explique où le mettre plutôt que de
 * masquer le bouton sans un mot — c'est le genre de fonction qu'on cherche
 * ensuite pendant dix minutes.
 */
function renderInfoclimatBar(point) {
  const station = point.infoclimat_station_id;

  if (!station) {
    return h(
      "p",
      { class: "hint ic-bar" },
      "Aucune station Infoclimat rattachée. Renseignez son identifiant " +
        "(par exemple STATIC0479) dans « Modifier » pour pouvoir récupérer ses " +
        "données directement depuis l'API."
    );
  }

  return h(
    "div",
    { class: "ic-bar" },
    h("span", { class: "chip chip-primary", text: "Infoclimat" }),
    h("span", { class: "mono", text: station }),
    h("button", {
      class: "btn btn-sm btn-primary",
      type: "button",
      id: "btn-fetch-ic",
      text: "Récupérer les dernières données",
      onclick: () => fetchInfoclimat(point, false),
    }),
    h("span", {
      class: "hint",
      text: state.infoclimat
        ? `Un appel au plus toutes les ${state.infoclimat.refresh_min_minutes} min (modifiable dans Paramètres).`
        : "",
    })
  );
}

/**
 * Mode « appel direct » : interroge l'API et complète la série.
 *
 * Le refus pour cause d'intervalle non écoulé n'est pas une erreur : on le
 * transforme en question, parce que l'utilisateur qui vient de cliquer sait
 * mieux que le réglage s'il a une raison d'insister.
 */
async function fetchInfoclimat(point, force) {
  const bouton = el("btn-fetch-ic");
  if (bouton) {
    bouton.disabled = true;
    bouton.textContent = "Appel en cours…";
  }

  try {
    // Le cas « déjà récupéré » ne peut se présenter qu'au tout premier appel sur
    // une station : ensuite la série existe et on la complète, ce qui ne crée
    // aucun doublon. Il reste possible si un JSON de la même période a été
    // importé à la main auparavant.
    const r = await avecConfirmationDeReimport((forceDoublon) =>
      invoke("fetch_infoclimat", {
        payload: {
          point_id: point.id,
          station_id: point.infoclimat_station_id,
          force,
          force_duplicate: forceDoublon,
        },
      })
    );

    if (!r) {
      toast("Récupération annulée : ces données sont déjà enregistrées sur ce point.");
      return;
    }

    toast(
      `${fmtNumber(r.inserted)} nouvelle(s) mesure(s) sur « ${r.point_name} » ` +
        `(${fmtNumber(r.unchanged)} déjà connue(s), ${fmtNumber(r.updated)} corrigée(s)) — ` +
        `${r.window_count} appel(s) du ${r.requested_start} au ${r.requested_end}.`
    );
    await loadPoints();
  } catch (e) {
    const message = String(e);

    // Cas « trop tôt » : proposer de forcer plutôt que d'imposer d'aller
    // changer un réglage global pour un besoin ponctuel.
    if (!force && message.includes("intervalle minimal")) {
      if (await window.confirm(`${message}\n\nLancer quand même l'appel maintenant ?`)) {
        await fetchInfoclimat(point, true);
        return;
      }
    } else {
      // Le message d'erreur d'authentification tient plusieurs paragraphes :
      // un toast le tronquerait, or c'est justement celui qu'il faut lire en
      // entier. On l'affiche en bandeau dans la fiche.
      showFetchError(message, point);
    }
  } finally {
    if (bouton) {
      bouton.disabled = false;
      bouton.textContent = "Récupérer les dernières données";
    }
  }
}

function showFetchError(message, point) {
  const body = el("detail-body");
  const existant = body.querySelector(".ic-error");
  if (existant) existant.remove();

  const isSshError = message.toLowerCase().includes("ssh") || (state.sshRelay && state.sshRelay.enabled);

  const children = [
    h("strong", { text: isSshError ? "Échec de récupération (Relai SSH)" : "Récupération impossible" }),
    // `white-space: pre-line` (styles.css) : le message d'IP est structuré en
    // paragraphes, les aplatir le rendrait illisible.
    h("p", { class: "pre", text: message }),
  ];

  if (isSshError) {
    children.push(
      h(
        "div",
        { class: "banner-actions" },
        h("button", {
          class: "btn btn-sm btn-primary",
          type: "button",
          text: "Désactiver le relai SSH et réessayer en direct",
          onclick: async () => {
            try {
              state.sshRelay = await invoke("set_ssh_relay_enabled", { enabled: false });
              toast("Relai SSH désactivé — nouvel essai en appel direct…");
              const errBanner = body.querySelector(".ic-error");
              if (errBanner) errBanner.remove();
              if (point) {
                await fetchInfoclimat(point, false);
              }
            } catch (err) {
              toast("Erreur lors de la désactivation du relai : " + String(err), true);
            }
          },
        })
      )
    );
  }

  const bandeau = h(
    "div",
    { class: "banner banner-warn ic-error" },
    ...children
  );
  body.insertBefore(bandeau, body.firstChild);
  toast("Récupération impossible — voir le détail dans la fiche.", true);
}

/**
 * Bandeau affiché sur une série dont l'ordre jour/mois n'a pas pu être prouvé.
 *
 * C'est le seul endroit de l'app où une donnée peut être fausse sans que rien
 * ne plante : le bandeau doit donc être explicite sur la conséquence (un décalage
 * de plusieurs mois), pas seulement signaler « attention ».
 */
function renderDateWarning(s) {
  const lu = s.date_order === "DayFirst" ? "jour d'abord (jj/mm)" : "mois d'abord (mm/jj)";
  const inverse = s.date_order === "DayFirst" ? "mois d'abord (mm/jj)" : "jour d'abord (jj/mm)";

  return h(
    "div",
    { class: "banner banner-warn" },
    h("strong", { text: "Format de date à confirmer" }),
    h("p", {
      text:
        `Aucune date de ce fichier ne permettait de trancher entre jj/mm et mm/jj ` +
        `(aucun jour supérieur à 12). Les dates ont été lues « ${lu} ». ` +
        `Si c'est faux, elles sont décalées de plusieurs mois.`,
    }),
    h(
      "div",
      { class: "banner-actions" },
      h("button", {
        class: "btn btn-sm",
        type: "button",
        text: `Confirmer « ${lu} »`,
        onclick: () => resolveDateOrder(s, false),
      }),
      h("button", {
        class: "btn btn-sm",
        type: "button",
        text: `Corriger en « ${inverse} »`,
        onclick: () => resolveDateOrder(s, true),
      })
    )
  );
}

/**
 * Confirme ou inverse l'ordre jour/mois d'une série déjà importée.
 * `swap = true` réécrit les horodatages bruts ; aucun réimport n'est nécessaire.
 */
async function resolveDateOrder(s, swap) {
  // Hors doute avéré, l'inversion touche une série réputée correcte : on demande.
  if (swap && !s.date_format_uncertain) {
    const ok = await window.confirm(
      "Inverser jour et mois sur toutes les mesures de cette série ?\n\n" +
        "Cette série n'est pas marquée douteuse : si elle était correcte, " +
        "cette opération rendra ses dates fausses. L'opération est réversible " +
        "en la relançant."
    );
    if (!ok) return;
  }

  try {
    const report = await invoke("resolve_series_date_order", {
      series_id: s.id,
      swap,
    });
    const ordre = report.date_order === "DayFirst" ? "jj/mm" : "mm/jj";
    toast(
      swap
        ? `${fmtNumber(report.updated_measurements)} mesures replacées — dates lues en ${ordre}, ` +
            `du ${fmtUtc(report.period_start_utc)} au ${fmtUtc(report.period_end_utc)}.`
        : `Format ${ordre} confirmé.`
    );
    await loadPoints();
  } catch (e) {
    toast(String(e), true);
  }
}

function stat(label, value, cold = false) {
  return h(
    "div",
    { class: `stat${cold ? " is-cold" : ""}` },
    h("span", { class: "stat-label", text: label }),
    h("span", { class: "stat-value", text: value })
  );
}

function renderStats(s) {
  const wrap = h("div", {});

  wrap.appendChild(
    h(
      "div",
      { class: "stat-grid" },
      stat("Minimum extrême", fmtTemp(s.extreme_min_c), true),
      stat("Moyenne des minimums quotidiens", fmtTemp(s.mean_daily_min_c)),
      stat("Maximum extrême", fmtTemp(s.extreme_max_c)),
      stat("Moyenne générale", fmtTemp(s.mean_c)),
      stat("Amplitude moyenne", fmtTemp(s.mean_amplitude_c)),
      stat("Amplitude maximale", fmtTemp(s.max_amplitude_c)),
      stat("Journées couvertes", fmtNumber(s.day_count)),
      stat("Mesures", fmtNumber(s.measurement_count))
    )
  );

  wrap.appendChild(
    h("p", {
      class: "muted",
      text: `Période : ${fmtUtc(s.first_utc)} → ${fmtUtc(s.last_utc)}${
        s.extreme_min_date ? ` · minimum atteint le ${s.extreme_min_date}` : ""
      }`,
      style: "margin-top:10px",
    })
  );

  return wrap;
}

async function recalibrate(series) {
  const input = window.prompt(
    "Nouveau décalage d'horloge, en secondes, à AJOUTER à l'horodatage brut pour obtenir l'UTC.\n" +
      "Exemples : logger réglé en UTC+1 → -3600 ; logger en avance de 45 s → -45.\n" +
      "Les horodatages sont recalculés depuis les valeurs brutes (les corrections ne s'empilent pas).",
    String(series.clock_offset_seconds)
  );
  if (input === null) return;

  const value = Number(input);
  if (!Number.isFinite(value) || !Number.isInteger(value)) {
    toast("Décalage invalide : un nombre entier de secondes est attendu.", true);
    return;
  }

  try {
    const n = await invoke("recalibrate_series", {
      series_id: series.id,
      clock_offset_seconds: value,
    });
    toast(`${n} mesure(s) recalées sur un décalage de ${value} s.`);
    await loadPoints();
  } catch (e) {
    toast(String(e), true);
  }
}

// -------------------------------------------------- formulaire « ajouter un point »

async function loadCategories() {
  state.categories = await invoke("category_options");
  const select = el("point-category");
  clear(select);
  for (const c of state.categories) {
    select.appendChild(
      h("option", { value: c.value }, `${c.value} — ${c.label}`)
    );
  }
  syncSizeOptions();
}

/**
 * Reconstruit la liste des tailles à partir des règles renvoyées par le Rust.
 * Aucune règle n'est dupliquée ici : on ne fait qu'afficher ce que le modèle autorise.
 */
function syncSizeOptions() {
  const category = state.categories.find((c) => c.value === el("point-category").value);
  const select = el("point-size");
  const previous = select.value;
  clear(select);

  if (!category) return;

  for (const size of category.allowed_sizes) {
    select.appendChild(
      h(
        "option",
        { value: size === null ? "" : size },
        size === null ? "— aucune —" : size === "Gr" ? "Gr (gros)" : "Pt (petit)"
      )
    );
  }

  const onlyNone = category.allowed_sizes.length === 1 && category.allowed_sizes[0] === null;
  select.disabled = onlyNone;
  if ([...select.options].some((o) => o.value === previous)) select.value = previous;

  el("size-hint").textContent = onlyNone
    ? "Un point Cat1 ne porte jamais de taille."
    : category.requires_size
      ? "Obligatoire pour cette catégorie."
      : "Facultatif : Gr ou aucune.";
}

/**
 * Mode courant du formulaire `#dlg-point`. Un seul formulaire sert à créer et à
 * modifier : dupliquer la modale dupliquerait aussi la logique catégorie/taille.
 */
const pointDialogState = { mode: "create", pointId: null, dirty: false };

/**
 * Ouvre le formulaire de point.
 *
 * @param {"create"|"edit"} mode
 * @param {{point?: object, lat?: number, lon?: number}} options
 */
function openPointDialog(mode, options = {}) {
  const { point = null, lat = null, lon = null } = options;

  hideError(el("point-error"));
  el("form-point").reset();

  pointDialogState.mode = mode;
  pointDialogState.pointId = point ? point.id : null;

  el("point-dialog-title").textContent =
    mode === "edit" ? "Modifier le point" : "Ajouter un point";

  if (point) {
    el("point-name").value = point.name;
    el("point-altitude").value = point.altitude_m ?? "";
    el("point-lat").value = point.latitude;
    el("point-lon").value = point.longitude;
    // L'ordre est impératif : `syncSizeOptions` reconstruit la liste des tailles
    // à partir de la catégorie, donc la catégorie doit être posée avant, et la
    // taille après — sinon le <select> est vidé juste après avoir été rempli.
    el("point-category").value = point.category;
    syncSizeOptions();
    el("point-size").value = point.size || "";

    el("factor-sky").value = point.factor_sky || "";
    el("factor-wind").value = point.factor_wind || "";
    el("factor-slope").value = point.factor_slope || "";
    el("factor-snow").value = point.factor_snow_cover || "";
    el("factor-forest").value = point.factor_forest_cover || "";
    el("point-open").checked = Boolean(point.is_open_terrain);
    el("point-notes").value = point.notes || "";

    el("point-station").value = point.infoclimat_station_id || "";
    el("point-romma").value = point.romma_station_id || "";
  } else {
    syncSizeOptions();
    if (lat !== null && lon !== null) {
      el("point-lat").value = lat.toFixed(6);
      el("point-lon").value = lon.toFixed(6);
    }
  }

  syncIgnAltitudeButton();
  el("ign-altitude-status").textContent = "";
  pointDialogState.dirty = false;
  el("dlg-point").returnValue = "";
  el("dlg-point").showModal();
}

/**
 * Coordonnées de la fiche point, ou `null` si l'une manque ou sort des bornes.
 * Le bouton IGN n'est actif qu'avec des coordonnées exploitables.
 */
function pointDialogCoords() {
  const latTxt = el("point-lat").value.trim();
  const lonTxt = el("point-lon").value.trim();
  if (latTxt === "" || lonTxt === "") return null;
  const lat = Number(latTxt), lon = Number(lonTxt);
  if (!Number.isFinite(lat) || !Number.isFinite(lon) || Math.abs(lat) > 90 || Math.abs(lon) > 180) return null;
  return { lat, lon };
}

function syncIgnAltitudeButton() {
  const btn = el("btn-ign-altitude");
  if (btn.dataset.busy === "1") return;
  const ok = pointDialogCoords() !== null;
  btn.disabled = !ok;
  btn.title = ok
    ? "Proposer l'altitude du sol selon l'IGN (RGE ALTI) — modifiable ensuite"
    : "Renseignez d'abord la latitude et la longitude";
}

/**
 * Remplit l'altitude avec la valeur IGN. Ce n'est qu'une proposition : le
 * champ reste libre, et un échec laisse la saisie manuelle intacte.
 */
async function fetchIgnAltitude() {
  const coords = pointDialogCoords();
  if (!coords) return;
  const btn = el("btn-ign-altitude");
  const statut = el("ign-altitude-status");
  btn.dataset.busy = "1";
  btn.disabled = true;
  btn.textContent = "Recherche…";
  statut.textContent = "Interrogation de l'IGN…";
  try {
    const z = await invoke("ign_altitude", { latitude: coords.lat, longitude: coords.lon });
    el("point-altitude").value = Math.round(z);
    statut.textContent = `Altitude IGN : ${z.toLocaleString("fr-FR")} m (modifiable si votre mesure diffère).`;
  } catch (e) {
    statut.textContent = "Altitude IGN indisponible : " + String(e).replace(/\.?$/, ".") + " Saisissez-la à la main.";
  } finally {
    btn.dataset.busy = "";
    btn.textContent = "Récupérer via IGN";
    syncIgnAltitudeButton();
  }
}

async function submitPoint(event) {
  event.preventDefault();
  const errorNode = el("point-error");
  hideError(errorNode);

  const altitude = el("point-altitude").value.trim();
  const payload = {
    project_id: state.project.id,
    name: el("point-name").value,
    latitude: Number(el("point-lat").value),
    longitude: Number(el("point-lon").value),
    altitude_m: altitude === "" ? null : Number(altitude),
    category: el("point-category").value,
    size: el("point-size").value || null,
    factor_sky: el("factor-sky").value || null,
    factor_wind: el("factor-wind").value || null,
    factor_slope: el("factor-slope").value || null,
    factor_snow_cover: el("factor-snow").value || null,
    factor_forest_cover: el("factor-forest").value || null,
    is_open_terrain: el("point-open").checked,
    notes: el("point-notes").value || null,

    infoclimat_station_id: el("point-station").value.trim() || null,
    romma_station_id: el("point-romma").value.trim() || null,
  };

  const edition = pointDialogState.mode === "edit";

  try {
    const point = edition
      ? await invoke("update_point", {
          point_id: pointDialogState.pointId,
          payload,
        })
      : await invoke("create_point", { payload });

    pointDialogState.dirty = false;
    el("dlg-point").close("saved");
    el("form-point").reset();
    syncSizeOptions();
    await loadPoints();
    toast(`Point « ${point.name} » ${edition ? "modifié" : "ajouté"}.`);
    if (!edition && depotState.apresPoint) {
      const chemins = depotState.apresPoint;
      depotState.apresPoint = null;
      deposerFichierImport(chemins);
    }
  } catch (e) {
    // Message métier venu du Rust (règle catégorie/taille, coordonnées…).
    showError(errorNode, String(e));
  }
}

/**
 * La confirmation annonce la cascade : supprimer une station efface aussi les
 * séries et mesures qui lui appartiennent, opération irréversible.
 */
async function deletePoint(point) {
  if (!(await askPointDeletion(point))) return;

  await invoke("delete_point", { point_id: point.id });
  state.exportSelection.delete(point.id);
  if (state.detailPointId === point.id) el("dlg-detail").close();
  await loadPoints();
  toast(`Point « ${point.name} » supprimé. Ctrl+Z pour annuler.`);
}

/** Résolveur vivant uniquement pendant l'affichage de la confirmation. */
let deletePointResolve = null;

/** Affiche une confirmation native à l'interface, fiable dans WebView2. */
function askPointDeletion(point) {
  const series = point.series_count || 0;
  const detail = series
    ? ` Ses ${series} série(s) de mesures seront aussi supprimée(s).`
    : " Ce point ne possède encore aucune série de mesures.";
  el("delete-point-message").textContent = `Voulez-vous supprimer « ${point.name} » ?${detail}`;

  const dlg = el("dlg-delete-point");
  dlg.returnValue = "";
  dlg.showModal();
  return new Promise((resolve) => {
    deletePointResolve = resolve;
  });
}

/** Ferme la confirmation et délivre le choix ; idempotent pour Échap. */
function resolvePointDeletion(confirmed) {
  const resolve = deletePointResolve;
  deletePointResolve = null;
  const dlg = el("dlg-delete-point");
  if (dlg.open) dlg.close(confirmed ? "confirm" : "cancel");
  if (resolve) resolve(confirmed);
}

// ------------------------------------------------------------------- import

/** Dernier onglet d'import choisi par l'utilisateur (confort, par poste). */
const IMPORT_TAB_KEY = "gws_import_tab";

function ongletImportPresent(tab) {
  return Boolean(tab && document.querySelector(`#dlg-import .tab[data-tab="${tab}"]`));
}

function memoriserOngletImport(tab) {
  try {
    localStorage.setItem(IMPORT_TAB_KEY, tab);
  } catch {
    // Stockage indisponible : l'onglet par défaut reprendra la main.
  }
}

/**
 * Onglet d'ouverture de l'import : le dernier utilisé ; à défaut, l'assistant
 * IA si une clé Groq est enregistrée, sinon l'import automatique Elitech, qui
 * ne demande aucune clé.
 */
async function ongletImportParDefaut() {
  let memorise = null;
  try {
    memorise = localStorage.getItem(IMPORT_TAB_KEY);
  } catch {
    memorise = null;
  }
  if (ongletImportPresent(memorise)) return memorise;
  // Automatique d'abord, même avec une clé Groq : un export Elitech se lit
  // localement, sans réseau. L'IA reste l'onglet de recours.
  return "auto";
}

const importState = {
  tab: "auto",
  /** Des jours de saisie manuelle ont été tapés et pas encore importés. */
  entryDirty: false,
  fileName: null,
  previewDelimiter: null,
  bytes: null,
  rc5: { readingId: null, portName: null },
  ai: {
    files: [],
    selectedIndex: 0,
    currentMapping: null,
  },
};

/**
 * Fenêtres dont la fermeture demande confirmation quand une saisie n'est pas
 * enregistrée : la fiche point, et l'import tant que des jours de saisie
 * manuelle ont été tapés sans être importés.
 */
const GARDES_FERMETURE = {
  "dlg-point": {
    modifie: () => pointDialogState.dirty,
    oublier: () => {
      pointDialogState.dirty = false;
    },
  },
  "dlg-import": {
    modifie: () => Boolean(importState.entryDirty),
    oublier: () => {
      importState.entryDirty = false;
    },
  },
};

/** Ferme `dlg`, après confirmation si une saisie en cours serait perdue. */
async function fermerAvecGarde(dlg) {
  if (!dlg) return;
  const garde = GARDES_FERMETURE[dlg.id];
  if (garde && garde.modifie()) {
    const ok = await confirm(
      "Des saisies n'ont pas été enregistrées.\n\nFermer quand même et les perdre ?"
    );
    if (!ok) return;
    garde.oublier();
  }
  dlg.close();
}

/** Signature d'une archive zip, donc d'un classeur OOXML : « PK\x03\x04 ». */
function looksLikeXlsx(bytes) {
  return (
    bytes.length >= 4 &&
    bytes[0] === 0x50 &&
    bytes[1] === 0x4b &&
    bytes[2] === 0x03 &&
    bytes[3] === 0x04
  );
}

function importContent() {
  return el("import-content").value;
}

/** `clock_offset_seconds` = secondes à AJOUTER au brut pour obtenir l'UTC vrai. */
function computeOffset() {
  const tz = Number(el("import-tz").value) || 0;
  const drift = Number(el("import-drift").value) || 0;
  return -(tz + drift);
}

function updateOffsetHint() {
  const tz = Number(el("import-tz").value) || 0;
  const drift = Number(el("import-drift").value) || 0;
  const offset = computeOffset();
  el("offset-hint").textContent =
    `Décalage appliqué : ${offset >= 0 ? "+" : ""}${offset} s ` +
    `(fuseau ${tz === 0 ? "UTC" : `UTC+${tz / 3600}`} → UTC, dérive ${drift} s). ` +
    `Chaque mesure sera stockée en UTC = horodatage brut ${offset >= 0 ? "+" : "−"} ${Math.abs(offset)} s ; ` +
    `le brut est conservé pour permettre un recalibrage ultérieur.`;
}

function importCommon() {
  return {
    point_id: el("import-point").value,
    content: importContent(),
    file_name: importState.fileName,
    role: el("import-role").value,
    clock_offset_seconds: computeOffset(),
    raw_timestamp_was_utc: Number(el("import-tz").value) === 0,
    logger_serial: el("import-serial").value.trim() || null,
    label: null,
    // Jamais `true` au premier essai : c'est le dialogue de ré-import qui
    // relance la commande avec ce drapeau, et lui seul.
    force_duplicate: false,
  };
}

// -------------------------------------------- notification de branchement RC-5

/**
 * Ce que la surveillance dit du dernier logger annoncé.
 *
 * Alimenté **uniquement** par les événements du fil de surveillance du backend
 * (`rc5-detecte`) : il n'y a pas de second mécanisme de détection côté frontend,
 * et il ne doit pas y en avoir — deux sources de vérité sur « un logger est-il
 * là ? » finiraient par diverger.
 *
 * Ce champ ne sert plus qu'à la notification de branchement, qui parle d'un
 * appareil à la fois. L'inventaire complet — plusieurs loggers branchés en même
 * temps — vit dans `GwsElitech`, qui reçoit chaque événement tel quel : c'est là
 * qu'était le défaut, un état unique côté JS ne pouvant décrire qu'un logger.
 */
const rc5Dernier = { portName: null, libelle: null };

/** La ligne du bouton d'accès : combien de loggers sont branchés, là, maintenant. */
function refreshElitechButton() {
  const note = el("elitech-entry-note");
  if (!note) return;
  const n = GwsElitech.nombreDetectes();
  note.textContent =
    n === 0
      ? "Lire un logger RC-5 branché en USB — aucun détecté pour l'instant"
      : n === 1
        ? `1 logger RC-5 branché · ${rc5Dernier.libelle || ""}`
        : `${n} loggers RC-5 branchés — un onglet par appareil`;
}

/**
 * Ouvre la page des loggers.
 *
 * Elle ne dépend plus d'un projet ouvert : elle charge la liste des projets et
 * laisse choisir dans le panneau du logger. Le projet courant, s'il y en a un,
 * n'est qu'une présélection.
 */
async function openElitechPage() {
  const projets = await invoke("list_projects");
  el("view-projects").hidden = true;
  el("view-home").hidden = true;
  el("view-project").hidden = true;
  // Cette page s'atteint aussi depuis la liste des projets, où le bouton
  // d'accueil part désactivé (rien à quoi revenir). Ici, il y a toujours la
  // liste des projets à retrouver : le bouton doit donc être actif, que ce
  // dernier ait été ouvert avec ou sans projet en cours.
  el("btn-home").disabled = false;
  GwsElitech.open({
    projects: projets,
    projectId: state.project ? state.project.id : null,
    pointId:
      state.detailPointId || (state.selectedIds.length === 1 ? state.selectedIds[0] : null),
  });
}

/** Retour à la liste des projets. Les onglets ouverts survivent (cf. `elitech.js`). */
function closeElitechPage() {
  GwsElitech.close();
  el("view-projects").hidden = false;
  // Sans projet en cours, on est revenu là où le bouton d'accueil n'a plus
  // rien à faire : `backToProjects()`, appelé juste après par l'appelant s'il
  // y avait un projet, le remettra à `false` dans ce cas.
  el("btn-home").disabled = !state.project;
}

/**
 * Notification persistante de branchement d'un logger.
 *
 * Persistante, et non un toast : elle porte une action, et un message qui
 * s'efface au bout de trois secondes n'en est pas une. Elle reste jusqu'à ce que
 * l'utilisateur clique, ou que le logger soit débranché.
 */
function showRc5Notice({ titre, detail, action, onAction, avertissement = false }) {
  // Sur téléphone, le RC-5 ne s'atteint plus depuis un projet : cette
  // notification proposait précisément l'inverse (« Importer » ouvrait la modale
  // d'import du projet ouvert). Elle n'a donc plus d'action à porter — et le
  // widget de l'accueil dit déjà, au même moment, qu'un logger est là.
  if (GwsMobile.estActif()) return;
  el("rc5-notice-title").textContent = titre;
  el("rc5-notice-detail").textContent = detail || "";
  const bouton = el("rc5-notice-import");
  bouton.textContent = action;
  bouton.onclick = () => {
    hideRc5Notice();
    onAction();
  };
  el("rc5-notice").classList.toggle("is-warning", avertissement);
  el("rc5-notice").hidden = false;
}

function hideRc5Notice() {
  el("rc5-notice").hidden = true;
}

/**
 * Ouvre l'import sur l'onglet USB et lance la lecture, sans autre clic.
 *
 * C'est l'objectif produit du branchement automatique : brancher, cliquer une
 * fois. Le point de rattachement reste à choisir — le deviner rattacherait une
 * série au mauvais endroit, ce que rien ne rattrape ensuite.
 */
async function importDepuisLogger() {
  if (!state.project) {
    toast("Ouvrez d'abord un projet : une série est toujours rattachée à un point.", true);
    return;
  }
  if (state.points.length === 0) {
    toast("Ajoutez d'abord un point : une série est toujours rattachée à un point.", true);
    openPointDialog("create");
    return;
  }
  await openImportDialog("rc5");
  await readRc5();
}

/**
 * Branche l'écoute de l'événement émis par le fil de surveillance du backend.
 *
 * Sans Tauri (page ouverte dans un navigateur seul), il n'y a pas d'événements :
 * l'absence est traitée, pas contournée.
 */
/**
 * Fenêtre de progression de la synchro Cloud. N'apparaît qu'au premier
 * élément réellement échangé (une synchro sans changement n'affiche rien) et
 * disparaît dès la fin, sans délai.
 */
// --- Projet Cloud au démarrage --------------------------------------------------
let cloudDemarrageEnCours = false;

function etapeCloudDemarrage(texte) {
  cloudDemarrageEnCours = true;
  const boite = el("cloud-demarrage");
  if (!boite.matches(":popover-open")) boite.showPopover();
  el("cloud-demarrage-label").textContent = texte;
  el("cloud-demarrage-count").textContent = "";
  const barre = el("cloud-demarrage-bar");
  barre.classList.add("indetermine");
  barre.style.width = "";
}

async function synchroCloudDemarrage() {
  etapeCloudDemarrage("Vérification des paquets…");
  try {
    const res = await invoke("cloud_sync");
    if (res && (res.uploaded > 0 || res.downloaded > 0)) {
      toast(`Synchronisation Cloud : ${res.uploaded} envoyé(s), ${res.downloaded} reçu(s).`);
      await loadCloudStatus();
      renderCloudStatus();
      // Des projets ont pu arriver : la liste est relue sans changer l'écran en cours.
      await loadProjects();
    }
  } catch (e) {
    console.warn("Synchro Cloud au démarrage :", e);
    toast(`Synchronisation Cloud impossible : ${e}`, true);
  } finally {
    cloudDemarrageEnCours = false;
    const boite = el("cloud-demarrage");
    if (boite.matches(":popover-open")) boite.hidePopover();
  }
}

function listenCloudProgress() {
  const evenements = window.__TAURI__ && window.__TAURI__.event;
  if (!evenements) return;
  const PHASES = { reception: "Réception", envoi: "Envoi", suppression: "Suppression" };
  evenements.listen("cloud-progress", ({ payload: p }) => {
    // Au démarrage, la progression va dans la fenêtre « Projet Cloud ».
    const id = cloudDemarrageEnCours ? "cloud-demarrage" : "cloud-progress";
    const boite = el(id);
    if (!boite.matches(":popover-open")) boite.showPopover();
    const phase = PHASES[p.phase] || "Synchronisation";
    if (cloudDemarrageEnCours) {
      el("cloud-demarrage-label").textContent = `${phase} : ${p.label}`;
    } else {
      el("cloud-progress-phase").textContent = phase;
      el("cloud-progress-label").textContent = p.label;
    }
    const barre = el(id + "-bar");
    if (p.total > 0) {
      barre.classList.remove("indetermine");
      barre.style.width = `${Math.min(100, Math.round((100 * p.done) / p.total))}%`;
      el(id + "-count").textContent = `${p.done}/${p.total}`;
    } else {
      barre.classList.add("indetermine");
      barre.style.width = "";
      el(id + "-count").textContent = "";
    }
  });
  evenements.listen("cloud-progress-end", () => {
    if (cloudDemarrageEnCours) return; // fermée par synchroCloudDemarrage
    const boite = el("cloud-progress");
    if (boite.matches(":popover-open")) boite.hidePopover();
  });
}

/** Liste des projets et de leur réglage de synchro (Paramètres → Cloud). */
async function renderCloudProjects() {
  const boite = el("cloud-projects");
  if (!boite) return;
  let prefs;
  try {
    prefs = await invoke("cloud_project_prefs");
  } catch (e) {
    boite.textContent = String(e);
    return;
  }
  boite.textContent = "";
  for (const p of prefs) {
    const ligne = document.createElement("label");
    ligne.className = "cloud-project-row";
    const choix = document.createElement("select");
    for (const [v, t] of [["auto", "Automatique"], ["on", "Synchronisé"], ["off", "Non synchronisé"]]) {
      const o = document.createElement("option");
      o.value = v;
      o.textContent = t;
      choix.appendChild(o);
    }
    choix.value = p.excluded === null ? "auto" : p.excluded ? "off" : "on";
    if (p.demo) {
      // Projet d'exemple : toujours exclu, côté Rust aussi (`projet_exclu`).
      choix.value = "off";
      choix.disabled = true;
      choix.title = "Projet d'exemple : jamais synchronisé vers le Cloud";
    }
    // « Automatique » n'est qu'un état initial : une fois choisi, le réglage reste explicite.
    choix.addEventListener("change", async () => {
      if (choix.value === "auto") return;
      try {
        await invoke("cloud_set_project_excluded", { project_id: p.id, excluded: choix.value === "off" });
      } catch (e) {
        toast(String(e), true);
      }
    });
    const nom = document.createElement("span");
    nom.textContent = p.name;
    ligne.append(nom, choix);
    boite.appendChild(ligne);
  }
}

function listenRc5() {
  const evenements = window.__TAURI__ && window.__TAURI__.event;
  if (!evenements) return;

  evenements.listen("rc5-detecte", ({ payload }) => {
    if (payload.kind === "rc5") {
      const serie = payload.logger_serial ? ` (n° ${payload.logger_serial})` : "";
      // Un logger confirmé. L'événement part **entier** vers `GwsElitech`, qui
      // range les loggers par port : c'est ce qui permet d'en tenir plusieurs.
      // Le résumer ici dans un état unique était précisément le défaut.
      // Le même événement, tel quel, vers **un** des deux modules : la page à
      // onglets du poste fixe, ou la zone de détection de l'accueil mobile.
      // Les deux à la fois construiraient deux panneaux par logger — deux
      // graphiques, deux jeux d'écouteurs — dont un invisible à jamais.
      if (GwsMobile.estActif()) GwsMobile.noteDetection(payload);
      else GwsElitech.noteDetection(payload);
      rc5Dernier.portName = payload.port.port_name;
      rc5Dernier.libelle = `${payload.model_label}${serie} · ${fmtNumber(payload.rec_count)} mesure(s)`;
      refreshElitechButton();
      showRc5Notice({
        titre: `Logger ${payload.model_label} détecté${serie}`,
        detail: `${fmtNumber(payload.rec_count)} mesure(s) en mémoire, sur ${payload.port.port_name}. Importer ses données ?`,
        action: "Importer",
        onAction: importDepuisLogger,
      });
    } else if (payload.kind === "autre" && payload.port_busy) {
      // Le seul « pas un RC-5 » qui mérite l'écran : un logger est sans doute
      // bien là, mais un autre programme le tient. Le message du backend dit
      // déjà quoi faire ; le bouton relance la recherche une fois ElitechLog
      // fermé.
      showRc5Notice({
        titre: "Logger branché, mais port occupé",
        detail: payload.message,
        action: "Réessayer",
        avertissement: true,
        onAction: async () => {
          try {
            await invoke("rescan_rc5");
            toast("Nouvelle recherche lancée.");
          } catch (e) {
            toast(String(e), true);
          }
        },
      });
    } else if (payload.kind === "debranche") {
      // L'onglet de ce logger reste ouvert : ce qui y est affiché a bien été lu,
      // et le refermer d'autorité ferait disparaître un rapport d'enregistrement
      // que l'utilisateur est peut-être en train de lire. Seules les actions qui
      // parlent au matériel s'éteignent (cf. `panneau.debrancher`).
      if (GwsMobile.estActif()) GwsMobile.noteDebranchement(payload.port_name);
      else GwsElitech.noteDebranchement(payload.port_name);
      if (rc5Dernier.portName === payload.port_name) {
        // La notification ne survit pas au débranchement : proposer d'importer
        // un logger absent ne mènerait qu'à une erreur.
        hideRc5Notice();
        rc5Dernier.portName = null;
        rc5Dernier.libelle = null;
      }
      refreshElitechButton();
      if (!el("view-elitech").hidden) {
        toast("Un logger a été débranché : les actions sur cet appareil ne répondront plus.");
      }
    }
    // Un CH340 qui n'est pas un RC-5 et ne bloque rien (un Arduino) n'a aucune
    // raison d'interrompre l'utilisateur : il n'est signalé que dans l'onglet
    // d'import, quand il cherche lui-même son logger.
  });
}

// ------------------------------------------------------- ré-import détecté

/**
 * Demande quoi faire d'un contenu déjà importé.
 *
 * Le backend n'a rien écrit quand il renvoie `duplicate_suspected` : tant que
 * cette promesse n'est pas résolue par un `true`, la base est intacte. C'est ce
 * qui permet de poser la question **après** l'analyse complète du fichier plutôt
 * qu'avant, donc avec de vrais chiffres à montrer.
 *
 * @returns {Promise<boolean>} `true` si l'utilisateur veut forcer.
 */
let duplicateResolve = null;

function askDuplicate(dup) {
  el("dup-message").textContent = dup.message;

  const kv = el("dup-details");
  clear(kv);
  const row = (k, v) => {
    kv.appendChild(h("dt", { text: k }));
    kv.appendChild(h("dd", { text: v }));
  };
  row("Point", dup.point_name);
  row("Série existante", dup.label || dup.raw_file_reference || dup.series_id);
  row("Importée le", fmtLocal(dup.imported_at));
  row("Source", sourceLabel(dup.source_type));
  row("Rôle", dup.role === "Primary" ? "Principal" : "Secours");
  row(
    "Mesures",
    `${fmtNumber(dup.measurement_count)} déjà en base · ${fmtNumber(dup.incoming_count)} dans le lot présenté`
  );
  row("Période couverte (UTC)", `${fmtUtc(dup.period_start_utc)} → ${fmtUtc(dup.period_end_utc)}`);

  const dlg = el("dlg-duplicate");
  dlg.returnValue = "";
  dlg.showModal();
  return new Promise((resolve) => {
    duplicateResolve = resolve;
  });
}

/**
 * Tranche la question posée par [`askDuplicate`], une fois pour toutes.
 *
 * Appelée depuis les deux boutons **et** depuis l'événement `close` (fermeture
 * par Échap). Le fait de vider `duplicateResolve` d'abord la rend idempotente :
 * le clic ferme le dialogue, ce qui déclenche `close`, qui rappellerait sinon la
 * fonction une seconde fois — avec `false`, écrasant le choix de l'utilisateur.
 *
 * Ne pas s'en remettre au seul événement `close` est délibéré : il n'est pas
 * émis partout de la même façon (le harnais de vérification, notamment, ne le
 * produit pas), et un import bloqué sur une promesse jamais tenue laisserait la
 * boîte d'import figée, bouton grisé, sans message.
 */
function reglerReimport(forcer) {
  const resoudre = duplicateResolve;
  duplicateResolve = null;
  const dlg = el("dlg-duplicate");
  if (dlg.open) dlg.close(forcer ? "force" : "cancel");
  if (resoudre) resoudre(forcer);
}

/**
 * Déroule un import en tenant compte de l'alerte de ré-import.
 *
 * `lancer(force)` doit invoquer la commande et renvoyer l'enveloppe
 * `{ outcome, … }`. Renvoie le rapport, ou `null` si l'utilisateur a renoncé.
 */
async function avecConfirmationDeReimport(lancer) {
  let issue = await lancer(false);
  if (issue.outcome !== "duplicate_suspected") return issue.report;

  const forcer = await askDuplicate(issue.duplicate);
  if (!forcer) return null;

  issue = await lancer(true);
  // Le second appel force : il ne peut plus revenir avec la même alerte.
  return issue.report;
}

/**
 * Ouvre l'import. `onglet` force l'onglet (lecture RC-5, fichier déposé) ;
 * sinon, voir `ongletImportParDefaut`.
 */
async function openImportDialog(onglet = null) {
  const tab = ongletImportPresent(onglet) ? onglet : await ongletImportParDefaut();
  const select = el("import-point");
  clear(select);
  for (const p of state.points) {
    select.appendChild(
      h("option", { value: p.id }, `${p.name} (${p.category}${p.size ? " " + p.size : ""})`)
    );
  }
  // Présélection : la fiche ouverte s'il y en a une, sinon l'unique point
  // sélectionné. Avec plusieurs points cochés, deviner serait plus risqué que
  // laisser choisir — une série rattachée au mauvais point fausse tout.
  const suggere =
    state.detailPointId || (state.selectedIds.length === 1 ? state.selectedIds[0] : null);
  if (suggere) select.value = suggere;

  el("analysis").hidden = true;
  el("ic-analysis").hidden = true;
  el("romma-analysis").hidden = true;
  el("rc5-analysis").hidden = true;
  el("rc5-status").textContent = "";
  el("preview").hidden = true;
  el("mapping-fields").hidden = true;
  el("import-report").hidden = true;
  hideError(el("import-error"));

  // Une lecture directe ne survit pas à la fermeture du dialogue : le jeton
  // désignerait un lot que l'utilisateur n'a plus sous les yeux, et le
  // pré-remplissage de dérive porterait sur un autre logger.
  importState.rc5 = { readingId: null, portName: null };
  el("import-tz").onchange = null;

  // La saisie manuelle repart sur la journée d'hier : c'est le relevé qu'on
  // recopie en rentrant du terrain, pas celui d'un jour encore en cours.
  const hier = new Date(Date.now() - 86400000).toISOString().slice(0, 10);
  el("entry-from").value = hier;
  el("entry-to").value = hier;
  renderEntryDays();
  importState.entryDirty = false;
  masquerProgressionImport();

  updateOffsetHint();

  // Pré-remplit les identifiants de station si le point présélectionné en porte.
  const choisi = findPoint(select.value);
  el("ic-station").value = (choisi && choisi.infoclimat_station_id) || "";
  el("romma-station").value = (choisi && choisi.romma_station_id) || "";

  switchTab(tab);
  remplirStations();
  if (!el("dlg-import").open) el("dlg-import").showModal();
}

// ------------------------------------------- fenêtre « Importer des données » : 3 onglets

/** Onglet visible (Fichier, Station en ligne, Relevé sur place) d'un lecteur interne. */
function groupeDuLecteur(tab) {
  if (tab === "station") return "station";
  if (tab === "entry" || tab === "rc5") return "releve";
  return "fichier";
}

/** Met l'interface des 3 onglets en accord avec le lecteur actif. */
function syncGroupeImport(tab) {
  const groupe = groupeDuLecteur(tab);
  for (const b of document.querySelectorAll("#dlg-import .tab-groupe")) {
    const actif = b.dataset.groupe === groupe;
    b.classList.toggle("is-active", actif);
    b.setAttribute("aria-selected", actif ? "true" : "false");
  }
  el("import-sous-fichier").hidden = groupe !== "fichier";
  el("import-sous-releve").hidden = groupe !== "releve";
  if (groupe === "fichier") el("import-format").value = tab;
  if (groupe === "releve") {
    for (const r of document.querySelectorAll('input[name="import-releve"]')) r.checked = r.value === tab;
  }
  el("import-point-field").hidden = groupe === "station";
  // Rien d'avancé à régler pour une API ou une saisie : on cache le bloc.
  el("import-avance").hidden = groupe === "station" || tab === "entry" || tab === "ai";
  el("btn-do-import").textContent = groupe === "station" ? "Récupérer" : tab === "entry" ? "Enregistrer" : "Importer";
}

/** Liste des points rattachés à une station Infoclimat (onglet Station en ligne). */
function remplirStations() {
  const zone = el("station-liste");
  clear(zone);
  const points = state.points.filter((p) => p.infoclimat_station_id);
  if (!points.length) {
    zone.appendChild(
      h("p", { class: "empty", text: "Aucun point n'est rattaché à une station Infoclimat. Renseignez la station dans la fiche du point." })
    );
    el("station-cout").textContent = "";
    return;
  }
  const preselection = state.detailPointId || (state.selectedIds.length ? state.selectedIds : null);
  for (const p of points) {
    const coche = !preselection || [].concat(preselection).includes(p.id);
    zone.appendChild(
      h(
        "label",
        { class: "checkbox" },
        h("input", { type: "checkbox", value: p.id, checked: coche, onchange: majCoutStations }),
        ` ${p.name} `,
        h("span", { class: "muted", text: `(${p.infoclimat_station_id})` })
      )
    );
  }
  majCoutStations();
}

function stationsCochees() {
  return [...el("station-liste").querySelectorAll("input:checked")].map((c) => findPoint(c.value)).filter(Boolean);
}

function majCoutStations() {
  const n = stationsCochees().length;
  const periode = document.querySelector('input[name="station-quoi"]:checked')?.value === "periode";
  el("station-cout").textContent = periode
    ? "Une période passée se récupère station par station : cochez-en une seule."
    : `Coût : ${n} requête(s) Infoclimat.`;
}

async function recupererStations() {
  const points = stationsCochees();
  const periode = document.querySelector('input[name="station-quoi"]:checked')?.value === "periode";
  if (!points.length) {
    showError(el("import-error"), "Cochez au moins une station.");
    return;
  }
  if (periode) {
    if (points.length > 1) {
      showError(el("import-error"), "Une période passée se récupère station par station : cochez-en une seule.");
      return;
    }
    el("dlg-import").close();
    openInfoclimatHistoryModal(points[0]);
    return;
  }
  const bouton = el("btn-do-import");
  bouton.disabled = true;
  try {
    for (const [i, p] of points.entries()) {
      afficherProgressionImport(`Récupération ${i + 1} / ${points.length} : ${p.name}…`);
      await fetchInfoclimat(p, false);
    }
  } finally {
    masquerProgressionImport();
    bouton.disabled = false;
  }
  el("dlg-import").close();
  toast(`Récupération terminée pour ${points.length} station(s).`);
}

/** Barre indéterminée « Import en cours… » (même style que la synchro Cloud). */
function afficherProgressionImport(libelle = "Import en cours…") {
  el("import-progress-label").textContent = libelle;
  el("import-progress").hidden = false;
}

function masquerProgressionImport() {
  el("import-progress").hidden = true;
}

function switchTab(tab) {
  importState.tab = tab;
  for (const b of document.querySelectorAll("#dlg-import .tab")) {
    const actif = b.dataset.tab === tab;
    b.classList.toggle("is-active", actif);
    b.setAttribute("aria-selected", actif ? "true" : "false");
    b.tabIndex = actif ? 0 : -1;
  }
  for (const p of document.querySelectorAll(".tab-panel"))
    p.hidden = p.dataset.panel !== tab;

  // Le réglage d'horloge n'a de sens que pour un **logger de terrain**, dont
  // l'horodatage est l'heure naïve d'une horloge interne. Infoclimat (`dh_utc`)
  // et ROMMA (`timestamp`, epoch Unix) sont nativement en UTC : laisser le champ
  // visible inviterait à appliquer une correction de fuseau à des instants déjà
  // justes, ce qui les fausserait.
  // Le RC-5 lu en direct est bien un logger de terrain : son horloge interne est
  // naïve exactement comme dans un export xlsx, et le réglage reste nécessaire.
  //
  // La saisie manuelle est un troisième cas : l'heure tapée est bien naïve et
  // demande un fuseau, mais ni dérive d'horloge (il n'y a pas d'horloge) ni
  // numéro de série (il n'y a pas d'appareil). Elle porte donc son propre
  // sélecteur de fuseau, dans son panneau, et n'affiche pas ce bloc-ci.
  const utcNatif = tab === "infoclimat" || tab === "romma" || tab === "entry" || tab === "station";
  el("import-clock").hidden = utcNatif;

  // Ni fichier ni contenu collé pour la voie USB ni pour la saisie manuelle ni pour l'IA :
  // les données viennent du câble, du clavier, ou de la zone dédiée de l'assistant IA.
  const sansFichier = tab === "rc5" || tab === "entry" || tab === "ai" || tab === "station";
  el("import-file").closest(".file-field").hidden = sansFichier;
  el("import-content").closest("label").hidden = sansFichier;

  if (tab === "ai") {
    checkAiKeyStatus();
  }

  const libelles = {
    infoclimat:
      "Export Infoclimat (.json ou .csv) — ou collez son contenu ci-dessous",
    romma: "Archive ROMMA (archives_*.csv) — ou collez son contenu ci-dessous",
    auto: "Export ElitechLog (.xls / .xlsx, ou .txt / .csv) — ou collez le contenu ci-dessous",
    manual: "Fichier texte délimité (.txt, .csv) — ou collez le contenu ci-dessous",
  };
  el("import-file-label").textContent = libelles[tab] || libelles.manual;
  syncGroupeImport(tab);
}

// --------------------------------------------------- saisie manuelle sur site
//
// Voir `src-tauri/src/manual_entry.rs` pour ce que devient la saisie. Côté UI,
// une seule règle porte tout le reste : **une ligne de valeur par jour**. Une
// case unique appliquée à une plage de dates n'existe pas dans cet écran, parce
// qu'elle fabriquerait des relevés qui n'ont jamais été faits.

/** Bornes de saisie, alignées sur celles du backend (`manual_entry::MAX_DAYS`). */
const ENTRY_MAX_DAYS = 366;

/** `2026-01-14` → `mercredi 14 janvier 2026`. */
function fmtJour(iso) {
  const d = new Date(`${iso}T00:00:00Z`);
  return d.toLocaleDateString("fr-FR", {
    weekday: "long",
    day: "numeric",
    month: "long",
    year: "numeric",
    timeZone: "UTC",
  });
}

/** Toutes les dates de `debut` à `fin` incluses, au format `AAAA-MM-JJ`. */
function joursEntre(debut, fin) {
  const jours = [];
  const a = Date.parse(`${debut}T00:00:00Z`);
  const b = Date.parse(`${fin}T00:00:00Z`);
  if (!Number.isFinite(a) || !Number.isFinite(b) || b < a) return jours;
  for (let t = a; t <= b && jours.length <= ENTRY_MAX_DAYS; t += 86400000) {
    jours.push(new Date(t).toISOString().slice(0, 10));
  }
  return jours;
}

/**
 * (Re)construit la liste des jours à saisir.
 *
 * Les valeurs déjà tapées sont **conservées** quand la plage change : corriger
 * une date de fin ne doit pas effacer une demi-heure de saisie.
 */
function renderEntryDays() {
  const zone = el("entry-days");
  const dejaSaisi = new Map();
  for (const champ of zone.querySelectorAll("input[data-date]")) {
    if (champ.value !== "") dejaSaisi.set(champ.dataset.date, champ.value);
  }

  const debut = el("entry-from").value;
  const fin = el("entry-to").value || debut;
  clear(zone);

  if (!debut) {
    zone.appendChild(
      h("p", { class: "entry-empty", text: "Choisissez au moins un jour à saisir." })
    );
    el("entry-hint").textContent = "";
    return;
  }

  const jours = joursEntre(debut, fin);
  if (jours.length === 0) {
    zone.appendChild(
      h("p", {
        class: "entry-empty",
        text: "Le dernier jour est antérieur au premier : inversez les deux dates.",
      })
    );
    el("entry-hint").textContent = "";
    return;
  }
  if (jours.length > ENTRY_MAX_DAYS) {
    zone.appendChild(
      h("p", {
        class: "entry-empty",
        text: `Plage trop longue (${ENTRY_MAX_DAYS} jours au maximum en une saisie).`,
      })
    );
    el("entry-hint").textContent = "";
    return;
  }

  for (const jour of jours) {
    zone.appendChild(
      h(
        "div",
        { class: "entry-day" },
        h("span", { class: "entry-day-label", text: fmtJour(jour) }),
        h("input", {
          type: "number",
          step: "0.1",
          inputmode: "decimal",
          placeholder: "°C",
          "data-date": jour,
          value: dejaSaisi.get(jour) || "",
        })
      )
    );
  }

  const kind = el("entry-kind").selectedOptions[0].textContent.trim().toLowerCase();
  const tz = Number(el("entry-tz").value) || 0;
  el("entry-hint").textContent =
    `${jours.length} valeur(s) à saisir — le ${kind} de ${el("entry-start").value} à ` +
    `${el("entry-end").value}, pour chaque jour. Heures lues en ` +
    `${tz === 0 ? "UTC" : `UTC+${tz / 3600}`} ; chaque valeur sera datée du milieu de la ` +
    `plage, converti en UTC.`;
}

/** La saisie telle que le backend l'attend, ou `null` si rien n'est saisi. */
function entryPayload() {
  const jours = [...el("entry-days").querySelectorAll("input[data-date]")].map((champ) => ({
    date: champ.dataset.date,
    // Une case vide part à `null` : c'est le backend qui refuse la saisie en
    // nommant le jour manquant, plutôt que l'UI qui devine un zéro.
    temperature_celsius: champ.value === "" ? null : Number(champ.value),
  }));
  if (jours.length === 0) return null;
  return {
    start_time: el("entry-start").value,
    end_time: el("entry-end").value,
    value_kind: el("entry-kind").value,
    days: jours,
  };
}

/** Souligne le premier jour sans valeur, pour ne pas le chercher à l'oeil. */
function marquerJoursManquants() {
  let premier = null;
  for (const champ of el("entry-days").querySelectorAll("input[data-date]")) {
    const vide = champ.value === "";
    champ.closest(".entry-day").classList.toggle("is-missing", vide);
    if (vide && !premier) premier = champ;
  }
  if (premier) premier.focus();
  return premier === null;
}

// ------------------------------------------------- RC-5 branché en USB direct

/**
 * Recherche un logger sans lire sa mémoire.
 *
 * Un seul échange de 160 octets, contre 23 pour une lecture complète : c'est la
 * réponse rapide à « qu'est-ce qui est branché ? ». Le `rescan_rc5` préalable
 * fait oublier au fil de surveillance les ports qu'il a déjà écartés — c'est ce
 * qui permet de réessayer après avoir fermé ElitechLog, sans débrancher.
 */
async function searchRc5() {
  hideError(el("import-error"));
  const statut = el("rc5-status");
  statut.textContent = "Recherche en cours…";
  try {
    await invoke("rescan_rc5");
    const identite = await invoke("identify_rc5", { port_name: null });
    importState.rc5.portName = identite.port_name;
    statut.textContent =
      `${identite.model_label} trouvé sur ${identite.port_name} — ` +
      `n° ${identite.logger_serial || "inconnu"}, ` +
      `${fmtNumber(identite.rec_count)} mesure(s) en mémoire, ` +
      `${identite.work_status_label}, cadence ${fmtDuration(identite.sample_interval_seconds)}.`;
  } catch (e) {
    importState.rc5.portName = null;
    statut.textContent = "";
    showError(el("import-error"), String(e));
  }
}

/**
 * Lit toute la mémoire du logger et montre ce qui en sort — **sans rien écrire**.
 *
 * Même intention que l'analyse d'un fichier, et même rendu : l'utilisateur voit
 * la période, un échantillon et les sauts suspects avant de décider. La lecture
 * est gardée côté backend ; le bouton « Importer » la reprendra telle quelle.
 */
async function readRc5() {
  hideError(el("import-error"));
  const box = el("rc5-analysis");
  const statut = el("rc5-status");
  const bouton = el("btn-rc5-read");
  bouton.disabled = true;
  statut.textContent = "Lecture du logger… (une dizaine de secondes pour une mémoire pleine)";
  try {
    const a = await invoke("read_rc5", { port_name: importState.rc5.portName });
    importState.rc5.readingId = a.reading_id;
    importState.rc5.portName = a.port_name;
    clear(box);

    const kv = h("dl", { class: "kv" });
    const row = (k, v) => {
      kv.appendChild(h("dt", { text: k }));
      kv.appendChild(h("dd", { text: v }));
    };

    row("Voie", `Lecture USB directe (${a.port_name}) — sans ElitechLog`);
    row("Modèle", a.model_label);
    row("N° de série lu", a.logger_serial || "non transmis");
    row("Description", a.user_info || "—");
    row("État du logger", a.work_status_label);
    row("Mesures lues", fmtNumber(a.measurement_count));
    row("Période brute (non corrigée)", `${a.raw_period_start || "—"} → ${a.raw_period_end || "—"}`);
    row("Cadence annoncée", fmtDuration(a.sample_interval_seconds));
    row("Horloge du logger", a.device_clock || "illisible");
    row("Seuils d'alarme configurés", `${fmtTemp(a.lower_limit_celsius)} → ${fmtTemp(a.upper_limit_celsius)}`);
    row(
      "Correction interne de l'appareil",
      a.temperature_calibration === 0
        ? "aucune"
        : `${fmtTemp(a.temperature_calibration)}, déjà comprise dans les valeurs`
    );
    row(
      "Ordre des dates",
      "sans objet — les instants sont calculés à partir du départ et de la cadence"
    );

    box.appendChild(kv);

    // Le numéro complet figure sur l'étiquette du logger et dans un export xlsx,
    // mais le protocole n'en transmet que dix caractères. Le dire ici évite que
    // l'utilisateur croie à une erreur de lecture.
    if (a.logger_serial) {
      box.appendChild(
        h("p", {
          class: "muted",
          style: "margin-top:8px",
          text:
            `Le protocole ne transmet que 10 caractères de numéro de série : « ${a.logger_serial} ». ` +
            `Si l'étiquette du logger en porte davantage (« EF… »), saisissez le numéro complet ` +
            `dans le champ « Numéro de série » ci-dessus — il fera foi.`,
        })
      );
    }

    // La dérive ne se déduit qu'une fois le fuseau du logger connu : on la
    // calcule ici, où ce choix est disponible, et on pré-remplit le champ.
    proposerDeriveRc5(a);

    if (a.sample.length > 0) {
      box.appendChild(
        h("p", { class: "muted", text: "Premières mesures interprétées :", style: "margin-top:8px" })
      );
      box.appendChild(
        h(
          "ul",
          { class: "warn-list", style: "background:#fff;color:inherit" },
          a.sample.map((m) =>
            h("li", {
              text: `n° ${m.source_line} · ${m.raw_timestamp} · ${fmtTemp(m.temperature_celsius)}`,
            })
          )
        )
      );
    }

    for (const w of a.read_warnings)
      box.appendChild(h("p", { class: "banner", text: w.message }));

    if (a.anomalies.length > 0) box.appendChild(anomalyList(a.anomalies, a.jump_thresholds));

    box.hidden = false;
    statut.textContent = `${fmtNumber(a.measurement_count)} mesure(s) lues. Vérifiez, puis importez.`;
  } catch (e) {
    importState.rc5.readingId = null;
    box.hidden = true;
    statut.textContent = "";
    showError(el("import-error"), String(e));
  } finally {
    bouton.disabled = false;
  }
}

/**
 * Pré-remplit la dérive d'horloge à partir de l'écart mesuré.
 *
 * Le backend ne fournit que `device_clock − UTC` : il ignore le fuseau réglé sur
 * le logger, et deviner un fuseau n'est pas son rôle. C'est ici, où le choix de
 * l'utilisateur est disponible, que l'écart devient une dérive — d'où le
 * recalcul à chaque changement du menu de fuseau.
 */
function proposerDeriveRc5(analyse) {
  if (analyse.clock_offset_vs_utc_seconds === null) return;
  const appliquer = () => {
    const tz = Number(el("import-tz").value) || 0;
    el("import-drift").value = String(analyse.clock_offset_vs_utc_seconds - tz);
    updateOffsetHint();
  };
  appliquer();
  // Remplace l'éventuel écouteur d'une lecture précédente : sans cela, deux
  // lectures successives poseraient deux calculs concurrents sur le même champ.
  el("import-tz").onchange = appliquer;
}

async function onFileChosen(event) {
  const file = event.target.files && event.target.files[0];
  if (!file) return;
  const charge = await chargerFichierImport(file);
  if (!charge) return;
  await aiguillerSelonFormat(file.name, charge);
}

/**
 * Charge un fichier dans l'import (sélecteur ou dépôt sur la fenêtre).
 * Renvoie `{ estZip, texte }`, ou `null` si la lecture a échoué — l'erreur
 * reste alors affichée dans la fenêtre d'import.
 */
async function chargerFichierImport(file) {
  hideError(el("import-error"));
  try {
    // Le format est déterminé par le **contenu**, jamais par l'extension : un
    // export ElitechLog s'appelle `.xls` alors que c'est du xlsx, et un CSV
    // Infoclimat renommé doit rester importable.
    const octets = new Uint8Array(await file.arrayBuffer());
    importState.fileName = file.name;
    const estZip = looksLikeXlsx(octets);

    if (estZip) {
      // Gardé en `Uint8Array` (1 octet par octet) : un `Array` de nombres
      // coûte 8 octets par élément, soit ×8 sur un classeur de plusieurs Mo.
      // L'IPC de Tauri 2 convertit un `Uint8Array` imbriqué en tableau JSON
      // au moment de l'appel (process-ipc-message-fn.js) : même charge utile
      // qu'avant côté Rust (`Vec<u8>`).
      importState.bytes = octets;
      // Le `<textarea>` ne peut pas montrer un zip : il porte une note, et la
      // validation du formulaire sait qu'un classeur passe par `bytes`.
      el("import-content").value =
        `[classeur xlsx : ${file.name} — ${fmtNumber(file.size)} octets]\n` +
        `Le contenu binaire n'est pas affichable ici. Cliquez sur « Analyser le fichier ».`;
      switchTab("auto");
      toast(`« ${file.name} » reconnu comme classeur ElitechLog (xlsx).`);
    } else {
      importState.bytes = null;
      el("import-content").value = await readFileAsText(file);
      toast(`« ${file.name} » chargé (${fmtNumber(file.size)} octets).`);
    }

    el("analysis").hidden = true;
    el("ic-analysis").hidden = true;
    el("romma-analysis").hidden = true;
    el("preview").hidden = true;
    el("mapping-fields").hidden = true;
    return { estZip, texte: estZip ? "" : importContent() };
  } catch (e) {
    showError(el("import-error"), `Lecture impossible de « ${file.name} » : ${e}`);
    return null;
  }
}

/**
 * Fichier déposé n'importe où sur la fenêtre (hors `.gws`, traité à part) :
 * ouvre l'import sur l'onglet deviné par `detect_import_format` (Rust).
 */
/** Fichiers déposés en attente : file d'attente, ou en attente d'un point. */
const depotState = { file: [], apresPoint: null };

/** Ouvre le fichier suivant de la file, s'il y en a un. */
function depotSuivant() {
  if (!depotState.file.length) return;
  const reste = depotState.file;
  depotState.file = [];
  setTimeout(() => deposerFichierImport(reste), 150);
}

async function deposerFichierImport(paths) {
  const chemins = paths.filter((p) => !p.toLowerCase().endsWith(".gws"));
  if (chemins.length === 0) return;
  const autreModale = [...document.querySelectorAll("dialog[open]")].some((d) => d.id !== "dlg-import");
  if (autreModale) {
    toast("Fermez d'abord la fenêtre ouverte, puis déposez à nouveau le fichier pour l'importer.", true);
    return;
  }
  if (!state.project || el("view-project").hidden) {
    toast("Ouvrez d'abord un projet pour y importer ce fichier.", true);
    return;
  }
  if (state.points.length === 0) {
    // Le fichier n'est pas perdu : il est repris dès que le point est créé.
    depotState.apresPoint = chemins;
    toast("Créez d'abord le point : le fichier déposé s'ouvrira ensuite.");
    el("btn-new-point").click();
    return;
  }
  // Plusieurs fichiers : un à la fois, le suivant s'ouvre à la fermeture de
  // la fenêtre d'import (import fait ou annulé).
  depotState.file = chemins.slice(1);
  if (chemins.length > 1) {
    toast(`${chemins.length} fichiers déposés : ils s'ouvriront l'un après l'autre.`);
  }

  const path = chemins[0];
  const nom = path.split(/[\\/]/).pop();
  const ext = (nom.split(".").pop() || "").toLowerCase();
  let octets;
  try {
    octets = await invoke("read_dropped_file", { path });
  } catch (err) {
    toast(String(err), true);
    return;
  }
  const MIME = { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", webp: "image/webp" };
  const file = new File([octets], nom, { type: MIME[ext] || "" });

  // Une image ne se lit que par l'assistant IA.
  if (MIME[ext]) {
    await openImportDialog("ai");
    handleAiFiles([file]);
    return;
  }

  if (!el("dlg-import").open) await openImportDialog();
  const charge = await chargerFichierImport(file);
  if (!charge) return;
  await aiguillerSelonFormat(nom, charge);
}

/**
 * Ouvre l'onglet deviné par `detect_import_format` (Rust) pour un fichier déjà
 * chargé par `chargerFichierImport` — même chemin pour le sélecteur et le dépôt.
 */
async function aiguillerSelonFormat(nom, charge) {
  let verdict;
  try {
    verdict = await invoke("detect_import_format", {
      file_name: nom,
      is_zip: charge.estZip,
      text: charge.texte.slice(0, 65536),
    });
  } catch (err) {
    showError(el("import-error"), String(err));
    return;
  }
  switchTab(verdict.onglet);
  el("import-format-detecte").textContent = verdict.reconnu
    ? `« ${nom} » : ${verdict.libelle}.`
    : `« ${nom} » : format non reconnu.`;
  if (verdict.reconnu) {
    // Plus de bouton « Analyser » à cliquer : l'aperçu vient tout seul.
    const analyse = { auto: "btn-analyze", infoclimat: "btn-analyze-ic", romma: "btn-analyze-romma", manual: "btn-preview" }[verdict.onglet];
    if (analyse) el(analyse).click();
  } else {
    showError(
      el("import-error"),
      `Format non reconnu pour « ${nom} ». Ouvert dans l'onglet « Manuel (colonnes mappées) » : ` +
        "affichez l'aperçu et désignez les colonnes. Vous pouvez aussi passer par l'onglet " +
        "« Assistant IA », qui lit les fichiers de format inconnu."
    );
  }
}

async function analyzeFile() {
  // Un classeur xlsx n'est pas du texte délimité : il a son propre parseur, son
  // propre résumé, et rien à faire des notions de séparateur ou d'ordre
  // jour/mois. L'aiguillage se fait sur ce qui a été chargé, pas sur un choix
  // que l'utilisateur aurait à faire.
  if (importState.bytes) return analyzeElitechXlsx();

  hideError(el("import-error"));
  const box = el("analysis");
  try {
    const a = await invoke("analyze_elitech", { content: importContent() });
    clear(box);

    const layout = a.layout;
    const kv = h("dl", { class: "kv" });
    const row = (k, v) => {
      kv.appendChild(h("dt", { text: k }));
      kv.appendChild(h("dd", { text: v }));
    };
    row("Mesures lisibles", fmtNumber(a.measurement_count));
    row("Lignes écartées", fmtNumber(a.warning_count));
    row("Période brute (non corrigée)", `${a.raw_period_start || "—"} → ${a.raw_period_end || "—"}`);
    row("Séparateur détecté", layout.delimiter);
    row(
      "Détection des colonnes",
      layout.positional_fallback
        ? "par le contenu (aucun intitulé reconnu)"
        : `par les intitulés (ligne ${layout.header_line_number})`
    );
    row(
      "Colonne horodatage",
      layout.timestamp.Single !== undefined
        ? `n° ${layout.timestamp.Single + 1}`
        : `date n° ${layout.timestamp.DateAndTime.date + 1}, heure n° ${layout.timestamp.DateAndTime.time + 1}`
    );
    row("Colonne température", `n° ${layout.temperature_column + 1}`);
    row(
      "Colonne humidité",
      layout.humidity_column === null ? "aucune" : `n° ${layout.humidity_column + 1}`
    );
    row("N° de série lu", a.metadata.logger_serial || "non trouvé");
    row("Intervalle lu", fmtDuration(a.metadata.sample_interval_seconds));

    box.appendChild(kv);

    if (a.sample.length > 0) {
      box.appendChild(
        h("p", { class: "muted", text: "Premières lignes interprétées :", style: "margin-top:8px" })
      );
      box.appendChild(
        h(
          "ul",
          { class: "warn-list", style: "background:#fff;color:inherit" },
          a.sample.map((m) =>
            h("li", {
              text: `ligne ${m.source_line} · ${m.raw_timestamp} · ${fmtTemp(m.temperature_celsius)}${
                m.humidity_percent === null ? "" : ` · ${m.humidity_percent} %`
              }`,
            })
          )
        )
      );
    }

    if (a.warnings.length > 0) box.appendChild(warningList(a.warnings, a.warning_count));

    box.hidden = false;
  } catch (e) {
    box.hidden = true;
    showError(el("import-error"), String(e));
  }
}

/**
 * Analyse d'un JSON Infoclimat avant import — mêmes intentions que
 * `analyzeFile` pour un Elitech : montrer ce qui a été compris avant d'écrire
 * quoi que ce soit en base.
 */
async function analyzeInfoclimat() {
  hideError(el("import-error"));
  const box = el("ic-analysis");
  try {
    const a = await invoke("analyze_infoclimat", {
      content: importContent(),
      station_id: el("ic-station").value.trim() || null,
    });
    clear(box);

    const kv = h("dl", { class: "kv" });
    const row = (k, v) => {
      kv.appendChild(h("dt", { text: k }));
      kv.appendChild(h("dd", { text: v }));
    };

    row("Station", a.station_id + (a.station && a.station.name ? ` — ${a.station.name}` : ""));
    if (a.available_stations.length > 1)
      row("Stations dans le fichier", a.available_stations.join(", "));
    row("Mesures lisibles", fmtNumber(a.measurement_count));
    row("Lignes écartées", fmtNumber(a.warning_count));
    row("Instants en double (ignorés)", fmtNumber(a.duplicate_count));
    row("Période (UTC)", `${fmtUtc(a.period_start_utc)} → ${fmtUtc(a.period_end_utc)}`);
    row("Cadence déduite", fmtDuration(a.sample_interval_seconds));
    row(
      "Position / altitude",
      a.station && a.station.latitude !== null
        ? `${a.station.latitude}, ${a.station.longitude}` +
            (a.station.elevation_m !== null ? ` · ${fmtNumber(a.station.elevation_m)} m` : "")
        : "—"
    );
    row("Signal radio (RSSI)", `${fmtNumber(a.with_rssi)} mesure(s) renseignée(s)`);
    row("Batterie (valeur brute)", `${fmtNumber(a.with_battery)} mesure(s) renseignée(s)`);
    if (a.unreadable_complements > 0)
      row("Champs « complements » illisibles", fmtNumber(a.unreadable_complements));
    row("Licence annoncée", a.data_license || "non précisée");

    box.appendChild(kv);

    if (a.sample.length > 0) {
      box.appendChild(
        h("p", { class: "muted", text: "Premières mesures interprétées :", style: "margin-top:8px" })
      );
      box.appendChild(
        h(
          "ul",
          { class: "warn-list", style: "background:#fff;color:inherit" },
          a.sample.map((m) =>
            h("li", {
              text:
                `${fmtUtc(m.timestamp_utc)} · ${fmtTemp(m.temperature_celsius)}` +
                (m.humidity_percent === null ? "" : ` · ${m.humidity_percent} %`) +
                (m.rssi_dbm === null ? "" : ` · ${m.rssi_dbm} dBm`) +
                (m.battery === null ? "" : ` · batt. ${m.battery} (unité indéterminée)`),
            })
          )
        )
      );
    }

    box.appendChild(h("p", { class: "attribution", text: a.attribution }));

    if (a.warnings.length > 0) box.appendChild(warningList(a.warnings, a.warning_count));

    box.hidden = false;
  } catch (e) {
    box.hidden = true;
    showError(el("import-error"), String(e));
  }
}

/**
 * Analyse d'un classeur ElitechLog (le format réel).
 *
 * Montre ce que le parseur a compris **avant** toute écriture : quelle feuille
 * il a retenue, quels intitulés, quelles métadonnées — et, surtout, les sauts de
 * température suspects, pour que l'utilisateur les découvre avant l'import et
 * non après.
 */
async function analyzeElitechXlsx() {
  hideError(el("import-error"));
  const box = el("analysis");
  try {
    const a = await invoke("analyze_elitech_xlsx", { bytes: importState.bytes });
    clear(box);

    const kv = h("dl", { class: "kv" });
    const row = (k, v) => {
      kv.appendChild(h("dt", { text: k }));
      kv.appendChild(h("dd", { text: v }));
    };

    row("Format reconnu", "Classeur xlsx (extension .xls trompeuse)");
    row("Feuille de données", a.data_sheet);
    row("Feuille de résumé", a.summary_sheet || "aucune");
    row("Feuilles du classeur", a.sheet_names.join(", "));
    row("Colonnes", a.header_labels.join(" · "));
    row("Mesures lisibles", fmtNumber(a.measurement_count));
    row("Lignes écartées", fmtNumber(a.warning_count));
    row(
      "Période brute (non corrigée)",
      `${a.raw_period_start || "—"} → ${a.raw_period_end || "—"}`
    );
    row("Modèle", a.metadata.device_model || "non trouvé");
    row("N° de série lu", a.metadata.logger_serial || "non trouvé");
    row("Type de capteur", a.metadata.sensor_type || "non précisé");
    row("Intervalle lu", fmtDuration(a.metadata.sample_interval_seconds));
    row(
      "Ordre des dates",
      "sans objet — l'horodatage est en ISO (AAAA-MM-JJ), donc non ambigu"
    );

    box.appendChild(kv);

    if (a.sample.length > 0) {
      box.appendChild(
        h("p", { class: "muted", text: "Premières lignes interprétées :", style: "margin-top:8px" })
      );
      box.appendChild(
        h(
          "ul",
          { class: "warn-list", style: "background:#fff;color:inherit" },
          a.sample.map((m) =>
            h("li", {
              text: `ligne ${m.source_line} · ${m.raw_timestamp} · ${fmtTemp(m.temperature_celsius)}`,
            })
          )
        )
      );
    }

    if (a.anomalies.length > 0) box.appendChild(anomalyList(a.anomalies, a.jump_thresholds));
    if (a.warnings.length > 0) box.appendChild(warningList(a.warnings, a.warning_count));

    box.hidden = false;
  } catch (e) {
    box.hidden = true;
    showError(el("import-error"), String(e));
  }
}

/**
 * Liste des sauts de température jugés trop brutaux.
 *
 * Volontairement **non bloquante** et formulée comme une question, pas comme un
 * verdict : un saut peut être un logger qu'on retire de son abri comme une vraie
 * rupture d'inversion — et c'est justement ce second cas qui intéresse la chasse
 * aux TAF. Aucune mesure n'est écartée ; c'est à l'utilisateur de trancher.
 */
function anomalyList(anomalies, seuils) {
  const critere = seuils
    ? `plus de ${fmtTemp(seuils.min_delta_c)} entre deux mesures séparées de moins de ` +
      `${Math.round(seuils.max_gap_seconds / 60)} min`
    : "écart trop rapide entre deux mesures";

  return h(
    "ul",
    { class: "warn-list" },
    h("li", {
      text:
        `${anomalies.length} saut(s) de température anormalement rapide(s) — critère : ${critere}. ` +
        `Les mesures sont conservées : vérifiez s'il s'agit du logger manipulé, ` +
        `ou d'un vrai phénomène (rafale de foehn, rupture d'inversion).`,
    }),
    anomalies.slice(0, 20).map((a) =>
      h("li", {
        text:
          `${a.from_timestamp} → ${a.to_timestamp} · ` +
          `${fmtTemp(a.from_celsius)} → ${fmtTemp(a.to_celsius)} ` +
          `(${a.delta_c > 0 ? "+" : ""}${fmtTemp(a.delta_c)} en ${Math.round(a.gap_seconds / 60)} min)` +
          (a.source_line ? ` · ligne ${a.source_line}` : ""),
      })
    )
  );
}

/**
 * Analyse d'une archive ROMMA avant import.
 *
 * Insiste sur deux choses que le fichier ne dit pas de lui-même : la **couverture
 * réelle** de chaque capteur (plusieurs ont été ajoutés en cours de route sur
 * neuf ans d'archive) et le fait que le rayonnement puisse venir de
 * `RadSolaireMax` faute de `RadSolaire`.
 */
async function analyzeRomma() {
  hideError(el("import-error"));
  const box = el("romma-analysis");
  try {
    const a = await invoke("analyze_romma", { content: importContent() });
    clear(box);

    const kv = h("dl", { class: "kv" });
    const row = (k, v) => {
      kv.appendChild(h("dt", { text: k }));
      kv.appendChild(h("dd", { text: v }));
    };

    row("Lignes de données", fmtNumber(a.total_data_lines));
    row("Mesures exploitables", fmtNumber(a.measurement_count));
    row(
      "Lignes écartées",
      `${fmtNumber(a.skipped_count)}` +
        (a.skipped_count > 0 ? " (sans température : une mesure ne peut pas s'en passer)" : "")
    );
    row("Instants en double (ignorés)", fmtNumber(a.duplicate_count));
    row("Période (UTC)", `${fmtUtc(a.period_start_utc)} → ${fmtUtc(a.period_end_utc)}`);
    row("Cadence déduite", fmtDuration(a.sample_interval_seconds));
    row("Colonnes", a.columns.map((c) => c.trim()).join(" · "));
    row(
      "Unités lues dans le fichier",
      a.units.length > 0
        ? a.units.map(([c, u]) => `${c.trim()} : ${u}`).join(" · ")
        : "aucune ligne d'unités"
    );
    row("Horodatage", "colonne « timestamp » (epoch Unix, UTC) — aucune correction appliquée");

    const n = a.measurement_count || 1;
    const part = (compte) => `${fmtNumber(compte)} (${Math.round((compte * 100) / n)} %)`;
    const c = a.field_counts;
    row(
      "Couverture des capteurs",
      `humidité ${part(c.humidity)} · pression ${part(c.pressure)} · ` +
        `vent ${part(c.wind_avg)} · rafale ${part(c.wind_gust)} · ` +
        `direction ${part(c.wind_direction)} · pluie ${part(c.rain)} · ` +
        `rayonnement ${part(c.solar)}`
    );

    box.appendChild(kv);

    if (a.solar_from_max) {
      box.appendChild(
        h("p", {
          class: "hint",
          text:
            "La colonne « RadSolaire » est vide sur tout le fichier : c'est " +
            "« RadSolaireMax » qui alimente le rayonnement. Ce n'est pas la même grandeur " +
            "(un maximum sur l'intervalle, pas une moyenne) — à garder en tête si vous " +
            "exploitez ce champ plus tard.",
        })
      );
    }

    if (a.sample.length > 0) {
      box.appendChild(
        h("p", { class: "muted", text: "Premières mesures interprétées :", style: "margin-top:8px" })
      );
      box.appendChild(
        h(
          "ul",
          { class: "warn-list", style: "background:#fff;color:inherit" },
          a.sample.map((m) =>
            h("li", {
              text:
                `${fmtUtc(m.timestamp_utc)} · ${fmtTemp(m.temperature_celsius)}` +
                (m.humidity_percent === null ? "" : ` · ${m.humidity_percent} %`) +
                (m.pressure_hpa === null ? "" : ` · ${m.pressure_hpa} hPa`) +
                (m.wind_avg_kmh === null ? "" : ` · vent ${m.wind_avg_kmh} km/h`) +
                (m.wind_direction_degrees === null ? "" : ` (${m.wind_direction_degrees}°)`) +
                (m.rain_mm === null ? "" : ` · ${m.rain_mm} mm`) +
                (m.solar_radiation === null ? "" : ` · ${m.solar_radiation} W/m²`),
            })
          )
        )
      );
    }

    if (a.warnings.length > 0) box.appendChild(warningList(a.warnings, a.skipped_count));

    box.hidden = false;
  } catch (e) {
    box.hidden = true;
    showError(el("import-error"), String(e));
  }
}

function warningList(warnings, total) {
  return h(
    "ul",
    { class: "warn-list" },
    h("li", {
      text: `${total} ligne(s) écartée(s)${total > warnings.length ? ` (${warnings.length} détaillées)` : ""} :`,
    }),
    warnings.map((w) => h("li", { text: `ligne ${w.line_number} — ${w.reason} — « ${w.raw_line} »` }))
  );
}

async function showPreview() {
  hideError(el("import-error"));
  const box = el("preview");
  try {
    const p = await invoke("preview_delimited", {
      content: importContent(),
      delimiter: el("map-delimiter").value || null,
      max_rows: 12,
    });
    importState.previewDelimiter = p.delimiter;

    clear(box);
    box.appendChild(
      h("p", {
        class: "muted",
        text: `Séparateur : ${p.delimiter} · ${p.column_count} colonne(s) · ${p.total_lines} ligne(s) au total`,
      })
    );

    const table = h("table", {});
    table.appendChild(
      h(
        "tr",
        {},
        Array.from({ length: p.column_count }, (_, i) => h("th", { text: `Colonne ${i + 1}` }))
      )
    );
    for (const row of p.rows) {
      table.appendChild(
        h(
          "tr",
          {},
          Array.from({ length: p.column_count }, (_, i) => h("td", { text: row[i] ?? "" }))
        )
      );
    }
    box.appendChild(h("div", { class: "preview-scroll" }, table));
    box.hidden = false;

    fillMappingSelects(p);
  } catch (e) {
    box.hidden = true;
    el("mapping-fields").hidden = true;
    showError(el("import-error"), String(e));
  }
}

function fillMappingSelects(preview) {
  const build = (id, allowNone) => {
    const select = el(id);
    clear(select);
    if (allowNone) select.appendChild(h("option", { value: "" }, "— aucune —"));
    for (let i = 0; i < preview.column_count; i++)
      select.appendChild(h("option", { value: String(i) }, `Colonne ${i + 1}`));
    return select;
  };

  const dt = build("map-datetime", false);
  const time = build("map-time", true);
  const temp = build("map-temp", false);
  const hum = build("map-hum", true);

  const s = preview.suggested;
  if (s) {
    dt.value = String(s.datetime_column);
    time.value = s.time_column === null ? "" : String(s.time_column);
    temp.value = String(s.temperature_column);
    hum.value = s.humidity_column === null ? "" : String(s.humidity_column);
    el("map-skip").value = String(s.skip_lines);
    el("map-header").checked = s.has_header;
  }

  el("mapping-fields").hidden = false;
}

// -------------------------------------------------- Assistant IA (Fichiers & Images)

async function checkAiKeyStatus() {
  try {
    const settings = await invoke("agent_settings");
    const hasKey = settings && settings.has_key;
    const warning = el("ai-key-warning");
    if (warning) warning.hidden = !!hasKey;
  } catch {
    // Si la commande échoue, ne bloque pas l'écran
  }
}

function handleAiFiles(fileList) {
  if (!fileList || fileList.length === 0) return;

  for (const file of Array.from(fileList)) {
    const name = file.name.toLowerCase();
    const isImage = file.type.startsWith("image/") || /\.(png|jpe?g|webp)$/i.test(name);
    const isText =
      file.type.startsWith("text/") ||
      /\.(csv|tsv|json|txt|dat|log)$/i.test(name) ||
      file.type === "application/json";

    if (!isImage && !isText) {
      toast(`Fichier ignoré car format non supporté : ${file.name}`, true);
      continue;
    }

    // Évite les doublons exacts par nom et taille
    if (importState.ai.files.some((f) => f.name === file.name && f.size === file.size)) {
      continue;
    }

    importState.ai.files.push({
      file,
      name: file.name,
      size: file.size,
      type: isImage ? "image" : "text",
      status: "pending", // "pending" | "analyzing" | "done" | "error"
      content: null,
      base64: null,
      mimeType: file.type || (isImage ? "image/png" : "text/plain"),
      result: null,
      error: null,
    });
  }

  renderAiQueue();
}

function renderAiQueue() {
  const queueWrap = el("ai-files-queue");
  const queueList = el("ai-queue-list");
  const queueTitle = el("ai-queue-title");
  clear(queueList);

  const files = importState.ai.files;
  if (files.length === 0) {
    queueWrap.hidden = true;
    el("ai-review-container").hidden = true;
    return;
  }

  queueWrap.hidden = false;
  queueTitle.textContent = `Fichiers & Images (${files.length})`;

  files.forEach((item, index) => {
    const isSelected = index === importState.ai.selectedIndex;
    const icon = item.type === "image" ? "image" : "fichier";
    const statusLabels = {
      pending: "En attente",
      analyzing: "Analyse…",
      done: "Prêt",
      error: "Erreur",
    };

    const li = h(
      "li",
      {
        class: `ai-queue-item ${isSelected ? "is-selected" : ""} ${item.status}`,
        onclick: () => {
          importState.ai.selectedIndex = index;
          renderAiQueue();
          if (item.result) {
            renderAiReview(item);
          }
        },
      },
      h("span", { class: "ai-item-icon" }, icone(icon)),
      h(
        "div",
        { class: "ai-item-info" },
        h("span", { class: "ai-item-name", text: item.name }),
        h(
          "span",
          { class: "ai-item-meta", text: `${fmtNumber(item.size)} octets · ${statusLabels[item.status]}` }
        )
      ),
      h("button", {
        type: "button",
        class: "btn btn-sm btn-ghost ai-item-remove",
        "aria-label": "Retirer ce fichier",
        title: "Retirer ce fichier",
        onclick: (e) => {
          e.stopPropagation();
          importState.ai.files.splice(index, 1);
          if (importState.ai.selectedIndex >= importState.ai.files.length) {
            importState.ai.selectedIndex = Math.max(0, importState.ai.files.length - 1);
          }
          renderAiQueue();
          if (importState.ai.files[importState.ai.selectedIndex]?.result) {
            renderAiReview(importState.ai.files[importState.ai.selectedIndex]);
          } else {
            el("ai-review-container").hidden = true;
          }
        },
      }, icone("croix"))
    );
    queueList.appendChild(li);
  });
}

async function runAiAnalysis() {
  const files = importState.ai.files;
  if (files.length === 0) {
    toast("Aucun fichier à analyser. Déposez d'abord des fichiers ou images.", true);
    return;
  }

  hideError(el("import-error"));
  const userInstruction = el("ai-instruction").value.trim() || null;
  const loading = el("ai-loading");
  const loadingText = el("ai-loading-text");
  const btnAnalyze = el("btn-ai-analyze-all");

  btnAnalyze.disabled = true;
  loading.hidden = false;

  for (let i = 0; i < files.length; i++) {
    const item = files[i];
    if (item.status === "done") continue;

    item.status = "analyzing";
    renderAiQueue();
    loadingText.textContent = `Analyse IA de « ${item.name} » (${i + 1}/${files.length})…`;

    try {
      if (item.type === "image") {
        if (!item.base64) {
          const dataUrl = await new Promise((resolve, reject) => {
            const reader = new FileReader();
            reader.onload = () => resolve(reader.result);
            reader.onerror = reject;
            reader.readAsDataURL(item.file);
          });
          const parts = dataUrl.split(",");
          item.base64 = parts[1];
          item.dataUrl = dataUrl;
        }

        const res = await invoke("ai_analyze_vision", {
          payload: {
            file_name: item.name,
            image_base64: item.base64,
            mime_type: item.mimeType || "image/png",
            user_instruction: userInstruction,
          },
        });

        item.result = res;
        item.status = "done";
      } else {
        if (!item.content) {
          item.content = await readFileAsText(item.file);
        }

        const res = await invoke("ai_analyze_file", {
          payload: {
            file_name: item.name,
            content: item.content,
            user_instruction: userInstruction,
          },
        });

        item.result = res;
        item.status = "done";
      }
    } catch (e) {
      item.status = "error";
      item.error = String(e);
      showError(el("import-error"), `Erreur sur « ${item.name} » : ${e}`);
    }
  }

  loading.hidden = true;
  btnAnalyze.disabled = false;
  renderAiQueue();

  const current = files[importState.ai.selectedIndex] || files[0];
  if (current && current.result) {
    renderAiReview(current);
  }
}

function renderAiReview(item) {
  if (!item || !item.result) return;
  const container = el("ai-review-container");
  container.hidden = false;

  const badge = el("ai-review-badge");
  const explanation = el("ai-review-explanation");
  const warningsBox = el("ai-review-warnings");

  const confPercent = Math.round((item.result.confidence || 0.9) * 100);
  badge.textContent =
    item.type === "image"
      ? `Capture d'écran (confiance ~${confPercent}%)`
      : `Fichier structuré (confiance ~${confPercent}%)`;
  badge.className = `badge ${confPercent >= 80 ? "badge-success" : "badge-warn"}`;

  explanation.textContent = item.result.explanation || "Structure analysée avec succès.";

  if (item.result.warnings && item.result.warnings.length > 0) {
    warningsBox.hidden = false;
    clear(warningsBox);
    warningsBox.appendChild(h("strong", { text: "Remarques de l'IA :" }));
    const ul = h("ul", { class: "warn-list", style: "margin-top:4px;" });
    for (const w of item.result.warnings) {
      ul.appendChild(h("li", { text: w }));
    }
    warningsBox.appendChild(ul);
  } else {
    warningsBox.hidden = true;
  }

  if (item.type === "image") {
    el("ai-vision-side-by-side").hidden = false;
    el("ai-text-review").hidden = true;

    const img = el("ai-vision-img-preview");
    img.src = item.dataUrl || `data:${item.mimeType};base64,${item.base64}`;

    const tbody = el("ai-vision-tbody");
    clear(tbody);

    const rows = item.result.rows || [];
    rows.forEach((r, idx) => {
      const tr = h(
        "tr",
        {},
        h("td", { class: "mono", text: String(r.source_line || idx + 1) }),
        h(
          "td",
          {},
          h("input", {
            type: "text",
            class: "ai-cell-edit mono",
            value: r.raw_timestamp || "",
            "data-field": "timestamp",
            "data-row": String(idx),
          })
        ),
        h(
          "td",
          {},
          h("input", {
            type: "number",
            step: "0.1",
            class: "ai-cell-edit mono",
            value:
              r.temperature !== null && r.temperature !== undefined
                ? String(r.temperature)
                : "",
            "data-field": "temperature",
            "data-row": String(idx),
          })
        ),
        h(
          "td",
          {},
          h("input", {
            type: "number",
            step: "0.1",
            class: "ai-cell-edit mono",
            value:
              r.humidity !== null && r.humidity !== undefined
                ? String(r.humidity)
                : "",
            "data-field": "humidity",
            "data-row": String(idx),
          })
        )
      );
      tbody.appendChild(tr);
    });
  } else {
    el("ai-vision-side-by-side").hidden = true;
    el("ai-text-review").hidden = false;

    const mapping = item.result.mapping;
    importState.ai.currentMapping = { ...mapping };

    el("ai-map-delimiter").value = mapping.delimiter || ";";
    el("ai-map-skip").value = String(mapping.skip_lines || 0);

    const totalCols = Math.max(
      mapping.datetime_column + 1,
      mapping.temperature_column + 1,
      (mapping.humidity_column || 0) + 1,
      8
    );

    const fillSelect = (id, selectedVal, allowNone) => {
      const sel = el(id);
      clear(sel);
      if (allowNone) sel.appendChild(h("option", { value: "" }, "— aucune —"));
      for (let c = 0; c < totalCols; c++) {
        sel.appendChild(h("option", { value: String(c) }, `Col ${c + 1}`));
      }
      sel.value = selectedVal !== null && selectedVal !== undefined ? String(selectedVal) : "";
    };

    fillSelect("ai-map-dt", mapping.datetime_column, false);
    fillSelect("ai-map-temp", mapping.temperature_column, false);
    fillSelect("ai-map-hum", mapping.humidity_column, true);

    renderAiTextTable(item.result.preview_rows || []);
  }
}

function renderAiTextTable(rows) {
  const tbody = el("ai-text-tbody");
  clear(tbody);
  if (!rows || rows.length === 0) {
    tbody.appendChild(
      h(
        "tr",
        {},
        h("td", {
          colspan: "4",
          class: "muted",
          style: "text-align:center;",
          text: "Aucune ligne de mesure reconnue",
        })
      )
    );
    return;
  }

  for (const r of rows) {
    tbody.appendChild(
      h(
        "tr",
        {},
        h("td", { class: "mono", text: String(r.source_line) }),
        h("td", { class: "mono", text: r.raw_timestamp }),
        h("td", { class: "mono", text: fmtTemp(r.temperature) }),
        h("td", { class: "mono", text: r.humidity !== null ? `${r.humidity} %` : "—" })
      )
    );
  }
}

async function recalculateAiPreview() {
  const item = importState.ai.files[importState.ai.selectedIndex];
  if (!item || item.type !== "text" || !item.content) return;

  hideError(el("import-error"));
  const humVal = el("ai-map-hum").value;
  const mapping = {
    file_type: "custom",
    delimiter: el("ai-map-delimiter").value || ";",
    skip_lines: Number(el("ai-map-skip").value) || 0,
    has_header: Number(el("ai-map-skip").value) > 0,
    datetime_column: Number(el("ai-map-dt").value) || 0,
    time_column: null,
    temperature_column: Number(el("ai-map-temp").value) || 1,
    humidity_column: humVal === "" ? null : Number(humVal),
    pressure_column: null,
    wind_speed_column: null,
    rain_column: null,
    json_records_path: null,
    json_time_field: null,
    json_temp_field: null,
    json_humidity_field: null,
    explanation: item.result?.mapping?.explanation || "Mapping ajusté manuellement",
    confidence: 1.0,
  };

  importState.ai.currentMapping = mapping;

  try {
    const res = await invoke("ai_preview_parsed", {
      content: item.content,
      mapping,
    });
    item.result.preview_rows = res.preview_rows;
    item.result.warnings = res.warnings;
    renderAiTextTable(res.preview_rows);
    toast("Aperçu recalculé.");
  } catch (e) {
    showError(el("import-error"), `Impossible de recalculer l'aperçu : ${e}`);
  }
}

function collectVisionTableRows() {
  const tbody = el("ai-vision-tbody");
  const trs = tbody.querySelectorAll("tr");
  const rows = [];

  trs.forEach((tr, idx) => {
    const tsInput = tr.querySelector('input[data-field="timestamp"]');
    const tempInput = tr.querySelector('input[data-field="temperature"]');
    const humInput = tr.querySelector('input[data-field="humidity"]');

    const ts = tsInput ? tsInput.value.trim() : "";
    const tempStr = tempInput ? tempInput.value.trim() : "";
    const humStr = humInput ? humInput.value.trim() : "";

    if (!ts || !tempStr) return;

    const temp = parseFloat(tempStr.replace(",", "."));
    const hum = humStr ? parseFloat(humStr.replace(",", ".")) : null;

    if (!isNaN(temp)) {
      rows.push({
        source_line: idx + 1,
        raw_timestamp: ts,
        temperature: temp,
        humidity: !isNaN(hum) ? hum : null,
        pressure: null,
        wind_speed: null,
        rain: null,
      });
    }
  });

  return rows;
}

async function submitImport(event) {
  event.preventDefault();
  hideError(el("import-error"));
  el("import-report").hidden = true;
  if (importState.tab === "station") return recupererStations();

  // La voie USB n'a ni fichier ni texte : ses données viennent du câble.
  const depuisLogger = importState.tab === "rc5";
  if (depuisLogger && !importState.rc5.readingId) {
    showError(
      el("import-error"),
      "Cliquez d'abord sur « Lire les données du logger » : l'import reprend la lecture " +
        "que vous venez de vérifier, il ne relit pas l'appareil à l'aveugle."
    );
    return;
  }
  // La saisie manuelle n'a pas davantage de fichier : ses données sont dans le
  // formulaire. Elle a en revanche sa propre garde, qui dit *quel* jour manque.
  const saisieManuelle = importState.tab === "entry";
  if (saisieManuelle && !marquerJoursManquants()) {
    showError(
      el("import-error"),
      "Chaque jour saisi porte sa propre valeur : complétez le jour surligné. " +
        "Recopier la même température sur plusieurs jours fabriquerait des relevés " +
        "qui n'ont jamais été faits."
    );
    return;
  }

  const depuisAi = importState.tab === "ai";
  if (depuisAi) {
    const item = importState.ai.files[importState.ai.selectedIndex];
    if (!item || !item.result) {
      showError(
        el("import-error"),
        "Sélectionnez et analysez d'abord un fichier ou une image avec l'IA avant d'importer."
      );
      return;
    }
  }

  // Un classeur xlsx n'a pas de contenu texte : c'est `importState.bytes` qui
  // porte le fichier. Exiger du texte ici bloquerait l'import du format réel.
  if (!depuisLogger && !saisieManuelle && !depuisAi && !importState.bytes && !importContent().trim()) {
    showError(el("import-error"), "Choisissez un fichier ou collez son contenu.");
    return;
  }
  // …et réciproquement : un classeur n'a rien à faire dans un onglet qui lit du
  // texte. Le dire ici évite une erreur de parsing incompréhensible plus loin.
  if (importState.bytes && importState.tab !== "auto") {
    showError(
      el("import-error"),
      "Le fichier chargé est un classeur ElitechLog (xlsx). Il s'importe depuis " +
        "l'onglet « Automatique (Elitech) » ; pour un autre onglet, choisissez un fichier texte."
    );
    return;
  }
  if (!el("import-point").value) {
    showError(el("import-error"), "Sélectionnez un point de rattachement.");
    return;
  }

  const button = el("btn-do-import");
  const libelleBouton = button.textContent;
  button.disabled = true;
  button.textContent = "Import en cours…";
  afficherProgressionImport();

  try {
    let lancer;
    if (depuisLogger) {
      lancer = (force) =>
        invoke("import_rc5", {
          payload: {
            ...importCommon(),
            content: "",
            file_name: null,
            reading_id: importState.rc5.readingId,
            port_name: importState.rc5.portName,
            force_duplicate: force,
          },
        });
    } else if (saisieManuelle) {
      const saisie = entryPayload();
      if (!saisie) {
        showError(el("import-error"), "Choisissez au moins un jour à saisir.");
        return;
      }
      const tz = Number(el("entry-tz").value) || 0;
      lancer = (force) =>
        invoke("import_manual_entry", {
          payload: {
            point_id: el("import-point").value,
            content: "",
            file_name: null,
            role: el("import-role").value,
            clock_offset_seconds: -tz,
            raw_timestamp_was_utc: tz === 0,
            logger_serial: null,
            label: null,
            force_duplicate: force,
            entry: saisie,
          },
        });
    } else if (depuisAi) {
      const item = importState.ai.files[importState.ai.selectedIndex];
      if (item.type === "image") {
        const rows = collectVisionTableRows();
        if (rows.length === 0) {
          showError(el("import-error"), "Aucune ligne de mesure valide dans le tableau.");
          return;
        }
        lancer = (force) =>
          invoke("import_ai", {
            payload: {
              common: {
                ...importCommon(),
                file_name: item.name,
                content: "",
                force_duplicate: force,
              },
              source_kind: {
                kind: "DirectRows",
                rows,
              },
            },
          });
      } else {
        const mapping = importState.ai.currentMapping || item.result.mapping;
        lancer = (force) =>
          invoke("import_ai", {
            payload: {
              common: {
                ...importCommon(),
                file_name: item.name,
                content: item.content,
                force_duplicate: force,
              },
              source_kind: {
                kind: "TextMapped",
                mapping,
              },
            },
          });
      }
    } else if (importState.tab === "auto" && importState.bytes) {
      lancer = (force) =>
        invoke("import_elitech_xlsx", {
          payload: {
            ...importCommon(),
            content: "",
            bytes: importState.bytes,
            force_duplicate: force,
          },
        });
    } else if (importState.tab === "auto") {
      lancer = (force) =>
        invoke("import_elitech", {
          payload: { ...importCommon(), force_duplicate: force },
        });
    } else if (importState.tab === "romma") {
      lancer = (force) =>
        invoke("import_romma", {
          payload: {
            point_id: el("import-point").value,
            content: importContent(),
            file_name: importState.fileName,
            role: el("import-role").value,
            station_id: el("romma-station").value.trim() || null,
            label: null,
            force_duplicate: force,
          },
        });
    } else if (importState.tab === "infoclimat") {
      lancer = (force) =>
        invoke("import_infoclimat", {
          payload: {
            point_id: el("import-point").value,
            content: importContent(),
            file_name: importState.fileName,
            role: el("import-role").value,
            station_id: el("ic-station").value.trim() || null,
            label: null,
            append_to_existing: el("ic-append").checked,
            force_duplicate: force,
          },
        });
    } else {
      const timeValue = el("map-time").value;
      const humValue = el("map-hum").value;
      const delimiter = el("map-delimiter").value || importState.previewDelimiter;
      if (!delimiter) {
        showError(el("import-error"), "Affichez d'abord l'aperçu pour déterminer le séparateur.");
        return;
      }
      lancer = (force) =>
        invoke("import_manual", {
          payload: {
            ...importCommon(),
            force_duplicate: force,
            mapping: {
              delimiter,
              skip_lines: Number(el("map-skip").value) || 0,
              has_header: el("map-header").checked,
              datetime_column: Number(el("map-datetime").value),
              time_column: timeValue === "" ? null : Number(timeValue),
              temperature_column: Number(el("map-temp").value),
              humidity_column: humValue === "" ? null : Number(humValue),
            },
          },
        });
    }

    const report = await avecConfirmationDeReimport(lancer);
    if (!report) {
      toast("Import annulé : ce contenu est déjà enregistré sur ce point.");
      return;
    }

    renderReport(report);
    // Les jours saisis sont enregistrés : fermer ne perd plus rien.
    if (saisieManuelle) importState.entryDirty = false;
    memoriserOngletImport(importState.tab);
    await loadPoints();
    toast(`${fmtNumber(report.imported_count)} mesures importées sur « ${report.point_name} ».`);
  } catch (e) {
    // Dans la fenêtre, pas dans un toast : un message long doit rester lisible.
    showError(el("import-error"), String(e));
  } finally {
    masquerProgressionImport();
    button.textContent = libelleBouton;
    button.disabled = false;
  }
}

function renderReport(r) {
  const box = el("import-report");
  clear(box);
  box.className = "report is-ok";

  const kv = h("dl", { class: "kv" });
  const row = (k, v) => {
    kv.appendChild(h("dt", { text: k }));
    kv.appendChild(h("dd", { text: v }));
  };
  const utcNatif = isUtcNative(r);

  row("Point", r.point_name);
  row("Source", sourceLabel(r.source_type));
  row("Rôle", r.role === "Primary" ? "Principal" : "Secours");
  row("Mesures ajoutées", fmtNumber(r.imported_count));
  if (r.updated_count) row("Mesures corrigées", fmtNumber(r.updated_count));
  if (r.unchanged_count) row("Déjà présentes (ignorées)", fmtNumber(r.unchanged_count));
  if (r.duplicate_count) row("Instants répétés dans la source", fmtNumber(r.duplicate_count));
  row("Lignes écartées", fmtNumber(r.skipped_count));
  row("Période couverte (UTC)", `${fmtUtc(r.period_start_utc)} → ${fmtUtc(r.period_end_utc)}`);
  if (!utcNatif) {
    row("Décalage d'horloge appliqué", `${r.clock_offset_seconds} s`);
    row("N° de série", r.logger_serial || "—");
    row("Ordre des dates", describeDateOrder(r.date_order));
  } else {
    row(
      "Horodatage",
      r.source_type === "RommaImport"
        ? "UTC d'origine (colonne « timestamp », epoch Unix) — aucune correction appliquée"
        : "UTC d'origine (dh_utc) — aucune correction appliquée"
    );
  }
  row("Intervalle", fmtDuration(r.sample_interval_seconds));
  row("Minimum extrême", fmtTemp(r.stats.extreme_min_c));
  row("Moyenne des minimums quotidiens", fmtTemp(r.stats.mean_daily_min_c));
  row("Journées couvertes", fmtNumber(r.stats.day_count));
  if (r.data_license) row("Licence des données", r.data_license);

  box.appendChild(kv);

  if (r.attribution) box.appendChild(h("p", { class: "attribution", text: r.attribution }));

  // Le doute sur les dates est plus grave qu'une ligne écartée : on le met en
  // tête des avertissements, juste après le tableau récapitulatif.
  if (r.date_format_uncertain) {
    box.appendChild(
      h(
        "div",
        { class: "banner banner-warn" },
        h("strong", { text: "Format de date à confirmer" }),
        h("p", {
          text:
            "Ce fichier ne prouvait pas son ordre jour/mois. Ouvrez le détail du point " +
            "pour confirmer la lecture ou l'inverser — aucun réimport ne sera nécessaire.",
        })
      )
    );
  }

  // Un import forcé malgré l'alerte de ré-import doit rester lisible dans le
  // bilan : c'est la seule trace, une fois la boîte fermée, qu'une série jumelle
  // a été créée sciemment.
  if (r.forced_duplicate) {
    box.appendChild(
      h(
        "div",
        { class: "banner banner-warn" },
        h("strong", { text: "Série créée malgré un contenu déjà présent" }),
        h("p", {
          text:
            "Ce point porte désormais deux séries de contenu identique. Les " +
            "statistiques agrégées du point compteront ces mesures deux fois.",
        })
      )
    );
  }

  // Les sauts de température viennent après les avertissements de format : ce
  // ne sont pas des erreurs de lecture, ce sont des mesures bien lues dont on
  // doute qu'elles décrivent encore le site.
  if (r.anomalies && r.anomalies.length > 0) box.appendChild(anomalyList(r.anomalies, null));

  if (r.warnings.length > 0) box.appendChild(warningList(r.warnings, r.warning_count));
  box.hidden = false;
}

/** Résume un `DateOrderEvidence` en une phrase lisible. */
function describeDateOrder(e) {
  if (!e) return "—";
  const ordre = e.order === "DayFirst" ? "jj/mm" : "mm/jj";
  if (e.conflicting) {
    return `${ordre} — attention, le fichier se contredit (${e.day_first_hits} preuve(s) jj/mm, ${e.month_first_hits} mm/jj)`;
  }
  if (e.uncertain) return `${ordre} (supposé — aucune preuve dans le fichier)`;
  if (e.day_first_hits + e.month_first_hits > 0) {
    const exemple = e.samples && e.samples.length ? ` (ex. ${e.samples[0]})` : "";
    return `${ordre} — prouvé par le fichier${exemple}`;
  }
  if (e.iso_hits > 0) return "ISO (aaaa-mm-jj) — sans ambiguïté";
  return ordre;
}

// ---------------------------------------------------------------- démarrage

// ------------------------------------------------- relai SSH : clé d'hôte

/**
 * Affiche le résultat d'un test du relai. Trois issues demandent autre chose
 * qu'un message :
 *  - `unknown` : premier contact. Rien n'a été envoyé au serveur ; on montre
 *    l'empreinte et on demande de la comparer avant de l'accepter ;
 *  - `changed` : l'empreinte ne correspond plus. Refus, et une seule issue
 *    proposée — oublier l'ancienne, si le serveur a vraiment été réinstallé ;
 *  - le reste : réussite ou échec ordinaire.
 */
function afficherResultatSsh(box, r, input) {
  clear(box);
  if (r.ok) {
    box.className = "banner is-ok";
    texteAvecIcone(box, "ok", r.message);
    return;
  }
  if (r.host_key_status === "unknown") {
    box.className = "banner banner-warn";
    const accepter = h("button", {
      type: "button",
      class: "btn btn-sm btn-primary",
      text: "C'est la bonne empreinte : faire confiance",
      onclick: async () => {
        accepter.disabled = true;
        try {
          state.sshRelay = await invoke("trust_ssh_host_key", {
            host: input.host,
            port: input.port,
            fingerprint: r.host_key,
          });
          renderSshHostKey();
          await testSshRelayConnection();
        } catch (e) {
          accepter.disabled = false;
          toast(String(e), true);
        }
      },
    });
    box.append(
      h("strong", { text: "Premier contact avec ce serveur" }),
      h("p", { text: r.message }),
      h("p", null, "Empreinte présentée : ", h("code", { text: r.host_key })),
      h(
        "p",
        null,
        "Comparez-la avec celle du serveur, en tapant sur celui-ci : ",
        h("code", { text: "ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub" }),
        ". Si elles diffèrent, n'acceptez pas : quelqu'un se fait passer pour votre serveur."
      ),
      h("div", { class: "banner-actions" }, accepter)
    );
    return;
  }
  if (r.host_key_status === "changed") {
    box.className = "banner is-danger";
    const oublier = h("button", {
      type: "button",
      class: "btn btn-sm",
      text: "Le serveur a été réinstallé : oublier l'ancienne empreinte",
      onclick: forgetSshHostKey,
    });
    box.append(
      h("strong", { text: "La clé du serveur a changé" }),
      h("p", { text: r.message }),
      h("div", { class: "banner-actions" }, oublier)
    );
    return;
  }
  box.className = "banner is-danger";
  texteAvecIcone(box, "croix", "Échec du test SSH : " + r.message);
}

function renderSshHostKey() {
  const ligne = el("ssh-host-key-line");
  if (!ligne) return;
  const cle = state.sshRelay?.trusted_host_key;
  ligne.hidden = !cle;
  el("ssh-host-key").textContent = cle || "";
}

async function forgetSshHostKey() {
  try {
    state.sshRelay = await invoke("forget_ssh_host_key");
    renderSshHostKey();
    toast("Empreinte oubliée : le prochain test demandera de confirmer la nouvelle.");
  } catch (e) {
    toast(String(e), true);
  }
}

// ------------------------------------------------------- partage par code

/**
 * Partage de points par code à six caractères (voir `share.rs`).
 *
 * Deux gestes, deux modales : « Partager » (depuis un projet ouvert) dépose des
 * points et rend un code ; « Recevoir » (depuis la liste des projets ou un
 * projet ouvert) lit l'aperçu d'un code, puis importe. L'aperçu précède
 * toujours l'import : rien n'est écrit tant que l'utilisateur n'a pas vu d'où
 * viennent les points et combien il y en a.
 */
const partage = {
  /** Code dont l'aperçu est affiché — le seul que « Importer » peut importer. */
  apercuCode: null,
};

function ligneKv(dl, cle, valeur) {
  dl.appendChild(h("dt", { text: cle }));
  dl.appendChild(h("dd", { text: valeur }));
}

function openShareDialog() {
  if (!state.project) return;
  hideError(el("share-error"));
  el("share-step-choose").hidden = false;
  el("share-step-done").hidden = true;

  // Pré-cochés : la sélection de la carte si elle existe, sinon tout le projet.
  const points = state.points || [];
  const coches = new Set(state.selectedIds.length ? state.selectedIds : points.map((p) => p.id));
  const liste = el("share-points");
  clear(liste);
  for (const p of points) {
    liste.appendChild(
      h(
        "li",
        null,
        h(
          "label",
          null,
          h("input", { type: "checkbox", value: p.id, checked: coches.has(p.id) }),
          h("span", { text: p.name }),
          h("span", {
            class: "share-point-meta",
            text: `${p.category}${p.size ? " " + p.size : ""} · ${p.series_count || 0} série(s)`,
          })
        )
      )
    );
  }
  if (!points.length) liste.appendChild(h("li", { class: "muted", text: "Ce projet n'a encore aucun point." }));
  majChoixPeriode();
  el("dlg-share").showModal();
}

/** La période n'a de sens qu'avec les séries : masquée pour « points seuls ». */
function majChoixPeriode() {
  const avecSeries = document.querySelector('input[name="share-content"]:checked')?.value === "series";
  el("share-period-line").hidden = !avecSeries;
}

function cocherTousLesPointsPartages(coche) {
  for (const c of el("share-points").querySelectorAll('input[type="checkbox"]')) c.checked = coche;
}

async function createShare() {
  hideError(el("share-error"));
  const ids = [...el("share-points").querySelectorAll('input[type="checkbox"]:checked')].map((c) => c.value);
  if (!ids.length) {
    showError(el("share-error"), "Cochez au moins un point.");
    return;
  }
  const avecSeries = document.querySelector('input[name="share-content"]:checked')?.value === "series";
  const bouton = el("btn-share-create");
  bouton.disabled = true;
  bouton.textContent = "Envoi en cours…";
  try {
    const r = await invoke("share_create", {
      request: {
        project_id: state.project.id,
        point_ids: ids,
        include_series: avecSeries,
        ttl_days: Number(el("share-ttl").value),
        period_days: avecSeries && el("share-period").value ? Number(el("share-period").value) : null,
      },
    });
    el("share-code").textContent = r.display_code;
    const dl = el("share-done-details");
    clear(dl);
    ligneKv(dl, "Points", fmtNumber(r.point_count));
    if (r.series_count) {
      ligneKv(dl, "Séries", `${fmtNumber(r.series_count)} (${fmtNumber(r.measurement_count)} mesures)`);
    }
    ligneKv(dl, "Valable jusqu'au", fmtLocal(r.expires_at));
    el("share-step-choose").hidden = true;
    el("share-step-done").hidden = false;
  } catch (e) {
    showError(el("share-error"), String(e));
  } finally {
    bouton.disabled = false;
    bouton.textContent = "Créer le code";
  }
}

async function copyShareCode() {
  const code = el("share-code").textContent;
  try {
    await navigator.clipboard.writeText(code);
    toast(`Code ${code} copié.`);
  } catch {
    // Presse-papiers refusé : on sélectionne le code, un Ctrl+C suffit.
    const plage = document.createRange();
    plage.selectNodeContents(el("share-code"));
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(plage);
    toast("Copie automatique impossible : le code est sélectionné, faites Ctrl+C.");
  }
}

function openReceiveDialog() {
  partage.apercuCode = null;
  hideError(el("receive-error"));
  el("receive-code").value = "";
  el("receive-preview").hidden = true;
  el("btn-receive-import").disabled = true;

  // « Dans le projet ouvert » n'a de sens que depuis un projet ouvert.
  const dansUnProjet = !!state.project && !el("view-project").hidden;
  el("receive-target-current-line").hidden = !dansUnProjet;
  el("receive-target-current").textContent = dansUnProjet
    ? `Dans le projet ouvert, « ${state.project.name} »`
    : "";
  document.querySelector('input[name="receive-target"][value="new"]').checked = true;
  el("dlg-receive").showModal();
  el("receive-code").focus();
}

function oublierApercuPartage() {
  partage.apercuCode = null;
  el("btn-receive-import").disabled = true;
  el("receive-preview").hidden = true;
}

async function previewReceivedShare() {
  hideError(el("receive-error"));
  oublierApercuPartage();
  const code = el("receive-code").value.trim();
  if (!code) {
    showError(el("receive-error"), "Saisissez le code reçu.");
    return;
  }
  const bouton = el("btn-receive-preview");
  bouton.disabled = true;
  try {
    const p = await invoke("share_preview", { code });
    const dl = el("receive-details");
    clear(dl);
    ligneKv(dl, "Code", p.display_code);
    ligneKv(dl, "Projet d'origine", p.source_project || "non précisé");
    ligneKv(
      dl,
      "Contenu",
      p.mode === "points_series"
        ? `${fmtPointCount(p.point_count)} et ${fmtNumber(p.series_count)} série(s) de mesures`
        : fmtPointCount(p.point_count)
    );
    ligneKv(dl, "Partagé le", fmtLocal(p.created_at));
    ligneKv(dl, "Expire le", fmtLocal(p.expires_at));
    partage.apercuCode = p.code;
    el("receive-preview").hidden = false;
    el("btn-receive-import").disabled = false;
  } catch (e) {
    showError(el("receive-error"), String(e));
  } finally {
    bouton.disabled = false;
  }
}

async function importReceivedShare() {
  if (!partage.apercuCode) return;
  hideError(el("receive-error"));
  const versProjetOuvert =
    !!state.project && document.querySelector('input[name="receive-target"]:checked')?.value === "current";
  const target = versProjetOuvert
    ? { kind: "existing", project_id: state.project.id }
    : { kind: "new_project" };
  const bouton = el("btn-receive-import");
  bouton.disabled = true;
  bouton.textContent = "Import en cours…";
  try {
    const r = await invoke("share_import", { code: partage.apercuCode, target });
    el("dlg-receive").close();
    const detail = r.series_created
      ? `${fmtPointCount(r.points_created)}, ${fmtNumber(r.series_created)} série(s), ${fmtNumber(r.measurements_written)} mesures`
      : fmtPointCount(r.points_created);
    toast(`Partage importé dans « ${r.project_name} » : ${detail}.`);
    if (versProjetOuvert) {
      await loadPoints();
    } else {
      await loadProjects();
      await openProject(r.project_id);
    }
  } catch (e) {
    showError(el("receive-error"), String(e));
  } finally {
    bouton.disabled = !partage.apercuCode;
    bouton.textContent = "Importer";
  }
}

async function loadIssuedShares() {
  try {
    renderIssuedShares(await invoke("share_list_issued"));
  } catch (e) {
    console.warn("Partages émis indisponibles :", e);
  }
}

const LIBELLES_STATUT_PARTAGE = { active: "actif", expired: "expiré", revoked: "révoqué" };

function renderIssuedShares(liste) {
  const ul = el("issued-shares");
  if (!ul) return;
  clear(ul);
  el("issued-shares-empty").hidden = liste.length > 0;
  for (const s of liste) {
    const contenu = s.series_count
      ? `${fmtPointCount(s.point_count)}, ${fmtNumber(s.series_count)} série(s)`
      : fmtPointCount(s.point_count);
    const etat = s.status === "active" ? `jusqu'au ${fmtLocal(s.expires_at)}` : LIBELLES_STATUT_PARTAGE[s.status];
    const li = h(
      "li",
      { class: s.status === "active" ? null : "is-inactive" },
      h("span", { class: "issued-code", text: s.display_code }),
      h("span", { class: "issued-meta", text: `${s.project_name || "projet supprimé"} · ${contenu} · ${etat}` })
    );
    if (s.status === "active") {
      const bouton = h("button", { type: "button", class: "btn btn-sm btn-danger", text: "Révoquer" });
      bouton.addEventListener("click", () => revokeIssuedShare(s, bouton));
      li.appendChild(bouton);
    }
    ul.appendChild(li);
  }
}

/**
 * Révocation en deux clics sur le même bouton, plutôt qu'un `confirm()` —
 * peu fiable dans WebView2 — ou qu'une modale de plus par-dessus les
 * Paramètres. Sans second clic dans les cinq secondes, le bouton se réarme.
 */
async function revokeIssuedShare(s, bouton) {
  if (bouton.dataset.confirm !== "1") {
    bouton.dataset.confirm = "1";
    bouton.textContent = "Confirmer la révocation";
    setTimeout(() => {
      if (bouton.isConnected && !bouton.disabled) {
        bouton.dataset.confirm = "";
        bouton.textContent = "Révoquer";
      }
    }, 5000);
    return;
  }
  bouton.disabled = true;
  try {
    renderIssuedShares(await invoke("share_revoke", { code: s.code }));
    toast(`Partage ${s.display_code} révoqué : le code n'ouvre plus rien.`);
  } catch (e) {
    bouton.disabled = false;
    toast(String(e), true);
  }
}

// ------------------------------------------------------- mises à jour

/**
 * Avertissement de mise à jour (voir `update.rs`). Au démarrage, une seule
 * vérification, silencieuse en cas d'échec : être hors ligne sur le terrain est
 * l'état normal de cette application, pas une erreur à signaler. En mode
 * « désactivé », le backend ne fait aucune requête.
 */
const majState = { reglages: null, disponible: null };

async function loadUpdateSettings() {
  try {
    majState.reglages = await invoke("update_settings");
    renderUpdateSettings();
  } catch (e) {
    console.warn("Réglages de mise à jour indisponibles :", e);
  }
}

function renderUpdateSettings() {
  const r = majState.reglages;
  if (!r) return;
  for (const radio of document.querySelectorAll('input[name="update-mode"]')) {
    radio.checked = radio.value === r.mode;
  }
  el("setting-update-beta").checked = r.channel === "beta";
  let info = `Version installée : ${r.current_version}${r.current_is_prerelease ? " (bêta)" : ""}.`;
  if (r.channel === "beta") info += " Canal : bêta.";
  if (r.last_check_at) info += ` Dernière vérification : ${fmtLocal(r.last_check_at)}.`;
  if (r.skipped_version) info += ` Version ignorée : ${r.skipped_version}.`;
  el("setting-update-info").textContent = info;
}

async function setUpdateMode(mode) {
  try {
    majState.reglages = await invoke("set_update_mode", { mode });
    renderUpdateSettings();
    toast(mode === "off"
      ? "Vérification des mises à jour désactivée."
      : "Vérification des mises à jour activée.");
  } catch (e) {
    toast(String(e), true);
  }
}

async function setUpdateChannel(beta) {
  try {
    majState.reglages = await invoke("set_update_channel", { channel: beta ? "beta" : "stable" });
    renderUpdateSettings();
    toast(beta
      ? "Canal bêta activé : la prochaine vérification proposera aussi les versions bêta."
      : "Retour au canal officiel : seules les versions officielles seront proposées.");
  } catch (e) {
    toast(String(e), true);
  }
}

async function checkUpdateNow() {
  const res = el("setting-update-result");
  res.hidden = false;
  res.textContent = "Vérification en cours…";
  el("btn-check-update").disabled = true;
  try {
    const r = await invoke("check_for_update", { manual: true });
    res.textContent = r.available
      ? `La version ${r.available.version}${r.available.prerelease ? " (version bêta)" : ""} est disponible.`
      : `Vous avez la dernière version (${r.current_version}).`;
    if (r.available) afficherBandeauMiseAJour(r.available, r.current_version);
    await loadUpdateSettings();
  } catch (e) {
    res.textContent = String(e);
  } finally {
    el("btn-check-update").disabled = false;
  }
}

async function verifierMisesAJourAuDemarrage() {
  try {
    const r = await invoke("check_for_update", { manual: false });
    if (r.available && !r.skipped) afficherBandeauMiseAJour(r.available, r.current_version);
  } catch (e) {
    console.info("Vérification des mises à jour impossible :", e);
  }
}

function afficherBandeauMiseAJour(info, versionActuelle) {
  majState.disponible = info;
  el("update-bar-text").textContent = info.prerelease
    ? `Version bêta ${info.version} de Gastinel Weather Studio disponible, pour tester les nouveautés (vous avez la ${versionActuelle}).`
    : `Gastinel Weather Studio ${info.version} est disponible (vous avez la ${versionActuelle}).`;
  el("update-bar-notes").textContent = info.notes || "";
  el("update-bar-notes").hidden = true;
  el("btn-update-notes").hidden = !info.notes;
  el("update-bar").hidden = false;
}

async function telechargerMiseAJour() {
  if (!majState.disponible) return;
  // La page de téléchargement lance d'elle-même le paquet .gws de cette
  // version (paramètre ?maj=) et explique quoi en faire. Version douteuse :
  // on retombe sur le lien du manifeste, comme avant. Rust revérifie les deux.
  const v = String(majState.disponible.version || "");
  const url = /^\d{1,4}\.\d{1,4}\.\d{1,4}(-[0-9A-Za-z.-]{1,20})?$/.test(v)
    ? `https://gastinel.me/gws/download/?maj=${v}`
    : majState.disponible.url;
  try {
    await invoke("open_update_download", { url });
  } catch (e) {
    toast(String(e), true);
  }
}

async function ignorerVersion() {
  if (!majState.disponible) return;
  try {
    majState.reglages = await invoke("skip_update_version", { version: majState.disponible.version });
    el("update-bar").hidden = true;
    toast(`La version ${majState.disponible.version} ne sera plus signalée.`);
  } catch (e) {
    toast(String(e), true);
  }
}

// ------------------------------------------------------------- paquets .gws

let pendingPackagePath = null;

async function promptPackageUpdate(packageInfo) {
  if (!packageInfo || !packageInfo.manifest) return;
  pendingPackagePath = packageInfo.file_path;
  const dlg = el("dlg-package-update");
  if (!dlg) return;

  el("pkg-current-version").textContent = `v${packageInfo.current_version}`;
  el("pkg-new-version").textContent = `v${packageInfo.manifest.version}`;
  if (packageInfo.manifest.title) {
    el("pkg-update-title").textContent = packageInfo.manifest.title;
  } else {
    el("pkg-update-title").textContent = `Mise à jour v${packageInfo.manifest.version}`;
  }
  el("pkg-update-changelog").textContent =
    packageInfo.manifest.changelog || packageInfo.manifest.notes || "Aucun détail fourni.";
  hideError(el("pkg-update-error"));

  const btnApply = el("btn-pkg-update-apply");
  btnApply.disabled = false;
  btnApply.textContent = "Installer la mise à jour et redémarrer";

  dlg.showModal();
}

async function applyPackageUpdate() {
  if (!pendingPackagePath) return;
  const btnApply = el("btn-pkg-update-apply");
  btnApply.disabled = true;
  btnApply.textContent = "Installation en cours...";
  hideError(el("pkg-update-error"));

  try {
    await invoke("apply_gws_package", { path: pendingPackagePath });
  } catch (e) {
    btnApply.disabled = false;
    btnApply.textContent = "Installer la mise à jour et redémarrer";
    showError(el("pkg-update-error"), String(e));
  }
}

async function handleOpenGwsPackageFile() {
  try {
    if (window.__TAURI__ && window.__TAURI__.dialog) {
      const selected = await window.__TAURI__.dialog.open({
        title: "Sélectionner un paquet de mise à jour .gws",
        filters: [{ name: "Paquet GWS (*.gws)", extensions: ["gws"] }],
        multiple: false,
      });
      if (selected) {
        const path = typeof selected === "string" ? selected : selected.path;
        if (path) {
          const info = await invoke("inspect_gws_package", { path });
          promptPackageUpdate(info);
        }
      }
    }
  } catch (e) {
    toast(String(e), true);
  }
}

// ------------------------------------------------------------- projet cloud

async function loadCloudStatus() {
  try {
    state.cloud = await invoke("cloud_status");
  } catch (e) {
    console.warn("Statut Cloud indisponible :", e);
    state.cloud = { logged_in: false, account: null, last_sync_at: null, auto_sync: true };
  }
}

function renderCloudStatus() {
  const c = state.cloud;
  if (c && c.logged_in) renderCloudProjects();
  const loggedOutBox = el("cloud-logged-out");
  const loggedInBox = el("cloud-logged-in");
  const errBox = el("cloud-error");
  if (!loggedOutBox || !loggedInBox) return;

  if (errBox) {
    errBox.hidden = true;
    errBox.textContent = "";
  }

  if (c && c.logged_in) {
    loggedOutBox.hidden = true;
    loggedInBox.hidden = false;
    const dispAcc = el("cloud-display-account");
    if (dispAcc) dispAcc.textContent = c.account || "";
    const syncStatus = el("cloud-sync-status");
    if (syncStatus) {
      if (c.last_sync_at) {
        const d = new Date(c.last_sync_at * 1000);
        syncStatus.textContent = `Dernière synchronisation : ${d.toLocaleDateString("fr-FR")} à ${d.toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" })}.`;
      } else {
        syncStatus.textContent = "Dernière synchronisation : aucune.";
      }
    }
    const autoSyncCb = el("cloud-auto-sync");
    if (autoSyncCb) autoSyncCb.checked = !!c.auto_sync;
  } else {
    loggedOutBox.hidden = false;
    loggedInBox.hidden = true;
    const pass = el("cloud-password");
    if (pass) pass.value = "";
    const inv = el("cloud-invite");
    if (inv) inv.value = "";
  }
}

async function cloudLogin() {
  const accountInput = el("cloud-account");
  const passwordInput = el("cloud-password");
  const errBox = el("cloud-error");
  if (errBox) errBox.hidden = true;

  const account = (accountInput?.value || "").trim().toLowerCase();
  const password = passwordInput?.value || "";

  if (!account || !/^[a-z]{3,32}$/.test(account)) {
    if (errBox) {
      errBox.textContent = "L'identifiant doit comporter 3 à 32 lettres minuscules (a-z uniquement).";
      errBox.hidden = false;
    }
    return;
  }
  if (!password || password.length < 8 || password.length > 30) {
    if (errBox) {
      errBox.textContent = "Le mot de passe doit comporter entre 8 et 30 caractères.";
      errBox.hidden = false;
    }
    return;
  }

  const btn = el("btn-cloud-login");
  const origText = btn ? btn.textContent : "Se connecter";
  if (btn) {
    btn.disabled = true;
    btn.textContent = "Connexion en cours…";
  }

  try {
    state.cloud = await invoke("cloud_login", { account, password });
    renderCloudStatus();
    toast(`Connecté au compte Cloud « ${state.cloud.account} ». Synchronisation…`);
    try {
      const res = await invoke("cloud_sync");
      await loadCloudStatus();
      renderCloudStatus();
      await loadProjects();
      toast(`Synchronisation terminée : ${res.uploaded} envoyé(s), ${res.downloaded} reçu(s).`);
    } catch (syncErr) {
      console.warn("Erreur synchro post-login :", syncErr);
      toast(`Connecté, mais la synchronisation a échoué : ${syncErr}`, true);
    }
  } catch (e) {
    if (errBox) {
      errBox.textContent = String(e);
      errBox.hidden = false;
    }
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = origText;
    }
  }
}

async function cloudRegister() {
  const accountInput = el("cloud-account");
  const passwordInput = el("cloud-password");
  const inviteInput = el("cloud-invite");
  const errBox = el("cloud-error");
  if (errBox) errBox.hidden = true;

  const account = (accountInput?.value || "").trim().toLowerCase();
  const password = passwordInput?.value || "";
  const invite = (inviteInput?.value || "").trim().toUpperCase();

  if (!account || !/^[a-z]{3,32}$/.test(account)) {
    if (errBox) {
      errBox.textContent = "L'identifiant doit comporter 3 à 32 lettres minuscules (a-z uniquement).";
      errBox.hidden = false;
    }
    return;
  }
  if (!password || password.length < 8 || password.length > 30) {
    if (errBox) {
      errBox.textContent = "Le mot de passe doit comporter entre 8 et 30 caractères.";
      errBox.hidden = false;
    }
    return;
  }
  if (!invite) {
    if (errBox) {
      errBox.textContent = "Le code d'invitation est obligatoire pour une première inscription.";
      errBox.hidden = false;
    }
    return;
  }

  const btn = el("btn-cloud-register");
  const origText = btn ? btn.textContent : "Première inscription";
  if (btn) {
    btn.disabled = true;
    btn.textContent = "Inscription en cours…";
  }

  try {
    state.cloud = await invoke("cloud_register", { account, password, invite });
    renderCloudStatus();
    toast(`Compte Cloud « ${state.cloud.account} » créé ! Envoi initial des données…`);
    try {
      const res = await invoke("cloud_sync");
      await loadCloudStatus();
      renderCloudStatus();
      await loadProjects();
      toast(`Données synchronisées dans le Cloud : ${res.uploaded} envoyé(s).`);
    } catch (syncErr) {
      console.warn("Erreur synchro post-register :", syncErr);
      toast(`Compte créé, mais l'envoi initial a échoué : ${syncErr}`, true);
    }
  } catch (e) {
    if (errBox) {
      errBox.textContent = String(e);
      errBox.hidden = false;
    }
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = origText;
    }
  }
}

async function cloudLogout() {
  const confirmed = await confirm("Se déconnecter du compte Cloud sur cet appareil ? Vos données locales sont conservées.");
  if (!confirmed) return;

  try {
    state.cloud = await invoke("cloud_logout");
    renderCloudStatus();
    toast("Déconnecté du Projet Cloud.");
  } catch (e) {
    toast(`Erreur de déconnexion : ${e}`, true);
  }
}

async function cloudSync() {
  const btn = el("btn-cloud-sync");
  const origText = btn ? btn.textContent : "Synchroniser maintenant";
  if (btn) {
    btn.disabled = true;
    btn.textContent = "Synchronisation…";
  }

  try {
    const res = await invoke("cloud_sync");
    toast(`Synchronisation terminée : ${res.uploaded} envoyé(s), ${res.downloaded} reçu(s).`);
    await loadCloudStatus();
    renderCloudStatus();
    await loadProjects();
    if (state.project) {
      await openProject(state.project.id);
    }
  } catch (e) {
    toast(`Erreur lors de la synchronisation : ${e}`, true);
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = origText;
    }
  }
}

async function cloudSetAutoSync(enabled) {
  try {
    await invoke("cloud_set_auto_sync", { enabled });
    if (state.cloud) state.cloud.auto_sync = enabled;
  } catch (e) {
    console.warn("Erreur auto_sync :", e);
  }
}

function wireUp() {
  // ---- première ouverture : la clé de l'agent, puis le prénom
  el("form-onboarding-key").addEventListener("submit", submitOnboardingKey);
  el("onboarding-key-link").addEventListener("click", openGroqKeysPage);
  // « Passer » n'enregistre rien et n'efface rien : il enchaîne, point.
  el("btn-onboarding-key-skip").addEventListener("click", showOnboarding);
  el("form-onboarding").addEventListener("submit", submitOnboarding);

  // ---- écran d'accueil : trois boutons, ou le remplissage de sept secondes
  for (const bouton of document.querySelectorAll("#home-actions .home-btn")) {
    bouton.addEventListener("click", () => {
      // Un clic, quel qu'il soit, coupe le compte à rebours — y compris sur un
      // autre bouton que celui qui se remplissait.
      annulerRemplissage();
      quitterAccueil(bouton.dataset.action);
    });
  }

  // ---- agent IA : question libre (les boutons rapides sont posés au rendu)
  el("form-agent-ask").addEventListener("submit", submitAgentQuestion);

  el("btn-demo-project").addEventListener("click", creerProjetExemple);
  el("btn-new-project").addEventListener("click", () => {
    hideError(el("project-error"));
    el("form-project").reset();
    el("dlg-project").showModal();
  });

  el("form-project").addEventListener("submit", async (event) => {
    event.preventDefault();
    hideError(el("project-error"));
    try {
      const project = await invoke("create_project", { name: el("project-name").value });
      el("dlg-project").close();
      await loadProjects();
      toast(`Projet « ${project.name} » créé.`);
      await openProject(project.id);
    } catch (e) {
      showError(el("project-error"), String(e));
    }
  });

  // ---- barre supérieure
  el("btn-home").addEventListener("click", () => {
    goHome();
  });
  el("btn-home").disabled = true;
  el("btn-settings").addEventListener("click", async () => {
    hideError(el("setting-error"));
    // Relit le statut de la clé à chaque ouverture : le fichier a pu être
    // remplacé depuis le démarrage, c'est même le geste qu'on attend après une
    // erreur d'IP.
    //
    // L'écran s'ouvre tout de suite. Le prénom et l'enregistrement restent
    // bloqués jusqu'à la fin du chargement : sinon la valeur chargée pourrait
    // écraser une saisie en cours, ou un enregistrement partir de valeurs périmées.
    const verrous = [el("setting-user-name"), el("setting-refresh")];
    for (const v of verrous) v.disabled = true;
    el("dlg-settings").showModal();
    afficherQuotaPartage(false);
    chargerAppareil();
    // Toujours rouvrir en haut.
    const defil = el("dlg-settings").querySelector(".dialog-form");
    requestAnimationFrame(() => { defil.scrollTop = 0; });
    renderProjetsGestion();
    try {
      await loadSettings();
    } catch (e) {
      showError(el("setting-error"), String(e));
    } finally {
      for (const v of verrous) v.disabled = false;
    }
  });
  // Enregistrement automatique : à la validation du champ (sortie ou Entrée).
  el("setting-refresh").addEventListener("change", saveSettings);
  el("setting-user-name").addEventListener("change", saveSettings);
  el("btn-setting-groq-key").addEventListener("click", saveGroqKeyFromSettings);
  el("setting-groq-key-link").addEventListener("click", openGroqKeysPage);
  el("btn-setting-infoclimat-key").addEventListener("click", saveInfoclimatKeyFromSettings);
  el("setting-infoclimat-key-link").addEventListener("click", openInfoclimatKeysPage);
  el("btn-save-ssh")?.addEventListener("click", saveSshRelayFromSettings);
  el("btn-test-ssh")?.addEventListener("click", testSshRelayConnection);
  el("btn-browse-ssh-key")?.addEventListener("click", browseSshKeyFile);
  el("setting-ssh-enabled")?.addEventListener("change", toggleSshRelayEnabled);
  for (const radio of document.querySelectorAll('input[name="ssh-auth-type"]')) {
    radio.addEventListener("change", (e) => updateSshAuthTypeVisibility(e.target.value));
  }
  el("btn-clear-data").addEventListener("click", clearAllData);
  el("btn-clear-tiles-cache").addEventListener("click", clearTilesCache);
  el("btn-open-gws-package")?.addEventListener("click", () => {
    el("dlg-settings").close();
    handleOpenGwsPackageFile();
  });
  el("btn-pkg-update-cancel")?.addEventListener("click", () => {
    el("dlg-package-update").close();
  });
  el("btn-pkg-update-apply")?.addEventListener("click", applyPackageUpdate);

  // Paquet .gws déposé sur la fenêtre. Tauri intercepte les fichiers venus du
  // système : seul son événement donne le chemin (l'événement HTML `drop` ne
  // le fournit pas dans WebView2).
  const ouvrirPaquet = async (path) => {
    try {
      promptPackageUpdate(await invoke("inspect_gws_package", { path }));
    } catch (err) {
      toast(String(err), true);
    }
  };
  window.__TAURI__?.webview?.getCurrentWebview?.().onDragDropEvent((ev) => {
    if (ev.payload?.type !== "drop") return;
    // Un dépôt pendant le compte à rebours de l'accueil l'interrompt, sinon
    // l'écran basculerait sous la fenêtre ouverte par le dépôt.
    if (homeTimers.auto) annulerRemplissage();
    const paths = ev.payload.paths || [];
    const path = paths.find((p) => p.toLowerCase().endsWith(".gws"));
    if (path) {
      ouvrirPaquet(path);
      return;
    }
    // L'onglet IA a sa propre zone de dépôt (gestionnaire plus bas).
    if (el("dlg-import").open && importState.tab === "ai") return;
    deposerFichierImport(paths);
  });
  // Double-clic sur un .gws alors que l'application tourne déjà.
  window.__TAURI__?.event?.listen("gws-paquet-ouvert", (ev) => ouvrirPaquet(ev.payload));

  el("btn-show-quickstart")?.addEventListener("click", () => {
    el("dlg-settings").close();
    showQuickstartGuide(true);
  });
  el("btn-quickstart-close")?.addEventListener("click", closeQuickstartGuide);
  el("dlg-quickstart")?.addEventListener("click", (e) => {
    if (e.target === el("dlg-quickstart")) closeQuickstartGuide();
  });
  el("dlg-quickstart")?.addEventListener("close", () => {
    localStorage.setItem("gws_quickstart_seen", "true");
  });

  // Préférences des onglets Elitech : appliquées tout de suite, sans attendre
  // « Enregistrer ». Ce ne sont pas des données du projet à valider côté
  // serveur, juste l'affichage de cette fenêtre — les traiter comme le reste du
  // formulaire ajouterait un clic pour un effet qui devrait être immédiat.
  el("setting-elitech-close-on-exit").addEventListener("change", (e) => {
    GwsElitech.setPrefs({ closeTabsOnExit: e.target.checked });
  });
  for (const bouton of document.querySelectorAll('input[name="elitech-tab-position"]')) {
    bouton.addEventListener("change", (e) => {
      if (e.target.checked) GwsElitech.setPrefs({ tabPosition: e.target.value });
    });
  }

  // ---- pagination du bloc Stations / Export
  el("stations-prev").addEventListener("click", () => showStationsPage(stationsPage - 1));
  el("stations-next").addEventListener("click", () => showStationsPage(stationsPage + 1));
  for (const dot of document.querySelectorAll("#stations-dots .dot"))
    dot.addEventListener("click", () => showStationsPage(Number(dot.dataset.page)));

  // ---- export (page 2 du bloc Stations)
  for (const b of document.querySelectorAll(".export-btn")) {
    b.addEventListener("click", () => lancerExport(b.dataset.export));
  }

  el("btn-clear-selection").addEventListener("click", () => GwsMap.clearSelection());

  el("btn-new-point").addEventListener("click", () => openPointDialog("create"));
  el("point-category").addEventListener("change", syncSizeOptions);
  // Carte et graphique se mesurent tous deux à la taille de leur conteneur :
  // sans ce rappel, la carte laisse des tuiles manquantes et le graphique garde
  // sa largeur d'avant le redimensionnement.
  window.addEventListener("resize", () => {
    GwsMap.refresh();
    GwsCross.resize();
    GwsCrossPlein.resize();
  });

  // Le bloc de croisement s'abonne à ses propres contrôles (sélecteur de
  // période, plage personnalisée) une seule fois : ils vivent dans le document,
  // pas dans un projet.
  GwsCross.init({
    invoke,
    onError: (message) => toast(message, true),
  });

  // Vue plein écran du croisement : elle lit les points du projet ouvert et la
  // sélection de la carte au moment où elle s'ouvre, sans rien y écrire.
  GwsCrossPlein.init({
    invoke,
    onError: (message) => toast(message, true),
    points: () => state.points || [],
    selection: () => state.selectedIds || [],
  });

  // La page Elitech reçoit ses dépendances une seule fois. Elle ne connaît ni
  // le projet ouvert ni la sélection : tout ce dont ses panneaux ont besoin leur
  // est passé à l'ouverture, et leurs retours repassent ici.
  GwsElitech.init({
    invoke,
    onToast: (message) => toast(message),
    onError: (message) => toast(message, true),
    // La même boîte de dialogue de ré-import que l'écran d'import : une seule
    // implémentation de cette question dans toute l'application.
    confirmDuplicate: avecConfirmationDeReimport,
    // Un enregistrement fait depuis un logger ne concerne la grille que si elle
    // montre le projet visé : la recharger autrement afficherait les points d'un
    // projet pour un autre.
    onSaved: (projectId) => {
      if (state.project && state.project.id === projectId) {
        loadPoints().catch((e) => toast(String(e), true));
      }
    },
  });
  el("btn-elitech").addEventListener("click", () => {
    openElitechPage().catch((e) => toast(String(e), true));
  });

  // ---- téléphone : l'accès au RC-5 change de forme, pas de contenu
  //
  // Les deux modules reçoivent les mêmes dépendances et le même flux de
  // détection ; un seul des deux est activé (cf. `mobile.js`, en-tête). Sur
  // poste fixe, `GwsMobile` reste inerte — `noteDetection` sort tout de suite
  // tant que `activer()` n'est pas passé.
  GwsMobile.init({
    invoke,
    onToast: (message) => toast(message),
    onError: (message) => toast(message, true),
    confirmDuplicate: avecConfirmationDeReimport,
    onSaved: (projectId) => {
      if (state.project && state.project.id === projectId) {
        loadPoints().catch((e) => toast(String(e), true));
      }
    },
    listProjects: () => invoke("list_projects"),
  });
  if (GwsMobile.estMobile()) {
    GwsMobile.activer();
    // L'onglet « USB direct (RC-5) » de l'import disparaît avec le reste : sur
    // téléphone, le RC-5 ne s'atteint plus depuis un projet. Retiré du
    // document plutôt que masqué — `switchTab` parcourt les onglets présents,
    // et un onglet caché mais atteignable serait un chemin de plus.
    const ongletRc5 = document.querySelector('#dlg-import .tab[data-tab="rc5"]');
    if (ongletRc5) ongletRc5.remove();
  }

  // ---- saisie manuelle : la liste des jours suit les dates et la plage horaire
  for (const champ of ["entry-from", "entry-to"]) {
    el(champ).addEventListener("change", renderEntryDays);
  }
  for (const champ of ["entry-start", "entry-end", "entry-kind", "entry-tz"]) {
    el(champ).addEventListener("change", renderEntryDays);
  }
  el("entry-days").addEventListener("input", (e) => {
    if (e.target.dataset.date) {
      e.target.closest(".entry-day").classList.remove("is-missing");
      importState.entryDirty = true;
    }
  });
  // Toute frappe dans la fiche point la marque « modifiée » (voir la garde de fermeture).
  el("form-point").addEventListener("input", () => {
    pointDialogState.dirty = true;
  });
  el("form-point").addEventListener("change", () => {
    pointDialogState.dirty = true;
  });
  el("point-lat").addEventListener("input", syncIgnAltitudeButton);
  el("point-lon").addEventListener("input", syncIgnAltitudeButton);
  el("btn-ign-altitude").addEventListener("click", fetchIgnAltitude);
  el("form-point").addEventListener("submit", submitPoint);
  el("delete-point-confirm").addEventListener("click", () => resolvePointDeletion(true));
  el("delete-point-cancel").addEventListener("click", () => resolvePointDeletion(false));
  el("dlg-delete-point").addEventListener("close", () => resolvePointDeletion(false));

  el("btn-imports").addEventListener("click", ouvrirImports);
  el("imports-recherche").addEventListener("input", () => afficherImports());

  el("btn-import").addEventListener("click", () => {
    if (state.points.length === 0) {
      toast("Ajoutez d'abord un point : une série est toujours rattachée à un point.", true);
      return;
    }
    openImportDialog();
  });
  el("import-file").addEventListener("change", onFileChosen);
  el("import-tz").addEventListener("change", updateOffsetHint);
  el("import-drift").addEventListener("input", updateOffsetHint);
  el("btn-analyze").addEventListener("click", analyzeFile);
  el("btn-analyze-ic").addEventListener("click", analyzeInfoclimat);
  el("btn-preview").addEventListener("click", showPreview);
  el("btn-rc5-search").addEventListener("click", searchRc5);
  el("btn-rc5-read").addEventListener("click", readRc5);
  el("rc5-notice-dismiss").addEventListener("click", hideRc5Notice);
  // Changer de point de rattachement doit suivre son identifiant de station,
  // sinon on importe la Moucherolle sous l'identifiant de Darbounouse.
  el("import-point").addEventListener("change", () => {
    const choisi = findPoint(el("import-point").value);
    el("ic-station").value = (choisi && choisi.infoclimat_station_id) || "";
  });
  el("form-import").addEventListener("submit", submitImport);
  for (const b of document.querySelectorAll("#dlg-import .tab-groupe")) {
    b.addEventListener("click", () => {
      hideError(el("import-error"));
      const g = b.dataset.groupe;
      if (g === "station") switchTab("station");
      else if (g === "releve") switchTab(document.querySelector('input[name="import-releve"]:checked')?.value || "entry");
      else switchTab(ongletImportPresent(el("import-format").value) && el("import-format").value !== "ai" ? el("import-format").value : "auto");
    });
  }
  el("import-format").addEventListener("change", () => switchTab(el("import-format").value));
  for (const r of document.querySelectorAll('input[name="import-releve"]')) r.addEventListener("change", () => switchTab(r.value));
  for (const r of document.querySelectorAll('input[name="station-quoi"]')) r.addEventListener("change", majCoutStations);

  // Ré-import : chaque bouton tranche directement. L'événement `close` couvre la
  // fermeture par Échap, qui vaut « annuler » — le défaut sûr. `reglerReimport`
  // étant idempotente, le `close` qui suit un clic ne défait pas le choix.
  el("dup-force").addEventListener("click", () => reglerReimport(true));
  el("dup-cancel").addEventListener("click", () => reglerReimport(false));
  el("dlg-duplicate").addEventListener("close", () => reglerReimport(false));

  el("btn-analyze-romma").addEventListener("click", analyzeRomma);

  // ---- Assistant IA (Fichiers & Images)
  const dropzone = el("ai-dropzone");
  if (dropzone) {
    ["dragenter", "dragover"].forEach((eventName) => {
      dropzone.addEventListener(eventName, (e) => {
        e.preventDefault();
        e.stopPropagation();
        dropzone.classList.add("is-dragover");
      });
    });
    ["dragleave", "drop"].forEach((eventName) => {
      dropzone.addEventListener(eventName, (e) => {
        e.preventDefault();
        e.stopPropagation();
        dropzone.classList.remove("is-dragover");
      });
    });
    dropzone.addEventListener("drop", (e) => {
      const dt = e.dataTransfer;
      if (dt && dt.files && dt.files.length > 0) {
        handleAiFiles(dt.files);
      }
    });

    // Fichiers venus du système : Tauri les intercepte, le `drop` HTML ci-dessus
    // ne reçoit rien. Seul son événement donne les chemins, avec une position
    // en pixels physiques : on la ramène en pixels CSS avant de la comparer au
    // rectangle de la zone. Rust ne lit que les chemins réellement déposés.
    const surLaZone = (pos) => {
      if (!pos || !el("dlg-import").open || importState.tab !== "ai") return false;
      const r = dropzone.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) return false;
      const k = window.devicePixelRatio || 1;
      const x = pos.x / k;
      const y = pos.y / k;
      return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
    };
    const MIME = { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", webp: "image/webp", json: "application/json" };
    const lireDepot = async (paths) => {
      const fichiers = [];
      for (const path of paths) {
        if (path.toLowerCase().endsWith(".gws")) continue; // traité par le gestionnaire des paquets
        const nom = path.split(/[\\/]/).pop();
        const ext = (nom.split(".").pop() || "").toLowerCase();
        try {
          const octets = await invoke("read_dropped_file", { path });
          fichiers.push(new File([octets], nom, { type: MIME[ext] || (/^(csv|tsv|txt|dat|log)$/.test(ext) ? "text/plain" : "") }));
        } catch (err) {
          toast(String(err), true);
        }
      }
      if (fichiers.length > 0) handleAiFiles(fichiers);
    };
    window.__TAURI__?.webview?.getCurrentWebview?.().onDragDropEvent((ev) => {
      const p = ev.payload || {};
      if (p.type === "leave") {
        dropzone.classList.remove("is-dragover");
      } else if (p.type === "enter" || p.type === "over") {
        dropzone.classList.toggle("is-dragover", surLaZone(p.position));
      } else if (p.type === "drop") {
        dropzone.classList.remove("is-dragover");
        if (surLaZone(p.position)) lireDepot(p.paths || []);
      }
    });
  }

  el("ai-input-files").addEventListener("change", (e) => {
    if (e.target.files && e.target.files.length > 0) {
      handleAiFiles(e.target.files);
      e.target.value = "";
    }
  });
  el("ai-input-dir").addEventListener("change", (e) => {
    if (e.target.files && e.target.files.length > 0) {
      handleAiFiles(e.target.files);
      e.target.value = "";
    }
  });

  window.addEventListener("paste", (e) => {
    if (importState.tab !== "ai" || !el("dlg-import").open) return;
    const items = e.clipboardData && e.clipboardData.items;
    if (!items) return;
    const files = [];
    for (const item of items) {
      if (item.type && item.type.startsWith("image/")) {
        const file = item.getAsFile();
        if (file) {
          const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
          const renamed = new File([file], `capture-${timestamp}.png`, { type: file.type });
          files.push(renamed);
        }
      }
    }
    if (files.length > 0) {
      handleAiFiles(files);
      toast(`${files.length} capture(s) collée(s) depuis le presse-papier.`);
    }
  });

  el("btn-ai-analyze-all").addEventListener("click", runAiAnalysis);
  el("btn-ai-recalculate-preview").addEventListener("click", recalculateAiPreview);
  const btnGroqSettings = el("btn-open-settings-groq");
  if (btnGroqSettings) {
    btnGroqSettings.addEventListener("click", () => {
      el("dlg-import").close();
      el("btn-settings").click();
    });
  }

  // ---- partage par code
  el("btn-share-settings")?.addEventListener("click", openShareDialog);
  el("btn-receive-share-settings")?.addEventListener("click", openReceiveDialog);
  el("btn-share-create")?.addEventListener("click", createShare);
  el("btn-share-copy")?.addEventListener("click", copyShareCode);
  for (const radio of document.querySelectorAll('input[name="share-content"]')) {
    radio.addEventListener("change", majChoixPeriode);
  }
  el("btn-share-all")?.addEventListener("click", () => cocherTousLesPointsPartages(true));
  el("btn-share-none")?.addEventListener("click", () => cocherTousLesPointsPartages(false));
  el("btn-receive-preview")?.addEventListener("click", previewReceivedShare);
  el("btn-receive-import")?.addEventListener("click", importReceivedShare);
  el("receive-code")?.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      previewReceivedShare();
    }
  });
  // Un code modifié après l'aperçu n'est plus celui qu'on a vu : on oublie.
  el("receive-code")?.addEventListener("input", oublierApercuPartage);

  // ---- projet cloud (synchronisation)
  el("btn-cloud-login")?.addEventListener("click", cloudLogin);
  el("btn-cloud-register")?.addEventListener("click", cloudRegister);
  el("btn-cloud-sync")?.addEventListener("click", cloudSync);
  el("btn-cloud-logout")?.addEventListener("click", cloudLogout);
  el("cloud-auto-sync")?.addEventListener("change", (e) => {
    cloudSetAutoSync(e.target.checked);
  });
  el("cloud-password")?.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      cloudLogin();
    }
  });
  el("cloud-invite")?.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      cloudRegister();
    }
  });

  // ---- mises à jour
  for (const radio of document.querySelectorAll('input[name="update-mode"]')) {
    radio.addEventListener("change", (e) => setUpdateMode(e.target.value));
  }
  el("btn-check-update").addEventListener("click", checkUpdateNow);
  el("setting-update-beta").addEventListener("change", (e) => setUpdateChannel(e.target.checked));
  el("btn-update-download").addEventListener("click", telechargerMiseAJour);
  el("btn-update-skip").addEventListener("click", ignorerVersion);
  el("btn-update-close").addEventListener("click", () => {
    el("update-bar").hidden = true;
  });
  el("btn-update-notes").addEventListener("click", () => {
    el("update-bar-notes").hidden = !el("update-bar-notes").hidden;
  });

  // ---- relai SSH : oubli de l'empreinte acceptée
  el("btn-ssh-forget-key")?.addEventListener("click", forgetSshHostKey);

  // Onglets d'import : clic, ou flèches / Début / Fin (motif ARIA « tablist »).
  // Le choix de l'utilisateur est retenu pour la prochaine ouverture.
  const choisirOnglet = (tab) => {
    switchTab(tab);
    memoriserOngletImport(tab);
  };
  for (const b of document.querySelectorAll("#dlg-import .tab"))
    b.addEventListener("click", () => choisirOnglet(b.dataset.tab));
  document.querySelector("#dlg-import .tabs")?.addEventListener("keydown", (e) => {
    const onglets = [...document.querySelectorAll("#dlg-import .tab")];
    const i = onglets.indexOf(document.activeElement);
    if (i < 0) return;
    const cible = {
      ArrowRight: (i + 1) % onglets.length,
      ArrowLeft: (i - 1 + onglets.length) % onglets.length,
      Home: 0,
      End: onglets.length - 1,
    }[e.key];
    if (cible === undefined) return;
    e.preventDefault();
    onglets[cible].focus();
    choisirOnglet(onglets[cible].dataset.tab);
  });

  for (const b of document.querySelectorAll("[data-close]"))
    b.addEventListener("click", () => fermerAvecGarde(b.closest("dialog")));
  // Paramètres : un clic sur le fond, hors du panneau, le ferme (Échap le fait
  // déjà, nativement). On compare au rectangle du panneau plutôt qu'à
  // `e.target` : un clic dans la marge intérieure viserait aussi le <dialog>.
  el("dlg-settings").addEventListener("click", (e) => {
    const dlg = el("dlg-settings");
    if (e.target !== dlg) return;
    const r = dlg.getBoundingClientRect();
    const dedans =
      e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
    if (!dedans) dlg.close();
  });

  // Raccourcis : Ctrl+I import, Ctrl+, Paramètres, Échap ferme le toast.
  // Les fenêtres modales se ferment déjà à Échap (comportement natif de <dialog>).
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !el("toast").hidden && !document.querySelector("dialog[open]")) {
      el("toast").hidden = true;
      return;
    }
    if (!e.ctrlKey || e.altKey || e.shiftKey || e.metaKey) return;
    const modaleOuverte = document.querySelector("dialog[open]");
    if (e.key === "i" || e.key === "I") {
      if (modaleOuverte || !state.project || el("view-project").hidden) return;
      e.preventDefault();
      el("btn-import").click();
    } else if (e.key === ",") {
      if (modaleOuverte || el("btn-settings").disabled) return;
      e.preventDefault();
      el("btn-settings").click();
    }
  });

  // Retour / Avancer sur PC, comme un navigateur : Alt+← / Alt+→ et les
  // boutons latéraux de la souris. Retour suit le même chemin que le bouton
  // retour Android (onglet Elitech, page Elitech, projet → liste) ; Avancer
  // rouvre le dernier projet quitté. Jamais quand une fenêtre est ouverte ou
  // pendant une saisie : Alt+← y garde son sens d'édition.
  const navigationPossible = (cible) =>
    !document.querySelector("dialog[open]") &&
    !(cible instanceof HTMLElement && cible.closest("input, textarea, select, [contenteditable]"));
  const naviguer = (sens) => {
    if (sens === "retour") return navigateAndroidBack();
    if (state.project || !navState.projetQuitte || el("view-projects").hidden) return false;
    const id = navState.projetQuitte;
    navState.projetQuitte = null;
    openProject(id).catch((err) => toast(String(err), true));
    return true;
  };
  document.addEventListener("keydown", (e) => {
    if (!e.altKey || e.ctrlKey || e.shiftKey || e.metaKey) return;
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    if (!navigationPossible(e.target)) return;
    if (naviguer(e.key === "ArrowLeft" ? "retour" : "avancer")) e.preventDefault();
  });
  // Boutons 3 et 4 = précédent / suivant. `mouseup` et non `auxclick` :
  // WebView2 ne déclenche pas toujours `auxclick` pour ces boutons.
  document.addEventListener("mouseup", (e) => {
    if (e.button !== 3 && e.button !== 4) return;
    if (!navigationPossible(e.target)) return;
    if (naviguer(e.button === 3 ? "retour" : "avancer")) e.preventDefault();
  });
  // Échap sur la page Elitech hors fenêtre : remonte d'un cran (onglet, puis
  // page), comme sa croix. Ailleurs Échap ne quitte pas un projet : trop
  // facile à presser par erreur.
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || el("view-elitech").hidden) return;
    if (!navigationPossible(e.target)) return;
    if (navigateAndroidBack()) e.preventDefault();
  });

  // F5 / Ctrl+R rechargeaient la page et ramenaient à l'écran d'accueil en
  // perdant le projet ouvert : bloqués. Un vrai rechargement reste possible
  // en relançant l'application.
  document.addEventListener("keydown", (e) => {
    const recharger =
      e.key === "F5" || ((e.ctrlKey || e.metaKey) && (e.key === "r" || e.key === "R"));
    if (recharger) e.preventDefault();
  });

  // Ctrl+Z / Ctrl+Y (ou Ctrl+Maj+Z) : annuler / rétablir les suppressions et
  // corrections de données. Dans un champ de saisie, la touche garde son sens
  // d'édition de texte.
  document.addEventListener("keydown", (e) => {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    const k = e.key.toLowerCase();
    const sens = k === "z" && !e.shiftKey ? "undo" : k === "y" || (k === "z" && e.shiftKey) ? "redo" : null;
    if (!sens) return;
    if (e.target instanceof HTMLElement && e.target.closest("input, textarea, select, [contenteditable]")) return;
    e.preventDefault();
    annulerOuRetablir(sens);
  });

  // Échap sur une fenêtre gardée : même confirmation que le bouton Annuler.
  for (const id of Object.keys(GARDES_FERMETURE)) {
    el(id)?.addEventListener("cancel", (e) => {
      if (!GARDES_FERMETURE[id].modifie()) return;
      e.preventDefault();
      fermerAvecGarde(el(id));
    });
  }

  el("dlg-import").addEventListener("close", depotSuivant);
  // Point abandonné : le fichier en attente est oublié.
  el("dlg-point").addEventListener("close", () => {
    if (el("dlg-point").returnValue !== "saved") setTimeout(() => (depotState.apresPoint = null), 0);
  });

  el("dlg-detail").addEventListener("close", () => {
    state.detailPointId = null;
  });

  el("dlg-infoclimat-history")?.addEventListener("close", () => {
    if (icHistoryUnlisten) {
      icHistoryUnlisten();
      icHistoryUnlisten = null;
    }
  });
}

async function boot() {
  if (!invoke) {
    document.body.prepend(
      h("p", {
        class: "form-error",
        text:
          "Cette page doit être ouverte depuis l'application Tauri (« npm run tauri dev » ou " +
          "« cargo tauri dev »). Les commandes du backend ne sont pas disponibles dans un navigateur seul.",
      })
    );
    return;
  }

  // Tout d'abord : l'interface démarre, le gardien de mise à jour `.gws` ne
  // doit pas restaurer l'ancienne version (sans attendre, sans bloquer).
  invoke("confirmer_demarrage_maj").catch((e) => console.warn("Confirmation de mise à jour :", e));

  wireUp();
  // Rien n'est branché tant que le fil de surveillance n'a rien dit : le bouton
  // d'accès le dit, sans se désactiver — la page a de quoi répondre même vide.
  refreshElitechButton();
  // Avant tout chargement : le fil de surveillance du backend a démarré avec
  // l'application, et un logger déjà branché est signalé dans les premières
  // secondes. Sans écouteur posé tôt, cet événement-là serait perdu.
  listenRc5();
  listenCloudProgress();
  try {
    await loadCategories();
    // Les réglages ne doivent pas empêcher l'application de démarrer : une clé
    // absente est un état normal tant qu'on n'utilise pas l'API.
    await loadSettings().catch((e) => console.warn("Paramètres indisponibles :", e));

    // Toute première ouverture : le prénom est demandé avant la liste des
    // projets. `loadSettings` l'a peut-être déjà renseigné ; on interroge quand
    // même le backend, qui reste la source de vérité si les réglages ont échoué.
    if (!(await loadUserName())) {
      // Desktop seulement. Sur téléphone, `Clee.API/` vit dans le répertoire de
      // données de l'application, sans navigateur côte à côte pour aller
      // chercher la clé : l'écran y serait une étape de plus sans le geste
      // qu'elle suppose. La première ouverture y reste celle du prénom, telle
      // qu'elle était.
      if (GwsMobile.estMobile()) showOnboarding();
      else showOnboardingKey();
      return;
    }

    // Les projets locaux s'affichent tout de suite ; la synchro Cloud suit en
    // arrière-plan (fenêtre « Projet Cloud ») au lieu de retarder la liste.
    const syncDemarrage = state.cloud?.logged_in && state.cloud?.auto_sync;
    if (syncDemarrage) {
      cloudAutoSyncFaite = true;
      etapeCloudDemarrage("Chargement des projets…");
    }
    const projects = await loadProjects();
    if (syncDemarrage) synchroCloudDemarrage();
    if (!localStorage.getItem("gws_quickstart_seen")) {
      if (projects && projects.length > 0) {
        localStorage.setItem("gws_quickstart_seen", "true");
      } else {
        showQuickstartGuide();
      }
    }
    // Sans `await` : la liste des projets est déjà là, la vérification
    // (réseau) ne doit rien retarder.
    verifierMisesAJourAuDemarrage();

    // Premier lancement après une mise à jour par paquet .gws.
    try {
      const version = await invoke("consommer_apres_maj");
      if (version) {
        el("apres-maj-version").textContent = `Version ${version}`;
        el("dlg-apres-maj").showModal();
      }
    } catch (e) {
      console.warn("Écran après mise à jour :", e);
    }

    // Paquet .gws passé au démarrage (ex: double-clic dans Windows)
    try {
      const startupPkg = await invoke("get_startup_package");
      if (startupPkg) {
        promptPackageUpdate(startupPkg);
      }
    } catch (e) {
      toast(`Paquet .gws refusé : ${e}`, true);
    }
  } catch (e) {
    toast(String(e), true);
  }
}

document.addEventListener("DOMContentLoaded", boot);

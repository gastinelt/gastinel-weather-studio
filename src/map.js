/*
 * Carte des points du projet (Leaflet).
 *
 * Module autonome : il ne connaît rien de `main.js`, qui lui injecte ses rappels
 * (`onCreateHere`, `onEditPoint`, `onSelectionChange`). L'inverse — une carte qui
 * appellerait directement les formulaires — rendrait les deux fichiers indissociables.
 *
 * Leaflet est vendorisé dans `vendor/leaflet/` : la CSP de l'application interdit
 * tout script externe, et l'app doit fonctionner dans un refuge sans réseau.
 * Les tuiles ne sortent plus jamais sur le réseau depuis ce fichier : elles
 * passent par la commande Tauri `get_tile`, qui sert le cache disque et ne
 * télécharge que si nécessaire (voir `tiles.rs`) — c'est ce qui permet à la
 * carte de rester utilisable en zone blanche sur les secteurs déjà visités.
 */

const GwsMap = (() => {
  "use strict";

  /**
   * Accès à Tauri, en local à ce module — `map.js` reste autonome (cf.
   * l'en-tête du fichier) et ne dépend pas de la constante équivalente de
   * `main.js`.
   */
  const { invoke } = window.__TAURI__ ? window.__TAURI__.core : { invoke: null };
  const evenements = window.__TAURI__ && window.__TAURI__.event;

  // ===========================================================================
  // Cache de tuiles hors-ligne
  // ===========================================================================
  //
  // Aucune tuile n'est plus demandée en URL directe depuis le JS : chaque tuile
  // passe par la commande Tauri `get_tile(source, z, x, y)`, qui sert le
  // fichier local s'il existe et ne télécharge que sinon (voir `tiles.rs`, où
  // vit désormais la clé TOP25 et la construction de l'URL WMTS — ce fichier
  // n'en a plus besoin).
  //
  // C'est ce qui rend la carte utilisable en zone blanche sur les secteurs déjà
  // consultés en ligne : la Rust écrit sur disque, le JS ne fait qu'afficher.

  /** Identifiant de fond attendu par `get_tile`, indexé par nom d'affichage. */
  const CLES_FOND = { "Photo aérienne": "photo", TOP25: "scan25" };

  /**
   * Couleurs de catégorie — **PROVISOIRES**.
   *
   * Ce ne sont pas des couleurs de charte : les vrais pictogrammes de catégorie
   * seront conçus dans un prompt dédié. Elles n'existent que pour rendre la
   * carte lisible et testable en attendant. Ne rien construire dessus.
   */
  const COULEURS_CATEGORIE = {
    Cat1: "#9aa4b1", // fond de vallée / plateau : non optimal
    Cat2: "#4a90d9", // dépression ouverte : sous-optimale
    Cat3: "#1e4d8b", // dépression fermée : optimale (le cœur de la chasse)
    Cat4: "#5b2d8e", // cuvette de haute montagne : optimale conditionnelle
    Col: "#c47d22",
    Versants: "#4c8066",
  };
  const COULEUR_INCONNUE = "#6b7480";

  /** Vue par défaut quand le projet n'a aucun point positionné. */
  const VERCORS = [45.05, 5.45];
  const ZOOM_MASSIF = 11;

  /**
   * Délai avant de considérer un clic sur marqueur comme « simple ».
   *
   * Un double-clic commence toujours par un clic : sans ce sursis, sélectionner
   * et éditer se déclencheraient tous les deux. 250 ms est le compromis usuel —
   * assez court pour que la sélection reste franche à l'usage.
   */
  const DELAI_DOUBLE_CLIC = 250;

  /**
   * Durée d'un appui long avant qu'il ne compte comme tel, en millisecondes.
   *
   * 500 ms est le seuil d'Android lui-même (`ViewConfiguration`) : s'en écarter
   * ferait diverger la carte du reste du système, et l'appui long est
   * précisément le geste dont l'utilisateur a déjà l'habitude ailleurs.
   */
  const DELAI_APPUI_LONG = 500;

  /**
   * Tolérance de déplacement du doigt pendant un appui long, en pixels CSS.
   *
   * Ce n'est pas du confort : un doigt ganté posé sur une tablette dérive de
   * quelques pixels sans intention de faire glisser la carte, et un seuil trop
   * serré transformerait chaque appui long en panoramique avorté. Au-delà,
   * c'est un panoramique et l'appui est abandonné — c'est ce qui garantit
   * qu'on ne crée jamais un point en déplaçant la carte.
   */
  const TOLERANCE_APPUI_LONG = 12;

  let map = null;
  let groupe = null;
  let rappels = {};
  let menuOuvert = false;
  /**
   * Mode « Ajouter un point » : un appui simple sur la carte crée un point.
   *
   * Hors de ce mode, **aucun** appui simple ne crée quoi que ce soit. C'est
   * tout l'objet de cette bascule : sur une dalle tactile, le premier temps
   * d'un panoramique et le premier temps d'un appui sont le même événement, et
   * l'application ne peut pas les distinguer au moment où elle devrait
   * décider. Plutôt que de deviner, on demande.
   */
  let modeAjout = false;
  /** Bouton de la bascule, gardé pour refléter l'état sans le rechercher. */
  let boutonAjout = null;
  let fonds = {};
  /** Nom d'affichage du fond actif — sert au pré-téléchargement de zone. */
  let fondActifNom = null;

  /** Ids des points sélectionnés pour le croisement de données (ordre non signifiant). */
  const selection = new Set();
  /**
   * Couleur d'anneau par point sélectionné, injectée par `main.js`.
   *
   * La carte ne choisit pas ces couleurs et ne connaît pas le bloc qui les
   * produit : elle les reçoit comme elle reçoit ses rappels, ce qui préserve
   * son autonomie (cf. l'en-tête de ce fichier). Vide = anneau bleu par défaut.
   */
  let couleursSelection = new Map();
  /** Marqueurs visuels indexés par id de point, pour re-styler sans tout redessiner. */
  const visuels = new Map();
  /** Timers de temporisation du clic simple, par id de point. */
  const clicsEnAttente = new Map();

  // -------------------------------------------------------------------------
  // Fonds de carte
  // -------------------------------------------------------------------------

  /**
   * Deux fonds, tous deux servis par la Géoplateforme IGN (`data.geopf.fr`).
   *
   * Testé le 18 août 2026 sur une tuile du Vercors (45.05, 5.45) :
   *
   * | Couche                                    | Point d'entrée              | Résultat |
   * |-------------------------------------------|-----------------------------|----------|
   * | `HR.ORTHOIMAGERY.ORTHOPHOTOS`             | `/wmts` (sans clé)          | 200, zoom natif ≤ 19 |
   * | `ORTHOIMAGERY.ORTHOPHOTOS`                | `/wmts` (sans clé)          | 200 également — les deux marchent |
   * | `GEOGRAPHICALGRIDSYSTEMS.MAPS.SCAN25TOUR` | `/private/wmts` + `apikey`  | 200, zoom natif ≤ 16 |
   * | `GEOGRAPHICALGRIDSYSTEMS.MAPS.SCAN25TOUR` | `/wmts` (sans clé)          | 400 « Layer unknown » |
   * | `GEOGRAPHICALGRIDSYSTEMS.MAPS.SCAN25TOUR` | `/private/wmts` sans clé    | 401 Unauthorized |
   *
   * Deux pièges que ce tableau explique :
   *  - le TOP25 n'est PAS sur `/wmts` mais sur `/private/wmts` ; interroger le
   *    point d'entrée public renvoie « couche inconnue », ce qui ressemble à une
   *    faute de frappe alors que c'est la bonne couche au mauvais endroit ;
   *  - son zoom natif s'arrête à 16. Sans `maxNativeZoom`, tout zoom au-delà
   *    produirait des 404 en rafale — et donc un faux bandeau « fond
   *    indisponible » alors que le service répond parfaitement.
   */
  /**
   * Couche de tuiles qui passe par `get_tile` au lieu d'une URL directe.
   *
   * On étend `L.TileLayer` (pas `L.GridLayer`) précisément pour hériter
   * gratuitement de sa gestion de `maxNativeZoom` : au-delà du zoom natif,
   * Leaflet appelle `createTile` avec des coordonnées **déjà ramenées** au
   * zoom natif (le sur-zoom n'est qu'un agrandissement CSS de cette tuile-là).
   * Seul `createTile` change ; tout le reste (zoom, overzoom, événements
   * `tileload`/`tileerror`) est celui de Leaflet.
   */
  const GwsTileLayer = L.TileLayer.extend({
    createTile(coords, done) {
      const tuile = document.createElement("img");
      tuile.alt = "";

      L.DomEvent.on(tuile, "load", L.Util.bind(this._tileOnLoad, this, done, tuile));
      L.DomEvent.on(tuile, "error", L.Util.bind(this._tileOnError, this, done, tuile));

      const indisponible = () => {
        // Zone jamais visitée en ligne, ou zone blanche : une tuile grisée dit
        // clairement « hors couverture », pas un bug d'affichage muet.
        tuile.classList.add("tile-indisponible");
        done(new Error("tuile indisponible"), tuile);
      };

      if (!invoke) {
        indisponible();
        return tuile;
      }

      invoke("get_tile", {
        source: this.options.gwsSource,
        z: coords.z,
        x: coords.x,
        y: coords.y,
      })
        .then((octets) => {
          // `get_tile` rend une réponse binaire : `octets` est un ArrayBuffer.
          const blob = new Blob([octets], { type: "image/jpeg" });
          tuile._gwsBlobUrl = URL.createObjectURL(blob);
          tuile.src = tuile._gwsBlobUrl;
        })
        .catch(indisponible);

      return tuile;
    },
  });

  /** Fabrique une couche, avec libération du blob une fois la tuile déchargée. */
  function creerFond(gwsSource, options) {
    const couche = new GwsTileLayer("", { ...options, gwsSource });
    couche.on("tileunload", (e) => {
      const tuile = e.tile;
      if (tuile && tuile._gwsBlobUrl) URL.revokeObjectURL(tuile._gwsBlobUrl);
    });
    return couche;
  }

  /**
   * Deux fonds, tous deux servis par la Géoplateforme IGN (`data.geopf.fr`),
   * via le cache disque de `tiles.rs` (voir son en-tête pour l'URL WMTS exacte
   * et le piège du point d'entrée `/private/wmts` du TOP25).
   *
   * `maxNativeZoom` reste ici : c'est ce qui évite de demander au cache/réseau
   * des tuiles au-delà de ce que le fond sait servir (16 pour le TOP25).
   */
  function fondsDeCarte() {
    const photo = creerFond("photo", {
      maxNativeZoom: 19,
      maxZoom: 21,
      attribution: "© IGN / Géoplateforme — BD ORTHO",
    });

    const top25 = creerFond("scan25", {
      maxNativeZoom: 16,
      maxZoom: 21,
      attribution: "© IGN / Géoplateforme — SCAN25® TOP25",
    });

    surveillerTuiles(top25, "top25");

    return { "Photo aérienne": photo, TOP25: top25 };
  }

  // -------------------------------------------------------------------------
  // Surveillance de chargement des tuiles
  // -------------------------------------------------------------------------

  /**
   * État de santé d'un fond, alimenté par les événements de tuiles de Leaflet.
   *
   * Un fond de carte qui ne charge pas ne produit aucune erreur JS : il produit
   * du gris. C'est le pire des symptômes — indiscernable d'une zone sans donnée,
   * d'un mauvais zoom ou d'un bug d'affichage. D'où cette surveillance : compter
   * les échecs, et le dire.
   */
  const sante = {
    top25: { erreurs: 0, succes: 0, signale: false },
  };

  /**
   * Seuil de déclenchement du bandeau.
   *
   * Une tuile isolée peut échouer sur un réseau instable sans que le service
   * soit en panne ; trois échecs sans le moindre succès, en revanche, ne
   * s'expliquent pas autrement que par un fond réellement inaccessible (clé
   * expirée, service coupé, pas de réseau).
   */
  const SEUIL_ECHEC = 3;

  function surveillerTuiles(couche, cle) {
    couche.on("tileerror", () => {
      const etat = sante[cle];
      etat.erreurs++;
      if (etat.succes === 0 && etat.erreurs >= SEUIL_ECHEC && !etat.signale) {
        etat.signale = true;
        afficherAlerteFond();
      }
    });

    couche.on("tileload", () => {
      const etat = sante[cle];
      etat.succes++;
      // Le fond répond : on retire un bandeau posé lors d'une coupure passagère.
      if (etat.signale) {
        etat.signale = false;
        etat.erreurs = 0;
        masquerAlerteFond();
      }
    });
  }

  function afficherAlerteFond() {
    const bandeau = document.getElementById("map-alert");
    if (!bandeau) return;
    bandeau.textContent =
      "Fond TOP25 indisponible — la clé d'accès IGN a peut-être expiré. " +
      "Basculez sur « Photo aérienne » en attendant ; voir le README (§ Fonds de carte) " +
      "pour remplacer la clé.";
    bandeau.hidden = false;
  }

  function masquerAlerteFond() {
    const bandeau = document.getElementById("map-alert");
    if (bandeau) bandeau.hidden = true;
  }

  // -------------------------------------------------------------------------
  // Pré-téléchargement d'une zone pour usage hors-ligne
  // -------------------------------------------------------------------------

  /** Écoute la progression une seule fois, quel que soit le nombre de zones téléchargées. */
  let ecouteProgressionBranchee = false;

  function afficherProgression(texte, ratio) {
    const bloc = document.getElementById("map-download");
    const libelle = document.getElementById("map-download-label");
    const barre = document.getElementById("map-download-bar");
    if (!bloc || !libelle || !barre) return;
    bloc.hidden = false;
    libelle.textContent = texte;
    barre.value = ratio;
  }

  function masquerProgression() {
    const bloc = document.getElementById("map-download");
    if (bloc) bloc.hidden = true;
  }

  function brancherEcouteProgression() {
    if (ecouteProgressionBranchee || !evenements) return;
    ecouteProgressionBranchee = true;
    evenements.listen("tile-download-progress", ({ payload }) => {
      const { downloaded, total } = payload;
      afficherProgression(
        `Téléchargement de la zone : ${downloaded} / ${total} tuile(s)`,
        total > 0 ? downloaded / total : 1
      );
    });
  }

  /**
   * Télécharge toutes les tuiles de la vue actuelle, sur le fond actif, pour
   * le zoom courant ± 2 niveaux — un compromis entre couverture utile sur le
   * terrain et volume de tuiles raisonnable (voir `tiles.rs`, qui applique de
   * toute façon un plafond dur si jamais ce calcul dérivait).
   */
  async function telechargerZoneVisible() {
    if (!map || !invoke) return;

    const source = CLES_FOND[fondActifNom];
    if (!source) return;

    const zoom = Math.round(map.getZoom());
    const bounds = map.getBounds();

    brancherEcouteProgression();
    afficherProgression("Préparation du téléchargement…", 0);

    try {
      const resume = await invoke("download_tile_area", {
        source,
        min_zoom: Math.max(0, zoom - 2),
        max_zoom: zoom + 2,
        north: bounds.getNorth(),
        south: bounds.getSouth(),
        east: bounds.getEast(),
        west: bounds.getWest(),
      });
      afficherProgression(
        `Zone disponible hors-ligne : ${resume.downloaded} tuile(s) téléchargée(s), ` +
          `${resume.already_cached} déjà en cache` +
          (resume.failed > 0 ? `, ${resume.failed} indisponible(s)` : "") +
          ".",
        1
      );
      setTimeout(masquerProgression, 4000);
    } catch (e) {
      afficherProgression(String(e), 0);
      setTimeout(masquerProgression, 6000);
    }
  }

  // -------------------------------------------------------------------------
  // Bascule entre les deux fonds
  // -------------------------------------------------------------------------

  /**
   * Bouton unique de bascule, en bas à droite de la carte.
   *
   * Un contrôle Leaflet plutôt qu'un bouton HTML posé par-dessus : c'est la
   * seule façon d'hériter gratuitement de la gestion des événements de Leaflet
   * (un clic sur le bouton ne doit ni déplacer la carte, ni fermer le menu, ni
   * créer un point).
   */
  function ajouterBascule(nomsFonds) {
    const Bascule = L.Control.extend({
      options: { position: "bottomright" },
      onAdd() {
        const conteneur = L.DomUtil.create("div", "map-toggle leaflet-bar");
        const bouton = L.DomUtil.create("button", "map-toggle-btn", conteneur);
        bouton.type = "button";
        bouton.title = "Changer de fond de carte";

        let index = 0;
        const peindre = () => {
          bouton.textContent = nomsFonds[(index + 1) % nomsFonds.length];
        };
        peindre();

        L.DomEvent.disableClickPropagation(conteneur);
        L.DomEvent.on(bouton, "click", () => {
          map.removeLayer(fonds[nomsFonds[index]]);
          index = (index + 1) % nomsFonds.length;
          map.addLayer(fonds[nomsFonds[index]]);
          fondActifNom = nomsFonds[index];
          // Quitter le TOP25 rend son bandeau d'erreur sans objet.
          if (nomsFonds[index] !== "TOP25") masquerAlerteFond();
          peindre();
        });

        return conteneur;
      },
    });

    fondActifNom = nomsFonds[0];
    new Bascule().addTo(map);
  }


  // -------------------------------------------------------------------------
  // Création d'un point : le mode explicite et l'appui long
  // -------------------------------------------------------------------------
  //
  // ## Pourquoi deux gestes et pas un
  //
  // Le clic droit restait le seul chemin vers « Créer un point ici ». Sur un
  // poste fixe il est parfait ; sur une tablette il n'existe pas. Leaflet
  // synthétise bien un `contextmenu` sur appui long, mais sans aucun retour
  // visuel : le doigt reste posé sur une carte qui ne bouge pas, et rien ne
  // dit si l'appui est compté, trop court, ou perdu parce que le doigt a
  // dérivé. Sur le terrain, cela se solde par des appuis répétés et des points
  // créés en double.
  //
  // D'où les deux chemins, qui ne visent pas le même moment :
  //
  //  * le **mode explicite** pour une campagne de saisie — on l'arme une fois,
  //    on pose les points, on le désarme. Le geste est un appui simple, le plus
  //    sûr de tous, et la bordure de la carte dit en permanence dans quel mode
  //    on se trouve ;
  //  * l'**appui long** pour le point isolé, quand armer un mode pour un seul
  //    point coûterait plus cher que le point lui-même. Il est ici redessiné
  //    avec un anneau qui se remplit : on voit l'appui être compté, et
  //    l'abandonner en glissant devient un choix plutôt qu'un échec.
  //
  // Les deux aboutissent au même `rappels.onCreateHere` que le clic droit du
  // poste fixe — il n'y a pas de seconde voie de création à maintenir.

  /** Chronomètre de l'appui long en cours, ou `null`. */
  let minuteurAppui = null;
  /** Anneau de progression affiché sous le doigt, ou `null`. */
  let anneauAppui = null;
  /**
   * Vrai juste après qu'un appui long a abouti.
   *
   * Android émet *aussi* son propre `contextmenu` synthétique sur le même
   * geste, un souffle après le nôtre. Sans ce verrou, le menu s'ouvrirait, se
   * refermerait et se rouvrirait décalé de quelques pixels.
   */
  let appuiLongAbouti = false;

  /** Retire l'anneau et le chronomètre, quelle qu'en soit la raison. */
  function annulerAppuiLong() {
    if (minuteurAppui !== null) {
      clearTimeout(minuteurAppui);
      minuteurAppui = null;
    }
    if (anneauAppui) {
      anneauAppui.remove();
      anneauAppui = null;
    }
  }

  /**
   * Arme la reconnaissance de l'appui long sur le conteneur de la carte.
   *
   * Les écouteurs sont posés sur le DOM et non sur Leaflet : Leaflet ne publie
   * pas le début d'un appui, seulement son issue, et c'est exactement le
   * moment qu'on veut rendre visible. `passive: true` sur `touchstart` et
   * `touchmove` est délibéré — ces écouteurs ne préviennent jamais le geste,
   * ils l'observent, et le panoramique de Leaflet doit rester aussi fluide
   * qu'avant.
   */
  function armerAppuiLong(conteneur) {
    conteneur.addEventListener(
      "touchstart",
      (e) => {
        // Deux doigts, c'est un pincement pour zoomer : rien à reconnaître ici.
        if (e.touches.length !== 1) {
          annulerAppuiLong();
          return;
        }
        // En mode ajout, l'appui simple suffit déjà : ajouter l'appui long
        // par-dessus ferait deux gestes pour un seul résultat.
        if (modeAjout) return;

        const toucher = e.touches[0];
        const rect = conteneur.getBoundingClientRect();
        const depart = { x: toucher.clientX, y: toucher.clientY };

        annulerAppuiLong();
        anneauAppui = document.createElement("div");
        anneauAppui.className = "map-appui-anneau";
        anneauAppui.style.left = `${depart.x - rect.left}px`;
        anneauAppui.style.top = `${depart.y - rect.top}px`;
        anneauAppui.style.setProperty("--duree-appui", `${DELAI_APPUI_LONG}ms`);
        conteneur.appendChild(anneauAppui);

        const surveiller = (ev) => {
          const t = ev.touches[0];
          if (!t) return;
          const dx = t.clientX - depart.x;
          const dy = t.clientY - depart.y;
          if (Math.hypot(dx, dy) > TOLERANCE_APPUI_LONG) {
            conteneur.removeEventListener("touchmove", surveiller);
            annulerAppuiLong();
          }
        };
        conteneur.addEventListener("touchmove", surveiller, { passive: true });
        conteneur.addEventListener(
          "touchend",
          () => conteneur.removeEventListener("touchmove", surveiller),
          { once: true }
        );

        minuteurAppui = setTimeout(() => {
          minuteurAppui = null;
          annulerAppuiLong();
          if (!map) return;

          // Une pulsation courte confirme l'appui alors que le doigt masque
          // encore l'endroit touché : c'est le seul retour perceptible quand
          // on regarde le terrain et pas l'écran. Absent de la WebView de
          // bureau, d'où la garde.
          if (navigator.vibrate) {
            try {
              navigator.vibrate(18);
            } catch (err) {
              /* Vibration refusée par la politique de la page : sans effet. */
            }
          }

          appuiLongAbouti = true;
          setTimeout(() => {
            appuiLongAbouti = false;
          }, 700);

          const point = L.point(depart.x - rect.left, depart.y - rect.top);
          ouvrirMenu(map.containerPointToLatLng(point), point);
        }, DELAI_APPUI_LONG);
      },
      { passive: true }
    );

    for (const evenement of ["touchend", "touchcancel"]) {
      conteneur.addEventListener(evenement, () => annulerAppuiLong(), {
        passive: true,
      });
    }
  }

  /**
   * Arme ou désarme le mode « Ajouter un point ».
   *
   * @param {boolean} actif état voulu.
   */
  function basculerModeAjout(actif) {
    modeAjout = !!actif;
    annulerAppuiLong();
    fermerMenu();

    if (boutonAjout) {
      boutonAjout.classList.toggle("is-actif", modeAjout);
      boutonAjout.setAttribute("aria-pressed", modeAjout ? "true" : "false");
      boutonAjout.title = modeAjout
        ? "Quitter le mode ajout (Échap)"
        : "Ajouter un point : toucher ensuite la carte";
    }
    if (map) {
      // La classe porte la bordure d'état et le curseur en croix. Elle est sur
      // le conteneur et non sur `<body>` : deux cartes coexisteraient sans se
      // marcher dessus, et une carte détruite emporte son état avec elle.
      map.getContainer().classList.toggle("is-mode-ajout", modeAjout);
    }

    const banniere = document.getElementById("map-mode-banner");
    if (banniere) banniere.hidden = !modeAjout;
  }


  // -------------------------------------------------------------------------
  // Position exploitable
  // -------------------------------------------------------------------------

  /**
   * Un point sans position utilisable ne doit pas casser la carte : il n'est
   * simplement pas dessiné, et reste visible dans la liste.
   *
   * En pratique `latitude`/`longitude` sont `REAL NOT NULL` et validées à la
   * création, donc le seul cas réellement atteignable est le (0, 0) saisi comme
   * remplissage — au large du golfe de Guinée, à 5 000 km du Vercors. Les gardes
   * `isFinite` protègent d'une base éditée à la main.
   */
  function positionUtilisable(p) {
    return (
      Number.isFinite(p.latitude) &&
      Number.isFinite(p.longitude) &&
      Math.abs(p.latitude) <= 90 &&
      Math.abs(p.longitude) <= 180 &&
      !(Math.abs(p.latitude) < 1e-6 && Math.abs(p.longitude) < 1e-6)
    );
  }

  // -------------------------------------------------------------------------
  // Menu contextuel
  // -------------------------------------------------------------------------

  function fermerMenu() {
    const menu = document.getElementById("map-menu");
    if (menu) {
      menu.hidden = true;
      while (menu.firstChild) menu.removeChild(menu.firstChild);
      menu.onkeydown = null;
    }
    menuOuvert = false;
  }

  function positionnerMenu(menu, containerPoint) {
    const rect = map.getContainer().getBoundingClientRect();
    menu.style.left = `${rect.left + containerPoint.x + window.scrollX}px`;
    menu.style.top = `${rect.top + containerPoint.y + window.scrollY}px`;
    menu.hidden = false;
    menuOuvert = true;
  }

  function ouvrirMenu(latlng, containerPoint) {
    const menu = document.getElementById("map-menu");
    if (!menu) return;
    fermerMenu();

    const item = document.createElement("button");
    item.type = "button";
    item.className = "map-menu-item";
    item.textContent = "Créer un point ici";
    item.addEventListener("click", () => {
      fermerMenu();
      if (rappels.onCreateHere) rappels.onCreateHere(latlng.lat, latlng.lng);
    });

    const telecharger = document.createElement("button");
    telecharger.type = "button";
    telecharger.className = "map-menu-item";
    telecharger.textContent = "Télécharger cette zone pour usage hors-ligne";
    telecharger.addEventListener("click", () => {
      fermerMenu();
      telechargerZoneVisible();
    });

    const coords = document.createElement("p");
    coords.className = "map-menu-coords";
    coords.textContent = `${latlng.lat.toFixed(5)}, ${latlng.lng.toFixed(5)}`;

    menu.appendChild(item);
    menu.appendChild(telecharger);
    menu.appendChild(coords);
    positionnerMenu(menu, containerPoint);
  }

  /**
   * Menu contextuel d'un marqueur.
   *
   * Le double-clic reste le geste d'édition documenté, mais il n'est pas
   * découvrable : ce menu donne une seconde porte d'entrée, visible.
   */
  function ouvrirMenuPoint(point, containerPoint) {
    const menu = document.getElementById("map-menu");
    if (!menu) return;
    fermerMenu();

    const titre = document.createElement("p");
    titre.className = "map-menu-title";
    titre.textContent = point.name;

    // Les deux premières entrées réutilisent la fiche et la récupération déjà
    // proposées ailleurs (rappels injectés par `main.js`) : rien n'est
    // dupliqué ici. Sans rappel fourni, l'entrée correspondante est omise.
    const entrees = [];
    if (rappels.onOpenDetail) {
      const details = document.createElement("button");
      details.type = "button";
      details.className = "map-menu-item";
      details.textContent = "Détails";
      details.addEventListener("click", () => {
        fermerMenu();
        rappels.onOpenDetail(point.id);
      });
      entrees.push(details);
    }
    if (rappels.onFetchData) {
      const recuperer = document.createElement("button");
      recuperer.type = "button";
      recuperer.className = "map-menu-item";
      recuperer.textContent = "Récupérer les données";
      recuperer.addEventListener("click", () => {
        fermerMenu();
        rappels.onFetchData(point.id);
      });
      entrees.push(recuperer);
    }

    const editer = document.createElement("button");
    editer.type = "button";
    editer.className = "map-menu-item";
    editer.textContent = "Modifier ce point";
    editer.addEventListener("click", () => {
      fermerMenu();
      if (rappels.onEditPoint) rappels.onEditPoint(point.id);
    });

    const selectionner = document.createElement("button");
    selectionner.type = "button";
    selectionner.className = "map-menu-item";
    selectionner.textContent = selection.has(point.id)
      ? "Retirer de la sélection"
      : "Ajouter à la sélection";
    selectionner.addEventListener("click", () => {
      fermerMenu();
      basculerSelection(point.id);
    });

    menu.appendChild(titre);
    for (const b of entrees) menu.appendChild(b);
    menu.appendChild(editer);
    menu.appendChild(selectionner);
    positionnerMenu(menu, containerPoint);

    // Clavier : le focus passe à la première entrée, les flèches parcourent
    // le menu (Échap est déjà géré globalement). Sans effet sur la souris.
    const boutons = [...menu.querySelectorAll(".map-menu-item")];
    menu.onkeydown = (e) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      e.preventDefault();
      const i = boutons.indexOf(document.activeElement);
      const pas = e.key === "ArrowDown" ? 1 : -1;
      boutons[(i + pas + boutons.length) % boutons.length].focus();
    };
    if (boutons.length) boutons[0].focus({ preventScroll: true });
  }

  // -------------------------------------------------------------------------
  // Sélection multiple
  // -------------------------------------------------------------------------

  /**
   * Style d'un marqueur selon son état de sélection.
   *
   * Un point sélectionné garde sa couleur de catégorie **au centre** —
   * l'information la plus utile ne doit pas être écrasée par l'état
   * d'interface. C'est l'anneau qui change : blanc fin au repos, épais une fois
   * sélectionné.
   *
   * La couleur de cet anneau est celle de la courbe du point dans le bloc
   * « Croisement de données » (`GwsCross.paletteFor`, poussée ici par
   * `setSelectionColors`). Comparer deux dépressions fermées — le cas d'usage
   * central — donne deux marqueurs de la même couleur de catégorie ; l'anneau
   * est ce qui permet de savoir laquelle est laquelle sur le graphique.
   * Sans couleur fournie, on retombe sur le bleu d'accent d'origine.
   */
  function peindreMarqueur(id, couleur) {
    const visuel = visuels.get(id);
    if (!visuel) return;
    const choisi = selection.has(id);
    visuel.setStyle({
      radius: choisi ? 8 : 6,
      weight: choisi ? 3 : 2,
      color: choisi ? couleursSelection.get(id) || "#1e4d8b" : "#ffffff",
      fillColor: couleur,
      fillOpacity: 0.95,
    });
  }

  /**
   * Injecte les couleurs d'anneau (`Map` id → couleur CSS) et repeint.
   *
   * Ne repeint que les marqueurs sélectionnés : les autres n'ont pas d'anneau
   * coloré, et redessiner toute la couche à chaque clic se voit sur un projet
   * un peu fourni.
   */
  function setSelectionColors(couleurs) {
    couleursSelection = couleurs instanceof Map ? couleurs : new Map();
    for (const id of selection) {
      const visuel = visuels.get(id);
      if (visuel) peindreMarqueur(id, visuel.options.fillColor);
    }
  }

  function notifierSelection() {
    if (rappels.onSelectionChange) rappels.onSelectionChange([...selection]);
  }

  function basculerSelection(id) {
    if (selection.has(id)) selection.delete(id);
    else selection.add(id);
    const visuel = visuels.get(id);
    if (visuel) peindreMarqueur(id, visuel.options.fillColor);
    notifierSelection();
  }

  /** Vide la sélection (bouton « tout désélectionner », changement de projet). */
  function clearSelection() {
    if (selection.size === 0) return;
    const ids = [...selection];
    selection.clear();
    for (const id of ids) {
      const visuel = visuels.get(id);
      if (visuel) peindreMarqueur(id, visuel.options.fillColor);
    }
    notifierSelection();
  }

  /** Sélection courante — lue par le bloc « Croisement de données ». */
  function getSelection() {
    return [...selection];
  }

  // -------------------------------------------------------------------------
  // API publique
  // -------------------------------------------------------------------------

  /**
   * Crée la carte si elle n'existe pas encore.
   *
   * ⚠️ Le conteneur doit être **visible** au moment de l'appel : Leaflet mesure
   * sa taille à la création. `#view-project` étant `hidden` au démarrage, il faut
   * l'afficher d'abord, puis appeler `ensure()`, puis `refresh()`.
   */
  function ensure(containerId, handlers) {
    rappels = handlers || {};
    if (map) return map;

    const conteneur = document.getElementById(containerId);
    if (!conteneur || typeof L === "undefined") return null;

    fonds = fondsDeCarte();
    const noms = Object.keys(fonds);

    map = L.map(conteneur, {
      center: VERCORS,
      zoom: ZOOM_MASSIF,
      // Zoom molette centré sur le curseur : c'est le comportement par défaut de
      // Leaflet (seul `scrollWheelZoom: 'center'` recentrerait sur la carte).
      // Explicite ici pour que personne ne « corrige » ça plus tard.
      scrollWheelZoom: true,
      // Rendu canvas : un seul élément pour tous les cercles, au lieu de deux
      // nœuds SVG par point — indispensable au-delà de quelques centaines de sites.
      preferCanvas: true,
      dragging: true,
      zoomControl: true,
      layers: [fonds[noms[0]]],
    });

    ajouterBascule(noms);
    L.control.scale({ imperial: false, position: "bottomleft" }).addTo(map);

    groupe = L.featureGroup().addTo(map);
    // Les cases de regroupement dépendent du zoom (pas du déplacement).
    map.on("zoomend", () => {
      if (pointsDessinables.length > SEUIL_REGROUPEMENT) dessinerMarqueurs();
    });

    map.on("contextmenu", (e) => {
      // Le `contextmenu` synthétique d'Android arrive juste après notre propre
      // appui long, sur le même geste (cf. `appuiLongAbouti`).
      if (appuiLongAbouti) return;
      ouvrirMenu(e.latlng, e.containerPoint);
    });

    map.on("click", (e) => {
      if (modeAjout) {
        // Un point par armement : laisser le mode actif ferait du moindre
        // appui de vérification un point de plus, et c'est sur la carte qu'on
        // vérifie ce qu'on vient de poser.
        basculerModeAjout(false);
        if (rappels.onCreateHere) rappels.onCreateHere(e.latlng.lat, e.latlng.lng);
        return;
      }
      fermerMenu();
    });

    // Le panoramique abandonne l'appui long : sans cela, un glissement lent et
    // court resterait sous la tolérance et ouvrirait le menu en fin de course.
    map.on("movestart", () => {
      annulerAppuiLong();
      fermerMenu();
    });

    armerAppuiLong(conteneur);

    document.addEventListener("keydown", (e) => {
      if (e.key !== "Escape") return;
      // Échap défait le dernier état posé : le menu s'il est ouvert, sinon le
      // mode. L'inverse laisserait le menu orphelin au-dessus d'une carte
      // revenue au repos.
      if (menuOuvert) fermerMenu();
      else if (modeAjout) basculerModeAjout(false);
    });

    return map;
  }

  /**
   * Regroupement des marqueurs, au-delà de SEUIL_REGROUPEMENT points (projet
   * France : 1 153 stations). Grille fixe en pixels au zoom courant : chaque
   * case de TAILLE_CASE px qui contient plusieurs points devient une pastille
   * chiffrée ; un clic zoome dessus. Au-dessous du seuil, rien ne change.
   */
  const SEUIL_REGROUPEMENT = 500;
  const TAILLE_CASE = 56;
  /** À ce zoom et au-delà, chaque point redevient visible. */
  const ZOOM_SANS_REGROUPEMENT = 13;
  let pointsDessinables = [];

  function dessinerMarqueurs() {
    groupe.clearLayers();
    visuels.clear();
    for (const timer of clicsEnAttente.values()) clearTimeout(timer);
    clicsEnAttente.clear();

    const zoom = map.getZoom();
    if (pointsDessinables.length <= SEUIL_REGROUPEMENT || !(zoom < ZOOM_SANS_REGROUPEMENT)) {
      for (const p of pointsDessinables) creerMarqueur(p);
      return;
    }
    const cases = new Map();
    for (const p of pointsDessinables) {
      const px = map.project([p.latitude, p.longitude], zoom);
      const cle = `${Math.floor(px.x / TAILLE_CASE)}:${Math.floor(px.y / TAILLE_CASE)}`;
      const c = cases.get(cle);
      if (c) c.push(p);
      else cases.set(cle, [p]);
    }
    for (const membres of cases.values()) {
      if (membres.length === 1) {
        creerMarqueur(membres[0]);
        continue;
      }
      let lat = 0, lon = 0;
      for (const m of membres) { lat += m.latitude; lon += m.longitude; }
      const n = membres.length;
      const taille = n < 10 ? 26 : n < 100 ? 32 : 38;
      const pastille = L.marker([lat / n, lon / n], {
        icon: L.divIcon({
          className: "map-cluster",
          html: `<span>${n}</span>`,
          iconSize: [taille, taille],
        }),
        keyboard: false,
      });
      pastille.on("click", (e) => {
        L.DomEvent.stopPropagation(e);
        const bornes = L.latLngBounds(membres.map((m) => [m.latitude, m.longitude]));
        map.fitBounds(bornes.pad(0.2), { maxZoom: Math.max(zoom + 2, ZOOM_SANS_REGROUPEMENT) });
      });
      groupe.addLayer(pastille);
    }
  }

  /** Redessine tous les marqueurs et recadre la vue. */
  function setPoints(points) {
    if (!map || !groupe) return;

    const dessinables = (points || []).filter(positionUtilisable);
    const ignores = (points || []).length - dessinables.length;

    // Un point supprimé (ou devenu impositionnable) ne doit pas rester
    // fantôme dans la sélection : le croisement porterait sur un id sans objet.
    const vivants = new Set(dessinables.map((p) => p.id));
    let selectionModifiee = false;
    for (const id of [...selection]) {
      if (!vivants.has(id)) {
        selection.delete(id);
        selectionModifiee = true;
      }
    }

    pointsDessinables = dessinables;
    dessinerMarqueurs();

    if (selectionModifiee) notifierSelection();

    const note = document.getElementById("map-note");
    if (note) {
      if (ignores > 0) {
        note.textContent = `${ignores} point(s) sans position exploitable — visibles dans la liste uniquement.`;
        note.hidden = false;
      } else {
        note.hidden = true;
      }
    }

    cadrer(dessinables.length > 0);
  }

  /** Marqueur d'un point : cible de clic invisible + pastille visible. */
  function creerMarqueur(p) {
    {
      const latlng = [p.latitude, p.longitude];
      const couleur = COULEURS_CATEGORIE[p.category] || COULEUR_INCONNUE;

      // Deux cercles superposés : une cible large et invisible pour le clic, un
      // marqueur discret pour l'œil. `fillOpacity: 0.001` plutôt que 0 — une
      // surface strictement nulle peut être ignorée par le test de survol.
      const zoneClic = L.circleMarker(latlng, {
        radius: 16,
        opacity: 0,
        fillOpacity: 0.001,
        interactive: true,
      });
      const visuel = L.circleMarker(latlng, {
        radius: 6,
        weight: 2,
        color: "#ffffff",
        fillColor: couleur,
        fillOpacity: 0.95,
        interactive: false,
      });

      visuels.set(p.id, visuel);

      const taille = p.size ? ` ${p.size}` : "";
      // Un nœud DOM et non une chaîne : Leaflet insère les chaînes par
      // `innerHTML`, et le nom d'un point peut venir d'un partage reçu. Avec
      // `textContent`, un nom comme « <style>… » s'affiche tel quel au lieu
      // d'être interprété (audit de sécurité du 18/09, §4).
      const infobulle = document.createElement("span");
      infobulle.className = "map-marker-tooltip";
      infobulle.textContent = `${p.name} — ${p.category}${taille}`;
      zoneClic.bindTooltip(infobulle, { direction: "top", offset: [0, -6] });

      /*
       * Clic simple = sélection, double-clic = édition.
       *
       * Leaflet émet un `click` avant tout `dblclick` : sans temporisation, le
       * double-clic sélectionnerait puis désélectionnerait le point au passage.
       * On diffère donc la sélection de `DELAI_DOUBLE_CLIC`, quitte à ce que
       * l'anneau apparaisse un quart de seconde plus tard.
       */
      zoneClic.on("click", (e) => {
        // En mode ajout, un marqueur n'est plus une cible mais un simple
        // décor : on laisse l'événement remonter à la carte, qui créera le
        // point. Sans cela, viser près d'un point existant ne ferait rien du
        // tout — le cas le plus fréquent, puisqu'on repose un point là où il y
        // en a déjà.
        if (modeAjout) return;
        // Sans cela, le clic remonte à la carte et referme le menu contextuel
        // au moment même où l'on ouvre la fiche.
        L.DomEvent.stopPropagation(e);
        fermerMenu();
        if (clicsEnAttente.has(p.id)) return;
        const timer = setTimeout(() => {
          clicsEnAttente.delete(p.id);
          basculerSelection(p.id);
        }, DELAI_DOUBLE_CLIC);
        clicsEnAttente.set(p.id, timer);
      });

      zoneClic.on("dblclick", (e) => {
        // Sans quoi le double-clic zoomerait la carte en plus d'ouvrir la fiche.
        L.DomEvent.stopPropagation(e);
        L.DomEvent.preventDefault(e);
        const timer = clicsEnAttente.get(p.id);
        if (timer) {
          clearTimeout(timer);
          clicsEnAttente.delete(p.id);
        }
        fermerMenu();
        if (rappels.onEditPoint) rappels.onEditPoint(p.id);
      });

      zoneClic.on("contextmenu", (e) => {
        L.DomEvent.stopPropagation(e);
        ouvrirMenuPoint(p, e.containerPoint);
      });

      groupe.addLayer(zoneClic);
      groupe.addLayer(visuel);
      peindreMarqueur(p.id, couleur);
    }
  }

  /**
   * Recadre la vue sur les marqueurs, ou sur le Vercors si le projet est vide.
   *
   * ⚠️ Se protège d'un conteneur pas encore mesuré : `fitBounds` sur une carte de
   * taille nulle ne lève aucune erreur, elle retombe au zoom 0 et affiche le monde
   * entier. Le symptôme (« la carte est dézoomée à fond ») ne dit rien de la cause,
   * d'où ce garde-fou plutôt qu'une dépendance à l'ordre des appels.
   */
  function cadrer(aDesPoints) {
    if (!map) return;

    map.invalidateSize();
    const taille = map.getSize();
    if (taille.x === 0 || taille.y === 0) {
      // Le conteneur n'a pas encore de dimensions : on réessaie à la frame suivante.
      window.requestAnimationFrame(() => cadrer(aDesPoints));
      return;
    }

    if (aDesPoints && groupe.getLayers().length > 0) {
      map.fitBounds(groupe.getBounds().pad(0.15), { maxZoom: 16 });
    } else {
      map.setView(VERCORS, ZOOM_MASSIF);
    }
  }

  /** Recalcule les dimensions — après affichage du conteneur ou un resize. */
  function refresh() {
    if (map) map.invalidateSize();
  }

  /** Centre la carte sur un point (sélection depuis la liste). */
  function focus(pointId, points) {
    if (!map) return;
    const p = (points || []).find((x) => x.id === pointId);
    if (p && positionUtilisable(p)) {
      map.panTo([p.latitude, p.longitude]);
    }
  }

  /** Détruit la carte au retour vers la liste des projets. */
  function destroy() {
    fermerMenu();
    // Avant `map.remove()` : la bascule va chercher le conteneur pour y retirer
    // sa classe d'état. Après, il n'y aurait plus de conteneur, et le mode
    // resterait armé pour le projet suivant — qui recevrait un point au premier
    // appui destiné à lire la carte.
    basculerModeAjout(false);
    boutonAjout = null;
    annulerAppuiLong();
    masquerAlerteFond();
    masquerProgression();
    for (const timer of clicsEnAttente.values()) clearTimeout(timer);
    clicsEnAttente.clear();
    selection.clear();
    couleursSelection = new Map();
    visuels.clear();
    sante.top25 = { erreurs: 0, succes: 0, signale: false };
    if (map) {
      map.remove();
      map = null;
      groupe = null;
      fonds = {};
      fondActifNom = null;
    }
  }

  return {
    ensure,
    setPoints,
    /** Arme/désarme le mode ajout depuis l'extérieur (raccourci, bouton hôte). */
    setAddMode: basculerModeAjout,
    isAddMode: () => modeAjout,
    refresh,
    focus,
    destroy,
    positionUtilisable,
    getSelection,
    setSelectionColors,
    toggleSelection: basculerSelection,
    clearSelection,
  };
})();

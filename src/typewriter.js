/*
 * Composant « machine à écrire » — écrit une fois, utilisé trois fois.
 *
 * Les usages ont des contraintes opposées, et c'est ce qui a dicté l'API :
 *
 *  - l'écran d'accueil écrit une phrase courte, lentement, et attend la fin pour
 *    faire apparaître ses trois boutons → il a besoin d'une promesse ;
 *  - le bloc « Agent IA » écrit un paragraphe entier et doit pouvoir être
 *    interrompu (nouvelle sélection, nouvelle question) sans laisser deux
 *    animations écrire dans le même nœud → il a besoin d'annuler ;
 *  - ce même bloc affiche du Markdown rendu (listes, gras) → il a besoin
 *    d'écrire dans une **arborescence**, pas dans une chaîne.
 *
 * D'où une fonction unique qui renvoie un objet {promise, skip, cancel}. Un
 * appel sur un nœud déjà en cours d'animation annule le précédent : c'est la
 * garantie qui évite l'entrelacement de deux réponses.
 *
 * ## Écrire dans du DOM sans le casser
 *
 * Animer du HTML caractère par caractère en concaténant une chaîne est une
 * fausse bonne idée : à mi-parcours, le nœud contient « <str » — soit un
 * balisage invalide que le navigateur réinterprète à chaque frappe. La solution
 * retenue est l'inverse : l'arborescence est construite **entière et tout de
 * suite** (voir `markdown.js`), puis vidée de son texte ; l'animation ne fait
 * que redonner ses caractères, dans l'ordre du document. Aucune balise n'est
 * jamais coupée, parce qu'aucune balise n'est jamais écrite.
 *
 * Corollaire : un élément dont aucun caractère n'est encore arrivé est masqué
 * (`.tw-attente`). Sans cela, toutes les puces d'une liste apparaîtraient d'un
 * coup avant même leur premier mot, ce qui vend la mèche et annule l'effet.
 */

const GwsTypewriter = (() => {
  /** Millisecondes par caractère, par défaut. */
  const VITESSE_DEFAUT = 22;

  /**
   * Pauses supplémentaires après certains caractères. Sans elles, l'effet est
   * régulier comme un métronome et se lit comme un défilement, pas comme une
   * frappe.
   */
  const PAUSES = { ".": 260, "?": 260, "!": 260, ",": 110, ";": 110, ":": 140, "\n": 200 };

  /** Animations en cours, indexées par nœud cible. */
  const enCours = new WeakMap();

  /** Classe posée sur un élément dont aucun caractère n'est encore écrit. */
  const CLASSE_ATTENTE = "tw-attente";

  /**
   * Classe posée sur l'élément où la frappe est en cours — c'est lui qui porte
   * le curseur en mode riche. Sur le conteneur, le curseur atterrirait sous le
   * dernier paragraphe (un `::after` de bloc), et non au bout du mot en train de
   * s'écrire.
   */
  const CLASSE_CURSEUR = "tw-curseur";

  // -------------------------------------------------------------------------
  // Écrivains : deux façons de révéler un texte, une seule interface
  // -------------------------------------------------------------------------
  //
  // { texte: string, afficher(n): void } — `texte` sert à connaître la longueur
  // totale et le caractère courant (pour les pauses et `onChar`), `afficher`
  // matérialise les `n` premiers caractères. Toute la mécanique de temporisation
  // est écrite une seule fois, contre cette interface.

  /** Cas simple : un `<span>` dont on tronque le texte. */
  function ecrivainTexte(hote, texte) {
    hote.textContent = "";
    // Le texte s'écrit dans un span, le curseur est un pseudo-élément du parent :
    // sans cela, le curseur se retrouverait *avant* le texte à chaque réécriture.
    const span = hote.ownerDocument.createElement("span");
    hote.appendChild(span);
    return {
      texte,
      afficher(n) {
        span.textContent = n >= texte.length ? texte : texte.slice(0, n);
      },
    };
  }

  /**
   * Cas riche : une arborescence déjà construite, vidée puis re-remplie.
   *
   * Le parcours relève, dans l'ordre du document, chaque nœud de texte (avec son
   * contenu, mis de côté) et chaque élément (avec l'intervalle de caractères
   * qu'il couvre). `afficher` redistribue ensuite les `n` caractères aux nœuds
   * de texte dans cet ordre, puis démasque les éléments atteints.
   */
  function ecrivainRiche(hote, fragment) {
    hote.textContent = "";
    hote.classList.add("tw-rich");
    hote.appendChild(fragment);

    const morceaux = [];
    const elements = [];
    let total = 0;

    (function parcourir(parent) {
      for (const enfant of Array.from(parent.childNodes)) {
        if (enfant.nodeType === 3) {
          morceaux.push({ noeud: enfant, texte: enfant.nodeValue });
          total += enfant.nodeValue.length;
          enfant.nodeValue = "";
        } else if (enfant.nodeType === 1) {
          // La référence est gardée en local : `elements` grossit pendant la
          // descente, son dernier élément n'est plus celui qu'on traite.
          const entree = { el: enfant, debut: total };
          elements.push(entree);
          parcourir(enfant);
          entree.fin = total;
        }
      }
    })(hote);

    const texte = morceaux.map((m) => m.texte).join("");

    return {
      texte,
      afficher(n) {
        let reste = n;
        for (const m of morceaux) {
          const pris = Math.max(0, Math.min(reste, m.texte.length));
          m.noeud.nodeValue = pris === m.texte.length ? m.texte : m.texte.slice(0, pris);
          reste -= pris;
        }
        // Élément où la frappe se trouve : le dernier, dans l'ordre du document,
        // qui contienne la position d'écriture. Pour « …dans un <strong>mot</strong> »
        // c'est le `<strong>` tant qu'on est dedans, le `<p>` ensuite — donc
        // toujours l'endroit exact où le curseur doit se poser.
        let porteur = null;
        for (const e of elements) {
          // Un élément se montre dès son premier caractère. Ceux qui n'en
          // portent aucun (`<br>`) se montrent quand l'écriture les atteint.
          const enAttente = e.fin === e.debut ? n < e.debut : n <= e.debut;
          e.el.classList.toggle(CLASSE_ATTENTE, enAttente);
          if (e.debut < n && n <= e.fin) porteur = e.el;
        }
        for (const e of elements) e.el.classList.toggle(CLASSE_CURSEUR, e.el === porteur);
      },
    };
  }

  // -------------------------------------------------------------------------
  // Animation
  // -------------------------------------------------------------------------

  /**
   * Écrit `texte` dans `node`, caractère par caractère.
   *
   * @param {HTMLElement} node      nœud cible (son contenu est remplacé)
   * @param {string}      texte     texte à écrire
   * @param {object}      [options]
   * @param {number}      [options.speed]      ms par caractère
   * @param {boolean}     [options.caret]      afficher un curseur clignotant
   * @param {Function}    [options.onChar]     appelé à chaque caractère écrit
   * @returns {{promise: Promise<void>, skip: Function, cancel: Function, done: Function}}
   */
  function type(node, texte, options = {}) {
    return animer(node, (hote) => ecrivainTexte(hote, texte), options);
  }

  /**
   * Même animation, sur une arborescence déjà construite.
   *
   * L'appelant fournit le DOM (typiquement `GwsMarkdown.toDom(texte)`) : ce
   * module ne connaît ni Markdown ni HTML, il ne sait que révéler des caractères
   * là où ils sont.
   *
   * @param {HTMLElement}    node
   * @param {DocumentFragment} fragment
   * @param {object}         [options]  identiques à [`type`]
   */
  function typeRich(node, fragment, options = {}) {
    return animer(node, (hote) => ecrivainRiche(hote, fragment), options);
  }

  function animer(node, fabriquerEcrivain, options) {
    const vitesse = options.speed ?? VITESSE_DEFAUT;
    const avecCaret = options.caret !== false;

    // Une seule animation par nœud : la nouvelle chasse l'ancienne.
    cancel(node);

    node.classList.add("tw");
    node.classList.toggle("tw-caret", avecCaret);
    // `ecrivainRiche` la repose s'il s'agit bien d'un rendu riche : on part
    // toujours d'un nœud propre, même si le précédent appel était de l'autre
    // mode.
    node.classList.remove("tw-rich");

    const ecrivain = fabriquerEcrivain(node);
    const texte = ecrivain.texte;
    ecrivain.afficher(0);

    let index = 0;
    let timer = null;
    let resoudre;
    let annulee = false;

    const promise = new Promise((r) => {
      resoudre = r;
    });

    /**
     * L'utilisateur qui a déjà lu la phrase ne doit pas attendre : `skip`
     * affiche tout d'un coup et tient la promesse normalement — l'appelant n'a
     * donc pas de cas particulier à traiter.
     */
    function terminer() {
      if (annulee) return;
      clearTimeout(timer);
      index = texte.length;
      ecrivain.afficher(index);
      node.classList.remove("tw-caret");
      enCours.delete(node);
      resoudre();
    }

    function etape() {
      if (annulee) return;
      if (index >= texte.length) {
        terminer();
        return;
      }
      const c = texte[index++];
      ecrivain.afficher(index);
      if (options.onChar) options.onChar(c, index);

      // `prefers-reduced-motion` est respecté en amont (voir `instant`), donc
      // arrivé ici l'animation est voulue.
      const delai = vitesse + (PAUSES[c] || 0);
      timer = setTimeout(etape, delai);
    }

    const controleur = {
      promise,
      skip: terminer,
      done: () => index >= texte.length,
      cancel: () => {
        annulee = true;
        clearTimeout(timer);
        node.classList.remove("tw-caret");
        enCours.delete(node);
        // La promesse est **résolue**, pas rejetée : une animation interrompue
        // est un événement normal, pas une erreur à attraper partout.
        resoudre();
      },
    };
    enCours.set(node, controleur);

    // Accessibilité : une personne qui a demandé moins d'animations n'a aucune
    // raison d'attendre qu'une phrase s'écrive avant de pouvoir agir.
    if (prefereMoinsDAnimations()) {
      terminer();
    } else {
      timer = setTimeout(etape, delaiInitial(options));
    }

    return controleur;
  }

  function delaiInitial(options) {
    return options.delay ?? 0;
  }

  function prefereMoinsDAnimations() {
    return (
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches
    );
  }

  /** Annule l'animation en cours sur ce nœud, s'il y en a une. */
  function cancel(node) {
    const precedente = enCours.get(node);
    if (precedente) precedente.cancel();
  }

  /** Écrit le texte immédiatement, sans animation (et annule toute animation). */
  function instant(node, texte) {
    cancel(node);
    node.classList.remove("tw-caret");
    node.textContent = texte;
  }

  return { type, typeRich, cancel, instant, VITESSE_DEFAUT };
})();

// Les tests hors navigateur (harnais rustc + mock Tauri) chargent ce fichier
// comme un module CommonJS.
if (typeof module !== "undefined" && module.exports) module.exports = GwsTypewriter;

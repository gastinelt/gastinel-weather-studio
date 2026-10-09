/*
 * Conversion Markdown → DOM, réduite à ce que l'agent produit réellement.
 *
 * ## Pourquoi pas de bibliothèque
 *
 * Le besoin tient en quatre constructions : des paragraphes, des listes à puces
 * ou numérotées, du gras, un peu d'italique et du code entre accents graves.
 * Un moteur Markdown complet apporterait tableaux, liens, images, HTML brut —
 * c'est-à-dire surtout une surface d'attaque et 40 ko de dépendance pour une
 * application qui n'a *aucun* outillage JS (ni Node, ni bundler : voir le
 * README). On écrit donc les 150 lignes nécessaires, et rien de plus.
 *
 * ## Pourquoi du DOM et pas du HTML
 *
 * `main.js` pose la règle « aucun `innerHTML` avec des données ». Elle vaut
 * doublement ici : le texte vient d'un modèle de langage, donc d'une source sur
 * laquelle personne ne peut donner de garantie. En construisant des nœuds avec
 * `createElement` / `createTextNode`, il n'existe littéralement pas de chemin
 * par lequel une balise écrite par le modèle deviendrait du balisage — un
 * `<script>` dans la réponse ressort comme le texte « <script> ».
 *
 * ## Limites assumées
 *
 * Pas de listes imbriquées, pas de liens, pas de tableaux, pas de blocs de code
 * multilignes. Si l'agent se met à en produire, c'est le *system prompt* qu'il
 * faut resserrer (voir `CONSIGNES_FORMAT` dans `groq.rs`), pas ce fichier qu'il
 * faut gonfler. Un délimiteur non apparié reste affiché tel quel : mieux vaut un
 * astérisque isolé visible qu'un morceau de phrase avalé par une règle trop
 * gourmande.
 */

const GwsMarkdown = (() => {
  /**
   * Découpe une ligne en fragments stylés.
   *
   * Balayage séquentiel plutôt qu'une pile de `String.replace` : une suite de
   * remplacements travaille sur le résultat des précédents, si bien qu'un
   * astérisque *à l'intérieur* d'un bloc de code finit par ouvrir de l'italique.
   * Ici, dès qu'un délimiteur est reconnu son contenu est mis de côté et n'est
   * plus jamais réexaminé.
   *
   * Les fragments stylés portent des `children` et non un texte plat : le
   * contenu d'un `**gras**` est retokenisé, si bien qu'un `*italique*` à
   * l'intérieur est reconnu au lieu de laisser ses astérisques à l'écran. La
   * récursion termine toujours — le contenu examiné est strictement plus court
   * que la ligne dont il provient, délimiteurs retirés.
   *
   * @param {string} ligne
   * @returns {Array<{kind: "text"|"code", text: string} |
   *                 {kind: "strong"|"em", children: Array}>}
   */
  function tokens(ligne) {
    const sortie = [];
    let tampon = "";
    const viderTampon = () => {
      if (tampon) sortie.push({ kind: "text", text: tampon });
      tampon = "";
    };

    let i = 0;
    while (i < ligne.length) {
      const reste = ligne.slice(i);
      let m;

      // Le code d'abord : ce qu'il contient ne doit rien déclencher d'autre.
      if ((m = /^`([^`\n]+)`/.exec(reste))) {
        viderTampon();
        sortie.push({ kind: "code", text: m[1] });
        i += m[0].length;
        continue;
      }

      // Gras + italique combinés. Traité avant `**`, qui sinon emporterait deux
      // des trois astérisques et laisserait le troisième à l'écran.
      if ((m = /^(\*\*\*|___)(\S(?:[\s\S]*?\S)??)\1/.exec(reste))) {
        viderTampon();
        sortie.push({
          kind: "strong",
          children: [{ kind: "em", children: tokens(m[2]) }],
        });
        i += m[0].length;
        continue;
      }

      // `**` avant `*`, sinon « **gras** » se lit comme un italique vide suivi
      // du mot puis d'un second italique vide.
      //
      // Le `??` final n'est pas décoratif : un groupe optionnel est gourmand par
      // défaut, et « **A** et **B** » se lisait alors comme un seul gras
      // « A** et **B » — le premier délimiteur fermant était sauté au profit du
      // dernier. Rendre le groupe paresseux fait gagner la fermeture la plus
      // proche, qui est la bonne.
      if ((m = /^(\*\*|__)(\S(?:[\s\S]*?\S)??)\1/.exec(reste))) {
        viderTampon();
        sortie.push({ kind: "strong", children: tokens(m[2]) });
        i += m[0].length;
        continue;
      }

      // Le contenu d'un italique ne peut ni commencer ni finir par un
      // délimiteur. Sans cette borne, « *ital avec **gras** dedans* » se lisait
      // comme un italique « ital avec * », le second astérisque du `**` servant
      // de fermeture : la phrase se retrouvait coupée au mauvais endroit. Faute
      // de pouvoir apparier proprement, on préfère laisser l'astérisque visible.
      if ((m = /^([*_])([^\s*_](?:[^*_\n]*?[^\s*_])?)\1/.exec(reste)) && ouvreUnMot(ligne, i)) {
        viderTampon();
        sortie.push({ kind: "em", children: tokens(m[2]) });
        i += m[0].length;
        continue;
      }

      tampon += ligne[i];
      i += 1;
    }

    viderTampon();
    return sortie;
  }

  /**
   * Un délimiteur d'italique ne compte qu'en début de mot.
   *
   * Sans cette garde, `raw_timestamp_was_utc` — un nom de champ qui revient
   * souvent dans les échanges sur ce projet — perdrait ses tirets bas au profit
   * d'un italique involontaire.
   */
  function ouvreUnMot(ligne, i) {
    if (i === 0) return true;
    return !/[\wÀ-ɏ]/.test(ligne[i - 1]);
  }

  /** Ligne de séparation (`---`, `***`) : sans objet dans un bloc étroit. */
  const SEPARATEUR = /^([-*_])\1{2,}$/;
  const TITRE = /^#{1,6}\s+(.+?)\s*#*$/;
  const PUCE = /^[-*+•]\s+(.+)$/;
  const NUMERO = /^\d+[.)]\s+(.+)$/;

  /**
   * Analyse le texte en blocs. Fonction **pure** : aucun DOM touché, ce qui la
   * rend testable telle quelle et laisse [`toDom`] seul responsable du rendu.
   *
   * @param {string} texte
   * @returns {Array<{kind: "p"|"h", lines?: Array, tokens?: Array} |
   *                 {kind: "ul"|"ol", items: Array}>}
   */
  function toBlocks(texte) {
    const lignes = String(texte == null ? "" : texte)
      .replace(/\r\n?/g, "\n")
      .split("\n");

    const blocs = [];
    let paragraphe = null;
    let liste = null;

    for (const brute of lignes) {
      const ligne = brute.trim();

      // Une ligne vide ferme ce qui est ouvert : c'est la seule ponctuation de
      // bloc que le format connaisse.
      if (!ligne || SEPARATEUR.test(ligne)) {
        paragraphe = null;
        liste = null;
        continue;
      }

      let m;
      if ((m = TITRE.exec(ligne))) {
        paragraphe = null;
        liste = null;
        blocs.push({ kind: "h", tokens: tokens(m[1]) });
        continue;
      }

      const puce = PUCE.exec(ligne);
      const numero = puce ? null : NUMERO.exec(ligne);
      if (puce || numero) {
        const type = puce ? "ul" : "ol";
        paragraphe = null;
        // Une liste à puces qui suit immédiatement une liste numérotée est bien
        // une nouvelle liste : on ne fusionne que des items de même nature.
        if (!liste || liste.kind !== type) {
          liste = { kind: type, items: [] };
          blocs.push(liste);
        }
        liste.items.push(tokens((puce || numero)[1]));
        continue;
      }

      liste = null;
      if (!paragraphe) {
        paragraphe = { kind: "p", lines: [] };
        blocs.push(paragraphe);
      }
      // Les retours à la ligne internes sont conservés (`<br>`) : l'agent écrit
      // souvent « Constat : … \n Conséquence : … » sans ligne vide, et coller
      // les deux phrases en un seul pavé perdrait sa mise en forme.
      paragraphe.lines.push(tokens(ligne));
    }

    return blocs;
  }

  /** Un fragment de ligne → nœud DOM. */
  function noeudToken(doc, token) {
    if (token.kind === "text") return doc.createTextNode(token.text);

    // Le code est terminal : ce qu'il contient est du texte, un point c'est tout.
    if (token.kind === "code") {
      const balise = doc.createElement("code");
      balise.appendChild(doc.createTextNode(token.text));
      return balise;
    }

    const enveloppe = doc.createElement(token.kind === "strong" ? "strong" : "em");
    remplirLigne(doc, enveloppe, token.children);
    return enveloppe;
  }

  function remplirLigne(doc, cible, jetons) {
    for (const t of jetons) cible.appendChild(noeudToken(doc, t));
  }

  /**
   * Construit le fragment DOM correspondant au texte Markdown.
   *
   * @param {string} texte
   * @param {Document} [doc]
   * @returns {DocumentFragment}
   */
  function toDom(texte, doc) {
    const d = doc || document;
    const fragment = d.createDocumentFragment();

    for (const bloc of toBlocks(texte)) {
      if (bloc.kind === "ul" || bloc.kind === "ol") {
        const liste = d.createElement(bloc.kind);
        for (const item of bloc.items) {
          const li = d.createElement("li");
          remplirLigne(d, li, item);
          liste.appendChild(li);
        }
        fragment.appendChild(liste);
        continue;
      }

      if (bloc.kind === "h") {
        // Rendu en paragraphe accentué, pas en `<h3>` : le bloc « Agent IA » est
        // étroit et n'a pas de hiérarchie de titres à respecter.
        const p = d.createElement("p");
        p.className = "md-title";
        remplirLigne(d, p, bloc.tokens);
        fragment.appendChild(p);
        continue;
      }

      const p = d.createElement("p");
      bloc.lines.forEach((jetons, index) => {
        if (index > 0) p.appendChild(d.createElement("br"));
        remplirLigne(d, p, jetons);
      });
      fragment.appendChild(p);
    }

    return fragment;
  }

  /** Texte sans balisage — utile pour un `title`, un toast ou un test. */
  function toPlainText(texte) {
    return toBlocks(texte)
      .map((bloc) => {
        if (bloc.kind === "ul" || bloc.kind === "ol")
          return bloc.items.map((i) => "• " + brut(i)).join("\n");
        if (bloc.kind === "h") return brut(bloc.tokens);
        return bloc.lines.map(brut).join("\n");
      })
      .join("\n\n");
  }

  const brut = (jetons) =>
    jetons.map((t) => (t.children ? brut(t.children) : t.text)).join("");

  return { toBlocks, toDom, toPlainText, tokens };
})();

// Chargé comme module CommonJS par le harnais de test hors navigateur.
if (typeof module !== "undefined" && module.exports) module.exports = GwsMarkdown;

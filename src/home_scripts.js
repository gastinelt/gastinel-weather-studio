/*
 * Scripts de l'écran d'accueil.
 *
 * ⚠️ JEU D'EXEMPLE. Ces dix scripts existent pour que le mécanisme tourne et
 * pour fixer le ton ; ils sont destinés à être **remplacés** par la liste
 * définitive (environ 50 scripts). Voir le README, § « Écran d'accueil ».
 *
 * Chaque script est un objet :
 *
 *   {
 *     text:          string,   // avec placeholders {name} {project_name}
 *                              //                   {point_count} {last_opened}
 *     defaultAction: string?   // "import" | "create" | "explore"
 *                              // absent → "explore" (voir DEFAULT_ACTION)
 *   }
 *
 * Le champ `defaultAction` désigne le bouton qui se remplit tout seul quand les
 * boutons apparaissent ; au bout de sept secondes son action se déclenche, sauf
 * si l'utilisateur a entre-temps cliqué sur l'un des boutons. Tous les scripts d'exemple s'en remettent au
 * défaut ; le champ est là pour qu'un script de la liste définitive puisse
 * mener ailleurs — un script qui parle d'un projet vide a plus de sens s'il
 * pousse vers « Ajouter un point ».
 *
 * Ton : le même que celui de l'agent (§ « Agent IA » du README). Factuel,
 * direct, pas de politesse creuse, vocabulaire du domaine sans guillemets.
 */

const GwsHomeScripts = (() => {
  /** Bouton qui se remplit quand un script ne demande rien de particulier. */
  const DEFAULT_ACTION = "explore";

  /** Durée du remplissage, en millisecondes. Spécifiée au prompt 5. */
  const AUTO_DELAY_MS = 7000;

  const SCRIPTS = [
    { text: "{name}. {project_name} — {point_count}. Dernière session {last_opened}." },
    { text: "{project_name} rouvert. {point_count} au relevé. Session précédente {last_opened}." },
    { text: "{name}, {project_name} compte {point_count}. Rien n'a bougé depuis {last_opened}." },
    { text: "Réseau {project_name} : {point_count}. Dernier accès {last_opened}." },
    { text: "{name}. Retour sur {project_name}, {last_opened}. État : {point_count}." },
    { text: "{project_name}. {point_count} en base, aucune analyse en attente." },
    { text: "{name}, les {point_count} de {project_name} attendent leurs séries." },
    { text: "Chargement de {project_name} terminé. {point_count}, dernière ouverture {last_opened}." },
    { text: "{project_name} — {point_count}. Les statistiques se recalculent à chaque import." },
    { text: "{name}. {last_opened}, vous quittiez {project_name} sur {point_count}." },
  ];

  /**
   * Remplace les placeholders d'un script.
   *
   * Un placeholder inconnu est laissé tel quel plutôt que vidé : voir
   * « {machin} » à l'écran désigne la faute de frappe, alors qu'un trou dans une
   * phrase se lit comme un bug d'affichage.
   */
  function fill(texte, valeurs) {
    return texte.replace(/\{(\w+)\}/g, (brut, cle) =>
      Object.prototype.hasOwnProperty.call(valeurs, cle) ? String(valeurs[cle]) : brut
    );
  }

  /**
   * Tire un script au hasard.
   *
   * `exclude` évite de retomber sur le même script deux ouvertures de suite —
   * sur dix scripts, la répétition immédiate est fréquente et se remarque.
   */
  function pick(exclude) {
    const candidats = SCRIPTS.length > 1 ? SCRIPTS.filter((s) => s.text !== exclude) : SCRIPTS;
    const choisi = candidats[Math.floor(Math.random() * candidats.length)];
    return { ...choisi, defaultAction: choisi.defaultAction || DEFAULT_ACTION };
  }

  return { SCRIPTS, DEFAULT_ACTION, AUTO_DELAY_MS, fill, pick };
})();

if (typeof module !== "undefined" && module.exports) module.exports = GwsHomeScripts;

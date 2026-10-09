# Gastinel Weather Studio 0.5.0 — feuille de route

Branche `release/0.5.0-final` (dossier `_CODE\GWS-050-final`), partie de
`feat/config-relais-signee`, qui contient déjà toute la 0.5.0-beta.5.

## Déjà dans la branche

| Sujet | État |
|---|---|
| Mémoire, performance, 9 points UX, projet d'exemple | beta.1 à beta.4 (publiées en stable sous 0.4.8) |
| Compte à rebours « Explorer le studio » | rétabli comme en 0.4.7 |
| Clic droit sur un point : Détails / Récupérer les données | beta.5 |
| Signature Ed25519 des paquets `.gws` | beta.5, testée dans l'appli réelle |
| Retour arrière automatique après une mise à jour ratée | beta.5 |
| Configuration signée du relais (relevé au démarrage, plancher 10 min) | `feat/config-relais-signee` |
| Résumé IA des gros projets : agrégats seulement au-delà de 15 points | 02/10, `083f75c` |
| Guide de départ ouvert automatiquement pour tous (stable et bêta) | 02/10, `beeb361` |
| Retour / Avancer sur PC : Alt+← / Alt+→, boutons latéraux de la souris ; Échap remonte d'un cran sur la page Elitech | 02/10, `a7d4955` |
| L'appli accepte le message `{gws: "escape"}` d'une extension pour fermer sa fenêtre | 02/10, `a7d4955` |

Non testé dans l'application compilée au 02/10 : résumé agrégé, guide pour
tous, Retour/Avancer.

## À faire pour la 0.5.0

### 1. Bandeau de quota Infoclimat (décidé par Théo le 02/10 : dans la 0.5.0)
- Emplacement : Paramètres, en bas, à gauche de « Fermer » ; bandeau d'environ
  14 px de haut sur la majeure partie de la largeur, sans courbe.
- Segments : noir = requêtes automatiques prédéfinies du serveur ; gris =
  requêtes manuelles de l'administrateur ; une couleur par appareil (8
  prédéfinies), nom de l'appareil au survol.
- Échelle : 4999 requêtes par 24 h glissantes en interne ; compteur affiché
  remis à zéro à minuit.
- Prérequis : le VPS compte les appels par appareil (identifiant généré au
  premier lancement), pas l'appareil lui-même.
- Les autres affichages du quota restent comme avant.

### 2. Annuler / Rétablir (Ctrl+Z / Ctrl+Y)
Règle décidée par Théo le 02/10 : **uniquement pour les actions destructrices**.

| Action | Annulable |
|---|---|
| Supprimer un projet | oui |
| Supprimer une station (point) | oui |
| Supprimer une série ou des données d'une station | oui |
| Ajouter une station | non |
| Ajouter ou importer des données | non |

Pistes : suppression différée (corbeille) plutôt que reconstruction après coup ;
message « Annulé : suppression de *nom* » ; historique limité à la session.

### 3. Refonte de l'import et clarification « anciennes / dernières données »
Panneau « Imports » dans le projet :
- liste de chaque import (fichier ou API), son point et sa date ;
- source : Infoclimat, ROMMA, Elitech, CSV, saisie manuelle ;
- état : dernières données récupérées, anciennes données jusqu'à telle date,
  en cours, en erreur ;
- actions : relancer, voir le détail, supprimer l'import.

### 4. Données de chaque station (demande de Théo du 02/10)
Une vue claire des données d'une station, pour :
- voir ce qu'elle contient : séries, source de chaque série, période, nombre
  de mesures, trous ;
- **modifier** : corriger une valeur, un décalage d'horloge, rattacher une
  série à une autre station ;
- **supprimer** : une série entière, une plage de dates, des mesures
  aberrantes.

Les suppressions passent par Annuler / Rétablir (point 2).

### 5. Extensions
- Ajouter dans carte3d et thermique la ligne qui relaie Échap :
  `addEventListener("keydown", e => e.key === "Escape" && parent.postMessage({ gws: "escape" }, "*"))`.
  Il faut une nouvelle version signée de chaque extension.
- Vérifier que carte3d 1.0.2 est bien publiée, sinon la republier signée.

### 6. Latence au clic sur un point (signalé par Théo le 02/10)
Avec beaucoup de points sélectionnés (projet France, 1153 points), un clic sur
un point met du temps à répondre.

**Cause (03/10) :** chaque changement de sélection relance `cross_analysis` sur
toute la sélection. Côté Rust, chaque point relisait toutes ses mesures. Côté
interface, le tableau appelait `toLocaleString("fr-FR")` une dizaine de fois
par ligne, et chaque appel construit un nouveau formateur.

**Fait sur `feat/donnees-a-la-demande` (0.5.1)**, mesuré sur la copie de la base
France (288 M mesures), avec 1153 points sélectionnés et la fenêtre de 30 jours
(celle par défaut) :

| Étape | 0.5.0 | Maintenant |
|---|---:|---:|
| Calcul Rust, premier clic | 55,8 s | 0,68 s (agrégats journaliers) |
| Calcul Rust, clic suivant (+1 ou −1 point) | 55,8 s | **0,037 s** (cache par point) |
| Rendu du tableau et du graphique | 0,75-0,87 s | **0,26-0,29 s** (formateurs créés une fois) |

Sur tout l'historique : 234 s → 2,2 s au premier clic, puis 0,037 s ; rendu
1,0-1,2 s → 0,29-0,32 s.

Reste à décider (Théo) : au-delà d'une cinquantaine de points, superposer toutes
les courbes n'est plus lisible (8 couleurs seulement). On pourrait limiter le
graphique aux N premiers points et garder le tableau complet. Ce n'est pas fait :
c'est un choix d'interface.

À noter : chaque changement de sélection déclenche aussi l'analyse IA
automatique (`planifierAnalyseAuto`, une requête Groq après 900 ms), même avec
1153 points.

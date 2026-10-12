# Feuille de route — Gastinel Weather Studio

Objectifs par génération. Les détails peuvent bouger ; le cap reste celui-ci.
Tout au long du chemin : correction des fonctions et des manques d'ergonomie
remontés à l'usage.

## v0.7.0 — Fonctions, mémoire et application Android

**Ordinateur**
- Import Elitech par période : choisir une plage dans la lecture du logger et
  l'envoyer dans un projet, puis une autre plage dans un autre projet.
- Optimisation de la mémoire.
- Quelques fonctions supplémentaires.

**Android** (reprise du projet déjà commencé)
- Choisir les fonctions gardées sur téléphone, par exemple consulter un projet
  sans pouvoir le modifier.
- Relevé sur le terrain : la lecture d'un Elitech est mise en attente sur le
  serveur, puis validée ou écartée dans un délai donné avant d'entrer dans le
  projet.

## Avant la v0.8.0 — Écran de mise à jour

Écran de mise à jour repensé, avec une animation propre à chaque nouvelle
génération (0.8, 0.9…), pas à chaque correctif.

## v0.8.0 — Écosystème

- Téléphone et ordinateurs réellement reliés, sur plusieurs appareils.
- Publication d'un projet Cloud en lecture seule pour tous, avec autorisation,
  via une extension ou un menu dédié.
- Accès aux projets Cloud au-delà du seul code d'invitation (quota, par exemple).

## v0.9.0 — Plateformes

- iPhone et Mac.
- Linux (Arch et autres distributions), Linux ARM64.
- Téléphones pliables (deux et trois volets).

## v1.0 — Tout importer

Compatibilité des imports la plus large possible : RC-5 Pro, Voltcraft
DL-101T et DL-121TH, et les autres formats rencontrés.

## v1.1 — Logiciel compagnon

Application Rust séparée, très légère (moins de 10 Mo, interface du type
Slint), compatible avec GWS : lire et régler les loggers de façon fiable sur
le terrain, sans le superflu, pour les passionnés de météo peu à l'aise avec
le numérique.

## v1.5 et v2.0

Extensions, et compatibilité avec des outils développés par d'autres avec une
IA. Détails dans [propositions-0.6.md](propositions-0.6.md).

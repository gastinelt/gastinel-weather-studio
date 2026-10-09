# Stations météo françaises Infoclimat : accès à l'API Open Data

## Source des identifiants
Un seul appel à `https://www.infoclimat.fr/opendata/stations_xhr.php`, le JSON que charge la carte de la page `/opendata/`. Il renvoie 1230 stations, dont **1153 avec `pays = "FR"`** (30 en outre-mer, départements 97x). Les candidats bruts sont dans `candidats_fr.json`.

Formats d'ID rencontrés : `STATIC0479` (382), `ME001` (98, stations de lycées « MAE »), `00002` (5 chiffres), `000X1` (alphanumérique), `VENTOUX` (texte libre, genre `synop`).
Codes de licence de cette liste : 0 = Etalab, 1 = CC BY, 2 = CC BY-NC.

Limite : cette liste ne contient que les stations publiées en Open Data. Les stations OMM/synop de Météo-France (`07xxx`) n'y figurent pas, et aucune page du site ne les liste en un seul endroit (`/climatologie/stations_principales.php` redirige vers `/opendata/`).

## Méthode (script `scripts/stations_fr/recenser_stations.py`)
- Un appel API par station, sur une fenêtre d'un jour (J-2), passé par le relais SSH. Si la réponse contient un bloc `stations[]`, la station est marquée `api_disponible: true` et ses métadonnées viennent de l'API. Sinon, elle est marquée `false` avec la raison.
- Au moins 1,1 s entre deux requêtes, avec un User-Agent identifiable. Sur une réponse 429 ou 503, le script fait une pause de 5 min.
- Le quota est incrémenté dans `%APPDATA%/fr.gastinel.weatherstudio/Clee.API/infoclimat_quota.json`, avec le même schéma que l'application.
- Le script reprend là où il s'est arrêté grâce au checkpoint `checkpoint.jsonl`. `--export` regénère le JSON et le CSV à partir de ce checkpoint.

## Résultats (24/09/2026)
- **1153 stations françaises**, **1153 accessibles** par l'API (bloc `stations[]` présent et `status` OK), **0 inaccessible**.
- 835 stations avaient des données le 22/09/2026 (`donnees_recentes: true`). Les 318 autres répondent mais n'ont rien envoyé ce jour-là.
- Réseaux : 1054 StatIC, 98 StatIC « ME » (lycées), 1 synop (VENTOUX).
- Licences renvoyées par l'API : 596 CC BY, 556 CC BY-NC (usage commercial interdit), 1 Etalab.
- Aucune erreur HTTP ni aucun 429 pendant le lot.

## Relancer
Les appels API passent par le relais SSH de l'application (IP fixe, empreinte ED25519 vérifiée). La clé et le quota utilisés sont ceux de l'app installée, dans `%APPDATA%/fr.gastinel.weatherstudio/Clee.API/`.

    python scripts/stations_fr/recenser_stations.py           # reprise automatique depuis checkpoint.jsonl
    python scripts/stations_fr/recenser_stations.py --export  # regénère JSON/CSV

Pour refaire un recensement complet, supprimez d'abord `checkpoint.jsonl` et `candidats_fr.json`.

Limite : `api_disponible` vaut forcément true pour toutes les stations, puisque la source ne liste que les stations publiées en Open Data. Les autres stations Infoclimat (OMM `07xxx`, stations amateurs non ouvertes) ne sont pas recensées ici.

# Sources météo complémentaires à Infoclimat

Recherche du 24/09/2026. Pages consultées directement quand c'était possible. Les portails construits en JavaScript (portail-api.meteofrance.fr) ont été vérifiés via la documentation Confluence de Météo-France et data.gouv.fr. Les points marqués *(à confirmer)* n'ont pas pu être relus sur la page source.

## Classement global (périmètre élargi)

Critères utilisés :
- **Complémentarité** : stations absentes d'Infoclimat (StatIC + SYNOP).
- **Montagne / vallées** : densité de stations utiles pour étudier les inversions (Alpes, Vercors).
- **Capteurs** : pro = abri normalisé, réseau d'État ; semi-pro = Davis ou équivalent ; amateur = non normalisé.
- **Automatisable** : accès possible par une API ou des fichiers téléchargeables.
- **Classes** : A = à intégrer ; B = intéressant mais accès à demander ; C = redondant ou inaccessible.

Les chiffres marqués « ≈ » sont des ordres de grandeur. Le recouvrement avec Infoclimat est à mesurer par croisement de coordonnées.

### A — à intégrer

| Plateforme | Accès | Stations FR hors Infoclimat | Intérêt montagne / vallées | Capteurs |
|---|---|---|---|---|
| Météo-France, fichiers data.gouv (quotidien, horaire, 6 min) et liste des stations | csv.gz ouverts, Etalab 2.0 | **Élevé** : ≈ 13 000 postes ouverts, dont de nombreux postes climatologiques (pluie, Tn/Tx) non republiés par Infoclimat | Bon : postes d'altitude et de fond de vallée | Pro |
| Météo-France DPObs (temps réel) et DPClim (à la demande) | API OAuth2 gratuite, 50 req/min | Même parc (≈ 2 000 stations automatiques) | Bon | Pro |
| Réseau nivo-météorologique Météo-France (Nivôse + postes des stations de ski) | data.gouv : « Données d'observations du réseau nivo-météorologique » et « Archive réseau nivo-météorologique » | ≈ 29 Nivôse + ≈ 150 postes de stations de ski, peu présents chez Infoclimat | **Très fort** (1 500 à 3 000 m, hiver) | Pro / conventionné |
| MeteoSwiss (frontalier : Genève, Valais, Jura) | STAC REST `data.geo.admin.ch/api/stac/v1/collections/ch.meteoschweiz.ogd-smn`, libre avec mention « Source: MeteoSwiss » | Aucune en France, ≈ 160 + 140 stations en Suisse | Fort, pour la référence alpine | Pro |
| Arpa Piemonte (frontalier) | API ouverte (`utility.arpa.piemonte.it/docs/` : historique et temps réel) | Aucune en France, plusieurs centaines en Piémont | Fort, versant italien | Pro |
| NOAA ISD / OGIMET | Fichiers ISD (NODD/AWS) ; OGIMET via formulaires | Aucune : ce sont les SYNOP, déjà chez Infoclimat | Faible | Pro |

NOAA ISD et OGIMET ne sont en A que comme secours pour les SYNOP. Ils n'apportent aucune station nouvelle.

### B — intéressant, accès à demander

| Plateforme | Pourquoi | Démarche |
|---|---|---|
| **ROMMA** | ≈ 250 stations Davis dans les Alpes, Vercors et Chartreuse compris, avec beaucoup de fonds de vallée : exactement la cible microclimats. Une partie est peut-être déjà relayée par Infoclimat (à vérifier). | Écrire à contact@romma.fr pour obtenir un export ou un flux autorisé |
| Centro Funzionale Valle d'Aosta | ≈ 100 stations de montagne (frontalier) ; anagrafica en PDF ; téléchargement web | Vérifier la licence et demander si un flux automatisable existe |
| INRAE AgroClim / CLIMATIK | 49 sites agroclimatiques en métropole, avec capteurs pro. Historique ouvert jusqu'au 31/12/2024 ; temps réel réservé | Compte CLIMATIK « recherche publique » ou demande à AgroClim |
| Météo Bretagne (MBdata / MétéoData) | ≈ 80 stations Davis en Bretagne, contrôlées ; hors zone alpine | Contacter l'association |
| WeatherLink (Davis) | Beaucoup de Davis amateurs en montagne, mais l'accès aux stations publiques d'autrui exige un abonnement Pro+ payé par station | Seulement si partenariat avec des propriétaires |
| Réseaux associatifs régionaux (Météo Centre, Météo Pyrénées, Réseau Météo du Jura, Météo Normandie, Météo-Alpes…) | Densité locale ; souvent déjà rattachés à StatIC | Au cas par cas, après croisement avec Infoclimat |
| Observatoires (Mont Aigoual, Pic du Midi) | Séries longues de haute qualité ; Aigoual est déjà un poste Météo-France (donc en A via data.gouv) ; le Pic du Midi a des données de recherche (OMP / P2OA) | Demande aux laboratoires |
| Stations agricoles privées (Weenat, Sencrop) | Très dense en plaine agricole | API réservée aux clients sur leurs propres capteurs : seulement via partenariat |

### C — redondant ou inaccessible

- Meteociel, Météo-Villes : pas d'API, CGU restrictives, données de seconde main.
- KeraunOS : portail temps réel qui réagrège, archive de 40 jours, pas d'API publique.
- Weather Underground PWS, Netatmo : API limitées aux contributeurs, capteurs amateurs non normalisés, forts biais urbains ou de toiture.
- Weathercloud (pas d'API de lecture officielle), Ecowitt (API limitée à ses propres appareils), CWOP / findu (amateurs, redondants avec WU).
- Sensor.Community : archives CSV quotidiennes ouvertes, mais les capteurs (BME280 non abrités) sont inadaptés à la climatologie.
- Stations routières (DIR, Vialis) : pas d'open data généralisé ; accès par convention uniquement.
- MeteoNetwork : essentiellement Italie. Open-Meteo : modèles, pas des mesures.

## Tableau comparatif (sources initiales)

| Source | API | Auth | Quotas | Licence / CGU | Stations FR | Historique | Temps réel | Recommandation |
|---|---|---|---|---|---|---|---|---|
| **Météo-France DPObs** (observations) | Oui, officielle (REST JSON/CSV) | Compte portail + jeton OAuth2 (gratuit) | ~50 req/min *(à confirmer pour DPObs)* | Licence Ouverte Etalab 2.0 | ~2 000 stations automatiques (6 min / horaire) | 24 h seulement | Oui (6 min, horaire, SYNOP 3 h) | **Utiliser** |
| **Météo-France DPClim** (climatologie) | Oui, officielle (commande asynchrone puis CSV) | Même compte / jeton | 50 req/min (sinon bannissement) | Etalab 2.0 | Toutes les stations historisées (~14 700 au total, dont ~13 400 ouvertes) | Depuis l'ouverture de chaque station | Non | **Utiliser** (historique ciblé) |
| **meteo.data.gouv.fr / data.gouv.fr** (fichiers en masse) | Fichiers statiques csv.gz par département et par période | Aucune | Aucun | Etalab 2.0 | Toutes | Complet (quotidien, horaire, 6 min) | Quotidien pour les 2 dernières années | **Utiliser en premier** (le plus simple) |
| **ROMMA** | Non (ni publique ni documentée) | Export CSV réservé aux membres (identifiant + mot de passe) | — | « Ne pas reproduire notre site et/ou nos données sans nous en informer » ; chaque partenaire reste propriétaire de ses données | ~250 (Alpes FR + CH + IT) | Oui, par export membre | Pages HTML uniquement | **Demander un accès** (contact@romma.fr) ; ne pas scraper |
| **Meteociel** | Non | — | — | Mentions légales : usage commercial des données « strictement interdit », copyright obligatoire | Réagrège SYNOP / Météo-France / StatIC Infoclimat / amateurs | Oui (sur le site) | Oui (sur le site) | **Éviter** (données secondaires, déjà couvertes à la source) |
| **Météo-Villes** | Non | — | — | SARL privée, © Météo-villes ; aucune licence de réutilisation | Cartes des Netatmo privées + données officielles | Chroniques | Cartes | **Éviter** |
| Weather Underground PWS | Oui (api.weather.com) | Clé réservée aux propriétaires d'une station qui envoie ses données | ~1 500 req/jour, 30/min | CGU IBM restrictives | Plusieurs milliers (qualité variable) | Partiel | Oui | Optionnel, faible priorité |
| Netatmo Weathermap | Oui (`getpublicdata`) | Application Netatmo + OAuth2 | Limité par utilisateur | CGU Netatmo (usage personnel) | Dizaines de milliers (non conformes OMM) | Non via getpublicdata | Oui | Éviter pour de la climatologie |
| MeteoNetwork (IT) | Oui (api.meteonetwork.it, jeton) | Inscription | — | CC BY-SA *(à confirmer)* | Surtout Italie, peu en France | Oui | Oui | Hors périmètre sauf zones frontalières |
| Open-Meteo | Oui, gratuite | Aucune (non commercial) | 10 000 req/jour | CC BY 4.0 | — | Réanalyses ERA5 | Modèles | **Ce ne sont PAS des mesures** (modèle / réanalyse) : uniquement pour combler des trous ou comparer |

## Détail par source

### 1. ROMMA (romma.fr)
- **API** : aucune API publique n'a été trouvée, ni de projet tiers (GitHub, Home Assistant) qui l'interroge.
- **Pages publiques** (HTML) : `station_24.php?id=<ID>` (mesures en direct), `station_jour.php?id=<ID>`, `station_clim_mois.php?id=<ID>&month=M&year=Y`, `station_clim_an.php?id=<ID>&year=Y`, `station_etude.php?id=<ID>`, la carte `carte00_tab.php` et le tableau `tabobs_tab.php`. Chaque station a une fiche PDF : `/stations/<code>/fiche_descriptive.pdf`. L'identifiant numérique permet de construire la liste des stations. Les coordonnées ne figurent que dans les fiches et sur la carte ; il n'y a pas de fichier de coordonnées.
- **Origine des `archives_<Nom>_<jj-mm-aaaa>_<jj-mm-aaaa>.csv`** : page `https://www.romma.fr/telechargement_station.php`, qui passe par un formulaire de connexion (« Login » et « Mot de passe »). Elle fait partie de l'espace adhérent / partenaire, à côté de `tableau_bord.php` et `adherent.php`. Les exports ne sont donc pas accessibles par une URL publique.
- **Conditions** : ROMMA est une association sans but lucratif. Selon la page Association, chaque partenaire reste propriétaire des données de sa station, et le pied de page demande de ne pas reproduire le site ni les données sans prévenir l'association.
- **Recommandation** : écrire à contact@romma.fr ou passer par `contact.php` pour demander un export groupé ou un flux autorisé. En attendant, garder l'import manuel des CSV. Ne pas scraper les pages PHP.

### 2. Météo-France
- **DPObs – API ciblée Données d'observation** : `https://public-api.meteofrance.fr/public/DPObs/v2/`.
  - Endpoints : `/liste-stations`, `/station-infra-horaire-6m`, `/station-horaire`, `/liste-stations-synop`, `/synop`, et aussi bouées et navires.
  - Authentification : en-tête `Authorization: Bearer <token>`. Le jeton OAuth2 s'obtient avec les identifiants client générés sur le portail. Il est court (environ 1 h *(à confirmer)*) et doit être renouvelé.
  - Contenu : 24 h de profondeur seulement, une station par requête.
  - Fréquence de mise à jour : horaire (vers H+10 min), toutes les 6 min pour les données infra-horaires, toutes les 3 h pour SYNOP.
  - Doc : https://confluence-meteofrance.atlassian.net/wiki/spaces/OpenDataMeteoFrance/pages/853639294
- **DPClim – API Données climatologiques** :
  - Endpoints : `liste-stations`, puis `commande-station/{infrahoraire-6m|horaire|quotidienne}` qui renvoie un identifiant de commande, puis `commande/fichier?id-cmde=` qui renvoie un CSV.
  - Quota : 50 requêtes par minute, sous peine de bannissement.
  - Doc : https://confluence-meteofrance.atlassian.net/wiki/spaces/OpenDataMeteoFrance/pages/854261785 ; fiche data.gouv : https://www.data.gouv.fr/dataservices/api-donnees-climatologiques
- **Fichiers en masse** (meteo.data.gouv.fr / data.gouv.fr) :
  - Données « de base » quotidiennes, horaires et 6 min, par département et par période, en csv.gz. Exemple pour le quotidien : 624 fichiers.
  - Mise à jour : annuelle avant 1950, mensuelle jusqu'à l'année N-2, quotidienne pour les deux dernières années. Heures en UTC.
  - Page : https://www.data.gouv.fr/datasets/donnees-climatologiques-de-base-quotidiennes
- **Liste des stations** : jeu « Informations sur les stations Météo-France (métadonnées) ». CSV de 14 729 stations, dont 13 362 ouvertes, avec identifiant à 8 chiffres, latitude/longitude, altitude et type de station. https://www.data.gouv.fr/datasets/informations-sur-les-stations-meteo-france-metadonnees
- **Licence** : Licence Ouverte 2.0 (Etalab). La réutilisation est libre, y compris commerciale, à condition de citer la source.
- **Recommandation** : commencer par les fichiers data.gouv pour l'historique, sans clé. Ajouter ensuite DPObs pour le temps réel, et DPClim seulement pour des besoins ponctuels. Attention aux doublons avec Infoclimat, qui republie déjà les SYNOP et une partie des stations Météo-France : dédupliquer par identifiant OMM ou Météo-France et par coordonnées.

### 3. Meteociel
- **API** : aucune.
- **Mentions légales** (`/copyright/mentions.php`) : l'usage commercial des images, cartes et données est « strictement interdit ». La mention « Copyright : Meteociel.fr » et la source des données doivent rester visibles.
- **Sources** : les données sont surtout reprises de SYNOP / Météo-France et de réseaux amateurs.
- **Recommandation** : éviter. Récupérer plutôt les données chez leurs sources primaires.

### 4. Météo-Villes (meteo-villes.com)
- Site de Météo-Villes SARL (directeur de la publication : Guillaume Séchet), protégé par un copyright.
- **Page Observations** : cartes des stations Netatmo privées, avec la mise en garde du site lui-même : « certaines données peuvent être erronées ».
- Aucune API ni licence de réutilisation.
- **Recommandation** : éviter.

### 5. Autres pistes
- **Weather Underground / Netatmo** : leurs API sont réservées aux contributeurs et soumises à des CGU restrictives. La qualité des données amateurs est hétérogène. Priorité faible.
- **Réseaux régionaux** : Météo Bretagne, Observatoire du Mont Aigoual (station Météo-France, donc déjà couverte par DPClim / data.gouv), etc. Aucune API publique repérée. Procéder comme pour ROMMA : demande directe.
- **Open-Meteo** : il s'agit de modèles et de réanalyses, pas d'observations. À ne jamais mélanger avec les mesures sans le signaler.

## Ordre d'intégration conseillé
1. Fichiers Météo-France sur data.gouv, avec la liste des stations (sans clé, licence Etalab).
2. API DPObs pour le temps réel (inscription gratuite, jeton OAuth2).
3. Demande officielle à ROMMA. D'ici là, conserver l'import manuel des CSV.
4. Éventuellement WU / Netatmo. Exclure Meteociel et Météo-Villes.

## Sources
- https://www.romma.fr/association.php · https://www.romma.fr/telechargement_station.php · https://www.romma.fr/station_24.php?id=279
- https://confluence-meteofrance.atlassian.net/wiki/spaces/OpenDataMeteoFrance/pages/853639294
- https://confluence-meteofrance.atlassian.net/wiki/spaces/OpenDataMeteoFrance/pages/854261785
- https://www.data.gouv.fr/dataservices/api-donnees-climatologiques
- https://www.data.gouv.fr/datasets/donnees-climatologiques-de-base-quotidiennes
- https://www.data.gouv.fr/datasets/informations-sur-les-stations-meteo-france-metadonnees
- https://www.meteociel.fr/copyright/mentions.php
- https://www.meteo-villes.com/observations · https://www.meteo-villes.com/mentions-legales
- https://www.data.gouv.fr/datasets/donnees-dobservations-du-reseau-nivo-meteorologique · https://www.data.gouv.fr/datasets/archive-reseau-nivo-meteorologique
- https://opendatadocs.meteoswiss.ch/a-data-groundbased/a1-automatic-weather-stations
- https://www.arpa.piemonte.it/dato/open-data · https://cf.regione.vda.it/media/docs/anagrafica-stazioni-attive.pdf
- https://agroclim.inrae.fr/services-et-outils/reseau-agroclimatique/acces-donnees-reseau-agro-inrae
- https://weatherlink.github.io/v2-api/data-permissions · https://www.asso-mb.bzh/reseau-mbdata
- https://www.ncei.noaa.gov/products/land-based-station/integrated-surface-database · https://www.ogimet.com/guia.phtml.en · https://archive.sensor.community/

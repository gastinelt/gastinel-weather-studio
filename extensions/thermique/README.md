# Extension « Analyse thermique » (`thermique`)

Trois analyses de température à partir des mesures du projet actif, calculées
en Rust dans la DLL (déterministes, sans réseau ni IA). Permission **`db_read`
uniquement** : la base est ouverte en `mode=ro`, aucune sauvegarde n'est
déclenchée à l'activation.

## Pourquoi un seul panneau à trois onglets

L'hôte n'offre qu'un bouton `map_view` par extension, qui ouvre une fenêtre.
Les trois analyses partagent la liste des points, la période et la DLL : un
panneau à onglets (Gradient vertical · Épisodes radiatifs · Profil vertical)
évite trois extensions et trois boutons sur la carte. La sélection des points
se fait dans le panneau : l'hôte ne transmet pas la sélection de la carte, et
le système d'extensions n'a pas été modifié.

## Compiler, tester, installer

```bash
cd extensions/thermique
cargo test --release                                   # tests unitaires (données synthétiques)
cargo build --release                                  # target/release/thermique.dll
cargo test --release --test reel -- --ignored --nocapture   # base réelle, lecture seule
cd ../../src-tauri && cargo test --test extension_thermique # bout en bout avec la vraie DLL
```

Installer : copier `manifest.json`, `target/release/thermique.dll` et `ui/`
dans `%APPDATA%\fr.gastinel.weatherstudio\extensions\thermique\`, puis activer
dans **Paramètres → Extensions**.

## Méthodes (`gws_ext_call`)

`project_id` est injecté par l'app. Les dates sont en epoch secondes UTC.

| Méthode | Paramètres | Réponse |
|---|---|---|
| `ping` | — | `"pong"` |
| `points` | — | points du projet ayant au moins une série non vide, avec leurs séries (`role`, `intervalle_s`, `label`, `t_min`, `t_max`, `minimum_horaire`) |
| `gradient` | `points[]`, `debut`, `fin`, `pas` (600–86400, défaut 3600) | `stats`, `serie` (≤ 3000 valeurs, agrégée au-delà), `pas_s`, `tolerance_s`, `dz_m`, `points` (séries utilisées), `avertissements` |
| `episodes` | `point`, `debut`, `fin` | `resultat` (`episodes[]`, `nuits_examinees`, `rejets` par critère, `pas_s`), `seuils`, `serie`, `avertissements` |
| `courbe` | `point`, `debut`, `fin` | température agrégée (≤ 1500 valeurs), pour afficher une nuit |
| `profil` | `points[]`, `t`, `fenetre_s` (0 = instantané, ≤ 7 j) | température de chaque point, `pente_100m`, `avertissements` |

### Garde-fous de volume

- Chaque lecture de mesures filtre sur `series_id` + plage de temps et passe
  par l'index unique `(series_id, timestamp_utc)` : jamais de scan global.
  Les bornes d'une série viennent de `MIN`/`MAX` sur cet index.
- Période limitée à **400 jours**, **30 points** au plus, séries renvoyées au
  panneau agrégées (moyenne/min/max par tranche) au-delà de 3000 valeurs.

### Choix de la série d'un point

Jamais de mélange Primary/Backup (double comptage). On prend une série
**Primary** ; s'il y en a plusieurs, celle qui a le plus de mesures attendues
sur la période (recouvrement ÷ intervalle). Une Backup n'est utilisée que si
aucune Primary ne couvre la période. Une série dont le libellé contient
« minimum » (résumés horaires Infoclimat, température = **minimum de l'heure**)
est signalée partout dans le panneau.

## 1. Gradient thermique vertical

- Grille commune au pas choisi (1 h par défaut). À chaque instant, on prend
  pour chaque point la mesure **la plus proche**, si elle est à moins de la
  tolérance : `max(intervalle_max / 2, 15 min)`, plafonnée à `pas / 2`.
- Un instant n'est retenu que si **tous** les points sélectionnés ont une
  valeur (sinon le gradient changerait parce qu'un point sort de la
  régression). La période de recouvrement réellement utilisée (premier et
  dernier instant commun, nombre d'instants) est affichée.
- Gradient = pente des moindres carrés de T en fonction de z, en °C/100 m
  (avec 2 points, c'est simplement ΔT/Δz). **> 0 = inversion.**
- Écart d'altitude minimal : 50 m (`DZ_MIN_M`), sinon l'erreur des capteurs
  (±0,3–0,5 °C) dominerait.
- Jour / nuit : hauteur du soleil calculée au centre des points
  (> −0,833°, formules de l'Astronomical Almanac).
- Statistiques : part du temps en inversion (totale, nuit, jour), gradient
  moyen (total, nuit, jour), inversion maximale et gradient le plus négatif
  avec leur date.

Limite : la régression mélange effets d'altitude et effets de site (un fond
de doline comme Méaudre est bien plus froid que son altitude ne le voudrait) :
le « gradient » est celui **de l'ensemble de points choisi**, pas celui de
l'atmosphère libre.

## 2. Détecteur d'épisodes radiatifs

Pour une station, chaque soirée dont le coucher du soleil tombe dans la
période est examinée. La série est rééchantillonnée sur une grille régulière
de pas `max(intervalle, 10 min)` (interpolation linéaire sur les trous
≤ 3 pas). Les vitesses sont mesurées sur **1 h glissante**.

| Critère | Constante | Valeur | Rôle |
|---|---|---|---|
| Couverture | `COUVERTURE_MIN` | 80 % de coucher − 3 h → lever + 3 h | sinon « données insuffisantes » |
| 1. Refroidissement crépusculaire | `REFROIDISSEMENT_MIN_C_H` | ≥ 1,5 °C/h, entre coucher − 3 h et coucher + 3 h | chute rapide propre aux nuits claires et calmes |
| Début de l'épisode | `DEBUT_C_H` | première heure à ≥ 1,0 °C/h dans la même fenêtre | |
| 2. Amplitude | `AMPLITUDE_MIN_C` | Tmax (coucher − 6 h → coucher) − Tmin ≥ 6 °C | |
| 3a. Pas de remontée | `REMONTEE_MAX_C` | jamais plus de 1,5 °C au-dessus du minimum déjà atteint, avant le minimum | écarte les passages nuageux / coups de vent |
| 3b. Plateau | `PLATEAU_MAX_C_H` | baisse moyenne < 0,8 °C/h sur les 3 h avant le minimum | stabilisation ; écarte une advection froide continue |
| 4. Rupture matinale | `RUPTURE_C_H` | hausse ≥ 1,0 °C/h sur 1 h, au plus tôt 1 h avant le lever, au plus tard 5 h après | le réchauffement doit être solaire |
| 5. Vent (si mesuré) | `VENT_MAX_KMH` | vent moyen coucher → lever ≤ 10 km/h | ignoré si la série n'a pas de vent |

Résultat par épisode : date (soir), début, heure et valeur du minimum,
amplitude, vitesse de refroidissement maximale, durée (début → rupture),
heure de rupture. La réponse compte aussi les nuits rejetées **par premier
critère non satisfait**, ce qui aide à régler les seuils.

### Limites (à lire)

- **Approximatif et non calibré scientifiquement** : seuils empiriques
  choisis pour des sites de fond de vallée / doline du Vercors, non validés
  contre des observations de nébulosité ou de rayonnement.
- Méthode fondée sur la **seule température d'une station** : un brouillard,
  une advection froide calme ou une nuit partiellement nuageuse peuvent être
  confondus avec un épisode radiatif ; à l'inverse un site ventilé (col,
  sommet, versant) manque souvent de vrais épisodes.
- **Sensible au pas de temps** : au pas horaire, le refroidissement est
  lissé et les heures (début, minimum, rupture) sont à ±1 h.
- **Résumés horaires « minimum »** : la température est le minimum de
  l'heure ; les vitesses et l'amplitude sont biaisées (le panneau l'affiche).
- En site encaissé, l'ombre portée du relief fait commencer le refroidissement
  bien avant le coucher astronomique : le « début » peut précéder le coucher
  de 2 à 3 h (la fenêtre de recherche commence à coucher − 3 h).
- Le vent n'est pris en compte que s'il est mesuré dans la même série
  (les séries Infoclimat de l'app n'en ont en général pas).

## 3. Profil vertical

Température en fonction de l'altitude des points sélectionnés à un instant :
mesure la plus proche (± max(intervalle/2, 15 min)) ou moyenne sur une
fenêtre centrée (1 h, 3 h, 24 h). Le curseur parcourt la période choisie
(pas 1 h) ; un point sans mesure à l'instant affiché est listé comme manquant.
La droite pointillée est la régression (rouge = inversion, bleu = normal).

## Fichiers

- `src/calc.rs` : calculs purs et tests unitaires synthétiques ;
- `src/lib.rs` : lecture SQLite (lecture seule), choix des séries, méthodes ;
- `tests/reel.rs` : test `#[ignore]` sur la base réelle (lecture seule) ;
- `ui/index.html`, `ui/thermique.js` : panneau en JS pur (canvas), sans script
  inline ni ressource externe (CSP du panneau).

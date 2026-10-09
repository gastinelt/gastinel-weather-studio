# Propositions pour la 0.6

Document de travail, rédigé sur la base de `release/0.5.0` (0.5.0-beta.1). **Rien n'est
implémenté** : ce sont des propositions à arbitrer. Les faits cités renvoient au code
(`fichier:fonction`) ou à un document du dépôt ; ce qui n'a pas pu être vérifié est marqué
*(à vérifier)*.

---

## A. Simplifier l'import

### A.1 Inventaire du parcours actuel

**Points d'entrée**

| Entrée | Où | Ce qui se passe |
|---|---|---|
| Bouton « Importer » / Ctrl+I | grille du projet (`wireUp`, raccourci ajouté en 0.5.0) | ouvre `#dlg-import` sur l'onglet par défaut |
| « Importer des données » | écran d'accueil du projet (`quitterAccueil("import")`) | idem, ou fiche point si le projet n'a aucun point |
| Dépôt d'un fichier sur la fenêtre | `onDragDropEvent` → `deposerFichierImport` (0.5.0) | lit le fichier (`read_dropped_file`), appelle `detect_import_format` (Rust) et bascule sur l'onglet deviné |
| Sélecteur de fichier dans la fenêtre d'import | `#import-file` → `onFileChosen` → `chargerFichierImport` | charge le fichier ; **pas** de détection de format, sauf le cas xlsx (signature zip → onglet « Automatique ») |
| Zone de dépôt de l'assistant IA | onglet `ai` (`handleAiFiles`) | file d'attente de fichiers/images, analyse par Groq |
| « Récupérer » Infoclimat (API) | fiche point (`fetchInfoclimat`, `btn-fetch-ic`) et fenêtre d'historique | appel direct à l'API, hors de la fenêtre d'import |
| Page RC-5 / surveillance USB | `elitech_watch.rs` (sondage des ports) + onglet `rc5` | lecture directe du logger |

**Onglets de la fenêtre d'import** (`src/index.html`, `#dlg-import`) : sept onglets.

1. `ai` — Assistant IA (fichiers CSV/JSON/TXT/TSV, dossiers, images) ; demande une clé Groq.
2. `auto` — Automatique (Elitech) : export ElitechLog `.xls` (en réalité xlsx, reconnu à sa
   signature, `looksLikeXlsx` / `elitech_xlsx.rs`) ou texte `.txt`/`.csv`
   (`elitech_import.rs`). `.esf` et `.elt` non lisibles (dit dans l'aide).
3. `rc5` — USB direct (`elitech_serial.rs`).
4. `manual` — Manuel (colonnes mappées) : séparateur, lignes ignorées, colonnes date / heure /
   température / humidité (`fillMappingSelects`).
5. `entry` — Saisie manuelle sans fichier (`manual_entry.rs`), une valeur par jour.
6. `infoclimat` — export Infoclimat JSON ou CSV (`infoclimat.rs`, format reconnu au contenu),
   option « compléter la série existante », champ station si le fichier en contient plusieurs.
7. `romma` — archive `archives_*.csv` (`romma_import.rs`), `climato_*.csv` refusés.

**Champs communs** : point de rattachement (`#import-point`, obligatoire), rôle de la série
(Principal / Secours, jamais déduit), bloc horloge (fuseau, dérive, n° de série) masqué pour
les sources nativement UTC (`switchTab`).

**Détection de format** (`src-tauri/src/import_detect.rs::detecter`) : image → `ai` ; zip →
`auto` ; puis Infoclimat, ROMMA, intitulés ElitechLog, dans cet ordre ; sinon `manual` avec
`reconnu: false`. Le contenu prime sur l'extension. Tests unitaires présents.

**Onglet par défaut** (`ongletImportParDefaut`) : dernier onglet utilisé (localStorage
`gws_import_tab`, 0.5.0), sinon `ai` si une clé Groq est enregistrée, sinon `auto`.

### A.2 Points de friction relevés dans le code

1. **Deux chemins de chargement, un seul détecte.** Le dépôt sur la fenêtre passe par
   `detect_import_format` ; le sélecteur `#import-file` (`onFileChosen`) non. Un CSV
   Infoclimat choisi au sélecteur depuis l'onglet « Manuel » reste dans « Manuel ».
2. **Sept onglets à lire avant d'agir.** L'utilisateur doit savoir d'où vient son fichier
   pour choisir l'onglet, alors que `detecter` sait déjà le trouver pour 4 formats sur 5.
3. **Aperçu séparé de l'import.** Chaque onglet a son bouton « Analyser le fichier »
   (`btn-analyze`, `btn-analyze-ic`, `btn-analyze-romma`) ; l'aperçu n'est pas automatique
   au chargement, et le bouton « Importer » est actif avant.
4. **Messages de garde en aval.** Un classeur xlsx chargé dans un autre onglet que
   « Automatique » est refusé au moment de l'import (`submitImport`), pas au chargement.
5. **Aucun point = impasse.** Dépôt ou import sans point → toast « Ajoutez d'abord un
   point » (`deposerFichierImport`, `quitterAccueil`) ; le fichier déposé est perdu, il faut
   le redéposer après avoir créé le point.
6. **Association point / station non exploitée.** Les points portent
   `infoclimat_station_id` et `romma_station_id` (`model.rs`) ; ils servent à pré-remplir
   les champs station, mais pas à proposer le point quand le fichier désigne une station
   connue (le point présélectionné vient de la fiche ouverte ou de la sélection unique).
7. **Mappings non mémorisés.** Le mapping de l'onglet « Manuel » et celui corrigé après
   l'IA (`importState.ai.currentMapping`) ne sont pas conservés : le même export d'un logger
   inconnu se remappe à chaque fois (aucune persistance trouvée, ni en base ni en
   localStorage).
8. **Un fichier à la fois** au dépôt fenêtre (« seul le premier est ouvert ») alors que
   l'onglet IA accepte plusieurs fichiers et des dossiers.
9. **Infoclimat en deux endroits.** Fichier dans la fenêtre d'import ; API dans la fiche
   point. Même série, deux parcours.
10. **Onglet IA par défaut dès qu'une clé existe** : un export ElitechLog standard, lu
    localement sans réseau par `auto`, part vers un service distant si l'utilisateur ne
    change pas d'onglet.

### A.3 Options de refonte

#### Option 1 — « Détecter partout » (évolution minimale)

- `onFileChosen` appelle `detect_import_format` comme le dépôt (factoriser avec
  `deposerFichierImport`).
- Aperçu lancé automatiquement après détection (appel de l'`analyze_*` de l'onglet choisi).
- Onglet par défaut : `auto` même avec une clé Groq ; l'IA reste un repli proposé quand
  `reconnu: false`.
- Le dépôt sans point ouvre la création de point puis reprend le fichier (garder le `File`
  en mémoire le temps du dialogue).

Effort : **faible** (1 à 2 jours, JS surtout, aucune migration). Risque : faible ;
l'analyse automatique d'une archive ROMMA de ~40 Mo (ordre de grandeur donné dans l'aide de
l'onglet) doit rester annulable et montrer la progression existante.

#### Option 2 — Une seule zone « Importer » avec détection, aperçu, confirmation (recommandée)

Écran unique en trois temps :

1. **Déposer / choisir** (un ou plusieurs fichiers, ou « Lire le logger USB », ou
   « Saisir à la main »). Plus d'onglets par source.
2. **Aperçu** par fichier : format détecté (libellé de `FormatDetecte`), période, nombre de
   mesures, anomalies, et **point proposé** :
   - station Infoclimat / ROMMA du fichier = `infoclimat_station_id` / `romma_station_id`
     d'un point → ce point ;
   - n° de série Elitech déjà vu sur une série d'un point → ce point ;
   - sinon, point de la fiche ouverte, sinon choix obligatoire (jamais deviné en silence).
   Un lien « Changer de format » garde l'accès aux lecteurs actuels en cas d'erreur de
   détection ; « Format inconnu » propose Manuel ou IA.
3. **Confirmer** : un bouton pour le lot, avec la confirmation de ré-import existante
   (`avecConfirmationDeReimport`).

Les sept lecteurs Rust restent tels quels ; seule l'orchestration change. Les panneaux
actuels deviennent des « réglages avancés » du format détecté (fuseau / dérive pour les
loggers, station pour un fichier Infoclimat multi-stations).

Effort : **moyen** (5 à 8 jours : refonte de `#dlg-import` et de `submitImport`, une
commande Rust de proposition de point, tests). Risques : régression sur les cas particuliers
aujourd'hui portés par un onglet (xlsx, horloge naïve vs UTC, garde « un jour = une valeur »
de la saisie manuelle) ; import par lot à rendre interruptible ; à tester sur les fichiers
de `docs/samples/`.

#### Option 3 — Option 2 + mémorisation des mappings

- Table `import_profiles` (empreinte d'en-tête normalisée → séparateur, lignes ignorées,
  colonnes, fuseau, point) alimentée à chaque import « Manuel » ou IA validé.
- À la détection, une empreinte connue donne `reconnu: true` avec le profil, sans appel IA.
- Gestion des profils dans les Paramètres (voir, renommer, supprimer).

Effort : **moyen+** (Option 2 + 3 à 4 jours, migration SQLite). Risques : un profil
appliqué à un fichier dont l'en-tête coïncide mais dont le sens des colonnes a changé
(il faut donc toujours afficher l'aperçu avant de confirmer) ; synchronisation Cloud des
profils à décider (probablement non en 0.6).

**Recommandation A** : faire l'option 1 d'abord (petit correctif utile tout de suite),
puis l'option 2 en 0.6 ; garder l'option 3 pour une version suivante.

---

## B. Service d'arrière-plan pour les API connectées

### B.1 Sources réseau présentes dans le code

| Source | Code | Déclenchement aujourd'hui | Limites connues (source) |
|---|---|---|---|
| **Infoclimat Open Data** (API) | `infoclimat_client.rs`, `commands.rs::fetch_infoclimat` | manuel (fiche point / historique) | tranches de **7 jours** par requête (`infoclimat.rs::MAX_DAYS_PER_REQUEST`) ; pause **1010 ms** entre tranches (`PAUSE_ENTRE_TRANCHES`, commentaire « < 1 req/s » — règle non sourcée dans le dépôt *(à vérifier dans la doc Infoclimat)*) ; compteur journalier avec plafond « recommandé » de **5000** (`DailyQuotaInfo`) *(origine du chiffre à vérifier)* ; intervalle minimal par série réglable, défaut **60 min**, bornes 1 min – 7 jours (`SETTING_REFRESH_MINUTES`) ; clé liée à l'IP publique, réponse `200` « Wrong ip address » traitée en erreur d'auth ; relais SSH optionnel (`ssh_relay.rs`) |
| **ROMMA** | `romma_import.rs` | fichier uniquement | **aucune API** ; export réservé aux membres ; « ne pas scraper » (`docs/sources_meteo_complementaires.md` §1) → **exclu** du service |
| **Météo-France** | aucun code ; cadrage dans `docs/meteo-france-cadrage.md` (working tree principal, non commité sur cette branche) | — | fichiers data.gouv sans compte, mis à jour quotidiennement pour 2025-2026 ; DPObs 24 h de profondeur, jeton personnel ; quota **100 req/min** depuis janvier 2026 « en phase d'expérimentation » (auparavant 50) selon la page Confluence citée dans le cadrage ; CGU du portail *(à vérifier)* |
| **Cloud GWS** | `cloud.rs`, `cloud_mois.rs` | une synchro automatique par session (`cloudAutoSyncFaite`, réglage `cloud_auto_sync`) | serveur maison ; limites de débit côté serveur *(à vérifier dans le code serveur, hors de ce dépôt)* |
| **Mises à jour** | `update.rs` | au démarrage (`verifierMisesAJourAuDemarrage`) et à la demande | — |
| **Catalogue d'extensions** | `extensions_catalogue.rs` | à l'affichage du catalogue (`extensions.js::rendreCatalogue`) | — |
| **Groq** (IA) | `groq.rs`, `ai_import.rs` | à la demande | les réponses 413/429 reprises dans les tests de `groq.rs` montrent un plafond de 8000 jetons/minute (TPM) pour le palier « on_demand » d'un compte ; le plafond dépend du compte *(à vérifier pour chaque utilisateur)* — **hors service** : aucune raison de l'appeler en tâche de fond |
| **IGN** (altitude, tuiles) | `altimetrie.rs`, `tiles.rs` | à la demande | hors périmètre |
| **Loggers RC-5** | `elitech_watch.rs` | sondage local des ports | pas une API, déjà en arrière-plan |

Seules deux sources ont un sens pour une collecte périodique : **Infoclimat** (existant) et
**Météo-France** (à construire, fichiers data.gouv puis DPObs). Le Cloud a déjà son propre
mécanisme ; l'intégrer au même planificateur est possible mais secondaire.

### B.2 Architecture proposée (dans le processus Tauri)

**Principe** : un planificateur unique, une tâche `tokio` lancée dans `setup`, qui ne
fait **rien** tant que l'option n'est pas activée. Option **désactivée par défaut**
(réglage `background_fetch_enabled = 0`), activable par source et par point.

```
setup() ── spawn(planificateur)
              │  toutes les 60 s : « qu'est-ce qui est dû ? »
              ├─ tâches dues = points avec station liée
              │    ET source activée ET (maintenant ≥ prochain_essai)
              ├─ file par source, un seul appel en vol par source
              ├─ limiteur (jeton seau) par source
              └─ résultat → base (append incrémental) → événement UI
```

- **Tâche** : `tauri::async_runtime::spawn` d'une boucle `tokio::time::interval` (la crate
  `tokio` avec `time` est déjà une dépendance). Les appels HTTP existants étant bloquants,
  chaque appel passe par `spawn_blocking`, comme `fetch_infoclimat` aujourd'hui.
- **Réutilisation** : extraire le cœur de `fetch_infoclimat` (calcul de la période depuis
  `last_measurement_utc`, appel, `persist_infoclimat`) en une fonction appelée à la fois par
  la commande et par le planificateur. Le stockage est déjà incrémental : reprise à la
  dernière mesure, dédoublonnage par contrainte d'unicité.
- **État persistant** (table `background_jobs`) : source, point, série, `last_success_at`,
  `next_attempt_at`, `consecutive_failures`, `last_error` (message nettoyé par `scrub` /
  `redact`, jamais d'URL avec jeton).
- **Planification** : intervalle par source = max(réglage utilisateur, plancher de la
  source). Infoclimat : réutiliser `SETTING_REFRESH_MINUTES` comme plancher (défaut 60 min).
  Météo-France fichiers : au plus quotidien, avec `ETag` / `If-Modified-Since` (proposition
  du cadrage §4). DPObs : 1 appel par station toutes les 6 à 60 min (ordre de grandeur du
  cadrage), très en dessous du quota annoncé.
- **Quotas** : limiteur par source ; Infoclimat garde la pause 1010 ms et le compteur
  journalier existant (`increment_daily_quota`) ; le service s'arrête pour la journée à un
  seuil prudent (par exemple 50 % du plafond enregistré) pour laisser la place aux usages
  manuels. Toute valeur de quota vient d'un réglage ou d'une constante documentée, jamais
  d'une supposition.
- **Backoff** : exponentiel avec gigue (1, 2, 4… plafonné à 6 h) sur erreur réseau, `429`,
  `503` ; en `429`, respecter `Retry-After` s'il est présent. Erreur d'authentification
  (clé invalide, « Wrong ip address ») : **suspendre la source** et notifier, pas de
  réessai en boucle.
- **Reprise après veille** : à chaque tick, comparer l'horloge murale au tick précédent ;
  un saut > 2 intervalles = retour de veille → pas de rafale : les tâches dues sont
  étalées (une par source et par minute). Le calcul « depuis la dernière mesure » rattrape
  naturellement le trou, en tranches de 7 jours pour Infoclimat.
- **Réseau absent** : un échec de résolution DNS / connexion compte comme « hors ligne »,
  sans incrémenter les échecs de la source.
- **Notifications** : événement Tauri `background-fetch-status` pour l'UI ; notification
  système (greffon `tauri-plugin-notification`, **nouvelle dépendance**) seulement pour les
  cas qui demandent une action (clé refusée, source suspendue), jamais à chaque succès.
- **Indicateur d'état dans l'UI** : pastille dans la barre du haut (inactif / à jour /
  en cours / erreur), infobulle « Infoclimat : dernière récupération il y a 12 min,
  prochaine dans 48 min ». Panneau Paramètres : interrupteur global, sources, fréquence,
  journal des 20 derniers passages, bouton « Suspendre ».
- **Concurrence avec l'UI** : un verrou par série empêche qu'un « Récupérer » manuel et le
  service écrivent la même série en même temps ; le manuel a priorité.
- **Mode mesure / dev** : si `GWS_DATA_DIR` est défini, le service reste désactivé sauf
  variable explicite, pour ne jamais consommer le quota réel depuis une instance de test.

### B.3 Application fermée : trois options

| Option | Fonctionne fenêtre fermée | Fonctionne session fermée | Coût / risques |
|---|---|---|---|
| **1. Dans le processus, fenêtre ouverte seulement** | non | non | aucun ajout ; la collecte s'arrête à la fermeture, rattrapage à la réouverture (le calcul incrémental le fait déjà) |
| **2. Icône de zone de notification** (fermer = masquer ; greffon/feature `tray-icon` de Tauri, et éventuellement `tauri-plugin-autostart` pour démarrer avec la session, **opt-in**) | oui | non | WebView gardée en mémoire (masquée) sauf à la détruire et la recréer ; confusion possible « j'ai fermé mais ça tourne » → menu clair « Quitter » ; interaction avec `tauri_plugin_single_instance` et le relancement après mise à jour (`ARG_APRES_MAJ`) à vérifier |
| **3. Service Windows** (binaire séparé, sans UI) | oui | oui | droits administrateur à l'installation, installeur à modifier, compte de service ≠ compte utilisateur : les clés (`Clee.API/`) et la base sont dans le profil utilisateur → à déplacer ou partager, ce qui touche à la sécurité des secrets ; accès concurrent à la base SQLite par deux processus ; spécifique Windows (rien pour Android) |

**Recommandation B** : option 1 en 0.6 (planificateur dans le processus, désactivé par
défaut, Infoclimat seul), puis option 2 derrière un réglage séparé, lui aussi désactivé par
défaut. L'option 3 n'est pas proportionnée pour une application de bureau personnelle :
le rattrapage incrémental couvre les heures où l'application est fermée, au prix d'un
délai, sans déplacer les secrets hors du profil utilisateur.

Ordre suggéré : (1) extraction du cœur de `fetch_infoclimat` + table d'état ;
(2) planificateur + backoff + indicateur UI ; (3) Météo-France fichiers quotidiens une
fois l'import manuel en place (cadrage §4, étapes 1-2) ; (4) zone de notification.

Effort estimé : étapes 1-2 **4 à 6 jours** ; étape 3 dépend du chantier Météo-France ;
étape 4 **2 à 3 jours**, plus les essais de mise à jour et d'instance unique.

Risques principaux : consommation silencieuse d'un quota partagé avec les usages manuels
et les scripts (`data/stations_fr/README.md` indique que les scripts utilisent la même clé
et le même compteur) ; clé Infoclimat liée à l'IP qui échoue en boucle hors du réseau
déclaré (d'où la suspension sur erreur d'auth) ; écriture en base pendant qu'un import
manuel ou une synchro Cloud tourne.

### B.4 Questions ouvertes

- Faut-il un réglage par point (« collecter automatiquement cette station ») ou global ?
- Le seuil d'arrêt sur quota Infoclimat (proportion du plafond) : quelle valeur ?
- Le Cloud doit-il rejoindre le planificateur (synchro périodique) ou rester « une fois par
  session » comme en 0.5.0 ?
- Règles d'usage Infoclimat et CGU du portail Météo-France pour des appels périodiques
  automatisés *(à vérifier auprès des fournisseurs)*.

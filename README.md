# Gastinel Weather Studio

Application desktop (Tauri v2 + Rust) pour la **chasse aux TAF** — les Trous À Froid du
massif du Vercors. Elle centralise des relevés de température venant de plusieurs sources
et permet de comparer la « puissance » thermique des sites prospectés.

**État actuel : socle + carte + interface + API Infoclimat + agent IA + les trois sources
de fichiers réelles + la lecture USB directe du RC-5 + une cible Android qui fait du
réseau.** Le modèle de données, la base SQLite, la carte interactive sur fonds
IGN, la grille 2×2 de l'espace de travail, la récupération des données Infoclimat (stations
Vantage Pro 2 du réseau StatIC **et** Crête de la Grande Moucherolle), l'**agent d'analyse
Groq** (§ 6) et l'**écran d'accueil animé** (§ 7) sont fonctionnels.

Trois sources de fichiers sont désormais lues **contre des exports réels**, et non contre
des hypothèses :

| Source | Format | Section |
|---|---|---|
| **ElitechLog** | un `.xls` qui est en réalité un **xlsx** — l'hypothèse du TXT tabulé était fausse | § 4 |
| **Infoclimat** | JSON **et CSV**, reconnus au contenu, un seul chemin de lecture | § 5 |
| **ROMMA** | `archives_*.csv` — nouvelle source, six grandeurs météo de plus | § 5bis |

À quoi s'ajoute une source qui n'est pas un fichier du tout : le **RC-5 lu directement par
son port série** (§ 4bis). Branché en USB, le logger est détecté tout seul et sa mémoire se
lit en un clic, sans ElitechLog. La voie a été **validée croisée contre l'export xlsx du
même appareil** : 10 307 mesures, zéro écart d'horodatage, zéro écart de température.

**L'export de fichiers n'est plus un placeholder** : CSV, TXT et HTML s'écrivent
réellement sur le disque, sur poste fixe **et** sur Android (§ 1, « Appels HTTP sur
Android », pour le détour par le sélecteur de documents du système). Le croisement de
données est complet lui aussi — statistiques comparées et courbes superposées sur une
fenêtre temporelle commune (§ 11). L'accès direct à Orange Live Objects n'est **pas**
construit — seule la valeur d'énumération existe (cf. § 5, « Ce que devient `DraginoApi` »).
Le **RC-5+** n'est pas supporté par la lecture directe, et les formats ElitechLog `.esf` et
`.elt` ne sont **pas** lus : ces deux décisions sont actées (§ 4bis et § 4).

**Vérifié en septembre 2026** sur Windows 11 (Rust 1.97.1, MSVC 14.44, WebView2 151) :
compilation sans avertissement de code **sur les deux cibles** (Windows et
`x86_64-linux-android`), **395 tests au vert** dans l'application (364 unitaires + 31
d'intégration, 2 `#[ignore]` nécessitant un RC-5 USB physique) **et 3 dans le greffon USB**,
migrations v1 → v6 rejouées sur des bases existantes, **appel réel à l'API
Groq réussi** (`openai/gpt-oss-20b` et Vision `qwen/qwen3.8-27b` / repli `qwen/qwen3.6-27b`), et **lecture série réelle du RC-5
réussie** — 10 307 mesures en 9,3 s sur `COM8`, reset validé sur matériel réel.

**Complété le 23 août 2026** — trois points mobiles, vérifiés sur l'émulateur Pixel 10 :

* l'**accueil du téléphone est coupé en deux**, liste des projets en haut et détection du
  RC-5 en bas, avec un écran plein par logger. Le RC-5 a **disparu du contexte projet** sur
  mobile — plus de notification de branchement, plus d'onglet « USB direct » dans l'import,
  plus de bandeau pleine largeur (§ 4ter, « l'accueil coupé en deux ») ;
* l'**APK de release est signé**, et c'est la cause première du « Application non
  installée » : l'APK produit jusqu'ici ne portait **aucun certificat**. Ce n'était pas le
  `minSdk` (24, très en dessous des 31 d'Android 12). Démontré dans les deux sens — le signé
  s'installe, le même privé de sa signature est refusé sur
  `INSTALL_PARSE_FAILED_NO_CERTIFICATES` (§ « Signature de l'APK »). **Deux autres causes
  donnent le même message** et se cumulent en pratique : un APK construit avec `--target`
  (une seule architecture, sous un nom qui dit « universal »), et une version antérieure
  signée d'une autre clé restée sur le téléphone. La marche à suivre couvre les trois
  (§ « Poser l'APK sur un téléphone ») ;
* l'**export CSV / TXT / HTML n'a pas bougé** : le sélecteur de documents Android s'ouvre
  toujours depuis le bloc Stations, après la restructuration de l'accueil.

⚠️ **L'appel réel à l'API Infoclimat échoue actuellement**, sur le poste fixe comme sur
mobile, avec `{"errors":["Wrong IP address"]}` : l'IP publique déclarée à la génération de
la clé n'est plus la bonne. Ce n'est pas une régression du code — c'est la limitation du
§ 5, et le remède est d'y regénérer une clé. Le test `#[ignore]` d'appel réel le montre tel
quel.

**Sur Android**, vérifié le même jour sur un émulateur Pixel (API 36) : les **trois appels
HTTPS** aboutissent (tuiles `data.geopf.fr`, agent Groq, aller-retour complet avec
Infoclimat), le **dézoom ne gèle plus** (40 tuiles non cachées en 993 ms, interface
réactive à 2 ms pendant la rafale), la **page RC-5 est utilisable à la taille d'un
téléphone**, et l'**export écrit réellement son fichier** à travers le sélecteur de
documents du système. En revanche la **lecture directe du RC-5 par USB-OTG n'a jamais
tourné sur du matériel** : tout le chemin existe et le pont Rust ↔ Kotlin est vérifié, mais
l'émulateur n'a pas de contrôleur USB hôte (§ 4quater — **à lire avant de s'y fier**).

Les **quatre fichiers de référence** sont parsés de bout en bout, et les chiffres qui
suivent sont ceux qu'affirment les tests :

| Fichier réel | Résultat |
|---|---|
| `Clee.API/Moucherolle.json` (9,78 Mo) | 7 905 mesures, zéro doublon — **mesuré le 19 août, non rejoué depuis** : le fichier n'est plus sur le poste, ses tests s'abstiennent |
| `docs/samples/elitech_rc5_reel.xls` (254 Ko) | **10 307 mesures**, zéro ligne écartée, bornes et extrêmes conformes au résumé du fichier lui-même, saut de fin de série signalé |
| `export_infoclimat.csv` (17 Mo) | **137 088 mesures**, zéro écartée, zéro `complements` illisible |
| `archives_Lans - Les Allières_….csv` (38 Mo) | **476 912 lignes**, 10 621 sans température écartées, **466 291 mesures importées**, zéro doublon |
| `docs/samples/rc5_direct/*.bin` (20 Ko) | trames série brutes du **même** RC-5 : **10 307 mesures**, concordance totale avec le xlsx (§ 4bis) |

Les **deux fonds IGN** chargent toujours depuis data.geopf.fr — orthophoto et TOP25, ce
dernier avec la clé provisoire `ign_scan_ws` (cf. § 9, et **ce qu'il faut changer le jour
où elle expire**).

---

## 1. Lancer le projet

### Prérequis

| Outil | Pourquoi | Installation |
|---|---|---|
| **Rust** (stable ≥ 1.77.2) | tout le backend | <https://rustup.rs> |
| **MSVC Build Tools** + SDK Windows | l'éditeur de liens de Rust sous Windows | Visual Studio Installer → « Desktop development with C++ » |
| **WebView2** | moteur de rendu de la fenêtre | déjà présent sur Windows 11 |
| **Tauri CLI** | commandes `dev` / `build` | `cargo install tauri-cli --version "^2"` |

> **Node.js n'est pas nécessaire.** Le frontend est du HTML/CSS/JS statique servi
> directement depuis `src/`, sans bundler. C'est un choix délibéré : une dépendance de
> moins à maintenir sur des postes familiaux, et un `cargo` suffit pour tout faire.

### Commandes

```bash
cargo tauri dev
```

```bash
cargo tauri build
```

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

Le greffon USB d'Android (§ 4quater) est un paquet séparé : ses tests ne sont **pas** pris
par la commande ci-dessus.

```bash
cargo test --manifest-path src-tauri/plugins/tauri-plugin-usbserial/Cargo.toml
```

La première compilation est longue (Tauri + SQLite compilé depuis les sources).

Deux tests font de **vrais appels réseau** et sont donc `#[ignore]` par défaut — `cargo test`
ne doit dépendre ni du réseau ni d'un quota. Ce sont les outils de diagnostic à lancer le
jour où « ça ne marche plus » :

```bash
cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture appel_reel
```

Un troisième test `#[ignore]` demande non pas le réseau mais **le RC-5 branché en USB** : il
refait toute la chaîne série sur le matériel et la compare à l'export xlsx (§ 4bis).

```bash
cargo test --manifest-path src-tauri/Cargo.toml --test rc5_direct -- --ignored --nocapture
```

Et quand le logger n'est pas détecté, l'outil de diagnostic dit **où ça s'arrête** —
énumération des ports, identification, lecture complète — plutôt que « pas de logger » :

```bash
cargo run --manifest-path src-tauri/Cargo.toml --example rc5_diagnostic
```

### Build Android

**Prérequis Android** :
- Android Studio avec SDK/NDK configuré
- Variables d'environnement : `ANDROID_HOME`, `NDK_HOME`, `JAVA_HOME`
- Cibles Rust Android : `aarch64-linux-android`, `armv7-linux-androideabi`, `i686-linux-android`, `x86_64-linux-android`
  - Installation : `rustup target add <cible>`
- Windows Developer Mode activé (pour les liens symboliques)
- Un émulateur Android (AVD Manager dans Android Studio)

**Compilation et déploiement** :

```bash
cargo tauri android dev
```

Lance la compilation croisée Rust → Android x86_64, construit l'APK, et le déploie sur l'émulateur.

**Génération de l'APK** (mode release, signé — cf. « Signature de l'APK » plus bas) :

```bash
cargo tauri android build
```

L'APK se trouve à :
```
src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
```

Sans `keystore.properties`, le même fichier sort **non signé** (`…-release-unsigned.apk`)
et **ne s'installe sur aucun téléphone** — voir « Signature de l'APK ».

Un AAB (Android App Bundle) est aussi généré à :
```
src-tauri/gen/android/app/build/outputs/bundle/universalRelease/app-universal-release.aab
```

**Limitations Android actuelles** :
- ⚠️ **La lecture directe du RC-5 par USB-OTG n'a jamais tourné sur du matériel.** Tout le
  chemin est en place et le pont Rust ↔ Kotlin est vérifié sur l'émulateur, mais celui-ci
  n'a pas de contrôleur USB hôte : rien de la puce CH340 elle-même n'a pu être exercé. À
  tenir pour non fonctionnel tant qu'un téléphone + adaptateur OTG + le logger n'auront pas
  confirmé (§ 4quater).
- Les racines de certificats sont **embarquées dans l'application**, pas prises au système :
  leur mise à jour est liée à celle de l'APK (voir « Appels HTTP sur Android » ci-dessous).
- La clé Infoclimat est liée à l'IP publique : une clé générée depuis le poste fixe est
  refusée depuis un téléphone en 4G (§ 5 et ci-dessous).
- Seule la **page RC-5** a été reprise pour le petit écran. La grille 2×2 de l'espace de
  travail ne l'a pas été.
- **Version minimale d'Android supportée : 7.0 Nougat (API 24)**, et il n'y a pas de
  maximum. Le téléphone du père est en **Android 12 (API 31)** : très largement au-dessus.
  Voir « Version d'Android supportée » plus bas.
- APK signé par une clé auto-signée, pour l'installation directe : pas de compte Play
  Console, donc pas de publication sur le magasin (« Signature de l'APK »).
- Le **dézoom de la carte** a été vérifié sur l'émulateur en Wi-Fi et hors ligne, mais
  **pas sur un réseau lent** : l'AVD ignore `adb emu network delay/speed` sur son Wi-Fi
  (mesuré : 1 766 ms avec un délai de 4 000 ms demandé). Le cas « 4G qui traîne » reste à
  vérifier sur appareil physique (§ 9).

**État actuel** : l'application démarre sur l'émulateur, la carte se charge depuis
`data.geopf.fr`, l'agent IA répond, et la page RC-5 s'affiche à la taille d'un téléphone.
Les tests desktop restent au vert, sans régression.

### Signature de l'APK — la cause de « Application non installée »

**Symptôme** : sur le téléphone Android 12 du père, l'installation de l'APK échoue au
moment de l'installation (pas au lancement), sur le message générique « Application non
installée », sans autre détail.

**Diagnostic.** Quatre pistes, écartées ou retenues sur des faits et non sur des
impressions :

| Piste | Vérification | Verdict |
|---|---|---|
| `minSdkVersion` trop élevé | `minSdk = 24` dans `gen/android/app/build.gradle.kts` **et** dans `plugins/tauri-plugin-usbserial/android/build.gradle.kts`. Android 12 = API 31 | ❌ écartée — 31 ≫ 24 |
| `usb-serial-for-android` exige plus récent | Le greffon déclare le même `minSdk = 24` ; la bibliothèque (3.9.0) ne remonte pas ce plancher | ❌ écartée |
| Architecture (ABI) absente de l'APK | `unzip -l` sur l'APK universel : `lib/arm64-v8a/`, `lib/armeabi-v7a/`, `lib/x86/`, `lib/x86_64/` — les quatre sont là | ❌ écartée |
| Conflit avec une installation antérieure signée autrement | Reste possible, mais ne peut pas être la cause **première** : voir ci-dessous | ⚠️ à écarter à l'installation |
| **APK non signé** | `apksigner verify` sur `app-universal-release-unsigned.apk` : `DOES NOT VERIFY — Missing META-INF/MANIFEST.MF`. Aucun bloc de signature, d'aucun schéma | ✅ **la cause** |

Le nom du fichier le disait déjà : `app-universal-release-**unsigned**.apk`. La
configuration Gradle générée par Tauri ne comporte aucun `signingConfig` pour le type
`release`, et **aucune version d'Android n'installe un APK non signé** — l'installateur
s'arrête avant même de regarder le manifeste, d'où un message qui ne dit rien du problème.
Ce n'est pas propre à Android 12 : le même APK échoue partout, de la même façon.

Ce qui masquait le problème : `cargo tauri android dev` et `--debug` produisent un APK
signé par la **clé de débogage** du SDK Android (`CN=Android Debug`), qui s'installe très
bien. Tout ce qui a été vérifié jusqu'ici l'avait été en debug.

**Correction** — un bloc `signingConfigs` dans `gen/android/app/build.gradle.kts`, alimenté
par un fichier `keystore.properties` qui n'est pas dans le dépôt :

```kotlin
// gen/android/app/build.gradle.kts
val keystoreProperties = Properties().apply {
    val fromEnv = System.getenv("GWS_KEYSTORE_PROPERTIES")
    val propFile = if (fromEnv != null) File(fromEnv) else rootProject.file("keystore.properties")
    if (propFile.exists()) propFile.inputStream().use { load(it) }
    else logger.warn("keystore.properties introuvable : l'APK de release sera NON SIGNÉ…")
}
val signeEnRelease = keystoreProperties.getProperty("storeFile") != null

android {
    signingConfigs {
        if (signeEnRelease) create("release") {
            storeFile = file(keystoreProperties.getProperty("storeFile"))
            storePassword = keystoreProperties.getProperty("storePassword")
            keyAlias = keystoreProperties.getProperty("keyAlias")
            keyPassword = keystoreProperties.getProperty("keyPassword")
            // minSdk 24 : v2/v3 suffisent, v1 (JAR) n'apporte plus rien.
            enableV1Signing = false
            enableV2Signing = true
            enableV3Signing = true
        }
    }
    buildTypes {
        getByName("release") {
            if (signeEnRelease) signingConfig = signingConfigs.getByName("release")
            …
        }
    }
}
```

**Le repli est délibéré** : sans `keystore.properties`, la compilation ne casse pas, elle
avertit et retombe sur l'APK non signé. Un poste qui ne fait que `cargo tauri android dev`
n'a aucune raison de détenir la clé de production.

**Créer le magasin de clés** (une fois, et pas dans le dépôt) :

```bash
keytool -genkeypair -v -keystore "$HOME/.gastinel/gws-release.jks" -alias gws -keyalg RSA -keysize 4096 -validity 10000
```

Puis `src-tauri/gen/android/keystore.properties` :

```properties
storeFile=C:/Users/<vous>/.gastinel/gws-release.jks
storePassword=…
keyAlias=gws
keyPassword=…
```

Les chemins s'écrivent en **barres obliques** : un `.properties` Java traite l'antislash
comme un caractère d'échappement, et un chemin Windows brut y serait tronqué.

⚠️ **Cette clé ne se perd pas.** Android refuse de mettre à jour une application installée
si la nouvelle version porte une autre signature — avec, précisément, le même message
« Application non installée ». Le magasin vit donc **hors de `src-tauri/gen/`**, qui est
régénéré par `tauri android init` et ignoré par git en entier. Le `.gitignore` du projet
refuse en plus `*.jks`, `*.keystore` et `keystore.properties` où qu'ils soient posés.

⚠️ **`gen/android/app/build.gradle.kts` est un fichier généré.** Ce bloc y est perdu au
prochain `tauri android init`, exactement comme le `MainActivity.kt` du bouton retour
(§ prompt 16). Les deux sont à recoller à partir de ce README après une régénération.

**Sur le téléphone du père, à la première installation avec la nouvelle clé** : si une
version debug (ou une tentative antérieure) est encore présente, **la désinstaller
d'abord**. Elle est signée par la clé de débogage du SDK, et Android refusera de la
remplacer par une application signée autrement.

#### La preuve, dans les deux sens

| Vérification | Résultat |
|---|---|
| L'APK de release porte maintenant une signature | `apksigner verify` → `Verifies` ; v2 **true**, v3 **true**, v1 false (attendu, `minSdk 24`) ; signataire `CN=Gastinel Weather Studio, …, C=FR`, RSA 4096 |
| Le nom de fichier change tout seul | `app-universal-release-**unsigned**.apk` → `app-universal-release.apk` |
| Il porte bien les quatre architectures | `arm64-v8a`, `armeabi-v7a`, `x86`, `x86_64` ; `aapt2 dump badging` → `native-code: 'arm64-v8a' 'armeabi-v7a' 'x86' 'x86_64'`, `minSdkVersion:'24'` |
| Il s'installe | `adb install app-universal-release.apk` → **Success** (émulateur Pixel 10) |
| Il démarre et rend l'écran attendu | ProGuard actif, aucun `FATAL EXCEPTION` ; accueil coupé en deux à l'écran |
| **Contre-épreuve** : le même APK privé de sa signature | `adb install` → **`INSTALL_PARSE_FAILED_NO_CERTIFICATES: Failed to collect certificates`** — c'est ce que l'installateur graphique affiche sous la forme « Application non installée » |
| Le conflit de signature est réel, lui aussi | l'APK debug (clé `CN=Android Debug`) a dû être **désinstallé** avant de poser le release : deux clés différentes pour le même `applicationId` ne cohabitent pas |

⚠️ **Ce qui n'a pas pu être vérifié ici** : l'installation sur un **Android 12 réel**. Le
seul système disponible sur ce poste est l'image API 37 de l'émulateur Pixel 10, et aucun
téléphone n'était branché. Le mécanisme démontré ci-dessus ne dépend d'aucune version
d'Android — un APK sans certificat est refusé partout, depuis toujours — mais la
confirmation finale reste **à faire sur le téléphone du père** : désinstaller ce qui y est
déjà, puis installer `app-universal-release.apk`.

### Version d'Android supportée

**Android 7.0 Nougat (API 24) au minimum**, sans maximum. Ce plancher est celui déclaré par
`minSdk = 24`, à deux endroits qui doivent rester d'accord :

| Fichier | Ligne |
|---|---|
| `src-tauri/gen/android/app/build.gradle.kts` | `minSdk = 24` (l'application) |
| `src-tauri/plugins/tauri-plugin-usbserial/android/build.gradle.kts` | `minSdk = 24` (le greffon USB) |

Aucune dépendance ne le remonte : `androidx.appcompat` 1.7.1 et
`androidx.lifecycle:lifecycle-process` 2.10.0 demandent 21, `usb-serial-for-android` 3.9.0
moins encore. Le chiffre est donc un choix du projet, pas une contrainte subie — et il
laisse **Android 12 (API 31), le téléphone visé, sept versions au-dessus du plancher**.

L'APK universel embarque les quatre architectures (`arm64-v8a`, `armeabi-v7a`, `x86`,
`x86_64`) : il n'y a pas de variante à choisir selon l'appareil.

#### ⚠️ Le piège de `--target` : un APK mono-architecture au nom identique

```bash
cargo tauri android build --apk --target x86_64   # ⚠️ POUR L'ÉMULATEUR SEULEMENT
cargo tauri android build --apk                   # ✅ les quatre architectures
```

Ces deux commandes écrivent au **même endroit et sous le même nom** —
`app/build/outputs/apk/universal/release/app-universal-release.apk` — alors que la première
ne contient qu'un seul `lib/<abi>/libgws_lib.so`. Le mot « universal » du chemin vient du
nom de la variante Gradle, pas du contenu ; il ment quand `--target` est passé.

Un APK x86_64 posé sur un téléphone (tous en ARM) échoue à l'installation sur… **le même
message « Application non installée »** que l'APK non signé. Deux causes distinctes, un seul
message : d'où l'intérêt de vérifier le contenu plutôt que de faire confiance au nom.

```bash
unzip -l app-universal-release.apk | grep 'lib/'
```

Quatre lignes attendues. Une seule → l'APK a été construit avec `--target`, il faut le
refaire sans.

#### Poser l'APK sur un téléphone, la marche à suivre

Les deux causes de « Application non installée » se cumulent, et se traitent dans cet ordre :

1. **Vérifier l'APK avant de l'envoyer** — les quatre `lib/` ci-dessus, et
   `apksigner verify` qui répond `Verifies`.
2. **Désinstaller ce qui est déjà sur le téléphone.** Toute version antérieure (debug, ou
   release d'une autre clé) bloque la nouvelle : `INSTALL_FAILED_UPDATE_INCOMPATIBLE`,
   affiché lui aussi comme « Application non installée ». Réglages → Applications →
   Gastinel Weather Studio → Désinstaller. ⚠️ Les données de l'application partent avec.
3. **Autoriser l'installation depuis la source** (navigateur, gestionnaire de fichiers,
   Drive…) : Android demande cette autorisation par application source, une seule fois.

#### ⚠️ Une quatrième cause, trouvée sur l'appareil réel : le stockage plein

Sur le téléphone effectivement utilisé — un **Samsung Galaxy A21s (SM-A217F), Android 12
réel, API 31, `arm64-v8a`** — aucune des trois causes ci-dessus n'était en jeu : l'app n'y
avait jamais été installée, l'APK était le bon (quatre architectures, signé), transféré par
câble USB.

`adb install` en direct sur l'appareil, branché à cet ordinateur, a donné le code exact —
ce que l'écran du téléphone ne montre jamais, se contentant du même « Application non
installée » pour toutes les causes :

```
Failure [INSTALL_FAILED_INSUFFICIENT_STORAGE]
```

`adb shell df /data` : **512 Mo libres sur 23 Go, 98 % utilisé**. L'APK fait 40 Mo, mais
l'installation a besoin de place en plus pour l'extraire — pas seulement pour le fichier
lui-même.

**Le message générique d'Android ne distingue aucune des quatre causes** (signature absente,
mauvaise architecture, conflit de clé, stockage insuffisant) : la seule façon fiable de
savoir laquelle est en cause est `adb install` depuis un ordinateur, avec le téléphone
branché en USB et le débogage activé — le code retourné (`INSTALL_PARSE_FAILED_NO_CERTIFICATES`,
`INSTALL_FAILED_NO_MATCHING_ABIS`, `INSTALL_FAILED_UPDATE_INCOMPATIBLE`,
`INSTALL_FAILED_INSUFFICIENT_STORAGE`…) dit sans ambiguïté laquelle.

**Correction : libérer de l'espace sur le téléphone** avant de retenter — au moins 1 à 2 Go
de marge, pas seulement les 40 Mo de l'APK.

**Confirmé résolu** : espace libéré sur le même Galaxy A21s (512 Mo → 1,9 Go, 98 % → 92 %
utilisé), `adb install` en direct depuis cet ordinateur → **Success**, application lancée
sur l'appareil → processus actif, aucun `FATAL EXCEPTION`. C'est la première fois que ce
prompt tourne sur un **Android 12 physique et non un émulateur** — la voie RC-5 par
USB-OTG (§ 4quater) reste, elle, à vérifier avec le boîtier réellement branché.

### Appels HTTP sur Android — racines de certificats embarquées

Les trois appels réseau de l'application — Infoclimat, Groq, et les tuiles `data.geopf.fr` —
échouaient tous sur Android, et pas pour la raison qu'on imagine : **la poignée de main TLS
ne se faisait pas**. `reqwest` 0.13 vérifie les certificats avec `rustls-platform-verifier`,
qui sur Android a besoin d'un pont JNI vers le `TrustManager` de la plateforme, monté côté
Java au démarrage. Sans ce pont, aucune requête HTTPS n'aboutit. Les deux messages
« non supporté sur Android pour le moment » de l'écran cachaient donc un problème de
transport, pas une fonctionnalité manquante.

**Décision : embarquer le magasin de racines de Mozilla dans le binaire** (`webpki-roots`),
et fournir à `reqwest` une `ClientConfig` rustls toute faite, plutôt que de monter le pont
JNI (Gradle, ProGuard, initialisation Java) qu'aurait exigé `rustls-platform-verifier`.

**Le compromis, en clair** : les racines de confiance ne suivent plus les mises à jour du
système Android, elles suivent celles de **l'application**. Une autorité de certification
révoquée le reste jusqu'au prochain build et à la prochaine installation de l'APK. C'est un
recul de sécurité réel, accepté ici parce que l'application est familiale, fermée, et
n'appelle que trois domaines fixes et stables (`www.infoclimat.fr`, `api.groq.com`,
`data.geopf.fr`) — pas un navigateur qui va n'importe où. **La contrepartie pratique :
rebâtir l'APK au moins une fois par an suffit à garder le magasin à jour**, `webpki-roots`
suivant les publications de Mozilla.

Trois points de mise en œuvre valent d'être notés :

* **Rien ne change sur desktop.** Windows continue d'utiliser SChannel via `native-tls`, et
  les autres systèmes le magasin système. Toute la configuration est sous
  `cfg(target_os = "android")`, dans `http_client.rs`.
* **Un seul endroit.** Les trois clients passaient chacun par leur propre
  `reqwest::blocking::Client::builder()`. Ils partagent désormais
  `http_client::client_builder()`. Ce n'est pas cosmétique : un quatrième appel qui
  oublierait la configuration TLS échouerait **sur mobile seulement**, donc au pire endroit
  pour s'en apercevoir.
* **L'ALPN est posé à la main.** Quand on lui fournit une `ClientConfig`, `reqwest` ne le
  renseigne plus lui-même. Le client est compilé sans `http2` : `http/1.1`, et rien d'autre.

#### Les clés d'API sur Android

Rétablir le TLS ne suffisait pas : `Clee.API/` est cherché « à la racine du projet », une
notion qui n'existe pas sur un téléphone. `current_dir()` y vaut `/` et `current_exe()`
pointe dans `/data/app/…` — deux endroits où l'on ne peut rien déposer. Infoclimat et Groq
auraient donc échoué une étape plus loin.

`api_keys::set_extra_root()` déclare au démarrage une racine supplémentaire, fouillée en
premier : le **répertoire de données de l'application**, celui-là même où vit déjà la base
SQLite. Sur desktop, cela n'enlève rien — les racines historiques restent fouillées ensuite.

Pour poser une clé sur un appareil de test (application en mode debug) :

```bash
adb push Clee.API/API.Groq.env /data/local/tmp/API.Groq.env
```

```bash
adb shell "run-as fr.gastinel.weatherstudio sh -c 'mkdir -p Clee.API; cat /data/local/tmp/API.Groq.env > Clee.API/API.Groq.env'"
```

⚠️ **La clé Infoclimat est liée à l'IP publique** (§ 5). Un téléphone en 4G sort par une IP
différente de celle du poste fixe : la clé y sera refusée avec `{"errors":["Wrong IP address"]}`
tant qu'elle n'aura pas été regénérée depuis ce réseau. Ce n'est pas un défaut de la voie
Android — c'est la même limitation qu'au § 5, simplement plus visible sur mobile.

#### Ce qui a été vérifié sur l'émulateur

Appels réels, faits depuis l'application installée sur un AVD Pixel, en invoquant les
commandes Tauri de la WebView (débogage distant `chrome://inspect`) :

| Service | Résultat |
|---|---|
| `data.geopf.fr` (tuile d'orthophoto) | **16 469 octets reçus en 1,7 s** |
| `api.groq.com` (agent IA) | **réponse complète, 874 tokens, 1,8 s** |
| `www.infoclimat.fr` | **poignée de main TLS et aller-retour HTTP réussis** ; l'API refuse la clé avec `Wrong IP address` — **le poste fixe reçoit exactement la même réponse au même instant**, c'est donc la clé qu'il faut regénérer, pas le transport qui est en cause |

### Où sont les données

La base est créée automatiquement au premier lancement dans le répertoire de données de
l'application :

```
%APPDATA%\fr.gastinel.weatherstudio\gastinel-weather-studio.sqlite3
```

Le chemin exact est aussi affiché dans la console au démarrage (`[gws] base de données : …`).
Pour repartir de zéro, il suffit de supprimer ce fichier.

---

## 2. Carte du projet

```
Gastinel Weather Studio/
├── src/                      Frontend statique (aucun build)
│   ├── index.html            Barre supérieure, grille 2×2, modales
│   ├── styles.css            ★ DA (Inter, bleu #1e4d8b, chiffres tabulaires, animations)
│   ├── main.js               Logique UI, sélection, pagination + appels aux commandes
│   ├── typewriter.js         ★ Machine à écrire — PARTAGÉE accueil + agent IA
│   ├── home_scripts.js       ★ Scripts de l'écran d'accueil — JEU D'EXEMPLE À REMPLACER
│   ├── map.js                ★ Carte Leaflet — FONDS IGN + CLÉ TOP25 à remplacer ici
│   ├── rc5.js                ★ Page du logger RC-5 — graphique, horloge, EFFACEMENT
│   ├── vendor/leaflet/       Leaflet vendorisé (la CSP interdit les CDN)
│   └── fonts/                Inter vendorisée en local (app utilisable hors-ligne)
│
├── src-tauri/
│   ├── tauri.conf.json       Fenêtre, CSP, bundle, icônes
│   ├── capabilities/         Permissions Tauri (seul `dialog:allow-save` est ouvert au JS)
│   ├── plugins/
│   │   └── tauri-plugin-usbserial/   ★ RC-5 par USB-OTG sur Android — NON TESTÉ SUR MATÉRIEL
│   │       ├── src/lib.rs            Façade Rust : ouvrir / écrire / lire / fermer
│   │       └── android/…/PortSerie.kt  ★ Enveloppe du pilote `usb-serial-for-android`
│   │       └── android/…/UsbSerialPlugin.kt  UsbManager, autorisation runtime, sessions
│   ├── icons/                Icônes générées (dégradé bleu + cuvette)
│   └── src/
│       ├── main.rs           Point d'entrée (coquille)
│       ├── lib.rs            Assemblage Tauri, ouverture de la base
│       ├── model.rs          ★ Entités + RÈGLE catégorie/taille + tests
│       ├── db.rs             ★ Schéma SQLite, migrations, requêtes + tests
│       ├── stats.rs          ★ Min/moy/max quotidiens, amplitudes… + tests
│       ├── parsing.rs        ★ Parsing + DÉTECTION DE L'ORDRE JOUR/MOIS + tests
│       ├── elitech_xlsx.rs   ★ Parseur du FORMAT RÉEL d'ElitechLog (xlsx) + tests
│       ├── elitech_import.rs ★ Parseur tabulaire — hypothèse, aucun fichier réel + tests
│       ├── anomaly.rs        ★ Sauts de température anormalement rapides + tests
│       ├── romma_import.rs   ★ Parseur des archives ROMMA (archives_*.csv) + tests
│       ├── infoclimat.rs     ★ Parseur Infoclimat, JSON ET CSV — fichier ET API + tests
│       ├── infoclimat_client.rs ★ Client HTTP — CLÉ, TRANCHES 7 J, ERREUR D'IP + tests
│       ├── http_client.rs    ★ Fabrique COMMUNE des clients HTTP — ET LE TLS D'ANDROID + tests
│       ├── tiles.rs          ★ Cache disque des tuiles, usage hors-ligne + tests
│       ├── export.rs         ★ Export CSV / TXT / HTML, ET l'export d'un logger lu + tests
│       ├── elitech_serial.rs ★ Protocole RC-5 — DEUX TRANSPORTS (série, USB-OTG) + tests
│       ├── elitech_watch.rs  ★ Surveillance du branchement, commune aux deux cibles + tests
│       ├── crossing.rs       ★ Croisement de données : fenêtre, agrégats, courbes + tests
│       ├── api_keys.rs       ★ Lecture des clés de Clee.API/ — COMMUNE AUX DEUX + tests
│       ├── groq.rs           ★ Agent IA — SYSTEM PROMPT MÉTIER, QUOTA 8000 TPM + tests
│       ├── manual_import.rs  ★ Import CSV à colonnes mappées (secours) + tests
│       ├── manual_entry.rs   ★ SAISIE MANUELLE sur site — aucun fichier, aucun appareil + tests
│       ├── commands.rs       Commandes exposées au frontend
│       └── error.rs          Erreur unique sérialisée vers l'UI
│   └── tests/
│       ├── import_bout_en_bout.rs     ★ Intégration : exemple SYNTHÉTIQUE → base → stats
│       ├── infoclimat_moucherolle.rs  ★ Intégration : export RÉEL 9,78 Mo → base → stats
│       ├── rc5_direct.rs             ★ Intégration : trames série RÉELLES du RC-5 (§ 4bis)
│       └── sources_reelles.rs         ★ Intégration : les TROIS fichiers RÉELS (§ 4, 5, 5bis)
│
├── docs/samples/             4 exemples synthétiques + 1 export Elitech RÉEL + 2 extraits réels
└── Clee.API/                 HORS DÉPÔT : clés Infoclimat ET Groq + Moucherolle.json
```

Les tests unitaires vivent à côté du code qu'ils testent (`mod tests` en bas de chaque
fichier) ; `src-tauri/tests/` contient les deux tests d'intégration, qui partent de
**vrais fichiers** et déroulent toute la chaîne. Ils servent aussi de garde-fou : si un
échantillon est modifié sans que le parseur suive, ils tombent.

`infoclimat_moucherolle.rs` s'appuie sur `Clee.API/Moucherolle.json`, qui est **hors
dépôt**. Sur un poste où le fichier n'a pas été copié, ces tests **s'abstiennent** au lieu
d'échouer, en écrivant une ligne visible avec `cargo test -- --nocapture` : un test rouge
parce qu'un fichier de 10 Mo n'est pas là ne renseignerait sur rien.

`sources_reelles.rs` applique la même convention aux trois sources ajoutées, avec un
partage assumé entre ce qui est committé et ce qui ne l'est pas :

| Fichier | Dans le dépôt | Test |
|---|---|---|
| `elitech_rc5_reel.xls` (254 Ko) | **en entier** | tourne partout |
| `infoclimat_export_extrait.csv` | **extrait** (120 mesures) | tourne partout |
| `romma_archives_extrait.csv` | **extrait** (150 lignes + un bloc de `NULL`) | tourne partout |
| `export_infoclimat.csv` (17 Mo) | non | s'abstient si absent |
| `archives_Lans - Les Allières_….csv` (38 Mo) | non | s'abstient si absent |

Les fichiers complets sont cherchés d'abord dans `docs/samples/`, puis dans
`%USERPROFILE%\Downloads` — là où ils ont été téléchargés sur le poste de développement.

---

## 3. Modèle de données

### Entités

| Entité | Table | Points notables |
|---|---|---|
| `Project` | `projects` | espace de travail géographique libre |
| `Point` | `points` | site prospecté ou station de référence |
| `LoggerSeries` | `logger_series` | **une série par logger physique / par import** |
| `Measurement` | `measurements` | mesure unitaire |

### La règle catégorie / taille

| Catégorie | Taille autorisée |
|---|---|
| `Cat1` — fond de vallée / plateau plat ouvert | **aucune** |
| `Cat2` — dépression ouverte | `Gr` ou aucune (**jamais** `Pt`) |
| `Cat3` — dépression fermée | `Gr` ou `Pt` (**jamais** vide) |
| `Cat4` — cuvette de haute montagne | `Gr` ou `Pt` (**jamais** vide) |

Elle est appliquée **à trois niveaux**, volontairement :

1. `model::validate_category_size` — seul juge, message d'erreur métier lisible ;
2. une contrainte `CHECK` SQL sur `points` — aucune ligne invalide ne peut exister, même
   écrite par un client SQLite tiers ;
3. l'UI construit ses listes déroulantes à partir de la commande `category_options`, donc
   **aucune règle n'est recopiée en JavaScript**.

Tests : `model.rs` couvre les 12 combinaisons (4 catégories × 3 tailles) de façon
exhaustive et générée, plus un test nommé par cas ; `db.rs` vérifie que la contrainte SQL
rattrape une insertion brute qui contournerait le Rust.

> ⚠️ **Piège SQL à connaître avant de toucher à cette contrainte.** En SQL,
> `size IN ('Gr','Pt')` ne vaut pas `FALSE` quand `size` est `NULL` : il vaut `NULL`. Or
> SQLite considère qu'une contrainte `CHECK` qui s'évalue à `NULL` est **satisfaite**.
> Écrite naïvement, la contrainte laissait donc passer un point `Cat3`/`Cat4` *sans
> taille* — exactement le cas qu'elle devait interdire. D'où le `size IS NOT NULL`
> explicite dans `db.rs`. Le test `la_contrainte_sql_double_la_validation_rust` existe
> précisément pour ça : c'est lui qui a révélé le trou.

### Le temps : la contrainte n°1

Le RC-5 n'a ni GPS ni NTP : son horloge dérive et peut être réglée en heure locale.

* **Tous les instants sont stockés en `INTEGER` = secondes epoch UTC.** Aucune date en
  texte, aucun fuseau attaché : un entier ne peut pas être « de l'heure locale par accident ».
* `Measurement.raw_timestamp` conserve l'horodatage **brut** du fichier, réinterprété tel
  quel comme de l'UTC, sans aucune conversion.
* `Measurement.timestamp_utc` est l'horodatage **corrigé**.
* Invariant tenu partout : `timestamp_utc = raw_timestamp + LoggerSeries.clock_offset_seconds`.
* `clock_offset_seconds` agrège deux corrections : la conversion de fuseau (logger réglé
  en UTC+1 → `-3600`) **et** la dérive constatée (logger en avance de 45 s → `-45`).
  L'UI d'import les saisit séparément et calcule la somme.
* `raw_timestamp_was_utc` est purement documentaire (traçabilité / debug).
* Comme le brut est conservé, la commande `recalibrate_series` permet de **réajuster une
  série après coup** : les horodatages sont recalculés depuis le brut, jamais depuis la
  valeur déjà corrigée — les corrections ne s'empilent donc pas.
* **Une seule opération réécrit `raw_timestamp`** : la permutation jour/mois
  (`swap_series_date_order`, cf. §4). C'est l'exception à l'immuabilité du brut, et elle est
  légitime — dans ce cas précis, c'est le brut lui-même qui porte la date fausse. Toute
  autre correction doit repartir du brut sans le modifier.
* **Les instants sont tronqués à la seconde dès leur création** (`model::now_utc`, utilisé
  partout au lieu de `Utc::now()`). La base ne stockant que des secondes, un objet gardé en
  mémoire avec des nanosecondes ne serait plus égal à celui relu juste après : un projet
  créé à `12:00:00,750` ressortait à `12:00:00` au listing suivant. On tronque donc à la
  source, pour que « ce que l'application manipule » et « ce que la base contient » soient
  toujours la même valeur.

### Séries : aucune fusion automatique

Un logger principal et son logger de secours produisent **deux `LoggerSeries` distinctes**
sur le même point, avec un `role` (`Primary` / `Backup`) **toujours choisi par
l'utilisateur**. Rien n'est déduit, rien n'est fusionné. `Db::measurements_for_point`
existe pour un aperçu mais est explicitement documentée comme inadaptée aux statistiques
(double comptage).

Cette règle a un revers, corrigé depuis : rien n'empêchait de créer une deuxième série
**complète** à partir d'un fichier déjà importé. L'application le signale désormais et
demande confirmation — voir § 4, « Ré-importer le même fichier ». Elle ne fusionne toujours
rien pour autant : elle demande, elle ne décide pas.

### Migrations de schéma

La version vit dans `PRAGMA user_version` et s'incrémente dans `Db::migrate`. Une base
ancienne est migrée à l'ouverture, sans intervention.

| Version | Apport |
|---|---|
| **v1** | schéma initial : projets, points, séries, mesures |
| **v2** | `date_order`, `date_format_uncertain` sur `logger_series` (§ 4) |
| **v3** | colonnes `rssi_dbm` / `battery`, licence et traçabilité d'API, `points.infoclimat_station_id`, table `app_settings`, et **`UNIQUE(series_id, timestamp_utc)`** (§ 5) |
| **v4** | `logger_series.content_fingerprint` : l'empreinte de contenu qui reconnaît un ré-import (§ 4) |
| **v5** | six colonnes météo facultatives sur `measurements`, `points.romma_station_id`, et **la reconstruction de `logger_series`** pour élargir son `CHECK` à `RommaImport` (§ 5bis) |

`SCHEMA_V1` n'est jamais modifié rétroactivement : sur une base neuve, les migrations sont
rejouées dans l'ordre. Déclarer une colonne des deux côtés ferait échouer l'`ALTER`. Chaque
migration a son test, qui fabrique une base de la version précédente et vérifie qu'elle
monte sans rien perdre.

**La v5 est la première à reconstruire une table**, et c'est la seule opération de
migration réellement risquée du projet. SQLite ne sait pas modifier une contrainte `CHECK`
(ni `ALTER CONSTRAINT`, ni `DROP CONSTRAINT`) : élargir `logger_series.source_type` à
`'RommaImport'` impose la procédure documentée — table jumelle, recopie, `DROP`,
renommage. Deux précautions, sans lesquelles la manœuvre détruit la base :

* **les clés étrangères sont coupées le temps de l'opération.** `measurements.series_id`
  référence `logger_series` avec `ON DELETE CASCADE` : supprimer la table sous les pieds de
  500 000 mesures les effacerait toutes. Elles sont remises en service **quoi qu'il
  arrive**, et un contrôle refuse d'ouvrir la base si une mesure s'est retrouvée orpheline ;
* **les index sont recréés après le renommage** — `DROP TABLE` les emporte avec elle.

Le test `une_base_v4_migre_vers_la_v5_sans_rien_perdre` part d'une v4 peuplée (deux séries
de sources différentes, tous les champs renseignés) et vérifie les trois choses qui
pourraient mal tourner : aucune mesure perdue, aucun champ de série perdu à la recopie, et
les six nouvelles colonnes **à `NULL` partout** sur l'existant. Un second test
(`une_base_v1_traverse_toutes_les_migrations`) enchaîne v1 → v5 d'une traite, parce que ce
chemin n'est pas la somme des chemins pris un à un.

### Statistiques (`stats.rs`)

`period_stats()` calcule en un passage : min / moyenne / max **quotidiens**, amplitude
quotidienne, minimum et maximum extrêmes, moyenne des minimums quotidiens (= la
« moyenne annuelle » quand la période fournie est une année), amplitude moyenne et
amplitude maximale, avec les dates associées.

Le découpage se fait sur le **jour calendaire UTC**, conformément à la spec. Une nuit
d'inversion se lit donc à cheval sur deux journées ; `daily_stats_with_day_offset()` est
déjà en place pour basculer plus tard sur une « journée météo » (18 h → 18 h) quand on
comparera les points entre eux.

**Ces fonctions ne prennent plus seulement des `&[Measurement]`.** Elles sont génériques sur
un trait `TemperatureSample` — un instant, une température, rien d'autre — implémenté par
`Measurement` et par `TempSample`, un couple de 16 octets sans allocation. Rien n'a été
dupliqué : il n'existe toujours qu'une seule implémentation de « moyenne des minimums
quotidiens ». La raison est mesurable : une `Measurement` porte quatorze champs dont un
`series_id: String`, soit une allocation par ligne ; sur l'archive ROMMA (476 912 mesures),
en reconstruire une par ligne pour n'en lire que deux colonnes coûte un demi-million
d'allocations — à chaque changement de sélection dans le bloc de croisement. Un test
(`les_deux_entrees_donnent_les_memes_statistiques`) vérifie que les deux chemins donnent des
`PeriodStats` identiques champ pour champ.

`aggregate_curve()` complète l'ensemble : elle réduit une série à des **seaux** (horaires,
quotidiens, hebdomadaires ou mensuels) portant chacun moyenne, minimum, maximum et nombre de
mesures. C'est ce qui rend le graphique du § 11 traçable ; la stratégie de granularité y est
détaillée.

**Les six champs météo de la v5 ne sont pas agrégés**, et c'est volontaire. `stats.rs`
n'en fournit qu'un inventaire, `weather_coverage()` : combien de mesures portent chaque
champ. Deux raisons de ne pas aller plus loin pour l'instant :

* les agrégats de température ont un sens métier arrêté (moyenne des minimums quotidiens,
  amplitude) ; ceux du vent et de la pluie restent à définir — et une « direction de vent
  moyenne » calculée comme une moyenne arithmétique de degrés n'a aucun sens ;
* ces grandeurs ne sont renseignées que par ROMMA. Sur un point qui porte aussi un logger
  Elitech, une moyenne porterait sur une fraction des mesures sans que rien ne le dise.

L'inventaire est là pour ça : savoir ce qu'on a **avant** d'en tirer une moyenne. Sur neuf
ans d'archive ROMMA, plusieurs capteurs ont été ajoutés en cours de route.

---

## 4. Import de fichiers logger

### Le format réel d'ElitechLog : un `.xls` qui est un **xlsx**

> Cette section remplace l'ancienne « hypothèses à corriger dès réception d'un vrai
> export ». Le vrai export est arrivé, il a été ouvert, et **l'hypothèse était fausse** :
> ElitechLog ne produit pas un TXT tabulé mais un classeur.

Fichier de référence : `docs/samples/elitech_rc5_reel.xls` (254 Ko), produit par
**ElitechLogWin 8.0.5** le 19 août 2026 — RC-5 n° `EFE205004333`, **10 307 mesures** du
1er mai au 16 août 2026, cadence 15 minutes. Il est **dans le dépôt** : c'est la référence
du parseur, et il est assez petit pour y rester.

**Le piège central : l'extension ment.** Le fichier s'appelle `.xls` mais ses premiers
octets sont `50 4B 03 04` — la signature d'une archive zip, donc du **OOXML (xlsx)**,
produit par la bibliothèque .NET NPOI. Ce n'est *pas* le vieux format binaire CFB
(`D0 CF 11 E0`). Toute détection de format par l'extension échoue ici : le parseur teste la
**signature**, jamais le nom.

Le classeur contient trois feuilles, nommées **en français** (la langue de l'interface au
moment de l'export) :

| # | Nom observé | Contenu | Utilisée ? |
|---|---|---|---|
| 1 | `Résumé` | modèle, n° de série, type de capteur, intervalle, seuils d'alarme, bornes | **oui** (métadonnées) |
| 2 | `Tableau de données` | le tableau `No.` / `Temps` / `Température°C` | **oui** (mesures) |
| 3 | `Graph` | une image JPEG de la courbe | non |

Les feuilles sont repérées **par leur contenu**, jamais par leur nom ni par leur rang : un
export dans une autre langue reste lisible. La feuille de données est celle dont une ligne
porte à la fois un intitulé d'horodatage et un intitulé de température, **et** dont une
ligne suivante contient un horodatage réellement lisible — sans cette dernière condition,
la feuille de résumé passerait pour un tableau (elle contient des cellules comme `34,1°C`).

**Quatre pièges de contenu**, tous constatés sur le fichier réel :

1. **Toutes les cellules sont du texte** (`t="inlineStr"`), y compris les nombres et les
   dates. Aucun type natif Excel à convertir — juste du texte à lire correctement.
2. **La virgule est le séparateur décimal** : `"22,2"`, jamais `"22.2"`.
3. **`sharedStrings.xml` est vide** (`count="0"`) : un lecteur qui supposerait des chaînes
   partagées lirait des cellules vides.
4. La feuille de résumé est un **tableau à deux colonnes de paires** (`A`/`B` puis `D`/`E`,
   `C` en gouttière) et non une liste de lignes `clé: valeur`.

Un cinquième piège vaut d'être nommé à part, parce qu'il porte sur les intitulés
eux-mêmes : la colonne d'horodatage s'appelle **`Temps`**, qui contient la sous-chaîne
`temp`. Sans garde, elle serait prise pour la colonne de température et le parseur lirait
des dates comme des degrés. `parsing::looks_like_temperature_header` écarte donc
explicitement tout intitulé commençant par `temps` ; deux tests verrouillent le couple.

**Le temps : ISO, mais naïf.** La colonne `Temps` vaut `2026-05-01 14:07:31` — année,
mois, jour, dans cet ordre. Il n'y a donc **aucune ambiguïté jour/mois** et le
`DateOrderScanner` est court-circuité (`DateOrderEvidence::iso`). Mais « non ambigu » n'est
pas « en UTC » : l'horodatage reste celui de l'horloge interne du RC-5, **sans fuseau**.
Contrairement à Infoclimat et à ROMMA, cette source garde donc tout le mécanisme de
`clock_offset_seconds`, et l'onglet d'import affiche bien le réglage d'horloge.

### Les formats `.esf` et `.elt` ne sont pas lus — décision actée

ElitechLog exporte trois formats en même temps. Un seul est exploitable :

| Format | Décision | Motif |
|---|---|---|
| `.xls` (= xlsx) | **supporté** | OOXML documenté, ouvrable dans n'importe quel tableur, contenu complet |
| `.esf` | **non supporté** | chiffré, aucune spécification publique. Rien à quoi s'accrocher : ni en-tête lisible, ni structure devinable. |
| `.elt` | **non supporté** | sérialisation binaire .NET propriétaire. Approchable par rétro-ingénierie, mais non documentée et **liée à la version d'ElitechLogWin** : un parseur écrit contre la 8.0.5 casserait sans prévenir à la mise à jour suivante, en silence, sur des données de terrain irremplaçables. |

**Ne pas rouvrir cette décision sans élément nouveau** — une spécification publiée, ou un
export xlsx qui s'avérerait incomplet. Le xlsx contient déjà tout ce que l'application
utilise (mesures, n° de série, intervalle, type de capteur). Si un export ne suit plus
cette structure, le **mode manuel** reste le filet de sécurité.

### Sauts de température : signalés, jamais écartés

L'analyse du fichier réel a relevé **+3,8 °C en 15 minutes** dans la dernière heure de la
série. Aucune situation météo ne fait cela à cet endroit et à cette saison : ce qui
explique la courbe, c'est le **retrait du logger** — sorti de son abri, il finit en poche
ou en véhicule pendant que l'enregistrement continue. Les dernières mesures ne décrivent
alors plus le site.

`anomaly.rs` détecte ces sauts. Seuil par défaut : **plus de 3 °C entre deux mesures
séparées de moins de 20 minutes**. Les deux bornes vont ensemble — 3 °C en une heure est
banal, 3 °C en un quart d'heure ne l'est pas.

**Aucune mesure n'est écartée.** Le saut apparaît dans l'analyse préalable *et* dans le
bilan d'import, avec ses deux instants, ses deux valeurs et sa ligne source ; l'utilisateur
décide. C'est délibéré : un vrai phénomène *peut* produire un saut brutal — rafale de
foehn, rupture d'inversion nocturne — et ce sont précisément les épisodes qui intéressent
la chasse aux TAF. Les effacer d'office reviendrait à gommer le sujet d'étude.

La détection tourne pour **toutes** les sources de fichier (Elitech xlsx, Elitech
tabulaire, import manuel), pas seulement le xlsx : un logger manipulé est un logger
manipulé, quel que soit le format de son export. Elle ne tourne pas pour Infoclimat ni pour
ROMMA — une station fixe ne se met pas en poche, et le signal n'y serait que du bruit.

### Le parseur tabulaire est conservé — pour quoi, exactement

`elitech_import.rs` (le parseur texte délimité) **n'est pas supprimé**. Mais son statut a
changé : il ne décrit plus « le format ElitechLog », il décrit un format hypothétique dont
**nous n'avons aucun exemplaire réel**. Il reste utile à deux titres :

* si une autre version d'ElitechLogWin, ou un autre modèle de logger, exportait du texte
  tabulé ;
* c'est lui qui porte le `DateOrderScanner`, seul mécanisme du projet capable de trancher
  un `05/01/2024`.

Autrement dit : **la détection d'ordre jour/mois ne sert plus aucune source réelle
connue.**

| Source | Format d'horodatage | Scanner utile ? |
|---|---|---|
| Elitech, export xlsx réel | `2026-05-01 14:07:31` | non (ISO) |
| Infoclimat (JSON et CSV) | `2026-06-24 20:10:00` | non (ISO) |
| ROMMA `archives_*.csv` | epoch Unix entier | non |
| Elitech tabulaire, hypothétique | `05/01/2024` | **oui** |
| Import manuel (fichier quelconque) | libre | **oui** |

Le mécanisme est donc conservé pour ces deux derniers cas, et documenté comme tel — pas
retiré, mais plus au centre. La section « Ordre jour/mois » ci-dessous reste valable pour
eux.

### Ordre jour/mois : détecter, marquer, corriger

`05/01/2024` est-il le 5 janvier ou le 1er mai ? Se tromper décale **toute une série de
plusieurs mois sans lever la moindre erreur**. C'était le seul endroit du socle où une
donnée pouvait être fausse en silence. Le parseur ne suppose donc plus : il **regarde le
fichier entier avant de parser** (`DateOrderScanner`, deux passes dans `parse_rows` et
`parse_manual`), et applique un ordre unique à toutes les lignes — jamais ligne par ligne,
ce qui décalerait une partie des mesures seulement.

| Ce que contient le fichier | Ordre appliqué | Série marquée douteuse ? |
|---|---|---|
| Au moins une date avec un champ > 12, toutes dans le même sens | l'ordre **prouvé** | non |
| Des preuves dans les deux sens | la **majorité** (égalité → repli) | **oui** + avertissement d'import |
| Aucune preuve, mais des dates ambiguës | le repli `DEFAULT_DATE_ORDER` | **oui** |
| Uniquement des dates ISO (`2024-01-05`), ou aucune date | repli, sans effet | non |

Le dernier cas est le piège : un fichier entièrement ISO n'est **pas** incertain — l'ordre
jour/mois n'a aucune prise sur lui. Le drapeau ne se lève que s'il existe une ambiguïté
*réelle*.

`AMBIGUOUS_DATE_IS_DAY_FIRST` existe toujours mais n'est plus qu'un **repli** : elle ne
sert que quand le fichier ne prouve rien, et dans ce cas la série est marquée.

**Contradictions : pourquoi on ne refuse pas l'import.** Un fichier mêlant `13/01` et
`01/13` est incohérent, mais le rejeter n'offrirait aucune issue à l'utilisateur (l'import
manuel partage le même parseur). On applique donc la majorité, on lève le drapeau, et on
émet un avertissement pointant la première ligne contradictoire. Conséquence assumée : une
telle série **ne pourra pas être permutée** après coup (certaines de ses dates ont un jour
> 12, la permutation n'y est pas définie) ; l'erreur le dit, et le vrai remède est de
scinder le fichier. On ne fabrique pas une date fausse pour se débarrasser du problème.

**Corriger après coup, sans réimport.** Une série marquée affiche un bandeau dans le détail
du point, avec deux actions : *confirmer* la lecture, ou *inverser* jour et mois.
L'inversion (`Db::swap_series_date_order`) permute les dates de **toutes** les mesures :

* elle réécrit `raw_timestamp` — c'est le brut lui-même qui porte la date fausse ;
* puis re-dérive `timestamp_utc = raw_timestamp + clock_offset_seconds`, avec la même
  instruction SQL que la recalibration d'horloge, pour que l'invariant n'existe qu'à un
  seul endroit textuel ;
* **rien n'est écrit tant que toutes les lignes n'ont pas été validées** : une permutation
  partielle serait pire que pas de permutation du tout.

L'opération est involutive (la relancer revient à l'état initial). Elle reste accessible
sur une série non marquée, mais derrière une confirmation : appliquée à une série correcte,
elle rendrait ses dates fausses sans que rien ne le signale.

### Comportement des parseurs

Commun aux deux formats Elitech :

* **Tolérant** : une ligne malformée produit un avertissement (numéro de ligne, contenu
  tronqué, raison) et est ignorée. Seul un fichier totalement inexploitable produit une
  erreur.
* **Analyse à blanc** : le bouton « Analyser le fichier » montre ce que le parseur a
  compris **avant** toute écriture en base — et, pour un classeur, la liste des sauts de
  température suspects, pour qu'ils se découvrent avant l'import et non après.
* **Le format est reconnu au contenu.** Le frontend teste les quatre octets de signature :
  `PK 03 04` route vers le parseur xlsx, tout le reste vers le parseur tabulaire. Il n'y a
  rien à choisir, et un fichier renommé ne trompe personne.

Propre au parseur tabulaire (formats hypothétiques et import manuel) :

* **Deux niveaux de détection** : par intitulés de colonnes ; à défaut (ou si le premier
  essai ne produit aucune mesure), par le contenu — premier champ qui ressemble à une
  date, premier champ numérique qui suit. Le schéma est déterminé une fois puis appliqué
  à tout le fichier : jamais ligne par ligne, ce qui décalerait silencieusement les colonnes.

Propre au parseur xlsx :

* **La feuille de données est trouvée par son contenu**, pas par son nom ni son rang, avec
  une vérification supplémentaire : une ligne suivante doit contenir un horodatage
  réellement lisible. Sans cela, la feuille de résumé passerait pour un tableau.
* **La feuille « Graph » (une image) est ignorée** sans que cela soit une erreur.

### Mode manuel (secours)

Si l'auto-détection se trompe, l'onglet « Manuel (colonnes mappées) » affiche un aperçu
tabulé numéroté et laisse choisir explicitement : séparateur, lignes à ignorer, présence
d'en-tête, colonne date, colonne heure (si séparée), colonne température, colonne
humidité. Aucune devinette — juste une suggestion pré-remplie que l'utilisateur peut
écraser.

Il **reste disponible** maintenant que le format réel est connu, et pour une raison
précise : si un futur export d'ElitechLog ne suivait plus la structure du classeur décrite
ci-dessus, c'est le seul chemin qui ne dépende d'aucune hypothèse. C'est aussi celui d'un
classeur réenregistré en CSV depuis Excel, ou de n'importe quel fichier tabulaire d'une
autre provenance.

### Saisie manuelle sur site (aucun fichier, aucun appareil)

> ⚠️ **À ne pas confondre avec le « mode manuel » ci-dessus.** Celui-là lit un *fichier*
> dont on désigne les colonnes ; celui-ci n'a aucun fichier — on tape la valeur.

Le cas d'usage est concret : relevé manuscrit sur le terrain, pas de logger disponible ce
jour-là. *« Le 14 janvier, entre 6 h et 9 h, il faisait −4,2 °C. »* C'est une donnée de plus
sur un point qui en a peu, et il serait dommage de la perdre — à condition de ne jamais la
faire passer pour un échantillonnage continu.

L'onglet « Saisie manuelle (sans fichier) » de l'écran d'import demande cinq choses : le
point, la plage de jours, la plage horaire dans la journée, la nature de la valeur
(moyenne, minimum ou maximum) et le fuseau dans lequel les heures ont été lues.

**Une valeur par jour, jamais une valeur pour tous.** Quand plusieurs jours sont
sélectionnés, l'interface affiche une ligne de saisie par jour. Une case unique appliquée
à toute la plage n'existe pas dans cet écran, et le backend refuse une saisie incomplète
en **nommant le jour** qui manque (`ManualEntryError::MissingValue`). Recopier la même
température sur cinq jours fabriquerait quatre relevés qui n'ont jamais été faits.

#### Une plage horaire dans un modèle qui ne connaît que des instants

« Moyenne 6 h–9 h » ne décrit pas ce qu'il faisait à 6 h : ça décrit trois heures. Le
modèle, lui, ne stocke que des instants. Deux décisions assumées :

* la mesure est datée du **milieu de la plage** (7 h 30 pour 6 h–9 h) — le seul point qui
  ne privilégie aucun des deux bords, et qui place la valeur au centre de ce qu'elle
  résume ;
* la nature de la valeur et les bornes de la plage vivent dans le **libellé de la série**
  (`Saisie manuelle — minimum sur 06:00–09:00 (3 jours)`), qui sert aussi de
  `raw_file_reference`. Ajouter deux colonnes à `measurements` pour une source sur sept
  aurait coûté du `NULL` partout ailleurs.

`sample_interval_seconds` porte la **durée de la plage** (10 800 s pour 6 h–9 h). Ce n'est
pas une cadence d'échantillonnage — il n'y en a pas — mais c'est l'étendue que couvre
chaque valeur, et c'est ce qu'il faut savoir pour relire la série sans se tromper.

L'heure tapée est **naïve**, comme celle d'un logger : c'est l'heure lue sur une montre. La
conversion en UTC passe par `clock_offset_seconds`, exactement au même endroit que pour un
fichier Elitech (`persist_import`). La contrainte n°1 du projet vaut donc ici sans
traitement particulier.

#### `SourceType::ManualEntry` — pourquoi une variante de plus

`Manual` existait déjà, mais il désigne le fichier à colonnes mappées : la donnée reste une
lecture de logger. Une valeur tapée à la main n'est pas de même nature, et c'est
précisément ce qu'il faut pouvoir distinguer plus tard. Réutiliser `Manual` aurait rendu
les deux cas indiscernables en base, sans espoir de les démêler après coup. Les libellés
affichés ont été clarifiés en conséquence :

| Variante | Libellé affiché | Ce que c'est |
|---|---|---|
| `Manual` | Fichier à colonnes mappées | un fichier, colonnes désignées à la main |
| `ManualEntry` | Saisie manuelle sur site | une valeur tapée, sans fichier ni appareil |

`SourceType::is_manual_entry()` est le point unique où la distinction se lit, et **trois
endroits qui restituent des chiffres s'en servent** :

* **l'export détaillé** (CSV et TXT) gagne une colonne `Origine` — mais *seulement* si
  l'export contient au moins une saisie manuelle. Un export 100 % logger garde exactement
  les colonnes qu'il avait, et un test le vérifie dans les deux sens ;
* **le résumé** (texte et HTML) affiche la mention sous le nom du point, **avant** les
  chiffres qu'elle qualifie — une note de bas de page se lirait après avoir conclu.
  `CrossPointStats::manual_entry_series` porte le compte, par série et non par mesure :
  c'est la nature de la source qu'il décrit ;
* **l'agent IA** reçoit la même information dans son contexte, pour qu'il ne présente pas
  une moyenne recopiée d'un carnet comme un résultat de mesure.

#### Migration v6

Élargir `CHECK (source_type IN …)` à `'ManualEntry'` demande, sous SQLite, de reconstruire
`logger_series` — la même manœuvre qu'en v5, avec les deux mêmes précautions : clés
étrangères désactivées le temps de l'opération (sans quoi le `DROP TABLE` emporterait
**toutes les mesures** par cascade) et index recréés après le renommage. Le test
`une_base_v5_migre_vers_la_v6_et_accepte_la_saisie_manuelle` part d'une base v5 peuplée et
vérifie les trois choses qui peuvent mal tourner : les mesures survivent, les séries
gardent tous leurs champs, et la contrainte ne s'est pas relâchée pour autant.

### Fichiers d'exemple

`docs/samples/` contient désormais deux familles de fichiers, qu'il ne faut pas confondre.

**Fichiers réels** — ce sont eux la référence :

* `elitech_rc5_reel.xls` — l'export ElitechLog complet (254 Ko, 10 307 mesures). C'est le
  fichier contre lequel `elitech_xlsx.rs` est écrit.
* `rc5_direct/devinfo.bin`, `rc5_direct/header.bin`, `rc5_direct/pages.bin` — les **trames
  série brutes** du même logger, capturées sur le port COM (20 Ko en tout). Ce sont les
  octets tels qu'ils sont sortis du câble, et c'est le décodeur de production qui les
  rejoue : voir § 4bis.
* `infoclimat_export_extrait.csv` — les 120 premières mesures de l'export CSV réel de
  `STATIC0213`, lignes `#` et ligne d'unités comprises.
* `romma_archives_extrait.csv` — les 150 premières lignes de l'archive réelle de Lans -
  Les Allières, **plus** un bloc de lignes à `Temp = NULL` ajouté exprès pour exercer le
  chemin d'écartement.

**Fichiers synthétiques** — fabriqués à la main pour exercer le parseur tabulaire et le
mécanisme d'ordre jour/mois. Aucun n'est un export réel ; les tests qui s'appuient dessus
valident la cohérence interne du code, **pas** un format constaté :

* `docs/samples/elitech_hypothese.txt` — format supposé, tabulé, avec une ligne
  volontairement cassée (`--`) pour voir le mécanisme d'avertissement. Dates **ISO**, donc
  non ambiguës : aucun drapeau de date ne doit se lever dessus.
* `docs/samples/generique_manuel.csv` — point-virgule, virgule décimale, date/heure
  séparées, deux lignes de métadonnées à ignorer : à ouvrir avec l'onglet manuel.
  **Entièrement ambigu** (toutes ses dates sont en `05/01`, `06/01`) : il lève le drapeau,
  et c'est voulu — un test le verrouille pour que personne ne « corrige » le fixture.
* `docs/samples/dates_jour_certain.csv` — contient `13/01/2024` : le fichier prouve son
  ordre, aucun drapeau.
* `docs/samples/dates_ambigues.csv` — aucune preuve possible : drapeau levé, puis
  permutation testée de bout en bout.

### Ré-importer le même fichier : détecté, jamais silencieux

**Ce qui change pour vous.** Si vous importez un contenu déjà enregistré sur ce point,
l'application **n'écrit rien** et affiche une boîte qui nomme l'import précédent — sa date,
son fichier, sa période, son nombre de mesures. Deux issues, toutes deux explicites :

* **Annuler l'import** (proposé par défaut, et ce que fait la touche Échap) ;
* **Importer quand même**, qui crée bel et bien une seconde série. Le bilan porte alors un
  bandeau d'avertissement, seule trace durable que le doublon est assumé.

Le cas légitime existe — deux loggers physiques ayant produit, par coïncidence, les mêmes
valeurs aux mêmes instants — donc rien n'est bloqué. Mais il est assez improbable pour
mériter un clic.

**Ce qui déclenche l'alerte, et ce qui ne la déclenche pas.** Le socle calcule une
empreinte SHA-256 du lot importé (`fingerprint.rs`), stockée sur la série
(`content_fingerprint`, migration v4). Elle couvre le nombre de mesures, les deux bornes de
la période **et** chaque valeur.

| Situation | Alerte ? | Pourquoi |
|---|---|---|
| Le même fichier rejoué à l'identique | **oui** | même empreinte |
| Le même fichier avec un autre réglage de fuseau | **oui** | l'empreinte porte sur l'horodatage **brut** ; corriger un décalage se fait sans réimport, avec « Recalibrer » |
| Un export du logger repris plus tard (l'ancien + la suite) | non | plus de mesures, autre borne de fin |
| Infoclimat redemandé sur une période qui en recouvre une autre | non | bornes et nombre de mesures différents |
| Le même contenu sur un **autre point** | non | la recherche est bornée au point |
| Une série importée avant la v4 | non | son empreinte est `NULL` : elle ne peut ni accuser ni être accusée |

La troisième et la quatrième ligne sont la vraie contrainte de conception : le mode
« récupération automatique » d'Infoclimat redemande volontiers sept jours dont cinq sont
déjà connus, et **ce complément incrémental doit passer sans rien demander**. Il passe par
construction, et trois tests le verrouillent — un sur l'empreinte elle-même, deux sur le
chemin complet d'import.

L'empreinte porte sur les **mesures lues**, pas sur les octets du fichier : un export
réenregistré (fins de ligne ou encodage changés d'un poste à l'autre) reste reconnu, et une
réponse d'API — qui n'a pas de fichier — est couverte de la même façon.

---

## 4bis. Lecture directe du RC-5 par le port série

> **Complément de l'import xlsx, jamais son remplaçant.** Les deux voies coexistent : le
> fichier reste le filet de sécurité si la lecture directe échoue sur un cas particulier.

Brancher le RC-5 en USB, et l'application le lit — sans ElitechLog, sans fichier
intermédiaire. Elle détecte le branchement toute seule et propose l'import.

### Le matériel, et pourquoi le VID:PID ne suffit pas

Le RC-5 se présente comme un adaptateur **USB-série CH340** (QinHeng Electronics,
`1a86:7523`), donc un `COMx` sous Windows. Mais la même puce équipe la plupart des clones
Arduino : **un CH340 détecté n'est pas un RC-5**. L'identification se fait donc en deux
temps — filtre sur le VID:PID, puis une commande `DevInfo` dont la réponse doit annoncer
`model_no = 50`. Un appareil qui ne répond pas, ou qui répond autre chose, ressort en
erreur, jamais en supposition.

Deux précautions accompagnent cet envoi à un appareil inconnu :

* **DTR est maintenu bas à l'ouverture** (`dtr_on_open(false)`). Le RC-5 l'ignore, mais un
  DTR levé **redémarre une carte Arduino** — et la détection interroge tout CH340 qui
  apparaît. Sans cette précaution, brancher le logger redémarrerait l'Arduino d'à côté.
* Le délai d'attente est réduit à **1,5 s** pour cette seule commande : un appareil muet ne
  doit pas figer la surveillance pendant cinq secondes.

### Le protocole

Non documenté officiellement. Point de départ : la bibliothèque Python non officielle
[`elitech-datareader`](https://github.com/civic/elitech-datareader), rétro-ingénierée pour
les RC-4 / RC-4HC / RC-5. **Chaque hypothèse a été revérifiée contre le logger réel** avant
d'être écrite — et deux d'entre elles ont demandé des corrections que la lib ne documente
pas (voir plus bas).

Liaison : **115200 bauds, 8N1**, sans contrôle de flux. Chaque échange est une requête de
5 octets suivie d'une réponse de **longueur connue à l'avance** — rien ne délimite ni
n'annonce la fin d'une trame, il faut lire exactement le nombre d'octets attendu.

| Requête | Octets | Réponse |
|---|---|---|
| `Init` | `CC 00 0A 00 D6` | 3 octets (`55 A5 FA` sur le RC-5 testé) |
| `DevInfo` | `CC 00 06 00 D2` | 160 octets |
| `DataHeader` | `33 <station> 01 00 <somme>` | 11 octets |
| `DataBody` | `33 <station> 02 <page> <somme>` | `2 × mesures + 2` octets |

La `<somme>` est la somme des octets précédents modulo 256. Les réponses la portent en
dernier octet : **elle est vérifiée ici**, alors que la bibliothèque Python l'ignore. C'est
le seul garde-fou contre une trame abîmée par un mauvais câble, et il ne coûte rien.

**Une seule commande d'écriture existe, et elle est arrivée après coup** : l'effacement de
la mémoire, ajouté avec la page dédiée du § 4ter. Tout le reste — réglage de l'horloge,
numéro de station, seuils d'alarme — reste hors de portée : une application de relevé n'a
aucune raison de reprogrammer l'appareil qu'elle lit. C'est ElitechLog qui fait cela.

L'effacement fait exception parce qu'il ne configure rien : il remet le boîtier à zéro pour
la campagne suivante (`CMD_PARAM_PUT = [0x05, 0x00]`, trame de 25 octets validée sur matériel réel
au prompt 21, avec passage immédiat à `rec_count = 0` et `work_status = NotStarted`), et c'est le
geste qui suit naturellement un relevé. Les précautions qui l'entourent sont détaillées au § 4ter.

### Deux comportements découverts sur le matériel, absents de la référence

**1. Une commande `0xCC` clôt la session.** Après un `DevInfo`, plus rien ne répond sur le
port tant qu'il n'a pas été refermé et rouvert. Le tableau des essais :

| Enchaînement dans une même session | Résultat |
|---|---|
| `DevInfo`, `DataHeader` | l'en-tête **ne répond pas** |
| `DevInfo`, `DevInfo` | le second **ne répond pas** |
| `DataHeader`, `DataHeader`, `DevInfo` | tout passe |
| `DataHeader`, `DataBody` × 21 | tout passe |

La règle n'est donc ni « une commande par session », ni une affaire de familles : les
requêtes `0x33` s'enchaînent sans limite, mais une requête `0xCC` est **la dernière que la
session servira**. La bibliothèque Python respecte la contrainte sans la nommer — elle
ouvre et referme le port autour de *chaque* commande. La lecture complète n'a donc besoin
que de **deux ouvertures** : la fiche d'identité, puis l'en-tête et les 21 pages d'affilée.

**2. Il faut un temps de repos entre deux ouvertures.** Rouvrir immédiatement après avoir
fermé laisse le logger muet. Mesure sur le matériel (trois séries d'essais) : **0 ms en
échec systématique, 50 ms en succès systématique** ; le temps de réponse, lui, ne bouge pas
(186 ms dans tous les cas). La valeur retenue est celle de la bibliothèque de référence —
**500 ms** — et non le minimum observé sur un seul appareil. L'attente est ciblée : elle ne
se déclenche que s'il reste du temps à courir depuis la dernière fermeture, donc jamais à
la première ouverture.

### Les horodatages n'existent pas dans l'appareil

**C'est le point le plus important de ce chantier.** Le RC-5 ne stocke *que des
températures* — deux octets par mesure, entier signé gros-boutiste en dixièmes de degré,
rien d'autre. L'instant de chaque mesure est **reconstruit** :

```text
instant(n) = start_time + n × intervalle          (n commençant à 0)
```

`start_time` et l'intervalle viennent de l'appareil. **ElitechLog fait exactement le même
calcul** — la validation croisée le prouve. Conséquence à garder en tête : la
reconstruction suppose une cadence parfaitement régulière du premier au dernier
enregistrement. Un logger arrêté puis relancé sans effacement la mettrait en défaut — **et
l'export xlsx serait faux de la même façon**. Ce n'est donc pas un défaut de cette voie,
mais une limite du matériel ; l'application la signale quand l'état du logger le laisse
craindre (« enregistrement en cours », désaccord entre les deux instants de départ
annoncés).

Le signe des températures mérite d'être nommé, parce que c'est la matière même du projet :
les valeurs négatives arrivent en complément à deux **sur 16 bits** (`FF F5` pour
−1,1 °C). Une lecture qui ne traiterait que l'octet de poids faible lirait +24,5 °C. C'est
exactement l'erreur qu'a commise la première sonde d'exploration, et que la validation
croisée a rattrapée — 1 919 mesures divergentes, toutes de +25,6 °C (= 256/10). Un test
unitaire fige désormais le cas.

### ✅ Validation croisée : concordance totale

Le même logger physique, lu par les deux voies, mesure par mesure :

| | Lecture directe | Export xlsx |
|---|---|---|
| Mesures | **10 307** | **10 307** |
| Première | `2026-05-01 14:07:31` · 22,2 °C | `2026-05-01 14:07:31` · 22,2 °C |
| Dernière | `2026-08-16 22:37:31` · 23,7 °C | `2026-08-16 22:37:31` · 23,7 °C |
| Écarts d'horodatage | **0** | — |
| Écarts de température | **0** | — |
| Min / moy / max | −9,9 / 12,9 / 34,1 °C | −9,9 / 12,8 / 34,1 °C (résumé du fichier) |
| Empreinte de contenu | identique | identique |
| Anomalies détectées | identiques | identiques |

L'écart entre `12,9` et `12,8` est l'arrondi de la moyenne affichée par ElitechLog dans sa
feuille de résumé ; les mesures, elles, sont égales une à une.

**Une divergence réelle, documentée et non masquée : le numéro de série.** Le champ
`dev_num` du protocole fait **10 octets** et contient `E205004333`, là où le xlsx annonce
`EFE205004333` — deux caractères de plus. Le champ est trop court pour le numéro entier :
ElitechLog connaît un préfixe que l'appareil ne transmet pas. L'application le dit à
l'écran et invite à saisir le numéro complet, qui fait alors foi ; `serials_match()`
reconnaît par ailleurs qu'un numéro est le suffixe de l'autre. Un test fige l'écart, pour
qu'un futur firmware qui le corrigerait se fasse remarquer.

### Le test permanent, et les fixtures

`src-tauri/tests/rc5_direct.rs` rejoue une **capture des trames série réelles**, committée
dans `docs/samples/rc5_direct/` :

| Fichier | Contenu |
|---|---|
| `devinfo.bin` | 160 octets — la réponse `DevInfo` telle qu'elle est sortie du câble |
| `header.bin` | 11 octets — la réponse `DataHeader` |
| `pages.bin` | 20 656 octets — les 21 réponses `DataBody` concaténées |

Ce sont les octets bruts, pas une reconstruction, et c'est **le décodeur de production** qui
les relit : le test ne teste pas une copie du code. Sept tests tournent partout et sans
matériel — dont la comparaison mesure par mesure avec le xlsx, l'égalité des empreintes et
l'égalité des anomalies.

Un huitième test est **ignoré par défaut** et demande le logger branché :

```text
cargo test --test rc5_direct -- --ignored --nocapture
```

Il refait toute la chaîne sur le matériel — énumération, identification, 21 échanges réels —
et compare le résultat au xlsx. Dernier passage : « Validation croisée sur matériel réel :
10307 mesures identiques à l'export xlsx. », en 9,7 s.

Un outil de diagnostic pas-à-pas complète le dispositif, pour répondre à la seule question
utile quand rien n'est détecté — *où ça s'arrête* :

```text
cargo run --example rc5_diagnostic          # cherche tout seul
cargo run --example rc5_diagnostic -- COM8  # port imposé
```

### Détection du branchement

Un fil de fond sonde les ports **toutes les 2,5 s** et émet l'événement Tauri
`rc5-detecte` ; le frontend en fait une notification persistante — persistante et non un
toast, parce qu'elle porte une action.

Windows sait notifier l'arrivée d'un périphérique (`WM_DEVICECHANGE`), mais il y faut une
fenêtre native et sa boucle de messages, que ni Tauri 2 ni `serialport` n'exposent. Écrire
un pont Win32 ajouterait du code spécifique à une plateforme pour gagner deux secondes de
latence. Le sondage l'emporte donc — **à condition de rester vraiment léger** :

* un cycle = **une énumération de ports**, rien d'autre. Aucun port ouvert, aucune trame
  émise ;
* le port n'est ouvert et interrogé **que** lorsqu'un CH340 vient d'apparaître, donc une
  fois par branchement, jamais en régime établi ;
* un port déjà examiné — identifié *ou* écarté — n'est plus réinterrogé tant qu'il reste
  branché.

En régime établi, le coût est celui d'un appel système toutes les 2,5 s.

**Le port occupé n'est pas un échec de détection.** Si ElitechLog est ouvert, il tient le
port : l'identification échoue en `PortBusy`, et c'est une information utile — un logger
*est* probablement là.

Le reconnaître demande une précaution. Sous Windows, `serialport` replie
`ERROR_ACCESS_DENIED`, `ERROR_FILE_NOT_FOUND` et `ERROR_PATH_NOT_FOUND` sur **un seul et
même** `ErrorKind::NoDevice` : « occupé » et « débranché » arrivent avec le même code. Le
texte de l'erreur, lui, est traduit par le système (« Accès refusé. »), et y chercher un
mot-clé casserait sur un poste en anglais. L'application tranche donc sur un fait
vérifiable et indépendant de la langue : **le port est-il encore énuméré ?** S'il l'est, il
existe et c'est l'accès qui est refusé ; sinon, il a réellement disparu. Sans ce tri — et
c'est ce qu'un essai avec un port réellement tenu par un autre programme a montré — un port
occupé s'annonçait « le logger a-t-il été débranché ? », soit exactement le mauvais
conseil. L'application l'affiche avec le message qui dit quoi faire (fermer
ElitechLog, icône de la zone de notification comprise) et propose **« Réessayer »**, qui
fait oublier au fil les ports déjà écartés. Sans ce bouton, il faudrait débrancher puis
rebrancher.

### Où arrivent les données

Exactement dans le même pipeline que les fichiers : `persist_import`, donc **même empreinte
de contenu, même détection de ré-import, même détection de saut de température** (le module
`anomaly` partagé, pas une seconde détection). Une seule chose distingue les deux voies en
base : le `SourceType` — **`ElitechDirect`** ici, `ElitechImport` pour un fichier. La
variante existait dans le modèle depuis le prompt 1 ; elle est enfin écrite.

Conséquence directe, et c'était une exigence : **relire en direct un logger déjà importé
par fichier est détecté comme ré-import**, pas dupliqué en silence. L'empreinte porte sur
les mesures, jamais sur le contenant — un test le vérifie sur les deux fixtures réelles.

`raw_file_reference` n'ayant pas de fichier à porter, le backend y inscrit
`Lecture directe RC-5 — COM8, n° E205004333` : la traçabilité de l'origine, dans le champ
prévu pour ça.

### L'horloge du logger, pré-remplie

`DevInfo` livre l'horloge interne de l'appareil au moment de la lecture. Comparée à celle du
PC, elle donne directement la dérive à corriger — l'application pré-remplit le champ
« Dérive » du réglage d'horloge, et le recalcule à chaque changement du menu de fuseau.

Le backend ne fournit volontairement que l'écart brut `horloge − UTC` : il ignore le fuseau
réglé sur le logger, et deviner un fuseau n'est pas son rôle. C'est le frontend, où ce choix
est disponible, qui transforme l'écart en dérive. Sur le logger de référence, l'écart relevé
était de −1 685 s, soit environ 28 minutes de retard — à confirmer par le fuseau réel avant
d'importer.

### Ce qui n'est pas supporté

* **Le RC-5+** : modèle différent, non couvert par la référence utilisée et non testé. Un
  `model_no` inattendu est **refusé explicitement**, avec un message qui renvoie vers
  l'export xlsx, plutôt que décodé au hasard. Le RC-4 (40) et le RC-4HC (42) sont dans le
  même cas — la structure des trames est proche, mais la taille de page diffère.
* **L'écriture** de configuration (horloge, numéro de station, seuils d'alarme) : voir plus
  haut. Seul l'effacement de la mémoire est implémenté, et dans un cadre décrit au § 4ter.

---

## 4ter. La page du logger RC-5

> Cette page existe désormais **sur les deux plateformes**. Le transport diffère — port
> série sur desktop, USB-OTG et `UsbManager` sur Android (§ 4quater) — mais les commandes
> appelées, le protocole et l'écran sont les mêmes. ⚠️ **La voie Android n'a pas encore été
> exercée sur du matériel** : lire le § 4quater avant de s'y fier.

Le § 4bis rendait le logger lisible depuis l'écran d'import. Cette page répond à une autre
question : on rentre du terrain, le boîtier dans la main, et on veut **voir ce qu'il
contient** avant de décider quoi que ce soit.

### Deux accès, tous deux conditionnels

Un bouton sur l'**écran d'accueil** d'un projet, et un second dans le bloc Stations. Les
deux sont **toujours visibles** et **désactivés tant qu'aucun RC-5 n'est détecté** :
éteints, ils disent « rien n'est branché », ce qui est une information utile ; les faire
disparaître obligerait à se demander où ils sont passés.

Ils s'activent **tout seuls** au branchement, sans relancer l'application. Il n'y a pas de
seconde détection côté frontend : l'état vient uniquement de l'événement `rc5-detecte`
émis par le fil de surveillance du § 4bis. Deux sources de vérité sur « un logger est-il
là ? » finiraient par diverger.

Le second accès n'est pas une redite. L'écran d'accueil s'efface au bout de sept secondes
(remplissage automatique du bouton par défaut) : sans le bouton du bloc Stations, la page
ne serait atteignable que pendant ce court instant. Le bouton d'accueil ne porte
d'ailleurs **pas** de remplissage automatique — un compte à rebours ne doit jamais mener
tout seul sur du matériel.

### Ce que la page contient

| Zone | Ce qui la sert réellement |
|---|---|
| Graphique de toute la mémoire | `read_rc5`, puis Chart.js vendorisé — le même composant que le bloc Croisement |
| Vérification de l'heure | `check_rc5_clock` → `elitech_serial::compare_clock` |
| Sauvegarder les données | `import_rc5` → `persist_import` — même empreinte SHA-256, même détection de ré-import |
| Exporter la lecture | `build_rc5_export` → `export::build_rc5_csv` / `build_rc5_txt` |
| Arrêter l'enregistrement | `stop_rc5` → `elitech_serial::stop_recording` |
| Réinitialiser | `reset_rc5` → `elitech_serial::clear_memory` — arrête d'abord le logger s'il enregistre |
| Informations de l'appareil (en bas) | les champs de `DevInfo`, tels que `elitech_serial` les a décodés |

Rien n'y est réimplémenté. La lecture est la commande de l'onglet d'import, l'écriture en
base est le pipeline commun, et le graphique est la bibliothèque déjà vendorisée.

### L'export : ce qu'il porte, et pourquoi ce n'est pas l'export ordinaire

Il exporte **la lecture affichée**, pas la base — et c'est le seul export de l'application
qui n'exige ni projet ouvert ni point de rattachement. C'est le cas courant au retour du
terrain : on veut le fichier tout de suite, on décidera plus tard s'il devient une série.
Le `reading_id` est exigé par le backend, donc l'export porte exactement ce que la courbe
montre — jamais une relecture qui pourrait différer si le logger enregistre encore.

Il ne passe **pas** par `export::build_csv`, pour une raison qui n'est pas d'organisation :
`ExportRow` date ses mesures en `DateTime<Utc>`, alors que le RC-5 ne connaît que l'heure
brute de son horloge interne, sans fuseau et éventuellement dérivante. Les faire traverser
un champ typé UTC les présenterait comme de l'UTC. L'en-tête de colonne s'appelle donc
`Horodatage_brut_logger`, et un test fige l'invariant : **le mot `UTC` ne doit apparaître
nulle part dans ce format**. Un export du même logger *après* enregistrement en base passe,
lui, par le chemin ordinaire et porte de l'UTC véritable ; les deux fichiers ne racontent
pas la même chose, et ils ne le prétendent pas.

### Les informations de l'appareil, et les deux champs qui n'existent pas

Le bloc du bas ne sert à décider de rien — mais il sert à comprendre un chiffre surprenant
plus haut : une température décalée quand une calibration interne est active, un logger
qui enregistre encore pendant qu'on le lit, des seuils d'alarme qui expliquent un
comportement du boîtier. Tout y vient de `DevInfo`, sans aucun calcul.

**Ni la version de firmware, ni le niveau de batterie n'y figurent : le protocole série du
RC-5 ne les transmet pas.** L'écran le dit en toutes lettres plutôt que de laisser chercher
— une information absente de l'appareil et une information oubliée par l'écran ne se
distinguent pas autrement.

### Sur téléphone : la même page, trois ajustements

Aucune version mobile séparée, et `rc5.js` ne contient aucun `if (mobile)`. Tout tient dans
une requête média à 640 px, et chacun des trois ajustements répond à un problème constaté à
411 px de large :

1. **L'en-tête passe en colonne**, titre puis boutons. Sur une seule ligne, le titre se
   réduisait à deux mots coupés.
2. **Le graphique descend à 260 px** de haut. Pas plus bas : c'est le sujet de la page, et
   l'aplatir davantage écrase précisément les minimums nocturnes qu'on vient y chercher.
3. **Les cibles tactiles passent à 44 px** — le minimum recommandé. En dessous, un bouton se
   rate au doigt, et l'un d'eux efface la mémoire du logger.

Un défaut découvert à cette occasion touchait **toutes** les modales de l'application, pas
seulement celle-ci : `.dialog-form` avait une largeur **fixe** de 440 px dans un `dialog`
plafonné à `92vw`. Sur un téléphone, le formulaire débordait et le texte de la confirmation
d'effacement — celui qui explique ce qui va être détruit — se coupait en plein milieu. Un
`max-width: 100%` le corrige partout.

### Sur téléphone : l'accueil coupé en deux, et un écran par logger

La page reste la même ; **c'est le chemin qui y mène qui change.** Sur poste fixe, le RC-5
s'atteint depuis une page à onglets (§ 4quinquies), depuis la notification de branchement,
et depuis l'onglet « USB direct » de la modale d'import. Sur téléphone, **il ne s'atteint
plus que d'un seul endroit** : la moitié basse de l'écran d'accueil.

**Ce qui a été retiré du mobile**, et non caché :

| Point d'entrée | Sur téléphone |
|---|---|
| Bandeau pleine largeur « Gastinel Weather Elitech » en bas de la liste des projets | **Remplacé** par la zone de détection — ce n'est pas un ajout, c'est le même emplacement qui cesse d'être un bouton pour devenir l'écran |
| Notification persistante de branchement (« Importer ») | **Supprimée** — `showRc5Notice` sort immédiatement (`main.js`) |
| Onglet « USB direct (RC-5) » de la modale d'import | **Retiré du document** au démarrage, pas masqué : un onglet caché mais atteignable serait un chemin de plus |

Les deux derniers vivaient dans le contexte d'un **projet ouvert** — ils menaient à
l'import d'une série vers un point. Or on branche un boîtier au retour du terrain, avant de
savoir à quel projet ses données iront : c'est le raisonnement qui avait déjà sorti la page
Elitech du projet sur poste fixe, poussé jusqu'au bout ici.

**La mise en page : deux zones fixes, 50 % / 50 %.**

```
┌──────────────────────────────┐
│  Projets            [+]      │  ← moitié haute : la liste, inchangée,
│  ┌────────────────────────┐  │    avec son propre défilement interne
│  │ Vercors nord      12 … │  │
│  │ Combe Laval        4 … │  │
│  └────────────────────────┘  │
├──────────────────────────────┤  ← trait de séparation
│           ((( 🔍 )))         │  ← la loupe et ses ondes, à l'échelle
│   Deux loggers trouvés.      │
│  ┌────────────────────────┐  │
│  │ RC-5 n°21050123     ▸  │  │  ← un widget par logger, zone défilable
│  │ COM8 · 10 307 mesures  │  │    délimitée par le bas de l'écran
│  └────────────────────────┘  │
│  ┌────────────────────────┐  │
│  │ RC-5 n°21050456     ▸  │  │
└──┴────────────────────────┴──┘
```

Deux hauteurs **imposées** (`height: 50%`, `flex: none`) et non deux zones qui se
partagent la place selon leur contenu : c'est ce qui garantit qu'une liste de trente
projets ne repousse pas la détection hors de l'écran, et qu'une liste de six loggers
n'empiète pas sur les projets. Chaque zone défile chez elle. La zone des widgets est bornée
**du bas de la loupe au bord physique de l'écran**, `env(safe-area-inset-bottom)` compris —
le dernier widget doit pouvoir être touché, pas seulement vu.

**L'animation est celle du poste fixe, pas une seconde.** La zone basse réutilise les
classes `.radar`, `.radar-onde`, `.radar-loupe`, `.detect-card` de l'onglet « Détection du
RC-5 » : mêmes ondes concentriques, même `@keyframes elitech-onde` (opacité 0,1 à
`scale(0.75)`, 0 au bord), mêmes cartes-onglets flottantes. **Seule l'échelle change** —
`width: min(150px, 40vw, 17vh)` au lieu de `min(420px, 70vw)`. Un second visuel aurait fait
deux applications au lieu d'une.

**La navigation est une pile, pas des onglets.** Toucher un widget ouvre un **écran plein**
dédié à ce logger. Il n'y a pas d'onglet pour passer à un autre : on remonte (bouton retour
Android du prompt 16, ou la flèche « ← Retour aux projets » de l'écran) et on redescend.
C'est le geste que le système fournit déjà, et une colonne d'onglets de 216 px prendrait la
moitié de la largeur d'un téléphone pour choisir entre deux appareils.

Une différence assumée avec le poste fixe : là-bas, la carte d'un logger **disparaît** de
la détection quand son onglet est ouvert (elle promet « ouvrir dans un nouvel onglet », et
l'onglet existe déjà). Ici, le widget reste : il *est* le seul chemin vers l'écran du
logger, et le faire disparaître le rendrait inatteignable.

**L'écran d'un logger n'a rien de neuf** : c'est le même `GwsRc5Panel`, cloné du même
`#tpl-rc5-panel`, avec l'export, l'effacement sous la même confirmation stricte, la
vérification de l'heure, le graphique et le bloc d'informations de l'appareil en bas. À
moins de 1 000 px, `.rc5-grid` passe déjà en colonne unique et l'ordre y devient celui qu'on
veut sur un téléphone : graphique, heure, sauvegarde, export, réinitialisation, puis
informations.

Le panneau **survit à un retour à l'accueil** : une lecture de mémoire prend une dizaine de
secondes de va-et-vient série, et la refaire à l'identique après un aller-retour serait une
punition.

| Fichier | Rôle |
|---|---|
| `src/mobile.js` | `GwsMobile` — la zone de détection de l'accueil, l'écran plein d'un logger, le retour |
| `src/styles.css`, bloc « Téléphone » | Les deux moitiés, l'échelle de la loupe, la zone défilable des widgets |
| `src/main.js` | Le branchement : mêmes dépendances que `GwsElitech`, même flux `rc5-detecte`, et les retraits ci-dessus |

#### Ce qui a été vérifié, et comment

Sur l'émulateur **Pixel 10 (API 37, x86_64)**, application réellement installée et lancée,
la WebView pilotée par le **Chrome DevTools Protocol** (`adb forward` vers
`webview_devtools_remote_<pid>`) : les loggers sont injectés par `GwsMobile.noteDetection`
avec la charge utile exacte de l'événement `rc5-detecte`, donc par le même chemin que la
surveillance réelle.

| Vérification | Mesure |
|---|---|
| La bascule mobile s'arme d'après le vrai `userAgent` | `GwsMobile.estActif()` → `true` sur la WebView Android, sans réglage |
| Le partage est exactement 50 / 50 | moitié haute **413,21 px**, moitié basse **413,21 px** (fenêtre de 923 px sous la barre) |
| La zone des widgets défile au lieu de déborder | 5 loggers : hauteur visible **188 px**, contenu **446 px**, bas de zone à **923,4 px** pour une fenêtre de **923 px** |
| Rien ne déborde en largeur | `documentElement.scrollWidth <= innerWidth` |
| Un widget ouvre l'écran du bon logger | clic sur le 1ᵉʳ → titre « RC-5 — n° 21050121 » ; `#view-projects` masqué |
| L'écran porte bien les six blocs attendus | « Données en mémoire », « Heure du logger », « Sauvegarder les données », « Exporter la lecture », « Réinitialiser le logger », « Informations de l'appareil » |
| Le retour Android remonte d'un cran | `window.__gwsAndroidBack()` → `true`, retour à la liste des projets |
| …et rend la main à la racine | second appel → `false` (Android quitte l'application) |
| On change de logger en repassant par l'accueil | clic sur le 2ᵉ widget → « RC-5 — n° 21050122 », sans onglet |
| La flèche « Retour aux projets » fait la même chose | même état après clic |
| La notification de branchement ne s'affiche plus | `#rc5-notice` reste masquée après une détection |
| L'onglet « USB direct (RC-5) » a disparu de l'import | `#dlg-import .tab[data-tab="rc5"]` absent du document |
| Le bandeau pleine largeur a disparu | `getComputedStyle(#btn-elitech).display` → `none` |
| Le panneau parle bien au vrai backend | avec un nom de port bidon, l'erreur remontée est celle du Rust : « Nom de port inattendu sur Android : … Attendu « usb:<numéro> » » |

⚠️ **Ce que l'émulateur ne peut pas montrer** : il n'a pas de contrôleur USB hôte. Aucune
lecture réelle d'un RC-5 n'a été exercée par ce chemin — seuls l'affichage, la navigation
et le câblage vers les commandes l'ont été. La voie matérielle reste celle validée au
prompt 15, dont ce prompt ne change pas une ligne.

#### L'export général n'a pas bougé

La restructuration de l'accueil ne touche que `#view-projects`. L'export ordinaire — celui
du **bloc Stations** de l'espace de travail, à ne pas confondre avec l'export d'une lecture
de logger — a été rejoué sur l'émulateur **après** les changements, par le chemin de
l'utilisateur et non par un appel interne : ouvrir un projet depuis la liste, « Explorer le
studio », page 2 du bloc Stations, cocher un point, « Exporter en CSV ».

| Vérification | Résultat |
|---|---|
| Les trois formats sont toujours proposés | « Exporter en CSV », « Exporter en TXT », « Exporter en HTML » |
| Les trois sections de l'écran sont intactes | « Points à exporter », « Période », « Contenu du texte (.txt) » |
| La garde de saisie répond | sans point coché : « Sélectionnez au moins un point à exporter. » |
| **Le sélecteur de documents Android s'ouvre** | `topResumedActivity=com.google.android.documentsui/…PickActivity`, `Intent { act=android.intent.action.CREATE_DOCUMENT … }` |

C'est le point 16 du prompt 14 rejoué à l'identique : rien n'a régressé.

#### ⚠️ Un test intermittent, découvert en passant — et qui n'est pas un test fragile

`commands::tests::un_recouvrement_partiel_infoclimat_ne_declenche_pas_l_alerte` a échoué
**deux fois sur environ vingt-cinq exécutions**, uniquement pendant qu'un build Gradle et
l'émulateur saturaient la machine ; zéro échec sur quinze exécutions à vide. Rien de ce
prompt ne le touche — aucun fichier Rust n'a été modifié.

La cause n'est pas le test, c'est `Db::find_infoclimat_series` :

* `imported_at` est stocké en **secondes entières** ;
* la recherche fait `ORDER BY s.imported_at DESC LIMIT 1`, sans départage ;
* le test crée **deux** séries Infoclimat sur le même point et la même station.

Machine rapide : les imports tombent dans la même seconde, l'égalité renvoie la série
d'origine, à laquelle il reste des jours à ajouter — le test passe. Machine chargée :
l'import intermédiaire franchit une frontière de seconde, sa série gagne le tri, elle
contient déjà tout, et `imported_count` vaut zéro.

Autrement dit : **deux séries de la même station importées dans la même seconde n'ont pas
d'ordre défini**, et « compléter la série existante » peut désigner l'une ou l'autre. À
traiter à part — ce prompt ne touche pas à la logique métier Rust.

`GwsMobile` et `GwsElitech` coexistent, et **un seul des deux est activé** : la bascule est
`/Android/i.test(navigator.userAgent)`, pas une largeur de fenêtre — une fenêtre de poste
fixe rétrécie n'est pas un téléphone, et faire changer l'accès au RC-5 de forme sur un
redimensionnement surprendrait plus que ça n'aiderait. (`localStorage["gws.forcer-mobile"]`
force la réponse ; aucun écran ne l'expose, c'est un outil de mise au point.)

### Le graphique : 16 000 mesures, 1 500 points, aucun extrême perdu

Un RC-5 plein contient 16 000 mesures. Les envoyer telles quelles à l'UI ferait un bon
mégaoctet de JSON et un graphique où chaque pixel porte huit points. Le backend agrège donc
par rang — les mesures sont régulièrement espacées par construction, un seau de *n* mesures
est aussi un seau de durée constante — jusqu'à `RC5_COURBE_MAX_POINTS` (1 500) points.

**Chaque seau garde son minimum et son maximum**, pas seulement sa moyenne. Sur ce projet,
c'est le minimum qui est le sujet : une courbe lissée qui le gomme serait pire qu'une
absence de courbe. L'infobulle affiche les trois, et un test vérifie qu'un minimum isolé
au milieu de 16 000 mesures survit à l'agrégation.

L'axe des abscisses porte l'**heure brute du logger**, non corrigée, et il est étiqueté
comme tel — jamais « UTC ». La page montre l'appareil tel qu'il est, horloge dérivante
comprise ; la correction n'intervient qu'à l'enregistrement, comme pour toutes les sources.

### L'horloge : deux écarts, et pourquoi ce n'est pas le même chiffre

`DevInfo` livre l'heure interne du logger. Comparée à celle du PC, elle donne **deux**
quantités qu'il ne faut pas confondre :

| Quantité | Formule | Ce que c'est |
|---|---|---|
| `offset_vs_utc_seconds` | `horloge − UTC` | un **fait**, calculable sans rien supposer |
| `drift_seconds` | `horloge − (UTC + fuseau)` | la **dérive** de l'appareil |

Un logger réglé sur UTC+1 et parfaitement à l'heure affiche une heure d'écart et une dérive
nulle. Dire « le logger avance d'une heure » serait faux — d'où le sélecteur de fuseau, et
le refus du backend de deviner ce fuseau lui-même, comme partout ailleurs dans ce projet.

Trois niveaux de gravité, décidés côté Rust parce qu'un seuil est une décision métier :

| Niveau | Écart (fuseau retiré) | Pourquoi ce seuil |
|---|---|---|
| `aligned` | ≤ 60 s | le RC-5 n'affiche pas les secondes et se règle à la minute |
| `notable` | ≤ 15 min | réel, mais à saisir à l'import, pas à craindre |
| `severe` | au-delà | un quart d'heure déplace un minimum nocturne d'un seau horaire à l'autre |

La dérive mesurée **pré-remplit** le champ de la zone d'enregistrement, juste en dessous :
c'est tout l'intérêt d'avoir les deux sur le même écran. Un test fige l'invariant qui les
relie — *heure brute + correction suggérée = UTC vrai* —, et un test matériel ignoré le
rejoue sur l'appareil réel.

### La réinitialisation — la seule commande d'écriture du projet

Elle efface la mémoire du **boîtier physique**. C'est irréversible côté appareil, et la
confusion qui coûterait le plus cher est traitée en premier : **les mesures déjà importées
restent en base**, intactes, consultables et exportables. Le texte de confirmation le dit,
le message de bilan le répète.

Trois gardes, chacune contre une erreur différente :

1. la modale distingue explicitement le boîtier de la base, et rappelle — s'il y a une
   lecture affichée non enregistrée — d'utiliser « Sauvegarder les données » d'abord ;
2. le bouton rouge ne s'active qu'après avoir recopié le mot **EFFACER** ;
3. le backend refuse la commande sans ce mot (`CONFIRMATION_REINITIALISATION`). Ce n'est
   pas une sécurité — le frontend est de confiance — c'est un garde d'appel : la commande
   la plus destructrice du projet ne doit pas être la plus facile à déclencher par
   distraction.

#### D'où vient chaque commande d'écriture — confirmé, ou à vérifier

Le protocole série du RC-5 n'a pas de spécification publique. Chaque octet écrit vient donc
soit d'une bibliothèque de référence, soit d'un sondage sur un appareil réel — et les deux
n'ont pas le même poids. Le tableau dit lequel, pour qu'on n'ait jamais à le redeviner :

| Commande | Trame | Source | Statut |
|---|---|---|---|
| Écriture de paramètres (= effacement) | `33 <station> 03 <corps DevInfo> <somme>` | `ParamPutRequest` de [`elitech-datareader`](https://github.com/civic/elitech-datareader) | **Confirmée par la référence.** Le découpage du bloc de paramètres, lui, ne l'est pas — d'où le renvoi de la fiche telle quelle (voir plus bas) |
| Arrêt de l'enregistrement | `33 <station> 08 00 <somme>` | **Sondage sur un RC-5 réel** (COM8, série E207102078) | **Non documentée par aucune source.** Vérifiée isolément sur le matériel : `work_status` passe à `Stopped`, `rec_count`, cadence, seuils, calibration et numéro de série ne bougent pas |
| Reprise de l'enregistrement | — | — | **N'existe pas.** Voir ci-dessous |

##### Pourquoi il n'y a pas de « Reprendre »

La question a été reposée et revérifiée à la source, plutôt que tranchée de mémoire :

* `elitech-datareader` — la bibliothèque de référence de ce projet — ne définit **aucune**
  commande de marche/arrêt pour ce protocole. Ses seuls opcodes de la famille `0x33` sont
  `01` (en-tête de données), `02` (corps), `03` (paramètres), `07` (horloge), `09` (info
  utilisateur), `0B` (numéro d'appareil) ;
* `python-elitech` a bien une `StopCommand`, mais pour une **autre famille de matériel** :
  USB HID, VID `0x04d8` (RC-51H, RC-5+, RC-55). Le nôtre est un USB-série CH340, VID
  `0x1a86` — les deux protocoles n'ont rien en commun ;
* le **manuel officiel du RC-5** est explicite : une fois l'enregistrement arrêté,
  *« il ne peut pas être redémarré en appuyant sur le bouton gauche ; il ne peut l'être
  qu'en réglant les paramètres dans le logiciel de gestion RC-5 »*. Autrement dit, la
  « reprise » du RC-5 **est** une écriture de paramètres — c'est-à-dire la commande qui
  efface la mémoire. Il n'existe pas de reprise non destructrice à implémenter ;
* un sondage des opcodes voisins (`03`, `04`, `06`, `0A`, `0C`, `0D` suivis de `00`) sur
  l'appareil réel n'a fait repasser `work_status` à `Running` dans aucun cas.

Le logiciel officiel ElitechLog propose bien « Pause » et « Reprendre » pour d'autres
loggers de la série RC — mais ceux de la famille USB HID, que ce projet refuse
explicitement (`SerialError::UnsupportedModel`, seul le modèle n° 50 est accepté). Une
commande de reprise inventée pour le RC-5 serait donc une écriture non vérifiée sur la
seule famille de matériel qu'on sait ne pas la supporter : elle n'est pas implémentée, et
ce n'est pas un oubli.

#### La réinitialisation d'un logger qui enregistre encore

Effacer un logger en cours d'enregistrement **échouait** : l'appareil continuait d'écrire
pendant l'opération, la relecture finale trouvait `rec_count != 0`, et la commande
ressortait en `ResetNotApplied`. Sur le terrain, cela a rendu un effacement pourtant voulu
et confirmé purement impossible.

`clear_memory` **arrête donc l'appareil d'abord**, quand — et seulement quand — il annonce
`WorkStatus::Running`. Deux sessions s'intercalent alors entre la lecture initiale et
l'écriture : l'envoi de `CMD_STOP`, puis la relecture qui le confirme.

Trois points qui ne sont pas négociables dans cet enchaînement :

* **l'arrêt est vérifié avant la moindre écriture.** Si le logger ne confirme pas être
  passé à `Stopped`, la commande ressort en `SerialError::StopNotApplied` et **rien n'est
  effacé** : effacer à l'aveugle un appareil dont on ignore l'état serait exactement ce
  qu'on cherche à éviter ;
* **le gabarit renvoyé à l'appareil est la fiche relue *après* l'arrêt**, pas celle
  d'avant — sinon l'écriture lui réimposerait l'état « en cours » qu'il vient de quitter ;
* **la garde de confirmation est intacte.** Le mot `EFFACER` reste exigé par `reset_rc5`.
  Ce qui a été levé est un blocage technique, pas une protection contre le clic accidentel.

L'utilisateur est prévenu **avant** de confirmer, pas après : la modale affiche un
avertissement quand le logger enregistre, parce qu'un RC-5 arrêté ne redémarrera pas seul
(voir ci-dessus) — repartir sur le terrain sans le savoir coûterait une campagne de
mesures. Le bilan le répète, et `Rc5ResetReport::recording_stopped` le porte jusqu'à l'UI.

Quatre tests couvrent la séquence sur transport simulé, comme le reste du module :
l'ordre des cinq trames émises, le choix du gabarit, l'annulation de l'effacement quand
l'arrêt n'est pas confirmé, et l'absence de commande d'arrêt superflue sur un logger déjà
à l'arrêt.

#### ⚠️ Ce que cette commande a d'incertain, et comment c'est encadré

Il n'existe **pas** de commande « effacer » dans ce protocole. Le RC-5 se réinitialise en
étant **reprogrammé** : la bibliothèque de référence note que toutes les données sont
effacées quand les paramètres sont écrits. C'est donc une écriture de paramètres
(`0x33 <station> 0x03 …`) qui est émise.

Le découpage exact du bloc de paramètres, lui, n'est pas connu champ par champ — et,
contrairement à tout le reste du § 4bis, **il n'a pas pu être vérifié contre le logger
réel**. Plutôt que de le reconstituer au jugé (une erreur d'un octet réglerait l'appareil
sur une cadence ou des seuils aberrants), la requête **renvoie à l'appareil sa propre fiche
d'identité** : le corps de la trame `DevInfo` qu'il vient d'émettre, sans son marqueur ni
sa somme de contrôle. La conséquence tient en une phrase : *aucun paramètre ne peut prendre
une valeur que l'appareil ne portait pas déjà.*

Le dispositif de vérification qui l'entoure :

* la réussite n'est **jamais** déduite de l'absence d'erreur. `clear_memory` relit la fiche
  d'identité dans une troisième session et exige `rec_count == 0` ; sinon la commande
  ressort en `SerialError::ResetNotApplied`, et il n'y a rien à défaire ;
* les réglages sont **comparés avant / après** — cadence, seuils d'alarme, calibration,
  numéro de station, numéro de série, unité d'affichage. Toute divergence est nommée à
  l'écran (« cadence : 900 s → 0 s ») avec la consigne de reprogrammer depuis ElitechLog,
  plutôt que passée sous silence ;
* rien n'est écrit sur un appareil qui n'est pas un RC-5 : le refus tombe après la lecture
  d'identité, avant la moindre requête `0x33`. Un test le vérifie en comptant les trames
  émises.

**Il n'y a volontairement pas de test matériel pour l'effacement.** Un test ignoré reste un
test qu'on lance un jour d'un `--ignored` global, et celui-là effacerait la mémoire du
logger branché — c'est-à-dire potentiellement des relevés jamais importés. Cette commande
se valide à la main, sur un logger dont les données ont d'abord été enregistrées. Tant que
ce n'est pas fait, c'est le seul endroit du projet où le comportement décrit est déduit et
non observé.

### La règle de session, encore

L'effacement enchaîne **trois sessions**, imposées par le comportement découvert au § 4bis
(une commande `0xCC` clôt la session) :

| Session | Ce qui s'y passe |
|---|---|
| 1 | `DevInfo` — l'état avant, et la trame qui sert de gabarit |
| 2 | l'écriture de paramètres, qui emporte l'effacement |
| 3 | `DevInfo` — l'état après, seule preuve que l'effacement a eu lieu |

Les 500 ms de repos entre deux ouvertures sont ceux de `SerialTransport::open`, déjà en
place : aucune temporisation supplémentaire n'est ajoutée ici, et il ne doit pas y en
avoir — ce serait une seconde règle à maintenir en parallèle de la première.

L'écriture est la seule commande émise **sans lire de réponse** (`Rc5Transport::send`).
Raison : `exchange` exige de connaître à l'avance la longueur de la réponse, que le
protocole n'annonce nulle part et qu'on ne connaît que pour les commandes observées sur le
matériel. Ne rien lire n'est pas une approximation — la fermeture du port qui suit évacue
ce que l'appareil aurait pu renvoyer, et la vérification se fait de toute façon sur son
état relu.

---

---

## 4quater. Le RC-5 sur Android : l'USB-OTG

### Le diagnostic d'abord — pourquoi `serialport` ne pouvait pas convenir

Avant d'écrire une ligne, la question posée était : la crate `serialport`, qui porte toute
la lecture directe côté desktop (§ 4bis), peut-elle fonctionner sur Android à travers un
adaptateur USB-OTG ? La réponse est **non**, et pour trois raisons cumulatives — aucune
n'est contournable, la première se lit dans le code de la crate elle-même :

1. **Elle n'énumère pas.** Dans `serialport` 4.9.0, `posix/enumerate.rs` est une cascade de
   `cfg_if`. Android n'y a **aucune branche** : ce n'est pas `target_os = "linux"`, donc le
   balayage de `/sys/class/tty` ne s'applique pas, et l'on tombe dans le `else` final —
   `Err("Not implemented for this OS")`. C'est exactement ce que rendait `list_rc5_ports`
   avant ce prompt.
2. **Il n'y a rien à énumérer.** Les noyaux Android grand public n'embarquent pas
   `CONFIG_USB_SERIAL_CH341` : aucun `/dev/ttyUSB0` n'apparaît au branchement, même en
   devinant le chemin.
3. **Et ce serait interdit.** Le bac à sable applicatif et la politique SELinux d'Android
   refusent à une application d'ouvrir un nœud de périphérique tty. Il faudrait le root.

Le seul accès légitime est l'**API USB Host** de la plateforme (`UsbManager`), en
Java/Kotlin, avec une autorisation demandée à l'utilisateur appareil par appareil. Côté
Rust, l'écosystème ne comble pas le trou : la crate `android-usbser` (0.2.3, juin 2026)
fait bien le pont JNI, mais **uniquement pour des périphériques CDC-ACM** — or une CH340
n'en est pas un, et ses auteurs annoncent eux-mêmes une crate « loin d'être complète ».

**Ce diagnostic concluait à un chantier disproportionné, et la recommandation était de
s'arrêter là.** La décision de le mener quand même a été prise explicitement, en
connaissance du coût et de l'impossibilité de le tester sans matériel. Ce qui suit décrit
ce qui a été construit, et surtout ce qui reste à vérifier.

### Ce qui a été construit : un greffon Tauri, pas un bricolage dans `gen/`

Le pont vit dans `src-tauri/plugins/tauri-plugin-usbserial/`. Ce n'est pas un détail
d'organisation : `src-tauri/gen/android/` est **régénéré** par `tauri android init` et
**ignoré par git** en entier (cf. `.gitignore`). Une classe Kotlin ou une ligne de
manifeste posée là serait perdue au premier clone. Un greffon, lui, est une dépendance
Cargo ordinaire dont le CLI Tauri injecte le projet Gradle dans `tauri.settings.gradle`, et
dont le fusionneur de manifestes reporte le `<uses-feature android:name="android.hardware.usb.host">`
dans celui de l'application.

| Fichier | Rôle |
|---|---|
| `src/lib.rs` | Façade Rust : `list_devices`, `SerialPort::{open,write,read,purge,close}` |
| `android/…/UsbSerialPlugin.kt` | `UsbManager`, autorisation runtime, sessions, fil unique |
| `android/…/PortSerie.kt` | Enveloppe du `Ch34xSerialDriver` de `usb-serial-for-android` : reliquat de lecture, purge, traduction des erreurs |
| `android/src/main/AndroidManifest.xml` | La déclaration `usb.host`, fusionnée dans l'app |

### Le protocole RC-5 n'a pas été réécrit — et c'est le point important

`elitech_serial.rs` expose depuis le § 4bis un trait `Rc5Transport` : `exchange`, `send`,
`reset_session`. Tout le reste — sommes de contrôle, `0xCC` qui clôt la session, repos de
500 ms entre fermeture et réouverture, décodage des 160 octets de `DevInfo` — est écrit
**au-dessus** de ce trait. Android n'a donc eu qu'à en fournir une seconde implémentation,
longue d'une soixantaine de lignes. Le greffon, lui, ne connaît que des octets.

C'est ce qui fait que la lecture mobile passe par exactement le même code que la lecture
desktop validée contre le matériel (10 307 mesures, zéro écart) — et non par une
réimplémentation Kotlin qu'il aurait fallu garder d'accord avec l'originale, et dont une
seule des deux aurait été testée.

Trois choses, en revanche, ont dû devenir communes aux deux transports :

* le **repos de 500 ms** entre fermeture et réouverture : c'est une contrainte du boîtier,
  pas du système, elle vaut donc aussi en USB-OTG. Il était jusqu'ici sous
  `cfg(not(target_os = "android"))` ;
* le **fil de surveillance** (`elitech_watch`), qui était un stub vide sur Android et qui
  est maintenant le même code des deux côtés ;
* les **noms de port**. Android n'a pas de `/dev/ttyUSB0` à donner, seulement un identifiant
  numérique **réattribué à chaque branchement**. Les ports y portent donc un nom fabriqué,
  `usb:<identifiant>`, qui traverse tout le reste du code sans qu'aucune couche supérieure
  ait à savoir de quel transport il s'agit.

### La demande d'autorisation, et le piège du sondage

Ouvrir un appareil USB fait apparaître une fenêtre système. Or la détection du § 4bis est
un **sondage toutes les 2,5 s** : tel quel, il aurait fait réapparaître cette fenêtre toutes
les deux secondes et demie. C'est la mémoire des ports déjà examinés (`Surveillance.connus`)
qui l'en empêche — un appareil refusé est classé « pas un RC-5 » et n'est plus interrogé
jusqu'au prochain débranchement, ou jusqu'à un « rechercher à nouveau » explicite, qui est
précisément le geste par lequel l'utilisateur redemande la fenêtre.

Deux détails de la plateforme méritent d'être notés, parce qu'ils se traduisent chacun par
une panne silencieuse quand on les manque :

* le `PendingIntent` de la demande doit être **`FLAG_MUTABLE`**. Le système y écrit
  lui-même l'appareil et le verdict avant de le renvoyer ; immuable, il revient vide et
  **toute demande paraît refusée** ;
* à partir d'Android 13, enregistrer un `BroadcastReceiver` sans préciser
  `RECEIVER_NOT_EXPORTED` est une **erreur fatale**, pas un avertissement.

### Le pilote de la puce n'est plus écrit ici — et c'est le prompt 15

Le prompt 13 avait écrit à la main la séquence d'initialisation de la CH340, dans un
`Ch340.kt` de deux cent cinquante lignes : registres de débit, LCR, lignes de contrôle,
valeurs relevées dans le pilote de référence Linux `drivers/usb/serial/ch341.c`. C'était le
seul point de tout le chantier qui n'avait aucune référence officielle — WCH ne documente
pas publiquement ces registres.

**Essayé sur le matériel, il a échoué.** Téléphone physique, adaptateur USB-OTG, RC-5 réel,
et l'essai refait avec un second adaptateur : Android détectait bien le périphérique — il
apparaissait dans `listDevices`, avec le bon `1a86:7523` — mais **aucune donnée ne
transitait**. Le détail bas niveau fautif n'a pas été identifié, et il n'y avait pas de
raison de le chercher : `mik3y/usb-serial-for-android` fait ce travail depuis 2013.

C'est la bibliothèque de référence pour ce cas d'usage exact. Elle ne demande ni root ni
pilote noyau — tout passe par la même API USB Host d'Android que le code maison — mais son
`Ch34xSerialDriver` est exercé sur du vrai matériel par ses auteurs, et treize ans de
rapports de bugs ont eu raison des détails de protocole qu'un portage à l'aveugle manque.
Le pilote maison a donc été supprimé, et rien d'autre :

| Ce qui a changé | Ce qui n'a pas changé |
|---|---|
| `Ch340.kt` supprimé, remplacé par `PortSerie.kt` (une enveloppe, pas un pilote) | La façade Rust `src/lib.rs`, à la ligne près |
| Une dépendance Gradle vers JitPack | `UsbSerialPlugin.kt` : `UsbManager`, autorisation runtime, sessions, fil unique |
| — | `elitech_serial.rs` : le protocole RC-5, sommes de contrôle, `0xCC`, repos de 500 ms |

Trois choses seulement restent en propre dans `PortSerie.kt`, parce que la bibliothèque ne
les fait pas :

* le **reliquat de lecture**. Un transfert en masse rend ce que le contrôleur a bufferisé,
  pas ce qu'on a demandé : lire 3 octets quand la puce en a 64 à donner perd les 61 autres.
  On lit donc toujours dans un tampon d'au moins un paquet entier, et le surplus est gardé
  pour l'appel suivant — le jeter décalerait l'alignement de toutes les trames ;
* la **purge de l'entrée**. `purgeHwBuffers` lève `UnsupportedOperationException` sur une
  CH34x : la puce n'a pas de commande de vidage. On draine en lisant jusqu'au silence ;
* la **traduction des erreurs**, qui conserve le message d'origine de la bibliothèque
  **mot pour mot** et le préfixe d'une étape nommée — ouverture, configuration série,
  écriture, lecture. Le prompt 13 a échoué sans qu'on puisse dire où : c'est précisément ce
  que cette traduction rend lisible dans un `logcat` la prochaine fois.

Ce qui vient de la bibliothèque, en revanche, est exactement ce que le code maison faisait
mal ou pas : `115200 8N1` par `setParameters` après un `open` qui initialise la puce, et
**DTR comme RTS laissés au repos** — transposition du `dtr_on_open(false)` desktop, pour la
même raison (la même puce équipe les clones Arduino, qu'un DTR levé redémarre). La
bibliothèque gère elle-même l'inversion de ces deux bits dans le registre de la puce.

#### Le piège Gradle : un dépôt ne se propage pas d'un projet à l'autre

`usb-serial-for-android` n'est publié que sur **JitPack** — le seul artefact du même nom sur
Maven Central est un portage tiers abandonné en 2019. Il faut donc déclarer ce dépôt, et
c'est là que Gradle surprend : **les dépôts ne s'héritent pas entre projets**. C'est `:app`
qui dépend de `project(":tauri-plugin-usbserial")`, donc c'est `:app` qui résout nos
dépendances externes transitives, avec **ses** dépôts (`google()`, `mavenCentral()`). Un
`repositories { }` posé dans le seul greffon le fait compiler et laisse l'application
échouer à l'assemblage.

D'où le `rootProject.allprojects { repositories { … } }` du `build.gradle.kts` du greffon.
Poser la ligne dans `gen/android/build.gradle.kts`, où elle serait plus naturelle, ne
tiendrait pas : ce fichier est régénéré par `tauri android init` et ignoré par git — la
raison d'être du greffon, cf. plus haut.

La version est **épinglée à 3.9.0**, pas à la dernière. À partir de 3.10, la publication
JitPack traîne un `kotlin-stdlib` 2.2.x : la résolution de Gradle le préférerait au 1.9.25
du greffon Kotlin de ce projet, et le compilateur 1.9 refuse les métadonnées d'un stdlib
2.2. Le `Ch34xSerialDriver` de 3.9.0 est celui de 3.11.0 à l'octet près (diff vérifié) ;
seul l'`androidx.annotation` transitif diffère.

### ⚠️ Ce qui a été vérifié, et ce qui ne l'a pas été

C'est la limite la plus importante de cette section, et elle ne se réduira pas sans
matériel : **l'émulateur Android n'a aucun contrôleur USB hôte**. Aucun appareil ne peut y
être branché, réellement ou virtuellement.

| Vérifié | Comment |
|---|---|
| La bibliothèque est résolue depuis JitPack | `:tauri-plugin-usbserial:assembleDebug` → `BUILD SUCCESSFUL` |
| `:app` la résout aussi — le piège des dépôts est bien évité | `:app:dependencies --configuration universalDebugRuntimeClasspath` → `com.github.mik3y:usb-serial-for-android:3.9.0` |
| Elle est réellement empaquetée dans l'APK | `cargo tauri android build --apk --debug --target aarch64` → APK produit ; `com/hoho/android/usbserial` et `Ch34xSerialDriver` présents dans son `classes.dex` |
| Le pilote maison a bien disparu | `PortSerie` dans les dex de l'APK, plus aucune trace de `Ch340` |
| Aucune régression desktop | 348 tests + 3 tests du greffon, tous verts, `elitech_serial.rs` inchangé |
| (prompt 13) Le greffon est chargé et atteignable | `Tauri/Plugin: pluginId: usbserial, command: listDevices` dans `logcat`, toutes les 2,5 s |
| (prompt 13) Le `<uses-feature usb.host>` arrive dans l'application | Présent dans le manifeste fusionné de l'APK |
| (prompt 13, **sur matériel**) Le périphérique est détecté | Le RC-5 apparaît dans `listDevices` avec son `1a86:7523`, sur deux adaptateurs OTG |

**Non vérifié — exige un téléphone, un adaptateur OTG et le RC-5 en main :**

* que le remplacement du pilote corrige effectivement la panne du prompt 13 ;
* le dialogue d'autorisation USB et son retour par diffusion ;
* les transferts en masse : une trame écrite, une trame lue, alignement compris ;
* la lecture complète des 10 307 mesures, et sa concordance avec l'export xlsx ;
* le repos de 500 ms appliqué à une fermeture/réouverture de session USB ;
* la réinitialisation, qui n'a de toute façon jamais été exercée sur le matériel (§ 12).

**Tant que cette liste n'a pas été faite, la lecture directe sur Android doit être tenue
pour non fonctionnelle**, et l'import xlsx reste la voie sûre sur mobile comme ailleurs.

#### Comment faire l'essai

Aucun téléphone n'était branché à la machine de développement au moment de ce prompt (`adb
devices` : liste vide), et l'émulateur ne peut rien y apporter. L'essai reste donc à faire,
dans cet ordre :

```
cargo tauri android build --apk --debug --target aarch64
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
adb logcat -c && adb logcat | findstr /i "usbserial RustStdoutStderr Ch34x"
```

Puis brancher l'adaptateur OTG et le RC-5, accepter la demande d'autorisation, et lancer une
lecture. Noter le **modèle exact du téléphone**, la **référence de l'adaptateur OTG** et le
message tel qu'il sort — c'est ce que le tableau ci-dessous sert à situer.

#### Si l'essai suivant échoue encore

Ne pas écrire un troisième pilote maison. Relever, dans `logcat`, le message exact tel qu'il
sort — `PortSerie.traduire` le nomme déjà d'une étape :

| Ce que dit le message | Où ça casse |
|---|---|
| L'appareil n'apparaît pas dans `listDevices` | Énumération — adaptateur OTG, câble, ou mode hôte absent de l'appareil |
| `PERMISSION_DENIED:` | Le dialogue d'autorisation, ou son `PendingIntent` |
| `Ouverture du port série USB (CH34x) : …` | `Ch34xSerialDriver.openInt` — réclamation d'interface, points de terminaison |
| `Configuration série 115200 8N1 (CH34x) : …` | `setParameters` / `setDTR` — registres de la puce |
| `Écriture sur le port série USB : …` | Premier échange, sens montant |
| `Lecture sur le port série USB : …` ou un `Timeout` Rust | Premier échange, sens descendant : la commande est partie, le boîtier n'a pas répondu |

C'est ce relevé, et non une reformulation, qui doit remonter pour qu'une décision soit prise
sur la suite : abandon de la lecture RC-5 directe sur mobile, ou autre piste.

## 5. API Infoclimat

Deuxième source de données de l'application, après les fichiers de logger. Elle couvre
**toutes** les stations professionnelles suivies par le projet :

| Station | Identifiant | Matériel |
|---|---|---|
| Crête de la Grande Moucherolle | `STATIC0479` | Dragino LoRa, republié via Orange Live Objects |
| Allières, Lans-en-Vercors, Méaudre, Darbounouse, Losence | à renseigner | Davis Vantage Pro 2, réseau StatIC |

### Il n'y a pas de « format Dragino »

C'est la correction la plus importante par rapport à ce qui avait été anticipé. La
Moucherolle est bien un capteur Dragino sur LoRa, mais **elle republie sur Infoclimat
exactement au même format que les Vantage Pro 2**. Un seul parseur couvre donc toutes les
sources d'API, et il n'existe qu'un seul type de source côté modèle :
`SourceType::InfoclimatApi`.

Voir aussi le § « Ce que devient `DraginoApi` » plus bas.

---

### ⚠️ La clé d'API est liée à votre adresse IP publique

**C'est le piège n°1 de cette API, et il ne ressemble pas à un problème d'IP.**

Infoclimat associe chaque clé à l'adresse IP publique déclarée **au moment de sa
génération**. Si cette adresse change, les appels cessent de fonctionner alors que la clé
est toujours valide en soi. Or l'IP publique d'une ligne grand public change souvent :

* redémarrage de la box (le cas le plus fréquent) ;
* renouvellement du bail DHCP par l'opérateur ;
* appel depuis un autre réseau : VPN, partage de connexion 4G, autre lieu.

#### Ce que répond réellement l'API — vérifié le 19 août 2026

Deux refus distincts, **dont aucun n'est un code d'authentification HTTP** :

| Cause | Code HTTP | Corps de la réponse |
|---|---|---|
| IP non déclarée | **`200 OK`** | texte brut `Wrong ip address` |
| Clé inconnue / révoquée | `400` | `{"errors":["No account found for this token: '…'"]}` |

Le premier cas est le pire : du point de vue du protocole, **c'est un succès**. Un client
qui suppose du JSON échoue sur une erreur de désérialisation à mille lieues du vrai
problème. C'est exactement pourquoi la reconnaissance se fait sur le *contenu* de la
réponse et non sur son code (`infoclimat::MOTIFS_AUTH`).

#### Ce que fait l'application

* elle reconnaît les deux refus et les traduit en un message unique et explicite, qui
  **nomme la cause probable** (l'IP a changé) et **donne la marche à suivre** ;
* ce message est affiché en bandeau dans la fiche de la station, pas en toast : il tient
  plusieurs paragraphes, et c'est précisément celui qu'il faut lire en entier ;
* l'écran Paramètres rappelle la contrainte **avant** que le problème n'arrive.

#### Que faire le jour où ça arrive

1. Ouvrir <https://www.infoclimat.fr/opendata/> **depuis le poste qui exécute les
   requêtes** (c'est son IP publique qui compte, pas celle de la box vue de l'intérieur) ;
2. se connecter, générer une nouvelle clé ;
3. remplacer le contenu de `Clee.API/API.Infoclimat.env` ;
4. rouvrir l'écran Paramètres — le statut de la clé y est relu à chaque ouverture.

#### Diagnostiquer sans lancer l'application

Un test d'intégration, `#[ignore]` par défaut, appelle l'API pour de vrai :

```bash
cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture appel_reel
```

Il affiche soit le nombre de mesures reçues, soit le message d'erreur complet tel que
l'utilisateur le verrait.

---

### La clé : où elle vit, et pourquoi elle ne fuite pas

Le fichier `Clee.API/API.Infoclimat.env` contient **la clé nue** — pas de `NOM=valeur`, pas
de retour à la ligne. Le lecteur accepte les deux formes (et les commentaires `#`), au cas
où quelqu'un réécrirait le fichier « proprement » plus tard. La variable d'environnement
`GWS_INFOCLIMAT_TOKEN` est prioritaire, ce qui sert aux tests.

Le dossier `Clee.API/` est **ignoré par git en entier** (et non plus seulement ses `.env` :
il contient aussi `Moucherolle.json`, 9,78 Mo, et une clé collée par erreur dans un fichier
sans extension `.env` serait passée à travers).

Trois garde-fous côté code :

* **l'URL** n'est jamais journalisée ni renvoyée telle quelle : `redact()` remplace la
  valeur du paramètre `token` par `***`. `reqwest` recopie l'URL appelée dans ses messages
  d'erreur réseau — sans cela, la clé partait dans l'UI ;
* **le corps de réponse** est nettoyé lui aussi, et ce n'était pas prévu : interrogée avec
  une clé inconnue, l'API **renvoie la clé en clair** dans son message d'erreur. `scrub()`
  l'efface avant que le message n'entre dans une erreur ;
* **l'UI** ne reçoit qu'un `KeyStatus` : les quatre derniers caractères et la longueur.
  Jamais la clé. Un test vérifie que la sérialisation ne la contient pas.

---

### Les deux modes de récupération

Ils partagent **exactement le même parseur** : `infoclimat::parse_infoclimat` travaille sur
un `&str`, pas sur un chemin de fichier. Aucun des deux modes n'a de code de lecture qui
lui soit propre — c'est ce qui garantit qu'un fichier validé à la main se comporte comme
une réponse d'API, et réciproquement.

#### Mode 1 — appel direct

Bouton **« Récupérer les dernières données »**, en tête de la fiche d'une station à
laquelle un identifiant Infoclimat est rattaché.

* la période demandée démarre **à la dernière mesure connue** de la série, pas au début de
  l'historique : redemander tout à chaque clic ne produirait que des doublons ;
* au tout premier appel (aucune mesure), on remonte 7 jours ;
* la période est découpée en **tranches de 7 jours maximum** (limite affichée par
  Infoclimat), enchaînées avec une pause de 400 ms — la plateforme ne plafonne pas les
  requêtes mais demande un usage raisonnable, elle est maintenue par des bénévoles ;
* un **garde-fou de fréquence** refuse de réinterroger une série avant l'intervalle minimal
  configuré (60 min par défaut, modifiable dans Paramètres). Le refus n'est pas un mur :
  l'UI propose de forcer l'appel ;
* l'appel réseau part dans un `spawn_blocking` : la fenêtre ne se fige pas pendant un
  rattrapage en plusieurs tranches.

#### Mode 2 — import d'un fichier

Onglet **« Infoclimat (JSON) »** de la modale d'import, pour un export téléchargé à la main
depuis le site. Une case à cocher permet de **compléter la série existante** de cette
station plutôt que d'en créer une nouvelle. Le choix reste explicite : la règle « on ne
fusionne jamais rien automatiquement » (§ 3) interdit de décider à la place de
l'utilisateur, mais rien n'empêche de le lui proposer.

Le réglage d'horloge est **masqué** sur cet onglet — voir ci-dessous.

---

### Le temps : aucune ambiguïté sur cette source

`dh_utc` est de l'**UTC réel**, au format `AAAA-MM-JJ HH:MM:SS`, mois toujours en deuxième
position. Toute la machinerie de détection jour/mois bâtie pour les fichiers Elitech (§ 4)
est donc **court-circuitée** :

* `clock_offset_seconds = 0` et `raw_timestamp == timestamp_utc` — l'invariant du § 3 dans
  son cas trivial ;
* `raw_timestamp_was_utc = true` ;
* `date_format_uncertain` est **toujours** `false` : aucune série Infoclimat ne peut porter
  le bandeau « format de date à confirmer » ;
* l'UI n'affiche ni « Recalibrer l'horloge » ni « Inverser jour/mois » sur ces séries. Les
  proposer n'offrirait qu'une façon de corrompre des horodatages justes par construction.

---

### Deux formats de fichier, un seul chemin de lecture

Le site d'Infoclimat propose l'export en **JSON** et en **CSV**. Les deux portent
exactement les mêmes 29 champs, dont `dh_utc` au même format. Il aurait été absurde d'en
faire deux parseurs : le lecteur CSV ne fait que **reconstruire des enregistrements
bruts**, puis rejoint la chaîne commune (conversion → dédoublonnage → cadence dominante).
Tout ce qui *décide* de quelque chose — quelle ligne écarter, comment lire un nombre,
comment traiter `complements` — n'existe donc qu'en un exemplaire. Un test le verrouille :
la même mesure, présentée dans les deux formats, doit ressortir strictement identique.

**Le format est déduit du contenu, jamais de l'extension** : un contenu qui commence par
`{` est du JSON, tout le reste est tenté en CSV. Côté interface, il n'y a donc rien à
choisir — le même onglet « Infoclimat (JSON / CSV) » accepte les deux, et l'analyse
préalable affiche lequel il a reconnu.

Un détail compte dans cet aiguillage : le refus d'authentification (`Wrong ip address`,
servi en **texte brut avec un code HTTP 200**) est reconnu *avant* la bascule vers le CSV.
Sans cette garde, il produirait une erreur d'intitulés incompréhensible au lieu de
l'explication sur l'IP.

Fichier de référence : `export_infoclimat.csv` — station `STATIC0213`, **La
Chapelle-en-Vercors / Combe de Darbounouse**, 137 088 mesures depuis le 14 octobre 2023.

**Ce que le CSV a de particulier**, constaté sur ce fichier :

* **trois blocs** : des lignes de métadonnées préfixées `#`, une ligne d'intitulés, puis —
  piège — une **ligne d'unités** (`string;YYYY-MM-DD hh:mm:ss;°C;hPa;…`) qu'il ne faut
  surtout pas prendre pour une mesure. Elle n'est pas sautée aveuglément : le lecteur
  vérifie que son champ `dh_utc` n'est pas une date, si bien qu'un export qui ne la
  porterait pas ne perdrait pas sa première mesure ;
* séparateur `;` ;
* **`complements` y est du JSON simple**, pas doublement encodé comme dans le JSON : la
  valeur brute est directement `{"rssi":-109,…}`. Une passe de décodage en moins, le même
  code de lecture ;
* la licence **n'est pas la même** que celle de la Moucherolle : ce fichier annonce
  `Licence Ouverte/Open License`. Elle est lue dans les lignes `#`, jamais supposée ;
* le CSV n'a pas de bloc `stations[]`. Le nom de la station vient des lignes `#`, et sa
  **position** du champ `complements` de la première mesure qui en porte une — seule
  source disponible. L'altitude, elle, n'est nulle part : elle reste vide, rien n'est
  inventé.

`SourceType::InfoclimatApi` reste la bonne variante pour un import CSV : c'est un mode
d'import de fichier **complémentaire** à l'API, pas une nouvelle source. Ce qui distingue
« importé d'un fichier » de « récupéré par l'API », c'est `raw_file_reference` — renseigné
dans le premier cas, `NULL` dans le second.

### Le format, et ses quatre pièges

Le fichier de référence est `Clee.API/Moucherolle.json` : 9,78 Mo, station `STATIC0479`,
55 jours à la cadence de 10 minutes, **7905 enregistrements**. Il sert de test
d'intégration réel (`src-tauri/tests/infoclimat_moucherolle.rs`), exactement comme
`docs/samples/` sert au parseur Elitech — à ceci près que celui-ci, lui, est authentique.

1. **Les nombres sont des chaînes JSON** : `"temperature": "13.8"`, pas `13.8`. Le parseur
   accepte les deux encodages, faute de savoir si toutes les stations se comportent pareil.
2. **`complements` est du JSON dans du JSON** :
   `"{\"rssi\":-86,\"lat\":45.0093,\"lon\":5.5575,\"batt\":40.44}"`. ⚠️ La description de
   ce champ dans le bloc `metadata` du même fichier annonce un format à 33 champs séparés
   par des `@` : **elle ne correspond pas au contenu réel** et doit être ignorée. Un
   `complements` illisible ne fait perdre que le RSSI et la batterie, jamais la mesure.
3. **La quasi-totalité des 29 champs valent `null`** pour cette station (pression, vent,
   pluie, UV…). Tout est `Option`, rien n'est supposé présent.
4. **`hourly` n'est pas qu'un dictionnaire de stations** : il porte aussi une clé
   `_params`. Les clés préfixées par `_` sont écartées, sans quoi `_params` serait compté
   comme une station.

Si le format réel diverge un jour de cette description, **c'est le fichier qui a raison** :
les tests sont écrits contre lui.

---

### Ce que le schéma a gagné (migration v3)

| Table | Colonne | Rôle |
|---|---|---|
| `measurements` | `rssi_dbm` | qualité du signal radio, en dBm (−117 à −73 sur l'échantillon) |
| `measurements` | `battery` | valeur **brute**, unité indéterminée — voir ci-dessous |
| `logger_series` | `data_license` | mention de licence renvoyée par l'API |
| `logger_series` | `station_id` | identifiant de la station interrogée |
| `logger_series` | `last_api_fetch_at` | dernier appel réussi (garde-fou de fréquence) |
| `points` | `infoclimat_station_id` | rattachement point ↔ station |
| `app_settings` | *(nouvelle table)* | réglages : intervalle de rafraîchissement |

`sample_interval_seconds` existait déjà sur `logger_series` ; il est désormais **déduit**
pour une série d'API (l'écart le plus fréquent entre deux mesures — le mode, pas la
médiane, pour qu'une station longtemps muette ne fausse pas la cadence nominale).

#### ⚠️ La batterie est illisible — et c'est assumé

Sur les 7905 mesures réelles, la clé `batt` vaut le plus souvent entre 39,7 et 41,1, mais
descend à **6,48** et monte à **72**. Ce n'est ni une tension Dragino plausible (3,0–4,2 V),
ni un pourcentage stable, ni un millivoltage. La valeur est donc stockée **telle quelle,
sans conversion ni mise à l'échelle**, et affichée sans unité. **Ne pas en tirer d'alerte
« batterie faible »** tant que l'échelle n'a pas été confirmée auprès du propriétaire de la
station.

#### `UNIQUE(series_id, timestamp_utc)`

L'API est une source **réinterrogeable**, dont deux appels successifs se recouvrent presque
toujours. Sans cette contrainte, chaque rafraîchissement empilait les mêmes mesures en
double, silencieusement. Elle porte sur le couple : deux séries distinctes peuvent toujours
avoir une mesure au même instant, on ne fusionne toujours rien.

**Stratégie de conflit**, selon le chemin :

* **création d'une série** (import de fichier) : `ON CONFLICT … DO NOTHING`. Un fichier qui
  répète un horodatage ne fait pas échouer l'import ; l'écart est remonté dans le bilan.
* **complément d'une série** (appel direct) : `ON CONFLICT … DO UPDATE … WHERE <une valeur
  diffère>`. Mettre à jour, mais seulement si la nouvelle valeur apporte quelque chose —
  Infoclimat peut republier une mesure corrigée, et `DO NOTHING` figerait à jamais la
  première version reçue. Le bilan distingue **insérées / corrigées / inchangées**.

**Effet de bord à connaître.** SQLite vérifie l'unicité *ligne à ligne* : il n'existe pas de
contrainte différée hors clés étrangères. Or les deux corrections en masse du socle
réécrivent tous les instants d'une série d'un coup — décaler une série régulière de +1 h
amène la mesure de 06:00 sur 07:00, instant encore occupé ; permuter jour et mois peut
faire échanger leur date à deux mesures (05/01 ↔ 01/05). `db::park_timestamps` gare donc
d'abord tous les horodatages sous le minimum de la série. Deux tests verrouillent ce
comportement.

**À la migration**, les doublons préexistants sont supprimés (on garde la ligne de plus
petit `id`, la première insérée) avant la création de l'index — sinon il ne pourrait pas
être créé. Le nombre de lignes supprimées est affiché dans la console : une base ne doit pas
rétrécir en silence.

---

### Licence et attribution

Le bloc `stations[]` annonce la licence. Pour la Moucherolle :
`NON-COMMERCIAL ONLY: CC BY NC` — **l'attribution est une condition de la licence, pas une
politesse**.

Elle est stockée **par série** (`data_license`) et non globalement : deux stations
Infoclimat peuvent ne pas porter la même licence. La mention
« Données Infoclimat (infoclimat.fr) — CC BY-NC, réutilisation non commerciale » apparaît :

* en pied de chaque série concernée dans la fiche d'une station ;
* dans l'analyse d'un fichier avant import, et dans le bilan d'import ;
* dans l'écran Paramètres.

L'export HTML devra la reprendre : c'est prévu au prompt dédié à l'export, pas ici.

---

### Ce que devient `DraginoApi`

La variante `SourceType::DraginoApi` **n'est produite par aucun code** et ne doit pas l'être.
Elle est désormais documentée comme réservée à un hypothétique accès **direct** à la
plateforme Orange Live Objects — les trames LoRa brutes, qu'Infoclimat republie aujourd'hui
sous `"source": "liveobjects"`. Rien n'indique qu'on en aura besoin un jour.

**Le nom est conservé tel quel**, pour une raison concrète : la chaîne `'DraginoApi'` est
gravée dans la contrainte `CHECK` de `logger_series`, et SQLite ne sait pas modifier une
contrainte `CHECK` sans reconstruire la table entière. Faire cette reconstruction pour un
gain purement cosmétique aurait été un risque sans contrepartie.

La clarté est donc obtenue autrement : **l'identifiant brut n'est jamais affiché**.
`SourceType::label_fr()` le traduit en « Orange Live Objects (réservé) », et l'UI n'utilise
que ce libellé — pour toutes les sources, d'ailleurs (« Fichier Elitech », « API
Infoclimat »…).

---

## 5bis. ROMMA — archives d'une station du massif alpin

Troisième source de données, après les fichiers de logger et Infoclimat. **ROMMA** (Réseau
d'Observation Météo du Massif Alpin) publie, pour chacune de ses stations, un export CSV de
l'historique complet.

Fichier de référence : `archives_Lans - Les Allières_31-05-2017_19-08-2026.csv` —
**476 912 mesures sur neuf ans**, au pas de 10 minutes, 38 Mo.

Il n'y a **pas d'API ROMMA** dans cette application : uniquement l'import de fichier. Le
champ « station ROMMA » du formulaire de point ne débloque donc aucune récupération
automatique, contrairement à son homologue Infoclimat — il sert à la traçabilité et à
retrouver la bonne série.

### Le temps : `timestamp`, et rien d'autre

Trois colonnes datent chaque ligne, et **une seule est utilisée** :

| Colonne | Contenu | Utilisée ? |
|---|---|---|
| `timestamp` | epoch Unix, **UTC** | **oui**, seule source de vérité |
| `Date` | `JJ-MM-AAAA`, heure **locale** | non |
| `heure` | `HH:MM`, heure **locale** | non |

Vérification faite sur le fichier réel : l'écart entre `Date`+`heure` et l'instant UTC de
`timestamp` vaut +1 h ou +2 h selon la saison — l'heure d'hiver et l'heure d'été
françaises. Reconstruire l'instant depuis `Date`+`heure` obligerait à rejouer les bascules
CET/CEST, avec l'heure sautée en mars et l'heure doublée en octobre à traiter. Le
`timestamp` évite tout cela.

ROMMA est donc, comme Infoclimat, une source **nativement UTC** : `clock_offset_seconds = 0`,
`raw_timestamp_was_utc = true`, aucun réglage d'horloge proposé dans l'onglet d'import, et
le `DateOrderScanner` court-circuité.

### Le format, et ce qu'il a d'inattendu

Séparateur `,`, décimale `.`, et — comme le CSV d'Infoclimat — **deux lignes d'en-tête** :

```text
Date,heure,timestamp,Temp,Tmin,Tmax,Hum,Pression,Pluie,Vmoy,VmoyDir,Rafale,RafaleDir,RadSolaire, RadSolaireMax
,,,degreC,degreC,degreC,%,hPa,mm,km/h,,km/h,,W/m2, W/m2
31-05-2017,19:00,1496250000,12.2,12.2,12.2,72,1021.0,0.00,10.0,SO,24.1,SO,NULL,NULL
```

La seconde ligne porte les **unités**. C'est d'elle que viennent les unités documentées sur
`Measurement` : elles sont **lues**, pas devinées, puis recoupées avec les ordres de
grandeur observés (un vent moyen sous 200 km/h, une pression dans `[850, 1100]` hPa à
1 000 m — deux assertions du test d'intégration).

Les colonnes sont repérées **par leur nom**, jamais par leur position : l'export porte déjà
un intitulé avec une espace de tête (` RadSolaireMax`), signe que l'ordre n'est pas une
garantie.

**Les valeurs manquantes s'écrivent littéralement `NULL`** (et `---` pour une direction de
vent indéterminée). Ce n'est pas une erreur de parsing, c'est un capteur qui n'existait pas
encore. `parsing::parse_number` les traite déjà comme des absences — aucun code spécifique.
Voici ce que contient le fichier réel :

| Colonne | Lignes à `NULL` | Lecture |
|---|---|---|
| `RadSolaire` | **476 912 (100 %)** | la colonne est vide de bout en bout |
| `RadSolaireMax` | 189 044 (40 %) | capteur ajouté en cours de route |
| `Pression` | 185 574 (39 %) | idem |
| `Pluie` | 103 848 (22 %) | |
| `Vmoy` / `Rafale` | ~71 000 (15 %) | |
| `Temp` | **10 621 (2,2 %)** | ces lignes sont **écartées** : sans température, pas de mesure |

Deux conséquences valent d'être connues :

* **le rayonnement vient de `RadSolaireMax`.** `RadSolaire` est vide sur *toutes* les
  lignes ; la prendre au pied de la lettre reviendrait à ne jamais rien stocker. Le repli
  est fait, **et signalé** — dans l'analyse préalable comme dans le code. Ce n'est pas la
  même grandeur (un maximum sur l'intervalle, pas une moyenne). Si un export renseigne un
  jour les deux colonnes, `RadSolaire` reprend la priorité ;
* **2,2 % des lignes sont écartées.** Le compte exact est affiché ; seuls les 200 premiers
  cas sont détaillés, parce que dix mille avertissements ne se lisent pas et coûteraient
  plusieurs mégaoctets.

### La direction du vent est une rose, pas des degrés

`VmoyDir` et `RafaleDir` contiennent des abréviations **françaises** à 16 points — `N`,
`NNE`, `NE`, … `NNO`, avec **`O` pour Ouest** là où l'anglais écrirait `W`. La conversion
en degrés est faite à l'import (`romma_import::direction_en_degres`). La valeur stockée est
donc un **secteur de 22,5°**, pas une mesure fine : à garder en tête avant d'en tirer une
statistique de direction.

Un nombre est accepté tel quel, au cas où un futur export publierait des degrés. La rose
anglaise (`W`, `NW`…) n'est volontairement **pas** gérée : l'accepter au petit bonheur
masquerait un changement de format.

### Ce que le schéma a gagné (migration v5)

`SourceType::RommaImport` s'ajoute aux variantes existantes — d'où la reconstruction de
`logger_series` décrite au § 3.

Six colonnes facultatives arrivent sur `measurements`, toutes `Option<f64>`, toutes `NULL`
pour les sources qui ne les publient pas — c'est-à-dire **toutes sauf ROMMA** :

| Colonne | Unité | Origine |
|---|---|---|
| `pressure_hpa` | hPa | `Pression` |
| `wind_avg_kmh` | km/h | `Vmoy` |
| `wind_gust_kmh` | km/h | `Rafale` |
| `wind_direction_degrees` | degrés | `VmoyDir`, converti depuis la rose |
| `rain_mm` | mm | `Pluie` |
| `solar_radiation` | W/m² | `RadSolaire`, sinon `RadSolaireMax` |

`solar_radiation` est le seul dont le nom ne porte pas son unité : celle du rayonnement
varie d'un réseau à l'autre (W/m², J/cm², kWh/m²) et figer `_wm2` obligerait à renommer la
colonne le jour où une autre source en publierait autrement.

Enfin, `points.romma_station_id` complète `infoclimat_station_id`. Saisie **libre** : ROMMA
n'expose pas d'annuaire interrogeable, et l'export ne nomme la station que dans son *nom de
fichier*.

### Gros volume

477 000 lignes, 38 Mo. Deux choses ont été faites pour que ça tienne :

* l'import s'écrit dans **une seule transaction**, avec une requête préparée réutilisée et
  un repère de progression tous les 25 000 enregistrements (`db::MEASUREMENT_BATCH`). Une
  seule transaction, pas plusieurs : un import à moitié écrit serait pire qu'un import
  lent ;
* le parseur travaille ligne à ligne, sans construire de structure intermédiaire au-delà
  des champs découpés.

Comptez quelques secondes pour une archive complète. Le test d'intégration
`l_archive_romma_complete_s_importe_en_base` déroule la chaîne entière sur le fichier réel.

Ce volume est aussi le cas dimensionnant du bloc « Croisement de données » : les chiffres
mesurés (lecture SQLite + agrégation, base sur disque) sont au § 11.

### Ce qui n'est pas lu : `climato_*.csv`

ROMMA publie aussi un fichier `climato_*.csv` : des **agrégats journaliers** (Tn, Tx,
amplitude, durée de gel…) dérivés des mêmes mesures. Il n'est délibérément pas supporté —
l'application recalcule tous ces agrégats elle-même depuis les mesures détaillées (§ 3), et
les importer reviendrait à stocker deux fois la même information, avec le risque qu'elles
divergent. Le message d'erreur du parseur le dit et oriente vers l'archive détaillée.

---

## 6. Agent IA (Groq)

Bloc « Agent IA » de la grille (bas-droite). Il analyse les points sélectionnés sur la
carte, en s'appuyant sur les **statistiques** calculées par `stats.rs` et sur les
**métadonnées** de chaque point — pas sur les valeurs brutes de température.

| | |
|---|---|
| **Modèle** | `openai/gpt-oss-20b` |
| **Endpoint** | `https://api.groq.com/openai/v1/chat/completions` (format OpenAI) |
| **Clé** | `Clee.API/API.Groq.env`, ou la variable `GWS_GROQ_TOKEN` |
| **Module** | `src-tauri/src/groq.rs` |

### Trois choses que l'appel réel a apprises

Contrairement à Infoclimat (§ 5), Groq se comporte proprement : vrais codes HTTP, erreurs
JSON structurées, et **la clé n'est jamais renvoyée** dans le corps d'erreur — une clé
invalide ne produit que `{"error":{"message":"Invalid API Key",…}}`. Trois surprises quand
même, toutes vérifiées contre l'API et couvertes par un test :

**1. `gpt-oss-20b` est un modèle à raisonnement, et le raisonnement se sert en premier.**
Sa réponse porte deux champs : `message.reasoning` et `message.content`. Le premier
consomme des tokens de sortie **avant** le second. Le tout premier appel de test, lancé
avec `max_tokens: 50`, a renvoyé `finish_reason: "length"`, 48 tokens de raisonnement et un
`content` **vide** — soit un bloc « Agent IA » désespérément blanc, sans la moindre erreur
pour l'expliquer. D'où trois décisions dans `groq.rs` : `reasoning_effort: "low"` (le
raisonnement retombe à une dizaine de tokens), `max_tokens: 1200`, et surtout le traitement
explicite du cas « content vide » → `GroqError::ReponseVide`, qui dit à l'utilisateur que
le budget est épuisé au lieu de n'afficher rien.

**2. La limite est de 8 000 tokens par minute** (`x-ratelimit-limit-tokens`, palier gratuit
`on_demand`). C'est peu : **six requêtes moyennes suffisent à l'épuiser** — mesuré, la
sixième est refusée. Cette limite conditionne toute la conception du contexte injecté
(voir plus bas) et le debounce du déclenchement automatique.

**3. Le dépassement arrive sous deux codes HTTP, et aucun n'est celui qu'on attend.**

| Situation | Statut | `code` |
|---|---|---|
| Une **seule** requête dépasse le plafond | `413` | `rate_limit_exceeded` |
| Le **cumul de la minute** dépasse le plafond | `429` | `rate_limit_exceeded` |
| Clé invalide ou révoquée | `401` | `invalid_api_key` |
| Modèle inconnu | `404` | `model_not_found` |

C'est le champ **`code`** qui tranche, pas le statut. Le message brut de Groq contient
l'identifiant d'organisation et un lien vers sa page de facturation : `message_de_quota()`
le reformule et n'en garde que ce qui est actionnable — le délai avant réessai, arrondi à
la seconde supérieure (« réessayez dans 0 seconde » n'aide personne).

### Comportement en cas d'erreur, côté UI

Aucune erreur brute n'arrive à l'écran. Les messages de `GroqError` sont rédigés pour
l'utilisateur et affichés tels quels dans le bloc :

* **clé absente** — le bloc est désactivé d'emblée (champ de question et boutons grisés) et
  affiche où poser la clé. Aucun appel n'est tenté, y compris le déclenchement automatique ;
* **clé refusée** — le message rappelle qu'une clé Groq, contrairement à une clé Infoclimat,
  **ne dépend pas de l'adresse IP** : si elle est refusée, c'est bien la clé qui est en
  cause, et il faut la régénérer sur `console.groq.com/keys` ;
* **quota atteint** — le message donne le plafond, l'ordre de grandeur (« environ six
  analyses consécutives ») et le délai avant réessai ;
* **réponse vide** — cas du raisonnement qui épuise le budget : invite à reformuler ou à
  réduire la sélection.

### Ce qui est envoyé au modèle

Le system prompt (`groq::SYSTEM_PROMPT`) porte **tout le contexte métier** : les quatre
catégories et leurs contraintes de taille, l'asymétrie de réaction entre petits et gros TAF,
le site de référence Darbounouse et le piège Darbounouse/Derbenouse, la liste des biais à
signaler, et les consignes de ton. Il est **strictement factuel** : pas de politesse, pas de
guillemets d'excuse autour du vocabulaire du domaine. Un test vérifie qu'aucun de ces
éléments ne disparaît d'une modification.

S'y ajoutent, pour chaque analyse, **les métadonnées réelles** des points concernés
(catégorie, taille, altitude, facteurs de site renseignés, notes) et **les statistiques**
de `stats.rs` : période couverte en UTC, minimum et maximum extrêmes avec leurs dates,
moyenne, moyenne des minima quotidiens, amplitude quotidienne moyenne et maximale.

**Aucune mesure brute n'est envoyée.** Le plafond de 8 000 tokens/minute le rendrait
impossible de toute façon — une série Elitech au quart d'heure sur un mois fait déjà 2 900
lignes — et le modèle est plus fiable sur des agrégats déjà calculés que sur de
l'arithmétique appliquée à des milliers de valeurs. Un appel sur un point tient autour de
900 tokens d'entrée ; la sélection est plafonnée à **8 points** par appel, et le dépassement
est signalé au modèle plutôt que tronqué en silence.

`ContextBuilder` calcule aussi le **recouvrement des périodes** et le dit explicitement :
comparer le minimum d'un site mesuré en janvier à celui d'un site mesuré en mars ne dit rien
des sites. C'est le biais le plus facile à commettre et le plus facile à détecter — donc il
est détecté en Rust, pas laissé à la sagacité du modèle.

### La réponse est du Markdown, et elle est rendue

Le modèle produit des puces et du gras **même quand on le lui interdit** — la consigne
« pas de Markdown » de la première version n'a pas tenu à l'usage, et le bloc affichait des
astérisques en clair au milieu des phrases. Deux corrections plutôt qu'une :

* `groq::CONSIGNES_FORMAT` **autorise** désormais explicitement ce que l'affichage sait
  rendre — puces, listes numérotées, `**gras**` — et interdit le reste (titres, tableaux,
  liens, blocs de code). Prompt et rendu doivent rester alignés : modifier l'un sans
  l'autre fait réapparaître le symptôme ;
* `src/markdown.js` convertit la réponse en **nœuds DOM** avant l'animation.

Pas de bibliothèque Markdown : le besoin tient en quatre constructions, et le projet n'a
aucun outillage JS (ni Node, ni bundler). Pas de `innerHTML` non plus — les nœuds sont
construits avec `createElement` / `createTextNode`, donc une balise écrite par le modèle
ressort comme du texte, jamais comme du balisage.

**Animer sans casser les balises.** Écrire du HTML caractère par caractère produirait
`<str` à mi-parcours. `GwsTypewriter.typeRich` fait l'inverse : l'arborescence est
construite entière et tout de suite, puis vidée de son texte ; l'animation ne fait que
redonner ses caractères dans l'ordre du document. Aucune balise n'est jamais coupée, parce
qu'aucune n'est jamais écrite. Les éléments dont aucun caractère n'est encore arrivé sont
masqués (`.tw-attente`) — sans quoi les puces d'une liste apparaîtraient toutes, vides, dès
le premier mot — et le curseur est porté par l'élément en cours de frappe (`.tw-curseur`),
pas par le conteneur, où il tomberait sous le dernier paragraphe.

Limites assumées : pas de listes imbriquées, pas de liens, pas de tableaux. Un délimiteur
non apparié reste affiché tel quel — mieux vaut un astérisque isolé visible qu'un bout de
phrase avalé par une règle trop gourmande.

### Les trois modes de déclenchement

1. **Boutons d'actions rapides**, reconstruits à chaque changement de sélection — l'UI
   n'offre que ce qui a du sens : « Comparer les points sélectionnés » (≥ 2 points),
   « Analyser cette série » (exactement 1), « Biais possibles », « Résumer l'état du
   projet » (toujours, et porte sur tout le projet, pas sur la sélection).
2. **Champ de question libre**, toujours disponible, avec le même contexte injecté.
3. **Déclenchement automatique** dès qu'une sélection d'au moins deux points change, avec
   un **debounce de 900 ms**. Ce n'est pas du confort : cocher cinq points à la souris
   produit cinq changements en deux secondes, soit cinq appels — de quoi épuiser le quota
   de la minute avant même que l'utilisateur ait fini de choisir. Deux garde-fous
   s'ajoutent : la signature de la sélection est mémorisée (revenir à la même paire ne
   relance rien), et une réponse arrivée après le départ d'une demande plus récente est
   **ignorée** plutôt qu'affichée par-dessus.

### Vérifier la clé contre l'API réelle

Comme pour Infoclimat, un test `#[ignore]` fait un vrai appel :

```bash
cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture appel_reel_groq
```

Il affiche le modèle, la consommation de tokens et la réponse.

---

## 6bis. Import assisté par IA (Fichiers structurés et Vision multimodale)

L'écran d'import (`#dlg-import`) intègre un onglet dédié **« ✨ Assistant IA (Fichiers & Images) »** (`ai_import.rs`) permettant d'importer rapidement des sources variées sans réglage manuel laborieux :

### 1. Analyse automatique de fichiers texte (CSV, TSV, JSON, TXT)
* **LLM utilisé** : `openai/gpt-oss-20b` via le client Groq existant (`groq.rs`).
* **Fonctionnement** : Un échantillon représentatif du fichier est envoyé au modèle avec les règles de structure météo. L'IA déduit le délimiteur, les lignes à ignorer, la ligne d'en-tête, et le mapping des colonnes (date/heure, température, humidité, pression, vent, pluie).
* **Consigne libre optionnelle** : L'utilisateur peut spécifier des instructions (ex: *"la colonne T_Air_2m est la température sous abri"*).
* **Validation humaine non contournable** : L'IA pré-remplit les contrôles. L'utilisateur peut ajuster les sélecteurs de colonnes, recalculer l'aperçu dynamique (`ai_preview_parsed`) et valide explicitement avant l'enregistrement effectif en base de données.

### 2. Import multimodal depuis des captures d'écran (Vision)
* **Modèle Vision** : `qwen/qwen3.8-27b` (avec repli de secours automatique sur `qwen/qwen3.6-27b`).
* **Glisser-déposer, sélection multiple et Presse-papier (Ctrl+V)** : Les captures d'écran de tableaux de bord, de logiciels anciens ou d'écrans de stations sont analysées visuellement pour en extraire les lignes de mesures.
* **Affichage côte à côte et tableau éditable** : L'image source originale est affichée face au tableau des données extraites. Chaque cellule (horodatage, température, humidité) est **directement éditable** par l'utilisateur pour corriger toute ambiguïté visuelle avant l'insertion en base (`import_ai` avec `DirectRows`).

---

## 7. Première ouverture, écran d'accueil et identité de l'utilisateur

### Les deux écrans de première ouverture

Une installation neuve traverse deux écrans avant la liste des projets, dans cet ordre :

| | Écran | Bloquant ? |
|---|---|---|
| 1 | **Clé API Groq** (desktop seulement) | non — « Passer, configurer plus tard » |
| 2 | **Prénom** | oui |

**Un seul signal les déclenche : l'absence de prénom enregistré.** Il n'y a pas de second
drapeau à tenir, et donc pas de moyen pour les deux écrans de se désynchroniser. Corollaire
assumé : quelqu'un qui passe l'étape 1 puis ferme l'application avant de donner son prénom
reverra les deux — ce qui est le comportement voulu, il n'a encore rien configuré.

L'étape 1 vient **avant** l'étape 2 parce que la clé est ce qu'un nouvel arrivant ne peut
pas deviner : la demander en premier lui laisse le temps de l'aller-retour sur le site de
Groq. Le prénom, lui, se remplit d'un trait.

Elle est **non bloquante**, et l'application reste entièrement utilisable sans clé : seul le
bloc « Agent IA » se déclare indisponible, exactement comme il le faisait déjà (§ 6).
Aucun appel n'est fait à Groq à cette étape — la validité d'une clé ne se prouve qu'en s'en
servant, et faire attendre le réseau ici n'apprendrait rien de plus qu'au premier usage.

**Desktop seulement.** Sur téléphone, `Clee.API/` vit dans le répertoire de données de
l'application et il n'y a pas de navigateur côte à côte pour aller chercher la clé :
l'écran y serait une étape de plus sans le geste qu'elle suppose. La première ouverture y
reste celle du prénom.

### Le lien « console.groq.com/keys »

Ce n'est **pas un `<a href>`**. Dans une webview, un lien ordinaire navigue la fenêtre
elle-même : l'application disparaît derrière la page Groq, sans barre d'adresse, sans
onglets, sans gestionnaire de mots de passe et sans moyen évident de revenir. C'est un
`<button class="link">` qui appelle `open_external_url`, laquelle passe par
`tauri-plugin-opener` et ouvre le **navigateur du système**.

Le greffon est monté pour son API Rust seule, comme `fs` et `usbserial` : aucune de ses
commandes n'est ouverte au frontend (cf. `capabilities/default.json`). `open_external_url`
n'accepte qu'une URL d'une **liste fermée** (`URL_AUTORISEES`, dans `commands.rs`) — les
pages de génération de clé de Groq et d'Infoclimat, rien d'autre. Ni `file://`, ni
`javascript:`, ni un exécutable. La règle est en Rust plutôt que dans un `allow-open-url`
scopé pour qu'un test la couvre.

L'adresse elle-même n'est écrite **qu'une fois**, dans `groq::KEY_SOURCE.signup_url` —
d'où sortent déjà les messages d'erreur de `api_keys` qui la citent. Le frontend la reçoit
via `agent_settings.key_signup_url` au lieu d'en garder une copie qui divergerait le jour où
Groq déplace sa console.

### Un seul chemin d'écriture pour la clé

L'écran de première ouverture et **Paramètres → Agent IA (Groq)** proposent tous deux de
renseigner la clé. Le doublon n'existe qu'à l'écran : les deux appellent `set_groq_key`,
qui appelle `groq::save_key`, qui appelle `api_keys::save_key`. Il n'existe donc aucun
chemin par lequel les deux écrans afficheraient des clés différentes.

Trois points méritent d'être notés dans `api_keys::save_key` :

* **le fichier qui existe déjà l'emporte** (`choose_key_file`). Sans cette règle, un
  enregistrement pourrait créer un second `Clee.API/API.Groq.env` ailleurs dans
  l'arborescence, que `load_key` ne lirait plus si un autre le précède dans la liste des
  candidats : l'utilisateur verrait « clé enregistrée », puis « aucune clé » dans les
  Paramètres. À défaut, on écrit dans le premier candidat — le répertoire de données de
  l'application, toujours accessible en écriture, ce que la racine du projet n'est pas une
  fois l'application installée dans `Program Files` ;
* **la validation reste syntaxique** (`sanitize_key`) : on refuse le vide, ce qui contient
  un espace (une copie qui a mal tourné) et ce qui dépasse 512 caractères (une page entière
  collée par mégarde). `extract_key` est réutilisée telle quelle, si bien qu'un collage de
  la forme `GROQ_API_KEY=gsk_…` depuis un tutoriel est accepté comme dans le fichier ;
* **`GWS_GROQ_TOKEN` reste prioritaire.** `load_key` regarde l'environnement avant le
  fichier. Sur un poste où la variable est posée, la clé fraîchement écrite ne servirait
  pas — `set_groq_key` le détecte et le dit, au lieu d'afficher une empreinte qui n'est pas
  celle qu'on vient de coller.

La commande renvoie un `AgentSettings` **relu depuis le disque**, jamais ce qu'on lui a
envoyé : les deux écrans affichent donc l'état réel. Et le champ de saisie des Paramètres
est toujours rendu vide — le backend ne divulgue jamais la clé en clair, seulement son
empreinte (§ 5, « La clé : où elle vit, et pourquoi elle ne fuite pas »).

### Le prénom

`app_settings.user_name` — la table clé/valeur existe depuis la migration v3, donc **aucune
migration de schéma** n'a été nécessaire. Il est demandé sur un écran dédié à la toute
première ouverture (formulaire nu, sans animation : c'est un prérequis à franchir, pas une
mise en scène), puis modifiable dans **Paramètres**. Il alimente les scripts de l'écran
d'accueil et le system prompt de l'agent.

### L'écran d'accueil

Il s'intercale entre la liste des projets et la grille 2×2, à l'ouverture d'un projet :

1. un script tiré au hasard s'écrit **lettre par lettre** ; un clic sur le texte le termine
   d'un coup pour qui l'a déjà lu ;
2. le texte terminé, les trois boutons apparaissent : **Importer des données**, **Créer un
   point d'enregistrement**, **Explorer le studio** ;
3. l'un d'eux se **remplit visuellement sur 7 secondes** — « Explorer le studio » par
   défaut ;
4. au bout des 7 secondes, son action se déclenche seule. N'importe quel clic annule le
   compte à rebours, y compris sur un autre bouton ;
5. les trois actions mènent à la grille 2×2 avec le **même reveal animé** qu'au prompt 3
   (`playReveal`, réutilisé et non dupliqué) ; elles ne diffèrent que par ce qui s'ouvre
   par-dessus.

Il n'y a **aucune barre de saisie** sur cet écran : on n'y répond que par les trois boutons
ou par le délai.

Pour que `{last_opened}` ne dise pas invariablement « à l'instant », la commande
`open_project` renvoie `previous_opened_at` **en plus** du projet : `touch_project` écrase
`last_opened_at` avec l'instant courant, et il fallait capturer la valeur d'avant.

### La structure des scripts — ⚠️ jeu d'exemple à remplacer

`src/home_scripts.js` contient **dix scripts d'exemple**. Ils existent pour que le mécanisme
tourne et pour fixer le ton ; ils sont destinés à être **remplacés par la liste définitive**
(environ cinquante scripts).

Chaque script est un objet :

```js
{
  text: "{name}, {project_name} compte {point_count}. Dernière session {last_opened}.",
  defaultAction: "explore",   // optionnel — "import" | "create" | "explore"
}
```

| Placeholder | Rendu |
|---|---|
| `{name}` | le prénom saisi (§ ci-dessus) |
| `{project_name}` | nom du projet |
| `{point_count}` | « aucun point », « 1 point », « 12 points » |
| `{last_opened}` | « à l'instant », « hier », « il y a 3 jours », puis date absolue au-delà d'une semaine |

**Pour en ajouter** : une ligne de plus dans le tableau `SCRIPTS`. C'est tout — le tirage,
le remplacement des placeholders et l'anti-répétition (le script précédent est exclu du
tirage suivant) sont déjà en place.

`defaultAction` désigne le bouton qui se remplit. Tous les scripts d'exemple s'en remettent
au défaut (`explore`) ; le champ existe pour qu'un script de la liste définitive puisse
mener ailleurs — un script qui parle d'un projet vide a plus de sens s'il pousse vers
« Créer un point d'enregistrement ». Un placeholder inconnu est laissé tel quel (`{machin}`)
plutôt que vidé : la faute de frappe se voit, alors qu'un trou dans une phrase se lit comme
un bug d'affichage.

### Le composant « machine à écrire » est écrit une fois

`src/typewriter.js` sert **à la fois** l'écran d'accueil et le bloc Agent IA. Les deux
usages ont des contraintes opposées — l'accueil écrit une phrase courte et attend la fin
pour afficher ses boutons ; l'agent écrit un paragraphe et doit pouvoir être interrompu par
une nouvelle demande — d'où une API unique qui renvoie `{ promise, skip, cancel, done }`.
Un appel sur un nœud déjà animé **annule le précédent** : c'est ce qui empêche deux
réponses de s'entrelacer dans le même bloc. `prefers-reduced-motion` affiche le texte
immédiatement.

Deux points d'entrée pour une seule mécanique de temporisation : `type()` écrit une chaîne,
`typeRich()` révèle une arborescence déjà construite (le Markdown de l'agent, § 6). Le
module ne connaît ni Markdown ni HTML — il ne sait que redonner des caractères là où ils
sont ; c'est l'appelant qui fournit le DOM.

La barre de remplissage des 7 secondes est, elle, une **exception assumée** à la réduction
de mouvement : elle n'est pas un ornement mais le seul indice visible qu'une action va se
déclencher toute seule. La supprimer laisserait l'utilisateur devant un écran qui bascule
sans prévenir.

## 8. Décisions d'architecture (à discuter)

* **`rusqlite` + feature `bundled` plutôt que `tauri-plugin-sql`.** Toute la logique reste
  en Rust typé et testable sans lancer l'application ; SQLite est compilé depuis les
  sources, donc aucune DLL à installer et une version identique sur tous les postes.
* **Pas de `tauri-plugin-dialog` / `tauri-plugin-fs`.** Le fichier est lu par un
  `<input type="file">` et décodé en texte côté frontend, puis envoyé au Rust. Un plugin
  et deux jeux de permissions en moins. **Contrepartie assumée** : `raw_file_reference` ne
  contient que le *nom* du fichier, pas son chemin absolu (le navigateur ne l'expose pas).
  Si la traçabilité du chemin complet devient nécessaire, il faudra basculer sur le plugin
  `dialog`.
* **UUID en `TEXT` pour les entités, entier auto-incrémenté pour les mesures.** Les UUID
  permettront de fusionner deux bases sans collision ; une mesure n'étant jamais référencée
  de l'extérieur, lui donner un UUID coûterait cher pour rien.
* **Facteurs de site en `Option<String>`.** Le format de saisie (note /5 ? pourcentage ?
  texte libre ?) n'est pas figé : on stocke du texte libre pour ne rien perdre, on typera
  quand la grille de notation sera arrêtée.
* **Aucune fusion de séries, mais plus de doublons *dans* une série.** Réimporter deux fois
  le même fichier crée toujours deux séries — « on ne fusionne jamais rien
  automatiquement ». En revanche `UNIQUE(series_id, timestamp_utc)` (v3) interdit qu'une
  même série porte deux mesures au même instant : indispensable dès lors qu'une source est
  *réinterrogeable* (§ 5). Il faudra probablement une suppression de série, non implémentée
  à ce stade.
* **`reqwest` en mode bloquant plutôt qu'un second client HTTP.** `reqwest` est déjà dans
  l'arbre de dépendances de Tauri (sans TLS) : le réutiliser coûte bien moins de
  compilation qu'ajouter `ureq` et sa pile TLS — ce qui compte sur un portable 8 Go. La
  feature `native-tls` s'appuie sur SChannel sous Windows, donc **aucun OpenSSL à
  compiler**. Le mode `blocking` évite de teindre tout le métier en `async` ; les appels
  réseau partent dans un `spawn_blocking` depuis la commande Tauri, la fenêtre ne se fige
  pas.
* **Inter vendorisée en local** plutôt qu'un lien Google Fonts : l'application doit
  fonctionner dans un refuge sans réseau. La CSP interdit d'ailleurs toute requête externe.

---

## 9. Carte

Bloc « Carte géographique » de la grille (haut-gauche). **Leaflet 1.9.4 est vendorisé** dans
`src/vendor/leaflet/` : la CSP de l'application interdit tout script externe, et l'app doit
fonctionner dans un refuge sans réseau. Seules les *tuiles* sortent sur le réseau.

### Fonds de carte — deux fonds IGN, deux niveaux de fiabilité

Un bouton en bas à droite de la carte bascule de l'un à l'autre (il affiche le nom du fond
vers lequel il fait basculer).

| Fond | Couche | Point d'entrée | Clé | Fiabilité |
|---|---|---|---|---|
| **Photo aérienne** | `HR.ORTHOIMAGERY.ORTHOPHOTOS` | `data.geopf.fr/wmts` | aucune | ✅ **fiable** — service public ouvert |
| **TOP25** | `GEOGRAPHICALGRIDSYSTEMS.MAPS.SCAN25TOUR` | `data.geopf.fr/**private**/wmts` | `apikey=ign_scan_ws` | ⚠️ **fragile** — clé provisoire partagée |

Le TOP25 (SCAN25) est la carte topographique au 1:25 000, **avec les courbes de niveau** :
c'est elle qui permet de repérer une doline. C'est aussi la seule des deux qui puisse
cesser de fonctionner du jour au lendemain.

#### ⚠️ Le jour où le TOP25 ne charge plus

`ign_scan_ws` est une **clé provisoire publique** mise à disposition par l'IGN au titre d'un
accord de droits partagés. Elle n'est garantie ni dans le temps, ni en volume.

L'application ne se contente pas d'afficher un rectangle gris : un tile layer qui échoue
trois fois d'affilée **sans aucun succès** déclenche un bandeau rouge sous la carte
(« Fond TOP25 indisponible — la clé d'accès IGN a peut-être expiré… »). Le seuil de trois
évite qu'une tuile perdue sur un réseau instable ne crie au loup ; le bandeau disparaît de
lui-même dès qu'une tuile repasse.

**Pour remplacer la clé** par une vraie clé de compte <https://cartes.gouv.fr> :

```js
// src/map.js, tout en haut du module — SEULE ligne à changer
const CLE_TOP25 = "ign_scan_ws";
```

Rien d'autre à toucher : ni l'URL, ni la CSP (le domaine `data.geopf.fr` ne change pas),
ni la logique de repli.

#### Ce qui a été testé, le 18 août 2026

Sur une tuile du Vercors (45.05, 5.45), en `TILEMATRIXSET=PM` :

| Couche | Point d'entrée | Résultat |
|---|---|---|
| `HR.ORTHOIMAGERY.ORTHOPHOTOS` | `/wmts`, sans clé | **200** — retenue |
| `ORTHOIMAGERY.ORTHOPHOTOS` | `/wmts`, sans clé | 200 également : les deux variantes marchent, `HR.` a été gardée |
| `GEOGRAPHICALGRIDSYSTEMS.MAPS.SCAN25TOUR` | `/private/wmts` + `apikey` | **200** — retenue (courbes de niveau vérifiées à l'œil sur la tuile) |
| `GEOGRAPHICALGRIDSYSTEMS.MAPS.SCAN25TOUR` | `/wmts`, sans clé | 400 « Layer unknown » |
| `GEOGRAPHICALGRIDSYSTEMS.MAPS.SCAN25TOUR` | `/private/wmts`, sans clé | 401 Unauthorized |
| `GEOGRAPHICALGRIDSYSTEMS.MAPS` (repli prévu) | `/private/wmts` + `apikey` | 200 — **n'a pas eu à servir**, le SCAN25TOUR répond |

Deux pièges que ce tableau explique, et qui coûteraient une soirée à retrouver :

1. **Le TOP25 n'est pas sur `/wmts` mais sur `/private/wmts`.** Interroger le point d'entrée
   public renvoie « couche inconnue », ce qui ressemble à une faute de frappe dans le nom de
   la couche alors que c'est la bonne couche au mauvais endroit.
2. **Le zoom natif du SCAN25TOUR s'arrête à 16** (au-delà : `404 No data found`). D'où
   `maxNativeZoom: 16` sur la couche : Leaflet ré-échantillonne la tuile 16 au lieu de
   réclamer des tuiles inexistantes. Sans ce réglage, zoomer à fond produirait une rafale de
   404 — et donc un **faux** bandeau « fond indisponible » alors que le service va bien.
   (L'orthophoto, elle, monte jusqu'au zoom 19 : `maxNativeZoom: 19`.)

**Usage à surveiller** : ces services sont gratuits pour un usage raisonnable, sans
engagement contractuel de notre côté. Un usage familial (2-3 postes) est très loin des
seuils, mais les conditions peuvent changer — si un fond cesse de répondre, c'est
`fondsDeCarte()` dans `src/map.js` qu'il faut regarder.

**Ajouter un fond implique de modifier la CSP** (`img-src` dans `src-tauri/tauri.conf.json`).
Un domaine oublié se traduit par des **tuiles muettes, sans aucune erreur Rust** — le seul
symptôme est une carte grise.

### Le dézoom sur Android — le correctif du prompt 10, revérifié sur mobile

Le gel de l'interface au dézoom sur une zone non cachée avait deux causes, corrigées côté
desktop : un délai réseau ramené à **2 s** par tuile, et surtout `get_tile` rendue `async`
avec `spawn_blocking`. Ce second point était le vrai coupable — une commande Tauri
*synchrone* s'exécute sur le pool restreint des workers de l'exécuteur asynchrone, pas sur
le pool bloquant ; au dézoom, Leaflet réclame des dizaines de tuiles d'un coup, chacune par
un appel réseau bloquant, et elles se sérialisaient.

Ce correctif ne dépend d'aucune plateforme : le pool bloquant de Tokio se comporte de la
même façon sur Android. Restait à le vérifier, parce que le réseau mobile, lui, ne se
comporte pas comme un Wi-Fi de bureau. **Il l'a été**, sur émulateur, en rafale de 40 tuiles
non cachées avec une sonde de réactivité lancée pendant la rafale :

| Situation | 40 tuiles | Sonde de réactivité pendant la rafale |
|---|---|---|
| Wi-Fi normal | **993 ms**, 40 réussies | **2 ms** |
| Aucun réseau (zone blanche) | **57 ms**, 40 échouées proprement | **2 ms** |

La sonde est une commande triviale (`list_projects`) émise 150 ms après le début de la
rafale. Si le pool était saturé — le symptôme du bug d'origine — elle attendrait son tour.
À 2 ms, il ne l'est pas : les tuiles ne se sérialisent pas, et l'interface ne se fige pas.
Le cas hors ligne est celui pour lequel le délai de 2 s existe : 40 échecs en 57 ms, la
carte se grise immédiatement au lieu d'attendre.

⚠️ **Un cas n'a pas pu être reproduit** : le réseau **lent**, celui qui traîne sans couper —
une 4G qui décroche à moitié. L'AVD ignore `adb emu network delay` et `network speed` sur
son interface Wi-Fi (mesuré : 1 766 ms pour une tuile avec un délai de 4 000 ms demandé),
et l'image système ne permet pas `adb root`, donc pas de trou noir DNS non plus. C'est
précisément le scénario qui avait produit le gel d'origine : il **reste à vérifier sur un
appareil physique**, en 4G, en dézoomant sur une zone jamais visitée.

### Gestes sur la carte

Ce point n'était pas spécifié en amont : le choix est documenté ici parce qu'il est
arbitraire et qu'il doit rester stable.

| Geste | Effet | Pourquoi ce choix |
|---|---|---|
| **Clic gauche** sur un marqueur | **sélectionne / désélectionne** (croisement de données) | c'est le geste le plus fréquent une fois les stations posées |
| **Double-clic** sur un marqueur | ouvre le formulaire d'édition | l'édition est ponctuelle : elle peut coûter un geste de plus |
| **Clic droit** sur un marqueur | menu « Modifier ce point » / « Ajouter à la sélection » | le double-clic n'est pas découvrable : ce menu est la porte visible |
| **Clic droit** sur le vide | menu « Créer un point ici » (préremplit lat/lon) | inchangé depuis le prompt 2 |
| **Clic** sur une ligne de la liste Stations | sélectionne / désélectionne | même objet, même geste des deux côtés |
| Boutons **Détail** / **Modifier** de la liste | fiche de la station / édition | l'édition ne doit jamais dépendre de la carte |

> **Le clic simple est temporisé de 250 ms.** Leaflet émet un `click` avant tout `dblclick` :
> sans ce sursis, un double-clic sélectionnerait puis désélectionnerait le point au passage.
> La contrepartie est que l'anneau de sélection apparaît un quart de seconde après le clic.

Un point sélectionné garde **sa couleur de catégorie au centre** — l'information la plus
utile ne doit pas être écrasée par un état d'interface. C'est l'anneau qui change : blanc
fin au repos, épais une fois sélectionné. La liste des stations affiche le même état par un
liseré bleu à gauche de la ligne.

**La couleur de cet anneau est celle de la courbe du point dans le bloc « Croisement de
données ».** C'est la seule correspondance point → couleur de l'application, et elle est
nécessaire : la couleur de catégorie ne distingue pas deux dépressions fermées, qui sont
précisément ce qu'on compare le plus souvent. `main.js` calcule l'attribution une fois
(`GwsCross.paletteFor`) et la pousse aux deux vues — `GwsMap.setSelectionColors` d'un côté,
le tableau comparatif de l'autre. La carte ignore donc toujours l'existence de `cross.js` :
elle reçoit des couleurs comme elle reçoit ses rappels, et reste le module autonome décrit
en tête de `map.js`.

La sélection est lisible par `GwsMap.getSelection()` et poussée à `main.js` par le rappel
`onSelectionChange`, qui la range dans `state.selectedIds`. Une station devenue
impositionnable est retirée de la sélection au rechargement — sans quoi le croisement
porterait sur un id sans objet.

### Autres comportements

* **Zoom molette centré sur le curseur** — comportement par défaut de Leaflet (seul
  `scrollWheelZoom: 'center'` recentrerait). Explicité en commentaire pour que personne ne
  le « corrige ».
* **Hitbox généreuse** : deux cercles superposés par point — une cible invisible de 16 px
  pour le clic, un marqueur discret de 6 px pour l'œil.
* **Recadrage automatique** sur l'ensemble des points, sinon Vercors (45.05, 5.45) au
  zoom 11. Le recadrage se protège d'un conteneur pas encore mesuré : `fitBounds` sur une
  carte de taille nulle ne lève aucune erreur, elle retombe au zoom 0 et affiche le monde.
* **Point sans position exploitable** (`(0, 0)`, valeurs non finies) : aucun marqueur, pas
  de participation au cadrage, et un bandeau sous la carte. Le point reste modifiable
  depuis la liste — l'édition ne dépend pas de la carte.

> Les **couleurs de catégorie sont provisoires** (`COULEURS_CATEGORIE` dans `map.js`). Ce ne
> sont pas des couleurs de charte : elles n'existent que pour rendre la carte lisible en
> attendant les vrais pictogrammes, qui restent un chantier séparé. Ne rien construire
> dessus.


---

## 10. Interface : la grille 2×2 et la direction artistique

### Disposition

Une **barre supérieure fine** (icône Accueil à gauche, nom de l'application et projet
courant au centre, icône Paramètres à droite), puis quatre zones **fixes** : ni panneaux
flottants, ni tiroirs rétractables. Chaque bloc reste au même endroit d'une séance à
l'autre.

```
┌────┬────────────────────────────────────────────┬────┐
│ 🏠 │      Gastinel Weather Studio · Projet       │ ⚙️ │
├────┴───────────────────────────────┬────────────┴────┤
│                                     │  STATIONS /      │
│   CARTE GÉOGRAPHIQUE (62 %)         │  EXPORT (38 %)   │
│                                     │  ← page 1/2 →    │
├──────────────────┬──────────────────┴──────────────────┤
│  CROISEMENT       │      AGENT IA (60 %)                │
│  (40 %)           │                                     │
└───────────────────┴─────────────────────────────────────┘
```

Les deux rangées ont volontairement des proportions différentes : en haut la carte domine,
en bas c'est l'agent qui a besoin de place pour du texte. Mesuré dans la fenêtre par défaut
(1280 × 840) : carte 771 px, stations 473 px et **même hauteur** que la carte ; croisement
498 px, agent 746 px.

> `minmax(0, …)` sur toutes les pistes de la grille. Sans cette borne basse, une grille CSS
> refuse de descendre sous la taille intrinsèque de son contenu : une liste de stations un
> peu longue pousserait la rangée hors de l'écran au lieu de faire défiler son seul bloc.

### État des quatre blocs

| Bloc | État |
|---|---|
| **Carte géographique** | complet (cf. § 9) |
| **Stations / Export** | page 1 complète, import **Elitech (xlsx ou texte)** / manuel / **Infoclimat (JSON ou CSV)** / **ROMMA** ; page 2 = **UI d'export seule, non fonctionnelle** |
| **Croisement de données** | **complet** (cf. § 11) — tableau comparatif + courbes superposées, sélecteur de fenêtre temporelle |
| **Agent IA** | **complet** — Groq `openai/gpt-oss-20b`, trois modes de déclenchement (cf. § 6) |

Les boutons « Exporter en CSV / TXT / HTML » affichent « à venir » et n'écrivent aucun
fichier (`TODO(export)` dans `main.js`). Leur source de données est cependant prête :
`crossing::CrossAnalysis` est un document complet et autonome (§ 11).

### La fiche d'une station est passée en modale

Le détail d'une station (séries, statistiques, **confirmation de l'ordre jour/mois**) était
une cinquième carte empilée sous la liste. Les quatre zones de la grille étant fixes et
attribuées, cette fiche est devenue une modale ouverte par le bouton « Détail ».
**Rien n'y a été retiré** — en particulier la résolution du doute sur les dates (§ 4), qui
reste le seul endroit où une donnée peut être fausse sans que rien ne plante.

La liste des stations est aussi passée du tableau à une **liste** : à 38 % de largeur, six
colonnes ne tiennent pas sans défilement horizontal. Catégorie et taille restent affichées.

C'est aussi cette fiche qui porte le rattachement Infoclimat : une barre en tête affiche
l'identifiant de la station et le bouton **« Récupérer les dernières données »**, ou — s'il
n'y a pas d'identifiant — une phrase indiquant où le renseigner. Masquer le bouton sans un
mot en ferait une fonction qu'on cherche ensuite pendant dix minutes. L'attribution
« Données Infoclimat » apparaît en pied de chaque série concernée (§ 5).

### Écran Paramètres

Il n'est plus vide. Il expose, en trois groupes :

* **Vous** — le **prénom** (§ 7), seul champ modifiable qui ne concerne pas une API ;
* **Agent IA (Groq)** — le statut de la clé Groq, un **champ pour la renseigner ou la
  remplacer** (même commande que l'écran de première ouverture, cf. § 7) et le modèle
  utilisé, avec le rappel du plafond de 8 000 tokens/minute qui explique les refus (§ 6) ;
* **API Infoclimat** — l'**intervalle minimal entre deux rafraîchissements automatiques**
  (60 min par défaut) et le statut de la clé Infoclimat.

Les deux statuts de clé partagent le même rendu (`renderKeyStatus`), parce que les deux
clés arrivent sous la même forme (`KeyStatus`, produit par `api_keys.rs`) : « configurée »
avec ses quatre derniers caractères, sa longueur et le chemin du fichier lu, ou « absente »
avec l'explication. **Jamais la clé en clair** : le backend ne la sérialise pas, un test le
vérifie.

Le champ de la clé Groq a son **propre bouton**, séparé du « Enregistrer » commun aux autres
réglages : on veut voir l'empreinte se mettre à jour immédiatement pour être sûr d'avoir
collé la bonne. Il est toujours rendu vide — pré-remplir une empreinte donnerait l'illusion
qu'on peut la modifier caractère par caractère, alors qu'on colle une clé entière ou on ne
touche à rien. Le laisser vide et cliquer n'efface donc pas la clé en place ; c'est dit dans
le message.

Un bandeau y rappelle la contrainte d'IP **avant** que le problème n'arrive. Les statuts
sont relus à chaque ouverture de l'écran : c'est exactement le geste qu'on fait après avoir
remplacé une clé. Le chargement est **attendu avant l'ouverture** de la modale depuis que
l'écran porte un champ éditable — ouvrir d'abord afficherait la valeur précédente, que le
chargement écraserait un instant plus tard, au milieu de la frappe.

### Direction artistique

* **Inter partout**, vendorisée en local (`src/fonts/`), titres et corps compris — y compris
  sur les contrôles Leaflet, qui embarquent sinon leur propre pile de polices.
* **Chiffres tabulaires** (`font-variant-numeric: tabular-nums`) sur toute donnée chiffrée :
  températures, statistiques, altitudes, coordonnées, champs numériques. Sans cela deux
  colonnes de températures ne s'alignent pas et la comparaison visuelle devient trompeuse.
* **Palette** : fond blanc cassé (`#fafbfc`), accent bleu `#1e4d8b`, gris clair `#e8eaed`.
  Le dégradé de marque (`--gradient-marque`) reprend celui de l'icône : bleu foncé → gris.
* **Icône** : la cuvette en U (dégradé bleu→gris) est dans `src-tauri/icons/`, référencée par
  `bundle.icon` dans `tauri.conf.json` — Tauri en fait à la fois l'icône de l'exécutable et
  celle de la fenêtre. Le nom « Gastinel Weather Studio » est le titre de la fenêtre **et**
  s'affiche dans la barre supérieure.

### Animations

Réservées aux **changements d'état structurants**. Durées centralisées en variables CSS.

| Transition | Effet | Durée |
|---|---|---|
| Ouverture d'un projet | les 4 blocs apparaissent en fondu + glissement de 8 px, décalés de 55 ms | 320 ms |
| Ouverture / fermeture d'une modale | fondu + léger agrandissement (0,97 → 1) | 180 ms |
| Bascule de page du bloc Stations | glissement latéral + fondu, dans le sens de la navigation | 220 ms |

**Rien sur le survol des boutons.** C'est l'interaction la plus répétée de l'application ;
120 ms d'attente multipliés par une séance de dépouillement se ressentent comme de la
lourdeur, pas comme de l'élégance. La transition qui existait sur `.btn:hover` a été retirée.

Le fondu d'ouverture est déclenché par une classe posée puis retirée par `main.js` : la
laisser en place ferait rejouer l'animation au moindre recalcul de style, et les blocs
clignoteraient pendant l'usage courant. Tout est neutralisé sous
`@media (prefers-reduced-motion: reduce)` — le mouvement est un confort, jamais une
information.

> Les modales natives `<dialog>` s'animent grâce à `@starting-style` et
> `transition-behavior: allow-discrete` : sans eux, `display: none` couperait net la
> transition de sortie. Sur un moteur qui ne les connaît pas, la modale s'affiche
> instantanément — dégradation sans casse.

---

## 11. Croisement de données

Le bloc bas-gauche compare les points cochés sur la carte. Son objet n'est pas d'aligner des
chiffres — un tableur le fait très bien — mais de montrer **ce qu'un tableau de moyennes ne
montre pas** : la forme des courbes, superposées, sur la même période.

### Deux zones, jamais paginées

Statistiques en haut, graphique en bas, **visibles en même temps**. Une bascule entre deux
pages aurait obligé à mémoriser les chiffres pour lire la courbe.

| Zone | Contenu |
|---|---|
| Haut | Une ligne par point : nom, catégorie et taille, minimum extrême, moyenne des minimums quotidiens, amplitude moyenne, amplitude maximale |
| Bas | Une courbe par point, superposées, agrégées à la granularité de la fenêtre |

Les deux zones sortent du **même** vecteur d'échantillons, lu une seule fois par point dans
`crossing::build`. Il est structurellement impossible qu'elles portent sur deux périodes
différentes — c'était le risque à écarter en premier, une moyenne sur neuf ans posée
au-dessus d'une courbe de sept jours ayant toutes les apparences de la rigueur.

Les températures du tableau sont affichées **sans unité**, rappelée une fois en note. Ce
n'est pas de la coquetterie : le « °C » répété vingt fois coûte une trentaine de pixels par
colonne, et sur un bloc large de 40 % de la fenêtre c'est ce qui faisait passer la cellule
de nom sur trois lignes. Les en-têtes sont abrégés pour la même raison, leur sens complet
vivant dans leur infobulle ; l'infobulle de chaque ligne porte le détail intégral (altitude,
bornes de la fenêtre, dates des extrêmes, couverture totale du point).

### La fenêtre temporelle, et son ancre

Un sélecteur commande **les deux zones** : 7 derniers jours, 30 derniers jours, année de la
dernière mesure, tout l'historique, ou une plage saisie à la main.

⚠️ **Les fenêtres relatives se comptent à partir de la dernière mesure disponible dans la
sélection, pas à partir de l'horloge du poste.** La lecture littérale (« les 7 jours qui
précèdent maintenant ») rendrait le bloc inutilisable sur son cas d'usage principal : un
logger relevé une fois par saison, dont le dernier fichier date de mars, afficherait un
graphique vide en août.

La contrepartie, c'est qu'un libellé seul serait trompeur. **La fenêtre résolue est donc
toujours affichée en toutes lettres** sous le sélecteur, avec la granularité retenue :
`20/07/2026 → 19/08/2026 UTC · moyennes quotidiennes`. Une plage personnalisée, elle, n'est
jamais rognée sur les données disponibles : c'est une demande explicite, et la voir revenir
vide est l'information utile.

`crossing::resolve_window()` est de l'arithmétique pure — aucune base de données — et porte
neuf tests, dont les bornes saisies à l'envers (remises dans l'ordre plutôt que rejetées) et
le seuil d'annualisation. Ce dernier vaut **365 jours** : en dessous, l'en-tête dit
« Moy. min » et non « Moy. min ᵃⁿ », parce que la moyenne des minimums quotidiens sur sept
mois n'est pas une moyenne annuelle.

### Agrégation : la granularité suit la fenêtre, jamais le volume

Un point relié à ROMMA porte 476 912 mesures. Superposer huit courbes de cette taille, c'est
3,8 millions de points à sérialiser en JSON, à faire traverser l'IPC Tauri, puis à
rastériser dans un panneau de 400 px de large. **On n'envoie donc jamais la mesure brute au
frontend**, mais des seaux calculés par `stats::aggregate_curve()` :

| Étendue de la fenêtre | Granularité | Seaux (majorant) |
|---|---|---|
| ≤ 10 jours | horaire | 240 |
| ≤ 400 jours | quotidienne | 400 |
| ≤ 3 ans | hebdomadaire | ~157 |
| au-delà | mensuelle | ~12 / an |

Trois propriétés à conserver :

* **la granularité ne dépend que de l'étendue demandée**, jamais du nombre de mesures
  trouvées dedans. C'est ce qui garantit que deux points aux pas d'échantillonnage
  différents — un logger toutes les 10 min, une station horaire — produisent des courbes
  **alignées sur les mêmes seaux**. Sans cela la superposition ne voudrait rien dire ;
* **les frontières sont calendaires** : un seau quotidien commence à minuit UTC, un
  hebdomadaire un lundi, un mensuel le 1er. L'adaptateur de dates du graphique utilise la
  même convention de semaine, sinon l'axe se décalerait d'un jour ;
* **un seau vide n'apparaît pas**. La courbe porte ses trous, elle ne les interpole pas :
  côté frontend, un écart de plus d'une fois et demie la largeur d'un seau insère une
  rupture explicite. Un logger arrêté six mois afficherait sinon un segment parfaitement
  droit d'un hiver à l'autre — une donnée inventée, et la plus crédible qui soit.

Chaque seau porte **moyenne, minimum et maximum**. La moyenne seule lisserait précisément ce
qu'on cherche : sur un seau quotidien, un TAF se reconnaît à son minimum. La courbe trace la
moyenne, l'infobulle donne les trois valeurs et le nombre de mesures.

### Le minimum extrême est donné deux fois, et c'est voulu

La colonne « Min ext. » porte sur la **fenêtre affichée**, comme tout le reste du tableau.
Mais s'en tenir là ferait disparaître le record de l'hiver précédent dès qu'on regarde les
trente derniers jours. La sous-ligne de chaque point porte donc aussi `rec. −16,8`, le
minimum sur **tout l'historique** du point — affiché seulement quand il ajoute quelque
chose, c'est-à-dire quand la fenêtre ne le contient pas déjà.

### Mesures de réactivité

Chiffres relevés le 20 août 2026, build `--release`, **base SQLite sur disque** (une base en
mémoire aurait masqué le coût de lecture des pages, qui est justement ce qu'on veut
chiffrer). Jeu d'essai : un point de 476 912 mesures sur neuf ans plus onze points de
6 480 mesures, généré par `cargo run --release --example dump_cross_fixtures`.

| Cas | Durée | JSON transmis |
|---|---|---|
| 3 points « logger », 30 jours | 6 ms | 15 Ko |
| 8 points « logger », 30 jours | 20 ms | 40 Ko |
| Point ROMMA seul, 7 ou 30 jours | ~85 ms | 5 Ko |
| Point ROMMA seul, tout l'historique | 260 ms | 17 Ko |
| **8 points dont ROMMA, tout l'historique** | **283 ms** | **24 Ko** |

Le pire cas réaliste tient donc sous les 300 ms, et le JSON transmis reste sous 25 Ko quelle
que soit la taille des séries — c'est tout l'objet de l'agrégation. Le plancher de ~85 ms
sur les fenêtres courtes vient du record de froid, qui balaie tout l'historique du point
(cf. § 12).

Le bloc se recharge sur changement de sélection avec une temporisation de **250 ms**, contre
900 ms pour l'agent IA. Les deux protègent de la même rafale — cocher cinq points à la
souris produit cinq changements en deux secondes — mais pas du même coût : l'agent consomme
un quota d'API distant, ce bloc lit une base locale. Pendant un calcul, le contenu précédent
est grisé et non vidé : le remplacer par « chargement… » ferait clignoter la comparaison à
chaque clic sur la carte.

### 0, 1, ou beaucoup de points

* **0 point** — le message d'invitation, dans les deux zones.
* **1 point** — les statistiques **et** la courbe s'affichent quand même. Une série seule
  vaut la peine d'être regardée, et refuser de la tracer aurait été une contrainte
  arbitraire ; une note invite à cocher un second point pour comparer.
* **au-delà de 8** — un avertissement de lisibilité, **sans rien bloquer** : les couleurs se
  répètent et les tracés se recouvrent, mais l'utilisateur reste seul juge de ce qu'il
  arrive à lire. Même seuil que le plafond de l'agent (§ 6), pour une raison différente :
  l'agent **tronque** faute de budget de tokens, le croisement se contente d'avertir.

Un point sélectionné qui ne porte aucune mesure sur la fenêtre reste affiché, avec sa
couverture réelle : « rien sur cette période » est une information, disparaître n'en est pas
une.

### La bibliothèque de graphique : Chart.js 4.4.7, vendorisée

**Pourquoi une bibliothèque plutôt qu'un tracé SVG maison** : infobulle multi-séries,
légende, redimensionnement et détection de survol représentent l'essentiel du travail, et
sont exactement ce qu'une bibliothèque éprouvée fournit.

**Pourquoi vendorisée et pas en CDN** : la CSP de l'application est `script-src 'self'`
(`tauri.conf.json`), et l'app doit fonctionner dans un refuge sans réseau. Même traitement
que Leaflet au prompt 2 : `src/vendor/chartjs/chart.umd.js`, 206 Ko, licence MIT conservée à
côté. Le commentaire `sourceMappingURL` final a été retiré — la carte n'est pas distribuée,
et le laisser produisait un 404 dans la console des devtools.

**Pourquoi un adaptateur de dates écrit à la main** : l'échelle `time` de Chart.js délègue
toute manipulation de date à un adaptateur, et les adaptateurs officiels embarquent une
bibliothèque complète (date-fns, Luxon ou Moment) — 40 à 200 Ko et une seconde dépendance à
suivre, pour un usage qui tient dans les méthodes `Date.prototype.*UTC*` du navigateur.
`src/vendor/chartjs/adapter-utc.js` fait ~190 lignes et travaille **en UTC strict**. Ce
n'est pas une simplification : c'est la contrainte n°1 du projet (§ 3). Un adaptateur en
heure locale décalerait l'axe d'une ou deux heures selon la saison, et un minimum nocturne
relevé à 05 h UTC apparaîtrait à 07 h en été.

Deux réglages méritent d'être notés :

* **aucune animation**, et **aucun lissage** (`tension: 0`). Une courbe de température ne
  doit pas être embellie par une interpolation qui inventerait des creux entre deux seaux ;
* **pas de légende Chart.js**. Le tableau du dessus en est une — même pastille de couleur
  devant chaque nom, toujours visible puisque le bloc ne pagine pas. En doubler une dans le
  graphique coûtait de 30 à 70 px selon le nombre de points, sur une zone qui en fait 180 :
  la moitié du tracé échangée contre une redite.

### Découpage du code

| Fichier | Rôle |
|---|---|
| `stats.rs` | `TemperatureSample`, `TempSample`, `Granularity`, `pick_granularity()`, `aggregate_curve()` — pur, sans base de données |
| `db.rs` | `temperature_samples_for_point()` (deux colonnes, filtrage temporel poussé dans SQL), `point_period_bounds()`, `point_record()` |
| `crossing.rs` | Résolution de fenêtre et assemblage du document `CrossAnalysis` |
| `commands.rs` | `cross_analysis` — trois lignes, elle traduit et ne décide pas |
| `src/cross.js` | Rendu. **Aucun calcul statistique** : tout arrive agrégé |

`crossing::build()` ne connaît ni Tauri ni le DOM, et produit un document complet et
autonome : métadonnées des points, statistiques de fenêtre, courbes. **C'est la source de
données de l'export à venir** (CSV / TXT / HTML), qui n'aura ni à relire la base ni à
recalculer une statistique. Côté frontend, `GwsCross.lastAnalysis()` expose la dernière
réponse pour la même raison.

`cargo run --release --example dump_cross_fixtures -- <dossier>` écrit sur disque, en JSON,
ce que la commande renvoie réellement pour huit cas de figure, et imprime les mesures du
tableau ci-dessus. C'est ce qui a servi à éprouver le rendu avec 0, 1, 2, 3, 8 et 12 points
sans importer 38 Mo à la main.

⚠️ **Deux pièges rencontrés, qui ne se voient pas à la lecture du code :**

* `Db::point_record()` ne doit appeler **aucune** autre méthode de `Db` pendant que la garde
  de `self.conn()` est vivante : le `Mutex` de connexion n'est pas réentrant, et le
  processus se bloque sans le moindre message. Les quatre agrégats sortent donc d'une
  requête unique ;
* dans `.cross-split`, les pistes de grille sont **assignées explicitement**
  (`grid-row: 1..4`). L'avertissement de lisibilité passe en `display: none` quand il n'a
  rien à dire ; il cesse alors d'être un élément de grille, tout remonte d'une piste, et le
  graphique se retrouve dans la piste prévue pour le tableau — écrasé à 14 px pendant que la
  piste du bas reste vide.

---

## 12. Limites connues de ce socle

* **Suppression de données** : la suppression complète de l'application (« Effacer toutes les données » dans les Paramètres, via `clear_all_data`) et la suppression d'un point/station avec toutes ses séries associées (dans le projet, via `delete_point`) sont implémentées et fonctionnelles. La suppression isolée d'un seul projet ou d'une seule série au sein d'un point reste à ajouter quand l'UI en aura besoin.
* **Les six champs météo de ROMMA sont stockés et relus, mais pas exploités** : ni
  statistiques, ni croisement, ni contexte pour l'agent IA. `stats::weather_coverage()` se
  contente d'en dresser l'inventaire (§ 3). C'est un choix, pas un oubli — les agrégats à
  calculer sur du vent et de la pluie restent à définir.
* **Aucune API ROMMA** : uniquement l'import de fichier. Le champ « station ROMMA » du
  formulaire de point ne débloque donc aucune récupération automatique, contrairement à son
  homologue Infoclimat.
* **Les agrégats journaliers de ROMMA (`climato_*.csv`) ne sont pas importables**, et ne le
  seront pas : l'application recalcule ces valeurs elle-même (§ 5bis).
* **Un classeur ElitechLog traverse l'IPC en tableau de nombres** (`invoke` sérialise en
  JSON), soit environ six fois la taille du fichier. Sans effet à l'échelle d'un RC-5, dont
  la mémoire borne l'export à ~500 Ko ; à reconsidérer si un logger d'une autre génération
  produit des classeurs de plusieurs dizaines de Mo.
* **La détection de saut de température ne connaît qu'un seuil, codé en dur**
  (3 °C / 20 min). Il n'est pas réglable depuis l'interface — les seuils sont un paramètre
  de `anomaly::JumpThresholds`, exposé au code et aux tests, pas à l'écran Paramètres.
* La **fiche d'un point** (modale « Détail ») charge encore **toutes** les mesures d'une
  série pour calculer ses statistiques, via `measurements_for_series`. Confortable jusqu'à
  quelques centaines de milliers de lignes ; au-delà il faudra la faire passer par la
  lecture allégée du § 11, que le bloc de croisement utilise déjà.
* `DraginoApi` (accès direct à Orange Live Objects) existe dans le modèle et dans la
  contrainte SQL, mais **aucun code ne le produit** — et ce n'est pas prévu.
  `SourceType::ElitechDirect`, réservé depuis le prompt 1, est en revanche **écrit** depuis
  la lecture série (§ 4bis), et `InfoclimatApi` depuis l'API (§ 5).
* ⚠️ **La lecture directe du RC-5 sur Android n'a jamais tourné sur du matériel.** Tout le
  chemin existe — greffon Tauri, pilote CH340 en Kotlin, pont vérifié sur émulateur — mais
  l'émulateur n'a pas de contrôleur USB hôte, donc rien de la puce elle-même n'a pu être
  exercé. À tenir pour non fonctionnel tant qu'un téléphone, un adaptateur OTG et le logger
  n'auront pas confirmé (§ 4quater).
* **Les racines de certificats sont embarquées dans l'APK Android**, pas prises au système :
  une autorité révoquée le reste jusqu'au prochain build. Compromis assumé pour trois
  domaines fixes (§ 1, « Appels HTTP sur Android »).
* **Le dézoom n'a pas été vérifié sur un réseau lent.** Wi-Fi et hors ligne le sont, sur
  émulateur ; le cas « 4G qui traîne » — celui qui avait produit le gel d'origine — reste à
  faire sur appareil physique, l'AVD ne sachant pas simuler de latence sur son Wi-Fi (§ 9).
* **Seule la page RC-5 a été reprise pour le petit écran.** La grille 2×2 de l'espace de
  travail ne l'a pas été, et c'était hors du périmètre de ce prompt.
* **La lecture directe ne connaît que le RC-5.** Le RC-5+, le RC-4 et le RC-4HC sont refusés
  avec un message explicite plutôt que décodés au hasard : leur protocole est proche mais la
  taille de page diffère, et aucun n'a pu être testé (§ 4bis).
* **Le protocole série n'est pas documenté officiellement.** Il vient d'une rétro-ingénierie
  tierce, revérifiée trame par trame contre un logger réel — mais un seul, et un seul
  firmware. Deux comportements absents de la référence ont déjà dû être découverts à
  l'usage (§ 4bis) ; d'autres peuvent rester. **L'import xlsx demeure le filet de sécurité**,
  et il n'a pas bougé.
* ⚠️ **La commande de réinitialisation du RC-5 n'a pas été vérifiée sur le matériel.**
  C'est la seule écriture du projet, et le seul endroit où le comportement décrit est
  déduit plutôt qu'observé. Elle est encadrée — la fiche d'identité de l'appareil lui est
  renvoyée telle quelle, l'effacement est confirmé par une relecture, les réglages sont
  comparés avant/après et toute divergence est nommée à l'écran — mais elle reste à valider
  à la main, sur un logger dont les données ont d'abord été enregistrées (§ 4ter).
* ⚠️ **L'arrêt automatique qui précède désormais la réinitialisation n'a pas non plus été
  exercé sur le matériel.** Ses deux morceaux ont chacun été vus fonctionner séparément —
  `CMD_STOP` a été sondé et vérifié sur un RC-5 réel, l'effacement l'a été sur un logger à
  l'arrêt — mais **leur enchaînement sur un logger réellement en cours d'enregistrement**
  n'a jamais tourné : il a été écrit et testé sur transport simulé uniquement. C'est
  précisément le cas rencontré sur le terrain, et c'est donc le premier à revalider dès que
  le matériel est de nouveau accessible. Protocole de vérification : lancer un
  enregistrement sur le logger, laisser quelques mesures s'accumuler, les enregistrer en
  base, puis déclencher « Réinitialiser » et vérifier que le bilan annonce bien l'arrêt et
  `0` mesure en mémoire.
* **La saisie manuelle date sa valeur du milieu de la plage horaire** : c'est une convention,
  pas une mesure. Pour un minimum, l'instant réel du minimum dans la plage est inconnu et le
  restera — c'est le prix d'un relevé manuscrit, et `SourceType::ManualEntry` est là pour
  que personne ne l'oublie en lisant une statistique (§ 4).
* **Les horodatages lus en direct sont reconstruits**, pas stockés : l'appareil ne garde que
  des températures. Un logger arrêté puis relancé sans effacement fausserait la
  reconstruction — *comme il fausserait l'export xlsx*, qui applique le même calcul (§ 4bis).
* **Le numéro de série lu en direct est tronqué à 10 caractères** par le protocole
  (`E205004333` au lieu de `EFE205004333`). L'écran le signale et le champ de saisie manuelle
  permet de rétablir le numéro complet (§ 4bis).
* **La détection de branchement est un sondage**, pas une notification système : jusqu'à
  2,5 s entre le branchement et la notification. Un pont `WM_DEVICECHANGE` la rendrait
  instantanée, au prix de code spécifique à Windows (§ 4bis).
* Le recalibrage d'horloge passe par une simple boîte `prompt()` — suffisant pour vérifier
  le mécanisme, à reprendre quand l'écran de gestion des séries sera repris.
* **L'écriture du fichier d'export passe par la boîte de dialogue native**, donc par le
  sélecteur de documents du système sur Android. Le chemin rendu y est une URI
  `content://`, et non un chemin de fichier : c'est `tauri_plugin_fs::Fs::open` qui la
  résout. Conséquence : l'application ne connaît pas le **chemin absolu** du fichier écrit
  sur Android, seulement son URI — le message de confirmation affiche donc cette dernière.
* **Le croisement ne porte que sur la température**, jamais sur les six grandeurs météo de
  ROMMA — même raison qu'au § 3.
* **Le record de froid coûte un balayage complet du point** à chaque calcul, y compris sur
  une fenêtre de sept jours. C'est le plancher de ~85 ms mesuré au § 11 sur l'archive
  ROMMA : `MIN(temperature_celsius)` ne peut pas s'appuyer sur
  `idx_measurements_series_ts`, qui ne porte pas la température. Un index dédié le
  ramènerait à une recherche, au prix d'un index de plus sur 476 912 lignes — pas rentable
  tant que 85 ms passent inaperçus.
* **Les dix scripts de l'écran d'accueil sont un jeu d'exemple** (§ 7), à remplacer par la
  liste définitive d'une cinquantaine de scripts.
* **L'agent est plafonné par le palier gratuit de Groq** : 8 000 tokens/minute, soit environ
  six analyses consécutives. Au-delà, il faut attendre — l'application le dit clairement,
  mais ne met pas les demandes en file d'attente et ne réessaie pas toute seule.
* **L'écran Paramètres reste minimal** : le prénom, le statut des deux clés et un réglage
  Infoclimat. La clé TOP25, notamment, se change toujours en dur dans `src/map.js`.
* **Aucune recherche de station Infoclimat** : l'identifiant (`STATIC0479`…) se saisit à la
  main dans le formulaire du point. Ni recherche par nom, ni découverte par proximité
  géographique — ce sera une amélioration future si le besoin se confirme.
* **Le rafraîchissement Infoclimat n'est pas automatique** : le garde-fou d'intervalle
  existe et est configurable, mais rien ne déclenche d'appel périodique tout seul. Il faut
  cliquer sur « Récupérer les dernières données ». Une tâche de fond serait le prolongement
  naturel de `last_api_fetch_at`.
* **L'unité du champ `battery` reste inconnue.** La valeur est conservée brute et affichée
  sans unité ; aucune alerte « batterie faible » ne doit en être tirée avant d'avoir
  confirmé l'échelle auprès du propriétaire de la station (§ 5).
* **L'attribution CC BY-NC n'est pas encore dans l'export.** Elle apparaît à l'écran, mais
  l'export HTML devra la reprendre — chantier du prompt dédié à l'export.
* **La liste des projets n'a pas changé** : elle reste l'écran simple du prompt 1, en attente
  de l'écran d'accueil dédié.
* **Le TOP25 dépend d'une clé provisoire partagée** (`ign_scan_ws`). L'application détecte
  son expiration et le dit, mais ne sait pas la renouveler seule : cf. § 9.

## Licence

Gastinel Weather Studio est un logiciel libre, distribué sous **GNU GPL v3 ou ultérieure**
(`GPL-3.0-or-later`). Le texte complet est dans [`LICENSE`](LICENSE).

Le **nom** « Gastinel Weather Studio » et le logo ne sont pas couverts par cette licence
(GPL v3, art. 7 e) : une version modifiée doit porter un autre nom et utiliser ses propres
clés et serveurs. Détails dans [`TRADEMARKS.md`](TRADEMARKS.md).

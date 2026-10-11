# Gastinel Weather Studio

Application de bureau (Windows, avec une cible Android) pour **centraliser, cartographier et analyser des relevés de température** — en particulier ceux des trous à froid (TAF) du Vercors. Écrite en Rust avec Tauri v2, interface en HTML/CSS/JS sans bundler.

> État : projet personnel en développement (version 0.5.x). Le logiciel fonctionne, mais la lecture USB du logger sur Android n'a pas encore été essayée sur du matériel réel.

## Fonctionnalités

- **Carte** interactive sur fonds IGN (orthophoto, TOP25), avec les points de mesure et les stations.
- **Import de relevés** : exports ElitechLog (`.xls`), Infoclimat (JSON et CSV), archives ROMMA (`.csv`). Le format est reconnu au contenu.
- **Lecture directe du logger Elitech RC-5** par le port série USB, sans passer par ElitechLog.
- **API Infoclimat** : récupération des données de stations avec votre propre clé.
- **Comparaison** : statistiques côte à côte et courbes superposées de plusieurs séries.
- **Assistant d'analyse** (optionnel) via l'API Groq, avec votre propre clé.
- **Export** en CSV, TXT et HTML.
- Données stockées **localement** (SQLite). Rien n'est envoyé ailleurs sans action de votre part.

## Compiler

Prérequis : [Rust](https://rustup.rs) (stable, 1.77.2 ou plus), les Build Tools MSVC avec le SDK Windows, WebView2 (présent sur Windows 11) et la CLI Tauri (`cargo install tauri-cli --version "^2"`). Node.js n'est pas nécessaire.

```bash
cargo tauri dev      # lancer en développement
cargo tauri build    # produire l'exécutable
cargo test --manifest-path src-tauri/Cargo.toml
```

La première compilation est longue (Tauri et SQLite sont compilés depuis les sources). Deux tests font de vrais appels réseau et sont ignorés par défaut ; un autre demande un RC-5 branché en USB.

## Clés d'API

Aucune clé n'est fournie. Pour utiliser l'API Infoclimat ou l'assistant Groq, créez vos propres clés et déposez-les dans un dossier `Clee.API/` (par exemple `Clee.API/API.Groq.env`), ou saisissez-les dans l'application. Ce dossier est ignoré par git et les clés restent sur votre machine.

## Licence

Logiciel libre sous **GNU GPL v3 ou ultérieure** (`GPL-3.0-or-later`), texte dans [`LICENSE`](LICENSE).

Le nom « Gastinel Weather Studio » et le logo ne sont pas couverts par cette licence (GPL v3, art. 7 e) : une version modifiée doit porter un autre nom. Détails dans [`TRADEMARKS.md`](TRADEMARKS.md).

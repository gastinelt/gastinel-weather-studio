# Extensions natives de Gastinel Weather Studio

## Risques acceptés (décision de l'utilisateur, 25/09/2026)

- **Aucune isolation mémoire.** Une extension est une `.dll` chargée dans le
  processus de l'app. Un bug (ou une panique non rattrapée) peut corrompre la
  mémoire ou fermer l'application entière. WASM et processus séparé ont été
  écartés en connaissance de cause.
- **Accès SQLite direct.** L'extension ouvre elle-même le fichier de base.
  Avec la permission `db_write`, rien ne contrôle ce qu'elle écrit.
- **Garde-fou unique :** avant de charger une extension qui déclare `db_write`,
  l'app copie toute la base (`VACUUM INTO`) dans
  `%APPDATA%\fr.gastinel.weatherstudio\sauvegardes-extensions\` et garde les
  3 dernières copies. Si la copie échoue, l'extension n'est **pas** chargée.
  Cette copie protège contre une perte de données, **pas** contre un plantage
  pendant que l'extension tourne. Sur une base de plusieurs dizaines de Go,
  chaque activation coûte autant d'espace disque et plusieurs minutes.

## Installer une extension

Copier son dossier dans `%APPDATA%\fr.gastinel.weatherstudio\extensions\`
(ou `extensions\` à côté de l'exécutable), puis l'activer dans
**Paramètres → Extensions**. Aucune recompilation de l'app.

```
extensions/<id>/manifest.json
               /<id>.dll
               /ui/index.html (+ js, css)
```

## Manifeste

```json
{
  "id": "carte3d",
  "name": "Carte 3D",
  "version": "0.1.0",
  "description": "…",
  "api_version": 1,
  "library": "carte3d.dll",
  "ui_entry": "index.html",
  "hook": "map_view",
  "permissions": ["db_read", "network"]
}
```

- `api_version` doit être égal à la version de l'app (`gws_extension_api::API_VERSION`),
  sinon l'extension est marquée « incompatible » et n'est pas chargée. La DLL
  est revérifiée au chargement (`gws_ext_api_version`).
- `permissions` : déclaratives (affichées à l'utilisateur), sauf `db_write` qui
  déclenche la sauvegarde et donne une URI SQLite `mode=rw` au lieu de `mode=ro`.
- `hook` : seul `map_view` existe en v1, qui ajoute un bouton sur la carte du projet.
  Le bouton ouvre `ui/<ui_entry>` dans une fenêtre.

## Interface (ABI C, version 1)

Rust n'a pas d'ABI stable : la frontière est en C, avec cinq symboles qui
échangent du JSON. En pratique, on implémente le trait `Extension` du crate
`gws-extension-api`, puis on appelle `declare_extension!(MonType)`.

| Symbole | Rôle |
|---|---|
| `gws_ext_api_version() -> u32` | Version ciblée |
| `gws_ext_init(ctx_json) -> *mut c_char` | `null` = succès, sinon message d'erreur. `ctx` : `db_path`, `db_uri`, `db_writable`, `data_dir`, `host_version` |
| `gws_ext_call(method, payload_json) -> *mut c_char` | `{"ok": …}` ou `{"err": "…"}`. `payload.project_id` = projet actif |
| `gws_ext_free(ptr)` | Libère une chaîne rendue par l'extension |
| `gws_ext_shutdown()` | Avant le déchargement |

Les paniques sont rattrapées par la macro et deviennent des erreurs JSON.

## Panneau (UI)

Il est servi par le protocole `gwsext` (`http://gwsext.localhost/<id>/…` sous
Windows) dans une iframe sans accès à l'API Tauri. Il parle à l'app par
`postMessage` :

- panneau → app : `{gws:"ready"}`, puis `{gws:"call", rid, method, payload}` ;
- app → panneau : `{gws:"context", project_id}` et `{gws:"reply", rid, ok|err}`.

## Robustesse au démarrage

L'app lit les manifestes au démarrage, puis charge les DLL activées dans un
fil séparé. Un manifeste illisible, une DLL absente ou corrompue, un symbole
manquant ou une version différente ne font que marquer l'extension en erreur
ou « incompatible » : l'app démarre normalement. Tests :
`cargo test --lib extensions`, `cargo test --test extension_carte3d` et
`cargo test --test extension_thermique`.

## Compiler la carte 3D

```bash
cd extensions/carte3d && cargo build --release
```

Copier ensuite `manifest.json`, `target/release/carte3d.dll` et `ui/` dans
`%APPDATA%\fr.gastinel.weatherstudio\extensions\carte3d\`.

## Analyse thermique

Gradient vertical, épisodes radiatifs et profil vertical (lecture seule) :
voir [`thermique/README.md`](thermique/README.md).

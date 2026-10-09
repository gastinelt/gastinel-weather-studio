# Signature Ed25519 des mises à jour

À partir de la 0.5.0, l'application **refuse tout paquet `.gws` qui n'est pas
signé** par une clé qu'elle connaît. Cette page explique le format, la
génération de la clé (à faire par le propriétaire, une fois), la publication et
la migration des versions déjà installées.

## Ce qui est vérifié, et où

| Étape | Contrôle | Code |
|---|---|---|
| Ouverture d'un `.gws` (double-clic, dépôt, bandeau) | `manifest.sig` présent, `key_id` connu, signature Ed25519 valide sur le message ci-dessous. Sinon : message « Mise à jour refusée : … », aucun bouton « installer quand même ». | `package::inspect` |
| Installation | Même vérification, puis SHA-256 **et** taille de l'exécutable extrait contre le manifeste signé. Rien n'est écrit avant. | `package::apply_update` |
| Avis de mise à jour (`latest.json`) | Inchangé : l'app ouvre la page de téléchargement dans le navigateur, elle ne télécharge ni ne lance aucun setup. La sécurité repose sur le `.gws`, vérifié quel que soit son canal d'arrivée. | `update.rs` |

Message signé (octets UTF-8, `\0` = octet nul) :

```
gws-update-v1\0<version>\0<min_compatible_version ou vide>\0<sha256 hex minuscule>\0<taille exe en octets>
```

La version fait partie du message : on ne peut pas rejouer la signature d'une
ancienne version sous un autre numéro. `manifest.sig` (dans le ZIP) :

```json
{"format": 1, "key_id": "maj-2026-a", "signature": "<64 octets en base64>"}
```

Clés publiques acceptées : `CLES_MAJ` dans `src-tauri/src/package.rs`, distinctes
de la clé des extensions. **Elle est vide dans le dépôt** : tant qu'elle l'est,
l'application compilée refuse tous les `.gws`.

## Générer la clé de production (propriétaire, une fois)

Sur le poste de publication, **hors du dépôt** (le script refuse un chemin dans
le dépôt et n'écrase jamais un fichier existant) ; le mot de passe est demandé
au clavier (12 caractères minimum) :

```powershell
pip install cryptography
python scripts\signer_maj.py generer-cle --sortie "$env:USERPROFILE\.gws\cle_maj_2026a.pem" --key-id maj-2026-a
python scripts\signer_maj.py generer-cle --sortie "<clé USB>\cle_maj_secours.pem"   --key-id maj-secours-1
```

Chaque commande affiche une ligne à coller dans `CLES_MAJ` :

```rust
pub const CLES_MAJ: &[(&str, [u8; 32])] = &[
    ("maj-2026-a", [0x.., ...]),
    ("maj-secours-1", [0x.., ...]),
];
```

Puis `cargo test --lib -- --ignored cles_de_production` doit passer (deux clés
au moins, aucune clé de test). La clé de secours reste hors ligne ; elle sert si
la clé active est perdue ou compromise (signer avec elle une version qui
embarque une nouvelle paire).

Garde : deux copies hors ligne des fichiers `.pem` (chiffrés), mot de passe
conservé séparément ; jamais sur le VPS, Forgejo ou une CI.

## Publier

```powershell
$env:GWS_CLE_MAJ = "$env:USERPROFILE\cles-gws\maj-2026-a.pem"
$env:GWS_CLE_MAJ_ID = "maj-2026-a"
powershell -ExecutionPolicy Bypass -File .\scripts\create_gws_package.ps1 -Notes "..."
python scripts\signer_maj.py verifier update-X.Y.Z.gws   # contrôle avec CLES_MAJ du code
```

Sans clé, le script crée le paquet mais avertit qu'il est **non signé** : ne pas
le publier. `signer` refuse une clé privée qui ne correspond pas à `CLES_MAJ`.

La commande `signer` affiche une valeur `gws-ed25519-v1:<key_id>:<base64>` à
mettre dans `platforms.windows-x86_64.signature` de `latest.json` /
`latest-beta.json` à la place de `placeholder_sig`. Ce champ existe déjà et
`gws_share.py` exige seulement qu'il soit non vide : le changement est
rétrocompatible, aucune version de l'app ne l'interprète aujourd'hui (il sert de
trace et permettra plus tard une vérification dès l'avis).

### Proposition pour `gws_share.py` (non appliquée)

Journaliser, sans refuser, un manifeste dont la signature n'a pas la forme
attendue — refuser casserait l'avis de la 0.4.7 tant que `latest.json` stable
porte `placeholder_sig`. Passer au refus une fois la première version signée
publiée sur les deux canaux.

```diff
@@ def update(self, target, arch, current, beta=False):
         plat = (manifest.get("platforms") or {}).get(f"{target}-{arch}")
         if not isinstance(plat, dict) or not plat.get("url") or not plat.get("signature"):
             return self.send_empty(204)
+        if not re.fullmatch(r"gws-ed25519-v1:[a-z0-9._-]{1,41}:[A-Za-z0-9+/]{86}==", str(plat["signature"])):
+            log(f"manifeste {manifest.get('version')} : signature absente ou mal formee")
```

(`re` et `log` existent déjà dans la copie lue le 27/09/2026, `/home/debian/staging/gws-beta/gws_share.py` ; vérifier la copie réellement en service avant d'appliquer.)

## Migration des versions installées

- **0.4.x et 0.5.0-beta.1/2** ne connaissent pas `manifest.sig` : elles
  n'ouvrent que `manifest.json` et l'exécutable, un fichier de plus dans le ZIP
  ne les gêne pas. Elles installent donc normalement la première version signée
  (vérifié dans l'historique de `package.rs` depuis son introduction, commit 9a3ff26 : seules ces deux entrées sont lues par nom ; les paquets restent compressés en deflate comme avant). Elles restent
  en revanche exposées à un `.gws` forgé tant qu'elles ne sont pas mises à jour ;
  aucune action côté serveur ne peut l'empêcher.
- **Première version signante (0.5.0)** : elle est livrée elle-même en `.gws`
  signé (et en setup). À partir d'elle, tout `.gws` non signé est refusé.
- Recommandation : notes de version de la 0.5.0 « mettez à jour : cette version
  vérifie l'authenticité des mises à jour », et ne plus publier de `.gws` non
  signé à partir de cette date.
- Le setup `.exe` n'est pas couvert par cette signature (il n'est jamais lancé
  par l'app) ; c'est le rôle d'Authenticode, voir la proposition initiale.

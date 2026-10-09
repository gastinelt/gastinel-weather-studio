# Configuration signée du relai SSH (v1)

À partir de cette branche, quand le **relai SSH est activé** (Paramètres → Relai
serveur), l'application lit sur ce serveur une petite configuration signée. Elle
servira plus tard à annoncer une extension et à porter des paramètres de quota
partagé. **Dans cette version, elle est seulement lue, vérifiée et conservée** :
rien n'est affiché, aucune extension n'est téléchargée ni installée, le quota
n'est pas interprété.

Relai désactivé (ou jamais configuré) : l'application se comporte exactement
comme la 0.5.0-beta.5, aucune connexion supplémentaire n'est ouverte.

Code : `src-tauri/src/relais_config.rs`. Outil de signature :
`scripts/signer_config.py` (tests : `scripts/test_signer_config.py`).

## Fichiers sur le serveur

Dans le dossier personnel de l'utilisateur SSH configuré dans l'application :

```
~/gws-relais/config.json   contenu signé (octets exacts)
~/gws-relais/config.sig    signature
```

Chemins : constantes `DOSSIER_DISTANT`, `FICHIER_CONFIG`, `FICHIER_SIGNATURE` de
`relais_config.rs`. Taille maximale : **64 Kio par fichier** ; au-delà, refus.

## Format de `config.json`

```json
{
  "format": 1,
  "serial": 7,
  "emis_le": "2026-09-29T10:00:00Z",
  "expire_le": "2026-12-31",
  "serveur": "relais-exemple",
  "extension": {
    "nom": "exemple",
    "version": "1.0.0",
    "url": "https://example.org/gws/exemple-1.0.0.zip",
    "sha256": "<64 caractères hexadécimaux>"
  },
  "quota": { "requetes_par_jour": 4000 }
}
```

| Champ | Obligatoire | Règle |
|---|---|---|
| `format` | oui | entier, doit valoir `1` ; sinon la configuration est ignorée (journal). |
| `serial` | oui | entier ≥ 0, **croissant** à chaque publication. |
| `emis_le` | oui | texte (date ISO), informatif, non contrôlé. |
| `expire_le` | non | `AAAA-MM-JJ` (valable jusqu'à la fin de ce jour, UTC) ou RFC 3339 avec fuseau. Dépassée ou illisible : refus. |
| `serveur` | oui | texte, informatif. |
| `extension` | non | `{nom, version, url, sha256}` : une **offre** seulement. `url` en `https://`, `sha256` en 64 hex ; sinon refus de toute la configuration. |
| `quota` | non | objet libre, conservé tel quel, non interprété. |

Les champs inconnus sont ignorés (compatibilité ascendante). Le fichier doit
être de l'UTF-8 (un BOM éventuel est toléré à la lecture, mais il fait partie
des octets signés).

## Signature

`config.sig` :

```json
{"format": 1, "key_id": "maj-2026-a", "signature": "<64 octets en base64>"}
```

Clés acceptées : **les mêmes que pour les mises à jour** (`CLES_MAJ` dans
`src-tauri/src/package.rs`). Message signé (Ed25519) :

```
b"GWS-CONFIG-RELAIS-V1\0" + octets exacts de config.json
```

Le préfixe (contexte) sépare les deux usages de la clé : le message d'une mise à
jour commence par `gws-update-v1\0`, celui d'une configuration par
`GWS-CONFIG-RELAIS-V1\0` ; ils diffèrent dès le premier octet, donc **aucune
signature de mise à jour ne peut être acceptée comme signature de configuration,
ni l'inverse**. C'est vérifié par des tests (Rust :
`signatures_de_maj_et_de_config_ne_sont_pas_interchangeables` ; Python :
`test_contexte_distinct_des_mises_a_jour`).

La vérification porte sur les **octets tels que reçus**, jamais sur une
re-sérialisation : après signature, ne plus toucher au fichier (ni fins de
ligne, ni espaces, ni encodage). Copier par `scp`/`sftp` en binaire.

## Contrôles, dans l'ordre

1. Taille de chaque fichier ≤ 64 Kio.
2. Signature : `format` 1, `key_id` connu de `CLES_MAJ`, signature Ed25519 valide.
3. JSON lisible, `format` = 1, champs obligatoires et types.
4. `expire_le` non dépassée.
5. Anti-rejeu, par rapport à la dernière configuration acceptée sur ce poste :
   - `serial` plus petit : refus ;
   - `serial` égal, contenu différent (SHA-256) : refus ;
   - `serial` égal, même contenu : déjà appliquée, rien à faire ;
   - `serial` plus grand : acceptée.

Tout échec (réseau, fichier absent, trop gros, signature, JSON, format, expirée,
rejeu) est journalisé sur la sortie d'erreur (`[relais-config] …`, sans
identifiant ni secret) et noté comme « dernier motif de refus » ; **la dernière
configuration valide reste en place**.

## Quand la lecture a lieu

Toujours en tâche de fond, et **seulement si le relai SSH est activé** :

- **au démarrage**, environ 30 s après le lancement (`DELAI_DEMARRAGE`), une
  seule fois ;
- **après chaque récupération Infoclimat** (`fetch_infoclimat`) passée par le
  relai ; l'appel d'API n'attend jamais.

Aucune minuterie permanente. Plancher : au plus une tentative toutes les
**10 minutes**, réussie ou non. L'heure de la dernière tentative est notée dans
les réglages (`relais_config_derniere_tentative`) **avant** la tentative, donc le
plancher survit à la fermeture de l'application et un échec (réseau, fichier
absent, serveur incompatible…) n'est pas réessayé avant 10 minutes. Si l'heure
enregistrée est dans le futur (horloge qui a reculé) ou illisible, elle est
ignorée : le plancher ne bloque jamais indéfiniment. Une seule tentative à la
fois, abandonnée après 45 s.

### Hypothèse : serveur Linux avec shell POSIX

Le relai exécute déjà `curl` à distance pour Infoclimat, il suppose donc déjà
un serveur Linux (Debian). La configuration signée ajoute l'hypothèse d'un
**shell de connexion POSIX** (`sh`, `bash`, `dash`…) et des commandes `head` et
`wc`. Avec un autre shell (par exemple `fish`) ou un serveur non Linux, la
sortie de la commande est inutilisable : l'application note le motif
« serveur non compatible ou fichier absent (…) » dans le journal et dans
`relais_config_dernier_refus`, garde la configuration précédente et ne réessaie
pas avant le plancher de 10 minutes. Le même motif est noté quand les fichiers
n'existent pas sur le serveur.

La commande distante est fixe (aucun texte venu du réseau ou de l'utilisateur) :

```sh
for f in config.json config.sig; do p="$HOME"/gws-relais/"$f"; if [ -f "$p" ]; then n=$(head -c 65537 -- "$p" | wc -c); printf '%s\n' "$n"; head -c 65537 -- "$p"; else printf 'absent\n'; fi; done
```

Elle suppose un shell POSIX et `head`/`wc` (présents sur Debian). La sortie lue
sur le canal SSH est en outre bornée côté client. La connexion utilise la même
clé d'hôte épinglée et les mêmes identifiants que le relai Infoclimat.

## Stockage local (`app_settings`)

| Clé | Contenu |
|---|---|
| `relais_config_json` | `config.json` accepté (texte exact) |
| `relais_config_sig` | `config.sig` correspondant (permet de re-vérifier à la lecture) |
| `relais_config_serial` | dernier `serial` accepté |
| `relais_config_sha256` | SHA-256 du contenu accepté |
| `relais_config_key_id` | clé qui a signé |
| `relais_config_acceptee_le` | date d'acceptation (RFC 3339) |
| `relais_config_dernier_refus` / `_le` | dernier motif d'échec et sa date (vidés après un succès) |
| `relais_config_derniere_tentative` | heure (RFC 3339) de la dernière tentative, réussie ou non : plancher de 10 minutes |

L'acceptation est écrite en une seule transaction (`Db::set_settings`).

## Commande Tauri (lecture seule)

`get_relais_config_status` → `{ presente, serial, key_id, acceptee_le,
offre_extension, expiree, dernier_refus, dernier_refus_le }`. Aucun écran ne
l'appelle encore. `presente` n'est vrai que si la configuration stockée se
re-vérifie (signature et format) ; `expiree` signale une configuration acceptée
dont la date est depuis dépassée. Le contenu du quota et les identifiants SSH
n'y figurent jamais.

## Procédure (propriétaire de la clé)

Sur le poste qui détient la clé privée de mise à jour (hors dépôt, jamais sur le
serveur) :

1. Écrire `config.json` (voir le format) avec un `serial` **supérieur** au
   précédent.
2. Signer ; le mot de passe de la clé est demandé au clavier (saisie masquée) :

   ```powershell
   python scripts\signer_config.py signer chemin\vers\config.json --cle <chemin de la clé .pem> --key-id maj-2026-a
   ```

   Le script refuse un contenu que l'application refuserait (format, serial,
   expiration, offre mal formée, taille) et une clé privée qui ne correspond pas
   à `CLES_MAJ`. Il écrit `config.sig` à côté de `config.json`.
3. Contrôler avec les clés du code :

   ```powershell
   python scripts\signer_config.py verifier chemin\vers\config.json
   ```

4. Copier **sans modification** les deux fichiers dans `~/gws-relais/` de
   l'utilisateur SSH du relai (`mkdir -p ~/gws-relais` au besoin).

## Limites de cette version

- Aucune interface : la configuration n'est visible que par la commande de
  statut (non branchée) et dans la base.
- L'offre d'extension n'est jamais téléchargée ni installée ; le quota n'est pas
  appliqué.
- Lecture au démarrage (≈ 30 s) et après un appel Infoclimat par le relai
  seulement : une application laissée ouverte sans appel ne relit pas la
  configuration (pas de minuterie).
- Pas de révocation explicite : une configuration n'est remplacée que par un
  `serial` supérieur ; `expire_le` en limite la durée.
- Le serveur est authentifié par sa clé d'hôte épinglée, mais la confiance dans
  la configuration repose uniquement sur la signature : un serveur compromis
  peut retirer ou retenir la configuration (l'ancienne reste alors en place),
  pas en forger une.
- Testé en tests unitaires uniquement (Rust et Python) ; pas encore contre un
  serveur réel.

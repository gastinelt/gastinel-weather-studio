# Guide de publication d'une mise à jour (Gastinel Weather Studio)

Ce document décrit le fonctionnement du système de mise à jour automatique de **Gastinel Weather Studio** et fournit la procédure pas à pas pour compiler, publier et déployer une nouvelle version.

---

## 1. Comment fonctionne le système de mise à jour ?

1. **Vérification automatique au démarrage :**  
   Lorsqu'un utilisateur lance l'application, celle-ci interroge l'endpoint du serveur :  
   `GET https://gastinel.me/gws/api/v1/update/windows/x86_64/<version_actuelle>`

2. **Résolution côté serveur :**  
   Le service local `gws-share` (port 8087, servi par Caddy) lit le fichier `/etc/gws-share/latest.json`.  
   - Si la version indiquée dans ce fichier est strictement supérieure à la version du client, l'API renvoie le descriptif de la mise à jour (notes, URL de téléchargement, date).  
   - Si le client est déjà à jour, l'API renvoie `204 No Content`.

3. **Téléchargement du paquet `.gws` :**  
   Le lien annoncé dans `latest.json` pointe vers le **paquet `.gws`** (pas vers l'installateur) :  
   `https://gastinel.me/gws/downloads/update-<version>.gws`  
   Les fichiers sont dans `/srv/gws-updates/` sur le serveur. L'app n'accepte qu'un lien commençant par `https://gastinel.me/gws/downloads/` (sans `..`).

4. **Bouton « Télécharger » de l'app (versions postérieures à 0.4.6) :**  
   l'app ouvre `https://gastinel.me/gws/download/?maj=<version>`. La page valide la version, vérifie que `update-<version>.gws` existe, lance son téléchargement et affiche un bandeau d'explication. **Le paquet doit donc s'appeler exactement `update-<version>.gws`.** Les apps 0.4.x déjà installées, elles, ouvrent directement le lien `.gws` de `latest.json`.

### Numéros de version : règles strictes

- **Trois nombres uniquement** : `0.4.7`, `0.5.0`. Un numéro à quatre nombres comme `0.4.4.1` est **invalide** : l'app le refuse et n'annonce rien.
- Une pré-version (`0.4.7-beta.1`) est **plus ancienne** que la version finale `0.4.7` (ordre SemVer). Ne publiez jamais une pré-version sur le canal stable en pensant qu'elle passera devant.
- La version doit être identique dans `Cargo.toml`, `tauri.conf.json`, le nom du paquet et `latest.json`.

---

## 2. Prérequis

- **Environnement local :**
  - Rust & Cargo opérationnels (`rustc`, `cargo`).
  - Tauri CLI (`cargo-tauri`).
  - NSIS installé (utilisé par Tauri pour créer l'installateur Windows).
  - OpenSSH client (`ssh`, `scp`) configuré avec l'hôte `cornafsuisse` (ou accès SSH vers votre serveur Debian distant).

- **Accès serveur :**
  - Utilisateur SSH avec droits `sudo` sur `cornafsuisse`.
  - Dossier distant d'hébergement des binaires : `/srv/gws-updates/`.
  - Fichier distant de configuration de mise à jour : `/etc/gws-share/latest.json`.

---

## 3. Procédure étape par étape

### Étape 1 : Incrémenter le numéro de version

Modifier les deux fichiers suivants en remplaçant l'ancienne version par la nouvelle (ex. `0.3.5`) :

1. **`src-tauri/Cargo.toml`** :
   ```toml
   [package]
   name = "gastinel-weather-studio"
   version = "0.3.5"
   ```

2. **`src-tauri/tauri.conf.json`** :
   ```json
   {
     "productName": "Gastinel Weather Studio",
     "version": "0.3.5",
     ...
   }
   ```

3. Mettre à jour `Cargo.lock` :
   ```powershell
   cargo check --manifest-path src-tauri/Cargo.toml
   ```

---

### Étape 2 : Valider et commiter dans Git

```powershell
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json
git commit -m "chore(release): v0.3.5 - description des changements"
```

---

### Étape 3 : Compiler l'application de production

Exécuter la compilation du bundle release :

```powershell
cargo tauri build
```
*(Ou double-cliquer sur le fichier `Compiler_Gastinel_Weather_Studio.bat` à la racine).*

> **Si l'application tourne** (lancée depuis `src-tauri\target`), ses fichiers sont verrouillés et la compilation échoue ou mélange les versions. Fermez-la, ou compilez dans un dossier séparé :
> ```powershell
> $env:CARGO_TARGET_DIR = "src-tauri\target-release-maj"
> cargo tauri build
> Remove-Item Env:CARGO_TARGET_DIR
> ```
> Les chemins `src-tauri\target\release\…` ci-dessous deviennent alors `src-tauri\target-release-maj\release\…`. Attention : `create_gws_package.ps1` lit l'exécutable dans `src-tauri\target\release\` ; avec un dossier séparé, fermez l'app et copiez-y l'exécutable avant de lancer le script.

L'installateur NSIS est généré dans :
`src-tauri\target\release\bundle\nsis\Gastinel Weather Studio_0.3.5_x64-setup.exe`

Copiez-le sur votre bureau pour archivage / tests :
```powershell
Copy-Item "src-tauri\target\release\bundle\nsis\Gastinel Weather Studio_0.3.5_x64-setup.exe" "$HOME\Desktop\" -Force
```

### Étape 3 bis : Générer le paquet de mise à jour `.gws` (Option recommandée sans SmartScreen)

Pour distribuer la mise à jour sous forme de paquet `.gws` (directement ouvrable au double-clic dans GWS sans alerte antivirus/SmartScreen) :

**Depuis la 0.5.0, le paquet doit être signé** (sinon l'application le refuse) : voir [`docs/signature-maj.md`](docs/signature-maj.md).

```powershell
$env:GWS_CLE_MAJ = "<chemin de la clé privée, hors dépôt>"
$env:GWS_CLE_MAJ_ID = "maj-2026-a"
powershell -ExecutionPolicy Bypass -File .\scripts\create_gws_package.ps1 -Notes "Vos notes de version"
python scripts\signer_maj.py verifier update-0.3.5.gws
```

Le fichier `update-0.3.5.gws` est généré à la racine du projet (fichier ignoré par git).

Le script écrit `manifest.json` en **UTF-8 sans BOM** (les anciennes versions du script ajoutaient un BOM que l'app refusait : aucun paquet ne s'appliquait). Contrôle rapide avant publication :
```powershell
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::OpenRead("$PWD\update-0.3.5.gws")
$s = $zip.GetEntry("manifest.json").Open(); $o = New-Object byte[] 3; [void]$s.Read($o, 0, 3); $s.Close(); $zip.Dispose()
$o -join ' '   # doit commencer par 123 ({) ; 239 187 191 = BOM, paquet à refaire
```

---

### Étape 4 : Téléverser les fichiers sur le serveur

1. **Copier les fichiers vers le serveur dans `/tmp` :**
   ```powershell
   scp "src-tauri\target\release\bundle\nsis\Gastinel Weather Studio_0.3.5_x64-setup.exe" debian@cornafsuisse:/tmp/Gastinel_Weather_Studio_0.3.5_x64-setup.exe
   scp "update-0.3.5.gws" debian@cornafsuisse:/tmp/update-0.3.5.gws
   ```

2. **Déplacer dans `/srv/gws-updates/` et ajuster les droits :**
   ```powershell
   ssh cornafsuisse "sudo cp /tmp/Gastinel_Weather_Studio_0.3.5_x64-setup.exe /tmp/update-0.3.5.gws /srv/gws-updates/ && sudo chown debian:debian /srv/gws-updates/* && sudo chmod 644 /srv/gws-updates/* && rm /tmp/Gastinel_Weather_Studio_0.3.5_x64-setup.exe /tmp/update-0.3.5.gws"
   ```

---

### Étape 5 : Mettre à jour le fichier `latest.json` sur le serveur

Créez un fichier local temporaire `latest.json` avec la structure suivante :

```json
{
  "version": "0.3.5",
  "notes": "Nouveautés de la v0.3.5 :\n- Description du changement 1\n- Description du changement 2",
  "pub_date": "2026-09-23T18:00:00Z",
  "platforms": {
    "windows-x86_64": {
      "url": "https://gastinel.me/gws/downloads/update-0.3.5.gws",
      "signature": "gws-ed25519-v1:maj-2026-a:<valeur affichée par signer_maj.py>"
    }
  }
}
```

Puis déployez-le sur le serveur :
```powershell
scp latest.json debian@cornafsuisse:/tmp/latest.json
ssh cornafsuisse "sudo cp /tmp/latest.json /etc/gws-share/latest.json && sudo chown root:root /etc/gws-share/latest.json && sudo chmod 644 /etc/gws-share/latest.json && rm /tmp/latest.json"
Remove-Item latest.json
```

---

### Étape 6 : Vérifier le bon fonctionnement

Exécutez les deux requêtes de contrôle :

1. **Vérifier l'accessibilité du binaire téléchargeable :**
   ```powershell
   curl.exe -I https://gastinel.me/gws/downloads/Gastinel_Weather_Studio_0.3.5_x64-setup.exe
   ```
   *Résultat attendu : `HTTP/1.1 200 OK` avec un `Content-Length` correspondant à la taille du fichier.*

2. **Vérifier la réponse de l'API de mise à jour pour une version antérieure :**
   ```powershell
   curl.exe -s https://gastinel.me/gws/api/v1/update/windows/x86_64/0.3.4
   ```
   *Résultat attendu : un JSON contenant la version `0.3.5`, les notes et le lien vers `update-0.3.5.gws`.*

3. **Vérifier le paquet `.gws` et la page de téléchargement :**
   ```powershell
   curl.exe -I https://gastinel.me/gws/downloads/update-0.3.5.gws
   ```
   Puis ouvrez `https://gastinel.me/gws/download/?maj=0.3.5` : le téléchargement doit démarrer et le bandeau vert s'afficher.

---

### Étape 7 : Mettre à jour la page de téléchargement du site

Le site statique n'est pas un dépôt git : sauvegardez d'abord les pages (localement et, sur le serveur, dans `/var/backups/site-<date>`).

1. Dans `gws/download/index.html` : numéro de version (balises `description`, `og:title`, badge), lien de l'installateur, lien du paquet `update-<version>.gws`, tailles réelles (`Content-Length` des `curl -I` ci-dessus) et liste des nouveautés.
2. Dans `gws/index.html` : badge et boutons « Télécharger v… ».
3. `node tools/precompress.js` dans le dossier du site, puis copie des `index.html`, `.gz` et `.br` (propriétaire `debian:debian`, droits 644).
4. Contrôle : `curl -H "Accept-Encoding: gzip"` (et sans compression, et `zstd`) doit renvoyer la nouvelle version.

---

## 4. Script PowerShell récapitulatif (Aide-mémoire)

Voici le bloc de commandes PowerShell enchaîné pour publier une mise à jour après le build :

```powershell
$VERSION = "0.3.5"
$EXE = "Gastinel Weather Studio_${VERSION}_x64-setup.exe"
$SRC = "src-tauri\target\release\bundle\nsis\$EXE"
$SERVER_EXE = "Gastinel_Weather_Studio_${VERSION}_x64-setup.exe"

# 1. Copie bureau
Copy-Item $SRC "$HOME\Desktop\" -Force

# 2. Upload installateur et paquet .gws
$GWS = "update-$VERSION.gws"
scp $SRC "debian@cornafsuisse:/tmp/$SERVER_EXE"
scp $GWS "debian@cornafsuisse:/tmp/$GWS"
ssh cornafsuisse "sudo cp /tmp/$SERVER_EXE /tmp/$GWS /srv/gws-updates/ && sudo chown debian:debian /srv/gws-updates/$SERVER_EXE /srv/gws-updates/$GWS && sudo chmod 644 /srv/gws-updates/$SERVER_EXE /srv/gws-updates/$GWS && rm /tmp/$SERVER_EXE /tmp/$GWS"

# 3. Upload latest.json
scp latest.json "debian@cornafsuisse:/tmp/latest.json"
ssh cornafsuisse "sudo cp /tmp/latest.json /etc/gws-share/latest.json && sudo chown root:root /etc/gws-share/latest.json && sudo chmod 644 /etc/gws-share/latest.json && rm /tmp/latest.json"

# 4. Tests
curl.exe -I "https://gastinel.me/gws/downloads/$SERVER_EXE"
curl.exe -I "https://gastinel.me/gws/downloads/$GWS"
curl.exe -s "https://gastinel.me/gws/api/v1/update/windows/x86_64/0.3.4"
```

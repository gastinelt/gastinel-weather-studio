# Defender et la 0.5.0 : enquête et signature du code

Rédigé le 03/10/2026, pendant l'absence de Théo. Tout ce qui suit a été vérifié
sur le PC (journal Defender, analyses en ligne de commande) ou sur les pages
officielles citées en fin de document.

## 1. Ce qui s'est passé

| Heure | Événement (journal « Windows Defender/Operational ») |
|---|---|
| 09:22:19 | `Trojan:Win32/Bearfoos.A!ml` sur l'exe installé (`AppData\Local\Gastinel Weather Studio\gastinel-weather-studio.exe`). Type « Chemin rapide », c'est-à-dire un verdict envoyé par le cloud de Microsoft. |
| 09:22:32 | Même verdict au démarrage du processus. Mise en quarantaine de l'exe, du raccourci du menu Démarrer et de l'entrée de désinstallation. |
| 09:22:59 | Même verdict sur `target\release\deps\gastinel_weather_studio.exe` (le fichier de compilation, au contenu identique), détecté par la protection en temps réel. |

`!ml` veut dire que c'est un **modèle d'apprentissage automatique** qui a
classé le fichier, et non une signature écrite par un analyste à partir d'un
vrai virus.

## 2. Les tests

| Fichier | Résultat |
|---|---|
| exe 0.4.7, 0.4.8, 0.5.0-beta.1, 0.5.0-beta.5 (tirés des `.gws` du serveur) | aucune menace |
| exe 0.5.0 publié | **Bearfoos.A!ml**, bloqué dès sa sortie du paquet |
| `update-0.5.0.gws` | **Bearfoos.A!ml** : Defender ouvre le ZIP |
| setup 0.5.0 (NSIS) | aucune menace à l'analyse, mais l'exe qu'il installe est bloqué au lancement |
| **0.5.0 recompilée à l'identique** (même commit `73b2e6a`, autre empreinte) | **aucune menace** à l'analyse, et **aucune détection après 45 s d'exécution** |

Les analyses ont été faites avec `MpCmdRun -Scan -ScanType 3 -File … -DisableRemediation`,
c'est-à-dire en mode rapport seul : aucune mise en quarantaine, aucun réglage
modifié. Les chaînes de caractères ajoutées depuis la beta.5 ne contiennent rien
qui ressemble à un comportement malveillant (pas de PowerShell, de `taskkill`,
de clé de registre « Run »…).

**Conclusion : c'est un faux positif attaché à CE fichier (à son empreinte), et
non au code de la 0.5.0.** Le modèle du cloud a jugé suspect un exe neuf, non
signé et jamais vu ailleurs. Une recompilation identique ne déclenche rien.

Le même risque existe pour **chaque nouvelle version non signée** : jusqu'ici,
1 version sur une douzaine a été touchée.

## 3. Ce qui a été fait

- L'annonce de la 0.5.0 a été retirée :
  - `latest.json` est revenu à la 0.4.8 (vérifié : une 0.4.7 se voit proposer
    la 0.4.8, une 0.4.8 et une 0.5.0 ne reçoivent plus rien) ;
  - les pages du site sont revenues à la 0.4.8.
- `update-0.5.0.gws` et le setup 0.5.0 restent dans `/srv/gws-updates`, mais
  avec les droits `600` : le serveur web répond **403**. Ça évite qu'un fichier
  classé malveillant reste téléchargeable sur gastinel.me, ce qui pourrait nuire
  à la réputation du domaine auprès de SmartScreen et de Google Safe Browsing.
  Pour annuler : `chmod 644` sur ces deux fichiers.
- Rien n'a été supprimé : des copies sont dans `~/staging/gws-stable-050`.

## 4. Les solutions de signature (Authenticode)

| Option | Coût | Ouvert à Théo (particulier, France) ? | Effet |
|---|---|---|---|
| **Azure Artifact Signing** (ex-Trusted Signing) | 9,99 $/mois | **Non en tant que particulier** : seulement aux États-Unis et au Canada. Oui pour une **organisation** de l'UE, ce qui demande une entité légale, un identifiant d'entreprise et une adresse e-mail sur le domaine de l'entité (à vérifier pour une micro-entreprise). | La réputation se construit au fil des versions signées |
| **Certificat OV standard « dans le cloud »** chez Certum (SimplySign) | **à partir de 209 € TTC** (cloud) ou 169 € TTC (carte à puce et lecteur) | **Oui**, il peut être au nom d'une personne physique | Pareil : réputation progressive, nom de l'éditeur affiché |
| Certificat OV chez un autre fournisseur (Sectigo, DigiCert, SSL.com…) | 150 à 300 $/an d'après Microsoft | Oui | Pareil |
| Certificat EV | 330 à 400 € et plus | Oui | **Ne fait plus mieux qu'un OV** depuis 2024 (doc Microsoft) |
| Certum « Open Source » (49 € TTC) ou SignPath Foundation (gratuit) | 0 à 49 € | **Seulement si GWS devient open source** (licence OSI, aucun composant propriétaire) | Pareil qu'un OV |
| Microsoft Store (MSIX) | gratuit | Oui | Aucun avertissement, mais Tauri ne produit pas de MSIX : gros chantier |

Points importants, tirés des pages officielles :
- **Depuis le 27/02/2026, un certificat de signature de code est valable au plus
  459 jours.** Une offre de 2 ou 3 ans demande donc des rééditions.
- **La clé privée doit être sur un matériel sécurisé** (règle du CA/B Forum
  depuis juin 2023) : soit une carte et un lecteur, soit un HSM dans le cloud.
  Avec Certum SimplySign, on signe depuis le PC grâce à SimplySign Desktop, avec
  un code à usage unique généré sur le téléphone.
- **Signer ne supprime pas tout de suite les avertissements.** Microsoft le dit
  clairement : un nouvel exe signé peut encore déclencher SmartScreen tant que
  la réputation n'est pas construite. En revanche, la réputation **s'accumule
  sur l'éditeur** d'une version à l'autre, alors qu'un exe non signé repart de
  zéro à chaque mise à jour. Et un exe signé par un éditeur identifié a beaucoup
  moins de chances d'être classé « cheval de Troie » par le modèle du cloud.
- **Tauri sait signer pendant la compilation** (`bundle.windows.certificateThumbprint`,
  ou `signCommand` pour un outil externe). Ça signe l'exe et l'installateur.
  La signature Authenticode s'ajoute à notre signature Ed25519 des `.gws` sans
  la remplacer : l'appli continue de vérifier `manifest.sig`.

## 5. Recommandation

1. **Tout de suite (Théo, avec un compte Microsoft)** : déclarer le faux positif
   sur https://www.microsoft.com/wdsi/filesubmission, en choisissant « Software
   developer » puis « Incorrectly detected as malware ». Fichier :
   `update-0.5.0.gws` ou le setup 0.5.0 (des copies sont dans
   `~/staging/gws-stable-050` sur le VPS et dans `GWS-050-final`). Détection :
   `Trojan:Win32/Bearfoos.A!ml`.
2. **Republier la 0.5.0 sans attendre de certificat** : une nouvelle compilation
   du même code donne une autre empreinte, qui n'est pas détectée (tests du
   point 2). Il faut la signer avec la clé `maj-2026-a` (ton mot de passe).
   Nouvelle étape de publication proposée : **installer d'abord le `.gws` sur
   ton PC, par le même chemin que les utilisateurs, et attendre quelques
   minutes** avant de mettre à jour `latest.json`. C'est exactement ce qui
   aurait détecté le problème ce matin, avant tout utilisateur.

   **Déjà prêt :** une 0.5.0 recompilée (exe et setup) dans
   `_CODE\GWS-defender\src-tauri\target\release\` (worktree au commit
   `73b2e6a`). Ni l'un ni l'autre n'est détecté, et l'exe a tourné 60 s sans
   réaction de Defender (03/10, 10 h 15). Empreintes : exe `454eb2260ec02b29…`,
   setup `25fd1eeb2912fcd9…`.
3. **À moyen terme : un certificat OV Certum « Standard Code Signing in the
   Cloud »** (environ 209 € par période de 459 jours au plus). C'est la seule
   option ouverte à un particulier en France pour une appli qui n'est pas open
   source. Si tu crées un jour une structure (entreprise, association), Azure
   Artifact Signing devient possible à 9,99 $/mois.

## Sources

- [Quickstart: Set up Artifact Signing (Microsoft Learn, mis à jour le 29/09/2026)](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart)
- [Code signing options for Windows app developers (Microsoft Learn, 29/08/2026)](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options)
- [SmartScreen reputation for Windows app developers (Microsoft Learn, mis à jour le 17/08/2026)](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)
- [Artifact Signing - Pricing (Azure)](https://azure.microsoft.com/en-us/pricing/details/artifact-signing/)
- [France absente de la liste des identités individuelles (Microsoft Q&A, 06/03/2026)](https://learn.microsoft.com/en-us/answers/questions/5810735/cant-create-a-new-trusted-signing-individual-ident)
- [Certum : offres Code Signing](https://shop.certum.eu/code-signing.html) et [Standard Code Signing in the Cloud](https://shop.certum.eu/standard-code-signing-in-the-cloud.html)
- [Certum : Open Source Code Signing in the Cloud](https://shop.certum.eu/open-source-code-signing-on-simplysign.html)
- [SignPath Foundation : conditions](https://signpath.org/terms)
- [Tauri v2 : Windows Code Signing](https://v2.tauri.app/distribute/sign/windows/)
- [Submit files for analysis by Microsoft (Microsoft Learn)](https://learn.microsoft.com/en-us/defender-xdr/submission-guide)

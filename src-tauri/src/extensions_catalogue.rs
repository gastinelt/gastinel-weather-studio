//! Catalogue d'extensions : téléchargement depuis gastinel.me, vérification,
//! installation, désinstallation.
//!
//! Les extensions ne sont **pas** livrées avec l'application. Elles se
//! téléchargent depuis l'écran Paramètres → Extensions, uniquement :
//!
//! ```text
//! https://gastinel.me/gws/downloads/extensions/catalogue.json
//!                                             /<id>-<version>.zip
//! ```
//!
//! Une extension est une `.dll` chargée dans le processus, avec tous les droits
//! de l'app : l'archive doit donc être **signée**. Chaque entrée du catalogue
//! porte l'empreinte SHA-256 de l'archive et une signature Ed25519 sur
//! (id, version, version d'API, empreinte), vérifiée avec [`CLE_PUBLIQUE`].
//! La clé privée reste sur le poste de publication
//! (`scripts/extensions/publier_extension.py`). Un serveur compromis ne peut
//! donc pas faire installer une DLL de son cru.
//!
//! Windows interdit de remplacer une DLL chargée : la mise à jour d'une
//! extension active est préparée dans `extensions-en-attente/` et appliquée au
//! prochain démarrage, avant tout chargement ([`appliquer_en_attente`]).

use std::io::Read;
use std::path::{Path, PathBuf};

use base64::prelude::*;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::extensions::{ExtensionHost, Manifest};
use gws_extension_api as api;

/// Clé publique de signature des extensions (générée le 25/09/2026 par
/// `publier_extension.py generer-cle`).
pub const CLE_PUBLIQUE: [u8; 32] = [
    0x75, 0x2c, 0xf2, 0x0a, 0x33, 0xd0, 0x91, 0xc6, 0x1d, 0xed, 0x6b, 0xa2, 0x7d, 0xc6, 0xec, 0x8b, 0x6c, 0xaf, 0xed,
    0x14, 0x37, 0x77, 0xe9, 0x9a, 0x5b, 0x3b, 0x38, 0x52, 0x53, 0x99, 0x1a, 0x1f,
];

pub const URL_CATALOGUE: &str = "https://gastinel.me/gws/downloads/extensions/catalogue.json";
const PREFIXE_ARCHIVES: &str = "https://gastinel.me/gws/downloads/extensions/";
const TAILLE_MAX: u64 = 64 * 1024 * 1024;
const DOSSIER_ATTENTE: &str = "extensions-en-attente";

/// Extensions retirées du logiciel. Elles ne sont plus proposées, même si le
/// catalogue en ligne les liste encore, et celles déjà installées sont
/// supprimées au démarrage ([`retirer_extensions_abandonnees`]).
///
/// `thermique` : retirée en 0.5.5, elle ne correspond plus à la direction du
/// logiciel (décision de Théo, 04/10/2026).
pub const EXTENSIONS_ABANDONNEES: &[&str] = &["thermique"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntreeCatalogue {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    #[serde(default)]
    pub description: String,
    pub size: u64,
    pub sha256: String,
    pub signature: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
struct Catalogue {
    #[serde(default)]
    extensions: Vec<EntreeCatalogue>,
}

/// Ligne affichée dans l'écran Extensions : l'offre du catalogue et ce qui est installé.
#[derive(Debug, Clone, Serialize)]
pub struct OffreExtension {
    #[serde(flatten)]
    pub entree: EntreeCatalogue,
    pub installed_version: Option<String>,
    /// L'API de l'extension correspond-elle à celle de cette version de l'app ?
    pub compatible: bool,
    /// Une mise à jour attend le prochain démarrage.
    pub pending_restart: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RapportInstallation {
    pub id: String,
    pub version: String,
    /// `true` : l'extension était active, la nouvelle version s'appliquera au
    /// prochain démarrage de l'application.
    pub restart_required: bool,
}

/// Message signé. Doit rester identique à `message_signe` dans
/// scripts/extensions/publier_extension.py.
fn message_signe(e: &EntreeCatalogue) -> Vec<u8> {
    format!("gws-extension-v1\0{}\0{}\0{}\0{}", e.id, e.version, e.api_version, e.sha256).into_bytes()
}

/// Identifiant utilisable comme nom de dossier : minuscules, chiffres, tirets.
fn id_valide(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}

/// Contrôles de l'entrée avant tout téléchargement : adresse, taille, signature.
pub fn verifier_entree(e: &EntreeCatalogue, cle: &[u8; 32]) -> Result<(), String> {
    if !id_valide(&e.id) {
        return Err(format!("identifiant d'extension invalide : {}", e.id));
    }
    if !e.url.starts_with(PREFIXE_ARCHIVES) || e.url.contains("..") || e.url.len() > 300 {
        return Err(format!("{} : adresse de téléchargement non autorisée", e.id));
    }
    if e.size == 0 || e.size > TAILLE_MAX {
        return Err(format!("{} : taille annoncée invalide", e.id));
    }
    let cle = VerifyingKey::from_bytes(cle).map_err(|e| format!("clé publique invalide : {e}"))?;
    let sig = BASE64_STANDARD.decode(&e.signature).map_err(|_| format!("{} : signature illisible", e.id))?;
    let sig = Signature::from_slice(&sig).map_err(|_| format!("{} : signature illisible", e.id))?;
    cle.verify(&message_signe(e), &sig)
        .map_err(|_| format!("{} : signature invalide, extension refusée", e.id))
}

/// L'archive téléchargée correspond-elle à l'entrée signée ?
pub fn verifier_archive(e: &EntreeCatalogue, octets: &[u8]) -> Result<(), String> {
    if octets.len() as u64 != e.size {
        return Err(format!("{} : taille reçue différente de celle annoncée", e.id));
    }
    let h = format!("{:x}", Sha256::digest(octets));
    if !h.eq_ignore_ascii_case(&e.sha256) {
        return Err(format!("{} : empreinte SHA-256 différente, archive refusée", e.id));
    }
    Ok(())
}

fn client() -> Result<reqwest::blocking::Client, String> {
    crate::http_client::client_builder()
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(10))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("client HTTP : {e}"))
}

fn telecharger(url: &str, max: u64) -> Result<Vec<u8>, String> {
    let r = client()?.get(url).send().map_err(|e| format!("gastinel.me injoignable : {e}"))?;
    if r.status().as_u16() == 404 {
        return Err("404".into());
    }
    if !r.status().is_success() {
        return Err(format!("gastinel.me : HTTP {}", r.status().as_u16()));
    }
    let mut octets = Vec::new();
    r.take(max + 1).read_to_end(&mut octets).map_err(|e| format!("lecture : {e}"))?;
    if octets.len() as u64 > max {
        return Err("réponse trop volumineuse".into());
    }
    Ok(octets)
}

/// Catalogue en ligne, croisé avec ce qui est installé. Les entrées dont la
/// signature ne vérifie pas sont écartées (et signalées dans la console).
pub fn offres(host: &ExtensionHost) -> Result<Vec<OffreExtension>, String> {
    let brut = match telecharger(URL_CATALOGUE, 1024 * 1024) {
        Ok(b) => b,
        Err(e) if e == "404" => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let cat: Catalogue = serde_json::from_slice(&brut).map_err(|e| format!("catalogue illisible : {e}"))?;
    let installees = host.list();
    let attente = host.data_dir().join(DOSSIER_ATTENTE);
    Ok(cat
        .extensions
        .into_iter()
        .filter(|e| !EXTENSIONS_ABANDONNEES.contains(&e.id.as_str()))
        .filter(|e| match verifier_entree(e, &CLE_PUBLIQUE) {
            Ok(()) => true,
            Err(m) => {
                eprintln!("[gws] catalogue : {m}");
                false
            }
        })
        .map(|e| OffreExtension {
            installed_version: installees.iter().find(|i| i.manifest.id == e.id).map(|i| i.manifest.version.clone()),
            compatible: e.api_version == api::API_VERSION,
            pending_restart: attente.join(&e.id).is_dir(),
            entree: e,
        })
        .collect())
}

/// Télécharge, vérifie et installe une extension du catalogue.
pub fn installer(host: &ExtensionHost, id: &str) -> Result<RapportInstallation, String> {
    let offre = offres(host)?
        .into_iter()
        .find(|o| o.entree.id == id)
        .ok_or_else(|| format!("{id} n'est pas (ou plus) au catalogue"))?;
    if !offre.compatible {
        return Err(format!(
            "{id} {} demande l'API v{}, cette version de l'application fournit la v{} : mettez l'application à jour",
            offre.entree.version,
            offre.entree.api_version,
            api::API_VERSION
        ));
    }
    let e = offre.entree;
    let octets = telecharger(&e.url, TAILLE_MAX)?;
    verifier_archive(&e, &octets)?;
    installer_archive(host, &e, &octets)
}

/// Décompresse une archive vérifiée et la met en place (ou en attente).
pub fn installer_archive(host: &ExtensionHost, e: &EntreeCatalogue, octets: &[u8]) -> Result<RapportInstallation, String> {
    let racine = host.dossier_installation();
    std::fs::create_dir_all(&racine).map_err(|er| er.to_string())?;
    let tmp = racine.join(format!(".installation-{}", e.id));
    let _ = std::fs::remove_dir_all(&tmp);
    extraire(octets, &tmp).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&tmp);
    })?;
    let controle = (|| {
        let m: Manifest = serde_json::from_str(
            &std::fs::read_to_string(tmp.join("manifest.json")).map_err(|_| "manifest.json absent de l'archive".to_string())?,
        )
        .map_err(|er| format!("manifest.json illisible : {er}"))?;
        if m.id != e.id || m.version != e.version || m.api_version != e.api_version {
            return Err("le manifeste de l'archive ne correspond pas au catalogue".to_string());
        }
        if !tmp.join(&m.library).is_file() {
            return Err(format!("bibliothèque {} absente de l'archive", m.library));
        }
        Ok(())
    })();
    if let Err(m) = controle {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(m);
    }

    let restart_required = host.est_chargee(&e.id);
    let cible = if restart_required {
        let attente = host.data_dir().join(DOSSIER_ATTENTE);
        std::fs::create_dir_all(&attente).map_err(|er| er.to_string())?;
        attente.join(&e.id)
    } else {
        racine.join(&e.id)
    };
    remplacer(&tmp, &cible)?;
    if !restart_required {
        host.decouvrir();
    }
    Ok(RapportInstallation { id: e.id.clone(), version: e.version.clone(), restart_required })
}

/// Remplace `cible` par `source` (renommage ; l'ancienne version est d'abord
/// mise de côté, puis supprimée).
fn remplacer(source: &Path, cible: &Path) -> Result<(), String> {
    let ancien = cible.with_extension("ancien");
    let _ = std::fs::remove_dir_all(&ancien);
    if cible.exists() {
        std::fs::rename(cible, &ancien).map_err(|e| format!("ancienne version verrouillée : {e}"))?;
    }
    if let Err(e) = std::fs::rename(source, cible) {
        if ancien.exists() {
            let _ = std::fs::rename(&ancien, cible);
        }
        return Err(format!("mise en place impossible : {e}"));
    }
    let _ = std::fs::remove_dir_all(&ancien);
    Ok(())
}

/// Décompresse en refusant tout chemin qui sortirait du dossier.
fn extraire(octets: &[u8], dest: &Path) -> Result<(), String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(octets)).map_err(|e| format!("archive illisible : {e}"))?;
    let mut total = 0u64;
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|e| format!("archive illisible : {e}"))?;
        let Some(rel) = f.enclosed_name() else {
            return Err(format!("chemin refusé dans l'archive : {}", f.name()));
        };
        let chemin = dest.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&chemin).map_err(|e| e.to_string())?;
            continue;
        }
        total += f.size();
        if total > 4 * TAILLE_MAX {
            return Err("archive trop volumineuse une fois décompressée".into());
        }
        if let Some(p) = chemin.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut sortie = std::fs::File::create(&chemin).map_err(|e| e.to_string())?;
        std::io::copy(&mut f, &mut sortie).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Désinstalle une extension téléchargée : déchargée d'abord, puis son
/// dossier supprimé. Celles livrées à côté de l'exécutable ne sont pas touchées.
pub fn desinstaller(host: &ExtensionHost, id: &str) -> Result<(), String> {
    let info = host.list().into_iter().find(|i| i.manifest.id == id).ok_or_else(|| format!("extension inconnue : {id}"))?;
    let dossier = PathBuf::from(&info.dir);
    if !dossier.starts_with(host.dossier_installation()) {
        return Err("cette extension n'a pas été installée depuis le catalogue".into());
    }
    if info.enabled {
        host.set_enabled(id, false)?;
    }
    host.decharger(id);
    std::fs::remove_dir_all(&dossier).map_err(|e| format!("suppression impossible : {e}"))?;
    let _ = std::fs::remove_dir_all(host.data_dir().join(DOSSIER_ATTENTE).join(id));
    host.oublier(id);
    Ok(())
}

/// Au démarrage, **avant** de créer l'hôte (aucune DLL n'est encore chargée) :
/// supprime les extensions retirées du logiciel, installées depuis le catalogue
/// ou en attente de mise à jour. Les données qu'elles ont produites
/// (`extensions-donnees/<id>`) restent, comme pour une désinstallation.
pub fn retirer_extensions_abandonnees(data_dir: &Path) {
    for id in EXTENSIONS_ABANDONNEES {
        for dossier in [data_dir.join("extensions").join(id), data_dir.join(DOSSIER_ATTENTE).join(id)] {
            if !dossier.is_dir() {
                continue;
            }
            match std::fs::remove_dir_all(&dossier) {
                Ok(()) => println!("[gws] extension {id} retirée du logiciel : {} supprimé", dossier.display()),
                Err(e) => eprintln!("[gws] extension {id} : suppression de {} impossible : {e}", dossier.display()),
            }
        }
    }
}

/// Au démarrage, **avant** de créer l'hôte : applique les mises à jour
/// préparées pendant que l'ancienne version était chargée.
pub fn appliquer_en_attente(data_dir: &Path) {
    let attente = data_dir.join(DOSSIER_ATTENTE);
    let Ok(lecture) = std::fs::read_dir(&attente) else { return };
    let racine = data_dir.join("extensions");
    let _ = std::fs::create_dir_all(&racine);
    for d in lecture.flatten() {
        let id = d.file_name().to_string_lossy().to_string();
        if !d.path().is_dir() || !id_valide(&id) {
            continue;
        }
        match remplacer(&d.path(), &racine.join(&id)) {
            Ok(()) => println!("[gws] extension {id} : mise à jour appliquée"),
            Err(e) => eprintln!("[gws] extension {id} : mise à jour en attente non appliquée : {e}"),
        }
    }
}

pub mod commands {
    use super::{OffreExtension, RapportInstallation};
    use crate::extensions::ExtensionHost;
    use std::sync::Arc;
    use tauri::State;

    #[tauri::command(rename_all = "snake_case")]
    pub async fn extensions_catalogue(host: State<'_, Arc<ExtensionHost>>) -> Result<Vec<OffreExtension>, String> {
        let host = host.inner().clone();
        tauri::async_runtime::spawn_blocking(move || super::offres(&host)).await.map_err(|e| e.to_string())?
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn extension_install(host: State<'_, Arc<ExtensionHost>>, id: String) -> Result<RapportInstallation, String> {
        let host = host.inner().clone();
        tauri::async_runtime::spawn_blocking(move || super::installer(&host, &id)).await.map_err(|e| e.to_string())?
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn extension_uninstall(host: State<'_, Arc<ExtensionHost>>, id: String) -> Result<(), String> {
        let host = host.inner().clone();
        tauri::async_runtime::spawn_blocking(move || super::desinstaller(&host, &id)).await.map_err(|e| e.to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn cle_test() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn archive(id: &str, version: &str) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("manifest.json", o).unwrap();
            std::io::Write::write_all(
                &mut z,
                format!(
                    r#"{{"id":"{id}","name":"T","version":"{version}","api_version":{},"library":"t.dll","ui_entry":"index.html"}}"#,
                    api::API_VERSION
                )
                .as_bytes(),
            )
            .unwrap();
            z.start_file("t.dll", o).unwrap();
            std::io::Write::write_all(&mut z, b"pas une vraie dll").unwrap();
            z.start_file("ui/index.html", o).unwrap();
            std::io::Write::write_all(&mut z, b"<p>ok</p>").unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    fn entree(id: &str, version: &str, octets: &[u8], cle: &SigningKey) -> EntreeCatalogue {
        let mut e = EntreeCatalogue {
            id: id.into(),
            name: "T".into(),
            version: version.into(),
            api_version: api::API_VERSION,
            description: String::new(),
            size: octets.len() as u64,
            sha256: format!("{:x}", Sha256::digest(octets)),
            signature: String::new(),
            url: format!("{PREFIXE_ARCHIVES}{id}-{version}.zip"),
        };
        e.signature = BASE64_STANDARD.encode(cle.sign(&message_signe(&e)).to_bytes());
        e
    }

    fn dossier_tmp(nom: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gws-cat-{nom}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn signature_valide_acceptee_et_toute_alteration_refusee() {
        let k = cle_test();
        let pk = k.verifying_key().to_bytes();
        let o = archive("demo", "1.0.0");
        let e = entree("demo", "1.0.0", &o, &k);
        assert!(verifier_entree(&e, &pk).is_ok());
        assert!(verifier_archive(&e, &o).is_ok());

        // Version, empreinte ou adresse modifiées : refus.
        let mut v = e.clone();
        v.version = "9.9.9".into();
        assert!(verifier_entree(&v, &pk).is_err());
        let mut h = e.clone();
        h.sha256 = "00".repeat(32);
        assert!(verifier_entree(&h, &pk).is_err());
        let mut u = e.clone();
        u.url = "https://evil.example/demo.zip".into();
        assert!(verifier_entree(&u, &pk).is_err());
        // Clé de l'app (différente de la clé de test) : refus.
        assert!(verifier_entree(&e, &CLE_PUBLIQUE).is_err());
        // Archive modifiée après signature : refus.
        let mut o2 = o.clone();
        let n = o2.len();
        o2[n / 2] ^= 1;
        assert!(verifier_archive(&e, &o2).is_err());
    }

    #[test]
    fn installation_puis_mise_a_jour_puis_desinstallation() {
        let d = dossier_tmp("install");
        let host = ExtensionHost::with_roots(&d, &d.join("x.sqlite3"), "t", vec![d.join("extensions")]);
        let k = cle_test();
        let o = archive("demo", "1.0.0");
        let r = installer_archive(&host, &entree("demo", "1.0.0", &o, &k), &o).unwrap();
        assert!(!r.restart_required);
        assert_eq!(host.list()[0].manifest.version, "1.0.0");
        assert!(d.join("extensions/demo/ui/index.html").is_file());

        let o2 = archive("demo", "1.0.1");
        installer_archive(&host, &entree("demo", "1.0.1", &o2, &k), &o2).unwrap();
        assert_eq!(host.list()[0].manifest.version, "1.0.1");

        desinstaller(&host, "demo").unwrap();
        assert!(host.list().is_empty());
        assert!(!d.join("extensions/demo").exists());
    }

    #[test]
    fn archive_dont_le_manifeste_ne_correspond_pas_refusee() {
        let d = dossier_tmp("manifeste");
        let host = ExtensionHost::with_roots(&d, &d.join("x.sqlite3"), "t", vec![d.join("extensions")]);
        let o = archive("autre", "1.0.0");
        let e = entree("demo", "1.0.0", &o, &cle_test());
        assert!(installer_archive(&host, &e, &o).is_err());
        assert!(!d.join("extensions/demo").exists());
        assert!(!d.join("extensions/.installation-demo").exists(), "dossier temporaire nettoyé");
    }

    #[test]
    fn mise_a_jour_en_attente_appliquee_au_demarrage() {
        let d = dossier_tmp("attente");
        let prete = d.join(DOSSIER_ATTENTE).join("demo");
        std::fs::create_dir_all(&prete).unwrap();
        std::fs::write(prete.join("manifest.json"), "v2").unwrap();
        std::fs::create_dir_all(d.join("extensions/demo")).unwrap();
        std::fs::write(d.join("extensions/demo/manifest.json"), "v1").unwrap();
        appliquer_en_attente(&d);
        assert_eq!(std::fs::read_to_string(d.join("extensions/demo/manifest.json")).unwrap(), "v2");
        assert!(!prete.exists());
    }

    #[test]
    fn extension_abandonnee_supprimee_au_demarrage() {
        let d = dossier_tmp("abandon");
        for dossier in ["extensions/thermique", "extensions-en-attente/thermique", "extensions/carte3d", "extensions-donnees/thermique"] {
            std::fs::create_dir_all(d.join(dossier)).unwrap();
            std::fs::write(d.join(dossier).join("manifest.json"), "{}").unwrap();
        }
        retirer_extensions_abandonnees(&d);
        assert!(!d.join("extensions/thermique").exists());
        assert!(!d.join("extensions-en-attente/thermique").exists());
        assert!(d.join("extensions/carte3d").is_dir(), "les autres extensions restent");
        assert!(d.join("extensions-donnees/thermique").is_dir(), "les données produites restent");
        // Deuxième démarrage : rien à faire, rien ne casse.
        retirer_extensions_abandonnees(&d);
    }

    /// Ce que `publier_extension.py paquet` a produit passe la vérification de
    /// l'app, avec la vraie clé publique : Python et Rust signent le même message.
    #[test]
    #[ignore = "demande scripts/extensions/dist (publier_extension.py paquet)"]
    fn paquets_publies_verifies_avec_la_cle_de_l_app() {
        let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/extensions/dist");
        let cat: Catalogue = serde_json::from_slice(&std::fs::read(dist.join("catalogue.json")).unwrap()).unwrap();
        assert!(!cat.extensions.is_empty());
        for e in &cat.extensions {
            verifier_entree(e, &CLE_PUBLIQUE).unwrap();
            let octets = std::fs::read(dist.join(format!("{}-{}.zip", e.id, e.version))).unwrap();
            verifier_archive(e, &octets).unwrap();
            let d = dossier_tmp(&format!("publie-{}", e.id));
            let host = ExtensionHost::with_roots(&d, &d.join("x.sqlite3"), "t", vec![d.join("extensions")]);
            installer_archive(&host, e, &octets).unwrap();
            assert_eq!(host.list()[0].manifest.version, e.version);
        }
    }

    #[test]
    #[ignore = "dépend du réseau (gastinel.me)"]
    fn appel_reel_catalogue_et_installation() {
        let d = dossier_tmp("reel");
        let host = ExtensionHost::with_roots(&d, &d.join("x.sqlite3"), "t", vec![d.join("extensions")]);
        let liste = offres(&host).unwrap();
        assert!(liste.iter().any(|o| o.entree.id == "carte3d"), "{liste:?}");
        assert!(!liste.iter().any(|o| o.entree.id == "thermique"), "extension retirée encore proposée");
        let r = installer(&host, "carte3d").unwrap();
        assert!(!r.restart_required);
        assert!(d.join("extensions/carte3d/carte3d.dll").is_file());
        assert_eq!(offres(&host).unwrap().iter().find(|o| o.entree.id == "carte3d").unwrap().installed_version.as_deref(), Some(r.version.as_str()));
    }

    #[test]
    fn chemins_sortants_refuses() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("../evade.txt", zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut z, b"x").unwrap();
            z.finish().unwrap();
        }
        let d = dossier_tmp("evasion");
        assert!(extraire(&buf.into_inner(), &d.join("x")).is_err());
        assert!(!d.join("evade.txt").exists());
    }
}

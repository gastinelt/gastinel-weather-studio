//! Gestion des paquets `.gws` (mises à jour directes sans installeur externe).
//!
//! Un paquet `.gws` est une archive compressée autonome (format ZIP standard)
//! contenant un `manifest.json` descriptif et l'exécutable `gastinel-weather-studio.exe`.
//!
//! ## Authenticité (signature Ed25519)
//!
//! Depuis la 0.5.0, un paquet doit contenir `manifest.sig` : une signature
//! Ed25519 du message
//! `gws-update-v1\0<version>\0<min_compatible_version|"">\0<checksum_sha256>\0<taille exe>`,
//! produite hors de l'application par `scripts/signer_maj.py` avec une clé
//! privée qui ne quitte pas le poste de publication. La vérification a lieu dans
//! [`inspect`], donc **avant** tout affichage « installer » et avant
//! [`apply_update`] ; un paquet sans signature, signé par une clé inconnue ou
//! dont le manifeste ou l'exécutable ont été modifiés est refusé, sans option
//! « installer quand même ». Le SHA-256 seul ne suffisait pas : quiconque
//! fabrique un `.gws` recalcule aussi l'empreinte de son propre manifeste.
//!
//! Les clés publiques acceptées sont dans [`CLES_MAJ`] (distinctes de la clé
//! des extensions). Les versions antérieures ignorent `manifest.sig` : un paquet
//! signé reste installable par elles.
//!
//! Lors de l'application :
//! 1. Le condensat SHA-256 du binaire extrait est vérifié par rapport au manifeste.
//! 2. L'exécutable en cours d'exécution est renommé en `.old` (l'OS Windows autorise le
//!    renommage d'un binaire actif, mais interdit son écrasement direct).
//! 3. Le nouveau binaire est écrit à l'emplacement standard.
//! 4. La nouvelle version est démarrée en sous-processus détaché, et l'instance
//!    actuelle quitte proprement.
//! 5. Un gardien détaché (l'ancienne version, depuis sa copie `.old`) attend
//!    que la nouvelle confirme son démarrage ; sinon il la termine et restaure
//!    l'ancienne. Voir [`crate::maj_gardien`].

use std::fs::File;
use std::io::Read;
use std::path::Path;
use base64::prelude::*;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};
use crate::update::{parse_version, CURRENT_VERSION};

const MANIFEST_NAME: &str = "manifest.json";
const BINARY_NAME: &str = "gastinel-weather-studio.exe";
const SIGNATURE_NAME: &str = "manifest.sig";

/// Clés publiques Ed25519 acceptées pour les mises à jour : `(key_id, clé)`.
///
/// **Vide tant que le propriétaire n'a pas généré la clé de production**
/// (`python scripts/signer_maj.py generer-cle ...`, voir
/// `docs/signature-maj.md`). Liste vide : tout paquet `.gws` est refusé.
/// C'est voulu : mieux vaut aucune installation qu'une installation non vérifiée.
/// Prévoir deux entrées : la clé active et une clé de secours gardée hors ligne.
pub const CLES_MAJ: &[(&str, [u8; 32])] = &[
    ("maj-2026-a", [0x3a, 0x10, 0x1b, 0x1d, 0x6c, 0x13, 0xaf, 0x2c, 0x9f, 0x8f, 0xbd, 0xeb, 0xb8, 0x21, 0x02, 0xe5, 0x17, 0xf7, 0x57, 0x32, 0xf1, 0x12, 0x21, 0x2e, 0xca, 0x03, 0x72, 0x11, 0x2b, 0x43, 0xbd, 0x82]),
    ("maj-secours-1", [0xc0, 0x00, 0x94, 0xf5, 0xfa, 0xc1, 0x5d, 0x03, 0x24, 0x8e, 0xc0, 0xd5, 0x6c, 0x75, 0x1c, 0xfd, 0xf2, 0x2c, 0xf6, 0xa1, 0xd4, 0xac, 0xf6, 0x20, 0x34, 0xfe, 0xa7, 0xc3, 0x4e, 0x8e, 0xd8, 0xf0]),
];

/// Contenu de `manifest.sig`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignatureMaj {
    pub format: u32,
    pub key_id: String,
    /// Signature Ed25519 (64 octets) en base64 standard.
    pub signature: String,
}

/// Message signé ; doit rester identique à `message_signe` de `scripts/signer_maj.py`.
pub fn message_signe(m: &PackageManifest, taille_exe: u64) -> Vec<u8> {
    format!(
        "gws-update-v1\0{}\0{}\0{}\0{}",
        m.version,
        m.min_compatible_version.as_deref().unwrap_or(""),
        m.checksum_sha256.to_ascii_lowercase(),
        taille_exe
    )
    .into_bytes()
}

/// Vérifie `manifest.sig` (contenu brut, `None` s'il est absent) contre le manifeste.
pub fn verifier_signature(
    m: &PackageManifest,
    taille_exe: u64,
    sig_brute: Option<&str>,
    cles: &[(&str, [u8; 32])],
) -> Result<(), String> {
    const REFUS: &str = "Mise à jour refusée : ";
    let brute = sig_brute.ok_or_else(|| {
        format!("{REFUS}le paquet n'est pas signé. Téléchargez la mise à jour depuis https://gastinel.me/gws/.")
    })?;
    let s: SignatureMaj = serde_json::from_str(brute.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("{REFUS}signature illisible ({e})."))?;
    if s.format != 1 {
        return Err(format!("{REFUS}format de signature inconnu ({}).", s.format));
    }
    let cle = cles
        .iter()
        .find(|(id, _)| *id == s.key_id)
        .ok_or_else(|| format!("{REFUS}signé par une clé inconnue (« {} »).", s.key_id))?;
    let cle = VerifyingKey::from_bytes(&cle.1).map_err(|e| format!("{REFUS}clé publique invalide ({e})."))?;
    let octets = BASE64_STANDARD
        .decode(s.signature.trim())
        .map_err(|_| format!("{REFUS}signature illisible."))?;
    let sig = Signature::from_slice(&octets).map_err(|_| format!("{REFUS}signature illisible."))?;
    cle.verify(&message_signe(m, taille_exe), &sig).map_err(|_| {
        format!("{REFUS}signature invalide, le paquet a été modifié ou ne provient pas de l'éditeur.")
    })
}

/// Nom de l'exécutable dans l'archive (racine ou `payload/`).
fn nom_binaire<R: std::io::Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>) -> AppResult<String> {
    if archive.by_name(BINARY_NAME).is_ok() {
        Ok(BINARY_NAME.to_string())
    } else if archive.by_name(&format!("payload/{BINARY_NAME}")).is_ok() {
        Ok(format!("payload/{BINARY_NAME}"))
    } else {
        Err(AppError::Other(format!("L'archive ne contient pas l'exécutable '{BINARY_NAME}'")))
    }
}

/// Métadonnées contenues dans le `manifest.json` d'un paquet `.gws`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageManifest {
    #[serde(rename = "type")]
    pub package_type: String,
    pub version: String,
    #[serde(default)]
    pub min_compatible_version: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub changelog: Option<String>,
    pub checksum_sha256: String,
}

/// Informations complètes sur un paquet inspecté, pour affichage dans l'UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub file_path: String,
    pub manifest: PackageManifest,
    pub current_version: String,
    pub is_newer: bool,
}

/// Inspecte une archive `.gws` sans extraire l'exécutable complet.
/// La signature est vérifiée ici : un paquet refusé n'est jamais proposé.
pub fn inspect(path: &Path) -> AppResult<PackageInfo> {
    inspect_avec_cles(path, CLES_MAJ).map(|(info, _)| info)
}

/// [`inspect`] avec un jeu de clés donné ; renvoie aussi la taille signée de l'exécutable.
fn inspect_avec_cles(path: &Path, cles: &[(&str, [u8; 32])]) -> AppResult<(PackageInfo, u64)> {
    if !path.exists() {
        return Err(AppError::Other(format!(
            "Fichier de paquet introuvable : {}",
            path.display()
        )));
    }

    let file = File::open(path).map_err(|e| {
        AppError::Other(format!("Impossible d'ouvrir le fichier : {e}"))
    })?;

    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        AppError::Other(format!("Archive .gws illisible ou corrompue : {e}"))
    })?;

    let mut manifest_content = String::new();
    {
        let mut manifest_file = archive.by_name(MANIFEST_NAME).map_err(|_| {
            AppError::Other(format!("Le paquet ne contient pas de fichier '{MANIFEST_NAME}'"))
        })?;
        manifest_file.read_to_string(&mut manifest_content).map_err(|e| {
            AppError::Other(format!("Lecture du manifeste impossible : {e}"))
        })?;
    }

    // Un BOM UTF-8 en tête (écrit par PowerShell 5) ferait échouer serde_json.
    let manifest: PackageManifest = serde_json::from_str(manifest_content.trim_start_matches('\u{feff}')).map_err(|e| {
        AppError::Other(format!("Manifeste JSON invalide : {e}"))
    })?;

    if manifest.package_type != "gws-update" {
        return Err(AppError::Other(format!(
            "Type de paquet inconnu : '{}' (attendu: 'gws-update')",
            manifest.package_type
        )));
    }

    let nom = nom_binaire(&mut archive)?;
    let taille_exe = archive
        .by_name(&nom)
        .map_err(|e| AppError::Other(format!("Exécutable illisible : {e}")))?
        .size();
    let sig_brute = match archive.by_name(SIGNATURE_NAME) {
        Ok(mut f) => {
            let mut t = String::new();
            f.read_to_string(&mut t)
                .map_err(|e| AppError::Other(format!("Lecture de la signature impossible : {e}")))?;
            Some(t)
        }
        Err(_) => None,
    };
    verifier_signature(&manifest, taille_exe, sig_brute.as_deref(), cles).map_err(AppError::Other)?;

    let is_newer = match (parse_version(&manifest.version), parse_version(CURRENT_VERSION)) {
        (Some(new_v), Some(cur_v)) => new_v > cur_v,
        _ => false,
    };

    Ok((
        PackageInfo {
            file_path: path.to_string_lossy().to_string(),
            manifest,
            current_version: CURRENT_VERSION.to_string(),
            is_newer,
        },
        taille_exe,
    ))
}

/// Argument passé à la nouvelle version relancée par [`apply_update`].
pub const ARG_APRES_MAJ: &str = "--apres-maj";

/// Applique la mise à jour depuis le paquet `.gws` et relance l'application.
pub fn apply_update(path: &Path) -> AppResult<()> {
    let new_bytes = extraire_verifie(path, CLES_MAJ)?;
    remplacer_et_relancer(&new_bytes)
}

/// Vérifie la signature puis extrait l'exécutable, contrôlé (taille et SHA-256)
/// contre le manifeste signé. Rien n'est écrit sur disque ici.
fn extraire_verifie(path: &Path, cles: &[(&str, [u8; 32])]) -> AppResult<Vec<u8>> {
    let (info, taille_signee) = inspect_avec_cles(path, cles)?;

    let file = File::open(path).map_err(|e| {
        AppError::Other(format!("Impossible de rouvrir le fichier : {e}"))
    })?;

    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        AppError::Other(format!("Archive corrompue : {e}"))
    })?;

    let binary_entry_name = nom_binaire(&mut archive)?;

    let mut binary_file = archive.by_name(&binary_entry_name).map_err(|e| {
        AppError::Other(format!("Extraction impossible de l'exécutable : {e}"))
    })?;

    let mut new_bytes = Vec::new();
    binary_file.read_to_end(&mut new_bytes).map_err(|e| {
        AppError::Other(format!("Lecture de l'exécutable échouée : {e}"))
    })?;

    // Validation du condensat SHA-256
    let mut hasher = Sha256::new();
    hasher.update(&new_bytes);
    let hash_result = format!("{:x}", hasher.finalize());

    if !hash_result.eq_ignore_ascii_case(&info.manifest.checksum_sha256) {
        return Err(AppError::Other(format!(
            "Condensat SHA-256 invalide ! Attendu: {}, Calculé: {}",
            info.manifest.checksum_sha256, hash_result
        )));
    }
    if new_bytes.len() as u64 != taille_signee {
        return Err(AppError::Other("Taille de l'exécutable différente de celle signée".into()));
    }
    Ok(new_bytes)
}

fn remplacer_et_relancer(new_bytes: &[u8]) -> AppResult<()> {

    // Récupération de l'emplacement de l'exécutable courant
    let current_exe = std::env::current_exe().map_err(|e| {
        AppError::Other(format!("Impossible de déterminer l'exécutable courant : {e}"))
    })?;
    let chemins = crate::maj_gardien::Chemins::pour(&current_exe);
    // Renomme l'exe courant en .old, écrit le nouveau, pose le marqueur.
    chemins.installer(new_bytes).map_err(AppError::Other)?;

    #[cfg(windows)]
    {
        // Lance la nouvelle version (--apres-maj) et le gardien, puis quitte.
        crate::maj_gardien::lancer_nouvelle_version(&chemins).map_err(AppError::Other)?;
        std::process::exit(0);
    }

    #[cfg(not(windows))]
    {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    // Paires de TEST, dérivées de graines fixes : ni la clé de production ni sa
    // clé de secours. Elles ne servent qu'ici.
    fn cle_test() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }
    fn autre_cle() -> SigningKey {
        SigningKey::from_bytes(&[9u8; 32])
    }
    fn cles_test() -> Vec<(&'static str, [u8; 32])> {
        vec![("test-a", cle_test().verifying_key().to_bytes())]
    }

    const EXE: &[u8] = b"MZ_DUMMY_EXE_CONTENT";

    fn manifeste(exe: &[u8]) -> PackageManifest {
        PackageManifest {
            package_type: "gws-update".into(),
            version: "99.0.0".into(),
            min_compatible_version: Some("0.1.0".into()),
            release_date: Some("2026-09-24T12:00:00Z".into()),
            title: Some("Version test".into()),
            changelog: Some("- Changement A\n- Changement B".into()),
            checksum_sha256: format!("{:x}", Sha256::digest(exe)),
        }
    }

    fn sig_json(m: &PackageManifest, taille: u64, cle: &SigningKey, key_id: &str) -> String {
        serde_json::to_string(&SignatureMaj {
            format: 1,
            key_id: key_id.into(),
            signature: BASE64_STANDARD.encode(cle.sign(&message_signe(m, taille)).to_bytes()),
        })
        .unwrap()
    }

    /// Écrit un .gws ; `bom` imite les manifestes PowerShell 5 (jusqu'à la 0.4.4).
    fn ecrire_paquet(
        m: &PackageManifest,
        exe: &[u8],
        sig: Option<&str>,
        bom: bool,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("gws_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pkg_path = dir.join("test_update.gws");
        let file = File::create(&pkg_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file(MANIFEST_NAME, options).unwrap();
        if bom {
            zip.write_all(b"\xEF\xBB\xBF").unwrap();
        }
        zip.write_all(serde_json::to_string(m).unwrap().as_bytes()).unwrap();
        zip.start_file(BINARY_NAME, options).unwrap();
        zip.write_all(exe).unwrap();
        if let Some(s) = sig {
            zip.start_file(SIGNATURE_NAME, options).unwrap();
            zip.write_all(s.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        (dir, pkg_path)
    }

    /// Le refus doit tomber dès l'inspection, et donc aussi à l'application.
    fn refus(m: &PackageManifest, exe: &[u8], sig: Option<&str>) -> String {
        let (dir, p) = ecrire_paquet(m, exe, sig, false);
        let e = inspect_avec_cles(&p, &cles_test()).expect_err("doit être refusé").to_string();
        let e2 = extraire_verifie(&p, &cles_test()).expect_err("doit être refusé").to_string();
        assert_eq!(e, e2);
        let _ = std::fs::remove_dir_all(&dir);
        e
    }

    #[test]
    fn paquet_signe_accepte() {
        let m = manifeste(EXE);
        let sig = sig_json(&m, EXE.len() as u64, &cle_test(), "test-a");
        let (dir, p) = ecrire_paquet(&m, EXE, Some(&sig), true);
        let (info, _) = inspect_avec_cles(&p, &cles_test()).expect("L'inspection doit réussir");
        assert_eq!(info.manifest.version, "99.0.0");
        assert!(info.is_newer);
        assert_eq!(info.manifest.title.as_deref(), Some("Version test"));
        assert_eq!(extraire_verifie(&p, &cles_test()).unwrap(), EXE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paquet_sans_signature_refuse() {
        assert!(refus(&manifeste(EXE), EXE, None).contains("pas signé"));
    }

    #[test]
    fn signature_d_une_autre_cle_refusee() {
        let m = manifeste(EXE);
        // Même key_id, mauvaise clé : signature invalide.
        let sig = sig_json(&m, EXE.len() as u64, &autre_cle(), "test-a");
        assert!(refus(&m, EXE, Some(&sig)).contains("signature invalide"));
        // key_id inconnu.
        let sig = sig_json(&m, EXE.len() as u64, &autre_cle(), "pirate");
        assert!(refus(&m, EXE, Some(&sig)).contains("clé inconnue"));
    }

    #[test]
    fn signature_illisible_refusee() {
        let m = manifeste(EXE);
        assert!(refus(&m, EXE, Some("placeholder_sig")).contains("illisible"));
        let s = r#"{"format":1,"key_id":"test-a","signature":"AAAA"}"#;
        assert!(refus(&m, EXE, Some(s)).contains("illisible"));
        let s = r#"{"format":2,"key_id":"test-a","signature":"AAAA"}"#;
        assert!(refus(&m, EXE, Some(s)).contains("format de signature inconnu"));
    }

    #[test]
    fn manifeste_altere_refuse() {
        let m = manifeste(EXE);
        let sig = sig_json(&m, EXE.len() as u64, &cle_test(), "test-a");
        // Version changée après signature (version forgée ou retour arrière).
        let mut m2 = m.clone();
        m2.version = "99.0.1".into();
        assert!(refus(&m2, EXE, Some(&sig)).contains("signature invalide"));
        let mut m3 = m.clone();
        m3.min_compatible_version = None;
        assert!(refus(&m3, EXE, Some(&sig)).contains("signature invalide"));
    }

    #[test]
    fn executable_remplace_refuse() {
        let m = manifeste(EXE);
        let sig = sig_json(&m, EXE.len() as u64, &cle_test(), "test-a");
        // Exécutable forgé, manifeste recalculé par l'attaquant : la signature ne suit pas.
        let forge: &[u8] = b"MZ_EXE_MALVEILLANT";
        assert!(refus(&manifeste(forge), forge, Some(&sig)).contains("signature invalide"));
        // Exécutable forgé de même taille, manifeste d'origine : refusé à l'extraction.
        let meme_taille: &[u8] = b"MZ_DUMMY_EXE_CONTENX";
        let (dir, p) = ecrire_paquet(&m, meme_taille, Some(&sig), false);
        let e = extraire_verifie(&p, &cles_test()).unwrap_err().to_string();
        assert!(e.contains("SHA-256"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sans_cle_configuree_tout_est_refuse() {
        let m = manifeste(EXE);
        let sig = sig_json(&m, EXE.len() as u64, &cle_test(), "test-a");
        let (dir, p) = ecrire_paquet(&m, EXE, Some(&sig), false);
        assert!(inspect_avec_cles(&p, &[]).unwrap_err().to_string().contains("clé inconnue"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Vecteur partagé avec `scripts/signer_maj.py --auto-test`.
    #[test]
    fn vecteur_partage_avec_le_script() {
        let m = manifeste(EXE);
        let attendu = format!("gws-update-v1\u{0}99.0.0\u{0}0.1.0\u{0}{:x}\u{0}20", Sha256::digest(EXE));
        assert_eq!(message_signe(&m, 20), attendu.into_bytes());
        let sig = BASE64_STANDARD.encode(cle_test().sign(&message_signe(&m, 20)).to_bytes());
        assert_eq!(sig, VECTEUR_SIGNATURE);
    }
    /// Signature attendue du vecteur ci-dessus (graine [7; 32]) ; le script la recalcule.
    const VECTEUR_SIGNATURE: &str = "0OWx/Ea+cow8N/NCNm/X6anKNBtR3k02cSkxBdW6qTB1Io9IWRN+QfziF+K2+/izkLtTYZcXZ0B3h2HomuFiCg==";

    /// Avant chaque publication : `cargo test --lib -- --ignored cles_de_production`.
    #[test]
    #[ignore = "échoue tant que la clé de production n'est pas renseignée"]
    fn cles_de_production_renseignees() {
        assert!(CLES_MAJ.len() >= 2, "renseigner la clé active et la clé de secours dans CLES_MAJ");
        for (id, cle) in CLES_MAJ {
            assert!(VerifyingKey::from_bytes(cle).is_ok(), "{id}");
            assert!(*cle != cle_test().verifying_key().to_bytes(), "{id} : clé de test");
            assert!(*cle != autre_cle().verifying_key().to_bytes(), "{id} : clé de test");
        }
    }

    #[test]
    fn rejet_d_un_paquet_avec_mauvais_type() {
        let dir = std::env::temp_dir().join(format!("gws_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pkg_path = dir.join("test_invalide.gws");

        let manifest = serde_json::json!({
            "type": "gws-project",
            "version": "1.0.0",
            "checksum_sha256": "1234"
        });

        {
            let file = File::create(&pkg_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            zip.start_file(MANIFEST_NAME, options).unwrap();
            zip.write_all(manifest.to_string().as_bytes()).unwrap();
            zip.finish().unwrap();
        }

        let err = inspect(&pkg_path).expect_err("Doit être rejeté");
        assert!(err.to_string().contains("Type de paquet inconnu"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

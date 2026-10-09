//! Avertissement de mise à jour.
//!
//! ## Ce que fait ce module
//!
//! Au démarrage (et sur demande, depuis les Paramètres), il demande au serveur
//! `gws-share` s'il existe une version plus récente que celle qui tourne. S'il y
//! en a une, l'UI affiche un bandeau : numéro, notes de version, et un bouton qui
//! ouvre la page de téléchargement **dans le navigateur du système**.
//!
//! L'utilisateur garde la main de bout en bout, sur trois réglages
//! (`app_settings`) :
//!
//! | Clé | Valeurs | Défaut |
//! |---|---|---|
//! | `update_check_mode` | `notify` (vérifier et prévenir) · `off` (ne jamais vérifier) | `notify` |
//! | `update_skipped_version` | version à ne plus signaler (« Ignorer cette version ») | — |
//! | `update_last_check_at` | instant de la dernière vérification (epoch s UTC) | — |
//! | `update_channel` | `stable` · `beta` (versions de test, choix explicite) | `stable` |
//!
//! ## Canal bêta
//!
//! Le canal `stable` interroge exactement la même adresse qu'avant l'ajout des
//! canaux. Le canal `beta` y ajoute `?channel=beta` : un serveur qui ne connaît
//! pas encore ce paramètre l'ignore et répond comme pour le canal stable, au
//! lieu d'une erreur. Les numéros suivent SemVer : `0.4.1-beta.1` est plus
//! récent que `0.4.0`, plus ancien que `0.4.1`.
//!
//! En mode `off`, **aucun octet ne part** au démarrage : c'est le sens d'un
//! interrupteur « désactivé ». Le bouton « Vérifier maintenant » reste possible,
//! puisque c'est une demande explicite.
//!
//! ## Pourquoi pas encore l'installation automatique
//!
//! Le greffon `tauri-plugin-updater` sait télécharger et installer tout seul,
//! mais seulement une archive **signée** avec une clé que l'application connaît
//! d'avance. Cette clé (`cargo tauri signer generate`) doit être créée et gardée
//! par le propriétaire du projet, avec un mot de passe — pas par un script. Tant
//! qu'elle n'existe pas, installer sans vérifier de signature reviendrait à
//! exécuter ce que le serveur voudra bien servir. Ce module s'arrête donc à
//! l'avertissement ; la procédure pour passer à l'étape suivante est dans
//! `server/README.md`, § « Publier une mise à jour ».
//!
//! Les paquets `.gws`, eux, sont signés (Ed25519) et vérifiés avant toute
//! installation : voir `package.rs` et `docs/signature-maj.md`. Le champ
//! `signature` de la réponse n'est pas interprété ici.
//!
//! ## Ce qui n'est pas cru sur parole
//!
//! * **La version annoncée** est comparée ici, numériquement, à celle du binaire :
//!   un serveur qui annoncerait une version plus ancienne n'est pas suivi.
//! * **Le lien de téléchargement** doit commencer par [`DOWNLOAD_PREFIX`] : un
//!   manifeste compromis ne peut pas envoyer l'utilisateur ailleurs.
//! * **Les notes** sont du texte, affiché comme du texte.

use std::io::Read;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::error::{AppError, AppResult};

pub const API_BASE: &str = "https://gastinel.me/gws/api/v1";
/// Seul préfixe accepté pour un lien de téléchargement.
pub const DOWNLOAD_PREFIX: &str = "https://gastinel.me/gws/downloads/";
/// Page de téléchargement ouverte par le bouton « Télécharger » : elle lance
/// d'elle-même le paquet `.gws` de la version passée en paramètre. Seule autre
/// adresse que [`open_download`] accepte, et seulement suivie d'une version valide.
pub const DOWNLOAD_PAGE_PREFIX: &str = "https://gastinel.me/gws/download/?maj=";

pub const SETTING_MODE: &str = "update_check_mode";
pub const SETTING_SKIPPED: &str = "update_skipped_version";
pub const SETTING_LAST_CHECK: &str = "update_last_check_at";
pub const SETTING_CHANNEL: &str = "update_channel";

/// Version du binaire en cours d'exécution — celle de `Cargo.toml`, qui doit
/// rester identique à celle de `tauri.conf.json`.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    /// Vérifier au démarrage et prévenir.
    Notify,
    /// Ne jamais vérifier seul.
    Off,
}

impl UpdateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateMode::Notify => "notify",
            UpdateMode::Off => "off",
        }
    }

    /// Toute valeur inconnue retombe sur le défaut, plutôt que de désactiver
    /// silencieusement les avertissements.
    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "off" => UpdateMode::Off,
            _ => UpdateMode::Notify,
        }
    }
}

/// Canal de mise à jour. `Stable` par défaut : le bêta est un choix explicite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    Stable,
    Beta,
}

impl UpdateChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateChannel::Stable => "stable",
            UpdateChannel::Beta => "beta",
        }
    }

    /// Toute valeur inconnue retombe sur `Stable` : on n'entre jamais dans le
    /// canal bêta par accident.
    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "beta" => UpdateChannel::Beta,
            _ => UpdateChannel::Stable,
        }
    }
}

/// Réglages exposés à l'UI.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateSettings {
    pub mode: UpdateMode,
    pub channel: UpdateChannel,
    pub current_version: String,
    /// La version installée est elle-même une pré-version (bêta).
    pub current_is_prerelease: bool,
    pub skipped_version: Option<String>,
    pub last_check_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub fn settings(db: &Db) -> AppResult<UpdateSettings> {
    Ok(UpdateSettings {
        mode: db.get_setting(SETTING_MODE)?.as_deref().map(UpdateMode::parse).unwrap_or(UpdateMode::Notify),
        channel: db.get_setting(SETTING_CHANNEL)?.as_deref().map(UpdateChannel::parse).unwrap_or(UpdateChannel::Stable),
        current_version: CURRENT_VERSION.to_string(),
        current_is_prerelease: parse_version(CURRENT_VERSION).is_some_and(|v| v.is_prerelease()),
        skipped_version: db.get_setting(SETTING_SKIPPED)?.filter(|v| !v.is_empty()),
        last_check_at: db
            .get_setting(SETTING_LAST_CHECK)?
            .and_then(|v| v.parse::<i64>().ok())
            .and_then(|s| chrono::DateTime::from_timestamp(s, 0)),
    })
}

pub fn set_mode(db: &Db, mode: UpdateMode) -> AppResult<UpdateSettings> {
    db.set_setting(SETTING_MODE, mode.as_str())?;
    settings(db)
}

pub fn set_channel(db: &Db, channel: UpdateChannel) -> AppResult<UpdateSettings> {
    db.set_setting(SETTING_CHANNEL, channel.as_str())?;
    settings(db)
}

/// « Ignorer cette version » : elle ne sera plus signalée au démarrage. Une
/// version encore plus récente, elle, le sera.
pub fn skip_version(db: &Db, version: &str) -> AppResult<UpdateSettings> {
    if parse_version(version).is_none() {
        return Err(AppError::Other(format!("Version invalide : {version}")));
    }
    db.set_setting(SETTING_SKIPPED, version)?;
    settings(db)
}

/// Numéro de version SemVer : `X.Y.Z` suivi d'une éventuelle pré-version
/// (`-beta.1`). Les métadonnées de build (`+…`) sont ignorées, comme le veut SemVer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Identifiants de pré-version ; vide pour une version stable.
    pub pre: Vec<String>,
}

impl Version {
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    /// Ordre SemVer 2.0 (§ 11) : `1.0.0-alpha < 1.0.0-alpha.1 < 1.0.0-beta <
    /// 1.0.0-beta.2 < 1.0.0-beta.11 < 1.0.0`.
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        let base = (self.major, self.minor, self.patch).cmp(&(o.major, o.minor, o.patch));
        if base != Equal {
            return base;
        }
        match (self.pre.is_empty(), o.pre.is_empty()) {
            (true, true) => return Equal,
            (true, false) => return Greater,
            (false, true) => return Less,
            _ => {}
        }
        for (a, b) in self.pre.iter().zip(&o.pre) {
            let ord = match (a.parse::<u64>(), b.parse::<u64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                (Ok(_), Err(_)) => Less,
                (Err(_), Ok(_)) => Greater,
                _ => a.cmp(b),
            };
            if ord != Equal {
                return ord;
            }
        }
        self.pre.len().cmp(&o.pre.len())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// `"0.10.2"`, `"0.4.1-beta.1"`. Comparaison numérique : `0.10.0 > 0.2.0`.
pub fn parse_version(v: &str) -> Option<Version> {
    let v = v.trim();
    let v = v.split_once('+').map_or(v, |(a, _)| a);
    let (noyau, pre) = match v.split_once('-') {
        Some((n, p)) => (n, Some(p)),
        None => (v, None),
    };
    let num = |s: Option<&str>| -> Option<u64> {
        let s = s?;
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let mut it = noyau.split('.');
    let (major, minor, patch) = (num(it.next())?, num(it.next())?, num(it.next())?);
    if it.next().is_some() {
        return None;
    }
    let pre = match pre {
        None => Vec::new(),
        Some(p) => {
            let ids: Vec<String> = p.split('.').map(str::to_string).collect();
            let valide = |i: &String| !i.is_empty() && i.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
            if !ids.iter().all(valide) {
                return None;
            }
            ids
        }
    };
    Some(Version { major, minor, patch, pre })
}

/// Une version disponible, telle que l'UI l'affiche.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: String,
    pub url: String,
    #[serde(default)]
    pub pub_date: Option<String>,
    /// Version bêta : l'UI le dit en toutes lettres.
    #[serde(default)]
    pub prerelease: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateCheck {
    pub current_version: String,
    /// `None` : rien de plus récent (ou vérification désactivée).
    pub available: Option<UpdateInfo>,
    /// La version disponible est celle que l'utilisateur a choisi d'ignorer.
    pub skipped: bool,
    /// `false` quand le mode `off` a empêché toute requête.
    pub checked: bool,
}

/// Réponse du serveur au format « serveur dynamique » de Tauri.
#[derive(Debug, Deserialize)]
struct ServerAnswer {
    version: String,
    #[serde(default)]
    notes: String,
    url: String,
    #[serde(default)]
    pub_date: Option<String>,
}

/// Plateforme au sens de Tauri : `windows`, `linux`, `darwin`, `android`.
fn target() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

/// Interprète la réponse du serveur. Séparé du réseau pour être testé.
pub fn interpret(status: u16, body: &[u8], current: &str) -> Result<Option<UpdateInfo>, String> {
    match status {
        204 => return Ok(None),
        200 => {}
        other => return Err(format!("statut HTTP {other}")),
    }
    let a: ServerAnswer = serde_json::from_slice(body).map_err(|e| format!("réponse illisible ({e})"))?;
    let (Some(nouvelle), Some(actuelle)) = (parse_version(&a.version), parse_version(current)) else {
        return Err(format!("numéro de version invalide : {}", a.version));
    };
    if nouvelle <= actuelle {
        return Ok(None);
    }
    if !a.url.starts_with(DOWNLOAD_PREFIX) || a.url.len() > 300 || a.url.contains("..") {
        return Err("lien de téléchargement hors du domaine attendu, ignoré".into());
    }
    Ok(Some(UpdateInfo {
        prerelease: nouvelle.is_prerelease(),
        version: a.version,
        notes: a.notes.chars().take(4000).collect(),
        url: a.url,
        pub_date: a.pub_date,
    }))
}

/// Adresse interrogée. Canal stable : identique à celle d'avant les canaux.
pub fn update_url(channel: UpdateChannel, os: &str, arch: &str, current: &str) -> String {
    let base = format!("{API_BASE}/update/{os}/{arch}/{current}");
    match channel {
        UpdateChannel::Stable => base,
        UpdateChannel::Beta => format!("{base}?channel=beta"),
    }
}

/// Interroge le serveur. Bloquant : à appeler depuis un `spawn_blocking`.
pub fn fetch_available(channel: UpdateChannel) -> Result<Option<UpdateInfo>, String> {
    let client = crate::http_client::client_builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .build()
        .map_err(|e| e.to_string())?;
    let url = update_url(channel, target(), std::env::consts::ARCH, CURRENT_VERSION);
    let resp = client.get(url).send().map_err(|e| {
        if e.is_timeout() { "délai dépassé".to_string() } else { e.to_string() }
    })?;
    let status = resp.status().as_u16();
    let mut body = Vec::new();
    resp.take(64 * 1024).read_to_end(&mut body).map_err(|e| e.to_string())?;
    interpret(status, &body, CURRENT_VERSION)
}

/// Vérification complète. `manual` : demandée par l'utilisateur, faite même en
/// mode `off`. Le réseau est confié à `fetch` pour que l'appelant le lance hors
/// du thread principal.
pub fn check(db: &Db, manual: bool, fetch: impl FnOnce() -> Result<Option<UpdateInfo>, String>) -> AppResult<UpdateCheck> {
    let reglages = settings(db)?;
    if reglages.mode == UpdateMode::Off && !manual {
        return Ok(UpdateCheck { current_version: CURRENT_VERSION.into(), available: None, skipped: false, checked: false });
    }
    let available = fetch().map_err(|e| AppError::Other(format!("Vérification des mises à jour impossible : {e}")))?;
    db.set_setting(SETTING_LAST_CHECK, &chrono::Utc::now().timestamp().to_string())?;
    let skipped = matches!((&available, &reglages.skipped_version), (Some(a), Some(s)) if &a.version == s);
    Ok(UpdateCheck { current_version: CURRENT_VERSION.into(), available, skipped, checked: true })
}

/// Adresse de la page de téléchargement pour `version`, ou `None` si la version
/// n'est pas un numéro SemVer strict (chiffres, lettres, `.` et `-` seulement).
pub fn download_page_url(version: &str) -> Option<String> {
    let sure = !version.is_empty()
        && version.len() <= 40
        && version.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    (sure && parse_version(version).is_some()).then(|| format!("{DOWNLOAD_PAGE_PREFIX}{version}"))
}

/// Liste fermée des adresses que l'app ouvre : un fichier sous
/// [`DOWNLOAD_PREFIX`] (liens des manifestes, comportement des 0.4.x), ou la
/// page de téléchargement suivie d'une version valide, et rien d'autre.
pub fn download_url_allowed(url: &str) -> bool {
    if url.len() > 300 || url.contains("..") {
        return false;
    }
    if url.starts_with(DOWNLOAD_PREFIX) {
        return true;
    }
    match url.strip_prefix(DOWNLOAD_PAGE_PREFIX) {
        Some(v) => download_page_url(v).as_deref() == Some(url),
        None => false,
    }
}

/// Ouvre la page de téléchargement dans le navigateur du système, après avoir
/// revérifié le préfixe : la commande est joignable depuis le JS.
pub fn open_download(url: &str) -> AppResult<()> {
    if !download_url_allowed(url) {
        return Err(AppError::Other(format!("Adresse de téléchargement non autorisée : {url}")));
    }
    tauri_plugin_opener::open_url(url, None::<&str>)
        .map_err(|e| AppError::Other(format!("Le navigateur n'a pas pu être ouvert : {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &[u8] = br#"{"version":"0.3.0","notes":"Partage de points","url":"https://gastinel.me/gws/downloads/gws_0.3.0_x64-setup.exe","signature":"SIG","pub_date":"2026-09-20T08:00:00Z"}"#;

    #[test]
    fn versions_comparees_numeriquement() {
        assert!(parse_version("0.10.0") > parse_version("0.2.0"));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("1.x.3"), None);
    }

    #[test]
    fn la_version_du_binaire_est_bien_formee() {
        assert!(parse_version(CURRENT_VERSION).is_some());
    }

    #[test]
    fn rien_de_neuf() {
        assert_eq!(interpret(204, b"", "0.2.0").unwrap(), None);
        assert_eq!(interpret(200, OK, "0.3.0").unwrap(), None, "même version");
        assert_eq!(interpret(200, OK, "0.10.0").unwrap(), None, "le serveur annonce plus ancien : ignoré");
    }

    #[test]
    fn nouvelle_version_signalee() {
        let info = interpret(200, OK, "0.2.0").unwrap().unwrap();
        assert_eq!(info.version, "0.3.0");
        assert!(info.url.starts_with(DOWNLOAD_PREFIX));
    }

    #[test]
    fn un_lien_hors_domaine_est_refuse() {
        for url in [
            "https://evil.example/gws.exe",
            "http://gastinel.me/gws/downloads/gws.exe",
            "https://gastinel.me/gws/downloads/../../x.exe",
            "https://gastinel.me.evil.example/gws/downloads/x.exe",
        ] {
            let body = format!(r#"{{"version":"9.0.0","notes":"","url":"{url}"}}"#);
            assert!(interpret(200, body.as_bytes(), "0.2.0").is_err(), "{url}");
        }
    }

    #[test]
    fn open_download_liste_fermee() {
        for ok in [
            "https://gastinel.me/gws/downloads/update-0.4.6.gws",
            "https://gastinel.me/gws/downloads/Gastinel_Weather_Studio_0.4.6_x64-setup.exe",
            "https://gastinel.me/gws/download/?maj=0.4.7",
            "https://gastinel.me/gws/download/?maj=0.10.0",
            "https://gastinel.me/gws/download/?maj=0.4.1-beta.1",
        ] {
            assert!(download_url_allowed(ok), "{ok}");
        }
        for ko in [
            "https://gastinel.me/gws/download/",
            "https://gastinel.me/gws/download/?maj=",
            "https://gastinel.me/gws/download/?maj=0.4",
            "https://gastinel.me/gws/download/?maj=0.4.4.1",
            "https://gastinel.me/gws/download/?maj=0.4.7&x=1",
            "https://gastinel.me/gws/download/?maj=0.4.7#a",
            "https://gastinel.me/gws/download/?maj=0.4.7+build",
            "https://gastinel.me/gws/download/?maj=0.4.7%3Cscript%3E",
            "https://gastinel.me/gws/download/?maj=%200.4.7",
            "https://gastinel.me/gws/download/?maj=0.4.7-<b>",
            "https://gastinel.me/gws/download/?maj=../../x",
            "https://gastinel.me/gws/download/../stack/",
            "https://gastinel.me/gws/downloads/../../x.exe",
            "https://gastinel.me/gws/stack/",
            "http://gastinel.me/gws/download/?maj=0.4.7",
            "https://gastinel.me.evil.example/gws/download/?maj=0.4.7",
            "https://evil.example/?u=https://gastinel.me/gws/download/?maj=0.4.7",
        ] {
            assert!(!download_url_allowed(ko), "{ko}");
        }
        assert_eq!(download_page_url("0.4.7").as_deref(), Some("https://gastinel.me/gws/download/?maj=0.4.7"));
        assert_eq!(download_page_url(" 0.4.7"), None);
        assert_eq!(download_page_url("0.4.7+b"), None);
    }

    #[test]
    fn mode_off_aucune_requete_au_demarrage() {
        let db = Db::open_in_memory().unwrap();
        set_mode(&db, UpdateMode::Off).unwrap();
        let r = check(&db, false, || panic!("aucune requête ne doit partir en mode off")).unwrap();
        assert!(!r.checked && r.available.is_none());
        // Une demande explicite, elle, passe.
        let r = check(&db, true, || Ok(None)).unwrap();
        assert!(r.checked);
        assert!(settings(&db).unwrap().last_check_at.is_some());
    }

    #[test]
    fn version_ignoree() {
        let db = Db::open_in_memory().unwrap();
        skip_version(&db, "0.3.0").unwrap();
        let info = interpret(200, OK, "0.2.0").unwrap();
        let r = check(&db, false, || Ok(info.clone())).unwrap();
        assert!(r.skipped);
        assert!(skip_version(&db, "pas une version").is_err());
    }

    #[test]
    fn canal_stable_par_defaut_et_url_inchangee() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(settings(&db).unwrap().channel, UpdateChannel::Stable);
        // Exactement l'adresse d'avant l'ajout des canaux.
        assert_eq!(
            update_url(UpdateChannel::Stable, "windows", "x86_64", "0.4.0"),
            "https://gastinel.me/gws/api/v1/update/windows/x86_64/0.4.0"
        );
        assert_eq!(UpdateChannel::parse("n'importe quoi"), UpdateChannel::Stable);
    }

    #[test]
    fn bascule_beta_puis_retour_stable() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(set_channel(&db, UpdateChannel::Beta).unwrap().channel, UpdateChannel::Beta);
        assert_eq!(
            update_url(UpdateChannel::Beta, "windows", "x86_64", "0.4.1-beta.1"),
            "https://gastinel.me/gws/api/v1/update/windows/x86_64/0.4.1-beta.1?channel=beta"
        );
        assert_eq!(set_channel(&db, UpdateChannel::Stable).unwrap().channel, UpdateChannel::Stable);
    }

    #[test]
    fn ordre_semver_avec_beta() {
        let v = |s| parse_version(s).unwrap();
        assert!(v("0.4.1-beta") > v("0.4.0"));
        assert!(v("0.4.1-beta.1") > v("0.4.0"));
        assert!(v("0.4.1-beta.1") < v("0.4.1"));
        assert!(v("0.4.1-beta.1") < v("0.5.0"));
        assert!(v("0.4.1-beta.2") < v("0.4.1-beta.11"));
        assert!(v("0.4.1-alpha") < v("0.4.1-beta"));
        assert!(v("0.4.1-beta") < v("0.4.1-beta.1"));
        assert_eq!(v("0.4.1+build.7"), v("0.4.1"));
        assert!(v("0.4.1-beta.1").is_prerelease() && !v("0.4.0").is_prerelease());
        for faux in ["0.4.1-", "0.4.1-beta..1", "0.4.1-bêta", "1a.2.3", "0.4"] {
            assert!(parse_version(faux).is_none(), "{faux}");
        }
    }

    #[test]
    fn beta_proposee_et_marquee() {
        let body = br#"{"version":"0.4.1-beta.1","notes":"Canal beta","url":"https://gastinel.me/gws/downloads/x.exe"}"#;
        let info = interpret(200, body, "0.4.0").unwrap().unwrap();
        assert!(info.prerelease);
        // Un bêta-testeur en 0.4.1-beta.1 se voit proposer la 0.4.1 stable…
        let stable = br#"{"version":"0.4.1","notes":"","url":"https://gastinel.me/gws/downloads/y.exe"}"#;
        let info = interpret(200, stable, "0.4.1-beta.1").unwrap().unwrap();
        assert!(!info.prerelease);
        // …mais pas une 0.3.0 plus ancienne.
        assert_eq!(interpret(200, OK, "0.4.1-beta.1").unwrap(), None);
    }

    #[test]
    #[ignore = "dépend du réseau (serveur gastinel.me)"]
    fn appel_reel_deux_canaux() {
        // Stable : la 0.4.0 est publiée, une 0.3.0 doit se la voir proposer.
        let s = interpret_reel(UpdateChannel::Stable, "0.3.0");
        assert!(s.is_some());
        // Bêta : nécessite le serveur à jour (voir docs/serveur-canal-beta.md).
        let b = interpret_reel(UpdateChannel::Beta, "0.4.0");
        eprintln!("canal bêta pour 0.4.0 : {b:?}");
    }

    fn interpret_reel(channel: UpdateChannel, current: &str) -> Option<UpdateInfo> {
        let url = update_url(channel, "windows", "x86_64", current);
        let resp = crate::http_client::client_builder().build().unwrap().get(url).send().unwrap();
        let status = resp.status().as_u16();
        let body = resp.bytes().unwrap();
        interpret(status, &body, current).unwrap()
    }

    #[test]
    fn mode_inconnu_retombe_sur_notify() {
        assert_eq!(UpdateMode::parse("n'importe quoi"), UpdateMode::Notify);
        assert_eq!(UpdateMode::parse("off"), UpdateMode::Off);
    }
}

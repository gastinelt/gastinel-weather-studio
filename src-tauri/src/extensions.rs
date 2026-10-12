//! Extensions natives : découverte, chargement dynamique, appels, sauvegarde.
//!
//! # Risques acceptés par l'utilisateur (décision du 25/09/2026)
//! * **Aucune isolation mémoire** : une extension est une `.dll` chargée dans
//!   le processus. Un bug peut corrompre la mémoire ou fermer l'application.
//! * **Accès SQLite direct** : l'extension ouvre elle-même le fichier de base.
//!   Avec `db_write`, rien ne contrôle ce qu'elle écrit.
//!
//! Seul garde-fou : avant de charger une extension qui déclare `db_write`,
//! l'app fait une **copie complète de la base** (`VACUUM INTO`) dans
//! `sauvegardes-extensions/`, et n'en garde que [`BACKUPS_KEPT`]. Cette copie
//! protège contre une perte de données, pas contre un plantage pendant que
//! l'extension tourne.
//!
//! # Disposition sur disque
//! ```text
//! <dossier>/extensions/<id>/manifest.json
//!                          /<library>          (.dll)
//!                          /ui/<ui_entry>      (panneau affiché dans l'app)
//! ```
//! Deux dossiers sont explorés : à côté de l'exécutable, puis dans les données
//! de l'app (`%APPDATA%\fr.gastinel.weatherstudio\extensions`, inscriptible
//! sans droits administrateur). Le second l'emporte en cas de doublon d'id.
//!
//! # Démarrage
//! La découverte ne lit que les manifestes. Les `.dll` des extensions activées
//! sont chargées **dans un fil séparé** : une sauvegarde de plusieurs Go ou une
//! extension lente ne retardent pas l'ouverture de l'app, et un échec de
//! chargement (fichier corrompu, symbole absent, version d'API différente) ne
//! fait que marquer l'extension « en erreur ».

use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gws_extension_api as api;
use serde::{Deserialize, Serialize};

pub const BACKUPS_KEPT: usize = 3;
const DOSSIER_EXTENSIONS: &str = "extensions";
const DOSSIER_SAUVEGARDES: &str = "sauvegardes-extensions";
const FICHIER_ACTIVATION: &str = "extensions.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub api_version: u32,
    /// Nom du fichier de bibliothèque, relatif au dossier de l'extension.
    pub library: String,
    /// Page HTML du panneau, relative à `ui/`.
    #[serde(default)]
    pub ui_entry: Option<String>,
    /// Point d'accroche de l'interface : `map_view` (bouton sur la carte du
    /// projet) ou `settings` (bouton en bas des Paramètres, depuis la 0.6.0).
    #[serde(default)]
    pub hook: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", content = "detail")]
pub enum Etat {
    Desactivee,
    Chargement,
    Active,
    Incompatible(String),
    Erreur(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionInfo {
    #[serde(flatten)]
    pub manifest: Manifest,
    pub dir: String,
    pub enabled: bool,
    pub state: Etat,
    pub last_backup: Option<String>,
}

type FnVersion = unsafe extern "C" fn() -> u32;
type FnInit = unsafe extern "C" fn(*const c_char) -> *mut c_char;
type FnCall = unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_char;
type FnFree = unsafe extern "C" fn(*mut c_char);
type FnShutdown = unsafe extern "C" fn();

/// Une extension chargée. Les pointeurs de fonction ne sont valides que tant
/// que `_lib` vit ; on ne les copie hors de la structure que sous le verrou
/// d'appel, que le déchargement prend aussi.
struct Chargee {
    call: FnCall,
    free: FnFree,
    shutdown: FnShutdown,
    _lib: libloading::Library,
}

struct Entree {
    info: ExtensionInfo,
    chargee: Option<Chargee>,
}

pub struct ExtensionHost {
    data_dir: PathBuf,
    db_path: PathBuf,
    racines: Vec<PathBuf>,
    host_version: String,
    entrees: Mutex<BTreeMap<String, Entree>>,
    /// Un verrou par extension pour les appels : une extension lente n'en
    /// bloque pas une autre, et la liste reste lisible pendant un appel.
    appels: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl ExtensionHost {
    pub fn new(data_dir: &Path, db_path: &Path, host_version: &str) -> Arc<Self> {
        let mut racines = Vec::new();
        if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
            racines.push(exe_dir.join(DOSSIER_EXTENSIONS));
        }
        racines.push(data_dir.join(DOSSIER_EXTENSIONS));
        Self::with_roots(data_dir, db_path, host_version, racines)
    }

    pub fn with_roots(data_dir: &Path, db_path: &Path, host_version: &str, racines: Vec<PathBuf>) -> Arc<Self> {
        let host = Arc::new(ExtensionHost {
            data_dir: data_dir.to_path_buf(),
            db_path: db_path.to_path_buf(),
            racines,
            host_version: host_version.to_string(),
            entrees: Mutex::new(BTreeMap::new()),
            appels: Mutex::new(HashMap::new()),
        });
        host.decouvrir();
        host
    }

    fn entrees(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Entree>> {
        self.entrees.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Lit les manifestes. Un manifeste illisible donne une entrée en erreur
    /// (nommée d'après son dossier) plutôt que de disparaître sans explication.
    pub fn decouvrir(&self) {
        let actives = self.lire_activation();
        let mut trouvees = BTreeMap::new();
        for racine in &self.racines {
            let Ok(lecture) = std::fs::read_dir(racine) else { continue };
            for d in lecture.flatten() {
                let dir = d.path();
                if !dir.is_dir() {
                    continue;
                }
                let nom_dossier = d.file_name().to_string_lossy().to_string();
                let (manifest, etat) = match std::fs::read_to_string(dir.join("manifest.json"))
                    .map_err(|e| e.to_string())
                    .and_then(|s| serde_json::from_str::<Manifest>(&s).map_err(|e| e.to_string()))
                {
                    Ok(m) => (m, Etat::Desactivee),
                    Err(e) => (
                        Manifest {
                            id: nom_dossier.clone(),
                            name: nom_dossier.clone(),
                            version: "?".into(),
                            description: String::new(),
                            api_version: 0,
                            library: String::new(),
                            ui_entry: None,
                            hook: None,
                            permissions: vec![],
                        },
                        Etat::Erreur(format!("manifest.json illisible : {e}")),
                    ),
                };
                let etat = if etat == Etat::Desactivee && manifest.api_version != api::API_VERSION {
                    Etat::Incompatible(format!(
                        "l'extension cible l'API v{}, l'application fournit la v{}",
                        manifest.api_version,
                        api::API_VERSION
                    ))
                } else {
                    etat
                };
                let enabled = actives.contains(&manifest.id);
                trouvees.insert(
                    manifest.id.clone(),
                    ExtensionInfo { manifest, dir: dir.to_string_lossy().to_string(), enabled, state: etat, last_backup: None },
                );
            }
        }
        let mut entrees = self.entrees();
        for (id, info) in trouvees {
            // Une extension déjà chargée garde son état et sa bibliothèque.
            if entrees.get(&id).is_some_and(|e| e.chargee.is_some()) {
                continue;
            }
            entrees.insert(id, Entree { info, chargee: None });
        }
    }

    pub fn list(&self) -> Vec<ExtensionInfo> {
        self.entrees().values().map(|e| e.info.clone()).collect()
    }

    /// Charge toutes les extensions activées (appelé dans un fil séparé).
    pub fn charger_actives(&self) {
        let ids: Vec<String> = self.entrees().values().filter(|e| e.info.enabled).map(|e| e.info.manifest.id.clone()).collect();
        for id in ids {
            self.charger(&id);
        }
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<ExtensionInfo, String> {
        {
            let mut entrees = self.entrees();
            let e = entrees.get_mut(id).ok_or_else(|| format!("extension inconnue : {id}"))?;
            e.info.enabled = enabled;
        }
        self.ecrire_activation();
        if enabled {
            self.charger(id);
        } else {
            self.decharger(id);
        }
        self.entrees().get(id).map(|e| e.info.clone()).ok_or_else(|| "extension disparue".into())
    }

    fn set_etat(&self, id: &str, etat: Etat) {
        if let Some(e) = self.entrees().get_mut(id) {
            e.info.state = etat;
        }
    }

    /// Charge une extension. Ne panique jamais : tout échec devient un état.
    pub fn charger(&self, id: &str) {
        let info = {
            let entrees = self.entrees();
            let Some(e) = entrees.get(id) else { return };
            if e.chargee.is_some() || matches!(e.info.state, Etat::Incompatible(_)) {
                return;
            }
            if e.info.manifest.library.is_empty() {
                return; // manifeste illisible : l'état porte déjà l'erreur
            }
            e.info.clone()
        };
        self.set_etat(id, Etat::Chargement);

        let ecriture = info.manifest.permissions.iter().any(|p| p == api::PERM_DB_WRITE);
        if ecriture {
            match self.sauvegarder(id) {
                Ok(p) => {
                    if let Some(e) = self.entrees().get_mut(id) {
                        e.info.last_backup = Some(p.to_string_lossy().to_string());
                    }
                }
                Err(err) => {
                    // Pas de sauvegarde, pas d'accès en écriture : non négociable.
                    self.set_etat(id, Etat::Erreur(format!("sauvegarde de la base impossible, extension non chargée : {err}")));
                    return;
                }
            }
        }

        match self.ouvrir(&info, ecriture) {
            Ok(c) => {
                if let Some(e) = self.entrees().get_mut(id) {
                    e.chargee = Some(c);
                    e.info.state = Etat::Active;
                }
            }
            Err(Etat::Incompatible(m)) => self.set_etat(id, Etat::Incompatible(m)),
            Err(Etat::Erreur(m)) => self.set_etat(id, Etat::Erreur(m)),
            Err(_) => self.set_etat(id, Etat::Erreur("échec du chargement".into())),
        }
    }

    fn ouvrir(&self, info: &ExtensionInfo, ecriture: bool) -> Result<Chargee, Etat> {
        let chemin = Path::new(&info.dir).join(&info.manifest.library);
        if !chemin.is_file() {
            return Err(Etat::Erreur(format!("bibliothèque absente : {}", chemin.display())));
        }
        // SAFETY : charger une bibliothèque exécute son code d'initialisation.
        // C'est précisément le risque accepté (aucune isolation) ; un fichier
        // corrompu ou qui n'est pas une DLL est refusé par le système et
        // revient ici en erreur.
        let lib = unsafe { libloading::Library::new(&chemin) }
            .map_err(|e| Etat::Erreur(format!("chargement refusé par le système : {e}")))?;
        unsafe {
            let sym = |nom: &[u8]| String::from_utf8_lossy(&nom[..nom.len() - 1]).to_string();
            let version: FnVersion = *lib
                .get::<FnVersion>(api::SYM_API_VERSION)
                .map_err(|_| Etat::Erreur(format!("symbole {} absent : ce n'est pas une extension GWS", sym(api::SYM_API_VERSION))))?;
            let v = version();
            if v != api::API_VERSION {
                return Err(Etat::Incompatible(format!(
                    "la bibliothèque est compilée pour l'API v{v}, l'application fournit la v{}",
                    api::API_VERSION
                )));
            }
            let get = |nom: &[u8]| -> Result<*const (), Etat> {
                lib.get::<*const ()>(nom)
                    .map(|s| *s)
                    .map_err(|_| Etat::Erreur(format!("symbole {} absent", sym(nom))))
            };
            let init: FnInit = std::mem::transmute(get(api::SYM_INIT)?);
            let call: FnCall = std::mem::transmute(get(api::SYM_CALL)?);
            let free: FnFree = std::mem::transmute(get(api::SYM_FREE)?);
            let shutdown: FnShutdown = std::mem::transmute(get(api::SYM_SHUTDOWN)?);

            let dossier_donnees = self.data_dir.join("extensions-donnees").join(&info.manifest.id);
            let _ = std::fs::create_dir_all(&dossier_donnees);
            let db = self.db_path.to_string_lossy().replace('\\', "/");
            let ctx = api::HostContext {
                api_version: api::API_VERSION,
                host_version: self.host_version.clone(),
                db_path: self.db_path.to_string_lossy().to_string(),
                db_uri: format!("file:{db}?mode={}", if ecriture { "rw" } else { "ro" }),
                db_writable: ecriture,
                data_dir: dossier_donnees.to_string_lossy().to_string(),
            };
            let ctx = CString::new(serde_json::to_string(&ctx).unwrap()).unwrap();
            let err = init(ctx.as_ptr());
            if !err.is_null() {
                let m = CStr::from_ptr(err).to_string_lossy().to_string();
                free(err);
                return Err(Etat::Erreur(format!("initialisation refusée : {m}")));
            }
            Ok(Chargee { call, free, shutdown, _lib: lib })
        }
    }

    pub fn decharger(&self, id: &str) {
        let verrou = self.verrou_appel(id);
        let _g = verrou.lock().unwrap_or_else(|e| e.into_inner());
        let chargee = self.entrees().get_mut(id).and_then(|e| e.chargee.take());
        if let Some(c) = chargee {
            unsafe { (c.shutdown)() };
            drop(c);
        }
        if let Some(e) = self.entrees().get_mut(id) {
            if !matches!(e.info.state, Etat::Incompatible(_)) {
                e.info.state = Etat::Desactivee;
            }
        }
    }

    /// La bibliothèque de cette extension est-elle chargée dans le processus ?
    pub fn est_chargee(&self, id: &str) -> bool {
        self.entrees().get(id).is_some_and(|e| e.chargee.is_some())
    }

    /// Retire une extension de la liste (après désinstallation).
    pub fn oublier(&self, id: &str) {
        self.entrees().remove(id);
    }

    /// Dossier où s'installent les extensions téléchargées (inscriptible).
    pub fn dossier_installation(&self) -> PathBuf {
        self.data_dir.join(DOSSIER_EXTENSIONS)
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn decharger_tout(&self) {
        let ids: Vec<String> = self.entrees().keys().cloned().collect();
        for id in ids {
            self.decharger(&id);
        }
    }

    fn verrou_appel(&self, id: &str) -> Arc<Mutex<()>> {
        self.appels.lock().unwrap_or_else(|e| e.into_inner()).entry(id.to_string()).or_default().clone()
    }

    /// Appelle une méthode de l'extension. Bloquant : à lancer hors du fil UI.
    pub fn call(&self, id: &str, method: &str, payload: &serde_json::Value) -> Result<serde_json::Value, String> {
        let verrou = self.verrou_appel(id);
        let _g = verrou.lock().unwrap_or_else(|e| e.into_inner());
        let (call, free) = {
            let entrees = self.entrees();
            let e = entrees.get(id).ok_or_else(|| format!("extension inconnue : {id}"))?;
            let c = e.chargee.as_ref().ok_or_else(|| format!("l'extension {id} n'est pas active"))?;
            (c.call, c.free)
        };
        // La bibliothèque ne peut pas être déchargée pendant l'appel : le
        // déchargement prend le même verrou d'appel.
        let m = CString::new(method).map_err(|e| e.to_string())?;
        let p = CString::new(payload.to_string()).map_err(|e| e.to_string())?;
        let brut = unsafe {
            let r = call(m.as_ptr(), p.as_ptr());
            if r.is_null() {
                return Err("réponse vide de l'extension".into());
            }
            let s = CStr::from_ptr(r).to_string_lossy().to_string();
            free(r);
            s
        };
        let v: serde_json::Value = serde_json::from_str(&brut).map_err(|e| format!("réponse illisible : {e}"))?;
        if let Some(err) = v.get("err") {
            return Err(err.as_str().unwrap_or("erreur").to_string());
        }
        Ok(v.get("ok").cloned().unwrap_or(serde_json::Value::Null))
    }

    /// Chemin d'un fichier d'interface, ou `None` s'il sort du dossier `ui/`.
    pub fn ui_file(&self, id: &str, relatif: &str) -> Option<PathBuf> {
        let dir = self.entrees().get(id).map(|e| PathBuf::from(&e.info.dir).join("ui"))?;
        let racine = dir.canonicalize().ok()?;
        let cible = racine.join(relatif).canonicalize().ok()?;
        cible.starts_with(&racine).then_some(cible).filter(|p| p.is_file())
    }

    /// Image produite par l'extension dans son dossier de données
    /// (`extensions-donnees/<id>/`), servie par le protocole `gwsext` sous
    /// `<id>/_donnees/…` : une texture n'a plus à transiter en data URL base64
    /// dans la réponse JSON. Seules les images JPEG et PNG sont servies, et
    /// jamais hors de ce dossier.
    pub fn data_file(&self, id: &str, relatif: &str) -> Option<PathBuf> {
        if !self.entrees().contains_key(id) {
            return None;
        }
        let racine = self.data_dir.join("extensions-donnees").join(id).canonicalize().ok()?;
        let cible = racine.join(relatif).canonicalize().ok()?;
        let image = matches!(
            cible.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
            Some("jpg" | "jpeg" | "png")
        );
        (image && cible.starts_with(&racine) && cible.is_file()).then_some(cible)
    }

    // --- Sauvegarde ---------------------------------------------------------

    /// Copie cohérente de la base par `VACUUM INTO`, sur une connexion à part
    /// en lecture seule : l'app continue d'utiliser la sienne pendant la copie.
    pub fn sauvegarder(&self, id: &str) -> Result<PathBuf, String> {
        let dossier = self.data_dir.join(DOSSIER_SAUVEGARDES);
        std::fs::create_dir_all(&dossier).map_err(|e| e.to_string())?;
        let nom = format!("{}-avant-{}.sqlite3", chrono::Local::now().format("%Y%m%d-%H%M%S"), nettoyer(id));
        let cible = dossier.join(nom);
        if self.db_path.is_file() {
            let conn = rusqlite::Connection::open_with_flags(&self.db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| e.to_string())?;
            conn.execute("VACUUM INTO ?1", [cible.to_string_lossy().to_string()]).map_err(|e| e.to_string())?;
        } else {
            return Err(format!("base introuvable : {}", self.db_path.display()));
        }
        purger_sauvegardes(&dossier, BACKUPS_KEPT);
        Ok(cible)
    }

    // --- Activation persistée ----------------------------------------------

    fn lire_activation(&self) -> Vec<String> {
        std::fs::read_to_string(self.data_dir.join(FICHIER_ACTIVATION))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn ecrire_activation(&self) {
        let ids: Vec<String> = self.entrees().values().filter(|e| e.info.enabled).map(|e| e.info.manifest.id.clone()).collect();
        let _ = std::fs::write(self.data_dir.join(FICHIER_ACTIVATION), serde_json::to_string_pretty(&ids).unwrap());
    }
}

fn nettoyer(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Garde les `n` sauvegardes les plus récentes (le nom commence par la date).
fn purger_sauvegardes(dossier: &Path, n: usize) {
    let Ok(lecture) = std::fs::read_dir(dossier) else { return };
    let mut fichiers: Vec<PathBuf> =
        lecture.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "sqlite3")).collect();
    fichiers.sort();
    let trop = fichiers.len().saturating_sub(n);
    for f in fichiers.into_iter().take(trop) {
        let _ = std::fs::remove_file(f);
    }
}

pub fn mime(chemin: &Path) -> &'static str {
    match chemin.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(nom: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gws-ext-test-{nom}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn ecrire_ext(racine: &Path, id: &str, api_version: u32, lib: &[u8], perms: &[&str]) {
        let d = racine.join(id);
        std::fs::create_dir_all(d.join("ui")).unwrap();
        let m = serde_json::json!({
            "id": id, "name": id, "version": "0.1.0", "api_version": api_version,
            "library": format!("{id}.dll"), "ui_entry": "index.html", "hook": "map_view",
            "permissions": perms,
        });
        std::fs::write(d.join("manifest.json"), m.to_string()).unwrap();
        std::fs::write(d.join(format!("{id}.dll")), lib).unwrap();
        std::fs::write(d.join("ui/index.html"), "<p>ok</p>").unwrap();
    }

    #[test]
    fn dossier_absent_ne_casse_rien() {
        let d = tmp("absent");
        let h = ExtensionHost::with_roots(&d, &d.join("x.sqlite3"), "t", vec![d.join("nexistepas")]);
        assert!(h.list().is_empty());
        h.charger_actives();
    }

    #[test]
    fn dll_corrompue_donne_une_erreur_sans_planter() {
        let d = tmp("corrompue");
        let r = d.join("extensions");
        ecrire_ext(&r, "cassee", api::API_VERSION, b"ceci n'est pas une dll", &[]);
        let h = ExtensionHost::with_roots(&d, &d.join("x.sqlite3"), "t", vec![r]);
        let info = h.set_enabled("cassee", true).unwrap();
        assert!(matches!(info.state, Etat::Erreur(_)), "{:?}", info.state);
        assert!(h.call("cassee", "x", &serde_json::Value::Null).is_err());
        // L'activation est mémorisée : au prochain démarrage, même résultat, sans panique.
        let h2 = ExtensionHost::with_roots(&d, &d.join("x.sqlite3"), "t", vec![d.join("extensions")]);
        h2.charger_actives();
        assert!(matches!(h2.list()[0].state, Etat::Erreur(_)));
    }

    #[test]
    fn version_d_api_differente_non_chargee() {
        let d = tmp("version");
        let r = d.join("extensions");
        ecrire_ext(&r, "vieille", api::API_VERSION + 1, b"x", &[]);
        let h = ExtensionHost::with_roots(&d, &d.join("x.sqlite3"), "t", vec![r]);
        let info = h.set_enabled("vieille", true).unwrap();
        assert!(matches!(info.state, Etat::Incompatible(_)));
    }

    #[test]
    fn manifeste_illisible_signale() {
        let d = tmp("manifeste");
        let r = d.join("extensions");
        std::fs::create_dir_all(r.join("bidon")).unwrap();
        std::fs::write(r.join("bidon/manifest.json"), "{pas du json").unwrap();
        let h = ExtensionHost::with_roots(&d, &d.join("x.sqlite3"), "t", vec![r]);
        assert!(matches!(h.list()[0].state, Etat::Erreur(_)));
        h.set_enabled("bidon", true).unwrap();
    }

    #[test]
    fn ecriture_sans_base_refusee_et_sauvegarde_avant_ecriture() {
        let d = tmp("sauvegarde");
        let r = d.join("extensions");
        ecrire_ext(&r, "ecrit", api::API_VERSION, b"x", &[api::PERM_DB_WRITE]);
        // Pas de base : sauvegarde impossible → jamais chargée.
        let h = ExtensionHost::with_roots(&d, &d.join("absente.sqlite3"), "t", vec![r.clone()]);
        let info = h.set_enabled("ecrit", true).unwrap();
        match info.state {
            Etat::Erreur(m) => assert!(m.contains("sauvegarde"), "{m}"),
            e => panic!("{e:?}"),
        }
        // Avec une base : la sauvegarde est faite (la dll bidon échoue ensuite).
        let db = d.join("base.sqlite3");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (42);").unwrap();
        let h = ExtensionHost::with_roots(&d, &db, "t", vec![r]);
        let info = h.set_enabled("ecrit", true).unwrap();
        let copie = PathBuf::from(info.last_backup.expect("sauvegarde attendue"));
        let v: i64 = rusqlite::Connection::open(&copie).unwrap().query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 42);
    }

    #[test]
    fn retention_des_sauvegardes() {
        let d = tmp("retention");
        for i in 0..6 {
            std::fs::write(d.join(format!("2026010{i}-000000-avant-x.sqlite3")), "").unwrap();
        }
        purger_sauvegardes(&d, 3);
        let mut restants: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        restants.sort();
        assert_eq!(restants.len(), 3);
        assert!(restants[0].starts_with("20260103"));
    }

    #[test]
    fn ui_file_refuse_la_sortie_du_dossier() {
        let d = tmp("ui");
        let r = d.join("extensions");
        ecrire_ext(&r, "ok", api::API_VERSION, b"x", &[]);
        let h = ExtensionHost::with_roots(&d, &d.join("x.sqlite3"), "t", vec![r]);
        assert!(h.ui_file("ok", "index.html").is_some());
        assert!(h.ui_file("ok", "../manifest.json").is_none());
        assert!(h.ui_file("ok", "absent.js").is_none());
    }

    #[test]
    fn data_file_ne_sert_que_les_images_du_dossier_de_l_extension() {
        let d = tmp("donnees");
        let r = d.join("extensions");
        ecrire_ext(&r, "ok", api::API_VERSION, b"x", &[]);
        let h = ExtensionHost::with_roots(&d, &d.join("x.sqlite3"), "t", vec![r]);
        let dd = d.join("extensions-donnees").join("ok").join("relief");
        std::fs::create_dir_all(&dd).unwrap();
        std::fs::write(dd.join("topo.jpg"), b"jpeg").unwrap();
        std::fs::write(dd.join("grille.json"), b"[]").unwrap();
        std::fs::write(d.join("secret.png"), b"png").unwrap();
        assert!(h.data_file("ok", "relief/topo.jpg").is_some());
        assert!(h.data_file("ok", "relief/grille.json").is_none());
        assert!(h.data_file("ok", "../../secret.png").is_none());
        assert!(h.data_file("inconnue", "relief/topo.jpg").is_none());
    }
}

// ---------------------------------------------------------------------------
// Commandes Tauri
// ---------------------------------------------------------------------------

pub mod commands {
    use super::{ExtensionHost, ExtensionInfo};
    use std::sync::Arc;
    use tauri::State;

    #[tauri::command(async, rename_all = "snake_case")]
    pub fn list_extensions(host: State<'_, Arc<ExtensionHost>>) -> Vec<ExtensionInfo> {
        host.decouvrir();
        host.list()
    }

    /// Asynchrone : activer une extension `db_write` copie toute la base.
    #[tauri::command(rename_all = "snake_case")]
    pub async fn set_extension_enabled(
        host: State<'_, Arc<ExtensionHost>>,
        id: String,
        enabled: bool,
    ) -> Result<ExtensionInfo, String> {
        let host = host.inner().clone();
        tauri::async_runtime::spawn_blocking(move || host.set_enabled(&id, enabled))
            .await
            .map_err(|e| e.to_string())?
    }

    /// Relais des appels du panneau d'une extension (via `postMessage`).
    #[tauri::command(rename_all = "snake_case")]
    pub async fn extension_call(
        host: State<'_, Arc<ExtensionHost>>,
        id: String,
        method: String,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let host = host.inner().clone();
        let debut = std::time::Instant::now();
        println!("[gws] extension {id} : appel « {method} »");
        let r = tauri::async_runtime::spawn_blocking(move || {
            let r = host.call(&id, &method, &payload);
            (id, method, r)
        })
        .await
        .map_err(|e| e.to_string())?;
        let (id, method, r) = r;
        match &r {
            Ok(_) => println!("[gws] extension {id} : « {method} » terminé en {} ms", debut.elapsed().as_millis()),
            Err(e) => println!("[gws] extension {id} : « {method} » en erreur : {e}"),
        }
        r
    }
}

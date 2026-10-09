//! Retour arrière automatique après une mise à jour `.gws` ratée.
//!
//! ## Protocole
//!
//! 1. L'ancienne version ([`crate::package::apply_update`]) renomme son exe en
//!    `.exe.old`, écrit le nouveau, pose le marqueur `.exe.maj-en-attente`
//!    ([`Chemins::installer`]), lance la nouvelle version (`--apres-maj`), puis
//!    **le gardien** : sa propre copie `.exe.old` relancée avec
//!    `--gardien-maj <pid> <exe>`, détachée, sans fenêtre ([`lancer_nouvelle_version`]).
//!    Elle quitte ensuite.
//! 2. Le gardien ([`executer_si_demande`]) s'arrête avant le moindre code Tauri :
//!    ni fenêtre, ni base, ni verrou d'instance unique. Il ouvre une poignée
//!    sur le processus `<pid>` (la poignée empêche la réutilisation du PID) et
//!    surveille ([`surveiller`]) :
//!    - marqueur supprimé → mise à jour confirmée, il quitte ;
//!    - processus mort (plantage, panique, DLL manquante) → restauration ;
//!    - marqueur non rafraîchi depuis [`Delais::silence`], ou attente totale
//!      au-delà de [`Delais::total`] → il termine CE processus (poignée), puis
//!      restaure.
//!
//!    Restaurer ([`Chemins::restaurer`]) : exe raté → `.exe.echec`, `.old` →
//!    exe, suppression de `.echec` et du marqueur, relance de l'ancienne version.
//! 3. La nouvelle version rafraîchit le marqueur pendant son `setup` (ouverture
//!    de la base, migrations éventuellement longues : [`au_demarrage`] /
//!    [`fin_du_setup`]), puis le supprime quand le frontend a réellement
//!    démarré (commande `consommer_apres_maj`, appelée au lancement de
//!    l'interface : [`confirmer_demarrage`]). Le `.old` est supprimé une fois
//!    le gardien sorti (il est verrouillé tant que le gardien s'exécute).
//!
//! Le gardien est le code de l'ANCIENNE version : la protection ne vaut qu'à
//! partir des mises à jour appliquées depuis une version qui contient ce module.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// Argument qui lance l'exe en mode gardien (aucune interface).
pub const ARG_GARDIEN_MAJ: &str = "--gardien-maj";

/// Emplacements des fichiers d'une mise à jour, dérivés du chemin de l'exe.
#[derive(Debug, Clone)]
pub struct Chemins {
    pub exe: PathBuf,
    pub old: PathBuf,
    pub marqueur: PathBuf,
    pub echec: PathBuf,
}

/// Réessaie une opération de fichier : sous Windows, l'image d'un processus
/// qui vient d'être terminé peut rester verrouillée quelques instants.
fn reessayer(mut f: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let mut derniere = None;
    for _ in 0..40 {
        match f() {
            Ok(()) => return Ok(()),
            Err(e) => derniere = Some(e),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(derniere.unwrap())
}

impl Chemins {
    pub fn pour(exe: &Path) -> Self {
        Chemins {
            exe: exe.to_path_buf(),
            // Même nom qu'avant (0.4.x) : `with_extension("exe.old")`.
            old: exe.with_extension("exe.old"),
            marqueur: exe.with_extension("exe.maj-en-attente"),
            echec: exe.with_extension("exe.echec"),
        }
    }

    pub fn courants() -> Option<Self> {
        std::env::current_exe().ok().map(|e| Self::pour(&e))
    }

    /// Remplace l'exe par `octets` (l'ancien devient `.old`) et pose le
    /// marqueur d'attente. En cas d'échec, l'ancien exe est remis en place.
    pub fn installer(&self, octets: &[u8]) -> Result<(), String> {
        let _ = std::fs::remove_file(&self.old);
        let _ = std::fs::remove_file(&self.echec);
        let _ = std::fs::remove_file(&self.marqueur);
        if self.old.exists() {
            return Err(format!(
                "Un ancien exécutable ({}) n'a pas pu être supprimé : une mise à jour est peut-être encore en cours. Réessayez dans une minute.",
                self.old.display()
            ));
        }
        std::fs::rename(&self.exe, &self.old)
            .map_err(|e| format!("Impossible de préparer le remplacement de l'exécutable : {e}"))?;
        if let Err(e) = std::fs::write(&self.exe, octets) {
            let _ = std::fs::remove_file(&self.exe);
            let _ = std::fs::rename(&self.old, &self.exe);
            return Err(format!("Écriture de la nouvelle version échouée : {e}"));
        }
        // Sans marqueur, le gardien considère la mise à jour confirmée : on
        // perd le retour arrière, pas la mise à jour.
        if let Err(e) = std::fs::write(&self.marqueur, b"en attente de confirmation") {
            eprintln!("[gws] marqueur de mise à jour non écrit : {e}");
        }
        Ok(())
    }

    /// Remet l'ancienne version en place. Le processus de la version ratée
    /// doit être terminé.
    pub fn restaurer(&self) -> Result<(), String> {
        if !self.old.exists() {
            return Err(format!("Ancienne version introuvable : {}", self.old.display()));
        }
        if self.exe.exists() {
            let _ = std::fs::remove_file(&self.echec);
            reessayer(|| std::fs::rename(&self.exe, &self.echec))
                .map_err(|e| format!("Version ratée non déplacée : {e}"))?;
        }
        if let Err(e) = reessayer(|| std::fs::rename(&self.old, &self.exe)) {
            // Surtout ne pas rester sans exe : remettre au moins la version ratée.
            let _ = std::fs::rename(&self.echec, &self.exe);
            return Err(format!("Ancienne version non restaurée : {e}"));
        }
        let _ = std::fs::remove_file(&self.echec);
        let _ = std::fs::remove_file(&self.marqueur);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Surveillance (logique pure, testée avec un processus simulé)
// ---------------------------------------------------------------------------

/// Processus surveillé par le gardien.
pub trait Surveille {
    fn vivant(&mut self) -> bool;
    /// Termine ce processus-là (et attend sa fin).
    fn terminer(&mut self);
}

#[derive(Debug, Clone, Copy)]
pub struct Delais {
    /// Marqueur non rafraîchi depuis ce délai → échec.
    pub silence: Duration,
    /// Attente maximale, rafraîchissements compris.
    pub total: Duration,
    pub pas: Duration,
}

pub const DELAIS: Delais = Delais {
    silence: Duration::from_secs(20),
    total: Duration::from_secs(180),
    pas: Duration::from_millis(250),
};

#[derive(Debug, PartialEq)]
pub enum Issue {
    Confirmee,
    /// La version ratée a été remplacée par l'ancienne (et relancée si possible).
    Restauree,
    RestaurationImpossible(String),
}

fn silence_du_marqueur(m: &Path) -> Duration {
    std::fs::metadata(m)
        .and_then(|md| md.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or_default()
}

pub fn surveiller(
    p: &mut dyn Surveille,
    ch: &Chemins,
    d: Delais,
    relancer: &mut dyn FnMut(&Path) -> std::io::Result<()>,
) -> Issue {
    let debut = Instant::now();
    loop {
        if !ch.marqueur.exists() {
            return Issue::Confirmee;
        }
        if !p.vivant() {
            // Confirmée juste avant de se fermer : rien à restaurer.
            if !ch.marqueur.exists() {
                return Issue::Confirmee;
            }
            break;
        }
        if silence_du_marqueur(&ch.marqueur) > d.silence || debut.elapsed() > d.total {
            p.terminer();
            break;
        }
        std::thread::sleep(d.pas);
    }
    match ch.restaurer() {
        Ok(()) => {
            if let Err(e) = relancer(&ch.exe) {
                eprintln!("[gws] ancienne version restaurée mais non relancée : {e}");
            }
            Issue::Restauree
        }
        Err(e) => Issue::RestaurationImpossible(e),
    }
}

// ---------------------------------------------------------------------------
// Côté nouvelle version : rafraîchissement et confirmation
// ---------------------------------------------------------------------------

static BATTEMENT: AtomicBool = AtomicBool::new(false);

fn toucher(m: &Path) {
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(m) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// À appeler en tout début de `run()` (après le test du mode gardien) : si une
/// mise à jour attend confirmation, rafraîchit le marqueur jusqu'à
/// [`fin_du_setup`] ; sinon supprime un éventuel `.old` résiduel.
pub fn au_demarrage() {
    let Some(ch) = Chemins::courants() else { return };
    if ch.marqueur.exists() {
        BATTEMENT.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            while BATTEMENT.load(Ordering::SeqCst) && ch.marqueur.exists() {
                toucher(&ch.marqueur);
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    } else {
        let _ = std::fs::remove_file(&ch.old);
        let _ = std::fs::remove_file(&ch.echec);
    }
}

/// Fin du `setup` : plus de rafraîchissement ; l'interface a
/// [`Delais::silence`] pour confirmer.
pub fn fin_du_setup() {
    if BATTEMENT.swap(false, Ordering::SeqCst) {
        if let Some(ch) = Chemins::courants() {
            toucher(&ch.marqueur);
        }
    }
}

/// L'interface a démarré : la mise à jour est confirmée. Le `.old` est
/// supprimé dès que le gardien l'a libéré.
pub fn confirmer_demarrage() {
    BATTEMENT.store(false, Ordering::SeqCst);
    let Some(ch) = Chemins::courants() else { return };
    if !ch.marqueur.exists() && !ch.old.exists() {
        return;
    }
    let _ = std::fs::remove_file(&ch.marqueur);
    std::thread::spawn(move || {
        let _ = std::fs::remove_file(&ch.echec);
        for _ in 0..60 {
            if !ch.old.exists() || std::fs::remove_file(&ch.old).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

// ---------------------------------------------------------------------------
// Windows : processus réels
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    /// Poignée sur un processus : tant qu'elle est ouverte, son PID ne peut
    /// pas être réattribué à un autre processus.
    pub struct Processus(HANDLE);

    // SAFETY : une poignée de processus est utilisable depuis n'importe quel fil.
    unsafe impl Send for Processus {}

    impl Processus {
        pub fn ouvrir(pid: u32) -> Option<Self> {
            // SAFETY : appel FFI sans pointeur ; résultat nul en cas d'échec.
            let h = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
            (!h.is_null()).then_some(Processus(h))
        }
    }

    impl super::Surveille for Processus {
        fn vivant(&mut self) -> bool {
            // SAFETY : poignée valide jusqu'au Drop.
            unsafe { WaitForSingleObject(self.0, 0) == WAIT_TIMEOUT }
        }
        fn terminer(&mut self) {
            // SAFETY : poignée valide, ouverte avec PROCESS_TERMINATE.
            unsafe {
                TerminateProcess(self.0, 1);
                WaitForSingleObject(self.0, 10_000);
            }
        }
    }

    impl Drop for Processus {
        fn drop(&mut self) {
            // SAFETY : poignée ouverte par OpenProcess, fermée une seule fois.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
pub use win::Processus;

/// Processus déjà terminé au moment où le gardien a voulu l'ouvrir.
struct Disparu;
impl Surveille for Disparu {
    fn vivant(&mut self) -> bool {
        false
    }
    fn terminer(&mut self) {}
}

#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Lance la nouvelle version, puis le gardien depuis la copie `.old`.
/// Si la nouvelle version ne peut pas démarrer, l'ancienne est remise en place.
#[cfg(windows)]
pub fn lancer_nouvelle_version(ch: &Chemins) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let enfant = match std::process::Command::new(&ch.exe).arg(crate::package::ARG_APRES_MAJ).spawn() {
        Ok(e) => e,
        Err(e) => {
            let r = ch.restaurer();
            return Err(format!(
                "Impossible de démarrer la nouvelle version : {e}.{}",
                match r {
                    Ok(()) => " L'ancienne version a été conservée.".to_string(),
                    Err(r) => format!(" Restauration impossible : {r}"),
                }
            ));
        }
    };
    let gardien = std::process::Command::new(&ch.old)
        .arg(ARG_GARDIEN_MAJ)
        .arg(enfant.id().to_string())
        .arg(&ch.exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .spawn();
    if let Err(e) = gardien {
        // Pas de retour arrière possible, mais la mise à jour reste valable.
        eprintln!("[gws] gardien de mise à jour non lancé : {e}");
        let _ = std::fs::remove_file(&ch.marqueur);
    }
    Ok(())
}

/// Point d'entrée du mode gardien. À appeler en toute première ligne de
/// `run()` : renvoie `Some(code)` si ce processus est un gardien (l'appelant
/// quitte alors aussitôt, sans rien initialiser d'autre).
pub fn executer_si_demande() -> Option<i32> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.get(1).map(|a| a == ARG_GARDIEN_MAJ) != Some(true) {
        return None;
    }
    let (Some(pid), Some(exe)) = (
        args.get(2).and_then(|a| a.to_str()).and_then(|s| s.parse::<u32>().ok()),
        args.get(3),
    ) else {
        return Some(2);
    };
    let ch = Chemins::pour(Path::new(exe));
    #[cfg(windows)]
    let issue = {
        use std::os::windows::process::CommandExt;
        let mut relancer = |exe: &Path| {
            std::process::Command::new(exe)
                .creation_flags(DETACHED_PROCESS)
                .spawn()
                .map(|_| ())
        };
        match Processus::ouvrir(pid) {
            Some(mut p) => surveiller(&mut p, &ch, DELAIS, &mut relancer),
            None => surveiller(&mut Disparu, &ch, DELAIS, &mut relancer),
        }
    };
    #[cfg(not(windows))]
    let issue = {
        let _ = pid;
        surveiller(&mut Disparu, &ch, DELAIS, &mut |_| Ok(()))
    };
    Some(match issue {
        Issue::Confirmee | Issue::Restauree => 0,
        Issue::RestaurationImpossible(_) => 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn dossier() -> PathBuf {
        let d = std::env::temp_dir().join(format!("gws_gardien_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Dossier d'installation factice : exe « ancien », puis mise à jour
    /// installée par `Chemins::installer`.
    fn installe(d: &Path) -> Chemins {
        let ch = Chemins::pour(&d.join("app.exe"));
        std::fs::write(&ch.exe, b"ANCIEN").unwrap();
        ch.installer(b"NOUVEAU").unwrap();
        ch
    }

    const RAPIDE: Delais = Delais {
        silence: Duration::from_millis(600),
        total: Duration::from_secs(5),
        pas: Duration::from_millis(20),
    };

    struct Simule {
        vivant: Cell<bool>,
        termine: Cell<bool>,
        meurt_apres: Option<u32>,
        appels: u32,
    }
    impl Simule {
        fn new(meurt_apres: Option<u32>) -> Self {
            Simule { vivant: Cell::new(true), termine: Cell::new(false), meurt_apres, appels: 0 }
        }
    }
    impl Surveille for Simule {
        fn vivant(&mut self) -> bool {
            self.appels += 1;
            if self.meurt_apres.is_some_and(|n| self.appels > n) {
                self.vivant.set(false);
            }
            self.vivant.get()
        }
        fn terminer(&mut self) {
            self.termine.set(true);
            self.vivant.set(false);
        }
    }

    #[test]
    fn installer_pose_old_et_marqueur() {
        let d = dossier();
        let ch = installe(&d);
        assert_eq!(std::fs::read(&ch.exe).unwrap(), b"NOUVEAU");
        assert_eq!(std::fs::read(&ch.old).unwrap(), b"ANCIEN");
        assert!(ch.marqueur.exists());
        // Suffixes compatibles avec la 0.4.x (.exe.old).
        assert_eq!(ch.old.file_name().unwrap(), "app.exe.old");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn confirmation_rien_n_est_restaure() {
        let d = dossier();
        let ch = installe(&d);
        let ch2 = ch.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::remove_file(&ch2.marqueur).unwrap();
        });
        let mut p = Simule::new(None);
        let mut relances = 0;
        let issue = surveiller(&mut p, &ch, RAPIDE, &mut |_| {
            relances += 1;
            Ok(())
        });
        t.join().unwrap();
        assert_eq!(issue, Issue::Confirmee);
        assert!(!p.termine.get());
        assert_eq!(relances, 0);
        assert_eq!(std::fs::read(&ch.exe).unwrap(), b"NOUVEAU");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn plantage_restaure_et_relance_l_ancienne() {
        let d = dossier();
        let ch = installe(&d);
        let mut p = Simule::new(Some(3));
        let mut relance = None;
        let issue = surveiller(&mut p, &ch, RAPIDE, &mut |e| {
            relance = Some(e.to_path_buf());
            Ok(())
        });
        assert_eq!(issue, Issue::Restauree);
        assert!(!p.termine.get(), "déjà mort : rien à terminer");
        assert_eq!(relance.as_deref(), Some(ch.exe.as_path()));
        assert_eq!(std::fs::read(&ch.exe).unwrap(), b"ANCIEN");
        assert!(!ch.old.exists() && !ch.marqueur.exists() && !ch.echec.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn silence_termine_le_processus_puis_restaure() {
        let d = dossier();
        let ch = installe(&d);
        let mut p = Simule::new(None);
        let debut = Instant::now();
        let issue = surveiller(&mut p, &ch, RAPIDE, &mut |_| Ok(()));
        assert_eq!(issue, Issue::Restauree);
        assert!(p.termine.get());
        assert!(debut.elapsed() >= RAPIDE.silence);
        assert_eq!(std::fs::read(&ch.exe).unwrap(), b"ANCIEN");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rafraichissement_prolonge_puis_plafond_total() {
        let d = dossier();
        let ch = installe(&d);
        let m = ch.marqueur.clone();
        let fin = Instant::now() + Duration::from_millis(2500);
        let t = std::thread::spawn(move || {
            while Instant::now() < fin {
                toucher(&m);
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let delais = Delais { total: Duration::from_millis(1500), ..RAPIDE };
        let debut = Instant::now();
        let mut p = Simule::new(None);
        let issue = surveiller(&mut p, &ch, delais, &mut |_| Ok(()));
        let duree = debut.elapsed();
        t.join().unwrap();
        // Rafraîchi : pas d'échec au bout du silence (0,6 s)…
        assert!(duree >= Duration::from_millis(1500), "{duree:?}");
        // …mais le plafond total l'emporte.
        assert!(duree < Duration::from_millis(2400), "{duree:?}");
        assert_eq!(issue, Issue::Restauree);
        assert!(p.termine.get());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sans_old_restauration_impossible_et_exe_conserve() {
        let d = dossier();
        let ch = installe(&d);
        std::fs::remove_file(&ch.old).unwrap();
        let issue = surveiller(&mut Disparu, &ch, RAPIDE, &mut |_| panic!("pas de relance"));
        assert!(matches!(issue, Issue::RestaurationImpossible(_)));
        assert_eq!(std::fs::read(&ch.exe).unwrap(), b"NOUVEAU");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Bout en bout avec de vrais processus Windows, dans un dossier
    /// temporaire (jamais l'exe installé) : la « nouvelle version » est une
    /// copie de ping.exe qui ne confirme jamais ; le « gardien » tourne depuis
    /// la copie `.old` (ping.exe + octets de fin qui la distinguent), comme en
    /// vrai. Le gardien doit terminer ce processus précis, renommer sa propre
    /// image `.old` → exe, et relancer.
    #[cfg(windows)]
    #[test]
    fn bout_en_bout_processus_reels() {
        use std::process::{Command, Stdio};
        let d = dossier();
        let ping = PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32").join("PING.EXE");
        let ch = Chemins::pour(&d.join("app.exe"));
        let mut ancien = std::fs::read(&ping).unwrap();
        ancien.extend_from_slice(b"GWS_ANCIENNE_VERSION");
        std::fs::write(&ch.exe, &ancien).unwrap();
        ch.installer(&std::fs::read(&ping).unwrap()).unwrap();

        let lancer = |exe: &Path| {
            Command::new(exe)
                .args(["-n", "60", "127.0.0.1"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut nouvelle = lancer(&ch.exe);
        let mut image_gardien = lancer(&ch.old); // image .old en cours d'exécution
        let mut p = Processus::ouvrir(nouvelle.id()).expect("poignée");
        assert!(p.vivant());

        let mut relancee = None;
        let issue = surveiller(&mut p, &ch, RAPIDE, &mut |e| {
            relancee = Some(
                Command::new(e).args(["-n", "1", "127.0.0.1"]).stdout(Stdio::null()).spawn()?,
            );
            Ok(())
        });
        assert_eq!(issue, Issue::Restauree);
        assert!(nouvelle.try_wait().unwrap().is_some(), "nouvelle version terminée");
        assert!(image_gardien.try_wait().unwrap().is_none(), "seul le PID visé est terminé");
        assert_eq!(std::fs::read(&ch.exe).unwrap(), ancien, "ancienne version en place");
        assert!(!ch.old.exists() && !ch.marqueur.exists() && !ch.echec.exists());
        let st = relancee.expect("relancée").wait().unwrap();
        assert!(st.success(), "l'exe restauré démarre");

        image_gardien.kill().unwrap();
        image_gardien.wait().unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn processus_disparu_ou_pid_invalide() {
        // PID 0 (processus inactif du système) : ouverture refusée.
        assert!(Processus::ouvrir(0).is_none());
    }
}

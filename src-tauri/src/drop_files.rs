//! Fichiers déposés depuis le système sur la fenêtre.
//!
//! Tauri intercepte le glisser-déposer : l'événement HTML `drop` ne reçoit
//! aucun fichier, seul l'événement Tauri donne les chemins. Pour que la zone
//! de dépôt de l'assistant IA puisse lire ces fichiers sans ouvrir le greffon
//! `fs` au JS, Rust retient ici les chemins que l'utilisateur vient réellement
//! de déposer (événement de fenêtre, hors de portée du JS), et
//! [`read`] ne lit que ceux-là : pas de lecture d'un chemin arbitraire.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::{AppError, AppResult};

/// Un dépôt reste lisible pendant ce délai.
const VALIDITE: Duration = Duration::from_secs(600);
/// Au-delà, le fichier n'est pas lu (les fichiers de l'IA sont petits).
pub const TAILLE_MAX: u64 = 20 * 1024 * 1024;
const NB_MAX: usize = 200;
/// Formats de la zone de dépôt de l'assistant IA, plus les classeurs ElitechLog
/// (`.xls` / `.xlsx`) qu'un dépôt sur la fenêtre envoie à l'import.
const EXTENSIONS: &[&str] = &[
    "csv", "tsv", "json", "txt", "dat", "log", "png", "jpg", "jpeg", "webp", "xls", "xlsx",
];

static DEPOSES: Mutex<Option<(Instant, Vec<PathBuf>)>> = Mutex::new(None);

/// Retient les chemins du dernier dépôt (remplace le précédent).
pub fn remember(paths: &[PathBuf]) {
    let liste = paths.iter().take(NB_MAX).cloned().collect();
    if let Ok(mut d) = DEPOSES.lock() {
        *d = Some((Instant::now(), liste));
    }
}

fn autorise(path: &Path, maintenant: Instant) -> bool {
    let Ok(d) = DEPOSES.lock() else { return false };
    matches!(&*d, Some((quand, liste)) if maintenant.duration_since(*quand) <= VALIDITE && liste.iter().any(|p| p == path))
}

fn extension_ok(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Contenu d'un fichier du dernier dépôt.
pub fn read(path: &str) -> AppResult<Vec<u8>> {
    let p = Path::new(path);
    if !autorise(p, Instant::now()) {
        return Err(AppError::Other("Ce fichier ne fait pas partie du dernier dépôt.".into()));
    }
    if !extension_ok(p) {
        return Err(AppError::Other(format!("Format non pris en charge : {path}")));
    }
    let meta = std::fs::metadata(p).map_err(|e| AppError::Other(format!("Fichier illisible ({e}) : {path}")))?;
    if !meta.is_file() {
        return Err(AppError::Other(format!("Ce n'est pas un fichier : {path}")));
    }
    if meta.len() > TAILLE_MAX {
        return Err(AppError::Other(format!("Fichier trop volumineux (plus de 20 Mo) : {path}")));
    }
    std::fs::read(p).map_err(|e| AppError::Other(format!("Fichier illisible ({e}) : {path}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Un seul test : l'état est global au processus.
    #[test]
    fn seuls_les_fichiers_deposes_sont_lus() {
        let dir = std::env::temp_dir().join(format!("gws-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("mesures.CSV");
        let exe = dir.join("outil.exe");
        let autre = dir.join("autre.csv");
        std::fs::write(&csv, b"t;v\n1;2\n").unwrap();
        std::fs::write(&exe, b"MZ").unwrap();
        std::fs::write(&autre, b"x").unwrap();

        // Rien de déposé : refus.
        assert!(read(csv.to_str().unwrap()).is_err());

        remember(&[csv.clone(), exe.clone(), dir.clone()]);
        assert_eq!(read(csv.to_str().unwrap()).unwrap(), b"t;v\n1;2\n");
        assert!(read(autre.to_str().unwrap()).is_err(), "non déposé");
        assert!(read(exe.to_str().unwrap()).is_err(), "extension refusée");
        assert!(read(dir.to_str().unwrap()).is_err(), "dossier");
        let detour = dir.join("..").join(dir.file_name().unwrap()).join("mesures.CSV");
        assert!(read(detour.to_str().unwrap()).is_err(), "chemin différent de celui déposé");

        // Un nouveau dépôt remplace le précédent.
        remember(&[autre.clone()]);
        assert!(read(csv.to_str().unwrap()).is_err());
        assert!(read(autre.to_str().unwrap()).is_ok());

        // Dépôt périmé.
        assert!(!autorise(&autre, Instant::now() + VALIDITE + Duration::from_secs(1)));

        std::fs::remove_dir_all(&dir).ok();
    }
}

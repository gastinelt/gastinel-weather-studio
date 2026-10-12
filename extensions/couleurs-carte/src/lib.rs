//! Extension « Couleurs de la carte » : réglage de la luminosité, de la saturation
//! et de la netteté du fond de carte.
//!
//! Aucun accès direct à la base de données de l'application ni au réseau n'est
//! requis (`permissions: []`). Les réglages sont persistés dans un fichier JSON
//! situé dans le dossier dédié de l'extension (`ctx.data_dir`).

use std::fs;
use std::path::{Path, PathBuf};

use gws_extension_api::{declare_extension, Extension, HostContext};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const NOM_FICHIER_REGLAGES: &str = "reglages.json";
pub const BORNE_MIN: i32 = -100;
pub const BORNE_MAX: i32 = 100;

/// Réglages visuels du fond de carte (-100 à +100, 0 = neutre).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reglages {
    pub luminosite: i32,
    pub saturation: i32,
    pub nettete: i32,
}

impl Default for Reglages {
    fn default() -> Self {
        Self {
            luminosite: 0,
            saturation: 0,
            nettete: 0,
        }
    }
}

impl Reglages {
    /// Borne chaque composante entre BORNE_MIN et BORNE_MAX.
    pub fn normaliser(&self) -> Self {
        Self {
            luminosite: self.luminosite.clamp(BORNE_MIN, BORNE_MAX),
            saturation: self.saturation.clamp(BORNE_MIN, BORNE_MAX),
            nettete: self.nettete.clamp(BORNE_MIN, BORNE_MAX),
        }
    }
}

#[derive(Default)]
pub struct CouleursCarte {
    data_dir: Option<PathBuf>,
}

impl Extension for CouleursCarte {
    fn init(&mut self, ctx: HostContext) -> Result<(), String> {
        let dir = PathBuf::from(ctx.data_dir);
        if !dir.exists() {
            let _ = fs::create_dir_all(&dir);
        }
        self.data_dir = Some(dir);
        Ok(())
    }

    fn call(&mut self, method: &str, payload: Value) -> Result<Value, String> {
        match method {
            "ping" => Ok(json!("pong")),
            "get_settings" => {
                let r = self.lire_reglages();
                Ok(json!(r))
            }
            "save_settings" => {
                let r = parse_reglages(&payload)?.normaliser();
                self.ecrire_reglages(r)?;
                Ok(json!(r))
            }
            "reset_settings" => {
                let defaut = Reglages::default();
                self.ecrire_reglages(defaut)?;
                Ok(json!(defaut))
            }
            m => Err(format!("méthode inconnue : {m}")),
        }
    }
}

impl CouleursCarte {
    fn chemin_fichier(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|d| d.join(NOM_FICHIER_REGLAGES))
    }

    fn lire_reglages(&self) -> Reglages {
        let Some(chemin) = self.chemin_fichier() else {
            return Reglages::default();
        };
        charger_reglages(&chemin)
    }

    fn ecrire_reglages(&self, r: Reglages) -> Result<(), String> {
        let chemin = self.chemin_fichier().ok_or_else(|| "dossier de données non initialisé".to_string())?;
        sauvegarder_reglages(&chemin, r)
    }
}

fn parse_reglages(p: &Value) -> Result<Reglages, String> {
    let lum = p.get("luminosite").and_then(Value::as_i64).unwrap_or(0) as i32;
    let sat = p.get("saturation").and_then(Value::as_i64).unwrap_or(0) as i32;
    let net = p.get("nettete").and_then(Value::as_i64).unwrap_or(0) as i32;
    Ok(Reglages {
        luminosite: lum,
        saturation: sat,
        nettete: net,
    })
}

fn charger_reglages(chemin: &Path) -> Reglages {
    match fs::read_to_string(chemin) {
        Ok(texte) => serde_json::from_str::<Reglages>(&texte)
            .map(|r| r.normaliser())
            .unwrap_or_default(),
        Err(_) => Reglages::default(),
    }
}

fn sauvegarder_reglages(chemin: &Path, r: Reglages) -> Result<(), String> {
    if let Some(parent) = chemin.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| format!("création du dossier parent : {e}"))?;
        }
    }
    let contenu = serde_json::to_string_pretty(&r).map_err(|e| format!("sérialisation : {e}"))?;
    fs::write(chemin, contenu).map_err(|e| format!("écriture de {NOM_FICHIER_REGLAGES} : {e}"))
}

declare_extension!(CouleursCarte);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reglages_defaut() {
        let r = Reglages::default();
        assert_eq!(r.luminosite, 0);
        assert_eq!(r.saturation, 0);
        assert_eq!(r.nettete, 0);
    }

    #[test]
    fn test_normalisation_bornes() {
        let r = Reglages {
            luminosite: 250,
            saturation: -180,
            nettete: 45,
        }
        .normaliser();

        assert_eq!(r.luminosite, 100);
        assert_eq!(r.saturation, -100);
        assert_eq!(r.nettete, 45);
    }

    #[test]
    fn test_sauvegarde_et_relecture() {
        let tmp = std::env::temp_dir().join(format!("gws_test_couleurs_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let chemin = tmp.join("reglages.json");

        let initial = Reglages {
            luminosite: 15,
            saturation: -30,
            nettete: 80,
        };

        sauvegarder_reglages(&chemin, initial).expect("sauvegarde");
        let relu = charger_reglages(&chemin);
        assert_eq!(relu, initial);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_fichier_absent_rend_defaut() {
        let tmp = std::env::temp_dir().join(format!("gws_test_inexistant_{}", std::process::id()));
        let chemin = tmp.join("introuvable.json");
        let r = charger_reglages(&chemin);
        assert_eq!(r, Reglages::default());
    }
}

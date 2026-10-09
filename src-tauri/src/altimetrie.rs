//! Altitude d'un point via le service d'altimétrie de l'IGN (RGE ALTI).
//!
//! Même service que l'extension Carte 3D, mais pas le même code : l'extension
//! est une DLL facultative, que l'app ne peut pas appeler pour une fonction de
//! base. L'appel tient en une requête, et passe ici par le client HTTP commun
//! (`http_client`) comme tous les appels réseau de l'app.
//!
//! Le RGE ALTI couvre la France (métropole et outre-mer). Ailleurs, en mer ou
//! hors couverture, le service répond `-99999` : c'est traduit en erreur
//! explicite, jamais en altitude.

use serde_json::Value;

use crate::error::{AppError, AppResult};

const URL: &str = "https://data.geopf.fr/altimetrie/1.0/calcul/alti/rest/elevation.json";
const RESSOURCE: &str = "ign_rge_alti_wld";
/// Valeur renvoyée par l'IGN hors couverture.
const SANS_DONNEE: f64 = -99999.0;

/// Refuse les coordonnées impossibles avant tout appel réseau.
pub fn valider(lat: f64, lon: f64) -> AppResult<()> {
    if !lat.is_finite() || !lon.is_finite() || !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(AppError::Other(format!("coordonnées invalides : {lat}, {lon}")));
    }
    Ok(())
}

/// Extrait l'altitude de la réponse JSON (`{"elevations":[963.43]}` avec
/// `zonly=true`, ou `[{"z":…}]` sans).
pub fn lire_reponse(corps: &str) -> AppResult<f64> {
    let v: Value = serde_json::from_str(corps).map_err(|e| AppError::Other(format!("réponse IGN illisible : {e}")))?;
    let premiere = v
        .get("elevations")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| AppError::Other("réponse IGN sans altitude".into()))?;
    let z = premiere
        .as_f64()
        .or_else(|| premiere.get("z").and_then(Value::as_f64))
        .ok_or_else(|| AppError::Other("altitude IGN illisible".into()))?;
    if z <= SANS_DONNEE + 1.0 {
        return Err(AppError::Other(
            "pas d'altitude IGN à cet endroit : le RGE ALTI ne couvre que la France (hors mer)".into(),
        ));
    }
    Ok((z * 10.0).round() / 10.0)
}

/// Altitude au sol (m) d'un point, arrondie au décimètre.
pub fn altitude_ign(lat: f64, lon: f64) -> AppResult<f64> {
    valider(lat, lon)?;
    let client = crate::http_client::client_builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::Other(format!("client HTTP : {e}")))?;
    let rep = client
        .get(format!("{URL}?lon={lon:.6}&lat={lat:.6}&resource={RESSOURCE}&zonly=true"))
        .send()
        .map_err(|e| AppError::Other(format!("service IGN injoignable (connexion ?) : {e}")))?;
    let statut = rep.status();
    let corps = rep.text().map_err(|e| AppError::Other(format!("réponse IGN : {e}")))?;
    if !statut.is_success() {
        return Err(AppError::Other(format!("service IGN indisponible (HTTP {})", statut.as_u16())));
    }
    lire_reponse(&corps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lit_une_altitude() {
        assert_eq!(lire_reponse(r#"{"elevations":[963.43]}"#).unwrap(), 963.4);
        assert_eq!(lire_reponse(r#"{"elevations":[{"lon":5.57,"lat":45.1,"z":990.04,"acc":2.5}]}"#).unwrap(), 990.0);
    }

    #[test]
    fn hors_couverture_est_une_erreur() {
        let e = lire_reponse(r#"{"elevations":[-99999]}"#).unwrap_err().to_string();
        assert!(e.contains("France"), "{e}");
    }

    #[test]
    fn reponses_invalides() {
        assert!(lire_reponse("pas du json").is_err());
        assert!(lire_reponse(r#"{"elevations":[]}"#).is_err());
        assert!(lire_reponse(r#"{"autre":1}"#).is_err());
    }

    #[test]
    fn coordonnees_invalides_refusees_sans_reseau() {
        assert!(valider(91.0, 5.0).is_err());
        assert!(valider(45.0, 181.0).is_err());
        assert!(valider(f64::NAN, 5.0).is_err());
        assert!(valider(45.1, 5.57).is_ok());
    }

    #[test]
    #[ignore = "dépend du réseau (service IGN)"]
    fn appel_reel_lans_en_vercors() {
        // Les Bruyères, Lans-en-Vercors : 990 m saisis sur le terrain.
        let z = altitude_ign(45.10, 5.57).unwrap();
        assert!((900.0..1100.0).contains(&z), "{z}");
        // Au large de Marseille : hors couverture.
        assert!(altitude_ign(42.8, 5.3).is_err());
    }
}

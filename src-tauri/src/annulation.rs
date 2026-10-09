//! Annuler / Rétablir (Ctrl+Z / Ctrl+Y) des actions **destructrices**.
//!
//! Règle décidée par Théo (02/10/2026) : seules les suppressions et les
//! modifications de données s'annulent — supprimer un projet, une station,
//! une série, des mesures, corriger une valeur. Ajouter une station ou
//! importer des données ne s'annule pas.
//!
//! Principe : avant d'effacer, on garde une **copie** de ce qui part (projet,
//! points, séries, mesures). Annuler réécrit la copie ; Rétablir rejoue la
//! suppression. L'historique vit en mémoire, le temps de la session : fermer
//! l'application rend les suppressions définitives, comme avant.
//!
//! Garde-fou mémoire : au-delà de [`MAX_MESURES_CONSERVEES`] mesures copiées,
//! une suppression n'est pas annulable (projet France : des millions de
//! lignes) ; l'interface le dit avant de confirmer.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::Serialize;

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::model::{LoggerSeries, Measurement, Point, Project};

/// Nombre d'actions gardées dans l'historique.
pub const MAX_ACTIONS: usize = 20;
/// Plafond de mesures copiées, toutes actions confondues (~150 o par mesure).
pub const MAX_MESURES_CONSERVEES: usize = 2_000_000;

/// Une série et ses mesures, telles qu'avant la suppression.
#[derive(Debug, Clone)]
pub struct SerieCopiee {
    pub serie: LoggerSeries,
    pub mesures: Vec<Measurement>,
}

#[derive(Debug, Clone)]
pub struct PointCopie {
    pub point: Point,
    pub series: Vec<SerieCopiee>,
}

#[derive(Debug, Clone)]
pub enum Action {
    SupprimerProjet {
        projet: Project,
        points: Vec<PointCopie>,
        /// Réglages propres au projet effacés avec lui (exclusion Cloud, démo).
        reglages: Vec<(String, String)>,
    },
    SupprimerPoint(PointCopie),
    SupprimerSerie(SerieCopiee),
    /// Mesures effacées d'une série qui, elle, reste.
    SupprimerMesures { series_id: String, mesures: Vec<Measurement> },
    /// Valeur corrigée : `avant` = la mesure d'origine, `apres` = °C posés.
    ModifierTemperature { series_id: String, avant: Measurement, apres: f64 },
}

impl Action {
    fn nb_mesures(&self) -> usize {
        let serie = |s: &SerieCopiee| s.mesures.len();
        let point = |p: &PointCopie| p.series.iter().map(serie).sum::<usize>();
        match self {
            Action::SupprimerProjet { points, .. } => points.iter().map(point).sum(),
            Action::SupprimerPoint(p) => point(p),
            Action::SupprimerSerie(s) => serie(s),
            Action::SupprimerMesures { mesures, .. } => mesures.len(),
            Action::ModifierTemperature { .. } => 1,
        }
    }

    /// Texte court pour le message « Annulé : … ».
    pub fn libelle(&self) -> String {
        match self {
            Action::SupprimerProjet { projet, .. } => format!("suppression du projet « {} »", projet.name),
            Action::SupprimerPoint(p) => format!("suppression de la station « {} »", p.point.name),
            Action::SupprimerSerie(s) => format!(
                "suppression d'une série ({} mesures)",
                s.mesures.len()
            ),
            Action::SupprimerMesures { mesures, .. } => format!("suppression de {} mesure(s)", mesures.len()),
            Action::ModifierTemperature { .. } => "correction d'une valeur".to_string(),
        }
    }

    /// Remet en base ce que l'action avait effacé ou modifié.
    fn defaire(&self, db: &Db) -> AppResult<()> {
        fn remettre_point(db: &Db, p: &PointCopie) -> AppResult<()> {
            db.insert_or_replace_point(&p.point)?;
            for s in &p.series {
                db.insert_or_replace_series_with_measurements(&s.serie, &s.mesures)?;
            }
            Ok(())
        }
        match self {
            Action::SupprimerProjet { projet, points, reglages } => {
                db.create_project_with_id(
                    &projet.id,
                    &projet.name,
                    projet.created_at.timestamp(),
                    projet.last_opened_at.timestamp(),
                )?;
                for p in points {
                    remettre_point(db, p)?;
                }
                for (k, v) in reglages {
                    db.set_setting(k, v)?;
                }
            }
            Action::SupprimerPoint(p) => remettre_point(db, p)?,
            Action::SupprimerSerie(s) => {
                db.insert_or_replace_series_with_measurements(&s.serie, &s.mesures)?;
            }
            Action::SupprimerMesures { series_id, mesures } => {
                db.restore_measurements(series_id, mesures)?;
            }
            Action::ModifierTemperature { series_id, avant, .. } => {
                db.set_measurement_temperature(series_id, avant.id, avant.temperature_celsius)?;
            }
        }
        Ok(())
    }

    /// Rejoue l'action (Rétablir).
    fn refaire(&self, db: &Db) -> AppResult<()> {
        match self {
            Action::SupprimerProjet { projet, reglages, .. } => {
                db.delete_project(&projet.id)?;
                for (k, _) in reglages {
                    db.delete_setting(k)?;
                }
            }
            Action::SupprimerPoint(p) => db.delete_point(&p.point.id)?,
            Action::SupprimerSerie(s) => db.delete_series(&s.serie.id)?,
            Action::SupprimerMesures { series_id, mesures } => {
                // Les identifiants changent à la réinsertion : on efface par instant.
                for m in mesures {
                    let t = m.timestamp_utc.timestamp();
                    db.delete_measurements_range(series_id, t, t)?;
                }
            }
            Action::ModifierTemperature { series_id, avant, apres } => {
                db.set_measurement_temperature(series_id, avant.id, *apres)?;
            }
        }
        Ok(())
    }
}

/// Ce que l'interface affiche : libellés des prochaines actions possibles.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EtatAnnulation {
    pub annuler: Option<String>,
    pub retablir: Option<String>,
}

#[derive(Default)]
pub struct Historique {
    inner: Mutex<(VecDeque<Action>, Vec<Action>)>,
}

impl Historique {
    /// Enregistre une action **déjà faite**. Toute nouvelle action vide
    /// « Rétablir », comme dans un éditeur.
    pub fn pousser(&self, action: Action) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.1.clear();
        g.0.push_back(action);
        while g.0.len() > MAX_ACTIONS
            || (g.0.len() > 1 && g.0.iter().map(Action::nb_mesures).sum::<usize>() > MAX_MESURES_CONSERVEES)
        {
            g.0.pop_front();
        }
    }

    pub fn etat(&self) -> EtatAnnulation {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        EtatAnnulation {
            annuler: g.0.back().map(Action::libelle),
            retablir: g.1.last().map(Action::libelle),
        }
    }

    /// Défait la dernière action. Renvoie son libellé, `None` si rien à défaire.
    pub fn annuler(&self, db: &Db) -> AppResult<Option<String>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(action) = g.0.pop_back() else { return Ok(None) };
        if let Err(e) = action.defaire(db) {
            g.0.push_back(action);
            return Err(e);
        }
        let libelle = action.libelle();
        g.1.push(action);
        Ok(Some(libelle))
    }

    pub fn retablir(&self, db: &Db) -> AppResult<Option<String>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(action) = g.1.pop() else { return Ok(None) };
        if let Err(e) = action.refaire(db) {
            g.1.push(action);
            return Err(e);
        }
        let libelle = action.libelle();
        g.0.push_back(action);
        Ok(Some(libelle))
    }
}

// ---------------------------------------------------------------------------
// Copies avant suppression
// ---------------------------------------------------------------------------

fn copier_serie(db: &Db, serie: LoggerSeries) -> AppResult<SerieCopiee> {
    let mesures = db.measurements_for_series(&serie.id)?;
    Ok(SerieCopiee { serie, mesures })
}

fn copier_point(db: &Db, point: Point) -> AppResult<PointCopie> {
    let series = db
        .list_series_for_point(&point.id)?
        .into_iter()
        .map(|s| copier_serie(db, s))
        .collect::<AppResult<Vec<_>>>()?;
    Ok(PointCopie { point, series })
}

/// Nombre de mesures d'un projet, sans les lire (table mensuelle).
pub fn mesures_du_projet(db: &Db, project_id: &str) -> AppResult<usize> {
    Ok(db.project_coverage(project_id)?.iter().map(|c| c.measurement_count).sum())
}

/// Supprime un projet ; annulable s'il tient sous le plafond.
/// Renvoie `true` si la suppression est annulable.
pub fn supprimer_projet(db: &Db, h: &Historique, project_id: &str) -> AppResult<bool> {
    let projet = db.get_project(project_id)?;
    let annulable = mesures_du_projet(db, project_id)? <= MAX_MESURES_CONSERVEES;
    let cles = [
        format!("cloud_exclude:{project_id}"),
        format!("{}{project_id}", crate::demo::SETTING_DEMO_PREFIX),
    ];
    let action = if annulable {
        let points = db
            .list_points(project_id)?
            .into_iter()
            .map(|p| copier_point(db, p))
            .collect::<AppResult<Vec<_>>>()?;
        let mut reglages = Vec::new();
        for k in &cles {
            if let Some(v) = db.get_setting(k)? {
                reglages.push((k.clone(), v));
            }
        }
        Some(Action::SupprimerProjet { projet, points, reglages })
    } else {
        None
    };
    db.delete_project(project_id)?;
    for k in &cles {
        let _ = db.delete_setting(k);
    }
    if let Some(a) = action {
        h.pousser(a);
    }
    Ok(annulable)
}

pub fn supprimer_point(db: &Db, h: &Historique, point_id: &str) -> AppResult<()> {
    let copie = copier_point(db, db.get_point(point_id)?)?;
    db.delete_point(point_id)?;
    h.pousser(Action::SupprimerPoint(copie));
    Ok(())
}

pub fn supprimer_serie(db: &Db, h: &Historique, series_id: &str) -> AppResult<()> {
    let copie = copier_serie(db, db.get_series(series_id)?)?;
    db.delete_series(series_id)?;
    h.pousser(Action::SupprimerSerie(copie));
    Ok(())
}

/// Supprime les mesures entre deux instants UTC (secondes, inclus).
pub fn supprimer_plage(db: &Db, h: &Historique, series_id: &str, de: i64, a: i64) -> AppResult<usize> {
    if de > a {
        return Err(AppError::Other("Plage vide : la date de début suit la date de fin.".into()));
    }
    let mesures = db.measurements_in_range(series_id, de, a)?;
    if mesures.is_empty() {
        return Ok(0);
    }
    let n = db.delete_measurements_range(series_id, de, a)?;
    h.pousser(Action::SupprimerMesures { series_id: series_id.to_string(), mesures });
    Ok(n)
}

/// Supprime des mesures choisies une à une (valeurs aberrantes).
pub fn supprimer_mesures(db: &Db, h: &Historique, series_id: &str, ids: &[i64]) -> AppResult<usize> {
    let mesures = db.measurements_by_id(series_id, ids)?;
    if mesures.is_empty() {
        return Ok(0);
    }
    let n = db.delete_measurements_by_id(series_id, ids)?;
    h.pousser(Action::SupprimerMesures { series_id: series_id.to_string(), mesures });
    Ok(n)
}

pub fn modifier_temperature(db: &Db, h: &Historique, series_id: &str, id: i64, celsius: f64) -> AppResult<()> {
    if !celsius.is_finite() || !(-90.0..=70.0).contains(&celsius) {
        return Err(AppError::Other("Température hors de la plage plausible (−90 à 70 °C).".into()));
    }
    let avant = db
        .measurements_by_id(series_id, &[id])?
        .pop()
        .ok_or_else(|| AppError::NotFound("Mesure", id.to_string()))?;
    db.set_measurement_temperature(series_id, id, celsius)?;
    h.pousser(Action::ModifierTemperature { series_id: series_id.to_string(), avant, apres: celsius });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{now_utc, NewPoint, SeriesRole, SourceType};
    use crate::parsing::DateOrder;
    use chrono::TimeZone;

    /// Projet, une station, une série de 10 mesures à l'heure (0 à 9 °C).
    fn base() -> (Db, String, String, String) {
        let db = Db::open_in_memory().unwrap();
        let projet = db.create_project("Test").unwrap();
        let point = Point::from_new(NewPoint {
            project_id: projet.id.clone(),
            name: "Moucherolle".into(),
            latitude: 45.0,
            longitude: 5.5,
            altitude_m: Some(1900.0),
            category: "Cat3".into(),
            size: Some("Gr".into()),
            factor_sky: None,
            factor_wind: None,
            factor_slope: None,
            factor_snow_cover: None,
            factor_forest_cover: None,
            is_open_terrain: None,
            notes: None,
            infoclimat_station_id: None,
            romma_station_id: None,
        })
        .unwrap();
        db.insert_point(&point).unwrap();
        let serie = LoggerSeries {
            id: "s1".into(),
            point_id: point.id.clone(),
            source_type: SourceType::ElitechImport,
            role: SeriesRole::Primary,
            logger_serial: None,
            imported_at: now_utc(),
            raw_file_reference: None,
            clock_offset_seconds: 0,
            raw_timestamp_was_utc: true,
            label: None,
            sample_interval_seconds: Some(3600),
            data_license: None,
            station_id: None,
            last_api_fetch_at: None,
            date_order: DateOrder::DayFirst,
            date_format_uncertain: false,
            content_fingerprint: None,
            measurement_count: 0,
        };
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mesures: Vec<Measurement> = (0..10)
            .map(|i| {
                let t = t0 + chrono::Duration::hours(i);
                Measurement::new("s1", t, t, i as f64)
            })
            .collect();
        db.insert_series_with_measurements(&serie, &mesures).unwrap();
        (db, projet.id, point.id, "s1".into())
    }
    use chrono::Utc;

    fn temperatures(db: &Db) -> Vec<f64> {
        db.measurements_for_series("s1").unwrap().iter().map(|m| m.temperature_celsius).collect()
    }

    #[test]
    fn plage_supprimee_puis_annulee_puis_retablie() {
        let (db, _, _, s) = base();
        let h = Historique::default();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap().timestamp();
        assert_eq!(supprimer_plage(&db, &h, &s, t0 + 2 * 3600, t0 + 4 * 3600).unwrap(), 3);
        assert_eq!(temperatures(&db).len(), 7);
        assert_eq!(h.annuler(&db).unwrap().as_deref(), Some("suppression de 3 mesure(s)"));
        assert_eq!(temperatures(&db), (0..10).map(|i| i as f64).collect::<Vec<_>>());
        assert!(h.retablir(&db).unwrap().is_some());
        assert_eq!(temperatures(&db).len(), 7);
        assert_eq!(h.etat().retablir, None);
    }

    #[test]
    fn correction_de_valeur_annulee() {
        let (db, _, _, s) = base();
        let h = Historique::default();
        let id = db.measurements_for_series(&s).unwrap()[5].id;
        modifier_temperature(&db, &h, &s, id, 42.0).unwrap();
        assert_eq!(temperatures(&db)[5], 42.0);
        h.annuler(&db).unwrap();
        assert_eq!(temperatures(&db)[5], 5.0);
        h.retablir(&db).unwrap();
        assert_eq!(temperatures(&db)[5], 42.0);
        assert!(modifier_temperature(&db, &h, &s, id, 500.0).is_err());
    }

    #[test]
    fn station_supprimee_revient_avec_ses_mesures() {
        let (db, projet, point, _) = base();
        let h = Historique::default();
        supprimer_point(&db, &h, &point).unwrap();
        assert!(db.list_points(&projet).unwrap().is_empty());
        h.annuler(&db).unwrap();
        assert_eq!(db.list_points(&projet).unwrap().len(), 1);
        assert_eq!(temperatures(&db).len(), 10);
    }

    #[test]
    fn projet_supprime_revient_entier() {
        let (db, projet, _, _) = base();
        let h = Historique::default();
        assert!(supprimer_projet(&db, &h, &projet).unwrap());
        assert!(db.get_project(&projet).is_err());
        assert_eq!(h.etat().annuler.as_deref(), Some("suppression du projet « Test »"));
        h.annuler(&db).unwrap();
        assert_eq!(db.get_project(&projet).unwrap().name, "Test");
        assert_eq!(temperatures(&db).len(), 10);
    }

    #[test]
    fn nouvelle_action_vide_retablir() {
        let (db, _, _, s) = base();
        let h = Historique::default();
        let ids: Vec<i64> = db.measurements_for_series(&s).unwrap().iter().take(2).map(|m| m.id).collect();
        supprimer_mesures(&db, &h, &s, &ids[..1]).unwrap();
        h.annuler(&db).unwrap();
        assert!(h.etat().retablir.is_some());
        supprimer_serie(&db, &h, &s).unwrap();
        assert_eq!(h.etat().retablir, None);
    }

    #[test]
    fn trous_detectes() {
        let t: Vec<i64> = (0..20).map(|i| i * 600).chain([20 * 600 + 7200, 20 * 600 + 7800]).collect();
        let g = crate::commands::trous(&t, 10);
        assert_eq!(g, vec![(19 * 600, 20 * 600 + 7200)]);
    }
}

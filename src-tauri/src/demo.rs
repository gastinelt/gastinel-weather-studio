//! Projet d'exemple, créé à la demande (bouton « Explorer un projet d'exemple »).
//!
//! Rien ne se crée tout seul : seule la commande `create_demo_project` appelle
//! [`creer_projet_exemple`]. Les données sont **synthétiques** et
//! déterministes (pas d'aléa, un générateur congruentiel à graine fixe) : trois
//! points, six semaines de température et d'humidité au pas de 30 minutes, avec
//! un cycle jour/nuit, des épisodes de plusieurs jours et une inversion
//! nocturne plus marquée au fond de la combe.
//!
//! Le projet est marqué par le réglage `demo_project:<id>` : l'interface le
//! signale « Exemple », et [`crate::cloud::projet_exclu`] l'exclut de la
//! synchronisation Cloud quel que soit le réglage de synchro du projet.

use chrono::{DateTime, Duration, Timelike, Utc};

use crate::db::Db;
use crate::error::AppResult;
use crate::model::{now_utc, LoggerSeries, Measurement, NewPoint, Point, Project, SeriesRole, SourceType};
use crate::parsing::DateOrder;

/// Préfixe du réglage qui marque un projet d'exemple (valeur « 1 »).
pub const SETTING_DEMO_PREFIX: &str = "demo_project:";
/// Nom du projet créé.
pub const NOM_PROJET: &str = "Exemple — projet de démonstration";
/// Durée couverte et pas des mesures.
pub const JOURS: i64 = 42;
pub const PAS_SECONDES: i64 = 1800;

/// Un point d'exemple et le caractère de son climat.
struct Modele {
    nom: &'static str,
    categorie: &'static str,
    taille: Option<&'static str>,
    lat: f64,
    lon: f64,
    altitude: f64,
    /// Décalage moyen par rapport au site de référence (°C).
    decalage: f64,
    /// Demi-amplitude du cycle jour/nuit (°C).
    amplitude: f64,
    /// Refroidissement nocturne supplémentaire par nuit claire (°C).
    inversion: f64,
    notes: &'static str,
}

const MODELES: [Modele; 3] = [
    Modele {
        nom: "Exemple — Fond de combe",
        categorie: "Cat3",
        taille: Some("Gr"),
        lat: 44.9812,
        lon: 5.4213,
        altitude: 1150.0,
        decalage: -1.5,
        amplitude: 7.0,
        inversion: 5.0,
        notes: "Point d'exemple : données synthétiques.",
    },
    Modele {
        nom: "Exemple — Versant sud",
        categorie: "Cat1",
        taille: None,
        lat: 44.9905,
        lon: 5.4302,
        altitude: 1240.0,
        decalage: 1.0,
        amplitude: 5.5,
        inversion: 0.5,
        notes: "Point d'exemple : données synthétiques.",
    },
    Modele {
        nom: "Exemple — Crête",
        categorie: "Cat2",
        taille: Some("Gr"),
        lat: 44.9971,
        lon: 5.4120,
        altitude: 1480.0,
        decalage: -2.0,
        amplitude: 3.5,
        inversion: 0.0,
        notes: "Point d'exemple : données synthétiques.",
    },
];

/// Générateur congruentiel : bruit reproductible dans [-1, 1].
struct Bruit(u64);
impl Bruit {
    fn suivant(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

/// Mesures synthétiques d'un point, de `debut` inclus à `debut + JOURS` exclu.
fn mesures(series_id: &str, m: &Modele, debut: DateTime<Utc>, graine: u64) -> Vec<Measurement> {
    let mut bruit = Bruit(graine);
    let n = JOURS * 86_400 / PAS_SECONDES;
    let mut sortie = Vec::with_capacity(n as usize);
    for i in 0..n {
        let t = debut + Duration::seconds(i * PAS_SECONDES);
        let jour = (i * PAS_SECONDES) as f64 / 86_400.0;
        let heure = t.hour() as f64 + t.minute() as f64 / 60.0;
        // Maximum vers 14 h UTC, minimum en fin de nuit.
        let cycle = (2.0 * std::f64::consts::PI * (heure - 14.0) / 24.0).cos();
        // Épisodes de quelques jours (redoux, vague de froid) et nébulosité.
        let episode = 4.0 * (2.0 * std::f64::consts::PI * jour / 9.0).sin()
            + 2.0 * (2.0 * std::f64::consts::PI * jour / 3.7).sin();
        let ciel_clair = 0.5 + 0.5 * (2.0 * std::f64::consts::PI * jour / 5.3).cos();
        let nuit = if cycle < 0.0 { -cycle } else { 0.0 };
        let temperature = 2.0 + m.decalage + episode + m.amplitude * cycle
            - m.inversion * ciel_clair * nuit
            + 0.4 * bruit.suivant();
        let humidite = (82.0 - 3.0 * (m.amplitude * cycle) + 6.0 * (1.0 - ciel_clair) + 3.0 * bruit.suivant())
            .clamp(25.0, 100.0);
        let mut mesure = Measurement::new_utc(series_id, t, (temperature * 10.0).round() / 10.0);
        mesure.humidity_percent = Some(humidite.round());
        sortie.push(mesure);
    }
    sortie
}

fn serie(point_id: &str) -> LoggerSeries {
    LoggerSeries {
        id: uuid::Uuid::new_v4().to_string(),
        point_id: point_id.to_string(),
        source_type: SourceType::Manual,
        role: SeriesRole::Primary,
        logger_serial: None,
        imported_at: now_utc(),
        raw_file_reference: Some("exemple-synthetique".into()),
        clock_offset_seconds: 0,
        raw_timestamp_was_utc: true,
        label: Some("Données d'exemple (synthétiques)".into()),
        sample_interval_seconds: Some(PAS_SECONDES),
        data_license: None,
        station_id: None,
        last_api_fetch_at: None,
        date_order: DateOrder::DayFirst,
        date_format_uncertain: false,
        content_fingerprint: None,
        measurement_count: 0,
    }
}

/// Le projet est-il un projet d'exemple ?
pub fn est_exemple(db: &Db, project_id: &str) -> bool {
    matches!(db.get_setting(&format!("{SETTING_DEMO_PREFIX}{project_id}")).ok().flatten().as_deref(), Some("1"))
}

/// Crée le projet d'exemple et ses données ; renvoie le projet relu en base.
pub fn creer_projet_exemple(db: &Db) -> AppResult<Project> {
    let projet = db.create_project(NOM_PROJET)?;
    // Marqué avant d'y écrire quoi que ce soit : une synchro lancée entre-temps
    // ne doit jamais le voir non marqué.
    db.set_setting(&format!("{SETTING_DEMO_PREFIX}{}", projet.id), "1")?;

    let fin = now_utc().with_minute(0).and_then(|t| t.with_second(0)).unwrap_or_else(now_utc);
    let debut = fin - Duration::days(JOURS);

    let resultat = (|| -> AppResult<()> {
        for (k, m) in MODELES.iter().enumerate() {
            let point = Point::from_new(NewPoint {
                project_id: projet.id.clone(),
                name: m.nom.to_string(),
                latitude: m.lat,
                longitude: m.lon,
                altitude_m: Some(m.altitude),
                category: m.categorie.to_string(),
                size: m.taille.map(String::from),
                factor_sky: None,
                factor_wind: None,
                factor_slope: None,
                factor_snow_cover: None,
                factor_forest_cover: None,
                is_open_terrain: None,
                notes: Some(m.notes.to_string()),
                infoclimat_station_id: None,
                romma_station_id: None,
            })?;
            db.insert_point(&point)?;
            let s = serie(&point.id);
            let lot = mesures(&s.id, m, debut, 0x5eed + k as u64);
            db.insert_series_with_measurements(&s, &lot)?;
        }
        Ok(())
    })();
    if let Err(e) = resultat {
        // Pas de projet d'exemple à moitié rempli.
        let _ = db.delete_project(&projet.id);
        let _ = db.delete_setting(&format!("{SETTING_DEMO_PREFIX}{}", projet.id));
        return Err(e);
    }
    db.get_project(&projet.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_projet_d_exemple_est_complet_et_marque() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.list_projects().unwrap().is_empty(), "rien ne se crée tout seul");

        let p = creer_projet_exemple(&db).unwrap();
        assert!(p.is_demo);
        assert!(est_exemple(&db, &p.id));
        assert_eq!(p.point_count, 3);

        let points = db.list_points(&p.id).unwrap();
        let attendu = JOURS * 86_400 / PAS_SECONDES;
        for pt in &points {
            let series = db.list_series_for_point(&pt.id).unwrap();
            assert_eq!(series.len(), 1);
            assert_eq!(series[0].measurement_count, attendu);
            let m = db.measurements_for_series(&series[0].id).unwrap();
            assert!(m.iter().all(|x| (-30.0..40.0).contains(&x.temperature_celsius)));
            assert!(m.iter().all(|x| matches!(x.humidity_percent, Some(h) if (0.0..=100.0).contains(&h))));
            // Cycle jour/nuit : chaque jour montre un écart nettement positif.
            let jour: Vec<f64> = m[..48].iter().map(|x| x.temperature_celsius).collect();
            let ecart = jour.iter().cloned().fold(f64::MIN, f64::max) - jour.iter().cloned().fold(f64::MAX, f64::min);
            assert!(ecart > 4.0, "amplitude diurne trop faible : {ecart}");
        }
    }

    #[test]
    fn le_projet_d_exemple_n_est_jamais_synchronise() {
        let db = Db::open_in_memory().unwrap();
        let p = creer_projet_exemple(&db).unwrap();
        assert!(crate::cloud::projet_exclu(&db, &p.id, 0));
        // Même si un réglage « synchronisé » était posé à la main.
        db.set_setting(&format!("{}{}", crate::cloud::SETTING_CLOUD_EXCLUDE_PREFIX, p.id), "0").unwrap();
        assert!(crate::cloud::projet_exclu(&db, &p.id, 0));
        // Un projet ordinaire n'est pas marqué.
        let autre = db.create_project("Vercors").unwrap();
        assert!(!db.get_project(&autre.id).unwrap().is_demo);
        assert!(!crate::cloud::projet_exclu(&db, &autre.id, 0));
    }

    #[test]
    fn les_donnees_sont_reproductibles() {
        let debut: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let a = mesures("s", &MODELES[0], debut, 7);
        let b = mesures("s", &MODELES[0], debut, 7);
        assert_eq!(a, b);
    }
}

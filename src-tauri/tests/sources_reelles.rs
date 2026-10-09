//! Tests d'intégration sur les **trois fichiers réels** des sources ajoutées.
//!
//! | Source | Fichier de référence | Ce qu'il prouve |
//! |---|---|---|
//! | Elitech xlsx | `docs/samples/elitech_rc5_reel.xls` | le vrai format d'ElitechLog se lit de bout en bout |
//! | Infoclimat CSV | `export_infoclimat.csv` | le lecteur CSV converge avec le JSON |
//! | ROMMA | `archives_Lans - Les Allières_…csv` | 477 000 lignes, `NULL` littéraux, gros volume |
//!
//! ## Où les fichiers sont cherchés
//!
//! Deux emplacements, dans cet ordre :
//!
//! 1. `docs/samples/` — ce que le dépôt contient. Le classeur Elitech y est en
//!    entier (254 Ko) ; les deux CSV, qui pèsent 17 et 38 Mo, n'y sont que sous
//!    forme d'**extraits** de quelques dizaines de lignes ;
//! 2. `%USERPROFILE%\Downloads` — les fichiers complets, tels qu'ils ont été
//!    téléchargés. Présents sur le poste de développement, absents partout
//!    ailleurs.
//!
//! Les tests qui portent sur un fichier complet **s'abstiennent** quand il n'est
//! pas là, avec une ligne visible sous `cargo test -- --nocapture`. C'est la même
//! convention que `infoclimat_moucherolle.rs` : un test rouge parce qu'un fichier
//! de 38 Mo n'a pas été copié ne renseignerait sur rien. Les tests sur extraits,
//! eux, tournent partout et sans condition.

use std::path::PathBuf;

use gws_lib::anomaly::JumpThresholds;
use gws_lib::db::Db;
use gws_lib::elitech_xlsx::{looks_like_xlsx, parse_elitech_xlsx};
use gws_lib::infoclimat::{parse_infoclimat_auto, InfoclimatFormat};
use gws_lib::model::{
    now_utc, LoggerSeries, Measurement, NewPoint, Point, Project, SeriesRole, SourceType,
};
use gws_lib::parsing::DateOrder;
use gws_lib::romma_import::parse_romma_archives;
use gws_lib::stats::{period_stats, weather_coverage};

// ---------------------------------------------------------------------------
// Localisation des fichiers
// ---------------------------------------------------------------------------

fn racine() -> PathBuf {
    // `CARGO_MANIFEST_DIR` pointe sur `src-tauri/` ; le dépôt est un cran plus haut.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("le dossier parent de src-tauri existe")
        .to_path_buf()
}

/// Un extrait committé dans `docs/samples/` — toujours présent.
fn extrait(nom: &str) -> PathBuf {
    racine().join("docs").join("samples").join(nom)
}

/// Le fichier complet, s'il est sur ce poste. `None` sinon.
fn fichier_complet(nom: &str) -> Option<PathBuf> {
    let candidats = [
        extrait(nom),
        PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
            .join("Downloads")
            .join(nom),
    ];
    candidats.into_iter().find(|c| c.exists())
}

/// Charge un fichier complet, ou explique pourquoi le test s'abstient.
fn charger(nom: &str) -> Option<Vec<u8>> {
    match fichier_complet(nom) {
        Some(chemin) => Some(std::fs::read(&chemin).expect("lecture du fichier de référence")),
        None => {
            eprintln!(
                "[ignoré] « {nom} » introuvable (ni dans docs/samples/, ni dans \
                 %USERPROFILE%\\Downloads) — test d'intégration non joué. \
                 Ce n'est pas un échec : ce fichier de plusieurs dizaines de Mo est hors dépôt."
            );
            None
        }
    }
}

fn texte(nom: &str) -> Option<String> {
    charger(nom).map(|o| String::from_utf8_lossy(&o).into_owned())
}

/// Base en mémoire avec un projet et un point, prête à recevoir une série.
fn base_avec_point() -> (Db, Point) {
    let db = Db::open_in_memory().expect("base mémoire");
    let projet: Project = db.create_project("Vercors").expect("projet");
    let point = Point::from_new(NewPoint {
        project_id: projet.id,
        name: "Darbounouse".into(),
        latitude: 44.9812,
        longitude: 5.4213,
        altitude_m: Some(1150.0),
        category: "Cat3".into(),
        size: Some("Gr".into()),
        factor_sky: None,
        factor_wind: None,
        factor_slope: None,
        factor_snow_cover: None,
        factor_forest_cover: None,
        is_open_terrain: Some(false),
        notes: None,
        infoclimat_station_id: None,
        romma_station_id: Some("Lans - Les Allières".into()),
    })
    .expect("point valide");
    db.insert_point(&point).expect("insertion du point");
    (db, point)
}

// ===========================================================================
// A — Elitech : le classeur xlsx réel
// ===========================================================================

const XLSX: &str = "elitech_rc5_reel.xls";
/// Ce que la feuille de résumé annonce : `Points de données = 10307`.
const MESURES_ATTENDUES: usize = 10_307;

#[test]
fn le_xls_d_elitech_est_en_realite_un_xlsx() {
    let Some(octets) = charger(XLSX) else { return };

    assert!(
        looks_like_xlsx(&octets),
        "le fichier porte l'extension .xls mais doit commencer par la signature zip PK — \
         c'est toute la raison d'être du parseur xlsx"
    );
    // Ce n'est **pas** un fichier CFB (le vrai format .xls binaire), dont la
    // signature serait D0 CF 11 E0. Confondre les deux ferait échouer l'ouverture.
    assert_ne!(&octets[..4], &[0xD0, 0xCF, 0x11, 0xE0]);
}

#[test]
fn le_classeur_reel_livre_le_compte_annonce_par_son_resume() {
    let Some(octets) = charger(XLSX) else { return };
    let export = parse_elitech_xlsx(&octets).expect("le classeur réel doit se lire");

    assert_eq!(
        export.measurements.len(),
        MESURES_ATTENDUES,
        "la feuille de résumé annonce « Points de données : {MESURES_ATTENDUES} »"
    );
    assert!(
        export.warnings.is_empty(),
        "aucune ligne ne devrait être écartée : {:?}",
        export.warnings.first()
    );
    assert_eq!(export.sheet_names.len(), 3, "Résumé, Tableau de données, Graph");
    assert_eq!(export.data_sheet, "Tableau de données");
    assert_eq!(export.summary_sheet.as_deref(), Some("Résumé"));
    assert_eq!(export.header_labels, vec!["No.", "Temps", "Température°C"]);
}

#[test]
fn les_metadonnees_du_classeur_reel_sont_lues() {
    let Some(octets) = charger(XLSX) else { return };
    let m = parse_elitech_xlsx(&octets).unwrap().metadata;

    assert_eq!(m.logger_serial.as_deref(), Some("EFE205004333"));
    assert_eq!(m.device_model.as_deref(), Some("RC-5"));
    assert_eq!(m.sensor_type.as_deref(), Some("Température"));
    assert_eq!(
        m.sample_interval_seconds,
        Some(900),
        "« Intervalle d'enregistrement : 15m » doit donner 900 s"
    );
}

/// Les bornes et les valeurs, confrontées à ce que le résumé du fichier annonce
/// lui-même (`Premier/Dernier enregistrement`, `Maximum`, `Minimum`).
#[test]
fn les_bornes_du_classeur_reel_correspondent_a_son_resume() {
    let Some(octets) = charger(XLSX) else { return };
    let export = parse_elitech_xlsx(&octets).unwrap();

    let premier = export.measurements.first().unwrap();
    let dernier = export.measurements.last().unwrap();
    assert_eq!(premier.raw_timestamp.to_string(), "2026-05-01 14:07:31");
    assert_eq!(dernier.raw_timestamp.to_string(), "2026-08-16 22:37:31");

    // Le 01/05 doit rester le 1er mai. Si l'ordre jour/mois avait été appliqué à
    // tort, ce serait le 5 janvier — et la série couvrirait huit mois.
    assert_eq!(premier.raw_timestamp.date().to_string(), "2026-05-01");
    assert_eq!(premier.temperature_celsius, 22.2, "« 22,2 » avec virgule décimale");

    let temperatures: Vec<f64> = export
        .measurements
        .iter()
        .map(|m| m.temperature_celsius)
        .collect();
    let mini = temperatures.iter().cloned().fold(f64::INFINITY, f64::min);
    let maxi = temperatures.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    assert!((mini - (-9.9)).abs() < 1e-9, "le résumé annonce Minimum = -9,9 °C");
    assert!((maxi - 34.1).abs() < 1e-9, "le résumé annonce Maximum = 34,1 °C");

    // Aucune température n'est restée collée à sa virgule : un parsing naïf
    // aurait produit des entiers (22 au lieu de 22,2) ou des NaN.
    assert!(temperatures.iter().all(|t| t.is_finite()));
    assert!(
        temperatures.iter().any(|t| t.fract().abs() > 1e-9),
        "au moins une température doit avoir une décimale"
    );
}

/// Le scanner d'ordre jour/mois est court-circuité sur ce format.
#[test]
fn aucune_serie_issue_du_classeur_reel_n_est_marquee_a_verifier() {
    let Some(octets) = charger(XLSX) else { return };
    let e = parse_elitech_xlsx(&octets).unwrap().date_order;

    assert_eq!(e.iso_hits, MESURES_ATTENDUES);
    assert_eq!(e.ambiguous_hits, 0);
    assert_eq!(e.day_first_hits, 0);
    assert_eq!(e.month_first_hits, 0);
    assert!(!e.uncertain, "un horodatage ISO ne laisse aucun doute");
    assert!(!e.conflicting);
}

/// **Le cas qui a motivé la détection d'anomalie** : +3,8 °C en 15 minutes dans
/// la dernière heure de la série, cohérent avec le retrait du logger.
#[test]
fn le_saut_de_fin_de_serie_du_fichier_reel_est_signale_sans_perte() {
    let Some(octets) = charger(XLSX) else { return };
    let export = parse_elitech_xlsx(&octets).unwrap();

    assert_eq!(
        export.measurements.len(),
        MESURES_ATTENDUES,
        "la détection ne doit écarter aucune mesure"
    );
    assert!(
        !export.anomalies.is_empty(),
        "le saut de fin de série doit être signalé"
    );

    let dernier_instant = export.measurements.last().unwrap().raw_timestamp;
    let saut = export
        .anomalies
        .last()
        .expect("au moins un saut signalé");
    let minutes_avant_la_fin = (dernier_instant - saut.to_timestamp).num_minutes();
    assert!(
        minutes_avant_la_fin <= 60,
        "le saut attendu est dans la dernière heure ; celui-ci est à {minutes_avant_la_fin} min de la fin"
    );
    assert!(
        saut.delta_c.abs() > 3.0 && saut.gap_seconds <= 20 * 60,
        "seuils par défaut : plus de 3 °C en moins de 20 min (ici {} °C en {} s)",
        saut.delta_c,
        saut.gap_seconds
    );
    assert!(saut.message.contains("logger"));

    // Un seuil très permissif ne doit plus rien trouver : c'est la preuve que la
    // détection dépend bien des seuils et n'est pas déclenchée au hasard.
    let permissif = JumpThresholds {
        min_delta_c: 50.0,
        max_gap_seconds: 20 * 60,
    };
    let calme = gws_lib::elitech_xlsx::parse_elitech_xlsx_with(&octets, &permissif).unwrap();
    assert!(calme.anomalies.is_empty());
}

/// Import complet en base, jusqu'aux statistiques relues.
#[test]
fn le_classeur_reel_s_importe_en_base_et_se_relit() {
    let Some(octets) = charger(XLSX) else { return };
    let export = parse_elitech_xlsx(&octets).unwrap();
    let (db, point) = base_avec_point();

    // Le RC-5 était réglé en UTC+2 : il faut retrancher deux heures.
    let offset = -7200;
    let serie = LoggerSeries {
        id: "serie-rc5".into(),
        point_id: point.id.clone(),
        source_type: SourceType::ElitechImport,
        role: SeriesRole::Primary,
        logger_serial: export.metadata.logger_serial.clone(),
        imported_at: now_utc(),
        raw_file_reference: Some(XLSX.into()),
        clock_offset_seconds: offset,
        raw_timestamp_was_utc: false,
        label: None,
        sample_interval_seconds: export.metadata.sample_interval_seconds,
        data_license: None,
        station_id: None,
        last_api_fetch_at: None,
        date_order: DateOrder::DayFirst,
        date_format_uncertain: false,
        content_fingerprint: Some("empreinte-test".into()),
        measurement_count: 0,
    };

    let mesures: Vec<Measurement> = export
        .measurements
        .iter()
        .map(|r| {
            let brut = r.raw_timestamp.and_utc();
            Measurement::new(
                serie.id.clone(),
                gws_lib::model::apply_clock_offset(brut, offset),
                brut,
                r.temperature_celsius,
            )
        })
        .collect();

    let ecrites = db
        .insert_series_with_measurements(&serie, &mesures)
        .expect("insertion");
    assert_eq!(ecrites, MESURES_ATTENDUES, "aucun instant répété dans le fichier");

    let relues = db.measurements_for_series(&serie.id).unwrap();
    assert_eq!(relues.len(), MESURES_ATTENDUES);

    // L'invariant du §1.4 tient sur les 10 307 mesures relues.
    for m in &relues {
        assert_eq!(
            m.timestamp_utc.timestamp(),
            m.raw_timestamp.timestamp() + offset
        );
    }

    // Un logger de terrain ne publie aucune des grandeurs météo de la v5.
    let couverture = weather_coverage(&relues);
    assert_eq!(couverture.measurement_count, MESURES_ATTENDUES);
    assert!(!couverture.has_weather_fields());

    let stats = period_stats(&relues);
    assert_eq!(stats.measurement_count, MESURES_ATTENDUES);
    assert!((stats.extreme_min_c.unwrap() - (-9.9)).abs() < 1e-9);
    assert!((stats.extreme_max_c.unwrap() - 34.1).abs() < 1e-9);
    assert!(stats.day_count >= 107, "la série couvre 107 jours");
}

// ===========================================================================
// B — Infoclimat : l'export CSV réel
// ===========================================================================

const IC_COMPLET: &str = "export_infoclimat.csv";
const IC_EXTRAIT: &str = "infoclimat_export_extrait.csv";
const IC_STATION: &str = "STATIC0213";

/// Sur l'extrait committé — ce test tourne **partout**.
#[test]
fn l_extrait_csv_infoclimat_se_lit() {
    let contenu = std::fs::read_to_string(extrait(IC_EXTRAIT))
        .expect("docs/samples/infoclimat_export_extrait.csv doit être dans le dépôt");
    let p = parse_infoclimat_auto(&contenu, None).expect("extrait CSV lisible");

    assert_eq!(p.format, InfoclimatFormat::Csv);
    assert_eq!(p.station_id, IC_STATION);
    assert_eq!(p.records.len(), 120, "120 mesures dans l'extrait");
    assert!(p.warnings.is_empty(), "{:?}", p.warnings.first());
    assert_eq!(p.duplicate_count, 0);
    assert_eq!(p.unreadable_complements, 0);
    assert_eq!(p.sample_interval_seconds, Some(600));

    let station = p.station.expect("les lignes « # » décrivent la station");
    assert_eq!(
        station.name.as_deref(),
        Some("La-Chapelle-en-Vercors - Combe de Darbounouse")
    );
    assert_eq!(station.license.as_deref(), Some("Licence Ouverte/Open License"));
}

/// Sur le fichier complet, quand il est là.
#[test]
fn le_csv_infoclimat_complet_se_lit_de_bout_en_bout() {
    let Some(contenu) = texte(IC_COMPLET) else { return };
    let p = parse_infoclimat_auto(&contenu, None).expect("le CSV réel doit se lire");

    assert_eq!(p.format, InfoclimatFormat::Csv);
    assert_eq!(p.station_id, IC_STATION);
    assert_eq!(p.available_stations, vec![IC_STATION.to_string()]);
    assert_eq!(
        p.records.len(),
        137_088,
        "le fichier contient 137 088 lignes de données"
    );
    assert!(
        p.warnings.is_empty(),
        "aucune ligne ne devrait être écartée : {:?}",
        p.warnings.first()
    );
    assert_eq!(p.duplicate_count, 0);
    assert_eq!(
        p.unreadable_complements, 0,
        "les 137 088 champs `complements` doivent se relire"
    );
    assert_eq!(p.sample_interval_seconds, Some(600));

    // `dh_utc` est de l'UTC vrai : les instants sont pris tels quels.
    assert_eq!(
        p.first_utc().unwrap().to_rfc3339(),
        "2023-10-14T09:00:00+00:00"
    );
    assert!(p.records.iter().all(|r| r.temperature_celsius.is_finite()));
}

/// **La preuve du travail de factorisation.** Une même mesure, présentée en JSON
/// et en CSV, doit ressortir strictement identique.
#[test]
fn une_meme_mesure_donne_le_meme_resultat_en_json_et_en_csv() {
    let csv = "\
# METADATA:\n\
# STATIC0213: La-Chapelle-en-Vercors - Combe de Darbounouse\n\
station_id;dh_utc;temperature;humidite;complements;source\n\
string;YYYY-MM-DD hh:mm:ss;°C;%;various;StatIC\n\
STATIC0213;2023-10-14 09:00:00;13;89;{\"rssi\":-109,\"lat\":44.9676,\"lon\":5.47711,\"batt\":36.9};liveobjects\n";

    let json = r#"{
        "status": "OK",
        "errors": [],
        "stations": [{ "id": "STATIC0213" }],
        "hourly": {
            "STATIC0213": [
                {
                    "dh_utc": "2023-10-14 09:00:00",
                    "temperature": "13",
                    "humidite": "89",
                    "complements": "{\"rssi\":-109,\"lat\":44.9676,\"lon\":5.47711,\"batt\":36.9}",
                    "source": "liveobjects"
                }
            ]
        }
    }"#;

    let depuis_csv = parse_infoclimat_auto(csv, None).unwrap();
    let depuis_json = parse_infoclimat_auto(json, None).unwrap();

    assert_eq!(depuis_csv.format, InfoclimatFormat::Csv);
    assert_eq!(depuis_json.format, InfoclimatFormat::Json);
    assert_eq!(
        depuis_csv.records, depuis_json.records,
        "les deux formats doivent converger vers la même structure interne"
    );
}

// ===========================================================================
// C — ROMMA : l'archive réelle
// ===========================================================================

const ROMMA_COMPLET: &str = "archives_Lans - Les Allières_31-05-2017_19-08-2026.csv";
const ROMMA_EXTRAIT: &str = "romma_archives_extrait.csv";

/// Sur l'extrait committé — ce test tourne **partout**.
#[test]
fn l_extrait_romma_se_lit_avec_ses_null() {
    let contenu = std::fs::read_to_string(extrait(ROMMA_EXTRAIT))
        .expect("docs/samples/romma_archives_extrait.csv doit être dans le dépôt");
    let p = parse_romma_archives(&contenu).expect("extrait ROMMA lisible");

    assert_eq!(p.columns.len(), 15);
    assert_eq!(
        p.units
            .iter()
            .find(|(c, _)| c == "Vmoy")
            .map(|(_, u)| u.as_str()),
        Some("km/h"),
        "l'unité du vent est lue dans le fichier, pas supposée"
    );
    assert_eq!(
        p.units
            .iter()
            .find(|(c, _)| c.trim() == "RadSolaireMax")
            .map(|(_, u)| u.as_str()),
        Some("W/m2")
    );

    // L'extrait contient un bloc de lignes sans température, ajouté exprès.
    assert!(p.skipped_count > 0, "les lignes `Temp = NULL` doivent être écartées");
    assert!(p
        .warnings
        .iter()
        .any(|w| w.reason.contains("température")));

    assert!(!p.records.is_empty());
    assert_eq!(p.sample_interval_seconds, Some(600));
    // La première mesure du fichier, en clair : 31-05-2017 19:00 locale.
    assert_eq!(
        p.records[0].timestamp_utc.to_rfc3339(),
        "2017-05-31T17:00:00+00:00"
    );
    assert_eq!(p.records[0].temperature_celsius, 12.2);
    assert_eq!(p.records[0].wind_direction_degrees, Some(225.0), "SO");
    assert_eq!(p.records[0].pressure_hpa, Some(1021.0));
    assert_eq!(p.records[0].solar_radiation, None, "RadSolaire et Max à NULL");
}

/// Sur l'archive complète : 477 000 lignes, neuf ans, gros volume.
#[test]
fn l_archive_romma_complete_se_lit_de_bout_en_bout() {
    let Some(contenu) = texte(ROMMA_COMPLET) else { return };
    let p = parse_romma_archives(&contenu).expect("l'archive réelle doit se lire");

    assert_eq!(
        p.total_data_lines, 476_912,
        "l'archive contient 476 912 lignes de données"
    );
    assert_eq!(
        p.skipped_count, 10_621,
        "10 621 lignes n'ont pas de température et sont écartées"
    );
    assert_eq!(p.records.len(), 476_912 - 10_621);
    assert_eq!(p.duplicate_count, 0, "aucun instant répété sur neuf ans");
    assert_eq!(p.sample_interval_seconds, Some(600));

    // Bornes : du 31 mai 2017 au 19 août 2026, en UTC.
    assert_eq!(
        p.first_utc().unwrap().to_rfc3339(),
        "2017-05-31T17:00:00+00:00"
    );
    assert!(p.last_utc().unwrap() > p.first_utc().unwrap());

    // `RadSolaire` est vide de bout en bout : le repli sur `RadSolaireMax` doit
    // être détecté et signalé, pas subi en silence.
    assert!(
        p.solar_from_max,
        "RadSolaire est à NULL sur les 476 912 lignes du fichier réel"
    );

    // Couverture des capteurs : certains ont été ajoutés en cours de route.
    let n = p.records.len();
    assert!(p.field_counts.humidity > n * 9 / 10, "l'humidité est presque toujours là");
    assert!(
        p.field_counts.pressure > 0 && p.field_counts.pressure < n,
        "la pression n'est renseignée que sur une partie de la période"
    );
    assert!(p.field_counts.solar > 0 && p.field_counts.solar < n);

    // Les unités annoncées sont bien celles documentées sur `Measurement`.
    let unite = |nom: &str| {
        p.units
            .iter()
            .find(|(c, _)| c.trim() == nom)
            .map(|(_, u)| u.clone())
    };
    assert_eq!(unite("Temp").as_deref(), Some("degreC"));
    assert_eq!(unite("Pression").as_deref(), Some("hPa"));
    assert_eq!(unite("Pluie").as_deref(), Some("mm"));
    assert_eq!(unite("Vmoy").as_deref(), Some("km/h"));
    assert_eq!(unite("Rafale").as_deref(), Some("km/h"));

    // Ordres de grandeur : la confirmation que les unités lues sont crédibles.
    let vents: Vec<f64> = p.records.iter().filter_map(|r| r.wind_avg_kmh).collect();
    let vent_max = vents.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    assert!(
        (0.0..=200.0).contains(&vent_max),
        "un vent moyen en km/h reste sous 200 ; obtenu {vent_max} — \
         une valeur en m/s serait bien plus basse, en nœuds bien plus haute"
    );

    let pressions: Vec<f64> = p.records.iter().filter_map(|r| r.pressure_hpa).collect();
    assert!(
        pressions.iter().all(|p| (850.0..=1100.0).contains(p)),
        "une pression en hPa à 1 000 m d'altitude tient dans [850, 1100]"
    );

    // Toutes les directions du fichier sont reconnues : aucun secteur inconnu
    // n'est passé au travers (ce serait le signe d'une rose anglaise).
    let avec_vent = p
        .records
        .iter()
        .filter(|r| r.wind_avg_kmh.is_some())
        .count();
    assert!(
        p.field_counts.wind_direction > avec_vent / 2,
        "la rose à 16 points française doit être reconnue sur l'essentiel des lignes"
    );
}

/// Import de l'archive complète en base : le chemin de gros volume.
#[test]
fn l_archive_romma_complete_s_importe_en_base() {
    let Some(contenu) = texte(ROMMA_COMPLET) else { return };
    let p = parse_romma_archives(&contenu).unwrap();
    let (db, point) = base_avec_point();

    let serie = LoggerSeries {
        id: "serie-romma".into(),
        point_id: point.id.clone(),
        source_type: SourceType::RommaImport,
        role: SeriesRole::Primary,
        logger_serial: None,
        imported_at: now_utc(),
        raw_file_reference: Some(ROMMA_COMPLET.into()),
        // Source nativement UTC : aucune correction d'horloge.
        clock_offset_seconds: 0,
        raw_timestamp_was_utc: true,
        label: None,
        sample_interval_seconds: p.sample_interval_seconds,
        data_license: None,
        station_id: point.romma_station_id.clone(),
        last_api_fetch_at: None,
        date_order: DateOrder::DayFirst,
        date_format_uncertain: false,
        content_fingerprint: Some("empreinte-romma".into()),
        measurement_count: 0,
    };

    let mesures: Vec<Measurement> = p
        .records
        .iter()
        .map(|r| Measurement {
            humidity_percent: r.humidity_percent,
            pressure_hpa: r.pressure_hpa,
            wind_avg_kmh: r.wind_avg_kmh,
            wind_gust_kmh: r.wind_gust_kmh,
            wind_direction_degrees: r.wind_direction_degrees,
            rain_mm: r.rain_mm,
            solar_radiation: r.solar_radiation,
            ..Measurement::new_utc(serie.id.clone(), r.timestamp_utc, r.temperature_celsius)
        })
        .collect();

    let ecrites = db
        .insert_series_with_measurements(&serie, &mesures)
        .expect("l'insertion en lots doit tenir 466 000 mesures");
    assert_eq!(ecrites, mesures.len());

    let relues = db.measurements_for_series(&serie.id).unwrap();
    assert_eq!(relues.len(), mesures.len());

    // Les six colonnes de la v5 font l'aller-retour en base sans perte.
    let couverture = weather_coverage(&relues);
    assert!(couverture.has_weather_fields());
    assert_eq!(couverture.pressure, p.field_counts.pressure);
    assert_eq!(couverture.wind_avg, p.field_counts.wind_avg);
    assert_eq!(couverture.wind_gust, p.field_counts.wind_gust);
    assert_eq!(couverture.wind_direction, p.field_counts.wind_direction);
    assert_eq!(couverture.rain, p.field_counts.rain);
    assert_eq!(couverture.solar_radiation, p.field_counts.solar);

    // Source nativement UTC : brut et corrigé coïncident partout.
    assert!(relues
        .iter()
        .all(|m| m.timestamp_utc == m.raw_timestamp));

    let stats = period_stats(&relues);
    assert!(stats.day_count > 3_000, "neuf ans de mesures");
    assert!(stats.extreme_min_c.unwrap() < -10.0);
}

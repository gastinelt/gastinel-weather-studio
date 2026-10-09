//! Test d'intégration : la chaîne complète « fichier sur le disque → base → statistiques ».
//!
//! Les tests unitaires des modules travaillent sur des chaînes écrites en dur.
//! Celui-ci part des **vrais fichiers** de `docs/samples/`, ce qui garantit deux
//! choses que les tests unitaires ne peuvent pas garantir :
//!
//! 1. les fichiers d'exemple restent cohérents avec le parseur (si quelqu'un
//!    modifie un échantillon sans toucher au code, ce test le signale) ;
//! 2. l'enchaînement parseur → correction d'horloge → insertion → relecture →
//!    `stats.rs` fonctionne réellement de bout en bout, y compris la conversion
//!    en UTC, qui est la contrainte n°1 du projet.
//!
//! ⚠️ `elitech_hypothese.txt` est une **hypothèse de format**, pas un export réel
//! du RC-5. Quand le vrai fichier arrivera, c'est ici qu'il faudra le brancher.

use chrono::{DateTime, NaiveDate, Utc};

use gws_lib::db::Db;
use gws_lib::elitech_import::{parse_elitech_export_detailed, RawMeasurement};
use gws_lib::manual_import::{parse_manual, ManualMapping};
use gws_lib::model::{
    apply_clock_offset, now_utc, LoggerSeries, Measurement, NewPoint, Point, SeriesRole, SourceType,
};
use gws_lib::parsing::DateOrder;
use gws_lib::stats::period_stats;

/// Les tests s'exécutent depuis la racine du paquet (`src-tauri/`).
fn sample(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/samples")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("échantillon introuvable : {} ({e})", path.display()))
}

fn utc(iso: &str) -> DateTime<Utc> {
    iso.parse().expect("horodatage de test invalide")
}

fn jour(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// Reproduit ce que fait `commands::persist_import` : horodatage brut réinterprété
/// en UTC sans conversion, puis application du décalage d'horloge.
fn en_mesures(series_id: &str, raws: &[RawMeasurement], offset: i64) -> Vec<Measurement> {
    raws.iter()
        .map(|r| {
            let raw_utc = r.raw_timestamp.and_utc();
            Measurement {
                humidity_percent: r.humidity_percent,
                ..Measurement::new(
                    series_id,
                    apply_clock_offset(raw_utc, offset),
                    raw_utc,
                    r.temperature_celsius,
                )
            }
        })
        .collect()
}

fn point_de_test(db: &Db, nom: &str) -> Point {
    let projet = db.create_project("Vercors").unwrap();
    let point = Point::from_new(NewPoint {
        project_id: projet.id,
        name: nom.to_string(),
        latitude: 44.9812,
        longitude: 5.4213,
        altitude_m: Some(1150.0),
        category: "Cat3".into(), // dépression fermée : la taille est obligatoire
        size: Some("Gr".into()),
        factor_sky: None,
        factor_wind: None,
        factor_slope: None,
        factor_snow_cover: None,
        factor_forest_cover: None,
        is_open_terrain: Some(false),
        notes: None,
        infoclimat_station_id: None,
        romma_station_id: None,
    })
    .unwrap();
    db.insert_point(&point).unwrap();
    point
}

fn serie(point_id: &str, source: SourceType, role: SeriesRole, offset: i64) -> LoggerSeries {
    LoggerSeries {
        id: uuid_simple(),
        point_id: point_id.to_string(),
        source_type: source,
        role,
        logger_serial: None,
        imported_at: now_utc(),
        raw_file_reference: None,
        clock_offset_seconds: offset,
        raw_timestamp_was_utc: false,
        label: None,
        sample_interval_seconds: None,
        data_license: None,
        station_id: None,
        last_api_fetch_at: None,
        date_order: DateOrder::DayFirst,
        date_format_uncertain: false,
        content_fingerprint: None,
        measurement_count: 0,
    }
}

/// Le test n'a pas besoin d'un vrai UUID, seulement d'un identifiant unique.
fn uuid_simple() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!("serie-test-{}", N.fetch_add(1, Ordering::Relaxed))
}

#[test]
fn export_elitech_hypothetique_jusquaux_statistiques() {
    let contenu = sample("elitech_hypothese.txt");
    let export = parse_elitech_export_detailed(&contenu).expect("le parseur doit accepter le fichier");

    // --- Métadonnées lues dans le bloc d'en-tête.
    assert_eq!(
        export.metadata.logger_serial.as_deref(),
        Some("RC5000123"),
        "le numéro de série doit gagner contre « Device: RC-5 »"
    );
    assert_eq!(export.metadata.sample_interval_seconds, Some(600));

    // --- Structure détectée.
    assert_eq!(export.layout.delimiter, "TAB");
    assert!(
        !export.layout.positional_fallback,
        "les intitulés de colonnes doivent suffire, sans repli positionnel"
    );
    assert!(export.layout.humidity_column.is_some());

    // --- 18 lignes de données, dont une température « -- » : 17 retenues, 1 écartée.
    assert_eq!(export.measurements.len(), 17);
    assert_eq!(export.warnings.len(), 1);
    let avertissement = &export.warnings[0];
    assert!(
        avertissement.reason.contains("température illisible"),
        "raison inattendue : {}",
        avertissement.reason
    );
    assert!(
        export.measurements.iter().all(|m| m.humidity_percent.is_some()),
        "l'humidité doit être lue quand la colonne est présente"
    );

    // --- Écriture en base, logger réglé en heure locale UTC+1 → offset -3600 s.
    let db = Db::open_in_memory().unwrap();
    let point = point_de_test(&db, "Darbounouse");
    let s = serie(
        &point.id,
        SourceType::ElitechImport,
        SeriesRole::Primary,
        -3600,
    );
    let mesures = en_mesures(&s.id, &export.measurements, -3600);
    db.insert_series_with_measurements(&s, &mesures).unwrap();

    // --- Relecture : l'invariant du §1.4 doit tenir après un aller-retour en base.
    let relues = db.measurements_for_series(&s.id).unwrap();
    assert_eq!(relues.len(), 17);
    for m in &relues {
        assert_eq!(
            m.timestamp_utc.timestamp() - m.raw_timestamp.timestamp(),
            -3600,
            "timestamp_utc doit toujours valoir raw_timestamp + clock_offset_seconds"
        );
    }
    // Première ligne : 06:00 dans le fichier (heure locale) → 05:00 UTC.
    assert_eq!(relues[0].raw_timestamp, utc("2024-01-05T06:00:00Z"));
    assert_eq!(relues[0].timestamp_utc, utc("2024-01-05T05:00:00Z"));

    // --- Statistiques, valeurs vérifiables à la main dans le fichier.
    let stats = period_stats(&relues);
    assert_eq!(stats.measurement_count, 17);
    assert_eq!(stats.day_count, 2);
    assert_eq!(stats.extreme_min_c, Some(-30.2));
    assert_eq!(stats.extreme_max_c, Some(-1.4));
    assert_eq!(stats.extreme_min_date, Some(jour(2024, 1, 6)));

    // 5 janvier : min -25,1 / max -2,1 → amplitude 23,0
    // 6 janvier : min -30,2 / max -1,4 → amplitude 28,8
    assert_eq!(stats.daily[0].date, jour(2024, 1, 5));
    assert_eq!(stats.daily[0].min_c, -25.1);
    assert_eq!(stats.daily[0].max_c, -2.1);
    assert_eq!(stats.daily[1].date, jour(2024, 1, 6));
    assert_eq!(stats.daily[1].min_c, -30.2);

    let moyenne_des_minima = stats.mean_daily_min_c.unwrap();
    assert!(
        (moyenne_des_minima - (-27.65)).abs() < 1e-9,
        "moyenne des minimums quotidiens attendue -27,65, obtenue {moyenne_des_minima}"
    );
    let amplitude_max = stats.max_amplitude_c.unwrap();
    assert!(
        (amplitude_max - 28.8).abs() < 1e-9,
        "amplitude maximale attendue 28,8, obtenue {amplitude_max}"
    );
    assert_eq!(stats.max_amplitude_date, Some(jour(2024, 1, 6)));
}

#[test]
fn csv_generique_par_le_mode_manuel() {
    let contenu = sample("generique_manuel.csv");

    // L'utilisateur décrit lui-même la structure : deux lignes de métadonnées à
    // ignorer, puis une ligne de titres, puis date / heure / température / humidité.
    let mapping = ManualMapping {
        delimiter: ";".to_string(),
        skip_lines: 2,
        has_header: true,
        datetime_column: 0,
        time_column: Some(1),
        temperature_column: 2,
        humidity_column: Some(3),
    };

    let parsed = parse_manual(&contenu, &mapping).expect("le mappage manuel doit être accepté");
    assert_eq!(parsed.measurements.len(), 13);
    assert!(
        parsed.warnings.is_empty(),
        "aucune ligne ne devrait être écartée : {:?}",
        parsed.warnings
    );

    // Dates « 05/01/2024 » : ambiguës, donc interprétées jour d'abord (hypothèse
    // européenne, cf. parsing::AMBIGUOUS_DATE_IS_DAY_FIRST). Températures en
    // virgule décimale (« -8,2 »), format tableur français.
    assert_eq!(
        parsed.measurements[0].raw_timestamp.and_utc(),
        utc("2024-01-05T06:00:00Z")
    );
    assert_eq!(parsed.measurements[0].temperature_celsius, -8.2);
    assert_eq!(parsed.measurements[0].humidity_percent, Some(86.0));

    // --- Ce fichier est le logger de secours : rôle Backup, aucune fusion avec
    //     le principal, et cette fois le logger était bien réglé en UTC (offset 0).
    let db = Db::open_in_memory().unwrap();
    let point = point_de_test(&db, "Darbounouse");
    let s = serie(&point.id, SourceType::Manual, SeriesRole::Backup, 0);
    let mesures = en_mesures(&s.id, &parsed.measurements, 0);
    db.insert_series_with_measurements(&s, &mesures).unwrap();

    let relues = db.measurements_for_series(&s.id).unwrap();
    assert_eq!(relues.len(), 13);
    for m in &relues {
        assert_eq!(
            m.timestamp_utc, m.raw_timestamp,
            "offset nul : le corrigé doit coïncider avec le brut"
        );
    }

    let stats = period_stats(&relues);
    assert_eq!(stats.day_count, 2);
    assert_eq!(stats.extreme_min_c, Some(-30.2));
    assert_eq!(stats.extreme_min_date, Some(jour(2024, 1, 6)));
}

/// Les deux échantillons décrivent le même épisode de froid, relevé par deux
/// loggers différents. Ils doivent produire **deux séries distinctes** sur le même
/// point — jamais une série fusionnée (§1.3).
#[test]
fn principal_et_secours_restent_deux_series() {
    let db = Db::open_in_memory().unwrap();
    let point = point_de_test(&db, "Darbounouse");

    let elitech = parse_elitech_export_detailed(&sample("elitech_hypothese.txt")).unwrap();
    let principal = serie(
        &point.id,
        SourceType::ElitechImport,
        SeriesRole::Primary,
        -3600,
    );
    let mesures = en_mesures(&principal.id, &elitech.measurements, -3600);
    db.insert_series_with_measurements(&principal, &mesures)
        .unwrap();

    let manuel = parse_manual(
        &sample("generique_manuel.csv"),
        &ManualMapping {
            delimiter: ";".to_string(),
            skip_lines: 2,
            has_header: true,
            datetime_column: 0,
            time_column: Some(1),
            temperature_column: 2,
            humidity_column: Some(3),
        },
    )
    .unwrap();
    let secours = serie(&point.id, SourceType::Manual, SeriesRole::Backup, 0);
    let mesures = en_mesures(&secours.id, &manuel.measurements, 0);
    db.insert_series_with_measurements(&secours, &mesures)
        .unwrap();

    let series = db.list_series_for_point(&point.id).unwrap();
    assert_eq!(series.len(), 2, "deux loggers = deux séries, jamais une fusion");

    let roles: Vec<SeriesRole> = series.iter().map(|s| s.role).collect();
    assert!(roles.contains(&SeriesRole::Primary));
    assert!(roles.contains(&SeriesRole::Backup));

    assert_eq!(db.measurements_for_series(&principal.id).unwrap().len(), 17);
    assert_eq!(db.measurements_for_series(&secours.id).unwrap().len(), 13);

    // Le point porte bien les 30 mesures, mais réparties en deux séries : les
    // agréger d'un bloc double-compterait l'épisode (d'où l'avertissement porté
    // par `Db::measurements_for_point`).
    assert_eq!(db.measurements_for_point(&point.id).unwrap().len(), 30);
    assert_eq!(db.list_points(&point.project_id).unwrap()[0].series_count, 2);
}

// ---------------------------------------------------------------------------
// Ordre jour/mois — de la détection à la correction a posteriori
// ---------------------------------------------------------------------------

/// Mappage des trois fixtures CSV « date ; heure ; température ; humidité ».
fn mappage_csv() -> ManualMapping {
    ManualMapping {
        delimiter: ";".to_string(),
        skip_lines: 2,
        has_header: true,
        datetime_column: 0,
        time_column: Some(1),
        temperature_column: 2,
        humidity_column: Some(3),
    }
}

/// `dates_jour_certain.csv` contient `13/01/2024` : le fichier prouve son ordre,
/// aucun drapeau ne doit être levé.
#[test]
fn import_avec_jour_certain_ne_leve_pas_de_drapeau() {
    let parsed = parse_manual(&sample("dates_jour_certain.csv"), &mappage_csv()).unwrap();

    assert_eq!(parsed.measurements.len(), 8);
    assert!(
        !parsed.date_order.uncertain,
        "le 13/01 prouve l'ordre : rien à confirmer"
    );
    assert!(parsed.date_order.prouve());
    assert_eq!(parsed.date_order.order, DateOrder::DayFirst);

    // Première mesure : 13 janvier, pas une date fantaisiste.
    assert_eq!(
        parsed.measurements[0].raw_timestamp.and_utc(),
        utc("2024-01-13T06:00:00Z")
    );
    // La ligne ambiguë du 05/02 suit l'ordre prouvé par le reste du fichier.
    assert_eq!(
        parsed.measurements[6].raw_timestamp.and_utc(),
        utc("2024-02-05T06:00:00Z")
    );
}

/// Le scénario complet du §A : un fichier qui ne prouve rien est importé, marqué,
/// puis corrigé **sans réimport**.
#[test]
fn import_ambigu_puis_permutation_bout_en_bout() {
    let parsed = parse_manual(&sample("dates_ambigues.csv"), &mappage_csv()).unwrap();

    // --- 1. Le fichier ne prouve rien : le drapeau doit se lever.
    assert!(
        parsed.date_order.uncertain,
        "toutes les dates ont leurs deux champs ≤ 12 : impossible de trancher"
    );
    assert!(!parsed.date_order.prouve());
    assert_eq!(parsed.measurements.len(), 8, "on marque, on ne bloque pas");

    // --- 2. Import, en propageant le doute sur la série comme le fait `persist_import`.
    let db = Db::open_in_memory().unwrap();
    let point = point_de_test(&db, "Darbounouse");
    let mut s = serie(&point.id, SourceType::Manual, SeriesRole::Primary, -3600);
    s.date_order = parsed.date_order.order;
    s.date_format_uncertain = parsed.date_order.uncertain;

    let mesures = en_mesures(&s.id, &parsed.measurements, -3600);
    db.insert_series_with_measurements(&s, &mesures).unwrap();

    assert!(db.get_series(&s.id).unwrap().date_format_uncertain);
    // Lu « jour d'abord » : le 5 janvier.
    let avant = db.measurements_for_series(&s.id).unwrap();
    assert_eq!(avant[0].raw_timestamp, utc("2024-01-05T06:00:00Z"));
    assert_eq!(avant[0].timestamp_utc, utc("2024-01-05T05:00:00Z"));

    // --- 3. L'utilisateur découvre que le logger sortait du MM/dd : permutation.
    let n = db.swap_series_date_order(&s.id).unwrap();
    assert_eq!(n, 8);

    let apres = db.measurements_for_series(&s.id).unwrap();
    assert_eq!(apres.len(), 8);

    // 05/01 relu « mois d'abord » = 1er mai ; l'offset UTC+1 est préservé.
    assert_eq!(apres[0].raw_timestamp, utc("2024-05-01T06:00:00Z"));
    assert_eq!(apres[0].timestamp_utc, utc("2024-05-01T05:00:00Z"));
    for m in &apres {
        assert_eq!(
            m.timestamp_utc.timestamp() - m.raw_timestamp.timestamp(),
            -3600,
            "l'invariant du §1.4 doit survivre à la permutation"
        );
    }

    // --- 4. La série n'est plus douteuse et l'ordre a basculé.
    let relue = db.get_series(&s.id).unwrap();
    assert!(!relue.date_format_uncertain);
    assert_eq!(relue.date_order, DateOrder::MonthFirst);

    // --- 5. Les statistiques suivent : la période a changé de saison.
    let stats = period_stats(&apres);
    assert_eq!(stats.first_utc, Some(utc("2024-05-01T05:00:00Z")));
    assert_eq!(
        stats.last_utc,
        Some(utc("2024-07-02T06:00:00Z")),
        "07/02 permuté donne le 2 juillet"
    );
    // Les températures, elles, n'ont pas bougé.
    assert_eq!(stats.extreme_min_c, Some(-30.2));
}

/// Garde-fou : `generique_manuel.csv` est entièrement ambigu. C'est attendu —
/// ce test existe pour que personne ne s'en étonne ni ne « corrige » le fixture.
#[test]
fn le_csv_generique_est_ambigu_et_doit_le_rester() {
    let parsed = parse_manual(&sample("generique_manuel.csv"), &mappage_csv()).unwrap();
    assert!(
        parsed.date_order.uncertain,
        "toutes ses dates sont en 05/01 et 06/01 : aucune preuve possible"
    );
}

//! Test d'intégration sur l'**export réel** de la Crête de la Grande Moucherolle.
//!
//! `Clee.API/Moucherolle.json` : 9,78 Mo, station `STATIC0479`, 55 jours à la
//! cadence de 10 minutes, récupéré le 19 août 2026. C'est le pendant, pour l'API
//! Infoclimat, de ce que `docs/samples/` est pour les fichiers Elitech : la seule
//! preuve que le parseur travaille sur la réalité et pas sur une reconstruction.
//!
//! ## Le fichier est absent du dépôt — et le test le dit
//!
//! `Clee.API/` est ignoré par git (le dossier contient aussi les clés d'API). Sur
//! un poste où le fichier n'a pas été copié, ces tests **s'abstiennent** au lieu
//! d'échouer : un test rouge parce qu'un fichier de 10 Mo n'est pas là ne
//! renseignerait sur rien. Ils écrivent alors une ligne visible avec
//! `cargo test -- --nocapture`.

use std::path::PathBuf;

use gws_lib::db::Db;
use gws_lib::infoclimat::{parse_infoclimat, InfoclimatPayload};
use gws_lib::model::{now_utc, LoggerSeries, Measurement, NewPoint, Point, SeriesRole, SourceType};
use gws_lib::parsing::DateOrder;
use gws_lib::stats::period_stats;

/// Nombre d'enregistrements attendus dans le fichier de référence.
const MESURES_ATTENDUES: usize = 7905;
const STATION: &str = "STATIC0479";

fn fichier_reference() -> Option<PathBuf> {
    // `CARGO_MANIFEST_DIR` pointe sur `src-tauri/` ; le dossier est un cran plus haut.
    let chemin = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("Clee.API")
        .join("Moucherolle.json");
    chemin.exists().then_some(chemin)
}

/// Charge le fichier, ou renvoie `None` en expliquant pourquoi le test s'abstient.
fn charger() -> Option<InfoclimatPayload> {
    let Some(chemin) = fichier_reference() else {
        eprintln!(
            "[ignoré] Clee.API/Moucherolle.json absent — test d'intégration Infoclimat non joué. \
             Ce n'est pas un échec : le dossier Clee.API/ est hors dépôt."
        );
        return None;
    };
    let contenu = std::fs::read_to_string(&chemin).expect("lecture du fichier de référence");
    Some(parse_infoclimat(&contenu, Some(STATION)).expect("parsing du fichier de référence"))
}

/// La preuve principale : le compte y est, sans doublon et sans ligne écartée.
#[test]
fn le_fichier_reel_donne_le_compte_attendu() {
    let Some(p) = charger() else { return };

    assert_eq!(
        p.records.len(),
        MESURES_ATTENDUES,
        "le fichier de référence contient {MESURES_ATTENDUES} enregistrements"
    );
    assert_eq!(p.station_id, STATION);
    assert_eq!(
        p.warnings.len(),
        0,
        "aucune ligne ne devrait être écartée : {:?}",
        p.warnings.first()
    );
    assert_eq!(p.duplicate_count, 0, "les 7905 instants sont distincts");
    assert_eq!(
        p.unreadable_complements, 0,
        "les 7905 champs `complements` doivent se relire"
    );
    assert_eq!(
        p.available_stations,
        vec![STATION.to_string()],
        "`_params` ne doit pas être compté comme une station"
    );
}

/// Les deux colonnes techniques de la migration v3, sur données réelles.
#[test]
fn les_colonnes_techniques_sont_renseignees_partout() {
    let Some(p) = charger() else { return };

    let avec_rssi = p.records.iter().filter(|r| r.rssi_dbm.is_some()).count();
    let avec_batterie = p.records.iter().filter(|r| r.battery.is_some()).count();
    assert_eq!(avec_rssi, MESURES_ATTENDUES);
    assert_eq!(avec_batterie, MESURES_ATTENDUES);

    // Bornes constatées sur l'échantillon. Le RSSI est cohérent avec du LoRa.
    let rssi: Vec<f64> = p.records.iter().filter_map(|r| r.rssi_dbm).collect();
    let rssi_min = rssi.iter().cloned().fold(f64::INFINITY, f64::min);
    let rssi_max = rssi.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    assert_eq!(rssi_min, -117.0);
    assert_eq!(rssi_max, -73.0);

    // La batterie, elle, ne ressemble à rien de connu : de 6,48 à 72, avec le
    // gros du peuplement vers 40. C'est précisément pourquoi elle est stockée
    // brute et affichée sans unité (cf. `model::Measurement::battery`).
    let batt: Vec<f64> = p.records.iter().filter_map(|r| r.battery).collect();
    let batt_min = batt.iter().cloned().fold(f64::INFINITY, f64::min);
    let batt_max = batt.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    assert_eq!(batt_min, 6.48);
    assert_eq!(batt_max, 72.0);
    assert!(
        batt_max / batt_min > 10.0,
        "l'amplitude de `batt` interdit d'y lire une tension ou un pourcentage"
    );
}

/// Cadence, période, identité et licence de la station.
#[test]
fn cadence_periode_et_licence() {
    let Some(p) = charger() else { return };

    assert_eq!(
        p.sample_interval_seconds,
        Some(600),
        "la station publie toutes les 10 minutes"
    );

    let debut = p.first_utc().unwrap();
    let fin = p.last_utc().unwrap();
    assert_eq!(debut.to_rfc3339(), "2026-06-24T20:10:00+00:00");
    assert_eq!(fin.to_rfc3339(), "2026-08-18T21:40:00+00:00");
    assert!(debut < fin, "les mesures doivent sortir triées");

    let station = p.station.as_ref().expect("bloc stations[] présent");
    assert_eq!(station.id, STATION);
    assert_eq!(station.latitude, Some(45.009));
    assert_eq!(station.longitude, Some(5.558));
    assert_eq!(station.elevation_m, Some(1965.0));
    assert!(station.name.as_deref().unwrap().contains("Moucherolle"));

    // C'est cette mention qui déclenche l'attribution obligatoire (partie D).
    assert_eq!(
        station.license.as_deref(),
        Some("NON-COMMERCIAL ONLY: CC BY NC")
    );
}

/// Les valeurs sont bien désérialisées depuis des **chaînes**, pas des nombres.
#[test]
fn les_temperatures_sont_plausibles_et_l_humidite_optionnelle() {
    let Some(p) = charger() else { return };

    let temps: Vec<f64> = p.records.iter().map(|r| r.temperature_celsius).collect();
    let min = temps.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = temps.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    assert!(
        (-40.0..40.0).contains(&min) && (-40.0..40.0).contains(&max),
        "températures hors du plausible pour 1965 m en été : {min} à {max}"
    );
    // Fin juin → mi-août à 1965 m : la série doit voir des positives franches.
    assert!(max > 15.0, "maximum étonnamment bas : {max}");

    let avec_humidite = p.records.iter().filter(|r| r.humidity_percent.is_some()).count();
    assert!(avec_humidite > 0, "la station publie de l'humidité");
    for r in p.records.iter().filter_map(|r| r.humidity_percent) {
        assert!((0.0..=100.0).contains(&r), "humidité hors bornes : {r}");
    }

    assert_eq!(p.records[0].source.as_deref(), Some("liveobjects"));
}

/// Le vrai bout-en-bout : parser le fichier réel, l'écrire en base, le relire, et
/// **le rejouer** pour vérifier qu'aucun doublon n'apparaît.
#[test]
fn du_fichier_reel_jusqu_a_la_base_sans_doublon() {
    let Some(p) = charger() else { return };

    let db = Db::open_in_memory().expect("base mémoire");
    let projet = db.create_project("Vercors").unwrap();
    let point = Point::from_new(NewPoint {
        project_id: projet.id,
        name: "Crête de la Grande Moucherolle".into(),
        latitude: 45.009,
        longitude: 5.558,
        altitude_m: Some(1965.0),
        category: "Cat4".into(),
        size: Some("Gr".into()),
        factor_sky: None,
        factor_wind: None,
        factor_slope: None,
        factor_snow_cover: None,
        factor_forest_cover: None,
        is_open_terrain: Some(true),
        notes: None,
        infoclimat_station_id: Some(STATION.into()),
        romma_station_id: None,
    })
    .unwrap();
    db.insert_point(&point).unwrap();

    let serie_id = "serie-moucherolle".to_string();
    let mesures: Vec<Measurement> = p
        .records
        .iter()
        // `dh_utc` étant de l'UTC vrai, brut et corrigé coïncident : c'est
        // l'invariant du §1.4 dans son cas trivial, offset nul — ce que dit
        // `Measurement::new_utc`.
        .map(|r| Measurement {
            humidity_percent: r.humidity_percent,
            rssi_dbm: r.rssi_dbm,
            battery: r.battery,
            ..Measurement::new_utc(serie_id.clone(), r.timestamp_utc, r.temperature_celsius)
        })
        .collect();

    let serie = LoggerSeries {
        id: serie_id.clone(),
        point_id: point.id.clone(),
        source_type: SourceType::InfoclimatApi,
        role: SeriesRole::Primary,
        logger_serial: None,
        imported_at: now_utc(),
        raw_file_reference: Some("Moucherolle.json".into()),
        clock_offset_seconds: 0,
        raw_timestamp_was_utc: true,
        label: None,
        sample_interval_seconds: p.sample_interval_seconds,
        data_license: p.station.as_ref().and_then(|s| s.license.clone()),
        station_id: Some(STATION.into()),
        last_api_fetch_at: None,
        date_order: DateOrder::DayFirst,
        date_format_uncertain: false,
        content_fingerprint: None,
        measurement_count: 0,
    };

    let ecrites = db
        .insert_series_with_measurements(&serie, &mesures)
        .unwrap();
    assert_eq!(ecrites, MESURES_ATTENDUES, "tout doit être écrit du premier coup");

    let relues = db.measurements_for_series(&serie_id).unwrap();
    assert_eq!(relues.len(), MESURES_ATTENDUES);
    assert_eq!(db.get_series(&serie_id).unwrap().measurement_count as usize, MESURES_ATTENDUES);
    assert_eq!(
        db.get_series(&serie_id).unwrap().data_license.as_deref(),
        Some("NON-COMMERCIAL ONLY: CC BY NC"),
        "la licence doit survivre à l'aller-retour en base (partie D)"
    );

    // L'invariant du §1.4 tient sur les 7905 lignes.
    for m in &relues {
        assert_eq!(m.timestamp_utc, m.raw_timestamp);
    }
    assert_eq!(relues[0].rssi_dbm, p.records[0].rssi_dbm);
    assert_eq!(relues[0].battery, p.records[0].battery);

    // --- Le point du prompt : rejouer le MÊME fichier ne crée aucun doublon.
    let bilan = db.append_measurements(&serie_id, &mesures).unwrap();
    assert_eq!(bilan.inserted, 0, "aucune mesure nouvelle");
    assert_eq!(bilan.updated, 0, "aucune valeur n'a changé");
    assert_eq!(bilan.unchanged, MESURES_ATTENDUES);
    assert_eq!(
        db.measurements_for_series(&serie_id).unwrap().len(),
        MESURES_ATTENDUES,
        "la base doit contenir exactement le même nombre de mesures qu'avant"
    );

    // --- Et un rappel « à cheval » n'ajoute que ce qui manque.
    let mut recouvrement: Vec<Measurement> = mesures[MESURES_ATTENDUES - 10..].to_vec();
    let mut suivante = recouvrement.last().unwrap().clone();
    suivante.timestamp_utc += chrono::Duration::minutes(10);
    suivante.raw_timestamp = suivante.timestamp_utc;
    recouvrement.push(suivante);

    let bilan = db.append_measurements(&serie_id, &recouvrement).unwrap();
    assert_eq!(bilan.inserted, 1);
    assert_eq!(bilan.unchanged, 10);
    assert_eq!(
        db.measurements_for_series(&serie_id).unwrap().len(),
        MESURES_ATTENDUES + 1
    );

    // --- Les statistiques traversent le tout sans broncher.
    let stats = period_stats(&relues);
    assert_eq!(stats.measurement_count, MESURES_ATTENDUES);
    assert_eq!(stats.day_count, 56, "du 24 juin au 18 août inclus");
    assert!(stats.extreme_min_c.unwrap() < stats.extreme_max_c.unwrap());
    assert!(stats.mean_amplitude_c.unwrap() > 0.0);
}

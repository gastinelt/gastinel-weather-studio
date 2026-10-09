//! Import de la collecte Infoclimat (une réponse d'API brute par fichier) dans
//! un projet existant.
//!
//! Usage :
//!   cargo run --release --example import_collecte -- <base.sqlite3> <dossier collecte> <id du projet>
//!
//! Le dossier contient un sous-dossier par station (`<id>/<début>_<fin>.json.gz`).
//! Chaque station est rattachée au point du projet dont `infoclimat_station_id`
//! est le même. Même chemin que l'import d'un fichier Infoclimat dans l'appli :
//! `parse_infoclimat`, série `InfoclimatApi` complétée si elle existe déjà,
//! créée sinon. Une station est écrite en une transaction : un arrêt en cours
//! de route ne laisse pas de station à moitié importée, et une relance reprend
//! sans doublon (contrainte d'unicité série + instant).

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use gws_lib::db::Db;
use gws_lib::fingerprint::series_fingerprint_of;
use gws_lib::infoclimat::{parse_infoclimat, InfoclimatRecord};
use gws_lib::model::{now_utc, LoggerSeries, Measurement, SeriesRole, SourceType};
use gws_lib::parsing::DateOrder;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage : import_collecte <base.sqlite3> <dossier collecte> <id du projet>");
        std::process::exit(2);
    }
    let (base, dossier, projet) = (Path::new(&args[1]), Path::new(&args[2]), args[3].as_str());

    // Points du projet, par station Infoclimat.
    let points: HashMap<String, String> = {
        let c = rusqlite::Connection::open(base).expect("ouverture de la base");
        let mut st = c
            .prepare("SELECT infoclimat_station_id, id FROM points WHERE project_id = ?1 AND infoclimat_station_id IS NOT NULL")
            .unwrap();
        let lignes = st.query_map([projet], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        lignes.map(Result::unwrap).collect()
    };
    println!("{} points rattachés à une station dans le projet", points.len());

    let db = Db::open(base).expect("ouverture Db");
    let debut = Instant::now();
    let mut stations: Vec<_> = std::fs::read_dir(dossier).unwrap().filter_map(Result::ok).filter(|e| e.path().is_dir()).collect();
    stations.sort_by_key(|e| e.file_name());
    let (mut total, mut sans_point, mut erreurs) = (0usize, 0usize, 0usize);

    for (rang, entree) in stations.iter().enumerate() {
        let station = entree.file_name().to_string_lossy().to_string();
        let Some(point_id) = points.get(&station) else {
            sans_point += 1;
            continue;
        };

        let mut records: Vec<InfoclimatRecord> = Vec::new();
        let mut licence = None;
        let mut fichiers: Vec<_> = std::fs::read_dir(entree.path()).unwrap().filter_map(Result::ok).map(|e| e.path()).collect();
        fichiers.sort();
        for f in &fichiers {
            let mut texte = String::new();
            let lu = if f.extension().is_some_and(|x| x == "gz") {
                flate2::read::GzDecoder::new(std::fs::File::open(f).unwrap()).read_to_string(&mut texte)
            } else {
                std::fs::File::open(f).unwrap().read_to_string(&mut texte)
            };
            if lu.is_err() {
                erreurs += 1;
                continue;
            }
            match parse_infoclimat(&texte, Some(&station)) {
                Ok(p) => {
                    if licence.is_none() {
                        licence = p.station.as_ref().and_then(|s| s.license.clone());
                    }
                    records.extend(p.records);
                }
                // Fenêtre vide : la station n'apparaît pas dans la réponse.
                Err(_) => {}
            }
        }
        records.sort_by_key(|r| r.timestamp_utc);
        records.dedup_by_key(|r| r.timestamp_utc);
        if records.is_empty() {
            continue;
        }

        let existante = db.find_infoclimat_series(point_id, &station).unwrap();
        let series_id = existante.as_ref().map(|s| s.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let sid: Arc<str> = series_id.as_str().into();
        let mesures = || {
            records.iter().map(|r| Measurement {
                humidity_percent: r.humidity_percent,
                rssi_dbm: r.rssi_dbm,
                battery: r.battery,
                ..Measurement::new_utc(sid.clone(), r.timestamp_utc, r.temperature_celsius)
            })
        };

        let ecrites = match existante {
            Some(_) => db.append_measurements(&series_id, &mesures().collect::<Vec<_>>()).unwrap().inserted,
            None => {
                let a_une_principale = db
                    .list_series_for_point(point_id)
                    .unwrap()
                    .iter()
                    .any(|s| s.role == SeriesRole::Primary);
                let serie = LoggerSeries {
                    id: series_id.clone(),
                    point_id: point_id.clone(),
                    source_type: SourceType::InfoclimatApi,
                    role: if a_une_principale { SeriesRole::Backup } else { SeriesRole::Primary },
                    logger_serial: None,
                    imported_at: now_utc(),
                    raw_file_reference: Some("collecte Infoclimat".into()),
                    clock_offset_seconds: 0,
                    raw_timestamp_was_utc: true,
                    label: None,
                    sample_interval_seconds: intervalle_dominant(&records),
                    data_license: licence,
                    station_id: Some(station.clone()),
                    last_api_fetch_at: Some(now_utc()),
                    date_order: DateOrder::DayFirst,
                    date_format_uncertain: false,
                    content_fingerprint: Some(series_fingerprint_of(&records)),
                    measurement_count: 0,
                };
                db.insert_series_with_measurements_iter(&serie, mesures()).unwrap()
            }
        };
        total += ecrites;
        println!(
            "[{}/{}] {station} : {} lues, {ecrites} nouvelles — total {total}, {:.0} s",
            rang + 1,
            stations.len(),
            records.len(),
            debut.elapsed().as_secs_f64()
        );
    }
    println!("FINI : {total} mesures écrites, {sans_point} stations sans point, {erreurs} fichiers illisibles, {:.0} s", debut.elapsed().as_secs_f64());
}

/// Écart le plus fréquent entre deux mesures, en secondes.
fn intervalle_dominant(r: &[InfoclimatRecord]) -> Option<i64> {
    let mut n: HashMap<i64, usize> = HashMap::new();
    for w in r.windows(2) {
        *n.entry((w[1].timestamp_utc - w[0].timestamp_utc).num_seconds()).or_default() += 1;
    }
    n.into_iter().max_by_key(|&(e, c)| (c, -e)).map(|(e, _)| e)
}

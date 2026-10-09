//! Banc de relecture — allocations et mémoire retenue par `Vec<Measurement>`.
//!
//! Usage :
//!   cargo run --manifest-path src-tauri/Cargo.toml --example bench_lecture --release
//!
//! Deux séries (Primary et Backup) de N mesures chacune, entrelacées dans le
//! temps, sont insérées dans une base fichier temporaire, puis relues par
//! `Db::measurements_for_point` (fiche d'une station, contexte de l'agent).
//! Le banc affiche le nombre d'allocations de la relecture, la mémoire encore
//! retenue par le résultat et une **signature** de toutes les colonnes lues :
//! deux versions du code qui relisent la même chose affichent la même
//! signature.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use chrono::{DateTime, Duration, Utc};
use gws_lib::db::Db;
use gws_lib::model::{LoggerSeries, Measurement, NewPoint, Point, SeriesRole, SourceType};
use gws_lib::parsing::DateOrder;
use sha2::{Digest, Sha256};

static CURRENT: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static COUNT: AtomicUsize = AtomicUsize::new(0);

struct TrackingAlloc;

unsafe impl GlobalAlloc for TrackingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            COUNT.fetch_add(1, Ordering::Relaxed);
            let prev = CURRENT.fetch_add(layout.size() as i64, Ordering::Relaxed);
            PEAK.fetch_max((prev + layout.size() as i64).max(0) as usize, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        CURRENT.fetch_sub(layout.size() as i64, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = System.realloc(ptr, layout, new_size);
        if !new_ptr.is_null() {
            COUNT.fetch_add(1, Ordering::Relaxed);
            let delta = new_size as i64 - layout.size() as i64;
            let prev = CURRENT.fetch_add(delta, Ordering::Relaxed);
            PEAK.fetch_max((prev + delta).max(0) as usize, Ordering::Relaxed);
        }
        new_ptr
    }
}

#[global_allocator]
static A: TrackingAlloc = TrackingAlloc;

fn serie(point_id: &str, id: &str, role: SeriesRole) -> LoggerSeries {
    LoggerSeries {
        id: id.to_string(),
        point_id: point_id.to_string(),
        source_type: SourceType::RommaImport,
        role,
        logger_serial: None,
        imported_at: DateTime::from_timestamp(1_767_225_600, 0).unwrap(),
        raw_file_reference: None,
        clock_offset_seconds: 0,
        raw_timestamp_was_utc: true,
        label: None,
        sample_interval_seconds: Some(600),
        date_order: DateOrder::DayFirst,
        date_format_uncertain: false,
        data_license: None,
        station_id: None,
        last_api_fetch_at: None,
        content_fingerprint: None,
        measurement_count: 0,
    }
}

fn main() {
    let n: usize = 200_000;
    let fichier = std::env::temp_dir().join(format!("gws_bench_lecture_{}.sqlite", uuid::Uuid::new_v4()));
    {
        let db = Db::open(&fichier).unwrap();
        let projet = db.create_project("Banc").unwrap();
        let point = Point::from_new(NewPoint {
            project_id: projet.id.clone(),
            name: "Point".into(),
            latitude: 45.0,
            longitude: 5.5,
            altitude_m: Some(1000.0),
            category: "Cat3".into(),
            size: Some("Gr".into()),
            factor_sky: None, factor_wind: None, factor_slope: None,
            factor_snow_cover: None, factor_forest_cover: None,
            is_open_terrain: None, notes: None,
            infoclimat_station_id: None, romma_station_id: None,
        })
        .unwrap();
        db.insert_point(&point).unwrap();

        // Identifiants fixes : la signature ne dépend d'aucun tirage.
        let t0: DateTime<Utc> = DateTime::from_timestamp(1_704_067_200, 0).unwrap();
        for (k, (id, role)) in [
            ("00000000-0000-4000-8000-000000000001", SeriesRole::Primary),
            ("00000000-0000-4000-8000-000000000002", SeriesRole::Backup),
        ]
        .into_iter()
        .enumerate()
        {
            let s = serie(&point.id, id, role);
            let mesures: Vec<Measurement> = (0..n)
                .map(|i| {
                    let t = t0 + Duration::seconds(600 * i as i64 + 300 * k as i64);
                    Measurement {
                        humidity_percent: (i % 3 != 0).then_some(50.0 + (i % 40) as f64),
                        ..Measurement::new_utc(id, t, -10.0 + (i % 250) as f64 * 0.1)
                    }
                })
                .collect();
            db.insert_series_with_measurements(&s, &mesures).unwrap();
        }

        let avant_count = COUNT.load(Ordering::Relaxed);
        let avant_mem = CURRENT.load(Ordering::Relaxed);
        PEAK.store(avant_mem.max(0) as usize, Ordering::Relaxed);
        let lues = db.measurements_for_point(&point.id).unwrap();
        let allocs = COUNT.load(Ordering::Relaxed) - avant_count;
        let retenu = CURRENT.load(Ordering::Relaxed) - avant_mem;
        let pic = PEAK.load(Ordering::Relaxed) as i64 - avant_mem;

        let mut h = Sha256::new();
        for m in &lues {
            let sid: &str = &m.series_id;
            h.update(format!(
                "{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}\n",
                m.id, sid, m.timestamp_utc.timestamp(), m.raw_timestamp.timestamp(),
                m.temperature_celsius.to_bits(), m.humidity_percent, m.rssi_dbm, m.battery,
                m.pressure_hpa, m.wind_avg_kmh, m.wind_gust_kmh, m.wind_direction_degrees,
                m.rain_mm, m.solar_radiation,
            ));
        }
        let signature = h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();

        println!("mesures relues        : {}", lues.len());
        println!("size_of::<Measurement>: {} o", std::mem::size_of::<Measurement>());
        println!("allocations relecture : {allocs}");
        println!("mémoire retenue       : {:.2} Mo", retenu as f64 / 1_048_576.0);
        println!("pic pendant relecture : {:.2} Mo", pic as f64 / 1_048_576.0);
        println!("signature             : {signature}");
    }
    for suffixe in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffixe}", fichier.display()));
    }
}

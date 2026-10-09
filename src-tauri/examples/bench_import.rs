//! Banc d'import de bout en bout — pic RAM, temps et **signature** du résultat.
//!
//! Usage :
//!   cargo run --manifest-path src-tauri/Cargo.toml --example bench_import --release
//!
//! Contrairement à `stress_mesures`, qui insère des `Measurement` déjà
//! construits, ce banc passe par les vraies commandes d'import
//! (`import_romma_impl`, `import_infoclimat_impl`) à partir d'un **fichier texte**
//! synthétique — donc parseur, empreinte, détection de ré-import, insertion et
//! statistiques compris.
//!
//! La signature condense le bilan d'import (hors identifiants aléatoires) et
//! toutes les lignes écrites en base : deux versions du code qui importent la
//! même chose affichent la même signature. C'est ce qui permet de vérifier qu'une
//! optimisation mémoire n'a rien changé au résultat.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Instant;

use gws_lib::commands::{
    import_infoclimat_impl, import_romma_impl, ImportOutcome, InfoclimatImportPayload,
    RommaImportPayload,
};
use gws_lib::db::Db;
use gws_lib::model::{NewPoint, Point};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Allocateur de suivi
// ---------------------------------------------------------------------------

static CURRENT: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct TrackingAlloc;

unsafe impl GlobalAlloc for TrackingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
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
            let delta = new_size as i64 - layout.size() as i64;
            let prev = CURRENT.fetch_add(delta, Ordering::Relaxed);
            PEAK.fetch_max((prev + delta).max(0) as usize, Ordering::Relaxed);
        }
        new_ptr
    }
}

#[global_allocator]
static A: TrackingAlloc = TrackingAlloc;

fn mb(octets: usize) -> f64 {
    octets as f64 / 1_048_576.0
}

// ---------------------------------------------------------------------------
// Fichiers synthétiques
// ---------------------------------------------------------------------------

const DEBUT_EPOCH: i64 = 1_496_250_000; // 31-05-2017 19:00 UTC, comme l'extrait réel

fn temperature(i: usize) -> f64 {
    let t = i as f64 * 600.0;
    let v = 5.0 + 12.0 * (t / (365.25 * 86_400.0) * std::f64::consts::TAU).sin()
        + 6.0 * (t / 86_400.0 * std::f64::consts::TAU).sin();
    (v * 10.0).round() / 10.0
}

/// Archive ROMMA : en-tête, ligne d'unités, `n` lignes au pas de 10 min.
/// ~2 % de lignes `Temp = NULL`, comme le fichier réel.
///
/// `desordre` : permute des paires de lignes et répète quelques instants, pour
/// exercer le chemin « fichier non trié / doublons ».
fn archive_romma(n: usize, desordre: bool) -> String {
    const DIRS: [&str; 6] = ["N", "SO", "OSO", "SSE", "---", "NNE"];
    let mut lignes: Vec<String> = Vec::with_capacity(n);
    for i in 0..n {
        let ts = DEBUT_EPOCH + i as i64 * 600;
        let temp = if i % 50 == 7 {
            "NULL".to_string()
        } else {
            format!("{}", temperature(i))
        };
        let hum = if i % 13 == 0 { "NULL".to_string() } else { format!("{}", 60 + i % 40) };
        let rad = if i % 3 == 0 { "NULL".to_string() } else { format!("{}", i % 900) };
        lignes.push(format!(
            "01-01-2020,00:00,{ts},{temp},{temp},{temp},{hum},{:.1},{:.2},{:.1},{},{:.1},{},NULL,{rad}",
            1000.0 + (i % 30) as f64,
            (i % 7) as f64 * 0.2,
            (i % 25) as f64,
            DIRS[i % DIRS.len()],
            (i % 40) as f64,
            DIRS[(i + 1) % DIRS.len()],
        ));
    }
    if desordre {
        for i in (0..n.saturating_sub(1)).step_by(97) {
            lignes.swap(i, i + 1);
        }
        for i in (0..n).step_by(1_001) {
            let copie = lignes[i].clone();
            lignes.push(copie);
        }
    }
    let mut s = String::with_capacity(n * 90);
    s.push_str("Date,heure,timestamp,Temp,Tmin,Tmax,Hum,Pression,Pluie,Vmoy,VmoyDir,Rafale,RafaleDir,RadSolaire, RadSolaireMax\n");
    s.push_str(",,,degreC,degreC,degreC,%,hPa,mm,km/h,,km/h,,W/m2, W/m2\n");
    for l in lignes {
        s.push_str(&l);
        s.push('\n');
    }
    s
}

/// Export CSV Infoclimat : métadonnées `#`, intitulés, unités, `n` lignes.
fn export_infoclimat(n: usize) -> String {
    let mut s = String::with_capacity(n * 160);
    s.push_str("# METADATA:\n# STATIC0213: La-Chapelle-en-Vercors - Combe de Darbounouse\n# Source: www.infoclimat.fr\n# Licence Ouverte/Open License: http://wiki.data.gouv.fr/images/9/9d/Licence_Ouverte.pdf\n");
    s.push_str("station_id;dh_utc;temperature;pression;pression_variation_3h;humidite;point_de_rosee;visibilite;vent_moyen;vent_rafales;vent_rafales_10min;vent_direction;temperature_min;temperature_max;pluie_1h;pluie_3h;pluie_6h;pluie_12h;pluie_24h;pluie_cumul_0h;pluie_intensite;pluie_intensite_max_1h;uv;uv_index;ensoleillement;temperature_sol;temps_omm;complements;source\n");
    s.push_str("string;YYYY-MM-DD hh:mm:ss;°C;hPa;hPa;%;°C;m;km/h;km/h;km/h;deg;°C;°C;mm;mm;mm;mm;mm;mm;mm/h;mm/h;W/m²;UV Index;hours;°C;x;various;x\n");
    let debut = chrono::DateTime::from_timestamp(1_697_274_000, 0).unwrap();
    for i in 0..n {
        let dh = (debut + chrono::Duration::seconds(i as i64 * 600)).format("%Y-%m-%d %H:%M:%S");
        let temp = if i % 40 == 3 { String::new() } else { format!("{}", temperature(i)) };
        let _ = writeln!(
            s,
            "STATIC0213;{dh};{temp};;;{};;;;;;;;;;;;;;;;;;;;;;{{\"rssi\":-{},\"lat\":44.9676,\"lon\":5.47711,\"batt\":36.{}}};liveobjects",
            80 + i % 20,
            100 + i % 17,
            i % 100,
        );
    }
    s
}

// ---------------------------------------------------------------------------
// Signature du résultat
// ---------------------------------------------------------------------------

fn signature(db: &Db, outcome: &ImportOutcome) -> String {
    let mut h = Sha256::new();
    let mut json = serde_json::to_value(outcome).unwrap();
    let report = json.get_mut("report").expect("import attendu, pas un doublon");
    let series_id = report["series_id"].as_str().unwrap().to_string();
    let obj = report.as_object_mut().unwrap();
    obj.remove("series_id");
    obj.remove("point_id");
    h.update(serde_json::to_string(&json).unwrap().as_bytes());

    let serie = db.get_series(&series_id).unwrap();
    h.update(
        format!(
            "{:?}|{:?}|{:?}|{}",
            serie.content_fingerprint,
            serie.sample_interval_seconds,
            serie.date_order,
            serie.measurement_count
        )
        .as_bytes(),
    );
    for m in db.measurements_for_series(&series_id).unwrap() {
        h.update(
            format!(
                "{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}\n",
                m.timestamp_utc.timestamp(),
                m.raw_timestamp.timestamp(),
                m.temperature_celsius,
                m.humidity_percent,
                m.rssi_dbm,
                m.battery,
                m.pressure_hpa,
                m.wind_avg_kmh,
                m.wind_gust_kmh,
                m.wind_direction_degrees,
                m.rain_mm,
                m.solar_radiation
            )
            .as_bytes(),
        );
    }
    let mut hex = String::new();
    for o in h.finalize().iter().take(8) {
        let _ = write!(hex, "{o:02x}");
    }
    hex
}

// ---------------------------------------------------------------------------
// Scénarios
// ---------------------------------------------------------------------------

fn nouveau_point(db: &Db) -> Point {
    let projet = db.create_project("Bench").unwrap();
    let point = Point::from_new(NewPoint {
        project_id: projet.id.clone(),
        name: "Point bench".to_string(),
        latitude: 45.0,
        longitude: 5.5,
        altitude_m: Some(1000.0),
        category: "Cat3".to_string(),
        size: Some("Gr".to_string()),
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
    point
}

fn est_doublon(o: &ImportOutcome) -> bool {
    matches!(o, ImportOutcome::DuplicateSuspected { .. })
}

/// Lance `import` deux fois : la première doit importer, la seconde doit être
/// reconnue comme ré-import. Affiche le pic RAM de la première, **au-delà** de
/// ce qui était déjà alloué (le fichier source compris).
fn scenario(nom: &str, contenu: String, import: impl Fn(&Db, &str, String) -> ImportOutcome) {
    // Base **sur disque**, comme dans l'app : une base `:memory:` ferait compter
    // les lignes écrites dans le pic RAM de l'import.
    let fichier = std::env::temp_dir().join(format!("gws_bench_{}.sqlite", uuid::Uuid::new_v4()));
    let db = Db::open(&fichier).unwrap();
    let point = nouveau_point(&db);
    let taille = contenu.len();
    let copie = contenu.clone();

    let base = CURRENT.load(Ordering::Relaxed).max(0) as usize;
    PEAK.store(base, Ordering::Relaxed);
    let t0 = Instant::now();
    let outcome = import(&db, &point.id, contenu);
    let ms = t0.elapsed().as_millis();
    let pic = PEAK.load(Ordering::Relaxed).saturating_sub(base);

    let sig = signature(&db, &outcome);
    let n = match &outcome {
        ImportOutcome::Imported { report } => report.imported_count,
        _ => 0,
    };
    let reimport = import(&db, &point.id, copie);

    println!(
        "{nom:<28} fichier {:>6.1} Mo | {n:>7} mesures | {ms:>6} ms | pic RAM {:>7.1} Mo | ré-import détecté : {} | signature {sig}",
        mb(taille),
        mb(pic),
        if est_doublon(&reimport) { "oui" } else { "NON" },
    );
    drop(db);
    for suffixe in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffixe}", fichier.display()));
    }
}

fn main() {
    let romma = |db: &Db, point_id: &str, content: String| {
        import_romma_impl(
            db,
            RommaImportPayload {
                point_id: point_id.to_string(),
                content,
                file_name: Some("archive.csv".into()),
                role: "Primary".into(),
                station_id: None,
                label: None,
                force_duplicate: false,
            },
        )
        .unwrap()
    };
    let infoclimat = |db: &Db, point_id: &str, content: String| {
        import_infoclimat_impl(
            db,
            InfoclimatImportPayload {
                point_id: point_id.to_string(),
                content,
                file_name: Some("export.csv".into()),
                role: "Primary".into(),
                station_id: None,
                label: None,
                append_to_existing: false,
                force_duplicate: false,
            },
        )
        .unwrap()
    };

    println!();
    scenario("ROMMA 477k (trié)", archive_romma(477_000, false), romma);
    scenario("ROMMA 100k (désordre+dbl)", archive_romma(100_000, true), romma);
    scenario("Infoclimat CSV 200k", export_infoclimat(200_000), infoclimat);
    println!();
}

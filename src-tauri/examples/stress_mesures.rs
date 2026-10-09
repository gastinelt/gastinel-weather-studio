//! Benchmark de charge — RAM, temps et limite pratique du système.
//!
//! Usage :
//!   cargo run --manifest-path src-tauri/Cargo.toml --example stress_mesures --release
//!
//! Le flag `--release` est **obligatoire** : en mode debug SQLite n'est pas
//! optimisé et les temps sont 10× plus longs — non représentatifs.
//!
//! Ce programme :
//!   1. Génère N mesures synthétiques en RAM (pas de fichier).
//!   2. Les insère dans une base SQLite en mémoire (`:memory:`).
//!   3. Les relit via `temperature_samples_for_point` (chemin du croisement).
//!   4. Calcule `period_stats` dessus.
//!   5. Rapporte pour chaque palier : temps, RAM allouée au pic, débit.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};
use gws_lib::db::Db;
use gws_lib::model::{LoggerSeries, Measurement, NewPoint, SeriesRole, SourceType};
use gws_lib::parsing::DateOrder;
use gws_lib::stats::period_stats;
use uuid::Uuid;

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
            let current = (prev + layout.size() as i64).max(0) as usize;
            PEAK.fetch_max(current, Ordering::Relaxed);
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
            let current = (prev + delta).max(0) as usize;
            PEAK.fetch_max(current, Ordering::Relaxed);
        }
        new_ptr
    }
}

#[global_allocator]
static A: TrackingAlloc = TrackingAlloc;

fn reset_peak() {
    PEAK.store(CURRENT.load(Ordering::Relaxed).max(0) as usize, Ordering::Relaxed);
}

fn peak_mb() -> f64 {
    PEAK.load(Ordering::Relaxed) as f64 / 1_048_576.0
}

fn current_mb() -> f64 {
    CURRENT.load(Ordering::Relaxed).max(0) as f64 / 1_048_576.0
}

// ---------------------------------------------------------------------------
// Génération synthétique
// ---------------------------------------------------------------------------

/// Mesures espacées de 10 min, sinusoïde annuelle −15 °C … +25 °C.
fn generer_mesures(series_id: &str, n: usize) -> Vec<Measurement> {
    let debut = DateTime::from_timestamp(1_577_836_800, 0).unwrap(); // 2020-01-01 UTC
    let pas = 600i64;
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let ts = debut + Duration::seconds(i as i64 * pas);
        let fa = (i as f64 * pas as f64) / (365.25 * 86_400.0);
        let fj = (i as f64 * pas as f64) / 86_400.0;
        let t = -15.0
            + 20.0 * (0.5 - 0.5 * (2.0 * std::f64::consts::PI * fa).cos())
            + 5.0 * (2.0 * std::f64::consts::PI * fj).sin();
        v.push(Measurement::new_utc(series_id, ts, t));
    }
    v
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paliers: &[usize] = &[
        10_000,
        50_000,
        100_000,
        250_000,
        500_000,
        1_000_000,
        2_000_000,
        5_000_000,
        10_000_000,
    ];

    println!();
    println!("╔═══════════════════════════════════════════════════════════════════════════════════╗");
    println!("║           GWS — Stress test mémoire et limites de charge                          ║");
    println!("╠═══════════════════════════════════════════════════════════════════════════════════╣");
    println!("║  Base SQLite :memory: vierge par palier. 10 min/mesure → 1 M ≈ 19 ans.           ║");
    println!("╚═══════════════════════════════════════════════════════════════════════════════════╝");
    println!();
    println!(
        "{:>12}  {:>9} {:>8}  {:>9} {:>8}  {:>9} {:>8}  {:>9}  {:>8}",
        "Mesures",
        "Génér.ms", "PicRAM",
        "Insert.ms", "PicRAM",
        "Rqt.ms",   "PicRAM",
        "Stats.ms",
        "BaseRAM",
    );
    println!("{}", "─".repeat(95));

    for &n in paliers {
        // ---- Base fraîche -----------------------------------------------
        let db = Db::open_in_memory()?;
        let projet = db.create_project("Stress")?;
        let point = gws_lib::model::Point::from_new(NewPoint {
            project_id: projet.id.clone(),
            name: "Point stress".to_string(),
            latitude: 45.0,
            longitude: 5.5,
            altitude_m: Some(1000.0),
            category: "Cat3".to_string(),
            size: Some("Gr".to_string()),
            factor_sky: None, factor_wind: None, factor_slope: None,
            factor_snow_cover: None, factor_forest_cover: None,
            is_open_terrain: None, notes: None,
            infoclimat_station_id: None, romma_station_id: None,
        })?;
        db.insert_point(&point)?;

        let series_id = Uuid::new_v4().to_string();
        let series = LoggerSeries {
            id: series_id.clone(),
            point_id: point.id.clone(),
            source_type: SourceType::RommaImport,
            role: SeriesRole::Primary,
            logger_serial: None,
            imported_at: Utc::now(),
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
        };

        // ---- 1. Génération ----------------------------------------------
        reset_peak();
        let t0 = Instant::now();
        let mesures = generer_mesures(&series_id, n);
        let t_gen = t0.elapsed().as_millis();
        let p_gen = peak_mb();

        // ---- 2. Insertion -----------------------------------------------
        reset_peak();
        let t0 = Instant::now();
        db.insert_series_with_measurements(&series, &mesures)?;
        let t_ins = t0.elapsed().as_millis();
        let p_ins = peak_mb();

        drop(mesures); // libère le Vec avant la requête

        // ---- 3. Requête temperature_samples_for_point -------------------
        reset_peak();
        let t0 = Instant::now();
        let samples = db.temperature_samples_for_point(&point.id, None, None)?;
        let t_rqt = t0.elapsed().as_millis();
        let p_rqt = peak_mb();

        // ---- 4. Calcul des stats (inclut daily_stats) -------------------
        reset_peak();
        let t0 = Instant::now();
        let stats = period_stats(&samples);
        let t_sta = t0.elapsed().as_millis();

        // empêche le compilateur d'optimiser
        assert!(stats.measurement_count > 0);
        drop(samples);

        // ---- RAM résiduelle (base SQLite :memory:) ----------------------
        let ram_base = current_mb();

        println!(
            "{:>12}  {:>9} {:>7.1}Mo  {:>9} {:>7.1}Mo  {:>9} {:>7.1}Mo  {:>9}  {:>7.1}Mo",
            fmt_n(n),
            fmt_ms(t_gen), p_gen,
            fmt_ms(t_ins), p_ins,
            fmt_ms(t_rqt), p_rqt,
            fmt_ms(t_sta),
            ram_base,
        );

        // Seuil d'arrêt : insertion > 2 min → limite atteinte
        if t_ins > 120_000 {
            println!();
            println!("⚠️  Insertion > 2 min — limite pratique atteinte, arrêt.");
            break;
        }
    }

    println!();
    println!("Légende colonnes :");
    println!("  Génér.  — Vec<Measurement> généré en RAM (temps + pic heap Rust)");
    println!("  Insert. — transaction SQLite :memory: complète");
    println!("  Rqt.    — temperature_samples_for_point (lecture filtrée = chemin du croisement)");
    println!("  Stats.  — period_stats Rust pur (inclut daily_stats, extreme_min, etc.)");
    println!("  PicRAM  — RAM heap allouée au pic de l'étape (allocateur Rust uniquement)");
    println!("  BaseRAM — RAM résiduelle (SQLite :memory: après libération des Vecs)");
    println!();

    Ok(())
}

fn fmt_n(n: usize) -> String {
    let s = n.to_string();
    let mut r = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 { r.push(' '); }
        r.push(c);
    }
    r.chars().rev().collect()
}

fn fmt_ms(ms: u128) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

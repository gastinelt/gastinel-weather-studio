//! Générateur de jeux d'essai pour le bloc « Croisement de données ».
//!
//! Écrit sur disque, en JSON, exactement ce que la commande Tauri
//! `cross_analysis` renvoie au frontend — même code, même sérialisation. Sert à
//! deux choses :
//!
//! * mettre le rendu de `cross.js` à l'épreuve d'un nombre variable de points et
//!   de volumes réalistes, sans avoir à importer 38 Mo d'archive à la main ;
//! * **mesurer** le coût du chemin complet (lecture SQLite + agrégation) sur un
//!   point de la taille de l'archive ROMMA, qui est le cas dimensionnant.
//!
//! Ce n'est pas un test : rien n'échoue ici, on observe. Les invariants, eux,
//! sont vérifiés par `cargo test` (`stats.rs` et `crossing.rs`).
//!
//! ```text
//! cargo run --release --example dump_cross_fixtures -- <répertoire de sortie>
//! ```

use std::path::PathBuf;
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};
use gws_lib::crossing::{self, TimeWindow};
use gws_lib::db::Db;
use gws_lib::model::{
    LoggerSeries, Measurement, NewPoint, Point, SeriesRole, SourceType,
};
use gws_lib::parsing::DateOrder;

/// Volume de l'archive ROMMA réelle (Lans - Les Allières, mai 2017 → août 2026).
const MESURES_ROMMA: i64 = 476_912;
const PAS_MINUTES: i64 = 10;

fn main() {
    let sortie: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".".to_string())
        .into();
    std::fs::create_dir_all(&sortie).expect("répertoire de sortie");

    // Base **sur disque**, et non en mémoire : c'est la seule mesure qui vaille.
    // Une base en mémoire fait disparaître le coût de lecture des pages, qui est
    // précisément ce qu'on veut chiffrer sur 476 912 mesures.
    let chemin = sortie.join("banc-essai.sqlite3");
    let _ = std::fs::remove_file(&chemin);
    let db = Db::open(&chemin).expect("base sur disque");
    println!("base sur disque : {}", chemin.display());
    let projet = db.create_project("Banc d'essai").expect("projet");

    // Un point volumineux, puis onze points ordinaires : de quoi éprouver aussi
    // bien le cas dimensionnant que l'avertissement de lisibilité (> 8 courbes).
    let mut ids = Vec::new();

    let debut = Instant::now();
    ids.push(creer_point(
        &db,
        &projet.id,
        "Lans - Les Allières (ROMMA)",
        "Cat1",
        None,
        MESURES_ROMMA,
        0.0,
        false,
    ));
    println!(
        "insertion de {MESURES_ROMMA} mesures : {} ms",
        debut.elapsed().as_millis()
    );

    let ordinaires: [(&str, &str, Option<&str>, f64, bool); 11] = [
        ("Darbounouse", "Cat3", Some("Gr"), -3.5, false),
        ("Combe Chaude", "Cat3", Some("Pt"), -1.0, false),
        ("Plateau de Font d'Urle", "Cat2", Some("Gr"), 1.5, false),
        ("Fessole", "Cat4", Some("Gr"), -5.0, false),
        ("Pré Peyret", "Cat2", None, 0.5, true),
        ("Chaumailloux", "Cat3", Some("Gr"), -2.0, false),
        ("Jasse de la Chau", "Cat4", Some("Pt"), -4.2, false),
        ("Gaudichard", "Cat1", None, 2.5, false),
        ("La Moucherolle", "Cat1", None, 3.0, false),
        ("Serre de Montué", "Cat2", Some("Gr"), 0.0, false),
        ("Trou du Glaz", "Cat3", Some("Pt"), -6.0, false),
    ];
    // 45 jours à un pas de 10 minutes : le volume d'un logger relevé en fin de saison.
    let mesures_logger = 45 * 24 * 60 / PAS_MINUTES;
    for (nom, cat, taille, decalage, avec_trou) in ordinaires {
        ids.push(creer_point(
            &db,
            &projet.id,
            nom,
            cat,
            taille,
            mesures_logger,
            decalage,
            avec_trou,
        ));
    }

    // --- Jeux d'essai du rendu -------------------------------------------
    let cas: [(&str, &[usize], TimeWindow); 8] = [
        ("00-aucun", &[], TimeWindow::All),
        ("01-un-point", &[1], TimeWindow::Last30Days),
        ("02-deux-points-7j", &[1, 2], TimeWindow::Last7Days),
        ("03-trois-points-30j", &[1, 2, 3], TimeWindow::Last30Days),
        ("04-huit-points-30j", &[1, 2, 3, 4, 5, 6, 7, 8], TimeWindow::Last30Days),
        ("05-douze-points-30j", &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 0], TimeWindow::Last30Days),
        ("06-romma-tout", &[0], TimeWindow::All),
        ("07-romma-et-logger-tout", &[0, 1, 2], TimeWindow::All),
    ];

    for (nom, indices, fenetre) in cas {
        let selection: Vec<String> = indices.iter().map(|i| ids[*i].clone()).collect();
        let t0 = Instant::now();
        let analyse = crossing::build(&db, &selection, fenetre).expect("analyse");
        let json = serde_json::to_string(&analyse).expect("sérialisation");

        println!(
            "{nom:28} {:>2} points · {:>5} ms (dont {:>5} ms mesurés côté Rust) · \
             {:>7} octets JSON · {:>4} seaux max",
            selection.len(),
            t0.elapsed().as_millis(),
            analyse.elapsed_ms,
            json.len(),
            analyse
                .points
                .iter()
                .map(|p| p.curve.len())
                .max()
                .unwrap_or(0),
        );

        std::fs::write(sortie.join(format!("{nom}.json")), json).expect("écriture");
    }

    // --- Mesure dédiée : le cas dimensionnant, chaque fenêtre ------------
    println!("\nPoint ROMMA seul ({MESURES_ROMMA} mesures), par fenêtre :");
    for fenetre in [
        TimeWindow::Last7Days,
        TimeWindow::Last30Days,
        TimeWindow::CurrentYear,
        TimeWindow::All,
    ] {
        // Trois passes : la première paie le remplissage du cache de pages SQLite.
        let mut derniere = 0;
        for _ in 0..3 {
            let t0 = Instant::now();
            let a = crossing::build(&db, &ids[0..1], fenetre).expect("analyse");
            derniere = t0.elapsed().as_millis();
            std::hint::black_box(a);
        }
        println!("  {:<32} {derniere:>5} ms", fenetre.label_fr());
    }

    println!("\nHuit points (dont ROMMA), tout l'historique :");
    let huit: Vec<String> = ids[0..8].to_vec();
    for _ in 0..2 {
        let t0 = Instant::now();
        let a = crossing::build(&db, &huit, TimeWindow::All).expect("analyse");
        println!(
            "  {:>5} ms · JSON {} octets",
            t0.elapsed().as_millis(),
            serde_json::to_string(&a).unwrap().len()
        );
    }

    println!("\nJeux d'essai écrits dans {}", sortie.display());
    drop(db);
    let _ = std::fs::remove_file(&chemin);
}

#[allow(clippy::too_many_arguments)]
fn creer_point(
    db: &Db,
    project_id: &str,
    nom: &str,
    categorie: &str,
    taille: Option<&str>,
    nb_mesures: i64,
    decalage: f64,
    avec_trou: bool,
) -> String {
    let point = Point::from_new(NewPoint {
        project_id: project_id.to_string(),
        name: nom.to_string(),
        latitude: 44.98,
        longitude: 5.42,
        altitude_m: Some(1150.0),
        category: categorie.to_string(),
        size: taille.map(String::from),
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
    .expect("point valide");
    db.insert_point(&point).expect("insertion du point");

    let serie = LoggerSeries {
        id: uuid::Uuid::new_v4().to_string(),
        point_id: point.id.clone(),
        source_type: SourceType::Manual,
        role: SeriesRole::Primary,
        logger_serial: None,
        imported_at: Utc::now(),
        raw_file_reference: None,
        clock_offset_seconds: 0,
        raw_timestamp_was_utc: true,
        label: None,
        sample_interval_seconds: Some(PAS_MINUTES * 60),
        data_license: None,
        station_id: None,
        last_api_fetch_at: None,
        date_order: DateOrder::DayFirst,
        date_format_uncertain: false,
        content_fingerprint: None,
        measurement_count: 0,
    };

    // Toutes les séries se terminent au même instant : c'est la situation
    // réaliste (des relevés remontés jusqu'à aujourd'hui) et celle où l'ancre
    // des fenêtres relatives se lit le mieux.
    let fin: DateTime<Utc> = "2026-08-19T12:30:00Z".parse().unwrap();
    let mut mesures = Vec::with_capacity(nb_mesures as usize);
    for i in 0..nb_mesures {
        let instant = fin - Duration::minutes(PAS_MINUTES * (nb_mesures - 1 - i));

        // Interruption de logger : dix jours sans rien, quinze jours avant la fin.
        if avec_trou {
            let jours_avant_fin = (fin - instant).num_days();
            if (15..25).contains(&jours_avant_fin) {
                continue;
            }
        }

        let heure = instant.timestamp() as f64 / 3600.0;
        let jour_de_l_annee = instant.timestamp() as f64 / 86400.0;
        // Cycle diurne + cycle saisonnier + un peu de bruit déterministe.
        let temp = 4.0
            + decalage
            + 8.0 * (heure * std::f64::consts::TAU / 24.0).sin()
            + 12.0 * (jour_de_l_annee * std::f64::consts::TAU / 365.25).sin()
            + ((i % 37) as f64 / 37.0 - 0.5) * 1.5;
        mesures.push(Measurement::new_utc(serie.id.clone(), instant, temp));
    }

    db.insert_series_with_measurements(&serie, &mesures)
        .expect("insertion des mesures");
    point.id
}

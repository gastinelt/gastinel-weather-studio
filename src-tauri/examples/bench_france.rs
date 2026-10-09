//! Banc de lecture sur une grosse base (projet France) : ce que coûte un
//! changement de sélection, c'est-à-dire un `cross_analysis`.
//!
//! Usage :
//!   cargo run --release --example bench_france -- <base.sqlite3> <id du projet>
//!
//! Lecture seule (aucune écriture hors migration de schéma à l'ouverture).
//! Les mêmes appels compilent sur la 0.5.0 et sur l'étape A : on compare les
//! deux binaires sur la même base.

use std::time::Instant;

use gws_lib::crossing::{build, TimeWindow};
use gws_lib::db::Db;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (base, projet) = (&args[1], &args[2]);

    let t = Instant::now();
    let db = Db::open(std::path::Path::new(base)).expect("ouverture");
    println!("ouverture (migration comprise) : {:.2} s", t.elapsed().as_secs_f64());

    // Points triés par volume, lu dans la petite table mensuelle.
    let mut points: Vec<(String, usize)> = db
        .project_coverage(projet)
        .unwrap()
        .into_iter()
        .map(|c| (c.point_id, c.measurement_count))
        .collect();
    points.sort_by(|a, b| b.1.cmp(&a.1));
    let total: usize = points.iter().map(|p| p.1).sum();
    println!("{} points, {} mesures", points.len(), total);
    let ids = |n: usize| points.iter().take(n).map(|p| p.0.clone()).collect::<Vec<_>>();

    let mesurer = |nom: &str, ids: &[String], w: TimeWindow| {
        let t = Instant::now();
        let a = build(&db, ids, w).unwrap();
        let mesures: usize = a.points.iter().map(|p| p.measurement_count).sum();
        println!(
            "{nom:<38} {:>8.3} s   ({} points, {} mesures dans la fenêtre)",
            t.elapsed().as_secs_f64(),
            ids.len(),
            mesures
        );
    };

    mesurer("1 point, tout l'historique", &ids(1), TimeWindow::All);
    mesurer("1 point, 30 derniers jours", &ids(1), TimeWindow::Last30Days);
    mesurer("1 point, année en cours", &ids(1), TimeWindow::CurrentYear);
    mesurer("8 points, tout l'historique", &ids(8), TimeWindow::All);
    mesurer("50 points, tout l'historique", &ids(50), TimeWindow::All);
    mesurer("tous les points, tout l'historique", &ids(points.len()), TimeWindow::All);
    mesurer("tous les points, 30 derniers jours", &ids(points.len()), TimeWindow::Last30Days);

    // `GWS_DUMP=fichier` : analyse complète de 8 points (tout l'historique) et
    // de 3 points (30 derniers jours), pour comparer deux versions du code
    // chiffre par chiffre.
    if let Ok(f) = std::env::var("GWS_DUMP") {
        let a = build(&db, &ids(8), TimeWindow::All).unwrap();
        let b = build(&db, &ids(3), TimeWindow::Last30Days).unwrap();
        let v = serde_json::json!([a.points, b.points]);
        std::fs::write(f, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }

    let t = Instant::now();
    for (id, _) in points.iter().take(100) {
        db.point_record(id).unwrap();
    }
    println!("{:<38} {:>8.3} s", "record de 100 points", t.elapsed().as_secs_f64());

    // Répartition du temps d'une analyse de toute la sélection, et poids de la
    // réponse que l'interface doit recevoir puis dessiner.
    if std::env::var("GWS_DETAIL").is_ok() {
        let tous = ids(points.len());
        let t = Instant::now();
        for id in &tous {
            db.point_record(id).unwrap();
        }
        println!("{:<38} {:>8.3} s", "record de tous les points", t.elapsed().as_secs_f64());
        let t = Instant::now();
        for id in &tous {
            db.get_point(id).unwrap();
            db.list_series_for_point(id).unwrap();
        }
        println!("{:<38} {:>8.3} s", "fiche + séries de tous les points", t.elapsed().as_secs_f64());
        // Clics successifs sur une base fraîchement ouverte (cache vide) :
        // première analyse, ajout d'un point, puis retrait de ce point.
        let db2 = Db::open(std::path::Path::new(base)).expect("ouverture");
        let sauf_un = &tous[..tous.len() - 1];
        for (nom, w) in [("30 derniers jours", TimeWindow::Last30Days), ("tout l'historique", TimeWindow::All)] {
            for (etape, sel) in [("1re analyse", sauf_un), ("+1 point", &tous[..]), ("-1 point", sauf_un)] {
                let t = Instant::now();
                build(&db2, sel, w).unwrap();
                println!("clic {nom:<18} {etape:<12} {:>8.3} s", t.elapsed().as_secs_f64());
            }
        }

        for (nom, w) in [("30 derniers jours", TimeWindow::Last30Days), ("tout l'historique", TimeWindow::All)] {
            let a = build(&db, &tous, w).unwrap();
            // `GWS_DUMP_TOUS=dossier` : la réponse complète, pour mesurer le
            // rendu de l'interface hors de l'appli.
            if let Ok(d) = std::env::var("GWS_DUMP_TOUS") {
                let nom_fichier = if matches!(w, TimeWindow::All) { "analyse-all.json" } else { "analyse-last30_days.json" };
                std::fs::write(std::path::Path::new(&d).join(nom_fichier), serde_json::to_vec(&a).unwrap()).unwrap();
            }
            let t = Instant::now();
            let json = serde_json::to_vec(&a).unwrap();
            let seaux: usize = a.points.iter().map(|p| p.curve.len()).sum();
            println!(
                "réponse {nom:<20} {:>8.1} Mo JSON, {seaux} seaux, sérialisée en {:.3} s",
                json.len() as f64 / 1e6,
                t.elapsed().as_secs_f64()
            );
        }
    }
}


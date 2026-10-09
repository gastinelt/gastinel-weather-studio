//! Comparaison de deux versions du code sur une copie de la vraie base :
//! toutes les analyses de croisement (chaque projet, chaque fenêtre, toute la
//! sélection puis chaque point seul), le record de chaque point et les
//! statistiques de chaque série, écrits en JSON (sans les durées de calcul).
//!
//! Usage : cargo run --release --example compare_base -- <base> <sortie.json>

use std::time::Instant;

use chrono::NaiveDate;
use gws_lib::crossing::{build, TimeWindow};
use gws_lib::db::Db;
use serde_json::{json, Value};

fn stats_serie(db: &Db, id: &str) -> Value { json!(gws_lib::stats::period_stats_from_days(&db.series_days(id).unwrap())) }

fn sans_duree(a: impl serde::Serialize) -> Value {
    let mut v = serde_json::to_value(a).unwrap();
    if let Some(o) = v.as_object_mut() {
        o.remove("elapsed_ms");
    }
    v
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let t = Instant::now();
    let db = Db::open(std::path::Path::new(&args[1])).expect("ouverture");
    eprintln!("ouverture (migration comprise) : {:.2} s", t.elapsed().as_secs_f64());

    let fenetres = [
        TimeWindow::All,
        TimeWindow::Last7Days,
        TimeWindow::Last30Days,
        TimeWindow::CurrentYear,
        TimeWindow::Custom {
            from: NaiveDate::from_ymd_opt(2025, 12, 15).unwrap(),
            to: NaiveDate::from_ymd_opt(2026, 2, 10).unwrap(),
        },
    ];
    let mut sortie = serde_json::Map::new();
    let t = Instant::now();
    for projet in db.list_projects().unwrap() {
        let points = db.list_points(&projet.id).unwrap();
        let ids: Vec<String> = points.iter().map(|p| p.id.clone()).collect();
        eprintln!("{} : {} points", projet.name, ids.len());
        let mut p = serde_json::Map::new();
        for w in fenetres {
            let cle = format!("{w:?}");
            p.insert(format!("tous {cle}"), sans_duree(build(&db, &ids, w).unwrap()));
            if ids.len() <= 200 {
                for id in &ids {
                    p.insert(format!("{id} {cle}"), sans_duree(build(&db, &[id.clone()], w).unwrap()));
                }
            }
        }
        for id in &ids {
            p.insert(format!("{id} record"), json!(db.point_record(id).unwrap()));
            for s in db.list_series_for_point(id).unwrap() {
                p.insert(format!("{id} serie {}", s.id), stats_serie(&db, &s.id));
            }
        }
        sortie.insert(projet.id.clone(), Value::Object(p));
    }
    eprintln!("calculs : {:.2} s", t.elapsed().as_secs_f64());
    std::fs::write(&args[2], serde_json::to_string_pretty(&sortie).unwrap()).unwrap();
}

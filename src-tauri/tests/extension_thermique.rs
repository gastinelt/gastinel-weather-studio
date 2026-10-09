//! Bout en bout avec la vraie DLL « Analyse thermique », si elle a été compilée
//! (`cargo build --release` dans `extensions/thermique`). Ignoré sinon.
use std::path::PathBuf;

use gws_lib::extensions::{Etat, ExtensionHost};
use serde_json::json;

#[test]
fn thermique_se_charge_et_calcule() {
    let racine = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../extensions/thermique");
    let dll = racine.join("target/release/thermique.dll");
    if !dll.is_file() {
        eprintln!("thermique.dll absente : test ignoré");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("gws-thermique-{}", uuid::Uuid::new_v4()));
    let ext = tmp.join("extensions/thermique");
    std::fs::create_dir_all(ext.join("ui")).unwrap();
    std::fs::copy(racine.join("manifest.json"), ext.join("manifest.json")).unwrap();
    std::fs::copy(&dll, ext.join("thermique.dll")).unwrap();

    // Base synthétique : fond de vallée (1000 m) et versant (1300 m), deux
    // jours au pas de 10 min. Nuit : le fond est 6 °C plus froid (inversion
    // +2 °C/100 m) ; jour : décroissance normale de -0,65 °C/100 m.
    let db = tmp.join("base.sqlite3");
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute_batch(
        "CREATE TABLE points (id TEXT, project_id TEXT, name TEXT, latitude REAL, longitude REAL, altitude_m REAL, category TEXT);
         CREATE TABLE logger_series (id TEXT, point_id TEXT, source_type TEXT, role TEXT, sample_interval_seconds INTEGER, label TEXT);
         CREATE TABLE measurements (series_id TEXT, timestamp_utc INTEGER, temperature_celsius REAL, wind_avg_kmh REAL);
         CREATE UNIQUE INDEX m_idx ON measurements(series_id, timestamp_utc);
         INSERT INTO points VALUES ('bas','p','Fond',45.13,5.52,1000,'Cat3'), ('haut','p','Versant',45.14,5.53,1300,'Versants'),
                                   ('x','autre','Ailleurs',45,5,500,'Cat1');
         INSERT INTO logger_series VALUES ('sb','bas','InfoclimatApi','Primary',600,NULL), ('sh','haut','InfoclimatApi','Primary',600,NULL),
                                          ('sbk','bas','ElitechImport','Backup',600,NULL);",
    )
    .unwrap();
    let j0: i64 = 1_768_435_200; // 15/01/2026 00:00 UTC
    {
        let mut ins = c.prepare("INSERT INTO measurements VALUES (?1, ?2, ?3, NULL)").unwrap();
        for i in 0..(2 * 144) {
            let t = j0 + i * 600;
            let h = ((t - j0) % 86400) as f64 / 3600.0;
            let nuit = !(8.0..16.0).contains(&h);
            let haut = 0.0;
            let bas = if nuit { haut - 6.0 } else { haut + 1.95 };
            ins.execute(rusqlite::params!["sh", t, haut]).unwrap();
            ins.execute(rusqlite::params!["sb", t, bas]).unwrap();
            // La série de secours, décalée de +50 °C, ne doit jamais être lue.
            ins.execute(rusqlite::params!["sbk", t, bas + 50.0]).unwrap();
        }
    }
    drop(c);

    let h = ExtensionHost::with_roots(&tmp, &db, "test", vec![tmp.join("extensions")]);
    let info = h.set_enabled("thermique", true).unwrap();
    assert_eq!(info.state, Etat::Active, "{:?}", info.state);
    assert_eq!(h.call("thermique", "ping", &json!({})).unwrap(), "pong");
    assert!(h.call("thermique", "inconnue", &json!({})).is_err());

    let pts = h.call("thermique", "points", &json!({"project_id": "p"})).unwrap();
    assert_eq!(pts["points"].as_array().unwrap().len(), 2);

    let g = h
        .call("thermique", "gradient", &json!({"project_id": "p", "points": ["bas", "haut"], "debut": j0, "fin": j0 + 2 * 86400 - 600}))
        .unwrap();
    let s = &g["stats"];
    assert_eq!(s["n"], 48);
    assert!((s["max_inversion"].as_f64().unwrap() - 2.0).abs() < 1e-9);
    assert!((s["min_gradient"].as_f64().unwrap() + 0.65).abs() < 1e-9);
    // 16 h de nuit sur 24 (bornes 8 h et 16 h UTC exclues de la nuit).
    assert!((s["part_inversion"].as_f64().unwrap() - 32.0 / 48.0).abs() < 1e-9);
    assert_eq!(g["points"][0]["role"], "Primary");

    // Point d'un autre projet : refusé (averti, puis moins de 2 points).
    assert!(h.call("thermique", "gradient", &json!({"project_id": "p", "points": ["bas", "x"], "debut": j0, "fin": j0 + 86400})).is_err());
    // Période trop longue : refusée.
    assert!(h.call("thermique", "gradient", &json!({"project_id": "p", "points": ["bas", "haut"], "debut": 0, "fin": j0})).is_err());

    let p = h.call("thermique", "profil", &json!({"project_id": "p", "points": ["bas", "haut"], "t": j0 + 3 * 3600})).unwrap();
    assert!((p["pente_100m"].as_f64().unwrap() - 2.0).abs() < 1e-9);
    let ep = h.call("thermique", "episodes", &json!({"project_id": "p", "point": "bas", "debut": j0, "fin": j0 + 86400})).unwrap();
    assert!(ep["resultat"]["nuits_examinees"].as_u64().unwrap() >= 1);
    let cb = h.call("thermique", "courbe", &json!({"project_id": "p", "point": "bas", "debut": j0, "fin": j0 + 86400})).unwrap();
    assert_eq!(cb["valeurs"].as_array().unwrap().len(), 145);

    // Lecture seule : pas de `db_write`, donc aucune sauvegarde.
    assert!(info.last_backup.is_none());
    h.set_enabled("thermique", false).unwrap();
    assert!(h.call("thermique", "ping", &json!({})).is_err());
}

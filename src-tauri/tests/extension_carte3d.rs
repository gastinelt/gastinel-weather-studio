//! Bout en bout avec la vraie DLL de la carte 3D, si elle a été compilée
//! (`cargo build --release` dans `extensions/carte3d`). Ignoré sinon.
use std::path::PathBuf;

use gws_lib::extensions::{Etat, ExtensionHost};

#[test]
fn carte3d_se_charge_et_repond() {
    let racine = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../extensions/carte3d");
    let dll = racine.join("target/release/carte3d.dll");
    if !dll.is_file() {
        eprintln!("carte3d.dll absente : test ignoré");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("gws-carte3d-{}", uuid::Uuid::new_v4()));
    let ext = tmp.join("extensions/carte3d");
    std::fs::create_dir_all(ext.join("ui")).unwrap();
    std::fs::copy(racine.join("manifest.json"), ext.join("manifest.json")).unwrap();
    std::fs::copy(&dll, ext.join("carte3d.dll")).unwrap();

    // Base minimale : un projet, deux points dans le Vercors.
    let db = tmp.join("base.sqlite3");
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute_batch(
        "CREATE TABLE points (id TEXT, project_id TEXT, name TEXT, latitude REAL, longitude REAL, altitude_m REAL, category TEXT);
         INSERT INTO points VALUES ('a','p','Bruyères',45.10,5.57,990,'Cat2'), ('b','p','Moucherolle',45.02,5.58,NULL,'Versants');",
    )
    .unwrap();
    drop(c);

    let h = ExtensionHost::with_roots(&tmp, &db, "test", vec![tmp.join("extensions")]);
    let info = h.set_enabled("carte3d", true).unwrap();
    assert_eq!(info.state, Etat::Active, "{:?}", info.state);
    assert_eq!(h.call("carte3d", "ping", &serde_json::json!({})).unwrap(), "pong");
    assert!(h.call("carte3d", "inconnue", &serde_json::json!({})).is_err());

    let scene = h.call("carte3d", "scene", &serde_json::json!({"project_id": "p"})).unwrap();
    let t = &scene["terrain"];
    let z = t["z"].as_array().unwrap();
    assert_eq!(z.len() as u64, t["nx"].as_u64().unwrap() * t["ny"].as_u64().unwrap());
    let max = z.iter().filter_map(|v| v.as_f64()).fold(0.0, f64::max);
    assert!(max > 1500.0, "le Vercors culmine au-delà de 1500 m, reçu {max}");
    std::fs::write(std::env::temp_dir().join("gws-carte3d-scene.json"), scene.to_string()).unwrap();

    // Lecture seule : l'extension n'a pas `db_write`, aucune sauvegarde faite.
    assert!(info.last_backup.is_none());
    h.set_enabled("carte3d", false).unwrap();
    assert!(h.call("carte3d", "ping", &serde_json::json!({})).is_err());
}

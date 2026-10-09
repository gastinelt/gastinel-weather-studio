//! Calculs sur la base réelle de l'utilisateur, ouverte EN LECTURE SEULE.
//! `cargo test --release --test reel -- --ignored --nocapture`
//! Écrit les réponses JSON dans `GWS_THERMIQUE_SORTIE` (si défini) pour le harnais du panneau.
use gws_extension_api::{Extension, HostContext};
use serde_json::{json, Value};
use thermique::Thermique;

fn ext() -> Option<Thermique> {
    let base = std::path::PathBuf::from(std::env::var("APPDATA").ok()?).join("fr.gastinel.weatherstudio/gastinel-weather-studio.sqlite3");
    if !base.is_file() {
        return None;
    }
    let uri = format!("file:{}?mode=ro", base.to_string_lossy().replace('\\', "/"));
    let mut e = Thermique::default();
    e.init(HostContext { api_version: 1, host_version: "test".into(), db_path: base.to_string_lossy().into(), db_uri: uri, db_writable: false, data_dir: std::env::temp_dir().to_string_lossy().into() }).unwrap();
    Some(e)
}

fn sortie(nom: &str, v: &Value) {
    if let Ok(d) = std::env::var("GWS_THERMIQUE_SORTIE") {
        std::fs::write(std::path::Path::new(&d).join(format!("{nom}.json")), v.to_string()).unwrap();
    }
}

#[test]
#[ignore]
fn vercors() {
    let Some(mut e) = ext() else { return };
    let c = e.call("points", json!({})).err();
    assert!(c.is_some());
    let proj = "bb7966a1-6859-409b-b5b0-2531dd985cbc";
    let pts = e.call("points", json!({"project_id": proj})).unwrap();
    sortie("points", &pts);
    let liste = pts["points"].as_array().unwrap();
    let id = |nom: &str| liste.iter().find(|p| p["name"].as_str().unwrap().starts_with(nom)).unwrap()["id"].as_str().unwrap().to_string();
    for p in liste {
        println!("{} {} m : {} série(s)", p["name"], p["altitude_m"], p["series"].as_array().unwrap().len());
    }
    // Hiver 2024-2025 : Engins (905), Méaudre (976), Lans Allières (1449), St Martin d'Hères (220).
    let (a, b) = (1_730_419_200i64, 1_740_787_200i64); // 01/11/2024 → 01/03/2025
    let sel = vec![id("Engin"), id("Méaudre"), id("Lans-en-Vercors - Les Alli")];
    let t0 = std::time::Instant::now();
    let g = e.call("gradient", json!({"project_id": proj, "points": sel, "debut": a, "fin": b})).unwrap();
    println!("gradient hiver ({:?}) : {}", t0.elapsed(), serde_json::to_string_pretty(&g["stats"]).unwrap());
    println!("avertissements : {}", g["avertissements"]);
    sortie("gradient", &g);
    // Été 2025 (jour : gradient attendu vers -0,5 à -1 °C/100 m), avec St Martin d'Hères.
    let (ae, be) = (1_751_328_000i64, 1_756_684_800i64); // 01/07 → 01/09/2025
    let sel2 = vec![id("Saint Martin"), id("Engin"), id("Lans-en-Vercors - Les Alli")];
    let g2 = e.call("gradient", json!({"project_id": proj, "points": sel2, "debut": ae, "fin": be})).unwrap();
    println!("gradient été : {}", serde_json::to_string_pretty(&g2["stats"]).unwrap());
    // Épisodes radiatifs à Méaudre, hiver.
    let t0 = std::time::Instant::now();
    let ep = e.call("episodes", json!({"project_id": proj, "point": id("Méaudre"), "debut": a, "fin": b})).unwrap();
    let r = &ep["resultat"];
    println!("épisodes Méaudre ({:?}) : {} / {} nuits, rejets {}", t0.elapsed(), r["episodes"].as_array().unwrap().len(), r["nuits_examinees"], r["rejets"]);
    for x in r["episodes"].as_array().unwrap().iter().take(5) {
        println!("  {x}");
    }
    println!("avertissements : {}", ep["avertissements"]);
    sortie("episodes", &ep);
    let e0 = &r["episodes"][0];
    let cb = e.call("courbe", json!({"project_id": proj, "point": id("Méaudre"), "debut": e0["coucher"].as_i64().unwrap() - 8*3600, "fin": e0["lever"].as_i64().unwrap() + 8*3600})).unwrap();
    sortie("courbe", &cb);
    // Profil : une nuit d'hiver à 5 h UTC, instantané puis moyenne 3 h.
    let t = e0["t_min"].as_i64().unwrap();
    let pr = e.call("profil", json!({"project_id": proj, "points": [id("Saint Martin"), id("Engin"), id("Méaudre"), id("Lans-en-Vercors - Les Alli")], "t": t, "fenetre_s": 0})).unwrap();
    println!("profil {} : {}", t, pr);
    sortie("profil", &pr);
    let pr3 = e.call("profil", json!({"project_id": proj, "points": sel, "t": t, "fenetre_s": 10800})).unwrap();
    println!("profil 3 h : pente {}", pr3["pente_100m"]);
    // Garde-fou de période.
    assert!(e.call("gradient", json!({"project_id": proj, "points": sel, "debut": 0, "fin": 1_700_000_000})).is_err());
}

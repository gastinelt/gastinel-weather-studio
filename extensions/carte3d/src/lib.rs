//! Extension « Carte 3D » : relief IGN + points du projet actif.
//!
//! Tout le rendu se fait dans la webview (WebGL, `ui/carte3d.js`) : une
//! bibliothèque de rendu Rust (wgpu, bevy…) exigerait une seconde fenêtre
//! native hors de l'interface Tauri. La partie Rust fournit les données :
//! lecture des points dans la base (en lecture seule), grille d'altitudes
//! (MNT LiDAR HD de l'IGN) et fond Plan IGN, mis en cache sur disque.

use std::path::PathBuf;

use gws_extension_api::{declare_extension, Extension, HostContext};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{json, Value};

/// Service d'altimétrie ponctuelle : secours si la grille WMS échoue.
const GEOPF: &str = "https://data.geopf.fr/altimetrie/1.0/calcul/alti/rest/elevation.json";
/// WMS « raster » de la Géoplateforme : altitudes brutes (BIL 32 bits) et fonds.
const WMS_R: &str = "https://data.geopf.fr/wms-r/wms";
/// MNT dérivé du LiDAR HD (pas natif 50 cm). Vérifié le 25/09/2026 : la couche
/// répond pour toute emprise (France entière en ~4 s) et vaut -9999 hors
/// couverture (mer, étranger).
const COUCHE_MNT: &str = "IGNF_LIDAR-HD_MNT_ELEVATION.MIXED.WGS84G";
const COUCHE_TOPO: &str = "GEOGRAPHICALGRIDSYSTEMS.PLANIGNV2";
/// Le service ponctuel accepte 5 000 points par requête (vérifié le 25/09/2026).
const LOT: usize = 2500;
/// Requêtes ponctuelles simultanées au plus : au-delà, l'IGN limite et refuse.
const PARALLELE: usize = 3;
/// Côté le plus long de la grille, en nombre de sommets.
const GRILLE_MAX: usize = 400;
/// Côté le plus long de l'image topo drapée sur le relief, en pixels.
const TEXTURE_MAX: usize = 1536;
/// Emprise minimale, en degrés (~3 km) : un projet d'un seul point garde un relief lisible.
const EMPRISE_MIN: f64 = 0.03;
/// Change quand le format du cache change : les anciens fichiers sont ignorés.
const VERSION_CACHE: &str = "v2";

#[derive(Default)]
pub struct Carte3d {
    db_uri: String,
    cache: PathBuf,
}

#[derive(Serialize)]
struct Point {
    id: String,
    name: String,
    lat: f64,
    lon: f64,
    altitude_m: Option<f64>,
    category: String,
}

impl Extension for Carte3d {
    fn init(&mut self, ctx: HostContext) -> Result<(), String> {
        self.db_uri = ctx.db_uri;
        self.cache = PathBuf::from(ctx.data_dir).join("relief");
        std::fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
        // Vérifie tout de suite que la base s'ouvre : une erreur ici s'affiche
        // dans les Paramètres plutôt qu'au premier clic.
        self.ouvrir().map(|_| ())
    }

    fn call(&mut self, method: &str, payload: Value) -> Result<Value, String> {
        match method {
            "scene" => self.scene(&payload),
            "texture_data_url" => {
                self.texture_data_url(payload.get("name").and_then(Value::as_str).ok_or("nom de texture manquant")?)
            }
            "ping" => Ok(json!("pong")),
            m => Err(format!("méthode inconnue : {m}")),
        }
    }
}

impl Carte3d {
    fn ouvrir(&self) -> Result<Connection, String> {
        Connection::open_with_flags(
            &self.db_uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("ouverture de la base : {e}"))
    }

    fn points(&self, projet: &str) -> Result<Vec<Point>, String> {
        let c = self.ouvrir()?;
        let mut st = c
            .prepare("SELECT id, name, latitude, longitude, altitude_m, category FROM points WHERE project_id = ?1")
            .map_err(|e| e.to_string())?;
        let lignes = st
            .query_map([projet], |r| {
                Ok(Point {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    lat: r.get(2)?,
                    lon: r.get(3)?,
                    altitude_m: r.get(4)?,
                    category: r.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        lignes.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    fn scene(&self, payload: &Value) -> Result<Value, String> {
        let projet = payload.get("project_id").and_then(Value::as_str).ok_or("aucun projet actif")?;
        let points = self.points(projet)?;
        if points.is_empty() {
            return Err("ce projet n'a encore aucun point".into());
        }
        let (mut s, mut n, mut w, mut e) = (90.0f64, -90.0f64, 180.0f64, -180.0f64);
        for p in &points {
            s = s.min(p.lat);
            n = n.max(p.lat);
            w = w.min(p.lon);
            e = e.max(p.lon);
        }
        // Marge de 15 %, emprise minimale, puis grille aux mailles ~carrées au sol.
        let coslat = ((s + n) / 2.0).to_radians().cos().max(0.2);
        let (clat, clon) = ((s + n) / 2.0, (w + e) / 2.0);
        let dlat = ((n - s) * 1.3).max(EMPRISE_MIN) / 2.0;
        let dlon = ((e - w) * 1.3).max(EMPRISE_MIN / coslat) / 2.0;
        let (s, n, w, e) = (clat - dlat, clat + dlat, clon - dlon, clon + dlon);
        let (haut_km, larg_km) = ((n - s) * 111.32, (e - w) * 111.32 * coslat);
        let (nx, ny) = if larg_km >= haut_km {
            (GRILLE_MAX, ((GRILLE_MAX as f64) * haut_km / larg_km).round().max(16.0) as usize)
        } else {
            (((GRILLE_MAX as f64) * larg_km / haut_km).round().max(16.0) as usize, GRILLE_MAX)
        };

        let (z, source) = self.altitudes(s, n, w, e, nx, ny)?;
        // Le fond topo est facultatif : sans lui, la teinte d'altitude suffit.
        let texture = self.texture(s, n, w, e, larg_km, haut_km).ok();
        Ok(json!({
            "points": points,
            "terrain": { "nx": nx, "ny": ny, "south": s, "north": n, "west": w, "east": e,
                         "width_km": larg_km, "height_km": haut_km, "z": z, "source": source },
            "texture": texture,
        }))
    }

    /// Grille d'altitudes, ligne par ligne du nord au sud, et source utilisée.
    /// Mise en cache par emprise.
    fn altitudes(&self, s: f64, n: f64, w: f64, e: f64, nx: usize, ny: usize) -> Result<(Vec<f32>, String), String> {
        let cle = format!("{VERSION_CACHE}_{s:.4}_{n:.4}_{w:.4}_{e:.4}_{nx}x{ny}.json");
        let fichier = self.cache.join(cle.replace('-', "m"));
        if let Some((z, src)) =
            std::fs::read_to_string(&fichier).ok().and_then(|t| serde_json::from_str::<(Vec<f32>, String)>(&t).ok())
        {
            if z.len() == nx * ny {
                return Ok((z, src));
            }
        }
        // Une requête WMS pour toute la grille ; le service ponctuel en secours.
        let (mut z, source) = match grille_wms(s, n, w, e, nx, ny) {
            Ok(z) => (z, "MNT IGN LiDAR HD".to_string()),
            Err(err_wms) => {
                let z = grille_ponctuelle(s, n, w, e, nx, ny).map_err(|e| format!("{err_wms} ; secours : {e}"))?;
                (z, "IGN RGE ALTI".to_string())
            }
        };
        combler(&mut z);
        let _ = std::fs::write(&fichier, serde_json::to_string(&(&z, &source)).unwrap_or_default());
        Ok((z, source))
    }

    /// Plan IGN v2 sur l'emprise exacte du relief, mis en cache sur disque.
    /// Rend l'URL relative sous laquelle `gwsext` sert le fichier.
    fn texture(&self, s: f64, n: f64, w: f64, e: f64, larg_km: f64, haut_km: f64) -> Result<String, String> {
        let cle = self.fichier_texture(s, n, w, e, larg_km, haut_km)?;
        Ok(format!("_donnees/relief/{cle}"))
    }

    /// Même image en data URL : secours si l'hôte ne sert pas `_donnees/`
    /// (version antérieure de l'application).
    fn texture_data_url(&self, nom: &str) -> Result<Value, String> {
        if nom.contains(['/', '\\']) || nom.contains("..") || !nom.ends_with(".jpg") {
            return Err("nom de texture invalide".into());
        }
        let octets = std::fs::read(self.cache.join(nom)).map_err(|e| format!("texture : {e}"))?;
        use base64::Engine;
        Ok(json!(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(octets))))
    }

    /// Télécharge au besoin le Plan IGN et rend le nom du fichier en cache.
    fn fichier_texture(&self, s: f64, n: f64, w: f64, e: f64, larg_km: f64, haut_km: f64) -> Result<String, String> {
        let (tw, th) = if larg_km >= haut_km {
            (TEXTURE_MAX, ((TEXTURE_MAX as f64) * haut_km / larg_km).round().max(64.0) as usize)
        } else {
            (((TEXTURE_MAX as f64) * larg_km / haut_km).round().max(64.0) as usize, TEXTURE_MAX)
        };
        let cle = format!("topo_{s:.4}_{n:.4}_{w:.4}_{e:.4}_{tw}x{th}.jpg");
        let cle = cle.replace('-', "m");
        let fichier = self.cache.join(&cle);
        match std::fs::metadata(&fichier) {
            Ok(m) if m.len() > 0 => {}
            _ => {
                let url = format!(
                    "{WMS_R}?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS={COUCHE_TOPO}&STYLES=&CRS=EPSG:4326&BBOX={s},{w},{n},{e}&WIDTH={tw}&HEIGHT={th}&FORMAT=image/jpeg"
                );
                let (o, type_mime) = telecharger(&url)?;
                if !type_mime.starts_with("image/jpeg") {
                    return Err(format!("fond topo : réponse {type_mime}"));
                }
                std::fs::write(&fichier, &o).map_err(|e| format!("fond topo : {e}"))?;
            }
        }
        Ok(cle)
    }
}

/// GET avec nouvel essai (3 tentatives, attente croissante) : l'IGN renvoie
/// parfois 429 ou 5xx sous charge, ce qui faisait échouer toute la scène.
fn telecharger(url: &str) -> Result<(Vec<u8>, String), String> {
    let mut derniere = String::new();
    for essai in 0..3u32 {
        if essai > 0 {
            std::thread::sleep(std::time::Duration::from_millis(800 * (1 << essai)));
        }
        match ureq::get(url).header("User-Agent", "GastinelWeatherStudio/carte3d").call() {
            Ok(mut r) => {
                let type_mime = r
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                return r
                    .body_mut()
                    .with_config()
                    .limit(64 * 1024 * 1024)
                    .read_to_vec()
                    .map(|o| (o, type_mime))
                    .map_err(|e| format!("lecture de la réponse IGN : {e}"));
            }
            Err(e) => derniere = format!("IGN injoignable : {e}"),
        }
    }
    Err(derniere)
}

/// Grille nx × ny en une requête WMS BIL 32 bits (petit-boutiste, nord en haut).
/// L'emprise demandée est élargie d'une demi-maille pour que le centre de chaque
/// pixel tombe exactement sur un sommet de la grille.
fn grille_wms(s: f64, n: f64, w: f64, e: f64, nx: usize, ny: usize) -> Result<Vec<f32>, String> {
    let (dy, dx) = ((n - s) / (ny - 1) as f64 / 2.0, (e - w) / (nx - 1) as f64 / 2.0);
    let url = format!(
        "{WMS_R}?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS={COUCHE_MNT}&STYLES=&CRS=EPSG:4326&BBOX={},{},{},{}&WIDTH={nx}&HEIGHT={ny}&FORMAT=image/x-bil;bits=32",
        s - dy,
        w - dx,
        n + dy,
        e + dx
    );
    let (octets, type_mime) = telecharger(&url)?;
    if !type_mime.starts_with("image/x-bil") || octets.len() != nx * ny * 4 {
        return Err(format!("grille LiDAR : réponse {type_mime} de {} octets", octets.len()));
    }
    Ok(octets.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

/// Secours : service ponctuel par lots, au plus [`PARALLELE`] requêtes à la fois.
fn grille_ponctuelle(s: f64, n: f64, w: f64, e: f64, nx: usize, ny: usize) -> Result<Vec<f32>, String> {
    let mut coords = Vec::with_capacity(nx * ny);
    for j in 0..ny {
        let lat = n - (n - s) * j as f64 / (ny - 1) as f64;
        for i in 0..nx {
            coords.push((w + (e - w) * i as f64 / (nx - 1) as f64, lat));
        }
    }
    let lots: Vec<&[(f64, f64)]> = coords.chunks(LOT).collect();
    let mut z = Vec::with_capacity(coords.len());
    for vague in lots.chunks(PARALLELE) {
        let reponses: Vec<Vec<f32>> = std::thread::scope(|sc| {
            let fils: Vec<_> = vague.iter().map(|lot| sc.spawn(move || lot_altitudes(lot))).collect();
            fils.into_iter()
                .zip(vague.iter())
                // Un lot perdu ne fait plus échouer la scène : ses trous sont comblés.
                .map(|(f, lot)| f.join().ok().and_then(Result::ok).unwrap_or_else(|| vec![-99999.0; lot.len()]))
                .collect()
        });
        for r in reponses {
            z.extend(r);
        }
    }
    if z.iter().all(|v| *v <= -1000.0) {
        return Err("aucune altitude reçue".into());
    }
    Ok(z)
}

/// Hors couverture (mer, étranger, lot perdu) : valeurs ≤ -1000 → altitude
/// valide la plus basse, bornée à [-50, 0].
fn combler(z: &mut [f32]) {
    let plancher = z.iter().copied().filter(|v| *v > -1000.0).fold(f32::INFINITY, f32::min);
    let plancher = if plancher.is_finite() { plancher.min(0.0).max(-50.0) } else { 0.0 };
    for v in z.iter_mut() {
        if *v <= -1000.0 {
            *v = plancher;
        }
    }
}

fn lot_altitudes(lot: &[(f64, f64)]) -> Result<Vec<f32>, String> {
    let corps = json!({
        "lon": lot.iter().map(|c| format!("{:.5}", c.0)).collect::<Vec<_>>().join("|"),
        "lat": lot.iter().map(|c| format!("{:.5}", c.1)).collect::<Vec<_>>().join("|"),
        "resource": "ign_rge_alti_wld",
        "zonly": "true",
    });
    let mut derniere = String::new();
    for essai in 0..3u32 {
        if essai > 0 {
            std::thread::sleep(std::time::Duration::from_millis(800 * (1 << essai)));
        }
        let rep: Value = match ureq::post(GEOPF).header("User-Agent", "GastinelWeatherStudio/carte3d").send_json(&corps) {
            Ok(mut r) => match r.body_mut().read_json() {
                Ok(v) => v,
                Err(e) => {
                    derniere = format!("réponse IGN illisible : {e}");
                    continue;
                }
            },
            Err(e) => {
                derniere = format!("altimétrie IGN injoignable : {e}");
                continue;
            }
        };
        let el = rep.get("elevations").and_then(Value::as_array).ok_or("réponse IGN sans « elevations »")?;
        if el.len() != lot.len() {
            return Err(format!("IGN : {} altitudes pour {} points", el.len(), lot.len()));
        }
        return Ok(el.iter().map(|v| v.as_f64().unwrap_or(-99999.0) as f32).collect());
    }
    Err(derniere)
}

declare_extension!(Carte3d);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combler_remplace_les_trous() {
        let mut z = vec![-9999.0, 12.0, 900.0, -99999.0];
        combler(&mut z);
        assert_eq!(z, vec![0.0, 12.0, 900.0, 0.0]);
    }

    #[test]
    #[ignore = "dépend du réseau (Géoplateforme IGN)"]
    fn grille_lidar_reelle_lans_en_vercors() {
        let z = grille_wms(45.09, 45.11, 5.56, 5.58, 20, 20).unwrap();
        assert_eq!(z.len(), 400);
        assert!(z.iter().all(|v| (800.0..1400.0).contains(v)), "{z:?}");
    }

    #[test]
    #[ignore = "dépend du réseau (Géoplateforme IGN)"]
    fn grille_france_entiere_en_une_requete() {
        let z = grille_wms(41.3, 51.1, -5.2, 9.6, 400, 265).unwrap();
        assert_eq!(z.len(), 400 * 265);
        assert!(z.iter().any(|v| *v > 3000.0), "Alpes attendues");
    }
}

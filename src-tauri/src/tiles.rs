//! Cache disque des tuiles de carte (Photo aérienne + TOP25/SCAN25 IGN).
//!
//! ## Pourquoi un cache fichiers, pas SQLite
//!
//! Une tuile est un blob binaire de quelques dizaines de Ko. Les ranger dans
//! `z/x/y.jpg` sous `tiles_cache/<fond>/` est aussi rapide qu'une table SQLite
//! pour ce volume, et surtout **inspectable directement** : on peut compter,
//! effacer ou copier un dossier de zoom avec l'explorateur de fichiers, ce
//! qu'une base ne permet pas sans outillage.
//!
//! ## Pourquoi le téléchargement vit ici, pas en JS
//!
//! Le frontend ne parle jamais au réseau pour les tuiles : il appelle la
//! commande Tauri `get_tile`, qui sert le fichier local s'il existe et ne
//! télécharge que sinon. C'est ce qui rend le mode hors-ligne possible — sans
//! ça, chaque tuile déjà vue redemanderait quand même le réseau à Leaflet.
//!
//! ## La clé TOP25
//!
//! `CLE_TOP25` est la même clé provisoire publique que documentée dans
//! `map.js` (`ign_scan_ws`, fournie par l'IGN, sans garantie de durée). Elle a
//! migré ici parce que c'est désormais ce module, et non plus le JS, qui
//! construit l'URL WMTS appelée.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};

/// Nom du dossier de cache, créé à côté de la base SQLite.
pub const CACHE_DIR_NAME: &str = "tiles_cache";

/// Délai maximal d'un téléchargement de tuile.
///
/// Court, volontairement : en zone blanche, l'utilisateur doit voir une tuile
/// grisée en quelques secondes, pas attendre un timeout réseau long pendant que
/// la carte semble figée. 2s suffit à distinguer « réseau lent » de « pas de
/// réseau du tout » ; au dézoom, Leaflet peut redemander des dizaines de tuiles
/// d'un coup, donc chaque seconde de timeout individuel compte.
const TIMEOUT: StdDuration = StdDuration::from_secs(2);

/// Clé provisoire du TOP25 — voir l'en-tête de ce module et celui de `map.js`.
const CLE_TOP25: &str = "ign_scan_ws";

/// Nombre maximal de niveaux de zoom dans une seule zone de pré-téléchargement
/// (bornes incluses). `4` correspond à « zoom courant ± 2 » : au-delà, le volume
/// de tuiles devient disproportionné pour un usage terrain.
pub const MAX_ZOOM_SPAN: u32 = 4;

/// Nombre maximal de tuiles dans une seule zone de pré-téléchargement.
///
/// Un garde-fou indépendant du zoom span : une zone très étendue à un zoom
/// modéré peut aussi représenter des milliers de tuiles.
pub const MAX_TILES_PER_DOWNLOAD: usize = 4000;

#[derive(Debug, thiserror::Error)]
pub enum TileError {
    #[error("Fond de carte inconnu : « {0} ». Attendu « photo » ou « scan25 ».")]
    UnknownSource(String),

    #[error("Tuile indisponible (hors ligne, ou zone jamais visitée en ligne) : {0}")]
    Unavailable(String),

    #[error("Plage ou zone de téléchargement invalide : {0}")]
    InvalidPlan(String),

    #[error("Erreur d'accès disque : {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Fonds de carte
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TileSource {
    Photo,
    Scan25,
}

impl TileSource {
    pub fn parse(value: &str) -> Result<Self, TileError> {
        match value {
            "photo" => Ok(TileSource::Photo),
            "scan25" => Ok(TileSource::Scan25),
            other => Err(TileError::UnknownSource(other.to_string())),
        }
    }

    fn dir_name(self) -> &'static str {
        match self {
            TileSource::Photo => "photo",
            TileSource::Scan25 => "scan25",
        }
    }

    /// Zoom natif maximal du fond — au-delà, IGN ne sert plus de tuile
    /// (cf. le tableau de mesures dans `map.js`).
    pub fn max_native_zoom(self) -> u32 {
        match self {
            TileSource::Photo => 19,
            TileSource::Scan25 => 16,
        }
    }

    /// URL WMTS d'une tuile, dans le même ordre de paramètres que produisait
    /// `map.js` (voir son historique de test du 18 août 2026).
    fn wmts_url(self, z: u32, x: u32, y: u32) -> String {
        let (base, layer, extra) = match self {
            TileSource::Photo => (
                "https://data.geopf.fr/wmts",
                "HR.ORTHOIMAGERY.ORTHOPHOTOS",
                String::new(),
            ),
            TileSource::Scan25 => (
                "https://data.geopf.fr/private/wmts",
                "GEOGRAPHICALGRIDSYSTEMS.MAPS.SCAN25TOUR",
                format!("apikey={}&", CLE_TOP25),
            ),
        };
        format!(
            "{base}?{extra}SERVICE=WMTS&VERSION=1.0.0&REQUEST=GetTile&LAYER={layer}\
             &STYLE=normal&TILEMATRIXSET=PM&FORMAT=image/jpeg\
             &TILEMATRIX={z}&TILEROW={y}&TILECOL={x}"
        )
    }
}

// ---------------------------------------------------------------------------
// Chemin d'une tuile sur disque
// ---------------------------------------------------------------------------

pub fn tile_path(cache_root: &Path, source: TileSource, z: u32, x: u32, y: u32) -> PathBuf {
    cache_root
        .join(source.dir_name())
        .join(z.to_string())
        .join(x.to_string())
        .join(format!("{y}.jpg"))
}

fn tile_cached(cache_root: &Path, source: TileSource, z: u32, x: u32, y: u32) -> bool {
    tile_path(cache_root, source, z, x, y).is_file()
}

// ---------------------------------------------------------------------------
// Récupération réseau — abstraite pour les tests
// ---------------------------------------------------------------------------

/// Abstraction du téléchargement réseau : la logique cache-first se teste sans
/// jamais toucher le réseau, en fournissant une implémentation factice.
pub trait TileFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Implémentation réelle : un client `reqwest` bloquant, à timeout court.
pub struct HttpTileFetcher {
    client: reqwest::blocking::Client,
}

impl HttpTileFetcher {
    pub fn new() -> Self {
        let client = crate::http_client::client_builder()
            .timeout(TIMEOUT)
            .build()
            .expect("client HTTP tuiles");
        HttpTileFetcher { client }
    }
}

impl Default for HttpTileFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl TileFetcher for HttpTileFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let reponse = self.client.get(url).send().map_err(|e| e.to_string())?;
        let status = reponse.status();
        if !status.is_success() {
            return Err(format!("HTTP {}", status.as_u16()));
        }
        reponse
            .bytes()
            .map(|b| b.to_vec())
            .map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Cache-first + repli réseau
// ---------------------------------------------------------------------------

/// Sert une tuile : disque d'abord, réseau ensuite (et enregistrée sur disque).
///
/// Aucune expiration : une tuile déjà sur disque n'est jamais revalidée. Les
/// fonds IGN ne changent pas assez souvent pour que ce soit un problème, et la
/// disponibilité hors-ligne prime sur la fraîcheur (cf. l'en-tête du module).
pub fn get_tile_cached(
    cache_root: &Path,
    fetcher: &impl TileFetcher,
    source: TileSource,
    z: u32,
    x: u32,
    y: u32,
) -> Result<Vec<u8>, TileError> {
    let path = tile_path(cache_root, source, z, x, y);
    if let Ok(bytes) = fs::read(&path) {
        return Ok(bytes);
    }

    let url = source.wmts_url(z, x, y);
    let bytes = fetcher.fetch(&url).map_err(TileError::Unavailable)?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, &bytes)?;

    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Pré-téléchargement d'une zone
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct TileBounds {
    pub north: f64,
    pub south: f64,
    pub east: f64,
    pub west: f64,
}

/// Projection Web Mercator standard (« slippy map ») longitude → colonne de tuile.
fn tile_x(lon: f64, z: u32) -> u32 {
    let n = 2f64.powi(z as i32);
    (((lon + 180.0) / 360.0) * n).floor().clamp(0.0, n - 1.0) as u32
}

/// Idem pour la latitude → ligne de tuile.
fn tile_y(lat: f64, z: u32) -> u32 {
    let n = 2f64.powi(z as i32);
    let lat_rad = lat.to_radians();
    (((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0) * n)
        .floor()
        .clamp(0.0, n - 1.0) as u32
}

/// Établit la liste des tuiles à télécharger pour une zone, avec les garde-fous
/// de taille (cf. `MAX_ZOOM_SPAN`, `MAX_TILES_PER_DOWNLOAD`).
///
/// Le zoom demandé est plafonné au zoom natif du fond : au-delà, IGN ne sert
/// aucune tuile, les demander ne ferait qu'échouer à coup sûr.
pub fn plan_download(
    source: TileSource,
    bounds: TileBounds,
    min_zoom: u32,
    max_zoom: u32,
) -> Result<Vec<(u32, u32, u32)>, TileError> {
    if max_zoom < min_zoom {
        return Err(TileError::InvalidPlan(
            "le zoom maximal est inférieur au zoom minimal".to_string(),
        ));
    }
    if max_zoom - min_zoom > MAX_ZOOM_SPAN {
        return Err(TileError::InvalidPlan(format!(
            "plage de zoom trop large ({} niveaux, {} au maximum)",
            max_zoom - min_zoom + 1,
            MAX_ZOOM_SPAN + 1
        )));
    }

    let capped_max = max_zoom.min(source.max_native_zoom());
    let mut tiles = Vec::new();
    for z in min_zoom..=capped_max {
        let x_min = tile_x(bounds.west, z);
        let x_max = tile_x(bounds.east, z);
        let y_min = tile_y(bounds.north, z);
        let y_max = tile_y(bounds.south, z);
        for x in x_min..=x_max {
            for y in y_min..=y_max {
                tiles.push((z, x, y));
            }
        }
    }

    if tiles.len() > MAX_TILES_PER_DOWNLOAD {
        return Err(TileError::InvalidPlan(format!(
            "zone trop grande : {} tuiles à télécharger ({} au maximum) — \
             réduisez la zone ou la plage de zoom",
            tiles.len(),
            MAX_TILES_PER_DOWNLOAD
        )));
    }

    Ok(tiles)
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct DownloadSummary {
    pub total: usize,
    pub downloaded: usize,
    pub already_cached: usize,
    pub failed: usize,
}

/// Télécharge un plan de tuiles, en sautant celles déjà en cache et en
/// notifiant la progression à chaque tuile traitée (téléchargée ou non).
pub fn download_plan(
    cache_root: &Path,
    fetcher: &impl TileFetcher,
    source: TileSource,
    tiles: &[(u32, u32, u32)],
    mut on_progress: impl FnMut(usize, usize),
) -> DownloadSummary {
    let total = tiles.len();
    let mut summary = DownloadSummary {
        total,
        ..Default::default()
    };

    for (i, &(z, x, y)) in tiles.iter().enumerate() {
        if tile_cached(cache_root, source, z, x, y) {
            summary.already_cached += 1;
        } else {
            match get_tile_cached(cache_root, fetcher, source, z, x, y) {
                Ok(_) => summary.downloaded += 1,
                Err(_) => summary.failed += 1,
            }
        }
        on_progress(i + 1, total);
    }

    summary
}

// ---------------------------------------------------------------------------
// Taille du cache — écran Paramètres
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct SourceCacheInfo {
    pub tile_count: u64,
    pub bytes: u64,
}

pub fn source_cache_info(cache_root: &Path, source: TileSource) -> SourceCacheInfo {
    let mut info = SourceCacheInfo::default();
    walk_dir(&cache_root.join(source.dir_name()), &mut info);
    info
}

fn walk_dir(dir: &Path, info: &mut SourceCacheInfo) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, info);
        } else if let Ok(meta) = entry.metadata() {
            info.tile_count += 1;
            info.bytes += meta.len();
        }
    }
}

/// Vide entièrement le cache, tous fonds confondus.
///
/// Pas de suppression sélective par fond à ce stade : l'écran Paramètres ne
/// propose qu'un seul bouton, cohérent avec la spécification (pas de gestion
/// de quota, juste l'affichage de la taille et une remise à zéro).
pub fn clear_cache(cache_root: &Path) -> std::io::Result<()> {
    if cache_root.is_dir() {
        fs::remove_dir_all(cache_root)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Répertoire de test isolé, nettoyé à la fin — pas de dépendance à
    /// `tempfile` : le reste du projet n'en a pas besoin non plus.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            static COMPTEUR: AtomicUsize = AtomicUsize::new(0);
            let n = COMPTEUR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("gws_tiles_test_{label}_{n}"));
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Fetcher factice : compte ses appels et répond ce qu'on lui a dit,
    /// pour vérifier cache-first et le repli sur échec sans jamais toucher au
    /// réseau.
    struct FakeFetcher {
        calls: Mutex<usize>,
        reponse: Result<Vec<u8>, String>,
    }

    impl FakeFetcher {
        fn ok(bytes: Vec<u8>) -> Self {
            FakeFetcher {
                calls: Mutex::new(0),
                reponse: Ok(bytes),
            }
        }
        fn err(message: &str) -> Self {
            FakeFetcher {
                calls: Mutex::new(0),
                reponse: Err(message.to_string()),
            }
        }
        fn call_count(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    impl TileFetcher for FakeFetcher {
        fn fetch(&self, _url: &str) -> Result<Vec<u8>, String> {
            *self.calls.lock().unwrap() += 1;
            self.reponse.clone()
        }
    }

    // -------------------------------------------------------------------
    // Construction des chemins z/x/y
    // -------------------------------------------------------------------

    #[test]
    fn le_chemin_separe_bien_les_deux_fonds() {
        let racine = PathBuf::from("tiles_cache");
        let photo = tile_path(&racine, TileSource::Photo, 12, 34, 56);
        let scan25 = tile_path(&racine, TileSource::Scan25, 12, 34, 56);

        assert_eq!(photo, racine.join("photo").join("12").join("34").join("56.jpg"));
        assert_eq!(scan25, racine.join("scan25").join("12").join("34").join("56.jpg"));
        assert_ne!(photo, scan25);
    }

    #[test]
    fn le_timeout_reseau_reste_court() {
        // Garde-fou de non-régression : un timeout trop long redevient
        // perceptible au dézoom sur une zone non cachée (voir l'en-tête de
        // la constante). 2s max.
        assert!(TIMEOUT <= StdDuration::from_secs(2));
    }

    #[test]
    fn une_source_inconnue_est_rejetee() {
        assert!(TileSource::parse("cadastre").is_err());
        assert!(TileSource::parse("photo").is_ok());
        assert!(TileSource::parse("scan25").is_ok());
    }

    // -------------------------------------------------------------------
    // Cache-first / repli réseau
    // -------------------------------------------------------------------

    #[test]
    fn une_tuile_deja_en_cache_ne_declenche_aucun_appel_reseau() {
        let dir = TempDir::new("cache_hit");
        let path = tile_path(&dir.0, TileSource::Photo, 10, 5, 5);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"tuile-existante").unwrap();

        let fetcher = FakeFetcher::err("le reseau ne doit jamais etre appele");
        let bytes = get_tile_cached(&dir.0, &fetcher, TileSource::Photo, 10, 5, 5).unwrap();

        assert_eq!(bytes, b"tuile-existante");
        assert_eq!(fetcher.call_count(), 0);
    }

    #[test]
    fn une_tuile_absente_est_telechargee_puis_ecrite_sur_disque() {
        let dir = TempDir::new("cache_miss");
        let fetcher = FakeFetcher::ok(b"tuile-telechargee".to_vec());

        let bytes = get_tile_cached(&dir.0, &fetcher, TileSource::Scan25, 8, 1, 2).unwrap();
        assert_eq!(bytes, b"tuile-telechargee");
        assert_eq!(fetcher.call_count(), 1);

        // Écrite au bon endroit, et un second appel la sert désormais du disque.
        let chemin = tile_path(&dir.0, TileSource::Scan25, 8, 1, 2);
        assert!(chemin.is_file());

        let bytes2 = get_tile_cached(&dir.0, &fetcher, TileSource::Scan25, 8, 1, 2).unwrap();
        assert_eq!(bytes2, b"tuile-telechargee");
        assert_eq!(fetcher.call_count(), 1, "le second appel doit venir du disque");
    }

    #[test]
    fn un_echec_reseau_ne_cree_aucun_fichier_et_remonte_une_erreur() {
        let dir = TempDir::new("cache_fail");
        let fetcher = FakeFetcher::err("zone blanche");

        let resultat = get_tile_cached(&dir.0, &fetcher, TileSource::Photo, 9, 3, 3);
        assert!(resultat.is_err());
        assert!(!tile_path(&dir.0, TileSource::Photo, 9, 3, 3).exists());
    }

    // -------------------------------------------------------------------
    // Planification du pré-téléchargement de zone
    // -------------------------------------------------------------------

    #[test]
    fn le_plan_couvre_la_boite_englobante_sur_chaque_zoom() {
        let bounds = TileBounds {
            north: 45.10,
            south: 45.00,
            east: 5.55,
            west: 5.40,
        };
        let plan = plan_download(TileSource::Photo, bounds, 10, 11).unwrap();

        let zooms: std::collections::HashSet<u32> = plan.iter().map(|(z, _, _)| *z).collect();
        assert_eq!(zooms, [10, 11].into_iter().collect());
        assert!(!plan.is_empty());
    }

    #[test]
    fn une_plage_de_zoom_trop_large_est_refusee() {
        let bounds = TileBounds {
            north: 45.10,
            south: 45.00,
            east: 5.55,
            west: 5.40,
        };
        // Span de 5 (10..=15), au-delà de MAX_ZOOM_SPAN (4) : refusé.
        let resultat = plan_download(TileSource::Photo, bounds, 10, 15);
        assert!(resultat.is_err());
    }

    #[test]
    fn le_zoom_est_plafonne_au_zoom_natif_du_fond() {
        let bounds = TileBounds {
            north: 45.10,
            south: 45.00,
            east: 5.55,
            west: 5.40,
        };
        // Le SCAN25 s'arrête à 16 : demander jusqu'à 18 ne doit produire aucune
        // tuile au-delà de 16, jamais une tentative de tuile inexistante.
        let plan = plan_download(TileSource::Scan25, bounds, 15, 18).unwrap();
        assert!(plan.iter().all(|(z, _, _)| *z <= 16));
    }

    #[test]
    fn une_zone_avec_trop_de_tuiles_est_refusee() {
        // Le monde entier à un zoom élevé : bien au-delà du plafond.
        let bounds = TileBounds {
            north: 60.0,
            south: -60.0,
            east: 170.0,
            west: -170.0,
        };
        let resultat = plan_download(TileSource::Photo, bounds, 10, 10);
        assert!(resultat.is_err());
    }

    // -------------------------------------------------------------------
    // Téléchargement d'un plan
    // -------------------------------------------------------------------

    #[test]
    fn le_telechargement_de_plan_saute_les_tuiles_deja_en_cache() {
        let dir = TempDir::new("plan_download");
        let deja_la = tile_path(&dir.0, TileSource::Photo, 12, 0, 0);
        fs::create_dir_all(deja_la.parent().unwrap()).unwrap();
        fs::write(&deja_la, b"deja-en-cache").unwrap();

        let fetcher = FakeFetcher::ok(b"nouvelle-tuile".to_vec());
        let plan = vec![(12, 0, 0), (12, 0, 1), (12, 1, 0)];

        let mut appels_progression = Vec::new();
        let resume = download_plan(&dir.0, &fetcher, TileSource::Photo, &plan, |fait, total| {
            appels_progression.push((fait, total));
        });

        assert_eq!(resume.total, 3);
        assert_eq!(resume.already_cached, 1);
        assert_eq!(resume.downloaded, 2);
        assert_eq!(resume.failed, 0);
        assert_eq!(fetcher.call_count(), 2);
        assert_eq!(appels_progression, vec![(1, 3), (2, 3), (3, 3)]);
    }

    #[test]
    fn le_telechargement_de_plan_compte_les_echecs_sans_interrompre() {
        let dir = TempDir::new("plan_partial_fail");
        let fetcher = FakeFetcher::err("panne");
        let plan = vec![(5, 0, 0), (5, 0, 1)];

        let resume = download_plan(&dir.0, &fetcher, TileSource::Photo, &plan, |_, _| {});

        assert_eq!(resume.total, 2);
        assert_eq!(resume.failed, 2);
        assert_eq!(resume.downloaded, 0);
    }

    // -------------------------------------------------------------------
    // Taille du cache et remise à zéro
    // -------------------------------------------------------------------

    #[test]
    fn la_taille_du_cache_compte_les_deux_fonds_separement() {
        let dir = TempDir::new("cache_info");
        let a = tile_path(&dir.0, TileSource::Photo, 1, 0, 0);
        let b = tile_path(&dir.0, TileSource::Scan25, 1, 0, 0);
        fs::create_dir_all(a.parent().unwrap()).unwrap();
        fs::create_dir_all(b.parent().unwrap()).unwrap();
        fs::write(&a, [0u8; 100]).unwrap();
        fs::write(&b, [0u8; 250]).unwrap();

        let photo = source_cache_info(&dir.0, TileSource::Photo);
        let scan25 = source_cache_info(&dir.0, TileSource::Scan25);

        assert_eq!(photo.tile_count, 1);
        assert_eq!(photo.bytes, 100);
        assert_eq!(scan25.tile_count, 1);
        assert_eq!(scan25.bytes, 250);
    }

    #[test]
    fn vider_le_cache_supprime_tout_et_ne_plante_pas_si_absent() {
        let dir = TempDir::new("cache_clear");
        let a = tile_path(&dir.0, TileSource::Photo, 1, 0, 0);
        fs::create_dir_all(a.parent().unwrap()).unwrap();
        fs::write(&a, b"x").unwrap();

        clear_cache(&dir.0).unwrap();
        assert!(!dir.0.exists());

        // Un second appel, sur un dossier déjà absent, ne doit pas échouer.
        clear_cache(&dir.0).unwrap();
    }
}

//! Couche commandes Tauri : le seul point de contact entre le frontend et le
//! métier. Elle est volontairement mince — elle traduit, elle ne décide pas.
//!
//! Deux conventions :
//!
//! * Les commandes à plusieurs paramètres utilisent `rename_all = "snake_case"` :
//!   le JS envoie exactement les mêmes noms que la signature Rust, ce qui évite
//!   la conversion implicite camelCase/snake_case et les bugs muets qui vont avec.
//! * Le **contenu des fichiers arrive déjà décodé en texte depuis le frontend**
//!   (`FileReader` + `TextDecoder`, avec gestion du BOM UTF-8/UTF-16 et repli
//!   Windows-1252). Conséquence : aucun plugin `dialog`/`fs` n'est nécessaire, et
//!   `raw_file_reference` ne contient que le **nom** du fichier, pas son chemin
//!   absolu — le navigateur ne l'expose pas. Si la traçabilité du chemin complet
//!   devient nécessaire, il faudra basculer sur `tauri-plugin-dialog`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::anomaly::{detect_jumps_raw, JumpThresholds, TemperatureJump};
use crate::api_keys::{self, KeyStatus};
use crate::crossing::{self, CrossAnalysis, TimeWindow};
use crate::db::Db;
use crate::export;
use crate::elitech_import::{
    parse_elitech_export_detailed, DetectedLayout, ExportMetadata, ImportWarning, ParsedRows,
    RawMeasurement,
};
use crate::elitech_serial::{self, Ch340Port, ClockCheck, DevInfo, Rc5Reading, ReadWarning};
use crate::elitech_watch::WatchHandle;
use crate::elitech_xlsx::{parse_elitech_xlsx, ElitechXlsxExport, XlsxMetadata};
use crate::error::{AppError, AppResult};
use crate::fingerprint::{series_fingerprint_of, FingerprintBuilder};
use crate::infoclimat::{
    self, infoclimat_date_evidence, parse_infoclimat_auto, InfoclimatFormat, InfoclimatPayload,
    InfoclimatRecord, StationInfo,
};
use crate::groq;
use crate::infoclimat_client::{self, InfoclimatClient};
use crate::manual_entry::{parse_manual_entry, ManualEntryInput};
use crate::manual_import::{parse_manual, preview_delimited as preview_impl, DelimitedPreview, ManualMapping};
use crate::model::{
    apply_clock_offset, now_utc, Category, LoggerSeries, Measurement, NewPoint, Point, Project, SeriesRole,
    SourceType,
};
use crate::parsing::{DateOrder, DateOrderEvidence};
use crate::romma_import::{
    lire_romma, romma_date_evidence, RommaApercu, RommaFieldCounts, RommaLecture, RommaPayload,
    RommaRecord, ROMMA_SAMPLE_LEN,
};
use crate::stats::{period_stats, period_stats_from_days, PeriodStats, TempSample};
use crate::tiles::{self, HttpTileFetcher, TileBounds, TileSource};

/// État applicatif partagé, injecté par `tauri::Builder::manage`.
pub struct AppState {
    pub db: Db,

    /// Poignée du fil de surveillance du RC-5 (voir [`crate::elitech_watch`]).
    ///
    /// `Option` parce que la surveillance peut ne pas avoir démarré — et
    /// l'application doit fonctionner sans elle : l'import par fichier n'a que
    /// faire d'un port série.
    pub rc5_watch: Mutex<Option<WatchHandle>>,

    /// Les lectures directes en cours, **une par port**.
    ///
    /// Une carte et non un `Option` : la page Elitech ouvre un onglet par
    /// logger branché, et chacun lit le sien. Avec une lecture unique, lire le
    /// second effacerait le jeton du premier — son export et son
    /// enregistrement repartiraient alors relire l'appareil, ou échoueraient.
    /// La clé est le nom du port : un logger n'a qu'une lecture vivante à la
    /// fois, la relire remplace la précédente plutôt que de l'empiler.
    ///
    /// Un `Mutex` et non un `RwLock` : deux accès par import, aucune contention
    /// à espérer d'un verrou plus subtil.
    rc5_reading: Mutex<HashMap<String, Rc5Cache>>,

    /// Racine du cache de tuiles (`tiles_cache/photo`, `tiles_cache/scan25`).
    pub tiles_cache_dir: PathBuf,

    /// Client HTTP des tuiles, construit une seule fois — la carte en appelle
    /// bien plus souvent qu'Infoclimat ne parle au réseau.
    pub tile_fetcher: HttpTileFetcher,

    /// Chemin d'un paquet .gws passé en argument de ligne de commande au démarrage.
    pub startup_package: Mutex<Option<String>>,
    /// Lancement qui suit l'application d'un paquet `.gws` (écran de confirmation).
    pub apres_maj: Mutex<bool>,
    /// Annuler / Rétablir des suppressions, le temps de la session.
    pub historique: crate::annulation::Historique,
}

impl AppState {
    pub fn new(db: Db, tiles_cache_dir: PathBuf) -> Self {
        AppState {
            db,
            rc5_watch: Mutex::new(None),
            rc5_reading: Mutex::new(HashMap::new()),
            tiles_cache_dir,
            tile_fetcher: HttpTileFetcher::new(),
            startup_package: Mutex::new(None),
            apres_maj: Mutex::new(false),
            historique: Default::default(),
        }
    }
}

/// Nombre maximum d'avertissements renvoyés à l'UI (le reste est compté, pas listé).
const MAX_WARNINGS_RETURNED: usize = 100;

// ---------------------------------------------------------------------------
// Projets
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> AppResult<Vec<Project>> {
    state.db.list_projects()
}

#[tauri::command]
pub fn create_project(state: State<'_, AppState>, name: String) -> AppResult<Project> {
    state.db.create_project(&name)
}

/// Un projet qu'on vient d'ouvrir, **plus la date de l'ouverture précédente**.
///
/// `touch_project` écrase `last_opened_at` avec l'instant courant : sans cette
/// capture, le `{last_opened}` de l'écran d'accueil afficherait invariablement
/// « à l'instant ». Le champ `project` est aplati, donc le frontend continue de
/// lire `project.name` et `project.point_count` sans rien changer.
#[derive(Debug, Clone, Serialize)]
pub struct OpenedProject {
    #[serde(flatten)]
    pub project: Project,
    /// Ouverture précédente, ou `null` si le projet n'avait jamais été ouvert
    /// depuis sa création.
    pub previous_opened_at: Option<DateTime<Utc>>,
}

#[tauri::command(rename_all = "snake_case")]
pub fn open_project(state: State<'_, AppState>, project_id: String) -> AppResult<OpenedProject> {
    let avant = state.db.get_project(&project_id)?;
    // Un projet jamais rouvert depuis sa création n'a pas d'« ouverture
    // précédente » : `created_at == last_opened_at` est le marqueur.
    let previous_opened_at = (avant.last_opened_at != avant.created_at).then_some(avant.last_opened_at);

    Ok(OpenedProject {
        project: state.db.touch_project(&project_id)?,
        previous_opened_at,
    })
}

// ---------------------------------------------------------------------------
// Points
// ---------------------------------------------------------------------------

/// Options de classification servies au frontend.
///
/// L'UI ne code **aucune** règle catégorie/taille en dur : elle construit ses
/// listes déroulantes à partir de cette réponse. Source de vérité unique côté Rust.
#[derive(Debug, Clone, Serialize)]
pub struct CategoryOption {
    pub value: String,
    pub label: String,
    /// `null` représente « pas de taille ».
    pub allowed_sizes: Vec<Option<String>>,
    pub requires_size: bool,
}

#[tauri::command]
pub fn category_options() -> Vec<CategoryOption> {
    Category::ALL
        .iter()
        .map(|c| CategoryOption {
            value: c.as_str().to_string(),
            label: c.label_fr().to_string(),
            allowed_sizes: c
                .allowed_sizes()
                .iter()
                .map(|s| s.map(|s| s.as_str().to_string()))
                .collect(),
            requires_size: c.requires_size(),
        })
        .collect()
}

#[tauri::command(rename_all = "snake_case")]
pub fn list_points(state: State<'_, AppState>, project_id: String) -> AppResult<Vec<Point>> {
    state.db.list_points(&project_id)
}

#[tauri::command]
pub fn create_point(state: State<'_, AppState>, payload: NewPoint) -> AppResult<Point> {
    // La validation catégorie/taille est faite ici, par le modèle.
    let point = Point::from_new(payload)?;
    state.db.insert_point(&point)?;
    state.db.get_point(&point.id)
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_point(state: State<'_, AppState>, point_id: String) -> AppResult<Point> {
    state.db.get_point(&point_id)
}

/// Modifie un point existant.
///
/// Le `project_id` du payload est **ignoré** au profit de celui du point en base :
/// le formulaire l'envoie par commodité, mais changer un point de projet n'est pas
/// une opération d'édition. L'identité (`id`, `created_at`) est préservée.
#[tauri::command(rename_all = "snake_case")]
pub fn update_point(
    state: State<'_, AppState>,
    point_id: String,
    payload: NewPoint,
) -> AppResult<Point> {
    let existant = state.db.get_point(&point_id)?;

    let mut payload = payload;
    payload.project_id = existant.project_id.clone();

    // Même porte de validation que la création : aucune règle ne peut être oubliée.
    let point = Point::from_parts(existant.id, existant.created_at, payload)?;
    state.db.update_point(&point)?;
    state.db.get_point(&point.id)
}

/// Supprime un projet, ses points, séries et mesures (confirmation côté interface).
#[tauri::command(rename_all = "snake_case")]
/// Renvoie `true` si la suppression peut être annulée (Ctrl+Z).
pub fn delete_project(state: State<'_, AppState>, project_id: String) -> AppResult<bool> {
    crate::annulation::supprimer_projet(&state.db, &state.historique, &project_id)
}

/// Nombre de mesures d'un projet, pour prévenir qu'une suppression trop
/// grosse ne pourra pas être annulée.
#[tauri::command(rename_all = "snake_case")]
pub fn project_measurement_count(state: State<'_, AppState>, project_id: String) -> AppResult<serde_json::Value> {
    let n = crate::annulation::mesures_du_projet(&state.db, &project_id)?;
    Ok(serde_json::json!({ "count": n, "undoable": n <= crate::annulation::MAX_MESURES_CONSERVEES }))
}

/// Crée le projet d'exemple (données synthétiques, jamais synchronisé).
/// Appelé seulement par le bouton « Explorer un projet d'exemple ».
#[tauri::command(async)]
pub fn create_demo_project(state: State<'_, AppState>) -> AppResult<Project> {
    crate::demo::creer_projet_exemple(&state.db)
}

/// Onglet d'import deviné pour un fichier déposé sur la fenêtre.
#[tauri::command(rename_all = "snake_case")]
pub fn detect_import_format(file_name: String, is_zip: bool, text: String) -> crate::import_detect::FormatDetecte {
    crate::import_detect::detecter(&file_name, is_zip, &text)
}

/// Supprime un point, ses séries et toutes leurs mesures associées.
#[tauri::command(rename_all = "snake_case")]
pub fn delete_point(state: State<'_, AppState>, point_id: String) -> AppResult<()> {
    crate::annulation::supprimer_point(&state.db, &state.historique, &point_id)
}

// ---------------------------------------------------------------------------
// Données d'une station : voir, corriger, supprimer (annulable)
// ---------------------------------------------------------------------------

/// Une page de mesures d'une série, la plus récente d'abord.
#[tauri::command(rename_all = "snake_case")]
pub fn series_measurements_page(
    state: State<'_, AppState>,
    series_id: String,
    offset: usize,
    limit: usize,
) -> AppResult<Vec<Measurement>> {
    state.db.measurements_page(&series_id, offset, limit.min(500))
}

/// Trous d'une série : `[début, fin]` en secondes UTC, les plus longs d'abord.
#[tauri::command(rename_all = "snake_case")]
pub fn series_gaps(state: State<'_, AppState>, series_id: String) -> AppResult<Vec<(i64, i64)>> {
    let instants: Vec<i64> = state
        .db
        .temperature_samples_for_series(&series_id)?
        .into_iter()
        .map(|t| t.timestamp_utc.timestamp())
        .collect();
    Ok(trous(&instants, 50))
}

/// Écarts anormaux entre mesures successives : plus de 3 fois le pas
/// habituel (médiane), et au moins une heure.
pub(crate) fn trous(instants: &[i64], max: usize) -> Vec<(i64, i64)> {
    if instants.len() < 3 {
        return Vec::new();
    }
    let mut pas: Vec<i64> = instants.windows(2).map(|w| w[1] - w[0]).filter(|d| *d > 0).collect();
    if pas.is_empty() {
        return Vec::new();
    }
    pas.sort_unstable();
    let seuil = (pas[pas.len() / 2] * 3).max(3600);
    let mut t: Vec<(i64, i64)> = instants
        .windows(2)
        .filter(|w| w[1] - w[0] > seuil)
        .map(|w| (w[0], w[1]))
        .collect();
    t.sort_by_key(|(a, b)| std::cmp::Reverse(b - a));
    t.truncate(max);
    t
}

/// Un import du projet (= une série) pour le panneau « Imports ».
#[derive(Debug, Serialize)]
pub struct ImportLigne {
    pub series: LoggerSeries,
    pub point_name: String,
    /// Première et dernière mesure en base (UTC).
    pub first_utc: Option<chrono::DateTime<chrono::Utc>>,
    pub last_utc: Option<chrono::DateTime<chrono::Utc>>,
    /// Récupérable par l'API (Infoclimat rattachée au point).
    pub refreshable: bool,
}

/// Tous les imports d'un projet, le plus récent d'abord.
///
/// « Anciennes données » = depuis `first_utc` ; « dernières données » =
/// jusqu'à `last_utc` ; `series.last_api_fetch_at` = dernier appel à l'API,
/// qui n'est pas la même chose que la dernière mesure reçue.
#[tauri::command(async, rename_all = "snake_case")]
pub fn list_project_imports(state: State<'_, AppState>, project_id: String) -> AppResult<Vec<ImportLigne>> {
    let mut lignes = Vec::new();
    for point in state.db.list_points(&project_id)? {
        for series in state.db.list_series_for_point(&point.id)? {
            let (first_utc, last_utc) = state.db.series_bounds_utc(&series.id)?;
            let refreshable = series.source_type == SourceType::InfoclimatApi
                && point.infoclimat_station_id.is_some();
            lignes.push(ImportLigne { series, point_name: point.name.clone(), first_utc, last_utc, refreshable });
        }
    }
    lignes.sort_by(|a, b| b.series.imported_at.cmp(&a.series.imported_at));
    Ok(lignes)
}

#[tauri::command(rename_all = "snake_case")]
pub fn delete_series(state: State<'_, AppState>, series_id: String) -> AppResult<()> {
    crate::annulation::supprimer_serie(&state.db, &state.historique, &series_id)
}

/// Supprime les mesures d'une série entre deux instants (secondes UTC, inclus).
#[tauri::command(rename_all = "snake_case")]
pub fn delete_measurements_range(
    state: State<'_, AppState>,
    series_id: String,
    from_utc: i64,
    to_utc: i64,
) -> AppResult<usize> {
    crate::annulation::supprimer_plage(&state.db, &state.historique, &series_id, from_utc, to_utc)
}

#[tauri::command(rename_all = "snake_case")]
pub fn delete_measurements(state: State<'_, AppState>, series_id: String, ids: Vec<i64>) -> AppResult<usize> {
    crate::annulation::supprimer_mesures(&state.db, &state.historique, &series_id, &ids)
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_measurement_temperature(
    state: State<'_, AppState>,
    series_id: String,
    id: i64,
    celsius: f64,
) -> AppResult<()> {
    crate::annulation::modifier_temperature(&state.db, &state.historique, &series_id, id, celsius)
}

/// Bandeau du quota partagé : `null` si le relai n'est pas activé.
#[tauri::command(rename_all = "snake_case")]
pub async fn quota_partage(state: State<'_, AppState>, forcer: bool) -> AppResult<Option<crate::quota_partage::EtatQuota>> {
    crate::quota_partage::lire(&state.db, forcer).await
}

#[tauri::command]
pub fn appareil_identite(state: State<'_, AppState>) -> AppResult<crate::quota_partage::IdentiteAppareil> {
    crate::quota_partage::identite(&state.db)
}

#[tauri::command(rename_all = "snake_case")]
pub fn appareil_changer(
    state: State<'_, AppState>,
    nom: String,
    couleur: u8,
) -> AppResult<crate::quota_partage::IdentiteAppareil> {
    crate::quota_partage::changer_identite(&state.db, &nom, couleur)
}

#[tauri::command]
pub fn undo_status(state: State<'_, AppState>) -> crate::annulation::EtatAnnulation {
    state.historique.etat()
}

/// Ctrl+Z : renvoie le libellé de l'action défaite, `null` s'il n'y en a pas.
#[tauri::command]
pub fn undo_last(state: State<'_, AppState>) -> AppResult<Option<String>> {
    state.historique.annuler(&state.db)
}

/// Ctrl+Y.
#[tauri::command]
pub fn redo_last(state: State<'_, AppState>) -> AppResult<Option<String>> {
    state.historique.retablir(&state.db)
}

// ---------------------------------------------------------------------------
// Séries et statistiques
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub fn list_series(state: State<'_, AppState>, point_id: String) -> AppResult<Vec<LoggerSeries>> {
    state.db.list_series_for_point(&point_id)
}

#[tauri::command(rename_all = "snake_case")]
pub fn series_stats(state: State<'_, AppState>, series_id: String) -> AppResult<PeriodStats> {
    // Seules l'heure et la température servent : pas de `Measurement` complet.
    Ok(period_stats_from_days(&state.db.series_days(&series_id)?))
}

// ---------------------------------------------------------------------------
// Croisement de données
// ---------------------------------------------------------------------------

/// Statistiques et courbes d'une sélection de points sur une fenêtre commune.
///
/// Commande volontairement squelettique : tout est dans [`crossing::build`],
/// qui ne dépend ni de Tauri ni du DOM et sera réutilisé tel quel par l'export.
///
/// Aucun plafond sur `point_ids` — contrairement à l'agent, qui tronque faute de
/// budget de tokens. Ici, au-delà de `crossing::SEUIL_LISIBILITE` courbes, le
/// résultat porte un avertissement de lisibilité et rien n'est écarté : c'est
/// l'utilisateur qui juge de ce qu'il arrive encore à lire.
#[tauri::command(rename_all = "snake_case")]
pub fn cross_analysis(
    state: State<'_, AppState>,
    point_ids: Vec<String>,
    window: TimeWindow,
) -> AppResult<CrossAnalysis> {
    crossing::build(&state.db, &point_ids, window)
}

/// Bilan mois par mois des stations choisies, pour la vue « Croisement en
/// plein écran » (voir `croisement_mensuel.rs`). Dates UTC incluses.
#[tauri::command(rename_all = "snake_case")]
pub fn cross_monthly(
    state: State<'_, AppState>,
    point_ids: Vec<String>,
    du: chrono::NaiveDate,
    au: chrono::NaiveDate,
    seuils: crate::croisement_mensuel::Seuils,
) -> AppResult<crate::croisement_mensuel::BilanMensuel> {
    crate::croisement_mensuel::construire(&state.db, &point_ids, du, au, seuils)
}

/// Applique un nouveau décalage d'horloge à une série déjà importée.
/// Les horodatages UTC sont recalculés depuis les horodatages bruts.
#[tauri::command(rename_all = "snake_case")]
pub fn recalibrate_series(
    state: State<'_, AppState>,
    series_id: String,
    clock_offset_seconds: i64,
) -> AppResult<usize> {
    state.db.recalibrate_series(&series_id, clock_offset_seconds)
}

/// Bilan d'une résolution d'ambiguïté de date.
#[derive(Debug, Clone, Serialize)]
pub struct DateOrderFixReport {
    pub series_id: String,
    /// Ordre en vigueur après l'opération.
    pub date_order: DateOrder,
    /// Toujours `false` en sortie : le doute est levé dans les deux cas.
    pub date_format_uncertain: bool,
    /// Nombre de mesures réécrites (0 pour une simple confirmation).
    pub updated_measurements: usize,
    /// Période couverte **après** correction — c'est ce qui permet à l'utilisateur
    /// de vérifier d'un coup d'œil que les dates sont retombées où il les attend.
    pub period_start_utc: Option<DateTime<Utc>>,
    pub period_end_utc: Option<DateTime<Utc>>,
}

/// Tranche l'ambiguïté jour/mois d'une série **déjà importée**.
///
/// * `swap = false` — l'utilisateur confirme la lecture actuelle. Rien n'est
///   recalculé, seul le drapeau tombe.
/// * `swap = true` — jour et mois sont permutés sur toute la série :
///   `raw_timestamp` est réécrit **et** `timestamp_utc` re-dérivé. Aucun réimport.
#[tauri::command(rename_all = "snake_case")]
pub fn resolve_series_date_order(
    state: State<'_, AppState>,
    series_id: String,
    swap: bool,
) -> AppResult<DateOrderFixReport> {
    let updated_measurements = if swap {
        state.db.swap_series_date_order(&series_id)?
    } else {
        state.db.confirm_series_date_order(&series_id)?;
        0
    };

    let series = state.db.get_series(&series_id)?;
    let stats = period_stats_from_days(&state.db.series_days(&series_id)?);

    Ok(DateOrderFixReport {
        series_id,
        date_order: series.date_order,
        date_format_uncertain: series.date_format_uncertain,
        updated_measurements,
        period_start_utc: stats.first_utc,
        period_end_utc: stats.last_utc,
    })
}

// ---------------------------------------------------------------------------
// Import — analyse préalable (aucune écriture en base)
// ---------------------------------------------------------------------------

/// Ce que l'auto-détection a compris du fichier, avant tout enregistrement.
/// L'utilisateur valide (ou bascule sur l'import manuel) au vu de ce résumé.
#[derive(Debug, Clone, Serialize)]
pub struct ElitechAnalysis {
    pub layout: DetectedLayout,
    pub metadata: ExportMetadata,
    pub total_lines: usize,
    pub measurement_count: usize,
    pub warning_count: usize,
    pub warnings: Vec<ImportWarning>,
    /// Bornes **brutes** (non corrigées) de la période couverte.
    pub raw_period_start: Option<NaiveDateTime>,
    pub raw_period_end: Option<NaiveDateTime>,
    /// Quelques lignes parsées, pour contrôle visuel.
    pub sample: Vec<RawMeasurement>,
    /// Ce que le fichier prouve de son ordre jour/mois — affiché **avant**
    /// l'import, pour que l'utilisateur juge sur pièces.
    pub date_order: DateOrderEvidence,
}

#[tauri::command]
pub fn analyze_elitech(content: String) -> AppResult<ElitechAnalysis> {
    let export = parse_elitech_export_detailed(&content)?;
    let (start, end) = raw_bounds(&export.measurements);

    Ok(ElitechAnalysis {
        layout: export.layout,
        metadata: export.metadata,
        total_lines: export.total_lines,
        measurement_count: export.measurements.len(),
        warning_count: export.warnings.len(),
        warnings: export.warnings.into_iter().take(MAX_WARNINGS_RETURNED).collect(),
        raw_period_start: start,
        raw_period_end: end,
        sample: export.measurements.into_iter().take(5).collect(),
        date_order: export.date_order,
    })
}

/// Aperçu tabulé d'un contenu quelconque, pour l'écran de mappage manuel.
#[tauri::command(rename_all = "snake_case")]
pub fn preview_delimited(
    content: String,
    delimiter: Option<String>,
    max_rows: Option<usize>,
) -> AppResult<DelimitedPreview> {
    Ok(preview_impl(
        &content,
        delimiter.as_deref(),
        max_rows.unwrap_or(15),
    )?)
}

// ---------------------------------------------------------------------------
// Import — écriture en base
// ---------------------------------------------------------------------------

/// Partie commune aux deux modes d'import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportCommon {
    pub point_id: String,
    /// Contenu texte du fichier, déjà décodé côté frontend.
    ///
    /// Vide pour l'import xlsx, qui transporte des **octets** (`bytes`) et non du
    /// texte : ce champ n'est lu que par les parseurs textuels, jamais par
    /// `persist_import`, qui reçoit déjà des mesures analysées.
    #[serde(default)]
    pub content: String,
    /// Nom du fichier d'origine (traçabilité).
    pub file_name: Option<String>,
    /// `"Primary"` ou `"Backup"` — **toujours choisi par l'utilisateur**.
    pub role: String,
    /// Secondes à ajouter à l'horodatage brut pour obtenir l'UTC vrai
    /// (conversion de fuseau + dérive d'horloge, agrégées).
    #[serde(default)]
    pub clock_offset_seconds: i64,
    /// La source fournissait-elle déjà de l'UTC ? (documentaire)
    #[serde(default)]
    pub raw_timestamp_was_utc: bool,
    /// Numéro de série saisi à la main ; s'il est vide on prend celui du fichier.
    pub logger_serial: Option<String>,
    pub label: Option<String>,
    /// `true` : créer la série même si son contenu a déjà été importé sur ce
    /// point. Ne vaut `true` qu'après un clic explicite dans l'avertissement de
    /// ré-import — jamais par défaut. Voir [`ImportOutcome`].
    #[serde(default)]
    pub force_duplicate: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ElitechImportPayload {
    #[serde(flatten)]
    pub common: ImportCommon,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManualImportPayload {
    #[serde(flatten)]
    pub common: ImportCommon,
    pub mapping: ManualMapping,
    /// Par défaut `Manual`. Permet de marquer un collage qui vient quand même
    /// d'un RC-5 comme `ElitechImport`.
    pub source_type: Option<String>,
}

/// Bilan d'import affiché à l'utilisateur.
#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub series_id: String,
    pub point_id: String,
    pub point_name: String,
    pub source_type: SourceType,
    pub role: SeriesRole,
    /// Mesures **réellement insérées** (instants encore inconnus de la série).
    pub imported_count: usize,
    /// Mesures dont une valeur a été mise à jour — voie Infoclimat uniquement,
    /// quand un appel renvoie une donnée corrigée pour un instant déjà connu.
    #[serde(default)]
    pub updated_count: usize,
    /// Mesures déjà présentes à l'identique : rien n'a été réécrit.
    /// C'est le cas normal du recouvrement entre deux appels d'API.
    #[serde(default)]
    pub unchanged_count: usize,
    /// Lignes écartées (= nombre d'avertissements).
    pub skipped_count: usize,
    /// Mesures non écrites parce qu'un même instant figurait déjà dans la série.
    /// Toujours 0 sur un fichier de logger sain ; non nul si la source répétait
    /// un horodatage (la contrainte d'unicité de la v3 l'écarte sans échouer).
    #[serde(default)]
    pub duplicate_count: usize,
    pub warning_count: usize,
    pub warnings: Vec<ImportWarning>,
    /// Période couverte, **après** correction d'horloge.
    pub period_start_utc: Option<DateTime<Utc>>,
    pub period_end_utc: Option<DateTime<Utc>>,
    pub clock_offset_seconds: i64,
    pub raw_timestamp_was_utc: bool,
    pub logger_serial: Option<String>,
    pub sample_interval_seconds: Option<i64>,
    /// Structure détectée (import automatique uniquement).
    pub layout: Option<DetectedLayout>,
    /// Ordre jour/mois appliqué, et ce qui l'a justifié.
    pub date_order: DateOrderEvidence,
    /// `true` : l'ordre appliqué n'est qu'un repli, à confirmer ou inverser.
    pub date_format_uncertain: bool,
    /// Empreinte du lot importé, telle qu'enregistrée sur la série.
    /// `None` quand on a complété une série existante : l'empreinte de celle-ci
    /// décrit son import d'origine, pas le lot qui vient de s'y ajouter.
    #[serde(default)]
    pub content_fingerprint: Option<String>,
    /// `true` : l'utilisateur a passé outre un avertissement de ré-import.
    #[serde(default)]
    pub forced_duplicate: bool,
    /// Licence renvoyée par la source, quand elle en annonce une (Infoclimat).
    #[serde(default)]
    pub data_license: Option<String>,
    /// Mention d'attribution à afficher avec ces données, le cas échéant.
    #[serde(default)]
    pub attribution: Option<String>,

    /// Sauts de température jugés trop brutaux pour être un phénomène de site.
    ///
    /// **Aucune mesure n'est écartée** : la liste est purement informative, à
    /// charge pour l'utilisateur de décider. Voir [`crate::anomaly`] pour la
    /// raison d'être du seuil et pour le cas réel qui l'a motivé.
    #[serde(default)]
    pub anomalies: Vec<TemperatureJump>,
    pub stats: PeriodStats,
}

// ---------------------------------------------------------------------------
// Détection de ré-import
// ---------------------------------------------------------------------------

/// Ce qu'on sait de la série qui porte déjà l'empreinte du lot présenté.
///
/// Tout est là pour que l'utilisateur **reconnaisse** l'import précédent — sa
/// date, son fichier, sa période — et décide en connaissance de cause. Un simple
/// « déjà importé » l'obligerait à aller vérifier ailleurs.
#[derive(Debug, Clone, Serialize)]
pub struct DuplicateImportInfo {
    pub series_id: String,
    pub point_id: String,
    pub point_name: String,
    /// Instant de l'import précédent (c'est la date citée dans le message).
    pub imported_at: DateTime<Utc>,
    pub source_type: SourceType,
    pub role: SeriesRole,
    /// Nom du fichier d'origine, si l'import précédent en avait un.
    pub raw_file_reference: Option<String>,
    pub label: Option<String>,
    pub measurement_count: i64,
    pub period_start_utc: Option<DateTime<Utc>>,
    pub period_end_utc: Option<DateTime<Utc>>,
    /// Nombre de mesures dans le lot qu'on s'apprêtait à importer.
    pub incoming_count: usize,
    pub fingerprint: String,
    /// Phrase prête à afficher — rédigée ici, comme tous les messages destinés
    /// à l'utilisateur (cf. l'en-tête de `error.rs`).
    pub message: String,
}

/// Résultat d'une commande d'import : soit c'est écrit, soit on demande d'abord.
///
/// Un `Result::Err` ne conviendrait pas : le frontend a besoin de la **structure**
/// (date, nom de la série, période) pour proposer un choix, alors que `AppError`
/// se sérialise en une chaîne. Une enveloppe étiquetée le dit sans ambiguïté et
/// oblige l'appelant à traiter les deux cas.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ImportOutcome {
    /// L'import a eu lieu.
    Imported { report: Box<ImportReport> },
    /// Rien n'a été écrit : ce contenu est déjà en base sur ce point.
    DuplicateSuspected { duplicate: Box<DuplicateImportInfo> },
}

impl ImportOutcome {
    fn imported(report: ImportReport) -> Self {
        ImportOutcome::Imported {
            report: Box::new(report),
        }
    }

    /// Accès au rapport — utilisé par `fetch_infoclimat`, qui rhabille le
    /// rapport d'import en `FetchReport`, et par les tests.
    pub fn report(&self) -> Option<&ImportReport> {
        match self {
            ImportOutcome::Imported { report } => Some(report),
            ImportOutcome::DuplicateSuspected { .. } => None,
        }
    }

    pub fn duplicate(&self) -> Option<&DuplicateImportInfo> {
        match self {
            ImportOutcome::DuplicateSuspected { duplicate } => Some(duplicate),
            ImportOutcome::Imported { .. } => None,
        }
    }
}

/// Cherche une série de même empreinte sur ce point, et la décrit s'il y en a une.
///
/// Appelée **avant toute écriture** : quand elle renvoie `Some`, la base n'a pas
/// été touchée et l'utilisateur garde le choix.
fn detecter_reimport(
    db: &Db,
    point: &Point,
    empreinte: &str,
    incoming_count: usize,
) -> AppResult<Option<DuplicateImportInfo>> {
    let Some(precedente) = db.find_series_by_fingerprint(&point.id, empreinte)? else {
        return Ok(None);
    };

    let (debut, fin) = db.series_bounds_utc(&precedente.id)?;

    // Comment nommer la série précédente : son libellé s'il existe, sinon le
    // fichier d'origine, sinon le début de son identifiant. Jamais rien
    // d'inventé — l'utilisateur doit pouvoir la retrouver dans la fiche du point.
    let designation = precedente
        .label
        .clone()
        .or_else(|| precedente.raw_file_reference.clone())
        .unwrap_or_else(|| format!("série {}", &precedente.id[..8.min(precedente.id.len())]));

    let message = format!(
        "Ce contenu semble déjà avoir été importé le {} sur « {} », dans « {} » \
         ({} mesure(s)). Les deux lots sont identiques : même nombre de mesures, \
         mêmes bornes, mêmes valeurs. Importer quand même créerait une seconde \
         série jumelle, et les statistiques du point compteraient tout en double.",
        precedente.imported_at.format("%d/%m/%Y à %H:%M UTC"),
        point.name,
        designation,
        precedente.measurement_count
    );

    Ok(Some(DuplicateImportInfo {
        series_id: precedente.id,
        point_id: point.id.clone(),
        point_name: point.name.clone(),
        imported_at: precedente.imported_at,
        source_type: precedente.source_type,
        role: precedente.role,
        raw_file_reference: precedente.raw_file_reference,
        label: precedente.label,
        measurement_count: precedente.measurement_count,
        period_start_utc: debut,
        period_end_utc: fin,
        incoming_count,
        fingerprint: empreinte.to_string(),
        message,
    }))
}

#[tauri::command]
pub fn import_elitech(
    state: State<'_, AppState>,
    payload: ElitechImportPayload,
) -> AppResult<ImportOutcome> {
    import_elitech_impl(&state.db, &payload)
}

/// Le corps de la commande, sans `State`.
///
/// Les commandes Tauri ne sont pas appelables hors d'une application : leur
/// premier paramètre exige un `State`, qui ne s'obtient que d'un `tauri::App`.
/// En reléguant le travail dans une fonction qui ne demande qu'un `&Db`, la
/// détection de ré-import se teste sur le **chemin réel** de l'import, et pas
/// sur une reconstitution approchée.
fn import_elitech_impl(db: &Db, payload: &ElitechImportPayload) -> AppResult<ImportOutcome> {
    let export = parse_elitech_export_detailed(&payload.common.content)?;
    let layout = export.layout.clone();
    let serial = payload
        .common
        .logger_serial
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| export.metadata.logger_serial.clone());

    persist_import(
        db,
        &payload.common,
        SourceType::ElitechImport,
        ParsedRows {
            measurements: export.measurements,
            warnings: export.warnings,
            date_order: export.date_order,
        },
        serial,
        export.metadata.sample_interval_seconds,
        Some(layout),
    )
}

#[tauri::command]
pub fn import_manual(
    state: State<'_, AppState>,
    payload: ManualImportPayload,
) -> AppResult<ImportOutcome> {
    let parsed = parse_manual(&payload.common.content, &payload.mapping)?;
    let source_type = match payload.source_type.as_deref() {
        Some(s) if !s.trim().is_empty() => SourceType::parse(s)?,
        _ => SourceType::Manual,
    };
    let serial = payload
        .common
        .logger_serial
        .clone()
        .filter(|s| !s.trim().is_empty());

    persist_import(&state.db, &payload.common, source_type, parsed, serial, None, None)
}

// ---------------------------------------------------------------------------
// Altitude IGN (bouton « Récupérer via IGN » de la fiche point)
// ---------------------------------------------------------------------------

/// Altitude au sol d'un point selon le RGE ALTI de l'IGN. Réseau : hors du fil UI.
#[tauri::command]
pub async fn ign_altitude(latitude: f64, longitude: f64) -> AppResult<f64> {
    tauri::async_runtime::spawn_blocking(move || crate::altimetrie::altitude_ign(latitude, longitude))
        .await
        .map_err(|e| AppError::Other(format!("Tâche asynchrone interrompue : {e}")))?
}

// ---------------------------------------------------------------------------
// Import assisté par IA (Groq LLM + Vision)
// ---------------------------------------------------------------------------

/// Contenu d'un fichier du dernier glisser-déposer (zone de l'assistant IA).
/// Seuls les chemins réellement déposés sont lisibles, voir `drop_files`.
#[tauri::command]
pub fn read_dropped_file(path: String) -> AppResult<tauri::ipc::Response> {
    crate::drop_files::read(&path).map(tauri::ipc::Response::new)
}

#[tauri::command]
pub async fn ai_analyze_file(
    payload: crate::ai_import::AiFileAnalysisRequest,
) -> AppResult<crate::ai_import::AiTextAnalysisResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let client = groq::GroqClient::from_local_key()?;
        crate::ai_import::analyze_text_with_ai(&client, &payload)
    })
    .await
    .map_err(|e| AppError::Other(format!("Tâche asynchrone interrompue : {e}")))?
}

#[tauri::command]
pub async fn ai_analyze_vision(
    payload: crate::ai_import::AiVisionAnalysisRequest,
) -> AppResult<crate::ai_import::AiVisionAnalysisResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let client = groq::GroqClient::from_local_key()?;
        crate::ai_import::analyze_vision_with_ai(&client, &payload)
    })
    .await
    .map_err(|e| AppError::Other(format!("Tâche vision interrompue : {e}")))?
}

#[tauri::command]
pub fn ai_preview_parsed(
    content: String,
    mapping: crate::ai_import::AiFileMapping,
) -> AppResult<crate::ai_import::AiTextAnalysisResult> {
    let normalized = crate::parsing::normalize_content(&content);
    let total_lines = normalized.lines().count();
    let (preview_rows, warnings) = crate::ai_import::generate_preview_from_text(&normalized, &mapping, 20)?;
    Ok(crate::ai_import::AiTextAnalysisResult {
        mapping,
        preview_rows,
        total_lines,
        warnings,
    })
}

#[tauri::command]
pub fn import_ai(
    state: State<'_, AppState>,
    payload: crate::ai_import::AiImportPayload,
) -> AppResult<ImportOutcome> {
    let parsed = match &payload.source_kind {
        crate::ai_import::AiSourceKind::TextMapped { mapping } => {
            crate::ai_import::parse_content_with_ai_mapping(&payload.common.content, mapping)?
        }
        crate::ai_import::AiSourceKind::DirectRows { rows } => {
            crate::ai_import::convert_direct_rows_to_parsed(rows)?
        }
    };

    let serial = payload
        .common
        .logger_serial
        .clone()
        .filter(|s| !s.trim().is_empty());

    persist_import(
        &state.db,
        &payload.common,
        SourceType::Manual,
        parsed,
        serial,
        None,
        None,
    )
}

// ---------------------------------------------------------------------------
// Fabrique commune
// ---------------------------------------------------------------------------

/// Convertit les lignes brutes en mesures UTC, crée la série et écrit le tout.
///
/// C'est **le seul endroit** où un horodatage naïf devient un instant UTC.
///
/// L'ordre des opérations compte : les mesures sont construites, leur empreinte
/// calculée, et le ré-import cherché **avant** la moindre écriture. Un doublon
/// détecté ressort donc sans qu'une ligne ait été insérée, pas comme un import
/// qu'il faudrait défaire.
fn persist_import(
    db: &Db,
    common: &ImportCommon,
    source_type: SourceType,
    parsed: ParsedRows,
    logger_serial: Option<String>,
    sample_interval_seconds: Option<i64>,
    layout: Option<DetectedLayout>,
) -> AppResult<ImportOutcome> {
    let point = db.get_point(&common.point_id)?;
    let role = SeriesRole::parse(&common.role)?;

    if parsed.measurements.is_empty() {
        return Err(AppError::Other(
            "Aucune mesure exploitable : rien n'a été enregistré.".to_string(),
        ));
    }

    // L'empreinte porte sur les horodatages bruts (cf. `fingerprint.rs`), donc
    // elle ne dépend pas du décalage d'horloge saisi : la calculer avant même de
    // fabriquer la série est cohérent, et évite de tirer un UUID pour rien.
    // Hachée directement sur les lignes brutes : aucun `Vec<Measurement>`
    // temporaire (même recette, cf. `FingerprintSample for RawMeasurement`).
    let empreinte = series_fingerprint_of(&parsed.measurements);

    if !common.force_duplicate {
        if let Some(duplicate) =
            detecter_reimport(db, &point, &empreinte, parsed.measurements.len())?
        {
            return Ok(ImportOutcome::DuplicateSuspected {
                duplicate: Box::new(duplicate),
            });
        }
    }

    let series = LoggerSeries {
        id: Uuid::new_v4().to_string(),
        point_id: point.id.clone(),
        source_type,
        role,
        logger_serial,
        imported_at: now_utc(),
        raw_file_reference: common
            .file_name
            .clone()
            .filter(|f| !f.trim().is_empty()),
        clock_offset_seconds: common.clock_offset_seconds,
        raw_timestamp_was_utc: common.raw_timestamp_was_utc,
        label: common.label.clone().filter(|l| !l.trim().is_empty()),
        sample_interval_seconds,
        data_license: None,
        station_id: None,
        last_api_fetch_at: None,
        date_order: parsed.date_order.order,
        // Le doute est levé aussi bien par l'absence de preuve que par des preuves
        // contradictoires : dans les deux cas l'ordre appliqué peut être faux.
        date_format_uncertain: parsed.date_order.uncertain || parsed.date_order.conflicting,
        content_fingerprint: Some(empreinte.clone()),
        measurement_count: 0,
    };

    let measurements = to_measurements(&series.id, &parsed.measurements, series.clock_offset_seconds);

    let ecrites = db.insert_series_with_measurements(&series, &measurements)?;
    let duplicate_count = measurements.len() - ecrites;

    let stats = period_stats(&measurements);
    let warning_count = parsed.warnings.len();

    // Détection centralisée, pour **toutes** les sources de fichier qui passent
    // par ici (Elitech tabulaire, Elitech xlsx, import manuel) : le saut brutal
    // est le signe d'un logger manipulé, pas d'un format de fichier particulier.
    // Elle porte sur les horodatages bruts, avant correction d'horloge — c'est
    // l'écart entre deux mesures voisines qui compte, et il ne change pas quand
    // toute la série est décalée du même nombre de secondes.
    let anomalies = detect_jumps_raw(&parsed.measurements, &JumpThresholds::default());

    Ok(ImportOutcome::imported(ImportReport {
        series_id: series.id,
        point_id: point.id,
        point_name: point.name,
        source_type,
        role,
        imported_count: ecrites,
        updated_count: 0,
        unchanged_count: 0,
        skipped_count: warning_count,
        duplicate_count,
        warning_count,
        warnings: parsed
            .warnings
            .into_iter()
            .take(MAX_WARNINGS_RETURNED)
            .collect(),
        period_start_utc: stats.first_utc,
        period_end_utc: stats.last_utc,
        clock_offset_seconds: series.clock_offset_seconds,
        raw_timestamp_was_utc: series.raw_timestamp_was_utc,
        logger_serial: series.logger_serial,
        sample_interval_seconds: series.sample_interval_seconds,
        layout,
        date_order: parsed.date_order,
        date_format_uncertain: series.date_format_uncertain,
        content_fingerprint: Some(empreinte),
        forced_duplicate: common.force_duplicate,
        data_license: None,
        attribution: None,
        anomalies,
        stats,
    }))
}

/// Passage horodatage brut → UTC corrigé.
///
/// L'horodatage du fichier est **naïf** : on le réinterprète tel quel comme de
/// l'UTC (`and_utc()`, aucune conversion), on le conserve dans `raw_timestamp`,
/// puis on applique `clock_offset_seconds` pour obtenir `timestamp_utc`.
fn to_measurements(
    series_id: &str,
    raws: &[RawMeasurement],
    clock_offset_seconds: i64,
) -> Vec<Measurement> {
    // Un seul `Arc<str>` pour tout le lot, cloné (compteur) par mesure.
    let series_id: std::sync::Arc<str> = series_id.into();
    raws.iter()
        .map(|r| {
            let raw_utc = r.raw_timestamp.and_utc();
            // Un logger de terrain ne publie ni signal radio, ni batterie, ni
            // aucun des champs météo de la v5 : `Measurement::new` les laisse
            // tous à `None`, et seule l'humidité est renseignée ensuite.
            Measurement {
                humidity_percent: r.humidity_percent,
                ..Measurement::new(
                    series_id.clone(),
                    apply_clock_offset(raw_utc, clock_offset_seconds),
                    raw_utc,
                    r.temperature_celsius,
                )
            }
        })
        .collect()
}

fn raw_bounds(raws: &[RawMeasurement]) -> (Option<NaiveDateTime>, Option<NaiveDateTime>) {
    (
        raws.iter().map(|r| r.raw_timestamp).min(),
        raws.iter().map(|r| r.raw_timestamp).max(),
    )
}
// ---------------------------------------------------------------------------
// Elitech xlsx — le format réel
// ---------------------------------------------------------------------------
//
// ## Pourquoi des octets et pas du texte
//
// Toutes les autres commandes d'import reçoivent une `String` : le frontend
// décode le fichier (BOM UTF-8/UTF-16, repli Windows-1252) avant de l'envoyer.
// Un classeur xlsx est une **archive zip** — le décoder en texte le détruirait.
// Cette voie transporte donc les octets tels quels, et c'est le seul endroit du
// projet où c'est le cas.
//
// ## Le coût, et pourquoi il est accepté
//
// `invoke` sérialise ses arguments en JSON : un `Vec<u8>` traverse l'IPC sous
// forme de tableau de nombres, soit environ six fois la taille du fichier. Sur
// l'export réel (254 Ko, 10 307 mesures) cela fait ~1,5 Mo de JSON — quelques
// dixièmes de seconde, une fois, au moment de l'analyse.
//
// C'est acceptable parce que la taille d'un export RC-5 est **bornée par la
// mémoire du logger** : 16 000 points au maximum, soit moins de 500 Ko de
// classeur. Le passage en corps de requête brut (`tauri::ipc::Request`)
// diviserait ce coût par six, au prix de faire voyager `point_id`, `role` et le
// décalage d'horloge dans des en-têtes — beaucoup de complexité pour un gain
// invisible à cette échelle. À reconsidérer seulement si un logger d'une autre
// génération produit des classeurs de plusieurs dizaines de Mo.

/// Ce que l'analyse d'un classeur ElitechLog apprend, avant tout enregistrement.
#[derive(Debug, Clone, Serialize)]
pub struct ElitechXlsxAnalysis {
    pub metadata: XlsxMetadata,
    /// Feuille retenue pour les données, et toutes celles du classeur.
    pub data_sheet: String,
    pub summary_sheet: Option<String>,
    pub sheet_names: Vec<String>,
    pub header_labels: Vec<String>,
    pub total_rows: usize,
    pub measurement_count: usize,
    pub warning_count: usize,
    pub warnings: Vec<ImportWarning>,
    /// Bornes **brutes** (non corrigées) de la période couverte.
    pub raw_period_start: Option<NaiveDateTime>,
    pub raw_period_end: Option<NaiveDateTime>,
    pub sample: Vec<RawMeasurement>,
    /// Toujours « ISO » pour ce format : aucune ambiguïté jour/mois à trancher.
    pub date_order: DateOrderEvidence,
    /// Sauts de température suspects — montrés **avant** l'import, pour que
    /// l'utilisateur sache à quoi s'attendre.
    pub anomalies: Vec<TemperatureJump>,
    /// Seuils appliqués, pour que le chiffre affiché soit interprétable.
    pub jump_thresholds: JumpThresholds,
}

fn analyser_xlsx(export: ElitechXlsxExport) -> ElitechXlsxAnalysis {
    let (start, end) = raw_bounds(&export.measurements);
    ElitechXlsxAnalysis {
        metadata: export.metadata,
        data_sheet: export.data_sheet,
        summary_sheet: export.summary_sheet,
        sheet_names: export.sheet_names,
        header_labels: export.header_labels,
        total_rows: export.total_rows,
        measurement_count: export.measurements.len(),
        warning_count: export.warnings.len(),
        warnings: export
            .warnings
            .into_iter()
            .take(MAX_WARNINGS_RETURNED)
            .collect(),
        raw_period_start: start,
        raw_period_end: end,
        sample: export.measurements.into_iter().take(5).collect(),
        date_order: export.date_order,
        anomalies: export.anomalies,
        jump_thresholds: JumpThresholds::default(),
    }
}

/// Analyse un classeur ElitechLog **sans rien écrire**.
#[tauri::command]
pub fn analyze_elitech_xlsx(bytes: Vec<u8>) -> AppResult<ElitechXlsxAnalysis> {
    Ok(analyser_xlsx(parse_elitech_xlsx(&bytes)?))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ElitechXlsxImportPayload {
    #[serde(flatten)]
    pub common: ImportCommon,
    /// Octets bruts du classeur. Voir la note ci-dessus.
    pub bytes: Vec<u8>,
}

#[tauri::command]
pub fn import_elitech_xlsx(
    state: State<'_, AppState>,
    payload: ElitechXlsxImportPayload,
) -> AppResult<ImportOutcome> {
    import_elitech_xlsx_impl(&state.db, &payload)
}

/// Corps de [`import_elitech_xlsx`], testable sans `State`.
fn import_elitech_xlsx_impl(
    db: &Db,
    payload: &ElitechXlsxImportPayload,
) -> AppResult<ImportOutcome> {
    let export = parse_elitech_xlsx(&payload.bytes)?;

    // Le numéro de série saisi à la main l'emporte sur celui du fichier : c'est
    // la même règle que pour l'import tabulaire, et elle sert quand un logger a
    // été réétiqueté sur le terrain.
    let serial = payload
        .common
        .logger_serial
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| export.metadata.logger_serial.clone());

    persist_import(
        db,
        &payload.common,
        // Même `SourceType` que l'import tabulaire : c'est le **même appareil**
        // et la même nature de donnée, seul le contenant diffère. En faire une
        // variante distincte aurait imposé une migration de la contrainte SQL
        // pour zéro information supplémentaire — `raw_file_reference` porte déjà
        // le nom du fichier, donc son format.
        SourceType::ElitechImport,
        ParsedRows {
            measurements: export.measurements,
            warnings: export.warnings,
            date_order: export.date_order,
        },
        serial,
        export.metadata.sample_interval_seconds,
        // Pas de `DetectedLayout` : cette structure décrit un découpage de texte
        // délimité (séparateur, index de colonnes), notion qui n'a pas de sens
        // pour un classeur. L'analyse préalable dit déjà quelle feuille et quels
        // intitulés ont été retenus.
        None,
    )
}

// ---------------------------------------------------------------------------
// Elitech RC-5 — lecture directe par le port série
// ---------------------------------------------------------------------------
//
// Cette voie double l'import xlsx, elle ne le remplace pas. Les deux produisent
// les mêmes `RawMeasurement` et passent par le même `persist_import` : même
// empreinte de contenu, donc même détection de ré-import, et mêmes anomalies de
// saut de température. La seule différence portée jusqu'en base est le
// `SourceType` — `ElitechDirect` ici, `ElitechImport` pour un fichier.
//
// Une lecture complète prend quelques secondes (21 échanges pour 10 000
// mesures). Elle part donc dans un `spawn_blocking`, comme les appels réseau, et
// son résultat est **mémorisé** : sans cela, analyser puis importer relirait
// deux fois le logger — et rien ne garantirait que l'utilisateur importe
// exactement ce qu'on lui a montré.

/// Une lecture déjà faite, gardée entre l'analyse et l'import.
struct Rc5Cache {
    reading_id: String,
    port_name: String,
    reading: Rc5Reading,
    host_utc_at_read: DateTime<Utc>,
}

/// Ce que l'analyse d'une lecture directe apprend, avant tout enregistrement.
///
/// Volontairement parallèle à [`ElitechXlsxAnalysis`] : l'UI affiche les deux de
/// la même façon, et ce qui diffère (l'état du logger, son horloge) est ce qui
/// n'existe tout simplement pas dans un fichier.
#[derive(Debug, Clone, Serialize)]
pub struct Rc5Analysis {
    /// Jeton de la lecture mémorisée, à repasser à `import_rc5`. C'est lui qui
    /// garantit qu'on importe le lot analysé et non un autre.
    pub reading_id: String,
    pub port_name: String,
    pub model_label: String,
    /// Numéro de série **tel que l'appareil le transmet** — tronqué à 10
    /// caractères par le protocole (cf. `elitech_serial`).
    pub logger_serial: Option<String>,
    pub user_info: Option<String>,
    pub work_status_label: String,
    pub sample_interval_seconds: Option<i64>,
    pub measurement_count: usize,
    /// Bornes **brutes** (non corrigées) de la période couverte.
    pub raw_period_start: Option<NaiveDateTime>,
    pub raw_period_end: Option<NaiveDateTime>,
    pub sample: Vec<RawMeasurement>,
    /// Toutes les mesures lues, **sous-échantillonnées pour le tracé**.
    ///
    /// C'est ce que la page RC-5 dessine : la mémoire entière du logger d'un
    /// coup d'oeil. `sample` reste ce qu'il était — les cinq premières mesures
    /// brutes, pour vérifier qu'elles sont interprétées comme il faut. Les deux
    /// ne répondent pas à la même question. Voir [`courbe_rc5`].
    pub curve: Vec<Rc5CurvePoint>,
    /// Toujours « ISO » : les instants sont calculés, jamais lus dans un texte.
    pub date_order: DateOrderEvidence,
    pub anomalies: Vec<TemperatureJump>,
    pub jump_thresholds: JumpThresholds,
    /// Avertissements propres au matériel (logger encore en marche, etc.).
    pub read_warnings: Vec<ReadWarning>,
    /// Horloge interne du logger au moment de la lecture, telle quelle.
    pub device_clock: Option<NaiveDateTime>,
    /// Horloge du PC au même instant — la référence de comparaison.
    pub host_utc_at_read: DateTime<Utc>,
    /// `device_clock − host_utc`, en secondes.
    ///
    /// Ce n'est **pas** encore la dérive : il y faut le fuseau réglé sur le
    /// logger, que l'utilisateur seul connaît. L'UI retranche le fuseau choisi
    /// pour obtenir la dérive à saisir. Faire ce calcul ici imposerait au backend
    /// de deviner un fuseau, ce qu'il ne fait nulle part ailleurs.
    pub clock_offset_vs_utc_seconds: Option<i64>,
    /// Seuils d'alarme configurés dans l'appareil, à titre indicatif.
    pub upper_limit_celsius: f64,
    pub lower_limit_celsius: f64,
    /// Correction interne appliquée par le logger, déjà comprise dans les valeurs.
    pub temperature_calibration: f64,
}

fn analyser_rc5(cache: &Rc5Cache) -> Rc5Analysis {
    let lecture = &cache.reading;
    let (start, end) = raw_bounds(&lecture.measurements);
    Rc5Analysis {
        reading_id: cache.reading_id.clone(),
        port_name: cache.port_name.clone(),
        model_label: lecture.device.model_label(),
        logger_serial: lecture.logger_serial(),
        user_info: lecture.device.user_info.clone(),
        work_status_label: lecture.device.work_status.label_fr(),
        sample_interval_seconds: Some(lecture.device.sample_interval_seconds),
        measurement_count: lecture.measurements.len(),
        raw_period_start: start,
        raw_period_end: end,
        sample: lecture.measurements.iter().take(5).cloned().collect(),
        curve: courbe_rc5(&lecture.measurements, RC5_COURBE_MAX_POINTS),
        date_order: lecture.date_order.clone(),
        anomalies: lecture.anomalies.clone(),
        jump_thresholds: JumpThresholds::default(),
        read_warnings: lecture.warnings.clone(),
        device_clock: lecture.device.device_clock,
        host_utc_at_read: cache.host_utc_at_read,
        clock_offset_vs_utc_seconds: lecture
            .device
            .clock_drift_seconds(cache.host_utc_at_read.naive_utc()),
        upper_limit_celsius: lecture.device.upper_limit_celsius,
        lower_limit_celsius: lecture.device.lower_limit_celsius,
        temperature_calibration: lecture.device.temperature_calibration,
    }
}

/// Les ports série portant un CH340, sans en ouvrir aucun.
///
/// Sert au bouton « rechercher un logger » : c'est la même énumération que celle
/// du fil de surveillance, exposée pour que l'utilisateur puisse la déclencher
/// lui-même plutôt que d'attendre le prochain cycle.
#[tauri::command]
pub fn list_rc5_ports() -> AppResult<Vec<Ch340Port>> {
    Ok(elitech_serial::list_ch340_ports()?)
}

/// Redemande une identification de tous les ports au prochain cycle.
///
/// Le cas d'usage : l'utilisateur vient de fermer ElitechLog après avoir vu
/// « port occupé ». Sans cela il devrait débrancher puis rebrancher le logger.
#[tauri::command]
pub fn rescan_rc5(state: State<'_, AppState>) -> AppResult<()> {
    if let Some(watch) = state.rc5_watch.lock().unwrap().as_ref() {
        watch.rescan();
    }
    Ok(())
}

/// Identifie l'appareil branché sans lire son historique.
///
/// Un seul échange de 160 octets : c'est ce qu'on veut pour répondre à
/// « qu'est-ce qui est branché ? » sans faire attendre plusieurs secondes.
#[tauri::command(rename_all = "snake_case")]
pub async fn identify_rc5(port_name: Option<String>) -> AppResult<Rc5Identity> {
    let identite = tauri::async_runtime::spawn_blocking(move || match port_name {
        Some(nom) if !nom.trim().is_empty() => {
            let nom = nom.trim().to_string();
            elitech_serial::identify_port(&nom).map(|info| (nom, info))
        }
        _ => elitech_serial::find_rc5().map(|(port, info)| (port.port_name, info)),
    })
    .await
    .map_err(|e| AppError::Other(format!("L'identification n'a pas pu aboutir : {e}")))??;

    let (port_name, device) = identite;
    Ok(Rc5Identity {
        model_label: device.model_label(),
        logger_serial: device.device_number.clone(),
        rec_count: device.rec_count,
        work_status_label: device.work_status.label_fr(),
        sample_interval_seconds: device.sample_interval_seconds,
        port_name,
        device,
    })
}

/// Fiche d'identité renvoyée par [`identify_rc5`].
#[derive(Debug, Clone, Serialize)]
pub struct Rc5Identity {
    pub port_name: String,
    pub model_label: String,
    pub logger_serial: Option<String>,
    pub rec_count: u16,
    pub work_status_label: String,
    pub sample_interval_seconds: i64,
    pub device: DevInfo,
}

/// Lit tout l'historique du logger **sans rien écrire en base**.
///
/// La lecture est mémorisée dans l'état applicatif : `import_rc5` la reprendra
/// sur présentation du `reading_id`, au lieu de refaire parler le logger.
#[tauri::command(rename_all = "snake_case")]
pub async fn read_rc5(
    state: State<'_, AppState>,
    port_name: Option<String>,
) -> AppResult<Rc5Analysis> {
    let (port_name, reading) = lire_rc5(port_name).await?;

    let cache = Rc5Cache {
        reading_id: Uuid::new_v4().to_string(),
        port_name,
        reading,
        host_utc_at_read: now_utc(),
    };
    let analyse = analyser_rc5(&cache);
    state
        .rc5_reading
        .lock()
        .unwrap()
        .insert(cache.port_name.clone(), cache);
    Ok(analyse)
}

/// Lecture brute, hors du thread principal — le seul endroit qui parle au port.
async fn lire_rc5(port_name: Option<String>) -> AppResult<(String, Rc5Reading)> {
    let resultat = tauri::async_runtime::spawn_blocking(move || {
        let nom = match port_name {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            // Aucun port imposé : on prend le premier CH340 qui se révèle être
            // un RC-5. C'est ce qui rend l'import « en un clic » possible.
            _ => elitech_serial::find_rc5()?.0.port_name,
        };
        elitech_serial::read_port(&nom, &JumpThresholds::default()).map(|l| (nom, l))
    })
    .await
    .map_err(|e| AppError::Other(format!("La lecture du logger n'a pas pu aboutir : {e}")))??;
    Ok(resultat)
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rc5ImportPayload {
    #[serde(flatten)]
    pub common: ImportCommon,
    /// Jeton rendu par `read_rc5`. Absent ou périmé, le logger est relu.
    pub reading_id: Option<String>,
    /// Port à relire le cas échéant. Absent, le premier RC-5 trouvé est pris.
    pub port_name: Option<String>,
}

/// Importe l'historique lu en direct.
#[tauri::command]
pub async fn import_rc5(
    state: State<'_, AppState>,
    payload: Rc5ImportPayload,
) -> AppResult<ImportOutcome> {
    // Reprise de la lecture analysée, si c'est bien la même. Le verrou est pris
    // et relâché ici, avant tout `await` : le garder au-delà bloquerait la
    // surveillance et l'UI pendant toute une relecture.
    let memorisee = {
        let mut caches = state.rc5_reading.lock().unwrap();
        // Recherche par jeton, pas par port : c'est le jeton qui garantit qu'on
        // enregistre le lot montré à l'utilisateur. Le port ne sert qu'à ranger.
        let port = payload.reading_id.as_ref().and_then(|id| {
            caches
                .iter()
                .find(|(_, c)| c.reading_id == *id)
                .map(|(port, _)| port.clone())
        });
        port.and_then(|p| caches.remove(&p))
    };

    let (port_name, reading) = match memorisee {
        Some(c) => (c.port_name, c.reading),
        None => lire_rc5(payload.port_name.clone()).await?,
    };

    import_rc5_impl(&state.db, &payload.common, &port_name, reading)
}

/// Corps de [`import_rc5`], testable sans `State` ni matériel.
fn import_rc5_impl(
    db: &Db,
    common: &ImportCommon,
    port_name: &str,
    reading: Rc5Reading,
) -> AppResult<ImportOutcome> {
    // Le numéro saisi à la main l'emporte sur celui de l'appareil — même règle
    // que pour les fichiers. Elle a ici une utilité supplémentaire : rétablir le
    // numéro complet (`EFE205004333`) là où le protocole n'en transmet que dix
    // caractères (`E205004333`).
    let serial = common
        .logger_serial
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| reading.logger_serial());

    let interval = reading.device.sample_interval_seconds;

    // `raw_file_reference` n'a pas de fichier à porter : on y met de quoi
    // retrouver l'origine de la série — le port et l'appareil. C'est la même
    // intention que le nom de fichier pour un import, pas un détournement.
    let mut commun = common.clone();
    if commun
        .file_name
        .as_ref()
        .map(|f| f.trim().is_empty())
        .unwrap_or(true)
    {
        commun.file_name = Some(match reading.logger_serial() {
            Some(sn) => format!("Lecture directe RC-5 — {port_name}, n° {sn}"),
            None => format!("Lecture directe RC-5 — {port_name}"),
        });
    }

    persist_import(
        db,
        &commun,
        // La variante prévue dès le premier jour dans le modèle, jamais écrite
        // jusqu'ici. Elle se distingue d'`ElitechImport` par la **voie**, pas par
        // l'appareil : c'est ce qui permettra de savoir, série par série, si les
        // données sont passées par ElitechLog ou non.
        SourceType::ElitechDirect,
        ParsedRows {
            measurements: reading.measurements,
            // Aucune ligne écartée : il n'y a pas de texte à interpréter, chaque
            // mesure est deux octets. Les avertissements matériels vivent dans
            // l'analyse, pas ici — ils ne désignent pas de ligne fautive.
            warnings: Vec::new(),
            date_order: reading.date_order,
        },
        serial,
        Some(interval),
        // Pas de `DetectedLayout` : aucune colonne, aucun séparateur.
        None,
    )
}

// ---------------------------------------------------------------------------
// Page RC-5 : horloge, courbe, réinitialisation
// ---------------------------------------------------------------------------
//
// Les trois commandes qui suivent servent l'écran dédié au logger branché. Elles
// ne dupliquent aucune logique : la comparaison d'horloge et l'effacement vivent
// dans `elitech_serial` (testables sans matériel), la courbe est un simple
// sous-échantillonnage de la lecture déjà faite, et la sauvegarde des données
// passe par `import_rc5`, inchangé.

/// Nombre maximal de points envoyés à l'UI pour le graphique de la page RC-5.
///
/// Un RC-5 plein contient 16 000 mesures. Les envoyer telles quelles ferait un
/// gros mégaoctet de JSON et un graphique où chaque pixel porte huit points :
/// illisible, et lent à redessiner. 1 500 points, c'est déjà plus que ce qu'un
/// écran peut distinguer sur la largeur d'une fenêtre.
///
/// L'agrégation garde **le minimum et le maximum de chaque seau** en plus de la
/// moyenne : sur ce projet, c'est le minimum qui compte, et une courbe lissée qui
/// le gomme serait pire qu'une absence de courbe.
pub const RC5_COURBE_MAX_POINTS: usize = 1_500;

/// Un point de la courbe de la page RC-5.
///
/// L'instant est l'horodatage **brut** du logger (son horloge interne, naïve) :
/// la page montre l'appareil tel qu'il est, avant toute correction. La correction
/// n'intervient qu'à l'enregistrement, comme pour toutes les autres sources.
#[derive(Debug, Clone, Serialize)]
pub struct Rc5CurvePoint {
    pub raw_timestamp: NaiveDateTime,
    pub average_celsius: f64,
    pub min_celsius: f64,
    pub max_celsius: f64,
    /// Nombre de mesures rassemblées dans ce point (1 si aucune agrégation).
    pub count: usize,
}

/// Sous-échantillonne une lecture pour l'affichage.
///
/// Découpage par **rang**, pas par durée : les mesures d'un RC-5 sont
/// régulièrement espacées par construction (l'instant est reconstruit à partir du
/// départ et de la cadence — voir `elitech_serial`), donc un seau de n mesures
/// est aussi un seau de durée constante. Un découpage temporel n'apporterait rien
/// et coûterait une recherche de bornes.
pub fn courbe_rc5(measurements: &[RawMeasurement], max_points: usize) -> Vec<Rc5CurvePoint> {
    if measurements.is_empty() || max_points == 0 {
        return Vec::new();
    }
    let taille = measurements.len().div_ceil(max_points).max(1);

    measurements
        .chunks(taille)
        .map(|seau| {
            let somme: f64 = seau.iter().map(|m| m.temperature_celsius).sum();
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for m in seau {
                min = min.min(m.temperature_celsius);
                max = max.max(m.temperature_celsius);
            }
            Rc5CurvePoint {
                // L'instant du **premier** élément du seau, pas son milieu : la
                // courbe doit commencer exactement à la première mesure lue, et
                // c'est aussi ce que fait l'agrégation du bloc Croisement.
                raw_timestamp: seau[0].raw_timestamp,
                average_celsius: somme / seau.len() as f64,
                min_celsius: min,
                max_celsius: max,
                count: seau.len(),
            }
        })
        .collect()
}

/// Ce que la vérification d'horloge renvoie à la page RC-5.
#[derive(Debug, Clone, Serialize)]
pub struct Rc5ClockReport {
    pub port_name: String,
    pub model_label: String,
    pub logger_serial: Option<String>,
    pub rec_count: u16,
    pub work_status_label: String,
    pub sample_interval_seconds: i64,
    pub check: ClockCheck,
}

/// Lit l'heure du logger et la compare à celle du PC.
///
/// `timezone_offset_seconds` est le fuseau **réglé sur l'appareil**, tel que
/// l'utilisateur le déclare (0 pour UTC, 3600 pour UTC+1…). Le backend ne le
/// devine pas — il ne devine jamais un fuseau, c'est la règle du projet. Voir
/// [`crate::elitech_serial::compare_clock`] pour ce que ce paramètre change.
#[tauri::command(rename_all = "snake_case")]
pub async fn check_rc5_clock(
    port_name: Option<String>,
    timezone_offset_seconds: Option<i64>,
) -> AppResult<Rc5ClockReport> {
    let fuseau = timezone_offset_seconds.unwrap_or(0);
    let maintenant = now_utc();

    let (port_name, device, check) = tauri::async_runtime::spawn_blocking(move || {
        let nom = match port_name {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => elitech_serial::find_rc5()?.0.port_name,
        };
        elitech_serial::check_clock_on_port(&nom, fuseau, maintenant)
            .map(|(info, check)| (nom, info, check))
    })
    .await
    .map_err(|e| AppError::Other(format!("La lecture de l'horloge n'a pas pu aboutir : {e}")))??;

    Ok(Rc5ClockReport {
        port_name,
        model_label: device.model_label(),
        logger_serial: device.device_number.clone(),
        rec_count: device.rec_count,
        work_status_label: device.work_status.label_fr(),
        sample_interval_seconds: device.sample_interval_seconds,
        check,
    })
}

/// Mot que le frontend doit renvoyer pour qu'un effacement parte.
///
/// Ce n'est pas une sécurité — le frontend est de confiance —, c'est un **garde
/// d'appel** : il rend impossible qu'un `invoke("reset_rc5")` écrit par
/// distraction, ou recopié d'un exemple, efface la mémoire d'un appareil. La
/// commande la plus destructrice du projet ne doit pas être la plus facile à
/// déclencher par accident.
pub const CONFIRMATION_REINITIALISATION: &str = "EFFACER";

/// Bilan d'un effacement, tel que la page RC-5 l'affiche.
#[derive(Debug, Clone, Serialize)]
pub struct Rc5ResetReport {
    pub port_name: String,
    pub logger_serial: Option<String>,
    /// Nombre de mesures que l'appareil contenait avant l'effacement.
    pub records_erased: u16,
    /// Toujours `true` quand la commande réussit : un effacement non appliqué
    /// ressort en erreur. Le champ existe pour que l'UI n'ait pas à le déduire.
    pub memory_cleared: bool,
    /// `false` si l'appareil est ressorti avec un réglage différent — voir
    /// [`crate::elitech_serial::CMD_PARAM_PUT`].
    pub parameters_preserved: bool,
    pub changed_parameters: Vec<String>,
    /// `true` si le logger enregistrait encore et a dû être arrêté avant
    /// l'effacement — voir [`crate::elitech_serial::clear_memory`]. L'UI s'en
    /// sert pour prévenir que l'appareil ne redémarrera pas tout seul.
    pub recording_stopped: bool,
    pub message: String,
}

/// **Efface la mémoire du logger physique.** Irréversible côté appareil.
///
/// Ce que cette commande ne touche pas, et c'est le point à ne jamais confondre :
/// **les mesures déjà importées restent en base**, intactes. L'effacement porte
/// sur le boîtier, pas sur le projet — c'est ce que le texte de confirmation de
/// l'UI doit dire, et ce que dit aussi le message de bilan.
///
/// Deux gardes avant l'envoi :
/// * la confirmation ([`CONFIRMATION_REINITIALISATION`]) ;
/// * l'identification de l'appareil, qui refuse tout ce qui n'est pas un RC-5
///   (dans [`crate::elitech_serial::clear_memory`], avant la moindre écriture).
///
/// Un **enregistrement en cours n'est plus un obstacle** : le logger est arrêté
/// automatiquement d'abord, et l'arrêt est confirmé avant que la moindre
/// écriture ne parte. S'il ne l'est pas, l'effacement n'a pas lieu. Ce n'est pas
/// un assouplissement de la garde de confirmation ci-dessus, qui reste entière :
/// c'est la levée d'un blocage technique qui empêchait un effacement pourtant
/// voulu et confirmé — le cas rencontré sur le terrain.
///
/// La lecture mémorisée par `read_rc5` est **oubliée** en sortie : elle désigne
/// un contenu que l'appareil n'a plus. La laisser permettrait d'importer, après
/// coup, des données que plus rien ne permet de relire — donc de croire à tort
/// qu'on peut encore vérifier.
#[tauri::command(rename_all = "snake_case")]
pub async fn reset_rc5(
    state: State<'_, AppState>,
    port_name: Option<String>,
    confirmation: String,
) -> AppResult<Rc5ResetReport> {
    if confirmation.trim() != CONFIRMATION_REINITIALISATION {
        return Err(elitech_serial::SerialError::ResetNotConfirmed.into());
    }

    let (port_name, rapport) = tauri::async_runtime::spawn_blocking(move || {
        let nom = match port_name {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => elitech_serial::find_rc5()?.0.port_name,
        };
        elitech_serial::reset_port(&nom).map(|r| (nom, r))
    })
    .await
    .map_err(|e| AppError::Other(format!("La réinitialisation n'a pas pu aboutir : {e}")))??;

    // Seule la lecture de **ce** port est oubliée : celles des autres loggers
    // branchés décrivent toujours un contenu qu'ils ont encore.
    state.rc5_reading.lock().unwrap().remove(&port_name);

    Ok(Rc5ResetReport {
        port_name,
        logger_serial: rapport.after.device_number.clone(),
        records_erased: rapport.before.rec_count,
        memory_cleared: rapport.memory_cleared(),
        parameters_preserved: rapport.parameters_preserved(),
        changed_parameters: rapport.changed_parameters.clone(),
        recording_stopped: rapport.recording_stopped,
        message: rapport.message(),
    })
}

/// Bilan d'un arrêt d'enregistrement, tel que la page RC-5 l'affiche.
#[derive(Debug, Clone, Serialize)]
pub struct Rc5StopReport {
    pub port_name: String,
    pub logger_serial: Option<String>,
    pub work_status: String,
    /// Mesures en mémoire **après** l'arrêt — inchangé par rapport à avant,
    /// contrairement à [`Rc5ResetReport::records_erased`]. Affiché pour que
    /// l'utilisateur voie que rien n'a disparu.
    pub rec_count: u16,
}

/// **Arrête l'enregistrement du logger physique, sans effacer ses mesures.**
///
/// À ne pas confondre avec [`reset_rc5`] : celui-là efface la mémoire par
/// construction (c'est l'écriture de paramètres qui l'accompagne). Celui-ci
/// envoie la commande `08 00` ([`crate::elitech_serial::CMD_STOP`]), trouvée
/// par sondage sur un appareil réel et vérifiée isolément — voir sa
/// documentation pour le détail de cette vérification et ses limites.
///
/// Pas de confirmation exigée ici, contrairement à `reset_rc5` : rien
/// d'irréversible ne se produit sur les données du logger.
#[tauri::command(rename_all = "snake_case")]
pub async fn stop_rc5(port_name: Option<String>) -> AppResult<Rc5StopReport> {
    let (port_name, apres) = tauri::async_runtime::spawn_blocking(move || {
        let nom = match port_name {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => elitech_serial::find_rc5()?.0.port_name,
        };
        elitech_serial::stop_port(&nom).map(|r| (nom, r))
    })
    .await
    .map_err(|e| AppError::Other(format!("L'arrêt n'a pas pu aboutir : {e}")))??;

    Ok(Rc5StopReport {
        port_name,
        logger_serial: apres.device_number.clone(),
        work_status: apres.work_status.label_fr(),
        rec_count: apres.rec_count,
    })
}

// ---------------------------------------------------------------------------
// Saisie manuelle sur site
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ManualEntryPayload {
    #[serde(flatten)]
    pub common: ImportCommon,
    pub entry: ManualEntryInput,
}

/// Enregistre une saisie manuelle : une valeur par jour, sur une plage horaire.
///
/// Aucun chemin d'écriture parallèle : la saisie devient des `RawMeasurement` et
/// rejoint [`persist_import`] comme un fichier. Elle hérite donc, sans une ligne
/// de plus, de la détection de ré-import par empreinte, du contrôle des bornes de
/// période et du recalibrage d'horloge — voir [`crate::manual_entry`].
///
/// Deux champs sont **imposés** ici plutôt que laissés au frontend :
/// * le `SourceType`, qui vaut toujours [`SourceType::ManualEntry`]. Le laisser
///   choisir permettrait de faire passer une estimation pour une mesure ;
/// * `raw_file_reference` et le libellé par défaut, qui portent la plage horaire
///   et la nature de la valeur — la seule trace de ce qui a été saisi.
#[tauri::command]
pub fn import_manual_entry(
    state: State<'_, AppState>,
    payload: ManualEntryPayload,
) -> AppResult<ImportOutcome> {
    import_manual_entry_impl(&state.db, &payload)
}

/// Corps de [`import_manual_entry`], testable sans `State`.
fn import_manual_entry_impl(db: &Db, payload: &ManualEntryPayload) -> AppResult<ImportOutcome> {
    let (saisie, lignes) = parse_manual_entry(&payload.entry)?;

    let mut commun = payload.common.clone();
    // Il n'y a pas de fichier : la traçabilité, c'est la description de la
    // saisie. Même intention que « Lecture directe RC-5 — COM8 » pour la voie
    // USB, dans le champ prévu pour ça.
    let description = saisie.series_label();
    if commun
        .file_name
        .as_ref()
        .map(|f| f.trim().is_empty())
        .unwrap_or(true)
    {
        commun.file_name = Some(description.clone());
    }
    if commun
        .label
        .as_ref()
        .map(|l| l.trim().is_empty())
        .unwrap_or(true)
    {
        commun.label = Some(description);
    }
    // Aucun numéro de série : il n'y a pas d'appareil. Un numéro saisi dans le
    // champ commun serait trompeur — il désignerait un logger qui n'a rien
    // enregistré ce jour-là.
    commun.logger_serial = None;

    persist_import(
        db,
        &commun,
        SourceType::ManualEntry,
        lignes,
        None,
        // Ce n'est pas une cadence d'échantillonnage mais l'**étendue** que
        // couvre chaque valeur. C'est la seule durée qui ait un sens ici, et
        // c'est celle qu'il faut connaître pour relire la série sans se
        // tromper : une valeur « 6 h–9 h » ne vaut pas pour la journée.
        Some(saisie.range_seconds()),
        None,
    )
}

// ---------------------------------------------------------------------------
// ROMMA — import d'une archive
// ---------------------------------------------------------------------------

/// Ce que l'analyse d'une archive ROMMA apprend, avant tout enregistrement.
#[derive(Debug, Clone, Serialize)]
pub struct RommaAnalysis {
    pub measurement_count: usize,
    /// Lignes écartées — **le total**, pas seulement celles qui sont détaillées.
    pub skipped_count: usize,
    pub warning_count: usize,
    pub warnings: Vec<ImportWarning>,
    pub duplicate_count: usize,
    pub total_data_lines: usize,
    pub columns: Vec<String>,
    pub units: Vec<(String, String)>,
    pub sample_interval_seconds: Option<i64>,
    pub period_start_utc: Option<DateTime<Utc>>,
    pub period_end_utc: Option<DateTime<Utc>>,
    /// `true` : le rayonnement vient de `RadSolaireMax`, `RadSolaire` étant vide.
    pub solar_from_max: bool,
    /// Couverture réelle de chaque champ facultatif.
    pub field_counts: RommaFieldCounts,
    pub sample: Vec<crate::romma_import::RommaRecord>,
}

fn analyser_romma(payload: &RommaPayload) -> RommaAnalysis {
    RommaAnalysis {
        measurement_count: payload.records.len(),
        skipped_count: payload.skipped_count,
        warning_count: payload.warnings.len(),
        warnings: payload
            .warnings
            .iter()
            .take(MAX_WARNINGS_RETURNED)
            .cloned()
            .collect(),
        duplicate_count: payload.duplicate_count,
        total_data_lines: payload.total_data_lines,
        columns: payload.columns.clone(),
        units: payload.units.clone(),
        sample_interval_seconds: payload.sample_interval_seconds,
        period_start_utc: payload.first_utc(),
        period_end_utc: payload.last_utc(),
        solar_from_max: payload.solar_from_max,
        field_counts: payload.field_counts,
        sample: payload.records.iter().take(ROMMA_SAMPLE_LEN).cloned().collect(),
    }
}

/// [`analyser_romma`] pour une archive lue en flux : même analyse, sans que les
/// mesures aient jamais été gardées en mémoire.
fn analyser_romma_apercu(apercu: &RommaApercu) -> RommaAnalysis {
    RommaAnalysis {
        measurement_count: apercu.record_count,
        skipped_count: apercu.skipped_count,
        warning_count: apercu.warnings.len(),
        warnings: apercu
            .warnings
            .iter()
            .take(MAX_WARNINGS_RETURNED)
            .cloned()
            .collect(),
        // Une archive n'est lue en flux que si elle est sans doublon.
        duplicate_count: 0,
        total_data_lines: apercu.total_data_lines,
        columns: apercu.columns().to_vec(),
        units: apercu.units().to_vec(),
        sample_interval_seconds: apercu.sample_interval_seconds,
        period_start_utc: Some(apercu.first_utc),
        period_end_utc: Some(apercu.last_utc),
        solar_from_max: apercu.solar_from_max,
        field_counts: apercu.field_counts,
        sample: apercu.sample.clone(),
    }
}

/// Analyse une archive ROMMA **déjà en mémoire**, sans rien écrire.
#[tauri::command]
pub fn analyze_romma(content: String) -> AppResult<RommaAnalysis> {
    Ok(match lire_romma(&content)? {
        RommaLecture::EnFlux(apercu) => analyser_romma_apercu(&apercu),
        RommaLecture::Complete(payload) => analyser_romma(&payload),
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct RommaImportPayload {
    pub point_id: String,
    /// Contenu du CSV, déjà décodé côté frontend.
    pub content: String,
    pub file_name: Option<String>,
    pub role: String,
    /// Identifiant / nom de la station ROMMA, saisi librement. Purement
    /// documentaire : ROMMA n'a pas d'annuaire interrogeable, et l'archive ne
    /// porte le nom de la station que dans son nom de fichier.
    pub station_id: Option<String>,
    pub label: Option<String>,
    #[serde(default)]
    pub force_duplicate: bool,
}

#[tauri::command]
pub fn import_romma(
    state: State<'_, AppState>,
    payload: RommaImportPayload,
) -> AppResult<ImportOutcome> {
    import_romma_impl(&state.db, payload)
}

/// Corps de [`import_romma`], testable sans `State`.
///
/// ## Ce que cette voie a de particulier
///
/// * **Source nativement UTC** : la colonne `timestamp` est un epoch, donc
///   `clock_offset_seconds = 0`, `raw_timestamp == timestamp_utc` et
///   `raw_timestamp_was_utc = true`. Aucun réglage d'horloge n'est demandé à
///   l'utilisateur — en proposer un l'inviterait à fausser des instants justes.
/// * **Six champs météo** que les autres sources laissent à `NULL`.
/// * **Gros volume** : l'archive de référence fait 477 000 mesures. L'écriture
///   passe par le chemin de lots de `Db::insert_series_with_measurements`.
/// * **Pas de complément de série existante** : contrairement à Infoclimat, il
///   n'y a pas d'API à rappeler. Réimporter une archive plus récente crée une
///   nouvelle série, et la détection de ré-import fait son travail habituel.
///
/// ## Mémoire
///
/// L'archive de référence est **déjà triée et sans doublon** : elle est lue en
/// flux ([`lire_romma`]). Aucune liste de mesures n'est gardée — le texte est
/// relu une fois pour l'empreinte, une fois pour l'insertion, et seuls
/// (instant, °C) sont retenus pour les statistiques.
///
/// Une archive à trier ou à dédoublonner prend le chemin complet : toutes ses
/// mesures en mémoire, le texte libéré dès qu'elles sont lues. Le résultat est le
/// même dans les deux cas — mêmes lignes, même empreinte, même bilan.
pub fn import_romma_impl(db: &Db, mut payload: RommaImportPayload) -> AppResult<ImportOutcome> {
    let point = db.get_point(&payload.point_id)?;
    let role = SeriesRole::parse(&payload.role)?;

    let (lot, source) = match lire_romma(&payload.content)? {
        RommaLecture::Complete(parsed) => {
            // Plusieurs dizaines de Mo de texte dont plus rien ne dépend.
            payload.content = String::new();
            let RommaPayload {
                records,
                warnings,
                skipped_count,
                duplicate_count,
                sample_interval_seconds,
                ..
            } = parsed;
            let lot = LotRomma {
                count: records.len(),
                empreinte: series_fingerprint_of(&records),
                sample_interval_seconds,
                skipped_count,
                duplicate_count,
                warnings,
            };
            (lot, SourceRomma::Complete(records))
        }
        RommaLecture::EnFlux(mut apercu) => {
            let mut empreinte = FingerprintBuilder::new(
                apercu.record_count,
                Some(apercu.first_utc),
                Some(apercu.last_utc),
            );
            for r in apercu.records(&payload.content) {
                empreinte.push(&r);
            }
            let lot = LotRomma {
                count: apercu.record_count,
                empreinte: empreinte.finish(),
                sample_interval_seconds: apercu.sample_interval_seconds,
                skipped_count: apercu.skipped_count,
                duplicate_count: 0,
                warnings: std::mem::take(&mut apercu.warnings),
            };
            (lot, SourceRomma::EnFlux(apercu))
        }
    };

    if lot.count == 0 {
        return Err(AppError::Other(
            "Aucune mesure exploitable dans cette archive : rien n'a été enregistré.".to_string(),
        ));
    }

    let series_id = Uuid::new_v4().to_string();

    let empreinte = lot.empreinte;
    if !payload.force_duplicate {
        if let Some(duplicate) = detecter_reimport(db, &point, &empreinte, lot.count)? {
            return Ok(ImportOutcome::DuplicateSuspected {
                duplicate: Box::new(duplicate),
            });
        }
    }

    let evidence = romma_date_evidence(lot.count);
    let station_id = payload
        .station_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        // À défaut de saisie, on reprend celle du point : elle a été renseignée
        // dans le formulaire précisément pour ça.
        .or_else(|| point.romma_station_id.clone());

    let series = LoggerSeries {
        id: series_id.clone(),
        point_id: point.id.clone(),
        source_type: SourceType::RommaImport,
        role,
        logger_serial: None,
        imported_at: now_utc(),
        raw_file_reference: payload
            .file_name
            .clone()
            .filter(|f| !f.trim().is_empty()),
        clock_offset_seconds: 0,
        raw_timestamp_was_utc: true,
        label: payload.label.clone().filter(|l| !l.trim().is_empty()),
        sample_interval_seconds: lot.sample_interval_seconds,
        // ROMMA ne publie pas de mention de licence dans ses exports.
        data_license: None,
        station_id,
        last_api_fetch_at: None,
        date_order: evidence.order,
        date_format_uncertain: false,
        content_fingerprint: Some(empreinte.clone()),
        measurement_count: 0,
    };

    // Chaque `Measurement` est fabriqué au moment de son écriture, puis jeté.
    // L'identifiant, lui, est alloué une fois et partagé.
    let sid: std::sync::Arc<str> = series_id.as_str().into();
    let (ecrites, stats) = match &source {
        SourceRomma::Complete(records) => {
            let mesures = records.iter().map(|r| mesure_romma(&sid, r));
            let ecrites = db.insert_series_with_measurements_iter(&series, mesures)?;
            (ecrites, period_stats(records))
        }
        SourceRomma::EnFlux(apercu) => {
            let mut echantillons: Vec<TempSample> = Vec::with_capacity(apercu.record_count);
            let mesures = apercu.records(&payload.content).map(|r| {
                echantillons.push(TempSample::new(r.timestamp_utc, r.temperature_celsius));
                mesure_romma(&sid, &r)
            });
            let ecrites = db.insert_series_with_measurements_iter(&series, mesures)?;
            (ecrites, period_stats(&echantillons))
        }
    };

    Ok(ImportOutcome::imported(ImportReport {
        series_id: series.id,
        point_id: point.id,
        point_name: point.name,
        source_type: SourceType::RommaImport,
        role,
        imported_count: ecrites,
        updated_count: 0,
        unchanged_count: 0,
        // Le total, pas le nombre d'avertissements retenus : une archive de neuf
        // ans écarte plus de 10 000 lignes sans température, et n'en détailler
        // que les premières ne doit pas faire mentir le compte.
        skipped_count: lot.skipped_count,
        duplicate_count: lot.duplicate_count,
        warning_count: lot.skipped_count,
        warnings: lot
            .warnings
            .into_iter()
            .take(MAX_WARNINGS_RETURNED)
            .collect(),
        period_start_utc: stats.first_utc,
        period_end_utc: stats.last_utc,
        clock_offset_seconds: 0,
        raw_timestamp_was_utc: true,
        logger_serial: None,
        sample_interval_seconds: lot.sample_interval_seconds,
        layout: None,
        date_order: evidence,
        date_format_uncertain: false,
        content_fingerprint: Some(empreinte),
        forced_duplicate: payload.force_duplicate,
        data_license: None,
        attribution: Some(crate::romma_import::ATTRIBUTION.to_string()),
        // Une station fixe ne se met pas en poche — même raison que pour
        // Infoclimat : la détection de saut n'a de sens que pour un logger mobile.
        anomalies: Vec::new(),
        stats,
    }))
}

/// Ce que l'import ROMMA doit savoir d'une archive, qu'elle ait été lue en flux
/// ou en entier.
struct LotRomma {
    count: usize,
    empreinte: String,
    sample_interval_seconds: Option<i64>,
    skipped_count: usize,
    duplicate_count: usize,
    warnings: Vec<ImportWarning>,
}

/// D'où l'insertion tire ses mesures.
enum SourceRomma {
    /// Chemin complet : mesures triées et dédoublonnées, en mémoire.
    Complete(Vec<RommaRecord>),
    /// Lecture en flux : les mesures se relisent dans `payload.content`.
    EnFlux(RommaApercu),
}

fn mesure_romma(series_id: &std::sync::Arc<str>, r: &RommaRecord) -> Measurement {
    Measurement {
        humidity_percent: r.humidity_percent,
        pressure_hpa: r.pressure_hpa,
        wind_avg_kmh: r.wind_avg_kmh,
        wind_gust_kmh: r.wind_gust_kmh,
        wind_direction_degrees: r.wind_direction_degrees,
        rain_mm: r.rain_mm,
        solar_radiation: r.solar_radiation,
        ..Measurement::new_utc(series_id.clone(), r.timestamp_utc, r.temperature_celsius)
    }
}

// ---------------------------------------------------------------------------
// Infoclimat — réglages
// ---------------------------------------------------------------------------

/// Clé de réglage : intervalle minimal, en minutes, entre deux rafraîchissements
/// automatiques d'une **même** série.
pub const SETTING_REFRESH_MINUTES: &str = "infoclimat_refresh_min_minutes";

/// Valeur par défaut : une heure.
///
/// Ni codée en dur dans le client, ni imposée : c'est le point de départ que
/// l'écran Paramètres laisse modifier. Une heure est confortable pour une station
/// qui publie toutes les 10 minutes, et reste dans l'esprit du « fair use »
/// qu'Infoclimat demande sur une plateforme maintenue par des bénévoles.
pub const DEFAULT_REFRESH_MINUTES: i64 = 60;

/// Bornes acceptées : de 1 minute à une semaine.
const MIN_REFRESH_MINUTES: i64 = 1;
const MAX_REFRESH_MINUTES: i64 = 7 * 24 * 60;

/// Nombre de jours récupérés au tout premier appel d'une station, quand aucune
/// mesure n'existe encore et qu'il n'y a donc pas de point de reprise.
const DEFAULT_FIRST_FETCH_DAYS: i64 = 7;

/// Ce que l'écran Paramètres affiche et modifie.
#[derive(Debug, Clone, Serialize)]
pub struct InfoclimatSettings {
    pub refresh_min_minutes: i64,
    /// Statut de la clé — **jamais la clé elle-même** (cf. [`KeyStatus`]).
    pub key: KeyStatus,
    pub attribution: String,
    pub max_days_per_request: i64,
    pub key_file: String,
    pub key_signup_url: String,
}

fn refresh_minutes(state: &State<'_, AppState>) -> AppResult<i64> {
    Ok(state
        .db
        .get_setting(SETTING_REFRESH_MINUTES)?
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| (MIN_REFRESH_MINUTES..=MAX_REFRESH_MINUTES).contains(v))
        .unwrap_or(DEFAULT_REFRESH_MINUTES))
}

// `async` : hors du thread principal, pour ne pas figer l'interface
// quand la base est occupée (synchro Cloud, import).
#[tauri::command(async)]
pub fn infoclimat_settings(state: State<'_, AppState>) -> AppResult<InfoclimatSettings> {
    Ok(InfoclimatSettings {
        refresh_min_minutes: refresh_minutes(&state)?,
        key: infoclimat_client::key_status(),
        attribution: infoclimat::ATTRIBUTION.to_string(),
        max_days_per_request: infoclimat::MAX_DAYS_PER_REQUEST,
        key_file: format!(
            "{}/{}",
            infoclimat_client::KEY_DIR,
            infoclimat_client::KEY_FILE
        ),
        key_signup_url: infoclimat_client::KEY_SOURCE.signup_url.to_string(),
    })
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_infoclimat_refresh_minutes(
    state: State<'_, AppState>,
    minutes: i64,
) -> AppResult<InfoclimatSettings> {
    if !(MIN_REFRESH_MINUTES..=MAX_REFRESH_MINUTES).contains(&minutes) {
        return Err(AppError::Other(format!(
            "Intervalle hors bornes : {minutes} minute(s). Attendu entre \
             {MIN_REFRESH_MINUTES} et {MAX_REFRESH_MINUTES} minutes (7 jours)."
        )));
    }
    state
        .db
        .set_setting(SETTING_REFRESH_MINUTES, &minutes.to_string())?;
    infoclimat_settings(state)
}

/// Enregistre la clé Infoclimat saisie dans les Paramètres.
#[tauri::command]
pub fn set_infoclimat_key(
    state: State<'_, AppState>,
    key: String,
) -> AppResult<InfoclimatSettings> {
    let chemin = infoclimat_client::save_key(&key).map_err(|e| AppError::Other(e.to_string()))?;

    if infoclimat_client::env_var_prend_le_dessus() {
        return Err(AppError::Other(format!(
            "Clé écrite dans {}, mais la variable d'environnement {} est définie sur ce \
             poste et reste prioritaire. Supprimez-la pour que cette clé serve.",
            chemin.display(),
            infoclimat_client::KEY_ENV_VAR
        )));
    }

    infoclimat_settings(state)
}

// ---------------------------------------------------------------------------
// Relai SSH pour Infoclimat (serveur Debian distant)
// ---------------------------------------------------------------------------

// `async` : hors du thread principal, pour ne pas figer l'interface
// quand la base est occupée (synchro Cloud, import).
#[tauri::command(async)]
pub fn get_ssh_relay_status(state: State<'_, AppState>) -> AppResult<crate::ssh_relay::SshRelayStatus> {
    crate::ssh_relay::get_status(&state.db)
}

#[tauri::command]
pub fn save_ssh_relay_config(
    state: State<'_, AppState>,
    input: crate::ssh_relay::SshRelayInput,
) -> AppResult<crate::ssh_relay::SshRelayStatus> {
    crate::ssh_relay::save_config(&state.db, &input)
}

#[tauri::command]
pub fn set_ssh_relay_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> AppResult<crate::ssh_relay::SshRelayStatus> {
    crate::ssh_relay::set_enabled(&state.db, enabled)
}

/// Résultat d'un test du relai, structuré pour que l'UI sache quoi proposer :
/// accepter une empreinte inconnue, alerter sur une empreinte changée, ou
/// simplement afficher le message.
#[derive(Debug, Clone, Serialize)]
pub struct SshTestResult {
    pub ok: bool,
    pub message: String,
    /// `trusted` | `unknown` | `changed` | `unchecked` (échec avant la clé).
    pub host_key_status: String,
    /// Empreinte présentée par le serveur, quand on l'a vue.
    pub host_key: Option<String>,
}

#[tauri::command]
pub async fn test_ssh_relay_connection(
    state: State<'_, AppState>,
    input: Option<crate::ssh_relay::SshRelayInput>,
) -> AppResult<SshTestResult> {
    let config = if let Some(inp) = input {
        let mut full_input = inp.clone();
        let port = full_input.port.unwrap_or(crate::ssh_relay::DEFAULT_SSH_PORT);
        // Un secret enregistré n'est réinjecté que vers la destination pour
        // laquelle il a été saisi — jamais vers un hôte tapé dans le formulaire
        // (ou fourni par un script) : audit de sécurité du 18/09, §2.
        let meme_destination =
            crate::ssh_relay::same_destination(&state.db, &full_input.host, port, &full_input.user)?;
        if meme_destination
            && full_input.auth_type == crate::ssh_relay::SshAuthType::Password
            && full_input.password.as_deref().unwrap_or("").is_empty()
        {
            full_input.password = state.db.get_setting(crate::ssh_relay::SETTING_SSH_PASSWORD)?;
        }
        if full_input.auth_type == crate::ssh_relay::SshAuthType::Key {
            if full_input.key_path.as_deref().unwrap_or("").is_empty() {
                full_input.key_path = state.db.get_setting(crate::ssh_relay::SETTING_SSH_KEY_PATH)?;
            }
            if meme_destination && full_input.key_passphrase.as_deref().unwrap_or("").is_empty() {
                full_input.key_passphrase =
                    state.db.get_setting(crate::ssh_relay::SETTING_SSH_KEY_PASSPHRASE)?;
            }
        }
        let auth = match full_input.auth_type {
            crate::ssh_relay::SshAuthType::Password => {
                let pwd = full_input.password.unwrap_or_default();
                if pwd.is_empty() {
                    return Err(AppError::Other("Mot de passe SSH requis pour le test.".into()));
                }
                crate::ssh_relay::SshAuth::Password(pwd)
            }
            crate::ssh_relay::SshAuthType::Key => {
                let p = full_input.key_path.unwrap_or_default();
                if p.is_empty() || !std::path::Path::new(&p).is_file() {
                    return Err(AppError::Other(format!("Fichier de clé introuvable : {p}")));
                }
                crate::ssh_relay::SshAuth::Key {
                    path: std::path::PathBuf::from(p),
                    passphrase: full_input.key_passphrase,
                }
            }
        };
        let host_key = crate::ssh_relay::trusted_key_for(&state.db, &full_input.host, port)?;
        crate::ssh_relay::SshRelayConfig {
            host: full_input.host,
            port,
            user: full_input.user,
            auth,
            enabled: true,
            host_key,
            appareil: None,
        }
    } else {
        match crate::ssh_relay::load_config(&state.db)? {
            Some(cfg) => cfg,
            None => return Err(AppError::Other("Aucun relai SSH configuré.".into())),
        }
    };

    use crate::ssh_relay::SshFailure;
    let attendue = config.host_key.clone();
    Ok(match crate::ssh_relay::test_connection(&config).await {
        Ok(message) => SshTestResult { ok: true, message, host_key_status: "trusted".into(), host_key: attendue },
        Err(SshFailure::UnknownHostKey(fp)) => SshTestResult {
            ok: false,
            message: "Premier contact avec ce serveur : vérifiez son empreinte avant de lui faire \
                      confiance. Aucun identifiant n'a été envoyé."
                .into(),
            host_key_status: "unknown".into(),
            host_key: Some(fp),
        },
        Err(e @ SshFailure::ChangedHostKey { .. }) => {
            let vue = match &e {
                SshFailure::ChangedHostKey { seen, .. } => Some(seen.clone()),
                _ => None,
            };
            SshTestResult { ok: false, message: e.to_string(), host_key_status: "changed".into(), host_key: vue }
        }
        Err(e) => SshTestResult { ok: false, message: e.to_string(), host_key_status: "unchecked".into(), host_key: None },
    })
}

/// Accepte l'empreinte d'un serveur SSH, après vérification par l'utilisateur.
#[tauri::command(rename_all = "snake_case")]
pub fn trust_ssh_host_key(
    state: State<'_, AppState>,
    host: String,
    port: u16,
    fingerprint: String,
) -> AppResult<crate::ssh_relay::SshRelayStatus> {
    crate::ssh_relay::trust_host_key(&state.db, &host, port, &fingerprint)
}

/// Oublie l'empreinte acceptée : la prochaine connexion redemandera confirmation.
#[tauri::command]
pub fn forget_ssh_host_key(state: State<'_, AppState>) -> AppResult<crate::ssh_relay::SshRelayStatus> {
    crate::ssh_relay::forget_host_key(&state.db)
}

/// Statut de la configuration signée reçue du relai (lecture seule, aucun
/// secret). Préparée pour un lot ultérieur : aucun écran ne l'appelle encore.
#[tauri::command(async)]
pub fn get_relais_config_status(
    state: State<'_, AppState>,
) -> AppResult<crate::relais_config::RelaisConfigStatus> {
    crate::relais_config::statut(&state.db)
}

// ---------------------------------------------------------------------------
// Partage par code (serveur gws-share, voir `share.rs`)
// ---------------------------------------------------------------------------

/// Crée un partage des points demandés et rend son code.
///
/// La base est lue ici, sur le fil de la commande ; seul l'envoi part dans un
/// `spawn_blocking`, comme les autres appels réseau de ce fichier.
#[tauri::command]
pub async fn share_create(
    app: AppHandle,
    request: crate::share::ShareRequest,
) -> AppResult<crate::share::ShareCreated> {
    let state = app.state::<AppState>();
    let (doc, measurement_count) = crate::share::build_document(&state.db, &request)?;
    let body = crate::share::serialize_document(&doc)?;
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        crate::share::publish(&state.db, &request, &doc, measurement_count, body)
    })
    .await
    .map_err(|e| AppError::Other(format!("Le partage n'a pas pu aboutir : {e}")))?
}

/// Aperçu d'un partage reçu : ses métadonnées, sans ses données.
#[tauri::command]
pub async fn share_preview(code: String) -> AppResult<crate::share::SharePreview> {
    tauri::async_runtime::spawn_blocking(move || crate::share::fetch_preview(&code))
        .await
        .map_err(|e| AppError::Other(format!("L'aperçu n'a pas pu aboutir : {e}")))?
        .map_err(AppError::from)
}

/// Télécharge, revalide et importe un partage.
#[tauri::command]
pub async fn share_import(
    app: AppHandle,
    code: String,
    target: crate::share::ImportTarget,
) -> AppResult<crate::share::ShareImportReport> {
    let normalise = crate::share::normalize_code(&code)?;
    let pour_appel = normalise.clone();
    let doc = tauri::async_runtime::spawn_blocking(move || crate::share::fetch_document(&pour_appel))
        .await
        .map_err(|e| AppError::Other(format!("Le téléchargement n'a pas pu aboutir : {e}")))??;
    let state = app.state::<AppState>();
    crate::share::import_document(&state.db, &normalise, &doc, &target)
}

/// Partages créés depuis ce poste — sans leur jeton de révocation.
// `async` : hors du thread principal, pour ne pas figer l'interface
// quand la base est occupée (synchro Cloud, import).
#[tauri::command(async)]
pub fn share_list_issued(state: State<'_, AppState>) -> AppResult<Vec<crate::share::IssuedShare>> {
    crate::share::list_issued(&state.db)
}

/// Révoque un partage créé depuis ce poste : il cesse aussitôt d'être lisible.
#[tauri::command]
pub async fn share_revoke(app: AppHandle, code: String) -> AppResult<Vec<crate::share::IssuedShare>> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        crate::share::revoke(&state.db, &code)
    })
    .await
    .map_err(|e| AppError::Other(format!("La révocation n'a pas pu aboutir : {e}")))??;
    crate::share::list_issued(&app.state::<AppState>().db)
}

// ---------------------------------------------------------------------------
// Avertissement de mise à jour (voir `update.rs`)
// ---------------------------------------------------------------------------

// `async` : hors du thread principal, pour ne pas figer l'interface
// quand la base est occupée (synchro Cloud, import).
#[tauri::command(async)]
pub fn update_settings(state: State<'_, AppState>) -> AppResult<crate::update::UpdateSettings> {
    crate::update::settings(&state.db)
}

/// `mode` : `notify` (vérifier et prévenir) ou `off` (ne jamais vérifier seul).
#[tauri::command]
pub fn set_update_mode(state: State<'_, AppState>, mode: crate::update::UpdateMode) -> AppResult<crate::update::UpdateSettings> {
    crate::update::set_mode(&state.db, mode)
}

/// Canal de mise à jour : `stable` (défaut) ou `beta`.
#[tauri::command]
pub fn set_update_channel(state: State<'_, AppState>, channel: crate::update::UpdateChannel) -> AppResult<crate::update::UpdateSettings> {
    crate::update::set_channel(&state.db, channel)
}

#[tauri::command]
pub fn skip_update_version(state: State<'_, AppState>, version: String) -> AppResult<crate::update::UpdateSettings> {
    crate::update::skip_version(&state.db, &version)
}

/// Vérifie s'il existe une version plus récente. `manual` : demandée depuis les
/// Paramètres, faite même quand les vérifications automatiques sont coupées.
#[tauri::command]
pub async fn check_for_update(state: State<'_, AppState>, manual: bool) -> AppResult<crate::update::UpdateCheck> {
    let reglages = crate::update::settings(&state.db)?;
    if reglages.mode == crate::update::UpdateMode::Off && !manual {
        return crate::update::check(&state.db, false, || Ok(None));
    }
    let canal = reglages.channel;
    let trouve = tauri::async_runtime::spawn_blocking(move || crate::update::fetch_available(canal))
        .await
        .map_err(|e| AppError::Other(format!("La vérification n'a pas pu aboutir : {e}")))?;
    crate::update::check(&state.db, manual, move || trouve)
}

/// Ouvre la page de téléchargement d'une mise à jour (domaine vérifié en Rust).
#[tauri::command]
pub fn open_update_download(url: String) -> AppResult<()> {
    crate::update::open_download(&url)
}

/// Inspecte un paquet de mise à jour .gws (métadonnées, changelog, version).
#[tauri::command]
pub fn inspect_gws_package(path: String) -> AppResult<crate::package::PackageInfo> {
    crate::package::inspect(std::path::Path::new(&path))
}

/// Applique la mise à jour depuis un paquet .gws et relance l'application.
#[tauri::command]
pub fn apply_gws_package(path: String) -> AppResult<()> {
    crate::package::apply_update(std::path::Path::new(&path))
}

/// Confirme au gardien de mise à jour que l'interface a démarré. Appelée tout
/// en haut de `boot()`, avant les chargements (synchro Cloud comprise) qui
/// pourraient dépasser le délai du gardien et faire restaurer à tort une
/// nouvelle version saine. Sans effet hors mise à jour en attente.
#[tauri::command]
pub fn confirmer_demarrage_maj() {
    crate::maj_gardien::confirmer_demarrage();
}

/// Version installée si ce lancement suit une mise à jour par paquet `.gws`
/// (une seule fois : l'écran de confirmation ne revient pas au rechargement).
#[tauri::command]
pub fn consommer_apres_maj(state: State<'_, AppState>) -> Option<String> {
    // Appelée par l'interface à son lancement : preuve que la nouvelle version
    // démarre vraiment. Le gardien de mise à jour n'a plus à intervenir.
    crate::maj_gardien::confirmer_demarrage();
    let mut drapeau = state.apres_maj.lock().unwrap();
    let oui = std::mem::take(&mut *drapeau);
    oui.then(|| crate::update::CURRENT_VERSION.to_string())
}

/// Récupère le paquet .gws passé en argument au démarrage s'il y en avait un.
#[tauri::command]
pub fn get_startup_package(state: State<'_, AppState>) -> AppResult<Option<crate::package::PackageInfo>> {
    let opt_path = state.startup_package.lock().unwrap().take();
    if let Some(p) = opt_path {
        match crate::package::inspect(std::path::Path::new(&p)) {
            Ok(info) => Ok(Some(info)),
            // Remonté à l'interface : un paquet refusé ne doit pas passer inaperçu.
            Err(e) => Err(e),
        }
    } else {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Projet Cloud (synchronisation chiffrée de bout en bout)
// ---------------------------------------------------------------------------

// `async` : hors du thread principal, pour ne pas figer l'interface
// quand la base est occupée (synchro Cloud, import).
#[tauri::command(async)]
pub fn cloud_status(state: State<'_, AppState>) -> AppResult<crate::cloud::CloudStatus> {
    crate::cloud::get_status(&state.db).map_err(Into::into)
}

#[tauri::command]
pub async fn cloud_register(
    app: AppHandle,
    account: String,
    password: String,
    invite: String,
) -> AppResult<crate::cloud::CloudStatus> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        crate::cloud::register(&account, &password, &invite, &state.db)
    })
    .await
    .map_err(|e| AppError::Other(format!("L'enregistrement n'a pas pu aboutir : {e}")))?
    .map_err(Into::into)
}

#[tauri::command]
pub async fn cloud_login(
    app: AppHandle,
    account: String,
    password: String,
) -> AppResult<crate::cloud::CloudStatus> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        crate::cloud::login(&account, &password, &state.db)
    })
    .await
    .map_err(|e| AppError::Other(format!("La connexion n'a pas pu aboutir : {e}")))?
    .map_err(Into::into)
}

#[tauri::command]
pub async fn cloud_logout(app: AppHandle) -> AppResult<crate::cloud::CloudStatus> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        crate::cloud::logout(&state.db)
    })
    .await
    .map_err(|e| AppError::Other(format!("La déconnexion n'a pas pu aboutir : {e}")))?
    .map_err(Into::into)
}

#[tauri::command]
pub async fn cloud_sync(app: AppHandle) -> AppResult<crate::cloud::CloudSyncResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // Chaque élément échangé part vers la fenêtre de progression.
        let r = crate::cloud::sync_avec_progression(&state.db, &|p| {
            let _ = app.emit("cloud-progress", p);
        });
        // Toujours émis, succès ou échec : la fenêtre se referme aussitôt.
        let _ = app.emit("cloud-progress-end", ());
        r
    })
    .await
    .map_err(|e| AppError::Other(format!("La synchronisation n'a pas pu aboutir : {e}")))?
    .map_err(Into::into)
}

/// Réglage de synchro d'un projet pour l'écran Paramètres.
#[derive(serde::Serialize)]
pub struct CloudProjectPref {
    pub id: String,
    pub name: String,
    /// `Some(true)` exclu, `Some(false)` synchronisé, `None` : automatique (seuil).
    pub excluded: Option<bool>,
    /// Projet d'exemple : jamais synchronisé, réglage non modifiable.
    pub demo: bool,
}

#[tauri::command(async)]
pub fn cloud_project_prefs(state: State<'_, AppState>) -> AppResult<Vec<CloudProjectPref>> {
    let mut r = Vec::new();
    for p in state.db.list_projects()? {
        let v = state.db.get_setting(&format!("{}{}", crate::cloud::SETTING_CLOUD_EXCLUDE_PREFIX, p.id))?;
        r.push(CloudProjectPref {
            id: p.id,
            name: p.name,
            excluded: v.as_deref().map(|x| x == "1"),
            demo: p.is_demo,
        });
    }
    Ok(r)
}

#[tauri::command(rename_all = "snake_case")]
pub fn cloud_set_project_excluded(state: State<'_, AppState>, project_id: String, excluded: bool) -> AppResult<()> {
    if !excluded && crate::demo::est_exemple(&state.db, &project_id) {
        return Err(AppError::Other("Un projet d'exemple n'est jamais synchronisé vers le Cloud.".into()));
    }
    state.db.set_setting(
        &format!("{}{}", crate::cloud::SETTING_CLOUD_EXCLUDE_PREFIX, project_id),
        if excluded { "1" } else { "0" },
    )
}

#[tauri::command]
pub fn cloud_set_auto_sync(state: State<'_, AppState>, enabled: bool) -> AppResult<()> {
    state.db.set_setting(
        crate::cloud::SETTING_CLOUD_AUTO_SYNC,
        if enabled { "1" } else { "0" },
    )?;
    Ok(())
}


#[tauri::command]
pub async fn pick_ssh_key_file(app: tauri::AppHandle) -> AppResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;

    let res = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Sélectionner la clé privée SSH")
            .blocking_pick_file()
            .and_then(|p| p.into_path().ok())
            .map(|p| p.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| AppError::Other(format!("Sélection de fichier interrompue : {e}")))?;

    Ok(res)
}

// ---------------------------------------------------------------------------
// Infoclimat — analyse préalable d'un contenu (aucune écriture)
// ---------------------------------------------------------------------------

/// Ce que l'analyse d'un JSON Infoclimat apprend, avant tout enregistrement.
#[derive(Debug, Clone, Serialize)]
pub struct InfoclimatAnalysis {
    pub station_id: String,
    pub station: Option<StationInfo>,
    pub available_stations: Vec<String>,
    pub measurement_count: usize,
    pub warning_count: usize,
    pub warnings: Vec<ImportWarning>,
    pub duplicate_count: usize,
    pub unreadable_complements: usize,
    pub sample_interval_seconds: Option<i64>,
    pub period_start_utc: Option<DateTime<Utc>>,
    pub period_end_utc: Option<DateTime<Utc>>,
    pub data_license: Option<String>,
    pub attribution: String,
    /// Quelques mesures interprétées, pour contrôle visuel.
    pub sample: Vec<InfoclimatRecord>,
    /// Combien des mesures portent un RSSI / une batterie exploitables.
    pub with_rssi: usize,
    pub with_battery: usize,
}

fn analyser(payload: InfoclimatPayload) -> InfoclimatAnalysis {
    let with_rssi = payload.records.iter().filter(|r| r.rssi_dbm.is_some()).count();
    let with_battery = payload.records.iter().filter(|r| r.battery.is_some()).count();

    InfoclimatAnalysis {
        period_start_utc: payload.first_utc(),
        period_end_utc: payload.last_utc(),
        data_license: payload.station.as_ref().and_then(|s| s.license.clone()),
        attribution: infoclimat::ATTRIBUTION.to_string(),
        sample: payload.records.iter().take(5).cloned().collect(),
        with_rssi,
        with_battery,
        measurement_count: payload.records.len(),
        warning_count: payload.warnings.len(),
        warnings: payload
            .warnings
            .into_iter()
            .take(MAX_WARNINGS_RETURNED)
            .collect(),
        station_id: payload.station_id,
        station: payload.station,
        available_stations: payload.available_stations,
        duplicate_count: payload.duplicate_count,
        unreadable_complements: payload.unreadable_complements,
        sample_interval_seconds: payload.sample_interval_seconds,
    }
}

/// Analyse un JSON Infoclimat **déjà en mémoire**, sans rien écrire.
#[tauri::command(rename_all = "snake_case")]
pub fn analyze_infoclimat(
    content: String,
    station_id: Option<String>,
) -> AppResult<InfoclimatAnalysis> {
    let payload = parse_infoclimat_auto(&content, station_id.as_deref())?;
    Ok(analyser(payload))
}

// ---------------------------------------------------------------------------
// Infoclimat — mode 1 : import d'un fichier téléchargé à la main
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct InfoclimatImportPayload {
    pub point_id: String,
    /// Contenu du fichier JSON, déjà décodé côté frontend.
    pub content: String,
    pub file_name: Option<String>,
    pub role: String,
    /// Station à extraire ; facultatif si le fichier n'en contient qu'une.
    pub station_id: Option<String>,
    pub label: Option<String>,
    /// `true` : compléter la série Infoclimat déjà rattachée à ce point pour
    /// cette station, au lieu d'en créer une nouvelle.
    ///
    /// Le choix reste **explicite**. La règle « on ne fusionne jamais rien
    /// automatiquement » (§1.3) interdit de décider à la place de l'utilisateur,
    /// mais rien n'empêche de lui proposer : réimporter deux exports successifs
    /// de la même station dans deux séries distinctes n'a aucun intérêt.
    #[serde(default)]
    pub append_to_existing: bool,

    /// `true` : créer la série même si ce contenu exact a déjà été importé sur
    /// ce point. Sans objet quand `append_to_existing` est vrai — on ne crée
    /// alors aucune série.
    #[serde(default)]
    pub force_duplicate: bool,
}

/// Importe un fichier JSON téléchargé depuis infoclimat.fr.
///
/// **Le parsing est strictement celui de l'appel direct** : les deux modes
/// appellent `infoclimat::parse_infoclimat` sur un `&str`. Aucun code de lecture
/// ne leur est propre — c'est ce qui garantit qu'un fichier validé à la main se
/// comporte comme une réponse d'API, et réciproquement.
#[tauri::command]
pub fn import_infoclimat(
    state: State<'_, AppState>,
    payload: InfoclimatImportPayload,
) -> AppResult<ImportOutcome> {
    import_infoclimat_impl(&state.db, payload)
}

/// Corps de [`import_infoclimat`], testable sans `State` (même raison que pour
/// [`import_elitech_impl`]).
///
/// Prise par valeur, comme [`import_romma_impl`] : le texte du fichier est
/// libéré dès qu'il est parsé.
pub fn import_infoclimat_impl(
    db: &Db,
    mut payload: InfoclimatImportPayload,
) -> AppResult<ImportOutcome> {
    let point = db.get_point(&payload.point_id)?;
    let role = SeriesRole::parse(&payload.role)?;
    let parsed = parse_infoclimat_auto(&payload.content, payload.station_id.as_deref())?;
    payload.content = String::new();

    if parsed.records.is_empty() {
        return Err(AppError::Other(
            "Aucune mesure exploitable dans ce fichier : rien n'a été enregistré.".to_string(),
        ));
    }

    let existante = if payload.append_to_existing {
        db.find_infoclimat_series(&point.id, &parsed.station_id)?
    } else {
        None
    };

    persist_infoclimat(
        db,
        &point,
        role,
        parsed,
        existante,
        payload
            .file_name
            .clone()
            .filter(|f| !f.trim().is_empty()),
        payload.label.clone().filter(|l| !l.trim().is_empty()),
        None,
        payload.force_duplicate,
    )
}

// ---------------------------------------------------------------------------
// Infoclimat — mode 2 : appel direct à l'API
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct InfoclimatFetchPayload {
    pub point_id: String,
    /// Station à interroger. Si absent, on prend celle rattachée au point.
    pub station_id: Option<String>,
    pub role: Option<String>,
    /// Profondeur d'historique, en jours, pour un **premier** appel (aucune
    /// mesure connue). Ignoré ensuite : on reprend à la dernière mesure.
    pub days_back: Option<i64>,
    /// Date de début explicite (ex: récupération d'anciennes données).
    pub start_date: Option<NaiveDate>,
    /// Date de fin explicite (ex: récupération d'anciennes données).
    pub end_date: Option<NaiveDate>,
    /// `true` : passer outre l'intervalle minimal de rafraîchissement.
    /// Réservé à un clic explicite de l'utilisateur.
    #[serde(default)]
    pub force: bool,
    /// `true` : créer la série même si ce contenu exact existe déjà sur ce point.
    ///
    /// Distinct de `force` à dessein : l'un passe outre un garde-fou de
    /// politesse envers Infoclimat, l'autre outre un risque de double comptage
    /// dans les statistiques. Les confondre ferait qu'un utilisateur pressé
    /// autoriserait sans le savoir la création d'une série jumelle.
    #[serde(default)]
    pub force_duplicate: bool,
}

/// Événement émis lors de la progression d'un appel à l'API Infoclimat.
pub const INFOCLIMAT_FETCH_PROGRESS_EVENT: &str = "infoclimat-fetch-progress";

/// Payload de progression envoyé au frontend pendant la récupération de tranches.
#[derive(Debug, Clone, Serialize)]
pub struct InfoclimatFetchProgressPayload {
    pub station_id: String,
    pub current: usize,
    pub total: usize,
    pub start: String,
    pub end: String,
}

/// Renvoie le quota journalier d'appels à l'API Infoclimat.
#[tauri::command]
pub fn get_infoclimat_quota() -> AppResult<crate::infoclimat_client::DailyQuotaInfo> {
    Ok(crate::infoclimat_client::load_daily_quota())
}

/// Bilan d'un appel direct.
#[derive(Debug, Clone, Serialize)]
pub struct FetchReport {
    pub series_id: String,
    pub point_id: String,
    pub point_name: String,
    pub station_id: String,
    pub station_name: Option<String>,
    /// Nombre de tranches de 7 jours effectivement appelées.
    pub window_count: usize,
    pub requested_start: NaiveDate,
    pub requested_end: NaiveDate,
    /// Séries créée à cette occasion, ou complétée ?
    pub created_series: bool,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub warning_count: usize,
    pub warnings: Vec<ImportWarning>,
    pub duplicate_count: usize,
    pub unreadable_complements: usize,
    pub sample_interval_seconds: Option<i64>,
    pub period_start_utc: Option<DateTime<Utc>>,
    pub period_end_utc: Option<DateTime<Utc>>,
    pub data_license: Option<String>,
    pub attribution: String,
    pub stats: PeriodStats,
}

/// Pendant d'[`ImportOutcome`] pour l'appel direct.
///
/// Le cas ne se présente qu'au **premier** appel sur une station : ensuite la
/// série existe et on la complète, ce qui ne crée aucun doublon. Mais ce premier
/// appel peut très bien retomber sur un JSON déjà importé à la main pour la même
/// période — d'où la même protection ici.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FetchOutcome {
    Fetched { report: Box<FetchReport> },
    DuplicateSuspected { duplicate: Box<DuplicateImportInfo> },
}

/// Récupère les données d'une station depuis l'API Infoclimat, deux cas :
///
/// * **premier appel** (aucune série API sur le point) : récupération
///   d'historique sur `days_back` jours — les « anciennes données » ;
/// * **appels suivants** : reprise depuis la dernière mesure en base
///   (`last_measurement_utc`) — les « dernières données ».
///
/// `last_api_fetch_at` (moment du dernier appel, garde-fou de fréquence) n'est
/// pas la dernière mesure reçue : une API peut répondre sans rien de neuf.
///
/// La commande est `async` pour que l'appel réseau parte dans un
/// `spawn_blocking` : le client HTTP est bloquant (le métier reste synchrone et
/// testable), et bloquer le thread principal figerait la fenêtre pendant les
/// quelques secondes d'un appel — voire la minute d'un rattrapage en plusieurs
/// tranches.
#[tauri::command]
pub async fn fetch_infoclimat(
    app: AppHandle,
    state: State<'_, AppState>,
    payload: InfoclimatFetchPayload,
) -> AppResult<FetchOutcome> {
    let point = state.db.get_point(&payload.point_id)?;

    let station_id = payload
        .station_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| point.infoclimat_station_id.clone())
        .ok_or_else(|| {
            AppError::Other(format!(
                "Aucune station Infoclimat n'est rattachée à « {} ». Renseignez son \
                 identifiant (par exemple STATIC0479) dans le formulaire de modification \
                 du point.",
                point.name
            ))
        })?;
    let station_id = station_id.trim().to_string();

    let existante = state.db.find_infoclimat_series(&point.id, &station_id)?;

    // --- Garde-fou de fréquence -------------------------------------------
    // Il ne protège pas Infoclimat d'un utilisateur pressé (un clic « forcer »
    // suffit) : il empêche surtout un rafraîchissement machinal de repartir
    // toutes les dix secondes sur une station qui ne publie qu'au quart d'heure.
    let intervalle = refresh_minutes(&state)?;
    if !payload.force {
        if let Some(serie) = existante.as_ref() {
            if let Some(dernier) = serie.last_api_fetch_at {
                let ecoule = Utc::now() - dernier;
                if ecoule < Duration::minutes(intervalle) {
                    let reste = intervalle - ecoule.num_minutes();
                    return Err(AppError::Other(format!(
                        "Dernier appel à l'API il y a {} minute(s). L'intervalle minimal \
                         configuré est de {intervalle} minute(s) : réessayez dans {} minute(s), \
                         ou relancez en forçant.",
                        ecoule.num_minutes().max(0),
                        reste.max(1)
                    )));
                }
            }
        }
    }

    // --- Période à demander ------------------------------------------------
    let aujourdhui = Utc::now().date_naive();
    let (debut, fin) = if let Some(custom_start) = payload.start_date {
        let custom_end = payload.end_date.unwrap_or(aujourdhui).min(aujourdhui);
        let custom_start = custom_start.min(custom_end);
        (custom_start, custom_end)
    } else {
        let debut = match existante.as_ref() {
            Some(serie) => match state.db.last_measurement_utc(&serie.id)? {
                Some(dernier) => dernier.date_naive(),
                None => aujourdhui - Duration::days(DEFAULT_FIRST_FETCH_DAYS - 1),
            },
            None => {
                let jours = payload
                    .days_back
                    .filter(|j| *j > 0)
                    .unwrap_or(DEFAULT_FIRST_FETCH_DAYS);
                aujourdhui - Duration::days(jours - 1)
            }
        };
        (debut.min(aujourdhui), aujourdhui)
    };

    // --- Appel réseau, hors du thread principal ---------------------------
    let station_pour_appel = station_id.clone();
    let ssh_relay = crate::ssh_relay::load_config(&state.db)?;
    // Copie pour la configuration signée du relai, relevée après l'appel.
    let relais_pour_config = ssh_relay.clone();
    let app_handle = app.clone();
    let station_pour_cb = station_pour_appel.clone();

    let resultat_appel = tauri::async_runtime::spawn_blocking(move || {
        let client = InfoclimatClient::from_local_key()?.with_ssh_relay(ssh_relay);
        let cb = move |current: usize, total: usize, win: crate::infoclimat::DateWindow| {
            let _ = app_handle.emit(
                INFOCLIMAT_FETCH_PROGRESS_EVENT,
                InfoclimatFetchProgressPayload {
                    station_id: station_pour_cb.clone(),
                    current,
                    total,
                    start: win.start.to_string(),
                    end: win.end.to_string(),
                },
            );
        };
        client.fetch_range_with_progress(&station_pour_appel, debut, fin, Some(cb))
    })
    .await;
    // Relai activé seulement : relevé de la configuration signée, en tâche de
    // fond et au plus toutes les 10 minutes. Retour immédiat, quel que soit le
    // résultat de l'appel ; relai désactivé ou absent : ne fait rien.
    crate::relais_config::declencher_si_opportun(app.clone(), relais_pour_config.as_ref());
    let outcome = resultat_appel
        .map_err(|e| AppError::Other(format!("L'appel à l'API n'a pas pu aboutir : {e}")))??;

    if outcome.records.is_empty() {
        return Err(AppError::Other(format!(
            "L'API n'a renvoyé aucune mesure pour {station_id} entre le {debut} et le \
             {fin}. La station est peut-être hors service sur cette période."
        )));
    }

    let role = match payload.role.as_deref() {
        Some(r) if !r.trim().is_empty() => SeriesRole::parse(r)?,
        _ => existante
            .as_ref()
            .map(|s| s.role)
            .unwrap_or(SeriesRole::Primary),
    };

    let parsed = InfoclimatPayload {
        station_id: outcome.station_id.clone(),
        station: outcome.station.clone(),
        records: outcome.records,
        warnings: outcome.warnings,
        sample_interval_seconds: outcome.sample_interval_seconds,
        duplicate_count: outcome.duplicate_count,
        unreadable_complements: outcome.unreadable_complements,
        available_stations: vec![outcome.station_id.clone()],
        // L'API répond toujours en JSON : le CSV n'existe qu'en téléchargement.
        format: InfoclimatFormat::Json,
    };

    let window_count = outcome.windows.len();
    let created_series = existante.is_none();
    let station_name = parsed.station.as_ref().and_then(|s| s.name.clone());
    let data_license = parsed.station.as_ref().and_then(|s| s.license.clone());
    let duplicate_count = parsed.duplicate_count;
    let unreadable_complements = parsed.unreadable_complements;
    let sample_interval_seconds = parsed.sample_interval_seconds;

    let issue = persist_infoclimat(
        &state.db,
        &point,
        role,
        parsed,
        existante,
        None,
        None,
        Some(now_utc()),
        payload.force_duplicate,
    )?;

    let rapport = match issue {
        ImportOutcome::Imported { report } => *report,
        ImportOutcome::DuplicateSuspected { duplicate } => {
            return Ok(FetchOutcome::DuplicateSuspected { duplicate })
        }
    };

    Ok(FetchOutcome::Fetched {
        report: Box::new(FetchReport {
            series_id: rapport.series_id,
            point_id: rapport.point_id,
            point_name: rapport.point_name,
            station_id,
            station_name,
            window_count,
            requested_start: debut,
            requested_end: fin,
            created_series,
            inserted: rapport.imported_count,
            updated: rapport.updated_count,
            unchanged: rapport.unchanged_count,
            warning_count: rapport.warning_count,
            warnings: rapport.warnings,
            duplicate_count,
            unreadable_complements,
            sample_interval_seconds,
            period_start_utc: rapport.period_start_utc,
            period_end_utc: rapport.period_end_utc,
            data_license,
            attribution: infoclimat::ATTRIBUTION.to_string(),
            stats: rapport.stats,
        }),
    })
}

// ---------------------------------------------------------------------------
// Infoclimat — fabrique commune aux deux modes
// ---------------------------------------------------------------------------

/// Écrit un lot de mesures Infoclimat, en créant la série ou en la complétant.
///
/// C'est ici que se joue la règle du §1.4 pour cette source : `dh_utc` étant de
/// l'UTC vrai, `raw_timestamp == timestamp_utc`, `clock_offset_seconds = 0` et
/// `raw_timestamp_was_utc = true`. La détection d'ordre jour/mois est
/// court-circuitée par [`infoclimat_date_evidence`] : aucune série Infoclimat ne
/// peut être marquée « format de date à confirmer ».
#[allow(clippy::too_many_arguments)]
fn persist_infoclimat(
    db: &Db,
    point: &Point,
    role: SeriesRole,
    parsed: InfoclimatPayload,
    existante: Option<LoggerSeries>,
    file_name: Option<String>,
    label: Option<String>,
    fetched_at: Option<DateTime<Utc>>,
    force_duplicate: bool,
) -> AppResult<ImportOutcome> {
    let data_license = parsed.station.as_ref().and_then(|s| s.license.clone());
    let evidence = infoclimat_date_evidence(parsed.records.len());

    let series_id = existante
        .as_ref()
        .map(|s| s.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Aucune liste de `Measurement` n'est construite d'avance : chacun est
    // fabriqué au moment de son écriture (voir `import_romma_impl`).
    //
    // Pas d'`apply_clock_offset` : l'offset est nul par construction et le
    // brut EST l'UTC — c'est ce que dit `Measurement::new_utc`, qui écrit les
    // deux instants identiques. L'invariant du §1.4, vérifié au cas trivial.
    let sid: std::sync::Arc<str> = series_id.as_str().into();
    let mesures = || {
        parsed.records.iter().map(|r| Measurement {
            humidity_percent: r.humidity_percent,
            rssi_dbm: r.rssi_dbm,
            battery: r.battery,
            ..Measurement::new_utc(sid.clone(), r.timestamp_utc, r.temperature_celsius)
        })
    };

    // L'empreinte ne sert qu'à la **création** d'une série. Compléter une série
    // existante est justement le geste normal du mode « récupération
    // automatique » : redemander sept jours dont cinq sont connus n'est pas un
    // ré-import, et la contrainte d'unicité de la v3 suffit à écarter le
    // recouvrement mesure par mesure.
    let empreinte = series_fingerprint_of(&parsed.records);
    let cree_une_serie = existante.is_none();

    if cree_une_serie && !force_duplicate {
        if let Some(duplicate) =
            detecter_reimport(db, point, &empreinte, parsed.records.len())?
        {
            return Ok(ImportOutcome::DuplicateSuspected {
                duplicate: Box::new(duplicate),
            });
        }
    }

    let (inserted, updated, unchanged) = match existante {
        Some(_) => {
            let bilan = db.append_measurements(&series_id, &mesures().collect::<Vec<_>>())?;
            (bilan.inserted, bilan.updated, bilan.unchanged)
        }
        None => {
            let series = LoggerSeries {
                id: series_id.clone(),
                point_id: point.id.clone(),
                source_type: SourceType::InfoclimatApi,
                role,
                logger_serial: None,
                imported_at: now_utc(),
                raw_file_reference: file_name,
                clock_offset_seconds: 0,
                raw_timestamp_was_utc: true,
                label,
                sample_interval_seconds: parsed.sample_interval_seconds,
                data_license: data_license.clone(),
                station_id: Some(parsed.station_id.clone()),
                last_api_fetch_at: fetched_at,
                date_order: evidence.order,
                date_format_uncertain: false,
                content_fingerprint: Some(empreinte.clone()),
                measurement_count: 0,
            };
            let ecrites = db.insert_series_with_measurements_iter(&series, mesures())?;
            (ecrites, 0, parsed.records.len() - ecrites)
        }
    };

    if let Some(at) = fetched_at {
        db.mark_series_fetched(&series_id, at)?;
    }

    // Les statistiques portent sur **toute** la série relue en base, pas sur le
    // seul lot qui vient d'arriver : après un rafraîchissement de dix mesures,
    // afficher « minimum extrême » sur ces dix-là n'aurait aucun sens.
    let stats = period_stats_from_days(&db.series_days(&series_id)?);
    let warning_count = parsed.warnings.len();

    Ok(ImportOutcome::imported(ImportReport {
        series_id,
        point_id: point.id.clone(),
        point_name: point.name.clone(),
        source_type: SourceType::InfoclimatApi,
        role,
        imported_count: inserted,
        updated_count: updated,
        unchanged_count: unchanged,
        skipped_count: warning_count,
        duplicate_count: parsed.duplicate_count,
        warning_count,
        warnings: parsed
            .warnings
            .into_iter()
            .take(MAX_WARNINGS_RETURNED)
            .collect(),
        period_start_utc: stats.first_utc,
        period_end_utc: stats.last_utc,
        clock_offset_seconds: 0,
        raw_timestamp_was_utc: true,
        logger_serial: None,
        sample_interval_seconds: parsed.sample_interval_seconds,
        layout: None,
        date_order: evidence,
        date_format_uncertain: false,
        // Une série complétée garde l'empreinte de son import d'origine : la
        // renvoyer ici laisserait croire qu'elle décrit le lot qui vient
        // d'arriver.
        content_fingerprint: cree_une_serie.then_some(empreinte),
        forced_duplicate: cree_une_serie && force_duplicate,
        data_license,
        attribution: Some(infoclimat::ATTRIBUTION.to_string()),
        // Une station fixe ne se met pas en poche : la détection de saut, pensée
        // pour un logger qu'on retire de son abri, n'aurait ici aucun sens et ne
        // produirait que du bruit sur les vraies ruptures d'inversion.
        anomalies: Vec::new(),
        stats,
    }))
}


// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Identité de l'utilisateur
// ---------------------------------------------------------------------------
//
// Un seul réglage, dans la table clé/valeur `app_settings` : aucune migration de
// schéma n'est nécessaire (la table est arrivée en v3). Il sert à deux endroits —
// les scripts de l'écran d'accueil et le system prompt de l'agent.

pub const SETTING_USER_NAME: &str = "user_name";

/// Longueur maximale d'un prénom. Généreuse, mais bornée : cette valeur part
/// dans le system prompt de chaque appel, où le budget est de 8 000 tokens/minute.
const MAX_USER_NAME_LEN: usize = 60;

/// Prénom enregistré, ou `None` à la toute première ouverture.
#[tauri::command]
pub fn get_user_name(state: State<'_, AppState>) -> AppResult<Option<String>> {
    Ok(state
        .db
        .get_setting(SETTING_USER_NAME)?
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty()))
}

#[tauri::command]
pub fn set_user_name(state: State<'_, AppState>, name: String) -> AppResult<String> {
    let propre = name.trim();
    if propre.is_empty() {
        return Err(AppError::Other(
            "Indiquez un prénom : il sert à vous adresser la parole sur l'écran \
             d'accueil et dans les analyses."
                .into(),
        ));
    }
    if propre.chars().count() > MAX_USER_NAME_LEN {
        return Err(AppError::Other(format!(
            "Prénom trop long ({} caractères, {MAX_USER_NAME_LEN} au maximum).",
            propre.chars().count()
        )));
    }
    state.db.set_setting(SETTING_USER_NAME, propre)?;
    Ok(propre.to_string())
}

#[tauri::command]
pub fn clear_all_data(state: State<'_, AppState>) -> AppResult<()> {
    state.db.clear_all_data()
}

// ---------------------------------------------------------------------------
// Agent IA (Groq)
// ---------------------------------------------------------------------------

/// Ce que l'écran Paramètres et le bloc « Agent IA » ont besoin de savoir.
#[derive(Debug, Clone, Serialize)]
pub struct AgentSettings {
    pub model: String,
    /// Statut de la clé — **jamais la clé elle-même**.
    pub key: KeyStatus,
    pub key_file: String,
    /// Page où générer une clé. Servie par le backend plutôt qu'écrite dans le
    /// JS : elle est déjà déclarée une fois dans `groq::KEY_SOURCE`, et c'est
    /// la même que citent les messages d'erreur de `api_keys`. Deux copies
    /// finiraient par diverger le jour où Groq déplace sa console.
    pub key_signup_url: String,
    /// Plafond du palier gratuit, affiché tel quel : il explique les refus.
    pub tokens_per_minute: u32,
    pub user_name: Option<String>,
}

// `async` : hors du thread principal, pour ne pas figer l'interface
// quand la base est occupée (synchro Cloud, import).
#[tauri::command(async)]
pub fn agent_settings(state: State<'_, AppState>) -> AppResult<AgentSettings> {
    Ok(AgentSettings {
        model: groq::MODEL.to_string(),
        key: groq::key_status(),
        key_file: format!("{}/{}", api_keys::KEY_DIR, groq::KEY_FILE),
        key_signup_url: groq::KEY_SOURCE.signup_url.to_string(),
        tokens_per_minute: groq::TOKENS_PAR_MINUTE,
        user_name: get_user_name(state)?,
    })
}

/// La clé Groq saisie à la main, enregistrée là où l'agent la relira.
///
/// Servie par **les deux** écrans qui proposent de renseigner la clé — celui de
/// première ouverture et l'onglet Paramètres. Le doublon n'existe qu'à l'écran :
/// il n'y a ici qu'un seul chemin d'écriture, et il rend le `AgentSettings`
/// rafraîchi, relu depuis le fichier — l'appelant affiche donc ce qui est
/// réellement sur le disque, pas ce qu'il vient d'envoyer.
///
/// Aucun appel à Groq n'est fait : la validité d'une clé ne se prouve qu'en
/// s'en servant, et l'écran de première ouverture n'a pas à attendre le réseau.
/// Une clé fausse échouera au premier usage de l'agent, avec le message de
/// `groq.rs` — comportement inchangé.
#[tauri::command]
pub fn set_groq_key(state: State<'_, AppState>, key: String) -> AppResult<AgentSettings> {
    let chemin = groq::save_key(&key).map_err(|e| AppError::Other(e.to_string()))?;

    // La variable d'environnement l'emporte sur le fichier dans `load_key`. Le
    // dire ici évite le pire des scénarios : « clé enregistrée », puis une
    // empreinte qui n'est pas celle qu'on vient de coller, sans explication.
    if groq::env_var_prend_le_dessus() {
        return Err(AppError::Other(format!(
            "Clé écrite dans {}, mais la variable d'environnement {} est définie sur ce              poste et reste prioritaire. Supprimez-la pour que cette clé serve.",
            chemin.display(),
            groq::KEY_ENV_VAR
        )));
    }

    agent_settings(state)
}

// ---------------------------------------------------------------------------
// Liens vers l'extérieur
// ---------------------------------------------------------------------------

/// Les URL que le frontend a le droit de faire ouvrir. Liste **fermée**.
///
/// Le greffon `opener` n'est pas exposé au frontend (cf. `capabilities/`) : il
/// n'y a donc pas d'`open` générique atteignable depuis le JS, seulement cette
/// commande, et seulement vers ces adresses. Une page qui parviendrait à
/// exécuter du script dans la webview ne peut pas s'en servir pour lancer un
/// exécutable ni ouvrir un `file://`.
const URL_AUTORISEES: &[&str] = &[
    // Génération d'une clé Groq — le lien de l'écran de première ouverture.
    groq::KEY_SOURCE.signup_url,
    // Génération d'une clé Infoclimat, citée dans le même esprit côté Paramètres.
    infoclimat_client::KEY_SOURCE.signup_url,
];

/// Ouvre une URL dans le navigateur **du système**, pas dans la webview.
///
/// Sans cette commande, un `<a href>` remplacerait l'application par la page
/// Groq dans sa propre fenêtre — sans barre d'adresse, sans onglets, sans
/// gestionnaire de mots de passe, et sans moyen évident de revenir.
#[tauri::command]
pub fn open_external_url(url: String) -> AppResult<()> {
    if !URL_AUTORISEES.contains(&url.as_str()) {
        return Err(AppError::Other(format!(
            "Adresse non autorisée : {url}"
        )));
    }
    tauri_plugin_opener::open_url(&url, None::<&str>)
        .map_err(|e| AppError::Other(format!("Le navigateur n'a pas pu être ouvert : {e}")))
}

/// Ce que le frontend demande à l'agent.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentRequest {
    pub project_id: String,
    /// Points sélectionnés sur la carte. Peut être vide (question libre sans
    /// sélection) — l'agent le sait alors explicitement.
    #[serde(default)]
    pub point_ids: Vec<String>,
    /// `compare` | `analyze` | `project` | `biases` | `question`.
    pub kind: String,
    /// Renseigné pour `kind = "question"` uniquement.
    #[serde(default)]
    pub question: Option<String>,
}

/// Nombre maximal de points injectés lors d'une analyse sur une sélection de points.
///
/// Le plafond n'est pas esthétique : à 8 000 tokens/minute, une comparaison détaillée
/// ou analyse de biais sur trop de points ferait saturer le quota.
const MAX_POINTS_SELECTION: usize = 8;

/// Nombre maximal de points injectés lors d'un résumé global de projet.
///
/// Les descriptions synthétiques par point étant compactes (~200 caractères / ~50 tokens),
/// un projet de 30 à 50 stations tient parfaitement dans le budget de contexte.
const MAX_POINTS_PROJET: usize = 50;

/// Au-delà de ce nombre de points, « Résumer l'état du projet » n'envoie plus
/// une description par point mais un résumé agrégé (nombre de points,
/// répartitions) : sinon un projet comme France (1 153 points) est refusé
/// d'emblée par le plafond de tokens.
const SEUIL_RESUME_AGREGE: usize = 15;

/// Demande du résumé agrégé : le modèle n'a pas le détail des points.
const DEMANDE_RESUME_AGREGE: &str = "Résume l'état de ce projet à partir de ces agrégats : \
     couverture des mesures, part des points sans donnée, ancienneté des dernières mesures \
     et ce qu'il manque pour une comparaison exploitable. Tu n'as pas le détail des points : \
     ne cite aucun site par son nom.";

/// Traduit le bouton cliqué en demande explicite pour le modèle.
///
/// Écrit ici et non côté JS : c'est de la formulation de prompt, donc du même
/// registre que `groq::SYSTEM_PROMPT` — le frontend n'envoie qu'un identifiant.
fn demande_pour(kind: &str, question: Option<&str>, nb_points: usize) -> AppResult<String> {
    let texte = match kind {
        "compare" => {
            if nb_points < 2 {
                return Err(AppError::Other(
                    "La comparaison demande au moins deux points sélectionnés.".into(),
                ));
            }
            "Compare ces points. Attribue les écarts observés à ce qui les explique \
             réellement (catégorie, taille, facteurs de site, longueur et recouvrement \
             des périodes) et signale les biais possibles avant toute conclusion sur la \
             puissance relative des sites."
                .to_string()
        }
        "analyze" => {
            if nb_points != 1 {
                return Err(AppError::Other(
                    "L'analyse de série porte sur un seul point sélectionné.".into(),
                ));
            }
            "Analyse cette série : ce que les statistiques disent du site au regard de sa \
             catégorie et de sa taille, ce qui est cohérent, ce qui ne l'est pas, et ce \
             qui manque pour conclure."
                .to_string()
        }
        "project" => "Résume l'état de ce projet : couverture des mesures, points sans \
             donnée, périodes hétérogènes, notes qualitatives des sites et ce qu'il manque \
             pour une comparaison exploitable."
            .to_string(),
        "biases" => "Liste les biais qui pèsent concrètement sur ces données, du plus au \
             moins probable au vu des métadonnées et des périodes fournies. Pour chacun, \
             dis ce qui permettrait de l'écarter."
            .to_string(),
        "question" => {
            let q = question
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .ok_or_else(|| AppError::Other("La question est vide.".into()))?;
            if q.chars().count() > 2000 {
                return Err(AppError::Other(
                    "Question trop longue (2 000 caractères au maximum).".into(),
                ));
            }
            q.to_string()
        }
        autre => {
            return Err(AppError::Other(format!(
                "Type d'analyse inconnu : « {autre} »."
            )))
        }
    };
    Ok(texte)
}

/// Rassemble métadonnées et statistiques des points concernés.
///
/// Les statistiques portent sur **toutes** les mesures du point, séries
/// confondues, lues dans les agrégats journaliers (`series_days`) : les mêmes
/// chiffres que `stats::period_stats` sur les mesures brutes, sans les relire.
fn contexte_des_points(
    db: &Db,
    point_ids: &[String],
    max_points: usize,
) -> AppResult<(Vec<groq::PointContext>, usize)> {
    let retenus = point_ids.len().min(max_points);
    let ignores = point_ids.len() - retenus;

    let mut contextes = Vec::with_capacity(retenus);
    for id in point_ids.iter().take(retenus) {
        let point = db.get_point(id)?;
        let jours = db.point_days(id, None, None)?;
        let series = db.list_series_for_point(id)?;
        contextes.push(groq::PointContext {
            point,
            stats: period_stats_from_days(&jours),
            series_count: series.len(),
            manual_entry_series: series
                .iter()
                .filter(|s| s.source_type.is_manual_entry())
                .count(),
        });
    }
    Ok((contextes, ignores))
}

/// Lance une analyse. `spawn_blocking` : le client Groq est bloquant, comme
/// celui d'Infoclimat, et un appel de quelques secondes figerait la fenêtre.
#[tauri::command]
pub async fn agent_analyze(
    state: State<'_, AppState>,
    payload: AgentRequest,
) -> AppResult<groq::AgentReply> {
    let statut = groq::key_status();
    if !statut.configured {
        return Err(AppError::Other(format!(
            "Agent IA indisponible. {}",
            statut.problem.unwrap_or_default()
        )));
    }

    let is_project = payload.kind == "project";
    // « Résumer l'état du projet » porte sur tout le projet, pas sur la sélection.
    let ids: Vec<String> = if is_project {
        state
            .db
            .list_points(&payload.project_id)?
            .into_iter()
            .map(|p| p.id)
            .collect()
    } else {
        payload.point_ids.clone()
    };

    if is_project && ids.len() > SEUIL_RESUME_AGREGE {
        let points = state.db.list_points(&payload.project_id)?;
        let coverage = state.db.project_coverage(&payload.project_id)?;
        let maintenant = chrono::Utc::now();
        let vue = groq::ProjectOverview {
            points: &points,
            coverage: &coverage,
            current_month: chrono::Datelike::year(&maintenant) as i64 * 100
                + chrono::Datelike::month(&maintenant) as i64,
        };
        let user_name = get_user_name(state)?;
        let system = groq::system_prompt(user_name.as_deref());
        let user = groq::ContextBuilder::build_overview(&vue, DEMANDE_RESUME_AGREGE);
        return lancer_agent(system, user).await;
    }

    let max_points = if is_project {
        MAX_POINTS_PROJET
    } else {
        MAX_POINTS_SELECTION
    };

    let (contextes, ignores) = contexte_des_points(&state.db, &ids, max_points)?;
    let mut demande = demande_pour(&payload.kind, payload.question.as_deref(), contextes.len())?;

    if ignores > 0 {
        // Tronquer en silence donnerait une analyse fausse d'apparence complète.
        let designation = if is_project {
            "sites du projet"
        } else {
            "points sélectionnés"
        };
        demande.push_str(&format!(
            "\n\n(Seuls les {max_points} premiers {designation} sont \
             joints ; {ignores} autre(s) ont été écartés faute de budget de contexte. \
             Mentionne-le.)"
        ));
    }

    let user_name = get_user_name(state)?;
    let system = groq::system_prompt(user_name.as_deref());
    let user = groq::ContextBuilder::build(&contextes, &demande);

    lancer_agent(system, user).await
}

/// Appel Groq hors du fil de l'interface : le client est bloquant.
async fn lancer_agent(system: String, user: String) -> AppResult<groq::AgentReply> {
    tauri::async_runtime::spawn_blocking(move || {
        let client = groq::GroqClient::from_local_key()?;
        client.complete(&system, &user)
    })
    .await
    .map_err(|e| AppError::Other(format!("L'analyse n'a pas pu aboutir : {e}")))?
    .map_err(Into::into)
}

// ---------------------------------------------------------------------------
// Cache de tuiles hors-ligne
// ---------------------------------------------------------------------------

/// Sert une tuile : cache disque d'abord, réseau ensuite.
///
/// C'est le **seul** point d'entrée du frontend vers une tuile — il n'y a plus
/// d'URL IGN appelée directement en JS. Le tableau d'octets traverse l'IPC tel
/// quel ; le frontend en fait un blob pour l'`<img>` de la tuile.
///
/// `async` + `spawn_blocking`, volontairement : une commande Tauri *synchrone*
/// s'exécute sur le pool (restreint) de threads workers de l'exécuteur async,
/// pas sur le pool de threads bloquants. Au dézoom, Leaflet demande d'un coup
/// des dizaines de tuiles absentes du cache ; chacune fait un appel réseau
/// bloquant (`reqwest::blocking`). Sans ce `spawn_blocking`, ces appels
/// saturent les quelques threads workers et se sérialisent, ce qui produisait
/// le gel perceptible de l'UI — même avec un timeout réseau court par tuile.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_tile(
    app: tauri::AppHandle,
    source: String,
    z: u32,
    x: u32,
    y: u32,
) -> AppResult<tauri::ipc::Response> {
    let source = TileSource::parse(&source)?;
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        tiles::get_tile_cached(&state.tiles_cache_dir, &state.tile_fetcher, source, z, x, y)
    })
    .await
    .map_err(|e| AppError::Other(format!("Chargement de tuile interrompu : {e}")))??;
    // Réponse binaire brute : un `Vec<u8>` partirait en tableau JSON de
    // nombres (~4 octets de texte par octet), à reparser côté webview.
    Ok(tauri::ipc::Response::new(bytes))
}

/// Progression envoyée au frontend pendant un pré-téléchargement de zone.
#[derive(Debug, Clone, Serialize)]
pub struct TileDownloadProgress {
    pub downloaded: usize,
    pub total: usize,
}

/// Nom de l'événement Tauri émis à chaque tuile traitée par [`download_tile_area`].
pub const TILE_DOWNLOAD_EVENT: &str = "tile-download-progress";

/// Télécharge toutes les tuiles d'une zone (bounding box de la vue carte) sur
/// une plage de zoom bornée, pour un usage hors-ligne sur le terrain.
///
/// Tourne dans un `spawn_blocking` : c'est potentiellement plusieurs centaines
/// de requêtes réseau et d'écritures disque, qui ne doivent pas geler l'UI.
/// La progression est émise tuile par tuile ; le frontend en tire une barre
/// simple (compte téléchargé / total).
#[tauri::command(rename_all = "snake_case")]
pub async fn download_tile_area(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    source: String,
    min_zoom: u32,
    max_zoom: u32,
    north: f64,
    south: f64,
    east: f64,
    west: f64,
) -> AppResult<tiles::DownloadSummary> {
    use tauri::Emitter;

    let source = TileSource::parse(&source)?;
    let bounds = TileBounds {
        north,
        south,
        east,
        west,
    };
    let plan = tiles::plan_download(source, bounds, min_zoom, max_zoom)?;

    let cache_dir = state.tiles_cache_dir.clone();
    let resume = tauri::async_runtime::spawn_blocking(move || {
        let fetcher = HttpTileFetcher::new();
        tiles::download_plan(&cache_dir, &fetcher, source, &plan, |downloaded, total| {
            let _ = app.emit(
                TILE_DOWNLOAD_EVENT,
                TileDownloadProgress { downloaded, total },
            );
        })
    })
    .await
    .map_err(|e| AppError::Other(format!("Le téléchargement de la zone a échoué : {e}")))?;

    Ok(resume)
}

/// Taille actuelle du cache sur disque, par fond — écran Paramètres.
#[derive(Debug, Clone, Serialize)]
pub struct TilesCacheInfo {
    pub photo: tiles::SourceCacheInfo,
    pub scan25: tiles::SourceCacheInfo,
    pub total_bytes: u64,
}

// `async` : hors du thread principal, pour ne pas figer l'interface
// quand la base est occupée (synchro Cloud, import).
#[tauri::command(async)]
pub fn tiles_cache_info(state: State<'_, AppState>) -> AppResult<TilesCacheInfo> {
    let photo = tiles::source_cache_info(&state.tiles_cache_dir, TileSource::Photo);
    let scan25 = tiles::source_cache_info(&state.tiles_cache_dir, TileSource::Scan25);
    Ok(TilesCacheInfo {
        photo,
        scan25,
        total_bytes: photo.bytes + scan25.bytes,
    })
}

/// Vide entièrement le cache de tuiles (les deux fonds).
#[tauri::command]
pub fn clear_tiles_cache(state: State<'_, AppState>) -> AppResult<()> {
    tiles::clear_cache(&state.tiles_cache_dir)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Export (page 2 du bloc Stations)
// ---------------------------------------------------------------------------

/// Contenu généré, prêt à écrire tel quel — et le nom de fichier suggéré pour
/// la boîte de dialogue d'enregistrement.
#[derive(Debug, Clone, Serialize)]
pub struct ExportContent {
    pub content: String,
    pub suggested_name: String,
}

/// Construit l'export de la lecture actuellement en mémoire.
///
/// ## Pourquoi il ne relit pas le logger
///
/// L'export porte sur **ce que la page affiche**, et rien d'autre. Relire
/// l'appareil pour l'exporter rouvrirait le port, redemanderait l'autorisation
/// USB sur Android, prendrait une dizaine de secondes — et pourrait rendre un
/// contenu différent de la courbe sous les yeux de l'utilisateur si le logger
/// enregistre encore. Le `reading_id` est donc exigé : sans lui, l'export est
/// refusé plutôt que fabriqué à partir d'autre chose.
///
/// ## Pourquoi il ne passe pas par la base
///
/// Parce que rien n'y est encore. Exporter avant d'enregistrer est le cas
/// courant sur le terrain : on veut le fichier, on décide plus tard s'il devient
/// une série. C'est aussi ce qui rend cet export utile sur mobile, où
/// l'enregistrement demande de choisir un point dans un projet ouvert.
#[tauri::command(rename_all = "snake_case")]
pub fn build_rc5_export(
    state: State<'_, AppState>,
    reading_id: String,
    format: String,
) -> AppResult<ExportContent> {
    let caches = state.rc5_reading.lock().unwrap();
    let cache = caches.values().find(|c| c.reading_id == reading_id);
    let Some(cache) = cache else {
        return Err(AppError::Other(
            "La lecture à exporter n'est plus en mémoire. Relisez le logger, puis \
             réessayez l'export."
                .to_string(),
        ));
    };

    match format.as_str() {
        "csv" => Ok(ExportContent {
            content: export::build_rc5_csv(&cache.reading, &cache.port_name),
            suggested_name: export::suggested_rc5_filename(&cache.reading, "csv"),
        }),
        "txt" => Ok(ExportContent {
            content: export::build_rc5_txt(&cache.reading, &cache.port_name),
            suggested_name: export::suggested_rc5_filename(&cache.reading, "txt"),
        }),
        // Pas de HTML ici : le HTML de `build_export` est une mise en page de
        // **statistiques comparées** entre points, ce dont une lecture de logger
        // non encore rattachée à un point n'a pas la matière.
        autre => Err(AppError::Other(format!(
            "Format d'export inconnu pour un logger : « {autre} ». Attendu « csv » ou « txt »."
        ))),
    }
}

/// Construit le contenu d'un export (CSV, TXT ou HTML) pour une sélection de
/// points et une fenêtre temporelle.
///
/// Ne relit ni ne recalcule ce que `cross_analysis` a déjà produit pour le bloc
/// Croisement de données : `crossing::build` est appelé une seule fois ici, et
/// tous les résumés (TXT, HTML) en sortent directement. Seul le détail ligne à
/// ligne (CSV, ou TXT en mode détail) relit les mesures brutes, sur la fenêtre
/// déjà résolue.
#[tauri::command(rename_all = "snake_case")]
pub fn build_export(
    state: State<'_, AppState>,
    point_ids: Vec<String>,
    window: TimeWindow,
    format: String,
    detail: bool,
) -> AppResult<ExportContent> {
    if point_ids.is_empty() {
        return Err(AppError::Other(
            "Sélectionnez au moins un point à exporter.".to_string(),
        ));
    }

    let analyse = crossing::build(&state.db, &point_ids, window)?;

    let besoin_du_detail = format == "csv" || (format == "txt" && detail);
    let lignes = if besoin_du_detail {
        export::collect_rows(&state.db, &point_ids, analyse.window.as_ref())?
    } else {
        Vec::new()
    };

    let (content, ext) = match format.as_str() {
        "csv" => (export::build_csv(&lignes), "csv"),
        "txt" if detail => (export::build_txt_detail(&lignes), "txt"),
        "txt" => (export::build_txt_summary(&analyse), "txt"),
        "html" => (export::build_html_summary(&analyse), "html"),
        autre => {
            return Err(AppError::Other(format!(
                "Format d'export inconnu : « {autre} »."
            )))
        }
    };

    let suggested_name = export::suggested_filename(&analyse, window, ext);

    Ok(ExportContent {
        content,
        suggested_name,
    })
}

/// Ouvre la boîte de dialogue native d'enregistrement puis écrit le contenu.
///
/// `async` + `spawn_blocking` pour la même raison qu'ailleurs dans ce fichier
/// (voir `get_tile`) : la boîte de dialogue bloque le thread qui l'appelle
/// jusqu'à ce que l'utilisateur choisisse un emplacement ou annule, ce qui ne
/// doit pas geler l'UI ni saturer les threads workers de l'exécuteur async.
///
/// `Ok(None)` signifie que l'utilisateur a annulé — ce n'est pas une erreur.
#[tauri::command(rename_all = "snake_case")]
pub async fn save_export_file(
    app: tauri::AppHandle,
    content: String,
    suggested_name: String,
) -> AppResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    use tauri_plugin_fs::FsExt;

    let app_pour_dialogue = app.clone();
    let chemin = tauri::async_runtime::spawn_blocking(move || {
        app_pour_dialogue
            .dialog()
            .file()
            .set_file_name(&suggested_name)
            .blocking_save_file()
    })
    .await
    .map_err(|e| AppError::Other(format!("Enregistrement interrompu : {e}")))?;

    let Some(chemin) = chemin else {
        return Ok(None);
    };

    // Ne **pas** passer par `into_path()` + `std::fs::write`.
    //
    // Sur desktop, le sélecteur rend un chemin de fichier et les deux marchent.
    // Sur Android, il rend une URI `content://` du Storage Access Framework :
    // `into_path()` la refuse (« URL is not a valid path ») et l'export échoue —
    // en laissant derrière lui le fichier **vide** que le sélecteur venait de
    // créer, ce qui est le pire des deux mondes. C'était le cas avant ce
    // prompt, et cela valait pour tous les exports, pas seulement le RC-5.
    //
    // `Fs::open` traite les deux formes : un chemin ordinaire d'un côté, et de
    // l'autre un descripteur de fichier obtenu du `ContentResolver` par le
    // greffon `fs`. Il rend un `std::fs::File` dans les deux cas — le reste du
    // code n'a donc pas à savoir sur quelle plateforme il tourne.
    let libelle = chemin.to_string();
    let mut options = tauri_plugin_fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    let ecriture = tauri::async_runtime::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write;
        let mut fichier = app.fs().open(chemin, options)?;
        fichier.write_all(content.as_bytes())?;
        fichier.flush()
    })
    .await
    .map_err(|e| AppError::Other(format!("Enregistrement interrompu : {e}")))?;

    ecriture.map_err(|e| {
        AppError::Other(format!(
            "L'export n'a pas pu être écrit dans « {libelle} » : {e}"
        ))
    })?;

    Ok(Some(libelle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elitech_import::parse_elitech_export;

    /// Piste mémoire n°2 : l'empreinte calculée directement sur les
    /// `RawMeasurement` est identique, octet pour octet, à l'ancienne voie qui
    /// passait par un `Vec<Measurement>` temporaire — quel que soit le décalage
    /// d'horloge, avec ou sans humidité, et sur un lot désordonné.
    #[test]
    fn empreinte_sur_mesures_brutes_identique_a_l_ancienne_voie() {
        use crate::fingerprint::series_fingerprint;
        let t0 = chrono::NaiveDate::from_ymd_opt(2026, 1, 5)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let raws: Vec<RawMeasurement> = (0..500)
            .map(|i| RawMeasurement {
                // Ordre volontairement non monotone : les bornes min/max comptent.
                raw_timestamp: t0 + chrono::Duration::minutes(((i * 37) % 500) as i64 * 10),
                temperature_celsius: -5.0 + (i as f64) * 0.013,
                humidity_percent: if i % 3 == 0 { None } else { Some(40.0 + i as f64 * 0.1) },
                source_line: i + 2,
            })
            .collect();
        for offset in [0_i64, -3600, 7200, 12345] {
            let ancienne = series_fingerprint(&to_measurements("", &raws, offset));
            assert_eq!(series_fingerprint_of(&raws), ancienne, "décalage {offset}");
        }
        assert_eq!(
            series_fingerprint_of::<RawMeasurement>(&[]),
            series_fingerprint(&[])
        );
    }

    /// La liste des URL ouvrables est fermée, et elle est bien celle que le
    /// reste du code cite déjà — pas une copie écrite à la main qui divergerait
    /// des messages d'erreur de `api_keys`.
    #[test]
    fn seules_les_url_declarees_sont_ouvrables() {
        assert!(URL_AUTORISEES.contains(&"https://console.groq.com/keys"));
        assert!(URL_AUTORISEES.contains(&groq::KEY_SOURCE.signup_url));

        for interdite in [
            "https://console.groq.com/keys/../autre",
            "http://console.groq.com/keys",
            "file:///C:/Windows/System32/cmd.exe",
            "javascript:alert(1)",
            "https://exemple.invalide/",
            "",
        ] {
            assert!(
                !URL_AUTORISEES.contains(&interdite),
                "{interdite} ne doit pas être ouvrable"
            );
        }
    }

    /// L'écran de première ouverture affiche l'adresse et l'ouvre : les deux
    /// doivent désigner la même page, sinon le lien mène ailleurs que le texte.
    #[test]
    fn l_adresse_servie_au_frontend_est_celle_qui_s_ouvre() {
        assert_eq!(
            groq::KEY_SOURCE.signup_url,
            "https://console.groq.com/keys",
            "page des clés d'API de Groq — vérifiée avant d'être écrite en dur"
        );
        assert!(URL_AUTORISEES.contains(&groq::KEY_SOURCE.signup_url));
    }

    #[test]
    fn options_de_categorie_refletent_la_regle_metier() {
        let options = category_options();
        assert_eq!(options.len(), 6);

        let cat1 = &options[0];
        assert_eq!(cat1.value, "Cat1");
        assert_eq!(cat1.allowed_sizes, vec![None]);
        assert!(!cat1.requires_size);

        let cat2 = &options[1];
        assert_eq!(cat2.allowed_sizes, vec![None, Some("Gr".to_string())]);
        assert!(!cat2.requires_size);

        for cat in [&options[2], &options[3]] {
            assert_eq!(
                cat.allowed_sizes,
                vec![Some("Gr".to_string()), Some("Pt".to_string())]
            );
            assert!(cat.requires_size);
        }

        for cat in [&options[4], &options[5]] {
            assert_eq!(cat.allowed_sizes, vec![None]);
            assert!(!cat.requires_size);
        }
    }

    #[test]
    fn conversion_brut_vers_utc_respecte_linvariant() {
        let raws = parse_elitech_export(
            "Time\tTemperature(°C)\n2024-01-05 07:00:00\t-12.5\n2024-01-05 07:10:00\t-13.1\n",
        )
        .unwrap();

        // Logger réglé en UTC+1 : offset = -3600.
        let mesures = to_measurements("s1", &raws, -3600);
        assert_eq!(mesures.len(), 2);
        assert_eq!(
            mesures[0].raw_timestamp,
            "2024-01-05T07:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            mesures[0].timestamp_utc,
            "2024-01-05T06:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        for m in &mesures {
            assert_eq!(
                m.timestamp_utc.timestamp() - m.raw_timestamp.timestamp(),
                -3600,
                "timestamp_utc doit toujours valoir raw_timestamp + offset"
            );
        }

        // Offset nul : brut et corrigé coïncident.
        let mesures = to_measurements("s1", &raws, 0);
        assert_eq!(mesures[0].timestamp_utc, mesures[0].raw_timestamp);
    }

    #[test]
    fn bornes_brutes() {
        let raws = parse_elitech_export(
            "Time\tTemp\n2024-01-05 07:10:00\t-13.1\n2024-01-05 07:00:00\t-12.5\n",
        )
        .unwrap();
        let (start, end) = raw_bounds(&raws);
        assert_eq!(start.unwrap().to_string(), "2024-01-05 07:00:00");
        assert_eq!(end.unwrap().to_string(), "2024-01-05 07:10:00");
        assert_eq!(raw_bounds(&[]), (None, None));
    }

    // -----------------------------------------------------------------------
    // Détection de ré-import
    // -----------------------------------------------------------------------
    //
    // Trois situations, et une seule doit alerter :
    //
    //   1. le même fichier rejoué à l'identique          → alerte, rien d'écrit
    //   2. une période qui recouvre partiellement        → import normal, muet
    //   3. l'utilisateur insiste après l'alerte          → seconde série créée
    //
    // La deuxième est la plus importante des trois : c'est le fonctionnement
    // *normal* du mode « récupération automatique » d'Infoclimat, et une alerte
    // à cet endroit rendrait la fonction inutilisable.

    fn base_avec_point() -> (Db, Point) {
        let db = Db::open_in_memory().unwrap();
        let projet = db.create_project("Vercors").unwrap();
        let point = Point::from_new(NewPoint {
            project_id: projet.id,
            name: "Darbounouse".into(),
            latitude: 44.9812,
            longitude: 5.4213,
            altitude_m: Some(1150.0),
            category: "Cat3".into(),
            size: Some("Gr".into()),
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
        (db, point)
    }

    /// Température **fonction de l'instant**, jamais du rang dans le lot.
    ///
    /// Sans cela, deux lots qui se recouvrent porteraient des valeurs
    /// différentes au même instant : le recouvrement se lirait comme une
    /// correction de la source, et le test ne prouverait plus rien du cas réel,
    /// où la station republie exactement la même mesure.
    fn temperature_a(minutes: u32) -> f64 {
        -8.0 - ((minutes % 1440) as f64) / 100.0
    }

    /// Minutes écoulées depuis le 1er janvier 2026, 00:00.
    fn minutes_absolues(jour: u32, pas: usize) -> u32 {
        (jour - 1) * 1440 + (pas as u32) * 10
    }

    /// Export Elitech synthétique : `n` lignes au pas de 10 minutes.
    fn export_elitech(depart_jour: u32, n: usize) -> String {
        let mut s = String::from("Time\tTemperature(°C)\n");
        for i in 0..n {
            let minutes = minutes_absolues(depart_jour, i);
            s.push_str(&format!(
                "2026-01-{:02} {:02}:{:02}:00\t{:.2}\n",
                minutes / 1440 + 1,
                (minutes / 60) % 24,
                minutes % 60,
                temperature_a(minutes)
            ));
        }
        s
    }

    fn charge_elitech(contenu: &str, point_id: &str, force: bool) -> ElitechImportPayload {
        ElitechImportPayload {
            common: ImportCommon {
                point_id: point_id.to_string(),
                content: contenu.to_string(),
                file_name: Some("RC5-export.txt".into()),
                role: "Primary".into(),
                clock_offset_seconds: -3600,
                raw_timestamp_was_utc: false,
                logger_serial: None,
                label: None,
                force_duplicate: force,
            },
        }
    }

    #[test]
    fn reimport_a_l_identique_alerte_et_n_ecrit_rien() {
        let (db, point) = base_avec_point();
        let fichier = export_elitech(5, 144);

        let premier = import_elitech_impl(&db, &charge_elitech(&fichier, &point.id, false)).unwrap();
        let rapport = premier.report().expect("le premier import doit passer");
        assert_eq!(rapport.imported_count, 144);
        assert!(rapport.content_fingerprint.is_some());
        assert!(!rapport.forced_duplicate);

        // Deuxième passage, fichier strictement identique.
        let second = import_elitech_impl(&db, &charge_elitech(&fichier, &point.id, false)).unwrap();
        let doublon = second
            .duplicate()
            .expect("un fichier déjà importé doit déclencher l'alerte");

        assert_eq!(doublon.series_id, rapport.series_id);
        assert_eq!(doublon.point_name, "Darbounouse");
        assert_eq!(doublon.measurement_count, 144);
        assert_eq!(doublon.incoming_count, 144);
        assert!(
            doublon.message.contains("déjà avoir été importé le"),
            "le message doit nommer la date de l'import précédent : {}",
            doublon.message
        );

        // Et surtout : rien n'a été écrit.
        assert_eq!(
            db.list_series_for_point(&point.id).unwrap().len(),
            1,
            "l'alerte doit précéder toute écriture, pas la rattraper"
        );
        assert_eq!(db.measurements_for_point(&point.id).unwrap().len(), 144);
    }

    #[test]
    fn l_import_force_cree_bien_la_seconde_serie() {
        let (db, point) = base_avec_point();
        let fichier = export_elitech(5, 60);

        import_elitech_impl(&db, &charge_elitech(&fichier, &point.id, false)).unwrap();
        assert!(import_elitech_impl(&db, &charge_elitech(&fichier, &point.id, false))
            .unwrap()
            .duplicate()
            .is_some());

        // L'utilisateur insiste (deux loggers physiques distincts, par exemple).
        let force = import_elitech_impl(&db, &charge_elitech(&fichier, &point.id, true)).unwrap();
        let rapport = force.report().expect("forcer doit aboutir à un import");
        assert!(rapport.forced_duplicate);
        assert_eq!(rapport.imported_count, 60);

        let series = db.list_series_for_point(&point.id).unwrap();
        assert_eq!(series.len(), 2, "la seconde série doit exister");
        assert_eq!(
            series[0].content_fingerprint, series[1].content_fingerprint,
            "les deux séries portent la même empreinte, assumée"
        );
        assert_eq!(db.measurements_for_point(&point.id).unwrap().len(), 120);
    }

    #[test]
    fn un_fichier_prolonge_n_est_pas_un_doublon() {
        // Le logger a continué d'enregistrer : le nouvel export contient tout
        // l'ancien, plus la suite. C'est un complément, pas une répétition.
        let (db, point) = base_avec_point();

        import_elitech_impl(&db, &charge_elitech(&export_elitech(5, 100), &point.id, false))
            .unwrap()
            .report()
            .expect("premier import");

        let suite = import_elitech_impl(
            &db,
            &charge_elitech(&export_elitech(5, 160), &point.id, false),
        )
        .unwrap();
        assert!(
            suite.duplicate().is_none(),
            "un export prolongé ne doit pas être pris pour un ré-import"
        );
        assert_eq!(suite.report().unwrap().imported_count, 160);
    }

    // --- Voie Infoclimat ---------------------------------------------------

    /// Réponse Infoclimat synthétique couvrant `n` pas de 10 min depuis `jour`.
    fn json_infoclimat(jour: u32, n: usize) -> String {
        let mut lignes = Vec::with_capacity(n);
        for i in 0..n {
            let minutes = minutes_absolues(jour, i);
            lignes.push(format!(
                r#"{{"dh_utc":"2026-01-{:02} {:02}:{:02}:00","temperature":"{:.2}"}}"#,
                minutes / 1440 + 1,
                (minutes / 60) % 24,
                minutes % 60,
                temperature_a(minutes)
            ));
        }
        format!(
            r#"{{"status":"OK","errors":[],"stations":[],"hourly":{{"STATIC0479":[{}]}}}}"#,
            lignes.join(",")
        )
    }

    fn charge_infoclimat(
        contenu: &str,
        point_id: &str,
        append: bool,
        force: bool,
    ) -> InfoclimatImportPayload {
        InfoclimatImportPayload {
            point_id: point_id.to_string(),
            content: contenu.to_string(),
            file_name: Some("infoclimat.json".into()),
            role: "Primary".into(),
            station_id: Some("STATIC0479".into()),
            label: None,
            append_to_existing: append,
            force_duplicate: force,
        }
    }

    /// **Le test qui compte** : le complément incrémental ne doit rien déclencher.
    #[test]
    fn un_recouvrement_partiel_infoclimat_ne_declenche_pas_l_alerte() {
        let (db, point) = base_avec_point();

        // 7 jours au pas de 10 min.
        let sept_jours = json_infoclimat(5, 7 * 144);
        premier_import_infoclimat(&db, &point, &sept_jours);

        // On redemande 7 jours à partir du 10 janvier : deux jours se recouvrent
        // avec le lot précédent, cinq sont neufs. C'est exactement ce que fait le
        // mode « récupération automatique » relancé quelques jours plus tard.
        let chevauchant = json_infoclimat(10, 7 * 144);

        // a) en créant une nouvelle série — aucune alerte, l'empreinte diffère.
        let neuve = import_infoclimat_impl(
            &db,
            charge_infoclimat(&chevauchant, &point.id, false, false),
        )
        .unwrap();
        assert!(
            neuve.duplicate().is_none(),
            "un recouvrement partiel n'est pas un ré-import"
        );

        // b) en complétant la série existante — le chemin normal. Aucune alerte
        //    non plus : compléter ne crée aucune série, donc aucun doublon.
        //    Base neuve : après a), « la série la plus récente » (à la seconde
        //    près) pouvait être celle de a), qui contient déjà tout — d'où un
        //    test instable selon la charge de la machine.
        let (db, point) = base_avec_point();
        premier_import_infoclimat(&db, &point, &sept_jours);
        let complement = import_infoclimat_impl(
            &db,
            charge_infoclimat(&chevauchant, &point.id, true, false),
        )
        .unwrap();
        let rapport = complement.report().expect("le complément doit passer");
        assert!(
            rapport.unchanged_count > 0,
            "les instants déjà connus doivent être reconnus, pas réécrits"
        );
        assert!(
            rapport.imported_count > 0,
            "les deux jours neufs doivent bien entrer"
        );
    }

    #[test]
    fn la_meme_periode_infoclimat_redemandee_a_l_identique_alerte() {
        let (db, point) = base_avec_point();
        let lot = json_infoclimat(5, 300);

        import_infoclimat_impl(&db, charge_infoclimat(&lot, &point.id, false, false))
            .unwrap()
            .report()
            .expect("premier import");

        // Même période, mêmes valeurs, dans une *nouvelle* série : c'est le cas
        // qu'on veut voir barré.
        let rejoue =
            import_infoclimat_impl(&db, charge_infoclimat(&lot, &point.id, false, false)).unwrap();
        assert!(rejoue.duplicate().is_some());
        assert_eq!(db.list_series_for_point(&point.id).unwrap().len(), 1);

        // Forcer reste possible.
        let force =
            import_infoclimat_impl(&db, charge_infoclimat(&lot, &point.id, false, true)).unwrap();
        assert!(force.report().unwrap().forced_duplicate);
        assert_eq!(db.list_series_for_point(&point.id).unwrap().len(), 2);
    }

    /// Premier import d'un lot Infoclimat, sans assertion : sert de mise en place.
    fn premier_import_infoclimat(db: &Db, point: &Point, json: &str) {
        import_infoclimat_impl(db, charge_infoclimat(json, &point.id, false, false))
            .unwrap()
            .report()
            .expect("mise en place");
    }

    #[test]
    fn une_serie_anterieure_a_la_v4_ne_bloque_aucun_import() {
        // Empreinte `NULL` en base : ces séries ne participent pas à la
        // détection. Un import qui recouvre leur contenu doit passer sans alerte
        // plutôt que d'échouer sur une comparaison avec du vide.
        let (db, point) = base_avec_point();
        let fichier = export_elitech(5, 40);

        let premier = import_elitech_impl(&db, &charge_elitech(&fichier, &point.id, false)).unwrap();
        let series_id = premier.report().unwrap().series_id.clone();

        // On simule l'existant : une série migrée depuis la v3, sans empreinte.
        db.conn_pour_test()
            .execute(
                "UPDATE logger_series SET content_fingerprint = NULL WHERE id = ?1",
                [&series_id],
            )
            .unwrap();

        let second = import_elitech_impl(&db, &charge_elitech(&fichier, &point.id, false)).unwrap();
        assert!(
            second.duplicate().is_none(),
            "une série sans empreinte ne peut ni accuser ni être accusée"
        );
    }

    // -- lecture directe du RC-5 -------------------------------------------
    //
    // Ces trois tests portent sur le **chemin d'import réel** de la voie série,
    // pas seulement sur son décodage : ils rejouent la capture du logger réel et
    // la font entrer en base par `import_rc5_impl`, exactement comme le fait la
    // commande Tauri.

    fn echantillon_brut(chemin: &str) -> Vec<u8> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/samples")
            .join(chemin);
        std::fs::read(&p).unwrap_or_else(|e| panic!("fixture introuvable : {} ({e})", p.display()))
    }

    /// La lecture directe du logger réel, décodée depuis les trames capturées.
    fn lecture_reelle() -> Rc5Reading {
        elitech_serial::decode_reading(
            &echantillon_brut("rc5_direct/devinfo.bin"),
            &echantillon_brut("rc5_direct/header.bin"),
            &echantillon_brut("rc5_direct/pages.bin"),
            &JumpThresholds::default(),
        )
        .expect("la capture série réelle se décode")
    }

    fn commun_rc5(point_id: &str, force: bool) -> ImportCommon {
        ImportCommon {
            point_id: point_id.to_string(),
            content: String::new(),
            file_name: None,
            role: "Primary".to_string(),
            clock_offset_seconds: 0,
            raw_timestamp_was_utc: false,
            logger_serial: None,
            label: None,
            force_duplicate: force,
        }
    }

    #[test]
    fn un_import_direct_cree_une_serie_elitech_direct() {
        let (db, point) = base_avec_point();

        let outcome =
            import_rc5_impl(&db, &commun_rc5(&point.id, false), "COM8", lecture_reelle()).unwrap();
        let rapport = outcome.report().expect("import effectué");

        assert_eq!(rapport.imported_count, 10_307);
        assert_eq!(rapport.source_type, SourceType::ElitechDirect);
        // Le numéro transmis par l'appareil, tronqué — c'est bien celui-là qui
        // est enregistré quand l'utilisateur n'en saisit pas d'autre.
        assert_eq!(rapport.logger_serial.as_deref(), Some("E205004333"));
        assert_eq!(rapport.sample_interval_seconds, Some(900));

        // Sans fichier, la traçabilité tient au port et à l'appareil.
        let series = db.list_series_for_point(&point.id).unwrap();
        assert_eq!(series.len(), 1);
        let reference = series[0].raw_file_reference.as_deref().unwrap();
        assert!(reference.contains("COM8"), "référence : {reference}");
        assert!(reference.contains("E205004333"), "référence : {reference}");
    }

    #[test]
    fn le_numero_saisi_a_la_main_retablit_le_numero_complet() {
        // La raison d'être de la règle ici : le protocole tronque à 10
        // caractères, l'étiquette du logger en porte 12.
        let (db, point) = base_avec_point();
        let mut commun = commun_rc5(&point.id, false);
        commun.logger_serial = Some("EFE205004333".to_string());

        let outcome = import_rc5_impl(&db, &commun, "COM8", lecture_reelle()).unwrap();
        assert_eq!(
            outcome.report().unwrap().logger_serial.as_deref(),
            Some("EFE205004333")
        );
    }

    #[test]
    fn relire_en_direct_un_logger_deja_importe_par_fichier_est_vu_comme_un_reimport() {
        // **L'exigence centrale de la partie C.4.** Les deux voies passent par le
        // même `persist_import`, donc par la même empreinte : le contenant change,
        // le contenu non.
        let (db, point) = base_avec_point();

        let xlsx = ElitechXlsxImportPayload {
            common: ImportCommon {
                point_id: point.id.clone(),
                content: String::new(),
                file_name: Some("elitech_rc5_reel.xls".to_string()),
                role: "Primary".to_string(),
                clock_offset_seconds: 0,
                raw_timestamp_was_utc: false,
                logger_serial: None,
                label: None,
                force_duplicate: false,
            },
            bytes: echantillon_brut("elitech_rc5_reel.xls"),
        };
        let premier = import_elitech_xlsx_impl(&db, &xlsx).unwrap();
        assert_eq!(premier.report().unwrap().imported_count, 10_307);

        // Même logger, même mémoire, autre voie : rien ne doit être réécrit.
        let second =
            import_rc5_impl(&db, &commun_rc5(&point.id, false), "COM8", lecture_reelle()).unwrap();
        let doublon = second
            .duplicate()
            .expect("le même contenu par une autre voie reste le même contenu");
        assert_eq!(doublon.measurement_count, 10_307);
        assert_eq!(db.list_series_for_point(&point.id).unwrap().len(), 1, "rien d'écrit");

        // …et l'utilisateur garde le dernier mot.
        let force =
            import_rc5_impl(&db, &commun_rc5(&point.id, true), "COM8", lecture_reelle()).unwrap();
        assert!(force.report().is_some());
        assert_eq!(db.list_series_for_point(&point.id).unwrap().len(), 2);
    }

    // -----------------------------------------------------------------------
    // Saisie manuelle sur site
    // -----------------------------------------------------------------------

    fn charge_saisie(point_id: &str, jours: &[(&str, f64)], force: bool) -> ManualEntryPayload {
        ManualEntryPayload {
            common: ImportCommon {
                point_id: point_id.to_string(),
                content: String::new(),
                file_name: None,
                role: "Primary".into(),
                // Relevé lu sur une montre réglée en heure d'hiver française.
                clock_offset_seconds: -3600,
                raw_timestamp_was_utc: false,
                logger_serial: None,
                label: None,
                force_duplicate: force,
            },
            entry: ManualEntryInput {
                start_time: "06:00".into(),
                end_time: "09:00".into(),
                value_kind: "Minimum".into(),
                days: jours
                    .iter()
                    .map(|(d, t)| crate::manual_entry::ManualEntryDay {
                        date: (*d).to_string(),
                        temperature_celsius: Some(*t),
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn une_saisie_manuelle_cree_une_serie_marquee_comme_telle() {
        let (db, point) = base_avec_point();
        let charge = charge_saisie(
            &point.id,
            &[("2026-01-14", -4.2), ("2026-01-15", -7.8)],
            false,
        );

        let rapport = import_manual_entry_impl(&db, &charge)
            .unwrap()
            .report()
            .cloned()
            .expect("la saisie doit s'enregistrer");

        assert_eq!(rapport.imported_count, 2, "une mesure par jour, pas plus");
        assert_eq!(rapport.source_type, SourceType::ManualEntry);

        // Et la série relue depuis la base porte bien la même origine : c'est
        // elle qui sera lue par les statistiques et par l'export.
        let series = db.list_series_for_point(&point.id).unwrap();
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].source_type, SourceType::ManualEntry);
        assert!(series[0].source_type.is_manual_entry());
        assert_eq!(series[0].measurement_count, 2);
    }

    /// La contrainte n°1 du projet vaut aussi pour cette source : l'heure tapée
    /// est naïve, et c'est `clock_offset_seconds` qui la ramène à l'UTC.
    ///
    /// Une plage 6 h–9 h en heure d'hiver française a pour milieu 7 h 30 locales,
    /// soit **6 h 30 UTC**.
    #[test]
    fn l_instant_enregistre_est_de_l_utc_vrai() {
        let (db, point) = base_avec_point();
        let charge = charge_saisie(&point.id, &[("2026-01-14", -4.2)], false);
        import_manual_entry_impl(&db, &charge).unwrap();

        let mesures = db.measurements_for_point(&point.id).unwrap();
        assert_eq!(mesures.len(), 1);
        assert_eq!(
            mesures[0].timestamp_utc.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-01-14T06:30:00Z"
        );
        // Le brut est conservé tel qu'il a été saisi, pour un recalibrage
        // ultérieur — exactement comme pour un fichier de logger.
        assert_eq!(
            mesures[0].raw_timestamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-01-14T07:30:00Z"
        );
        assert_eq!(mesures[0].temperature_celsius, -4.2);
    }

    /// La saisie passe par le même pipeline que les fichiers : elle hérite donc
    /// de la détection de ré-import, sans une ligne écrite pour elle.
    #[test]
    fn saisir_deux_fois_la_meme_chose_est_detecte_comme_un_reimport() {
        let (db, point) = base_avec_point();
        let charge = charge_saisie(&point.id, &[("2026-01-14", -4.2)], false);

        import_manual_entry_impl(&db, &charge).unwrap();
        let second = import_manual_entry_impl(&db, &charge).unwrap();

        let doublon = second
            .duplicate()
            .expect("une saisie identique doit alerter, pas se dupliquer en silence");
        assert_eq!(doublon.source_type, SourceType::ManualEntry);
        assert_eq!(db.list_series_for_point(&point.id).unwrap().len(), 1);
    }

    /// Le libellé et la référence portent la plage horaire et la nature de la
    /// valeur : c'est la seule trace de ce qui a été saisi, la mesure n'étant
    /// qu'un instant et une température.
    #[test]
    fn la_serie_garde_la_plage_horaire_et_la_nature_de_la_valeur() {
        let (db, point) = base_avec_point();
        import_manual_entry_impl(&db, &charge_saisie(&point.id, &[("2026-01-14", -4.2)], false))
            .unwrap();

        let series = db.list_series_for_point(&point.id).unwrap();
        let libelle = series[0].label.clone().unwrap();
        assert!(libelle.contains("minimum"), "{libelle}");
        assert!(libelle.contains("06:00"), "{libelle}");
        assert!(libelle.contains("09:00"), "{libelle}");
        assert_eq!(series[0].raw_file_reference.as_deref(), Some(libelle.as_str()));
        assert_eq!(
            series[0].sample_interval_seconds,
            Some(3 * 3600),
            "l'étendue couverte par chaque valeur, pas une cadence"
        );
    }

    /// Aucun numéro de série : il n'y a pas d'appareil. En laisser passer un
    /// désignerait un logger qui n'a rien enregistré ce jour-là.
    #[test]
    fn aucun_numero_de_serie_n_est_attache_a_une_saisie() {
        let (db, point) = base_avec_point();
        let mut charge = charge_saisie(&point.id, &[("2026-01-14", -4.2)], false);
        charge.common.logger_serial = Some("EFE205004333".into());

        import_manual_entry_impl(&db, &charge).unwrap();
        let series = db.list_series_for_point(&point.id).unwrap();
        assert_eq!(series[0].logger_serial, None);
    }

    #[test]
    fn un_jour_sans_valeur_n_ecrit_rien_du_tout() {
        let (db, point) = base_avec_point();
        let mut charge = charge_saisie(
            &point.id,
            &[("2026-01-14", -4.2), ("2026-01-15", -7.8)],
            false,
        );
        charge.entry.days[1].temperature_celsius = None;

        assert!(import_manual_entry_impl(&db, &charge).is_err());
        assert!(
            db.list_series_for_point(&point.id).unwrap().is_empty(),
            "une saisie refusée ne doit laisser aucune trace"
        );
    }

    /// La distinction demandée du côté des statistiques : le croisement de
    /// données compte les séries saisies à la main, et l'export le dit.
    #[test]
    fn les_statistiques_et_l_export_signalent_la_saisie_manuelle() {
        let (db, point) = base_avec_point();

        // Un logger, puis une saisie manuelle, sur le même point.
        import_elitech_impl(&db, &charge_elitech(&export_elitech(14, 6), &point.id, false)).unwrap();
        import_manual_entry_impl(&db, &charge_saisie(&point.id, &[("2026-01-14", -4.2)], false))
            .unwrap();

        let analyse = crossing::build(&db, std::slice::from_ref(&point.id), TimeWindow::All).unwrap();
        assert_eq!(analyse.points[0].manual_entry_series, 1);

        let resume = export::build_txt_summary(&analyse);
        assert!(
            resume.contains("saisie manuelle"),
            "le résumé doit distinguer les deux origines :\n{resume}"
        );

        // Et le détail ligne à ligne porte une colonne « Origine » — elle
        // n'apparaît que parce qu'une saisie manuelle est présente.
        let lignes = export::collect_rows(&db, std::slice::from_ref(&point.id), analyse.window.as_ref()).unwrap();
        let csv = export::build_csv(&lignes);
        assert!(csv.contains("Origine"), "{}", &csv[..120.min(csv.len())]);
        assert!(csv.contains(SourceType::ManualEntry.label_fr()));
        assert!(csv.contains(SourceType::ElitechImport.label_fr()));
    }

    /// …et réciproquement : un export qui ne contient que des mesures d'appareil
    /// garde exactement les colonnes qu'il avait.
    #[test]
    fn sans_saisie_manuelle_l_export_ne_gagne_aucune_colonne() {
        let (db, point) = base_avec_point();
        import_elitech_impl(&db, &charge_elitech(&export_elitech(14, 6), &point.id, false)).unwrap();

        let analyse = crossing::build(&db, std::slice::from_ref(&point.id), TimeWindow::All).unwrap();
        assert_eq!(analyse.points[0].manual_entry_series, 0);

        let lignes = export::collect_rows(&db, std::slice::from_ref(&point.id), analyse.window.as_ref()).unwrap();
        let csv = export::build_csv(&lignes);
        assert!(!csv.contains("Origine"));
    }

    // -----------------------------------------------------------------------
    // Page RC-5 : la courbe envoyée à l'UI
    // -----------------------------------------------------------------------

    fn mesures_brutes(n: usize) -> Vec<RawMeasurement> {
        (0..n)
            .map(|i| RawMeasurement {
                raw_timestamp: NaiveDate::from_ymd_opt(2026, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    + Duration::minutes(15 * i as i64),
                temperature_celsius: i as f64,
                humidity_percent: None,
                source_line: i + 1,
            })
            .collect()
    }

    #[test]
    fn une_petite_lecture_est_tracee_mesure_par_mesure() {
        let courbe = courbe_rc5(&mesures_brutes(50), RC5_COURBE_MAX_POINTS);
        assert_eq!(courbe.len(), 50);
        assert!(courbe.iter().all(|p| p.count == 1));
        assert_eq!(courbe[0].average_celsius, 0.0);
        assert_eq!(courbe[49].max_celsius, 49.0);
    }

    /// Le sous-échantillonnage ne doit jamais gommer un extrême : sur ce projet,
    /// c'est le minimum qui est le sujet.
    #[test]
    fn le_sous_echantillonnage_conserve_les_extremes() {
        let mut mesures = mesures_brutes(16_000);
        mesures[7_777].temperature_celsius = -30.0;

        let courbe = courbe_rc5(&mesures, RC5_COURBE_MAX_POINTS);
        assert!(courbe.len() <= RC5_COURBE_MAX_POINTS);
        assert!(
            courbe.iter().any(|p| p.min_celsius == -30.0),
            "le minimum isolé doit survivre à l'agrégation"
        );
        // La courbe commence exactement à la première mesure lue.
        assert_eq!(courbe[0].raw_timestamp, mesures[0].raw_timestamp);
    }

    #[test]
    fn une_lecture_vide_ne_produit_aucune_courbe() {
        assert!(courbe_rc5(&[], RC5_COURBE_MAX_POINTS).is_empty());
    }

    // --- Import ROMMA : lecture en flux / chemin complet ----------------------

    /// `n` lignes au pas de 10 min, dans l'ordre ou non.
    fn archive_romma(n: usize, inverser: bool) -> String {
        let mut lignes: Vec<String> = (0..n)
            .map(|i| {
                let temp = if i % 7 == 3 {
                    "NULL".to_string()
                } else {
                    format!("{:.1}", -5.0 + (i % 50) as f64 * 0.3)
                };
                format!(
                    "01-01-2020,00:00,{},{temp},{temp},{temp},{},1012.0,0.00,2.0,SO,5.0,SO,NULL,{}",
                    1_496_250_000 + i as i64 * 600,
                    60 + i % 30,
                    i % 400
                )
            })
            .collect();
        if inverser {
            lignes.reverse();
        }
        format!(
            "Date,heure,timestamp,Temp,Tmin,Tmax,Hum,Pression,Pluie,Vmoy,VmoyDir,Rafale,RafaleDir,RadSolaire, RadSolaireMax\n\
             ,,,degreC,degreC,degreC,%,hPa,mm,km/h,,km/h,,W/m2, W/m2\n{}\n",
            lignes.join("\n")
        )
    }

    fn importer_romma(db: &Db, point: &Point, content: String) -> ImportReport {
        let outcome = import_romma_impl(
            db,
            RommaImportPayload {
                point_id: point.id.clone(),
                content,
                file_name: None,
                role: "Primary".into(),
                station_id: None,
                label: None,
                force_duplicate: false,
            },
        )
        .unwrap();
        match outcome {
            ImportOutcome::Imported { report } => *report,
            ImportOutcome::DuplicateSuspected { .. } => panic!("import attendu"),
        }
    }

    /// Le flux n'est qu'un raccourci : la même archive, triée (lue en flux) ou
    /// à l'envers (chemin complet, qui trie), doit donner **exactement** le même
    /// import — empreinte comprise, sans quoi la détection de ré-import ne
    /// reconnaîtrait plus une archive déjà importée par l'autre chemin.
    #[test]
    fn l_import_romma_en_flux_equivaut_au_chemin_complet() {
        let triee = archive_romma(300, false);
        let inversee = archive_romma(300, true);
        assert!(matches!(lire_romma(&triee).unwrap(), RommaLecture::EnFlux(_)));
        assert!(matches!(lire_romma(&inversee).unwrap(), RommaLecture::Complete(_)));

        let (db_flux, point_flux) = base_avec_point();
        let (db_complet, point_complet) = base_avec_point();
        let flux = importer_romma(&db_flux, &point_flux, triee.clone());
        let complet = importer_romma(&db_complet, &point_complet, inversee.clone());

        assert_eq!(flux.imported_count, 300 - 300 / 7 - 1);
        assert_eq!(flux.imported_count, complet.imported_count);
        assert_eq!(flux.skipped_count, complet.skipped_count);
        assert_eq!(flux.duplicate_count, complet.duplicate_count);
        assert_eq!(flux.sample_interval_seconds, complet.sample_interval_seconds);
        assert!(flux.content_fingerprint.is_some());
        assert_eq!(flux.content_fingerprint, complet.content_fingerprint);
        assert_eq!(flux.stats, complet.stats);

        let lignes = |db: &Db, id: &str| {
            db.measurements_for_series(id)
                .unwrap()
                .into_iter()
                .map(|m| {
                    (
                        m.timestamp_utc,
                        m.raw_timestamp,
                        m.temperature_celsius,
                        m.humidity_percent,
                        m.pressure_hpa,
                        m.wind_avg_kmh,
                        m.wind_gust_kmh,
                        m.wind_direction_degrees,
                        m.rain_mm,
                        m.solar_radiation,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            lignes(&db_flux, &flux.series_id),
            lignes(&db_complet, &complet.series_id)
        );

        // Et l'empreinte d'une voie reconnaît le ré-import par l'autre.
        let reimport = import_romma_impl(
            &db_flux,
            RommaImportPayload {
                point_id: point_flux.id.clone(),
                content: inversee,
                file_name: None,
                role: "Primary".into(),
                station_id: None,
                label: None,
                force_duplicate: false,
            },
        )
        .unwrap();
        assert!(matches!(reimport, ImportOutcome::DuplicateSuspected { .. }));
    }

    #[test]
    fn l_apercu_romma_en_flux_equivaut_au_chemin_complet() {
        let triee = archive_romma(120, false);
        let en_flux = serde_json::to_value(analyze_romma(triee.clone()).unwrap()).unwrap();
        let complet = serde_json::to_value(analyser_romma(
            &crate::romma_import::parse_romma_archives(&triee).unwrap(),
        ))
        .unwrap();
        assert_eq!(en_flux, complet);
    }

    #[test]
    fn contexte_des_points_respecte_les_plafonds_projet_et_selection() {
        let db = Db::open_in_memory().unwrap();
        let projet = db.create_project("Projet Test").unwrap();

        let mut point_ids = Vec::new();
        for i in 1..=25 {
            let point = Point::from_new(NewPoint {
                project_id: projet.id.clone(),
                name: format!("Station {i}"),
                latitude: 44.9 + (i as f64 * 0.01),
                longitude: 5.4 + (i as f64 * 0.01),
                altitude_m: Some(1200.0),
                category: "Cat3".into(),
                size: Some("Gr".into()),
                factor_sky: None,
                factor_wind: None,
                factor_slope: None,
                factor_snow_cover: None,
                factor_forest_cover: None,
                is_open_terrain: None,
                notes: Some(format!("Note pour station {i}")),
                infoclimat_station_id: None,
                romma_station_id: None,
            })
            .unwrap();
            db.insert_point(&point).unwrap();
            point_ids.push(point.id);
        }

        // Cas sélection : plafonné à MAX_POINTS_SELECTION (8)
        let (ctx_sel, ignores_sel) =
            contexte_des_points(&db, &point_ids, MAX_POINTS_SELECTION).unwrap();
        assert_eq!(ctx_sel.len(), 8);
        assert_eq!(ignores_sel, 17);

        // Cas projet : inclut tous les 25 sites (MAX_POINTS_PROJET = 50)
        let (ctx_proj, ignores_proj) =
            contexte_des_points(&db, &point_ids, MAX_POINTS_PROJET).unwrap();
        assert_eq!(ctx_proj.len(), 25);
        assert_eq!(ignores_proj, 0);

        // Les notes de stations sont bien conservées dans chaque contexte
        assert_eq!(
            ctx_proj[0].point.notes.as_deref(),
            Some("Note pour station 1")
        );
        assert_eq!(
            ctx_proj[24].point.notes.as_deref(),
            Some("Note pour station 25")
        );
    }
}



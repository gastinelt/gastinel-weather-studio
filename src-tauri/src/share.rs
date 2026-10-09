//! Partage de points (et de leurs séries) par un **code à 6 caractères**.
//!
//! ## Le principe
//!
//! L'émetteur choisit des points ; l'application les met au format
//! `gws-share/1` et les dépose sur le serveur `gws-share` (le VPS, derrière
//! `https://gastinel.me/gws/api/v1`). Le serveur rend un code — `XZ4K9P` — que
//! l'émetteur transmet comme il veut (SMS, oral, papier). Le destinataire tape le
//! code, voit un **aperçu** (combien de points, de quel projet, jusqu'à quand),
//! et seulement ensuite importe.
//!
//! Le contrat complet (routes, statuts, format) est dans `server/README.md` ;
//! ce module en est le client.
//!
//! ## Ce que ce module ne fait jamais
//!
//! * **Faire confiance au serveur.** Tout ce qui arrive est revalidé ici avec les
//!   mêmes bornes que le serveur ([`validate_document`]), puis une seconde fois
//!   par [`Point::from_parts`] et la contrainte `CHECK` de SQLite. Un serveur
//!   compromis, ou usurpé, ne doit pas pouvoir écrire autre chose qu'un point
//!   valide.
//! * **Reprendre l'empreinte de contenu reçue.** `content_fingerprint` sert à
//!   reconnaître un ré-import ; la recopier laisserait l'émetteur décider de ce
//!   qui passe pour un doublon chez le destinataire. Elle est recalculée à partir
//!   des mesures reçues ([`crate::fingerprint::series_fingerprint`]).
//! * **Mélanger un partage aux données de l'utilisateur sans le dire.** Par
//!   défaut, l'import crée un projet à part, nommé d'après le code : les points
//!   d'un tiers restent identifiables tant que l'utilisateur ne les a pas
//!   déplacés lui-même.
//! * **Remonter le jeton de révocation au frontend.** Il n'est rendu qu'une fois
//!   par le serveur, stocké ici dans `issued_shares`, et ne ressort de Rust que
//!   pour partir vers le serveur dans l'en-tête `X-Revoke-Token`.
//!
//! ## Ce qui est transmis
//!
//! Les points (nom, position, altitude, catégorie, taille, facteurs de site,
//! notes) et, en mode `points_series`, leurs séries réduites à **l'horodatage
//! brut et la température**. Brut, pas corrigé : le destinataire recalcule
//! `timestamp_utc = raw_timestamp + clock_offset_seconds` et garde donc la
//! possibilité de recalibrer, exactement comme l'émetteur.
//!
//! Ne partent pas : identifiants locaux, stations Infoclimat/ROMMA rattachées,
//! humidité, champs radio et météo complémentaires — la v1 du format ne les
//! porte pas.

use std::io::Read;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::{secs, Db};
use crate::error::{AppError, AppResult};
use crate::fingerprint::series_fingerprint;
use crate::model::{
    apply_clock_offset, now_utc, validate_category_size, Category, LoggerSeries, Measurement,
    NewPoint, Point, PointSize, SeriesRole, SourceType,
};
use crate::parsing::DateOrder;

/// Racine de l'API. En dur, et en HTTPS : c'est le seul serveur auquel ce
/// module parle, et aucune redirection n'est suivie (voir [`client`]).
pub const API_BASE: &str = "https://gastinel.me/gws/api/v1";

pub const FORMAT: &str = "gws-share/1";

/// Alphabet de Crockford, identique au serveur : ni I, ni L, ni O, ni U.
const ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const CODE_LEN: usize = 6;

pub const DEFAULT_TTL_DAYS: u32 = 90;
pub const MAX_TTL_DAYS: u32 = 90;

// Bornes du format — **copie de celles du serveur** (`server/gws_share.py`).
// Les changer d'un côté sans l'autre ferait refuser des partages valides.
pub const MAX_POINTS: usize = 500;
pub const MAX_SERIES: usize = 200;
pub const MAX_MEASUREMENTS: usize = 450_000;
const MAX_BYTES_POINTS: usize = 256 * 1024;
const MAX_BYTES_POINTS_SERIES: usize = 8 * 1024 * 1024;
const TS_MIN: i64 = 631_152_000; // 1990-01-01
const TS_MAX: i64 = 4_102_444_800; // 2100-01-01
const TEMP_MIN: f64 = -90.0;
const TEMP_MAX: f64 = 70.0;

/// Plafond de lecture d'une réponse : le plus gros partage possible, plus une
/// marge. Un serveur qui en renverrait davantage n'est plus le nôtre.
const MAX_RESPONSE_BYTES: u64 = (MAX_BYTES_POINTS_SERIES + 256 * 1024) as u64;

// ---------------------------------------------------------------------------
// Format gws-share/1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShareMode {
    #[serde(rename = "points")]
    Points,
    #[serde(rename = "points_series")]
    PointsSeries,
}

impl ShareMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ShareMode::Points => "points",
            ShareMode::PointsSeries => "points_series",
        }
    }

    fn max_bytes(self) -> usize {
        match self {
            ShareMode::Points => MAX_BYTES_POINTS,
            ShareMode::PointsSeries => MAX_BYTES_POINTS_SERIES,
        }
    }
}

/// Document `gws-share/1`. `deny_unknown_fields` à tous les niveaux, comme le
/// serveur : un champ inattendu est une erreur, pas une donnée ignorée.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareDocument {
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    pub mode: ShareMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_project: Option<String>,
    pub points: Vec<SharedPoint>,
    #[serde(default)]
    pub series: Vec<SharedSeries>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedPoint {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub altitude_m: Option<f64>,
    pub category: String,
    #[serde(default)]
    pub size: Option<String>,
    // Le serveur exige un booléen s'il est présent : on l'omet plutôt que
    // d'écrire `null` quand le point ne le renseigne pas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_open_terrain: Option<bool>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub factor_sky: Option<String>,
    #[serde(default)]
    pub factor_wind: Option<String>,
    #[serde(default)]
    pub factor_slope: Option<String>,
    #[serde(default)]
    pub factor_snow_cover: Option<String>,
    #[serde(default)]
    pub factor_forest_cover: Option<String>,
    /// UUID du point chez l'émetteur. **Informatif seulement** : il n'est jamais
    /// utilisé pour rapprocher un point reçu d'un point local — un émetteur
    /// pourrait sinon viser un point précis du destinataire.
    #[serde(default)]
    pub origin_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedSeries {
    pub point_index: usize,
    pub role: String,
    pub source_type: String,
    #[serde(default)]
    pub clock_offset_seconds: i64,
    #[serde(default)]
    pub raw_timestamp_was_utc: bool,
    #[serde(default)]
    pub date_order: Option<String>,
    #[serde(default)]
    pub date_format_uncertain: bool,
    /// Transmise pour information ; **recalculée** à l'import (voir l'en-tête).
    #[serde(default)]
    pub content_fingerprint: Option<String>,
    /// `[horodatage_brut, température]` — le brut, jamais l'instant corrigé.
    pub measurements: Vec<(i64, f64)>,
}

// ---------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------

/// Erreurs du partage, rédigées pour l'utilisateur final.
#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("Code invalide : six caractères attendus, lettres et chiffres (ex. XZ4-K9P).")]
    BadCode,

    #[error("Ce code ne correspond à aucun partage, ou le partage a expiré.")]
    NotFound,

    #[error("Trop de tentatives sur le serveur de partage. Réessayez dans {0}.")]
    TooManyRequests(String),

    #[error("Le serveur de partage est saturé ou plein pour l'instant. Réessayez plus tard.")]
    Unavailable,

    #[error("Partage trop volumineux : {size} pour une limite de {limit}. Choisissez une période de mesures plus courte, partagez moins de points, ou les points seuls.")]
    TooLarge { size: String, limit: String },

    #[error("Le serveur de partage a refusé la demande : {0}")]
    Rejected(String),

    #[error("Serveur de partage injoignable : {0}. Vérifiez la connexion Internet.")]
    Network(String),

    #[error("Réponse inattendue du serveur de partage : {0}")]
    BadResponse(String),

    /// Le document reçu ne respecte pas le format. Message précis (quel champ)
    /// pour qu'un bogue d'émetteur soit diagnosticable, sans rien importer.
    #[error("Partage refusé, contenu invalide : {0}")]
    Invalid(String),

    #[error("Rien à partager : sélectionnez au moins un point.")]
    Empty,

    #[error("Ce partage n'a pas été créé depuis ce poste : impossible de le révoquer d'ici.")]
    UnknownIssued,
}

impl From<ShareError> for AppError {
    fn from(e: ShareError) -> Self {
        AppError::Other(e.to_string())
    }
}

fn invalid(msg: impl Into<String>) -> ShareError {
    ShareError::Invalid(msg.into())
}

// ---------------------------------------------------------------------------
// Codes
// ---------------------------------------------------------------------------

/// Normalise un code saisi, **comme le serveur** : majuscules, tirets et espaces
/// ignorés, `I`/`L` → `1`, `O` → `0`. `xz4-k9p` et `XZ4K9P` sont le même code.
pub fn normalize_code(raw: &str) -> Result<String, ShareError> {
    let code: String = raw
        .chars()
        .filter(|c| !matches!(c, '-' | ' ' | '\u{a0}'))
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        })
        .collect();
    if code.chars().count() == CODE_LEN && code.chars().all(|c| ALPHABET.contains(c)) {
        Ok(code)
    } else {
        Err(ShareError::BadCode)
    }
}

/// `XZ4K9P` → `XZ4-K9P`, plus facile à lire et à dicter.
pub fn display_code(code: &str) -> String {
    if code.len() == CODE_LEN {
        format!("{}-{}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

// ---------------------------------------------------------------------------
// Validation (tout ce qui arrive d'ailleurs)
// ---------------------------------------------------------------------------

/// Caractères refusés dans tout texte reçu : demi-caractères UTF-16 isolés
/// (impossibles en `String` Rust, déjà filtrés par serde), contrôles C0/C1, et
/// marques de direction qui retournent l'affichage d'un nom (« Trojan Source »).
fn bad_char(c: char, multiline: bool) -> bool {
    let o = c as u32;
    (o < 0x20 && !(multiline && matches!(c, '\t' | '\n' | '\r')))
        || (0x7F..=0x9F).contains(&o)
        || (0x202A..=0x202E).contains(&o)
        || (0x2066..=0x2069).contains(&o)
        || matches!(o, 0x200E | 0x200F | 0x061C)
}

fn check_text(
    value: Option<&str>,
    max: usize,
    field: &str,
    required: bool,
    multiline: bool,
    markup: bool,
) -> Result<(), ShareError> {
    let Some(s) = value else {
        return if required {
            Err(invalid(format!("{field} : obligatoire")))
        } else {
            Ok(())
        };
    };
    let n = s.chars().count();
    if (required && n == 0) || n > max {
        return Err(invalid(format!("{field} : {} à {max} caractères", u8::from(required))));
    }
    if s.chars().any(|c| bad_char(c, multiline) || (markup && matches!(c, '<' | '>'))) {
        return Err(invalid(format!("{field} : caractère non autorisé")));
    }
    Ok(())
}

/// Revalide un document **reçu** avec les bornes du serveur, et davantage : la
/// règle catégorie/taille de [`validate_category_size`], les types de source et
/// rôles connus de ce binaire.
pub fn validate_document(doc: &ShareDocument) -> Result<(), ShareError> {
    if doc.format != FORMAT {
        return Err(invalid(format!("format « {} » non géré (attendu {FORMAT})", doc.format)));
    }
    check_text(doc.source_project.as_deref(), 200, "source_project", false, false, true)?;
    if doc.points.is_empty() || doc.points.len() > MAX_POINTS {
        return Err(invalid(format!("1 à {MAX_POINTS} points attendus")));
    }
    for (i, p) in doc.points.iter().enumerate() {
        let w = format!("points[{i}]");
        check_text(Some(&p.name), 120, &format!("{w}.name"), true, false, true)?;
        if !(p.latitude.is_finite() && (-90.0..=90.0).contains(&p.latitude)) {
            return Err(invalid(format!("{w}.latitude hors de [-90, 90]")));
        }
        if !(p.longitude.is_finite() && (-180.0..=180.0).contains(&p.longitude)) {
            return Err(invalid(format!("{w}.longitude hors de [-180, 180]")));
        }
        if let Some(a) = p.altitude_m {
            if !(a.is_finite() && (-500.0..=9000.0).contains(&a)) {
                return Err(invalid(format!("{w}.altitude_m hors de [-500, 9000]")));
            }
        }
        let category = Category::parse(&p.category).map_err(|e| invalid(format!("{w} : {e}")))?;
        let size = PointSize::parse_optional(p.size.as_deref()).map_err(|e| invalid(format!("{w} : {e}")))?;
        validate_category_size(category, size).map_err(|e| invalid(format!("{w} : {e}")))?;
        check_text(p.notes.as_deref(), 4000, &format!("{w}.notes"), false, true, false)?;
        for (k, v) in [
            ("factor_sky", &p.factor_sky),
            ("factor_wind", &p.factor_wind),
            ("factor_slope", &p.factor_slope),
            ("factor_snow_cover", &p.factor_snow_cover),
            ("factor_forest_cover", &p.factor_forest_cover),
        ] {
            check_text(v.as_deref(), 200, &format!("{w}.{k}"), false, false, false)?;
        }
        if let Some(o) = &p.origin_id {
            if o.is_empty() || o.len() > 64 || !o.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Err(invalid(format!("{w}.origin_id invalide")));
            }
        }
    }

    match doc.mode {
        ShareMode::Points if !doc.series.is_empty() => {
            return Err(invalid("le mode « points » ne transporte aucune série"));
        }
        ShareMode::PointsSeries if doc.series.is_empty() || doc.series.len() > MAX_SERIES => {
            return Err(invalid(format!("1 à {MAX_SERIES} séries attendues")));
        }
        _ => {}
    }
    let mut total = 0usize;
    for (j, s) in doc.series.iter().enumerate() {
        let w = format!("series[{j}]");
        if s.point_index >= doc.points.len() {
            return Err(invalid(format!("{w}.point_index hors des points transmis")));
        }
        SeriesRole::parse(&s.role).map_err(|e| invalid(format!("{w} : {e}")))?;
        SourceType::parse(&s.source_type).map_err(|e| invalid(format!("{w} : {e}")))?;
        if s.clock_offset_seconds.abs() > 2_592_000 {
            return Err(invalid(format!("{w}.clock_offset_seconds hors de ±30 jours")));
        }
        if let Some(o) = &s.date_order {
            DateOrder::parse(o).ok_or_else(|| invalid(format!("{w}.date_order inconnu")))?;
        }
        if s.measurements.is_empty() {
            return Err(invalid(format!("{w} : aucune mesure")));
        }
        total += s.measurements.len();
        if total > MAX_MEASUREMENTS {
            return Err(invalid(format!("plus de {MAX_MEASUREMENTS} mesures au total")));
        }
        for (k, (ts, t)) in s.measurements.iter().enumerate() {
            if !(TS_MIN..=TS_MAX).contains(ts) || !t.is_finite() || !(TEMP_MIN..=TEMP_MAX).contains(t) {
                return Err(invalid(format!(
                    "{w}.measurements[{k}] : horodatage 1990-2100 et température [-90, 70] °C attendus"
                )));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Construction d'un partage (côté émetteur)
// ---------------------------------------------------------------------------

/// Ce que le frontend demande à partager.
#[derive(Debug, Clone, Deserialize)]
pub struct ShareRequest {
    pub project_id: String,
    pub point_ids: Vec<String>,
    /// `false` : les points seuls. `true` : avec toutes leurs séries.
    #[serde(default)]
    pub include_series: bool,
    #[serde(default)]
    pub ttl_days: Option<u32>,
    /// Ne partager que les mesures des `n` derniers jours (instant UTC corrigé).
    /// `None` : toute la série. C'est le moyen de faire passer une archive trop
    /// lourde pour un seul partage — ROMMA, par exemple, dépasse seule la limite.
    #[serde(default)]
    pub period_days: Option<u32>,
}

/// Séparateur de milliers « à la française » pour les messages.
fn milliers(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// Assemble le document à partir de la base. Les points sont pris dans l'ordre
/// de la base (alphabétique), pas dans celui de la sélection : deux partages
/// des mêmes points produisent le même document.
pub fn build_document(db: &Db, req: &ShareRequest) -> AppResult<(ShareDocument, usize)> {
    if req.point_ids.is_empty() {
        return Err(ShareError::Empty.into());
    }
    if req.point_ids.len() > MAX_POINTS {
        return Err(ShareError::Rejected(format!("{MAX_POINTS} points au plus par partage")).into());
    }
    let project = db.get_project(&req.project_id)?;
    let points: Vec<Point> = db
        .list_points(&req.project_id)?
        .into_iter()
        .filter(|p| req.point_ids.contains(&p.id))
        .collect();
    if points.is_empty() {
        return Err(ShareError::Empty.into());
    }

    let mut series = Vec::new();
    let mut measurement_count = 0usize;
    // La série la plus lourde, pour pouvoir la nommer si le total dépasse.
    let mut plus_lourde: Option<(usize, String)> = None;
    let depuis = req
        .period_days
        .filter(|&j| j > 0)
        .map(|j| Utc::now() - chrono::Duration::days(i64::from(j)));
    if req.include_series {
        for (index, point) in points.iter().enumerate() {
            for s in db.list_series_for_point(&point.id)? {
                // Filtre de période en SQL, deux colonnes seulement : plus de
                // série entière relue en `Measurement` pour n'en garder qu'un bout.
                let mesures = db.raw_temperatures_since(&s.id, depuis.map(secs))?;
                if mesures.is_empty() {
                    continue;
                }
                measurement_count += mesures.len();
                if plus_lourde.as_ref().map_or(true, |(n, _)| mesures.len() > *n) {
                    plus_lourde = Some((
                        mesures.len(),
                        format!("« {} » ({})", point.name, s.source_type.label_fr()),
                    ));
                }
                series.push(SharedSeries {
                    point_index: index,
                    role: s.role.as_str().to_string(),
                    source_type: s.source_type.as_str().to_string(),
                    clock_offset_seconds: s.clock_offset_seconds,
                    raw_timestamp_was_utc: s.raw_timestamp_was_utc,
                    date_order: Some(s.date_order.as_str().to_string()),
                    date_format_uncertain: s.date_format_uncertain,
                    content_fingerprint: s.content_fingerprint.clone(),
                    measurements: mesures,
                });
            }
        }
    }
    // Trop de mesures : on le dit avec les chiffres et le nom de la série en
    // cause, et on propose les deux issues, plutôt que le message générique de
    // la validation (qui parlait d'un point à corriger, alors qu'aucun ne l'est).
    if measurement_count > MAX_MEASUREMENTS {
        let (n, nom) = plus_lourde.unwrap_or((0, String::new()));
        return Err(AppError::Other(format!(
            "Trop de mesures pour un seul partage : {} (limite {}). La série la plus lourde \
             est {nom}, avec {} mesures. Choisissez une période plus courte dans « Période des \
             mesures », décochez ce point, ou partagez les points seuls.",
            milliers(measurement_count),
            milliers(MAX_MEASUREMENTS),
            milliers(n),
        )));
    }
    // Des points sans aucune série en mode « avec séries » : le serveur
    // refuserait un mode `points_series` vide, on retombe sur `points`.
    let mode = if series.is_empty() { ShareMode::Points } else { ShareMode::PointsSeries };

    let doc = ShareDocument {
        format: FORMAT.to_string(),
        created_at: Some(Utc::now().timestamp()),
        mode,
        source_project: Some(project.name.chars().take(200).collect()),
        points: points
            .iter()
            .map(|p| SharedPoint {
                name: p.name.clone(),
                latitude: p.latitude,
                longitude: p.longitude,
                altitude_m: p.altitude_m,
                category: p.category.as_str().to_string(),
                size: p.size.map(|s| s.as_str().to_string()),
                is_open_terrain: p.is_open_terrain,
                notes: p.notes.clone(),
                factor_sky: p.factor_sky.clone(),
                factor_wind: p.factor_wind.clone(),
                factor_slope: p.factor_slope.clone(),
                factor_snow_cover: p.factor_snow_cover.clone(),
                factor_forest_cover: p.factor_forest_cover.clone(),
                origin_id: Some(p.id.clone()),
            })
            .collect(),
        series,
    };
    // Mêmes règles à l'envoi qu'à la réception : un point créé avant une règle
    // plus stricte (un `<` dans un nom, par exemple) est signalé ici, avec son
    // nom, plutôt que refusé par le serveur avec un simple indice.
    validate_document(&doc).map_err(|e| AppError::Other(format!(
        "Ce partage ne peut pas partir tel quel — {e}. Corrigez le point concerné puis réessayez."
    )))?;
    Ok((doc, measurement_count))
}

fn fmt_size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} Mo", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} Ko", bytes.div_ceil(1024))
    }
}

/// Sérialise un document et vérifie qu'il tient dans la limite de son mode.
pub fn serialize_document(doc: &ShareDocument) -> Result<Vec<u8>, ShareError> {
    let body = serde_json::to_vec(doc).map_err(|e| ShareError::BadResponse(e.to_string()))?;
    let limit = doc.mode.max_bytes();
    if body.len() > limit {
        return Err(ShareError::TooLarge { size: fmt_size(body.len()), limit: fmt_size(limit) });
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// Client du serveur de partage : HTTPS seulement, **aucune redirection suivie**.
/// Une redirection emporterait le document (ou le jeton de révocation) vers un
/// hôte que ce module n'a pas choisi.
fn client() -> Result<reqwest::blocking::Client, ShareError> {
    crate::http_client::client_builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .build()
        .map_err(|e| ShareError::Network(e.to_string()))
}

/// Lit le corps d'une réponse sans jamais dépasser [`MAX_RESPONSE_BYTES`].
fn read_body(resp: reqwest::blocking::Response) -> Result<Vec<u8>, ShareError> {
    let mut body = Vec::new();
    resp.take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|e| ShareError::Network(e.to_string()))?;
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(ShareError::BadResponse("réponse démesurée, lecture interrompue".into()));
    }
    Ok(body)
}

fn fmt_retry(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{} h", seconds.div_ceil(3600))
    } else {
        format!("{} min", seconds.div_ceil(60).max(1))
    }
}

/// Traduit un statut d'erreur du serveur en message utilisateur.
fn error_for(resp: reqwest::blocking::Response) -> ShareError {
    let status = resp.status().as_u16();
    let retry = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(600);
    let detail = read_body(resp)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| format!("statut HTTP {status}"));
    match status {
        404 => ShareError::NotFound,
        429 => ShareError::TooManyRequests(fmt_retry(retry)),
        503 | 507 => ShareError::Unavailable,
        413 => ShareError::TooLarge { size: "plus que la limite".into(), limit: fmt_size(MAX_BYTES_POINTS_SERIES) },
        400..=499 => ShareError::Rejected(detail),
        _ => ShareError::BadResponse(detail),
    }
}

fn network(e: reqwest::Error) -> ShareError {
    ShareError::Network(if e.is_timeout() { "délai dépassé".into() } else { e.to_string() })
}

/// Réponse du serveur à la création.
#[derive(Debug, Clone, Deserialize)]
struct CreatedResponse {
    code: String,
    expires_at: i64,
    revoke_token: String,
    point_count: usize,
    series_count: usize,
}

/// Dépose un document. Bloquant : à appeler depuis un `spawn_blocking`.
fn post_document(body: Vec<u8>, ttl_days: u32) -> Result<CreatedResponse, ShareError> {
    let resp = client()?
        .post(format!("{API_BASE}/shares?ttl_days={ttl_days}"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .map_err(network)?;
    if resp.status().as_u16() != 201 {
        return Err(error_for(resp));
    }
    let created: CreatedResponse = serde_json::from_slice(&read_body(resp)?)
        .map_err(|e| ShareError::BadResponse(e.to_string()))?;
    // Le serveur doit rendre un code bien formé et un jeton non vide : sinon ce
    // n'est pas une création exploitable, et on ne l'enregistre pas.
    normalize_code(&created.code).map_err(|_| ShareError::BadResponse("code mal formé".into()))?;
    if created.revoke_token.len() < 16 || created.revoke_token.len() > 128 {
        return Err(ShareError::BadResponse("jeton de révocation mal formé".into()));
    }
    Ok(created)
}

/// Aperçu d'un partage : ses métadonnées, jamais ses données.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharePreview {
    pub code: String,
    pub display_code: String,
    pub mode: ShareMode,
    pub point_count: usize,
    pub series_count: usize,
    pub source_project: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct PreviewResponse {
    code: String,
    mode: ShareMode,
    point_count: usize,
    series_count: usize,
    #[serde(default)]
    source_project: Option<String>,
    created_at: i64,
    expires_at: i64,
}

/// Interroge `/preview`. Bloquant.
pub fn fetch_preview(code: &str) -> Result<SharePreview, ShareError> {
    let code = normalize_code(code)?;
    let resp = client()?
        .get(format!("{API_BASE}/shares/{code}/preview"))
        .send()
        .map_err(network)?;
    if !resp.status().is_success() {
        return Err(error_for(resp));
    }
    let p: PreviewResponse = serde_json::from_slice(&read_body(resp)?)
        .map_err(|e| ShareError::BadResponse(e.to_string()))?;
    if p.code != code {
        return Err(ShareError::BadResponse("le serveur a répondu pour un autre code".into()));
    }
    // Affiché tel quel à l'écran : mêmes règles qu'un nom de projet reçu.
    check_text(p.source_project.as_deref(), 200, "source_project", false, false, true)?;
    let ts = |s: i64| DateTime::from_timestamp(s, 0).ok_or_else(|| ShareError::BadResponse("date invalide".into()));
    Ok(SharePreview {
        display_code: display_code(&code),
        code,
        mode: p.mode,
        point_count: p.point_count,
        series_count: p.series_count,
        source_project: p.source_project,
        created_at: ts(p.created_at)?,
        expires_at: ts(p.expires_at)?,
    })
}

/// Télécharge et **revalide** un partage complet. Bloquant.
pub fn fetch_document(code: &str) -> Result<ShareDocument, ShareError> {
    let code = normalize_code(code)?;
    let resp = client()?
        .get(format!("{API_BASE}/shares/{code}"))
        .send()
        .map_err(network)?;
    if !resp.status().is_success() {
        return Err(error_for(resp));
    }
    let doc: ShareDocument = serde_json::from_slice(&read_body(resp)?)
        .map_err(|e| invalid(format!("JSON non conforme ({e})")))?;
    validate_document(&doc)?;
    Ok(doc)
}

/// Révoque côté serveur. `Ok(())` aussi quand le partage n'existe plus (déjà
/// expiré ou révoqué) : dans les deux cas, il n'est plus lisible. Bloquant.
fn delete_share(code: &str, token: &str) -> Result<(), ShareError> {
    let resp = client()?
        .delete(format!("{API_BASE}/shares/{code}"))
        .header("X-Revoke-Token", token)
        .send()
        .map_err(network)?;
    match resp.status().as_u16() {
        204 | 404 => Ok(()),
        _ => Err(error_for(resp)),
    }
}

// ---------------------------------------------------------------------------
// Partages émis depuis ce poste (table `issued_shares`, migration v8)
// ---------------------------------------------------------------------------

/// Un partage créé depuis ce poste, tel que l'UI le voit — **sans le jeton**.
#[derive(Debug, Clone, Serialize)]
pub struct IssuedShare {
    pub code: String,
    pub display_code: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub mode: String,
    pub point_count: i64,
    pub series_count: i64,
    pub measurement_count: i64,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// `active` | `expired` | `revoked`.
    pub status: String,
}

/// Résultat d'une création, rendu à l'UI.
#[derive(Debug, Clone, Serialize)]
pub struct ShareCreated {
    pub code: String,
    pub display_code: String,
    pub expires_at: DateTime<Utc>,
    pub mode: String,
    pub point_count: usize,
    pub series_count: usize,
    pub measurement_count: usize,
}

/// Envoie le document puis enregistre le partage émis. Le jeton de révocation
/// est écrit en base **avant** que quoi que ce soit ne remonte à l'UI : si
/// l'écriture échouait, le partage serait aussitôt révoqué plutôt que laissé
/// irrévocable.
pub fn publish(db: &Db, req: &ShareRequest, doc: &ShareDocument, measurement_count: usize, body: Vec<u8>) -> AppResult<ShareCreated> {
    let ttl = req.ttl_days.unwrap_or(DEFAULT_TTL_DAYS).clamp(1, MAX_TTL_DAYS);
    let created = post_document(body, ttl)?;
    let now = secs(now_utc());
    let enregistre = db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO issued_shares (code, project_id, mode, point_count, series_count,
                 measurement_count, created_at, expires_at, revoke_token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                created.code,
                req.project_id,
                doc.mode.as_str(),
                created.point_count as i64,
                created.series_count as i64,
                measurement_count as i64,
                now,
                created.expires_at,
                created.revoke_token,
            ],
        )
    });
    if let Err(e) = enregistre {
        let _ = delete_share(&created.code, &created.revoke_token);
        return Err(AppError::Other(format!(
            "Le partage n'a pas pu être enregistré sur ce poste ({e}) ; il a été retiré du serveur."
        )));
    }
    Ok(ShareCreated {
        display_code: display_code(&created.code),
        code: created.code,
        expires_at: DateTime::from_timestamp(created.expires_at, 0).unwrap_or_else(Utc::now),
        mode: doc.mode.as_str().to_string(),
        point_count: created.point_count,
        series_count: created.series_count,
        measurement_count,
    })
}

pub fn list_issued(db: &Db) -> AppResult<Vec<IssuedShare>> {
    let now = secs(now_utc());
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT s.code, s.project_id, p.name, s.mode, s.point_count, s.series_count,
                    s.measurement_count, s.created_at, s.expires_at, s.revoked_at
               FROM issued_shares s LEFT JOIN projects p ON p.id = s.project_id
              ORDER BY s.created_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let code: String = r.get(0)?;
            let expires: i64 = r.get(8)?;
            let revoked: Option<i64> = r.get(9)?;
            let status = if revoked.is_some() {
                "revoked"
            } else if expires <= now {
                "expired"
            } else {
                "active"
            };
            Ok(IssuedShare {
                display_code: display_code(&code),
                code,
                project_id: r.get(1)?,
                project_name: r.get(2)?,
                mode: r.get(3)?,
                point_count: r.get(4)?,
                series_count: r.get(5)?,
                measurement_count: r.get(6)?,
                created_at: crate::db::ts_from_secs(r.get(7)?)?,
                expires_at: crate::db::ts_from_secs(expires)?,
                revoked_at: revoked.map(crate::db::ts_from_secs).transpose()?,
                status: status.to_string(),
            })
        })?;
        rows.collect()
    })
    .map_err(AppError::from)
}

/// Jeton de révocation d'un partage émis ici. Ne sort de ce module que vers
/// [`delete_share`].
fn revoke_token(db: &Db, code: &str) -> AppResult<String> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT revoke_token FROM issued_shares WHERE code = ?1",
            [code],
            |r| r.get::<_, String>(0),
        )
    })
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => ShareError::UnknownIssued.into(),
        other => AppError::from(other),
    })
}

/// Révoque un partage émis depuis ce poste. Réseau bloquant : `spawn_blocking`.
pub fn revoke(db: &Db, raw_code: &str) -> AppResult<()> {
    let code = normalize_code(raw_code)?;
    let token = revoke_token(db, &code)?;
    delete_share(&code, &token)?;
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE issued_shares SET revoked_at = ?2 WHERE code = ?1 AND revoked_at IS NULL",
            params![code, secs(now_utc())],
        )
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Import (côté destinataire)
// ---------------------------------------------------------------------------

/// Où ranger un partage reçu.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImportTarget {
    /// Un projet neuf, nommé d'après le code — le défaut, et le plus sûr.
    NewProject,
    /// Un projet existant, désigné explicitement par l'utilisateur.
    Existing { project_id: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ShareImportReport {
    pub project_id: String,
    pub project_name: String,
    pub points_created: usize,
    pub series_created: usize,
    pub measurements_written: usize,
}

/// Écrit un document **déjà validé** dans la base.
///
/// Chaque point passe par [`Point::from_parts`] (la porte unique des règles
/// métier) et reçoit un identifiant neuf : rien de ce qui vient de l'émetteur
/// ne désigne une ligne locale. En cas d'échec en cours de route sur un projet
/// neuf, le projet est supprimé — la cascade emporte ce qui avait été écrit.
pub fn import_document(db: &Db, code: &str, doc: &ShareDocument, target: &ImportTarget) -> AppResult<ShareImportReport> {
    validate_document(doc)?;
    let (project, created_here) = match target {
        ImportTarget::NewProject => {
            let origine = doc.source_project.as_deref().unwrap_or("").trim();
            let nom = if origine.is_empty() {
                format!("Partage {}", display_code(code))
            } else {
                format!("Partage {} — {origine}", display_code(code))
            };
            (db.create_project(&nom.chars().take(120).collect::<String>())?, true)
        }
        ImportTarget::Existing { project_id } => (db.get_project(project_id)?, false),
    };

    let resultat = write_points(db, code, doc, &project.id);
    match resultat {
        Ok((points_created, series_created, measurements_written)) => Ok(ShareImportReport {
            project_id: project.id,
            project_name: project.name,
            points_created,
            series_created,
            measurements_written,
        }),
        Err(e) => {
            if created_here {
                let _ = db.delete_project(&project.id);
            }
            Err(e)
        }
    }
}

fn write_points(db: &Db, code: &str, doc: &ShareDocument, project_id: &str) -> AppResult<(usize, usize, usize)> {
    let reference = format!("Partage GWS {}", display_code(code));
    let mut ids = Vec::with_capacity(doc.points.len());
    for p in &doc.points {
        let point = Point::from_new(NewPoint {
            project_id: project_id.to_string(),
            name: p.name.clone(),
            latitude: p.latitude,
            longitude: p.longitude,
            altitude_m: p.altitude_m,
            category: p.category.clone(),
            size: p.size.clone(),
            factor_sky: p.factor_sky.clone(),
            factor_wind: p.factor_wind.clone(),
            factor_slope: p.factor_slope.clone(),
            factor_snow_cover: p.factor_snow_cover.clone(),
            factor_forest_cover: p.factor_forest_cover.clone(),
            is_open_terrain: p.is_open_terrain,
            notes: p.notes.clone(),
            infoclimat_station_id: None,
            romma_station_id: None,
        })?;
        db.insert_point(&point)?;
        ids.push(point.id);
    }

    let mut series_created = 0usize;
    let mut written = 0usize;
    for s in &doc.series {
        let series_id = Uuid::new_v4().to_string();
        let offset = s.clock_offset_seconds;
        let mut mesures = Vec::with_capacity(s.measurements.len());
        let sid: std::sync::Arc<str> = series_id.as_str().into();
        for &(raw, t) in &s.measurements {
            let raw = DateTime::from_timestamp(raw, 0)
                .ok_or_else(|| invalid("horodatage hors de l'intervalle représentable"))?;
            mesures.push(Measurement::new(sid.clone(), apply_clock_offset(raw, offset), raw, t));
        }
        let series = LoggerSeries {
            id: series_id,
            point_id: ids[s.point_index].clone(),
            source_type: SourceType::parse(&s.source_type)?,
            role: SeriesRole::parse(&s.role)?,
            logger_serial: None,
            imported_at: now_utc(),
            raw_file_reference: Some(reference.clone()),
            clock_offset_seconds: offset,
            raw_timestamp_was_utc: s.raw_timestamp_was_utc,
            label: Some(reference.clone()),
            sample_interval_seconds: None,
            date_order: s.date_order.as_deref().and_then(DateOrder::parse).unwrap_or(DateOrder::DayFirst),
            date_format_uncertain: s.date_format_uncertain,
            data_license: None,
            station_id: None,
            last_api_fetch_at: None,
            // Recalculée : l'empreinte reçue n'est qu'une affirmation de l'émetteur.
            content_fingerprint: Some(series_fingerprint(&mesures)),
            measurement_count: 0,
        };
        written += db.insert_series_with_measurements(&series, &mesures)?;
        series_created += 1;
    }
    Ok((ids.len(), series_created, written))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(name: &str, category: &str, size: Option<&str>) -> SharedPoint {
        SharedPoint {
            name: name.into(),
            latitude: 44.97,
            longitude: 5.48,
            altitude_m: Some(1200.0),
            category: category.into(),
            size: size.map(Into::into),
            is_open_terrain: Some(true),
            notes: Some("fond de doline\nvent < 5 km/h".into()),
            factor_sky: None,
            factor_wind: None,
            factor_slope: None,
            factor_snow_cover: None,
            factor_forest_cover: None,
            origin_id: Some("0f8e7c1a-0000-4000-8000-000000000001".into()),
        }
    }

    fn doc_series() -> ShareDocument {
        ShareDocument {
            format: FORMAT.into(),
            created_at: Some(1_789_720_000),
            mode: ShareMode::PointsSeries,
            source_project: Some("TAF vercors".into()),
            points: vec![point("Darbounouse", "Cat3", Some("Gr")), point("Col de la Croix", "Col", None)],
            series: vec![SharedSeries {
                point_index: 0,
                role: "Primary".into(),
                source_type: "ElitechDirect".into(),
                clock_offset_seconds: -3600,
                raw_timestamp_was_utc: false,
                date_order: Some("DayFirst".into()),
                date_format_uncertain: false,
                content_fingerprint: Some("affirmation-de-l-emetteur".into()),
                measurements: vec![(1_757_000_000, -12.5), (1_757_000_600, -13.0), (1_757_001_200, -12.0)],
            }],
        }
    }

    #[test]
    fn normalisation_des_codes_comme_le_serveur() {
        assert_eq!(normalize_code("xz4-k9p").unwrap(), "XZ4K9P");
        assert_eq!(normalize_code(" il0 ooo ").unwrap(), "110000");
        assert!(normalize_code("ABCDE").is_err());
        assert!(normalize_code("ABCDEU").is_err(), "U n'est pas dans l'alphabet");
        assert!(normalize_code("ABCDEFG").is_err());
        assert_eq!(display_code("XZ4K9P"), "XZ4-K9P");
    }

    #[test]
    fn un_document_valide_passe_y_compris_col_et_versants() {
        let mut d = doc_series();
        d.points.push(point("Versant nord", "Versants", None));
        validate_document(&d).unwrap();
    }

    #[test]
    fn le_json_produit_est_celui_que_le_serveur_attend() {
        let json: serde_json::Value = serde_json::to_value(doc_series()).unwrap();
        assert_eq!(json["mode"], "points_series");
        assert_eq!(json["series"][0]["measurements"][0], serde_json::json!([1_757_000_000, -12.5]));
        let mut sans = point("P", "Cat1", None);
        sans.is_open_terrain = None;
        let p = serde_json::to_value(sans).unwrap();
        assert!(p.get("is_open_terrain").is_none(), "le serveur refuse null pour un booléen");
    }

    #[test]
    fn champs_inconnus_refuses_a_la_lecture() {
        let mut v = serde_json::to_value(doc_series()).unwrap();
        v["espion"] = serde_json::json!(1);
        assert!(serde_json::from_value::<ShareDocument>(v).is_err());
        let mut v = serde_json::to_value(doc_series()).unwrap();
        v["points"][0]["x"] = serde_json::json!(1);
        assert!(serde_json::from_value::<ShareDocument>(v).is_err());
    }

    #[test]
    fn documents_pieges_refuses() {
        let cas: Vec<(&str, Box<dyn Fn(&mut ShareDocument)>)> = vec![
            ("balise dans un nom", Box::new(|d| d.points[0].name = "<img src=x>".into())),
            ("inversion de direction", Box::new(|d| d.points[0].name = "abc\u{202e}cba".into())),
            ("controle dans les notes", Box::new(|d| d.points[0].notes = Some("a\u{0}b".into()))),
            ("Cat3 sans taille", Box::new(|d| d.points[0].size = None)),
            ("Col avec taille", Box::new(|d| d.points[1].size = Some("Gr".into()))),
            ("latitude", Box::new(|d| d.points[0].latitude = 91.0)),
            ("latitude NaN", Box::new(|d| d.points[0].latitude = f64::NAN)),
            ("source inconnue", Box::new(|d| d.series[0].source_type = "Exotique".into())),
            ("role inconnu", Box::new(|d| d.series[0].role = "Main".into())),
            ("point_index", Box::new(|d| d.series[0].point_index = 9)),
            ("temperature absurde", Box::new(|d| d.series[0].measurements[0].1 = 150.0)),
            ("horodatage 1970", Box::new(|d| d.series[0].measurements[0].0 = 5)),
            ("series en mode points", Box::new(|d| d.mode = ShareMode::Points)),
            ("format", Box::new(|d| d.format = "gws-share/2".into())),
            ("projet a balise", Box::new(|d| d.source_project = Some("<style>".into()))),
        ];
        for (nom, altere) in cas {
            let mut d = doc_series();
            altere(&mut d);
            assert!(validate_document(&d).is_err(), "{nom} aurait dû être refusé");
        }
    }

    #[test]
    fn import_cree_un_projet_a_part_avec_horodatage_brut_et_empreinte_recalculee() {
        let db = Db::open_in_memory().unwrap();
        let d = doc_series();
        let rapport = import_document(&db, "XZ4K9P", &d, &ImportTarget::NewProject).unwrap();
        assert_eq!(rapport.project_name, "Partage XZ4-K9P — TAF vercors");
        assert_eq!((rapport.points_created, rapport.series_created, rapport.measurements_written), (2, 1, 3));

        let points = db.list_points(&rapport.project_id).unwrap();
        let darb = points.iter().find(|p| p.name == "Darbounouse").unwrap();
        assert_ne!(darb.id, "0f8e7c1a-0000-4000-8000-000000000001", "identifiant neuf, jamais celui de l'émetteur");
        let series = db.list_series_for_point(&darb.id).unwrap();
        assert_eq!(series.len(), 1);
        let s = &series[0];
        assert_eq!(s.source_type, SourceType::ElitechDirect, "type de source d'origine conservé");
        assert_eq!(s.clock_offset_seconds, -3600);
        assert_ne!(s.content_fingerprint.as_deref(), Some("affirmation-de-l-emetteur"));

        let m = db.measurements_for_series(&s.id).unwrap();
        assert_eq!(secs(m[0].raw_timestamp), 1_757_000_000);
        assert_eq!(secs(m[0].timestamp_utc), 1_757_000_000 - 3600, "UTC = brut + décalage");
    }

    #[test]
    fn un_partage_invalide_ne_cree_aucun_projet() {
        let db = Db::open_in_memory().unwrap();
        let mut d = doc_series();
        d.points[1].category = "Cat4".into(); // Cat4 sans taille : refusé
        assert!(import_document(&db, "XZ4K9P", &d, &ImportTarget::NewProject).is_err());
        assert!(db.list_projects().unwrap().is_empty());
    }

    #[test]
    fn un_echec_en_cours_d_ecriture_defait_le_projet_cree() {
        let db = Db::open_in_memory().unwrap();
        let mut d = doc_series();
        // Passe la validation du format, mais la base la refuse : une table
        // disparue simule une panne d'écriture au milieu de l'import.
        db.with_conn(|c| c.execute_batch("DROP TABLE measurements")).unwrap();
        d.series[0].measurements.truncate(1);
        assert!(import_document(&db, "XZ4K9P", &d, &ImportTarget::NewProject).is_err());
        assert!(db.list_projects().unwrap().is_empty(), "le projet à moitié rempli doit être retiré");
    }

    #[test]
    fn construction_puis_relecture_aller_retour() {
        let db = Db::open_in_memory().unwrap();
        let rapport = import_document(&db, "ABC123", &doc_series(), &ImportTarget::NewProject).unwrap();
        let ids: Vec<String> = db.list_points(&rapport.project_id).unwrap().into_iter().map(|p| p.id).collect();
        let req = ShareRequest { project_id: rapport.project_id.clone(), point_ids: ids, include_series: true, ttl_days: None, period_days: None };
        let (doc, n) = build_document(&db, &req).unwrap();
        assert_eq!((doc.mode, doc.points.len(), doc.series.len(), n), (ShareMode::PointsSeries, 2, 1, 3));
        assert_eq!(doc.series[0].measurements[0], (1_757_000_000, -12.5), "le brut repart, pas l'UTC corrigé");
        serialize_document(&doc).unwrap();

        let req = ShareRequest { include_series: false, ..req };
        let (doc, n) = build_document(&db, &req).unwrap();
        assert_eq!((doc.mode, doc.series.len(), n), (ShareMode::Points, 0, 0));
    }

    /// Cycle complet contre le **vrai** serveur : création, aperçu, import dans un
    /// second poste simulé, révocation, puis code devenu illisible.
    ///
    /// `cargo test --lib -- --ignored --nocapture test_reel_cycle_de_partage`
    #[test]
    #[ignore = "appelle le serveur de partage réel (gastinel.me)"]
    fn test_reel_cycle_de_partage() {
        // Émetteur : un projet avec deux points (dont un Col) et une série.
        let emetteur = Db::open_in_memory().unwrap();
        let recu = import_document(&emetteur, "TEST00", &doc_series(), &ImportTarget::NewProject).unwrap();
        let ids = emetteur.list_points(&recu.project_id).unwrap().into_iter().map(|p| p.id).collect();
        let req = ShareRequest { project_id: recu.project_id, point_ids: ids, include_series: true, ttl_days: Some(1), period_days: None };
        let (doc, n) = build_document(&emetteur, &req).unwrap();
        let body = serialize_document(&doc).unwrap();
        let cree = publish(&emetteur, &req, &doc, n, body).unwrap();
        println!("Partage créé : {} (expire {})", cree.display_code, cree.expires_at);

        let apercu = fetch_preview(&cree.display_code.to_lowercase()).unwrap();
        assert_eq!((apercu.point_count, apercu.series_count), (2, 1));

        // Destinataire : une autre base, comme un autre poste.
        let destinataire = Db::open_in_memory().unwrap();
        let recu = fetch_document(&cree.code).unwrap();
        let rapport = import_document(&destinataire, &cree.code, &recu, &ImportTarget::NewProject).unwrap();
        assert_eq!((rapport.points_created, rapport.series_created, rapport.measurements_written), (2, 1, 3));

        revoke(&emetteur, &cree.code).unwrap();
        assert!(matches!(fetch_preview(&cree.code), Err(ShareError::NotFound)));
        assert_eq!(list_issued(&emetteur).unwrap()[0].status, "revoked");
    }

    #[test]
    fn la_periode_ne_garde_que_les_mesures_recentes() {
        let db = Db::open_in_memory().unwrap();
        // Les mesures de `doc_series` datent de septembre 2025.
        let rapport = import_document(&db, "ABC123", &doc_series(), &ImportTarget::NewProject).unwrap();
        let ids: Vec<String> = db.list_points(&rapport.project_id).unwrap().into_iter().map(|p| p.id).collect();
        let req = ShareRequest {
            project_id: rapport.project_id,
            point_ids: ids,
            include_series: true,
            ttl_days: None,
            period_days: Some(30),
        };
        let (doc, n) = build_document(&db, &req).unwrap();
        assert_eq!((doc.mode, n), (ShareMode::Points, 0), "rien de récent : les points seuls partent");
        let (doc, n) = build_document(&db, &ShareRequest { period_days: None, ..req }).unwrap();
        assert_eq!((doc.mode, n), (ShareMode::PointsSeries, 3));
    }

    #[test]
    fn separateur_de_milliers() {
        assert_eq!(milliers(477_312), "477 312");
        assert_eq!(milliers(450_000), "450 000");
        assert_eq!(milliers(12), "12");
        assert_eq!(milliers(1_000_000), "1 000 000");
    }

    #[test]
    fn les_partages_emis_ne_revelent_pas_le_jeton() {
        let db = Db::open_in_memory().unwrap();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO issued_shares (code, project_id, mode, point_count, series_count,
                     measurement_count, created_at, expires_at, revoke_token)
                 VALUES ('XZ4K9P', NULL, 'points', 1, 0, 0, 100, 200, 'secret-jeton-de-test-0123456789')",
                [],
            )
        })
        .unwrap();
        let liste = list_issued(&db).unwrap();
        assert_eq!(liste[0].status, "expired");
        let json = serde_json::to_string(&liste).unwrap();
        assert!(!json.contains("secret-jeton"), "le jeton ne doit jamais sortir vers l'UI");
    }
}

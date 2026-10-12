//! Projet Cloud — synchronisation chiffrée de bout en bout.
//!
//! ## Principe de sécurité
//!
//! * **Le mot de passe ne transite jamais** :
//!   `mot de passe + kdf_salt --Argon2id--> auth_key (32 o) || kek (32 o)`
//!   - `auth_key` est envoyée au serveur pour authentifier la session (le serveur n'en
//!     garde qu'une empreinte salée, jamais `auth_key` directement).
//!   - `kek` (Key Encryption Key) reste strictement sur l'appareil et sert à emballer /
//!     déballer la `data_key` (clé de chiffrement des données).
//! * **Le serveur ne voit que des octets chiffrés** :
//!   Tous les enregistrements (projets, stations, séries) sont chiffrés en local avec
//!   la `data_key` via **AES-256-GCM** (AEAD authentifié) avant tout téléversement.
//! * **Format de bloc inviolable** :
//!   Chaque bloc téléversé porte le préfixe magique `GWC1` (Gastinel Weather Cloud v1),
//!   est compressé en gzip si volumineux, et chiffré de bout en bout.
//! * **Zéro tiers de confiance** :
//!   Même en cas de compromission totale du VPS, l'attaquant n'a accès qu'à des blocs
//!   opaques chiffrés et ne peut ni lire les points météo ni déchiffrer les séries.

use std::collections::HashSet;
use std::io::Read;
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::prelude::*;
use chrono::Utc;
use flate2::read::{GzDecoder, GzEncoder};
use flate2::Compression;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::Db;
use crate::error::AppError;
use crate::model::{Category, LoggerSeries, Measurement, Point, PointSize, SeriesRole, SourceType};
use crate::cloud_mois::{cle_meta, cle_mois, est_un_mois, lire_repere, repere};
use crate::parsing::DateOrder;

/// Erreurs du Projet Cloud, rédigées pour l'utilisateur final.
#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("{0}")]
    Validation(String),

    #[error("Serveur Cloud injoignable : {0}. Vérifiez la connexion Internet.")]
    Network(String),

    #[error(
        "Le serveur Cloud a répondu de façon inattendue ({0}). Réessayez dans quelques \
         minutes : il est peut-être en maintenance."
    )]
    BadResponse(String),

    #[error("Erreur interne Cloud : {0}")]
    Internal(String),

    #[error(
        "La base de données locale a refusé l'opération Cloud (détail : {0}). \
         Réessayez ; si le message revient, fermez puis rouvrez GWS."
    )]
    Database(#[from] rusqlite::Error),
}

impl From<CloudError> for AppError {
    fn from(e: CloudError) -> Self {
        AppError::Other(e.to_string())
    }
}

impl From<AppError> for CloudError {
    fn from(e: AppError) -> Self {
        CloudError::Internal(e.to_string())
    }
}

pub type CloudResult<T> = Result<T, CloudError>;

pub const API_BASE: &str = "https://gastinel.me/gws/api/v1";

/// Adresse de l'API. Les tests d'intégration la redirigent vers une copie
/// locale du serveur (`GWS_CLOUD_TEST_API`) ; l'app publiée ne lit rien.
fn api_base() -> String {
    #[cfg(test)]
    if let Ok(u) = std::env::var("GWS_CLOUD_TEST_API") {
        return u;
    }
    API_BASE.to_string()
}

pub const SETTING_CLOUD_ACCOUNT: &str = "cloud_account";
pub const SETTING_CLOUD_TOKEN: &str = "cloud_token";
pub const SETTING_CLOUD_KDF_SALT: &str = "cloud_kdf_salt";
pub const SETTING_CLOUD_WRAPPED_KEY: &str = "cloud_wrapped_key";
pub const SETTING_CLOUD_DATA_KEY: &str = "cloud_data_key";
pub const SETTING_CLOUD_AUTO_SYNC: &str = "cloud_auto_sync";
pub const SETTING_CLOUD_LAST_SYNC_AT: &str = "cloud_last_sync_at";
pub const SETTING_CLOUD_LAST_SYNC_SEQ: &str = "cloud_last_sync_seq";

/// Préfixe magique obligatoire de 4 octets exigé par le serveur cloud pour chaque bloc.
pub const CLOUD_MAGIC: &[u8; 4] = b"GWC1";

/// Valide la forme d'un identifiant de compte : 3 à 32 lettres minuscules.
pub fn is_valid_account(account: &str) -> bool {
    account.len() >= 3
        && account.len() <= 32
        && account.chars().all(|c| c.is_ascii_lowercase())
}

/// Valide la longueur du mot de passe : 8 à 30 caractères.
pub fn is_valid_password(password: &str) -> bool {
    password.len() >= 8 && password.len() <= 30
}

/// Dérive `(auth_key, kek)` à partir du mot de passe et du sel avec Argon2id.
/// Retourne `(auth_key: [u8; 32], kek: [u8; 32])`.
pub fn derive_auth_and_kek(password: &str, salt: &[u8]) -> Result<([u8; 32], [u8; 32]), CloudError> {
    // Paramètres Argon2id : 64 Mio, 3 itérations, 1 thread
    let params = Params::new(65536, 3, 1, Some(64))
        .map_err(|e| CloudError::Internal(format!("Paramètres Argon2 invalides : {e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut output = [0u8; 64];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut output)
        .map_err(|e| CloudError::Internal(format!("Échec dérivation Argon2id : {e}")))?;

    let mut auth_key = [0u8; 32];
    let mut kek = [0u8; 32];
    auth_key.copy_from_slice(&output[..32]);
    kek.copy_from_slice(&output[32..]);
    Ok((auth_key, kek))
}

/// Chiffre des octets en AES-256-GCM : `nonce (12 o) || ciphertext + tag (16 o)`.
pub fn encrypt_aes_gcm(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, CloudError> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| CloudError::Internal(format!("Clé AES invalide : {e}")))?;

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| CloudError::Internal(format!("Échec chiffrement AES-GCM : {e}")))?;

    let mut result = Vec::with_capacity(12 + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// Déchiffre des octets en AES-256-GCM depuis `nonce (12 o) || ciphertext + tag`.
pub fn decrypt_aes_gcm(key: &[u8; 32], payload: &[u8]) -> Result<Vec<u8>, CloudError> {
    if payload.len() < 12 + 16 {
        return Err(CloudError::Validation("Paquet chiffré trop court ou tronqué".into()));
    }
    let (nonce_bytes, ciphertext) = payload.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| CloudError::Internal(format!("Clé AES invalide : {e}")))?;
    let nonce = Nonce::from_slice(nonce_bytes);

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| CloudError::Validation("Authentification échouée : mot de passe incorrect ou données corrompues".into()))
}

/// Calcule l'empreinte SHA-256 hexadécimale de données.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Prépare le payload : compresse en gzip si > 1024 octets.
/// Préfixe : `0x01` si gzip, `0x00` si JSON brut.
pub fn pack_payload(json_bytes: &[u8]) -> Vec<u8> {
    if json_bytes.len() > 1024 {
        let mut encoder = GzEncoder::new(json_bytes, Compression::fast());
        let mut compressed = Vec::new();
        if encoder.read_to_end(&mut compressed).is_ok() && compressed.len() < json_bytes.len() {
            let mut res = Vec::with_capacity(1 + compressed.len());
            res.push(0x01);
            res.extend_from_slice(&compressed);
            return res;
        }
    }
    let mut res = Vec::with_capacity(1 + json_bytes.len());
    res.push(0x00);
    res.extend_from_slice(json_bytes);
    res
}

/// Déballe le payload : décompresse si `0x01` (gzip), sinon renvoie le JSON brut.
pub fn unpack_payload(payload: &[u8]) -> Result<Vec<u8>, CloudError> {
    if payload.is_empty() {
        return Ok(Vec::new());
    }
    if payload[0] == 0x01 {
        let mut decoder = GzDecoder::new(&payload[1..]);
        let mut decompressed = Vec::new();
        (&mut decoder)
            .take(64 * 1024 * 1024)
            .read_to_end(&mut decompressed)
            .map_err(|e| CloudError::Validation(format!("Erreur décompression gzip : {e}")))?;
        return Ok(decompressed);
    }
    if payload[0] == 0x00 {
        return Ok(payload[1..].to_vec());
    }
    // Repli : si pas de préfixe 0x00/0x01 (ex. JSON direct commençant par '{')
    Ok(payload.to_vec())
}

/// Chiffre un enregistrement Cloud pour le téléversement :
/// empaquette (avec compression gzip si > 1 Ko), chiffre en AES-256-GCM, et
/// préfixe avec `GWC1`.
pub fn encrypt_record(key: &[u8; 32], plaintext_json: &[u8]) -> Result<Vec<u8>, CloudError> {
    let packed = pack_payload(plaintext_json);
    let ciphertext = encrypt_aes_gcm(key, &packed)?;
    let mut record = Vec::with_capacity(4 + ciphertext.len());
    record.extend_from_slice(CLOUD_MAGIC);
    record.extend_from_slice(&ciphertext);
    Ok(record)
}

/// Déchiffre un enregistrement Cloud téléchargé du serveur :
/// vérifie le préfixe `GWC1`, déchiffre via AES-256-GCM, et décompresse si nécessaire.
pub fn decrypt_record(key: &[u8; 32], raw_bytes: &[u8]) -> Result<Vec<u8>, CloudError> {
    if raw_bytes.len() < 12 + 16 {
        return Err(CloudError::Validation("Paquet chiffré trop court ou tronqué".into()));
    }
    let encrypted = if raw_bytes.len() >= 4 + 12 + 16 && &raw_bytes[..4] == CLOUD_MAGIC {
        &raw_bytes[4..]
    } else {
        raw_bytes
    };
    let decrypted_packed = decrypt_aes_gcm(key, encrypted)?;
    unpack_payload(&decrypted_packed)
}

// ---------------------------------------------------------------------------
// Client HTTP
// ---------------------------------------------------------------------------

fn http_client() -> Result<reqwest::blocking::Client, CloudError> {
    crate::http_client::client_builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        // HTTPS imposé, sauf en test contre la copie locale du serveur.
        .https_only(!cfg!(test) || std::env::var("GWS_CLOUD_TEST_API").is_err())
        .build()
        .map_err(|e| CloudError::Network(format!("Erreur client HTTP : {e}")))
}

// ---------------------------------------------------------------------------
// Statuts et structures de données
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudStatus {
    pub logged_in: bool,
    pub account: Option<String>,
    pub last_sync_at: Option<i64>,
    pub auto_sync: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudSyncResult {
    pub uploaded: usize,
    pub downloaded: usize,
    pub deleted: usize,
    pub details: Vec<String>,
}

/// Avancement d'une synchro, émis à chaque élément réellement échangé.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CloudProgress {
    /// « verification », « reception », « envoi » ou « suppression ».
    pub phase: &'static str,
    /// Projet en cours d'envoi ou de vérification ; vide hors de cette étape.
    pub projet: String,
    /// Ce qui passe, en clair pour l'utilisateur (« Série Darbounouse »).
    pub label: String,
    pub done: usize,
    pub total: usize,
}

/// Réglage par projet : `cloud_exclude:<id>` vaut « 1 » (exclu) ou « 0 » (synchronisé).
pub const SETTING_CLOUD_EXCLUDE_PREFIX: &str = "cloud_exclude:";
/// Sans réglage explicite, un projet plus gros que ceci n'est pas synchronisé :
/// le projet France (~550 M mesures) ne part pas dans le Cloud par surprise.
pub const SEUIL_EXCLUSION_AUTO: i64 = 10_000_000;

/// Un projet est-il exclu de la synchro ? Un projet d'exemple l'est toujours ;
/// sinon réglage explicite, sinon seuil.
pub fn projet_exclu(db: &Db, project_id: &str, nb_mesures: i64) -> bool {
    if crate::demo::est_exemple(db, project_id) {
        return true;
    }
    match db.get_setting(&format!("{SETTING_CLOUD_EXCLUDE_PREFIX}{project_id}")).ok().flatten().as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => nb_mesures > SEUIL_EXCLUSION_AUTO,
    }
}

#[derive(Debug, Deserialize)]
struct PreloginResponse {
    kdf_salt: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct LoginResponse {
    token: String,
    expires_at: i64,
    kdf_salt: String,
    wrapped_key: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct RegisterResponse {
    token: String,
    expires_at: i64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ChangesResponse {
    seq: i64,
    more: bool,
    records: Vec<CloudRecordMeta>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct CloudRecordMeta {
    key: String,
    seq: i64,
    deleted: bool,
    size: usize,
}

// Structures pour la sérialisation des entités dans le cloud
#[derive(Debug, Serialize, Deserialize)]
struct SyncedProject {
    id: String,
    name: String,
    created_at: i64,
    last_opened_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct SyncedPoint {
    id: String,
    project_id: String,
    name: String,
    latitude: f64,
    longitude: f64,
    altitude_m: Option<f64>,
    category: String,
    size: Option<String>,
    factor_sky: Option<String>,
    factor_wind: Option<String>,
    factor_slope: Option<String>,
    factor_snow_cover: Option<String>,
    factor_forest_cover: Option<String>,
    is_open_terrain: Option<bool>,
    notes: Option<String>,
    infoclimat_station_id: Option<String>,
    romma_station_id: Option<String>,
    created_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct SyncedSeries {
    id: String,
    point_id: String,
    source_type: String,
    role: String,
    logger_serial: Option<String>,
    imported_at: i64,
    raw_file_reference: Option<String>,
    clock_offset_seconds: i64,
    raw_timestamp_was_utc: bool,
    label: Option<String>,
    sample_interval_seconds: Option<i64>,
    date_order: String,
    date_format_uncertain: bool,
    data_license: Option<String>,
    station_id: Option<String>,
    last_api_fetch_at: Option<i64>,
    content_fingerprint: Option<String>,
    measurements: Vec<SyncedMeasurement>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SyncedMeasurement {
    ts: i64,
    raw: i64,
    t: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    h: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rssi: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sol: Option<f64>,
}

// ---------------------------------------------------------------------------
// Fonctions métier Cloud
// ---------------------------------------------------------------------------

/// Retourne le statut courant de la connexion Cloud.
pub fn get_status(db: &Db) -> CloudResult<CloudStatus> {
    let account = db.get_setting(SETTING_CLOUD_ACCOUNT)?;
    let token = db.get_setting(SETTING_CLOUD_TOKEN)?;
    let auto_sync = db.get_setting(SETTING_CLOUD_AUTO_SYNC)?.map(|v| v == "1").unwrap_or(true);
    let last_sync_at = db
        .get_setting(SETTING_CLOUD_LAST_SYNC_AT)?
        .and_then(|s| s.parse::<i64>().ok());

    let logged_in = account.is_some() && token.is_some();

    Ok(CloudStatus {
        logged_in,
        account,
        last_sync_at,
        auto_sync,
    })
}

/// Première inscription avec un code d'invitation généré par l'administrateur.
pub fn register(account: &str, password: &str, invite: &str, db: &Db) -> CloudResult<CloudStatus> {
    let account = account.trim().to_lowercase();
    if !is_valid_account(&account) {
        return Err(CloudError::Validation(
            "Identifiant invalide : 3 à 32 lettres minuscules (a-z uniquement).".into(),
        ));
    }
    if !is_valid_password(password) {
        return Err(CloudError::Validation(
            "Mot de passe invalide : 8 à 30 caractères requis.".into(),
        ));
    }
    let invite = invite.trim();
    if invite.is_empty() {
        return Err(CloudError::Validation(
            "Code d'invitation requis pour la première inscription.".into(),
        ));
    }

    // 1. Génération du sel et de la clé de données aléatoire
    let mut kdf_salt = [0u8; 16];
    OsRng.fill_bytes(&mut kdf_salt);

    let mut data_key = [0u8; 32];
    OsRng.fill_bytes(&mut data_key);

    // 2. Dérivation Argon2id
    let (auth_key, kek) = derive_auth_and_kek(password, &kdf_salt)?;

    // 3. Emballage de la clé de données avec la kek
    let wrapped_key = encrypt_aes_gcm(&kek, &data_key)?;

    // 4. Appel d'enregistrement au serveur
    let client = http_client()?;
    let url = format!("{}/cloud/register", api_base());
    let payload = serde_json::json!({
        "account": account,
        "invite": invite,
        "kdf_salt": BASE64_STANDARD.encode(kdf_salt),
        "auth_key": BASE64_STANDARD.encode(auth_key),
        "wrapped_key": BASE64_STANDARD.encode(wrapped_key.clone()),
    });
    let body = serde_json::to_vec(&payload)
        .map_err(|e| CloudError::Internal(format!("Erreur sérialisation : {e}")))?;

    let mut resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .map_err(|e| CloudError::Network(format!("Impossible de joindre le serveur : {e}")))?;

    let mut resp_body = Vec::new();
    (&mut resp)
        .take(64 * 1024)
        .read_to_end(&mut resp_body)
        .map_err(|e| CloudError::Network(format!("Erreur lecture réponse : {e}")))?;

    if !resp.status().is_success() {
        let err_text = serde_json::from_slice::<serde_json::Value>(&resp_body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| "Erreur lors de l'enregistrement".into());
        return Err(CloudError::Validation(err_text));
    }

    let reg: RegisterResponse = serde_json::from_slice(&resp_body)
        .map_err(|e| CloudError::BadResponse(format!("Réponse serveur invalide : {e}")))?;

    // 5. Stockage sécurisé des réglages
    db.set_setting(SETTING_CLOUD_ACCOUNT, &account)?;
    db.set_setting(SETTING_CLOUD_TOKEN, &reg.token)?;
    db.set_setting(SETTING_CLOUD_KDF_SALT, &BASE64_STANDARD.encode(kdf_salt))?;
    db.set_setting(SETTING_CLOUD_WRAPPED_KEY, &BASE64_STANDARD.encode(wrapped_key))?;
    db.set_setting(SETTING_CLOUD_DATA_KEY, &BASE64_STANDARD.encode(data_key))?;
    db.set_setting(SETTING_CLOUD_LAST_SYNC_SEQ, "0")?;

    get_status(db)
}

/// Connexion à un compte existant.
pub fn login(account: &str, password: &str, db: &Db) -> CloudResult<CloudStatus> {
    let account = account.trim().to_lowercase();
    if !is_valid_account(&account) {
        return Err(CloudError::Validation(
            "Identifiant invalide : 3 à 32 lettres minuscules (a-z uniquement).".into(),
        ));
    }
    if !is_valid_password(password) {
        return Err(CloudError::Validation(
            "Mot de passe invalide : 8 à 30 caractères requis.".into(),
        ));
    }

    let client = http_client()?;

    // 1. Pré-connexion pour récupérer le sel
    let pre_url = format!("{}/cloud/prelogin/{account}", api_base());
    let mut pre_resp = client
        .get(&pre_url)
        .send()
        .map_err(|e| CloudError::Network(format!("Impossible de joindre le serveur : {e}")))?;

    if !pre_resp.status().is_success() {
        return Err(CloudError::Validation("Identifiant ou mot de passe incorrect.".into()));
    }

    let mut pre_body = Vec::new();
    (&mut pre_resp)
        .take(64 * 1024)
        .read_to_end(&mut pre_body)
        .map_err(|e| CloudError::Network(format!("Erreur lecture pré-connexion : {e}")))?;

    let pre: PreloginResponse = serde_json::from_slice(&pre_body)
        .map_err(|e| CloudError::BadResponse(format!("Réponse serveur invalide : {e}")))?;

    let kdf_salt = BASE64_STANDARD
        .decode(&pre.kdf_salt)
        .map_err(|_| CloudError::BadResponse("Sel invalide renvoyé par le serveur".into()))?;

    // 2. Dérivation Argon2id
    let (auth_key, kek) = derive_auth_and_kek(password, &kdf_salt)?;

    // 3. Connexion
    let login_url = format!("{}/cloud/login", api_base());
    let payload = serde_json::json!({
        "account": account,
        "auth_key": BASE64_STANDARD.encode(auth_key),
    });
    let body = serde_json::to_vec(&payload)
        .map_err(|e| CloudError::Internal(format!("Erreur sérialisation : {e}")))?;

    let mut resp = client
        .post(&login_url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .map_err(|e| CloudError::Network(format!("Impossible de joindre le serveur : {e}")))?;

    let mut resp_body = Vec::new();
    (&mut resp)
        .take(64 * 1024)
        .read_to_end(&mut resp_body)
        .map_err(|e| CloudError::Network(format!("Erreur lecture réponse login : {e}")))?;

    if !resp.status().is_success() {
        return Err(CloudError::Validation("Identifiant ou mot de passe incorrect.".into()));
    }

    let log_res: LoginResponse = serde_json::from_slice(&resp_body)
        .map_err(|e| CloudError::BadResponse(format!("Réponse serveur invalide : {e}")))?;

    let wrapped_key = BASE64_STANDARD
        .decode(&log_res.wrapped_key)
        .map_err(|_| CloudError::BadResponse("Clé chiffrée reçue invalide".into()))?;

    // 4. Déballage de la clé de données locale
    let data_key_vec = decrypt_aes_gcm(&kek, &wrapped_key)
        .map_err(|_| CloudError::Validation("Mot de passe incorrect ou clé corrompue.".into()))?;

    if data_key_vec.len() != 32 {
        return Err(CloudError::Validation("Clé de données déballée de taille invalide.".into()));
    }

    // 5. Stockage des réglages
    db.set_setting(SETTING_CLOUD_ACCOUNT, &account)?;
    db.set_setting(SETTING_CLOUD_TOKEN, &log_res.token)?;
    db.set_setting(SETTING_CLOUD_KDF_SALT, &log_res.kdf_salt)?;
    db.set_setting(SETTING_CLOUD_WRAPPED_KEY, &log_res.wrapped_key)?;
    db.set_setting(SETTING_CLOUD_DATA_KEY, &BASE64_STANDARD.encode(&data_key_vec))?;

    get_status(db)
}

/// Déconnexion du compte Cloud.
pub fn logout(db: &Db) -> CloudResult<CloudStatus> {
    let token = db.get_setting(SETTING_CLOUD_TOKEN)?;
    if let Some(tok) = token {
        if let Ok(client) = http_client() {
            let _ = client
                .post(format!("{}/cloud/logout", api_base()))
                .header("Authorization", format!("Bearer {tok}"))
                .send();
        }
    }

    db.delete_setting(SETTING_CLOUD_ACCOUNT)?;
    db.delete_setting(SETTING_CLOUD_TOKEN)?;
    db.delete_setting(SETTING_CLOUD_KDF_SALT)?;
    db.delete_setting(SETTING_CLOUD_WRAPPED_KEY)?;
    db.delete_setting(SETTING_CLOUD_DATA_KEY)?;
    db.delete_setting(SETTING_CLOUD_LAST_SYNC_SEQ)?;
    let _ = db.clear_cloud_synced_hashes();

    get_status(db)
}

/// Synchronisation bidirectionnelle chiffrée avec le serveur Cloud.
/// Envois simultanés : le serveur n'en traite que deux à la fois par compte.
const ENVOIS_PARALLELES: usize = 2;

struct MoisAEnvoyer {
    cle: String,
    serie: String,
    mois: i64,
    version: i64,
    label: String,
}

fn nom_mois(mois: i64) -> String {
    const NOMS: [&str; 12] = [
        "janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre",
        "décembre",
    ];
    let m = (mois % 100) as usize;
    format!("{} {}", NOMS.get(m.wrapping_sub(1)).copied().unwrap_or("?"), mois / 100)
}

/// Chiffre et envoie un enregistrement. `false` si le serveur ne l'a pas pris.
fn envoyer(client: &reqwest::blocking::Client, token: &str, cle_donnees: &[u8; 32], cle: &str, clair: &[u8]) -> bool {
    let Ok(chiffre) = encrypt_record(cle_donnees, clair) else { return false };
    match client
        .put(format!("{}/cloud/records/{cle}", api_base()))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/octet-stream")
        .body(chiffre)
        .send()
    {
        Ok(r) if r.status().is_success() => true,
        Ok(r) => {
            eprintln!("[gws cloud] PUT {cle} échoué : code {}", r.status());
            false
        }
        Err(e) => {
            eprintln!("[gws cloud] PUT {cle} échoué : {e}");
            false
        }
    }
}

/// Envoie les mois en attente ; renvoie le nombre d'envois réussis.
fn envoyer_mois(
    db: &Db,
    client: &reqwest::blocking::Client,
    token: &str,
    cle_donnees: &[u8; 32],
    liste: &[MoisAEnvoyer],
    progres: &(dyn Fn(CloudProgress) + Sync),
) -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let suivant = AtomicUsize::new(0);
    let faits = AtomicUsize::new(0);
    let reussis = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..ENVOIS_PARALLELES.min(liste.len()) {
            sc.spawn(|| loop {
                let i = suivant.fetch_add(1, Ordering::Relaxed);
                let Some(t) = liste.get(i) else { break };
                progres(CloudProgress {
                    phase: "envoi",
                    label: t.label.clone(),
                    done: faits.load(Ordering::Relaxed),
                    total: liste.len(),
                    ..Default::default()
                });
                // Version lue *avant* les mesures : si elles changent entre-temps,
                // le repère sera en retard et le mois repartira la fois suivante.
                if let Ok(mesures) = db.measurements_in_month(&t.serie, t.mois) {
                    let clair = crate::cloud_mois::encoder(&t.serie, t.mois, &mesures);
                    drop(mesures);
                    if envoyer(client, token, cle_donnees, &t.cle, &clair) {
                        let _ = db.set_cloud_synced_hash(&t.cle, &repere(t.version, &t.serie, t.mois));
                        reussis.fetch_add(1, Ordering::Relaxed);
                    }
                }
                faits.fetch_add(1, Ordering::Relaxed);
            });
        }
    });
    reussis.into_inner()
}

/// Écrit un mois reçu du Cloud et note sa version locale, pour qu'il ne
/// reparte pas au prochain envoi.
fn appliquer_mois(db: &Db, cle: &str, clair: &[u8]) -> Result<(), String> {
    let (serie, mois, mesures) = crate::cloud_mois::decoder(clair)?;
    let v = db.replace_month(&serie, mois, &mesures).map_err(|e| e.to_string())?;
    db.set_cloud_synced_hash(cle, &repere(v, &serie, mois)).map_err(|e| e.to_string())
}

/// Série décrite par un enregistrement Cloud (métadonnées seules), pour les tests.
#[cfg(test)]
fn serie_depuis_sync(ss: &SyncedSeries) -> LoggerSeries {
    LoggerSeries {
        id: ss.id.clone(),
        point_id: ss.point_id.clone(),
        source_type: SourceType::parse(&ss.source_type).unwrap_or(SourceType::Manual),
        role: SeriesRole::parse(&ss.role).unwrap_or(SeriesRole::Primary),
        logger_serial: ss.logger_serial.clone(),
        imported_at: chrono::DateTime::from_timestamp(ss.imported_at, 0).unwrap_or_default(),
        raw_file_reference: ss.raw_file_reference.clone(),
        clock_offset_seconds: ss.clock_offset_seconds,
        raw_timestamp_was_utc: ss.raw_timestamp_was_utc,
        label: ss.label.clone(),
        sample_interval_seconds: ss.sample_interval_seconds,
        date_order: DateOrder::parse(&ss.date_order).unwrap_or(DateOrder::DayFirst),
        date_format_uncertain: ss.date_format_uncertain,
        data_license: ss.data_license.clone(),
        station_id: ss.station_id.clone(),
        last_api_fetch_at: ss.last_api_fetch_at.and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
        content_fingerprint: ss.content_fingerprint.clone(),
        measurement_count: 0,
    }
}

/// Nature d'un enregistrement, sans son identifiant (qui ne parle à personne).
fn libelle_cle(cle: &str) -> String {
    match cle.split_once(':').map(|(t, _)| t) {
        Some("project") => "Projet".into(),
        Some("point") => "Station".into(),
        Some("series") => "Série de mesures".into(),
        _ => "Élément".into(),
    }
}

pub fn sync(db: &Db) -> CloudResult<CloudSyncResult> {
    sync_avec_progression(db, &|_| {})
}

/// Synchro complète ; `progres` est appelé à chaque élément échangé.
pub fn sync_avec_progression(db: &Db, progres: &(dyn Fn(CloudProgress) + Sync)) -> CloudResult<CloudSyncResult> {
    let _account = db.get_setting(SETTING_CLOUD_ACCOUNT)?.ok_or_else(|| {
        CloudError::Validation("Aucun compte Cloud configuré. Connectez-vous d'abord.".into())
    })?;
    let token = db.get_setting(SETTING_CLOUD_TOKEN)?.ok_or_else(|| {
        CloudError::Validation("Session Cloud expirée ou absente. Reconnectez-vous.".into())
    })?;
    let data_key_b64 = db.get_setting(SETTING_CLOUD_DATA_KEY)?.ok_or_else(|| {
        CloudError::Validation("Clé de chiffrement absente. Reconnectez-vous.".into())
    })?;
    let data_key_vec = BASE64_STANDARD
        .decode(data_key_b64)
        .map_err(|_| CloudError::Validation("Clé locale corrompue.".into()))?;
    if data_key_vec.len() != 32 {
        return Err(CloudError::Validation("Clé locale invalide.".into()));
    }
    let mut data_key = [0u8; 32];
    data_key.copy_from_slice(&data_key_vec);

    let client = http_client()?;
    let since = db
        .get_setting(SETTING_CLOUD_LAST_SYNC_SEQ)?
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);

    let mut uploaded = 0;
    let mut downloaded = 0;
    let mut deleted = 0;
    let mut details = Vec::new();

    // -----------------------------------------------------------------------
    // 1. Récupération des changements distants depuis le dernier numéro de séquence
    // -----------------------------------------------------------------------
    //
    // Chaque page est lue directement depuis la réponse (sans copie du corps)
    // et rangée aussitôt : seules les clés restent, déjà classées dans l'ordre
    // de traitement. Les enregistrements eux-mêmes ne sont demandés qu'une fois
    // toutes les pages lues — les suppressions passent avant toutes les
    // réceptions, quelle que soit la page d'où elles viennent : traiter chaque
    // page à son arrivée changerait l'état final.
    let mut suppressions: [Vec<String>; 3] = Default::default(); // séries, stations, projets
    let mut receptions: [Vec<String>; 3] = Default::default(); // projets, stations, séries
    let mut cursor = since;
    let mut latest_seq;

    loop {
        let changes_url = format!("{}/cloud/changes?since={cursor}", api_base());
        let mut resp = client
            .get(&changes_url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .map_err(|e| CloudError::Network(format!("Erreur lors de la récupération des changements : {e}")))?;

        if resp.status().as_u16() == 401 {
            return Err(CloudError::Validation("Session Cloud expirée. Veuillez vous reconnecter.".into()));
        }

        if !resp.status().is_success() {
            return Err(CloudError::Network(format!("Erreur serveur lors de la synchronisation (code {})", resp.status())));
        }

        let changes: ChangesResponse = serde_json::from_reader(std::io::BufReader::new((&mut resp).take(16 * 1024 * 1024)))
            .map_err(|e| {
                if e.is_io() {
                    CloudError::Network(format!("Erreur lecture changements : {e}"))
                } else {
                    CloudError::BadResponse(format!("Format des changements invalide : {e}"))
                }
            })?;

        latest_seq = changes.seq;
        let has_more = changes.more;
        let count = changes.records.len();

        for rec in changes.records {
            if rec.seq > cursor {
                cursor = rec.seq;
            }
            // Mêmes rangs que les anciens tris (une clé inconnue va en dernier).
            let (serie, station, projet) =
                (rec.key.starts_with("series:"), rec.key.starts_with("point:"), rec.key.starts_with("project:"));
            if rec.deleted {
                suppressions[if serie { 0 } else if station { 1 } else { 2 }].push(rec.key);
            } else {
                receptions[if projet { 0 } else if station { 1 } else { 2 }].push(rec.key);
            }
        }

        if !has_more || count == 0 || cursor >= latest_seq {
            break;
        }
    }

    // -----------------------------------------------------------------------
    // 2. Traitement des suppressions distantes en premier (ordre : séries, points, projets)
    // -----------------------------------------------------------------------
    // Rangement par nature = ancien tri stable : l'ordre du serveur est gardé
    // au sein de chaque nature.
    let total_suppr: usize = suppressions.iter().map(Vec::len).sum();
    for (i, key) in suppressions.into_iter().flatten().enumerate() {
        progres(CloudProgress { phase: "suppression", label: libelle_cle(&key), done: i, total: total_suppr, ..Default::default() });
        let id = match key.split_once(':') {
            Some((_, id)) => id,
            None => continue,
        };
        if key.starts_with("series:") {
            // Un mois ou une série : le repère local le dit. Mois vidé sur un
            // autre poste → on vide le même ; sinon c'est la série entière.
            match db.get_cloud_synced_hash(&key).ok().flatten().as_deref().and_then(lire_repere) {
                Some((_, serie, mois)) => {
                    let _ = db.replace_month(&serie, mois, &[]);
                }
                None => {
                    let _ = db.delete_series(id);
                }
            }
            let _ = db.delete_cloud_synced_hash(&key);
            deleted += 1;
        } else if key.starts_with("point:") {
            let _ = db.delete_point(id);
            let _ = db.delete_cloud_synced_hash(&key);
            deleted += 1;
        } else if key.starts_with("project:") {
            let _ = db.delete_project(id);
            let _ = db.delete_cloud_synced_hash(&key);
            deleted += 1;
        }
    }

    // -----------------------------------------------------------------------
    // 3. Traitement des insertions/mises à jour distantes (ordre : projets, points, séries)
    // -----------------------------------------------------------------------
    // Entre séries, l'ordre du serveur (métadonnées envoyées avant leurs mois)
    // est conservé.
    let mut mois_en_attente: Vec<(String, Vec<u8>)> = Vec::new();

    let total_recu: usize = receptions.iter().map(Vec::len).sum();
    for (i, key) in receptions.into_iter().flatten().enumerate() {
        progres(CloudProgress { phase: "reception", label: libelle_cle(&key), done: i, total: total_recu, ..Default::default() });
        let rec_url = format!("{}/cloud/records/{}", api_base(), key);
        let rec_resp = client
            .get(&rec_url)
            .header("Authorization", format!("Bearer {token}"))
            .send();

        if let Ok(mut r) = rec_resp {
            if r.status().is_success() {
                let mut enc_bytes = Vec::new();
                if (&mut r).take(50 * 1024 * 1024).read_to_end(&mut enc_bytes).is_ok() {
                    if let Ok(decrypted) = decrypt_record(&data_key, &enc_bytes) {
                        let hash = sha256_hex(&decrypted);

                        if key.starts_with("project:") {
                            if let Ok(p) = serde_json::from_slice::<SyncedProject>(&decrypted) {
                                if db.create_project_with_id(&p.id, &p.name, p.created_at, p.last_opened_at).is_ok() {
                                    let _ = db.set_cloud_synced_hash(&key, &hash);
                                    downloaded += 1;
                                }
                            }
                        } else if key.starts_with("point:") {
                            if let Ok(sp) = serde_json::from_slice::<SyncedPoint>(&decrypted) {
                                let cat = Category::parse(&sp.category).unwrap_or(Category::Cat1);
                                let sz = PointSize::parse_optional(sp.size.as_deref()).ok().flatten();
                                let pt = Point {
                                    id: sp.id,
                                    project_id: sp.project_id,
                                    name: sp.name,
                                    latitude: sp.latitude,
                                    longitude: sp.longitude,
                                    altitude_m: sp.altitude_m,
                                    category: cat,
                                    size: sz,
                                    factor_sky: sp.factor_sky,
                                    factor_wind: sp.factor_wind,
                                    factor_slope: sp.factor_slope,
                                    factor_snow_cover: sp.factor_snow_cover,
                                    factor_forest_cover: sp.factor_forest_cover,
                                    is_open_terrain: sp.is_open_terrain,
                                    notes: sp.notes,
                                    infoclimat_station_id: sp.infoclimat_station_id,
                                    romma_station_id: sp.romma_station_id,
                                    created_at: chrono::DateTime::from_timestamp(sp.created_at, 0).unwrap_or_default(),
                                    series_count: 0,
                                };
                                if db.insert_or_replace_point(&pt).is_ok() {
                                    let _ = db.set_cloud_synced_hash(&key, &hash);
                                    downloaded += 1;
                                }
                            }
                        } else if key.starts_with("series:") && est_un_mois(&decrypted) {
                            match appliquer_mois(db, &key, &decrypted) {
                                Ok(()) => downloaded += 1,
                                // Série pas encore reçue : on réessaie en fin de réception.
                                Err(_) => mois_en_attente.push((key.clone(), decrypted)),
                            }
                        } else if key.starts_with("series:") {
                            if let Ok(ss) = serde_json::from_slice::<SyncedSeries>(&decrypted) {
                                let source_type = SourceType::parse(&ss.source_type).unwrap_or(SourceType::Manual);
                                let role = SeriesRole::parse(&ss.role).unwrap_or(SeriesRole::Primary);
                                let date_order = DateOrder::parse(&ss.date_order).unwrap_or(DateOrder::DayFirst);
                                let imported_at = chrono::DateTime::from_timestamp(ss.imported_at, 0).unwrap_or_default();
                                let last_api_fetch_at = ss.last_api_fetch_at.and_then(|t| chrono::DateTime::from_timestamp(t, 0));

                                let series = LoggerSeries {
                                    id: ss.id.clone(),
                                    point_id: ss.point_id.clone(),
                                    source_type,
                                    role,
                                    logger_serial: ss.logger_serial,
                                    imported_at,
                                    raw_file_reference: ss.raw_file_reference,
                                    clock_offset_seconds: ss.clock_offset_seconds,
                                    raw_timestamp_was_utc: ss.raw_timestamp_was_utc,
                                    label: ss.label,
                                    sample_interval_seconds: ss.sample_interval_seconds,
                                    date_order,
                                    date_format_uncertain: ss.date_format_uncertain,
                                    data_license: ss.data_license,
                                    station_id: ss.station_id,
                                    last_api_fetch_at,
                                    content_fingerprint: ss.content_fingerprint,
                                    measurement_count: ss.measurements.len() as i64,
                                };

                                // Un seul identifiant partagé par toutes les mesures du lot.
                                let sid: std::sync::Arc<str> = ss.id.as_str().into();
                                let measurements: Vec<Measurement> = ss
                                    .measurements
                                    .into_iter()
                                    .map(|sm| Measurement {
                                        id: 0,
                                        series_id: sid.clone(),
                                        timestamp_utc: chrono::DateTime::from_timestamp(sm.ts, 0).unwrap_or_default(),
                                        raw_timestamp: chrono::DateTime::from_timestamp(sm.raw, 0).unwrap_or_default(),
                                        temperature_celsius: sm.t,
                                        humidity_percent: sm.h,
                                        rssi_dbm: sm.rssi,
                                        battery: sm.bat,
                                        pressure_hpa: sm.p,
                                        wind_avg_kmh: sm.w,
                                        wind_gust_kmh: sm.wg,
                                        wind_direction_degrees: sm.wd,
                                        rain_mm: sm.r,
                                        solar_radiation: sm.sol,
                                    })
                                    .collect();

                                if db.insert_or_replace_series_with_measurements(&series, &measurements).is_ok() {
                                    let _ = db.set_cloud_synced_hash(&key, &hash);
                                    downloaded += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    for (cle, clair) in mois_en_attente {
        match appliquer_mois(db, &cle, &clair) {
            Ok(()) => downloaded += 1,
            Err(e) => eprintln!("[gws cloud] {cle} non appliqué : {e}"),
        }
    }

    // -----------------------------------------------------------------------
    // 4. Téléversement des données locales (projets, points, séries)
    // -----------------------------------------------------------------------
    let mut local_known_keys = HashSet::new();
    let local_projects = db.list_projects().unwrap_or_default();
    // Total = nombre de stations : bon marché à compter, et c'est l'unité qui
    // avance de façon régulière pendant l'envoi.
    let total_points: usize = local_projects.iter().map(|p| db.list_points(&p.id).map(|v| v.len()).unwrap_or(0)).sum();
    let mut points_vus = 0usize;
    let mut mois_a_envoyer: Vec<MoisAEnvoyer> = Vec::new();

    for p in local_projects {
        let p_key = format!("project:{}", p.id);
        local_known_keys.insert(p_key.clone());

        let contenu: Vec<(Point, Vec<LoggerSeries>)> = db
            .list_points(&p.id)
            .unwrap_or_default()
            .into_iter()
            .map(|pt| {
                let series = db.list_series_for_point(&pt.id).unwrap_or_default();
                (pt, series)
            })
            .collect();
        let nb_mesures: i64 = contenu.iter().flat_map(|(_, s)| s.iter()).map(|s| s.measurement_count).sum();
        if projet_exclu(db, &p.id, nb_mesures) {
            // Clés gardées « connues » : sinon l'étape 5 supprimerait du Cloud
            // (et donc des autres postes) ce qui y a été envoyé auparavant.
            for (pt, series) in &contenu {
                local_known_keys.insert(format!("point:{}", pt.id));
                for s in series {
                    local_known_keys.insert(cle_meta(&s.id));
                    for (mois, _, _) in db.series_months(&s.id).unwrap_or_default() {
                        local_known_keys.insert(cle_mois(&s.id, mois));
                    }
                }
            }
            points_vus += contenu.len();
            details.push(format!("Projet « {} » non synchronisé ({nb_mesures} mesures)", p.name));
            continue;
        }

        let synced_p = SyncedProject {
            id: p.id.clone(),
            name: p.name.clone(),
            created_at: p.created_at.timestamp(),
            last_opened_at: p.last_opened_at.timestamp(),
        };

        if let Ok(json_bytes) = serde_json::to_vec(&synced_p) {
            let hash = sha256_hex(&json_bytes);
            let already_synced = db.get_cloud_synced_hash(&p_key).ok().flatten().as_deref() == Some(&hash);

            if !already_synced {
                progres(CloudProgress {
                    phase: "envoi",
                    label: format!("Projet « {} »", p.name),
                    done: points_vus,
                    total: total_points,
                    projet: p.name.clone(),
                });
                if let Ok(enc_bytes) = encrypt_record(&data_key, &json_bytes) {
                    let put_url = format!("{}/cloud/records/{p_key}", api_base());
                    let put_res = client
                        .put(&put_url)
                        .header("Authorization", format!("Bearer {token}"))
                        .header("Content-Type", "application/octet-stream")
                        .body(enc_bytes)
                        .send();

                    if let Ok(r) = put_res {
                        if r.status().is_success() {
                            let _ = db.set_cloud_synced_hash(&p_key, &hash);
                            uploaded += 1;
                        } else {
                            eprintln!("[gws cloud] PUT {} échoué : code {}", p_key, r.status());
                        }
                    }
                }
            }
        }

        // Stations de ce projet
        for (pt, local_series) in contenu {
            points_vus += 1;
            let nom_point = pt.name.clone();
            // Chaque station est d'abord comparée au Cloud : la fenêtre dit
            // « Vérification » tant que rien n'est à envoyer (0.6.0).
            progres(CloudProgress {
                phase: "verification",
                label: format!("Station « {nom_point} »"),
                done: points_vus,
                total: total_points,
                projet: p.name.clone(),
            });
            let pt_key = format!("point:{}", pt.id);
            local_known_keys.insert(pt_key.clone());

            let synced_pt = SyncedPoint {
                id: pt.id.clone(),
                project_id: pt.project_id.clone(),
                name: pt.name.clone(),
                latitude: pt.latitude,
                longitude: pt.longitude,
                altitude_m: pt.altitude_m,
                category: pt.category.as_str().to_string(),
                size: pt.size.map(|s| s.as_str().to_string()),
                factor_sky: pt.factor_sky,
                factor_wind: pt.factor_wind,
                factor_slope: pt.factor_slope,
                factor_snow_cover: pt.factor_snow_cover,
                factor_forest_cover: pt.factor_forest_cover,
                is_open_terrain: pt.is_open_terrain,
                notes: pt.notes,
                infoclimat_station_id: pt.infoclimat_station_id,
                romma_station_id: pt.romma_station_id,
                created_at: pt.created_at.timestamp(),
            };

            if let Ok(json_bytes) = serde_json::to_vec(&synced_pt) {
                let hash = sha256_hex(&json_bytes);
                let already_synced = db.get_cloud_synced_hash(&pt_key).ok().flatten().as_deref() == Some(&hash);

                if !already_synced {
                    progres(CloudProgress {
                        phase: "envoi",
                        label: format!("Station « {nom_point} »"),
                        done: points_vus,
                        total: total_points,
                        projet: p.name.clone(),
                    });
                    if let Ok(enc_bytes) = encrypt_record(&data_key, &json_bytes) {
                        let put_url = format!("{}/cloud/records/{pt_key}", api_base());
                        let put_res = client
                            .put(&put_url)
                            .header("Authorization", format!("Bearer {token}"))
                            .header("Content-Type", "application/octet-stream")
                            .body(enc_bytes)
                            .send();

                        if let Ok(r) = put_res {
                            if r.status().is_success() {
                                let _ = db.set_cloud_synced_hash(&pt_key, &hash);
                                uploaded += 1;
                            } else {
                                eprintln!("[gws cloud] PUT {} échoué : code {}", pt_key, r.status());
                            }
                        }
                    }
                }
            }

            // Séries de cette station : métadonnées, puis un enregistrement par
            // mois (voir `cloud_mois`). Les mois à envoyer partent après la
            // boucle, en parallèle.
            for s in local_series {
                // Même clé qu'avant la 0.4.2 : l'enregistrement complet d'autrefois
                // est remplacé par les métadonnées seules.
                let meta_key = cle_meta(&s.id);
                local_known_keys.insert(meta_key.clone());

                let synced_s = SyncedSeries {
                    id: s.id.clone(),
                    point_id: s.point_id.clone(),
                    source_type: s.source_type.as_str().to_string(),
                    role: s.role.as_str().to_string(),
                    logger_serial: s.logger_serial.clone(),
                    imported_at: s.imported_at.timestamp(),
                    raw_file_reference: s.raw_file_reference.clone(),
                    clock_offset_seconds: s.clock_offset_seconds,
                    raw_timestamp_was_utc: s.raw_timestamp_was_utc,
                    label: s.label.clone(),
                    sample_interval_seconds: s.sample_interval_seconds,
                    date_order: s.date_order.as_str().to_string(),
                    date_format_uncertain: s.date_format_uncertain,
                    data_license: s.data_license.clone(),
                    station_id: s.station_id.clone(),
                    last_api_fetch_at: s.last_api_fetch_at.map(|dt| dt.timestamp()),
                    content_fingerprint: s.content_fingerprint.clone(),
                    measurements: Vec::new(),
                };
                if let Ok(json_bytes) = serde_json::to_vec(&synced_s) {
                    let hash = sha256_hex(&json_bytes);
                    if db.get_cloud_synced_hash(&meta_key).ok().flatten().as_deref() != Some(hash.as_str()) {
                        progres(CloudProgress {
                            phase: "envoi",
                            label: format!("Série « {nom_point} »"),
                            done: points_vus,
                            total: total_points,
                            projet: p.name.clone(),
                        });
                        if envoyer(&client, &token, &data_key, &meta_key, &json_bytes) {
                            let _ = db.set_cloud_synced_hash(&meta_key, &hash);
                            uploaded += 1;
                        }
                    }
                }

                for (mois, n, version) in db.series_months(&s.id).unwrap_or_default() {
                    // Mois vidé : sa clé n'est plus « connue », l'étape 5 le retire du Cloud.
                    if n <= 0 {
                        continue;
                    }
                    let cle = cle_mois(&s.id, mois);
                    local_known_keys.insert(cle.clone());
                    let deja = db.get_cloud_synced_hash(&cle).ok().flatten().as_deref().and_then(lire_repere);
                    if deja.map(|r| r.0) != Some(version) {
                        mois_a_envoyer.push(MoisAEnvoyer {
                            cle,
                            serie: s.id.clone(),
                            mois,
                            version,
                            label: format!("« {nom_point} », {}", nom_mois(mois)),
                        });
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // 4 bis. Mois de mesures : lus un par un (jamais une série entière en
    // mémoire), chiffrés et envoyés par ENVOIS_PARALLELES fils. Chaque mois
    // envoyé est noté aussitôt : une coupure reprend là où elle s'est arrêtée.
    // -----------------------------------------------------------------------
    uploaded += envoyer_mois(db, &client, &token, &data_key, &mois_a_envoyer, progres);

    // -----------------------------------------------------------------------
    // 5. Propagation des suppressions locales vers le Cloud
    // -----------------------------------------------------------------------
    let previously_synced_keys = db.list_cloud_synced_keys().unwrap_or_default();
    for prev_key in previously_synced_keys {
        if !local_known_keys.contains(&prev_key) {
            progres(CloudProgress { phase: "suppression", label: libelle_cle(&prev_key), done: 0, total: 0, ..Default::default() });
            let del_url = format!("{}/cloud/records/{prev_key}", api_base());
            let _ = client
                .delete(&del_url)
                .header("Authorization", format!("Bearer {token}"))
                .send();
            let _ = db.delete_cloud_synced_hash(&prev_key);
            deleted += 1;
        }
    }

    // -----------------------------------------------------------------------
    // 6. Mise à jour de la dernière séquence de synchronisation
    // -----------------------------------------------------------------------
    let final_seq = if uploaded > 0 || deleted > 0 {
        let ch_res = client
            .get(format!("{}/cloud/changes?since={latest_seq}", api_base()))
            .header("Authorization", format!("Bearer {token}"))
            .send();
        if let Ok(mut r) = ch_res {
            let mut b = Vec::new();
            if (&mut r).take(2 * 1024 * 1024).read_to_end(&mut b).is_ok() {
                if let Ok(ch) = serde_json::from_slice::<ChangesResponse>(&b) {
                    ch.seq
                } else {
                    latest_seq
                }
            } else {
                latest_seq
            }
        } else {
            latest_seq
        }
    } else {
        latest_seq
    };

    let now_ts = Utc::now().timestamp();
    db.set_setting(SETTING_CLOUD_LAST_SYNC_AT, &now_ts.to_string())?;
    db.set_setting(SETTING_CLOUD_LAST_SYNC_SEQ, &final_seq.to_string())?;

    details.push(format!("{uploaded} élément(s) envoyé(s)"));
    details.push(format!("{downloaded} élément(s) reçu(s)"));

    Ok(CloudSyncResult {
        uploaded,
        downloaded,
        deleted,
        details,
    })
}

// ---------------------------------------------------------------------------
// Tests unitaires
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn point_test(project_id: &str) -> Point {
        Point {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: project_id.to_string(),
            name: "Darbounouse".into(),
            latitude: 44.98,
            longitude: 5.42,
            altitude_m: Some(1300.0),
            category: Category::Cat1,
            size: None,
            factor_sky: None,
            factor_wind: None,
            factor_slope: None,
            factor_snow_cover: None,
            factor_forest_cover: None,
            is_open_terrain: None,
            notes: None,
            infoclimat_station_id: None,
            romma_station_id: None,
            created_at: Utc::now(),
            series_count: 0,
        }
    }

    fn mesure_test(serie: &str, ts: i64, t: f64) -> Measurement {
        Measurement {
            id: 0,
            series_id: serie.into(),
            timestamp_utc: chrono::DateTime::from_timestamp(ts, 0).unwrap(),
            raw_timestamp: chrono::DateTime::from_timestamp(ts, 0).unwrap(),
            temperature_celsius: t,
            humidity_percent: Some(80.0),
            rssi_dbm: None,
            battery: None,
            pressure_hpa: None,
            wind_avg_kmh: None,
            wind_gust_kmh: None,
            wind_direction_degrees: None,
            rain_mm: None,
            solar_radiation: None,
        }
    }

    /// Deux postes, un serveur : copie locale de `gws_share.py` (voir
    /// `scripts/tests/serveur_cloud_local.py`, qui fixe les variables).
    #[test]
    #[ignore = "demande une copie locale du serveur gws-share"]
    fn synchro_par_mois_entre_deux_postes() {
        let compte = std::env::var("GWS_CLOUD_TEST_ACCOUNT").expect("GWS_CLOUD_TEST_ACCOUNT");
        let invite = std::env::var("GWS_CLOUD_TEST_INVITE").expect("GWS_CLOUD_TEST_INVITE");
        let mdp = "motdepasse-test";
        let a = Db::open_in_memory().unwrap();
        let b = Db::open_in_memory().unwrap();
        register(&compte, mdp, &invite, &a).unwrap();

        // Poste A : un projet, une station, une série sur trois mois.
        let projet = a.create_project("Vercors").unwrap();
        let pt = point_test(&projet.id);
        a.insert_point(&pt).unwrap();
        let serie = serie_depuis_sync(&SyncedSeries {
            id: uuid::Uuid::new_v4().to_string(),
            point_id: pt.id.clone(),
            source_type: SourceType::Manual.as_str().to_string(),
            role: SeriesRole::Primary.as_str().to_string(),
            logger_serial: None,
            imported_at: 1_700_000_000,
            raw_file_reference: None,
            clock_offset_seconds: 0,
            raw_timestamp_was_utc: true,
            label: Some("test".into()),
            sample_interval_seconds: Some(600),
            date_order: DateOrder::DayFirst.as_str().to_string(),
            date_format_uncertain: false,
            data_license: None,
            station_id: None,
            last_api_fetch_at: None,
            content_fingerprint: None,
            measurements: Vec::new(),
        });
        let jan = 1_704_067_200; // 2024-01-01
        let mesures: Vec<_> = [jan, jan + 600, jan + 31 * 86400, jan + 60 * 86400]
            .iter()
            .enumerate()
            .map(|(i, &ts)| mesure_test(&serie.id, ts, i as f64))
            .collect();
        a.insert_or_replace_series_with_measurements(&serie, &mesures).unwrap();

        let r = sync(&a).unwrap();
        assert_eq!(r.uploaded, 6, "projet + station + série + 3 mois : {r:?}");
        assert_eq!(sync(&a).unwrap().uploaded, 0, "rien n'a changé");

        // Une mesure de plus en février : seul février repart.
        a.insert_or_replace_series_with_measurements(&serie, &[mesure_test(&serie.id, jan + 32 * 86400, 9.0)])
            .unwrap();
        assert_eq!(sync(&a).unwrap().uploaded, 1);

        // Poste B : reçoit tout, à l'identique, et ne renvoie rien.
        login(&compte, mdp, &b).unwrap();
        sync(&b).unwrap();
        let lire = |db: &Db| {
            db.measurements_for_series(&serie.id)
                .unwrap()
                .iter()
                .map(|m| (m.timestamp_utc.timestamp(), m.temperature_celsius, m.humidity_percent))
                .collect::<Vec<_>>()
        };
        assert_eq!(lire(&b), lire(&a));
        assert_eq!(b.list_series_for_point(&pt.id).unwrap()[0].label.as_deref(), Some("test"));
        assert_eq!(sync(&b).unwrap().uploaded, 0, "pas d'écho de ce qui vient d'être reçu");

        // A vide mars : B le vide aussi.
        a.replace_month(&serie.id, 202403, &[]).unwrap();
        sync(&a).unwrap();
        sync(&b).unwrap();
        assert_eq!(lire(&b), lire(&a));
        assert_eq!(lire(&b).len(), 4);

        // A supprime la série : B aussi.
        a.delete_series(&serie.id).unwrap();
        sync(&a).unwrap();
        sync(&b).unwrap();
        assert!(b.list_series_for_point(&pt.id).unwrap().is_empty());
    }

    #[test]
    fn exclusion_de_projet_reglage_puis_seuil() {
        let db = Db::open_in_memory().unwrap();
        // Automatique : seul le seuil décide.
        assert!(!projet_exclu(&db, "p", SEUIL_EXCLUSION_AUTO));
        assert!(projet_exclu(&db, "p", SEUIL_EXCLUSION_AUTO + 1));
        // Réglage explicite : l'emporte sur le seuil, dans les deux sens.
        db.set_setting(&format!("{SETTING_CLOUD_EXCLUDE_PREFIX}p"), "0").unwrap();
        assert!(!projet_exclu(&db, "p", i64::MAX));
        db.set_setting(&format!("{SETTING_CLOUD_EXCLUDE_PREFIX}p"), "1").unwrap();
        assert!(projet_exclu(&db, "p", 0));
        assert!(!projet_exclu(&db, "autre", 0));
    }

    #[test]
    fn libelles_sans_identifiant() {
        assert_eq!(libelle_cle("series:0f8b1c9e-0000-4000-8000-000000000000"), "Série de mesures");
        assert_eq!(libelle_cle("bizarre"), "Élément");
    }

    #[test]
    fn validation_identifiant_et_mot_de_passe() {
        assert!(is_valid_account("theo"));
        assert!(is_valid_account("gastinel"));
        assert!(!is_valid_account("th")); // trop court (< 3)
        assert!(!is_valid_account("Theo")); // majuscules interdites
        assert!(!is_valid_account("theo_12")); // chiffres et underscores interdits

        assert!(is_valid_password("12345678")); // 8 car.
        assert!(is_valid_password("mon_mot_de_passe_secret_30car")); // 30 car.
        assert!(!is_valid_password("court")); // < 8 car.
        assert!(!is_valid_password("mot_de_passe_qui_depasse_les_trente_caracteres_limite"));
    }

    #[test]
    fn derivation_argon2id_stable_et_distincte() {
        let salt = [42u8; 16];
        let (auth1, kek1) = derive_auth_and_kek("motdepasse123", &salt).unwrap();
        let (auth2, kek2) = derive_auth_and_kek("motdepasse123", &salt).unwrap();

        assert_eq!(auth1, auth2);
        assert_eq!(kek1, kek2);
        assert_ne!(auth1, kek1); // auth_key et kek doivent être étanches
    }

    #[test]
    fn chiffrement_dechiffrement_aes_256_gcm() {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);

        let data = b"Donnees secretes de Darbounouse -34.6 C";
        let encrypted = encrypt_aes_gcm(&key, data).unwrap();
        assert_ne!(encrypted, data);
        assert!(encrypted.len() >= data.len() + 12 + 16);

        let decrypted = decrypt_aes_gcm(&key, &encrypted).unwrap();
        assert_eq!(decrypted, data);

        // Mauvaise clé -> refus
        let mut bad_key = key;
        bad_key[0] ^= 1;
        assert!(decrypt_aes_gcm(&bad_key, &encrypted).is_err());
    }

    #[test]
    fn emballage_cle_donnees() {
        let mut kek = [0u8; 32];
        let mut data_key = [0u8; 32];
        OsRng.fill_bytes(&mut kek);
        OsRng.fill_bytes(&mut data_key);

        let wrapped = encrypt_aes_gcm(&kek, &data_key).unwrap();
        let unwrapped = decrypt_aes_gcm(&kek, &wrapped).unwrap();
        assert_eq!(unwrapped, data_key);
    }

    #[test]
    fn test_record_encryption_with_magic_and_compression() {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);

        let json = br#"{"id":"test-proj","name":"TAF Vercors","created_at":123456789,"last_opened_at":123456789}"#;
        let enc = encrypt_record(&key, json).unwrap();
        assert_eq!(&enc[..4], CLOUD_MAGIC);
        assert!(enc.len() >= 4 + 24 + 16);

        let dec = decrypt_record(&key, &enc).unwrap();
        assert_eq!(dec, json);
    }
    // --- Réception paginée contre un faux serveur local ------------------------

    /// Faux serveur `gws-share` minimal : `/cloud/changes` découpé en pages de
    /// `par_page` enregistrements, `/cloud/records/<clé>` rendu tel quel ; les
    /// envois et suppressions sont acceptés sans rien changer au journal.
    fn faux_serveur(journal: Vec<(String, bool, Vec<u8>)>, par_page: usize) -> String {
        use std::io::{BufRead, BufReader, Write};
        let ecoute = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let adresse = format!("http://{}", ecoute.local_addr().unwrap());
        std::thread::spawn(move || {
            for flux in ecoute.incoming() {
                let Ok(flux) = flux else { break };
                let mut lecteur = BufReader::new(flux.try_clone().unwrap());
                let mut ligne = String::new();
                if lecteur.read_line(&mut ligne).is_err() {
                    continue;
                }
                let mut longueur = 0usize;
                loop {
                    let mut h = String::new();
                    if lecteur.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        longueur = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut corps = vec![0u8; longueur];
                let _ = lecteur.read_exact(&mut corps);
                let mut morceaux = ligne.split_whitespace();
                let (methode, chemin) = (morceaux.next().unwrap_or(""), morceaux.next().unwrap_or(""));
                let dernier = journal.len() as i64;
                let (statut, reponse): (&str, Vec<u8>) = if methode == "GET" && chemin.starts_with("/cloud/changes") {
                    let depuis: i64 = chemin.rsplit('=').next().and_then(|s| s.parse().ok()).unwrap_or(0);
                    let page: Vec<_> = journal
                        .iter()
                        .enumerate()
                        .map(|(i, r)| (i as i64 + 1, r))
                        .filter(|(seq, _)| *seq > depuis)
                        .take(par_page)
                        .map(|(seq, (cle, suppr, _))| serde_json::json!({"key": cle, "seq": seq, "deleted": suppr, "size": 0}))
                        .collect();
                    let fin = depuis + page.len() as i64;
                    let r = serde_json::json!({"seq": dernier, "more": fin < dernier, "records": page});
                    ("200 OK", serde_json::to_vec(&r).unwrap())
                } else if methode == "GET" {
                    let cle = chemin.trim_start_matches("/cloud/records/");
                    match journal.iter().rev().find(|(c, s, _)| c == cle && !s) {
                        Some((_, _, o)) => ("200 OK", o.clone()),
                        None => ("404 Not Found", Vec::new()),
                    }
                } else {
                    ("200 OK", b"{}".to_vec())
                };
                let mut f = flux;
                let _ = write!(f, "HTTP/1.1 {statut}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reponse.len());
                let _ = f.write_all(&reponse);
            }
        });
        adresse
    }

    /// État observable de la base après synchro, trié pour comparaison.
    fn etat_final(db: &Db) -> Vec<String> {
        let mut e = Vec::new();
        for p in db.list_projects().unwrap() {
            e.push(format!("projet {} {}", p.id, p.name));
            for pt in db.list_points(&p.id).unwrap() {
                e.push(format!("station {} {} {}", pt.id, pt.project_id, pt.name));
                for s in db.list_series_for_point(&pt.id).unwrap() {
                    e.push(format!("serie {} {:?}", s.id, s.label));
                    for m in db.measurements_for_series(&s.id).unwrap() {
                        e.push(format!("mesure {} {} {}", s.id, m.timestamp_utc.timestamp(), m.temperature_celsius));
                    }
                }
            }
        }
        for k in db.list_cloud_synced_keys().unwrap() {
            let h = db.get_cloud_synced_hash(&k).unwrap().unwrap_or_default();
            // Le repère d'un mois porte la version locale : seule sa présence compte.
            e.push(format!("repere {k} {}", if h.contains('|') { "mois" } else { h.as_str() }));
        }
        e.push(format!("seq {:?}", db.get_setting(SETTING_CLOUD_LAST_SYNC_SEQ).unwrap()));
        e.sort();
        e
    }

    /// Même journal distant, découpé en pages de tailles différentes : l'état
    /// final de la base ne dépend pas du découpage (ni de la façon dont les
    /// pages sont lues), et reste celui attendu.
    #[test]
    fn reception_paginee_meme_etat_final_quelle_que_soit_la_taille_des_pages() {
        let cle = [7u8; 32];
        let chiffre = |v: &[u8]| encrypt_record(&cle, v).unwrap();


        let jan = 1_704_067_200; // 2024-01-01
        let projet = SyncedProject { id: "p-distant".into(), name: "Distant".into(), created_at: jan, last_opened_at: jan };
        let mut station = point_test("p-distant");
        station.id = "x-distant".into();
        let synced_station = SyncedPoint {
            id: station.id.clone(),
            project_id: station.project_id.clone(),
            name: "Station distante".into(),
            latitude: 45.0,
            longitude: 5.5,
            altitude_m: Some(1200.0),
            category: "Cat2".into(),
            size: None,
            factor_sky: None,
            factor_wind: None,
            factor_slope: None,
            factor_snow_cover: None,
            factor_forest_cover: None,
            is_open_terrain: None,
            notes: None,
            infoclimat_station_id: None,
            romma_station_id: None,
            created_at: jan,
        };
        let serie = SyncedSeries {
            id: "s-distante".into(),
            point_id: station.id.clone(),
            source_type: SourceType::Manual.as_str().to_string(),
            role: SeriesRole::Primary.as_str().to_string(),
            logger_serial: None,
            imported_at: jan,
            raw_file_reference: None,
            clock_offset_seconds: 0,
            raw_timestamp_was_utc: true,
            label: Some("distante".into()),
            sample_interval_seconds: Some(600),
            date_order: DateOrder::DayFirst.as_str().to_string(),
            date_format_uncertain: false,
            data_license: None,
            station_id: None,
            last_api_fetch_at: None,
            content_fingerprint: None,
            measurements: Vec::new(),
        };
        let janvier = crate::cloud_mois::encoder(
            &serie.id,
            202401,
            &[mesure_test(&serie.id, jan, 1.5), mesure_test(&serie.id, jan + 600, 2.5)],
        );
        let fevrier = crate::cloud_mois::encoder(&serie.id, 202402, &[mesure_test(&serie.id, jan + 32 * 86400, -3.0)]);

        // Le mois de janvier arrive avant les métadonnées de sa série (autre
        // page) : il doit être rejoué en fin de réception.
        let journal = vec![
            (cle_mois(&serie.id, 202401), false, chiffre(&janvier)),
            ("project:p-distant".to_string(), false, chiffre(&serde_json::to_vec(&projet).unwrap())),
            ("point:d-local".to_string(), true, Vec::new()),
            ("point:x-distant".to_string(), false, chiffre(&serde_json::to_vec(&synced_station).unwrap())),
            ("project:q-local".to_string(), true, Vec::new()),
            (cle_meta(&serie.id), false, chiffre(&serde_json::to_vec(&serie).unwrap())),
            (cle_mois(&serie.id, 202402), false, chiffre(&fevrier)),
        ];

        let mut etats = Vec::new();
        for par_page in [1usize, 2, 3, 100] {
            let db = Db::open_in_memory().unwrap();
            db.set_setting(SETTING_CLOUD_ACCOUNT, "compte-test").unwrap();
            db.set_setting(SETTING_CLOUD_TOKEN, "jeton-test").unwrap();
            db.set_setting(SETTING_CLOUD_DATA_KEY, &BASE64_STANDARD.encode(cle)).unwrap();
            // Local : un projet avec une station supprimée ailleurs, et un projet
            // vide supprimé ailleurs.
            db.create_project_with_id("l-local", "Local", jan, jan).unwrap();
            let mut d = point_test("l-local");
            d.id = "d-local".into();
            db.insert_point(&d).unwrap();
            db.set_cloud_synced_hash("point:d-local", "ancien").unwrap();
            db.create_project_with_id("q-local", "Vide", jan, jan).unwrap();
            db.set_cloud_synced_hash("project:q-local", "ancien").unwrap();

            std::env::set_var("GWS_CLOUD_TEST_API", faux_serveur(journal.clone(), par_page));
            let r = sync(&db).unwrap();
            assert_eq!(r.downloaded, 5, "projet + station + série + 2 mois ({par_page}/page) : {r:?}");
            etats.push(etat_final(&db));
        }
        std::env::remove_var("GWS_CLOUD_TEST_API");

        let e = &etats[0];
        assert!(e.iter().any(|l| l == "projet p-distant Distant"));
        assert!(e.iter().any(|l| l.starts_with("station x-distant p-distant")));
        assert!(!e.iter().any(|l| l.contains("d-local")), "station supprimée ailleurs : {e:?}");
        assert!(!e.iter().any(|l| l.contains("q-local")), "projet supprimé ailleurs : {e:?}");
        assert_eq!(e.iter().filter(|l| l.starts_with("mesure s-distante")).count(), 3);
        for (i, autre) in etats.iter().enumerate().skip(1) {
            assert_eq!(autre, e, "découpage n° {i} différent");
        }
    }
}

//! Relai SSH pour les appels API Infoclimat via serveur personnel (Debian).
//!
//! ## Pourquoi ce module existe
//!
//! L'API Open Data Infoclimat lie chaque clé à l'adresse IP publique déclarée
//! lors de sa création. Pour un utilisateur dont la box Internet a une IP
//! dynamique qui change, les appels échouent fréquemment.
//!
//! Ce module permet de faire transiter les appels Infoclimat par un serveur
//! personnel (par exemple un serveur Debian ayant une IP fixe ou déclarée)
//! via SSH, sans changer le fonctionnement normal de l'application lorsqu'il
//! est désactivé.
//!
//! ## Choix technique : exécution distante `curl` via `russh`
//!
//! L'approche retenue est l'exécution distante d'une commande `curl` via
//! une bibliothèque pure Rust (`russh`) plutôt qu'un tunnel local :
//!
//! * **Aucun problème de certificat TLS / SNI** : un tunnel TCP local vers
//!   `127.0.0.1` entraînerait des erreurs de validation de certificat
//!   (le certificat servi par Infoclimat est pour `www.infoclimat.fr`).
//! * **Aucun état de socket persistant** : chaque requête est exécutée de façon
//!   indépendante et résiliente, sans conflit de port local.
//! * **Pure Rust** : `russh` ne nécessite aucun compilateur C ni OpenSSL externe
//!   sous Windows et gère nativement les clés Ed25519, RSA, et les ciphers modernes
//!   (ChaCha20-Poly1305, Curve25519) utilisés par Debian 13.
//!
//! ## Sécurité des identifiants
//!
//! Les identifiants (mot de passe ou phrase secrète) sont stockés localement dans
//! la base de données SQLite de l'application (`app_settings`), qui réside dans le
//! dossier AppData de l'utilisateur (jamais versionné).
//! Le frontend n'a **jamais** accès aux secrets en clair : [`SshRelayStatus`] ne
//! transmet que des drapeaux booléens (`has_password`, `has_passphrase`).
//!
//! Un secret stocké n'est réutilisé que pour **l'hôte, le port et l'utilisateur
//! pour lesquels il a été enregistré** : changer l'adresse dans le formulaire
//! oblige à ressaisir le mot de passe. Sans cette règle, un simple appel IPC
//! désignant un autre serveur lui aurait fait envoyer le mot de passe enregistré
//! (audit de sécurité du 18/09, §2).
//!
//! ## Clé d'hôte épinglée
//!
//! Le client vérifie l'identité du serveur **avant** toute authentification. À la
//! première connexion, rien n'est envoyé : l'empreinte présentée par le serveur
//! remonte à l'UI, qui demande à l'utilisateur de la comparer avec celle du
//! serveur (`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub`) et de la
//! confirmer. Ensuite, toute autre empreinte est refusée — c'est le signe d'une
//! interception, ou d'un serveur réinstallé. Auparavant n'importe quelle clé était
//! acceptée, ce qui livrait le mot de passe et le token Infoclimat à quiconque se
//! plaçait sur le chemin réseau (audit du 18/09, §1).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::infoclimat::{erreur_authentification, ressemble_a_un_refus_d_auth, InfoclimatError};

pub const SETTING_SSH_HOST: &str = "ssh_relay_host";
pub const SETTING_SSH_PORT: &str = "ssh_relay_port";
pub const SETTING_SSH_USER: &str = "ssh_relay_user";
pub const SETTING_SSH_AUTH_TYPE: &str = "ssh_relay_auth_type";
pub const SETTING_SSH_KEY_PATH: &str = "ssh_relay_key_path";
pub const SETTING_SSH_KEY_PASSPHRASE: &str = "ssh_relay_key_passphrase";
pub const SETTING_SSH_PASSWORD: &str = "ssh_relay_password";
pub const SETTING_SSH_ENABLED: &str = "ssh_relay_enabled";
/// Empreinte SHA-256 de la clé d'hôte acceptée (`SHA256:…`).
pub const SETTING_SSH_HOST_KEY: &str = "ssh_relay_host_key";
/// `hôte:port` auquel cette empreinte s'applique : elle ne vaut pour aucun autre.
pub const SETTING_SSH_HOST_KEY_FOR: &str = "ssh_relay_host_key_for";

pub const DEFAULT_SSH_PORT: u16 = 22;
pub const SSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const SSH_CMD_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshAuthType {
    Password,
    Key,
}

impl SshAuthType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SshAuthType::Password => "password",
            SshAuthType::Key => "key",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "password" => SshAuthType::Password,
            _ => SshAuthType::Key,
        }
    }
}

#[derive(Debug, Clone)]
pub enum SshAuth {
    Password(String),
    Key {
        path: PathBuf,
        passphrase: Option<String>,
    },
}

/// Configuration interne complète du relai SSH.
#[derive(Debug, Clone)]
pub struct SshRelayConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    pub enabled: bool,
    /// Empreinte de clé d'hôte attendue. `None` : serveur encore inconnu, la
    /// connexion s'arrête avant toute authentification.
    pub host_key: Option<String>,
    /// Identité de cet appareil, notée dans le journal du serveur à chaque
    /// appel Infoclimat (quota partagé, voir `quota_partage.rs`).
    pub appareil: Option<crate::quota_partage::IdentiteAppareil>,
}

/// Échec d'une connexion au relai, distingué selon ce que l'UI doit en faire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshFailure {
    /// Serveur jamais vu : l'utilisateur doit vérifier puis accepter l'empreinte.
    UnknownHostKey(String),
    /// L'empreinte a changé depuis qu'elle a été acceptée.
    ChangedHostKey { expected: String, seen: String },
    Other(String),
}

impl std::fmt::Display for SshFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SshFailure::UnknownHostKey(fp) => write!(
                f,
                "Serveur SSH pas encore reconnu (empreinte {fp}). Ouvrez Paramètres → Relai \
                 serveur (SSH), testez la connexion et confirmez l'empreinte."
            ),
            SshFailure::ChangedHostKey { expected, seen } => write!(
                f,
                "La clé du serveur SSH a changé : attendue {expected}, reçue {seen}. Connexion \
                 refusée, aucun identifiant n'a été envoyé. Si le serveur n'a pas été réinstallé, \
                 quelqu'un intercepte peut-être la connexion."
            ),
            SshFailure::Other(msg) => f.write_str(msg),
        }
    }
}

impl From<String> for SshFailure {
    fn from(msg: String) -> Self {
        SshFailure::Other(msg)
    }
}

/// Une empreinte telle que l'affiche OpenSSH : `SHA256:` suivi de 43 caractères
/// base64 sans remplissage.
pub fn is_fingerprint(s: &str) -> bool {
    s.strip_prefix("SHA256:")
        .map(|b| b.len() == 43 && b.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/'))
        .unwrap_or(false)
}

/// Empreinte acceptée pour `host:port`, s'il y en a une.
pub fn trusted_key_for(db: &Db, host: &str, port: u16) -> AppResult<Option<String>> {
    let pour = db.get_setting(SETTING_SSH_HOST_KEY_FOR)?;
    if pour.as_deref() != Some(format!("{}:{port}", host.trim()).as_str()) {
        return Ok(None);
    }
    Ok(db.get_setting(SETTING_SSH_HOST_KEY)?.filter(|k| is_fingerprint(k)))
}

/// Accepte une empreinte pour `host:port`, après vérification par l'utilisateur.
pub fn trust_host_key(db: &Db, host: &str, port: u16, fingerprint: &str) -> AppResult<SshRelayStatus> {
    let fingerprint = fingerprint.trim();
    if !is_fingerprint(fingerprint) {
        return Err(AppError::Other(format!("Empreinte invalide : {fingerprint}")));
    }
    if host.trim().is_empty() || port == 0 {
        return Err(AppError::Other("Hôte ou port SSH invalide.".into()));
    }
    db.set_setting(SETTING_SSH_HOST_KEY, fingerprint)?;
    db.set_setting(SETTING_SSH_HOST_KEY_FOR, &format!("{}:{port}", host.trim()))?;
    get_status(db)
}

/// Oublie l'empreinte acceptée (serveur réinstallé, par exemple).
pub fn forget_host_key(db: &Db) -> AppResult<SshRelayStatus> {
    db.set_setting(SETTING_SSH_HOST_KEY, "")?;
    db.set_setting(SETTING_SSH_HOST_KEY_FOR, "")?;
    get_status(db)
}

/// Données envoyées par le frontend pour mettre à jour la configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct SshRelayInput {
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    #[serde(alias = "authType")]
    pub auth_type: SshAuthType,
    #[serde(alias = "keyPath")]
    pub key_path: Option<String>,
    /// Nouveau mot de passe (si `None`, conserve le mot de passe déjà en base).
    pub password: Option<String>,
    /// Nouvelle phrase secrète (si `None`, conserve la phrase secrète déjà en base).
    #[serde(alias = "keyPassphrase", alias = "passphrase")]
    pub key_passphrase: Option<String>,
    pub enabled: Option<bool>,
}

/// Statut transmis à l'UI — **ne contient aucun mot de passe ni phrase secrète**.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshRelayStatus {
    pub configured: bool,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth_type: SshAuthType,
    pub key_path: Option<String>,
    pub has_password: bool,
    pub has_passphrase: bool,
    pub enabled: bool,
    /// Empreinte de clé d'hôte acceptée pour l'hôte et le port enregistrés
    /// (information publique, pas un secret).
    pub trusted_host_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Persistance en base SQLite (app_settings)
// ---------------------------------------------------------------------------

/// Charge la configuration complète du relai depuis la base SQLite.
pub fn load_config(db: &Db) -> AppResult<Option<SshRelayConfig>> {
    let host = match db.get_setting(SETTING_SSH_HOST)? {
        Some(h) if !h.trim().is_empty() => h.trim().to_string(),
        _ => return Ok(None),
    };

    let user = match db.get_setting(SETTING_SSH_USER)? {
        Some(u) if !u.trim().is_empty() => u.trim().to_string(),
        _ => return Ok(None),
    };

    let port: u16 = db
        .get_setting(SETTING_SSH_PORT)?
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|&p| p > 0)
        .unwrap_or(DEFAULT_SSH_PORT);

    let auth_type = db
        .get_setting(SETTING_SSH_AUTH_TYPE)?
        .as_deref()
        .map(SshAuthType::parse)
        .unwrap_or(SshAuthType::Key);

    let auth = match auth_type {
        SshAuthType::Password => {
            let pwd = db.get_setting(SETTING_SSH_PASSWORD)?.unwrap_or_default();
            SshAuth::Password(pwd)
        }
        SshAuthType::Key => {
            let key_path = db.get_setting(SETTING_SSH_KEY_PATH)?.unwrap_or_default();
            let passphrase = db
                .get_setting(SETTING_SSH_KEY_PASSPHRASE)?
                .filter(|p| !p.is_empty());
            SshAuth::Key {
                path: PathBuf::from(key_path),
                passphrase,
            }
        }
    };

    let enabled = db
        .get_setting(SETTING_SSH_ENABLED)?
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let host_key = trusted_key_for(db, &host, port)?;
    Ok(Some(SshRelayConfig {
        host,
        port,
        user,
        auth,
        enabled,
        host_key,
        appareil: crate::quota_partage::identite(db).ok(),
    }))
}

/// Renvoie le statut exposé à l'UI sans divulguer de secret.
pub fn get_status(db: &Db) -> AppResult<SshRelayStatus> {
    let host = db.get_setting(SETTING_SSH_HOST)?.unwrap_or_default();
    let user = db.get_setting(SETTING_SSH_USER)?.unwrap_or_default();
    let port: u16 = db
        .get_setting(SETTING_SSH_PORT)?
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|&p| p > 0)
        .unwrap_or(DEFAULT_SSH_PORT);

    let auth_type = db
        .get_setting(SETTING_SSH_AUTH_TYPE)?
        .as_deref()
        .map(SshAuthType::parse)
        .unwrap_or(SshAuthType::Key);

    let key_path = db.get_setting(SETTING_SSH_KEY_PATH)?;
    let has_password = db
        .get_setting(SETTING_SSH_PASSWORD)?
        .map(|p| !p.is_empty())
        .unwrap_or(false);
    let has_passphrase = db
        .get_setting(SETTING_SSH_KEY_PASSPHRASE)?
        .map(|p| !p.is_empty())
        .unwrap_or(false);
    let enabled = db
        .get_setting(SETTING_SSH_ENABLED)?
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let configured = !host.trim().is_empty()
        && !user.trim().is_empty()
        && match auth_type {
            SshAuthType::Password => has_password,
            SshAuthType::Key => key_path.as_deref().map(|p| !p.trim().is_empty()).unwrap_or(false),
        };

    let trusted_host_key = trusted_key_for(db, &host, port)?;
    Ok(SshRelayStatus {
        configured,
        host,
        port,
        user,
        auth_type,
        key_path,
        has_password,
        has_passphrase,
        enabled,
        trusted_host_key,
    })
}

/// Enregistre les réglages du relai SSH depuis la saisie utilisateur.
pub fn save_config(db: &Db, input: &SshRelayInput) -> AppResult<SshRelayStatus> {
    let host = input.host.trim();
    if host.is_empty() {
        return Err(AppError::Other("L'adresse de l'hôte SSH ne peut pas être vide.".into()));
    }
    let user = input.user.trim();
    if user.is_empty() {
        return Err(AppError::Other("Le nom d'utilisateur SSH ne peut pas être vide.".into()));
    }
    let port = input.port.unwrap_or(DEFAULT_SSH_PORT);
    if port == 0 {
        return Err(AppError::Other("Le port SSH doit être compris entre 1 et 65535.".into()));
    }

    // Le mot de passe et la phrase secrète enregistrés appartiennent à une
    // destination précise. Si elle change, les conserver les enverrait à un
    // autre serveur : on les efface et on exige une nouvelle saisie.
    let meme_destination = same_destination(db, host, port, user)?;
    if !meme_destination {
        db.set_setting(SETTING_SSH_PASSWORD, "")?;
        db.set_setting(SETTING_SSH_KEY_PASSPHRASE, "")?;
    }

    db.set_setting(SETTING_SSH_HOST, host)?;
    db.set_setting(SETTING_SSH_USER, user)?;
    db.set_setting(SETTING_SSH_PORT, &port.to_string())?;
    db.set_setting(SETTING_SSH_AUTH_TYPE, input.auth_type.as_str())?;

    match input.auth_type {
        SshAuthType::Password => {
            if let Some(ref pwd) = input.password {
                // Si l'utilisateur a tapé une nouvelle valeur
                if !pwd.is_empty() {
                    db.set_setting(SETTING_SSH_PASSWORD, pwd)?;
                }
            } else {
                // Vérifier qu'un mot de passe existait déjà (effacé plus haut
                // si la destination a changé)
                let existant = db.get_setting(SETTING_SSH_PASSWORD)?.unwrap_or_default();
                if existant.is_empty() {
                    return Err(AppError::Other(if meme_destination {
                        "Veuillez renseigner le mot de passe SSH.".into()
                    } else {
                        "Le serveur ou l'utilisateur a changé : ressaisissez le mot de passe SSH.".into()
                    }));
                }
            }
        }
        SshAuthType::Key => {
            let key_path = input.key_path.as_deref().unwrap_or("").trim();
            if key_path.is_empty() {
                return Err(AppError::Other("Veuillez indiquer le chemin vers le fichier de clé privée SSH.".into()));
            }
            if !Path::new(key_path).is_file() {
                return Err(AppError::Other(format!(
                    "Le fichier de clé privée spécifié n'existe pas : {key_path}"
                )));
            }
            db.set_setting(SETTING_SSH_KEY_PATH, key_path)?;

            if let Some(ref pass) = input.key_passphrase {
                db.set_setting(SETTING_SSH_KEY_PASSPHRASE, pass)?;
            }
        }
    }

    if let Some(enabled) = input.enabled {
        db.set_setting(SETTING_SSH_ENABLED, if enabled { "1" } else { "0" })?;
    }

    get_status(db)
}

/// `true` si `host`, `port` et `user` sont ceux déjà enregistrés — seule
/// situation où un secret stocké peut être réutilisé.
pub fn same_destination(db: &Db, host: &str, port: u16, user: &str) -> AppResult<bool> {
    let h = db.get_setting(SETTING_SSH_HOST)?.unwrap_or_default();
    let u = db.get_setting(SETTING_SSH_USER)?.unwrap_or_default();
    let p = db
        .get_setting(SETTING_SSH_PORT)?
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|&p| p > 0)
        .unwrap_or(DEFAULT_SSH_PORT);
    Ok(!h.trim().is_empty() && h.trim() == host.trim() && u.trim() == user.trim() && p == port)
}

/// Active ou désactive le relai SSH.
pub fn set_enabled(db: &Db, enabled: bool) -> AppResult<SshRelayStatus> {
    db.set_setting(SETTING_SSH_ENABLED, if enabled { "1" } else { "0" })?;
    get_status(db)
}

// ---------------------------------------------------------------------------
// Client SSH (russh) et exécution distante
// ---------------------------------------------------------------------------

struct Handler {
    /// Empreinte acceptée par l'utilisateur, `None` si le serveur est inconnu.
    expected: Option<String>,
    /// Empreinte réellement présentée, relevée pour le message d'erreur.
    seen: Arc<std::sync::Mutex<Option<String>>>,
}

impl russh::client::Handler for Handler {
    type Error = russh::Error;

    /// Appelée pendant l'échange de clés, **avant** toute authentification :
    /// refuser ici garantit qu'aucun mot de passe ni signature ne part.
    fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        let fp = server_public_key.fingerprint(russh::keys::HashAlg::Sha256).to_string();
        let ok = self.expected.as_deref() == Some(fp.as_str());
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(fp);
        async move { Ok(ok) }
    }
}

/// Établit la connexion et authentifie le client SSH.
async fn connect_and_auth(config: &SshRelayConfig) -> Result<russh::client::Handle<Handler>, SshFailure> {
    let client_config = Arc::new(russh::client::Config::default());
    let seen = Arc::new(std::sync::Mutex::new(None));
    let handler = Handler { expected: config.host_key.clone(), seen: seen.clone() };

    let addr = (config.host.as_str(), config.port);
    let connexion = tokio::time::timeout(
        SSH_CONNECT_TIMEOUT,
        russh::client::connect(client_config, addr, handler),
    )
    .await
    .map_err(|_| {
        format!(
            "Délai d'attente dépassé ({:?}) lors de la connexion SSH à {}:{}",
            SSH_CONNECT_TIMEOUT, config.host, config.port
        )
    })?;
    let mut session = match connexion {
        Ok(s) => s,
        Err(e) => {
            // Refus de la clé d'hôte : on dit laquelle a été vue, et pourquoi.
            let vue = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
            return Err(match (vue, &config.host_key) {
                (Some(vue), None) => SshFailure::UnknownHostKey(vue),
                (Some(vue), Some(attendue)) if &vue != attendue => {
                    SshFailure::ChangedHostKey { expected: attendue.clone(), seen: vue }
                }
                _ => SshFailure::Other(format!(
                    "Impossible de joindre le serveur SSH {}:{}: {e}",
                    config.host, config.port
                )),
            });
        }
    };

    match &config.auth {
        SshAuth::Password(pwd) => {
            let auth_res = session
                .authenticate_password(&config.user, pwd)
                .await
                .map_err(|e| format!("Erreur de protocole lors de l'authentification par mot de passe : {e}"))?;
            if !auth_res.success() {
                return Err(SshFailure::Other(format!(
                    "Authentification SSH refusée : mot de passe incorrect pour l'utilisateur « {} ».",
                    config.user
                )));
            }
        }
        SshAuth::Key { path, passphrase } => {
            let secret_key = russh::keys::load_secret_key(path, passphrase.as_deref()).map_err(|e| {
                format!(
                    "Impossible de charger ou déchiffrer la clé privée SSH ({}) : {e}",
                    path.display()
                )
            })?;

            // Pour RSA, on utilise Sha256 (compatibilité avec OpenSSH moderne sur Debian 13 qui désactive sha-rsa).
            // Pour les autres types de clés (Ed25519, etc.), le hash_alg est ignoré.
            let key_with_alg = russh::keys::key::PrivateKeyWithHashAlg::new(
                Arc::new(secret_key),
                Some(russh::keys::HashAlg::Sha256),
            );

            let auth_res = session
                .authenticate_publickey(&config.user, key_with_alg)
                .await
                .map_err(|e| format!("Erreur de protocole lors de l'authentification par clé publique : {e}"))?;

            if !auth_res.success() {
                return Err(SshFailure::Other(format!(
                    "Authentification SSH refusée : clé publique rejetée par le serveur pour « {} ».",
                    config.user
                )));
            }
        }
    }

    Ok(session)
}

/// Exécute une commande distante et récupère (exit_code, stdout, stderr).
async fn run_remote_command(
    config: &SshRelayConfig,
    command: &str,
) -> Result<(u32, Vec<u8>, Vec<u8>), SshFailure> {
    run_remote_command_borne(config, command, None).await
}

/// Exécute une commande distante **fixe** (écrite dans le code, jamais composée
/// avec un texte venu du réseau ou de l'utilisateur) en bornant la sortie :
/// au-delà de `limite` octets sur stdout ou stderr, la connexion est abandonnée.
/// Sert à la configuration signée du relai (`relais_config.rs`), où la taille
/// est plafonnée côté serveur par `head -c`, et ici encore au cas où le serveur
/// ne respecterait pas ce plafond.
pub(crate) async fn executer_commande_fixe_bornee(
    config: &SshRelayConfig,
    command: &'static str,
    limite: usize,
) -> Result<(u32, Vec<u8>, Vec<u8>), SshFailure> {
    run_remote_command_borne(config, command, Some(limite)).await
}

/// Exécution d'une commande distante, bornée ou non (`limite`).

async fn run_remote_command_borne(
    config: &SshRelayConfig,
    command: &str,
    limite: Option<usize>,
) -> Result<(u32, Vec<u8>, Vec<u8>), SshFailure> {
    let session = connect_and_auth(config).await?;

    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("Impossible d'ouvrir un canal de session SSH : {e}"))?;

    channel
        .exec(false, command)
        .await
        .map_err(|e| format!("Échec lors de l'envoi de la commande distante : {e}"))?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = 0u32;

    loop {
        let msg = match tokio::time::timeout(SSH_CMD_TIMEOUT, channel.wait()).await {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(_) => {
                return Err(SshFailure::Other(format!(
                    "Délai d'exécution dépassé ({:?}) sur le serveur distant.",
                    SSH_CMD_TIMEOUT
                )));
            }
        };

        match msg {
            russh::ChannelMsg::Data { data } => {
                stdout.extend_from_slice(&data);
            }
            russh::ChannelMsg::ExtendedData { data, ext: 1 } => {
                stderr.extend_from_slice(&data);
            }
            russh::ChannelMsg::ExitStatus { exit_status } => {
                exit_code = exit_status;
            }
            russh::ChannelMsg::Eof => {
                break;
            }
            _ => {}
        }

        if let Some(max) = limite {
            if stdout.len() > max || stderr.len() > max {
                let _ = session.disconnect(russh::Disconnect::ByApplication, "", "fr").await;
                return Err(SshFailure::Other(format!(
                    "Réponse du serveur distant trop volumineuse (plus de {max} octets)."
                )));
            }
        }
    }

    // Fermeture propre de la session
    let _ = session.disconnect(russh::Disconnect::ByApplication, "", "fr").await;

    Ok((exit_code, stdout, stderr))
}

/// Teste la connexion SSH et vérifie que `curl` est disponible sur le serveur.
pub async fn test_connection(config: &SshRelayConfig) -> Result<String, SshFailure> {
    let (code, stdout, stderr) = run_remote_command(config, "which curl && curl --version").await?;

    if code != 0 {
        let err_text = String::from_utf8_lossy(&stderr);
        let out_text = String::from_utf8_lossy(&stdout);
        return Err(SshFailure::Other(format!(
            "Connexion SSH réussie, mais 'curl' est absent ou a échoué (code {}) :\n{}{}",
            code, out_text, err_text
        )));
    }

    let version_line = String::from_utf8_lossy(&stdout)
        .lines()
        .find(|l| l.contains("curl"))
        .unwrap_or("curl présent")
        .to_string();

    Ok(format!(
        "Connexion SSH réussie à {}@{} (port {}) !\n{}",
        config.user, config.host, config.port, version_line
    ))
}

/// Échappe une chaîne pour insertion sûre dans un argument de commande shell entre apostrophes simples `'...'`.
pub fn escape_shell_arg(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Découpe la sortie de `curl` formatée avec `-w "\n__HTTP_STATUS__:%{http_code}"`.
pub fn parse_curl_output(raw_output: &str) -> Result<(u16, String), String> {
    const MARKER: &str = "__HTTP_STATUS__:";
    if let Some(pos) = raw_output.rfind(MARKER) {
        let body = &raw_output[..pos];
        let status_str = raw_output[pos + MARKER.len()..].trim();
        let status: u16 = status_str.parse().map_err(|e| {
            format!("Code HTTP invalide renvoyé par curl ({status_str}) : {e}")
        })?;
        // Retire le dernier saut de ligne éventuel avant le marqueur
        let body = body.strip_suffix('\n').unwrap_or(body);
        let body = body.strip_suffix('\r').unwrap_or(body);
        Ok((status, body.to_string()))
    } else {
        Err("Réponse curl inattendue : marqueur de statut HTTP manquant.".into())
    }
}

/// Exécute une requête Infoclimat à travers le relai SSH via `curl` sur le serveur distant.
pub async fn execute_infoclimat_fetch(
    config: &SshRelayConfig,
    url: &str,
    token: &str,
) -> Result<String, InfoclimatError> {
    let escaped_url = escape_shell_arg(url);
    let prefixe = config.appareil.as_ref().and_then(crate::quota_partage::prefixe_journal).unwrap_or_default();
    let cmd = format!("{prefixe}curl -s -S -w \"\\n__HTTP_STATUS__:%{{http_code}}\" -m 60 {escaped_url}");

    let (code, stdout_bytes, stderr_bytes) = run_remote_command(config, &cmd)
        .await
        .map_err(|e| InfoclimatError::SshRelay(e.to_string()))?;

    if code != 0 {
        let err_text = String::from_utf8_lossy(&stderr_bytes);
        let out_text = String::from_utf8_lossy(&stdout_bytes);
        let err_complet = format!("{err_text}\n{out_text}").trim().to_string();
        let masque = crate::infoclimat_client::scrub(&err_complet, token);
        return Err(InfoclimatError::SshRelay(format!(
            "Échec de curl sur le serveur distant (code {code}) : {masque}"
        )));
    }

    let brut = String::from_utf8(stdout_bytes)
        .map_err(|e| InfoclimatError::Json(format!("Réponse du relai SSH non-UTF8 : {e}")))?;

    let (status, corps) = parse_curl_output(&brut)
        .map_err(|e| InfoclimatError::SshRelay(e))?;

    let visible = crate::infoclimat_client::scrub(&corps, token);

    // 401 / 403 : refus d'authentification
    if status == 401 || status == 403 {
        return Err(erreur_authentification(&visible));
    }

    // Refus d'auth Infoclimat classique (ex: 200 + "Wrong ip address")
    if ressemble_a_un_refus_d_auth(&corps) {
        return Err(erreur_authentification(&visible));
    }

    if status < 200 || status >= 300 {
        return Err(InfoclimatError::Http {
            status,
            body: visible.chars().take(300).collect(),
        });
    }

    Ok(corps)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "SHA256:rj8ceBN/oRM3cJJztn/x+BeovzPYINDGQW3nnl+0+2c";

    #[test]
    fn format_des_empreintes() {
        assert!(is_fingerprint(FP));
        assert!(!is_fingerprint("SHA256:court"));
        assert!(!is_fingerprint("MD5:rj8ceBN/oRM3cJJztn/x+BeovzPYINDGQW3nnl+0+2c"));
        assert!(!is_fingerprint(&format!("{FP} ")));
    }

    #[test]
    fn une_empreinte_ne_vaut_que_pour_son_hote_et_son_port() {
        let db = Db::open_in_memory().unwrap();
        trust_host_key(&db, "192.0.2.1", 22, FP).unwrap();
        assert_eq!(trusted_key_for(&db, "192.0.2.1", 22).unwrap().as_deref(), Some(FP));
        assert_eq!(trusted_key_for(&db, "192.0.2.1", 2222).unwrap(), None);
        assert_eq!(trusted_key_for(&db, "attaquant.example", 22).unwrap(), None);
        assert!(trust_host_key(&db, "h", 22, "pas une empreinte").is_err());
        forget_host_key(&db).unwrap();
        assert_eq!(trusted_key_for(&db, "192.0.2.1", 22).unwrap(), None);
    }

    fn saisie(host: &str, user: &str, password: Option<&str>) -> SshRelayInput {
        SshRelayInput {
            host: host.into(),
            port: Some(22),
            user: user.into(),
            auth_type: SshAuthType::Password,
            key_path: None,
            password: password.map(Into::into),
            key_passphrase: None,
            enabled: Some(true),
        }
    }

    #[test]
    fn changer_de_serveur_oblige_a_ressaisir_le_mot_de_passe() {
        let db = Db::open_in_memory().unwrap();
        save_config(&db, &saisie("vps.example", "debian", Some("secret"))).unwrap();
        assert!(same_destination(&db, "vps.example", 22, "debian").unwrap());

        // Même destination, mot de passe non ressaisi : conservé.
        save_config(&db, &saisie("vps.example", "debian", None)).unwrap();
        assert_eq!(db.get_setting(SETTING_SSH_PASSWORD).unwrap().as_deref(), Some("secret"));

        // Autre serveur sans nouveau mot de passe : refusé, et l'ancien effacé.
        let err = save_config(&db, &saisie("attaquant.example", "debian", None)).unwrap_err();
        assert!(err.to_string().contains("ressaisissez"));
        assert_eq!(db.get_setting(SETTING_SSH_PASSWORD).unwrap().as_deref(), Some(""));
    }

    #[test]
    fn echappement_url_shell_protege_les_apostrophes() {
        let url = "https://example.com/api?a=1&b=2'or'1'='1";
        let escaped = escape_shell_arg(url);
        assert_eq!(escaped, "'https://example.com/api?a=1&b=2'\\''or'\\''1'\\''='\\''1'");
    }

    #[test]
    fn parse_curl_output_separe_corps_et_code() {
        let raw = "{\"status\":\"OK\",\"data\":[1,2,3]}\n__HTTP_STATUS__:200\n";
        let (status, body) = parse_curl_output(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "{\"status\":\"OK\",\"data\":[1,2,3]}");
    }

    #[test]
    fn parse_curl_output_detecte_erreurs() {
        let sans_marqueur = "Texte sans marqueur";
        assert!(parse_curl_output(sans_marqueur).is_err());

        let marqueur_invalide = "corps\n__HTTP_STATUS__:pas_un_nombre";
        assert!(parse_curl_output(marqueur_invalide).is_err());
    }

    #[test]
    fn validation_champs_requis_a_l_enregistrement() {
        let db = Db::open_in_memory().unwrap();

        let input_vide = SshRelayInput {
            host: "".into(),
            port: Some(22),
            user: "debian".into(),
            auth_type: SshAuthType::Key,
            key_path: Some("C:\\nonexistent".into()),
            password: None,
            key_passphrase: None,
            enabled: Some(false),
        };
        assert!(save_config(&db, &input_vide).is_err());

        let input_port_invalide = SshRelayInput {
            host: "1.2.3.4".into(),
            port: Some(0),
            user: "debian".into(),
            auth_type: SshAuthType::Key,
            key_path: Some("C:\\nonexistent".into()),
            password: None,
            key_passphrase: None,
            enabled: Some(false),
        };
        assert!(save_config(&db, &input_port_invalide).is_err());
    }

    #[test]
    fn persistance_et_statut_sans_fuite_de_secrets() {
        let db = Db::open_in_memory().unwrap();

        // Clé privée temporaire
        let temp_dir = std::env::temp_dir().join("gws-ssh-test");
        let _ = std::fs::create_dir_all(&temp_dir);
        let temp_key = temp_dir.join("id_test");
        std::fs::write(&temp_key, "dummy key content").unwrap();

        let input = SshRelayInput {
            host: "192.0.2.1".into(),
            port: Some(22),
            user: "debian".into(),
            auth_type: SshAuthType::Key,
            key_path: Some(temp_key.to_string_lossy().to_string()),
            password: None,
            key_passphrase: Some("mon_secret_ultra_confidentiel".into()),
            enabled: Some(true),
        };

        let status = save_config(&db, &input).unwrap();
        assert!(status.configured);
        assert!(status.enabled);
        assert!(status.has_passphrase);
        assert!(!status.has_password);
        assert_eq!(status.host, "192.0.2.1");
        assert_eq!(status.user, "debian");

        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("mon_secret_ultra_confidentiel"));

        // Test bascule activé / désactivé
        let status_desactive = set_enabled(&db, false).unwrap();
        assert!(!status_desactive.enabled);
        assert!(status_desactive.configured);
    }

    /// Test réseau réel contre un serveur SSH distant — `#[ignore]` par défaut.
    ///
    /// Lit les paramètres de connexion depuis les variables d'environnement suivantes :
    /// - `GWS_SSH_HOST` (ex: `192.0.2.1` ou `ssh.mondomaine.fr`)
    /// - `GWS_SSH_PORT` (optionnel, défaut `22`)
    /// - `GWS_SSH_USER` (optionnel, défaut `debian`)
    /// - `GWS_SSH_KEY_PATH` (ex: `C:\Users\nom\.ssh\id_ed25519`)
    /// - `GWS_SSH_PASSPHRASE` (optionnel)
    ///
    /// Pour l'exécuter :
    /// `$env:GWS_SSH_HOST="..."; $env:GWS_SSH_KEY_PATH="..."; cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture test_reel_ssh_relay`
    #[tokio::test]
    #[ignore = "appelle le serveur SSH distant réel"]
    async fn test_reel_ssh_relay() {
        let host = match std::env::var("GWS_SSH_HOST") {
            Ok(h) if !h.trim().is_empty() => h.trim().to_string(),
            _ => {
                eprintln!("Variable GWS_SSH_HOST non définie, test réel ignoré.");
                return;
            }
        };

        let key_path_str = match std::env::var("GWS_SSH_KEY_PATH") {
            Ok(p) if !p.trim().is_empty() => p.trim().to_string(),
            _ => {
                eprintln!("Variable GWS_SSH_KEY_PATH non définie, test réel ignoré.");
                return;
            }
        };

        let key_path = PathBuf::from(&key_path_str);
        if !key_path.is_file() {
            eprintln!("Fichier de clé SSH introuvable : {key_path:?}");
            return;
        }

        let port: u16 = std::env::var("GWS_SSH_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(22);
        let user = std::env::var("GWS_SSH_USER").unwrap_or_else(|_| "debian".to_string());
        let passphrase = std::env::var("GWS_SSH_PASSPHRASE").ok().filter(|s| !s.is_empty());

        let mut config = SshRelayConfig {
            host,
            port,
            user,
            auth: SshAuth::Key {
                path: key_path,
                passphrase,
            },
            enabled: true,
            host_key: None,
            appareil: None,
        };

        // Premier passage : serveur inconnu, la connexion doit s'arrêter avant
        // l'authentification en rendant l'empreinte vue.
        let empreinte = match test_connection(&config).await {
            Err(SshFailure::UnknownHostKey(fp)) => fp,
            autre => panic!("un serveur inconnu doit être refusé avant authentification : {autre:?}"),
        };
        println!("Empreinte présentée par le serveur : {empreinte}");
        assert!(is_fingerprint(&empreinte));

        // Une empreinte fausse : refus explicite, pas d'authentification.
        config.host_key = Some(format!("SHA256:{}", "A".repeat(43)));
        assert!(matches!(test_connection(&config).await, Err(SshFailure::ChangedHostKey { .. })));

        config.host_key = Some(empreinte);
        let res = test_connection(&config).await;
        match res {
            Ok(msg) => {
                println!("Succès du test de connexion SSH réel :\n{msg}");
                assert!(msg.contains("curl"));
            }
            Err(e) => {
                panic!("Échec du test de connexion SSH réel : {e}");
            }
        }
    }
}

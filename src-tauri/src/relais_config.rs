//! Configuration signée reçue du serveur relai SSH (lecture seule, v1).
//!
//! ## Rôle
//!
//! Le serveur que l'utilisateur emploie déjà comme relai pour Infoclimat
//! (`ssh_relay.rs`) peut déposer, dans le dossier personnel de l'utilisateur SSH,
//! une petite configuration signée : `~/gws-relais/config.json` et
//! `~/gws-relais/config.sig`. Elle servira plus tard à annoncer une extension et à
//! porter des paramètres de quota partagé. **Dans ce lot, elle est seulement
//! lue, vérifiée et conservée** : l'offre d'extension n'est ni téléchargée ni
//! installée, le quota n'est pas interprété, et rien n'est affiché.
//!
//! ## Garanties
//!
//! * Récupération tentée **uniquement si le relai est activé**, au plus une fois
//!   toutes les [`INTERVALLE_MIN`] (plancher conservé dans les réglages, donc
//!   d'une session à l'autre), en tâche de fond : [`DELAI_DEMARRAGE`] après le
//!   lancement, et après chaque appel Infoclimat passé par le relai. Aucune
//!   minuterie permanente ; un appel d'API n'est jamais retardé. Relai
//!   désactivé : rien ne change.
//! * Commande distante **fixe** ([`commande_distante`]), construite uniquement à
//!   partir des constantes de ce module ; aucun texte venu du réseau ou de
//!   l'utilisateur n'y est interpolé.
//! * Taille plafonnée à [`TAILLE_MAX`] par fichier (côté serveur par `head -c`,
//!   et côté client par la lecture bornée du canal SSH).
//! * Signature Ed25519 vérifiée sur les **octets exacts** de `config.json`, avec
//!   les clés des mises à jour ([`crate::package::CLES_MAJ`]) mais un **contexte
//!   distinct** ([`CONTEXTE`]) : une signature de mise à jour n'est jamais
//!   acceptée comme signature de configuration, ni l'inverse.
//! * Anti-rejeu par `serial` croissant et empreinte SHA-256 du contenu.
//! * Tout échec est journalisé (sans secret) et laisse en place la dernière
//!   configuration valide.
//!
//! Voir `docs/config-relais.md` et `scripts/signer_config.py`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::prelude::*;
use chrono::{DateTime, NaiveDate, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::Db;
use crate::error::AppResult;
use crate::package::SignatureMaj;
use crate::ssh_relay::SshRelayConfig;

// ---------------------------------------------------------------------------
// Constantes (faciles à changer)
// ---------------------------------------------------------------------------

/// Dossier, relatif au dossier personnel de l'utilisateur SSH.
pub const DOSSIER_DISTANT: &str = "gws-relais";
/// Contenu signé (octets exacts).
pub const FICHIER_CONFIG: &str = "config.json";
/// Signature : `{"format":1,"key_id":"…","signature":"<base64>"}`.
pub const FICHIER_SIGNATURE: &str = "config.sig";
/// Taille maximale de chacun des deux fichiers (64 Kio).
pub const TAILLE_MAX: usize = 64 * 1024;
/// Préfixe du message signé ; doit rester identique à `CONTEXTE` de
/// `scripts/signer_config.py`. Il diffère dès le premier octet de celui des
/// mises à jour (`gws-update-v1\0`, voir `package::message_signe`).
pub const CONTEXTE: &[u8] = b"GWS-CONFIG-RELAIS-V1\0";
/// Seul format de `config.json` compris par cette version.
pub const FORMAT_CONFIG: u64 = 1;
/// Intervalle minimal entre deux tentatives de récupération, réussies ou non
/// (heure de la dernière tentative dans [`SETTING_DERNIERE_TENTATIVE`]).
pub const INTERVALLE_MIN: Duration = Duration::from_secs(10 * 60);
/// Délai maximal d'une récupération complète (connexion comprise).
pub const DELAI_RECUPERATION: Duration = Duration::from_secs(45);

// Réglages (`app_settings`), tous préfixés `relais_config_`.
pub const SETTING_JSON: &str = "relais_config_json";
pub const SETTING_SIG: &str = "relais_config_sig";
pub const SETTING_SERIAL: &str = "relais_config_serial";
pub const SETTING_SHA256: &str = "relais_config_sha256";
pub const SETTING_KEY_ID: &str = "relais_config_key_id";
pub const SETTING_ACCEPTEE_LE: &str = "relais_config_acceptee_le";
pub const SETTING_DERNIER_REFUS: &str = "relais_config_dernier_refus";
pub const SETTING_DERNIER_REFUS_LE: &str = "relais_config_dernier_refus_le";
/// Heure (RFC 3339) de la dernière tentative de relevé, réussie ou non : le
/// plancher de [`INTERVALLE_MIN`] survit ainsi à la fermeture de l'application.
pub const SETTING_DERNIERE_TENTATIVE: &str = "relais_config_derniere_tentative";

// ---------------------------------------------------------------------------
// Format
// ---------------------------------------------------------------------------

/// Offre d'extension : **seulement une offre**, jamais téléchargée ni installée
/// automatiquement dans ce lot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OffreExtension {
    pub nom: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
}

/// Contenu de `config.json` (`format` = 1). Les champs inconnus sont ignorés.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigRelais {
    pub format: u64,
    /// Entier ≥ 0, croissant d'une publication à l'autre.
    pub serial: u64,
    /// Date d'émission (ISO), informative.
    pub emis_le: String,
    /// Date ISO (`AAAA-MM-JJ` ou RFC 3339) au-delà de laquelle la configuration
    /// est refusée. Une date seule vaut jusqu'à la fin de ce jour (UTC).
    #[serde(default)]
    pub expire_le: Option<String>,
    /// Nom du serveur, informatif.
    pub serveur: String,
    #[serde(default)]
    pub extension: Option<OffreExtension>,
    /// Objet libre, conservé tel quel, non interprété dans ce lot.
    #[serde(default)]
    pub quota: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Message signé : [`CONTEXTE`] suivi des octets bruts de `config.json`.
pub fn message_signe_config(config: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(CONTEXTE.len() + config.len());
    m.extend_from_slice(CONTEXTE);
    m.extend_from_slice(config);
    m
}

/// Vérifie `config.sig` contre les octets exacts de `config.json` ; renvoie le
/// `key_id` de la clé qui a signé.
pub fn verifier_signature_config(
    config: &[u8],
    sig_brute: &[u8],
    cles: &[(&str, [u8; 32])],
) -> Result<String, String> {
    let texte = std::str::from_utf8(sig_brute).map_err(|_| "signature illisible (pas du texte UTF-8).".to_string())?;
    let s: SignatureMaj = serde_json::from_str(texte.trim_start_matches('\u{feff}'))
        .map_err(|_| "signature illisible (JSON attendu).".to_string())?;
    if s.format != 1 {
        return Err(format!("format de signature inconnu ({}).", s.format));
    }
    let (_, cle) = cles
        .iter()
        .find(|(id, _)| *id == s.key_id)
        .ok_or_else(|| format!("signée par une clé inconnue (« {} »).", s.key_id))?;
    let cle = VerifyingKey::from_bytes(cle).map_err(|_| "clé publique invalide.".to_string())?;
    let octets = BASE64_STANDARD
        .decode(s.signature.trim())
        .map_err(|_| "signature illisible (base64).".to_string())?;
    let sig = Signature::from_slice(&octets).map_err(|_| "signature illisible (longueur).".to_string())?;
    cle.verify(&message_signe_config(config), &sig)
        .map_err(|_| "signature invalide : fichier modifié ou non signé par l'éditeur.".to_string())?;
    Ok(s.key_id)
}

/// Lit une date ISO : RFC 3339, ou `AAAA-MM-JJ` (valable jusqu'à la fin du jour UTC).
/// Renvoie l'instant d'expiration.
pub fn instant_d_expiration(texte: &str) -> Option<DateTime<Utc>> {
    let t = texte.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(t) {
        return Some(d.with_timezone(&Utc));
    }
    let jour = NaiveDate::parse_from_str(t, "%Y-%m-%d").ok()?;
    Some(jour.succ_opt()?.and_hms_opt(0, 0, 0)?.and_utc())
}

/// Analyse une configuration **dont la signature a déjà été vérifiée**.
pub fn analyser_config(config: &[u8]) -> Result<ConfigRelais, String> {
    let texte = std::str::from_utf8(config).map_err(|_| "config.json n'est pas du texte UTF-8.".to_string())?;
    let texte = texte.trim_start_matches('\u{feff}');
    let valeur: serde_json::Value =
        serde_json::from_str(texte).map_err(|e| format!("config.json illisible ({e})."))?;
    match valeur.get("format").and_then(|f| f.as_u64()) {
        Some(FORMAT_CONFIG) => {}
        Some(autre) => return Err(format!("format de configuration inconnu ({autre}) : configuration ignorée.")),
        None => return Err("champ « format » absent ou invalide : configuration ignorée.".into()),
    }
    let c: ConfigRelais =
        serde_json::from_value(valeur).map_err(|e| format!("config.json invalide ({e})."))?;
    if let Some(e) = &c.expire_le {
        if instant_d_expiration(e).is_none() {
            return Err(format!("date d'expiration illisible (« {e} »)."));
        }
    }
    if let Some(x) = &c.extension {
        if x.nom.trim().is_empty() || x.version.trim().is_empty() {
            return Err("offre d'extension incomplète (nom ou version vide).".into());
        }
        if !x.url.starts_with("https://") {
            return Err("offre d'extension : l'adresse doit commencer par https://.".into());
        }
        if x.sha256.len() != 64 || !x.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("offre d'extension : sha256 attendu en 64 caractères hexadécimaux.".into());
        }
    }
    Ok(c)
}

/// `true` si la configuration est expirée à l'instant `maintenant`.
pub fn est_expiree(c: &ConfigRelais, maintenant: DateTime<Utc>) -> bool {
    c.expire_le
        .as_deref()
        .and_then(instant_d_expiration)
        .map(|fin| maintenant >= fin)
        .unwrap_or(false)
}

/// Empreinte SHA-256 (hex minuscule) des octets exacts de `config.json`.
pub fn empreinte(config: &[u8]) -> String {
    format!("{:x}", Sha256::digest(config))
}

/// Résultat d'une évaluation réussie.
#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    pub config: ConfigRelais,
    pub key_id: String,
    pub sha256: String,
    /// Même `serial` et même contenu que la configuration déjà acceptée.
    pub deja_appliquee: bool,
}

/// Contrôle complet, sans effet de bord, dans cet ordre : taille, signature,
/// JSON et format, expiration, anti-rejeu. `precedent` = `(serial, sha256)` de
/// la dernière configuration acceptée.
pub fn evaluer(
    config: &[u8],
    sig: &[u8],
    cles: &[(&str, [u8; 32])],
    maintenant: DateTime<Utc>,
    precedent: Option<(u64, &str)>,
) -> Result<Evaluation, String> {
    if config.len() > TAILLE_MAX || sig.len() > TAILLE_MAX {
        return Err(format!("fichier trop volumineux (plus de {TAILLE_MAX} octets)."));
    }
    let key_id = verifier_signature_config(config, sig, cles)?;
    let c = analyser_config(config)?;
    if est_expiree(&c, maintenant) {
        return Err(format!(
            "configuration expirée (expire_le = {}).",
            c.expire_le.as_deref().unwrap_or("")
        ));
    }
    let sha256 = empreinte(config);
    let mut deja_appliquee = false;
    if let Some((serial_prec, sha_prec)) = precedent {
        if c.serial < serial_prec {
            return Err(format!(
                "rejeu refusé : serial {} plus ancien que celui déjà accepté ({serial_prec}).",
                c.serial
            ));
        }
        if c.serial == serial_prec {
            if sha256.eq_ignore_ascii_case(sha_prec) {
                deja_appliquee = true;
            } else {
                return Err(format!(
                    "serial {} déjà accepté avec un autre contenu : refusé.",
                    c.serial
                ));
            }
        }
    }
    Ok(Evaluation { config: c, key_id, sha256, deja_appliquee })
}

// ---------------------------------------------------------------------------
// Persistance
// ---------------------------------------------------------------------------

/// Issue d'une réception traitée avec succès.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    Acceptee { serial: u64 },
    DejaAppliquee { serial: u64 },
}

fn precedent(db: &Db) -> AppResult<Option<(u64, String)>> {
    let serial = db.get_setting(SETTING_SERIAL)?.and_then(|s| s.trim().parse::<u64>().ok());
    let sha = db.get_setting(SETTING_SHA256)?.filter(|s| !s.is_empty());
    Ok(match (serial, sha) {
        (Some(s), Some(h)) => Some((s, h)),
        _ => None,
    })
}

/// Enregistre le motif d'un échec, sans toucher à la configuration acceptée.
pub fn noter_refus(db: &Db, motif: &str, maintenant: DateTime<Utc>) {
    eprintln!("[relais-config] configuration ignorée : {motif}");
    let _ = db.set_settings(&[
        (SETTING_DERNIER_REFUS, motif),
        (SETTING_DERNIER_REFUS_LE, &maintenant.to_rfc3339()),
    ]);
}

/// Traite les deux fichiers reçus : évalue, puis enregistre si la configuration
/// est nouvelle. En cas d'échec, la configuration précédente reste en place et
/// le motif est noté.
pub fn traiter_reception(
    db: &Db,
    config: &[u8],
    sig: &[u8],
    cles: &[(&str, [u8; 32])],
    maintenant: DateTime<Utc>,
) -> Result<Issue, String> {
    let prec = precedent(db).map_err(|e| e.to_string())?;
    let ev = match evaluer(config, sig, cles, maintenant, prec.as_ref().map(|(s, h)| (*s, h.as_str()))) {
        Ok(ev) => ev,
        Err(motif) => {
            noter_refus(db, &motif, maintenant);
            return Err(motif);
        }
    };
    if ev.deja_appliquee {
        let _ = db.set_settings(&[(SETTING_DERNIER_REFUS, ""), (SETTING_DERNIER_REFUS_LE, "")]);
        return Ok(Issue::DejaAppliquee { serial: ev.config.serial });
    }
    // Les deux fichiers sont du texte UTF-8 : vérifié par la signature et l'analyse.
    let json = String::from_utf8_lossy(config);
    let sig_txt = String::from_utf8_lossy(sig);
    let serial = ev.config.serial.to_string();
    let quand = maintenant.to_rfc3339();
    db.set_settings(&[
        (SETTING_JSON, &json),
        (SETTING_SIG, &sig_txt),
        (SETTING_KEY_ID, &ev.key_id),
        (SETTING_ACCEPTEE_LE, &quand),
        (SETTING_SERIAL, &serial),
        (SETTING_SHA256, &ev.sha256),
        (SETTING_DERNIER_REFUS, ""),
        (SETTING_DERNIER_REFUS_LE, ""),
    ])
    .map_err(|e| format!("enregistrement impossible : {e}"))?;
    eprintln!(
        "[relais-config] configuration acceptée : serial {}, clé {}",
        ev.config.serial, ev.key_id
    );
    Ok(Issue::Acceptee { serial: ev.config.serial })
}

/// Configuration acceptée, **re-vérifiée** (signature et format) depuis la base.
/// `None` si aucune, ou si ce qui est stocké ne se vérifie plus. L'expiration
/// n'est pas testée ici : voir [`est_expiree`].
pub fn configuration_acceptee(db: &Db, cles: &[(&str, [u8; 32])]) -> AppResult<Option<ConfigRelais>> {
    let json = db.get_setting(SETTING_JSON)?.unwrap_or_default();
    let sig = db.get_setting(SETTING_SIG)?.unwrap_or_default();
    if json.is_empty() || sig.is_empty() {
        return Ok(None);
    }
    if verifier_signature_config(json.as_bytes(), sig.as_bytes(), cles).is_err() {
        return Ok(None);
    }
    Ok(analyser_config(json.as_bytes()).ok())
}

/// Statut en lecture seule pour la commande `get_relais_config_status`.
/// Ne contient aucun secret (ni identifiant SSH, ni contenu du quota).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelaisConfigStatus {
    pub presente: bool,
    pub serial: Option<u64>,
    pub key_id: Option<String>,
    pub acceptee_le: Option<String>,
    pub offre_extension: bool,
    pub expiree: bool,
    pub dernier_refus: Option<String>,
    pub dernier_refus_le: Option<String>,
}

pub fn statut_avec(db: &Db, cles: &[(&str, [u8; 32])], maintenant: DateTime<Utc>) -> AppResult<RelaisConfigStatus> {
    let non_vide = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    let c = configuration_acceptee(db, cles)?;
    Ok(RelaisConfigStatus {
        presente: c.is_some(),
        serial: c.as_ref().map(|c| c.serial),
        key_id: c.as_ref().and(non_vide(db.get_setting(SETTING_KEY_ID)?)),
        acceptee_le: c.as_ref().and(non_vide(db.get_setting(SETTING_ACCEPTEE_LE)?)),
        offre_extension: c.as_ref().map(|c| c.extension.is_some()).unwrap_or(false),
        expiree: c.as_ref().map(|c| est_expiree(c, maintenant)).unwrap_or(false),
        dernier_refus: non_vide(db.get_setting(SETTING_DERNIER_REFUS)?),
        dernier_refus_le: non_vide(db.get_setting(SETTING_DERNIER_REFUS_LE)?),
    })
}

pub fn statut(db: &Db) -> AppResult<RelaisConfigStatus> {
    statut_avec(db, crate::package::CLES_MAJ, Utc::now())
}

// ---------------------------------------------------------------------------
// Récupération par le relai SSH
// ---------------------------------------------------------------------------

/// Commande distante **fixe**, construite une fois à partir des seules
/// constantes du module. Pour chacun des deux fichiers, elle écrit soit
/// `absent\n`, soit `<taille>\n` suivi d'au plus `TAILLE_MAX + 1` octets du
/// fichier (l'octet de plus permet de détecter un fichier trop gros).
pub fn commande_distante() -> &'static str {
    static COMMANDE: OnceLock<String> = OnceLock::new();
    COMMANDE.get_or_init(|| {
        let lim = TAILLE_MAX + 1;
        format!(
            "for f in {FICHIER_CONFIG} {FICHIER_SIGNATURE}; do \
             p=\"$HOME\"/{DOSSIER_DISTANT}/\"$f\"; \
             if [ -f \"$p\" ]; then \
             n=$(head -c {lim} -- \"$p\" | wc -c); printf '%s\\n' \"$n\"; head -c {lim} -- \"$p\"; \
             else printf 'absent\\n'; fi; done"
        )
    })
}

/// Plafond de la sortie lue sur le canal SSH (deux fichiers + en-têtes).
pub const SORTIE_MAX: usize = 2 * (TAILLE_MAX + 1 + 32);

/// Début du motif noté quand la sortie de la commande distante est inutilisable :
/// fichier absent, ou serveur dont le shell n'est pas POSIX (voir la doc).
pub const MOTIF_INUTILISABLE: &str = "serveur non compatible ou fichier absent";

fn inutilisable(detail: &str) -> String {
    format!("{MOTIF_INUTILISABLE} ({detail}). Nouvel essai dans 10 minutes au plus tôt.")
}

/// Découpe la sortie de [`commande_distante`] en `(config.json, config.sig)`.
pub fn decouper_sortie(sortie: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut reste = sortie;
    let mut fichiers = Vec::with_capacity(2);
    for nom in [FICHIER_CONFIG, FICHIER_SIGNATURE] {
        let fin = reste
            .iter()
            .take(32)
            .position(|&b| b == b'\n')
            .ok_or_else(|| inutilisable("réponse inattendue, en-tête manquant"))?;
        let entete = std::str::from_utf8(&reste[..fin]).unwrap_or("").trim();
        reste = &reste[fin + 1..];
        if entete == "absent" {
            return Err(inutilisable(&format!("aucun fichier {DOSSIER_DISTANT}/{nom} sur le serveur")));
        }
        let n: usize = entete
            .parse()
            .map_err(|_| inutilisable("réponse inattendue, taille illisible"))?;
        if n > TAILLE_MAX {
            return Err(format!("{nom} trop volumineux (plus de {TAILLE_MAX} octets)."));
        }
        if reste.len() < n {
            return Err(inutilisable(&format!("{nom} tronqué pendant la lecture")));
        }
        fichiers.push(reste[..n].to_vec());
        reste = &reste[n..];
    }
    if !reste.is_empty() {
        return Err(inutilisable("réponse inattendue, octets en trop"));
    }
    let sig = fichiers.pop().unwrap_or_default();
    let config = fichiers.pop().unwrap_or_default();
    Ok((config, sig))
}

/// Récupère les deux fichiers par le relai (connexion à clé d'hôte épinglée).
pub async fn recuperer(relais: &SshRelayConfig) -> Result<(Vec<u8>, Vec<u8>), String> {
    let (code, stdout, _stderr) =
        crate::ssh_relay::executer_commande_fixe_bornee(relais, commande_distante(), SORTIE_MAX)
            .await
            .map_err(|e| format!("relai SSH : {e}"))?;
    if code != 0 {
        return Err(inutilisable(&format!("la commande de lecture a échoué, code {code}")));
    }
    decouper_sortie(&stdout)
}

/// `true` si une nouvelle tentative est permise à `maintenant`, la précédente
/// ayant eu lieu à `derniere` (lue dans les réglages, donc conservée d'une
/// session à l'autre). Une date **dans le futur** (horloge qui a reculé, réglage
/// incohérent) est ignorée : le plancher ne peut jamais bloquer indéfiniment.
pub fn tentative_permise(derniere: Option<DateTime<Utc>>, maintenant: DateTime<Utc>) -> bool {
    match derniere {
        None => true,
        Some(t) if t > maintenant => true,
        Some(t) => (maintenant - t).to_std().map(|d| d >= INTERVALLE_MIN).unwrap_or(true),
    }
}

/// Sérialise les réservations (lecture puis écriture du réglage).
static RESERVATION: Mutex<()> = Mutex::new(());
/// Une seule récupération à la fois dans ce processus.
static EN_COURS: AtomicBool = AtomicBool::new(false);

/// Réserve le créneau : si le plancher est respecté, note **avant** la tentative
/// l'heure de celle-ci (réussie ou non) dans [`SETTING_DERNIERE_TENTATIVE`].
/// Une valeur illisible est traitée comme absente.
pub fn reserver_creneau(db: &Db, maintenant: DateTime<Utc>) -> AppResult<bool> {
    let _verrou = RESERVATION.lock().unwrap_or_else(|e| e.into_inner());
    let derniere = db
        .get_setting(SETTING_DERNIERE_TENTATIVE)?
        .and_then(|s| DateTime::parse_from_rfc3339(s.trim()).ok())
        .map(|d| d.with_timezone(&Utc));
    if !tentative_permise(derniere, maintenant) {
        return Ok(false);
    }
    db.set_setting(SETTING_DERNIERE_TENTATIVE, &maintenant.to_rfc3339())?;
    Ok(true)
}

/// Délai entre le lancement de l'application et le relevé de démarrage.
pub const DELAI_DEMARRAGE: Duration = Duration::from_secs(30);

/// Relevé de démarrage : attend [`DELAI_DEMARRAGE`], puis, **si le relai est
/// activé**, tente un relevé (plancher de 10 minutes compris). Une seule fois ;
/// aucune minuterie permanente. Ne bloque pas le démarrage.
pub fn planifier_au_demarrage(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(DELAI_DEMARRAGE).await;
        let relais = {
            use tauri::Manager;
            let Some(state) = app.try_state::<crate::commands::AppState>() else {
                return;
            };
            match crate::ssh_relay::load_config(&state.db) {
                Ok(r) => r,
                Err(_) => return,
            }
        };
        declencher_si_opportun(app, relais.as_ref());
    });
}

/// Lance, en tâche de fond, la récupération de la configuration signée si le
/// relai est activé et si la dernière tentative (réglage persistant) date de
/// plus de [`INTERVALLE_MIN`]. Retour immédiat : l'appelant n'attend jamais.
pub fn declencher_si_opportun(app: tauri::AppHandle, relais: Option<&SshRelayConfig>) {
    let Some(relais) = relais.filter(|r| r.enabled).cloned() else {
        return;
    };
    if EN_COURS.swap(true, Ordering::SeqCst) {
        return;
    }
    let reserve = {
        use tauri::Manager;
        match app.try_state::<crate::commands::AppState>() {
            Some(state) => reserver_creneau(&state.db, Utc::now()).unwrap_or(false),
            None => false,
        }
    };
    if !reserve {
        EN_COURS.store(false, Ordering::SeqCst);
        return;
    }
    tauri::async_runtime::spawn(async move {
        let resultat = match tokio::time::timeout(DELAI_RECUPERATION, recuperer(&relais)).await {
            Ok(r) => r,
            Err(_) => Err(format!("délai dépassé ({} s).", DELAI_RECUPERATION.as_secs())),
        };
        {
            use tauri::Manager;
            if let Some(state) = app.try_state::<crate::commands::AppState>() {
                let maintenant = Utc::now();
                match resultat {
                    Ok((config, sig)) => {
                        let _ = traiter_reception(&state.db, &config, &sig, crate::package::CLES_MAJ, maintenant);
                    }
                    Err(motif) => noter_refus(&state.db, &motif, maintenant),
                }
            }
        }
        EN_COURS.store(false, Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::{message_signe, verifier_signature, PackageManifest};
    use chrono::TimeZone;
    use ed25519_dalek::{Signer, SigningKey};

    // Paires de TEST (graines fixes, publiques) : jamais une vraie clé.
    fn cle_test() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }
    fn autre_cle() -> SigningKey {
        SigningKey::from_bytes(&[9u8; 32])
    }
    fn cles_test() -> Vec<(&'static str, [u8; 32])> {
        vec![("test-a", cle_test().verifying_key().to_bytes())]
    }

    fn sig_json_de(message: &[u8], cle: &SigningKey, key_id: &str) -> Vec<u8> {
        serde_json::to_vec(&SignatureMaj {
            format: 1,
            key_id: key_id.into(),
            signature: BASE64_STANDARD.encode(cle.sign(message).to_bytes()),
        })
        .unwrap()
    }
    fn signer(config: &[u8]) -> Vec<u8> {
        sig_json_de(&message_signe_config(config), &cle_test(), "test-a")
    }
    fn config(serial: u64) -> Vec<u8> {
        format!(
            "{{\n  \"format\": 1,\n  \"serial\": {serial},\n  \"emis_le\": \"2026-09-29\",\n  \
             \"serveur\": \"relais-test\"\n}}\n"
        )
        .into_bytes()
    }
    fn t(j: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, j, 12, 0, 0).unwrap()
    }

    #[test]
    fn configuration_signee_acceptee_et_champs_inconnus_ignores() {
        let c = br#"{"format":1,"serial":3,"emis_le":"2026-09-29T10:00:00Z","serveur":"s",
            "futur_champ":{"a":1},"quota":{"max_par_jour":4000},
            "extension":{"nom":"carte3d","version":"1.2.0","url":"https://example.org/x.zip",
            "sha256":"0000000000000000000000000000000000000000000000000000000000000000"}}"#;
        let ev = evaluer(c, &signer(c), &cles_test(), t(29), None).unwrap();
        assert_eq!(ev.key_id, "test-a");
        assert_eq!(ev.config.serial, 3);
        assert!(!ev.deja_appliquee);
        assert_eq!(ev.config.extension.as_ref().unwrap().nom, "carte3d");
        assert_eq!(ev.config.quota.as_ref().unwrap()["max_par_jour"], 4000);
        assert_eq!(ev.sha256, empreinte(c));
    }

    #[test]
    fn un_octet_modifie_invalide_la_signature() {
        let c = config(1);
        let sig = signer(&c);
        let mut altere = c.clone();
        // Une espace devient tabulation : JSON équivalent, octets différents.
        // La vérification porte sur les octets exacts, jamais sur une re-sérialisation.
        assert_eq!(altere[2], b' ');
        altere[2] = b'\t';
        assert!(analyser_config(&altere).is_ok());
        assert!(evaluer(&altere, &sig, &cles_test(), t(29), None).unwrap_err().contains("signature invalide"));
    }

    #[test]
    fn cle_inconnue_ou_mauvaise_cle_refusees() {
        let c = config(1);
        let sig = sig_json_de(&message_signe_config(&c), &autre_cle(), "test-a");
        assert!(evaluer(&c, &sig, &cles_test(), t(29), None).unwrap_err().contains("signature invalide"));
        let sig = sig_json_de(&message_signe_config(&c), &autre_cle(), "pirate");
        assert!(evaluer(&c, &sig, &cles_test(), t(29), None).unwrap_err().contains("clé inconnue"));
        // Aucune clé : tout est refusé.
        assert!(evaluer(&c, &signer(&c), &[], t(29), None).unwrap_err().contains("clé inconnue"));
    }

    #[test]
    fn signature_illisible_refusee() {
        let c = config(1);
        assert!(evaluer(&c, b"placeholder", &cles_test(), t(29), None).unwrap_err().contains("illisible"));
        let s = br#"{"format":1,"key_id":"test-a","signature":"AAAA"}"#;
        assert!(evaluer(&c, s, &cles_test(), t(29), None).unwrap_err().contains("illisible"));
        let s = br#"{"format":2,"key_id":"test-a","signature":"AAAA"}"#;
        assert!(evaluer(&c, s, &cles_test(), t(29), None).unwrap_err().contains("format de signature"));
    }

    /// Séparation des domaines : une signature de mise à jour n'est jamais une
    /// signature de configuration, et inversement.
    #[test]
    fn signatures_de_maj_et_de_config_ne_sont_pas_interchangeables() {
        // Les deux préfixes diffèrent dès le premier octet : aucun message ne
        // peut être à la fois un message de mise à jour et de configuration.
        assert!(!b"gws-update-v1\0".starts_with(&CONTEXTE[..1]));
        let m = PackageManifest {
            package_type: "gws-update".into(),
            version: "99.0.0".into(),
            min_compatible_version: Some("0.1.0".into()),
            release_date: None,
            title: None,
            changelog: None,
            checksum_sha256: "ab".repeat(32),
        };
        let msg_maj = message_signe(&m, 20);
        assert_ne!(&msg_maj[..CONTEXTE.len().min(msg_maj.len())], CONTEXTE);

        // 1. Une vraie signature de mise à jour, présentée comme config.sig
        //    d'un config.json qui contiendrait exactement le message signé.
        let sig_maj = sig_json_de(&msg_maj, &cle_test(), "test-a");
        assert!(verifier_signature(&m, 20, Some(std::str::from_utf8(&sig_maj).unwrap()), &cles_test()).is_ok());
        assert!(verifier_signature_config(&msg_maj, &sig_maj, &cles_test())
            .unwrap_err()
            .contains("signature invalide"));

        // 2. Une signature sur les octets bruts (sans contexte) est refusée.
        let c = config(1);
        let sig_brute = sig_json_de(&c, &cle_test(), "test-a");
        assert!(verifier_signature_config(&c, &sig_brute, &cles_test()).is_err());

        // 3. Une signature de configuration, présentée comme manifest.sig, est refusée.
        let sig_conf = signer(&msg_maj);
        assert!(verifier_signature_config(&msg_maj, &sig_conf, &cles_test()).is_ok());
        let e = verifier_signature(&m, 20, Some(std::str::from_utf8(&sig_conf).unwrap()), &cles_test())
            .unwrap_err();
        assert!(e.contains("signature invalide"), "{e}");
    }

    #[test]
    fn format_inconnu_ou_json_illisible_ignores() {
        let c = br#"{"format":2,"serial":1,"emis_le":"x","serveur":"s"}"#;
        assert!(evaluer(c, &signer(c), &cles_test(), t(29), None).unwrap_err().contains("format de configuration inconnu"));
        let c = b"pas du json";
        assert!(evaluer(c, &signer(c), &cles_test(), t(29), None).unwrap_err().contains("illisible"));
        let c = br#"{"format":1,"serial":-1,"emis_le":"x","serveur":"s"}"#;
        assert!(evaluer(c, &signer(c), &cles_test(), t(29), None).unwrap_err().contains("invalide"));
        let c = br#"{"format":1,"emis_le":"x","serveur":"s"}"#;
        assert!(evaluer(c, &signer(c), &cles_test(), t(29), None).is_err());
    }

    #[test]
    fn offre_d_extension_mal_formee_refusee() {
        let c = br#"{"format":1,"serial":1,"emis_le":"x","serveur":"s","extension":{"nom":"a","version":"1","url":"http://x","sha256":"00"}}"#;
        assert!(evaluer(c, &signer(c), &cles_test(), t(29), None).unwrap_err().contains("https://"));
    }

    #[test]
    fn trop_gros_refuse() {
        let mut c = config(1);
        c.resize(TAILLE_MAX + 1, b' ');
        assert!(evaluer(&c, &signer(&c), &cles_test(), t(29), None).unwrap_err().contains("volumineux"));
    }

    #[test]
    fn expiration() {
        let c = br#"{"format":1,"serial":1,"emis_le":"x","serveur":"s","expire_le":"2026-09-28"}"#;
        assert!(evaluer(c, &signer(c), &cles_test(), t(28), None).is_ok()); // valable tout le jour
        assert!(evaluer(c, &signer(c), &cles_test(), t(29), None).unwrap_err().contains("expirée"));
        let c = br#"{"format":1,"serial":1,"emis_le":"x","serveur":"s","expire_le":"2026-09-29T11:00:00Z"}"#;
        assert!(evaluer(c, &signer(c), &cles_test(), t(29), None).unwrap_err().contains("expirée"));
        let c = br#"{"format":1,"serial":1,"emis_le":"x","serveur":"s","expire_le":"demain"}"#;
        assert!(evaluer(c, &signer(c), &cles_test(), t(29), None).unwrap_err().contains("illisible"));
    }

    #[test]
    fn anti_rejeu() {
        let c5 = config(5);
        let h5 = empreinte(&c5);
        let c4 = config(4);
        assert!(evaluer(&c4, &signer(&c4), &cles_test(), t(29), Some((5, &h5))).unwrap_err().contains("rejeu"));
        let ev = evaluer(&c5, &signer(&c5), &cles_test(), t(29), Some((5, &h5))).unwrap();
        assert!(ev.deja_appliquee);
        let mut autre = c5.clone();
        autre.extend_from_slice(b" ");
        assert!(evaluer(&autre, &signer(&autre), &cles_test(), t(29), Some((5, &h5)))
            .unwrap_err()
            .contains("autre contenu"));
        assert!(!evaluer(&config(6), &signer(&config(6)), &cles_test(), t(29), Some((5, &h5))).unwrap().deja_appliquee);
    }

    #[test]
    fn persistance_garde_la_derniere_configuration_valide() {
        let db = Db::open_in_memory().unwrap();
        let cles = cles_test();
        let vide = statut_avec(&db, &cles, t(29)).unwrap();
        assert!(!vide.presente && vide.serial.is_none() && vide.dernier_refus.is_none());

        let c2 = config(2);
        assert_eq!(traiter_reception(&db, &c2, &signer(&c2), &cles, t(29)), Ok(Issue::Acceptee { serial: 2 }));
        assert_eq!(traiter_reception(&db, &c2, &signer(&c2), &cles, t(29)), Ok(Issue::DejaAppliquee { serial: 2 }));
        // Octets exacts conservés.
        assert_eq!(db.get_setting(SETTING_JSON).unwrap().unwrap().as_bytes(), &c2[..]);

        // Un rejeu et une signature invalide ne remplacent rien.
        let c1 = config(1);
        assert!(traiter_reception(&db, &c1, &signer(&c1), &cles, t(29)).is_err());
        let c3 = config(3);
        assert!(traiter_reception(&db, &c3, &signer(&c2), &cles, t(29)).is_err());
        let s = statut_avec(&db, &cles, t(29)).unwrap();
        assert!(s.presente);
        assert_eq!(s.serial, Some(2));
        assert_eq!(s.key_id.as_deref(), Some("test-a"));
        assert!(s.acceptee_le.is_some());
        assert!(!s.offre_extension);
        assert!(s.dernier_refus.unwrap().contains("signature invalide"));

        // Une réception réussie efface le motif de refus.
        assert_eq!(traiter_reception(&db, &c3, &signer(&c3), &cles, t(29)), Ok(Issue::Acceptee { serial: 3 }));
        let s = statut_avec(&db, &cles, t(29)).unwrap();
        assert_eq!(s.serial, Some(3));
        assert!(s.dernier_refus.is_none());

        // Contenu stocké altéré : il ne se vérifie plus, il n'est plus présenté.
        db.set_setting(SETTING_JSON, "{\"format\":1,\"serial\":99,\"emis_le\":\"x\",\"serveur\":\"s\"}").unwrap();
        assert!(!statut_avec(&db, &cles, t(29)).unwrap().presente);
    }

    #[test]
    fn le_statut_ne_contient_ni_quota_ni_secret() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting(crate::ssh_relay::SETTING_SSH_PASSWORD, "mot_de_passe_de_test").unwrap();
        let c = br#"{"format":1,"serial":1,"emis_le":"x","serveur":"s","quota":{"jeton":"valeur_du_quota"}}"#;
        traiter_reception(&db, c, &signer(c), &cles_test(), t(29)).unwrap();
        let json = serde_json::to_string(&statut_avec(&db, &cles_test(), t(29)).unwrap()).unwrap();
        assert!(!json.contains("mot_de_passe_de_test"));
        assert!(!json.contains("valeur_du_quota"));
    }

    #[test]
    fn statut_signale_l_expiration() {
        let db = Db::open_in_memory().unwrap();
        let c = br#"{"format":1,"serial":1,"emis_le":"x","serveur":"s","expire_le":"2026-09-28"}"#;
        traiter_reception(&db, c, &signer(c), &cles_test(), t(28)).unwrap();
        assert!(!statut_avec(&db, &cles_test(), t(28)).unwrap().expiree);
        assert!(statut_avec(&db, &cles_test(), t(29)).unwrap().expiree);
    }

    /// La commande distante ne contient que des constantes sûres.
    #[test]
    fn commande_distante_fixe() {
        for c in [DOSSIER_DISTANT, FICHIER_CONFIG, FICHIER_SIGNATURE] {
            assert!(c.chars().all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch)), "{c}");
        }
        let cmd = commande_distante();
        assert!(cmd.contains("\"$HOME\"/gws-relais/\"$f\""));
        assert!(cmd.contains(&format!("head -c {}", TAILLE_MAX + 1)));
        assert!(!cmd.contains('`'));
    }

    #[test]
    fn decoupage_de_la_sortie() {
        let c = config(1);
        let s = signer(&c);
        let mut sortie = format!("{}\n", c.len()).into_bytes();
        sortie.extend_from_slice(&c);
        sortie.extend_from_slice(format!("  {}\n", s.len()).as_bytes()); // `wc -c` BSD : espaces
        sortie.extend_from_slice(&s);
        assert_eq!(decouper_sortie(&sortie).unwrap(), (c.clone(), s.clone()));

        assert!(decouper_sortie(b"absent\nabsent\n").unwrap_err().contains("aucun fichier"));
        let mut gros = format!("{}\n", TAILLE_MAX + 1).into_bytes();
        gros.resize(gros.len() + TAILLE_MAX + 1, b'x');
        assert!(decouper_sortie(&gros).unwrap_err().contains("volumineux"));
        assert!(decouper_sortie(b"10\ncourt").unwrap_err().contains("tronqué"));
        let mut trop = sortie.clone();
        trop.push(b'!');
        assert!(decouper_sortie(&trop).unwrap_err().contains("en trop"));
        assert!(decouper_sortie(b"").is_err());

        // Sortie inutilisable (shell non POSIX, fichier absent…) : message clair.
        for inutilisable in [&b"absent\nabsent\n"[..], b"", b"fish: Unknown command\n", b"10\ncourt"] {
            let e = decouper_sortie(inutilisable).unwrap_err();
            assert!(e.starts_with(MOTIF_INUTILISABLE), "{e}");
        }
    }

    fn min(m: i64) -> chrono::Duration {
        chrono::Duration::minutes(m)
    }

    #[test]
    fn frequence_limitee() {
        let t0 = t(29);
        assert!(tentative_permise(None, t0));
        assert!(!tentative_permise(Some(t0), t0 + min(1)));
        assert!(!tentative_permise(Some(t0), t0 + min(10) - chrono::Duration::seconds(1)));
        assert!(tentative_permise(Some(t0), t0 + min(10)));
        // Horloge qui a reculé : la date enregistrée est dans le futur, ignorée.
        assert!(tentative_permise(Some(t0 + min(60 * 24 * 365)), t0));
    }

    /// Le plancher est dans la base : il survit à un redémarrage (simulé par
    /// une relecture de la même base, sans aucun état en mémoire).
    #[test]
    fn plancher_persistant_apres_redemarrage() {
        let dir = std::env::temp_dir().join(format!("gws_relais_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = dir.join("base.sqlite3");
        let t0 = t(29);
        {
            let db = Db::open(&chemin).unwrap();
            assert!(reserver_creneau(&db, t0).unwrap());
            assert!(!reserver_creneau(&db, t0 + min(2)).unwrap());
        } // « fermeture de l'application »
        {
            let db = Db::open(&chemin).unwrap();
            assert!(!reserver_creneau(&db, t0 + min(5)).unwrap(), "plancher oublié après redémarrage");
            assert!(reserver_creneau(&db, t0 + min(10)).unwrap());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Un échec compte comme une tentative : pas de nouvel essai avant 10 minutes.
    #[test]
    fn echec_puis_nouvelle_tentative_apres_dix_minutes() {
        let db = Db::open_in_memory().unwrap();
        let t0 = t(29);
        assert!(reserver_creneau(&db, t0).unwrap());
        noter_refus(&db, &inutilisable("test"), t0);
        assert!(!reserver_creneau(&db, t0 + min(9)).unwrap());
        assert!(reserver_creneau(&db, t0 + min(10)).unwrap());
        assert!(statut_avec(&db, &cles_test(), t0).unwrap().dernier_refus.unwrap().starts_with(MOTIF_INUTILISABLE));
    }

    /// Réglage incohérent (horloge qui a reculé, valeur illisible) : jamais bloquant.
    #[test]
    fn horloge_incoherente_ne_bloque_jamais() {
        let db = Db::open_in_memory().unwrap();
        let t0 = t(29);
        db.set_setting(SETTING_DERNIERE_TENTATIVE, &(t0 + min(60 * 24 * 30)).to_rfc3339()).unwrap();
        assert!(reserver_creneau(&db, t0).unwrap());
        // La date du futur a été remplacée par l'heure réelle : le plancher repart de là.
        assert!(!reserver_creneau(&db, t0 + min(1)).unwrap());
        db.set_setting(SETTING_DERNIERE_TENTATIVE, "n'importe quoi").unwrap();
        assert!(reserver_creneau(&db, t0 + min(2)).unwrap());
    }

    /// Vecteur d'interopérabilité produit par `scripts/signer_config.py signer`
    /// (clé de TEST de graine [7; 32], publique) : le Rust doit l'accepter.
    #[test]
    fn vecteur_signe_par_le_script_python() {
        const CONFIG: &[u8] = include_bytes!("testdata/config-relais/config.json");
        const SIG: &[u8] = include_bytes!("testdata/config-relais/config.sig");
        let ev = evaluer(CONFIG, SIG, &cles_test(), t(29), None).expect("vecteur Python refusé");
        assert_eq!(ev.key_id, "test-a");
        assert_eq!(ev.config.serial, 1);
        assert!(ev.config.extension.is_some());
    }
}

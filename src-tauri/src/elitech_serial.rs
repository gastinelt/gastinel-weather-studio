//! Lecture **directe** du RC-5 par son port série, sans ElitechLog ni fichier.
//!
//! ┌───────────────────────────────────────────────────────────────────────────┐
//! │  Ce module ne remplace pas `elitech_xlsx.rs` : il le double.              │
//! │  Les deux voies convergent vers les mêmes `RawMeasurement`, et donc vers  │
//! │  le même pipeline d'import (empreinte, doublons, anomalies).              │
//! └───────────────────────────────────────────────────────────────────────────┘
//!
//! ## Ce qu'est le RC-5 vu du PC
//!
//! Un adaptateur **USB-série CH340** (QinHeng Electronics, `1a86:7523`), donc un
//! `COMx` sous Windows. Le VID:PID ne suffit **pas** à conclure qu'un RC-5 est
//! branché : la même puce équipe la plupart des clones Arduino. C'est pourquoi
//! l'identification se fait en deux temps — filtre sur le VID:PID, puis une
//! commande `DevInfo` dont la réponse doit annoncer `model_no = 50`.
//!
//! ## Le protocole
//!
//! Il n'est **pas documenté officiellement**. Le point de départ est la
//! bibliothèque Python non officielle `elitech-datareader`
//! (github.com/civic/elitech-datareader), obtenue par rétro-ingénierie pour les
//! RC-4 / RC-4HC / RC-5. Chaque hypothèse qu'elle porte a été **revérifiée
//! contre le logger réel** (n° `EFE205004333`, 10 307 mesures) avant d'être
//! écrite ici ; les trames de cette vérification sont committées dans
//! `docs/samples/rc5_direct/` et rejouées par le test d'intégration
//! `tests/rc5_direct.rs`.
//!
//! Liaison : **115200 bauds, 8 bits, sans parité, 1 bit d'arrêt**, sans contrôle
//! de flux. Chaque échange est une requête de 5 octets suivie d'une réponse de
//! longueur **connue à l'avance** — il n'y a ni délimiteur ni longueur annoncée,
//! donc lire « ce qui vient » ne marche pas : il faut lire exactement le nombre
//! d'octets attendu.
//!
//! | Requête | Octets | Réponse |
//! |---|---|---|
//! | `Init` | `CC 00 0A 00 D6` | 3 octets (`55 A5 FA` sur le RC-5 testé) |
//! | `DevInfo` | `CC 00 06 00 D2` | 160 octets |
//! | `DataHeader` | `33 <station> 01 00 <somme>` | 11 octets |
//! | `DataBody` | `33 <station> 02 <page> <somme>` | `2 × mesures + 2` octets |
//!
//! La `<somme>` est la somme de tous les octets précédents modulo 256. Les
//! réponses portent la même somme en dernier octet — elle est **vérifiée** ici,
//! alors que la bibliothèque Python l'ignore : c'est le seul garde-fou contre
//! une trame tronquée par un câble douteux, et il ne coûte rien.
//!
//! ## Le point le plus important : les horodatages n'existent pas dans l'appareil
//!
//! Le RC-5 ne stocke **que des températures** — deux octets par mesure, rien
//! d'autre. L'instant de chaque mesure est *reconstruit* :
//!
//! ```text
//! instant(n) = start_time + n × intervalle          (n commençant à 0)
//! ```
//!
//! `start_time` et l'intervalle viennent de l'appareil. ElitechLog fait
//! exactement le même calcul — la validation croisée le prouve : sur 10 307
//! mesures couvrant 107 jours, **aucun** écart d'horodatage avec l'export xlsx.
//!
//! Conséquence à garder en tête : cette reconstruction suppose une cadence
//! parfaitement régulière du premier au dernier enregistrement. Un logger arrêté
//! puis relancé sans effacement, ou un démarrage différé, la mettrait en défaut —
//! **et l'export xlsx serait faux de la même façon**, puisqu'il applique le même
//! calcul. Ce n'est donc pas un défaut de cette voie-ci, mais une limite du
//! matériel, signalée par un avertissement quand l'état du logger le laisse
//! craindre (voir [`ReadWarningKind`]).
//!
//! ## Ce que la voie directe ne donne pas, contrairement au xlsx
//!
//! Le numéro de série complet. Le champ `dev_num` de `DevInfo` fait **10 octets**
//! et contient `E205004333`, alors que la feuille de résumé du xlsx annonce
//! `EFE205004333` — deux caractères de plus. Le champ est trop court pour le
//! numéro entier : ElitechLog connaît un préfixe que l'appareil ne transmet pas.
//! Voir [`serials_match`] ; le champ de saisie manuelle du numéro de série reste
//! disponible à l'import, comme pour les autres sources.
//!
//! ## Ce qui n'est **pas** supporté
//!
//! * Le **RC-5+** : modèle différent, non couvert par la référence utilisée et
//!   non testé ici. Un `model_no` inattendu est refusé explicitement plutôt que
//!   décodé au hasard.
//! * L'**écriture de configuration** : réglage d'horloge, numéro de station,
//!   seuils d'alarme. Le protocole les prévoit ; ce module ne les implémente
//!   pas. Une application de relevé n'a aucune raison de reprogrammer l'appareil
//!   qu'elle lit — c'est le travail d'ElitechLog.
//!
//! ## La seule exception : l'effacement de la mémoire
//!
//! [`clear_memory`] est la **seule** commande d'écriture de ce module, et la
//! seule dont le comportement n'ait pas été vérifié sur le logger réel. Elle ne
//! configure rien : elle remet le boîtier à zéro pour la campagne suivante,
//! c'est-à-dire le geste qui suit un relevé. Tout ce qui l'encadre — pourquoi la
//! requête renvoie à l'appareil sa propre fiche d'identité, et comment le
//! résultat est vérifié plutôt que supposé — est documenté sur
//! [`CMD_PARAM_PUT`]. **À lire avant d'y toucher.**

#[cfg(not(target_os = "android"))]
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

use crate::anomaly::{detect_jumps_raw, JumpThresholds, TemperatureJump};
use crate::elitech_import::RawMeasurement;
use crate::parsing::DateOrderEvidence;

// ---------------------------------------------------------------------------
// Constantes du protocole et du matériel
// ---------------------------------------------------------------------------

/// QinHeng Electronics — le fabricant de la puce CH340.
pub const CH340_VID: u16 = 0x1a86;
/// CH340 / CH341 en mode série.
pub const CH340_PID: u16 = 0x7523;

/// Débit de la liaison. Non négociable : l'appareil ne répond à rien d'autre.
pub const BAUD_RATE: u32 = 115_200;

/// `model_no` annoncé par un RC-5. Le RC-4 vaut 40, le RC-4HC 42 — ni l'un ni
/// l'autre n'est supporté ici, faute d'avoir pu les tester.
pub const RC5_MODEL_NO: u8 = 50;

/// Mesures par page pour un RC-5. C'est une propriété du **modèle**, pas un
/// choix : demander une page plus grande ferait répondre l'appareil sur une
/// longueur qu'on ne saurait pas prédire.
pub const RC5_PAGE_SIZE: usize = 500;

/// Longueur de la réponse à `DevInfo`.
pub const DEV_INFO_LEN: usize = 160;
/// Longueur de la réponse à `DataHeader`.
pub const DATA_HEADER_LEN: usize = 11;
/// Longueur de la réponse à `Init`.
pub const INIT_LEN: usize = 3;

/// Premier octet de toutes les réponses observées.
const MARQUEUR: u8 = 0x55;

/// Délai d'attente d'une réponse. 5 s comme la bibliothèque de référence : une
/// page de 1002 octets à 115200 bauds prend moins de 100 ms, la marge ne couvre
/// que le temps de réaction de l'appareil.
pub const TIMEOUT: Duration = Duration::from_millis(5_000);

/// Délai d'attente réduit pour la **seule** commande d'identification.
///
/// Elle est envoyée à des ports dont on ne sait pas encore s'ils portent un
/// RC-5. Sur un CH340 qui n'en est pas un (un Arduino), personne ne répond :
/// attendre 5 s figerait la détection d'autant. 1,5 s suffit très largement — le
/// RC-5 réel répond en quelques dizaines de millisecondes.
pub const TIMEOUT_IDENTIFICATION: Duration = Duration::from_millis(1_500);

// ---------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------

/// Échecs de la voie série. Les messages sont rédigés pour l'utilisateur final,
/// comme ceux d'`InfoclimatError` — ils disent quoi faire, pas seulement ce qui
/// s'est passé.
#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize, Deserialize)]
pub enum SerialError {
    #[error("Impossible d'énumérer les ports série : {0}")]
    Enumeration(String),

    #[error(
        "Aucun logger RC-5 n'est branché. Branchez-le en USB avec son câble de données \
         (certains câbles ne portent que l'alimentation), puis réessayez."
    )]
    NoDevice,

    #[error(
        "Le port {port} est déjà utilisé par un autre programme. La cause la plus \
         probable est ElitechLog resté ouvert : un seul logiciel à la fois peut parler \
         au logger. Fermez ElitechLog — y compris son icône dans la zone de \
         notification — puis réessayez."
    )]
    PortBusy { port: String },

    #[error("Le port {port} a disparu — le logger a-t-il été débranché ?")]
    PortGone { port: String },

    #[error("Ouverture du port {port} impossible : {message}")]
    OpenFailed { port: String, message: String },

    #[error("Échec de la communication sur le port {port} : {message}")]
    WriteFailed { port: String, message: String },

    #[error(
        "Le logger n'a pas répondu dans le temps imparti ({expected} octets attendus, \
         {received} reçus). Débranchez puis rebranchez-le, et vérifiez qu'ElitechLog \
         n'est pas ouvert."
    )]
    Timeout { expected: usize, received: usize },

    #[error(
        "Réponse incohérente du logger : {0}. Il ne s'agit peut-être pas d'un RC-5 — \
         d'autres appareils utilisent la même puce USB (CH340)."
    )]
    BadFrame(String),

    #[error(
        "Somme de contrôle incorrecte (attendue {expected:#04X}, reçue {actual:#04X}). \
         La trame est arrivée abîmée : essayez un autre câble ou un autre port USB."
    )]
    BadChecksum { expected: u8, actual: u8 },

    #[error(
        "Cet appareil annonce le modèle n° {model_no}, qui n'est pas un RC-5 (n° 50). \
         Seul le RC-5 est supporté par la lecture directe ; pour un autre modèle — \
         RC-5+ compris — passez par un export xlsx d'ElitechLog."
    )]
    UnsupportedModel { model_no: u8 },

    #[error(
        "Le logger ne contient aucune mesure : il n'a jamais été démarré, ou sa mémoire \
         vient d'être effacée."
    )]
    NoRecords,

    #[error(
        "Le logger n'annonce pas d'instant de départ exploitable ({0}) : les horodatages \
         ne peuvent pas être reconstruits. Passez par un export xlsx d'ElitechLog."
    )]
    NoStartTime(String),

    #[error(
        "Le logger annonce un intervalle d'enregistrement nul : les horodatages ne \
         peuvent pas être reconstruits."
    )]
    NoInterval,

    #[error(
        "La réinitialisation n'a pas été appliquée : le logger annonce encore {remaining} mesure(s) en mémoire. Rien n'a été effacé. Un enregistrement en cours n'est pourtant plus un obstacle — l'appareil est arrêté avant l'effacement. Si l'échec persiste, effacez sa mémoire depuis ElitechLog."
    )]
    ResetNotApplied { remaining: u16 },

    #[error(
        "Confirmation absente ou incorrecte : la réinitialisation n'a pas été envoyée,          et la mémoire du logger est intacte."
    )]
    ResetNotConfirmed,

    #[error(
        "L'arrêt n'a pas été pris en compte : le logger annonce toujours l'état          « {etat} ». Ses mesures n'ont pas été touchées."
    )]
    StopNotApplied { etat: String },
}

#[cfg(not(target_os = "android"))]
fn port_encore_present(port: &str) -> bool {
    serialport::available_ports()
        .map(|ports| ports.iter().any(|p| p.port_name == port))
        .unwrap_or(false)
}

#[cfg(not(target_os = "android"))]
fn traduire_ouverture(port: &str, e: &serialport::Error) -> SerialError {
    use serialport::ErrorKind;
    match e.kind() {
        // Conservé bien qu'inatteignable sous Windows : les autres systèmes
        // remontent bien un refus de permission, et rien ne garantit que la
        // crate gardera son repli actuel.
        ErrorKind::Io(std::io::ErrorKind::PermissionDenied) => SerialError::PortBusy {
            port: port.to_string(),
        },
        ErrorKind::NoDevice => {
            if port_encore_present(port) {
                SerialError::PortBusy {
                    port: port.to_string(),
                }
            } else {
                SerialError::PortGone {
                    port: port.to_string(),
                }
            }
        }
        _ => SerialError::OpenFailed {
            port: port.to_string(),
            message: e.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Trames — construction
// ---------------------------------------------------------------------------

/// Somme de contrôle du protocole : somme des octets modulo 256.
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b))
}

/// `CC 00 0A 00 D6` — poignée de main. La somme y est figée : la requête est
/// constante, il n'y a rien à calculer à l'exécution.
pub fn init_request() -> [u8; 5] {
    [0xCC, 0x00, 0x0A, 0x00, 0xD6]
}

/// `CC 00 06 00 D2` — demande la fiche d'identité de l'appareil.
pub fn dev_info_request() -> [u8; 5] {
    [0xCC, 0x00, 0x06, 0x00, 0xD2]
}

/// `33 <station> 01 00 <somme>` — nombre de mesures et instant de départ.
pub fn data_header_request(station_no: u8) -> [u8; 5] {
    let mut t = [0x33, station_no, 0x01, 0x00, 0x00];
    t[4] = checksum(&t[..4]);
    t
}

/// `33 <station> 02 <page> <somme>` — une page de mesures.
pub fn data_body_request(station_no: u8, page: u8) -> [u8; 5] {
    let mut t = [0x33, station_no, 0x02, page, 0x00];
    t[4] = checksum(&t[..4]);
    t
}

// ---------------------------------------------------------------------------
// Trames — décodage
// ---------------------------------------------------------------------------

/// Vérifie marqueur et somme de contrôle d'une réponse.
fn verifier_trame(trame: &[u8], quoi: &str) -> Result<(), SerialError> {
    if trame.len() < 2 {
        return Err(SerialError::BadFrame(format!("{quoi} : trame trop courte")));
    }
    if trame[0] != MARQUEUR {
        return Err(SerialError::BadFrame(format!(
            "{quoi} : premier octet {:#04X} au lieu de {MARQUEUR:#04X}",
            trame[0]
        )));
    }
    let attendue = checksum(&trame[..trame.len() - 1]);
    let recue = trame[trame.len() - 1];
    if attendue != recue {
        return Err(SerialError::BadChecksum {
            expected: attendue,
            actual: recue,
        });
    }
    Ok(())
}

/// Les sept octets d'un horodatage : année sur 2 octets (gros-boutiste), puis
/// mois, jour, heure, minute, seconde.
///
/// Renvoie `None` — et non une erreur — quand les champs ne forment pas une date
/// valide. Ce n'est pas théorique : sur le logger de référence, `last_online`
/// contenait `07 EA 08 13 14 81 1F`, soit « 129 minutes ». Un champ de date
/// aberrant dans une zone qu'on n'exploite pas ne doit pas faire échouer une
/// lecture par ailleurs parfaite.
fn decoder_horodatage(b: &[u8]) -> Option<NaiveDateTime> {
    if b.len() != 7 {
        return None;
    }
    let annee = i32::from(u16::from_be_bytes([b[0], b[1]]));
    let date = NaiveDate::from_ymd_opt(annee, u32::from(b[2]), u32::from(b[3]))?;
    let heure = NaiveTime::from_hms_opt(u32::from(b[4]), u32::from(b[5]), u32::from(b[6]))?;
    Some(date.and_time(heure))
}

/// État de fonctionnement annoncé par l'appareil.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkStatus {
    /// Jamais démarré.
    NotStarted,
    /// Enregistrement en cours.
    Running,
    /// Arrêté — l'état normal d'un logger qu'on vient de relever.
    Stopped,
    /// Démarrage différé programmé, pas encore commencé.
    DelayedStart,
    /// Valeur inattendue, conservée telle quelle plutôt que ramenée de force.
    Unknown(u8),
}

impl WorkStatus {
    fn from_byte(b: u8) -> Self {
        match b {
            0x00 => WorkStatus::NotStarted,
            0x01 => WorkStatus::Running,
            0x02 => WorkStatus::Stopped,
            0x03 => WorkStatus::DelayedStart,
            other => WorkStatus::Unknown(other),
        }
    }

    pub fn label_fr(self) -> String {
        match self {
            WorkStatus::NotStarted => "jamais démarré".to_string(),
            WorkStatus::Running => "enregistrement en cours".to_string(),
            WorkStatus::Stopped => "arrêté".to_string(),
            WorkStatus::DelayedStart => "démarrage différé programmé".to_string(),
            WorkStatus::Unknown(b) => format!("état inconnu ({b:#04X})"),
        }
    }
}

/// Fiche d'identité lue dans la réponse `DevInfo`.
///
/// Les champs conservés sont ceux qui servent : identifier l'appareil, savoir
/// combien de mesures lire, reconstruire leurs instants. Les réglages d'alarme
/// sonore ou de bouton d'arrêt sont volontairement laissés de côté —
/// l'application ne les affiche nulle part et ne les modifie jamais.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevInfo {
    /// Numéro de station : l'adresse de l'appareil sur le bus. Vaut 1 en usage
    /// courant, mais il est **relu**, jamais supposé — les requêtes de données
    /// le portent, et une valeur fausse ferait taire l'appareil.
    pub station_no: u8,
    /// 50 pour un RC-5.
    pub model_no: u8,
    /// Cadence d'enregistrement, en secondes (900 pour 15 minutes).
    pub sample_interval_seconds: i64,
    /// Nombre de mesures en mémoire.
    pub rec_count: u16,
    /// Instant de la première mesure, tel que l'appareil l'annonce. Naïf : c'est
    /// l'heure de son horloge interne, sans fuseau.
    pub start_time: Option<NaiveDateTime>,
    /// Horloge de l'appareil **au moment de la lecture**. C'est ce qui permet de
    /// mesurer sa dérive : comparée à l'heure du PC, elle donne directement la
    /// correction à saisir à l'import.
    pub device_clock: Option<NaiveDateTime>,
    /// Dernière connexion à un logiciel, telle qu'annoncée. Souvent aberrante
    /// (voir [`decoder_horodatage`]) — purement indicative.
    pub last_online: Option<NaiveDateTime>,
    pub work_status: WorkStatus,
    /// Numéro de série sur 10 octets — **tronqué** par rapport au xlsx, voir la
    /// documentation du module.
    pub device_number: Option<String>,
    /// Champ libre ; vaut `"RC-5 Data Logger"` par défaut, et se retrouve dans le
    /// xlsx sous « Description du voyage ».
    pub user_info: Option<String>,
    /// Seuils d'alarme configurés, en °C.
    pub upper_limit_celsius: f64,
    pub lower_limit_celsius: f64,
    /// Correction que l'appareil applique à ses propres mesures, en °C.
    pub temperature_calibration: f64,
    /// `true` si l'appareil est réglé en Celsius. **Les valeurs transmises
    /// restent en dixièmes de °C** quel que soit ce réglage : il ne change que
    /// l'affichage sur l'écran du logger. Conservé pour le signaler à
    /// l'utilisateur, jamais pour convertir quoi que ce soit.
    pub celsius: bool,
}

impl DevInfo {
    /// Modèle en clair, pour l'affichage.
    pub fn model_label(&self) -> String {
        match self.model_no {
            40 => "RC-4".to_string(),
            42 => "RC-4HC".to_string(),
            RC5_MODEL_NO => "RC-5".to_string(),
            other => format!("modèle n° {other}"),
        }
    }

    /// Dérive de l'horloge du logger par rapport à une référence, en secondes.
    ///
    /// Positif : le logger est **en avance**. `clock_offset_seconds` porte
    /// l'opposé (il s'ajoute à l'horodatage brut) — la conversion est laissée à
    /// l'UI, qui la combine déjà avec le fuseau choisi.
    pub fn clock_drift_seconds(&self, reference: NaiveDateTime) -> Option<i64> {
        self.device_clock.map(|h| (h - reference).num_seconds())
    }
}

/// Décode les 160 octets de `DevInfo`.
pub fn decode_dev_info(trame: &[u8]) -> Result<DevInfo, SerialError> {
    if trame.len() != DEV_INFO_LEN {
        return Err(SerialError::BadFrame(format!(
            "DevInfo : {} octets au lieu de {DEV_INFO_LEN}",
            trame.len()
        )));
    }
    verifier_trame(trame, "DevInfo")?;

    // Décalages relevés sur l'appareil réel, puis revérifiés champ par champ
    // contre la feuille de résumé du xlsx du **même** logger.
    let interval_h = i64::from(trame[5]);
    let interval_m = i64::from(trame[6]);
    let interval_s = i64::from(trame[7]);

    let texte = |debut: usize, longueur: usize| -> Option<String> {
        // Le champ est complété de zéros après la valeur. L'appareil n'y met que
        // de l'ASCII, mais `from_utf8_lossy` évite qu'un octet parasite dans un
        // champ purement descriptif fasse échouer toute la lecture.
        let s = String::from_utf8_lossy(&trame[debut..debut + longueur])
            .trim_end_matches('\0')
            .trim()
            .to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };

    Ok(DevInfo {
        station_no: trame[1],
        model_no: trame[3],
        sample_interval_seconds: interval_h * 3600 + interval_m * 60 + interval_s,
        rec_count: u16::from_be_bytes([trame[29], trame[30]]),
        start_time: decoder_horodatage(&trame[20..27]),
        device_clock: decoder_horodatage(&trame[31..38]),
        last_online: decoder_horodatage(&trame[12..19]),
        work_status: WorkStatus::from_byte(trame[19]),
        device_number: texte(138, 10),
        user_info: texte(38, 100),
        upper_limit_celsius: f64::from(i16::from_be_bytes([trame[8], trame[9]])) / 10.0,
        lower_limit_celsius: f64::from(i16::from_be_bytes([trame[10], trame[11]])) / 10.0,
        temperature_calibration: f64::from(trame[152] as i8) / 10.0,
        // 0x31 = Celsius, 0x13 = Fahrenheit — deux valeurs miroir, comme tous
        // les drapeaux de ce protocole.
        celsius: trame[151] != 0x13,
    })
}

/// Ce que l'en-tête de données annonce.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataHeader {
    pub rec_count: u16,
    pub start_time: Option<NaiveDateTime>,
}

/// Décode les 11 octets de `DataHeader`.
pub fn decode_data_header(trame: &[u8]) -> Result<DataHeader, SerialError> {
    if trame.len() != DATA_HEADER_LEN {
        return Err(SerialError::BadFrame(format!(
            "DataHeader : {} octets au lieu de {DATA_HEADER_LEN}",
            trame.len()
        )));
    }
    verifier_trame(trame, "DataHeader")?;
    Ok(DataHeader {
        rec_count: u16::from_be_bytes([trame[1], trame[2]]),
        start_time: decoder_horodatage(&trame[3..10]),
    })
}

/// Décode une page de mesures : `count` entiers signés de 16 bits, en dixièmes
/// de degré, gros-boutistes.
///
/// Le signe compte, et il est la matière même du projet : les températures
/// négatives arrivent en complément à deux sur **16** bits (`FF F5` pour
/// −1,1 °C). Une lecture qui ne traiterait que l'octet de poids faible lirait
/// +24,5 °C — l'erreur exacte qu'a commise la première sonde d'exploration, et
/// que la validation croisée a rattrapée.
pub fn decode_data_page(trame: &[u8], count: usize) -> Result<Vec<i16>, SerialError> {
    let attendu = count * 2 + 2;
    if trame.len() != attendu {
        return Err(SerialError::BadFrame(format!(
            "DataBody : {} octets au lieu de {attendu}",
            trame.len()
        )));
    }
    verifier_trame(trame, "DataBody")?;
    Ok((0..count)
        .map(|i| i16::from_be_bytes([trame[1 + i * 2], trame[2 + i * 2]]))
        .collect())
}

/// Découpage de la lecture en pages : `(numéro de page, nombre de mesures)`.
///
/// La dernière page est partielle, et sa longueur doit être calculée **avant**
/// de lire : la réponse n'annonce pas sa taille, on lit exactement ce qu'on a
/// prédit. Un `rec_count` multiple exact de la taille de page ne produit pas de
/// page vide finale.
pub fn page_plan(rec_count: usize, page_size: usize) -> Vec<(u8, usize)> {
    if rec_count == 0 || page_size == 0 {
        return Vec::new();
    }
    let pages = rec_count.div_ceil(page_size);
    (0..pages)
        .map(|p| {
            let restant = rec_count - p * page_size;
            (p as u8, restant.min(page_size))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Des dixièmes de degré aux mesures du modèle
// ---------------------------------------------------------------------------

/// Nature d'un avertissement de lecture directe.
///
/// Séparée du message pour que l'UI puisse traiter les cas différemment sans
/// relire du texte français, et pour que les tests portent sur la nature du
/// problème, pas sur sa formulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadWarningKind {
    /// L'appareil enregistre encore : la dernière mesure peut être en cours.
    StillRunning,
    /// `DevInfo` et `DataHeader` n'annoncent pas le même nombre de mesures.
    RecordCountMismatch,
    /// …ni le même instant de départ.
    StartTimeMismatch,
    /// L'appareil applique une correction interne à ses mesures.
    CalibrationApplied,
    /// L'appareil est réglé en Fahrenheit (l'affichage seulement).
    FahrenheitDisplay,
}

/// Avertissement de lecture — signalé, jamais bloquant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadWarning {
    pub kind: ReadWarningKind,
    pub message: String,
}

/// Tout ce qu'une lecture directe a livré.
///
/// Volontairement calqué sur [`crate::elitech_xlsx::ElitechXlsxExport`] : mêmes
/// `measurements`, même `date_order`, mêmes `anomalies`. C'est ce qui permet aux
/// deux voies de rejoindre `persist_import` sans chemin d'insertion parallèle.
#[derive(Debug, Clone, Serialize)]
pub struct Rc5Reading {
    pub device: DevInfo,
    pub header: DataHeader,
    pub measurements: Vec<RawMeasurement>,
    pub warnings: Vec<ReadWarning>,
    /// Toujours une preuve « ISO » : les instants sont *calculés*, pas lus dans
    /// un texte — il n'y a aucun ordre jour/mois à deviner.
    pub date_order: DateOrderEvidence,
    pub anomalies: Vec<TemperatureJump>,
}

impl Rc5Reading {
    /// Numéro de série à proposer à l'import : celui de l'appareil, tronqué.
    pub fn logger_serial(&self) -> Option<String> {
        self.device.device_number.clone()
    }
}

/// Reconstruit les instants et fabrique les mesures.
///
/// `source_line` porte le **numéro d'enregistrement** (1-based), c'est-à-dire la
/// colonne `No.` du xlsx — et non son numéro de ligne dans la feuille, qui vaut
/// `No. + 1` à cause de l'en-tête. C'est la seule référence qui ait un sens ici :
/// il n'y a pas de fichier.
pub fn build_measurements(
    start: NaiveDateTime,
    interval_seconds: i64,
    valeurs: &[i16],
) -> Vec<RawMeasurement> {
    let pas = chrono::Duration::seconds(interval_seconds);
    valeurs
        .iter()
        .enumerate()
        .map(|(i, dixiemes)| RawMeasurement {
            raw_timestamp: start + pas * (i as i32),
            temperature_celsius: f64::from(*dixiemes) / 10.0,
            // Le RC-5 n'a pas de capteur d'humidité (c'est le RC-4HC qui en a
            // un). `None`, et non `0.0` : une valeur absente n'est pas une
            // valeur nulle — et c'est aussi ce que produit le parseur xlsx, ce
            // qui conditionne l'égalité des empreintes entre les deux voies.
            humidity_percent: None,
            source_line: i + 1,
        })
        .collect()
}

/// Assemble une lecture complète à partir des trames brutes.
///
/// Cette fonction ne touche **pas** au port série : elle prend les octets déjà
/// reçus. C'est ce qui permet au test d'intégration de rejouer une capture réelle
/// sans matériel, et à la lecture réelle de partager exactement le même code de
/// décodage que ce test.
pub fn decode_reading(
    dev_info_frame: &[u8],
    data_header_frame: &[u8],
    pages_frame: &[u8],
    thresholds: &JumpThresholds,
) -> Result<Rc5Reading, SerialError> {
    let device = decode_dev_info(dev_info_frame)?;
    if device.model_no != RC5_MODEL_NO {
        return Err(SerialError::UnsupportedModel {
            model_no: device.model_no,
        });
    }
    let header = decode_data_header(data_header_frame)?;

    let mut warnings = Vec::new();
    if device.rec_count != header.rec_count {
        warnings.push(ReadWarning {
            kind: ReadWarningKind::RecordCountMismatch,
            message: format!(
                "L'appareil annonce {} mesure(s) dans sa fiche d'identité mais {} dans \
                 l'en-tête de données. C'est l'en-tête qui fait foi ici.",
                device.rec_count, header.rec_count
            ),
        });
    }
    if device.start_time.is_some()
        && header.start_time.is_some()
        && device.start_time != header.start_time
    {
        warnings.push(ReadWarning {
            kind: ReadWarningKind::StartTimeMismatch,
            message: format!(
                "Deux instants de départ différents sont annoncés ({}, puis {}). Les \
                 horodatages sont reconstruits à partir de celui de l'en-tête de \
                 données ; vérifiez-les avant d'importer.",
                horodatage_ou_tiret(device.start_time),
                horodatage_ou_tiret(header.start_time),
            ),
        });
    }
    if device.work_status == WorkStatus::Running {
        warnings.push(ReadWarning {
            kind: ReadWarningKind::StillRunning,
            message: "Le logger enregistre encore. La dernière mesure lue peut être \
                      incomplète, et une relecture donnera un lot différent — donc une \
                      nouvelle série, que la détection de doublon ne signalera pas. \
                      Arrêtez le logger avant de relever si vous voulez un lot figé."
                .to_string(),
        });
    }
    if device.temperature_calibration != 0.0 {
        warnings.push(ReadWarning {
            kind: ReadWarningKind::CalibrationApplied,
            message: format!(
                "Le logger applique une correction interne de {:+.1} °C à ses mesures. \
                 Elle est déjà comprise dans les valeurs lues, ici comme dans un export \
                 ElitechLog — aucune correction supplémentaire n'est à saisir.",
                device.temperature_calibration
            ),
        });
    }
    if !device.celsius {
        warnings.push(ReadWarning {
            kind: ReadWarningKind::FahrenheitDisplay,
            message: "Le logger est réglé en Fahrenheit. Cela ne concerne que son écran : \
                      les valeurs transmises restent en dixièmes de degré Celsius, et \
                      sont lues comme telles."
                .to_string(),
        });
    }

    // L'en-tête de données fait foi sur les deux champs qui pilotent la lecture :
    // c'est la réponse à la commande qui précède immédiatement le transfert.
    let rec_count = usize::from(header.rec_count);
    if rec_count == 0 {
        return Err(SerialError::NoRecords);
    }
    let start = header
        .start_time
        .or(device.start_time)
        .ok_or_else(|| SerialError::NoStartTime(hexa(&data_header_frame[3..10])))?;
    if device.sample_interval_seconds <= 0 {
        return Err(SerialError::NoInterval);
    }

    // Les pages sont concaténées bout à bout : leurs longueurs se déduisent du
    // plan, aucune n'annonce la sienne.
    let mut valeurs = Vec::with_capacity(rec_count);
    let mut offset = 0usize;
    for (page, count) in page_plan(rec_count, RC5_PAGE_SIZE) {
        let longueur = count * 2 + 2;
        let fin = offset + longueur;
        if fin > pages_frame.len() {
            return Err(SerialError::BadFrame(format!(
                "page {page} : {} octets disponibles, {longueur} attendus",
                pages_frame.len().saturating_sub(offset)
            )));
        }
        valeurs.extend(decode_data_page(&pages_frame[offset..fin], count)?);
        offset = fin;
    }

    let measurements = build_measurements(start, device.sample_interval_seconds, &valeurs);
    let anomalies = detect_jumps_raw(&measurements, thresholds);

    Ok(Rc5Reading {
        date_order: DateOrderEvidence::iso(measurements.len()),
        device,
        header,
        measurements,
        warnings,
        anomalies,
    })
}

fn horodatage_ou_tiret(d: Option<NaiveDateTime>) -> String {
    d.map(|d| d.to_string()).unwrap_or_else(|| "—".to_string())
}

fn hexa(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Deux numéros de série désignent-ils le même logger ?
///
/// L'appareil transmet `E205004333` là où ElitechLog écrit `EFE205004333` : le
/// champ du protocole fait 10 octets, deux de moins qu'il n'en faudrait. On
/// accepte donc qu'un numéro soit le **suffixe** de l'autre.
///
/// C'est un rapprochement d'affichage, pas une clé : la détection de doublon
/// repose sur l'empreinte des mesures (`fingerprint.rs`), qui, elle, ne dépend
/// d'aucun numéro de série.
pub fn serials_match(a: &str, b: &str) -> bool {
    let a = a.trim().to_ascii_uppercase();
    let b = b.trim().to_ascii_uppercase();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.ends_with(&b) || b.ends_with(&a)
}

// ---------------------------------------------------------------------------
// Transport : le port série lui-même
// ---------------------------------------------------------------------------

/// Un canal capable d'envoyer une requête et de lire une réponse de longueur
/// connue.
///
/// Cette abstraction n'existe que pour une raison : permettre de tester la
/// **séquence** des échanges (identification, en-tête, pagination) sans matériel,
/// en rejouant une capture. Le décodage, lui, est déjà testable sans elle — il ne
/// prend que des `&[u8]`.
pub trait Rc5Transport {
    fn exchange(&mut self, request: &[u8], expected: usize) -> Result<Vec<u8>, SerialError>;

    /// Émet une requête **sans lire de réponse**.
    ///
    /// Nécessaire parce que [`Rc5Transport::exchange`] exige de connaître à
    /// l'avance la longueur de la réponse — le protocole ne l'annonce nulle part
    /// (voir l'en-tête du module). Cette longueur n'est connue que pour les
    /// commandes observées sur le matériel, et l'écriture de paramètres
    /// ([`param_put_request`]) n'en fait pas partie : c'est la seule commande qui
    /// emprunte ce chemin.
    ///
    /// Ne rien lire n'est pas une approximation : la réponse éventuelle est
    /// évacuée par la fermeture du port qui suit, et la vérification se fait sur
    /// l'**état de l'appareil relu ensuite**, pas sur un accusé de réception.
    /// Voir [`clear_memory`].
    fn send(&mut self, request: &[u8]) -> Result<(), SerialError>;

    /// Termine la session en cours et en ouvre une neuve.
    ///
    /// Voir [`SESSION_FERMEE_PAR_CC`] : ce n'est pas une commodité, c'est une
    /// obligation du protocole.
    fn reset_session(&mut self) -> Result<(), SerialError>;
}

/// **Une commande `0xCC` clôt la session — plus rien ne répond ensuite.**
///
/// Constatation faite sur le logger réel, et qui n'apparaît nulle part dans la
/// bibliothèque de référence : celle-ci ouvre et referme le port autour de
/// *chaque* commande, si bien que la contrainte y est respectée sans jamais être
/// nommée. Le tableau des essais :
///
/// | Session | Enchaînement | Résultat |
/// |---|---|---|
/// | A | `DevInfo`, `DataHeader` | l'en-tête **ne répond pas** |
/// | B | `DevInfo` \| `DataHeader`, `DataBody`×2 | tout passe |
/// | D | `DevInfo`, `DevInfo` | le second **ne répond pas** |
/// | E | `DataHeader`, `DataHeader`, `DevInfo` | tout passe |
///
/// La règle n'est donc pas « une commande par session », ni une histoire de
/// familles de commandes : les requêtes `0x33` (`DataHeader`, `DataBody`)
/// s'enchaînent sans limite, et l'une d'elles peut même suivre l'autre
/// indéfiniment — mais une requête `0x cc` (`Init`, `DevInfo`) est **la dernière
/// que la session servira**.
///
/// Conséquence concrète : [`read_all`] lit la fiche d'identité, referme, puis
/// enchaîne l'en-tête et les 21 pages dans une seule seconde session. Deux
/// ouvertures au total, pas vingt-trois.
pub const SESSION_FERMEE_PAR_CC: () = ();

/// Temps de repos imposé à l'appareil entre la fermeture d'un port et sa
/// réouverture.
///
/// **Mesuré, pas supposé.** Rouvrir le port immédiatement après l'avoir fermé
/// laisse le RC-5 muet : il accepte la requête et ne répond jamais. La sonde de
/// mesure (trois séries d'essais sur le logger réel) donne 0 ms en échec
/// systématique et 50 ms en succès systématique ; le temps de réponse, lui, ne
/// bouge pas (186 ms dans tous les cas).
///
/// La valeur retenue est celle de la bibliothèque de référence — 500 ms — et non
/// le minimum observé : ce minimum a été relevé sur **un** appareil, et la marge
/// ne coûte rien puisque l'attente est ciblée (voir [`attendre_le_repos`]) et
/// qu'une lecture complète dure de toute façon plusieurs secondes.
///
/// C'est le seul comportement du protocole qui ne se lise pas dans les octets :
/// sans ce délai, l'enchaînement « identifier puis lire » — celui de l'import en
/// un clic — échoue à tous les coups.
pub const REPOS_APRES_FERMETURE: Duration = Duration::from_millis(500);

/// Instant de la dernière libération d'un port, tous ports confondus.
///
/// Global et non par port : la contrainte vient de l'appareil, et il n'y en a
/// qu'un de branché en pratique. Un `static` évite surtout d'avoir à faire
/// transiter cet état par toutes les couches appelantes pour un détail de
/// temporisation.
static DERNIERE_FERMETURE: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

fn verrou_fermeture() -> std::sync::MutexGuard<'static, Option<Instant>> {
    // Un verrou empoisonné signifie qu'un fil a paniqué en le tenant. La donnée
    // protégée n'est qu'un horodatage : la reprendre telle quelle est sans
    // risque, et bien préférable à une panique en cascade.
    DERNIERE_FERMETURE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Dort le temps qu'il reste à courir depuis la dernière fermeture, s'il en
/// reste. Le premier port ouvert dans le processus n'attend donc rien.
///
/// Vaut pour les deux transports : la contrainte vient du boîtier, pas du
/// système. Un RC-5 lu par USB-OTG a exactement le même besoin de repos qu'un
/// RC-5 lu par un port COM.
fn attendre_le_repos() {
    let reste = {
        let dernier = verrou_fermeture();
        dernier.and_then(|t| REPOS_APRES_FERMETURE.checked_sub(t.elapsed()))
    };
    if let Some(d) = reste {
        std::thread::sleep(d);
    }
}

/// Le vrai port série.
///
/// `port` est une `Option` pour une raison précise : [`Self::reset_session`] doit
/// **fermer avant d'ouvrir**. Sans ce `take()`, le port neuf serait demandé alors
/// que l'ancien est encore tenu — le système refuserait l'accès, et l'attente de
/// repos serait comptée depuis une fermeture qui n'a pas encore eu lieu.
#[cfg(not(target_os = "android"))]
pub struct SerialTransport {
    port: Option<Box<dyn serialport::SerialPort>>,
    port_name: String,
    timeout: Duration,
}

/// Le même transport, mais par l'USB-OTG d'Android.
///
/// Même forme et même contrat que ci-dessus — `Option` pour la même raison, et
/// le même repos de 500 ms entre fermeture et réouverture. Ce qui change est ce
/// qu'il y a dans la boîte : non plus un port série monté par le système, mais
/// une session ouverte à travers `UsbManager` par le greffon
/// `tauri-plugin-usbserial`, qui confie la puce CH340 à
/// `usb-serial-for-android`.
#[cfg(target_os = "android")]
pub struct SerialTransport {
    port: Option<tauri_plugin_usbserial::SerialPort>,
    port_name: String,
    timeout: Duration,
}

impl Drop for SerialTransport {
    fn drop(&mut self) {
        if self.port.is_some() {
            self.fermer();
        }
    }
}

impl SerialTransport {
    #[cfg(not(target_os = "android"))]
    /// Ouvre le port aux paramètres du RC-5.
    ///
    /// `dtr_on_open(false)` est délibéré. Le RC-5 ignore DTR, mais la même puce
    /// CH340 équipe les clones Arduino, sur lesquels un DTR levé provoque un
    /// **reset de la carte**. Comme la détection envoie une commande
    /// d'identification à tout CH340 pour savoir ce qu'il est, laisser DTR à sa
    /// valeur par défaut redémarrerait l'Arduino du voisin à chaque branchement.
    pub fn open(port_name: &str, timeout: Duration) -> Result<Self, SerialError> {
        attendre_le_repos();

        let port = serialport::new(port_name, BAUD_RATE)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .dtr_on_open(false)
            // Timeout court sur chaque appel système : `exchange` boucle et
            // applique lui-même le délai global. Un timeout long ici rendrait
            // toute lecture partielle indétectable avant son expiration.
            .timeout(Duration::from_millis(200))
            .open()
            .map_err(|e| traduire_ouverture(port_name, &e))?;

        Ok(SerialTransport {
            port: Some(port),
            port_name: port_name.to_string(),
            timeout,
        })
    }

    #[cfg(target_os = "android")]
    /// Ouvre la session USB, en 115200 8N1, DTR et RTS au repos.
    ///
    /// Le nom de port est ici un `usb:<identifiant>` fabriqué par
    /// [`list_ch340_ports`] : Android ne monte aucun `/dev/ttyUSB*` et n'a donc
    /// pas de nom à donner. Cette forme conserve un `port_name` unique à
    /// travers tout le code — commandes, événements, écrans — sans qu'aucune
    /// couche au-dessus ait à savoir de quel transport il s'agit.
    ///
    /// **Peut demander une autorisation à l'utilisateur**, et attendre sa
    /// réponse : cet appel ne doit jamais partir du fil principal.
    pub fn open(port_name: &str, timeout: Duration) -> Result<Self, SerialError> {
        let device_id = device_id_du_port(port_name)?;
        attendre_le_repos();

        let port = tauri_plugin_usbserial::SerialPort::open(device_id, BAUD_RATE).map_err(|e| {
            SerialError::OpenFailed {
                port: port_name.to_string(),
                message: e.to_string(),
            }
        })?;

        Ok(SerialTransport {
            port: Some(port),
            port_name: port_name.to_string(),
            timeout,
        })
    }

    /// Libère le port et note l'instant, pour que la prochaine ouverture laisse à
    /// l'appareil son temps de repos.
    fn fermer(&mut self) {
        drop(self.port.take());
        *verrou_fermeture() = Some(Instant::now());
    }
}

/// Extrait l'identifiant Android d'un nom de port `usb:<n>`.
///
/// Vérifié plutôt que supposé : un nom de port venu d'ailleurs (un `COM8`
/// recopié depuis une base partagée avec le desktop, par exemple) doit produire
/// un message clair, pas une ouverture au hasard sur l'appareil n° 0.
#[cfg(target_os = "android")]
fn device_id_du_port(port_name: &str) -> Result<i32, SerialError> {
    port_name
        .strip_prefix(PREFIXE_PORT_USB)
        .and_then(|n| n.parse::<i32>().ok())
        .ok_or_else(|| SerialError::OpenFailed {
            port: port_name.to_string(),
            message: format!(
                "Nom de port inattendu sur Android : « {port_name} ». Attendu                  « {PREFIXE_PORT_USB}<numéro> ». Débranchez et rebranchez le logger."
            ),
        })
}

/// Préfixe des noms de port fabriqués sur Android.
///
/// Android n'expose pas de nom de périphérique série : il n'a qu'un identifiant
/// numérique, **réattribué à chaque branchement**. Le préfixe rend la
/// provenance lisible dans les journaux et les messages d'erreur, où l'on voit
/// autrement passer des `COM8` et des `/dev/ttyUSB0`.
#[cfg(target_os = "android")]
pub const PREFIXE_PORT_USB: &str = "usb:";

impl Rc5Transport for SerialTransport {
    #[cfg(not(target_os = "android"))]
    fn exchange(&mut self, request: &[u8], expected: usize) -> Result<Vec<u8>, SerialError> {
        let port_name = self.port_name.clone();
        let timeout = self.timeout;
        let port = self.port.as_mut().ok_or_else(|| SerialError::WriteFailed {
            port: port_name.clone(),
            message: "session fermée".to_string(),
        })?;

        // Une réponse en retard de l'échange précédent fausserait tout l'alignement
        // des trames suivantes : on repart d'un tampon vide à chaque requête.
        let _ = port.clear(serialport::ClearBuffer::Input);

        eprintln!("[RC5-SERIAL] >>> REQ ({port_name}, {} o): {:02X?}", request.len(), request);

        port.write_all(request)
            .and_then(|()| port.flush())
            .map_err(|e| SerialError::WriteFailed {
                port: port_name.clone(),
                message: e.to_string(),
            })?;

        // Lecture par accumulation : rien dans le protocole n'annonce la
        // longueur, c'est l'appelant qui la connaît. Un `read` unique renverrait
        // le premier fragment arrivé, souvent bien plus court que la trame.
        let mut recu = vec![0u8; expected];
        let mut lus = 0usize;
        let debut = Instant::now();
        while lus < expected {
            if debut.elapsed() > timeout {
                eprintln!("[RC5-SERIAL] !!! TIMEOUT ({port_name}): attendus {expected}, reçus {lus}");
                return Err(SerialError::Timeout {
                    expected,
                    received: lus,
                });
            }
            match port.read(&mut recu[lus..]) {
                Ok(0) => {}
                Ok(n) => lus += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => {
                    eprintln!("[RC5-SERIAL] !!! ERR READ ({port_name}): {e}");
                    return Err(SerialError::WriteFailed {
                        port: port_name.clone(),
                        message: e.to_string(),
                    })
                }
            }
        }
        if recu.len() <= 32 {
            eprintln!("[RC5-SERIAL] <<< RESP ({port_name}, {} o): {:02X?}", recu.len(), recu);
        } else {
            eprintln!(
                "[RC5-SERIAL] <<< RESP ({port_name}, {} o): {:02X?} ... [{:02X?}]",
                recu.len(),
                &recu[..16],
                &recu[recu.len() - 4..]
            );
        }
        Ok(recu)
    }

    #[cfg(target_os = "android")]
    /// Même conversation, mêmes règles, transport différent.
    ///
    /// La structure est délibérément calquée sur celle du port série
    /// ci-dessus : vider l'entrée, écrire, puis **accumuler** jusqu'à la
    /// longueur attendue. Rien dans le protocole n'annonce la longueur d'une
    /// réponse, et un transfert USB rend ce que le contrôleur a bufferisé, pas
    /// ce qu'on a demandé — une lecture unique renverrait souvent une trame
    /// tronquée, exactement comme sur un port série.
    fn exchange(&mut self, request: &[u8], expected: usize) -> Result<Vec<u8>, SerialError> {
        let port_name = self.port_name.clone();
        let timeout = self.timeout;
        let port = self.port.as_mut().ok_or_else(|| SerialError::WriteFailed {
            port: port_name.clone(),
            message: "session fermée".to_string(),
        })?;

        let _ = port.purge();

        eprintln!("[RC5-USB] >>> REQ ({port_name}, {} o): {:02X?}", request.len(), request);

        port.write(request, timeout.as_millis() as u64)
            .map_err(|e| SerialError::WriteFailed {
                port: port_name.clone(),
                message: e.to_string(),
            })?;

        let mut recu: Vec<u8> = Vec::with_capacity(expected);
        let debut = Instant::now();
        while recu.len() < expected {
            let reste = match timeout.checked_sub(debut.elapsed()) {
                Some(d) if !d.is_zero() => d,
                _ => {
                    eprintln!("[RC5-USB] !!! TIMEOUT ({port_name}): attendus {expected}, reçus {}", recu.len());
                    return Err(SerialError::Timeout {
                        expected,
                        received: recu.len(),
                    })
                }
            };
            let morceau = port
                .read(expected - recu.len(), reste.as_millis() as u64)
                .map_err(|e| SerialError::WriteFailed {
                    port: port_name.clone(),
                    message: e.to_string(),
                })?;
            recu.extend_from_slice(&morceau);
        }
        if recu.len() <= 32 {
            eprintln!("[RC5-USB] <<< RESP ({port_name}, {} o): {:02X?}", recu.len(), recu);
        } else {
            eprintln!(
                "[RC5-USB] <<< RESP ({port_name}, {} o): {:02X?} ... [{:02X?}]",
                recu.len(),
                &recu[..16],
                &recu[recu.len() - 4..]
            );
        }
        Ok(recu)
    }

    #[cfg(not(target_os = "android"))]
    fn send(&mut self, request: &[u8]) -> Result<(), SerialError> {
        let port_name = self.port_name.clone();
        let port = self.port.as_mut().ok_or_else(|| SerialError::WriteFailed {
            port: port_name.clone(),
            message: "session fermée".to_string(),
        })?;
        eprintln!("[RC5-SERIAL] >>> SEND-NO-RESP ({port_name}, {} o): {:02X?}", request.len(), request);
        port.write_all(request)
            .and_then(|()| port.flush())
            .map_err(|e| SerialError::WriteFailed {
                port: port_name,
                message: e.to_string(),
            })
    }

    #[cfg(target_os = "android")]
    fn send(&mut self, request: &[u8]) -> Result<(), SerialError> {
        let port_name = self.port_name.clone();
        let timeout = self.timeout;
        let port = self.port.as_mut().ok_or_else(|| SerialError::WriteFailed {
            port: port_name.clone(),
            message: "session fermée".to_string(),
        })?;
        eprintln!("[RC5-USB] >>> SEND-NO-RESP ({port_name}, {} o): {:02X?}", request.len(), request);
        port.write(request, timeout.as_millis() as u64)
            .map_err(|e| SerialError::WriteFailed {
                port: port_name,
                message: e.to_string(),
            })
    }

    fn reset_session(&mut self) -> Result<(), SerialError> {
        eprintln!("[RC5-SESSION] === RESET SESSION ({}) ===", self.port_name);
        self.fermer();
        let mut neuf = SerialTransport::open(&self.port_name, self.timeout)?;
        // `neuf` cède son port et repart sans rien à fermer : sans ce `take`,
        // sa libération en fin de portée refermerait le port qu'on vient
        // d'installer, et noterait une fermeture qui n'a pas eu lieu.
        //
        // Identique sur les deux transports depuis que le port USB-OTG est un
        // vrai port : c'est ce qui permet au `0xCC` de fin de session d'être
        // suivi d'une réouverture, sur mobile comme sur desktop.
        self.port = neuf.port.take();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Séquences de haut niveau
// ---------------------------------------------------------------------------

/// Interroge la fiche d'identité et vérifie que c'est bien un RC-5.
///
/// C'est **l'étape de confirmation** évoquée en tête de module : elle transforme
/// « un CH340 est branché » en « un RC-5 n° X est branché ». Un appareil qui ne
/// répond pas, ou qui répond autre chose, ressort en erreur — jamais en
/// supposition.
pub fn identify<T: Rc5Transport>(transport: &mut T) -> Result<DevInfo, SerialError> {
    let info = decode_dev_info(&transport.exchange(&dev_info_request(), DEV_INFO_LEN)?)?;
    if info.model_no != RC5_MODEL_NO {
        return Err(SerialError::UnsupportedModel {
            model_no: info.model_no,
        });
    }
    Ok(info)
}

/// Lit l'historique complet : identité, en-tête, puis toutes les pages.
///
/// L'ordre n'est pas négociable — la taille de chaque réponse suivante se déduit
/// de la précédente. C'est aussi ce qui rend la validation « étape par étape »
/// possible : chaque échange est vérifié (marqueur, somme de contrôle) avant que
/// le suivant ne soit émis.
///
/// La coupure de session après `DevInfo` est **obligatoire**, pas prudentielle :
/// voir [`SESSION_FERMEE_PAR_CC`]. Sans elle, l'en-tête de données ne répond
/// jamais — et c'est exactement l'échec qu'a produit la première version de ce
/// code sur le logger réel.
pub fn read_all<T: Rc5Transport>(
    transport: &mut T,
    thresholds: &JumpThresholds,
) -> Result<Rc5Reading, SerialError> {
    let dev_info_frame = transport.exchange(&dev_info_request(), DEV_INFO_LEN)?;
    let device = decode_dev_info(&dev_info_frame)?;
    if device.model_no != RC5_MODEL_NO {
        return Err(SerialError::UnsupportedModel {
            model_no: device.model_no,
        });
    }

    transport.reset_session()?;

    let header_frame =
        transport.exchange(&data_header_request(device.station_no), DATA_HEADER_LEN)?;
    let header = decode_data_header(&header_frame)?;
    if header.rec_count == 0 {
        return Err(SerialError::NoRecords);
    }

    let mut pages_frame = Vec::new();
    for (page, count) in page_plan(usize::from(header.rec_count), RC5_PAGE_SIZE) {
        let trame = transport.exchange(&data_body_request(device.station_no, page), count * 2 + 2)?;
        // Contrôlée ici en plus de `decode_reading` : une page abîmée doit être
        // signalée tout de suite, avec son numéro, pas vingt pages plus loin.
        verifier_trame(&trame, &format!("page {page}"))?;
        pages_frame.extend_from_slice(&trame);
    }

    decode_reading(&dev_info_frame, &header_frame, &pages_frame, thresholds)
}

// ---------------------------------------------------------------------------
// Détection des ports
// ---------------------------------------------------------------------------

/// Un port série portant une puce CH340 — donc un RC-5 *possible*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ch340Port {
    /// `"COM8"` sous Windows, `"/dev/ttyUSB0"` ailleurs.
    pub port_name: String,
    /// Numéro de série **de la puce USB**, quand elle en expose un. À ne pas
    /// confondre avec celui du logger : les CH340 bon marché n'en ont
    /// généralement aucun, qui interdit de s'en servir pour reconnaître un
    /// appareil d'un branchement à l'autre.
    pub usb_serial: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

/// Tous les ports série portant un CH340.
///
/// Volontairement **sans aucune ouverture de port** : c'est l'opération qui
/// tourne en boucle en arrière-plan (voir [`crate::elitech_watch`]), et elle doit
/// rester une simple énumération système.
#[cfg(not(target_os = "android"))]
pub fn list_ch340_ports() -> Result<Vec<Ch340Port>, SerialError> {
    let ports =
        serialport::available_ports().map_err(|e| SerialError::Enumeration(e.to_string()))?;

    let mut trouves: Vec<Ch340Port> = ports
        .into_iter()
        .filter_map(|p| match &p.port_type {
            serialport::SerialPortType::UsbPort(usb)
                if usb.vid == CH340_VID && usb.pid == CH340_PID =>
            {
                Some(Ch340Port {
                    port_name: p.port_name.clone(),
                    usb_serial: usb.serial_number.clone(),
                    manufacturer: usb.manufacturer.clone(),
                    product: usb.product.clone(),
                })
            }
            _ => None,
        })
        .collect();

    // L'ordre d'énumération n'est garanti par aucun système. Le figer évite que
    // deux relevés successifs paraissent différer alors que rien n'a bougé.
    trouves.sort_by(|a, b| a.port_name.cmp(&b.port_name));
    Ok(trouves)
}

/// Le même inventaire, par l'API USB Host d'Android.
///
/// Le filtre est **le même** que sur desktop — VID:PID `1a86:7523` — et pour la
/// même raison : le nom de produit d'un adaptateur bon marché est souvent vide
/// ou fantaisiste, seul l'identifiant de puce désigne vraiment un CH340.
///
/// Aucune ouverture ici non plus : cette fonction tourne en boucle dans
/// [`crate::elitech_watch`], et ouvrir un appareil déclencherait une demande
/// d'autorisation système toutes les deux secondes et demie.
///
/// Sur un appareil sans mode hôte USB — ou quand le greffon n'est pas monté —
/// la liste revient vide plutôt qu'en erreur : « rien n'est branché » est le
/// message juste, et c'est déjà ce que dit l'écran.
#[cfg(target_os = "android")]
pub fn list_ch340_ports() -> Result<Vec<Ch340Port>, SerialError> {
    if !tauri_plugin_usbserial::disponible() {
        return Ok(Vec::new());
    }
    let appareils = tauri_plugin_usbserial::list_devices()
        .map_err(|e| SerialError::Enumeration(e.to_string()))?;

    let mut trouves: Vec<Ch340Port> = appareils
        .into_iter()
        .filter(|d| d.vendor_id == i32::from(CH340_VID) && d.product_id == i32::from(CH340_PID))
        .map(|d| Ch340Port {
            port_name: format!("{PREFIXE_PORT_USB}{}", d.device_id),
            usb_serial: d.serial_number,
            manufacturer: d.manufacturer_name,
            product: d.product_name,
        })
        .collect();

    trouves.sort_by(|a, b| a.port_name.cmp(&b.port_name));
    Ok(trouves)
}

/// Ouvre un port et demande son identité, avec le délai d'attente réduit.
pub fn identify_port(port_name: &str) -> Result<DevInfo, SerialError> {
    let mut transport = SerialTransport::open(port_name, TIMEOUT_IDENTIFICATION)?;
    identify(&mut transport)
}

/// Lit tout l'historique du logger branché sur ce port.
pub fn read_port(port_name: &str, thresholds: &JumpThresholds) -> Result<Rc5Reading, SerialError> {
    let mut transport = SerialTransport::open(port_name, TIMEOUT)?;
    read_all(&mut transport, thresholds)
}

/// Le premier port qui porte réellement un RC-5, identification comprise.
///
/// Renvoie aussi les échecs rencontrés en chemin : un CH340 qui n'a pas répondu
/// mérite d'être montré à l'utilisateur (c'est peut-être son logger, sur un
/// câble douteux) plutôt que passé sous silence.
pub fn find_rc5() -> Result<(Ch340Port, DevInfo), SerialError> {
    let ports = list_ch340_ports()?;
    if ports.is_empty() {
        return Err(SerialError::NoDevice);
    }
    let mut derniere = None;
    for port in ports {
        match identify_port(&port.port_name) {
            Ok(info) => return Ok((port, info)),
            Err(e) => derniere = Some(e),
        }
    }
    Err(derniere.unwrap_or(SerialError::NoDevice))
}

// ---------------------------------------------------------------------------
// Vérification de l'horloge
// ---------------------------------------------------------------------------

/// Écart en deçà duquel on considère les deux horloges alignées, en secondes.
///
/// Une minute : le RC-5 n'affiche pas les secondes et son horloge se règle à la
/// minute depuis ElitechLog. Exiger mieux signalerait en permanence un décalage
/// que l'appareil ne sait pas éviter.
pub const HORLOGE_ALIGNEE_MAX_S: i64 = 60;

/// Au-delà, le décalage cesse d'être un détail : un quart d'heure suffit à
/// déplacer un minimum nocturne d'un seau horaire à l'autre.
pub const HORLOGE_NOTABLE_MAX_S: i64 = 15 * 60;

/// Gravité du décalage entre l'horloge du logger et celle du PC.
///
/// Trois niveaux plutôt qu'un simple booléen : l'UI ne dit pas la même chose
/// pour trente secondes et pour deux heures, et c'est au backend de trancher —
/// le seuil est une décision métier, pas une affaire de présentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockDriftSeverity {
    /// Moins d'[`HORLOGE_ALIGNEE_MAX_S`] : rien à corriger.
    Aligned,
    /// Décalage réel mais modéré — à saisir à l'import, pas à craindre.
    Notable,
    /// Décalage important : les horodatages du logger sont franchement faux.
    Severe,
    /// L'appareil n'a pas livré d'horloge exploitable.
    Unreadable,
}

impl ClockDriftSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            ClockDriftSeverity::Aligned => "aligned",
            ClockDriftSeverity::Notable => "notable",
            ClockDriftSeverity::Severe => "severe",
            ClockDriftSeverity::Unreadable => "unreadable",
        }
    }
}

/// Comparaison de l'horloge du logger avec celle du PC.
///
/// ## Pourquoi deux écarts et non un seul
///
/// L'appareil annonce une heure **naïve** : celle de son horloge interne, sans
/// fuseau. Deux quantités différentes s'en déduisent, et les confondre est
/// exactement l'erreur à éviter :
///
/// * `offset_vs_utc_seconds` = `horloge − UTC`. C'est un **fait**, calculable
///   sans rien supposer.
/// * `drift_seconds` = `horloge − (UTC + fuseau)`. C'est la **dérive** de
///   l'appareil, et elle n'a de sens qu'une fois connu le fuseau sur lequel il a
///   été réglé — que seul l'utilisateur sait.
///
/// Un logger réglé sur UTC+1 et parfaitement à l'heure affiche un écart de
/// +3 600 s et une dérive nulle. Dire « le logger avance d'une heure » serait
/// faux ; c'est pourquoi le fuseau est un paramètre d'entrée et non une
/// supposition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClockCheck {
    /// Heure interne du logger, telle qu'il l'annonce (naïve).
    pub device_clock: Option<NaiveDateTime>,
    /// Heure du PC au même instant — la référence.
    pub host_utc: DateTime<Utc>,
    /// Fuseau supposé réglé sur le logger, en secondes (0, 3600, 7200…).
    pub timezone_offset_seconds: i64,
    /// `horloge − UTC`, en secondes. Positif : le logger est en avance sur l'UTC.
    pub offset_vs_utc_seconds: Option<i64>,
    /// `horloge − (UTC + fuseau)`, en secondes. C'est la dérive proprement dite.
    pub drift_seconds: Option<i64>,
    pub severity: ClockDriftSeverity,
    /// Valeur à donner à `clock_offset_seconds` pour un import fait maintenant,
    /// avec ce fuseau : `-(fuseau + dérive)`.
    ///
    /// Le même calcul que celui de l'écran d'import (`computeOffset` côté
    /// frontend), fait ici pour que la page RC-5 n'ait pas à le refaire.
    pub suggested_clock_offset_seconds: Option<i64>,
    /// Phrase prête à afficher, rédigée ici comme tous les messages destinés à
    /// l'utilisateur (cf. l'en-tête d'`error.rs`).
    pub message: String,
}

/// Compare l'horloge lue à une référence. **Aucun accès au port** : c'est ce qui
/// permet de tester la règle des seuils et la formule de dérive sans matériel.
pub fn compare_clock(
    device_clock: Option<NaiveDateTime>,
    host_utc: DateTime<Utc>,
    timezone_offset_seconds: i64,
) -> ClockCheck {
    let Some(horloge) = device_clock else {
        return ClockCheck {
            device_clock: None,
            host_utc,
            timezone_offset_seconds,
            offset_vs_utc_seconds: None,
            drift_seconds: None,
            severity: ClockDriftSeverity::Unreadable,
            suggested_clock_offset_seconds: None,
            message: "Le logger n'a pas livré d'heure exploitable : impossible de \
                      vérifier son horloge. Les horodatages restent corrigeables à \
                      l'import, mais il faudra saisir la dérive à la main."
                .to_string(),
        };
    };

    let ecart = (horloge - host_utc.naive_utc()).num_seconds();
    let derive = ecart - timezone_offset_seconds;
    let absolue = derive.abs();

    let severity = if absolue <= HORLOGE_ALIGNEE_MAX_S {
        ClockDriftSeverity::Aligned
    } else if absolue <= HORLOGE_NOTABLE_MAX_S {
        ClockDriftSeverity::Notable
    } else {
        ClockDriftSeverity::Severe
    };

    let sens = if derive > 0 { "avance" } else { "retard" };
    let message = match severity {
        ClockDriftSeverity::Aligned => format!(
            "Horloge du logger conforme : {} d'écart avec le PC, une fois le fuseau \
             retiré. Rien à corriger.",
            fmt_duree(absolue)
        ),
        ClockDriftSeverity::Notable => format!(
            "Le logger a {} de {sens} sur l'heure de référence. Ses horodatages sont \
             décalés d'autant ; la correction est appliquée à l'import (dérive \
             {derive:+} s), les mesures déjà en base ne bougent pas.",
            fmt_duree(absolue)
        ),
        ClockDriftSeverity::Severe => format!(
            "Le logger a {} de {sens} sur l'heure de référence — c'est beaucoup. \
             Vérifiez le fuseau choisi avant de conclure : un logger réglé sur une \
             autre heure locale que celle sélectionnée donne exactement ce résultat. \
             Si le fuseau est bon, l'horloge a réellement dérivé et tous ses \
             horodatages sont faux de {derive:+} s.",
            fmt_duree(absolue)
        ),
        // Traité plus haut : il n'y a pas de dérive à décrire sans horloge.
        ClockDriftSeverity::Unreadable => String::new(),
    };

    ClockCheck {
        device_clock: Some(horloge),
        host_utc,
        timezone_offset_seconds,
        offset_vs_utc_seconds: Some(ecart),
        drift_seconds: Some(derive),
        severity,
        // Même formule que l'écran d'import : ce qu'il faut **ajouter** à
        // l'horodatage brut pour retomber sur de l'UTC vrai.
        suggested_clock_offset_seconds: Some(-(timezone_offset_seconds + derive)),
        message,
    }
}

/// « 2 min 5 s », « 1 h 12 min », « 45 s » — pour un écart d'horloge.
fn fmt_duree(secondes: i64) -> String {
    let s = secondes.abs();
    if s < 60 {
        return format!("{s} s");
    }
    if s < 3600 {
        let reste = s % 60;
        return if reste == 0 {
            format!("{} min", s / 60)
        } else {
            format!("{} min {reste} s", s / 60)
        };
    }
    let reste = (s % 3600) / 60;
    if reste == 0 {
        format!("{} h", s / 3600)
    } else {
        format!("{} h {reste} min", s / 3600)
    }
}

/// Lit l'horloge d'un logger branché et la compare à celle du PC.
///
/// Un seul échange de 160 octets : c'est `DevInfo` qui porte l'horloge, et c'est
/// la même trame que celle de l'identification. Rien de neuf n'est demandé à
/// l'appareil — la vérification d'heure ne coûte donc pas plus cher qu'un
/// « qu'est-ce qui est branché ? ».
pub fn check_clock_on_port(
    port_name: &str,
    timezone_offset_seconds: i64,
    host_utc: DateTime<Utc>,
) -> Result<(DevInfo, ClockCheck), SerialError> {
    let mut transport = SerialTransport::open(port_name, TIMEOUT_IDENTIFICATION)?;
    let info = identify(&mut transport)?;
    let controle = compare_clock(info.device_clock, host_utc, timezone_offset_seconds);
    Ok((info, controle))
}

// ---------------------------------------------------------------------------
// Réinitialisation : effacement de la mémoire du logger
// ---------------------------------------------------------------------------

/// Sous-commande d'**écriture des paramètres** de la famille `0x33`.
///
/// ⚠️ **C'est la seule commande d'écriture de tout le projet, et la seule dont
/// le comportement n'a pas été vérifié sur le matériel.** Elle mérite donc
/// d'être lue en entier avant d'être touchée.
///
/// ## D'où elle vient
///
/// De la bibliothèque de référence (`elitech-datareader`), où la commande
/// « set » écrit les paramètres de l'appareil, et où il est noté que **toutes
/// les données du logger sont effacées quand les paramètres sont écrits**. C'est
/// ce mécanisme-là — et non une commande « effacer » dédiée, qui n'existe pas —
/// qui remet la mémoire à zéro : le RC-5 se réinitialise en étant reprogrammé.
///
/// ## Ce qu'on écrit, et pourquoi c'est le choix le moins risqué
///
/// Le découpage exact du bloc de paramètres n'est pas connu champ par champ.
/// Plutôt que de le reconstituer au jugé — une erreur d'un seul octet réglerait
/// l'appareil sur une cadence ou des seuils aberrants — la requête **renvoie à
/// l'appareil sa propre fiche d'identité**, telle qu'il vient de la transmettre :
/// le corps de la trame `DevInfo`, sans son marqueur ni sa somme de contrôle.
///
/// La conséquence tient en une phrase : *aucun paramètre ne peut prendre une
/// valeur que l'appareil ne portait pas déjà*. Le seul effet recherché est
/// l'effacement qui accompagne l'écriture.
///
/// ## Et si la disposition était malgré tout mauvaise
///
/// [`clear_memory`] relit la fiche d'identité après coup et **compare champ par
/// champ** ce qui est comparable (cadence, seuils, calibration, numéro de série,
/// numéro de station). Une divergence est remontée telle quelle à l'utilisateur,
/// avec la liste de ce qui a bougé, plutôt que passée sous silence — voir
/// [`ResetReport::parameters_preserved`]. Rien de tout cela n'est réparable
/// depuis l'application : c'est ElitechLog qui reprogramme un logger.
/// Opcode de configuration des paramètres et d'effacement rapide du RC-5
/// sur protocole USB-série CH340 (`0x05 0x00`, 25 octets).
///
/// Découvert et validé sur matériel réel au prompt 21 : le logger efface sa
/// mémoire (`rec_count = 0`), repasse à `NotStarted` (`0x00`), et conserve
/// l'intégralité de ses paramètres de fonctionnement.
pub const CMD_PARAM_PUT: [u8; 2] = [0x05, 0x00];

/// Sous-commande d'**arrêt de l'enregistrement**, découverte par sondage sur un
/// RC-5 réel (COM8, série E207102078) : ni `elitech-datareader` ni
/// `python-elitech` — les deux bibliothèques de référence du projet — ne la
/// documentent pour ce protocole série. `python-elitech` a bien une commande
/// `StopCommand` équivalente, mais pour une autre famille de matériel (USB HID,
/// RC-51H/RC-5+/RC-55, VID `0x04d8`) — pas la nôtre (USB-série CH340, VID
/// `0x1a86`).
///
/// Vérifiée isolément : à partir d'un logger fraîchement remis à `NotStarted`,
/// `0x33 <station> 08 00 <somme>` seul fait passer `work_status` à `Stopped`
/// sans changer `rec_count`, la cadence, les seuils, la calibration ni le
/// numéro de série. C'est la commande que le bouton « Arrêter » d'ElitechLog
/// envoie vraisemblablement — le manuel officiel ne mentionne un effacement
/// que pour « Réinitialisation rapide » ([`CMD_PARAM_PUT`]), jamais pour
/// l'arrêt.
///
/// ⚠️ Aucune commande de **reprise** n'a été trouvée en sondant les opcodes
/// voisins (`03`, `04`, `06`, `0A`, `0C`, `0D` suivis de `00`) : aucun ne fait
/// repasser `work_status` à `Running`. Tout indique que redémarrer
/// l'enregistrement est un geste physique sur l'appareil (bouton ▶), pas une
/// commande logicielle — d'où l'absence de fonction symétrique ici.
pub const CMD_STOP: u8 = 0x08;

/// Construit la requête d'arrêt : `33 <station> 08 00 <somme>`.
pub fn stop_request(station_no: u8) -> [u8; 5] {
    let mut trame = [0x33, station_no, CMD_STOP, 0x00, 0x00];
    trame[4] = checksum(&trame[..4]);
    trame
}

/// Construit la requête d'écriture de paramètres (25 octets) à partir de la fiche d'identité
/// relue (`DevInfo`) : `33 <station> 05 00 <cadence:3> <haut:2> <bas:2> <station:1> <stop_btn:1> <delay:1> <tone:1> <alarm:1> <unit:1> <calib:1> 00 00 00 00 00 00 <somme>`
pub fn param_put_request(dev_info_frame: &[u8]) -> Result<Vec<u8>, SerialError> {
    if dev_info_frame.len() != DEV_INFO_LEN {
        return Err(SerialError::BadFrame(format!(
            "ParamPut : fiche d'identité de {} octets au lieu de {DEV_INFO_LEN}",
            dev_info_frame.len()
        )));
    }
    verifier_trame(dev_info_frame, "ParamPut")?;
    let info = decode_dev_info(dev_info_frame)?;
    Ok(param_put_request_from_info(&info).to_vec())
}

/// Construit directement la trame de 25 octets à partir d'un `DevInfo`.
pub fn param_put_request_from_info(info: &DevInfo) -> [u8; 25] {
    let mut trame = [0u8; 25];
    trame[0] = 0x33;
    trame[1] = info.station_no;
    trame[2] = CMD_PARAM_PUT[0]; // 0x05
    trame[3] = CMD_PARAM_PUT[1]; // 0x00

    let h = (info.sample_interval_seconds / 3600) as u8;
    let m = ((info.sample_interval_seconds % 3600) / 60) as u8;
    let s = (info.sample_interval_seconds % 60) as u8;
    trame[4] = h;
    trame[5] = m;
    trame[6] = s;

    let upper = (info.upper_limit_celsius * 10.0) as i16;
    trame[7..9].copy_from_slice(&upper.to_be_bytes());

    let lower = (info.lower_limit_celsius * 10.0) as i16;
    trame[9..11].copy_from_slice(&lower.to_be_bytes());

    trame[11] = info.station_no;
    trame[12] = 0x13; // Bouton d'arrêt autorisé (0x13)
    trame[13] = 0x00; // Délai
    trame[14] = 0x31; // Signal sonore désactivé (0x31)
    trame[15] = 0x00; // Alarme désactivée (0x00)
    trame[16] = if info.celsius { 0x31 } else { 0x13 }; // Unité (°C = 0x31)
    trame[17] = (info.temperature_calibration * 10.0) as i8 as u8;
    // Octets 18..24 : humidité et réservé
    trame[24] = checksum(&trame[..24]);
    trame
}

/// Ce que la réinitialisation a produit, avant et après.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResetReport {
    /// Fiche d'identité relevée **avant** l'effacement.
    pub before: DevInfo,
    /// Fiche d'identité relevée **après**. C'est elle qui fait foi.
    pub after: DevInfo,
    /// Paramètres dont la valeur a changé entre les deux — normalement vide.
    /// Chaque entrée est une phrase (« cadence : 900 s → 0 s »).
    pub changed_parameters: Vec<String>,
    /// `true` si le logger enregistrait encore et a dû être arrêté avant
    /// l'effacement. L'utilisateur doit le savoir : son logger ne redémarrera
    /// pas tout seul.
    pub recording_stopped: bool,
}

impl ResetReport {
    /// `true` si la mémoire est effectivement vide.
    pub fn memory_cleared(&self) -> bool {
        self.after.rec_count == 0
    }

    /// `true` si l'appareil est ressorti avec exactement les réglages qu'il avait.
    pub fn parameters_preserved(&self) -> bool {
        self.changed_parameters.is_empty()
    }

    /// Phrase de bilan destinée à l'utilisateur.
    pub fn message(&self) -> String {
        let efface = self.before.rec_count;
        // Phrase ajoutée en tête quand l'appareil enregistrait : il est
        // maintenant arrêté et, sur un RC-5, seul ElitechLog peut le relancer.
        // L'utilisateur doit repartir en le sachant.
        let arret = if self.recording_stopped {
            "Le logger enregistrait : il a été arrêté avant l'effacement, et ne redémarrera pas seul — reprogrammez-le depuis ElitechLog pour relancer un enregistrement. "
        } else {
            ""
        };
        if self.parameters_preserved() {
            format!(
                "{arret}Mémoire du logger effacée : {efface} mesure(s) supprimées de l'appareil. \
                 Ses réglages sont inchangés (cadence {} s, seuils {} / {} °C). Les données \
                 déjà importées dans l'application ne sont pas concernées.",
                self.after.sample_interval_seconds,
                self.after.lower_limit_celsius,
                self.after.upper_limit_celsius,
            )
        } else {
            format!(
                "{arret}Mémoire du logger effacée ({efface} mesure(s)), mais certains de ses \
                 réglages ont changé au passage : {}. Reprogrammez l'appareil depuis \
                 ElitechLog avant de le renvoyer sur le terrain. Les données déjà \
                 importées dans l'application ne sont pas concernées.",
                self.changed_parameters.join(" ; ")
            )
        }
    }
}

/// Compare les réglages, hors tout ce qui doit légitimement bouger.
///
/// `rec_count`, `start_time` et `device_clock` sont **attendus** différents après
/// un effacement : les comparer signalerait un problème à chaque fois.
fn parametres_modifies(avant: &DevInfo, apres: &DevInfo) -> Vec<String> {
    let mut changes = Vec::new();
    if avant.sample_interval_seconds != apres.sample_interval_seconds {
        changes.push(format!(
            "cadence d'enregistrement : {} s → {} s",
            avant.sample_interval_seconds, apres.sample_interval_seconds
        ));
    }
    if avant.upper_limit_celsius != apres.upper_limit_celsius
        || avant.lower_limit_celsius != apres.lower_limit_celsius
    {
        changes.push(format!(
            "seuils d'alarme : {} / {} °C → {} / {} °C",
            avant.lower_limit_celsius,
            avant.upper_limit_celsius,
            apres.lower_limit_celsius,
            apres.upper_limit_celsius
        ));
    }
    if avant.temperature_calibration != apres.temperature_calibration {
        changes.push(format!(
            "correction interne : {:+.1} °C → {:+.1} °C",
            avant.temperature_calibration, apres.temperature_calibration
        ));
    }
    if avant.station_no != apres.station_no {
        changes.push(format!(
            "numéro de station : {} → {}",
            avant.station_no, apres.station_no
        ));
    }
    if avant.device_number != apres.device_number {
        changes.push(format!(
            "numéro de série : {} → {}",
            avant.device_number.clone().unwrap_or_else(|| "—".to_string()),
            apres.device_number.clone().unwrap_or_else(|| "—".to_string())
        ));
    }
    if avant.celsius != apres.celsius {
        changes.push("unité d'affichage (Celsius / Fahrenheit)".to_string());
    }
    changes
}

/// Efface la mémoire du logger, puis **vérifie que c'est fait**.
///
/// Trois sessions, imposées par la règle du protocole (une commande `0xCC` clôt
/// la session — voir [`SESSION_FERMEE_PAR_CC`]) :
///
/// | Session | Ce qui s'y passe |
/// |---|---|
/// | 1 | `DevInfo` : l'état avant, et la trame qui sert de gabarit |
/// | 2 | l'écriture de paramètres, qui emporte l'effacement |
/// | 3 | `DevInfo` : l'état après, seule preuve que l'effacement a eu lieu |
///
/// **Si le logger enregistre encore**, deux sessions s'intercalent entre la 1 et
/// la 2 : l'envoi de [`CMD_STOP`] puis la relecture qui le confirme. Sans cet
/// arrêt, l'appareil continue d'écrire pendant l'effacement et la relecture
/// finale ressort en [`SerialError::ResetNotApplied`] — la situation bloquante
/// rencontrée sur le terrain. La fiche relue après l'arrêt devient alors le
/// gabarit, à la place de celle de la session 1.
///
/// Les temps de repos entre sessions sont ceux de [`REPOS_APRES_FERMETURE`], que
/// [`SerialTransport::open`] applique déjà — il n'y a pas de temporisation
/// supplémentaire ici, et il ne doit pas y en avoir : ce serait une seconde règle
/// à maintenir en parallèle de la première.
///
/// La réussite n'est **jamais** déduite de l'absence d'erreur : elle est lue dans
/// la fiche d'identité relue. Un logger qui n'aurait pas obtempéré ressort en
/// [`SerialError::ResetNotApplied`], et il n'y a alors rien à défaire.
pub fn clear_memory<T: Rc5Transport>(transport: &mut T) -> Result<ResetReport, SerialError> {
    eprintln!("[RC5-CLEAR] == Début clear_memory ==");
    let trame_avant = transport.exchange(&dev_info_request(), DEV_INFO_LEN)?;
    let avant = decode_dev_info(&trame_avant)?;
    eprintln!(
        "[RC5-CLEAR] 1. Avant : model_no={}, station_no={}, work_status={:?} ({}), rec_count={}",
        avant.model_no,
        avant.station_no,
        avant.work_status,
        avant.work_status.label_fr(),
        avant.rec_count
    );
    if avant.model_no != RC5_MODEL_NO {
        return Err(SerialError::UnsupportedModel {
            model_no: avant.model_no,
        });
    }

    let recording_stopped = avant.work_status == WorkStatus::Running;
    let trame_gabarit = if recording_stopped {
        eprintln!("[RC5-CLEAR] 2a. Logger en cours d'enregistrement -> envoi commande d'arrêt (CMD_STOP)...");
        transport.reset_session()?;
        transport.send(&stop_request(avant.station_no))?;

        transport.reset_session()?;
        eprintln!("[RC5-CLEAR] 2b. Relecture DevInfo pour confirmer l'arrêt...");
        let trame_apres_arret = transport.exchange(&dev_info_request(), DEV_INFO_LEN)?;
        let apres_arret = decode_dev_info(&trame_apres_arret)?;
        eprintln!(
            "[RC5-CLEAR] 2c. Statut après arrêt : work_status={:?} ({}), rec_count={}",
            apres_arret.work_status,
            apres_arret.work_status.label_fr(),
            apres_arret.rec_count
        );
        if apres_arret.work_status != WorkStatus::Stopped {
            eprintln!("[RC5-CLEAR] !!! Échec arrêt : état {:?}", apres_arret.work_status);
            return Err(SerialError::StopNotApplied {
                etat: apres_arret.work_status.label_fr(),
            });
        }
        trame_apres_arret
    } else {
        eprintln!("[RC5-CLEAR] 2. Logger déjà à l'arrêt/non démarré -> pas d'arrêt préalable nécessaire.");
        trame_avant
    };

    let requete = param_put_request(&trame_gabarit)?;
    eprintln!("[RC5-CLEAR] 3. Envoi CMD_PARAM_PUT ({} octets)...", requete.len());

    transport.reset_session()?;
    transport.send(&requete)?;

    transport.reset_session()?;
    eprintln!("[RC5-CLEAR] 4. Relecture finale DevInfo pour vérifier l'effacement...");
    let trame_apres = transport.exchange(&dev_info_request(), DEV_INFO_LEN)?;
    let apres = decode_dev_info(&trame_apres)?;
    eprintln!(
        "[RC5-CLEAR] 5. Statut final : work_status={:?} ({}), rec_count={}",
        apres.work_status,
        apres.work_status.label_fr(),
        apres.rec_count
    );

    if apres.rec_count != 0 {
        eprintln!("[RC5-CLEAR] !!! Échec effacement : rec_count={}", apres.rec_count);
        return Err(SerialError::ResetNotApplied {
            remaining: apres.rec_count,
        });
    }

    let changed_parameters = parametres_modifies(&avant, &apres);
    eprintln!("[RC5-CLEAR] == Succès clear_memory (params modifiés: {:?}) ==", changed_parameters);
    Ok(ResetReport {
        before: avant,
        after: apres,
        changed_parameters,
        recording_stopped,
    })
}

/// Efface la mémoire du logger branché sur ce port.
pub fn reset_port(port_name: &str) -> Result<ResetReport, SerialError> {
    let mut transport = SerialTransport::open(port_name, TIMEOUT)?;
    clear_memory(&mut transport)
}

/// Arrête l'enregistrement en cours, **sans effacer les mesures déjà prises**.
///
/// Deux sessions, comme pour [`clear_memory`] et pour la même raison
/// ([`SESSION_FERMEE_PAR_CC`]) : la commande `08 00` n'attend pas de réponse
/// (voir [`Rc5Transport::send`]), donc la seule preuve qu'elle a été prise en
/// compte est une relecture de `DevInfo` dans une session neuve.
///
/// Contrairement à [`clear_memory`], la réussite ne se lit pas sur
/// `rec_count` — qui ne doit justement **pas** bouger — mais sur
/// `work_status`, qui doit devenir [`WorkStatus::Stopped`].
pub fn stop_recording<T: Rc5Transport>(transport: &mut T) -> Result<DevInfo, SerialError> {
    eprintln!("[RC5-STOP] == Début stop_recording ==");
    let info_avant = decode_dev_info(&transport.exchange(&dev_info_request(), DEV_INFO_LEN)?)?;
    eprintln!(
        "[RC5-STOP] 1. Avant arrêt : model_no={}, station_no={}, work_status={:?} ({}), rec_count={}",
        info_avant.model_no,
        info_avant.station_no,
        info_avant.work_status,
        info_avant.work_status.label_fr(),
        info_avant.rec_count
    );
    if info_avant.model_no != RC5_MODEL_NO {
        return Err(SerialError::UnsupportedModel {
            model_no: info_avant.model_no,
        });
    }

    transport.reset_session()?;
    eprintln!("[RC5-STOP] 2. Envoi CMD_STOP (station={})...", info_avant.station_no);
    transport.send(&stop_request(info_avant.station_no))?;

    transport.reset_session()?;
    eprintln!("[RC5-STOP] 3. Relecture DevInfo pour vérifier le passage à Stopped...");
    let apres = decode_dev_info(&transport.exchange(&dev_info_request(), DEV_INFO_LEN)?)?;
    eprintln!(
        "[RC5-STOP] 4. Après arrêt : work_status={:?} ({}), rec_count={}",
        apres.work_status,
        apres.work_status.label_fr(),
        apres.rec_count
    );

    if apres.work_status != WorkStatus::Stopped {
        eprintln!("[RC5-STOP] !!! Arrêt non appliqué : état {:?}", apres.work_status);
        return Err(SerialError::StopNotApplied {
            etat: apres.work_status.label_fr(),
        });
    }

    eprintln!("[RC5-STOP] == Succès stop_recording ==");
    Ok(apres)
}

/// Arrête l'enregistrement du logger branché sur ce port.
pub fn stop_port(port_name: &str) -> Result<DevInfo, SerialError> {
    let mut transport = SerialTransport::open(port_name, TIMEOUT)?;
    stop_recording(&mut transport)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trame `DevInfo` du logger réel `EFE205004333`, telle que capturée.
    /// Le fichier committé (`docs/samples/rc5_direct/devinfo.bin`) en est la copie
    /// binaire ; ces octets-ci gardent les tests unitaires autonomes.
    const DEV_INFO_REEL: [u8; 160] = [
        0x55, 0x01, 0x01, 0x32, 0x0C, 0x00, 0x0F, 0x00, 0x01, 0x90, 0xFE, 0xD4, 0x07, 0xEA, 0x08,
        0x13, 0x14, 0x81, 0x1F, 0x02, 0x07, 0xEA, 0x05, 0x01, 0x0E, 0x07, 0x1F, 0x31, 0x64, 0x28,
        0x43, 0x07, 0xEA, 0x08, 0x14, 0x0A, 0x22, 0x0B, 0x52, 0x43, 0x2D, 0x35, 0x20, 0x44, 0x61,
        0x74, 0x61, 0x20, 0x4C, 0x6F, 0x67, 0x67, 0x65, 0x72, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x45, 0x32, 0x30, 0x35, 0x30, 0x30, 0x34, 0x33, 0x33, 0x33, 0x00, 0x13,
        0x00, 0x31, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x96,
    ];

    /// Trame `DataHeader` du même logger.
    const HEADER_REEL: [u8; 11] = [
        0x55, 0x28, 0x43, 0x07, 0xEA, 0x05, 0x01, 0x0E, 0x07, 0x1F, 0xEB,
    ];

    fn instant(iso: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(iso, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn les_requetes_constantes_portent_la_bonne_somme() {
        // `Init` et `DevInfo` ont leur somme écrite en dur : ce test est ce qui
        // garantit qu'elle est juste, puisque rien ne la recalcule.
        let init = init_request();
        assert_eq!(checksum(&init[..4]), init[4]);
        let info = dev_info_request();
        assert_eq!(checksum(&info[..4]), info[4]);
    }

    #[test]
    fn les_requetes_de_donnees_correspondent_aux_trames_observees() {
        assert_eq!(data_header_request(1), [0x33, 0x01, 0x01, 0x00, 0x35]);
        assert_eq!(data_body_request(1, 0), [0x33, 0x01, 0x02, 0x00, 0x36]);
        assert_eq!(data_body_request(1, 20), [0x33, 0x01, 0x02, 0x14, 0x4A]);
    }

    #[test]
    fn la_somme_de_controle_deborde_sans_paniquer() {
        // 0xFF × 4 = 1020, soit 0xFC modulo 256. Une addition non enveloppante
        // paniquerait en profil `debug` — c'est-à-dire pendant les tests.
        assert_eq!(checksum(&[0xFF, 0xFF, 0xFF, 0xFF]), 0xFC);
    }

    #[test]
    fn la_fiche_du_logger_reel_se_decode() {
        let info = decode_dev_info(&DEV_INFO_REEL).expect("trame réelle valide");
        assert_eq!(info.station_no, 1);
        assert_eq!(info.model_no, RC5_MODEL_NO);
        assert_eq!(info.model_label(), "RC-5");
        assert_eq!(info.sample_interval_seconds, 900);
        assert_eq!(info.rec_count, 10_307);
        assert_eq!(info.start_time, Some(instant("2026-05-01 14:07:31")));
        assert_eq!(info.device_clock, Some(instant("2026-08-20 10:34:11")));
        assert_eq!(info.work_status, WorkStatus::Stopped);
        assert_eq!(info.device_number.as_deref(), Some("E205004333"));
        assert_eq!(info.user_info.as_deref(), Some("RC-5 Data Logger"));
        assert_eq!(info.upper_limit_celsius, 40.0);
        assert_eq!(info.lower_limit_celsius, -30.0);
        assert_eq!(info.temperature_calibration, 0.0);
        assert!(info.celsius);
    }

    #[test]
    fn un_champ_de_date_aberrant_ne_fait_pas_echouer_la_lecture() {
        // `last_online` du logger réel vaut « 129 minutes » : invalide. Le reste
        // de la fiche est parfaitement lisible et doit le rester.
        let info = decode_dev_info(&DEV_INFO_REEL).unwrap();
        assert_eq!(info.last_online, None);
        assert_eq!(info.rec_count, 10_307);
    }

    #[test]
    fn len_tete_de_donnees_du_logger_reel_se_decode() {
        let h = decode_data_header(&HEADER_REEL).expect("trame réelle valide");
        assert_eq!(h.rec_count, 10_307);
        assert_eq!(h.start_time, Some(instant("2026-05-01 14:07:31")));
    }

    #[test]
    fn une_somme_de_controle_fausse_est_refusee() {
        let mut abimee = HEADER_REEL;
        abimee[10] ^= 0xFF;
        assert!(matches!(
            decode_data_header(&abimee),
            Err(SerialError::BadChecksum { .. })
        ));
    }

    #[test]
    fn un_marqueur_inattendu_est_refuse() {
        // Le cas concret : un CH340 qui n'est pas un RC-5 et qui répond n'importe
        // quoi. Mieux vaut une erreur qu'une fiche d'identité inventée.
        let mut trame = [0u8; DEV_INFO_LEN];
        trame[0] = 0xAA;
        trame[159] = checksum(&trame[..159]);
        assert!(matches!(
            decode_dev_info(&trame),
            Err(SerialError::BadFrame(_))
        ));
    }

    #[test]
    fn un_modele_non_rc5_est_refuse_explicitement() {
        // Un RC-4 (modèle 40) : la structure des trames est la même, mais la
        // taille de page diffère — le décoder comme un RC-5 donnerait des
        // mesures fausses sans rien signaler.
        let mut trame = DEV_INFO_REEL;
        trame[3] = 40;
        trame[159] = checksum(&trame[..159]);
        let mut faux = TransportRejoue::new(vec![trame.to_vec()]);
        assert!(matches!(
            identify(&mut faux),
            Err(SerialError::UnsupportedModel { model_no: 40 })
        ));
    }

    #[test]
    fn les_temperatures_negatives_gardent_leur_signe() {
        // La régression qu'il faut empêcher : `FF F5` lu sur 8 bits donne +24,5.
        let mut trame = vec![MARQUEUR, 0x00, 0xDE, 0xFF, 0xF5, 0xFF, 0xFF, 0x00, 0x00];
        let somme = checksum(&trame);
        trame.push(somme);
        let valeurs = decode_data_page(&trame, 4).unwrap();
        assert_eq!(valeurs, vec![222, -11, -1, 0]);
    }

    #[test]
    fn le_plan_de_pagination_couvre_exactement_les_mesures() {
        // Le cas réel : 21 pages, dont une dernière de 307.
        let plan = page_plan(10_307, RC5_PAGE_SIZE);
        assert_eq!(plan.len(), 21);
        assert_eq!(plan[0], (0, 500));
        assert_eq!(plan[20], (20, 307));
        assert_eq!(plan.iter().map(|(_, c)| c).sum::<usize>(), 10_307);

        // Multiple exact : pas de page vide en trop.
        let plan = page_plan(1_000, RC5_PAGE_SIZE);
        assert_eq!(plan, vec![(0, 500), (1, 500)]);

        assert!(page_plan(0, RC5_PAGE_SIZE).is_empty());
    }

    #[test]
    fn les_instants_sont_reconstruits_a_la_cadence_annoncee() {
        let m = build_measurements(instant("2026-05-01 14:07:31"), 900, &[222, 224, -11]);
        assert_eq!(m[0].raw_timestamp, instant("2026-05-01 14:07:31"));
        assert_eq!(m[1].raw_timestamp, instant("2026-05-01 14:22:31"));
        assert_eq!(m[2].raw_timestamp, instant("2026-05-01 14:37:31"));
        assert_eq!(m[2].temperature_celsius, -1.1);
        // Aucune humidité : le RC-5 n'en mesure pas, et le parseur xlsx produit
        // `None` lui aussi — condition de l'égalité des empreintes.
        assert!(m.iter().all(|x| x.humidity_percent.is_none()));
        // `source_line` = numéro d'enregistrement, comme la colonne `No.`.
        assert_eq!(m[0].source_line, 1);
        assert_eq!(m[2].source_line, 3);
    }

    #[test]
    fn la_derive_dhorloge_est_signee_dans_le_bon_sens() {
        let info = decode_dev_info(&DEV_INFO_REEL).unwrap();
        // Horloge du logger : 2026-08-20 10:34:11. Un PC en retard de 60 s donne
        // un logger « en avance » de 60 s.
        let drift = info.clock_drift_seconds(instant("2026-08-20 10:33:11"));
        assert_eq!(drift, Some(60));
    }

    #[test]
    fn les_numeros_de_serie_tronques_se_reconnaissent() {
        assert!(serials_match("E205004333", "EFE205004333"));
        assert!(serials_match("EFE205004333", "E205004333"));
        assert!(serials_match("efe205004333", "EFE205004333"));
        assert!(!serials_match("E205004333", "E205004334"));
        assert!(!serials_match("", "EFE205004333"));
    }

    // -- séquence complète, sur un transport rejoué ------------------------

    /// Rejoue des réponses préparées, dans l'ordre, et retient les requêtes.
    ///
    /// Il **applique la règle de session du vrai appareil** (voir
    /// [`SESSION_FERMEE_PAR_CC`]) : après une requête `0xCC`, tout échange qui ne
    /// serait pas précédé d'un `reset_session` expire, comme le ferait le
    /// logger. Sans cela, ce transport serait plus permissif que le matériel et
    /// les tests laisseraient passer précisément le bug qui a été rencontré.
    struct TransportRejoue {
        reponses: Vec<Vec<u8>>,
        pub envoyees: Vec<Vec<u8>>,
        /// Position des réinitialisations dans `envoyees`.
        pub reinitialisations: Vec<usize>,
        session_close: bool,
    }

    impl TransportRejoue {
        fn new(reponses: Vec<Vec<u8>>) -> Self {
            TransportRejoue {
                reponses,
                envoyees: Vec::new(),
                reinitialisations: Vec::new(),
                session_close: false,
            }
        }
    }

    impl Rc5Transport for TransportRejoue {
        fn exchange(&mut self, request: &[u8], expected: usize) -> Result<Vec<u8>, SerialError> {
            if self.session_close {
                // Le logger réel ne répond simplement plus : pas d'erreur, un
                // silence. C'est ce silence-là qu'on reproduit.
                return Err(SerialError::Timeout {
                    expected,
                    received: 0,
                });
            }
            self.envoyees.push(request.to_vec());
            if request.first() == Some(&0xCC) {
                self.session_close = true;
            }
            if self.reponses.is_empty() {
                return Err(SerialError::Timeout {
                    expected,
                    received: 0,
                });
            }
            let r = self.reponses.remove(0);
            assert_eq!(
                r.len(),
                expected,
                "l'appelant n'attend pas la bonne longueur"
            );
            Ok(r)
        }

        fn send(&mut self, request: &[u8]) -> Result<(), SerialError> {
            // Le vrai transport n'attend rien en retour ; le mock non plus. La
            // requête est enregistrée comme les autres, pour que les tests
            // puissent vérifier *ce qui a été émis*.
            self.envoyees.push(request.to_vec());
            if request.first() == Some(&0xCC) {
                self.session_close = true;
            }
            Ok(())
        }

        fn reset_session(&mut self) -> Result<(), SerialError> {
            self.reinitialisations.push(self.envoyees.len());
            self.session_close = false;
            Ok(())
        }
    }

    /// Fabrique une fiche d'identité cohérente pour un petit lot de mesures.
    fn fiche(rec_count: u16, interval_s: u8) -> Vec<u8> {
        let mut t = DEV_INFO_REEL;
        t[6] = 0; // minutes
        t[7] = interval_s; // secondes
        t[29..31].copy_from_slice(&rec_count.to_be_bytes());
        t[159] = checksum(&t[..159]);
        t.to_vec()
    }

    fn entete(rec_count: u16) -> Vec<u8> {
        let mut t = HEADER_REEL;
        t[1..3].copy_from_slice(&rec_count.to_be_bytes());
        t[10] = checksum(&t[..10]);
        t.to_vec()
    }

    fn page(valeurs: &[i16]) -> Vec<u8> {
        let mut t = vec![MARQUEUR];
        for v in valeurs {
            t.extend_from_slice(&v.to_be_bytes());
        }
        let somme = checksum(&t);
        t.push(somme);
        t
    }

    #[test]
    fn la_sequence_complete_enchaine_les_bonnes_requetes() {
        let valeurs: Vec<i16> = (0..600).map(|i| i as i16 - 100).collect();
        let mut transport = TransportRejoue::new(vec![
            fiche(600, 30),
            entete(600),
            page(&valeurs[..500]),
            page(&valeurs[500..]),
        ]);

        let lecture = read_all(&mut transport, &JumpThresholds::default()).unwrap();

        assert_eq!(lecture.measurements.len(), 600);
        assert_eq!(lecture.measurements[0].temperature_celsius, -10.0);
        assert_eq!(lecture.measurements[599].temperature_celsius, 49.9);
        // Cadence de 30 s : la 600ᵉ mesure est 599 × 30 s après la première.
        assert_eq!(
            lecture.measurements[599].raw_timestamp - lecture.measurements[0].raw_timestamp,
            chrono::Duration::seconds(599 * 30)
        );

        // Et surtout : les requêtes émises, dans l'ordre, avec le bon numéro de
        // page. C'est là que se joue la lecture d'un historique long.
        assert_eq!(
            transport.envoyees,
            vec![
                dev_info_request().to_vec(),
                data_header_request(1).to_vec(),
                data_body_request(1, 0).to_vec(),
                data_body_request(1, 1).to_vec(),
            ]
        );

        // Une seule coupure de session, et elle tombe juste après `DevInfo` :
        // c'est la contrainte du matériel (cf. `SESSION_FERMEE_PAR_CC`). En
        // rajouter une par page coûterait 500 ms chacune pour rien.
        assert_eq!(transport.reinitialisations, vec![1]);
    }

    #[test]
    fn sans_coupure_de_session_len_tete_reste_muette() {
        // Ce test décrit l'échec réellement observé sur le logger : `DevInfo`
        // répond, puis `DataHeader` n'obtient rien du tout. Il fige la raison
        // d'être de `reset_session` — le supprimer casserait ce test avant de
        // casser le matériel.
        let mut transport = TransportRejoue::new(vec![fiche(2, 30), entete(2)]);
        let devinfo = transport
            .exchange(&dev_info_request(), DEV_INFO_LEN)
            .expect("la fiche répond");
        assert_eq!(devinfo.len(), DEV_INFO_LEN);
        assert!(matches!(
            transport.exchange(&data_header_request(1), DATA_HEADER_LEN),
            Err(SerialError::Timeout { received: 0, .. })
        ));

        // …et une session neuve débloque tout.
        transport.reset_session().unwrap();
        assert!(transport
            .exchange(&data_header_request(1), DATA_HEADER_LEN)
            .is_ok());
    }

    #[test]
    fn un_logger_vide_est_refuse_avant_toute_lecture_de_page() {
        let mut transport = TransportRejoue::new(vec![fiche(0, 30), entete(0)]);
        assert!(matches!(
            read_all(&mut transport, &JumpThresholds::default()),
            Err(SerialError::NoRecords)
        ));
        // Aucune page n'a été demandée : on ne lit pas dans le vide.
        assert_eq!(transport.envoyees.len(), 2);
    }

    #[test]
    fn un_desaccord_sur_le_nombre_de_mesures_est_signale_sans_bloquer() {
        // La fiche annonce 3 mesures, l'en-tête 2. C'est l'en-tête qui pilote.
        let mut transport =
            TransportRejoue::new(vec![fiche(3, 30), entete(2), page(&[100, 200])]);
        let lecture = read_all(&mut transport, &JumpThresholds::default()).unwrap();
        assert_eq!(lecture.measurements.len(), 2);
        assert!(lecture
            .warnings
            .iter()
            .any(|w| w.kind == ReadWarningKind::RecordCountMismatch));
    }

    #[test]
    fn un_saut_de_temperature_est_detecte_comme_pour_le_xlsx() {
        // Retrait du logger de l'extérieur : +12 °C en une cadence.
        let mut transport =
            TransportRejoue::new(vec![fiche(3, 30), entete(3), page(&[-50, -48, 72])]);
        let lecture = read_all(&mut transport, &JumpThresholds::default()).unwrap();
        assert_eq!(lecture.anomalies.len(), 1);
        assert_eq!(lecture.anomalies[0].delta_c, 12.0);
        assert_eq!(lecture.anomalies[0].source_line, 3);
    }

    // -----------------------------------------------------------------------
    // Vérification de l'horloge
    // -----------------------------------------------------------------------

    /// Référence commune aux tests d'horloge : un instant UTC quelconque mais fixe.
    fn reference() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 8, 21, 10, 0, 0).unwrap()
    }

    fn horloge(h: u32, m: u32, s: u32) -> Option<NaiveDateTime> {
        Some(
            NaiveDate::from_ymd_opt(2026, 8, 21)
                .unwrap()
                .and_hms_opt(h, m, s)
                .unwrap(),
        )
    }

    #[test]
    fn un_logger_regle_sur_utc_et_a_l_heure_ne_derive_pas() {
        let c = compare_clock(horloge(10, 0, 0), reference(), 0);
        assert_eq!(c.offset_vs_utc_seconds, Some(0));
        assert_eq!(c.drift_seconds, Some(0));
        assert_eq!(c.severity, ClockDriftSeverity::Aligned);
        assert_eq!(c.suggested_clock_offset_seconds, Some(0));
    }

    /// Le cas qui justifie le paramètre de fuseau : une heure d'écart peut être
    /// une horloge parfaite réglée sur UTC+1, ou une horloge qui avance d'une
    /// heure. Le backend ne tranche pas tout seul.
    #[test]
    fn une_heure_d_ecart_n_est_une_derive_que_si_le_fuseau_est_utc() {
        let vu_comme_utc = compare_clock(horloge(11, 0, 0), reference(), 0);
        assert_eq!(vu_comme_utc.drift_seconds, Some(3600));
        assert_eq!(vu_comme_utc.severity, ClockDriftSeverity::Severe);

        let vu_comme_utc_plus_1 = compare_clock(horloge(11, 0, 0), reference(), 3600);
        assert_eq!(vu_comme_utc_plus_1.offset_vs_utc_seconds, Some(3600));
        assert_eq!(vu_comme_utc_plus_1.drift_seconds, Some(0));
        assert_eq!(vu_comme_utc_plus_1.severity, ClockDriftSeverity::Aligned);
    }

    #[test]
    fn les_trois_niveaux_de_gravite_suivent_leurs_seuils() {
        let a = compare_clock(horloge(10, 0, 45), reference(), 0);
        assert_eq!(a.severity, ClockDriftSeverity::Aligned);

        let n = compare_clock(horloge(10, 5, 0), reference(), 0);
        assert_eq!(n.severity, ClockDriftSeverity::Notable);

        let s = compare_clock(horloge(10, 20, 0), reference(), 0);
        assert_eq!(s.severity, ClockDriftSeverity::Severe);
    }

    /// L'invariant qui relie cette page à l'import : appliquer la correction
    /// suggérée à l'horodatage brut doit rendre de l'UTC vrai.
    ///
    /// Un logger réglé sur UTC+1 et en retard de 120 s date une mesure prise à
    /// 10:00:00 UTC à 10:58:00. Ajouter la correction suggérée doit ramener à
    /// 10:00:00 exactement.
    #[test]
    fn la_correction_suggeree_ramene_bien_a_l_utc_vrai() {
        let c = compare_clock(horloge(10, 58, 0), reference(), 3600);
        assert_eq!(c.drift_seconds, Some(-120));

        let brut = horloge(10, 58, 0).unwrap();
        let correction = c.suggested_clock_offset_seconds.unwrap();
        let corrige = brut + chrono::Duration::seconds(correction);
        assert_eq!(corrige, reference().naive_utc());
    }

    #[test]
    fn une_horloge_illisible_ne_fait_pas_echouer_la_verification() {
        let c = compare_clock(None, reference(), 3600);
        assert_eq!(c.severity, ClockDriftSeverity::Unreadable);
        assert_eq!(c.drift_seconds, None);
        assert_eq!(c.suggested_clock_offset_seconds, None);
        assert!(!c.message.is_empty(), "l'UI doit avoir quelque chose à dire");
    }

    #[test]
    fn le_message_dit_le_sens_du_decalage() {
        let avance = compare_clock(horloge(10, 5, 0), reference(), 0);
        assert!(avance.message.contains("avance"), "{}", avance.message);
        let retard = compare_clock(horloge(9, 55, 0), reference(), 0);
        assert!(retard.message.contains("retard"), "{}", retard.message);
    }

    // -----------------------------------------------------------------------
    // Réinitialisation
    // -----------------------------------------------------------------------

    /// Fiche d'identité annonçant un autre modèle que le RC-5.
    fn fiche_autre_modele() -> Vec<u8> {
        let mut t = DEV_INFO_REEL;
        t[3] = 40; // RC-4
        t[159] = checksum(&t[..159]);
        t.to_vec()
    }

    #[test]
    fn la_requete_d_ecriture_construit_la_trame_de_25_octets() {
        let fiche_avant = fiche(10, 60);
        let requete = param_put_request(&fiche_avant).unwrap();

        assert_eq!(requete.len(), 25, "la trame de configuration fait 25 octets");
        assert_eq!(requete[0], 0x33, "famille de commandes 0x33");
        assert_eq!(requete[1], fiche_avant[1], "numéro de station de l'appareil");
        assert_eq!(requete[2], CMD_PARAM_PUT[0]);
        assert_eq!(requete[3], CMD_PARAM_PUT[1]);
        assert_eq!(
            *requete.last().unwrap(),
            checksum(&requete[..24]),
            "la somme de contrôle est celle du protocole"
        );
    }

    #[test]
    fn une_fiche_abimee_ne_produit_aucune_requete_d_ecriture() {
        let mut fiche_avant = fiche(10, 60);
        fiche_avant[159] ^= 0xFF; // somme fausse
        assert!(matches!(
            param_put_request(&fiche_avant),
            Err(SerialError::BadChecksum { .. })
        ));
    }

    #[test]
    fn l_effacement_enchaine_lire_ecrire_relire_et_verifie_le_resultat() {
        let mut t = TransportRejoue::new(vec![fiche(10_307, 60), fiche(0, 60)]);
        let rapport = clear_memory(&mut t).unwrap();

        assert_eq!(rapport.before.rec_count, 10_307);
        assert!(rapport.memory_cleared());
        assert!(rapport.parameters_preserved(), "{:?}", rapport.changed_parameters);

        // Trois trames émises, dans cet ordre, et deux coupures de session : la
        // règle du `0xCC` ne souffre aucune exception (cf. SESSION_FERMEE_PAR_CC).
        assert_eq!(t.envoyees.len(), 3);
        assert_eq!(t.envoyees[0], dev_info_request());
        assert_eq!(t.envoyees[1][..4], [0x33, 0x01, CMD_PARAM_PUT[0], CMD_PARAM_PUT[1]]);
        assert_eq!(t.envoyees[2], dev_info_request());
        assert_eq!(t.reinitialisations, vec![1, 2]);
    }

    /// La réussite n'est jamais déduite de l'absence d'erreur : elle est lue dans
    /// la fiche relue. Un logger qui n'obtempère pas doit le dire.
    #[test]
    fn un_effacement_sans_effet_ressort_en_erreur() {
        let mut t = TransportRejoue::new(vec![fiche(10_307, 60), fiche(10_307, 60)]);
        assert_eq!(
            clear_memory(&mut t),
            Err(SerialError::ResetNotApplied {
                remaining: 10_307
            })
        );
    }

    /// Le garde-fou de la seule commande d'écriture du projet : si l'appareil
    /// ressort avec un réglage différent, on le nomme au lieu de s'en remettre à
    /// la chance.
    #[test]
    fn un_reglage_modifie_au_passage_est_nomme() {
        // Même appareil, mais une cadence de 30 s au lieu de 60 s après coup.
        let mut t = TransportRejoue::new(vec![fiche(10_307, 60), fiche(0, 30)]);
        let rapport = clear_memory(&mut t).unwrap();

        assert!(rapport.memory_cleared());
        assert!(!rapport.parameters_preserved());
        assert_eq!(rapport.changed_parameters.len(), 1);
        assert!(
            rapport.changed_parameters[0].contains("cadence"),
            "{:?}",
            rapport.changed_parameters
        );
        assert!(
            rapport.message().contains("ElitechLog"),
            "le message doit dire où reprogrammer l'appareil"
        );
    }

    /// Rien n'est écrit sur un appareil qui n'est pas un RC-5 : le refus tombe
    /// après la lecture d'identité, avant la moindre requête `0x33`.
    #[test]
    fn aucune_ecriture_sur_un_appareil_qui_n_est_pas_un_rc5() {
        let mut t = TransportRejoue::new(vec![fiche_autre_modele()]);
        assert_eq!(
            clear_memory(&mut t),
            Err(SerialError::UnsupportedModel { model_no: 40 })
        );
        assert_eq!(t.envoyees.len(), 1, "seule la fiche d'identité a été demandée");
        assert!(t.reinitialisations.is_empty());
    }

    /// Comme [`fiche`], mais en imposant l'octet d'état de marche (offset 19).
    /// `0x01` = en cours d'enregistrement, `0x02` = arrêté.
    fn fiche_etat(rec_count: u16, interval_s: u8, work_status: u8) -> Vec<u8> {
        let mut t = fiche(rec_count, interval_s);
        t[19] = work_status;
        let n = t.len();
        t[n - 1] = checksum(&t[..n - 1]);
        t
    }

    /// Le cas rencontré sur le terrain : l'utilisateur doit effacer un logger qui
    /// enregistre encore. L'effacement ne doit plus être bloqué — mais l'arrêt
    /// doit précéder l'écriture, et non l'inverse.
    #[test]
    fn un_logger_en_cours_est_arrete_avant_d_etre_efface() {
        let mut t = TransportRejoue::new(vec![
            fiche_etat(10_307, 60, 0x01), // état avant : en cours
            fiche_etat(10_307, 60, 0x02), // après l'arrêt : arrêté, mesures intactes
            fiche_etat(0, 60, 0x02),      // après l'effacement
        ]);
        let rapport = clear_memory(&mut t).unwrap();

        assert!(rapport.recording_stopped, "l'arrêt automatique doit être signalé");
        assert_eq!(rapport.before.rec_count, 10_307);
        assert!(rapport.memory_cleared());
        assert!(
            rapport.parameters_preserved(),
            "l'arrêt ne doit rien changer d'autre : {:?}",
            rapport.changed_parameters
        );

        // Cinq trames : lire, arrêter, relire (preuve de l'arrêt), écrire, relire.
        // L'ordre est le fond du correctif — l'arrêt vient *avant* l'écriture.
        assert_eq!(t.envoyees.len(), 5);
        assert_eq!(t.envoyees[0], dev_info_request());
        assert_eq!(t.envoyees[1], stop_request(1));
        assert_eq!(t.envoyees[2], dev_info_request());
        assert_eq!(t.envoyees[3][..4], [0x33, 0x01, CMD_PARAM_PUT[0], CMD_PARAM_PUT[1]]);
        assert_eq!(t.envoyees[4], dev_info_request());
        // Une coupure de session avant chaque trame qui suit un échange : la
        // règle du `0xCC` vaut aussi pour les deux sessions ajoutées.
        assert_eq!(t.reinitialisations, vec![1, 2, 3, 4]);
    }

    /// Le gabarit renvoyé à l'appareil doit être la fiche relue *après* l'arrêt,
    /// pas celle d'avant : sinon l'écriture lui réimposerait l'état « en cours »
    /// qu'il vient de quitter.
    #[test]
    fn le_gabarit_ecrit_est_la_fiche_d_apres_l_arret() {
        let mut t = TransportRejoue::new(vec![
            fiche_etat(10_307, 60, 0x01),
            fiche_etat(10_307, 60, 0x02),
            fiche_etat(0, 60, 0x02),
        ]);
        clear_memory(&mut t).unwrap();

        let attendu = param_put_request(&fiche_etat(10_307, 60, 0x02)).unwrap();
        assert_eq!(t.envoyees[3], attendu);
    }

    /// Si l'arrêt automatique n'est pas confirmé, rien ne doit être écrit : mieux
    /// vaut une erreur claire qu'un effacement à l'aveugle sur un appareil dont
    /// on ignore l'état.
    #[test]
    fn un_arret_non_confirme_annule_l_effacement() {
        let mut t = TransportRejoue::new(vec![
            fiche_etat(10_307, 60, 0x01),
            fiche_etat(10_307, 60, 0x01), // toujours en cours : l'arrêt n'a rien fait
        ]);
        assert_eq!(
            clear_memory(&mut t),
            Err(SerialError::StopNotApplied {
                etat: WorkStatus::Running.label_fr(),
            })
        );
        assert!(
            !t.envoyees.iter().any(|r| r.len() > 3 && r[2..4] == CMD_PARAM_PUT),
            "aucune écriture ne doit partir quand l'arrêt n'est pas confirmé"
        );
    }

    /// Symétrique : un logger déjà arrêté ne doit pas recevoir de commande
    /// d'arrêt superflue — la séquence historique reste inchangée.
    #[test]
    fn un_logger_deja_arrete_n_est_pas_arrete_une_seconde_fois() {
        let mut t = TransportRejoue::new(vec![fiche(10_307, 60), fiche(0, 60)]);
        let rapport = clear_memory(&mut t).unwrap();

        assert!(!rapport.recording_stopped);
        assert_eq!(t.envoyees.len(), 3);
        assert!(!t.envoyees.iter().any(|r| r.len() > 2 && r[2] == CMD_STOP));
    }

    /// Le bilan doit dire que l'appareil a été arrêté : sur un RC-5, il ne
    /// redémarrera pas seul, et repartir sur le terrain sans le savoir coûterait
    /// une campagne de mesures.
    #[test]
    fn le_bilan_previent_que_le_logger_a_ete_arrete() {
        let mut t = TransportRejoue::new(vec![
            fiche_etat(10_307, 60, 0x01),
            fiche_etat(10_307, 60, 0x02),
            fiche_etat(0, 60, 0x02),
        ]);
        let message = clear_memory(&mut t).unwrap().message();
        assert!(message.contains("arrêté"), "{message}");
        assert!(message.contains("ElitechLog"), "{message}");
    }

    #[test]
    fn le_bilan_rappelle_que_la_base_n_est_pas_touchee() {
        let mut t = TransportRejoue::new(vec![fiche(10_307, 60), fiche(0, 60)]);
        let rapport = clear_memory(&mut t).unwrap();
        assert!(
            rapport.message().contains("importées dans l'application"),
            "{}",
            rapport.message()
        );
    }

}

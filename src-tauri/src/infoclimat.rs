//! Intégration de l'API Open Data d'Infoclimat.
//!
//! ## Une seule source, plusieurs stations
//!
//! Ce module couvre **toutes** les stations interrogées via Infoclimat, sans
//! distinction : les Vantage Pro 2 du réseau StatIC (Allières, Lans-en-Vercors,
//! Méaudre, Darbounouse, Losence) **et** la Crête de la Grande Moucherolle
//! (`STATIC0479`).
//!
//! La Moucherolle est physiquement un capteur Dragino sur LoRa, republié par
//! Orange Live Objects (`"source": "liveobjects"` dans les enregistrements) — mais
//! elle ressort d'Infoclimat au format commun. **Il n'existe donc pas de « format
//! Dragino » à gérer** : un seul parseur, un seul [`SourceType::InfoclimatApi`].
//! La variante `SourceType::DraginoApi` reste réservée à un hypothétique accès
//! *direct* à Live Objects, que rien n'utilise aujourd'hui.
//!
//! ## Le temps : aucune ambiguïté ici
//!
//! `dh_utc` est de l'**UTC réel**, au format `"YYYY-MM-DD HH:MM:SS"`, avec un mois
//! toujours en deuxième position. Toute la machinerie de détection jour/mois bâtie
//! pour les fichiers Elitech ([`crate::parsing::DateOrderScanner`]) est donc
//! court-circuitée : une série Infoclimat a toujours `clock_offset_seconds = 0`,
//! `raw_timestamp_was_utc = true` et `date_format_uncertain = false`. Voir
//! [`infoclimat_date_evidence`].
//!
//! ## Pièges du format, constatés sur un export réel
//!
//! Le fichier de référence est `Clee.API/Moucherolle.json` (55 jours à la cadence
//! de 10 minutes, 7905 enregistrements). Quatre choses s'y voient :
//!
//! 1. **Les nombres sont des chaînes JSON** : `"temperature": "13.8"`, pas `13.8`.
//!    D'où [`Scalar`], qui accepte les deux encodages — on ne sait pas si toutes
//!    les stations se comportent pareil.
//! 2. **`complements` est du JSON dans du JSON** :
//!    `"{\"rssi\":-86,\"lat\":45.0093,\"lon\":5.5575,\"batt\":40.44}"`. Il faut le
//!    re-décoder après le premier passage. ⚠️ La description de ce champ dans le
//!    bloc `metadata` du même fichier annonce un format à 33 champs séparés par
//!    des `@` : **elle ne correspond pas au contenu réel** et doit être ignorée.
//! 3. **La quasi-totalité des 29 champs valent `null`** pour cette station
//!    (pression, vent, pluie, UV…). Tout est donc `Option`, rien n'est supposé
//!    présent.
//! 4. **`hourly` n'est pas qu'un dictionnaire de stations** : il contient aussi une
//!    clé `_params` (un tableau de noms de champs). Les clés techniques préfixées
//!    par `_` sont écartées.
//!
//! ## Deux formats de fichier, un seul chemin de lecture
//!
//! Le site d'Infoclimat propose l'export en **JSON** et en **CSV**. Les deux
//! portent exactement les mêmes 29 champs, dont `dh_utc` au même format. Il
//! aurait été absurde d'en faire deux parseurs : le lecteur CSV
//! ([`parse_infoclimat_csv`]) ne fait que **reconstruire des [`RawRecord`]**, puis
//! rejoint la chaîne commune `convertir` → `dedoublonner` → `intervalle_dominant`.
//! Tout ce qui décide de quelque chose — quelle ligne écarter, comment lire un
//! nombre, comment traiter `complements` — n'existe donc qu'en un exemplaire.
//!
//! [`parse_infoclimat_auto`] choisit le lecteur d'après le contenu (un JSON
//! commence par `{`), sans regarder l'extension du fichier.
//!
//! ### Ce que le CSV a de particulier (fichier réel : `export_infoclimat.csv`,
//! station `STATIC0213` — La Chapelle-en-Vercors / Combe de Darbounouse, 137 088
//! mesures)
//!
//! * **trois blocs** : des lignes de métadonnées préfixées `#`, une ligne
//!   d'intitulés, puis — piège — une **ligne d'unités**
//!   (`string;YYYY-MM-DD hh:mm:ss;°C;hPa;…`) qu'il ne faut surtout pas prendre
//!   pour une mesure ;
//! * séparateur `;` ;
//! * **`complements` y est du JSON simple**, pas doublement encodé comme dans le
//!   JSON : la valeur brute est directement `{"rssi":-109,…}`. Elle se relit donc
//!   avec le même `serde_json::from_str` que dans l'autre format, une passe en
//!   moins ;
//! * la licence n'est pas la même que celle de la Moucherolle : ce fichier
//!   annonce `Licence Ouverte/Open License`. Elle est lue dans les lignes `#`,
//!   jamais supposée.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::elitech_import::ImportWarning;
use crate::fingerprint::FingerprintSample;
use crate::stats::TemperatureSample;
use crate::parsing::{DateOrder, DateOrderEvidence};

/// Mention d'attribution affichée dès qu'une donnée Infoclimat est présentée.
///
/// La Moucherolle est diffusée sous `NON-COMMERCIAL ONLY: CC BY NC` : l'attribution
/// est **exigée**, pas optionnelle. Elle apparaît sur la fiche du point et devra
/// être reprise telle quelle dans l'export HTML (chantier séparé).
pub const ATTRIBUTION: &str = "Données Infoclimat (infoclimat.fr) — CC BY-NC, réutilisation non commerciale";

/// Nombre maximum de jours **consécutifs** acceptés par requête.
///
/// Limite affichée sur <https://www.infoclimat.fr/opendata/> : « 7 jours
/// consécutifs maximum, sauf si vous êtes propriétaire de la station ». Une
/// demande plus longue est découpée par [`split_windows`].
pub const MAX_DAYS_PER_REQUEST: i64 = 7;

// ---------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum InfoclimatError {
    /// **Le cas à ne pas confondre avec autre chose.** Voir [`AUTH_HELP`].
    #[error("{0}")]
    Auth(String),

    #[error("Le serveur Infoclimat a répondu {status} : {body}")]
    Http { status: u16, body: String },

    #[error("Appel à l'API Infoclimat impossible : {0}")]
    Network(String),

    #[error("Réponse Infoclimat illisible : {0}")]
    Json(String),

    #[error("L'API Infoclimat a renvoyé une erreur : {0}")]
    Api(String),

    #[error(
        "La station « {0} » n'est pas dans la réponse. Vérifiez l'identifiant \
         (il ressemble à « STATIC0479 ») et que la station diffuse bien ses données \
         en open data sur la période demandée."
    )]
    StationAbsente(String),

    #[error("Aucune clé d'API Infoclimat trouvée. {0}")]
    CleIntrouvable(String),

    #[error("Relai SSH Infoclimat : {0}")]
    SshRelay(String),

    #[error("Période invalide : la date de fin ({1}) précède la date de début ({0}).")]
    PeriodeInvalide(NaiveDate, NaiveDate),
}

/// Message d'erreur d'authentification, volontairement long.
///
/// L'API ne dit **pas** « mauvaise IP » de façon exploitable : elle répond
/// `HTTP 200` avec le corps texte `Wrong ip address`, ce qui fait planter tout
/// client qui suppose du JSON. Le vrai diagnostic — « votre IP publique a changé »
/// — n'est écrit nulle part dans sa réponse. C'est donc à nous de l'expliquer,
/// sinon l'utilisateur cherchera du côté de la clé alors que la clé est bonne.
pub const AUTH_HELP: &str = "\
La clé d'API Infoclimat a été refusée.

CAUSE LA PLUS PROBABLE : l'adresse IP publique de ce poste a changé. Infoclimat lie \
chaque clé à l'IP déclarée au moment de sa génération ; si la box a redémarré, si \
l'opérateur a renouvelé le bail, ou si l'appel part d'un autre réseau (VPN, partage de \
connexion, autre lieu), la clé cesse de fonctionner — alors qu'elle est toujours valide \
en soi. Le message renvoyé par l'API ne le dit pas clairement, c'est le piège.

À FAIRE : ouvrir https://www.infoclimat.fr/opendata/ depuis CE poste, se connecter, \
regénérer une clé, puis remplacer le contenu de Clee.API/API.Infoclimat.env.
Attention : c'est l'IP publique de la machine qui exécute la requête qu'il faut \
déclarer, pas l'adresse locale de la box ou du routeur.

AUTRE POSSIBILITÉ, moins fréquente : la clé elle-même est fausse, tronquée ou révoquée. \
L'écran Paramètres affiche ses derniers caractères et sa longueur — de quoi vérifier \
d'un coup d'œil qu'elle correspond bien à celle générée sur le site.";

/// Fabrique l'erreur d'authentification, en y joignant ce que le serveur a dit.
pub fn erreur_authentification(reponse_serveur: &str) -> InfoclimatError {
    let extrait = extrait_lisible(reponse_serveur, 200);
    if extrait.is_empty() {
        InfoclimatError::Auth(AUTH_HELP.to_string())
    } else {
        InfoclimatError::Auth(format!(
            "{AUTH_HELP}\n\nRéponse brute du serveur : « {extrait} »"
        ))
    }
}

/// Coupe proprement un corps de réponse pour l'afficher dans un message d'erreur.
fn extrait_lisible(raw: &str, max: usize) -> String {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max {
        compact
    } else {
        let tronque: String = compact.chars().take(max).collect();
        format!("{tronque}…")
    }
}

/// Motifs qui désignent un refus d'authentification, quel que soit le code HTTP.
///
/// Les deux premiers ont été **observés en vrai** le 19 août 2026, et ils ne se
/// ressemblent pas du tout :
///
/// | Cas | Code HTTP | Corps |
/// |---|---|---|
/// | IP non déclarée | `200 OK` | texte brut `Wrong ip address` |
/// | Clé inconnue | `400` | JSON `{"errors":["No account found for this token: '…'"]}` |
///
/// Autrement dit : ni l'un ni l'autre n'est un `401`/`403`, et le premier n'est
/// même pas une erreur du point de vue du protocole. D'où la reconnaissance par
/// le contenu. Les motifs suivants sont des filets de sécurité si les libellés
/// changent.
const MOTIFS_AUTH: [&str; 7] = [
    "wrong ip address",
    "no account found",
    "wrong ip",
    "invalid token",
    "bad token",
    "token invalide",
    "unauthorized",
];

/// Est-ce que ce corps de réponse sent le refus d'authentification ?
pub fn ressemble_a_un_refus_d_auth(body: &str) -> bool {
    let minuscules = body.to_lowercase();
    MOTIFS_AUTH.iter().any(|m| minuscules.contains(m))
}

// ---------------------------------------------------------------------------
// Structures désérialisées
// ---------------------------------------------------------------------------

/// Valeur numérique de l'API, qui peut arriver en nombre **ou** en chaîne.
///
/// Sur la Moucherolle, `temperature` et `humidite` sont systématiquement des
/// chaînes (`"13.8"`, `"50"`). Rien ne garantit que toutes les stations du réseau
/// fassent pareil : accepter les deux formes coûte dix lignes et évite un import
/// vide inexplicable le jour où une VP2 répondra en nombres.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Scalar {
    Nombre(f64),
    Texte(String),
}

impl Scalar {
    fn as_f64(&self) -> Option<f64> {
        match self {
            Scalar::Nombre(v) => Some(*v),
            Scalar::Texte(t) => {
                let t = t.trim();
                if t.is_empty() {
                    None
                } else {
                    // La virgule décimale n'a jamais été observée, mais elle ne
                    // coûte rien à tolérer.
                    t.replace(',', ".").parse::<f64>().ok()
                }
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RawRoot {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    errors: Option<serde_json::Value>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    stations: Vec<RawStation>,
    #[serde(default)]
    hourly: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawStation {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    latitude: Option<Scalar>,
    #[serde(default)]
    longitude: Option<Scalar>,
    #[serde(default)]
    elevation: Option<Scalar>,
    #[serde(default)]
    license: Option<RawLicense>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawLicense {
    /// Attention au nom : `license.license` porte le libellé, `license.code` un entier.
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawRecord {
    #[serde(default)]
    dh_utc: Option<String>,
    #[serde(default)]
    temperature: Option<Scalar>,
    #[serde(default)]
    humidite: Option<Scalar>,
    /// Chaîne contenant elle-même du JSON — cf. l'en-tête du module.
    #[serde(default)]
    complements: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

/// Contenu utile du champ `complements`, une fois le second décodage fait.
#[derive(Debug, Clone, Default, Deserialize)]
struct Complements {
    #[serde(default)]
    rssi: Option<Scalar>,
    #[serde(default)]
    batt: Option<Scalar>,
    /// Position de la station, telle que la publie la passerelle LoRa.
    ///
    /// Inutile côté JSON (le bloc `stations[]` la donne déjà, et mieux), mais
    /// c'est la **seule** source de position d'un export CSV : celui-ci n'a pas
    /// de bloc `stations`, juste quelques lignes `#`. Voir `station_du_csv`.
    #[serde(default)]
    lat: Option<Scalar>,
    #[serde(default)]
    lon: Option<Scalar>,
}

// ---------------------------------------------------------------------------
// Structures exposées
// ---------------------------------------------------------------------------

/// Identité d'une station telle que l'API la décrit (bloc `stations[]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StationInfo {
    pub id: String,
    pub name: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub elevation_m: Option<f64>,
    /// Libellé brut, ex. `"NON-COMMERCIAL ONLY: CC BY NC"`.
    pub license: Option<String>,
    pub license_url: Option<String>,
}

/// Une mesure lue dans la réponse, avant tout passage en base.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InfoclimatRecord {
    /// **UTC réel** : `dh_utc` est déjà en UTC, aucune conversion n'est appliquée.
    pub timestamp_utc: DateTime<Utc>,
    pub temperature_celsius: f64,
    pub humidity_percent: Option<f64>,
    pub rssi_dbm: Option<f64>,
    /// Valeur brute, unité indéterminée — cf. `model::Measurement::battery`.
    pub battery: Option<f64>,
    /// Chaîne d'origine (`"liveobjects"`, `"wlkIPxml"`…), purement documentaire.
    pub source: Option<String>,
}

// L'empreinte et les statistiques se calculent **sur l'enregistrement parsé** :
// la source est nativement UTC, donc l'instant brut est l'instant UTC, exactement
// comme dans le `Measurement::new_utc` qui en sera tiré à l'écriture.
impl FingerprintSample for InfoclimatRecord {
    fn raw_timestamp(&self) -> DateTime<Utc> {
        self.timestamp_utc
    }
    fn temperature_celsius(&self) -> f64 {
        self.temperature_celsius
    }
    fn humidity_percent(&self) -> Option<f64> {
        self.humidity_percent
    }
}

impl TemperatureSample for InfoclimatRecord {
    fn instant(&self) -> DateTime<Utc> {
        self.timestamp_utc
    }
    fn temperature(&self) -> f64 {
        self.temperature_celsius
    }
}

/// Ce qu'une réponse (ou un fichier) Infoclimat contient, une fois analysée.
#[derive(Debug, Clone, Serialize)]
pub struct InfoclimatPayload {
    pub station_id: String,
    pub station: Option<StationInfo>,
    pub records: Vec<InfoclimatRecord>,
    pub warnings: Vec<ImportWarning>,
    /// Écart le plus fréquent entre deux mesures successives, en secondes.
    pub sample_interval_seconds: Option<i64>,
    /// Enregistrements écartés parce qu'ils portaient le même instant qu'un autre.
    pub duplicate_count: usize,
    /// Enregistrements dont le champ `complements` n'a pas pu être re-décodé.
    /// RSSI et batterie y sont simplement absents — aucune mesure n'est perdue.
    pub unreadable_complements: usize,
    /// Identifiants de station présents dans la réponse (aide au diagnostic).
    pub available_stations: Vec<String>,
    /// Lequel des deux formats a été lu. Affiché à l'utilisateur : c'est la
    /// confirmation que l'auto-détection a vu juste.
    #[serde(default)]
    pub format: InfoclimatFormat,
}

/// Format du contenu Infoclimat effectivement lu.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InfoclimatFormat {
    /// Réponse d'API ou fichier `.json` téléchargé sur le site.
    #[default]
    Json,
    /// Export `.csv` téléchargé sur le site (séparateur `;`).
    Csv,
}

impl InfoclimatFormat {
    pub fn label_fr(self) -> &'static str {
        match self {
            InfoclimatFormat::Json => "JSON",
            InfoclimatFormat::Csv => "CSV (séparateur « ; »)",
        }
    }
}

impl InfoclimatPayload {
    pub fn first_utc(&self) -> Option<DateTime<Utc>> {
        self.records.first().map(|r| r.timestamp_utc)
    }

    pub fn last_utc(&self) -> Option<DateTime<Utc>> {
        self.records.last().map(|r| r.timestamp_utc)
    }
}

/// Ce que le modèle doit inscrire dans `LoggerSeries.date_order` pour cette source.
///
/// Une série Infoclimat n'a **aucune** ambiguïté jour/mois : `dh_utc` est
/// `YYYY-MM-DD`, mois en deuxième position, sans exception. On renvoie donc une
/// preuve « ISO », ni incertaine ni contradictoire — ce qui garantit que l'UI
/// n'affichera jamais le bandeau « format de date à confirmer » sur ces séries.
pub fn infoclimat_date_evidence(record_count: usize) -> DateOrderEvidence {
    DateOrderEvidence {
        order: DateOrder::DayFirst,
        day_first_hits: 0,
        month_first_hits: 0,
        ambiguous_hits: 0,
        iso_hits: record_count,
        uncertain: false,
        conflicting: false,
        first_conflict_line: None,
        samples: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Parseur
// ---------------------------------------------------------------------------

/// Analyse une réponse Infoclimat **déjà en mémoire**.
///
/// La signature prend un `&str` et non un chemin : c'est ce qui permet aux deux
/// modes de récupération — appel direct à l'API et import d'un fichier téléchargé
/// à la main depuis le site — de partager exactement le même code. Aucun des deux
/// n'a de chemin de parsing qui lui soit propre.
///
/// `station_id` :
/// * `Some(id)` — on exige cette station, et son absence est une erreur explicite ;
/// * `None` — on prend l'unique station présente (le cas d'un fichier téléchargé
///   pour une seule station). S'il y en a plusieurs, l'appelant doit trancher :
///   deviner reviendrait à rattacher des mesures au mauvais point.
pub fn parse_infoclimat(
    content: &str,
    station_id: Option<&str>,
) -> Result<InfoclimatPayload, InfoclimatError> {
    let taille = content.trim();
    if taille.is_empty() {
        return Err(InfoclimatError::Json("réponse vide".to_string()));
    }

    // Un corps qui ne commence pas par `{` n'est pas du JSON : c'est le cas du
    // refus d'authentification, qui répond « Wrong ip address » en texte brut
    // avec un code HTTP 200. On le reconnaît ici pour que l'import d'un fichier
    // enregistré par erreur donne le même message clair que l'appel direct.
    if !taille.starts_with('{') {
        if ressemble_a_un_refus_d_auth(taille) {
            return Err(erreur_authentification(taille));
        }
        return Err(InfoclimatError::Json(format!(
            "le contenu ne commence pas par « {{ » — ce n'est pas du JSON Infoclimat. \
             Début reçu : « {} »",
            extrait_lisible(taille, 120)
        )));
    }

    let root: RawRoot = serde_json::from_str(taille)
        .map_err(|e| InfoclimatError::Json(format!("{e} (le fichier est-il complet ?)")))?;

    verifier_statut(&root)?;

    let available_stations: Vec<String> = root
        .hourly
        .keys()
        .filter(|k| !k.starts_with('_')) // `_params` n'est pas une station
        .cloned()
        .collect();

    let station_id = match station_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(demande) => demande.to_string(),
        None => match available_stations.as_slice() {
            [unique] => unique.clone(),
            [] => {
                return Err(InfoclimatError::Api(
                    "la réponse ne contient aucune station. La période demandée est \
                     peut-être hors de la plage diffusée par la station."
                        .to_string(),
                ))
            }
            plusieurs => {
                return Err(InfoclimatError::Api(format!(
                    "la réponse contient {} stations ({}). Précisez laquelle rattacher \
                     à ce point : la deviner risquerait d'attribuer des mesures au mauvais site.",
                    plusieurs.len(),
                    plusieurs.join(", ")
                )))
            }
        },
    };

    let brut = root
        .hourly
        .get(&station_id)
        .ok_or_else(|| InfoclimatError::StationAbsente(station_id.clone()))?;

    let bruts: Vec<RawRecord> = serde_json::from_value(brut.clone())
        .map_err(|e| InfoclimatError::Json(format!("enregistrements de {station_id} : {e}")))?;

    let station = root
        .stations
        .iter()
        .find(|s| s.id.as_deref() == Some(station_id.as_str()))
        .map(to_station_info);

    let (records, warnings, unreadable_complements) = convertir(&bruts);
    let (records, duplicate_count) = dedoublonner(records);
    let sample_interval_seconds = intervalle_dominant(&records);

    Ok(InfoclimatPayload {
        station_id,
        station,
        records,
        warnings,
        sample_interval_seconds,
        duplicate_count,
        unreadable_complements,
        available_stations,
        format: InfoclimatFormat::Json,
    })
}

// ---------------------------------------------------------------------------
// Aiguillage JSON / CSV
// ---------------------------------------------------------------------------

/// Analyse un contenu Infoclimat **quel que soit son format**.
///
/// C'est le point d'entrée que l'application utilise : le format est déduit du
/// **contenu** et non de l'extension du fichier (un `.txt` renommé depuis un
/// export CSV doit s'importer, un `.json` tronqué doit échouer en le disant).
///
/// La règle est celle du premier caractère significatif : `{` annonce du JSON,
/// tout le reste est tenté en CSV. Un refus d'authentification (`Wrong ip
/// address`, servi en texte brut avec un code HTTP 200) est reconnu avant, pour
/// qu'il produise le message d'aide et non une erreur de CSV incompréhensible.
pub fn parse_infoclimat_auto(
    content: &str,
    station_id: Option<&str>,
) -> Result<InfoclimatPayload, InfoclimatError> {
    let taille = content.trim_start_matches('\u{feff}').trim();
    if taille.is_empty() {
        return Err(InfoclimatError::Json("réponse vide".to_string()));
    }
    if !taille.starts_with('{') && ressemble_a_un_refus_d_auth(taille) {
        return Err(erreur_authentification(taille));
    }
    if taille.starts_with('{') {
        parse_infoclimat(content, station_id)
    } else {
        parse_infoclimat_csv(content, station_id)
    }
}

// ---------------------------------------------------------------------------
// Lecteur CSV
// ---------------------------------------------------------------------------

/// Le texte ressemble-t-il à un export Infoclimat ? JSON portant `dh_utc`, ou
/// CSV dont une ligne d'intitulés porte `dh_utc` (la règle du lecteur CSV).
pub fn ressemble_a_un_export(content: &str) -> bool {
    let texte = content.trim_start_matches('\u{feff}').trim_start();
    if texte.starts_with('{') {
        return texte.contains("\"dh_utc\"");
    }
    texte
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .take(50)
        .any(|l| decouper_csv(l).iter().any(|c| c.eq_ignore_ascii_case("dh_utc")))
}

/// Séparateur de l'export CSV d'Infoclimat. Fixe, pas détecté : le format est
/// connu, et `complements` contient des virgules qui piégeraient une
/// auto-détection.
pub const CSV_DELIMITER: char = ';';

/// Analyse un export CSV téléchargé sur infoclimat.fr.
///
/// **Ne parse rien de plus que le JSON** : il reconstruit des enregistrements
/// bruts et les remet dans la chaîne commune (`convertir`, `dedoublonner`,
/// `intervalle_dominant`). C'est ce qui garantit qu'une même mesure donne le
/// même résultat, quel que soit le fichier d'où elle vient.
///
/// Voir l'en-tête du module pour la structure du fichier (blocs `#`, ligne
/// d'unités) et pour ce que le CSV a de particulier.
pub fn parse_infoclimat_csv(
    content: &str,
    station_id: Option<&str>,
) -> Result<InfoclimatPayload, InfoclimatError> {
    if content.trim().is_empty() {
        return Err(InfoclimatError::Json("fichier vide".to_string()));
    }

    let mut metadonnees: Vec<(String, String)> = Vec::new();
    let mut entete: Option<(usize, Vec<String>)> = None;
    // Lignes de données **empruntées** au texte, découpées seulement au moment
    // de les lire : garder 29 `String` par ligne coûtait plus d'un Ko par mesure.
    let mut lignes_donnees: Vec<(usize, &str)> = Vec::new();
    let mut unites_sautees = false;

    for (index, ligne) in content.lines().enumerate() {
        let brut = ligne.trim_start_matches('\u{feff}');
        if brut.trim().is_empty() {
            continue;
        }

        // Bloc 1 : les lignes de métadonnées, de la forme `# clé: valeur`.
        if let Some(reste) = brut.trim_start().strip_prefix('#') {
            if let Some((cle, valeur)) = reste.split_once(':') {
                let cle = cle.trim();
                let valeur = valeur.trim();
                if !cle.is_empty() && !valeur.is_empty() {
                    metadonnees.push((cle.to_string(), valeur.to_string()));
                }
            }
            continue;
        }

        let champs = decouper_csv(brut);

        // Bloc 2 : la ligne d'intitulés, reconnue à la présence de `dh_utc`.
        if entete.is_none() {
            if champs.iter().any(|c| c.eq_ignore_ascii_case("dh_utc")) {
                entete = Some((index + 1, champs.iter().map(|c| c.to_string()).collect()));
            }
            continue;
        }

        // Bloc 3 : les données — précédées d'une ligne d'unités qu'il faut
        // sauter. Elle n'est **pas** sautée aveuglément : on vérifie que son
        // champ `dh_utc` n'est pas une date. Un export qui ne la porterait pas
        // ne perdrait donc pas sa première mesure.
        if !unites_sautees {
            unites_sautees = true;
            let (_, colonnes) = entete.as_ref().expect("l'en-tête est lu");
            let position = colonnes
                .iter()
                .position(|c| c.eq_ignore_ascii_case("dh_utc"));
            let est_unites = position
                .and_then(|i| champs.get(i))
                .map(|v| parse_dh_utc(v).is_none())
                .unwrap_or(true);
            if est_unites {
                continue;
            }
        }

        lignes_donnees.push((index + 1, brut));
    }

    let Some((ligne_entete, colonnes)) = entete else {
        return Err(InfoclimatError::Json(
            "aucune ligne d'intitulés reconnue : un export CSV d'Infoclimat contient une \
             ligne « station_id;dh_utc;temperature;… ». S'il s'agit d'un autre format \
             tabulaire, utilisez l'import manuel (colonnes mappées)."
                .to_string(),
        ));
    };

    let colonne = |nom: &str| colonnes.iter().position(|c| c.eq_ignore_ascii_case(nom));
    let col_dh = colonne("dh_utc").ok_or_else(|| {
        InfoclimatError::Json(format!(
            "la ligne d'intitulés (ligne {ligne_entete}) ne porte pas de colonne `dh_utc`"
        ))
    })?;
    let col_station = colonne("station_id");
    let col_temperature = colonne("temperature");
    let col_humidite = colonne("humidite");
    let col_complements = colonne("complements");
    let col_source = colonne("source");

    // Les stations présentes, dans l'ordre d'apparition — un export peut en
    // contenir plusieurs, comme une réponse d'API.
    let mut available_stations: Vec<String> = Vec::new();
    for (_, ligne) in &lignes_donnees {
        let champs = decouper_csv(ligne);
        if let Some(id) = col_station.and_then(|i| champs.get(i)).map(|s| s.trim()) {
            if !id.is_empty() && !available_stations.iter().any(|s| s == id) {
                available_stations.push(id.to_string());
            }
        }
    }

    let station_id = match station_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(demande) => demande.to_string(),
        None => match available_stations.as_slice() {
            [unique] => unique.clone(),
            // Sans colonne `station_id`, le fichier ne concerne qu'une station
            // qu'il ne nomme pas : on prend celle annoncée en métadonnées.
            [] => station_des_metadonnees(&metadonnees).ok_or_else(|| {
                InfoclimatError::Api(
                    "le fichier ne nomme aucune station, ni en colonne `station_id` \
                     ni dans ses lignes de métadonnées. Indiquez l'identifiant à la main."
                        .to_string(),
                )
            })?,
            plusieurs => {
                return Err(InfoclimatError::Api(format!(
                    "le fichier contient {} stations ({}). Précisez laquelle rattacher \
                     à ce point : la deviner risquerait d'attribuer des mesures au mauvais site.",
                    plusieurs.len(),
                    plusieurs.join(", ")
                )))
            }
        },
    };

    if !available_stations.is_empty() && !available_stations.contains(&station_id) {
        return Err(InfoclimatError::StationAbsente(station_id));
    }

    // Reconstruction des enregistrements bruts : **la seule chose que ce lecteur
    // fait de spécifique**. La conversion est le chemin commun avec le JSON
    // ([`Conversion`]) ; elle se fait ligne à ligne, sans garder la liste des
    // enregistrements bruts — elle doublait la mémoire d'un gros export.
    let mut conversion = Conversion::avec_capacite(lignes_donnees.len());
    let mut bruts_retenus = 0usize;
    let mut position: Option<(f64, f64)> = None;

    for (numero, ligne) in &lignes_donnees {
        let champs = decouper_csv(ligne);
        if let (Some(i), false) = (col_station, available_stations.is_empty()) {
            if champs.get(i).map(|s| s.trim()) != Some(station_id.as_str()) {
                continue;
            }
        }
        let brut = RawRecord {
            dh_utc: champs.get(col_dh).map(|v| v.trim().to_string()),
            temperature: texte_scalaire(&champs, col_temperature),
            humidite: texte_scalaire(&champs, col_humidite),
            complements: col_complements
                .and_then(|i| champs.get(i))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            source: col_source
                .and_then(|i| champs.get(i))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
        };
        if position.is_none() {
            position = position_des_complements(&brut);
        }
        conversion.ajouter(&brut, *numero);
        bruts_retenus += 1;
    }
    drop(lignes_donnees);

    if bruts_retenus == 0 {
        return Err(InfoclimatError::StationAbsente(station_id));
    }

    let Conversion {
        records,
        warnings,
        complements_illisibles: unreadable_complements,
    } = conversion;
    let (records, duplicate_count) = dedoublonner(records);
    let sample_interval_seconds = intervalle_dominant(&records);
    let station = station_du_csv(&station_id, &metadonnees, position);

    Ok(InfoclimatPayload {
        station_id,
        station,
        records,
        warnings,
        sample_interval_seconds,
        duplicate_count,
        unreadable_complements,
        available_stations,
        format: InfoclimatFormat::Csv,
    })
}

/// Découpe une ligne CSV. Aucun champ n'est cité dans l'export réel — pas même
/// `complements`, qui contient pourtant des virgules (sans effet : le séparateur
/// est `;`). Un découpage simple suffit donc, et ne cache rien.
fn decouper_csv(ligne: &str) -> Vec<&str> {
    ligne
        .trim_end_matches(['\r', '\n'])
        .split(CSV_DELIMITER)
        .map(str::trim)
        .collect()
}

/// Valeur d'une colonne facultative, sous la forme que [`Scalar`] attend.
///
/// Le CSV n'a que du texte : on emballe en `Scalar::Texte`, et c'est
/// `Scalar::as_f64` — le même code que pour le JSON — qui décide si c'est un
/// nombre. Une cellule vide devient `None`, jamais `0`.
fn texte_scalaire(champs: &[&str], colonne: Option<usize>) -> Option<Scalar> {
    colonne
        .and_then(|i| champs.get(i))
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(|v| Scalar::Texte(v.to_string()))
}

/// Identifiant de station annoncé par les lignes `#`.
///
/// Le fichier réel écrit `# STATIC0213: La-Chapelle-en-Vercors - Combe de
/// Darbounouse` : la **clé** est l'identifiant, la valeur est le nom.
fn station_des_metadonnees(metadonnees: &[(String, String)]) -> Option<String> {
    metadonnees
        .iter()
        .map(|(cle, _)| cle.trim())
        .find(|cle| {
            let majuscules = cle.to_uppercase();
            majuscules.starts_with("STATIC") || majuscules.starts_with("STATION")
        })
        .map(|cle| cle.to_string())
}

/// Identité de la station telle qu'un CSV permet de la reconstituer.
///
/// Un CSV n'a pas de bloc `stations[]` : le nom et la licence viennent des lignes
/// `#`, et la position — quand elle existe — du champ `complements` de la
/// première mesure qui en porte une ([`position_des_complements`], appliquée
/// au fil de la lecture). Rien n'est inventé : chaque champ absent reste `None`.
fn station_du_csv(
    station_id: &str,
    metadonnees: &[(String, String)],
    position: Option<(f64, f64)>,
) -> Option<StationInfo> {
    let nom = metadonnees
        .iter()
        .find(|(cle, _)| cle.eq_ignore_ascii_case(station_id))
        .map(|(_, valeur)| valeur.clone());

    // « Licence Ouverte/Open License: http://… » — la clé porte le libellé, la
    // valeur l'URL. C'est l'inverse de la convention du JSON, où `license` est un
    // objet ; d'où ce traitement à part plutôt qu'un partage forcé.
    let licence = metadonnees.iter().find(|(cle, _)| {
        let c = cle.to_lowercase();
        c.contains("licence") || c.contains("license")
    });

    if nom.is_none() && licence.is_none() && position.is_none() {
        return None;
    }

    Some(StationInfo {
        id: station_id.to_string(),
        name: nom,
        latitude: position.map(|(lat, _)| lat),
        longitude: position.map(|(_, lon)| lon),
        // Un CSV n'annonce jamais d'altitude : ne rien inventer.
        elevation_m: None,
        license: licence.map(|(cle, _)| cle.clone()),
        license_url: licence.map(|(_, valeur)| valeur.clone()),
    })
}

/// Position (lat, lon) portée par le champ `complements` d'un enregistrement,
/// si elle y est complète et lisible.
fn position_des_complements(brut: &RawRecord) -> Option<(f64, f64)> {
    let c = serde_json::from_str::<Complements>(brut.complements.as_deref()?).ok()?;
    let lat = c.lat.as_ref().and_then(Scalar::as_f64)?;
    let lon = c.lon.as_ref().and_then(Scalar::as_f64)?;
    Some((lat, lon))
}

/// Refuse une réponse dont l'enveloppe annonce déjà un échec.
///
/// `status` vaut `"OK"` sur une réponse saine. `errors` est un tableau vide dans
/// ce cas — il faut donc distinguer « vide » de « absent », un `[]` ne devant pas
/// être pris pour une erreur.
fn verifier_statut(root: &RawRoot) -> Result<(), InfoclimatError> {
    let erreurs = root
        .errors
        .as_ref()
        .map(decrire_erreurs)
        .unwrap_or_default();

    let statut_ko = root
        .status
        .as_deref()
        .map(|s| !s.eq_ignore_ascii_case("OK"))
        .unwrap_or(false);

    if erreurs.is_empty() && !statut_ko {
        return Ok(());
    }

    let detail = if erreurs.is_empty() {
        root.message
            .clone()
            .unwrap_or_else(|| format!("statut « {} »", root.status.as_deref().unwrap_or("?")))
    } else {
        erreurs
    };

    if ressemble_a_un_refus_d_auth(&detail) {
        return Err(erreur_authentification(&detail));
    }
    Err(InfoclimatError::Api(detail))
}

/// Aplatit le champ `errors`, qui peut être un tableau, un objet ou une chaîne.
fn decrire_erreurs(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Array(items) if items.is_empty() => String::new(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(decrire_erreurs)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ; "),
        serde_json::Value::String(s) => s.trim().to_string(),
        autre => autre.to_string(),
    }
}

fn to_station_info(s: &RawStation) -> StationInfo {
    StationInfo {
        id: s.id.clone().unwrap_or_default(),
        name: s.name.clone(),
        latitude: s.latitude.as_ref().and_then(Scalar::as_f64),
        longitude: s.longitude.as_ref().and_then(Scalar::as_f64),
        elevation_m: s.elevation.as_ref().and_then(Scalar::as_f64),
        license: s.license.as_ref().and_then(|l| l.license.clone()),
        license_url: s.license.as_ref().and_then(|l| l.url.clone()),
    }
}

/// Transforme les enregistrements bruts en mesures exploitables.
///
/// Une ligne est écartée — avec un avertissement listé à l'utilisateur — si son
/// horodatage est illisible ou si la température manque : `temperature_celsius`
/// est `NOT NULL` en base, une mesure sans température n'est pas une mesure.
/// Tout le reste (humidité, RSSI, batterie) est optionnel et n'écarte rien.
fn convertir(bruts: &[RawRecord]) -> (Vec<InfoclimatRecord>, Vec<ImportWarning>, usize) {
    let mut conversion = Conversion::avec_capacite(bruts.len());
    for (index, brut) in bruts.iter().enumerate() {
        // L'API ne numérote pas ses lignes : on désigne alors le rang dans le
        // tableau. Un CSV, lui, a de vraies lignes — bien plus utiles pour aller
        // regarder le fichier — et appelle directement `Conversion::ajouter`.
        conversion.ajouter(brut, index + 1);
    }
    (
        conversion.records,
        conversion.warnings,
        conversion.complements_illisibles,
    )
}

/// Accumulateur de [`convertir`] : le même traitement, un enregistrement à la
/// fois. C'est ce qui permet au lecteur CSV de convertir chaque ligne dès qu'il
/// l'a lue, sans garder la liste complète des enregistrements bruts.
struct Conversion {
    records: Vec<InfoclimatRecord>,
    warnings: Vec<ImportWarning>,
    complements_illisibles: usize,
}

impl Conversion {
    fn avec_capacite(n: usize) -> Self {
        Conversion {
            records: Vec::with_capacity(n),
            warnings: Vec::new(),
            complements_illisibles: 0,
        }
    }

    /// `rang` : numéro de ligne (CSV) ou rang dans le tableau (JSON), 1-based.
    fn ajouter(&mut self, brut: &RawRecord, rang: usize) {
        let records = &mut self.records;
        let warnings = &mut self.warnings;
        let complements_illisibles = &mut self.complements_illisibles;
        let apercu = brut.dh_utc.clone().unwrap_or_else(|| "?".to_string());

        let Some(dh) = brut.dh_utc.as_deref() else {
            warnings.push(ImportWarning::new(
                rang,
                &apercu,
                "enregistrement sans champ dh_utc",
            ));
            return;
        };

        let Some(timestamp_utc) = parse_dh_utc(dh) else {
            warnings.push(ImportWarning::new(
                rang,
                dh,
                "horodatage dh_utc illisible (attendu « AAAA-MM-JJ HH:MM:SS »)",
            ));
            return;
        };

        let Some(temperature_celsius) = brut.temperature.as_ref().and_then(Scalar::as_f64) else {
            warnings.push(ImportWarning::new(
                rang,
                dh,
                "aucune température exploitable sur cet enregistrement",
            ));
            return;
        };

        let complements = match brut.complements.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(brut_json) => match serde_json::from_str::<Complements>(brut_json) {
                Ok(c) => Some(c),
                Err(_) => {
                    // Format inattendu (le `metadata` du fichier en décrit un
                    // autre, à 33 champs séparés par des `@`). On perd RSSI et
                    // batterie, jamais la mesure elle-même.
                    *complements_illisibles += 1;
                    None
                }
            },
        };

        records.push(InfoclimatRecord {
            timestamp_utc,
            temperature_celsius,
            humidity_percent: brut.humidite.as_ref().and_then(Scalar::as_f64),
            rssi_dbm: complements
                .as_ref()
                .and_then(|c| c.rssi.as_ref())
                .and_then(Scalar::as_f64),
            battery: complements
                .as_ref()
                .and_then(|c| c.batt.as_ref())
                .and_then(Scalar::as_f64),
            source: brut.source.clone(),
        });
    }
}

/// `"2026-06-24 20:10:00"` → instant UTC.
///
/// Le `T` de l'ISO 8601 est toléré, ainsi qu'un `Z` final : ni l'un ni l'autre n'a
/// été observé, mais les accepter ne coûte rien et évite un import vide si le
/// format évolue.
pub fn parse_dh_utc(raw: &str) -> Option<DateTime<Utc>> {
    let nettoye = raw.trim().trim_end_matches('Z').replace('T', " ");
    NaiveDateTime::parse_from_str(&nettoye, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(&nettoye, "%Y-%m-%d %H:%M"))
        .ok()
        .map(|naive| naive.and_utc())
}

/// Trie par instant croissant et supprime les instants répétés.
///
/// Nécessaire même en amont de la base : la contrainte `UNIQUE(series_id,
/// timestamp_utc)` rejetterait ces doublons, et deux tranches de 7 jours qui se
/// recouvrent en produisent mécaniquement. Le premier enregistrement rencontré
/// gagne — l'API renvoie ses tranches dans l'ordre chronologique, c'est donc la
/// valeur la plus anciennement publiée.
fn dedoublonner(mut records: Vec<InfoclimatRecord>) -> (Vec<InfoclimatRecord>, usize) {
    records.sort_by_key(|r| r.timestamp_utc);
    let avant = records.len();
    records.dedup_by_key(|r| r.timestamp_utc);
    let doublons = avant - records.len();
    (records, doublons)
}

/// Écart le plus fréquent entre deux mesures successives, en secondes.
///
/// La médiane serait faussée par une station longtemps muette ; le **mode** donne
/// la cadence nominale même si la série est trouée. Sur la Moucherolle : 600 s.
/// Les écarts nuls ou négatifs sont impossibles après [`dedoublonner`].
fn intervalle_dominant(records: &[InfoclimatRecord]) -> Option<i64> {
    if records.len() < 2 {
        return None;
    }
    let mut comptes: HashMap<i64, usize> = HashMap::new();
    for paire in records.windows(2) {
        let ecart = paire[1].timestamp_utc.timestamp() - paire[0].timestamp_utc.timestamp();
        if ecart > 0 {
            *comptes.entry(ecart).or_default() += 1;
        }
    }
    comptes
        .into_iter()
        // À égalité de fréquence, on prend le plus petit écart : c'est celui qui
        // a le plus de chances d'être la cadence, un multiple étant un trou.
        .min_by_key(|&(ecart, n)| (std::cmp::Reverse(n), ecart))
        .map(|(ecart, _)| ecart)
}

// ---------------------------------------------------------------------------
// Découpage des périodes
// ---------------------------------------------------------------------------

/// Une tranche de requête, bornes **incluses** (l'API raisonne en jours entiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DateWindow {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl DateWindow {
    pub fn days(&self) -> i64 {
        (self.end - self.start).num_days() + 1
    }
}

/// Découpe `[start, end]` en tranches d'au plus [`MAX_DAYS_PER_REQUEST`] jours.
///
/// Les bornes sont incluses des deux côtés : une tranche de 7 jours va du 1er au
/// 7, pas du 1er au 8. Les tranches ne se recouvrent pas — le dédoublonnage de
/// [`dedoublonner`] et la contrainte d'unicité en base restent malgré tout la
/// vraie garantie, l'API pouvant renvoyer une mesure à cheval sur minuit.
pub fn split_windows(start: NaiveDate, end: NaiveDate) -> Result<Vec<DateWindow>, InfoclimatError> {
    if end < start {
        return Err(InfoclimatError::PeriodeInvalide(start, end));
    }

    let mut fenetres = Vec::new();
    let mut curseur = start;
    while curseur <= end {
        let fin = (curseur + Duration::days(MAX_DAYS_PER_REQUEST - 1)).min(end);
        fenetres.push(DateWindow {
            start: curseur,
            end: fin,
        });
        curseur = fin + Duration::days(1);
    }
    Ok(fenetres)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Réponse minimale au format réel — le double encodage de `complements`
    /// est reproduit tel quel (les `\"` sont bien dans la chaîne JSON).
    const EXEMPLE: &str = r#"{
        "status": "OK",
        "errors": [],
        "data": [],
        "stations": [
            {
                "id": "STATIC0479",
                "name": "Crête de la Moucherolle - Villard de Lans",
                "latitude": 45.009,
                "longitude": 5.558,
                "elevation": 1965,
                "type": "static",
                "license": {
                    "code": 2,
                    "license": "NON-COMMERCIAL ONLY: CC BY NC",
                    "url": "https://creativecommons.org/licenses/by-nc/2.0/fr/"
                }
            }
        ],
        "metadata": { "temperature": "temperature,°C" },
        "hourly": {
            "STATIC0479": [
                {
                    "id_station": "STATIC0479",
                    "dh_utc": "2026-06-24 20:10:00",
                    "temperature": "16.8",
                    "pression": null,
                    "humidite": "50",
                    "vent_moyen": null,
                    "uv": null,
                    "complements": "{\"rssi\":-94,\"lat\":45.0093,\"lon\":5.5575,\"batt\":39.84}",
                    "source": "liveobjects",
                    "uv_index": null
                },
                {
                    "id_station": "STATIC0479",
                    "dh_utc": "2026-06-24 20:20:00",
                    "temperature": "16.5",
                    "humidite": null,
                    "complements": null,
                    "source": "liveobjects"
                },
                {
                    "id_station": "STATIC0479",
                    "dh_utc": "2026-06-24 20:30:00",
                    "temperature": "16.2",
                    "humidite": "51",
                    "complements": "{\"rssi\":-86,\"lat\":45.0093,\"lon\":5.5575,\"batt\":40.44}",
                    "source": "liveobjects"
                }
            ],
            "_params": ["temperature", "humidite"]
        }
    }"#;

    #[test]
    fn parse_une_reponse_type() {
        let p = parse_infoclimat(EXEMPLE, Some("STATIC0479")).unwrap();

        assert_eq!(p.station_id, "STATIC0479");
        assert_eq!(p.records.len(), 3);
        assert!(p.warnings.is_empty());

        // Les valeurs numériques arrivent en CHAÎNES : c'est le piège n°1.
        assert_eq!(p.records[0].temperature_celsius, 16.8);
        assert_eq!(p.records[0].humidity_percent, Some(50.0));
        // `null` doit rester `None`, pas devenir 0.
        assert_eq!(p.records[1].humidity_percent, None);

        // `complements` : JSON dans du JSON.
        assert_eq!(p.records[0].rssi_dbm, Some(-94.0));
        assert_eq!(p.records[0].battery, Some(39.84));
        assert_eq!(p.records[2].rssi_dbm, Some(-86.0));
        // `complements: null` ne casse rien et n'est pas compté comme illisible.
        assert_eq!(p.records[1].rssi_dbm, None);
        assert_eq!(p.records[1].battery, None);
        assert_eq!(p.unreadable_complements, 0);

        assert_eq!(p.records[0].source.as_deref(), Some("liveobjects"));
        assert_eq!(p.sample_interval_seconds, Some(600));
    }

    #[test]
    fn dh_utc_est_de_l_utc_reel() {
        let p = parse_infoclimat(EXEMPLE, Some("STATIC0479")).unwrap();
        assert_eq!(
            p.records[0].timestamp_utc,
            "2026-06-24T20:10:00Z".parse::<DateTime<Utc>>().unwrap(),
            "aucune conversion de fuseau ne doit être appliquée à dh_utc"
        );
    }

    #[test]
    fn identite_et_licence_de_la_station() {
        let p = parse_infoclimat(EXEMPLE, Some("STATIC0479")).unwrap();
        let s = p.station.expect("bloc stations[] présent");
        assert_eq!(s.id, "STATIC0479");
        assert_eq!(s.latitude, Some(45.009));
        assert_eq!(s.elevation_m, Some(1965.0));
        assert_eq!(s.license.as_deref(), Some("NON-COMMERCIAL ONLY: CC BY NC"));
    }

    /// `_params` est une clé technique de `hourly`, pas une station.
    #[test]
    fn la_cle_params_n_est_pas_prise_pour_une_station() {
        let p = parse_infoclimat(EXEMPLE, None).unwrap();
        assert_eq!(p.available_stations, vec!["STATIC0479".to_string()]);
        assert_eq!(
            p.station_id, "STATIC0479",
            "sans station demandée, l'unique station réelle doit être choisie"
        );
    }

    #[test]
    fn station_absente_donne_une_erreur_explicite() {
        let err = parse_infoclimat(EXEMPLE, Some("STATIC9999")).unwrap_err();
        assert!(matches!(err, InfoclimatError::StationAbsente(_)));
        assert!(err.to_string().contains("STATIC9999"));
    }

    #[test]
    fn les_nombres_non_encodes_en_chaine_passent_aussi() {
        let json = EXEMPLE.replace(r#""temperature": "16.8""#, r#""temperature": 16.8"#);
        let p = parse_infoclimat(&json, Some("STATIC0479")).unwrap();
        assert_eq!(p.records[0].temperature_celsius, 16.8);
    }

    #[test]
    fn une_ligne_sans_temperature_est_ecartee_avec_avertissement() {
        let json = EXEMPLE.replace(r#""temperature": "16.5""#, r#""temperature": null"#);
        let p = parse_infoclimat(&json, Some("STATIC0479")).unwrap();
        assert_eq!(p.records.len(), 2, "la ligne sans température est écartée");
        assert_eq!(p.warnings.len(), 1);
        assert!(p.warnings[0].reason.contains("température"));
    }

    #[test]
    fn un_complements_illisible_ne_perd_que_rssi_et_batterie() {
        let json = EXEMPLE.replace(
            r#""{\"rssi\":-94,\"lat\":45.0093,\"lon\":5.5575,\"batt\":39.84}""#,
            r#""uv_index@uv_index_max@solar_rad_max""#,
        );
        let p = parse_infoclimat(&json, Some("STATIC0479")).unwrap();
        assert_eq!(p.records.len(), 3, "aucune mesure ne doit être perdue");
        assert_eq!(p.records[0].temperature_celsius, 16.8);
        assert_eq!(p.records[0].rssi_dbm, None);
        assert_eq!(p.unreadable_complements, 1);
    }

    #[test]
    fn les_instants_repetes_sont_ecartes() {
        let json = EXEMPLE.replace(r#""dh_utc": "2026-06-24 20:20:00""#, r#""dh_utc": "2026-06-24 20:10:00""#);
        let p = parse_infoclimat(&json, Some("STATIC0479")).unwrap();
        assert_eq!(p.records.len(), 2);
        assert_eq!(p.duplicate_count, 1);
    }

    #[test]
    fn les_mesures_sortent_triees() {
        let desordre = r#"{
            "status":"OK","errors":[],"stations":[],
            "hourly":{"S1":[
                {"dh_utc":"2026-06-24 21:00:00","temperature":"1"},
                {"dh_utc":"2026-06-24 20:00:00","temperature":"2"}
            ]}
        }"#;
        let p = parse_infoclimat(desordre, Some("S1")).unwrap();
        assert_eq!(p.records[0].temperature_celsius, 2.0);
        assert_eq!(p.records[1].temperature_celsius, 1.0);
    }

    #[test]
    fn refus_d_authentification_reconnu_hors_json() {
        let err = parse_infoclimat("Wrong ip address", None).unwrap_err();
        match err {
            InfoclimatError::Auth(message) => {
                assert!(message.contains("adresse IP publique"));
                assert!(message.contains("infoclimat.fr/opendata"));
                assert!(message.contains("Wrong ip address"));
            }
            autre => panic!("attendu une erreur d'authentification, obtenu {autre:?}"),
        }
    }

    #[test]
    fn un_contenu_qui_n_est_pas_du_json_est_rejete_clairement() {
        let err = parse_infoclimat("Date;Temp\n2026-01-01;5", None).unwrap_err();
        assert!(matches!(err, InfoclimatError::Json(_)));
        assert!(err.to_string().contains("pas du JSON Infoclimat"));
    }

    #[test]
    fn un_statut_en_erreur_est_remonte() {
        let json = r#"{"status":"ERROR","errors":["Période trop longue"],"hourly":{}}"#;
        let err = parse_infoclimat(json, None).unwrap_err();
        assert!(err.to_string().contains("Période trop longue"));
    }

    /// Un `errors: []` est le cas NORMAL : il ne doit surtout pas être pris
    /// pour une erreur.
    #[test]
    fn un_tableau_d_erreurs_vide_n_est_pas_une_erreur() {
        assert!(parse_infoclimat(EXEMPLE, Some("STATIC0479")).is_ok());
    }

    #[test]
    fn plusieurs_stations_sans_choix_explicite_refusent_de_deviner() {
        let json = r#"{
            "status":"OK","errors":[],"stations":[],
            "hourly":{
                "S1":[{"dh_utc":"2026-06-24 20:00:00","temperature":"1"}],
                "S2":[{"dh_utc":"2026-06-24 20:00:00","temperature":"2"}]
            }
        }"#;
        let err = parse_infoclimat(json, None).unwrap_err();
        assert!(err.to_string().contains("2 stations"));
        // Mais avec un choix explicite, ça passe.
        assert_eq!(parse_infoclimat(json, Some("S2")).unwrap().records[0].temperature_celsius, 2.0);
    }

    // --- Découpage des périodes ---

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn une_periode_courte_tient_en_une_tranche() {
        let f = split_windows(d("2026-06-01"), d("2026-06-03")).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].start, d("2026-06-01"));
        assert_eq!(f[0].end, d("2026-06-03"));
        assert_eq!(f[0].days(), 3);
    }

    /// Exactement 7 jours : une seule tranche, la limite est inclusive.
    #[test]
    fn sept_jours_pile_ne_sont_pas_coupes() {
        let f = split_windows(d("2026-06-01"), d("2026-06-07")).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].days(), 7);
    }

    #[test]
    fn huit_jours_sont_coupes_en_deux() {
        let f = split_windows(d("2026-06-01"), d("2026-06-08")).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].end, d("2026-06-07"));
        assert_eq!(f[1].start, d("2026-06-08"));
        assert_eq!(f[1].end, d("2026-06-08"));
    }

    /// Les 55 jours du fichier de référence : tranches jointives, sans trou ni
    /// recouvrement, et aucune ne dépasse la limite.
    #[test]
    fn un_long_historique_est_couvert_sans_trou_ni_recouvrement() {
        let debut = d("2026-06-24");
        let fin = d("2026-08-18");
        let f = split_windows(debut, fin).unwrap();

        assert_eq!(f.first().unwrap().start, debut);
        assert_eq!(f.last().unwrap().end, fin);
        for w in &f {
            assert!(w.days() <= MAX_DAYS_PER_REQUEST, "tranche trop longue : {w:?}");
            assert!(w.start <= w.end);
        }
        for paire in f.windows(2) {
            assert_eq!(
                paire[1].start,
                paire[0].end + Duration::days(1),
                "les tranches doivent être jointives"
            );
        }
        let total: i64 = f.iter().map(|w| w.days()).sum();
        assert_eq!(total, (fin - debut).num_days() + 1);
    }

    #[test]
    fn un_seul_jour_donne_une_tranche_d_un_jour() {
        let f = split_windows(d("2026-06-01"), d("2026-06-01")).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].days(), 1);
    }

    #[test]
    fn une_periode_a_l_envers_est_refusee() {
        assert!(matches!(
            split_windows(d("2026-06-08"), d("2026-06-01")),
            Err(InfoclimatError::PeriodeInvalide(_, _))
        ));
    }

    #[test]
    fn la_preuve_de_date_infoclimat_ne_laisse_aucun_doute() {
        let e = infoclimat_date_evidence(42);
        assert!(!e.uncertain, "aucun doute jour/mois sur du dh_utc");
        assert!(!e.conflicting);
        assert_eq!(e.iso_hits, 42);
    }

    #[test]
    fn intervalle_dominant_resiste_aux_trous() {
        let base: DateTime<Utc> = "2026-06-24T20:00:00Z".parse().unwrap();
        let mesure = |minutes: i64| InfoclimatRecord {
            timestamp_utc: base + Duration::minutes(minutes),
            temperature_celsius: 0.0,
            humidity_percent: None,
            rssi_dbm: None,
            battery: None,
            source: None,
        };
        // Cadence 10 min, avec un trou d'une heure au milieu.
        let records = vec![
            mesure(0),
            mesure(10),
            mesure(20),
            mesure(80),
            mesure(90),
            mesure(100),
        ];
        assert_eq!(intervalle_dominant(&records), Some(600));
        assert_eq!(intervalle_dominant(&records[..1]), None);
    }

    // -----------------------------------------------------------------------
    // Export CSV
    // -----------------------------------------------------------------------

    /// Extrait fidèle de `export_infoclimat.csv` : les quatre lignes `#`, les 29
    /// intitulés, la ligne d'unités, puis des mesures. Les champs vides sont
    /// laissés vides, comme dans le fichier réel.
    const CSV: &str = "\
# METADATA:\n\
# STATIC0213: La-Chapelle-en-Vercors - Combe de Darbounouse\n\
# Source: www.infoclimat.fr\n\
# Licence Ouverte/Open License: http://wiki.data.gouv.fr/images/9/9d/Licence_Ouverte.pdf\n\
station_id;dh_utc;temperature;pression;pression_variation_3h;humidite;point_de_rosee;visibilite;vent_moyen;vent_rafales;vent_rafales_10min;vent_direction;temperature_min;temperature_max;pluie_1h;pluie_3h;pluie_6h;pluie_12h;pluie_24h;pluie_cumul_0h;pluie_intensite;pluie_intensite_max_1h;uv;uv_index;ensoleillement;temperature_sol;temps_omm;complements;source\n\
string;YYYY-MM-DD hh:mm:ss;°C;hPa;hPa;%;°C;m;km/h;km/h;km/h;deg;°C;°C;mm;mm;mm;mm;mm;mm;mm/h;mm/h;W/m²;UV Index;hours;°C;url;various;StatIC\n\
STATIC0213;2023-10-14 09:00:00;13;;;89;;;;;;;;;;;;;;;;;;;;;;{\"rssi\":-109,\"lat\":44.9676,\"lon\":5.47711,\"batt\":36.9};liveobjects\n\
STATIC0213;2023-10-14 09:10:00;12.5;;;91;;;;;;;;;;;;;;;;;;;;;;{\"rssi\":-114,\"lat\":44.9676,\"lon\":5.47711,\"batt\":36.9};liveobjects\n\
STATIC0213;2023-10-14 09:20:00;12.1;;;;;;;;;;;;;;;;;;;;;;;;;;liveobjects\n\
";

    fn csv() -> InfoclimatPayload {
        parse_infoclimat_auto(CSV, None).expect("extrait CSV lisible")
    }

    #[test]
    fn le_format_est_deduit_du_contenu_pas_de_l_extension() {
        assert_eq!(csv().format, InfoclimatFormat::Csv);
        assert_eq!(
            parse_infoclimat_auto(EXEMPLE, Some("STATIC0479")).unwrap().format,
            InfoclimatFormat::Json
        );
    }

    #[test]
    fn la_ligne_d_unites_n_est_pas_prise_pour_une_mesure() {
        let p = csv();
        assert_eq!(p.records.len(), 3, "3 mesures, la ligne d'unités exclue");
        assert!(
            p.warnings.is_empty(),
            "la ligne d'unités ne doit pas non plus produire d'avertissement : {:?}",
            p.warnings.first()
        );
        assert_eq!(
            p.records[0].timestamp_utc.to_rfc3339(),
            "2023-10-14T09:00:00+00:00"
        );
    }

    /// Le cœur du travail : CSV et JSON doivent produire **la même chose**.
    #[test]
    fn le_csv_et_le_json_convergent_vers_la_meme_structure() {
        let p = csv();

        // Nombres en chaînes, exactement comme en JSON.
        assert_eq!(p.records[0].temperature_celsius, 13.0);
        assert_eq!(p.records[1].temperature_celsius, 12.5);
        // Une cellule vide reste `None`, jamais 0.
        assert_eq!(p.records[0].humidity_percent, Some(89.0));
        assert_eq!(p.records[2].humidity_percent, None);

        // `complements` : le CSV le porte en JSON simple (pas doublement encodé),
        // et se relit avec le même code.
        assert_eq!(p.records[0].rssi_dbm, Some(-109.0));
        assert_eq!(p.records[0].battery, Some(36.9));
        assert_eq!(p.records[1].rssi_dbm, Some(-114.0));
        // Champ absent : ni RSSI, ni batterie, et surtout pas « illisible ».
        assert_eq!(p.records[2].rssi_dbm, None);
        assert_eq!(p.unreadable_complements, 0);

        assert_eq!(p.records[0].source.as_deref(), Some("liveobjects"));
        assert_eq!(p.sample_interval_seconds, Some(600));
        assert_eq!(p.duplicate_count, 0);
    }

    #[test]
    fn la_station_et_sa_licence_viennent_des_lignes_de_metadonnees() {
        let p = csv();
        assert_eq!(p.station_id, "STATIC0213");
        assert_eq!(p.available_stations, vec!["STATIC0213".to_string()]);

        let station = p.station.expect("le CSV décrit sa station");
        assert_eq!(
            station.name.as_deref(),
            Some("La-Chapelle-en-Vercors - Combe de Darbounouse")
        );
        assert_eq!(station.license.as_deref(), Some("Licence Ouverte/Open License"));
        assert!(station
            .license_url
            .as_deref()
            .unwrap_or_default()
            .contains("Licence_Ouverte"));

        // La position n'est nulle part dans les lignes `#` : elle est reprise du
        // champ `complements`, seule source disponible dans un CSV.
        assert_eq!(station.latitude, Some(44.9676));
        assert_eq!(station.longitude, Some(5.47711));
        assert_eq!(station.elevation_m, None, "aucune altitude n'est inventée");
    }

    #[test]
    fn les_avertissements_pointent_la_ligne_du_fichier() {
        let mut csv = CSV.to_string();
        csv.push_str("STATIC0213;pas une date;12.0;;;;;;;;;;;;;;;;;;;;;;;;;;liveobjects\n");
        let p = parse_infoclimat_csv(&csv, None).unwrap();

        assert_eq!(p.records.len(), 3);
        assert_eq!(p.warnings.len(), 1);
        assert_eq!(
            p.warnings[0].line_number, 10,
            "ligne 10 du fichier (4 lignes « # », l'en-tête, les unités, 3 mesures), \
             et non le rang 4 dans le tableau"
        );
        assert!(p.warnings[0].reason.contains("dh_utc"));
    }

    #[test]
    fn une_station_demandee_absente_du_csv_est_une_erreur_explicite() {
        let erreur = parse_infoclimat_csv(CSV, Some("STATIC9999")).unwrap_err();
        assert!(matches!(erreur, InfoclimatError::StationAbsente(_)));
    }

    #[test]
    fn un_csv_a_deux_stations_refuse_de_deviner() {
        let mut csv = CSV.to_string();
        csv.push_str("STATIC0479;2023-10-14 09:30:00;9.5;;;70;;;;;;;;;;;;;;;;;;;;;;;liveobjects\n");

        // Sans précision, deviner reviendrait à rattacher des mesures au mauvais point.
        let erreur = parse_infoclimat_csv(&csv, None).unwrap_err();
        assert!(erreur.to_string().contains("2 stations"));

        // Avec la station précisée, seules ses lignes sont retenues.
        let p = parse_infoclimat_csv(&csv, Some("STATIC0479")).unwrap();
        assert_eq!(p.records.len(), 1);
        assert_eq!(p.records[0].temperature_celsius, 9.5);
    }

    #[test]
    fn un_export_sans_ligne_d_unites_ne_perd_pas_sa_premiere_mesure() {
        let sans_unites = "\
station_id;dh_utc;temperature;humidite\n\
STATIC0213;2023-10-14 09:00:00;13;89\n\
STATIC0213;2023-10-14 09:10:00;12.5;91\n";
        let p = parse_infoclimat_csv(sans_unites, None).unwrap();
        assert_eq!(p.records.len(), 2, "aucune mesure ne doit être sautée à tort");
    }

    #[test]
    fn un_refus_d_authentification_garde_son_message_d_aide_meme_en_mode_auto() {
        // L'API répond « Wrong ip address » en texte brut avec un code HTTP 200.
        // Sans garde, l'aiguillage le tenterait en CSV et rendrait une erreur
        // d'intitulés au lieu de l'explication sur l'IP.
        let erreur = parse_infoclimat_auto("Wrong ip address", None).unwrap_err();
        assert!(matches!(erreur, InfoclimatError::Auth(_)));
        assert!(erreur.to_string().contains("adresse IP publique"));
    }

    #[test]
    fn un_contenu_tabulaire_inconnu_oriente_vers_l_import_manuel() {
        let erreur = parse_infoclimat_auto("a;b;c\n1;2;3\n", None).unwrap_err();
        assert!(erreur.to_string().contains("import manuel"));
    }
}

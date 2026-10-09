//! Parseur des archives **ROMMA** (`archives_*.csv`).
//!
//! ROMMA — Réseau d'Observation Météo du Massif Alpin — publie, pour chacune de
//! ses stations, un export CSV de l'historique complet. Le fichier de référence
//! de ce module est `archives_Lans - Les Allières_31-05-2017_19-08-2026.csv` :
//! **476 912 mesures sur neuf ans**, au pas de 10 minutes.
//!
//! ## Format, tel qu'il est réellement
//!
//! Séparateur `,`, décimale `.`, et **deux lignes d'en-tête et non une** :
//!
//! ```text
//! Date,heure,timestamp,Temp,Tmin,Tmax,Hum,Pression,Pluie,Vmoy,VmoyDir,Rafale,RafaleDir,RadSolaire, RadSolaireMax
//! ,,,degreC,degreC,degreC,%,hPa,mm,km/h,,km/h,,W/m2, W/m2
//! 31-05-2017,19:00,1496250000,12.2,12.2,12.2,72,1021.0,0.00,10.0,SO,24.1,SO,NULL,NULL
//! ```
//!
//! La seconde ligne porte les **unités**, pas des données. C'est elle qui donne
//! les unités documentées dans [`crate::model::Measurement`] : on ne les devine
//! pas, on les lit. Elle se reconnaît sans ambiguïté au fait que sa colonne
//! `timestamp` est vide — un vrai enregistrement y porte toujours un entier.
//!
//! ## Le temps : `timestamp` et rien d'autre
//!
//! Trois colonnes datent chaque ligne, et **une seule est utilisée** :
//!
//! | Colonne | Contenu | Utilisée ? |
//! |---|---|---|
//! | `timestamp` | epoch Unix, **UTC** | **oui**, seule source de vérité |
//! | `Date` | `JJ-MM-AAAA`, heure **locale** | non |
//! | `heure` | `HH:MM`, heure **locale** | non |
//!
//! Vérification faite sur le fichier réel : l'écart entre `Date`+`heure` et
//! l'instant UTC de `timestamp` vaut +1 h ou +2 h selon la saison — autrement dit
//! l'heure d'hiver et l'heure d'été françaises. Reconstruire l'instant depuis
//! `Date`+`heure` obligerait donc à rejouer les règles de bascule CET/CEST, avec
//! l'heure sautée en mars et l'heure doublée en octobre à traiter. Le
//! `timestamp` évite tout cela : il est déjà en UTC, sans ambiguïté et sans
//! ordre jour/mois à trancher.
//!
//! ROMMA est donc, comme Infoclimat, une source **nativement UTC** :
//! `clock_offset_seconds = 0`, `raw_timestamp_was_utc = true`, et le
//! [`crate::parsing::DateOrderScanner`] est court-circuité.
//!
//! ## Les valeurs manquantes : `NULL`, pas vide
//!
//! Une valeur absente s'écrit **littéralement `NULL`** (et `---` pour une
//! direction de vent indéterminée). Ce n'est pas une erreur de parsing, c'est un
//! capteur qui n'existait pas encore ou qui n'a rien renvoyé.
//! [`crate::parsing::parse_number`] traite déjà `NULL`, `---`, `N/A` comme des
//! absences — aucun code spécifique n'est nécessaire ici.
//!
//! Ce que le fichier réel contient, sur ses 476 912 lignes :
//!
//! | Colonne | Lignes à `NULL` | Lecture |
//! |---|---|---|
//! | `RadSolaire` | **476 912 (100 %)** | la colonne est vide de bout en bout — voir plus bas |
//! | `RadSolaireMax` | 189 044 (40 %) | capteur ajouté en cours de route |
//! | `Pression` | 185 574 (39 %) | idem |
//! | `Pluie` | 103 848 (22 %) | |
//! | `Vmoy` / `Rafale` | ~71 000 (15 %) | |
//! | `Temp` | **10 621 (2,2 %)** | ces lignes sont **écartées** : sans température, pas de mesure |
//!
//! ### Pourquoi le rayonnement vient de `RadSolaireMax`
//!
//! `RadSolaire` est à `NULL` sur **la totalité** du fichier de référence : la
//! prendre au pied de la lettre reviendrait à ne jamais rien stocker. Seule
//! `RadSolaireMax` porte des valeurs (287 868 lignes). C'est donc elle qui
//! alimente `solar_radiation`, et [`RommaPayload::solar_from_max`] le signale
//! pour que le rapport d'import le dise plutôt que de le taire.
//!
//! Si un jour un export renseigne les deux colonnes, `RadSolaire` (la moyenne sur
//! l'intervalle) reprend la priorité : c'est la grandeur la plus directement
//! comparable d'un pas de temps à l'autre.
//!
//! ## Direction du vent : une rose, pas des degrés
//!
//! `VmoyDir` et `RafaleDir` ne contiennent pas de degrés mais des abréviations
//! **françaises** à 16 points (`N`, `NNE`, `NE`, … `NNO` — avec `O` pour Ouest,
//! là où l'anglais écrirait `W`). La conversion en degrés est faite par
//! [`direction_en_degres`]. La valeur stockée est donc un **secteur de 22,5°**,
//! pas une mesure fine : ne pas en tirer de statistique de direction moyenne sans
//! garder cela en tête.
//!
//! ## Volume
//!
//! 38 Mo, 477 000 lignes. Le parseur travaille **ligne à ligne** sur un `&str`
//! déjà en mémoire (le frontend lit le fichier), sans jamais construire de
//! structure intermédiaire par ligne au-delà des champs découpés, et l'écriture
//! passe par le chemin de lots de [`crate::db`].
//!
//! ## Ce qui n'est pas lu : `climato_*.csv`
//!
//! ROMMA publie aussi un fichier `climato_*.csv` : des **agrégats journaliers**
//! (Tn, Tx, amplitude, durée de gel…) dérivés des mêmes mesures. Il n'est
//! délibérément pas supporté — l'application recalcule tous ces agrégats
//! elle-même depuis les mesures (voir [`crate::stats`]), et les importer
//! reviendrait à stocker deux fois la même information, avec le risque qu'elles
//! divergent.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::elitech_import::ImportWarning;
use crate::fingerprint::FingerprintSample;
use crate::stats::TemperatureSample;
use crate::parsing::{parse_number, DateOrderEvidence};

/// Mention d'attribution affichée dès qu'une donnée ROMMA est présentée.
///
/// ROMMA n'accompagne pas ses exports d'une mention de licence (contrairement à
/// Infoclimat, qui en écrit une dans le fichier). Cette phrase est donc une
/// **attribution de courtoisie**, pas la reprise d'une clause : elle nomme la
/// source pour que personne, en relisant un graphique dans deux ans, ne prenne
/// ces mesures pour celles d'un logger du projet. Si ROMMA publie un jour des
/// conditions de réutilisation, c'est ici qu'il faudra les refléter.
pub const ATTRIBUTION: &str = "Données ROMMA (romma.fr) — réseau d'observation météo du massif alpin";

/// Séparateur du format. Fixé, pas détecté : un CSV ROMMA est toujours en `,`,
/// et l'auto-détection se tromperait sur une ligne où toutes les valeurs sont à
/// `NULL` (que des virgules et des lettres).
pub const DELIMITER: char = ',';

/// Nombre d'avertissements réellement conservés.
///
/// Le fichier de référence contient 10 621 lignes sans température. En garder une
/// trace complète coûterait plusieurs mégaoctets pour une liste que personne ne
/// lira jusqu'au bout ; [`RommaPayload::skipped_count`] en donne le compte exact,
/// et cette borne garde les premiers cas, qui suffisent au diagnostic.
pub const MAX_WARNINGS_KEPT: usize = 200;

// ---------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RommaError {
    #[error("Le fichier est vide.")]
    EmptyFile,

    #[error(
        "Aucune ligne d'en-tête ROMMA reconnue. Un export `archives_*.csv` commence par \
         « Date,heure,timestamp,Temp,… ». S'il s'agit d'un `climato_*.csv` (agrégats \
         journaliers), il n'est pas supporté : importez l'archive détaillée."
    )]
    NoHeader,

    #[error(
        "Colonne « {0} » absente de l'en-tête. Colonnes trouvées : {1}. \
         Sans elle, aucune mesure ne peut être datée ni exploitée."
    )]
    ColonneManquante(&'static str, String),

    #[error(
        "Aucune mesure exploitable : {0} ligne(s) inspectée(s), toutes écartées. \
         Vérifiez que le fichier n'est pas tronqué."
    )]
    NoDataRows(usize),
}

// ---------------------------------------------------------------------------
// Structures exposées
// ---------------------------------------------------------------------------

/// Une mesure ROMMA, avant tout passage en base.
///
/// Tous les champs météo sont optionnels ; seule la température ne l'est pas —
/// une ligne sans température n'est pas une mesure et n'arrive jamais ici.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RommaRecord {
    /// **UTC réel**, lu dans la colonne `timestamp` (epoch Unix).
    pub timestamp_utc: DateTime<Utc>,
    pub temperature_celsius: f64,
    pub humidity_percent: Option<f64>,
    pub pressure_hpa: Option<f64>,
    pub wind_avg_kmh: Option<f64>,
    pub wind_gust_kmh: Option<f64>,
    /// Direction du vent moyen, convertie depuis la rose à 16 points.
    pub wind_direction_degrees: Option<f64>,
    pub rain_mm: Option<f64>,
    /// Rayonnement en W/m² — voir [`RommaPayload::solar_from_max`].
    pub solar_radiation: Option<f64>,
    /// Numéro de ligne (1-based) dans le fichier, pour le diagnostic.
    pub source_line: usize,
}

// L'empreinte et les statistiques se calculent **sur l'enregistrement parsé** :
// la source est nativement UTC, donc l'instant brut est l'instant UTC, exactement
// comme dans le `Measurement::new_utc` qui en sera tiré à l'écriture.
impl FingerprintSample for RommaRecord {
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

impl TemperatureSample for RommaRecord {
    fn instant(&self) -> DateTime<Utc> {
        self.timestamp_utc
    }
    fn temperature(&self) -> f64 {
        self.temperature_celsius
    }
}

/// Ce qu'une archive ROMMA contient, une fois analysée.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RommaPayload {
    pub records: Vec<RommaRecord>,
    /// Jusqu'à [`MAX_WARNINGS_KEPT`] lignes écartées, avec leur motif.
    pub warnings: Vec<ImportWarning>,
    /// Nombre **total** de lignes écartées (peut dépasser `warnings.len()`).
    pub skipped_count: usize,
    /// Écart le plus fréquent entre deux mesures successives, en secondes.
    pub sample_interval_seconds: Option<i64>,
    /// Instants répétés, écartés avant insertion.
    pub duplicate_count: usize,
    /// Lignes de données parcourues (en-tête et unités exclues).
    pub total_data_lines: usize,
    /// Intitulés de colonnes lus dans l'en-tête.
    pub columns: Vec<String>,
    /// Unités lues dans la seconde ligne, par nom de colonne.
    pub units: Vec<(String, String)>,
    /// `true` : `RadSolaire` était vide de bout en bout et le rayonnement vient
    /// de `RadSolaireMax`. À dire à l'utilisateur — ce n'est pas la même grandeur.
    pub solar_from_max: bool,
    /// Combien de mesures portent chaque champ facultatif, dans l'ordre :
    /// humidité, pression, vent moyen, rafale, direction, pluie, rayonnement.
    pub field_counts: RommaFieldCounts,
}

/// Couverture réelle de chaque champ facultatif dans le lot lu.
///
/// Affiché avant l'import : sur neuf ans d'archive, plusieurs capteurs ont été
/// ajoutés en cours de route, et l'utilisateur doit pouvoir constater qu'une
/// colonne n'est renseignée que sur une partie de la période.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommaFieldCounts {
    pub humidity: usize,
    pub pressure: usize,
    pub wind_avg: usize,
    pub wind_gust: usize,
    pub wind_direction: usize,
    pub rain: usize,
    pub solar: usize,
}

impl RommaPayload {
    pub fn first_utc(&self) -> Option<DateTime<Utc>> {
        self.records.first().map(|r| r.timestamp_utc)
    }

    pub fn last_utc(&self) -> Option<DateTime<Utc>> {
        self.records.last().map(|r| r.timestamp_utc)
    }
}

/// Ce que le modèle doit inscrire dans `LoggerSeries.date_order` pour ROMMA.
///
/// Un epoch Unix n'a pas d'ordre jour/mois : la preuve est « ISO », ni incertaine
/// ni contradictoire, et l'UI n'affichera jamais le bandeau « format de date à
/// confirmer » sur ces séries.
pub fn romma_date_evidence(record_count: usize) -> DateOrderEvidence {
    DateOrderEvidence::iso(record_count)
}

// ---------------------------------------------------------------------------
// Rose des vents
// ---------------------------------------------------------------------------

/// Table de conversion de la rose à 16 points **française** vers les degrés.
///
/// `O` = Ouest (et non `W`), `S` = Sud, etc. Les valeurs sont les milieux de
/// secteur : `NNE` = 22,5°, `NE` = 45°, et ainsi de suite.
const ROSE_DES_VENTS: [(&str, f64); 16] = [
    ("N", 0.0),
    ("NNE", 22.5),
    ("NE", 45.0),
    ("ENE", 67.5),
    ("E", 90.0),
    ("ESE", 112.5),
    ("SE", 135.0),
    ("SSE", 157.5),
    ("S", 180.0),
    ("SSO", 202.5),
    ("SO", 225.0),
    ("OSO", 247.5),
    ("O", 270.0),
    ("ONO", 292.5),
    ("NO", 315.0),
    ("NNO", 337.5),
];

/// `"SSO"` → `Some(202.5)`, `"NULL"` / `"---"` → `None`.
///
/// Un nombre est accepté tel quel : rien ne garantit qu'un futur export ROMMA
/// (ou un autre réseau) ne publiera pas directement des degrés, et le refuser
/// perdrait de l'information pour rien. La valeur est alors ramenée dans
/// `[0, 360[` — un capteur qui renvoie 360° désigne le nord, pas un secteur
/// au-delà du tour complet.
pub fn direction_en_degres(raw: &str) -> Option<f64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    // Comparaison sans allocation pour le cas courant (ASCII) : appelée sur
    // chaque ligne d'archive, à chaque relecture.
    let trouve = if s.is_ascii() {
        ROSE_DES_VENTS.iter().find(|(nom, _)| nom.eq_ignore_ascii_case(s))
    } else {
        let majuscules = s.to_uppercase();
        ROSE_DES_VENTS.iter().find(|(nom, _)| *nom == majuscules)
    };
    if let Some((_, degres)) = trouve {
        return Some(*degres);
    }
    // `parse_number` écarte déjà « NULL », « --- », « N/A »… La variante anglaise
    // (`W`, `NW`…) n'est volontairement pas gérée : elle n'apparaît pas dans le
    // format et l'accepter au petit bonheur masquerait un changement de format.
    parse_number(s)
        .filter(|d| d.is_finite())
        .map(|d| d.rem_euclid(360.0))
}

// ---------------------------------------------------------------------------
// Parseur
// ---------------------------------------------------------------------------

/// Colonnes attendues. Le repérage se fait **par nom**, jamais par position :
/// l'export a déjà une colonne dont l'intitulé porte une espace de tête
/// (` RadSolaireMax`), signe que l'ordre n'est pas une garantie.
const COL_TIMESTAMP: &str = "timestamp";
const COL_TEMP: &str = "temp";

/// Ce que l'en-tête d'une archive apprend une fois pour toutes : où sont les
/// colonnes, quelles sont les unités, où commencent les données.
///
/// Lue une seule fois, puis réutilisée par chaque passe sur les lignes : c'est ce
/// qui permet de relire le fichier en flux ([`RommaApercu::records`]) avec
/// exactement les mêmes règles que [`parse_romma_archives`].
#[derive(Debug, Clone)]
struct Structure {
    entete: Vec<String>,
    units: Vec<(String, String)>,
    /// Index (0-based) de la première ligne candidate aux données.
    premiere_ligne: usize,
    col_timestamp: usize,
    col_temp: usize,
    col_hum: Option<usize>,
    col_pression: Option<usize>,
    col_pluie: Option<usize>,
    col_vmoy: Option<usize>,
    col_vmoy_dir: Option<usize>,
    col_rafale: Option<usize>,
    col_rad: Option<usize>,
    col_rad_max: Option<usize>,
}

/// Une ligne de données lue.
enum Ligne {
    /// `rad_moyen` : la colonne `RadSolaire` était renseignée sur cette ligne.
    Mesure { record: RommaRecord, rad_moyen: bool },
    Ecartee(String),
}

/// Le texte ressemble-t-il à une archive ROMMA ? Même règle que l'en-tête du
/// parseur : une des 50 premières lignes porte `timestamp` et `Temp`.
pub fn ressemble_a_une_archive(content: &str) -> bool {
    content.lines().take(50).any(|l| {
        let noms: Vec<String> = decouper(l).iter().map(|c| normaliser(c)).collect();
        noms.iter().any(|n| n == COL_TIMESTAMP) && noms.iter().any(|n| n == COL_TEMP)
    })
}

fn lire_structure(content: &str) -> Result<Structure, RommaError> {
    if content.trim().is_empty() {
        return Err(RommaError::EmptyFile);
    }

    let mut lignes = content.lines().enumerate();

    // --- 1. En-tête : la première ligne qui porte `timestamp` et `Temp`.
    let (index_entete, entete) = lignes
        .by_ref()
        .take(50)
        .map(|(i, l)| (i, decouper(l)))
        .find(|(_, champs)| {
            let noms: Vec<String> = champs.iter().map(|c| normaliser(c)).collect();
            noms.iter().any(|n| n == COL_TIMESTAMP) && noms.iter().any(|n| n == COL_TEMP)
        })
        .ok_or(RommaError::NoHeader)?;

    let noms: Vec<String> = entete.iter().map(|c| normaliser(c)).collect();
    let index = |nom: &str| noms.iter().position(|n| n == nom);

    let col_timestamp = index(COL_TIMESTAMP)
        .ok_or_else(|| RommaError::ColonneManquante("timestamp", entete.join(", ")))?;
    let col_temp =
        index(COL_TEMP).ok_or_else(|| RommaError::ColonneManquante("Temp", entete.join(", ")))?;

    // --- 2. Ligne d'unités : celle qui suit immédiatement l'en-tête **si** sa
    // colonne `timestamp` n'est pas un entier. Ne pas la sauter aveuglément :
    // un export qui ne la porterait pas perdrait sinon sa première mesure.
    let mut units: Vec<(String, String)> = Vec::new();
    let saut_unites = match content.lines().nth(index_entete + 1) {
        Some(premiere) => {
            let champs = decouper(premiere);
            let est_unites = champs
                .get(col_timestamp)
                .map(|v| v.trim().parse::<i64>().is_err())
                .unwrap_or(true);
            if est_unites {
                units = entete
                    .iter()
                    .zip(champs.iter())
                    .filter(|(_, u)| !u.trim().is_empty())
                    .map(|(c, u)| (c.trim().to_string(), u.trim().to_string()))
                    .collect();
                1
            } else {
                0
            }
        }
        None => 0,
    };

    Ok(Structure {
        premiere_ligne: index_entete + 1 + saut_unites,
        entete: entete.iter().map(|c| c.to_string()).collect(),
        units,
        col_timestamp,
        col_temp,
        col_hum: index("hum"),
        col_pression: index("pression"),
        col_pluie: index("pluie"),
        col_vmoy: index("vmoy"),
        col_vmoy_dir: index("vmoydir"),
        col_rafale: index("rafale"),
        col_rad: index("radsolaire"),
        col_rad_max: index("radsolairemax"),
    })
}

impl Structure {
    /// Lignes de données non vides : `(numéro 1-based, texte)`.
    fn lignes<'a>(&self, content: &'a str) -> impl Iterator<Item = (usize, &'a str)> + 'a {
        content
            .lines()
            .enumerate()
            .skip(self.premiere_ligne)
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(offset, l)| (offset + 1, l)) // 1-based, comme dans un éditeur de texte
    }

    fn lire_ligne(&self, numero: usize, ligne: &str) -> Ligne {
        let necessaire = self.col_timestamp.max(self.col_temp);
        let champs = decouper(ligne);

        if champs.len() <= necessaire {
            return Ligne::Ecartee(format!(
                "{} colonne(s) trouvée(s), au moins {} attendue(s)",
                champs.len(),
                necessaire + 1
            ));
        }

        let Some(timestamp_utc) = parse_epoch(&champs[self.col_timestamp]) else {
            return Ligne::Ecartee(format!(
                "horodatage epoch illisible (« {} »)",
                champs[self.col_timestamp].trim()
            ));
        };

        // Une ligne sans température n'est pas une mesure : `temperature_celsius`
        // est `NOT NULL` en base, et une station muette sur son capteur principal
        // n'apporte rien à la chasse aux TAF. C'est le cas des 2,2 % de lignes
        // `Temp = NULL` du fichier réel.
        let Some(temperature_celsius) = valeur(&champs, Some(self.col_temp)) else {
            return Ligne::Ecartee(
                "aucune température (« NULL ») : la ligne ne peut pas devenir une mesure"
                    .to_string(),
            );
        };

        let rad_moyen = valeur(&champs, self.col_rad);

        Ligne::Mesure {
            rad_moyen: rad_moyen.is_some(),
            record: RommaRecord {
                timestamp_utc,
                temperature_celsius,
                humidity_percent: valeur(&champs, self.col_hum),
                pressure_hpa: valeur(&champs, self.col_pression),
                wind_avg_kmh: valeur(&champs, self.col_vmoy),
                wind_gust_kmh: valeur(&champs, self.col_rafale),
                wind_direction_degrees: self
                    .col_vmoy_dir
                    .and_then(|i| champs.get(i))
                    .and_then(|v| direction_en_degres(v)),
                rain_mm: valeur(&champs, self.col_pluie),
                // `RadSolaire` d'abord, `RadSolaireMax` en repli — voir l'en-tête du
                // module : la première est vide de bout en bout sur le fichier réel.
                solar_radiation: rad_moyen.or_else(|| valeur(&champs, self.col_rad_max)),
                source_line: numero,
            },
        }
    }
}

/// Analyse une archive ROMMA **déjà en mémoire**.
///
/// Rend toutes les mesures, triées et dédoublonnées. Pour une archive déjà
/// triée — le cas du fichier réel — [`lire_romma`] obtient le même résultat
/// sans jamais les garder toutes en mémoire.
pub fn parse_romma_archives(content: &str) -> Result<RommaPayload, RommaError> {
    let structure = lire_structure(content)?;

    let mut records: Vec<RommaRecord> = Vec::new();
    let mut warnings: Vec<ImportWarning> = Vec::new();
    let mut skipped_count = 0usize;
    let mut total_data_lines = 0usize;
    let mut rad_moyen_vu = false;

    for (numero, ligne) in structure.lignes(content) {
        total_data_lines += 1;
        match structure.lire_ligne(numero, ligne) {
            Ligne::Mesure { record, rad_moyen } => {
                rad_moyen_vu |= rad_moyen;
                records.push(record);
            }
            Ligne::Ecartee(motif) => {
                ecarter(&mut warnings, &mut skipped_count, numero, ligne, motif)
            }
        }
    }

    if records.is_empty() {
        return Err(RommaError::NoDataRows(total_data_lines));
    }

    let solar_from_max = !rad_moyen_vu && records.iter().any(|r| r.solar_radiation.is_some());
    let (records, duplicate_count) = dedoublonner(records);
    let sample_interval_seconds = intervalle_dominant(&records);
    let field_counts = compter(&records);

    let Structure { entete, units, .. } = structure;
    Ok(RommaPayload {
        sample_interval_seconds,
        duplicate_count,
        total_data_lines,
        columns: entete,
        units,
        solar_from_max,
        field_counts,
        skipped_count,
        warnings,
        records,
    })
}

// ---------------------------------------------------------------------------
// Lecture en flux
// ---------------------------------------------------------------------------
//
// ## Pourquoi
//
// `parse_romma_archives` garde toutes les mesures pour les trier et en retirer
// les doublons : 477 000 `RommaRecord` (~75 Mo), plus le tampon du tri stable
// (~35 Mo). Or l'archive réelle est **déjà triée et sans doublon** — vérifié sur
// ses 476 912 lignes. Dans ce cas, trier ne fait rien et dédoublonner non plus :
// garder les mesures ne sert à rien, on peut les relire dans le texte au moment
// de s'en servir.
//
// ## Comment
//
// Une première passe ([`lire_romma`]) lit tout sans rien garder et vérifie que
// les instants sont **strictement croissants**. Si c'est le cas, elle rend un
// [`RommaApercu`] : tout ce que dit `RommaPayload`, sauf la liste. Les passes
// suivantes (empreinte, insertion) relisent le texte avec [`RommaApercu::records`].
//
// Sinon — fichier réordonné, instant répété — elle rend le `RommaPayload`
// complet de `parse_romma_archives`, et le résultat reste celui d'avant, à
// l'identique. Le flux n'est qu'un raccourci pour le cas où il ne change rien.

/// Nombre de mesures gardées pour l'aperçu affiché avant import.
pub const ROMMA_SAMPLE_LEN: usize = 5;

/// Résultat d'une lecture de [`lire_romma`].
#[derive(Debug, Clone)]
pub enum RommaLecture {
    /// Archive triée et sans doublon : rien n'est gardé, les mesures se relisent
    /// dans le texte.
    EnFlux(RommaApercu),
    /// Archive à trier ou à dédoublonner : toutes les mesures, comme
    /// [`parse_romma_archives`].
    Complete(RommaPayload),
}

/// Tout ce que [`RommaPayload`] dit d'une archive, **sans la liste des mesures**.
///
/// N'existe que pour une archive déjà triée et sans doublon : `duplicate_count`
/// y vaut donc toujours 0.
#[derive(Debug, Clone)]
pub struct RommaApercu {
    structure: Structure,
    pub record_count: usize,
    pub first_utc: DateTime<Utc>,
    pub last_utc: DateTime<Utc>,
    pub warnings: Vec<ImportWarning>,
    pub skipped_count: usize,
    pub sample_interval_seconds: Option<i64>,
    pub total_data_lines: usize,
    pub solar_from_max: bool,
    pub field_counts: RommaFieldCounts,
    /// Les [`ROMMA_SAMPLE_LEN`] premières mesures.
    pub sample: Vec<RommaRecord>,
}

impl RommaApercu {
    pub fn columns(&self) -> &[String] {
        &self.structure.entete
    }

    pub fn units(&self) -> &[(String, String)] {
        &self.structure.units
    }

    /// Relit les mesures dans `content` — **le même texte** que celui passé à
    /// [`lire_romma`] —, dans l'ordre, une à une.
    pub fn records<'a>(&'a self, content: &'a str) -> RommaRecords<'a> {
        RommaRecords {
            structure: &self.structure,
            lignes: Box::new(self.structure.lignes(content)),
            restant: self.record_count,
        }
    }
}

/// Itérateur de [`RommaApercu::records`]. Sa longueur est connue d'avance
/// (`ExactSizeIterator`) : c'est elle qui rythme les repères de progression de
/// l'insertion.
pub struct RommaRecords<'a> {
    structure: &'a Structure,
    lignes: Box<dyn Iterator<Item = (usize, &'a str)> + 'a>,
    restant: usize,
}

impl Iterator for RommaRecords<'_> {
    type Item = RommaRecord;

    fn next(&mut self) -> Option<RommaRecord> {
        for (numero, ligne) in self.lignes.by_ref() {
            if let Ligne::Mesure { record, .. } = self.structure.lire_ligne(numero, ligne) {
                // Même texte, mêmes règles : la relecture retrouve exactement les
                // mesures comptées par la première passe.
                self.restant = self.restant.saturating_sub(1);
                return Some(record);
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.restant, Some(self.restant))
    }
}

impl ExactSizeIterator for RommaRecords<'_> {}

/// Lit une archive en **une passe**, sans garder ses mesures si elle est déjà
/// triée et sans doublon. Voir la section « Lecture en flux ».
pub fn lire_romma(content: &str) -> Result<RommaLecture, RommaError> {
    let structure = lire_structure(content)?;

    let mut warnings: Vec<ImportWarning> = Vec::new();
    let mut skipped_count = 0usize;
    let mut total_data_lines = 0usize;
    let mut rad_moyen_vu = false;
    let mut solaire_vu = false;
    let mut record_count = 0usize;
    let mut bornes: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    let mut ecarts: HashMap<i64, usize> = HashMap::new();
    let mut field_counts = RommaFieldCounts::default();
    let mut sample: Vec<RommaRecord> = Vec::with_capacity(ROMMA_SAMPLE_LEN);

    for (numero, ligne) in structure.lignes(content) {
        total_data_lines += 1;
        let record = match structure.lire_ligne(numero, ligne) {
            Ligne::Mesure { record, rad_moyen } => {
                rad_moyen_vu |= rad_moyen;
                record
            }
            Ligne::Ecartee(motif) => {
                ecarter(&mut warnings, &mut skipped_count, numero, ligne, motif);
                continue;
            }
        };

        if let Some((_, precedent)) = bornes {
            if record.timestamp_utc <= precedent {
                // Désordre ou instant répété : seul le chemin complet sait trier
                // et dédoublonner. On lui laisse l'archive, sans rien perdre.
                return parse_romma_archives(content).map(RommaLecture::Complete);
            }
            *ecarts
                .entry(record.timestamp_utc.timestamp() - precedent.timestamp())
                .or_default() += 1;
        }
        bornes = Some(match bornes {
            Some((premier, _)) => (premier, record.timestamp_utc),
            None => (record.timestamp_utc, record.timestamp_utc),
        });

        record_count += 1;
        solaire_vu |= record.solar_radiation.is_some();
        compter_un(&mut field_counts, &record);
        if sample.len() < ROMMA_SAMPLE_LEN {
            sample.push(record);
        }
    }

    let Some((first_utc, last_utc)) = bornes else {
        return Err(RommaError::NoDataRows(total_data_lines));
    };

    Ok(RommaLecture::EnFlux(RommaApercu {
        structure,
        record_count,
        first_utc,
        last_utc,
        warnings,
        skipped_count,
        sample_interval_seconds: mode_des_ecarts(ecarts),
        total_data_lines,
        solar_from_max: !rad_moyen_vu && solaire_vu,
        field_counts,
        sample,
    }))
}

// ---------------------------------------------------------------------------
// Briques
// ---------------------------------------------------------------------------

fn decouper(ligne: &str) -> Vec<&str> {
    // Pas de guillemets à gérer : l'export ne cite aucun champ, et tous les
    // contenus sont des nombres ou des abréviations sans virgule.
    //
    // Des `&str` empruntés à la ligne et non des `String` : une archive se
    // relit plusieurs fois en flux, et quinze allocations par ligne sur 477 000
    // lignes dominaient le temps de lecture.
    ligne
        .trim_end_matches(['\r', '\n'])
        .split(DELIMITER)
        .map(str::trim)
        .collect()
}

/// Nom de colonne comparable : minuscules, sans espaces ni accents.
///
/// Indispensable ici : l'export porte littéralement `« RadSolaireMax »` avec une
/// espace de tête, et comparer les intitulés bruts la manquerait.
fn normaliser(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            'é' | 'è' | 'ê' => 'e',
            'à' | 'â' => 'a',
            autre => autre,
        })
        .collect()
}

/// Valeur numérique d'une colonne facultative. `NULL`, `---` et le vide donnent
/// `None` — c'est [`parse_number`] qui les reconnaît, pas ce module.
fn valeur(champs: &[&str], colonne: Option<usize>) -> Option<f64> {
    colonne
        .and_then(|i| champs.get(i))
        .and_then(|v| parse_number(v))
}

/// Variante pour une colonne obligatoire déjà localisée.
fn parse_epoch(raw: &str) -> Option<DateTime<Utc>> {
    let secondes: i64 = raw.trim().parse().ok()?;
    DateTime::from_timestamp(secondes, 0)
}

fn ecarter(
    warnings: &mut Vec<ImportWarning>,
    compteur: &mut usize,
    numero: usize,
    ligne: &str,
    motif: impl Into<String>,
) {
    *compteur += 1;
    if warnings.len() < MAX_WARNINGS_KEPT {
        warnings.push(ImportWarning::new(numero, ligne, motif));
    }
}

/// Trie par instant croissant et supprime les instants répétés.
///
/// Le fichier réel est déjà trié et sans doublon (vérifié sur ses 476 912
/// lignes), mais la contrainte `UNIQUE(series_id, timestamp_utc)` rejetterait un
/// export qui ne le serait pas : autant l'écarter proprement ici et le dire.
fn dedoublonner(mut records: Vec<RommaRecord>) -> (Vec<RommaRecord>, usize) {
    records.sort_by_key(|r| r.timestamp_utc);
    let avant = records.len();
    records.dedup_by_key(|r| r.timestamp_utc);
    let doublons = avant - records.len();
    (records, doublons)
}

/// Écart le plus fréquent entre deux mesures successives, en secondes.
///
/// Le **mode** et non la médiane : neuf ans d'archive comportent forcément des
/// coupures, et la médiane d'une série trouée n'est pas sa cadence. Sur le
/// fichier de référence : 600 s.
fn intervalle_dominant(records: &[RommaRecord]) -> Option<i64> {
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
    mode_des_ecarts(comptes)
}

/// L'écart le plus fréquent ; à égalité, le plus court.
fn mode_des_ecarts(comptes: HashMap<i64, usize>) -> Option<i64> {
    comptes
        .into_iter()
        .min_by_key(|&(ecart, n)| (std::cmp::Reverse(n), ecart))
        .map(|(ecart, _)| ecart)
}

fn compter(records: &[RommaRecord]) -> RommaFieldCounts {
    let mut c = RommaFieldCounts::default();
    for r in records {
        compter_un(&mut c, r);
    }
    c
}

fn compter_un(c: &mut RommaFieldCounts, r: &RommaRecord) {
    c.humidity += r.humidity_percent.is_some() as usize;
    c.pressure += r.pressure_hpa.is_some() as usize;
    c.wind_avg += r.wind_avg_kmh.is_some() as usize;
    c.wind_gust += r.wind_gust_kmh.is_some() as usize;
    c.wind_direction += r.wind_direction_degrees.is_some() as usize;
    c.rain += r.rain_mm.is_some() as usize;
    c.solar += r.solar_radiation.is_some() as usize;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Extrait fidèle du fichier réel : en-tête, ligne d'unités, puis des lignes
    /// couvrant les trois cas notables (tout renseigné, `NULL` partiels,
    /// `Temp = NULL`).
    const EXTRAIT: &str = "\
Date,heure,timestamp,Temp,Tmin,Tmax,Hum,Pression,Pluie,Vmoy,VmoyDir,Rafale,RafaleDir,RadSolaire, RadSolaireMax
,,,degreC,degreC,degreC,%,hPa,mm,km/h,,km/h,,W/m2, W/m2
31-05-2017,19:00,1496250000,12.2,12.2,12.2,72,1021.0,0.00,10.0,SO,24.1,SO,NULL,NULL
31-05-2017,19:10,1496250600,12.0,12.0,12.0,73,1021.1,0.20,8.0,SSO,20.0,S,NULL,412
08-06-2017,00:30,1496874600,NULL,NULL,NULL,NULL,NULL,0.00,0.0,---,0.0,---,NULL,NULL
31-05-2017,19:20,1496251200,11.8,11.8,11.8,74,NULL,0.00,3.2,N,9.7,NNO,NULL,0
";

    fn extrait() -> RommaPayload {
        parse_romma_archives(EXTRAIT).expect("extrait ROMMA lisible")
    }

    #[test]
    fn la_ligne_d_unites_n_est_pas_prise_pour_une_mesure() {
        let p = extrait();
        assert_eq!(p.records.len(), 3, "4 lignes de données, 1 sans température");
        assert_eq!(
            p.records[0].temperature_celsius, 12.2,
            "la première mesure doit être la ligne de 19:00, pas la ligne d'unités"
        );

        // Les unités sont lues et conservées : c'est d'elles que viennent les
        // unités documentées sur `Measurement`.
        let unites: Vec<&str> = p.units.iter().map(|(c, _)| c.as_str()).collect();
        assert!(unites.contains(&"Temp"));
        assert_eq!(
            p.units
                .iter()
                .find(|(c, _)| c == "Pression")
                .map(|(_, u)| u.as_str()),
            Some("hPa")
        );
        assert_eq!(
            p.units
                .iter()
                .find(|(c, _)| c == "Vmoy")
                .map(|(_, u)| u.as_str()),
            Some("km/h")
        );
    }

    #[test]
    fn le_timestamp_epoch_est_la_seule_source_de_l_instant() {
        let p = extrait();
        // 1496250000 = 2017-05-31 17:00:00 UTC, alors que le fichier affiche
        // « 31-05-2017 19:00 » en heure locale (UTC+2 en mai). Reconstruire
        // l'instant depuis Date+heure donnerait deux heures de trop.
        assert_eq!(
            p.records[0].timestamp_utc.to_rfc3339(),
            "2017-05-31T17:00:00+00:00"
        );
    }

    #[test]
    fn les_null_litteraux_deviennent_des_absences_pas_des_zeros() {
        let p = extrait();
        let troisieme = &p.records[2]; // 19:20
        assert_eq!(troisieme.pressure_hpa, None, "« NULL » n'est pas 0 hPa");
        assert_eq!(troisieme.humidity_percent, Some(74.0));
        assert_eq!(troisieme.rain_mm, Some(0.0), "0.00 mm reste une vraie valeur");
        assert_eq!(troisieme.solar_radiation, Some(0.0), "0 W/m² la nuit, pas NULL");
    }

    #[test]
    fn une_ligne_sans_temperature_est_ecartee_avec_son_motif() {
        let p = extrait();
        assert_eq!(p.skipped_count, 1);
        assert_eq!(p.warnings.len(), 1);
        assert!(p.warnings[0].reason.contains("température"));
        assert_eq!(p.warnings[0].line_number, 5);
    }

    #[test]
    fn le_rayonnement_vient_de_radsolairemax_quand_radsolaire_est_vide() {
        let p = extrait();
        assert!(
            p.solar_from_max,
            "RadSolaire est NULL partout : le repli doit être signalé"
        );
        assert_eq!(p.records[1].solar_radiation, Some(412.0));
        assert_eq!(p.field_counts.solar, 2, "deux mesures sur trois ont un rayonnement");
    }

    #[test]
    fn la_rose_des_vents_francaise_est_convertie_en_degres() {
        assert_eq!(direction_en_degres("N"), Some(0.0));
        assert_eq!(direction_en_degres("NNE"), Some(22.5));
        assert_eq!(direction_en_degres("SO"), Some(225.0));
        assert_eq!(direction_en_degres("O"), Some(270.0), "O = Ouest, pas Est");
        assert_eq!(direction_en_degres("NNO"), Some(337.5));
        assert_eq!(direction_en_degres("sso"), Some(202.5), "insensible à la casse");

        // Absences.
        assert_eq!(direction_en_degres("NULL"), None);
        assert_eq!(direction_en_degres("---"), None);
        assert_eq!(direction_en_degres(""), None);

        // Un futur export en degrés reste lisible.
        assert_eq!(direction_en_degres("135"), Some(135.0));
        assert_eq!(direction_en_degres("360"), Some(0.0));
    }

    #[test]
    fn les_directions_du_fichier_sont_toutes_reconnues() {
        let p = extrait();
        assert_eq!(p.records[0].wind_direction_degrees, Some(225.0)); // SO
        assert_eq!(p.records[1].wind_direction_degrees, Some(202.5)); // SSO
        assert_eq!(p.records[2].wind_direction_degrees, Some(0.0)); // N
    }

    #[test]
    fn la_cadence_dominante_est_deduite() {
        let p = extrait();
        assert_eq!(p.sample_interval_seconds, Some(600));
    }

    #[test]
    fn les_mesures_sont_triees_meme_si_le_fichier_ne_l_est_pas() {
        let p = extrait();
        let instants: Vec<i64> = p.records.iter().map(|r| r.timestamp_utc.timestamp()).collect();
        let mut trie = instants.clone();
        trie.sort_unstable();
        assert_eq!(instants, trie);
    }

    #[test]
    fn un_instant_repete_est_ecarte() {
        let doublon = format!(
            "{EXTRAIT}31-05-2017,19:00,1496250000,99.9,99.9,99.9,10,1000.0,0.00,1.0,N,2.0,N,NULL,NULL\n"
        );
        let p = parse_romma_archives(&doublon).unwrap();
        assert_eq!(p.duplicate_count, 1);
        assert_eq!(p.records.len(), 3);
        assert_eq!(
            p.records[0].temperature_celsius, 12.2,
            "le premier enregistrement rencontré gagne"
        );
    }

    #[test]
    fn un_climato_est_refuse_avec_un_message_qui_oriente() {
        let climato = "Date,Tn,Tx,Tmoy,Amplitude,DureeGel,Pn,Px,Pmoy,PluieJour\n\
,degreC,degreC,degreC,degreC,min,hPa,hPa,hPa,mm\n\
31-05-2017,12.2,13.4,12.8,1.2,0,1021.0,1021.0,1021.0,0.0\n";
        let erreur = parse_romma_archives(climato).unwrap_err();
        assert!(matches!(erreur, RommaError::NoHeader));
        assert!(erreur.to_string().contains("climato"));
    }

    #[test]
    fn un_fichier_vide_est_refuse() {
        assert!(matches!(
            parse_romma_archives("   \n\n"),
            Err(RommaError::EmptyFile)
        ));
    }

    #[test]
    fn un_export_sans_ligne_d_unites_ne_perd_pas_sa_premiere_mesure() {
        let sans_unites = "\
Date,heure,timestamp,Temp,Hum
31-05-2017,19:00,1496250000,12.2,72
31-05-2017,19:10,1496250600,12.0,73
";
        let p = parse_romma_archives(sans_unites).unwrap();
        assert_eq!(p.records.len(), 2, "aucune ligne ne doit être sautée à tort");
        assert!(p.units.is_empty());
    }

    #[test]
    fn la_preuve_de_date_est_iso_donc_jamais_incertaine() {
        let e = romma_date_evidence(3);
        assert_eq!(e.iso_hits, 3);
        assert!(!e.uncertain);
        assert!(!e.conflicting);
        assert_eq!(e.ambiguous_hits, 0);
    }

    // --- Lecture en flux ----------------------------------------------------

    /// Archive triée : en-tête, unités, lignes valides et écartées mêlées, un
    /// trou de 30 min pour que la cadence dominante ait un concurrent.
    const TRIEE: &str = "\
Date,heure,timestamp,Temp,Tmin,Tmax,Hum,Pression,Pluie,Vmoy,VmoyDir,Rafale,RafaleDir,RadSolaire, RadSolaireMax
,,,degreC,degreC,degreC,%,hPa,mm,km/h,,km/h,,W/m2, W/m2
31-05-2017,19:00,1496250000,12.2,12.2,12.2,72,1021.0,0.00,10.0,SO,24.1,SO,NULL,NULL
31-05-2017,19:10,1496250600,12.0,12.0,12.0,73,1021.1,0.20,8.0,SSO,20.0,S,NULL,412

31-05-2017,19:20,1496251200,NULL,NULL,NULL,NULL,NULL,0.00,0.0,---,0.0,---,NULL,NULL
31-05-2017,19:30,1496251800,11.8,11.8,11.8,74,NULL,0.00,3.2,N,9.7,NNO,NULL,0
31-05-2017,19:40,pas-un-epoch,11.8,11.8,11.8,74,NULL,0.00,3.2,N,9.7,NNO,NULL,0
31-05-2017,20:10,1496253600,11.1,11.1,11.1,NULL,1020.4,0.00,1.0,O,2.0,O,NULL,NULL
31-05-2017,20:20,1496254200,10.9,10.9,10.9,80,1020.4,0.00,1.0,O,2.0,O,NULL,NULL
";

    fn en_flux(content: &str) -> RommaApercu {
        match lire_romma(content).expect("archive lisible") {
            RommaLecture::EnFlux(a) => a,
            RommaLecture::Complete(_) => panic!("une archive triée doit être lue en flux"),
        }
    }

    #[test]
    fn la_lecture_en_flux_dit_exactement_ce_que_dit_le_parseur_complet() {
        let complet = parse_romma_archives(TRIEE).unwrap();
        let apercu = en_flux(TRIEE);

        assert_eq!(apercu.record_count, complet.records.len());
        assert_eq!(apercu.record_count, 5);
        assert_eq!(Some(apercu.first_utc), complet.first_utc());
        assert_eq!(Some(apercu.last_utc), complet.last_utc());
        assert_eq!(apercu.sample_interval_seconds, complet.sample_interval_seconds);
        assert_eq!(apercu.sample_interval_seconds, Some(600));
        assert_eq!(apercu.skipped_count, complet.skipped_count);
        assert_eq!(apercu.warnings, complet.warnings);
        assert_eq!(apercu.total_data_lines, complet.total_data_lines);
        assert_eq!(apercu.solar_from_max, complet.solar_from_max);
        assert_eq!(apercu.field_counts, complet.field_counts);
        assert_eq!(apercu.columns(), complet.columns.as_slice());
        assert_eq!(apercu.units(), complet.units.as_slice());
        assert_eq!(apercu.sample, complet.records[..ROMMA_SAMPLE_LEN].to_vec());

        // La relecture retrouve les mêmes mesures, numéros de ligne compris, et
        // annonce sa longueur juste.
        let relues = apercu.records(TRIEE);
        assert_eq!(relues.len(), 5);
        assert_eq!(relues.collect::<Vec<_>>(), complet.records);
    }

    #[test]
    fn une_archive_en_desordre_ou_avec_doublon_prend_le_chemin_complet() {
        // Une mesure de 19:05 **après** celle de 20:20. (Dans `EXTRAIT`, la ligne
        // du 8 juin qui rompt l'ordre n'a pas de température : écartée, elle ne
        // compte pas, et l'extrait se lit bien en flux.)
        let desordre = format!(
            "{TRIEE}31-05-2017,19:05,1496250300,12.1,12.1,12.1,72,1021.0,0.00,9.0,SO,22.0,SO,NULL,NULL\n"
        );
        match lire_romma(&desordre).unwrap() {
            RommaLecture::Complete(p) => {
                assert_eq!(p.duplicate_count, 0);
                assert_eq!(p.records[1].timestamp_utc.timestamp(), 1_496_250_300);
                assert_eq!(p, parse_romma_archives(&desordre).unwrap());
            }
            RommaLecture::EnFlux(_) => panic!("une archive en désordre doit être triée"),
        }
        assert!(matches!(lire_romma(EXTRAIT).unwrap(), RommaLecture::EnFlux(_)));

        // Instant répété : trié au sens large, mais pas strictement.
        let doublon = format!(
            "{TRIEE}31-05-2017,20:20,1496254200,99.0,99.0,99.0,80,1020.4,0.00,1.0,O,2.0,O,NULL,NULL\n"
        );
        match lire_romma(&doublon).unwrap() {
            RommaLecture::Complete(p) => {
                assert_eq!(p.duplicate_count, 1);
                assert_eq!(p, parse_romma_archives(&doublon).unwrap());
            }
            RommaLecture::EnFlux(_) => panic!("un doublon doit passer par le chemin complet"),
        }
    }

    #[test]
    fn la_lecture_en_flux_garde_les_erreurs_du_parseur() {
        assert!(matches!(lire_romma("  \n"), Err(RommaError::EmptyFile)));
        assert!(matches!(lire_romma("a,b\n1,2\n"), Err(RommaError::NoHeader)));
        let sans_mesure = "timestamp,Temp\n1496250000,NULL\n";
        assert!(matches!(lire_romma(sans_mesure), Err(RommaError::NoDataRows(1))));
    }
}

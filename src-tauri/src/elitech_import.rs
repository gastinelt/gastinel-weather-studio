//! ┌───────────────────────────────────────────────────────────────────────────┐
//! │  ⚠️  PREMIÈRE HYPOTHÈSE — À VALIDER CONTRE UN FICHIER RÉEL  ⚠️              │
//! └───────────────────────────────────────────────────────────────────────────┘
//!
//! Ce module parse les exports du **Elitech RC-5**. Au moment où il est écrit,
//! nous n'avons **aucun fichier réel** sous la main (attendu sous 2-3 jours) et
//! il n'existe pas de spécification publique du format TXT d'ElitechLog. Tout ce
//! qui suit est donc une reconstruction plausible, volontairement tolérante.
//!
//! ## Hypothèses prises (liste exhaustive, à cocher dès réception d'un vrai export)
//!
//! 1. **Encodage** : décodé côté frontend (UTF-8 / UTF-16 LE-BE via BOM /
//!    Windows-1252 en repli) avant d'arriver ici. Ce module ne voit que de l'UTF-8.
//! 2. **Structure** : un bloc d'en-tête « clé : valeur » (numéro de série,
//!    intervalle d'échantillonnage, seuils d'alarme…), puis une ligne de titres
//!    de colonnes, puis les lignes de données.
//! 3. **Séparateur** : auto-détecté parmi tabulation, point-virgule, virgule,
//!    barre verticale — la tabulation gagne à égalité (cf.
//!    [`crate::parsing::CANDIDATE_DELIMITERS`]).
//! 4. **Colonnes** : repérées par leur intitulé (« Time », « Date », « Heure »,
//!    « Temperature(°C) », « Humidity(%RH) »…). Un éventuel index de ligne
//!    (« No. ») est simplement ignoré. Si aucun intitulé n'est reconnu, on bascule
//!    sur une détection **positionnelle** (premier champ qui ressemble à une date,
//!    puis premier champ numérique après).
//! 5. **Date ambiguë** `05/01/2024` : interprétée **jour d'abord**. C'est le point
//!    le plus risqué du module — voir [`crate::parsing::AMBIGUOUS_DATE_IS_DAY_FIRST`].
//! 6. **Fuseau** : les horodatages du fichier sont considérés comme **naïfs**
//!    (aucun fuseau). La conversion vers l'UTC est faite *en dehors* de ce module,
//!    par `clock_offset_seconds` (voir `commands.rs`). Ce parseur ne décide jamais
//!    d'un fuseau tout seul.
//! 7. **Tolérance** : une ligne malformée produit un [`ImportWarning`] et est
//!    ignorée ; seul un fichier totalement inexploitable produit une
//!    [`ImportError`].
//!
//! ## Quoi corriger quand le vrai fichier arrivera
//!
//! Dans 99 % des cas, il suffira de toucher :
//! * [`crate::parsing::AMBIGUOUS_DATE_IS_DAY_FIRST`] (ordre jour/mois) ;
//! * les listes de mots-clés de `looks_like_*_header` dans `parsing.rs` ;
//! * [`SERIAL_KEYS`] / [`INTERVAL_KEYS`] ci-dessous.
//!
//! Si le format s'avère trop exotique, l'utilisateur dispose de toute façon du
//! **mode manuel** (`manual_import.rs`) : colonnes mappées à la main.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::parsing::{
    date_token, detect_delimiter, looks_like_date_only_header, looks_like_humidity_header,
    looks_like_temperature_header, looks_like_time_only_header, looks_like_timestamp_header,
    normalize_content, parse_date_and_time_with_order, parse_datetime_flexible,
    parse_datetime_flexible_with_order, parse_interval_seconds, parse_number, split_line,
    DateOrderEvidence, DateOrderScanner,
};

// ---------------------------------------------------------------------------
// Types partagés avec l'import manuel
// ---------------------------------------------------------------------------
// Ils vivent ici parce que l'import Elitech est le chemin principal ;
// `manual_import.rs` (mode de secours) les réutilise.

/// Une mesure telle qu'elle sort du fichier : horodatage **brut**, non corrigé,
/// sans fuseau. La conversion en UTC est faite par l'appelant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawMeasurement {
    pub raw_timestamp: NaiveDateTime,
    pub temperature_celsius: f64,
    pub humidity_percent: Option<f64>,
    /// Numéro de ligne (1-based) dans le fichier source, pour le diagnostic.
    pub source_line: usize,
}

/// Ligne ignorée + raison, remontée telle quelle à l'utilisateur.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportWarning {
    pub line_number: usize,
    /// Tronquée à 200 caractères : on ne veut pas noyer l'UI.
    pub raw_line: String,
    pub reason: String,
}

impl ImportWarning {
    pub fn new(line_number: usize, raw_line: &str, reason: impl Into<String>) -> Self {
        let mut raw: String = raw_line.chars().take(200).collect();
        if raw_line.chars().count() > 200 {
            raw.push('…');
        }
        ImportWarning {
            line_number,
            raw_line: raw,
            reason: reason.into(),
        }
    }
}

/// Échecs qui empêchent d'exploiter le fichier **entier**.
#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize, Deserialize)]
pub enum ImportError {
    #[error("Le fichier est vide.")]
    EmptyFile,

    #[error(
        "Aucune ligne de données exploitable n'a été trouvée ({inspected_lines} lignes inspectées). \
         Le format ne correspond pas à ce que le parseur attend — utilisez l'import manuel."
    )]
    NoDataRows { inspected_lines: usize },

    #[error(
        "Impossible d'identifier une colonne d'horodatage ni par les intitulés, \
         ni par le contenu des lignes."
    )]
    NoTimestampColumn,

    #[error("Impossible d'identifier une colonne de température.")]
    NoTemperatureColumn,

    #[error("Mapping de colonnes invalide : {0}")]
    InvalidMapping(String),
}

/// Résultat brut d'un parsing : les mesures retenues et les lignes écartées.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedRows {
    pub measurements: Vec<RawMeasurement>,
    pub warnings: Vec<ImportWarning>,
    /// Ce que le fichier a prouvé (ou pas) de son ordre jour/mois. C'est ce qui
    /// alimente le drapeau « format de date incertain » porté par la série.
    pub date_order: DateOrderEvidence,
}

// ---------------------------------------------------------------------------
// Détection de structure
// ---------------------------------------------------------------------------

/// Où lire l'horodatage : une seule colonne, ou date + heure séparées.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimestampLayout {
    Single(usize),
    DateAndTime { date: usize, time: usize },
}

impl TimestampLayout {
    fn max_index(self) -> usize {
        match self {
            TimestampLayout::Single(i) => i,
            TimestampLayout::DateAndTime { date, time } => date.max(time),
        }
    }
}

/// Structure détectée dans le fichier — affichée telle quelle à l'utilisateur
/// pour qu'il puisse juger si l'auto-détection a vu juste.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedLayout {
    /// Rendu lisible du séparateur : `"TAB"`, `";"`, `","`, `"|"`.
    pub delimiter: String,
    /// Numéro (1-based) de la ligne d'intitulés, `None` si détection positionnelle.
    pub header_line_number: Option<usize>,
    pub header_labels: Vec<String>,
    pub timestamp: TimestampLayout,
    pub temperature_column: usize,
    pub humidity_column: Option<usize>,
    /// `true` si les colonnes ont été devinées par le contenu et non par les titres.
    pub positional_fallback: bool,
}

/// Métadonnées lues dans l'en-tête du fichier.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportMetadata {
    pub logger_serial: Option<String>,
    pub sample_interval_seconds: Option<i64>,
    /// Les lignes d'en-tête brutes, pour que l'utilisateur puisse vérifier.
    pub header_lines: Vec<String>,
}

/// Tout ce que le parseur a compris du fichier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElitechExport {
    pub metadata: ExportMetadata,
    pub layout: DetectedLayout,
    pub measurements: Vec<RawMeasurement>,
    pub warnings: Vec<ImportWarning>,
    pub total_lines: usize,
    /// Bilan de la détection d'ordre jour/mois — affiché à l'utilisateur avant
    /// l'import, pour qu'il puisse juger sur pièces.
    pub date_order: DateOrderEvidence,
}

/// Clés d'en-tête reconnues comme « numéro de série du logger », **de la plus
/// spécifique à la plus vague**. L'ordre compte : un en-tête contient souvent à
/// la fois `Device: RC-5` (le modèle) et `Serial Number: RC5000123` (ce qu'on veut).
///
/// « device » tout court est volontairement absent : c'est le modèle, pas le numéro.
pub const SERIAL_KEYS: [&str; 9] = [
    "serial number",
    "numéro de série",
    "numero de serie",
    "serial",
    "device id",
    "logger id",
    "instrument id",
    "s/n",
    "sn",
];

/// Clés d'en-tête reconnues comme « intervalle d'échantillonnage »,
/// de la plus spécifique à la plus vague.
pub const INTERVAL_KEYS: [&str; 9] = [
    "log interval",
    "sample interval",
    "sampling interval",
    "record interval",
    "intervalle d'enregistrement",
    "intervalle",
    "interval",
    "période",
    "periode",
];

// ---------------------------------------------------------------------------
// API publique
// ---------------------------------------------------------------------------

/// Signature demandée au §2 du cahier des charges : rend uniquement les mesures.
///
/// Enveloppe mince autour de [`parse_elitech_export_detailed`], qui est ce que
/// l'application utilise réellement (elle a besoin des métadonnées et des
/// avertissements). Conservée telle quelle pour rester fidèle à la spec et parce
/// qu'elle rend les tests plus lisibles.
pub fn parse_elitech_export(content: &str) -> Result<Vec<RawMeasurement>, ImportError> {
    parse_elitech_export_detailed(content).map(|e| e.measurements)
}

/// Parse complet : métadonnées + structure détectée + mesures + avertissements.
pub fn parse_elitech_export_detailed(content: &str) -> Result<ElitechExport, ImportError> {
    let normalized = normalize_content(content);
    let lines: Vec<&str> = normalized.lines().collect();

    if lines.iter().all(|l| l.trim().is_empty()) {
        return Err(ImportError::EmptyFile);
    }

    let delimiter = detect_delimiter(&lines);

    // --- 1. Tentative « par intitulés de colonnes »
    let header_hit = find_header_line(&lines, delimiter);

    let mut attempt: Option<(DetectedLayout, ParsedRows, usize)> = None;

    if let Some((header_idx, labels)) = header_hit.clone() {
        if let Some((timestamp, temperature_column, humidity_column)) = map_columns(&labels) {
            let layout = DetectedLayout {
                delimiter: render_delimiter(delimiter),
                header_line_number: Some(header_idx + 1),
                header_labels: labels,
                timestamp,
                temperature_column,
                humidity_column,
                positional_fallback: false,
            };
            let rows = parse_rows(&lines, header_idx + 1, delimiter, &layout);
            if !rows.measurements.is_empty() {
                attempt = Some((layout, rows, header_idx));
            }
        }
    }

    // --- 2. Repli positionnel : on devine les colonnes d'après le contenu.
    // Utile si les intitulés sont en chinois, absents, ou si la « ligne d'en-tête »
    // détectée était en fait une ligne de métadonnées.
    if attempt.is_none() {
        let data_start = header_hit.as_ref().map(|(i, _)| i + 1).unwrap_or(0);
        let (timestamp, temperature_column) = detect_layout_positional(&lines, data_start, delimiter)
            .ok_or(ImportError::NoDataRows {
                inspected_lines: lines.len(),
            })?;
        let layout = DetectedLayout {
            delimiter: render_delimiter(delimiter),
            header_line_number: None,
            header_labels: Vec::new(),
            timestamp,
            temperature_column,
            // En mode positionnel on ne devine pas l'humidité : une colonne
            // numérique de plus est trop ambiguë (point de rosée ? tension pile ?).
            humidity_column: None,
            positional_fallback: true,
        };
        let rows = parse_rows(&lines, data_start, delimiter, &layout);
        if rows.measurements.is_empty() {
            return Err(ImportError::NoDataRows {
                inspected_lines: lines.len(),
            });
        }
        attempt = Some((layout, rows, data_start.saturating_sub(1)));
    }

    let (layout, rows, header_idx) = attempt.expect("attempt est renseigné ou on a déjà quitté");

    // --- 3. Métadonnées : uniquement dans ce qui précède le tableau.
    let metadata = extract_metadata(&lines[..header_idx.min(lines.len())], delimiter);

    Ok(ElitechExport {
        metadata,
        layout,
        measurements: rows.measurements,
        warnings: rows.warnings,
        total_lines: lines.len(),
        date_order: rows.date_order,
    })
}

// ---------------------------------------------------------------------------
// Implémentation
// ---------------------------------------------------------------------------

fn render_delimiter(delimiter: char) -> String {
    match delimiter {
        '\t' => "TAB".to_string(),
        other => other.to_string(),
    }
}

/// Cherche la ligne d'intitulés : au moins deux champs, dont un qui ressemble à
/// un horodatage **et** un qui ressemble à une température.
///
/// On ne regarde que les 80 premières lignes : au-delà, c'est du corps de tableau.
fn find_header_line(lines: &[&str], delimiter: char) -> Option<(usize, Vec<String>)> {
    for (idx, line) in lines.iter().enumerate().take(80) {
        let fields = split_line(line, delimiter);
        if fields.len() < 2 {
            continue;
        }
        let has_ts = fields.iter().any(|f| looks_like_timestamp_header(f));
        let has_temp = fields.iter().any(|f| looks_like_temperature_header(f));
        if has_ts && has_temp {
            return Some((idx, fields));
        }
    }
    None
}

/// L'export porte-t-il une ligne d'intitulés que ce parseur reconnaît ?
///
/// Sert à deviner l'onglet d'import d'un fichier déposé : seule la détection
/// **par intitulés** compte ici, pas le repli positionnel, qui accepterait
/// presque n'importe quel tableau daté.
pub fn a_des_intitules_reconnus(content: &str) -> bool {
    let normalized = normalize_content(content);
    let lines: Vec<&str> = normalized.lines().take(200).collect();
    if lines.is_empty() {
        return false;
    }
    let delimiter = detect_delimiter(&lines);
    let Some((_, labels)) = find_header_line(&lines, delimiter) else {
        return false;
    };
    // Des intitulés « Date / Heure / Température » suffisent à la lecture,
    // mais pas à reconnaître ElitechLog : n'importe quel tableur les porte.
    // On exige en plus un marqueur propre à ElitechLog.
    map_columns(&labels).is_some() && (a_un_marqueur_elitech(&lines) || a_des_intitules_elitech(&labels))
}

/// Préambule d'un export ElitechLog : le nom du logiciel, ou la clé
/// `Serial Number:` de l'en-tête clé/valeur.
fn a_un_marqueur_elitech(lines: &[&str]) -> bool {
    lines.iter().take(40).any(|l| {
        let l = l.trim().to_lowercase();
        l.contains("elitech") || l.starts_with("serial number:")
    })
}

/// Intitulés anglais d'ElitechLog, unité collée entre parenthèses :
/// « Temperature(°C) », « Humidity(%RH) ».
fn a_des_intitules_elitech(labels: &[String]) -> bool {
    labels.iter().any(|l| {
        let l = l.trim().to_lowercase();
        l.starts_with("temperature(") || l.starts_with("humidity(")
    })
}

/// Associe les intitulés aux rôles (horodatage / température / humidité).
fn map_columns(labels: &[String]) -> Option<(TimestampLayout, usize, Option<usize>)> {
    let temperature = labels
        .iter()
        .position(|l| looks_like_temperature_header(l))?;
    let humidity = labels.iter().position(|l| looks_like_humidity_header(l));

    let date_only = labels.iter().position(|l| looks_like_date_only_header(l));
    let time_only = labels.iter().position(|l| looks_like_time_only_header(l));

    let timestamp = match (date_only, time_only) {
        (Some(d), Some(t)) if d != t => TimestampLayout::DateAndTime { date: d, time: t },
        _ => TimestampLayout::Single(labels.iter().position(|l| looks_like_timestamp_header(l))?),
    };

    Some((timestamp, temperature, humidity))
}

/// Devine les colonnes d'après le contenu des lignes, sans intitulés.
///
/// On retient la **première** ligne qui donne un couple (horodatage, nombre)
/// cohérent, et on applique ce schéma à tout le fichier — surtout pas une
/// détection ligne à ligne, qui décalerait silencieusement les colonnes.
fn detect_layout_positional(
    lines: &[&str],
    start: usize,
    delimiter: char,
) -> Option<(TimestampLayout, usize)> {
    for line in lines.iter().skip(start).take(500) {
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_line(line, delimiter);
        if fields.len() < 2 {
            continue;
        }

        for i in 0..fields.len() {
            let Some(dt) = parse_datetime_flexible(&fields[i]) else {
                continue;
            };

            // Cas « date seule dans une colonne, heure dans la suivante ».
            let (layout, first_value_idx) = if dt.time() == chrono::NaiveTime::MIN
                && i + 1 < fields.len()
                && crate::parsing::parse_time(&fields[i + 1]).is_some()
            {
                (
                    TimestampLayout::DateAndTime {
                        date: i,
                        time: i + 1,
                    },
                    i + 2,
                )
            } else {
                (TimestampLayout::Single(i), i + 1)
            };

            if let Some(temp_idx) = (first_value_idx..fields.len())
                .find(|j| parse_number(&fields[*j]).is_some())
            {
                return Some((layout, temp_idx));
            }
        }
    }
    None
}

/// Applique un schéma de colonnes à toutes les lignes de données.
///
/// **Deux passes** sur les mêmes lignes : la première ne fait que collecter les
/// jetons de date pour déterminer l'ordre jour/mois du fichier, la seconde parse
/// réellement avec cet ordre figé. On ne devine jamais ligne par ligne — ce serait
/// le meilleur moyen de décaler silencieusement une partie des mesures.
///
/// Le coût (un découpage de plus par ligne) est négligeable sur un export de
/// quelques milliers de lignes ; ne pas « optimiser » en gardant tout en mémoire.
fn parse_rows(
    lines: &[&str],
    start: usize,
    delimiter: char,
    layout: &DetectedLayout,
) -> ParsedRows {
    let mut measurements = Vec::new();
    let mut warnings = Vec::new();

    let needed = layout
        .timestamp
        .max_index()
        .max(layout.temperature_column)
        .max(layout.humidity_column.unwrap_or(0));

    // --- Passe 1 : de quel ordre jour/mois ce fichier témoigne-t-il ?
    let mut scanner = DateOrderScanner::new();
    for (offset, line) in lines.iter().enumerate().skip(start) {
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_line(line, delimiter);
        if fields.len() <= needed {
            continue;
        }
        let jeton = match layout.timestamp {
            TimestampLayout::Single(i) => date_token(&fields[i]),
            TimestampLayout::DateAndTime { date, .. } => date_token(&fields[date]),
        };
        scanner.observe(offset + 1, jeton);
    }
    let evidence = scanner.finish();

    // Un fichier qui se contredit n'est pas rejeté (l'utilisateur n'aurait aucune
    // issue) : on applique la majorité, on le dit, et la série sera marquée.
    if evidence.conflicting {
        let ligne = evidence.first_conflict_line.unwrap_or(0);
        let contenu = lines.get(ligne.saturating_sub(1)).copied().unwrap_or("");
        warnings.push(ImportWarning::new(
            ligne,
            contenu,
            format!(
                "dates contradictoires : {} preuve(s) « jour d'abord » et {} preuve(s) \
                 « mois d'abord » dans le même fichier. L'ordre majoritaire ({}) a été \
                 appliqué et la série est marquée à vérifier.",
                evidence.day_first_hits,
                evidence.month_first_hits,
                evidence.order.label_fr()
            ),
        ));
    }

    // --- Passe 2 : parsing effectif, ordre figé.
    let order = evidence.order;

    for (offset, line) in lines.iter().enumerate().skip(start) {
        let line_number = offset + 1; // 1-based, comme dans un éditeur de texte
        if line.trim().is_empty() {
            continue;
        }

        let fields = split_line(line, delimiter);
        if fields.len() <= needed {
            warnings.push(ImportWarning::new(
                line_number,
                line,
                format!(
                    "{} colonne(s) trouvée(s), au moins {} attendue(s)",
                    fields.len(),
                    needed + 1
                ),
            ));
            continue;
        }

        let raw_timestamp = match layout.timestamp {
            TimestampLayout::Single(i) => parse_datetime_flexible_with_order(&fields[i], order),
            TimestampLayout::DateAndTime { date, time } => {
                parse_date_and_time_with_order(&fields[date], &fields[time], order)
            }
        };

        let Some(raw_timestamp) = raw_timestamp else {
            warnings.push(ImportWarning::new(
                line_number,
                line,
                "horodatage illisible",
            ));
            continue;
        };

        let Some(temperature_celsius) = parse_number(&fields[layout.temperature_column]) else {
            warnings.push(ImportWarning::new(
                line_number,
                line,
                format!(
                    "température illisible dans la colonne {} (« {} »)",
                    layout.temperature_column + 1,
                    fields[layout.temperature_column]
                ),
            ));
            continue;
        };

        let humidity_percent = layout
            .humidity_column
            .and_then(|i| fields.get(i))
            .and_then(|f| parse_number(f));

        measurements.push(RawMeasurement {
            raw_timestamp,
            temperature_celsius,
            humidity_percent,
            source_line: line_number,
        });
    }

    ParsedRows {
        measurements,
        warnings,
        date_order: evidence,
    }
}

/// Extrait numéro de série et intervalle depuis le bloc d'en-tête.
///
/// Deux formes acceptées : `Clé : valeur` et `Clé<séparateur>valeur`.
fn extract_metadata(header_lines: &[&str], delimiter: char) -> ExportMetadata {
    let pairs: Vec<(String, String)> = header_lines
        .iter()
        .filter_map(|l| split_key_value(l, delimiter))
        .map(|(k, v)| (k.to_lowercase().trim().to_string(), v))
        .collect();

    // On balaie les clés par ordre de spécificité (et non les lignes par ordre
    // d'apparition) : `Serial Number` doit gagner contre `Device`, quelle que
    // soit leur position dans le fichier.
    let lookup = |keys: &[&str]| -> Option<String> {
        for key in keys {
            for (k, v) in &pairs {
                if (k == key || k.contains(key)) && !v.is_empty() {
                    return Some(v.clone());
                }
            }
        }
        None
    };

    ExportMetadata {
        logger_serial: lookup(&SERIAL_KEYS),
        sample_interval_seconds: lookup(&INTERVAL_KEYS).as_deref().and_then(parse_interval_seconds),
        header_lines: header_lines
            .iter()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    }
}

fn split_key_value(line: &str, delimiter: char) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // « Serial Number: RC5-000123 » — attention à ne pas couper sur le « : » d'une
    // heure : on exige que la partie gauche contienne au moins une lettre.
    if let Some(pos) = trimmed.find(':') {
        let (k, v) = trimmed.split_at(pos);
        if k.chars().any(|c| c.is_alphabetic()) {
            return Some((k.trim().to_string(), v[1..].trim().to_string()));
        }
    }

    let fields = split_line(trimmed, delimiter);
    if fields.len() >= 2 && fields[0].chars().any(|c| c.is_alphabetic()) {
        return Some((fields[0].clone(), fields[1].clone()));
    }

    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
    }

    /// Forme la plus probable d'un export ElitechLog : en-tête clé/valeur,
    /// titres de colonnes, données tabulées.
    const EXPORT_TABULE: &str = "Elitech Log Data\n\
Device: RC-5\n\
Serial Number: RC5000123\n\
Log Interval: 00:10:00\n\
Temperature Unit: °C\n\
\n\
No.\tTime\tTemperature(°C)\tHumidity(%RH)\n\
1\t2024-01-05 07:00:00\t-12.5\t88\n\
2\t2024-01-05 07:10:00\t-13.1\t89\n\
3\t2024-01-05 07:20:00\t-14.0\t90\n";

    #[test]
    fn parse_un_export_tabule() {
        let export = parse_elitech_export_detailed(EXPORT_TABULE).unwrap();

        assert_eq!(export.metadata.logger_serial.as_deref(), Some("RC5000123"));
        assert_eq!(export.metadata.sample_interval_seconds, Some(600));
        assert_eq!(export.layout.delimiter, "TAB");
        assert_eq!(export.layout.header_line_number, Some(7));
        assert!(!export.layout.positional_fallback);
        assert_eq!(export.layout.timestamp, TimestampLayout::Single(1));
        assert_eq!(export.layout.temperature_column, 2);
        assert_eq!(export.layout.humidity_column, Some(3));

        assert_eq!(export.measurements.len(), 3);
        assert!(export.warnings.is_empty());

        let first = &export.measurements[0];
        assert_eq!(first.raw_timestamp, dt(2024, 1, 5, 7, 0, 0));
        assert_eq!(first.temperature_celsius, -12.5);
        assert_eq!(first.humidity_percent, Some(88.0));
        assert_eq!(first.source_line, 8);
    }

    #[test]
    fn signature_du_cahier_des_charges() {
        let m = parse_elitech_export(EXPORT_TABULE).unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!(m[2].temperature_celsius, -14.0);
    }

    #[test]
    fn separateur_point_virgule_et_virgule_decimale() {
        let content = "Serial Number;RC5000999\n\
Date;Heure;Température;Humidité\n\
05/01/2024;07:00:00;-12,5;88\n\
05/01/2024;07:10:00;-13,1;89\n";
        let export = parse_elitech_export_detailed(content).unwrap();
        assert_eq!(export.layout.delimiter, ";");
        assert_eq!(
            export.layout.timestamp,
            TimestampLayout::DateAndTime { date: 0, time: 1 }
        );
        assert_eq!(export.measurements.len(), 2);
        assert_eq!(export.measurements[0].raw_timestamp, dt(2024, 1, 5, 7, 0, 0));
        assert_eq!(export.measurements[0].temperature_celsius, -12.5);
        assert_eq!(export.metadata.logger_serial.as_deref(), Some("RC5000999"));
    }

    #[test]
    fn tolerance_aux_lignes_malformees() {
        let content = "No.\tTime\tTemperature(°C)\n\
1\t2024-01-05 07:00:00\t-12.5\n\
2\tpas une date\t-13.1\n\
3\t2024-01-05 07:20:00\tHS\n\
\n\
4\t2024-01-05 07:30:00\n\
5\t2024-01-05 07:40:00\t-15.2\n";
        let export = parse_elitech_export_detailed(content).unwrap();

        assert_eq!(export.measurements.len(), 2, "seules 2 lignes sont valides");
        assert_eq!(export.warnings.len(), 3);

        // Les avertissements pointent la bonne ligne et disent pourquoi.
        assert_eq!(export.warnings[0].line_number, 3);
        assert!(export.warnings[0].reason.contains("horodatage"));
        assert_eq!(export.warnings[1].line_number, 4);
        assert!(export.warnings[1].reason.contains("température"));
        assert_eq!(export.warnings[2].line_number, 6);
        assert!(export.warnings[2].reason.contains("colonne"));
    }

    #[test]
    fn repli_positionnel_sans_entetes_reconnaissables() {
        // Intitulés en chinois non couverts + colonnes dans un ordre inhabituel :
        // le parseur doit quand même retrouver horodatage et température.
        let content = "1\t2024-01-05 07:00:00\t-12.5\n\
2\t2024-01-05 07:10:00\t-13.1\n";
        let export = parse_elitech_export_detailed(content).unwrap();
        assert!(export.layout.positional_fallback);
        assert_eq!(export.layout.timestamp, TimestampLayout::Single(1));
        assert_eq!(export.layout.temperature_column, 2);
        assert_eq!(export.measurements.len(), 2);
        assert_eq!(export.measurements[1].temperature_celsius, -13.1);
    }

    #[test]
    fn repli_positionnel_date_et_heure_separees() {
        let content = "1|2024-01-05|07:00:00|-12.5\n\
2|2024-01-05|07:10:00|-13.1\n";
        let export = parse_elitech_export_detailed(content).unwrap();
        assert!(export.layout.positional_fallback);
        assert_eq!(
            export.layout.timestamp,
            TimestampLayout::DateAndTime { date: 1, time: 2 }
        );
        assert_eq!(export.layout.temperature_column, 3);
        assert_eq!(export.measurements[0].raw_timestamp, dt(2024, 1, 5, 7, 0, 0));
    }

    #[test]
    fn fichier_vide() {
        assert!(matches!(
            parse_elitech_export_detailed(""),
            Err(ImportError::EmptyFile)
        ));
        assert!(matches!(
            parse_elitech_export_detailed("   \n \n"),
            Err(ImportError::EmptyFile)
        ));
    }

    #[test]
    fn fichier_sans_donnees() {
        let content = "Rapport ElitechLog\nAucune donnée enregistrée.\n";
        assert!(matches!(
            parse_elitech_export_detailed(content),
            Err(ImportError::NoDataRows { .. })
        ));
    }

    #[test]
    fn bom_et_fins_de_ligne_windows() {
        let content = "\u{feff}Time\tTemperature\r\n2024-01-05 07:00:00\t-12.5\r\n";
        let export = parse_elitech_export_detailed(content).unwrap();
        assert_eq!(export.measurements.len(), 1);
        assert_eq!(export.measurements[0].temperature_celsius, -12.5);
    }

    #[test]
    fn format_am_pm() {
        // « 5/1/2024 » est ambigu : jour d'abord par convention (cf. parsing.rs).
        let content = "Time\tTemp\n5/1/2024 7:00:00 AM\t-12.5\n5/1/2024 7:00:00 PM\t3.4\n";
        let export = parse_elitech_export_detailed(content).unwrap();
        assert_eq!(export.measurements[0].raw_timestamp, dt(2024, 1, 5, 7, 0, 0));
        assert_eq!(export.measurements[1].raw_timestamp, dt(2024, 1, 5, 19, 0, 0));
    }

    #[test]
    fn ligne_de_metadonnees_confondue_avec_un_entete() {
        // « Start Time » + « Temperature Unit » sur la même ligne ressemblent à des
        // titres de colonnes. Le parseur s'y trompe, mais la ligne de vrais titres
        // devient alors une ligne de données illisible → avertissement, pas de perte
        // de données : la seule vraie mesure est bien récupérée.
        let content = "Start Time: 2024-01-05 06:00:00\tTemperature Unit: C\n\
Time\tTemperature(°C)\n\
2024-01-05 07:00:00\t-12.5\n";
        let export = parse_elitech_export_detailed(content).unwrap();
        assert_eq!(export.measurements.len(), 1);
        assert_eq!(export.measurements[0].temperature_celsius, -12.5);
        assert_eq!(export.warnings.len(), 1);
        assert_eq!(export.warnings[0].line_number, 2);
    }

    #[test]
    fn les_avertissements_sont_tronques() {
        let long = "x".repeat(500);
        let w = ImportWarning::new(12, &long, "trop long");
        assert_eq!(w.raw_line.chars().count(), 201);
        assert!(w.raw_line.ends_with('…'));
    }

    // -----------------------------------------------------------------------
    // Ordre jour/mois
    // -----------------------------------------------------------------------

    #[test]
    fn un_export_avec_un_jour_superieur_a_douze_est_certain() {
        let export = parse_elitech_export_detailed(
            "Time\tTemperature(°C)\n\
             13/01/2024 07:00:00\t-21.4\n\
             05/01/2024 07:00:00\t-12.5\n",
        )
        .unwrap();

        assert!(
            !export.date_order.uncertain,
            "une seule preuve suffit à lever le doute"
        );
        assert_eq!(export.date_order.order, crate::parsing::DateOrder::DayFirst);
        assert_eq!(export.measurements.len(), 2);
        // La ligne ambiguë doit avoir été lue avec l'ordre prouvé par l'autre.
        assert_eq!(
            export.measurements[1].raw_timestamp.date(),
            chrono::NaiveDate::from_ymd_opt(2024, 1, 5).unwrap()
        );
    }

    /// Un fichier en `MM/dd` doit être détecté comme tel, **contre** le repli.
    #[test]
    fn un_export_en_mois_d_abord_est_detecte_contre_le_repli() {
        let export = parse_elitech_export_detailed(
            "Time\tTemperature(°C)\n\
             01/13/2024 07:00:00\t-21.4\n\
             01/05/2024 07:00:00\t-12.5\n",
        )
        .unwrap();

        assert_eq!(
            export.date_order.order,
            crate::parsing::DateOrder::MonthFirst
        );
        assert!(!export.date_order.uncertain);
        // 01/05 doit être lu comme le 5 janvier, pas le 1er mai.
        assert_eq!(
            export.measurements[1].raw_timestamp.date(),
            chrono::NaiveDate::from_ymd_opt(2024, 1, 5).unwrap()
        );
    }

    #[test]
    fn un_export_totalement_ambigu_leve_le_drapeau() {
        let export = parse_elitech_export_detailed(
            "Time\tTemperature(°C)\n\
             05/01/2024 07:00:00\t-12.5\n\
             06/01/2024 07:00:00\t-13.1\n",
        )
        .unwrap();

        assert!(export.date_order.uncertain, "rien ne prouve l'ordre ici");
        assert!(!export.date_order.prouve());
        assert_eq!(
            export.measurements.len(),
            2,
            "le doute ne doit pas empêcher l'import : on marque, on ne bloque pas"
        );
    }

    /// L'export de référence est en dates ISO : aucun drapeau ne doit se lever.
    #[test]
    fn un_export_iso_ne_leve_aucun_drapeau() {
        let export = parse_elitech_export_detailed(EXPORT_TABULE).unwrap();
        assert!(
            !export.date_order.uncertain,
            "l'ISO est auto-descriptif, il n'y a rien à confirmer"
        );
        assert!(export.date_order.iso_hits > 0);
        assert_eq!(export.date_order.ambiguous_hits, 0);
    }

    #[test]
    fn un_export_contradictoire_produit_un_avertissement() {
        let export = parse_elitech_export_detailed(
            "Time\tTemperature(°C)\n\
             13/01/2024 07:00:00\t-21.4\n\
             14/01/2024 07:00:00\t-22.0\n\
             01/13/2024 07:00:00\t-12.5\n",
        )
        .unwrap();

        assert!(export.date_order.conflicting);
        assert!(export.date_order.uncertain);
        assert_eq!(
            export.date_order.order,
            crate::parsing::DateOrder::DayFirst,
            "2 preuves contre 1 : la majorité l'emporte"
        );
        assert_eq!(
            export.measurements.len(),
            3,
            "aucune mesure ne doit être perdue à cause du conflit"
        );
        assert!(
            export
                .warnings
                .iter()
                .any(|w| w.reason.contains("contradictoires")),
            "le conflit doit être signalé à l'utilisateur"
        );
    }
}

//! **Mode de secours** de l'import : CSV générique dont les colonnes sont
//! désignées explicitement par l'utilisateur.
//!
//! Raison d'être : tant qu'on n'a pas vu un vrai export ElitechLog, il est
//! probable que l'auto-détection de `elitech_import.rs` se trompe. Ce module ne
//! devine **rien** — l'utilisateur regarde un aperçu du fichier et dit
//! « colonne 2 = date, colonne 3 = heure, colonne 4 = température ». C'est aussi
//! le chemin à utiliser pour réimporter un XLS ElitechLog réenregistré en CSV
//! depuis Excel, ou pour recopier des données d'une autre source.
//!
//! Seuls le nettoyage des cellules et le parsing des valeurs (dates ambiguës,
//! virgule décimale, unités) restent partagés avec l'import automatique — voir
//! `parsing.rs`.

use serde::{Deserialize, Serialize};

use crate::elitech_import::{ImportError, ImportWarning, ParsedRows, RawMeasurement};
use crate::parsing::{
    date_token, detect_delimiter, looks_like_humidity_header, looks_like_temperature_header,
    looks_like_timestamp_header, normalize_content, parse_date_and_time_with_order,
    parse_datetime_flexible, parse_datetime_flexible_with_order, parse_number, split_line,
    DateOrderScanner,
};

/// Description, fournie par l'utilisateur, de la structure du fichier.
///
/// Les index de colonnes sont **0-based** (l'UI affiche « colonne 1 » pour 0).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManualMapping {
    /// `"TAB"`, `";"`, `","`, `"|"`… (voir [`delimiter_from_str`]).
    pub delimiter: String,
    /// Nombre de lignes à ignorer en tête (bloc de métadonnées).
    #[serde(default)]
    pub skip_lines: usize,
    /// La première ligne conservée est-elle une ligne de titres ?
    #[serde(default)]
    pub has_header: bool,
    /// Colonne contenant la date — ou l'horodatage complet si `time_column` est vide.
    pub datetime_column: usize,
    /// Colonne contenant l'heure, si elle est séparée de la date.
    #[serde(default)]
    pub time_column: Option<usize>,
    pub temperature_column: usize,
    #[serde(default)]
    pub humidity_column: Option<usize>,
}

/// Traduit le libellé de séparateur venu de l'UI en caractère.
pub fn delimiter_from_str(raw: &str) -> Result<char, ImportError> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "TAB" | "\\T" | "TABULATION" => Ok('\t'),
        "" => Err(ImportError::InvalidMapping(
            "aucun séparateur choisi".to_string(),
        )),
        _ => {
            let mut chars = raw.trim().chars();
            let first = chars.next().unwrap();
            if chars.next().is_some() {
                Err(ImportError::InvalidMapping(format!(
                    "séparateur « {raw} » : un seul caractère attendu (ou TAB)"
                )))
            } else {
                Ok(first)
            }
        }
    }
}

fn render_delimiter(delimiter: char) -> String {
    match delimiter {
        '\t' => "TAB".to_string(),
        other => other.to_string(),
    }
}

/// Aperçu tabulé du contenu, pour que l'UI puisse afficher les colonnes numérotées.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelimitedPreview {
    pub delimiter: String,
    /// Les `max_rows` premières lignes découpées, telles quelles.
    pub rows: Vec<Vec<String>>,
    /// Nombre de colonnes le plus fréquent.
    pub column_count: usize,
    pub total_lines: usize,
    /// Mapping proposé par défaut. L'utilisateur reste libre de tout changer :
    /// c'est une suggestion, pas une détection sur laquelle on s'appuie.
    pub suggested: Option<ManualMapping>,
}

/// Découpe le début du fichier pour alimenter l'écran de mappage.
///
/// `delimiter` vide ⇒ auto-détection.
pub fn preview_delimited(
    content: &str,
    delimiter: Option<&str>,
    max_rows: usize,
) -> Result<DelimitedPreview, ImportError> {
    let normalized = normalize_content(content);
    let lines: Vec<&str> = normalized.lines().collect();
    if lines.iter().all(|l| l.trim().is_empty()) {
        return Err(ImportError::EmptyFile);
    }

    let delimiter = match delimiter.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => delimiter_from_str(d)?,
        None => detect_delimiter(&lines),
    };

    let rows: Vec<Vec<String>> = lines
        .iter()
        .take(max_rows)
        .map(|l| split_line(l, delimiter))
        .collect();

    let column_count = most_frequent_column_count(&rows);
    let suggested = suggest_mapping(&rows, delimiter);

    Ok(DelimitedPreview {
        delimiter: render_delimiter(delimiter),
        rows,
        column_count,
        total_lines: lines.len(),
        suggested,
    })
}

fn most_frequent_column_count(rows: &[Vec<String>]) -> usize {
    let mut counts: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for r in rows {
        *counts.entry(r.len()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(len, freq)| (*freq, *len))
        .map(|(len, _)| len)
        .unwrap_or(0)
}

/// Suggestion de mappage : titres reconnus si possible, sinon première colonne
/// qui contient une date et première colonne numérique qui suit.
fn suggest_mapping(rows: &[Vec<String>], delimiter: char) -> Option<ManualMapping> {
    let base = ManualMapping {
        delimiter: render_delimiter(delimiter),
        skip_lines: 0,
        has_header: false,
        datetime_column: 0,
        time_column: None,
        temperature_column: 1,
        humidity_column: None,
    };

    // a) via les titres
    for (idx, row) in rows.iter().enumerate().take(40) {
        if row.len() < 2 {
            continue;
        }
        let ts = row.iter().position(|c| looks_like_timestamp_header(c));
        let temp = row.iter().position(|c| looks_like_temperature_header(c));
        if let (Some(ts), Some(temp)) = (ts, temp) {
            return Some(ManualMapping {
                skip_lines: idx,
                has_header: true,
                datetime_column: ts,
                temperature_column: temp,
                humidity_column: row.iter().position(|c| looks_like_humidity_header(c)),
                ..base
            });
        }
    }

    // b) via le contenu
    for (idx, row) in rows.iter().enumerate().take(200) {
        if row.len() < 2 {
            continue;
        }
        for i in 0..row.len() {
            let Some(dt) = parse_datetime_flexible(&row[i]) else {
                continue;
            };
            let (time_column, next) = if dt.time() == chrono::NaiveTime::MIN
                && i + 1 < row.len()
                && crate::parsing::parse_time(&row[i + 1]).is_some()
            {
                (Some(i + 1), i + 2)
            } else {
                (None, i + 1)
            };
            if let Some(temp) = (next..row.len()).find(|j| parse_number(&row[*j]).is_some()) {
                return Some(ManualMapping {
                    skip_lines: idx,
                    has_header: false,
                    datetime_column: i,
                    time_column,
                    temperature_column: temp,
                    humidity_column: None,
                    ..base
                });
            }
        }
    }

    None
}

/// Parse le contenu en appliquant **exactement** le mappage fourni.
///
/// Aucune auto-détection ici : si l'utilisateur se trompe de colonne, il verra des
/// avertissements ligne par ligne plutôt qu'un import silencieusement décalé.
pub fn parse_manual(content: &str, mapping: &ManualMapping) -> Result<ParsedRows, ImportError> {
    let delimiter = delimiter_from_str(&mapping.delimiter)?;
    validate_mapping(mapping)?;

    let normalized = normalize_content(content);
    let lines: Vec<&str> = normalized.lines().collect();
    if lines.iter().all(|l| l.trim().is_empty()) {
        return Err(ImportError::EmptyFile);
    }

    let mut start = mapping.skip_lines;
    if mapping.has_header {
        start += 1;
    }
    if start >= lines.len() {
        return Err(ImportError::NoDataRows {
            inspected_lines: lines.len(),
        });
    }

    let needed = mapping
        .datetime_column
        .max(mapping.time_column.unwrap_or(0))
        .max(mapping.temperature_column)
        .max(mapping.humidity_column.unwrap_or(0));

    let mut measurements = Vec::new();
    let mut warnings = Vec::new();

    // --- Passe 1 : ordre jour/mois du fichier (cf. `elitech_import::parse_rows`).
    // Le mappage dit *où* est la date ; il ne dit rien de son ordre, qui reste à
    // détecter exactement comme dans l'import automatique.
    let mut scanner = DateOrderScanner::new();
    for (offset, line) in lines.iter().enumerate().skip(start) {
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_line(line, delimiter);
        if fields.len() <= needed {
            continue;
        }
        scanner.observe(offset + 1, date_token(&fields[mapping.datetime_column]));
    }
    let evidence = scanner.finish();

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
        let line_number = offset + 1;
        if line.trim().is_empty() {
            continue;
        }

        let fields = split_line(line, delimiter);
        if fields.len() <= needed {
            warnings.push(ImportWarning::new(
                line_number,
                line,
                format!(
                    "{} colonne(s) trouvée(s), au moins {} attendue(s) d'après le mappage",
                    fields.len(),
                    needed + 1
                ),
            ));
            continue;
        }

        let raw_timestamp = match mapping.time_column {
            Some(t) => {
                parse_date_and_time_with_order(&fields[mapping.datetime_column], &fields[t], order)
            }
            None => parse_datetime_flexible_with_order(&fields[mapping.datetime_column], order),
        };

        let Some(raw_timestamp) = raw_timestamp else {
            warnings.push(ImportWarning::new(
                line_number,
                line,
                format!(
                    "horodatage illisible dans la colonne {} (« {} »)",
                    mapping.datetime_column + 1,
                    fields[mapping.datetime_column]
                ),
            ));
            continue;
        };

        let Some(temperature_celsius) = parse_number(&fields[mapping.temperature_column]) else {
            warnings.push(ImportWarning::new(
                line_number,
                line,
                format!(
                    "température illisible dans la colonne {} (« {} »)",
                    mapping.temperature_column + 1,
                    fields[mapping.temperature_column]
                ),
            ));
            continue;
        };

        let humidity_percent = mapping
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

    if measurements.is_empty() {
        return Err(ImportError::NoDataRows {
            inspected_lines: lines.len(),
        });
    }

    Ok(ParsedRows {
        measurements,
        warnings,
        date_order: evidence,
    })
}

fn validate_mapping(m: &ManualMapping) -> Result<(), ImportError> {
    if m.datetime_column == m.temperature_column {
        return Err(ImportError::InvalidMapping(
            "la colonne d'horodatage et celle de température doivent être différentes".to_string(),
        ));
    }
    if m.time_column == Some(m.temperature_column) {
        return Err(ImportError::InvalidMapping(
            "la colonne d'heure et celle de température doivent être différentes".to_string(),
        ));
    }
    if m.time_column == Some(m.datetime_column) {
        return Err(ImportError::InvalidMapping(
            "la colonne de date et celle d'heure doivent être différentes".to_string(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn mapping() -> ManualMapping {
        ManualMapping {
            delimiter: ",".to_string(),
            skip_lines: 0,
            has_header: true,
            datetime_column: 0,
            time_column: None,
            temperature_column: 1,
            humidity_column: None,
        }
    }

    #[test]
    fn import_csv_simple() {
        let content = "date;temp\n2024-01-05 07:00:00;-12.5\n2024-01-05 07:10:00;-13.1\n";
        let m = ManualMapping {
            delimiter: ";".to_string(),
            ..mapping()
        };
        let parsed = parse_manual(content, &m).unwrap();
        assert_eq!(parsed.measurements.len(), 2);
        assert!(parsed.warnings.is_empty());
        assert_eq!(
            parsed.measurements[0].raw_timestamp,
            NaiveDate::from_ymd_opt(2024, 1, 5)
                .unwrap()
                .and_hms_opt(7, 0, 0)
                .unwrap()
        );
        assert_eq!(parsed.measurements[1].temperature_celsius, -13.1);
    }

    #[test]
    fn colonnes_date_et_heure_separees_avec_metadonnees_en_tete() {
        let content = "Export maison\nGénéré le 12/02/2024\n\
Date,Heure,Capteur,T,HR\n\
05/01/2024,07:00:00,A,-12,5,88\n";
        // Fichier volontairement piégeux : virgule décimale ET séparateur virgule.
        // On vérifie skip_lines + has_header + colonnes date/heure séparées, et on
        // documente au passage ce qui se passe quand le séparateur coupe un nombre.
        let m = ManualMapping {
            delimiter: ",".to_string(),
            skip_lines: 2,
            has_header: true,
            datetime_column: 0,
            time_column: Some(1),
            temperature_column: 3,
            humidity_column: None,
        };
        let parsed = parse_manual(content, &m).unwrap();
        assert_eq!(parsed.measurements.len(), 1);
        assert_eq!(
            parsed.measurements[0].raw_timestamp,
            NaiveDate::from_ymd_opt(2024, 1, 5)
                .unwrap()
                .and_hms_opt(7, 0, 0)
                .unwrap()
        );
        // La virgule décimale a été mangée par le séparateur : on lit « -12 ».
        // C'est le comportement attendu — d'où l'aperçu avant import.
        assert_eq!(parsed.measurements[0].temperature_celsius, -12.0);
        assert_eq!(parsed.measurements[0].source_line, 4);
    }

    #[test]
    fn humidite_optionnelle() {
        let content = "2024-01-05 07:00:00\t-12.5\t88\n";
        let m = ManualMapping {
            delimiter: "TAB".to_string(),
            has_header: false,
            humidity_column: Some(2),
            ..mapping()
        };
        let parsed = parse_manual(content, &m).unwrap();
        assert_eq!(parsed.measurements[0].humidity_percent, Some(88.0));
    }

    #[test]
    fn lignes_malformees_signalees_sans_bloquer() {
        let content = "2024-01-05 07:00:00;-12.5\n\
bidon;-13.1\n\
2024-01-05 07:20:00;HS\n\
2024-01-05 07:30:00\n\
2024-01-05 07:40:00;-15.2\n";
        let m = ManualMapping {
            delimiter: ";".to_string(),
            has_header: false,
            ..mapping()
        };
        let parsed = parse_manual(content, &m).unwrap();
        assert_eq!(parsed.measurements.len(), 2);
        assert_eq!(parsed.warnings.len(), 3);
        assert_eq!(parsed.warnings[0].line_number, 2);
        assert!(parsed.warnings[0].reason.contains("horodatage"));
        assert_eq!(parsed.warnings[1].line_number, 3);
        assert_eq!(parsed.warnings[2].line_number, 4);
    }

    #[test]
    fn mappage_invalide_rejete() {
        let m = ManualMapping {
            temperature_column: 0,
            ..mapping()
        };
        assert!(matches!(
            parse_manual("a,b\n", &m),
            Err(ImportError::InvalidMapping(_))
        ));

        let m = ManualMapping {
            delimiter: "abc".to_string(),
            ..mapping()
        };
        assert!(matches!(
            parse_manual("a,b\n", &m),
            Err(ImportError::InvalidMapping(_))
        ));
    }

    #[test]
    fn aucune_donnee_exploitable() {
        let m = ManualMapping {
            delimiter: ";".to_string(),
            has_header: false,
            ..mapping()
        };
        assert!(matches!(
            parse_manual("rien;du tout\nvraiment;rien\n", &m),
            Err(ImportError::NoDataRows { .. })
        ));
        assert!(matches!(
            parse_manual("", &m),
            Err(ImportError::EmptyFile)
        ));
    }

    #[test]
    fn apercu_et_suggestion_sur_entetes() {
        let content = "No.;Time;Temperature(°C);Humidity(%RH)\n1;2024-01-05 07:00:00;-12,5;88\n";
        let preview = preview_delimited(content, None, 20).unwrap();
        assert_eq!(preview.delimiter, ";");
        assert_eq!(preview.column_count, 4);
        assert_eq!(preview.total_lines, 2);
        assert_eq!(preview.rows[0][1], "Time");

        let s = preview.suggested.unwrap();
        assert_eq!(s.skip_lines, 0);
        assert!(s.has_header);
        assert_eq!(s.datetime_column, 1);
        assert_eq!(s.temperature_column, 2);
        assert_eq!(s.humidity_column, Some(3));
    }

    #[test]
    fn apercu_et_suggestion_sans_entetes() {
        let content = "1\t2024-01-05\t07:00:00\t-12.5\n2\t2024-01-05\t07:10:00\t-13.1\n";
        let preview = preview_delimited(content, Some("TAB"), 20).unwrap();
        let s = preview.suggested.unwrap();
        assert!(!s.has_header);
        assert_eq!(s.datetime_column, 1);
        assert_eq!(s.time_column, Some(2));
        assert_eq!(s.temperature_column, 3);
    }

    #[test]
    fn separateurs_acceptes() {
        assert_eq!(delimiter_from_str("TAB").unwrap(), '\t');
        assert_eq!(delimiter_from_str("tab").unwrap(), '\t');
        assert_eq!(delimiter_from_str(";").unwrap(), ';');
        assert_eq!(delimiter_from_str("|").unwrap(), '|');
        assert!(delimiter_from_str("").is_err());
        assert!(delimiter_from_str(";;").is_err());
    }

    // -----------------------------------------------------------------------
    // Ordre jour/mois
    // -----------------------------------------------------------------------

    /// Le mappage manuel dit *où* est la date ; il ne dit rien de son ordre,
    /// qui doit être détecté exactement comme dans l'import automatique.
    #[test]
    fn l_import_manuel_detecte_aussi_l_ordre_des_dates() {
        let mut m = mapping();
        m.delimiter = ";".to_string();
        m.time_column = Some(1);
        m.temperature_column = 2;

        let parsed = parse_manual(
            "Date;Heure;Temp\n13/01/2024;07:00:00;-21,4\n05/01/2024;07:00:00;-12,5\n",
            &m,
        )
        .unwrap();

        assert!(!parsed.date_order.uncertain, "la ligne 13/01 fait preuve");
        assert_eq!(parsed.date_order.order, crate::parsing::DateOrder::DayFirst);
        assert_eq!(
            parsed.measurements[1].raw_timestamp.date(),
            NaiveDate::from_ymd_opt(2024, 1, 5).unwrap()
        );
    }

    #[test]
    fn l_import_manuel_signale_un_fichier_ambigu() {
        let mut m = mapping();
        m.delimiter = ";".to_string();
        m.time_column = Some(1);
        m.temperature_column = 2;

        let parsed = parse_manual(
            "Date;Heure;Temp\n05/01/2024;07:00:00;-12,5\n06/01/2024;07:00:00;-13,1\n",
            &m,
        )
        .unwrap();

        assert!(parsed.date_order.uncertain);
        assert_eq!(parsed.measurements.len(), 2, "on marque, on ne bloque pas");
    }
}

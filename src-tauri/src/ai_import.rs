//! Import assisté par IA — analyse automatique de fichiers (CSV, JSON, TXT)
//! et de captures d'écran via Groq (LLM texte + Vision).
//!
//! ## Principes de conception
//!
//! 1. **Validation non contournable** : L'IA ne sert qu'à *proposer* un mapping
//!    ou extraire des lignes depuis une image. L'utilisateur visualise toujours les
//!    données extraites et le mapping proposé avant de confirmer l'import.
//! 2. **Réutilisation de l'infrastructure existante** : Même client Groq, même
//!    gestion de clé API (pi_keys.rs), même pipeline d'insertion (persist_import,
//!    ingerprint.rs, DateOrderScanner).
//! 3. **Tolérance et robustesse** : En cas d'ambiguïté, l'utilisateur peut fournir
//!    une consigne textuelle libre. Si l'IA échoue, l'import manuel classique reste
//!    disponible.

use serde::{Deserialize, Serialize};

use crate::elitech_import::{ImportError, ImportWarning, ParsedRows, RawMeasurement};
use crate::error::{AppError, AppResult};
use crate::groq::{GroqClient, GroqError};
use crate::manual_import::delimiter_from_str;
use crate::parsing::{
    date_token, normalize_content, parse_datetime_flexible_with_order, parse_number, split_line,
    DateOrderScanner,
};

// ---------------------------------------------------------------------------
// Structures de données pour l'analyse IA
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiFileAnalysisRequest {
    pub file_name: String,
    pub content: String,
    pub user_instruction: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiFileMapping {
    /// "csv", "tsv", "json", ou "custom"
    pub file_type: String,
    /// Séparateur (";", ",", "TAB", "|")
    pub delimiter: String,
    pub skip_lines: usize,
    pub has_header: bool,
    pub datetime_column: usize,
    pub time_column: Option<usize>,
    pub temperature_column: usize,
    pub humidity_column: Option<usize>,
    pub pressure_column: Option<usize>,
    pub wind_speed_column: Option<usize>,
    pub rain_column: Option<usize>,
    /// Pour le format JSON : chemin vers la liste d'enregistrements (ex: "data", "records" ou "")
    pub json_records_path: Option<String>,
    pub json_time_field: Option<String>,
    pub json_temp_field: Option<String>,
    pub json_humidity_field: Option<String>,
    /// Explication claire en français générée par l'IA pour l'utilisateur
    pub explanation: String,
    /// Indice de confiance estimé (0.0 à 1.0)
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiPreviewRow {
    pub source_line: usize,
    pub raw_timestamp: String,
    pub temperature: f64,
    pub humidity: Option<f64>,
    pub pressure: Option<f64>,
    pub wind_speed: Option<f64>,
    pub rain: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiTextAnalysisResult {
    pub mapping: AiFileMapping,
    pub preview_rows: Vec<AiPreviewRow>,
    pub total_lines: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiVisionAnalysisRequest {
    pub file_name: String,
    /// Image encodée en base64 (sans le préfixe data URL)
    pub image_base64: String,
    /// Type MIME (ex: "image/png", "image/jpeg", "image/webp")
    pub mime_type: String,
    pub user_instruction: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiVisionAnalysisResult {
    pub rows: Vec<AiPreviewRow>,
    pub explanation: String,
    pub warnings: Vec<String>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AiSourceKind {
    /// Parsing de fichier texte avec mapping validé
    TextMapped { mapping: AiFileMapping },
    /// Lignes de mesures directes (validées ou issues de vision)
    DirectRows { rows: Vec<AiPreviewRow> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiImportPayload {
    pub common: crate::commands::ImportCommon,
    pub source_kind: AiSourceKind,
}

// ---------------------------------------------------------------------------
// Prompts pour l'IA
// ---------------------------------------------------------------------------

const SYSTEM_PROMPT_TEXT_ANALYSIS: &str = r#"Tu es un analyseur expert de fichiers météorologiques (CSV, TSV, JSON, TXT).
Ton rôle est d'analyser un échantillon de fichier fourni et de déterminer la structure exacte pour mapper les données vers les champs suivants :
- Date / Horodatage (colonne ou champ timestamp/date/time)
- Température en °C (colonne ou champ temperature)
- Humidité relative en % (optionnel)
- Pression atmosphérique en hPa (optionnel)
- Vitesse du vent en km/h (optionnel)
- Précipitations / pluie en mm (optionnel)

RÈGLES STRICTES :
1. Réponds UNIQUEMENT sous forme d'un objet JSON valide, sans texte additionnel ni markdown en dehors du JSON.
2. Les index de colonnes sont 0-based (la première colonne est 0).
3. Si la date et l'heure sont dans deux colonnes séparées, renseigne datetime_column pour la date et time_column pour l'heure. Si elles sont réunies, time_column doit être null.
4. Identifie le séparateur réel ("," , ";" , "TAB" , "|") et le nombre de lignes d'en-tête à ignorer.
5. Si le fichier est un JSON, remplis json_records_path (ex: "data", "records", ou vide pour la racine), json_time_field, json_temp_field, json_humidity_field.
6. Rédige une brève explication en français (2 phrases max) claire et factuelle dans le champ "explanation".

FORMAT DU JSON ATTENDU :
{
  "file_type": "csv",
  "delimiter": ";",
  "skip_lines": 0,
  "has_header": true,
  "datetime_column": 0,
  "time_column": null,
  "temperature_column": 1,
  "humidity_column": 2,
  "pressure_column": null,
  "wind_speed_column": null,
  "rain_column": null,
  "json_records_path": null,
  "json_time_field": null,
  "json_temp_field": null,
  "json_humidity_field": null,
  "explanation": "Fichier CSV délimité par des points-virgules...",
  "confidence": 0.95
}"#;

const SYSTEM_PROMPT_VISION_ANALYSIS: &str = r#"Tu es un assistant de vision expert en extraction de données météo depuis des captures d'écran, graphiques, tableaux et afficheurs de stations météo.
Ton rôle est d'extraire fidèlement toutes les lignes de mesures visibles dans l'image sous forme structurée.

RÈGLES STRICTES :
1. Réponds UNIQUEMENT sous la forme d'un objet JSON valide :
{
  "rows": [
    {
      "timestamp": "YYYY-MM-DD HH:MM:SS",
      "temperature": 12.5,
      "humidity": 65.0,
      "pressure": 1013.2,
      "wind_speed": 15.0,
      "rain": 0.0
    }
  ],
  "explanation": "Brève description en français de ce qui a été extrait de l'image.",
  "confidence": 0.90,
  "warnings": []
}
2. Ne falsifie aucune valeur : si un chiffre est illisible ou douteux, signale-le dans warnings.
3. Si l'année manque dans l'image, utilise l'année en cours ou laisse la date sous format MM-DD HH:MM.
4. Les températures doivent être en degrés Celsius. Si l'affichage est en Fahrenheit, convertis en °C."#;

// ---------------------------------------------------------------------------
// Fonctions d'analyse
// ---------------------------------------------------------------------------

/// Analyse un fichier texte (CSV/TSV/JSON/TXT) avec Groq.
pub fn analyze_text_with_ai(
    client: &GroqClient,
    req: &AiFileAnalysisRequest,
) -> AppResult<AiTextAnalysisResult> {
    let normalized = normalize_content(&req.content);
    let lines: Vec<&str> = normalized.lines().collect();
    if lines.is_empty() || lines.iter().all(|l| l.trim().is_empty()) {
        return Err(AppError::Other("Le fichier est vide.".to_string()));
    }

    let sample_lines: Vec<&str> = lines.iter().take(40).copied().collect();
    let sample = sample_lines.join("\n");

    let mut user_prompt = format!(
        "Nom du fichier : {}\nNombre total de lignes : {}\n\nÉchantillon du contenu :\n`\n{}\n`\n",
        req.file_name,
        lines.len(),
        sample
    );

    if let Some(instruction) = &req.user_instruction {
        if !instruction.trim().is_empty() {
            user_prompt.push_str(&format!(
                "\nConsigne spécifique de l'utilisateur :\n{}\n",
                instruction.trim()
            ));
        }
    }

    let reply = client
        .complete_json(SYSTEM_PROMPT_TEXT_ANALYSIS, &user_prompt, None)
        .map_err(|e| match e {
            GroqError::CleIntrouvable(msg) => AppError::Other(format!(
                "Clé API Groq manquante pour l'assistant IA.\n{msg}"
            )),
            GroqError::Auth(msg) => AppError::Other(format!("Clé API Groq refusée : {msg}")),
            GroqError::Quota(msg) => AppError::Other(msg),
            other => AppError::Other(format!("Erreur d'analyse IA : {other}")),
        })?;

    let mapping = parse_ai_mapping_json(&reply.text)?;
    let (preview_rows, warnings) = generate_preview_from_text(&normalized, &mapping, 10)?;

    Ok(AiTextAnalysisResult {
        mapping,
        preview_rows,
        total_lines: lines.len(),
        warnings,
    })
}

/// Analyse une image ou capture d'écran avec le modèle Vision de Groq.
pub fn analyze_vision_with_ai(
    client: &GroqClient,
    req: &AiVisionAnalysisRequest,
) -> AppResult<AiVisionAnalysisResult> {
    if req.image_base64.trim().is_empty() {
        return Err(AppError::Other("Aucune image fournie.".to_string()));
    }

    let mime = if req.mime_type.is_empty() {
        "image/png"
    } else {
        req.mime_type.as_str()
    };

    let data_url = format!("data:{mime};base64,{}", req.image_base64.trim());

    let mut user_text = format!("Analyse cette image ({}) et extrait les relevés météo.", req.file_name);
    if let Some(inst) = &req.user_instruction {
        if !inst.trim().is_empty() {
            user_text.push_str(&format!("\nConsigne additionnelle : {}", inst.trim()));
        }
    }

    let reply = client
        .complete_vision(
            SYSTEM_PROMPT_VISION_ANALYSIS,
            &user_text,
            &data_url,
            None,
        )
        .map_err(|e| match e {
            GroqError::CleIntrouvable(msg) => AppError::Other(format!(
                "Clé API Groq manquante pour l'assistant Vision.\n{msg}"
            )),
            GroqError::Auth(msg) => AppError::Other(format!("Clé API Groq refusée : {msg}")),
            GroqError::Quota(msg) => AppError::Other(msg),
            other => AppError::Other(format!("Erreur d'analyse Vision IA : {other}")),
        })?;

    parse_vision_result_json(&reply.text)
}

// ---------------------------------------------------------------------------
// Décodage des réponses JSON de l'IA
// ---------------------------------------------------------------------------

fn extract_json_block(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                return &trimmed[start..=end];
            }
        }
    }
    trimmed
}

fn parse_ai_mapping_json(raw_json: &str) -> AppResult<AiFileMapping> {
    let clean_json = extract_json_block(raw_json);
    serde_json::from_str::<AiFileMapping>(clean_json).map_err(|e| {
        AppError::Other(format!(
            "Réponse JSON de l'IA non conforme : {e}\nTexte brut reçu : {raw_json}"
        ))
    })
}

#[derive(Deserialize)]
struct RawVisionOutput {
    #[serde(default)]
    rows: Vec<RawVisionRow>,
    #[serde(default)]
    explanation: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    warnings: Vec<String>,
}

#[derive(Deserialize)]
struct RawVisionRow {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    time: Option<String>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    temp: Option<f64>,
    #[serde(default)]
    humidity: Option<f64>,
    #[serde(default)]
    pressure: Option<f64>,
    #[serde(default)]
    wind_speed: Option<f64>,
    #[serde(default)]
    rain: Option<f64>,
}

fn parse_vision_result_json(raw_json: &str) -> AppResult<AiVisionAnalysisResult> {
    let clean_json = extract_json_block(raw_json);
    let parsed: RawVisionOutput = serde_json::from_str(clean_json).map_err(|e| {
        AppError::Other(format!(
            "Réponse Vision IA non conforme : {e}\nTexte brut : {raw_json}"
        ))
    })?;

    let mut rows = Vec::new();
    let mut warnings = parsed.warnings;

    for (idx, r) in parsed.rows.into_iter().enumerate() {
        let ts = r
            .timestamp
            .or_else(|| {
                if let (Some(d), Some(t)) = (r.date, r.time) {
                    Some(format!("{d} {t}"))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| format!("Ligne {}", idx + 1));

        let temp = r.temperature.or(r.temp).unwrap_or(0.0);

        rows.push(AiPreviewRow {
            source_line: idx + 1,
            raw_timestamp: ts,
            temperature: temp,
            humidity: r.humidity,
            pressure: r.pressure,
            wind_speed: r.wind_speed,
            rain: r.rain,
        });
    }

    if rows.is_empty() {
        warnings.push("Aucune ligne de mesure exploitable n'a été détectée dans l'image.".to_string());
    }

    Ok(AiVisionAnalysisResult {
        rows,
        explanation: parsed
            .explanation
            .unwrap_or_else(|| "Extraction visuelle terminée.".to_string()),
        warnings,
        confidence: parsed.confidence.unwrap_or(0.8),
    })
}

// ---------------------------------------------------------------------------
// Application du mapping et génération de l'aperçu
// ---------------------------------------------------------------------------

pub fn generate_preview_from_text(
    content: &str,
    mapping: &AiFileMapping,
    max_rows: usize,
) -> AppResult<(Vec<AiPreviewRow>, Vec<String>)> {
    let parsed = parse_content_with_ai_mapping(content, mapping)?;
    let mut preview = Vec::new();
    let mut warnings = Vec::new();

    for w in &parsed.warnings {
        warnings.push(format!("Ligne {} : {}", w.line_number, w.reason));
    }

    for m in parsed.measurements.iter().take(max_rows) {
        preview.push(AiPreviewRow {
            source_line: m.source_line,
            raw_timestamp: m.raw_timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            temperature: m.temperature_celsius,
            humidity: m.humidity_percent,
            pressure: None,
            wind_speed: None,
            rain: None,
        });
    }

    Ok((preview, warnings))
}

/// Parse le contenu entier selon le mapping IA fourni.
pub fn parse_content_with_ai_mapping(
    content: &str,
    mapping: &AiFileMapping,
) -> Result<ParsedRows, ImportError> {
    if mapping.file_type == "json" {
        return parse_json_content(content, mapping);
    }

    let delimiter = delimiter_from_str(&mapping.delimiter)?;
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
                    "{} colonne(s) trouvée(s), au moins {} attendue(s)",
                    fields.len(),
                    needed + 1
                ),
            ));
            continue;
        }

        let raw_timestamp = match mapping.time_column {
            Some(t) => crate::parsing::parse_date_and_time_with_order(
                &fields[mapping.datetime_column],
                &fields[t],
                order,
            ),
            None => parse_datetime_flexible_with_order(&fields[mapping.datetime_column], order),
        };

        let raw_timestamp = match raw_timestamp {
            Some(dt) => dt,
            None => {
                warnings.push(ImportWarning::new(
                    line_number,
                    line,
                    format!("date/heure illisible dans « {} »", fields[mapping.datetime_column]),
                ));
                continue;
            }
        };

        let temperature = match parse_number(&fields[mapping.temperature_column]) {
            Some(t) => t,
            None => {
                warnings.push(ImportWarning::new(
                    line_number,
                    line,
                    format!(
                        "température invalide dans « {} »",
                        fields[mapping.temperature_column]
                    ),
                ));
                continue;
            }
        };

        let humidity = mapping
            .humidity_column
            .and_then(|idx| fields.get(idx))
            .and_then(|raw| parse_number(raw));

        measurements.push(RawMeasurement {
            raw_timestamp,
            temperature_celsius: temperature,
            humidity_percent: humidity,
            source_line: line_number,
        });
    }

    Ok(ParsedRows {
        measurements,
        warnings,
        date_order: evidence,
    })
}

/// Parse du contenu JSON.
fn parse_json_content(content: &str, mapping: &AiFileMapping) -> Result<ParsedRows, ImportError> {
    let root: serde_json::Value = serde_json::from_str(content).map_err(|e| {
        ImportError::InvalidMapping(format!("JSON invalide : {e}"))
    })?;

    let array = match &mapping.json_records_path {
        Some(path) if !path.trim().is_empty() => {
            let mut cur = &root;
            for segment in path.split('.') {
                if let Some(next) = cur.get(segment) {
                    cur = next;
                } else {
                    return Err(ImportError::InvalidMapping(format!(
                        "Chemin JSON « {path} » introuvable"
                    )));
                }
            }
            cur.as_array().ok_or_else(|| {
                ImportError::InvalidMapping(format!("Le chemin JSON « {path} » n'est pas un tableau"))
            })?
        }
        _ => {
            if let Some(arr) = root.as_array() {
                arr
            } else if let Some(arr) = root.get("data").and_then(|v| v.as_array()) {
                arr
            } else if let Some(arr) = root.get("records").and_then(|v| v.as_array()) {
                arr
            } else {
                return Err(ImportError::InvalidMapping(
                    "Aucun tableau d'enregistrements trouvé à la racine du JSON".to_string(),
                ));
            }
        }
    };

    let time_field = mapping.json_time_field.as_deref().unwrap_or("timestamp");
    let temp_field = mapping.json_temp_field.as_deref().unwrap_or("temperature");
    let hum_field = mapping.json_humidity_field.as_deref();

    let mut measurements = Vec::new();
    let mut warnings = Vec::new();
    let scanner = DateOrderScanner::new();

    for (idx, item) in array.iter().enumerate() {
        let line_number = idx + 1;
        let time_val = item
            .get(time_field)
            .or_else(|| item.get("time"))
            .or_else(|| item.get("date"))
            .or_else(|| item.get("datetime"));

        let item_str = item.to_string();
        let time_str = match time_val {
            Some(v) if v.is_string() => v.as_str().unwrap().to_string(),
            Some(v) if v.is_number() => {
                let ts = v.as_i64().unwrap_or(0);
                chrono::DateTime::from_timestamp(ts, 0)
                    .map(|dt| dt.naive_utc().format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_default()
            }
            _ => {
                warnings.push(ImportWarning::new(
                    line_number,
                    &item_str,
                    format!("champ d'horodatage « {time_field} » manquant"),
                ));
                continue;
            }
        };

        let raw_timestamp = match parse_datetime_flexible_with_order(
            &time_str,
            crate::parsing::DateOrder::DayFirst,
        ) {
            Some(dt) => dt,
            None => {
                warnings.push(ImportWarning::new(
                    line_number,
                    &item_str,
                    format!("horodatage illisible : « {time_str} »"),
                ));
                continue;
            }
        };

        let temp_val = item
            .get(temp_field)
            .or_else(|| item.get("temp"))
            .or_else(|| item.get("temperature_celsius"));

        let temperature = match temp_val {
            Some(v) => {
                if let Some(f) = v.as_f64() {
                    f
                } else if let Some(s) = v.as_str() {
                    match parse_number(s) {
                        Some(f) => f,
                        None => {
                            warnings.push(ImportWarning::new(
                                line_number,
                                &item_str,
                                format!("température non numérique : « {s} »"),
                            ));
                            continue;
                        }
                    }
                } else {
                    warnings.push(ImportWarning::new(
                        line_number,
                        &item_str,
                        format!("température absente ou invalide"),
                    ));
                    continue;
                }
            }
            None => {
                warnings.push(ImportWarning::new(
                    line_number,
                    &item_str,
                    format!("champ température « {temp_field} » introuvable"),
                ));
                continue;
            }
        };

        let humidity = hum_field
            .and_then(|f| item.get(f))
            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(parse_number)));

        measurements.push(RawMeasurement {
            raw_timestamp,
            temperature_celsius: temperature,
            humidity_percent: humidity,
            source_line: line_number,
        });
    }

    Ok(ParsedRows {
        measurements,
        warnings,
        date_order: scanner.finish(),
    })
}

/// Convertit des lignes directes (vision ou modifiées dans l'UI) en `ParsedRows`.
pub fn convert_direct_rows_to_parsed(rows: &[AiPreviewRow]) -> Result<ParsedRows, ImportError> {
    if rows.is_empty() {
        return Err(ImportError::EmptyFile);
    }

    let mut measurements = Vec::new();
    let mut warnings = Vec::new();

    for r in rows {
        let dt = match parse_datetime_flexible_with_order(
            &r.raw_timestamp,
            crate::parsing::DateOrder::DayFirst,
        ) {
            Some(dt) => dt,
            None => {
                warnings.push(ImportWarning::new(
                    r.source_line,
                    &r.raw_timestamp,
                    format!("horodatage illisible : « {} »", r.raw_timestamp),
                ));
                continue;
            }
        };

        measurements.push(RawMeasurement {
            raw_timestamp: dt,
            temperature_celsius: r.temperature,
            humidity_percent: r.humidity,
            source_line: r.source_line,
        });
    }

    if measurements.is_empty() {
        return Err(ImportError::NoDataRows {
            inspected_lines: rows.len(),
        });
    }

    Ok(ParsedRows {
        measurements,
        warnings,
        date_order: DateOrderScanner::new().finish(),
    })
}

// ---------------------------------------------------------------------------
// Tests unitaires
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ai_mapping_json_valide() {
        let json = r#"{
            "file_type": "csv",
            "delimiter": ";",
            "skip_lines": 1,
            "has_header": true,
            "datetime_column": 0,
            "time_column": 1,
            "temperature_column": 2,
            "humidity_column": 3,
            "pressure_column": null,
            "wind_speed_column": null,
            "rain_column": null,
            "json_records_path": null,
            "json_time_field": null,
            "json_temp_field": null,
            "json_humidity_field": null,
            "explanation": "CSV standard avec date en col 0, heure en col 1, temp en col 2.",
            "confidence": 0.98
        }"#;

        let mapping = parse_ai_mapping_json(json).expect("Mapping valide");
        assert_eq!(mapping.delimiter, ";");
        assert_eq!(mapping.datetime_column, 0);
        assert_eq!(mapping.time_column, Some(1));
        assert_eq!(mapping.temperature_column, 2);
        assert_eq!(mapping.humidity_column, Some(3));
        assert!(mapping.confidence > 0.9);
    }

    #[test]
    fn parsing_csv_avec_mapping_ia() {
        let content = "Titre du fichier\nDate;Heure;Temperature;Humidite\n01/02/2024;14:00;15.2;60\n01/02/2024;14:15;15.0;62";
        let mapping = AiFileMapping {
            file_type: "csv".to_string(),
            delimiter: ";".to_string(),
            skip_lines: 1,
            has_header: true,
            datetime_column: 0,
            time_column: Some(1),
            temperature_column: 2,
            humidity_column: Some(3),
            pressure_column: None,
            wind_speed_column: None,
            rain_column: None,
            json_records_path: None,
            json_time_field: None,
            json_temp_field: None,
            json_humidity_field: None,
            explanation: "Test".to_string(),
            confidence: 1.0,
        };

        let (preview, warnings) = generate_preview_from_text(content, &mapping, 5).unwrap();
        assert_eq!(preview.len(), 2);
        assert_eq!(preview[0].temperature, 15.2);
        assert_eq!(preview[0].humidity, Some(60.0));
        assert!(warnings.is_empty());
    }

    #[test]
    fn parsing_json_avec_mapping_ia() {
        let json_content = r#"{
            "station": "TestSite",
            "data": [
                {"timestamp": "2024-02-01 12:00:00", "temperature": 18.5, "humidity": 55.0},
                {"timestamp": "2024-02-01 12:15:00", "temperature": 18.2, "humidity": 56.0}
            ]
        }"#;

        let mapping = AiFileMapping {
            file_type: "json".to_string(),
            delimiter: "".to_string(),
            skip_lines: 0,
            has_header: false,
            datetime_column: 0,
            time_column: None,
            temperature_column: 0,
            humidity_column: None,
            pressure_column: None,
            wind_speed_column: None,
            rain_column: None,
            json_records_path: Some("data".to_string()),
            json_time_field: Some("timestamp".to_string()),
            json_temp_field: Some("temperature".to_string()),
            json_humidity_field: Some("humidity".to_string()),
            explanation: "JSON Test".to_string(),
            confidence: 1.0,
        };

        let parsed = parse_content_with_ai_mapping(json_content, &mapping).unwrap();
        assert_eq!(parsed.measurements.len(), 2);
        assert_eq!(parsed.measurements[0].temperature_celsius, 18.5);
        assert_eq!(parsed.measurements[0].humidity_percent, Some(55.0));
    }

    #[test]
    fn parsing_vision_output_valide() {
        let vision_json = r#"{
            "rows": [
                {"timestamp": "2024-01-15 08:30:00", "temperature": -4.2, "humidity": 80.0},
                {"timestamp": "2024-01-15 08:45:00", "temperature": -4.5, "humidity": 82.0}
            ],
            "explanation": "Tableau extrait de la capture d'écran.",
            "confidence": 0.95,
            "warnings": []
        }"#;

        let res = parse_vision_result_json(vision_json).unwrap();
        assert_eq!(res.rows.len(), 2);
        assert_eq!(res.rows[0].temperature, -4.2);
        assert_eq!(res.rows[1].temperature, -4.5);
    }

    #[test]
    #[ignore = "dépend du réseau et consomme le quota Groq"]
    fn test_reel_vision_ai_import() {
        let client = match GroqClient::from_local_key() {
            Ok(c) => c,
            Err(e) => {
                println!("[ai-import-vision] pas de clé exploitable : {e}");
                return;
            }
        };

        let image_bytes = match std::fs::read("../docs/samples/test_table_meteo.png") {
            Ok(b) => b,
            Err(_) => match std::fs::read("docs/samples/test_table_meteo.png") {
                Ok(b) => b,
                Err(e) => panic!("Impossible de lire docs/samples/test_table_meteo.png : {e}"),
            },
        };

        fn b64_encode(bytes: &[u8]) -> String {
            const CHARSET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
            for chunk in bytes.chunks(3) {
                let b0 = chunk[0] as usize;
                let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
                let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
                let triple = (b0 << 16) | (b1 << 8) | b2;
                out.push(CHARSET[(triple >> 18) & 0x3F] as char);
                out.push(CHARSET[(triple >> 12) & 0x3F] as char);
                if chunk.len() > 1 {
                    out.push(CHARSET[(triple >> 6) & 0x3F] as char);
                } else {
                    out.push('=');
                }
                if chunk.len() > 2 {
                    out.push(CHARSET[triple & 0x3F] as char);
                } else {
                    out.push('=');
                }
            }
            out
        }

        let req = AiVisionAnalysisRequest {
            file_name: "test_table_meteo.png".to_string(),
            image_base64: b64_encode(&image_bytes),
            mime_type: "image/png".to_string(),
            user_instruction: Some("Extrais les mesures visibles".to_string()),
        };

        let res = analyze_vision_with_ai(&client, &req).expect("Échec analyze_vision_with_ai");
        println!("[ai-import-vision] {} mesures extraites :", res.rows.len());
        for r in &res.rows {
            println!("  -> {} | {} °C | hum: {:?} | press: {:?}", r.raw_timestamp, r.temperature, r.humidity, r.pressure);
        }
        assert_eq!(res.rows.len(), 3);
        assert!((res.rows[0].temperature - 18.5).abs() < 0.1);
    }
}

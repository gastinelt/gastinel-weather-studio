//! Parseur de l'export **réel** d'ElitechLog — un classeur xlsx déguisé en `.xls`.
//!
//! ┌───────────────────────────────────────────────────────────────────────────┐
//! │  Ce module remplace l'hypothèse tabulaire de `elitech_import.rs`.         │
//! │  Il est écrit contre un fichier réel, pas contre une reconstruction.      │
//! └───────────────────────────────────────────────────────────────────────────┘
//!
//! ## Le fichier de référence
//!
//! `EFE205004333_20260819221959.xls`, produit par ElitechLogWin 8.0.5 le
//! 19 août 2026 : RC-5 n° `EFE205004333`, 10 307 mesures du 1er mai au 16 août
//! 2026, cadence 15 minutes.
//!
//! ## Ce que le fichier est vraiment
//!
//! **Un `.xlsx`, malgré son extension `.xls`.** Ses huit premiers octets sont
//! `50 4B 03 04` (`PK` — une archive zip), donc du OOXML, et non le format
//! binaire CFB des vieux Excel. Le producteur est la bibliothèque .NET NPOI.
//! Conséquence pratique : **la détection de format par l'extension est un piège**
//! — `calamine::open_workbook_auto` ouvrirait une branche `Xls` qui échouerait.
//! On instancie donc `Xlsx` explicitement, après avoir vérifié la signature zip.
//!
//! ## Structure du classeur
//!
//! Trois feuilles, nommées **en français** (la langue de l'interface au moment de
//! l'export — un export anglais les nommerait autrement) :
//!
//! | # | Nom observé | Contenu |
//! |---|---|---|
//! | 1 | `Résumé` | modèle, n° de série, type de capteur, intervalle, seuils d'alarme, bornes |
//! | 2 | `Tableau de données` | le tableau `No.` / `Temps` / `Température°C` |
//! | 3 | `Graph` | une image JPEG de la courbe — **ignorée** |
//!
//! Les feuilles sont repérées **par leur contenu**, jamais par leur nom ni par
//! leur rang : c'est ce qui permet au parseur de survivre à un export dans une
//! autre langue. La feuille de données est celle dont une ligne porte à la fois
//! un intitulé d'horodatage et un intitulé de température.
//!
//! ## Pièges du contenu
//!
//! 1. **Toutes les cellules sont du texte** (`t="inlineStr"`), y compris les
//!    nombres et les dates. Il n'y a aucun type natif Excel à convertir — juste
//!    du texte à lire correctement. Le parseur accepte quand même les types
//!    natifs, au cas où une version future d'ElitechLog changerait d'avis.
//! 2. **La virgule est le séparateur décimal** : `"22,2"`, jamais `"22.2"`.
//!    C'est [`crate::parsing::parse_number`] qui s'en charge (il traite déjà le
//!    cas, y compris `"1 234,5"`).
//! 3. **Le `sharedStrings.xml` est vide** (`count="0"`) : tout est en chaîne
//!    inline. Un lecteur qui supposerait des chaînes partagées lirait des cellules
//!    vides.
//! 4. La feuille de résumé est un **tableau à deux colonnes de paires**
//!    (`A`/`B` puis `D`/`E`) et non une liste de lignes `clé: valeur`.
//!
//! ## Le temps : ISO, mais naïf
//!
//! La colonne `Temps` vaut `"2026-05-01 14:07:31"` — année, mois, jour, dans cet
//! ordre. **Il n'y a donc aucune ambiguïté jour/mois à trancher**, et le
//! [`crate::parsing::DateOrderScanner`] est court-circuité, comme il l'est déjà
//! pour Infoclimat (voir [`crate::parsing::DateOrderEvidence::iso`]).
//!
//! ⚠️ Ne pas confondre « non ambigu » et « en UTC ». L'horodatage reste **naïf** :
//! c'est l'heure de l'horloge interne du RC-5, sans fuseau. Contrairement à
//! Infoclimat (`raw_timestamp_was_utc = true` d'office), cette source garde donc
//! tout le mécanisme de `clock_offset_seconds` — c'est lui, et lui seul, qui
//! ramène ces instants à l'UTC vrai.
//!
//! ## Les formats qu'on ne lit pas — et pourquoi
//!
//! ElitechLog exporte trois formats en même temps. Un seul est exploitable :
//!
//! | Format | Décision | Motif |
//! |---|---|---|
//! | `.xls` (xlsx) | **supporté** | OOXML documenté, lisible par n'importe quel tableur, contenu complet |
//! | `.esf` | **non supporté** | chiffré, aucune spécification publique. Rien à quoi s'accrocher : ni en-tête lisible, ni structure devinable. |
//! | `.elt` | **non supporté** | sérialisation binaire .NET propriétaire. Techniquement approchable par rétro-ingénierie, mais le format n'est pas documenté et **change avec les versions d'ElitechLogWin** : un parseur écrit contre 8.0.5 casserait sans prévenir à la mise à jour suivante, en silence et sur des données de terrain irremplaçables. |
//!
//! Cette décision est **actée** : ne pas la rouvrir sans un élément nouveau (une
//! spécification publiée, ou un export xlsx qui s'avérerait incomplet). Le xlsx
//! contient déjà tout ce que l'application utilise.
//!
//! ## Si un jour un export ne suit plus cette structure
//!
//! Le mode d'import manuel ([`crate::manual_import`]) reste disponible : il prend
//! n'importe quel texte délimité et laisse l'utilisateur désigner les colonnes.
//! C'est le filet de sécurité, et il n'a pas été retiré.

use std::io::Cursor;

use calamine::{Data, Range, Reader, Xlsx};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::anomaly::{detect_jumps_raw, JumpThresholds, TemperatureJump};
use crate::elitech_import::{ImportError, ImportWarning, RawMeasurement};
use crate::parsing::{
    looks_like_temperature_header, looks_like_timestamp_header, parse_interval_seconds,
    parse_number, DateOrderEvidence,
};

/// Signature d'une archive zip — donc d'un fichier OOXML.
pub const ZIP_MAGIC: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];

/// `true` si ces octets sont ceux d'un classeur OOXML (xlsx), quelle que soit
/// l'extension du fichier.
///
/// C'est le test qui permet à l'UI de router un fichier `.xls` vers *ce* parseur
/// plutôt que vers celui des exports tabulaires.
pub fn looks_like_xlsx(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[..4] == ZIP_MAGIC
}

/// Métadonnées lues sur la feuille de résumé.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct XlsxMetadata {
    /// `S/N` — ex. `"EFE205004333"`.
    pub logger_serial: Option<String>,
    /// `Intervalle d'enregistrement` — `"15m"` devient 900.
    pub sample_interval_seconds: Option<i64>,
    /// `Type de capteur` — ex. `"Température"`.
    pub sensor_type: Option<String>,
    /// `Modèle` — ex. `"RC-5"`.
    pub device_model: Option<String>,
    /// Toutes les paires clé/valeur trouvées, dans l'ordre de lecture, pour que
    /// l'utilisateur puisse vérifier ce que le parseur a vu.
    pub summary_pairs: Vec<(String, String)>,
}

/// Tout ce que le classeur a livré.
#[derive(Debug, Clone, Serialize)]
pub struct ElitechXlsxExport {
    pub metadata: XlsxMetadata,
    /// Nom de la feuille de données retenue (`"Tableau de données"`).
    pub data_sheet: String,
    /// Nom de la feuille de résumé exploitée, s'il y en avait une.
    pub summary_sheet: Option<String>,
    /// Toutes les feuilles du classeur, dans l'ordre — aide au diagnostic.
    pub sheet_names: Vec<String>,
    /// Intitulés de la ligne d'en-tête retenue.
    pub header_labels: Vec<String>,
    pub measurements: Vec<RawMeasurement>,
    pub warnings: Vec<ImportWarning>,
    /// Lignes de données parcourues, en-tête comprise.
    pub total_rows: usize,
    /// Toujours une preuve « ISO » : ce format ne peut pas être ambigu.
    pub date_order: DateOrderEvidence,
    /// Sauts de température jugés trop brutaux — **signalés, pas écartés**.
    pub anomalies: Vec<TemperatureJump>,
}

// ---------------------------------------------------------------------------
// Clés de la feuille de résumé
// ---------------------------------------------------------------------------
//
// Chaque entrée est cherchée en minuscules et sans accent (voir `normaliser`) :
// « Modèle » et « Modele » tombent sur la même clé, et un export anglais est
// couvert par les variantes correspondantes.

const SERIAL_KEYS: [&str; 5] = ["s/n", "sn", "serial number", "numero de serie", "serial"];
const INTERVAL_KEYS: [&str; 4] = [
    "intervalle d'enregistrement",
    "log interval",
    "sample interval",
    "record interval",
];
const SENSOR_KEYS: [&str; 3] = ["type de capteur", "sensor type", "channel type"];
const MODEL_KEYS: [&str; 3] = ["modele", "model", "device model"];

// ---------------------------------------------------------------------------
// API publique
// ---------------------------------------------------------------------------

/// Parse un export ElitechLog xlsx depuis ses octets bruts.
///
/// Prend des `&[u8]` et non un chemin : c'est ce qui permet au frontend de lire
/// le fichier lui-même (il le fait déjà pour les autres formats) et de garder un
/// seul chemin de code entre l'import interactif et les tests.
pub fn parse_elitech_xlsx(bytes: &[u8]) -> Result<ElitechXlsxExport, ImportError> {
    parse_elitech_xlsx_with(bytes, &JumpThresholds::default())
}

/// Variante à seuils d'anomalie explicites — utilisée par les tests.
pub fn parse_elitech_xlsx_with(
    bytes: &[u8],
    thresholds: &JumpThresholds,
) -> Result<ElitechXlsxExport, ImportError> {
    if bytes.is_empty() {
        return Err(ImportError::EmptyFile);
    }
    if !looks_like_xlsx(bytes) {
        return Err(ImportError::InvalidMapping(
            "Ce fichier n'est pas un classeur xlsx : ses premiers octets ne sont pas ceux \
             d'une archive zip. Un export ElitechLog `.xls` est en réalité du xlsx ; \
             s'il s'agit d'un `.esf` ou d'un `.elt`, ces formats ne sont pas lisibles \
             (voir la documentation du module) — réexportez en « Excel » depuis ElitechLog."
                .to_string(),
        ));
    }

    let mut classeur: Xlsx<_> = Xlsx::new(Cursor::new(bytes)).map_err(|e| {
        ImportError::InvalidMapping(format!(
            "classeur xlsx illisible : {e}. Le fichier est peut-être tronqué \
             (téléchargement interrompu) ou protégé par mot de passe."
        ))
    })?;

    let sheet_names = classeur.sheet_names();
    if sheet_names.is_empty() {
        return Err(ImportError::EmptyFile);
    }

    // Chaque feuille est lue une fois et gardée : le classeur ne fait que
    // quelques centaines de Ko, et relire la feuille de données pour trouver
    // l'en-tête puis à nouveau pour les lignes doublerait le travail.
    let mut feuilles: Vec<(String, Range<Data>)> = Vec::with_capacity(sheet_names.len());
    for nom in &sheet_names {
        match classeur.worksheet_range(nom) {
            Ok(plage) => feuilles.push((nom.clone(), plage)),
            // Une feuille illisible (la feuille « Graph » n'est qu'une image)
            // ne doit pas faire échouer l'import.
            Err(_) => continue,
        }
    }

    let (index_donnees, tableau) = feuilles
        .iter()
        .enumerate()
        .find_map(|(i, (_, plage))| trouver_tableau(plage).map(|t| (i, t)))
        .ok_or(ImportError::NoTimestampColumn)?;

    let (nom_donnees, plage_donnees) = &feuilles[index_donnees];
    let Tableau {
        ligne_entete,
        colonnes,
        timestamp_col,
        temperature_col,
        humidity_col,
    } = tableau;

    let (measurements, warnings) = lire_lignes(
        plage_donnees,
        ligne_entete,
        timestamp_col,
        temperature_col,
        humidity_col,
    );

    if measurements.is_empty() {
        return Err(ImportError::NoDataRows {
            inspected_lines: plage_donnees.rows().count(),
        });
    }

    // La feuille de résumé est cherchée parmi **toutes** les autres, pas
    // seulement celle qui précède : rien ne garantit son rang.
    let resume = feuilles
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != index_donnees)
        .map(|(_, (nom, plage))| (nom.clone(), extraire_paires(plage)))
        .find(|(_, paires)| !paires.is_empty());

    let (summary_sheet, paires) = match resume {
        Some((nom, paires)) => (Some(nom), paires),
        None => (None, Vec::new()),
    };

    let anomalies = detect_jumps_raw(&measurements, thresholds);

    Ok(ElitechXlsxExport {
        metadata: metadonnees(paires),
        data_sheet: nom_donnees.clone(),
        summary_sheet,
        sheet_names,
        header_labels: colonnes,
        date_order: DateOrderEvidence::iso(measurements.len()),
        total_rows: plage_donnees.rows().count(),
        measurements,
        warnings,
        anomalies,
    })
}

// ---------------------------------------------------------------------------
// Lecture des cellules
// ---------------------------------------------------------------------------

/// Contenu textuel d'une cellule, quel que soit son type de stockage.
///
/// L'export réel n'utilise que `inlineStr`, donc `Data::String`. Les autres
/// branches ne coûtent rien et couvrent un futur export qui écrirait des types
/// natifs : sans elles, un tel fichier se lirait comme entièrement vide.
fn texte(cell: &Data) -> String {
    match cell {
        Data::String(s) => s.trim().to_string(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => f.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.trim().to_string(),
        // `NaiveDateTime::to_string()` rend « 2026-05-01 14:07:31 », exactement le
        // format qu'attend `parse_horodatage` : la conversion boucle proprement.
        Data::DateTime(d) => d.as_datetime().map(|dt| dt.to_string()).unwrap_or_default(),
        Data::Error(_) | Data::Empty => String::new(),
    }
}

/// L'en-tête du tableau de données et la position de ses colonnes utiles.
struct Tableau {
    /// Index (0-based) de la ligne d'en-tête dans la plage.
    ligne_entete: usize,
    colonnes: Vec<String>,
    timestamp_col: usize,
    temperature_col: usize,
    humidity_col: Option<usize>,
}

/// Cherche le tableau de mesures dans une feuille.
///
/// Trois conditions, dont la troisième est celle qui compte :
///
/// 1. une ligne portant **à la fois** un intitulé d'horodatage et un intitulé de
///    température ;
/// 2. la colonne `No.` est ignorée, l'humidité repérée si elle existe ;
/// 3. **au moins une ligne suivante contient un horodatage réellement lisible**
///    dans la colonne désignée.
///
/// Sans la condition 3, la feuille de résumé passerait pour un tableau de
/// données : elle contient des cellules comme `« 34,1°C »` (qui ressemble à un
/// intitulé de température) à côté de `« Premier enregistrement »`. Exiger une
/// vraie date en dessous tranche sans avoir à connaître le nom des feuilles —
/// donc sans dépendre de la langue de l'export.
fn trouver_tableau(plage: &Range<Data>) -> Option<Tableau> {
    for (index, ligne) in plage.rows().enumerate().take(80) {
        let colonnes: Vec<String> = ligne.iter().map(texte).collect();
        if colonnes.len() < 2 {
            continue;
        }
        let Some(temperature_col) = colonnes.iter().position(|c| looks_like_temperature_header(c))
        else {
            continue;
        };
        let Some(timestamp_col) = colonnes.iter().position(|c| looks_like_timestamp_header(c))
        else {
            continue;
        };

        let prouve = plage
            .rows()
            .skip(index + 1)
            .take(20)
            .filter_map(|l| l.get(timestamp_col))
            .any(|c| parse_horodatage(&texte(c)).is_some());
        if !prouve {
            continue;
        }

        return Some(Tableau {
            ligne_entete: index,
            humidity_col: colonnes
                .iter()
                .position(|c| crate::parsing::looks_like_humidity_header(c)),
            colonnes,
            timestamp_col,
            temperature_col,
        });
    }
    None
}

fn lire_lignes(
    plage: &Range<Data>,
    ligne_entete: usize,
    timestamp_col: usize,
    temperature_col: usize,
    humidity_col: Option<usize>,
) -> (Vec<RawMeasurement>, Vec<ImportWarning>) {
    let mut measurements = Vec::new();
    let mut warnings = Vec::new();
    let necessaire = timestamp_col
        .max(temperature_col)
        .max(humidity_col.unwrap_or(0));

    for (index, ligne) in plage.rows().enumerate().skip(ligne_entete + 1) {
        // Numéro affiché à l'utilisateur : la ligne telle qu'il la verrait en
        // ouvrant le classeur dans un tableur, donc 1-based.
        let numero = index + 1;

        let cellules: Vec<String> = ligne.iter().map(texte).collect();
        if cellules.iter().all(|c| c.is_empty()) {
            continue; // ligne vide de fin de feuille
        }
        if cellules.len() <= necessaire {
            warnings.push(ImportWarning::new(
                numero,
                &cellules.join(" | "),
                format!(
                    "{} colonne(s) trouvée(s), au moins {} attendue(s)",
                    cellules.len(),
                    necessaire + 1
                ),
            ));
            continue;
        }

        let Some(raw_timestamp) = parse_horodatage(&cellules[timestamp_col]) else {
            warnings.push(ImportWarning::new(
                numero,
                &cellules.join(" | "),
                format!(
                    "horodatage illisible (« {} », attendu « AAAA-MM-JJ HH:MM:SS »)",
                    cellules[timestamp_col]
                ),
            ));
            continue;
        };

        let Some(temperature_celsius) = parse_number(&cellules[temperature_col]) else {
            warnings.push(ImportWarning::new(
                numero,
                &cellules.join(" | "),
                format!(
                    "température illisible (« {} »)",
                    cellules[temperature_col]
                ),
            ));
            continue;
        };

        measurements.push(RawMeasurement {
            raw_timestamp,
            temperature_celsius,
            humidity_percent: humidity_col
                .and_then(|i| cellules.get(i))
                .and_then(|c| parse_number(c)),
            source_line: numero,
        });
    }

    (measurements, warnings)
}

/// `"2026-05-01 14:07:31"` → instant naïf.
///
/// Le format est **imposé**, pas deviné : c'est celui qu'ElitechLog écrit, et
/// accepter des variantes libres rouvrirait par la fenêtre l'ambiguïté jour/mois
/// que ce format élimine. Seules deux tolérances : le `T` de l'ISO 8601, et
/// l'absence de secondes.
pub fn parse_horodatage(raw: &str) -> Option<NaiveDateTime> {
    let nettoye = raw.trim().replace('T', " ");
    NaiveDateTime::parse_from_str(&nettoye, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(&nettoye, "%Y-%m-%d %H:%M"))
        .ok()
}

// ---------------------------------------------------------------------------
// Feuille de résumé
// ---------------------------------------------------------------------------

/// Paires clé/valeur de la feuille de résumé.
///
/// La feuille est un tableau à **deux colonnes de paires** : `A`/`B` d'un côté,
/// `D`/`E` de l'autre, la colonne `C` servant de gouttière. On ne code donc pas
/// « la valeur est en colonne B » ; on prend, pour chaque cellule non vide, la
/// **première cellule non vide à sa droite**, et on garde la paire si la clé
/// ressemble à un libellé (elle contient une lettre) et la valeur à autre chose
/// qu'un titre de section.
fn extraire_paires(plage: &Range<Data>) -> Vec<(String, String)> {
    let mut paires = Vec::new();

    for ligne in plage.rows() {
        let cellules: Vec<String> = ligne.iter().map(texte).collect();
        let mut i = 0;
        while i < cellules.len() {
            let cle = cellules[i].trim();
            if cle.is_empty() || !cle.chars().any(|c| c.is_alphabetic()) {
                i += 1;
                continue;
            }
            match cellules
                .iter()
                .enumerate()
                .skip(i + 1)
                .find(|(_, v)| !v.trim().is_empty())
            {
                Some((j, valeur)) => {
                    paires.push((cle.to_string(), valeur.trim().to_string()));
                    // On repart **après** la valeur : sans cela, « Modèle | RC-5 »
                    // produirait aussi la paire absurde (« RC-5 », « Type de capteur »).
                    i = j + 1;
                }
                None => break,
            }
        }
    }

    paires
}

/// Minuscules, sans accents ni espaces superflus — pour comparer des clés.
fn normaliser(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'à' | 'â' | 'ä' => 'a',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            autre => autre,
        })
        .collect()
}

fn metadonnees(paires: Vec<(String, String)>) -> XlsxMetadata {
    // Les clés sont balayées par ordre de spécificité, et non les paires par
    // ordre d'apparition : `S/N` doit gagner contre une clé plus vague, où
    // qu'elle se trouve dans la feuille.
    let chercher = |cles: &[&str]| -> Option<String> {
        for cle in cles {
            for (k, v) in &paires {
                let k = normaliser(k);
                if (k == *cle || k.contains(cle)) && !v.trim().is_empty() && v.trim() != "N/A" {
                    return Some(v.trim().to_string());
                }
            }
        }
        None
    };

    XlsxMetadata {
        logger_serial: chercher(&SERIAL_KEYS),
        sample_interval_seconds: chercher(&INTERVAL_KEYS)
            .as_deref()
            .and_then(parse_interval_seconds),
        sensor_type: chercher(&SENSOR_KEYS),
        device_model: chercher(&MODEL_KEYS),
        summary_pairs: paires,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    /// Fabrique un classeur xlsx minimal reproduisant **exactement** les
    /// conventions du fichier réel : chaînes inline, virgule décimale, feuilles
    /// nommées en français, résumé en deux colonnes de paires.
    ///
    /// Écrire ce classeur à la main plutôt que d'en committer un binaire garde
    /// les hypothèses visibles : si demain le parseur cesse de gérer les chaînes
    /// inline, ce test le dit sans qu'il faille ouvrir un fichier opaque.
    fn classeur_test(lignes_donnees: &str) -> Vec<u8> {
        fn zip_entry(nom: &str, contenu: &str, sortie: &mut Vec<u8>, entrees: &mut Vec<(String, u32, u32, usize)>) {
            let data = contenu.as_bytes();
            let crc = crc32(data);
            let offset = sortie.len();
            sortie.extend_from_slice(&ZIP_MAGIC);
            sortie.extend_from_slice(&[0x0A, 0x00]); // version
            sortie.extend_from_slice(&[0x00, 0x00]); // flags
            sortie.extend_from_slice(&[0x00, 0x00]); // stored
            sortie.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // date/heure
            sortie.extend_from_slice(&crc.to_le_bytes());
            sortie.extend_from_slice(&(data.len() as u32).to_le_bytes());
            sortie.extend_from_slice(&(data.len() as u32).to_le_bytes());
            sortie.extend_from_slice(&(nom.len() as u16).to_le_bytes());
            sortie.extend_from_slice(&[0x00, 0x00]); // extra
            sortie.extend_from_slice(nom.as_bytes());
            sortie.extend_from_slice(data);
            entrees.push((nom.to_string(), crc, data.len() as u32, offset));
        }

        fn crc32(data: &[u8]) -> u32 {
            let mut crc = 0xFFFF_FFFFu32;
            for octet in data {
                crc ^= *octet as u32;
                for _ in 0..8 {
                    let masque = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xEDB8_8320 & masque);
                }
            }
            !crc
        }

        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Résumé" sheetId="1" r:id="rId1"/><sheet name="Tableau de données" sheetId="2" r:id="rId2"/></sheets></workbook>"#;
        let wb_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#;

        // Feuille de résumé : deux colonnes de paires, colonne C en gouttière.
        let sheet1 = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Rapport de données</t></is></c></row><row r="5"><c r="A5" t="inlineStr"><is><t>Modèle</t></is></c><c r="B5" t="inlineStr"><is><t>RC-5</t></is></c><c r="C5" t="inlineStr"><is><t></t></is></c><c r="D5" t="inlineStr"><is><t>Type de capteur</t></is></c><c r="E5" t="inlineStr"><is><t>Température</t></is></c></row><row r="6"><c r="A6" t="inlineStr"><is><t>S/N</t></is></c><c r="B6" t="inlineStr"><is><t>EFE205004333</t></is></c><c r="C6" t="inlineStr"><is><t></t></is></c><c r="D6" t="inlineStr"><is><t>Version du firmware</t></is></c><c r="E6" t="inlineStr"><is><t>V1,2</t></is></c></row><row r="10"><c r="A10" t="inlineStr"><is><t>Bouton d'arrêt</t></is></c><c r="B10" t="inlineStr"><is><t>Desactivé</t></is></c><c r="C10" t="inlineStr"><is><t></t></is></c><c r="D10" t="inlineStr"><is><t>Intervalle d'enregistrement</t></is></c><c r="E10" t="inlineStr"><is><t>15m</t></is></c></row></sheetData></worksheet>"#;

        let sheet2 = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>No.</t></is></c><c r="B1" t="inlineStr"><is><t>Temps</t></is></c><c r="C1" t="inlineStr"><is><t>Température°C</t></is></c></row>{lignes_donnees}</sheetData></worksheet>"#
        );

        let mut sortie = Vec::new();
        let mut entrees = Vec::new();
        zip_entry("[Content_Types].xml", content_types, &mut sortie, &mut entrees);
        zip_entry("_rels/.rels", rels, &mut sortie, &mut entrees);
        zip_entry("xl/workbook.xml", workbook, &mut sortie, &mut entrees);
        zip_entry("xl/_rels/workbook.xml.rels", wb_rels, &mut sortie, &mut entrees);
        zip_entry("xl/worksheets/sheet1.xml", sheet1, &mut sortie, &mut entrees);
        zip_entry("xl/worksheets/sheet2.xml", &sheet2, &mut sortie, &mut entrees);

        let debut_central = sortie.len();
        for (nom, crc, taille, offset) in &entrees {
            sortie.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
            sortie.extend_from_slice(&[0x14, 0x00]); // version made by
            sortie.extend_from_slice(&[0x0A, 0x00]); // version needed
            sortie.extend_from_slice(&[0x00, 0x00]); // flags
            sortie.extend_from_slice(&[0x00, 0x00]); // stored
            sortie.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
            sortie.extend_from_slice(&crc.to_le_bytes());
            sortie.extend_from_slice(&taille.to_le_bytes());
            sortie.extend_from_slice(&taille.to_le_bytes());
            sortie.extend_from_slice(&(nom.len() as u16).to_le_bytes());
            sortie.extend_from_slice(&[0x00, 0x00]); // extra
            sortie.extend_from_slice(&[0x00, 0x00]); // commentaire
            sortie.extend_from_slice(&[0x00, 0x00]); // disque
            sortie.extend_from_slice(&[0x00, 0x00]); // attributs internes
            sortie.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // attributs externes
            sortie.extend_from_slice(&(*offset as u32).to_le_bytes());
            sortie.extend_from_slice(nom.as_bytes());
        }
        let taille_centrale = sortie.len() - debut_central;

        sortie.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
        sortie.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        sortie.extend_from_slice(&(entrees.len() as u16).to_le_bytes());
        sortie.extend_from_slice(&(entrees.len() as u16).to_le_bytes());
        sortie.extend_from_slice(&(taille_centrale as u32).to_le_bytes());
        sortie.extend_from_slice(&(debut_central as u32).to_le_bytes());
        sortie.extend_from_slice(&[0x00, 0x00]);
        sortie
    }

    fn ligne(numero: usize, temps: &str, temperature: &str) -> String {
        let r = numero + 1;
        format!(
            r#"<row r="{r}"><c r="A{r}" t="inlineStr"><is><t>{numero}</t></is></c><c r="B{r}" t="inlineStr"><is><t>{temps}</t></is></c><c r="C{r}" t="inlineStr"><is><t>{temperature}</t></is></c></row>"#
        )
    }

    fn export_type() -> ElitechXlsxExport {
        let lignes = ligne(1, "2026-05-01 14:07:31", "22,2")
            + &ligne(2, "2026-05-01 14:22:31", "22,4")
            + &ligne(3, "2026-05-01 14:37:31", "21,4");
        parse_elitech_xlsx(&classeur_test(&lignes)).expect("classeur de test lisible")
    }

    #[test]
    fn les_feuilles_sont_reperees_par_leur_contenu_pas_par_leur_nom() {
        let e = export_type();
        assert_eq!(e.data_sheet, "Tableau de données");
        assert_eq!(e.summary_sheet.as_deref(), Some("Résumé"));
        assert_eq!(e.header_labels, vec!["No.", "Temps", "Température°C"]);
    }

    #[test]
    fn la_virgule_decimale_est_lue_comme_un_separateur_decimal() {
        let e = export_type();
        assert_eq!(e.measurements.len(), 3);
        assert_eq!(e.measurements[0].temperature_celsius, 22.2);
        assert_eq!(e.measurements[1].temperature_celsius, 22.4);
        assert_eq!(e.measurements[2].temperature_celsius, 21.4);
    }

    #[test]
    fn l_horodatage_iso_est_lu_sans_ambiguite_jour_mois() {
        let e = export_type();
        assert_eq!(
            e.measurements[0].raw_timestamp,
            NaiveDate::from_ymd_opt(2026, 5, 1)
                .unwrap()
                .and_hms_opt(14, 7, 31)
                .unwrap(),
            "le 01/05 doit rester le 1er mai, pas le 5 janvier"
        );

        // Le scanner d'ordre jour/mois est court-circuité : aucune preuve
        // ambiguë, aucun doute, donc aucun bandeau « à confirmer » côté UI.
        assert_eq!(e.date_order.iso_hits, 3);
        assert_eq!(e.date_order.ambiguous_hits, 0);
        assert!(!e.date_order.uncertain);
        assert!(!e.date_order.conflicting);
    }

    #[test]
    fn les_metadonnees_de_la_feuille_de_resume_sont_lues() {
        let m = export_type().metadata;
        assert_eq!(m.logger_serial.as_deref(), Some("EFE205004333"));
        assert_eq!(m.sample_interval_seconds, Some(900), "« 15m » = 900 s");
        assert_eq!(m.sensor_type.as_deref(), Some("Température"));
        assert_eq!(m.device_model.as_deref(), Some("RC-5"));
    }

    #[test]
    fn un_saut_brutal_est_signale_sans_ecarter_la_mesure() {
        let lignes = ligne(1, "2026-08-16 22:07:31", "21,0")
            + &ligne(2, "2026-08-16 22:22:31", "21,2")
            + &ligne(3, "2026-08-16 22:37:31", "25,0");
        let e = parse_elitech_xlsx(&classeur_test(&lignes)).unwrap();

        assert_eq!(e.measurements.len(), 3, "aucune mesure n'est écartée");
        assert_eq!(e.anomalies.len(), 1);
        assert!((e.anomalies[0].delta_c - 3.8).abs() < 1e-9);
        assert_eq!(e.anomalies[0].source_line, 4);
    }

    #[test]
    fn une_ligne_illisible_est_ecartee_avec_son_motif() {
        let lignes = ligne(1, "2026-05-01 14:07:31", "22,2")
            + &ligne(2, "pas une date", "22,4")
            + &ligne(3, "2026-05-01 14:37:31", "HS")
            + &ligne(4, "2026-05-01 14:52:31", "21,2");
        let e = parse_elitech_xlsx(&classeur_test(&lignes)).unwrap();

        assert_eq!(e.measurements.len(), 2);
        assert_eq!(e.warnings.len(), 2);
        assert!(e.warnings[0].reason.contains("horodatage"));
        assert!(e.warnings[1].reason.contains("température"));
    }

    #[test]
    fn un_fichier_qui_n_est_pas_un_zip_est_refuse_avec_un_message_utile() {
        let erreur = parse_elitech_xlsx(b"ceci n'est pas un classeur").unwrap_err();
        let texte = erreur.to_string();
        assert!(texte.contains(".esf"), "le message doit citer les formats non lus : {texte}");
        assert!(texte.contains("zip"));
    }

    #[test]
    fn un_fichier_vide_est_refuse() {
        assert!(matches!(
            parse_elitech_xlsx(&[]),
            Err(ImportError::EmptyFile)
        ));
    }

    #[test]
    fn la_signature_zip_est_reconnue() {
        assert!(looks_like_xlsx(&[0x50, 0x4B, 0x03, 0x04, 0x00]));
        assert!(!looks_like_xlsx(b"No.\tTime\tTemperature"));
        assert!(!looks_like_xlsx(&[0x50, 0x4B]));
    }
}

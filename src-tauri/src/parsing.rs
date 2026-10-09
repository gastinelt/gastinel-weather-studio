//! Briques de parsing partagées entre l'import Elitech (auto-détecté) et
//! l'import manuel (colonnes mappées à la main).
//!
//! Tout ce qui est « deviné » ici est isolé dans ce fichier pour qu'un ajustement
//! au format réel d'ElitechLog reste local (cf. `elitech_import.rs`).

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};

/// Séparateurs candidats, **par ordre de préférence** en cas d'égalité de score.
///
/// La tabulation d'abord : ElitechLog exporte du TXT, et un TXT tabulé est
/// l'hypothèse la plus probable. La virgule en dernier parce qu'elle entre en
/// conflit avec la virgule décimale d'un export en locale française.
pub const CANDIDATE_DELIMITERS: [char; 4] = ['\t', ';', ',', '|'];

/// Repli historique : quand **rien** dans le fichier ne permet de trancher, on
/// lit `05/01/2024` comme le 5 janvier (format européen).
///
/// ⚠️ Cette constante n'est plus le juge de paix qu'elle était. L'ordre jour/mois
/// est désormais **détecté fichier par fichier** ([`DateOrderScanner`]) ; elle ne
/// sert que lorsque le fichier ne fournit aucune preuve — et dans ce cas la série
/// est marquée « format incertain », donc corrigeable après coup sans réimport.
/// Voir la section « Ordre jour/mois » du README.
pub const AMBIGUOUS_DATE_IS_DAY_FIRST: bool = true;

// ---------------------------------------------------------------------------
// Ordre jour/mois
// ---------------------------------------------------------------------------

/// Ordre des composantes d'une date numérique, appliqué à un **fichier entier**.
///
/// Jamais décidé ligne par ligne : un fichier mélangeant deux ordres serait
/// indéchiffrable, et deviner au cas par cas décalerait silencieusement une
/// partie des mesures. On tranche une fois, sur l'ensemble des preuves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DateOrder {
    /// `dd/MM/yyyy` — convention européenne.
    DayFirst,
    /// `MM/dd/yyyy` — convention nord-américaine.
    MonthFirst,
}

impl DateOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            DateOrder::DayFirst => "DayFirst",
            DateOrder::MonthFirst => "MonthFirst",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "DayFirst" => Some(DateOrder::DayFirst),
            "MonthFirst" => Some(DateOrder::MonthFirst),
            _ => None,
        }
    }

    pub fn inverse(self) -> Self {
        match self {
            DateOrder::DayFirst => DateOrder::MonthFirst,
            DateOrder::MonthFirst => DateOrder::DayFirst,
        }
    }

    /// Libellé destiné à l'UI (aucune logique ne doit en dépendre).
    pub fn label_fr(self) -> &'static str {
        match self {
            DateOrder::DayFirst => "jour d'abord (jj/mm)",
            DateOrder::MonthFirst => "mois d'abord (mm/jj)",
        }
    }
}

/// Ordre retenu quand le fichier ne prouve rien. Dérivé de la constante
/// historique pour qu'il n'existe qu'un seul endroit à basculer.
pub const DEFAULT_DATE_ORDER: DateOrder = if AMBIGUOUS_DATE_IS_DAY_FIRST {
    DateOrder::DayFirst
} else {
    DateOrder::MonthFirst
};

/// Ce qu'un jeton de date prouve **à lui seul**, indépendamment du reste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateTokenKind {
    /// `2024-01-05` : année en tête, aucune ambiguïté possible.
    Iso,
    /// Le premier champ dépasse 12 : ce ne peut être qu'un jour.
    PreuveJourDAbord,
    /// Le second champ dépasse 12 : ce ne peut être qu'un jour, donc mois en tête.
    PreuveMoisDAbord,
    /// Les deux champs sont ≤ 12 : le jeton ne prouve rien.
    Ambigu,
    /// Pas une date exploitable.
    Illisible,
}

/// Isole la partie date d'un horodatage complet : `05/01/2024 07:30` → `05/01/2024`.
///
/// Renvoie la chaîne telle quelle si elle ne contient pas d'heure.
pub fn date_token(raw: &str) -> &str {
    let s = raw.trim();
    let s = s.split(['T', ' ']).next().unwrap_or(s);
    s.trim()
}

/// Classe un jeton de date sans jamais le convertir.
pub fn classify_date_token(raw: &str) -> DateTokenKind {
    let s = date_token(raw);
    if s.is_empty() {
        return DateTokenKind::Illisible;
    }
    let Some(sep) = ['-', '/', '.'].into_iter().find(|c| s.contains(*c)) else {
        return DateTokenKind::Illisible;
    };
    let parts: Vec<&str> = s.split(sep).collect();
    if parts.len() != 3 {
        return DateTokenKind::Illisible;
    }
    // Année en tête : le format est auto-descriptif.
    if parts[0].trim().len() == 4 {
        return match (
            parts[0].trim().parse::<i32>(),
            parts[1].trim().parse::<u32>(),
            parts[2].trim().parse::<u32>(),
        ) {
            (Ok(_), Ok(_), Ok(_)) => DateTokenKind::Iso,
            _ => DateTokenKind::Illisible,
        };
    }
    let (Ok(a), Ok(b)) = (parts[0].trim().parse::<u32>(), parts[1].trim().parse::<u32>()) else {
        return DateTokenKind::Illisible;
    };
    if parts[2].trim().parse::<i64>().is_err() {
        return DateTokenKind::Illisible;
    }
    match (a > 12, b > 12) {
        // « 13/14/2024 » : aucun ordre ne rend la date valide.
        (true, true) => DateTokenKind::Illisible,
        (true, false) => DateTokenKind::PreuveJourDAbord,
        (false, true) => DateTokenKind::PreuveMoisDAbord,
        (false, false) => DateTokenKind::Ambigu,
    }
}

/// Bilan de la détection d'ordre sur un fichier entier.
///
/// Sérialisé jusqu'à l'UI : c'est ce qui permet d'expliquer à l'utilisateur
/// *pourquoi* une série est marquée douteuse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DateOrderEvidence {
    /// Ordre effectivement appliqué au parsing.
    pub order: DateOrder,
    pub day_first_hits: usize,
    pub month_first_hits: usize,
    pub ambiguous_hits: usize,
    pub iso_hits: usize,
    /// `true` : le fichier ne prouve pas son ordre alors qu'il contient des dates
    /// ambiguës. `order` est un **repli**, il peut être faux.
    pub uncertain: bool,
    /// `true` : le fichier contient des preuves dans les deux sens (§ README).
    pub conflicting: bool,
    /// Ligne de la première preuve qui contredit l'ordre majoritaire.
    pub first_conflict_line: Option<usize>,
    /// Jusqu'à 3 jetons observés, pour que l'UI puisse montrer un exemple concret.
    pub samples: Vec<String>,
}

impl DateOrderEvidence {
    /// Preuve d'un format **ISO** (`AAAA-MM-JJ`), donc sans la moindre ambiguïté
    /// jour/mois : `count` horodatages lus, aucune décision à prendre.
    ///
    /// C'est le court-circuit du [`DateOrderScanner`] pour toutes les sources qui
    /// datent en ISO — et elles sont aujourd'hui majoritaires :
    ///
    /// | Source | Format d'horodatage | Scanner utile ? |
    /// |---|---|---|
    /// | Infoclimat (JSON et CSV) | `dh_utc` = `2026-06-24 20:10:00` | non |
    /// | Elitech, export xlsx réel | `Temps` = `2026-05-01 14:07:31` | non |
    /// | ROMMA `archives_*.csv` | epoch Unix entier | non |
    /// | Elitech, formats tabulaires hypothétiques | `05/01/2024` | **oui** |
    ///
    /// Autrement dit : le mécanisme de détection d'ordre jour/mois ne sert plus
    /// qu'à d'éventuels exports Elitech au format tabulaire, dont nous n'avons
    /// **aucun exemplaire réel**. Il est conservé pour eux (et pour l'import
    /// manuel), pas retiré — mais aucun fichier réel connu n'en dépend.
    pub fn iso(count: usize) -> Self {
        DateOrderEvidence {
            iso_hits: count,
            ..DateOrderEvidence::vide()
        }
    }

    /// Aucune date rencontrée : rien à décider, rien à signaler.
    pub fn vide() -> Self {
        DateOrderEvidence {
            order: DEFAULT_DATE_ORDER,
            day_first_hits: 0,
            month_first_hits: 0,
            ambiguous_hits: 0,
            iso_hits: 0,
            uncertain: false,
            conflicting: false,
            first_conflict_line: None,
            samples: Vec::new(),
        }
    }

    /// `true` si au moins une preuve a été trouvée, dans un sens ou dans l'autre.
    pub fn prouve(&self) -> bool {
        self.day_first_hits > 0 || self.month_first_hits > 0
    }
}

/// Accumulateur : on lui présente le jeton de date de chaque ligne, il tranche
/// à la fin. Voir [`DateOrderScanner::finish`] pour les règles de décision.
#[derive(Debug, Default)]
pub struct DateOrderScanner {
    day_first_hits: usize,
    month_first_hits: usize,
    ambiguous_hits: usize,
    iso_hits: usize,
    first_day_first_line: Option<usize>,
    first_month_first_line: Option<usize>,
    samples: Vec<String>,
}

impl DateOrderScanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, line_number: usize, raw: &str) {
        match classify_date_token(raw) {
            DateTokenKind::Iso => self.iso_hits += 1,
            DateTokenKind::Ambigu => self.ambiguous_hits += 1,
            DateTokenKind::Illisible => {}
            DateTokenKind::PreuveJourDAbord => {
                self.day_first_hits += 1;
                self.first_day_first_line.get_or_insert(line_number);
                self.pousser_exemple(raw);
            }
            DateTokenKind::PreuveMoisDAbord => {
                self.month_first_hits += 1;
                self.first_month_first_line.get_or_insert(line_number);
                self.pousser_exemple(raw);
            }
        }
    }

    fn pousser_exemple(&mut self, raw: &str) {
        if self.samples.len() < 3 {
            let jeton = date_token(raw).to_string();
            if !self.samples.contains(&jeton) {
                self.samples.push(jeton);
            }
        }
    }

    /// Règles de décision — **l'endroit à lire** pour comprendre le comportement :
    ///
    /// | Situation | `order` | `uncertain` |
    /// |---|---|---|
    /// | Preuves dans un seul sens | le sens prouvé | `false` |
    /// | Preuves contradictoires | la majorité (égalité → repli) | `true` + `conflicting` |
    /// | Aucune preuve, ≥ 1 date ambiguë | repli | **`true`** |
    /// | Que de l'ISO, ou aucune date | repli (sans effet) | **`false`** |
    ///
    /// Le dernier cas est le piège à ne pas manquer : un fichier entièrement en
    /// dates ISO n'est **pas** incertain — l'ordre jour/mois n'a aucune prise sur
    /// lui. Ne lever le drapeau que s'il existe une ambiguïté *réelle*.
    pub fn finish(self) -> DateOrderEvidence {
        let conflicting = self.day_first_hits > 0 && self.month_first_hits > 0;

        let order = if self.day_first_hits > self.month_first_hits {
            DateOrder::DayFirst
        } else if self.month_first_hits > self.day_first_hits {
            DateOrder::MonthFirst
        } else {
            // Zéro preuve, ou autant dans les deux sens : on ne sait pas.
            DEFAULT_DATE_ORDER
        };

        // Le doute ne porte que sur les dates réellement ambiguës. Sans preuve et
        // sans ambiguïté (fichier ISO), il n'y a rien à confirmer.
        let sans_preuve = self.day_first_hits == 0 && self.month_first_hits == 0;
        let uncertain = conflicting || (sans_preuve && self.ambiguous_hits > 0);

        // La ligne du conflit est celle de la preuve *minoritaire* : c'est elle
        // qui contredit l'ordre appliqué, donc celle qu'il faut aller regarder.
        let first_conflict_line = if conflicting {
            match order {
                DateOrder::DayFirst => self.first_month_first_line,
                DateOrder::MonthFirst => self.first_day_first_line,
            }
        } else {
            None
        };

        DateOrderEvidence {
            order,
            day_first_hits: self.day_first_hits,
            month_first_hits: self.month_first_hits,
            ambiguous_hits: self.ambiguous_hits,
            iso_hits: self.iso_hits,
            uncertain,
            conflicting,
            first_conflict_line,
            samples: self.samples,
        }
    }
}

/// Permute jour et mois. `None` si le jour dépasse 12 : la permutation n'y est
/// pas définie, et fabriquer une date approchante serait pire que refuser.
///
/// Toujours définie sur une série marquée « incertaine » : si elle l'est, c'est
/// justement qu'aucune de ses dates n'avait de champ > 12.
pub fn swap_day_month(date: NaiveDate) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(date.year(), date.day(), date.month())
}

/// Variante horodatée : la partie heure est conservée telle quelle.
pub fn swap_day_month_datetime(dt: NaiveDateTime) -> Option<NaiveDateTime> {
    Some(swap_day_month(dt.date())?.and_time(dt.time()))
}

/// Retire le BOM et normalise les fins de ligne.
pub fn normalize_content(content: &str) -> String {
    content
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

/// Découpe une ligne et nettoie chaque champ (espaces, guillemets simples/doubles).
pub fn split_line(line: &str, delimiter: char) -> Vec<String> {
    line.split(delimiter).map(clean_field).collect()
}

fn clean_field(field: &str) -> String {
    let f = field.trim();
    let f = f
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(f);
    f.trim().to_string()
}

/// Devine le séparateur : on garde celui qui découpe le plus de lignes en un
/// nombre de colonnes constant (≥ 2). En cas d'égalité, l'ordre de
/// [`CANDIDATE_DELIMITERS`] tranche.
pub fn detect_delimiter(lines: &[&str]) -> char {
    let sample: Vec<&&str> = lines.iter().take(200).collect();
    let mut best = (CANDIDATE_DELIMITERS[0], 0usize);

    for delimiter in CANDIDATE_DELIMITERS {
        // Nombre de colonnes le plus fréquent pour ce séparateur.
        let mut counts: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        for line in &sample {
            let n = line.split(delimiter).count();
            if n >= 2 {
                *counts.entry(n).or_insert(0) += 1;
            }
        }
        let score = counts.values().copied().max().unwrap_or(0);
        if score > best.1 {
            best = (delimiter, score);
        }
    }

    best.0
}

/// Parse un nombre en tolérant les unités et la virgule décimale.
///
/// Accepte : `-12.5`, `-12,5`, `+3.2`, `-12.5 °C`, `12.5℃`, `56 %RH`, `1 234,5`.
/// Renvoie `None` pour les marqueurs de valeur absente (`""`, `--`, `N/A`, `NULL`).
pub fn parse_number(raw: &str) -> Option<f64> {
    // Chemin rapide, sans allocation : le cas de loin le plus courant (`12.5`,
    // `-3`, `1021.0`). Un texte que `f64::from_str` accepte ne contient ni espace,
    // ni virgule, ni unité finale : le chemin complet ci-dessous le rendrait tel
    // quel — au `+` près d'un exposant (`1e+5`), dont le retrait ne change pas la
    // valeur. Seul un résultat fini est pris ici ; `inf` / `NaN` suivent le chemin
    // complet, qui les refuse. Appelée ~8 fois par ligne d'une archive ROMMA de
    // 477 000 lignes, l'ancienne version allouait une demi-douzaine de `String`
    // par appel.
    if let Ok(v) = raw.trim().parse::<f64>() {
        if v.is_finite() {
            return Some(v);
        }
    }

    let mut s: String = raw.trim().to_string();
    if s.is_empty() {
        return None;
    }

    // Unités et symboles courants.
    for unit in [
        "°C", "℃", "°F", "℉", "%RH", "%rh", "%Rh", "RH", "rh", "%", "C", "F", "°",
    ] {
        if s.ends_with(unit) {
            s = s[..s.len() - unit.len()].trim().to_string();
        }
    }

    // Espaces (y compris insécables) utilisés comme séparateur de milliers.
    s = s.replace('\u{a0}', "").replace('\u{202f}', "").replace(' ', "");

    let upper = s.to_ascii_uppercase();
    if s.is_empty()
        || matches!(
            upper.as_str(),
            "-" | "--" | "---" | "N/A" | "NA" | "NULL" | "NONE" | "?" | "ERR" | "ERROR"
        )
    {
        return None;
    }

    // « 1,234.5 » → virgule = séparateur de milliers ; « 12,5 » → virgule décimale.
    if s.contains('.') && s.contains(',') {
        s = s.replace(',', "");
    } else {
        s = s.replace(',', ".");
    }
    s = s.replace('+', "");

    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse une date isolée : `2024-01-05`, `2024/01/05`, `05/01/2024`, `05.01.2024`,
/// `5/1/24`.
///
/// Utilise l'ordre de repli. Les chemins d'import passent par
/// [`parse_date_with_order`] avec l'ordre détecté sur le fichier.
pub fn parse_date(raw: &str) -> Option<NaiveDate> {
    parse_date_with_order(raw, DEFAULT_DATE_ORDER)
}

/// Parse une date isolée en imposant l'ordre jour/mois.
///
/// `order` ne sert que d'arbitre : une date qui se prouve elle-même (un champ
/// > 12) ou une date ISO est lue correctement quel que soit l'ordre passé.
pub fn parse_date_with_order(raw: &str, order: DateOrder) -> Option<NaiveDate> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let sep = ['-', '/', '.'].into_iter().find(|c| s.contains(*c))?;
    let parts: Vec<&str> = s.split(sep).collect();
    if parts.len() != 3 {
        return None;
    }

    let a: i64 = parts[0].trim().parse().ok()?;
    let b: u32 = parts[1].trim().parse().ok()?;
    let c: i64 = parts[2].trim().parse().ok()?;

    // Année en tête (ISO) : le premier champ a 4 chiffres.
    if parts[0].trim().len() == 4 {
        return NaiveDate::from_ymd_opt(a as i32, b, c as u32);
    }

    let year = expand_year(c);
    let (day, month) = if a > 12 {
        // La date se prouve elle-même : l'ordre imposé ne s'applique pas.
        (a as u32, b)
    } else if b > 12 {
        (b, a as u32)
    } else {
        match order {
            DateOrder::DayFirst => (a as u32, b),
            DateOrder::MonthFirst => (b, a as u32),
        }
    };

    NaiveDate::from_ymd_opt(year, month, day)
}

fn expand_year(y: i64) -> i32 {
    match y {
        0..=69 => (2000 + y) as i32,
        70..=99 => (1900 + y) as i32,
        other => other as i32,
    }
}

/// Parse une heure : `07:30`, `07:30:15`, `7:30:15 AM`, `07:30:15.250`.
pub fn parse_time(raw: &str) -> Option<NaiveTime> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }

    let upper = s.to_ascii_uppercase();
    let (body, pm, has_meridiem) = if let Some(rest) = upper.strip_suffix("AM") {
        (rest.trim().to_string(), false, true)
    } else if let Some(rest) = upper.strip_suffix("PM") {
        (rest.trim().to_string(), true, true)
    } else {
        (upper.clone(), false, false)
    };

    let parts: Vec<&str> = body.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }

    let mut hour: u32 = parts[0].trim().parse().ok()?;
    let minute: u32 = if parts.len() > 1 {
        parts[1].trim().parse().ok()?
    } else {
        0
    };
    // Les fractions de seconde sont tronquées : le RC-5 échantillonne à la minute.
    let second: u32 = if parts.len() > 2 {
        let sec = parts[2].trim();
        let sec = sec.split(['.', ',']).next().unwrap_or(sec);
        sec.parse().ok()?
    } else {
        0
    };

    if has_meridiem {
        if hour == 12 {
            hour = 0;
        }
        if pm {
            hour += 12;
        }
    }

    NaiveTime::from_hms_opt(hour, minute, second)
}

/// Parse un horodatage complet dans une seule cellule.
///
/// Gère `2024-01-05 07:30:00`, `2024-01-05T07:30:00`, `05/01/2024 07:30`,
/// `1/5/24 7:30:00 PM`, et la date seule (→ minuit).
pub fn parse_datetime_flexible(raw: &str) -> Option<NaiveDateTime> {
    parse_datetime_flexible_with_order(raw, DEFAULT_DATE_ORDER)
}

/// Idem, en imposant l'ordre jour/mois détecté sur le fichier.
pub fn parse_datetime_flexible_with_order(raw: &str, order: DateOrder) -> Option<NaiveDateTime> {
    let s = raw.trim().replace('T', " ");
    let s = s.trim_end_matches('Z').trim();
    if s.is_empty() {
        return None;
    }

    let mut tokens = s.split_whitespace();
    let premier = tokens.next()?;
    let rest: Vec<&str> = tokens.collect();

    let date = parse_date_with_order(premier, order)?;
    if rest.is_empty() {
        return Some(date.and_time(NaiveTime::from_hms_opt(0, 0, 0)?));
    }

    let time = parse_time(&rest.join(" "))?;
    Some(date.and_time(time))
}

/// Parse un horodatage réparti sur deux cellules (date puis heure).
pub fn parse_date_and_time(date_raw: &str, time_raw: &str) -> Option<NaiveDateTime> {
    parse_date_and_time_with_order(date_raw, time_raw, DEFAULT_DATE_ORDER)
}

/// Idem, en imposant l'ordre jour/mois détecté sur le fichier.
pub fn parse_date_and_time_with_order(
    date_raw: &str,
    time_raw: &str,
    order: DateOrder,
) -> Option<NaiveDateTime> {
    let date = parse_date_with_order(date_raw, order)?;
    let time = parse_time(time_raw)?;
    Some(date.and_time(time))
}

/// Parse une durée d'intervalle d'échantillonnage : `00:10:00`, `10 min`,
/// `600s`, `1 h`, `30`. Renvoie des secondes.
pub fn parse_interval_seconds(raw: &str) -> Option<i64> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }

    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        let nums: Option<Vec<i64>> = parts.iter().map(|p| p.trim().parse::<i64>().ok()).collect();
        let nums = nums?;
        return match nums.len() {
            3 => Some(nums[0] * 3600 + nums[1] * 60 + nums[2]),
            2 => Some(nums[0] * 60 + nums[1]),
            _ => None,
        };
    }

    let digits: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    let value: f64 = digits.replace(',', ".").parse().ok()?;
    let unit = s[digits.len()..].trim();

    let factor = match unit {
        "" | "s" | "sec" | "secs" | "second" | "seconds" | "seconde" | "secondes" => 1.0,
        "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
        "h" | "hr" | "hour" | "hours" | "heure" | "heures" => 3600.0,
        _ => return None,
    };

    Some((value * factor).round() as i64)
}

/// `true` si le libellé de colonne ressemble à un horodatage.
pub fn looks_like_timestamp_header(header: &str) -> bool {
    let h = header.to_lowercase();
    // « temps » est l'intitulé de la colonne d'horodatage de l'export xlsx réel
    // d'ElitechLog. Il ressemble dangereusement à « température » : voir la
    // garde symétrique dans `looks_like_temperature_header`.
    [
        "time", "date", "heure", "horodat", "timestamp", "datetime", "temps", "时间", "日期",
    ]
    .iter()
    .any(|k| h.contains(k))
}

/// `true` si le libellé de colonne ressemble à une température.
pub fn looks_like_temperature_header(header: &str) -> bool {
    let h = header.to_lowercase();
    if ["hum", "rh", "dew", "point de rosée"].iter().any(|k| h.contains(k)) {
        return false;
    }
    // **Piège du format Elitech réel.** Sa colonne d'horodatage s'intitule
    // « Temps », qui contient « temp » : sans cette garde, elle serait prise pour
    // la colonne de température, et le parseur lirait des dates comme des degrés.
    // « Température » ne commence pas par « temps » — la distinction tient.
    if h.trim_start().starts_with("temps") {
        return false;
    }
    ["temp", "°c", "℃", "temperature", "température", "温度", "t(", "t ("]
        .iter()
        .any(|k| h.contains(k))
        || h == "t"
}

/// `true` si le libellé de colonne ressemble à une humidité.
pub fn looks_like_humidity_header(header: &str) -> bool {
    let h = header.to_lowercase();
    ["hum", "rh", "%rh", "humidité", "humidity", "湿度"]
        .iter()
        .any(|k| h.contains(k))
}

/// `true` si le libellé désigne spécifiquement une colonne « date seule ».
pub fn looks_like_date_only_header(header: &str) -> bool {
    let h = header.to_lowercase();
    (h.contains("date") || h.contains("日期")) && !h.contains("time") && !h.contains("heure")
}

/// `true` si le libellé désigne spécifiquement une colonne « heure seule ».
pub fn looks_like_time_only_header(header: &str) -> bool {
    let h = header.to_lowercase();
    (h.contains("time") || h.contains("heure") || h.contains("时间")) && !h.contains("date")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_avec_unites_et_virgule() {
        assert_eq!(parse_number("-12.5"), Some(-12.5));
        assert_eq!(parse_number("-12,5"), Some(-12.5));
        assert_eq!(parse_number("+3.2"), Some(3.2));
        assert_eq!(parse_number("-34.6 °C"), Some(-34.6));
        assert_eq!(parse_number("21.3℃"), Some(21.3));
        assert_eq!(parse_number("56 %RH"), Some(56.0));
        assert_eq!(parse_number("1,234.5"), Some(1234.5));
        assert_eq!(parse_number("0"), Some(0.0));
    }

    #[test]
    fn nombres_absents() {
        assert_eq!(parse_number(""), None);
        assert_eq!(parse_number("  "), None);
        assert_eq!(parse_number("--"), None);
        assert_eq!(parse_number("N/A"), None);
        assert_eq!(parse_number("abc"), None);
    }

    /// Les bords du chemin rapide : ce que `f64::from_str` accepte seul doit
    /// donner la même chose qu'avant, et ce qu'il accepte mais qui n'est pas un
    /// nombre fini doit rester refusé.
    #[test]
    fn nombres_du_chemin_rapide() {
        assert_eq!(parse_number(" 12.5 "), Some(12.5));
        assert_eq!(parse_number("1021.0"), Some(1021.0));
        assert_eq!(parse_number("1e+5"), Some(100_000.0));
        assert_eq!(parse_number("1E5"), Some(100_000.0));
        assert_eq!(parse_number(".5"), Some(0.5));
        assert_eq!(parse_number("5."), Some(5.0));
        assert_eq!(parse_number("NULL"), None);
        assert_eq!(parse_number("NaN"), None);
        assert_eq!(parse_number("inf"), None);
        assert_eq!(parse_number("-infinity"), None);
        assert_eq!(parse_number("1e999"), None);
    }

    #[test]
    fn dates_iso_et_europeennes() {
        assert_eq!(
            parse_date("2024-01-05"),
            NaiveDate::from_ymd_opt(2024, 1, 5)
        );
        assert_eq!(
            parse_date("2024/01/05"),
            NaiveDate::from_ymd_opt(2024, 1, 5)
        );
        // 25 > 12 → forcément le jour, aucune ambiguïté.
        assert_eq!(
            parse_date("25/12/2024"),
            NaiveDate::from_ymd_opt(2024, 12, 25)
        );
        // Deuxième champ > 12 → mois en tête (format US).
        assert_eq!(
            parse_date("12/25/2024"),
            NaiveDate::from_ymd_opt(2024, 12, 25)
        );
        // Ambigu → jour d'abord (hypothèse européenne).
        assert_eq!(parse_date("05/01/2024"), NaiveDate::from_ymd_opt(2024, 1, 5));
        assert_eq!(parse_date("5.1.24"), NaiveDate::from_ymd_opt(2024, 1, 5));
        assert_eq!(parse_date("pas une date"), None);
    }

    #[test]
    fn heures() {
        assert_eq!(parse_time("07:30"), NaiveTime::from_hms_opt(7, 30, 0));
        assert_eq!(parse_time("07:30:15"), NaiveTime::from_hms_opt(7, 30, 15));
        assert_eq!(parse_time("7:30:15.250"), NaiveTime::from_hms_opt(7, 30, 15));
        assert_eq!(parse_time("7:30 PM"), NaiveTime::from_hms_opt(19, 30, 0));
        assert_eq!(parse_time("12:00 AM"), NaiveTime::from_hms_opt(0, 0, 0));
        assert_eq!(parse_time("12:00 PM"), NaiveTime::from_hms_opt(12, 0, 0));
        assert_eq!(parse_time("nope"), None);
    }

    #[test]
    fn horodatages_complets() {
        let expected = NaiveDate::from_ymd_opt(2024, 1, 5)
            .unwrap()
            .and_hms_opt(7, 30, 0)
            .unwrap();
        assert_eq!(parse_datetime_flexible("2024-01-05 07:30:00"), Some(expected));
        assert_eq!(parse_datetime_flexible("2024-01-05T07:30:00"), Some(expected));
        assert_eq!(parse_datetime_flexible("2024-01-05T07:30:00Z"), Some(expected));
        assert_eq!(parse_datetime_flexible("05/01/2024 07:30"), Some(expected));
        assert_eq!(
            parse_datetime_flexible("2024-01-05"),
            NaiveDate::from_ymd_opt(2024, 1, 5).unwrap().and_hms_opt(0, 0, 0)
        );
        assert_eq!(parse_datetime_flexible(""), None);
        assert_eq!(parse_datetime_flexible("Température"), None);
    }

    #[test]
    fn deux_colonnes_date_heure() {
        assert_eq!(
            parse_date_and_time("05/01/2024", "07:30:00"),
            NaiveDate::from_ymd_opt(2024, 1, 5).unwrap().and_hms_opt(7, 30, 0)
        );
    }

    #[test]
    fn detection_du_separateur() {
        let tab = vec!["No.\tTime\tTemp", "1\t2024-01-05 07:00\t-12.5"];
        assert_eq!(detect_delimiter(&tab), '\t');

        let semi = vec!["No.;Time;Temp", "1;2024-01-05 07:00;-12,5"];
        assert_eq!(detect_delimiter(&semi), ';');

        let comma = vec!["No.,Time,Temp", "1,2024-01-05 07:00,-12.5"];
        assert_eq!(detect_delimiter(&comma), ',');
    }

    #[test]
    fn intervalles() {
        assert_eq!(parse_interval_seconds("00:10:00"), Some(600));
        assert_eq!(parse_interval_seconds("10 min"), Some(600));
        assert_eq!(parse_interval_seconds("600s"), Some(600));
        assert_eq!(parse_interval_seconds("1 h"), Some(3600));
        assert_eq!(parse_interval_seconds("30"), Some(30));
        assert_eq!(parse_interval_seconds("beaucoup"), None);
    }

    #[test]
    fn reconnaissance_des_entetes() {
        assert!(looks_like_timestamp_header("Date Time"));
        assert!(looks_like_timestamp_header("Heure"));
        assert!(looks_like_temperature_header("Temperature(°C)"));
        assert!(looks_like_temperature_header("Temp"));
        assert!(!looks_like_temperature_header("Humidity(%RH)"));

        // Le couple d'intitulés de l'export xlsx réel d'ElitechLog. « Temps »
        // contient « temp » : c'est le piège que la garde du parseur écarte.
        assert!(looks_like_timestamp_header("Temps"));
        assert!(!looks_like_temperature_header("Temps"));
        assert!(looks_like_temperature_header("Température°C"));
        assert!(!looks_like_timestamp_header("Température°C"));
        assert!(looks_like_humidity_header("Humidity(%RH)"));
        assert!(looks_like_date_only_header("Date"));
        assert!(!looks_like_date_only_header("Date Time"));
        assert!(looks_like_time_only_header("Time"));
    }

    #[test]
    fn nettoyage_du_contenu() {
        let raw = "\u{feff}a\r\nb\rc\n";
        assert_eq!(normalize_content(raw), "a\nb\nc\n");
    }

    #[test]
    fn decoupage_avec_guillemets() {
        assert_eq!(
            split_line("\"2024-01-05 07:00\";\" -12,5 \"", ';'),
            vec!["2024-01-05 07:00".to_string(), "-12,5".to_string()]
        );
    }

    // -----------------------------------------------------------------------
    // Ordre jour/mois
    // -----------------------------------------------------------------------

    /// Passe une liste de jetons au scanner et rend le bilan.
    fn scanner(jetons: &[&str]) -> DateOrderEvidence {
        let mut s = DateOrderScanner::new();
        for (i, jeton) in jetons.iter().enumerate() {
            s.observe(i + 1, jeton);
        }
        s.finish()
    }

    #[test]
    fn un_jour_superieur_a_douze_prouve_le_jour_d_abord() {
        assert_eq!(
            classify_date_token("13/01/2024"),
            DateTokenKind::PreuveJourDAbord
        );
        assert_eq!(
            classify_date_token("25/12/2024 07:30"),
            DateTokenKind::PreuveJourDAbord,
            "la partie heure ne doit pas gêner la classification"
        );
    }

    #[test]
    fn un_second_champ_superieur_a_douze_prouve_le_mois_d_abord() {
        assert_eq!(
            classify_date_token("01/13/2024"),
            DateTokenKind::PreuveMoisDAbord
        );
    }

    #[test]
    fn les_dates_a_deux_champs_faibles_sont_ambigues() {
        assert_eq!(classify_date_token("05/01/2024"), DateTokenKind::Ambigu);
        assert_eq!(classify_date_token("2024-01-05"), DateTokenKind::Iso);
        assert_eq!(classify_date_token("pas une date"), DateTokenKind::Illisible);
        // Aucun ordre ne rend « 13/14 » valide.
        assert_eq!(classify_date_token("13/14/2024"), DateTokenKind::Illisible);
    }

    #[test]
    fn une_preuve_suffit_a_lever_le_doute() {
        let e = scanner(&["05/01/2024", "13/01/2024", "06/01/2024"]);
        assert_eq!(e.order, DateOrder::DayFirst);
        assert!(!e.uncertain, "une preuve rend la lecture certaine");
        assert!(!e.conflicting);
        assert_eq!(e.day_first_hits, 1);
        assert_eq!(e.ambiguous_hits, 2);
        assert_eq!(e.samples, vec!["13/01/2024".to_string()]);

        let e = scanner(&["01/13/2024", "01/05/2024"]);
        assert_eq!(e.order, DateOrder::MonthFirst);
        assert!(!e.uncertain);
    }

    #[test]
    fn un_fichier_sans_preuve_reste_incertain() {
        let e = scanner(&["05/01/2024", "06/01/2024", "07/02/2024"]);
        assert_eq!(e.order, DEFAULT_DATE_ORDER, "l'ordre n'est qu'un repli");
        assert!(
            e.uncertain,
            "sans preuve et avec des dates ambiguës, il faut le dire"
        );
        assert!(!e.prouve());
    }

    /// Le piège : un fichier entièrement ISO n'est **pas** incertain. L'ordre
    /// jour/mois n'a simplement aucune prise sur lui.
    #[test]
    fn un_fichier_iso_n_est_jamais_incertain() {
        let e = scanner(&["2024-01-05", "2024-01-06", "2024-02-07"]);
        assert!(
            !e.uncertain,
            "l'ISO est auto-descriptif : rien à confirmer, aucun drapeau à lever"
        );
        assert_eq!(e.iso_hits, 3);
        assert_eq!(e.ambiguous_hits, 0);

        // Idem pour un fichier sans aucune date lisible.
        assert!(!scanner(&["", "bruit"]).uncertain);
    }

    #[test]
    fn des_preuves_contradictoires_suivent_la_majorite_et_signalent_le_conflit() {
        // 2 preuves « jour d'abord » (lignes 1 et 2) contre 1 « mois d'abord » (ligne 3).
        let e = scanner(&["13/01/2024", "14/01/2024", "01/13/2024"]);
        assert_eq!(e.order, DateOrder::DayFirst, "la majorité l'emporte");
        assert!(e.conflicting);
        assert!(e.uncertain, "un fichier qui se contredit doit être vérifié");
        assert_eq!(
            e.first_conflict_line,
            Some(3),
            "on pointe la preuve minoritaire, celle qui contredit l'ordre appliqué"
        );

        // À égalité, on ne tranche pas : repli.
        let e = scanner(&["13/01/2024", "01/13/2024"]);
        assert_eq!(e.order, DEFAULT_DATE_ORDER);
        assert!(e.conflicting && e.uncertain);
    }

    #[test]
    fn parse_date_avec_ordre_impose_ignore_la_constante() {
        let cinq_janvier = NaiveDate::from_ymd_opt(2024, 1, 5);
        let premier_mai = NaiveDate::from_ymd_opt(2024, 5, 1);

        assert_eq!(
            parse_date_with_order("05/01/2024", DateOrder::DayFirst),
            cinq_janvier
        );
        assert_eq!(
            parse_date_with_order("05/01/2024", DateOrder::MonthFirst),
            premier_mai
        );

        // Une date qui se prouve elle-même ne se laisse pas imposer un ordre faux.
        assert_eq!(
            parse_date_with_order("25/12/2024", DateOrder::MonthFirst),
            NaiveDate::from_ymd_opt(2024, 12, 25)
        );
        // L'ISO non plus.
        assert_eq!(
            parse_date_with_order("2024-01-05", DateOrder::MonthFirst),
            cinq_janvier
        );
    }

    #[test]
    fn permuter_jour_mois_refuse_un_jour_superieur_a_douze() {
        let d = |y, m, j| NaiveDate::from_ymd_opt(y, m, j).unwrap();

        assert_eq!(swap_day_month(d(2024, 1, 5)), Some(d(2024, 5, 1)));
        // 25 janvier : le jour ne peut pas devenir un mois.
        assert_eq!(swap_day_month(d(2024, 1, 25)), None);

        // La permutation est involutive tant qu'elle est définie.
        let date = d(2024, 3, 7);
        assert_eq!(swap_day_month(swap_day_month(date).unwrap()), Some(date));

        // L'heure est conservée telle quelle.
        let dt = d(2024, 1, 5).and_hms_opt(7, 30, 15).unwrap();
        let permute = swap_day_month_datetime(dt).unwrap();
        assert_eq!(permute.date(), d(2024, 5, 1));
        assert_eq!(permute.time(), dt.time());
    }

    #[test]
    fn ordre_de_date_aller_retour_et_inverse() {
        for o in [DateOrder::DayFirst, DateOrder::MonthFirst] {
            assert_eq!(DateOrder::parse(o.as_str()), Some(o));
            assert_eq!(o.inverse().inverse(), o);
            assert_ne!(o.inverse(), o);
        }
        assert_eq!(DateOrder::parse("n'importe quoi"), None);
    }

    #[test]
    fn extraction_du_jeton_de_date() {
        assert_eq!(date_token("05/01/2024 07:30:00"), "05/01/2024");
        assert_eq!(date_token("2024-01-05T07:30:00"), "2024-01-05");
        assert_eq!(date_token("  05/01/2024  "), "05/01/2024");
    }
}

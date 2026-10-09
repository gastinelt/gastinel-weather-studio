//! **Saisie manuelle sur site** : une valeur tapée à la main, sans fichier ni
//! appareil.
//!
//! ┌───────────────────────────────────────────────────────────────────────────┐
//! │  À ne pas confondre avec `manual_import.rs`.                              │
//! │  Celui-là lit un *fichier* dont l'utilisateur désigne les colonnes ; ici  │
//! │  il n'y a aucun fichier, l'utilisateur tape la valeur elle-même.          │
//! └───────────────────────────────────────────────────────────────────────────┘
//!
//! ## Le cas d'usage
//!
//! Relevé manuscrit sur le terrain, sans logger disponible ce jour-là : « le
//! 14 janvier, entre 6 h et 9 h, il faisait −4,2 °C en moyenne ». On veut le
//! garder — c'est une donnée de plus sur un point qui en a peu — sans jamais
//! faire croire qu'elle vaut un échantillonnage continu.
//!
//! ## Ce qu'une saisie n'est pas
//!
//! Elle porte sur une **plage horaire**, pas sur un instant. « Moyenne 6 h–9 h »
//! ne décrit pas ce qu'il faisait à 6 h ; ça décrit trois heures. Le modèle, lui,
//! ne connaît que des instants. Deux conséquences assumées :
//!
//! * l'instant retenu est le **milieu de la plage** (7 h 30 pour 6 h–9 h). C'est
//!   le seul point de la plage qui ne privilégie aucun de ses deux bords, et il
//!   place la valeur au centre de ce qu'elle résume. Retenir le début l'aurait
//!   décalée d'une demi-plage vers le passé, sans rien gagner ;
//! * la nature de la valeur (moyenne, minimum, maximum) et les bornes de la
//!   plage sont écrites dans le **libellé de la série**, pas dans la mesure. Le
//!   modèle `Measurement` n'a pas de champ pour ça, et lui en ajouter un pour
//!   une source sur sept aurait coûté une colonne `NULL` partout ailleurs — voir
//!   [`ValidatedEntry::series_label`], qui rend la phrase.
//!
//! Ce que la base garde de tout cela, et qui suffit à ne pas s'y tromper, c'est
//! le [`crate::model::SourceType::ManualEntry`] porté par la série.
//!
//! ## Une valeur par jour, jamais une valeur pour tous
//!
//! Quand plusieurs jours sont saisis d'un coup, chacun porte **sa** valeur.
//! Recopier la même température sur cinq jours parce que l'utilisateur n'en a
//! tapé qu'une fabriquerait quatre mesures qui n'ont jamais été relevées ; c'est
//! refusé ici ([`ManualEntryError::MissingValue`]), pas comblé en silence.
//!
//! ## Le temps
//!
//! Comme pour un logger, l'horodatage produit est **naïf** : c'est l'heure que
//! l'utilisateur a lue sur sa montre. La conversion en UTC est faite une seule
//! fois, plus loin, par `clock_offset_seconds` — exactement le même chemin que
//! pour un fichier Elitech (voir `commands::persist_import`). Rien de particulier
//! n'est fait ici : c'est ce qui garantit que la contrainte n°1 du projet vaut
//! aussi pour cette source.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};

use crate::elitech_import::{ParsedRows, RawMeasurement};
use crate::parsing::DateOrderEvidence;

/// Températures acceptées, en °C. Bornes volontairement très larges : elles ne
/// jugent pas la vraisemblance d'un relevé (−35 °C est un excellent relevé pour
/// ce projet), elles arrêtent la faute de frappe — un `422` tapé pour `4,22`.
pub const TEMPERATURE_MIN: f64 = -90.0;
pub const TEMPERATURE_MAX: f64 = 60.0;

/// Nombre de jours saisissables en une fois.
///
/// 366 : une année complète d'un carnet de terrain, et une borne qui empêche
/// qu'une plage mal tapée (2020 → 2026) fabrique deux mille champs de saisie.
pub const MAX_DAYS: usize = 366;

// ---------------------------------------------------------------------------
// Nature de la valeur saisie
// ---------------------------------------------------------------------------

/// Ce que la valeur tapée représente sur la plage horaire.
///
/// Conservée pour être **écrite au clair dans le libellé de la série** : un
/// minimum de nuit et une moyenne de nuit ne se lisent pas de la même façon, et
/// rien dans la mesure elle-même ne les distinguerait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManualValueKind {
    Average,
    Minimum,
    Maximum,
}

impl ManualValueKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ManualValueKind::Average => "Average",
            ManualValueKind::Minimum => "Minimum",
            ManualValueKind::Maximum => "Maximum",
        }
    }

    pub fn label_fr(self) -> &'static str {
        match self {
            ManualValueKind::Average => "moyenne",
            ManualValueKind::Minimum => "minimum",
            ManualValueKind::Maximum => "maximum",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ManualEntryError> {
        match raw.trim() {
            "Average" | "average" | "moyenne" => Ok(ManualValueKind::Average),
            "Minimum" | "minimum" | "min" => Ok(ManualValueKind::Minimum),
            "Maximum" | "maximum" | "max" => Ok(ManualValueKind::Maximum),
            other => Err(ManualEntryError::UnknownValueKind(other.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------

/// Échecs de la saisie manuelle. Comme partout ailleurs dans ce projet, les
/// messages sont rédigés pour l'utilisateur : ils disent quoi corriger.
#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize, Deserialize)]
pub enum ManualEntryError {
    #[error("Type de valeur inconnu : « {0} » (attendu : moyenne, minimum ou maximum)")]
    UnknownValueKind(String),

    #[error("Heure illisible : « {0} » (format attendu : HH:MM, par exemple 06:00)")]
    BadTime(String),

    #[error("Date illisible : « {0} » (format attendu : AAAA-MM-JJ)")]
    BadDate(String),

    #[error(
        "La plage horaire est vide : {start} et {end} ne délimitent aucune durée. \
         Indiquez une heure de fin postérieure à l'heure de début."
    )]
    EmptyRange { start: String, end: String },

    #[error("Aucun jour n'a été saisi : indiquez au moins une date et sa valeur.")]
    NoDays,

    #[error(
        "{count} jours demandés, {max} au maximum en une seule saisie. \
         Vérifiez les dates de début et de fin, puis saisissez en plusieurs fois \
         si la plage est vraiment aussi longue."
    )]
    TooManyDays { count: usize, max: usize },

    #[error(
        "Le {date} n'a pas de valeur. Chaque jour saisi porte la sienne : recopier \
         la même température sur plusieurs jours fabriquerait des relevés qui n'ont \
         jamais été faits."
    )]
    MissingValue { date: String },

    #[error(
        "Le {date} porte {value} °C, hors des bornes acceptées ({min} à {max} °C). \
         S'agit-il d'une faute de frappe ?"
    )]
    TemperatureOutOfRange {
        date: String,
        value: f64,
        min: f64,
        max: f64,
    },

    #[error("Le {date} est saisi deux fois. Chaque jour ne peut porter qu'une valeur par saisie.")]
    DuplicateDay { date: String },
}

// ---------------------------------------------------------------------------
// Ce que l'UI envoie
// ---------------------------------------------------------------------------

/// Une journée saisie : sa date, et la valeur relevée ce jour-là.
///
/// `temperature_celsius` est une `Option` **volontairement** : le formulaire
/// laisse forcément des cases vides tant qu'il n'est pas rempli, et l'oubli doit
/// ressortir en erreur nommée ([`ManualEntryError::MissingValue`], qui dit
/// *quel* jour) plutôt qu'en zéro silencieux.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManualEntryDay {
    /// `AAAA-MM-JJ`. Aucune ambiguïté jour/mois possible, contrairement aux
    /// fichiers : c'est un `<input type="date">` qui la produit, pas un humain.
    pub date: String,
    #[serde(default)]
    pub temperature_celsius: Option<f64>,
}

/// La saisie complète, telle qu'elle arrive du formulaire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManualEntryInput {
    /// Début de la plage horaire, `HH:MM`.
    pub start_time: String,
    /// Fin de la plage horaire, `HH:MM`. `"24:00"` est accepté pour dire
    /// « jusqu'à la fin de la journée » — un `<input type="time">` ne le produit
    /// pas, mais une plage 21 h → minuit se saisit naturellement comme ça.
    pub end_time: String,
    /// `"Average"`, `"Minimum"` ou `"Maximum"`.
    pub value_kind: String,
    pub days: Vec<ManualEntryDay>,
}

/// La même saisie, validée : plus une seule chaîne à interpréter.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedEntry {
    pub start: NaiveTime,
    /// Minutes depuis minuit, de 1 à 1440 — 1440 étant « minuit du lendemain ».
    /// Un `NaiveTime` ne sait pas représenter 24:00 ; ce compteur, si.
    pub end_minutes: u32,
    pub value_kind: ManualValueKind,
    pub days: Vec<(NaiveDate, f64)>,
}

impl ValidatedEntry {
    /// Instant retenu pour un jour donné : le **milieu** de la plage horaire.
    ///
    /// Voir l'en-tête du module pour la raison. Le calcul se fait en minutes
    /// depuis minuit, ce qui laisse une plage se terminer à 24:00 sans cas
    /// particulier : le milieu de 21:00–24:00 est 22:30, du même jour.
    pub fn instant(&self, day: NaiveDate) -> NaiveDateTime {
        let debut = self.start_minutes();
        let milieu = (debut + self.end_minutes) / 2;
        // `milieu` reste strictement inférieur à 1440 : avec `debut < end_minutes`
        // et `end_minutes <= 1440`, la moyenne vaut au plus (1439 + 1440) / 2, soit
        // 1439. Le `unwrap_or` n'est donc jamais emprunté ; il est là pour ne pas
        // paniquer si cette arithmétique changeait un jour.
        let heure = NaiveTime::from_hms_opt(milieu / 60, milieu % 60, 0).unwrap_or(NaiveTime::MIN);
        day.and_time(heure)
    }

    fn start_minutes(&self) -> u32 {
        self.start
            .signed_duration_since(NaiveTime::MIN)
            .num_minutes() as u32
    }

    /// Durée de la plage, en secondes. Sert de `sample_interval_seconds` :
    /// c'est l'étendue que couvre chaque valeur, la seule « cadence » qui ait un
    /// sens ici — il n'y a aucun échantillonnage.
    pub fn range_seconds(&self) -> i64 {
        (i64::from(self.end_minutes) - i64::from(self.start_minutes())) * 60
    }

    /// Phrase à écrire dans le libellé de la série et dans `raw_file_reference`.
    ///
    /// C'est le seul endroit où « moyenne 06:00–09:00 » survit : la mesure, elle,
    /// n'est qu'un instant et une température.
    pub fn series_label(&self) -> String {
        format!(
            "Saisie manuelle — {} sur {}–{} ({} jour{})",
            self.value_kind.label_fr(),
            self.start.format("%H:%M"),
            fmt_minutes(self.end_minutes),
            self.days.len(),
            if self.days.len() > 1 { "s" } else { "" }
        )
    }
}

/// `1440` → `"24:00"`, `570` → `"09:30"`.
fn fmt_minutes(minutes: u32) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// `"06:00"` → minutes depuis minuit. Accepte `"6:00"`, `"06:00:00"` et `"24:00"`.
fn parse_minutes(raw: &str) -> Result<u32, ManualEntryError> {
    let brut = raw.trim();
    let mauvaise = || ManualEntryError::BadTime(brut.to_string());
    let mut morceaux = brut.split(':');
    let heures: u32 = morceaux
        .next()
        .ok_or_else(mauvaise)?
        .trim()
        .parse()
        .map_err(|_| mauvaise())?;
    let minutes: u32 = match morceaux.next() {
        Some(m) => m.trim().parse().map_err(|_| mauvaise())?,
        None => 0,
    };
    // Les secondes sont tolérées mais ignorées : la plage se raisonne à la
    // minute, et un `<input type="time">` peut en produire selon son `step`.
    if let Some(s) = morceaux.next() {
        s.trim().parse::<u32>().map_err(|_| mauvaise())?;
    }
    if minutes > 59 {
        return Err(mauvaise());
    }
    let total = heures * 60 + minutes;
    if total > 24 * 60 {
        return Err(mauvaise());
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Vérifie la saisie de bout en bout et la traduit en valeurs typées.
///
/// **Rien n'est corrigé en silence** : une case vide, une date en double, une
/// température aberrante ressortent en erreur nommée, avec le jour concerné.
/// C'est la contrepartie d'une source où il n'y a aucun fichier à relire pour
/// comprendre après coup ce qui a été enregistré.
pub fn validate(input: &ManualEntryInput) -> Result<ValidatedEntry, ManualEntryError> {
    let debut_min = parse_minutes(&input.start_time)?;
    let fin_min = parse_minutes(&input.end_time)?;
    if fin_min <= debut_min {
        return Err(ManualEntryError::EmptyRange {
            start: fmt_minutes(debut_min),
            end: fmt_minutes(fin_min),
        });
    }
    let start = NaiveTime::from_hms_opt(debut_min / 60, debut_min % 60, 0)
        .ok_or_else(|| ManualEntryError::BadTime(input.start_time.clone()))?;

    let value_kind = ManualValueKind::parse(&input.value_kind)?;

    if input.days.is_empty() {
        return Err(ManualEntryError::NoDays);
    }
    if input.days.len() > MAX_DAYS {
        return Err(ManualEntryError::TooManyDays {
            count: input.days.len(),
            max: MAX_DAYS,
        });
    }

    let mut jours: Vec<(NaiveDate, f64)> = Vec::with_capacity(input.days.len());
    for jour in &input.days {
        let date = NaiveDate::parse_from_str(jour.date.trim(), "%Y-%m-%d")
            .map_err(|_| ManualEntryError::BadDate(jour.date.clone()))?;

        if jours.iter().any(|(d, _)| *d == date) {
            return Err(ManualEntryError::DuplicateDay {
                date: date.to_string(),
            });
        }

        let valeur = jour
            .temperature_celsius
            .ok_or_else(|| ManualEntryError::MissingValue {
                date: date.to_string(),
            })?;
        if !valeur.is_finite() || !(TEMPERATURE_MIN..=TEMPERATURE_MAX).contains(&valeur) {
            return Err(ManualEntryError::TemperatureOutOfRange {
                date: date.to_string(),
                value: valeur,
                min: TEMPERATURE_MIN,
                max: TEMPERATURE_MAX,
            });
        }
        jours.push((date, valeur));
    }

    // Ordre chronologique : le formulaire les envoie déjà triés, mais l'empreinte
    // de contenu et les bornes de période s'en remettent à cet ordre. Le garantir
    // ici coûte un tri sur au plus 366 éléments.
    jours.sort_by_key(|(d, _)| *d);

    Ok(ValidatedEntry {
        start,
        end_minutes: fin_min,
        value_kind,
        days: jours,
    })
}

/// Transforme la saisie validée en lignes brutes, prêtes pour le pipeline
/// d'import commun (`commands::persist_import`).
///
/// C'est ce qui fait qu'une saisie manuelle profite sans un mot de plus de la
/// détection de ré-import, du contrôle des bornes et du recalibrage d'horloge :
/// elle arrive au même endroit que les autres sources, sous la même forme.
///
/// `source_line` porte le **rang du jour** (1-based) : il n'y a pas de fichier,
/// donc pas de numéro de ligne ; c'est la seule référence qui ait un sens, et
/// c'est déjà la convention retenue pour la lecture directe du RC-5.
pub fn to_parsed_rows(entry: &ValidatedEntry) -> ParsedRows {
    let measurements: Vec<RawMeasurement> = entry
        .days
        .iter()
        .enumerate()
        .map(|(i, (date, temperature))| RawMeasurement {
            raw_timestamp: entry.instant(*date),
            temperature_celsius: *temperature,
            // Aucune humidité : on ne saisit qu'une température. `None` et non
            // `0.0` — une valeur absente n'est pas une valeur nulle.
            humidity_percent: None,
            source_line: i + 1,
        })
        .collect();

    ParsedRows {
        // Aucune ligne écartée : la validation refuse la saisie entière plutôt
        // que d'en garder une partie. Un formulaire à moitié enregistré serait
        // plus difficile à rattraper qu'un formulaire refusé.
        warnings: Vec::new(),
        // Les dates viennent d'un sélecteur de date, jamais d'un texte à
        // interpréter : il n'y a aucun ordre jour/mois à deviner.
        date_order: DateOrderEvidence::iso(measurements.len()),
        measurements,
    }
}

/// Raccourci : valide puis convertit.
pub fn parse_manual_entry(
    input: &ManualEntryInput,
) -> Result<(ValidatedEntry, ParsedRows), ManualEntryError> {
    let validee = validate(input)?;
    let lignes = to_parsed_rows(&validee);
    Ok((validee, lignes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jour(date: &str, t: f64) -> ManualEntryDay {
        ManualEntryDay {
            date: date.to_string(),
            temperature_celsius: Some(t),
        }
    }

    fn saisie(days: Vec<ManualEntryDay>) -> ManualEntryInput {
        ManualEntryInput {
            start_time: "06:00".to_string(),
            end_time: "09:00".to_string(),
            value_kind: "Minimum".to_string(),
            days,
        }
    }

    #[test]
    fn l_instant_retenu_est_le_milieu_de_la_plage() {
        let v = validate(&saisie(vec![jour("2026-01-14", -4.2)])).unwrap();
        let rows = to_parsed_rows(&v);
        assert_eq!(rows.measurements.len(), 1);
        assert_eq!(
            rows.measurements[0].raw_timestamp.to_string(),
            "2026-01-14 07:30:00"
        );
    }

    #[test]
    fn une_plage_qui_finit_a_minuit_est_acceptee() {
        let mut input = saisie(vec![jour("2026-01-14", -4.2)]);
        input.start_time = "21:00".to_string();
        input.end_time = "24:00".to_string();
        let v = validate(&input).unwrap();
        assert_eq!(v.end_minutes, 1440);
        assert_eq!(v.range_seconds(), 3 * 3600);
        assert_eq!(
            v.instant(NaiveDate::from_ymd_opt(2026, 1, 14).unwrap())
                .to_string(),
            "2026-01-14 22:30:00"
        );
    }

    #[test]
    fn chaque_jour_porte_sa_propre_valeur() {
        let v = validate(&saisie(vec![
            jour("2026-01-14", -4.2),
            jour("2026-01-15", -7.8),
            jour("2026-01-16", -1.0),
        ]))
        .unwrap();
        let rows = to_parsed_rows(&v);
        let valeurs: Vec<f64> = rows
            .measurements
            .iter()
            .map(|m| m.temperature_celsius)
            .collect();
        assert_eq!(valeurs, vec![-4.2, -7.8, -1.0]);
        // Trois jours, trois instants distincts, tous au milieu de la plage.
        assert_eq!(
            rows.measurements[2].raw_timestamp.to_string(),
            "2026-01-16 07:30:00"
        );
    }

    #[test]
    fn un_jour_sans_valeur_est_refuse_et_dit_lequel() {
        let input = saisie(vec![
            jour("2026-01-14", -4.2),
            ManualEntryDay {
                date: "2026-01-15".to_string(),
                temperature_celsius: None,
            },
        ]);
        assert_eq!(
            validate(&input),
            Err(ManualEntryError::MissingValue {
                date: "2026-01-15".to_string()
            })
        );
    }

    #[test]
    fn une_plage_horaire_vide_est_refusee() {
        let mut input = saisie(vec![jour("2026-01-14", -4.2)]);
        input.end_time = "06:00".to_string();
        assert!(matches!(
            validate(&input),
            Err(ManualEntryError::EmptyRange { .. })
        ));
    }

    #[test]
    fn une_temperature_aberrante_est_refusee() {
        let input = saisie(vec![jour("2026-01-14", 422.0)]);
        assert!(matches!(
            validate(&input),
            Err(ManualEntryError::TemperatureOutOfRange { .. })
        ));
    }

    #[test]
    fn un_jour_saisi_deux_fois_est_refuse() {
        let input = saisie(vec![jour("2026-01-14", -4.2), jour("2026-01-14", -5.0)]);
        assert_eq!(
            validate(&input),
            Err(ManualEntryError::DuplicateDay {
                date: "2026-01-14".to_string()
            })
        );
    }

    #[test]
    fn les_jours_ressortent_dans_l_ordre_chronologique() {
        let v = validate(&saisie(vec![
            jour("2026-01-16", -1.0),
            jour("2026-01-14", -4.2),
        ]))
        .unwrap();
        assert_eq!(v.days[0].0.to_string(), "2026-01-14");
        assert_eq!(v.days[1].0.to_string(), "2026-01-16");
    }

    #[test]
    fn le_libelle_dit_la_nature_et_la_plage() {
        let v = validate(&saisie(vec![
            jour("2026-01-14", -4.2),
            jour("2026-01-15", -3.0),
        ]))
        .unwrap();
        assert_eq!(
            v.series_label(),
            "Saisie manuelle — minimum sur 06:00–09:00 (2 jours)"
        );
    }

    #[test]
    fn les_trois_natures_de_valeur_font_l_aller_retour() {
        for k in [
            ManualValueKind::Average,
            ManualValueKind::Minimum,
            ManualValueKind::Maximum,
        ] {
            assert_eq!(ManualValueKind::parse(k.as_str()).unwrap(), k);
        }
        assert!(matches!(
            ManualValueKind::parse("médiane"),
            Err(ManualEntryError::UnknownValueKind(_))
        ));
    }

    #[test]
    fn une_heure_illisible_est_refusee() {
        let mut input = saisie(vec![jour("2026-01-14", -4.2)]);
        input.start_time = "6h".to_string();
        assert!(matches!(validate(&input), Err(ManualEntryError::BadTime(_))));
        input.start_time = "25:00".to_string();
        assert!(matches!(validate(&input), Err(ManualEntryError::BadTime(_))));
    }

    #[test]
    fn une_saisie_vide_est_refusee() {
        assert_eq!(validate(&saisie(vec![])), Err(ManualEntryError::NoDays));
    }

    #[test]
    fn au_dela_d_une_annee_la_saisie_est_refusee() {
        let jours: Vec<ManualEntryDay> = (0..MAX_DAYS + 1)
            .map(|i| {
                let d =
                    NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64);
                jour(&d.to_string(), -1.0)
            })
            .collect();
        assert!(matches!(
            validate(&saisie(jours)),
            Err(ManualEntryError::TooManyDays { .. })
        ));
    }

    #[test]
    fn aucun_ordre_jour_mois_n_est_a_deviner() {
        let v = validate(&saisie(vec![jour("2026-01-14", -4.2)])).unwrap();
        let rows = to_parsed_rows(&v);
        assert!(!rows.date_order.uncertain);
        assert!(!rows.date_order.conflicting);
        assert_eq!(rows.date_order.iso_hits, 1);
    }
}

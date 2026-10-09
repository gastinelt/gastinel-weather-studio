//! Statistiques dérivées d'une série de mesures (§1.5).
//!
//! Toutes les fonctions travaillent sur des tranches de [`Measurement`] et
//! **n'ont aucune dépendance à la base de données** : elles sont donc testables
//! directement, et réutilisables plus tard pour comparer deux points entre eux.
//!
//! Conventions :
//! * Le découpage en journées se fait sur le **jour calendaire UTC**
//!   (`timestamp_utc.date_naive()`), conformément au §1.5. C'est cohérent avec le
//!   stockage, mais attention : une nuit d'inversion se lit à cheval sur deux
//!   jours UTC. Un découpage « nuit météo » (18 h → 18 h locales) sera sans doute
//!   plus pertinent pour la chasse aux TAF ; on l'ajoutera quand on comparera
//!   des points entre eux — d'où [`daily_stats_with_day_offset`], déjà prévu.
//! * Une journée sans aucune mesure n'apparaît pas dans le résultat (pas de
//!   trou comblé artificiellement) ; `day_count` compte les journées observées.
//! * Les agrégats sont `None` sur une série vide plutôt que `0.0` — pas de
//!   fausse donnée.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::model::Measurement;

// ---------------------------------------------------------------------------
// De quoi les statistiques de température ont réellement besoin
// ---------------------------------------------------------------------------
//
// Historiquement tout ce fichier prenait des `&[Measurement]`. C'est resté vrai
// — [`Measurement`] implémente le trait ci-dessous — mais ce n'est plus la
// *seule* entrée possible, et la raison est mesurable.
//
// Une [`Measurement`] porte quatorze champs, dont un `series_id: String` : une
// allocation par ligne. Sur l'archive ROMMA (476 912 mesures pour un seul
// point), reconstruire la structure complète pour n'en lire que l'horodatage et
// la température coûte un demi-million d'allocations et ~70 Mo, à chaque
// changement de sélection dans le bloc « Croisement de données ».
//
// D'où [`TempSample`] : 16 octets, aucune allocation, lu directement par une
// requête à deux colonnes (`Db::temperature_samples_for_point`). Les fonctions
// de ce module ont été **généralisées, pas dupliquées** : il n'existe toujours
// qu'une seule implémentation de « moyenne des minimums quotidiens ».

/// Le strict nécessaire pour les agrégats de température : un instant, une valeur.
pub trait TemperatureSample {
    fn instant(&self) -> DateTime<Utc>;
    fn temperature(&self) -> f64;
}

impl TemperatureSample for Measurement {
    fn instant(&self) -> DateTime<Utc> {
        self.timestamp_utc
    }
    fn temperature(&self) -> f64 {
        self.temperature_celsius
    }
}

/// Échantillon minimal (instant UTC, °C), sans métadonnée ni allocation.
///
/// `Copy` volontairement : ces valeurs se manipulent par centaines de milliers,
/// et un `clone()` implicite y serait un piège.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TempSample {
    pub timestamp_utc: DateTime<Utc>,
    pub temperature_celsius: f64,
}

impl TempSample {
    pub fn new(timestamp_utc: DateTime<Utc>, temperature_celsius: f64) -> Self {
        TempSample {
            timestamp_utc,
            temperature_celsius,
        }
    }
}

impl TemperatureSample for TempSample {
    fn instant(&self) -> DateTime<Utc> {
        self.timestamp_utc
    }
    fn temperature(&self) -> f64 {
        self.temperature_celsius
    }
}

/// Statistiques d'une journée calendaire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailyStats {
    /// Jour calendaire (UTC), au format ISO `YYYY-MM-DD` une fois sérialisé.
    pub date: NaiveDate,
    pub min_c: f64,
    pub mean_c: f64,
    pub max_c: f64,
    /// `max_c - min_c` : amplitude thermique du jour.
    pub amplitude_c: f64,
    pub measurement_count: usize,
}

/// Agrégats sur toute la période fournie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeriodStats {
    pub measurement_count: usize,
    /// Nombre de journées calendaires effectivement observées.
    pub day_count: usize,
    pub first_utc: Option<DateTime<Utc>>,
    pub last_utc: Option<DateTime<Utc>>,

    /// Minimum absolu sur toute la période (le « record » du point).
    pub extreme_min_c: Option<f64>,
    /// Maximum absolu sur toute la période.
    pub extreme_max_c: Option<f64>,
    /// Moyenne de toutes les mesures (pondérée par mesure, pas par jour).
    pub mean_c: Option<f64>,

    /// Moyenne des minimums quotidiens. Appliquée à une période d'un an, c'est
    /// la « moyenne annuelle des minimums quotidiens » du §1.5.
    pub mean_daily_min_c: Option<f64>,
    /// Symétrique, utile pour caractériser le contraste jour/nuit d'un site.
    pub mean_daily_max_c: Option<f64>,

    /// Moyenne des amplitudes quotidiennes.
    pub mean_amplitude_c: Option<f64>,
    /// Amplitude quotidienne maximale observée.
    pub max_amplitude_c: Option<f64>,
    /// Jour de l'amplitude maximale (utile pour aller regarder la courbe).
    pub max_amplitude_date: Option<NaiveDate>,
    /// Jour du minimum absolu.
    pub extreme_min_date: Option<NaiveDate>,

    pub daily: Vec<DailyStats>,
}

impl PeriodStats {
    /// Résultat d'une série vide : tout à `None`, aucun jour.
    pub fn empty() -> Self {
        PeriodStats {
            measurement_count: 0,
            day_count: 0,
            first_utc: None,
            last_utc: None,
            extreme_min_c: None,
            extreme_max_c: None,
            mean_c: None,
            mean_daily_min_c: None,
            mean_daily_max_c: None,
            mean_amplitude_c: None,
            max_amplitude_c: None,
            max_amplitude_date: None,
            extreme_min_date: None,
            daily: Vec::new(),
        }
    }
}

/// Min / moyenne / max / amplitude pour chaque jour calendaire **UTC**.
/// Le résultat est trié chronologiquement (garanti par la `BTreeMap`).
pub fn daily_stats<T: TemperatureSample>(measurements: &[T]) -> Vec<DailyStats> {
    daily_stats_with_day_offset(measurements, 0)
}

/// Variante avec décalage de découpage en secondes.
///
/// `day_offset_seconds` est retranché à l'horodatage avant d'en extraire la date.
/// Avec `0` on obtient le jour calendaire UTC (comportement du §1.5). Avec
/// `18 * 3600` on obtiendrait des « journées » démarrant à 18 h UTC, ce qui
/// regroupe une nuit d'inversion complète dans un seul seau. Non utilisé par
/// l'UI pour l'instant, mais le point d'extension est là.
pub fn daily_stats_with_day_offset<T: TemperatureSample>(
    measurements: &[T],
    day_offset_seconds: i64,
) -> Vec<DailyStats> {
    // Accumulation directe : min/max/sum/count mis à jour mesure par mesure,
    // sans allouer de Vec<f64> intermédiaire par jour.
    // Sur 477 000 mesures ROMMA (≈ 365 jours), l'ancienne version créait
    // ~365 vecteurs et faisait un second passage pour les agréger. Ici : un
    // seul passage, zéro allocation de Vec.
    struct Accum {
        min: f64,
        max: f64,
        sum: f64,
        count: usize,
    }
    let mut buckets: BTreeMap<NaiveDate, Accum> = BTreeMap::new();

    for m in measurements {
        let t = m.temperature();
        if !t.is_finite() {
            continue; // NaN / infini contamineraient min/max/moy
        }
        let shifted = m.instant() - Duration::seconds(day_offset_seconds);
        let entry = buckets
            .entry(shifted.date_naive())
            .or_insert(Accum { min: f64::INFINITY, max: f64::NEG_INFINITY, sum: 0.0, count: 0 });
        if t < entry.min { entry.min = t; }
        if t > entry.max { entry.max = t; }
        entry.sum += t;
        entry.count += 1;
    }

    buckets
        .into_iter()
        .map(|(date, a)| DailyStats {
            date,
            min_c: a.min,
            mean_c: a.sum / a.count as f64,
            max_c: a.max,
            amplitude_c: a.max - a.min,
            measurement_count: a.count,
        })
        .collect()
}

/// Minimum absolu sur la période (§1.5 « minimum extrême »).
pub fn extreme_min<T: TemperatureSample>(measurements: &[T]) -> Option<f64> {
    measurements
        .iter()
        .map(|m| m.temperature())
        .filter(|t| t.is_finite())
        .fold(None, |acc: Option<f64>, t| {
            Some(match acc {
                Some(a) if a <= t => a,
                _ => t,
            })
        })
}

/// Maximum absolu sur la période.
pub fn extreme_max<T: TemperatureSample>(measurements: &[T]) -> Option<f64> {
    measurements
        .iter()
        .map(|m| m.temperature())
        .filter(|t| t.is_finite())
        .fold(None, |acc: Option<f64>, t| {
            Some(match acc {
                Some(a) if a >= t => a,
                _ => t,
            })
        })
}

/// Moyenne des minimums quotidiens (§1.5). Sur une période d'un an c'est la
/// « moyenne annuelle des minimums quotidiens ».
pub fn mean_of_daily_minima(daily: &[DailyStats]) -> Option<f64> {
    mean(daily.iter().map(|d| d.min_c))
}

/// Moyenne des amplitudes quotidiennes (§1.5).
pub fn mean_amplitude(daily: &[DailyStats]) -> Option<f64> {
    mean(daily.iter().map(|d| d.amplitude_c))
}

/// Amplitude quotidienne maximale (§1.5).
pub fn max_amplitude(daily: &[DailyStats]) -> Option<(NaiveDate, f64)> {
    daily
        .iter()
        .fold(None, |acc: Option<(NaiveDate, f64)>, d| match acc {
            Some((_, best)) if best >= d.amplitude_c => acc,
            _ => Some((d.date, d.amplitude_c)),
        })
}

fn mean<I: Iterator<Item = f64>>(values: I) -> Option<f64> {
    let mut sum = 0.0;
    let mut n = 0usize;
    for v in values {
        if v.is_finite() {
            sum += v;
            n += 1;
        }
    }
    if n == 0 {
        None
    } else {
        Some(sum / n as f64)
    }
}

// ---------------------------------------------------------------------------
// Champs météo complémentaires (v5)
// ---------------------------------------------------------------------------
//
// ## Ce qui est fait ici, et ce qui ne l'est pas
//
// La v5 a ajouté six grandeurs sur [`Measurement`] : pression, vent moyen,
// rafale, direction, pluie, rayonnement. Elles sont **stockées et relues**, mais
// **aucune statistique n'est calculée dessus** — ni moyenne, ni extrême, ni
// corrélation avec la température.
//
// C'est délibéré, et ce n'est pas un oubli à combler mécaniquement :
//
// * les agrégats de température du §1.5 ont un sens métier arrêté (moyenne des
//   minimums quotidiens, amplitude), ceux du vent et de la pluie restent à
//   définir — une « direction de vent moyenne » n'a d'ailleurs aucun sens
//   calculée comme une moyenne arithmétique de degrés ;
// * ces grandeurs ne sont renseignées **que** par la source ROMMA. Les mélanger
//   aux statistiques d'un point qui porte aussi un logger Elitech produirait des
//   moyennes portant sur une fraction des mesures, sans que rien ne le dise.
//
// Ce que cette section fournit à la place : de quoi **savoir ce qu'on a**. Le
// jour où l'exploitation analytique sera écrite, elle partira de là.

/// Combien de mesures portent chaque champ facultatif.
///
/// Ce n'est pas une statistique mais un **inventaire** : sur une série ROMMA de
/// neuf ans, plusieurs capteurs ont été ajoutés en cours de route, et une
/// moyenne calculée sans savoir qu'elle ne porte que sur 60 % de la période
/// serait trompeuse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeatherCoverage {
    /// Mesures examinées, tous champs confondus.
    pub measurement_count: usize,
    pub humidity: usize,
    pub pressure: usize,
    pub wind_avg: usize,
    pub wind_gust: usize,
    pub wind_direction: usize,
    pub rain: usize,
    pub solar_radiation: usize,
    /// Champs techniques hérités de la v3 (stations Infoclimat).
    pub rssi: usize,
    pub battery: usize,
}

impl WeatherCoverage {
    /// `true` si au moins un champ météo de la v5 est renseigné quelque part.
    ///
    /// Sert à l'UI : inutile d'afficher un bloc « autres grandeurs » sur une
    /// série de logger, qui n'en porte aucune.
    pub fn has_weather_fields(&self) -> bool {
        self.pressure > 0
            || self.wind_avg > 0
            || self.wind_gust > 0
            || self.wind_direction > 0
            || self.rain > 0
            || self.solar_radiation > 0
    }
}

/// Inventaire des champs facultatifs présents dans une tranche de mesures.
pub fn weather_coverage(measurements: &[Measurement]) -> WeatherCoverage {
    let mut c = WeatherCoverage {
        measurement_count: measurements.len(),
        ..WeatherCoverage::default()
    };
    for m in measurements {
        c.humidity += m.humidity_percent.is_some() as usize;
        c.pressure += m.pressure_hpa.is_some() as usize;
        c.wind_avg += m.wind_avg_kmh.is_some() as usize;
        c.wind_gust += m.wind_gust_kmh.is_some() as usize;
        c.wind_direction += m.wind_direction_degrees.is_some() as usize;
        c.rain += m.rain_mm.is_some() as usize;
        c.solar_radiation += m.solar_radiation.is_some() as usize;
        c.rssi += m.rssi_dbm.is_some() as usize;
        c.battery += m.battery.is_some() as usize;
    }
    c
}

/// Calcule tous les agrégats du §1.5 en un seul passage.
///
/// La tranche n'a pas besoin d'être triée.
pub fn period_stats<T: TemperatureSample>(measurements: &[T]) -> PeriodStats {
    if measurements.is_empty() {
        return PeriodStats::empty();
    }

    let daily = daily_stats(measurements);

    let first_utc = measurements.iter().map(|m| m.instant()).min();
    let last_utc = measurements.iter().map(|m| m.instant()).max();

    let extreme_min_c = extreme_min(measurements);
    let extreme_min_date = extreme_min_c.and_then(|target| {
        daily
            .iter()
            .find(|d| d.min_c == target)
            .map(|d| d.date)
    });

    let amp_max = max_amplitude(&daily);

    PeriodStats {
        measurement_count: measurements.len(),
        day_count: daily.len(),
        first_utc,
        last_utc,
        extreme_min_c,
        extreme_max_c: extreme_max(measurements),
        mean_c: mean(measurements.iter().map(|m| m.temperature())),
        mean_daily_min_c: mean_of_daily_minima(&daily),
        mean_daily_max_c: mean(daily.iter().map(|d| d.max_c)),
        mean_amplitude_c: mean_amplitude(&daily),
        max_amplitude_c: amp_max.map(|(_, v)| v),
        max_amplitude_date: amp_max.map(|(d, _)| d),
        extreme_min_date,
        daily,
    }
}

/// Restreint une série à un intervalle `[from, to)` (bornes UTC).
pub fn filter_period<T: TemperatureSample + Clone>(
    measurements: &[T],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<T> {
    measurements
        .iter()
        .filter(|m| m.instant() >= from && m.instant() < to)
        .cloned()
        .collect()
}

/// Statistiques restreintes à une année civile UTC — c'est le cadre attendu par
/// « moyenne **annuelle** des minimums quotidiens ».
pub fn stats_for_year<T: TemperatureSample + Clone>(measurements: &[T], year: i32) -> PeriodStats {
    let subset: Vec<T> = measurements
        .iter()
        .filter(|m| m.instant().year() == year)
        .cloned()
        .collect();
    period_stats(&subset)
}

// ---------------------------------------------------------------------------
// Agrégation pour l'affichage (bloc « Croisement de données »)
// ---------------------------------------------------------------------------
//
// ## Le problème
//
// Un point relié à ROMMA porte 476 912 mesures sur neuf ans. Superposer huit
// courbes de cette taille, c'est 3,8 millions de points à sérialiser en JSON,
// à faire traverser l'IPC Tauri, puis à rastériser dans un panneau de 400 px de
// large. Illisible, et lent des deux côtés.
//
// ## La règle retenue
//
// On n'envoie jamais la mesure brute au frontend : on envoie **des seaux**, dont
// la largeur dépend de l'étendue de la fenêtre demandée, et jamais du nombre de
// mesures qu'elle contient. Deux points aux pas d'échantillonnage différents
// (un logger toutes les 10 min, une station horaire) produisent donc des
// courbes **alignées sur les mêmes seaux** — condition sine qua non pour que la
// superposition veuille dire quelque chose.
//
// | Étendue de la fenêtre | Granularité | Seaux (majorant) |
// |---|---|---|
// | ≤ 10 jours            | horaire     | 240   |
// | ≤ 400 jours           | quotidienne | 400   |
// | ≤ 3 ans               | hebdomadaire| ~157  |
// | au-delà               | mensuelle   | ~12/an|
//
// Le majorant tient dans tous les cas sous ~400 points par courbe, soit ~3 200
// pour huit points sélectionnés : négligeable sur l'IPC comme au rendu.
//
// ## Ce que porte un seau
//
// Moyenne **et** min **et** max. La moyenne seule lisserait précisément ce qu'on
// cherche : sur un seau quotidien, un TAF se reconnaît à son minimum, pas à sa
// moyenne. La courbe trace la moyenne, l'infobulle donne les trois valeurs, et
// le min reste disponible pour tracer une enveloppe.

/// Largeur des seaux d'agrégation d'une courbe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Granularity {
    Hour,
    Day,
    Week,
    Month,
}

impl Granularity {
    /// Libellé destiné à l'UI (aucune logique ne doit en dépendre).
    pub fn label_fr(self) -> &'static str {
        match self {
            Granularity::Hour => "moyennes horaires",
            Granularity::Day => "moyennes quotidiennes",
            Granularity::Week => "moyennes hebdomadaires",
            Granularity::Month => "moyennes mensuelles",
        }
    }

    /// Début du seau contenant `instant`, en UTC.
    ///
    /// Les frontières sont **calendaires**, pas « il y a N heures » : un seau
    /// quotidien commence à minuit UTC, un seau hebdomadaire un lundi, un seau
    /// mensuel le 1er. C'est ce qui garantit que deux séries échantillonnées
    /// différemment tombent dans les mêmes seaux.
    pub fn bucket_start(self, instant: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Granularity::Hour => {
                let secs = instant.timestamp();
                Utc.timestamp_opt(secs - secs.rem_euclid(3600), 0)
                    .single()
                    .unwrap_or(instant)
            }
            Granularity::Day => midnight(instant.date_naive()),
            Granularity::Week => {
                let date = instant.date_naive();
                let depuis_lundi = date.weekday().num_days_from_monday() as i64;
                midnight(date - Duration::days(depuis_lundi))
            }
            Granularity::Month => {
                let date = instant.date_naive();
                midnight(
                    NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap_or(date),
                )
            }
        }
    }
}

fn midnight(date: NaiveDate) -> DateTime<Utc> {
    DateTime::from_naive_utc_and_offset(
        date.and_hms_opt(0, 0, 0)
            .expect("minuit est une heure valide de toute date"),
        Utc,
    )
}

/// Bornes de bascule d'une granularité à la suivante, en jours d'étendue.
/// Constantes nommées plutôt que littéraux : ce sont les seuls chiffres à
/// bouger si l'affichage se révèle trop dense ou trop grossier à l'usage.
pub const SEUIL_HORAIRE_JOURS: i64 = 10;
pub const SEUIL_QUOTIDIEN_JOURS: i64 = 400;
pub const SEUIL_HEBDOMADAIRE_JOURS: i64 = 3 * 365;

/// Granularité adaptée à une fenêtre `[from, to)`.
///
/// Ne dépend **que** de l'étendue demandée, jamais du volume trouvé dedans :
/// deux points comparés sur la même fenêtre doivent produire des seaux
/// identiques, même si l'un porte dix fois plus de mesures que l'autre.
pub fn pick_granularity(from: DateTime<Utc>, to: DateTime<Utc>) -> Granularity {
    let jours = (to - from).num_days().max(0);
    if jours <= SEUIL_HORAIRE_JOURS {
        Granularity::Hour
    } else if jours <= SEUIL_QUOTIDIEN_JOURS {
        Granularity::Day
    } else if jours <= SEUIL_HEBDOMADAIRE_JOURS {
        Granularity::Week
    } else {
        Granularity::Month
    }
}

/// Un seau de courbe agrégée.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    /// Début du seau, en UTC (frontière calendaire — cf. [`Granularity::bucket_start`]).
    pub bucket_start_utc: DateTime<Utc>,
    pub mean_c: f64,
    pub min_c: f64,
    pub max_c: f64,
    pub sample_count: usize,
}

/// Agrège une série en seaux de largeur `granularity`.
///
/// * Le résultat est trié chronologiquement (`BTreeMap`).
/// * Un seau **sans aucune mesure n'apparaît pas** : la courbe porte donc ses
///   trous, elle ne les interpole pas. Le frontend les rend comme des coupures,
///   ce qui est la seule lecture honnête d'un logger qui a cessé d'enregistrer.
/// * Les `NaN` sont écartés, exactement comme dans [`daily_stats`].
pub fn aggregate_curve<T: TemperatureSample>(
    samples: &[T],
    granularity: Granularity,
) -> Vec<CurvePoint> {
    // (somme, n, min, max) par seau : un seul passage, sans conserver les
    // valeurs individuelles — c'est ce qui rend l'opération tenable sur 477 000
    // mesures.
    let mut buckets: BTreeMap<DateTime<Utc>, (f64, usize, f64, f64)> = BTreeMap::new();

    for s in samples {
        let t = s.temperature();
        if !t.is_finite() {
            continue;
        }
        let cle = granularity.bucket_start(s.instant());
        let e = buckets
            .entry(cle)
            .or_insert((0.0, 0, f64::INFINITY, f64::NEG_INFINITY));
        e.0 += t;
        e.1 += 1;
        if t < e.2 {
            e.2 = t;
        }
        if t > e.3 {
            e.3 = t;
        }
    }

    buckets
        .into_iter()
        .map(|(bucket_start_utc, (somme, n, min, max))| CurvePoint {
            bucket_start_utc,
            mean_c: somme / n as f64,
            min_c: min,
            max_c: max,
            sample_count: n,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Agrégats journaliers (table `series_days`, schéma v11)
// ---------------------------------------------------------------------------
//
// Sur une fenêtre de plus de dix jours, aucune statistique ni aucune courbe n'a
// besoin des mesures elles-mêmes : tout se déduit, **exactement**, de quatre
// nombres par jour calendaire UTC (nombre, somme, min, max) et des deux
// instants extrêmes. On stocke la somme et non la moyenne : les jours se
// regroupent alors sans erreur en semaines et en mois, et plusieurs séries d'un
// même point se combinent comme le faisait la lecture brute.
//
// Seul écart possible avec le calcul sur mesures brutes : l'ordre des
// additions, donc le dernier chiffre d'une moyenne (de l'ordre de 1e-12 °C).

/// Un jour calendaire UTC d'une série (ou d'un point, séries combinées).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DayAgg {
    /// Jours écoulés depuis le 1970-01-01 (UTC).
    pub day: i64,
    pub n: usize,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub first_utc: DateTime<Utc>,
    pub last_utc: DateTime<Utc>,
}

impl DayAgg {
    pub fn date(&self) -> NaiveDate {
        NaiveDate::from_num_days_from_ce_opt(self.day as i32 + 719_163).unwrap_or_default()
    }
}

/// Numéro de jour UTC (depuis 1970-01-01) d'un instant.
pub fn day_number(instant: DateTime<Utc>) -> i64 {
    instant.timestamp().div_euclid(86_400)
}

/// Agrégats journaliers calculés depuis des mesures brutes — même découpage
/// que [`daily_stats`]. Sert aux bords d'une fenêtre (jours entamés) et aux
/// tests d'équivalence.
pub fn day_aggs<T: TemperatureSample>(samples: &[T]) -> Vec<DayAgg> {
    let mut jours: BTreeMap<i64, DayAgg> = BTreeMap::new();
    for s in samples {
        let t = s.temperature();
        if !t.is_finite() {
            continue;
        }
        let i = s.instant();
        let d = jours.entry(day_number(i)).or_insert(DayAgg {
            day: day_number(i),
            n: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            first_utc: i,
            last_utc: i,
        });
        d.n += 1;
        d.sum += t;
        d.min = d.min.min(t);
        d.max = d.max.max(t);
        d.first_utc = d.first_utc.min(i);
        d.last_utc = d.last_utc.max(i);
    }
    jours.into_values().collect()
}

/// [`period_stats`] calculé depuis des agrégats journaliers triés par jour.
pub fn period_stats_from_days(days: &[DayAgg]) -> PeriodStats {
    let days: Vec<&DayAgg> = days.iter().filter(|d| d.n > 0).collect();
    if days.is_empty() {
        return PeriodStats::empty();
    }
    let daily: Vec<DailyStats> = days
        .iter()
        .map(|d| DailyStats {
            date: d.date(),
            min_c: d.min,
            mean_c: d.sum / d.n as f64,
            max_c: d.max,
            amplitude_c: d.max - d.min,
            measurement_count: d.n,
        })
        .collect();

    let n: usize = days.iter().map(|d| d.n).sum();
    let somme: f64 = days.iter().map(|d| d.sum).sum();
    let extreme_min_c = days.iter().map(|d| d.min).fold(None, |a: Option<f64>, t| Some(a.map_or(t, |a| a.min(t))));
    let extreme_max_c = days.iter().map(|d| d.max).fold(None, |a: Option<f64>, t| Some(a.map_or(t, |a| a.max(t))));
    let extreme_min_date = extreme_min_c.and_then(|cible| daily.iter().find(|d| d.min_c == cible).map(|d| d.date));
    let amp_max = max_amplitude(&daily);

    PeriodStats {
        measurement_count: n,
        day_count: daily.len(),
        first_utc: days.iter().map(|d| d.first_utc).min(),
        last_utc: days.iter().map(|d| d.last_utc).max(),
        extreme_min_c,
        extreme_max_c,
        mean_c: Some(somme / n as f64),
        mean_daily_min_c: mean_of_daily_minima(&daily),
        mean_daily_max_c: mean(daily.iter().map(|d| d.max_c)),
        mean_amplitude_c: mean_amplitude(&daily),
        max_amplitude_c: amp_max.map(|(_, v)| v),
        max_amplitude_date: amp_max.map(|(d, _)| d),
        extreme_min_date,
        daily,
    }
}

/// [`aggregate_curve`] calculé depuis des agrégats journaliers. Valable pour
/// les granularités jour, semaine et mois : un seau horaire ne se déduit pas
/// d'un jour.
pub fn aggregate_curve_from_days(days: &[DayAgg], granularity: Granularity) -> Vec<CurvePoint> {
    debug_assert!(granularity != Granularity::Hour);
    let mut seaux: BTreeMap<DateTime<Utc>, (f64, usize, f64, f64)> = BTreeMap::new();
    for d in days.iter().filter(|d| d.n > 0) {
        let e = seaux
            .entry(granularity.bucket_start(midnight(d.date())))
            .or_insert((0.0, 0, f64::INFINITY, f64::NEG_INFINITY));
        e.0 += d.sum;
        e.1 += d.n;
        e.2 = e.2.min(d.min);
        e.3 = e.3.max(d.max);
    }
    seaux
        .into_iter()
        .map(|(bucket_start_utc, (somme, n, min, max))| CurvePoint {
            bucket_start_utc,
            mean_c: somme / n as f64,
            min_c: min,
            max_c: max,
            sample_count: n,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn m(iso_utc: &str, temp: f64) -> Measurement {
        let ts: DateTime<Utc> = iso_utc.parse().expect("horodatage de test invalide");
        Measurement::new_utc("serie-test", ts, temp)
    }

    fn approx(a: f64, b: f64) {
        assert!(
            (a - b).abs() < 1e-9,
            "attendu {b}, obtenu {a} (écart {})",
            (a - b).abs()
        );
    }

    /// Jeu de données synthétique, valeurs connues d'avance :
    ///
    /// | jour       | mesures      | min | moy | max | amplitude |
    /// |------------|--------------|-----|-----|-----|-----------|
    /// | 2024-01-01 | -5, 0, 5     | -5  |  0  |  5  |   10      |
    /// | 2024-01-02 | -10, -8, -6  | -10 | -8  | -6  |    4      |
    /// | 2024-01-03 | 1, 2, 3      |  1  |  2  |  3  |    2      |
    fn jeu_synthetique() -> Vec<Measurement> {
        vec![
            m("2024-01-01T00:00:00Z", -5.0),
            m("2024-01-01T06:00:00Z", 0.0),
            m("2024-01-01T14:00:00Z", 5.0),
            m("2024-01-02T00:00:00Z", -10.0),
            m("2024-01-02T06:00:00Z", -8.0),
            m("2024-01-02T14:00:00Z", -6.0),
            m("2024-01-03T00:00:00Z", 1.0),
            m("2024-01-03T06:00:00Z", 2.0),
            m("2024-01-03T14:00:00Z", 3.0),
        ]
    }

    #[test]
    fn min_moy_max_quotidiens() {
        let daily = daily_stats(&jeu_synthetique());
        assert_eq!(daily.len(), 3);

        assert_eq!(daily[0].date, NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
        approx(daily[0].min_c, -5.0);
        approx(daily[0].mean_c, 0.0);
        approx(daily[0].max_c, 5.0);
        assert_eq!(daily[0].measurement_count, 3);

        approx(daily[1].min_c, -10.0);
        approx(daily[1].mean_c, -8.0);
        approx(daily[1].max_c, -6.0);

        approx(daily[2].min_c, 1.0);
        approx(daily[2].mean_c, 2.0);
        approx(daily[2].max_c, 3.0);
    }

    #[test]
    fn amplitudes_quotidiennes() {
        let daily = daily_stats(&jeu_synthetique());
        approx(daily[0].amplitude_c, 10.0);
        approx(daily[1].amplitude_c, 4.0);
        approx(daily[2].amplitude_c, 2.0);
    }

    #[test]
    fn minimum_extreme() {
        approx(extreme_min(&jeu_synthetique()).unwrap(), -10.0);
        approx(extreme_max(&jeu_synthetique()).unwrap(), 5.0);
    }

    #[test]
    fn moyenne_des_minimums_quotidiens() {
        let daily = daily_stats(&jeu_synthetique());
        // (-5 + -10 + 1) / 3 = -14/3
        approx(mean_of_daily_minima(&daily).unwrap(), -14.0 / 3.0);
    }

    #[test]
    fn amplitude_moyenne_et_maximale() {
        let daily = daily_stats(&jeu_synthetique());
        // (10 + 4 + 2) / 3 = 16/3
        approx(mean_amplitude(&daily).unwrap(), 16.0 / 3.0);
        let (date, value) = max_amplitude(&daily).unwrap();
        approx(value, 10.0);
        assert_eq!(date, NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
    }

    #[test]
    fn agregat_complet() {
        let s = period_stats(&jeu_synthetique());
        assert_eq!(s.measurement_count, 9);
        assert_eq!(s.day_count, 3);
        approx(s.extreme_min_c.unwrap(), -10.0);
        approx(s.extreme_max_c.unwrap(), 5.0);
        approx(s.mean_c.unwrap(), (-5.0 + 0.0 + 5.0 - 10.0 - 8.0 - 6.0 + 1.0 + 2.0 + 3.0) / 9.0);
        approx(s.mean_daily_min_c.unwrap(), -14.0 / 3.0);
        approx(s.mean_daily_max_c.unwrap(), (5.0 - 6.0 + 3.0) / 3.0);
        approx(s.mean_amplitude_c.unwrap(), 16.0 / 3.0);
        approx(s.max_amplitude_c.unwrap(), 10.0);
        assert_eq!(
            s.max_amplitude_date,
            Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
        );
        assert_eq!(
            s.extreme_min_date,
            Some(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap())
        );
        assert_eq!(
            s.first_utc,
            Some("2024-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap())
        );
        assert_eq!(
            s.last_utc,
            Some("2024-01-03T14:00:00Z".parse::<DateTime<Utc>>().unwrap())
        );
    }

    #[test]
    fn serie_vide_ne_panique_pas() {
        let s = period_stats::<Measurement>(&[]);
        assert_eq!(s.measurement_count, 0);
        assert_eq!(s.day_count, 0);
        assert!(s.extreme_min_c.is_none());
        assert!(s.mean_daily_min_c.is_none());
        assert!(s.max_amplitude_c.is_none());
        assert!(s.daily.is_empty());
    }

    #[test]
    fn jour_unique_amplitude_nulle_si_une_seule_mesure() {
        let s = period_stats(&[m("2024-03-01T02:00:00Z", -12.5)]);
        assert_eq!(s.day_count, 1);
        approx(s.extreme_min_c.unwrap(), -12.5);
        approx(s.max_amplitude_c.unwrap(), 0.0);
        approx(s.mean_daily_min_c.unwrap(), -12.5);
    }

    #[test]
    fn decoupage_sur_le_jour_utc() {
        // 23:30 et 00:30 encadrent minuit UTC → deux journées distinctes.
        let daily = daily_stats(&[
            m("2024-01-01T23:30:00Z", -20.0),
            m("2024-01-02T00:30:00Z", -22.0),
        ]);
        assert_eq!(daily.len(), 2);
        approx(daily[0].min_c, -20.0);
        approx(daily[1].min_c, -22.0);

        // Avec un découpage démarrant à 18 h, la même nuit tient dans un seul seau.
        let nuit = daily_stats_with_day_offset(
            &[
                m("2024-01-01T23:30:00Z", -20.0),
                m("2024-01-02T00:30:00Z", -22.0),
            ],
            18 * 3600,
        );
        assert_eq!(nuit.len(), 1);
        approx(nuit[0].min_c, -22.0);
        approx(nuit[0].amplitude_c, 2.0);
    }

    #[test]
    fn desordre_chronologique_sans_effet() {
        let mut data = jeu_synthetique();
        data.reverse();
        let s = period_stats(&data);
        assert_eq!(s.day_count, 3);
        approx(s.mean_daily_min_c.unwrap(), -14.0 / 3.0);
        assert_eq!(s.daily[0].date, NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
    }

    #[test]
    fn filtrage_de_periode_et_annee() {
        let mut data = jeu_synthetique();
        data.push(m("2023-12-31T12:00:00Z", 40.0));

        let s2024 = stats_for_year(&data, 2024);
        assert_eq!(s2024.measurement_count, 9);
        approx(s2024.extreme_max_c.unwrap(), 5.0);

        let s2023 = stats_for_year(&data, 2023);
        assert_eq!(s2023.measurement_count, 1);

        let sub = filter_period(
            &data,
            "2024-01-02T00:00:00Z".parse().unwrap(),
            "2024-01-03T00:00:00Z".parse().unwrap(),
        );
        assert_eq!(sub.len(), 3);
        approx(period_stats(&sub).extreme_min_c.unwrap(), -10.0);
    }

    #[test]
    fn moyenne_annuelle_des_minimums_sur_365_jours() {
        // Un an de mesures, un minimum quotidien qui vaut exactement -jour/10.
        let mut data = Vec::new();
        let start = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        let mut expected_sum = 0.0;
        for day in 0..365i64 {
            let date = start + Duration::days(day);
            let min = -(day as f64) / 10.0;
            expected_sum += min;
            data.push(m(
                &format!("{}T03:00:00Z", date.format("%Y-%m-%d")),
                min,
            ));
            data.push(m(
                &format!("{}T13:00:00Z", date.format("%Y-%m-%d")),
                min + 8.0,
            ));
        }

        let s = stats_for_year(&data, 2025);
        assert_eq!(s.day_count, 365);
        approx(s.mean_daily_min_c.unwrap(), expected_sum / 365.0);
        approx(s.mean_amplitude_c.unwrap(), 8.0);
        approx(s.max_amplitude_c.unwrap(), 8.0);
    }

    /// L'inventaire des champs facultatifs, sur un mélange réaliste : une série
    /// de logger (température seule) et une série ROMMA (six grandeurs de plus).
    #[test]
    fn inventaire_des_champs_meteo() {
        let logger = m("2026-01-05T07:00:00Z", -12.5);
        let romma = Measurement {
            humidity_percent: Some(72.0),
            pressure_hpa: Some(1021.0),
            wind_avg_kmh: Some(10.0),
            wind_gust_kmh: Some(24.1),
            wind_direction_degrees: Some(225.0),
            rain_mm: Some(0.0),
            solar_radiation: Some(412.0),
            ..m("2026-01-05T07:10:00Z", -12.0)
        };

        let seul_logger = weather_coverage(&[logger.clone()]);
        assert_eq!(seul_logger.measurement_count, 1);
        assert!(
            !seul_logger.has_weather_fields(),
            "une série de logger ne porte aucune grandeur météo"
        );

        let melange = weather_coverage(&[logger, romma]);
        assert_eq!(melange.measurement_count, 2);
        assert!(melange.has_weather_fields());
        // Une seule des deux mesures porte ces champs : c'est exactement ce que
        // l'inventaire doit rendre visible avant qu'on en tire une moyenne.
        assert_eq!(melange.pressure, 1);
        assert_eq!(melange.wind_avg, 1);
        assert_eq!(melange.wind_gust, 1);
        assert_eq!(melange.wind_direction, 1);
        assert_eq!(melange.rain, 1);
        assert_eq!(melange.solar_radiation, 1);
        assert_eq!(melange.humidity, 1);
        assert_eq!(melange.rssi, 0);
    }

    /// Une mesure ROMMA à `0.0` mm de pluie est renseignée, pas absente : compter
    /// « les valeurs non nulles » au lieu des `Some` fausserait tout.
    #[test]
    fn un_zero_est_une_valeur_renseignee() {
        let zero = Measurement {
            rain_mm: Some(0.0),
            solar_radiation: Some(0.0),
            ..m("2026-01-05T02:00:00Z", -8.0)
        };
        let c = weather_coverage(&[zero]);
        assert_eq!(c.rain, 1);
        assert_eq!(c.solar_radiation, 1);
    }

    #[test]
    fn inventaire_d_une_serie_vide() {
        let c = weather_coverage(&[]);
        assert_eq!(c.measurement_count, 0);
        assert!(!c.has_weather_fields());
    }

    // -----------------------------------------------------------------------
    // Échantillon léger et agrégation de courbe
    // -----------------------------------------------------------------------

    fn e(iso_utc: &str, temp: f64) -> TempSample {
        TempSample::new(iso_utc.parse().expect("horodatage de test invalide"), temp)
    }

    fn t(iso_utc: &str) -> DateTime<Utc> {
        iso_utc.parse().expect("horodatage de test invalide")
    }

    /// Le seul risque de la généralisation : que `TempSample` et `Measurement`
    /// ne donnent pas le même résultat. Le jeu synthétique passe donc par les
    /// deux chemins, et les deux `PeriodStats` doivent être identiques champ
    /// pour champ.
    #[test]
    fn les_deux_entrees_donnent_les_memes_statistiques() {
        let mesures = jeu_synthetique();
        let legers: Vec<TempSample> = mesures
            .iter()
            .map(|m| TempSample::new(m.timestamp_utc, m.temperature_celsius))
            .collect();

        assert_eq!(period_stats(&mesures), period_stats(&legers));
        assert_eq!(daily_stats(&mesures), daily_stats(&legers));
        assert_eq!(extreme_min(&mesures), extreme_min(&legers));
    }

    #[test]
    fn granularite_choisie_sur_l_etendue_pas_sur_le_volume() {
        let debut = t("2024-01-01T00:00:00Z");
        assert_eq!(
            pick_granularity(debut, debut + Duration::days(7)),
            Granularity::Hour
        );
        assert_eq!(
            pick_granularity(debut, debut + Duration::days(SEUIL_HORAIRE_JOURS)),
            Granularity::Hour
        );
        assert_eq!(
            pick_granularity(debut, debut + Duration::days(SEUIL_HORAIRE_JOURS + 1)),
            Granularity::Day
        );
        assert_eq!(
            pick_granularity(debut, debut + Duration::days(30)),
            Granularity::Day
        );
        // Une année civile complète reste en quotidien : 366 seaux se lisent.
        assert_eq!(
            pick_granularity(debut, debut + Duration::days(366)),
            Granularity::Day
        );
        assert_eq!(
            pick_granularity(debut, debut + Duration::days(SEUIL_QUOTIDIEN_JOURS + 1)),
            Granularity::Week
        );
        // Neuf ans d'archive ROMMA.
        assert_eq!(
            pick_granularity(debut, debut + Duration::days(9 * 365)),
            Granularity::Month
        );
        // Fenêtre dégénérée (bornes inversées) : pas de panique, pas de division.
        assert_eq!(pick_granularity(debut, debut), Granularity::Hour);
        assert_eq!(
            pick_granularity(debut, debut - Duration::days(5)),
            Granularity::Hour
        );
    }

    #[test]
    fn les_seaux_horaires_sont_alignes_sur_l_heure_ronde() {
        // Quatre mesures dans la même heure, une dans la suivante.
        let courbe = aggregate_curve(
            &[
                e("2026-02-03T08:03:00Z", -4.0),
                e("2026-02-03T08:17:00Z", -6.0),
                e("2026-02-03T08:31:00Z", -2.0),
                e("2026-02-03T08:59:59Z", 0.0),
                e("2026-02-03T09:00:00Z", 5.0),
            ],
            Granularity::Hour,
        );

        assert_eq!(courbe.len(), 2);
        assert_eq!(courbe[0].bucket_start_utc, t("2026-02-03T08:00:00Z"));
        assert_eq!(courbe[0].sample_count, 4);
        approx(courbe[0].mean_c, -3.0);
        approx(courbe[0].min_c, -6.0);
        approx(courbe[0].max_c, 0.0);

        assert_eq!(courbe[1].bucket_start_utc, t("2026-02-03T09:00:00Z"));
        assert_eq!(courbe[1].sample_count, 1);
        approx(courbe[1].mean_c, 5.0);
        approx(courbe[1].min_c, 5.0);
        approx(courbe[1].max_c, 5.0);
    }

    /// Le seau quotidien doit redonner exactement les mêmes min/moy/max que
    /// `daily_stats` — sinon la zone du haut et le graphique du bas
    /// raconteraient deux histoires différentes sur la même fenêtre.
    #[test]
    fn le_seau_quotidien_coincide_avec_daily_stats() {
        let mesures = jeu_synthetique();
        let courbe = aggregate_curve(&mesures, Granularity::Day);
        let quotidien = daily_stats(&mesures);

        assert_eq!(courbe.len(), quotidien.len());
        for (c, d) in courbe.iter().zip(quotidien.iter()) {
            assert_eq!(c.bucket_start_utc.date_naive(), d.date);
            approx(c.min_c, d.min_c);
            approx(c.mean_c, d.mean_c);
            approx(c.max_c, d.max_c);
            assert_eq!(c.sample_count, d.measurement_count);
        }
    }

    #[test]
    fn le_seau_hebdomadaire_commence_un_lundi() {
        // 2026-02-03 est un mardi ; le lundi de sa semaine est le 2026-02-02.
        let courbe = aggregate_curve(
            &[
                e("2026-02-02T00:00:00Z", 1.0), // lundi
                e("2026-02-03T12:00:00Z", 3.0), // mardi
                e("2026-02-08T23:00:00Z", 5.0), // dimanche, même semaine
                e("2026-02-09T01:00:00Z", 9.0), // lundi suivant
            ],
            Granularity::Week,
        );

        assert_eq!(courbe.len(), 2);
        assert_eq!(courbe[0].bucket_start_utc, t("2026-02-02T00:00:00Z"));
        assert_eq!(courbe[0].sample_count, 3);
        approx(courbe[0].mean_c, 3.0);
        assert_eq!(courbe[1].bucket_start_utc, t("2026-02-09T00:00:00Z"));
        assert_eq!(courbe[1].sample_count, 1);
    }

    #[test]
    fn le_seau_mensuel_commence_le_premier() {
        let courbe = aggregate_curve(
            &[
                e("2025-12-31T23:00:00Z", -10.0),
                e("2026-01-01T00:00:00Z", -20.0),
                e("2026-01-31T23:59:00Z", 0.0),
                e("2026-02-01T00:00:00Z", 4.0),
            ],
            Granularity::Month,
        );

        assert_eq!(courbe.len(), 3);
        assert_eq!(courbe[0].bucket_start_utc, t("2025-12-01T00:00:00Z"));
        assert_eq!(courbe[1].bucket_start_utc, t("2026-01-01T00:00:00Z"));
        assert_eq!(courbe[1].sample_count, 2);
        approx(courbe[1].mean_c, -10.0);
        approx(courbe[1].min_c, -20.0);
        assert_eq!(courbe[2].bucket_start_utc, t("2026-02-01T00:00:00Z"));
    }

    /// Deux points échantillonnés différemment (10 min contre 1 h) doivent
    /// tomber dans **les mêmes seaux** : c'est la condition pour que la
    /// superposition des courbes soit comparable point à point.
    #[test]
    fn deux_pas_d_echantillonnage_differents_partagent_les_memes_seaux() {
        let mut dense = Vec::new();
        let mut clairseme = Vec::new();
        let depart = t("2026-03-01T00:00:00Z");
        for i in 0..(24 * 6) {
            dense.push(TempSample::new(depart + Duration::minutes(10 * i), -3.0));
        }
        for i in 0..24 {
            clairseme.push(TempSample::new(depart + Duration::hours(i), -8.0));
        }

        let a = aggregate_curve(&dense, Granularity::Hour);
        let b = aggregate_curve(&clairseme, Granularity::Hour);
        assert_eq!(a.len(), 24);
        assert_eq!(b.len(), 24);
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.bucket_start_utc, y.bucket_start_utc);
        }
        assert_eq!(a[0].sample_count, 6);
        assert_eq!(b[0].sample_count, 1);
    }

    /// Un seau vide n'est pas comblé : la courbe porte le trou. Une interpolation
    /// silencieuse ferait croire à un logger qui a continué d'enregistrer.
    #[test]
    fn un_trou_de_mesures_reste_un_trou() {
        let courbe = aggregate_curve(
            &[
                e("2026-01-01T00:00:00Z", -5.0),
                // rien du 2 au 9 janvier
                e("2026-01-10T00:00:00Z", -7.0),
            ],
            Granularity::Day,
        );
        assert_eq!(courbe.len(), 2);
        assert_eq!(
            (courbe[1].bucket_start_utc - courbe[0].bucket_start_utc).num_days(),
            9
        );
    }

    #[test]
    fn agregation_d_une_serie_vide_ou_non_finie() {
        assert!(aggregate_curve::<TempSample>(&[], Granularity::Day).is_empty());

        // Un NaN ne crée pas de seau, et ne contamine pas celui de son jour.
        let courbe = aggregate_curve(
            &[
                e("2026-01-01T00:00:00Z", f64::NAN),
                e("2026-01-01T01:00:00Z", -5.0),
            ],
            Granularity::Day,
        );
        assert_eq!(courbe.len(), 1);
        assert_eq!(courbe[0].sample_count, 1);
        approx(courbe[0].mean_c, -5.0);
    }

    /// Le volume réel de l'archive ROMMA (neuf ans à un pas de dix minutes) doit
    /// tenir dans une poignée de seaux — c'est tout l'intérêt de l'opération.
    #[test]
    fn neuf_ans_de_mesures_tiennent_en_une_centaine_de_seaux() {
        let depart = t("2017-06-01T00:00:00Z");
        let mut serie = Vec::with_capacity(477_000);
        for i in 0..477_000i64 {
            serie.push(TempSample::new(
                depart + Duration::minutes(10 * i),
                (i % 400) as f64 / 10.0 - 20.0,
            ));
        }

        let g = pick_granularity(serie[0].timestamp_utc, serie.last().unwrap().timestamp_utc);
        assert_eq!(g, Granularity::Month);

        let courbe = aggregate_curve(&serie, g);
        assert!(
            courbe.len() < 150,
            "neuf ans en seaux mensuels : {} seaux, attendu ~110",
            courbe.len()
        );
        assert_eq!(
            courbe.iter().map(|c| c.sample_count).sum::<usize>(),
            477_000,
            "aucune mesure ne doit se perdre dans l'agrégation"
        );
        assert_eq!(courbe[0].bucket_start_utc, t("2017-06-01T00:00:00Z"));
    }

    /// Statistiques et courbes tirées des jours = celles tirées des mesures
    /// brutes, sur une série irrégulière avec trous, valeurs négatives et
    /// minimum atteint deux fois.
    #[test]
    fn jours_et_mesures_brutes_donnent_les_memes_resultats() {
        let depart = t("2024-02-27T05:17:00Z");
        let mut serie = Vec::new();
        for i in 0..40_000i64 {
            if (i / 500) % 7 == 3 {
                continue; // trous
            }
            let v = ((i * 7919) % 613) as f64 / 10.0 - 30.0;
            serie.push(TempSample::new(depart + Duration::minutes(10 * i + (i % 3)), v));
        }
        let brut = period_stats(&serie);
        let jours = period_stats_from_days(&day_aggs(&serie));

        assert_eq!(jours.measurement_count, brut.measurement_count);
        assert_eq!(jours.day_count, brut.day_count);
        assert_eq!((jours.first_utc, jours.last_utc), (brut.first_utc, brut.last_utc));
        assert_eq!(jours.extreme_min_c, brut.extreme_min_c);
        assert_eq!(jours.extreme_max_c, brut.extreme_max_c);
        assert_eq!(jours.extreme_min_date, brut.extreme_min_date);
        assert_eq!(jours.max_amplitude_date, brut.max_amplitude_date);
        assert_eq!(jours.max_amplitude_c, brut.max_amplitude_c);
        for (a, b) in [
            (jours.mean_c, brut.mean_c),
            (jours.mean_daily_min_c, brut.mean_daily_min_c),
            (jours.mean_daily_max_c, brut.mean_daily_max_c),
            (jours.mean_amplitude_c, brut.mean_amplitude_c),
        ] {
            approx(a.unwrap(), b.unwrap());
        }
        assert_eq!(jours.daily.len(), brut.daily.len());
        for (a, b) in jours.daily.iter().zip(&brut.daily) {
            assert_eq!((a.date, a.min_c, a.max_c, a.measurement_count), (b.date, b.min_c, b.max_c, b.measurement_count));
            approx(a.mean_c, b.mean_c);
        }

        for g in [Granularity::Day, Granularity::Week, Granularity::Month] {
            let a = aggregate_curve_from_days(&day_aggs(&serie), g);
            let b = aggregate_curve(&serie, g);
            assert_eq!(a.len(), b.len(), "{g:?}");
            for (x, y) in a.iter().zip(&b) {
                assert_eq!((x.bucket_start_utc, x.min_c, x.max_c, x.sample_count), (y.bucket_start_utc, y.min_c, y.max_c, y.sample_count));
                approx(x.mean_c, y.mean_c);
            }
        }
    }

    #[test]
    fn numero_de_jour_et_date() {
        let d = day_aggs(&[e("2026-10-02T23:59:59Z", 1.0), e("1969-12-31T12:00:00Z", 2.0)]);
        assert_eq!(d[0].day, -1);
        assert_eq!(d[0].date(), NaiveDate::from_ymd_opt(1969, 12, 31).unwrap());
        assert_eq!(d[1].date(), NaiveDate::from_ymd_opt(2026, 10, 2).unwrap());
    }
}

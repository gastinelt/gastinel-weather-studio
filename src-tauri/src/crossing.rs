//! Croisement de données : ce que le bloc bas-gauche affiche pour une sélection
//! de points et une fenêtre temporelle.
//!
//! Ce module est **la** source du contenu de ce bloc. Il ne connaît ni Tauri ni
//! le DOM : il produit une [`CrossAnalysis`], un document complet et autonome
//! (métadonnées des points, statistiques de fenêtre, courbes agrégées). La
//! commande Tauri de `commands.rs` ne fait que l'appeler et le sérialiser.
//!
//! C'est délibéré, pour deux raisons :
//!
//! * l'export (CSV / TXT / HTML, chantier séparé) aura besoin exactement de ces
//!   données-là. Il appellera [`build`] et écrira le format voulu, sans
//!   redemander quoi que ce soit à la base ni recalculer une statistique ;
//! * la résolution de fenêtre ([`resolve_window`]) est arithmétique pure, donc
//!   testable sans base de données — et c'est l'endroit où une erreur passerait
//!   le plus facilement inaperçue.
//!
//! ## La décision qui mérite d'être discutée : l'ancre des fenêtres relatives
//!
//! « Les 7 derniers jours » se compte à partir de **la dernière mesure
//! disponible dans la sélection**, pas à partir de l'horloge du poste.
//!
//! La lecture littérale (« les 7 jours qui précèdent maintenant ») rendrait le
//! bloc inutilisable sur son cas d'usage principal : un logger relevé une fois
//! par saison, dont le dernier fichier date de mars, afficherait un graphique
//! vide en août. Avec l'ancre sur la donnée, la même sélection montre les sept
//! derniers jours **enregistrés**.
//!
//! La contrepartie, c'est qu'un libellé seul serait trompeur. La fenêtre résolue
//! ([`ResolvedWindow::from_utc`] / [`ResolvedWindow::to_utc`]) est donc toujours
//! renvoyée, et l'UI l'affiche en toutes lettres à côté du sélecteur.
//!
//! ## Fenêtre unique
//!
//! Les statistiques du haut et la courbe du bas sortent du **même** vecteur
//! d'échantillons, lu une seule fois par point. Il est structurellement
//! impossible qu'elles portent sur deux périodes différentes.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::db::{Db, PointRecord};
use crate::error::AppResult;
use crate::model::{Category, Point, PointSize};
use crate::stats::{
    aggregate_curve, aggregate_curve_from_days, day_aggs, day_number, period_stats,
    period_stats_from_days, pick_granularity, CurvePoint, DayAgg, Granularity, TempSample,
};

/// Au-delà de ce nombre de courbes superposées, l'UI avertit sur la lisibilité.
///
/// Même valeur que le plafond de l'agent (`commands::MAX_POINTS_PAR_APPEL`),
/// mais pour une raison différente et un effet différent : l'agent **tronque**
/// (c'est un budget de tokens), le croisement **avertit sans rien bloquer**
/// (c'est une gêne de lecture, et l'utilisateur est seul juge de ce qu'il
/// arrive encore à lire).
pub const SEUIL_LISIBILITE: usize = 8;

// ---------------------------------------------------------------------------
// Fenêtre temporelle
// ---------------------------------------------------------------------------

/// Fenêtre demandée par l'utilisateur, telle qu'elle arrive du sélecteur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TimeWindow {
    /// Les 7 derniers jours de données (cf. l'ancre, en tête de module).
    Last7Days,
    /// Les 30 derniers jours de données.
    Last30Days,
    /// L'année civile de la dernière mesure, du 1er janvier à celle-ci.
    CurrentYear,
    /// Tout ce que la sélection couvre.
    All,
    /// Plage saisie à la main, deux dates calendaires **UTC** incluses.
    Custom { from: NaiveDate, to: NaiveDate },
}

impl TimeWindow {
    pub fn label_fr(self) -> &'static str {
        match self {
            TimeWindow::Last7Days => "7 derniers jours de données",
            TimeWindow::Last30Days => "30 derniers jours de données",
            TimeWindow::CurrentYear => "Année de la dernière mesure",
            TimeWindow::All => "Tout l'historique disponible",
            TimeWindow::Custom { .. } => "Plage personnalisée",
        }
    }
}

/// Fenêtre effectivement retenue, bornes incluses, avec sa granularité.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedWindow {
    pub requested: TimeWindow,
    pub label: String,
    pub from_utc: DateTime<Utc>,
    pub to_utc: DateTime<Utc>,
    pub granularity: Granularity,
    pub granularity_label: String,
    /// `true` si la fenêtre couvre au moins une année pleine : c'est la condition
    /// pour que « moyenne des minimums quotidiens » mérite le mot *annuelle*.
    pub covers_a_full_year: bool,
}

/// Nombre de jours à partir duquel on parle de moyenne **annualisée**.
/// 365 et non 366 : une année bissextile ne doit pas disqualifier le libellé.
const JOURS_POUR_ANNUALISER: i64 = 365;

/// Traduit une fenêtre demandée en bornes UTC concrètes.
///
/// `couverture` est l'étendue réellement disponible dans la sélection
/// (première et dernière mesure, tous points confondus) ; c'est elle qui sert
/// d'ancre aux fenêtres relatives. Renvoie `None` si la sélection ne porte
/// aucune mesure — il n'y a alors rien à ancrer, et inventer une fenêtre
/// donnerait un graphique vide qui aurait l'air d'un bug.
pub fn resolve_window(
    window: TimeWindow,
    couverture: Option<(DateTime<Utc>, DateTime<Utc>)>,
) -> Option<ResolvedWindow> {
    let (premiere, derniere) = couverture?;

    let (mut from, to) = match window {
        TimeWindow::Last7Days => (derniere - Duration::days(7), derniere),
        TimeWindow::Last30Days => (derniere - Duration::days(30), derniere),
        TimeWindow::CurrentYear => (debut_d_annee(derniere.year()), derniere),
        TimeWindow::All => (premiere, derniere),
        TimeWindow::Custom { from, to } => {
            // Bornes saisies à l'envers : on les remet dans l'ordre plutôt que
            // de renvoyer une fenêtre vide que rien n'expliquerait à l'écran.
            let (a, b) = if from <= to { (from, to) } else { (to, from) };
            (minuit(a), fin_de_journee(b))
        }
    };

    // Une fenêtre relative ne descend jamais avant la première mesure : afficher
    // un axe qui commence six mois avant le premier relevé donnerait une courbe
    // écrasée dans le coin droit du graphique.
    if !matches!(window, TimeWindow::Custom { .. }) && from < premiere {
        from = premiere;
    }

    let granularity = pick_granularity(from, to);
    Some(ResolvedWindow {
        requested: window,
        label: window.label_fr().to_string(),
        from_utc: from,
        to_utc: to,
        granularity,
        granularity_label: granularity.label_fr().to_string(),
        covers_a_full_year: (to - from).num_days() >= JOURS_POUR_ANNUALISER,
    })
}

pub(crate) fn minuit(date: NaiveDate) -> DateTime<Utc> {
    DateTime::from_naive_utc_and_offset(
        date.and_hms_opt(0, 0, 0).expect("minuit est valide"),
        Utc,
    )
}

pub(crate) fn fin_de_journee(date: NaiveDate) -> DateTime<Utc> {
    DateTime::from_naive_utc_and_offset(
        date.and_hms_opt(23, 59, 59).expect("23:59:59 est valide"),
        Utc,
    )
}

fn debut_d_annee(annee: i32) -> DateTime<Utc> {
    minuit(NaiveDate::from_ymd_opt(annee, 1, 1).expect("le 1er janvier existe"))
}

// ---------------------------------------------------------------------------
// Document produit
// ---------------------------------------------------------------------------

/// Un point de la sélection : son identité, ses agrégats de fenêtre, sa courbe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossPointStats {
    pub point_id: String,
    pub name: String,
    pub category: Category,
    pub category_label: String,
    pub size: Option<PointSize>,
    pub altitude_m: Option<f64>,

    /// Mesures **dans la fenêtre**. Zéro est un cas normal (le point n'avait
    /// rien à enregistrer sur cette période) et doit se lire comme tel.
    pub measurement_count: usize,
    /// Journées calendaires effectivement observées dans la fenêtre.
    pub day_count: usize,
    pub first_utc: Option<DateTime<Utc>>,
    pub last_utc: Option<DateTime<Utc>>,

    // --- Les quatre statistiques demandées, toutes calculées sur la fenêtre
    pub extreme_min_c: Option<f64>,
    pub extreme_min_date: Option<NaiveDate>,
    pub mean_daily_min_c: Option<f64>,
    pub mean_amplitude_c: Option<f64>,
    pub max_amplitude_c: Option<f64>,
    pub max_amplitude_date: Option<NaiveDate>,

    /// Record de froid **hors fenêtre**, sur tout l'historique du point.
    ///
    /// Redondant avec `extreme_min_c` quand la fenêtre vaut « tout
    /// l'historique », et c'est voulu : sur une fenêtre courte, ne montrer que
    /// le minimum des trente derniers jours ferait disparaître le record de
    /// l'hiver précédent sans que rien ne le signale.
    pub record_min_c: Option<f64>,
    /// Volume et couverture totale du point, fenêtre mise à part.
    pub total_measurement_count: usize,
    pub history_first_utc: Option<DateTime<Utc>>,
    pub history_last_utc: Option<DateTime<Utc>>,

    /// Nombre de séries du point qui sont des **saisies manuelles sur site**
    /// (cf. [`crate::model::SourceType::is_manual_entry`]).
    ///
    /// Non nul, il change la façon de lire tout ce qui précède : une moyenne des
    /// minimums établie sur des relevés recopiés d'un carnet n'a pas le statut de
    /// la même moyenne établie sur un logger. Le chiffre est **compté par série
    /// et non par mesure** : c'est la nature de la source qu'il décrit, pas un
    /// volume — et le mélange des deux dans une même statistique est précisément
    /// ce qu'il sert à signaler.
    ///
    /// L'export (résumé texte et HTML) et le contexte envoyé à l'agent s'en
    /// servent pour le dire noir sur blanc.
    pub manual_entry_series: usize,

    /// Courbe agrégée à la granularité de la fenêtre (cf. `stats::aggregate_curve`).
    pub curve: Vec<CurvePoint>,
}

/// Tout ce dont le bloc a besoin pour se dessiner — et tout ce dont un
/// exportateur aurait besoin pour écrire un rapport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossAnalysis {
    /// `None` quand aucun point sélectionné ne porte de mesure.
    pub window: Option<ResolvedWindow>,
    pub points: Vec<CrossPointStats>,
    /// Points sélectionnés qui n'ont aucune mesure, fenêtre comprise ou non.
    /// Comptés à part pour que l'UI distingue « rien à comparer » de « rien sur
    /// cette période-là ».
    pub points_without_data: usize,
    /// Au-delà de [`SEUIL_LISIBILITE`] courbes, un avertissement — jamais un refus.
    pub readability_warning: Option<String>,
    /// Durée du calcul côté Rust, lecture en base comprise. Affichée discrètement :
    /// c'est le seul moyen de voir se dégrader la réactivité avant qu'elle ne gêne.
    pub elapsed_ms: u64,
}

/// Agrégats journaliers d'un point sur `[from, to]` (bornes incluses).
///
/// Les jours entiers viennent de `series_days` ; les jours entamés aux deux
/// bords sont recalculés depuis les mesures de la fenêtre, pour que le
/// résultat soit exactement celui de la lecture brute.
pub(crate) fn jours_de_fenetre(db: &Db, point_id: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> AppResult<Vec<DayAgg>> {
    if to < from {
        return Ok(Vec::new());
    }
    let debut_jour = |d: i64| Utc.timestamp_opt(d * 86_400, 0).single().unwrap_or(from);
    let (d0, d1) = (day_number(from), day_number(to));
    let premier_entier = if from == debut_jour(d0) { d0 } else { d0 + 1 };
    let dernier_entier = if to == debut_jour(d1 + 1) - Duration::seconds(1) { d1 } else { d1 - 1 };
    if premier_entier > dernier_entier {
        return Ok(day_aggs(&db.temperature_samples_for_point(point_id, Some(from), Some(to))?));
    }
    let mut jours = Vec::new();
    if premier_entier > d0 {
        let fin = debut_jour(premier_entier) - Duration::seconds(1);
        jours.extend(day_aggs(&db.temperature_samples_for_point(point_id, Some(from), Some(fin))?));
    }
    jours.extend(db.point_days(point_id, Some(premier_entier), Some(dernier_entier))?);
    if dernier_entier < d1 {
        let debut = debut_jour(dernier_entier + 1);
        jours.extend(day_aggs(&db.temperature_samples_for_point(point_id, Some(debut), Some(to))?));
    }
    Ok(jours)
}

/// Construit l'analyse pour une sélection et une fenêtre.
///
/// Deux passes, et c'est nécessaire : la fenêtre relative s'ancre sur la
/// couverture de **toute** la sélection, qu'on ne connaît qu'après avoir
/// interrogé chaque point. La première passe ne lit que des agrégats
/// (`MIN`/`MAX`/`COUNT`), pas les mesures ; la seconde ne lit que la tranche
/// retenue.
pub fn build(db: &Db, point_ids: &[String], window: TimeWindow) -> AppResult<CrossAnalysis> {
    construire(db, point_ids, window, true)
}

fn construire(db: &Db, point_ids: &[String], window: TimeWindow, avec_cache: bool) -> AppResult<CrossAnalysis> {
    let debut = std::time::Instant::now();

    // --- Passe 1 : identité, record et couverture de chaque point.
    let mut releves = Vec::with_capacity(point_ids.len());
    let mut couverture: Option<(DateTime<Utc>, DateTime<Utc>)> = None;

    for id in point_ids {
        let point = db.get_point(id)?;
        let empreinte = db.empreinte_donnees_point(id)?;
        let connu = if avec_cache { db.cache_croisement().historique(id, &empreinte) } else { None };
        let (record, saisies) = match connu {
            Some(c) => c,
            None => {
                let record = db.point_record(id)?;
                // Les saisies manuelles changent la lecture de tout le reste :
                // le compte est celui des séries, pas des mesures (voir
                // `CrossPointStats::manual_entry_series`).
                let saisies = db
                    .list_series_for_point(id)?
                    .iter()
                    .filter(|s| s.source_type.is_manual_entry())
                    .count();
                if avec_cache {
                    db.cache_croisement().noter_historique(id, &empreinte, record, saisies);
                }
                (record, saisies)
            }
        };

        if let (Some(a), Some(b)) = (record.first_utc, record.last_utc) {
            couverture = Some(match couverture {
                Some((min, max)) => (min.min(a), max.max(b)),
                None => (a, b),
            });
        }
        releves.push((point, empreinte, record, saisies));
    }

    let resolue = resolve_window(window, couverture);
    let cle = resolue
        .as_ref()
        .map(|w| (w.from_utc.timestamp(), w.to_utc.timestamp(), w.granularity));

    // --- Passe 2 : statistiques et courbe de la fenêtre.
    let mut points = Vec::with_capacity(releves.len());
    let mut sans_donnee = 0usize;

    for (point, empreinte, record, saisies) in releves {
        if record.measurement_count == 0 {
            sans_donnee += 1;
        }
        let deja = match (avec_cache, cle) {
            (true, Some(c)) => db.cache_croisement().fenetre(&point.id, &empreinte, c),
            _ => None,
        };
        let mut calcule = match deja {
            Some(p) => p,
            None => {
                let p = calculer_point(db, &point, record, saisies, resolue.as_ref())?;
                if let (true, Some(c)) = (avec_cache, cle) {
                    db.cache_croisement().noter_fenetre(&point.id, &empreinte, c, p.clone());
                }
                p
            }
        };
        // L'identité du point ne fait pas partie des données mesurées : elle
        // est toujours relue, un renommage se voit au clic suivant.
        calcule.name = point.name.clone();
        calcule.category = point.category;
        calcule.category_label = point.category.label_fr().to_string();
        calcule.size = point.size;
        calcule.altitude_m = point.altitude_m;
        points.push(calcule);
    }

    let readability_warning = (points.len() > SEUIL_LISIBILITE).then(|| {
        format!(
            "{} courbes superposées : au-delà de {SEUIL_LISIBILITE}, les couleurs se \
             répètent et les tracés se recouvrent. Le graphique reste utilisable, sa \
             lecture non.",
            points.len()
        )
    });

    Ok(CrossAnalysis {
        window: resolue,
        points,
        points_without_data: sans_donnee,
        readability_warning,
        elapsed_ms: debut.elapsed().as_millis() as u64,
    })
}

/// Statistiques et courbe d'un point sur la fenêtre retenue.
fn calculer_point(
    db: &Db,
    point: &Point,
    record: PointRecord,
    saisies: usize,
    resolue: Option<&ResolvedWindow>,
) -> AppResult<CrossPointStats> {
    // Au-delà de dix jours, statistiques et courbe se tirent des agrégats
    // journaliers : quelques milliers de lignes au lieu de toutes les mesures
    // de la fenêtre (cadrage « données à la demande », étape A).
    let (stats, curve) = match resolue {
        Some(w) if w.granularity == Granularity::Hour => {
            let echantillons: Vec<TempSample> =
                db.temperature_samples_for_point(&point.id, Some(w.from_utc), Some(w.to_utc))?;
            (period_stats(&echantillons), aggregate_curve(&echantillons, w.granularity))
        }
        Some(w) => {
            let jours = jours_de_fenetre(db, &point.id, w.from_utc, w.to_utc)?;
            (period_stats_from_days(&jours), aggregate_curve_from_days(&jours, w.granularity))
        }
        None => (period_stats::<TempSample>(&[]), Vec::new()),
    };

    Ok(CrossPointStats {
        point_id: point.id.clone(),
        name: point.name.clone(),
        category: point.category,
        category_label: point.category.label_fr().to_string(),
        size: point.size,
        altitude_m: point.altitude_m,

        measurement_count: stats.measurement_count,
        day_count: stats.day_count,
        first_utc: stats.first_utc,
        last_utc: stats.last_utc,

        extreme_min_c: stats.extreme_min_c,
        extreme_min_date: stats.extreme_min_date,
        mean_daily_min_c: stats.mean_daily_min_c,
        mean_amplitude_c: stats.mean_amplitude_c,
        max_amplitude_c: stats.max_amplitude_c,
        max_amplitude_date: stats.max_amplitude_date,

        record_min_c: record.record_min_c,
        total_measurement_count: record.measurement_count,
        history_first_utc: record.first_utc,
        history_last_utc: record.last_utc,

        manual_entry_series: saisies,

        curve,
    })
}

// ---------------------------------------------------------------------------
// Cache par point
// ---------------------------------------------------------------------------
//
// Avec beaucoup de points sélectionnés, chaque clic sur la carte relance
// l'analyse de toute la sélection. Or les chiffres d'un point ne dépendent que
// de ses données et de la fenêtre. On les garde donc, avec l'empreinte de ses
// données (`Db::empreinte_donnees_point`) : un clic ne recalcule que le point
// ajouté, ou ceux dont une mesure a changé entre-temps.

/// Fenêtres gardées par point : les plus récemment utilisées.
const FENETRES_PAR_POINT: usize = 4;

/// Bornes de la fenêtre (secondes UTC) et granularité.
type CleFenetre = (i64, i64, Granularity);

/// Résultats par point du bloc « Croisement de données », tenus par [`Db`].
#[derive(Default)]
pub struct CacheCroisement {
    points: HashMap<String, EntreePoint>,
}

struct EntreePoint {
    empreinte: String,
    record: PointRecord,
    saisies: usize,
    fenetres: Vec<(CleFenetre, CrossPointStats)>,
}

impl CacheCroisement {
    fn historique(&self, point_id: &str, empreinte: &str) -> Option<(PointRecord, usize)> {
        self.points
            .get(point_id)
            .filter(|e| e.empreinte == empreinte)
            .map(|e| (e.record, e.saisies))
    }

    /// Nouvelle empreinte : les fenêtres calculées sur les anciennes données
    /// partent avec l'ancienne entrée.
    fn noter_historique(&mut self, point_id: &str, empreinte: &str, record: PointRecord, saisies: usize) {
        self.points.insert(
            point_id.to_string(),
            EntreePoint { empreinte: empreinte.to_string(), record, saisies, fenetres: Vec::new() },
        );
    }

    fn fenetre(&mut self, point_id: &str, empreinte: &str, cle: CleFenetre) -> Option<CrossPointStats> {
        let e = self.points.get_mut(point_id).filter(|e| e.empreinte == empreinte)?;
        let rang = e.fenetres.iter().position(|(k, _)| *k == cle)?;
        let trouvee = e.fenetres.remove(rang);
        let copie = trouvee.1.clone();
        e.fenetres.push(trouvee);
        Some(copie)
    }

    fn noter_fenetre(&mut self, point_id: &str, empreinte: &str, cle: CleFenetre, stats: CrossPointStats) {
        if let Some(e) = self.points.get_mut(point_id).filter(|e| e.empreinte == empreinte) {
            e.fenetres.retain(|(k, _)| *k != cle);
            e.fenetres.push((cle, stats));
            if e.fenetres.len() > FENETRES_PAR_POINT {
                e.fenetres.remove(0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        LoggerSeries, Measurement, NewPoint, Point, SeriesRole, SourceType,
    };
    use crate::parsing::DateOrder;

    fn t(iso: &str) -> DateTime<Utc> {
        iso.parse().expect("horodatage de test invalide")
    }

    fn d(y: i32, m: u32, j: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, j).unwrap()
    }

    /// Couverture type d'une archive ROMMA : neuf ans, jusqu'à l'été 2026.
    fn neuf_ans() -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        Some((t("2017-05-31T00:00:00Z"), t("2026-08-19T12:30:00Z")))
    }

    #[test]
    fn sans_aucune_mesure_il_n_y_a_pas_de_fenetre() {
        assert!(resolve_window(TimeWindow::All, None).is_none());
        assert!(resolve_window(TimeWindow::Last7Days, None).is_none());
    }

    /// Le point du module : l'ancre est la dernière **mesure**, pas l'horloge.
    /// Un jeu de logger de 2024 relu en 2026 doit rester affichable.
    #[test]
    fn les_fenetres_relatives_s_ancrent_sur_la_derniere_mesure() {
        let vieux = Some((t("2024-01-01T00:00:00Z"), t("2024-03-15T09:00:00Z")));
        let w = resolve_window(TimeWindow::Last7Days, vieux).unwrap();
        assert_eq!(w.to_utc, t("2024-03-15T09:00:00Z"));
        assert_eq!(w.from_utc, t("2024-03-08T09:00:00Z"));
        assert_eq!(w.granularity, Granularity::Hour);
        assert!(!w.covers_a_full_year);
    }

    #[test]
    fn trente_jours_et_annee_en_cours() {
        let w = resolve_window(TimeWindow::Last30Days, neuf_ans()).unwrap();
        assert_eq!(w.from_utc, t("2026-07-20T12:30:00Z"));
        assert_eq!(w.to_utc, t("2026-08-19T12:30:00Z"));
        assert_eq!(w.granularity, Granularity::Day);

        let w = resolve_window(TimeWindow::CurrentYear, neuf_ans()).unwrap();
        assert_eq!(w.from_utc, t("2026-01-01T00:00:00Z"));
        assert_eq!(w.to_utc, t("2026-08-19T12:30:00Z"));
        assert_eq!(w.granularity, Granularity::Day);
        // 231 jours : pas encore une année pleine, le mot « annuelle » serait faux.
        assert!(!w.covers_a_full_year);
    }

    #[test]
    fn tout_l_historique_prend_les_bornes_reelles_et_passe_au_mensuel() {
        let w = resolve_window(TimeWindow::All, neuf_ans()).unwrap();
        assert_eq!(w.from_utc, t("2017-05-31T00:00:00Z"));
        assert_eq!(w.to_utc, t("2026-08-19T12:30:00Z"));
        assert_eq!(w.granularity, Granularity::Month);
        assert!(w.covers_a_full_year);
    }

    /// Une fenêtre relative plus longue que les données disponibles est ramenée
    /// à la première mesure : sinon l'axe s'étirerait sur du vide.
    #[test]
    fn une_fenetre_plus_longue_que_les_donnees_est_rognee() {
        let court = Some((t("2026-08-17T00:00:00Z"), t("2026-08-19T00:00:00Z")));
        let w = resolve_window(TimeWindow::Last30Days, court).unwrap();
        assert_eq!(w.from_utc, t("2026-08-17T00:00:00Z"));
        assert_eq!(w.to_utc, t("2026-08-19T00:00:00Z"));
        assert_eq!(w.granularity, Granularity::Hour);
    }

    #[test]
    fn une_plage_personnalisee_couvre_ses_deux_journees_entieres() {
        let w = resolve_window(
            TimeWindow::Custom {
                from: d(2026, 1, 10),
                to: d(2026, 1, 12),
            },
            neuf_ans(),
        )
        .unwrap();
        assert_eq!(w.from_utc, t("2026-01-10T00:00:00Z"));
        assert_eq!(w.to_utc, t("2026-01-12T23:59:59Z"));
    }

    /// Bornes saisies à l'envers : remises dans l'ordre, pas rejetées. Une
    /// fenêtre vide à l'écran n'expliquerait pas d'où vient le problème.
    #[test]
    fn une_plage_saisie_a_l_envers_est_remise_dans_l_ordre() {
        let w = resolve_window(
            TimeWindow::Custom {
                from: d(2026, 1, 12),
                to: d(2026, 1, 10),
            },
            neuf_ans(),
        )
        .unwrap();
        assert_eq!(w.from_utc, t("2026-01-10T00:00:00Z"));
        assert_eq!(w.to_utc, t("2026-01-12T23:59:59Z"));
    }

    /// Une plage personnalisée hors des données n'est **pas** rognée : c'est une
    /// demande explicite, et la voir revenir vide est l'information utile.
    #[test]
    fn une_plage_personnalisee_hors_donnees_reste_telle_quelle() {
        let w = resolve_window(
            TimeWindow::Custom {
                from: d(1990, 1, 1),
                to: d(1990, 1, 3),
            },
            neuf_ans(),
        )
        .unwrap();
        assert_eq!(w.from_utc, t("1990-01-01T00:00:00Z"));
    }

    #[test]
    fn le_seuil_d_annualisation_tombe_a_365_jours() {
        let un_an = Some((t("2025-01-01T00:00:00Z"), t("2026-01-01T00:00:00Z")));
        assert!(resolve_window(TimeWindow::All, un_an).unwrap().covers_a_full_year);

        let un_jour_de_moins = Some((t("2025-01-02T00:00:00Z"), t("2026-01-01T00:00:00Z")));
        assert!(!resolve_window(TimeWindow::All, un_jour_de_moins)
            .unwrap()
            .covers_a_full_year);
    }

    /// Le sélecteur du frontend envoie du JSON : sa forme fait partie du contrat.
    #[test]
    fn la_fenetre_se_deserialise_depuis_le_json_du_selecteur() {
        let w: TimeWindow = serde_json::from_str(r#"{"kind":"last30_days"}"#).unwrap();
        assert_eq!(w, TimeWindow::Last30Days);

        let w: TimeWindow =
            serde_json::from_str(r#"{"kind":"custom","from":"2026-01-10","to":"2026-01-12"}"#)
                .unwrap();
        assert_eq!(
            w,
            TimeWindow::Custom {
                from: d(2026, 1, 10),
                to: d(2026, 1, 12)
            }
        );
    }

    // -----------------------------------------------------------------------
    // Bout en bout, sur une base en mémoire
    // -----------------------------------------------------------------------

    fn nouveau_point(project_id: &str, nom: &str, categorie: &str, taille: Option<&str>) -> NewPoint {
        NewPoint {
            project_id: project_id.to_string(),
            name: nom.to_string(),
            latitude: 44.9812,
            longitude: 5.4213,
            altitude_m: Some(1150.0),
            category: categorie.to_string(),
            size: taille.map(String::from),
            factor_sky: None,
            factor_wind: None,
            factor_slope: None,
            factor_snow_cover: None,
            factor_forest_cover: None,
            is_open_terrain: None,
            notes: None,
            infoclimat_station_id: None,
            romma_station_id: None,
        }
    }

    fn serie(point_id: &str) -> LoggerSeries {
        LoggerSeries {
            id: uuid::Uuid::new_v4().to_string(),
            point_id: point_id.to_string(),
            source_type: SourceType::Manual,
            role: SeriesRole::Primary,
            logger_serial: None,
            imported_at: Utc::now(),
            raw_file_reference: None,
            clock_offset_seconds: 0,
            raw_timestamp_was_utc: true,
            label: None,
            sample_interval_seconds: Some(3600),
            data_license: None,
            station_id: None,
            last_api_fetch_at: None,
            date_order: DateOrder::DayFirst,
            date_format_uncertain: false,
            content_fingerprint: None,
            measurement_count: 0,
        }
    }

    /// Deux points, dix jours de mesures horaires chacun, le second décalé de
    /// +5 °C partout — un écart constant est le seul jeu où l'on sait d'avance
    /// ce que *toutes* les statistiques doivent donner.
    fn base_avec_deux_points() -> (Db, String, String) {
        let db = Db::open_in_memory().unwrap();
        let projet = db.create_project("Vercors").unwrap();

        let mut ids = Vec::new();
        for (nom, decalage) in [("Darbounouse", 0.0), ("Combe Chaude", 5.0)] {
            let point =
                Point::from_new(nouveau_point(&projet.id, nom, "Cat3", Some("Gr"))).unwrap();
            db.insert_point(&point).unwrap();

            let s = serie(&point.id);
            // Minimum du jour vers 5 h du matin, comme une nuit d'inversion.
            let mut mesures = Vec::new();
            for h in 0..(24 * 10) {
                let instant = t("2026-01-01T00:00:00Z") + Duration::hours(h);
                let heure = (h % 24) as f64;
                let temp = -8.0 + decalage + (heure - 5.0).abs() * 0.8;
                mesures.push(Measurement::new_utc(s.id.clone(), instant, temp));
            }
            db.insert_series_with_measurements(&s, &mesures).unwrap();
            ids.push(point.id);
        }

        (db, ids[0].clone(), ids[1].clone())
    }

    /// Fenêtre aux bords non alignés sur minuit : jours entiers lus en base,
    /// jours entamés recalculés — le tout identique à la lecture brute.
    #[test]
    fn jours_de_fenetre_egalent_la_lecture_brute() {
        let db = Db::open_in_memory().unwrap();
        let projet = db.create_project("Vercors").unwrap();
        let point = Point::from_new(nouveau_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();
        let s = serie(&point.id);
        let mesures: Vec<Measurement> = (0..(24 * 6 * 40))
            .map(|i| Measurement::new_utc(s.id.clone(), t("2026-01-01T00:00:00Z") + Duration::minutes(10 * i), ((i * 31) % 97) as f64 / 5.0 - 10.0))
            .collect();
        db.insert_series_with_measurements(&s, &mesures).unwrap();

        for (de, a) in [
            ("2026-01-03T07:20:00Z", "2026-02-02T15:00:00Z"),
            ("2026-01-03T00:00:00Z", "2026-01-31T23:59:59Z"),
            ("2026-01-05T12:00:00Z", "2026-01-06T11:00:00Z"),
            ("2025-12-01T00:00:00Z", "2026-03-01T00:00:00Z"),
        ] {
            let (de, a) = (t(de), t(a));
            let attendu = day_aggs(&db.temperature_samples_for_point(&point.id, Some(de), Some(a)).unwrap());
            let obtenu = jours_de_fenetre(&db, &point.id, de, a).unwrap();
            assert_eq!(obtenu.len(), attendu.len(), "{de} → {a}");
            for (x, y) in obtenu.iter().zip(&attendu) {
                assert_eq!((x.day, x.n, x.min, x.max, x.first_utc, x.last_utc), (y.day, y.n, y.min, y.max, y.first_utc, y.last_utc));
                assert!((x.sum - y.sum).abs() < 1e-9);
            }
        }

        // Fenêtre de 40 jours : granularité quotidienne, donc lue dans les jours.
        let analyse = build(&db, &[point.id.clone()], TimeWindow::All).unwrap();
        assert_eq!(analyse.window.as_ref().unwrap().granularity, Granularity::Day);
        let p = &analyse.points[0];
        assert_eq!(p.measurement_count, mesures.len());
        assert_eq!(p.curve.len(), 40);
        assert_eq!(p.curve.iter().map(|c| c.sample_count).sum::<usize>(), mesures.len());
    }

    /// L'analyse sans sa durée de calcul, qui diffère forcément d'un appel à l'autre.
    fn sans_duree(a: &CrossAnalysis) -> serde_json::Value {
        let mut v = serde_json::to_value(a).unwrap();
        v.as_object_mut().unwrap().remove("elapsed_ms");
        v
    }

    /// Le cache rend exactement le calcul complet, et le suit à chaque
    /// modification des données d'un point.
    #[test]
    fn le_cache_rend_le_calcul_complet_et_suit_les_modifications() {
        let (db, a, b) = base_avec_deux_points();
        let ids = [a.clone(), b.clone()];
        let verifier = |etape: &str| {
            for w in [TimeWindow::All, TimeWindow::Last7Days, TimeWindow::Last30Days] {
                let avec = build(&db, &ids, w).unwrap();
                let encore = build(&db, &ids, w).unwrap();
                let sans = construire(&db, &ids, w, false).unwrap();
                assert_eq!(sans_duree(&avec), sans_duree(&sans), "{etape} ({w:?}) : premier appel");
                assert_eq!(sans_duree(&encore), sans_duree(&sans), "{etape} ({w:?}) : appel servi par le cache");
            }
        };
        verifier("départ");
        let avant = sans_duree(&build(&db, &ids, TimeWindow::All).unwrap());

        let serie_a = db.list_series_for_point(&a).unwrap()[0].id.clone();
        let fin = t("2026-01-11T00:00:00Z");
        db.append_measurements(&serie_a, &[Measurement::new_utc(serie_a.as_str(), fin, -30.0)]).unwrap();
        verifier("ajout d'une mesure");
        assert_ne!(sans_duree(&build(&db, &ids, TimeWindow::All).unwrap()), avant);

        let une = db.measurements_for_series(&serie_a).unwrap()[5].id;
        db.set_measurement_temperature(&serie_a, une, 12.5).unwrap();
        verifier("correction d'une valeur");

        let de = t("2026-01-03T00:00:00Z").timestamp();
        db.delete_measurements_range(&serie_a, de, de + 6 * 3600).unwrap();
        verifier("suppression d'une plage");

        // Ré-import : nouvelle série au même volume, valeurs différentes.
        let volume = db.measurements_for_series(&serie_a).unwrap().len();
        db.delete_series(&serie_a).unwrap();
        let nouvelle = serie(&a);
        let mesures: Vec<Measurement> = (0..volume as i64)
            .map(|h| Measurement::new_utc(nouvelle.id.clone(), t("2026-01-01T00:00:00Z") + Duration::hours(h), 3.0))
            .collect();
        db.insert_series_with_measurements(&nouvelle, &mesures).unwrap();
        verifier("ré-import au même volume");

        // Un renommage se voit tout de suite, sans toucher aux mesures.
        let mut point = db.get_point(&b).unwrap();
        point.name = "Combe Froide".into();
        db.update_point(&point).unwrap();
        verifier("renommage");
        let analyse = build(&db, &ids, TimeWindow::All).unwrap();
        assert!(analyse.points.iter().any(|p| p.name == "Combe Froide"));
    }

    #[test]
    fn deux_points_partagent_la_meme_fenetre_et_les_memes_seaux() {
        let (db, a, b) = base_avec_deux_points();
        let analyse = build(&db, &[a, b], TimeWindow::All).unwrap();

        let w = analyse.window.as_ref().unwrap();
        assert_eq!(w.granularity, Granularity::Hour);
        assert_eq!(analyse.points.len(), 2);
        assert_eq!(analyse.points_without_data, 0);
        assert!(analyse.readability_warning.is_none());

        let [p1, p2] = &analyse.points[..] else {
            panic!("deux points attendus")
        };
        assert_eq!(p1.measurement_count, 240);
        assert_eq!(p2.measurement_count, 240);
        assert_eq!(p1.curve.len(), p2.curve.len());
        for (x, y) in p1.curve.iter().zip(p2.curve.iter()) {
            assert_eq!(
                x.bucket_start_utc, y.bucket_start_utc,
                "les deux courbes doivent partager leurs seaux"
            );
        }

        // Le second point est décalé de +5 °C, partout.
        let ecart = p2.extreme_min_c.unwrap() - p1.extreme_min_c.unwrap();
        assert!((ecart - 5.0).abs() < 1e-9, "écart obtenu : {ecart}");
        assert_eq!(p1.record_min_c, p1.extreme_min_c);
        assert_eq!(p1.total_measurement_count, 240);
    }

    /// Une fenêtre courte restreint les statistiques **et** la courbe, ensemble.
    #[test]
    fn la_fenetre_restreint_les_stats_et_la_courbe_de_la_meme_facon() {
        let (db, a, _) = base_avec_deux_points();
        let analyse = build(&db, std::slice::from_ref(&a), TimeWindow::Last7Days).unwrap();
        let p = &analyse.points[0];

        // 7 jours à un pas horaire, bornes incluses : 169 mesures.
        assert_eq!(p.measurement_count, 169);
        assert_eq!(p.curve.len(), 169);
        assert_eq!(p.day_count, 8);

        let w = analyse.window.as_ref().unwrap();
        assert!(p.first_utc.unwrap() >= w.from_utc);
        assert!(p.last_utc.unwrap() <= w.to_utc);

        // Le record, lui, reste celui de tout l'historique — c'est son intérêt.
        assert_eq!(p.record_min_c, Some(-8.0));
        assert_eq!(p.total_measurement_count, 240);
    }

    /// Une plage personnalisée qui ne recoupe rien : zéro mesure, mais le point
    /// reste présent, avec son record et sa couverture réelle. C'est ce qui
    /// permet à l'UI de dire « rien sur cette période » plutôt que rien du tout.
    #[test]
    fn une_fenetre_sans_mesure_laisse_le_point_visible() {
        let (db, a, _) = base_avec_deux_points();
        let analyse = build(
            &db,
            std::slice::from_ref(&a),
            TimeWindow::Custom {
                from: d(1990, 1, 1),
                to: d(1990, 1, 3),
            },
        )
        .unwrap();

        let p = &analyse.points[0];
        assert_eq!(p.measurement_count, 0);
        assert!(p.curve.is_empty());
        assert!(p.extreme_min_c.is_none());
        assert_eq!(p.record_min_c, Some(-8.0));
        assert_eq!(p.total_measurement_count, 240);
        // Le point a des données — juste pas là. Il ne compte pas comme vide.
        assert_eq!(analyse.points_without_data, 0);
    }

    #[test]
    fn une_selection_vide_ne_produit_ni_fenetre_ni_panique() {
        let (db, _, _) = base_avec_deux_points();
        let analyse = build(&db, &[], TimeWindow::All).unwrap();
        assert!(analyse.window.is_none());
        assert!(analyse.points.is_empty());
        assert!(analyse.readability_warning.is_none());
    }

    /// Un point sans la moindre mesure ne fait pas échouer la comparaison : il
    /// est compté à part, et l'autre point reste analysé.
    #[test]
    fn un_point_sans_mesure_est_compte_a_part() {
        let (db, a, _) = base_avec_deux_points();
        let projet_id = db.list_projects().unwrap()[0].id.clone();
        let vide =
            Point::from_new(nouveau_point(&projet_id, "Jamais relevé", "Cat1", None)).unwrap();
        db.insert_point(&vide).unwrap();

        let analyse = build(&db, &[a, vide.id], TimeWindow::All).unwrap();
        assert_eq!(analyse.points.len(), 2);
        assert_eq!(analyse.points_without_data, 1);
        assert!(analyse.window.is_some(), "l'autre point porte la fenêtre");
        assert_eq!(analyse.points[1].total_measurement_count, 0);
        assert!(analyse.points[1].record_min_c.is_none());
    }
}

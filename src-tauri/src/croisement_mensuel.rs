//! Croisement en plein écran : le bilan **mois par mois** de chaque station.
//!
//! Le bloc « Croisement de données » de la grille compare des stations sur une
//! fenêtre (`crossing.rs`). La vue plein écran répond à une autre question :
//! comment chaque station a évolué, mois après mois, sur plusieurs années. Le
//! profil « Par défaut » (1er janvier 2020 → 30 septembre 2026) sert à suivre
//! le réchauffement.
//!
//! Tout se calcule depuis les agrégats journaliers (`series_days`), jamais
//! depuis les mesures : six ans sur dix stations, c'est quelques dizaines de
//! milliers de jours au lieu de plusieurs millions de mesures.
//!
//! ## Ce que vaut un « jour chaud » ou un « jour de gel »
//!
//! Définitions de Météo-France, seuils réglables :
//! * jour chaud : maximale du jour ≥ seuil chaud (25 °C par défaut) ;
//! * jour de gel : minimale du jour ≤ seuil de gel (0 °C par défaut).
//!
//! Les jours sont des jours calendaires **UTC**, comme partout dans l'app.
//!
//! ## Mois incomplets
//!
//! Un mois où le logger n'a tourné que dix jours compte forcément moins de jours
//! de gel qu'un mois complet. Chaque mois porte donc son nombre de jours
//! observés, et la moyenne « jours par mois » de la période n'utilise que les
//! mois couverts à [`COUVERTURE_MOIS_COMPLET`] ou plus.

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::crossing::{fin_de_journee, jours_de_fenetre, minuit};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::stats::DayAgg;

/// Part des jours d'un mois à observer pour qu'il compte comme complet.
pub const COUVERTURE_MOIS_COMPLET: f64 = 0.9;

/// Seuils des comptes de jours, en °C.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Seuils {
    /// Jour chaud : maximale ≥ ce seuil.
    pub chaud_c: f64,
    /// Jour de gel : minimale ≤ ce seuil.
    pub gel_c: f64,
}

impl Default for Seuils {
    fn default() -> Self {
        Seuils { chaud_c: 25.0, gel_c: 0.0 }
    }
}

/// Un mois d'une station.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MoisStation {
    pub annee: i32,
    pub mois: u32,
    /// Jours observés dans le mois.
    pub jours_observes: usize,
    /// Nombre de jours du mois calendaire (28 à 31).
    pub jours_du_mois: u32,
    pub complet: bool,
    /// Moyenne de toutes les mesures du mois.
    pub moy_c: Option<f64>,
    pub moy_min_c: Option<f64>,
    pub moy_max_c: Option<f64>,
    pub ext_min_c: Option<f64>,
    pub ext_min_date: Option<NaiveDate>,
    pub ext_max_c: Option<f64>,
    pub ext_max_date: Option<NaiveDate>,
    pub jours_chauds: usize,
    pub jours_gel: usize,
}

/// Synthèse d'une station sur toute la période demandée.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SynthesePeriode {
    pub jours_observes: usize,
    pub mois_observes: usize,
    pub mois_complets: usize,
    /// Moyenne de toutes les mesures de la période.
    pub moy_c: Option<f64>,
    /// Moyenne des minimales de **tous** les jours observés.
    pub moy_min_c: Option<f64>,
    pub moy_max_c: Option<f64>,
    pub ext_min_c: Option<f64>,
    pub ext_min_date: Option<NaiveDate>,
    pub ext_max_c: Option<f64>,
    pub ext_max_date: Option<NaiveDate>,
    /// Moyenne sur les seuls mois complets ; `None` s'il n'y en a aucun.
    pub jours_chauds_par_mois: Option<f64>,
    pub jours_gel_par_mois: Option<f64>,
}

/// Un mois de l'année (janvier, février…) moyenné sur toutes les années de la
/// période : la colonne « janv. » du tableau, le point « janv. » du graphique.
///
/// Les moyennes portent sur tous les jours observés de ce mois, toutes années
/// confondues. Les extrêmes sont ceux de tous ces jours. Les comptes de jours
/// sont la moyenne des seuls mois complets (voir [`COUVERTURE_MOIS_COMPLET`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormaleMois {
    /// 1 à 12.
    pub mois: u32,
    /// Années où ce mois a au moins un jour observé.
    pub annees: usize,
    pub mois_complets: usize,
    pub jours_observes: usize,
    pub moy_c: Option<f64>,
    pub moy_min_c: Option<f64>,
    pub moy_max_c: Option<f64>,
    pub ext_min_c: Option<f64>,
    pub ext_min_date: Option<NaiveDate>,
    pub ext_max_c: Option<f64>,
    pub ext_max_date: Option<NaiveDate>,
    pub jours_chauds_par_mois: Option<f64>,
    pub jours_gel_par_mois: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilanStation {
    pub point_id: String,
    pub name: String,
    pub altitude_m: Option<f64>,
    /// Mois observés seulement, dans l'ordre. Un mois sans mesure n'a pas de ligne.
    pub mois: Vec<MoisStation>,
    /// Les 12 mois de l'année, moyennés sur les années de la période (toujours 12).
    pub normales: Vec<NormaleMois>,
    pub periode: SynthesePeriode,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilanMensuel {
    pub du: NaiveDate,
    pub au: NaiveDate,
    pub seuils: Seuils,
    pub couverture_mois_complet: f64,
    pub stations: Vec<BilanStation>,
    pub elapsed_ms: u64,
}

fn jours_dans_le_mois(annee: i32, mois: u32) -> u32 {
    let (a, m) = if mois == 12 { (annee + 1, 1) } else { (annee, mois + 1) };
    let premier_suivant = NaiveDate::from_ymd_opt(a, m, 1).expect("1er du mois valide");
    premier_suivant.pred_opt().expect("veille valide").day()
}

fn moyenne(valeurs: impl Iterator<Item = f64>) -> Option<f64> {
    let (somme, n) = valeurs.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| somme / n as f64)
}

/// Moyenne de toutes les mesures des jours donnés (sommes et nombres journaliers).
fn moyenne_mesures(jours: &[&DayAgg]) -> Option<f64> {
    let n: usize = jours.iter().map(|j| j.n).sum();
    (n > 0).then(|| jours.iter().map(|j| j.sum).sum::<f64>() / n as f64)
}

/// Extrême d'un ensemble de jours ; à égalité, le premier jour l'emporte.
fn extreme(jours: &[&DayAgg], valeur: fn(&DayAgg) -> f64, plus_bas: bool) -> (Option<f64>, Option<NaiveDate>) {
    let mut meilleur: Option<&DayAgg> = None;
    for j in jours {
        let mieux = match meilleur {
            None => true,
            Some(m) if plus_bas => valeur(j) < valeur(m),
            Some(m) => valeur(j) > valeur(m),
        };
        if mieux {
            meilleur = Some(j);
        }
    }
    (meilleur.map(valeur), meilleur.map(|j| j.date()))
}

/// Bilan d'une station à partir de ses agrégats journaliers (triés ou non).
/// Fonction pure : c'est elle que les tests vérifient.
pub fn bilan_des_jours(jours: &[DayAgg], seuils: Seuils) -> (Vec<MoisStation>, Vec<NormaleMois>, SynthesePeriode) {
    let mut valides: Vec<&DayAgg> = jours.iter().filter(|j| j.n > 0).collect();
    valides.sort_by_key(|j| j.day);

    let mut mois = Vec::new();
    let mut debut = 0;
    while debut < valides.len() {
        let date = valides[debut].date();
        let (annee, m) = (date.year(), date.month());
        let mut fin = debut;
        while fin < valides.len() && {
            let d = valides[fin].date();
            d.year() == annee && d.month() == m
        } {
            fin += 1;
        }
        let tranche = &valides[debut..fin];
        let jours_du_mois = jours_dans_le_mois(annee, m);
        let (ext_min_c, ext_min_date) = extreme(tranche, |j| j.min, true);
        let (ext_max_c, ext_max_date) = extreme(tranche, |j| j.max, false);
        mois.push(MoisStation {
            annee,
            mois: m,
            jours_observes: tranche.len(),
            jours_du_mois,
            complet: tranche.len() as f64 >= COUVERTURE_MOIS_COMPLET * jours_du_mois as f64,
            moy_c: moyenne_mesures(tranche),
            moy_min_c: moyenne(tranche.iter().map(|j| j.min)),
            moy_max_c: moyenne(tranche.iter().map(|j| j.max)),
            ext_min_c,
            ext_min_date,
            ext_max_c,
            ext_max_date,
            jours_chauds: tranche.iter().filter(|j| j.max >= seuils.chaud_c).count(),
            jours_gel: tranche.iter().filter(|j| j.min <= seuils.gel_c).count(),
        });
        debut = fin;
    }

    let complets: Vec<&MoisStation> = mois.iter().filter(|m| m.complet).collect();
    let (ext_min_c, ext_min_date) = extreme(&valides, |j| j.min, true);
    let (ext_max_c, ext_max_date) = extreme(&valides, |j| j.max, false);
    let periode = SynthesePeriode {
        jours_observes: valides.len(),
        mois_observes: mois.len(),
        mois_complets: complets.len(),
        moy_c: moyenne_mesures(&valides),
        moy_min_c: moyenne(valides.iter().map(|j| j.min)),
        moy_max_c: moyenne(valides.iter().map(|j| j.max)),
        ext_min_c,
        ext_min_date,
        ext_max_c,
        ext_max_date,
        jours_chauds_par_mois: moyenne(complets.iter().map(|m| m.jours_chauds as f64)),
        jours_gel_par_mois: moyenne(complets.iter().map(|m| m.jours_gel as f64)),
    };

    let normales = (1..=12u32)
        .map(|numero| {
            let jours_m: Vec<&DayAgg> = valides.iter().copied().filter(|j| j.date().month() == numero).collect();
            let mois_m: Vec<&MoisStation> = mois.iter().filter(|m| m.mois == numero).collect();
            let complets_m: Vec<&MoisStation> = mois_m.iter().copied().filter(|m| m.complet).collect();
            let (ext_min_c, ext_min_date) = extreme(&jours_m, |j| j.min, true);
            let (ext_max_c, ext_max_date) = extreme(&jours_m, |j| j.max, false);
            NormaleMois {
                mois: numero,
                annees: mois_m.len(),
                mois_complets: complets_m.len(),
                jours_observes: jours_m.len(),
                moy_c: moyenne_mesures(&jours_m),
                moy_min_c: moyenne(jours_m.iter().map(|j| j.min)),
                moy_max_c: moyenne(jours_m.iter().map(|j| j.max)),
                ext_min_c,
                ext_min_date,
                ext_max_c,
                ext_max_date,
                jours_chauds_par_mois: moyenne(complets_m.iter().map(|m| m.jours_chauds as f64)),
                jours_gel_par_mois: moyenne(complets_m.iter().map(|m| m.jours_gel as f64)),
            }
        })
        .collect();
    (mois, normales, periode)
}

/// Bilan mensuel de plusieurs stations, du `du` au `au` inclus (dates UTC).
pub fn construire(db: &Db, point_ids: &[String], du: NaiveDate, au: NaiveDate, seuils: Seuils) -> AppResult<BilanMensuel> {
    let debut = std::time::Instant::now();
    if !seuils.chaud_c.is_finite() || !seuils.gel_c.is_finite() {
        return Err(AppError::Other("Seuils de température invalides.".to_string()));
    }
    let (du, au) = if du <= au { (du, au) } else { (au, du) };

    let mut stations = Vec::with_capacity(point_ids.len());
    for id in point_ids {
        let point = db.get_point(id)?;
        let jours = jours_de_fenetre(db, id, minuit(du), fin_de_journee(au))?;
        let (mois, normales, periode) = bilan_des_jours(&jours, seuils);
        stations.push(BilanStation {
            point_id: point.id,
            name: point.name,
            altitude_m: point.altitude_m,
            mois,
            normales,
            periode,
        });
    }

    Ok(BilanMensuel {
        du,
        au,
        seuils,
        couverture_mois_complet: COUVERTURE_MOIS_COMPLET,
        stations,
        elapsed_ms: debut.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    /// Un jour d'agrégat, minimale et maximale données.
    fn jour(a: i32, m: u32, j: u32, min: f64, max: f64) -> DayAgg {
        let date = NaiveDate::from_ymd_opt(a, m, j).unwrap();
        let debut = Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap());
        DayAgg {
            day: debut.timestamp().div_euclid(86_400),
            n: 2,
            sum: min + max,
            min,
            max,
            first_utc: debut,
            last_utc: debut + chrono::Duration::hours(23),
        }
    }

    fn approx(a: Option<f64>, b: f64) {
        let a = a.expect("valeur attendue");
        assert!((a - b).abs() < 1e-9, "attendu {b}, obtenu {a}");
    }

    #[test]
    fn longueur_des_mois() {
        assert_eq!(jours_dans_le_mois(2024, 2), 29);
        assert_eq!(jours_dans_le_mois(2026, 2), 28);
        assert_eq!(jours_dans_le_mois(2026, 12), 31);
        assert_eq!(jours_dans_le_mois(2026, 9), 30);
    }

    #[test]
    fn un_mois_complet_compte_ses_jours_chauds_et_de_gel() {
        // Juillet 2025 : 31 jours, maximale 20 + j % 10 (25 atteint 15 fois),
        // minimale -1 le 1er et le 2, 10 ensuite.
        let jours: Vec<DayAgg> = (1..=31)
            .map(|j| jour(2025, 7, j, if j <= 2 { -1.0 } else { 10.0 }, 20.0 + (j % 10) as f64))
            .collect();
        let (mois, _, periode) = bilan_des_jours(&jours, Seuils::default());
        assert_eq!(mois.len(), 1);
        let m = &mois[0];
        assert_eq!((m.annee, m.mois, m.jours_observes, m.jours_du_mois), (2025, 7, 31, 31));
        assert!(m.complet);
        // j % 10 ∈ {5..9} : jours 5-9, 15-19, 25-29 → 15 jours ≥ 25 °C.
        assert_eq!(m.jours_chauds, 15);
        assert_eq!(m.jours_gel, 2);
        assert_eq!(m.ext_min_c, Some(-1.0));
        assert_eq!(m.ext_min_date, NaiveDate::from_ymd_opt(2025, 7, 1));
        assert_eq!(m.ext_max_c, Some(29.0));
        assert_eq!(m.ext_max_date, NaiveDate::from_ymd_opt(2025, 7, 9));
        approx(m.moy_min_c, (2.0 * -1.0 + 29.0 * 10.0) / 31.0);
        approx(periode.jours_chauds_par_mois, 15.0);
        approx(periode.jours_gel_par_mois, 2.0);
    }

    #[test]
    fn un_mois_incomplet_ne_compte_pas_dans_la_moyenne_par_mois() {
        let mut jours: Vec<DayAgg> = (1..=30).map(|j| jour(2026, 1, j, -5.0, 2.0)).collect();
        // Février : seulement 10 jours, tous de gel.
        jours.extend((1..=10).map(|j| jour(2026, 2, j, -3.0, 4.0)));
        let (mois, _, periode) = bilan_des_jours(&jours, Seuils::default());
        assert_eq!(mois.len(), 2);
        assert!(mois[0].complet, "30 jours sur 31 : au-dessus de 90 %");
        assert!(!mois[1].complet);
        assert_eq!(mois[1].jours_gel, 10);
        assert_eq!((periode.mois_observes, periode.mois_complets), (2, 1));
        // Seul janvier compte : 30 jours de gel, pas (30 + 10) / 2.
        approx(periode.jours_gel_par_mois, 30.0);
        assert_eq!(periode.jours_observes, 40);
        assert_eq!(periode.ext_min_c, Some(-5.0));
        assert_eq!(periode.ext_max_c, Some(4.0));
    }

    #[test]
    fn les_seuils_sont_inclusifs_et_reglables() {
        let jours = vec![jour(2026, 3, 1, 0.0, 25.0), jour(2026, 3, 2, 0.1, 24.9)];
        let (mois, _, _) = bilan_des_jours(&jours, Seuils::default());
        assert_eq!((mois[0].jours_chauds, mois[0].jours_gel), (1, 1));
        let (mois, _, _) = bilan_des_jours(&jours, Seuils { chaud_c: 30.0, gel_c: -2.0 });
        assert_eq!((mois[0].jours_chauds, mois[0].jours_gel), (0, 0));
    }

    #[test]
    fn rien_a_lire_donne_une_synthese_vide() {
        let (mois, normales, periode) = bilan_des_jours(&[], Seuils::default());
        assert!(mois.is_empty());
        assert_eq!(normales.len(), 12);
        assert!(normales.iter().all(|n| n.annees == 0 && n.moy_c.is_none()));
        assert_eq!(periode.jours_observes, 0);
        assert_eq!(periode.moy_min_c, None);
        assert_eq!(periode.jours_gel_par_mois, None);
    }

    #[test]
    fn les_mois_sortent_dans_l_ordre_meme_si_les_jours_arrivent_melanges() {
        let jours = vec![jour(2026, 5, 3, 5.0, 15.0), jour(2025, 12, 31, -2.0, 3.0), jour(2026, 1, 1, -4.0, 1.0)];
        let (mois, _, _) = bilan_des_jours(&jours, Seuils::default());
        let cles: Vec<(i32, u32)> = mois.iter().map(|m| (m.annee, m.mois)).collect();
        assert_eq!(cles, vec![(2025, 12), (2026, 1), (2026, 5)]);
    }

    #[test]
    fn les_normales_moyennent_chaque_mois_sur_les_annees() {
        // Janvier 2024 complet (10 jours de gel), janvier 2025 complet (31 jours
        // de gel), janvier 2026 partiel (5 jours à -10 °C), un jour de juillet.
        let mut jours: Vec<DayAgg> =
            (1..=31).map(|j| jour(2024, 1, j, if j <= 10 { -6.0 } else { 1.0 }, 2.0)).collect();
        jours.extend((1..=31).map(|j| jour(2025, 1, j, -2.0, 4.0)));
        jours.extend((1..=5).map(|j| jour(2026, 1, j, -10.0, 0.0)));
        jours.push(jour(2025, 7, 14, 15.0, 31.0));
        let (_, normales, periode) = bilan_des_jours(&jours, Seuils::default());

        let janv = &normales[0];
        assert_eq!((janv.mois, janv.annees, janv.mois_complets, janv.jours_observes), (1, 3, 2, 67));
        approx(janv.moy_max_c, (31.0 * 2.0 + 31.0 * 4.0) / 67.0);
        let somme: f64 = jours.iter().filter(|j| j.date().month() == 1).map(|j| j.sum).sum();
        approx(janv.moy_c, somme / (67.0 * 2.0));
        assert_eq!(janv.ext_min_c, Some(-10.0));
        assert_eq!(janv.ext_min_date, NaiveDate::from_ymd_opt(2026, 1, 1));
        // 10 (2024) et 31 (2025) ; janvier 2026, partiel, ne compte pas.
        approx(janv.jours_gel_par_mois, 20.5);

        let juillet = &normales[6];
        assert_eq!((juillet.annees, juillet.mois_complets), (1, 0));
        assert_eq!(juillet.jours_chauds_par_mois, None, "aucun juillet complet");
        assert_eq!(juillet.ext_max_c, Some(31.0));
        assert_eq!(normales[2].annees, 0);
        assert!(periode.moy_c.is_some());
    }
}

//! Détection de **sauts de température anormalement rapides**.
//!
//! ## D'où vient ce besoin
//!
//! L'analyse de l'export réel du RC-5 (`EFE205004333`, 10 307 mesures du 1er mai
//! au 16 août 2026, cadence 15 min) a relevé un saut de **+3,8 °C en 15 minutes**
//! dans la toute dernière heure de la série. Aucune situation météo ne fait cela
//! à cet endroit et à cette saison : ce qui explique la courbe, c'est le
//! **retrait du logger** — sorti de son abri, il finit dans une poche ou dans un
//! véhicule pendant que l'enregistrement continue. Les dernières mesures ne
//! décrivent alors plus le site, mais le trajet du relève-données.
//!
//! ## Ce que ce module fait — et ce qu'il ne fait pas
//!
//! Il **signale**, il n'écarte pas. La mesure part en base comme les autres et
//! reste visible dans les statistiques ; l'utilisateur voit un avertissement dans
//! le rapport d'import et décide lui-même de la supprimer ou de la garder.
//!
//! C'est délibéré, pour deux raisons :
//!
//! * un vrai phénomène météo *peut* produire un saut brutal — une rafale de foehn
//!   ou la rupture d'une inversion nocturne font monter la température de
//!   plusieurs degrés en quelques minutes, et ce sont précisément les épisodes
//!   qui intéressent la chasse aux TAF. Les effacer d'office reviendrait à
//!   gommer le sujet d'étude ;
//! * un filtre automatique qui supprime des données sans trace est le meilleur
//!   moyen de rendre une série inexplicable six mois plus tard.
//!
//! ## Le seuil
//!
//! Par défaut : **plus de 3 °C d'écart entre deux mesures consécutives séparées
//! de moins de 20 minutes**. Les deux bornes vont ensemble — 3 °C en une heure
//! est banal, 3 °C en un quart d'heure ne l'est pas.
//!
//! Ce couple est calibré sur la cadence du RC-5 (15 min) : il laisse passer
//! l'écart normal entre deux mesures voisines, tout en attrapant le saut de fin
//! de série. Sur les 10 306 écarts du fichier réel, il n'en retient qu'une
//! poignée. Un seuil plus bas noierait le signal ; plus haut, il manquerait le
//! cas qui a motivé le module.

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::elitech_import::RawMeasurement;
use crate::model::Measurement;

/// Seuils de détection. Modifiables, mais jamais devinés par le code.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct JumpThresholds {
    /// Écart de température, en valeur absolue, au-delà duquel on signale.
    pub min_delta_c: f64,
    /// Durée maximale entre les deux mesures pour que l'écart soit « rapide ».
    pub max_gap_seconds: i64,
}

impl Default for JumpThresholds {
    fn default() -> Self {
        JumpThresholds {
            min_delta_c: 3.0,
            max_gap_seconds: 20 * 60,
        }
    }
}

/// Un saut retenu, décrit assez précisément pour que l'utilisateur puisse aller
/// regarder les deux mesures concernées dans le fichier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemperatureJump {
    /// Ligne (1-based) de la **seconde** mesure dans le fichier source, quand la
    /// source en a une. `0` pour une série relue en base.
    pub source_line: usize,
    /// Horodatage de la mesure précédente, tel qu'il figure dans le fichier.
    pub from_timestamp: NaiveDateTime,
    pub to_timestamp: NaiveDateTime,
    pub from_celsius: f64,
    pub to_celsius: f64,
    /// `to - from`, **signé** : un retrait de logger monte, une remise en place
    /// descend. Le signe fait partie du diagnostic.
    pub delta_c: f64,
    pub gap_seconds: i64,
    /// Phrase prête à afficher, rédigée ici comme tous les messages destinés à
    /// l'utilisateur.
    pub message: String,
}

/// Les sauts trouvés dans une suite de mesures **triée par instant croissant**.
///
/// La suite n'est pas retriée : les parseurs la produisent dans l'ordre du
/// fichier, et un fichier désordonné est une anomalie en soi, que les
/// avertissements de parsing signalent déjà. Retrier ici masquerait ce défaut.
pub fn detect_jumps_raw(
    measurements: &[RawMeasurement],
    thresholds: &JumpThresholds,
) -> Vec<TemperatureJump> {
    detect(
        measurements.iter().map(|m| {
            (
                m.raw_timestamp,
                m.temperature_celsius,
                m.source_line,
            )
        }),
        thresholds,
    )
}

/// Même détection, sur des mesures déjà passées en base.
///
/// Utilise `timestamp_utc` (l'instant vrai) et non le brut : deux mesures peuvent
/// être voisines en brut sans l'être en UTC si la série a été recalibrée.
pub fn detect_jumps(
    measurements: &[Measurement],
    thresholds: &JumpThresholds,
) -> Vec<TemperatureJump> {
    detect(
        measurements
            .iter()
            .map(|m| (m.timestamp_utc.naive_utc(), m.temperature_celsius, 0)),
        thresholds,
    )
}

fn detect<I>(points: I, thresholds: &JumpThresholds) -> Vec<TemperatureJump>
where
    I: IntoIterator<Item = (NaiveDateTime, f64, usize)>,
{
    let mut sauts = Vec::new();
    let mut precedent: Option<(NaiveDateTime, f64)> = None;

    for (instant, temperature, ligne) in points {
        // Un NaN ne se compare à rien : il ne prouve ni saut ni continuité, et le
        // laisser passer produirait des `delta` NaN dans le rapport.
        if !temperature.is_finite() {
            continue;
        }

        if let Some((instant_precedent, temperature_precedente)) = precedent {
            let ecart_secondes = (instant - instant_precedent).num_seconds();
            let delta = temperature - temperature_precedente;

            // `ecart_secondes > 0` écarte deux mesures au même instant (dont le
            // « saut » serait une contradiction de la source, pas un phénomène)
            // et un fichier qui remonterait le temps.
            if ecart_secondes > 0
                && ecart_secondes <= thresholds.max_gap_seconds
                && delta.abs() > thresholds.min_delta_c
            {
                sauts.push(TemperatureJump {
                    source_line: ligne,
                    from_timestamp: instant_precedent,
                    to_timestamp: instant,
                    from_celsius: temperature_precedente,
                    to_celsius: temperature,
                    delta_c: delta,
                    gap_seconds: ecart_secondes,
                    message: message(
                        delta,
                        ecart_secondes,
                        temperature_precedente,
                        temperature,
                        instant,
                    ),
                });
            }
        }

        precedent = Some((instant, temperature));
    }

    sauts
}

fn message(
    delta: f64,
    ecart_secondes: i64,
    avant: f64,
    apres: f64,
    instant: NaiveDateTime,
) -> String {
    let minutes = (ecart_secondes as f64 / 60.0).round() as i64;
    let sens = if delta > 0.0 { "hausse" } else { "baisse" };
    format!(
        "{sens} de {:.1} °C en {minutes} min ({:.1} → {:.1} °C, le {}). \
         Écart trop brutal pour un phénomène de site : vérifiez qu'il ne s'agit pas \
         du logger manipulé (retiré de son abri, mis en poche ou en véhicule). \
         La mesure est conservée — à vous de décider de la retirer.",
        delta.abs(),
        avant,
        apres,
        instant.format("%d/%m/%Y à %H:%M"),
    )
}

/// Instant UTC d'un saut, quand on a besoin de le situer dans la série corrigée.
///
/// Le saut est daté en **brut** (c'est ce que l'utilisateur lit dans le fichier) ;
/// cette fonction applique le décalage d'horloge de la série pour le replacer sur
/// l'axe UTC des statistiques.
pub fn jump_utc(jump: &TemperatureJump, clock_offset_seconds: i64) -> DateTime<Utc> {
    jump.to_timestamp.and_utc() + Duration::seconds(clock_offset_seconds)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn raw(minutes: i64, temperature: f64, ligne: usize) -> RawMeasurement {
        RawMeasurement {
            raw_timestamp: NaiveDate::from_ymd_opt(2026, 8, 16)
                .unwrap()
                .and_hms_opt(20, 0, 0)
                .unwrap()
                + Duration::minutes(minutes),
            temperature_celsius: temperature,
            humidity_percent: None,
            source_line: ligne,
        }
    }

    #[test]
    fn une_serie_calme_ne_declenche_rien() {
        let serie: Vec<_> = (0..20)
            .map(|i| raw(i * 15, 12.0 + (i as f64) * 0.2, i as usize + 2))
            .collect();
        assert!(detect_jumps_raw(&serie, &JumpThresholds::default()).is_empty());
    }

    /// Le cas réel : +3,8 °C en 15 minutes en fin de série (retrait du logger).
    #[test]
    fn le_saut_de_fin_de_serie_est_signale() {
        let serie = vec![
            raw(0, 21.0, 10_305),
            raw(15, 21.2, 10_306),
            raw(30, 25.0, 10_307),
        ];
        let sauts = detect_jumps_raw(&serie, &JumpThresholds::default());

        assert_eq!(sauts.len(), 1);
        let s = &sauts[0];
        assert_eq!(s.source_line, 10_307);
        assert!((s.delta_c - 3.8).abs() < 1e-9);
        assert_eq!(s.gap_seconds, 900);
        assert!(s.message.contains("hausse"));
        assert!(s.message.contains("logger"));
    }

    /// Le même écart, mais étalé sur deux heures : plus rien d'anormal.
    #[test]
    fn le_meme_ecart_sur_deux_heures_ne_declenche_pas() {
        let serie = vec![raw(0, 21.2, 2), raw(120, 25.0, 3)];
        assert!(detect_jumps_raw(&serie, &JumpThresholds::default()).is_empty());
    }

    /// Une chute brutale est tout aussi suspecte qu'une hausse — le signe est
    /// conservé pour que l'utilisateur sache dans quel sens ça s'est produit.
    #[test]
    fn une_chute_brutale_est_signalee_avec_son_signe() {
        let serie = vec![raw(0, 25.0, 2), raw(10, 20.0, 3)];
        let sauts = detect_jumps_raw(&serie, &JumpThresholds::default());
        assert_eq!(sauts.len(), 1);
        assert_eq!(sauts[0].delta_c, -5.0);
        assert!(sauts[0].message.contains("baisse"));
    }

    #[test]
    fn les_seuils_sont_reglables() {
        let serie = vec![raw(0, 12.0, 2), raw(15, 13.5, 3)];
        assert!(detect_jumps_raw(&serie, &JumpThresholds::default()).is_empty());

        let strict = JumpThresholds {
            min_delta_c: 1.0,
            max_gap_seconds: 20 * 60,
        };
        assert_eq!(detect_jumps_raw(&serie, &strict).len(), 1);
    }

    #[test]
    fn deux_mesures_au_meme_instant_ne_sont_pas_un_saut() {
        let serie = vec![raw(0, 12.0, 2), raw(0, 20.0, 3)];
        assert!(detect_jumps_raw(&serie, &JumpThresholds::default()).is_empty());
    }

    #[test]
    fn un_nan_ne_produit_pas_de_saut_nan() {
        let serie = vec![raw(0, 12.0, 2), raw(15, f64::NAN, 3), raw(30, 12.4, 4)];
        assert!(detect_jumps_raw(&serie, &JumpThresholds::default()).is_empty());
    }
}

//! Empreinte de contenu d'une série importée — détection de ré-import.
//!
//! ## Le problème
//!
//! Depuis la v3, `UNIQUE(series_id, timestamp_utc)` interdit deux mesures au
//! même instant *dans une même série*. Rien n'empêchait en revanche de créer une
//! **seconde série complète** à partir d'un fichier déjà importé : deux séries
//! distinctes, mêmes données, statistiques du point faussées par le double
//! comptage. C'est arrivé à l'usage.
//!
//! ## Ce qu'on hache, et pourquoi
//!
//! L'empreinte porte sur les **mesures lues**, pas sur les octets du fichier :
//!
//! * un fichier Elitech ré-enregistré (fins de ligne CRLF → LF, encodage changé
//!   au passage d'un poste à l'autre) donne les mêmes mesures : c'est bien un
//!   ré-import, et un hachage d'octets ne le verrait pas ;
//! * une réponse d'API n'a pas de fichier du tout — sans cela, la voie
//!   Infoclimat resterait sans protection ;
//! * l'horodatage retenu est le **brut** (`raw_timestamp`), pas l'UTC corrigé :
//!   réimporter le même fichier en changeant le fuseau au passage reste un
//!   ré-import. Corriger un décalage après coup se fait sans réimport, avec
//!   `Db::recalibrate_series`.
//!
//! ## Pourquoi un recouvrement partiel ne déclenche rien
//!
//! C'est la contrainte n°1 de cette détection : le mode « récupération
//! automatique » d'Infoclimat redemande volontiers sept jours dont cinq sont
//! déjà connus, et ce complément incrémental doit passer sans alerte.
//!
//! L'empreinte le garantit par construction. Le condensat couvre le nombre de
//! mesures, les deux bornes de la période **et** chaque valeur : deux lots qui ne
//! se recouvrent que partiellement diffèrent forcément sur au moins un de ces
//! trois plans. Seul un lot **strictement identique** — même fichier, ou même
//! période exacte redemandée sans qu'aucune donnée n'ait bougé — retombe sur la
//! même empreinte. Vérifié par les tests de ce module et de `commands`.
//!
//! ## SHA-256
//!
//! `sha2` était déjà dans l'arbre de dépendances (Tauri en dépend, via ses
//! vérifications de mise à jour) : la déclarer en dépendance directe n'ajoute
//! aucune compilation. Aucune propriété cryptographique n'est requise ici — on
//! veut seulement qu'une collision fortuite soit impossible en pratique — mais
//! autant prendre ce qui est déjà là plutôt que d'écrire un condensat maison.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::model::Measurement;

/// Préfixe de version du format haché.
///
/// Il fige la recette : si un jour on décide d'inclure l'humidité autrement ou
/// d'arrondir les températures, passer à `gws-fp-2` évite qu'une ancienne
/// empreinte et une nouvelle se comparent à tort — elles ne pourront plus
/// coïncider.
const VERSION: &str = "gws-fp-1";

/// Ce que l'empreinte lit d'une mesure : l'instant brut et deux valeurs.
///
/// Un trait plutôt qu'un `&[Measurement]` : les imports volumineux (ROMMA,
/// Infoclimat) calculent l'empreinte **directement sur les enregistrements
/// parsés**, sans construire une seconde copie de 477 000 `Measurement` rien que
/// pour la hacher.
pub trait FingerprintSample {
    fn raw_timestamp(&self) -> DateTime<Utc>;
    fn temperature_celsius(&self) -> f64;
    fn humidity_percent(&self) -> Option<f64>;
}

/// Mesure brute d'un fichier : l'horodatage naïf est réinterprété tel quel
/// comme de l'UTC, exactement comme le fait `to_measurements` pour remplir
/// `Measurement::raw_timestamp`. `persist_import` hache donc les lignes lues
/// sans construire un `Vec<Measurement>` temporaire.
impl FingerprintSample for crate::elitech_import::RawMeasurement {
    fn raw_timestamp(&self) -> DateTime<Utc> {
        self.raw_timestamp.and_utc()
    }
    fn temperature_celsius(&self) -> f64 {
        self.temperature_celsius
    }
    fn humidity_percent(&self) -> Option<f64> {
        self.humidity_percent
    }
}

impl FingerprintSample for Measurement {
    fn raw_timestamp(&self) -> DateTime<Utc> {
        self.raw_timestamp
    }
    fn temperature_celsius(&self) -> f64 {
        self.temperature_celsius
    }
    fn humidity_percent(&self) -> Option<f64> {
        self.humidity_percent
    }
}

/// Condensat hexadécimal (64 caractères) du contenu d'un lot de mesures.
///
/// Un lot vide renvoie tout de même une empreinte : c'est au code appelant de
/// refuser un import sans mesure, pas à cette fonction de le deviner.
pub fn series_fingerprint(measurements: &[Measurement]) -> String {
    series_fingerprint_of(measurements)
}

/// [`series_fingerprint`] sur n'importe quel échantillon : **même recette**,
/// donc même condensat pour les mêmes valeurs, quel que soit le type lu.
pub fn series_fingerprint_of<T: FingerprintSample>(measurements: &[T]) -> String {
    let premier = measurements.iter().map(|m| m.raw_timestamp()).min();
    let dernier = measurements.iter().map(|m| m.raw_timestamp()).max();
    let mut empreinte = FingerprintBuilder::new(measurements.len(), premier, dernier);
    for m in measurements {
        empreinte.push(m);
    }
    empreinte.finish()
}

/// L'empreinte calculée **au fil de l'eau**, pour un lot qu'on ne garde pas en
/// mémoire (lecture en flux d'une archive ROMMA).
///
/// L'en-tête du condensat porte le nombre de mesures et les bornes de la
/// période : l'appelant doit donc les connaître **avant** la première mesure —
/// d'où une passe préalable sur le lot. [`series_fingerprint_of`] passe par ici :
/// il n'existe qu'une recette.
pub struct FingerprintBuilder {
    hasher: Sha256,
}

impl FingerprintBuilder {
    /// `premier` / `dernier` : plus petit et plus grand instant **brut** du lot.
    pub fn new(
        count: usize,
        premier: Option<DateTime<Utc>>,
        dernier: Option<DateTime<Utc>>,
    ) -> Self {
        let mut hasher = Sha256::new();
        // En-tête : ce que le prompt appelait « l'empreinte de repli » — nombre de
        // mesures et bornes de la période. Elle ne remplace pas le détail, elle le
        // précède : un lot tronqué ou décalé diverge dès la première ligne du
        // condensat, sans dépendre du contenu.
        hasher.update(VERSION.as_bytes());
        hasher.update(format!("
count={count}").as_bytes());
        hasher.update(format!("
first={}", premier.map(|d| d.timestamp()).unwrap_or(0)).as_bytes());
        hasher.update(format!("
last={}", dernier.map(|d| d.timestamp()).unwrap_or(0)).as_bytes());
        hasher.update(b"
--
");
        FingerprintBuilder { hasher }
    }

    /// Ajoute une mesure, **dans l'ordre du lot**.
    pub fn push<T: FingerprintSample>(&mut self, m: &T) {
        // `{}` sur un `f64` produit la plus courte représentation qui relit à
        // l'identique : deux lectures du même fichier donnent la même chaîne,
        // sans dépendre d'un arrondi choisi ici.
        self.hasher.update(
            format!(
                "{};{};{}
",
                m.raw_timestamp().timestamp(),
                m.temperature_celsius(),
                match m.humidity_percent() {
                    Some(h) => h.to_string(),
                    None => String::new(),
                }
            )
            .as_bytes(),
        );
    }

    pub fn finish(self) -> String {
        let condensat = self.hasher.finalize();
        let mut hex = String::with_capacity(64);
        for octet in condensat {
            use std::fmt::Write;
            let _ = write!(hex, "{octet:02x}");
        }
        hex
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};

    fn instant(iso: &str) -> DateTime<Utc> {
        iso.parse().unwrap()
    }

    /// Fabrique `n` mesures régulières au pas de 10 minutes à partir de `debut`.
    fn serie(debut: &str, n: usize) -> Vec<Measurement> {
        let t0 = instant(debut);
        (0..n)
            .map(|i| {
                let t = t0 + Duration::minutes(10 * i as i64);
                Measurement::new_utc("peu-importe", t, -5.0 - (i as f64) * 0.1)
            })
            .collect()
    }

    #[test]
    fn le_meme_contenu_donne_la_meme_empreinte() {
        let a = serie("2026-01-05T00:00:00Z", 50);
        let b = serie("2026-01-05T00:00:00Z", 50);
        assert_eq!(series_fingerprint(&a), series_fingerprint(&b));
        assert_eq!(series_fingerprint(&a).len(), 64);
    }

    #[test]
    fn l_identifiant_de_serie_n_entre_pas_dans_l_empreinte() {
        // Sans quoi la détection serait impossible : chaque import neuf tire un
        // UUID, donc aucune empreinte ne se répéterait jamais.
        let mut a = serie("2026-01-05T00:00:00Z", 10);
        let b = a.clone();
        for m in &mut a {
            m.series_id = "un-autre-uuid".into();
            m.id = 4242;
        }
        assert_eq!(series_fingerprint(&a), series_fingerprint(&b));
    }

    #[test]
    fn une_seule_valeur_qui_change_suffit_a_diverger() {
        let a = serie("2026-01-05T00:00:00Z", 50);
        let mut b = a.clone();
        b[17].temperature_celsius += 0.1;
        assert_ne!(series_fingerprint(&a), series_fingerprint(&b));

        let mut c = a.clone();
        c[3].humidity_percent = Some(82.0);
        assert_ne!(series_fingerprint(&a), series_fingerprint(&c));
    }

    #[test]
    fn le_decalage_d_horloge_ne_change_pas_l_empreinte() {
        // `timestamp_utc` bouge, `raw_timestamp` non : c'est le même fichier,
        // relu avec un autre fuseau. On veut l'alerte, donc la même empreinte.
        let a = serie("2026-01-05T00:00:00Z", 20);
        let mut b = a.clone();
        for m in &mut b {
            m.timestamp_utc = m.raw_timestamp - Duration::hours(1);
        }
        assert_eq!(series_fingerprint(&a), series_fingerprint(&b));
    }

    /// Le cas que la détection ne doit **jamais** confondre avec un doublon.
    #[test]
    fn un_recouvrement_partiel_donne_une_empreinte_differente() {
        // Sept jours au pas de 10 min, puis les cinq derniers jours + deux jours
        // neufs — exactement le mode « récupération automatique » d'Infoclimat.
        let complet = serie("2026-01-05T00:00:00Z", 7 * 144);
        let chevauchant = serie("2026-01-07T00:00:00Z", 7 * 144);

        assert_ne!(
            series_fingerprint(&complet),
            series_fingerprint(&chevauchant),
            "un complément incrémental ne doit pas passer pour un ré-import"
        );

        // Même début, plus de mesures : le cas d'un export de logger repris plus
        // tard, qui contient tout l'ancien plus le neuf.
        let prolonge = serie("2026-01-05T00:00:00Z", 7 * 144 + 12);
        assert_ne!(series_fingerprint(&complet), series_fingerprint(&prolonge));

        // Et le sous-ensemble strict (mêmes bornes de début, fin plus tôt).
        let tronque = serie("2026-01-05T00:00:00Z", 7 * 144 - 1);
        assert_ne!(series_fingerprint(&complet), series_fingerprint(&tronque));
    }

    #[test]
    fn l_ordre_des_mesures_compte_mais_pas_les_bornes_seules() {
        // Deux lots de même taille et de mêmes bornes, contenus différents :
        // l'en-tête (count/first/last) ne suffirait pas, le détail tranche.
        let a = serie("2026-01-05T00:00:00Z", 30);
        let mut b = a.clone();
        b[5].temperature_celsius = a[6].temperature_celsius;
        b[6].temperature_celsius = a[5].temperature_celsius;
        assert_ne!(series_fingerprint(&a), series_fingerprint(&b));
    }

    #[test]
    fn le_lot_vide_ne_panique_pas() {
        assert_eq!(series_fingerprint(&[]).len(), 64);
    }
}

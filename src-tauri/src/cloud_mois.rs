//! Cloud : une série = un enregistrement de métadonnées + un enregistrement
//! par mois de mesures.
//!
//! Avant la 0.4.2, une série partait en un seul bloc JSON avec toutes ses
//! mesures : ajouter une journée renvoyait la série entière, et la synchro
//! relisait toute la base pour savoir ce qui avait changé. Désormais :
//!
//! * `series:<id de la série>` porte les métadonnées seules (JSON, sans
//!   mesures) ;
//! * `series:<empreinte>` porte un mois de mesures, en binaire compact (`GWM1`).
//!   La table `series_months` (schéma v10) dit quels mois ont changé, sans
//!   relire de mesures ; seul le mois modifié repart.
//!
//! Même préfixe pour les deux : le serveur n'accepte que `project|point|series`
//! suivis d'un UUID (vérifié sur gws_share.py le 25/09/2026). L'empreinte d'un
//! mois est donc mise en forme comme un UUID, et c'est le contenu déchiffré qui
//! dit de quoi il s'agit. Le serveur ne voit ni la série ni le mois.
//!
//! Clients 0.4.0 : un mois ne se lit pas comme du JSON, ils l'ignorent ; des
//! métadonnées sans mesures n'effacent rien chez eux (leur réception ne fait
//! qu'ajouter). Ils gardent leurs données, sans recevoir les nouveaux mois.

use serde::{Deserialize, Serialize};

use crate::model::Measurement;

pub const PREFIXE: &str = "series:";
const MAGIE: &[u8; 4] = b"GWM1";

/// Le contenu déchiffré est-il un mois de mesures (et non des métadonnées) ?
pub fn est_un_mois(clair: &[u8]) -> bool {
    clair.starts_with(MAGIE)
}

/// Clé des métadonnées d'une série.
pub fn cle_meta(series_id: &str) -> String {
    format!("{PREFIXE}{series_id}")
}

/// Clé d'un mois de mesures : empreinte de (série, mois), au format UUID.
pub fn cle_mois(series_id: &str, mois: i64) -> String {
    let h = crate::cloud::sha256_hex(format!("gws-mois\0{series_id}\0{mois}").as_bytes());
    format!("{PREFIXE}{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Repère local d'un mois envoyé ou reçu : `version|série|mois`. La série et le
/// mois servent quand le Cloud annonce la suppression d'un mois, dont la clé
/// seule ne dit rien.
pub fn repere(version: i64, series_id: &str, mois: i64) -> String {
    format!("{version}|{series_id}|{mois}")
}

pub fn lire_repere(r: &str) -> Option<(i64, String, i64)> {
    let mut it = r.splitn(3, '|');
    let v = it.next()?.parse().ok()?;
    let s = it.next()?.to_string();
    let m = it.next()?.parse().ok()?;
    Some((v, s, m))
}

#[derive(Serialize, Deserialize)]
struct Entete {
    s: String,
    m: i64,
}

/// Contenu d'un enregistrement de mois, avant compression et chiffrement.
///
/// `GWM1`, longueur (u16) et en-tête JSON `{s, m}`, nombre de lignes (u32),
/// puis par ligne : instant UTC et instant brut (i64), température (f64),
/// masque des champs présents (u16), et ces champs (f64) dans l'ordre du
/// masque. Petit-boutiste. Sans perte : les valeurs restent en f64.
pub fn encoder(series_id: &str, mois: i64, mesures: &[Measurement]) -> Vec<u8> {
    let entete = serde_json::to_vec(&Entete { s: series_id.to_string(), m: mois }).unwrap_or_default();
    let mut b = Vec::with_capacity(16 + entete.len() + mesures.len() * 40);
    b.extend_from_slice(MAGIE);
    b.extend_from_slice(&(entete.len() as u16).to_le_bytes());
    b.extend_from_slice(&entete);
    b.extend_from_slice(&(mesures.len() as u32).to_le_bytes());
    for m in mesures {
        b.extend_from_slice(&m.timestamp_utc.timestamp().to_le_bytes());
        b.extend_from_slice(&m.raw_timestamp.timestamp().to_le_bytes());
        b.extend_from_slice(&m.temperature_celsius.to_le_bytes());
        let champs = optionnels(m);
        let masque = champs.iter().enumerate().fold(0u16, |a, (i, v)| if v.is_some() { a | (1 << i) } else { a });
        b.extend_from_slice(&masque.to_le_bytes());
        for v in champs.into_iter().flatten() {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    b
}

fn optionnels(m: &Measurement) -> [Option<f64>; 9] {
    [
        m.humidity_percent,
        m.rssi_dbm,
        m.battery,
        m.pressure_hpa,
        m.wind_avg_kmh,
        m.wind_gust_kmh,
        m.wind_direction_degrees,
        m.rain_mm,
        m.solar_radiation,
    ]
}

/// Inverse d'[`encoder`] : (série, mois, mesures).
pub fn decoder(b: &[u8]) -> Result<(String, i64, Vec<Measurement>), String> {
    let mut r = Lecteur { b, i: 0 };
    if r.prendre(4)? != MAGIE {
        return Err("mois Cloud : format inconnu".into());
    }
    let n_entete = u16::from_le_bytes(r.tab()?) as usize;
    let entete: Entete = serde_json::from_slice(r.prendre(n_entete)?).map_err(|e| format!("mois Cloud : {e}"))?;
    let n = u32::from_le_bytes(r.tab()?) as usize;
    // Plafond de cohérence : une ligne fait au moins 26 octets.
    if n > b.len() / 26 + 1 {
        return Err("mois Cloud : nombre de lignes incohérent".into());
    }
    let mut mesures = Vec::with_capacity(n);
    // Un seul identifiant partagé par toutes les mesures du mois.
    let sid: std::sync::Arc<str> = entete.s.as_str().into();
    for _ in 0..n {
        let ts = i64::from_le_bytes(r.tab()?);
        let raw = i64::from_le_bytes(r.tab()?);
        let t = f64::from_le_bytes(r.tab()?);
        let masque = u16::from_le_bytes(r.tab()?);
        let mut v = [None; 9];
        for (i, case) in v.iter_mut().enumerate() {
            if masque & (1 << i) != 0 {
                *case = Some(f64::from_le_bytes(r.tab()?));
            }
        }
        let instant = |s: i64| chrono::DateTime::from_timestamp(s, 0).ok_or("mois Cloud : instant invalide");
        mesures.push(Measurement {
            id: 0,
            series_id: sid.clone(),
            timestamp_utc: instant(ts)?,
            raw_timestamp: instant(raw)?,
            temperature_celsius: t,
            humidity_percent: v[0],
            rssi_dbm: v[1],
            battery: v[2],
            pressure_hpa: v[3],
            wind_avg_kmh: v[4],
            wind_gust_kmh: v[5],
            wind_direction_degrees: v[6],
            rain_mm: v[7],
            solar_radiation: v[8],
        });
    }
    Ok((entete.s, entete.m, mesures))
}

struct Lecteur<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Lecteur<'a> {
    fn prendre(&mut self, n: usize) -> Result<&'a [u8], String> {
        let fin = self.i.checked_add(n).filter(|&f| f <= self.b.len()).ok_or("mois Cloud : contenu tronqué")?;
        let s = &self.b[self.i..fin];
        self.i = fin;
        Ok(s)
    }

    fn tab<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.prendre(N)?.try_into().expect("longueur exacte"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesure(ts: i64, t: f64, h: Option<f64>, vent: Option<f64>) -> Measurement {
        Measurement {
            id: 0,
            series_id: "s".into(),
            timestamp_utc: chrono::DateTime::from_timestamp(ts, 0).unwrap(),
            raw_timestamp: chrono::DateTime::from_timestamp(ts - 3600, 0).unwrap(),
            temperature_celsius: t,
            humidity_percent: h,
            rssi_dbm: None,
            battery: None,
            pressure_hpa: None,
            wind_avg_kmh: vent,
            wind_gust_kmh: None,
            wind_direction_degrees: None,
            rain_mm: None,
            solar_radiation: Some(0.0),
        }
    }

    #[test]
    fn aller_retour_sans_perte() {
        let m = vec![mesure(1_704_067_200, -12.345678901, Some(87.5), None), mesure(1_704_067_800, 0.1, None, Some(12.0))];
        let b = encoder("serie-1", 202401, &m);
        assert!(est_un_mois(&b));
        assert!(!est_un_mois(b"{\"id\":\"x\"}"));
        let (s, mois, lues) = decoder(&b).unwrap();
        assert_eq!((s.as_str(), mois), ("serie-1", 202401));
        assert_eq!(lues.len(), 2);
        for (a, l) in m.iter().zip(&lues) {
            assert_eq!(a.timestamp_utc, l.timestamp_utc);
            assert_eq!(a.raw_timestamp, l.raw_timestamp);
            assert_eq!(a.temperature_celsius.to_bits(), l.temperature_celsius.to_bits());
            assert_eq!(optionnels(a), optionnels(l));
            assert_eq!(&*l.series_id, "serie-1");
        }
    }

    #[test]
    fn contenu_abime_refuse() {
        let b = encoder("s", 202401, &[mesure(1_704_067_200, 1.0, Some(1.0), None)]);
        for coupe in [0, 3, 10, b.len() - 1] {
            assert!(decoder(&b[..coupe]).is_err(), "coupé à {coupe}");
        }
        let mut faux = b.clone();
        faux[0] = b'X';
        assert!(decoder(&faux).is_err());
    }

    #[test]
    fn cles_au_format_du_serveur() {
        // RECORD_KEY_RE de gws_share.py.
        let re = |k: &str| {
            let Some(id) = k.strip_prefix("series:") else { return false };
            let parts: Vec<_> = id.split('-').collect();
            parts.iter().map(|p| p.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
                && parts.iter().all(|p| p.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')))
        };
        let k = cle_mois("0f8b1c9e-0000-4000-8000-000000000000", 202401);
        assert!(re(&k), "{k}");
        assert!(re(&cle_meta("0f8b1c9e-0000-4000-8000-000000000000")));
        assert_ne!(k, cle_mois("0f8b1c9e-0000-4000-8000-000000000000", 202402));
        assert_eq!(k, cle_mois("0f8b1c9e-0000-4000-8000-000000000000", 202401), "stable");
    }

    #[test]
    fn reperes() {
        assert_eq!(lire_repere(&repere(42, "s-1", 202401)), Some((42, "s-1".into(), 202401)));
        assert_eq!(lire_repere("sig:hash"), None);
    }

    #[test]
    fn taille_d_un_mois_a_10_minutes() {
        // 31 jours × 144 mesures, température et humidité : ~40 o par ligne avant gzip.
        let m: Vec<_> = (0..31 * 144).map(|i| mesure(1_704_067_200 + i * 600, 1.0, Some(80.0), None)).collect();
        let b = encoder("s", 202401, &m);
        assert!(b.len() < 31 * 144 * 44, "{}", b.len());
    }
}

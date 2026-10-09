//! Quota Infoclimat partagé, vu du relai SSH (bandeau des Paramètres, 0.5.0).
//!
//! ## Comptage
//!
//! Le compte est tenu **sur le serveur relai**, pas déclaré par l'appareil :
//! chaque appel Infoclimat passé par le relai écrit d'abord, **sur le serveur**,
//! une ligne dans `~/gws-relais/journal/appels.log` (voir [`prefixe_journal`]) :
//!
//! ```text
//! <secondes UTC> <identifiant appareil> <couleur 0-7> <nom en base64>
//! ```
//!
//! Chaque appel compte, réussi ou non (règle de Théo du 29/09).
//!
//! ## Trois familles (décision de Théo du 29/09)
//!
//! * **noir** — requêtes automatiques du serveur : compteur du collecteur,
//!   `~/gws-collecte/quota.json` (`{"date": "AAAA-MM-JJ", "requetes": n}`, jour
//!   de Paris), que le relevé météo en direct incrémente aussi ;
//! * **gris** — requêtes manuelles de l'administrateur : lignes du journal dont
//!   l'identifiant vaut `admin` ;
//! * **une couleur par appareil** — les autres lignes du journal.
//!
//! Le compteur affiché repart à zéro à minuit, heure de Paris, comme celui du
//! collecteur. Plafond affiché : [`PLAFOND`] (4999, « moins de 5000 »).
//!
//! ## Sécurité
//!
//! La commande de lecture est **fixe** ([`commande_lecture`]) et bornée. Le
//! préfixe d'écriture n'interpole que des valeurs validées ici : identifiant
//! hexadécimal, chiffre de couleur, nom encodé en base64 (alphabet sans
//! caractère spécial pour le shell), le tout entre apostrophes.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::error::{AppError, AppResult};

pub const PLAFOND: u32 = 4999;
const SETTING_ID: &str = "appareil_id";
const SETTING_NOM: &str = "appareil_nom";
const SETTING_COULEUR: &str = "appareil_couleur";
const NOM_MAX: usize = 40;
/// Nombre de couleurs prédéfinies (palette côté interface).
pub const NB_COULEURS: u8 = 8;
/// Identifiant réservé aux requêtes manuelles de l'administrateur (gris).
pub const ID_ADMIN: &str = "admin";
/// Plafond de sortie lue sur le canal SSH.
pub const SORTIE_MAX: usize = 1_600_000;
const MARQUEUR: &str = "__GWS_JOURNAL__";
/// Relecture au plus une fois par minute (sauf demande explicite).
const INTERVALLE_LECTURE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentiteAppareil {
    pub id: String,
    pub nom: String,
    pub couleur: u8,
}

impl IdentiteAppareil {
    fn valide(&self) -> bool {
        self.id.len() == 16
            && self.id.chars().all(|c| c.is_ascii_hexdigit())
            && self.couleur < NB_COULEURS
    }
}

fn nom_par_defaut() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|n| nettoyer_nom(&n))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Cet appareil".to_string())
}

fn nettoyer_nom(n: &str) -> String {
    n.trim().chars().filter(|c| !c.is_control()).take(NOM_MAX).collect()
}

/// Identité de cet appareil ; créée au premier appel (identifiant aléatoire,
/// nom de l'ordinateur, couleur au hasard parmi 8).
pub fn identite(db: &Db) -> AppResult<IdentiteAppareil> {
    let id = match db.get_setting(SETTING_ID)? {
        Some(id) if id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit()) => id,
        _ => {
            let id: String = (0..8).map(|_| format!("{:02x}", rand::random::<u8>())).collect();
            db.set_setting(SETTING_ID, &id)?;
            id
        }
    };
    let nom = db
        .get_setting(SETTING_NOM)?
        .map(|n| nettoyer_nom(&n))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(nom_par_defaut);
    let couleur = match db.get_setting(SETTING_COULEUR)?.and_then(|c| c.parse::<u8>().ok()) {
        Some(c) if c < NB_COULEURS => c,
        _ => {
            let c = rand::random::<u8>() % NB_COULEURS;
            db.set_setting(SETTING_COULEUR, &c.to_string())?;
            c
        }
    };
    Ok(IdentiteAppareil { id, nom, couleur })
}

pub fn changer_identite(db: &Db, nom: &str, couleur: u8) -> AppResult<IdentiteAppareil> {
    let nom = nettoyer_nom(nom);
    if nom.is_empty() {
        return Err(AppError::Other("Donnez un nom à cet appareil.".into()));
    }
    if couleur >= NB_COULEURS {
        return Err(AppError::Other("Couleur inconnue.".into()));
    }
    db.set_settings(&[(SETTING_NOM, &nom), (SETTING_COULEUR, &couleur.to_string())])?;
    vider_cache();
    identite(db)
}

/// Préfixe shell ajouté devant le `curl` d'un appel Infoclimat relayé : note
/// l'appel dans le journal **du serveur**. `None` si l'identité est invalide
/// (l'appel part quand même, sans être noté).
pub fn prefixe_journal(a: &IdentiteAppareil) -> Option<String> {
    if !a.valide() {
        return None;
    }
    let nom = base64::engine::general_purpose::STANDARD.encode(nettoyer_nom(&a.nom).as_bytes());
    Some(format!(
        "mkdir -p \"$HOME\"/gws-relais/journal && printf '%s %s %s %s\\n' \"$(date +%s)\" '{}' '{}' '{}' >> \"$HOME\"/gws-relais/journal/appels.log; ",
        a.id, a.couleur, nom
    ))
}

/// Commande de lecture, fixe : compteur du collecteur, puis fin du journal.
pub fn commande_lecture() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| {
        format!(
            "q=\"$HOME\"/gws-collecte/quota.json; if [ -f \"$q\" ]; then head -c 4096 -- \"$q\"; fi; \
             printf '\\n{MARQUEUR}\\n'; \
             j=\"$HOME\"/gws-relais/journal/appels.log; \
             if [ -f \"$j\" ]; then tail -n 20000 -- \"$j\" | head -c {}; fi",
            SORTIE_MAX - 8192
        )
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaAppareil {
    pub id: String,
    pub nom: String,
    pub couleur: u8,
    pub requetes: u32,
    pub moi: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EtatQuota {
    pub plafond: u32,
    /// Jour affiché (Paris).
    pub jour: String,
    pub automatiques: u32,
    pub manuelles: u32,
    pub appareils: Vec<QuotaAppareil>,
    pub total: u32,
    pub lu_le: DateTime<Utc>,
}

/// Décalage de Paris (heure d'été européenne : du dernier dimanche de mars au
/// dernier dimanche d'octobre, 01:00 UTC).
fn decalage_paris(t: DateTime<Utc>) -> i64 {
    fn dernier_dimanche(annee: i32, mois: u32) -> NaiveDate {
        let mut d = NaiveDate::from_ymd_opt(annee, mois, 31).unwrap();
        while d.weekday() != Weekday::Sun {
            d = d.pred_opt().unwrap();
        }
        d
    }
    let a = t.year();
    let debut = Utc.from_utc_datetime(&dernier_dimanche(a, 3).and_hms_opt(1, 0, 0).unwrap());
    let fin = Utc.from_utc_datetime(&dernier_dimanche(a, 10).and_hms_opt(1, 0, 0).unwrap());
    if t >= debut && t < fin {
        7200
    } else {
        3600
    }
}

/// Jour de Paris de `t` et instant UTC de son minuit.
fn jour_paris(t: DateTime<Utc>) -> (NaiveDate, i64) {
    let local = t.timestamp() + decalage_paris(t);
    let jour = DateTime::from_timestamp(local, 0).unwrap().date_naive();
    let minuit_local = jour.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
    // Le décalage à minuit peut différer de celui de `t` les jours de changement d'heure.
    let approx = DateTime::from_timestamp(minuit_local - 3600, 0).unwrap();
    (jour, minuit_local - decalage_paris(approx))
}

#[derive(Deserialize)]
struct QuotaCollecteur {
    date: String,
    requetes: u32,
}

/// Interprète la sortie de [`commande_lecture`].
pub fn interpreter(sortie: &[u8], moi: &IdentiteAppareil, maintenant: DateTime<Utc>) -> EtatQuota {
    let texte = String::from_utf8_lossy(sortie);
    let (quota, journal) = texte.split_once(MARQUEUR).unwrap_or((&texte, ""));
    let (jour, minuit) = jour_paris(maintenant);
    let jour_txt = jour.format("%Y-%m-%d").to_string();

    let automatiques = serde_json::from_str::<QuotaCollecteur>(quota.trim())
        .ok()
        .filter(|q| q.date == jour_txt)
        .map(|q| q.requetes)
        .unwrap_or(0);

    let mut manuelles = 0u32;
    let mut par_appareil: BTreeMap<String, QuotaAppareil> = BTreeMap::new();
    for ligne in journal.lines() {
        let mut champs = ligne.split_whitespace();
        let (Some(ts), Some(id)) = (champs.next().and_then(|t| t.parse::<i64>().ok()), champs.next()) else {
            continue;
        };
        if ts < minuit || ts > maintenant.timestamp() + 300 {
            continue;
        }
        if id == ID_ADMIN {
            manuelles += 1;
            continue;
        }
        if id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        let couleur = champs.next().and_then(|c| c.parse::<u8>().ok()).filter(|c| *c < NB_COULEURS).unwrap_or(0);
        let nom = champs
            .next()
            .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .map(|n| nettoyer_nom(&n))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Appareil".to_string());
        let e = par_appareil.entry(id.to_string()).or_insert_with(|| QuotaAppareil {
            id: id.to_string(),
            nom: nom.clone(),
            couleur,
            requetes: 0,
            moi: id == moi.id,
        });
        // Le nom et la couleur les plus récents l'emportent.
        e.nom = nom;
        e.couleur = couleur;
        e.requetes += 1;
    }
    if let Some(e) = par_appareil.get_mut(&moi.id) {
        e.nom = moi.nom.clone();
        e.couleur = moi.couleur;
    }
    let mut appareils: Vec<QuotaAppareil> = par_appareil.into_values().collect();
    appareils.sort_by(|a, b| b.moi.cmp(&a.moi).then(b.requetes.cmp(&a.requetes)));
    let total = automatiques + manuelles + appareils.iter().map(|a| a.requetes).sum::<u32>();
    EtatQuota { plafond: PLAFOND, jour: jour_txt, automatiques, manuelles, appareils, total, lu_le: maintenant }
}

fn cache() -> &'static Mutex<Option<(Instant, EtatQuota)>> {
    static C: OnceLock<Mutex<Option<(Instant, EtatQuota)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

fn vider_cache() {
    *cache().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// État du quota partagé ; `None` si le relai n'est pas activé.
pub async fn lire(db: &Db, forcer: bool) -> AppResult<Option<EtatQuota>> {
    let Some(relais) = crate::ssh_relay::load_config(db)?.filter(|c| c.enabled) else {
        return Ok(None);
    };
    if !forcer {
        if let Some((quand, etat)) = cache().lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if quand.elapsed() < INTERVALLE_LECTURE {
                return Ok(Some(etat.clone()));
            }
        }
    }
    let moi = identite(db)?;
    let (_, sortie, _) = crate::ssh_relay::executer_commande_fixe_bornee(&relais, commande_lecture(), SORTIE_MAX)
        .await
        .map_err(|e| AppError::Other(format!("Quota partagé illisible : {e}")))?;
    let etat = interpreter(&sortie, &moi, Utc::now());
    *cache().lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), etat.clone()));
    Ok(Some(etat))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moi() -> IdentiteAppareil {
        IdentiteAppareil { id: "0123456789abcdef".into(), nom: "PC Théo".into(), couleur: 3 }
    }

    #[test]
    fn prefixe_sans_caractere_dangereux() {
        let mut a = moi();
        a.nom = "a'; rm -rf ~ #".into();
        let p = prefixe_journal(&a).unwrap();
        assert!(!p.contains("rm -rf"), "le nom doit être encodé : {p}");
        assert!(p.contains("'0123456789abcdef' '3' '"));
        a.id = "zz'".into();
        assert!(prefixe_journal(&a).is_none());
    }

    #[test]
    fn heure_de_paris() {
        // 2 octobre 2026, 23:30 UTC = 3 octobre 01:30 à Paris (été).
        let t = Utc.with_ymd_and_hms(2026, 10, 2, 23, 30, 0).unwrap();
        let (j, minuit) = jour_paris(t);
        assert_eq!(j.to_string(), "2026-10-03");
        assert_eq!(minuit, Utc.with_ymd_and_hms(2026, 10, 2, 22, 0, 0).unwrap().timestamp());
        // Hiver : 15 janvier 23:30 UTC = 16 janvier 00:30.
        let t = Utc.with_ymd_and_hms(2026, 1, 15, 23, 30, 0).unwrap();
        assert_eq!(jour_paris(t).0.to_string(), "2026-01-16");
    }

    #[test]
    fn interpretation_par_famille() {
        let maintenant = Utc.with_ymd_and_hms(2026, 10, 2, 14, 0, 0).unwrap();
        let hier = Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap().timestamp();
        let t = maintenant.timestamp() - 600;
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let sortie = format!(
            "{{\"date\": \"2026-10-02\", \"requetes\": 857}}\n{MARQUEUR}\n\
             {hier} 0123456789abcdef 3 {a}\n\
             {t} 0123456789abcdef 3 {a}\n\
             {t} 0123456789abcdef 3 {a}\n\
             {t} fedcba9876543210 5 {b}\n\
             {t} admin 0 {c}\n\
             ligne illisible\n",
            a = b64("Ancien nom"),
            b = b64("Portable Nils"),
            c = b64("admin"),
        );
        let e = interpreter(sortie.as_bytes(), &moi(), maintenant);
        assert_eq!(e.automatiques, 857);
        assert_eq!(e.manuelles, 1);
        assert_eq!(e.appareils.len(), 2);
        assert!(e.appareils[0].moi);
        assert_eq!(e.appareils[0].requetes, 2, "la ligne d'hier ne compte pas");
        assert_eq!(e.appareils[0].nom, "PC Théo");
        assert_eq!(e.appareils[1].nom, "Portable Nils");
        assert_eq!(e.total, 857 + 1 + 3);
        assert_eq!(e.plafond, 4999);
    }

    #[test]
    fn compteur_du_collecteur_d_un_autre_jour_ignore() {
        let maintenant = Utc.with_ymd_and_hms(2026, 10, 3, 8, 0, 0).unwrap();
        let e = interpreter(b"{\"date\": \"2026-10-02\", \"requetes\": 4000}\n__GWS_JOURNAL__\n", &moi(), maintenant);
        assert_eq!(e.automatiques, 0);
    }
}

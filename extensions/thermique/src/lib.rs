//! Extension « Analyse thermique » : gradient vertical, épisodes radiatifs,
//! profil vertical. Lecture seule de la base ; calculs déterministes dans
//! [`calc`], le panneau (`ui/`) ne fait qu'afficher.
//!
//! Garde-fous de volume (la base finale compte des centaines de millions de
//! lignes) : chaque lecture de mesures est bornée par `series_id` + plage de
//! temps (index unique `(series_id, timestamp_utc)`), la plage est limitée à
//! [`MAX_FENETRE_JOURS`], le nombre de points à [`MAX_POINTS`], et les séries
//! renvoyées au panneau sont agrégées au-delà de [`MAX_SERIE`] valeurs.

pub mod calc;

use gws_extension_api::{declare_extension, Extension, HostContext};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{json, Value};

use calc::Station;

pub const MAX_FENETRE_JOURS: i64 = 400;
pub const MAX_POINTS: usize = 30;
pub const MAX_SERIE: usize = 3000;
/// Fenêtre maximale de moyenne pour le profil vertical.
pub const MAX_FENETRE_PROFIL_S: i64 = 7 * 86400;
/// Pas de la grille commune du gradient (défaut) et bornes acceptées.
pub const PAS_DEFAUT_S: i64 = 3600;
const PAS_BORNES: (i64, i64) = (600, 86400);
/// Tolérance d'appariement : mesure la plus proche à ± max(intervalle/2, 15 min),
/// plafonnée à la moitié du pas de la grille (une mesure ne sert qu'à un instant).
pub const TOLERANCE_MIN_S: i64 = 900;

#[derive(Default)]
pub struct Thermique {
    db_uri: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SerieInfo {
    pub id: String,
    pub role: String,
    pub source_type: String,
    pub intervalle_s: i64,
    pub label: Option<String>,
    pub t_min: i64,
    pub t_max: i64,
    /// Série de résumés horaires dont la température est le MINIMUM de l'heure.
    pub minimum_horaire: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PointInfo {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub altitude_m: Option<f64>,
    pub category: String,
    pub series: Vec<SerieInfo>,
}

impl Extension for Thermique {
    fn init(&mut self, ctx: HostContext) -> Result<(), String> {
        self.db_uri = ctx.db_uri;
        self.ouvrir().map(|_| ())
    }

    fn call(&mut self, method: &str, p: Value) -> Result<Value, String> {
        let c = self.ouvrir()?;
        match method {
            "ping" => Ok(json!("pong")),
            "points" => points(&c, projet(&p)?).map(|v| json!({ "points": v })),
            "gradient" => gradient(&c, &p),
            "episodes" => episodes(&c, &p),
            "courbe" => courbe(&c, &p),
            "profil" => profil(&c, &p),
            m => Err(format!("méthode inconnue : {m}")),
        }
    }
}

impl Thermique {
    fn ouvrir(&self) -> Result<Connection, String> {
        Connection::open_with_flags(
            &self.db_uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("ouverture de la base : {e}"))
    }
}

fn projet(p: &Value) -> Result<&str, String> {
    p.get("project_id").and_then(Value::as_str).ok_or_else(|| "aucun projet actif".into())
}

fn entier(p: &Value, cle: &str) -> Result<i64, String> {
    p.get(cle).and_then(Value::as_f64).map(|x| x as i64).ok_or_else(|| format!("paramètre « {cle} » manquant"))
}

fn plage(p: &Value) -> Result<(i64, i64), String> {
    let (a, b) = (entier(p, "debut")?, entier(p, "fin")?);
    if b <= a {
        return Err("la fin doit suivre le début".into());
    }
    if b - a > MAX_FENETRE_JOURS * 86400 {
        return Err(format!("période limitée à {MAX_FENETRE_JOURS} jours"));
    }
    Ok((a, b))
}

fn est_minimum_horaire(label: &Option<String>) -> bool {
    label.as_deref().is_some_and(|l| l.to_lowercase().contains("minimum"))
}

/// Points du projet ayant au moins une série non vide. Les bornes de chaque
/// série viennent de MIN/MAX sur l'index (deux recherches, pas de scan).
pub fn points(c: &Connection, projet: &str) -> Result<Vec<PointInfo>, String> {
    let e = |e: rusqlite::Error| e.to_string();
    let mut st = c
        .prepare("SELECT id, name, latitude, longitude, altitude_m, category FROM points WHERE project_id = ?1 ORDER BY altitude_m")
        .map_err(e)?;
    let pts: Vec<PointInfo> = st
        .query_map([projet], |r| {
            Ok(PointInfo {
                id: r.get(0)?,
                name: r.get(1)?,
                lat: r.get(2)?,
                lon: r.get(3)?,
                altitude_m: r.get(4)?,
                category: r.get(5)?,
                series: Vec::new(),
            })
        })
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)?;
    let mut ser = c
        .prepare(
            "SELECT s.id, s.role, s.source_type, s.sample_interval_seconds, s.label,
                    (SELECT MIN(timestamp_utc) FROM measurements m WHERE m.series_id = s.id),
                    (SELECT MAX(timestamp_utc) FROM measurements m WHERE m.series_id = s.id)
             FROM logger_series s WHERE s.point_id = ?1",
        )
        .map_err(e)?;
    let mut out = Vec::new();
    for mut p in pts {
        let series: Vec<SerieInfo> = ser
            .query_map([&p.id], |r| {
                let label: Option<String> = r.get(4)?;
                Ok((
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    SerieInfo {
                        id: r.get(0)?,
                        role: r.get(1)?,
                        source_type: r.get(2)?,
                        intervalle_s: r.get::<_, Option<i64>>(3)?.unwrap_or(600).max(1),
                        minimum_horaire: est_minimum_horaire(&label),
                        label,
                        t_min: 0,
                        t_max: 0,
                    },
                ))
            })
            .map_err(e)?
            .filter_map(|r| match r {
                Ok((Some(a), Some(b), mut s)) => {
                    s.t_min = a;
                    s.t_max = b;
                    Some(Ok(s))
                }
                Ok(_) => None,
                Err(x) => Some(Err(x)),
            })
            .collect::<Result<_, _>>()
            .map_err(e)?;
        if !series.is_empty() {
            p.series = series;
            out.push(p);
        }
    }
    Ok(out)
}

/// Série retenue pour un point sur [a, b] : jamais de mélange Primary/Backup
/// (double comptage). Primary d'abord ; parmi elles, le plus grand nombre de
/// mesures attendu sur la plage (recouvrement / intervalle) ; Backup seulement
/// s'il n'y a aucune Primary couvrant la plage.
pub fn choisir_serie(p: &PointInfo, a: i64, b: i64) -> Option<&SerieInfo> {
    let estim = |s: &SerieInfo| (s.t_max.min(b) - s.t_min.max(a)).max(0) / s.intervalle_s;
    let meilleure = |role: &str| {
        p.series.iter().filter(|s| s.role == role && estim(s) > 0).max_by_key(|s| (estim(s), s.t_max))
    };
    meilleure("Primary").or_else(|| meilleure("Backup"))
}

/// Mesures (t, température, vent) d'une série sur [a, b], via l'index.
pub fn mesures(c: &Connection, serie: &str, a: i64, b: i64) -> Result<(Vec<(i64, f64)>, Vec<(i64, f64)>), String> {
    let e = |e: rusqlite::Error| e.to_string();
    let mut st = c
        .prepare_cached(
            "SELECT timestamp_utc, temperature_celsius, wind_avg_kmh FROM measurements
             WHERE series_id = ?1 AND timestamp_utc BETWEEN ?2 AND ?3 ORDER BY timestamp_utc",
        )
        .map_err(e)?;
    let mut t = Vec::new();
    let mut v = Vec::new();
    let mut rows = st.query(rusqlite::params![serie, a, b]).map_err(e)?;
    while let Some(r) = rows.next().map_err(e)? {
        let ts: i64 = r.get(0).map_err(e)?;
        let temp: Option<f64> = r.get(1).map_err(e)?;
        if let Some(x) = temp.filter(|x| x.is_finite()) {
            t.push((ts, x));
        }
        if let Some(w) = r.get::<_, Option<f64>>(2).map_err(e)?.filter(|x| x.is_finite()) {
            v.push((ts, w));
        }
    }
    Ok((t, v))
}

fn ids(p: &Value) -> Result<Vec<String>, String> {
    let v: Vec<String> = p
        .get("points")
        .and_then(Value::as_array)
        .ok_or("paramètre « points » manquant")?
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    if v.len() > MAX_POINTS {
        return Err(format!("au plus {MAX_POINTS} points"));
    }
    Ok(v)
}

/// Points demandés, appartenant au projet, avec altitude et série sur [a, b].
struct Choix {
    point: PointInfo,
    serie: SerieInfo,
}

fn selection(c: &Connection, p: &Value, a: i64, b: i64, avert: &mut Vec<String>) -> Result<Vec<Choix>, String> {
    let voulus = ids(p)?;
    let tous = points(c, projet(p)?)?;
    let mut out = Vec::new();
    for id in &voulus {
        let Some(pt) = tous.iter().find(|x| &x.id == id) else {
            avert.push(format!("point {id} : absent du projet ou sans mesures"));
            continue;
        };
        if pt.altitude_m.is_none() {
            avert.push(format!("{} : altitude inconnue, ignoré", pt.name));
            continue;
        }
        match choisir_serie(pt, a, b) {
            Some(s) => {
                if s.minimum_horaire {
                    avert.push(format!("{} : résumé horaire (température = MINIMUM de l'heure), biais froid", pt.name));
                }
                out.push(Choix { point: pt.clone(), serie: s.clone() })
            }
            None => avert.push(format!("{} : aucune mesure sur la période", pt.name)),
        }
    }
    Ok(out)
}

fn resume(ch: &Choix) -> Value {
    json!({
        "id": ch.point.id, "name": ch.point.name, "altitude_m": ch.point.altitude_m,
        "serie_id": ch.serie.id, "role": ch.serie.role, "source_type": ch.serie.source_type,
        "label": ch.serie.label, "intervalle_s": ch.serie.intervalle_s,
        "minimum_horaire": ch.serie.minimum_horaire,
    })
}

fn centre(ch: &[Choix]) -> (f64, f64) {
    let n = ch.len().max(1) as f64;
    (ch.iter().map(|x| x.point.lat).sum::<f64>() / n, ch.iter().map(|x| x.point.lon).sum::<f64>() / n)
}

pub fn gradient(c: &Connection, p: &Value) -> Result<Value, String> {
    let (a, b) = plage(p)?;
    let pas = p.get("pas").and_then(Value::as_i64).unwrap_or(PAS_DEFAUT_S).clamp(PAS_BORNES.0, PAS_BORNES.1);
    let mut avert = Vec::new();
    let ch = selection(c, p, a, b, &mut avert)?;
    if ch.len() < 2 {
        return Err("il faut au moins 2 points avec altitude et mesures sur la période".into());
    }
    let alts: Vec<f64> = ch.iter().map(|x| x.point.altitude_m.unwrap_or(0.0)).collect();
    let dz = alts.iter().cloned().fold(f64::MIN, f64::max) - alts.iter().cloned().fold(f64::MAX, f64::min);
    if dz < calc::DZ_MIN_M {
        return Err(format!("écart d'altitude de {dz:.0} m : au moins {:.0} m nécessaires", calc::DZ_MIN_M));
    }
    let interv_max = ch.iter().map(|x| x.serie.intervalle_s).max().unwrap_or(600);
    let tol = (interv_max / 2).max(TOLERANCE_MIN_S).min(pas / 2);
    let donnees: Vec<Vec<(i64, f64)>> = ch.iter().map(|x| mesures(c, &x.serie.id, a - tol, b + tol).map(|m| m.0)).collect::<Result<_, _>>()?;
    let stations: Vec<Station> = ch.iter().zip(&donnees).map(|(x, d)| Station { alt: x.point.altitude_m.unwrap_or(0.0), serie: d }).collect();
    let (lat, lon) = centre(&ch);
    let g = calc::gradient(&stations, a, b, pas, tol, lat, lon);
    let stats = calc::stats_gradient(&g);
    let brut: Vec<(i64, f64)> = g.iter().map(|x| (x.t, x.g)).collect();
    let agrege = brut.len() > MAX_SERIE;
    Ok(json!({
        "points": ch.iter().map(resume).collect::<Vec<_>>(),
        "pas_s": pas, "tolerance_s": tol, "dz_m": dz,
        "stats": stats,
        "agrege": agrege,
        "serie": calc::agreger(&brut, MAX_SERIE),
        "avertissements": avert,
    }))
}

pub fn episodes(c: &Connection, p: &Value) -> Result<Value, String> {
    let (a, b) = plage(p)?;
    let id = p.get("point").and_then(Value::as_str).ok_or("paramètre « point » manquant")?;
    let tous = points(c, projet(p)?)?;
    let pt = tous.iter().find(|x| x.id == id).ok_or("point absent du projet ou sans mesures")?;
    let s = choisir_serie(pt, a, b).ok_or("aucune mesure sur la période")?;
    // Marges : l'après-midi précédent et la matinée suivante sont nécessaires.
    let (t, v) = mesures(c, &s.id, a - 8 * 3600, b + 30 * 3600)?;
    let r = calc::episodes(&t, &v, a, b, s.intervalle_s, pt.lat, pt.lon);
    let mut avert = Vec::new();
    if s.minimum_horaire {
        avert.push("Série de résumés horaires : la température est le MINIMUM de chaque heure. Les vitesses sont calculées sur des minima horaires (pas 1 h) : refroidissement et rupture sont lissés, horaires à ±1 h.".to_string());
    } else if s.intervalle_s > 1800 {
        avert.push(format!("Pas de {} min : vitesses lissées, horaires approximatifs.", s.intervalle_s / 60));
    }
    if v.is_empty() {
        avert.push("Pas de vent mesuré : le critère vent n'est pas appliqué.".into());
    }
    Ok(json!({
        "point": { "id": pt.id, "name": pt.name, "altitude_m": pt.altitude_m },
        "serie": s,
        "resultat": r,
        "seuils": {
            "refroidissement_min_c_h": calc::REFROIDISSEMENT_MIN_C_H, "debut_c_h": calc::DEBUT_C_H,
            "amplitude_min_c": calc::AMPLITUDE_MIN_C, "remontee_max_c": calc::REMONTEE_MAX_C,
            "plateau_max_c_h": calc::PLATEAU_MAX_C_H, "rupture_c_h": calc::RUPTURE_C_H,
            "vent_max_kmh": calc::VENT_MAX_KMH, "couverture_min": calc::COUVERTURE_MIN,
        },
        "avertissements": avert,
    }))
}

/// Courbe de température d'un point sur [a, b], agrégée à ≤ 1500 valeurs.
pub fn courbe(c: &Connection, p: &Value) -> Result<Value, String> {
    let (a, b) = plage(p)?;
    let id = p.get("point").and_then(Value::as_str).ok_or("paramètre « point » manquant")?;
    let tous = points(c, projet(p)?)?;
    let pt = tous.iter().find(|x| x.id == id).ok_or("point absent du projet ou sans mesures")?;
    let s = choisir_serie(pt, a, b).ok_or("aucune mesure sur la période")?;
    let (t, _) = mesures(c, &s.id, a, b)?;
    Ok(json!({ "serie": s, "valeurs": calc::agreger(&t, 1500) }))
}

pub fn profil(c: &Connection, p: &Value) -> Result<Value, String> {
    let t = entier(p, "t")?;
    let fen = p.get("fenetre_s").and_then(Value::as_i64).unwrap_or(0).clamp(0, MAX_FENETRE_PROFIL_S);
    let (a, b) = (t - fen / 2, t + fen / 2);
    let mut avert = Vec::new();
    let ch = selection(c, p, a - 3600, b + 3600, &mut avert)?;
    let mut lignes = Vec::new();
    let (mut zs, mut ts) = (Vec::new(), Vec::new());
    for x in &ch {
        let tol = (x.serie.intervalle_s / 2).max(TOLERANCE_MIN_S);
        let (val, n) = if fen == 0 {
            let (m, _) = mesures(c, &x.serie.id, t - tol, t + tol)?;
            (calc::valeur_proche(&m, t, tol), 1)
        } else {
            let (m, _) = mesures(c, &x.serie.id, a, b)?;
            calc::moyenne_fenetre(&m, a, b).map_or((None, 0), |(v, n)| (Some(v), n))
        };
        if let Some(v) = val {
            zs.push(x.point.altitude_m.unwrap_or(0.0));
            ts.push(v);
        }
        let mut r = resume(x);
        r["temperature"] = json!(val);
        r["n"] = json!(if val.is_some() { n } else { 0 });
        lignes.push(r);
    }
    Ok(json!({
        "t": t, "fenetre_s": fen,
        "points": lignes,
        "pente_100m": calc::pente_100m(&zs, &ts),
        "avertissements": avert,
    }))
}

declare_extension!(Thermique);

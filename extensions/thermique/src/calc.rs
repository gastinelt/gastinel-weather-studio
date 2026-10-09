//! Calculs purs (sans base ni réseau), déterministes et testables.
//!
//! Toutes les heures sont des epoch secondes UTC. Les séries sont des vecteurs
//! `(t, valeur)` **triés par t croissant** (c'est l'ordre de l'index SQLite).

use serde::Serialize;

pub type Serie = [(i64, f64)];

// ---------------------------------------------------------------------------
// Soleil (formules approchées de l'Astronomical Almanac, précision ~1 min)
// ---------------------------------------------------------------------------

/// Hauteur du soleil (degrés) à l'instant `t` pour (lat, lon) en degrés.
pub fn hauteur_soleil(t: i64, lat: f64, lon: f64) -> f64 {
    let n = t as f64 / 86400.0 + 2440587.5 - 2451545.0;
    let l = (280.460 + 0.9856474 * n).rem_euclid(360.0);
    let g = (357.528 + 0.9856003 * n).rem_euclid(360.0).to_radians();
    let lambda = (l + 1.915 * g.sin() + 0.020 * (2.0 * g).sin()).to_radians();
    let eps = (23.439 - 0.0000004 * n).to_radians();
    let dec = (eps.sin() * lambda.sin()).asin();
    let ra = (eps.cos() * lambda.sin()).atan2(lambda.cos()).to_degrees();
    let gmst = (18.697374558 + 24.06570982441908 * n).rem_euclid(24.0) * 15.0;
    let h = (gmst + lon - ra).to_radians();
    let phi = lat.to_radians();
    (phi.sin() * dec.sin() + phi.cos() * dec.cos() * h.cos()).asin().to_degrees()
}

/// Hauteur retenue pour le lever/coucher (réfraction + demi-diamètre).
const HAUTEUR_HORIZON: f64 = -0.833;

pub fn est_jour(t: i64, lat: f64, lon: f64) -> bool {
    hauteur_soleil(t, lat, lon) > HAUTEUR_HORIZON
}

/// Midi solaire approché (UTC) du jour civil UTC contenant `t`.
pub fn midi_solaire(t: i64, lon: f64) -> i64 {
    t.div_euclid(86400) * 86400 + 43200 - (lon / 15.0 * 3600.0) as i64
}

/// Coucher puis lever suivant, cherchés par pas de 2 min à partir du midi
/// solaire. `None` en nuit/jour polaire (hors sujet dans les Alpes).
pub fn coucher_lever(midi: i64, lat: f64, lon: f64) -> Option<(i64, i64)> {
    const PAS: i64 = 120;
    let mut t = midi;
    let fin = midi + 86400;
    while t < fin && est_jour(t, lat, lon) {
        t += PAS;
    }
    let coucher = t;
    while t < fin + 43200 && !est_jour(t, lat, lon) {
        t += PAS;
    }
    if coucher >= fin || t >= fin + 43200 {
        return None;
    }
    Some((coucher, t))
}

// ---------------------------------------------------------------------------
// Accès aux séries
// ---------------------------------------------------------------------------

/// Valeur de la mesure la plus proche de `t`, si elle est à moins de `tol` s.
pub fn valeur_proche(s: &Serie, t: i64, tol: i64) -> Option<f64> {
    let i = s.partition_point(|m| m.0 < t);
    let mut best: Option<(i64, f64)> = None;
    for j in [i.wrapping_sub(1), i] {
        if let Some(&(tm, v)) = s.get(j) {
            let d = (tm - t).abs();
            if d <= tol && best.map_or(true, |b| d < b.0) {
                best = Some((d, v));
            }
        }
    }
    best.map(|b| b.1)
}

/// Moyenne des mesures dans [a, b] et leur nombre.
pub fn moyenne_fenetre(s: &Serie, a: i64, b: i64) -> Option<(f64, usize)> {
    let i = s.partition_point(|m| m.0 < a);
    let j = s.partition_point(|m| m.0 <= b);
    if j <= i {
        return None;
    }
    let somme: f64 = s[i..j].iter().map(|m| m.1).sum();
    Some((somme / (j - i) as f64, j - i))
}

/// Pente des moindres carrés de T en fonction de z, en °C / 100 m.
/// `None` si l'écart d'altitude est trop faible (< 1 m).
pub fn pente_100m(z: &[f64], t: &[f64]) -> Option<f64> {
    let n = z.len() as f64;
    if z.len() < 2 || z.len() != t.len() {
        return None;
    }
    let mz = z.iter().sum::<f64>() / n;
    let mt = t.iter().sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (a, b) in z.iter().zip(t) {
        sxy += (a - mz) * (b - mt);
        sxx += (a - mz) * (a - mz);
    }
    if sxx < 1.0 {
        return None;
    }
    Some(sxy / sxx * 100.0)
}

// ---------------------------------------------------------------------------
// Gradient thermique vertical
// ---------------------------------------------------------------------------

/// Écart d'altitude minimal entre le point le plus bas et le plus haut. En
/// dessous, l'incertitude des capteurs (±0,3 à 0,5 °C) domine le signal :
/// 0,5 °C sur 50 m = 1 °C/100 m d'erreur possible.
pub const DZ_MIN_M: f64 = 50.0;

pub struct Station<'a> {
    pub alt: f64,
    pub serie: &'a Serie,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstantGradient {
    pub t: i64,
    /// °C / 100 m ; > 0 = inversion.
    pub g: f64,
    pub jour: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct StatsGradient {
    pub n: usize,
    pub debut: Option<i64>,
    pub fin: Option<i64>,
    pub part_inversion: f64,
    pub part_inversion_nuit: f64,
    pub part_inversion_jour: f64,
    pub moyenne: Option<f64>,
    pub moyenne_nuit: Option<f64>,
    pub moyenne_jour: Option<f64>,
    pub n_nuit: usize,
    pub n_jour: usize,
    pub max_inversion: Option<f64>,
    pub t_max_inversion: Option<i64>,
    pub min_gradient: Option<f64>,
    pub t_min_gradient: Option<i64>,
}

/// Gradient à chaque instant d'une grille régulière de pas `pas` où **toutes**
/// les stations ont une mesure à moins de `tol` secondes. Exiger toutes les
/// stations évite qu'un gradient change de valeur parce qu'un point
/// disparaît de la régression.
pub fn gradient(stations: &[Station], debut: i64, fin: i64, pas: i64, tol: i64, lat: f64, lon: f64) -> Vec<InstantGradient> {
    let alts: Vec<f64> = stations.iter().map(|s| s.alt).collect();
    let mut out = Vec::new();
    let mut t = debut.div_euclid(pas) * pas;
    if t < debut {
        t += pas;
    }
    let mut vals = Vec::with_capacity(stations.len());
    while t <= fin {
        vals.clear();
        for s in stations {
            match valeur_proche(s.serie, t, tol) {
                Some(v) => vals.push(v),
                None => break,
            }
        }
        if vals.len() == stations.len() {
            if let Some(g) = pente_100m(&alts, &vals) {
                out.push(InstantGradient { t, g, jour: est_jour(t, lat, lon) });
            }
        }
        t += pas;
    }
    out
}

pub fn stats_gradient(v: &[InstantGradient]) -> StatsGradient {
    let moy = |it: &mut dyn Iterator<Item = f64>| {
        let (mut s, mut n) = (0.0, 0usize);
        for x in it {
            s += x;
            n += 1;
        }
        (n > 0).then(|| s / n as f64)
    };
    let part = |f: &dyn Fn(&InstantGradient) -> bool| {
        let tot = v.iter().filter(|x| f(x)).count();
        if tot == 0 {
            0.0
        } else {
            v.iter().filter(|x| f(x) && x.g > 0.0).count() as f64 / tot as f64
        }
    };
    let mut st = StatsGradient {
        n: v.len(),
        debut: v.first().map(|x| x.t),
        fin: v.last().map(|x| x.t),
        part_inversion: part(&|_| true),
        part_inversion_nuit: part(&|x| !x.jour),
        part_inversion_jour: part(&|x| x.jour),
        moyenne: moy(&mut v.iter().map(|x| x.g)),
        moyenne_nuit: moy(&mut v.iter().filter(|x| !x.jour).map(|x| x.g)),
        moyenne_jour: moy(&mut v.iter().filter(|x| x.jour).map(|x| x.g)),
        n_nuit: v.iter().filter(|x| !x.jour).count(),
        n_jour: v.iter().filter(|x| x.jour).count(),
        ..Default::default()
    };
    if let Some(m) = v.iter().max_by(|a, b| a.g.total_cmp(&b.g)) {
        if m.g > 0.0 {
            st.max_inversion = Some(m.g);
            st.t_max_inversion = Some(m.t);
        }
    }
    if let Some(m) = v.iter().min_by(|a, b| a.g.total_cmp(&b.g)) {
        st.min_gradient = Some(m.g);
        st.t_min_gradient = Some(m.t);
    }
    st
}

/// Réduit une série longue en `n_max` tranches de même durée (moyenne, min, max).
#[derive(Debug, Clone, Serialize)]
pub struct Tranche {
    pub t: i64,
    pub moy: f64,
    pub min: f64,
    pub max: f64,
}

pub fn agreger(v: &[(i64, f64)], n_max: usize) -> Vec<Tranche> {
    if v.is_empty() {
        return Vec::new();
    }
    if v.len() <= n_max {
        return v.iter().map(|&(t, x)| Tranche { t, moy: x, min: x, max: x }).collect();
    }
    let (a, b) = (v[0].0, v[v.len() - 1].0);
    let largeur = ((b - a) / n_max as i64).max(1);
    let mut out: Vec<Tranche> = Vec::new();
    let (mut cle, mut s, mut n, mut mi, mut ma) = (i64::MIN, 0.0, 0usize, f64::MAX, f64::MIN);
    let pousser = |cle: i64, s: f64, n: usize, mi: f64, ma: f64, out: &mut Vec<Tranche>| {
        if n > 0 {
            out.push(Tranche { t: a + cle * largeur + largeur / 2, moy: s / n as f64, min: mi, max: ma });
        }
    };
    for &(t, x) in v {
        let k = (t - a) / largeur;
        if k != cle {
            pousser(cle, s, n, mi, ma, &mut out);
            cle = k;
            s = 0.0;
            n = 0;
            mi = f64::MAX;
            ma = f64::MIN;
        }
        s += x;
        n += 1;
        mi = mi.min(x);
        ma = ma.max(x);
    }
    pousser(cle, s, n, mi, ma, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Détecteur d'épisodes radiatifs
// ---------------------------------------------------------------------------
// Seuils empiriques, NON calibrés scientifiquement (voir README). Ils visent
// une nuit claire et calme en site de fond de vallée / doline, où l'air se
// refroidit vite après le coucher, se stabilise, puis remonte avec le soleil.

/// Pas de rééchantillonnage minimal (s). Un pas plus fin que la série ne
/// crée pas d'information ; on prend max(intervalle de la série, 10 min).
pub const PAS_MIN_S: i64 = 600;
/// Trou maximal comblé par interpolation linéaire, en nombre de pas.
pub const TROU_MAX_PAS: i64 = 3;
/// Part minimale de la nuit (coucher − 3 h → lever + 3 h) couverte par des
/// données, sinon la nuit est écartée (« données insuffisantes »).
pub const COUVERTURE_MIN: f64 = 0.8;
/// Critère 1 — refroidissement crépusculaire : vitesse maximale de baisse,
/// mesurée sur 1 h glissante entre coucher − 3 h et coucher + 3 h. Une nuit
/// claire et calme dépasse couramment 2 °C/h en fond de vallée.
pub const REFROIDISSEMENT_MIN_C_H: f64 = 1.5;
/// Début de l'épisode : première heure glissante (même fenêtre) dont la
/// baisse atteint cette vitesse.
pub const DEBUT_C_H: f64 = 1.0;
/// Critère 2 — amplitude : Tmax de l'après-midi (coucher − 6 h → coucher)
/// moins Tmin de la nuit.
pub const AMPLITUDE_MIN_C: f64 = 6.0;
/// Critère 3a — pas de réchauffement nocturne : entre le début et le
/// minimum, la température ne remonte jamais de plus de ce seuil au-dessus
/// du minimum déjà atteint (un nuage ou une bouffée de vent la fait remonter).
pub const REMONTEE_MAX_C: f64 = 1.5;
/// Critère 3b — plateau / stabilisation : baisse moyenne sur les 3 h qui
/// précèdent le minimum inférieure à ce seuil (sinon c'est plutôt une
/// advection froide continue qu'un refroidissement radiatif qui sature).
pub const PLATEAU_MAX_C_H: f64 = 0.8;
pub const PLATEAU_DUREE_S: i64 = 3 * 3600;
/// Critère 4 — rupture matinale : première heure glissante après le minimum
/// où la hausse atteint cette vitesse…
pub const RUPTURE_C_H: f64 = 1.0;
/// … et qui a lieu au plus tôt `RUPTURE_AVANCE_MAX_S` avant le lever du
/// soleil (une rupture en pleine nuit trahit du vent ou des nuages) et au plus
/// tard `RUPTURE_RETARD_MAX_S` après.
pub const RUPTURE_AVANCE_MAX_S: i64 = 3600;
pub const RUPTURE_RETARD_MAX_S: i64 = 5 * 3600;
/// Critère 5 (seulement si la série a du vent) : vent moyen nocturne maximal.
pub const VENT_MAX_KMH: f64 = 10.0;

#[derive(Debug, Clone, Serialize)]
pub struct Episode {
    pub coucher: i64,
    pub lever: i64,
    pub debut: i64,
    pub t_min: i64,
    pub tmin: f64,
    pub tmax_veille: f64,
    pub amplitude: f64,
    pub refroidissement_max: f64,
    pub rupture: i64,
    pub duree_h: f64,
    pub vent_moyen: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Rejets {
    pub donnees_insuffisantes: usize,
    pub refroidissement: usize,
    pub amplitude: usize,
    pub remontee: usize,
    pub plateau: usize,
    pub rupture: usize,
    pub vent: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ResultatEpisodes {
    pub nuits_examinees: usize,
    pub pas_s: i64,
    pub episodes: Vec<Episode>,
    pub rejets: Rejets,
}

/// Grille régulière `t0 + i·pas` interpolée linéairement (trous ≤ TROU_MAX_PAS).
struct Grille {
    t0: i64,
    pas: i64,
    v: Vec<Option<f64>>,
}

impl Grille {
    fn new(s: &Serie, t0: i64, t1: i64, pas: i64) -> Grille {
        let n = ((t1 - t0) / pas + 1).max(0) as usize;
        let mut v = vec![None; n];
        let trou = TROU_MAX_PAS * pas;
        for (i, slot) in v.iter_mut().enumerate() {
            let t = t0 + i as i64 * pas;
            let k = s.partition_point(|m| m.0 < t);
            if let Some(&(tk, vk)) = s.get(k) {
                if tk == t {
                    *slot = Some(vk);
                    continue;
                }
                if k > 0 {
                    let (ta, va) = s[k - 1];
                    if tk - ta <= trou {
                        *slot = Some(va + (vk - va) * (t - ta) as f64 / (tk - ta) as f64);
                    }
                }
            }
        }
        Grille { t0, pas, v }
    }
    fn idx(&self, t: i64) -> usize {
        (((t - self.t0) as f64 / self.pas as f64).round().max(0.0) as usize).min(self.v.len().saturating_sub(1))
    }
    fn t(&self, i: usize) -> i64 {
        self.t0 + i as i64 * self.pas
    }
    /// Pas couvrant une heure (au moins 1).
    fn k_heure(&self) -> usize {
        ((3600 / self.pas).max(1)) as usize
    }
    /// Baisse en °C/h entre i et i+k (positive = refroidissement).
    fn baisse(&self, i: usize) -> Option<f64> {
        let k = self.k_heure();
        let a = (*self.v.get(i)?)?;
        let b = (*self.v.get(i + k)?)?;
        Some((a - b) / (k as f64 * self.pas as f64 / 3600.0))
    }
}

/// Détecte les épisodes pour chaque soirée dont le coucher est dans [debut, fin].
/// `vent` : mesures de vent moyen (km/h), éventuellement vide.
pub fn episodes(temp: &Serie, vent: &Serie, debut: i64, fin: i64, intervalle: i64, lat: f64, lon: f64) -> ResultatEpisodes {
    let pas = intervalle.max(PAS_MIN_S);
    let mut res = ResultatEpisodes { pas_s: pas, ..Default::default() };
    let t0 = (debut - 8 * 3600).div_euclid(pas) * pas;
    let g = Grille::new(temp, t0, fin + 30 * 3600, pas);
    let mut jour = debut.div_euclid(86400) * 86400;
    while jour <= fin {
        let midi = midi_solaire(jour, lon);
        jour += 86400;
        let Some((coucher, lever)) = coucher_lever(midi, lat, lon) else { continue };
        if coucher < debut || coucher > fin {
            continue;
        }
        res.nuits_examinees += 1;
        match nuit(&g, vent, coucher, lever) {
            Ok(e) => res.episodes.push(e),
            Err(r) => match r {
                Rejet::Donnees => res.rejets.donnees_insuffisantes += 1,
                Rejet::Refroidissement => res.rejets.refroidissement += 1,
                Rejet::Amplitude => res.rejets.amplitude += 1,
                Rejet::Remontee => res.rejets.remontee += 1,
                Rejet::Plateau => res.rejets.plateau += 1,
                Rejet::Rupture => res.rejets.rupture += 1,
                Rejet::Vent => res.rejets.vent += 1,
            },
        }
    }
    res
}

#[derive(Debug, PartialEq)]
pub enum Rejet {
    Donnees,
    Refroidissement,
    Amplitude,
    Remontee,
    Plateau,
    Rupture,
    Vent,
}

fn nuit(g: &Grille, vent: &Serie, coucher: i64, lever: i64) -> Result<Episode, Rejet> {
    let h = 3600;
    // Couverture.
    let (a, b) = (g.idx(coucher - 3 * h), g.idx(lever + 3 * h));
    let couverts = g.v[a..=b].iter().filter(|x| x.is_some()).count();
    if (couverts as f64) < COUVERTURE_MIN * (b - a + 1) as f64 {
        return Err(Rejet::Donnees);
    }
    // 1. Refroidissement crépusculaire.
    let (ra, rb) = (g.idx(coucher - 3 * h), g.idx(coucher + 3 * h));
    let mut refr_max = f64::MIN;
    let mut i_debut = None;
    for i in ra..=rb {
        if let Some(r) = g.baisse(i) {
            refr_max = refr_max.max(r);
            if i_debut.is_none() && r >= DEBUT_C_H {
                i_debut = Some(i);
            }
        }
    }
    if refr_max < REFROIDISSEMENT_MIN_C_H {
        return Err(Rejet::Refroidissement);
    }
    let i_debut = i_debut.unwrap_or(ra);
    // Minimum entre le début et lever + 3 h.
    let (ma, mb) = (i_debut, g.idx(lever + 3 * h));
    let (i_min, tmin) = (ma..=mb)
        .filter_map(|i| g.v[i].map(|v| (i, v)))
        .min_by(|x, y| x.1.total_cmp(&y.1))
        .ok_or(Rejet::Donnees)?;
    // 2. Amplitude.
    let tmax = g.v[g.idx(coucher - 6 * h)..=g.idx(coucher)].iter().flatten().fold(f64::MIN, |m, &x| m.max(x));
    if tmax == f64::MIN {
        return Err(Rejet::Donnees);
    }
    let amplitude = tmax - tmin;
    if amplitude < AMPLITUDE_MIN_C {
        return Err(Rejet::Amplitude);
    }
    // 3a. Pas de remontée nocturne notable avant le minimum.
    let mut plancher = f64::MAX;
    for x in g.v[i_debut..=i_min].iter().flatten() {
        plancher = plancher.min(*x);
        if x - plancher > REMONTEE_MAX_C {
            return Err(Rejet::Remontee);
        }
    }
    // 3b. Plateau : baisse lente dans les 3 h avant le minimum.
    let k = (PLATEAU_DUREE_S / g.pas) as usize;
    if i_min >= i_debut + k {
        if let Some(v0) = g.v[i_min - k] {
            let vitesse = (v0 - tmin) / (PLATEAU_DUREE_S as f64 / 3600.0);
            if vitesse > PLATEAU_MAX_C_H {
                return Err(Rejet::Plateau);
            }
        }
    }
    // 4. Rupture matinale.
    let lim = g.idx(lever + RUPTURE_RETARD_MAX_S);
    let i_rupt = (i_min..=lim).find(|&i| g.baisse(i).is_some_and(|r| -r >= RUPTURE_C_H)).ok_or(Rejet::Rupture)?;
    let rupture = g.t(i_rupt);
    if rupture < lever - RUPTURE_AVANCE_MAX_S {
        return Err(Rejet::Rupture);
    }
    // 5. Vent (si mesuré).
    let vent_moyen = moyenne_fenetre(vent, coucher, lever).map(|m| m.0);
    if vent_moyen.is_some_and(|v| v > VENT_MAX_KMH) {
        return Err(Rejet::Vent);
    }
    let debut = g.t(i_debut);
    Ok(Episode {
        coucher,
        lever,
        debut,
        t_min: g.t(i_min),
        tmin,
        tmax_veille: tmax,
        amplitude,
        refroidissement_max: refr_max,
        rupture,
        duree_h: (rupture - debut) as f64 / 3600.0,
        vent_moyen,
    })
}

// ---------------------------------------------------------------------------
// Tests sur données synthétiques
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    // Vercors (Méaudre), 15 janvier 2026 00:00 UTC.
    const LAT: f64 = 45.13;
    const LON: f64 = 5.52;
    const J0: i64 = 1_768_435_200;

    fn serie(debut: i64, fin: i64, pas: i64, f: impl Fn(i64) -> f64) -> Vec<(i64, f64)> {
        (0..).map(|i| debut + i * pas).take_while(|&t| t <= fin).map(|t| (t, f(t))).collect()
    }

    #[test]
    fn soleil_coherent_en_hiver() {
        let (c, l) = coucher_lever(midi_solaire(J0, LON), LAT, LON).unwrap();
        // Janvier à Grenoble : coucher ~16 h UTC, lever ~7 h UTC.
        let hc = (c - J0) as f64 / 3600.0;
        let hl = (l - J0 - 86400) as f64 / 3600.0;
        assert!((15.8..16.5).contains(&hc), "coucher {hc}");
        assert!((6.8..7.5).contains(&hl), "lever {hl}");
        assert!(hauteur_soleil(J0 + 11 * 3600, LAT, LON) > 15.0);
    }

    #[test]
    fn gradient_normal() {
        // -0,65 °C/100 m entre 900, 1400 et 1900 m.
        let bas = serie(J0, J0 + 86400, 600, |t| 5.0 + ((t - J0) as f64 / 86400.0 * 6.28).sin());
        let mil: Vec<_> = bas.iter().map(|&(t, v)| (t, v - 3.25)).collect();
        let haut: Vec<_> = bas.iter().map(|&(t, v)| (t, v - 6.5)).collect();
        let st = [Station { alt: 900.0, serie: &bas }, Station { alt: 1400.0, serie: &mil }, Station { alt: 1900.0, serie: &haut }];
        let g = gradient(&st, J0, J0 + 86400, 3600, 900, LAT, LON);
        assert_eq!(g.len(), 25);
        assert!(g.iter().all(|x| (x.g + 0.65).abs() < 1e-9));
        let s = stats_gradient(&g);
        assert_eq!(s.part_inversion, 0.0);
        assert!(s.max_inversion.is_none());
        assert!(s.n_jour > 0 && s.n_nuit > 0);
    }

    #[test]
    fn inversion_claire() {
        // Fond de vallée à -8 °C, versant 300 m plus haut à -2 °C : +2 °C/100 m.
        let bas = serie(J0, J0 + 12 * 3600, 900, |_| -8.0);
        let haut = serie(J0, J0 + 12 * 3600, 900, |_| -2.0);
        let st = [Station { alt: 1000.0, serie: &bas }, Station { alt: 1300.0, serie: &haut }];
        let g = gradient(&st, J0, J0 + 12 * 3600, 3600, 900, LAT, LON);
        let s = stats_gradient(&g);
        assert_eq!(s.part_inversion, 1.0);
        assert!((s.max_inversion.unwrap() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn appariement_temporel_et_absence_de_recouvrement() {
        let a = serie(J0, J0 + 86400, 600, |_| 0.0);
        let b = serie(J0 + 2 * 86400, J0 + 3 * 86400, 600, |_| 1.0);
        let st = [Station { alt: 1000.0, serie: &a }, Station { alt: 1500.0, serie: &b }];
        assert!(gradient(&st, J0, J0 + 3 * 86400, 3600, 900, LAT, LON).is_empty());
        // Décalage de 20 min : hors tolérance de 15 min à l'heure pile... sauf
        // qu'une mesure sur trois tombe à moins de 15 min : on vérifie la tolérance.
        let c = serie(J0 + 1200, J0 + 86400, 3600, |_| 1.0);
        let st = [Station { alt: 1000.0, serie: &a }, Station { alt: 1500.0, serie: &c }];
        assert!(gradient(&st, J0, J0 + 86400, 3600, 900, LAT, LON).is_empty());
        assert_eq!(gradient(&st, J0, J0 + 86400, 3600, 1800, LAT, LON).len(), 24);
    }

    /// Nuit radiative : 8 °C à 14 h UTC, chute rapide dès 15 h (3 °C/h puis
    /// ralentissement exponentiel vers -6 °C), minimum vers le lever, remontée
    /// franche à partir de 8 h UTC.
    fn nuit_radiative(t: i64) -> f64 {
        let h = (t - J0) as f64 / 3600.0; // heures depuis le 15/01 00 UTC
        if h < 15.0 {
            8.0
        } else if h < 31.5 {
            let x = h - 15.0;
            -6.0 + 14.0 * (-x / 2.5).exp() - 0.05 * x
        } else {
            let base = -6.0 + 14.0 * (-16.5f64 / 2.5).exp() - 0.05 * 16.5;
            base + 2.0 * (h - 31.5)
        }
    }

    #[test]
    fn episode_radiatif_detecte_avec_bons_horaires() {
        let s = serie(J0 - 86400, J0 + 3 * 86400, 600, nuit_radiative);
        let r = episodes(&s, &[], J0, J0 + 86399, 600, LAT, LON);
        assert_eq!(r.nuits_examinees, 1);
        assert_eq!(r.episodes.len(), 1, "{:?}", r.rejets);
        let e = &r.episodes[0];
        let h = |t: i64| (t - J0) as f64 / 3600.0;
        assert!((14.2..15.2).contains(&h(e.debut)), "début {}", h(e.debut));
        assert!((31.0..31.6).contains(&h(e.t_min)), "minimum {}", h(e.t_min));
        assert!((31.0..32.0).contains(&h(e.rupture)), "rupture {}", h(e.rupture));
        assert!(e.refroidissement_max > 2.5);
        assert!((e.amplitude - 14.8).abs() < 0.3, "amplitude {}", e.amplitude);
        assert!(e.duree_h > 15.0);
    }

    #[test]
    fn nuit_nuageuse_ou_ventee_non_detectee() {
        // Baisse lente et régulière (0,3 °C/h), faible amplitude.
        let s = serie(J0 - 86400, J0 + 3 * 86400, 600, |t| 4.0 - 0.3 * ((t - J0) as f64 / 3600.0 - 15.0).clamp(0.0, 16.0));
        let r = episodes(&s, &[], J0, J0 + 86399, 600, LAT, LON);
        assert!(r.episodes.is_empty());
        assert_eq!(r.rejets.refroidissement, 1);
        // Nuit radiative mais vent mesuré fort : rejetée par le critère vent.
        let s = serie(J0 - 86400, J0 + 3 * 86400, 600, nuit_radiative);
        let v = serie(J0 - 86400, J0 + 3 * 86400, 600, |_| 25.0);
        let r = episodes(&s, &v, J0, J0 + 86399, 600, LAT, LON);
        assert!(r.episodes.is_empty());
        assert_eq!(r.rejets.vent, 1);
        // Nuit radiative interrompue par un passage nuageux à minuit (+3 °C).
        let s = serie(J0 - 86400, J0 + 3 * 86400, 600, |t| {
            let h = (t - J0) as f64 / 3600.0;
            nuit_radiative(t) + if (23.0..26.0).contains(&h) { 3.0 } else { 0.0 }
        });
        let r = episodes(&s, &[], J0, J0 + 86399, 600, LAT, LON);
        assert!(r.episodes.is_empty(), "{:?}", r.episodes);
    }

    #[test]
    fn donnees_insuffisantes() {
        let s = serie(J0, J0 + 14 * 3600, 600, nuit_radiative);
        let r = episodes(&s, &[], J0, J0 + 86399, 600, LAT, LON);
        assert_eq!(r.rejets.donnees_insuffisantes, 1);
    }

    #[test]
    fn profil_pente_et_moyenne() {
        let z = [900.0, 1400.0, 1900.0];
        let t = [2.0, -1.25, -4.5];
        assert!((pente_100m(&z, &t).unwrap() + 0.65).abs() < 1e-9);
        assert!(pente_100m(&[1000.0, 1000.0], &[1.0, 2.0]).is_none());
        let s = serie(J0, J0 + 3600, 600, |t| (t - J0) as f64 / 600.0);
        assert_eq!(moyenne_fenetre(&s, J0, J0 + 1200), Some((1.0, 3)));
        assert_eq!(valeur_proche(&s, J0 + 700, 200), Some(1.0));
        assert_eq!(valeur_proche(&s, J0 + 300, 200), None);
    }

    #[test]
    fn agregation_bornee() {
        let v: Vec<(i64, f64)> = (0..10_000).map(|i| (i * 3600, i as f64)).collect();
        let a = agreger(&v, 500);
        assert!(a.len() <= 501 && a.len() >= 490);
        assert!(a.iter().all(|x| x.min <= x.moy && x.moy <= x.max));
    }
}

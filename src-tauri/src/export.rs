//! Export des mesures (page 2 du bloc Stations) : CSV, texte, HTML.
//!
//! Ce module ne recalcule aucune statistique : le résumé par point réutilise
//! [`crate::crossing::CrossAnalysis`] — elle-même bâtie sur
//! [`crate::stats::period_stats`] — et le détail ligne à ligne relit
//! [`crate::db::Db::measurements_for_point_in_window`]. Le rôle de ce module se
//! limite à mettre ces deux sources en forme dans le format demandé.
//!
//! Les trois formats couvrent deux besoins différents :
//! * CSV et TXT « détail » : une ligne par mesure, pour un tableur (le père de
//!   l'utilisateur travaille dans Excel — voir le README) ;
//! * TXT « résumé » et HTML : les agrégats déjà calculés pour le bloc
//!   Croisement de données, mis en page pour la lecture ou le partage.

use chrono::Utc;
use std::collections::HashMap;

use crate::crossing::{CrossAnalysis, ResolvedWindow};
use crate::crossing::TimeWindow;
use crate::db::Db;
use crate::elitech_serial::Rc5Reading;
use crate::error::AppResult;
use crate::model::{Measurement, SourceType};

// ---------------------------------------------------------------------------
// Lignes de détail (CSV / TXT détaillé)
// ---------------------------------------------------------------------------

/// Une mesure, avec de quoi l'identifier sans redemander la base : le nom de
/// la station et le rôle de la série (Primary/Backup) dont elle vient.
#[derive(Debug, Clone)]
pub struct ExportRow {
    pub point_name: String,
    pub series_label: String,
    /// Origine de la série dont vient la mesure.
    ///
    /// Présente pour une seule raison, mais elle est décisive : distinguer une
    /// mesure d'appareil d'une **valeur saisie à la main**
    /// ([`SourceType::ManualEntry`]). Les deux se ressemblent une fois dans un
    /// tableur — même colonne, même unité — alors qu'une moyenne 6 h–9 h
    /// recopiée d'un carnet n'a pas le statut d'un échantillon. La colonne
    /// « Origine » n'apparaît que si l'export en contient au moins une, pour ne
    /// pas alourdir les exports qui n'en ont aucune (voir [`OptionalColumns`]).
    pub source_type: SourceType,
    pub timestamp_utc: chrono::DateTime<Utc>,
    pub temperature_celsius: f64,
    pub pressure_hpa: Option<f64>,
    pub wind_avg_kmh: Option<f64>,
    pub wind_gust_kmh: Option<f64>,
    pub wind_direction_degrees: Option<f64>,
    pub rain_mm: Option<f64>,
    pub solar_radiation: Option<f64>,
}

impl ExportRow {
    fn from_measurement(
        point_name: &str,
        series_label: &str,
        source_type: SourceType,
        m: &Measurement,
    ) -> Self {
        ExportRow {
            point_name: point_name.to_string(),
            series_label: series_label.to_string(),
            source_type,
            timestamp_utc: m.timestamp_utc,
            temperature_celsius: m.temperature_celsius,
            pressure_hpa: m.pressure_hpa,
            wind_avg_kmh: m.wind_avg_kmh,
            wind_gust_kmh: m.wind_gust_kmh,
            wind_direction_degrees: m.wind_direction_degrees,
            rain_mm: m.rain_mm,
            solar_radiation: m.solar_radiation,
        }
    }
}

/// Relit les mesures des points demandés, sur la fenêtre déjà résolue par
/// [`crate::crossing::build`] — pas de seconde résolution de fenêtre ici.
///
/// `window` à `None` (aucune mesure dans la sélection) donne un export vide,
/// pas une erreur : un CSV avec seulement son en-tête reste un export valide.
pub fn collect_rows(
    db: &Db,
    point_ids: &[String],
    window: Option<&ResolvedWindow>,
) -> AppResult<Vec<ExportRow>> {
    let mut rows = Vec::new();
    let Some(w) = window else {
        return Ok(rows);
    };

    for id in point_ids {
        let point = db.get_point(id)?;
        let series = db.list_series_for_point(id)?;
        let par_serie: HashMap<String, (&'static str, SourceType)> = series
            .iter()
            .map(|s| (s.id.clone(), (s.role.as_str(), s.source_type)))
            .collect();

        let measurements =
            db.measurements_for_point_in_window(id, Some(w.from_utc), Some(w.to_utc))?;
        for m in &measurements {
            // Une mesure sans série listée ne devrait pas exister (clé étrangère
            // et `ON DELETE CASCADE`). Si elle existait, mieux vaut l'exporter
            // avec une origine par défaut que la faire disparaître de l'export.
            let (label, source) = par_serie
                .get(&*m.series_id)
                .copied()
                .unwrap_or(("?", SourceType::Manual));
            rows.push(ExportRow::from_measurement(&point.name, label, source, m));
        }
    }

    Ok(rows)
}

// ---------------------------------------------------------------------------
// Colonnes météo optionnelles — n'apparaissent que si au moins une ligne les porte
// ---------------------------------------------------------------------------

struct OptionalColumns {
    /// Colonne « Origine ». Levée dès qu'une ligne au moins vient d'une saisie
    /// manuelle : c'est le seul cas où la provenance change la lecture des
    /// chiffres. Un export 100 % logger n'a rien à y gagner.
    origin: bool,
    pressure: bool,
    wind_avg: bool,
    wind_gust: bool,
    wind_direction: bool,
    rain: bool,
    solar: bool,
}

impl OptionalColumns {
    fn detect(rows: &[ExportRow]) -> Self {
        OptionalColumns {
            origin: rows.iter().any(|r| r.source_type.is_manual_entry()),
            pressure: rows.iter().any(|r| r.pressure_hpa.is_some()),
            wind_avg: rows.iter().any(|r| r.wind_avg_kmh.is_some()),
            wind_gust: rows.iter().any(|r| r.wind_gust_kmh.is_some()),
            wind_direction: rows.iter().any(|r| r.wind_direction_degrees.is_some()),
            rain: rows.iter().any(|r| r.rain_mm.is_some()),
            solar: rows.iter().any(|r| r.solar_radiation.is_some()),
        }
    }
}

/// Nombre décimal au format français (virgule) — cohérent avec le reste de
/// l'UI (`cross.js::fmtDegres`) et nécessaire pour qu'Excel FR reconnaisse la
/// valeur comme un nombre plutôt que du texte.
fn fmt_fr(v: f64) -> String {
    format!("{v:.1}").replace('.', ",")
}

#[cfg(test)]
fn fmt_fr_opt(v: Option<f64>) -> String {
    v.map(fmt_fr).unwrap_or_default()
}

/// Échappement minimal d'un champ CSV : entre guillemets si le champ contient
/// le séparateur, un guillemet ou un retour à la ligne.
#[cfg(test)]
fn csv_field(value: &str) -> String {
    if value.contains(';') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

// ---------------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------------

/// CSV `;`, BOM UTF-8 en tête (Excel Windows), une ligne par mesure.
///
/// Les colonnes météo optionnelles n'apparaissent que si au moins une mesure
/// de l'export les porte — un export Elitech (température seule) ne traîne
/// pas six colonnes vides derrière lui.
pub fn build_csv(rows: &[ExportRow]) -> String {
    let cols = OptionalColumns::detect(rows);

    let mut header = vec!["Point", "Serie", "Horodatage_UTC", "Temperature_C"];
    if cols.origin {
        header.insert(2, "Origine");
    }
    if cols.pressure {
        header.push("Pression_hPa");
    }
    if cols.wind_avg {
        header.push("Vent_moyen_kmh");
    }
    if cols.wind_gust {
        header.push("Rafale_kmh");
    }
    if cols.wind_direction {
        header.push("Direction_deg");
    }
    if cols.rain {
        header.push("Pluie_mm");
    }
    if cols.solar {
        header.push("Rayonnement_Wm2");
    }

    // Le BOM UTF-8 est un caractère de contenu ordinaire ici : `String` est
    // toujours de l'UTF-8, il se traduit en EF BB BF une fois écrit sur disque.
    let mut out = String::with_capacity(estimate_capacity(rows.len()));
    out.push('\u{FEFF}');
    out.push_str(&header.join(";"));
    out.push_str("\r\n");

    write_detail_rows(&mut out, rows, &cols, ';', "\r\n", push_csv_field);
    out
}

/// Capacité initiale d'un export détaillé : ~80 octets par ligne (point,
/// série, horodatage, température et quelques colonnes). Une estimation, pas
/// une borne : elle évite surtout la série de réallocations par doublement.
fn estimate_capacity(rows: usize) -> usize {
    256 + rows.saturating_mul(80)
}

/// Écrit `v` au format décimal français (`fmt_fr`) directement dans `out`.
fn push_fr(out: &mut String, v: f64) {
    use std::fmt::Write as _;
    let debut = out.len();
    let _ = write!(out, "{v:.1}");
    // `{:.1}` produit au plus un point décimal : le remplacer en place donne
    // exactement `format!("{v:.1}").replace('.', ",")`.
    if let Some(pos) = out[debut..].find('.') {
        out.replace_range(debut + pos..debut + pos + 1, ",");
    }
}

fn push_fr_opt(out: &mut String, v: Option<f64>) {
    if let Some(x) = v {
        push_fr(out, x);
    }
}

/// Même règle que [`csv_field`], écrite directement dans `out`.
fn push_csv_field(out: &mut String, value: &str) {
    if value.contains(';') || value.contains('"') || value.contains('\n') {
        out.push('"');
        for c in value.chars() {
            if c == '"' {
                out.push('"');
            }
            out.push(c);
        }
        out.push('"');
    } else {
        out.push_str(value);
    }
}

fn push_raw_field(out: &mut String, value: &str) {
    out.push_str(value);
}

/// Lignes de détail communes au CSV et au TXT : mêmes colonnes, même ordre,
/// seuls le séparateur, la fin de ligne et l'échappement des champs texte
/// changent. Écrit champ par champ dans `out`, sans `Vec<String>` par ligne.
fn write_detail_rows(
    out: &mut String,
    rows: &[ExportRow],
    cols: &OptionalColumns,
    sep: char,
    eol: &str,
    text_field: fn(&mut String, &str),
) {
    for r in rows {
        text_field(out, &r.point_name);
        out.push(sep);
        text_field(out, &r.series_label);
        out.push(sep);
        if cols.origin {
            text_field(out, r.source_type.label_fr());
            out.push(sep);
        }
        out.push_str(
            &r.timestamp_utc
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        out.push(sep);
        push_fr(out, r.temperature_celsius);
        for (actif, valeur) in [
            (cols.pressure, r.pressure_hpa),
            (cols.wind_avg, r.wind_avg_kmh),
            (cols.wind_gust, r.wind_gust_kmh),
            (cols.wind_direction, r.wind_direction_degrees),
            (cols.rain, r.rain_mm),
            (cols.solar, r.solar_radiation),
        ] {
            if actif {
                out.push(sep);
                push_fr_opt(out, valeur);
            }
        }
        out.push_str(eol);
    }
}

// ---------------------------------------------------------------------------
// Texte — détail (tabulé) ou résumé (statistiques)
// ---------------------------------------------------------------------------

/// Même contenu que le CSV, en texte tabulé plutôt qu'en `;` — lisible dans
/// n'importe quel éditeur, sans dépendre d'un tableur pour aligner les colonnes.
pub fn build_txt_detail(rows: &[ExportRow]) -> String {
    let cols = OptionalColumns::detect(rows);

    let mut header = vec!["Point", "Série", "Horodatage UTC", "Température (°C)"];
    if cols.origin {
        header.insert(2, "Origine");
    }
    if cols.pressure {
        header.push("Pression (hPa)");
    }
    if cols.wind_avg {
        header.push("Vent moyen (km/h)");
    }
    if cols.wind_gust {
        header.push("Rafale (km/h)");
    }
    if cols.wind_direction {
        header.push("Direction (°)");
    }
    if cols.rain {
        header.push("Pluie (mm)");
    }
    if cols.solar {
        header.push("Rayonnement (W/m²)");
    }

    let mut out = String::with_capacity(estimate_capacity(rows.len()));
    out.push_str(&header.join("\t"));
    out.push('\n');

    write_detail_rows(&mut out, rows, &cols, '\t', "\n", push_raw_field);
    out
}

fn fmt_opt_1(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1} °C")).unwrap_or_else(|| "—".to_string())
}

/// Résumé statistique seul : pas une ligne par mesure, un paragraphe par point.
///
/// Les chiffres sont ceux déjà calculés par `crossing::build` pour le bloc
/// Croisement de données — les mêmes que le tableau affiché à l'écran.
pub fn build_txt_summary(analysis: &CrossAnalysis) -> String {
    let mut out = String::new();
    out.push_str("Résumé statistique — Gastinel Weather Studio\r\n");
    out.push_str("=============================================\r\n\r\n");

    match &analysis.window {
        Some(w) => {
            out.push_str(&format!(
                "Période : {} ({} UTC → {} UTC)\r\n\r\n",
                w.label,
                w.from_utc.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                w.to_utc.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ));
        }
        None => {
            out.push_str("Aucune mesure sur la sélection : rien à résumer.\r\n\r\n");
            return out;
        }
    }

    let annualisee = analysis
        .window
        .as_ref()
        .map(|w| w.covers_a_full_year)
        .unwrap_or(false);

    for p in &analysis.points {
        out.push_str(&format!(
            "{} — {}{}\r\n",
            p.name,
            p.category_label,
            p.size.map(|s| format!(" {}", s.label_fr())).unwrap_or_default()
        ));
        out.push_str(&format!("  Mesures sur la période : {}\r\n", p.measurement_count));
        // Dit juste sous le nom du point, avant les chiffres qu'il qualifie :
        // une note de bas de page se lirait après avoir conclu.
        if p.manual_entry_series > 0 {
            out.push_str(&format!(
                "  /!\\ {} série(s) de saisie manuelle sur site : les statistiques \
                 ci-dessous mêlent des mesures de logger et des valeurs relevées à \
                 la main sur une plage horaire.\r\n",
                p.manual_entry_series
            ));
        }
        out.push_str(&format!("  Minimum extrême        : {}\r\n", fmt_opt_1(p.extreme_min_c)));
        out.push_str(&format!(
            "  Moyenne des minimums{} : {}\r\n",
            if annualisee { " (annuelle)" } else { "" },
            fmt_opt_1(p.mean_daily_min_c)
        ));
        out.push_str(&format!("  Amplitude moyenne      : {}\r\n", fmt_opt_1(p.mean_amplitude_c)));
        out.push_str(&format!("  Amplitude maximale     : {}\r\n", fmt_opt_1(p.max_amplitude_c)));
        out.push_str(&format!(
            "  Record de froid (historique complet) : {}\r\n",
            fmt_opt_1(p.record_min_c)
        ));
        out.push_str("\r\n");
    }

    out
}

// ---------------------------------------------------------------------------
// HTML — rapport autonome
// ---------------------------------------------------------------------------

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Rapport HTML autonome (un seul fichier, aucune dépendance externe à
/// l'ouverture) : tableau de statistiques résumées, pas de graphique — dupliquer
/// Chart.js dans le fichier exporté coûterait plus qu'il n'apporterait pour un
/// tableau qui se lit très bien seul (cf. l'en-tête du module).
///
/// La police système sert de secours à Inter plutôt qu'un embarquement en
/// base64 du fichier `.woff2` : ça évite d'ajouter une dépendance (`base64`)
/// au seul bénéfice d'un rapport statique, pour un gain visuel marginal.
pub fn build_html_summary(analysis: &CrossAnalysis) -> String {
    let periode = match &analysis.window {
        Some(w) => format!(
            "{} ({} UTC → {} UTC)",
            html_escape(&w.label),
            w.from_utc.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            w.to_utc.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
        None => "Aucune mesure sur la sélection".to_string(),
    };
    let annualisee = analysis
        .window
        .as_ref()
        .map(|w| w.covers_a_full_year)
        .unwrap_or(false);

    let mut lignes = String::new();
    for p in &analysis.points {
        // Même intention que dans le résumé texte : la mention voyage avec le
        // nom du point, pas en légende sous le tableau.
        let pastille = if p.manual_entry_series > 0 {
            format!(
                " <span class=\"manual\" title=\"{} série(s) de saisie manuelle sur site : \
                 valeurs relevées à la main sur une plage horaire, et non mesurées par un \
                 logger\">saisie manuelle</span>",
                p.manual_entry_series
            )
        } else {
            String::new()
        };
        lignes.push_str(&format!(
            "<tr><td>{}{}</td><td>{}{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td>\
             <td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>\n",
            html_escape(&p.name),
            pastille,
            html_escape(&p.category_label),
            p.size
                .map(|s| format!(" ({})", html_escape(s.label_fr())))
                .unwrap_or_default(),
            p.measurement_count,
            fmt_opt_1(p.extreme_min_c),
            fmt_opt_1(p.mean_daily_min_c),
            fmt_opt_1(p.mean_amplitude_c),
            fmt_opt_1(p.max_amplitude_c),
        ));
    }

    let noms: Vec<String> = analysis
        .points
        .iter()
        .map(|p| html_escape(&p.name))
        .collect();
    let titre = if noms.is_empty() {
        "Export".to_string()
    } else {
        noms.join(", ")
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="fr">
<head>
<meta charset="utf-8">
<title>Rapport — {titre}</title>
<style>
  :root {{
    --bleu: #0072b2;
    --gris-fonce: #2b3640;
    --gris: #5b6b7a;
    --gris-clair: #eef1f5;
    --bordure: #d7dee6;
  }}
  body {{
    font-family: "Inter", "Segoe UI", system-ui, sans-serif;
    color: var(--gris-fonce);
    background: #fff;
    margin: 0;
    padding: 2.5rem 3rem;
  }}
  h1 {{ color: var(--bleu); margin: 0 0 .25rem; font-size: 1.5rem; }}
  .sous-titre {{ color: var(--gris); margin: 0 0 2rem; font-size: .95rem; }}
  table {{ border-collapse: collapse; width: 100%; font-size: .9rem; }}
  th, td {{ padding: .5rem .75rem; border-bottom: 1px solid var(--bordure); text-align: left; }}
  th {{ background: var(--gris-clair); color: var(--gris-fonce); font-weight: 600; }}
  td.num, th.num {{ text-align: right; font-variant-numeric: tabular-nums; }}
  .manual {{ display: inline-block; margin-left: .4rem; padding: .05rem .4rem;
             border: 1px solid var(--bordure); border-radius: 999px;
             font-size: .7rem; color: var(--gris-fonce); vertical-align: middle; }}
  .note {{ color: var(--gris); font-size: .85rem; margin-top: 1.5rem; }}
  footer {{ color: var(--gris); font-size: .8rem; margin-top: 3rem; border-top: 1px solid var(--bordure); padding-top: 1rem; }}
</style>
</head>
<body>
  <h1>Rapport de mesures — {titre}</h1>
  <p class="sous-titre">Période : {periode}</p>
  <table>
    <thead>
      <tr>
        <th>Point</th><th>Catégorie</th>
        <th class="num">Mesures</th>
        <th class="num">Min. extrême (°C)</th>
        <th class="num">Moy. min{annuel} (°C)</th>
        <th class="num">Amplitude moy. (°C)</th>
        <th class="num">Amplitude max. (°C)</th>
      </tr>
    </thead>
    <tbody>
{lignes}    </tbody>
  </table>
  <p class="note">
    Le détail complet des mesures (une ligne par relevé) est disponible en export CSV,
    sur la même sélection et la même période.
  </p>
  <footer>Généré par Gastinel Weather Studio.</footer>
</body>
</html>
"#,
        titre = titre,
        periode = periode,
        annuel = if annualisee { " ann." } else { "" },
        lignes = lignes,
    )
}

// ---------------------------------------------------------------------------
// Nom de fichier suggéré
// ---------------------------------------------------------------------------

fn slugify(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let slug = slug.trim_matches('_').to_string();
    if slug.is_empty() {
        "point".to_string()
    } else {
        slug
    }
}

fn window_slug(window: TimeWindow) -> String {
    match window {
        TimeWindow::Last7Days => "7j".to_string(),
        TimeWindow::Last30Days => "30j".to_string(),
        TimeWindow::CurrentYear => "annee".to_string(),
        TimeWindow::All => "tout".to_string(),
        TimeWindow::Custom { from, to } => format!("{from}_au_{to}"),
    }
}

/// Nom de fichier par défaut : point(s) + fenêtre + date du jour, ex.
/// `Darbounouse_30j_2026-08-20.csv`.
pub fn suggested_filename(analysis: &CrossAnalysis, window: TimeWindow, ext: &str) -> String {
    let noms: Vec<String> = analysis.points.iter().map(|p| slugify(&p.name)).collect();
    let partie_noms = match noms.len() {
        0 => "export".to_string(),
        1 => noms[0].clone(),
        2 => format!("{}-{}", noms[0], noms[1]),
        n => format!("{n}_stations"),
    };
    let date_du_jour = Utc::now().date_naive();
    format!("{partie_noms}_{}_{date_du_jour}.{ext}", window_slug(window))
}

// ---------------------------------------------------------------------------
// Export d'une lecture directe du RC-5
// ---------------------------------------------------------------------------

/// Export du contenu d'un logger **tel qu'il vient d'être lu**, sans passer par
/// la base.
///
/// ## Pourquoi ce n'est pas [`build_csv`]
///
/// Une seule raison, mais elle est décisive : [`ExportRow`] date ses mesures en
/// `DateTime<Utc>`, alors que le RC-5 ne connaît que **l'heure brute de son
/// horloge interne**, sans fuseau et éventuellement dérivante. Les faire
/// traverser un champ typé UTC les présenterait comme de l'UTC — ce qu'elles ne
/// sont pas, et ce que la page RC-5 se garde justement d'affirmer, jusque dans
/// l'étiquette de son axe des abscisses.
///
/// L'en-tête de colonne le dit donc en toutes lettres. Un export du même logger
/// **après** enregistrement passe, lui, par [`build_csv`] et porte de l'UTC
/// véritable : les deux fichiers ne racontent pas la même chose, et ils ne le
/// prétendent pas.
///
/// ## Le récapitulatif en tête
///
/// Les lignes `#` avant l'en-tête portent l'identité de l'appareil. Sans elles,
/// un CSV de 16 000 lignes de température ne dit ni de quel boîtier il vient, ni
/// dans quel état il était. Le `#` est la convention de commentaire que tous les
/// tableurs savent ignorer à l'import.
pub fn build_rc5_csv(reading: &Rc5Reading, port_name: &str) -> String {
    let d = &reading.device;
    // Même BOM que `build_csv`, et pour la même raison : sans lui, Excel lit
    // l'UTF-8 comme du Latin-1 et les degrés deviennent des hiéroglyphes.
    let mut out = String::from("\u{FEFF}");

    out.push_str(&format!("# Logger {} lu sur {port_name}\r\n", d.model_label()));
    out.push_str(&format!(
        "# Numero de serie (transmis, tronque a 10 caracteres) : {}\r\n",
        d.device_number.clone().unwrap_or_else(|| "inconnu".into())
    ));
    out.push_str(&format!("# Etat : {}\r\n", d.work_status.label_fr()));
    out.push_str(&format!("# Cadence : {} s\r\n", d.sample_interval_seconds));
    out.push_str(&format!(
        "# Horloge du logger a la lecture : {}\r\n",
        d.device_clock
            .map(|h| h.to_string())
            .unwrap_or_else(|| "inconnue".into())
    ));
    out.push_str(
        "# Horodatages : heure BRUTE du logger, sans fuseau ni correction de derive.\r\n",
    );
    out.push_str("Horodatage_brut_logger;Temperature_C\r\n");

    for m in &reading.measurements {
        out.push_str(&m.raw_timestamp.format("%Y-%m-%dT%H:%M:%S").to_string());
        out.push(';');
        out.push_str(&fmt_fr(m.temperature_celsius));
        out.push_str("\r\n");
    }

    out
}

/// Le même contenu en texte tabulé — pendant de [`build_txt_detail`].
///
/// Le récapitulatif y est plus complet que dans le CSV : un fichier texte se lit
/// tel quel, il peut donc porter ce qu'un tableur relèguerait dans des lignes de
/// commentaire (seuils d'alarme, correction interne de l'appareil).
pub fn build_rc5_txt(reading: &Rc5Reading, port_name: &str) -> String {
    let d = &reading.device;
    let mut out = String::new();

    out.push_str(&format!("Logger {} — lu sur {port_name}\n", d.model_label()));
    out.push_str(&format!(
        "Numéro de série (transmis, tronqué à 10 caractères) : {}\n",
        d.device_number.clone().unwrap_or_else(|| "inconnu".into())
    ));
    out.push_str(&format!("État : {}\n", d.work_status.label_fr()));
    out.push_str(&format!("Cadence : {} s\n", d.sample_interval_seconds));
    out.push_str(&format!(
        "Seuils d'alarme : {} °C / {} °C\n",
        fmt_fr(d.lower_limit_celsius),
        fmt_fr(d.upper_limit_celsius)
    ));
    out.push_str(&format!(
        "Correction interne appliquée par l'appareil : {} °C\n",
        fmt_fr(d.temperature_calibration)
    ));
    out.push_str(&format!("Mesures : {}\n", reading.measurements.len()));
    out.push_str("\nHorodatages : heure BRUTE du logger, sans fuseau ni correction de dérive.\n\n");
    out.push_str("Horodatage brut\tTempérature (°C)\n");

    for m in &reading.measurements {
        out.push_str(&m.raw_timestamp.format("%Y-%m-%d %H:%M:%S").to_string());
        out.push('\t');
        out.push_str(&fmt_fr(m.temperature_celsius));
        out.push('\n');
    }

    out
}

/// Nom de fichier proposé pour l'export d'un logger.
///
/// Le numéro de série plutôt que le nom d'un point : à ce stade, la lecture n'est
/// rattachée à **aucun** point — c'est justement ce que fait l'enregistrement,
/// qui peut ne jamais avoir lieu.
pub fn suggested_rc5_filename(reading: &Rc5Reading, ext: &str) -> String {
    let serie = reading
        .logger_serial()
        .map(|s| slugify(&s))
        .unwrap_or_else(|| "logger".to_string());
    let date_du_jour = Utc::now().date_naive();
    format!("RC5_{serie}_{date_du_jour}.{ext}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crossing::CrossPointStats;
    use crate::model::{Category, PointSize};
    use chrono::TimeZone;

    fn t(iso: &str) -> chrono::DateTime<Utc> {
        iso.parse().expect("horodatage de test invalide")
    }

    fn ligne_simple(nom: &str, temp: f64) -> ExportRow {
        ExportRow {
            point_name: nom.to_string(),
            series_label: "Primary".to_string(),
            source_type: SourceType::ElitechImport,
            timestamp_utc: t("2026-08-01T10:00:00Z"),
            temperature_celsius: temp,
            pressure_hpa: None,
            wind_avg_kmh: None,
            wind_gust_kmh: None,
            wind_direction_degrees: None,
            rain_mm: None,
            solar_radiation: None,
        }
    }

    // ----------------------------------------------------------------- CSV

    #[test]
    fn le_csv_commence_par_un_bom_et_utilise_le_point_virgule() {
        let csv = build_csv(&[ligne_simple("Darbounouse", -3.2)]);
        assert!(csv.starts_with('\u{FEFF}'));
        assert!(csv.contains("Point;Serie;Horodatage_UTC;Temperature_C"));
        // Virgule décimale française, pas de point.
        assert!(csv.contains("-3,2"));
    }

    #[test]
    fn le_csv_omet_les_colonnes_meteo_entierement_vides() {
        let csv = build_csv(&[ligne_simple("Darbounouse", -3.2)]);
        assert!(!csv.contains("Pression_hPa"));
        assert!(!csv.contains("Rayonnement_Wm2"));
    }

    #[test]
    fn le_csv_inclut_une_colonne_meteo_des_qu_une_seule_mesure_la_porte() {
        let mut ligne = ligne_simple("Moucherolle", 2.0);
        ligne.pressure_hpa = Some(1013.4);
        let csv = build_csv(&[ligne_simple("Autre", 0.0), ligne]);
        assert!(csv.contains("Pression_hPa"));
        // La ligne sans pression laisse le champ vide plutôt qu'une valeur inventée.
        let lignes: Vec<&str> = csv.lines().collect();
        assert!(lignes[1].ends_with(';')); // "Autre" : dernier champ (Pression) vide
    }

    #[test]
    fn le_csv_echappe_un_nom_contenant_le_separateur() {
        let csv = build_csv(&[ligne_simple("Point; test", 1.0)]);
        assert!(csv.contains("\"Point; test\""));
    }

    // ----------------------------------------------------------------- TXT détail

    #[test]
    fn le_txt_detail_est_tabule_et_sans_bom() {
        let txt = build_txt_detail(&[ligne_simple("Darbounouse", -3.2)]);
        assert!(!txt.starts_with('\u{FEFF}'));
        assert!(txt.contains('\t'));
        assert!(txt.contains("-3,2"));
    }

    // ----------------------------------------------------------------- résumé (TXT/HTML)

    fn analyse_de_test(min_extreme: Option<f64>) -> CrossAnalysis {
        use crate::crossing::{ResolvedWindow, TimeWindow as TW};
        use crate::stats::Granularity;
        let window = ResolvedWindow {
            requested: TW::Last30Days,
            label: "30 derniers jours de données".to_string(),
            from_utc: Utc.with_ymd_and_hms(2026, 7, 21, 0, 0, 0).unwrap(),
            to_utc: Utc.with_ymd_and_hms(2026, 8, 20, 0, 0, 0).unwrap(),
            granularity: Granularity::Day,
            granularity_label: "Jour".to_string(),
            covers_a_full_year: false,
        };
        CrossAnalysis {
            window: Some(window),
            points: vec![CrossPointStats {
                point_id: "p1".to_string(),
                name: "Darbounouse".to_string(),
                category: Category::Cat3,
                category_label: "Dépression fermée".to_string(),
                size: Some(PointSize::Gr),
                altitude_m: Some(1050.0),
                measurement_count: 120,
                day_count: 30,
                first_utc: Some(t("2026-07-21T00:00:00Z")),
                last_utc: Some(t("2026-08-19T23:00:00Z")),
                extreme_min_c: min_extreme,
                extreme_min_date: None,
                mean_daily_min_c: Some(-1.5),
                mean_amplitude_c: Some(8.2),
                max_amplitude_c: Some(14.0),
                max_amplitude_date: None,
                record_min_c: Some(-12.4),
                total_measurement_count: 5000,
                history_first_utc: None,
                history_last_utc: None,
                manual_entry_series: 0,
                curve: Vec::new(),
            }],
            points_without_data: 0,
            readability_warning: None,
            elapsed_ms: 1,
        }
    }

    #[test]
    fn le_resume_texte_reprend_les_stats_deja_calculees() {
        let analyse = analyse_de_test(Some(-3.2));
        let txt = build_txt_summary(&analyse);
        assert!(txt.contains("Darbounouse"));
        assert!(txt.contains("-3,2 °C") || txt.contains("-3.2 °C"));
        assert!(txt.contains("120"));
    }

    #[test]
    fn le_resume_texte_sans_fenetre_ne_plante_pas() {
        let mut analyse = analyse_de_test(None);
        analyse.window = None;
        let txt = build_txt_summary(&analyse);
        assert!(txt.contains("rien"));
    }

    #[test]
    fn le_rapport_html_est_un_document_valide_a_minima() {
        let analyse = analyse_de_test(Some(-3.2));
        let html = build_html_summary(&analyse);
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("</html>"));
        assert!(html.contains("Darbounouse"));
        // Pas de dépendance externe : pas d'URL http(s) chargée par la page.
        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
    }

    #[test]
    fn le_rapport_html_echappe_les_noms() {
        let mut analyse = analyse_de_test(Some(-3.2));
        analyse.points[0].name = "<script>alert(1)</script>".to_string();
        let html = build_html_summary(&analyse);
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
    }

    // ----------------------------------------------------------------- nom de fichier

    #[test]
    fn le_nom_de_fichier_reprend_le_point_et_la_fenetre() {
        let analyse = analyse_de_test(Some(-3.2));
        let nom = suggested_filename(&analyse, TimeWindow::Last30Days, "csv");
        assert!(nom.starts_with("Darbounouse_30j_"));
        assert!(nom.ends_with(".csv"));
    }

    #[test]
    fn le_nom_de_fichier_gere_plusieurs_points() {
        let mut analyse = analyse_de_test(Some(-3.2));
        analyse.points.push(analyse.points[0].clone());
        analyse.points.push(analyse.points[0].clone());
        let nom = suggested_filename(&analyse, TimeWindow::All, "html");
        assert!(nom.contains("3_stations"));
    }

    // --- Export d'une lecture directe du RC-5 ------------------------------

    fn lecture_rc5_factice() -> Rc5Reading {
        use crate::elitech_serial::{
            build_measurements, DataHeader, DevInfo, WorkStatus, RC5_MODEL_NO,
        };

        let depart = chrono::NaiveDate::from_ymd_opt(2026, 1, 14)
            .unwrap()
            .and_hms_opt(3, 0, 0)
            .unwrap();
        // −18,3 °C puis −21,7 °C : deux valeurs négatives à une décimale, soit
        // exactement ce que le format français doit écrire avec une virgule.
        let mesures = build_measurements(depart, 900, &[-183, -217, -195]);

        Rc5Reading {
            device: DevInfo {
                station_no: 1,
                model_no: RC5_MODEL_NO,
                sample_interval_seconds: 900,
                rec_count: 3,
                start_time: Some(depart),
                device_clock: Some(depart + chrono::Duration::hours(6)),
                last_online: None,
                work_status: WorkStatus::Stopped,
                device_number: Some("E205004333".to_string()),
                user_info: Some("RC-5 Data Logger".to_string()),
                upper_limit_celsius: 60.0,
                lower_limit_celsius: -30.0,
                temperature_calibration: 0.0,
                celsius: true,
            },
            header: DataHeader {
                rec_count: 3,
                start_time: Some(depart),
            },
            measurements: mesures,
            warnings: Vec::new(),
            date_order: crate::parsing::DateOrderEvidence::iso(3),
            anomalies: Vec::new(),
        }
    }

    /// L'invariant de ce format : **ne jamais présenter l'heure du logger comme
    /// de l'UTC**. C'est la seule raison pour laquelle cet export n'est pas
    /// `build_csv`, et un `Horodatage_UTC` qui réapparaîtrait ici serait un
    /// mensonge sur la donnée, pas un détail de libellé.
    #[test]
    fn l_export_d_un_logger_ne_pretend_jamais_donner_de_l_utc() {
        let csv = build_rc5_csv(&lecture_rc5_factice(), "COM8");
        assert!(csv.contains("Horodatage_brut_logger;Temperature_C"));
        assert!(!csv.contains("Horodatage_UTC"));
        assert!(csv.contains("heure BRUTE du logger"));

        let txt = build_rc5_txt(&lecture_rc5_factice(), "usb:1002");
        assert!(!txt.contains("UTC"));
        assert!(txt.contains("heure BRUTE du logger"));
    }

    /// Le récapitulatif en tête est ce qui rattache le fichier à un boîtier :
    /// sans lui, un CSV de températures ne dit pas d'où il vient.
    #[test]
    fn l_export_d_un_logger_porte_son_identite() {
        let csv = build_rc5_csv(&lecture_rc5_factice(), "usb:1002");
        assert!(csv.contains("E205004333"));
        assert!(csv.contains("RC-5"));
        assert!(csv.contains("usb:1002"));
        assert!(csv.contains("arrêté") || csv.contains("arr"));
    }

    /// Toutes les mesures, dans l'ordre, en virgule française — le fichier est
    /// destiné à Excel en configuration française (cf. l'en-tête du module).
    #[test]
    fn l_export_d_un_logger_porte_toutes_ses_mesures() {
        let lecture = lecture_rc5_factice();
        let csv = build_rc5_csv(&lecture, "COM8");
        let lignes: Vec<&str> = csv.lines().filter(|l| l.starts_with("2026-")).collect();

        assert_eq!(lignes.len(), lecture.measurements.len());
        assert_eq!(lignes[0], "2026-01-14T03:00:00;-18,3");
        assert_eq!(lignes[1], "2026-01-14T03:15:00;-21,7");
        assert_eq!(lignes[2], "2026-01-14T03:30:00;-19,5");
    }

    /// Le nom de fichier porte le numéro de série, pas un nom de point : à ce
    /// stade la lecture n'est rattachée à aucun point, et elle peut ne jamais
    /// l'être.
    #[test]
    fn le_nom_de_fichier_d_un_logger_porte_son_numero_de_serie() {
        let nom = suggested_rc5_filename(&lecture_rc5_factice(), "csv");
        assert!(nom.starts_with("RC5_E205004333_"));
        assert!(nom.ends_with(".csv"));
    }

    /// Un logger dont le protocole n'a pas transmis de numéro doit quand même
    /// produire un nom de fichier utilisable.
    #[test]
    fn un_logger_sans_numero_de_serie_a_quand_meme_un_nom_de_fichier() {
        let mut lecture = lecture_rc5_factice();
        lecture.device.device_number = None;
        assert!(suggested_rc5_filename(&lecture, "txt").starts_with("RC5_logger_"));
    }

    // --- Piste mémoire n°5 : sortie identique octet pour octet -------------
    //
    // Copies figées des implémentations d'avant (un `Vec<String>` + `join` par
    // ligne), gardées comme référence.

    fn ancien_build_csv(rows: &[ExportRow]) -> String {
        let cols = OptionalColumns::detect(rows);

        let mut header = vec!["Point", "Serie", "Horodatage_UTC", "Temperature_C"];
        if cols.origin {
            header.insert(2, "Origine");
        }
        if cols.pressure {
            header.push("Pression_hPa");
        }
        if cols.wind_avg {
            header.push("Vent_moyen_kmh");
        }
        if cols.wind_gust {
            header.push("Rafale_kmh");
        }
        if cols.wind_direction {
            header.push("Direction_deg");
        }
        if cols.rain {
            header.push("Pluie_mm");
        }
        if cols.solar {
            header.push("Rayonnement_Wm2");
        }

        // Le BOM UTF-8 est un caractère de contenu ordinaire ici : `String` est
        // toujours de l'UTF-8, il se traduit en EF BB BF une fois écrit sur disque.
        let mut out = String::from("\u{FEFF}");
        out.push_str(&header.join(";"));
        out.push_str("\r\n");

        for r in rows {
            let mut fields = vec![
                csv_field(&r.point_name),
                csv_field(&r.series_label),
                r.timestamp_utc
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                fmt_fr(r.temperature_celsius),
            ];
            if cols.origin {
                fields.insert(2, csv_field(r.source_type.label_fr()));
            }
            if cols.pressure {
                fields.push(fmt_fr_opt(r.pressure_hpa));
            }
            if cols.wind_avg {
                fields.push(fmt_fr_opt(r.wind_avg_kmh));
            }
            if cols.wind_gust {
                fields.push(fmt_fr_opt(r.wind_gust_kmh));
            }
            if cols.wind_direction {
                fields.push(fmt_fr_opt(r.wind_direction_degrees));
            }
            if cols.rain {
                fields.push(fmt_fr_opt(r.rain_mm));
            }
            if cols.solar {
                fields.push(fmt_fr_opt(r.solar_radiation));
            }
            out.push_str(&fields.join(";"));
            out.push_str("\r\n");
        }

        out
    }

    fn ancien_build_txt_detail(rows: &[ExportRow]) -> String {
        let cols = OptionalColumns::detect(rows);

        let mut header = vec!["Point", "Série", "Horodatage UTC", "Température (°C)"];
        if cols.origin {
            header.insert(2, "Origine");
        }
        if cols.pressure {
            header.push("Pression (hPa)");
        }
        if cols.wind_avg {
            header.push("Vent moyen (km/h)");
        }
        if cols.wind_gust {
            header.push("Rafale (km/h)");
        }
        if cols.wind_direction {
            header.push("Direction (°)");
        }
        if cols.rain {
            header.push("Pluie (mm)");
        }
        if cols.solar {
            header.push("Rayonnement (W/m²)");
        }

        let mut out = String::new();
        out.push_str(&header.join("\t"));
        out.push('\n');

        for r in rows {
            let mut fields = vec![
                r.point_name.clone(),
                r.series_label.clone(),
                r.timestamp_utc
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                fmt_fr(r.temperature_celsius),
            ];
            if cols.origin {
                fields.insert(2, r.source_type.label_fr().to_string());
            }
            if cols.pressure {
                fields.push(fmt_fr_opt(r.pressure_hpa));
            }
            if cols.wind_avg {
                fields.push(fmt_fr_opt(r.wind_avg_kmh));
            }
            if cols.wind_gust {
                fields.push(fmt_fr_opt(r.wind_gust_kmh));
            }
            if cols.wind_direction {
                fields.push(fmt_fr_opt(r.wind_direction_degrees));
            }
            if cols.rain {
                fields.push(fmt_fr_opt(r.rain_mm));
            }
            if cols.solar {
                fields.push(fmt_fr_opt(r.solar_radiation));
            }
            out.push_str(&fields.join("\t"));
            out.push('\n');
        }

        out
    }

    /// Jeu de données de test couvrant toutes les branches : colonnes
    /// optionnelles présentes ou non, saisie manuelle (colonne Origine),
    /// champs à échapper, valeurs négatives, arrondis, NaN, zéro négatif.
    fn jeu_export() -> Vec<ExportRow> {
        let noms = ["Col; du \"Loup\"", "Plateau", "Ligne\nbrisée", "Été"];
        let valeurs = [
            -5.04, -0.04, 0.05, 12.25, 99.95, 1e6, f64::NAN, -0.0, 3.25,
        ];
        let mut lignes = Vec::new();
        for i in 0..400_usize {
            let v = valeurs[i % valeurs.len()] + (i as f64) * 0.01;
            let opt = |k: usize| if (i + k) % 4 == 0 { None } else { Some(v * k as f64) };
            lignes.push(ExportRow {
                point_name: noms[i % noms.len()].to_string(),
                series_label: if i % 2 == 0 { "Primary" } else { "Backup" }.to_string(),
                source_type: if i % 7 == 0 { SourceType::ManualEntry } else { SourceType::Manual },
                timestamp_utc: Utc.timestamp_opt(1_767_225_600 + (i as i64) * 600, 0).unwrap(),
                temperature_celsius: v,
                pressure_hpa: opt(1),
                wind_avg_kmh: opt(2),
                wind_gust_kmh: opt(3),
                wind_direction_degrees: opt(5),
                rain_mm: opt(6),
                solar_radiation: opt(7),
            });
        }
        lignes
    }

    #[test]
    fn export_detail_identique_a_l_ancienne_implementation() {
        let complet = jeu_export();
        // Variante température seule (aucune colonne optionnelle, pas d'Origine).
        let minimal: Vec<ExportRow> = complet
            .iter()
            .cloned()
            .map(|mut r| {
                r.source_type = SourceType::Manual;
                r.pressure_hpa = None;
                r.wind_avg_kmh = None;
                r.wind_gust_kmh = None;
                r.wind_direction_degrees = None;
                r.rain_mm = None;
                r.solar_radiation = None;
                r
            })
            .collect();
        // Variante : une seule colonne optionnelle (pluie).
        let pluie: Vec<ExportRow> = minimal
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, mut r)| {
                r.rain_mm = (i % 3 == 0).then_some(i as f64 * 0.2);
                r
            })
            .collect();
        for (nom, lot) in [("complet", &complet), ("minimal", &minimal), ("pluie", &pluie), ("vide", &Vec::new())] {
            assert_eq!(build_csv(lot).as_bytes(), ancien_build_csv(lot).as_bytes(), "CSV {nom}");
            assert_eq!(
                build_txt_detail(lot).as_bytes(),
                ancien_build_txt_detail(lot).as_bytes(),
                "TXT {nom}"
            );
        }
    }
}

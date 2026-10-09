//! Agent d'analyse — client de l'API Groq et construction du contexte métier.
//!
//! ## Ce que l'appel réel a appris (à ne pas ré-oublier)
//!
//! Contrairement à Infoclimat (§ 5), Groq se comporte proprement : vrais codes
//! HTTP, erreurs JSON structurées, et **la clé n'est jamais renvoyée** dans le
//! corps d'erreur. Trois surprises quand même, toutes vérifiées contre l'API :
//!
//! 1. **`openai/gpt-oss-20b` est un modèle à raisonnement.** Sa réponse porte
//!    *deux* champs : `message.reasoning` (le raisonnement) et `message.content`
//!    (la réponse). Le premier consomme des tokens de sortie **avant** le second.
//!    Un premier essai à `max_tokens: 50` a renvoyé `finish_reason: "length"`,
//!    48 tokens de raisonnement et un `content` **vide** — soit un bloc « Agent
//!    IA » désespérément blanc, sans la moindre erreur pour l'expliquer. D'où
//!    [`REASONING_EFFORT`] à `"low"` et un [`MAX_TOKENS`] large, et surtout le
//!    traitement explicite du cas « content vide » dans [`extract_reply`].
//!
//! 2. **La limite est de 8 000 tokens par minute** (`x-ratelimit-limit-tokens`,
//!    palier gratuit `on_demand`). C'est peu : six requêtes moyennes suffisent à
//!    l'épuiser. Cette limite est la raison d'être de [`ContextBuilder`], qui
//!    n'injecte que des **statistiques agrégées** — jamais les mesures brutes,
//!    dont une seule série dépasserait à elle seule le budget d'une minute.
//!
//! 3. **Le dépassement se présente sous deux codes**, et aucun n'est celui
//!    qu'on attend en premier : `413` quand une *seule* requête dépasse le
//!    plafond, `429` quand c'est le *cumul* de la minute. Les deux portent
//!    `code: "rate_limit_exceeded"` — c'est ce champ qui tranche, pas le statut.
//!    Le message d'erreur contient l'identifiant d'organisation et un lien
//!    commercial : [`message_de_quota`] le reformule plutôt que de le recopier.

use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};

use crate::api_keys::{self, KeySource, KeyStatus};
use crate::model::{Category, Point, PointSize};
use crate::stats::PeriodStats;

/// Endpoint compatible OpenAI.
pub const BASE_URL: &str = "https://api.groq.com/openai/v1/chat/completions";

/// Modèle imposé par le cahier des charges du prompt 5.
pub const MODEL: &str = "openai/gpt-oss-20b";

pub const KEY_FILE: &str = "API.Groq.env";
pub const KEY_ENV_VAR: &str = "GWS_GROQ_TOKEN";

pub const KEY_SOURCE: KeySource = KeySource {
    file_name: KEY_FILE,
    env_var: KEY_ENV_VAR,
    provider: "Groq",
    signup_url: "https://console.groq.com/keys",
};

/// Budget de sortie. Large **parce que le raisonnement se sert en premier** :
/// à 50, le modèle a rendu une réponse vide (cf. l'en-tête du module).
const MAX_TOKENS: u32 = 1200;

/// Le raisonnement est facturé au budget de sortie et n'est jamais affiché :
/// `"low"` le ramène d'une cinquantaine de tokens à une dizaine, sans dégrader
/// des réponses de cette longueur.
const REASONING_EFFORT: &str = "low";

/// Basse, mais non nulle : on veut des chiffres reproductibles, pas une
/// reformulation différente à chaque clic sur la même sélection.
const TEMPERATURE: f32 = 0.2;

/// Un appel dépasse rarement 3 s ; 45 s couvre une file d'attente chargée sans
/// laisser le bloc « Agent IA » tourner indéfiniment.
const TIMEOUT: StdDuration = StdDuration::from_secs(45);

/// Plafond de tokens par minute du palier gratuit, **mesuré** sur l'en-tête
/// `x-ratelimit-limit-tokens`. Sert à dimensionner le contexte injecté.
pub const TOKENS_PAR_MINUTE: u32 = 8000;

// ---------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum GroqError {
    #[error("Aucune clé d'API Groq n'est configurée.\n\n{0}")]
    CleIntrouvable(String),

    #[error(
        "L'API Groq a refusé la clé (« {0} »).\n\n\
         La clé de Clee.API/API.Groq.env n'est plus valide ou a été révoquée. \
         Générez-en une nouvelle sur https://console.groq.com/keys et remplacez le \
         contenu du fichier. Contrairement à Infoclimat, une clé Groq ne dépend pas \
         de votre adresse IP : si elle est refusée, c'est bien la clé qui est en cause."
    )]
    Auth(String),

    #[error("{0}")]
    Quota(String),

    #[error("L'API Groq a répondu {status} : {body}")]
    Http { status: u16, body: String },

    #[error("L'appel à l'API Groq a échoué : {0}")]
    Network(String),

    #[error("Réponse de l'API Groq illisible : {0}")]
    Reponse(String),

    #[error(
        "Le modèle a produit un raisonnement mais aucune réponse (budget de sortie \
         épuisé). Reformulez la question, ou réduisez le nombre de points sélectionnés."
    )]
    ReponseVide,
}

/// Reformule un message de dépassement de quota.
///
/// Le message brut de Groq contient l'identifiant d'organisation et un lien vers
/// la page de facturation — ni l'un ni l'autre n'a sa place dans le bloc « Agent
/// IA ». On n'en garde que ce qui est actionnable : le délai avant réessai.
fn message_de_quota(brut: &str) -> String {
    let attente = delai_avant_reessai(brut);

    let mut message = String::from(
        "Quota Groq atteint. Le palier gratuit est limité à 8 000 tokens par minute, \
         soit environ six analyses consécutives.",
    );
    match attente {
        Some(secondes) => message.push_str(&format!(
            " Réessayez dans {secondes} seconde(s)."
        )),
        None => message.push_str(" Réessayez dans une minute."),
    }
    message.push_str(
        " Réduire le nombre de points sélectionnés diminue la taille de chaque appel.",
    );
    message
}

/// Extrait le délai de réessai du message de Groq (`"Please try again in 9.53s"`).
///
/// Arrondi à la seconde supérieure : un message qui dit « réessayez dans 9,53
/// secondes » se lit moins bien qu'« dans 10 secondes », et arrondir vers le bas
/// ferait échouer le réessai.
fn delai_avant_reessai(brut: &str) -> Option<u64> {
    let apres = brut.split("try again in ").nth(1)?;
    let chiffres: String = apres
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let secondes: f64 = chiffres.parse().ok()?;
    Some(secondes.ceil().max(1.0) as u64)
}

// ---------------------------------------------------------------------------
// Le system prompt
// ---------------------------------------------------------------------------

/// Contexte métier injecté en tête de **chaque** appel.
///
/// Le contenu vient du prompt 5 et n'est pas à « simplifier » : la classification
/// en quatre catégories, l'asymétrie de réaction petit/gros TAF et la liste des
/// biais sont ce qui distingue une analyse utile d'une paraphrase de chiffres.
///
/// Le ton est prescrit aussi strictement que le fond : ces données sont lues par
/// des gens qui connaissent le domaine, et une réponse qui explique ce qu'est un
/// TAF entre guillemets leur fait perdre du temps.
pub const SYSTEM_PROMPT: &str = r#"Tu analyses des données de température de trous à froid (TAF) dans le Vercors.

CLASSIFICATION DES SITES (4 catégories, indépendantes de la taille) :
- Cat 1 : fond de vallée / plateau plat ouvert. Non optimal, l'air froid s'échappe. Jamais de taille associée.
- Cat 2 : dépression/doline/cuvette ouverte. Sous-optimal (pente, vent, ciel dégagé empêchent la stagnation). Taille : Gr ou aucune, jamais Pt.
- Cat 3 : dépression/doline/cuvette fermée. Optimal, l'air stagne. Taille Gr ou Pt obligatoire.
- Cat 4 : cuvette de haute montagne, parmi les plus puissantes d'Europe. Optimal mais conditionnel : le comportement s'inverse en cas d'inversion atmosphérique généralisée, et le site est sensible au vent d'altitude. Taille Gr ou Pt obligatoire.

RÈGLES DE COMPORTEMENT THERMIQUE :
- Les petits TAF réagissent vite aux variations générales de température, dans les deux sens (réchauffement comme refroidissement).
- Les gros TAF mettent plus de temps à refroidir, mais refroidissent plus longtemps et se réchauffent plus doucement. Une comparaison sur un épisode froid court désavantage mécaniquement un gros TAF face à un petit.
- Le site de référence du réseau est Darbounouse (catégorie 3, gros, record -34.6°C en 2024). Ne pas confondre avec Derbenouse (catégorie 2, site distinct).

BIAIS À SIGNALER SYSTÉMATIQUEMENT QUAND PERTINENT :
Un écart de température entre deux sites peut avoir des causes qui ne sont pas une différence réelle de puissance du TAF : couverture neigeuse du capteur, exposition au vent, durée de l'épisode froid trop courte pour un gros site, dérive de calibration entre un logger principal et son secours, décalage d'horodatage mal corrigé. Signale ces biais possibles plutôt que de conclure directement qu'un site est "plus froid" sans nuance. Prends également en compte les notes qualitatives des stations (doutes sur l'emplacement, configuration terrain, observations) quand elles sont fournies.

TON : factuel, direct, aucune formule de politesse, vocabulaire du domaine sans guillemets explicatifs. Réponses courtes et denses, pas de remplissage."#;

/// Consignes de format, ajoutées au system prompt.
///
/// Séparées du bloc métier ci-dessus pour que celui-ci reste modifiable sans
/// toucher aux contraintes techniques (longueur, sous-ensemble de Markdown
/// autorisé).
///
/// **Ces consignes doivent rester alignées sur `src/markdown.js`**, qui rend la
/// réponse. Interdire le Markdown *tout court* — ce que faisait la version
/// précédente — n'a pas fonctionné : le modèle produisait des puces `*` et du
/// `**gras**` malgré la consigne, et le bloc affichait les astérisques
/// littéralement. On autorise donc explicitement ce que le rendu sait faire, et
/// on interdit le reste (tableaux, liens, blocs de code), qui n'est pas rendu.
const CONSIGNES_FORMAT: &str = r#"
CONTRAINTES DE FORMAT :
- Réponse en français, 120 mots maximum sauf demande explicite de développement.
- Mise en forme autorisée, et strictement limitée à : puces commençant par un tiret, listes numérotées, **gras** pour un chiffre ou un nom de site. Pas de titres, pas de tableaux, pas de liens, pas de blocs de code : la réponse s'affiche dans un bloc étroit.
- Les températures sont en degrés Celsius, une décimale.
- N'invente aucune valeur : si une statistique n'est pas dans le contexte fourni, dis qu'elle manque.
- Les périodes fournies sont en UTC. Ne les convertis pas."#;

/// Ajoute l'identité de l'utilisateur au system prompt, si elle est connue.
///
/// Volontairement minimal : la consigne dit à quoi sert le prénom (s'adresser à
/// quelqu'un), pas d'en faire un prétexte à des amabilités que le reste du
/// prompt interdit par ailleurs.
pub fn system_prompt(user_name: Option<&str>) -> String {
    let mut prompt = String::with_capacity(SYSTEM_PROMPT.len() + 400);
    prompt.push_str(SYSTEM_PROMPT);
    prompt.push_str(CONSIGNES_FORMAT);

    if let Some(nom) = user_name.map(str::trim).filter(|n| !n.is_empty()) {
        prompt.push_str(&format!(
            "\n\nL'utilisateur s'appelle {nom}. Tu peux t'adresser à lui par son prénom \
             si c'est utile, sans jamais ajouter de formule de politesse."
        ));
    }
    prompt
}

// ---------------------------------------------------------------------------
// Construction du contexte de données
// ---------------------------------------------------------------------------

/// Un point et ses statistiques, prêts à être décrits au modèle.
pub struct PointContext {
    pub point: Point,
    pub stats: PeriodStats,
    pub series_count: usize,
    /// Combien de ces séries sont des **saisies manuelles sur site**.
    ///
    /// Le modèle doit pouvoir dire « cette moyenne repose en partie sur des
    /// relevés recopiés à la main » plutôt que la présenter comme un résultat de
    /// mesure. C'est la seule information de provenance qui lui est transmise, et
    /// c'est celle qui change une conclusion.
    pub manual_entry_series: usize,
}

/// Assemble le message utilisateur : métadonnées + statistiques + demande.
///
/// **Aucune mesure brute n'entre ici.** Le plafond de 8 000 tokens/minute
/// (cf. en-tête du module) rend cela impossible de toute façon : une série
/// Elitech au quart d'heure sur un mois fait déjà 2 900 lignes. Le modèle reçoit
/// donc ce que `stats.rs` a calculé, ce qui est aussi plus fiable — il n'a pas à
/// faire d'arithmétique sur des milliers de valeurs.
pub struct ContextBuilder;

impl ContextBuilder {
    /// Décrit un point : ce qui influe sur l'interprétation, rien d'autre.
    fn decrire_point(ctx: &PointContext) -> String {
        let p = &ctx.point;
        let mut lignes = Vec::new();

        let taille = match p.size {
            Some(PointSize::Gr) => " gros",
            Some(PointSize::Pt) => " petit",
            None => "",
        };
        lignes.push(format!(
            "- {} — catégorie {}{taille} ({})",
            p.name,
            categorie_numero(p.category),
            p.category.label_fr()
        ));

        let mut situation = Vec::new();
        if let Some(alt) = p.altitude_m {
            situation.push(format!("{alt:.0} m"));
        }
        situation.push(format!("{:.4}/{:.4}", p.latitude, p.longitude));
        lignes.push(format!("  situation : {}", situation.join(", ")));

        // Les facteurs de site sont du texte libre saisi par l'utilisateur : ils
        // ne sont pas normalisés, mais ce sont eux qui expliquent la moitié des
        // écarts. On les passe tels quels, seulement ceux qui sont renseignés.
        let facteurs = [
            ("ciel", &p.factor_sky),
            ("vent", &p.factor_wind),
            ("pente", &p.factor_slope),
            ("enneigement", &p.factor_snow_cover),
            ("couvert forestier", &p.factor_forest_cover),
        ];
        let renseignes: Vec<String> = facteurs
            .iter()
            .filter_map(|(nom, valeur)| {
                valeur
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(|v| format!("{nom} : {v}"))
            })
            .collect();
        if !renseignes.is_empty() {
            lignes.push(format!("  facteurs de site — {}", renseignes.join(" ; ")));
        }
        if let Some(true) = p.is_open_terrain {
            lignes.push("  terrain déclaré ouvert".to_string());
        }
        if let Some(notes) = p.notes.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            // Tronqué : une note de terrain peut être longue, et le budget de
            // tokens est le même pour tout le monde.
            lignes.push(format!("  notes : {}", tronquer(notes, 240)));
        }

        lignes.push(format!(
            "  {}",
            decrire_stats(&ctx.stats, ctx.series_count, ctx.manual_entry_series)
        ));
        lignes.join("\n")
    }

    /// Message utilisateur complet.
    pub fn build(points: &[PointContext], demande: &str) -> String {
        let mut texte = String::new();

        if points.is_empty() {
            texte.push_str(
                "Aucun point n'est sélectionné : aucune donnée de mesure n'est jointe à \
                 cette question.\n\n",
            );
        } else {
            texte.push_str(&format!(
                "DONNÉES DES {} POINT(S) SÉLECTIONNÉ(S) — statistiques calculées sur \
                 l'intégralité des mesures enregistrées, périodes en UTC :\n\n",
                points.len()
            ));
            for ctx in points {
                texte.push_str(&Self::decrire_point(ctx));
                texte.push_str("\n\n");
            }

            // Les périodes ne se recouvrent pas forcément — c'est le premier
            // biais à écarter avant de comparer deux minima, et le modèle ne
            // peut le voir que si on le lui dit.
            if points.len() >= 2 {
                if let Some(avertissement) = periodes_disjointes(points) {
                    texte.push_str(&avertissement);
                    texte.push_str("\n\n");
                }
            }
        }

        texte.push_str("DEMANDE :\n");
        texte.push_str(demande);
        texte
    }
}

/// Résumé d'un gros projet : uniquement des agrégats, jamais une ligne par point.
///
/// Au-delà de quelques dizaines de points, décrire chaque site dépasse le
/// plafond de 8 000 tokens/minute et la requête est refusée d'emblée (projet
/// France : 1 153 points). Le modèle reçoit le nombre de points et des
/// répartitions, ce qui suffit pour « résumer l'état du projet ».
pub struct ProjectOverview<'a> {
    pub points: &'a [Point],
    /// Couverture par point, dans n'importe quel ordre ; un point absent compte
    /// comme sans mesure.
    pub coverage: &'a [crate::db::PointCoverage],
    /// Mois courant AAAAMM (UTC), pour classer les points par fraîcheur.
    pub current_month: i64,
}

impl ContextBuilder {
    /// Message utilisateur d'un résumé agrégé : sa taille ne dépend pas du
    /// nombre de points.
    pub fn build_overview(vue: &ProjectOverview, demande: &str) -> String {
        use std::collections::{BTreeMap, HashMap};
        let couv: HashMap<&str, &crate::db::PointCoverage> =
            vue.coverage.iter().map(|c| (c.point_id.as_str(), c)).collect();
        let n = vue.points.len();
        let mut texte = format!(
            "PROJET DE {n} POINTS — résumé agrégé (les descriptions individuelles ne \
             sont pas jointes, le projet est trop grand). Mois en UTC.\n\n"
        );

        let mut categories: BTreeMap<&str, usize> = BTreeMap::new();
        for p in vue.points {
            *categories.entry(categorie_numero(p.category)).or_default() += 1;
        }
        let cats: Vec<String> = categories.iter().map(|(c, k)| format!("{c} : {k}")).collect();
        texte.push_str(&format!("Catégories — {}\n", cats.join(", ")));

        let alts: Vec<f64> = vue.points.iter().filter_map(|p| p.altitude_m).collect();
        if !alts.is_empty() {
            let mn = alts.iter().cloned().fold(f64::INFINITY, f64::min);
            let mx = alts.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            texte.push_str(&format!(
                "Altitude : {mn:.0} à {mx:.0} m ({} point(s) sans altitude)\n",
                n - alts.len()
            ));
        }

        let avec_notes = vue
            .points
            .iter()
            .filter(|p| p.notes.as_deref().is_some_and(|t| !t.trim().is_empty()))
            .count();
        texte.push_str(&format!("Points avec notes de terrain : {avec_notes}\n"));

        let mois_abs = |m: i64| (m / 100) * 12 + (m % 100 - 1);
        let fmt_mois = |m: i64| format!("{:04}-{:02}", m / 100, m % 100);
        let (mut sans, mut total) = (0usize, 0usize);
        let (mut premier, mut dernier) = (i64::MAX, i64::MIN);
        // Fraîcheur : écart en mois entre la dernière mesure et le mois courant.
        let (mut recents, mut moins_6, mut anciens) = (0usize, 0usize, 0usize);
        for p in vue.points {
            match couv.get(p.id.as_str()) {
                Some(c) if c.measurement_count > 0 => {
                    total += c.measurement_count;
                    if let Some(f) = c.first_month {
                        premier = premier.min(f);
                    }
                    if let Some(l) = c.last_month {
                        dernier = dernier.max(l);
                        match mois_abs(vue.current_month) - mois_abs(l) {
                            e if e <= 1 => recents += 1,
                            e if e <= 6 => moins_6 += 1,
                            _ => anciens += 1,
                        }
                    }
                }
                _ => sans += 1,
            }
        }
        texte.push_str(&format!(
            "Points avec mesures : {} ; sans aucune mesure : {sans}\n",
            n - sans
        ));
        if total > 0 && premier <= dernier {
            texte.push_str(&format!(
                "Mesures au total : {total}, de {} à {}\n",
                fmt_mois(premier),
                fmt_mois(dernier)
            ));
            texte.push_str(&format!(
                "Dernière mesure ce mois-ci ou le précédent : {recents} point(s) ; \
                 il y a 2 à 6 mois : {moins_6} ; plus ancienne : {anciens}\n"
            ));
        }
        texte.push_str("\nDEMANDE :\n");
        texte.push_str(demande);
        texte
    }
}

/// `Cat3` → `"3"`. Les catégories hors grille sont nommées telles quelles.
fn categorie_numero(c: Category) -> &'static str {
    match c {
        Category::Cat1 => "1",
        Category::Cat2 => "2",
        Category::Cat3 => "3",
        Category::Cat4 => "4",
        Category::Col => "Col",
        Category::Versants => "Versants",
    }
}

fn tronquer(texte: &str, max: usize) -> String {
    if texte.chars().count() <= max {
        return texte.to_string();
    }
    let court: String = texte.chars().take(max).collect();
    format!("{court}…")
}

fn opt_temp(valeur: Option<f64>) -> String {
    match valeur {
        Some(v) => format!("{v:.1} °C"),
        None => "n/d".to_string(),
    }
}

/// Résumé statistique d'un point, sur une ligne dense.
fn decrire_stats(s: &PeriodStats, series_count: usize, manual_entry_series: usize) -> String {
    if s.measurement_count == 0 {
        return "aucune mesure enregistrée".to_string();
    }

    let periode = match (s.first_utc, s.last_utc) {
        (Some(d), Some(f)) => format!(
            "{} → {} UTC",
            d.format("%Y-%m-%d %H:%M"),
            f.format("%Y-%m-%d %H:%M")
        ),
        _ => "période inconnue".to_string(),
    };

    let mut parties = vec![
        format!(
            "{} mesures sur {} jour(s) ({series_count} série(s)), {periode}",
            s.measurement_count, s.day_count
        ),
        format!(
            "min {} le {}",
            opt_temp(s.extreme_min_c),
            s.extreme_min_date
                .map(|d| d.to_string())
                .unwrap_or_else(|| "?".into())
        ),
        format!("max {}", opt_temp(s.extreme_max_c)),
        format!("moyenne {}", opt_temp(s.mean_c)),
        format!("moyenne des minima quotidiens {}", opt_temp(s.mean_daily_min_c)),
        format!("amplitude quotidienne moyenne {}", opt_temp(s.mean_amplitude_c)),
    ];

    if let (Some(amp), Some(date)) = (s.max_amplitude_c, s.max_amplitude_date) {
        parties.push(format!("amplitude maximale {amp:.1} °C le {date}"));
    }

    // Dit **avant** que le modèle conclue, et non en note de bas de page : une
    // série saisie à la main est une estimation ponctuelle sur une plage horaire,
    // pas un échantillonnage continu, et cela change ce qu'on peut affirmer.
    if manual_entry_series > 0 {
        parties.push(format!(
            "⚠ dont {manual_entry_series} série(s) de saisie manuelle sur site \
             (valeurs relevées à la main sur une plage horaire, pas des mesures \
             de logger)"
        ));
    }

    parties.join(" ; ")
}

/// Signale des périodes de mesure qui ne se recouvrent pas.
///
/// Comparer le minimum d'un site mesuré en janvier à celui d'un site mesuré en
/// mars ne dit rien du site : c'est le biais le plus facile à commettre et le
/// plus facile à détecter, donc on le détecte ici plutôt que d'espérer que le
/// modèle le remarque.
fn periodes_disjointes(points: &[PointContext]) -> Option<String> {
    let bornes: Vec<_> = points
        .iter()
        .filter_map(|c| Some((c.point.name.as_str(), c.stats.first_utc?, c.stats.last_utc?)))
        .collect();
    if bornes.len() < 2 {
        return None;
    }

    let debut_commun = bornes.iter().map(|(_, d, _)| *d).max()?;
    let fin_commune = bornes.iter().map(|(_, _, f)| *f).min()?;

    if debut_commun > fin_commune {
        Some(
            "ATTENTION : les périodes de mesure de ces points ne se recouvrent pas du tout. \
             Toute comparaison directe de leurs extrêmes porte sur des épisodes météo \
             différents et ne dit rien de la puissance relative des sites. Signale-le en \
             premier."
                .to_string(),
        )
    } else {
        let jours = (fin_commune - debut_commun).num_days();
        Some(format!(
            "Période commune aux points sélectionnés : {} → {} UTC, soit {jours} jour(s). \
             Les statistiques ci-dessus portent chacune sur la période complète du point, \
             pas seulement sur ce recouvrement.",
            debut_commun.format("%Y-%m-%d"),
            fin_commune.format("%Y-%m-%d")
        ))
    }
}

/// Modèle multimodal Vision recommandé et actif sur Groq (2026).
/// Contexte 131k, latence mesurée ~1s sur les tableaux météo.
pub const VISION_MODEL: &str = "qwen/qwen3.8-27b";

/// Modèle Vision de secours si le modèle principal est indisponible.
pub const VISION_MODEL_FALLBACK: &str = "qwen/qwen3.6-27b";

// ---------------------------------------------------------------------------
// Client HTTP
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(untagged)]
pub enum ContentPart<'a> {
    Text {
        #[serde(rename = "type")]
        kind: &'static str,
        text: &'a str,
    },
    ImageUrl {
        #[serde(rename = "type")]
        kind: &'static str,
        image_url: ImageUrlPayload<'a>,
    },
}

#[derive(Serialize)]
pub struct ImageUrlPayload<'a> {
    pub url: &'a str,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum MessageContent<'a> {
    Text(&'a str),
    Parts(Vec<ContentPart<'a>>),
}

#[derive(Serialize)]
pub struct ChatMessage<'a> {
    pub role: &'a str,
    pub content: MessageContent<'a>,
}

#[derive(Serialize)]
pub struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    pub kind: &'a str,
}

#[derive(Serialize)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<ChatMessage<'a>>,
    pub max_tokens: u32,
    pub temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat<'a>>,
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    message: Option<ResponseMessage>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    /// Le raisonnement du modèle. Jamais affiché — il est ici pour que la
    /// désérialisation ne le perde pas silencieusement, et pour le diagnostic.
    #[serde(default)]
    reasoning: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

#[derive(Deserialize)]
struct ApiErrorEnvelope {
    error: ApiError,
}

#[derive(Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: Option<String>,
}

/// Ce qu'un appel réussi rend.
#[derive(Debug, Clone, Serialize)]
pub struct AgentReply {
    pub text: String,
    pub model: String,
    pub usage: Option<Usage>,
}

/// Extrait la réponse d'un corps JSON.
///
/// Le cas « `content` vide alors que `reasoning` est rempli » est un vrai cas
/// observé, pas une précaution théorique : c'est la première réponse qu'a rendue
/// l'API lors du test d'intégration. Le confondre avec une réponse valide
/// afficherait un bloc vide sans explication.
fn extract_reply(corps: &str, model_name: &str) -> Result<AgentReply, GroqError> {
    let parsee: ChatResponse = serde_json::from_str(corps)
        .map_err(|e| GroqError::Reponse(format!("{e} — corps reçu : {}", tronquer(corps, 200))))?;

    let choix = parsee
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| GroqError::Reponse("aucun choix dans la réponse".into()))?;

    let tronquee = choix.finish_reason.as_deref() == Some("length");
    let message = choix.message.unwrap_or(ResponseMessage {
        content: None,
        reasoning: None,
    });

    let texte = message.content.unwrap_or_default();
    let texte = texte.trim();

    if texte.is_empty() {
        // Le raisonnement seul ne constitue pas une réponse : il n'est ni destiné
        // à l'utilisateur ni fiable hors contexte. On le trace côté console pour
        // le diagnostic — c'est la seule trace qui explique un bloc resté vide.
        if let Some(raisonnement) = message.reasoning.as_deref().filter(|r| !r.is_empty()) {
            println!(
                "[groq] réponse vide (budget épuisé). Début du raisonnement produit : {}",
                tronquer(raisonnement, 160)
            );
        }
        return Err(GroqError::ReponseVide);
    }

    let mut texte = texte.to_string();
    if tronquee {
        texte.push_str("\n\n[réponse tronquée : budget de sortie atteint]");
    }

    Ok(AgentReply {
        text: texte,
        model: model_name.to_string(),
        usage: parsee.usage,
    })
}

/// Traduit un corps d'erreur de Groq en [`GroqError`].
///
/// C'est **`code`** qui tranche, pas le statut HTTP : un dépassement de quota
/// arrive en `413` ou en `429` selon qu'il vient d'une requête isolée ou du
/// cumul de la minute (cf. l'en-tête du module).
fn classer_erreur(status: u16, corps: &str, model_name: &str) -> GroqError {
    let analysee: Option<ApiError> = serde_json::from_str::<ApiErrorEnvelope>(corps)
        .ok()
        .map(|e| e.error);

    let (message, code) = match analysee {
        Some(e) => (e.message, e.code.unwrap_or_default()),
        None => (tronquer(corps, 300), String::new()),
    };

    if code == "rate_limit_exceeded" || status == 429 || status == 413 {
        return GroqError::Quota(message_de_quota(&message));
    }
    if code == "invalid_api_key" || status == 401 || status == 403 {
        return GroqError::Auth(message);
    }
    if code == "model_not_found" || code == "model_decommissioned" || status == 404 {
        return GroqError::Http {
            status,
            body: format!(
                "{message} (le modèle attendu par cette application est {model_name})"
            ),
        };
    }

    GroqError::Http {
        status,
        body: tronquer(&message, 300),
    }
}

pub struct GroqClient {
    token: String,
    base_url: String,
    agent: reqwest::blocking::Client,
}

impl GroqClient {
    pub fn from_local_key() -> Result<Self, GroqError> {
        let (token, _) = api_keys::load_key(&KEY_SOURCE)
            .map_err(|e| GroqError::CleIntrouvable(e.to_string()))?;
        Self::with_token(token, BASE_URL)
    }

    pub fn with_token(
        token: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, GroqError> {
        let agent = crate::http_client::client_builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| GroqError::Network(e.to_string()))?;

        Ok(GroqClient {
            token: token.into(),
            base_url: base_url.into(),
            agent,
        })
    }

    /// Exécute une requête de chat générique.
    pub fn execute_chat(&self, req: &ChatRequest<'_>) -> Result<AgentReply, GroqError> {
        let corps_requete = serde_json::to_string(req)
            .map_err(|e| GroqError::Reponse(format!("requête non sérialisable : {e}")))?;

        let reponse = self
            .agent
            .post(&self.base_url)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .body(corps_requete)
            .send()
            .map_err(|e| GroqError::Network(scrub(&e.to_string(), &self.token)))?;

        let status = reponse.status().as_u16();
        let corps = reponse
            .text()
            .map_err(|e| GroqError::Network(scrub(&e.to_string(), &self.token)))?;

        if status >= 400 {
            return Err(classer_erreur(status, &scrub(&corps, &self.token), req.model));
        }
        extract_reply(&corps, req.model)
    }

    /// Un appel complet pour l'analyse textuelle métier.
    pub fn complete(&self, system: &str, user: &str) -> Result<AgentReply, GroqError> {
        let req = ChatRequest {
            model: MODEL,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: MessageContent::Text(system),
                },
                ChatMessage {
                    role: "user",
                    content: MessageContent::Text(user),
                },
            ],
            max_tokens: MAX_TOKENS,
            temperature: TEMPERATURE,
            reasoning_effort: Some(REASONING_EFFORT),
            response_format: None,
        };
        self.execute_chat(&req)
    }

    /// Un appel avec consigne JSON et modèle au choix.
    pub fn complete_json(&self, system: &str, user: &str, model: Option<&str>) -> Result<AgentReply, GroqError> {
        let chosen_model = model.unwrap_or(MODEL);
        let req = ChatRequest {
            model: chosen_model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: MessageContent::Text(system),
                },
                ChatMessage {
                    role: "user",
                    content: MessageContent::Text(user),
                },
            ],
            max_tokens: 2000,
            temperature: 0.1,
            reasoning_effort: if chosen_model == MODEL { Some(REASONING_EFFORT) } else { None },
            response_format: Some(ResponseFormat { kind: "json_object" }),
        };
        self.execute_chat(&req)
    }

    /// Un appel multimodal (Vision) avec image base64 formatée en Data URL.
    pub fn complete_vision(
        &self,
        system: &str,
        user_text: &str,
        image_data_url: &str,
        model: Option<&str>,
    ) -> Result<AgentReply, GroqError> {
        let chosen_model = model.unwrap_or(VISION_MODEL);
        let req = ChatRequest {
            model: chosen_model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: MessageContent::Text(system),
                },
                ChatMessage {
                    role: "user",
                    content: MessageContent::Parts(vec![
                        ContentPart::Text {
                            kind: "text",
                            text: user_text,
                        },
                        ContentPart::ImageUrl {
                            kind: "image_url",
                            image_url: ImageUrlPayload { url: image_data_url },
                        },
                    ]),
                },
            ],
            max_tokens: 2500,
            temperature: 0.1,
            reasoning_effort: None,
            response_format: Some(ResponseFormat { kind: "json_object" }),
        };

        match self.execute_chat(&req) {
            Ok(reply) => Ok(reply),
            Err(e) if chosen_model == VISION_MODEL => {
                // Tentative automatique sur le modèle de secours vision si erreur modèle (décommissionné ou introuvable)
                let is_model_error = match &e {
                    GroqError::Http { status, body } => {
                        *status == 404
                            || (*status == 400
                                && (body.contains("decommissioned")
                                    || body.contains("not found")
                                    || body.contains("model_decommissioned")))
                    }
                    _ => false,
                };
                if is_model_error {
                    let fallback_req = ChatRequest {
                        model: VISION_MODEL_FALLBACK,
                        ..req
                    };
                    self.execute_chat(&fallback_req)
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }
}

/// Efface la clé d'un texte destiné à l'affichage.
///
/// Groq ne renvoie pas la clé dans ses erreurs — vérifié : une clé invalide ne
/// produit que `"Invalid API Key"`. Mais `reqwest` peut recopier des en-têtes
/// dans un message d'erreur réseau, et la leçon d'Infoclimat (§ 5 du README) est
/// qu'on ne fait pas confiance là-dessus : toute chaîne qui vient du réseau passe
/// par ici avant d'entrer dans une erreur.
pub fn scrub(texte: &str, token: &str) -> String {
    if token.is_empty() {
        texte.to_string()
    } else {
        texte.replace(token, "***")
    }
}

/// Statut de la clé Groq pour l'écran Paramètres.
pub fn key_status() -> KeyStatus {
    api_keys::key_status(&KEY_SOURCE)
}

/// Enregistre la clé Groq saisie à la main, et renvoie le chemin écrit.
///
/// Passe-plat volontaire vers [`api_keys::save_key`] : l'écran de première
/// ouverture et l'écran Paramètres appellent tous deux la même commande, qui
/// appelle cette fonction, qui écrit dans le fichier que [`key_status`] relit
/// juste après. Il n'existe donc aucun chemin par lequel les deux écrans
/// pourraient afficher des clés différentes.
pub fn save_key(brut: &str) -> Result<std::path::PathBuf, api_keys::KeyError> {
    api_keys::save_key(&KEY_SOURCE, brut)
}

/// `GWS_GROQ_TOKEN` est-elle posée ? Voir [`api_keys::env_var_prend_le_dessus`].
pub fn env_var_prend_le_dessus() -> bool {
    api_keys::env_var_prend_le_dessus(&KEY_SOURCE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_system_prompt_porte_tout_le_contexte_metier() {
        let p = system_prompt(None);
        // Les quatre catégories, la règle de taille, l'asymétrie petit/gros et
        // le piège Darbounouse/Derbenouse : si l'un saute, l'agent redevient un
        // commentateur de chiffres.
        assert!(p.contains("Cat 1"));
        assert!(p.contains("Cat 4"));
        assert!(p.contains("Taille Gr ou Pt obligatoire"));
        assert!(p.contains("jamais Pt"));
        assert!(p.contains("Darbounouse"));
        assert!(p.contains("Derbenouse"));
        assert!(p.contains("-34.6"));
        assert!(p.contains("BIAIS À SIGNALER"));
        assert!(p.contains("dérive de calibration"));
        assert!(p.contains("aucune formule de politesse"));
    }

    #[test]
    fn le_prenom_est_injecte_sans_autoriser_les_amabilites() {
        let p = system_prompt(Some("Robin"));
        assert!(p.contains("Robin"));
        assert!(p.contains("sans jamais ajouter de formule de politesse"));

        // Un prénom vide ou blanc ne doit pas produire de phrase bancale.
        assert!(!system_prompt(Some("   ")).contains("L'utilisateur s'appelle"));
        assert!(!system_prompt(None).contains("L'utilisateur s'appelle"));
    }

    #[test]
    fn le_quota_est_reconnu_sur_ses_deux_codes_http() {
        // Cas réel observé : requête unique trop grosse → 413.
        let corps_413 = r#"{"error":{"message":"Request too large for model `openai/gpt-oss-20b` in organization `org_01m0` service tier `on_demand` on tokens per minute (TPM): Limit 8000, Requested 21092, please reduce your message size and try again. Need more tokens? Upgrade to Dev Tier today at https://console.groq.com/settings/billing","type":"tokens","code":"rate_limit_exceeded"}}"#;
        assert!(matches!(
            classer_erreur(413, corps_413, MODEL),
            GroqError::Quota(_)
        ));

        // Cas réel observé : cumul de la minute dépassé → 429.
        let corps_429 = r#"{"error":{"message":"Rate limit reached for model `openai/gpt-oss-20b` in organization `org_01m0` service tier `on_demand` on tokens per minute (TPM): Limit 8000, Used 7369, Requested 1902. Please try again in 9.532499999s.","type":"tokens","code":"rate_limit_exceeded"}}"#;
        let e = classer_erreur(429, corps_429, MODEL);
        let message = e.to_string();
        assert!(matches!(e, GroqError::Quota(_)));
        // Le délai est repris, l'identifiant d'organisation et le lien
        // commercial ne le sont pas.
        assert!(message.contains("10 seconde"), "message : {message}");
        assert!(!message.contains("org_01m0"));
        assert!(!message.contains("billing"));
    }

    #[test]
    fn le_delai_de_reessai_est_arrondi_vers_le_haut() {
        assert_eq!(delai_avant_reessai("Please try again in 9.532499999s."), Some(10));
        assert_eq!(delai_avant_reessai("try again in 2s"), Some(2));
        assert_eq!(delai_avant_reessai("aucun délai ici"), None);
        // Un délai infime doit rester ≥ 1 s : « réessayez dans 0 seconde » n'aide personne.
        assert_eq!(delai_avant_reessai("try again in 0.02s"), Some(1));
    }

    #[test]
    fn une_cle_refusee_donne_une_erreur_dediee() {
        // Corps exact renvoyé par l'API pour une clé inconnue.
        let corps = r#"{"error":{"message":"Invalid API Key","type":"invalid_request_error","code":"invalid_api_key"}}"#;
        let e = classer_erreur(401, corps, MODEL);
        assert!(matches!(e, GroqError::Auth(_)));
        let message = e.to_string();
        assert!(message.contains("Clee.API/API.Groq.env"));
        // Le contraste avec Infoclimat est la première chose à vérifier quand on
        // diagnostique : une clé Groq ne dépend pas de l'IP.
        assert!(message.contains("ne dépend pas"));
    }

    #[test]
    fn un_modele_inconnu_rappelle_le_modele_attendu() {
        let corps = r#"{"error":{"message":"The model `openai/gpt-oss-999b` does not exist or you do not have access to it.","type":"invalid_request_error","code":"model_not_found"}}"#;
        assert!(classer_erreur(404, corps, MODEL).to_string().contains(MODEL));
    }

    /// Le piège n°1 de ce modèle : du raisonnement, pas de réponse.
    #[test]
    fn une_reponse_sans_contenu_mais_avec_raisonnement_est_une_erreur() {
        // Corps réel du premier appel de test (`max_tokens: 50`).
        let corps = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"","reasoning":"The user says: Test de connexion..."},"finish_reason":"length"}],"usage":{"prompt_tokens":85,"completion_tokens":50,"total_tokens":135}}"#;
        assert!(matches!(extract_reply(corps, MODEL), Err(GroqError::ReponseVide)));
    }

    #[test]
    fn une_reponse_normale_est_extraite_sans_le_raisonnement() {
        let corps = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"Darbounouse : min -21,4 °C.","reasoning":"Need to respond factually."},"finish_reason":"stop"}],"usage":{"prompt_tokens":108,"completion_tokens":50,"total_tokens":158}}"#;
        let reponse = extract_reply(corps, MODEL).unwrap();
        assert_eq!(reponse.text, "Darbounouse : min -21,4 °C.");
        assert!(!reponse.text.contains("Need to respond"));
        assert_eq!(reponse.usage.unwrap().total_tokens, 158);
    }

    #[test]
    fn une_reponse_tronquee_le_dit() {
        let corps = r#"{"choices":[{"message":{"content":"Le minimum de Darbounouse est"},"finish_reason":"length"}]}"#;
        let reponse = extract_reply(corps, MODEL).unwrap();
        assert!(reponse.text.contains("tronquée"));
    }

    #[test]
    fn la_cle_est_effacee_de_tout_texte_reseau() {
        let token = "gsk_secret123";
        let brut = format!("error sending request with header authorization: Bearer {token}");
        assert!(!scrub(&brut, token).contains(token));
        // Un token vide ne doit pas transformer le message en bouillie.
        assert_eq!(scrub("message", ""), "message");
    }

    // -----------------------------------------------------------------------
    // Construction du contexte
    // -----------------------------------------------------------------------

    use crate::model::{now_utc, Measurement};
    use chrono::{Duration, TimeZone, Utc};

    fn mesure(serie: &str, heures: i64, t: f64) -> Measurement {
        let instant = Utc.with_ymd_and_hms(2025, 1, 10, 0, 0, 0).unwrap() + Duration::hours(heures);
        Measurement::new_utc(serie, instant, t)
    }

    #[test]
    fn resume_agrege_de_1153_points_reste_petit_et_sans_noms() {
        use crate::db::PointCoverage;
        let points: Vec<Point> = (0..1153)
            .map(|i| {
                let mut p = point_test(&format!("Station{i:04}"), Category::Cat3, None);
                p.notes = Some("une longue note de terrain ".repeat(20));
                p
            })
            .collect();
        // Une station sur trois sans mesure, les autres finies en 2026-09.
        let coverage: Vec<PointCoverage> = points
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 3 != 0)
            .map(|(_, p)| PointCoverage {
                point_id: p.id.clone(),
                measurement_count: 1000,
                first_month: Some(202401),
                last_month: Some(202609),
            })
            .collect();
        let vue = ProjectOverview { points: &points, coverage: &coverage, current_month: 202610 };
        let texte = ContextBuilder::build_overview(&vue, "Résume.");
        assert!(texte.len() < 1500, "résumé de {} caractères", texte.len());
        assert!(texte.contains("PROJET DE 1153 POINTS"));
        assert!(texte.contains("sans aucune mesure : 385"));
        assert!(texte.contains("de 2024-01 à 2026-09"));
        assert!(texte.contains("ce mois-ci ou le précédent : 768"));
        assert!(!texte.contains("Station0001"), "aucun nom de point ne doit partir");
        assert!(!texte.contains("longue note"));
    }

    #[test]
    fn avertissement_saisie_manuelle_sans_espaces_parasites() {
        let s = crate::stats::period_stats(&[mesure("s", 0, -3.0), mesure("s", 1, -2.0)]);
        let t = decrire_stats(&s, 1, 1);
        assert!(!t.contains("  "), "espaces multiples : {t}");
    }

    fn point_test(nom: &str, categorie: Category, taille: Option<PointSize>) -> Point {
        Point {
            id: format!("id-{nom}"),
            project_id: "p".into(),
            name: nom.into(),
            latitude: 44.9812,
            longitude: 5.4213,
            altitude_m: Some(1180.0),
            category: categorie,
            size: taille,
            factor_sky: Some("dégagé".into()),
            factor_wind: None,
            factor_slope: None,
            factor_snow_cover: Some("30 cm".into()),
            factor_forest_cover: None,
            is_open_terrain: None,
            notes: None,
            infoclimat_station_id: None,
            romma_station_id: None,
            created_at: now_utc(),
            series_count: 1,
        }
    }

    fn contexte(nom: &str, temperatures: &[f64]) -> PointContext {
        let mesures: Vec<Measurement> = temperatures
            .iter()
            .enumerate()
            .map(|(i, t)| mesure("s", i as i64, *t))
            .collect();
        PointContext {
            point: point_test(nom, Category::Cat3, Some(PointSize::Gr)),
            stats: crate::stats::period_stats(&mesures),
            series_count: 1,
            manual_entry_series: 0,
        }
    }

    #[test]
    fn le_contexte_porte_la_categorie_la_taille_et_les_stats() {
        let ctx = contexte("Darbounouse", &[-12.0, -21.4, -8.0]);
        let texte = ContextBuilder::build(&[ctx], "Analyse cette série.");

        assert!(texte.contains("Darbounouse"));
        assert!(texte.contains("catégorie 3"));
        assert!(texte.contains("gros"));
        assert!(texte.contains("Dépression fermée"));
        // Les facteurs renseignés passent, les vides ne créent pas de ligne vide.
        assert!(texte.contains("ciel : dégagé"));
        assert!(texte.contains("enneigement : 30 cm"));
        assert!(!texte.contains("vent :"));
        // Les statistiques, pas les valeurs brutes.
        assert!(texte.contains("min -21.4 °C"));
        assert!(texte.contains("Analyse cette série."));
    }

    #[test]
    fn le_contexte_reste_compact_face_a_la_limite_de_tokens() {
        // Quatre points bien remplis : le contexte doit rester loin des 8 000
        // tokens/minute du palier gratuit. On mesure en caractères (~4 par
        // token) : 4 000 caractères ≈ 1 000 tokens, large marge.
        let points: Vec<PointContext> = ["Darbounouse", "Derbenouse", "Moucherolle", "Chaumailloux"]
            .iter()
            .map(|n| contexte(n, &[-12.0, -21.4, -8.0, -3.2, -17.9]))
            .collect();
        let texte = ContextBuilder::build(&points, "Compare ces points.");
        assert!(
            texte.len() < 4000,
            "contexte de {} caractères : trop gros pour un plafond de {TOKENS_PAR_MINUTE} tokens/min",
            texte.len()
        );
    }

    #[test]
    fn des_periodes_qui_ne_se_recouvrent_pas_sont_signalees() {
        let janvier = contexte("A", &[-10.0, -12.0]);

        let mars: Vec<Measurement> = (0..3)
            .map(|i| {
                let instant = Utc.with_ymd_and_hms(2025, 3, 20, 0, 0, 0).unwrap()
                    + Duration::hours(i);
                Measurement::new_utc("s2", instant, -2.0)
            })
            .collect();
        let b = PointContext {
            point: point_test("B", Category::Cat2, None),
            stats: crate::stats::period_stats(&mars),
            series_count: 1,
            manual_entry_series: 0,
        };

        let texte = ContextBuilder::build(&[janvier, b], "Compare.");
        assert!(texte.contains("ne se recouvrent pas"));
    }

    #[test]
    fn un_point_sans_mesure_le_dit_plutot_que_d_afficher_des_zeros() {
        let vide = PointContext {
            point: point_test("Neuf", Category::Cat1, None),
            stats: crate::stats::PeriodStats::empty(),
            series_count: 0,
            manual_entry_series: 0,
        };
        let texte = ContextBuilder::build(&[vide], "Résume.");
        assert!(texte.contains("aucune mesure enregistrée"));
        assert!(!texte.contains("0.0 °C"));
    }

    #[test]
    fn le_contexte_inclut_les_notes_de_station_quand_presentes() {
        let mut pt_avec_notes = point_test("Station Douteuse", Category::Cat2, None);
        pt_avec_notes.notes = Some("Doute sur la pertinence du site, cuvette trop ventilée".into());
        let ctx_notes = PointContext {
            point: pt_avec_notes,
            stats: crate::stats::PeriodStats::empty(),
            series_count: 0,
            manual_entry_series: 0,
        };

        let pt_sans_notes = point_test("Station Normale", Category::Cat3, Some(PointSize::Gr));
        let ctx_sans_notes = PointContext {
            point: pt_sans_notes,
            stats: crate::stats::PeriodStats::empty(),
            series_count: 0,
            manual_entry_series: 0,
        };

        let texte = ContextBuilder::build(&[ctx_notes, ctx_sans_notes], "Résume.");
        assert!(texte.contains("notes : Doute sur la pertinence du site, cuvette trop ventilée"));
        // Vérifie que le prompt system contient bien la consigne de prise en compte des notes
        assert!(SYSTEM_PROMPT.contains("notes qualitatives"));
    }

    // -----------------------------------------------------------------------
    // Appel réel — `#[ignore]` par défaut
    // -----------------------------------------------------------------------

    /// Vérifie la clé du poste **contre l'API réelle**, comme pour Infoclimat.
    ///
    /// ```text
    /// cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture appel_reel_groq
    /// ```
    #[test]
    #[ignore = "dépend du réseau et consomme le quota (8 000 tokens/minute)"]
    fn appel_reel_groq() {
        let client = match GroqClient::from_local_key() {
            Ok(c) => c,
            Err(e) => {
                println!("[groq] pas de clé exploitable : {e}");
                return;
            }
        };

        let ctx = contexte("Darbounouse", &[-12.0, -21.4, -8.0]);
        let user = ContextBuilder::build(&[ctx], "Analyse cette série en deux phrases.");

        match client.complete(&system_prompt(Some("Robin")), &user) {
            Ok(r) => {
                println!("[groq] modèle : {}", r.model);
                if let Some(u) = r.usage {
                    println!(
                        "[groq] tokens : {} entrée + {} sortie = {} (plafond {TOKENS_PAR_MINUTE}/min)",
                        u.prompt_tokens, u.completion_tokens, u.total_tokens
                    );
                }
                println!("[groq] réponse :\n{}", r.text);
                assert!(!r.text.trim().is_empty());
            }
            Err(e) => panic!("appel réel échoué : {e}"),
        }
    }

    /// Vérifie le modèle de vision **contre l'API réelle Groq**.
    ///
    /// ```text
    /// cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture appel_reel_vision_groq
    /// ```
    #[test]
    #[ignore = "dépend du réseau et consomme le quota Groq"]
    fn appel_reel_vision_groq() {
        let client = match GroqClient::from_local_key() {
            Ok(c) => c,
            Err(e) => {
                println!("[groq-vision] pas de clé exploitable : {e}");
                return;
            }
        };

        let image_bytes = match std::fs::read("../docs/samples/test_table_meteo.png") {
            Ok(b) => b,
            Err(_) => match std::fs::read("docs/samples/test_table_meteo.png") {
                Ok(b) => b,
                Err(e) => panic!("Impossible de lire docs/samples/test_table_meteo.png : {e}"),
            },
        };

        fn b64_encode(bytes: &[u8]) -> String {
            const CHARSET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
            for chunk in bytes.chunks(3) {
                let b0 = chunk[0] as usize;
                let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
                let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
                let triple = (b0 << 16) | (b1 << 8) | b2;
                out.push(CHARSET[(triple >> 18) & 0x3F] as char);
                out.push(CHARSET[(triple >> 12) & 0x3F] as char);
                if chunk.len() > 1 {
                    out.push(CHARSET[(triple >> 6) & 0x3F] as char);
                } else {
                    out.push('=');
                }
                if chunk.len() > 2 {
                    out.push(CHARSET[triple & 0x3F] as char);
                } else {
                    out.push('=');
                }
            }
            out
        }
        let b64 = b64_encode(&image_bytes);
        let data_url = format!("data:image/png;base64,{b64}");

        let sys = r#"Tu es un assistant vision météo. Réponds UNIQUEMENT avec un objet JSON : {"rows":[{"timestamp":"2026-09-06 14:00:00","temperature":18.5}],"explanation":"ok"}"#;
        let user = "Extrais les données météo de cette image.";

        match client.complete_vision(sys, user, &data_url, None) {
            Ok(r) => {
                println!("[groq-vision] modèle utilisé : {}", r.model);
                if let Some(u) = r.usage {
                    println!(
                        "[groq-vision] tokens : {} entrée + {} sortie = {}",
                        u.prompt_tokens, u.completion_tokens, u.total_tokens
                    );
                }
                println!("[groq-vision] réponse brute :\n{}", r.text);
                assert!(!r.text.trim().is_empty());
                assert!(r.text.contains('{') && r.text.contains('}'));
            }
            Err(e) => panic!("appel réel vision échoué : {e}"),
        }
    }
}

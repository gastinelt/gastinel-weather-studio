//! Modèle de données du domaine « chasse aux TAF » (Trous À Froid).
//!
//! Décisions d'architecture prises ici (à discuter / faire évoluer) :
//!
//! * **Identifiants** : UUID v4 stockés en `TEXT`. Stables, sans collision si un
//!   jour on fusionne deux bases (deux postes de la famille). Exception :
//!   [`Measurement::id`] est un entier auto-incrémenté, car il peut y avoir des
//!   centaines de milliers de lignes et une mesure n'est jamais référencée depuis
//!   l'extérieur — un UUID par mesure coûterait cher pour zéro bénéfice.
//!
//! * **Instants** : côté Rust, tout instant est un [`chrono::DateTime<Utc>`].
//!   Il n'existe *aucun* `NaiveDateTime` dans le modèle persisté : la conversion
//!   « horodatage brut du logger → UTC » est faite une seule fois, à l'import
//!   (voir [`crate::elitech_import`] et [`apply_clock_offset`]). C'est la
//!   contrainte n°1 du projet.
//!
//! * **Facteurs de site** : `Option<String>` pour l'instant. Le format de saisie
//!   n'est pas figé (note /5 ? pourcentage ? texte libre ?) ; on stocke du texte
//!   libre pour ne rien perdre, on typera quand la grille de notation sera arrêtée.
//!
//! * **Validation catégorie/taille** : implémentée dans [`validate_category_size`],
//!   appelée par [`Point::from_new`]. Elle est *aussi* dupliquée en contrainte
//!   `CHECK` SQL (voir `db.rs`) — défense en profondeur : même un `INSERT` manuel
//!   avec un client SQLite tiers ne peut pas corrompre la classification.

use chrono::{DateTime, Duration, Timelike, Utc};

use crate::parsing::DateOrder;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Classification des points
// ---------------------------------------------------------------------------

/// Catégorie morphologique du site (obligatoire sur tout [`Point`]).
///
/// Rappel métier (documentaire, n'influence aucun calcul) :
/// * `Cat1` — fond de vallée / plateau plat ouvert : non optimal, l'air froid s'échappe.
/// * `Cat2` — dépression / doline / cuvette **ouverte** : sous-optimale.
/// * `Cat3` — dépression / doline / cuvette **fermée** : optimale, l'air stagne.
/// * `Cat4` — cuvette de haute montagne : optimale mais conditionnelle.
/// * `Col` — passage entre deux versants.
/// * `Versants` — station située sur un versant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Category {
    Cat1,
    Cat2,
    Cat3,
    Cat4,
    Col,
    Versants,
}

/// Taille du site. `Gr` = gros, `Pt` = petit.
///
/// L'absence de taille est représentée par `Option::None` côté Rust et par
/// `NULL` côté SQLite — il n'y a volontairement pas de variante `None` dans
/// l'enum, pour que le compilateur force à traiter le cas « pas de taille ».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PointSize {
    Gr,
    Pt,
}

const NONE_ONLY: [Option<PointSize>; 1] = [None];
const NONE_OR_GR: [Option<PointSize>; 2] = [None, Some(PointSize::Gr)];
const GR_OR_PT: [Option<PointSize>; 2] = [Some(PointSize::Gr), Some(PointSize::Pt)];

impl Category {
    pub const ALL: [Category; 6] = [
        Category::Cat1,
        Category::Cat2,
        Category::Cat3,
        Category::Cat4,
        Category::Col,
        Category::Versants,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Cat1 => "Cat1",
            Category::Cat2 => "Cat2",
            Category::Cat3 => "Cat3",
            Category::Cat4 => "Cat4",
            Category::Col => "Col",
            Category::Versants => "Versants",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        match raw.trim() {
            "Cat1" => Ok(Category::Cat1),
            "Cat2" => Ok(Category::Cat2),
            "Cat3" => Ok(Category::Cat3),
            "Cat4" => Ok(Category::Cat4),
            "Col" => Ok(Category::Col),
            "Versants" => Ok(Category::Versants),
            other => Err(ValidationError::UnknownCategory(other.to_string())),
        }
    }

    /// Libellé métier, exposé à l'UI (aucune logique ne doit en dépendre).
    pub fn label_fr(self) -> &'static str {
        match self {
            Category::Cat1 => "Fond de vallée / plateau plat ouvert",
            Category::Cat2 => "Dépression ouverte",
            Category::Cat3 => "Dépression fermée",
            Category::Cat4 => "Cuvette de haute montagne",
            Category::Col => "Col",
            Category::Versants => "Versants",
        }
    }

    /// Tailles autorisées pour cette catégorie — source de vérité de la règle §1.2.
    pub fn allowed_sizes(self) -> &'static [Option<PointSize>] {
        match self {
            Category::Cat1 => &NONE_ONLY,
            Category::Cat2 => &NONE_OR_GR,
            Category::Cat3 | Category::Cat4 => &GR_OR_PT,
            Category::Col | Category::Versants => &NONE_ONLY,
        }
    }

    /// `true` si la catégorie *exige* une taille (Cat3 / Cat4).
    pub fn requires_size(self) -> bool {
        !self.allowed_sizes().contains(&None)
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PointSize {
    pub const ALL: [PointSize; 2] = [PointSize::Gr, PointSize::Pt];

    pub fn as_str(self) -> &'static str {
        match self {
            PointSize::Gr => "Gr",
            PointSize::Pt => "Pt",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        match raw.trim() {
            "Gr" => Ok(PointSize::Gr),
            "Pt" => Ok(PointSize::Pt),
            other => Err(ValidationError::UnknownSize(other.to_string())),
        }
    }

    /// Tolère `None`, la chaîne vide et `"None"` (ce que renvoie un `<select>` vide).
    pub fn parse_optional(raw: Option<&str>) -> Result<Option<Self>, ValidationError> {
        match raw.map(str::trim) {
            None | Some("") | Some("None") | Some("null") => Ok(None),
            Some(other) => PointSize::parse(other).map(Some),
        }
    }

    pub fn label_fr(self) -> &'static str {
        match self {
            PointSize::Gr => "Gros",
            PointSize::Pt => "Petit",
        }
    }
}

impl fmt::Display for PointSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Erreurs de validation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("Catégorie inconnue : « {0} » (attendu : Cat1, Cat2, Cat3, Cat4, Col ou Versants)")]
    UnknownCategory(String),

    #[error("Taille inconnue : « {0} » (attendu : Gr, Pt, ou vide)")]
    UnknownSize(String),

    #[error("Un point {category} ne peut pas porter la taille {size} — {expected}")]
    SizeNotAllowed {
        category: Category,
        size: PointSize,
        expected: &'static str,
    },

    #[error("Un point {category} doit obligatoirement porter une taille (Gr ou Pt)")]
    SizeRequired { category: Category },

    #[error("Le nom du point ne peut pas être vide")]
    EmptyPointName,

    #[error("Le nom du projet ne peut pas être vide")]
    EmptyProjectName,

    #[error("Latitude hors bornes : {0} (attendu entre -90 et 90)")]
    LatitudeOutOfRange(f64),

    #[error("Longitude hors bornes : {0} (attendu entre -180 et 180)")]
    LongitudeOutOfRange(f64),

    #[error("Type de source inconnu : « {0} »")]
    UnknownSourceType(String),

    #[error("Rôle de série inconnu : « {0} » (attendu : Primary ou Backup)")]
    UnknownRole(String),
}

/// **Règle de validation critique du §1.2.**
///
/// | Catégorie | Taille autorisée |
/// |-----------|------------------|
/// | `Cat1`    | `None` uniquement |
/// | `Cat2`    | `Gr` ou `None` (jamais `Pt`) |
/// | `Cat3`    | `Gr` ou `Pt` (jamais vide) |
/// | `Cat4`    | `Gr` ou `Pt` (jamais vide) |
/// | `Col`     | `None` uniquement |
/// | `Versants`| `None` uniquement |
///
/// Cette fonction est le point de passage unique : `Point::from_new` l'appelle,
/// et la contrainte `CHECK` SQL en est la copie littérale.
pub fn validate_category_size(
    category: Category,
    size: Option<PointSize>,
) -> Result<(), ValidationError> {
    use Category::*;
    use PointSize::*;

    match (category, size) {
        // Cat1 : aucune taille.
        (Cat1, None) => Ok(()),
        (Cat1, Some(s)) => Err(ValidationError::SizeNotAllowed {
            category: Cat1,
            size: s,
            expected: "un point Cat1 n'a jamais de taille",
        }),

        // Cat2 : Gr ou rien, jamais Pt.
        (Cat2, None) | (Cat2, Some(Gr)) => Ok(()),
        (Cat2, Some(Pt)) => Err(ValidationError::SizeNotAllowed {
            category: Cat2,
            size: Pt,
            expected: "un point Cat2 accepte uniquement Gr ou aucune taille",
        }),

        // Cat3 / Cat4 : Gr ou Pt, jamais vide.
        (Cat3, Some(_)) | (Cat4, Some(_)) => Ok(()),
        (Cat3, None) => Err(ValidationError::SizeRequired { category: Cat3 }),
        (Cat4, None) => Err(ValidationError::SizeRequired { category: Cat4 }),

        // Un col ou un versant n'utilise pas la graduation de taille des cuvettes.
        (Col | Versants, None) => Ok(()),
        (Col | Versants, Some(s)) => Err(ValidationError::SizeNotAllowed {
            category,
            size: s,
            expected: "un col ou un versant n'a jamais de taille",
        }),
    }
}

// ---------------------------------------------------------------------------
// Projet
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_opened_at: DateTime<Utc>,
    /// Dérivé (non stocké) : rempli par les requêtes de listing, pratique pour l'UI.
    #[serde(default)]
    pub point_count: i64,
    /// Dérivé (non stocké en colonne) : projet d'exemple, voir `crate::demo`.
    #[serde(default)]
    pub is_demo: bool,
}

/// Instant courant, **tronqué à la seconde**.
///
/// La base ne stocke que des secondes epoch (voir `db.rs`). Si un objet gardé en
/// mémoire portait des nanosecondes, il ne serait pas égal à celui relu juste
/// après en base : `create_project()` renverrait `12:00:00,750` là où un
/// `list_projects()` immédiat renverrait `12:00:00`. On tronque donc à la source,
/// pour que « ce que l'application manipule » et « ce que la base contient »
/// soient toujours la même valeur.
pub fn now_utc() -> DateTime<Utc> {
    let now = Utc::now();
    now.with_nanosecond(0).unwrap_or(now)
}

impl Project {
    pub fn new(name: &str) -> Result<Self, ValidationError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ValidationError::EmptyProjectName);
        }
        let now = now_utc();
        Ok(Project {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            created_at: now,
            last_opened_at: now,
            point_count: 0,
            is_demo: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Point d'intérêt / station
// ---------------------------------------------------------------------------

/// Charge utile de création d'un point (ce que l'UI envoie).
///
/// `category` et `size` sont des `String` volontairement : on veut renvoyer une
/// erreur métier lisible (« Catégorie inconnue : … ») plutôt qu'un échec serde
/// opaque quand l'UI envoie n'importe quoi.
#[derive(Debug, Clone, Deserialize)]
pub struct NewPoint {
    pub project_id: String,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub altitude_m: Option<f64>,
    pub category: String,
    pub size: Option<String>,
    pub factor_sky: Option<String>,
    pub factor_wind: Option<String>,
    pub factor_slope: Option<String>,
    pub factor_snow_cover: Option<String>,
    pub factor_forest_cover: Option<String>,
    pub is_open_terrain: Option<bool>,
    pub notes: Option<String>,
    /// Identifiant de la station Infoclimat rattachée (ex. `"STATIC0479"`).
    /// Absent du formulaire des versions antérieures : `#[serde(default)]`.
    #[serde(default)]
    pub infoclimat_station_id: Option<String>,
    /// Identifiant / nom de la station ROMMA rattachée (v5), saisi librement.
    #[serde(default)]
    pub romma_station_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Point {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    /// Altitude en mètres. Optionnelle : souvent relevée après coup.
    pub altitude_m: Option<f64>,
    pub category: Category,
    pub size: Option<PointSize>,

    // --- Facteurs de site (format de saisie non figé : texte libre pour l'instant)
    /// Facteur ciel / dégagement.
    pub factor_sky: Option<String>,
    /// Facteur vent / exposition.
    pub factor_wind: Option<String>,
    /// Pente.
    pub factor_slope: Option<String>,
    /// Enneigement.
    pub factor_snow_cover: Option<String>,
    /// Couvert forestier.
    pub factor_forest_cover: Option<String>,
    /// Terrain ouvert avec écoulement d'air possible (ruisseau exutoire…) vs fermé.
    pub is_open_terrain: Option<bool>,

    pub notes: Option<String>,

    /// Identifiant de la station Infoclimat rattachée à ce point, s'il y en a une
    /// (ex. `"STATIC0479"` pour la Crête de la Grande Moucherolle).
    ///
    /// Saisi à la main : ce prompt ne construit **pas** de recherche de station
    /// par nom ou par proximité. C'est ce champ qui autorise le bouton
    /// « Récupérer les dernières données » sur la fiche du point.
    pub infoclimat_station_id: Option<String>,

    /// Station ROMMA rattachée à ce point, s'il y en a une (v5).
    ///
    /// Saisie **libre**, exactement comme l'identifiant Infoclimat : ROMMA
    /// n'expose pas d'annuaire interrogeable, et l'export ne porte le nom de la
    /// station que dans son *nom de fichier*
    /// (`archives_Lans - Les Allières_31-05-2017_19-08-2026.csv`). On y met donc
    /// ce que l'utilisateur reconnaît — « Lans - Les Allières » par exemple.
    ///
    /// Contrairement à Infoclimat, ce champ ne débloque **aucun appel réseau** :
    /// il n'existe pas d'API ROMMA ici, uniquement l'import de fichier. Il sert à
    /// la traçabilité et à retrouver la bonne série sur la fiche du point.
    #[serde(default)]
    pub romma_station_id: Option<String>,

    pub created_at: DateTime<Utc>,

    /// Dérivé (non stocké) : nombre de séries rattachées, rempli au listing.
    #[serde(default)]
    pub series_count: i64,
}

impl Point {
    /// Construit un point **valide** ou échoue. Aucun autre chemin ne doit
    /// permettre de fabriquer un `Point` incohérent.
    pub fn from_new(input: NewPoint) -> Result<Self, ValidationError> {
        Self::from_parts(Uuid::new_v4().to_string(), now_utc(), input)
    }

    /// Reconstruit un point en **conservant son identité** (id et date de
    /// création). C'est le chemin de la modification.
    ///
    /// Toutes les validations vivent ici, et uniquement ici : créer et modifier
    /// passent par la même porte, il n'y a donc aucun risque qu'une règle soit
    /// appliquée à la création mais oubliée à la mise à jour.
    pub fn from_parts(
        id: String,
        created_at: DateTime<Utc>,
        input: NewPoint,
    ) -> Result<Self, ValidationError> {
        let name = input.name.trim();
        if name.is_empty() {
            return Err(ValidationError::EmptyPointName);
        }
        if !(-90.0..=90.0).contains(&input.latitude) || input.latitude.is_nan() {
            return Err(ValidationError::LatitudeOutOfRange(input.latitude));
        }
        if !(-180.0..=180.0).contains(&input.longitude) || input.longitude.is_nan() {
            return Err(ValidationError::LongitudeOutOfRange(input.longitude));
        }

        let category = Category::parse(&input.category)?;
        let size = PointSize::parse_optional(input.size.as_deref())?;
        validate_category_size(category, size)?;

        Ok(Point {
            id,
            project_id: input.project_id,
            name: name.to_string(),
            latitude: input.latitude,
            longitude: input.longitude,
            altitude_m: input.altitude_m,
            category,
            size,
            factor_sky: normalize_opt(input.factor_sky),
            factor_wind: normalize_opt(input.factor_wind),
            factor_slope: normalize_opt(input.factor_slope),
            factor_snow_cover: normalize_opt(input.factor_snow_cover),
            factor_forest_cover: normalize_opt(input.factor_forest_cover),
            is_open_terrain: input.is_open_terrain,
            notes: normalize_opt(input.notes),
            infoclimat_station_id: normalize_opt(input.infoclimat_station_id),
            romma_station_id: normalize_opt(input.romma_station_id),
            created_at,
            series_count: 0,
        })
    }

    /// Re-validation défensive (appelée avant tout `UPDATE`).
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_category_size(self.category, self.size)
    }
}

fn normalize_opt(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Séries de mesures
// ---------------------------------------------------------------------------

/// Origine d'une série. Les variantes non implémentées sont déjà présentes pour
/// que le schéma SQLite n'ait pas à bouger quand on branchera les API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceType {
    /// Fichier exporté par ElitechLog (TXT/CSV).
    ElitechImport,
    /// **Lecture USB directe du RC-5** — implémentée (voir [`crate::elitech_serial`]).
    ///
    /// Même appareil et même nature de donnée qu'[`SourceType::ElitechImport`],
    /// mais **une autre voie** : le port série au lieu d'un fichier produit par
    /// ElitechLog. La distinction est conservée en base parce qu'elle répond à
    /// une question qui se pose vraiment quand on doute d'une série — « est-ce
    /// passé par ElitechLog, ou lu en direct ? ».
    ///
    /// Les deux voies produisent des mesures **identiques** : c'est vérifié
    /// mesure par mesure par `tests/rc5_direct.rs`, sur le même logger physique.
    /// L'horodatage y est donc naïf de la même façon, et `clock_offset_seconds`
    /// reste tout aussi nécessaire.
    ElitechDirect,
    /// **API Infoclimat** — implémentée (voir [`crate::infoclimat`]).
    ///
    /// C'est le type de *toutes* les données venues de l'API, quelle que soit la
    /// station : les Vantage Pro 2 du réseau StatIC **comme** la Crête de la
    /// Grande Moucherolle (`STATIC0479`). La Moucherolle est un Dragino LoRa,
    /// mais elle republie sur Infoclimat exactement au même format que les VP2 —
    /// il n'y a donc qu'un seul parseur, et pas de « format Dragino » distinct.
    InfoclimatApi,
    /// Accès **direct** à la plateforme Orange Live Objects — réservé, jamais produit.
    ///
    /// ⚠️ Ne pas confondre avec la Moucherolle : ses mesures arrivent par
    /// [`SourceType::InfoclimatApi`], pas par ici. Cette variante n'existe que
    /// pour le jour où l'on interrogerait Live Objects sans passer par
    /// Infoclimat (les trames LoRa brutes, republiées par Infoclimat sous
    /// `"source": "liveobjects"`). Aucun code ne l'écrit aujourd'hui.
    ///
    /// Le nom `DraginoApi` est conservé tel quel : la chaîne est gravée dans la
    /// contrainte `CHECK` de `logger_series`, et la renommer imposerait une
    /// reconstruction complète de la table pour un gain purement cosmétique.
    /// L'UI n'affiche jamais l'identifiant brut mais [`SourceType::label_fr`].
    DraginoApi,
    /// **Import d'une archive ROMMA** (`archives_*.csv`) — implémenté (voir
    /// [`crate::romma_import`]).
    ///
    /// ROMMA (Réseau d'Observation Météo du Massif Alpin) publie, pour chacune
    /// de ses stations, un export CSV horodaté en **epoch Unix UTC**. C'est donc
    /// une source nativement UTC, au même titre qu'Infoclimat, et pour la même
    /// raison : la colonne `timestamp` ne dépend d'aucun fuseau. Les colonnes
    /// `Date` / `heure` du même fichier, elles, sont en heure locale — elles ne
    /// sont **jamais** utilisées pour dater une mesure (cf. `romma_import`).
    RommaImport,
    /// Saisie / collage manuel (mode de secours de l'import).
    ///
    /// ⚠️ Malgré son nom, c'est bien un **fichier** qui arrive par là : le mode
    /// « colonnes mappées » de l'import (voir [`crate::manual_import`]), où
    /// l'utilisateur désigne lui-même les colonnes au lieu de laisser
    /// l'auto-détection deviner. La donnée reste une lecture de logger.
    /// Pour une valeur tapée à la main, c'est [`SourceType::ManualEntry`].
    Manual,
    /// **Valeur saisie à la main**, sans fichier ni appareil (voir
    /// [`crate::manual_entry`]).
    ///
    /// Relevé manuscrit sur site, recopié dans l'application : « moyenne 6 h–9 h
    /// = −4,2 °C le 14 janvier ». Ce n'est **pas** une mesure au sens des autres
    /// sources — c'est une estimation ponctuelle, portant sur une plage horaire,
    /// et non un échantillon instantané d'une série continue.
    ///
    /// D'où une variante à part entière plutôt qu'un simple libellé : les
    /// statistiques, l'export et l'agent doivent pouvoir répondre à « cette
    /// moyenne repose-t-elle sur des mesures ou sur des relevés à la main ? ».
    /// C'est ce que tranche [`SourceType::is_manual_entry`].
    ManualEntry,
}

impl SourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceType::ElitechImport => "ElitechImport",
            SourceType::ElitechDirect => "ElitechDirect",
            SourceType::InfoclimatApi => "InfoclimatApi",
            SourceType::DraginoApi => "DraginoApi",
            SourceType::RommaImport => "RommaImport",
            SourceType::Manual => "Manual",
            SourceType::ManualEntry => "ManualEntry",
        }
    }

    /// Libellé affiché par l'UI. C'est la seule forme montrée à l'utilisateur :
    /// l'identifiant brut (`DraginoApi`…) ne doit jamais apparaître à l'écran.
    pub fn label_fr(self) -> &'static str {
        match self {
            SourceType::ElitechImport => "Fichier Elitech",
            SourceType::ElitechDirect => "Elitech (USB direct)",
            SourceType::InfoclimatApi => "API Infoclimat",
            SourceType::DraginoApi => "Orange Live Objects (réservé)",
            SourceType::RommaImport => "Fichier ROMMA (archives)",
            SourceType::Manual => "Fichier à colonnes mappées",
            SourceType::ManualEntry => "Saisie manuelle sur site",
        }
    }

    /// `true` si les horodatages de cette source sont de l'UTC vrai dès l'origine.
    ///
    /// Pour Infoclimat, `dh_utc` est de l'UTC réel ; pour ROMMA, la colonne
    /// `timestamp` est un epoch Unix. Dans les deux cas la détection d'ordre
    /// jour/mois bâtie pour les fichiers Elitech n'a aucun sens et doit être
    /// court-circuitée (`clock_offset_seconds = 0`, `raw_timestamp_was_utc = true`).
    ///
    /// ⚠️ `ElitechImport` n'en fait **pas** partie, y compris pour l'export xlsx
    /// réel : son champ `Temps` est bien en ISO (donc sans ambiguïté jour/mois),
    /// mais il reste **naïf** — le RC-5 écrit l'heure de son horloge interne,
    /// sans fuseau. C'est `clock_offset_seconds` qui la ramène à l'UTC, et ce
    /// mécanisme reste indispensable pour cette source.
    pub fn is_utc_native(self) -> bool {
        matches!(
            self,
            SourceType::InfoclimatApi | SourceType::DraginoApi | SourceType::RommaImport
        )
    }

    /// `true` si la donnée a été **tapée à la main**, et non mesurée par un
    /// appareil puis relevée.
    ///
    /// C'est la distinction qui compte pour interpréter une statistique : une
    /// moyenne calculée sur 4 320 échantillons d'un logger et une moyenne
    /// recopiée d'un carnet ne se valent pas, même si elles s'affichent dans la
    /// même colonne. Tout ce qui restitue des chiffres à l'utilisateur — export,
    /// couverture statistique, contexte de l'agent — s'en sert pour le dire.
    pub fn is_manual_entry(self) -> bool {
        matches!(self, SourceType::ManualEntry)
    }

    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        match raw.trim() {
            "ElitechImport" => Ok(SourceType::ElitechImport),
            "ElitechDirect" => Ok(SourceType::ElitechDirect),
            "InfoclimatApi" => Ok(SourceType::InfoclimatApi),
            "DraginoApi" => Ok(SourceType::DraginoApi),
            "RommaImport" => Ok(SourceType::RommaImport),
            "Manual" => Ok(SourceType::Manual),
            "ManualEntry" => Ok(SourceType::ManualEntry),
            other => Err(ValidationError::UnknownSourceType(other.to_string())),
        }
    }
}

/// Rôle de la série. **Toujours assigné explicitement par l'utilisateur**, jamais
/// déduit : deux loggers sur un même point restent deux séries distinctes, on ne
/// fusionne rien automatiquement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeriesRole {
    Primary,
    Backup,
}

impl SeriesRole {
    pub fn as_str(self) -> &'static str {
        match self {
            SeriesRole::Primary => "Primary",
            SeriesRole::Backup => "Backup",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        match raw.trim() {
            "Primary" => Ok(SeriesRole::Primary),
            "Backup" => Ok(SeriesRole::Backup),
            other => Err(ValidationError::UnknownRole(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggerSeries {
    pub id: String,
    pub point_id: String,
    pub source_type: SourceType,
    pub role: SeriesRole,
    /// Numéro de série du logger physique, si on a pu le lire dans l'export.
    pub logger_serial: Option<String>,
    /// Instant de l'**import**, pas de la mesure.
    pub imported_at: DateTime<Utc>,
    /// Nom / chemin du fichier source, pour la traçabilité.
    pub raw_file_reference: Option<String>,

    /// Nombre de secondes **ajoutées** à l'horodatage brut pour obtenir l'UTC vrai.
    ///
    /// Il agrège deux corrections :
    /// * la conversion de fuseau (logger réglé en UTC+1 → `-3600`) ;
    /// * la dérive d'horloge constatée (logger en avance de 45 s → `-45`).
    ///
    /// Invariant : `timestamp_utc == raw_timestamp + clock_offset_seconds`.
    pub clock_offset_seconds: i64,

    /// La source fournissait-elle déjà de l'UTC ? Purement documentaire (debug),
    /// la correction effective est portée par `clock_offset_seconds`.
    pub raw_timestamp_was_utc: bool,

    /// Libellé libre optionnel (« logger fond de doline », « secours arbre »).
    pub label: Option<String>,
    /// Cadence attendue de la série, en secondes (600 pour du 10 minutes).
    ///
    /// Lue dans l'en-tête pour un fichier Elitech, **déduite de l'écart le plus
    /// fréquent entre deux mesures** pour une série Infoclimat (l'API ne l'annonce
    /// pas). Sert à repérer les trous : un écart nettement supérieur à cette
    /// valeur signale une coupure de la station.
    pub sample_interval_seconds: Option<i64>,

    /// Mention de licence renvoyée par la source, telle quelle (v3).
    ///
    /// Ex. `"NON-COMMERCIAL ONLY: CC BY NC"` pour la Moucherolle. Stockée par
    /// série et non globalement : deux stations Infoclimat peuvent ne pas porter
    /// la même licence. Elle conditionne l'attribution affichée et — le jour où
    /// l'app sera distribuée — ce qu'on a le droit d'en faire.
    #[serde(default)]
    pub data_license: Option<String>,

    /// Instant du dernier appel API réussi pour cette série (v3).
    ///
    /// C'est ce que compare le garde-fou de fréquence : un rafraîchissement
    /// automatique est refusé tant que l'intervalle minimal fixé dans les
    /// Paramètres n'est pas écoulé. `None` pour toute série qui ne vient pas
    /// d'une API.
    #[serde(default)]
    pub last_api_fetch_at: Option<DateTime<Utc>>,

    /// Identifiant de la station interrogée (`"STATIC0479"`), pour une série d'API.
    #[serde(default)]
    pub station_id: Option<String>,

    /// Ordre jour/mois appliqué à la lecture des dates de cette série.
    ///
    /// Détecté sur le fichier à l'import (voir [`crate::parsing::DateOrderScanner`]),
    /// ou hérité du repli si le fichier ne prouvait rien — auquel cas
    /// `date_format_uncertain` est levé.
    pub date_order: DateOrder,

    /// `true` : le fichier ne permettait pas de trancher l'ordre jour/mois (ou se
    /// contredisait). Les dates sont peut-être décalées de plusieurs mois.
    ///
    /// L'UI l'affiche en bandeau et propose de confirmer ou d'inverser ; la
    /// correction se fait **sans réimport** (voir `Db::swap_series_date_order`).
    #[serde(default)]
    pub date_format_uncertain: bool,

    /// Condensat du lot de mesures qui a créé cette série (v4).
    ///
    /// Sert uniquement à reconnaître un ré-import : avant de créer une série, on
    /// cherche si le même point en porte déjà une de même empreinte. Voir
    /// [`crate::fingerprint`] pour la recette et pour la raison qui fait qu'un
    /// complément incrémental d'API ne peut pas la déclencher.
    ///
    /// `None` sur toute série antérieure à la v4 : on ne peut pas recalculer
    /// l'empreinte d'un fichier qu'on n'a plus. Ces séries ne participent donc
    /// pas à la détection, ni comme suspect ni comme référence.
    #[serde(default)]
    pub content_fingerprint: Option<String>,

    /// Dérivé (non stocké) : rempli au listing.
    #[serde(default)]
    pub measurement_count: i64,
}

// ---------------------------------------------------------------------------
// Mesure
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    /// Auto-incrément SQLite ; vaut 0 tant que la mesure n'est pas insérée.
    pub id: i64,
    pub series_id: Arc<str>,

    /// **Toujours de l'UTC absolu**, déjà corrigé par `clock_offset_seconds`.
    pub timestamp_utc: DateTime<Utc>,

    /// Horodatage brut du fichier, *réinterprété tel quel comme s'il était UTC*
    /// (aucune conversion appliquée). C'est le pivot qui permet de recalculer
    /// `timestamp_utc` si l'utilisateur affine le décalage après coup —
    /// voir `Db::recalibrate_series`.
    pub raw_timestamp: DateTime<Utc>,

    pub temperature_celsius: f64,
    /// Le RC-5 peut logger l'humidité selon le modèle ; souvent absent.
    pub humidity_percent: Option<f64>,

    /// Qualité du signal radio, en dBm (v3).
    ///
    /// Renseigné par les stations qui publient un champ `complements` avec une
    /// clé `rssi` — la Moucherolle en fournit un pour chaque mesure (observé
    /// entre −117 et −73 dBm). `None` partout ailleurs.
    #[serde(default)]
    pub rssi_dbm: Option<f64>,

    /// Valeur de batterie **brute, non convertie** (v3).
    ///
    /// ⚠️ L'unité n'est pas déterminée. Sur l'échantillon réel de la Moucherolle
    /// (7905 mesures), la clé `batt` vaut le plus souvent entre 39,7 et 41,1,
    /// mais descend à 6,48 et monte à 72 : ce n'est ni une tension Dragino
    /// plausible (3,0–4,2 V), ni un pourcentage stable, ni un millivoltage. On
    /// stocke donc **la valeur telle quelle, sans conversion ni mise à l'échelle**,
    /// et l'UI l'affiche sans unité. À interpréter avec prudence : ne pas en
    /// tirer d'alerte « batterie faible » tant que l'échelle n'est pas confirmée
    /// auprès du propriétaire de la station.
    #[serde(default)]
    pub battery: Option<f64>,

    // -----------------------------------------------------------------------
    // Champs météo complémentaires (v5, arrivés avec la source ROMMA)
    // -----------------------------------------------------------------------
    //
    // Tous `Option<f64>`, tous `NULL` pour les sources qui ne les publient pas —
    // c'est-à-dire, aujourd'hui, **toutes sauf ROMMA**. Ils sont stockés et
    // relus, mais aucune statistique n'est encore calculée dessus : voir
    // `stats::weather_coverage`, qui se contente de dire combien de mesures
    // portent chaque champ.
    //
    // Les unités ne sont pas devinées : elles sont lues dans la ligne d'unités du
    // fichier ROMMA (`degreC;hPa;mm;km/h;W/m2`) et vérifiées contre les ordres de
    // grandeur observés. Voir l'en-tête de [`crate::romma_import`].
    /// Pression atmosphérique, en hectopascals.
    #[serde(default)]
    pub pressure_hpa: Option<f64>,

    /// Vent moyen, en **km/h** (unité annoncée par le fichier ROMMA et cohérente
    /// avec les valeurs observées : maximum 105 km/h sur neuf ans).
    #[serde(default)]
    pub wind_avg_kmh: Option<f64>,

    /// Rafale maximale sur l'intervalle, en km/h.
    #[serde(default)]
    pub wind_gust_kmh: Option<f64>,

    /// Direction du vent moyen, en **degrés** (0 = nord, 90 = est).
    ///
    /// ⚠️ ROMMA ne publie pas de degrés mais une **rose à 16 points en français**
    /// (`N`, `NNE`, … `NNO`). La conversion est faite à l'import par
    /// `romma_import::direction_en_degres`, qui documente la table. La valeur
    /// stockée est donc un secteur de 22,5°, pas une mesure fine.
    #[serde(default)]
    pub wind_direction_degrees: Option<f64>,

    /// Précipitations sur l'intervalle, en millimètres.
    #[serde(default)]
    pub rain_mm: Option<f64>,

    /// Rayonnement solaire, en **W/m²** (unité annoncée par le fichier ROMMA).
    ///
    /// Le nom reste sans suffixe d'unité, contrairement aux champs de vent :
    /// l'unité du rayonnement peut varier d'un réseau à l'autre (W/m², J/cm²,
    /// kWh/m²) et figer `_wm2` dans le nom obligerait à renommer la colonne le
    /// jour où une autre source en publierait dans une autre unité.
    #[serde(default)]
    pub solar_radiation: Option<f64>,
}

impl Measurement {
    /// Mesure « nue » : un instant, une température, et **rien d'autre**.
    ///
    /// Tous les champs optionnels valent `None`. C'est le constructeur à
    /// privilégier partout où la source ne publie que de la température : il
    /// évite qu'une nouvelle colonne facultative (il y en a eu six d'un coup en
    /// v5) oblige à retoucher chaque littéral de structure du projet.
    ///
    /// `id` vaut 0 : la mesure n'est pas encore insérée.
    pub fn new(
        series_id: impl Into<Arc<str>>,
        timestamp_utc: DateTime<Utc>,
        raw_timestamp: DateTime<Utc>,
        temperature_celsius: f64,
    ) -> Self {
        Measurement {
            id: 0,
            series_id: series_id.into(),
            timestamp_utc,
            raw_timestamp,
            temperature_celsius,
            humidity_percent: None,
            rssi_dbm: None,
            battery: None,
            pressure_hpa: None,
            wind_avg_kmh: None,
            wind_gust_kmh: None,
            wind_direction_degrees: None,
            rain_mm: None,
            solar_radiation: None,
        }
    }

    /// Variante d'un instant **déjà en UTC vrai** : `raw_timestamp == timestamp_utc`.
    ///
    /// C'est l'invariant du §1.4 vérifié au cas trivial, celui des sources
    /// nativement UTC (Infoclimat, ROMMA). Passer par ce constructeur plutôt que
    /// d'écrire deux fois le même instant rend l'intention explicite.
    pub fn new_utc(
        series_id: impl Into<Arc<str>>,
        timestamp_utc: DateTime<Utc>,
        temperature_celsius: f64,
    ) -> Self {
        Measurement::new(series_id, timestamp_utc, timestamp_utc, temperature_celsius)
    }
}

/// Applique la correction d'horloge : `raw + offset`.
///
/// `raw` est l'horodatage tel qu'écrit par le logger, réinterprété en UTC sans
/// conversion. Passer par cette fonction (et pas par une addition ad hoc)
/// garantit que l'invariant du §1.4 est respecté partout.
pub fn apply_clock_offset(raw: DateTime<Utc>, clock_offset_seconds: i64) -> DateTime<Utc> {
    raw + Duration::seconds(clock_offset_seconds)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Tableau de référence du §1.2, recopié littéralement.
    /// `true` = la combinaison doit être acceptée.
    const MATRIX: [(Category, Option<PointSize>, bool); 18] = [
        (Category::Cat1, None, true),
        (Category::Cat1, Some(PointSize::Gr), false),
        (Category::Cat1, Some(PointSize::Pt), false),
        (Category::Cat2, None, true),
        (Category::Cat2, Some(PointSize::Gr), true),
        (Category::Cat2, Some(PointSize::Pt), false),
        (Category::Cat3, None, false),
        (Category::Cat3, Some(PointSize::Gr), true),
        (Category::Cat3, Some(PointSize::Pt), true),
        (Category::Cat4, None, false),
        (Category::Cat4, Some(PointSize::Gr), true),
        (Category::Cat4, Some(PointSize::Pt), true),
        (Category::Col, None, true),
        (Category::Col, Some(PointSize::Gr), false),
        (Category::Col, Some(PointSize::Pt), false),
        (Category::Versants, None, true),
        (Category::Versants, Some(PointSize::Gr), false),
        (Category::Versants, Some(PointSize::Pt), false),
    ];

    fn expected_for(category: Category, size: Option<PointSize>) -> bool {
        MATRIX
            .iter()
            .find(|(c, s, _)| *c == category && *s == size)
            .map(|(_, _, ok)| *ok)
            .unwrap_or_else(|| panic!("combinaison non couverte : {category:?} / {size:?}"))
    }

    /// Les 18 cas (6 catégories × 3 valeurs de taille) sont générés
    /// exhaustivement, puis confrontés au tableau de référence. Si un jour on
    /// ajoute une catégorie ou une taille, ce test échoue tout seul.
    #[test]
    fn matrice_categorie_taille_exhaustive() {
        let sizes: [Option<PointSize>; 3] = [None, Some(PointSize::Gr), Some(PointSize::Pt)];
        let mut checked = 0;

        for category in Category::ALL {
            for size in sizes {
                let expected = expected_for(category, size);
                let actual = validate_category_size(category, size).is_ok();
                assert_eq!(
                    actual, expected,
                    "{category} + {size:?} : attendu valide={expected}, obtenu={actual}"
                );
                // `allowed_sizes()` doit rester cohérent avec la validation.
                assert_eq!(
                    category.allowed_sizes().contains(&size),
                    expected,
                    "{category}.allowed_sizes() incohérent avec validate_category_size pour {size:?}"
                );
                checked += 1;
            }
        }

        assert_eq!(checked, 18, "les 18 combinaisons doivent être testées");
    }

    // --- Les 12 cas, un par un, pour un diagnostic immédiat en cas d'échec ---

    #[test]
    fn cat1_sans_taille_ok() {
        assert!(validate_category_size(Category::Cat1, None).is_ok());
    }

    #[test]
    fn cat1_gr_rejete() {
        assert!(matches!(
            validate_category_size(Category::Cat1, Some(PointSize::Gr)),
            Err(ValidationError::SizeNotAllowed { .. })
        ));
    }

    #[test]
    fn cat1_pt_rejete() {
        assert!(matches!(
            validate_category_size(Category::Cat1, Some(PointSize::Pt)),
            Err(ValidationError::SizeNotAllowed { .. })
        ));
    }

    #[test]
    fn cat2_sans_taille_ok() {
        assert!(validate_category_size(Category::Cat2, None).is_ok());
    }

    #[test]
    fn cat2_gr_ok() {
        assert!(validate_category_size(Category::Cat2, Some(PointSize::Gr)).is_ok());
    }

    #[test]
    fn cat2_pt_rejete() {
        assert!(matches!(
            validate_category_size(Category::Cat2, Some(PointSize::Pt)),
            Err(ValidationError::SizeNotAllowed { .. })
        ));
    }

    #[test]
    fn cat3_sans_taille_rejete() {
        assert!(matches!(
            validate_category_size(Category::Cat3, None),
            Err(ValidationError::SizeRequired { .. })
        ));
    }

    #[test]
    fn cat3_gr_ok() {
        assert!(validate_category_size(Category::Cat3, Some(PointSize::Gr)).is_ok());
    }

    #[test]
    fn cat3_pt_ok() {
        assert!(validate_category_size(Category::Cat3, Some(PointSize::Pt)).is_ok());
    }

    #[test]
    fn cat4_sans_taille_rejete() {
        assert!(matches!(
            validate_category_size(Category::Cat4, None),
            Err(ValidationError::SizeRequired { .. })
        ));
    }

    #[test]
    fn cat4_gr_ok() {
        assert!(validate_category_size(Category::Cat4, Some(PointSize::Gr)).is_ok());
    }

    #[test]
    fn cat4_pt_ok() {
        assert!(validate_category_size(Category::Cat4, Some(PointSize::Pt)).is_ok());
    }

    #[test]
    fn col_et_versants_nont_pas_de_taille() {
        for category in [Category::Col, Category::Versants] {
            assert!(validate_category_size(category, None).is_ok());
            assert!(matches!(
                validate_category_size(category, Some(PointSize::Gr)),
                Err(ValidationError::SizeNotAllowed { .. })
            ));
        }
    }

    // --- La règle doit aussi tenir au niveau du constructeur ---

    fn new_point(category: &str, size: Option<&str>) -> NewPoint {
        NewPoint {
            project_id: "p".into(),
            name: "Darbounouse".into(),
            latitude: 44.98,
            longitude: 5.42,
            altitude_m: Some(1150.0),
            category: category.into(),
            size: size.map(|s| s.to_string()),
            factor_sky: None,
            factor_wind: None,
            factor_slope: None,
            factor_snow_cover: None,
            factor_forest_cover: None,
            is_open_terrain: Some(false),
            notes: None,
            infoclimat_station_id: None,
            romma_station_id: None,
        }
    }

    #[test]
    fn constructeur_applique_la_regle() {
        assert!(Point::from_new(new_point("Cat3", Some("Gr"))).is_ok());
        assert!(Point::from_new(new_point("Cat3", None)).is_err());
        assert!(Point::from_new(new_point("Cat1", Some("Pt"))).is_err());
        // Un <select> vide renvoie "" : doit être compris comme « pas de taille ».
        assert!(Point::from_new(new_point("Cat1", Some(""))).is_ok());
        assert!(Point::from_new(new_point("Cat2", Some("None"))).is_ok());
    }

    #[test]
    fn constructeur_rejette_les_champs_aberrants() {
        let mut p = new_point("Cat3", Some("Gr"));
        p.name = "   ".into();
        assert!(matches!(
            Point::from_new(p),
            Err(ValidationError::EmptyPointName)
        ));

        let mut p = new_point("Cat3", Some("Gr"));
        p.latitude = 91.0;
        assert!(matches!(
            Point::from_new(p),
            Err(ValidationError::LatitudeOutOfRange(_))
        ));

        let mut p = new_point("Cat3", Some("Gr"));
        p.longitude = -181.0;
        assert!(matches!(
            Point::from_new(p),
            Err(ValidationError::LongitudeOutOfRange(_))
        ));

        assert!(Point::from_new(new_point("Cat5", None)).is_err());
        assert!(Point::from_new(new_point("Cat3", Some("XL"))).is_err());
    }

    #[test]
    fn requires_size_coherent() {
        assert!(!Category::Cat1.requires_size());
        assert!(!Category::Cat2.requires_size());
        assert!(Category::Cat3.requires_size());
        assert!(Category::Cat4.requires_size());
        assert!(!Category::Col.requires_size());
        assert!(!Category::Versants.requires_size());
    }

    #[test]
    fn correction_horloge() {
        let raw: DateTime<Utc> = "2024-02-10T07:00:00Z".parse().unwrap();
        // Logger réglé en UTC+1 → il faut retrancher une heure.
        assert_eq!(
            apply_clock_offset(raw, -3600),
            "2024-02-10T06:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        // Logger en retard de 90 s → il faut ajouter 90 s.
        assert_eq!(
            apply_clock_offset(raw, 90),
            "2024-02-10T07:01:30Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(apply_clock_offset(raw, 0), raw);
    }

    #[test]
    fn round_trip_des_enums() {
        for c in Category::ALL {
            assert_eq!(Category::parse(c.as_str()).unwrap(), c);
        }
        for s in PointSize::ALL {
            assert_eq!(PointSize::parse(s.as_str()).unwrap(), s);
        }
        for s in [
            SourceType::ElitechImport,
            SourceType::ElitechDirect,
            SourceType::InfoclimatApi,
            SourceType::DraginoApi,
            SourceType::RommaImport,
            SourceType::Manual,
            SourceType::ManualEntry,
        ] {
            assert_eq!(SourceType::parse(s.as_str()).unwrap(), s);
        }
        for r in [SeriesRole::Primary, SeriesRole::Backup] {
            assert_eq!(SeriesRole::parse(r.as_str()).unwrap(), r);
        }
    }
}

//! Persistance SQLite.
//!
//! ## Choix de stockage
//!
//! * **SQLite via `rusqlite` (feature `bundled`)**. Pas de plugin `tauri-plugin-sql` :
//!   on veut que toute la logique métier reste en Rust typé, testable sans lancer
//!   l'application. `bundled` compile SQLite depuis les sources → zéro DLL à
//!   installer sur les postes de la famille.
//!
//! * **Tous les instants sont des entiers : secondes depuis l'epoch Unix, en UTC.**
//!   Aucune date n'est stockée en texte, aucune n'a de fuseau attaché. C'est la
//!   traduction en base de la contrainte n°1 du projet : un `INTEGER` ne peut pas
//!   être « de l'heure locale par accident ». Conversion centralisée dans
//!   [`secs`] / [`ts_from_secs`].
//!
//! * **La règle catégorie/taille est dupliquée en contrainte `CHECK`.** Le Rust
//!   valide (message clair), SQLite garantit (aucune ligne invalide ne peut
//!   exister, même écrite par un client tiers). Les deux doivent être modifiées
//!   ensemble — voir `model::validate_category_size`.
//!
//! * **`measurements.id` est un `INTEGER PRIMARY KEY`** (alias de `rowid`) : le
//!   volume peut monter à plusieurs centaines de milliers de lignes par point.
//!
//! * **Pas de fusion de séries, mais plus de doublons *dans* une série.**
//!   Réimporter deux fois le même fichier crée toujours deux séries distinctes :
//!   on ne fusionne jamais rien automatiquement (§1.3). En revanche, depuis la v3,
//!   `UNIQUE(series_id, timestamp_utc)` interdit qu'une même série porte deux
//!   mesures au même instant. C'est devenu indispensable avec l'API Infoclimat :
//!   c'est une source *réinterrogeable*, dont deux appels successifs se recouvrent
//!   presque toujours. Voir [`Db::append_measurements`] pour la stratégie de
//!   conflit retenue.

use std::borrow::Borrow;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use rusqlite::types::{ToSqlOutput, Value, ValueRef};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use crate::error::{AppError, AppResult};
use crate::model::{
    Category, LoggerSeries, Measurement, Point, PointSize, Project, SeriesRole, SourceType,
};
use crate::parsing::{swap_day_month_datetime, DateOrder};
use crate::stats::{DayAgg, TempSample};
use serde::{Deserialize, Serialize};

/// Version du schéma stockée dans `PRAGMA user_version`.
/// À incrémenter en ajoutant une étape dans [`Db::migrate`].
pub const SCHEMA_VERSION: i64 = 11;

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS projects (
    id              TEXT    PRIMARY KEY,
    name            TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,   -- epoch secondes UTC
    last_opened_at  INTEGER NOT NULL    -- epoch secondes UTC
);

CREATE TABLE IF NOT EXISTS points (
    id                   TEXT    PRIMARY KEY,
    project_id           TEXT    NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name                 TEXT    NOT NULL,
    latitude             REAL    NOT NULL,
    longitude            REAL    NOT NULL,
    altitude_m           REAL,
    category             TEXT    NOT NULL,
    size                 TEXT,
    factor_sky           TEXT,
    factor_wind          TEXT,
    factor_slope         TEXT,
    factor_snow_cover    TEXT,
    factor_forest_cover  TEXT,
    is_open_terrain      INTEGER,        -- 0/1/NULL
    notes                TEXT,
    created_at           INTEGER NOT NULL,

    -- Copie littérale de model::validate_category_size (§1.2).
    -- Toute modification ici doit être répercutée côté Rust, et inversement.
    CHECK (category IN ('Cat1','Cat2','Cat3','Cat4')),
    CHECK (size IS NULL OR size IN ('Gr','Pt')),
    CHECK (
           (category = 'Cat1' AND size IS NULL)
        OR (category = 'Cat2' AND (size IS NULL OR size = 'Gr'))
        -- `size IN ('Gr','Pt')` vaut NULL — et non FALSE — quand `size` est NULL,
        -- or une contrainte CHECK qui s'évalue à NULL est considérée comme
        -- satisfaite par SQLite (logique ternaire). Sans le `size IS NOT NULL`
        -- explicite ci-dessous, un point Cat3/Cat4 **sans taille** passait donc en
        -- base alors que c'est précisément ce que la règle interdit.
        OR (category IN ('Cat3','Cat4') AND size IS NOT NULL AND size IN ('Gr','Pt'))
    )
);
CREATE INDEX IF NOT EXISTS idx_points_project ON points(project_id);

CREATE TABLE IF NOT EXISTS logger_series (
    id                      TEXT    PRIMARY KEY,
    point_id                TEXT    NOT NULL REFERENCES points(id) ON DELETE CASCADE,
    source_type             TEXT    NOT NULL,
    role                    TEXT    NOT NULL,
    logger_serial           TEXT,
    imported_at             INTEGER NOT NULL,  -- instant de l'import, pas de la mesure
    raw_file_reference      TEXT,
    clock_offset_seconds    INTEGER NOT NULL DEFAULT 0,
    raw_timestamp_was_utc   INTEGER NOT NULL DEFAULT 0,
    label                   TEXT,
    sample_interval_seconds INTEGER,

    CHECK (role IN ('Primary','Backup')),
    CHECK (source_type IN ('ElitechImport','ElitechDirect','InfoclimatApi','DraginoApi','Manual'))
);
CREATE INDEX IF NOT EXISTS idx_series_point ON logger_series(point_id);

CREATE TABLE IF NOT EXISTS measurements (
    id                  INTEGER PRIMARY KEY,
    series_id           TEXT    NOT NULL REFERENCES logger_series(id) ON DELETE CASCADE,
    -- UTC absolu, DÉJÀ corrigé : timestamp_utc = raw_timestamp + clock_offset_seconds
    timestamp_utc       INTEGER NOT NULL,
    -- Horodatage brut du fichier, réinterprété en UTC sans conversion.
    -- Conservé pour pouvoir recalculer timestamp_utc si le décalage est affiné.
    raw_timestamp       INTEGER NOT NULL,
    temperature_celsius REAL    NOT NULL,
    humidity_percent    REAL
);
CREATE INDEX IF NOT EXISTS idx_measurements_series_ts
    ON measurements(series_id, timestamp_utc);
"#;

/// Migration v2 — traçabilité de l'ordre jour/mois appliqué à l'import.
///
/// `SCHEMA_V1` n'est **pas** modifié : sur une base neuve, `migrate` rejoue v1
/// puis v2. Déclarer les colonnes des deux côtés ferait échouer l'`ALTER`.
const SCHEMA_V2: &str = r#"
-- Ordre effectivement appliqué à la lecture des dates de cette série.
-- 'DayFirst' par défaut : c'est ce que faisait le parseur pour toutes les séries
-- importées avant cette migration.
ALTER TABLE logger_series ADD COLUMN date_order TEXT NOT NULL DEFAULT 'DayFirst';

-- 1 = le fichier ne prouvait pas son ordre (ou se contredisait) : à confirmer.
-- 0 pour l'existant : on ne peut pas rejuger un fichier qu'on n'a plus, et lever
-- le drapeau rétroactivement partout ne serait que du bruit.
ALTER TABLE logger_series ADD COLUMN date_format_uncertain INTEGER NOT NULL DEFAULT 0;
"#;

/// Migration v3 — intégration de l'API Infoclimat.
///
/// Quatre choses, toutes conditionnées par le fait que l'API est une source
/// **réinterrogeable** là où un fichier de logger ne l'est pas :
///
/// 1. deux colonnes techniques sur `measurements` (`rssi_dbm`, `battery`), que
///    seules les stations publiant un champ `complements` renseignent ;
/// 2. de quoi tracer une série d'API sur `logger_series` : licence des données,
///    identifiant de station, instant du dernier appel réussi ;
/// 3. `points.infoclimat_station_id`, le rattachement demandé en partie E ;
/// 4. **l'unicité `(series_id, timestamp_utc)`**, la vraie raison d'être de cette
///    migration : sans elle, deux appels d'API qui se recouvrent empilent
///    silencieusement les mêmes mesures en double.
///
/// La table `app_settings` accompagne le tout : l'écran Paramètres a besoin d'un
/// endroit où poser l'intervalle minimal de rafraîchissement.
const SCHEMA_V3: &str = r#"
-- --- Colonnes techniques des mesures ------------------------------------
-- Qualité du signal radio, en dBm.
ALTER TABLE measurements ADD COLUMN rssi_dbm REAL;
-- Batterie, VALEUR BRUTE non convertie : l'unité n'est pas déterminée (cf. le
-- commentaire de `model::Measurement::battery`). Aucune mise à l'échelle ici.
ALTER TABLE measurements ADD COLUMN battery REAL;

-- --- Traçabilité d'une série d'API --------------------------------------
-- Mention de licence telle que renvoyée par l'API ("NON-COMMERCIAL ONLY: CC BY NC").
ALTER TABLE logger_series ADD COLUMN data_license TEXT;
-- Identifiant de la station interrogée ("STATIC0479").
ALTER TABLE logger_series ADD COLUMN station_id TEXT;
-- Dernier appel API réussi (epoch s UTC) — garde-fou de fréquence.
ALTER TABLE logger_series ADD COLUMN last_api_fetch_at INTEGER;

-- --- Rattachement d'un point à une station Infoclimat --------------------
ALTER TABLE points ADD COLUMN infoclimat_station_id TEXT;

-- --- Réglages applicatifs ------------------------------------------------
CREATE TABLE IF NOT EXISTS app_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// Étape d'unicité de la v3, séparée du reste parce qu'elle doit s'exécuter
/// **après** un dédoublonnage : `CREATE UNIQUE INDEX` échoue si la base existante
/// contient déjà deux mesures au même instant dans une même série.
///
/// On garde la ligne de plus petit `id` — la première insérée, donc celle du
/// premier import. L'ancien index non unique devient redondant (mêmes colonnes,
/// même ordre) : on le supprime.
const SCHEMA_V3_UNIQUE: &str = r#"
DELETE FROM measurements
 WHERE id NOT IN (SELECT MIN(id) FROM measurements GROUP BY series_id, timestamp_utc);

CREATE UNIQUE INDEX IF NOT EXISTS idx_measurements_series_ts_unique
    ON measurements(series_id, timestamp_utc);

DROP INDEX IF EXISTS idx_measurements_series_ts;
"#;

/// Migration v4 — empreinte de contenu, pour reconnaître un ré-import.
///
/// L'unicité posée en v3 protège *l'intérieur* d'une série. Elle ne dit rien du
/// cas où l'on rejoue le même fichier dans une **nouvelle** série : deux séries
/// jumelles, et des statistiques de point qui comptent tout en double. Cette
/// colonne porte le condensat du lot importé (voir [`crate::fingerprint`]).
///
/// `NULL` pour l'existant, et c'est voulu : recalculer l'empreinte demanderait
/// les fichiers d'origine, qu'on n'a plus. Les séries antérieures ne participent
/// donc pas à la détection — ni comme suspect, ni comme référence. L'index est
/// partiel pour la même raison : il ne référence que les lignes renseignées.
const SCHEMA_V4: &str = r#"
ALTER TABLE logger_series ADD COLUMN content_fingerprint TEXT;

CREATE INDEX IF NOT EXISTS idx_series_point_fingerprint
    ON logger_series(point_id, content_fingerprint)
 WHERE content_fingerprint IS NOT NULL;
"#;

/// Migration v5 — source ROMMA et champs météo complémentaires.
///
/// Trois choses, dont une qui coûte cher :
///
/// 1. **six colonnes facultatives sur `measurements`** (pression, vent, pluie,
///    rayonnement). `NULL` pour tout l'existant, et c'est exactement ce que le
///    test `une_base_v4_migre_avec_les_champs_meteo_a_null` vérifie ;
/// 2. `points.romma_station_id`, le pendant de `infoclimat_station_id` ;
/// 3. **la reconstruction de `logger_series`**, pour élargir la contrainte
///    `CHECK (source_type IN …)` à `'RommaImport'`.
///
/// ## Pourquoi reconstruire la table
///
/// SQLite ne sait pas modifier une contrainte `CHECK` : ni `ALTER TABLE … ALTER
/// CONSTRAINT`, ni `DROP CONSTRAINT`. La seule voie est la procédure documentée
/// (« [Making Other Kinds Of Table Schema Changes] ») : créer une table jumelle
/// au bon schéma, y recopier les lignes, supprimer l'ancienne, renommer.
///
/// Deux précautions, sans lesquelles la manœuvre casse la base :
///
/// * **les clés étrangères sont désactivées le temps de l'opération** (voir
///   [`Db::migrate`]). `measurements.series_id` référence `logger_series` ;
///   supprimer la table sous les pieds de 500 000 mesures les effacerait par
///   cascade. Le `PRAGMA foreign_key_check` qui suit vérifie qu'aucune référence
///   n'est restée en l'air.
/// * **les index sont recréés après le renommage**. `DROP TABLE` emporte les
///   index de la table ; les oublier ne casserait rien de visible, mais le
///   listing des séries et la détection de ré-import repasseraient en balayage
///   complet.
///
/// L'ordre compte : on supprime `logger_series` **avant** de renommer la jumelle.
/// Aucune table ne référence `logger_series_v5`, le renommage ne réécrit donc
/// aucune clause `REFERENCES` ailleurs — celle de `measurements` continue de
/// désigner `logger_series`, qui redevient valide à l'instant du renommage.
///
/// [Making Other Kinds Of Table Schema Changes]: https://www.sqlite.org/lang_altertable.html#otheralter
const SCHEMA_V5: &str = r#"
-- --- Champs météo complémentaires (source ROMMA) ------------------------
-- `NULL` pour toutes les mesures existantes : aucune source antérieure ne
-- publiait autre chose que température et humidité.
ALTER TABLE measurements ADD COLUMN pressure_hpa           REAL;
ALTER TABLE measurements ADD COLUMN wind_avg_kmh           REAL;
ALTER TABLE measurements ADD COLUMN wind_gust_kmh          REAL;
ALTER TABLE measurements ADD COLUMN wind_direction_degrees REAL;
ALTER TABLE measurements ADD COLUMN rain_mm                REAL;
ALTER TABLE measurements ADD COLUMN solar_radiation        REAL;

-- --- Rattachement d'un point à une station ROMMA -------------------------
ALTER TABLE points ADD COLUMN romma_station_id TEXT;

-- --- Élargissement du CHECK de `logger_series.source_type` ---------------
CREATE TABLE logger_series_v5 (
    id                      TEXT    PRIMARY KEY,
    point_id                TEXT    NOT NULL REFERENCES points(id) ON DELETE CASCADE,
    source_type             TEXT    NOT NULL,
    role                    TEXT    NOT NULL,
    logger_serial           TEXT,
    imported_at             INTEGER NOT NULL,
    raw_file_reference      TEXT,
    clock_offset_seconds    INTEGER NOT NULL DEFAULT 0,
    raw_timestamp_was_utc   INTEGER NOT NULL DEFAULT 0,
    label                   TEXT,
    sample_interval_seconds INTEGER,
    date_order              TEXT    NOT NULL DEFAULT 'DayFirst',
    date_format_uncertain   INTEGER NOT NULL DEFAULT 0,
    data_license            TEXT,
    station_id              TEXT,
    last_api_fetch_at       INTEGER,
    content_fingerprint     TEXT,

    CHECK (role IN ('Primary','Backup')),
    CHECK (source_type IN ('ElitechImport','ElitechDirect','InfoclimatApi',
                           'DraginoApi','RommaImport','Manual'))
);

INSERT INTO logger_series_v5 (
    id, point_id, source_type, role, logger_serial, imported_at,
    raw_file_reference, clock_offset_seconds, raw_timestamp_was_utc,
    label, sample_interval_seconds, date_order, date_format_uncertain,
    data_license, station_id, last_api_fetch_at, content_fingerprint
)
SELECT
    id, point_id, source_type, role, logger_serial, imported_at,
    raw_file_reference, clock_offset_seconds, raw_timestamp_was_utc,
    label, sample_interval_seconds, date_order, date_format_uncertain,
    data_license, station_id, last_api_fetch_at, content_fingerprint
FROM logger_series;

DROP TABLE logger_series;
ALTER TABLE logger_series_v5 RENAME TO logger_series;

-- Les index de l'ancienne table sont partis avec elle.
CREATE INDEX IF NOT EXISTS idx_series_point ON logger_series(point_id);
CREATE INDEX IF NOT EXISTS idx_series_point_fingerprint
    ON logger_series(point_id, content_fingerprint)
 WHERE content_fingerprint IS NOT NULL;
"#;

/// Migration v6 — la saisie manuelle sur site devient une source à part entière.
///
/// Une seule chose, et c'est la même manœuvre coûteuse qu'en v5 : **élargir le
/// `CHECK (source_type IN …)`** de `logger_series` à `'ManualEntry'`. SQLite ne
/// sait toujours pas modifier une contrainte en place ; il faut donc recréer la
/// table, y recopier les lignes, supprimer l'ancienne et renommer.
///
/// Les mêmes deux précautions qu'en v5 s'appliquent, pour les mêmes raisons :
/// clés étrangères désactivées le temps de l'opération (sans quoi le `DROP
/// TABLE` emporterait toutes les mesures par cascade), et index recréés après le
/// renommage.
///
/// ## Pourquoi une variante plutôt qu'un libellé
///
/// `'Manual'` existait déjà, mais il désigne un **fichier** dont l'utilisateur a
/// désigné les colonnes lui-même : la donnée reste une lecture de logger. Une
/// valeur tapée à la main — « moyenne 6 h–9 h = −4,2 °C » — n'est pas de même
/// nature, et c'est précisément ce que les statistiques et l'agent doivent
/// pouvoir distinguer. Réutiliser `'Manual'` aurait rendu les deux cas
/// indiscernables en base, sans espoir de les démêler après coup.
const SCHEMA_V6: &str = r#"
CREATE TABLE logger_series_v6 (
    id                      TEXT    PRIMARY KEY,
    point_id                TEXT    NOT NULL REFERENCES points(id) ON DELETE CASCADE,
    source_type             TEXT    NOT NULL,
    role                    TEXT    NOT NULL,
    logger_serial           TEXT,
    imported_at             INTEGER NOT NULL,
    raw_file_reference      TEXT,
    clock_offset_seconds    INTEGER NOT NULL DEFAULT 0,
    raw_timestamp_was_utc   INTEGER NOT NULL DEFAULT 0,
    label                   TEXT,
    sample_interval_seconds INTEGER,
    date_order              TEXT    NOT NULL DEFAULT 'DayFirst',
    date_format_uncertain   INTEGER NOT NULL DEFAULT 0,
    data_license            TEXT,
    station_id              TEXT,
    last_api_fetch_at       INTEGER,
    content_fingerprint     TEXT,

    CHECK (role IN ('Primary','Backup')),
    CHECK (source_type IN ('ElitechImport','ElitechDirect','InfoclimatApi',
                           'DraginoApi','RommaImport','Manual','ManualEntry'))
);

INSERT INTO logger_series_v6 (
    id, point_id, source_type, role, logger_serial, imported_at,
    raw_file_reference, clock_offset_seconds, raw_timestamp_was_utc,
    label, sample_interval_seconds, date_order, date_format_uncertain,
    data_license, station_id, last_api_fetch_at, content_fingerprint
)
SELECT
    id, point_id, source_type, role, logger_serial, imported_at,
    raw_file_reference, clock_offset_seconds, raw_timestamp_was_utc,
    label, sample_interval_seconds, date_order, date_format_uncertain,
    data_license, station_id, last_api_fetch_at, content_fingerprint
FROM logger_series;

DROP TABLE logger_series;
ALTER TABLE logger_series_v6 RENAME TO logger_series;

-- Les index de l'ancienne table sont partis avec elle.
CREATE INDEX IF NOT EXISTS idx_series_point ON logger_series(point_id);
CREATE INDEX IF NOT EXISTS idx_series_point_fingerprint
    ON logger_series(point_id, content_fingerprint)
 WHERE content_fingerprint IS NOT NULL;
"#;

/// Migration v7 — ajout des catégories `Col` et `Versants`.
///
/// La catégorie participe à des contraintes `CHECK` de `points`. SQLite ne
/// sachant pas les modifier en place, la table est reconstruite en conservant
/// ses colonnes et identifiants.
const SCHEMA_V7: &str = r#"
CREATE TABLE points_v7 (
    id                    TEXT    PRIMARY KEY,
    project_id            TEXT    NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name                  TEXT    NOT NULL,
    latitude              REAL    NOT NULL,
    longitude             REAL    NOT NULL,
    altitude_m            REAL,
    category              TEXT    NOT NULL,
    size                  TEXT,
    factor_sky            TEXT,
    factor_wind           TEXT,
    factor_slope          TEXT,
    factor_snow_cover     TEXT,
    factor_forest_cover   TEXT,
    is_open_terrain       INTEGER,
    notes                 TEXT,
    created_at            INTEGER NOT NULL,
    infoclimat_station_id TEXT,
    romma_station_id      TEXT,

    CHECK (category IN ('Cat1','Cat2','Cat3','Cat4','Col','Versants')),
    CHECK (size IS NULL OR size IN ('Gr','Pt')),
    CHECK (
           (category IN ('Cat1','Col','Versants') AND size IS NULL)
        OR (category = 'Cat2' AND (size IS NULL OR size = 'Gr'))
        OR (category IN ('Cat3','Cat4') AND size IS NOT NULL AND size IN ('Gr','Pt'))
    )
);

INSERT INTO points_v7 (
    id, project_id, name, latitude, longitude, altitude_m, category, size,
    factor_sky, factor_wind, factor_slope, factor_snow_cover,
    factor_forest_cover, is_open_terrain, notes, created_at,
    infoclimat_station_id, romma_station_id
)
SELECT
    id, project_id, name, latitude, longitude, altitude_m, category, size,
    factor_sky, factor_wind, factor_slope, factor_snow_cover,
    factor_forest_cover, is_open_terrain, notes, created_at,
    infoclimat_station_id, romma_station_id
FROM points;

DROP TABLE points;
ALTER TABLE points_v7 RENAME TO points;
CREATE INDEX IF NOT EXISTS idx_points_project ON points(project_id);
"#;

/// Migration v8 — partages émis depuis ce poste (voir [`crate::share`]).
///
/// Une table, et une raison : **le jeton de révocation n'est rendu qu'une fois
/// par le serveur**. Sans cette table, un partage créé deviendrait irrévocable
/// jusqu'à son expiration. Le jeton y est stocké et n'en sort que vers le
/// serveur ; l'UI ne voit que le reste.
///
/// `project_id` passe à `NULL` si le projet est supprimé : le partage, lui,
/// existe toujours sur le serveur, et doit rester révocable.
///
/// Rien n'est ajouté à `points` : un partage reçu est rangé par défaut dans un
/// projet à part, nommé d'après le code — la provenance se lit dans le nom du
/// projet et dans la référence de chaque série, sans colonne supplémentaire.
const SCHEMA_V8: &str = r#"
CREATE TABLE IF NOT EXISTS issued_shares (
    code              TEXT    PRIMARY KEY,
    project_id        TEXT    REFERENCES projects(id) ON DELETE SET NULL,
    mode              TEXT    NOT NULL CHECK (mode IN ('points','points_series')),
    point_count       INTEGER NOT NULL,
    series_count      INTEGER NOT NULL,
    measurement_count INTEGER NOT NULL,
    created_at        INTEGER NOT NULL,   -- epoch secondes UTC
    expires_at        INTEGER NOT NULL,   -- epoch secondes UTC, fixé par le serveur
    revoke_token      TEXT    NOT NULL,
    revoked_at        INTEGER
);
"#;

/// Migration v9 — table de suivi des empreintes synchronisées dans le Cloud.
///
/// Permet d'éviter de ré-émettre ou re-télécharger des enregistrements inchangés,
/// et de propager proprement les suppressions locales.
const SCHEMA_V9: &str = r#"
CREATE TABLE IF NOT EXISTS cloud_synced_hashes (
    key  TEXT PRIMARY KEY,
    hash TEXT NOT NULL
);
"#;

/// Migration v10 — état des mesures par série et par mois (UTC).
///
/// `n` : nombre de mesures du mois ; `version` : augmente à chaque ajout,
/// modification ou suppression d'une mesure du mois. Tenue à jour par des
/// triggers, donc aussi quand un script externe (import France) écrit dans la
/// base. Deux usages :
/// * le nombre de mesures d'une série se lit sans `COUNT(*)` sur `measurements`
///   (qui coûtait des secondes sur 1 153 stations) ;
/// * la synchro Cloud sait quels mois ont changé sans relire une seule mesure.
///
/// Coût : chaque écriture de mesure met aussi à jour une ligne de cette petite
/// table (quelques milliers de lignes au plus, toujours en cache).
const SCHEMA_V10: &str = r#"
CREATE TABLE IF NOT EXISTS series_months (
    series_id TEXT    NOT NULL REFERENCES logger_series(id) ON DELETE CASCADE,
    month     INTEGER NOT NULL,              -- AAAAMM, en UTC
    n         INTEGER NOT NULL DEFAULT 0,
    version   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (series_id, month)
) WITHOUT ROWID;

"#;

/// Triggers de `series_months`. Rejoués (`IF NOT EXISTS`) à chaque ouverture :
/// un import en masse (`scripts/stations_fr/importer_france.py`) retire le
/// trigger d'ajout pendant son chargement, qui va deux fois plus vite sans lui.
const TRIGGERS_V10: &str = r#"
CREATE TRIGGER IF NOT EXISTS trg_mesures_ajout AFTER INSERT ON measurements BEGIN
    INSERT INTO series_months (series_id, month, n, version)
    VALUES (NEW.series_id, CAST(strftime('%Y%m', NEW.timestamp_utc, 'unixepoch') AS INTEGER), 1, 1)
    ON CONFLICT (series_id, month) DO UPDATE SET n = n + 1, version = version + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_mesures_retrait AFTER DELETE ON measurements BEGIN
    UPDATE series_months SET n = n - 1, version = version + 1
    WHERE series_id = OLD.series_id
      AND month = CAST(strftime('%Y%m', OLD.timestamp_utc, 'unixepoch') AS INTEGER);
END;

CREATE TRIGGER IF NOT EXISTS trg_mesures_modif AFTER UPDATE ON measurements BEGIN
    UPDATE series_months SET n = n - 1, version = version + 1
    WHERE series_id = OLD.series_id
      AND month = CAST(strftime('%Y%m', OLD.timestamp_utc, 'unixepoch') AS INTEGER);
    INSERT INTO series_months (series_id, month, n, version)
    VALUES (NEW.series_id, CAST(strftime('%Y%m', NEW.timestamp_utc, 'unixepoch') AS INTEGER), 1, 1)
    ON CONFLICT (series_id, month) DO UPDATE SET n = n + 1, version = version + 1;
END;
"#;

/// Migration v11 — agrégats journaliers de température par série (jour UTC).
///
/// Une fenêtre de plus de dix jours (courbes, statistiques, record d'un point)
/// se lit ici plutôt que dans `measurements` : sur le projet France, dix ans
/// d'une station tiennent en 3 650 lignes au lieu de 500 000.
/// `day` : jours depuis le 1970-01-01. On stocke la somme et non la moyenne,
/// pour regrouper exactement les jours en semaines et en mois.
///
/// Tenue à jour par des triggers, comme `series_months` : un ajout met le jour
/// à jour en place ; une suppression ou une modification marque seulement le
/// jour `stale` (un minimum ne se retire pas d'un agrégat), et
/// [`RAFRAICHIR_SERIES_DAYS`] le recalcule avant toute lecture.
const SCHEMA_V11: &str = r#"
CREATE TABLE IF NOT EXISTS series_days (
    series_id TEXT    NOT NULL REFERENCES logger_series(id) ON DELETE CASCADE,
    day       INTEGER NOT NULL,
    n         INTEGER NOT NULL,
    t_sum     REAL    NOT NULL,
    t_min     REAL    NOT NULL,
    t_max     REAL    NOT NULL,
    first_ts  INTEGER NOT NULL,
    last_ts   INTEGER NOT NULL,
    stale     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (series_id, day)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_series_days_stale ON series_days(stale) WHERE stale = 1;
"#;

/// Calcul complet des jours de toutes les séries (migration v11).
const REMPLIR_SERIES_DAYS: &str = r#"
DELETE FROM series_days;
INSERT INTO series_days (series_id, day, n, t_sum, t_min, t_max, first_ts, last_ts, stale)
    SELECT m.series_id, ((m.timestamp_utc - ((m.timestamp_utc % 86400) + 86400) % 86400) / 86400), count(*), sum(m.temperature_celsius),
           min(m.temperature_celsius), max(m.temperature_celsius),
           min(m.timestamp_utc), max(m.timestamp_utc), 0
    FROM measurements m
    GROUP BY 1, 2;
"#;

/// Séries qui ont des mesures mais aucun jour (base rouverte par une version
/// sans la table, import en masse sans trigger) : calculées à l'ouverture.
const REPARER_SERIES_DAYS: &str = r#"
INSERT OR REPLACE INTO series_days (series_id, day, n, t_sum, t_min, t_max, first_ts, last_ts, stale)
    SELECT m.series_id, ((m.timestamp_utc - ((m.timestamp_utc % 86400) + 86400) % 86400) / 86400), count(*), sum(m.temperature_celsius),
           min(m.temperature_celsius), max(m.temperature_celsius),
           min(m.timestamp_utc), max(m.timestamp_utc), 0
    FROM measurements m
    WHERE m.series_id IN (
        SELECT s.id FROM logger_series s
        WHERE NOT EXISTS (SELECT 1 FROM series_days x WHERE x.series_id = s.id)
          AND EXISTS (SELECT 1 FROM measurements y WHERE y.series_id = s.id)
    )
    GROUP BY 1, 2;
"#;

/// Recalcule les jours marqués `stale`, puis retire ceux qui n'ont plus de
/// mesure. Ne coûte rien quand aucun jour n'est marqué (index partiel).
pub(crate) const RAFRAICHIR_SERIES_DAYS: &str = r#"
INSERT OR REPLACE INTO series_days (series_id, day, n, t_sum, t_min, t_max, first_ts, last_ts, stale)
    SELECT d.series_id, d.day, count(*), sum(m.temperature_celsius),
           min(m.temperature_celsius), max(m.temperature_celsius),
           min(m.timestamp_utc), max(m.timestamp_utc), 0
    FROM series_days d
    JOIN measurements m ON m.series_id = d.series_id
                       AND m.timestamp_utc >= d.day * 86400
                       AND m.timestamp_utc < (d.day + 1) * 86400
    WHERE d.stale = 1
    GROUP BY d.series_id, d.day;
DELETE FROM series_days WHERE stale = 1;
"#;

const TRIGGERS_V11: &str = r#"
CREATE TRIGGER IF NOT EXISTS trg_jours_ajout AFTER INSERT ON measurements BEGIN
    INSERT INTO series_days (series_id, day, n, t_sum, t_min, t_max, first_ts, last_ts, stale)
    VALUES (NEW.series_id, ((NEW.timestamp_utc - ((NEW.timestamp_utc % 86400) + 86400) % 86400) / 86400), 1, NEW.temperature_celsius,
            NEW.temperature_celsius, NEW.temperature_celsius, NEW.timestamp_utc, NEW.timestamp_utc, 0)
    ON CONFLICT (series_id, day) DO UPDATE SET
        n = n + 1,
        t_sum = t_sum + excluded.t_sum,
        t_min = min(t_min, excluded.t_min),
        t_max = max(t_max, excluded.t_max),
        first_ts = min(first_ts, excluded.first_ts),
        last_ts = max(last_ts, excluded.last_ts);
END;

CREATE TRIGGER IF NOT EXISTS trg_jours_retrait AFTER DELETE ON measurements BEGIN
    UPDATE series_days SET stale = 1 WHERE series_id = OLD.series_id AND day = ((OLD.timestamp_utc - ((OLD.timestamp_utc % 86400) + 86400) % 86400) / 86400);
END;

CREATE TRIGGER IF NOT EXISTS trg_jours_modif AFTER UPDATE OF timestamp_utc, temperature_celsius, series_id ON measurements BEGIN
    UPDATE series_days SET stale = 1 WHERE series_id = OLD.series_id AND day = ((OLD.timestamp_utc - ((OLD.timestamp_utc % 86400) + 86400) % 86400) / 86400);
    INSERT INTO series_days (series_id, day, n, t_sum, t_min, t_max, first_ts, last_ts, stale)
    VALUES (NEW.series_id, ((NEW.timestamp_utc - ((NEW.timestamp_utc % 86400) + 86400) % 86400) / 86400), 0, 0, 0, 0, 0, 0, 1)
    ON CONFLICT (series_id, day) DO UPDATE SET stale = 1;
END;
"#;

/// Reconstruit l'état des séries qui n'en ont aucun (base migrée, import en
/// masse sans trigger, import interrompu). Coût nul quand tout est à jour.
///
/// La version vaut l'instant présent : elle diffère ainsi de toute version déjà
/// envoyée au Cloud, et le mois sera renvoyé une fois par précaution.
const REPARER_SERIES_MONTHS: &str = r#"
INSERT OR REPLACE INTO series_months (series_id, month, n, version)
    SELECT m.series_id, CAST(strftime('%Y%m', m.timestamp_utc, 'unixepoch') AS INTEGER), count(*),
           CAST(strftime('%s', 'now') AS INTEGER)
    FROM measurements m
    WHERE m.series_id IN (
        SELECT s.id FROM logger_series s
        WHERE NOT EXISTS (SELECT 1 FROM series_months x WHERE x.series_id = s.id)
    )
    GROUP BY 1, 2;
"#;

/// Bornes UTC `[début, fin)` d'un mois `AAAAMM`, en secondes.
pub fn bornes_mois(mois: i64) -> Option<(i64, i64)> {
    let (a, m) = ((mois / 100) as i32, (mois % 100) as u32);
    let debut = chrono::NaiveDate::from_ymd_opt(a, m, 1)?;
    let fin = if m == 12 { chrono::NaiveDate::from_ymd_opt(a + 1, 1, 1)? } else { chrono::NaiveDate::from_ymd_opt(a, m + 1, 1)? };
    Some((
        debut.and_hms_opt(0, 0, 0)?.and_utc().timestamp(),
        fin.and_hms_opt(0, 0, 0)?.and_utc().timestamp(),
    ))
}

// ---------------------------------------------------------------------------
// Conversion des instants
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Écriture d'une mesure : une seule liste de colonnes, deux stratégies de conflit
// ---------------------------------------------------------------------------
//
// Les treize colonnes d'une mesure étaient recopiées à l'identique dans quatre
// endroits (insertion, mise à jour, deux `SELECT`). Depuis la v5 elles sont
// treize et non plus sept : la moindre divergence entre deux de ces listes
// écrirait silencieusement des `NULL`. On les déclare donc **une fois**.

/// Nombre de mesures préparées entre deux repères de progression.
///
/// Ce n'est **pas** une taille de transaction : l'import reste tout ou rien, une
/// seule transaction du début à la fin. C'est le pas auquel on rend la main pour
/// dire où en est le travail — une archive ROMMA de 477 000 lignes s'importe
/// sinon dans un silence complet de plusieurs secondes.
pub const MEASUREMENT_BATCH: usize = 25_000;

/// Insertion d'un **premier** lot : un instant déjà pris est simplement ignoré.
/// C'est le chemin d'un fichier, où un horodatage répété est une anomalie de la
/// source et non une donnée corrigée.
const INSERT_MEASUREMENT_IGNORE: &str = "INSERT INTO measurements (
        series_id, timestamp_utc, raw_timestamp, temperature_celsius,
        humidity_percent, rssi_dbm, battery, pressure_hpa, wind_avg_kmh,
        wind_gust_kmh, wind_direction_degrees, rain_mm, solar_radiation
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
     ON CONFLICT(series_id, timestamp_utc) DO NOTHING";

/// Complément d'une série existante : un instant déjà connu est **mis à jour**,
/// mais seulement si au moins une valeur diffère (cf. [`Db::append_measurements`]).
const INSERT_MEASUREMENT_UPSERT: &str = "INSERT INTO measurements (
        series_id, timestamp_utc, raw_timestamp, temperature_celsius,
        humidity_percent, rssi_dbm, battery, pressure_hpa, wind_avg_kmh,
        wind_gust_kmh, wind_direction_degrees, rain_mm, solar_radiation
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
     ON CONFLICT(series_id, timestamp_utc) DO UPDATE SET
        raw_timestamp          = excluded.raw_timestamp,
        temperature_celsius    = excluded.temperature_celsius,
        humidity_percent       = excluded.humidity_percent,
        rssi_dbm               = excluded.rssi_dbm,
        battery                = excluded.battery,
        pressure_hpa           = excluded.pressure_hpa,
        wind_avg_kmh           = excluded.wind_avg_kmh,
        wind_gust_kmh          = excluded.wind_gust_kmh,
        wind_direction_degrees = excluded.wind_direction_degrees,
        rain_mm                = excluded.rain_mm,
        solar_radiation        = excluded.solar_radiation
      WHERE raw_timestamp          IS NOT excluded.raw_timestamp
         OR temperature_celsius    IS NOT excluded.temperature_celsius
         OR humidity_percent       IS NOT excluded.humidity_percent
         OR rssi_dbm               IS NOT excluded.rssi_dbm
         OR battery                IS NOT excluded.battery
         OR pressure_hpa           IS NOT excluded.pressure_hpa
         OR wind_avg_kmh           IS NOT excluded.wind_avg_kmh
         OR wind_gust_kmh          IS NOT excluded.wind_gust_kmh
         OR wind_direction_degrees IS NOT excluded.wind_direction_degrees
         OR rain_mm                IS NOT excluded.rain_mm
         OR solar_radiation        IS NOT excluded.solar_radiation";

/// Mise à jour conditionnelle d'une mesure dont l'instant est **déjà connu**.
///
/// Utilisée par [`Db::append_measurements`] en deuxième passe, après qu'un
/// `INSERT … DO NOTHING` ait détecté la collision : on met à jour uniquement si
/// au moins une valeur diffère (`IS NOT` traite `NULL` comme une valeur à part
/// entière, contrairement à `<>`).
///
/// Les positions des paramètres (?1…?13) sont identiques à celles de
/// [`bind_measurement`] — même appel, même jeu de valeurs, SQL différent.
const UPDATE_MEASUREMENT_IF_CHANGED: &str = "UPDATE measurements SET
     raw_timestamp          = ?3,
     temperature_celsius    = ?4,
     humidity_percent       = ?5,
     rssi_dbm               = ?6,
     battery                = ?7,
     pressure_hpa           = ?8,
     wind_avg_kmh           = ?9,
     wind_gust_kmh          = ?10,
     wind_direction_degrees = ?11,
     rain_mm                = ?12,
     solar_radiation        = ?13
  WHERE series_id = ?1 AND timestamp_utc = ?2
    AND (   raw_timestamp          IS NOT ?3
         OR temperature_celsius    IS NOT ?4
         OR humidity_percent       IS NOT ?5
         OR rssi_dbm               IS NOT ?6
         OR battery                IS NOT ?7
         OR pressure_hpa           IS NOT ?8
         OR wind_avg_kmh           IS NOT ?9
         OR wind_gust_kmh          IS NOT ?10
         OR wind_direction_degrees IS NOT ?11
         OR rain_mm                IS NOT ?12
         OR solar_radiation        IS NOT ?13)";

/// Paramètres d'une mesure, dans l'ordre des colonnes déclarées par
/// [`INSERT_MEASUREMENT_IGNORE`] et [`INSERT_MEASUREMENT_UPSERT`].
///
/// `series_id` est passé à part : un lot d'insertion écrit toutes ses mesures
/// dans la même série, dont l'identifiant peut ne pas encore être celui porté par
/// les structures (cas de la création, où la série est fabriquée après coup).
///
/// Les nombres sont des [`Value`] **possédées** (les instants sont convertis en
/// secondes à la volée, il n'existe aucun `i64` à emprunter dans la mesure),
/// mais l'identifiant de série est **emprunté** : le recopier en `String` à
/// chaque ligne coûtait une allocation par mesure — 477 000 pour une archive
/// ROMMA — pour un texte identique d'un bout à l'autre du lot.
fn bind_measurement<'a>(series_id: &'a str, m: &Measurement) -> [ToSqlOutput<'a>; 13] {
    fn opt(v: Option<f64>) -> ToSqlOutput<'static> {
        ToSqlOutput::Owned(v.map(Value::Real).unwrap_or(Value::Null))
    }
    fn nombre(v: Value) -> ToSqlOutput<'static> {
        ToSqlOutput::Owned(v)
    }
    [
        ToSqlOutput::Borrowed(ValueRef::Text(series_id.as_bytes())),
        nombre(Value::Integer(secs(m.timestamp_utc))),
        nombre(Value::Integer(secs(m.raw_timestamp))),
        nombre(Value::Real(m.temperature_celsius)),
        opt(m.humidity_percent),
        opt(m.rssi_dbm),
        opt(m.battery),
        opt(m.pressure_hpa),
        opt(m.wind_avg_kmh),
        opt(m.wind_gust_kmh),
        opt(m.wind_direction_degrees),
        opt(m.rain_mm),
        opt(m.solar_radiation),
    ]
}

/// `DateTime<Utc>` → secondes epoch (ce qui part en base).
pub fn secs(dt: DateTime<Utc>) -> i64 {
    dt.timestamp()
}

/// Secondes epoch → `DateTime<Utc>` (ce qui sort de la base).
/// Couverture d'un point : volume total et premier / dernier mois (AAAAMM,
/// UTC) ayant au moins une mesure.
#[derive(Debug, Clone, PartialEq)]
pub struct PointCoverage {
    pub point_id: String,
    pub measurement_count: usize,
    pub first_month: Option<i64>,
    pub last_month: Option<i64>,
}

pub fn ts_from_secs(value: i64) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::from_timestamp(value, 0)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(0, value))
}

/// Bilan d'un ajout de mesures à une série existante ([`Db::append_measurements`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppendOutcome {
    /// Instants encore inconnus de la série.
    pub inserted: usize,
    /// Instants déjà connus, dont au moins une valeur a changé.
    pub updated: usize,
    /// Instants déjà connus, strictement identiques : rien n'a été réécrit.
    pub unchanged: usize,
}

impl AppendOutcome {
    pub fn total(&self) -> usize {
        self.inserted + self.updated + self.unchanged
    }
}

/// Gare les `timestamp_utc` d'une série sous son minimum courant, le temps d'un
/// recalcul en masse.
///
/// **Pourquoi c'est nécessaire.** Depuis la v3, `(series_id, timestamp_utc)` est
/// unique, et SQLite vérifie cette unicité *ligne à ligne* — il n'existe pas de
/// contrainte différée hors clés étrangères. Or les deux opérations de correction
/// de la v1 réécrivent tous les instants d'une série d'un coup :
///
/// * `recalibrate_series` décale tout de `d` secondes. Décaler une série
///   régulière de +1 h amène la mesure de 06:00 sur 07:00 — instant qu'occupe
///   encore, à cet instant précis de la mise à jour, la mesure de 07:00. Collision
///   quasi systématique.
/// * `swap_series_date_order` permute jour et mois : deux mesures peuvent
///   littéralement échanger leur date (05/01 ↔ 01/05).
///
/// **Comment.** Une passe préalable écrit `plancher - id` dans chaque ligne, où
/// `plancher` est le minimum de la série lu *avant* toute écriture. Comme `id` est
/// unique et ≥ 1, ces valeurs sont distinctes entre elles et toutes strictement
/// inférieures au plus petit instant réel de la série : aucune collision possible,
/// ni avec une ligne déjà garée, ni avec une ligne pas encore traitée. Les autres
/// séries ne sont pas concernées — l'unicité porte sur le couple.
fn park_timestamps(tx: &rusqlite::Transaction<'_>, series_id: &str) -> AppResult<()> {
    let plancher: Option<i64> = tx.query_row(
        "SELECT MIN(timestamp_utc) FROM measurements WHERE series_id = ?1",
        [series_id],
        |r| r.get(0),
    )?;
    if let Some(plancher) = plancher {
        tx.execute(
            "UPDATE measurements SET timestamp_utc = ?2 - id WHERE series_id = ?1",
            params![series_id, plancher],
        )?;
    }
    Ok(())
}

/// Emballe une erreur métier dans une erreur rusqlite (utilisé par les mappers
/// de lignes, dont la signature est imposée par `query_map`).
fn conv<E>(e: E) -> rusqlite::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}

// ---------------------------------------------------------------------------
// Connexion
// ---------------------------------------------------------------------------

pub struct Db {
    conn: Mutex<Connection>,
    /// Résultats du bloc « Croisement de données » par point, réutilisés tant
    /// que les données du point n'ont pas changé (voir `crossing::CacheCroisement`).
    croisement: Mutex<crate::crossing::CacheCroisement>,
}

/// Taille du cache de pages SQLite (négatif = Kio, convention SQLite).
///
/// Android : le système tue les applications en arrière-plan selon leur
/// empreinte mémoire ; on se limite à 8 Mo. Poste fixe : 16 Mo (32 Mo
/// jusqu'à la 0.4.7) ; la fenêtre mmap reste, elle, à 256 Mo.
#[cfg(target_os = "android")]
pub(crate) const CACHE_SIZE_KIB: i64 = -8_192;
#[cfg(not(target_os = "android"))]
pub(crate) const CACHE_SIZE_KIB: i64 = -16_384;

/// Fenêtre mmap de SQLite, en octets.
///
/// Android : 0 (mmap désactivé, défaut de SQLite) ; les pages projetées
/// comptent dans la mémoire résidente du processus. Poste fixe : 256 Mo.
#[cfg(target_os = "android")]
pub(crate) const MMAP_SIZE_BYTES: i64 = 0;
#[cfg(not(target_os = "android"))]
pub(crate) const MMAP_SIZE_BYTES: i64 = 268_435_456;

impl Db {
    pub fn open(path: &Path) -> AppResult<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    /// Base éphémère — utilisée par les tests.
    pub fn open_in_memory() -> AppResult<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> AppResult<Self> {
        // WAL : lectures concurrentes pendant qu'un import écrit.
        // `pragma_update` ne convient pas ici : journal_mode renvoie une ligne.
        let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        // Cache de pages (valeur négative = kilo-octets, convention SQLite).
        // Poste fixe : 16 Mo au lieu des 2 Mo par défaut. Sur une archive ROMMA
        // (477 000 lignes × ~200 o), les scans de fenêtre temporelle restent
        // ainsi en mémoire entre deux appels successifs — changement de
        // sélection, recalcul de stats — sans relire le fichier.
        // Android : 8 Mo, voir `CACHE_SIZE_KIB`.
        conn.pragma_update(None, "cache_size", CACHE_SIZE_KIB)?;

        // Memory-mapped I/O. Poste fixe : 256 Mo de fenêtre adressable ; sur
        // SSD/NVMe, SQLite lit les pages via mmap plutôt que pread() —
        // typiquement 2× plus rapide pour les scans séquentiels.
        // Android : désactivé (0, la valeur par défaut de SQLite), voir
        // `MMAP_SIZE_BYTES`.
        conn.pragma_update(None, "mmap_size", MMAP_SIZE_BYTES)?;

        // Tris et index temporaires en mémoire plutôt que sur disque.
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        // Les extensions `db_write` ouvrent leur propre connexion : sans délai
        // d'attente, une écriture concurrente échouerait aussitôt sur
        // « database is locked » au lieu d'attendre quelques instants.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;

        Self::migrate(&conn)?;
        Ok(Db {
            conn: Mutex::new(conn),
            croisement: Mutex::new(Default::default()),
        })
    }

    /// Cache du bloc « Croisement de données ». Verrou distinct de celui de la
    /// connexion : ne jamais le garder pendant une requête.
    pub(crate) fn cache_croisement(&self) -> MutexGuard<'_, crate::crossing::CacheCroisement> {
        self.croisement.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Verrou de connexion. Un `Mutex` empoisonné (panique dans un autre thread)
    /// ne doit pas rendre l'application inutilisable : on récupère la connexion.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Exécute une fonction sous le verrou de connexion.
    ///
    /// Pour les modules qui possèdent leur propre table — `share` et ses
    /// `issued_shares` : leurs requêtes restent à côté du code qui leur donne
    /// un sens, sans exposer la connexion elle-même hors de ce fichier.
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
        f(&self.conn())
    }

    /// Accès direct à la connexion, **réservé aux tests**.
    ///
    /// Sert à fabriquer des états que l'application ne sait plus produire — par
    /// exemple une série sans empreinte de contenu, telle qu'en laisse une base
    /// migrée depuis la v3. Reconstituer ces cas autrement demanderait de garder
    /// du code mort dans le chemin normal.
    #[cfg(test)]
    pub(crate) fn conn_pour_test(&self) -> MutexGuard<'_, Connection> {
        self.conn()
    }

    fn migrate(conn: &Connection) -> AppResult<()> {
        let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;

        if current < 1 {
            conn.execute_batch(SCHEMA_V1)?;
        }
        if current < 2 {
            conn.execute_batch(SCHEMA_V2)?;
        }
        if current < 3 {
            conn.execute_batch(SCHEMA_V3)?;

            // Le dédoublonnage supprime des lignes : on dit combien, plutôt que
            // de laisser une base rétrécir en silence. Sur une base saine, zéro.
            let doublons: i64 = conn.query_row(
                "SELECT COUNT(*) - COUNT(DISTINCT series_id || ':' || timestamp_utc)
                 FROM measurements",
                [],
                |r| r.get(0),
            )?;
            if doublons > 0 {
                println!(
                    "[gws] migration v3 : {doublons} mesure(s) en double supprimée(s) \
                     (même série, même instant) avant la pose de la contrainte d'unicité."
                );
            }
            conn.execute_batch(SCHEMA_V3_UNIQUE)?;
        }
        if current < 4 {
            conn.execute_batch(SCHEMA_V4)?;
        }
        if current < 5 {
            // La v5 reconstruit `logger_series` (élargissement d'un CHECK, que
            // SQLite ne sait pas modifier en place). `measurements` la référence
            // avec `ON DELETE CASCADE` : si les clés étrangères restaient
            // actives, le `DROP TABLE` emporterait **toutes les mesures**.
            //
            // `PRAGMA foreign_keys` est un no-op à l'intérieur d'une
            // transaction ; il est donc posé ici, avant `execute_batch` (qui
            // ouvre la sienne pour chaque instruction), et non dedans.
            conn.pragma_update(None, "foreign_keys", false)?;
            let resultat = conn.execute_batch(SCHEMA_V5);
            // Remise en état **quoi qu'il arrive** : une migration à moitié
            // faite avec les clés étrangères désactivées est le pire des états.
            conn.pragma_update(None, "foreign_keys", true)?;
            resultat?;

            // Aucune mesure ne doit se retrouver orpheline après le remplacement
            // de la table. Le contraire signalerait une erreur de recopie ; on
            // refuse alors d'ouvrir la base plutôt que de la laisser incohérente.
            let orphelines: i64 = conn.query_row(
                "SELECT COUNT(*) FROM measurements m
                  WHERE NOT EXISTS (SELECT 1 FROM logger_series s WHERE s.id = m.series_id)",
                [],
                |r| r.get(0),
            )?;
            if orphelines > 0 {
                return Err(AppError::Other(format!(
                    "Migration v5 interrompue : {orphelines} mesure(s) ont perdu leur série \
                     pendant la reconstruction de `logger_series`. La base n'a pas été \
                     mise à jour ; restaurez une sauvegarde avant de réessayer."
                )));
            }
        }
        if current < 6 {
            // Même manœuvre qu'en v5, pour la même raison (un `CHECK` à élargir)
            // et donc avec les mêmes précautions : voir [`SCHEMA_V6`].
            conn.pragma_update(None, "foreign_keys", false)?;
            let resultat = conn.execute_batch(SCHEMA_V6);
            conn.pragma_update(None, "foreign_keys", true)?;
            resultat?;

            let orphelines: i64 = conn.query_row(
                "SELECT COUNT(*) FROM measurements m
                  WHERE NOT EXISTS (SELECT 1 FROM logger_series s WHERE s.id = m.series_id)",
                [],
                |r| r.get(0),
            )?;
            if orphelines > 0 {
                return Err(AppError::Other(format!(
                    "Migration v6 interrompue : {orphelines} mesure(s) ont perdu leur série \
                     pendant la reconstruction de `logger_series`. La base n'a pas été \
                     mise à jour ; restaurez une sauvegarde avant de réessayer."
                )));
            }
        }
        if current < 7 {
            // `logger_series` référence `points` : pendant le remplacement de
            // table, les clés étrangères restent inactives pour préserver les
            // séries et leurs mesures.
            conn.pragma_update(None, "foreign_keys", false)?;
            let resultat = conn.execute_batch(SCHEMA_V7);
            conn.pragma_update(None, "foreign_keys", true)?;
            resultat?;

            let orphelines: i64 = conn.query_row(
                "SELECT COUNT(*) FROM logger_series s
                  WHERE NOT EXISTS (SELECT 1 FROM points p WHERE p.id = s.point_id)",
                [],
                |r| r.get(0),
            )?;
            if orphelines > 0 {
                return Err(AppError::Other(format!(
                    "Migration v7 interrompue : {orphelines} série(s) ont perdu leur point pendant la reconstruction de `points`."
                )));
            }
        }
        if current < 8 {
            conn.execute_batch(SCHEMA_V8)?;
        }
        if current < 9 {
            conn.execute_batch(SCHEMA_V9)?;
        }
        if current < 10 {
            // Un seul passage sur `measurements` pour l'état initial, puis les
            // triggers prennent le relais. Dans une transaction : pas de table
            // à moitié remplie si l'app est fermée pendant la migration.
            conn.execute_batch(&format!("BEGIN; {SCHEMA_V10} {REPARER_SERIES_MONTHS} COMMIT;"))?;
        }
        // À chaque ouverture, après les migrations : triggers présents et
        // aucune série sans état (voir `TRIGGERS_V10`).
        conn.execute_batch(&format!("BEGIN; {TRIGGERS_V10} {REPARER_SERIES_MONTHS} COMMIT;"))?;
        if current < 11 {
            // Premier calcul des jours, une fois : quelques secondes pour
            // quelques millions de mesures.
            conn.execute_batch(&format!("BEGIN; {SCHEMA_V11} {REMPLIR_SERIES_DAYS} COMMIT;"))?;
        }
        conn.execute_batch(&format!(
            "BEGIN; {TRIGGERS_V11} {REPARER_SERIES_DAYS} {RAFRAICHIR_SERIES_DAYS} COMMIT;"
        ))?;

        if current != SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Projets
    // -----------------------------------------------------------------------

    pub fn list_projects(&self) -> AppResult<Vec<Project>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.id, p.name, p.created_at, p.last_opened_at,
                    (SELECT COUNT(*) FROM points WHERE project_id = p.id) AS point_count,
                    EXISTS(SELECT 1 FROM app_settings WHERE key = 'demo_project:' || p.id AND value = '1') AS is_demo
             FROM projects p
             ORDER BY p.last_opened_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_project)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create_project(&self, name: &str) -> AppResult<Project> {
        let project = Project::new(name)?;
        self.conn().execute(
            "INSERT INTO projects (id, name, created_at, last_opened_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                project.id,
                project.name,
                secs(project.created_at),
                secs(project.last_opened_at)
            ],
        )?;
        Ok(project)
    }

    pub fn create_project_with_id(
        &self,
        id: &str,
        name: &str,
        created_at: i64,
        last_opened_at: i64,
    ) -> AppResult<Project> {
        self.conn().execute(
            "INSERT INTO projects (id, name, created_at, last_opened_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, last_opened_at = MAX(projects.last_opened_at, excluded.last_opened_at)",
            params![id, name, created_at, last_opened_at],
        )?;
        self.get_project(id)
    }

    /// Supprime un projet et, par cascade, ses points, séries et mesures.
    ///
    /// Exposé par la commande `delete_project` (Paramètres › Gestion des projets),
    /// et utilisé pour défaire un import de partage qui a échoué en cours de route
    /// (voir `share::import_document`). Côté Cloud, les enregistrements du projet
    /// ne sont plus connus localement : la synchro suivante les supprime (étape 5).
    pub fn delete_project(&self, id: &str) -> AppResult<()> {
        self.conn().execute("DELETE FROM projects WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn get_project(&self, id: &str) -> AppResult<Project> {
        let conn = self.conn();
        let found = conn
            .query_row(
                "SELECT p.id, p.name, p.created_at, p.last_opened_at,
                        (SELECT COUNT(*) FROM points WHERE project_id = p.id) AS point_count,
                    EXISTS(SELECT 1 FROM app_settings WHERE key = 'demo_project:' || p.id AND value = '1') AS is_demo
                 FROM projects p WHERE p.id = ?1",
                [id],
                row_to_project,
            )
            .optional()?;
        found.ok_or_else(|| AppError::NotFound("Projet", id.to_string()))
    }

    /// Marque le projet comme ouvert (met à jour `last_opened_at`) et le renvoie.
    pub fn touch_project(&self, id: &str) -> AppResult<Project> {
        let updated = self.conn().execute(
            "UPDATE projects SET last_opened_at = ?2 WHERE id = ?1",
            params![id, secs(Utc::now())],
        )?;
        if updated == 0 {
            return Err(AppError::NotFound("Projet", id.to_string()));
        }
        self.get_project(id)
    }

    // -----------------------------------------------------------------------
    // Points
    // -----------------------------------------------------------------------

    pub fn list_points(&self, project_id: &str) -> AppResult<Vec<Point>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT pt.*,
                    (SELECT COUNT(*) FROM logger_series WHERE point_id = pt.id) AS series_count
             FROM points pt
             WHERE pt.project_id = ?1
             ORDER BY pt.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([project_id], row_to_point)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_point(&self, id: &str) -> AppResult<Point> {
        let conn = self.conn();
        let found = conn
            .query_row(
                "SELECT pt.*,
                        (SELECT COUNT(*) FROM logger_series WHERE point_id = pt.id) AS series_count
                 FROM points pt WHERE pt.id = ?1",
                [id],
                row_to_point,
            )
            .optional()?;
        found.ok_or_else(|| AppError::NotFound("Point", id.to_string()))
    }

    /// Insère un point **déjà validé** (`Point::from_new`). La re-validation est
    /// défensive : elle protège d'un futur appelant qui construirait un `Point`
    /// à la main.
    pub fn insert_point(&self, point: &Point) -> AppResult<()> {
        point.validate()?;
        self.conn().execute(
            "INSERT INTO points (
                id, project_id, name, latitude, longitude, altitude_m,
                category, size,
                factor_sky, factor_wind, factor_slope, factor_snow_cover,
                factor_forest_cover, is_open_terrain, notes, created_at,
                infoclimat_station_id, romma_station_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                point.id,
                point.project_id,
                point.name,
                point.latitude,
                point.longitude,
                point.altitude_m,
                point.category.as_str(),
                point.size.map(|s| s.as_str()),
                point.factor_sky,
                point.factor_wind,
                point.factor_slope,
                point.factor_snow_cover,
                point.factor_forest_cover,
                point.is_open_terrain,
                point.notes,
                secs(point.created_at),
                point.infoclimat_station_id,
                point.romma_station_id,
            ],
        )?;
        Ok(())
    }

    pub fn insert_or_replace_point(&self, point: &Point) -> AppResult<()> {
        point.validate()?;
        self.conn().execute(
            "INSERT INTO points (
                id, project_id, name, latitude, longitude, altitude_m,
                category, size,
                factor_sky, factor_wind, factor_slope, factor_snow_cover,
                factor_forest_cover, is_open_terrain, notes, created_at,
                infoclimat_station_id, romma_station_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name, latitude = excluded.latitude, longitude = excluded.longitude,
                altitude_m = excluded.altitude_m, category = excluded.category, size = excluded.size,
                factor_sky = excluded.factor_sky, factor_wind = excluded.factor_wind, factor_slope = excluded.factor_slope,
                factor_snow_cover = excluded.factor_snow_cover, factor_forest_cover = excluded.factor_forest_cover,
                is_open_terrain = excluded.is_open_terrain, notes = excluded.notes,
                infoclimat_station_id = excluded.infoclimat_station_id, romma_station_id = excluded.romma_station_id",
            params![
                point.id,
                point.project_id,
                point.name,
                point.latitude,
                point.longitude,
                point.altitude_m,
                point.category.as_str(),
                point.size.map(|s| s.as_str()),
                point.factor_sky,
                point.factor_wind,
                point.factor_slope,
                point.factor_snow_cover,
                point.factor_forest_cover,
                point.is_open_terrain,
                point.notes,
                secs(point.created_at),
                point.infoclimat_station_id,
                point.romma_station_id,
            ],
        )?;
        Ok(())
    }

    /// Met à jour un point existant.
    ///
    /// `project_id` et `created_at` ne figurent **pas** dans le `SET` : un point
    /// ne change pas de projet ni de date de création par ce chemin. La contrainte
    /// `CHECK` catégorie/taille protège en dernier ressort, comme à l'insertion.
    pub fn update_point(&self, point: &Point) -> AppResult<()> {
        point.validate()?;
        let updated = self.conn().execute(
            "UPDATE points SET
                name = ?2, latitude = ?3, longitude = ?4, altitude_m = ?5,
                category = ?6, size = ?7,
                factor_sky = ?8, factor_wind = ?9, factor_slope = ?10,
                factor_snow_cover = ?11, factor_forest_cover = ?12,
                is_open_terrain = ?13, notes = ?14,
                infoclimat_station_id = ?15, romma_station_id = ?16
             WHERE id = ?1",
            params![
                point.id,
                point.name,
                point.latitude,
                point.longitude,
                point.altitude_m,
                point.category.as_str(),
                point.size.map(|s| s.as_str()),
                point.factor_sky,
                point.factor_wind,
                point.factor_slope,
                point.factor_snow_cover,
                point.factor_forest_cover,
                point.is_open_terrain,
                point.notes,
                point.infoclimat_station_id,
                point.romma_station_id,
            ],
        )?;
        if updated == 0 {
            return Err(AppError::NotFound("Point", point.id.clone()));
        }
        Ok(())
    }

    /// Supprime un point et, par cascade, ses séries et mesures associées.
    pub fn delete_point(&self, id: &str) -> AppResult<()> {
        let deleted = self.conn().execute("DELETE FROM points WHERE id = ?1", [id])?;
        if deleted == 0 {
            return Err(AppError::NotFound("Point", id.to_string()));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Séries
    // -----------------------------------------------------------------------

    pub fn list_series_for_point(&self, point_id: &str) -> AppResult<Vec<LoggerSeries>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT s.*,
                    (SELECT COALESCE(SUM(n), 0) FROM series_months WHERE series_id = s.id) AS measurement_count
             FROM logger_series s
             WHERE s.point_id = ?1
             ORDER BY s.imported_at DESC",
        )?;
        let rows = stmt.query_map([point_id], row_to_series)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_series(&self, id: &str) -> AppResult<LoggerSeries> {
        let conn = self.conn();
        let found = conn
            .query_row(
                "SELECT s.*,
                        (SELECT COALESCE(SUM(n), 0) FROM series_months WHERE series_id = s.id) AS measurement_count
                 FROM logger_series s WHERE s.id = ?1",
                [id],
                row_to_series,
            )
            .optional()?;
        found.ok_or_else(|| AppError::NotFound("Série", id.to_string()))
    }

    /// Insère la série **et** ses mesures dans une seule transaction : un import
    /// qui échoue à mi-parcours ne laisse jamais de série vide en base.
    ///
    /// Renvoie le nombre de mesures **réellement écrites**. Il peut être inférieur
    /// à `measurements.len()` si la source elle-même contenait deux lignes au même
    /// instant : la contrainte d'unicité de la v3 les écarte
    /// (`ON CONFLICT … DO NOTHING`) au lieu de faire échouer tout l'import.
    /// L'écart est remonté à l'utilisateur dans le bilan d'import.
    pub fn insert_series_with_measurements(
        &self,
        series: &LoggerSeries,
        measurements: &[Measurement],
    ) -> AppResult<usize> {
        self.insert_series_with_measurements_iter(series, measurements)
    }

    /// [`Self::insert_series_with_measurements`], mais les mesures arrivent d'un
    /// **itérateur** : un import volumineux les fabrique une à une depuis ses
    /// enregistrements parsés, au lieu de construire d'abord une seconde copie
    /// complète en mémoire. Même transaction unique, même tout ou rien.
    pub fn insert_series_with_measurements_iter<I>(
        &self,
        series: &LoggerSeries,
        measurements: I,
    ) -> AppResult<usize>
    where
        I: IntoIterator,
        I::IntoIter: ExactSizeIterator,
        I::Item: Borrow<Measurement>,
    {
        let measurements = measurements.into_iter();
        let total = measurements.len();
        let mut conn = self.conn();
        let tx = conn.transaction()?;

        tx.execute(
            "INSERT INTO logger_series (
                id, point_id, source_type, role, logger_serial, imported_at,
                raw_file_reference, clock_offset_seconds, raw_timestamp_was_utc,
                label, sample_interval_seconds, date_order, date_format_uncertain,
                data_license, station_id, last_api_fetch_at, content_fingerprint
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                series.id,
                series.point_id,
                series.source_type.as_str(),
                series.role.as_str(),
                series.logger_serial,
                secs(series.imported_at),
                series.raw_file_reference,
                series.clock_offset_seconds,
                series.raw_timestamp_was_utc,
                series.label,
                series.sample_interval_seconds,
                series.date_order.as_str(),
                series.date_format_uncertain,
                series.data_license,
                series.station_id,
                series.last_api_fetch_at.map(secs),
                series.content_fingerprint,
            ],
        )?;

        let mut written = 0usize;
        {
            let mut stmt = tx.prepare(INSERT_MEASUREMENT_IGNORE)?;
            for (rang, m) in measurements.enumerate() {
                written +=
                    stmt.execute(params_from_iter(bind_measurement(&series.id, m.borrow())))?;
                // Une archive ROMMA fait 477 000 lignes : sans ce repère, l'import
                // paraît figé pendant une dizaine de secondes. Rien n'est écrit
                // sur disque avant le `commit` — l'import reste tout ou rien.
                let faites = rang + 1;
                if total > MEASUREMENT_BATCH && (faites % MEASUREMENT_BATCH == 0 || faites == total) {
                    println!("[gws] insertion : {faites} / {total} mesures préparées");
                }
            }
        }

        tx.commit()?;
        Ok(written)
    }

    /// Insère ou met à jour la série et ses mesures (utilisé par la synchronisation Cloud).
    pub fn insert_or_replace_series_with_measurements(
        &self,
        series: &LoggerSeries,
        measurements: &[Measurement],
    ) -> AppResult<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        upsert_series_meta(&tx, series)?;

        let mut written = 0usize;
        {
            let mut stmt = tx.prepare(INSERT_MEASUREMENT_UPSERT)?;
            for lot in measurements.chunks(MEASUREMENT_BATCH) {
                for m in lot {
                    written += stmt.execute(params_from_iter(bind_measurement(&series.id, m)))?;
                }
            }
        }

        tx.commit()?;
        Ok(written)
    }

    /// État par mois d'une série : `(AAAAMM, nombre de mesures, version)`.
    pub fn series_months(&self, series_id: &str) -> AppResult<Vec<(i64, i64, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT month, n, version FROM series_months WHERE series_id = ?1 ORDER BY month",
        )?;
        let rows = stmt.query_map([series_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Mesures d'un mois d'une série, triées. Un mois pèse au plus quelques
    /// milliers de lignes : c'est l'unité de lecture de la synchro Cloud, qui ne
    /// charge ainsi jamais une série entière en mémoire.
    pub fn measurements_in_month(&self, series_id: &str, mois: i64) -> AppResult<Vec<Measurement>> {
        let (debut, fin) = bornes_mois(mois).ok_or_else(|| AppError::Other(format!("mois invalide : {mois}")))?;
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT id, series_id, timestamp_utc, raw_timestamp,
                    temperature_celsius, humidity_percent, rssi_dbm, battery,
                    pressure_hpa, wind_avg_kmh, wind_gust_kmh,
                    wind_direction_degrees, rain_mm, solar_radiation
             FROM measurements
             WHERE series_id = ?1 AND timestamp_utc >= ?2 AND timestamp_utc < ?3
             ORDER BY timestamp_utc",
        )?;
        let rows = stmt.query_map(params![series_id, debut, fin], lecteur_mesures())?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Remplace les mesures d'un mois par celles reçues du Cloud, en une
    /// transaction, et renvoie la version du mois qui en résulte.
    pub fn replace_month(&self, series_id: &str, mois: i64, mesures: &[Measurement]) -> AppResult<i64> {
        let (debut, fin) = bornes_mois(mois).ok_or_else(|| AppError::Other(format!("mois invalide : {mois}")))?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM measurements WHERE series_id = ?1 AND timestamp_utc >= ?2 AND timestamp_utc < ?3",
            params![series_id, debut, fin],
        )?;
        {
            let mut stmt = tx.prepare(INSERT_MEASUREMENT_UPSERT)?;
            for m in mesures {
                stmt.execute(params_from_iter(bind_measurement(series_id, m)))?;
            }
        }
        let version: i64 = tx
            .query_row(
                "SELECT version FROM series_months WHERE series_id = ?1 AND month = ?2",
                params![series_id, mois],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        tx.commit()?;
        Ok(version)
    }
}

/// Insère ou met à jour les métadonnées d'une série, sans toucher aux mesures.
fn upsert_series_meta(conn: &Connection, series: &LoggerSeries) -> AppResult<()> {
    {
        conn.execute(
            "INSERT INTO logger_series (
                id, point_id, source_type, role, logger_serial, imported_at,
                raw_file_reference, clock_offset_seconds, raw_timestamp_was_utc,
                label, sample_interval_seconds, date_order, date_format_uncertain,
                data_license, station_id, last_api_fetch_at, content_fingerprint
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET
                point_id = excluded.point_id,
                source_type = excluded.source_type,
                role = excluded.role,
                logger_serial = excluded.logger_serial,
                imported_at = excluded.imported_at,
                raw_file_reference = excluded.raw_file_reference,
                clock_offset_seconds = excluded.clock_offset_seconds,
                raw_timestamp_was_utc = excluded.raw_timestamp_was_utc,
                label = excluded.label,
                sample_interval_seconds = excluded.sample_interval_seconds,
                date_order = excluded.date_order,
                date_format_uncertain = excluded.date_format_uncertain,
                data_license = excluded.data_license,
                station_id = excluded.station_id,
                last_api_fetch_at = excluded.last_api_fetch_at,
                content_fingerprint = excluded.content_fingerprint",
            params![
                series.id,
                series.point_id,
                series.source_type.as_str(),
                series.role.as_str(),
                series.logger_serial,
                secs(series.imported_at),
                series.raw_file_reference,
                series.clock_offset_seconds,
                series.raw_timestamp_was_utc,
                series.label,
                series.sample_interval_seconds,
                series.date_order.as_str(),
                series.date_format_uncertain,
                series.data_license,
                series.station_id,
                series.last_api_fetch_at.map(secs),
                series.content_fingerprint,
            ],
        )?;
    }
    Ok(())
}

impl Db {

    /// Supprime une série et, par cascade SQLite, ses mesures associées.
    pub fn delete_series(&self, id: &str) -> AppResult<()> {
        let deleted = self.conn().execute("DELETE FROM logger_series WHERE id = ?1", [id])?;
        if deleted == 0 {
            return Err(AppError::NotFound("Série", id.to_string()));
        }
        Ok(())
    }


    /// Ajoute des mesures à une série **existante** — le chemin du mode « appel
    /// direct » à l'API, qui réinterroge périodiquement la même série.
    ///
    /// ## Stratégie de conflit retenue
    ///
    /// Deux passes par mesure (seulement si l'instant est déjà connu) :
    ///
    /// 1. `INSERT … DO NOTHING` : `changes()` vaut 1 si la ligne est nouvelle,
    ///    0 si l'instant existait déjà.
    /// 2. Sur collision (0), `UPDATE … WHERE <une valeur diffère>` : `changes()`
    ///    vaut 1 si la mise à jour a changé quelque chose, 0 si tout était déjà
    ///    identique.
    ///
    /// Cette approche évite le préchargement de **tous** les timestamps de la
    /// série dans un `HashSet` (ancienne implémentation). Sur une série ROMMA de
    /// 477 000 lignes, ce préchargement allouait ~3,8 Mo et balayait la table
    /// entière avant chaque rafraîchissement API. La deuxième passe SQL n'est
    /// exécutée que pour les collisions, qui sont le cas normal (deux appels API
    /// successifs se recouvrent presque toujours).
    ///
    /// `IS NOT` est la comparaison « distincte » de SQLite : elle traite `NULL`
    /// comme une valeur, contrairement à `<>` qui renverrait `NULL` (donc faux).
    pub fn append_measurements(
        &self,
        series_id: &str,
        measurements: &[Measurement],
    ) -> AppResult<AppendOutcome> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;

        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM logger_series WHERE id = ?1",
                [series_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Err(AppError::NotFound("Série", series_id.to_string()));
        }

        let mut outcome = AppendOutcome::default();
        {
            let mut stmt_insert = tx.prepare(INSERT_MEASUREMENT_IGNORE)?;
            let mut stmt_update = tx.prepare(UPDATE_MEASUREMENT_IF_CHANGED)?;

            for m in measurements {
                let binds = bind_measurement(series_id, m);
                let inserted = stmt_insert.execute(params_from_iter(binds.iter()))?;
                if inserted == 1 {
                    // Ligne nouvelle : INSERT a réussi sans conflit.
                    outcome.inserted += 1;
                } else {
                    // Collision : l'instant existait déjà — essayer de mettre
                    // à jour si au moins une valeur diffère.
                    let updated = stmt_update.execute(params_from_iter(binds.iter()))?;
                    if updated == 1 {
                        outcome.updated += 1;
                    } else {
                        outcome.unchanged += 1;
                    }
                }
            }
        }

        tx.commit()?;
        Ok(outcome)
    }


    /// Horodate le dernier appel d'API réussi d'une série (garde-fou de fréquence).
    pub fn mark_series_fetched(&self, series_id: &str, at: DateTime<Utc>) -> AppResult<()> {
        let updated = self.conn().execute(
            "UPDATE logger_series SET last_api_fetch_at = ?2 WHERE id = ?1",
            params![series_id, secs(at)],
        )?;
        if updated == 0 {
            return Err(AppError::NotFound("Série", series_id.to_string()));
        }
        Ok(())
    }

    /// Instant de la mesure la plus récente d'une série, s'il y en a une.
    /// C'est le point de reprise du mode « appel direct » : on redemande à partir
    /// de là plutôt que de retélécharger tout l'historique.
    pub fn last_measurement_utc(&self, series_id: &str) -> AppResult<Option<DateTime<Utc>>> {
        let conn = self.conn();
        let max: Option<i64> = conn.query_row(
            "SELECT MAX(timestamp_utc) FROM measurements WHERE series_id = ?1",
            [series_id],
            |r| r.get(0),
        )?;
        Ok(max.map(ts_from_secs).transpose()?)
    }

    /// Séries d'un point provenant de l'API Infoclimat, pour une station donnée.
    /// Sert à retrouver la série à compléter plutôt qu'à en créer une nouvelle
    /// à chaque rafraîchissement.
    pub fn find_infoclimat_series(
        &self,
        point_id: &str,
        station_id: &str,
    ) -> AppResult<Option<LoggerSeries>> {
        let conn = self.conn();
        let found = conn
            .query_row(
                "SELECT s.*,
                        (SELECT COALESCE(SUM(n), 0) FROM series_months WHERE series_id = s.id) AS measurement_count
                 FROM logger_series s
                 WHERE s.point_id = ?1 AND s.source_type = 'InfoclimatApi' AND s.station_id = ?2
                 ORDER BY s.imported_at DESC
                 LIMIT 1",
                params![point_id, station_id],
                row_to_series,
            )
            .optional()?;
        Ok(found)
    }

    /// Série de ce point portant déjà cette empreinte de contenu, s'il en existe.
    ///
    /// C'est tout le mécanisme de détection de ré-import : avant de créer une
    /// série, on demande ici si le même lot a déjà été enregistré **sur ce
    /// point**. La portée est volontairement le point et non le projet — deux
    /// loggers voisins d'un même projet peuvent produire des fichiers proches,
    /// alors que le même contenu deux fois sur le *même* point est presque
    /// toujours une manipulation en double.
    ///
    /// La plus récente est renvoyée : après un import forcé, deux séries portent
    /// la même empreinte, et c'est celle qu'on vient d'accepter qui est la plus
    /// parlante dans le message d'alerte suivant.
    pub fn find_series_by_fingerprint(
        &self,
        point_id: &str,
        fingerprint: &str,
    ) -> AppResult<Option<LoggerSeries>> {
        let conn = self.conn();
        let found = conn
            .query_row(
                "SELECT s.*,
                        (SELECT COALESCE(SUM(n), 0) FROM series_months WHERE series_id = s.id) AS measurement_count
                 FROM logger_series s
                 WHERE s.point_id = ?1 AND s.content_fingerprint = ?2
                 ORDER BY s.imported_at DESC
                 LIMIT 1",
                params![point_id, fingerprint],
                row_to_series,
            )
            .optional()?;
        Ok(found)
    }

    /// Bornes UTC d'une série, sans relire ses mesures.
    ///
    /// `measurements_for_series` suivi d'un `period_stats` donnerait la même
    /// chose, au prix du chargement de toute la série — trop cher pour la seule
    /// phrase « la série existante couvre telle période » d'une alerte.
    pub fn series_bounds_utc(
        &self,
        series_id: &str,
    ) -> AppResult<(Option<DateTime<Utc>>, Option<DateTime<Utc>>)> {
        let conn = self.conn();
        let bornes: (Option<i64>, Option<i64>) = conn.query_row(
            "SELECT MIN(timestamp_utc), MAX(timestamp_utc)
             FROM measurements WHERE series_id = ?1",
            [series_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((
            bornes.0.map(ts_from_secs).transpose()?,
            bornes.1.map(ts_from_secs).transpose()?,
        ))
    }

    // -----------------------------------------------------------------------
    // Réglages applicatifs
    // -----------------------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "SELECT value FROM app_settings WHERE key = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.conn().execute(
            "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Écrit plusieurs réglages d'un bloc : tous ou aucun (une transaction).
    /// Sert aux états qui doivent rester cohérents entre eux, comme la
    /// configuration signée du relai (`relais_config.rs`) et son anti-rejeu.
    pub fn set_settings(&self, paires: &[(&str, &str)]) -> AppResult<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for (key, value) in paires {
            tx.execute(
                "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> AppResult<()> {
        self.conn().execute("DELETE FROM app_settings WHERE key = ?1", [key])?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Empreintes de synchronisation Cloud
    // -----------------------------------------------------------------------

    pub fn get_cloud_synced_hash(&self, key: &str) -> AppResult<Option<String>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "SELECT hash FROM cloud_synced_hashes WHERE key = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_cloud_synced_hash(&self, key: &str, hash: &str) -> AppResult<()> {
        self.conn().execute(
            "INSERT INTO cloud_synced_hashes (key, hash) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET hash = excluded.hash",
            params![key, hash],
        )?;
        Ok(())
    }

    pub fn delete_cloud_synced_hash(&self, key: &str) -> AppResult<()> {
        self.conn().execute(
            "DELETE FROM cloud_synced_hashes WHERE key = ?1",
            params![key],
        )?;
        Ok(())
    }

    pub fn clear_cloud_synced_hashes(&self) -> AppResult<()> {
        self.conn().execute("DELETE FROM cloud_synced_hashes", [])?;
        Ok(())
    }

    pub fn list_cloud_synced_keys(&self) -> AppResult<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT key FROM cloud_synced_hashes")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    pub fn clear_all_data(&self) -> AppResult<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM measurements", [])?;
        conn.execute("DELETE FROM logger_series", [])?;
        conn.execute("DELETE FROM points", [])?;
        conn.execute("DELETE FROM projects", [])?;
        conn.execute("DELETE FROM app_settings WHERE key = 'user_name'", [])?;
        Ok(())
    }

    /// Réapplique un nouveau décalage d'horloge à toute une série.
    ///
    /// C'est le scénario prévu au §1.4 : on découvre après coup que le RC-5 était
    /// réglé en UTC+2 et non UTC+1. On recalcule `timestamp_utc` **depuis
    /// `raw_timestamp`**, jamais depuis l'ancienne valeur corrigée — sinon les
    /// corrections s'empileraient.
    pub fn recalibrate_series(&self, series_id: &str, new_offset_seconds: i64) -> AppResult<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;

        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM logger_series WHERE id = ?1",
                [series_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Err(AppError::NotFound("Série", series_id.to_string()));
        }

        park_timestamps(&tx, series_id)?;
        let updated = tx.execute(
            "UPDATE measurements SET timestamp_utc = raw_timestamp + ?2 WHERE series_id = ?1",
            params![series_id, new_offset_seconds],
        )?;
        tx.execute(
            "UPDATE logger_series SET clock_offset_seconds = ?2 WHERE id = ?1",
            params![series_id, new_offset_seconds],
        )?;

        tx.commit()?;
        Ok(updated)
    }

    /// L'utilisateur confirme que l'ordre jour/mois appliqué est le bon.
    ///
    /// Ne recalcule **rien** : seul le doute est levé. C'est le pendant sans
    /// risque de [`Db::swap_series_date_order`].
    pub fn confirm_series_date_order(&self, series_id: &str) -> AppResult<()> {
        let updated = self.conn().execute(
            "UPDATE logger_series SET date_format_uncertain = 0 WHERE id = ?1",
            [series_id],
        )?;
        if updated == 0 {
            return Err(AppError::NotFound("Série", series_id.to_string()));
        }
        Ok(())
    }

    /// Permute jour et mois sur **toute** une série déjà importée.
    ///
    /// C'est le rattrapage prévu quand on découvre après coup que le fichier
    /// était en `MM/dd` alors qu'on l'a lu en `dd/MM` (ou l'inverse). Aucun
    /// réimport n'est nécessaire.
    ///
    /// ⚠️ Contrairement à [`Db::recalibrate_series`], cette opération réécrit
    /// `raw_timestamp` : c'est le brut lui-même qui porte la date fausse. On
    /// recalcule donc le brut, **puis** on re-dérive `timestamp_utc` avec la même
    /// instruction SQL que la recalibration, pour que l'invariant du §1.4
    /// n'existe qu'à un seul endroit textuel.
    ///
    /// Rien n'est écrit tant que **toutes** les lignes n'ont pas été validées :
    /// une série dont une date a un jour > 12 n'est pas permutable, et une
    /// permutation partielle serait pire que pas de permutation du tout.
    pub fn swap_series_date_order(&self, series_id: &str) -> AppResult<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;

        let (offset, ordre_actuel): (i64, String) = tx
            .query_row(
                "SELECT clock_offset_seconds, date_order FROM logger_series WHERE id = ?1",
                [series_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| AppError::NotFound("Série", series_id.to_string()))?;

        let ordre_actuel = DateOrder::parse(&ordre_actuel)
            .ok_or_else(|| AppError::Other(format!("Ordre de date illisible : {ordre_actuel}")))?;

        // --- Calcul intégral avant la moindre écriture.
        let a_permuter: Vec<(i64, i64)> = {
            let mut stmt =
                tx.prepare("SELECT id, raw_timestamp FROM measurements WHERE series_id = ?1")?;
            let lignes = stmt.query_map([series_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?;

            let mut sorties = Vec::new();
            let mut impossibles = 0usize;
            let mut premiere_impossible: Option<DateTime<Utc>> = None;

            for ligne in lignes {
                let (id, brut) = ligne?;
                let instant = ts_from_secs(brut)?;
                match swap_day_month_datetime(instant.naive_utc()) {
                    Some(permute) => sorties.push((id, permute.and_utc().timestamp())),
                    None => {
                        impossibles += 1;
                        premiere_impossible.get_or_insert(instant);
                    }
                }
            }

            if impossibles > 0 {
                let exemple = premiere_impossible
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                return Err(AppError::Other(format!(
                    "Permutation impossible : {impossibles} mesure(s) ont un jour supérieur à 12 \
                     (la première est le {exemple}), leur date se lit donc déjà sans ambiguïté. \
                     Ce fichier mêle deux formats de date : il faut le scinder et le réimporter. \
                     Rien n'a été modifié."
                )));
            }
            sorties
        };

        {
            let mut stmt = tx.prepare("UPDATE measurements SET raw_timestamp = ?2 WHERE id = ?1")?;
            for (id, brut) in &a_permuter {
                stmt.execute(params![id, brut])?;
            }
        }

        park_timestamps(&tx, series_id)?;

        // Copie littérale de `recalibrate_series` : l'invariant vit à un seul endroit.
        tx.execute(
            "UPDATE measurements SET timestamp_utc = raw_timestamp + ?2 WHERE series_id = ?1",
            params![series_id, offset],
        )?;

        tx.execute(
            "UPDATE logger_series SET date_order = ?2, date_format_uncertain = 0 WHERE id = ?1",
            params![series_id, ordre_actuel.inverse().as_str()],
        )?;

        tx.commit()?;
        Ok(a_permuter.len())
    }

    // -----------------------------------------------------------------------
    // Mesures
    // -----------------------------------------------------------------------

    /// (instant brut, °C) d'une série, triés, à partir d'un instant UTC
    /// facultatif. Sert au partage, qui n'envoie que ces deux valeurs.
    pub fn raw_temperatures_since(&self, series_id: &str, depuis: Option<i64>) -> AppResult<Vec<(i64, f64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT raw_timestamp, temperature_celsius FROM measurements
             WHERE series_id = ?1 AND timestamp_utc >= ?2
             ORDER BY timestamp_utc",
        )?;
        let rows = stmt.query_map(params![series_id, depuis.unwrap_or(i64::MIN)], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Mesures d'une série entre deux instants UTC inclus (secondes), triées.
    pub fn measurements_in_range(&self, series_id: &str, de: i64, a: i64) -> AppResult<Vec<Measurement>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, series_id, timestamp_utc, raw_timestamp,
                    temperature_celsius, humidity_percent, rssi_dbm, battery,
                    pressure_hpa, wind_avg_kmh, wind_gust_kmh,
                    wind_direction_degrees, rain_mm, solar_radiation
             FROM measurements
             WHERE series_id = ?1 AND timestamp_utc BETWEEN ?2 AND ?3
             ORDER BY timestamp_utc",
        )?;
        let rows = stmt.query_map(params![series_id, de, a], lecteur_mesures())?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Une page de mesures d'une série, la plus récente d'abord.
    pub fn measurements_page(&self, series_id: &str, offset: usize, limit: usize) -> AppResult<Vec<Measurement>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, series_id, timestamp_utc, raw_timestamp,
                    temperature_celsius, humidity_percent, rssi_dbm, battery,
                    pressure_hpa, wind_avg_kmh, wind_gust_kmh,
                    wind_direction_degrees, rain_mm, solar_radiation
             FROM measurements
             WHERE series_id = ?1
             ORDER BY timestamp_utc DESC
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![series_id, limit as i64, offset as i64], lecteur_mesures())?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Supprime les mesures d'une série entre deux instants UTC inclus.
    pub fn delete_measurements_range(&self, series_id: &str, de: i64, a: i64) -> AppResult<usize> {
        Ok(self.conn().execute(
            "DELETE FROM measurements WHERE series_id = ?1 AND timestamp_utc BETWEEN ?2 AND ?3",
            params![series_id, de, a],
        )?)
    }

    /// Supprime des mesures précises d'une série (par identifiant).
    pub fn delete_measurements_by_id(&self, series_id: &str, ids: &[i64]) -> AppResult<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut n = 0;
        {
            let mut stmt = tx.prepare("DELETE FROM measurements WHERE series_id = ?1 AND id = ?2")?;
            for id in ids {
                n += stmt.execute(params![series_id, id])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Mesures d'une série par identifiant (ordre de l'horodatage).
    pub fn measurements_by_id(&self, series_id: &str, ids: &[i64]) -> AppResult<Vec<Measurement>> {
        let mut tout = Vec::with_capacity(ids.len());
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, series_id, timestamp_utc, raw_timestamp,
                    temperature_celsius, humidity_percent, rssi_dbm, battery,
                    pressure_hpa, wind_avg_kmh, wind_gust_kmh,
                    wind_direction_degrees, rain_mm, solar_radiation
             FROM measurements WHERE series_id = ?1 AND id = ?2",
        )?;
        let mut lire = lecteur_mesures();
        for id in ids {
            if let Some(m) = stmt.query_row(params![series_id, id], &mut lire).optional()? {
                tout.push(m);
            }
        }
        tout.sort_by_key(|m| m.timestamp_utc);
        Ok(tout)
    }

    /// Change la température d'une mesure. Renvoie le nombre de lignes modifiées.
    pub fn set_measurement_temperature(&self, series_id: &str, id: i64, celsius: f64) -> AppResult<usize> {
        Ok(self.conn().execute(
            "UPDATE measurements SET temperature_celsius = ?3 WHERE series_id = ?1 AND id = ?2",
            params![series_id, id, celsius],
        )?)
    }

    /// Réécrit des mesures dans une série existante (même instant = remplacée).
    /// Sert à défaire une suppression ou une modification.
    pub fn restore_measurements(&self, series_id: &str, mesures: &[Measurement]) -> AppResult<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut n = 0usize;
        {
            let mut stmt = tx.prepare(INSERT_MEASUREMENT_UPSERT)?;
            for m in mesures {
                n += stmt.execute(params_from_iter(bind_measurement(series_id, m)))?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn measurements_for_series(&self, series_id: &str) -> AppResult<Vec<Measurement>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, series_id, timestamp_utc, raw_timestamp,
                    temperature_celsius, humidity_percent, rssi_dbm, battery,
                    pressure_hpa, wind_avg_kmh, wind_gust_kmh,
                    wind_direction_degrees, rain_mm, solar_radiation
             FROM measurements
             WHERE series_id = ?1
             ORDER BY timestamp_utc",
        )?;
        let rows = stmt.query_map([series_id], lecteur_mesures())?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// (instant UTC, °C) de toute une série, triés — le strict nécessaire pour
    /// `period_stats`.
    ///
    /// Relire des `Measurement` complets pour n'en garder que la température
    /// coûtait treize colonnes et une copie du `series_id` par ligne : sur une
    /// série de 200 000 mesures, une centaine de Mo pour un calcul qui en demande
    /// moins de 5.
    pub fn temperature_samples_for_series(&self, series_id: &str) -> AppResult<Vec<TempSample>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT timestamp_utc, temperature_celsius
             FROM measurements
             WHERE series_id = ?1
             ORDER BY timestamp_utc",
        )?;
        let rows = stmt.query_map([series_id], |row| {
            Ok(TempSample::new(ts_from_secs(row.get(0)?)?, row.get(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Toutes les mesures d'un point, **toutes séries confondues**.
    ///
    /// ⚠️ À n'utiliser que pour un aperçu : mélanger un logger principal et son
    /// secours fausse les statistiques (double comptage). Les écrans de
    /// comparaison travailleront série par série.
    pub fn measurements_for_point(&self, point_id: &str) -> AppResult<Vec<Measurement>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT m.id, m.series_id, m.timestamp_utc, m.raw_timestamp,
                    m.temperature_celsius, m.humidity_percent, m.rssi_dbm, m.battery,
                    m.pressure_hpa, m.wind_avg_kmh, m.wind_gust_kmh,
                    m.wind_direction_degrees, m.rain_mm, m.solar_radiation
             FROM measurements m
             JOIN logger_series s ON s.id = m.series_id
             WHERE s.point_id = ?1
             ORDER BY m.timestamp_utc",
        )?;
        let rows = stmt.query_map([point_id], lecteur_mesures())?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Mesures complètes d'un point sur une fenêtre `[from, to]`, toutes séries
    /// confondues — la version filtrée de [`Db::measurements_for_point`], pour
    /// l'export (page 2 du bloc Stations) qui n'a besoin ni de tout l'historique
    /// ni des colonnes météo qu'ignore `temperature_samples_for_point`.
    ///
    /// Même avertissement que `measurements_for_point` : Primary et Backup sont
    /// mélangés dans le résultat. Sans conséquence pour un export ligne à ligne
    /// (chaque ligne porte son propre `series_id`), contrairement à une
    /// statistique qui les confondrait.
    pub fn measurements_for_point_in_window(
        &self,
        point_id: &str,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> AppResult<Vec<Measurement>> {
        let mut sql = String::from(
            "SELECT m.id, m.series_id, m.timestamp_utc, m.raw_timestamp,
                    m.temperature_celsius, m.humidity_percent, m.rssi_dbm, m.battery,
                    m.pressure_hpa, m.wind_avg_kmh, m.wind_gust_kmh,
                    m.wind_direction_degrees, m.rain_mm, m.solar_radiation
             FROM measurements m
             JOIN logger_series s ON s.id = m.series_id
             WHERE s.point_id = ?1",
        );
        if let Some(f) = from {
            sql.push_str(&format!(" AND m.timestamp_utc >= {}", secs(f)));
        }
        if let Some(t) = to {
            sql.push_str(&format!(" AND m.timestamp_utc <= {}", secs(t)));
        }
        sql.push_str(" ORDER BY m.timestamp_utc");

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([point_id], lecteur_mesures())?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // -----------------------------------------------------------------------
    // Lecture allégée — bloc « Croisement de données »
    // -----------------------------------------------------------------------
    //
    // `measurements_for_point` reconstruit une `Measurement` complète par ligne,
    // `series_id: String` compris. C'est ce qu'il faut pour la fiche d'une
    // station ou le contexte de l'agent, qui lisent un point à la fois et à la
    // demande. Ce n'est pas ce qu'il faut pour le croisement, qui relit **huit
    // points à chaque changement de sélection** : sur l'archive ROMMA
    // (476 912 mesures), ça fait un demi-million d'allocations pour n'utiliser
    // que deux colonnes sur quatorze.
    //
    // Les trois méthodes qui suivent ne lisent que ce dont les statistiques de
    // température ont besoin, et poussent le filtrage temporel dans SQL —
    // l'index `idx_measurements_series_ts` couvre exactement ce prédicat.

    /// Échantillons (instant UTC, °C) d'un point sur une fenêtre `[from, to]`.
    ///
    /// Les bornes sont **inclusives** et facultatives ; `None` des deux côtés
    /// donne tout l'historique du point. Le résultat est trié chronologiquement.
    ///
    /// Les bornes sont interpolées dans le SQL plutôt que passées en paramètres
    /// liés parce qu'un prédicat de la forme `?2 IS NULL OR ts >= ?2` empêche
    /// SQLite de se servir de l'index : il ne sait pas, au moment de choisir son
    /// plan, si la borne sera nulle. Ce sont des `i64` calculés ici, jamais du
    /// texte venu de l'extérieur — aucune surface d'injection.
    pub fn temperature_samples_for_point(
        &self,
        point_id: &str,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> AppResult<Vec<TempSample>> {
        let mut sql = String::from(
            "SELECT m.timestamp_utc, m.temperature_celsius
               FROM measurements m
               JOIN logger_series s ON s.id = m.series_id
              WHERE s.point_id = ?1",
        );
        if let Some(f) = from {
            sql.push_str(&format!(" AND m.timestamp_utc >= {}", secs(f)));
        }
        if let Some(t) = to {
            sql.push_str(&format!(" AND m.timestamp_utc <= {}", secs(t)));
        }
        sql.push_str(" ORDER BY m.timestamp_utc");

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([point_id], |row| {
            Ok(TempSample::new(ts_from_secs(row.get(0)?)?, row.get(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Agrégats journaliers d'un point, **toutes séries confondues** (même
    /// règle que [`Db::temperature_samples_for_point`]), pour les jours
    /// `[from_day, to_day]` (numéros de jour UTC, bornes incluses).
    pub fn point_days(&self, point_id: &str, from_day: Option<i64>, to_day: Option<i64>) -> AppResult<Vec<DayAgg>> {
        let mut sql = String::from(
            "SELECT d.day, SUM(d.n), SUM(d.t_sum), MIN(d.t_min), MAX(d.t_max), MIN(d.first_ts), MAX(d.last_ts)
               FROM series_days d
               JOIN logger_series s ON s.id = d.series_id
              WHERE s.point_id = ?1",
        );
        if let Some(f) = from_day {
            sql.push_str(&format!(" AND d.day >= {f}"));
        }
        if let Some(t) = to_day {
            sql.push_str(&format!(" AND d.day <= {t}"));
        }
        sql.push_str(" GROUP BY d.day ORDER BY d.day");
        self.lire_jours(&sql, point_id)
    }

    /// Agrégats journaliers d'une série, triés.
    pub fn series_days(&self, series_id: &str) -> AppResult<Vec<DayAgg>> {
        self.lire_jours(
            "SELECT day, n, t_sum, t_min, t_max, first_ts, last_ts
               FROM series_days WHERE series_id = ?1 ORDER BY day",
            series_id,
        )
    }

    fn lire_jours(&self, sql: &str, id: &str) -> AppResult<Vec<DayAgg>> {
        let conn = self.conn();
        conn.execute_batch(RAFRAICHIR_SERIES_DAYS)?;
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([id], |r| {
            Ok(DayAgg {
                day: r.get(0)?,
                n: r.get::<_, i64>(1)?.max(0) as usize,
                sum: r.get(2)?,
                min: r.get(3)?,
                max: r.get(4)?,
                first_utc: ts_from_secs(r.get(5)?)?,
                last_utc: ts_from_secs(r.get(6)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Empreinte des données d'un point : change dès qu'une mesure d'une de
    /// ses séries est ajoutée, modifiée ou supprimée, ou qu'une série est
    /// ajoutée, retirée ou change de source.
    ///
    /// Lue dans `series_months`, dont les triggers augmentent `version` à
    /// chaque écriture de mesure : quelques centaines de lignes par point, sans
    /// toucher à `measurements`. Les identifiants des séries en font partie,
    /// pour qu'un ré-import (nouvelle série, même volume) ne passe pas inaperçu.
    pub fn empreinte_donnees_point(&self, point_id: &str) -> AppResult<String> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT (SELECT group_concat(x, ',') FROM
                        (SELECT id || ':' || source_type AS x FROM logger_series
                          WHERE point_id = ?1 ORDER BY id)),
                    COUNT(sm.month), COALESCE(SUM(sm.n), 0), COALESCE(SUM(sm.version), 0)
               FROM logger_series s
               LEFT JOIN series_months sm ON sm.series_id = s.id
              WHERE s.point_id = ?1",
        )?;
        let (series, mois, n, version): (Option<String>, i64, i64, i64) =
            stmt.query_row([point_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(format!("{}|{mois}|{n}|{version}", series.unwrap_or_default()))
    }

    /// Couverture de chaque point d'un projet, lue dans `series_months`.
    ///
    /// Sert au résumé IA des gros projets (France : 1 153 points) : un seul
    /// passage sur la petite table mensuelle, jamais sur `measurements`.
    /// Un point sans aucune série renvoie `0` mesure et des mois `None`.
    pub fn project_coverage(&self, project_id: &str) -> AppResult<Vec<PointCoverage>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.id, COALESCE(SUM(sm.n), 0),
                    MIN(CASE WHEN sm.n > 0 THEN sm.month END),
                    MAX(CASE WHEN sm.n > 0 THEN sm.month END)
               FROM points p
               LEFT JOIN logger_series s ON s.point_id = p.id
               LEFT JOIN series_months sm ON sm.series_id = s.id
              WHERE p.project_id = ?1
              GROUP BY p.id",
        )?;
        let rows = stmt.query_map([project_id], |row| {
            Ok(PointCoverage {
                point_id: row.get(0)?,
                measurement_count: row.get::<_, i64>(1)?.max(0) as usize,
                first_month: row.get(2)?,
                last_month: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Bornes de la période couverte par un point, **tout l'historique**.
    ///
    /// Sert à ancrer les fenêtres relatives (« 30 derniers jours ») sur la
    /// dernière mesure disponible plutôt que sur l'horloge du poste : un jeu de
    /// logger de 2024 relu en 2026 donnerait sinon une fenêtre toujours vide.
    pub fn point_period_bounds(&self, point_id: &str) -> AppResult<Option<PeriodBounds>> {
        let conn = self.conn();
        conn.execute_batch(RAFRAICHIR_SERIES_DAYS)?;
        let mut stmt = conn.prepare(
            "SELECT MIN(d.first_ts), MAX(d.last_ts)
               FROM series_days d
               JOIN logger_series s ON s.id = d.series_id
              WHERE s.point_id = ?1",
        )?;
        let bornes: (Option<i64>, Option<i64>) =
            stmt.query_row([point_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        match bornes {
            (Some(min), Some(max)) => Ok(Some((ts_from_secs(min)?, ts_from_secs(max)?))),
            _ => Ok(None),
        }
    }

    /// Record de froid d'un point **sur tout son historique**, et volume total.
    ///
    /// Distinct des statistiques de fenêtre : le minimum extrême d'un site est
    /// une propriété du site, pas de la période qu'on regarde en ce moment. Une
    /// fiche qui n'afficherait que le minimum des trente derniers jours ferait
    /// disparaître le record de l'hiver précédent sans le dire.
    ///
    /// Un seul passage sur les lignes du point : quatre agrégats dans la même
    /// requête. `MIN`/`MAX`/`COUNT` n'imposent aucun tri, contrairement à un
    /// `ORDER BY temperature_celsius LIMIT 1` — qui trierait 476 912 lignes.
    ///
    /// ⚠️ Une seule prise du verrou de connexion. Appeler ici une autre méthode
    /// de [`Db`] alors que la garde de `self.conn()` est encore vivante bloque
    /// le processus sur un `Mutex` non réentrant, sans message d'erreur.
    pub fn point_record(&self, point_id: &str) -> AppResult<PointRecord> {
        let conn = self.conn();
        conn.execute_batch(RAFRAICHIR_SERIES_DAYS)?;
        let mut stmt = conn.prepare(
            "SELECT MIN(d.t_min), COALESCE(SUM(d.n), 0),
                    MIN(d.first_ts), MAX(d.last_ts)
               FROM series_days d
               JOIN logger_series s ON s.id = d.series_id
              WHERE s.point_id = ?1",
        )?;
        let (record_min_c, measurement_count, premier, dernier): (
            Option<f64>,
            i64,
            Option<i64>,
            Option<i64>,
        ) = stmt.query_row([point_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;

        Ok(PointRecord {
            record_min_c,
            measurement_count: measurement_count.max(0) as usize,
            first_utc: premier.map(ts_from_secs).transpose()?,
            last_utc: dernier.map(ts_from_secs).transpose()?,
        })
    }
}

/// Première et dernière mesure d'un point, en UTC.
pub type PeriodBounds = (DateTime<Utc>, DateTime<Utc>);

/// Ce qu'un point vaut **hors de toute fenêtre** : son record et sa couverture.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PointRecord {
    /// Minimum absolu sur tout l'historique du point, toutes séries confondues.
    pub record_min_c: Option<f64>,
    pub measurement_count: usize,
    pub first_utc: Option<DateTime<Utc>>,
    pub last_utc: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Mappers de lignes
// ---------------------------------------------------------------------------

fn row_to_project(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get("id")?,
        name: row.get("name")?,
        created_at: ts_from_secs(row.get("created_at")?)?,
        last_opened_at: ts_from_secs(row.get("last_opened_at")?)?,
        point_count: row.get("point_count").unwrap_or(0),
        is_demo: row.get("is_demo").unwrap_or(false),
    })
}

fn row_to_point(row: &Row<'_>) -> rusqlite::Result<Point> {
    let category: String = row.get("category")?;
    let size: Option<String> = row.get("size")?;
    Ok(Point {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        name: row.get("name")?,
        latitude: row.get("latitude")?,
        longitude: row.get("longitude")?,
        altitude_m: row.get("altitude_m")?,
        category: Category::parse(&category).map_err(conv)?,
        size: PointSize::parse_optional(size.as_deref()).map_err(conv)?,
        factor_sky: row.get("factor_sky")?,
        factor_wind: row.get("factor_wind")?,
        factor_slope: row.get("factor_slope")?,
        factor_snow_cover: row.get("factor_snow_cover")?,
        factor_forest_cover: row.get("factor_forest_cover")?,
        is_open_terrain: row.get("is_open_terrain")?,
        notes: row.get("notes")?,
        infoclimat_station_id: row.get("infoclimat_station_id")?,
        romma_station_id: row.get("romma_station_id")?,
        created_at: ts_from_secs(row.get("created_at")?)?,
        series_count: row.get("series_count").unwrap_or(0),
    })
}

fn row_to_series(row: &Row<'_>) -> rusqlite::Result<LoggerSeries> {
    let source_type: String = row.get("source_type")?;
    let role: String = row.get("role")?;
    Ok(LoggerSeries {
        id: row.get("id")?,
        point_id: row.get("point_id")?,
        source_type: SourceType::parse(&source_type).map_err(conv)?,
        role: SeriesRole::parse(&role).map_err(conv)?,
        logger_serial: row.get("logger_serial")?,
        imported_at: ts_from_secs(row.get("imported_at")?)?,
        raw_file_reference: row.get("raw_file_reference")?,
        clock_offset_seconds: row.get("clock_offset_seconds")?,
        raw_timestamp_was_utc: row.get("raw_timestamp_was_utc")?,
        label: row.get("label")?,
        sample_interval_seconds: row.get("sample_interval_seconds")?,
        data_license: row.get("data_license")?,
        station_id: row.get("station_id")?,
        last_api_fetch_at: row
            .get::<_, Option<i64>>("last_api_fetch_at")?
            .map(ts_from_secs)
            .transpose()?,
        date_order: {
            let brut: String = row.get("date_order")?;
            DateOrder::parse(&brut).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    format!("ordre de date inconnu en base : « {brut} »").into(),
                )
            })?
        },
        date_format_uncertain: row.get("date_format_uncertain")?,
        content_fingerprint: row.get("content_fingerprint")?,
        measurement_count: row.get("measurement_count").unwrap_or(0),
    })
}

/// Lecteur de lignes `measurements` qui **partage** l'identifiant de série :
/// chaque `series_id` distinct n'est alloué qu'une fois par requête, et toutes
/// les mesures de la série reçoivent un clone de ce même `Arc<str>` (un
/// compteur incrémenté) au lieu d'une chaîne de 36 octets chacune.
///
/// Les séries d'un point se comptent sur les doigts d'une main (Primary,
/// Backup, quelques imports) : une recherche linéaire suffit, et elle tient
/// aussi quand la requête entrelace deux séries triées par instant.
fn lecteur_mesures() -> impl FnMut(&Row<'_>) -> rusqlite::Result<Measurement> {
    let mut connus: Vec<Arc<str>> = Vec::new();
    move |row| {
        let series_id = match row.get_ref("series_id")? {
            rusqlite::types::ValueRef::Text(b) => match connus.iter().find(|c| c.as_bytes() == b) {
                Some(c) => c.clone(),
                None => {
                    let neuf: Arc<str> = match std::str::from_utf8(b) {
                        Ok(texte) => texte.into(),
                        // Même lecture (et même erreur) qu'avant : `String`.
                        Err(_) => row.get::<_, String>("series_id")?.into(),
                    };
                    connus.push(neuf.clone());
                    neuf
                }
            },
            // Type inattendu : même lecture (et même erreur) qu'avant.
            _ => row.get::<_, String>("series_id")?.into(),
        };
        row_to_measurement(row, series_id)
    }
}

fn row_to_measurement(row: &Row<'_>, series_id: Arc<str>) -> rusqlite::Result<Measurement> {
    Ok(Measurement {
        id: row.get("id")?,
        series_id,
        timestamp_utc: ts_from_secs(row.get("timestamp_utc")?)?,
        raw_timestamp: ts_from_secs(row.get("raw_timestamp")?)?,
        temperature_celsius: row.get("temperature_celsius")?,
        humidity_percent: row.get("humidity_percent")?,
        rssi_dbm: row.get("rssi_dbm")?,
        battery: row.get("battery")?,
        pressure_hpa: row.get("pressure_hpa")?,
        wind_avg_kmh: row.get("wind_avg_kmh")?,
        wind_gust_kmh: row.get("wind_gust_kmh")?,
        wind_direction_degrees: row.get("wind_direction_degrees")?,
        rain_mm: row.get("rain_mm")?,
        solar_radiation: row.get("solar_radiation")?,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{apply_clock_offset, now_utc, NewPoint};
    use uuid::Uuid;

    fn db() -> Db {
        Db::open_in_memory().expect("base mémoire")
    }

    fn new_point(project_id: &str, name: &str, category: &str, size: Option<&str>) -> NewPoint {
        NewPoint {
            project_id: project_id.to_string(),
            name: name.to_string(),
            latitude: 44.9812,
            longitude: 5.4213,
            altitude_m: Some(1150.0),
            category: category.to_string(),
            size: size.map(|s| s.to_string()),
            factor_sky: Some("dégagé").map(String::from),
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
    fn cycle_projet_point() {
        let db = db();
        assert!(db.list_projects().unwrap().is_empty());

        let projet = db.create_project("Vercors").unwrap();
        let liste = db.list_projects().unwrap();
        assert_eq!(liste.len(), 1);
        assert_eq!(liste[0].name, "Vercors");
        assert_eq!(liste[0].point_count, 0);

        let point = Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let points = db.list_points(&projet.id).unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].name, "Darbounouse");
        assert_eq!(points[0].category, Category::Cat3);
        assert_eq!(points[0].size, Some(PointSize::Gr));
        assert_eq!(points[0].altitude_m, Some(1150.0));
        assert_eq!(points[0].factor_sky.as_deref(), Some("dégagé"));
        assert_eq!(points[0].is_open_terrain, Some(false));
        assert_eq!(points[0].series_count, 0);

        assert_eq!(db.list_projects().unwrap()[0].point_count, 1);
        assert!(db.get_point("inexistant").is_err());
    }

    #[test]
    fn touch_projet_met_a_jour_la_date_douverture() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let rouvert = db.touch_project(&projet.id).unwrap();
        assert!(rouvert.last_opened_at >= projet.last_opened_at);
        assert!(db.touch_project("inexistant").is_err());
    }

    #[test]
    fn projet_sans_nom_refuse() {
        assert!(db().create_project("   ").is_err());
    }

    /// La contrainte SQL doit rattraper une écriture qui contournerait le Rust.
    #[test]
    fn la_contrainte_sql_double_la_validation_rust() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();

        let insert_brut = |category: &str, size: Option<&str>| {
            db.conn().execute(
                "INSERT INTO points (id, project_id, name, latitude, longitude,
                                     category, size, created_at)
                 VALUES (?1, ?2, 'X', 44.0, 5.0, ?3, ?4, 0)",
                params![uuid::Uuid::new_v4().to_string(), projet.id, category, size],
            )
        };

        assert!(insert_brut("Cat1", None).is_ok());
        assert!(insert_brut("Cat1", Some("Gr")).is_err());
        assert!(insert_brut("Cat2", Some("Gr")).is_ok());
        assert!(insert_brut("Cat2", Some("Pt")).is_err());
        assert!(insert_brut("Cat3", None).is_err());
        assert!(insert_brut("Cat3", Some("Pt")).is_ok());
        assert!(insert_brut("Cat4", None).is_err());
        assert!(insert_brut("Cat4", Some("Gr")).is_ok());
        assert!(insert_brut("Cat5", None).is_err());
    }

    fn measurement(series_id: &str, raw_iso: &str, offset: i64, temp: f64) -> Measurement {
        let raw: DateTime<Utc> = raw_iso.parse().unwrap();
        Measurement {
            humidity_percent: Some(85.0),
            ..Measurement::new(series_id, apply_clock_offset(raw, offset), raw, temp)
        }
    }

    fn series(point_id: &str, offset: i64) -> LoggerSeries {
        LoggerSeries {
            id: uuid::Uuid::new_v4().to_string(),
            point_id: point_id.to_string(),
            source_type: SourceType::ElitechImport,
            role: SeriesRole::Primary,
            logger_serial: Some("RC5000123".into()),
            imported_at: Utc::now(),
            raw_file_reference: Some("export.txt".into()),
            clock_offset_seconds: offset,
            raw_timestamp_was_utc: false,
            label: None,
            sample_interval_seconds: Some(600),
            data_license: None,
            station_id: None,
            last_api_fetch_at: None,
            date_order: DateOrder::DayFirst,
            date_format_uncertain: false,
            content_fingerprint: None,
            measurement_count: 0,
        }
    }

    #[test]
    fn les_echantillons_d_une_serie_donnent_les_memes_stats_que_ses_mesures() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point = Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        // Deux jours, insérés dans le désordre : la relecture doit trier.
        let s = series(&point.id, 0);
        let mesures = vec![
            measurement(&s.id, "2024-01-06T03:00:00Z", 0, -18.2),
            measurement(&s.id, "2024-01-05T07:00:00Z", 0, -12.5),
            measurement(&s.id, "2024-01-05T14:00:00Z", 0, 2.4),
            measurement(&s.id, "2024-01-06T13:00:00Z", 0, -1.0),
        ];
        db.insert_series_with_measurements(&s, &mesures).unwrap();

        let echantillons = db.temperature_samples_for_series(&s.id).unwrap();
        assert_eq!(echantillons.len(), 4);
        assert!(echantillons.windows(2).all(|p| p[0].timestamp_utc <= p[1].timestamp_utc));
        assert_eq!(
            crate::stats::period_stats(&echantillons),
            crate::stats::period_stats(&db.measurements_for_series(&s.id).unwrap())
        );
        // Une autre série du même point n'y figure pas.
        let autre = series(&point.id, 0);
        db.insert_series_with_measurements(&autre, &[measurement(&autre.id, "2024-01-05T08:00:00Z", 0, 30.0)])
            .unwrap();
        assert_eq!(db.temperature_samples_for_series(&s.id).unwrap().len(), 4);
    }

    #[test]
    fn bornes_des_mois() {
        use chrono::TimeZone;
        let (d, f) = bornes_mois(202412).unwrap();
        assert_eq!(d, Utc.with_ymd_and_hms(2024, 12, 1, 0, 0, 0).unwrap().timestamp());
        assert_eq!(f, Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap().timestamp());
        assert!(bornes_mois(202413).is_none());
    }

    /// Les jours stockés valent un recalcul complet depuis les mesures.
    fn verifier_jours(db: &Db, series_id: &str, etape: &str) {
        let stockes = db.series_days(series_id).unwrap();
        let attendus = crate::stats::day_aggs(&db.temperature_samples_for_series(series_id).unwrap());
        assert_eq!(stockes.len(), attendus.len(), "{etape} : nombre de jours");
        for (a, b) in stockes.iter().zip(&attendus) {
            assert_eq!((a.day, a.n, a.min, a.max, a.first_utc, a.last_utc), (b.day, b.n, b.min, b.max, b.first_utc, b.last_utc), "{etape}");
            assert!((a.sum - b.sum).abs() < 1e-9, "{etape} : somme");
        }
    }

    #[test]
    fn jours_suivent_toutes_les_ecritures() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point = Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();
        let s = series(&point.id, 0);
        let depart: DateTime<Utc> = "2024-01-30T20:00:00Z".parse().unwrap();
        let mesures: Vec<Measurement> = (0..600)
            .map(|i| Measurement::new(s.id.as_str(), depart + chrono::Duration::minutes(10 * i), depart + chrono::Duration::minutes(10 * i), ((i * 37) % 91) as f64 / 4.0 - 12.0))
            .collect();
        db.insert_series_with_measurements(&s, &mesures).unwrap();
        verifier_jours(&db, &s.id, "insertion");

        // Ajout qui recouvre et prolonge.
        let suite: Vec<Measurement> = (550..800)
            .map(|i| Measurement::new(s.id.as_str(), depart + chrono::Duration::minutes(10 * i), depart + chrono::Duration::minutes(10 * i), -20.0 + i as f64 / 100.0))
            .collect();
        db.append_measurements(&s.id, &suite).unwrap();
        verifier_jours(&db, &s.id, "ajout avec recouvrement");

        // Suppression du minimum d'un jour : le minimum doit remonter.
        let min_id = db.measurements_for_series(&s.id).unwrap().iter().min_by(|a, b| a.temperature_celsius.total_cmp(&b.temperature_celsius)).unwrap().id;
        db.delete_measurements_by_id(&s.id, &[min_id]).unwrap();
        verifier_jours(&db, &s.id, "suppression par identifiant");

        let a = (depart + chrono::Duration::hours(30)).timestamp();
        db.delete_measurements_range(&s.id, a, a + 86_400).unwrap();
        verifier_jours(&db, &s.id, "suppression d'une plage");

        let id = db.measurements_for_series(&s.id).unwrap()[3].id;
        db.set_measurement_temperature(&s.id, id, 42.0).unwrap();
        verifier_jours(&db, &s.id, "modification d'une valeur");

        db.recalibrate_series(&s.id, 7 * 3600).unwrap();
        verifier_jours(&db, &s.id, "recalage d'horloge");

        db.replace_month(&s.id, 202402, &[measurement(&s.id, "2024-02-10T12:00:00Z", 0, 5.0)]).unwrap();
        verifier_jours(&db, &s.id, "remplacement d'un mois");

        let avant = db.measurements_for_series(&s.id).unwrap();
        db.delete_measurements_range(&s.id, 0, i64::MAX / 4).unwrap();
        assert!(db.series_days(&s.id).unwrap().is_empty());
        db.restore_measurements(&s.id, &avant).unwrap();
        verifier_jours(&db, &s.id, "restauration (annulation)");

        // Record et période d'un point : lus dans les jours.
        let r = db.point_record(&point.id).unwrap();
        let toutes = db.temperature_samples_for_series(&s.id).unwrap();
        assert_eq!(r.measurement_count, toutes.len());
        assert_eq!(r.record_min_c, crate::stats::extreme_min(&toutes));
        assert_eq!(r.first_utc, toutes.first().map(|t| t.timestamp_utc));
        assert_eq!(r.last_utc, toutes.last().map(|t| t.timestamp_utc));

        db.delete_series(&s.id).unwrap();
        assert!(db.series_days(&s.id).unwrap().is_empty());
        assert_eq!(db.point_record(&point.id).unwrap().measurement_count, 0);
    }

    #[test]
    fn jours_d_un_point_combinent_ses_series() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point = Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();
        let (a, mut b) = (series(&point.id, 0), series(&point.id, 0));
        b.role = SeriesRole::Backup;
        db.insert_series_with_measurements(&a, &[measurement(&a.id, "2024-03-01T01:00:00Z", 0, -2.0), measurement(&a.id, "2024-03-02T01:00:00Z", 0, 1.0)]).unwrap();
        db.insert_series_with_measurements(&b, &[measurement(&b.id, "2024-03-01T05:00:00Z", 0, -6.0)]).unwrap();
        let jours = db.point_days(&point.id, None, None).unwrap();
        let attendus = crate::stats::day_aggs(&db.temperature_samples_for_point(&point.id, None, None).unwrap());
        assert_eq!(jours, attendus);
        let d = jours[1].day;
        assert_eq!(db.point_days(&point.id, Some(d), Some(d)).unwrap().len(), 1);
    }

    #[test]
    fn jours_se_reparent_apres_un_import_sans_trigger() {
        let db = db();
        let projet = db.create_project("France").unwrap();
        let point = Point::from_new(new_point(&projet.id, "Station", "Cat1", None)).unwrap();
        db.insert_point(&point).unwrap();
        let s = series(&point.id, 0);
        db.insert_series_with_measurements(&s, &[]).unwrap();
        {
            let conn = db.conn_pour_test();
            conn.execute_batch("DROP TRIGGER trg_jours_ajout").unwrap();
            conn.execute(
                "INSERT INTO measurements (series_id, timestamp_utc, raw_timestamp, temperature_celsius)
                 VALUES (?1, 1704067200, 1704067200, 1.0), (?1, 1706745600, 1706745600, 2.0)",
                [&s.id],
            )
            .unwrap();
            Db::migrate(&conn).unwrap();
        }
        verifier_jours(&db, &s.id, "réparation");
        assert_eq!(db.series_days(&s.id).unwrap().len(), 2);
    }

    #[test]
    fn etat_par_mois_suit_ajouts_modifs_et_suppressions() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point = Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();
        let s = series(&point.id, 0);
        db.insert_series_with_measurements(
            &s,
            &[
                measurement(&s.id, "2024-01-31T23:50:00Z", 0, -3.0),
                measurement(&s.id, "2024-02-01T00:00:00Z", 0, -4.0),
                measurement(&s.id, "2024-02-01T00:10:00Z", 0, -4.5),
            ],
        )
        .unwrap();
        let etat = db.series_months(&s.id).unwrap();
        assert_eq!(etat.iter().map(|e| (e.0, e.1)).collect::<Vec<_>>(), vec![(202401, 1), (202402, 2)]);
        assert_eq!(db.list_series_for_point(&point.id).unwrap()[0].measurement_count, 3);

        // Remplacement d'un mois (réception Cloud) : février n'a plus qu'une mesure.
        let v_avant = etat[1].2;
        let v = db
            .replace_month(&s.id, 202402, &[measurement(&s.id, "2024-02-10T12:00:00Z", 0, 5.0)])
            .unwrap();
        assert!(v > v_avant);
        assert_eq!(db.measurements_in_month(&s.id, 202402).unwrap().len(), 1);
        assert_eq!(db.series_months(&s.id).unwrap()[1].1, 1);
        // Janvier n'a pas bougé.
        assert_eq!(db.series_months(&s.id).unwrap()[0], etat[0]);

        // Recalage d'horloge : la mesure de fin janvier passe en février.
        db.recalibrate_series(&s.id, 3600).unwrap();
        let apres: Vec<_> = db.series_months(&s.id).unwrap().iter().map(|e| (e.0, e.1)).collect();
        assert_eq!(apres, vec![(202401, 0), (202402, 2)]);

        // Suppression de la série : plus aucun état.
        db.delete_series(&s.id).unwrap();
        assert!(db.series_months(&s.id).unwrap().is_empty());
    }

    #[test]
    fn etat_par_mois_se_repare_apres_un_import_sans_trigger() {
        let db = db();
        let projet = db.create_project("France").unwrap();
        let point = Point::from_new(new_point(&projet.id, "Station", "Cat1", None)).unwrap();
        db.insert_point(&point).unwrap();
        let s = series(&point.id, 0);
        db.insert_series_with_measurements(&s, &[]).unwrap();
        {
            // Ce que fait le script d'import : trigger retiré, insertion directe.
            let conn = db.conn_pour_test();
            conn.execute_batch("DROP TRIGGER trg_mesures_ajout").unwrap();
            conn.execute(
                "INSERT INTO measurements (series_id, timestamp_utc, raw_timestamp, temperature_celsius)
                 VALUES (?1, 1704067200, 1704067200, 1.0), (?1, 1706745600, 1706745600, 2.0)",
                [&s.id],
            )
            .unwrap();
        }
        assert!(db.series_months(&s.id).unwrap().is_empty());
        // Réouverture de l'app : trigger remis, état reconstruit.
        Db::migrate(&db.conn_pour_test()).unwrap();
        let etat: Vec<_> = db.series_months(&s.id).unwrap().iter().map(|e| (e.0, e.1)).collect();
        assert_eq!(etat, vec![(202401, 1), (202402, 1)]);
        db.insert_series_with_measurements(&series(&point.id, 0), &[]).unwrap();
        db.replace_month(&s.id, 202403, &[measurement(&s.id, "2024-03-02T00:00:00Z", 0, 3.0)]).unwrap();
        assert_eq!(db.series_months(&s.id).unwrap().len(), 3, "trigger d'ajout de nouveau actif");
    }

    #[test]
    fn import_serie_et_relecture() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point = Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        // Logger réglé en UTC+1 → offset -3600.
        let s = series(&point.id, -3600);
        let mesures = vec![
            measurement(&s.id, "2024-01-05T07:00:00Z", -3600, -12.5),
            measurement(&s.id, "2024-01-05T07:10:00Z", -3600, -13.1),
        ];
        db.insert_series_with_measurements(&s, &mesures).unwrap();

        let series_lues = db.list_series_for_point(&point.id).unwrap();
        assert_eq!(series_lues.len(), 1);
        assert_eq!(series_lues[0].measurement_count, 2);
        assert_eq!(series_lues[0].role, SeriesRole::Primary);
        assert_eq!(series_lues[0].source_type, SourceType::ElitechImport);
        assert_eq!(series_lues[0].clock_offset_seconds, -3600);
        assert!(!series_lues[0].raw_timestamp_was_utc);
        assert_eq!(series_lues[0].sample_interval_seconds, Some(600));

        let lues = db.measurements_for_series(&s.id).unwrap();
        assert_eq!(lues.len(), 2);
        assert!(lues[0].id > 0);
        // L'invariant du §1.4 : le stocké est corrigé, le brut est conservé.
        assert_eq!(
            lues[0].timestamp_utc,
            "2024-01-05T06:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            lues[0].raw_timestamp,
            "2024-01-05T07:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(lues[0].temperature_celsius, -12.5);
        assert_eq!(lues[0].humidity_percent, Some(85.0));

        assert_eq!(db.list_points(&projet.id).unwrap()[0].series_count, 1);
        assert_eq!(db.measurements_for_point(&point.id).unwrap().len(), 2);
    }

    #[test]
    fn recalibrage_repart_toujours_du_brut() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point = Point::from_new(new_point(&projet.id, "Losence", "Cat3", Some("Pt"))).unwrap();
        db.insert_point(&point).unwrap();

        let s = series(&point.id, -3600);
        let mesures = vec![measurement(&s.id, "2024-01-05T07:00:00Z", -3600, -12.5)];
        db.insert_series_with_measurements(&s, &mesures).unwrap();

        // On découvre que le logger était en fait réglé en UTC+2.
        let n = db.recalibrate_series(&s.id, -7200).unwrap();
        assert_eq!(n, 1);

        let lues = db.measurements_for_series(&s.id).unwrap();
        assert_eq!(
            lues[0].timestamp_utc,
            "2024-01-05T05:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            "le recalibrage doit repartir du brut, pas empiler les corrections"
        );
        assert_eq!(
            lues[0].raw_timestamp,
            "2024-01-05T07:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(db.get_series(&s.id).unwrap().clock_offset_seconds, -7200);

        // Retour à zéro : on retombe exactement sur le brut.
        db.recalibrate_series(&s.id, 0).unwrap();
        let lues = db.measurements_for_series(&s.id).unwrap();
        assert_eq!(lues[0].timestamp_utc, lues[0].raw_timestamp);

        assert!(db.recalibrate_series("inexistant", 0).is_err());
    }

    #[test]
    fn les_series_ne_sont_jamais_fusionnees() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point = Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let principal = series(&point.id, 0);
        let mut secours = series(&point.id, 0);
        secours.role = SeriesRole::Backup;

        db.insert_series_with_measurements(
            &principal,
            &[measurement(&principal.id, "2024-01-05T07:00:00Z", 0, -12.5)],
        )
        .unwrap();
        db.insert_series_with_measurements(
            &secours,
            &[measurement(&secours.id, "2024-01-05T07:00:00Z", 0, -12.2)],
        )
        .unwrap();

        let series_lues = db.list_series_for_point(&point.id).unwrap();
        assert_eq!(series_lues.len(), 2, "deux loggers = deux séries distinctes");
        assert_eq!(db.measurements_for_series(&principal.id).unwrap().len(), 1);
        assert_eq!(db.measurements_for_series(&secours.id).unwrap().len(), 1);
    }

    #[test]
    fn cle_etrangere_respectee() {
        let db = db();
        let orpheline = series("point-inexistant", 0);
        assert!(db
            .insert_series_with_measurements(&orpheline, &[])
            .is_err());
    }

    #[test]
    fn migration_idempotente() {
        let db = db();
        let version: i64 = db
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        Db::migrate(&db.conn()).unwrap();
    }

    // -----------------------------------------------------------------------
    // Modification d'un point
    // -----------------------------------------------------------------------

    #[test]
    fn mettre_a_jour_un_point_ecrit_les_nouveaux_champs() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point =
            Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let mut modifie = new_point(&projet.id, "Darbounouse (fond)", "Cat4", Some("Pt"));
        modifie.latitude = 45.0123;
        modifie.altitude_m = Some(1420.0);
        modifie.notes = Some("doline profonde".into());
        let modifie = Point::from_parts(point.id.clone(), point.created_at, modifie).unwrap();
        db.update_point(&modifie).unwrap();

        let relu = db.get_point(&point.id).unwrap();
        assert_eq!(relu.name, "Darbounouse (fond)");
        assert_eq!(relu.category, Category::Cat4);
        assert_eq!(relu.size, Some(PointSize::Pt));
        assert_eq!(relu.latitude, 45.0123);
        assert_eq!(relu.altitude_m, Some(1420.0));
        assert_eq!(relu.notes.as_deref(), Some("doline profonde"));
    }

    #[test]
    fn mettre_a_jour_un_point_ne_change_ni_projet_ni_date_de_creation() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point =
            Point::from_new(new_point(&projet.id, "Losence", "Cat3", Some("Pt"))).unwrap();
        db.insert_point(&point).unwrap();

        let modifie =
            Point::from_parts(point.id.clone(), point.created_at, new_point(&projet.id, "Losence 2", "Cat3", Some("Gr")))
                .unwrap();
        db.update_point(&modifie).unwrap();

        let relu = db.get_point(&point.id).unwrap();
        assert_eq!(relu.project_id, projet.id);
        assert_eq!(relu.created_at, point.created_at);
        assert_eq!(relu.id, point.id);
    }

    #[test]
    fn mettre_a_jour_un_point_inexistant_est_une_erreur() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let fantome =
            Point::from_new(new_point(&projet.id, "Nulle part", "Cat1", None)).unwrap();
        assert!(db.update_point(&fantome).is_err());
    }

    /// La règle catégorie/taille doit être rejouée à la modification, pas
    /// seulement à la création.
    #[test]
    fn mettre_a_jour_revalide_la_regle_categorie_taille() {
        assert!(matches!(
            Point::from_parts(
                "id".into(),
                Utc::now(),
                new_point("p", "X", "Cat3", None)
            ),
            Err(crate::model::ValidationError::SizeRequired { .. })
        ));
        assert!(Point::from_parts(
            "id".into(),
            Utc::now(),
            new_point("p", "X", "Cat1", Some("Gr"))
        )
        .is_err());

        let mut hors_bornes = new_point("p", "X", "Cat3", Some("Gr"));
        hors_bornes.latitude = 91.0;
        assert!(Point::from_parts("id".into(), Utc::now(), hors_bornes).is_err());
    }

    // -----------------------------------------------------------------------
    // Ordre jour/mois
    // -----------------------------------------------------------------------

    #[test]
    fn la_migration_v2_ajoute_les_colonnes_de_date() {
        let db = db();
        let version: i64 = db
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        // La v2 n'est plus la dernière : ce qui doit rester vrai, c'est qu'une
        // base neuve est au moins à la v2 — pas qu'elle s'y arrête.
        assert!(version >= 2, "les colonnes de date arrivent avec la v2");

        // Les colonnes doivent exister et se relire via le mapper.
        let projet = db.create_project("Vercors").unwrap();
        let point =
            Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let mut s = series(&point.id, 0);
        s.date_order = DateOrder::MonthFirst;
        s.date_format_uncertain = true;
        db.insert_series_with_measurements(&s, &[]).unwrap();

        let relue = db.get_series(&s.id).unwrap();
        assert_eq!(relue.date_order, DateOrder::MonthFirst);
        assert!(relue.date_format_uncertain);
    }

    /// Prépare une série d'une seule mesure, datée du 5 janvier, lue « jour d'abord ».
    fn serie_ambigue(db: &Db, offset: i64) -> (String, String) {
        let projet = db.create_project("Vercors").unwrap();
        let point =
            Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let mut s = series(&point.id, offset);
        s.date_format_uncertain = true;
        // 5 janvier 07:00 en brut.
        let m = measurement(&s.id, "2024-01-05T07:00:00Z", offset, -12.5);
        db.insert_series_with_measurements(&s, &[m]).unwrap();
        (point.id, s.id)
    }

    #[test]
    fn permuter_les_dates_recalcule_le_brut_et_l_utc() {
        let db = db();
        // Logger réglé en UTC+1 : l'offset doit survivre à la permutation.
        let (_, serie_id) = serie_ambigue(&db, -3600);

        let n = db.swap_series_date_order(&serie_id).unwrap();
        assert_eq!(n, 1);

        let lues = db.measurements_for_series(&serie_id).unwrap();
        // 05/01 lu « jour d'abord » = 5 janvier ; permuté = 1er mai.
        assert_eq!(
            lues[0].raw_timestamp,
            "2024-05-01T07:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            "le brut lui-même portait la date fausse : il doit être réécrit"
        );
        assert_eq!(
            lues[0].timestamp_utc,
            "2024-05-01T06:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            "timestamp_utc doit être re-dérivé du nouveau brut, offset conservé"
        );
        // L'invariant du §1.4 tient toujours.
        assert_eq!(
            lues[0].timestamp_utc.timestamp() - lues[0].raw_timestamp.timestamp(),
            -3600
        );
    }

    #[test]
    fn permuter_les_dates_efface_le_drapeau_et_inverse_l_ordre() {
        let db = db();
        let (_, serie_id) = serie_ambigue(&db, 0);
        assert!(db.get_series(&serie_id).unwrap().date_format_uncertain);

        db.swap_series_date_order(&serie_id).unwrap();

        let s = db.get_series(&serie_id).unwrap();
        assert_eq!(s.date_order, DateOrder::MonthFirst, "l'ordre doit s'inverser");
        assert!(!s.date_format_uncertain, "le doute est levé");

        // Involutive : une seconde permutation ramène à l'état initial.
        db.swap_series_date_order(&serie_id).unwrap();
        let lues = db.measurements_for_series(&serie_id).unwrap();
        assert_eq!(
            lues[0].raw_timestamp,
            "2024-01-05T07:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(db.get_series(&serie_id).unwrap().date_order, DateOrder::DayFirst);
    }

    /// Une série dont une date a un jour > 12 n'est pas permutable. L'échec doit
    /// être total : mieux vaut ne rien faire qu'une permutation partielle.
    #[test]
    fn permuter_refuse_une_serie_impossible_sans_rien_ecrire() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point =
            Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let s = series(&point.id, 0);
        let mesures = vec![
            measurement(&s.id, "2024-01-05T07:00:00Z", 0, -12.5), // permutable
            measurement(&s.id, "2024-01-25T07:00:00Z", 0, -13.1), // jour 25 : impossible
        ];
        db.insert_series_with_measurements(&s, &mesures).unwrap();

        let err = db.swap_series_date_order(&s.id).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("jour supérieur à 12"),
            "message peu explicite : {message}"
        );

        // Atomicité : la mesure permutable ne doit pas avoir bougé non plus.
        let lues = db.measurements_for_series(&s.id).unwrap();
        assert_eq!(
            lues[0].raw_timestamp,
            "2024-01-05T07:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            "aucune écriture ne doit avoir eu lieu"
        );
        assert_eq!(db.get_series(&s.id).unwrap().date_order, DateOrder::DayFirst);
    }

    #[test]
    fn confirmer_l_ordre_ne_touche_pas_aux_mesures() {
        let db = db();
        let (_, serie_id) = serie_ambigue(&db, -3600);

        db.confirm_series_date_order(&serie_id).unwrap();

        let s = db.get_series(&serie_id).unwrap();
        assert!(!s.date_format_uncertain, "le doute est levé");
        assert_eq!(s.date_order, DateOrder::DayFirst, "l'ordre ne change pas");

        let lues = db.measurements_for_series(&serie_id).unwrap();
        assert_eq!(
            lues[0].raw_timestamp,
            "2024-01-05T07:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            "confirmer ne recalcule rien"
        );

        assert!(db.confirm_series_date_order("inexistante").is_err());
        assert!(db.swap_series_date_order("inexistante").is_err());
    }

    // -----------------------------------------------------------------------
    // Migration v3 — colonnes techniques, licence, unicité
    // -----------------------------------------------------------------------

    #[test]
    fn la_migration_v3_ajoute_les_colonnes_de_l_api() {
        let db = db();
        let version: i64 = db
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        // Une base neuve rejoue toutes les migrations : elle est à la version
        // courante, pas à 3. Ce que ce test garde, c'est que les colonnes
        // apportées par la v3 survivent aux migrations suivantes.
        assert_eq!(version, SCHEMA_VERSION);

        let projet = db.create_project("Vercors").unwrap();
        let mut brut = new_point(&projet.id, "Moucherolle", "Cat4", Some("Gr"));
        brut.infoclimat_station_id = Some("STATIC0479".into());
        let point = Point::from_new(brut).unwrap();
        db.insert_point(&point).unwrap();
        assert_eq!(
            db.get_point(&point.id)
                .unwrap()
                .infoclimat_station_id
                .as_deref(),
            Some("STATIC0479")
        );

        let mut s = series(&point.id, 0);
        s.source_type = SourceType::InfoclimatApi;
        s.data_license = Some("NON-COMMERCIAL ONLY: CC BY NC".into());
        s.station_id = Some("STATIC0479".into());
        s.sample_interval_seconds = Some(600);
        let quand: DateTime<Utc> = "2026-08-19T06:00:00Z".parse().unwrap();
        s.last_api_fetch_at = Some(quand);

        let mut m = measurement(&s.id, "2026-08-18T21:40:00Z", 0, 12.3);
        m.rssi_dbm = Some(-86.0);
        m.battery = Some(40.44);
        db.insert_series_with_measurements(&s, &[m]).unwrap();

        let relue = db.get_series(&s.id).unwrap();
        assert_eq!(
            relue.data_license.as_deref(),
            Some("NON-COMMERCIAL ONLY: CC BY NC")
        );
        assert_eq!(relue.station_id.as_deref(), Some("STATIC0479"));
        assert_eq!(relue.last_api_fetch_at, Some(quand));
        assert_eq!(relue.sample_interval_seconds, Some(600));

        let lues = db.measurements_for_series(&s.id).unwrap();
        assert_eq!(lues[0].rssi_dbm, Some(-86.0));
        assert_eq!(
            lues[0].battery,
            Some(40.44),
            "la batterie est stockee BRUTE, sans conversion"
        );
    }

    /// Piste mémoire n°1 : relues depuis la base, les mesures de deux séries
    /// entrelacées gardent chacune leur identifiant, et toutes celles d'une
    /// même série partagent **le même** `Arc<str>` (une allocation par série).
    /// La forme JSON (IPC, fichiers) reste une simple chaîne.
    #[test]
    fn relecture_partage_l_identifiant_de_serie() {
        let db = Db::open_in_memory().unwrap();
        let projet = db.create_project("P").unwrap();
        let point = Point::from_new(new_point(&projet.id, "A", "Cat1", None)).unwrap();
        db.insert_point(&point).unwrap();
        let (a, b) = (series(&point.id, 0), series(&point.id, 0));
        let t0 = "2026-01-05T00:00:00Z";
        let mut ma = Vec::new();
        let mut mb = Vec::new();
        for i in 0..50_i64 {
            let raw: DateTime<Utc> = t0.parse::<DateTime<Utc>>().unwrap() + chrono::Duration::minutes(10 * i);
            ma.push(Measurement::new(a.id.as_str(), raw, raw, i as f64));
            let raw_b = raw + chrono::Duration::minutes(5);
            mb.push(Measurement::new(b.id.as_str(), raw_b, raw_b, -(i as f64)));
        }
        db.insert_series_with_measurements(&a, &ma).unwrap();
        db.insert_series_with_measurements(&b, &mb).unwrap();

        let lues = db.measurements_for_point(&point.id).unwrap();
        assert_eq!(lues.len(), 100);
        let (de_a, de_b): (Vec<_>, Vec<_>) = lues.iter().partition(|m| &*m.series_id == a.id.as_str());
        assert_eq!(de_a.len(), 50);
        assert!(de_b.iter().all(|m| &*m.series_id == b.id.as_str()));
        assert!(de_a.iter().all(|m| Arc::ptr_eq(&m.series_id, &de_a[0].series_id)));
        assert!(de_b.iter().all(|m| Arc::ptr_eq(&m.series_id, &de_b[0].series_id)));
        // Entrelacées : A, B, A, B… (triées par instant).
        assert_eq!(&*lues[0].series_id, a.id.as_str());
        assert_eq!(&*lues[1].series_id, b.id.as_str());

        let json = serde_json::to_value(&lues[0]).unwrap();
        assert_eq!(json["series_id"], serde_json::Value::String(a.id.clone()));
        let retour: Measurement = serde_json::from_value(json).unwrap();
        assert_eq!(retour, lues[0]);
    }

    /// Piste mémoire n°4 : les pragmas effectivement appliqués à une base
    /// fichier. Sur poste fixe : 16 Mo de cache, 256 Mo de mmap ;
    /// les valeurs réduites ne s'appliquent que sous `target_os = "android"`.
    #[test]
    fn pragmas_memoire_appliques_selon_la_cible() {
        let fichier = std::env::temp_dir().join(format!("gws-pragma-{}.sqlite3", Uuid::new_v4()));
        {
            let db = Db::open(&fichier).unwrap();
            let conn = db.conn.lock().unwrap();
            let cache: i64 = conn.query_row("PRAGMA cache_size", [], |r| r.get(0)).unwrap();
            let mmap: i64 = conn.query_row("PRAGMA mmap_size", [], |r| r.get(0)).unwrap();
            assert_eq!(cache, CACHE_SIZE_KIB);
            assert_eq!(mmap, MMAP_SIZE_BYTES);
            #[cfg(not(target_os = "android"))]
            {
                assert_eq!(cache, -16_384);
                assert_eq!(mmap, 268_435_456);
            }
        }
        for suffixe in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffixe}", fichier.display()));
        }
    }

    /// Une base deja remplie doit migrer sans rien perdre — et ses eventuels
    /// doublons doivent etre resorbes, sans quoi l'index unique ne pourrait pas
    /// etre cree.
    #[test]
    fn une_base_v2_avec_doublons_migre_proprement() {
        let fichier = std::env::temp_dir().join(format!("gws-migr-{}.sqlite3", Uuid::new_v4()));

        // --- Etape 1 : fabriquer une base *v2* (schema v1 + v2, rien de plus).
        let (point_id, serie_id) = {
            let conn = Connection::open(&fichier).unwrap();
            conn.pragma_update(None, "foreign_keys", true).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();

            let projet_id = Uuid::new_v4().to_string();
            let point_id = Uuid::new_v4().to_string();
            let serie_id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO projects (id, name, created_at, last_opened_at)
                 VALUES (?1, 'Vercors', 0, 0)",
                [&projet_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO points (id, project_id, name, latitude, longitude,
                                     category, size, created_at)
                 VALUES (?1, ?2, 'Darbounouse', 44.98, 5.42, 'Cat3', 'Gr', 0)",
                params![point_id, projet_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO logger_series (id, point_id, source_type, role, imported_at)
                 VALUES (?1, ?2, 'ElitechImport', 'Primary', 0)",
                params![serie_id, point_id],
            )
            .unwrap();

            // Trois mesures dont DEUX au meme instant : c'est exactement ce que
            // l'index unique ne tolerera pas.
            for (ts, temp) in [(1000_i64, -12.5_f64), (1000, -12.5), (1600, -13.1)] {
                conn.execute(
                    "INSERT INTO measurements
                        (series_id, timestamp_utc, raw_timestamp, temperature_celsius)
                     VALUES (?1, ?2, ?2, ?3)",
                    params![serie_id, ts, temp],
                )
                .unwrap();
            }
            (point_id, serie_id)
        };

        // --- Etape 2 : ouvrir la base avec le code courant → migration v2 → v3.
        {
            let db = Db::open(&fichier).unwrap();
            let version: i64 = db
                .conn()
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap();
            assert_eq!(version, SCHEMA_VERSION);

            // Rien de l'existant n'a disparu…
            assert_eq!(db.get_point(&point_id).unwrap().name, "Darbounouse");
            // …sauf le doublon.
            let mesures = db.measurements_for_series(&serie_id).unwrap();
            assert_eq!(mesures.len(), 2, "le doublon doit avoir ete resorbe");
            assert_eq!(mesures[0].timestamp_utc.timestamp(), 1000);
            assert_eq!(mesures[1].timestamp_utc.timestamp(), 1600);

            // Les nouvelles colonnes existent et valent NULL sur l'existant.
            assert_eq!(mesures[0].rssi_dbm, None);
            assert_eq!(db.get_series(&serie_id).unwrap().data_license, None);
            // Idem pour l'empreinte de la v4 : on ne peut pas la recalculer sans
            // le fichier d'origine, cette série reste donc hors détection.
            assert_eq!(db.get_series(&serie_id).unwrap().content_fingerprint, None);

            // Et la contrainte est bien en place desormais.
            let conflit = db.conn().execute(
                "INSERT INTO measurements
                    (series_id, timestamp_utc, raw_timestamp, temperature_celsius)
                 VALUES (?1, 1600, 1600, 0.0)",
                [&serie_id],
            );
            assert!(
                conflit.is_err(),
                "UNIQUE(series_id, timestamp_utc) doit rejeter ce doublon"
            );
        }

        // --- Etape 3 : rouvrir → la migration est idempotente.
        {
            let db = Db::open(&fichier).unwrap();
            assert_eq!(db.measurements_for_series(&serie_id).unwrap().len(), 2);
        }

        let _ = std::fs::remove_file(&fichier);
    }

    // -----------------------------------------------------------------------
    // Migration v4 — empreinte de contenu
    // -----------------------------------------------------------------------

    #[test]
    fn l_empreinte_est_stockee_et_retrouvee_par_point() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let darbounouse =
            Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        let derbenouse =
            Point::from_new(new_point(&projet.id, "Derbenouse", "Cat2", None)).unwrap();
        db.insert_point(&darbounouse).unwrap();
        db.insert_point(&derbenouse).unwrap();

        let mut s = series(&darbounouse.id, 0);
        s.content_fingerprint = Some("abc123".into());
        db.insert_series_with_measurements(
            &s,
            &[
                measurement(&s.id, "2026-01-05T06:00:00Z", 0, -12.5),
                measurement(&s.id, "2026-01-05T06:10:00Z", 0, -12.9),
            ],
        )
        .unwrap();

        assert_eq!(
            db.get_series(&s.id).unwrap().content_fingerprint.as_deref(),
            Some("abc123")
        );

        let trouvee = db
            .find_series_by_fingerprint(&darbounouse.id, "abc123")
            .unwrap()
            .expect("l'empreinte doit être retrouvée sur son point");
        assert_eq!(trouvee.id, s.id);
        assert_eq!(trouvee.measurement_count, 2);

        // La recherche est bornée au point : le même contenu sur un site voisin
        // n'est pas un ré-import, c'est deux loggers qui mesurent la même chose.
        assert!(db
            .find_series_by_fingerprint(&derbenouse.id, "abc123")
            .unwrap()
            .is_none());
        assert!(db
            .find_series_by_fingerprint(&darbounouse.id, "autre")
            .unwrap()
            .is_none());

        let (debut, fin) = db.series_bounds_utc(&s.id).unwrap();
        assert_eq!(
            debut.unwrap(),
            "2026-01-05T06:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            fin.unwrap(),
            "2026-01-05T06:10:00Z".parse::<DateTime<Utc>>().unwrap()
        );

        // Une série sans mesure n'a pas de bornes — et ne doit pas paniquer.
        let vide = series(&darbounouse.id, 0);
        db.insert_series_with_measurements(&vide, &[]).unwrap();
        assert_eq!(db.series_bounds_utc(&vide.id).unwrap(), (None, None));
    }

    /// Une empreinte `NULL` ne doit jamais se comparer à quoi que ce soit — pas
    /// même à une autre empreinte `NULL`. En SQL c'est acquis (`= NULL` ne vaut
    /// jamais vrai), mais c'est exactement le comportement dont dépend le fait
    /// que les séries d'avant la v4 restent hors détection : autant le fixer.
    #[test]
    fn une_empreinte_absente_ne_correspond_a_rien() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point = Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let s = series(&point.id, 0); // content_fingerprint: None
        db.insert_series_with_measurements(&s, &[measurement(&s.id, "2026-01-05T06:00:00Z", 0, -1.0)])
            .unwrap();

        assert!(db.get_series(&s.id).unwrap().content_fingerprint.is_none());
        assert!(db
            .find_series_by_fingerprint(&point.id, "")
            .unwrap()
            .is_none());
    }

    /// Deux series **distinctes** peuvent porter le meme instant : l'unicite est
    /// bien locale a une serie, elle n'a pas transforme les series en une seule.
    #[test]
    fn l_unicite_ne_deborde_pas_d_une_serie_sur_l_autre() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point =
            Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let a = series(&point.id, 0);
        let b = series(&point.id, 0);
        db.insert_series_with_measurements(
            &a,
            &[measurement(&a.id, "2026-01-05T07:00:00Z", 0, -12.5)],
        )
        .unwrap();
        db.insert_series_with_measurements(
            &b,
            &[measurement(&b.id, "2026-01-05T07:00:00Z", 0, -12.2)],
        )
        .unwrap();

        assert_eq!(db.measurements_for_series(&a.id).unwrap().len(), 1);
        assert_eq!(db.measurements_for_series(&b.id).unwrap().len(), 1);
    }

    /// Un fichier qui repete un horodatage ne fait pas echouer l'import : les
    /// lignes en trop sont ecartees et l'appelant en est informe par l'ecart
    /// entre le nombre fourni et le nombre ecrit.
    #[test]
    fn un_import_avec_horodatage_repete_ecarte_le_doublon_sans_planter() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point =
            Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let s = series(&point.id, 0);
        let mesures = vec![
            measurement(&s.id, "2026-01-05T07:00:00Z", 0, -12.5),
            measurement(&s.id, "2026-01-05T07:00:00Z", 0, -99.9), // meme instant
            measurement(&s.id, "2026-01-05T07:10:00Z", 0, -13.1),
        ];
        let ecrites = db.insert_series_with_measurements(&s, &mesures).unwrap();

        assert_eq!(ecrites, 2, "la ligne en double n'est pas ecrite");
        let lues = db.measurements_for_series(&s.id).unwrap();
        assert_eq!(lues.len(), 2);
        assert_eq!(
            lues[0].temperature_celsius, -12.5,
            "la PREMIERE valeur est conservee"
        );
    }

    // -----------------------------------------------------------------------
    // Ajout de mesures a une serie existante (mode « appel direct »)
    // -----------------------------------------------------------------------

    fn serie_api(db: &Db) -> (String, String) {
        let projet = db.create_project("Vercors").unwrap();
        let point =
            Point::from_new(new_point(&projet.id, "Moucherolle", "Cat4", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let mut s = series(&point.id, 0);
        s.source_type = SourceType::InfoclimatApi;
        s.station_id = Some("STATIC0479".into());
        db.insert_series_with_measurements(
            &s,
            &[
                measurement(&s.id, "2026-08-18T21:20:00Z", 0, 12.0),
                measurement(&s.id, "2026-08-18T21:30:00Z", 0, 11.8),
            ],
        )
        .unwrap();
        (point.id, s.id)
    }

    /// Le cas normal d'un rafraichissement : la periode redemandee recouvre
    /// largement ce qu'on a deja. Rien ne doit etre duplique.
    #[test]
    fn un_rappel_d_api_qui_recouvre_n_ajoute_que_le_nouveau() {
        let db = db();
        let (_, serie_id) = serie_api(&db);

        let lot = vec![
            measurement(&serie_id, "2026-08-18T21:20:00Z", 0, 12.0), // deja connu
            measurement(&serie_id, "2026-08-18T21:30:00Z", 0, 11.8), // deja connu
            measurement(&serie_id, "2026-08-18T21:40:00Z", 0, 11.5), // nouveau
        ];
        let bilan = db.append_measurements(&serie_id, &lot).unwrap();

        assert_eq!(bilan.inserted, 1);
        assert_eq!(bilan.updated, 0);
        assert_eq!(bilan.unchanged, 2);
        assert_eq!(bilan.total(), 3);
        assert_eq!(db.measurements_for_series(&serie_id).unwrap().len(), 3);
    }

    /// Rejouer **exactement** le meme lot deux fois de suite ne doit rien
    /// changer : c'est le test « pas de doublon silencieux ».
    #[test]
    fn rejouer_le_meme_lot_est_sans_effet() {
        let db = db();
        let (_, serie_id) = serie_api(&db);

        let lot = vec![
            measurement(&serie_id, "2026-08-18T21:20:00Z", 0, 12.0),
            measurement(&serie_id, "2026-08-18T21:40:00Z", 0, 11.5),
        ];
        db.append_measurements(&serie_id, &lot).unwrap();
        let apres_premier = db.measurements_for_series(&serie_id).unwrap();

        let bilan = db.append_measurements(&serie_id, &lot).unwrap();
        assert_eq!(bilan.inserted, 0);
        assert_eq!(bilan.updated, 0);
        assert_eq!(bilan.unchanged, 2);
        assert_eq!(db.measurements_for_series(&serie_id).unwrap(), apres_premier);
    }

    /// Une valeur corrigee par Infoclimat doit ecraser l'ancienne, pas s'ajouter
    /// a cote ni etre ignoree.
    #[test]
    fn une_valeur_corrigee_remplace_l_ancienne() {
        let db = db();
        let (_, serie_id) = serie_api(&db);

        let mut corrigee = measurement(&serie_id, "2026-08-18T21:20:00Z", 0, 12.4);
        corrigee.rssi_dbm = Some(-91.0);
        let bilan = db.append_measurements(&serie_id, &[corrigee]).unwrap();

        assert_eq!(bilan.updated, 1);
        assert_eq!(bilan.inserted, 0);

        let lues = db.measurements_for_series(&serie_id).unwrap();
        assert_eq!(lues.len(), 2, "toujours deux mesures, pas trois");
        assert_eq!(lues[0].temperature_celsius, 12.4);
        assert_eq!(lues[0].rssi_dbm, Some(-91.0));
    }

    #[test]
    fn ajouter_a_une_serie_inexistante_est_une_erreur() {
        let db = db();
        assert!(db.append_measurements("inexistante", &[]).is_err());
    }

    #[test]
    fn horodatage_du_dernier_appel_et_reprise() {
        let db = db();
        let (point_id, serie_id) = serie_api(&db);

        assert_eq!(
            db.last_measurement_utc(&serie_id).unwrap(),
            Some("2026-08-18T21:30:00Z".parse::<DateTime<Utc>>().unwrap())
        );

        let quand: DateTime<Utc> = "2026-08-19T06:00:00Z".parse().unwrap();
        db.mark_series_fetched(&serie_id, quand).unwrap();
        assert_eq!(
            db.get_series(&serie_id).unwrap().last_api_fetch_at,
            Some(quand)
        );
        assert!(db.mark_series_fetched("inexistante", quand).is_err());

        // La serie se retrouve par (point, station) — c'est ce qui evite d'en
        // creer une nouvelle a chaque rafraichissement.
        let trouvee = db.find_infoclimat_series(&point_id, "STATIC0479").unwrap();
        assert_eq!(trouvee.map(|s| s.id), Some(serie_id));
        assert!(db
            .find_infoclimat_series(&point_id, "STATIC9999")
            .unwrap()
            .is_none());
    }

    // -----------------------------------------------------------------------
    // Le garde-fou d'unicite ne doit pas casser les corrections en masse
    // -----------------------------------------------------------------------

    /// Decaler une serie reguliere d'un pas entier fait glisser chaque mesure sur
    /// l'instant de sa voisine. Sans mise en attente prealable des horodatages,
    /// l'index unique ferait echouer la mise a jour a mi-parcours.
    #[test]
    fn recalibrer_d_un_pas_entier_ne_heurte_pas_l_unicite() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point =
            Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let s = series(&point.id, 0);
        let mesures = vec![
            measurement(&s.id, "2026-01-05T06:00:00Z", 0, -12.5),
            measurement(&s.id, "2026-01-05T07:00:00Z", 0, -13.1),
            measurement(&s.id, "2026-01-05T08:00:00Z", 0, -13.4),
        ];
        db.insert_series_with_measurements(&s, &mesures).unwrap();

        // +1 h : 06:00 → 07:00, qui est occupe par la mesure suivante.
        let n = db.recalibrate_series(&s.id, 3600).unwrap();
        assert_eq!(n, 3);

        let lues = db.measurements_for_series(&s.id).unwrap();
        assert_eq!(lues.len(), 3);
        assert_eq!(
            lues[0].timestamp_utc,
            "2026-01-05T07:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            lues[2].timestamp_utc,
            "2026-01-05T09:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        for m in &lues {
            assert_eq!(
                m.timestamp_utc.timestamp() - m.raw_timestamp.timestamp(),
                3600
            );
        }
    }

    /// Deux dates qui s'echangent a la permutation (05/01 ↔ 01/05) : meme piege,
    /// meme parade.
    #[test]
    fn permuter_deux_dates_qui_s_echangent_ne_heurte_pas_l_unicite() {
        let db = db();
        let projet = db.create_project("Vercors").unwrap();
        let point =
            Point::from_new(new_point(&projet.id, "Darbounouse", "Cat3", Some("Gr"))).unwrap();
        db.insert_point(&point).unwrap();

        let s = series(&point.id, 0);
        let mesures = vec![
            measurement(&s.id, "2026-01-05T07:00:00Z", 0, -12.5), // 5 janvier
            measurement(&s.id, "2026-05-01T07:00:00Z", 0, 14.2),  // 1er mai
        ];
        db.insert_series_with_measurements(&s, &mesures).unwrap();

        db.swap_series_date_order(&s.id).unwrap();

        let lues = db.measurements_for_series(&s.id).unwrap();
        assert_eq!(lues.len(), 2, "les deux mesures doivent survivre a l'echange");
        // Elles ont echange leur date, donc leurs temperatures ont change de place.
        assert_eq!(lues[0].temperature_celsius, 14.2);
        assert_eq!(lues[1].temperature_celsius, -12.5);
    }

    // -----------------------------------------------------------------------
    // Reglages
    // -----------------------------------------------------------------------

    #[test]
    fn les_reglages_se_lisent_et_s_ecrasent() {
        let db = db();
        assert_eq!(db.get_setting("inconnu").unwrap(), None);

        db.set_setting("infoclimat_refresh_min_minutes", "60")
            .unwrap();
        assert_eq!(
            db.get_setting("infoclimat_refresh_min_minutes")
                .unwrap()
                .as_deref(),
            Some("60")
        );

        db.set_setting("infoclimat_refresh_min_minutes", "15")
            .unwrap();
        assert_eq!(
            db.get_setting("infoclimat_refresh_min_minutes")
                .unwrap()
                .as_deref(),
            Some("15"),
            "reecrire un reglage doit l'ecraser, pas en creer un second"
        );
    }

    /// **Le test de la migration v5.**
    ///
    /// La v5 est la première à *reconstruire* une table : élargir le `CHECK` de
    /// `logger_series.source_type` n'est pas faisable en place sous SQLite. Trois
    /// choses peuvent mal tourner, et ce test les couvre toutes :
    ///
    /// 1. les mesures pourraient **disparaître par cascade** quand
    ///    `logger_series` est supprimée (`measurements.series_id` la référence
    ///    avec `ON DELETE CASCADE`) ;
    /// 2. les séries pourraient perdre une colonne à la recopie ;
    /// 3. les nouvelles colonnes météo pourraient être créées ailleurs qu'à `NULL`.
    ///
    /// La base de départ est une **v4 complète et peuplée** : deux séries de
    /// sources différentes, plusieurs mesures, tous les champs de la v3 et de la
    /// v4 renseignés.
    #[test]
    fn une_base_v4_migre_vers_la_v5_sans_rien_perdre() {
        let fichier = std::env::temp_dir().join(format!("gws-migr5-{}.sqlite3", Uuid::new_v4()));

        // --- Étape 1 : fabriquer une base v4 peuplée.
        let (point_id, serie_elitech, serie_infoclimat) = {
            let conn = Connection::open(&fichier).unwrap();
            conn.pragma_update(None, "foreign_keys", true).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            conn.execute_batch(SCHEMA_V3).unwrap();
            conn.execute_batch(SCHEMA_V3_UNIQUE).unwrap();
            conn.execute_batch(SCHEMA_V4).unwrap();
            conn.pragma_update(None, "user_version", 4).unwrap();

            let projet_id = Uuid::new_v4().to_string();
            let point_id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO projects (id, name, created_at, last_opened_at)
                 VALUES (?1, 'Vercors', 0, 0)",
                [&projet_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO points (id, project_id, name, latitude, longitude,
                                     category, size, created_at, infoclimat_station_id)
                 VALUES (?1, ?2, 'Darbounouse', 44.98, 5.42, 'Cat3', 'Gr', 0, 'STATIC0213')",
                params![point_id, projet_id],
            )
            .unwrap();

            let serie_elitech = Uuid::new_v4().to_string();
            let serie_infoclimat = Uuid::new_v4().to_string();

            // Une série de fichier, avec décalage d'horloge et empreinte…
            conn.execute(
                "INSERT INTO logger_series (
                    id, point_id, source_type, role, logger_serial, imported_at,
                    raw_file_reference, clock_offset_seconds, raw_timestamp_was_utc,
                    label, sample_interval_seconds, date_order, date_format_uncertain,
                    content_fingerprint)
                 VALUES (?1, ?2, 'ElitechImport', 'Primary', 'EFE205004333', 100,
                         'export.txt', -3600, 0, 'fond de doline', 900, 'DayFirst', 1,
                         'empreinte-abc')",
                params![serie_elitech, point_id],
            )
            .unwrap();
            // …et une série d'API, avec licence, station et dernier appel.
            conn.execute(
                "INSERT INTO logger_series (
                    id, point_id, source_type, role, imported_at,
                    clock_offset_seconds, raw_timestamp_was_utc, date_order,
                    date_format_uncertain, data_license, station_id, last_api_fetch_at)
                 VALUES (?1, ?2, 'InfoclimatApi', 'Backup', 200,
                         0, 1, 'DayFirst', 0, 'CC BY NC', 'STATIC0213', 12345)",
                params![serie_infoclimat, point_id],
            )
            .unwrap();

            for (serie, ts, temp, hum) in [
                (&serie_elitech, 1000_i64, -12.5_f64, Some(88.0_f64)),
                (&serie_elitech, 1600, -13.1, None),
                (&serie_infoclimat, 1000, -9.9, Some(50.0)),
            ] {
                conn.execute(
                    "INSERT INTO measurements
                        (series_id, timestamp_utc, raw_timestamp, temperature_celsius,
                         humidity_percent, rssi_dbm, battery)
                     VALUES (?1, ?2, ?2, ?3, ?4, -94.0, 39.84)",
                    params![serie, ts, temp, hum],
                )
                .unwrap();
            }

            (point_id, serie_elitech, serie_infoclimat)
        };

        // --- Étape 2 : ouvrir avec le code courant → migration v4 → v5.
        {
            let db = Db::open(&fichier).unwrap();
            let version: i64 = db
                .conn()
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap();
            assert_eq!(version, SCHEMA_VERSION);

            // (1) Aucune mesure n'a été emportée par la reconstruction de table.
            let elitech = db.measurements_for_series(&serie_elitech).unwrap();
            let infoclimat = db.measurements_for_series(&serie_infoclimat).unwrap();
            assert_eq!(elitech.len(), 2, "les mesures ne doivent pas suivre le DROP TABLE");
            assert_eq!(infoclimat.len(), 1);

            // (2) Les séries ont gardé **tous** leurs champs à la recopie.
            let s = db.get_series(&serie_elitech).unwrap();
            assert_eq!(s.source_type, SourceType::ElitechImport);
            assert_eq!(s.role, SeriesRole::Primary);
            assert_eq!(s.logger_serial.as_deref(), Some("EFE205004333"));
            assert_eq!(s.raw_file_reference.as_deref(), Some("export.txt"));
            assert_eq!(s.clock_offset_seconds, -3600);
            assert_eq!(s.label.as_deref(), Some("fond de doline"));
            assert_eq!(s.sample_interval_seconds, Some(900));
            assert!(s.date_format_uncertain);
            assert_eq!(s.content_fingerprint.as_deref(), Some("empreinte-abc"));
            assert_eq!(s.measurement_count, 2);

            let api = db.get_series(&serie_infoclimat).unwrap();
            assert_eq!(api.source_type, SourceType::InfoclimatApi);
            assert_eq!(api.data_license.as_deref(), Some("CC BY NC"));
            assert_eq!(api.station_id.as_deref(), Some("STATIC0213"));
            assert_eq!(api.last_api_fetch_at.map(secs), Some(12345));

            // (3) Les six colonnes météo existent et valent NULL sur tout
            // l'existant — c'est la garantie demandée : aucune source antérieure
            // ne publiait ces grandeurs, rien ne doit être inventé.
            for m in elitech.iter().chain(infoclimat.iter()) {
                assert_eq!(m.pressure_hpa, None);
                assert_eq!(m.wind_avg_kmh, None);
                assert_eq!(m.wind_gust_kmh, None);
                assert_eq!(m.wind_direction_degrees, None);
                assert_eq!(m.rain_mm, None);
                assert_eq!(m.solar_radiation, None);
            }
            // Les colonnes de la v3, elles, ont bien conservé leurs valeurs.
            assert_eq!(elitech[0].humidity_percent, Some(88.0));
            assert_eq!(elitech[0].rssi_dbm, Some(-94.0));
            assert_eq!(elitech[0].battery, Some(39.84));

            // Le point a gagné sa colonne ROMMA, à NULL, sans perdre l'autre.
            let point = db.get_point(&point_id).unwrap();
            assert_eq!(point.infoclimat_station_id.as_deref(), Some("STATIC0213"));
            assert_eq!(point.romma_station_id, None);

            // La contrainte élargie accepte désormais `RommaImport`…
            let romma = LoggerSeries {
                id: Uuid::new_v4().to_string(),
                point_id: point_id.clone(),
                source_type: SourceType::RommaImport,
                role: SeriesRole::Primary,
                logger_serial: None,
                imported_at: now_utc(),
                raw_file_reference: Some("archives_Lans.csv".into()),
                clock_offset_seconds: 0,
                raw_timestamp_was_utc: true,
                label: None,
                sample_interval_seconds: Some(600),
                data_license: None,
                station_id: Some("Lans - Les Allières".into()),
                last_api_fetch_at: None,
                date_order: DateOrder::DayFirst,
                date_format_uncertain: false,
                content_fingerprint: Some("empreinte-romma".into()),
                measurement_count: 0,
            };
            let mesure = Measurement {
                pressure_hpa: Some(1021.0),
                wind_avg_kmh: Some(10.0),
                wind_gust_kmh: Some(24.1),
                wind_direction_degrees: Some(225.0),
                rain_mm: Some(0.0),
                solar_radiation: Some(412.0),
                humidity_percent: Some(72.0),
                ..Measurement::new_utc(
                    romma.id.clone(),
                    ts_from_secs(1_496_250_000).unwrap(),
                    12.2,
                )
            };
            assert_eq!(
                db.insert_series_with_measurements(&romma, &[mesure]).unwrap(),
                1
            );

            // …et les six champs se relisent tels qu'ils ont été écrits.
            let relue = &db.measurements_for_series(&romma.id).unwrap()[0];
            assert_eq!(relue.pressure_hpa, Some(1021.0));
            assert_eq!(relue.wind_avg_kmh, Some(10.0));
            assert_eq!(relue.wind_gust_kmh, Some(24.1));
            assert_eq!(relue.wind_direction_degrees, Some(225.0));
            assert_eq!(relue.rain_mm, Some(0.0));
            assert_eq!(relue.solar_radiation, Some(412.0));

            // La contrainte reste stricte pour tout le reste : la reconstruction
            // ne doit pas l'avoir élargie plus que voulu.
            let refuse = db.conn().execute(
                "INSERT INTO logger_series (id, point_id, source_type, role, imported_at)
                 VALUES (?1, ?2, 'MeteoFranceApi', 'Primary', 0)",
                params![Uuid::new_v4().to_string(), point_id],
            );
            assert!(
                refuse.is_err(),
                "le CHECK doit toujours refuser une source inconnue"
            );

            // Les clés étrangères ont été remises en service après la migration :
            // une série orpheline reste impossible.
            let orpheline = db.conn().execute(
                "INSERT INTO logger_series (id, point_id, source_type, role, imported_at)
                 VALUES (?1, 'point-inexistant', 'RommaImport', 'Primary', 0)",
                [Uuid::new_v4().to_string()],
            );
            assert!(
                orpheline.is_err(),
                "PRAGMA foreign_keys doit être réactivé après la migration v5"
            );
        }

        // --- Étape 3 : rouvrir → la migration est idempotente.
        {
            let db = Db::open(&fichier).unwrap();
            assert_eq!(db.measurements_for_series(&serie_elitech).unwrap().len(), 2);
            assert_eq!(
                db.get_series(&serie_elitech).unwrap().content_fingerprint.as_deref(),
                Some("empreinte-abc")
            );
        }

        let _ = std::fs::remove_file(&fichier);
    }

    /// Une base **v1** doit traverser toutes les migrations d'affilée, v5 comprise.
    ///
    /// Le chemin v1 → v5 n'est pas la somme des chemins testés un à un : la
    /// reconstruction de `logger_series` en v5 s'applique à une table qui vient
    /// d'être élargie par v2, v3 et v4 dans la même ouverture.
    #[test]
    fn une_base_v1_traverse_toutes_les_migrations() {
        let fichier = std::env::temp_dir().join(format!("gws-migr1-{}.sqlite3", Uuid::new_v4()));

        let serie_id = {
            let conn = Connection::open(&fichier).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();

            let projet_id = Uuid::new_v4().to_string();
            let point_id = Uuid::new_v4().to_string();
            let serie_id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO projects (id, name, created_at, last_opened_at)
                 VALUES (?1, 'Vercors', 0, 0)",
                [&projet_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO points (id, project_id, name, latitude, longitude,
                                     category, size, created_at)
                 VALUES (?1, ?2, 'Losence', 44.9, 5.4, 'Cat1', NULL, 0)",
                params![point_id, projet_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO logger_series (id, point_id, source_type, role, imported_at)
                 VALUES (?1, ?2, 'Manual', 'Primary', 0)",
                params![serie_id, point_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO measurements
                    (series_id, timestamp_utc, raw_timestamp, temperature_celsius)
                 VALUES (?1, 500, 500, -7.5)",
                [&serie_id],
            )
            .unwrap();
            serie_id
        };

        let db = Db::open(&fichier).unwrap();
        let version: i64 = db
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let mesures = db.measurements_for_series(&serie_id).unwrap();
        assert_eq!(mesures.len(), 1);
        assert_eq!(mesures[0].temperature_celsius, -7.5);
        assert_eq!(mesures[0].solar_radiation, None);
        assert_eq!(db.get_series(&serie_id).unwrap().date_order, DateOrder::DayFirst);

        let _ = std::fs::remove_file(&fichier);
    }

    /// **Le test de la migration v6.**
    ///
    /// La v6 refait la manœuvre coûteuse de la v5 — reconstruire `logger_series`
    /// pour élargir un `CHECK` — au bénéfice d'une seule valeur : `'ManualEntry'`.
    /// Les mêmes trois choses peuvent mal tourner, et sont vérifiées ici :
    ///
    /// 1. les mesures pourraient disparaître par cascade au `DROP TABLE` ;
    /// 2. les séries pourraient perdre un champ à la recopie ;
    /// 3. la contrainte pourrait être élargie **plus que voulu**, et laisser
    ///    entrer n'importe quelle chaîne comme source.
    ///
    /// La base de départ est une **v5 peuplée**, c'est-à-dire l'état exact d'une
    /// base créée par la version précédente de l'application.
    #[test]
    fn une_base_v5_migre_vers_la_v6_et_accepte_la_saisie_manuelle() {
        let fichier = std::env::temp_dir().join(format!("gws-migr6-{}.sqlite3", Uuid::new_v4()));

        // --- Étape 1 : fabriquer une base v5 peuplée.
        let (point_id, serie_id) = {
            let conn = Connection::open(&fichier).unwrap();
            conn.pragma_update(None, "foreign_keys", true).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            conn.execute_batch(SCHEMA_V3).unwrap();
            conn.execute_batch(SCHEMA_V3_UNIQUE).unwrap();
            conn.execute_batch(SCHEMA_V4).unwrap();
            conn.pragma_update(None, "foreign_keys", false).unwrap();
            conn.execute_batch(SCHEMA_V5).unwrap();
            conn.pragma_update(None, "foreign_keys", true).unwrap();
            conn.pragma_update(None, "user_version", 5).unwrap();

            let projet_id = Uuid::new_v4().to_string();
            let point_id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO projects (id, name, created_at, last_opened_at)
                 VALUES (?1, 'Vercors', 0, 0)",
                [&projet_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO points (id, project_id, name, latitude, longitude,
                                     category, size, created_at)
                 VALUES (?1, ?2, 'Darbounouse', 44.98, 5.42, 'Cat3', 'Gr', 0)",
                params![point_id, projet_id],
            )
            .unwrap();

            let serie_id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO logger_series (
                    id, point_id, source_type, role, logger_serial, imported_at,
                    raw_file_reference, clock_offset_seconds, raw_timestamp_was_utc,
                    label, sample_interval_seconds, date_order, date_format_uncertain,
                    content_fingerprint)
                 VALUES (?1, ?2, 'ElitechDirect', 'Primary', 'E205004333', 100,
                         'Lecture directe RC-5 - COM8', -3600, 0, 'fond de doline',
                         900, 'DayFirst', 1, 'empreinte-abc')",
                params![serie_id, point_id],
            )
            .unwrap();
            for (ts, temp) in [(1000_i64, -12.5_f64), (1600, -13.1)] {
                conn.execute(
                    "INSERT INTO measurements
                        (series_id, timestamp_utc, raw_timestamp, temperature_celsius)
                     VALUES (?1, ?2, ?2, ?3)",
                    params![serie_id, ts, temp],
                )
                .unwrap();
            }

            (point_id, serie_id)
        };

        // --- Étape 2 : ouvrir avec le code courant → migration v5 → v6.
        let db = Db::open(&fichier).unwrap();
        let version: i64 = db
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        // (1) Les mesures ont survécu au DROP TABLE.
        assert_eq!(db.measurements_for_series(&serie_id).unwrap().len(), 2);

        // (2) La série a gardé tous ses champs.
        let s = db.get_series(&serie_id).unwrap();
        assert_eq!(s.source_type, SourceType::ElitechDirect);
        assert_eq!(s.logger_serial.as_deref(), Some("E205004333"));
        assert_eq!(s.clock_offset_seconds, -3600);
        assert_eq!(s.label.as_deref(), Some("fond de doline"));
        assert_eq!(s.sample_interval_seconds, Some(900));
        assert!(s.date_format_uncertain);
        assert_eq!(s.content_fingerprint.as_deref(), Some("empreinte-abc"));
        assert_eq!(s.measurement_count, 2);

        // (3) La contrainte élargie accepte `ManualEntry`…
        let saisie = LoggerSeries {
            id: Uuid::new_v4().to_string(),
            point_id: point_id.clone(),
            source_type: SourceType::ManualEntry,
            role: SeriesRole::Primary,
            logger_serial: None,
            imported_at: now_utc(),
            raw_file_reference: Some("Saisie manuelle - minimum sur 06:00-09:00".into()),
            clock_offset_seconds: -3600,
            raw_timestamp_was_utc: false,
            label: Some("Saisie manuelle - minimum sur 06:00-09:00".into()),
            sample_interval_seconds: Some(10_800),
            data_license: None,
            station_id: None,
            last_api_fetch_at: None,
            date_order: DateOrder::DayFirst,
            date_format_uncertain: false,
            content_fingerprint: Some("empreinte-saisie".into()),
            measurement_count: 0,
        };
        let mesure = Measurement::new_utc(
            saisie.id.clone(),
            ts_from_secs(1_768_372_200).unwrap(),
            -4.2,
        );
        assert_eq!(
            db.insert_series_with_measurements(&saisie, &[mesure]).unwrap(),
            1
        );

        let relue = db.get_series(&saisie.id).unwrap();
        assert_eq!(relue.source_type, SourceType::ManualEntry);
        assert!(relue.source_type.is_manual_entry());
        // …et `Manual` reste ce qu'il était : un fichier à colonnes mappées.
        assert!(!SourceType::Manual.is_manual_entry());

        // …sans que la contrainte se soit relâchée pour autant.
        let conn = db.conn();
        assert!(
            conn.execute(
                "INSERT INTO logger_series (id, point_id, source_type, role, imported_at)
                 VALUES ('x', ?1, 'SaisieManuelle', 'Primary', 0)",
                [&point_id],
            )
            .is_err(),
            "le CHECK doit toujours refuser une source inconnue"
        );
        drop(conn);
        drop(db);
        let _ = std::fs::remove_file(&fichier);
    }

}

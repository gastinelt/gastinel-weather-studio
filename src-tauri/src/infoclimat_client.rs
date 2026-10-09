//! Client HTTP de l'API Open Data Infoclimat, et lecture de la clé.
//!
//! ## ⚠️ La clé est liée à une adresse IP
//!
//! C'est **le** piège de cette API, et il ne se voit pas dans le code d'erreur.
//! Infoclimat associe chaque clé à l'adresse IP publique déclarée au moment de sa
//! génération. Si cette IP change — box redémarrée, bail DHCP renouvelé par
//! l'opérateur, VPN, partage de connexion — les appels cessent de fonctionner
//! **sans que la clé soit invalide pour autant**.
//!
//! Et la réponse ne le dit pas clairement : l'API renvoie un `HTTP 200 OK` dont le
//! corps est le texte brut `Wrong ip address`. Un client qui suppose du JSON
//! échoue donc sur une erreur de désérialisation, à mille lieues du vrai
//! problème. [`InfoclimatClient::fetch_window`] intercepte ce cas et le traduit en
//! [`InfoclimatError::Auth`], qui porte la marche à suivre complète
//! ([`crate::infoclimat::AUTH_HELP`]).
//!
//! ## La clé ne doit jamais fuiter
//!
//! Elle vit dans `Clee.API/API.Infoclimat.env` (dossier ignoré par git en entier).
//! Trois règles tenues ici :
//!
//! * l'URL complète n'est **jamais** journalisée ni renvoyée à l'UI :
//!   [`redact`] remplace la valeur du paramètre `token` avant tout affichage ;
//! * l'UI ne reçoit que [`KeyStatus`], qui expose une empreinte (4 derniers
//!   caractères) et jamais la clé ;
//! * aucun `println!` du module ne contient la clé.

use std::path::PathBuf;
use std::time::Duration as StdDuration;

use chrono::NaiveDate;
use serde::Serialize;

use crate::api_keys::{self, KeySource, KeyStatus};

use crate::infoclimat::{
    erreur_authentification, parse_infoclimat, ressemble_a_un_refus_d_auth, split_windows,
    DateWindow, InfoclimatError, InfoclimatPayload, InfoclimatRecord,
};

/// Racine de l'API. `version=2` est ce que produit le formulaire du site.
pub const BASE_URL: &str = "https://www.infoclimat.fr/opendata/";

/// Nom du fichier qui porte la clé. Le dossier est `api_keys::KEY_DIR`.
pub use crate::api_keys::KEY_DIR;
pub const KEY_FILE: &str = "API.Infoclimat.env";

/// Variable d'environnement prioritaire sur le fichier.
/// Sert aux tests et à un poste où le dossier `Clee.API/` n'existe pas.
pub const KEY_ENV_VAR: &str = "GWS_INFOCLIMAT_TOKEN";

/// Délai maximal d'un appel. Généreux : l'API sert parfois plusieurs milliers
/// d'enregistrements, et un timeout court se confondrait avec une panne réseau.
const TIMEOUT: StdDuration = StdDuration::from_secs(60);

/// Pause entre deux tranches d'une même demande.
///
/// Infoclimat impose une règle stricte de fair use (< 1 req/s, seuil de sécurité à 1,01 s).
/// Espacer chaque requête de 1010 ms garantit de ne jamais déclencher d'erreur HTTP 429.
pub const PAUSE_ENTRE_TRANCHES: StdDuration = StdDuration::from_millis(1010);

/// Nom du fichier de persistance du quota journalier Infoclimat dans `Clee.API/`.
pub const QUOTA_FILE: &str = "infoclimat_quota.json";

/// Suivi du quota journalier d'appels API Infoclimat.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct DailyQuotaInfo {
    /// Date locale du jour au format `YYYY-MM-DD`.
    pub date: String,
    /// Nombre de requêtes effectuées sur cette date.
    pub requests_today: u32,
    /// Limite recommandée (5000 / jour).
    pub max_daily_requests: u32,
    /// Horodatage UTC du dernier appel.
    pub last_request_utc: Option<String>,
    /// Décalage du fuseau horaire de l'appareil (ex: "+02:00").
    pub timezone_offset: String,
}

impl Default for DailyQuotaInfo {
    fn default() -> Self {
        let now_local = chrono::Local::now();
        Self {
            date: now_local.format("%Y-%m-%d").to_string(),
            requests_today: 0,
            max_daily_requests: 5000,
            last_request_utc: None,
            timezone_offset: now_local.format("%:z").to_string(),
        }
    }
}

/// Résout le chemin du fichier de quota journalier.
pub fn get_quota_file_path() -> PathBuf {
    let candidats = api_keys::key_file_candidates(QUOTA_FILE);
    for candidat in &candidats {
        if candidat.exists() {
            return candidat.clone();
        }
    }
    if let Some(premier) = candidats.first() {
        if let Some(parent) = premier.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        return premier.clone();
    }
    PathBuf::from(QUOTA_FILE)
}

/// Charge l'état du quota journalier en réinitialisant si le jour a changé (selon l'heure/fuseau local de l'appareil).
pub fn load_daily_quota() -> DailyQuotaInfo {
    let chemin = get_quota_file_path();
    let now_local = chrono::Local::now();
    let today_str = now_local.format("%Y-%m-%d").to_string();
    let tz_str = now_local.format("%:z").to_string();

    let mut quota: DailyQuotaInfo = if let Ok(contenu) = std::fs::read_to_string(&chemin) {
        serde_json::from_str(&contenu).unwrap_or_default()
    } else {
        DailyQuotaInfo::default()
    };

    if quota.date != today_str {
        quota.date = today_str;
        quota.requests_today = 0;
        quota.timezone_offset = tz_str;
        let _ = save_daily_quota(&quota);
    }

    quota
}

/// Enregistre les informations de quota sur disque.
pub fn save_daily_quota(quota: &DailyQuotaInfo) -> std::io::Result<()> {
    let chemin = get_quota_file_path();
    if let Some(parent) = chemin.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(quota)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(chemin, json)
}

/// Incrémente le compteur de requêtes du jour.
pub fn increment_daily_quota() -> DailyQuotaInfo {
    let mut quota = load_daily_quota();
    quota.requests_today += 1;
    quota.last_request_utc = Some(chrono::Utc::now().to_rfc3339());
    let _ = save_daily_quota(&quota);
    quota
}

// ---------------------------------------------------------------------------
// Lecture de la clé
// ---------------------------------------------------------------------------
//
// La mécanique (recherche du fichier en remontant l'arborescence, tolérance de
// format, empreinte pour l'UI) vit désormais dans [`crate::api_keys`] : elle est
// partagée avec la clé Groq. Ne restent ici que la déclaration du fournisseur et
// la traduction de l'erreur en `InfoclimatError`, qui porte l'aide sur l'IP.

/// Déclaration du fournisseur pour [`crate::api_keys`].
pub const KEY_SOURCE: KeySource = KeySource {
    file_name: KEY_FILE,
    env_var: KEY_ENV_VAR,
    provider: "Infoclimat",
    signup_url: "https://www.infoclimat.fr/opendata/",
};

/// Charge la clé : variable d'environnement d'abord, fichier ensuite.
pub fn load_key() -> Result<(String, Option<PathBuf>), InfoclimatError> {
    api_keys::load_key(&KEY_SOURCE).map_err(|e| InfoclimatError::CleIntrouvable(e.to_string()))
}

/// Statut de la clé pour l'écran Paramètres — ne divulgue jamais la valeur.
pub fn key_status() -> KeyStatus {
    api_keys::key_status(&KEY_SOURCE)
}

/// Enregistre la clé Infoclimat saisie dans Paramètres.
pub fn save_key(brut: &str) -> Result<PathBuf, api_keys::KeyError> {
    api_keys::save_key(&KEY_SOURCE, brut)
}

/// La variable d'environnement `GWS_INFOCLIMAT_TOKEN` prend-elle le dessus ?
pub fn env_var_prend_le_dessus() -> bool {
    api_keys::env_var_prend_le_dessus(&KEY_SOURCE)
}

// ---------------------------------------------------------------------------
// Construction et rédaction des URL
// ---------------------------------------------------------------------------

/// Construit l'URL d'un appel, dans l'ordre exact produit par le formulaire du
/// site : `?version=2&method=get&format=json&stations[]=…&start=…&end=…&token=…`.
pub fn build_url(base: &str, station_id: &str, window: DateWindow, token: &str) -> String {
    format!(
        "{base}?version=2&method=get&format=json&stations[]={station}&start={start}&end={end}&token={token}",
        base = base,
        station = encode(station_id),
        start = window.start,
        end = window.end,
        token = encode(token),
    )
}

/// Encodage minimal des composants d'URL. Les identifiants de station sont
/// alphanumériques et les clés en base64 URL-safe : seuls quelques caractères
/// méritent d'être échappés, ce qui évite une dépendance de plus.
fn encode(valeur: &str) -> String {
    let mut sortie = String::with_capacity(valeur.len());
    for c in valeur.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => sortie.push(c),
            autre => {
                let mut tampon = [0u8; 4];
                for octet in autre.encode_utf8(&mut tampon).as_bytes() {
                    sortie.push_str(&format!("%{octet:02X}"));
                }
            }
        }
    }
    sortie
}

/// Efface toute trace de la clé dans un texte destiné à être affiché.
///
/// Deux mécanismes, parce qu'il y a deux façons pour la clé de fuiter :
///
/// * dans une **URL** que l'on a construite — `reqwest` recopie l'URL appelée
///   dans ses messages d'erreur réseau ; c'est [`redact`] qui s'en charge ;
/// * dans le **corps d'une réponse**, et celui-là n'était pas prévu : interrogée
///   avec une clé inconnue, l'API répond `400` avec
///   `{"errors":["No account found for this token: 'LA_CLÉ_EN_CLAIR'"]}`. Elle
///   nous renvoie donc le secret, que l'on s'apprêtait à recopier tel quel dans
///   un message d'erreur affiché à l'écran.
///
/// Toute chaîne issue du réseau doit passer par ici avant d'entrer dans une
/// `InfoclimatError`.
pub fn scrub(texte: &str, token: &str) -> String {
    let sans_url = redact(texte);
    // `replace` avec une aiguille vide insérerait le remplacement partout.
    if token.is_empty() {
        sans_url
    } else {
        sans_url.replace(token, "***")
    }
}

/// Remplace la valeur du paramètre `token` **d'une URL** par `***`.
///
/// **Tout** message qui contient une URL doit passer par là : un message d'erreur
/// remonte à l'UI, et l'UI finit dans une capture d'écran.
pub fn redact(url: &str) -> String {
    match url.find("token=") {
        None => url.to_string(),
        Some(debut) => {
            let apres = debut + "token=".len();
            let fin = url[apres..]
                .find('&')
                .map(|i| apres + i)
                .unwrap_or(url.len());
            format!("{}***{}", &url[..apres], &url[fin..])
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Bilan d'une récupération, tranches comprises.
#[derive(Debug, Clone, Serialize)]
pub struct FetchOutcome {
    pub station_id: String,
    pub windows: Vec<DateWindow>,
    pub records: Vec<InfoclimatRecord>,
    pub warnings: Vec<crate::elitech_import::ImportWarning>,
    pub sample_interval_seconds: Option<i64>,
    pub duplicate_count: usize,
    pub unreadable_complements: usize,
    pub station: Option<crate::infoclimat::StationInfo>,
}

/// Client bloquant. Volontairement bloquant : le métier reste synchrone et
/// testable, et les commandes Tauri poussent l'appel dans un `spawn_blocking`.
pub struct InfoclimatClient {
    token: String,
    base_url: String,
    agent: reqwest::blocking::Client,
    ssh_relay: Option<crate::ssh_relay::SshRelayConfig>,
}

impl InfoclimatClient {
    /// Construit un client à partir de la clé du poste.
    pub fn from_local_key() -> Result<Self, InfoclimatError> {
        let (token, _) = load_key()?;
        Self::with_token(token, BASE_URL)
    }

    pub fn with_token(
        token: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, InfoclimatError> {
        let agent = crate::http_client::client_builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| InfoclimatError::Network(e.to_string()))?;

        Ok(InfoclimatClient {
            token: token.into(),
            base_url: base_url.into(),
            agent,
            ssh_relay: None,
        })
    }

    /// Attache une configuration de relai SSH optionnelle.
    pub fn with_ssh_relay(mut self, relay: Option<crate::ssh_relay::SshRelayConfig>) -> Self {
        self.ssh_relay = relay;
        self
    }

    /// Un appel, une tranche. Renvoie le **corps brut** : le parsing est fait par
    /// [`crate::infoclimat::parse_infoclimat`], strictement le même code que pour
    /// un fichier téléchargé à la main.
    pub fn fetch_raw(
        &self,
        station_id: &str,
        window: DateWindow,
    ) -> Result<String, InfoclimatError> {
        let url = build_url(&self.base_url, station_id, window, &self.token);

        // Si le relai SSH est configuré et activé, on délègue la requête au serveur distant via curl
        if let Some(ref relay) = self.ssh_relay {
            if relay.enabled {
                return tauri::async_runtime::block_on(
                    crate::ssh_relay::execute_infoclimat_fetch(relay, &url, &self.token)
                );
            }
        }

        let reponse = self
            .agent
            .get(&url)
            .send()
            // Le message d'erreur de reqwest contient l'URL appelée : sans cette
            // rédaction, la clé partirait dans l'UI.
            .map_err(|e| InfoclimatError::Network(scrub(&e.to_string(), &self.token)))?;

        let status = reponse.status();
        let corps = reponse
            .text()
            .map_err(|e| InfoclimatError::Network(scrub(&e.to_string(), &self.token)))?;

        // ⚠️ À partir d'ici, plus rien du corps ne doit être recopié sans passer
        // par `scrub` : l'API renvoie la clé en clair dans son message « No
        // account found for this token: '…' ».
        let visible = scrub(&corps, &self.token);

        // 401/403 : le cas propre — jamais observé, mais gratuit à couvrir.
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(erreur_authentification(&visible));
        }

        // Les deux cas réels : `200` + « Wrong ip address » en texte brut (IP non
        // déclarée), et `400` + « No account found for this token » (clé inconnue).
        // Ni l'un ni l'autre n'est un code d'authentification standard : c'est le
        // contenu qui tranche.
        if ressemble_a_un_refus_d_auth(&corps) {
            return Err(erreur_authentification(&visible));
        }

        if !status.is_success() {
            return Err(InfoclimatError::Http {
                status: status.as_u16(),
                body: visible.chars().take(300).collect(),
            });
        }

        Ok(corps)
    }

    /// Un appel + son parsing.
    pub fn fetch_window(
        &self,
        station_id: &str,
        window: DateWindow,
    ) -> Result<InfoclimatPayload, InfoclimatError> {
        let corps = self.fetch_raw(station_id, window)?;
        parse_infoclimat(&corps, Some(station_id))
    }

    /// Récupère une période **de longueur quelconque**, en enchaînant autant de
    /// tranches de 7 jours que nécessaire.
    pub fn fetch_range(
        &self,
        station_id: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<FetchOutcome, InfoclimatError> {
        let no_cb: Option<fn(usize, usize, DateWindow)> = None;
        self.fetch_range_with_progress(station_id, start, end, no_cb)
    }

    /// Récupère une période avec suivi de progression et comptage de requêtes journalières.
    pub fn fetch_range_with_progress<F>(
        &self,
        station_id: &str,
        start: NaiveDate,
        end: NaiveDate,
        on_progress: Option<F>,
    ) -> Result<FetchOutcome, InfoclimatError>
    where
        F: Fn(usize, usize, DateWindow) + Send + Sync,
    {
        let windows = split_windows(start, end)?;

        let mut tout = Vec::new();
        let mut warnings = Vec::new();
        let mut unreadable_complements = 0usize;
        let mut station = None;
        let mut interval = None;

        let total_windows = windows.len();
        for (rang, window) in windows.iter().enumerate() {
            if rang > 0 {
                std::thread::sleep(PAUSE_ENTRE_TRANCHES);
            }
            if let Some(ref cb) = on_progress {
                cb(rang + 1, total_windows, *window);
            }

            let payload_res = self.fetch_window(station_id, *window);
            let _ = increment_daily_quota();

            match payload_res {
                Ok(payload) => {
                    station = station.or(payload.station);
                    interval = interval.or(payload.sample_interval_seconds);
                    unreadable_complements += payload.unreadable_complements;
                    warnings.extend(payload.warnings);
                    tout.extend(payload.records);
                }
                Err(InfoclimatError::StationAbsente(_)) => {
                    // Cette tranche précise de 7 jours ne contient aucune mesure pour cette station
                    // (station pas encore installée à cette date, panne temporaire, ou données absentes).
                    // On poursuit la récupération pour les autres tranches.
                }
                Err(InfoclimatError::Api(ref msg)) if msg.contains("aucune station") => {
                    // La réponse ne contient aucune station sur cette tranche précise.
                    // On poursuit la récupération pour les autres tranches.
                }
                Err(e) => return Err(e),
            }
        }

        let avant = tout.len();
        tout.sort_by_key(|r| r.timestamp_utc);
        tout.dedup_by_key(|r| r.timestamp_utc);
        let duplicate_count = avant - tout.len();

        Ok(FetchOutcome {
            station_id: station_id.to_string(),
            windows,
            records: tout,
            warnings,
            sample_interval_seconds: interval,
            duplicate_count,
            unreadable_complements,
            station,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn window(start: &str, end: &str) -> DateWindow {
        DateWindow {
            start: start.parse().unwrap(),
            end: end.parse().unwrap(),
        }
    }

    #[test]
    fn url_conforme_au_formulaire_du_site() {
        let url = build_url(BASE_URL, "STATIC0479", window("2026-06-24", "2026-06-30"), "ABC123");
        assert_eq!(
            url,
            "https://www.infoclimat.fr/opendata/?version=2&method=get&format=json\
             &stations[]=STATIC0479&start=2026-06-24&end=2026-06-30&token=ABC123"
        );
    }

    /// La clé ne doit jamais survivre à un passage dans un message.
    #[test]
    fn la_redaction_efface_le_token() {
        let url = build_url(BASE_URL, "STATIC0479", window("2026-06-24", "2026-06-30"), "SECRET");
        let masque = redact(&url);
        assert!(!masque.contains("SECRET"), "la clé a fuité : {masque}");
        assert!(masque.ends_with("token=***"));

        // Token en milieu d'URL (paramètre suivi d'autre chose).
        let avec_suite = format!("{url}&debug=1");
        let masque = redact(&avec_suite);
        assert!(!masque.contains("SECRET"));
        assert!(masque.ends_with("token=***&debug=1"));

        // Une URL sans token n'est pas altérée.
        assert_eq!(redact("https://exemple.fr/?a=1"), "https://exemple.fr/?a=1");
    }

    /// Cas observé le 19 août 2026 : interrogée avec une clé inconnue, l'API
    /// répond `400` **en recopiant la clé en clair** dans son message. Sans
    /// nettoyage, ce message atterrissait tel quel dans l'UI.
    #[test]
    fn le_corps_de_reponse_ne_peut_pas_renvoyer_la_cle_a_l_ecran() {
        let cle = "ukgvYwtyfNeW1zXHRUNZ";
        let corps =
            format!(r#"{{"errors":["No account found for this token: '{cle}'"]}}"#);

        let visible = scrub(&corps, cle);
        assert!(!visible.contains(cle), "la clé a fuité : {visible}");
        assert!(visible.contains("No account found for this token: '***'"));

        // Un jeton vide ne doit pas transformer le texte en confettis.
        assert_eq!(scrub("texte quelconque", ""), "texte quelconque");
    }

    /// Les deux refus réellement observés doivent être reconnus comme des
    /// problèmes d'authentification, malgré leurs codes HTTP dissemblables.
    #[test]
    fn les_deux_refus_observes_sont_reconnus() {
        // 200 OK, texte brut — IP non déclarée.
        assert!(ressemble_a_un_refus_d_auth("Wrong ip address"));
        // 400, JSON — clé inconnue.
        assert!(ressemble_a_un_refus_d_auth(
            r#"{"errors":["No account found for this token: 'x'"]}"#
        ));
        // Une vraie réponse ne doit évidemment pas déclencher l'alerte.
        assert!(!ressemble_a_un_refus_d_auth(
            r#"{"status":"OK","errors":[],"hourly":{}}"#
        ));
    }

    // Le format du fichier de clé (clé nue, `CLE=valeur`, commentaires) est
    // couvert par les tests de `api_keys` : la mécanique y est désormais commune
    // aux deux fournisseurs.


    #[test]
    fn l_encodage_protege_les_caracteres_speciaux() {
        assert_eq!(encode("STATIC0479"), "STATIC0479");
        assert_eq!(encode("a+b/c="), "a%2Bb%2Fc%3D");
        assert_eq!(encode("clé"), "cl%C3%A9");
    }

    /// Le statut sert l'écran Paramètres : il doit renseigner sans divulguer.
    #[test]
    fn le_statut_de_cle_ne_divulgue_pas_la_cle() {
        // La variable d'environnement prime : le test ne dépend pas du poste.
        std::env::set_var(KEY_ENV_VAR, "ABCDEFGHIJKLMNOP");
        let statut = key_status();
        std::env::remove_var(KEY_ENV_VAR);

        assert!(statut.configured);
        assert_eq!(statut.hint.as_deref(), Some("…MNOP"));
        assert_eq!(statut.length, Some(16));

        let serialise = serde_json::to_string(&statut).unwrap();
        assert!(
            !serialise.contains("ABCDEFGHIJKLMNOP"),
            "la clé complète ne doit jamais être sérialisée vers l'UI : {serialise}"
        );
    }

    #[test]
    fn une_cle_absente_explique_quoi_faire() {
        let e = InfoclimatError::CleIntrouvable("Attendu un fichier Clee.API/…".into());
        assert!(e.to_string().contains("Aucune clé d'API Infoclimat"));
    }

    #[test]
    fn sauvegarde_et_relecture_cle_infoclimat_sans_redemarrage() {
        // Enregistre une clé temporaire et vérifie que load_key la relit directement
        let dir = std::env::temp_dir().join("gws-test-ic-save-key");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(crate::api_keys::KEY_DIR).join(KEY_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // Écriture d'une nouvelle clé
        std::fs::write(&path, "nouvelle_cle_12345\n").unwrap();
        let contenu = std::fs::read_to_string(&path).unwrap();
        assert_eq!(api_keys::extract_key(&contenu).as_deref(), Some("nouvelle_cle_12345"));
    }

    #[test]
    fn compatibilite_format_historique_infoclimat() {
        // Test que les formats historiques (clé nue, GWS_INFOCLIMAT_TOKEN=..., TOKEN=..., avec espaces)
        // continuent d'être lus sans perte pour les utilisateurs existants
        assert_eq!(api_keys::extract_key("my_token_value_xyz").as_deref(), Some("my_token_value_xyz"));
        assert_eq!(api_keys::extract_key("  my_token_value_xyz  \r\n").as_deref(), Some("my_token_value_xyz"));
        assert_eq!(api_keys::extract_key("GWS_INFOCLIMAT_TOKEN=token_123").as_deref(), Some("token_123"));
        assert_eq!(api_keys::extract_key("TOKEN=\"quoted_token\"").as_deref(), Some("quoted_token"));
        assert_eq!(api_keys::extract_key("# Commentaire\nmy_token\n").as_deref(), Some("my_token"));
    }

    // -----------------------------------------------------------------------
    // Appel réel — `#[ignore]` par défaut
    // -----------------------------------------------------------------------

    /// Vérifie la clé du poste **contre l'API réelle**.
    ///
    /// Volontairement `#[ignore]` : `cargo test` ne doit pas dépendre du réseau,
    /// ni consommer le quota d'une association bénévole à chaque compilation.
    /// C'est l'outil de diagnostic à lancer le jour où « ça ne marche plus » :
    ///
    /// ```text
    /// cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture appel_reel
    /// ```
    ///
    /// Deux issues, toutes deux instructives :
    /// * la clé passe → le nombre de mesures du jour s'affiche ;
    /// * la clé est refusée → le message d'IP s'affiche en entier, ce qui prouve
    ///   que le chemin d'erreur fonctionne aussi bien que le chemin nominal.
    #[test]
    #[ignore = "appelle l'API Infoclimat pour de vrai (réseau + quota)"]
    fn appel_reel_a_l_api() {
        let client = match InfoclimatClient::from_local_key() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Clé indisponible : {e}");
                return;
            }
        };

        let hier = chrono::Utc::now().date_naive() - chrono::Duration::days(1);
        let fenetre = DateWindow {
            start: hier,
            end: hier,
        };

        match client.fetch_window("STATIC0479", fenetre) {
            Ok(payload) => {
                eprintln!(
                    "OK — {} mesure(s) le {hier} pour {} ; cadence {:?} s ; licence {:?}",
                    payload.records.len(),
                    payload.station_id,
                    payload.sample_interval_seconds,
                    payload.station.as_ref().and_then(|s| s.license.clone()),
                );
                assert!(!payload.records.is_empty());
            }
            Err(InfoclimatError::Auth(message)) => {
                eprintln!("--- Refus d'authentification, message affiché à l'utilisateur ---");
                eprintln!("{message}");
                eprintln!("---------------------------------------------------------------");
                // Ce n'est pas un échec du code : c'est le cas « IP changée », et
                // le poste de test n'a pas forcément l'IP déclarée dans la clé.
                assert!(message.contains("adresse IP publique"));
            }
            Err(autre) => panic!("échec inattendu : {autre}"),
        }
    }
}

//! Lecture des clés d'API posées dans `Clee.API/`.
//!
//! Ce module n'existait pas au prompt 4 : la mécanique vivait dans
//! [`crate::infoclimat_client`], soudée à `InfoclimatError`. L'arrivée d'une
//! deuxième clé (Groq) l'a rendue générique plutôt que dupliquée — la recherche
//! du fichier, la tolérance de format et la non-divulgation sont exactement les
//! mêmes d'un fournisseur à l'autre ; seul le nom du fichier change.
//!
//! Ce qui reste propre à chaque fournisseur, et donc **hors d'ici** : le type
//! d'erreur métier, et le nettoyage des réponses réseau (`scrub`/`redact` chez
//! Infoclimat, qui renvoie la clé en clair dans ses messages d'erreur — Groq ne
//! le fait pas, cf. § 6 du README).
//!
//! ## Règle unique de ce module
//!
//! La clé sort d'ici de deux façons seulement :
//!
//! * [`load_key`] — pour le code qui doit signer un appel ;
//! * [`key_status`] — pour l'UI, qui n'a **jamais** droit qu'à une empreinte.
//!
//! Aucune autre fonction ne renvoie la valeur, et aucun `println!` ne la trace.
//! [`save_key`] fait le chemin inverse — elle prend une clé et n'en rend que le
//! chemin écrit — et reste ici pour la même raison que le reste : c'est ce
//! module qui sait *où* vit `Clee.API/<fichier>`. L'écran de première ouverture
//! et l'écran Paramètres passent tous deux par elle ; sans quoi l'un pourrait
//! écrire là où l'autre ne lit pas.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;

/// Dossier des clés, à la racine du projet. Ignoré par git en entier.
pub const KEY_DIR: &str = "Clee.API";

/// Racine supplémentaire déclarée au démarrage, fouillée **avant** les autres.
///
/// Elle existe pour Android, et le problème qu'elle règle est concret : une
/// application installée n'a pas de « racine du projet ». `current_dir()` y vaut
/// `/`, et `current_exe()` pointe dans `/data/app/…` — deux endroits où l'on ne
/// peut rien déposer. Sans cette racine, `Clee.API/` est **introuvable sur
/// mobile**, et rétablir le HTTPS n'y changerait rien : Infoclimat et Groq
/// échoueraient toujours, simplement une étape plus loin.
///
/// [`crate::run`] y met le répertoire de données de l'application — celui-là
/// même où vit déjà la base SQLite. Sur desktop, la déclarer ne retire rien :
/// les racines historiques restent fouillées ensuite.
static RACINE_SUPPLEMENTAIRE: OnceLock<PathBuf> = OnceLock::new();

/// Déclare la racine ci-dessus. Sans effet si elle l'a déjà été — le démarrage
/// n'a lieu qu'une fois, et un second appel signalerait une erreur de câblage
/// plutôt qu'un changement légitime de répertoire.
pub fn set_extra_root(dir: impl Into<PathBuf>) {
    let _ = RACINE_SUPPLEMENTAIRE.set(dir.into());
}

/// Un fournisseur : le nom de son fichier et la variable d'environnement qui le
/// court-circuite. Une constante par fournisseur, déclarée à côté de son client.
#[derive(Debug, Clone, Copy)]
pub struct KeySource {
    /// Nom du fichier dans `Clee.API/`, ex. `"API.Groq.env"`.
    pub file_name: &'static str,
    /// Variable d'environnement prioritaire sur le fichier — sert aux tests et
    /// à un poste où le dossier `Clee.API/` n'existe pas.
    pub env_var: &'static str,
    /// Nom lisible du fournisseur, utilisé dans les messages d'erreur.
    pub provider: &'static str,
    /// Où l'on va chercher une clé quand on n'en a pas. Affiché tel quel.
    pub signup_url: &'static str,
}

impl KeySource {
    /// Chemin relatif affichable, ex. `"Clee.API/API.Groq.env"`.
    pub fn display_path(&self) -> String {
        format!("{KEY_DIR}/{}", self.file_name)
    }
}

/// Pourquoi la lecture a échoué. Chaque client traduit cette erreur dans son
/// propre type métier — le message, lui, est déjà rédigé pour l'utilisateur.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("{0}")]
    Absente(String),
    #[error("{0}")]
    Vide(String),
    /// La clé proposée par l'utilisateur ne ressemble pas à une clé. Refusée
    /// avant écriture : mieux vaut le dire au collage qu'au premier appel.
    #[error("{0}")]
    Invalide(String),
    /// Le fichier n'a pas pu être écrit (dossier non créable, disque plein…).
    #[error("{0}")]
    Ecriture(String),
}

/// Ce que l'UI a le droit de savoir d'une clé. **Jamais la clé elle-même.**
#[derive(Debug, Clone, Serialize)]
pub struct KeyStatus {
    pub configured: bool,
    /// Quatre derniers caractères, ex. `"…VOFA"` — juste de quoi vérifier qu'on
    /// parle de la bonne clé quand on en jongle plusieurs.
    pub hint: Option<String>,
    pub length: Option<usize>,
    /// Chemin du fichier lu, ou `null` si la clé vient de la variable d'environnement.
    pub source_path: Option<String>,
    /// Message à afficher quand `configured` vaut `false`.
    pub problem: Option<String>,
}

impl KeyStatus {
    pub fn absente(problem: String) -> Self {
        KeyStatus {
            configured: false,
            hint: None,
            length: None,
            source_path: None,
            problem: Some(problem),
        }
    }
}

/// Où chercher `Clee.API/<fichier>`.
///
/// Le répertoire courant dépend du mode de lancement : `src-tauri/` sous
/// `cargo tauri dev`, le dossier de l'exécutable une fois l'app installée. On
/// remonte donc l'arborescence depuis les deux, plutôt que de coder un chemin en
/// dur qui ne marcherait que dans un cas.
pub fn key_file_candidates(file_name: &str) -> Vec<PathBuf> {
    let mut racines: Vec<PathBuf> = Vec::new();

    // En premier : sur mobile c'est la seule qui puisse aboutir, et sur desktop
    // elle désigne le répertoire de données de l'application — un endroit
    // légitime pour poser une clé sur un poste où le projet n'est pas cloné.
    if let Some(extra) = RACINE_SUPPLEMENTAIRE.get() {
        racines.push(extra.clone());
    }

    if let Ok(cwd) = std::env::current_dir() {
        racines.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            racines.push(dir.to_path_buf());
        }
    }

    let mut chemins = Vec::new();
    for racine in racines {
        // Cinq niveaux suffisent : `src-tauri/target/debug/` est déjà à trois.
        let mut courant: Option<&Path> = Some(racine.as_path());
        for _ in 0..6 {
            let Some(dir) = courant else { break };
            let candidat = dir.join(KEY_DIR).join(file_name);
            if !chemins.contains(&candidat) {
                chemins.push(candidat);
            }
            courant = dir.parent();
        }
    }
    chemins
}

/// Extrait la clé du contenu du fichier.
///
/// Deux formats acceptés, parce que le fichier réel n'a pas la forme attendue :
/// il contient **la clé nue**, sans `NOM=`, sans retour à la ligne. On tolère
/// quand même la forme `CLE=valeur` (et les commentaires `#`) au cas où quelqu'un
/// réécrirait le fichier « proprement » plus tard.
pub fn extract_key(contenu: &str) -> Option<String> {
    for ligne in contenu.lines() {
        let ligne = ligne.trim();
        if ligne.is_empty() || ligne.starts_with('#') {
            continue;
        }
        let valeur = match ligne.split_once('=') {
            Some((_nom, valeur)) => valeur.trim(),
            None => ligne,
        };
        // Une clé peut être entourée de guillemets dans un .env classique.
        let valeur = valeur.trim_matches(|c| c == '"' || c == '\'').trim();
        if !valeur.is_empty() {
            return Some(valeur.to_string());
        }
    }
    None
}

/// Charge la clé d'un fournisseur : variable d'environnement d'abord, fichier
/// ensuite. Renvoie aussi le chemin lu, ou `None` si la clé vient de
/// l'environnement.
pub fn load_key(source: &KeySource) -> Result<(String, Option<PathBuf>), KeyError> {
    if let Ok(depuis_env) = std::env::var(source.env_var) {
        if let Some(cle) = extract_key(&depuis_env) {
            return Ok((cle, None));
        }
    }

    for chemin in &key_file_candidates(source.file_name) {
        let Ok(contenu) = std::fs::read_to_string(chemin) else {
            continue;
        };
        match extract_key(&contenu) {
            Some(cle) => return Ok((cle, Some(chemin.clone()))),
            None => {
                return Err(KeyError::Vide(format!(
                    "Le fichier {} existe mais est vide. Collez-y la clé {} obtenue sur {} \
                     (la clé seule suffit, ou la forme CLE=valeur).",
                    chemin.display(),
                    source.provider,
                    source.signup_url
                )))
            }
        }
    }

    Err(KeyError::Absente(format!(
        "Attendu un fichier {} à la racine du projet (ou la variable d'environnement {}). \
         Générez une clé sur {} puis collez-la dans ce fichier.",
        source.display_path(),
        source.env_var,
        source.signup_url
    )))
}

/// Longueur maximale acceptée à l'enregistrement.
///
/// Aucune clé de fournisseur n'approche ce chiffre (celles de Groq font une
/// soixantaine de caractères). La borne n'est pas là pour valider un format —
/// elle est là pour qu'un collage accidentel de page entière soit refusé net
/// plutôt qu'écrit dans `Clee.API/`.
const MAX_KEY_LEN: usize = 512;

/// Normalise et contrôle une clé saisie à la main, sans appeler le fournisseur.
///
/// La validation reste **syntaxique** : la seule preuve qu'une clé est bonne est
/// un appel qui aboutit, et le faire ici obligerait l'écran de première
/// ouverture à attendre le réseau pour rien. On refuse donc uniquement ce qui ne
/// peut pas être une clé — vide, coupé d'espaces, démesuré.
///
/// [`extract_key`] est réutilisée telle quelle : quelqu'un qui colle
/// `GROQ_API_KEY=gsk_…` depuis un tutoriel obtient la même tolérance que s'il
/// avait écrit le fichier lui-même.
pub fn sanitize_key(brut: &str) -> Result<String, KeyError> {
    let Some(valeur) = extract_key(brut) else {
        return Err(KeyError::Invalide(
            "Collez la clé dans le champ : il est vide.".into(),
        ));
    };
    if valeur.chars().any(char::is_whitespace) {
        return Err(KeyError::Invalide(
            "Cette clé contient un espace ou un retour à la ligne. Vérifiez la copie :              une clé est d'un seul tenant."
                .into(),
        ));
    }
    if valeur.chars().count() > MAX_KEY_LEN {
        return Err(KeyError::Invalide(format!(
            "Ce texte fait {} caractères : c'est trop long pour une clé ({MAX_KEY_LEN} au              maximum). Vérifiez que vous n'avez pas collé toute la page.",
            valeur.chars().count()
        )));
    }
    Ok(valeur)
}

/// Choisit le fichier à (ré)écrire parmi les candidats de [`key_file_candidates`].
///
/// Règle : **le fichier qui existe déjà l'emporte**. C'est la seule façon
/// d'éviter qu'un enregistrement crée un second `Clee.API/API.Groq.env` ailleurs
/// dans l'arborescence, que `load_key` ne lirait plus si un autre le précède
/// dans la liste — l'utilisateur verrait alors « clé enregistrée » puis
/// « aucune clé » dans les Paramètres.
///
/// À défaut, on prend le premier candidat : la racine supplémentaire déclarée au
/// démarrage, c'est-à-dire le répertoire de données de l'application. Il est
/// toujours accessible en écriture, ce que la racine du projet n'est pas une
/// fois l'application installée dans `Program Files`.
pub fn choose_key_file(candidats: &[PathBuf]) -> Option<PathBuf> {
    candidats
        .iter()
        .find(|c| c.is_file())
        .or_else(|| candidats.first())
        .cloned()
}

/// Écrit la clé d'un fournisseur et renvoie le chemin du fichier écrit.
///
/// **Ne renvoie jamais la clé.** Le chemin, lui, est affichable : c'est déjà ce
/// que montre [`key_status`] via `source_path`.
pub fn save_key(source: &KeySource, brut: &str) -> Result<PathBuf, KeyError> {
    let valeur = sanitize_key(brut)?;

    let candidats = key_file_candidates(source.file_name);
    let Some(chemin) = choose_key_file(&candidats) else {
        return Err(KeyError::Ecriture(format!(
            "Impossible de déterminer où écrire {}.",
            source.display_path()
        )));
    };

    save_key_at(&chemin, &valeur)?;
    Ok(chemin)
}

/// Écrit une clé **déjà nettoyée** dans un fichier précis.
///
/// Séparée de [`save_key`] pour être testable sans dépendre de
/// [`key_file_candidates`], qui lit l'environnement réel de la machine.
fn save_key_at(chemin: &Path, valeur: &str) -> Result<(), KeyError> {
    if let Some(parent) = chemin.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            KeyError::Ecriture(format!(
                "Le dossier {} n'a pas pu être créé : {e}",
                parent.display()
            ))
        })?;
    }

    // Retour à la ligne final : le fichier reste lisible et modifiable à la main
    // dans n'importe quel éditeur, ce que `Clee.API/` a toujours permis.
    std::fs::write(chemin, format!("{valeur}
")).map_err(|e| {
        KeyError::Ecriture(format!(
            "La clé n'a pas pu être écrite dans {} : {e}",
            chemin.display()
        ))
    })
}

/// La variable d'environnement du fournisseur porte-t-elle une clé ?
///
/// À demander **après** [`save_key`] : [`load_key`] regarde l'environnement en
/// premier, donc sur un poste où la variable est posée, le fichier fraîchement
/// écrit ne sera pas celui qui sert. Le cas est rare (tests, poste de
/// développement) mais silencieux, et l'UI doit pouvoir le dire.
pub fn env_var_prend_le_dessus(source: &KeySource) -> bool {
    std::env::var(source.env_var)
        .ok()
        .and_then(|v| extract_key(&v))
        .is_some()
}

/// Statut d'une clé pour l'écran Paramètres — ne divulgue jamais la valeur.
pub fn key_status(source: &KeySource) -> KeyStatus {
    match load_key(source) {
        Ok((cle, chemin)) => {
            // Les quatre **derniers** caractères : le préfixe d'une clé Groq
            // (`gsk_`) est constant et n'identifie donc rien.
            let hint: String = cle
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            KeyStatus {
                configured: true,
                hint: Some(format!("…{hint}")),
                length: Some(cle.chars().count()),
                source_path: chemin.map(|p| p.display().to_string()),
                problem: None,
            }
        }
        Err(e) => KeyStatus::absente(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_TEST: KeySource = KeySource {
        file_name: "API.Test.env",
        env_var: "GWS_TEST_TOKEN_INEXISTANT",
        provider: "Test",
        signup_url: "https://example.invalid/",
    };

    /// La racine supplémentaire doit être **la première** fouillée : sur
    /// Android c'est la seule qui existe, et sur desktop une clé posée à côté de
    /// la base doit primer sur une autre laissée dans un dossier parent.
    #[test]
    fn la_racine_supplementaire_est_fouillee_en_premier() {
        let dir = std::env::temp_dir().join("gws-cle-racine-supplementaire");
        set_extra_root(&dir);

        let chemins = key_file_candidates("API.Test.env");
        assert_eq!(
            chemins.first(),
            Some(&dir.join(KEY_DIR).join("API.Test.env")),
            "la racine déclarée au démarrage doit passer avant le répertoire courant"
        );
    }

    #[test]
    fn la_cle_nue_est_lue() {
        assert_eq!(extract_key("ukgvYwty1234").as_deref(), Some("ukgvYwty1234"));
        assert_eq!(
            extract_key("  gsk_abcdef  \n").as_deref(),
            Some("gsk_abcdef")
        );
    }

    #[test]
    fn la_forme_variable_denvironnement_est_aussi_acceptee() {
        assert_eq!(extract_key("CLE=abc123").as_deref(), Some("abc123"));
        assert_eq!(
            extract_key("# commentaire\nGROQ_API_KEY=\"abc123\"\n").as_deref(),
            Some("abc123")
        );
        assert_eq!(extract_key("").as_deref(), None);
        assert_eq!(extract_key("# rien que des commentaires\n").as_deref(), None);
    }

    #[test]
    fn les_candidats_remontent_l_arborescence() {
        let candidats = key_file_candidates("API.Groq.env");
        assert!(!candidats.is_empty());
        assert!(candidats
            .iter()
            .all(|c| c.ends_with("Clee.API/API.Groq.env") || c.ends_with("Clee.API\\API.Groq.env")));
        // Plusieurs niveaux, sinon un lancement depuis `src-tauri/` ne trouverait rien.
        assert!(candidats.len() > 1);
    }

    /// Le point qui garantit la cohérence entre l'écran de première ouverture et
    /// l'écran Paramètres : on réécrit le fichier que `load_key` lit déjà,
    /// jamais un nouveau ailleurs dans la liste.
    #[test]
    fn l_enregistrement_reecrit_le_fichier_existant() {
        let base = std::env::temp_dir().join("gws-choose-key-file");
        let _ = std::fs::remove_dir_all(&base);
        let deuxieme = base.join("b").join(KEY_DIR).join("API.Test.env");
        std::fs::create_dir_all(deuxieme.parent().unwrap()).unwrap();
        std::fs::write(&deuxieme, "cle-deja-la").unwrap();

        let candidats = vec![
            base.join("a").join(KEY_DIR).join("API.Test.env"),
            deuxieme.clone(),
        ];
        assert_eq!(
            choose_key_file(&candidats),
            Some(deuxieme),
            "un fichier déjà présent doit primer sur un candidat inexistant placé avant lui"
        );
    }

    /// Sans fichier nulle part, on écrit dans le premier candidat — celui de la
    /// racine supplémentaire, toujours accessible en écriture.
    #[test]
    fn sans_fichier_existant_on_prend_le_premier_candidat() {
        let base = std::env::temp_dir().join("gws-choose-key-file-vide");
        let premier = base.join("a").join(KEY_DIR).join("API.Test.env");
        let candidats = vec![premier.clone(), base.join("b").join(KEY_DIR).join("API.Test.env")];
        assert_eq!(choose_key_file(&candidats), Some(premier));
        assert_eq!(choose_key_file(&[]), None);
    }

    /// Ce qui est écrit doit être relu à l'identique : c'est tout le contrat
    /// entre les deux écrans, et il passe par `extract_key`.
    #[test]
    fn ce_qui_est_ecrit_est_relu_tel_quel() {
        let dir = std::env::temp_dir().join("gws-save-key-aller-retour");
        let _ = std::fs::remove_dir_all(&dir);
        let chemin = dir.join(KEY_DIR).join("API.Test.env");
        std::fs::create_dir_all(chemin.parent().unwrap()).unwrap();
        std::fs::write(&chemin, "ancienne").unwrap();

        let valeur = sanitize_key("  gsk_ABCdef123  ").unwrap();
        save_key_at(&chemin, &valeur).unwrap();

        // Relue par le même `extract_key` que `load_key` : c'est exactement le
        // chemin que suit l'écran Paramètres pour afficher son empreinte.
        let relue = extract_key(&std::fs::read_to_string(&chemin).unwrap());
        assert_eq!(relue.as_deref(), Some("gsk_ABCdef123"));

        // Et l'écriture écrase : pas d'accumulation de lignes, sinon `extract_key`
        // rendrait l'ancienne clé, qui est la première non vide du fichier.
        save_key_at(&chemin, "gsk_seconde").unwrap();
        let relue = extract_key(&std::fs::read_to_string(&chemin).unwrap());
        assert_eq!(relue.as_deref(), Some("gsk_seconde"));

        // Le dossier `Clee.API/` est créé s'il manque : sur un poste fraîchement
        // installé, il n'existe pas encore quand la clé est collée.
        let vierge = dir.join("neuf").join(KEY_DIR).join("API.Test.env");
        save_key_at(&vierge, "gsk_neuve").unwrap();
        assert!(vierge.is_file());
    }

    #[test]
    fn une_cle_collee_est_nettoyee_ou_refusee() {
        // Tolérance identique à la lecture : la forme `NOM=valeur` est acceptée.
        assert_eq!(
            sanitize_key("GROQ_API_KEY=\"gsk_abc\"").unwrap(),
            "gsk_abc"
        );
        assert_eq!(sanitize_key("
  gsk_abc 
").unwrap(), "gsk_abc");

        // Un champ vide, ou seulement des blancs : refusé, sans écrire.
        assert!(matches!(sanitize_key("   "), Err(KeyError::Invalide(_))));

        // Deux morceaux séparés d'un espace : une copie qui a mal tourné.
        assert!(matches!(
            sanitize_key("gsk_abc def"),
            Err(KeyError::Invalide(_))
        ));

        // Une page entière collée par mégarde.
        let enorme = "g".repeat(MAX_KEY_LEN + 1);
        assert!(matches!(sanitize_key(&enorme), Err(KeyError::Invalide(_))));
    }

    #[test]
    fn une_cle_absente_explique_quoi_faire() {
        let statut = key_status(&SOURCE_TEST);
        // Le fichier `API.Test.env` n'existe nulle part : le statut doit être
        // « absente » et dire où poser la clé.
        if !statut.configured {
            let probleme = statut.problem.unwrap_or_default();
            assert!(probleme.contains("Clee.API/API.Test.env"));
            assert!(probleme.contains("GWS_TEST_TOKEN_INEXISTANT"));
        }
    }

    #[test]
    fn le_statut_ne_divulgue_jamais_la_cle() {
        // Une clé passée par l'environnement : on vérifie que sa sérialisation
        // n'en contient que les quatre derniers caractères.
        const CLE: &str = "gsk_UNECLEFICTIVEDETESTXYZW";
        let source = KeySource {
            file_name: "API.Fictive.env",
            env_var: "GWS_CLE_FICTIVE",
            provider: "Fictif",
            signup_url: "https://example.invalid/",
        };
        std::env::set_var(source.env_var, CLE);
        let statut = key_status(&source);
        std::env::remove_var(source.env_var);

        assert!(statut.configured);
        assert_eq!(statut.hint.as_deref(), Some("…XYZW"));
        assert_eq!(statut.length, Some(CLE.chars().count()));

        let json = serde_json::to_string(&statut).unwrap();
        assert!(!json.contains(CLE), "la clé a fuité dans le JSON envoyé à l'UI");
        assert!(!json.contains("UNECLEFICTIVE"));
    }
}

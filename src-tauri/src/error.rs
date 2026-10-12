//! Erreur unique remontée aux commandes Tauri.
//!
//! Tauri exige que le type d'erreur d'une commande soit `Serialize`. On
//! sérialise simplement le `Display` : le frontend reçoit une chaîne en français,
//! directement affichable. Si un jour on a besoin de distinguer les cas côté UI
//! (par ex. pour surligner un champ de formulaire), il suffira de sérialiser une
//! structure `{ kind, message }`.

use crate::elitech_import::ImportError;
use crate::infoclimat::InfoclimatError;
use crate::model::ValidationError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Validation(#[from] ValidationError),

    #[error("{0}")]
    Import(#[from] ImportError),

    /// Les messages d'`InfoclimatError` sont déjà rédigés pour l'utilisateur
    /// final — celui de l'erreur d'authentification tient plusieurs paragraphes
    /// et explique la contrainte d'IP. On ne préfixe donc rien.
    #[error("{0}")]
    Infoclimat(#[from] InfoclimatError),

    /// Même principe que pour Infoclimat : les messages de `GroqError` sont déjà
    /// rédigés pour l'utilisateur (clé refusée, quota de la minute épuisé,
    /// réponse vide) et disent quoi faire. On ne préfixe rien.
    #[error("{0}")]
    Groq(#[from] crate::groq::GroqError),

    /// Idem : les messages de `RommaError` orientent déjà l'utilisateur (« s'il
    /// s'agit d'un climato_*.csv, importez l'archive détaillée »).
    #[error("{0}")]
    Romma(#[from] crate::romma_import::RommaError),

    /// Lecture directe du RC-5. Même principe encore : « fermez ElitechLog »,
    /// « branchez le logger », « essayez un autre câble » — les messages sont
    /// des consignes, pas des diagnostics techniques. Rien à préfixer.
    #[error("{0}")]
    Serial(#[from] crate::elitech_serial::SerialError),

    /// Saisie manuelle sur site. Les messages nomment le jour fautif (« Le
    /// 2026-01-15 n'a pas de valeur ») : c'est ce qui permet de corriger le
    /// formulaire sans le relire en entier.
    #[error("{0}")]
    ManualEntry(#[from] crate::manual_entry::ManualEntryError),

    #[error(
        "La base de données a refusé l'opération (détail : {0}). Réessayez ; si le \
         message revient, fermez puis rouvrez GWS."
    )]
    Database(#[from] rusqlite::Error),

    #[error(
        "Fichier inaccessible ({0}). Vérifiez qu'il existe, qu'il n'est pas ouvert \
         dans un autre logiciel (Excel…) et que le dossier est accessible."
    )]
    Io(#[from] std::io::Error),

    /// Messages déjà rédigés pour l'utilisateur (tuile indisponible, zone de
    /// téléchargement disproportionnée) : rien à préfixer.
    #[error("{0}")]
    Tile(#[from] crate::tiles::TileError),

    #[error(
        "{0} introuvable (identifiant {1}) : il a peut-être été supprimé entre-temps. \
         Rechargez la liste et réessayez."
    )]
    NotFound(&'static str, String),

    #[error("{0}")]
    Other(String),
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;

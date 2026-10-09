//! Interface des extensions natives de Gastinel Weather Studio.
//!
//! Une extension est une bibliothèque dynamique (`.dll` sous Windows) chargée
//! **dans le processus de l'application**, sans aucune isolation mémoire. Rust
//! n'ayant pas d'ABI stable, la frontière entre l'app et l'extension est une
//! **ABI C** : cinq fonctions `extern "C"` qui n'échangent que des entiers et
//! des chaînes UTF-8 terminées par un zéro, contenant du JSON. L'app et une
//! extension peuvent donc être compilées avec des versions différentes de Rust.
//!
//! Côté extension, on n'écrit pas ces fonctions à la main : on implémente le
//! trait [`Extension`] et on appelle [`declare_extension!`].
//!
//! # Contrat mémoire
//! Toute chaîne rendue par l'extension est allouée par elle et **libérée par
//! elle** via [`SYM_FREE`] : l'app ne la libère jamais avec son propre
//! allocateur. Les chaînes passées par l'app ne vivent que le temps de l'appel.

use serde::{Deserialize, Serialize};

/// Version de l'interface. À incrémenter à tout changement incompatible des
/// symboles ci-dessous ou de [`HostContext`]. Une extension qui déclare une
/// autre version n'est **pas chargée**.
pub const API_VERSION: u32 = 1;

/// `extern "C" fn() -> u32` — version d'API ciblée par l'extension.
pub const SYM_API_VERSION: &[u8] = b"gws_ext_api_version\0";
/// `extern "C" fn(ctx_json: *const c_char) -> *mut c_char` — null si succès,
/// sinon message d'erreur (à libérer par [`SYM_FREE`]).
pub const SYM_INIT: &[u8] = b"gws_ext_init\0";
/// `extern "C" fn(method: *const c_char, payload_json: *const c_char) -> *mut c_char`
/// — réponse JSON `{"ok": …}` ou `{"err": "…"}`, jamais null.
pub const SYM_CALL: &[u8] = b"gws_ext_call\0";
/// `extern "C" fn(s: *mut c_char)` — libère une chaîne rendue par l'extension.
pub const SYM_FREE: &[u8] = b"gws_ext_free\0";
/// `extern "C" fn()` — appelée avant le déchargement.
pub const SYM_SHUTDOWN: &[u8] = b"gws_ext_shutdown\0";

/// Permissions **déclaratives** d'un manifeste : elles informent l'utilisateur,
/// rien ne les impose techniquement — sauf `db_write`, qui déclenche la
/// sauvegarde automatique de la base avant le chargement.
pub const PERM_DB_READ: &str = "db_read";
pub const PERM_DB_WRITE: &str = "db_write";
pub const PERM_NETWORK: &str = "network";

/// Contexte transmis à l'initialisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostContext {
    pub api_version: u32,
    pub host_version: String,
    /// Chemin du fichier SQLite de l'app.
    pub db_path: String,
    /// URI SQLite à ouvrir : `mode=ro` si l'extension n'a pas déclaré
    /// `db_write`, `mode=rw` sinon (la sauvegarde est alors déjà faite).
    pub db_uri: String,
    pub db_writable: bool,
    /// Dossier propre à l'extension (cache, réglages), créé par l'app.
    pub data_dir: String,
}

/// Ce que chaque extension implémente.
pub trait Extension: Send {
    /// Appelée une fois, après le chargement.
    fn init(&mut self, ctx: HostContext) -> Result<(), String>;
    /// Appel depuis l'interface de l'extension (son panneau dans l'app).
    /// `payload` porte toujours `project_id` (projet actif, ou null).
    fn call(&mut self, method: &str, payload: serde_json::Value) -> Result<serde_json::Value, String>;
    /// Appelée avant le déchargement : fermer connexions et fichiers.
    fn shutdown(&mut self) {}
}

/// Génère les cinq fonctions C à partir d'un type `Default + Extension`.
///
/// Chaque entrée est protégée par `catch_unwind` : une panique de l'extension
/// devient une erreur JSON au lieu de traverser la frontière C (comportement
/// indéfini). Cela ne protège **pas** contre une corruption mémoire.
#[macro_export]
macro_rules! declare_extension {
    ($ty:ty) => {
        static __GWS_EXT: ::std::sync::Mutex<Option<$ty>> = ::std::sync::Mutex::new(None);

        fn __gws_str(s: String) -> *mut ::std::os::raw::c_char {
            ::std::ffi::CString::new(s.replace('\0', " ")).unwrap().into_raw()
        }
        unsafe fn __gws_in(p: *const ::std::os::raw::c_char) -> String {
            if p.is_null() {
                return String::new();
            }
            ::std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }

        #[no_mangle]
        pub extern "C" fn gws_ext_api_version() -> u32 {
            $crate::API_VERSION
        }

        #[no_mangle]
        pub unsafe extern "C" fn gws_ext_init(ctx: *const ::std::os::raw::c_char) -> *mut ::std::os::raw::c_char {
            let brut = __gws_in(ctx);
            let r = ::std::panic::catch_unwind(move || -> Result<(), String> {
                let ctx: $crate::HostContext = ::serde_json::from_str(&brut).map_err(|e| e.to_string())?;
                let mut ext = <$ty as ::std::default::Default>::default();
                $crate::Extension::init(&mut ext, ctx)?;
                *__GWS_EXT.lock().unwrap_or_else(|e| e.into_inner()) = Some(ext);
                Ok(())
            });
            match r {
                Ok(Ok(())) => ::std::ptr::null_mut(),
                Ok(Err(e)) => __gws_str(e),
                Err(_) => __gws_str("panique pendant l'initialisation".into()),
            }
        }

        #[no_mangle]
        pub unsafe extern "C" fn gws_ext_call(
            method: *const ::std::os::raw::c_char,
            payload: *const ::std::os::raw::c_char,
        ) -> *mut ::std::os::raw::c_char {
            let (m, p) = (__gws_in(method), __gws_in(payload));
            let r = ::std::panic::catch_unwind(move || -> Result<::serde_json::Value, String> {
                let v: ::serde_json::Value = ::serde_json::from_str(&p).unwrap_or(::serde_json::Value::Null);
                let mut g = __GWS_EXT.lock().unwrap_or_else(|e| e.into_inner());
                let ext = g.as_mut().ok_or("extension non initialisée")?;
                $crate::Extension::call(ext, &m, v)
            });
            let rep = match r {
                Ok(Ok(v)) => ::serde_json::json!({ "ok": v }),
                Ok(Err(e)) => ::serde_json::json!({ "err": e }),
                Err(_) => ::serde_json::json!({ "err": "panique dans l'extension" }),
            };
            __gws_str(rep.to_string())
        }

        #[no_mangle]
        pub unsafe extern "C" fn gws_ext_free(s: *mut ::std::os::raw::c_char) {
            if !s.is_null() {
                drop(::std::ffi::CString::from_raw(s));
            }
        }

        #[no_mangle]
        pub extern "C" fn gws_ext_shutdown() {
            let _ = ::std::panic::catch_unwind(|| {
                if let Some(mut ext) = __GWS_EXT.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    $crate::Extension::shutdown(&mut ext);
                }
            });
        }
    };
}

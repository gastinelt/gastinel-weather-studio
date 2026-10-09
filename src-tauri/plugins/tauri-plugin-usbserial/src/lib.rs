//! Liaison série USB-OTG pour Android — la face Rust.
//!
//! ## Pourquoi ce greffon existe
//!
//! Le RC-5 se présente comme un adaptateur USB-série à puce **CH340**. Sur
//! desktop, la crate `serialport` suffit : le système monte un `COM8` ou un
//! `/dev/ttyUSB0` et l'on écrit dedans. Sur Android, rien de tout cela :
//!
//! * `serialport` 4 ne sait **pas** énumérer sur cette cible — son
//!   `available_ports()` y retombe sur la branche « Not implemented for this
//!   OS » (Android n'est pas `target_os = "linux"` pour ses `cfg`) ;
//! * même en devinant un chemin, aucun `/dev/ttyUSB*` n'apparaît : les noyaux
//!   Android n'embarquent pas le pilote `ch341`, et le bac à sable des
//!   applications interdirait de toute façon d'ouvrir un nœud de périphérique ;
//! * le seul accès légitime est l'**API USB Host** de la plateforme
//!   (`UsbManager`), en Java/Kotlin, avec une autorisation demandée à
//!   l'utilisateur pour chaque appareil.
//!
//! Il faut donc un pont, et il faut le mettre quelque part de durable. Ce
//! greffon est ce quelque part : une bibliothèque Android (`android/`) qui parle
//! à `UsbManager` et pilote la puce CH340, et cette façade Rust qui l'appelle.
//! Le dossier `src-tauri/gen/android/` ne convenait pas : il est régénéré par
//! `tauri android init` et ignoré par git.
//!
//! ## Qui pilote la puce
//!
//! Plus nous. Le pilote CH340 écrit à la main au prompt 13 (`Ch340.kt`)
//! énumérait l'appareil sans jamais faire passer un octet, sur deux adaptateurs
//! OTG différents. Il a été remplacé par
//! [`usb-serial-for-android`](https://github.com/mik3y/usb-serial-for-android)
//! et son `Ch34xSerialDriver` — même API USB Host, même espace utilisateur, ni
//! root ni pilote noyau, mais treize ans d'usage et des tests sur du matériel
//! réel. Ce qui reste en propre est dans `android/…/PortSerie.kt`, et rien de la
//! présente façade n'a changé : la frontière du pont est la même.
//!
//! ## Ce qui **n'est pas** ici
//!
//! Le protocole RC-5. Il reste intégralement dans `elitech_serial.rs`, côté
//! Rust, et n'est pas réécrit en Kotlin. Ce greffon ne connaît que des octets :
//! ouvrir, écrire, lire, fermer. C'est ce qui permet à la lecture Android de
//! passer par **exactement** le même code — sommes de contrôle, repos de 500 ms,
//! `0xCC` de fin de session — que la lecture desktop validée sur le matériel.
//!
//! ## L'accès global
//!
//! [`SerialPort::open`] est appelé depuis `elitech_serial::SerialTransport`, une
//! structure qui ne connaît ni `AppHandle` ni `PluginHandle`. Plutôt que de
//! faire transiter un handle par toutes les couches pour un détail de
//! plomberie, le greffon garde le sien dans un [`OnceLock`] posé à
//! l'initialisation. C'est le même parti que `api_keys::set_extra_root`.
//!
//! ## ⚠️ Ce code n'a pas été exercé sur du matériel
//!
//! L'émulateur Android n'a pas de contrôleur USB hôte : rien de ce fichier ni de
//! son pendant Kotlin ne peut y être vérifié. Le pilote de la puce, lui, n'est
//! plus le nôtre (voir ci-dessus) — mais **le passage du pilote maison à
//! `usb-serial-for-android` n'a pas non plus été essayé sur le matériel**, et
//! tant qu'il ne l'aura pas été, la lecture directe sur Android reste à tenir
//! pour non fonctionnelle. Voir le README, § 4quater.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder, TauriPlugin};
use tauri::Wry;

#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "fr.gastinel.usbserial";

/// Ce qui peut mal se passer entre l'appel Rust et la puce.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum UsbError {
    /// Le greffon n'est pas là : cible non-Android, ou initialisation ratée.
    #[error("La liaison USB directe n'est pas disponible sur cette plateforme.")]
    Indisponible,
    /// L'utilisateur a refusé — ou ignoré — la demande d'autorisation USB.
    #[error(
        "Autorisation USB refusée pour l'appareil {device_id}. Rebranchez le logger et \
         acceptez la demande d'accès affichée par Android."
    )]
    PermissionRefusee { device_id: i32 },
    /// Le câble a été arraché pendant un échange.
    ///
    /// Distinguée de [`UsbError::Autre`] parce qu'elle ne demande pas la même
    /// chose : celle-ci se rattrape en rebranchant, et l'écran peut le dire au
    /// lieu d'afficher le message brut d'un transfert qui a rendu -1. C'est le
    /// cas le plus fréquent sur le terrain — le câble OTG pend au bout de la
    /// tablette pendant que le boîtier est dans la poche.
    #[error(
        "Le logger a été débranché pendant la lecture (session {session_id}). \
         Rebranchez le boîtier : la détection le retrouvera toute seule."
    )]
    Debranche { session_id: i64 },
    /// Tout le reste : interface déjà prise, puce muette…
    #[error("{0}")]
    Autre(String),
}

/// Un appareil USB vu par la plateforme.
///
/// Le champ décisif est le couple `vendor_id`/`product_id` : c'est lui, et non
/// le nom, qui désigne un CH340 (`1a86:7523`). Le nom de produit d'un
/// adaptateur bon marché est souvent vide ou fantaisiste.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbDeviceInfo {
    /// Identifiant attribué par Android à ce branchement. **Il change à chaque
    /// rebranchement** : il ne peut pas servir à reconnaître un appareil d'une
    /// session à l'autre.
    pub device_id: i32,
    pub vendor_id: i32,
    pub product_id: i32,
    /// Nom système, du genre `/dev/bus/usb/001/002`. Informatif seulement.
    pub device_name: String,
    pub product_name: Option<String>,
    pub manufacturer_name: Option<String>,
    pub serial_number: Option<String>,
    /// `true` si l'application a déjà l'autorisation pour cet appareil — donc
    /// si l'ouvrir ne fera **pas** apparaître de fenêtre système.
    pub has_permission: bool,
}

// --- Charges utiles échangées avec le Kotlin -------------------------------
//
// Les octets voyagent en tableau d'entiers plutôt qu'en base64 : les trames du
// RC-5 font au plus quelques centaines d'octets (500 mesures par page), le
// surcoût est négligeable, et un tableau reste lisible dans les journaux quand
// il faut regarder ce que la puce a vraiment renvoyé.
//
// `u16` et non `u8` : `JSObject` côté Kotlin manipule des entiers signés, et un
// octet ≥ 128 relu en `i8` deviendrait négatif. Passer par un type plus large
// rend la conversion explicite des deux côtés.

#[derive(Debug, Serialize, Deserialize)]
struct SansArgument {}

#[derive(Debug, Deserialize)]
struct ListeAppareils {
    devices: Vec<UsbDeviceInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OuvrirArgs {
    device_id: i32,
    baud_rate: u32,
    /// `false` : on ne lève **pas** DTR à l'ouverture. Le RC-5 l'ignore, mais la
    /// même puce CH340 équipe les clones Arduino, qu'un DTR levé redémarre.
    /// Même précaution que le `dtr_on_open(false)` du côté desktop.
    dtr: bool,
    rts: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OuvertureFaite {
    session_id: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EcrireArgs {
    session_id: i64,
    data: Vec<u16>,
    timeout_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LireArgs {
    session_id: i64,
    /// Nombre d'octets **au plus**. La lecture peut en rendre moins : c'est
    /// l'appelant qui sait combien il en attend et qui boucle.
    len: usize,
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
struct LectureFaite {
    data: Vec<u16>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionArgs {
    session_id: i64,
}

#[cfg(target_os = "android")]
static GREFFON: OnceLock<tauri::plugin::PluginHandle<Wry>> = OnceLock::new();

#[cfg(not(target_os = "android"))]
static GREFFON: OnceLock<()> = OnceLock::new();

/// Vrai si la plateforme peut, en principe, parler à un appareil USB.
///
/// « En principe » : cela ne dit pas qu'un appareil est branché, seulement que
/// le pont existe. Sur desktop, toujours faux — c'est `serialport` qui s'en
/// charge, pas ce greffon.
pub fn disponible() -> bool {
    GREFFON.get().is_some()
}

#[cfg(target_os = "android")]
fn appeler<A: Serialize, R: serde::de::DeserializeOwned>(
    commande: &str,
    args: A,
) -> Result<R, UsbError> {
    let handle = GREFFON.get().ok_or(UsbError::Indisponible)?;
    handle
        .run_mobile_plugin::<R>(commande, args)
        .map_err(|e| traduire(&e.to_string()))
}

#[cfg(not(target_os = "android"))]
fn appeler<A: Serialize, R: serde::de::DeserializeOwned>(
    _commande: &str,
    _args: A,
) -> Result<R, UsbError> {
    Err(UsbError::Indisponible)
}

/// Le Kotlin ne peut rejeter qu'avec un message.
///
/// Le refus d'autorisation est le seul cas que l'appelant doive pouvoir
/// distinguer — c'est le seul qui se rattrape en redemandant — donc il est
/// marqué par un préfixe convenu entre les deux côtés du pont.
///
/// `cfg(test)` autant que `cfg(target_os = "android")` : la fonction n'est
/// appelée que sur Android, mais c'est une analyse de chaîne, et c'est
/// justement le genre de code qui casse en silence. Le compiler aussi sur le
/// poste de développement permet de la tester là où les tests tournent.
#[cfg(any(target_os = "android", test))]
fn traduire(message: &str) -> UsbError {
    if let Some((_, reste)) = message.split_once(PREFIXE_DEBRANCHE) {
        return UsbError::Debranche {
            session_id: reste.trim().parse().unwrap_or(-1),
        };
    }
    match message.split_once(PREFIXE_REFUS) {
        Some((_, reste)) => UsbError::PermissionRefusee {
            device_id: reste.trim().parse().unwrap_or(-1),
        },
        None => UsbError::Autre(message.to_string()),
    }
}

/// Marqueur de refus d'autorisation, partagé avec `UsbSerialPlugin.kt`.
/// **Les deux côtés doivent rester d'accord sur cette chaîne.**
pub const PREFIXE_REFUS: &str = "PERMISSION_DENIED:";

/// Marqueur de débranchement à chaud, partagé avec `UsbSerialPlugin.kt`.
/// **Les deux côtés doivent rester d'accord sur cette chaîne.**
pub const PREFIXE_DEBRANCHE: &str = "DEVICE_DETACHED:";

/// Tous les appareils USB actuellement branchés.
pub fn list_devices() -> Result<Vec<UsbDeviceInfo>, UsbError> {
    let reponse: ListeAppareils = appeler("listDevices", SansArgument {})?;
    Ok(reponse.devices)
}

/// Une session ouverte sur un appareil, configurée pour du 115200 8N1.
///
/// La fermeture est faite par [`Drop`] : une session laissée ouverte garde
/// l'interface USB réservée, et le branchement suivant échouerait sans que rien
/// n'explique pourquoi.
#[derive(Debug)]
pub struct SerialPort {
    session_id: i64,
    ferme: bool,
}

impl SerialPort {
    /// Ouvre l'appareil, en demandant l'autorisation à l'utilisateur si elle
    /// n'a pas déjà été accordée.
    ///
    /// **L'appel bloque** : côté Kotlin, la demande d'autorisation est une
    /// fenêtre système et une diffusion asynchrone ; la réponse au Rust n'est
    /// donnée qu'une fois l'utilisateur décidé. C'est voulu — l'appelant est
    /// déjà dans un `spawn_blocking` — mais cela interdit d'appeler cette
    /// fonction depuis le fil principal.
    pub fn open(device_id: i32, baud_rate: u32) -> Result<Self, UsbError> {
        let faite: OuvertureFaite = appeler(
            "openDevice",
            OuvrirArgs {
                device_id,
                baud_rate,
                dtr: false,
                rts: false,
            },
        )?;
        Ok(SerialPort {
            session_id: faite.session_id,
            ferme: false,
        })
    }

    /// Écrit une trame complète.
    pub fn write(&mut self, data: &[u8], timeout_ms: u64) -> Result<(), UsbError> {
        let _: SansArgument = appeler(
            "writeBytes",
            EcrireArgs {
                session_id: self.session_id,
                data: data.iter().map(|b| u16::from(*b)).collect(),
                timeout_ms,
            },
        )?;
        Ok(())
    }

    /// Lit **au plus** `len` octets. Un retour vide signifie « rien n'est arrivé
    /// dans le délai », pas une erreur : c'est à l'appelant de décider si le
    /// silence est un échec.
    pub fn read(&mut self, len: usize, timeout_ms: u64) -> Result<Vec<u8>, UsbError> {
        let faite: LectureFaite = appeler(
            "readBytes",
            LireArgs {
                session_id: self.session_id,
                len,
                timeout_ms,
            },
        )?;
        Ok(faite.data.into_iter().map(|v| v as u8).collect())
    }

    /// Vide ce qui traîne en entrée.
    ///
    /// Pendant du `clear(ClearBuffer::Input)` de `serialport`, et pour la même
    /// raison : une réponse en retard de l'échange précédent décalerait
    /// l'alignement de toutes les trames suivantes.
    pub fn purge(&mut self) -> Result<(), UsbError> {
        let _: SansArgument = appeler(
            "purgeInput",
            SessionArgs {
                session_id: self.session_id,
            },
        )?;
        Ok(())
    }

    /// Libère l'appareil. Idempotent — [`Drop`] le rappelle sans dommage.
    pub fn close(&mut self) {
        if self.ferme {
            return;
        }
        self.ferme = true;
        let _: Result<SansArgument, UsbError> = appeler(
            "closeDevice",
            SessionArgs {
                session_id: self.session_id,
            },
        );
    }
}

impl Drop for SerialPort {
    fn drop(&mut self) {
        self.close();
    }
}

/// Initialise le greffon. À brancher sur `tauri::Builder::plugin`.
///
/// Le type de runtime est fixé à [`Wry`] plutôt que laissé générique : le
/// `OnceLock` ci-dessus doit être typé sur un runtime concret, et l'application
/// n'en a qu'un. Un greffon générique obligerait à une conversion de type sans
/// aucun bénéfice ici.
pub fn init() -> TauriPlugin<Wry> {
    Builder::new("usbserial")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                let handle = _api.register_android_plugin(PLUGIN_IDENTIFIER, "UsbSerialPlugin")?;
                let _ = GREFFON.set(handle);
            }
            Ok(())
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sur desktop, le greffon doit se déclarer indisponible **sans paniquer** :
    /// `elitech_serial` s'appuie là-dessus pour choisir entre `serialport` et le
    /// pont USB, et une panique ici ferait tomber toute la détection.
    #[test]
    fn hors_android_la_liaison_est_indisponible() {
        assert!(!disponible());
        assert_eq!(list_devices().unwrap_err(), UsbError::Indisponible);
        assert_eq!(
            SerialPort::open(1, 115_200).unwrap_err(),
            UsbError::Indisponible
        );
    }

    /// Le message d'un refus doit nommer l'appareil : c'est ce que l'écran
    /// affiche, et « autorisation refusée » tout court n'aiderait personne à
    /// comprendre lequel des deux appareils branchés a été refusé.
    #[test]
    fn le_refus_d_autorisation_nomme_l_appareil() {
        let e = UsbError::PermissionRefusee { device_id: 1002 };
        assert!(e.to_string().contains("1002"));
    }

    /// Le préfixe convenu avec le Kotlin est un contrat : s'il change d'un seul
    /// côté, un refus d'autorisation devient une erreur générique et l'écran
    /// cesse de proposer de redemander.
    #[test]
    fn le_prefixe_de_refus_est_celui_attendu_par_le_kotlin() {
        assert_eq!(PREFIXE_REFUS, "PERMISSION_DENIED:");
    }

    /// Même contrat pour le débranchement : ce préfixe est la seule chose qui
    /// sépare « rebranchez le câble » d'un message de transfert illisible.
    #[test]
    fn le_prefixe_de_debranchement_est_celui_attendu_par_le_kotlin() {
        assert_eq!(PREFIXE_DEBRANCHE, "DEVICE_DETACHED:");
    }

    /// Les trois formes de message que le Kotlin peut renvoyer, et la seule
    /// chose qui compte : qu'elles ne se confondent pas.
    #[test]
    fn la_traduction_separe_refus_debranchement_et_reste() {
        assert_eq!(
            traduire("PERMISSION_DENIED:1002"),
            UsbError::PermissionRefusee { device_id: 1002 }
        );
        assert_eq!(
            traduire("DEVICE_DETACHED:7"),
            UsbError::Debranche { session_id: 7 }
        );
        assert_eq!(
            traduire("bulkTransfer a rendu -1"),
            UsbError::Autre("bulkTransfer a rendu -1".to_string())
        );
    }

    /// Un message tronqué ou malformé ne doit pas paniquer : `parse` échoue, on
    /// retombe sur -1 et l'utilisateur reçoit un message imparfait plutôt qu'un
    /// écran mort au milieu d'une lecture de mémoire.
    #[test]
    fn un_identifiant_illisible_ne_fait_pas_paniquer() {
        assert_eq!(
            traduire("DEVICE_DETACHED:"),
            UsbError::Debranche { session_id: -1 }
        );
        assert_eq!(
            traduire("PERMISSION_DENIED:abc"),
            UsbError::PermissionRefusee { device_id: -1 }
        );
    }

    /// Le message d'un débranchement doit dire quoi faire — c'est le seul cas
    /// que l'utilisateur peut corriger lui-même, sur place, sans rien connaître
    /// de l'USB.
    #[test]
    fn le_message_de_debranchement_dit_quoi_faire() {
        let e = UsbError::Debranche { session_id: 3 };
        assert!(e.to_string().contains("Rebranchez"));
    }
}

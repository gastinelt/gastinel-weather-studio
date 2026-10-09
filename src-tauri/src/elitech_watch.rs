//! Surveillance du branchement d'un RC-5, en arrière-plan.
//!
//! ## L'objectif produit
//!
//! Brancher le logger en USB, et que l'application le remarque toute seule :
//! « Logger RC-5 détecté (n° X) — importer ses données ? ». Pas de menu à
//! chercher, pas de port à choisir.
//!
//! ## Pourquoi un sondage, et pourquoi il reste léger
//!
//! Windows sait notifier l'arrivée d'un périphérique (`WM_DEVICECHANGE`), mais
//! il faut pour cela une fenêtre native et sa boucle de messages : ni Tauri 2 ni
//! `serialport` n'en exposent d'accès. Écrire soi-même un pont Win32 ajouterait
//! du code spécifique à une plateforme, à maintenir, pour gagner deux secondes de
//! latence. Le sondage l'emporte donc, à condition de rester **vraiment** léger :
//!
//! * un cycle = **une énumération des ports**, rien d'autre. Aucun port n'est
//!   ouvert, aucune trame n'est émise ;
//! * le port n'est ouvert et interrogé **que** lorsqu'un port CH340 vient
//!   d'apparaître — donc une fois par branchement, jamais en régime établi ;
//! * un CH340 déjà vu et déjà identifié (ou déjà refusé) n'est plus réinterrogé
//!   tant qu'il reste branché.
//!
//! En régime établi — le cas de figure pendant 99,9 % de la vie de
//! l'application — le coût est donc celui d'un appel système toutes les
//! [`INTERVALLE`], ce qui est de l'ordre du bruit de fond.
//!
//! ## Un CH340 n'est pas un RC-5
//!
//! La puce équipe aussi la plupart des clones Arduino. Le fil de surveillance
//! envoie donc une commande d'identification (`DevInfo`) à tout CH340 nouvellement
//! apparu, et n'annonce un logger que si la réponse annonce bien le modèle 50.
//! Deux précautions accompagnent cet envoi :
//!
//! * DTR est maintenu bas à l'ouverture (voir
//!   [`crate::elitech_serial::SerialTransport::open`]) — sans quoi une carte
//!   Arduino branchée à côté redémarrerait à chaque cycle de détection ;
//! * le délai d'attente est réduit à
//!   [`crate::elitech_serial::TIMEOUT_IDENTIFICATION`], pour qu'un appareil muet
//!   ne fige pas la surveillance.
//!
//! ## Le port occupé n'est pas un échec de détection
//!
//! Si ElitechLog est ouvert, il tient le port et l'identification échoue avec
//! [`crate::elitech_serial::SerialError::PortBusy`]. C'est une information utile —
//! un logger *est* probablement branché — et elle est remontée à l'UI telle
//! quelle, avec son message qui dit de fermer ElitechLog. Le port reste dans les
//! « déjà vus » pour ne pas répéter la tentative en boucle ; il sera réessayé au
//! prochain branchement.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use crate::elitech_serial::{identify_port, list_ch340_ports};
use crate::elitech_serial::{Ch340Port, DevInfo, SerialError};

/// Période du sondage.
///
/// 2,5 s : assez court pour que le branchement paraisse instantané, assez long
/// pour que le coût soit indiscernable de zéro.
pub const INTERVALLE: Duration = Duration::from_millis(2_500);

/// Nom de l'événement Tauri. Le frontend écoute cette chaîne exacte.
pub const EVENEMENT: &str = "rc5-detecte";

/// Ce que la surveillance a conclu d'un port fraîchement apparu.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Detection {
    /// Un RC-5 confirmé : modèle vérifié, fiche d'identité lue.
    Rc5 {
        port: Ch340Port,
        device: Box<DevInfo>,
        /// Modèle en clair (`"RC-5"`), pour que l'UI n'ait pas à le déduire.
        model_label: String,
        /// Nombre de mesures en mémoire — le chiffre qui décide l'utilisateur.
        rec_count: u16,
        /// Numéro de série tel que l'appareil le transmet (tronqué, cf.
        /// `elitech_serial`).
        logger_serial: Option<String>,
    },
    /// Un CH340 est apparu, mais ce n'est pas un RC-5 (ou il n'a pas répondu).
    ///
    /// Remonté quand même : distinguer « rien de branché » de « quelque chose est
    /// branché mais ne répond pas » est exactement ce qui manque quand ElitechLog
    /// tient le port.
    Autre {
        port: Ch340Port,
        /// Message déjà rédigé pour l'utilisateur.
        message: String,
        /// `true` si la cause est un port occupé — le seul cas où l'UI a une
        /// action concrète à proposer (fermer ElitechLog).
        port_busy: bool,
    },
    /// Le logger précédemment détecté a été débranché.
    Debranche { port_name: String },
}

/// Ce qu'on retient d'un port déjà examiné, pour ne pas le réinterroger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Rc5,
    PasRc5,
}

/// L'état d'un cycle à l'autre. Isolé du fil d'exécution pour être testable :
/// c'est ici que vit toute la logique « nouveau / déjà vu / disparu ».
#[derive(Debug, Default)]
pub struct Surveillance {
    connus: HashMap<String, Verdict>,
}

impl Surveillance {
    pub fn new() -> Self {
        Surveillance::default()
    }

    /// Un cycle : compare la liste des ports à celle du cycle précédent et
    /// n'interroge que les nouveaux.
    ///
    /// `identifier` est passé en paramètre plutôt qu'appelé directement : c'est
    /// ce qui permet aux tests de rejouer un branchement, un débranchement et un
    /// port occupé sans matériel.
    pub fn cycle<F>(&mut self, ports: &[Ch340Port], mut identifier: F) -> Vec<Detection>
    where
        F: FnMut(&Ch340Port) -> Result<DevInfo, SerialError>,
    {
        let mut evenements = Vec::new();

        // Débranchements d'abord : l'UI doit retirer sa notification avant
        // d'éventuellement en afficher une autre.
        let presents: Vec<&str> = ports.iter().map(|p| p.port_name.as_str()).collect();
        let partis: Vec<String> = self
            .connus
            .keys()
            .filter(|nom| !presents.contains(&nom.as_str()))
            .cloned()
            .collect();
        for nom in partis {
            let verdict = self.connus.remove(&nom);
            // Seul le départ d'un vrai RC-5 intéresse l'utilisateur : un Arduino
            // débranché n'a jamais rien affiché, il n'a rien à retirer.
            if verdict == Some(Verdict::Rc5) {
                evenements.push(Detection::Debranche { port_name: nom });
            }
        }

        for port in ports {
            if self.connus.contains_key(&port.port_name) {
                continue; // déjà examiné, on ne le réveille pas
            }
            match identifier(port) {
                Ok(device) => {
                    self.connus.insert(port.port_name.clone(), Verdict::Rc5);
                    evenements.push(Detection::Rc5 {
                        model_label: device.model_label(),
                        rec_count: device.rec_count,
                        logger_serial: device.device_number.clone(),
                        port: port.clone(),
                        device: Box::new(device),
                    });
                }
                Err(e) => {
                    self.connus.insert(port.port_name.clone(), Verdict::PasRc5);
                    evenements.push(Detection::Autre {
                        port: port.clone(),
                        message: e.to_string(),
                        port_busy: matches!(e, SerialError::PortBusy { .. }),
                    });
                }
            }
        }

        evenements
    }

    /// Oublie tout ce qui a été vu : le prochain cycle réexaminera chaque port.
    ///
    /// Sert au bouton « rechercher à nouveau » : après avoir fermé ElitechLog,
    /// l'utilisateur veut une nouvelle tentative sans avoir à débrancher.
    pub fn reset(&mut self) {
        self.connus.clear();
    }

    /// Les ports actuellement tenus pour des RC-5.
    pub fn ports_rc5(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .connus
            .iter()
            .filter(|(_, verdict)| **verdict == Verdict::Rc5)
            .map(|(nom, _)| nom.clone())
            .collect();
        v.sort();
        v
    }
}

/// Poignée permettant d'arrêter la surveillance et de forcer un réexamen.
///
/// Partagée avec l'état Tauri : la commande « rechercher à nouveau » lève le
/// drapeau, le fil le voit au cycle suivant.
#[derive(Clone)]
pub struct WatchHandle {
    arret: Arc<AtomicBool>,
    reexaminer: Arc<AtomicBool>,
}

impl WatchHandle {
    /// Demande au fil de s'arrêter à la fin du cycle en cours.
    pub fn stop(&self) {
        self.arret.store(true, Ordering::Relaxed);
    }

    /// Demande à oublier les ports déjà examinés (cf. [`Surveillance::reset`]).
    pub fn rescan(&self) {
        self.reexaminer.store(true, Ordering::Relaxed);
    }
}

/// Démarre le fil de surveillance.
///
/// Le même code sur les deux plateformes depuis que l'USB-OTG est branché : la
/// seule différence tient dans ce que [`list_ch340_ports`] va chercher — des
/// ports système d'un côté, l'inventaire d'`UsbManager` de l'autre.
///
/// ## La demande d'autorisation, sur Android
///
/// Ouvrir un appareil USB y fait apparaître une fenêtre système. Un sondage qui
/// réessaierait chaque cycle la ferait donc réapparaître toutes les 2,5 s.
/// C'est la mémoire des ports déjà examinés (`connus`) qui l'en empêche : un
/// appareil refusé est classé « pas un RC-5 » et n'est plus interrogé jusqu'au
/// prochain débranchement — ou jusqu'à un « rechercher à nouveau » explicite,
/// qui est précisément le geste par lequel l'utilisateur redemande la fenêtre.
pub fn spawn<F>(mut emettre: F) -> WatchHandle
where
    F: FnMut(&Detection) + Send + 'static,
{
    let handle = WatchHandle {
        arret: Arc::new(AtomicBool::new(false)),
        reexaminer: Arc::new(AtomicBool::new(false)),
    };
    let interne = handle.clone();

    std::thread::Builder::new()
        .name("rc5-watch".to_string())
        .spawn(move || {
            let mut surveillance = Surveillance::new();
            while !interne.arret.load(Ordering::Relaxed) {
                if interne.reexaminer.swap(false, Ordering::Relaxed) {
                    surveillance.reset();
                }
                // Une énumération qui échoue (pilote en cours d'installation,
                // service occupé) ne doit pas tuer la surveillance : on saute ce
                // cycle et on réessaie au suivant.
                if let Ok(ports) = list_ch340_ports() {
                    for evenement in surveillance.cycle(&ports, |p| identify_port(&p.port_name)) {
                        emettre(&evenement);
                    }
                }
                std::thread::sleep(INTERVALLE);
            }
        })
        .expect("le fil de surveillance du RC-5 n'a pas pu démarrer");

    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elitech_serial::RC5_MODEL_NO;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn port(nom: &str) -> Ch340Port {
        Ch340Port {
            port_name: nom.to_string(),
            usb_serial: None,
            manufacturer: Some("wch.cn".to_string()),
            product: Some("USB-SERIAL CH340".to_string()),
        }
    }

    fn devinfo(serial: &str, rec_count: u16) -> DevInfo {
        DevInfo {
            station_no: 1,
            model_no: RC5_MODEL_NO,
            sample_interval_seconds: 900,
            rec_count,
            start_time: None,
            device_clock: None,
            last_online: None,
            work_status: crate::elitech_serial::WorkStatus::Stopped,
            device_number: Some(serial.to_string()),
            user_info: None,
            upper_limit_celsius: 40.0,
            lower_limit_celsius: -30.0,
            temperature_calibration: 0.0,
            celsius: true,
        }
    }

    #[test]
    fn un_branchement_est_annonce_une_seule_fois() {
        let mut s = Surveillance::new();
        let appels = Rc::new(RefCell::new(0));

        let mut identifier = {
            let appels = Rc::clone(&appels);
            move |_: &Ch340Port| {
                *appels.borrow_mut() += 1;
                Ok(devinfo("E205004333", 10_307))
            }
        };

        let ports = vec![port("COM8")];
        let e = s.cycle(&ports, &mut identifier);
        assert_eq!(e.len(), 1);
        assert!(matches!(e[0], Detection::Rc5 { rec_count: 10_307, .. }));

        // Le cœur de la légèreté : les cycles suivants ne rouvrent pas le port.
        for _ in 0..10 {
            assert!(s.cycle(&ports, &mut identifier).is_empty());
        }
        assert_eq!(*appels.borrow(), 1);
    }

    #[test]
    fn un_debranchement_est_annonce_puis_un_rebranchement_aussi() {
        let mut s = Surveillance::new();
        let mut identifier = |_: &Ch340Port| Ok(devinfo("E205004333", 10));

        s.cycle(&[port("COM8")], &mut identifier);
        let e = s.cycle(&[], &mut identifier);
        assert_eq!(
            e,
            vec![Detection::Debranche {
                port_name: "COM8".to_string()
            }]
        );
        assert!(s.ports_rc5().is_empty());

        // Rebranché : c'est de nouveau un port inconnu, donc réidentifié.
        let e = s.cycle(&[port("COM8")], &mut identifier);
        assert_eq!(e.len(), 1);
        assert!(matches!(e[0], Detection::Rc5 { .. }));
    }

    #[test]
    fn un_ch340_qui_nest_pas_un_rc5_ne_se_fait_pas_passer_pour_un_logger() {
        // Le cas de l'Arduino : la puce est la bonne, l'appareil ne répond pas.
        let mut s = Surveillance::new();
        let mut identifier = |_: &Ch340Port| {
            Err(SerialError::Timeout {
                expected: 160,
                received: 0,
            })
        };
        let e = s.cycle(&[port("COM3")], &mut identifier);
        assert!(matches!(
            e[0],
            Detection::Autre {
                port_busy: false,
                ..
            }
        ));
        assert!(s.ports_rc5().is_empty());
        // Et son débranchement ne fabrique pas d'événement : rien n'avait été
        // annoncé comme logger.
        assert!(s.cycle(&[], &mut identifier).is_empty());
    }

    #[test]
    fn un_port_occupe_est_signale_comme_tel() {
        let mut s = Surveillance::new();
        let mut identifier = |_: &Ch340Port| {
            Err(SerialError::PortBusy {
                port: "COM8".to_string(),
            })
        };
        let e = s.cycle(&[port("COM8")], &mut identifier);
        match &e[0] {
            Detection::Autre {
                port_busy, message, ..
            } => {
                assert!(port_busy);
                // Le message doit nommer le coupable probable : c'est là toute
                // son utilité.
                assert!(message.contains("ElitechLog"));
            }
            autre => panic!("attendu Autre, obtenu {autre:?}"),
        }
    }

    #[test]
    fn reexaminer_permet_de_reessayer_apres_avoir_ferme_elitechlog() {
        let mut s = Surveillance::new();
        let ports = vec![port("COM8")];

        let mut occupe = |_: &Ch340Port| {
            Err(SerialError::PortBusy {
                port: "COM8".to_string(),
            })
        };
        s.cycle(&ports, &mut occupe);
        assert!(s.cycle(&ports, &mut occupe).is_empty(), "déjà examiné");

        s.reset();
        let mut libre = |_: &Ch340Port| Ok(devinfo("E205004333", 42));
        let e = s.cycle(&ports, &mut libre);
        assert!(matches!(e[0], Detection::Rc5 { rec_count: 42, .. }));
    }

    /// Le frontend lit ces champs par leur nom exact, dans `main.js`. Un
    /// renommage côté Rust ne casserait rien à la compilation et se verrait
    /// seulement à l'exécution, sur un poste où un logger est branché — c'est-à-dire
    /// tard, et rarement. Ce test met le contrat par écrit.
    #[test]
    fn la_forme_json_de_levenement_est_celle_que_le_frontend_lit() {
        let json = |d: &Detection| serde_json::to_value(d).unwrap();

        let branche = json(&Detection::Rc5 {
            port: port("COM8"),
            model_label: "RC-5".to_string(),
            rec_count: 10_307,
            logger_serial: Some("E205004333".to_string()),
            device: Box::new(devinfo("E205004333", 10_307)),
        });
        assert_eq!(branche["kind"], "rc5");
        assert_eq!(branche["model_label"], "RC-5");
        assert_eq!(branche["rec_count"], 10_307);
        assert_eq!(branche["logger_serial"], "E205004333");
        assert_eq!(branche["port"]["port_name"], "COM8");

        let occupe = json(&Detection::Autre {
            port: port("COM8"),
            message: "…".to_string(),
            port_busy: true,
        });
        assert_eq!(occupe["kind"], "autre");
        assert_eq!(occupe["port_busy"], true);
        assert_eq!(occupe["message"], "…");

        let parti = json(&Detection::Debranche {
            port_name: "COM8".to_string(),
        });
        assert_eq!(parti["kind"], "debranche");
        assert_eq!(parti["port_name"], "COM8");

        // Le nom de l'événement aussi : `main.js` l'écoute en dur.
        assert_eq!(EVENEMENT, "rc5-detecte");
    }

    #[test]
    fn deux_ch340_simultanes_sont_traites_separement() {
        let mut s = Surveillance::new();
        let mut identifier = |p: &Ch340Port| {
            if p.port_name == "COM8" {
                Ok(devinfo("E205004333", 10_307))
            } else {
                Err(SerialError::Timeout {
                    expected: 160,
                    received: 0,
                })
            }
        };
        let e = s.cycle(&[port("COM3"), port("COM8")], &mut identifier);
        assert_eq!(e.len(), 2);
        assert_eq!(s.ports_rc5(), vec!["COM8".to_string()]);
    }
}

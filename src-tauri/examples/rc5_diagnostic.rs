//! Diagnostic de la liaison série avec un RC-5, étape par étape.
//!
//! Répond à la seule question qui se pose quand la détection automatique ne
//! trouve rien : **où ça s'arrête**. Chaque étape est affichée séparément, avec
//! les octets reçus, pour qu'un échec désigne son point de rupture au lieu de se
//! résumer à « pas de logger ».
//!
//! ```text
//! cargo run --example rc5_diagnostic              # cherche tout seul
//! cargo run --example rc5_diagnostic -- COM8      # port imposé
//! ```
//!
//! Ce n'est pas un test : rien n'échoue ici, on observe. Les invariants du
//! protocole sont vérifiés par `cargo test` (`elitech_serial` et
//! `tests/rc5_direct.rs`).
//!
//! Aucune commande d'écriture n'est émise : l'appareil n'est jamais modifié.

// Cet exemple parle au port série, qui n'existe pas sur la cible Android : la
// dépendance `serialport` y est retirée (voir `Cargo.toml`). Sans ce garde,
// `cargo check --target …-linux-android --all-targets` échoue sur un outil de
// diagnostic desktop qui n'a de toute façon rien à y faire.
#[cfg(target_os = "android")]
fn main() {
    eprintln!("Diagnostic RC-5 : outil desktop, sans objet sur Android (pas de port série).");
}

#[cfg(not(target_os = "android"))]
use gws_lib::anomaly::JumpThresholds;
#[cfg(not(target_os = "android"))]
use gws_lib::elitech_serial::{self, CH340_PID, CH340_VID};

#[cfg(not(target_os = "android"))]
fn main() {
    let port_impose = std::env::args().nth(1);

    // --- Étape 1 : tous les ports série, CH340 ou non ---------------------
    //
    // Montrer aussi les autres est délibéré : « aucun port du tout » et « des
    // ports, mais pas de CH340 » demandent deux réactions différentes (pilote
    // absent d'un côté, mauvais câble ou appareil éteint de l'autre).
    println!("== 1. Ports série du système ==");
    match serialport::available_ports() {
        Ok(ports) if ports.is_empty() => println!("  aucun port série."),
        Ok(ports) => {
            for p in &ports {
                match &p.port_type {
                    serialport::SerialPortType::UsbPort(usb) => println!(
                        "  {} — USB {:04x}:{:04x} {} {}{}",
                        p.port_name,
                        usb.vid,
                        usb.pid,
                        usb.manufacturer.clone().unwrap_or_default(),
                        usb.product.clone().unwrap_or_default(),
                        if usb.vid == CH340_VID && usb.pid == CH340_PID {
                            "  <-- CH340"
                        } else {
                            ""
                        }
                    ),
                    autre => println!("  {} — {autre:?}", p.port_name),
                }
            }
        }
        Err(e) => println!("  énumération impossible : {e}"),
    }

    println!("\n== 2. Ports CH340 retenus ==");
    let ch340 = match elitech_serial::list_ch340_ports() {
        Ok(v) => v,
        Err(e) => {
            println!("  {e}");
            return;
        }
    };
    if ch340.is_empty() {
        println!("  aucun. Un RC-5 branché apparaîtrait ici.");
    }
    for p in &ch340 {
        println!("  {} — {:?}", p.port_name, p.product);
    }

    // --- Étape 3 : identification -----------------------------------------
    let candidats: Vec<String> = match &port_impose {
        Some(nom) => vec![nom.clone()],
        None => ch340.iter().map(|p| p.port_name.clone()).collect(),
    };
    if candidats.is_empty() {
        println!("\nRien à interroger. Branchez le logger et relancez.");
        return;
    }

    println!("\n== 3. Identification (DevInfo, 160 octets attendus) ==");
    let mut retenu = None;
    for nom in &candidats {
        match elitech_serial::identify_port(nom) {
            Ok(info) => {
                println!("  {nom} : {} confirmé", info.model_label());
                println!("    n° de série (tronqué)  : {:?}", info.device_number);
                println!("    description            : {:?}", info.user_info);
                println!("    mesures en mémoire     : {}", info.rec_count);
                println!("    cadence                : {} s", info.sample_interval_seconds);
                println!("    première mesure        : {:?}", info.start_time);
                println!("    horloge de l'appareil  : {:?}", info.device_clock);
                println!("    heure du PC (UTC)      : {}", chrono::Utc::now().naive_utc());
                if let Some(d) = info.clock_drift_seconds(chrono::Utc::now().naive_utc()) {
                    println!(
                        "    écart horloge / UTC    : {d:+} s (retirer le fuseau réglé \
                         sur le logger pour obtenir la dérive)"
                    );
                }
                println!("    état                   : {}", info.work_status.label_fr());
                println!("    seuils d'alarme        : {} / {} °C",
                    info.lower_limit_celsius, info.upper_limit_celsius);
                println!("    correction interne     : {:+.1} °C", info.temperature_calibration);
                retenu = Some(nom.clone());
                break;
            }
            Err(e) => println!("  {nom} : {e}"),
        }
    }

    let Some(port) = retenu else {
        println!("\nAucun RC-5 n'a répondu. La communication de base ne passe pas — \
                  inutile d'aller plus loin.");
        return;
    };

    // --- Étape 4 : lecture complète ---------------------------------------
    println!("\n== 4. Lecture de l'historique complet ==");
    let debut = std::time::Instant::now();
    match elitech_serial::read_port(&port, &JumpThresholds::default()) {
        Ok(lecture) => {
            let duree = debut.elapsed();
            println!("  {} mesures en {:.1} s", lecture.measurements.len(), duree.as_secs_f64());
            if let (Some(p), Some(d)) = (
                lecture.measurements.first(),
                lecture.measurements.last(),
            ) {
                println!("  période brute : {} → {}", p.raw_timestamp, d.raw_timestamp);
            }
            let temps: Vec<f64> = lecture
                .measurements
                .iter()
                .map(|m| m.temperature_celsius)
                .collect();
            let min = temps.iter().copied().fold(f64::INFINITY, f64::min);
            let max = temps.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let moy = temps.iter().sum::<f64>() / temps.len() as f64;
            // Ces trois chiffres se comparent directement à la feuille « Résumé »
            // d'un export xlsx du même logger : c'est le contrôle le plus rapide.
            println!("  min / moy / max : {min:.1} / {moy:.1} / {max:.1} °C");
            println!("  5 premières :");
            for m in lecture.measurements.iter().take(5) {
                println!("    n°{} · {} · {} °C", m.source_line, m.raw_timestamp, m.temperature_celsius);
            }
            println!("  5 dernières :");
            for m in lecture.measurements.iter().rev().take(5).collect::<Vec<_>>().iter().rev() {
                println!("    n°{} · {} · {} °C", m.source_line, m.raw_timestamp, m.temperature_celsius);
            }
            for a in &lecture.anomalies {
                println!("  anomalie : {}", a.message);
            }
            for w in &lecture.warnings {
                println!("  avertissement : {}", w.message);
            }
        }
        Err(e) => println!("  échec : {e}"),
    }
}

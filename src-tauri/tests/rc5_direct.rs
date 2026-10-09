//! **Validation croisée** : lecture directe du RC-5 contre export xlsx.
//!
//! ## Ce que ce test prouve
//!
//! Les deux voies d'import du **même logger physique** — le port série et le
//! fichier produit par ElitechLog — donnent exactement les mêmes mesures. C'est
//! l'étape qui fait la différence entre « un décodage qui semble marcher » et un
//! décodage vérifié : l'export xlsx est la référence connue comme fiable, et
//! toute divergence de la voie directe est un défaut de la voie directe.
//!
//! ## Les deux jeux de données
//!
//! Ils viennent du même appareil, le RC-5 n° `EFE205004333`, et de la même
//! mémoire — 10 307 mesures du 1ᵉʳ mai au 16 août 2026, cadence 15 minutes :
//!
//! | Fixture | Origine |
//! |---|---|
//! | `docs/samples/elitech_rc5_reel.xls` | export « Excel » d'ElitechLogWin 8.0.5, le 19 août 2026 |
//! | `docs/samples/rc5_direct/*.bin` | **trames série brutes**, capturées sur le port COM le 20 août 2026 |
//!
//! Les fixtures directes sont les octets **tels qu'ils sont sortis du câble**,
//! pas une reconstruction : `devinfo.bin` (160 octets), `header.bin` (11 octets)
//! et `pages.bin` (les 21 réponses `DataBody` concaténées, 20 656 octets). Elles
//! sont rejouées par le décodeur de production, celui-là même qu'appelle la
//! lecture réelle — le test ne teste donc pas une copie du code.
//!
//! ## Résultat au moment de l'écriture
//!
//! Concordance **totale** : 10 307 mesures de part et d'autre, aucun écart
//! d'horodatage, aucun écart de température. La seule divergence constatée entre
//! les deux voies porte sur le **numéro de série** — voir
//! `le_numero_de_serie_est_la_seule_divergence_connue`, qui la fige.

use chrono::Utc;
use std::path::PathBuf;

use gws_lib::anomaly::JumpThresholds;
use gws_lib::elitech_serial::{self, RC5_MODEL_NO};
use gws_lib::elitech_xlsx::parse_elitech_xlsx;
use gws_lib::fingerprint::series_fingerprint;
use gws_lib::model::{apply_clock_offset, Measurement};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn racine() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("le dossier parent de src-tauri existe")
        .to_path_buf()
}

fn echantillon(chemin: &str) -> Vec<u8> {
    let p = racine().join("docs").join("samples").join(chemin);
    std::fs::read(&p).unwrap_or_else(|e| panic!("fixture introuvable : {} ({e})", p.display()))
}

/// Rejoue la capture série et renvoie la lecture décodée.
fn lecture_directe() -> gws_lib::elitech_serial::Rc5Reading {
    elitech_serial::decode_reading(
        &echantillon("rc5_direct/devinfo.bin"),
        &echantillon("rc5_direct/header.bin"),
        &echantillon("rc5_direct/pages.bin"),
        &JumpThresholds::default(),
    )
    .expect("la capture série réelle se décode")
}

// ---------------------------------------------------------------------------
// Partie A — la capture elle-même est cohérente
// ---------------------------------------------------------------------------

#[test]
fn la_capture_serie_reelle_se_decode_entierement() {
    let lecture = lecture_directe();

    assert_eq!(lecture.device.model_no, RC5_MODEL_NO);
    assert_eq!(lecture.device.station_no, 1);
    assert_eq!(lecture.device.sample_interval_seconds, 900);
    assert_eq!(lecture.header.rec_count, 10_307);
    assert_eq!(lecture.measurements.len(), 10_307);

    // Le logger était arrêté : aucune mesure en cours, donc aucun avertissement
    // « enregistrement en marche ». Le lot est figé, et rejouable à l'identique.
    assert!(
        lecture.warnings.is_empty(),
        "avertissements inattendus : {:?}",
        lecture.warnings
    );
}

#[test]
fn toutes_les_sommes_de_controle_de_la_capture_sont_bonnes() {
    // `decode_reading` les vérifie déjà ; ce test le dit explicitement, page par
    // page, pour qu'une fixture corrompue au fil des commits se signale d'elle-même.
    let pages = echantillon("rc5_direct/pages.bin");
    let mut offset = 0usize;
    let mut total = 0usize;
    for (page, count) in elitech_serial::page_plan(10_307, elitech_serial::RC5_PAGE_SIZE) {
        let fin = offset + count * 2 + 2;
        let valeurs = elitech_serial::decode_data_page(&pages[offset..fin], count)
            .unwrap_or_else(|e| panic!("page {page} : {e}"));
        assert_eq!(valeurs.len(), count);
        total += count;
        offset = fin;
    }
    assert_eq!(offset, pages.len(), "octets restants après la dernière page");
    assert_eq!(total, 10_307);
}

// ---------------------------------------------------------------------------
// Partie D — la validation croisée proprement dite
// ---------------------------------------------------------------------------

#[test]
fn les_deux_voies_donnent_exactement_les_memes_mesures() {
    let directe = lecture_directe();
    let xlsx = parse_elitech_xlsx(&echantillon("elitech_rc5_reel.xls"))
        .expect("l'export xlsx de référence se lit");

    assert_eq!(
        directe.measurements.len(),
        xlsx.measurements.len(),
        "nombre de mesures différent entre la lecture directe et l'export xlsx"
    );

    // Comparaison **mesure par mesure**, pas seulement des bornes ou d'un
    // résumé : un décalage d'une seule ligne au milieu de la série ne changerait
    // ni le compte, ni la période, ni la moyenne de façon visible.
    let mut ecarts_temps = Vec::new();
    let mut ecarts_valeurs = Vec::new();
    for (i, (d, x)) in directe
        .measurements
        .iter()
        .zip(xlsx.measurements.iter())
        .enumerate()
    {
        if d.raw_timestamp != x.raw_timestamp {
            ecarts_temps.push((i + 1, d.raw_timestamp, x.raw_timestamp));
        }
        // Égalité exacte, et non à epsilon près : les deux voies produisent des
        // dixièmes de degré. `222 / 10.0` donne le même `f64` des deux côtés,
        // que le 222 vienne de deux octets ou de la chaîne « 22,2 ».
        if d.temperature_celsius != x.temperature_celsius {
            ecarts_valeurs.push((i + 1, d.temperature_celsius, x.temperature_celsius));
        }
        assert_eq!(
            d.humidity_percent, x.humidity_percent,
            "mesure {} : humidité divergente",
            i + 1
        );
    }

    assert!(
        ecarts_temps.is_empty(),
        "{} écart(s) d'horodatage, 5 premiers : {:?}",
        ecarts_temps.len(),
        &ecarts_temps[..ecarts_temps.len().min(5)]
    );
    assert!(
        ecarts_valeurs.is_empty(),
        "{} écart(s) de température, 5 premiers : {:?}",
        ecarts_valeurs.len(),
        &ecarts_valeurs[..ecarts_valeurs.len().min(5)]
    );
}

#[test]
fn les_deux_voies_ont_la_meme_empreinte_donc_le_meme_doublon() {
    // C'est l'exigence de la partie C.4 : relire en direct un logger déjà importé
    // par fichier doit être vu comme un ré-import, pas silencieusement dupliqué.
    // L'empreinte porte sur les mesures, jamais sur le contenant — c'est
    // précisément ce qui rend les deux voies comparables.
    let directe = lecture_directe();
    let xlsx = parse_elitech_xlsx(&echantillon("elitech_rc5_reel.xls")).unwrap();

    // Même conversion que `commands::to_measurements`, avec un décalage nul :
    // l'empreinte porte de toute façon sur `raw_timestamp`, donc elle ne dépend
    // pas de la correction d'horloge (c'est tout l'intérêt du choix fait dans
    // `fingerprint.rs`).
    let en_mesures = |brutes: &[gws_lib::elitech_import::RawMeasurement]| -> Vec<Measurement> {
        brutes
            .iter()
            .map(|m| {
                let raw_utc = m.raw_timestamp.and_utc();
                Measurement {
                    humidity_percent: m.humidity_percent,
                    ..Measurement::new(
                        "serie-de-test",
                        apply_clock_offset(raw_utc, 0),
                        raw_utc,
                        m.temperature_celsius,
                    )
                }
            })
            .collect()
    };

    assert_eq!(
        series_fingerprint(&en_mesures(&directe.measurements)),
        series_fingerprint(&en_mesures(&xlsx.measurements)),
    );
}

#[test]
fn les_deux_voies_signalent_les_memes_anomalies() {
    // La partie C.5 : c'est bien le module `anomaly` partagé qui travaille, pas
    // une seconde détection écrite pour la voie série.
    let directe = lecture_directe();
    let xlsx = parse_elitech_xlsx(&echantillon("elitech_rc5_reel.xls")).unwrap();

    assert_eq!(directe.anomalies.len(), xlsx.anomalies.len());
    for (d, x) in directe.anomalies.iter().zip(xlsx.anomalies.iter()) {
        assert_eq!(d.from_timestamp, x.from_timestamp);
        assert_eq!(d.to_timestamp, x.to_timestamp);
        assert_eq!(d.delta_c, x.delta_c);
        // `source_line` diffère volontairement d'une unité : le xlsx compte les
        // lignes de la feuille (en-tête comprise), la voie directe les numéros
        // d'enregistrement. Voir `elitech_serial::build_measurements`.
        assert_eq!(d.source_line + 1, x.source_line);
    }
}

#[test]
fn les_metadonnees_concordent_avec_la_feuille_de_resume() {
    let directe = lecture_directe();
    let xlsx = parse_elitech_xlsx(&echantillon("elitech_rc5_reel.xls")).unwrap();

    assert_eq!(directe.device.model_label(), "RC-5");
    assert_eq!(
        xlsx.metadata.device_model.as_deref(),
        Some("RC-5"),
        "le classeur annonce bien le même modèle"
    );
    assert_eq!(
        Some(directe.device.sample_interval_seconds),
        xlsx.metadata.sample_interval_seconds,
        "cadence : « 15m » côté classeur, 900 s côté série"
    );
    // Les seuils d'alarme du résumé (« H1: Supérieur: 40,0°C », « L1: Inférieur:
    // -30,0°C ») se retrouvent au dixième près dans la fiche de l'appareil.
    assert_eq!(directe.device.upper_limit_celsius, 40.0);
    assert_eq!(directe.device.lower_limit_celsius, -30.0);
    // « Description du voyage » du classeur = `user_info` du protocole.
    assert_eq!(
        directe.device.user_info.as_deref(),
        Some("RC-5 Data Logger")
    );
}

#[test]
fn le_numero_de_serie_est_la_seule_divergence_connue() {
    // Elle est documentée, pas masquée : le champ du protocole fait 10 octets,
    // le numéro complet en compte 12. Ce test fige l'écart pour qu'un futur
    // firmware qui le corrigerait se fasse remarquer.
    let directe = lecture_directe();
    let xlsx = parse_elitech_xlsx(&echantillon("elitech_rc5_reel.xls")).unwrap();

    let serie_directe = directe.logger_serial().expect("l'appareil donne un numéro");
    let serie_xlsx = xlsx
        .metadata
        .logger_serial
        .clone()
        .expect("le classeur donne un numéro");

    assert_eq!(serie_directe, "E205004333");
    assert_eq!(serie_xlsx, "EFE205004333");
    assert_ne!(serie_directe, serie_xlsx, "l'écart est réel");

    // …mais les deux désignent bien le même appareil, et l'application sait le
    // reconnaître.
    assert!(elitech_serial::serials_match(&serie_directe, &serie_xlsx));
}

// ---------------------------------------------------------------------------
// Le matériel réel, quand il est branché
// ---------------------------------------------------------------------------

/// Lecture d'un vrai RC-5, s'il est branché sur ce poste.
///
/// Ignoré par défaut : un test rouge parce qu'aucun logger n'est branché ne
/// renseignerait sur rien — même convention que pour les fichiers de 38 Mo de
/// `sources_reelles.rs`. À lancer avec le logger branché par :
///
/// ```text
/// cargo test --test rc5_direct -- --ignored --nocapture
/// ```
///
/// Ce qu'il vérifie va au-delà de la capture rejouée : que l'énumération des
/// ports trouve le CH340, que l'identification confirme le modèle, et que la
/// lecture complète — 21 échanges réels — redonne bien les mêmes mesures que
/// l'export xlsx.
#[test]
#[ignore = "nécessite un RC-5 branché en USB"]
fn lecture_du_materiel_reel_branche() {
    let ports = elitech_serial::list_ch340_ports().expect("énumération des ports");
    println!("Ports CH340 détectés : {ports:?}");
    assert!(
        !ports.is_empty(),
        "aucun CH340 branché — ce test suppose un RC-5 sur le poste"
    );

    let (port, info) = elitech_serial::find_rc5().expect("un RC-5 répond");
    println!(
        "RC-5 sur {} : n° {:?}, {} mesure(s), horloge {:?}, état « {} »",
        port.port_name,
        info.device_number,
        info.rec_count,
        info.device_clock,
        info.work_status.label_fr()
    );
    assert_eq!(info.model_no, RC5_MODEL_NO);

    let lecture = elitech_serial::read_port(&port.port_name, &JumpThresholds::default())
        .expect("la lecture complète aboutit");
    assert_eq!(lecture.measurements.len(), usize::from(lecture.header.rec_count));

    // Comparaison avec l'export xlsx **seulement** si le logger contient encore
    // le même lot qu'au moment de la capture. Un logger relancé depuis a d'autres
    // données, et c'est normal : on ne fait pas échouer le test pour autant.
    let xlsx = parse_elitech_xlsx(&echantillon("elitech_rc5_reel.xls")).unwrap();
    if lecture.measurements.len() == xlsx.measurements.len()
        && lecture.measurements.first().map(|m| m.raw_timestamp)
            == xlsx.measurements.first().map(|m| m.raw_timestamp)
    {
        for (i, (d, x)) in lecture
            .measurements
            .iter()
            .zip(xlsx.measurements.iter())
            .enumerate()
        {
            assert_eq!(d.raw_timestamp, x.raw_timestamp, "mesure {}", i + 1);
            assert_eq!(d.temperature_celsius, x.temperature_celsius, "mesure {}", i + 1);
        }
        println!(
            "Validation croisée sur matériel réel : {} mesures identiques à l'export xlsx.",
            lecture.measurements.len()
        );
    } else {
        println!(
            "Le logger ne contient plus le lot de référence ({} mesures depuis {:?}) : \
             comparaison au xlsx ignorée.",
            lecture.measurements.len(),
            lecture.measurements.first().map(|m| m.raw_timestamp)
        );
    }
}

/// Vérification de l'horloge **sur le matériel réel** — non destructive.
///
/// ```text
/// cargo test --test rc5_direct -- --ignored --nocapture
/// ```
///
/// Elle n'ouvre qu'une session et n'émet qu'une commande de lecture : c'est le
/// même échange que l'identification. Le test n'affirme rien sur la valeur de la
/// dérive — elle dépend du logger qu'on a sous la main — mais il vérifie
/// l'invariant qui compte : **appliquer la correction suggérée à l'heure lue
/// redonne l'heure de référence**, au fuseau déclaré près.
///
/// ## Et l'effacement ?
///
/// Il n'y a **volontairement pas** de test matériel pour `reset_port`. Un test
/// ignoré reste un test qu'on lance un jour d'un `--ignored` global : celui-ci
/// effacerait la mémoire du logger branché, c'est-à-dire potentiellement des
/// relevés jamais importés. La commande d'écriture se valide à la main, sur un
/// logger dont les données ont d'abord été enregistrées — voir le README, § 4ter.
#[test]
#[ignore = "nécessite un RC-5 branché en USB"]
fn l_horloge_du_materiel_reel_se_verifie() {
    let (port, _) = elitech_serial::find_rc5().expect("un RC-5 répond");

    let reference = Utc::now();
    let (info, controle) = elitech_serial::check_clock_on_port(&port.port_name, 3600, reference)
        .expect("la lecture de l'horloge aboutit");

    println!(
        "Horloge du logger : {:?} · écart/UTC {:?} s · dérive (UTC+1) {:?} s · {}",
        info.device_clock,
        controle.offset_vs_utc_seconds,
        controle.drift_seconds,
        controle.message
    );

    let Some(horloge) = controle.device_clock else {
        // Un logger dont l'horloge est illisible est un cas prévu, pas un échec :
        // le décodeur le signale sans faire échouer la lecture.
        assert_eq!(
            controle.severity,
            elitech_serial::ClockDriftSeverity::Unreadable
        );
        return;
    };

    // L'invariant : brut + correction = UTC vrai.
    let correction = controle.suggested_clock_offset_seconds.unwrap();
    assert_eq!(
        horloge + chrono::Duration::seconds(correction),
        reference.naive_utc(),
        "la correction suggérée doit ramener l'heure lue sur la référence"
    );
}

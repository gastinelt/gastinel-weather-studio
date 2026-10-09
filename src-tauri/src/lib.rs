//! Gastinel Weather Studio — socle applicatif.
//!
//! Découpage des modules :
//!
//! | Module              | Rôle |
//! |---------------------|------|
//! | [`api_keys`]        | Lecture des clés de `Clee.API/` — **commune aux deux fournisseurs** |
//! | [`groq`]            | Agent IA : client Groq, system prompt métier, contexte injecté |
//! | [`model`]           | Entités du domaine + **règle de validation catégorie/taille** |
//! | [`db`]              | Schéma SQLite, migrations, requêtes |
//! | [`stats`]           | Calculs dérivés (min/moy/max quotidiens, amplitudes…) |
//! | [`parsing`]         | Briques de parsing (dates, nombres, séparateurs) |
//! | [`anomaly`]         | Détection des sauts de température anormalement rapides |
//! | [`elitech_xlsx`]    | Parseur de l'export ElitechLog **réel** (xlsx déguisé en `.xls`) |
//! | [`elitech_import`]  | Parseur tabulaire ElitechLog — hypothèse, sans fichier réel connu |
//! | [`elitech_serial`]  | Lecture **directe** du RC-5 par le port série (CH340) |
//! | `tauri-plugin-usbserial` | Greffon local : le même RC-5, mais par l'USB-OTG d'Android |
//! | [`elitech_watch`]   | Surveillance du branchement d'un RC-5, en arrière-plan |
//! | [`infoclimat`]      | Parseur Infoclimat, **JSON et CSV** (fichier **et** appel direct) |
//! | [`romma_import`]    | Parseur des archives ROMMA (`archives_*.csv`) |
//! | [`infoclimat_client`] | Client HTTP Infoclimat : clé, tranches de 7 jours, erreur d'IP |
//! | [`http_client`]     | Fabrique commune des clients HTTP — **et le TLS d'Android** |
//! | [`manual_import`]   | Import CSV à colonnes mappées (mode de secours) |
//! | [`manual_entry`]    | **Saisie manuelle sur site** : une valeur tapée, sans fichier |
//! | [`crossing`]        | Croisement de données : fenêtre temporelle, agrégats et courbes |
//! | [`commands`]        | Commandes Tauri exposées au frontend |
//! | [`error`]           | Erreur unique sérialisable vers l'UI |
//! | [`fingerprint`]     | Empreinte de contenu d'un lot importé — détection de ré-import |
//! | [`tiles`]           | Cache disque des tuiles de carte, usage hors-ligne sur le terrain |
//! | [`export`]          | Export CSV / TXT / HTML (page 2 du bloc Stations) |
//! | [`share`]           | Partage de points par code à 6 caractères (serveur `gws-share`) |
//! | [`update`]          | Avertissement de mise à jour, désactivable dans les Paramètres |

pub mod ai_import;
pub mod altimetrie;
pub mod anomaly;
pub mod annulation;
pub mod api_keys;
pub mod commands;
pub mod cloud;
pub mod cloud_mois;
pub mod crossing;
pub mod croisement_mensuel;
pub mod db;
pub mod demo;
pub mod drop_files;
pub mod elitech_import;
pub mod elitech_serial;
pub mod elitech_watch;
pub mod elitech_xlsx;
pub mod error;
pub mod export;
pub mod extensions;
pub mod extensions_catalogue;
pub mod fingerprint;
pub mod groq;
pub mod http_client;
pub mod import_detect;
pub mod infoclimat;
pub mod infoclimat_client;
pub mod maj_gardien;
pub mod manual_entry;
pub mod manual_import;
pub mod model;
pub mod package;
pub mod parsing;
pub mod quota_partage;
pub mod relais_config;
pub mod romma_import;
pub mod share;
pub mod ssh_relay;
pub mod stats;
pub mod tiles;
pub mod update;

use tauri::{Emitter, Manager};

/// Nom du fichier de base, créé dans le répertoire de données de l'application
/// (`%APPDATA%\fr.gastinel.weatherstudio\` sous Windows).
pub const DATABASE_FILE: &str = "gastinel-weather-studio.sqlite3";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Mode gardien de mise à jour (`--gardien-maj`) : aucune fenêtre, ni base,
    // ni instance unique — il surveille la nouvelle version puis quitte.
    if let Some(code) = maj_gardien::executer_si_demande() {
        std::process::exit(code);
    }
    // Mise à jour en attente de confirmation : rafraîchit le marqueur pendant
    // le setup ; sinon, supprime un éventuel `.exe.old` résiduel.
    maj_gardien::au_demarrage();

    // Relance après l'application d'un paquet `.gws` : l'ancienne instance
    // vient de se terminer, on lui laisse le temps de libérer le verrou
    // d'instance unique avant de le demander à notre tour.
    let apres_maj = std::env::args().any(|a| a == package::ARG_APRES_MAJ);
    if apres_maj {
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }

    let builder = tauri::Builder::default();
    // Doit être le premier greffon monté (cf. sa documentation).
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
        if let Some(fenetre) = app.get_webview_window("main") {
            let _ = fenetre.unminimize();
            let _ = fenetre.set_focus();
        }
        if let Some(chemin) = argv.iter().skip(1).find(|a| a.to_lowercase().ends_with(".gws")) {
            if let Err(e) = app.emit("gws-paquet-ouvert", chemin.clone()) {
                eprintln!("[gws] paquet .gws non transmis : {e}");
            }
        }
    }));
    builder
        .plugin(tauri_plugin_dialog::init())
        // Monté pour son API Rust seule : `save_export_file` s'en sert pour
        // écrire dans ce que le sélecteur de fichiers a rendu — un chemin sur
        // desktop, une URI `content://` sur Android. Aucune de ses commandes
        // n'est autorisée côté frontend (cf. `capabilities/default.json`).
        .plugin(tauri_plugin_fs::init())
        // Liaison série USB-OTG. Sans effet sur desktop — la façade se déclare
        // indisponible et `elitech_serial` reste sur `serialport`. Sur Android
        // c'est le seul accès possible au RC-5 (voir le greffon et § 4quater).
        .plugin(tauri_plugin_usbserial::init())
        // Ouverture d'une URL dans le navigateur du système (lien « obtenir une
        // clé » de l'écran de première ouverture). Monté pour son API Rust
        // seule : le frontend passe par `open_external_url`, qui n'accepte
        // qu'une URL d'une liste fermée.
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            // `GWS_DATA_DIR` (facultatif) remplace le dossier de données : sert à
            // lancer une instance de développement ou de mesure sur un dossier
            // jetable, sans jamais toucher à la base réelle. Absent ou vide, rien
            // ne change.
            let data_dir = match std::env::var_os("GWS_DATA_DIR").filter(|v| !v.is_empty()) {
                Some(dossier) => {
                    let dossier = std::path::PathBuf::from(dossier);
                    std::fs::create_dir_all(&dossier)?;
                    println!("[gws] GWS_DATA_DIR : dossier de données remplacé");
                    dossier
                }
                None => app.path().app_data_dir()?,
            };
            let db_path = data_dir.join(DATABASE_FILE);

            // Utile quand on cherche la base pour l'inspecter avec un client SQLite.
            println!("[gws] base de données : {}", db_path.display());

            // Les clés d'API sont cherchées dans `Clee.API/` à la racine du
            // projet — une notion qui n'existe pas sur un téléphone. On déclare
            // donc le répertoire de données comme racine supplémentaire : sur
            // Android c'est le seul endroit où l'on puisse déposer un
            // `Clee.API/API.Groq.env`, et sur desktop cela n'enlève rien.
            api_keys::set_extra_root(&data_dir);

            let db = db::Db::open(&db_path)?;
            let tiles_cache_dir = data_dir.join(tiles::CACHE_DIR_NAME);
            let state = commands::AppState::new(db, tiles_cache_dir);

            // Surveillance du branchement d'un RC-5. Elle démarre avec
            // l'application et n'en sort qu'un événement : c'est le frontend qui
            // décide d'en faire une notification ou non.
            //
            // Un échec de démarrage ne doit pas empêcher l'application de
            // s'ouvrir — l'import par fichier n'a que faire d'un port série.
            let emetteur = app.handle().clone();
            let watch = elitech_watch::spawn(move |detection| {
                if let Err(e) = emetteur.emit(elitech_watch::EVENEMENT, detection) {
                    eprintln!("[gws] événement RC-5 non transmis : {e}");
                }
            });
            *state.rc5_watch.lock().unwrap() = Some(watch);

            // Détection d'un fichier .gws passé en argument de ligne de commande (ex: double-clic Windows)
            if let Some(arg) = std::env::args().skip(1).find(|a| a.to_lowercase().ends_with(".gws")) {
                let p = std::path::PathBuf::from(&arg);
                if p.exists() {
                    println!("[gws] Paquet .gws détecté au démarrage : {}", p.display());
                    *state.startup_package.lock().unwrap() = Some(p.to_string_lossy().to_string());
                }
            }

            *state.apres_maj.lock().unwrap() = apres_maj;
            app.manage(state);

            // Extensions : manifestes lus maintenant, bibliothèques chargées
            // en arrière-plan — un échec ne retarde ni n'empêche le démarrage.
            // Mises à jour d'extensions préparées pendant qu'elles étaient chargées.
            extensions_catalogue::appliquer_en_attente(&data_dir);
            // Extensions retirées du logiciel : supprimées avant tout chargement.
            extensions_catalogue::retirer_extensions_abandonnees(&data_dir);
            let hote = extensions::ExtensionHost::new(&data_dir, &db_path, env!("CARGO_PKG_VERSION"));
            let hote_fil = hote.clone();
            std::thread::spawn(move || {
                hote_fil.charger_actives();
                for x in hote_fil.list().iter().filter(|x| x.enabled) {
                    println!("[gws] extension {} : {:?}", x.manifest.id, x.state);
                }
            });
            app.manage(hote);
            // Configuration signée du relai : un relevé ~30 s après le
            // lancement, seulement si le relai SSH est activé (plancher de
            // 10 minutes compris). Tâche de fond unique, sans minuterie.
            relais_config::planifier_au_demarrage(app.handle().clone());
            // Base ouverte : l'interface a maintenant `maj_gardien::DELAIS.silence`
            // pour confirmer le démarrage (`consommer_apres_maj`).
            maj_gardien::fin_du_setup();
            Ok(())
        })
        // Panneaux des extensions : `http://gwsext.localhost/<id>/<fichier>`
        // sous Windows. Seuls les fichiers du dossier `ui/` sont servis, plus
        // les images du dossier de données de l'extension sous `_donnees/`.
        .register_uri_scheme_protocol("gwsext", |ctx, requete| {
            use tauri::http::Response;
            let refus = || Response::builder().status(404).body(Vec::new()).unwrap();
            let Some(hote) = ctx.app_handle().try_state::<std::sync::Arc<extensions::ExtensionHost>>() else {
                return refus();
            };
            let chemin = requete.uri().path().trim_start_matches('/').to_string();
            let chemin = percent_decode(&chemin);
            let Some((id, fichier)) = chemin.split_once('/') else { return refus() };
            let trouve = match fichier.strip_prefix("_donnees/") {
                Some(relatif) => hote.data_file(id, relatif),
                None => hote.ui_file(id, fichier),
            };
            match trouve.and_then(|p| std::fs::read(&p).ok().map(|b| (p, b))) {
                Some((p, octets)) => Response::builder()
                    .header("Content-Type", extensions::mime(&p))
                    .header(
                        "Content-Security-Policy",
                        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:",
                    )
                    .body(octets)
                    .unwrap(),
                None => refus(),
            }
        })
        // Fichiers déposés depuis le système : retenus pour `read_dropped_file`
        // (zone de dépôt de l'assistant IA), seule lecture qu'elle autorise.
        .on_window_event(|_w, ev| {
            if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = ev {
                drop_files::remember(paths);
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::ign_altitude,
            commands::read_dropped_file,
            commands::detect_import_format,
            commands::create_demo_project,
            extensions::commands::list_extensions,
            extensions::commands::set_extension_enabled,
            extensions::commands::extension_call,
            extensions_catalogue::commands::extensions_catalogue,
            extensions_catalogue::commands::extension_install,
            extensions_catalogue::commands::extension_uninstall,
            // Projets
            commands::list_projects,
            commands::create_project,
            commands::open_project,
            // Points
            commands::category_options,
            commands::list_points,
            commands::create_point,
            commands::get_point,
            commands::update_point,
            commands::delete_point,
            // Séries et statistiques
            commands::list_series,
            commands::series_measurements_page,
            commands::series_gaps,
            commands::list_project_imports,
            commands::delete_series,
            commands::delete_measurements_range,
            commands::delete_measurements,
            commands::set_measurement_temperature,
            commands::project_measurement_count,
            commands::undo_status,
            commands::quota_partage,
            commands::appareil_identite,
            commands::appareil_changer,
            commands::undo_last,
            commands::redo_last,
            commands::series_stats,
            commands::recalibrate_series,
            commands::resolve_series_date_order,
            // Croisement de données (bloc bas-gauche)
            commands::cross_analysis,
            // Croisement en plein écran : bilan mois par mois
            commands::cross_monthly,
            // Import
            commands::analyze_elitech,
            commands::preview_delimited,
            commands::import_elitech,
            commands::import_manual,
            // Import assisté par IA (fichiers & captures d'écran)
            commands::ai_analyze_file,
            commands::ai_analyze_vision,
            commands::ai_preview_parsed,
            commands::import_ai,
            // Saisie manuelle sur site : une valeur tapée à la main, sans
            // fichier ni appareil. Même pipeline que les autres imports.
            commands::import_manual_entry,
            // Elitech, format réel : un classeur xlsx, transporté en octets.
            // La reconnaissance du format se fait côté frontend (4 octets de
            // signature) : envoyer 40 Mo au backend juste pour ça n'aurait rien
            // apporté.
            commands::analyze_elitech_xlsx,
            commands::import_elitech_xlsx,
            // Elitech RC-5, lecture directe par le port série. Complément de
            // l'import xlsx, jamais son remplaçant : si la lecture directe échoue
            // sur un cas particulier, le fichier reste la voie sûre.
            commands::list_rc5_ports,
            commands::rescan_rc5,
            commands::identify_rc5,
            commands::read_rc5,
            commands::import_rc5,
            // Page RC-5 dédiée : vérification de l'horloge et effacement de la
            // mémoire du boîtier. `reset_rc5` est la seule commande destructrice
            // sur le matériel — elle exige une confirmation explicite, et
            // n'efface jamais les données déjà en base.
            commands::check_rc5_clock,
            commands::reset_rc5,
            // Arrêt de l'enregistrement, sans effacement — trouvé par sondage
            // sur le matériel réel (voir `elitech_serial::CMD_STOP`). Distinct
            // de `reset_rc5` : aucune confirmation n'est exigée, puisque rien
            // n'est perdu.
            commands::stop_rc5,
            // Export de la lecture en cours, sans passer par la base : sur le
            // terrain, on veut le fichier avant de décider s'il devient une
            // série.
            commands::build_rc5_export,
            // Infoclimat (VP2 du réseau StatIC + Moucherolle STATIC0479)
            commands::infoclimat_settings,
            commands::set_infoclimat_refresh_minutes,
            commands::set_infoclimat_key,
            commands::get_infoclimat_quota,
            commands::analyze_infoclimat,
            commands::import_infoclimat,
            commands::fetch_infoclimat,
            // Relai SSH Infoclimat
            commands::get_ssh_relay_status,
            commands::save_ssh_relay_config,
            commands::set_ssh_relay_enabled,
            commands::test_ssh_relay_connection,
            commands::pick_ssh_key_file,
            // Empreinte de la clé d'hôte : acceptée explicitement par l'utilisateur
            // après vérification, jamais en silence (audit du 18/09, §1).
            commands::trust_ssh_host_key,
            commands::forget_ssh_host_key,
            // Configuration signée du relai : statut en lecture seule, sans
            // secret (voir `relais_config.rs`). Aucune commande d'écriture.
            commands::get_relais_config_status,
            // ROMMA (archives_*.csv)
            commands::analyze_romma,
            commands::import_romma,
            // Identité de l'utilisateur (écran d'accueil + ton de l'agent)
            commands::get_user_name,
            commands::set_user_name,
            // Données applicatives
            commands::clear_all_data,
            // Cache de tuiles hors-ligne (Photo aérienne + TOP25/SCAN25)
            commands::get_tile,
            commands::download_tile_area,
            commands::tiles_cache_info,
            commands::clear_tiles_cache,
            // Agent IA (Groq)
            commands::agent_settings,
            commands::set_groq_key,
            commands::agent_analyze,
            // Liens vers l'extérieur (liste d'URL fermée, cf. `commands.rs`)
            commands::open_external_url,
            // Export (page 2 du bloc Stations)
            commands::build_export,
            commands::save_export_file,
            // Partage de points par code (serveur gws-share). Le jeton de
            // révocation ne remonte jamais au frontend.
            commands::share_create,
            commands::share_preview,
            commands::share_import,
            commands::share_list_issued,
            commands::share_revoke,
            // Avertissement de mise à jour, désactivable (Paramètres).
            commands::update_settings,
            commands::set_update_mode,
            commands::set_update_channel,
            commands::skip_update_version,
            commands::check_for_update,
            commands::open_update_download,
            // Paquets de mise à jour .gws
            commands::inspect_gws_package,
            commands::apply_gws_package,
            commands::get_startup_package,
            commands::confirmer_demarrage_maj,
            commands::consommer_apres_maj,
            commands::delete_project,
            // Projet Cloud (synchronisation chiffrée de bout en bout)
            commands::cloud_status,
            commands::cloud_register,
            commands::cloud_login,
            commands::cloud_logout,
            commands::cloud_sync,
            commands::cloud_set_auto_sync,
            commands::cloud_project_prefs,
            commands::cloud_set_project_excluded,
        ])
        .run(tauri::generate_context!())
        .expect("échec au lancement de l'application Tauri");
}

/// Décodage `%XX` minimal pour les chemins servis aux panneaux d'extension.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

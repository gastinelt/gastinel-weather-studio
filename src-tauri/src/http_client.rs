//! Fabrique commune des clients HTTP bloquants du projet.
//!
//! Les trois appels réseau de l'application — l'API Infoclimat, l'agent Groq et
//! les tuiles `data.geopf.fr` — passaient chacun par leur propre
//! `reqwest::blocking::Client::builder()`, avec le même `user_agent` recopié.
//! Ce module les réunit pour une raison qui n'est pas cosmétique : **la
//! configuration TLS d'Android ne doit exister qu'à un seul endroit**. Un
//! quatrième appel qui oublierait de la reprendre échouerait sur mobile, et
//! seulement sur mobile.
//!
//! ## Le problème Android
//!
//! `reqwest` 0.13 vérifie les certificats avec `rustls-platform-verifier`, qui
//! sur Android **ne fonctionne pas seul** : il lui faut un pont JNI vers le
//! `TrustManager` de la plateforme, monté côté Java au démarrage de
//! l'application. Sans ce pont, toute requête HTTPS échoue à la poignée de main.
//! C'est ce qui rendait Infoclimat, Groq et les tuiles inaccessibles sur mobile.
//!
//! ## Le choix fait : des racines statiques embarquées
//!
//! Plutôt que de monter ce pont JNI (Gradle, ProGuard, initialisation Java), on
//! embarque le magasin de racines de Mozilla dans le binaire, via
//! [`webpki_roots`], et on court-circuite le vérificateur de plateforme avec une
//! configuration `rustls` fournie clé en main à `reqwest`.
//!
//! **Le compromis est réel et assumé** : les racines de confiance ne suivent
//! plus les mises à jour du système Android, elles suivent celles de
//! l'application. Une autorité révoquée le reste jusqu'au prochain build. Il est
//! acceptable ici parce que l'application est familiale, fermée, et n'appelle que
//! trois domaines fixes (`www.infoclimat.fr`, `api.groq.com`, `data.geopf.fr`) —
//! pas un navigateur qui va n'importe où. Voir le README, § « Appels HTTP sur
//! Android ».
//!
//! ## Ce qui ne change pas
//!
//! **Rien sur desktop.** Windows continue d'utiliser SChannel via `native-tls`,
//! Linux et macOS le magasin système via le vérificateur de plateforme. La
//! configuration ci-dessous est strictement sous `cfg(target_os = "android")` :
//! le chemin qui fonctionne déjà n'est pas touché.

/// Identification de l'application auprès des trois services appelés.
///
/// Une seule chaîne pour les trois clients : Infoclimat et Groq la lisent dans
/// leurs journaux, et il vaut mieux qu'ils y voient la même application.
pub const USER_AGENT: &str = "GastinelWeatherStudio/0.1 (usage personnel, non commercial)";

/// Le constructeur de client que tout le projet doit utiliser.
///
/// L'appelant garde la main sur ce qui lui est propre — au premier chef le
/// `timeout`, qui va de 2 s pour une tuile à 60 s pour un rattrapage Infoclimat.
/// Ce qui est commun (identification, et TLS sur Android) est déjà posé.
pub fn client_builder() -> reqwest::blocking::ClientBuilder {
    let builder = reqwest::blocking::Client::builder().user_agent(USER_AGENT);

    #[cfg(target_os = "android")]
    let builder = builder.use_preconfigured_tls(android_tls_config());

    builder
}

/// Configuration `rustls` adossée aux racines embarquées (Android uniquement).
///
/// Elle remplace intégralement le vérificateur de plateforme : `reqwest` reçoit
/// une `ClientConfig` toute faite et ne construit plus la sienne.
#[cfg(target_os = "android")]
fn android_tls_config() -> rustls::ClientConfig {
    use std::sync::Arc;

    // `webpki_roots` livre des ancres de confiance (sujet + clé publique), pas
    // des certificats DER complets : c'est exactement ce qu'attend
    // `RootCertStore`, et c'est aussi pourquoi `add_root_certificate()` de
    // `reqwest`, qui veut du DER, ne convenait pas ici.
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    // Le fournisseur cryptographique doit être **celui de `reqwest`**, sinon la
    // `ClientConfig` qu'on lui passe n'est pas cohérente avec le reste de sa
    // pile. On reprend donc sa propre logique : le défaut installé dans le
    // processus s'il y en a un, `aws-lc-rs` sinon — le fournisseur que la
    // fonctionnalité `rustls` de `reqwest` active.
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));

    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        // Les versions par défaut de `rustls` sont toujours cohérentes entre
        // elles : l'erreur ne peut pas se produire ici.
        .expect("versions TLS par défaut de rustls")
        .with_root_certificates(roots)
        .with_no_client_auth();

    // `reqwest` pose lui-même l'ALPN quand il construit sa configuration ; avec
    // une configuration fournie, c'est à nous de le faire. Le client est compilé
    // sans la fonctionnalité `http2` (voir `Cargo.toml`), donc HTTP/1.1 seul.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];

    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_user_agent_identifie_l_application_et_son_usage() {
        assert!(USER_AGENT.starts_with("GastinelWeatherStudio/"));
        assert!(USER_AGENT.contains("non commercial"));
    }

    /// Le point du module : que le client se construise. Sur Android, cela
    /// exerce toute la fabrication de la `ClientConfig` — magasin de racines,
    /// fournisseur cryptographique, versions de protocole — sans qu'aucun octet
    /// ne parte sur le réseau.
    #[test]
    fn le_client_commun_se_construit() {
        let client = client_builder()
            .timeout(std::time::Duration::from_secs(5))
            .build();
        assert!(client.is_ok(), "client HTTP commun inconstructible");
    }

    /// Sur Android, le magasin embarqué doit être **non vide** : une
    /// `ClientConfig` sans aucune racine se construit très bien et refuse
    /// ensuite tous les certificats du monde. C'est le mode de panne qu'on ne
    /// verrait qu'à l'exécution, sur l'appareil.
    #[cfg(target_os = "android")]
    #[test]
    fn les_racines_embarquees_ne_sont_pas_vides() {
        assert!(
            webpki_roots::TLS_SERVER_ROOTS.len() > 50,
            "magasin de racines embarqué suspect : {} ancres",
            webpki_roots::TLS_SERVER_ROOTS.len()
        );
    }

    /// L'ALPN annoncé doit correspondre aux fonctionnalités compilées : le
    /// client n'a pas `http2`, annoncer `h2` ferait négocier un protocole que la
    /// pile ne sait pas parler.
    #[cfg(target_os = "android")]
    #[test]
    fn l_alpn_annonce_est_celui_que_le_client_sait_parler() {
        let config = android_tls_config();
        assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }
}

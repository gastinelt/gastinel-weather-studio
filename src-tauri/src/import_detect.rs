//! Onglet d'import deviné pour un fichier déposé sur la fenêtre.
//!
//! Aucune règle propre ici : chaque format est reconnu par la fonction du
//! parseur qui le lit (`looks_like_xlsx`, en-têtes ROMMA, `dh_utc` Infoclimat,
//! intitulés ElitechLog). Le contenu prime sur l'extension, comme partout dans
//! l'import ; l'extension ne sert qu'à reconnaître une image.

use serde::Serialize;

/// Verdict renvoyé au frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormatDetecte {
    /// Onglet de l'import à ouvrir : `auto`, `infoclimat`, `romma`, `ai` ou
    /// `manual` (repli quand rien n'est reconnu).
    pub onglet: &'static str,
    /// Nom du format, pour le message affiché.
    pub libelle: &'static str,
    /// `false` : aucun format connu, l'onglet est un repli.
    pub reconnu: bool,
}

const fn verdict(onglet: &'static str, libelle: &'static str, reconnu: bool) -> FormatDetecte {
    FormatDetecte { onglet, libelle, reconnu }
}

fn extension(nom: &str) -> String {
    nom.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// Devine l'onglet. `est_zip` : les octets commencent par la signature zip
/// (classeur OOXML) ; `texte` : le début du fichier décodé (vide pour un zip).
pub fn detecter(nom_fichier: &str, est_zip: bool, texte: &str) -> FormatDetecte {
    if matches!(extension(nom_fichier).as_str(), "png" | "jpg" | "jpeg" | "webp") {
        return verdict("ai", "image (lecture par l'assistant IA)", true);
    }
    if est_zip {
        return verdict("auto", "classeur ElitechLog (xlsx)", true);
    }
    if texte.trim().is_empty() {
        return verdict("manual", "fichier vide ou illisible", false);
    }
    // Infoclimat et ROMMA avant Elitech : leurs en-têtes portent aussi une
    // colonne de temps et une de température, que la détection Elitech
    // accepterait.
    if crate::infoclimat::ressemble_a_un_export(texte) {
        return verdict("infoclimat", "export Infoclimat", true);
    }
    if crate::romma_import::ressemble_a_une_archive(texte) {
        return verdict("romma", "archive ROMMA", true);
    }
    if crate::elitech_import::a_des_intitules_reconnus(texte) {
        return verdict("auto", "export ElitechLog (texte)", true);
    }
    verdict("manual", "format non reconnu", false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classeur_zip_va_vers_elitech() {
        assert_eq!(detecter("export.xls", true, "").onglet, "auto");
    }

    #[test]
    fn image_va_vers_l_assistant() {
        let v = detecter("releve.JPG", false, "");
        assert_eq!(v.onglet, "ai");
        assert!(v.reconnu);
    }

    #[test]
    fn archive_romma_reconnue() {
        let t = "Date,heure,timestamp,Temp,Tmin,Tmax,Hum\n,,,degreC,degreC,degreC,%\n31-05-2017,19:00,1496250000,12.2,12.2,12.2,72\n";
        assert_eq!(detecter("archives_x.csv", false, t).onglet, "romma");
    }

    #[test]
    fn infoclimat_csv_et_json_reconnus() {
        let csv = "# station: X\nstation_id;dh_utc;temperature\n;;degC\nSTATIC0479;2024-01-05 10:00:00;-3.2\n";
        assert_eq!(detecter("export.txt", false, csv).onglet, "infoclimat");
        let json = r#"{"hourly":{"STATIC0479":[{"dh_utc":"2024-01-05 10:00:00"}]}}"#;
        assert_eq!(detecter("data.json", false, json).onglet, "infoclimat");
    }

    #[test]
    fn export_elitech_texte_reconnu() {
        let t = "No.\tTime\tTemperature(°C)\tHumidity(%RH)\n1\t2024-01-05 10:00:00\t-3.2\t80\n";
        let v = detecter("RC5.txt", false, t);
        assert_eq!(v.onglet, "auto");
        assert!(v.reconnu);
    }

    #[test]
    fn inconnu_propose_le_manuel() {
        let v = detecter("notes.csv", false, "a;b;c\n1;2;3\n");
        assert_eq!(v.onglet, "manual");
        assert!(!v.reconnu);
        assert!(!detecter("vide.txt", false, "  \n").reconnu);
    }

    const ECHANTILLONS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/samples/");

    fn echantillon(nom: &str) -> String {
        String::from_utf8_lossy(&std::fs::read(format!("{ECHANTILLONS}{nom}")).unwrap()).into_owned()
    }

    #[test]
    fn csv_generique_n_est_pas_elitech() {
        let v = detecter("generique_manuel.csv", false, &echantillon("generique_manuel.csv"));
        assert_eq!(v.onglet, "manual");
        assert!(!v.reconnu);
    }

    #[test]
    fn echantillons_reels_reconnus() {
        let t = echantillon("elitech_hypothese.txt");
        assert_eq!(detecter("elitech_hypothese.txt", false, &t).onglet, "auto");
        let t = echantillon("infoclimat_export_extrait.csv");
        assert_eq!(detecter("x.csv", false, &t).onglet, "infoclimat");
        let t = echantillon("romma_archives_extrait.csv");
        assert_eq!(detecter("x.csv", false, &t).onglet, "romma");
        let xls = std::fs::read(format!("{ECHANTILLONS}elitech_rc5_reel.xls")).unwrap();
        assert!(xls.starts_with(b"PK"));
        assert_eq!(detecter("elitech_rc5_reel.xls", true, "").onglet, "auto");
    }

    #[test]
    fn cas_limite_unite_entre_parentheses_en_francais() {
        // Unité entre parenthèses mais intitulés français, sans préambule
        // Elitech : reste un tableur générique.
        let t = "Date;Heure;Température (°C);Humidité (%)
05/01/2024;06:00:00;-8,2;86
";
        assert_eq!(detecter("x.csv", false, t).onglet, "manual");
        // Intitulés génériques mais préambule Elitech : reconnu.
        let t = "Elitech Log Data
Serial Number: RC5000123

Time	Temperature
2024-01-05 06:00:00	-8.2
";
        assert_eq!(detecter("x.txt", false, t).onglet, "auto");
    }

    #[test]
    fn le_contenu_prime_sur_l_extension() {
        let t = "Date,heure,timestamp,Temp\n31-05-2017,19:00,1496250000,12.2\n";
        assert_eq!(detecter("renomme.json", false, t).onglet, "romma");
    }
}

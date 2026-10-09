// Aucune commande n'est exposée au frontend : le greffon n'est appelé que
// depuis Rust (`elitech_serial.rs`). La liste vide est donc volontaire — elle
// évite de créer une surface d'API JS que personne n'utiliserait, et avec elle
// tout un jeu de permissions à maintenir.
const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();
}

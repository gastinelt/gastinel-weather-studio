// Masque la console Windows en build release (elle reste ouverte en debug,
// c'est là qu'on lit le chemin de la base et les éventuelles erreurs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    gws_lib::run()
}

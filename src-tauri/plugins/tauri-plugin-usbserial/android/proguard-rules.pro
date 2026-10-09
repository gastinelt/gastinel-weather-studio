# Le greffon est atteint par réflexion depuis le pont Tauri (nom de classe et
# noms de commandes passés en chaînes) : R8 le supprimerait en compilation de
# release, et l'erreur ne se verrait qu'à l'exécution sur l'APK signé.
-keep class fr.gastinel.usbserial.** { *; }

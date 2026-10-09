plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "fr.gastinel.usbserial"
    compileSdk = 36

    defaultConfig {
        minSdk = 24
        consumerProguardFiles("consumer-rules.pro")
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
}

// `usb-serial-for-android` n'est publié que sur JitPack (le seul artefact du
// même nom sur Maven Central est un portage tiers abandonné en 2019, en
// version 1.0.0). Il faut donc déclarer ce dépôt — et le déclarer **pour tous
// les projets**, pas seulement pour celui-ci.
//
// La raison est une règle de Gradle qui surprend : les dépôts ne se propagent
// pas d'un projet à l'autre. C'est `:app` qui dépend de
// `project(":tauri-plugin-usbserial")`, et c'est donc `:app` qui résout nos
// dépendances externes transitives — avec **ses** dépôts (`google()`,
// `mavenCentral()`, déclarés dans le `allprojects` de
// `gen/android/build.gradle.kts`), pas avec les nôtres. Sans cette ligne, ce
// module compile et l'application échoue à l'assemblage.
//
// Pourquoi ici, et pas dans `gen/android/build.gradle.kts` où ce serait plus
// naturel : ce fichier-là est **régénéré** par `tauri android init` et ignoré
// par git en entier (cf. `.gitignore`). La déclaration y serait perdue au
// premier clone — c'est exactement la raison d'être de ce greffon, cf. le
// README § 4quater.
rootProject.allprojects {
    repositories {
        maven { setUrl("https://jitpack.io") }
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.1")
    // Le projet `:tauri-android` est injecté dans `settings.gradle` par le CLI
    // Tauri : il apporte `Plugin`, `Invoke`, `JSObject` et les annotations.
    implementation(project(":tauri-android"))

    // Le pilote de la puce CH340 — voir `PortSerie.kt` pour la raison du
    // remplacement du pilote maison du prompt 13.
    //
    // Version épinglée à 3.9.0 et **pas** à la dernière (3.11.0) : à partir de
    // 3.10, la publication JitPack traîne un `kotlin-stdlib` 2.2.x en
    // dépendance. La résolution de Gradle le préférerait au 1.9.25 du greffon
    // Kotlin de ce projet, et le compilateur 1.9 refuse les métadonnées d'un
    // stdlib 2.2 (« Module was compiled with an incompatible version of
    // Kotlin »). 3.9.0 ne dépend que d'`androidx.annotation` ; son
    // `Ch34xSerialDriver` est celui de 3.11.0 à l'octet près.
    //
    // `implementation` et non `api` : aucun type de la bibliothèque ne traverse
    // la frontière de ce module — `PortSerie` les garde tous privés.
    implementation("com.github.mik3y:usb-serial-for-android:3.9.0")
}

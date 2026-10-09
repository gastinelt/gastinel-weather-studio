<#
.SYNOPSIS
    Génère un paquet de mise à jour .gws à partir du binaire release de Gastinel Weather Studio.

.DESCRIPTION
    Ce script prend l'exécutable compilé (target/release/gastinel-weather-studio.exe),
    calcule son empreinte SHA-256, génère le manifest.json et assemble l'archive .gws.

.PARAMETER Notes
    Les notes de version / changelog (texte ou markdown). Optionnel.

.PARAMETER Cle
    Chemin de la clé privée de signature des mises à jour (hors dépôt).
    Défaut : variable d'environnement GWS_CLE_MAJ. Voir docs/signature-maj.md.

.PARAMETER KeyId
    Identifiant de la clé (entrée de CLES_MAJ dans src-tauri/src/package.rs).
    Défaut : variable d'environnement GWS_CLE_MAJ_ID.

.EXAMPLE
    .\scripts\create_gws_package.ps1 -Notes "Corrections de bugs et améliorations de performances."

.EXAMPLE
    .\scripts\create_gws_package.ps1 -Notes "..." -Cle D:\cles\gws_maj.pem -KeyId maj-2026-a
#>

param(
    [string]$Notes = "Mise à jour de Gastinel Weather Studio",
    [string]$Cle = $env:GWS_CLE_MAJ,
    [string]$KeyId = $env:GWS_CLE_MAJ_ID
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $ScriptDir

# 1. Vérification de l'exécutable
$ExePath = Join-Path $ProjectRoot "src-tauri\target\release\gastinel-weather-studio.exe"
if (-not (Test-Path $ExePath)) {
    Write-Error "Exécutable introuvable : $ExePath`nVeuillez d'abord compiler le projet avec : cargo build --release --manifest-path src-tauri/Cargo.toml"
}

# 2. Lecture de la version depuis Cargo.toml
$CargoToml = Get-Content (Join-Path $ProjectRoot "src-tauri\Cargo.toml") -Raw
if ($CargoToml -match 'version\s*=\s*"([^"]+)"') {
    $Version = $Matches[1]
} else {
    Write-Error "Impossible de lire la version dans src-tauri/Cargo.toml"
}

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "  Création du paquet .gws pour la version $Version" -ForegroundColor Cyan
Write-Host "==========================================================" -ForegroundColor Cyan

# 3. Calcul du condensat SHA-256
Write-Host "Calcul du SHA-256 de $ExePath..."
$Hash = (Get-FileHash -Path $ExePath -Algorithm SHA256).Hash.ToLower()
Write-Host "SHA-256 : $Hash" -ForegroundColor Green

# 4. Préparation du dossier temporaire
$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("gws_pkg_" + [System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $TempDir | Out-Null

try {
    # 5. Création du manifest.json
    $Manifest = [ordered]@{
        "type"             = "gws-update"
        "version"          = $Version
        "title"            = "Gastinel Weather Studio v$Version"
        "release_date"     = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
        "checksum_sha256"  = $Hash
        "changelog"        = $Notes
    }

    $ManifestJson = $Manifest | ConvertTo-Json -Depth 4
    $ManifestPath = Join-Path $TempDir "manifest.json"
    # UTF-8 SANS BOM : [Text.Encoding]::UTF8 en ajoute un, que serde_json refuse.
    [System.IO.File]::WriteAllText($ManifestPath, $ManifestJson, (New-Object System.Text.UTF8Encoding $false))

    # Copie de l'exécutable dans le dossier temporaire
    Copy-Item $ExePath (Join-Path $TempDir "gastinel-weather-studio.exe")

    # 6. Création de l'archive .gws (format ZIP standard)
    $OutputFile = Join-Path $ProjectRoot "update-$Version.gws"
    if (Test-Path $OutputFile) {
        Remove-Item $OutputFile -Force
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory($TempDir, $OutputFile, [System.IO.Compression.CompressionLevel]::Optimal, $false)

    # 7. Signature Ed25519 (obligatoire pour les applications 0.5.0 et suivantes)
    if ($Cle -and $KeyId) {
        python (Join-Path $ScriptDir "signer_maj.py") signer $OutputFile --cle $Cle --key-id $KeyId
        if ($LASTEXITCODE -ne 0) {
            Remove-Item $OutputFile -Force
            Write-Error "Signature échouée : paquet supprimé."
        }
    } else {
        Write-Warning "Paquet NON SIGNÉ (ni -Cle/-KeyId ni GWS_CLE_MAJ/GWS_CLE_MAJ_ID) : les versions 0.5.0 et suivantes le refuseront. Ne pas le publier."
    }

    Write-Host "`nSuccès ! Paquet créé :" -ForegroundColor Green
    Write-Host "  Fichier : $OutputFile" -ForegroundColor Yellow
    $Size = (Get-Item $OutputFile).Length / 1MB
    Write-Host "  Taille  : $([math]::Round($Size, 2)) Mo" -ForegroundColor Yellow
    Write-Host "==========================================================" -ForegroundColor Cyan
} finally {
    if (Test-Path $TempDir) {
        Remove-Item $TempDir -Recurse -Force
    }
}

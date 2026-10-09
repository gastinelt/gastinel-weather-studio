@echo off
title Compilation Gastinel Weather Studio
cd /d "%~dp0"
echo ========================================================
echo   Compilation de Gastinel Weather Studio
echo ========================================================
echo.
cargo tauri build
if %errorlevel% equ 0 (
    echo.
    echo ========================================================
    echo   Compilation reussie ! Copie de l'installateur...
    echo ========================================================
    for /r "src-tauri\target\release\bundle\nsis" %%f in (*.exe) do (
        copy /y "%%f" "%USERPROFILE%\Desktop\"
        echo   Copie : %%~nxf vers le Bureau
    )
    echo.
    echo Termine ! Le nouvel installateur est pret sur votre Bureau.
    echo.
    pause
) else (
    echo.
    echo Erreur lors de la compilation.
    echo.
    pause
)

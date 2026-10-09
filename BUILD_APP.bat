@echo off
cd /d "%~dp0"
echo Building Gastinel Weather Studio...
cargo tauri build
if %errorlevel% equ 0 (
    echo.
    echo Build reussi!
    echo L'app a ete compilee: src-tauri\target\release\gastinel-weather-studio.exe
    echo.
    pause
) else (
    echo Build echoue.
    echo.
    pause
)

@echo off
setlocal
cd /d "%~dp0"
title GoRide - fabrication de l'executable

echo.
echo  === GoRide : fabrication de l'installateur Windows ===
echo.

where npm >nul 2>nul || (echo [ERREUR] Node.js est introuvable. Installe-le depuis https://nodejs.org & goto :fail)
where cargo >nul 2>nul || (echo [ERREUR] Rust est introuvable. Installe-le depuis https://rustup.rs & goto :fail)

if not exist node_modules (
    echo [1/3] Installation des dependances JavaScript...
    call npm install || goto :fail
) else (
    echo [1/3] Dependances JavaScript deja installees.
)

echo [2/3] Compilation en mode release (plusieurs minutes la premiere fois)...
call npm run tauri build || goto :fail

echo [3/3] Copie des fichiers dans le dossier "release"...
if not exist release mkdir release
copy /y "target\release\goride.exe" "release\GoRide.exe" >nul || goto :fail
copy /y "target\release\bundle\nsis\*-setup.exe" "release\" >nul

echo.
echo  Termine !
echo   - release\GoRide.exe            : l'application, a lancer directement
for %%f in (release\*-setup.exe) do echo   - %%f : l'installateur a partager
echo.
explorer release
pause
exit /b 0

:fail
echo.
echo  [ECHEC] La fabrication s'est arretee. Lis le message d'erreur ci-dessus.
pause
exit /b 1

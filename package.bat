@echo off
setlocal EnableExtensions EnableDelayedExpansion
cd /d "%~dp0"

for /f "tokens=2 delims== " %%V in ('findstr /b /c:"version = " Cargo.toml') do if not defined VERSION set "VERSION=%%~V"
if not defined VERSION set "VERSION=0.0.0"

echo.
echo ============================================
echo  ScreenRec - release packaging
echo ============================================
echo.

call build.bat
if errorlevel 1 (
    echo [ScreenRec] Build failed.
    exit /b 1
)

if not exist "target\release\screen_rec.exe" (
    echo [ScreenRec] target\release\screen_rec.exe was not produced.
    exit /b 1
)

if exist "dist" rmdir /s /q "dist"
mkdir "dist\ScreenRec"

copy /y "target\release\screen_rec.exe" "dist\ScreenRec\ScreenRec.exe" >nul
copy /y "resources\ScreenRec.ico" "dist\ScreenRec\ScreenRec.ico" >nul

echo [ScreenRec] Portable files assembled in dist\ScreenRec

powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "Compress-Archive -Path 'dist\ScreenRec\*' -DestinationPath 'dist\ScreenRec-Portable-!VERSION!.zip' -Force" >nul 2>nul

set "ISCC="
for %%P in (
    "%ProgramFiles(x86)%\Inno Setup 6\ISCC.exe"
    "%ProgramFiles%\Inno Setup 6\ISCC.exe"
    "%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe"
    "%USERPROFILE%\Tools\InnoSetup\ISCC.exe"
) do (
    if not defined ISCC if exist "%%~P" set "ISCC=%%~P"
)
if not defined ISCC (
    for /f "delims=" %%I in ('where ISCC.exe 2^>nul') do set "ISCC=%%I"
)

if defined ISCC (
    echo [ScreenRec] Compiling installer with "!ISCC!" ...
    "!ISCC!" /DMyAppVersion=!VERSION! "installer\screen_rec.iss"
    if errorlevel 1 (
        echo [ScreenRec] Installer compilation failed.
        exit /b 1
    )
    if exist "dist\ScreenRec-Setup-!VERSION!.exe" (
        copy /y "dist\ScreenRec-Setup-!VERSION!.exe" "ScreenRec-Setup.exe" >nul
    )
) else (
    echo [ScreenRec] Inno Setup was not found - installer was not compiled.
    echo             Install it from https://jrsoftware.org/isinfo.php then run package.bat again.
)

echo.
echo [ScreenRec] Done.
echo   Portable app : dist\ScreenRec\ScreenRec.exe
echo   Portable zip : dist\ScreenRec-Portable-!VERSION!.zip
if exist "dist\ScreenRec-Setup-!VERSION!.exe" echo   Installer    : dist\ScreenRec-Setup-!VERSION!.exe
echo.

exit /b 0

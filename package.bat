@echo off
setlocal EnableExtensions EnableDelayedExpansion
cd /d "%~dp0"

for /f "tokens=2 delims== " %%V in ('findstr /b /c:"version = " Cargo.toml') do if not defined VERSION set "VERSION=%%~V"
if not defined VERSION set "VERSION=0.0.0"

echo.
echo ============================================
echo  Scroder - release packaging
echo ============================================
echo.

call build.bat
if errorlevel 1 (
    echo [Scroder] Build failed.
    exit /b 1
)

if not exist "target\release\scroder.exe" (
    echo [Scroder] target\release\scroder.exe was not produced.
    exit /b 1
)

if exist "dist" rmdir /s /q "dist"
mkdir "dist\Scroder"

copy /y "target\release\scroder.exe" "dist\Scroder\Scroder.exe" >nul
copy /y "resources\Scroder.ico" "dist\Scroder\Scroder.ico" >nul

echo [Scroder] Portable files assembled in dist\Scroder

powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "Compress-Archive -Path 'dist\Scroder\*' -DestinationPath 'dist\Scroder-Portable-!VERSION!.zip' -Force" >nul 2>nul

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
    echo [Scroder] Compiling installer with "!ISCC!" ...
    "!ISCC!" /DMyAppVersion=!VERSION! "installer\scroder.iss"
    if errorlevel 1 (
        echo [Scroder] Installer compilation failed.
        exit /b 1
    )
    if exist "dist\Scroder-Setup-!VERSION!.exe" (
        copy /y "dist\Scroder-Setup-!VERSION!.exe" "Scroder-Setup.exe" >nul
    )
) else (
    echo [Scroder] Inno Setup was not found - installer was not compiled.
    echo             Install it from https://jrsoftware.org/isinfo.php then run package.bat again.
)

echo.
echo [Scroder] Done.
echo   Portable app : dist\Scroder\Scroder.exe
echo   Portable zip : dist\Scroder-Portable-!VERSION!.zip
if exist "dist\Scroder-Setup-!VERSION!.exe" echo   Installer    : dist\Scroder-Setup-!VERSION!.exe
echo.

exit /b 0

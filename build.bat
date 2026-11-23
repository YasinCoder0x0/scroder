@echo off
setlocal EnableExtensions EnableDelayedExpansion
cd /d "%~dp0"

where cargo >nul 2>nul
if errorlevel 1 (
    echo [ScreenRec] cargo was not found in PATH.
    echo Install Rust from https://rustup.rs and try again.
    exit /b 1
)

rem Set up the MSVC toolchain (link.exe / rc.exe) unless it is already active.
if not defined VSCMD_ARG_TGT_ARCH (
    set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
    if exist "!VSWHERE!" (
        for /f "usebackq delims=" %%I in (`"!VSWHERE!" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2^>nul`) do set "VSDIR=%%I"
    )

    if defined VSDIR if exist "!VSDIR!\VC\Auxiliary\Build\vcvars64.bat" (
        call "!VSDIR!\VC\Auxiliary\Build\vcvars64.bat" >nul
    )
)

if not defined VSCMD_ARG_TGT_ARCH (
    rem Fallback for installations that vswhere cannot see.
    for %%P in (
        "%ProgramFiles(x86)%\Microsoft Visual Studio\18\BuildTools"
        "%ProgramFiles%\Microsoft Visual Studio\18\BuildTools"
        "%ProgramFiles%\Microsoft Visual Studio\2022\Community"
        "%ProgramFiles%\Microsoft Visual Studio\2022\Professional"
        "%ProgramFiles%\Microsoft Visual Studio\2022\Enterprise"
        "%ProgramFiles%\Microsoft Visual Studio\2022\BuildTools"
    ) do (
        if not defined VSCMD_ARG_TGT_ARCH if exist "%%~P\VC\Auxiliary\Build\vcvars64.bat" (
            call "%%~P\VC\Auxiliary\Build\vcvars64.bat" >nul
        )
    )
)

if not defined VSCMD_ARG_TGT_ARCH (
    echo [ScreenRec] Could not initialize the Visual Studio C++ build tools.
    echo Install "Desktop development with C++" from the Visual Studio Installer.
    exit /b 1
)

if "%~1"=="" (
    echo [ScreenRec] Building optimized release binary...
    cargo build --release
) else (
    cargo %*
)

exit /b %ERRORLEVEL%

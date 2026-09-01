@echo off
REM Hebnix Windows Build Script
REM This script builds the entire Hebnix project and installs dependencies

setlocal enabledelayedexpansion

set SCRIPT_DIR=%~dp0
set HEBNIX_RS_DIR=%SCRIPT_DIR%hebnix_rs
set RLAPI_BRIDGE_DIR=%SCRIPT_DIR%rlapi_bridge
set VENDOR_DIR=%HEBNIX_RS_DIR%\vendor

REM Parse command line arguments
set TARGET=all
set SKIP_DEPS=0
set CLEAN=0

:parse_args
if "%~1"=="" goto end_parse_args
if /I "%~1"=="--help" goto show_help
if /I "%~1"=="-h" goto show_help
if /I "%~1"=="clean" set CLEAN=1 & shift & goto parse_args
if /I "%~1"=="bridge" set TARGET=bridge & shift & goto parse_args
if /I "%~1"=="build" set TARGET=build & shift & goto parse_args
if /I "%~1"=="package" set TARGET=package & shift & goto parse_args
if /I "%~1"=="--skip-deps" set SKIP_DEPS=1 & shift & goto parse_args
echo Unknown argument: %~1
goto show_help
:end_parse_args

if %CLEAN%==1 goto do_clean

REM Display banner
echo =======================================
echo   Hebnix Windows Build Script
echo =======================================
echo.

REM Detect if running in CI
set CI_BUILD=0
if defined GITHUB_ACTIONS set CI_BUILD=1
if defined CI set CI_BUILD=1

REM Check for required tools
echo [1/5] Checking build tools...
call :check_tool cargo "Rust" "https://rustup.rs"
if errorlevel 1 exit /b 1

if not "%TARGET%"=="build" (
    call :check_tool go "Go" "https://go.dev/dl/"
    if errorlevel 1 exit /b 1
)

call :check_tool powershell "PowerShell" ""
if errorlevel 1 exit /b 1

REM Install third-party dependencies
if %SKIP_DEPS%==0 (
    echo.
    echo [2/5] Installing third-party dependencies...
    call :install_deps
    if errorlevel 1 exit /b 1
) else (
    echo.
    echo [2/5] Skipping dependency installation (--skip-deps)
)

REM Build based on target
if "%TARGET%"=="bridge" goto build_bridge_only
if "%TARGET%"=="build" goto build_rust_only
if "%TARGET%"=="package" goto build_package

REM Default: build everything
echo.
echo [3/5] Building rlapi-bridge...
call :build_bridge
if errorlevel 1 exit /b 1

echo.
echo [4/5] Building Hebnix Rust components...
call :build_rust
if errorlevel 1 exit /b 1

echo.
echo [5/5] Build complete!
echo.
echo   Main executable: %HEBNIX_RS_DIR%\target\release\hebnix-app.exe
echo   Lite executable: %HEBNIX_RS_DIR%\target\release\hebnix-lite.exe
echo   Bridge executable: %RLAPI_BRIDGE_DIR%\dist\rlapi-bridge.exe
echo.
echo To create a distribution package, run:
echo   build.bat package
goto end

:build_bridge_only
echo.
echo [3/5] Building rlapi-bridge...
call :build_bridge
if errorlevel 1 exit /b 1
echo.
echo [4/5] Skipping Rust build
echo [5/5] Build complete!
echo   Bridge executable: %RLAPI_BRIDGE_DIR%\dist\rlapi-bridge.exe
goto end

:build_rust_only
echo.
echo [3/5] Skipping bridge build
echo [4/5] Building Hebnix Rust components...
call :build_rust
if errorlevel 1 exit /b 1
echo.
echo [5/5] Build complete!
echo   Main executable: %HEBNIX_RS_DIR%\target\release\hebnix-app.exe
echo   Lite executable: %HEBNIX_RS_DIR%\target\release\hebnix-lite.exe
goto end

:build_package
echo.
echo [3/5] Building rlapi-bridge...
call :build_bridge
if errorlevel 1 exit /b 1

echo.
echo [4/5] Building and packaging...
pushd "%HEBNIX_RS_DIR%"
powershell -NoProfile -ExecutionPolicy Bypass -File package.ps1
set BUILD_ERR=!errorlevel!
popd
if !BUILD_ERR! neq 0 (
    echo ERROR: Package creation failed
    exit /b 1
)
echo.
echo [5/5] Package complete!
echo   Distribution: %HEBNIX_RS_DIR%\dist\
goto end

:do_clean
echo Cleaning build artifacts...
if exist "%HEBNIX_RS_DIR%\target" rmdir /S /Q "%HEBNIX_RS_DIR%\target"
if exist "%HEBNIX_RS_DIR%\dist" rmdir /S /Q "%HEBNIX_RS_DIR%\dist"
if exist "%RLAPI_BRIDGE_DIR%\dist" rmdir /S /Q "%RLAPI_BRIDGE_DIR%\dist"
if exist "%RLAPI_BRIDGE_DIR%\vendor" rmdir /S /Q "%RLAPI_BRIDGE_DIR%\vendor"
echo Clean complete
goto end

:show_help
echo Usage: build.bat [target] [options]
echo.
echo Targets:
echo   (none)        Build everything (default)
echo   bridge        Build rlapi-bridge only
echo   build         Build Rust components only
echo   package       Build and create distribution package
echo   clean         Remove all build artifacts
echo.
echo Options:
echo   --skip-deps   Skip third-party dependency installation
echo   -h, --help    Show this help message
echo.
echo Examples:
echo   build.bat                    Build everything
echo   build.bat package            Build and package for distribution
echo   build.bat build --skip-deps  Build Rust only, skip deps
echo   build.bat clean              Clean build artifacts
exit /b 0

REM === Subroutines ===

:check_tool
where %1 >nul 2>&1
if errorlevel 1 (
    echo ERROR: %~2 not found in PATH
    if not "%~3"=="" echo Please install from: %~3
    exit /b 1
)
echo   [OK] %~2 found
exit /b 0

:install_deps
REM Check curl-impersonate
if not exist "%VENDOR_DIR%\curl-impersonate\curl-impersonate.exe" (
    echo   Installing curl-impersonate...
    call :download_curl_impersonate
    if errorlevel 1 exit /b 1
) else (
    echo   [OK] curl-impersonate already installed
)

REM Check cacert.pem
if not exist "%VENDOR_DIR%\curl-impersonate\cacert.pem" (
    echo   Installing cacert.pem...
    call :download_cacert
    if errorlevel 1 exit /b 1
) else (
    echo   [OK] cacert.pem already installed
)

REM Check steam_api64.dll
if not exist "%VENDOR_DIR%\steam_api64.dll" (
    if %CI_BUILD%==1 (
        echo   WARNING: steam_api64.dll not found
        echo   Build will continue, but steam_api64.dll must be added manually
        echo   Download from: https://partner.steamgames.com/downloads/list
    ) else (
        echo.
        echo   WARNING: steam_api64.dll not found!
        echo   You must manually download the Steamworks SDK from:
        echo   https://partner.steamgames.com/downloads/list
        echo.
        echo   Extract steam_api64.dll to: %VENDOR_DIR%\
        echo.
        echo   Press any key to continue without it, or Ctrl+C to abort...
        pause >nul
    )
) else (
    echo   [OK] steam_api64.dll found
)
exit /b 0

:download_curl_impersonate
if not exist "%VENDOR_DIR%" mkdir "%VENDOR_DIR%"
echo   Downloading curl-impersonate v1.5.6...
powershell -NoProfile -ExecutionPolicy Bypass -Command "& { $ProgressPreference = 'SilentlyContinue'; Invoke-WebRequest -Uri 'https://github.com/lexiforest/curl-impersonate/releases/download/v1.5.6/libcurl-impersonate-v1.5.6.x86_64-win32.tar.gz' -OutFile '%VENDOR_DIR%\curl-impersonate.tar.gz' }"
if errorlevel 1 (
    echo ERROR: Failed to download curl-impersonate
    exit /b 1
)
echo   Verifying SHA256...
powershell -NoProfile -ExecutionPolicy Bypass -Command "& { $hash = (Get-FileHash '%VENDOR_DIR%\curl-impersonate.tar.gz' -Algorithm SHA256).Hash; if ($hash -ne '0b4e5552a818190dc1fd8bc89a4e78ea45df5546c69af8e935c791621bed66f5') { Write-Error 'SHA256 mismatch'; exit 1 } }"
if errorlevel 1 (
    echo ERROR: SHA256 verification failed for curl-impersonate
    exit /b 1
)
echo   Extracting...
tar -xzf "%VENDOR_DIR%\curl-impersonate.tar.gz" -C "%VENDOR_DIR%"
if errorlevel 1 (
    echo ERROR: Failed to extract curl-impersonate
    exit /b 1
)
del "%VENDOR_DIR%\curl-impersonate.tar.gz"
echo   [OK] curl-impersonate installed
exit /b 0

:download_cacert
if not exist "%VENDOR_DIR%\curl-impersonate" mkdir "%VENDOR_DIR%\curl-impersonate"
echo   Downloading cacert.pem...
powershell -NoProfile -ExecutionPolicy Bypass -Command "& { $ProgressPreference = 'SilentlyContinue'; Invoke-WebRequest -Uri 'https://curl.se/ca/cacert-2026-05-14.pem' -OutFile '%VENDOR_DIR%\curl-impersonate\cacert.pem' }"
if errorlevel 1 (
    echo ERROR: Failed to download cacert.pem
    exit /b 1
)
echo   Verifying SHA256...
powershell -NoProfile -ExecutionPolicy Bypass -Command "& { $hash = (Get-FileHash '%VENDOR_DIR%\curl-impersonate\cacert.pem' -Algorithm SHA256).Hash; if ($hash -ne '86a1f3366afac7c6f8ae9f3c779ac221129328c43f0ab2b8817eb2f362a5025c') { Write-Error 'SHA256 mismatch'; exit 1 } }"
if errorlevel 1 (
    echo ERROR: SHA256 verification failed for cacert.pem
    exit /b 1
)
echo   [OK] cacert.pem installed
exit /b 0

:build_bridge
pushd "%RLAPI_BRIDGE_DIR%"
call build.bat
set BUILD_ERR=!errorlevel!
popd
if !BUILD_ERR! neq 0 (
    echo ERROR: rlapi-bridge build failed
    exit /b 1
)
exit /b 0

:build_rust
pushd "%HEBNIX_RS_DIR%"
echo   Building hebnix-app (release)...
cargo build --release --bin hebnix-app
if errorlevel 1 (
    popd
    echo ERROR: hebnix-app build failed
    exit /b 1
)
echo   Building hebnix-lite (release)...
cargo build --release --bin hebnix-lite --features lite
if errorlevel 1 (
    popd
    echo ERROR: hebnix-lite build failed
    exit /b 1
)
popd
exit /b 0

:end
endlocal
exit /b 0

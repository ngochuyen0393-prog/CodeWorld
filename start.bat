@echo off
rem ============================================================
rem CodeWorld launcher (keep ASCII only - cmd parses bat in GBK,
rem UTF-8 Chinese comments break the whole script)
rem 1. delete old world db (fresh world)  2. cargo run
rem debug build is fast; battle lasts ~12s; world idles after
rem prerequisite: Rust installed (cargo in PATH)
rem ============================================================
cd /d %~dp0

if not exist codeworld.db (
    echo [start.bat] no old world found, starting fresh
) else (
    echo [start.bat] deleting old world codeworld.db ...
    del /q codeworld.db
)

echo [start.bat] building and running (first build takes 1-3 min) ...
cargo run -p codeworld
if errorlevel 1 (
    echo.
    echo [start.bat] run failed: check Rust install / cargo in PATH
)

echo.
echo [start.bat] world closed, press any key to exit
pause >nul

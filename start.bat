@echo off
rem ============================================================
rem CodeWorld launcher (ASCII only - cmd parses bat in GBK)
rem stdout = battlefield (ANSI), all logs go to files:
rem   build.log  - cargo build output (first build 1-3 min)
rem   battle.log - game INFO logs (scan/move/attack/kill/plunder)
rem prerequisite: Rust installed (cargo in PATH)
rem ============================================================
cd /d %~dp0

if not exist codeworld.db (
    echo [start.bat] no old world found, starting fresh
) else (
    echo [start.bat] deleting old world codeworld.db ...
    del /q codeworld.db
)

echo [start.bat] building (first build takes 1-3 min, progress in build.log) ...
cargo build -p codeworld 2>build.log
if errorlevel 1 (
    echo.
    echo [start.bat] BUILD FAILED - open build.log to see the reason
    pause
    exit /b 1
)

echo [start.bat] world starting. Watch the screen: red A charges cyan D.
echo [start.bat] Battle report: battle.log   Press Enter (in this window) to close world.
echo.
target\debug\codeworld.exe 2>battle.log

echo.
echo [start.bat] world closed. Battle report saved to battle.log
pause >nul

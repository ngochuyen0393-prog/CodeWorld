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

echo [start.bat] world starting. Melee scenario (default):
echo   red A charges across the field, cyan D holds ground with shield 10
echo   (counterattacks every 4 ticks), yellow H spawns mid-air at (10,8),
echo   lands, then opportunistically joins whoever gets close (50 pct block).
echo   Row 1 of the screen shows live battle caption: tick / A-D distance /
echo   D shield left / alive count.
echo   Outcomes vary: A dies to H, or slays H first then falls to D's wall.
echo   Want the classic 1v1 duel instead? Set CODEWORLD_H_ENABLED=0.
echo   Want A to win? See start-reversal.bat (thin shield, no H).
echo [start.bat] Battle report: battle.log   Press Enter (in this window) to close world.
echo.
target\debug\codeworld.exe 2>battle.log

echo.
echo [start.bat] world closed. Battle report saved to battle.log
pause >nul

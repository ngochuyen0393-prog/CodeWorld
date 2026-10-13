@echo off
rem ============================================================
rem CodeWorld launcher - REVERSAL scenario (ASCII only - cmd parses bat in GBK)
rem Same flow as start.bat but: D's shield = 1 (thin) and H disabled,
rem so A grinds through the shield and kills D (the underdog reversal).
rem Env vars: CODEWORLD_D_SHIELD=1  CODEWORLD_H_ENABLED=0
rem ============================================================
cd /d %~dp0

set CODEWORLD_D_SHIELD=1
set CODEWORLD_H_ENABLED=0

if not exist codeworld.db (
    echo [start-reversal.bat] no old world found, starting fresh
) else (
    echo [start-reversal.bat] deleting old world codeworld.db ...
    del /q codeworld.db
)

echo [start-reversal.bat] building (first build takes 1-3 min, progress in build.log) ...
cargo build -p codeworld 2>build.log
if errorlevel 1 (
    echo.
    echo [start-reversal.bat] BUILD FAILED - open build.log to see the reason
    pause
    exit /b 1
)

echo [start-reversal.bat] world starting. REVERSAL scenario:
echo   red A charges cyan D. D's shield is only 1, so A's third blade
echo   gets through and A kills D (reversal!). H is disabled for a pure duel.
echo [start-reversal.bat] Battle report: battle.log   Press Enter (in this window) to close world.
echo.
target\debug\codeworld.exe 2>battle.log

echo.
echo [start-reversal.bat] world closed. Battle report saved to battle.log
pause >nul

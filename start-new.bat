@echo off
rem ============================================================
rem CodeWorld - NEW PLAYER mode (empty world + onboarding)
rem ASCII only - cmd parses bat in GBK
rem D-021: empty world, you create your character in the browser.
rem ============================================================
cd /d %~dp0

if not exist my-world.db (
    echo [start-new.bat] starting a FRESH world (my-world.db)
) else (
    echo [start-new.bat] keeping YOUR world (my-world.db) - delete it to restart fresh
)
del /q my-world.db-journal my-world.db-wal my-world.db-shm 2>nul

rem block: an old world process locks codeworld.exe and breaks the build below
tasklist /FI "IMAGENAME eq codeworld.exe" 2>nul | find /I "codeworld.exe" >nul && (
    echo [start-new.bat] OLD WORLD IS STILL RUNNING - close it first ^(press q in its window^),
    echo [start-new.bat] otherwise it blocks the build and you keep playing the OLD version.
    pause
    exit /b 1
)

echo [start-new.bat] building (first build takes 1-3 min, progress in build.log) ...
cargo build -p codeworld 2>build.log
if errorlevel 1 (
    echo.
    echo [start-new.bat] BUILD FAILED - open build.log to see the reason
    pause
    exit /b 1
)

echo.
echo [start-new.bat] ================================================
echo [start-new.bat]  NEW PLAYER MODE
echo [start-new.bat]  1. open http://localhost:8080 in your browser
echo [start-new.bat]  2. create your character (name + starter template)
echo [start-new.bat]  3. your creature is born - you control it
echo [start-new.bat]  your code lives at players/YOURNAME.wat
echo [start-new.bat]  (edit that file = edit your creature's behavior)
echo [start-new.bat] ================================================
echo [start-new.bat] Press Enter (in this window) to close world.
echo.

set CODEWORLD_EMPTY_WORLD=1
set CODEWORLD_DB_PATH=my-world.db
target\debug\codeworld.exe 2>battle.log

echo.
echo [start-new.bat] world closed. Battle report saved to battle.log
pause >nul

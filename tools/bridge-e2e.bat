@echo off
setlocal enabledelayedexpansion
rem ============================================================
rem D-001b bridge e2e (ASCII only - UTF-8 Chinese breaks in GBK cmd)
rem Chain: bridge-demo.wat bridge_send -> world outbound HTTP POST
rem        -> fake-bridge-server.py replies {"dx":3} -> world delivers
rem        bridge_on_reply -> creature walks east 3 ticks (marker=6)
rem usage: bridge-e2e.bat [--ci]   (--ci: no pause at the end)
rem ============================================================

cd /d %~dp0..

rem --- 1. pre-check: codeworld.exe must not be running ---
tasklist | findstr /I "codeworld.exe" >nul 2>nul
if !errorlevel! == 0 (
  echo [e2e] FAIL: codeworld.exe already running, close it first.
  exit /b 1
)

rem --- 2. start fake bridge server on 127.0.0.1:18080 (idle 15s self-exit) ---
start /B "" python tools\fake-bridge-server.py > fake-bridge.log 2>&1
ping -n 2 127.0.0.1 >nul

rem --- 3. write vault (world dir, same level as codeworld.db) ---
echo [{"port":1,"url":"http://127.0.0.1:18080","note":"fake bridge server"}]> bridges.json

rem --- 4. env + clean state: only bridge-demo.wat on stage ---
set CODEWORLD_BRIDGE=1
set CODEWORLD_WS_PORT=0
set CODEWORLD_H_ENABLED=0
set CODEWORLD_S_ENABLED=0
set CODEWORLD_PLAYERS_DIR=bridge_test_players
if not exist bridge_test_players mkdir bridge_test_players
copy /Y players\bridge-demo.wat bridge_test_players\ >nul
del /Q codeworld.db 2>nul

rem --- 5. run world for 25 ticks (1s each): background cmd, main flow waits then kills ---
rem (world process is resident by design; piping stdin would deadlock the caller -
rem  so run detached, wait out the ticks, then verify+kill)
start /B "" cmd /c "target\debug\codeworld.exe >nul 2>battle.log"
ping -n 26 127.0.0.1 >nul

rem --- 6. cleanup: verify before kill (never kill blind) ---
tasklist | findstr /I "codeworld.exe" >nul 2>nul
if !errorlevel! == 0 (
  taskkill /F /IM codeworld.exe >nul 2>nul
  echo [e2e] note: killed leftover codeworld.exe
)
rem fake bridge server self-exits when idle - no python killing here (never
rem kill blind: other python processes on this machine are none of our business)

rem --- 7. strip ANSI color codes: tracing splits marker=N with color escapes ---
rem (same trap as D-007 smoke: raw battle.log cannot be matched by findstr)
python -c "import re;d=open('battle.log','rb').read();open('battle-clean.log','wb').write(re.sub(b'\x1b[[0-9;]*m',b'',d))"

rem --- 8. assert five-step chain on the clean log ---
set PASS=1
echo [e2e] ---- five-step chain assertions ----

rem step 1: world queued the letter (outbound queue line, payload_len=6)
findstr /C:"payload_len=6" battle-clean.log >nul 2>nul
if !errorlevel! == 0 (
  echo [e2e] step1 bridge_send queued: PASS
) else (
  echo [e2e] step1 bridge_send queued: FAIL
  set PASS=0
)

rem step 1b: world sent it out (outbound INFO line, 6 bytes of WHERE?)
findstr /C:"bytes=6" battle-clean.log >nul 2>nul
if !errorlevel! == 0 (
  echo [e2e] step1b outbound line bytes=6: PASS
) else (
  echo [e2e] step1b outbound line bytes=6: FAIL
  set PASS=0
)

rem step 2: fake server received the letter
findstr /C:"POST received" fake-bridge.log >nul 2>nul
if !errorlevel! == 0 (
  echo [e2e] step2 fake server received: PASS
) else (
  echo [e2e] step2 fake server received: FAIL
  set PASS=0
)

rem step 3: reply delivered via grow allocation (arena-side INFO line)
findstr /C:"call bridge_on_reply" battle-clean.log >nul 2>nul
if !errorlevel! == 0 (
  echo [e2e] step3 call bridge_on_reply: PASS
) else (
  echo [e2e] step3 call bridge_on_reply: FAIL
  set PASS=0
)

rem step 4: creature parsed digit 3 from the reply (marker=5)
findstr /C:"marker=5" battle-clean.log >nul 2>nul
if !errorlevel! == 0 (
  echo [e2e] step4 creature got command marker=5: PASS
) else (
  echo [e2e] step4 creature got command marker=5: FAIL
  set PASS=0
)

rem step 5: behavior changed - creature walked east (marker=6)
findstr /C:"marker=6" battle-clean.log >nul 2>nul
if !errorlevel! == 0 (
  echo [e2e] step5 creature walked east marker=6: PASS
) else (
  echo [e2e] step5 creature walked east marker=6: FAIL
  set PASS=0
)

echo [e2e] ---- excerpt: outbound ----
findstr /C:"bytes=6" battle-clean.log 2>nul
echo [e2e] ---- excerpt: fake server ----
type fake-bridge.log 2>nul
echo [e2e] ---- excerpt: reply delivery ----
findstr /C:"call bridge_on_reply" battle-clean.log 2>nul

if !PASS! == 1 (
  echo [e2e] RESULT: ALL PASS
) else (
  echo [e2e] RESULT: FAILED - see battle-clean.log / fake-bridge.log
)
if "%1" NEQ "--ci" pause
if !PASS! == 1 (exit /b 0) else (exit /b 1)

@echo off
rem ============================================================
rem CodeWorld 最简可玩版启动脚本
rem 作用：删除旧世界数据库（开新世界）-> 编译并运行主程序
rem 说明：不用 --release——debug 编译快得多，MVP tick 1秒/拍性能足够；
rem       战斗约 12 秒演完（A 冲锋 9 步 + 3 刀 + D 反制击杀），之后世界空转
rem 前置：本机需已安装 Rust（cargo 在 PATH）
rem ============================================================
cd /d %~dp0

if not exist codeworld.db (
    echo [start.bat] 未发现旧世界，直接开新世界
) else (
    echo [start.bat] 删除旧世界 codeworld.db ...
    del /q codeworld.db
)

echo [start.bat] 正在编译运行（首次编译需 1-3 分钟）...
cargo run -p codeworld
if errorlevel 1 (
    echo.
    echo [start.bat] 运行失败：请确认已安装 Rust 且 cargo 在 PATH 中
)

echo.
echo [start.bat] 世界已关闭，按任意键退出
pause >nul

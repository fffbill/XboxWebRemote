@echo off
rem Xbox 网页串流 - Windows 启动脚本
chcp 65001 >nul
cd /d "%~dp0"

rem 1. 检查 Python
python --version >nul 2>&1
if errorlevel 1 (
  echo [X] 未找到 Python, 请先安装 Python 3.9+ : https://www.python.org/downloads/
  pause & exit /b 1
)

rem 2. 检查/安装 cryptography (XAL 签名用, 首次运行需要)
python -c "import cryptography" >nul 2>&1
if errorlevel 1 (
  echo [i] 首次运行: 安装 cryptography 库...
  python -m pip install --user cryptography
  if errorlevel 1 (
    echo [X] cryptography 安装失败, 请手动执行: python -m pip install cryptography
    pause & exit /b 1
  )
)

rem 3. 启动服务 (会自动注册 ms-xal:// 登录回调协议)
start "XboxWebRemote" /min cmd /c "python server.py"

rem 4. 等服务就绪后打开浏览器
timeout /t 2 /nobreak >nul
start "" http://127.0.0.1:8977

echo.
echo  ================================================
echo   Xbox 网页串流已启动: http://127.0.0.1:8977
echo   - 登录回调协议 ms-xal:// 已自动注册
echo   - 关闭此窗口即停止服务
echo  ================================================
echo.
pause

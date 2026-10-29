#!/bin/zsh
# Xbox 网页串流 启动器
# 双击运行: 启动本地服务并打开浏览器 (Chrome 优先)

DIR="$(cd "$(dirname "$0")" && pwd)"
PORT=8977
URL="http://127.0.0.1:${PORT}"

# 如果端口上已有旧实例, 先关掉 (同时会结束上次遗留的 server.py)
OLD_PIDS=$(lsof -ti tcp:${PORT} -sTCP:LISTEN 2>/dev/null)
if [ -n "$OLD_PIDS" ]; then
  kill $OLD_PIDS 2>/dev/null
  sleep 1
fi

cd "$DIR"

# 注册 ms-xal-… 登录回调协议处理器 (幂等)
LSREG="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
[ -x "$LSREG" ] && "$LSREG" -f "$DIR/XboxLoginHelper.app" >/dev/null 2>&1

# 后台启动服务, 日志留在本窗口
"${AUTOPY:-python3}" server.py &
SERVER_PID=$!

# 等服务就绪
for i in {1..20}; do
  if curl -s -o /dev/null --max-time 1 "$URL/api/auth/status"; then break; fi
  sleep 0.5
done

# 打开浏览器: 优先 Chrome (手柄/键盘兼容性最好), 否则默认浏览器
if [ -d "/Applications/Google Chrome.app" ]; then
  open -a "Google Chrome" "$URL"
else
  open "$URL"
fi

echo ""
echo "================================================"
echo "  ✅ 网页已在浏览器打开: $URL"
echo "  · 服务器日志显示在本窗口, 关闭窗口即停止服务"
echo "  · 令牌保存在: $DIR/tokens.json"
echo "================================================"
echo ""

# 保持窗口: 挂住 server 进程, Ctrl+C 或关窗口退出
wait $SERVER_PID

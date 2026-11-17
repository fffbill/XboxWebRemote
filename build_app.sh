#!/bin/bash
# 构建 Xbox网页串流.app 并安装到 /Applications
# 用法: ./build_app.sh   (在仓库根目录执行)
set -e
cd "$(dirname "$0")"

APP_NAME="Xbox网页串流"
APP_DIR="build/${APP_NAME}.app"
CONTENTS="$APP_DIR/Contents"

echo "==> 清理旧的构建产物"
rm -rf build
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"

echo "==> 编译 Swift 启动器"
swiftc -O -o "$CONTENTS/MacOS/XboxWebRemote" app-src/launcher.swift

echo "==> 生成图标 (Xbox 绿色标志 → icns)"
ICON_SRC=""
# qlmanage 遵守 svg 的 width/height 属性 (16px), 必须先放大到 1024 再渲染
sed -e 's/width="16" height="16"/width="1024" height="1024"/' www/assets/xbox-green.svg > build/xbox-1024.svg 2>/dev/null
if qlmanage -t -s 1024 -o build build/xbox-1024.svg >/dev/null 2>&1; then
  ICON_SRC="build/xbox-1024.svg.png"
fi
if [ -z "$ICON_SRC" ] || [ ! -s "$ICON_SRC" ]; then
  # 回退: 用主机产品图
  cp www/assets/series-x.png build/icon-src.png
  ICON_SRC="build/icon-src.png"
fi
mkdir -p build/AppIcon.iconset
# iconset 只接受固定命名 (16/16@2x/32/32@2x/128/128@2x/256/256@2x/512/512@2x)
# 生成精确尺寸并按规范命名 — 多余或非法命名的文件会被 iconutil 静默丢弃 (曾因此缺 512@2x 导致聚焦/启动台空白)
sips -z 16 16   "$ICON_SRC" --out build/AppIcon.iconset/icon_16x16.png      >/dev/null
sips -z 32 32   "$ICON_SRC" --out build/AppIcon.iconset/icon_16x16@2x.png   >/dev/null
sips -z 32 32   "$ICON_SRC" --out build/AppIcon.iconset/icon_32x32.png      >/dev/null
sips -z 64 64   "$ICON_SRC" --out build/AppIcon.iconset/icon_32x32@2x.png   >/dev/null
sips -z 128 128 "$ICON_SRC" --out build/AppIcon.iconset/icon_128x128.png    >/dev/null
sips -z 256 256 "$ICON_SRC" --out build/AppIcon.iconset/icon_128x128@2x.png >/dev/null
sips -z 256 256 "$ICON_SRC" --out build/AppIcon.iconset/icon_256x256.png    >/dev/null
sips -z 512 512 "$ICON_SRC" --out build/AppIcon.iconset/icon_256x256@2x.png >/dev/null
sips -z 512 512 "$ICON_SRC" --out build/AppIcon.iconset/icon_512x512.png    >/dev/null
sips -z 1024 1024 "$ICON_SRC" --out build/AppIcon.iconset/icon_512x512@2x.png >/dev/null
iconutil -c icns build/AppIcon.iconset -o "$CONTENTS/Resources/AppIcon.icns"

echo "==> 拷贝资源"
cp server.py "$CONTENTS/Resources/"
cp -R www "$CONTENTS/Resources/www"
mkdir -p "$CONTENTS/Resources/bin"
[ -f bin/ffmpeg ] && cp bin/ffmpeg "$CONTENTS/Resources/bin/ffmpeg"
cp -R XboxLoginHelper.app "$CONTENTS/Resources/XboxLoginHelper.app"

cat > "$CONTENTS/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>Xbox网页串流</string>
	<key>CFBundleDisplayName</key><string>Xbox网页串流</string>
	<key>CFBundleIdentifier</key><string>local.xboxwebremote.app</string>
	<key>CFBundleExecutable</key><string>XboxWebRemote</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleShortVersionString</key><string>1.2</string>
	<key>CFBundleVersion</key><string>12</string>
	<key>CFBundleIconFile</key><string>AppIcon</string>
	<key>LSMinimumSystemVersion</key><string>12.0</string>
	<key>NSHighResolutionCapable</key><true/>
	<key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
</dict>
</plist>
PLIST

echo "==> ad-hoc 签名"
xattr -cr "$APP_DIR" 2>/dev/null || true
codesign --force --deep --sign - "$APP_DIR"

echo "==> 安装到 /Applications (迁移令牌)"
mkdir -p "$HOME/Library/Application Support/XboxWebRemote"
# 迁移现有登录令牌 (桌面运行版 → App 数据目录), 保留登录状态
for src in "$HOME/Desktop/Xbox网页串流/tokens.json" "$HOME/.zcode/workspace/default/XboxWebRemote/tokens.json"; do
  if [ -f "$src" ] && [ ! -f "$HOME/Library/Application Support/XboxWebRemote/tokens.json" ]; then
    cp "$src" "$HOME/Library/Application Support/XboxWebRemote/tokens.json"
    chmod 600 "$HOME/Library/Application Support/XboxWebRemote/tokens.json"
    echo "    已迁移令牌: $src"
    break
  fi
done
rm -rf "/Applications/${APP_NAME}.app"
cp -R "$APP_DIR" "/Applications/${APP_NAME}.app"
xattr -dr com.apple.quarantine "/Applications/${APP_NAME}.app" 2>/dev/null || true

echo ""
echo "✅ 构建完成: /Applications/${APP_NAME}.app"
echo "   - 数据目录: ~/Library/Application Support/XboxWebRemote/"
echo "   - 日志: ~/Library/Logs/XboxWebRemote.log"

// libopus 静态编译 (vendored 源码, 无需系统依赖)
use std::path::PathBuf;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let opus = PathBuf::from(&manifest).join("vendor/opus-1.5.2");

    if !opus.join("src/opus_decoder.c").is_file() {
        // 无源码时跳过 (音频功能不可用, 其余功能不受影响)
        println!("cargo:warning=vendor/opus-1.5.2 不存在, 音频解码禁用");
        return;
    }

    let mut build = cc::Build::new();

    // opus 浮点构建的标准配置
    build.define("OPUS_BUILD", None);
    build.define("HAVE_LRINT", None);
    build.define("HAVE_LRINTF", None);
    build.define("VAR_ARRAYS", None);
    build.define("PACKAGE_VERSION", Some("\"1.5.2\""));
    build.flag("-O2");

    // 头文件搜索路径
    build.include(opus.join("include"));
    build.include(opus.join("celt"));
    build.include(opus.join("silk"));
    build.include(opus.join("silk/float"));
    build.include(opus.join("src"));

    // 编译解码所需的 .c 文件 (float 构建; src/ 只需解码器两件, 排除编码器)
    let mut sources: Vec<PathBuf> = Vec::new();
    for dir in ["celt", "silk", "silk/float", "src"] {
        let d = opus.join(dir);
        for entry in std::fs::read_dir(&d).unwrap() {
            let f = entry.unwrap().path();
            if f.extension().and_then(|e| e.to_str()) == Some("c") {
                let name = f.file_name().unwrap().to_string_lossy().into_owned();
                if dir == "src" && !["opus.c", "opus_decoder.c"].contains(&name.as_str()) {
                    continue;
                }
                sources.push(f);
            }
        }
    }
    // silk/fixed 与 silk/hybrid 等目录在 float 构建下不需要
    for s in sources {
        build.file(s);
    }

    build.warnings(false);
    build.compile("opus");

    // 链接 CoreMedia/CoreVideo 等系统框架在此不需要 (opus 纯 C, 无系统依赖)

    // 头文件供 include 使用
    println!("cargo:include={}", opus.join("include").display());
}

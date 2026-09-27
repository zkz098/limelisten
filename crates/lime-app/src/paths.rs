//! 工具/模型/数据目录的定位（绿色便携优先，开发目录兜底）。

use std::path::{Path, PathBuf};

/// 项目根（开发态）：`<manifest>/../../`
fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// exe 所在目录（便携态）
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn candidates(rel: &str) -> Vec<PathBuf> {
    vec![product_root().join(rel), project_root().join(rel), exe_dir().join(rel)]
}

/// 运行时的“产品根目录”：优先 exe 旁边的 portable 目录结构，其次开发项目根。
fn product_root() -> PathBuf {
    let exe = exe_dir();
    // 便携结构：<root>/limelisten.exe + <root>/tools + <root>/models
    if exe.join("tools").is_dir() || exe.join("models").is_dir() {
        return exe;
    }
    // 开发态（cargo run）：exe 在 target/… 下，项目根里有 Cargo.toml，用项目根；
    // 发布包拷到别的机器时该路径不存在，退回 exe 目录，保证下载的模型/工具落在自己旁边。
    let proj = project_root();
    if proj.join("Cargo.toml").is_file() {
        proj
    } else {
        exe
    }
}

/// 找 whisper-cli.exe（cublas 优先，其次 blas/纯 CPU）
pub fn find_whisper_exe() -> Option<PathBuf> {
    const RELS: &[&str] = &[
        "tools/whisper/cuda/Release/whisper-cli.exe",
        "tools/whisper/cublas12.4/Release/whisper-cli.exe",
        "tools/whisper/cublas/Release/whisper-cli.exe",
        "tools/whisper/vulkan/Release/whisper-cli.exe",
        "tools/whisper/blas/Release/whisper-cli.exe",
        "tools/whisper/cpu/Release/whisper-cli.exe",
        // 开发期实际下载位置
        ".tools/cublas12.4/Release/whisper-cli.exe",
        ".tools/vulkan/Release/whisper-cli.exe",
        ".tools/blas/Release/whisper-cli.exe",
    ];
    for rel in RELS {
        for c in candidates(rel) {
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// 找 ggml 模型：优先已下载的大模型，其次小模型。
pub fn find_model(prefer: &[&str]) -> Option<PathBuf> {
    let models_dir_candidates = [
        product_root().join("models"),
        project_root().join("models"),
        project_root().join(".tools/models"),
    ];
    for dir in models_dir_candidates.iter() {
        if !dir.is_dir() {
            continue;
        }
        for name in prefer {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    // 兜底：目录里任意 ggml-*.bin
    for dir in models_dir_candidates.iter() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut found: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("ggml-") && n.ends_with(".bin"))
                        .unwrap_or(false)
                })
                .collect();
            found.sort();
            if let Some(p) = found.into_iter().next() {
                return Some(p);
            }
        }
    }
    None
}

/// 默认优先模型（GRILLING 第 5 轮：turbo q5_0 为准）
pub fn default_model() -> Option<PathBuf> {
    find_model(&[
        "ggml-large-v3-turbo-q5_0.bin",
        "ggml-medium.en.bin",
        "ggml-small.en.bin",
        "ggml-base.en.bin",
    ])
}

/// 按规格名称查找模型 (turbo, small, base, tiny)
#[allow(dead_code)]
pub fn find_model_by_spec(spec: &str) -> Option<PathBuf> {
    let prefer: &[&str] = match spec {
        "turbo" => &[
            "ggml-large-v3-turbo-q5_0.bin",
            "ggml-large-v3-turbo.bin",
            "ggml-turbo.bin",
        ],
        "small" => &["ggml-small.en.bin", "ggml-small.bin"],
        "base" => &["ggml-base.en.bin", "ggml-base.bin"],
        "tiny" => &["ggml-tiny.en.bin", "ggml-tiny.bin"],
        _ => return None,
    };
    let models_dir_candidates = [
        product_root().join("models"),
        project_root().join("models"),
        project_root().join(".tools/models"),
    ];
    for dir in models_dir_candidates.iter() {
        if !dir.is_dir() {
            continue;
        }
        for name in prefer {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// 模型存放主目录（新建模型下载位置）
pub fn models_dir() -> PathBuf {
    let d = product_root().join("models");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub struct ModelSpecMeta {
    pub spec: &'static str,
    pub name: &'static str,
    pub filename: &'static str,
    pub desc: &'static str,
    pub approx_size_mb: u32,
    pub size_bytes: u64,
    pub primary_url: &'static str,
    pub fallback_url: &'static str,
}

/// 支持的 4 款测速与下载模型规格定义
pub const MODEL_SPECS: &[ModelSpecMeta] = &[
    ModelSpecMeta {
        spec: "turbo",
        name: "Large-v3-Turbo",
        filename: "ggml-large-v3-turbo.bin",
        desc: "高精度首选，复杂杂音及考试真题推荐 (~547MB)",
        approx_size_mb: 547,
        size_bytes: 574_041_195,
        primary_url: "https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        fallback_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
    },
    ModelSpecMeta {
        spec: "small",
        name: "Small",
        filename: "ggml-small.bin",
        desc: "速度与精度均衡，中端硬件适用 (~466MB)",
        approx_size_mb: 466,
        size_bytes: 487_614_201,
        primary_url: "https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        fallback_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
    },
    ModelSpecMeta {
        spec: "base",
        name: "Base",
        filename: "ggml-base.bin",
        desc: "极速轻量，轻薄本与普通核显流畅运行 (~142MB)",
        approx_size_mb: 142,
        size_bytes: 147_964_211,
        primary_url: "https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        fallback_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
    },
    ModelSpecMeta {
        spec: "tiny",
        name: "Tiny",
        filename: "ggml-tiny.bin",
        desc: "极轻超快，极低算力与内存要求 (~75MB)",
        approx_size_mb: 75,
        size_bytes: 77_691_713,
        primary_url: "https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        fallback_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
    },
];

pub fn get_spec_meta(spec: &str) -> Option<&'static ModelSpecMeta> {
    MODEL_SPECS.iter().find(|m| m.spec == spec)
}

// ---------------------------------------------------------------------------
// whisper-cli 转写引擎：软件内一键下载（官方 Windows 预编译包，解压即用）
// ---------------------------------------------------------------------------

/// 转写引擎根目录：`<产品根>/tools/whisper`
pub fn whisper_dir() -> PathBuf {
    let d = product_root().join("tools").join("whisper");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub struct WhisperBuildMeta {
    /// 目录/标识：`cpu` / `blas` / `cublas12.4`（与 `find_whisper_exe` 的搜索路径对应）
    pub id: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    /// 官方发布包文件名（zip 内为 `Release/whisper-cli.exe` + DLL）
    pub asset: &'static str,
    pub size_bytes: u64,
    /// 依次尝试的下载地址（国内镜像优先，GitHub 直连兜底）
    pub urls: &'static [&'static str],
}

/// 三种 x64 运行包。zip 解压后落在 `tools/whisper/<id>/Release/`，与 `find_whisper_exe` 对齐。
pub const WHISPER_BUILDS: &[WhisperBuildMeta] = &[
    WhisperBuildMeta {
        id: "cpu",
        name: "CPU 通用版",
        desc: "纯 CPU 运行，任何机器都能用",
        asset: "whisper-bin-x64.zip",
        size_bytes: 7_982_101,
        urls: &[
            "https://gh-proxy.com/https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.1/whisper-bin-x64.zip",
            "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.1/whisper-bin-x64.zip",
        ],
    },
    WhisperBuildMeta {
        id: "blas",
        name: "OpenBLAS 加速版",
        desc: "CPU + OpenBLAS，速度更稳（推荐）",
        asset: "whisper-blas-bin-x64.zip",
        size_bytes: 20_769_031,
        urls: &[
            "https://gh-proxy.com/https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.1/whisper-blas-bin-x64.zip",
            "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.1/whisper-blas-bin-x64.zip",
        ],
    },
    WhisperBuildMeta {
        id: "cublas12.4",
        name: "NVIDIA CUDA 12.4 版",
        desc: "N 卡加速，需已安装 NVIDIA 驱动",
        asset: "whisper-cublas-12.4.0-bin-x64.zip",
        size_bytes: 677_887_125,
        urls: &[
            "https://gh-proxy.com/https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.1/whisper-cublas-12.4.0-bin-x64.zip",
            "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.1/whisper-cublas-12.4.0-bin-x64.zip",
        ],
    },
];

pub fn get_whisper_build(id: &str) -> Option<&'static WhisperBuildMeta> {
    WHISPER_BUILDS.iter().find(|b| b.id == id)
}

/// 某个构建包的 `whisper-cli.exe` 是否已就位
pub fn whisper_build_installed(id: &str) -> bool {
    whisper_dir()
        .join(id)
        .join("Release")
        .join("whisper-cli.exe")
        .is_file()
}

/// 缓存/数据目录（分析中间产物：16k wav、whisper json）
pub fn data_dir() -> PathBuf {
    let d = product_root().join("data");
    let _ = std::fs::create_dir_all(&d);
    d
}

#[allow(dead_code)]
pub fn cache_dir() -> PathBuf {
    let d = data_dir().join("cache");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// 用文件路径 + 修改时间生成稳定的缓存 key
#[allow(dead_code)]
pub fn cache_key(path: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    path.to_string_lossy().hash(&mut h);
    if let Ok(md) = std::fs::metadata(path) {
        md.len().hash(&mut h);
        if let Ok(t) = md.modified() {
            if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
                d.as_secs().hash(&mut h);
            }
        }
    }
    format!("{:016x}", h.finish())
}

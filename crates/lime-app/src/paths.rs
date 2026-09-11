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

/// 运行时的“产品根目录”：优先 exe 旁边的 portable 目录结构，其次项目根。
fn product_root() -> PathBuf {
    let exe = exe_dir();
    // 便携结构：<root>/limelisten.exe + <root>/tools + <root>/models
    if exe.join("tools").is_dir() || exe.join("models").is_dir() {
        exe
    } else {
        project_root()
    }
}

/// 找 whisper-cli.exe（cublas 优先，其次 blas/纯 CPU）
pub fn find_whisper_exe() -> Option<PathBuf> {
    const RELS: &[&str] = &[
        "tools/whisper/cuda/Release/whisper-cli.exe",
        "tools/whisper/cublas12.4/Release/whisper-cli.exe",
        "tools/whisper/cublas/Release/whisper-cli.exe",
        "tools/whisper/blas/Release/whisper-cli.exe",
        "tools/whisper/cpu/Release/whisper-cli.exe",
        // 开发期实际下载位置
        ".tools/cublas12.4/Release/whisper-cli.exe",
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

/// 缓存/数据目录（分析中间产物：16k wav、whisper json）
pub fn data_dir() -> PathBuf {
    let d = product_root().join("data");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn cache_dir() -> PathBuf {
    let d = data_dir().join("cache");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// 用文件路径 + 修改时间生成稳定的缓存 key
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

//! 应用自更新模块（基于 `sigilup` 后量子混合签名与原子替换）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sigilup::manifest::FileEpochStore;
use sigilup::replace::{install_portable_update, spawn_relaunch};
use sigilup::transport::BlockingUpdater;
use sigilup::{
    semver, AvailableUpdate, KeyPinFile, UpdateDecision, UpdateEvaluator, UrlPolicy, VerifyContext,
};
use slint::ComponentHandle as _;

use crate::paths;
use crate::MainWindow;

const PINS_JSON: &str = include_str!("../sigilup-pins.json");

const MANIFEST_URLS: &[&str] = &[
    "https://github.com/zkz098/limelisten/releases/latest/download/latest.hsig.json",
    "https://gh-proxy.com/https://github.com/zkz098/limelisten/releases/latest/download/latest.hsig.json",
];

const ALLOWED_URL_PREFIXES: &[&str] = &[
    "https://github.com/zkz098/limelisten/releases/",
    "https://gh-proxy.com/https://github.com/zkz098/limelisten/releases/",
];

const EXPECTED_KIND: &str = "portable";
const MAX_EXE_BYTES: u64 = 256 * 1024 * 1024;

#[cfg(windows)]
const EXPECTED_EXE_NAME: &str = "limelisten.exe";
#[cfg(not(windows))]
const EXPECTED_EXE_NAME: &str = "limelisten";

/// UI 更新状态码（与 `ui/app.slint` 中 `update-state` 对齐）。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiUpdateState {
    Idle = 0,
    Checking = 1,
    Available = 2,
    Installing = 3,
    ReadyToRestart = 4,
    Error = 5,
}

#[derive(Default)]
struct UpdaterShared {
    busy: AtomicBool,
    pending_update: Mutex<Option<AvailableUpdate>>,
}

/// 构造 `UpdateEvaluator`，统一绑定信任根、版本、渠道与 URL 白名单策略。
pub fn build_evaluator(current_version: semver::Version) -> Result<UpdateEvaluator, String> {
    let pins = KeyPinFile::from_json_str(PINS_JSON).map_err(|e| format!("解析信任根失败: {e}"))?;
    let trust_store = pins.to_trust_store();
    let url_policy = UrlPolicy::with_prefixes(ALLOWED_URL_PREFIXES.iter().copied());
    Ok(UpdateEvaluator::new(trust_store, current_version)
        .with_url_policy(url_policy)
        .with_expected_kind(EXPECTED_KIND))
}

fn current_app_version() -> Result<semver::Version, String> {
    semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| format!("解析当前版本号失败: {e}"))
}

fn epoch_store_path() -> PathBuf {
    paths::data_dir().join("update-epoch.json")
}

fn staging_dir_path() -> PathBuf {
    paths::cache_dir().join("update-staging")
}

fn push_ui_state(
    ui_weak: &slint::Weak<MainWindow>,
    state: UiUpdateState,
    status: String,
    progress: f32,
) {
    let ui_weak = ui_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        ui.set_update_state(state as i32);
        ui.set_update_status(status.into());
        ui.set_update_progress(progress);
    });
}

fn run_check_worker(ui_weak: slint::Weak<MainWindow>, shared: Arc<UpdaterShared>) {
    let outcome = perform_check();
    shared.busy.store(false, Ordering::SeqCst);

    match outcome {
        Ok(UpdateDecision::UpToDate) => {
            if let Ok(mut guard) = shared.pending_update.lock() {
                *guard = None;
            }
            push_ui_state(
                &ui_weak,
                UiUpdateState::Idle,
                format!("当前已是最新版本 (v{})", env!("CARGO_PKG_VERSION")),
                0.0,
            );
        }
        Ok(UpdateDecision::UpdateAvailable(update)) => {
            let ver = update.version.to_string();
            let notes = update.notes.clone();
            if let Ok(mut guard) = shared.pending_update.lock() {
                *guard = Some(*update);
            }
            let ui_weak_clone = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = ui_weak_clone.upgrade() else {
                    return;
                };
                ui.set_update_available_version(ver.clone().into());
                ui.set_update_notes(notes.into());
                ui.set_update_state(UiUpdateState::Available as i32);
                ui.set_update_status(format!("发现新版本 v{ver}").into());
                ui.set_update_progress(0.0);
            });
        }
        Err(err) => {
            push_ui_state(&ui_weak, UiUpdateState::Error, err, 0.0);
        }
    }
}

fn perform_check() -> Result<UpdateDecision, String> {
    let current_ver = current_app_version()?;
    let mut evaluator = build_evaluator(current_ver)?;
    let mut epoch_store = FileEpochStore::new(epoch_store_path());
    let updater = BlockingUpdater::default();
    updater
        .check_for_update_any(
            MANIFEST_URLS,
            &mut evaluator,
            &mut epoch_store,
            VerifyContext::now(),
        )
        .map_err(|e| format!("检查更新失败: {e}"))
}

fn run_install_worker(
    ui_weak: slint::Weak<MainWindow>,
    shared: Arc<UpdaterShared>,
    update: AvailableUpdate,
) {
    let result = perform_download_and_install(&ui_weak, &update);
    shared.busy.store(false, Ordering::SeqCst);

    match result {
        Ok(()) => {
            if let Ok(mut guard) = shared.pending_update.lock() {
                *guard = None;
            }
            push_ui_state(
                &ui_weak,
                UiUpdateState::ReadyToRestart,
                format!("v{} 已就绪，重启后生效", update.version),
                1.0,
            );
        }
        Err(err) => {
            push_ui_state(&ui_weak, UiUpdateState::Error, err, 0.0);
        }
    }
}

fn perform_download_and_install(
    ui_weak: &slint::Weak<MainWindow>,
    update: &AvailableUpdate,
) -> Result<(), String> {
    let staging = staging_dir_path();
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("创建暂存目录失败: {e}"))?;

    let updater = BlockingUpdater::default();
    let mut last_pct: i32 = -1;
    let archive_path = updater
        .download_and_verify(update, &staging, |downloaded, total| {
            let ratio = total
                .filter(|&t| t > 0)
                .map(|t| (downloaded as f64 / t as f64).clamp(0.0, 1.0) as f32)
                .unwrap_or(0.0);
            let pct = (ratio * 100.0).round() as i32;
            if pct != last_pct && (pct % 2 == 0 || pct == 100) {
                last_pct = pct;
                push_ui_state(
                    ui_weak,
                    UiUpdateState::Installing,
                    format!("正在下载并校验更新 ({pct}%)..."),
                    ratio * 0.9,
                );
            }
        })
        .map_err(|e| format!("下载或签名校验失败: {e}"))?;

    push_ui_state(
        ui_weak,
        UiUpdateState::Installing,
        "正在替换可执行文件...".into(),
        0.95,
    );

    install_portable_update(&archive_path, &staging, EXPECTED_EXE_NAME, MAX_EXE_BYTES)
        .map_err(|e| format!("替换可执行文件失败: {e}"))?;

    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

/// 绑定自更新相关回调到 Slint 主窗口。
pub fn wire_updater(ui: &MainWindow) {
    let shared = Arc::new(UpdaterShared::default());

    {
        let ui_weak = ui.as_weak();
        let shared = Arc::clone(&shared);
        ui.on_check_for_updates(move || {
            if shared.busy.swap(true, Ordering::SeqCst) {
                return;
            }
            push_ui_state(
                &ui_weak,
                UiUpdateState::Checking,
                "正在检查更新...".into(),
                0.0,
            );
            let ui_weak_bg = ui_weak.clone();
            let shared_bg = Arc::clone(&shared);
            std::thread::spawn(move || run_check_worker(ui_weak_bg, shared_bg));
        });
    }

    {
        let ui_weak = ui.as_weak();
        let shared = Arc::clone(&shared);
        ui.on_download_and_install_update(move || {
            let Some(update) = shared.pending_update.lock().ok().and_then(|g| g.clone()) else {
                return;
            };
            if shared.busy.swap(true, Ordering::SeqCst) {
                return;
            }
            push_ui_state(
                &ui_weak,
                UiUpdateState::Installing,
                "正在准备下载更新...".into(),
                0.0,
            );
            let ui_weak_bg = ui_weak.clone();
            let shared_bg = Arc::clone(&shared);
            std::thread::spawn(move || run_install_worker(ui_weak_bg, shared_bg, update));
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_restart_to_update(move || {
            if let Err(err) = spawn_relaunch(std::iter::empty::<&str>()) {
                push_ui_state(
                    &ui_weak,
                    UiUpdateState::Error,
                    format!("重启失败: {err}"),
                    0.0,
                );
                return;
            }
            std::process::exit(0);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_and_url_policy_build_cleanly() {
        let mut evaluator = build_evaluator(semver::Version::new(0, 1, 4))
            .expect("embedded sigilup-pins.json and URL allowlist should be valid");
        assert_eq!(evaluator.expected_kind.as_deref(), Some("portable"));
        assert!(evaluator
            .url_policy
            .validate("https://github.com/zkz098/limelisten/releases/download/v0.1.5/a.zip")
            .is_ok());
        assert!(
            evaluator
                .url_policy
                .validate("https://gh-proxy.com/https://github.com/zkz098/limelisten/releases/download/v0.1.5/a.zip")
                .is_ok()
        );
        assert!(evaluator
            .url_policy
            .validate("https://evil.example.com/a.zip")
            .is_err());
        // Ensure evaluator stays mutable-usable without warnings
        evaluator.channel = None;
    }
}

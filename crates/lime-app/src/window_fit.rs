//! 启动与跨屏缩放时的窗口尺寸自适应：
//! 防止高 DPI 缩放（如 1080p @ 150%/200%）或小分辨率屏幕（如 1366x768、1280x720）
//! 下主窗口超出屏幕可用工作区，导致底部控制栏被任务栏遮挡或无法缩小。

use crate::MainWindow;
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint::ComponentHandle;

/// 设计基准尺寸（与 `ui/app.slint` 中 `MainWindow` 的 `preferred-*` 一致）。
const PREFERRED_LOGICAL_WIDTH: f64 = 1320.0;
const PREFERRED_LOGICAL_HEIGHT: f64 = 860.0;
const BASE_MIN_LOGICAL_WIDTH: f64 = 640.0;
const BASE_MIN_LOGICAL_HEIGHT: f64 = 440.0;

/// 为系统窗口标题栏与边框预留的逻辑像素余量，确保整个窗口外框不越出工作区。
const FRAME_MARGIN_W_LOGICAL: f64 = 16.0;
const FRAME_MARGIN_H_LOGICAL: f64 = 44.0;
#[cfg(not(windows))]
const NON_WINDOWS_PANEL_H_LOGICAL: f64 = 48.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkAreaPhys {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct FittedGeometry {
    inner_width_logical: f32,
    inner_height_logical: f32,
    min_width_logical: f32,
    min_height_logical: f32,
    outer_x_phys: i32,
    outer_y_phys: i32,
    was_clamped: bool,
}

/// 计算适应指定物理工作区与缩放倍率的窗口内框尺寸和居中外框位置。
fn compute_fitted_geometry(
    work: WorkAreaPhys,
    scale_factor: f64,
    current_logical_size: Option<(f64, f64)>,
) -> FittedGeometry {
    let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let work_w_logical = work.width as f64 / scale;
    let work_h_logical = work.height as f64 / scale;

    let max_inner_w = (work_w_logical - FRAME_MARGIN_W_LOGICAL).max(320.0);
    let max_inner_h = (work_h_logical - FRAME_MARGIN_H_LOGICAL).max(240.0);

    let (desired_w, desired_h) =
        current_logical_size.unwrap_or((PREFERRED_LOGICAL_WIDTH, PREFERRED_LOGICAL_HEIGHT));

    let target_w = desired_w.min(max_inner_w);
    let target_h = desired_h.min(max_inner_h);
    let was_clamped = target_w + 0.5 < desired_w || target_h + 0.5 < desired_h;

    let min_w = BASE_MIN_LOGICAL_WIDTH.min(max_inner_w);
    let min_h = BASE_MIN_LOGICAL_HEIGHT.min(max_inner_h);

    let outer_w_phys = ((target_w + FRAME_MARGIN_W_LOGICAL) * scale).round() as i32;
    let outer_h_phys = ((target_h + FRAME_MARGIN_H_LOGICAL) * scale).round() as i32;
    let outer_x_phys = work.x + ((work.width as i32 - outer_w_phys) / 2).max(0);
    let outer_y_phys = work.y + ((work.height as i32 - outer_h_phys) / 2).max(0);

    FittedGeometry {
        inner_width_logical: target_w as f32,
        inner_height_logical: target_h as f32,
        min_width_logical: min_w as f32,
        min_height_logical: min_h as f32,
        outer_x_phys,
        outer_y_phys,
        was_clamped,
    }
}

#[cfg(windows)]
fn monitor_work_area_phys(
    monitor: &winit::monitor::MonitorHandle,
    scale_factor: f64,
) -> WorkAreaPhys {
    let pos = monitor.position();
    let size = monitor.size();
    let center_x = pos.x + (size.width as i32) / 2;
    let center_y = pos.y + (size.height as i32) / 2;
    query_windows_work_area(center_x, center_y).unwrap_or(WorkAreaPhys {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size
            .height
            .saturating_sub((48.0 * scale_factor).round() as u32),
    })
}

#[cfg(windows)]
fn query_windows_work_area(center_x: i32, center_y: i32) -> Option<WorkAreaPhys> {
    #[repr(C)]
    #[derive(Copy, Clone)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[repr(C)]
    struct MonitorInfo {
        cb_size: u32,
        rc_monitor: Rect,
        rc_work: Rect,
        dw_flags: u32,
    }
    extern "system" {
        fn MonitorFromPoint(pt: Point, dw_flags: u32) -> *mut std::ffi::c_void;
        fn GetMonitorInfoW(h_monitor: *mut std::ffi::c_void, lpmi: *mut MonitorInfo) -> i32;
    }
    const MONITOR_DEFAULTTONEAREST: u32 = 2;

    unsafe {
        let hmon = MonitorFromPoint(
            Point {
                x: center_x,
                y: center_y,
            },
            MONITOR_DEFAULTTONEAREST,
        );
        if hmon.is_null() {
            return None;
        }
        let mut info = MonitorInfo {
            cb_size: std::mem::size_of::<MonitorInfo>() as u32,
            rc_monitor: Rect::default(),
            rc_work: Rect::default(),
            dw_flags: 0,
        };
        if GetMonitorInfoW(hmon, &mut info) == 0 {
            return None;
        }
        Some(WorkAreaPhys {
            x: info.rc_work.left,
            y: info.rc_work.top,
            width: (info.rc_work.right - info.rc_work.left).max(1) as u32,
            height: (info.rc_work.bottom - info.rc_work.top).max(1) as u32,
        })
    }
}

#[cfg(not(windows))]
fn monitor_work_area_phys(
    monitor: &winit::monitor::MonitorHandle,
    scale_factor: f64,
) -> WorkAreaPhys {
    let pos = monitor.position();
    let size = monitor.size();
    let panel_margin = (NON_WINDOWS_PANEL_H_LOGICAL * scale_factor).round() as u32;
    WorkAreaPhys {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height.saturating_sub(panel_margin).max(1),
    }
}

fn sync_responsive_flags(ui: &MainWindow, logical_w: f64, logical_h: f64) {
    let compact_toolbar = logical_w < 1160.0;
    let compact_window = logical_w < 820.0 || logical_h < 560.0;
    if ui.get_compact_toolbar() != compact_toolbar {
        ui.set_compact_toolbar(compact_toolbar);
    }
    if ui.get_compact_window() != compact_window {
        ui.set_compact_window(compact_window);
    }
}

fn clamp_to_current_monitor(ui: &MainWindow, initial_fit: bool) {
    let window = ui.window();
    let fitted = window.with_winit_window(|winit_win| {
        let scale = winit_win.scale_factor();
        let phys = winit_win.inner_size();
        let cur_w = phys.width as f64 / scale;
        let cur_h = phys.height as f64 / scale;

        let Some(monitor) = winit_win
            .current_monitor()
            .or_else(|| winit_win.primary_monitor())
            .or_else(|| winit_win.available_monitors().next())
        else {
            return (cur_w, cur_h);
        };

        let work = monitor_work_area_phys(&monitor, scale);
        let current = if initial_fit {
            None
        } else {
            Some((cur_w, cur_h))
        };

        let geom = compute_fitted_geometry(work, scale, current);
        winit_win.set_min_inner_size(Some(winit::dpi::LogicalSize::new(
            geom.min_width_logical as f64,
            geom.min_height_logical as f64,
        )));

        if initial_fit && geom.was_clamped {
            let _ = winit_win.request_inner_size(winit::dpi::LogicalSize::new(
                geom.inner_width_logical as f64,
                geom.inner_height_logical as f64,
            ));
            winit_win.set_outer_position(winit::dpi::PhysicalPosition::new(
                geom.outer_x_phys,
                geom.outer_y_phys,
            ));
            (
                geom.inner_width_logical as f64,
                geom.inner_height_logical as f64,
            )
        } else if !initial_fit && geom.was_clamped {
            let _ = winit_win.request_inner_size(winit::dpi::LogicalSize::new(
                geom.inner_width_logical as f64,
                geom.inner_height_logical as f64,
            ));
            (
                geom.inner_width_logical as f64,
                geom.inner_height_logical as f64,
            )
        } else if initial_fit {
            (PREFERRED_LOGICAL_WIDTH, PREFERRED_LOGICAL_HEIGHT)
        } else {
            (cur_w, cur_h)
        }
    });

    if let Some((w, h)) = fitted {
        sync_responsive_flags(ui, w, h);
    }
}

/// 显示主窗口、按当前显示器可用工作区自适应窗口大小，并监听跨屏 DPI 切换与窗口缩放事件。
pub fn show_and_fit(ui: &MainWindow) -> Result<(), slint::PlatformError> {
    ui.show()?;
    clamp_to_current_monitor(ui, true);

    let ui_weak = ui.as_weak();
    ui.window().on_winit_window_event(move |win, event| {
        match event {
            winit::event::WindowEvent::ScaleFactorChanged { .. } => {
                let ui_weak = ui_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        clamp_to_current_monitor(&ui, false);
                    }
                });
            }
            winit::event::WindowEvent::Resized(phys) => {
                let scale = win.scale_factor() as f64;
                if scale > 0.0 {
                    let w = phys.width as f64 / scale;
                    let h = phys.height as f64 / scale;
                    let ui_weak = ui_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_weak.upgrade() {
                            sync_responsive_flags(&ui, w, h);
                        }
                    });
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_1080p_at_100_percent_without_clamping() {
        let work = WorkAreaPhys {
            x: 0,
            y: 0,
            width: 1920,
            height: 1032,
        };
        let g = compute_fitted_geometry(work, 1.0, None);
        assert!(!g.was_clamped);
        assert_eq!(g.inner_width_logical, 1320.0);
        assert_eq!(g.inner_height_logical, 860.0);
        assert_eq!(g.min_width_logical, 640.0);
        assert_eq!(g.min_height_logical, 440.0);
    }

    #[test]
    fn clamps_1080p_at_150_percent_scaling() {
        // 1920x1080 @ 150% -> 1280x720 逻辑像素，扣除任务栏后约 1280x672 逻辑像素（1920x1008 物理像素）
        let work = WorkAreaPhys {
            x: 0,
            y: 0,
            width: 1920,
            height: 1008,
        };
        let g = compute_fitted_geometry(work, 1.5, None);
        assert!(g.was_clamped);
        assert_eq!(g.inner_width_logical, 1264.0);
        assert_eq!(g.inner_height_logical, 628.0);
        assert_eq!(g.min_width_logical, 640.0);
        assert_eq!(g.min_height_logical, 440.0);
    }

    #[test]
    fn clamps_1080p_at_200_percent_scaling_and_min_size() {
        // 1920x1080 @ 200% -> 960x540 逻辑像素，扣除任务栏后约 960x492 逻辑像素（1920x984 物理像素）
        let work = WorkAreaPhys {
            x: 0,
            y: 0,
            width: 1920,
            height: 984,
        };
        let g = compute_fitted_geometry(work, 2.0, None);
        assert!(g.was_clamped);
        assert_eq!(g.inner_width_logical, 944.0);
        assert_eq!(g.inner_height_logical, 448.0);
        assert_eq!(g.min_width_logical, 640.0);
        assert_eq!(g.min_height_logical, 440.0);
    }

    #[test]
    fn clamps_1366x768_laptop_screen() {
        let work = WorkAreaPhys {
            x: 0,
            y: 0,
            width: 1366,
            height: 720,
        };
        let g = compute_fitted_geometry(work, 1.0, None);
        assert!(g.was_clamped);
        assert_eq!(g.inner_width_logical, 1320.0);
        assert_eq!(g.inner_height_logical, 676.0);
    }

    #[test]
    fn clamps_portrait_monitor_width() {
        let work = WorkAreaPhys {
            x: 1920,
            y: 0,
            width: 1080,
            height: 1872,
        };
        let g = compute_fitted_geometry(work, 1.0, None);
        assert!(g.was_clamped);
        assert_eq!(g.inner_width_logical, 1064.0);
        assert_eq!(g.inner_height_logical, 860.0);
        assert!(g.outer_x_phys >= 1920);
    }
}

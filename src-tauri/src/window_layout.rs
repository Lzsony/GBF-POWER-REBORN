use tauri::{LogicalSize, PhysicalPosition, WebviewWindow};

const WIDTH: f64 = 400.0;
const HEIGHT: f64 = 520.0;
const MIN_WIDTH: f64 = WIDTH;
const MIN_HEIGHT: f64 = HEIGHT;

fn sizes(available: (f64, f64)) -> (LogicalSize<f64>, LogicalSize<f64>) {
    let (width, height) = (available.0.max(1.0), available.1.max(1.0));
    (
        LogicalSize::new(WIDTH.min(width), HEIGHT.min(height)),
        LogicalSize::new(MIN_WIDTH.min(width), MIN_HEIGHT.min(height)),
    )
}

fn fitted_sizes(
    work: (u32, u32),
    chrome: (f64, f64),
    scale: f64,
) -> (LogicalSize<f64>, LogicalSize<f64>) {
    sizes((
        (f64::from(work.0) - chrome.0) / scale - 16.0,
        (f64::from(work.1) - chrome.1) / scale - 16.0,
    ))
}

fn requested_size(
    size: LogicalSize<f64>,
    actual_chrome: (f64, f64),
    reported_chrome: (f64, f64),
    scale: f64,
) -> LogicalSize<f64> {
    LogicalSize::new(
        size.width + (actual_chrome.0 - reported_chrome.0).max(0.0) / scale,
        size.height + (actual_chrome.1 - reported_chrome.1).max(0.0) / scale,
    )
}

/// Fit before showing the window. Account for native chrome, scale and the monitor work area.
pub fn fit_initial(window: &WebviewWindow) -> tauri::Result<()> {
    let monitor = match window.current_monitor()? {
        Some(monitor) => Some(monitor),
        None => window.primary_monitor()?,
    };
    let Some(monitor) = monitor else {
        return Ok(());
    };
    let work = monitor.work_area();
    let scale = monitor.scale_factor();
    let inner = window.inner_size()?;
    let outer = window.outer_size()?;
    let reported_chrome = (
        f64::from(outer.width.saturating_sub(inner.width)),
        f64::from(outer.height.saturating_sub(inner.height)),
    );
    #[cfg(target_os = "macos")]
    let actual_chrome = {
        // Full-size content views can make Tao report inner == outer while
        // WKWebView still excludes the native titlebar. Measure the real layout.
        let native = unsafe { &*(window.ns_window()? as *const objc2_app_kit::NSWindow) };
        let frame = native.frame();
        let content = native.contentLayoutRect();
        (
            ((frame.size.width - content.size.width) * scale).max(reported_chrome.0),
            ((frame.size.height - content.size.height) * scale).max(reported_chrome.1),
        )
    };
    #[cfg(not(target_os = "macos"))]
    let actual_chrome = reported_chrome;
    let (chrome_x, chrome_y) = actual_chrome;
    let (size, minimum) = fitted_sizes(
        (work.size.width, work.size.height),
        (chrome_x, chrome_y),
        scale,
    );
    let requested = requested_size(size, actual_chrome, reported_chrome, scale);
    window.set_min_size(Some(requested_size(
        minimum,
        actual_chrome,
        reported_chrome,
        scale,
    )))?;
    window.set_size(requested)?;
    window.set_max_size(Some(requested))?;
    window.set_resizable(false)?;
    window.set_maximizable(false)?;
    let x = f64::from(work.position.x)
        + (f64::from(work.size.width) - size.width * scale - chrome_x) / 2.0;
    let y = f64::from(work.position.y)
        + (f64::from(work.size.height) - size.height * scale - chrome_y) / 2.0;
    window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_content_titlebar_is_compensated_without_double_counting() {
        let size = LogicalSize::new(400.0, 520.0);
        assert_eq!(
            requested_size(size, (0.0, 64.0), (0.0, 0.0), 2.0).height,
            552.0
        );
        assert_eq!(
            requested_size(size, (0.0, 64.0), (0.0, 64.0), 2.0).height,
            520.0
        );
    }
    #[test]
    fn large_workspace_keeps_portrait_defaults() {
        let (size, minimum) = sizes((1400.0, 900.0));
        assert_eq!((size.width, size.height), (400.0, 520.0));
        assert_eq!((minimum.width, minimum.height), (400.0, 520.0));
    }
    #[test]
    fn small_workspace_overrides_minimum_instead_of_overflowing() {
        let (size, minimum) = sizes((360.0, 380.0));
        assert_eq!((size.width, size.height), (360.0, 380.0));
        assert_eq!((minimum.width, minimum.height), (360.0, 380.0));
    }
    #[test]
    fn simulated_windows_dpi_preserves_work_area_and_native_borders() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for work in [(1920, 1040), (800, 560), (640, 400)] {
                let chrome = (16.0 * scale, 39.0 * scale);
                let (size, minimum) = fitted_sizes(work, chrome, scale);
                assert!(size.width * scale + chrome.0 + 16.0 * scale <= f64::from(work.0) + 0.01);
                assert!(size.height * scale + chrome.1 + 16.0 * scale <= f64::from(work.1) + 0.01);
                assert!(minimum.width <= size.width && minimum.height <= size.height);
            }
        }
    }
}

//! Windows 窗口材质层（液态玻璃底座）。
//!
//! 优先级：
//! 1. Windows 11 22H2+ —— Mica（DWMWA_SYSTEMBACKDROP_TYPE = MainWindow）；
//! 2. Windows 10 / 11 早期 —— Acrylic（SetWindowCompositionAttribute，奶白磨砂）；
//! 3. 更老系统 —— 无材质，UI 自动使用不透明白底降级（见 liquid.rs）。
//!
//! eframe 0.36 没有窗口句柄回调，这里用独立线程按窗口标题 FindWindowW，
//! 等窗口可见后应用材质；全程纯 FFI（dwmapi/user32 为系统库，零第三方依赖）。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::Duration;

/// 窗口标题（main 设窗口、glass 找句柄共用，必须一致）。
pub const WINDOW_TITLE: &str = "星露谷物语模组管理器";

/// 系统材质是否已成功应用（每帧 UI 查询，决定用半透明玻璃还是纯白降级底）。
static MATERIAL_OK: AtomicBool = AtomicBool::new(false);

pub fn material_active() -> bool {
    MATERIAL_OK.load(Ordering::Relaxed)
}

/// 后台等待窗口出现并应用材质（不阻塞启动）。
pub fn init(window_title: &'static str) {
    std::thread::spawn(move || {
        let Ok(hwnd) = wait_for_window(window_title, Duration::from_secs(20)) else {
            log("window not found within timeout");
            return;
        };
        log(&format!(
            "hwnd found: {:p}, os build {}",
            hwnd,
            os_build()
        ));
        // 尽早移除系统标题栏按钮（Mica 扩展帧后 DWM 会重新画出来）。
        install_borderless_subclass(hwnd);
        // 关键：等交换链/合成器就绪再贴材质。窗口刚创建的前几百毫秒内贴
        // Mica 会偶发 latch 成纯黑背景（后续帧无法自愈）。
        std::thread::sleep(Duration::from_millis(450));
        for i in 0..10 {
            if unsafe { apply_material(hwnd) } {
                MATERIAL_OK.store(true, Ordering::Relaxed);
                log("material applied OK");
                return;
            }
            log(&format!("apply attempt {i} failed"));
            std::thread::sleep(Duration::from_millis(300));
        }
        log("all material attempts failed -> opaque fallback");
    });
}

/// UI 线程每帧调用：① 启动后前几秒多次重贴材质（消除竞态黑窗）；
/// ② 窗口从失焦重新获得焦点时强制恢复（DWM 合成器在某些场景会丢材质）。
///
/// `focused` 为当前窗口焦点状态。
pub fn on_frame(title: &'static str, focused: bool, uptime_secs: f64) {
    use std::sync::atomic::AtomicU32;
    static REAPPLY_COUNT: AtomicU32 = AtomicU32::new(0);
    static NEXT_REAPPLY_MS: AtomicU32 = AtomicU32::new(500);
    static WAS_FOCUSED: AtomicBool = AtomicBool::new(true);

    let now_ms = (uptime_secs * 1000.0) as u32;

    // 启动后 0.8 / 1.6 / 3.0 秒在 UI 线程重贴（幂等，便宜）。
    if REAPPLY_COUNT.load(Ordering::Relaxed) < 3
        && now_ms >= NEXT_REAPPLY_MS.load(Ordering::Relaxed)
    {
        if apply_on_ui_thread(title) {
            let n = REAPPLY_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            NEXT_REAPPLY_MS.store(now_ms + 800, Ordering::Relaxed);
            log(&format!("scheduled reapply #{n}"));
        }
    }

    // 失焦 → 聚焦边沿：toggle 恢复一次（启动 4 秒后的焦点切换才算数，
    // 避开窗口创建瞬间的 false→true 抖动）。
    let was = WAS_FOCUSED.load(Ordering::Relaxed);
    if focused && !was && uptime_secs > 4.0 {
        log("focus regained -> material recover");
        let _ = recover_on_ui_thread(title);
    }
    WAS_FOCUSED.store(focused, Ordering::Relaxed);
}

/// 先撤销背景类型再重新设置 Mica，强制 DWM 重建背景（治黑窗）。
pub fn recover_on_ui_thread(title: &str) -> bool {
    let needle = wide(title);
    let hwnd = unsafe { FindWindowW(core::ptr::null(), needle.as_ptr()) };
    if hwnd.is_null() {
        return false;
    }
    install_borderless_subclass(hwnd);
    unsafe {
        let off = 0i32; // DWMSBT_AUTO
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &off as *const i32 as _,
            4,
        );
        let m0 = Margins {
            left: 0,
            right: 0,
            top: 0,
            bottom: 0,
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m0);
        let ok = apply_material(hwnd);
        MATERIAL_OK.store(ok, Ordering::Relaxed);
        ok
    }
}

// ---------- FFI ----------

type HWND = *mut core::ffi::c_void;

#[repr(C)]
struct Margins {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

// Win32 DWM / user32 均为系统自带 DLL，windows-gnu 工具链自带其导入库。
unsafe extern "system" {
    fn DwmSetWindowAttribute(
        hwnd: HWND,
        attr: u32,
        data: *const core::ffi::c_void,
        size: u32,
    ) -> i32;
    fn DwmExtendFrameIntoClientArea(hwnd: HWND, margins: *const Margins) -> i32;
    fn FindWindowW(class: *const u16, window: *const u16) -> HWND;
    fn IsWindowVisible(hwnd: HWND) -> i32;
    fn SetWindowCompositionAttribute(hwnd: HWND, data: *mut WcaData) -> i32;
    fn GetWindowLongPtrW(hwnd: HWND, index: i32) -> isize;
    fn SetWindowLongPtrW(hwnd: HWND, index: i32, new_proc: isize) -> isize;
    fn CallWindowProcW(
        prev_proc: WndProcFn,
        hwnd: HWND,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize;
    fn GetWindowRect(hwnd: HWND, rect: *mut RectL) -> i32;
    fn IsZoomed(hwnd: HWND) -> i32;
    fn SetWindowPos(
        hwnd: HWND,
        hwnd_after: HWND,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
}

// ---------- 无边框窗口子类化：移除系统标题按钮 ----------
//
// with_decorations(false) + DwmExtendFrameIntoClientArea(-1) 之后，Win11 的
// DWM 仍会在右上角画系统的最小化/最大化/关闭按钮。拦截 WM_NCCALCSIZE 让整个
// 窗口都成为客户区，系统标题栏（含按钮）即不再绘制；Mica 圆角/阴影不受影响。
// WM_NCHITTEST 里补回窗口边缘的缩放命中区。

type WndProcFn =
    unsafe extern "system" fn(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize;

#[repr(C)]
struct RectL {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

const GWLP_WNDPROC: i32 = -4;
const GWL_STYLE: i32 = -16;
const WM_NCCALCSIZE: u32 = 0x0083;
const WM_NCHITTEST: u32 = 0x0084;
const WM_ERASEBKGND: u32 = 0x0014;

// 窗口样式：去掉标题栏与系统菜单，DWM 就不会再画系统按钮。
const WS_CAPTION: isize = 0x00C0_0000;
const WS_SYSMENU: isize = 0x0008_0000;
const WS_MINIMIZEBOX: isize = 0x0002_0000;
const WS_MAXIMIZEBOX: isize = 0x0001_0000;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_FRAMECHANGED: u32 = 0x0020;

const HTCLIENT: isize = 1;
const HTLEFT: isize = 10;
const HTRIGHT: isize = 11;
const HTTOP: isize = 12;
const HTTOPLEFT: isize = 13;
const HTTOPRIGHT: isize = 14;
const HTBOTTOM: isize = 15;
const HTBOTTOMLEFT: isize = 16;
const HTBOTTOMRIGHT: isize = 17;

/// 边缘缩放命中带宽度（物理像素）。
const RESIZE_BORDER: i32 = 6;

static OLD_WNDPROC: AtomicIsize = AtomicIsize::new(0);

/// 安装窗口子类（幂等；多线程并发调用也只会安装一次）。
fn install_borderless_subclass(hwnd: HWND) {
    if OLD_WNDPROC.load(Ordering::Acquire) != 0 {
        return;
    }
    // SAFETY：hwnd 由 FindWindowW 取得且窗口存活；替换后所有消息仍转发给旧过程。
    unsafe {
        let old = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, borderless_wndproc as isize);
        if old == 0 {
            return;
        }
        // CAS 防后台线程与 UI 线程同时安装：落选者把旧过程还原回去。
        if OLD_WNDPROC
            .compare_exchange(0, old, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, old);
        } else {
            // 去掉标题栏/系统菜单/最小化最大化框样式，DWM 就不会再绘制
            // 系统的最小化/最大化/关闭按钮。保留 WS_THICKFRAME 以便拖边缩放。
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            let stripped = style & !(WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX);
            if stripped != style {
                SetWindowLongPtrW(hwnd, GWL_STYLE, stripped);
                // 通知系统框架已改变，重绘非客户区。
                SetWindowPos(
                    hwnd,
                    core::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            }
            log("borderless subclass installed (system caption removed)");
        }
    }
}

unsafe extern "system" fn borderless_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    if msg == WM_NCCALCSIZE && wparam == 1 {
        // 保持建议矩形不动并返回 0：客户区占满整个窗口，
        // 系统标题栏与最小化/最大化/关闭按钮全部移除。
        return 0;
    }
    if msg == WM_ERASEBKGND {
        // 不擦除背景：透明窗口的底色由 DWM 材质(Mica)填充，
        // 系统擦除会造成缩放/拖拽时的黑色闪烁。
        return 1;
    }

    let old = OLD_WNDPROC.load(Ordering::Acquire);
    if old == 0 {
        return 0;
    }
    let old_proc: WndProcFn = unsafe { core::mem::transmute(old) };

    if msg == WM_NCHITTEST {
        let hit = unsafe { CallWindowProcW(old_proc, hwnd, msg, wparam, lparam) };
        // winit 把无边框窗口的客户区命中统一交给应用；在窗口最外圈
        // 补一条缩放带，保留拖边调整大小（最大化时不处理，交给系统）。
        if hit == HTCLIENT && unsafe { IsZoomed(hwnd) } == 0 {
            let mut rc = RectL {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if unsafe { GetWindowRect(hwnd, &mut rc) } != 0 {
                // GET_X_LPARAM / GET_Y_LPARAM：低/高 16 位有符号扩展。
                let x = (lparam as i16) as i32;
                let y = ((lparam >> 16) as i16) as i32;
                let left = x < rc.left + RESIZE_BORDER;
                let right = x >= rc.right - RESIZE_BORDER;
                let top = y < rc.top + RESIZE_BORDER;
                let bottom = y >= rc.bottom - RESIZE_BORDER;
                return match (left, right, top, bottom) {
                    (true, false, true, false) => HTTOPLEFT,
                    (true, false, false, true) => HTBOTTOMLEFT,
                    (false, true, true, false) => HTTOPRIGHT,
                    (false, true, false, true) => HTBOTTOMRIGHT,
                    (true, false, _, _) => HTLEFT,
                    (false, true, _, _) => HTRIGHT,
                    (_, _, true, false) => HTTOP,
                    (_, _, false, true) => HTBOTTOM,
                    _ => hit,
                };
            }
        }
        return hit;
    }

    unsafe { CallWindowProcW(old_proc, hwnd, msg, wparam, lparam) }
}

const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;

const DWMWCP_ROUND: i32 = 2;
const DWMSBT_MAINWINDOW: i32 = 2; // Mica

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;
const ACCENT_ENABLE_HOSTBACKDROP: u32 = 5; // Win11 合成器背景（近 Mica）

fn log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::temp_dir().join("stardew_mod_manager_glass.log"))
    {
        let _ = writeln!(f, "{msg}");
    }
}

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    /// 0xAABBGGRR：奶白半透明（Acrylic 着色）。
    color: u32,
    animation: u32,
}

#[repr(C)]
struct WcaData {
    attr: u32,
    data: *mut core::ffi::c_void,
    size: usize,
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(core::iter::once(0))
        .collect()
}

fn wait_for_window(title: &str, timeout: Duration) -> Result<HWND, ()> {
    let needle = wide(title);
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        let hwnd = unsafe { FindWindowW(core::ptr::null(), needle.as_ptr()) };
        if !hwnd.is_null() && unsafe { IsWindowVisible(hwnd) } != 0 {
            // 再多等两帧，确保交换链已就位。
            std::thread::sleep(Duration::from_millis(120));
            return Ok(hwnd);
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    Err(())
}

/// 依次尝试 Mica → Acrylic。
///
/// # Safety
/// 仅调用系统 API，传入的句柄由 FindWindowW 取得且窗口存活。
unsafe fn apply_material(hwnd: HWND) -> bool {
    // 浅色外观（Mica/标题按钮遵循亮色系）。
    let light: i32 = 0;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &light as *const i32 as _,
        4,
    );
    // Win11 圆角（无边框窗口也强制圆角）。
    let corner = DWMWCP_ROUND;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        &corner as *const i32 as _,
        4,
    );

    // 模式可由环境变量 FIRE_GLASS_MODE 切换（mica / mica-noext / host / acrylic），
    // 默认 mica。用于诊断不同合成路径。
    let mode = std::env::var("FIRE_GLASS_MODE").unwrap_or_else(|_| "mica".to_string());
    log(&format!("mode={mode}"));

    let extend = |m: i32| {
        let margins = Margins {
            left: m,
            right: m,
            top: m,
            bottom: m,
        };
        let hr = DwmExtendFrameIntoClientArea(hwnd, &margins);
        log(&format!("extend({m}) hr=0x{:08x}", hr as u32));
    };

    match mode.as_str() {
        "mica-noext" => {
            // 不扩展帧，只设背景类型。
            set_mica(hwnd)
        }
        "host" => {
            extend(-1);
            apply_accent(hwnd, ACCENT_ENABLE_HOSTBACKDROP, 0)
        }
        "acrylic" => {
            extend(-1);
            let ok = apply_accent(hwnd, ACCENT_ENABLE_ACRYLICBLURBEHIND, 0x96_FF_FF_FF);
            log(&format!("acrylic ok={ok}"));
            ok
        }
        _ => {
            extend(-1);
            // 1) Mica（Win11 22H2+，旧系统返回 E_INVALIDARG）。
            if set_mica(hwnd) {
                return true;
            }
            // 2) Win11 HostBackdrop。
            if apply_accent(hwnd, ACCENT_ENABLE_HOSTBACKDROP, 0) {
                log("host backdrop ok");
                return true;
            }
            // 3) Acrylic（Win10 1803+ / 其余兜底），奶白着色。
            let ok = apply_accent(hwnd, ACCENT_ENABLE_ACRYLICBLURBEHIND, 0x96_FF_FF_FF);
            log(&format!("acrylic ok={ok}"));
            ok
        }
    }
}

/// 设置 Mica 背景类型并报告结果。
///
/// # Safety
/// 系统 API 调用，hwnd 有效。
unsafe fn set_mica(hwnd: HWND) -> bool {
    let backdrop = DWMSBT_MAINWINDOW;
    let hr = DwmSetWindowAttribute(
        hwnd,
        DWMWA_SYSTEMBACKDROP_TYPE,
        &backdrop as *const i32 as _,
        4,
    );
    log(&format!("mica hr=0x{:08x}", hr as u32));
    hr >= 0
}

/// 在 UI 线程上尝试一次材质应用（跨线程设置 DWM 背景在部分窗口样式上无效）。
/// 由 App 每帧节流调用；成功或仍找不到窗口均不阻塞。
pub fn apply_on_ui_thread(title: &str) -> bool {
    let needle = wide(title);
    let hwnd = unsafe { FindWindowW(core::ptr::null(), needle.as_ptr()) };
    if hwnd.is_null() || unsafe { IsWindowVisible(hwnd) } == 0 {
        return false;
    }
    install_borderless_subclass(hwnd);
    if unsafe { apply_material(hwnd) } {
        MATERIAL_OK.store(true, Ordering::Relaxed);
        log("material applied on UI thread");
        true
    } else {
        false
    }
}

/// 通过 SetWindowCompositionAttribute 应用指定 AccentState。
///
/// # Safety
/// 调用系统 API，句柄有效。
unsafe fn apply_accent(hwnd: HWND, state: u32, color: u32) -> bool {
    let mut policy = AccentPolicy {
        state,
        flags: 2,
        color,
        animation: 0,
    };
    let mut data = WcaData {
        attr: WCA_ACCENT_POLICY,
        data: &mut policy as *mut AccentPolicy as _,
        size: size_of::<AccentPolicy>(),
    };
    SetWindowCompositionAttribute(hwnd, &mut data) != 0
}

/// 读取系统内部版本号（用于诊断；失败返回 0）。
fn os_build() -> u32 {
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }
    #[repr(C)]
    #[allow(non_snake_case)]
    struct OsVersionInfo {
        size: u32,
        Major: u32,
        Minor: u32,
        Build: u32,
        Platform: u32,
        szCSDVersion: [u16; 128],
    }
    let mut info = OsVersionInfo {
        size: size_of::<OsVersionInfo>() as u32,
        Major: 0,
        Minor: 0,
        Build: 0,
        Platform: 0,
        szCSDVersion: [0; 128],
    };
    // SAFETY：结构体大小正确，RtlGetVersion 不做访问校验。
    unsafe {
        if RtlGetVersion(&mut info) == 0 {
            info.Build
        } else {
            0
        }
    }
}

//! 主题与材质助手 —— 白色「液态玻璃」（Apple 风）。
//!
//! 底座由系统 DWM 提供（Win11 Mica / Win10 Acrylic，见 glass.rs），
//! 本模块负责绘制浮在材质上的内容：半透明白色玻璃卡片、发丝描边、
//! 顶部高光棱边、柔和投影、大连续圆角与苹果系语义色。
//!
//! 系统材质不可用时（老系统），所有玻璃材质自动降级为不透明白色，
//! 界面依旧完整可用。

use egui::{Color32, CornerRadius, FontId, Id, Margin, Rect, RichText, Shadow, Stroke, Vec2};

// ---------- 玻璃材质（带 alpha；材质缺失时自动换不透明降级色） ----------

/// 纯白（降级场景使用）。
pub const OPAQUE_WHITE: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// 无材质时的窗口底色（苹果系统灰 #F5F5F7）。
pub const OPAQUE_BG: Color32 = Color32::from_rgb(0xF5, 0xF5, 0xF7);
/// 无材质时侧栏/标题栏的次层底色。
pub const OPAQUE_BG_DEEP: Color32 = Color32::from_rgb(0xEC, 0xEC, 0xF0);

// 兼容旧引用（FLAT_BG 现指向不透明白色降级底色）。
pub const FLAT_BG: Color32 = OPAQUE_BG;

/// rgba 便捷构造（整数手动预乘，因此可用于 const）。
const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(
        ((r as u32 * a as u32) / 255) as u8,
        ((g as u32 * a as u32) / 255) as u8,
        ((b as u32 * a as u32) / 255) as u8,
        a,
    )
}

/// 窗口最底层：材质开启时全透（露出 Mica/Acrylic），否则白灰底。
pub fn window_fill() -> Color32 {
    if crate::glass::material_active() {
        Color32::TRANSPARENT
    } else {
        OPAQUE_BG
    }
}

/// 玻璃卡片填充。
pub fn glass_card() -> Color32 {
    if crate::glass::material_active() {
        rgba(255, 255, 255, 212)
    } else {
        OPAQUE_WHITE
    }
}

/// 侧栏玻璃条：比卡片更透一点的白色振动层。
pub fn glass_sidebar() -> Color32 {
    if crate::glass::material_active() {
        rgba(255, 255, 255, 64)
    } else {
        OPAQUE_BG_DEEP
    }
}

/// 标题栏：极淡白雾面。
pub fn glass_titlebar() -> Color32 {
    if crate::glass::material_active() {
        rgba(255, 255, 255, 36)
    } else {
        OPAQUE_BG_DEEP
    }
}

/// 状态栏：半透明白。
pub fn glass_statusbar() -> Color32 {
    if crate::glass::material_active() {
        rgba(255, 255, 255, 168)
    } else {
        OPAQUE_WHITE
    }
}

/// 输入框/文本框底色。
pub fn input_fill() -> Color32 {
    if crate::glass::material_active() {
        rgba(255, 255, 255, 200)
    } else {
        rgba(0, 0, 0, 12)
    }
}

/// 发丝描边（黑 7%）。
pub const HAIRLINE: Color32 = rgba(0, 0, 0, 18);
/// 更淡的分隔线（黑 5%）。
pub const HAIRLINE_SOFT: Color32 = rgba(0, 0, 0, 12);
/// 悬停描边（黑 12%）。
pub const HAIRLINE_STRONG: Color32 = rgba(0, 0, 0, 32);
/// 玻璃顶部高光棱边（白 70%）。
pub const GLASS_EDGE: Color32 = rgba(255, 255, 255, 178);
/// 柔和投影色。
pub const SHADOW_TINT: Color32 = rgba(0, 0, 0, 26);

// ---------- 苹果系语义色板（浅色） ----------

pub const PRIMARY: Color32 = Color32::from_rgb(0x00, 0x7A, 0xFF); // 系统蓝
pub const PRIMARY_SOFT: Color32 = rgba(0x00, 0x7A, 0xFF, 32);

pub const SUCCESS: Color32 = Color32::from_rgb(0x1F, 0x8A, 0x4C); // 可读的深绿
pub const SUCCESS_SOFT: Color32 = rgba(52, 199, 89, 42);

pub const WARNING: Color32 = Color32::from_rgb(0xB2, 0x50, 0x00);
pub const WARNING_SOFT: Color32 = rgba(255, 149, 0, 44);

pub const DANGER: Color32 = Color32::from_rgb(0xD7, 0x00, 0x15); // 系统红
pub const DANGER_SOFT: Color32 = rgba(255, 59, 48, 38);

pub const VIOLET: Color32 = Color32::from_rgb(0x89, 0x44, 0xAB);
pub const VIOLET_SOFT: Color32 = rgba(175, 82, 222, 40);

pub const TEAL: Color32 = Color32::from_rgb(0x0B, 0x76, 0x89);
pub const TEAL_SOFT: Color32 = rgba(48, 176, 199, 40);

pub const SKY: Color32 = Color32::from_rgb(0x0B, 0x6F, 0x96);
pub const SKY_SOFT: Color32 = rgba(24, 168, 216, 38);

pub const SLATE: Color32 = Color32::from_rgb(0x6E, 0x6E, 0x73); // 系统灰
pub const SLATE_SOFT: Color32 = rgba(0, 0, 0, 24);

// ---------- 文字 ----------

const TEXT_MAIN: Color32 = Color32::from_rgb(0x1D, 0x1D, 0x1F);
const TEXT_DIM: Color32 = Color32::from_rgb(0x6E, 0x6E, 0x73);
const ACCENT_TEXT: Color32 = Color32::from_rgb(0x00, 0x58, 0xD1);

pub fn accent() -> Color32 {
    PRIMARY
}
pub fn accent_text() -> Color32 {
    ACCENT_TEXT
}
pub fn text_main() -> Color32 {
    TEXT_MAIN
}
pub fn text_dim() -> Color32 {
    TEXT_DIM
}

/// 线性混合两色（t=0 → a，t=1 → b）。
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// 替换颜色的 alpha 分量。
pub fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

// ---------- 标题栏窗口控制按钮 ----------

pub fn title_btn_hover() -> Color32 {
    rgba(0, 0, 0, 22)
}
pub fn title_btn_active() -> Color32 {
    rgba(0, 0, 0, 38)
}
pub fn title_btn_close_hover() -> Color32 {
    Color32::from_rgb(0xFF, 0x3B, 0x30)
}
pub fn title_btn_close_active() -> Color32 {
    DANGER
}
pub fn title_btn_close_text() -> Color32 {
    Color32::WHITE
}

/// 安装全局样式。
pub fn install(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    let mut v = egui::Visuals::light();

    v.override_text_color = Some(TEXT_MAIN);
    v.hyperlink_color = ACCENT_TEXT;
    v.selection.bg_fill = PRIMARY;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.faint_bg_color = rgba(0, 0, 0, 8);
    v.extreme_bg_color = input_fill();
    v.panel_fill = Color32::TRANSPARENT; // 窗口底板交给系统材质
    v.window_fill = glass_card();
    v.window_stroke = Stroke::new(1.0, HAIRLINE);
    v.window_corner_radius = CornerRadius::same(16);
    v.window_shadow = Shadow {
        offset: [0, 4],
        blur: 24,
        spread: 0,
        color: rgba(0, 0, 0, 30),
    };
    v.popup_shadow = v.window_shadow;
    v.widgets.noninteractive.bg_fill = Color32::TRANSPARENT;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE_SOFT);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_MAIN);

    // 普通控件：白色玻璃片 + 发丝边 + 10px 连续圆角。
    v.widgets.inactive.weak_bg_fill = rgba(255, 255, 255, 132);
    // 滑块导轨用的就是 inactive.bg_fill（半透明白），在白色玻璃卡片上等于看不见，
    // 单独给它一点灰让导轨形状可见；按钮用的是 weak_bg_fill，不受影响。
    v.widgets.inactive.bg_fill = rgba(0, 0, 0, 26);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    // 已滑过的部分用主色填充，一眼能看出当前取值。
    v.slider_trailing_fill = true;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_MAIN);
    v.widgets.inactive.corner_radius = CornerRadius::same(10);
    v.widgets.inactive.expansion = 0.0;

    v.widgets.hovered.weak_bg_fill = rgba(255, 255, 255, 200);
    v.widgets.hovered.bg_fill = rgba(255, 255, 255, 200);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, HAIRLINE_STRONG);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_MAIN);
    v.widgets.hovered.corner_radius = CornerRadius::same(10);
    v.widgets.hovered.expansion = 0.0;

    v.widgets.active.weak_bg_fill = PRIMARY_SOFT;
    v.widgets.active.bg_fill = PRIMARY_SOFT;
    v.widgets.active.bg_stroke = Stroke::new(1.0, with_alpha(PRIMARY, 120));
    v.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT_TEXT);
    v.widgets.active.corner_radius = CornerRadius::same(10);
    v.widgets.active.expansion = 0.0;

    // 勾选/开关/滑块等强调件。
    v.widgets.open.weak_bg_fill = rgba(255, 255, 255, 160);
    v.widgets.open.bg_fill = rgba(255, 255, 255, 160);
    v.widgets.open.bg_stroke = Stroke::new(1.0, HAIRLINE);
    v.widgets.open.fg_stroke = Stroke::new(1.0, TEXT_MAIN);
    v.widgets.open.corner_radius = CornerRadius::same(10);

    ctx.set_visuals(v);

    let mut style = (*ctx.style_of(egui::Theme::Light)).clone();
    style.spacing.button_padding = Vec2::new(14.0, 7.0);
    style.spacing.item_spacing = Vec2::new(8.0, 10.0);
    style.spacing.interact_size.y = 30.0;
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(10);
    ctx.set_style_of(egui::Theme::Light, style);
}

// ---------- 面板材质 ----------

/// 中央内容容器：直接浮在系统材质上，无实底；卡片自带玻璃。
pub fn panel_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(window_fill())
        .corner_radius(CornerRadius::same(16))
        .outer_margin(Margin {
            left: 12,
            right: 12,
            top: 10,
            bottom: 10,
        })
        .inner_margin(Margin {
            left: 18,
            right: 18,
            top: 14,
            bottom: 16,
        })
}

/// 侧栏：白色振动玻璃条 + 右侧发丝分割。
pub fn sidebar_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(glass_sidebar())
        .stroke(Stroke::new(1.0, HAIRLINE_SOFT))
        .inner_margin(Margin {
            left: 14,
            right: 12,
            top: 14,
            bottom: 12,
        })
}

/// 顶部标题栏：淡白雾面 + 底部分割线。
pub fn title_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(glass_titlebar())
        .stroke(Stroke::new(1.0, HAIRLINE_SOFT))
        .inner_margin(Margin::symmetric(16, 8))
}

/// 底部状态栏：半透明白 + 顶部分割线。
pub fn status_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(glass_statusbar())
        .stroke(Stroke::new(1.0, HAIRLINE_SOFT))
        .inner_margin(Margin {
            left: 14,
            right: 14,
            top: 8,
            bottom: 8,
        })
}

// ---------- 玻璃卡片 ----------

pub fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    card_accent(ui, None, add_contents)
}

/// 玻璃卡片：半透明白 + 发丝描边 + 顶部高光棱边 + 柔和投影。
pub fn card_accent<R>(
    ui: &mut egui::Ui,
    accent: Option<Color32>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let shadow = if crate::glass::material_active() {
        Shadow {
            offset: [0, 3],
            blur: 18,
            spread: 0,
            color: SHADOW_TINT,
        }
    } else {
        Shadow {
            offset: [0, 1],
            blur: 6,
            spread: 0,
            color: rgba(0, 0, 0, 18),
        }
    };
    let frame = egui::Frame::NONE
        .fill(glass_card())
        .stroke(Stroke::new(1.0, HAIRLINE))
        .corner_radius(CornerRadius::same(16))
        .shadow(shadow)
        .inner_margin(Margin::same(16));
    let resp = frame.show(ui, add_contents);
    let r = resp.response.rect;
    if r.width() > 32.0 && r.height() > 8.0 {
        // 玻璃顶部高光棱边（内嵌 16px，避开圆角）。
        if crate::glass::material_active() {
            let edge = egui::Rect::from_min_max(
                egui::pos2(r.left() + 16.0, r.top() + 0.75),
                egui::pos2(r.right() - 16.0, r.top() + 1.25),
            );
            ui.painter().rect_filled(edge, CornerRadius::same(0), GLASS_EDGE);
        }
        if let Some(c) = accent {
            let bar = egui::Rect::from_min_size(
                egui::pos2(r.left() + 5.0, r.top() + 12.0),
                egui::vec2(4.0, (r.height() - 24.0).max(8.0)),
            );
            ui.painter().rect_filled(bar, CornerRadius::same(2), c);
        }
    }
    resp
}

/// 主操作按钮：系统蓝实心、白字、连续圆角。
pub fn cta_button(ui: &mut egui::Ui, label: impl Into<String>, enabled: bool) -> egui::Response {
    let text = RichText::new(label).color(Color32::WHITE).strong();
    let btn = egui::Button::new(text)
        .fill(PRIMARY)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(10))
        .min_size(Vec2::new(0.0, 32.0));
    if enabled {
        ui.add(btn)
    } else {
        ui.add_enabled(false, btn)
    }
}

// ---------- 语义组件 ----------

/// 区块标题：左侧彩色竖条 + 深色大标题。
pub fn section_title(ui: &mut egui::Ui, text: &str, color: Color32) {
    let height = 24.0_f32;
    let width = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.top() + 3.0),
            egui::vec2(4.0, height - 6.0),
        ),
        CornerRadius::same(2),
        color,
    );
    painter.text(
        egui::pos2(rect.left() + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(17.0),
        TEXT_MAIN,
    );
}

/// 行内区块标题：彩色小竖条 + 彩色加粗文字。
pub fn inline_title(ui: &mut egui::Ui, text: &str, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(4.0, 17.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(2), color);
    ui.add_space(2.0);
    ui.label(RichText::new(text).size(16.0).strong().color(color));
}

/// 彩色胶囊标签。
pub fn pill(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) -> egui::Response {
    let font = egui::FontId::proportional(12.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, fg);
    let size = egui::vec2(galley.size().x + 20.0, 22.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let radius = CornerRadius::same(11);
    ui.painter().rect_filled(rect, radius, bg);
    let text_pos = rect.center() - galley.size() / 2.0;
    ui.painter().galley(text_pos, galley, fg);
    resp
}

/// 统计色块：玻璃卡片 + 左彩条 + 彩色大数字 + 灰标签。
pub fn stat_chip(ui: &mut egui::Ui, label: &str, value: &str, color: Color32, _soft: Color32) {
    let size = egui::vec2(118.0, 56.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let radius = CornerRadius::same(14);
    let painter = ui.painter();
    painter.rect_filled(rect, radius, glass_card());
    painter.rect_stroke(rect, radius, Stroke::new(1.0, HAIRLINE), egui::StrokeKind::Inside);
    painter.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(rect.left() + 12.0, rect.top() + 14.0),
            egui::vec2(4.0, 28.0),
        ),
        CornerRadius::same(2),
        color,
    );
    painter.text(
        egui::pos2(rect.left() + 24.0, rect.top() + 13.0),
        egui::Align2::LEFT_TOP,
        label,
        egui::FontId::proportional(11.0),
        TEXT_DIM,
    );
    painter.text(
        egui::pos2(rect.left() + 24.0, rect.top() + 26.0),
        egui::Align2::LEFT_TOP,
        value,
        egui::FontId::proportional(20.0),
        color,
    );
}

/// 次按钮：浅彩玻璃底 + 同色描边。
pub fn soft_button(ui: &mut egui::Ui, label: &str, color: Color32, soft: Color32) -> egui::Response {
    let btn = egui::Button::new(RichText::new(label).color(color).strong())
        .fill(soft)
        .stroke(Stroke::new(1.0, with_alpha(color, 140)))
        .corner_radius(CornerRadius::same(10))
        .min_size(Vec2::new(0.0, 30.0));
    ui.add(btn)
}

/// 进度条：细圆角轨道 + 彩色填充。
pub fn progress_bar(ui: &mut egui::Ui, frac: f32, color: Color32) {
    let width = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 8.0), egui::Sense::hover());
    let radius = CornerRadius::same(4);
    let painter = ui.painter();
    painter.rect_filled(rect, radius, rgba(0, 0, 0, 22));
    let filled_w = rect.width() * frac.clamp(0.0, 1.0);
    if filled_w > 1.0 {
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(filled_w, rect.height())),
            radius,
            color,
        );
    }
}

/// 状态文字 → 语义色（成功/进行中/失败/中性）。
pub fn status_colors(status: &str) -> (Color32, Color32) {
    if status.starts_with("失败") {
        (DANGER, DANGER_SOFT)
    } else if status.starts_with("已安装") || status.starts_with("完成") {
        (SUCCESS, SUCCESS_SOFT)
    } else if status.starts_with("下载中") || status.starts_with("解析") || status.starts_with("队列") {
        (PRIMARY, PRIMARY_SOFT)
    } else {
        (SLATE, SLATE_SOFT)
    }
}

pub fn status_pill(ui: &mut egui::Ui, status: &str) -> egui::Response {
    let (fg, bg) = status_colors(status);
    pill(ui, status, fg, bg)
}

// ---------- 液态玻璃「滑动选择器」 ----------
//
// 苹果 Liquid Glass 式分段控件：一块高光白玻璃在选项之间平滑滑动，
// 位置与宽度随选项文字长短实时伸缩。分两部分：
//   1. animated_slider_rect —— 指数平滑插值（spring 手感，停得稳）
//   2. paint_glass_slider   —— 白玻璃 + 柔影 + 发丝边 + 顶部高光棱边
// 横向 segmented() 是成品组件；竖向导航在 app 侧调用 nav_slider()。

/// 滑块填充：材质开启时高透白玻璃，否则纯白。
fn slider_fill() -> Color32 {
    if crate::glass::material_active() {
        rgba(255, 255, 255, 230)
    } else {
        Color32::WHITE
    }
}

/// 分段轨道凹槽填充。
fn track_fill() -> Color32 {
    if crate::glass::material_active() {
        rgba(0, 0, 0, 15)
    } else {
        rgba(0, 0, 0, 20)
    }
}

/// 让一块 rect 以指数平滑（spring-like）滑向 target。
/// 首次出现直接落在 target 上（启动不播一遍滑动）。
fn animated_slider_rect(ui: &egui::Ui, id: Id, target: Rect) -> Rect {
    // dt 上限 1/30s：掉帧后恢复也不会出现一大步跳跃，动画节奏始终均匀。
    let dt = ui.input(|i| i.stable_dt.clamp(0.0, 1.0 / 30.0)) as f32;
    let (rect, moving) = ui.ctx().data_mut(|d| {
        if let Some(prev) = d.get_temp::<Rect>(id) {
            // 时间常数约 1/17 秒：比系统分段控件略快，干脆利落。
            let t = 1.0 - (-dt * 17.0).exp();
            let next = Rect::from_min_max(prev.min.lerp(target.min, t), prev.max.lerp(target.max, t));
            // 足够近时吸附，避免永远差零点几像素。
            if next.center().distance(target.center()) < 0.35
                && (next.width() - target.width()).abs() < 0.35
                && (next.height() - target.height()).abs() < 0.35
            {
                d.insert_temp(id, target);
                (target, false)
            } else {
                d.insert_temp(id, next);
                (next, true)
            }
        } else {
            d.insert_temp(id, target);
            (target, false)
        }
    });
    // 关键：egui 默认只在有输入事件时重绘。动画进行中必须主动预订下一帧，
    // 否则鼠标静止点击后滑块会一顿一顿（帧间隔被鼠标事件牵着走）。
    if moving {
        ui.ctx().request_repaint();
    }
    rect
}

/// 画一片液态玻璃滑块（柔影 + 白玻璃 + 发丝描边 + 顶部高光棱边）。
pub fn paint_glass_slider(ui: &egui::Ui, rect: Rect, radius: u8) {
    let painter = ui.painter();
    let cr = CornerRadius::same(radius);
    let shadow = if crate::glass::material_active() {
        Shadow {
            offset: [0, 2],
            blur: 9,
            spread: 0,
            color: rgba(0, 0, 0, 34),
        }
    } else {
        Shadow {
            offset: [0, 1],
            blur: 5,
            spread: 0,
            color: rgba(0, 0, 0, 26),
        }
    };
    // 0.36 的柔影：Shadow::as_shape 生成带 blur_width 的 RectShape，先画在主体之下。
    painter.add(shadow.as_shape(rect, cr));
    painter.rect_filled(rect, cr, slider_fill());
    painter.rect_stroke(
        rect,
        cr,
        Stroke::new(0.75, rgba(0, 0, 0, 26)),
        egui::StrokeKind::Inside,
    );

    if rect.width() > radius as f32 * 2.0 {
        // 顶部内高光棱边：液态玻璃的标志性反光。
        let edge = Rect::from_min_max(
            egui::pos2(rect.left() + radius as f32 * 0.7, rect.top() + 0.75),
            egui::pos2(rect.right() - radius as f32 * 0.7, rect.top() + 1.4),
        );
        painter.rect_filled(edge, CornerRadius::same(0), rgba(255, 255, 255, 210));
    }
}

/// 画分段控件的凹槽轨道。
pub fn paint_segment_track(ui: &egui::Ui, rect: Rect, radius: u8) {
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(radius), track_fill());
    // 凹槽内顶发丝阴影 + 底部一丝反光，增强「凹陷」层次。
    painter.line_segment(
        [
            egui::pos2(rect.left() + radius as f32, rect.top() + 0.75),
            egui::pos2(rect.right() - radius as f32, rect.top() + 0.75),
        ],
        Stroke::new(0.75, rgba(0, 0, 0, 16)),
    );
}

/// 横向液态玻璃分段选择器（苹果 segmented control）。
///
/// - `items`：`(值, 文案)` 列表，滑块宽度按各自文案宽度伸缩；
/// - 返回本次被点击的值（未点击返回 None）。
pub fn segmented<T>(
    ui: &mut egui::Ui,
    id_src: &str,
    items: &[(T, &str)],
    selected: T,
) -> Option<T>
where
    T: PartialEq + Copy,
{
    let height = 30.0_f32;
    let pad_x = 15.0_f32;
    let gap = 2.0_f32;
    let font = FontId::proportional(14.0);

    // 每段文字只布局一次（占位色），宽度测量与最终绘制共用这份 galley，
    // 避免动画期间每帧对每段文字重复布局/分配。
    let galleys: Vec<std::sync::Arc<egui::Galley>> = items
        .iter()
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap((*label).to_string(), font.clone(), Color32::PLACEHOLDER)
        })
        .collect();
    let seg_w: Vec<f32> = galleys.iter().map(|g| g.size().x + pad_x * 2.0).collect();
    let total_w = seg_w.iter().sum::<f32>() + gap * (items.len().saturating_sub(1)) as f32;

    let (track_rect, _) =
        ui.allocate_exact_size(Vec2::new(total_w, height), egui::Sense::hover());
    let radius = (height / 2.0) as u8;
    paint_segment_track(ui, track_rect, radius);

    // 摆放每段并找到当前选中段的目标框。
    let anim_id = ui.id().with(("glass_seg", id_src));
    let mut target = track_rect;
    let mut rects: Vec<(T, Rect, f32)> = Vec::with_capacity(items.len());
    let mut x = track_rect.left();
    for (i, ((key, _), w)) in items.iter().zip(seg_w.iter()).enumerate() {
        let r = Rect::from_min_size(egui::pos2(x, track_rect.top()), Vec2::new(*w, height));
        rects.push((*key, r, galleys[i].size().x));
        if *key == selected {
            target = r;
        }
        x += w + gap;
    }
    let slider_target = target.shrink(2.0);
    let anim = animated_slider_rect(ui, anim_id, slider_target);
    paint_glass_slider(ui, anim, radius.saturating_sub(2));

    // 交互与文字（文字最后画，永远在滑块之上）。
    let painter = ui.painter();
    let mut clicked = None;
    for (i, (key, r, text_w)) in rects.iter().enumerate() {
        let resp = ui.interact(*r, anim_id.with(i), egui::Sense::click());
        let is_sel = *key == selected;
        if !is_sel && resp.hovered() {
            painter.rect_filled(
                r.shrink(2.0),
                CornerRadius::same(radius.saturating_sub(2)),
                rgba(0, 0, 0, 9),
            );
        }
        let color = if is_sel {
            PRIMARY
        } else if resp.hovered() {
            TEXT_MAIN
        } else {
            TEXT_DIM
        };
        let galley = galleys[i].clone();
        let text_pos = egui::pos2(
            r.center().x - text_w / 2.0,
            r.center().y - galley.size().y / 2.0,
        );
        painter.galley_with_override_text_color(text_pos, galley, color);
        if resp.clicked() {
            clicked = Some(*key);
        }
    }
    clicked
}

/// 竖向导航用：把一块玻璃滑块动画地移动/伸缩到 `target`。
pub fn nav_slider(ui: &egui::Ui, id_src: &str, target: Rect) {
    let id = ui.id().with(("glass_nav", id_src));
    let anim = animated_slider_rect(ui, id, target);
    paint_glass_slider(ui, anim, 14);
}

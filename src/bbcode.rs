//! Nexus 模组描述的 BBCode 简易渲染器（容错，非完整规格）。
//!
//! 支持：[b][i][u][s]、[color=..]、[size=n]、[url=..]、[img]、
//! [center]、[list]/[*]、[hr]、<br>；未识别标签剥离保留内容。
//! 描述里的外链图片走 imgcache 直接按绝对 URL 加载。

use crate::imgcache;
use egui::{Color32, RichText};
use regex::Regex;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 解析结果缓存（弹窗每帧重绘，避免重复解析长描述）；上限 8 条 LRU。
static DOC_CACHE: OnceLock<Mutex<Vec<(String, Vec<Block>)>>> = OnceLock::new();

fn doc_cache() -> &'static Mutex<Vec<(String, Vec<Block>)>> {
    DOC_CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

#[derive(Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    color: Option<Color32>,
    size: f32,
}

#[derive(Clone)]
struct Span {
    text: String,
    style: Style,
    link: Option<String>,
}

#[derive(Clone)]
enum Block {
    /// 普通段落行（可自动换行）。
    Line { spans: Vec<Span>, centered: bool, bullet: bool },
    /// 整行独立图片。
    Image(String),
    Rule,
}

fn tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\[/?[a-z\*][^\]]*\]").unwrap())
}

fn named_color(name: &str) -> Option<Color32> {
    let map: HashMap<&str, u32> = [
        ("red", 0xE53935),
        ("green", 0x43A047),
        ("blue", 0x1E88E5),
        ("yellow", 0xFDD835),
        ("orange", 0xFB8C00),
        ("purple", 0x8E24AA),
        ("violet", 0x8E24AA),
        ("cyan", 0x00ACC1),
        ("brown", 0x6D4C41),
        ("white", 0xFFFFFF),
        ("black", 0x000000),
        ("gray", 0x757575),
        ("grey", 0x757575),
        ("pink", 0xEC407A),
        ("gold", 0xC9A227),
        ("silver", 0x9E9E9E),
    ]
    .iter()
    .copied()
    .collect();
    map.get(name.to_lowercase().trim())
        .map(|&h| Color32::from_rgb((h >> 16) as u8, (h >> 8) as u8, h as u8))
}

fn parse_color(v: &str) -> Option<Color32> {
    let v = v.trim().trim_start_matches('#');
    if let Ok(n) = u32::from_str_radix(v, 16) {
        return match v.len() {
            6 => Some(Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8)),
            3 => {
                let r = ((n >> 8) & 0xF) as u8;
                let g = ((n >> 4) & 0xF) as u8;
                let b = (n & 0xF) as u8;
                Some(Color32::from_rgb(r * 17, g * 17, b * 17))
            }
            _ => None,
        };
    }
    named_color(v)
}

/// BBCode 1-7 相对字号 → egui 字号。
fn bb_size(n: i32) -> f32 {
    match n {
        1 => 10.5,
        2 => 11.5,
        3 => 12.5,
        4 => 13.5,
        5 => 15.0,
        6 => 17.0,
        _ => 19.0,
    }
}

fn html_unescape(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn parse(text: &str) -> Vec<Block> {
    // 块级预处理：<br> 系列换行；[hr] 单独成行。
    let t = Regex::new(r"(?i)<br\s*/?>").unwrap().replace_all(text, "\n");
    let t = Regex::new(r"(?i)\[hr\][^\[]*(\[/hr\])?").unwrap().replace_all(&t, "\n[[-HR-]]\n");
    let mut blocks = Vec::new();
    let mut center_depth = 0u32;
    let mut list_depth = 0u32;

    for raw_line in t.split('\n') {
        let mut line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        // 独立分隔线。
        if line.contains("[[-HR-]]") {
            blocks.push(Block::Rule);
            if line.replace("[[-HR-]]", "").trim().is_empty() {
                continue;
            }
        }
        let centered_before = center_depth > 0;
        // 整块开关计数（不跨行精确配对，按出现次数近似）。
        let open_center = line.matches("[center]").count()
            + line.matches("[CENTER]").count()
            + line.matches("[center").count();
        let close_center = line.matches("[/center]").count() + line.matches("[/CENTER]").count();
        let open_list = line.matches("[list").count();
        let close_list = line.matches("[/list]").count();

        // 整行独立图片：[img...]url[/img]
        let img_re =
            Regex::new(r"(?is)^\s*(?:\[/?(?:center|list)[^\]]*\]\s*)*\[img[^\]]*\](.*?)\[/img\]\s*(?:\[/?(?:center|list)[^\]]*\]\s*)*$")
                .unwrap();
        if let Some(c) = img_re.captures(line) {
            let url = c.get(1).unwrap().as_str().trim();
            if url.starts_with("http") {
                blocks.push(Block::Image(url.to_string()));
                center_depth = center_depth.saturating_add(open_center as u32);
                center_depth = center_depth.saturating_sub(close_center as u32);
                list_depth = list_depth.saturating_add(open_list as u32);
                list_depth = list_depth.saturating_sub(close_list as u32);
                continue;
            }
        }

        let bullet = (line.contains("[*]") || line.contains("[*]")) && list_depth > 0
            || line.contains("[*]");

        let spans = parse_inline(line);
        if !spans.is_empty() {
            blocks.push(Block::Line {
                spans,
                centered: centered_before,
                bullet,
            });
        }
        center_depth = center_depth.saturating_add(open_center as u32);
        center_depth = center_depth.saturating_sub(close_center as u32);
        list_depth = list_depth.saturating_add(open_list as u32);
        list_depth = list_depth.saturating_sub(close_list as u32);
    }
    blocks
}

/// 解析一行内的行内标签为 Span 序列。
fn parse_inline(line: &str) -> Vec<Span> {
    let re = tag_re();
    let mut spans = Vec::new();
    let mut style = Style::default();
    let mut last = 0usize;

    let push_text = |text: &str, style: &Style, spans: &mut Vec<Span>| {
        let t = html_unescape(text);
        if !t.is_empty() {
            spans.push(Span {
                text: t,
                style: style.clone(),
                link: None,
            });
        }
    };

    for m in re.find_iter(line) {
        if m.start() > last {
            push_text(&line[last..m.start()], &style, &mut spans);
        }
        let tag = m.as_str();
        let lower = tag.to_lowercase();
        let inner = &tag[1..tag.len() - 1]; // 去括号
        let name_val: Vec<&str> = inner.splitn(2, '=').collect();
        let name = name_val[0].trim_start_matches('/').to_lowercase();
        let val = if name_val.len() > 1 {
            name_val[1].trim().trim_matches('"')
        } else {
            ""
        };
        let closing = inner.trim_start().starts_with('/');

        match name.as_str() {
            "b" => style.bold = !closing,
            "i" => style.italic = !closing,
            "u" => style.underline = !closing,
            "s" => style.strike = !closing,
            "color" if !closing => style.color = parse_color(val),
            "color" => style.color = None,
            "size" if !closing => style.size = val.parse::<i32>().map(bb_size).unwrap_or(12.5),
            "size" => style.size = 0.0,
            "font" if closing => {} // 字体标签整体忽略
            "font" => {}
            "url" => {
                // 抓 [/url] 前的内容作为链接文本。
                let close_re = Regex::new(r"(?i)\[/url\]").unwrap();
                if let Some(cm) = close_re.find(&line[m.end()..]) {
                    let link_text = &line[m.end()..m.end() + cm.start()];
                    let href = if !val.is_empty() {
                        val.to_string()
                    } else {
                        link_text.trim().to_string()
                    };
                    if href.starts_with("http") {
                        spans.push(Span {
                            text: html_unescape(strip_tags(link_text).trim()),
                            style: Style {
                                color: Some(crate::liquid::PRIMARY),
                                underline: true,
                                ..Default::default()
                            },
                            link: Some(href),
                        });
                    } else {
                        push_text(link_text, &style, &mut spans);
                    }
                    last = m.end() + cm.end();
                    continue;
                }
            }
            "img" => {
                // 行内混排图片：跳到 [/img]，暂不内联（少见），丢弃。
                let close_re = Regex::new(r"(?i)\[/img\]").unwrap();
                if let Some(cm) = close_re.find(&line[m.end()..]) {
                    last = m.end() + cm.end();
                    continue;
                }
            }
            "*" | "/*" | "list" | "center" | "/list" | "/center" => {}
            _ => {}
        }
        last = m.end();
    }
    if last < line.len() {
        push_text(&line[last..], &style, &mut spans);
    }
    spans
}

fn strip_tags(s: &str) -> &str {
    // 链接文本里再嵌套标签的情况很少，简单返回原文（渲染时标签会显示），
    // 这里只处理最常见的纯文本。
    s.trim_matches(|c: char| c == '[' || c == ']')
}

fn rich_from(span: &Span) -> RichText {
    let mut rt = RichText::new(&span.text)
        .color(span.style.color.unwrap_or_else(crate::liquid::text_main))
        .size(if span.style.size > 0.0 {
            span.style.size
        } else {
            12.5
        });
    if span.style.bold {
        rt = rt.strong();
    }
    if span.style.italic {
        rt = rt.italics();
    }
    if span.style.underline {
        rt = rt.underline();
    }
    if span.style.strike {
        rt = rt.strikethrough();
    }
    rt
}

/// 在给定 ui 中渲染整段 BBCode 描述。
pub fn render(ui: &mut egui::Ui, text: &str) {
    // 解析结果按原文缓存（命中 LRU 提到队首）。
    let blocks = {
        let mut cache = doc_cache().lock().unwrap();
        if let Some(pos) = cache.iter().position(|(k, _)| k == text) {
            let entry = cache.remove(pos);
            let blocks = entry.1.clone();
            cache.insert(0, entry);
            blocks
        } else {
            let blocks = parse(text);
            cache.insert(0, (text.to_string(), blocks.clone()));
            while cache.len() > 8 {
                cache.pop();
            }
            blocks
        }
    };
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(380.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for b in blocks {
                match b {
                    Block::Rule => {
                        ui.add_space(4.0);
                        ui.separator();
                        ui.add_space(4.0);
                    }
                    Block::Image(url) => {
                        image_block(ui, &url);
                    }
                    Block::Line { spans, centered, bullet } => {
                        let draw = |ui: &mut egui::Ui| {
                            ui.horizontal_wrapped(|ui| {
                                if bullet {
                                    ui.label(
                                        RichText::new("• ").color(crate::liquid::text_dim()),
                                    );
                                }
                                for sp in &spans {
                                    if let Some(href) = &sp.link {
                                        let resp =
                                            ui.add(egui::Link::new(rich_from(sp)));
                                        if resp.clicked() {
                                            ui.ctx().open_url(egui::OpenUrl {
                                                url: href.clone(),
                                                new_tab: true,
                                            });
                                        }
                                        resp.on_hover_text(href);
                                    } else if !sp.text.trim().is_empty() {
                                        ui.label(rich_from(sp));
                                    }
                                }
                            });
                        };
                        if centered {
                            ui.vertical_centered(|ui| {
                                ui.set_max_width(ui.available_width());
                                draw(ui);
                            });
                        } else {
                            draw(ui);
                        }
                        ui.add_space(2.0);
                    }
                }
            }
        });
}

/// 描述中的外链图片：限宽 320，按纵横比缩放。
fn image_block(ui: &mut egui::Ui, url: &str) {
    let state = imgcache::request_abs(ui.ctx(), url);
    let max_w = 320.0_f32.min(ui.available_width());
    match state {
        imgcache::ImgState::Ready(tex) => {
            let [tw, th] = tex.size();
            let h = max_w * th as f32 / tw as f32;
            ui.add(
                egui::Image::from_texture(&tex)
                    .fit_to_exact_size(egui::vec2(max_w, h)),
            );
        }
        imgcache::ImgState::Loading => {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                crate::liquid::spinner(ui);
                ui.label(RichText::new("图片加载中…").size(11.0).weak());
            });
            ui.add_space(2.0);
        }
        imgcache::ImgState::Failed => {
            ui.label(
                RichText::new("🖼 图片不可用")
                    .size(11.0)
                    .color(crate::liquid::text_dim()),
            );
        }
    }
}

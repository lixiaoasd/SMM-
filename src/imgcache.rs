//! 镜像缩略图异步加载与缓存。
//!
//! - 首次请求：占位 Loading，后台线程 内存←磁盘←HTTP 逐级取图；
//! - 下载后原图落盘（%APPDATA%\StardewModManager\img-cache），解码时缩到
//!   最大宽 560px 再上传 GPU，避免 1080p 原图每张占 8MB 显存；
//! - 内存纹理 LRU 上限 256 张（滚动浏览几千张也不会爆显存）；
//! - 同一 key 的并发请求只触发一次加载（Loading 占位去重）。

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// 解码后最大宽度（卡片 ~96px + 详情 ~320px，兼顾高分屏 2x）。
const MAX_W: u32 = 560;
/// 内存中同时保留的纹理数（LRU）。
const MEM_LIMIT: usize = 256;
/// 取图 worker 数量。
///
/// 原实现是「每张未命中图片各起一个线程 + 各建一个 HTTP Client」：快速滚动
/// 几千条的模组清单时会瞬间拉起上百个线程和连接池。改成固定小线程池 +
/// 共享 Client，并发上限可控。
const WORKERS: usize = 4;

/// 取图任务：(图片 URL, 缓存 key, egui 上下文)。
type Job = (String, String, Context);

static JOBS: OnceLock<std::sync::mpsc::Sender<Job>> = OnceLock::new();
static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

fn client() -> &'static reqwest::blocking::Client {
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .user_agent("StardewModManager")
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new())
    })
}

/// 懒启动固定数量的取图 worker，返回任务队列发送端。
fn jobs() -> &'static std::sync::mpsc::Sender<Job> {
    JOBS.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let rx = std::sync::Arc::new(Mutex::new(rx));
        for _ in 0..WORKERS {
            let rx = rx.clone();
            std::thread::spawn(move || loop {
                // 只在等待取任务时持锁，取到立即释放。
                let job = {
                    let guard = rx.lock().unwrap();
                    guard.recv()
                };
                let Ok((url, key, ctx)) = job else { return };
                let result = load_and_decode(&url, &key);
                let mut st = store().lock().unwrap();
                match result {
                    Some(img) => {
                        let tex = ctx.load_texture(
                            format!("mirror-img:{key}"),
                            img,
                            TextureOptions::LINEAR,
                        );
                        st.map.insert(key.clone(), Slot::Ready(tex));
                        touch_lru(&mut st, &key);
                        evict(&mut st);
                    }
                    None => {
                        st.map.insert(key.clone(), Slot::Failed);
                    }
                }
                drop(st);
                ctx.request_repaint();
            });
        }
        tx
    })
}

enum Slot {
    Loading,
    Ready(TextureHandle),
    Failed,
}

struct Store {
    map: HashMap<String, Slot>,
    /// LRU 顺序（front=最近使用）。
    lru: VecDeque<String>,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| {
        Mutex::new(Store {
            map: HashMap::new(),
            lru: VecDeque::new(),
        })
    })
}

fn cache_dir() -> PathBuf {
    crate::model::config_dir().join("img-cache")
}

/// 用镜像相对路径生成安全的磁盘缓存文件名。
fn disk_path(key: &str) -> PathBuf {
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache_dir().join(safe)
}

/// 请求镜像内图片（rel 形如 "thumbs/123.png"，自动拼镜像 base）。
pub fn request(ctx: &Context, base: &str, rel: &str) -> ImgState {
    let key = rel.trim();
    if key.is_empty() {
        return ImgState::Failed;
    }
    let url = format!("{}/{}", base.trim().trim_end_matches('/'), key.trim_start_matches('/'));
    request_url(ctx, &url, key)
}

/// 请求任意外站图片（描述内嵌图）；key 用 URL 哈希。
pub fn request_abs(ctx: &Context, url: &str) -> ImgState {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let key = format!("ext_{:016x}", hasher.finish());
    request_url(ctx, url, &key)
}

fn request_url(ctx: &Context, url: &str, key: &str) -> ImgState {
    // 先查内存。
    {
        let mut st = store().lock().unwrap();
        if let Some(slot) = st.map.get(key) {
            // 先把状态取出（Ready 克隆纹理句柄，廉价 Arc 引用），再调整 LRU。
            let state = match slot {
                Slot::Loading => ImgState::Loading,
                Slot::Ready(t) => ImgState::Ready(t.clone()),
                Slot::Failed => ImgState::Failed,
            };
            touch_lru(&mut st, key);
            return state;
        }
        st.map.insert(key.to_string(), Slot::Loading);
        st.lru.push_front(key.to_string());
    }

    let _ = jobs().send((url.to_string(), key.to_string(), ctx.clone()));

    ImgState::Loading
}

fn touch_lru(st: &mut Store, key: &str) {
    if let Some(pos) = st.lru.iter().position(|k| k == key) {
        st.lru.remove(pos);
    }
    st.lru.push_front(key.to_string());
}

fn evict(st: &mut Store) {
    while st.lru.len() > MEM_LIMIT {
        if let Some(old) = st.lru.pop_back() {
            // Loading/Failed 条目可能不在 map 正常位，但 remove 无害。
            st.map.remove(&old);
        } else {
            break;
        }
    }
}

/// 磁盘→HTTP 取字节，缩到 MAX_W 宽后解码为 ColorImage。
fn load_and_decode(url: &str, key: &str) -> Option<ColorImage> {
    let dp = disk_path(key);
    let bytes = match std::fs::read(&dp) {
        Ok(b) if !b.is_empty() => b,
        _ => {
            let mut resp = client().get(url).send().ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let mut buf = Vec::new();
            resp.read_to_end(&mut buf).ok()?;
            if buf.is_empty() {
                return None;
            }
            // 落盘（失败不影响本次显示）。
            if let Some(parent) = dp.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&dp, &buf);
            buf
        }
    };
    decode_resized(&bytes)
}

/// 解码并把过宽的图等比缩小，控制显存占用。
fn decode_resized(bytes: &[u8]) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    let rgba = if w > MAX_W {
        let n_h = ((h as f32) * (MAX_W as f32) / (w as f32)).round() as u32;
        image::imageops::thumbnail(&img.to_rgba8(), MAX_W, n_h.max(1))
    } else {
        img.to_rgba8()
    };
    let (w2, h2) = (rgba.width() as usize, rgba.height() as usize);
    Some(ColorImage::from_rgba_unmultiplied(
        [w2, h2],
        rgba.as_raw(),
    ))
}

/// request() 的即时结果。
pub enum ImgState {
    Loading,
    Ready(TextureHandle),
    Failed,
}

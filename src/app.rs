//! 应用主界面与状态管理（egui）。
//!
//! 三个页面：模组库（镜像直装：分类侧栏 + 搜索 + 排序，一键装进 Mods）、
//! 我的模组（启用/禁用/删除）、设置（游戏路径 / API Key / SMAPI）。
//!
//! 下载流程：在模组库点「⚡ 直装」→ 后台从镜像源下载（真实进度条）→
//! 下载完成自动解压安装到 Mods（见 mirror.rs）。

use crate::bbcode;
use crate::imgcache;
use crate::installer;
use crate::liquid;
use crate::mirror::{
    self, MJobSnapshot, MirrorCollection, MirrorMod, MState as MJobState,
};
use crate::model::{ModEntry, Settings};
use crate::mods as mods_mgr;
use crate::server::{self, HostKitConfig, HostStatus, SaveInfo, VntRuntime};
use crate::p2p::{self, DownloadSource, Friend, ModSync, PeerInfo, Profile, SharedMod};
use crate::watch::{WatchJob, WState};
use crate::web::{self, NexusMod, NexusModSummary};
use crate::paths::{self, GameEnv};
use egui::{Color32, RichText};
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---------- 后台线程 → UI 的结果槽 ----------

static LIST_RESULT: OnceLock<Mutex<Option<Result<Vec<NexusModSummary>, String>>>> =
    OnceLock::new();
static MIRROR_LIST_RESULT: OnceLock<Mutex<Option<Result<Vec<MirrorMod>, String>>>> =
    OnceLock::new();
static MIRROR_COLLS_RESULT: OnceLock<Mutex<Option<Vec<MirrorCollection>>>> = OnceLock::new();
static ID_RESULT: OnceLock<Mutex<Option<(u32, Result<NexusMod, String>)>>> = OnceLock::new();
static ZIP_RESULT: OnceLock<Mutex<Option<Result<Vec<String>, String>>>> = OnceLock::new();
static SMAPI_RESULT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static SAVES_RESULT: OnceLock<Mutex<Option<Vec<SaveInfo>>>> = OnceLock::new();
static HOST_PROBE_RESULT: OnceLock<Mutex<Option<HostProbe>>> = OnceLock::new();
static HOST_MSG_RESULT: OnceLock<Mutex<Option<String>>> = OnceLock::new();

// —— 联机大厅 ——
static PEERS_RESULT: OnceLock<Mutex<Option<Vec<PeerInfo>>>> = OnceLock::new();
static PEER_MODS_RESULT: OnceLock<Mutex<Option<Result<Vec<SharedMod>, String>>>> =
    OnceLock::new();
static SOCIAL_MSG_RESULT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static SYNC_PROGRESS_RESULT: OnceLock<Mutex<Option<(String, bool, usize, usize)>>> =
    OnceLock::new();

// ---------- 页面 ----------

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Host,
    Social,
    Download,
    Mods,
    Settings,
}

/// 开服相关的环境探测结果（在后台线程里收集，避免卡 UI）。
#[derive(Debug, Clone, Default)]
struct HostProbe {
    game_running: bool,
    /// UDP 24642 已在本机被监听 = 服务器已经开起来了。
    port_busy: bool,
    local_ip: Option<String>,
    firewall_ok: bool,
    vnt: VntRuntime,
    /// 插件写出的房主状态（已过滤掉过期数据）。
    status: Option<HostStatus>,
}

impl HostProbe {
    fn collect(game: Option<&std::path::Path>) -> Self {
        HostProbe {
            game_running: server::game_running(),
            port_busy: server::port_in_use(server::GAME_PORT),
            local_ip: server::primary_local_ip(),
            firewall_ok: server::firewall_allowed(server::GAME_PORT),
            vnt: server::vnt_probe(),
            status: game.and_then(server::read_status).map(|s| s.status),
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum EnableFilter {
    All,
    Enabled,
    Disabled,
}

#[derive(PartialEq, Clone, Copy)]
enum NexusListType {
    Trending,
    LatestUpdated,
    LatestAdded,
}

impl NexusListType {
    fn api_key_name(&self) -> &'static str {
        match self {
            NexusListType::Trending => "trending",
            NexusListType::LatestUpdated => "latest_updated",
            NexusListType::LatestAdded => "latest_added",
        }
    }
    fn label(&self) -> &'static str {
        match self {
            NexusListType::Trending => "热门",
            NexusListType::LatestUpdated => "最新更新",
            NexusListType::LatestAdded => "最新发布",
        }
    }
    fn all() -> [NexusListType; 3] {
        [
            NexusListType::Trending,
            NexusListType::LatestUpdated,
            NexusListType::LatestAdded,
        ]
    }
}

/// 镜像库「过滤+排序」视图缓存：输入键（合集/分类/搜索/排序/数据版本）不变时，
/// 每帧直接复用派生结果，避免对整个清单重复克隆、过滤、排序。
struct MirrorView {
    coll_sel: String,
    sel: String,
    q: String,
    sort: u8,
    list_ver: u64,
    colls_ver: u64,
    mods_ver: u64,
    /// 清单总数。
    total_n: usize,
    /// 分类统计（数量降序）。
    cats: Vec<(String, usize)>,
    /// 热门合集行 (id, 中文名, tooltip, 命中数)。
    coll_rows: Vec<(String, String, String, usize)>,
    /// 过滤+排序后的展示列表。
    list: Vec<MirrorMod>,
    /// 每行的缺失前置/冲突警告（与 list 平行对齐；None=无警告）。
    warns: Vec<Option<RowWarn>>,
}

/// 模组库一行的交互结果。
enum MirrorRowAction {
    None,
    /// 点击了「⚡ 直装 / 重新下载」。
    Install(MirrorMod),
    /// 点击了行内「取消」。
    Cancel(u64),
    /// 点击卡片主体，打开模组详情（携带预计算的前置/冲突）。
    OpenDetail(MirrorMod, Vec<MissingDep>, Vec<String>),
}

/// 一行的缺失前置/冲突警告（MirrorView 构建时预计算，与 list 平行对齐）。
#[derive(Clone)]
struct RowWarn {
    /// 缺失的必需前置（未安装且非 SMAPI 框架本身）。
    missing: Vec<MissingDep>,
    /// 冲突的已安装模组显示名（双向检测：本模组规则 + 已装模组规则）。
    conflicts: Vec<String>,
}

/// 缺失的一个前置：镜像收录时带条目（可直接补装），否则仅 UniqueID。
#[derive(Clone)]
struct MissingDep {
    uid: String,
    mirror: Option<MirrorMod>,
}

impl MissingDep {
    /// 展示名：镜像收录用其名称，未收录标注。
    fn display(&self) -> String {
        match &self.mirror {
            Some(m) if !m.name.is_empty() => m.name.clone(),
            _ => format!("{}（未收录）", self.uid),
        }
    }
}

/// 直装前检查弹窗（发现缺前置/冲突时弹出，让用户确认如何处理）。
struct DepModal {
    target: MirrorMod,
    missing: Vec<MissingDep>,
    conflicts: Vec<String>,
}

/// 编译 SMAPI Conflicts 正则（大小写不敏感；非法/过大的模式忽略）。
fn conf_regex(pattern: &str) -> Option<regex::Regex> {
    let p = pattern.trim();
    if p.is_empty() {
        return None;
    }
    regex::RegexBuilder::new(p)
        .case_insensitive(true)
        .size_limit(1 << 16)
        .build()
        .ok()
}

/// 虚拟化列表的等高行高（卡片实测约 104px，留 4px 余量防相邻行重叠）。
const MIRROR_ROW_H: f32 = 108.0;

pub struct App {
    settings: Settings,
    env: GameEnv,
    mods: Vec<ModEntry>,
    page: Page,
    status: String,
    theme_applied: bool,
    /// 应用启动时刻（glass 材质重贴时序用）。
    start: Instant,

    // —— 我的模组页 ——
    search: String,
    enable_filter: EnableFilter,
    armed_delete: Option<String>,

    // —— 下载页：N 网列表 ——
    list_type: NexusListType,
    nexus_list: Vec<NexusModSummary>,
    list_loading: bool,
    list_loaded: bool,
    list_error: Option<String>,
    selected: HashSet<u32>,
    hide_installed: bool,
    // 按 ID 查询
    id_input: String,
    id_result: Option<NexusMod>,
    id_error: Option<String>,
    id_loading: bool,

    // —— 浏览器下载监控 ——
    watch_jobs: Vec<WatchJob>,
    finished_ok_seen: HashSet<u64>,

    // —— 镜像源直装 ——
    mirror_list: Vec<MirrorMod>,
    mirror_loading: bool,
    mirror_loaded: bool,
    mirror_error: Option<String>,
    mirror_jobs: Vec<MJobSnapshot>,
    mirror_ok_seen: HashSet<u64>,
    /// 选中的分类中文名；空串=全部。
    mirror_category: String,
    /// 选中的热门合集 id；空串=未选合集。合集与分类互斥。
    mirror_collection: String,
    /// 镜像源提供的热门合集（新手必装 / 大型扩展 等）。
    mirror_colls: Vec<MirrorCollection>,
    /// 搜索关键字（匹配名称/作者/描述）。
    mirror_search: String,
    /// 模组库排序：0=人气（清单默认序），1=名称，2=大小。
    mirror_sort: u8,

    // —— 镜像库性能缓存：避免每帧全量过滤/排序/安装匹配 ——
    /// 已安装模组 UniqueID（大写）集合，refresh 时重建。
    installed_uids: HashSet<String>,
    /// 已安装模组归一化名集合（名称 + 文件夹名），refresh 时重建。
    installed_norms: HashSet<String>,
    /// 已安装模组的冲突规则 (正则模式, 拥有者显示名)，refresh 时重建。
    installed_confs: Vec<(String, String)>,
    /// 直装前检查弹窗（缺前置/冲突确认）。
    dep_modal: Option<DepModal>,
    /// 模组详情弹窗：(模组, 缺失前置, 冲突名)。
    detail_modal: Option<(MirrorMod, Vec<MissingDep>, Vec<String>)>,
    /// 版本号：本地模组扫描（refresh）后 +1。
    mods_ver: u64,
    /// 版本号：镜像清单更新后 +1。
    mirror_list_ver: u64,
    /// 版本号：热门合集更新后 +1。
    mirror_colls_ver: u64,
    /// 镜像清单中已安装 mod_id 集合缓存：(mods_ver, mirror_list_ver, 集合, 数量)。
    mirror_installed_cache: Option<(u64, u64, HashSet<i64>, usize)>,
    /// 过滤+排序视图缓存（输入键不变时直接复用）。
    mirror_view: Option<MirrorView>,

    // —— 串行下载队列：一次只开一个文件页，装完再开下一个 ——
    dl_queue: VecDeque<(u32, String)>,
    dl_current: Option<(u32, String)>,
    dl_baseline: u64,
    dl_total: usize,

    // —— 本地 zip ——
    zip_msg: String,

    // —— 设置页 ——
    game_path_input: String,
    api_key_input: String,
    mirror_url_input: String,
    smapi_busy: bool,

    // —— 一键开服页 ——
    saves: Vec<SaveInfo>,
    saves_loaded: bool,
    probe: HostProbe,
    probe_at: Instant,
    probe_busy: bool,
    host_busy: bool,
    host_msg: String,
    copied: bool,
    /// 待广播给所有玩家的公告文字。
    announce_input: String,

    // —— 联机大厅页 ——
    profile: Option<Profile>,
    profile_name_input: String,
    profile_avatar_idx: usize,
    social_online: bool,
    peers: Vec<PeerInfo>,
    peers_at: Instant,
    friends: Vec<Friend>,
    friends_loaded: bool,
    peer_mods: Vec<SharedMod>,
    peer_mods_loading: bool,
    peer_mods_error: Option<String>,
    /// 当前选中的好友 UID（用于浏览对方模组）。
    selected_friend_uid: String,
    /// 当前选中的好友 IP（从 peers 列表查找）。
    selected_friend_ip: String,
    social_msg: String,
    /// 选中的要发送的本地模组文件夹名。
    send_mod_selected: String,
    /// 模组同步结果。
    sync: Option<ModSync>,
    /// 下载来源选择（0=好友+服务器同时, 1=仅服务器, 2=仅好友）。
    sync_source: usize,
    /// 批量补齐是否进行中。
    sync_busy: bool,
    /// 批量补齐进度文字。
    sync_progress: String,
    /// 是否显示模组同步弹窗。
    show_sync_modal: bool,
    /// 已为哪个好友 UID 显示过弹窗（避免重复弹）。
    sync_modal_shown_uid: String,

    // —— 灵动岛（下载指示器）——
    /// 0.0=收起, 1.0=展开；每帧 lerp 趋近目标。
    island_expand: f32,
    /// 最近 60 个速度样本（KB/s），用于波形图。
    island_speeds: Vec<f32>,
    /// 上次采样时的已下载字节数（用于计算速度）。
    island_last_bytes: u64,
    /// 上次采样时间。
    island_last_time: Instant,
    /// 下载完成后显示「所有内容已下载」的倒计时（秒）。
    island_done_secs: f32,
    /// 上一帧是否在下载（用于检测 Downloading→Idle 转换触发 Done 态）。
    island_was_downloading: bool,
}

impl Default for App {
    fn default() -> Self {
        let settings = Settings::load();
        let env = paths::detect(settings.game_path.as_deref());
        let mods = match &env.mods_path {
            Some(p) => mods_mgr::scan_mods(p),
            None => Vec::new(),
        };
        App {
            settings,
            env,
            mods,
            // 落地页保持模组库不变：一键开服是侧栏第一项，点一下就到。
            page: Page::Download,
            status: String::new(),
            theme_applied: false,
            start: Instant::now(),
            search: String::new(),
            enable_filter: EnableFilter::All,
            armed_delete: None,
            list_type: NexusListType::Trending,
            nexus_list: Vec::new(),
            list_loading: false,
            list_loaded: false,
            list_error: None,
            selected: HashSet::new(),
            hide_installed: false,
            id_input: String::new(),
            id_result: None,
            id_error: None,
            id_loading: false,
            watch_jobs: Vec::new(),
            finished_ok_seen: HashSet::new(),
            mirror_list: Vec::new(),
            mirror_loading: false,
            mirror_loaded: false,
            mirror_error: None,
            mirror_jobs: Vec::new(),
            mirror_ok_seen: HashSet::new(),
            mirror_category: String::new(),
            mirror_collection: String::new(),
            mirror_colls: Vec::new(),
            mirror_search: String::new(),
            mirror_sort: 0,
            installed_uids: HashSet::new(),
            installed_norms: HashSet::new(),
            installed_confs: Vec::new(),
            dep_modal: None,
            detail_modal: None,
            mods_ver: 0,
            mirror_list_ver: 0,
            mirror_colls_ver: 0,
            mirror_installed_cache: None,
            mirror_view: None,
            dl_queue: VecDeque::new(),
            dl_current: None,
            dl_baseline: 0,
            dl_total: 0,
            zip_msg: String::new(),
            game_path_input: String::new(),
            api_key_input: String::new(),
            mirror_url_input: String::new(),
            smapi_busy: false,
            saves: Vec::new(),
            saves_loaded: false,
            probe: HostProbe::default(),
            probe_at: Instant::now(),
            probe_busy: false,
            host_busy: false,
            host_msg: String::new(),
            copied: false,
            announce_input: String::new(),

            // —— 联机大厅 ——
            profile: p2p::load_profile(),
            profile_name_input: p2p::load_profile()
                .map(|p| p.name)
                .unwrap_or_default(),
            profile_avatar_idx: 0,
            social_online: false,
            peers: Vec::new(),
            peers_at: Instant::now(),
            friends: Vec::new(),
            friends_loaded: false,
            peer_mods: Vec::new(),
            peer_mods_loading: false,
            peer_mods_error: None,
            selected_friend_uid: String::new(),
            selected_friend_ip: String::new(),
            social_msg: String::new(),
            send_mod_selected: String::new(),
            sync: None,
            sync_source: 0,
            sync_busy: false,
            sync_progress: String::new(),
            show_sync_modal: false,
            sync_modal_shown_uid: String::new(),

            // —— 灵动岛 ——
            island_expand: 0.0,
            island_speeds: Vec::new(),
            island_last_bytes: 0,
            island_last_time: Instant::now(),
            island_done_secs: 0.0,
            island_was_downloading: false,
        }
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        liquid::install(&cc.egui_ctx);
        install_fonts(&cc.egui_ctx);
        let mut app = App::default();
        app.game_path_input = app.settings.game_path.clone().unwrap_or_default();
        app.api_key_input = app.settings.nexus_api_key.clone();
        app.mirror_url_input = if app.settings.mirror_url.trim().is_empty() {
            crate::model::DEFAULT_MIRROR_URL.to_string()
        } else {
            app.settings.mirror_url.clone()
        };
        if let Some(p) = app.env.mods_path.clone() {
            crate::watch::set_mods_dir(p.clone());
            mirror::set_mods_dir(p);
        }
        app
    }

    fn refresh(&mut self) {
        self.env = paths::detect(self.settings.game_path.as_deref());
        if let Some(g) = &self.env.game_path {
            let mods_dir = g.join("Mods");
            let _ = std::fs::create_dir_all(&mods_dir);
            self.env.mods_path = Some(mods_dir.clone());
            self.mods = mods_mgr::scan_mods(&mods_dir);
            crate::watch::set_mods_dir(mods_dir.clone());
            mirror::set_mods_dir(mods_dir);
        } else {
            self.mods.clear();
        }
        // 重建已安装键集合：镜像库每行都要查安装状态，不能每帧对每个
        // 模组重复做字符串归一化（O(清单 × 已安装) 次堆分配）。
        self.mods_ver += 1;
        self.installed_uids = self
            .mods
            .iter()
            .map(|m| m.unique_id().to_uppercase())
            .collect();
        self.installed_norms = self
            .mods
            .iter()
            .flat_map(|m| {
                let folder = m
                    .path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                [
                    Self::normalize_name(&m.name()),
                    Self::normalize_name(&folder),
                ]
            })
            .filter(|s| !s.is_empty())
            .collect();
        // 收集已安装模组声明的冲突规则（用于双向冲突检测）。
        self.installed_confs = self
            .mods
            .iter()
            .flat_map(|m| {
                let name = m.name();
                m.manifest
                    .iter()
                    .flat_map(|mf| mf.conflicts.iter())
                    .map(move |c| (c.clone(), name.clone()))
            })
            .collect();
    }

    // ---------- 后台结果轮询 ----------

    fn poll_background(&mut self, ctx: &egui::Context) {
        if let Some(r) = LIST_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.list_loading = false;
            match r {
                Ok(list) => {
                    self.status = format!("已拉取 {} 个模组", list.len());
                    self.nexus_list = list;
                    self.list_loaded = true;
                    self.list_error = None;
                }
                Err(e) => self.list_error = Some(e),
            }
        }
        if let Some((id, r)) = ID_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.id_loading = false;
            match r {
                Ok(m) => {
                    self.id_result = Some(m);
                    self.id_error = None;
                }
                Err(e) => self.id_error = Some(format!("（ID {id}）{e}")),
            }
        }
        if let Some(r) = ZIP_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            match r {
                Ok(names) => {
                    self.refresh();
                    self.zip_msg = format!("本地安装完成：{}", names.join("、"));
                    self.status = "本地压缩包安装完成".to_string();
                }
                Err(e) => self.zip_msg = format!("本地安装失败：{e}"),
            }
        }
        if let Some(msg) = SMAPI_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.smapi_busy = false;
            self.status = msg;
            self.refresh();
        }
        if let Some(r) = MIRROR_LIST_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.mirror_loading = false;
            match r {
                Ok(list) => {
                    self.mirror_loaded = true;
                    self.mirror_list = list;
                    self.mirror_error = None;
                    // 新清单分类集合可能变化，回到“全部”。
                    if self
                        .mirror_list
                        .iter()
                        .all(|m| m.category != self.mirror_category)
                    {
                        self.mirror_category.clear();
                    }
                    // 选中的合集若在新清单里已不存在，同样取消选择。
                    if !self.mirror_collection.is_empty()
                        && !self
                            .mirror_colls
                            .iter()
                            .any(|c| c.id == self.mirror_collection)
                    {
                        self.mirror_collection.clear();
                    }
                    self.status = format!("镜像清单已刷新（{} 个模组）", self.mirror_list.len());
                }
                Err(e) => self.mirror_error = Some(e),
            }
            self.mirror_list_ver += 1;
        }
        if let Some(colls) = MIRROR_COLLS_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.mirror_colls = colls;
            self.mirror_colls_ver += 1;
            if !self.mirror_collection.is_empty()
                && !self
                    .mirror_colls
                    .iter()
                    .any(|c| c.id == self.mirror_collection)
            {
                self.mirror_collection.clear();
            }
        }
        self.mirror_jobs = mirror::snapshot();
        let mut mirror_done: Option<String> = None;
        for j in &self.mirror_jobs {
            if matches!(j.state, MJobState::Finished(true, _))
                && self.mirror_ok_seen.insert(j.id)
            {
                let detail = match &j.state {
                    MJobState::Finished(_, d) => d.clone(),
                    _ => String::new(),
                };
                mirror_done = Some(format!("「{}」{detail}", j.title));
            }
        }
        if let Some(msg) = mirror_done {
            self.refresh();
            self.status = msg;
        }

        // 监控任务快照；新出现的成功安装触发模组列表刷新。
        self.watch_jobs = crate::watch::snapshot();
        let mut newest_done: Option<String> = None;
        for j in &self.watch_jobs {
            if matches!(j.state, WState::Finished(true, _)) && self.finished_ok_seen.insert(j.id)
            {
                let detail = match &j.state {
                    WState::Finished(_, d) => d.clone(),
                    _ => String::new(),
                };
                newest_done = Some(format!("「{}」{detail}", j.title));
            }
        }
        if let Some(msg) = newest_done {
            self.refresh();
            self.status = msg;
        }

        // 串行队列：当前模组有新的监控任务落定（装完或失败）就开下一个。
        if self.dl_current.is_some() {
            let mut settled: Option<String> = None;
            for j in &self.watch_jobs {
                if j.id > self.dl_baseline
                    && let WState::Finished(ok, detail) = &j.state
                {
                    settled = Some(if *ok {
                        format!("✅ {detail}")
                    } else {
                        format!("⚠ 安装未成功：{detail}（可稍后单独重试）")
                    });
                    break;
                }
            }
            if let Some(msg) = settled {
                let name = self
                    .dl_current
                    .as_ref()
                    .map(|(_, n)| n.clone())
                    .unwrap_or_default();
                self.status = format!("「{name}」{msg}");
                self.dl_current = None;
                self.advance_queue();
            }
        }

        if let Some(list) = SAVES_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.saves = list;
            // 首次进入时默认选中最近保存的存档。
            if self.settings.host.save_name.is_empty() {
                self.settings.host.save_name = self
                    .saves
                    .iter()
                    .find(|s| s.can_host)
                    .or(self.saves.first())
                    .map(|s| s.folder.clone())
                    .unwrap_or_default();
            }
        }
        if let Some(p) = HOST_PROBE_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.probe_busy = false;
            self.probe = p;
            self.probe_at = Instant::now();
        }
        if let Some(msg) = HOST_MSG_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.host_busy = false;
            self.status = msg.clone();
            self.host_msg = msg;
            // 开服/停服后立刻重新探测，状态栏不用等下一轮。
            self.probe_at = self.start;
            self.saves_loaded = false;
        }

        // 开服页才做环境探测：一次 PowerShell 查询约数百毫秒，必须在后台线程跑。
        if self.page == Page::Host {
            if !self.saves_loaded {
                self.saves_loaded = true;
                std::thread::spawn(|| {
                    let list = server::scan_saves();
                    *SAVES_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(list);
                });
            }
            if !self.probe_busy && self.probe_at.elapsed() > Duration::from_secs(3) {
                self.probe_busy = true;
                let game = self.env.game_path.clone();
                std::thread::spawn(move || {
                    let p = HostProbe::collect(game.as_deref());
                    *HOST_PROBE_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(p);
                });
            }
        }

        // —— 联机大厅后台 ——
        if let Some(list) = PEERS_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.peers = list;
            self.peers_at = Instant::now();
        }
        if let Some(r) = PEER_MODS_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.peer_mods_loading = false;
            match r {
                Ok(mods) => {
                    self.peer_mods = mods.clone();
                    self.peer_mods_error = None;
                    // 自动与本地模组对比
                    let local = self.env.mods_path.as_deref().map(p2p::list_shared_mods).unwrap_or_default();
                    self.sync = Some(p2p::compare_mods(&local, &mods));
                    // 如果有缺失且未为该好友弹过窗，弹窗
                    if let Some(s) = &self.sync {
                        if !s.missing.is_empty()
                            && self.sync_modal_shown_uid != self.selected_friend_uid
                        {
                            self.show_sync_modal = true;
                            self.sync_modal_shown_uid = self.selected_friend_uid.clone();
                        }
                    }
                }
                Err(e) => self.peer_mods_error = Some(e),
            }
        }
        if let Some(msg) = SOCIAL_MSG_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.social_msg = msg;
            // 传送/下载后刷新模组列表
            self.refresh();
        }
        // 批量补齐进度
        if let Some((prog, done, ok, fail)) = SYNC_PROGRESS_RESULT.get().and_then(|m| m.lock().unwrap().take()) {
            self.sync_progress = prog;
            if done {
                self.sync_busy = false;
                self.social_msg = format!("补齐完成：成功 {ok} 个，失败 {fail} 个");
                // 刷新本地模组列表
                self.refresh();
            }
        }

        // 联机大厅页才做发现刷新
        if self.page == Page::Social {
            if !self.friends_loaded {
                self.friends_loaded = true;
                self.friends = p2p::load_friends();
            }
            // 每 3 秒在后台拉取一次附近玩家列表
            if self.peers_at.elapsed() > Duration::from_secs(3) {
                self.peers_at = Instant::now();
                std::thread::spawn(|| {
                    let list = p2p::get_peers();
                    *PEERS_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(list);
                });
            }
            // 自动侦测：如果已上线且有在线好友但未选中，自动选第一个并拉取模组
            if self.social_online && !self.peer_mods_loading {
                let selected_online = self.peers.iter().find(|p| p.uid == self.selected_friend_uid);
                if selected_online.is_none() {
                    // 当前选中的不在线（或没选），选第一个在线好友
                    if let Some(p) = self.peers.first() {
                        self.selected_friend_uid = p.uid.clone();
                        self.selected_friend_ip = p.ip.clone();
                        self.peer_mods_loading = true;
                        let ip = p.ip.clone();
                        std::thread::spawn(move || {
                            let r = p2p::fetch_peer_mods(&ip).map_err(|e| e.to_string());
                            *PEER_MODS_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(r);
                        });
                    }
                }
            }
        }

        // 下载进度 / 倒计时需要持续重绘。
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    // ---------- 动作 ----------

    fn fetch_list(&mut self) {
        if self.settings.nexus_api_key.trim().is_empty() {
            self.list_error = Some("请先在「设置」页填写 N 网 API Key（免费账号即可）".to_string());
            return;
        }
        self.list_loading = true;
        self.list_error = None;
        let key = self.settings.nexus_api_key.trim().to_string();
        let lt = self.list_type.api_key_name().to_string();
        std::thread::spawn(move || {
            let r = web::nexus_mod_list(&key, &lt);
            *LIST_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(r);
        });
    }

    fn query_id(&mut self) {
        let Some(id) = web::parse_mod_id(&self.id_input) else {
            self.id_error = Some("请输入模组 ID 数字或 N 网模组链接".to_string());
            return;
        };
        if self.settings.nexus_api_key.trim().is_empty() {
            self.id_error = Some("请先在「设置」页填写 N 网 API Key".to_string());
            return;
        }
        self.id_loading = true;
        self.id_error = None;
        let key = self.settings.nexus_api_key.trim().to_string();
        std::thread::spawn(move || {
            let r = web::nexus_mod_info(&key, id);
            *ID_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some((id, r));
        });
    }

    /// 串行下载队列：一次只开一个文件页，等这个模组下载安装完成后
    /// 再自动开下一个，避免一次弹一堆标签页触发浏览器风控/验证。
    fn open_browser_downloads(&mut self, picks: Vec<(u32, String)>) {
        if picks.is_empty() {
            self.status = "请先勾选要下载的模组".to_string();
            return;
        }
        let Some(mods_dir) = self.env.mods_path.clone() else {
            self.status = "未找到 Mods 目录，请先在「设置」页指定游戏路径".to_string();
            return;
        };
        crate::watch::set_mods_dir(mods_dir);
        if !crate::watch::session_active() {
            crate::watch::start_session();
        }
        self.dl_total = picks.len();
        self.dl_queue = picks.into();
        self.dl_current = None;
        self.advance_queue();
    }

    fn queue_idle(&self) -> bool {
        self.dl_current.is_none() && self.dl_queue.is_empty()
    }

    /// 名称归一化：只保留字母/数字并转小写，用于已安装匹配。
    fn normalize_name(s: &str) -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    }

    /// 判断某个 N 网模组是否已安装（按名称与本地模组清单/文件夹名比对）。
    /// 规则：归一化后相等，或一者是另一者的前缀（前缀长度 ≥ 4），
    /// 这样「SMAPI 4.5.2」能匹配已装的「SMAPI」，「NPC Map Locations 2」
    /// 能匹配「NPCMapLocations」。
    fn is_installed_name(&self, nexus_name: &str) -> bool {
        let n = Self::normalize_name(nexus_name);
        if n.is_empty() {
            return false;
        }
        self.mods.iter().any(|m| {
            let folder = m
                .path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let name = m.name().to_string();
            [name, folder].iter().any(|c| {
                let c = Self::normalize_name(c);
                if c.is_empty() {
                    return false;
                }
                c == n
                    || (c.chars().count() >= 4 && n.starts_with(&c))
                    || (n.chars().count() >= 4 && c.starts_with(&n))
            })
        })
    }

    fn advance_queue(&mut self) {
        if self.dl_current.is_some() {
            return;
        }
        let Some((id, name)) = self.dl_queue.pop_front() else {
            return;
        };
        // 记录打开页面时已有的监控任务，之后出现的即视为本次下载。
        self.dl_baseline = self.watch_jobs.iter().map(|j| j.id).max().unwrap_or(0);
        crate::watch::open_external(&web::mod_files_tab_url(id));
        self.dl_current = Some((id, name.clone()));
        let idx = self.dl_total - self.dl_queue.len();
        self.status = format!(
            "下载队列 {idx}/{}：已打开「{name}」的文件页。请在网页点 Manual Download → Slow Download，\
             下载安装完成后自动开下一个；页面打不开/验证卡住就点「跳过此模组」",
            self.dl_total
        );
    }

    /// 当前文件页异常（验证过不去/下载失败）时跳到下一个。
    fn skip_current(&mut self) {
        if let Some((id, name)) = self.dl_current.take() {
            self.status = format!("已跳过「{name}」（ID {id}），可稍后在列表里单独重试");
            self.advance_queue();
        }
    }

    fn stop_queue(&mut self) {
        self.dl_queue.clear();
        self.dl_current = None;
        self.status = "下载队列已停止".to_string();
    }

    fn install_zip_picked(&mut self, path: PathBuf) {
        let Some(mods_dir) = self.env.mods_path.clone() else {
            self.zip_msg = "未找到 Mods 目录，请先在「设置」页指定游戏路径".to_string();
            return;
        };
        self.zip_msg = format!("正在安装 {} …", path.display());
        std::thread::spawn(move || {
            let r = installer::install_archive(&path, &mods_dir).map_err(|e| e.to_string());
            *ZIP_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(r);
        });
    }

    /// 判断镜像模组是否已安装：unique_id 精确匹配优先，否则名称归一化匹配。
    fn is_mirror_installed(&self, mm: &MirrorMod) -> bool {
        // 无 unique_id 时仅按归一化名精确匹配：前缀模糊会把
        // 「SDV-Radiance - KOR Translation」误判成 SDV-Radiance 已装。
        // 键集合在 refresh() 时预构建，这里 O(1) 查询。
        let uid = mm.unique_id.trim();
        if !uid.is_empty() && self.installed_uids.contains(&uid.to_uppercase()) {
            return true;
        }
        let n = Self::normalize_name(&mm.name);
        !n.is_empty() && self.installed_norms.contains(&n)
    }

    // ---------- 前置补齐 / 冲突检测 ----------

    /// 镜像清单 UniqueID（大写）→ 条目映射（清单已按人气降序，取首个命中）。
    fn mirror_uid_index(&self) -> HashMap<String, MirrorMod> {
        let mut map = HashMap::new();
        for m in &self.mirror_list {
            let uid = m.unique_id.trim().to_uppercase();
            if !uid.is_empty() {
                map.entry(uid).or_insert_with(|| m.clone());
            }
        }
        map
    }

    /// mm 的缺失必需前置（未安装且非 SMAPI 框架本身）。
    fn missing_deps_for(&self, mm: &MirrorMod, uid_idx: &HashMap<String, MirrorMod>) -> Vec<MissingDep> {
        let mut out = Vec::new();
        for d in &mm.deps {
            let uid = d.trim();
            if uid.is_empty() {
                continue;
            }
            let up = uid.to_uppercase();
            // SMAPI 是框架不是镜像模组，不算缺失前置。
            if up == "SMAPI" || up == "STARDEWMODDINGAPI" {
                continue;
            }
            if self.installed_uids.contains(&up) {
                continue;
            }
            out.push(MissingDep {
                uid: uid.to_string(),
                mirror: uid_idx.get(&up).cloned(),
            });
        }
        out
    }

    /// 已安装模组声明的冲突规则预编译（view 构建/弹窗检查共用）。
    fn installed_conf_regexes(&self) -> Vec<(regex::Regex, String)> {
        self.installed_confs
            .iter()
            .filter_map(|(p, owner)| conf_regex(p).map(|re| (re, owner.clone())))
            .collect()
    }

    /// mm 与已安装模组的冲突（双向）：
    /// 1) mm 的 Conflicts 正则命中任一已安装模组；
    /// 2) 任一已安装模组的 Conflicts 正则命中 mm 的 UniqueID。
    fn conflicts_for(&self, mm: &MirrorMod, installed_re: &[(regex::Regex, String)]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if !mm.confs.is_empty() {
            for m in &self.mods {
                let uid = m.unique_id();
                if mm
                    .confs
                    .iter()
                    .any(|c| conf_regex(c).is_some_and(|re| re.is_match(&uid)))
                {
                    let name = m.name();
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
        }
        let target = mm.unique_id.trim();
        if !target.is_empty() {
            for (re, owner) in installed_re {
                if re.is_match(target) && !out.contains(owner) {
                    out.push(owner.clone());
                }
            }
        }
        out
    }

    /// 直装入口：先检查缺前置/冲突；有缺失时弹窗确认（可一并补齐），否则直接装。
    fn install_with_check(&mut self, mm: MirrorMod) {
        if self.env.mods_path.is_none() {
            self.status = "未找到 Mods 目录，请先在「设置」页指定游戏路径".to_string();
            return;
        }
        let uid_idx = self.mirror_uid_index();
        let missing = self.missing_deps_for(&mm, &uid_idx);
        let installed_re = self.installed_conf_regexes();
        let conflicts = self.conflicts_for(&mm, &installed_re);
        if missing.is_empty() && conflicts.is_empty() {
            self.download_mirror(mm);
            return;
        }
        self.dep_modal = Some(DepModal {
            target: mm,
            missing,
            conflicts,
        });
    }

    /// 确保镜像清单的「已安装 mod_id」缓存最新（版本键命中则零开销）。
    /// 在 ui_library 每帧开头调用。
    fn ensure_installed_ids(&mut self) {
        let key = (self.mods_ver, self.mirror_list_ver);
        if self
            .mirror_installed_cache
            .as_ref()
            .is_some_and(|(mv, lv, _, _)| (*mv, *lv) == key)
        {
            return;
        }
        let mut set: HashSet<i64> = HashSet::new();
        let mut n = 0usize;
        for mm in &self.mirror_list {
            if self.is_mirror_installed(mm) {
                set.insert(mm.mod_id);
                n += 1;
            }
        }
        self.mirror_installed_cache = Some((self.mods_ver, self.mirror_list_ver, set, n));
    }

    /// 确保镜像库「过滤+排序」视图缓存最新（搜索词/分类/排序/数据未变时复用，
    /// 避免每帧对整个清单做克隆+过滤+排序）。
    fn ensure_mirror_view(&mut self) {
        let coll_sel = self.mirror_collection.clone();
        let sel = self.mirror_category.clone();
        let q = self.mirror_search.trim().to_lowercase();
        let sort = self.mirror_sort;
        if let Some(v) = &self.mirror_view {
            if v.coll_sel == coll_sel
                && v.sel == sel
                && v.q == q
                && v.sort == sort
                && v.list_ver == self.mirror_list_ver
                && v.colls_ver == self.mirror_colls_ver
                && v.mods_ver == self.mods_ver
            {
                return;
            }
        }

        // —— 分类统计（按数量降序）——
        let total_n = self.mirror_list.len();
        let mut cat_counts: HashMap<String, usize> = HashMap::new();
        for m in &self.mirror_list {
            let key = if m.category.is_empty() {
                "未分类".to_string()
            } else {
                m.category.clone()
            };
            *cat_counts.entry(key).or_default() += 1;
        }
        let mut cats: Vec<(String, usize)> = cat_counts.into_iter().collect();
        cats.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        // —— 热门合集统计（仅显示当前清单里有货的合集）——
        let mut coll_rows: Vec<(String, String, String, usize)> = Vec::new();
        for c in &self.mirror_colls {
            let set: HashSet<i64> = c.mods.iter().copied().collect();
            let n = self
                .mirror_list
                .iter()
                .filter(|m| set.contains(&m.mod_id))
                .count();
            if n > 0 {
                let tip = if c.desc.is_empty() {
                    c.en.clone()
                } else {
                    c.desc.clone()
                };
                coll_rows.push((c.id.clone(), c.zh.clone(), tip, n));
            }
        }

        // —— 合集 / 分类 + 关键字过滤 + 排序 ——
        let coll_set: HashSet<i64> = self
            .mirror_colls
            .iter()
            .find(|c| c.id == coll_sel)
            .map(|c| c.mods.iter().copied().collect())
            .unwrap_or_default();
        let mut list: Vec<MirrorMod> = self
            .mirror_list
            .iter()
            .filter(|m| {
                // 合集与分类互斥：选了合集就只看合集收录的 modId。
                let scope_ok = if !coll_sel.is_empty() {
                    coll_set.contains(&m.mod_id)
                } else {
                    sel.is_empty()
                        || (!m.category.is_empty() && m.category == sel)
                        || (m.category.is_empty() && sel == "未分类")
                };
                scope_ok
                    && (q.is_empty()
                        || m.name.to_lowercase().contains(&q)
                        || m.author.to_lowercase().contains(&q)
                        || m.description.to_lowercase().contains(&q)
                        || m.category_en.to_lowercase().contains(&q))
            })
            .cloned()
            .collect();
        match sort {
            1 => list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            2 => list.sort_by(|a, b| b.size.cmp(&a.size)),
            _ => {}
        }

        // —— 预计算每行的缺失前置/冲突警告（仅在清单/本地安装变化时算一次）——
        let uid_idx = self.mirror_uid_index();
        let installed_re = self.installed_conf_regexes();
        let warns: Vec<Option<RowWarn>> = list
            .iter()
            .map(|mm| {
                let missing = self.missing_deps_for(mm, &uid_idx);
                let conflicts = self.conflicts_for(mm, &installed_re);
                if missing.is_empty() && conflicts.is_empty() {
                    None
                } else {
                    Some(RowWarn { missing, conflicts })
                }
            })
            .collect();

        self.mirror_view = Some(MirrorView {
            coll_sel,
            sel,
            q,
            sort,
            list_ver: self.mirror_list_ver,
            colls_ver: self.mirror_colls_ver,
            mods_ver: self.mods_ver,
            total_n,
            cats,
            coll_rows,
            list,
            warns,
        });
    }

    /// 该镜像文件当前是否有任务（返回任务快照）。
    fn mirror_job_for(&self, file: &str) -> Option<&MJobSnapshot> {
        self.mirror_jobs.iter().find(|j| j.key == file)
    }

    /// 生效的镜像源地址：空串回退云端默认地址（兼容旧配置）。
    /// locked-mirror 发布版恒为默认服务器，忽略用户配置。
    fn effective_mirror_url(&self) -> String {
        if cfg!(feature = "locked-mirror") {
            return crate::model::DEFAULT_MIRROR_URL.to_string();
        }
        let b = self.settings.mirror_url.trim();
        if b.is_empty() {
            crate::model::DEFAULT_MIRROR_URL.to_string()
        } else {
            b.to_string()
        }
    }

    fn fetch_mirror(&mut self) {
        let base = self.effective_mirror_url();
        self.mirror_loading = true;
        self.mirror_error = None;
        std::thread::spawn(move || {
            let r = mirror::fetch_index(&base);
            // 合集是增强信息：拉取失败/旧镜像无此文件时静默降级为空列表。
            let colls = mirror::fetch_collections(&base).unwrap_or_default();
            *MIRROR_LIST_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(r);
            *MIRROR_COLLS_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() =
                Some(colls);
        });
    }

    fn download_mirror(&mut self, mm: MirrorMod) {
        let Some(mods_dir) = self.env.mods_path.clone() else {
            self.status = "未找到 Mods 目录，请先在「设置」页指定游戏路径".to_string();
            return;
        };
        mirror::set_mods_dir(mods_dir);
        mirror::download(self.effective_mirror_url(), mm);
    }

    fn install_smapi_bg(&mut self) {
        let Some(game_path) = self.env.game_path.clone() else {
            self.status = "未找到游戏目录，请先在「设置」页指定".to_string();
            return;
        };
        self.smapi_busy = true;
        self.status = "正在下载并安装 SMAPI…".to_string();
        std::thread::spawn(move || {
            let msg = match installer::install_smapi(&game_path) {
                Ok(m) => m,
                Err(e) => format!("SMAPI 安装失败：{e}"),
            };
            *SMAPI_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(msg);
        });
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // 清成半透明白：材质开启时 Mica 仍可透出，关闭时为白底；
        // 关键是杜绝缩放/拖拽过程中出现黑色闪烁。
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 210)
            .to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.theme_applied {
            liquid::install(&ctx);
            self.theme_applied = true;
        }
        // DWM 材质：启动初期多次重贴防竞态黑窗，重新聚焦时尝试恢复。
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        crate::glass::on_frame(
            crate::glass::WINDOW_TITLE,
            focused,
            self.start.elapsed().as_secs_f64(),
        );
        self.poll_background(&ctx);

        self.ui_title_bar(ui);
        self.ui_sidebar(ui);
        self.ui_status_bar(ui);

        egui::CentralPanel::default()
            .frame(liquid::panel_frame())
            .show(ui, |ui| {
                self.ui_header(ui);
                ui.separator();
                self.ui_global_progress(ui);
                match self.page {
                    Page::Host => self.ui_host_page(ui),
                    Page::Social => self.ui_social_page(ui),
                    Page::Download => self.ui_download_page(ui),
                    Page::Mods => self.ui_mods_page(ui),
                    Page::Settings => self.ui_settings_page(ui),
                }
            });

        // 灵动岛（下载指示器）：浮动在窗口顶部居中，不占布局空间。
        self.ui_dynamic_island(&ctx);

        // 模组同步弹窗（全局，不限于 Social 页）
        if self.show_sync_modal {
            let ctx = ui.ctx().clone();
            egui::Window::new("🔍 模组同步 — 检测到缺失模组")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .min_width(460.0)
                .show(&ctx, |ui| {
                    self.ui_sync_modal(ui);
                });
        }

        // 直装确认弹窗（缺前置 / 冲突提示，模组库点「⚡ 直装」时触发）
        if self.dep_modal.is_some() {
            let ctx = ui.ctx().clone();
            egui::Window::new("⚠ 安装前检查")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .min_width(440.0)
                .show(&ctx, |ui| {
                    self.ui_dep_modal(ui);
                });
        }

        // 模组详情弹窗（封面 + 中文长描述 BBCode 渲染）。
        if self.detail_modal.is_some() {
            self.ui_detail_modal(ui.ctx());
        }
    }
}

// ---------- 布局：标题栏 / 侧边栏 / 页眉 / 状态栏 ----------

impl App {
    fn ui_title_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("app_title_bar")
            .frame(liquid::title_frame())
            .exact_size(44.0)
            .show(ui, |ui| {
                // macOS 交通灯参数：命中区 26×26，圆点半径 6，三颗总宽约 70。
                let btn_size = egui::vec2(26.0, 26.0);
                let dot_r = 6.0_f32;
                let glyph = egui::Color32::from_rgba_unmultiplied(60, 60, 60, 220);

                // 绘制一颗交通灯：彩色圆点 + 悬停符号 + 按下反馈。
                let draw_light = |ui: &egui::Ui, rect: egui::Rect, color: egui::Color32, hovered: bool, down: bool, symbol: &str| {
                    let c = rect.center();
                    let r = if down { dot_r - 0.8 } else { dot_r };
                    let fill = if down {
                        egui::Color32::from_rgba_unmultiplied(
                            color.r().saturating_sub(30),
                            color.g().saturating_sub(30),
                            color.b().saturating_sub(30),
                            255,
                        )
                    } else {
                        color
                    };
                    ui.painter().circle_filled(c, r, fill);
                    ui.painter().circle_stroke(
                        c,
                        r,
                        egui::Stroke::new(0.5, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 30)),
                    );
                    if hovered {
                        ui.painter().text(
                            c,
                            egui::Align2::CENTER_CENTER,
                            symbol,
                            egui::FontId::proportional(12.0),
                            glyph,
                        );
                    }
                };

                // 左侧依次排列：关闭(红) → 最小化(黄) → 最大化(绿)，正宗 macOS 顺序。
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add_space(10.0);

                    // 关闭（红）。
                    let (close_rect, close_resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                    let down = close_resp.is_pointer_button_down_on();
                    draw_light(ui, close_rect, egui::Color32::from_rgb(0xFF, 0x5F, 0x57), close_resp.hovered(), down, "×");
                    if close_resp.clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }

                    // 最小化（黄）。
                    let (min_rect, min_resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                    let down = min_resp.is_pointer_button_down_on();
                    draw_light(ui, min_rect, egui::Color32::from_rgb(0xFE, 0xBC, 0x2E), min_resp.hovered(), down, "−");
                    if min_resp.clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }

                    // 最大化 / 还原（绿）。
                    let (max_rect, max_resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                    let down = max_resp.is_pointer_button_down_on();
                    let is_max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
                    let sym = if is_max { "⤢" } else { "+" };
                    draw_light(ui, max_rect, egui::Color32::from_rgb(0x28, 0xC8, 0x40), max_resp.hovered(), down, sym);
                    if max_resp.clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!is_max));
                    }

                    ui.add_space(12.0);

                    // 标题文字紧跟交通灯。
                    ui.painter().text(
                        egui::pos2(ui.cursor().left(), ui.max_rect().center().y),
                        egui::Align2::LEFT_CENTER,
                        "星露谷物语模组管理器",
                        egui::FontId::proportional(15.0),
                        liquid::text_main(),
                    );
                });

                // 整条标题栏（不含按钮）作为拖拽区；双击切换最大化。
                let bar_rect = ui.max_rect();
                let lights_right = bar_rect.left() + 10.0 + btn_size.x * 3.0 + 12.0;
                let drag_rect = egui::Rect::from_min_max(
                    egui::pos2(lights_right, bar_rect.top()),
                    bar_rect.max,
                );
                let drag = ui.interact(
                    drag_rect,
                    ui.id().with("title_bar_drag"),
                    egui::Sense::click_and_drag(),
                );
                if drag.drag_started() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    let is_max = ui
                        .ctx()
                        .input(|i| i.viewport().maximized.unwrap_or(false));
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Maximized(!is_max));
                }
                ui.allocate_space(egui::vec2(0.0, 4.0));
            });
    }

    fn ui_sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("sidebar")
            .resizable(false)
            .default_size(230.0)
            .frame(liquid::sidebar_frame())
            .show(ui, |ui| {
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("星露谷物语模组管理器").strong().size(18.0));
                        ui.label(
                            RichText::new("Stardew Mod Manager")
                                .size(12.0)
                                .color(liquid::text_dim()),
                        );
                    });
                });
                ui.add_space(18.0);

                let items = [
                    (Page::Host, "一键开服"),
                    (Page::Social, "联机大厅"),
                    (Page::Download, "模组库"),
                    (Page::Mods, "我的模组"),
                    (Page::Settings, "设置"),
                ];
                // 先为每个条目分配行 rect，并测量文字宽度（滑块宽度要随文字伸缩）。
                let row_h = 44.0_f32;
                let mut rows: Vec<(Page, egui::Rect, f32, egui::Response)> = Vec::new();
                for (p, label) in items {
                    let w = ui.available_width();
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(w, row_h), egui::Sense::click());
                    let text_w = ui
                        .painter()
                        .layout_no_wrap(
                            label.to_string(),
                            egui::FontId::proportional(16.0),
                            Color32::PLACEHOLDER,
                        )
                        .size()
                        .x;
                    rows.push((p, rect, text_w, resp));
                    ui.add_space(2.0);
                }
                // 选中条目内容框（图标+文字）就是滑块的目标位置/宽度。
                let target = rows
                    .iter()
                    .find(|(p, _, _, _)| *p == self.page)
                    .map(|(_, r, tw, _)| {
                        let h = 40.0_f32;
                        let w = 10.0 + 26.0 + 9.0 + tw + 16.0;
                        egui::Rect::from_min_size(
                            egui::pos2(r.left() + 4.0, r.center().y - h / 2.0),
                            egui::vec2(w, h),
                        )
                    })
                    .unwrap_or(rows[0].1);
                // 液态玻璃滑块在三个条目间上下滑动、按文字长短伸缩。
                liquid::nav_slider(ui, "main", target);

                let painter = ui.painter();
                for (i, (p, rect, _, resp)) in rows.iter().enumerate() {
                    let label = items[i].1;
                    let selected = *p == self.page;
                    if resp.clicked() {
                        self.page = *p;
                    }
                    // 未选中行的悬停反馈（滑块自身不需要 hover）。
                    if !selected && resp.hovered() {
                        painter.rect_filled(
                            rect.shrink2(egui::vec2(4.0, 2.0)),
                            egui::CornerRadius::same(14),
                            Color32::from_black_alpha(10),
                        );
                    }
                    let icon = egui::Rect::from_min_size(
                        egui::pos2(rect.left() + 14.0, rect.center().y - 13.0),
                        egui::vec2(26.0, 26.0),
                    );
                    // 选中：蓝玻璃图标 chip；未选中：中性灰 chip。
                    let (icon_bg, icon_fg) = if selected {
                        (liquid::PRIMARY_SOFT, liquid::PRIMARY)
                    } else {
                        (Color32::from_black_alpha(14), liquid::SLATE)
                    };
                    painter.rect_filled(icon, egui::CornerRadius::same(9), icon_bg);
                    painter.text(
                        icon.center(),
                        egui::Align2::CENTER_CENTER,
                        label.chars().next().unwrap_or('·').to_string(),
                        egui::FontId::proportional(13.0),
                        icon_fg,
                    );
                    painter.text(
                        egui::pos2(rect.left() + 49.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::FontId::proportional(16.0),
                        if selected {
                            liquid::PRIMARY
                        } else {
                            liquid::text_main()
                        },
                    );
                }

                ui.add_space(18.0);
                ui.separator();
                ui.add_space(10.0);
                ui.label(RichText::new("游戏状态").size(13.0).strong().color(liquid::text_dim()));
                if let Some(g) = &self.env.game_path {
                    ui.label(RichText::new("✅ 已找到星露谷").size(13.0).color(liquid::SUCCESS));
                    if let Some(p) = g.file_name() {
                        ui.label(
                            RichText::new(p.to_string_lossy().to_string())
                                .size(12.0)
                                .color(liquid::text_dim()),
                        );
                    }
                } else {
                    ui.label(RichText::new("⚠ 未找到游戏").size(13.0).color(liquid::WARNING));
                }
                match &self.env.smapi_version {
                    Some(v) => ui.label(RichText::new(format!("SMAPI {v}")).size(13.0)),
                    None => ui
                        .label(RichText::new("SMAPI 未安装").size(12.0).color(liquid::text_dim())),
                };
                let active = self.mirror_jobs.iter().filter(|j| j.is_active()).count();
                if active > 0 {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("⏳ {active} 个下载任务进行中"))
                            .size(12.0)
                            .color(liquid::SUCCESS),
                    );
                }
            });
    }

    fn ui_header(&mut self, ui: &mut egui::Ui) {
        let (title, right) = match self.page {
            Page::Host => ("一键开服", "刷新状态"),
            Page::Social => ("联机大厅", "刷新附近玩家"),
            Page::Download => ("模组库", "清空已结束任务"),
            Page::Mods => ("我的模组", "重新扫描"),
            Page::Settings => ("设置", ""),
        };
        let color = liquid::PRIMARY;
        ui.horizontal(|ui| {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(6.0, 24.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(3), color);
            ui.add_space(2.0);
            ui.label(RichText::new(title).size(21.0).strong().color(liquid::text_main()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !right.is_empty() && ui.button(right).clicked() {
                    match self.page {
                        Page::Host => {
                            self.saves_loaded = false;
                            self.probe_at = self.start;
                        }
                        Page::Social => {
                            self.peers_at = self.start;
                        }
                        Page::Download => {
                            crate::watch::clear_finished();
                            mirror::clear_finished();
                        }
                        Page::Mods => self.refresh(),
                        Page::Settings => {}
                    }
                }
            });
        });
    }

    fn ui_status_bar(&mut self, ui: &mut egui::Ui) {
        if self.status.is_empty() {
            return;
        }
        let status_color = if self.status.contains("失败") || self.status.contains("错误") {
            liquid::DANGER
        } else if self.status.contains("完成") || self.status.contains("成功") || self.status.contains("已安装") {
            liquid::SUCCESS
        } else if self.status.contains("正在") || self.status.contains("…") {
            liquid::WARNING
        } else {
            liquid::PRIMARY
        };
        egui::Panel::bottom("status_bar")
            .frame(liquid::status_frame())
            .exact_size(40.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(4.0, 15.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, egui::CornerRadius::same(2), status_color);
                    ui.label(RichText::new(&self.status).color(status_color));
                    if ui.small_button("关闭").clicked() {
                        self.status.clear();
                    }
                });
                ui.add_space(4.0);
            });
    }

    /// 全局下载进度条：任何页面只要有活跃任务都显示。
    fn ui_global_progress(&mut self, ui: &mut egui::Ui) {
        // 镜像直装：真实百分比进度条（可多个并行）。
        for job in self.mirror_jobs.iter().filter(|j| j.is_active()).cloned().collect::<Vec<_>>() {
            liquid::card_accent(ui, Some(liquid::PRIMARY), |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⚡").size(16.0).color(liquid::PRIMARY));
                    ui.label(RichText::new(&job.title).strong());
                    ui.label(
                        RichText::new(mirror_state_label(&job.state))
                            .size(12.0)
                            .color(liquid::PRIMARY),
                    );
                    if job.state == MJobState::Downloading {
                        if job.total > 0 {
                            ui.label(
                                RichText::new(format!(
                                    "{} / {} MB",
                                    job.done as f64 / 1048576.0,
                                    job.total as f64 / 1048576.0
                                ))
                                .size(12.0)
                                .weak(),
                            );
                        } else {
                            ui.label(
                                RichText::new(format!("{:.1} MB", job.done as f64 / 1048576.0))
                                    .size(12.0)
                                    .weak(),
                            );
                        }
                    }
                    ui.spinner();
                    if job.state == MJobState::Downloading
                        && ui.small_button("取消").clicked()
                    {
                        mirror::cancel_job(job.id);
                    }
                });
                if job.state == MJobState::Downloading {
                    if let Some(frac) = job.fraction() {
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .text(format!("{:.0}%", frac * 100.0))
                                .fill(liquid::PRIMARY),
                        );
                    } else {
                        ui.add(egui::ProgressBar::new(0.0).animate(true));
                    }
                }
            });
            ui.add_space(8.0);
        }
    }

    /// 灵动岛：苹果风格下载指示器。
    ///
    /// - 平时：黑色胶囊 + 绿色 ✓（空闲）/ 脉动绿点（下载中）/ 绿 ✓ 完成（3 秒）
    /// - 悬停：展开为圆角矩形，显示下载项名称、进度条、波形图、速度
    /// - 动画：lerp 收起 ↔ 展开
    fn ui_dynamic_island(&mut self, ctx: &egui::Context) {
        // 1. 状态判定
        let has_mirror_active = self.mirror_jobs.iter().any(|j| j.is_active());
        let has_nexus_active = self.dl_current.is_some() || !self.dl_queue.is_empty();
        let is_downloading = has_mirror_active || has_nexus_active;

        // 2. 状态转换：下载→空闲触发 Done 态显示 3 秒
        if !is_downloading && self.island_was_downloading {
            self.island_done_secs = 3.0;
            self.island_speeds.clear();
        }
        self.island_was_downloading = is_downloading;
        let dt = ctx.input(|i| i.unstable_dt);
        if self.island_done_secs > 0.0 {
            self.island_done_secs = (self.island_done_secs - dt).max(0.0);
        }

        // 3. 速度采样
        let now = Instant::now();
        let total_done: u64 = self
            .mirror_jobs
            .iter()
            .filter(|j| j.is_active())
            .map(|j| j.done)
            .sum();
        let elapsed = now.duration_since(self.island_last_time).as_secs_f32();
        if is_downloading && elapsed > 0.1 {
            let delta = total_done.saturating_sub(self.island_last_bytes);
            let kbps = delta as f32 / 1024.0 / elapsed.max(0.01);
            self.island_speeds.push(kbps);
            if self.island_speeds.len() > 60 {
                self.island_speeds.remove(0);
            }
            self.island_last_bytes = total_done;
            self.island_last_time = now;
        }

        // 4. 数据来源
        let active_job = self
            .mirror_jobs
            .iter()
            .find(|j| j.state == MJobState::Downloading)
            .or_else(|| self.mirror_jobs.iter().find(|j| j.is_active()));
        let current_title = active_job
            .map(|j| j.title.clone())
            .or_else(|| self.dl_current.as_ref().map(|(_, n)| n.clone()))
            .unwrap_or_default();
        let current_done = active_job.map(|j| j.done).unwrap_or(0);
        let current_total = active_job.map(|j| j.total).unwrap_or(0);
        let current_frac = active_job.and_then(|j| j.fraction());
        let speed_kbps = self.island_speeds.last().copied().unwrap_or(0.0);

        // 5. 尺寸（lerp 收起 ↔ 展开）
        // 收起态宽度按「左右内边距 + 图标占位 + 图文间距 + 实测文字宽」精确计算，
        // 让图标和文字作为一组水平居中；下载态/完成态文案等宽，切换时胶囊不缩放。
        let show_done = self.island_done_secs > 0.0;
        let label = if is_downloading {
            "下载"
        } else if show_done {
            "完成"
        } else {
            ""
        };
        let label_font = egui::FontId::proportional(12.0);
        // 「下载」「完成」均为两个中文字，12px 字号下宽度即 24px。
        let text_w = if label.is_empty() { 0.0 } else { 24.0 };
        const PAD_X: f32 = 14.0;
        const ICON_SPAN: f32 = 14.0; // 脉动外圈最大直径
        const ICON_GAP: f32 = 8.0;
        let collapsed_w = if label.is_empty() {
            PAD_X * 2.0 + ICON_SPAN
        } else {
            PAD_X * 2.0 + ICON_SPAN + ICON_GAP + text_w
        };
        let collapsed_h = 28.0_f32;
        let expanded_w = 340.0_f32;
        let expanded_h = 156.0_f32;
        let eased = self.island_expand;
        let w = collapsed_w + (expanded_w - collapsed_w) * eased;
        let h = collapsed_h + (expanded_h - collapsed_h) * eased;

        egui::Area::new(egui::Id::new("dynamic_island"))
            .anchor(egui::Align2::CENTER_TOP, [0.0, 50.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
                let hovered = response.hovered();
                // 目标展开度（下一帧生效）
                let target = if hovered { 1.0 } else { 0.0 };
                self.island_expand += (target - self.island_expand) * 0.18;
                // 动画期间必须预订下一帧（脉动/展开/完成倒计时都按 60fps 走），
                // 否则只有鼠标事件和 500ms 轮询才触发重绘，动画会一顿一顿。
                if is_downloading
                    || self.island_done_secs > 0.0
                    || (target - self.island_expand).abs() > 0.002
                {
                    ctx.request_repaint_after(Duration::from_millis(16));
                }

                let painter = ui.painter();
                let corner_r = (h * 0.5).min(18.0) as u8;
                painter.rect_filled(
                    rect,
                    egui::CornerRadius::same(corner_r),
                    egui::Color32::BLACK,
                );

                let ca = ((1.0 - self.island_expand).clamp(0.0, 1.0) * 255.0) as u8;
                let ea = (self.island_expand.clamp(0.0, 1.0) * 255.0) as u8;
                let center = rect.center();
                let dot_r = 5.5_f32;
                let green = |a: u8| egui::Color32::from_rgba_unmultiplied(0x34, 0xC7, 0x59, a);
                let white = |a: u8| egui::Color32::from_rgba_unmultiplied(255, 255, 255, a);

                // —— 收起态内容（alpha = 1 - expand）——
                // 图标与文字作为一组水平居中（坐标相对锚点中心，展开动画时不漂移）。
                let (icon_x, text_x) = if label.is_empty() {
                    (center.x, center.x)
                } else {
                    let content_left = center.x - collapsed_w / 2.0 + PAD_X;
                    (
                        content_left + ICON_SPAN / 2.0,
                        content_left + ICON_SPAN + ICON_GAP + text_w / 2.0,
                    )
                };
                // 手绘对勾：自定义字体缺 ✓ 字形（会渲染成豆腐块），直接画两段白线。
                let draw_check = |c: egui::Pos2, r: f32, col: Color32| {
                    let stroke = egui::Stroke::new(1.6, col);
                    painter.line_segment(
                        [
                            egui::pos2(c.x - r * 0.38, c.y + r * 0.04),
                            egui::pos2(c.x - r * 0.10, c.y + r * 0.34),
                        ],
                        stroke,
                    );
                    painter.line_segment(
                        [
                            egui::pos2(c.x - r * 0.10, c.y + r * 0.34),
                            egui::pos2(c.x + r * 0.42, c.y - r * 0.32),
                        ],
                        stroke,
                    );
                };
                if is_downloading {
                    // 脉动绿点 + "下载" 文字
                    let t = ctx.input(|i| i.time);
                    let pulse = (0.5 + 0.5 * (t * 4.0).sin()) as f32;
                    let a = (ca as f32 * (0.45 + 0.55 * pulse)) as u8;
                    painter.circle_filled(egui::pos2(icon_x, center.y), dot_r + 1.5 * pulse, green(a));
                    painter.text(
                        egui::pos2(text_x, center.y),
                        egui::Align2::CENTER_CENTER,
                        label,
                        label_font.clone(),
                        white(ca),
                    );
                } else if show_done {
                    // 绿 ✓ + "完成"
                    let c = egui::pos2(icon_x, center.y);
                    painter.circle_filled(c, dot_r, green(ca));
                    draw_check(c, dot_r, white(ca));
                    painter.text(
                        egui::pos2(text_x, center.y),
                        egui::Align2::CENTER_CENTER,
                        label,
                        label_font.clone(),
                        white(ca),
                    );
                } else {
                    // 空闲：绿色圆 + 白 ✓
                    painter.circle_filled(center, dot_r, green(ca));
                    draw_check(center, dot_r, white(ca));
                }

                // —— 展开态内容（alpha = expand）——
                if ea > 5 {
                    let pad = 14.0;
                    let body = rect.shrink2(egui::vec2(pad, pad));

                    let title = if is_downloading {
                        "正在下载"
                    } else {
                        "所有内容已下载"
                    };
                    painter.text(
                        body.left_top(),
                        egui::Align2::LEFT_TOP,
                        title,
                        egui::FontId::proportional(13.0),
                        white(ea),
                    );

                    if is_downloading {
                        // 当前下载项名称
                        let name_color =
                            egui::Color32::from_rgba_unmultiplied(220, 220, 220, ea);
                        let name_pos = egui::pos2(body.left(), body.top() + 22.0);
                        let name = if current_title.chars().count() > 28 {
                            let head: String = current_title.chars().take(25).collect();
                            format!("{head}…")
                        } else {
                            current_title.clone()
                        };
                        painter.text(
                            name_pos,
                            egui::Align2::LEFT_TOP,
                            &name,
                            egui::FontId::proportional(11.0),
                            name_color,
                        );

                        // 波形图（先绘制，进度条在最底部）
                        let wave_top = body.top() + 44.0;
                        let wave_bottom = body.bottom() - 28.0;
                        let wave_h = (wave_bottom - wave_top).max(8.0);
                        let wave_w = body.width();
                        let bar_count = 40_usize;
                        let bar_w = wave_w / bar_count as f32;
                        let max_speed = self
                            .island_speeds
                            .iter()
                            .cloned()
                            .fold(1.0_f32, f32::max)
                            .max(1.0);
                        let sample_count = self.island_speeds.len();
                        let start = sample_count.saturating_sub(bar_count);
                        for i in 0..bar_count {
                            let idx = start + i;
                            let v = if idx < sample_count {
                                self.island_speeds[idx]
                            } else {
                                0.0
                            };
                            let bh = (v / max_speed).clamp(0.0, 1.0) * wave_h;
                            let bx = body.left() + i as f32 * bar_w;
                            let bar_rect = egui::Rect::from_min_max(
                                egui::pos2(bx + 1.0, wave_bottom - bh),
                                egui::pos2(bx + bar_w - 1.0, wave_bottom),
                            );
                            let alpha = (ea as f32 * 0.85) as u8;
                            painter.rect_filled(
                                bar_rect,
                                egui::CornerRadius::same(1),
                                egui::Color32::from_rgba_unmultiplied(0x34, 0xC7, 0x59, alpha),
                            );
                        }

                        // 进度条
                        let bar_y = body.bottom() - 14.0;
                        let bar_rect = egui::Rect::from_min_max(
                            egui::pos2(body.left(), bar_y),
                            egui::pos2(body.right(), bar_y + 6.0),
                        );
                        let bg_a = (ea as f32 * 0.18) as u8;
                        painter.rect_filled(
                            bar_rect,
                            egui::CornerRadius::same(3),
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, bg_a),
                        );
                        let frac = current_frac.unwrap_or(0.0);
                        let fill_w = bar_rect.width() * frac;
                        let fill_rect = egui::Rect::from_min_max(
                            bar_rect.min,
                            egui::pos2(bar_rect.left() + fill_w, bar_rect.bottom()),
                        );
                        painter.rect_filled(
                            fill_rect,
                            egui::CornerRadius::same(3),
                            green(ea),
                        );

                        // 进度文字 + 速度
                        let info_color =
                            egui::Color32::from_rgba_unmultiplied(200, 200, 200, ea);
                        let info_text = if current_total > 0 {
                            format!(
                                "{:.1}MB / {:.1}MB  {:.0}%  {:.0} KB/s",
                                current_done as f64 / 1048576.0,
                                current_total as f64 / 1048576.0,
                                frac * 100.0,
                                speed_kbps
                            )
                        } else {
                            format!(
                                "{:.1}MB  {:.0} KB/s",
                                current_done as f64 / 1048576.0,
                                speed_kbps
                            )
                        };
                        painter.text(
                            egui::pos2(body.left(), bar_y - 4.0),
                            egui::Align2::LEFT_BOTTOM,
                            &info_text,
                            egui::FontId::proportional(10.0),
                            info_color,
                        );
                    } else {
                        // 完成 / 空闲态：显示统计信息
                        let n = self.mirror_ok_seen.len();
                        let msg_color =
                            egui::Color32::from_rgba_unmultiplied(180, 220, 180, ea);
                        painter.text(
                            egui::pos2(body.left(), body.top() + 26.0),
                            egui::Align2::LEFT_TOP,
                            format!("今日已安装 {n} 个模组"),
                            egui::FontId::proportional(11.0),
                            msg_color,
                        );
                    }
                }
            });
    }
}

// ---------- 下载模组页 ----------

impl App {
    fn ui_download_page(&mut self, ui: &mut egui::Ui) {
        if self.mirror_loaded {
            self.ui_library(ui);
        } else {
            self.ui_library_gate(ui);
        }
    }

    /// 首次进入的欢迎页：拉取镜像清单后进入模组库。
    fn ui_library_gate(&mut self, ui: &mut egui::Ui) {
        ui.add_space((ui.available_height() * 0.15).max(24.0));
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("⚡").size(48.0).color(liquid::PRIMARY));
            ui.add_space(10.0);
            ui.label(
                RichText::new("模组库")
                    .size(26.0)
                    .strong()
                    .color(liquid::text_main()),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new("镜像直装 · 免浏览器 · 真实下载进度，一键装进 Mods")
                    .size(13.0)
                    .color(liquid::text_dim()),
            );
            ui.add_space(2.0);
            ui.label(
                RichText::new(format!("镜像源：{}", self.effective_mirror_url()))
                    .size(11.5)
                    .weak(),
            );
            if let Some(e) = &self.mirror_error {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("⚠ {e}")).color(liquid::DANGER));
            }
            ui.add_space(16.0);
            ui.allocate_ui_with_layout(
                egui::vec2(240.0, 36.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    if self.mirror_loading {
                        ui.spinner();
                        ui.add_space(6.0);
                        ui.label(RichText::new("正在拉取清单…").color(liquid::PRIMARY));
                    } else if liquid::cta_button(ui, "⚡ 拉取镜像清单", true).clicked() {
                        self.fetch_mirror();
                    }
                },
            );
        });
    }

    /// 模组库专属布局：顶部统计栏 + 左侧分类栏 + 右侧全高模组列表。
    /// 统计与过滤走缓存；列表按等高行虚拟化，几千条清单也只布局视口内十几行。
    fn ui_library(&mut self, ui: &mut egui::Ui) {
        self.ensure_installed_ids();
        self.ensure_mirror_view();
        // 视图临时 take 出来：下面的交互闭包需要 &mut self，避免借用冲突。
        let mut view = self.mirror_view.take().unwrap();
        let installed_n = self
            .mirror_installed_cache
            .as_ref()
            .map(|(_, _, _, n)| *n)
            .unwrap_or(0);

        // —— 顶部统计栏 ——
        let total_n = view.total_n;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("共 {total_n} 个模组 · 已安装 {installed_n} 个"))
                    .size(12.5)
                    .color(liquid::text_dim()),
            );
            if self.mirror_loading {
                ui.spinner();
            }
            if let Some(e) = &self.mirror_error {
                ui.label(
                    RichText::new(format!("⚠ {e}"))
                        .size(12.0)
                        .color(liquid::DANGER),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if liquid::cta_button(ui, "↻ 刷新清单", !self.mirror_loading).clicked() {
                    self.fetch_mirror();
                }
            });
        });
        ui.add_space(10.0);

        let coll_sel = view.coll_sel.clone();
        let sel = view.sel.clone();
        let sort = view.sort;
        let shown_n = view.list.len();
        let coll_sel_name = view
            .coll_rows
            .iter()
            .find(|(id, _, _, _)| *id == coll_sel)
            .map(|(_, zh, _, _)| zh.clone())
            .unwrap_or_default();

        // —— 两栏主体（占满剩余高度，各自独立滚动）——
        let body_h = ui.available_height();
        let rail_w = 192.0_f32;
        let mut action = MirrorRowAction::None;
        ui.horizontal(|ui| {
            // ===== 左侧：搜索 + 分类栏 =====
            ui.allocate_ui_with_layout(
                egui::vec2(rail_w, body_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    liquid::card(ui, |ui| {
                        ui.set_min_height(body_h - 2.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.mirror_search)
                                .hint_text("搜索模组 / 作者 / 描述")
                                .desired_width(rail_w - 32.0),
                        );
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .max_height(ui.available_height())
                            .show(ui, |ui| {
                                    // —— 热门合集（跨分类的精选标签）——
                                    if !view.coll_rows.is_empty() {
                                        ui.label(
                                            RichText::new("✨ 热门合集")
                                                .size(11.0)
                                                .color(liquid::text_dim()),
                                        );
                                        ui.add_space(4.0);
                                        for (id, zh, tip, n) in &view.coll_rows {
                                            let on = coll_sel == *id;
                                            let resp = mirror_cat_row(
                                                ui,
                                                &format!("{zh}（{n}）"),
                                                on,
                                            )
                                            .on_hover_text(tip);
                                            if resp.clicked() {
                                                if on {
                                                    self.mirror_collection.clear();
                                                } else {
                                                    self.mirror_collection = id.clone();
                                                    self.mirror_category.clear();
                                                }
                                            }
                                        }
                                        ui.add_space(8.0);
                                        ui.separator();
                                        ui.add_space(6.0);
                                    }
                                    // —— Nexus 官方分类 ——
                                    ui.label(
                                        RichText::new("全部分类")
                                            .size(11.0)
                                            .color(liquid::text_dim()),
                                    );
                                    ui.add_space(4.0);
                                    if mirror_cat_row(
                                        ui,
                                        &format!("全部（{total_n}）"),
                                        sel.is_empty() && coll_sel.is_empty(),
                                    )
                                    .clicked()
                                    {
                                        self.mirror_category.clear();
                                        self.mirror_collection.clear();
                                    }
                                    for (name, n) in &view.cats {
                                        let on = sel == *name;
                                        if mirror_cat_row(
                                            ui,
                                            &format!("{name}（{n}）"),
                                            on,
                                        )
                                        .clicked()
                                        {
                                            if on {
                                                self.mirror_category.clear();
                                            } else {
                                                self.mirror_category = name.clone();
                                                self.mirror_collection.clear();
                                            }
                                        }
                                    }
                                }
                            );
                    });
                },
            );
            ui.add_space(10.0);

            // ===== 右侧：排序工具条 + 模组列表 =====
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), body_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.horizontal(|ui| {
                        if !coll_sel.is_empty() {
                            ui.label(
                                RichText::new(format!("✨ {coll_sel_name}"))
                                    .size(12.0)
                                    .color(liquid::PRIMARY),
                            );
                            ui.label(
                                RichText::new("·")
                                    .size(12.0)
                                    .color(liquid::text_dim()),
                            );
                        }
                        ui.label(
                            RichText::new(format!("当前显示 {shown_n} 个"))
                                .size(12.0)
                                .color(liquid::text_dim()),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                egui::ComboBox::from_id_salt("mirror_sort")
                                    .selected_text(match sort {
                                        1 => "名称 A-Z",
                                        2 => "体积从大到小",
                                        _ => "人气最高",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut self.mirror_sort,
                                            0u8,
                                            "人气最高",
                                        );
                                        ui.selectable_value(
                                            &mut self.mirror_sort,
                                            1u8,
                                            "名称 A-Z",
                                        );
                                        ui.selectable_value(
                                            &mut self.mirror_sort,
                                            2u8,
                                            "体积从大到小",
                                        );
                                    });
                                ui.label(
                                    RichText::new("排序：")
                                        .size(12.0)
                                        .color(liquid::text_dim()),
                                );
                            },
                        );
                    });
                    ui.add_space(6.0);
                    if view.list.is_empty() {
                        ui.add_space(28.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new("🔍").size(28.0));
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new("没有符合条件的模组，换个分类或关键字试试")
                                    .size(13.0)
                                    .color(liquid::text_dim()),
                            );
                        });
                    } else {
                        // 虚拟化：只布局视口覆盖的行（show_rows 自动补 item_spacing）。
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show_rows(ui, MIRROR_ROW_H, view.list.len(), |ui, range| {
                                for i in range {
                                    let warn = view.warns.get(i).and_then(|w| w.as_ref());
                                    match self.mirror_mod_row(ui, &view.list[i], warn) {
                                        MirrorRowAction::Install(mm) => {
                                            action = MirrorRowAction::Install(mm);
                                        }
                                        MirrorRowAction::Cancel(id) => {
                                            action = MirrorRowAction::Cancel(id);
                                        }
                                        MirrorRowAction::OpenDetail(mm, dep, conf) => {
                                            action = MirrorRowAction::OpenDetail(mm, dep, conf);
                                        }
                                        MirrorRowAction::None => {}
                                    }
                                }
                            });
                    }
                },
            );
        });

        match action {
            MirrorRowAction::Install(mm) => self.install_with_check(mm),
            MirrorRowAction::Cancel(id) => mirror::cancel_job(id),
            MirrorRowAction::OpenDetail(mm, dep, conf) => {
                self.detail_modal = Some((mm, dep, conf));
            }
            MirrorRowAction::None => {}
        }
        self.mirror_view = Some(view);
    }

    /// 模组库中的一张模组卡（等高行，供 show_rows 虚拟化）。
    /// 第三行按任务状态显示进度/结果（替代描述行），保证所有行高度一致。
    /// 无任务时优先显示缺失前置（橙）/冲突（红）警告，其次描述。
    fn mirror_mod_row(
        &self,
        ui: &mut egui::Ui,
        mm: &MirrorMod,
        warn: Option<&RowWarn>,
    ) -> MirrorRowAction {
        let mut action = MirrorRowAction::None;
        let job = self.mirror_job_for(&mm.file);
        let busy = job.is_some_and(|j| j.is_active());
        liquid::card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // 左：封面 + 文本信息，整块可点击打开详情。
                let info_w = (ui.available_width() - 118.0).max(0.0);
                let inner = ui.allocate_ui_with_layout(
                    egui::vec2(info_w, ui.available_height()),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        self.mirror_thumb(ui, mm, egui::vec2(94.0, 70.0));
                        ui.add_space(10.0);
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                let title = if mm.name_zh.is_empty() {
                                    mm.name.clone()
                                } else {
                                    mm.name_zh.clone()
                                };
                                ui.add(
                                    egui::Label::new(RichText::new(title).strong()).truncate(),
                                );
                                if !mm.version.is_empty() {
                                    liquid::pill(
                                        ui,
                                        &format!("v{}", mm.version),
                                        liquid::SKY,
                                        liquid::SKY_SOFT,
                                    );
                                }
                                liquid::pill(
                                    ui,
                                    &format!("{:.1} MB", mm.size as f64 / 1048576.0),
                                    liquid::SLATE,
                                    liquid::SLATE_SOFT,
                                );
                                if !mm.category.is_empty() {
                                    let resp = liquid::pill(
                                        ui,
                                        &mm.category,
                                        liquid::VIOLET,
                                        liquid::VIOLET_SOFT,
                                    );
                                    if !mm.category_en.is_empty() {
                                        resp.on_hover_text(mm.category_en.clone());
                                    }
                                }
                                if self.is_mirror_installed(mm) {
                                    liquid::pill(
                                        ui,
                                        "✅ 已安装",
                                        liquid::SUCCESS,
                                        liquid::SUCCESS_SOFT,
                                    );
                                }
                                // 摘要标记：有警告时第一行就能看到。
                                if let Some(w) = warn {
                                    if !w.conflicts.is_empty() {
                                        liquid::pill(
                                            ui,
                                            "✗ 冲突",
                                            liquid::DANGER,
                                            liquid::DANGER_SOFT,
                                        );
                                    }
                                    if !w.missing.is_empty() {
                                        liquid::pill(
                                            ui,
                                            "⚠ 缺前置",
                                            liquid::WARNING,
                                            liquid::WARNING_SOFT,
                                        );
                                    }
                                }
                            });
                            // 第二行：作者；有中文译名时附上英文原名。
                            let sub = if mm.name_zh.is_empty() {
                                format!("作者：{}", mm.author)
                            } else {
                                format!("作者：{}  ·  {}", mm.author, mm.name)
                            };
                            ui.add(
                                egui::Label::new(RichText::new(sub).size(11.0).weak())
                                    .truncate(),
                            );
                            // 第三行：任务状态优先，其次警告（冲突 > 缺前置），无警告显示简介。
                            match job {
                                Some(j) => match &j.state {
                                    MJobState::Downloading => {
                                        ui.horizontal(|ui| {
                                            let text = match j.fraction() {
                                                Some(f) => {
                                                    format!("⏳ 下载中 {:.0}%", f * 100.0)
                                                }
                                                None => "⏳ 下载中…".to_string(),
                                            };
                                            ui.label(
                                                RichText::new(text)
                                                    .size(11.0)
                                                    .color(liquid::PRIMARY),
                                            );
                                            if ui
                                                .add(egui::Link::new(
                                                    RichText::new("取消").size(11.0),
                                                ))
                                                .clicked()
                                            {
                                                action = MirrorRowAction::Cancel(j.id);
                                            }
                                        });
                                    }
                                    MJobState::Installing => {
                                        ui.label(
                                            RichText::new("正在安装到 Mods…")
                                                .size(11.0)
                                                .color(liquid::WARNING),
                                        );
                                    }
                                    MJobState::Finished(ok, detail) => {
                                        ui.label(
                                            RichText::new(detail).size(11.0).color(if *ok {
                                                liquid::SUCCESS
                                            } else {
                                                liquid::DANGER
                                            }),
                                        );
                                    }
                                },
                                None => {
                                    let mut tip = String::new();
                                    if let Some(w) = warn {
                                        if !w.conflicts.is_empty() {
                                            tip = format!("与{}冲突", w.conflicts.join("、"));
                                            ui.label(
                                                RichText::new(format!("⛔ {tip}"))
                                                    .size(11.0)
                                                    .color(liquid::DANGER),
                                            );
                                        } else if !w.missing.is_empty() {
                                            let names: Vec<String> = w
                                                .missing
                                                .iter()
                                                .map(|d| d.display())
                                                .collect();
                                            tip = format!("缺少前置：{}", names.join("、"));
                                            ui.label(
                                                RichText::new(format!("⚠ {tip}"))
                                                    .size(11.0)
                                                    .color(liquid::WARNING),
                                            );
                                        }
                                        if !w.conflicts.is_empty() && !w.missing.is_empty() {
                                            let names: Vec<String> = w
                                                .missing
                                                .iter()
                                                .map(|d| d.display())
                                                .collect();
                                            tip = format!(
                                                "{}；缺少前置：{}",
                                                tip,
                                                names.join("、")
                                            );
                                        }
                                    }
                                    if tip.is_empty() {
                                        // 简介：中文一句话 > 英文一句话 > manifest 描述。
                                        let brief = if !mm.summary_zh.is_empty() {
                                            mm.summary_zh.as_str()
                                        } else if !mm.summary.is_empty() {
                                            mm.summary.as_str()
                                        } else {
                                            mm.description.as_str()
                                        };
                                        if !brief.is_empty() {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(brief)
                                                        .size(11.5)
                                                        .color(liquid::text_dim()),
                                                )
                                                .truncate(),
                                            );
                                        } else {
                                            // 空占位：保持行高一致（虚拟化要求等高）。
                                            ui.label(RichText::new(" ").size(11.5));
                                        }
                                    } else {
                                        // 警告被截断时悬停看全。
                                        ui.response().on_hover_text(tip);
                                    }
                                }
                            }
                        });
                    },
                );
                // 文本/封面区点击 → 详情（点了行内「取消」时不触发）。
                let resp = inner
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text("点击查看模组详情");
                if resp.clicked() && !matches!(action, MirrorRowAction::Cancel(_)) {
                    let (missing, conflicts) = match warn {
                        Some(w) => (w.missing.clone(), w.conflicts.clone()),
                        None => (Vec::new(), Vec::new()),
                    };
                    action = MirrorRowAction::OpenDetail(mm.clone(), missing, conflicts);
                }
                // 右：操作按钮（在可点区之外，不会误触详情）。
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if busy {
                        match job.and_then(|j| j.fraction()) {
                            Some(f) => format!("⏳ {:.0}%", f * 100.0),
                            None => "⏳…".to_string(),
                        }
                    } else if self.is_mirror_installed(mm) {
                        "重新下载".to_string()
                    } else {
                        "⚡ 直装".to_string()
                    };
                    if liquid::cta_button(ui, label, !busy).clicked() {
                        action = MirrorRowAction::Install(mm.clone());
                    }
                });
            });
        });
        action
    }

    /// 模组封面缩略图（94×70 行卡 / 176×132 详情窗共用）；无图或失败时占位。
    fn mirror_thumb(&self, ui: &mut egui::Ui, mm: &MirrorMod, size: egui::Vec2) {
        let rounding = egui::CornerRadius::same(8);
        let placeholder = |ui: &mut egui::Ui, glyph: &str| {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, rounding, liquid::SLATE_SOFT);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                glyph,
                egui::FontId::proportional(size.y * 0.4),
                liquid::text_dim(),
            );
        };
        if mm.thumb.is_empty() {
            placeholder(ui, "🖼");
            return;
        }
        match imgcache::request(ui.ctx(), &self.effective_mirror_url(), &mm.thumb) {
            imgcache::ImgState::Ready(tex) => {
                ui.add(
                    egui::Image::from_texture(&tex)
                        .fit_to_exact_size(size)
                        .corner_radius(rounding),
                );
            }
            imgcache::ImgState::Loading => placeholder(ui, "⏳"),
            imgcache::ImgState::Failed => placeholder(ui, "🖼"),
        }
    }

    /// 使用说明卡：浏览器登录 + 下载方法。
    fn ui_guide_card(&mut self, ui: &mut egui::Ui) {
        let login_done = self.settings.nexus_login_done;
        liquid::card_accent(ui, Some(liquid::PRIMARY), |ui| {
            ui.set_width(ui.available_width());
            liquid::section_title(ui, "使用方法（浏览器下载，全程免配置）", liquid::PRIMARY);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if login_done {
                    ui.label(RichText::new("✅ ① 已在系统浏览器登录 N 网").color(liquid::SUCCESS));
                } else {
                    ui.label(
                        RichText::new(
                            "① 在系统浏览器登录 N 网（免费账号即可），完成后勾选右侧",
                        )
                        .size(12.0),
                    );
                }
                if ui.button("在浏览器登录 N 网").clicked() {
                    crate::watch::open_external("https://users.nexusmods.com/");
                }
                let mut done = login_done;
                if ui.checkbox(&mut done, "我已登录").changed() {
                    self.settings.nexus_login_done = done;
                    let _ = self.settings.save();
                }
            });
            ui.label(
                RichText::new(
                    "② 点模组的「🌐 浏览器下载」→ 在打开的网页点 Manual Download → Slow Download（免费账号可用）\
                     → 浏览器开始下载后剩下的交给本程序：自动监控「下载」文件夹、显示进度、下载完自动安装到 Mods。\
                     批量勾选时自动逐个下载：一个安装完成后再开下一个页面，避免触发浏览器风控。",
                )
                .size(12.0),
            );
            ui.label(
                RichText::new(
                    "注意：请点 Manual Download / Slow Download，不要点 Mod Manager Download（那是给 Vortex 等管理器用的）；\
                     下载的模组 zip 安装成功后会被自动清理。",
                )
                .size(11.0)
                .weak(),
            );
        });
    }

    /// N 网模组浏览 + 勾选 + 浏览器下载 + 按 ID 查询。
    fn ui_browse_card(&mut self, ui: &mut egui::Ui) {
        liquid::card_accent(ui, Some(liquid::PRIMARY), |ui| {
            ui.set_width(ui.available_width());
            liquid::section_title(ui, "N 网模组库", liquid::PRIMARY);

            // 列表类型切换（液态玻璃分段） + 拉取
            ui.horizontal(|ui| {
                let tabs: Vec<(NexusListType, &str)> =
                    NexusListType::all().iter().map(|t| (*t, t.label())).collect();
                if let Some(t) =
                    liquid::segmented(ui, "nexus_list_type", &tabs, self.list_type)
                {
                    if t != self.list_type {
                        self.list_type = t;
                        self.list_loaded = false;
                        self.fetch_list();
                    }
                }
                if self.list_loading {
                    ui.spinner();
                } else if liquid::cta_button(
                    ui,
                    if self.list_loaded { "刷新列表" } else { "拉取模组列表" },
                    true,
                )
                .clicked()
                {
                    self.fetch_list();
                }
            });

            if let Some(e) = &self.list_error {
                ui.label(RichText::new(format!("⚠ {e}")).color(liquid::DANGER));
            }

            // 按 ID / 链接查询
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("按 ID/链接：");
                ui.add(
                    egui::TextEdit::singleline(&mut self.id_input)
                        .hint_text("如 2400 或模组页面链接")
                        .desired_width(280.0),
                );
                if liquid::cta_button(ui, "查询", !self.id_loading).clicked() {
                    self.query_id();
                }
                if self.id_loading {
                    ui.spinner();
                }
            });
            if let Some(e) = &self.id_error {
                ui.label(RichText::new(e).size(11.0).color(liquid::DANGER));
            }
            if let Some(m) = self.id_result.clone() {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} · ID {} · 作者 {} · v{}",
                                    m.name, m.mod_id, m.author, m.latest_version
                                ))
                                .strong(),
                            );
                            if self.is_installed_name(&m.name) {
                                liquid::pill(
                                    ui,
                                    "✅ 已安装",
                                    liquid::SUCCESS,
                                    liquid::SUCCESS_SOFT,
                                );
                            }
                        });
                        if !m.summary.is_empty() {
                            ui.label(RichText::new(&m.summary).size(11.0).weak());
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if liquid::cta_button(ui, "🌐 浏览器下载", self.queue_idle()).clicked() {
                            self.open_browser_downloads(vec![(m.mod_id, m.name.clone())]);
                        }
                    });
                });
            }

            // 勾选操作条
            if self.list_loaded && !self.nexus_list.is_empty() {
                ui.separator();
                let total = self.nexus_list.len();
                let n_sel = self
                    .nexus_list
                    .iter()
                    .filter(|m| self.selected.contains(&m.mod_id))
                    .count();
                ui.horizontal(|ui| {
                    if ui
                        .small_button("全选")
                        .on_hover_text("只勾选未安装的模组")
                        .clicked()
                    {
                        self.selected = self
                            .nexus_list
                            .iter()
                            .filter(|m| !self.is_installed_name(&m.name))
                            .map(|m| m.mod_id)
                            .collect();
                    }
                    if ui.small_button("清空选择").clicked() {
                        self.selected.clear();
                    }
                    ui.checkbox(&mut self.hide_installed, "隐藏已安装");
                    let installed_n = self
                        .nexus_list
                        .iter()
                        .filter(|m| self.is_installed_name(&m.name))
                        .count();
                    liquid::stat_chip(
                        ui,
                        "已安装",
                        &installed_n.to_string(),
                        liquid::SUCCESS,
                        liquid::SUCCESS_SOFT,
                    );
                    ui.label(
                        RichText::new(format!("已选 {n_sel} / {total}"))
                            .size(12.0)
                            .weak(),
                    );
                    if liquid::cta_button(
                        ui,
                        &format!("🌐 浏览器下载已勾选（{n_sel}）"),
                        n_sel > 0 && self.queue_idle(),
                    )
                    .on_hover_text("自动逐个下载：一个模组安装完成后再打开下一个文件页")
                    .clicked()
                    {
                        let picks: Vec<(u32, String)> = self
                            .nexus_list
                            .iter()
                            .filter(|m| self.selected.contains(&m.mod_id))
                            .map(|m| (m.mod_id, m.name.clone()))
                            .collect();
                        self.open_browser_downloads(picks);
                    }
                    if !self.queue_idle() {
                        ui.label(
                            RichText::new("队列进行中，一次只开一个页面")
                                .size(11.0)
                                .weak(),
                        );
                    }
                });

                // 模组列表
                let list = self.nexus_list.clone();
                let mut dl: Option<(u32, String)> = None;
                egui::ScrollArea::vertical()
                    .max_height(330.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for m in &list {
                            if self.hide_installed && self.is_installed_name(&m.name) {
                                continue;
                            }
                            let mut on = self.selected.contains(&m.mod_id);
                            liquid::card(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    if ui.checkbox(&mut on, "").changed() {
                                        if on {
                                            self.selected.insert(m.mod_id);
                                        } else {
                                            self.selected.remove(&m.mod_id);
                                        }
                                    }
                                    ui.vertical(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new(&m.name).strong());
                                            if self.is_installed_name(&m.name) {
                                                liquid::pill(
                                                    ui,
                                                    "✅ 已安装",
                                                    liquid::SUCCESS,
                                                    liquid::SUCCESS_SOFT,
                                                );
                                            }
                                            liquid::pill(
                                                ui,
                                                &format!("v{}", m.version),
                                                liquid::SKY,
                                                liquid::SKY_SOFT,
                                            );
                                            liquid::pill(
                                                ui,
                                                &format!("⬇ {}", m.downloads),
                                                liquid::SLATE,
                                                liquid::SLATE_SOFT,
                                            );
                                        });
                                        ui.label(
                                            RichText::new(format!(
                                                "作者：{} · ID {}",
                                                m.author, m.mod_id
                                            ))
                                            .size(11.0)
                                            .weak(),
                                        );
                                        if !m.summary.is_empty() {
                                            ui.label(
                                                RichText::new(&m.summary).size(11.0).weak(),
                                            );
                                        }
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if liquid::cta_button(
                                                ui,
                                                "🌐 浏览器下载",
                                                self.queue_idle(),
                                            )
                                            .on_hover_text(
                                                "打开文件页 → 点 Manual Download → Slow Download，\
                                                 下载完成自动安装",
                                            )
                                            .clicked()
                                            {
                                                dl = Some((m.mod_id, m.name.clone()));
                                            }
                                        },
                                    );
                                });
                            });
                            ui.add_space(4.0);
                        }
                    });
                if let Some(x) = dl {
                    self.open_browser_downloads(vec![x]);
                }
            }
        });
    }

    /// 下载监控卡：浏览器下载进度 + 自动安装结果。
    fn ui_watch_card(&mut self, ui: &mut egui::Ui) {
        liquid::card_accent(ui, Some(liquid::PRIMARY), |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                liquid::section_title(ui, "下载监控", liquid::PRIMARY);
                let active = self.watch_jobs.iter().filter(|j| j.is_active()).count();
                let ok = self
                    .watch_jobs
                    .iter()
                    .filter(|j| matches!(j.state, WState::Finished(true, _)))
                    .count();
                let fail = self
                    .watch_jobs
                    .iter()
                    .filter(|j| matches!(j.state, WState::Finished(false, _)))
                    .count();
                ui.label(
                    RichText::new(format!("进行中 {active} · 已安装 {ok} · 异常 {fail}"))
                        .size(12.0)
                        .weak(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::watch::session_active() {
                        ui.label(
                            RichText::new("🟢 监控中").size(12.0).color(liquid::SUCCESS),
                        );
                        if ui.small_button("停止监控").clicked() {
                            crate::watch::stop_session();
                            self.stop_queue();
                            self.status = "已停止监控浏览器下载".to_string();
                        }
                    } else if ui.small_button("开始监控").clicked() {
                        if let Some(d) = self.env.mods_path.clone() {
                            crate::watch::set_mods_dir(d);
                        }
                        crate::watch::start_session();
                        self.status = "已开始监控：浏览器下载的模组 zip 会自动安装".to_string();
                    }
                });
            });

            // 串行队列状态行
            if self.dl_current.is_some() || !self.dl_queue.is_empty() {
                let (cur_txt, remain) = match &self.dl_current {
                    Some((id, name)) => (
                        format!("当前：{name}（ID {id}）"),
                        self.dl_queue.len(),
                    ),
                    None => ("当前：无".to_string(), self.dl_queue.len()),
                };
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("🚚 下载队列进行中 · {cur_txt} · 待开 {remain} 个"))
                            .size(12.0),
                    );
                    if ui
                        .small_button("跳过此模组")
                        .on_hover_text("当前页面验证过不去 / 下载失败时，直接开下一个")
                        .clicked()
                    {
                        self.skip_current();
                    }
                    if ui.small_button("停止队列").clicked() {
                        self.stop_queue();
                    }
                });
            }
            ui.label(
                RichText::new(format!(
                    "监控目录：{}",
                    crate::watch::downloads_dir()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "未找到（%USERPROFILE%\\Downloads）".to_string())
                ))
                .size(11.0)
                .weak(),
            );

            if self.watch_jobs.is_empty() {
                ui.label(
                    RichText::new("暂无任务：点模组的「🌐 浏览器下载」，浏览器开始下载后这里会出现实时进度。")
                        .size(12.0)
                        .weak(),
                );
                return;
            }

            let jobs: Vec<WatchJob> = self.watch_jobs.iter().rev().cloned().collect();
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for job in jobs {
                        self.ui_watch_row(ui, &job);
                    }
                });
        });
    }

    fn ui_watch_row(&mut self, ui: &mut egui::Ui, job: &WatchJob) {
        let (icon, color) = state_visual(&job.state);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).color(color).size(15.0));
                ui.label(RichText::new(&job.title).strong());
                ui.label(RichText::new(state_label(&job.state)).size(11.0).color(color));
                if job.state == WState::Downloading && job.done > 0 {
                    ui.label(
                        RichText::new(format!("{:.1} MB", job.done as f64 / 1048576.0))
                            .size(11.0)
                            .weak(),
                    );
                }
                if job.is_active() {
                    ui.spinner();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let WState::Finished(false, _) = job.state
                        && let Some(p) = &job.path
                        && ui.small_button("打开文件夹").clicked()
                    {
                        open_in_explorer(p);
                    }
                    if !job.is_active() && ui.small_button("✕").on_hover_text("移除该记录").clicked()
                    {
                        crate::watch::remove_job(job.id);
                    }
                });
            });
            match &job.state {
                WState::Downloading => {
                    ui.label(
                        RichText::new("浏览器下载中（按浏览器里的下载进度为准，完成后自动接管安装）")
                            .size(11.0)
                            .weak(),
                    );
                }
                WState::Installing => {
                    ui.label(RichText::new("正在校验并安装到 Mods…").size(11.0).weak());
                }
                WState::Finished(_, detail) => {
                    if !detail.is_empty() {
                        ui.label(RichText::new(detail).size(11.0).weak());
                    }
                }
            }
            ui.add_space(3.0);
        });
    }

    /// 本地 zip 安装（兜底）。
    fn ui_zip_card(&mut self, ui: &mut egui::Ui) {
        liquid::card(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new("📦 本地压缩包安装（手动指定 zip）")
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if liquid::cta_button(ui, "选择 zip 压缩包安装", true).clicked() {
                            if let Some(p) = pick_file() {
                                self.install_zip_picked(p);
                            }
                        }
                        if !self.zip_msg.is_empty() {
                            ui.label(RichText::new(&self.zip_msg).size(12.0).weak());
                        }
                    });
                });
        });
    }
}

// ---------- 我的模组页 ----------

impl App {
    fn ui_mods_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("搜索");
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("名称 / 作者 / ID")
                    .desired_width(220.0),
            );
            ui.label("状态");
            let filters = [
                (EnableFilter::All, "全部"),
                (EnableFilter::Enabled, "已启用"),
                (EnableFilter::Disabled, "已禁用"),
            ];
            if let Some(f) = liquid::segmented(ui, "enable_filter", &filters, self.enable_filter) {
                self.enable_filter = f;
            }
        });
        ui.add_space(6.0);

        let filtered = self.filtered_mods();
        let total = filtered.len();
        let enabled_n = filtered.iter().filter(|m| m.enabled).count();
        ui.horizontal(|ui| {
            liquid::stat_chip(ui, "筛选结果", &total.to_string(), liquid::PRIMARY, liquid::PRIMARY_SOFT);
            liquid::stat_chip(ui, "已启用", &enabled_n.to_string(), liquid::SUCCESS, liquid::SUCCESS_SOFT);
            liquid::stat_chip(
                ui,
                "已禁用",
                &(total - enabled_n).to_string(),
                liquid::SLATE,
                liquid::SLATE_SOFT,
            );
            if ui.button("全部启用").clicked() {
                for m in &filtered {
                    if !m.enabled {
                        let _ = mods_mgr::toggle_mod(m, true);
                    }
                }
                self.refresh();
            }
            if ui.button("全部禁用").clicked() {
                for m in &filtered {
                    if m.enabled {
                        let _ = mods_mgr::toggle_mod(m, false);
                    }
                }
                self.refresh();
            }
        });
        ui.add_space(8.0);

        let mut toggles: Vec<(String, bool)> = Vec::new();
        let mut open_dir: Option<PathBuf> = None;
        let mut ask_delete: Option<String> = None;
        let mut do_delete: Option<ModEntry> = None;

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .column(Column::exact(44.0))
            .column(Column::initial(300.0).at_least(160.0))
            .column(Column::auto())
            .column(Column::initial(140.0))
            .column(Column::exact(200.0))
            .header(22.0, |mut header| {
                header.col(|ui| {
                    ui.strong("启用");
                });
                header.col(|ui| {
                    ui.strong("名称");
                });
                header.col(|ui| {
                    ui.strong("版本");
                });
                header.col(|ui| {
                    ui.strong("作者");
                });
                header.col(|ui| {
                    ui.strong("操作");
                });
            })
            .body(|body| {
                body.rows(30.0, filtered.len(), |mut row| {
                    let m = &filtered[row.index()];
                    let id = m.unique_id();
                    row.col(|ui| {
                        let mut want = m.enabled;
                        if ui.checkbox(&mut want, "").changed() {
                            toggles.push((id.clone(), want));
                        }
                    });
                    row.col(|ui| {
                        let mut txt = RichText::new(m.name());
                        if m.is_content_pack {
                            txt = txt.color(Color32::from_rgb(0x6F, 0xB1, 0xFF));
                        } else if !m.enabled {
                            txt = txt.color(Color32::from_rgb(0xA9, 0xB1, 0xBC));
                        }
                        ui.label(txt);
                    });
                    row.col(|ui| {
                        liquid::pill(ui, &m.version(), liquid::SLATE, liquid::SLATE_SOFT);
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(m.author()).size(12.0).weak());
                    });
                    row.col(|ui| {
                        ui.horizontal(|ui| {
                            if ui.small_button("文件夹").clicked() {
                                open_dir = Some(m.path.clone());
                            }
                            if ui.small_button("删除").clicked() {
                                ask_delete = Some(id.clone());
                            }
                            if self.armed_delete.as_deref() == Some(id.as_str()) {
                                if ui.small_button("确认删除").clicked() {
                                    do_delete = Some(m.clone());
                                }
                                if ui.small_button("取消").clicked() {
                                    self.armed_delete = None;
                                }
                            }
                        });
                    });
                });
            });

        let changed = !toggles.is_empty();
        for (id, want) in &toggles {
            if let Some(m) = self.mods.iter().find(|m| &m.unique_id() == id).cloned() {
                if let Err(e) = mods_mgr::toggle_mod(&m, *want) {
                    self.status = format!("操作失败：{e}");
                }
            }
        }
        if changed {
            self.refresh();
        }
        if let Some(p) = open_dir {
            open_in_explorer(&p);
        }
        if let Some(id) = ask_delete {
            self.armed_delete = Some(id);
            self.status = "再点一次「确认删除」将彻底删除该模组文件夹（不可恢复）".to_string();
        }
        if let Some(m) = do_delete {
            match std::fs::remove_dir_all(&m.path) {
                Ok(()) => {
                    self.armed_delete = None;
                    self.status = format!("已删除 {}", m.name());
                    self.refresh();
                }
                Err(e) => self.status = format!("删除失败：{e}"),
            }
        }
    }

    fn filtered_mods(&self) -> Vec<ModEntry> {
        let q = self.search.trim().to_lowercase();
        self.mods
            .iter()
            .filter(|m| {
                let ok_enable = match self.enable_filter {
                    EnableFilter::All => true,
                    EnableFilter::Enabled => m.enabled,
                    EnableFilter::Disabled => !m.enabled,
                };
                let ok_search = q.is_empty()
                    || m.name().to_lowercase().contains(&q)
                    || m.author().to_lowercase().contains(&q)
                    || m.unique_id().to_lowercase().contains(&q);
                ok_enable && ok_search
            })
            .cloned()
            .collect()
    }
}

// ---------- 一键开服页 ----------

impl App {
    fn ui_host_page(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.ui_host_actions(ui);
                ui.add_space(8.0);
                self.ui_host_status(ui);
                ui.add_space(8.0);
                self.ui_host_saves(ui);
                ui.add_space(8.0);
                self.ui_host_vnt(ui);
                ui.add_space(8.0);
                self.ui_host_options(ui);
                ui.add_space(8.0);
                self.ui_host_share(ui);
            });
    }

    // —— 开服 / 停服 / 手动控制 ——

    fn ui_host_actions(&mut self, ui: &mut egui::Ui) {
        let game_ok = self.env.game_path.is_some();
        let hosting = self.probe.port_busy;
        let paused = self.probe.status.as_ref().map(|s| s.time_paused).unwrap_or(false);
        let plugin_ready = self.env.game_path.is_some();
        liquid::card_accent(ui, Some(liquid::PRIMARY), |ui| {
            ui.set_width(ui.available_width());
            liquid::section_title(ui, "开服", liquid::PRIMARY);
            ui.horizontal_wrapped(|ui| {
                if liquid::cta_button(
                    ui,
                    if hosting { "重新开服" } else { "🚀 一键开服" },
                    game_ok && !self.host_busy,
                )
                .clicked()
                {
                    self.start_hosting();
                }
                if liquid::soft_button(
                    ui,
                    "暂停时间",
                    liquid::WARNING,
                    liquid::WARNING_SOFT,
                )
                .clicked()
                    && plugin_ready
                {
                    self.send_host_command("freeze");
                }
                if liquid::soft_button(ui, "恢复时间", liquid::SUCCESS, liquid::SUCCESS_SOFT).clicked()
                    && plugin_ready
                {
                    self.send_host_command("unfreeze");
                }
                if liquid::soft_button(
                    ui,
                    "保存并回到标题（停服）",
                    liquid::DANGER,
                    liquid::DANGER_SOFT,
                )
                .clicked()
                    && plugin_ready
                {
                    self.send_host_command("quit");
                }
                if self.host_busy {
                    ui.spinner();
                }
            });
            ui.add_space(2.0);
            ui.label(
                RichText::new(
                    "「一键开服」= 自动装好房主助手 → 启动游戏 → 读取所选存档并开房。\
                     之后在游戏里按 Esc → 联机 → 「邀请好友」/让好友从「加入局域网游戏」进来即可。",
                )
                .size(11.0)
                .weak(),
            );
            if !self.host_msg.is_empty() {
                ui.add_space(2.0);
                let color = if self.host_msg.contains("失败") {
                    liquid::DANGER
                } else {
                    liquid::SUCCESS
                };
                ui.label(RichText::new(&self.host_msg).size(12.0).color(color));
            }
            if paused {
                ui.label(
                    RichText::new("当前游戏时间处于暂停状态（暂停时玩家仍可自由行动，只是时间不走）")
                        .size(11.0)
                        .color(liquid::WARNING),
                );
            }
        });
    }

    fn ui_host_status(&mut self, ui: &mut egui::Ui) {
        let p = self.probe.clone();
        liquid::card(ui, |ui| {
            ui.set_width(ui.available_width());
            let accent = if p.port_busy { liquid::SUCCESS } else { liquid::SLATE };
            liquid::section_title(
                ui,
                if p.port_busy { "服务器运行中" } else { "服务器未开" },
                accent,
            );
            ui.horizontal_wrapped(|ui| {
                liquid::stat_chip(
                    ui,
                    "游戏进程",
                    if p.game_running { "运行中" } else { "未运行" },
                    if p.game_running { liquid::SUCCESS } else { liquid::SLATE },
                    liquid::SUCCESS_SOFT,
                );
                liquid::stat_chip(
                    ui,
                    "联机端口",
                    if p.port_busy { "已监听" } else { "未监听" },
                    if p.port_busy { liquid::SUCCESS } else { liquid::SLATE },
                    liquid::SUCCESS_SOFT,
                );
                liquid::stat_chip(
                    ui,
                    "在线玩家",
                    &p.status
                        .as_ref()
                        .map(|s| s.players.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    liquid::PRIMARY,
                    liquid::PRIMARY_SOFT,
                );
                liquid::stat_chip(
                    ui,
                    "虚拟局域网",
                    p.vnt.ip.as_deref().unwrap_or("未连接"),
                    if p.vnt.running { liquid::TEAL } else { liquid::SLATE },
                    liquid::TEAL_SOFT,
                );
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("本机地址：").size(12.0).color(liquid::text_dim()));
                match &p.local_ip {
                    Some(ip) => {
                        ui.label(RichText::new(ip).size(12.0).strong());
                    }
                    None => {
                        ui.label(RichText::new("未获取到").size(12.0).color(liquid::text_dim()));
                    }
                }
                if p.firewall_ok {
                    ui.label(RichText::new("· 防火墙已放行").size(12.0).color(liquid::SUCCESS));
                } else {
                    ui.label(
                        RichText::new("· 防火墙未放行（好友连不上时点这里）")
                            .size(12.0)
                            .color(liquid::WARNING),
                    );
                    if ui.small_button("放行 UDP 24642").clicked() {
                        self.allow_firewall();
                    }
                }
            });
            match &p.status {
                Some(st) if st.hosting => {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        liquid::pill(
                            ui,
                            if st.time_paused { "⏸ 时间已暂停" } else { "▶ 时间流动中" },
                            if st.time_paused { liquid::WARNING } else { liquid::SUCCESS },
                            if st.time_paused { liquid::WARNING_SOFT } else { liquid::SUCCESS_SOFT },
                        );
                        if st.time_paused && !st.pause_reason().is_empty() {
                            ui.label(
                                RichText::new(format!("（{}）", st.pause_reason()))
                                    .size(12.0)
                                    .color(liquid::text_dim()),
                            );
                        }
                        liquid::pill(
                            ui,
                            &format!(
                                "{} {} 日",
                                st.season_label(),
                                st.day_of_month
                            ),
                            liquid::VIOLET,
                            liquid::VIOLET_SOFT,
                        );
                        liquid::pill(ui, &st.clock(), liquid::PRIMARY, liquid::PRIMARY_SOFT);
                    });
                    ui.label(
                        RichText::new(format!(
                            "存档 {} · 农场 {} · 玩家 {}",
                            st.save_name,
                            st.farm_name,
                            if st.player_names.is_empty() {
                                "（仅房主）".to_string()
                            } else {
                                st.player_names.join("、")
                            }
                        ))
                        .size(12.0)
                        .color(liquid::text_dim()),
                    );
                    ui.label(
                        RichText::new(format!(
                            "房主已连续无操作 {} 分 {} 秒（达到设定值会自动暂停时间）",
                            st.afk_seconds / 60,
                            st.afk_seconds % 60
                        ))
                        .size(11.0)
                        .color(liquid::text_dim()),
                    );
                    if !st.note.is_empty() {
                        ui.label(RichText::new(&st.note).size(12.0).color(liquid::text_main()));
                    }
                }
                _ => {
                    ui.label(
                        RichText::new(
                            "还没有检测到房主状态。点上面的「一键开服」后，这里会显示存档、玩家、\
                             游戏内时间与自动暂停情况。",
                        )
                        .size(12.0)
                        .color(liquid::text_dim()),
                    );
                }
            }
        });
    }

    // —— 存档 ——

    fn ui_host_saves(&mut self, ui: &mut egui::Ui) {
        liquid::card(ui, |ui| {
            ui.set_width(ui.available_width());
            liquid::section_title(ui, "选择要开的存档", liquid::VIOLET);
            ui.label(
                RichText::new(
                    "标记「可开房」的存档是在游戏里启用过联机的；开服后游戏进程会读取该存档并作为房主开房。",
                )
                .size(11.0)
                .weak(),
            );
            if self.saves.is_empty() {
                ui.label(
                    RichText::new("没找到存档，先在游戏里创建并保存一次。")
                        .size(12.0)
                        .color(liquid::WARNING),
                );
                return;
            }
            let mut picked: Option<String> = None;
            for s in &self.saves {
                let selected = self.settings.host.save_name == s.folder;
                if save_row(ui, s, selected).clicked() {
                    picked = Some(s.folder.clone());
                }
            }
            if let Some(name) = picked {
                self.settings.host.save_name = name;
                let _ = self.settings.save();
            }
        });
    }

    // —— vnt 内网穿透 ——

    fn ui_host_vnt(&mut self, ui: &mut egui::Ui) {
        let installed = server::vnt_installed();
        let running = self.probe.vnt.running;
        let ip = self.probe.vnt.ip.clone();
        liquid::card_accent(ui, Some(liquid::TEAL), |ui| {
            ui.set_width(ui.available_width());
            liquid::section_title(ui, "异地联机加速（vnt 内网穿透）", liquid::TEAL);
            ui.label(
                RichText::new(
                    "双方都填同一个「组网编号」后，会自动打 P2P 隧道组出一个虚拟局域网，\
                     不需要公网 IP、不需要端口映射。跨运营商/跨地区联机比直连稳很多。",
                )
                .size(11.0)
                .weak(),
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                liquid::pill(
                    ui,
                    if installed { "vnt 已安装" } else { "vnt 未安装" },
                    if installed { liquid::SUCCESS } else { liquid::SLATE },
                    if installed { liquid::SUCCESS_SOFT } else { liquid::SLATE_SOFT },
                );
                liquid::pill(
                    ui,
                    if running { "隧道已连接" } else { "隧道未连接" },
                    if running { liquid::SUCCESS } else { liquid::SLATE },
                    if running { liquid::SUCCESS_SOFT } else { liquid::SLATE_SOFT },
                );
                if let Some(ip) = &ip {
                    liquid::pill(ui, ip, liquid::TEAL, liquid::TEAL_SOFT);
                }
                if self.host_busy {
                    ui.spinner();
                }
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if !installed {
                    if liquid::cta_button(ui, "下载 vnt（约 7 MB）", true).clicked() {
                        self.vnt_install();
                    }
                } else {
                    if liquid::cta_button(ui, "启动 vnt 隧道", !running).clicked() {
                        self.vnt_start();
                    }
                    if liquid::soft_button(ui, "断开隧道", liquid::DANGER, liquid::DANGER_SOFT).clicked()
                        && running
                    {
                        self.vnt_stop();
                    }
                    if liquid::soft_button(ui, "更新 vnt", liquid::SLATE, liquid::SLATE_SOFT).clicked() {
                        self.vnt_install();
                    }
                }
                if ui.small_button("打开 vnt 目录").clicked() {
                    let dir = server::vnt_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    open_in_explorer(&dir);
                }
                if self.probe.vnt.running {
                    ui.label(
                        RichText::new("启动/停止隧道需要管理员权限，会弹一次 UAC 授权窗。")
                            .size(11.0)
                            .color(liquid::text_dim()),
                    );
                }
            });
            ui.add_space(6.0);
            let mut dirty = false;
            egui::Grid::new("vnt_fields")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("组网编号");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.settings.vnt.network_code)
                            .hint_text("房主和好友填完全一样的一串字符")
                            .desired_width(320.0),
                    );
                    dirty |= r.lost_focus();
                    ui.end_row();

                    ui.label("组网密码");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.settings.vnt.password)
                            .hint_text("可选，填了就两边都要一致")
                            .desired_width(320.0)
                            .password(true),
                    );
                    dirty |= r.lost_focus();
                    ui.end_row();

                    ui.label("服务器");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.settings.vnt.server)
                            .hint_text("101.35.230.139:6660（公共服务器，也可自建）")
                            .desired_width(320.0),
                    );
                    dirty |= r.lost_focus();
                    ui.end_row();

                    ui.label("设备名");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.settings.vnt.device_name)
                            .hint_text("留空用电脑主机名")
                            .desired_width(320.0),
                    );
                    dirty |= r.lost_focus();
                    ui.end_row();
                });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                dirty |= ui
                    .checkbox(&mut self.settings.vnt.rtx, "QUIC 优化传输")
                    .changed();
                dirty |= ui
                    .checkbox(&mut self.settings.vnt.fec, "FEC 前向纠错（弱网更稳）")
                    .changed();
                dirty |= ui
                    .checkbox(&mut self.settings.vnt.no_broadcast, "关闭虚拟网广播")
                    .on_hover_text("关掉后「加入局域网游戏」列表搜不到主机，只能手动输 IP 加入")
                    .changed();
            });
            if dirty {
                let _ = self.settings.save();
            }
            if installed {
                egui::CollapsingHeader::new("vnt 运行日志")
                    .id_salt("vnt_log")
                    .show(ui, |ui| {
                        let tail = server::vnt_log_tail(20);
                        if tail.is_empty() {
                            ui.label(RichText::new("暂无日志").size(11.0).weak());
                        } else {
                            ui.label(RichText::new(tail).size(11.0).monospace());
                        }
                    });
            }
        });
    }

    // —— 房主功能 ——

    fn ui_host_options(&mut self, ui: &mut egui::Ui) {
        let mut dirty = false;
        liquid::card_accent(ui, Some(liquid::VIOLET), |ui| {
            ui.set_width(ui.available_width());
            liquid::section_title(ui, "房主助手", liquid::VIOLET);
            ui.label(
                RichText::new(
                    "改动会随「一键开服」一起写进游戏；游戏已经在跑时，点一次「暂停时间/恢复时间」\
                     也会顺带把设置同步进游戏。",
                )
                .size(11.0)
                .weak(),
            );
            ui.add_space(4.0);

            let cfg = &mut self.settings.host;
            // 默认滑杆只有 100pt，右侧还要放说明文字，调宽一点更好拖。
            ui.spacing_mut().slider_width = 160.0;
            ui.horizontal_wrapped(|ui| {
                dirty |= ui.checkbox(&mut cfg.afk_pause, "房主挂机自动暂停时间").changed();
                ui.add_enabled(
                    cfg.afk_pause,
                    egui::Slider::new(&mut cfg.afk_minutes, 1..=120).suffix(" 分钟"),
                );
                ui.label(
                    RichText::new("（房主不碰键鼠超过设定时长，游戏时间自动停住，等房主回来再继续）")
                        .size(11.0)
                        .color(liquid::text_dim()),
                );
            });
            ui.horizontal_wrapped(|ui| {
                dirty |= ui
                    .checkbox(&mut cfg.empty_pause, "无人在线时自动暂停时间")
                    .changed();
                ui.add_enabled(
                    cfg.empty_pause,
                    egui::Slider::new(&mut cfg.empty_minutes, 1..=240).suffix(" 分钟"),
                );
                ui.label(
                    RichText::new("（好友还没进来时不让农场空跑，有人加入立即恢复）")
                        .size(11.0)
                        .color(liquid::text_dim()),
                );
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                dirty |= ui.checkbox(&mut cfg.announce_join, "玩家上线时公告").changed();
                dirty |= ui.checkbox(&mut cfg.announce_leave, "玩家下线时公告").changed();
                dirty |= ui.checkbox(&mut cfg.notify_pause, "暂停/恢复时聊天框提示").changed();
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("欢迎语");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut cfg.welcome_text)
                        .hint_text("新玩家加入时自动广播，留空则不发送")
                        .desired_width(360.0),
                );
                dirty |= r.lost_focus();
                ui.label("冻结热键");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut cfg.freeze_hotkey)
                        .hint_text("F8")
                        .desired_width(70.0),
                );
                dirty |= r.lost_focus();
                ui.label(
                    RichText::new("（游戏内按该键冻结/恢复时间）")
                        .size(11.0)
                        .color(liquid::text_dim()),
                );
            });
                ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("广播公告");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.announce_input)
                        .hint_text("发给所有玩家的提示，回车发送")
                        .desired_width(360.0),
                );
                let sent = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("发送").clicked() || sent) && !self.announce_input.trim().is_empty()
                {
                    let text = self.announce_input.trim().to_string();
                    self.announce_input.clear();
                    self.send_host_command(&format!("announce|{text}"));
                }
            });
        });
        if dirty {
            let _ = self.settings.save();
        }
    }

    // —— 邀请信息 ——

    fn ui_host_share(&mut self, ui: &mut egui::Ui) {
        let text = self.share_text();
        liquid::card(ui, |ui| {
            ui.set_width(ui.available_width());
            liquid::section_title(ui, "发给好友的开服信息", liquid::SKY);
            ui.label(
                RichText::new(text.clone())
                    .size(12.0)
                    .color(liquid::text_main()),
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if liquid::cta_button(ui, "复制邀请信息", true).clicked() {
                    ui.ctx().copy_text(text.clone());
                    self.copied = true;
                }
                if self.copied {
                    ui.label(RichText::new("已复制").size(12.0).color(liquid::SUCCESS));
                }
            });
        });
    }

    // —— 动作实现 ——

    fn start_hosting(&mut self) {
        let Some(game) = self.env.game_path.clone() else {
            self.status = "未找到游戏目录，请先在「设置」页指定".to_string();
            return;
        };
        let save = self.settings.host.save_name.trim().to_string();
        if save.is_empty() {
            self.status = "请先在下面选一个要开的存档".to_string();
            return;
        }
        let mut cfg = self.settings.host.clone();
        cfg.auto_host = true;
        self.host_busy = true;
        self.host_msg.clear();
        self.status = format!("正在部署房主助手并启动游戏（存档 {save}）…");
        std::thread::spawn(move || {
            let msg = match run_host_start(&game, &cfg, &save) {
                Ok(m) => m,
                Err(e) => format!("开服失败：{e}"),
            };
            *HOST_MSG_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(msg);
        });
    }

    fn vnt_install(&mut self) {
        self.host_busy = true;
        self.status = "正在从 GitHub 下载 vnt…".to_string();
        std::thread::spawn(|| {
            let msg = match server::vnt_download() {
                Ok(p) => format!("vnt 已就绪：{}", p.display()),
                Err(e) => format!("vnt 下载失败：{e}"),
            };
            *HOST_MSG_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(msg);
        });
    }

    fn vnt_start(&mut self) {
        let opt = self.settings.vnt.clone();
        self.host_busy = true;
        self.status = "正在启动 vnt 隧道（请在 UAC 弹窗点「是」）…".to_string();
        std::thread::spawn(move || {
            let msg = match server::vnt_start(&opt) {
                Ok(()) => "已启动 vnt，隧道建立中（约 3~5 秒后虚拟 IP 会出现）".to_string(),
                Err(e) => format!("vnt 启动失败：{e}"),
            };
            *HOST_MSG_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(msg);
        });
    }

    fn vnt_stop(&mut self) {
        self.host_busy = true;
        self.status = "正在断开 vnt 隧道…".to_string();
        std::thread::spawn(|| {
            let msg = match server::vnt_stop() {
                Ok(()) => "已断开 vnt 隧道".to_string(),
                Err(e) => format!("断开失败：{e}"),
            };
            *HOST_MSG_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(msg);
        });
    }

    fn allow_firewall(&mut self) {
        self.status = "正在添加防火墙放行规则（请在 UAC 弹窗点「是」）…".to_string();
        std::thread::spawn(|| {
            let msg = match server::allow_firewall(server::GAME_PORT) {
                Ok(()) => format!("已放行 UDP {} 端口", server::GAME_PORT),
                Err(e) => format!("放行失败：{e}"),
            };
            *HOST_MSG_RESULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(msg);
        });
    }

    /// 指令很轻（写一个小文件），直接同步发。
    fn send_host_command(&mut self, cmd: &str) {
        let Some(game) = self.env.game_path.clone() else {
            self.status = "未找到游戏目录".to_string();
            return;
        };
        match server::send_command(&game, cmd) {
            Ok(()) => {
                self.status = match cmd {
                    "freeze" => "已请求暂停游戏时间".to_string(),
                    "unfreeze" => "已请求恢复游戏时间".to_string(),
                    "quit" => "已请求保存并回到标题，服务器稍后关闭".to_string(),
                    _ => "已发送公告".to_string(),
                };
                self.host_msg = self.status.clone();
                self.probe_at = self.start;
            }
            Err(e) => self.status = format!("指令发送失败：{e}"),
        }
    }

    fn share_text(&self) -> String {
        let v = &self.settings.vnt;
        let mut s = String::from("【星露谷联机邀请】\n");
        let code = v.network_code.trim();
        if code.is_empty() {
            s.push_str("vnt 组网：还没填组网编号（在「异地联机加速」卡片里填一个双方一致的编号）\n");
        } else {
            s.push_str(&format!("vnt 组网编号：{code}\n"));
            if !v.password.trim().is_empty() {
                s.push_str(&format!("vnt 组网密码：{}\n", v.password.trim()));
            }
            s.push_str(&format!("vnt 服务器：{}\n", server::normalize_server(&v.server)));
        }
        match (&self.probe.vnt.ip, &self.probe.local_ip) {
            (Some(ip), _) => s.push_str(&format!("房主虚拟 IP：{}（联机 → 加入局域网游戏，或直接输这个 IP）\n", ip)),
            (None, Some(ip)) => s.push_str(&format!("房主局域网 IP：{}（在同一 Wi-Fi 下可直接用）\n", ip)),
            _ => {}
        }
        s.push_str(
            "好友准备：装同版本 SMAPI → 装和房主一样的模组 → 装 vnt 并填上面的组网编号 → \
             进游戏选「联机 → 加入局域网游戏」，搜不到就手动输入房主 IP。\n",
        );
        s
    }
}

/// 开服动作（在后台线程执行）：装插件 → 写配置 → 起游戏 → 通知插件读档开房。
fn run_host_start(game: &Path, cfg: &HostKitConfig, save: &str) -> Result<String, String> {
    server::deploy_hostkit(game).map_err(|e| e.to_string())?;
    server::write_config(game, cfg).map_err(|e| e.to_string())?;
    let launched = if server::game_running() {
        "游戏已在运行"
    } else {
        server::launch_smapi(game).map_err(|e| e.to_string())?;
        "已启动游戏"
    };
    // 不管游戏是不是刚起来，都补一条指令：插件收到后会重读配置并重新武装自动读档，
    // 这样「游戏已经开在标题界面」的情况下也能一键开服。
    server::send_command(game, &format!("host|{save}")).map_err(|e| e.to_string())?;
    Ok(format!(
        "{launched}，正在以房主身份读取存档「{save}」，几秒后服务器就绪"
    ))
}

/// 存档列表的一行：标题 + 日期/时长/保存时间，右侧开房可行性标记。
fn save_row(ui: &mut egui::Ui, s: &SaveInfo, selected: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 50.0), egui::Sense::click());
    let painter = ui.painter();
    let radius = egui::CornerRadius::same(10);
    if selected {
        painter.rect_filled(rect, radius, liquid::PRIMARY_SOFT);
    } else if resp.hovered() {
        painter.rect_filled(rect, radius, Color32::from_black_alpha(10));
    }
    painter.text(
        egui::pos2(rect.left() + 12.0, rect.top() + 7.0),
        egui::Align2::LEFT_TOP,
        s.title(),
        egui::FontId::proportional(14.0),
        liquid::text_main(),
    );
    painter.text(
        egui::pos2(rect.left() + 12.0, rect.top() + 28.0),
        egui::Align2::LEFT_TOP,
        format!(
            "{} · 已玩 {:.0} 小时 · 保存于 {}",
            s.date_label(),
            s.play_hours,
            s.saved_ago_label()
        ),
        egui::FontId::proportional(11.0),
        liquid::text_dim(),
    );
    let (tag, color) = if s.can_host {
        ("可开房", liquid::SUCCESS)
    } else {
        ("未启用联机", liquid::WARNING)
    };
    painter.text(
        egui::pos2(rect.right() - 12.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        tag,
        egui::FontId::proportional(12.0),
        color,
    );
    resp
}

// ---------- 联机大厅页 ----------

impl App {
    fn ui_social_page(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    self.ui_social_profile(ui);
                    ui.add_space(8.0);
                    self.ui_social_friends(ui);
                    ui.add_space(8.0);
                    self.ui_social_nearby(ui);
                    ui.add_space(8.0);
                    self.ui_social_sync(ui);
                });
            });
    }

    /// 档案/登录卡片。
    fn ui_social_profile(&mut self, ui: &mut egui::Ui) {
        liquid::card(ui, |ui| {
            ui.set_width(ui.available_width());
            liquid::inline_title(ui, "我的档案", liquid::PRIMARY);
            ui.add_space(6.0);

            let online = self.social_online;
            let pill_color = if online {
                liquid::SUCCESS
            } else {
                liquid::SLATE
            };
            let pill_text = if online { "● 在线" } else { "● 离线" };
            ui.horizontal(|ui| {
                liquid::pill(ui, pill_text, pill_color, liquid::SLATE_SOFT);
                ui.add_space(8.0);
                if let Some(p) = &self.profile {
                    ui.label(
                        RichText::new(format!("{} {}", p.avatar, p.name))
                            .size(16.0)
                            .color(liquid::text_main()),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("UID: {}", p.uid))
                            .size(11.0)
                            .color(liquid::text_dim()),
                    );
                }
            });

            ui.add_space(8.0);

            if self.profile.is_none() {
                // 注册表单
                ui.label(RichText::new("创建档案（本地保存，无需注册）").size(13.0).color(liquid::text_dim()));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("昵称");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.profile_name_input)
                            .desired_width(160.0)
                            .hint_text("输入你的昵称"),
                    );
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("头像");
                    for (i, av) in p2p::AVATARS.iter().enumerate() {
                        let selected = self.profile_avatar_idx == i;
                        let txt = if selected {
                            RichText::new(*av).size(22.0)
                        } else {
                            RichText::new(*av).size(18.0).color(liquid::text_dim())
                        };
                        if ui.add(egui::Button::new(txt).frame(false)).clicked() {
                            self.profile_avatar_idx = i;
                        }
                    }
                });
                ui.add_space(6.0);
                if liquid::cta_button(ui, "注册并上线", true).clicked() {
                    let name = self.profile_name_input.trim().to_string();
                    if name.is_empty() {
                        self.social_msg = "请输入昵称".to_string();
                        return;
                    }
                    let p = Profile {
                        uid: p2p::generate_uid(),
                        name,
                        avatar: p2p::AVATARS[self.profile_avatar_idx].to_string(),
                    };
                    let mods_path = self
                        .env
                        .mods_path
                        .clone()
                        .unwrap_or_default();
                    match p2p::start_server(mods_path, p.clone()) {
                        Ok(()) => {
                            let _ = p2p::save_profile(&p);
                            self.profile = Some(p);
                            self.social_online = true;
                            self.social_msg = "已注册并上线！好友在 vnt 同网段即可发现你".to_string();
                        }
                        Err(e) => self.social_msg = e,
                    }
                }
            } else if let Some(p) = self.profile.clone() {
                // 已有档案
                ui.horizontal(|ui| {
                    if !online {
                        if liquid::cta_button(ui, "上线", true).clicked() {
                            let mods_path = self
                                .env
                                .mods_path
                                .clone()
                                .unwrap_or_default();
                            match p2p::start_server(mods_path, p.clone()) {
                                Ok(()) => {
                                    self.social_online = true;
                                    self.social_msg = "已上线！好友在 vnt 同网段即可发现你".to_string();
                                }
                                Err(e) => self.social_msg = e,
                            }
                        }
                    } else {
                        if liquid::soft_button(ui, "离线", liquid::DANGER, liquid::DANGER_SOFT).clicked() {
                            p2p::stop_server();
                            self.social_online = false;
                            self.social_msg = "已离线".to_string();
                        }
                    }
                });
            }

            if !self.social_msg.is_empty() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(&self.social_msg)
                        .size(12.0)
                        .color(liquid::text_dim()),
                );
            }

            ui.add_space(6.0);
            ui.label(
                RichText::new("提示：上线前先在「一键开服」页连接 vnt，好友用相同的组网编号加入即可互相发现")
                    .size(11.0)
                    .color(liquid::text_dim()),
            );
        });
    }

    /// 好友列表卡片。
    fn ui_social_friends(&mut self, ui: &mut egui::Ui) {
        liquid::card(ui, |ui| {
            ui.set_width(ui.available_width());
            liquid::inline_title(ui, "好友列表", liquid::VIOLET);
            ui.add_space(6.0);

            if self.friends.is_empty() {
                ui.label(
                    RichText::new("还没有好友。在下面「附近玩家」中添加")
                        .size(13.0)
                        .color(liquid::text_dim()),
                );
                return;
            }

            // 构建在线好友 IP 查找表
            let online_ips: HashMap<String, String> = self
                .peers
                .iter()
                .map(|p| (p.uid.clone(), p.ip.clone()))
                .collect();

            for f in self.friends.clone().iter() {
                let ip = online_ips.get(&f.uid).cloned();
                let online = ip.is_some();
                let color = if online { liquid::SUCCESS } else { liquid::SLATE };
                let status = if online { "在线" } else { "离线" };

                ui.horizontal(|ui| {
                    ui.label(RichText::new(&f.avatar).size(18.0));
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&f.name).size(14.0).color(liquid::text_main()));
                        ui.label(RichText::new(status).size(11.0).color(color));
                    });
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if online {
                                let ip = ip.unwrap();
                                let selected = self.selected_friend_uid == f.uid;
                                if liquid::soft_button(
                                    ui,
                                    if selected { "正在浏览" } else { "浏览模组" },
                                    liquid::TEAL,
                                    liquid::TEAL_SOFT,
                                )
                                .clicked()
                                {
                                    self.selected_friend_uid = f.uid.clone();
                                    self.selected_friend_ip = ip.clone();
                                    self.peer_mods_loading = true;
                                    self.peer_mods_error = None;
                                    self.peer_mods.clear();
                                    std::thread::spawn(move || {
                                        let r = p2p::fetch_peer_mods(&ip).map_err(|e| e.to_string());
                                        *PEER_MODS_RESULT
                                            .get_or_init(|| Mutex::new(None))
                                            .lock()
                                            .unwrap() = Some(r);
                                    });
                                }
                            }
                            if liquid::soft_button(ui, "删除", liquid::DANGER, liquid::DANGER_SOFT).clicked() {
                                self.friends.retain(|x| x.uid != f.uid);
                                let _ = p2p::save_friends(&self.friends);
                                if self.selected_friend_uid == f.uid {
                                    self.selected_friend_uid.clear();
                                    self.selected_friend_ip.clear();
                                    self.peer_mods.clear();
                                }
                            }
                        },
                    );
                });
                ui.separator();
            }
        });
    }

    /// 附近玩家卡片（未加好友的在线玩家）。
    fn ui_social_nearby(&mut self, ui: &mut egui::Ui) {
        liquid::card(ui, |ui| {
            ui.set_width(ui.available_width());
            liquid::inline_title(ui, "附近玩家", liquid::TEAL);
            ui.add_space(6.0);

            let nearby: Vec<PeerInfo> = self
                .peers
                .iter()
                .filter(|p| !p.is_friend(&self.friends))
                .cloned()
                .collect();

            if nearby.is_empty() {
                ui.label(
                    RichText::new(if self.social_online {
                        "暂无附近玩家。确保好友已上线且 vnt 组网编号一致"
                    } else {
                        "请先上线，才能发现附近玩家"
                    })
                    .size(13.0)
                    .color(liquid::text_dim()),
                );
                return;
            }

            for p in &nearby {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&p.avatar).size(18.0));
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(&p.name)
                                .size(14.0)
                                .color(liquid::text_main()),
                        );
                        ui.label(
                            RichText::new(format!("{}:{}", &p.ip, p.port))
                                .size(11.0)
                                .color(liquid::text_dim()),
                        );
                    });
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if liquid::cta_button(ui, "加好友", true).clicked() {
                                self.friends.push(Friend {
                                    uid: p.uid.clone(),
                                    name: p.name.clone(),
                                    avatar: p.avatar.clone(),
                                    note: String::new(),
                                });
                                let _ = p2p::save_friends(&self.friends);
                                self.social_msg =
                                    format!("已添加「{}」为好友", p.name);
                            }
                        },
                    );
                });
                ui.separator();
            }
        });
    }

    /// 模组同步卡片：自动侦测好友模组 → 对比 → 补齐缺失。
    fn ui_social_sync(&mut self, ui: &mut egui::Ui) {
        liquid::card(ui, |ui| {
            ui.set_width(ui.available_width());
            liquid::inline_title(ui, "模组同步", liquid::SKY);
            ui.add_space(6.0);

            if self.selected_friend_uid.is_empty() {
                ui.label(
                    RichText::new("上线后会自动侦测好友的模组列表，缺少的会弹窗提示")
                        .size(13.0)
                        .color(liquid::text_dim()),
                );
                return;
            }

            if self.peer_mods_loading {
                ui.label(
                    RichText::new("正在侦测好友模组列表…")
                        .size(13.0)
                        .color(liquid::text_dim()),
                );
                return;
            }

            if let Some(e) = &self.peer_mods_error {
                ui.label(
                    RichText::new(format!("侦测失败：{e}"))
                        .size(13.0)
                        .color(liquid::DANGER),
                );
                return;
            }

            let sync = match &self.sync {
                Some(s) => s,
                None => return,
            };

            let total_peer = self.peer_mods.len();
            let missing = sync.missing.len();
            let matching = sync.matching.len();
            let extra = sync.extra.len();

            ui.horizontal(|ui| {
                liquid::stat_chip(ui, "好友模组", &total_peer.to_string(), liquid::SKY, liquid::SKY_SOFT);
                liquid::stat_chip(ui, "缺少", &missing.to_string(), liquid::WARNING, liquid::WARNING_SOFT);
                liquid::stat_chip(ui, "已有一致", &matching.to_string(), liquid::SUCCESS, liquid::SUCCESS_SOFT);
                liquid::stat_chip(ui, "你多出的", &extra.to_string(), liquid::SLATE, liquid::SLATE_SOFT);
            });

            if missing == 0 {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("✓ 模组完全一致，无需补齐")
                        .size(14.0)
                        .color(liquid::SUCCESS),
                );
                return;
            }

            ui.add_space(6.0);

            if self.sync_busy {
                ui.label(
                    RichText::new(&self.sync_progress)
                        .size(13.0)
                        .color(liquid::PRIMARY),
                );
            } else {
                if liquid::cta_button(ui, &format!("补齐 {} 个缺失模组…", missing), true)
                    .clicked()
                {
                    self.show_sync_modal = true;
                }
            }

            if !self.social_msg.is_empty() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(&self.social_msg)
                        .size(12.0)
                        .color(liquid::text_dim()),
                );
            }
        });
    }

    /// 模组同步弹窗内容。
    fn ui_sync_modal(&mut self, ui: &mut egui::Ui) {
        let missing = self.sync.as_ref().map(|s| s.missing.clone()).unwrap_or_default();
        if missing.is_empty() || self.selected_friend_ip.is_empty() {
            self.show_sync_modal = false;
            return;
        }

        // 查找好友名
        let friend_name = self
            .peers
            .iter()
            .find(|p| p.uid == self.selected_friend_uid)
            .map(|p| p.name.clone())
            .or_else(|| {
                self.friends
                    .iter()
                    .find(|f| f.uid == self.selected_friend_uid)
                    .map(|f| f.name.clone())
            })
            .unwrap_or_else(|| "好友".to_string());

        ui.add_space(4.0);
        ui.label(
            RichText::new(format!(
                "「{}」的模组列表中，你缺少以下 {} 个模组：",
                friend_name,
                missing.len()
            ))
            .size(14.0)
            .color(liquid::text_main()),
        );
        ui.add_space(6.0);

        // 缺失模组列表（可滚动）
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for m in &missing {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("·").size(13.0).color(liquid::text_dim()));
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(&m.name)
                                    .size(13.0)
                                    .color(liquid::text_main()),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{}  v{}  by {}  {}",
                                    m.folder,
                                    m.version,
                                    m.author,
                                    m.size_label()
                                ))
                                .size(11.0)
                                .color(liquid::text_dim()),
                            );
                        });
                    });
                }
            });

        ui.add_space(8.0);

        // 下载来源选择（锁定发布版固定走镜像服务器，不给 P2P 选项）
        if cfg!(feature = "locked-mirror") {
            ui.label(
                RichText::new("下载来源：🔒 镜像服务器（发布版仅允许从服务器拉取）")
                    .size(12.5)
                    .color(liquid::text_dim()),
            );
        } else {
            ui.horizontal(|ui| {
                ui.label(RichText::new("下载来源").size(13.0).color(liquid::text_dim()));
                let labels = ["好友 + 服务器同时（竞速）", "仅镜像服务器", "仅好友 P2P"];
                let mut idx = self.sync_source;
                egui::ComboBox::from_id_salt("sync_modal_source")
                    .selected_text(labels[idx])
                    .show_ui(ui, |ui| {
                        for (i, label) in labels.iter().enumerate() {
                            ui.selectable_value(&mut idx, i, *label);
                        }
                    });
                self.sync_source = idx;
            });
        }

        ui.add_space(4.0);

        if self.sync_busy {
            ui.label(
                RichText::new(&self.sync_progress)
                    .size(13.0)
                    .color(liquid::PRIMARY),
            );
        } else {
            ui.horizontal(|ui| {
                let mods_path = self.env.mods_path.clone().unwrap_or_default();
                let mirror_url = self.effective_mirror_url();
                let ip = self.selected_friend_ip.clone();
                let source = if cfg!(feature = "locked-mirror") {
                    DownloadSource::Mirror
                } else {
                    match self.sync_source {
                        0 => DownloadSource::Both,
                        1 => DownloadSource::Mirror,
                        _ => DownloadSource::Friend,
                    }
                };
                let ml = missing.clone();
                if liquid::cta_button(ui, "确认补齐", true).clicked() {
                    self.show_sync_modal = false;
                    self.sync_busy = true;
                    self.sync_progress = "准备中…".to_string();
                    let ip2 = ip.clone();
                    let mu = mirror_url.clone();
                    let mp = mods_path.clone();
                    let ml2 = ml.clone();
                    std::thread::spawn(move || {
                        let (ok, fail) = p2p::fill_missing(
                            &ip2,
                            &ml2,
                            source,
                            &mu,
                            &mp,
                            &|i, total, msg| {
                                *SYNC_PROGRESS_RESULT
                                    .get_or_init(|| Mutex::new(None))
                                    .lock()
                                    .unwrap() = Some((
                                    format!("({i}/{total}) {msg}"),
                                    false,
                                    0,
                                    0,
                                ));
                            },
                        );
                        *SYNC_PROGRESS_RESULT
                            .get_or_init(|| Mutex::new(None))
                            .lock()
                            .unwrap() = Some((
                            format!("补齐完成：成功 {ok}，失败 {fail}"),
                            true,
                            ok,
                            fail,
                        ));
                    });
                }
                if liquid::soft_button(
                    ui,
                    "以后再说",
                    liquid::SLATE,
                    liquid::SLATE_SOFT,
                )
                .clicked()
                {
                    self.show_sync_modal = false;
                }
            });
        }
    }

    /// 直装确认弹窗：列出缺失前置（可一并补齐）与冲突警告。
    fn ui_dep_modal(&mut self, ui: &mut egui::Ui) {
        let Some(mut dm) = self.dep_modal.take() else {
            return;
        };

        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("准备安装「{}」", dm.target.name))
                .size(14.0)
                .color(liquid::text_main()),
        );
        ui.add_space(6.0);

        // 冲突警告（红字）
        if !dm.conflicts.is_empty() {
            ui.label(
                RichText::new(format!("⛔ 与已安装模组冲突：{}", dm.conflicts.join("、")))
                    .size(12.5)
                    .color(liquid::DANGER),
            );
            ui.label(
                RichText::new("同时启用可能崩溃或行为异常，建议只保留其一。")
                    .size(11.0)
                    .color(liquid::text_dim()),
            );
            ui.add_space(6.0);
        }

        // 缺失前置列表（橙字）
        if !dm.missing.is_empty() {
            ui.label(
                RichText::new(format!(
                    "检测到 {} 个必需前置未安装：",
                    dm.missing.len()
                ))
                .size(12.5)
                .color(liquid::WARNING),
            );
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for d in &dm.missing {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("·").size(13.0).color(liquid::text_dim()),
                            );
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new(d.display())
                                        .size(12.5)
                                        .color(liquid::text_main()),
                                );
                                ui.label(
                                    RichText::new(match &d.mirror {
                                        Some(m) => {
                                            format!("镜像收录 · {} · v{}", d.uid, m.version)
                                        }
                                        None => if cfg!(feature = "locked-mirror") {
                                            "镜像未收录，暂不可安装".to_string()
                                        } else {
                                            "镜像未收录，需手动从 N 网下载".to_string()
                                        },
                                    })
                                    .size(10.5)
                                    .color(liquid::text_dim()),
                                );
                            });
                        });
                    }
                });
            ui.add_space(6.0);
        }

        ui.add_space(4.0);
        let n_in_mirror = dm
            .missing
            .iter()
            .filter(|d| d.mirror.is_some())
            .count();
        ui.horizontal(|ui| {
            if n_in_mirror > 0
                && liquid::cta_button(
                    ui,
                    &format!("⚡ 一并安装（{} 个前置）", n_in_mirror),
                    true,
                )
                .clicked()
            {
                // 前置先入队（镜像源下载各自后台进行，安装有全局锁不互踩），
                // 目标模组最后装。
                for d in dm.missing.iter().filter(|d| d.mirror.is_some()) {
                    if let Some(m) = d.mirror.clone() {
                        self.download_mirror(m);
                    }
                }
                let target = dm.target.clone();
                self.dep_modal = None;
                self.download_mirror(target);
                return;
            }
            if liquid::soft_button(
                ui,
                if dm.missing.is_empty() { "仍要安装" } else { "仅安装此模组" },
                liquid::SLATE,
                liquid::SLATE_SOFT,
            )
            .clicked()
            {
                let target = dm.target.clone();
                self.dep_modal = None;
                self.download_mirror(target);
                return;
            }
            if liquid::soft_button(
                ui,
                "取消",
                liquid::SLATE,
                liquid::SLATE_SOFT,
            )
            .clicked()
            {
                self.dep_modal = None;
                return;
            }
        });
        if self.dep_modal.is_none() {
            return;
        }
        // 未点任何按钮：放回状态等下一帧。
        self.dep_modal = Some(dm);
    }

    /// 模组详情弹窗：大封面、标题/元信息、中文 BBCode 长描述、直装入口。
    fn ui_detail_modal(&mut self, ctx: &egui::Context) {
        let Some((mm, missing, conflicts)) = self.detail_modal.clone() else {
            return;
        };
        let mut open = true;
        let mut close_req = false;
        let mut do_install = false;
        egui::Window::new("模组详情")
            .id(egui::Id::new("mirror_detail_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .default_height(600.0)
            .min_width(440.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(2.0);
                // 头部：封面 + 标题/元信息。
                ui.horizontal(|ui| {
                    self.mirror_thumb(ui, &mm, egui::vec2(176.0, 132.0));
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        if !mm.name_zh.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&mm.name_zh).size(16.0).strong(),
                                )
                                .wrap(),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&mm.name).size(11.5).weak(),
                                )
                                .wrap(),
                            );
                        } else {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&mm.name).size(15.0).strong(),
                                )
                                .wrap(),
                            );
                        }
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            if !mm.version.is_empty() {
                                liquid::pill(
                                    ui,
                                    &format!("v{}", mm.version),
                                    liquid::SKY,
                                    liquid::SKY_SOFT,
                                );
                            }
                            liquid::pill(
                                ui,
                                &format!("{:.1} MB", mm.size as f64 / 1048576.0),
                                liquid::SLATE,
                                liquid::SLATE_SOFT,
                            );
                            if !mm.category.is_empty() {
                                liquid::pill(
                                    ui,
                                    &mm.category,
                                    liquid::VIOLET,
                                    liquid::VIOLET_SOFT,
                                );
                            }
                            if !mm.author.is_empty() {
                                liquid::pill(
                                    ui,
                                    &format!("作者：{}", mm.author),
                                    liquid::SLATE,
                                    liquid::SLATE_SOFT,
                                );
                            }
                            if self.is_mirror_installed(&mm) {
                                liquid::pill(
                                    ui,
                                    "✅ 已安装",
                                    liquid::SUCCESS,
                                    liquid::SUCCESS_SOFT,
                                );
                            }
                        });
                        ui.add_space(4.0);
                        // 锁定发布版不提供 N 网外链（模组只从镜像服务器获取）
                        if !cfg!(feature = "locked-mirror") && mm.mod_id > 0 {
                            let url = format!(
                                "https://www.nexusmods.com/stardewvalley/mods/{}",
                                mm.mod_id
                            );
                            if ui
                                .add(egui::Link::new(
                                    RichText::new("🔗 Nexus Mods 原页面").size(11.5),
                                ))
                                .clicked()
                            {
                                ui.ctx().open_url(egui::OpenUrl {
                                    url,
                                    new_tab: true,
                                });
                            }
                        }
                        if !conflicts.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "⛔ 与{}冲突",
                                        conflicts.join("、")
                                    ))
                                    .size(11.0)
                                    .color(liquid::DANGER),
                                )
                                .wrap(),
                            );
                        }
                        if !missing.is_empty() {
                            let names: Vec<String> =
                                missing.iter().map(|d| d.display()).collect();
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "⚠ 缺少前置：{}",
                                        names.join("、")
                                    ))
                                    .size(11.0)
                                    .color(liquid::WARNING),
                                )
                                .wrap(),
                            );
                        }
                    });
                });

                ui.add_space(8.0);

                // 一句话简介（中文优先）。
                let brief = if !mm.summary_zh.is_empty() {
                    mm.summary_zh.as_str()
                } else {
                    mm.summary.as_str()
                };
                if !brief.is_empty() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(brief)
                                .size(12.5)
                                .italics()
                                .color(liquid::text_dim()),
                        )
                        .wrap(),
                    );
                    ui.add_space(6.0);
                }

                // 长描述：中文机翻优先，英文原文收进折叠区。
                let zh_desc = mm.desc_zh.as_str();
                let en_desc = mm.desc.as_str();
                if !zh_desc.is_empty() {
                    liquid::section_title(ui, "详细介绍（中文机翻）", liquid::PRIMARY);
                    bbcode::render(ui, zh_desc);
                    if !en_desc.is_empty() {
                        ui.add_space(4.0);
                        egui::CollapsingHeader::new("查看英文原文")
                            .id_salt("detail_en_desc")
                            .show(ui, |ui| {
                                bbcode::render(ui, en_desc);
                            });
                    }
                } else if !en_desc.is_empty() {
                    liquid::section_title(ui, "详细介绍", liquid::PRIMARY);
                    bbcode::render(ui, en_desc);
                } else if !mm.description.is_empty() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&mm.description)
                                .size(12.5)
                                .color(liquid::text_dim()),
                        )
                        .wrap(),
                    );
                } else {
                    ui.label(
                        RichText::new("暂无详细描述。")
                            .size(12.0)
                            .color(liquid::text_dim()),
                    );
                }

                // 技术细节：前置/冲突规则。
                if !mm.deps.is_empty() || !mm.confs.is_empty() {
                    ui.add_space(4.0);
                    egui::CollapsingHeader::new("依赖与冲突规则（技术细节）")
                        .id_salt("detail_rules")
                        .show(ui, |ui| {
                            if !mm.deps.is_empty() {
                                ui.label(
                                    RichText::new("必需前置 UniqueID：")
                                        .size(11.5)
                                        .color(liquid::text_dim()),
                                );
                                for d in &mm.deps {
                                    ui.label(
                                        RichText::new(format!("· {d}")).size(11.5),
                                    );
                                }
                            }
                            if !mm.confs.is_empty() {
                                ui.add_space(2.0);
                                ui.label(
                                    RichText::new("冲突规则（SMAPI 正则）：")
                                        .size(11.5)
                                        .color(liquid::text_dim()),
                                );
                                for c in &mm.confs {
                                    ui.label(
                                        RichText::new(format!("· {c}")).size(11.5),
                                    );
                                }
                            }
                        });
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let busy = self
                        .mirror_job_for(&mm.file)
                        .is_some_and(|j| j.is_active());
                    let label = if busy {
                        "下载中…"
                    } else if self.is_mirror_installed(&mm) {
                        "重新下载"
                    } else {
                        "⚡ 直装此模组"
                    };
                    if liquid::cta_button(ui, label, !busy).clicked() {
                        do_install = true;
                    }
                    if liquid::soft_button(
                        ui,
                        "关闭",
                        liquid::SLATE,
                        liquid::SLATE_SOFT,
                    )
                    .clicked()
                    {
                        close_req = true;
                    }
                });
            });

        if do_install {
            self.detail_modal = None;
            self.install_with_check(mm);
        } else if !open || close_req {
            self.detail_modal = None;
        }
    }
}

// ---------- 设置页 ----------

impl App {
    fn ui_settings_page(&mut self, ui: &mut egui::Ui) {
        // 游戏环境
        liquid::card_accent(ui, Some(liquid::PRIMARY), |ui| {
            ui.set_width(ui.available_width());
            liquid::section_title(ui, "游戏环境", liquid::PRIMARY);
            ui.horizontal(|ui| {
                ui.label("游戏目录：");
                ui.add(
                    egui::TextEdit::singleline(&mut self.game_path_input)
                        .hint_text(r"如 D:\Steam\steamapps\common\Stardew Valley")
                        .desired_width(420.0),
                );
                if ui.button("浏览…").clicked() {
                    if let Some(p) = pick_folder() {
                        self.game_path_input = p.display().to_string();
                    }
                }
                if liquid::cta_button(ui, "保存并重扫", true).clicked() {
                    let p = self.game_path_input.trim().to_string();
                    self.settings.game_path = if p.is_empty() { None } else { Some(p) };
                    let _ = self.settings.save();
                    self.refresh();
                    self.status = "游戏路径已保存".to_string();
                }
                if ui.small_button("重新自动检测").clicked() {
                    self.settings.game_path = None;
                    let _ = self.settings.save();
                    self.game_path_input.clear();
                    self.refresh();
                }
            });
            match &self.env.game_path {
                Some(g) => ui.label(RichText::new(format!("✅ {}", g.display())).color(liquid::SUCCESS)),
                None => ui.label(
                    RichText::new("⚠ 未找到游戏目录，请手动指定含 Stardew Valley.exe 的文件夹")
                        .color(liquid::WARNING),
                ),
            };
            if let Some(m) = &self.env.mods_path {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Mods：{}", m.display())).weak());
                    if ui.small_button("打开 Mods 目录").clicked() {
                        open_in_explorer(m);
                    }
                });
            }
            if let Some(g) = &self.env.game_path {
                ui.horizontal(|ui| {
                    if ui.button("启动游戏 (SMAPI)").clicked() {
                        launch(&g.join("StardewModdingAPI.exe"), g);
                    }
                    let exe = if g.join("Stardew Valley.exe").exists() {
                        g.join("Stardew Valley.exe")
                    } else {
                        g.join("StardewValley.exe")
                    };
                    if ui.button("启动游戏 (原版)").clicked() {
                        launch(&exe, g);
                    }
                    if ui.small_button("打开游戏目录").clicked() {
                        open_in_explorer(g);
                    }
                });
            }
        });
        ui.add_space(8.0);

        // API Key（锁定发布版不需要：模组只从镜像服务器拉取）
        if !cfg!(feature = "locked-mirror") {
            liquid::card(ui, |ui| {
                ui.set_width(ui.available_width());
                liquid::section_title(ui, "Nexus Mods API Key", liquid::PRIMARY);
                ui.label(
                    RichText::new(
                        "免费账号即可：登录 nexusmods.com → 账号设置 → API 页面生成个人 API Key。\
                         仅用于读取模组列表/文件信息，下载本身不需要它。",
                    )
                    .size(11.0)
                    .weak(),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.api_key_input)
                            .hint_text("粘贴 API Key")
                            .desired_width(460.0)
                            .password(true),
                    );
                    if liquid::cta_button(ui, "保存", true).clicked() {
                        self.settings.nexus_api_key = self.api_key_input.trim().to_string();
                        let _ = self.settings.save();
                        self.status = "API Key 已保存".to_string();
                    }
                    if ui.small_button("打开 API Key 页面").clicked() {
                        crate::watch::open_external(
                            "https://www.nexusmods.com/users/myaccount?tab=api",
                        );
                    }
                });
            });
            ui.add_space(8.0);
        }

        // 镜像源
        liquid::card_accent(ui, Some(liquid::PRIMARY), |ui| {
            ui.set_width(ui.available_width());
            liquid::section_title(ui, "镜像源（直装下载地址）", liquid::PRIMARY);
            if cfg!(feature = "locked-mirror") {
                ui.label(
                    RichText::new(format!(
                        "🔒 已锁定官方镜像：{}（所有模组仅从该服务器拉取）",
                        crate::model::DEFAULT_MIRROR_URL
                    ))
                    .size(11.5)
                    .color(liquid::SUCCESS),
                );
            } else {
                ui.label(
                    RichText::new(
                        "默认使用云端模组库，开箱即用，无需自己开服务器。\
                         高级用户自建镜像时，可改成本地地址（如 http://localhost:8770）。",
                    )
                    .size(11.0)
                    .weak(),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.mirror_url_input)
                            .hint_text(crate::model::DEFAULT_MIRROR_URL)
                            .desired_width(460.0),
                    );
                    if liquid::cta_button(ui, "保存", true).clicked() {
                        self.settings.mirror_url = self.mirror_url_input.trim().to_string();
                        let _ = self.settings.save();
                        self.mirror_loaded = false;
                        self.status = "镜像源地址已保存".to_string();
                    }
                    if ui.small_button("测试连接").clicked() {
                        self.fetch_mirror();
                    }
                });
            }
        });
        ui.add_space(8.0);

        // SMAPI
        liquid::card_accent(ui, Some(liquid::PRIMARY), |ui| {
            ui.set_width(ui.available_width());
            liquid::section_title(ui, "SMAPI（运行模组的必需加载器）", liquid::PRIMARY);
            ui.horizontal(|ui| {
                match &self.env.smapi_version {
                    Some(v) => ui.label(RichText::new(format!("已安装版本：{v}")).color(liquid::SUCCESS)),
                    None => ui
                        .label(RichText::new("未检测到 SMAPI（StardewModdingAPI.exe）").color(liquid::WARNING)),
                };
                if liquid::cta_button(ui, "一键安装 / 更新 SMAPI", !self.smapi_busy).clicked() {
                    self.install_smapi_bg();
                }
                if self.smapi_busy {
                    ui.spinner();
                }
            });
            ui.label(
                RichText::new("自动从 GitHub 下载官方安装包并静默安装，过程中可能短暂弹出安装器窗口。")
                    .size(11.0)
                    .weak(),
            );
        });
    }
}

// ---------- 小工具 ----------

fn state_visual(s: &WState) -> (&'static str, Color32) {
    match s {
        WState::Downloading => ("⬇", liquid::PRIMARY),
        WState::Installing => ("📦", liquid::WARNING),
        WState::Finished(true, _) => ("✅", liquid::SUCCESS),
        WState::Finished(false, _) => ("❌", liquid::DANGER),
    }
}

fn state_label(s: &WState) -> &'static str {
    match s {
        WState::Downloading => "浏览器下载中",
        WState::Installing => "安装中",
        WState::Finished(true, _) => "已安装",
        WState::Finished(false, _) => "异常",
    }
}

fn mirror_state_label(s: &MJobState) -> &'static str {
    match s {
        MJobState::Downloading => "镜像下载中",
        MJobState::Installing => "安装中",
        MJobState::Finished(true, _) => "已安装",
        MJobState::Finished(false, _) => "失败",
    }
}

/// 模组库左侧分类栏的一行：选中蓝底高亮，悬停浅灰。
fn mirror_cat_row(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let row_h = 30.0_f32;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, egui::CornerRadius::same(8), liquid::PRIMARY_SOFT);
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 1.0, rect.top() + 7.0),
            egui::vec2(3.0, row_h - 14.0),
        );
        painter.rect_filled(bar, egui::CornerRadius::same(2), liquid::PRIMARY);
    } else if resp.hovered() {
        painter.rect_filled(
            rect,
            egui::CornerRadius::same(8),
            Color32::from_black_alpha(12),
        );
    }
    painter.text(
        egui::pos2(rect.left() + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        if selected {
            liquid::PRIMARY
        } else {
            liquid::text_main()
        },
    );
    resp
}

/// 压平换行并按字符数截断描述文本。
fn truncate_flat(s: &str, max_chars: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    if flat.chars().count() <= max_chars {
        flat
    } else {
        let head: String = flat.chars().take(max_chars).collect();
        format!("{head}…")
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // 拉丁字形优先用 Segoe UI（Windows 上最接近 SF 的系统字体），
    // 缺字（中文）按 family 列表顺序回落到微软雅黑。
    let latin_candidates = [
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\SegoeUI-VF.ttf",
    ];
    for path in latin_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("latin".to_owned(), egui::FontData::from_owned(bytes).into());
            if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                list.insert(0, "latin".to_owned());
            }
            break;
        }
    }
    let cjk_candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\Deng.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    for path in cjk_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    list.push("cjk".to_owned());
                }
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

fn open_in_explorer(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

fn launch(exe: &std::path::Path, cwd: &std::path::Path) {
    if !exe.is_file() {
        return;
    }
    let _ = std::process::Command::new(exe).current_dir(cwd).spawn();
}

fn pick_file() -> Option<PathBuf> {
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
$d = New-Object System.Windows.Forms.OpenFileDialog
$d.Filter = "压缩包 (*.zip)|*.zip|所有文件 (*.*)|*.*"
if ($d.ShowDialog() -eq 'OK') { Write-Output $d.FileName }
"#;
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(PathBuf::from(s)) }
}

fn pick_folder() -> Option<PathBuf> {
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object System.Windows.Forms.FolderBrowserDialog
if ($f.ShowDialog() -eq 'OK') { Write-Output $f.SelectedPath }
"#;
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(PathBuf::from(s)) }
}

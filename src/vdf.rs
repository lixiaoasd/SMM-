//! Steam 文本 VDF（`localconfig.vdf` 这类）的**字节级定点编辑**。
//!
//! 设计前提（均在本机真实文件上验证过）：
//!   * 文件含中文（136147 字节 ≠ 132598 字符），**纯 LF**、无 BOM —— 所以全程按
//!     `&[u8]` 操作，绝不用 `String` 下标切片；
//!   * 全文件唯一的 `//` 出现在**字符串字面量内部**（同时含 `\"` 和 `{}`），
//!     所以括号配对必须**字符串感知**，否则会毁掉那个值；
//!   * `"413150"` 这类字符串在文件里多处出现 —— 必须**路径感知**定位。
//!
//! 编辑策略是「只 splice、不重排」：只替换/插入目标字节区间，其余字节逐字保留。
//! 这是「不破坏用户其它 Steam 设置」的机制性保证。

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VdfErr {
    /// 不是 UTF-8 文本 VDF（例如 UTF-16）。
    NotUtf8,
    /// 字符串或块没有闭合。
    Unterminated,
    /// 结构不符合预期（找不到目标路径、键值形态异常）。
    NoStructure,
}

impl fmt::Display for VdfErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            VdfErr::NotUtf8 => "不是 UTF-8 文本 VDF",
            VdfErr::Unterminated => "VDF 结构未闭合",
            VdfErr::NoStructure => "VDF 结构不符合预期",
        };
        f.write_str(s)
    }
}

impl std::error::Error for VdfErr {}

/// 字节区间（半开）。
pub type Span = (usize, usize);

pub struct Pair {
    /// 键的原始字节（含转义）。
    pub key: Vec<u8>,
    /// 键的闭引号下标（用于推导键值分隔符）。
    pub key_close: usize,
    /// 值的开引号下标。
    pub value_open: usize,
    /// 值的闭引号下标。
    pub value_end: usize,
    /// 值的原始字节（含转义，不含引号）。
    pub value_raw: Vec<u8>,
}

pub struct Block {
    pub key: Vec<u8>,
    /// `{` 与 `}` 的下标。
    pub open: usize,
    pub close: usize,
}

pub enum Entry {
    Pair(Pair),
    Block(Block),
}

pub struct BlockView {
    pub entries: Vec<Entry>,
    /// 本块 `}` 的下标。
    pub close: usize,
}

/// 解析出的文档根块。
pub struct Root {
    pub open: usize,
}

// ---------- 扫描器 ----------

struct Scan<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Scan<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    /// 跳过空白与行注释。只在**字符串外**调用。
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') => self.pos += 1,
                Some(b'/') if self.src.get(self.pos + 1) == Some(&b'/') => {
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        if c == b'\n' {
                            break;
                        }
                    }
                }
                _ => return,
            }
        }
    }

    /// 读取一个带引号的字符串。返回值的原始字节（含转义，不含引号）。
    fn read_quoted(&mut self) -> Result<(Span, Vec<u8>), VdfErr> {
        if self.peek() != Some(b'"') {
            return Err(VdfErr::NoStructure);
        }
        self.pos += 1;
        let start = self.pos;
        let mut raw = Vec::new();
        while let Some(c) = self.peek() {
            match c {
                b'\\' => {
                    raw.push(b'\\');
                    self.pos += 1;
                    match self.peek() {
                        Some(n) => {
                            raw.push(n);
                            self.pos += 1;
                        }
                        None => return Err(VdfErr::Unterminated),
                    }
                }
                b'"' => {
                    let span = (start, self.pos);
                    self.pos += 1;
                    return Ok((span, raw));
                }
                _ => {
                    raw.push(c);
                    self.pos += 1;
                }
            }
        }
        Err(VdfErr::Unterminated)
    }
}

// ---------- 解析 ----------

/// 解析文档根块（首个 token 是键，紧随其后的块）。
pub fn parse_root(src: &[u8]) -> Result<Root, VdfErr> {
    if src.len() >= 2 && src[0] == 0xFF && src[1] == 0xFE {
        return Err(VdfErr::NotUtf8);
    }
    // 容忍 UTF-8 BOM。
    let mut scan = Scan {
        src,
        pos: if src.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 },
    };
    scan.skip_trivia();
    let (_, _key) = scan.read_quoted()?;
    scan.skip_trivia();
    if scan.peek() != Some(b'{') {
        return Err(VdfErr::NoStructure);
    }
    let open = scan.pos;
    block(src, open)?; // 顺带校验根块能完整解析
    Ok(Root { open })
}

/// 列出 `open` 处 `{` 所在块的**直接子项**。
pub fn block(src: &[u8], open: usize) -> Result<BlockView, VdfErr> {
    if src.get(open) != Some(&b'{') {
        return Err(VdfErr::NoStructure);
    }
    let mut scan = Scan {
        src,
        pos: open + 1,
    };
    let mut entries = Vec::new();
    loop {
        scan.skip_trivia();
        match scan.peek() {
            None => return Err(VdfErr::Unterminated),
            Some(b'}') => {
                let close = scan.pos;
                return Ok(BlockView { entries, close });
            }
            Some(b'"') => {
                let (_, key_raw) = scan.read_quoted()?;
                let key_close = scan.pos - 1;
                scan.skip_trivia();
                match scan.peek() {
                    Some(b'{') => {
                        let child_open = scan.pos;
                        let child = block(src, child_open)?;
                        scan.pos = child.close + 1;
                        entries.push(Entry::Block(Block {
                            key: key_raw,
                            open: child_open,
                            close: child.close,
                        }));
                    }
                    Some(b'"') => {
                        let value_open = scan.pos;
                        let (_, value_raw) = scan.read_quoted()?;
                        let value_end = scan.pos - 1;
                        entries.push(Entry::Pair(Pair {
                            key: key_raw,
                            key_close,
                            value_open,
                            value_end,
                            value_raw,
                        }));
                    }
                    _ => return Err(VdfErr::NoStructure),
                }
            }
            _ => return Err(VdfErr::NoStructure),
        }
    }
}

/// 在 `open` 块的直接子项里找名叫 `key` 的块。
///
/// 键比较**不区分大小写**：Steam 自己的写法并不统一 —— 同一个键在不同账号的
/// localconfig.vdf 里会写成 `"apps"` 或 `"Apps"`（实测两种都存在），严格匹配会
/// 直接判成结构不符。
pub fn descend(src: &[u8], open: usize, key: &str) -> Result<Option<Block>, VdfErr> {
    for e in block(src, open)?.entries {
        if let Entry::Block(b) = e {
            if decode(&b.key).eq_ignore_ascii_case(key) {
                return Ok(Some(b));
            }
        }
    }
    Ok(None)
}

/// 沿键链逐层下钻，任一层缺失即 `NoStructure`。**不做全树字符串搜索**。
pub fn descend_path(src: &[u8], mut open: usize, path: &[&str]) -> Result<Block, VdfErr> {
    let mut last: Option<Block> = None;
    for k in path {
        let b = descend(src, open, k)?.ok_or(VdfErr::NoStructure)?;
        open = b.open;
        last = Some(b);
    }
    last.ok_or(VdfErr::NoStructure)
}

/// `Software → Valve → Steam → apps`。
pub fn apps_block(src: &[u8], root_open: usize) -> Result<Block, VdfErr> {
    descend_path(src, root_open, &["Software", "Valve", "Steam", "apps"])
}

/// 读 `apps/<app>/<key>`。结构缺失 → `Err`；app 块或键不存在 → `Ok(None)`。
pub fn get_app_string(src: &[u8], app: &str, key: &str) -> Result<Option<String>, VdfErr> {
    let root = parse_root(src)?;
    let apps = apps_block(src, root.open)?;
    let Some(app_block) = find_child_block(src, apps.open, app)? else {
        return Ok(None);
    };
    for e in block(src, app_block.open)?.entries {
        if let Entry::Pair(p) = e {
            if decode(&p.key).eq_ignore_ascii_case(key) {
                return Ok(Some(decode(&p.value_raw)));
            }
        }
    }
    Ok(None)
}

/// 写 `apps/<app>/<key>`。四种情况见函数体注释。返回值是**新的完整文件字节**。
pub fn set_app_string(src: &[u8], app: &str, key: &str, value: &str) -> Result<Vec<u8>, VdfErr> {
    let root = parse_root(src)?;
    let apps = apps_block(src, root.open)?;
    let encoded = encode(value);

    let Some(app_block) = find_child_block(src, apps.open, app)? else {
        // ③ app 块不存在：在 apps 块尾插入一个完整的新块。
        let apps_indent = indent_of_line(src, apps.open);
        let (at, text) = append_before_close(src, apps.close, &apps_indent, |ci| {
            let mut t = Vec::new();
            let sep = b"\t\t";
            t.extend_from_slice(ci);
            t.extend_from_slice(b"\"");
            t.extend_from_slice(app.as_bytes());
            t.extend_from_slice(b"\"\n");
            t.extend_from_slice(ci);
            t.extend_from_slice(b"{\n");
            t.extend_from_slice(ci);
            t.push(b'\t');
            t.extend_from_slice(b"\"");
            t.extend_from_slice(key.as_bytes());
            t.extend_from_slice(b"\"");
            t.extend_from_slice(sep);
            t.extend_from_slice(b"\"");
            t.extend_from_slice(&encoded);
            t.extend_from_slice(b"\"\n");
            t.extend_from_slice(ci);
            t.extend_from_slice(b"}\n");
            t
        });
        return Ok(splice(src, at, 0, &text));
    };

    let app_view = block(src, app_block.open)?;
    let existing = app_view.entries.iter().find_map(|e| match e {
        Entry::Pair(p) if decode(&p.key).eq_ignore_ascii_case(key) => Some(p),
        _ => None,
    });

    if let Some(p) = existing {
        // ① 键已存在：只换值区间的字节，键与分隔符原样保留。
        return Ok(splice(
            src,
            p.value_open + 1,
            p.value_end - (p.value_open + 1),
            &encoded,
        ));
    }

    // ② 键不存在：在 app 块尾插一行。
    let app_indent = indent_of_line(src, app_block.open);
    let sep = sibling_sep(src, &app_view.entries);
    let (at, text) = append_before_close(src, app_block.close, &app_indent, |ci| {
        let mut t = Vec::new();
        t.extend_from_slice(ci);
        t.extend_from_slice(b"\"");
        t.extend_from_slice(key.as_bytes());
        t.extend_from_slice(b"\"");
        t.extend_from_slice(&sep);
        t.extend_from_slice(b"\"");
        t.extend_from_slice(&encoded);
        t.extend_from_slice(b"\"\n");
        t
    });
    Ok(splice(src, at, 0, &text))
}

fn find_child_block(src: &[u8], open: usize, key: &str) -> Result<Option<Block>, VdfErr> {
    descend(src, open, key)
}


/// 取同一块里任一兄弟键值对的「键闭引号 → 值开引号」原始字节作为分隔符。
fn sibling_sep(src: &[u8], entries: &[Entry]) -> Vec<u8> {
    for e in entries {
        if let Entry::Pair(p) = e {
            let from = p.key_close + 1;
            if p.value_open > from {
                return src[from..p.value_open].to_vec();
            }
        }
    }
    // 退而求其次：整块的默认分隔符（实测 localconfig.vdf 为两个 tab）。
    b"\t\t".to_vec()
}

/// 在块 `close` 之前插入内容。`render` 收到「子项缩进」并返回完整插入字节。
/// 返回 (插入位置, 插入字节)。
fn append_before_close(
    src: &[u8],
    close: usize,
    fallback_indent: &[u8],
    render: impl FnOnce(&[u8]) -> Vec<u8>,
) -> (usize, Vec<u8>) {
    let ls = line_start(src, close);
    let leading = &src[ls..close];
    let own_line = ls > 0 && leading.iter().all(|b| *b == b'\t' || *b == b' ');
    let close_indent: Vec<u8> = if own_line {
        leading.to_vec()
    } else {
        fallback_indent.to_vec()
    };
    let mut child_indent = close_indent.clone();
    child_indent.push(b'\t');

    let mut text = Vec::new();
    if !own_line {
        // 单行块（`{}` / `{ "k" "v" }`）：先把 `}` 推到下一行。
        text.push(b'\n');
    }
    text.extend_from_slice(&render(&child_indent));
    if !own_line {
        text.extend_from_slice(&close_indent);
    }
    (if own_line { ls } else { close }, text)
}

/// `at` 所在行的起始下标。
fn line_start(src: &[u8], at: usize) -> usize {
    let mut i = at.min(src.len());
    while i > 0 && src[i - 1] != b'\n' {
        i -= 1;
    }
    i
}

/// `at` 所在行的前导 tab/空格。
fn indent_of_line(src: &[u8], at: usize) -> Vec<u8> {
    let ls = line_start(src, at);
    let mut end = ls;
    while end < src.len() && (src[end] == b'\t' || src[end] == b' ') {
        end += 1;
    }
    src[ls..end].to_vec()
}

fn splice(src: &[u8], at: usize, del: usize, ins: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() + ins.len());
    out.extend_from_slice(&src[..at]);
    out.extend_from_slice(ins);
    out.extend_from_slice(&src[at + del..]);
    out
}

// ---------- 字符串编解码 ----------

/// VDF 值编码：`\` → `\\`、`"` → `\"`。
fn encode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            _ => out.push(b),
        }
    }
    out
}

/// VDF 值解码（`\\` `\"` `\n` `\t`；未知转义原样保留）。
pub fn decode(raw: &[u8]) -> String {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' && i + 1 < raw.len() {
            match raw[i + 1] {
                b'\\' => out.push(b'\\'),
                b'"' => out.push(b'"'),
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                other => {
                    out.push(b'\\');
                    out.push(other);
                }
            }
            i += 2;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 字符串外的 `{` / `}` 是否收支平衡（写盘前自检用）。
pub fn braces_balanced(src: &[u8]) -> bool {
    let mut scan = Scan { src, pos: 0 };
    let mut depth: i64 = 0;
    while let Some(c) = scan.peek() {
        match c {
            b'"' => {
                if scan.read_quoted().is_err() {
                    return false;
                }
            }
            b'{' => {
                depth += 1;
                scan.pos += 1;
            }
            b'}' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
                scan.pos += 1;
            }
            _ => scan.pos += 1,
        }
    }
    depth == 0
}

// ---------- 测试 ----------

#[cfg(test)]
mod tests {
    use super::*;

    /// 仿真实文件结构：含中文、含一个「值里带 \" 和 {} 和 // 的转义 JSON」、
    /// 含位置更深的同名 "413150" 兄弟键。
    const FIXTURE: &str = concat!(
        "\"UserLocalConfigStore\"\n",
        "{\n",
        "\t\"Software\"\n",
        "\t{\n",
        "\t\t\"Valve\"\n",
        "\t\t{\n",
        "\t\t\t\"Steam\"\n",
        "\t\t\t{\n",
        "\t\t\t\t\"friends\"\n",
        "\t\t\t\t{\n",
        "\t\t\t\t\t\"413150\"\t\t\"should-not-be-touched\"\n",
        "\t\t\t\t\t\"blob\"\t\t\"{\\\"url\\\":\\\"https://x//y\\\"}\"\n",
        "\t\t\t\t}\n",
        "\t\t\t\t\"apps\"\n",
        "\t\t\t\t{\n",
        "\t\t\t\t\t\"2280\"\n",
        "\t\t\t\t\t{\n",
        "\t\t\t\t\t\t\"LastPlayed\"\t\t\"111\"\n",
        "\t\t\t\t\t}\n",
        "\t\t\t\t\t\"413150\"\n",
        "\t\t\t\t\t{\n",
        "\t\t\t\t\t\t\"LastPlayed\"\t\t\"1790315158\"\n",
        "\t\t\t\t\t\t\"中文备注\"\t\t\"测试\"\n",
        "\t\t\t\t\t}\n",
        "\t\t\t\t}\n",
        "\t\t\t\t\"SmallMode\"\t\t\"0\"\n",
        "\t\t\t}\n",
        "\t\t}\n",
        "\t}\n",
        "}\n",
    );

    const SMAPI_MIN: &str = r#""D:\SteamLibrary\steamapps\common\Stardew Valley\StardewModdingAPI.exe" %command%"#;

    fn fixture_without_app() -> String {
        FIXTURE.replace(
            concat!(
                "\t\t\t\t\t\"413150\"\n",
                "\t\t\t\t\t{\n",
                "\t\t\t\t\t\t\"LastPlayed\"\t\t\"1790315158\"\n",
                "\t\t\t\t\t\t\"中文备注\"\t\t\"测试\"\n",
                "\t\t\t\t\t}\n",
            ),
            "",
        )
    }

    fn read(src: &[u8], key: &str) -> Option<String> {
        get_app_string(src, "413150", key).unwrap()
    }

    #[test]
    fn reads_existing_and_missing() {
        let src = FIXTURE.as_bytes();
        assert_eq!(read(src, "LastPlayed").as_deref(), Some("1790315158"));
        assert_eq!(read(src, "中文备注").as_deref(), Some("测试"));
        assert_eq!(read(src, "LaunchOptions"), None);
    }

    #[test]
    fn key_case_is_ignored() {
        // 真实文件里 Steam 可能写成 "Apps"/"LaunchOptions"，也可能写法大小写不同。
        let src = FIXTURE.replace("\"apps\"", "\"Apps\"");
        let src = set_app_string(src.as_bytes(), "413150", "launchoptions", "V").unwrap();
        assert_eq!(
            get_app_string(&src, "413150", "LaunchOptions").unwrap().as_deref(),
            Some("V")
        );
        // 大写块键也要能被下钻到。
        assert_eq!(
            get_app_string(&src, "413150", "LastPlayed").unwrap().as_deref(),
            Some("1790315158")
        );
    }

    #[test]
    fn case1_replaces_value_only() {
        // 预置一个已有值，再替换：只允许值区间变化，其余字节逐字保留。
        let src = set_app_string(FIXTURE.as_bytes(), "413150", "LaunchOptions", "OLD").unwrap();
        let at = find(b"\"OLD\"", &src).expect("应已插入 OLD") + 1;
        let expect = splice(&src, at, 3, &encode(SMAPI_MIN));
        let src2 = set_app_string(&src, "413150", "LaunchOptions", SMAPI_MIN).unwrap();
        assert_eq!(src2, expect, "除值区间外不得有任何字节变化");
        assert_eq!(read(&src2, "LaunchOptions").as_deref(), Some(SMAPI_MIN));
    }

    #[test]
    fn case2_inserts_key_when_absent() {
        let src = set_app_string(FIXTURE.as_bytes(), "413150", "LaunchOptions", SMAPI_MIN).unwrap();
        assert!(braces_balanced(&src));
        assert_eq!(read(&src, "LaunchOptions").as_deref(), Some(SMAPI_MIN));
        // 分隔符取自兄弟项（两个 tab）
        assert!(find(b"\"LaunchOptions\"\t\t\"", &src).is_some());
        // 缩进 = app 块 `}` 所在行缩进 + 1 tab（实测 6 个 tab）
        assert!(find(b"\t\t\t\t\t\t\"LaunchOptions\"", &src).is_some());
        // 其它键不受影响
        assert_eq!(read(&src, "LastPlayed").as_deref(), Some("1790315158"));
        assert_eq!(read(&src, "中文备注").as_deref(), Some("测试"));
    }

    #[test]
    fn case3_inserts_app_block_when_missing() {
        let base = fixture_without_app();
        assert_eq!(read(base.as_bytes(), "LastPlayed"), None);
        let src =
            set_app_string(base.as_bytes(), "413150", "LaunchOptions", SMAPI_MIN).unwrap();
        assert!(braces_balanced(&src));
        assert_eq!(read(&src, "LaunchOptions").as_deref(), Some(SMAPI_MIN));
        // apps 的直接子项从 2 个（原 1 + 新 1）变为 2
        let root = parse_root(&src).unwrap();
        let apps = apps_block(&src, root.open).unwrap();
        let n = block(&src, apps.open).unwrap().entries.len();
        assert_eq!(n, 2);
        // 新块缩进 5 个 tab
        assert!(find(b"\t\t\t\t\t\"413150\"\n", &src).is_some());
    }

    #[test]
    fn case4_clear_writes_empty_string() {
        let src = set_app_string(FIXTURE.as_bytes(), "413150", "LaunchOptions", SMAPI_MIN).unwrap();
        let src = set_app_string(&src, "413150", "LaunchOptions", "").unwrap();
        assert_eq!(read(&src, "LaunchOptions").as_deref(), Some(""));
        assert!(find(b"\"LaunchOptions\"\t\t\"\"", &src).is_some());
        assert!(braces_balanced(&src));
    }

    #[test]
    fn escapes_roundtrip() {
        let src = set_app_string(FIXTURE.as_bytes(), "413150", "LaunchOptions", r#"a"b\c"#).unwrap();
        assert_eq!(read(&src, "LaunchOptions").as_deref(), Some(r#"a"b\c"#));
        assert!(find(br#""a\"b\\c""#, &src).is_some());
    }

    #[test]
    fn ignores_appid_in_strings_and_other_blocks() {
        // 回归金标准：friends 块里的 "413150" 键、以及值内含 \" {} // 的转义 JSON
        // 都不得被改动。
        let src = set_app_string(FIXTURE.as_bytes(), "413150", "LaunchOptions", SMAPI_MIN).unwrap();
        assert_eq!(read(&src, "LaunchOptions").as_deref(), Some(SMAPI_MIN));
        assert!(find(b"\"413150\"\t\t\"should-not-be-touched\"", &src).is_some());
        assert!(find(br#""blob"		"{\"url\":\"https://x//y\"}""#, &src).is_some());
        // 唯一变化在 apps 子树里
        let a = find(b"\"friends\"", &src);
        let b = find(b"\"friends\"", FIXTURE.as_bytes());
        assert_eq!(a, b);
    }

    #[test]
    fn refuses_when_structure_missing() {
        let bad = b"\"SomethingElse\"\n{\n\t\"a\"\t\t\"b\"\n}\n";
        assert_eq!(
            set_app_string(bad, "413150", "LaunchOptions", "x"),
            Err(VdfErr::NoStructure)
        );
        assert_eq!(get_app_string(bad, "413150", "LaunchOptions"), Err(VdfErr::NoStructure));
    }

    #[test]
    fn rejects_utf16() {
        let src = [0xFFu8, 0xFE, 0x61, 0x00];
        assert_eq!(get_app_string(&src, "1", "2"), Err(VdfErr::NotUtf8));
    }

    #[test]
    fn handles_empty_and_inline_blocks() {
        let inline = "\"UserLocalConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"apps\"\n\t\t\t\t{\n\t\t\t\t\t\"413150\"\t\t{}\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n";
        let src = set_app_string(inline.as_bytes(), "413150", "LaunchOptions", "v").unwrap();
        assert!(braces_balanced(&src));
        assert_eq!(read(&src, "LaunchOptions").as_deref(), Some("v"));

        let empty_apps = "\"UserLocalConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"apps\"\n\t\t\t\t{\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n";
        let src = set_app_string(empty_apps.as_bytes(), "413150", "LaunchOptions", "v").unwrap();
        assert!(braces_balanced(&src));
        assert_eq!(read(&src, "LaunchOptions").as_deref(), Some("v"));
    }

    #[test]
    fn handles_no_trailing_newline() {
        let no_nl = FIXTURE.trim_end_matches('\n');
        let src = set_app_string(no_nl.as_bytes(), "413150", "LaunchOptions", "v").unwrap();
        assert!(braces_balanced(&src));
        assert_eq!(read(&src, "LaunchOptions").as_deref(), Some("v"));
    }

    #[test]
    fn handles_crlf() {
        let crlf = FIXTURE.replace('\n', "\r\n");
        let src = set_app_string(crlf.as_bytes(), "413150", "LaunchOptions", "v").unwrap();
        assert!(braces_balanced(&src));
        assert_eq!(read(&src, "LaunchOptions").as_deref(), Some("v"));
    }

    #[test]
    fn braces_balanced_detects_imbalance() {
        assert!(braces_balanced(FIXTURE.as_bytes()));
        assert!(!braces_balanced(b"\"a\"\n{\n"));
    }

    fn find(needle: &[u8], hay: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }
}

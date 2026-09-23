//! 界面与日志文案的本地化。
//!
//! 词条存放在 `assets/i18n/*.toml`，编译期嵌入二进制，运行期不读外部文件。
//! TOML 用嵌套表书写，加载时展平为 `"err.config_read"` 这样的点分键。
//!
//! 当前语言是进程级全局状态：CLI 启动时按配置设定一次，GUI 在用户切换语言时更新，
//! 使得日志与错误提示随界面语言即时改变，无需层层传递语言参数。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const ZH_CN_SRC: &str = include_str!("../../../assets/i18n/zh-CN.toml");
const EN_US_SRC: &str = include_str!("../../../assets/i18n/en-US.toml");

/// 界面语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Language {
    /// 简体中文
    #[default]
    #[serde(rename = "zh-CN", alias = "zh", alias = "zh_CN")]
    ZhCn,
    /// English
    #[serde(rename = "en-US", alias = "en", alias = "en_US")]
    EnUs,
}

impl Language {
    /// 全部可选语言，顺序即 GUI 下拉框的显示顺序。
    pub const ALL: [Language; 2] = [Language::ZhCn, Language::EnUs];

    /// BCP 47 标签，也是配置文件中的写法。
    pub const fn tag(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
        }
    }

    /// 语言的自称，用于 GUI 下拉框：无论当前界面是什么语言都显示本族名。
    pub const fn native_name(self) -> &'static str {
        match self {
            Self::ZhCn => "简体中文",
            Self::EnUs => "English",
        }
    }

    /// 解析 BCP 47 标签，无法识别时回退到简体中文。
    pub fn from_tag(tag: &str) -> Self {
        let lower = tag.to_ascii_lowercase();
        if lower.starts_with("en") {
            Self::EnUs
        } else {
            Self::ZhCn
        }
    }

    /// 按环境变量推测系统语言。Windows 上这些变量通常为空，会回退到默认值。
    pub fn detect() -> Self {
        for var in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
            if let Ok(value) = std::env::var(var) {
                if !value.is_empty() && value != "C" && value != "POSIX" {
                    return Self::from_tag(&value);
                }
            }
        }
        Self::default()
    }

    const fn index(self) -> u8 {
        match self {
            Self::ZhCn => 0,
            Self::EnUs => 1,
        }
    }

    const fn from_index(index: u8) -> Self {
        match index {
            1 => Self::EnUs,
            _ => Self::ZhCn,
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tag())
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// 设定当前语言，影响此后所有[`tr`]调用。
pub fn set_language(lang: Language) {
    CURRENT.store(lang.index(), Ordering::Relaxed);
}

/// 读取当前语言。
pub fn language() -> Language {
    Language::from_index(CURRENT.load(Ordering::Relaxed))
}

/// 按当前语言取词条。
///
/// 缺词条时依次回退：当前语言 → 英文 → 键名本身。返回键名而非 panic，
/// 是为了让漏翻的条目在界面上直接暴露出来，同时不影响程序运行。
pub fn tr(key: &str) -> &'static str {
    tr_in(language(), key)
}

/// 按指定语言取词条，不受全局语言影响。
pub fn tr_in(lang: Language, key: &str) -> &'static str {
    catalog(lang)
        .get(key)
        .or_else(|| catalog(Language::EnUs).get(key))
        .map(String::as_str)
        .unwrap_or_else(|| leak_key(key))
}

/// 取词条并替换 `{name}` 占位符。
///
/// ```ignore
/// tr_args("log.rule_listening", &[("addr", "127.0.0.1:8080"), ("detail", "反向")]);
/// ```
///
/// 未被 `args` 覆盖的占位符原样保留，便于在界面上看出缺了哪个参数。
pub fn tr_args(key: &str, args: &[(&str, &str)]) -> String {
    interpolate(tr(key), args)
}

/// [`tr_args`]的指定语言版本。
pub fn tr_args_in(lang: Language, key: &str, args: &[(&str, &str)]) -> String {
    interpolate(tr_in(lang, key), args)
}

fn interpolate(template: &str, args: &[(&str, &str)]) -> String {
    if args.is_empty() || !template.contains('{') {
        return template.to_owned();
    }
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match args.iter().find(|(k, _)| *k == name) {
                    Some((_, value)) => out.push_str(value),
                    // 无对应参数时保留原样，方便定位漏传的占位符
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            // 没有闭合花括号，剩余部分整体照抄
            None => {
                out.push('{');
                out.push_str(after);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn catalog(lang: Language) -> &'static HashMap<String, String> {
    static ZH: OnceLock<HashMap<String, String>> = OnceLock::new();
    static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
    match lang {
        Language::ZhCn => ZH.get_or_init(|| parse(ZH_CN_SRC, "zh-CN")),
        Language::EnUs => EN.get_or_init(|| parse(EN_US_SRC, "en-US")),
    }
}

/// 展平嵌套 TOML 表为点分键。词条文件随二进制一起编译，
/// 解析失败说明资源文件写坏了，属于编译期就该发现的问题，故直接 panic。
fn parse(src: &str, tag: &str) -> HashMap<String, String> {
    let root: toml::Value = toml::from_str(src)
        .unwrap_or_else(|e| panic!("i18n catalog {tag} is malformed: {e}"));
    let mut out = HashMap::new();
    flatten(&root, String::new(), &mut out);
    out
}

fn flatten(value: &toml::Value, prefix: String, out: &mut HashMap<String, String>) {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(child, path, out);
            }
        }
        toml::Value::String(text) => {
            out.insert(prefix, text.clone());
        }
        // 词条文件只应含字符串，其他类型忽略
        _ => {}
    }
}

/// 缺失的键以自身作为文案返回。键的总数有限且来自代码常量，
/// 泄漏可忽略，换来的是 `tr` 可以返回 `&'static str`。
fn leak_key(key: &str) -> &'static str {
    static MISSING: OnceLock<parking_lot::Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut map = MISSING
        .get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
        .lock();
    if let Some(existing) = map.get(key) {
        return existing;
    }
    let leaked: &'static str = Box::leak(key.to_owned().into_boxed_str());
    map.insert(key.to_owned(), leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_catalogs_parse_and_share_keys() {
        let zh = catalog(Language::ZhCn);
        let en = catalog(Language::EnUs);
        assert!(!zh.is_empty(), "zh-CN catalog is empty");

        let mut missing: Vec<_> = zh.keys().filter(|k| !en.contains_key(*k)).collect();
        missing.extend(en.keys().filter(|k| !zh.contains_key(*k)));
        missing.sort();
        assert!(missing.is_empty(), "keys present in only one catalog: {missing:?}");
    }

    #[test]
    fn interpolation_fills_and_preserves() {
        assert_eq!(
            interpolate("{a} → {b}", &[("a", "x"), ("b", "y")]),
            "x → y"
        );
        // 未传的占位符保留原样
        assert_eq!(interpolate("{a} → {b}", &[("a", "x")]), "x → {b}");
        // 没有闭合花括号时不丢字符
        assert_eq!(interpolate("{unclosed", &[("a", "x")]), "{unclosed");
        assert_eq!(interpolate("no placeholders", &[("a", "x")]), "no placeholders");
    }

    #[test]
    fn missing_key_returns_key_itself() {
        assert_eq!(tr_in(Language::ZhCn, "no.such.key"), "no.such.key");
    }

    #[test]
    fn language_tags_round_trip() {
        for lang in Language::ALL {
            assert_eq!(Language::from_tag(lang.tag()), lang);
        }
    }
}

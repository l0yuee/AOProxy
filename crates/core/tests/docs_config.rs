//! 文档里的配置示例必须真的能被解析。
//!
//! 这个测试存在的原因：文档曾把上游和认证的类型字段写成 `type =`，而结构体上的
//! 字段叫 `kind`，且带 `#[serde(deny_unknown_fields)]`——照文档抄的配置会被 serde
//! 判为未知字段、整份配置加载失败。纯人工核对挡不住这类漂移，于是把文档当输入
//! 喂给真正的解析器。
//!
//! 覆盖方式：
//! - 每个 ```toml 块都过一遍反序列化，抓字段名拼错、枚举取值不存在、类型不匹配。
//! - 不含 `[rules.tls]` 的块再跑一次 [`Config::validate`]，抓端口冲突、目标 URL 非法。
//!   带 TLS 的块跳过校验：文档里的证书路径是 `/etc/letsencrypt/...` 这类示例，
//!   本机不存在，而 `TlsConfig::validate` 会检查文件是否存在。

use std::path::{Path, PathBuf};

use aoproxy_core::Config;

/// 仓库根目录。`CARGO_MANIFEST_DIR` 指向 `crates/core`，上跳两级即是根。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("解析仓库根目录")
}

/// 一个从文档里抠出来的 toml 代码块。
struct Block {
    /// 文档路径，断言失败时用来定位。
    file: &'static str,
    /// 代码块首行在文档中的行号（1 起）。
    line: usize,
    /// 已补全成完整配置的 toml 原文。
    toml: String,
    /// 是否含 TLS 小节（决定跑不跑 `validate`）。
    has_tls: bool,
}

/// 提取所有 ```toml 块，并把片段补成完整配置。
///
/// 文档里的块有三种形态：整份配置（以 `version` 开头）、规则数组片段（`[[rules]]`
/// 开头）、子表片段（`[app]` / `[rules.auth]` 等）。后两种单独喂给 serde 会因缺少
/// 上文而解析失败，所以按形态补上最小前缀——补的是脚手架，字段名仍来自文档。
fn blocks(file: &'static str, text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut lines = text.lines().enumerate();

    while let Some((idx, line)) = lines.next() {
        if line.trim_start() != "```toml" {
            continue;
        }
        let start = idx + 2; // 围栏下一行，转 1 起行号
        let mut body = Vec::new();
        for (_, l) in lines.by_ref() {
            if l.trim_start().starts_with("```") {
                break;
            }
            body.push(l);
        }

        let raw = body.join("\n");
        let first = body
            .iter()
            .map(|l| l.trim())
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap_or("");

        // 按片段形态补前缀
        let toml = if first.starts_with("version") {
            raw.clone()
        } else if first.starts_with("[[rules]]") || first.starts_with("[app]") {
            format!("version = 1\n{raw}")
        } else if first.starts_with("[rules.") {
            // 子表要挂在某条规则下：给一条反向规则当宿主。反向模式需要 target，
            // 顺手满足；token 认证也只在反向模式下合法。
            format!(
                "version = 1\n\
                 [[rules]]\n\
                 id = \"doc\"\n\
                 name = \"doc\"\n\
                 mode = \"reverse\"\n\
                 listen = \"127.0.0.1:18080\"\n\
                 target = \"https://api.anthropic.com\"\n\
                 {raw}"
            )
        } else {
            continue; // 不是配置片段（比如纯字段说明表）
        };

        let has_tls = raw.contains("[rules.tls]");
        out.push(Block {
            file,
            line: start,
            toml,
            has_tls,
        });
    }

    out
}

fn all_blocks() -> Vec<Block> {
    let root = repo_root();
    let mut out = Vec::new();
    for rel in [
        "README.md",
        "docs/config-reference.md",
        "docs/deploy-linux.md",
    ] {
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读取 {} 失败：{e}", path.display()));
        out.extend(blocks(rel, &text));
    }
    out
}

/// 文档里的每个配置块都能被 serde 解析。
///
/// `deny_unknown_fields` 意味着字段名写错就是硬失败，正是要抓的那类错。
#[test]
fn documented_configs_deserialize() {
    let found = all_blocks();
    assert!(
        found.len() >= 10,
        "只提取到 {} 个配置块，提取逻辑可能失效了",
        found.len()
    );

    for b in &found {
        if let Err(e) = toml::from_str::<Config>(&b.toml) {
            panic!("{}:{} 的配置示例解析失败：{e}", b.file, b.line);
        }
    }
}

/// 不带 TLS 的配置块还要能通过业务校验。
#[test]
fn documented_configs_validate() {
    for b in all_blocks().iter().filter(|b| !b.has_tls) {
        let config: Config = toml::from_str(&b.toml)
            .unwrap_or_else(|e| panic!("{}:{} 解析失败：{e}", b.file, b.line));
        if let Err(e) = config.validate() {
            panic!("{}:{} 的配置示例校验失败：{e}", b.file, b.line);
        }
    }
}

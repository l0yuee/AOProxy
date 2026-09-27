//! 代理引擎：管理多条规则的生命周期。
//!
//! [`Engine`] 持有全部规则的运行状态，提供启动、停止和查询接口。
//! 每条规则在独立的 tokio 任务中运行，通过 [`CancellationToken`] 优雅停止。

pub mod auth;
pub mod counter;
pub mod forward;
pub mod http_relay;
pub mod listener;
pub mod reverse;
pub mod tls;
pub mod upstream;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{AppConfig, Config, Rule};
use crate::error::{Error, Result};
use crate::status::Stats;

/// 单条规则的运行状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleStatus {
    Stopped,
    Starting,
    Running,
    Failed(String),
}

impl std::fmt::Display for RuleStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => f.write_str("stopped"),
            Self::Starting => f.write_str("starting"),
            Self::Running => f.write_str("running"),
            Self::Failed(reason) => write!(f, "failed: {reason}"),
        }
    }
}

/// 单条规则的完整运行时状态（状态 + 统计）。
pub struct RuleState {
    pub status: RuleStatus,
    pub stats: Arc<Stats>,
    /// 取消令牌，调用 `cancel()` 即停止该规则。
    cancel: Option<CancellationToken>,
    handle: Option<JoinHandle<()>>,
    /// 启动代数：每次启动与每次叫停都加一。
    ///
    /// 启动要先绑定端口（await），监听任务退出时要回写状态，这两处都拿自己记下的
    /// 代数与当前值核对：对不上说明这期间规则已被叫停、删除或重新启动过，手里的
    /// 那份状态已经不归自己管。否则旧任务收尾时会把新实例的状态改成已停止、清掉
    /// 新实例的取消令牌。
    generation: u64,
}

impl std::fmt::Debug for RuleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleState")
            .field("status", &self.status)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl RuleState {
    fn new() -> Self {
        Self {
            status: RuleStatus::Stopped,
            stats: Arc::new(Stats::new()),
            cancel: None,
            handle: None,
            generation: 0,
        }
    }
}

type StateMap = HashMap<String, RuleState>;

/// 代理引擎：进程级单例，管理全部规则的生命周期。
pub struct Engine {
    config: Arc<RwLock<Config>>,
    states: Arc<RwLock<StateMap>>,
    /// 配置文件路径。`None` 表示这份配置不来自磁盘（CLI 的临时规则、单元测试），
    /// 此时改动只存在于内存中，[`Engine::persist`] 静默跳过写盘。
    /// GUI 可以换用另一个配置文件（[`Engine::switch_config`]），所以带锁。
    config_path: RwLock<Option<PathBuf>>,
    /// 串行化写盘。两次保存若交错进行，要么后落盘的是较旧的快照，
    /// 要么两边同时写同一个临时文件，把内容写花。
    persist_lock: Mutex<()>,
}

impl Engine {
    /// 从配置创建引擎，初始化规则状态表（全部为 Stopped）。
    ///
    /// 这样构造的引擎不关联配置文件，规则改动不会落盘。要持久化请用
    /// [`Engine::with_config_path`]。
    pub fn new(config: Config) -> Self {
        Self::build(config, None)
    }

    /// 从配置创建引擎，并记住它来自哪个文件，使规则改动能写回磁盘。
    pub fn with_config_path(config: Config, path: impl Into<PathBuf>) -> Self {
        Self::build(config, Some(path.into()))
    }

    fn build(config: Config, config_path: Option<PathBuf>) -> Self {
        let mut states = HashMap::new();
        for rule in &config.rules {
            states.insert(rule.id.clone(), RuleState::new());
        }
        Self {
            config: Arc::new(RwLock::new(config)),
            states: Arc::new(RwLock::new(states)),
            config_path: RwLock::new(config_path),
            persist_lock: Mutex::new(()),
        }
    }

    /// 读取当前配置的快照。
    pub fn config(&self) -> Config {
        self.config.read().clone()
    }

    /// 查询单条规则的运行状态（不存在时返回 None）。
    pub fn rule_status(&self, id: &str) -> Option<RuleStatus> {
        self.states.read().get(id).map(|s| s.status.clone())
    }

    /// 查询所有规则的状态快照：`(id, status, stats_snapshot)`。
    pub fn all_statuses(&self) -> Vec<(String, RuleStatus, crate::status::StatsSnapshot)> {
        self.states
            .read()
            .iter()
            .map(|(id, s)| (id.clone(), s.status.clone(), s.stats.snapshot()))
            .collect()
    }

    /// 启动单条规则。若规则已在运行则返回 `Error::AlreadyRunning`。
    pub async fn start_rule(&self, id: &str) -> Result<()> {
        let rule = {
            let cfg = self.config.read();
            cfg.rule(id)
                .ok_or_else(|| Error::UnknownRule(id.to_owned()))?
                .clone()
        };

        // 检查并设置为 Starting，记下这次启动的代数。
        let generation = {
            let mut states = self.states.write();
            let state = states
                .entry(id.to_owned())
                .or_insert_with(RuleState::new);
            if state.status == RuleStatus::Running || state.status == RuleStatus::Starting {
                return Err(Error::AlreadyRunning { id: id.to_owned() });
            }
            state.status = RuleStatus::Starting;
            state.generation += 1;
            state.generation
        };

        // 先绑定端口。端口被占用、证书读不出来这类错误必须当场返回给调用方，
        // 否则 CLI 会报"已启动"，GUI 会亮绿灯，真相只留在日志里。
        let bound = match listener::bind(&rule).await {
            Ok(b) => b,
            Err(e) => {
                let mut states = self.states.write();
                if let Some(state) = states.get_mut(id).filter(|s| s.generation == generation) {
                    state.status = RuleStatus::Failed(e.to_string());
                }
                return Err(e);
            }
        };

        // 状态先写成 Running 再 spawn：反过来的话，任务可能抢先结束并写入
        // Stopped/Failed，随后被这里的赋值覆盖回 Running。
        let cancel = CancellationToken::new();
        let stats = {
            let mut states = self.states.write();
            match states.get_mut(id).filter(|s| s.generation == generation) {
                Some(state) => {
                    state.status = RuleStatus::Running;
                    state.cancel = Some(cancel.clone());
                    Arc::clone(&state.stats)
                }
                // 绑定端口期间规则被叫停或删掉了：这次启动作废，
                // 端口随 `bound` 一起释放。
                None => return Ok(()),
            }
        };

        let states = Arc::clone(&self.states);
        let rule_id = id.to_owned();
        let handle = tokio::spawn(async move {
            let result = listener::serve(bound, rule, stats, cancel).await;
            if let Err(e) = &result {
                tracing::error!(
                    rule_id = %rule_id,
                    "{}",
                    crate::i18n::tr_args("log.rule_failed", &[("reason", &e.localized())])
                );
            }

            let mut states = states.write();
            if let Some(state) = states
                .get_mut(&rule_id)
                .filter(|s| s.generation == generation)
            {
                state.status = match result {
                    Ok(()) => RuleStatus::Stopped,
                    Err(e) => RuleStatus::Failed(e.to_string()),
                };
                state.cancel = None;
                state.handle = None;
            }
        });

        // 任务若已经跑完（启动后立刻被取消），状态已不是 Running，句柄无需保留。
        {
            let mut states = self.states.write();
            if let Some(state) = states.get_mut(id) {
                if state.generation == generation && state.status == RuleStatus::Running {
                    state.handle = Some(handle);
                }
            }
        }

        Ok(())
    }

    /// 停止单条规则，等待其任务退出（最多 5 秒）。
    ///
    /// 正在绑定端口的启动也一并作废：它绑完会发现代数变了，自行放弃。
    pub async fn stop_rule(&self, id: &str) -> Result<()> {
        let (cancel, handle) = {
            let mut states = self.states.write();
            let state = states
                .get_mut(id)
                .ok_or_else(|| Error::UnknownRule(id.to_owned()))?;
            if matches!(state.status, RuleStatus::Running | RuleStatus::Starting) {
                // 状态当场改好，而不是等监听任务退出时再回写：下面最多只等 5 秒，
                // 等不到的话任务会在稍后某个时刻收尾，那时规则也许已被重新启动了。
                state.generation += 1;
                state.status = RuleStatus::Stopped;
            }
            (state.cancel.take(), state.handle.take())
        };

        if let Some(token) = cancel {
            token.cancel();
        }
        if let Some(h) = handle {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), h).await;
        }
        Ok(())
    }

    /// 启动配置中全部已启用的规则。
    pub async fn start_all(&self) -> Vec<(String, Error)> {
        let ids: Vec<String> = self
            .config
            .read()
            .enabled_rules()
            .iter()
            .map(|r| r.id.clone())
            .collect();

        let mut errors = Vec::new();
        for id in ids {
            if let Err(e) = self.start_rule(&id).await {
                errors.push((id, e));
            }
        }
        errors
    }

    /// 停止全部规则。
    pub async fn stop_all(&self) {
        let ids: Vec<String> = self.states.read().keys().cloned().collect();
        for id in ids {
            let _ = self.stop_rule(&id).await;
        }
    }

    /// 热重载配置：停止已删除的规则，新增规则的状态条目，已有规则继续运行。
    pub async fn reload(&self, new_config: Config) {
        let new_ids: HashSet<&str> = new_config.rules.iter().map(|r| r.id.as_str()).collect();
        let removed: Vec<String> = self
            .config
            .read()
            .rules
            .iter()
            .filter(|r| !new_ids.contains(r.id.as_str()))
            .map(|r| r.id.clone())
            .collect();

        for id in &removed {
            let _ = self.stop_rule(id).await;
        }
        self.replace_config(new_config);

        tracing::info!("{}", crate::i18n::tr("log.reload"));
    }

    /// 换用另一个配置文件：`config` 是 `path` 里的内容，此后的改动都写到 `path`。
    ///
    /// 新配置里定义一字未改的规则照常运行——只是挪了个位置的配置不必打断正在跑的
    /// 规则；其余在跑的（被删掉的、同一个 ID 定义已经不同的）先停掉：同一个 ID 在
    /// 两个文件里可能是完全不同的两条规则，不能拿旧的定义接着跑。新文件的应用设置
    /// （语言、日志）即时生效。不写盘：`path` 里本来就是这份内容。
    pub async fn switch_config(&self, config: Config, path: PathBuf) {
        let changed: Vec<String> = self
            .config
            .read()
            .rules
            .iter()
            .filter(|old| config.rule(&old.id) != Some(*old))
            .map(|r| r.id.clone())
            .collect();

        for id in &changed {
            let _ = self.stop_rule(id).await;
        }
        apply_app_settings(&config.app);
        {
            // 与 persist 互斥：别让一次正在进行的保存把旧配置写进新文件。
            let _persist = self.persist_lock.lock();
            *self.config_path.write() = Some(path);
            self.replace_config(config);
        }
    }

    /// 换上新配置，状态表随之对齐：摘掉已不存在的规则，给新来的补条目。
    /// 调用方负责先停掉要摘掉的规则。
    fn replace_config(&self, config: Config) {
        let mut states = self.states.write();
        states.retain(|id, _| config.rule(id).is_some());
        for rule in &config.rules {
            states.entry(rule.id.clone()).or_insert_with(RuleState::new);
        }
        *self.config.write() = config;
    }

    /// 配置文件路径。`None` 表示这份配置不来自磁盘，改动不会落盘。
    pub fn config_path(&self) -> Option<PathBuf> {
        self.config_path.read().clone()
    }

    /// 正在运行的规则数，供托盘图标状态与悬停提示使用。
    pub fn running_count(&self) -> usize {
        self.states
            .read()
            .values()
            .filter(|s| s.status == RuleStatus::Running)
            .count()
    }

    /// 把当前配置写回磁盘。未关联配置文件时静默成功。
    ///
    /// 快照在写盘锁里取：多次保存排着队进行，最后落盘的一定是最新的配置。
    pub fn persist(&self) -> Result<()> {
        let _guard = self.persist_lock.lock();
        let Some(path) = self.config_path() else {
            return Ok(());
        };
        let cfg = self.config.read().clone();
        cfg.save(&path)
    }

    /// 新增规则，或按 ID 覆盖已有规则，随后落盘。
    ///
    /// 校验在配置副本上做：`Config::validate` 检查的是重复 ID 与端口冲突这类
    /// 跨规则约束，必须连同其余规则一起看，单独校验这一条查不出来。副本通过
    /// 校验后才写回内存，非法规则不会留下痕迹。
    ///
    /// 覆盖正在运行的规则时先停掉旧实例，否则改了监听端口的新实例会撞在
    /// 旧实例上；改完若原先在跑且仍处启用态，自动重新启动。
    pub async fn upsert_rule(&self, rule: Rule) -> Result<()> {
        let id = rule.id.clone();
        let enabled = rule.enabled;

        let mut draft = self.config.read().clone();
        draft.put_rule(rule.clone());
        draft.validate()?;

        let was_running = self.rule_status(&id) == Some(RuleStatus::Running);
        if was_running {
            let _ = self.stop_rule(&id).await;
        }

        // 等旧实例退出的这段时间里，别处可能改过配置（比如切换了另一条规则的开关）。
        // 在最新的配置上再套一次这条改动，而不是把开头那份副本整个写回去——
        // 那会把别人的改动悄悄覆盖掉。
        {
            let mut config = self.config.write();
            let mut latest = config.clone();
            latest.put_rule(rule);
            latest.validate()?;
            *config = latest;
        }
        self.states
            .write()
            .entry(id.clone())
            .or_insert_with(RuleState::new);
        self.persist()?;

        if was_running && enabled {
            self.start_rule(&id).await?;
        }
        Ok(())
    }

    /// 删除规则：先停止，再从配置与状态表摘除，随后落盘。
    pub async fn remove_rule(&self, id: &str) -> Result<()> {
        if self.config.read().rule(id).is_none() {
            return Err(Error::UnknownRule(id.to_owned()));
        }
        let _ = self.stop_rule(id).await;
        self.config.write().rules.retain(|r| r.id != id);
        self.states.write().remove(id);
        self.persist()
    }

    /// 切换规则的启用开关：置为启用则启动，置为停用则停止，两者都落盘。
    pub async fn set_rule_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        {
            let mut cfg = self.config.write();
            let rule = cfg
                .rule_mut(id)
                .ok_or_else(|| Error::UnknownRule(id.to_owned()))?;
            rule.enabled = enabled;
        }
        self.persist()?;

        if enabled {
            // 已在运行时 start_rule 会返回 AlreadyRunning，这不算切换失败。
            match self.start_rule(id).await {
                Ok(()) | Err(Error::AlreadyRunning { .. }) => Ok(()),
                Err(e) => Err(e),
            }
        } else {
            self.stop_rule(id).await
        }
    }

    /// 替换全局应用设置，把语言、日志开关与级别即时应用到本进程，随后落盘。
    ///
    /// 副作用先于写盘生效：即便写盘失败，界面语言与日志行为也已跟上用户的选择，
    /// 调用方拿到的错误只关乎持久化。
    pub fn update_app_config(&self, app: AppConfig) -> Result<()> {
        apply_app_settings(&app);
        self.config.write().app = app;
        self.persist()
    }
}

/// 把应用设置里即时生效的几项（界面语言、日志开关与级别）应用到本进程。
fn apply_app_settings(app: &AppConfig) {
    crate::i18n::set_language(app.effective_language());
    crate::log::set_logging_enabled(app.logging_enabled);
    crate::log::set_level(app.log_level);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthConfig, Mode, Upstream};

    /// 用 [`Engine::new`] 构造的引擎 `config_path` 为 `None`，`persist()` 静默成功，
    /// 所以下面这些用例不碰磁盘，也不需要临时目录。
    fn engine() -> Engine {
        Engine::new(Config::default_empty())
    }

    fn rule(id: &str, port: u16) -> Rule {
        Rule {
            id: id.to_owned(),
            name: String::new(),
            enabled: true,
            mode: Mode::Reverse,
            listen: format!("127.0.0.1:{port}"),
            target: Some("https://api.anthropic.com".to_owned()),
            upstream: Upstream::default(),
            tls: None,
            auth: AuthConfig::default(),
        }
    }

    // ── upsert_rule ────────────────────────────────────────

    #[tokio::test]
    async fn upsert_adds_rule_and_state_entry() {
        let e = engine();
        e.upsert_rule(rule("a", 9300)).await.unwrap();

        assert_eq!(e.config().rules.len(), 1);
        assert_eq!(e.rule_status("a"), Some(RuleStatus::Stopped));
    }

    #[tokio::test]
    async fn upsert_same_id_replaces_instead_of_appending() {
        let e = engine();
        e.upsert_rule(rule("a", 9301)).await.unwrap();

        let mut changed = rule("a", 9302);
        changed.name = "renamed".to_owned();
        e.upsert_rule(changed).await.unwrap();

        let cfg = e.config();
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].listen, "127.0.0.1:9302");
        assert_eq!(cfg.rules[0].name, "renamed");
    }

    /// 校验在配置副本上做，被拒的规则不能留下痕迹。
    #[tokio::test]
    async fn upsert_invalid_rule_leaves_config_untouched() {
        let e = engine();
        e.upsert_rule(rule("a", 9310)).await.unwrap();

        let mut bad = rule("b", 9311);
        bad.target = None; // 反向模式缺 target
        assert!(e.upsert_rule(bad).await.is_err());

        let cfg = e.config();
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].id, "a");
        assert_eq!(e.rule_status("b"), None);
    }

    /// 端口冲突是跨规则约束：只校验新来的这一条查不出来。
    #[tokio::test]
    async fn upsert_listen_conflict_rejected() {
        let e = engine();
        e.upsert_rule(rule("a", 9320)).await.unwrap();
        assert!(matches!(
            e.upsert_rule(rule("b", 9320)).await,
            Err(Error::ListenConflict { .. })
        ));
        assert_eq!(e.config().rules.len(), 1);
    }

    // ── remove_rule ────────────────────────────────────────

    #[tokio::test]
    async fn remove_drops_rule_and_state() {
        let e = engine();
        e.upsert_rule(rule("a", 9330)).await.unwrap();
        e.remove_rule("a").await.unwrap();

        assert!(e.config().rules.is_empty());
        assert_eq!(e.rule_status("a"), None);
    }

    #[tokio::test]
    async fn remove_unknown_rule_reports_unknown() {
        let e = engine();
        assert!(matches!(
            e.remove_rule("ghost").await,
            Err(Error::UnknownRule(id)) if id == "ghost"
        ));
    }

    // ── set_rule_enabled ───────────────────────────────────

    #[tokio::test]
    async fn disabling_rule_writes_flag() {
        let e = engine();
        e.upsert_rule(rule("a", 9340)).await.unwrap();
        e.set_rule_enabled("a", false).await.unwrap();

        assert!(!e.config().rules[0].enabled);
        assert_eq!(e.rule_status("a"), Some(RuleStatus::Stopped));
    }

    /// 启用会真的去绑端口，成功后状态转 Running；再启用一次不算失败
    /// （`AlreadyRunning` 被当作切换成功）。
    #[tokio::test]
    async fn enabling_rule_starts_and_is_idempotent() {
        let e = engine();
        let mut r = rule("a", 0); // 端口 0：由系统分配，不会撞上别的测试
        r.enabled = false;
        e.upsert_rule(r).await.unwrap();

        e.set_rule_enabled("a", true).await.unwrap();
        assert_eq!(e.rule_status("a"), Some(RuleStatus::Running));

        e.set_rule_enabled("a", true).await.unwrap();
        assert_eq!(e.rule_status("a"), Some(RuleStatus::Running));

        e.set_rule_enabled("a", false).await.unwrap();
        assert_eq!(e.running_count(), 0);
    }

    #[tokio::test]
    async fn set_enabled_unknown_rule_reports_unknown() {
        let e = engine();
        assert!(matches!(
            e.set_rule_enabled("ghost", true).await,
            Err(Error::UnknownRule(_))
        ));
    }

    // ── reload ─────────────────────────────────────────────

    /// 热重载：消失的规则连状态条目一起摘掉，新来的补上条目。
    #[tokio::test]
    async fn reload_drops_removed_and_adds_new_states() {
        let e = engine();
        e.upsert_rule(rule("a", 9350)).await.unwrap();

        let mut next = Config::default_empty();
        next.rules.push(rule("b", 9351));
        e.reload(next).await;

        assert_eq!(e.rule_status("a"), None, "已删除规则的状态条目应当摘掉");
        assert_eq!(e.rule_status("b"), Some(RuleStatus::Stopped));
        assert_eq!(e.config().rules[0].id, "b");
        assert_eq!(e.all_statuses().len(), 1);
    }

    // ── 启停 ───────────────────────────────────────────────

    /// 系统分配一个此刻空闲的端口。拿到后立即释放，留给规则去绑。
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("分配空闲端口")
            .port()
    }

    /// 停下之后立刻再启动：旧监听任务的收尾不能把新实例的状态改掉。
    #[tokio::test]
    async fn restart_right_after_stop() {
        let e = engine();
        e.upsert_rule(rule("a", free_port())).await.unwrap();
        for _ in 0..20 {
            e.start_rule("a").await.unwrap();
            assert_eq!(e.rule_status("a"), Some(RuleStatus::Running));
            e.stop_rule("a").await.unwrap();
            assert_eq!(e.rule_status("a"), Some(RuleStatus::Stopped));
        }
        assert_eq!(e.running_count(), 0);
    }

    // ── switch_config ──────────────────────────────────────

    /// 换配置文件：定义一字未改的规则照常运行，改过的停掉，没了的连状态一起摘掉，
    /// 此后的改动写进新文件。
    #[tokio::test]
    async fn switch_config_keeps_only_identical_rules_running() {
        let e = engine();
        let same = rule("same", free_port());
        let changed = rule("changed", free_port());
        let gone = rule("gone", free_port());
        for r in [&same, &changed, &gone] {
            e.upsert_rule(r.clone()).await.unwrap();
            e.start_rule(&r.id).await.unwrap();
        }

        let mut next = Config::default_empty();
        next.rules.push(same.clone());
        next.rules.push(Rule {
            name: "renamed".to_owned(),
            ..changed.clone()
        });
        next.rules.push(rule("new", 9360));

        let path = std::env::temp_dir().join(format!(
            "aoproxy_switch_{}_{}.toml",
            std::process::id(),
            free_port()
        ));
        e.switch_config(next, path.clone()).await;

        assert_eq!(e.rule_status("same"), Some(RuleStatus::Running));
        assert_eq!(e.rule_status("changed"), Some(RuleStatus::Stopped));
        assert_eq!(e.rule_status("gone"), None);
        assert_eq!(e.rule_status("new"), Some(RuleStatus::Stopped));
        assert_eq!(e.config_path(), Some(path.clone()));

        e.set_rule_enabled("new", false).await.unwrap();
        let saved = Config::load(&path).expect("改动应写进新文件");
        assert!(!saved.rule("new").unwrap().enabled);

        e.stop_all().await;
        let _ = std::fs::remove_file(&path);
    }

    // ── persist ────────────────────────────────────────────

    #[test]
    fn persist_without_config_path_is_a_no_op() {
        let e = engine();
        assert!(e.config_path().is_none());
        e.persist().expect("无关联文件时应静默成功");
    }
}

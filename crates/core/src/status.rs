//! 运行期流量统计。
//!
//! 每条规则持有一个 [`Stats`] 实例；所有计数器均为原子类型，可无锁地从多个
//! tokio 任务并发更新。[`StatsSnapshot`] 是某时刻的只读快照，适合 GUI 渲染。

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// 单条规则的运行期统计计数器。
#[derive(Debug, Default)]
pub struct Stats {
    /// 当前活跃连接数；正常情况 ≥ 0，调试时可能短暂为负。
    active: AtomicI64,
    /// 自启动以来累计建立的连接总数。
    total: AtomicU64,
    /// 上行字节数（客户端 → 目标）。
    bytes_up: AtomicU64,
    /// 下行字节数（目标 → 客户端）。
    bytes_down: AtomicU64,
    /// 累计连接级错误次数。
    errors: AtomicU64,
    /// 认证失败次数（Basic / SOCKS5 用户名密码 / 路径令牌均计入）。
    auth_failures: AtomicU64,
}

/// [`Stats`] 的只读快照，所有字段均为普通整数，可安全 `Clone` 与跨线程传递。
#[derive(Debug, Clone, Default)]
pub struct StatsSnapshot {
    pub active: i64,
    pub total: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub errors: u64,
    pub auth_failures: u64,
}

impl Stats {
    /// 构造全零统计器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 新连接建立时调用。
    pub fn conn_open(&self) {
        self.active.fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    /// 连接关闭时调用，传入本次连接的上下行字节数。
    ///
    /// 活跃连接数只在这里递减，因此每条连接至多调用一次；中途想记录流量请用
    /// [`Stats::add_bytes`]。
    pub fn conn_close(&self, bytes_up: u64, bytes_down: u64) {
        self.active.fetch_sub(1, Ordering::Relaxed);
        self.bytes_up.fetch_add(bytes_up, Ordering::Relaxed);
        self.bytes_down.fetch_add(bytes_down, Ordering::Relaxed);
    }

    /// 只累加流量，不改变活跃连接数。隧道关闭、请求完成时由处理器调用。
    pub fn add_bytes(&self, bytes_up: u64, bytes_down: u64) {
        self.bytes_up.fetch_add(bytes_up, Ordering::Relaxed);
        self.bytes_down.fetch_add(bytes_down, Ordering::Relaxed);
    }

    /// 记录一次连接级错误。
    pub fn error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一次认证失败。
    pub fn auth_fail(&self) {
        self.auth_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// 取当前快照（全部字段使用 Relaxed 读，适合展示，不保证强一致性）。
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            active: self.active.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            bytes_up: self.bytes_up.load(Ordering::Relaxed),
            bytes_down: self.bytes_down.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            auth_failures: self.auth_failures.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stats_all_zero() {
        let s = Stats::new();
        let snap = s.snapshot();
        assert_eq!(snap.active, 0);
        assert_eq!(snap.total, 0);
        assert_eq!(snap.bytes_up, 0);
        assert_eq!(snap.bytes_down, 0);
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.auth_failures, 0);
    }

    #[test]
    fn conn_open_increments_active_and_total() {
        let s = Stats::new();
        s.conn_open();
        let snap = s.snapshot();
        assert_eq!(snap.active, 1);
        assert_eq!(snap.total, 1);
        assert_eq!(snap.bytes_up, 0);
    }

    #[test]
    fn conn_close_decrements_active_and_adds_bytes() {
        let s = Stats::new();
        s.conn_open();
        s.conn_close(10, 20);
        let snap = s.snapshot();
        assert_eq!(snap.active, 0);
        assert_eq!(snap.total, 1);
        assert_eq!(snap.bytes_up, 10);
        assert_eq!(snap.bytes_down, 20);
    }

    #[test]
    fn add_bytes_does_not_touch_active() {
        let s = Stats::new();
        s.conn_open();
        s.add_bytes(5, 7);
        let snap = s.snapshot();
        // conn_open'd but not conn_close'd: active must still be 1
        assert_eq!(snap.active, 1);
        assert_eq!(snap.bytes_up, 5);
        assert_eq!(snap.bytes_down, 7);
    }

    #[test]
    fn auth_fail_increments_counter() {
        let s = Stats::new();
        s.auth_fail();
        s.auth_fail();
        assert_eq!(s.snapshot().auth_failures, 2);
    }

    #[test]
    fn error_increments_counter() {
        let s = Stats::new();
        s.error();
        assert_eq!(s.snapshot().errors, 1);
    }

    #[test]
    fn multiple_conns_snapshot_consistent() {
        let s = Stats::new();
        s.conn_open();
        s.conn_open();
        s.conn_open();
        s.conn_close(0, 0);
        let snap = s.snapshot();
        assert_eq!(snap.active, 2);
        assert_eq!(snap.total, 3);
    }

    #[test]
    fn bytes_accumulate_across_calls() {
        let s = Stats::new();
        s.conn_open();
        s.conn_close(100, 200);
        s.conn_open();
        s.conn_close(50, 75);
        let snap = s.snapshot();
        assert_eq!(snap.bytes_up, 150);
        assert_eq!(snap.bytes_down, 275);
        assert_eq!(snap.total, 2);
        assert_eq!(snap.active, 0);
    }
}

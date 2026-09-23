//! 单实例守卫。
//!
//! 第二次启动不再开新窗口，而是把已在运行的那个唤到前台然后自己退出。
//!
//! 做法是抢占一个回环端口：抢到即首实例，抢不到说明别人已经在跑，于是连上去
//! 打个招呼再退出。首实例的 accept 循环每收到一次连接就往 channel 送一下，
//! [`AppBridge`] 那头接住后发 `showRequested`。
//!
//! 为什么不用文件锁：Windows 上进程被强杀后锁文件会留下，下次启动得先判断
//! 里面的 PID 是否还活着，而读 PID、查进程、处理 PID 复用是一串平台分支。
//! 端口由内核在进程结束时无条件回收，没有残留状态要清理。
//!
//! 端口写死在回环地址上，所以不经过防火墙提示（那只针对 `0.0.0.0` 的监听），
//! 也不会被外部访问到。
//!
//! [`AppBridge`]: crate::bridge

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// 守卫端口。选在动态端口区间的高位，避开常见服务。
/// 改动它等于换一把锁：新旧版本同时装在一台机器上时会各自成为首实例。
const PORT: u16 = 49517;

/// 连接首实例时的超时。对方就在本机，正常情况下是微秒级；
/// 给上限是防止端口被别的程序占着却不 accept，把启动卡死。
const POKE_TIMEOUT: Duration = Duration::from_millis(500);

/// 抢锁结果。
pub enum Acquire {
    /// 本进程是首实例，附带「有人想唤起窗口」的通知接收端。
    Primary(Receiver<()>),
    /// 已有实例在跑，已通知其唤起窗口，本进程应当退出。
    Secondary,
}

/// 抢占单实例锁。
pub fn acquire() -> Acquire {
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, PORT);
    match TcpListener::bind(addr) {
        Ok(listener) => {
            let (tx, rx) = channel();
            spawn_accept_loop(listener, tx);
            Acquire::Primary(rx)
        }
        Err(_) => {
            // 绑不上就认为已有实例。也可能是端口被别的程序占了——那种情况下
            // poke 会失败，此时宁可放行也不要让程序打不开。
            if poke(addr) {
                Acquire::Secondary
            } else {
                let (_tx, rx) = channel();
                Acquire::Primary(rx)
            }
        }
    }
}

/// 首实例的 accept 循环。每来一个连接就当作一次「唤起窗口」请求。
///
/// 连接内容一概不读：能连上就说明是同一把锁的持有者在打招呼，
/// 读取内容只会多一条需要防御的输入路径。
fn spawn_accept_loop(listener: TcpListener, tx: Sender<()>) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(_) => {
                    // 送不出去说明接收端没了（AppBridge 已销毁），循环该结束。
                    if tx.send(()).is_err() {
                        break;
                    }
                }
                // 单次 accept 失败（如 fd 用尽）不该让守卫罢工，继续等下一个。
                Err(_) => continue,
            }
        }
    });
}

/// 通知首实例唤起窗口。返回是否确实联系上了。
fn poke(addr: SocketAddrV4) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr.into(), POKE_TIMEOUT) else {
        return false;
    };
    // 写一个字节让对端的 accept 确定拿到连接。写失败也算联系上了：
    // 连接已经建立，对端的 accept 早就返回了。
    let _ = stream.set_write_timeout(Some(POKE_TIMEOUT));
    let _ = stream.write_all(b"\n");
    true
}

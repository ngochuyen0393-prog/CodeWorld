//! 外桥出站管理（D-001b）：保险库 BridgeVault + 出站信箱 BridgeOutbox + 手搓 HTTP
//!
//! 世界=邮差：桥把沙箱的信（bridge_send 排队）经 HTTP POST 发往玩家自备的外部
//! 服务器，响应 body 原样投回沙箱（bridge_on_reply）。世界不看内容不代存不重试
//!（design-bridge.md 定案；机制层实现归世界，内容归玩家）。
//!
//! MVP 客户端边界：手搓 HTTP/1.1 短连接，仅支持本机回环 http（127.0.0.1/localhost）
//!——禁新增依赖（无 reqwest/ureq），https 真客户端挂起（design-bridge §2.8）。
//! 客户端局限：不支持 Transfer-Encoding: chunked（MVP，回信需 Content-Length/短连接 EOF）。
//!
//! 线程模型：设计值 8 出站线程，MVP 单工作线程先行——每秒 64 封理论上限远超
//! 单线程 5s 超时窗的消化能力风险存在，实测积压信号出现再扩线程（挂起项）。

use anyhow::Result;
use codeworld_core::entity::EntityId;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, RwLock};
use std::time::Duration;

/// 回信读取保险丝（worker 侧总响应上限：头+体，防坏服务器灌爆内存）。
/// 与护栏 payload 上限（config）分工：worker 防灌爆（读上限），host 投递前
/// 再按 payload_max 截断（防超规）。
const REPLY_READ_CAP: usize = 1024 * 1024;

/// 保险库条目（解析校验后的形态）
///
/// 世界进程持有，沙箱永远拿不到登记内容（铁律①：沙箱里只有 port 别名）。
#[derive(Debug, Clone)]
pub(crate) struct BridgeEndpoint {
    /// 端点别名（>0，重复登记后者胜）
    pub port: i32,
    /// URL 原文（日志用）
    pub url: String,
    /// 解析后的主机名（仅 127.0.0.1 / localhost——MVP 客户端只做本机 http）
    pub host: String,
    /// 解析后的连接端口
    pub port_num: u16,
    /// 解析后的请求路径（缺省 "/"）
    pub path: String,
    /// 鉴权头集（世界代签：原样贴上出站请求——凭证不落沙箱内存）
    pub headers: HashMap<String, String>,
}

/// 凭证保险库（design-bridge 第三节：账号层外部端点登记表）
///
/// bridges.json（世界目录，与 codeworld.db 同级）加载 + 校验 + mtime 热重载。
/// 校验规则：port>0（重复后者胜+warn）；http URL 的 host 必须本机回环（否则
/// warn 拒收该条——MVP 客户端只做本机 http）；https URL warn 停用。
#[derive(Debug, Default)]
pub(crate) struct BridgeVault {
    /// 登记表文件路径
    path: PathBuf,
    /// 校验通过的端点（登记顺序，重复 port 已去重）
    endpoints: Vec<BridgeEndpoint>,
    /// 上次加载时的 mtime（None=文件不存在或瞬时读取失败——后者保留重试语义；
    /// 热重载判定依据）
    mtime: Option<std::time::SystemTime>,
}

/// bridges.json 的 serde 中间形态（宽松：headers/note 可缺省）
#[derive(serde::Deserialize)]
struct BridgeEndpointRaw {
    port: i32,
    url: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    #[allow(dead_code)] // note 仅人读，世界不解释（保留字段完整性）
    note: String,
}

impl BridgeVault {
    /// 加载保险库（文件不存在=空表首启；JSON 坏=warn 空表不崩世界）
    pub fn load(path: &Path) -> Self {
        let mut mtime = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok());
        let endpoints = match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                // P2-B：瞬时读取失败（杀软/备份锁等非 NotFound 错误）不缓存 mtime
                //（置 None）——保留"下拍重试"语义，防"空表+新 mtime"把桥钉死到
                // 下次手改文件
                tracing::warn!(error = %e, path = %path.display(), "[bridge] 保险库读取失败，按空表运行");
                mtime = None;
                Vec::new()
            }
            Ok(text) => match serde_json::from_str::<Vec<BridgeEndpointRaw>>(&text) {
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        "[bridge] 保险库解析失败，按空表运行（坏配置不崩世界）"
                    );
                    Vec::new()
                }
                Ok(list) => Self::validate(list),
            },
        };
        tracing::info!(
            port_count = endpoints.len(),
            file = %path.display(),
            "[bridge] 保险库加载"
        );
        Self {
            path: path.to_path_buf(),
            endpoints,
            mtime,
        }
    }

    /// 校验端点列表（详见 struct 文档的规则）
    fn validate(list: Vec<BridgeEndpointRaw>) -> Vec<BridgeEndpoint> {
        let mut out: Vec<BridgeEndpoint> = Vec::new();
        for raw in list {
            if raw.port <= 0 {
                tracing::warn!(port = raw.port, url = %raw.url, "[bridge] 端点 port 非法（须>0），拒收");
                continue;
            }
            // scheme 校验（大小写不敏感前缀匹配）
            let lower = raw.url.to_ascii_lowercase();
            if let Some(rest) = lower.strip_prefix("https://") {
                let _ = rest;
                tracing::warn!(
                    port = raw.port,
                    url = %raw.url,
                    "[bridge] https 端点本 MVP 不支持，停用（挂起项见 design-bridge §2.8）"
                );
                continue;
            }
            if !lower.starts_with("http://") {
                tracing::warn!(port = raw.port, url = %raw.url, "[bridge] 非_http scheme 不支持，拒收");
                continue;
            }
            // host 校验：MVP 客户端只做本机回环（localhost 大小写不敏感——
            // http://LocalHost:8080 与 http://localhost 等价，P2-D）
            let Some((host, port_num, path)) = parse_http_url(&raw.url) else {
                tracing::warn!(port = raw.port, url = %raw.url, "[bridge] URL 解析失败，拒收");
                continue;
            };
            if host != "127.0.0.1" && !host.eq_ignore_ascii_case("localhost") {
                tracing::warn!(
                    port = raw.port,
                    url = %raw.url,
                    "[bridge] http 端点非本机回环（仅 127.0.0.1/localhost），拒收"
                );
                continue;
            }
            // 重复后者胜：同 port 旧条目移除再追加
            if let Some(pos) = out.iter().position(|e| e.port == raw.port) {
                tracing::warn!(
                    port = raw.port,
                    old = %out[pos].url,
                    new = %raw.url,
                    "[bridge] 端点重复登记，后者胜"
                );
                out.remove(pos);
            }
            out.push(BridgeEndpoint {
                port: raw.port,
                url: raw.url,
                host,
                port_num,
                path,
                headers: raw.headers,
            });
        }
        out
    }

    /// mtime 热重载检查（每拍 stat 一次）：文件变更则重载。返回是否发生了重载。
    pub fn reload_if_changed(&mut self) -> bool {
        let mtime = std::fs::metadata(&self.path)
            .ok()
            .and_then(|m| m.modified().ok());
        if mtime == self.mtime {
            return false;
        }
        let reloaded = Self::load(&self.path);
        let count = reloaded.endpoints.len();
        self.path = reloaded.path;
        self.endpoints = reloaded.endpoints;
        self.mtime = reloaded.mtime;
        tracing::info!(port_count = count, "[bridge] 保险库热重载");
        true
    }

    /// 已登记端口列表（升序；拍首快照填充数据源）
    pub fn ports(&self) -> Vec<i32> {
        let mut ports: Vec<i32> = self.endpoints.iter().map(|e| e.port).collect();
        ports.sort_unstable();
        ports
    }

    /// 解析 port -> 端点（collect 出站时解析 URL；热重载后消失 -> None 作废）
    pub fn resolve(&self, port: i32) -> Option<&BridgeEndpoint> {
        self.endpoints.iter().find(|e| e.port == port)
    }
}

/// 解析 http://host[:port][/path]（手搓最小版：仅本机 http 需要）
///
/// 返回 (host, port, path)；端口缺省 80，路径缺省 "/"；端口非数字 -> None。
fn parse_http_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (authority.to_string(), 80),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port, path))
}

/// host→SocketAddr 解析（P1-A：connect 必须走 connect_timeout——`TcpStream::connect`
/// 不受 set_read/write_timeout 约束，对静默丢 SYN 的端口 connect 走 OS 默认
///（Windows ~21s），单工作线程队头阻塞远超 5s 护栏承诺）
///
/// 保险库已限定 host ∈ {127.0.0.1, localhost}（validate 侧 localhost 大小写
/// 不敏感）：localhost 各形态直接映射 127.0.0.1（不走 DNS 解析，杜绝悬挂）；
/// 其他 host 返回 None（护栏双重防御，调用侧归超时路径不投递）。
fn resolve_host_addr(host: &str, port_num: u16) -> Option<std::net::SocketAddr> {
    if host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost") {
        Some(std::net::SocketAddr::from(([127, 0, 0, 1], port_num)))
    } else {
        None
    }
}

/// 出站信件（collect 出站时组装，worker 消费）
#[derive(Debug)]
pub(crate) struct Letter {
    /// 发起实体
    pub entity: EntityId,
    /// 端点别名（信封字段）
    pub port: i32,
    /// 单实体单调序号（信封字段）
    pub seq: u64,
    /// 信体（世界不解释）
    pub payload: Vec<u8>,
    /// 已解析的端点（热重载作废在 collect 侧判，Letter 只带有效端点）
    pub endpoint: BridgeEndpoint,
}

/// 回信事件（worker -> 主循环，阶段二 a-1 消费）
#[derive(Debug)]
pub(crate) enum ReplyEvent {
    /// 外部服务器响应（body 原样投递 bridge_on_reply）
    Reply {
        entity: EntityId,
        port: i32,
        seq: u64,
        body: Vec<u8>,
    },
    /// 出站超时/连接失败/非 2xx——不投递，代码自管（世界不重试不代放弃）
    Timeout { entity: EntityId, seq: u64 },
}

/// 出站信箱：保险库 + 双通道（出站/回信）+ 可选工作线程
///
/// - `with_worker`（生产）：1 个 std 工作线程消化出站信件（设计值 8，MVP 单线程
///   先行，积压信号出现再扩），回信/超时入回信通道。
/// - `without_worker`（测试）：不起线程，测试直接从 outbound_rx 收出站信件。
pub(crate) struct BridgeOutbox {
    /// 保险库（Arc<RwLock>：热重载写、拍首快照/collect 解析读）
    vault: Arc<RwLock<BridgeVault>>,
    /// 出站通道发送端
    outbound_tx: mpsc::Sender<Letter>,
    /// 出站通道接收端（worker 构造时移交线程；无 worker 构造留给测试收信）
    #[cfg_attr(not(test), allow(dead_code))] // worker 模式 rx 已移交线程，字段仅测试路径在读
    outbound_rx: Option<mpsc::Receiver<Letter>>,
    /// 回信通道接收端（阶段二 a-1 drain）
    reply_rx: mpsc::Receiver<ReplyEvent>,
    /// 回信通道测试发送端（仅测试构造保留——向 reply_rx 直注事件）
    #[cfg(test)]
    reply_tx_test: Option<mpsc::Sender<ReplyEvent>>,
}

impl BridgeOutbox {
    /// 生产构造：加载保险库 + 起 1 个工作线程
    pub fn new(path: &Path, timeout_s: u64) -> Result<Self> {
        Self::build(path, timeout_s, true)
    }

    /// 测试构造：加载保险库但不起线程（测试直接收出站信件）
    #[cfg(test)]
    pub fn without_worker(path: &Path, timeout_s: u64) -> Result<Self> {
        Self::build(path, timeout_s, false)
    }

    fn build(path: &Path, timeout_s: u64, with_worker: bool) -> Result<Self> {
        let vault = Arc::new(RwLock::new(BridgeVault::load(path)));
        let (outbound_tx, outbound_rx) = mpsc::channel::<Letter>();
        let (reply_tx, reply_rx) = mpsc::channel::<ReplyEvent>();
        if with_worker {
            let timeout = Duration::from_secs(timeout_s);
            std::thread::Builder::new()
                .name("bridge-outbox".to_string())
                .spawn(move || worker_loop(outbound_rx, reply_tx, timeout))?;
            tracing::info!(
                timeout_s,
                threads = 1,
                "[bridge] 出站工作线程已启动（设计值 8，MVP 单线程先行）"
            );
            Ok(Self {
                vault,
                outbound_tx,
                outbound_rx: None,
                reply_rx,
                #[cfg(test)]
                reply_tx_test: None,
            })
        } else {
            Ok(Self {
                vault,
                outbound_tx,
                outbound_rx: Some(outbound_rx),
                reply_rx,
                #[cfg(test)]
                reply_tx_test: Some(reply_tx.clone()),
            })
        }
    }

    /// 保险库只读访问（拍首 ports 快照 / collect 解析 URL）
    pub fn vault(&self) -> &RwLock<BridgeVault> {
        &self.vault
    }

    /// 推一封信进出站通道（通道断=世界在关闭中，静默丢弃）
    pub fn send_letter(&self, letter: Letter) {
        let _ = self.outbound_tx.send(letter);
    }

    /// 非阻塞收一封出站信（无 worker 测试模式；worker 模式恒 None——rx 已移交）
    #[cfg(test)]
    pub fn try_recv_letter(&mut self) -> Option<Letter> {
        self.outbound_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
    }

    /// 非阻塞收一封回信事件（阶段二 a-1 drain 用）
    pub fn try_recv_reply(&mut self) -> Option<ReplyEvent> {
        self.reply_rx.try_recv().ok()
    }

    /// 测试注入口：直接向回信通道塞事件（绕过 worker/网络）
    #[cfg(test)]
    pub fn push_reply_for_test(&mut self, event: ReplyEvent) {
        let _ = self.reply_tx_for_test().send(event);
    }

    /// 测试注入口的后备发送端（reply_tx 在 worker 构造时移交线程；测试构造
    /// 场景 reply 通道只进不出——需要克隆发送端留存）
    #[cfg(test)]
    fn reply_tx_for_test(&self) -> &mpsc::Sender<ReplyEvent> {
        // reply_tx 不在 struct 里——测试构造路径也不移交。此处借 outbound_tx
        // 不可行，改为在 struct 保留一个测试用克隆发送端
        self.reply_tx_test.as_ref().expect("测试构造必须带 reply_tx 克隆")
    }
}

/// 工作线程主循环：阻塞收信 -> 手搓 HTTP -> 回信/超时入通道
///
/// 线程退出：出站通道全部发送端 drop（世界关闭）时 recv 返回 Err 自然退出。
fn worker_loop(rx: mpsc::Receiver<Letter>, reply_tx: mpsc::Sender<ReplyEvent>, timeout: Duration) {
    // recv 返回 Err（全部发送端 drop=世界关闭）即迭代结束，线程自然退出
    for letter in rx {
        // P2-C：每信一包 panic 护栏——worker 线程 panic 不能静默死掉（线程死了
        // 出站队列无人消化=整条桥哑掉且无日志可查）。panic 信归超时路径（信的
        // 生命周期必须终结，否则 inflight 计数泄漏堵死该沙箱），warn 留痕线程存活
        let _ = catch_letter_panic(
            || handle_letter(&letter, &reply_tx, timeout),
            |payload| {
                tracing::warn!(
                    entity = %letter.entity.0,
                    seq = letter.seq,
                    panic = ?payload,
                    "[bridge] 出站处理 panic（已捕获，工作线程存活；信归超时路径）"
                );
                let _ = reply_tx.send(ReplyEvent::Timeout {
                    entity: letter.entity,
                    seq: letter.seq,
                });
            },
        );
    }
}

/// 单信处理（worker_loop 的工作单元，拆出便于 panic 护栏包裹）
fn handle_letter(letter: &Letter, reply_tx: &mpsc::Sender<ReplyEvent>, timeout: Duration) {
    // 出站信封（design-bridge 2.1）：世界生成，entity/port/seq 供玩家对账，
    // payload 原样内嵌（内容永不入日志，也不进世界侧任何展示）
    let body = envelope_json(
        letter.entity.0.as_u128(),
        letter.port,
        letter.seq,
        &letter.payload,
    );
    match http_post(&letter.endpoint, &body, timeout) {
        Ok(body) => {
            let _ = reply_tx.send(ReplyEvent::Reply {
                entity: letter.entity,
                port: letter.port,
                seq: letter.seq,
                body,
            });
        }
        Err(e) => {
            tracing::debug!(
                entity = %letter.entity.0,
                seq = letter.seq,
                error = %e,
                "[bridge] 出站失败（归超时路径）"
            );
            let _ = reply_tx.send(ReplyEvent::Timeout {
                entity: letter.entity,
                seq: letter.seq,
            });
        }
    }
}

/// 单信 panic 护栏（P2-C）：工作单元 panic 被捕获转交 on_panic（不静默），
/// 调用方存活返回 None；正常路径原样返回 Some(结果)
fn catch_letter_panic<R>(
    work: impl FnOnce() -> R + std::panic::UnwindSafe,
    on_panic: impl FnOnce(Box<dyn std::any::Any + Send>),
) -> Option<R> {
    match std::panic::catch_unwind(work) {
        Ok(result) => Some(result),
        Err(payload) => {
            on_panic(payload);
            None
        }
    }
}

/// 手搓 HTTP/1.1 POST（一次性短连接，Connection: close）
///
/// 写请求（含代签头）-> 读响应到 EOF（或保险丝上限）-> 解析 status+body。
/// 非 2xx / 响应畸形 -> Err（归超时路径不投递）。
fn http_post(endpoint: &BridgeEndpoint, body: &[u8], timeout: Duration) -> std::io::Result<Vec<u8>> {
    // P1-A：connect 也吃超时——host 预解析成 SocketAddr 后走 connect_timeout
    //（裸 connect() 不受 set_*_timeout 约束，静默丢 SYN 时队头阻塞 OS 级超时窗）
    let addr = resolve_host_addr(&endpoint.host, endpoint.port_num).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("非本机回环 host：{}", endpoint.host),
        )
    })?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    // 请求头：世界代签（登记的鉴权头原样贴上——凭证不落沙箱内存）
    let mut req = format!(
        "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        endpoint.path,
        endpoint.host,
        endpoint.port_num,
        body.len()
    );
    for (k, v) in &endpoint.headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    // 读响应到 EOF（保险丝：总响应超 REPLY_READ_CAP 即停读，防坏服务器灌爆）
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > REPLY_READ_CAP {
                    break;
                }
            }
            Err(e) => return Err(e),
        }
    }
    parse_http_response(&raw)
}

/// 解析 HTTP 响应：取 status line 判 2xx，取 header/body 分隔后的 body
fn parse_http_response(raw: &[u8]) -> std::io::Result<Vec<u8>> {
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "响应无 header/body 分隔"))?;
    let status_line = std::str::from_utf8(&raw[..sep])
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "status line 非 UTF-8"))?
        .lines()
        .next()
        .unwrap_or("");
    // "HTTP/1.1 200 OK" -> 取第二段数字判 2xx
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "status line 畸形")
        })?;
    if !(200..300).contains(&code) {
        return Err(std::io::Error::other(format!("非 2xx 响应：{code}")));
    }
    Ok(raw[sep + 4..].to_vec())
}

/// 组装出站信封（design-bridge 2.1：世界发出去的 HTTP body 格式）
///
/// 世界不解释 payload；entity/port/seq 供玩家对账（信封字段玻璃不透明——
/// 玩家服务器能知道"哪个实体在问"，世界侧不泄露对手情报）。
/// entity 以十进制字符串承载（Ruling：serde_json 默认 Number 装不下 u128，
/// 实测 json! 数字形态 panic "number out of range"；且 u64 以上整数在 JS 端
/// Number 本就失精——字符串是跨语言对账最稳形态，玩家侧 parse 回 u128）。
pub(crate) fn envelope_json(entity: u128, port: i32, seq: u64, payload: &[u8]) -> Vec<u8> {
    let bytes: Vec<serde_json::Value> = payload.iter().map(|b| serde_json::json!(b)).collect();
    let envelope = serde_json::json!({
        "world": "codeworld",
        "entity": entity.to_string(),
        "port": port,
        "seq": seq,
        "payload": bytes,
    });
    serde_json::to_vec(&envelope).expect("信封序列化不可失败")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试固定 EntityId
    fn id(u: u128) -> EntityId {
        EntityId(uuid::Uuid::from_u128(u))
    }

    /// 写临时保险库文件（返回路径；测试结束由临时目录自清理语义托管——
    /// 项目内无 tempfile 依赖，直接用系统临时目录+唯一文件名，测后删除）
    fn write_vault(text: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("cw-bridge-test-{}.json", std::process::id()));
        // 并行测试防踩：按调用序加个随机尾巴（uuid 快速来源）
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        path.set_file_name(format!(
            "cw-bridge-test-{}-{n}.json",
            std::process::id()
        ));
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn vault_loads_and_validates_endpoints() {
        // 加载+校验：本机 http 收；重复 port 后者胜；非本机 http 拒收；
        // https 停用；port<=0 拒收；ports() 升序
        let path = write_vault(
            r#"[
                {"port": 2, "url": "http://127.0.0.1:18080", "note": "b"},
                {"port": 1, "url": "http://localhost:18081", "note": "a"},
                {"port": 2, "url": "http://127.0.0.1:19999", "note": "later-wins"},
                {"port": 3, "url": "http://10.0.0.5:80", "note": "non-local"},
                {"port": 4, "url": "https://example.com/api", "note": "https"},
                {"port": 0, "url": "http://127.0.0.1:1", "note": "bad-port"}
            ]"#,
        );
        let vault = BridgeVault::load(&path);
        assert_eq!(vault.ports(), vec![1, 2], "有效端点应为 1、2（升序）");
        let ep2 = vault.resolve(2).expect("port 2 应在库");
        assert_eq!(ep2.url, "http://127.0.0.1:19999", "重复登记后者胜");
        assert_eq!(ep2.port_num, 19999);
        assert_eq!(ep2.path, "/");
        let ep1 = vault.resolve(1).expect("port 1 应在库");
        assert_eq!(ep1.host, "localhost");
        assert!(vault.resolve(3).is_none(), "非本机 http 应拒收");
        assert!(vault.resolve(4).is_none(), "https 应停用");
        assert!(vault.resolve(0).is_none(), "port<=0 应拒收");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn vault_missing_file_loads_empty() {
        // 首启无保险库文件 = 空表（不报错不崩世界）
        let path = std::env::temp_dir().join(format!(
            "cw-bridge-missing-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let vault = BridgeVault::load(&path);
        assert!(vault.ports().is_empty());
    }

    #[test]
    fn vault_reload_on_mtime_change() {
        // 热重载：文件变更（mtime 变化）后 reload_if_changed 重载出新端点集
        let path = write_vault(r#"[{"port": 1, "url": "http://127.0.0.1:18080"}]"#);
        let mut vault = BridgeVault::load(&path);
        assert_eq!(vault.ports(), vec![1]);
        // mtime 精度保护：等一拍再改写（NTFS mtime 分辨率足够，50ms 保险）
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(
            &path,
            r#"[{"port": 1, "url": "http://127.0.0.1:18080"}, {"port": 7, "url": "http://localhost:1"}]"#,
        )
        .unwrap();
        assert!(vault.reload_if_changed(), "mtime 变化应触发重载");
        assert_eq!(vault.ports(), vec![1, 7], "热重载后应见到新端点");
        // 未变更不再重载
        assert!(!vault.reload_if_changed());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn vault_file_deleted_reloads_empty() {
        // 文件被删 -> mtime 变 None -> 重载为空表（端点热重载后消失的信件作废路径前置）
        let path = write_vault(r#"[{"port": 1, "url": "http://127.0.0.1:18080"}]"#);
        let mut vault = BridgeVault::load(&path);
        assert_eq!(vault.ports(), vec![1]);
        std::fs::remove_file(&path).unwrap();
        assert!(vault.reload_if_changed());
        assert!(vault.ports().is_empty(), "删除后重载应为空表");
    }

    #[test]
    fn vault_host_case_insensitive_accepts_localhost_variants() {
        // P2-D：host 比较大小写不敏感——http://LocalHost:8080 应被接受
        //（等价 127.0.0.1）；非回环 host 即使大写形态仍拒收
        let path = write_vault(
            r#"[
                {"port": 1, "url": "http://LocalHost:8080"},
                {"port": 2, "url": "http://LOCALHOST"},
                {"port": 3, "url": "http://Example.COM"}
            ]"#,
        );
        let vault = BridgeVault::load(&path);
        assert!(vault.resolve(1).is_some(), "LocalHost 混合大小写应接受");
        assert!(vault.resolve(2).is_some(), "LOCALHOST 全大写应接受");
        assert!(vault.resolve(3).is_none(), "非回环 host（大写形态）仍拒收");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn vault_transient_read_failure_keeps_mtime_uncached_for_retry() {
        // P2-B：瞬时读取失败（目录替代法——metadata 对目录成功、read_to_string
        // 对目录报错且非 NotFound，等价杀软/备份锁的瞬时失败形态）不得缓存 mtime：
        // 下拍 reload_if_changed 必须仍触发重试，防"空表+新 mtime"把桥卡死到
        // 下次手改文件
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("cw-bridge-dir-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut vault = BridgeVault::load(&dir);
        assert!(vault.ports().is_empty(), "读取失败按空表运行");
        assert!(
            vault.mtime.is_none(),
            "瞬时读取失败不得缓存 mtime（否则下拍不再重试，桥被钉死）"
        );
        assert!(
            vault.reload_if_changed(),
            "mtime 未被坏读取钉死：下拍应仍触发重载重试"
        );
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn outbox_without_worker_collects_outbound_letters() {
        // 无 worker 构造：不起线程，测试直接收出站信件（字段齐）
        let path = write_vault(r#"[{"port": 1, "url": "http://127.0.0.1:18080"}]"#);
        let mut outbox = BridgeOutbox::without_worker(&path, 5).unwrap();
        outbox.send_letter(Letter {
            entity: id(1),
            port: 1,
            seq: 17,
            payload: b"WHERE?".to_vec(),
            endpoint: outbox.vault().read().unwrap().resolve(1).unwrap().clone(),
        });
        outbox.send_letter(Letter {
            entity: id(2),
            port: 1,
            seq: 1,
            payload: vec![],
            endpoint: outbox.vault().read().unwrap().resolve(1).unwrap().clone(),
        });
        let l1 = outbox.try_recv_letter().expect("应收到第一封");
        assert_eq!(l1.entity, id(1));
        assert_eq!(l1.seq, 17);
        assert_eq!(l1.payload, b"WHERE?");
        assert_eq!(l1.endpoint.port_num, 18080);
        let l2 = outbox.try_recv_letter().expect("应收到第二封");
        assert_eq!(l2.seq, 1);
        assert!(outbox.try_recv_letter().is_none(), "收完即 None");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn envelope_json_fields_match_protocol() {
        // 信封形状（design-bridge 2.1）：world/entity/port/seq/payload 五字段；
        // entity=十进制字符串（u128 数字形态 serde_json panic，见 envelope_json 文档）
        let bytes = envelope_json(42, 1, 17, b"hi");
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["world"], "codeworld");
        assert_eq!(v["entity"], "42", "entity 应为十进制字符串（u128 承载）");
        assert_eq!(v["port"], 1);
        assert_eq!(v["seq"], 17);
        assert_eq!(v["payload"], serde_json::json!([104, 105]), "payload 应为字节数组");
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        assert_eq!(keys.len(), 5, "信封恰好五字段（不多不少）");
        // 大数 u128 不失真（对账锚：EntityId 是随机 u128，全量可逆）
        let big = envelope_json(u128::MAX, 1, 1, b"");
        let bv: serde_json::Value = serde_json::from_slice(&big).unwrap();
        assert_eq!(
            bv["entity"],
            "340282366920938463463374607431768211455",
            "u128::MAX 应无损承载"
        );
    }

    /// 迷你 HTTP 服务器：accept 一次，读请求，回 200 {"dx":3}，返回收到的请求字节
    fn mini_http_server(listener: std::net::TcpListener) -> std::io::Result<Vec<u8>> {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let mut req = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    req.extend_from_slice(&buf[..n]);
                    // 读到请求体即可回（按 Content-Length 判，简化：收到 \r\n\r\n+body）
                    if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                        let cl = req[..pos]
                            .split(|b| *b == b'\n')
                            .find_map(|line| {
                                let s = std::str::from_utf8(line).ok()?;
                                s.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        if req.len() >= pos + 4 + cl {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 9\r\nConnection: close\r\n\r\n{\"dx\":3}")?;
        stream.flush()?;
        Ok(req)
    }

    #[test]
    fn resolve_host_addr_maps_localhost_variants_to_loopback() {
        // P1-A：host→SocketAddr 解析——127.0.0.1 与 localhost 全部大小写形态
        // 都映射 127.0.0.1（不走 DNS 悬挂）；其他 host 一律 None（护栏双重防御，
        // 调用侧归超时路径不投递）
        for host in ["127.0.0.1", "localhost", "LocalHost", "LOCALHOST"] {
            let addr = resolve_host_addr(host, 8080)
                .unwrap_or_else(|| panic!("本机回环 host {host} 应可解析"));
            assert_eq!(
                addr,
                std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
                "host {host} 应映射回环地址"
            );
        }
        assert!(resolve_host_addr("example.com", 8080).is_none(), "非回环 host 应 None");
        assert!(resolve_host_addr("", 8080).is_none(), "空 host 应 None");
    }

    #[test]
    fn catch_letter_panic_swallows_panic_and_reports() {
        // P2-C：护栏行为——工作单元 panic 不炸调用方（工作线程存活语义），
        // panic 载荷转交 on_panic（不静默）；正常路径原样返回且不触发 on_panic
        let hit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hit2 = hit.clone();
        let r = catch_letter_panic(
            || -> &'static str { panic!("模拟处理中 panic"); },
            |_payload| hit2.store(true, std::sync::atomic::Ordering::SeqCst),
        );
        assert!(r.is_none(), "panic 路径应返 None");
        assert!(
            hit.load(std::sync::atomic::Ordering::SeqCst),
            "on_panic 应被调用（线程存活状态可见，不静默）"
        );
        let r2 = catch_letter_panic(|| 42_i32, |_| panic!("成功路径不应触发 on_panic"));
        assert_eq!(r2, Some(42), "正常路径应原样返回");
    }

    #[test]
    fn http_post_roundtrip_against_local_listener() {
        // 手搓 HTTP 往返：请求含方法/路径/Host/Content-Type/Content-Length/
        // Connection: close + 代签头；响应 200 body 原样解析
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || mini_http_server(listener));
        let endpoint = BridgeEndpoint {
            port: 1,
            url: format!("http://127.0.0.1:{}/hook", addr.port()),
            host: "127.0.0.1".to_string(),
            port_num: addr.port(),
            path: "/hook".to_string(),
            headers: [("Authorization".to_string(), "Bearer tok".to_string())]
                .into_iter()
                .collect(),
        };
        let body = envelope_json(9, 1, 1, b"ping");
        let reply = http_post(&endpoint, &body, Duration::from_secs(5)).expect("200 应成功");
        assert_eq!(reply, b"{\"dx\":3}", "回信 body 应原样");
        let req = server.join().unwrap().unwrap();
        let req_text = String::from_utf8_lossy(&req);
        assert!(req_text.starts_with("POST /hook HTTP/1.1\r\n"), "方法/路径/版本");
        assert!(req_text.contains("Connection: close\r\n"), "短连接声明");
        assert!(req_text.contains("Content-Type: application/json\r\n"));
        assert!(
            req_text.contains(&format!("Content-Length: {}\r\n", body.len())),
            "Content-Length 应与信封字节数一致"
        );
        assert!(req_text.contains("Authorization: Bearer tok\r\n"), "代签头应原样贴上");
        // 请求体=信封 JSON：服务器侧解析回 Value 验证五字段（serde_json 默认按键
        // 排序，字段顺序不断言——玩家服务器按字段名读，顺序无关）
        let body_start = req.windows(4).position(|w| w == b"\r\n\r\n").expect("请求应有头体分隔") + 4;
        let body_val: serde_json::Value =
            serde_json::from_slice(&req[body_start..]).expect("请求体应为合法 JSON 信封");
        assert_eq!(body_val["world"], "codeworld");
        assert_eq!(body_val["entity"], "9", "entity 应为十进制字符串（u128 承载）");
        assert_eq!(body_val["port"], 1);
        assert_eq!(body_val["seq"], 1);
        assert_eq!(
            body_val["payload"],
            serde_json::json!([112, 105, 110, 103]),
            "payload 应为 \"ping\" 的字节数组"
        );
    }

    #[test]
    fn http_post_non_2xx_fails() {
        // 非 2xx -> Err（归超时路径不投递）
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(b"HTTP/1.1 500 Oops\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            stream.flush().unwrap();
        });
        let endpoint = BridgeEndpoint {
            port: 1,
            url: format!("http://127.0.0.1:{}", addr.port()),
            host: "127.0.0.1".to_string(),
            port_num: addr.port(),
            path: "/".to_string(),
            headers: HashMap::new(),
        };
        assert!(http_post(&endpoint, b"{}", Duration::from_secs(5)).is_err());
        server.join().unwrap();
    }

    #[test]
    fn worker_delivers_reply_and_timeout_events() {
        // worker 线程端到端（进程内）：可达端点 -> Reply 事件；不可达端点 -> Timeout
        let path = write_vault(r#"[{"port": 1, "url": "http://127.0.0.1:1"}]"#);
        // 先占住一个真端口给端点 1 用（动态端口，注册进保险库文件再建 outbox）
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::fs::write(
            &path,
            serde_json::to_string(&serde_json::json!([{
                "port": 1,
                "url": format!("http://127.0.0.1:{}", addr.port()),
            }]))
            .unwrap(),
        )
        .unwrap();
        let mut outbox = BridgeOutbox::new(&path, 1).unwrap(); // 1s 超时：本机往返足够，Timeout 场景不用等太久
        let server = std::thread::spawn(move || mini_http_server(listener));
        outbox.send_letter(Letter {
            entity: id(10),
            port: 1,
            seq: 1,
            payload: b"q".to_vec(),
            endpoint: outbox.vault().read().unwrap().resolve(1).unwrap().clone(),
        });
        // 回信事件（读端点已收 200，worker 应转 Reply）
        let mut got_reply = false;
        for _ in 0..100 {
            if let Some(ReplyEvent::Reply { entity, seq, body, .. }) = outbox.try_recv_reply() {
                assert_eq!(entity, id(10));
                assert_eq!(seq, 1);
                assert_eq!(body, b"{\"dx\":3}");
                got_reply = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(got_reply, "应收到 Reply 事件");
        server.join().unwrap().unwrap();
        // 不可达端点 -> Timeout 事件。注意：本机防火墙把 refused 静默吞成
        // connect 超时（实测 python connect 同端口 timed out），所以这里等满
        // 1s 超时窗（上同 poll 循环）
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead); // 释放端口但不再监听 -> connect 超时（本机防火墙行为）
        std::fs::write(
            &path,
            serde_json::to_string(&serde_json::json!([{
                "port": 2,
                "url": format!("http://127.0.0.1:{dead_port}"),
            }]))
            .unwrap(),
        )
        .unwrap();
        outbox.vault().write().unwrap().reload_if_changed();
        outbox.send_letter(Letter {
            entity: id(11),
            port: 2,
            seq: 2,
            payload: b"x".to_vec(),
            endpoint: outbox.vault().read().unwrap().resolve(2).unwrap().clone(),
        });
        let mut got_timeout = false;
        for _ in 0..60 {
            if let Some(ReplyEvent::Timeout { entity, seq }) = outbox.try_recv_reply() {
                assert_eq!(entity, id(11));
                assert_eq!(seq, 2);
                got_timeout = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(got_timeout, "不可达端点应产生 Timeout 事件");
        let _ = std::fs::remove_file(&path);
    }
}

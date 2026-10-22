//! WASM 沙箱管理：SandboxManager/SandboxHandle/SandboxData
//!
//! 架构文档六节（WASM 沙箱设计）+ 6.3（安全防线）。
//! 基于 wasmtime，启用 fuel 指令计量。spike/route_a 已验证 API。

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;
use wasmtime::{Caller, Config, Engine, Instance, Linker, Module, Store};

use crate::attack::PendingAttack;
use crate::channel::ChannelManager;
use codeworld_core::entity::EntityId;
use codeworld_core::pending::PendingChange;

/// attack payload 最大字节数（1MB，架构 4.2 技术保护）
///
/// import 层拒绝超限请求：防一次性拷贝大块线性内存（最大可到 64MB）+
/// 超大模块编译长时间阻塞 tick（编译不受 fuel 约束）。
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;

/// 造物出生点相对创造者的最大偏移（门二 spawn 位置起点限制）
///
/// 出生坐标必须在创造者快照坐标 ±10 格（每轴盒式判定）内——防远程投射刷屏。
/// MVP 一刀切的简单规则；规则由宪法允许后续玩法化（decisions.md #012）。
pub const SPAWN_RANGE: f64 = 10.0;

/// spawn 外观字符串（glyph/color，NUL 结尾）最大字节数
///
/// import 层扫描上限：内存里没有 NUL 时最多读这么多字节就拒绝，
/// 防无界扫描线性内存。
pub const MAX_META_LEN: usize = 64;

/// field_pulse 脉冲半径合法下限（D-012：域 [0.1, 25.0]——0.1 防零/微半径刷屏）
pub const FIELD_PULSE_RADIUS_MIN: f64 = 0.1;
/// field_pulse 脉冲半径合法上限（D-012：与世界尺度护栏同值——脉冲再大盖不过
/// 身体 radius 上限尺度）
pub const FIELD_PULSE_RADIUS_MAX: f64 = 25.0;

/// on_field 场信箱容量（D-012 协议 v3：host 写域内实体清单的缓冲区槽数上限）
///
/// 世界实体上限 64（#045），域内实体（不含场源自己）最多 63——64 槽恒够，
/// 信箱永不截断。信箱=线性内存末尾 4+64×8=516 字节（世界保留，玩家勿占用，
/// api.md 载明）。
pub const FIELD_MAILBOX_CAP: usize = 64;

/// spawn 出生数据包最大字节数（64KB，D-017 挂靠立法：世界不解释内容）
///
/// blob_len 超限 → 整次 spawn 拒绝（在读内存之前拦下防大拷贝）；
/// **不裁剪**——裁剪数据包 = 偷改玩家数据。
pub const MAX_SPAWN_BLOB_LEN: usize = 64 * 1024;

/// thrust 单次出力上限（D-010a 冲量小刀，1.7 标定法跳跃锚点）：向量模长上限
/// 7.7 格/拍。推导：跳跃 ≤3 格高 → 连续弹道峰值 h = v0²/(2g) → v0 = √(2·9.8·3)
/// ≈ 7.67，取 7.7。超限归一化压回（方向不变）——满力 = 跳 3 格高。
pub const THRUST_MAX_PER_CALL: f64 = 7.7;

/// thrust 出力燃料系数（D-010a 参数锚点）：满力一次 ≈5% fuel。
/// 代价 = (有效冲量模长 / THRUST_MAX_PER_CALL) × (fuel_limit × 5%)。
pub const THRUST_FUEL_RATIO: f64 = 0.05;

/// 本 tick 排队的 thrust 冲量请求（env.thrust 推入，阶段二收集统一应用）
///
/// 【#021 切割声明】已退役的"推挤"（持续重叠互相挤开→连体 bug）≠ 冲量（一次性
/// 动量交换，撞完就分）：thrust 是一次性冲量非持续推挤。
/// 【力出自己身铁律】只开 thrust 给 self，不开放"推别人"API（推人走碰撞或击退
/// payload，与"攻击形态归玩家"同哲学）。
#[derive(Debug, Clone)]
pub struct PendingThrust {
    /// 出力者（冲量只施加给自己）
    pub entity: EntityId,
    /// 冲量矢量（import 层已过上限归一化钳制，模长 ≤ THRUST_MAX_PER_CALL）
    pub impulse: (f64, f64),
}

/// 本 tick 排队的 spawn 请求（env.spawn 推入，阶段二收集统一处理）
///
/// import 层只做零 world 校验（限流/有限性/钻地/±10 格/外观格式），
/// 产权链 owner 继承与世界实体上限在阶段二判（import 拿不到 world）。
#[derive(Debug, Clone)]
pub struct PendingSpawn {
    /// 创造者（沙箱所属实体）
    pub creator: EntityId,
    /// 造物出生坐标（import 层已校验：有限、y>=0、创造者快照 ±10 格内）
    pub pos: (f64, f64),
    /// 造物 WAT 源码文本（阶段二编译；编译失败作废不崩世界）
    pub code: String,
    /// 造物外观字形（import 层已校验：恰好 1 个字符）
    pub glyph: String,
    /// 造物外观颜色（import 层已校验：非空）
    pub color: String,
    /// 身体半径（#030 spawn 第 7 参；import 层已校验钳制 0.05..=25.0；
    /// creation 身体=占地格 AABB 不用它，快照协议携带）
    pub radius: f64,
    /// 质量（#034 spawn 第 8 参；import 层已钳制 0.05..=100.0，物理人格属性）
    pub mass: f64,
    /// 绑定契约（#034 修订四 spawn 第 9 参；import 层已钳制非法值为 unbound：
    /// 0=独立存在 1=fade_on_death 主人死塔消亡）
    pub binding: i32,
    /// 运动身份（D-003 spawn 第 10 参；import 层已钳制非法值为 static：
    /// 0=static 钉死（默认=现状，格对齐 AABB）/ 1=dynamic 全活（受完整物理，
    /// 身体=声明 radius 的圆）；kinematic 剧本档缓立）
    pub motion: i32,
    /// 出生初速 x 分量（spawn 第 11 参，D-018 一体式发射提前落地；import 层已校验：
    /// 有限 + 模长 ≤ 世界极速，恰等放行。velocity 字段所有实体都有——static 造物
    /// 钉死不吃速度物理，初速仅对 dynamic 造物有物理意义，照存不裁）
    pub vx: f64,
    /// 出生初速 y 分量（spawn 第 12 参，校验同上）
    pub vy: f64,
    /// 出生数据包（spawn 第 13/14 参，D-017 挂靠立法：世界不解释内容——母体传讯/
    /// 代际遗传/子弹带任务参数，通道归世界内容归玩家。blob_len≤0 = 空包合法；
    /// > MAX_SPAWN_BLOB_LEN 整次拒绝不裁剪。随造物沙箱存活，经 env.birth_blob 读回）
    pub blob: Vec<u8>,
    /// 场域半径（D-012 spawn 第 15 参；import 层已钳制 [0.0, 25.0]，0=无场默认）
    pub field_radius: f64,
    /// 场重力缩放（D-012 spawn 第 16 参；import 层已钳制 [0.0, 10.0]，1.0=中性）
    pub field_gravity_scale: f64,
    /// 场阻力改写量（D-012 spawn 第 17 参；加法式 delta，import 层已钳制
    /// [-1.0, +25.0]，0.0=中性）
    pub field_drag_delta: f64,
    /// 液体密度（D-009 spawn 第 18 参；import 层已钳制 [0.0, 5.0]，0=非液体默认）
    pub liquid_density: f64,
    /// 水中阻尼（D-009 spawn 第 19 参；import 层已钳制 [0.0, 5.0]，0=无阻尼；
    /// 结算时有效保留系数另钳 [0,1]）
    pub liquid_drag: f64,
    /// 链声明（D-011 spawn 第 20 参；import 层已钳制非法值为无链：
    /// 0=放生默认 / 1=活链可控+可传，binding 家族新成员）
    pub link: i32,
    /// 造物 fuel 配额（继承创造者沙箱的 fuel_limit）
    pub fuel_limit: u64,
}

/// 链消息出站方向（D-011：import 层只认方向，扇出/沿链寻址在世界侧阶段二做
/// ——import 闭包是 'static 拿不到 world）
#[derive(Debug, Clone)]
pub enum LinkOutKind {
    /// 下行广播给名下全部活链造物（to 由阶段二按 link_to 账本扇出）
    Command,
    /// 上行沿链投母体（to=拍首快照的 link_parent）
    Report { to: EntityId },
    /// 横向坐标寻址（to=坐标命中的视口内实体）
    Send { to: EntityId },
    /// 全量覆盖自己的公开存储块（世界侧落账，当拍闭环不进投递信箱）
    StorageWrite,
}

/// 本 tick 排队的链路出站请求（五 import 的 command/report/send/storage_write
/// 共用管道；storage_read 是同步读不走此队列）
#[derive(Debug, Clone)]
pub struct PendingLinkOut {
    /// 发送方（沙箱所属实体）
    pub from: EntityId,
    /// 出站方向
    pub kind: LinkOutKind,
    /// 信体（import 层已读出并过上限校验）
    pub payload: Vec<u8>,
}

/// 贴身可读实体快照（D-011 storage_read/send 的数据通路）：host 拍首按视口
/// 过滤存活实体填充（与 visible 同批同源口径，EntityId 升序稳定排序、排除自己）
#[derive(Debug, Clone)]
pub struct LinkPeer {
    /// 实体 id（send 坐标命中后的投递目标）
    pub id: EntityId,
    /// 坐标快照（精确匹配寻址——visible_at 给出什么就发什么，所见即所发）
    pub pos: (f64, f64),
    /// 身体半径快照（贴身判据 r_self + r_peer + 0.5 用）
    pub radius: f64,
    /// 公开存储块共享快照（storage_read 拉读的数据源）。Arc=host 拍首每实体
    /// 单份快照、各读方 peers 引用同一份（D-011-R1 P1-3：N 份而非逐沙箱 N² 份
    /// 满盘拷贝；一拍一快照，拍间不共享）
    pub storage: Arc<Vec<u8>>,
}

/// storage_read 贴身判距冗余（与 melee 同款 0.5——读硬盘=物理凑过去读，
/// design-link 五节；引用 core 单一实现防两套口径）
pub const STORAGE_READ_REACH: f64 = codeworld_core::collision::MELEE_REACH;

/// 本 tick 排队的 destroy 请求（env.destroy 推入，阶段二产权校验后应用）
///
/// 产权约束（宪法 #015）：MVP 只允许销毁自家造物（owner 匹配，阶段二用 world 判）；
/// 击杀掠夺式的销毁走攻击通路，不归 destroy（destroy 无掠夺）。
#[derive(Debug, Clone)]
pub struct PendingDestroy {
    /// 销毁发起者
    pub destroyer: EntityId,
    /// 目标实体（i64 两半重建）
    pub target: EntityId,
}

/// 本 tick 排队的力场脉冲请求（D-012 交付物 8：分离式发射器，env.field_pulse
/// 推入，阶段二收集统一应用）
///
/// 语义：以 (x,y) 为中心 radius 半径圆域内，给所有带速度实体（弹丸+dynamic
/// 造物+任何 velocity≠0 的实体）速度一次性加上 (vx,vy) 冲量——物理推，不是
/// 规则层（velocity≡0 的实体推不动：物理推是速度叠加，碰撞响应另刀）。
/// D-018 shoot 退役工程的初速来源：存量炮弹+炮身（场造物）每拍 pulse 推膛内炮弹。
#[derive(Debug, Clone)]
pub struct PendingFieldPulse {
    /// 脉冲发起者（沙箱所属实体；与脉冲作用域无豁免关系——场不认人）
    pub source: EntityId,
    /// 脉冲中心 x（import 层已校验：有限、调用者快照 ±10 格内）
    pub x: f64,
    /// 脉冲中心 y
    pub y: f64,
    /// 冲量 x 分量（import 层已校验：有限、模长 ≤ 世界极速，恰等放行）
    pub vx: f64,
    /// 冲量 y 分量
    pub vy: f64,
    /// 脉冲半径（import 层已钳制 [0.1, 25.0]）
    pub radius: f64,
}

/// 本 tick 排队的出站桥信件（D-001b env.bridge_send 推入，阶段二收集统一出站）
///
/// port 是保险库别名（沙箱永远拿不到 URL——铁律①，design-bridge 3.2）；
/// payload 任意字节（世界不解释不拆信）。seq 不在此结构：序号由世界出站时
/// 统一取号（SandboxData.bridge_seq 单调递增），沙箱内代码对 seq 不可见。
#[derive(Debug, Clone)]
pub struct PendingBridgeSend {
    /// 目标端点别名（import 层已查过登记快照）
    pub port: i32,
    /// 信体（import 层已读出并过上限校验）
    pub payload: Vec<u8>,
}

/// 沙箱用户数据（Store data，用于被动观测，架构 4.3）
///
/// `mem_read_count`/`mem_write_count` MVP 不真实统计（wasmtime 不直接暴露，
/// 需 import 函数拦截，留 Phase 2），保留字段供 ProbeMeta 结构完整。
///
/// `entity_id`/`pending_attacks` 为攻击流程服务（架构 4.2）：import 函数 env.attack_at
/// 在 on_tick 执行期间被调，此时 world 被借用不可改，故只把攻击请求推入 pending_attacks，
/// 由主循环阶段二统一收集处理（与 PendingChange 模式一致）。
#[derive(Debug, Default)]
pub struct SandboxData {
    /// 初始 fuel 配额（用于计算 consumed）
    pub fuel_limit: u64,
    /// 已消耗 fuel（call 后由 update_fuel 同步）
    pub fuel_consumed: u64,
    /// 内存读次数（MVP 不真实统计）
    pub mem_read_count: u64,
    /// 内存写次数（MVP 不真实统计）
    pub mem_write_count: u64,
    /// API 调用列表（import 函数调用时记录）
    pub api_calls: Vec<String>,
    /// 本沙箱所属实体ID（import 函数需据此识别攻击者；Default 随机值仅占位，
    /// 真实沙箱由 create_sandbox 显式设置）
    pub entity_id: EntityId,
    /// 本 tick 排队的攻击请求（env.attack_at 推入，阶段二收集后 drain 清空）
    pub pending_attacks: Vec<PendingAttack>,
    /// 本沙箱实体的坐标快照（host 每 tick 阶段一执行玩家代码【前】填充；
    /// import 回调拿不到 world，玩家代码经 env.get_pos 读到的是 tick 开始时的状态，
    /// 与架构 4.1"玩家代码读取的是 tick 开始时的快照状态"一致）
    pub pos: (f64, f64),
    /// 最近敌人的坐标快照（host 每 tick 阶段一【前】填充；None=视野内没有其他存活实体，
    /// env.scan 返回 0）。填充按渲染同款 viewport 过滤（架构 4.2"视距内"前置条件：
    /// 屏幕外不可锁定；超视距手段玩家自行构建，#018），单敌人取最近者
    /// （架构 3.2 感知接口的最简版）。
    pub nearest_enemy: Option<(f64, f64)>,
    /// 本 tick 排队的移动请求（env.move_by 推入 PendingChange::SetPos，
    /// 阶段二收集后并入主 pending 队列统一应用，与攻击排队同模式）
    pub pending_moves: Vec<PendingChange>,
    /// 本 tick env.log 的 marker 值序列（按调用顺序记录）。
    /// 战况字幕的盾量上报通路：玩家代码主动 log（如 D 拦截时 log(7) 后跟 log(余量)），
    /// host 侧解析该序列更新字幕——玩家自愿上报，不破坏沙箱隔离（架构 4.2 铁律：
    /// 字幕=渲染层展示，只读，不写回 world）。
    pub log_markers: Vec<i32>,
    /// 可见实体坐标快照（三层感知 Phase A 眼睛列表，架构 3.2 get_visible_entities
    /// 的 import 数据通路）：host 每 tick 阶段一执行玩家代码【前】按 viewport 过滤
    /// 存活实体填充，EntityId 升序稳定排序、排除自己（env.visible_count/visible_at
    /// 的数据源；import 回调拿不到 world，同 pos/nearest_enemy 快照模式）。
    pub visible: Vec<(f64, f64)>,
    /// 本 tick 排队的造物请求（env.spawn 推入，阶段二收集后 drain 清空；
    /// 每实体每 tick 限流 1 个在 import 层保证）
    pub pending_spawns: Vec<PendingSpawn>,
    /// 本 tick 排队的销毁请求（env.destroy 推入，阶段二产权校验后 drain 清空）
    pub pending_destroys: Vec<PendingDestroy>,
    /// 本 tick 排队的力场脉冲请求（D-012 env.field_pulse 推入，阶段二收集统一
    /// 应用后 drain 清空；每实体每拍限 1 次在 import 层保证）
    pub pending_field_pulses: Vec<PendingFieldPulse>,
    /// 本 tick 的悬停声明（env.hover 置位，host 阶段一收件并入免重力集合后清空；
    /// 每拍重申语义——玩家代码要持续对抗世界规则，上一拍声明不延续）
    pub hover_declared: bool,
    /// 本 tick 排队的 thrust 冲量请求（env.thrust 推入，host 阶段二收集统一应用
    /// 后 drain 清空；#021 切割声明见 PendingThrust）
    pub pending_thrusts: Vec<PendingThrust>,
    /// 本 tick thrust 累计 fuel 代价（env.thrust import 层累加；host 收集侧统一从
    /// 该沙箱当拍剩余 fuel 扣除——import 闭包内不真扣 store fuel（不打乱配额
    /// 重置语义），也不烧光 fuel 触发 out-of-fuel trap（Windows 进程 abort，
    /// .cc-bugs #5 红线，host 侧 set_fuel 构造等价效果）
    pub thrust_fuel_cost: u64,
    /// 出生数据包（spawn 第 13/14 参的读取侧：造物诞生时 host 写入，造物代码经
    /// env.birth_blob 随时读回——世界不解释内容，通道归世界内容归玩家。随沙箱
    /// 存活不持久化，跨重启不保留；玩家进场与弹丸无包，读回恒 0）
    pub birth_blob: Vec<u8>,
    /// 沙箱资产段（#051 皮入沙箱）：装戴清单——asset_id 列表（av_+16hex，指向
    /// bin 侧 AssetMirror 全局镜像仓；同图全局一份，MC 同款去重）。数据非执行：
    /// 不烧 fuel、不参与编译实例化；换血配额门按镜像仓实大小核实（不信任玩家
    /// 自报，查不到 fail-closed 拒绝换血）。装戴时由 host 写入，与 birth_blob 同
    /// 生命周期（随沙箱存活，不持久化，换血时随行转移——replace_sandbox_assets）。
    pub assets: Vec<String>,
    // === 外桥快照/排队段（D-001b：host 拍首填充 + import 排队 + 阶段二收集）===
    // import 闭包是 'static 拿不到 world/vault，一切经 SandboxData 快照传递
    //（与 pos/nearest_enemy/pending_attacks 同款模式）。

    /// 本拍桥开关快照（host 每拍拍首填充；false 时 bridge_send 恒返 4——
    /// 世界中途关桥要复位，禁用测试环境 SandboxData::default 即 false 零回归）
    pub bridge_enabled: bool,
    /// 已登记 port 快照（升序；host 拍首从保险库读入——import 查表判"无此端点"）
    pub bridge_ports: Vec<i32>,
    /// 桥在途信件计数（import 成功 +1；host 回信/超时/作废/丢弃时 -1。
    /// 沙箱销毁随 SandboxData 一起消亡，无需显式清零）
    pub bridge_inflight: u32,
    /// 本拍已成功发信标记（每沙箱每拍 1 发限流；host 阶段二 collect 出站后清）
    pub bridge_sent_this_tick: bool,
    /// 本拍排队的出站信件（env.bridge_send 推入，host 阶段二收集统一出站）
    pub bridge_pending: Vec<PendingBridgeSend>,
    /// 单实体单调递增信件序号（host collect 出站时取号 +1；玩家对账/去重用，
    /// seq 对沙箱内代码不可见——信封由世界生成）
    pub bridge_seq: u64,
    /// payload 单信上限字节数（host 从 config 装配传入——别与 bin 侧硬编码两套）
    pub bridge_payload_max: usize,
    /// 在途上限（host 从 config 装配传入；import 判"在途满"返 3 的依据）
    pub bridge_inflight_max: u32,
    // === 世界内链路段快照/排队段（D-011：host 拍首填充 + import 排队 + 阶段二
    // === 收集，与 bridge 管道分记账分管道——桥管出世界，链管世界内）===

    /// 名下活链造物数快照（host 拍首按 link_to==自己 且 存活 计数填充；
    /// command 返回值=入队链数的依据，实际扇出在世界侧阶段二）
    pub link_children: u32,
    /// 活链母体快照（host 拍首读实体 link_to 填充；None=无链——report 恒返 2）
    pub link_parent: Option<EntityId>,
    /// 自己身体半径快照（host 拍首填充；storage_read 贴身判据 r_self 侧）
    pub link_radius: f64,
    /// 视口内实体快照（host 拍首按 viewport 过滤存活实体填充，EntityId 升序、
    /// 排除自己——send 坐标寻址与 storage_read 贴身判据的数据源）
    pub link_peers: Vec<LinkPeer>,
    /// 自己公开存储块镜像（host 拍首读实体 storage 填充；storage_write 同时
    /// 更新镜像——同拍读自己可见新值，真源在世界侧实体字段）
    pub link_storage_own: Vec<u8>,
    /// 本拍 command 已成功标记（每沙箱每拍 1 次；host collect 后清——限流以拍为单位）
    pub link_command_sent: bool,
    /// 本拍 report 已成功标记（同上）
    pub link_report_sent: bool,
    /// 本拍 send 已成功标记（同上）
    pub link_send_sent: bool,
    /// 本拍 storage_write 已成功标记（同上）
    pub link_sw_sent: bool,
    /// 本拍 storage_read 已成功标记（读也限 1 次/拍——五 import 同节律）
    pub link_sr_sent: bool,
    /// 本拍排队的链路出站请求（command/report/send/storage_write 共用管道，
    /// 与 bridge_pending 分记账；host 阶段二收集后 drain 清空）
    pub link_pending: Vec<PendingLinkOut>,
    /// 链消息 payload 单信上限字节数（host 从 config 装配传入）
    pub link_payload_max: usize,
    /// 公开存储块容量上限字节数（host 从 config 装配传入；超容拒绝不截断）
    pub storage_cap: usize,
    /// 地形快照指针（D-016：move_by/spawn 的钻地判据从 y<0 改 y<ground(x)）。
    /// 与 probe_bridge 同款窗口契约：仅主循环阶段一执行玩家代码的窗口内非空
    ///（host 挂载 -> call -> 置空同一循环体作用域），窗口外恒 null——
    /// import 层判空时回退平地判据 y<0（无地形的测试环境零回归）。
    /// # SAFETY：挂载方保证指针在窗口期内指向存活的 world.terrain。
    pub terrain: *const codeworld_core::terrain::Terrain,
    /// probe 通路桥指针（三层感知 Phase A，见 ProbeBridge 的 SAFETY 契约）。
    /// 仅在主循环阶段一执行玩家代码的窗口内非空（挂载 -> call -> 置空同一循环体
    /// 作用域），窗口外恒为 null——import 层判空作废探测。
    pub probe_bridge: *mut ProbeBridge,
}

impl SandboxData {
    /// 创建空沙箱数据
    pub fn new() -> Self {
        Self::default()
    }
}

/// 游魂窗口执行体的 store 数据（D-005 刀0：call_duel_step 窗口内专用）
///
/// 游魂是临时执行体（每拍重编译实例化，烧完即弃）——不进 SandboxData 主表。
/// import 闭包是 'static 拿不到调用方引用，守方沙箱经本结构里的指针传递
///（与 ProbeBridge/SandboxData.terrain 同款窗口契约模式）。
pub struct DuelGhostData {
    /// 守方沙箱指针（内存映射直连的"数据线插头"）。
    /// # SAFETY 契约：仅在 call_duel_step 函数体存活期内非空且指向存活的
    /// SandboxHandle（挂载 -> 实例化 -> call -> drop 发生在同一函数体作用域）；
    /// 窗口结束游魂 store 随局部变量销毁，import 闭包不再可能被调。
    pub defender: *mut SandboxHandle,
    /// 当前窗口拍号（env.duel_round 的返回值，0 起）
    pub round: u32,
    /// 本窗口决斗读累计字节数（世界记账日志用）
    pub bytes_read: u64,
    /// 本窗口决斗写累计字节数（世界记账日志用）
    pub bytes_written: u64,
}

/// 游魂窗口投票（D-005 刀0：游魂 on_duel 返回值的机械翻译——世界零裁判，
/// 只翻译游魂自己的返回值，胜负不由世界判定）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuelVerdict {
    /// 游魂返回非 1 值：继续会话（下拍再开窗）
    Continue,
    /// 游魂返回 1：宣布完成，会话正常收尾
    Done,
}

/// 游魂窗口的执行结果（call_duel_step 成功路径的产物；
/// 崩溃走 Err——实例化失败/导出缺失/trap 由调用方按崩溃死关闭会话）
#[derive(Debug, Clone, Copy)]
pub struct DuelStepOutcome {
    /// 本窗口游魂实际烧掉的 fuel（预算 - 剩余）
    pub fuel_used: u64,
    /// 游魂投票（返回值 1=Done，其余=Continue）
    pub outcome: DuelVerdict,
}

/// probe 通路桥（三层感知 Phase A：import 闭包访问目标沙箱的桥接上下文）
///
/// 为什么需要：import 闭包是 'static 的，无法持有 host 引用；probe 又必须同步
/// 调用目标沙箱的 on_probe（玩家代码要当场拿到响应，不能像 attack_at 那样排队）。
/// 桥接方案：主循环阶段一执行玩家代码的窗口内，把窗口上下文挂到每个沙箱的
/// SandboxData.probe_bridge，窗口结束置空。
///
/// # SAFETY 契约（挂载方必须遵守）
/// - **悬垂**：probe_bridge 仅在 host 栈上 ProbeBridge 值存活期间非空（挂载 ->
///   call -> 置空发生在同一循环体作用域），窗口外恒为 null，import 层判空作废。
/// - **aliasing**：caller 自身的 `&mut SandboxHandle` 由外层循环持有；桥里的
///   sandboxes 指针只访问**其他实体**的 entry——坐标查找已排除 caller_id，
///   HashMap 不同 entry 的值指向互不重叠的独立分配。
pub struct ProbeBridge {
    /// 可见实体 id 列表（viewport 过滤 + 存活 + EntityId 升序；含 caller 自己——
    /// probe 的坐标查找在此列表上排除自己，作为 host 快照遗漏时的第二道防线。
    /// 【D-018】发射痕迹并入 visible 的旧口径随痕迹机制退役（#045 连带）：
    /// 造物弹天然在 visible 列表，独立痕迹标记多余）
    pub visible_ids: Vec<EntityId>,
    /// 与 visible_ids 一一平行的坐标列表
    pub visible_positions: Vec<(f64, f64)>,
    /// 沙箱表指针（仅阶段一窗口内有效）
    pub sandboxes: *mut HashMap<EntityId, SandboxHandle>,
    /// 沙箱管理器指针（call_on_probe 经由它走既有 probe 通道，仅窗口内有效）
    pub manager: *const SandboxManager,
    /// 探测者 id（坐标查找排除自己 + 日志）
    pub caller_id: EntityId,
    /// on_probe 独立小 fuel 预算（架构 3.3 probe_target_instruction_limit，config 可调）
    pub probe_fuel: u64,
}

/// 地形钻地判据（D-016）："y < ground(x)" 的沙箱侧实现。
/// terrain 指针判空：无 host 挂载（测试环境/窗口外）回退平地判据 y<0.0——
/// 与旧世界零回归（平地地形 ground(x)≡0 两判据等价）。
/// 返回 Some(ground_y)=钻地（携带地面高度供日志），None=合法。
fn terrain_depth_violation(
    terrain: *const codeworld_core::terrain::Terrain,
    x: f64,
    y: f64,
) -> Option<f64> {
    let ground = if terrain.is_null() {
        0.0
    } else {
        // SAFETY：窗口契约（见 SandboxData.terrain 文档）——窗口期内指针指向
        // 存活的 world.terrain，import 闭包只在此窗口内被玩家代码调用
        unsafe { (*terrain).height(x) }
    };
    (y < ground).then_some(ground)
}

/// 沙箱句柄（持有 Store + Instance）
#[derive(Debug)]
pub struct SandboxHandle {
    /// WASM Store（含用户数据 + fuel）
    pub store: Store<SandboxData>,
    /// WASM 实例
    pub instance: Instance,
}

/// 沙箱管理器（架构文档六节）
///
/// 持有 wasmtime Engine（启用 fuel 指令计量）和 Linker（注册系统 import 函数）。
pub struct SandboxManager {
    engine: Engine,
    linker: Linker<SandboxData>,
    /// 决斗游魂专用 linker（D-005 刀0）：只提供三个决斗原语 import——
    /// 游魂的最小环境（通信自由，注入无门：进来只有决斗三原语，
    /// 走私任何其他 env.* import 的模块拒绝实例化）
    duel_linker: Linker<DuelGhostData>,
}

impl SandboxManager {
    /// 创建管理器：初始化 Engine（启用 fuel）和 Linker（注册 env.log）。
    /// 世界极速用缺省常数 WORLD_MAX_SPEED（测试与默认装配用）
    pub fn new() -> Result<Self> {
        Self::new_with_max_speed(codeworld_core::physics::WORLD_MAX_SPEED)
    }

    /// 创建管理器（指定世界极速，#034 修订一）：发射钳制与飞行钳制同源。
    /// 【D-018】够不着判定阻力装配（new_with_reach_physics）随 shoot import
    /// 退役——阻力只活在物理步/解算器，不再有判定侧消费点
    pub fn new_with_max_speed(max_speed: f64) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        // 内存限制：每个 WASM 实例最大 64MB（CLAUDE.md 技术约束）
        // static_memory_maximum_size 设静态内存上限；完整动态内存硬上限用 ResourceLimiter（Phase 2）
        config.static_memory_maximum_size(64 * 1024 * 1024);
        let engine = Engine::new(&config)?;
        let linker = Self::make_linker(&engine, max_speed)?;
        // 决斗游魂 linker（D-005 刀0）：与主 linker 分家——游魂窗口的最小 import 环境独立成表
        let duel_linker = Self::make_duel_linker(&engine)?;
        tracing::debug!(max_speed, "SandboxManager 创建");
        Ok(Self { engine, linker, duel_linker })
    }

    /// 构造 Linker，注册系统 import 函数（env.log / env.attack_at）
    ///
    /// env.attack_at(x, y, payload_ptr, payload_len) -> i32（架构 4.2）：
    ///   - 不检查坐标有无实体（import 无 world 访问权），只把攻击请求排队到
    ///     SandboxData.pending_attacks，实际 hit/miss 在阶段二 process_attacks 判断。
    ///   - payload 从 caller 的 memory export 读取 [ptr..ptr+len]；模块未导出 memory
    ///     或 ptr<0/len<=0/越界 -> 空 payload（空 payload 由阶段二用默认击杀 WAT）。
    ///   - len 超过 MAX_PAYLOAD_LEN（1MB）直接拒绝：不入队、不拷贝内存
    ///     （防大块拷贝 + 超大模块编译阻塞 tick，编译不受 fuel 约束）。
    ///   - 返回 1=已排队，0=拒绝（超限）。
    fn make_linker(engine: &Engine, max_speed: f64) -> Result<Linker<SandboxData>> {
        let mut linker = Linker::new(engine);
        // env.log(v)：玩家代码日志（v 为玩家自定义 marker，含义见各生物 WAT 注释）。
        // 记 api_calls 元数据 + marker 值序列（log_markers，host 字幕解析用）
        // + tracing::info 输出到 stderr（战斗节点可见性）。
        linker.func_wrap("env", "log", |mut caller: Caller<SandboxData>, v: i32| {
            let data = caller.data_mut();
            data.api_calls.push("log".to_string());
            data.log_markers.push(v);
            tracing::info!(
                entity_id = %data.entity_id.0,
                marker = v,
                "[player_log] 玩家代码日志"
            );
        })?;
        // env.get_pos() -> (f64, f64)：读自己坐标（架构 3.2 空间接口）。
        // 多值返回用元组（wasmtime multi-value）；数据来自 host 填充的坐标快照。
        linker.func_wrap("env", "get_pos", |mut caller: Caller<SandboxData>| -> (f64, f64) {
            caller.data_mut().api_calls.push("get_pos".to_string());
            let (x, y) = caller.data().pos;
            tracing::debug!(
                entity_id = %caller.data().entity_id.0,
                x,
                y,
                "[get_pos] 玩家代码读取自身坐标"
            );
            (x, y)
        })?;
        // env.scan(x_ptr, y_ptr) -> i32：感知接口 MVP（架构 3.2 感知接口最简版，
        // 文档完整版为 get_visible_entities 返回可见实体列表；MVP 单敌人=最近者）。
        // 有敌人：把最近敌人坐标写进 caller memory（x 8字节@x_ptr、y 8字节@y_ptr），
        // 返回 1；无敌人或写入指针非法：不写 memory，返回 0（玩家代码自判）。
        linker.func_wrap(
            "env",
            "scan",
            |mut caller: Caller<SandboxData>, x_ptr: i32, y_ptr: i32| -> i32 {
                caller.data_mut().api_calls.push("scan".to_string());
                let entity = caller.data().entity_id;
                let Some(enemy) = caller.data().nearest_enemy else {
                    tracing::debug!(entity_id = %entity.0, "[scan] 视野内无目标");
                    return 0;
                };
                // 原子语义预检：两个指针先全量校验（只查不写），全部合法才落笔，
                // 避免其一非法时另一半坐标已写入 memory 的半截副作用
                let ptrs_ok = can_write_f64_to_caller(&mut caller, x_ptr)
                    && can_write_f64_to_caller(&mut caller, y_ptr);
                if !ptrs_ok {
                    tracing::warn!(
                        entity_id = %entity.0,
                        x_ptr,
                        y_ptr,
                        "[scan] 写入指针非法，扫描作废"
                    );
                    return 0;
                }
                write_f64_to_caller(&mut caller, x_ptr, enemy.0);
                write_f64_to_caller(&mut caller, y_ptr, enemy.1);
                tracing::info!(
                    entity_id = %entity.0,
                    enemy_x = enemy.0,
                    enemy_y = enemy.1,
                    "[scan] 锁定最近敌人"
                );
                1
            },
        )?;
        // env.move_by(dx, dy) -> i32：相对移动（任务书指定；架构 3.2 文档为绝对 set_pos，
        // MVP 取相对语义——目标点由 host 侧用坐标快照计算，玩家代码无需自己维护位置）。
        // 合法 -> 排队 PendingChange::SetPos 进 pending_moves（阶段二统一应用，与攻击排队
        // 同模式，不即时改坐标）；拒绝 -> 返回 0 不入队。拒绝条件：
        //   - dx/dy 非有限（NaN/无穷，WASM 浮点除零可产生）：防污染世界状态
        //   - 目标点非有限（有限+有限可上溢为无穷）或 y < ground(x)（钻地；D-016 起
        //     地面=高度数组，平地退化 y<0）
        // 注：同 tick 多次 move_by 各自从同一快照算绝对目标，阶段二按序应用后
        // 仅最后一次生效（非增量累加）；正常用法每 tick 只 move 一次。
        linker.func_wrap(
            "env",
            "move_by",
            |mut caller: Caller<SandboxData>, dx: f64, dy: f64| -> i32 {
                caller.data_mut().api_calls.push("move_by".to_string());
                let entity = caller.data().entity_id;
                let (x, y) = caller.data().pos;
                if !dx.is_finite() || !dy.is_finite() {
                    tracing::warn!(
                        entity_id = %entity.0,
                        dx,
                        dy,
                        "[move_by] 偏移量非有限，拒绝入队"
                    );
                    return 0;
                }
                let target = (x + dx, y + dy);
                // 目标点有限性守卫：dx/dy 各自有限不代表和有限——有限+有限按 IEEE 754
                // 可上溢为无穷（如 x=1e308 再 +1e308），非有限坐标入队会污染世界状态
                //（感知算出 NaN、attack_at 坐标相等判定永失配）
                if !target.0.is_finite() || !target.1.is_finite() {
                    tracing::warn!(
                        entity_id = %entity.0,
                        target_x = target.0,
                        target_y = target.1,
                        "[move_by] 目标点非有限，拒绝入队"
                    );
                    return 0;
                }
                if let Some(ground) =
                    terrain_depth_violation(caller.data().terrain, target.0, target.1)
                {
                    tracing::warn!(
                        entity_id = %entity.0,
                        target_x = target.0,
                        target_y = target.1,
                        ground,
                        "[move_by] 目标钻地（y<ground(x)，D-016），拒绝入队"
                    );
                    return 0;
                }
                caller
                    .data_mut()
                    .pending_moves
                    .push(PendingChange::SetPos {
                        entity,
                        new_pos: target,
                    });
                tracing::info!(
                    entity_id = %entity.0,
                    from = ?(x, y),
                    to = ?target,
                    "[move_by] 移动已排队（阶段二应用）"
                );
                1
            },
        )?;
        // env.hover()：玩家代码声明"本拍我要悬停"（期四第七刀）。无参无返回，只置位
        // SandboxData.hover_declared——host 阶段一执行完玩家代码后收集声明并入本拍
        // 免重力集合（与操控悬停同集合同机制，decisions.md #030 升力独立）。
        // 每拍重申语义：声明不跨拍延续，玩家代码要悬停就得每拍出力声明（#029 无税，
        // 但"持续对抗世界规则"以代码持续声明体现）。
        linker.func_wrap("env", "hover", |mut caller: Caller<SandboxData>| {
            let entity = caller.data().entity_id;
            caller.data_mut().api_calls.push("hover".to_string());
            caller.data_mut().hover_declared = true;
            tracing::info!(
                entity_id = %entity.0,
                "[hover] 悬停声明（本拍免重力，每拍需重申）"
            );
        })?;
        // env.thrust(dx, dy) -> i32：本拍给自己一记冲量（D-010a 冲量小刀，第 14 个
        // 玩家 import）。冲量只入自己的速度字段（力出自己身铁律：不开放"推别人"
        // API，推人走碰撞或击退 payload），阶段二统一累加应用，本拍物理步的速度
        // 积分路径带位移——跳跃/踏水推进/轻功水上漂的地基（D-010 自主出力）。
        // 【#021 切割声明】已退役的"推挤"（持续重叠互相挤开→连体 bug）≠ 冲量
        //（一次性动量交换，撞完就分）：thrust 是一次性冲量非持续推挤。
        // 上限钳制：向量模长 > THRUST_MAX_PER_CALL 归一化压回（方向不变）——
        // 满力 = 跳 ≤3 格高（1.7 标定法跳跃锚点，推导见常量注释）。
        // fuel 代价：cost = (有效模长/7.7) × (fuel_limit × 5%)，满力一次 ≈5% fuel。
        // import 侧只粗校验+记账（thrust_fuel_cost 累计），真正扣除在 host 收集侧
        // 统一做（闭包内不扣 store fuel——不打乱配额重置语义；也不烧光 fuel 触发
        // out-of-fuel trap——Windows 上是进程 abort，.cc-bugs #5 红线）。
        // 返回 1 = 已排队；0 = 拒绝（NaN/Inf 输入 / 剩余 fuel 不足本次出力）。
        linker.func_wrap(
            "env",
            "thrust",
            |mut caller: Caller<SandboxData>, dx: f64, dy: f64| -> i32 {
                let entity = caller.data().entity_id;
                caller.data_mut().api_calls.push("thrust".to_string());
                // NaN/Inf 输入按 0 拒绝：冲量进速度字段会污染世界状态（同 move_by 口径）
                if !dx.is_finite() || !dy.is_finite() {
                    tracing::warn!(
                        entity_id = %entity.0,
                        dx,
                        dy,
                        "[thrust] 冲量非有限，拒绝入队"
                    );
                    return 0;
                }
                // 上限钳制：模长超 THRUST_MAX_PER_CALL 归一化压回（方向不变）——
                // 超限请求按满力计价（钳制后模长），白嫖不了方向折扣
                let mag = (dx * dx + dy * dy).sqrt();
                let (ix, iy) = if mag > THRUST_MAX_PER_CALL {
                    let scale = THRUST_MAX_PER_CALL / mag;
                    (dx * scale, dy * scale)
                } else {
                    (dx, dy)
                };
                // fuel 代价：满力 = fuel_limit × 5%（D-010a 出力燃料系数锚点）
                let fuel_limit = caller.data().fuel_limit;
                let eff_mag = (ix * ix + iy * iy).sqrt();
                let cost =
                    ((eff_mag / THRUST_MAX_PER_CALL) * (fuel_limit as f64 * THRUST_FUEL_RATIO))
                        as u64;
                // import 侧粗校验：活体剩余 fuel（wasmtime Caller::get_fuel 实时读，
                // 反映 on_tick 至今的烧量）须覆盖"已记账 thrust 代价 + 本次代价"
                // ——真正的扣除在 host 收集侧统一 set_fuel 做。live_fuel 只会减少
                //（wasm 单调烧），故逐次校验即累计有保障
                let live_fuel = caller.get_fuel().unwrap_or(0);
                if cost > 0 && live_fuel < caller.data().thrust_fuel_cost + cost {
                    tracing::warn!(
                        entity_id = %entity.0,
                        cost,
                        live_fuel,
                        "[thrust] 剩余 fuel 不足本次出力，拒绝"
                    );
                    return 0;
                }
                let data = caller.data_mut();
                data.pending_thrusts.push(PendingThrust {
                    entity,
                    impulse: (ix, iy),
                });
                data.thrust_fuel_cost += cost;
                tracing::info!(
                    entity_id = %entity.0,
                    dx = ix,
                    dy = iy,
                    fuel_cost = cost,
                    "[thrust] 冲量已排队（阶段二应用）"
                );
                1
            },
        )?;
        linker.func_wrap(
            "env",
            "attack_at",
            |mut caller: Caller<SandboxData>, x: f64, y: f64, ptr: i32, len: i32| -> i32 {
                let attacker = caller.data().entity_id;
                // 超限拒绝：在读 memory 之前拦下（避免无谓拷贝），返回 0 告知玩家代码
                if len > 0 && len as usize > MAX_PAYLOAD_LEN {
                    tracing::warn!(
                        attacker = %attacker.0,
                        actual_len = len,
                        max = MAX_PAYLOAD_LEN,
                        "[attack_at] payload 超限，拒绝入队"
                    );
                    return 0;
                }
                let payload = read_payload_from_caller(&mut caller, ptr, len);
                let payload_len = payload.len(); // 实际读取到的字节数（非调用方声称的 len）
                caller
                    .data_mut()
                    .pending_attacks
                    .push(PendingAttack::strike(attacker, (x, y), payload));
                tracing::info!(
                    attacker = %attacker.0,
                    x,
                    y,
                    payload_len,
                    "[attack_at] 攻击已排队"
                );
                1 // 已排队
            },
        )?;
        // env.visible_count() -> i32：眼睛列表（三层感知 Phase A，架构 3.2
        // get_visible_entities 的 import 数据通路）。返回 host 填充的可见实体
        // 坐标快照长度（viewport 过滤 + EntityId 升序 + 排除自己）。
        linker.func_wrap("env", "visible_count", |mut caller: Caller<SandboxData>| -> i32 {
            caller.data_mut().api_calls.push("visible_count".to_string());
            let n = caller.data().visible.len() as i32;
            tracing::debug!(
                entity_id = %caller.data().entity_id.0,
                count = n,
                "[visible_count] 可见敌人数"
            );
            n
        })?;
        // env.visible_at(idx, x_ptr, y_ptr) -> i32：按 idx 取第 N 个可见实体坐标
        //（写 caller memory，同 scan 模式：x 8字节@x_ptr、y 8字节@y_ptr）。
        // 返回 1 成功 / 0 越界（idx 非法）或写入指针非法。
        linker.func_wrap(
            "env",
            "visible_at",
            |mut caller: Caller<SandboxData>, idx: i32, x_ptr: i32, y_ptr: i32| -> i32 {
                caller.data_mut().api_calls.push("visible_at".to_string());
                let entity = caller.data().entity_id;
                // idx<0 as usize 符号扩展成巨大值 -> get 返回 None，与越界同语义
                let Some(p) = caller.data().visible.get(idx as usize).copied() else {
                    tracing::debug!(
                        entity_id = %entity.0,
                        idx,
                        "[visible_at] 越界，无该可见实体"
                    );
                    return 0;
                };
                // 原子语义预检：两个指针先全量校验（只查不写），全部合法才落笔
                //（同 scan：避免其一非法时另一半坐标已写入的半截副作用）
                let ptrs_ok = can_write_f64_to_caller(&mut caller, x_ptr)
                    && can_write_f64_to_caller(&mut caller, y_ptr);
                if !ptrs_ok {
                    tracing::warn!(
                        entity_id = %entity.0,
                        idx,
                        x_ptr,
                        y_ptr,
                        "[visible_at] 写入指针非法，读取作废"
                    );
                    return 0;
                }
                write_f64_to_caller(&mut caller, x_ptr, p.0);
                write_f64_to_caller(&mut caller, y_ptr, p.1);
                tracing::info!(
                    entity_id = %entity.0,
                    idx,
                    x = p.0,
                    y = p.1,
                    "[visible_at] 读取可见实体坐标"
                );
                1
            },
        )?;
        // env.probe(tx, ty, req_ptr, req_len, resp_ptr, resp_cap) -> i32：探测
        //（三层感知 Phase B，架构 3.2 probe 通道 + 4.3 被动观测）。
        // - 按坐标在可见快照（桥）里找目标（架构 3.2 通道开启条件：视距内），
        //   排除自己；MVP 不引入 EntityId 传递（玩家代码持有坐标即可）。
        // - 调 channel.rs 既有 probe 通道（on_probe + meta 采集 + 容错），复用不重写。
        // - 被探测方 on_probe 用 host 侧独立小 fuel 包裹（存剩余 -> set 预算 ->
        //   调 -> 记消耗 -> 恢复原 fuel；已知限制：wasmtime 25 + Windows 下 fuel
        //   耗尽 trap = 进程 abort（.cc-bugs #5），预算把最坏情形缩到快速中止）。
        // - response 之后追加 24 字节被动体徵（host 采集，玩家不可伪造）：
        //   [0..8] 目标 api_calls 计数 u64 LE；[8..16] 目标 fuel 剩余 u64 LE
        //   （探测前快照）；[16..24] 本次 on_probe 消耗 u64 LE。
        // - 返回：>=0 实际写入字节数（超 resp_cap 截断）；-1 坐标无实体/桥未挂载/
        //   resp 不可写；-2 目标无 on_probe（响应位置已写默认被动体徵）。
        linker.func_wrap(
            "env",
            "probe",
            |mut caller: Caller<SandboxData>,
             tx: f64,
             ty: f64,
             req_ptr: i32,
             req_len: i32,
             resp_ptr: i32,
             resp_cap: i32|
             -> i32 {
                caller.data_mut().api_calls.push("probe".to_string());
                let caller_id = caller.data().entity_id;
                let bridge_ptr = caller.data().probe_bridge;
                if bridge_ptr.is_null() {
                    tracing::warn!(entity_id = %caller_id.0, "[probe] 通路桥未挂载，探测作废");
                    return -1;
                }
                // SAFETY: 桥仅在 run_player_code 执行窗口内非空（挂载 -> call ->
                // 置空同一循环体作用域），本函数只在窗口内被调，ProbeBridge 存活。
                let bridge = unsafe { &mut *bridge_ptr };
                // 坐标查找（可见列表内 + 排除自己：host 快照已排除，此处为第二道防线）。
                // 【D-018】#034 修订三的"同坐标实体与痕迹并存"双轨查找随痕迹机制
                // 退役（#045 连带：造物天然在 visible 列表，痕迹分支已删）
                let coordinate_match = |(id, p): (&EntityId, (f64, f64))| {
                    *id != caller_id && p == (tx, ty)
                };
                let target_id = bridge
                    .visible_ids
                    .iter()
                    .zip(bridge.visible_positions.iter().copied())
                    .find(|(id, p)| coordinate_match((id, *p)))
                    .map(|(id, _)| *id);
                let Some(target_id) = target_id else {
                    tracing::info!(entity_id = %caller_id.0, tx, ty, "[probe] 坐标无可见实体");
                    return -1;
                };
                // SAFETY: sandboxes 指针窗口内有效；目标 != caller（上一步排除），
                // 不会与外层持有的 caller 自身 &mut handle aliasing（不同 entry）。
                let sandboxes = unsafe { &mut *bridge.sandboxes };
                let Some(target) = sandboxes.get_mut(&target_id) else {
                    tracing::warn!(
                        entity_id = %caller_id.0,
                        target = %target_id.0,
                        "[probe] 目标无沙箱（死者或框架实体）"
                    );
                    return -1;
                };
                // SAFETY: manager 指针窗口内有效（SandboxManager 与沙箱表同生命周期）。
                let manager = unsafe { &*bridge.manager };
                // request 字节从 caller memory 读（同 attack_at 的 payload 模式）
                let request = read_payload_from_caller(&mut caller, req_ptr, req_len);
                // host 侧独立小 fuel 包裹（架构 3.3 probe_target_instruction_limit）
                let prev_fuel = target.store.get_fuel().unwrap_or(0);
                if let Err(e) = target.store.set_fuel(bridge.probe_fuel) {
                    tracing::warn!(
                        target = %target_id.0,
                        error = %e,
                        "[probe] 预算设置失败，探测作废"
                    );
                    return -1;
                }
                let had_on_probe = manager.has_export(target, "on_probe").unwrap_or(false);
                let probe_result = ChannelManager::probe(manager, target, &request);
                let fuel_left = target.store.get_fuel().unwrap_or(bridge.probe_fuel);
                let probe_fuel_used = bridge.probe_fuel.saturating_sub(fuel_left);
                if let Err(e) = target.store.set_fuel(prev_fuel) {
                    tracing::warn!(target = %target_id.0, error = %e, "[probe] 目标 fuel 恢复失败");
                }
                // update_fuel 在包裹期间用预算尺度算过 fuel_consumed（假象），
                // 恢复后按原尺度重算，目标元数据不被探测污染
                let limit = target.store.data().fuel_limit;
                target.store.data_mut().fuel_consumed = limit.saturating_sub(prev_fuel);
                // 组装响应：目标 on_probe 返回 + 24 字节被动体徵
                let mut body = probe_result.response.clone();
                let api_count = probe_result.meta.api_calls.len() as u64;
                body.extend_from_slice(&api_count.to_le_bytes());
                body.extend_from_slice(&prev_fuel.to_le_bytes());
                body.extend_from_slice(&probe_fuel_used.to_le_bytes());
                if resp_cap <= 0 {
                    tracing::warn!(entity_id = %caller_id.0, resp_cap, "[probe] resp_cap 非正，响应丢弃");
                    return -1;
                }
                let written = write_bytes_to_caller(&mut caller, resp_ptr, &body, resp_cap as usize);
                if written == 0 {
                    tracing::warn!(
                        entity_id = %caller_id.0,
                        resp_ptr,
                        "[probe] 响应写入失败（指针非法/无 memory）"
                    );
                    return -1;
                }
                // response 摘要（前 4 字节 i32 hex；玩家内容照录——框架只透传不审查，
                // 撒谎是否成立由探测者比对 response 与体徵自行判断）
                let resp_head = if probe_result.response.len() >= 4 {
                    format!("0x{:08X}", i32::from_le_bytes(
                        probe_result.response[..4].try_into().unwrap_or([0; 4]),
                    ))
                } else if probe_result.response.is_empty() {
                    "-".to_string()
                } else {
                    format!("{} 字节", probe_result.response.len())
                };
                tracing::info!(
                    prober = %caller_id.0,
                    target = %target_id.0,
                    resp_len = written,
                    resp_head = %resp_head,
                    api_calls = api_count,
                    fuel_used = probe_fuel_used,
                    "[probe] 探测完成（response+被动体徵）"
                );
                if had_on_probe {
                    written as i32
                } else {
                    tracing::info!(
                        target = %target_id.0,
                        "[probe] 目标无 on_probe，仅返回默认被动体徵"
                    );
                    -2
                }
            },
        )?;
        // env.spawn(code_ptr, code_len, x, y, glyph_ptr, color_ptr, radius, mass, binding,
        //           motion, vx, vy, blob_ptr, blob_len) -> i32：造物
        //（门二开放，world-foundation 门二 + decisions.md #012 能力统一：造物再造造物合法）。
        // 玩家代码提交一段 WAT 文本 + 出生坐标 + 外观，请求造一个新实体。
        // import 层只做零 world 校验（同 move_by 模式：快照坐标 host 填充，玩家伪造不了）：
        //   - 限流：每实体每 tick 最多 1 个请求，多余拒绝
        //   - 代码：len>0 且 <= MAX_PAYLOAD_LEN（编译不受 fuel 约束，防超大模块阻塞 tick），
        //     从 memory [ptr..ptr+len) 读 UTF-8 文本
        //   - 出生点：x/y 有限、y>=ground(x)（不钻地，D-016 地形判据）、创造者快照
        //     ±10 格内（SPAWN_RANGE，防远程投射刷屏；规则由宪法允许后续玩法化）
        //   - 外观：glyph/color 从 memory 读 NUL 结尾字符串（≤ MAX_META_LEN），
        //     glyph 恰 1 字符、color 非空
        //   - radius 第 7 参钳制 0.05..=25.0（#030）；mass 第 8 参钳制 0.05..=100.0
        //     （#034 物理人格属性，除零护栏）
        //   - binding 第 9 参钳制非法值回 unbound（#034 修订四）
        //   - motion 第 10 参钳制非法值回 static（D-003）：0=钉死默认/1=全活 dynamic
        //   - 出生初速第 11/12 参：vx/vy 有限 + 模长 ≤ 世界极速（经装配 max_speed，
        //     与飞行钳制同源同口径（世界极速恰等放行）；D-018 一体式发射落地）
        //   - 出生数据包第 13/14 参：blob_len ≤0 = 空包合法（无包语义）；
        //     > MAX_SPAWN_BLOB_LEN 整次拒绝（读内存前拦下防大拷贝，不裁剪——
        //     裁剪数据包=偷改玩家数据）；D-017 挂靠立法：世界不解释内容
        //   - 力场第 15/16/17 参（D-012）：field_radius 钳 [0,25]（0=无场）、
        //     field_gravity_scale 钳 [0,10]（非有限钳 1.0）、field_drag_delta 钳
        //     [-1,+25]（非有限钳 0）——出生声明家族哲学：超界钳回+警告非拒绝
        //   - 液体第 18/19 参（D-009）：liquid_density 钳 [0,5]（0=非液体默认）、
        //     liquid_drag 钳 [0,5]（0=无阻尼；结算时有效保留系数另钳 [0,1]）
        //   - 链第 20 参（D-011）：link 钳非法值回无链（0=放生默认/1=活链）
        // 产权链 owner 继承、世界实体上限、WAT 编译都在阶段二判（返回 1=已排队，
        // 阶段二拒绝以日志为准，api.md 载明）。
        // D-009：spawn 超 17 参——wasmtime 25 的 IntoFunc 宏上限 17 参
        //（Caller+17，func.rs for_each_function_signature 最后一档），func_wrap
        // 装不下 20 个闭包参数，改走 func_new 动态注册（任意元数）。参数经
        // Val::I32/F64 匹配抽取（import 签名已声明类型，运行期只会是声明类型；
        // 防御性回退 0/NAN，落在既有校验拒绝路径上，不崩世界）。
        linker.func_new(
            "env",
            "spawn",
            wasmtime::FuncType::new(
                engine,
                [
                    wasmtime::ValType::I32, wasmtime::ValType::I32, // code_ptr/len
                    wasmtime::ValType::F64, wasmtime::ValType::F64, // x, y
                    wasmtime::ValType::I32, wasmtime::ValType::I32, // glyph/color ptr
                    wasmtime::ValType::F64, wasmtime::ValType::F64, // radius, mass
                    wasmtime::ValType::I32, wasmtime::ValType::I32, // binding, motion
                    wasmtime::ValType::F64, wasmtime::ValType::F64, // vx, vy
                    wasmtime::ValType::I32, wasmtime::ValType::I32, // blob ptr/len
                    wasmtime::ValType::F64, wasmtime::ValType::F64, wasmtime::ValType::F64, // field 三参（D-012）
                    wasmtime::ValType::F64, wasmtime::ValType::F64, // liquid 两参（D-009 第 18/19 参）
                    wasmtime::ValType::I32, // link（D-011 第 20 参）
                ],
                [wasmtime::ValType::I32],
            ),
            move |mut caller: Caller<SandboxData>,
                  params: &[wasmtime::Val],
                  results: &mut [wasmtime::Val]|
             -> Result<()> {
                // 类型抽取（防御性回退：I32→0 / F64→NAN——NAN 会落在既有的
                // 非有限拒绝路径上，warn 带变体名）
                let val_kind = |v: Option<&wasmtime::Val>| -> &'static str {
                    match v {
                        Some(wasmtime::Val::I32(_)) => "i32",
                        Some(wasmtime::Val::I64(_)) => "i64",
                        Some(wasmtime::Val::F32(_)) => "f32",
                        Some(wasmtime::Val::F64(_)) => "f64",
                        Some(_) => "other",
                        None => "missing",
                    }
                };
                let i32_at = |i: usize| -> i32 {
                    match params.get(i) {
                        Some(wasmtime::Val::I32(v)) => *v,
                        other => {
                            tracing::warn!(idx = i, got = val_kind(other), "[spawn] 参数类型异常，按 0 处理");
                            0
                        }
                    }
                };
                let f64_at = |i: usize| -> f64 {
                    match params.get(i) {
                        // wasmtime 25 的 Val::F64 存裸 IEEE-754 位（u64），from_bits 还原
                        Some(wasmtime::Val::F64(v)) => f64::from_bits(*v),
                        other => {
                            tracing::warn!(idx = i, got = val_kind(other), "[spawn] 参数类型异常，按 NAN 处理");
                            f64::NAN
                        }
                    }
                };
                let code_ptr = i32_at(0);
                let code_len = i32_at(1);
                let x = f64_at(2);
                let y = f64_at(3);
                let glyph_ptr = i32_at(4);
                let color_ptr = i32_at(5);
                let radius = f64_at(6);
                let mass = f64_at(7);
                let binding = i32_at(8);
                let motion = i32_at(9);
                let vx = f64_at(10);
                let vy = f64_at(11);
                let blob_ptr = i32_at(12);
                let blob_len = i32_at(13);
                let field_radius = f64_at(14);
                let field_gravity_scale = f64_at(15);
                let field_drag_delta = f64_at(16);
                let liquid_density = f64_at(17);
                let liquid_drag = f64_at(18);
                let link = i32_at(19);
                // 校验体（20 参签名，与 func_wrap 时代闭包体逐行同代码）
                #[allow(clippy::too_many_arguments)]
                let spawn_call = |caller: &mut Caller<SandboxData>,
                                  code_ptr: i32,
                                  code_len: i32,
                                  x: f64,
                                  y: f64,
                                  glyph_ptr: i32,
                                  color_ptr: i32,
                                  radius: f64,
                                  mass: f64,
                                  binding: i32,
                                  motion: i32,
                                  vx: f64,
                                  vy: f64,
                                  blob_ptr: i32,
                                  blob_len: i32,
                                  field_radius: f64,
                                  field_gravity_scale: f64,
                                  field_drag_delta: f64,
                                  liquid_density: f64,
                                  liquid_drag: f64,
                                  link: i32|
                 -> i32 {
                caller.data_mut().api_calls.push("spawn".to_string());
                let creator = caller.data().entity_id;
                // 限流：每实体每 tick 最多 1 个造物请求
                if !caller.data().pending_spawns.is_empty() {
                    tracing::warn!(
                        entity_id = %creator.0,
                        "[spawn] 本 tick 已有造物请求，限流拒绝（1 个/tick）"
                    );
                    return 0;
                }
                // 代码长度预检：在读 memory 前拦下超限/非法长度（同 attack_at）
                if code_len <= 0 || code_len as usize > MAX_PAYLOAD_LEN {
                    tracing::warn!(
                        entity_id = %creator.0,
                        code_len,
                        max = MAX_PAYLOAD_LEN,
                        "[spawn] 造物代码长度非法，拒绝"
                    );
                    return 0;
                }
                let code_bytes = read_payload_from_caller(caller, code_ptr, code_len);
                let Ok(code) = String::from_utf8(code_bytes) else {
                    tracing::warn!(
                        entity_id = %creator.0,
                        "[spawn] 造物代码非 UTF-8 文本，拒绝"
                    );
                    return 0;
                };
                // 出生点校验：有限 + 不钻地 + 创造者快照 ±10 格内
                if !x.is_finite() || !y.is_finite() {
                    tracing::warn!(entity_id = %creator.0, x, y, "[spawn] 出生点非有限，拒绝");
                    return 0;
                }
                if let Some(ground) =
                    terrain_depth_violation(caller.data().terrain, x, y)
                {
                    tracing::warn!(
                        entity_id = %creator.0,
                        x,
                        y,
                        ground,
                        "[spawn] 出生点钻地（y<ground(x)，D-016），拒绝"
                    );
                    return 0;
                }
                let (px, py) = caller.data().pos;
                if (x - px).abs() > SPAWN_RANGE || (y - py).abs() > SPAWN_RANGE {
                    tracing::warn!(
                        entity_id = %creator.0,
                        creator_pos = ?(px, py),
                        birth = ?(x, y),
                        range = SPAWN_RANGE,
                        "[spawn] 出生点超出创造者 ±10 格，拒绝"
                    );
                    return 0;
                }
                // 外观校验：glyph 恰 1 字符、color 非空（NUL 结尾字符串从 memory 读）
                let Some(glyph) = read_cstring_from_caller(caller, glyph_ptr, MAX_META_LEN)
                else {
                    tracing::warn!(
                        entity_id = %creator.0,
                        glyph_ptr,
                        "[spawn] 字形读取失败（指针非法/无 NUL/非 UTF-8），拒绝"
                    );
                    return 0;
                };
                if glyph.chars().count() != 1 {
                    tracing::warn!(
                        entity_id = %creator.0,
                        glyph = %glyph,
                        "[spawn] 字形必须是单字符，拒绝"
                    );
                    return 0;
                }
                let Some(color) = read_cstring_from_caller(caller, color_ptr, MAX_META_LEN)
                else {
                    tracing::warn!(
                        entity_id = %creator.0,
                        color_ptr,
                        "[spawn] 颜色读取失败（指针非法/无 NUL/非 UTF-8），拒绝"
                    );
                    return 0;
                };
                if color.is_empty() {
                    tracing::warn!(entity_id = %creator.0, "[spawn] 颜色为空，拒绝");
                    return 0;
                }
                // 出生初速校验（spawn 第 11/12 参，D-018 一体式发射提前落地）：
                // 有限性 + 模长 ≤ 世界极速（经装配 max_speed，与飞行钳制同源同口径；
                // 严格 > 拒绝——恰等于极速放行）
                if !vx.is_finite() || !vy.is_finite() {
                    tracing::warn!(
                        entity_id = %creator.0,
                        vx,
                        vy,
                        "[spawn] 出生初速非有限，拒绝"
                    );
                    return 0;
                }
                let speed2 = vx * vx + vy * vy;
                if speed2 > max_speed * max_speed {
                    tracing::warn!(
                        entity_id = %creator.0,
                        speed = speed2.sqrt(),
                        max = max_speed,
                        "[spawn] 出生初速超世界极速，拒绝（恰等放行）"
                    );
                    return 0;
                }
                // 出生数据包长度预检（spawn 第 13/14 参，D-017 挂靠立法）：blob_len >0
                // 且超限 → 整次拒绝（读内存之前拦下防大拷贝；不裁剪——裁剪数据包=
                // 偷改玩家数据）。blob_len ≤0 = 空包合法（无包语义）
                if blob_len > 0 && blob_len as usize > MAX_SPAWN_BLOB_LEN {
                    tracing::warn!(
                        entity_id = %creator.0,
                        actual_len = blob_len,
                        max = MAX_SPAWN_BLOB_LEN,
                        "[spawn] 出生数据包超限，整次拒绝（不裁剪）"
                    );
                    return 0;
                }
                let blob = if blob_len <= 0 {
                    Vec::new()
                } else {
                    read_payload_from_caller(caller, blob_ptr, blob_len)
                };
                // fuel 配额继承创造者（造物与创造者同算力档）
                let fuel_limit = caller.data().fuel_limit;
                // #030 第 7 参 radius 校验钳制（0.05..=25.0，非有限回退 0.5，超界 warn）
                let radius = codeworld_core::collision::clamp_radius(radius);
                // #034 第 8 参 mass 校验钳制（0.05..=100.0，非有限回退 1.0，超界 warn）
                let mass = codeworld_core::collision::clamp_mass(mass);
                // #034 修订四第 9 参 binding 校验钳制（非法值钳 unbound，warn）
                let binding = codeworld_core::entity::clamp_binding(binding);
                // D-003 第 10 参 motion 校验钳制（非法值钳 static，warn）
                let motion = codeworld_core::entity::clamp_motion(motion);
                // D-012 第 15/16/17 参力场三参校验钳制（超界钳回+warn 非拒绝：
                // 0=无场 / 1.0=中性缩放 / 0.0=中性 delta 都是无场现状语义）
                let field_radius = codeworld_core::entity::clamp_field_radius(field_radius);
                let field_gravity_scale =
                    codeworld_core::entity::clamp_field_gravity_scale(field_gravity_scale);
                let field_drag_delta =
                    codeworld_core::entity::clamp_field_drag_delta(field_drag_delta);
                // D-009 第 18/19 参液体两参校验钳制（超界钳回+warn 非拒绝：
                // 0=非液体默认 / 0=无阻尼；k>1 声明合法、结算时另钳 [0,1]）
                let liquid_density = codeworld_core::entity::clamp_liquid_density(liquid_density);
                let liquid_drag = codeworld_core::entity::clamp_liquid_drag(liquid_drag);
                // D-011 第 20 参链声明校验钳制（非法值钳无链，warn）
                let link = codeworld_core::entity::clamp_link(link);
                tracing::info!(
                    creator = %creator.0,
                    birth = ?(x, y),
                    glyph = %glyph,
                    color = %color,
                    radius,
                    mass,
                    binding,
                    motion,
                    vx,
                    vy,
                    blob_len = blob.len(),
                    code_len = code.len(),
                    field_radius,
                    field_gravity_scale,
                    field_drag_delta,
                    liquid_density,
                    liquid_drag,
                    link,
                    "[spawn] 造物请求已排队（阶段二统一处理）"
                );
                caller
                    .data_mut()
                    .pending_spawns
                    .push(PendingSpawn {
                        creator,
                        pos: (x, y),
                        code,
                        glyph,
                        color,
                        radius,
                        mass,
                        binding,
                        motion,
                        vx,
                        vy,
                        blob,
                        field_radius,
                        field_gravity_scale,
                        field_drag_delta,
                        liquid_density,
                        liquid_drag,
                        link,
                        fuel_limit,
                    });
                1
                };
                let ret = spawn_call(
                    &mut caller,
                    code_ptr,
                    code_len,
                    x,
                    y,
                    glyph_ptr,
                    color_ptr,
                    radius,
                    mass,
                    binding,
                    motion,
                    vx,
                    vy,
                    blob_ptr,
                    blob_len,
                    field_radius,
                    field_gravity_scale,
                    field_drag_delta,
                    liquid_density,
                    liquid_drag,
                    link,
                );
                results[0] = wasmtime::Val::I32(ret);
                Ok(())
            },
        )?;
        // env.birth_blob(ptr: i32, cap: i32) -> i32：读回自己的出生数据包（第 13 个玩家
        // import，spawn 第 13/14 参的读取侧，D-017 挂靠立法：世界不解释内容——母体传讯/
        // 代际遗传/子弹带任务参数，通道归世界内容归玩家）。数据包由 host 在造物诞生时
        // 写进 SandboxData.birth_blob，本 import 原样搬运回 caller memory：
        //   - 返回 0 = 无数据包（玩家进场/弹丸恒此路径），不写 memory
        //   - 返回正数 N = 已把 N 字节写入 memory[ptr..ptr+N)（原子：能整体写入才写）
        //   - 返回负数 -N = 数据包长 N 但 cap 不足或指针非法，未写任何字节（日志区分
        //     两种原因；玩家按 64KB 上限备缓冲一次必成）
        // 生命周期：随沙箱存活（每拍可读），沙箱不持久化故跨重启不保留（api.md 载明）。
        linker.func_wrap(
            "env",
            "birth_blob",
            |mut caller: Caller<SandboxData>, ptr: i32, cap: i32| -> i32 {
                caller.data_mut().api_calls.push("birth_blob".to_string());
                let entity = caller.data().entity_id;
                let blob = caller.data().birth_blob.clone();
                if blob.is_empty() {
                    tracing::debug!(
                        entity_id = %entity.0,
                        "[birth_blob] 无出生数据包，返回 0"
                    );
                    return 0;
                }
                let n = blob.len();
                // 容量预检：cap 为负或小于 N 都算容量不足（cap 是 i32，负值转 usize
                // 会回绕成巨数，必须先判符号）
                let cap_ok = cap >= 0 && (cap as usize) >= n;
                if !cap_ok {
                    tracing::warn!(
                        entity_id = %entity.0,
                        need = n,
                        cap,
                        "[birth_blob] 缓冲区容量不足，未写任何字节（按 64KB 上限备缓冲一次必成）"
                    );
                    return -(n as i32);
                }
                // 原子写：write_bytes_to_caller 整段校验后才落笔，指针非法时返回 0
                // 不留半截数据
                let written = write_bytes_to_caller(&mut caller, ptr, &blob, n);
                if written == n {
                    tracing::debug!(
                        entity_id = %entity.0,
                        len = n,
                        "[birth_blob] 出生数据包已读回 caller memory"
                    );
                    n as i32
                } else {
                    tracing::warn!(
                        entity_id = %entity.0,
                        ptr,
                        need = n,
                        "[birth_blob] 写入指针非法，未写任何字节"
                    );
                    -(n as i32)
                }
            },
        )?;
        // 【D-018 退役】env.shoot（#034 第 12 个玩家 import）已随 #045 标准弹丸
        // 退役移除——世界不配发身体（#038 修路不跑车/#011 零预设）。远程发射
        // 一体式走 env.spawn 出生初速（第 11/12 参）+ 接触通道代码攻（on_contact
        // + attack_at）；分离式走 env.field_pulse。教学模板见 docs/newbie-first-thrower.md
        // （投掷器模板第一课）。
        // env.destroy(id_lo: i64, id_hi: i64) -> i32：销毁（门二开放，产权链 #015）。
        // EntityId(Uuid u128) 拆 lo/hi 两个 i64 传（WAT 侧负 i64 经 as u64 恢复位型）。
        // import 只排队：产权校验（只能销毁自家造物，owner 匹配）需要 world，
        // 在阶段二 process_destroys 判；destroy 不触发掠夺（不是击杀，宪法铁律）。
        // 自毁走 life()=0，本接口对自家实体同样放行（owner 恒匹配，语义等价无掠夺）。
        linker.func_wrap(
            "env",
            "destroy",
            |mut caller: Caller<SandboxData>, id_lo: i64, id_hi: i64| -> i32 {
                caller.data_mut().api_calls.push("destroy".to_string());
                let destroyer = caller.data().entity_id;
                // i64 -> u64 用 as 截断恢复位型（符号扩展陷阱：WAT 侧 -1 = u64::MAX），
                // 拼回 u128 再转 Uuid
                let id_u = ((id_hi as u64 as u128) << 64) | (id_lo as u64 as u128);
                let target = EntityId(Uuid::from_u128(id_u));
                caller
                    .data_mut()
                    .pending_destroys
                    .push(PendingDestroy { destroyer, target });
                tracing::info!(
                    destroyer = %destroyer.0,
                    target = %target.0,
                    "[destroy] 销毁请求已排队（阶段二产权校验）"
                );
                1
            },
        )?;
        // env.field_pulse(x: f64, y: f64, vx: f64, vy: f64, radius: f64) -> i32：
        // 力场脉冲（D-012 交付物 8，第 15 个玩家 import，分离式发射器）——以
        // (x,y) 为中心 radius 圆域内，给所有带速度实体一次性速度叠加 (vx,vy)。
        // 物理推不是规则层：velocity≡0 的实体推不动（速度叠加对零速无效果，
        // 推别人走碰撞/击退是 D-010 碰撞响应的活）。import 层只做零 world 校验：
        //   - 限流：每实体每 tick 最多 1 次（与 spawn 同款），多余拒绝
        //   - x/y/vx/vy 有限性（NaN/inf 拒绝）
        //   - 冲量模长 ≤ 世界极速（严格 > 拒绝恰等放行，与 spawn 初速同口径）
        //   - radius 钳 [0.1, 25.0]（超界钳回不拒绝——出生声明家族哲学）
        //   - (x,y) 在调用者快照 ±10 格内（SPAWN_RANGE 同款防远程）
        // 域内结算/被推者清单/极速钳制在阶段二（import 拿不到 world）。
        linker.func_wrap(
            "env",
            "field_pulse",
            move |mut caller: Caller<SandboxData>,
                  x: f64,
                  y: f64,
                  vx: f64,
                  vy: f64,
                  radius: f64|
                  -> i32 {
                caller.data_mut().api_calls.push("field_pulse".to_string());
                let source = caller.data().entity_id;
                // 限流：每实体每 tick 最多 1 次脉冲
                if !caller.data().pending_field_pulses.is_empty() {
                    tracing::warn!(
                        entity_id = %source.0,
                        "[field_pulse] 本 tick 已有脉冲请求，限流拒绝（1 次/tick）"
                    );
                    return 0;
                }
                // 有限性校验（五参数全部要有限）
                if !x.is_finite() || !y.is_finite() || !vx.is_finite() || !vy.is_finite() || !radius.is_finite() {
                    tracing::warn!(
                        entity_id = %source.0,
                        x, y, vx, vy, radius,
                        "[field_pulse] 参数非有限，拒绝"
                    );
                    return 0;
                }
                // 冲量模长 ≤ 世界极速（经装配 max_speed；严格 > 拒绝，恰等放行）
                let speed2 = vx * vx + vy * vy;
                if speed2 > max_speed * max_speed {
                    tracing::warn!(
                        entity_id = %source.0,
                        impulse = speed2.sqrt(),
                        max = max_speed,
                        "[field_pulse] 冲量模长超世界极速，拒绝（恰等放行）"
                    );
                    return 0;
                }
                // (x,y) 必须在调用者本拍快照 ±10 格内（SPAWN_RANGE 同款防远程）
                let (px, py) = caller.data().pos;
                if (x - px).abs() > SPAWN_RANGE || (y - py).abs() > SPAWN_RANGE {
                    tracing::warn!(
                        entity_id = %source.0,
                        caller_pos = ?(px, py),
                        center = ?(x, y),
                        range = SPAWN_RANGE,
                        "[field_pulse] 脉冲中心超出调用者 ±10 格，拒绝"
                    );
                    return 0;
                }
                // radius 钳 [0.1, 25.0]（超界钳回+警告非拒绝；非有限已被上拦）
                let clamped_radius = radius.clamp(FIELD_PULSE_RADIUS_MIN, FIELD_PULSE_RADIUS_MAX);
                if clamped_radius != radius {
                    tracing::warn!(
                        entity_id = %source.0,
                        requested = radius,
                        clamped = clamped_radius,
                        "[field_pulse] 脉冲半径校验钳制"
                    );
                }
                tracing::info!(
                    source = %source.0,
                    center = ?(x, y),
                    impulse = ?(vx, vy),
                    radius = clamped_radius,
                    "[field_pulse] 脉冲请求已排队（阶段二统一应用）"
                );
                caller
                    .data_mut()
                    .pending_field_pulses
                    .push(PendingFieldPulse {
                        source,
                        x,
                        y,
                        vx,
                        vy,
                        radius: clamped_radius,
                    });
                1
            },
        )?;
        // env.bridge_send(port: i32, ptr: i32, len: i32) -> i32：外桥发信
        //（D-001b 出线，第 16 个玩家 import；设计 2.1/2.6）。信件排队到
        // SandboxData.bridge_pending，实际 HTTP 出站在阶段二收集（host 工作线程）。
        // 判序（Ruling：超限检查在读内存之前、port 检查之后——多重违规时数据
        // 尺寸优先于路由有效性的可观测顺序，多重违规组合本身是病态调用）：
        //   4 = 桥未启用（快照开关 false，世界中途关桥/禁用环境）
        //   2 = payload 超限（读内存之前拦下，同 attack_at 护栏哲学）
        //   1 = 无此端点（port 不在登记快照）
        //   3 = 本拍已发（每沙箱每拍 1 发）或在途满（容量护栏）
        //   0 = 已排队（只表示"进了邮筒"，不表示送达/已回复——异步跨拍）
        // 内存读取同 attack_at：len<=0/越界/无 memory export -> 空 payload（空信
        // 也是信，照常排队）；本接口烧的 fuel=玩家自己的 on_tick 指令，无附加费。
        linker.func_wrap(
            "env",
            "bridge_send",
            |mut caller: Caller<SandboxData>, port: i32, ptr: i32, len: i32| -> i32 {
                caller.data_mut().api_calls.push("bridge_send".to_string());
                let entity = caller.data().entity_id;
                // 判序第一步：未启用恒拒（快照开关，host 拍首填充/复位）
                if !caller.data().bridge_enabled {
                    tracing::debug!(
                        entity_id = %entity.0,
                        "[bridge_send] 桥未启用，恒返 4"
                    );
                    return 4;
                }
                // 判序第二步：payload 超限（读内存之前拦下——防大拷贝，同 attack_at）
                if len > 0 && len as usize > caller.data().bridge_payload_max {
                    tracing::warn!(
                        entity_id = %entity.0,
                        actual_len = len,
                        max = caller.data().bridge_payload_max,
                        "[bridge_send] payload 超限，拒绝入队"
                    );
                    return 2;
                }
                // 判序第三步：port 不在登记快照（快照升序，二分查表）
                if caller
                    .data()
                    .bridge_ports
                    .binary_search(&port)
                    .is_err()
                {
                    tracing::info!(entity_id = %entity.0, port, "[bridge_send] 无此端点");
                    return 1;
                }
                // 判序第四步：每拍 1 发限流（与 spawn 同款节律——世界节律定天花板）
                if caller.data().bridge_sent_this_tick {
                    tracing::warn!(
                        entity_id = %entity.0,
                        "[bridge_send] 本拍已发，限流拒绝（1 发/tick）"
                    );
                    return 3;
                }
                // 判序第五步：在途满（容量护栏，防灌爆出站队列）
                if caller.data().bridge_inflight >= caller.data().bridge_inflight_max {
                    tracing::warn!(
                        entity_id = %entity.0,
                        inflight = caller.data().bridge_inflight,
                        max = caller.data().bridge_inflight_max,
                        "[bridge_send] 在途满，拒绝入队"
                    );
                    return 3;
                }
                // 全部前置通过：读内存 + 入队 + 在途/限流标记
                let payload = read_payload_from_caller(&mut caller, ptr, len);
                caller
                    .data_mut()
                    .bridge_pending
                    .push(PendingBridgeSend { port, payload: payload.clone() });
                let data = caller.data_mut();
                data.bridge_inflight += 1;
                data.bridge_sent_this_tick = true;
                tracing::info!(
                    entity_id = %entity.0,
                    port,
                    payload_len = payload.len(),
                    "[bridge_send] 信件已排队（阶段二统一出站）"
                );
                0
            },
        )?;
        // === D-011 世界内链路段五 import（command/report/send/storage_write/
        // === storage_read，第 17-21 个玩家 import）——全部照 bridge_send 闭包
        // === 风格：api_calls 计数 + 返回码 + 限流 flag + INFO 日志。与 bridge
        // === 管道分记账分管道（bridge_pending 管出世界，link_pending 管世界内）。
        // === import 闭包是 'static 拿不到 world：扇出/沿链/落账全在世界侧阶段
        // === 二，这里只做零 world 校验 + 快照判定 + 入队 ===
        // env.command(ptr: i32, len: i32) -> i32：下行广播（母体剑诀控万剑）。
        // 返回：正数=入队链数（拍首快照 link_children，实际扇出阶段二）/ 0=无活链
        // 或参数坏或限流。每拍 1 次；无视口限制（风筝线可放出屏幕）。
        linker.func_wrap(
            "env",
            "command",
            |mut caller: Caller<SandboxData>, ptr: i32, len: i32| -> i32 {
                caller.data_mut().api_calls.push("command".to_string());
                let entity = caller.data().entity_id;
                // payload 超限在读内存前拦下（同 attack_at/bridge_send 护栏哲学）
                if len > 0 && len as usize > caller.data().link_payload_max {
                    tracing::warn!(
                        entity_id = %entity.0,
                        actual_len = len,
                        max = caller.data().link_payload_max,
                        "[command] payload 超限，拒绝入队"
                    );
                    return 0;
                }
                // 每拍 1 次限流（同 bridge_send 节律）
                if caller.data().link_command_sent {
                    tracing::warn!(
                        entity_id = %entity.0,
                        "[command] 本拍已广播，限流拒绝（1 次/tick）"
                    );
                    return 0;
                }
                let children = caller.data().link_children;
                if children == 0 {
                    // 放生（无链）或断链（母体名下已无活链孩子）：无收信人
                    tracing::debug!(
                        entity_id = %entity.0,
                        "[command] 名下无活链造物，返回 0"
                    );
                    return 0;
                }
                let payload = read_payload_from_caller(&mut caller, ptr, len);
                caller.data_mut().link_pending.push(PendingLinkOut {
                    from: entity,
                    kind: LinkOutKind::Command,
                    payload: payload.clone(),
                });
                caller.data_mut().link_command_sent = true;
                tracing::info!(
                    entity_id = %entity.0,
                    children,
                    payload_len = payload.len(),
                    "[command] 下行广播已排队（阶段二按链账本扇出）"
                );
                children as i32
            },
        )?;
        // env.report(ptr: i32, len: i32) -> i32：上行沿链（侦察弹报坐标）。
        // 返回：1=入队 / 0=参数坏 / 2=无链或断链。每拍 1 次；无视口限制。
        linker.func_wrap(
            "env",
            "report",
            |mut caller: Caller<SandboxData>, ptr: i32, len: i32| -> i32 {
                caller.data_mut().api_calls.push("report".to_string());
                let entity = caller.data().entity_id;
                if len > 0 && len as usize > caller.data().link_payload_max {
                    tracing::warn!(
                        entity_id = %entity.0,
                        actual_len = len,
                        max = caller.data().link_payload_max,
                        "[report] payload 超限，拒绝入队"
                    );
                    return 0;
                }
                if caller.data().link_report_sent {
                    tracing::warn!(
                        entity_id = %entity.0,
                        "[report] 本拍已上报，限流拒绝（1 次/tick）"
                    );
                    return 0;
                }
                // 无链或断链（拍首快照 link_parent=None）：链没连着，投无可投
                let Some(parent) = caller.data().link_parent else {
                    tracing::debug!(
                        entity_id = %entity.0,
                        "[report] 无链或断链，返回 2"
                    );
                    return 2;
                };
                let payload = read_payload_from_caller(&mut caller, ptr, len);
                caller.data_mut().link_pending.push(PendingLinkOut {
                    from: entity,
                    kind: LinkOutKind::Report { to: parent },
                    payload: payload.clone(),
                });
                caller.data_mut().link_report_sent = true;
                tracing::info!(
                    entity_id = %entity.0,
                    parent = %parent.0,
                    payload_len = payload.len(),
                    "[report] 上行已排队（阶段二沿链投母体）"
                );
                1
            },
        )?;
        // env.send(x: f64, y: f64, ptr: i32, len: i32) -> i32：横向坐标寻址
        //（点寻址所见即所发，与 probe/attack_at 同哲学）。门槛=目标在发送方
        // 视口内（link_peers 快照=发送方 visible 列表成员——屏幕内无欺骗的
        // 发送版：看不见的发不到，想发先 scan）。返回：1=入队 / 0=参数坏
        //（坐标非有限/超限/限流）/ -1=坐标无实体或不视口内。每拍 1 次。
        linker.func_wrap(
            "env",
            "send",
            |mut caller: Caller<SandboxData>, x: f64, y: f64, ptr: i32, len: i32| -> i32 {
                caller.data_mut().api_calls.push("send".to_string());
                let entity = caller.data().entity_id;
                // 参数坏三连（先于视口判定——数据尺寸/有限性优先于路由有效性，
                // 同 bridge_send 判序哲学）
                if !x.is_finite() || !y.is_finite() {
                    tracing::warn!(
                        entity_id = %entity.0,
                        x,
                        y,
                        "[send] 坐标非有限，拒绝入队"
                    );
                    return 0;
                }
                if len > 0 && len as usize > caller.data().link_payload_max {
                    tracing::warn!(
                        entity_id = %entity.0,
                        actual_len = len,
                        max = caller.data().link_payload_max,
                        "[send] payload 超限，拒绝入队"
                    );
                    return 0;
                }
                if caller.data().link_send_sent {
                    tracing::warn!(
                        entity_id = %entity.0,
                        "[send] 本拍已发，限流拒绝（1 次/tick）"
                    );
                    return 0;
                }
                // 坐标寻址：视口内实体快照精确匹配（visible_at 给出什么就发什么，
                // bit 相等）；列表 EntityId 升序，同坐标多实体取 id 最小者（确定序）
                let Some(target) = caller
                    .data()
                    .link_peers
                    .iter()
                    .find(|p| p.pos == (x, y))
                    .map(|p| p.id)
                else {
                    tracing::info!(
                        entity_id = %entity.0,
                        x,
                        y,
                        "[send] 坐标无实体或不视口内，返回 -1"
                    );
                    return -1;
                };
                let payload = read_payload_from_caller(&mut caller, ptr, len);
                caller.data_mut().link_pending.push(PendingLinkOut {
                    from: entity,
                    kind: LinkOutKind::Send { to: target },
                    payload: payload.clone(),
                });
                caller.data_mut().link_send_sent = true;
                tracing::info!(
                    entity_id = %entity.0,
                    target = %target.0,
                    payload_len = payload.len(),
                    "[send] 横向信已排队（阶段二投递）"
                );
                1
            },
        )?;
        // env.storage_write(ptr: i32, len: i32) -> i32：全量覆盖自己的公开块
        //（移动硬盘写盘）。返回：N=写入字节 / -1=超容拒绝（不截断——硬盘写一半
        // =坏盘，诚实拒绝）/ 0=参数坏。每拍 1 次。写入双落：镜像同步（同拍读
        // 自己可见）+ 排队（阶段二落世界侧实体字段=真源，随实体入快照）。
        linker.func_wrap(
            "env",
            "storage_write",
            |mut caller: Caller<SandboxData>, ptr: i32, len: i32| -> i32 {
                caller.data_mut().api_calls.push("storage_write".to_string());
                let entity = caller.data().entity_id;
                // 负长度=参数坏（0=合法清空，全量覆盖语义下写 0 字节=清盘）
                if len < 0 {
                    tracing::warn!(
                        entity_id = %entity.0,
                        len,
                        "[storage_write] 负长度参数坏，拒绝"
                    );
                    return 0;
                }
                // 超容拒绝在读内存前拦下（防大拷贝；不截断=不偷改玩家数据）
                if len as usize > caller.data().storage_cap {
                    tracing::warn!(
                        entity_id = %entity.0,
                        actual_len = len,
                        cap = caller.data().storage_cap,
                        "[storage_write] 超容拒绝（不截断）"
                    );
                    return -1;
                }
                if caller.data().link_sw_sent {
                    tracing::warn!(
                        entity_id = %entity.0,
                        "[storage_write] 本拍已写盘，限流拒绝（1 次/tick）"
                    );
                    return 0;
                }
                let payload = read_payload_from_caller(&mut caller, ptr, len);
                let n = payload.len();
                caller.data_mut().link_storage_own = payload.clone();
                caller.data_mut().link_pending.push(PendingLinkOut {
                    from: entity,
                    kind: LinkOutKind::StorageWrite,
                    payload,
                });
                caller.data_mut().link_sw_sent = true;
                tracing::info!(
                    entity_id = %entity.0,
                    len = n,
                    "[storage_write] 公开块覆盖已排队（阶段二落世界侧账）"
                );
                n as i32
            },
        )?;
        // env.storage_read(x: f64, y: f64, ptr: i32, cap: i32) -> i32：贴身读
        // 对方公开块（读硬盘=物理凑过去读）。返回：N=读到字节（写入 memory
        // [ptr..ptr+N)）/ 0=cap 不足或指针坏 / -1=坐标无实体 / -2=够不着
        //（贴身判据 dist < r_self + r_target + 0.5 严格小于，melee 同款）/
        // -3=对方无存储（空块同返 -3——没写字的盘=世界视角无存储，代码事实
        // 只此一路，api.md 同口径）。每拍 1 次。
        linker.func_wrap(
            "env",
            "storage_read",
            |mut caller: Caller<SandboxData>,
             x: f64,
             y: f64,
             ptr: i32,
             cap: i32|
             -> i32 {
                caller.data_mut().api_calls.push("storage_read".to_string());
                let entity = caller.data().entity_id;
                // 参数坏：非有限坐标 / 负 cap（i32 负值转 usize 回绕，先判符号）
                if !x.is_finite() || !y.is_finite() || cap < 0 {
                    tracing::warn!(
                        entity_id = %entity.0,
                        x,
                        y,
                        cap,
                        "[storage_read] 参数坏（非有限坐标/负 cap）"
                    );
                    return 0;
                }
                if caller.data().link_sr_sent {
                    tracing::warn!(
                        entity_id = %entity.0,
                        "[storage_read] 本拍已读盘，限流拒绝（1 次/tick）"
                    );
                    return 0;
                }
                // 读自己：坐标=自己快照 pos，距离 0 天然成立，读镜像（storage_write
                // 已同步——同拍写后读可见新值）。block 统一 Arc 视图：peer 路是
                // host 拍首共享快照（引用计数拷贝零深拷贝），own 路镜像本就在
                // SandboxData 里就地包 Arc（每拍至多一次，读方自担）
                let (sx, sy) = caller.data().pos;
                let block: Arc<Vec<u8>> = if x == sx && y == sy {
                    Arc::new(caller.data().link_storage_own.clone())
                } else {
                    // 坐标寻址：视口内实体快照精确匹配（同 send 口径）
                    let Some(peer) = caller
                        .data()
                        .link_peers
                        .iter()
                        .find(|p| p.pos == (x, y))
                    else {
                        tracing::info!(
                            entity_id = %entity.0,
                            x,
                            y,
                            "[storage_read] 坐标无实体，返回 -1"
                        );
                        return -1;
                    };
                    // 贴身判据：dist < r_self + r_peer + 0.5 严格小于（melee 同款，
                    // 恰好等于判距=够不着）；读自己走上面的镜像分支不经过此判
                    let (px, py) = peer.pos;
                    let dx = px - sx;
                    let dy = py - sy;
                    let reach = caller.data().link_radius + peer.radius + STORAGE_READ_REACH;
                    if dx * dx + dy * dy >= reach * reach {
                        tracing::info!(
                            entity_id = %entity.0,
                            peer = %peer.id.0,
                            reach,
                            "[storage_read] 够不着（贴身判据外），返回 -2"
                        );
                        return -2;
                    }
                    peer.storage.clone()
                };
                // 空块=对方无存储（"没硬盘"）；cap 不足不写（原子语义）
                if block.is_empty() {
                    tracing::info!(
                        entity_id = %entity.0,
                        "[storage_read] 对方无存储（空块），返回 -3"
                    );
                    return -3;
                }
                let n = block.len();
                if (cap as usize) < n {
                    tracing::warn!(
                        entity_id = %entity.0,
                        need = n,
                        cap,
                        "[storage_read] cap 不足，未写任何字节"
                    );
                    return 0;
                }
                let written = write_bytes_to_caller(&mut caller, ptr, &block, n);
                if written != n {
                    tracing::warn!(
                        entity_id = %entity.0,
                        ptr,
                        need = n,
                        "[storage_read] 写入指针非法，未写任何字节"
                    );
                    return 0;
                }
                caller.data_mut().link_sr_sent = true;
                tracing::info!(
                    entity_id = %entity.0,
                    len = n,
                    "[storage_read] 贴身读取完成"
                );
                n as i32
            },
        )?;
        Ok(linker)
    }

    /// 构造决斗游魂 linker（D-005 刀0：内存映射直连的三个 import，世界只当硬件）
    ///
    /// 最小环境铁律：游魂窗口内可调的 import 只有这三个——
    /// - `env.duel_read(ptr, len, out_ptr) -> i32`：读守方内存 → 写进游魂自己的
    ///   memory out_ptr 处；返回实际字节数，越界/非法返回 -1
    /// - `env.duel_write(ptr, bytes_ptr, len) -> i32`：把游魂自己 memory
    ///   [bytes_ptr, +len) 的字节写进守方内存 ptr 处；返回写入字节数，越界返回 -1
    /// - `env.duel_round() -> i32`：当前窗口拍号（0 起，游魂做轮次策略用）
    ///
    /// 读就是读、写就是写，中间无世界代码、无语义解释（宪法二：世界只当硬件）。
    /// 走私其他 env.* import 的游魂模块在实例化时被拒（Err）。
    fn make_duel_linker(engine: &Engine) -> Result<Linker<DuelGhostData>> {
        let mut linker = Linker::new(engine);
        // env.duel_read(ptr, len, out_ptr) -> i32：守方内存 → 游魂内存的单向搬运
        linker.func_wrap(
            "env",
            "duel_read",
            |mut caller: Caller<DuelGhostData>, ptr: i32, len: i32, out_ptr: i32| -> i32 {
                let bridge = caller.data().defender;
                if ptr < 0 || len < 0 || out_ptr < 0 || bridge.is_null() {
                    tracing::debug!("[duel] duel_read 参数非法/数据线未挂载，返回 -1");
                    return -1;
                }
                // SAFETY：窗口契约（见 DuelGhostData.defender 文档）——指针仅在
                // call_duel_step 函数体存活期内非空且指向存活守方沙箱
                let defender = unsafe { &mut *bridge };
                // 守方 fuel 守卫（#046 账本铁律）：防御性快照恢复——当前 wasmtime
                // 版本 host 侧 Memory::read/write 是裸 slice 拷贝、不计量 fuel，
                // 保留守卫防未来计量策略变化。快照失败=操作不执行返回 -1（反向
                // 抹零会伤守方油箱），与 call_on_field 家族 Err 传播口径一致
                let prev_fuel = match defender.store.get_fuel() {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(error = %e, "[duel] 守方 fuel 快照失败，读作废返回 -1（不动守方油箱）");
                        return -1;
                    }
                };
                let read = duel_read_bytes(defender, ptr as u32, len as u32);
                let _ = defender.store.set_fuel(prev_fuel);
                match read {
                    Ok(bytes) => {
                        let n = bytes.len();
                        // 落笔游魂自己的内存（越界 -1：游魂自己量入为出，世界不 grow 不补偿）
                        match ghost_memory_write(&mut caller, out_ptr, &bytes) {
                            Ok(()) => {
                                caller.data_mut().bytes_read += n as u64;
                                tracing::debug!(
                                    ptr,
                                    len,
                                    out_ptr,
                                    "[duel] 游魂读守方内存 {n} 字节"
                                );
                                n as i32
                            }
                            Err(e) => {
                                tracing::debug!(ptr, out_ptr, error = %e, "[duel] 游魂内存落笔越界，读作废返回 -1");
                                -1
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!(ptr, len, error = %e, "[duel] 守方内存读越界/失败，返回 -1");
                        -1
                    }
                }
            },
        )?;
        // env.duel_write(ptr, bytes_ptr, len) -> i32：游魂内存 → 守方内存的单向搬运
        linker.func_wrap(
            "env",
            "duel_write",
            |mut caller: Caller<DuelGhostData>, ptr: i32, bytes_ptr: i32, len: i32| -> i32 {
                let bridge = caller.data().defender;
                if ptr < 0 || bytes_ptr < 0 || len < 0 || bridge.is_null() {
                    tracing::debug!("[duel] duel_write 参数非法/数据线未挂载，返回 -1");
                    return -1;
                }
                // 先从游魂自己的内存取字节（越界 -1，与读同款量入为出）
                let payload = match ghost_memory_read(&mut caller, bytes_ptr, len) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::debug!(bytes_ptr, len, error = %e, "[duel] 游魂内存取字节越界，写作废返回 -1");
                        return -1;
                    }
                };
                // SAFETY：同 duel_read 窗口契约
                let defender = unsafe { &mut *bridge };
                // 守方 fuel 守卫（同 duel_read：防御性快照恢复，host 读写当前
                // 不计量 fuel；快照失败=操作不执行返回 -1，不动守方油箱）
                let prev_fuel = match defender.store.get_fuel() {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(error = %e, "[duel] 守方 fuel 快照失败，写作废返回 -1（不动守方油箱）");
                        return -1;
                    }
                };
                let write = duel_write_bytes(defender, ptr as u32, &payload);
                let _ = defender.store.set_fuel(prev_fuel);
                match write {
                    Ok(()) => {
                        let n = payload.len();
                        caller.data_mut().bytes_written += n as u64;
                        tracing::debug!(ptr, len, "[duel] 游魂写守方内存 {n} 字节");
                        n as i32
                    }
                    Err(e) => {
                        tracing::debug!(ptr, len, error = %e, "[duel] 守方内存写越界/失败，返回 -1");
                        -1
                    }
                }
            },
        )?;
        // env.duel_round() -> i32：当前窗口拍号（世界唯一的"场外信息"，机械事实）
        linker.func_wrap("env", "duel_round", |caller: Caller<DuelGhostData>| -> i32 {
            caller.data().round as i32
        })?;
        tracing::debug!("[duel] 决斗游魂 linker 构造完成（最小环境：三原语 import）");
        Ok(linker)
    }

    /// 创建沙箱：编译 WASM + 实例化 + 设置 fuel + 绑定 entity_id
    ///
    /// `entity_id` 写入 SandboxData，使 import 函数 env.attack_at 能识别攻击者。
    pub fn create_sandbox(
        &self,
        wasm_bytes: &[u8],
        fuel: u64,
        entity_id: EntityId,
    ) -> Result<SandboxHandle> {
        let module = Module::new(&self.engine, wasm_bytes)?;
        let mut store = Store::new(&self.engine, SandboxData::new());
        store.data_mut().fuel_limit = fuel;
        store.data_mut().entity_id = entity_id;
        store.set_fuel(fuel)?;
        let instance = self.linker.instantiate(&mut store, &module)?;
        tracing::debug!(fuel, entity_id = %entity_id.0, "沙箱创建");
        Ok(SandboxHandle { store, instance })
    }

    /// 销毁沙箱（drop Store/Instance，线性内存随之释放）
    pub fn destroy_sandbox(&self, handle: SandboxHandle) {
        drop(handle);
        tracing::debug!("沙箱销毁");
    }

    /// 替换沙箱（路线A：先建新、成功才拆旧；失败时旧沙箱原样带回）
    ///
    /// P0 僵尸实体修复：旧实现先 drop(handle) 再 Module::new，payload 非法时编译失败
    /// -> 旧沙箱已销毁、新沙箱未建，实体失去沙箱变成僵尸（永不可被攻击、代码永不运行）。
    /// 改为先建后拆：编译/实例化失败时旧沙箱随 Err 原样返回，调用方放回映射表即可，
    /// "存活实体必有沙箱"不变量不破。
    /// fuel 语义：新沙箱配额由调用方传入（attack 路径传旧沙箱剩余量，防反复被攻击回满刷配额）。
    pub fn replace_sandbox(
        &self,
        mut handle: SandboxHandle,
        new_wasm_bytes: &[u8],
        fuel: u64,
    ) -> std::result::Result<SandboxHandle, (SandboxHandle, anyhow::Error)> {
        // 建新前读出 entity_id，传给新沙箱（攻击替换后仍是同一实体）
        let entity_id = handle.store.data().entity_id;
        match self.create_sandbox(new_wasm_bytes, fuel, entity_id) {
            Ok(new_handle) => {
                // 成功才拆旧。旧沙箱若还有排队未处理的攻击请求，随替换丢弃（warn 提示）。
                let stale = handle.store.data().pending_attacks.len();
                if stale > 0 {
                    tracing::warn!(
                        entity_id = %entity_id.0,
                        count = stale,
                        "旧沙箱仍有未处理攻击请求，随替换丢弃"
                    );
                }
                // #051 夺沙箱=夺全套：资产段随行（在拆旧前转移——失败路径根本
                // 进不了这里，旧沙箱随 Err 原样带回，资产自然不丢）
                let mut new_handle = new_handle;
                Self::replace_sandbox_assets(&mut handle, &mut new_handle);
                drop(handle);
                tracing::debug!(entity_id = %entity_id.0, "沙箱替换（路线A，先建后拆）");
                Ok(new_handle)
            }
            Err(e) => {
                tracing::debug!(
                    entity_id = %entity_id.0,
                    error = %e,
                    "沙箱替换失败，旧沙箱保留"
                );
                Err((handle, e))
            }
        }
    }

    /// 换血接线（#051 夺取语义：沙箱易主=资产随行）——旧沙箱资产段整体转移进
    /// 新沙箱（移动非复制：转移后旧沙箱资产段清空，防双重持有）。
    /// replace_sandbox 的 Ok 分支已内置调用（拆旧前转移，失败路径资产自然留在
    /// 旧沙箱随 Err 原样带回）；独立成方法供测试与未来装戴刀复用。
    pub fn replace_sandbox_assets(old: &mut SandboxHandle, new: &mut SandboxHandle) {
        let moved = std::mem::take(&mut old.store.data_mut().assets);
        let count = moved.len();
        new.store.data_mut().assets = moved;
        if count > 0 {
            tracing::debug!(count, "[skin] 沙箱资产段随行转移（夺沙箱=夺全套）");
        }
    }

    /// 更新 fuel_consumed（fuel_limit - get_fuel）
    fn update_fuel(&self, handle: &mut SandboxHandle) -> Result<()> {
        let remaining = handle.store.get_fuel()?;
        let limit = handle.store.data().fuel_limit;
        handle.store.data_mut().fuel_consumed = limit.saturating_sub(remaining);
        Ok(())
    }

    /// 调用 life() -> bool（i32 != 0 = true）
    pub fn call_life(&self, handle: &mut SandboxHandle) -> Result<bool> {
        let func = handle.instance.get_typed_func::<(), i32>(&mut handle.store, "life")?;
        let result = func.call(&mut handle.store, ())?;
        self.update_fuel(handle)?;
        tracing::debug!(life = result, "call life");
        Ok(result != 0)
    }

    /// 调用 on_tick()（MVP 空实现，无参数；没导出则跳过）
    pub fn call_on_tick(&self, handle: &mut SandboxHandle) -> Result<()> {
        if self.has_export(handle, "on_tick")? {
            let func = handle.instance.get_typed_func::<(), ()>(&mut handle.store, "on_tick")?;
            func.call(&mut handle.store, ())?;
            self.update_fuel(handle)?;
        }
        Ok(())
    }

    /// 调用 on_probe(request) -> Vec<u8>
    ///
    /// WASM 签名 i32->i32，request 前 4 字节（le）转 i32（不足补 0；完整 Vec 需内存传递，Phase 2）。
    /// 没导出 on_probe 时返回空 Vec（防御性，公共 API 自保）。
    pub fn call_on_probe(&self, handle: &mut SandboxHandle, request: &[u8]) -> Result<Vec<u8>> {
        if !self.has_export(handle, "on_probe")? {
            tracing::debug!("on_probe 未导出，返回空响应");
            return Ok(Vec::new());
        }
        let arg = bytes_to_i32(request);
        let func = handle.instance.get_typed_func::<i32, i32>(&mut handle.store, "on_probe")?;
        let result = func.call(&mut handle.store, arg)?;
        self.update_fuel(handle)?;
        tracing::debug!(arg, result, "call on_probe");
        Ok(result.to_le_bytes().to_vec())
    }

    /// 调用 on_attack(payload) -> bool（i32 != 0 = 拦截）
    ///
    /// 没导出 on_attack 时返回 false（不拦截，架构 4.2：直接替换）。
    /// payload 前 4 字节（le）转 i32（同 call_on_probe）。
    pub fn call_on_attack(&self, handle: &mut SandboxHandle, payload: &[u8]) -> Result<bool> {
        if !self.has_export(handle, "on_attack")? {
            tracing::debug!("on_attack 未导出，不拦截");
            return Ok(false);
        }
        let arg = bytes_to_i32(payload);
        let func = handle.instance.get_typed_func::<i32, i32>(&mut handle.store, "on_attack")?;
        let result = func.call(&mut handle.store, arg)?;
        self.update_fuel(handle)?;
        tracing::debug!(arg, result, "call on_attack");
        Ok(result != 0)
    }

    /// 检查实例是否导出某函数
    pub fn has_export(&self, handle: &mut SandboxHandle, name: &str) -> Result<bool> {
        Ok(handle.instance.get_func(&mut handle.store, name).is_some())
    }

    /// 调用 on_contact(other_x, other_y)（D-007 ②接触回调；host 侧独立预算包裹）
    ///
    /// 返回 `Ok(Some(fuel_used))` = 已调用（used 记在预算尺度）；
    /// `Ok(None)` = 未导出（no-op，与 on_attack 未导出同款防御）。
    /// 预算包裹（probe 同款三步）：存剩余量 -> set 预算 -> 调 -> 恢复；
    /// fuel_consumed 元数据按原尺度重算（包裹期间预算尺度算过假象，防污染）。
    pub fn call_on_contact(
        &self,
        handle: &mut SandboxHandle,
        other_x: f64,
        other_y: f64,
        budget: u64,
    ) -> Result<Option<u64>> {
        if !self.has_export(handle, "on_contact")? {
            tracing::debug!("[contact] on_contact 未导出，跳过");
            return Ok(None);
        }
        let limit = handle.store.data().fuel_limit;
        let prev_fuel = handle.store.get_fuel()?;
        if let Err(e) = handle.store.set_fuel(budget) {
            anyhow::bail!("[contact] 预算设置失败: {e}");
        }
        let func = handle
            .instance
            .get_typed_func::<(f64, f64), ()>(&mut handle.store, "on_contact")?;
        let call_result = func.call(&mut handle.store, (other_x, other_y));
        // 无论调用成败都恢复原 fuel（Err 上抛前不能弄丢目标剩余配额）
        let fuel_left = handle.store.get_fuel().unwrap_or(budget);
        let used = budget.saturating_sub(fuel_left);
        if let Err(e) = handle.store.set_fuel(prev_fuel) {
            tracing::warn!(error = %e, "[contact] fuel 恢复失败");
        }
        handle.store.data_mut().fuel_consumed = limit.saturating_sub(prev_fuel);
        call_result?;
        tracing::info!(other_x, other_y, fuel_used = used, "call on_contact");
        Ok(Some(used))
    }

    /// 调用 on_death()（D-007 ③遗嘱钩子；host 侧独立预算包裹，世界托管执行）
    ///
    /// 返回 `Ok(Some(fuel_used))` = 已调用；`Ok(None)` = 未导出（无遗嘱，现状行为）。
    /// 预算即遗嘱字数上限：燃料耗尽遗嘱写不完（世界不补偿不重试；wasm 内烧穿
    /// fuel 在 Windows 上是进程 abort，.cc-bugs #5 环境级问题，生产化前需升级
    /// wasmtime——本包裹只保证正常路径的预算记账与恢复）。
    pub fn call_on_death(&self, handle: &mut SandboxHandle, budget: u64) -> Result<Option<u64>> {
        if !self.has_export(handle, "on_death")? {
            tracing::debug!("[will] on_death 未导出，无遗嘱");
            return Ok(None);
        }
        let limit = handle.store.data().fuel_limit;
        let prev_fuel = handle.store.get_fuel()?;
        if let Err(e) = handle.store.set_fuel(budget) {
            anyhow::bail!("[will] 预算设置失败: {e}");
        }
        let func = handle
            .instance
            .get_typed_func::<(), ()>(&mut handle.store, "on_death")?;
        let call_result = func.call(&mut handle.store, ());
        let fuel_left = handle.store.get_fuel().unwrap_or(budget);
        let used = budget.saturating_sub(fuel_left);
        if let Err(e) = handle.store.set_fuel(prev_fuel) {
            tracing::warn!(error = %e, "[will] fuel 恢复失败");
        }
        handle.store.data_mut().fuel_consumed = limit.saturating_sub(prev_fuel);
        call_result?;
        tracing::info!(fuel_used = used, "call on_death");
        Ok(Some(used))
    }

    /// 调用 on_field(n_ptr, ids_cap, ids_ptr) -> i32（D-012 协议 v3：力场询问
    /// 应答制，host→沙箱调用，第 14 个玩家函数契约）
    ///
    /// 通信契约（api.md 载明）：host 把域内实体数 n（i32 LE）写进 `n_ptr`、
    /// 清单（EntityId 低 64 位，每实体 8 字节 LE，最多 min(n, FIELD_MAILBOX_CAP)
    /// 条）写进 `[ids_ptr, ids_ptr+8×写入数)`，然后把三个地址作为参数调 on_field。
    /// **场信箱 = 沙箱线性内存末尾 516 字节（世界保留，玩家勿占用）**——地址由
    /// host 选在内存顶端：玩家声明的内存多大，信箱就跟到多高，玩家数据段压不进。
    ///
    /// 返回值（场有表达权，世界有结算权）：
    /// - `Ok(Some(0))` = 维持出生声明（笨场几步完事）；
    /// - `Ok(Some(k))`（k≠0）= 本拍 drag_delta 应答覆盖值（k as f64）；
    /// - `Ok(None)` = 未导出 on_field / 内存太小放不下信箱（按出生声明结算，
    ///   no-op 防御，与 on_attack 未导出同款）；
    /// - `Err` = 调用失败（签名不匹配等）——调用方按出生声明结算（世界诚实，
    ///   不用坏数据）。
    ///
    /// 预算包裹（on_contact 同款三步）：存剩余量 -> set 预算 -> 调 -> 恢复；
    /// fuel_consumed 元数据按原尺度重算（包裹期间预算尺度是假象，防污染）。
    pub fn call_on_field(
        &self,
        handle: &mut SandboxHandle,
        ids: &[u64],
        budget: u64,
    ) -> Result<Option<i32>> {
        if !self.has_export(handle, "on_field")? {
            tracing::debug!("[field] on_field 未导出，按出生声明结算");
            return Ok(None);
        }
        let Some(memory) = handle.instance.get_memory(&mut handle.store, "memory") else {
            tracing::warn!("[field] 场源未导出 memory，无法投递域内清单，按出生声明结算");
            return Ok(None);
        };
        // 场信箱选址：线性内存末尾（4 字节 n + 64×8 字节清单 = 516 字节保留区）
        let reserve = 4 + FIELD_MAILBOX_CAP * 8;
        let mem_size = memory.data_size(&handle.store);
        if mem_size < reserve {
            tracing::warn!(
                mem_size,
                reserve,
                "[field] 线性内存小于场信箱保留区，按出生声明结算"
            );
            return Ok(None);
        }
        let ids_ptr = mem_size - FIELD_MAILBOX_CAP * 8;
        let n_ptr = ids_ptr - 4;
        let n = ids.len().min(FIELD_MAILBOX_CAP);
        // 截断显式警告（D-012 收尾刀 P1-2）：max_entities 可经 CODEWORLD_MAX_ENTITIES
        // 调大 >64，静默截断会让模块把"恰好 64"误读为"全部在场"——warn 一行让
        // 排障有迹可循（信箱容量推导是结构改动，非收尾刀范围）
        if ids.len() > FIELD_MAILBOX_CAP {
            tracing::warn!(
                entity_id = %handle.store.data().entity_id.0,
                ids_in_domain = ids.len(),
                cap = FIELD_MAILBOX_CAP,
                "[field] 域内实体数超场信箱容量，清单截断（模块只见前 64 条）"
            );
        }
        // 签名核验先于预算包裹（签名不匹配 Err 时原剩余量分毫不动）
        let func = handle.instance.get_typed_func::<(i32, i32, i32), i32>(
            &mut handle.store,
            "on_field",
        )?;
        // 剩余量快照在写信【前】——wasmtime 的 host 侧 memory.write 也会计量
        // fuel（实测约 4 步），那是世界投递清单的通信成本，不烧场源 tick 配额；
        // 预算包裹恢复到本快照=场只为自己 on_field 的计算付费
        let limit = handle.store.data().fuel_limit;
        let prev_fuel = handle.store.get_fuel()?;
        // 先写 n（i32 LE），再写清单（每实体 8 字节 LE，EntityId 低 64 位）
        memory.write(&mut handle.store, n_ptr, &(n as i32).to_le_bytes())?;
        for (i, v) in ids.iter().take(n).enumerate() {
            memory.write(&mut handle.store, ids_ptr + i * 8, &v.to_le_bytes())?;
        }
        // 预算包裹：set 预算 -> 调 -> 恢复（无论成败都恢复到写信前快照）
        if let Err(e) = handle.store.set_fuel(budget) {
            anyhow::bail!("[field] 预算设置失败: {e}");
        }
        let call_result = func.call(&mut handle.store, (n_ptr as i32, FIELD_MAILBOX_CAP as i32, ids_ptr as i32));
        let fuel_left = handle.store.get_fuel().unwrap_or(budget);
        let used = budget.saturating_sub(fuel_left);
        if let Err(e) = handle.store.set_fuel(prev_fuel) {
            tracing::warn!(error = %e, "[field] fuel 恢复失败");
        }
        handle.store.data_mut().fuel_consumed = limit.saturating_sub(prev_fuel);
        let answer = call_result?;
        tracing::info!(
            entity_id = %handle.store.data().entity_id.0,
            n,
            answer,
            fuel_used = used,
            "[field] call on_field（询问应答完成）"
        );
        Ok(Some(answer))
    }

    /// 调用 bridge_on_reply(port, ptr, len)（D-001b 外桥回信投递；host 侧独立
    /// 预算包裹，on_contact 同款三步）
    ///
    /// 回信写入沙箱内存 = **grow 分配**：取 memory 当前页数 old_pages，grow 出
    /// 所需页数，字节写在偏移 old_pages*65536 处（新增零页，永不覆盖玩家数据——
    /// 与 D-012 场信箱"线性内存末尾保留段"不冲突：信箱是世界保留区约定，回信是
    /// grow 追加区），ptr 即该偏移。
    /// 返回 `Ok(Some(fuel_used))` = 已投递（used 记在预算尺度）；
    /// `Ok(None)` = 未导出 bridge_on_reply（缺省=收不到回信，设计 2.6 零副作用）
    /// 或未导出 memory（字节无处落笔，投递作废不崩世界）。
    pub fn call_on_bridge_reply(
        &self,
        handle: &mut SandboxHandle,
        port: i32,
        payload: &[u8],
        budget: u64,
    ) -> Result<Option<u64>> {
        if !self.has_export(handle, "bridge_on_reply")? {
            tracing::debug!("[bridge] bridge_on_reply 未导出，回信零副作用丢弃");
            return Ok(None);
        }
        let Some(memory) = handle.instance.get_memory(&mut handle.store, "memory") else {
            tracing::warn!("[bridge] 未导出 memory，回信无处落笔，投递作废");
            return Ok(None);
        };
        // 签名核验先于 grow+写回（P2-A：坏签名导出时不留"字节已落内存但回调
        // 没跑"的半副作用），也先于预算包裹（签名不匹配 Err 时原剩余量分毫不动）
        let func = handle.instance.get_typed_func::<(i32, i32, i32), ()>(
            &mut handle.store,
            "bridge_on_reply",
        )?;
        // grow 分配：offset = 调 grow 前的内存字节数（新增零页永不覆盖玩家数据）。
        // 空回信 grow 0 页（offset 指向旧末尾也无妨——len=0 不读字节）。
        // grow 失败（超 64MB 上限）= 投递作废，不崩世界（诚实丢弃，世界不重试）。
        let old_bytes = memory.data_size(&handle.store);
        let delta_pages = payload.len().div_ceil(65536) as u64;
        if delta_pages > 0 {
            if let Err(e) = memory.grow(&mut handle.store, delta_pages) {
                tracing::warn!(
                    entity_id = %handle.store.data().entity_id.0,
                    error = %e,
                    delta_pages,
                    "[bridge] 内存 grow 失败，回信投递作废"
                );
                return Ok(None);
            }
        }
        let offset = old_bytes;
        if let Err(e) = memory.write(&mut handle.store, offset, payload) {
            tracing::warn!(error = %e, offset, "[bridge] 回信写入失败，投递作废");
            return Ok(None);
        }
        let limit = handle.store.data().fuel_limit;
        let prev_fuel = handle.store.get_fuel()?;
        if let Err(e) = handle.store.set_fuel(budget) {
            anyhow::bail!("[bridge] 预算设置失败: {e}");
        }
        let call_result = func.call(
            &mut handle.store,
            (port, offset as i32, payload.len() as i32),
        );
        let fuel_left = handle.store.get_fuel().unwrap_or(budget);
        let used = budget.saturating_sub(fuel_left);
        if let Err(e) = handle.store.set_fuel(prev_fuel) {
            tracing::warn!(error = %e, "[bridge] fuel 恢复失败");
        }
        handle.store.data_mut().fuel_consumed = limit.saturating_sub(prev_fuel);
        call_result?;
        tracing::info!(
            entity_id = %handle.store.data().entity_id.0,
            port,
            len = payload.len(),
            offset,
            fuel_used = used,
            "call bridge_on_reply"
        );
        Ok(Some(used))
    }

    /// 链消息通用投递器（D-011：on_command/on_report/on_message 三回调的全套
    /// 先例复刻，call_on_bridge_reply 同款六步）——签名核验先于 grow+写（P2-A：
    /// 坏签名导出不留"字节已落内存但回调没跑"的半副作用，也先于预算包裹）、
    /// grow 分配 offset=旧末尾（新增零页永不覆盖玩家数据）、probe_fuel 预算
    /// 包裹调（接收方为收信付费的上限是防守预算，不占主配额）、缺省导出
    /// Ok(None) 零副作用（信塞进无人收件箱，世界记账照结算不泄漏）。
    ///
    /// `with_from=false` 时回调签名 `(i32, i32)`（on_command——造物收母体剑诀，
    /// 无需知道是哪个母体：链连着呢）；`true` 时 `(i64, i32, i32)`（on_report/
    /// on_message，`from`=源 EntityId 低 64 位——母体多孩子区分靠它，field
    /// 信箱同款先例）。
    pub fn deliver_message(
        &self,
        handle: &mut SandboxHandle,
        export: &str,
        from: u64,
        with_from: bool,
        payload: &[u8],
        budget: u64,
    ) -> Result<Option<u64>> {
        if !self.has_export(handle, export)? {
            tracing::debug!(
                export,
                entity = %handle.store.data().entity_id.0,
                "[link] 缺省导出，消息零副作用丢弃"
            );
            return Ok(None);
        }
        let Some(memory) = handle.instance.get_memory(&mut handle.store, "memory") else {
            tracing::warn!(
                export,
                entity = %handle.store.data().entity_id.0,
                "[link] 未导出 memory，消息无处落笔，投递作废"
            );
            return Ok(None);
        };
        // 双签名核验（先于 grow+写+预算包裹——签名不匹配 Err 时原剩余量与内存
        // 分毫不动）
        let call_result = if with_from {
            let func = handle
                .instance
                .get_typed_func::<(i64, i32, i32), ()>(&mut handle.store, export)?;
            // grow 分配：offset = 调 grow 前的内存字节数（同 call_on_bridge_reply）
            let old_bytes = memory.data_size(&handle.store);
            let delta_pages = payload.len().div_ceil(65536) as u64;
            if delta_pages > 0 {
                if let Err(e) = memory.grow(&mut handle.store, delta_pages) {
                    tracing::warn!(
                        entity_id = %handle.store.data().entity_id.0,
                        error = %e,
                        delta_pages,
                        "[link] 内存 grow 失败，消息投递作废"
                    );
                    return Ok(None);
                }
            }
            if let Err(e) = memory.write(&mut handle.store, old_bytes, payload) {
                tracing::warn!(error = %e, offset = old_bytes, "[link] 消息写入失败，投递作废");
                return Ok(None);
            }
            let limit = handle.store.data().fuel_limit;
            let prev_fuel = handle.store.get_fuel()?;
            if let Err(e) = handle.store.set_fuel(budget) {
                anyhow::bail!("[link] 预算设置失败: {e}");
            }
            let r = func.call(&mut handle.store, (from as i64, old_bytes as i32, payload.len() as i32));
            let fuel_left = handle.store.get_fuel().unwrap_or(budget);
            let used = budget.saturating_sub(fuel_left);
            if let Err(e) = handle.store.set_fuel(prev_fuel) {
                tracing::warn!(error = %e, "[link] fuel 恢复失败");
            }
            handle.store.data_mut().fuel_consumed = limit.saturating_sub(prev_fuel);
            r.map(|_| used)
        } else {
            let func = handle
                .instance
                .get_typed_func::<(i32, i32), ()>(&mut handle.store, export)?;
            let old_bytes = memory.data_size(&handle.store);
            let delta_pages = payload.len().div_ceil(65536) as u64;
            if delta_pages > 0 {
                if let Err(e) = memory.grow(&mut handle.store, delta_pages) {
                    tracing::warn!(
                        entity_id = %handle.store.data().entity_id.0,
                        error = %e,
                        delta_pages,
                        "[link] 内存 grow 失败，消息投递作废"
                    );
                    return Ok(None);
                }
            }
            if let Err(e) = memory.write(&mut handle.store, old_bytes, payload) {
                tracing::warn!(error = %e, offset = old_bytes, "[link] 消息写入失败，投递作废");
                return Ok(None);
            }
            let limit = handle.store.data().fuel_limit;
            let prev_fuel = handle.store.get_fuel()?;
            if let Err(e) = handle.store.set_fuel(budget) {
                anyhow::bail!("[link] 预算设置失败: {e}");
            }
            let r = func.call(&mut handle.store, (old_bytes as i32, payload.len() as i32));
            let fuel_left = handle.store.get_fuel().unwrap_or(budget);
            let used = budget.saturating_sub(fuel_left);
            if let Err(e) = handle.store.set_fuel(prev_fuel) {
                tracing::warn!(error = %e, "[link] fuel 恢复失败");
            }
            handle.store.data_mut().fuel_consumed = limit.saturating_sub(prev_fuel);
            r.map(|_| used)
        };
        let used = call_result?;
        tracing::info!(
            entity_id = %handle.store.data().entity_id.0,
            export,
            from,
            len = payload.len(),
            offset_hint = "grow 末尾",
            fuel_used = used,
            "call 链消息投递"
        );
        Ok(Some(used))
    }

    /// 决斗读：读 defender 沙箱线性内存 [ptr, ptr+len) 到字节缓冲（D-005 交付2，
    /// "沙箱通道"的最小物理实现——世界只当硬件，读就是读）
    ///
    /// 守方未导出 memory / 越界 -> Err。host 直调入口（测试与未来世界侧工具用）；
    /// 游魂窗口内的实际搬运走 import 闭包（经 duel_read_bytes 同一硬件层）。
    pub fn duel_read(&self, defender: &mut SandboxHandle, ptr: u32, len: u32) -> Result<Vec<u8>> {
        duel_read_bytes(defender, ptr, len)
    }

    /// 决斗写：把字节写进 defender 沙箱线性内存 [ptr, ptr+len)（D-005 交付2，
    /// 写就是写，无中间世界代码、无语义解释）
    ///
    /// 守方未导出 memory / 越界 -> Err。
    pub fn duel_write(&self, defender: &mut SandboxHandle, ptr: u32, bytes: &[u8]) -> Result<()> {
        duel_write_bytes(defender, ptr, bytes)
    }

    /// 游魂窗口（D-005 交付3）：把攻方游魂模块实例化为临时执行体，给它独立
    /// fuel 预算（从 DuelSession.fuel_stake 扣，账本铁律 #046——烧的是攻方的钱），
    /// 在"能对守方内存读写"的环境里跑约定导出 `on_duel`（() -> i32，返回值即投票：
    /// 1=宣布完成会话收尾，其余=继续）。
    ///
    /// 最小实例化策略（刀0 拍板）：每拍临时实例化（Module::new + instantiate +
    /// 跑一步 + drop）——pooling/实例池化是拍速升级议题的既定坑，刀0 不碰，
    /// 每拍重编译的代价诚实接受（design-duel 弹药库两坑记档）。
    ///
    /// 崩溃三路全走 Err（调用方按崩溃死关闭会话，世界只看见崩溃）：
    /// 模块编译失败 / 实例化失败（含走私多余 import）/ on_duel 导出缺失 / trap。
    /// budget=0 拒绝开窗（Windows 下 wasm 零 fuel 执行是进程 abort，.cc-bugs #5）。
    /// 红线：窗口内烧穿 budget 的 out-of-fuel trap 在 Windows 下同样是进程 abort
    /// 非 Err（.cc-bugs #5 既有环境限制，暴露面与玩家 on_tick 死循环等价，非新增）——
    /// 预算由 bin 侧油尽驱逐先行兜底（fuel_stake < duel_step_fuel 不开窗）。
    pub fn call_duel_step(
        &self,
        attacker_module: &[u8],
        defender: &mut SandboxHandle,
        budget: u64,
        round: u32,
    ) -> Result<DuelStepOutcome> {
        if budget == 0 {
            anyhow::bail!("[duel] 游魂窗口预算为 0，拒绝开窗（零 fuel 执行是进程 abort 红线）");
        }
        // 每拍临时实例化：编译（不受 fuel 约束）——失败=游魂崩溃
        let module = Module::new(&self.engine, attacker_module)
            .context("[duel] 游魂模块编译失败（崩溃死）")?;
        // 游魂 store：数据线插头（守方指针）+ 窗口拍号；指针在本函数体存活期内有效
        let mut ghost = Store::new(
            &self.engine,
            DuelGhostData {
                defender: defender as *mut SandboxHandle,
                round,
                bytes_read: 0,
                bytes_written: 0,
            },
        );
        ghost.set_fuel(budget)?;
        // 实例化（最小 linker：走私多余 import 在此被拒）——失败=游魂崩溃
        let instance = self
            .duel_linker
            .instantiate(&mut ghost, &module)
            .context("[duel] 游魂实例化失败（崩溃死：垃圾模块/走私 import）")?;
        // 约定导出 on_duel——缺失=游魂崩溃（世界不给无意识的游魂开窗）
        let func = instance
            .get_typed_func::<(), i32>(&mut ghost, "on_duel")
            .context("[duel] 游魂缺 on_duel 导出（崩溃死）")?;
        // 跑一步：投票 1=Done 收尾，其余=Continue；trap=崩溃
        let call_result = func.call(&mut ghost, ());
        let fuel_left = ghost.get_fuel().unwrap_or(0);
        let fuel_used = budget.saturating_sub(fuel_left);
        match call_result {
            Ok(vote) => {
                tracing::debug!(
                    round,
                    budget,
                    fuel_used,
                    "[duel] 游魂窗口完成（fuel_used 记 debug，账本烧攻方赌注）"
                );
                Ok(DuelStepOutcome {
                    fuel_used,
                    outcome: if vote == 1 { DuelVerdict::Done } else { DuelVerdict::Continue },
                })
            }
            Err(e) => anyhow::bail!("[duel] 游魂 on_duel trap（崩溃死）：{e}（本窗已烧 {fuel_used}）"),
        }
    }

    /// 引擎引用
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

/// 决斗读硬件层（D-005 交付2）：读守方线性内存 [ptr, ptr+len)
///
/// 守方未导出 memory / 越界 -> Err。import 闭包与 manager.duel_read 共用本层
/// （闭包是 'static 拿不到 manager，直接调模块级函数，同一份实现零重复）。
fn duel_read_bytes(defender: &mut SandboxHandle, ptr: u32, len: u32) -> Result<Vec<u8>> {
    let memory = defender
        .instance
        .get_memory(&mut defender.store, "memory")
        .ok_or_else(|| anyhow::anyhow!("[duel] 守方未导出 memory，无可映射内存"))?;
    let size = memory.data_size(&defender.store);
    let start = ptr as usize;
    // checked_add：32 位平台 ptr+len 回绕防御（与 read_payload_from_caller 同款）
    let end = start
        .checked_add(len as usize)
        .ok_or_else(|| anyhow::anyhow!("[duel] 读地址回绕 ptr={ptr} len={len}"))?;
    if end > size {
        anyhow::bail!("[duel] 决斗读越界：[{ptr}, {}) 超出守方线性内存 {size}", ptr as usize + len as usize);
    }
    let mut buf = vec![0u8; len as usize];
    memory.read(&defender.store, start, &mut buf)?;
    tracing::debug!(ptr, len, "[duel] 决斗读硬件层完成（守方内存 -> 字节缓冲）");
    Ok(buf)
}

/// 决斗写硬件层（D-005 交付2）：把 bytes 写进守方线性内存 [ptr, ..)
///
/// 守方未导出 memory / 越界 -> Err。
fn duel_write_bytes(defender: &mut SandboxHandle, ptr: u32, bytes: &[u8]) -> Result<()> {
    let memory = defender
        .instance
        .get_memory(&mut defender.store, "memory")
        .ok_or_else(|| anyhow::anyhow!("[duel] 守方未导出 memory，无可映射内存"))?;
    let size = memory.data_size(&defender.store);
    let start = ptr as usize;
    let end = start
        .checked_add(bytes.len())
        .ok_or_else(|| anyhow::anyhow!("[duel] 写地址回绕 ptr={ptr} len={}", bytes.len()))?;
    if end > size {
        anyhow::bail!("[duel] 决斗写越界：[{ptr}, {}) 超出守方线性内存 {size}", ptr as usize + bytes.len());
    }
    memory.write(&mut defender.store, start, bytes)?;
    tracing::debug!(ptr, len = bytes.len(), "[duel] 决斗写硬件层完成（字节缓冲 -> 守方内存）");
    Ok(())
}

/// 游魂自己的内存读（env.duel_write 取源字节用）：[ptr, ptr+len)，越界 Err
fn ghost_memory_read(caller: &mut Caller<DuelGhostData>, ptr: i32, len: i32) -> Result<Vec<u8>> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow::anyhow!("[duel] 游魂未导出 memory"))?;
    let data = memory.data(&*caller);
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or_else(|| anyhow::anyhow!("[duel] 游魂读地址回绕 ptr={ptr} len={len}"))?;
    if end > data.len() {
        anyhow::bail!("[duel] 游魂读越界：[{ptr}, {end}) 超出自身线性内存 {}", data.len());
    }
    Ok(data[start..end].to_vec())
}

/// 游魂自己的内存写（env.duel_read 落笔用）：越界 Err（世界不 grow 不补偿）
fn ghost_memory_write(
    caller: &mut Caller<DuelGhostData>,
    ptr: i32,
    bytes: &[u8],
) -> Result<()> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow::anyhow!("[duel] 游魂未导出 memory"))?;
    let data_len = memory.data(&*caller).len();
    let start = ptr as usize;
    let end = start
        .checked_add(bytes.len())
        .ok_or_else(|| anyhow::anyhow!("[duel] 游魂写地址回绕 ptr={ptr}"))?;
    if end > data_len {
        anyhow::bail!("[duel] 游魂写越界：[{ptr}, {end}) 超出自身线性内存 {data_len}");
    }
    memory.data_mut(&mut *caller)[start..end].copy_from_slice(bytes);
    Ok(())
}

/// &[u8] 前 4 字节（le）转 i32（不足补 0；WASM i32 限制，完整 Vec 需内存传递 Phase 2）
fn bytes_to_i32(bytes: &[u8]) -> i32 {
    let mut arr = [0u8; 4];
    let len = bytes.len().min(4);
    arr[..len].copy_from_slice(&bytes[..len]);
    i32::from_le_bytes(arr)
}

/// 从 caller 的 memory export 读取 payload 字节（架构 4.2：attack_at 的 payload 参数）
///
/// 模块未导出 memory / ptr<0 / len<=0 / 越界 -> 空 Vec。
/// 空 payload 由阶段二 process_attacks 用 DEFAULT_KILL_WASM（默认击杀）。
fn read_payload_from_caller(caller: &mut Caller<SandboxData>, ptr: i32, len: i32) -> Vec<u8> {
    if ptr < 0 || len <= 0 {
        return Vec::new();
    }
    let Some(memory) = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    else {
        tracing::debug!("[read_payload] 模块未导出 memory，payload 为空");
        return Vec::new();
    };
    let data = memory.data(&*caller);
    let start = ptr as usize;
    // checked_add：32 位平台上 start+len 可能回绕成小值绕过越界检查（防御性修复）
    let Some(end) = start.checked_add(len as usize) else {
        tracing::warn!(
            ptr,
            len,
            "[read_payload] payload 读取地址回绕，置空"
        );
        return Vec::new();
    };
    if end > data.len() {
        tracing::warn!(
            ptr,
            len,
            mem_len = data.len(),
            "[read_payload] payload 读取越界，置空"
        );
        return Vec::new();
    }
    data[start..end].to_vec()
}

/// 检查向 caller 的 memory export 写入一个 f64 是否合法（只查不写）
///
/// env.scan 的双指针原子预检用：两个指针全部合法才落笔，避免半写入副作用。
fn can_write_f64_to_caller(caller: &mut Caller<SandboxData>, ptr: i32) -> bool {
    if ptr < 0 {
        return false;
    }
    let Some(memory) = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    else {
        return false;
    };
    let Some(end) = (ptr as usize).checked_add(8) else {
        return false;
    };
    end <= memory.data(&*caller).len()
}

/// 向 caller 的 memory export 写入一个 f64（8字节小端）（env.scan 的敌人坐标输出）
///
/// 模块未导出 memory / ptr<0 / ptr+8 越界 -> false（写入失败，调用方返回 0 作废扫描）。
fn write_f64_to_caller(caller: &mut Caller<SandboxData>, ptr: i32, value: f64) -> bool {
    if ptr < 0 {
        return false;
    }
    let Some(memory) = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    else {
        tracing::debug!("[write_f64] 模块未导出 memory，写入失败");
        return false;
    };
    let start = ptr as usize;
    // checked_add：防 32 位回绕绕过越界检查（与 read_payload 同款防御）
    let Some(end) = start.checked_add(8) else {
        tracing::warn!(ptr, "[write_f64] 写入地址回绕，写入失败");
        return false;
    };
    if end > memory.data(&*caller).len() {
        tracing::warn!(ptr, "[write_f64] 写入越界，写入失败");
        return false;
    }
    memory.data_mut(&mut *caller)[start..end].copy_from_slice(&value.to_le_bytes());
    true
}

/// 向 caller 的 memory export 写入字节切片（env.probe 的 response+体徵输出）
///
/// 模块未导出 memory / ptr<0 / cap=0 / 写入范围越界 -> 0（不写，防半截数据）。
/// bytes.len() > cap 时只写前 cap 字节（超容量截断），返回实际写入字节数。
fn write_bytes_to_caller(
    caller: &mut Caller<SandboxData>,
    ptr: i32,
    bytes: &[u8],
    cap: usize,
) -> usize {
    if ptr < 0 || cap == 0 {
        return 0;
    }
    let Some(memory) = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    else {
        tracing::debug!("[write_bytes] 模块未导出 memory，写入失败");
        return 0;
    };
    let start = ptr as usize;
    let want = bytes.len().min(cap);
    let Some(end) = start.checked_add(want) else {
        tracing::warn!(ptr, want, "[write_bytes] 写入地址回绕，写入失败");
        return 0;
    };
    if end > memory.data(&*caller).len() {
        tracing::warn!(ptr, want, mem_len = memory.data(&*caller).len(), "[write_bytes] 写入越界，写入失败");
        return 0;
    }
    memory.data_mut(&mut *caller)[start..end].copy_from_slice(&bytes[..want]);
    want
}

/// 从 caller 的 memory export 读取 NUL 结尾字符串（env.spawn 的 glyph/color 参数）
///
/// 从 ptr 起扫描到首个 NUL（最多扫 max_len 字节，防无界扫描线性内存），
/// UTF-8 解码失败 / ptr<0 / 无 memory / max_len 内无 NUL -> None。
fn read_cstring_from_caller(
    caller: &mut Caller<SandboxData>,
    ptr: i32,
    max_len: usize,
) -> Option<String> {
    if ptr < 0 {
        return None;
    }
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())?;
    let data = memory.data(&*caller);
    let start = ptr as usize;
    let window = data.get(start..)?.get(..max_len)?;
    let nul = window.iter().position(|&b| b == 0)?;
    String::from_utf8(window[..nul].to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::EntityId;
    use uuid::Uuid;

    /// 测试用固定 EntityId（沙箱需绑定 entity_id 供 import 函数识别攻击者）
    fn id(u: u128) -> EntityId {
        EntityId(Uuid::from_u128(u))
    }

    const LIFE_TRUE_WAT: &str = r#"(module (func (export "life") (result i32) i32.const 1))"#;
    const LIFE_FALSE_WAT: &str = r#"(module (func (export "life") (result i32) i32.const 0))"#;
    const PROBE_WAT: &str = r#"(module
        (func (export "on_probe") (param i32) (result i32)
          local.get 0 i32.const 1 i32.add))"#;
    const ATTACK_INTERCEPT_WAT: &str =
        r#"(module (func (export "on_attack") (param i32) (result i32) i32.const 1))"#;

    fn wasm(wat_text: &str) -> Vec<u8> {
        wat::parse_bytes(wat_text.as_bytes()).unwrap().into_owned()
    }

    #[test]
    fn new_creates_manager() {
        let mgr = SandboxManager::new().unwrap();
        let _ = mgr.engine();
    }

    #[test]
    fn create_sandbox_returns_handle() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(1)).unwrap();
        assert!(mgr.call_life(&mut handle).unwrap());
    }

    #[test]
    fn call_life_returns_false_for_zero() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_FALSE_WAT), 1000, id(1)).unwrap();
        assert!(!mgr.call_life(&mut handle).unwrap());
    }

    #[test]
    fn has_export_checks_function() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(1)).unwrap();
        assert!(mgr.has_export(&mut handle, "life").unwrap());
        assert!(!mgr.has_export(&mut handle, "nonexistent").unwrap());
    }

    #[test]
    fn call_on_probe_returns_response() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(PROBE_WAT), 1000, id(1)).unwrap();
        // on_probe(42) -> 43
        let response = mgr.call_on_probe(&mut handle, &[42]).unwrap();
        let val = i32::from_le_bytes(response.as_slice().try_into().unwrap());
        assert_eq!(val, 43);
    }

    #[test]
    fn call_on_attack_returns_intercept() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(ATTACK_INTERCEPT_WAT), 1000, id(1)).unwrap();
        assert!(mgr.call_on_attack(&mut handle, &[1]).unwrap()); // 拦截
    }

    // === D-007 三兄弟：call_on_contact / call_on_death（host 侧独立预算包裹）===

    /// 接触记录模块：on_contact 把对方坐标存进两个 f64 global + 调用计数
    ///（get_gx/get_gy/get_calls 观测导出，坐标值断言用）
    const CONTACT_RECORDER_WAT: &str = r#"(module
  (global $gx (mut f64) (f64.const -999))
  (global $gy (mut f64) (f64.const -999))
  (global $calls (mut i32) (i32.const 0))
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_contact") (param $ox f64) (param $oy f64)
    local.get $ox global.set $gx
    local.get $oy global.set $gy
    global.get $calls i32.const 1 i32.add global.set $calls)
  (func (export "get_gx") (result f64) global.get $gx)
  (func (export "get_gy") (result f64) global.get $gy)
  (func (export "get_calls") (result i32) global.get $calls))"#;

    /// 遗嘱记录模块：on_death 把标记置 1（get_will 观测导出，触发断言用）
    const WILL_RECORDER_WAT: &str = r#"(module
  (global $will (mut i32) (i32.const 0))
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_death") global.get $will i32.const 1 i32.add global.set $will)
  (func (export "get_will") (result i32) global.get $will))"#;

    #[test]
    fn call_on_contact_passes_opposite_coords() {
        // D-007 ②：on_contact(other_x, other_y) 收到对方坐标（f64,f64 多参元组直传），
        // WAT global 记录后观测导出断言值正确；返回 Some(used)=已调用
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(CONTACT_RECORDER_WAT), 100_000, id(30))
            .unwrap();
        let used = mgr
            .call_on_contact(&mut handle, 3.5, -2.25, 10_000)
            .unwrap()
            .expect("导出了 on_contact 应返回 Some(used)");
        assert!(used > 0 && used <= 10_000, "消耗应记在预算尺度内（used={used}）");
        assert_eq!(call_f64_export(&mut handle, "get_gx"), 3.5, "对方 x 应透传");
        assert_eq!(call_f64_export(&mut handle, "get_gy"), -2.25, "对方 y 应透传");
        assert_eq!(call_i32_export(&mut handle, "get_calls"), 1, "调用一次计数 1");
    }

    #[test]
    fn call_on_contact_without_export_is_noop() {
        // 负面路径：不导出 on_contact -> Ok(None) 不 Err（与 on_attack 未导出同款防御）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(31)).unwrap();
        assert!(
            mgr.call_on_contact(&mut handle, 1.0, 2.0, 10_000)
                .unwrap()
                .is_none(),
            "未导出应返回 None（no-op）"
        );
    }

    #[test]
    fn call_on_contact_restores_fuel_and_recomputes_consumed() {
        // 燃料不污染（probe 同款）：包裹前 fuel=55_555 -> 调用后恢复 55_555；
        // fuel_consumed 元数据按原尺度重算（包裹期间预算尺度算过假象）。
        // 注：不构造 wasm 内烧穿预算的场景——wasmtime 25 + Windows 下 fuel 耗尽
        // trap = 进程 abort（.cc-bugs #5 红线），预算中断由 host 侧 set_fuel
        // 包裹逻辑保证，测试只验证记账与恢复。
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(CONTACT_RECORDER_WAT), 100_000, id(32))
            .unwrap();
        handle.store.set_fuel(55_555).unwrap();
        mgr.call_on_contact(&mut handle, 7.0, 8.0, 10_000).unwrap();
        assert_eq!(
            handle.store.get_fuel().unwrap(),
            55_555,
            "调用后 fuel 应恢复原值（接触通知不扣手续费）"
        );
        assert_eq!(
            handle.store.data().fuel_consumed,
            100_000 - 55_555,
            "fuel_consumed 应按原尺度重算，不被包裹污染"
        );
    }

    #[test]
    fn call_on_death_invokes_export_and_returns_fuel_used() {
        // D-007 ③：on_death 无参被调（WAT global 标记置位），返回 Some(used)
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(WILL_RECORDER_WAT), 100_000, id(33))
            .unwrap();
        let used = mgr
            .call_on_death(&mut handle, 200_000)
            .unwrap()
            .expect("导出了 on_death 应返回 Some(used)");
        assert!(used > 0 && used <= 200_000, "消耗应记在预算尺度内（used={used}）");
        assert_eq!(call_i32_export(&mut handle, "get_will"), 1, "遗嘱钩子应被调用");
    }

    #[test]
    fn call_on_death_without_export_is_noop() {
        // 负面路径：不导出 on_death -> Ok(None)（现状行为，零回归：无遗嘱=无事发生）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(34)).unwrap();
        assert!(
            mgr.call_on_death(&mut handle, 200_000).unwrap().is_none(),
            "未导出应返回 None（无遗嘱）"
        );
    }

    #[test]
    fn replace_sandbox_creates_new() {
        let mgr = SandboxManager::new().unwrap();
        let handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(1)).unwrap();
        let mut handle = mgr.replace_sandbox(handle, &wasm(LIFE_FALSE_WAT), 1000).unwrap();
        assert!(!mgr.call_life(&mut handle).unwrap()); // life()=0
    }

    #[test]
    fn destroy_sandbox_drops_handle() {
        let mgr = SandboxManager::new().unwrap();
        let handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(1)).unwrap();
        mgr.destroy_sandbox(handle); // 不崩即可
    }

    #[test]
    fn call_on_tick_skips_if_no_export() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(1)).unwrap();
        // LIFE_TRUE_WAT 没有 on_tick，call_on_tick 不应报错
        assert!(mgr.call_on_tick(&mut handle).is_ok());
    }

    // === env.bridge_send import（D-001b 外桥出线：发信排队）===

    /// 发信测试模块：导出 memory + data 段 "WHERE?"（偏移 0，6 字节）+ send0 导出
    ///（直通 bridge_send 三参，测试侧经 get_typed_func 直调并取返回码）
    const BRIDGE_SENDER_WAT: &str = r#"(module
  (import "env" "bridge_send" (func $bridge_send (param i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "WHERE?")
  (func (export "send0") (param $port i32) (param $ptr i32) (param $len i32) (result i32)
    local.get $port local.get $ptr local.get $len call $bridge_send)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 拍首快照预置 helper：模拟 host fill（bridge_enabled/ports/上限装配）
    fn enable_bridge(
        handle: &mut SandboxHandle,
        ports: &[i32],
        payload_max: usize,
        inflight_max: u32,
    ) {
        let d = handle.store.data_mut();
        d.bridge_enabled = true;
        d.bridge_ports = ports.to_vec();
        d.bridge_payload_max = payload_max;
        d.bridge_inflight_max = inflight_max;
    }

    fn bridge_send_call(
        mgr: &SandboxManager,
        handle: &mut SandboxHandle,
        port: i32,
        ptr: i32,
        len: i32,
    ) -> i32 {
        mgr.call_on_tick(handle).unwrap(); // 预热（无 on_tick 导出即 no-op）
        let func = handle
            .instance
            .get_typed_func::<(i32, i32, i32), i32>(&mut handle.store, "send0")
            .unwrap();
        func.call(&mut handle.store, (port, ptr, len)).unwrap()
    }

    #[test]
    fn bridge_send_disabled_returns_4() {
        // 返回码 4：bridge_enabled=false（SandboxData 默认）——桥未启用恒拒
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(BRIDGE_SENDER_WAT), 10_000, id(70)).unwrap();
        assert_eq!(bridge_send_call(&mgr, &mut handle, 1, 0, 6), 4);
        assert!(handle.store.data().bridge_pending.is_empty(), "拒绝不入队");
    }

    #[test]
    fn bridge_send_unknown_port_returns_1() {
        // 返回码 1：已启用但 port 不在登记快照
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(BRIDGE_SENDER_WAT), 10_000, id(71)).unwrap();
        enable_bridge(&mut handle, &[1, 2], 8192, 4);
        assert_eq!(bridge_send_call(&mgr, &mut handle, 9, 0, 6), 1);
        assert!(handle.store.data().bridge_pending.is_empty(), "拒绝不入队");
    }

    #[test]
    fn bridge_send_oversize_returns_2_without_reading_memory() {
        // 返回码 2：len 超 payload 上限——读内存之前拦下（不入队不拷贝）；
        // 越界读地址也安全（超限分支先于内存访问）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(BRIDGE_SENDER_WAT), 10_000, id(72)).unwrap();
        enable_bridge(&mut handle, &[1], 4, 4); // 上限 4 字节，"WHERE?" 6 字节
        assert_eq!(bridge_send_call(&mgr, &mut handle, 1, 0, 6), 2);
        // 越界地址 + 超限长度同码（超限先拦，不到读内存那步）
        assert_eq!(bridge_send_call(&mgr, &mut handle, 1, i32::MAX, 99), 2);
        assert!(handle.store.data().bridge_pending.is_empty(), "拒绝不入队");
    }

    #[test]
    fn bridge_send_twice_this_tick_returns_3() {
        // 返回码 3（限流）：本拍已成功发过一封——每沙箱每拍 1 发（设计 2.5）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(BRIDGE_SENDER_WAT), 10_000, id(73)).unwrap();
        enable_bridge(&mut handle, &[1], 8192, 4);
        assert_eq!(bridge_send_call(&mgr, &mut handle, 1, 0, 6), 0, "首封应成功");
        assert_eq!(bridge_send_call(&mgr, &mut handle, 1, 0, 6), 3, "第二封限流拒绝");
        assert_eq!(handle.store.data().bridge_pending.len(), 1, "只有首封入队");
        assert_eq!(handle.store.data().bridge_inflight, 1, "在途 +1 仅首封");
    }

    #[test]
    fn bridge_send_inflight_full_returns_3() {
        // 返回码 3（容量）：在途计数已达上限
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(BRIDGE_SENDER_WAT), 10_000, id(74)).unwrap();
        enable_bridge(&mut handle, &[1], 8192, 4);
        handle.store.data_mut().bridge_inflight = 4; // 预置已满
        assert_eq!(bridge_send_call(&mgr, &mut handle, 1, 0, 6), 3);
        assert!(handle.store.data().bridge_pending.is_empty(), "拒绝不入队");
    }

    #[test]
    fn bridge_send_success_queues_letter_with_memory_bytes() {
        // 成功码 0：信件入队（payload 从 memory [ptr..ptr+len] 读出）+ 在途 +1
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(BRIDGE_SENDER_WAT), 10_000, id(75)).unwrap();
        enable_bridge(&mut handle, &[1], 8192, 4);
        assert_eq!(bridge_send_call(&mgr, &mut handle, 1, 0, 6), 0);
        let pending = &handle.store.data().bridge_pending;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].port, 1);
        assert_eq!(pending[0].payload, b"WHERE?".to_vec(), "应从 memory 读出信体");
        assert_eq!(handle.store.data().bridge_inflight, 1, "成功入队在途 +1");
    }

    #[test]
    fn bridge_send_bad_memory_reads_empty_payload() {
        // 空 payload 路径（attack_at 同款）：len<=0 / 无 memory / 越界 -> 空 payload
        // 照常入队（世界只送信不拆信，空信也是信）。每拍 1 发限流，本测试只发
        // len=0 一封（ptr<0 的空 payload 语义与 attack_at 同源，既有实现覆盖）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(BRIDGE_SENDER_WAT), 10_000, id(76)).unwrap();
        enable_bridge(&mut handle, &[1], 8192, 4);
        assert_eq!(bridge_send_call(&mgr, &mut handle, 1, 0, 0), 0, "len=0 放行空信");
        let pending = &handle.store.data().bridge_pending;
        assert_eq!(pending.len(), 1);
        assert!(pending[0].payload.is_empty(), "len=0 -> 空 payload");
    }

    // === call_on_bridge_reply（D-001b：回信投递，grow 分配写入）===

    /// 回信观察模块：bridge_on_reply 扫回信字节找第一个 ASCII 数字存 $cmd
    ///（get_cmd 观测导出断言用）——与 bridge-demo.wat 同款解析逻辑的测试缩小版
    const REPLY_WATCHER_WAT: &str = r#"(module
  (global $cmd (mut i32) (i32.const 0))
  (memory (export "memory") 1)
  (func $scan_digit (param $ptr i32) (param $len i32) (result i32)
    (local $i i32) (local $c i32)
    (block $done
      (loop $loop
        (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
        (local.set $c (i32.load8_u (i32.add (local.get $ptr) (local.get $i))))
        (if (i32.and (i32.ge_u (local.get $c) (i32.const 48)) (i32.le_u (local.get $c) (i32.const 57)))
          (then (return (i32.sub (local.get $c) (i32.const 48)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)))
    i32.const 0)
  (func (export "bridge_on_reply") (param $port i32) (param $ptr i32) (param $len i32)
    (global.set $cmd (call $scan_digit (local.get $ptr) (local.get $len))))
  (func (export "life") (result i32) i32.const 1)
  (func (export "get_cmd") (result i32) (global.get $cmd)))"#;

    #[test]
    fn call_on_bridge_reply_grows_memory_and_delivers() {
        // 回信写入 = grow 新页（永不覆盖玩家数据），bridge_on_reply 读到回信字节：
        // {"dx":3} 含数字 3 -> $cmd=3；内存从 1 页长到 2 页（grow 实证）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(REPLY_WATCHER_WAT), 100_000, id(80)).unwrap();
        let mem = handle.instance.get_memory(&mut handle.store, "memory").unwrap();
        let before_pages = mem.size(&handle.store);
        assert_eq!(before_pages, 1);
        let used = mgr
            .call_on_bridge_reply(&mut handle, 1, br#"{"dx":3}"#, 10_000)
            .unwrap()
            .expect("导出了 bridge_on_reply 应返回 Some(used)");
        assert!(used > 0 && used <= 10_000, "消耗应记在预算尺度内");
        let mem = handle.instance.get_memory(&mut handle.store, "memory").unwrap();
        assert!(
            mem.size(&handle.store) > before_pages,
            "回信应经 grow 新页写入（页数增长）"
        );
        let func = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_cmd")
            .unwrap();
        assert_eq!(func.call(&mut handle.store, ()).unwrap(), 3, "造物应读到数字 3");
    }

    #[test]
    fn call_on_bridge_reply_empty_body_delivers_len_zero() {
        // 空回信照常投递（len=0，grow 0 页）：扫描无数字 -> $cmd 归 0
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(REPLY_WATCHER_WAT), 100_000, id(81)).unwrap();
        mgr.call_on_bridge_reply(&mut handle, 1, b"no digits", 10_000)
            .unwrap()
            .expect("第一封有内容");
        let func = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_cmd")
            .unwrap();
        assert_eq!(func.call(&mut handle.store, ()).unwrap(), 0, "无数字应扫出 0");
        mgr.call_on_bridge_reply(&mut handle, 1, &[], 10_000).unwrap();
        assert_eq!(func.call(&mut handle.store, ()).unwrap(), 0, "空信 len=0 可投递");
    }

    #[test]
    fn call_on_bridge_reply_without_export_is_noop() {
        // 负面路径：缺省导出 = 收不到回信（设计 2.6），Ok(None) 零副作用
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(82)).unwrap();
        assert!(
            mgr.call_on_bridge_reply(&mut handle, 1, b"x", 10_000)
                .unwrap()
                .is_none()
        );
    }

    /// 坏签名模块：导出了 bridge_on_reply 但签名不符（无参返 i32——
    /// has_export 用 get_func 探测会过，get_typed_func 才暴露签名不匹配）
    const REPLY_BAD_SIG_WAT: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "bridge_on_reply") (result i32) i32.const 7)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn call_on_bridge_reply_bad_signature_leaves_no_half_side_effect() {
        // P2-A：签名核验先于 grow+写回——错误签名导出时投递 Err，且内存分毫
        // 不动（不再留下"字节已落内存但回调没跑"的半副作用残局）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(REPLY_BAD_SIG_WAT), 1000, id(84))
            .unwrap();
        let before = handle
            .instance
            .get_memory(&mut handle.store, "memory")
            .unwrap()
            .size(&handle.store);
        assert!(
            mgr.call_on_bridge_reply(&mut handle, 1, b"123", 10_000)
                .is_err(),
            "签名不匹配应 Err（get_typed_func 失败）"
        );
        let after = handle
            .instance
            .get_memory(&mut handle.store, "memory")
            .unwrap()
            .size(&handle.store);
        assert_eq!(after, before, "签名核验失败不得动内存（无 grow 半副作用）");
    }

    #[test]
    fn call_on_bridge_reply_restores_fuel() {
        // 燃料不污染（on_contact 同款）：包裹前 fuel=55_555 -> 调用后恢复原值
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(REPLY_WATCHER_WAT), 100_000, id(83)).unwrap();
        handle.store.set_fuel(55_555).unwrap();
        mgr.call_on_bridge_reply(&mut handle, 1, b"7", 10_000).unwrap();
        assert_eq!(
            handle.store.get_fuel().unwrap(),
            55_555,
            "调用后 fuel 应恢复原值（回信投递不扣手续费）"
        );
    }

    // === env.attack_at import（架构 4.2：攻击排队）===

    /// 攻击方 WAT：导出 memory + on_tick 调 attack_at(5.0, 5.0, 0, 0)（空 payload）
    const ATTACKER_WAT: &str = r#"(module
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "on_tick")
    f64.const 5.0 f64.const 5.0 i32.const 0 i32.const 0 call $attack_at drop)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 带 payload 的攻击方 WAT：data 段写 [1,2,3,4] 到 memory[0..4]，on_tick 调
    /// attack_at(7.0, 8.0, 0, 4)（payload_ptr=0, payload_len=4，验证从 memory 读 payload）
    const ATTACKER_WITH_DATA_WAT: &str = r#"(module
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "\01\02\03\04")
  (func (export "on_tick")
    f64.const 7.0 f64.const 8.0 i32.const 0 i32.const 4 call $attack_at drop)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn attack_at_import_queues_pending_attack() {
        // env.attack_at import：on_tick 调 attack_at -> 请求排入 pending_attacks
        // （import 不检查坐标，只排队；实际击杀由阶段二 process_attacks 处理）
        let mgr = SandboxManager::new().unwrap();
        let attacker_id = id(42);
        let mut handle = mgr
            .create_sandbox(&wasm(ATTACKER_WAT), 1000, attacker_id)
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        let attacks = &handle.store.data().pending_attacks;
        assert_eq!(attacks.len(), 1, "应排队 1 次攻击");
        assert_eq!(attacks[0].attacker, attacker_id, "攻击者应为沙箱绑定的实体");
        assert_eq!(attacks[0].target_pos, (5.0, 5.0));
        assert!(attacks[0].payload.is_empty(), "空 payload（ptr=0,len=0）");
    }

    #[test]
    fn attack_at_import_reads_payload_from_memory() {
        // import 从 caller 的 memory export 读取 payload 字节 [ptr..ptr+len]
        let mgr = SandboxManager::new().unwrap();
        let attacker_id = id(7);
        let mut handle = mgr
            .create_sandbox(&wasm(ATTACKER_WITH_DATA_WAT), 1000, attacker_id)
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        let attacks = &handle.store.data().pending_attacks;
        assert_eq!(attacks.len(), 1);
        assert_eq!(attacks[0].target_pos, (7.0, 8.0));
        assert_eq!(
            attacks[0].payload, vec![1, 2, 3, 4],
            "应从 memory 读取 payload 字节"
        );
    }

    #[test]
    fn replace_sandbox_preserves_entity_id() {
        // 路线A替换后新沙箱保留旧 entity_id（攻击替换的是同一实体的代码，身份不变）
        let mgr = SandboxManager::new().unwrap();
        let attacker_id = id(99);
        let handle = mgr
            .create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, attacker_id)
            .unwrap();
        assert_eq!(handle.store.data().entity_id, attacker_id, "替换前 entity_id 应已绑定");
        let new_handle = mgr
            .replace_sandbox(handle, &wasm(LIFE_FALSE_WAT), 1000)
            .unwrap();
        assert_eq!(
            new_handle.store.data().entity_id,
            attacker_id,
            "替换后 entity_id 应保留"
        );
    }

    // === P0 僵尸实体修复：先建后拆 ===

    #[test]
    fn replace_sandbox_invalid_wasm_keeps_old_handle() {
        // P0 僵尸修复：非法 WASM payload 编译失败时，旧沙箱必须原样返回（随 Err 载荷），
        // 调用方放回映射表后"存活实体必有沙箱"不变量不破。
        // 旧实现先 drop(handle) 再 Module::new，失败后旧沙箱已销毁 -> 僵尸实体。
        let mgr = SandboxManager::new().unwrap();
        let owner = id(100);
        let handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, owner).unwrap();
        // 垃圾字节：非合法 WASM 魔数，Module::new 必失败
        let garbage: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
        let Err((mut old_handle, _err)) = mgr.replace_sandbox(handle, garbage, 1000) else {
            panic!("非法 payload 应替换失败并带回旧沙箱");
        };
        // 旧沙箱原样可用：life() 仍是原模块的 true
        assert!(
            mgr.call_life(&mut old_handle).unwrap(),
            "失败后旧沙箱应保留且可继续调用"
        );
        // 旧沙箱 entity_id 身份不变
        assert_eq!(old_handle.store.data().entity_id, owner, "旧沙箱身份应不变");
    }

    // === #051 皮入沙箱：沙箱资产段（装戴清单随沙箱走，夺沙箱=夺全套）===

    #[test]
    fn replace_sandbox_transfers_assets_to_new_sandbox() {
        // 规则（#051）：换血=夺全套——replace_sandbox 成功建新后，旧沙箱资产段
        // 整体转移进新沙箱（装戴清单随沙箱易主随行，无拆分选项）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(1)).unwrap();
        // 装戴清单由 host 写入（未来装戴刀的入口；本测直接落 SandboxData 验转移语义）
        handle.store.data_mut().assets = vec![
            "av_0123456789abcdef".to_string(),
            "av_fedcba9876543210".to_string(),
        ];
        let new_handle = mgr
            .replace_sandbox(handle, &wasm(LIFE_FALSE_WAT), 1000)
            .unwrap();
        assert_eq!(
            new_handle.store.data().assets,
            vec![
                "av_0123456789abcdef".to_string(),
                "av_fedcba9876543210".to_string()
            ],
            "换血后新沙箱应读得到完整资产清单（随行转移）"
        );
    }

    #[test]
    fn replace_sandbox_failure_keeps_assets_in_old_sandbox() {
        // 负面（卡面负面路径②的 arena 层锚）：换血失败（payload 非法）时旧沙箱
        // 原样带回——资产清单必须还在旧沙箱里（拒绝换血，资产不丢）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(2)).unwrap();
        handle.store.data_mut().assets = vec!["av_0123456789abcdef".to_string()];
        let garbage: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
        let Err((old_handle, _)) = mgr.replace_sandbox(handle, garbage, 1000) else {
            panic!("非法 payload 应替换失败并带回旧沙箱");
        };
        assert_eq!(
            old_handle.store.data().assets,
            vec!["av_0123456789abcdef".to_string()],
            "换血失败资产应留在旧沙箱（原样带回）"
        );
    }

    #[test]
    fn replaced_sandbox_assets_writable_by_new_code() {
        // 卡面 5（改皮写口存在性）：换血后新沙箱 assets 段可被新代码/host 改写
        // 并读回——接触通道的换血路径本身就是写口，本测锚定写口存在（改皮战术
        // "金龙旗改黑龙旗=占领宣示" 的通路基础，零新 import）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(3)).unwrap();
        handle.store.data_mut().assets = vec!["av_0123456789abcdef".to_string()];
        let mut new_handle = mgr
            .replace_sandbox(handle, &wasm(LIFE_FALSE_WAT), 1000)
            .unwrap();
        // 改皮：新主人整表换新（黑龙旗替金龙旗）
        new_handle.store.data_mut().assets = vec!["av_ffffffffffffffff".to_string()];
        assert_eq!(
            new_handle.store.data().assets,
            vec!["av_ffffffffffffffff".to_string()],
            "换血后资产段可写且写后可读回（改皮战术通路）"
        );
    }

    #[test]
    fn replace_sandbox_assets_empties_old_sandbox() {
        // 边界：转移是 take（移动语义）——转移后旧沙箱资产段为空，防双重持有
        let mgr = SandboxManager::new().unwrap();
        let mut old = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(4)).unwrap();
        let mut new = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(5)).unwrap();
        old.store.data_mut().assets = vec!["av_0123456789abcdef".to_string()];
        SandboxManager::replace_sandbox_assets(&mut old, &mut new);
        assert!(
            old.store.data().assets.is_empty(),
            "转移后旧沙箱资产段应清空（移动非复制）"
        );
        assert_eq!(new.store.data().assets.len(), 1, "新沙箱应收下清单");
    }

    // === P1：payload 大小上限（import 层拒绝超限）===

    /// 大内存攻击方 WAT：24 页 memory（约 1.5MB），on_tick 连调两次 attack_at——
    /// 第一次 len=上限（应排队），第二次 len=上限+1（应拒绝）
    const OVERSIZED_ATTACKER_WAT: &str = r#"(module
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 24)
  (func (export "on_tick")
    f64.const 1.0 f64.const 2.0 i32.const 0 i32.const 1048576 call $attack_at drop
    f64.const 1.0 f64.const 2.0 i32.const 0 i32.const 1048577 call $attack_at drop)
  (func (export "life") (result i32) i32.const 1))"#;

    // === env.get_pos import（架构 3.2：读自己坐标）===

    /// 探针 WAT：on_tick 调 get_pos（多值返回 x,y），存入全局变量供 host 观测
    const GET_POS_PROBE_WAT: &str = r#"(module
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (global $gx (mut f64) (f64.const 0))
  (global $gy (mut f64) (f64.const 0))
  (func (export "on_tick")
    call $get_pos
    global.set $gy
    global.set $gx)
  (func (export "get_x") (result f64) global.get $gx)
  (func (export "get_y") (result f64) global.get $gy)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 从实体当前沙箱调用无参 f64 导出（测试观测探针全局变量）
    fn call_f64_export(handle: &mut SandboxHandle, name: &str) -> f64 {
        let func = handle
            .instance
            .get_typed_func::<(), f64>(&mut handle.store, name)
            .unwrap();
        func.call(&mut handle.store, ()).unwrap()
    }

    #[test]
    fn get_pos_import_returns_sandbox_pos() {
        // get_pos import：on_tick 调 get_pos 拿到 host 填充的自身坐标快照。
        // host 在阶段一前把实体坐标写进 SandboxData.pos（import 回调拿不到 world，
        // 快照模式与 attack_at 排队一致：玩家代码读 tick 开始时的状态）。
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(GET_POS_PROBE_WAT), 100_000, id(301))
            .unwrap();
        handle.store.data_mut().pos = (3.5, 7.25);
        mgr.call_on_tick(&mut handle).unwrap();
        let x = call_f64_export(&mut handle, "get_x");
        let y = call_f64_export(&mut handle, "get_y");
        assert_eq!(x, 3.5, "get_pos 应返回快照坐标 x");
        assert_eq!(y, 7.25, "get_pos 应返回快照坐标 y");
        assert!(
            handle.store.data().api_calls.contains(&"get_pos".to_string()),
            "get_pos 调用应记入 api_calls 元数据"
        );
    }

    // === env.scan import（架构 3.2 感知接口 MVP：最近敌人坐标写 memory）===

    /// 扫描方 WAT：on_tick 调 scan(0, 8)（敌人 x 写 memory[0..8]、y 写 [8..16]），
    /// 返回值存全局；导出读回函数供 host 观测
    const SCAN_PROBE_WAT: &str = r#"(module
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (global $found (mut i32) (i32.const 0))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const 0 i32.const 8 call $scan global.set $found)
  (func (export "get_found") (result i32) global.get $found)
  (func (export "get_enemy_x") (result f64) (f64.load (i32.const 0)))
  (func (export "get_enemy_y") (result f64) (f64.load (i32.const 8)))
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn scan_import_writes_enemy_pos_to_memory() {
        // scan：有敌人时把最近敌人坐标写入 caller memory（x@ptr0 8字节、y@ptr1 8字节），返回 1
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(SCAN_PROBE_WAT), 100_000, id(401))
            .unwrap();
        handle.store.data_mut().nearest_enemy = Some((-2.5, 9.75));
        mgr.call_on_tick(&mut handle).unwrap();
        let func_found = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_found")
            .unwrap();
        assert_eq!(func_found.call(&mut handle.store, ()).unwrap(), 1, "找到敌人应返回 1");
        let x = call_f64_export(&mut handle, "get_enemy_x");
        let y = call_f64_export(&mut handle, "get_enemy_y");
        assert_eq!(x, -2.5, "敌人 x 应写入 memory[0..8]");
        assert_eq!(y, 9.75, "敌人 y 应写入 memory[8..16]");
        assert!(
            handle.store.data().api_calls.contains(&"scan".to_string()),
            "scan 调用应记入 api_calls 元数据"
        );
    }

    #[test]
    fn scan_import_returns_zero_when_no_enemy() {
        // 负面路径：世界没有其他存活实体（nearest_enemy=None）-> 返回 0、不写 memory
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(SCAN_PROBE_WAT), 100_000, id(402))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        let func_found = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_found")
            .unwrap();
        assert_eq!(func_found.call(&mut handle.store, ()).unwrap(), 0, "无敌人应返回 0");
        let x = call_f64_export(&mut handle, "get_enemy_x");
        assert_eq!(x, 0.0, "无敌人不应写 memory（保持初始 0）");
    }

    #[test]
    fn scan_import_rejects_out_of_bounds_ptr() {
        // 负面路径：写入指针越界（memory 1 页=64KB，ptr=65536 越界）-> 返回 0 不 panic
        const SCAN_BAD_PTR_WAT: &str = r#"(module
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (global $found (mut i32) (i32.const 0))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const 65536 i32.const 8 call $scan global.set $found)
  (func (export "get_found") (result i32) global.get $found)
  (func (export "life") (result i32) i32.const 1))"#;
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(SCAN_BAD_PTR_WAT), 100_000, id(403))
            .unwrap();
        handle.store.data_mut().nearest_enemy = Some((1.0, 2.0));
        mgr.call_on_tick(&mut handle).unwrap();
        let func_found = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_found")
            .unwrap();
        assert_eq!(
            func_found.call(&mut handle.store, ()).unwrap(),
            0,
            "指针越界应返回 0（扫描作废）"
        );
    }

    // === env.move_by import（架构 3.2 空间接口 MVP：相对移动经 SetPos 队列）===

    /// 移动方 WAT：on_tick 调 move_by(2.0, 3.0)，返回值存全局
    const MOVER_WAT: &str = r#"(module
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    f64.const 2.0 f64.const 3.0 call $move_by global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 地下移动 WAT：on_tick 调 move_by(0.0, -100.0)（从 y>=0 出发目标必钻地）
    const UNDERGROUND_MOVER_WAT: &str = r#"(module
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    f64.const 0.0 f64.const -100.0 call $move_by global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    /// NaN 移动 WAT：on_tick 调 move_by(NaN, 0.0)（除零/溢出可产生 NaN，防御性拒绝）
    const NAN_MOVER_WAT: &str = r#"(module
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    f64.const 0.0 f64.const 0.0 f64.div f64.const 0.0 call $move_by global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn move_by_import_queues_setpos_change() {
        // move_by：合法移动 -> 排队 PendingChange::SetPos（目标点=快照坐标+偏移），返回 1。
        // 不即时改坐标：与攻击排队同模式，阶段二统一应用（架构 4.1 PendingChanges）。
        let mgr = SandboxManager::new().unwrap();
        let mover_id = id(501);
        let mut handle = mgr
            .create_sandbox(&wasm(MOVER_WAT), 100_000, mover_id)
            .unwrap();
        handle.store.data_mut().pos = (10.0, 20.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let func_ret = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_ret")
            .unwrap();
        assert_eq!(func_ret.call(&mut handle.store, ()).unwrap(), 1, "合法移动应返回 1");
        let moves = &handle.store.data().pending_moves;
        assert_eq!(moves.len(), 1, "应排队 1 条移动请求");
        match &moves[0] {
            PendingChange::SetPos { entity, new_pos } => {
                assert_eq!(*entity, mover_id, "SetPos 应指向沙箱绑定的实体");
                assert_eq!(*new_pos, (12.0, 23.0), "目标点应为快照坐标+偏移");
            }
            other => panic!("应为 SetPos 变更，实际 {other:?}"),
        }
        assert!(
            handle.store.data().api_calls.contains(&"move_by".to_string()),
            "move_by 调用应记入 api_calls 元数据"
        );
    }

    #[test]
    fn move_by_import_rejects_underground_target() {
        // 负面路径（移动越界）：目标 y < 0 钻地 -> 拒绝（返回 0，不入队）。
        // 默认物理地面 y=0，钻地移动是非法请求。
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(UNDERGROUND_MOVER_WAT), 100_000, id(502))
            .unwrap();
        handle.store.data_mut().pos = (5.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let func_ret = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_ret")
            .unwrap();
        assert_eq!(func_ret.call(&mut handle.store, ()).unwrap(), 0, "钻地移动应被拒绝");
        assert!(
            handle.store.data().pending_moves.is_empty(),
            "被拒绝的移动不应入队"
        );
    }

    #[test]
    fn move_by_import_rejects_non_finite_delta() {
        // 负面路径：dx/dy 非有限（NaN/无穷，WASM 除零可产生）-> 拒绝（返回 0，不入队），
        // 防止 NaN 坐标污染世界状态。
        // 构造：0.0/0.0 = NaN（WASM 浮点除零不 trap，产生 NaN）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(NAN_MOVER_WAT), 100_000, id(503))
            .unwrap();
        handle.store.data_mut().pos = (5.0, 5.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let func_ret = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_ret")
            .unwrap();
        assert_eq!(func_ret.call(&mut handle.store, ()).unwrap(), 0, "NaN 移动应被拒绝");
        assert!(
            handle.store.data().pending_moves.is_empty(),
            "被拒绝的移动不应入队"
        );
    }

    /// 大坐标移动 WAT：on_tick 调 move_by(1e308, 0.0)——host 侧 pos.x=1e308 时
    /// x+dx 按 IEEE 754 上溢为 +inf（有限+有限可上溢）
    const BIG_MOVER_WAT: &str = r#"(module
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    f64.const 1e308 f64.const 0.0 call $move_by global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn move_by_import_rejects_overflowed_target() {
        // P1（审查 1 轮）：dx/dy 各自有限，但 x+dx 上溢为 +inf -> 目标点非有限，
        // 必须拒绝（入队 SetPos(inf) 会污染世界状态：感知 NaN/攻击坐标永失配）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(BIG_MOVER_WAT), 100_000, id(504))
            .unwrap();
        handle.store.data_mut().pos = (1e308, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let func_ret = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_ret")
            .unwrap();
        assert_eq!(
            func_ret.call(&mut handle.store, ()).unwrap(),
            0,
            "上溢目标点应被拒绝"
        );
        assert!(
            handle.store.data().pending_moves.is_empty(),
            "上溢移动不应入队"
        );
    }

    #[test]
    fn scan_import_writes_nothing_when_y_ptr_invalid() {
        // P2（审查 1 轮）：y 指针非法时 x 也不应被写入（原子语义：全写或全不写），
        // 与注释"写入指针非法：不写 memory"一致
        const SCAN_BAD_Y_PTR_WAT: &str = r#"(module
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (global $found (mut i32) (i32.const 0))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const 0 i32.const 65536 call $scan global.set $found)
  (func (export "get_found") (result i32) global.get $found)
  (func (export "get_enemy_x") (result f64) (f64.load (i32.const 0)))
  (func (export "life") (result i32) i32.const 1))"#;
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(SCAN_BAD_Y_PTR_WAT), 100_000, id(404))
            .unwrap();
        handle.store.data_mut().nearest_enemy = Some((9.5, 2.5));
        mgr.call_on_tick(&mut handle).unwrap();
        let func_found = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_found")
            .unwrap();
        assert_eq!(func_found.call(&mut handle.store, ()).unwrap(), 0, "y 指针非法应返回 0");
        let x = call_f64_export(&mut handle, "get_enemy_x");
        assert_eq!(x, 0.0, "y 指针非法时 x 不应被写入（保持初始 0）");
    }

    #[test]
    fn attack_at_import_rejects_oversized_payload() {
        // P1：len 超过 1MB 上限的攻击请求被 import 拒绝（不入队），不拷贝大块内存
        let mgr = SandboxManager::new().unwrap();
        let attacker_id = id(201);
        let mut handle = mgr
            .create_sandbox(&wasm(OVERSIZED_ATTACKER_WAT), 100_000, attacker_id)
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        let attacks = &handle.store.data().pending_attacks;
        assert_eq!(attacks.len(), 1, "只有上限内的那一刀应入队");
        assert_eq!(
            attacks[0].payload.len(),
            crate::sandbox::MAX_PAYLOAD_LEN,
            "入队 payload 应恰为上限大小（len == 上限不超限）"
        );
    }

    // === env.log marker 值记录（战况字幕的盾量上报通路：玩家代码 log(7) 后跟 log(余量)）===

    /// 连发两个 marker 的 WAT：on_tick 调 log(7) 再 log(5)（模拟 D 拦截上报盾量 5）
    const DOUBLE_LOG_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (func (export "on_tick")
    i32.const 7 call $log
    i32.const 5 call $log)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn log_import_records_marker_values_in_order() {
        // env.log：除 api_calls 元数据与 stderr 日志外，把 marker 值按序记入
        // SandboxData.log_markers（host 侧字幕解析盾量的数据源，玩家代码零改动）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(DOUBLE_LOG_WAT), 100_000, id(601))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(
            handle.store.data().log_markers,
            vec![7, 5],
            "env.log 的 marker 值应按调用顺序记录"
        );
    }

    #[test]
    fn log_markers_stay_empty_when_no_log_called() {
        // 负面路径：不调 log 的模块 -> log_markers 保持为空
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(LIFE_TRUE_WAT), 100_000, id(602))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert!(
            handle.store.data().log_markers.is_empty(),
            "未调 env.log 时 log_markers 应为空"
        );
    }

    // === env.visible_count / env.visible_at（三层感知 Phase A：眼睛列表）===

    /// 眼睛列表消费者 WAT：on_tick 先 visible_count 存全局，再对 idx=0 调
    /// visible_at(0, 0, 8)（x 写 memory[0..8]、y 写 [8..16]）
    const EYES_WAT: &str = r#"(module
  (import "env" "visible_count" (func $visible_count (result i32)))
  (import "env" "visible_at" (func $visible_at (param i32 i32 i32) (result i32)))
  (global $count (mut i32) (i32.const -1))
  (global $got (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (func (export "on_tick")
    call $visible_count global.set $count
    i32.const 0 i32.const 0 i32.const 8 call $visible_at global.set $got)
  (func (export "get_count") (result i32) global.get $count)
  (func (export "get_got") (result i32) global.get $got)
  (func (export "get_x") (result f64) (f64.load (i32.const 0)))
  (func (export "get_y") (result f64) (f64.load (i32.const 8)))
  (func (export "life") (result i32) i32.const 1))"#;

    /// 眼睛列表越界读取 WAT：对 idx=5 调 visible_at（列表只有 1 项）
    const EYES_OUT_OF_RANGE_WAT: &str = r#"(module
  (import "env" "visible_at" (func $visible_at (param i32 i32 i32) (result i32)))
  (global $got (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const 5 i32.const 0 i32.const 8 call $visible_at global.set $got)
  (func (export "get_got") (result i32) global.get $got)
  (func (export "get_x") (result f64) (f64.load (i32.const 0)))
  (func (export "life") (result i32) i32.const 1))"#;

    /// 眼睛列表负索引 WAT：对 idx=-1 调 visible_at
    const EYES_NEGATIVE_IDX_WAT: &str = r#"(module
  (import "env" "visible_at" (func $visible_at (param i32 i32 i32) (result i32)))
  (global $got (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const -1 i32.const 0 i32.const 8 call $visible_at global.set $got)
  (func (export "get_got") (result i32) global.get $got)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 眼睛列表坏指针 WAT：x_ptr 越界（memory 1 页 = 64KB，ptr=65536）
    const EYES_BAD_PTR_WAT: &str = r#"(module
  (import "env" "visible_at" (func $visible_at (param i32 i32 i32) (result i32)))
  (global $got (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const 0 i32.const 65536 i32.const 8 call $visible_at global.set $got)
  (func (export "get_got") (result i32) global.get $got)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn visible_count_returns_snapshot_len() {
        // visible_count：返回 host 填充的可见快照长度（host 侧已排除自己）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(EYES_WAT), 100_000, id(701)).unwrap();
        handle.store.data_mut().visible = vec![(1.0, 2.0), (3.0, 4.0)];
        mgr.call_on_tick(&mut handle).unwrap();
        let count = call_i32_export(&mut handle, "get_count");
        assert_eq!(count, 2, "visible_count 应返回快照长度");
        assert!(
            handle.store.data().api_calls.contains(&"visible_count".to_string()),
            "visible_count 调用应记入 api_calls 元数据"
        );
    }

    #[test]
    fn visible_count_zero_when_list_empty() {
        // 负面路径：可见列表为空（周围无敌人）-> 返回 0
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(EYES_WAT), 100_000, id(702)).unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        let count = call_i32_export(&mut handle, "get_count");
        assert_eq!(count, 0, "空列表应返回 0");
    }

    #[test]
    fn visible_at_reads_coordinates_into_memory() {
        // visible_at：idx=0 命中 -> 坐标写 memory（x@0..8 y@8..16），返回 1
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(EYES_WAT), 100_000, id(703)).unwrap();
        handle.store.data_mut().visible = vec![(7.5, -2.25), (100.0, 200.0)];
        mgr.call_on_tick(&mut handle).unwrap();
        let got = call_i32_export(&mut handle, "get_got");
        assert_eq!(got, 1, "idx=0 应命中");
        assert_eq!(call_f64_export(&mut handle, "get_x"), 7.5, "x 应写入 memory[0..8]");
        assert_eq!(call_f64_export(&mut handle, "get_y"), -2.25, "y 应写入 memory[8..16]");
        assert!(
            handle.store.data().api_calls.contains(&"visible_at".to_string()),
            "visible_at 调用应记入 api_calls 元数据"
        );
    }

    #[test]
    fn visible_at_out_of_range_returns_zero() {
        // 负面路径：idx 越界（列表 1 项取 idx=5）-> 返回 0、不写 memory
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(EYES_OUT_OF_RANGE_WAT), 100_000, id(704))
            .unwrap();
        handle.store.data_mut().visible = vec![(7.5, -2.25)];
        mgr.call_on_tick(&mut handle).unwrap();
        let got = call_i32_export(&mut handle, "get_got");
        assert_eq!(got, 0, "越界应返回 0");
        let x = call_f64_export(&mut handle, "get_x");
        assert_eq!(x, 0.0, "越界不应写 memory（保持初始 0）");
    }

    #[test]
    fn visible_at_negative_idx_returns_zero() {
        // 负面路径：idx 负数 -> 返回 0（负索引不是合法查询）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(EYES_NEGATIVE_IDX_WAT), 100_000, id(705))
            .unwrap();
        handle.store.data_mut().visible = vec![(7.5, -2.25)];
        mgr.call_on_tick(&mut handle).unwrap();
        let got = call_i32_export(&mut handle, "get_got");
        assert_eq!(got, 0, "负索引应返回 0");
    }

    #[test]
    fn visible_at_bad_ptr_returns_zero() {
        // 负面路径：写入指针越界 -> 返回 0 不 panic（同 scan 坏指针语义）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(EYES_BAD_PTR_WAT), 100_000, id(706))
            .unwrap();
        handle.store.data_mut().visible = vec![(7.5, -2.25)];
        mgr.call_on_tick(&mut handle).unwrap();
        let got = call_i32_export(&mut handle, "get_got");
        assert_eq!(got, 0, "指针非法应返回 0");
    }

    // === env.probe（三层感知 Phase A：探测层 + 撒谎机制 + 被动体徵）===

    /// 被探测目标 WAT（撒谎者）：on_probe 固定回 "LIE9"（i32 LE 字节序列
    /// [0x4C,0x49,0x45,0x39]——谎称友军、life=9 的可识别假串，4 字节为
    /// on_probe i32->i32 签名的 MVP 上限，Phase 2 扩内存传递）
    const LIAR_WAT: &str = r#"(module
  (func (export "on_probe") (param i32) (result i32) i32.const 0x3945494C)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 无 on_probe 的目标 WAT（哑巴：被探测只能暴露默认被动体徵）
    const NO_PROBE_WAT: &str = r#"(module
  (func (export "life") (result i32) i32.const 1))"#;

    /// 探测者 WAT 模板：on_tick 调 probe(tx, ty, 100, 4, 200, 64)，返回值存全局。
    /// memory 布局：[100..104]=request 字节、[200..264]=response 缓冲（cap 64）。
    fn prober_wat(tx: &str, ty: &str) -> String {
        format!(
            r#"(module
  (import "env" "probe" (func $probe (param f64 f64 i32 i32 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -999))
  (memory (export "memory") 1)
  (data (i32.const 100) "\01\02\03\04")
  (func (export "on_tick")
    f64.const {tx} f64.const {ty} i32.const 100 i32.const 4 i32.const 200 i32.const 64
    call $probe global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "get_resp0") (result i32) (i32.load (i32.const 200)))
  (func (export "get_resp8") (result i32) (i32.load (i32.const 208)))
  (func (export "get_resp12") (result i32) (i32.load (i32.const 212)))
  (func (export "get_resp16") (result i32) (i32.load (i32.const 216)))
  (func (export "get_resp20") (result i32) (i32.load (i32.const 220)))
  (func (export "life") (result i32) i32.const 1))"#
        )
    }

    /// 从实体当前沙箱调用无参 i32 导出（测试观测探针全局变量）
    fn call_i32_export(handle: &mut SandboxHandle, name: &str) -> i32 {
        let func = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, name)
            .unwrap();
        func.call(&mut handle.store, ()).unwrap()
    }

    /// probe 测试挂载：目标沙箱入表 + 探测者建桥（bridge 由调用方持有保活，
    /// 测试结束前用 null 断开，符合 ProbeBridge 的 SAFETY 契约）
    #[test]
    fn probe_hit_returns_liar_response_plus_vitals() {
        // probe 命中：目标 on_probe 的假串原样透传（撒谎=玩家代码自由，框架只透传），
        // 之后追加 24 字节被动体徵（api_count/fuel 剩余/fuel 消耗，host 采集不可伪造）。
        // 总响应 4+24=28 字节 <= resp_cap 64，返回 28。
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(id(2), mgr.create_sandbox(&wasm(LIAR_WAT), 100_000, id(2)).unwrap());
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("7.0", "8.0")), 100_000, id(1))
            .unwrap();
        prober.store.data_mut().visible = vec![(7.0, 8.0)];
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), id(2)],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, 28, "命中应写入 4 字节假串 + 24 字节体徵");
        // 假串原样到达探测者（撒谎透传）
        let lie = call_i32_export(&mut prober, "get_resp0");
        assert_eq!(lie, 0x3945494C, "response 前 4 字节应是目标的假串 LIE9");
        // 体徵块：api_count（u64 LE @+4，目标没调过 api = 0）
        // 用 +12 的 i32 读 fuel 剩余低 32 位（prev_fuel=100_000 未跑过任何调用）
        let fuel_remaining = call_i32_export(&mut prober, "get_resp12");
        assert_eq!(fuel_remaining, 100_000, "体徵应含目标探测前 fuel 剩余量");
        // fuel 消耗 > 0（on_probe 至少执行了几条指令）
        let fuel_used = call_i32_export(&mut prober, "get_resp20");
        assert!(fuel_used > 0, "体徵应含本次 on_probe 实际消耗");
        assert!(
            prober.store.data().api_calls.contains(&"probe".to_string()),
            "probe 调用应记入探测者 api_calls 元数据"
        );
    }

    #[test]
    fn probe_miss_returns_minus_one() {
        // 负面路径：坐标上没有可见实体 -> 返回 -1，不写响应
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(id(2), mgr.create_sandbox(&wasm(LIAR_WAT), 100_000, id(2)).unwrap());
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("99.0", "99.0")), 100_000, id(1))
            .unwrap();
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), id(2)],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, -1, "坐标无人应返回 -1");
    }

    #[test]
    fn probe_self_pos_returns_minus_one() {
        // 负面路径（防御）：bridge 被污染含 caller 自己的坐标时，probe 自己仍被拒。
        // 正常 host 填充会排除自己，此测试锁定 import 层的第二道防线。
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(id(1), mgr.create_sandbox(&wasm(LIAR_WAT), 100_000, id(1)).unwrap());
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("3.0", "3.0")), 100_000, id(1))
            .unwrap();
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1)],
            visible_positions: vec![(3.0, 3.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, -1, "probe 自己应被拒绝");
    }

    #[test]
    fn probe_target_without_on_probe_returns_minus2_with_vitals() {
        // 负面路径：目标无 on_probe 导出 -> 返回 -2，响应位置写默认被动体徵（24 字节）
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(id(3), mgr.create_sandbox(&wasm(NO_PROBE_WAT), 100_000, id(3)).unwrap());
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("7.0", "8.0")), 100_000, id(1))
            .unwrap();
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), id(3)],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, -2, "目标无 on_probe 应返回 -2");
        // 默认被动体徵仍写入：-2 时响应为空，体徵块从 offset 0 开始
        //（api_count 在 [0..8]、fuel 在 [8..16]）——fuel 低 4 字节在 memory[208..212]
        let fuel_remaining = call_i32_export(&mut prober, "get_resp8");
        assert_eq!(fuel_remaining, 100_000, "-2 时也应写入默认体徵");
    }

    #[test]
    fn probe_truncates_response_to_resp_cap() {
        // 边界：resp_cap 小于 body（28 字节）-> 只写 cap 字节，返回 cap
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(id(2), mgr.create_sandbox(&wasm(LIAR_WAT), 100_000, id(2)).unwrap());
        // cap=10：模板的 resp_ptr=200 cap=64，此测试用内联小 cap 模块
        const PROBER_SMALL_CAP_WAT: &str = r#"(module
  (import "env" "probe" (func $probe (param f64 f64 i32 i32 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -999))
  (memory (export "memory") 1)
  (data (i32.const 100) "\01\02\03\04")
  (func (export "on_tick")
    f64.const 7.0 f64.const 8.0 i32.const 100 i32.const 4 i32.const 200 i32.const 10
    call $probe global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;
        let mut prober = mgr
            .create_sandbox(&wasm(PROBER_SMALL_CAP_WAT), 100_000, id(1))
            .unwrap();
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), id(2)],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, 10, "超容量应截断为 resp_cap 字节");
    }

    #[test]
    fn probe_restores_target_fuel_and_records_consumption() {
        // fuel 包裹（host 侧预算控制）：探测前目标 fuel=55_555 -> 探测后必须恢复
        // 55_555（probe 不扣目标手续费，架构 3.3）；消耗记进体徵；
        // fuel_consumed 元数据按原尺度修正（update_fuel 在包裹期间用预算尺度算过假象）。
        // 注：本测试不构造 on_probe 烧穿预算的场景——wasmtime 25 + Windows 下
        // fuel 耗尽 trap = 进程 abort（.cc-bugs #5），预算中断行为由 host 侧
        // set_fuel 包裹逻辑保证，测试只验证记账与恢复。
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        let mut liar = mgr.create_sandbox(&wasm(LIAR_WAT), 100_000, id(2)).unwrap();
        liar.store.set_fuel(55_555).unwrap();
        map.insert(id(2), liar);
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("7.0", "8.0")), 100_000, id(1))
            .unwrap();
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), id(2)],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        let ret = call_i32_export(&mut prober, "get_ret");
        drop(bridge);
        assert_eq!(ret, 28, "命中应正常返回");
        // 目标 fuel 恢复到探测前值
        let map_handle = map.get(&id(2)).unwrap();
        assert_eq!(
            map_handle.store.get_fuel().unwrap(),
            55_555,
            "探测后目标 fuel 应恢复原值（probe 不扣手续费）"
        );
        // fuel_consumed 元数据按原尺度修正（limit - prev_fuel）
        assert_eq!(
            map_handle.store.data().fuel_consumed,
            100_000 - 55_555,
            "fuel_consumed 应按探测前尺度重算，不被包裹污染"
        );
        // 体徵记录：fuel 剩余 = 55_555（探测前快照）
        let fuel_remaining = call_i32_export(&mut prober, "get_resp12");
        assert_eq!(fuel_remaining, 55_555, "体徵 fuel 剩余应是探测前快照");
        let fuel_used = call_i32_export(&mut prober, "get_resp20");
        assert!(fuel_used > 0 && fuel_used < 10_000, "消耗应在预算内且大于 0");
    }

    #[test]
    fn probe_with_null_bridge_returns_minus_one() {
        // 负面路径（防御）：桥未挂载（非阶段一窗口调用）-> 返回 -1 不崩
        let mgr = SandboxManager::new().unwrap();
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("7.0", "8.0")), 100_000, id(1))
            .unwrap();
        mgr.call_on_tick(&mut prober).unwrap();
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, -1, "桥未挂载应返回 -1");
    }


    // === env.spawn / env.destroy（门二：代码造物，decisions.md #012/#015）===

    /// WAT 字符串字面量转义（按字节）：\ 与 " 转义、换行转 \0a（WAT 禁裸换行）、
    /// 非 ASCII 字节逐字节转 \XX（裸高字节 wat crate 会错误解码）。
    /// 转义后每字节仍解码回 1 字节，len 与原文本字节数一致。
    fn wat_escape(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'\\' => out.push_str("\\\\"),
                b'"' => out.push_str("\\\""),
                b'\n' => out.push_str("\\0a"),
                0x20..=0x7E => out.push(b as char),
                _ => out.push_str(&format!("\\{b:02x}")),
            }
        }
        out
    }

    /// 造物方 WAT 生成器：data 段放造物源码（[0..)）+ 字形 T（[8192]）+ 颜色 cyan（[8256]
    /// ——外观段远离代码段，防大体积造物源码与外观 data 段重叠相互覆盖），
    /// on_tick 调 spawn(0, code.len(), x, y, 256, 300)，返回值存全局 get_ret。
    /// code_len 用 Rust 侧 code.len() 计算——手数 WAT 字符串长度天然易错（截断坑）。
    fn spawner_wat(code: &str, x: &str, y: &str) -> String {
        spawner_binding_wat(code, x, y, "i32.const 0")
    }

    /// 带契约的造物方 WAT（#034 修订四：spawn 第 9 参 binding；
    /// D-003：第 10 参 motion，默认补 i32.const 0 兼容旧调用点）
    fn spawner_binding_wat(code: &str, x: &str, y: &str, binding: &str) -> String {
        spawner_full_wat(code, x, y, binding, "i32.const 0")
    }

    /// 全参造物方 WAT（D-003：binding 第 9 参 + motion 第 10 参）
    fn spawner_full_wat(code: &str, x: &str, y: &str, binding: &str, motion: &str) -> String {
        spawner_velocity_wat(code, x, y, binding, motion, "f64.const 0.0", "f64.const 0.0")
    }

    /// 带出生初速的造物方 WAT（spawn 第 11/12 参 vx/vy；14 参生成器最全档：
    /// blob 恒无包 i32.const 0/0——blob 用例走 spawner_blob_wat；D-011 起补
    /// 第 20 参 link，默认无链 i32.const 0）
    fn spawner_velocity_wat(
        code: &str,
        x: &str,
        y: &str,
        binding: &str,
        motion: &str,
        vx: &str,
        vy: &str,
    ) -> String {
        let escaped = wat_escape(code);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    {x} {y}
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    {binding}
    {motion}
    {vx} {vy}
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 1.0
    f64.const 0.0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    call $spawn global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = code.len(),
            x = x,
            y = y,
            binding = binding,
            motion = motion,
            vx = vx,
            vy = vy,
        )
    }

    /// 连发两次 spawn 的 WAT：同 tick 第二次应被限流拒绝（每实体每 tick 最多 1 个）
    fn double_spawner_wat(code: &str) -> String {
        let escaped = wat_escape(code);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32) (result i32)))
  (global $r1 (mut i32) (i32.const -1))
  (global $r2 (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const 1.0 f64.const 1.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 1.0
    f64.const 0.0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    call $spawn global.set $r1
    i32.const 0 i32.const {len}
    f64.const 4.0 f64.const 4.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 1.0
    f64.const 0.0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    call $spawn global.set $r2)
  (func (export "get_r1") (result i32) global.get $r1)
  (func (export "get_r2") (result i32) global.get $r2)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = code.len(),
        )
    }

    /// 空 code WAT：code_len=0 应拒绝（没有代码造不出活的造物）
    const EMPTY_CODE_SPAWNER_WAT: &str = r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const 0
    f64.const 1.0 f64.const 1.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 1.0
    f64.const 0.0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    call $spawn global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 坏字形 WAT：glyph 两个字符 "TT" 应拒绝（渲染层一字形一实体）
    fn bad_glyph_spawner_wat(code: &str) -> String {
        let escaped = wat_escape(code);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "TT\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const 1.0 f64.const 1.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 1.0
    f64.const 0.0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    call $spawn global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = code.len(),
        )
    }

    /// 造物方 WAT 的合法造物源码（最简 life 模块，data 段内容与此一致）
    const SPAWNED_CODE: &str = r#"(module (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn spawn_import_queues_pending_spawn() {
        // env.spawn：合法请求（±10 内、限流未满、代码/外观合法）-> 排队 PendingSpawn，
        // 返回 1。owner 不在 import 层解析（import 拿不到 world），阶段二按产权链继承。
        let mgr = SandboxManager::new().unwrap();
        let creator = id(801);
        let wat_text = spawner_wat(SPAWNED_CODE, "f64.const 2.0", "f64.const 3.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, creator)
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let ret = call_i32_export(&mut handle, "get_ret");
        assert_eq!(ret, 1, "合法 spawn 应返回 1（已排队）");
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns.len(), 1, "应排队 1 个造物请求");
        let s = &spawns[0];
        assert_eq!(s.creator, creator, "creator 应为沙箱绑定实体");
        assert_eq!(s.pos, (2.0, 3.0), "出生点应保存请求传入的绝对坐标");
        assert_eq!(s.code, SPAWNED_CODE, "造物代码应从 memory data 段读取（NUL 截断）");
        assert_eq!(s.glyph, "T", "字形应从 memory 读取");
        assert_eq!(s.color, "cyan", "颜色应从 memory 读取");
        assert_eq!(s.fuel_limit, 100_000, "fuel 配额应继承创造者沙箱的 fuel_limit");
        assert_eq!(s.mass, 1.0, "#034 第 8 参 mass 应透传（import 层已钳制）");
        assert!(
            handle.store.data().api_calls.contains(&"spawn".to_string()),
            "spawn 调用应记入 api_calls 元数据"
        );
    }

    #[test]
    fn spawn_import_clamps_mass_out_of_bounds() {
        // #034 第 8 参 mass 护栏：1000 超上限钳到 100，0.001 低下限钳到 0.05（除零护栏）
        let mgr = SandboxManager::new().unwrap();
        let escaped = wat_escape(SPAWNED_CODE);
        // 两次调用只排队第一次（限流）——本测试只关心 mass 钳制，单发即可
        let wat_text = format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const 1.0 f64.const 1.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1000.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 1.0
    f64.const 0.0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    call $spawn drop)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = SPAWNED_CODE.len(),
        );
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(808))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].mass, 100.0, "超上限 mass 应钳到 100.0");
    }

    // === D-012 力场出生声明（spawn 第 15/16/17 参）===

    /// 指定 field 三参的造物方 WAT（D-012 spawn 第 15/16/17 参，17 参签名）：
    /// field 三参压栈序 = radius / gravity_scale / drag_delta
    fn field_spawner_wat(radius: &str, scale: &str, delta: &str) -> String {
        let escaped = wat_escape(SPAWNED_CODE);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const 1.0 f64.const 1.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    i32.const 0
    f64.const {radius}
    f64.const {scale}
    f64.const {delta}
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    call $spawn global.set $ret)
  (global $ret (mut i32) (i32.const -1))
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = SPAWNED_CODE.len(),
            radius = radius,
            scale = scale,
            delta = delta,
        )
    }

    #[test]
    fn spawn_import_queues_field_params() {
        // env.spawn 第 15/16/17 参透传：合法域内值原样进 PendingSpawn（import 层
        // 钳制后存档，阶段二 set_field_* 直存落库）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = field_spawner_wat("3.0", "0.5", "1.5");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(811))
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns.len(), 1, "合法 field 请求应排队");
        assert_eq!(spawns[0].field_radius, 3.0, "第 15 参场域半径应透传");
        assert_eq!(spawns[0].field_gravity_scale, 0.5, "第 16 参重力缩放应透传");
        assert_eq!(spawns[0].field_drag_delta, 1.5, "第 17 参阻力改写量应透传");
    }

    #[test]
    fn spawn_import_clamps_field_radius() {
        // 第 15 参场域半径护栏：100 超上限钳 25.0；-1 负值钳 0（无场最保守回退）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = field_spawner_wat("100.0", "1.0", "0.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(812))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns[0].field_radius, 25.0, "超上限场域半径应钳 25.0");
    }

    #[test]
    fn spawn_import_clamps_field_gravity_scale() {
        // 第 16 参重力缩放护栏：50 超上限钳 10.0；非有限值钳 1.0（中性缩放）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = field_spawner_wat("0.0", "50.0", "0.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(813))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns[0].field_gravity_scale, 10.0, "超上限重力缩放应钳 10.0");
    }

    // === D-009 液体出生声明（spawn 第 18/19 参）===

    /// 指定液体两参的造物方 WAT（D-009 spawn 第 18/19 参，19 参签名）：
    /// liquid 两参压栈序 = density / drag
    fn liquid_spawner_wat(density: &str, drag: &str) -> String {
        let escaped = wat_escape(SPAWNED_CODE);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "W\00")
  (data (i32.const 8256) "blue\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const 1.0 f64.const 1.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 1.0
    f64.const 0.0
    f64.const {density}
    f64.const {drag}
    i32.const 0
    call $spawn drop)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = SPAWNED_CODE.len(),
            density = density,
            drag = drag,
        )
    }

    #[test]
    fn spawn_import_queues_liquid_params() {
        // env.spawn 第 18/19 参透传：合法域内值原样进 PendingSpawn（import 层
        // 钳制后存档，阶段二 set_liquid_* 直存落库）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = liquid_spawner_wat("1.0", "0.35");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(821))
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns.len(), 1, "合法液体请求应排队");
        assert_eq!(spawns[0].liquid_density, 1.0, "第 18 参液体密度应透传");
        assert_eq!(spawns[0].liquid_drag, 0.35, "第 19 参水中阻尼应透传");
    }

    #[test]
    fn spawn_import_clamps_liquid_density_out_of_domain() {
        // 第 18 参液体密度护栏：50 超上限钳 5.0（出生声明家族哲学：超界钳回+
        // 警告非拒绝）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = liquid_spawner_wat("50.0", "0.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(822))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].liquid_density, 5.0, "超上限液体密度应钳 5.0");
    }

    #[test]
    fn spawn_import_clamps_liquid_drag_negative_and_non_finite() {
        // 第 19 参水中阻尼护栏：负值钳 0（无阻尼最保守回退）；非有限值钳 0
        let mgr = SandboxManager::new().unwrap();
        let wat_text = liquid_spawner_wat("1.0", "-1.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(823))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns[0].liquid_drag, 0.0, "负值阻尼应钳 0.0");
        // 非有限值（canonical NaN）：钳 0
        let wat_text = liquid_spawner_wat("1.0", "nan");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(824))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns[0].liquid_drag, 0.0, "非有限阻尼应钳 0.0");
    }

    #[test]
    fn spawn_import_clamps_field_drag_delta() {
        // 第 17 参阻力改写量护栏：100 超上限钳 25.0；-5 低下限钳 -1.0
        let mgr = SandboxManager::new().unwrap();
        let wat_text = field_spawner_wat("0.0", "1.0", "-5.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(814))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns[0].field_drag_delta, -1.0, "低下限阻力改写位应钳 -1.0");
    }

    // === D-012 协议 v3：on_field 询问应答（host→沙箱调用，第 14 个玩家函数契约）===

    /// 场源 WAT：导出 on_field(n_ptr, ids_cap, ids_ptr) -> i32——读 n（n_ptr 处
    /// i32）存入全局，返回常数 42（非 0=本拍 drag_delta 应答覆盖值）
    fn field_source_wat() -> String {
        r#"(module
  (memory (export "memory") 1)
  (global $n (mut i32) (i32.const -1))
  (global $first_id_hi (mut i32) (i32.const -1))
  (func (export "on_field") (param $n_ptr i32) (param $cap i32) (param $ids_ptr i32) (result i32)
    (global.set $n (i32.load (local.get $n_ptr)))
    (global.set $first_id_hi (i32.load (local.get $ids_ptr)))
    (i32.const 42))
  (func (export "get_n") (result i32) (global.get $n))
  (func (export "get_first_id_hi") (result i32) (global.get $first_id_hi))
  (func (export "life") (result i32) (i32.const 1)))"#
            .to_string()
    }

    #[test]
    fn call_on_field_skips_if_no_export() {
        // 未导出 on_field：按出生声明结算（no-op 防御，与 on_attack 未导出同款），
        // 返回 Ok(None)
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(LIFE_TRUE_WAT), 100_000, id(820))
            .unwrap();
        let out = mgr.call_on_field(&mut handle, &[1, 2, 3], 10_000).unwrap();
        assert!(out.is_none(), "未导出 on_field 应返回 None");
    }

    #[test]
    fn call_on_field_writes_ids_and_reads_answer() {
        // 预算包裹 + 通信锚：host 把 n 写进 n_ptr、清单写进 [ids_ptr, +8n)，
        // on_field 读到 n=2 与首个 id 低 32 位；返回 42 透传；fuel 预算独立
        //（包裹后恢复原剩余，fuel_consumed 元数据按原尺度重算）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&field_source_wat()), 100_000, id(821))
            .unwrap();
        // 显式置满 fuel 再包裹（实例化会吃掉几个 fuel，与 on_contact 恢复测试
        // 同款前置，断言"包裹恢复原值"不被实例化开销污染）
        handle.store.set_fuel(100_000).unwrap();
        let ids: [u64; 2] = [0x1122334455667788, 0x0102030405060708];
        let before = handle.store.get_fuel().unwrap();
        let out = mgr.call_on_field(&mut handle, &ids, 10_000).unwrap();
        let after = handle.store.get_fuel().unwrap();
        assert_eq!(out, Some(42), "on_field 返回值应透传");
        assert_eq!(call_i32_export(&mut handle, "get_n"), 2, "n 应写入 n_ptr 处");
        assert_eq!(
            call_i32_export(&mut handle, "get_first_id_hi"),
            0x5566_7788_u32 as i32,
            "首个 id 低 64 位 LE 的低 32 位应可读"
        );
        // 燃料恢复：预算包裹不烧目标 tick 配额（剩余量回原值；断言带实测值
        // 便于诊断包裹外泄漏）
        assert_eq!(
            after, before,
            "包裹后 fuel 应恢复到调用前剩余（before={before} after={after}）"
        );
    }

    #[test]
    fn call_on_field_signature_mismatch_is_err_not_panic() {
        // Err 路径（host 侧等价构造，.cc-bugs #5 红线：不用 wasm 内 trap）：
        // on_field 签名不匹配 -> Err 上抛，调用方按出生声明结算；fuel 不丢
        let mgr = SandboxManager::new().unwrap();
        let bad = r#"(module
  (memory (export "memory") 1)
  (func (export "on_field") (result i32) (i32.const 7))
  (func (export "life") (result i32) (i32.const 1)))"#;
        let mut handle = mgr.create_sandbox(&wasm(bad), 100_000, id(822)).unwrap();
        handle.store.set_fuel(100_000).unwrap();
        let out = mgr.call_on_field(&mut handle, &[1], 10_000);
        assert!(out.is_err(), "签名不匹配应 Err 上抛");
        assert_eq!(
            handle.store.get_fuel().unwrap(),
            100_000,
            "Err 路径 fuel 不应被弄丢"
        );
    }

    #[test]
    fn call_on_field_truncates_mailbox_without_panic() {
        // D-012 收尾刀 P1-2：域内实体数 > FIELD_MAILBOX_CAP（max_entities 可经
        // CODEWORLD_MAX_ENTITIES 调大）时清单截断——warn 一行 + 不 panic +
        // 模块读到的 n 恒 ≤ cap（信箱写入边界永不越界）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&field_source_wat()), 100_000, id(823))
            .unwrap();
        handle.store.set_fuel(100_000).unwrap();
        // 70 个 id（> 64 槽）：截断到 64，n 写入 64，on_field 正常应答
        let ids: Vec<u64> = (0..70).map(|i| i as u64 + 1).collect();
        let out = mgr.call_on_field(&mut handle, &ids, 10_000).unwrap();
        assert_eq!(out, Some(42), "截断不影响应答透传");
        assert_eq!(
            call_i32_export(&mut handle, "get_n"),
            FIELD_MAILBOX_CAP as i32,
            "超容清单写入模块读到的 n 应为截断后的 cap 值"
        );
    }

    // === D-012 交付物 8：field_pulse 分离式发射器（第 15 个 env import）===

    /// field_pulse 调用方 WAT：on_tick 调 env.field_pulse(x,y,vx,vy,radius)，
    /// 返回值存全局供断言
    fn pulse_wat(x: &str, y: &str, vx: &str, vy: &str, radius: &str) -> String {
        format!(
            r#"(module
  (import "env" "field_pulse" (func $field_pulse (param f64 f64 f64 f64 f64) (result i32)))
  (memory (export "memory") 1)
  (global $ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    (global.set $ret (call $field_pulse (f64.const {x}) (f64.const {y}) (f64.const {vx}) (f64.const {vy}) (f64.const {radius}))))
  (func (export "get_ret") (result i32) (global.get $ret))
  (func (export "life") (result i32) (i32.const 1)))"#,
            x = x,
            y = y,
            vx = vx,
            vy = vy,
            radius = radius,
        )
    }

    #[test]
    fn field_pulse_import_queues_pending_pulse() {
        // 合法脉冲排队：PendingFieldPulse 入队（source=沙箱绑定实体），返回 1
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&pulse_wat("1.0", "2.0", "3.0", "4.0", "3.0")), 100_000, id(830))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "合法脉冲应返回 1");
        let pulses = &handle.store.data().pending_field_pulses;
        assert_eq!(pulses.len(), 1);
        assert_eq!(pulses[0].source, id(830));
        assert_eq!(pulses[0].x, 1.0);
        assert_eq!(pulses[0].radius, 3.0);
    }

    #[test]
    fn field_pulse_rejects_non_finite() {
        // 校验一：坐标/冲量非有限拒绝
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&pulse_wat("nan", "2.0", "3.0", "4.0", "3.0")), 100_000, id(831))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "非有限应拒绝");
        assert!(handle.store.data().pending_field_pulses.is_empty());
    }

    #[test]
    fn field_pulse_rejects_impulse_over_world_max_speed() {
        // 校验二：冲量模长超世界极速拒绝（30,0 → 模长 30 > 25）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&pulse_wat("1.0", "2.0", "30.0", "0.0", "3.0")), 100_000, id(832))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "超极速冲量应拒绝");
        assert!(handle.store.data().pending_field_pulses.is_empty());
    }

    #[test]
    fn field_pulse_allows_impulse_exactly_max_speed() {
        // 校验三：恰等极速放行（与 spawn 初速同口径：严格 > 拒绝）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&pulse_wat("1.0", "2.0", "7.0", "24.0", "3.0")), 100_000, id(833))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "恰等极速应放行");
    }

    #[test]
    fn field_pulse_clamps_radius() {
        // 校验四：radius 钳 [0.1, 25.0]（100 → 25）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&pulse_wat("1.0", "2.0", "1.0", "1.0", "100.0")), 100_000, id(834))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "radius 钳制不是拒绝");
        assert_eq!(
            handle.store.data().pending_field_pulses[0].radius, 25.0,
            "超上限 radius 应钳 25.0"
        );
    }

    #[test]
    fn field_pulse_rejects_out_of_caller_range() {
        // 校验五：(x,y) 超出调用者快照 ±10 格拒绝（SPAWN_RANGE 同款防远程）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&pulse_wat("20.0", "2.0", "1.0", "1.0", "3.0")), 100_000, id(835))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "超 ±10 格应拒绝");
        assert!(handle.store.data().pending_field_pulses.is_empty());
    }

    #[test]
    fn field_pulse_rate_limited_per_tick() {
        // 限流：每实体每拍 1 次（与 spawn 同款）
        let mgr = SandboxManager::new().unwrap();
        let double = r#"(module
  (import "env" "field_pulse" (func $field_pulse (param f64 f64 f64 f64 f64) (result i32)))
  (memory (export "memory") 1)
  (global $r1 (mut i32) (i32.const -1))
  (global $r2 (mut i32) (i32.const -1))
  (func (export "on_tick")
    (global.set $r1 (call $field_pulse (f64.const 1.0) (f64.const 1.0) (f64.const 1.0) (f64.const 1.0) (f64.const 2.0)))
    (global.set $r2 (call $field_pulse (f64.const 2.0) (f64.const 1.0) (f64.const 1.0) (f64.const 1.0) (f64.const 2.0))))
  (func (export "get_r1") (result i32) (global.get $r1))
  (func (export "get_r2") (result i32) (global.get $r2))
  (func (export "life") (result i32) (i32.const 1)))"#;
        let mut handle = mgr.create_sandbox(&wasm(double), 100_000, id(836)).unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_r1"), 1, "首脉冲排队");
        assert_eq!(call_i32_export(&mut handle, "get_r2"), 0, "第二脉冲限流拒绝");
        assert_eq!(handle.store.data().pending_field_pulses.len(), 1);
    }

    // === D-003 motion 出生声明（spawn 第 10 参）===

    #[test]
    fn spawn_import_passes_motion_through_and_clamps_invalid() {
        // D-003 第 10 参 motion：1=dynamic 透传；非法值（7/2）钳 static——
        // 坏声明不进世界，回退现状语义（与 binding 同款口径）
        let mgr = SandboxManager::new().unwrap();
        let creator = id(811);
        let wat_text = spawner_full_wat(
            SPAWNED_CODE,
            "f64.const 2.0",
            "f64.const 3.0",
            "i32.const 0",
            "i32.const 1",
        );
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, creator)
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(
            handle.store.data().pending_spawns[0].motion,
            codeworld_core::entity::MOTION_DYNAMIC,
            "合法值 1 应透传 dynamic"
        );
        // 非法值 7（含 kinematic 预留位 2）钳 static
        for bad in ["i32.const 7", "i32.const 2"] {
            handle.store.data_mut().pending_spawns.clear();
            let bad_text = spawner_full_wat(
                SPAWNED_CODE,
                "f64.const 2.0",
                "f64.const 3.0",
                "i32.const 0",
                bad,
            );
            let mut h = mgr
                .create_sandbox(&wasm(&bad_text), 100_000, id(812))
                .unwrap();
            h.store.data_mut().pos = (10.0, 10.0);
            mgr.call_on_tick(&mut h).unwrap();
            assert_eq!(
                h.store.data().pending_spawns[0].motion,
                codeworld_core::entity::MOTION_STATIC,
                "非法 motion（{bad}）应钳 static"
            );
        }
    }

    #[test]
    fn spawn_import_motion_defaults_to_static_when_zero() {
        // 边界：motion=0（显式钉死）与省略语义等价——PendingSpawn.motion=static
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(
                &wasm(&spawner_wat(SPAWNED_CODE, "f64.const 2.0", "f64.const 3.0")),
                100_000,
                id(813),
            )
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(
            handle.store.data().pending_spawns[0].motion,
            codeworld_core::entity::MOTION_STATIC,
            "默认档（0）应排队 static"
        );
    }

    // === 出生初速（spawn 第 11/12 参，D-018 一体式发射提前落地）===

    #[test]
    fn spawn_import_rejects_birth_velocity_over_max() {
        // 出生初速模长 28.28 > 世界极速 25 → 拒绝返回 0，不入队（恰等放行口径）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(
                &wasm(&spawner_velocity_wat(
                    SPAWNED_CODE,
                    "f64.const 2.0",
                    "f64.const 3.0",
                    "i32.const 0",
                    "i32.const 1",
                    "f64.const 20.0",
                    "f64.const 20.0",
                )),
                100_000,
                id(821),
            )
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let ret = call_i32_export(&mut handle, "get_ret");
        assert_eq!(ret, 0, "超极速初速应拒绝返回 0");
        assert!(
            handle.store.data().pending_spawns.is_empty(),
            "超极速初速不应入队"
        );
    }

    #[test]
    fn spawn_import_allows_birth_velocity_exactly_max() {
        // 边界：初速恰等于世界极速 25 放行（严格 > 判据）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(
                &wasm(&spawner_velocity_wat(
                    SPAWNED_CODE,
                    "f64.const 2.0",
                    "f64.const 3.0",
                    "i32.const 0",
                    "i32.const 1",
                    "f64.const 25.0",
                    "f64.const 0.0",
                )),
                100_000,
                id(822),
            )
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let ret = call_i32_export(&mut handle, "get_ret");
        assert_eq!(ret, 1, "恰等于极速应放行（严格 > 判据）");
        assert_eq!(
            handle.store.data().pending_spawns.len(),
            1,
            "恰极速初速应入队"
        );
    }

    #[test]
    fn spawn_import_rejects_nonfinite_birth_velocity() {
        // NaN 初速拒绝：speed2 含 NaN 时 > 比较恒 false，必须显式有限性校验拦下
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(
                &wasm(&spawner_velocity_wat(
                    SPAWNED_CODE,
                    "f64.const 2.0",
                    "f64.const 3.0",
                    "i32.const 0",
                    "i32.const 1",
                    "f64.const nan",
                    "f64.const 0.0",
                )),
                100_000,
                id(823),
            )
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let ret = call_i32_export(&mut handle, "get_ret");
        assert_eq!(ret, 0, "NaN 初速应拒绝返回 0");
        assert!(
            handle.store.data().pending_spawns.is_empty(),
            "NaN 初速不应入队"
        );
    }

    #[test]
    fn spawn_import_passes_birth_velocity_through() {
        // 合法初速 (3, -4)（模长 5 < 25）→ PendingSpawn 原样透传
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(
                &wasm(&spawner_velocity_wat(
                    SPAWNED_CODE,
                    "f64.const 2.0",
                    "f64.const 3.0",
                    "i32.const 0",
                    "i32.const 1",
                    "f64.const 3.0",
                    "f64.const -4.0",
                )),
                100_000,
                id(824),
            )
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns.len(), 1, "合法初速应入队");
        assert_eq!(spawns[0].vx, 3.0, "vx 应透传");
        assert_eq!(spawns[0].vy, -4.0, "vy 应透传");
    }

    // === 出生数据包（spawn 第 13/14 参 blob_ptr/blob_len，D-017 挂靠立法）===

    /// 带出生数据包的造物方 WAT：blob 数据段放 8500（外观段 8256 之后），
    /// blob_ptr/blob_len 以 WAT 表达式传入（超限用例传 i32.const 65537 即可——
    /// 拒绝发生在读内存前，无需真备 64KB 数据）
    fn spawner_blob_wat(
        code: &str,
        x: &str,
        y: &str,
        blob_data: &[u8],
        blob_ptr: &str,
        blob_len: &str,
    ) -> String {
        let escaped = wat_escape(code);
        let blob_escaped: String = blob_data.iter().map(|b| format!("\\{b:02x}")).collect();
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (data (i32.const 8500) "{blob_escaped}")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    {x} {y}
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 0.0
    {blob_ptr} {blob_len}
    f64.const 0.0
    f64.const 1.0
    f64.const 0.0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    call $spawn global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = code.len(),
            x = x,
            y = y,
            blob_ptr = blob_ptr,
            blob_len = blob_len,
        )
    }

    #[test]
    fn spawn_import_reads_birth_blob_from_memory() {
        // blob_len 5（1..=64KB）→ 从 caller memory 原样读字节入队（世界不解释内容）
        let mgr = SandboxManager::new().unwrap();
        let blob = [0xDEu8, 0xAD, 0xBE, 0xEF, 0x42];
        let mut handle = mgr
            .create_sandbox(
                &wasm(&spawner_blob_wat(
                    SPAWNED_CODE,
                    "f64.const 2.0",
                    "f64.const 3.0",
                    &blob,
                    "i32.const 8500",
                    "i32.const 5",
                )),
                100_000,
                id(831),
            )
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "合法 blob 应放行");
        assert_eq!(
            handle.store.data().pending_spawns[0].blob,
            blob.to_vec(),
            "blob 应从 memory 原样读入"
        );
    }

    #[test]
    fn spawn_import_empty_blob_len_zero_is_legal() {
        // blob_len=0 → 空数据包合法（无包语义），spawn 照常入队
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(
                &wasm(&spawner_blob_wat(
                    SPAWNED_CODE,
                    "f64.const 2.0",
                    "f64.const 3.0",
                    &[],
                    "i32.const 0",
                    "i32.const 0",
                )),
                100_000,
                id(832),
            )
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "空包应合法放行");
        assert!(
            handle.store.data().pending_spawns[0].blob.is_empty(),
            "空包 blob 应为空"
        );
    }

    #[test]
    fn spawn_import_negative_blob_len_is_empty_packet() {
        // blob_len=-1 → 合法（负长度同空包语义，不读内存）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(
                &wasm(&spawner_blob_wat(
                    SPAWNED_CODE,
                    "f64.const 2.0",
                    "f64.const 3.0",
                    &[],
                    "i32.const 0",
                    "i32.const -1",
                )),
                100_000,
                id(833),
            )
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "负长度应合法放行");
        assert!(
            handle.store.data().pending_spawns[0].blob.is_empty(),
            "负长度 blob 应为空"
        );
    }

    #[test]
    fn spawn_import_rejects_oversized_blob() {
        // blob_len=65537 > 64KB 上限 → 整次拒绝（读内存前拦下，不裁剪——
        // 裁剪数据包=偷改玩家数据）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(
                &wasm(&spawner_blob_wat(
                    SPAWNED_CODE,
                    "f64.const 2.0",
                    "f64.const 3.0",
                    &[],
                    "i32.const 0",
                    "i32.const 65537",
                )),
                100_000,
                id(834),
            )
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "超限 blob 应整次拒绝");
        assert!(
            handle.store.data().pending_spawns.is_empty(),
            "超限 blob 不应入队"
        );
    }

    #[test]
    fn spawn_import_allows_blob_exactly_64k() {
        // 边界：blob_len=65536 恰等上限放行（blob 内容=零页初始内存，只锚长度）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(
                &wasm(&spawner_blob_wat(
                    SPAWNED_CODE,
                    "f64.const 2.0",
                    "f64.const 3.0",
                    &[],
                    "i32.const 0",
                    "i32.const 65536",
                )),
                100_000,
                id(835),
            )
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "恰等 64KB 应放行");
        assert_eq!(
            handle.store.data().pending_spawns[0].blob.len(),
            65536,
            "恰等上限的 blob 应完整读入"
        );
    }

    // === env.birth_blob import（第 13 个玩家 import：造物读回自己的出生数据包）===

    /// birth_blob 读取方 WAT：data 段 [2048] 放哨兵 \77（"未写任何字节"断言锚点），
    /// on_tick 调 birth_blob(ptr, cap) 返回值存全局
    fn birth_blob_reader_wat(ptr: &str, cap: &str) -> String {
        format!(
            r#"(module
  (import "env" "birth_blob" (func $birth_blob (param i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 2048) "\77")
  (func (export "on_tick")
    {ptr} {cap}
    call $birth_blob global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#
        )
    }

    /// 读沙箱线性内存 [start..start+len)（birth_blob 写入侧断言用）
    fn read_memory(handle: &mut SandboxHandle, start: usize, len: usize) -> Vec<u8> {
        let memory = handle
            .instance
            .get_memory(&mut handle.store, "memory")
            .expect("测试模块应导出 memory");
        memory.data(&handle.store)[start..start + len].to_vec()
    }

    #[test]
    fn birth_blob_import_returns_zero_when_no_packet() {
        // 无出生数据包（默认空）→ 返回 0，不写 memory（哨兵字节原样）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&birth_blob_reader_wat("i32.const 3000", "i32.const 64")), 100_000, id(841))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "无包应返回 0");
        assert_eq!(read_memory(&mut handle, 2048, 1), vec![0x77], "无包不应写 memory");
    }

    #[test]
    fn birth_blob_import_writes_and_returns_len() {
        // 有包 + cap 足够 → 原样写入 [ptr..ptr+N)，返回 N（原子整体写入）
        let mgr = SandboxManager::new().unwrap();
        let blob = vec![0x11u8, 0x22, 0x33, 0x44, 0x55];
        let mut handle = mgr
            .create_sandbox(&wasm(&birth_blob_reader_wat("i32.const 3000", "i32.const 64")), 100_000, id(842))
            .unwrap();
        handle.store.data_mut().birth_blob = blob.clone();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 5, "写入成功应返回 N=5");
        assert_eq!(read_memory(&mut handle, 3000, 5), blob, "blob 应原样写入 memory");
        assert_eq!(read_memory(&mut handle, 3005, 1), vec![0], "写入不应越过 N 边界");
        assert_eq!(read_memory(&mut handle, 2048, 1), vec![0x77], "哨兵不应受扰");
    }

    #[test]
    fn birth_blob_import_returns_negative_when_cap_short() {
        // cap 不足（2 < 5）→ 返回 -N=-5，未写任何字节（玩家按 64KB 备缓冲一次必成）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&birth_blob_reader_wat("i32.const 3000", "i32.const 2")), 100_000, id(843))
            .unwrap();
        handle.store.data_mut().birth_blob = vec![0xAAu8; 5];
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), -5, "cap 不足应返回 -N");
        assert_eq!(read_memory(&mut handle, 3000, 1), vec![0], "cap 不足不应写 memory");
    }

    #[test]
    fn birth_blob_import_returns_negative_when_cap_zero() {
        // cap=0 → 容量不足路径（返回 -N，不写）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&birth_blob_reader_wat("i32.const 3000", "i32.const 0")), 100_000, id(844))
            .unwrap();
        handle.store.data_mut().birth_blob = vec![0xAAu8; 5];
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), -5, "cap=0 应返回 -N");
    }

    #[test]
    fn birth_blob_import_returns_negative_when_ptr_illegal() {
        // 指针越界（70000 > 64KB 页）→ 返回 -N，未写任何字节
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&birth_blob_reader_wat("i32.const 70000", "i32.const 64")), 100_000, id(845))
            .unwrap();
        handle.store.data_mut().birth_blob = vec![0xAAu8; 5];
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), -5, "指针非法应返回 -N");
    }

    // === 绑定契约（#034 修订四：spawn 第 9 参；shoot 第 6 参随 D-018 退役）===

    #[test]
    fn spawn_import_passes_binding_through_and_clamps_invalid() {
        // spawn 第 9 参：合法值透传（1=fade_on_death 塔随主消亡）；非法值钳 unbound
        let mgr = SandboxManager::new().unwrap();
        let creator = id(801);
        let wat_text =
            spawner_binding_wat(SPAWNED_CODE, "f64.const 2.0", "f64.const 3.0", "i32.const 1");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, creator)
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(handle.store.data().pending_spawns[0].binding, 1, "合法契约值应透传");
        // 非法值
        handle.store.data_mut().pending_spawns.clear();
        let bad_text =
            spawner_binding_wat(SPAWNED_CODE, "f64.const 2.0", "f64.const 3.0", "i32.const 7");
        let mut h2 = mgr
            .create_sandbox(&wasm(&bad_text), 100_000, id(802))
            .unwrap();
        h2.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut h2).unwrap();
        assert_eq!(
            h2.store.data().pending_spawns[0].binding, 0,
            "非法契约值应钳 unbound"
        );
    }


    #[test]
    fn spawn_import_rejects_second_request_same_tick() {
        // 限流：每实体每 tick 最多 1 个 spawn 请求，第二次调用返回 0 不入队
        let mgr = SandboxManager::new().unwrap();
        let wat_text = double_spawner_wat(SPAWNED_CODE);
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(802))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_r1"), 1, "第一次应排队成功");
        assert_eq!(call_i32_export(&mut handle, "get_r2"), 0, "第二次应被限流拒绝");
        assert_eq!(
            handle.store.data().pending_spawns.len(),
            1,
            "同 tick 只应有 1 个请求入队"
        );
    }

    #[test]
    fn spawn_import_rejects_birth_out_of_creator_range() {
        // 位置起点限制：出生点超出创造者快照 ±10 格 -> 拒绝（防远程投射刷屏，
        // MVP 一刀切简单规则；规则由宪法允许后续玩法化，#012）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = spawner_wat(SPAWNED_CODE, "f64.const 21.0", "f64.const 10.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(803))
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "越界出生点应被拒绝");
        assert!(
            handle.store.data().pending_spawns.is_empty(),
            "被拒绝的造物请求不应入队"
        );
    }

    #[test]
    fn spawn_import_rejects_underground_birth() {
        // 出生点 y<0 钻地 -> 拒绝（默认物理地面 y=0，与 move_by 同语义）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = spawner_wat(SPAWNED_CODE, "f64.const 12.0", "f64.const -1.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(804))
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "钻地出生点应被拒绝");
    }

    #[test]
    fn spawn_import_rejects_non_finite_birth() {
        // 出生点非有限（NaN）-> 拒绝，防污染世界状态（与 move_by NaN 同款防御）。
        // WAT 无 NaN 字面量直接写法缺口：0.0/0.0 产生 NaN（浮点除零不 trap）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = spawner_wat(SPAWNED_CODE, "f64.const 0.0 f64.const 0.0 f64.div", "f64.const 0.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(805))
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "NaN 出生点应被拒绝");
        assert!(handle.store.data().pending_spawns.is_empty(), "NaN 请求不应入队");
    }

    #[test]
    fn spawn_import_rejects_empty_code() {
        // code_len=0（没有代码）-> 拒绝：造物是活实体，必须有可编译的代码
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(EMPTY_CODE_SPAWNER_WAT), 100_000, id(806))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "空代码应被拒绝");
        assert!(handle.store.data().pending_spawns.is_empty(), "空代码请求不应入队");
    }

    #[test]
    fn spawn_import_rejects_multi_char_glyph() {
        // 字形必须是单字符（渲染层一实体一字符），"TT" -> 拒绝
        let mgr = SandboxManager::new().unwrap();
        let wat_text = bad_glyph_spawner_wat(SPAWNED_CODE);
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(807))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "多字符字形应被拒绝");
        assert!(handle.store.data().pending_spawns.is_empty(), "坏字形请求不应入队");
    }

    /// 销毁方 WAT：on_tick 调 destroy(id_lo, id_hi)，返回值存全局。
    /// 目标 id = 0x0102030405060708_FFFFFFFFFFFFFFFF（hi 半含全 1 低半，
    /// 锁定 i64 -> u64 的 as 截断语义：WAT 侧 -1 表示 u64::MAX）。
    const DESTROYER_WAT: &str = r#"(module
  (import "env" "destroy" (func $destroy (param i64 i64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    i64.const -1 i64.const 0x0102030405060708 call $destroy global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn destroy_import_queues_pending_destroy() {
        // env.destroy：EntityId 以 i64 两半传递（u128 拆 lo/hi；WAT 侧负 i64 经 as
        // 恢复位型）。import 只排队（产权校验在阶段二，import 拿不到 world）。
        let mgr = SandboxManager::new().unwrap();
        let destroyer = id(808);
        let mut handle = mgr.create_sandbox(&wasm(DESTROYER_WAT), 100_000, destroyer).unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "合法 destroy 应返回 1");
        let destroys = &handle.store.data().pending_destroys;
        assert_eq!(destroys.len(), 1, "应排队 1 个销毁请求");
        assert_eq!(destroys[0].destroyer, destroyer, "发起者应为沙箱绑定实体");
        assert_eq!(
            destroys[0].target,
            EntityId(Uuid::from_u128(0x0102_0304_0506_0708_FFFF_FFFF_FFFF_FFFF)),
            "目标 id 应由 i64 两半无失真重建"
        );
        assert!(
            handle.store.data().api_calls.contains(&"destroy".to_string()),
            "destroy 调用应记入 api_calls 元数据"
        );
    }

    // === env.hover import（期四第七刀：玩家代码每拍悬停声明，接入物理免重力集合）===

    /// 悬停声明 WAT：on_tick 调 hover()（无参无返回）
    const HOVER_WAT: &str = r#"(module
  (import "env" "hover" (func $hover))
  (global $called (mut i32) (i32.const 0))
  (func (export "on_tick")
    call $hover
    global.get $called i32.const 1 i32.add global.set $called)
  (func (export "get_called") (result i32) global.get $called)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn hover_import_marks_declaration_for_current_tick() {
        // hover()：玩家代码声明"本拍我要悬停"——import 置位 SandboxData.hover_declared，
        // host 阶段一收件后并入本拍免重力集合（与操控悬停同集合同机制，#030 升力独立；
        // #029 无税：悬停是持续对抗世界规则的代码声明，每拍需重申，不延续）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(HOVER_WAT), 100_000, id(901)).unwrap();
        assert!(
            !handle.store.data().hover_declared,
            "未调用 hover 前声明位应为 false"
        );
        mgr.call_on_tick(&mut handle).unwrap();
        assert!(
            handle.store.data().hover_declared,
            "on_tick 调 hover 后声明位应为 true"
        );
        assert!(
            handle.store.data().api_calls.contains(&"hover".to_string()),
            "hover 调用应记入 api_calls 元数据"
        );
    }

    // === env.thrust import（D-010a 冲量小刀：本拍给自己一记冲量，耗 fuel）===

    /// 满力跳 WAT：on_tick 调 thrust(0, 7.7)（跳 ≤3 格高锚点值）
    const THRUSTER_WAT: &str = r#"(module
  (import "env" "thrust" (func $thrust (param f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const 0))
  (func (export "on_tick")
    f64.const 0.0 f64.const 7.7 call $thrust global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn thrust_import_queues_pending_thrust() {
        // thrust(0, 7.7)：排队 1 条冲量请求（满力=跳 ≤3 格高锚点值），
        // api_calls 记录元数据
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(THRUSTER_WAT), 100_000, id(911)).unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "合法 thrust 应返回 1");
        let thrusts = &handle.store.data().pending_thrusts;
        assert_eq!(thrusts.len(), 1, "应排队 1 个冲量请求");
        assert_eq!(thrusts[0].entity, id(911), "冲量应绑定沙箱实体");
        assert!(
            (thrusts[0].impulse.0 - 0.0).abs() < 1e-9 && (thrusts[0].impulse.1 - 7.7).abs() < 1e-9,
            "满力冲量应原样入队 (0, 7.7)，实际 {:?}",
            thrusts[0].impulse
        );
        assert!(
            handle.store.data().api_calls.contains(&"thrust".to_string()),
            "thrust 调用应记入 api_calls 元数据"
        );
        // fuel 记账：满力一次 = fuel_limit × 5% = 5_000
        assert_eq!(
            handle.store.data().thrust_fuel_cost, 5_000,
            "满力出力应记账 fuel_limit×5%"
        );
    }

    #[test]
    fn thrust_import_rejects_non_finite() {
        // NaN/Inf 输入按 0 拒绝：冲量进速度字段会污染世界状态
        let wat = r#"(module
  (import "env" "thrust" (func $thrust (param f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const 0))
  (func (export "on_tick")
    f64.const nan f64.const 1.0 call $thrust global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(wat), 100_000, id(912)).unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(
            call_i32_export(&mut handle, "get_ret"),
            0,
            "NaN 冲量应被拒绝返回 0"
        );
        assert!(
            handle.store.data().pending_thrusts.is_empty(),
            "拒绝的冲量不入队"
        );
    }

    #[test]
    fn thrust_import_clamps_over_limit_keeping_direction() {
        // 超上限归一化钳制：thrust(0, 77)（模长 77 > 7.7）压回 (0, 7.7) 方向不变；
        // 次满力（模长 5 < 7.7）原样入队不钳
        let wat = r#"(module
  (import "env" "thrust" (func $thrust (param f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const 0))
  (func (export "on_tick")
    f64.const 0.0 f64.const 77.0 call $thrust drop
    f64.const 3.0 f64.const 4.0 call $thrust global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(wat), 100_000, id(913)).unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "第二次 thrust 合法应返回 1");
        let thrusts = &handle.store.data().pending_thrusts;
        assert_eq!(thrusts.len(), 2, "两次调用都应入队");
        assert!(
            (thrusts[0].impulse.1 - 7.7).abs() < 1e-9,
            "超限冲量应归一化压回 7.7（方向不变），实际 {:?}",
            thrusts[0].impulse
        );
        assert!(
            (thrusts[1].impulse.0 - 3.0).abs() < 1e-9 && (thrusts[1].impulse.1 - 4.0).abs() < 1e-9,
            "次满力冲量应原样入队，实际 {:?}",
            thrusts[1].impulse
        );
    }

    #[test]
    fn thrust_import_rejects_when_fuel_insufficient() {
        // fuel 粗校验：活体剩余 fuel 低于本次出力代价则拒绝返回 0。
        // 构造：fuel_limit=1_000_000（满力代价 50_000），set_fuel(500) 模拟
        // on_tick 烧得只剩零头——500 远小于 50_000，拒绝；且不在 wasm 内烧穿
        // fuel（.cc-bugs #5 红线：不用 out-of-fuel trap 构造测试）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(THRUSTER_WAT), 1_000_000, id(914)).unwrap();
        handle.store.set_fuel(500).unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(
            call_i32_export(&mut handle, "get_ret"),
            0,
            "剩余 fuel 不足应拒绝返回 0"
        );
        assert!(
            handle.store.data().pending_thrusts.is_empty(),
            "fuel 不足的冲量不入队"
        );
        assert_eq!(
            handle.store.data().thrust_fuel_cost, 0,
            "拒绝的出力不记账 fuel 代价"
        );
    }

    // === 决斗制攻杀 D-005 刀0：内存映射直连 + 游魂窗口（design-duel）===

    /// 决斗守方靶 WAT：导出 memory + 头部 16 字节魔数（游魂的侦察目标）
    const DUEL_DEFENDER_WAT: &str = r#"(module
  (memory (export "memory") 1)
  (data (i32.const 0) "DEFENDER-MAGIC01")
  (func (export "life") (result i32) i32.const 1))"#;

    /// 决斗游魂 WAT：读守方 [0,16) 进自己内存、把读到的东西写回守方 [64,80)、
    /// 经决斗通道把当前窗口拍号写在守方 [48]，返回 0（继续）
    const DUEL_GHOST_WAT: &str = r#"(module
  (import "env" "duel_read" (func $duel_read (param i32 i32 i32) (result i32)))
  (import "env" "duel_write" (func $duel_write (param i32 i32 i32) (result i32)))
  (import "env" "duel_round" (func $duel_round (result i32)))
  (memory (export "memory") 1)
  (func (export "on_duel") (result i32)
    (drop (call $duel_read (i32.const 0) (i32.const 16) (i32.const 0)))
    (drop (call $duel_write (i32.const 64) (i32.const 0) (i32.const 16)))
    (i32.store8 (i32.const 32) (call $duel_round))
    (drop (call $duel_write (i32.const 48) (i32.const 32) (i32.const 1)))
    (i32.const 0)))"#;

    /// 决斗游魂 WAT（收工版）：同上但返回 1（宣布完成，会话正常收尾）
    const DUEL_GHOST_DONE_WAT: &str = r#"(module
  (import "env" "duel_read" (func $duel_read (param i32 i32 i32) (result i32)))
  (import "env" "duel_write" (func $duel_write (param i32 i32 i32) (result i32)))
  (import "env" "duel_round" (func $duel_round (result i32)))
  (memory (export "memory") 1)
  (func (export "on_duel") (result i32)
    (drop (call $duel_read (i32.const 0) (i32.const 16) (i32.const 0)))
    (i32.const 1)))"#;

    /// 违规游魂 WAT：除了三个决斗 import 还想用 env.log（最小环境应拒绝实例化）
    const DUEL_GHOST_SMUGGLER_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "duel_read" (func $duel_read (param i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "on_duel") (result i32) (i32.const 0)))"#;

    fn duel_stage() -> (SandboxManager, SandboxHandle, SandboxHandle) {
        let mgr = SandboxManager::new().unwrap();
        let defender = mgr
            .create_sandbox(&wasm(DUEL_DEFENDER_WAT), 1_000_000, id(901))
            .unwrap();
        let plain = mgr
            .create_sandbox(&wasm(TARGET_WAT_ARENA), 1_000_000, id(902))
            .unwrap();
        (mgr, defender, plain)
    }

    /// 无决斗 import 的素模块（游魂缺 on_duel 导出的崩溃构造）
    const TARGET_WAT_ARENA: &str = r#"(module (memory (export "memory") 1))"#;

    #[test]
    fn duel_read_write_primitives_normal_and_oob() {
        // 交付2：决斗读/写原语——正常路径读写守方内存；越界 Err；守方无 memory Err
        let (mgr, mut defender, _plain) = duel_stage();
        // 正常写：把字节写进守方 [100, 105)
        mgr.duel_write(&mut defender, 100, b"HELLO").unwrap();
        // 正常读：读回来一致（读就是读，写就是写，中间无世界代码）
        let bytes = mgr.duel_read(&mut defender, 100, 5).unwrap();
        assert_eq!(bytes, b"HELLO", "决斗读应取回刚写入的字节");
        // 越界读：ptr+len 超出线性内存 -> Err
        let size = {
            let mem = defender.instance.get_memory(&mut defender.store, "memory").unwrap();
            mem.data_size(&defender.store)
        };
        assert!(mgr.duel_read(&mut defender, size as u32, 1).is_err(), "读越界应 Err");
        assert!(mgr.duel_write(&mut defender, size as u32, b"x").is_err(), "写越界应 Err");
        // 末尾恰好压线：[size-1, size) 合法（闭区间外才越界）
        mgr.duel_read(&mut defender, size as u32 - 1, 1).unwrap();
        // 守方未导出 memory -> Err（布景没有可映射的内存）
        let mut memless = mgr.create_sandbox(&wasm(DEAD_PLAYER_WAT_ARENA), 1_000, id(903)).unwrap();
        assert!(mgr.duel_read(&mut memless, 0, 1).is_err(), "无 memory 守方读取应 Err");
        assert!(mgr.duel_write(&mut memless, 0, b"x").is_err(), "无 memory 守方写入应 Err");
    }

    /// 无 memory 的最小模块（布景构造用）
    const DEAD_PLAYER_WAT_ARENA: &str = r#"(module (func (export "life") (result i32) i32.const 0))"#;

    #[test]
    fn call_duel_step_ghost_reads_and_writes_defender_memory() {
        // 交付3：游魂窗口——游魂读到守方魔数、写回守方、拿到拍号；返回 0=继续
        let (mgr, mut defender, _) = duel_stage();
        let ghost = wasm(DUEL_GHOST_WAT);
        let outcome = mgr.call_duel_step(&ghost, &mut defender, 10_000, 0).unwrap();
        assert_eq!(outcome.outcome, DuelVerdict::Continue, "返回 0 = 继续会话");
        assert!(outcome.fuel_used > 0, "游魂窗口应烧 fuel（烧的是攻方赌注）");
        assert!(outcome.fuel_used < 10_000, "轻量游魂不应烧穿窗口预算");
        // 游魂把守方魔数写回了守方 [64, 80)——读->写全链证据
        let echoed = mgr.duel_read(&mut defender, 64, 16).unwrap();
        assert_eq!(echoed, b"DEFENDER-MAGIC01", "游魂应把读到的守方内存写回守方");
        // 游魂拿到的拍号 = 传入的 round
        let round_byte = mgr.duel_read(&mut defender, 48, 1).unwrap();
        assert_eq!(round_byte[0], 0, "round=0 窗口，duel_round 应返回 0");
    }

    #[test]
    fn call_duel_step_round_number_flows_through() {
        // 交付3：duel_round 返回当前窗口拍号（游魂可据此做轮次策略）
        let (mgr, mut defender, _) = duel_stage();
        let ghost = wasm(DUEL_GHOST_WAT);
        mgr.call_duel_step(&ghost, &mut defender, 10_000, 7).unwrap();
        let round_byte = mgr.duel_read(&mut defender, 48, 1).unwrap();
        assert_eq!(round_byte[0], 7, "游魂应看到第 7 号窗口");
    }

    #[test]
    fn call_duel_step_done_vote_finishes() {
        // 交付3：游魂返回 1 = 宣布完成（会话正常收尾的投票）
        let (mgr, mut defender, _) = duel_stage();
        let ghost = wasm(DUEL_GHOST_DONE_WAT);
        let outcome = mgr.call_duel_step(&ghost, &mut defender, 10_000, 3).unwrap();
        assert_eq!(outcome.outcome, DuelVerdict::Done, "返回 1 = Done");
    }

    #[test]
    fn call_duel_step_garbage_module_is_crash() {
        // 交付4 负路径：游魂 payload 是垃圾字节 -> 实例化失败 = 崩溃（Err，会话由调用方关闭）
        let (mgr, mut defender, _) = duel_stage();
        let garbage: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
        assert!(
            mgr.call_duel_step(garbage, &mut defender, 10_000, 0).is_err(),
            "垃圾模块实例化失败应 Err"
        );
        // 守方无损：魔数原样
        assert_eq!(mgr.duel_read(&mut defender, 0, 16).unwrap(), b"DEFENDER-MAGIC01");
    }

    #[test]
    fn call_duel_step_missing_on_duel_is_crash() {
        // 交付4 负路径：游魂没导出 on_duel = 崩溃（约定导出名缺失）
        let (mgr, mut defender, plain) = duel_stage();
        let mut plain = plain;
        assert!(
            mgr.call_duel_step(&wasm(TARGET_WAT_ARENA), &mut defender, 10_000, 0).is_err(),
            "缺 on_duel 导出应 Err"
        );
        assert!(mgr.duel_read(&mut plain, 0, 1).is_ok(), "旁观沙箱不受影响");
    }

    #[test]
    fn duel_ghost_import_env_is_minimal_three() {
        // 交付3：游魂 import 环境最小集=三个决斗原语——走私 env.log 的模块拒绝实例化
        let (mgr, mut defender, _) = duel_stage();
        assert!(
            mgr.call_duel_step(&wasm(DUEL_GHOST_SMUGGLER_WAT), &mut defender, 10_000, 0)
                .is_err(),
            "世界不给游魂多余通道（通信自由，注入无门——进来只有决斗三原语）"
        );
    }

    #[test]
    fn duel_step_burns_stake_not_defender_fuel() {
        // 账本铁律 #046：游魂烧攻方赌注，守方 fuel 分毫不动（host 侧 memory 操作
        // 会被 wasmtime 计量进守方油箱——守卫快照/恢复必须抹平）
        let (mgr, mut defender, _) = duel_stage();
        let before = defender.store.get_fuel().unwrap();
        let ghost = wasm(DUEL_GHOST_WAT);
        let outcome = mgr.call_duel_step(&ghost, &mut defender, 10_000, 0).unwrap();
        let after = defender.store.get_fuel().unwrap();
        assert_eq!(before, after, "守方 fuel 不应因决斗窗口变化（账户跟人不跟房子）");
        assert!(outcome.fuel_used > 0, "游魂自己烧自己的预算");
    }

    // === D-011 世界内链路段：spawn 第 20 参 link + 五 import + 三回调投递 ===

    /// 带链声明的造物方 WAT（D-011 spawn 第 20 参）：link 表达式直传
    fn spawner_link_wat(code: &str, link: &str) -> String {
        let escaped = wat_escape(code);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "K\00")
  (data (i32.const 8256) "lime\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const 1.0 f64.const 1.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 1.0
    f64.const 0.0
    f64.const 0.0
    f64.const 0.0
    {link}
    call $spawn global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = code.len(),
            link = link,
        )
    }

    /// command 广播方 WAT：on_tick 调 command(0, 6)（data 段 "TURN+2" 6 字节）
    const COMMANDER_WAT: &str = r#"(module
  (import "env" "command" (func $command (param i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "TURN+2")
  (func (export "on_tick")
    i32.const 0 i32.const 6 call $command global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    /// report 上行方 WAT：on_tick 调 report(0, 6)（data 段 "STATUS" 6 字节）
    const REPORTER_WAT: &str = r#"(module
  (import "env" "report" (func $report (param i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "STATUS")
  (func (export "on_tick")
    i32.const 0 i32.const 6 call $report global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    /// send 横向信 WAT：on_tick 调 send(7.0, 8.0, 0, 5)（data 段 "HELLO" 5 字节）
    const SENDER_WAT: &str = r#"(module
  (import "env" "send" (func $send (param f64 f64 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "HELLO")
  (func (export "on_tick")
    f64.const 7.0 f64.const 8.0 i32.const 0 i32.const 5 call $send global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    /// storage_write 方 WAT：on_tick 调 storage_write(0, 6)（data 段 "INTEL1" 6 字节）
    const STORAGE_WRITER_WAT: &str = r#"(module
  (import "env" "storage_write" (func $storage_write (param i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "INTEL1")
  (func (export "on_tick")
    i32.const 0 i32.const 6 call $storage_write global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    /// storage_read 方 WAT：on_tick 调 storage_read(7.0, 8.0, 100, 64)，返回值存
    /// 全局；哨兵字节 @[2048]（"未写任何字节"断言锚）
    fn storage_reader_wat(tx: &str, ty: &str) -> String {
        format!(
            r#"(module
  (import "env" "storage_read" (func $storage_read (param f64 f64 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 2048) "\77")
  (func (export "on_tick")
    f64.const {tx} f64.const {ty} i32.const 100 i32.const 64
    call $storage_read global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "get_first") (result i32) (i32.load8_u (i32.const 100)))
  (func (export "life") (result i32) i32.const 1))"#,
            tx = tx,
            ty = ty,
        )
    }

    /// 链快照预置 helper：模拟 host 拍首填充（children/parent/radius/peers/own/
    /// 上限装配——import 闭包是 'static 拿不到 world，一切经 SandboxData 快照）
    fn fill_link_snapshot(
        handle: &mut SandboxHandle,
        children: u32,
        parent: Option<EntityId>,
        own: &[u8],
        peers: Vec<LinkPeer>,
        payload_max: usize,
        cap: usize,
    ) {
        let d = handle.store.data_mut();
        d.link_children = children;
        d.link_parent = parent;
        d.link_radius = 0.5;
        d.link_storage_own = own.to_vec();
        d.link_peers = peers;
        d.link_payload_max = payload_max;
        d.storage_cap = cap;
    }

    fn link_peer(id_u: u128, pos: (f64, f64), storage: &[u8]) -> LinkPeer {
        LinkPeer {
            id: id(id_u),
            pos,
            radius: 0.5,
            storage: Arc::new(storage.to_vec()),
        }
    }

    #[test]
    fn spawn_import_passes_link_through_and_clamps_invalid() {
        // spawn 第 20 参 link：1=活链透传；非法值（7/2/-1）钳无链——坏声明不进
        // 世界，回退放生现状（与 binding/motion 同款口径）
        let mgr = SandboxManager::new().unwrap();
        let creator = id(880);
        let wat_text = spawner_link_wat(SPAWNED_CODE, "i32.const 1");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, creator)
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(
            handle.store.data().pending_spawns[0].link,
            codeworld_core::entity::LINK_ACTIVE,
            "合法值 1 应透传活链"
        );
        for bad in ["i32.const 7", "i32.const 2", "i32.const -1"] {
            handle.store.data_mut().pending_spawns.clear();
            let bad_text = spawner_link_wat(SPAWNED_CODE, bad);
            let mut h = mgr
                .create_sandbox(&wasm(&bad_text), 100_000, id(881))
                .unwrap();
            h.store.data_mut().pos = (10.0, 10.0);
            mgr.call_on_tick(&mut h).unwrap();
            assert_eq!(
                h.store.data().pending_spawns[0].link,
                codeworld_core::entity::LINK_UNLINKED,
                "非法 link（{bad}）应钳无链"
            );
        }
    }

    #[test]
    fn command_import_without_children_returns_zero() {
        // 链三态·放生：无活链孩子（快照 children=0）-> command 返回 0 不入队
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(COMMANDER_WAT), 10_000, id(882)).unwrap();
        fill_link_snapshot(&mut handle, 0, None, &[], Vec::new(), 8192, 4096);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "无活链应返回 0");
        assert!(handle.store.data().link_pending.is_empty(), "拒绝不入队");
    }

    #[test]
    fn command_import_queues_broadcast_with_children_count() {
        // 链三态·活链：快照 children=3 -> 返回 3（入队链数=拍首快照），payload
        // 原样入队（扇出在世界侧阶段二做——import 拿不到 world）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(COMMANDER_WAT), 10_000, id(883)).unwrap();
        fill_link_snapshot(&mut handle, 3, None, &[], Vec::new(), 8192, 4096);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 3, "活链应返回孩子数 3");
        let pending = &handle.store.data().link_pending;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].from, id(883));
        assert!(matches!(pending[0].kind, LinkOutKind::Command));
        assert_eq!(pending[0].payload, b"TURN+2".to_vec(), "信体应从 memory 读出");
    }

    #[test]
    fn command_import_oversize_and_ratelimit_reject() {
        // 负面路径：payload 超上限（读内存前拦下）返 0；同拍第二次返 0（限流
        // 1 次/拍/沙箱），下一拍 sent 清后恢复
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(COMMANDER_WAT), 10_000, id(884)).unwrap();
        fill_link_snapshot(&mut handle, 1, None, &[], Vec::new(), 4, 4096); // 上限 4 < 6
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "超限应拒绝");
        assert!(handle.store.data().link_pending.is_empty(), "超限不入队");
        // 恢复上限再发：成功；同拍第二发限流拒绝
        fill_link_snapshot(&mut handle, 1, None, &[], Vec::new(), 8192, 4096);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "首封应入队");
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "同拍第二次应限流拒绝");
        assert_eq!(handle.store.data().link_pending.len(), 1, "只有首封入队");
    }

    #[test]
    fn report_import_without_parent_returns_two() {
        // 链三态·放生：无链（parent=None）-> report 返回 2（无链或断链）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(REPORTER_WAT), 10_000, id(885)).unwrap();
        fill_link_snapshot(&mut handle, 0, None, &[], Vec::new(), 8192, 4096);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 2, "无链应返回 2");
        assert!(handle.store.data().link_pending.is_empty());
    }

    #[test]
    fn report_import_queues_uplink_to_parent() {
        // 链三态·活链：parent 有值 -> 返回 1，to=母体入队（上行沿链投）
        let mgr = SandboxManager::new().unwrap();
        let parent = id(900);
        let mut handle = mgr.create_sandbox(&wasm(REPORTER_WAT), 10_000, id(886)).unwrap();
        fill_link_snapshot(&mut handle, 0, Some(parent), &[], Vec::new(), 8192, 4096);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "活链上行应入队");
        let pending = &handle.store.data().link_pending;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].from, id(886));
        match &pending[0].kind {
            LinkOutKind::Report { to } => assert_eq!(*to, parent, "上行目标应为快照母体"),
            other => panic!("应为 Report，实际 {other:?}"),
        }
        assert_eq!(pending[0].payload, b"STATUS".to_vec());
    }

    #[test]
    fn send_import_hits_visible_peer_and_queues() {
        // 数据通道：坐标在 peer 快照（=发送方视口内实体）精确命中 -> 返回 1，
        // to=命中实体入队
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(SENDER_WAT), 10_000, id(887)).unwrap();
        fill_link_snapshot(
            &mut handle,
            0,
            None,
            &[],
            vec![link_peer(901, (7.0, 8.0), &[])],
            8192,
            4096,
        );
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "视口内命中应入队");
        match &handle.store.data().link_pending[0].kind {
            LinkOutKind::Send { to } => assert_eq!(*to, id(901), "目标应为坐标命中的实体"),
            other => panic!("应为 Send，实际 {other:?}"),
        }
    }

    #[test]
    fn send_import_misses_returns_minus_one() {
        // 负面路径：坐标无 peer（视口外/空场）-> -1；坐标非有限 -> 0（参数坏）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(SENDER_WAT), 10_000, id(888)).unwrap();
        // 空 peers：坐标无人 -> -1
        fill_link_snapshot(&mut handle, 0, None, &[], Vec::new(), 8192, 4096);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), -1, "坐标无实体应 -1");
        // 非 finite 坐标（0.0/0.0=NaN）：参数坏 -> 0
        let nan_sender = r#"(module
  (import "env" "send" (func $send (param f64 f64 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "HELLO")
  (func (export "on_tick")
    f64.const 0.0 f64.const 0.0 f64.div f64.const 0.0
    i32.const 0 i32.const 5 call $send global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;
        let mut h2 = mgr.create_sandbox(&wasm(nan_sender), 10_000, id(889)).unwrap();
        fill_link_snapshot(&mut h2, 0, None, &[], Vec::new(), 8192, 4096);
        mgr.call_on_tick(&mut h2).unwrap();
        assert_eq!(call_i32_export(&mut h2, "get_ret"), 0, "非有限坐标应参数坏拒绝");
    }

    #[test]
    fn storage_write_roundtrip_and_over_capacity_reject() {
        // 存储件：写入返回 N 且镜像同步（同拍读自己可见）；超容 -1 拒绝不截断
        //（读内存前拦下，硬盘写一半=坏盘）；len=0 合法清空
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(STORAGE_WRITER_WAT), 10_000, id(890)).unwrap();
        // 超容（上限 4 < 6）：-1 拒绝，镜像不动
        fill_link_snapshot(&mut handle, 0, None, &[0x11], Vec::new(), 8192, 4);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), -1, "超容应拒绝不截断");
        assert_eq!(handle.store.data().link_storage_own, vec![0x11], "拒绝不得动镜像");
        // 恢复上限写入：返回 6，镜像同步
        fill_link_snapshot(&mut handle, 0, None, &[0x11], Vec::new(), 8192, 4096);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 6, "写入应返回字节数");
        assert_eq!(
            handle.store.data().link_storage_own,
            b"INTEL1".to_vec(),
            "镜像应同步（同拍读自己可见）"
        );
        assert!(matches!(
            handle.store.data().link_pending[0].kind,
            LinkOutKind::StorageWrite
        ));
    }

    #[test]
    fn storage_read_near_success_and_self_read() {
        // 存储件读：贴身 peer 命中返回 N 写 memory（peer 在贴身圈内：dist 1.0
        // < 0.5+0.5+0.5=1.5）；读自己坐标=镜像（距离 0 天然成立）；哨兵字节不受扰
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&storage_reader_wat("1.0", "0.0")), 10_000, id(891))
            .unwrap();
        fill_link_snapshot(
            &mut handle,
            0,
            None,
            b"OWNDATA",
            vec![link_peer(902, (1.0, 0.0), b"PEERDATA")],
            8192,
            4096,
        );
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 8, "贴身读应返回 8 字节");
        assert_eq!(call_i32_export(&mut handle, "get_first"), b'P' as i32, "应读到对方块首字节");
        assert_eq!(read_memory(&mut handle, 2048, 1), vec![0x77], "写入不应越过 N 边界");
        // 读自己：坐标=自己快照 pos（(0,0) 默认）-> 读镜像 7 字节
        let mut h2 = mgr
            .create_sandbox(&wasm(&storage_reader_wat("0.0", "0.0")), 10_000, id(892))
            .unwrap();
        fill_link_snapshot(&mut h2, 0, None, b"OWNDATA", Vec::new(), 8192, 4096);
        mgr.call_on_tick(&mut h2).unwrap();
        assert_eq!(call_i32_export(&mut h2, "get_ret"), 7, "读自己应返回镜像字节数");
        assert_eq!(call_i32_export(&mut h2, "get_first"), b'O' as i32, "应读到自己块首字节");
    }

    #[test]
    fn storage_read_distance_and_absence_negative_paths() {
        // 负面路径四连：坐标无实体 -1 / 够不着 -2（距离判据严格 <）/ 空块 -3 /
        // cap 不足 0——四种语义互不混淆
        let mgr = SandboxManager::new().unwrap();
        // 远 peer（视口内但够不着）：-2
        let mut handle = mgr
            .create_sandbox(&wasm(&storage_reader_wat("30.0", "30.0")), 10_000, id(893))
            .unwrap();
        fill_link_snapshot(
            &mut handle,
            0,
            None,
            &[],
            vec![link_peer(903, (30.0, 30.0), b"FAR")],
            8192,
            4096,
        );
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), -2, "视口内但够不着应 -2");
        // 空场坐标：-1
        let mut h2 = mgr
            .create_sandbox(&wasm(&storage_reader_wat("99.0", "99.0")), 10_000, id(894))
            .unwrap();
        fill_link_snapshot(&mut h2, 0, None, &[], Vec::new(), 8192, 4096);
        mgr.call_on_tick(&mut h2).unwrap();
        assert_eq!(call_i32_export(&mut h2, "get_ret"), -1, "坐标无实体应 -1");
        // 贴身但对方空块：-3（"没硬盘"与"空硬盘"区分；peer 在贴身圈内）
        let mut h3 = mgr
            .create_sandbox(&wasm(&storage_reader_wat("1.0", "0.0")), 10_000, id(895))
            .unwrap();
        fill_link_snapshot(
            &mut h3,
            0,
            None,
            &[],
            vec![link_peer(904, (1.0, 0.0), &[])],
            8192,
            4096,
        );
        mgr.call_on_tick(&mut h3).unwrap();
        assert_eq!(call_i32_export(&mut h3, "get_ret"), -3, "对方空块应 -3");
        // 贴身有块但 cap 不足：0（不写任何字节）
        let small_cap = r#"(module
  (import "env" "storage_read" (func $storage_read (param f64 f64 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (func (export "on_tick")
    f64.const 1.0 f64.const 0.0 i32.const 100 i32.const 2
    call $storage_read global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;
        let mut h4 = mgr.create_sandbox(&wasm(small_cap), 10_000, id(896)).unwrap();
        fill_link_snapshot(
            &mut h4,
            0,
            None,
            &[],
            vec![link_peer(905, (1.0, 0.0), b"PEERDATA")],
            8192,
            4096,
        );
        mgr.call_on_tick(&mut h4).unwrap();
        assert_eq!(call_i32_export(&mut h4, "get_ret"), 0, "cap 不足应返回 0 不写");
        // 空场读取也不限流占用（失败不置位）：下一拍同沙箱仍可读——用 h3（-3
        // 失败路径）复跑验证 sent 未置位
        mgr.call_on_tick(&mut h3).unwrap();
        assert_eq!(call_i32_export(&mut h3, "get_ret"), -3, "失败路径不占限流（可重试）");
    }

    #[test]
    fn storage_read_rate_limited_per_tick() {
        // 限流：storage_read 成功一次后同拍第二次返 0（五 import 各 1 次/拍）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(&storage_reader_wat("1.0", "0.0")), 10_000, id(897))
            .unwrap();
        fill_link_snapshot(
            &mut handle,
            0,
            None,
            &[],
            vec![link_peer(906, (1.0, 0.0), b"PEERDATA")],
            8192,
            4096,
        );
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 8, "首次贴身读应成功");
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "同拍第二次应限流拒绝");
    }

    /// 链消息观察者 WAT：on_command/on_report/on_message 各记一票，from 存全局
    ///（payload 首字节存 $first 验证 grow 写入）
    const LINK_OBSERVER_WAT: &str = r#"(module
  (global $cmd (mut i32) (i32.const 0))
  (global $rep (mut i32) (i32.const 0))
  (global $msg (mut i32) (i32.const 0))
  (global $from (mut i64) (i64.const -1))
  (global $first (mut i32) (i32.const 0))
  (memory (export "memory") 1)
  (func (export "on_command") (param $ptr i32) (param $len i32)
    (global.set $cmd (i32.add (global.get $cmd) (i32.const 1)))
    (global.set $first (i32.load8_u (local.get $ptr))))
  (func (export "on_report") (param $from i64) (param $ptr i32) (param $len i32)
    (global.set $rep (i32.add (global.get $rep) (i32.const 1)))
    (global.set $from (local.get $from)))
  (func (export "on_message") (param $from i64) (param $ptr i32) (param $len i32)
    (global.set $msg (i32.add (global.get $msg) (i32.const 1)))
    (global.set $from (local.get $from)))
  (func (export "get_cmd") (result i32) global.get $cmd)
  (func (export "get_rep") (result i32) global.get $rep)
  (func (export "get_msg") (result i32) global.get $msg)
  (func (export "get_from") (result i64) global.get $from)
  (func (export "get_first") (result i32) global.get $first)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn deliver_message_grows_memory_and_calls_on_command() {
        // on_command 投递：grow 新页写入（offset=旧末尾）、回调被调、payload
        // 首字节可读、消耗记在预算尺度
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LINK_OBSERVER_WAT), 100_000, id(910)).unwrap();
        let mem = handle.instance.get_memory(&mut handle.store, "memory").unwrap();
        let before_pages = mem.size(&handle.store);
        let used = mgr
            .deliver_message(&mut handle, "on_command", 7, false, b"TURN+2", 10_000)
            .unwrap()
            .expect("导出了 on_command 应返回 Some(used)");
        assert!(used > 0 && used <= 10_000, "消耗应记在预算尺度内（used={used}）");
        let mem = handle.instance.get_memory(&mut handle.store, "memory").unwrap();
        assert!(mem.size(&handle.store) > before_pages, "消息应经 grow 新页写入");
        assert_eq!(call_i32_export(&mut handle, "get_cmd"), 1, "on_command 应被调一次");
        assert_eq!(call_i32_export(&mut handle, "get_first"), b'T' as i32, "payload 首字节应可读");
        // on_report/on_message 的 from 参数 = 源 EntityId 低 64 位（i64 直传）
        let from = 0x1122334455667788u64;
        mgr.deliver_message(&mut handle, "on_report", from, true, b"x", 10_000)
            .unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_rep"), 1, "on_report 应被调一次");
        assert_eq!(
            call_i64_export(&mut handle, "get_from"),
            from as i64,
            "from 应为源 EntityId 低 64 位"
        );
        mgr.deliver_message(&mut handle, "on_message", from, true, b"y", 10_000)
            .unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_msg"), 1, "on_message 应被调一次");
    }

    #[test]
    fn deliver_message_without_export_is_noop() {
        // 负面路径：缺省导出 -> Ok(None) 零副作用（信塞进无人收件箱，不 grow
        // 不调用不崩世界）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(911)).unwrap();
        assert!(
            mgr.deliver_message(&mut handle, "on_message", 1, true, b"x", 10_000)
                .unwrap()
                .is_none(),
            "未导出应返回 None"
        );
    }

    /// 坏签名观察者：导出了 on_message 但签名不符（无参返 i32）
    const LINK_BAD_SIG_WAT: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "on_message") (result i32) i32.const 7)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn deliver_message_bad_signature_leaves_no_half_side_effect() {
        // P2-A 先例：签名核验先于 grow+写——错误签名导出时投递 Err，内存分毫
        // 不动（无"字节已落内存但回调没跑"的半副作用残局）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LINK_BAD_SIG_WAT), 1000, id(912)).unwrap();
        let before = handle
            .instance
            .get_memory(&mut handle.store, "memory")
            .unwrap()
            .size(&handle.store);
        assert!(
            mgr.deliver_message(&mut handle, "on_message", 1, true, b"123", 10_000)
                .is_err(),
            "签名不匹配应 Err"
        );
        let after = handle
            .instance
            .get_memory(&mut handle.store, "memory")
            .unwrap()
            .size(&handle.store);
        assert_eq!(after, before, "签名核验失败不得动内存");
    }

    #[test]
    fn deliver_message_restores_fuel() {
        // 燃料不污染（on_contact/bridge_on_reply 同款）：包裹前 fuel=55_555 ->
        // 调用后恢复原值（收信烧独立预算不扣主配额）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LINK_OBSERVER_WAT), 100_000, id(913)).unwrap();
        handle.store.set_fuel(55_555).unwrap();
        mgr.deliver_message(&mut handle, "on_message", 1, true, b"7", 10_000)
            .unwrap();
        assert_eq!(
            handle.store.get_fuel().unwrap(),
            55_555,
            "调用后 fuel 应恢复原值（收信不扣主配额手续费）"
        );
    }

    /// 调用沙箱无参 i64 导出（from 断言观测）
    fn call_i64_export(handle: &mut SandboxHandle, name: &str) -> i64 {
        let func = handle
            .instance
            .get_typed_func::<(), i64>(&mut handle.store, name)
            .unwrap();
        func.call(&mut handle.store, ()).unwrap()
    }
}

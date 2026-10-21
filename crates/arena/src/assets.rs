//! #051 皮入沙箱·资产配额账（Screeps 5MB 思路）。
//!
//! 直觉锚（世界观看 docs/decisions.md #051）：配额账管的是"盒子里装多少东西"，
//! 64MB 内存上限管的是"魂的房间多大"——两本账并行不混。
//!
//! 配额账两源：wasm 字节（引擎）+ 资产段（皮，按指纹查镜像仓实大小——镜像仓
//! 在 bin 侧 mirror.rs，本 crate 只认字节数不认内容）；魂（线性内存）由 64MB
//! static_memory 独立封顶——三段合计封顶的总精神由两本账共同实现
//!（docs/api.md 五点五同口径）。
//!
//! 超限=拒绝入箱（同 blob 整次拒绝哲学——超限拒绝装戴，不裁剪）。
//!
//! 数据非执行（#051 工程三点）：资产段不烧 fuel、不参与编译实例化，只占
//! 沙箱内存边界——配额是唯一约束面。

/// 单沙箱资产配额（字节）。Screeps 5MB 思路，默认 1MB（bin 侧 CODEWORLD_ASSET_QUOTA
/// 覆盖，config 可调），与 64MB 内存上限的比例≈1/64（资产=富余量非主粮）。
/// 常数入 game-menu 1.7 总账。
pub const SANDBOX_ASSET_QUOTA_BYTES: usize = 1024 * 1024;

/// 单沙箱资产配额校验器（无状态：每次按实况校验，不落账）。
///
/// 每次校验都按当次实况（当次模块字节 + 随行清单实大小）对上限判定，不在
/// 结构体里记持久账——沙箱的真实占用由事实本身（模块在不在、清单有什么）
/// 说话，校验器只回答"装不装得下"。不信任玩家自报：资产大小由调用方按
/// 镜像仓实大小给出（bin 侧换血门），不由玩家代码申报。
#[derive(Debug, Clone)]
pub struct SandboxAssetQuota {
    /// 配额上限（字节）
    limit: usize,
}

impl SandboxAssetQuota {
    /// 创建校验器（默认上限 [`SANDBOX_ASSET_QUOTA_BYTES`]）
    pub fn new() -> Self {
        Self::with_limit(SANDBOX_ASSET_QUOTA_BYTES)
    }

    /// 创建校验器（指定上限；bin 侧 CODEWORLD_ASSET_QUOTA 覆盖传入）
    pub fn with_limit(limit: usize) -> Self {
        Self { limit }
    }

    /// 配额上限（字节）
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// 装戴校验：模块字节+Σ资产大小 ≤ 配额 → Ok；超限 → Err(超额字节)
    /// （Err 携带差额，调用方日志+拒绝装戴）。
    /// 整次拒绝不裁剪：同 blob 超限哲学，部分入箱=半截皮比没皮更糟。
    /// 注：当前生产路径仅用 [`Self::admit_swap`]（换血门）；本方法留给装戴刀
    ///（出生装戴时校验），两方法同口径同边界（admit_and_admit_swap_same_
    /// semantics 测试锚定）。
    pub fn admit(&self, module_bytes: usize, assets: &[(String, usize)]) -> Result<(), usize> {
        Self::check(self.limit, module_bytes, assets, "装戴")
    }

    /// 换血校验（#051 换血场景定死语义：**拒绝换血，资产不丢**）：新模块字节+
    /// 转移资产清单合计 ≤ 配额 → Ok；超限 → Err(超额字节)——调用方拒绝换血
    /// （旧沙箱保留原样，夺取方装不下自己的 payload 那是夺取失败）。
    /// bin 侧 main.rs replace_code 换血配额门是唯一生产消费点。
    pub fn admit_swap(
        &self,
        new_module_bytes: usize,
        transferred: &[(String, usize)],
    ) -> Result<(), usize> {
        Self::check(self.limit, new_module_bytes, transferred, "换血")
    }

    /// 共用判定核心：两源合计（模块字节+Σ资产大小）对上限；超限 warn+Err 差额
    fn check(limit: usize, module_bytes: usize, assets: &[(String, usize)], scene: &str) -> Result<(), usize> {
        let total = Self::total_of(module_bytes, assets);
        if total > limit {
            tracing::warn!(
                module_bytes,
                assets_bytes = total - module_bytes,
                limit,
                over = total - limit,
                scene,
                "[asset_quota] 超配额拒绝（整次拒绝不裁剪，不落账）"
            );
            return Err(total - limit);
        }
        Ok(())
    }

    /// 合计：模块字节+Σ资产大小（饱和加法防溢出——usize 溢出在配额语义下必超限）
    fn total_of(module_bytes: usize, assets: &[(String, usize)]) -> usize {
        let assets_bytes = assets
            .iter()
            .fold(0usize, |acc, (_, size)| acc.saturating_add(*size));
        module_bytes.saturating_add(assets_bytes)
    }
}

impl Default for SandboxAssetQuota {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试助手：asset_id 列表转 (id, size) 对
    fn sized(ids: &[(&str, usize)]) -> Vec<(String, usize)> {
        ids.iter()
            .map(|(id, s)| (id.to_string(), *s))
            .collect()
    }

    #[test]
    fn default_limit_is_one_megabyte() {
        // 创建：默认上限=SANDBOX_ASSET_QUOTA_BYTES=1MB（game-menu 1.7 总账锚）
        let q = SandboxAssetQuota::new();
        assert_eq!(q.limit(), SANDBOX_ASSET_QUOTA_BYTES);
        assert_eq!(q.limit(), 1024 * 1024);
        assert_eq!(
            SandboxAssetQuota::default().limit(),
            SANDBOX_ASSET_QUOTA_BYTES
        );
    }

    #[test]
    fn admit_under_limit_ok() {
        // 装戴：模块 100B + 资产 300B ≤ 1MB → Ok（无状态校验器：按实况判定不落账）
        let q = SandboxAssetQuota::new();
        assert!(q.admit(100, &sized(&[("av_aabbccddeeff0011", 300)])).is_ok());
    }

    #[test]
    fn admit_exactly_at_limit_passes() {
        // 边界（恰等放行，同 spawn 初速/收图尺寸先例）：合计恰等上限 → Ok
        let q = SandboxAssetQuota::with_limit(1000);
        assert!(q
            .admit(400, &sized(&[("av_aabbccddeeff0011", 600)]))
            .is_ok());
    }

    #[test]
    fn admit_over_limit_rejects_with_diff() {
        // 负面（卡面负面路径①）：超限 → Err(超额字节)，调用方日志+拒绝装戴
        let q = SandboxAssetQuota::with_limit(1000);
        let err = q
            .admit(500, &sized(&[("av_aabbccddeeff0011", 600)]))
            .expect_err("超限应拒绝");
        assert_eq!(err, 100, "Err 携带超额字节：1100-1000=100");
    }

    #[test]
    fn admit_swap_under_limit_ok() {
        // 换血（正面）：新模块 500B + 随行资产 400B ≤ 1MB → Ok（换血可成）
        let q = SandboxAssetQuota::new();
        assert!(q
            .admit_swap(500, &sized(&[("av_aabbccddeeff0011", 400)]))
            .is_ok());
    }

    #[test]
    fn admit_swap_over_limit_rejects_with_diff() {
        // 负面（卡面负面路径②的前置校验）：换血挤不下 → Err(超额字节)；
        // 调用方（bin replace_code）据此拒绝换血——旧沙箱保留，资产不丢
        let q = SandboxAssetQuota::with_limit(1000);
        let err = q
            .admit_swap(600, &sized(&[("av_aabbccddeeff0011", 500)]))
            .expect_err("挤不下应拒绝");
        assert_eq!(err, 100, "Err 携带超额字节：1100-1000=100");
    }

    #[test]
    fn admit_and_admit_swap_same_semantics() {
        // 边界：admit 与 admit_swap 同口径同边界——同一输入两入口判定一致
        //（Ok/Err 与差额字节全等；装戴刀接入时换血门语义不漂移）
        let q = SandboxAssetQuota::with_limit(1000);
        let assets = sized(&[("av_aabbccddeeff0011", 400)]);
        assert_eq!(q.admit(600, &assets), q.admit_swap(600, &assets), "恰等边界两入口一致");
        assert_eq!(
            q.admit(601, &assets).expect_err("超限应拒绝"),
            q.admit_swap(601, &assets).expect_err("超限应拒绝"),
            "超额字节两入口一致"
        );
        assert_eq!(q.admit(0, &[]), q.admit_swap(0, &[]), "空载两入口一致");
    }

    #[test]
    fn admit_zero_assets_and_zero_module_ok() {
        // 边界：空装戴（无模块无资产）恒合法——玩家进场/弹丸的无皮路径零负担
        let q = SandboxAssetQuota::new();
        assert!(q.admit(0, &[]).is_ok());
    }
}

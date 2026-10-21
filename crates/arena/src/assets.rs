//! #051 皮入沙箱·资产配额账：引擎+魂+皮合计封顶（Screeps 5MB 思路）。
//!
//! 直觉锚（世界观看 docs/decisions.md #051）：配额管的是"盒子里装多少东西"，
//! 64MB 内存上限管的是"魂的房间多大"——两本账并行不混。
//!
//! 超限=拒绝入箱（同 blob 整次拒绝哲学——超限拒绝装戴，不裁剪）。记账三源：
//! wasm 字节（引擎）/线性内存上限（魂）/资产段（皮，按指纹查镜像仓实大小，
//! 镜像仓在 bin 侧 mirror.rs，本 crate 只认字节数不认内容）。
//!
//! 数据非执行（#051 工程三点）：资产段不烧 fuel、不参与编译实例化，只占
//! 沙箱内存边界——配额是唯一约束面。

/// 单沙箱资产配额（字节）。Screeps 5MB 思路，默认 1MB（bin 侧 CODEWORLD_ASSET_QUOTA
/// 覆盖，config 可调），与 64MB 内存上限的比例≈1/64（资产=富余量非主粮）。
/// 常数入 game-menu 1.7 总账。
pub const SANDBOX_ASSET_QUOTA_BYTES: usize = 1024 * 1024;

/// 单沙箱资产配额账（引擎侧记账，不信任玩家自报）。
///
/// 账目=模块字节数（引擎）+资产段合计（皮）。装戴（`admit`）落账；
/// 换血（`admit_swap`）只校验不落账——账随沙箱实际状态走，拒绝的换血不产生账目。
#[derive(Debug, Clone)]
pub struct SandboxAssetQuota {
    /// 配额上限（字节）
    limit: usize,
    /// 引擎账：当前模块字节数
    module_bytes: usize,
    /// 皮账：资产段合计字节数
    assets_bytes: usize,
}

impl SandboxAssetQuota {
    /// 创建配额账（默认上限 [`SANDBOX_ASSET_QUOTA_BYTES`]）
    pub fn new() -> Self {
        Self::with_limit(SANDBOX_ASSET_QUOTA_BYTES)
    }

    /// 创建配额账（指定上限；bin 侧 CODEWORLD_ASSET_QUOTA 覆盖传入）
    pub fn with_limit(limit: usize) -> Self {
        Self {
            limit,
            module_bytes: 0,
            assets_bytes: 0,
        }
    }

    /// 配额上限（字节）
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// 当前模块字节数（引擎账）
    pub fn module_bytes(&self) -> usize {
        self.module_bytes
    }

    /// 当前资产段合计字节数（皮账）
    pub fn assets_bytes(&self) -> usize {
        self.assets_bytes
    }

    /// 装戴校验：模块字节+Σ资产大小 ≤ 配额 → Ok（并整次落账）；超限 → Err(超额字节)
    /// （Err 携带差额，调用方日志+拒绝装戴；账不动——失败的装戴不产生账目）。
    /// 整次拒绝不裁剪：同 blob 超限哲学，部分入箱=半截皮比没皮更糟。
    pub fn admit(
        &mut self,
        module_bytes: usize,
        assets: &[(String, usize)],
    ) -> Result<(), usize> {
        let total = Self::total_of(module_bytes, assets);
        if total > self.limit {
            tracing::warn!(
                module_bytes,
                assets_bytes = total - module_bytes,
                limit = self.limit,
                over = total - self.limit,
                "[asset_quota] 装戴拒绝：模块+资产超配额（整次拒绝不裁剪）"
            );
            return Err(total - self.limit);
        }
        self.module_bytes = module_bytes;
        self.assets_bytes = total - module_bytes;
        Ok(())
    }

    /// 换血校验（#051 换血场景定死语义：**拒绝换血，资产不丢**）：新模块字节+
    /// 转移资产清单合计 ≤ 配额 → Ok；超限 → Err(超额字节)——调用方拒绝换血
    /// （旧沙箱保留原样，夺取方装不下自己的 payload 那是夺取失败）。
    /// 只校验不落账：账随沙箱实际换血成功而走，本方法无副作用。
    pub fn admit_swap(
        &self,
        new_module_bytes: usize,
        transferred: &[(String, usize)],
    ) -> Result<(), usize> {
        let total = Self::total_of(new_module_bytes, transferred);
        if total > self.limit {
            tracing::warn!(
                new_module_bytes,
                assets_bytes = total - new_module_bytes,
                limit = self.limit,
                over = total - self.limit,
                "[asset_quota] 换血拒绝：新模块+随行资产超配额（拒绝换血，资产不丢）"
            );
            return Err(total - self.limit);
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
    fn admit_under_limit_records_account() {
        // 装戴：模块 100B + 资产 300B ≤ 1MB → Ok，账=引擎 100B+皮 300B（不信任自报，引擎记账）
        let mut q = SandboxAssetQuota::new();
        assert!(q.admit(100, &sized(&[("av_aabbccddeeff0011", 300)])).is_ok());
        assert_eq!(q.module_bytes(), 100);
        assert_eq!(q.assets_bytes(), 300);
    }

    #[test]
    fn admit_exactly_at_limit_passes() {
        // 边界（恰等放行，同 spawn 初速/收图尺寸先例）：合计恰等上限 → Ok
        let mut q = SandboxAssetQuota::with_limit(1000);
        assert!(q
            .admit(400, &sized(&[("av_aabbccddeeff0011", 600)]))
            .is_ok());
        assert_eq!(q.module_bytes(), 400);
        assert_eq!(q.assets_bytes(), 600);
    }

    #[test]
    fn admit_over_limit_rejects_with_diff_and_keeps_account() {
        // 负面（卡面负面路径①）：超限 → Err(超额字节)，且失败装不落账（账保持原值）
        let mut q = SandboxAssetQuota::with_limit(1000);
        assert!(q.admit(100, &[]).is_ok());
        let err = q
            .admit(500, &sized(&[("av_aabbccddeeff0011", 600)]))
            .expect_err("超限应拒绝");
        assert_eq!(err, 100, "Err 携带超额字节：1100-1000=100");
        // 失败装不产生账目：账仍是首次成功装的 100B
        assert_eq!(q.module_bytes(), 100);
        assert_eq!(q.assets_bytes(), 0);
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
    fn admit_swap_is_side_effect_free() {
        // 边界：admit_swap 只校验不落账（无 &mut self）——拒绝的换血不改任何账目
        let q = SandboxAssetQuota::with_limit(1000);
        let _ = q.admit_swap(5000, &[]);
        assert_eq!(q.module_bytes(), 0, "校验方法无副作用");
        assert_eq!(q.assets_bytes(), 0);
    }

    #[test]
    fn admit_zero_assets_and_zero_module_ok() {
        // 边界：空装戴（无模块无资产）恒合法——玩家进场/弹丸的无皮路径零负担
        let mut q = SandboxAssetQuota::new();
        assert!(q.admit(0, &[]).is_ok());
        assert_eq!(q.module_bytes(), 0);
        assert_eq!(q.assets_bytes(), 0);
    }
}

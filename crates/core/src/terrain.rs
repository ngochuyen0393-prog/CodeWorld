//! 地形（D-016 地形起伏函数：高度图地面四件套之一/二）
//!
//! 表示=均匀采样高度数组（0.25 格/采样，100 格世界 401 点）+ 线性插值查询 O(1)；
//! 生成=1D 值噪声 3~4 层叠 fBm + pow 压谷 + 坡度钳制（防不可爬陡坡）+ 种子可复现
//!（确定性合规：同 (preset, seed) 必产同一高度数组，零随机源）；
//! y=0 退位为坐标基准线（海平面语义），丘正坑负全合法。
//! config 预设：平地 Flat / 丘陵 Hills / 山地 Mountain。
//! 心智模型（土豆定案）：人走在地面上不是走在坐标上，坐标只是定位；
//! 各种"面"（地面/水面/地层）是物理实体，坐标轴是描述它们的尺子。
//! 除非 X：可破坏地形（位图掩码，Worms 级）挂"玩家把挖洞/掏山玩成主流战术"
//! 信号——否则高度数组不动。
//!
//! 域：世界 x ∈ [TERRAIN_X_MIN, TERRAIN_X_MAX]（[-50, 50]，与世界宽度 100 格对齐），
//! 域外查询钳到域边（世界边缘外地面延伸，高度不变）。

use serde::{Deserialize, Serialize};

/// 地形采样间距（格/采样点）。0.25 格 → 100 格世界 401 点（D-016 定案）。
pub const TERRAIN_SPACING: f64 = 0.25;

/// 地形域左端（世界 x 最小值，与视口半宽 50 对齐）
pub const TERRAIN_X_MIN: f64 = -50.0;

/// 地形域右端（世界 x 最大值）
pub const TERRAIN_X_MAX: f64 = 50.0;

/// 地形预设（config 预设：平地/丘陵/山地，D-016 定案）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerrainPreset {
    /// 平地：全零高度数组（y=0 基准线即地面，与旧地面语义完全一致）
    Flat,
    /// 丘陵：3 层 fBm，幅度 3 格，平缓可走
    Hills,
    /// 山地：4 层 fBm，幅度 8 格，pow 压谷出明显山峰
    Mountain,
}

impl TerrainPreset {
    /// 从配置字符串解析（大小写不敏感；"丘陵"/"山地" 中文别名同收；
    /// 非法值回退 Flat——世界常数坏值不进世界，与 CODEWORLD_* 系列护栏同款）
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "hills" | "hill" | "丘陵" => TerrainPreset::Hills,
            "mountain" | "mountains" | "山地" | "山" => TerrainPreset::Mountain,
            _ => TerrainPreset::Flat,
        }
    }

    /// 预设名（日志/快照可读）
    pub fn as_str(&self) -> &'static str {
        match self {
            TerrainPreset::Flat => "flat",
            TerrainPreset::Hills => "hills",
            TerrainPreset::Mountain => "mountain",
        }
    }

    /// 该预设的生成参数（octaves/幅度/基波长/压谷指数/坡度钳制）
    fn gen_params(&self) -> TerrainGenParams {
        match self {
            TerrainPreset::Flat => TerrainGenParams {
                octaves: 1,
                base_amplitude: 0.0,
                persistence: 0.5,
                feature_size: 16.0,
                valley_pow: 1.0,
                max_slope: 100.0,
            },
            TerrainPreset::Hills => TerrainGenParams {
                octaves: 3,
                base_amplitude: 3.0,
                persistence: 0.5,
                feature_size: 16.0,
                valley_pow: 1.2,
                max_slope: 1.0,
            },
            TerrainPreset::Mountain => TerrainGenParams {
                octaves: 4,
                base_amplitude: 8.0,
                persistence: 0.55,
                feature_size: 20.0,
                valley_pow: 1.6,
                max_slope: 1.5,
            },
        }
    }
}

/// 地形生成参数（预设内部用；pow 压谷 + 坡度钳制的具体数值）
struct TerrainGenParams {
    /// fBm 层数（3~4，D-016 定案）
    octaves: u32,
    /// 基幅度（格）： heights ⊆ [-base_amplitude, +base_amplitude]
    base_amplitude: f64,
    /// 幅度衰减（每层减半附近）
    persistence: f64,
    /// 基波长（格）：第 o 层波长 = feature_size / 2^o
    feature_size: f64,
    /// pow 压谷指数（>1：浅谷压平、山峰保留——|v|^p 对小 |v| 压缩更强）
    valley_pow: f64,
    /// 坡度钳制（格高度差/格水平距离上限，防不可爬陡坡）
    max_slope: f64,
}

/// SplitMix64 风格整数哈希 → [0,1)（确定性值噪声的地板函数；
/// 无外部 crate、无 RNG 状态，同 (i, seed) 必同值）
fn hash01(i: i64, seed: u64) -> f64 {
    let mut z = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seed.wrapping_add(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // 取高 53 位 → [0,1) 均匀分布（f64 精确区间）
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// 单层值噪声：格点哈希 + smoothstep 插值 → [-1, 1]（C1 连续）
///
/// lattice=格长（该层波长）；x/lattice 落在哪格，两端点哈希值平滑插值。
fn value_noise(x: f64, lattice: f64, seed: u64) -> f64 {
    let g = x / lattice;
    let xi = g.floor();
    let f = g - xi;
    // smoothstep（C1）：两端值平滑过渡，无格点折角
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash01(xi as i64, seed);
    let b = hash01(xi as i64 + 1, seed);
    2.0 * (a + (b - a) * u) - 1.0
}

/// 高度图地面（D-016 四件套①②：表示+生成）
///
/// 均匀采样高度数组 + 线性插值（Hill Climb Racing / Tiny Wings 验证的行业标准）。
/// 种子可复现：`Terrain::new(preset, seed)` 同参数必产同一数组（确定性合规）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Terrain {
    /// 高度采样数组（heights[i] = x_i 处地面高度，x_i = X_MIN + i*SPACING）
    heights: Vec<f64>,
    /// 生成预设（快照可读性/日志用；除非 X 条款外高度数组不动）
    pub preset: TerrainPreset,
    /// 生成种子（可复现凭据：同 preset+seed 同地形）
    pub seed: u64,
}

impl Terrain {
    /// 按预设+种子生成地形（确定性：同参数同结果）
    pub fn new(preset: TerrainPreset, seed: u64) -> Self {
        let params = preset.gen_params();
        let sample_count = ((TERRAIN_X_MAX - TERRAIN_X_MIN) / TERRAIN_SPACING) as usize + 1;
        let mut heights = Vec::with_capacity(sample_count);
        // 1. fBm：octaves 层值噪声叠加（波长减半/幅度衰减），叠加和 clamp 到 [-1,1]
        let mut weights = Vec::with_capacity(params.octaves as usize);
        let mut amp = 1.0;
        for _ in 0..params.octaves {
            weights.push(amp);
            amp *= params.persistence;
        }
        for i in 0..sample_count {
            let x = TERRAIN_X_MIN + i as f64 * TERRAIN_SPACING;
            let mut v = 0.0;
            for (o, &w) in weights.iter().enumerate() {
                // 每层独立种子（层间去相关）：seed 混入层号
                let layer_seed = seed ^ ((o as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
                let lattice = params.feature_size / (1u32 << o) as f64;
                v += w * value_noise(x, lattice, layer_seed);
            }
            // 2. fBm 归一：clamp 到 [-1,1]（叠加和理论可达 ±Σw，主层主导实测
            //    ~±1.3；clamp 保域，不做除法归一——除法会把全地形整体压扁）
            let v = v.clamp(-1.0, 1.0);
            // 3. pow 压谷（[0,1] 域标准做法）：u=(v+1)/2 → u^p → 回 [-1,1]。
            //    p>1 对小 u 压缩更强、u=1 不动——浅谷压平变宽、山峰保留高度
            let u = (v + 1.0) / 2.0;
            let v = 2.0 * u.powf(params.valley_pow) - 1.0;
            // 4. 幅度定标
            heights.push(params.base_amplitude * v);
        }
        // 5. 坡度钳制（防不可爬陡坡）：相邻采样高度差 ≤ max_slope * spacing。
        //    前向+后向两遍（经典斜率限制器）：前向压上升沿、后向压下降沿，
        //    单向偏置被第二遍抵消；只把值拉向邻点，全局极值只减不增。
        let max_delta = params.max_slope * TERRAIN_SPACING;
        if max_delta.is_finite() && max_delta > 0.0 {
            for i in 1..heights.len() {
                let lo = heights[i - 1] - max_delta;
                let hi = heights[i - 1] + max_delta;
                heights[i] = heights[i].clamp(lo, hi);
            }
            for i in (0..heights.len() - 1).rev() {
                let lo = heights[i + 1] - max_delta;
                let hi = heights[i + 1] + max_delta;
                heights[i] = heights[i].clamp(lo, hi);
            }
        }
        tracing::info!(
            preset = preset.as_str(),
            seed,
            samples = heights.len(),
            min_h = heights.iter().cloned().fold(f64::INFINITY, f64::min),
            max_h = heights.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            "[terrain] 地形生成完成（D-016 高度图）"
        );
        Self {
            heights,
            preset,
            seed,
        }
    }

    /// 平地地形（全零高度数组，y=0 即地面——与旧地面语义逐位一致）
    pub fn flat() -> Self {
        Self::new(TerrainPreset::Flat, 0)
    }

    /// 地面高度查询 O(1)：线性插值（四件套①）
    ///
    /// 域外 x 钳到域边（世界边缘外地面延伸）。
    pub fn height(&self, x: f64) -> f64 {
        let xc = x.clamp(TERRAIN_X_MIN, TERRAIN_X_MAX);
        let f = (xc - TERRAIN_X_MIN) / TERRAIN_SPACING;
        let mut i = f.floor() as usize;
        // 右端点：i+1 越界时钳到最后一个区间、t=1（取末采样值）
        if i >= self.heights.len().saturating_sub(1) {
            i = self.heights.len().saturating_sub(1);
            return self.heights[i];
        }
        let t = f - i as f64;
        let a = self.heights[i];
        let b = self.heights[i + 1];
        a + (b - a) * t
    }

    /// 采样点数（快照体积/测试用）
    pub fn sample_count(&self) -> usize {
        self.heights.len()
    }

    /// 高度数组只读快照（渲染/网络投影用；除非 X 条款：高度数组不动）
    pub fn samples(&self) -> &[f64] {
        &self.heights
    }

    /// CCD 落点查表（四件套③：弹道路径与地形面的首次接触）
    ///
    /// 路径 from→to（弹心，一拍位移），radius 弹半径：
    /// 接触条件 path_y(t) = ground(x(t)) + radius。地形是分段线性函数
    ///（0.25 格/段），路径是线性参数曲线——每段内方程对 t 线性，逐段解析求解，
    /// 取路径方向上第一个有效根。返回 t ∈ [0,1]；None = 全程在地形上方（不落地）。
    /// 起步已在地面下（异常态兜底）：返回 Some(0.0) 立即触地（与旧平地口径一致）。
    /// 纯查表无 IO、每拍热路径：日志豁免，可观测性以锚测试为主。
    pub fn segment_first_contact(
        &self,
        from: (f64, f64),
        to: (f64, f64),
        radius: f64,
    ) -> Option<f64> {
        let (x0, y0) = from;
        let (x1, y1) = to;
        let dx = x1 - x0;
        let dy = y1 - y0;
        // 起步已在地面下：异常态兜底，立即触地（t=0）
        if y0 < self.height(x0) + radius {
            return Some(0.0);
        }
        if dx.abs() < 1e-12 {
            // 垂直路径：地面高度恒定，直接解一元一次
            let surface = self.height(x0) + radius;
            if y1 < surface && y0 > y1 {
                let t = (y0 - surface) / (y0 - y1);
                return Some(t.clamp(0.0, 1.0));
            }
            return None;
        }
        // 一般路径：逐地形格（分段线性）解 A + B*t = 0
        let n = self.heights.len();
        if n < 2 {
            return None;
        }
        // 路径覆盖的采样格区间（沿路径方向遍历，保证取到的是时间上"最先"接触）：
        // dx>0 从起点格升序、dx<0 从起点格降序——反向路径若仍按格下标升序扫，
        // 先解出的是时间上"后发生"的出口侧根（R84 复审缺陷 1：左向穿山返回
        // 出口侧 t 而非入口侧）。
        // 域外段处理（近似口径，如实记录）：域外地面语义=域边高度恒定延伸
        //（与 height() 钳制一致），实现上把域外 x 钳进边格、在边格线性段的
        // 延长线上解根——平地（边格等高）时延长线与延伸面重合无误差；边格
        // 有坡度时是外推近似，误差随域外距离线性放大（每采样段高差
        // ≤max_slope×spacing，即每格坡度≤max_slope）。恒定面精确解为已知
        // 改进项（.cc-bugs #20），平地射程标定等既有用例不受影响（边格等高
        // 时两口径逐位一致）
        let dir = dx.signum();
        let cell_of = |x: f64| -> usize {
            (((x - TERRAIN_X_MIN) / TERRAIN_SPACING).floor() as i64)
                .clamp(0, (n - 2) as i64) as usize
        };
        let lo = cell_of(x0).min(cell_of(x1));
        let hi = cell_of(x0).max(cell_of(x1));
        // 按路径方向枚举格：dir>0 升序（lo→hi），dir<0 降序（hi→lo）
        for k in 0..=(hi - lo) {
            let i = if dir > 0.0 { lo + k } else { hi - k };
            let ga = self.heights[i];
            let gb = self.heights[i + 1];
            // 该格内 ground(x) = ga + (x - xa)/spacing * (gb - ga)；
            // 代入 x = x0 + t*dx, y = y0 + t*dy，接触条件整理为 A + B*t = 0：
            let slope = (gb - ga) / TERRAIN_SPACING; // 每格 x 的地面斜率
            let a = y0 - radius - ga - (x0 - (TERRAIN_X_MIN + i as f64 * TERRAIN_SPACING)) * slope;
            let b = dy - dx * slope;
            if b.abs() < 1e-15 {
                continue; // 平行于该段地面：无交点
            }
            let t = -a / b;
            if !(0.0..=1.0).contains(&t) {
                continue;
            }
            // t 对应的 x 必须落在本格内（交点在段上，不是段的延长线；
            // 格点处相邻两段值相同，边界根在邻格同样解出——容差只需吸收
            // 浮点噪声，绝不能用半格级容差：段的延长线处真实地面已偏离
            // 该段线性，外推根=假落点）。
            // 例外：边格延伸——i=0（左界）/i=n-2（右界）的段在域外按上述
            // 近似口径放行延长线根（见上文"域外段处理"注记）
            let tx = x0 + t * dx;
            let xa = TERRAIN_X_MIN + i as f64 * TERRAIN_SPACING;
            let xb = TERRAIN_X_MIN + (i + 1) as f64 * TERRAIN_SPACING;
            let edge_extend = (i == 0 && tx < xa - 1e-9) || (i == n - 2 && tx > xb + 1e-9);
            let in_seg = tx >= xa - 1e-9 && tx <= xb + 1e-9;
            if in_seg || edge_extend {
                return Some(t);
            }
        }
        None
    }
}

impl Default for Terrain {
    /// 默认平地（旧世界快照 serde 回退：无 terrain 字段 → 平地，行为零回归）
    fn default() -> Self {
        Self::flat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === 表示（四件套①：高度数组+线性插值）===

    #[test]
    fn flat_terrain_is_all_zero() {
        // 创建：平地=全零高度数组，y=0 即地面（与旧地面语义逐位一致）
        let t = Terrain::flat();
        assert_eq!(t.sample_count(), 401, "100 格世界 0.25 采样 = 401 点");
        for (i, &h) in t.samples().iter().enumerate() {
            assert_eq!(h, 0.0, "平地采样点 {i} 应为零");
        }
        assert_eq!(t.height(0.0), 0.0);
        assert_eq!(t.height(-50.0), 0.0);
        assert_eq!(t.height(50.0), 0.0);
        assert_eq!(t.height(12.34), 0.0);
    }

    #[test]
    fn height_interpolates_linearly_between_samples() {
        // 读写：格点间线性插值——构造已知数组（经 serde 注入测试不可行，改用
        // 平地+山地的性质断言；此处用 mount terrain 验证插值连续性）
        let t = Terrain::new(TerrainPreset::Hills, 7);
        // 格点值 = 数组值
        for i in [0usize, 1, 5, 100, 200, 400] {
            let x = TERRAIN_X_MIN + i as f64 * TERRAIN_SPACING;
            assert!((t.height(x) - t.samples()[i]).abs() < 1e-12, "格点 {i} 应取原值");
        }
        // 中点 = 两端平均（线性插值）
        let i = 100;
        let xm = TERRAIN_X_MIN + (i as f64 + 0.5) * TERRAIN_SPACING;
        let mid = (t.samples()[i] + t.samples()[i + 1]) / 2.0;
        assert!((t.height(xm) - mid).abs() < 1e-12, "中点应为两端平均");
    }

    #[test]
    fn height_clamps_outside_domain() {
        // 边界：域外 x 钳到域边（世界边缘外地面延伸，高度不变）
        let t = Terrain::new(TerrainPreset::Mountain, 3);
        assert_eq!(t.height(-100.0), t.height(TERRAIN_X_MIN));
        assert_eq!(t.height(100.0), t.height(TERRAIN_X_MAX));
        assert_eq!(t.height(f64::NEG_INFINITY), t.height(TERRAIN_X_MIN));
    }

    #[test]
    fn height_is_finite_for_all_presets() {
        // 边界：任意 x 查询有限（无 NaN/inf 污染世界）
        for preset in [TerrainPreset::Flat, TerrainPreset::Hills, TerrainPreset::Mountain] {
            let t = Terrain::new(preset, 42);
            for i in -60..=60 {
                assert!(t.height(i as f64 * 1.7).is_finite(), "{preset:?} 高度应有限");
            }
        }
    }

    // === 生成（四件套②：fBm+pow 压谷+坡度钳制+种子可复现）===

    #[test]
    fn same_seed_same_terrain() {
        // 确定性：同 (preset, seed) 产同一高度数组（逐位相等）
        let a = Terrain::new(TerrainPreset::Mountain, 123);
        let b = Terrain::new(TerrainPreset::Mountain, 123);
        assert_eq!(a.samples(), b.samples(), "同种子必同地形");
        assert_eq!(a.preset, b.preset);
        assert_eq!(a.seed, b.seed);
    }

    #[test]
    fn different_seed_different_terrain() {
        // 读写：换种子换地形（哈希雪崩：相邻种子也应有可观差异）
        let a = Terrain::new(TerrainPreset::Hills, 1);
        let b = Terrain::new(TerrainPreset::Hills, 2);
        assert_ne!(a.samples(), b.samples(), "异种子应异地形");
    }

    #[test]
    fn heights_within_preset_amplitude() {
        // 边界：幅度钳制——heights ⊆ [-ampl, +ampl]（fBm 归一后 |v|≤1，
        // pow 保号不放大，坡度钳制只减不增全局极值）
        for (preset, ampl) in [(TerrainPreset::Hills, 3.0), (TerrainPreset::Mountain, 8.0)] {
            let t = Terrain::new(preset, 99);
            for &h in t.samples() {
                assert!(
                    h.abs() <= ampl + 1e-9,
                    "{preset:?} 高度 {h} 超出幅度 {ampl}"
                );
            }
        }
    }

    #[test]
    fn slope_clamp_bounds_neighbor_delta() {
        // 边界：坡度钳制——相邻采样高度差 ≤ max_slope * spacing（防不可爬陡坡）
        for (preset, max_slope) in [(TerrainPreset::Hills, 1.0), (TerrainPreset::Mountain, 1.5)] {
            let t = Terrain::new(preset, 5);
            let max_delta = max_slope * TERRAIN_SPACING + 1e-9;
            for i in 1..t.sample_count() {
                let d = (t.samples()[i] - t.samples()[i - 1]).abs();
                assert!(d <= max_delta, "{preset:?} 采样 {i} 坡度 {d} 超限 {max_delta}");
            }
        }
    }

    #[test]
    fn hills_and_mountain_have_relief() {
        // 读写：非平地预设确有起伏（最大高差可观——生成器不是恒等零）
        let h = Terrain::new(TerrainPreset::Hills, 11);
        let m = Terrain::new(TerrainPreset::Mountain, 11);
        let spread = |t: &Terrain| {
            let (mn, mx) = t.samples().iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
            mx - mn
        };
        assert!(spread(&h) > 1.0, "丘陵应有起伏，实际高差 {}", spread(&h));
        assert!(spread(&m) > 3.0, "山地应有明显起伏，实际高差 {}", spread(&m));
        assert!(spread(&m) >= spread(&h), "山地比丘陵更起伏");
    }

    // === CCD 落点查表（四件套③：路径×地形首接触）===

    #[test]
    fn ccd_flat_no_contact_when_above() {
        // 边界：全程在地面之上 → None（不落地）
        let t = Terrain::flat();
        assert_eq!(t.segment_first_contact((0.0, 10.0), (5.0, 8.0), 0.5), None);
        // 平飞
        assert_eq!(t.segment_first_contact((0.0, 1.0), (10.0, 1.0), 0.5), None);
    }

    #[test]
    fn ccd_flat_vertical_fall() {
        // 读写：平地垂直下落穿半径线 → t 解析正确
        let t = Terrain::flat();
        // 从 (3, 4) 落到 (3, -6)，半径 0.5：穿 y=0.5 于 t = 3.5/10
        let got = t.segment_first_contact((3.0, 4.0), (3.0, -6.0), 0.5);
        assert!((got.unwrap() - 0.35).abs() < 1e-12, "垂直下落 t 应为 0.35");
    }

    #[test]
    fn ccd_flat_diagonal_fall() {
        // 读写：平地斜射落地——与旧平地口径 (y0-r)/(y0-y1) 一致
        let t = Terrain::flat();
        let from = (0.0, 1.0);
        let to = (20.0, -1.0); // 穿 y=0.5 于 t = 0.5/2 = 0.25
        let got = t.segment_first_contact(from, to, 0.5).unwrap();
        assert!((got - 0.25).abs() < 1e-12, "斜射 t 应为 0.25");
    }

    #[test]
    fn ccd_start_underground_returns_zero() {
        // 边界：起步已在地下（异常态兜底）→ Some(0.0) 立即触地
        let t = Terrain::new(TerrainPreset::Mountain, 8);
        // 找一个地面高度 > 5 的 x，从 y=3（地下）出发
        let x_deep = t
            .samples()
            .iter()
            .enumerate()
            .find(|(_, &h)| h > 5.0)
            .map(|(i, _)| TERRAIN_X_MIN + i as f64 * TERRAIN_SPACING);
        // 硬化（R84 审查 P2）：找不到高采样点必须响亮失败，禁条件空转假绿
        //（山地 seed=8 峰值锚 >4.0，实测 ≈5.92，>5.0 前提恒成立）
        let x = x_deep.expect("山地 seed=8 必有高度 >5.0 的采样点（峰锚在案）");
        assert_eq!(
            t.segment_first_contact((x, 3.0), (x + 1.0, 2.0), 0.5),
            Some(0.0),
            "地下起步应立即触地"
        );
    }

    #[test]
    fn ccd_hill_blocks_flight_that_flat_would_pass() {
        // 集成：山丘挡弹——平地上不落地的弹道，撞上山坡应落地（逐段查表生效）
        let t = Terrain::new(TerrainPreset::Mountain, 8);
        // 找最高采样点
        let (peak_i, &peak_h) = t
            .samples()
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let peak_x = TERRAIN_X_MIN + peak_i as f64 * TERRAIN_SPACING;
        assert!(peak_h > 4.0, "山地最高点应 >4 格（实际 {peak_h}）");
        // 低弹道横穿山腰：从峰左侧向右平飞，起点在峰上空安全高度
        //（起点 y 必须 > 全程最低安全线 ground(x)+r，否则起步即"地下"）
        let y = peak_h + 1.0;
        let start = peak_x - 2.0;
        let end = peak_x + 2.0;
        // 前提自检：起点确在地面之上
        assert!(y > t.height(start) + 0.3, "测试前提：起点应在地上");
        let contact = t.segment_first_contact((start, y), (end, y - peak_h * 0.9), 0.3);
        // 向下斜穿山体必触地
        let tc = contact.expect("下压弹道撞山坡应落地");
        let lx = start + (end - start) * tc;
        let ly = y + (y - peak_h * 0.9 - y) * tc;
        let gap = (ly - t.height(lx) - 0.3).abs();
        assert!(gap < 1e-6, "落点应贴地面（弹心=地面+半径），lx={lx} ly={ly} ground={} gap={gap}", t.height(lx));
        // 落点在路径前半段（山坡上，而非冲过峰后）
        assert!(tc < 0.9, "应在穿越峰区时落地（t={tc}）");
    }

    #[test]
    fn ccd_valley_does_not_catch_high_arc() {
        // 边界：高弧线越过山谷不落地（查表不误报——只有真穿面才报）
        let t = Terrain::new(TerrainPreset::Mountain, 8);
        // 找最低点（谷）
        let (valley_i, &valley_h) = t
            .samples()
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let valley_x = TERRAIN_X_MIN + valley_i as f64 * TERRAIN_SPACING;
        // 谷上方 20 格平飞：绝不落地
        assert_eq!(
            t.segment_first_contact((valley_x - 5.0, valley_h + 20.0), (valley_x + 5.0, valley_h + 20.0), 0.5),
            None,
            "谷顶 20 格平飞不应落地"
        );
    }

    #[test]
    fn ccd_mountain_crossing_first_contact_both_directions_match_brute_force() {
        // D-016 R84 复审返工锚：左向穿山一拍整段穿越（表面被穿两次）时必须返回
        // 时间上"最先"的接触（入口侧），不是出口侧——方向序错误的回归锚。
        // 参考=独立暴力扫描（1e-6 步长 + 二分精化），不复用被测函数逻辑。
        let terrain = Terrain::new(TerrainPreset::Mountain, 8);
        let (peak_i, &peak_h) = terrain
            .samples()
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let peak_x = TERRAIN_X_MIN + peak_i as f64 * TERRAIN_SPACING;
        let c = peak_h - 0.5; // 水平弹道高度：峰下 0.5，必穿山面两次
        let r = 0.3;
        let (xl, xr) = (peak_x - 5.0, peak_x + 5.0);
        // 前提自检：两端点都在接触线上方（路径表面被穿两次，方向序才有意义）
        for &x in &[xl, xr] {
            assert!(c > terrain.height(x) + r, "前提：端点 x={x} 应在接触线上方");
        }
        // 独立暴力参考：t∈[0,1] 步长 1e-6 扫描首次穿透，命中后二分 40 轮精化
        let brute_first_contact = |from: (f64, f64), to: (f64, f64)| -> Option<f64> {
            const STEPS: u32 = 1_000_000;
            let dt = 1.0 / STEPS as f64;
            let pen = |tt: f64| -> bool {
                let x = from.0 + (to.0 - from.0) * tt;
                let y = from.1 + (to.1 - from.1) * tt;
                y < terrain.height(x) + r
            };
            if pen(0.0) {
                return Some(0.0); // 起步穿透兜底（本测试路径不会走到）
            }
            let mut prev_t = 0.0f64;
            for s in 1..=STEPS {
                let tt = s as f64 * dt;
                if pen(tt) {
                    let (mut lo, mut hi) = (prev_t, tt);
                    for _ in 0..40 {
                        let mid = (lo + hi) / 2.0;
                        if pen(mid) {
                            hi = mid;
                        } else {
                            lo = mid;
                        }
                    }
                    return Some((lo + hi) / 2.0);
                }
                prev_t = tt;
            }
            None
        };
        // 右向（dx>0）：修前已碰巧正确（左→右扫格=时间序）
        let ref_r = brute_first_contact((xl, c), (xr, c)).expect("右向路径必穿山触地");
        let got_r = terrain
            .segment_first_contact((xl, c), (xr, c), r)
            .expect("右向必返回接触");
        assert!((got_r - ref_r).abs() < 5e-6, "右向首接触 t={got_r} 应=暴力参考 {ref_r}");
        // 左向（dx<0，R84 缺陷主场景）：修前返回出口侧 t≈0.558，真值≈0.099（入口侧）
        let ref_l = brute_first_contact((xr, c), (xl, c)).expect("左向路径必穿山触地");
        let got_l = terrain
            .segment_first_contact((xr, c), (xl, c), r)
            .expect("左向必返回接触");
        assert!(
            (got_l - ref_l).abs() < 5e-6,
            "左向首接触 t={got_l} 应=暴力参考 {ref_l}（时间上最先=入口侧）"
        );
        // 方向不对称性自检：两方向首接触 t 必不同（山形非对称，两交点 t 不互补）
        assert!(
            (ref_l - ref_r).abs() > 1e-3,
            "前提自检失败：两方向首接触 t 应不同（左向={ref_l} 右向={ref_r}），否则锚不住方向序"
        );
    }

    // === 序列化（快照持久化）===

    #[test]
    fn terrain_serialize_roundtrip() {
        // 读写：快照往返保留高度数组/预设/种子（世界重启地形不变形）。
        // 逐值比对用 1e-9 容差：serde_json f64 收发实测存在 1ULP 级漂移
        //（实体坐标快照同路径同性质，非地形独有；物理意义远低于任何判定容差）
        let t = Terrain::new(TerrainPreset::Mountain, 777);
        let json = serde_json::to_string(&t).unwrap();
        let back: Terrain = serde_json::from_str(&json).unwrap();
        assert_eq!(back.preset, TerrainPreset::Mountain);
        assert_eq!(back.seed, 777);
        assert_eq!(back.sample_count(), t.sample_count());
        let max_drift = t
            .samples()
            .iter()
            .zip(back.samples())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f64, f64::max);
        assert!(max_drift < 1e-9, "往返高度漂移 {max_drift} 应 < 1e-9");
    }

    #[test]
    fn terrain_default_is_flat() {
        // 边界：旧世界快照无 terrain 字段 → serde 回退平地（零回归锚）
        let t = Terrain::default();
        assert_eq!(t.preset, TerrainPreset::Flat);
        assert_eq!(t.height(0.0), 0.0);
    }

    // === 预设解析 ===

    #[test]
    fn preset_parse_aliases() {
        // 读写：英文/中文/大小写别名；非法值回退平地（坏值不进世界）
        assert_eq!(TerrainPreset::parse("flat"), TerrainPreset::Flat);
        assert_eq!(TerrainPreset::parse("HILLS"), TerrainPreset::Hills);
        assert_eq!(TerrainPreset::parse("丘陵"), TerrainPreset::Hills);
        assert_eq!(TerrainPreset::parse("mountain"), TerrainPreset::Mountain);
        assert_eq!(TerrainPreset::parse(" 山地 "), TerrainPreset::Mountain);
        assert_eq!(TerrainPreset::parse("moon"), TerrainPreset::Flat, "非法回退平地");
        assert_eq!(TerrainPreset::parse(""), TerrainPreset::Flat);
    }

    #[test]
    fn preset_str_roundtrip() {
        for p in [TerrainPreset::Flat, TerrainPreset::Hills, TerrainPreset::Mountain] {
            assert_eq!(TerrainPreset::parse(p.as_str()), p, "as_str 应可 parse 回");
        }
    }
}

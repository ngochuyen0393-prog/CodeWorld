//! #051 资产镜像仓：全局内容寻址——同图全局一份（sha256 指纹去重，MC 同款）。
//!
//! 世界只搬运不理解（修路不跑车）：快照只带 asset_id 指纹不带货；前端首见
//! 从世界侧 HTTP 直取原图（D-017 的 /assets ServeDir 已在）。镜像仓在本刀的
//! 消费面=配额记账查实大小（bin main.rs 换血配额校验）+指纹去重账；原图字节
//! 落盘/伺服仍归 D-017 avatars.rs（镜像仓只存元数据，不存内容副本）。
//!
//! 装戴清单指向镜像仓：沙箱 SandboxData.assets 里的 asset_id 在镜像仓可查
//! （查不到=大小按 0 记并 warn，容错不炸换血）。

use crate::net::avatars::AvatarRegistry;
use codeworld_core::mask::AvatarMask;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

/// 镜像仓单资产条目（元数据；内容字节归盘上 PNG，镜像仓不持副本）
/// 本刀消费面仅 asset_id/bytes（配额记账）；其余字段=登记表全量投影（卡面
/// #051 定案结构），挂实体渲染刀消费——留位不删，allow 压制 dead_code。
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct MirroredAsset {
    /// 资产 id（av_+16hex，沿用 D-017 格式）
    pub asset_id: String,
    /// 内容指纹（去重判据：同 sha256=同资产，不重复入仓账）
    pub sha256: String,
    /// 字节数（配额记账用，=盘上 PNG 实大小）
    pub bytes: usize,
    pub name: String,
    pub orig_w: u32,
    pub orig_h: u32,
    /// 世界高度格数（D-017 只登记不消费；随行携带保持镜像仓条目=登记表全量投影）
    pub height_world: f64,
    /// 128 表（渲染/物理共用同源；本刀不消费，挂实体渲染刀用）
    pub mask: AvatarMask,
}

/// 资产镜像仓：asset_id -> 条目 + sha256 -> 首个登记 id 的去重账。
///
/// 同图全局一份的判据=unique_contents()（不同 asset_id 同内容只记一份内容账；
/// id 层全量可查——装戴清单里任何一个 id 都必须能查到大小，缺查=记账塌方）。
#[derive(Debug, Default)]
pub(crate) struct AssetMirror {
    /// asset_id -> 条目（全量可查；同内容多 id 各有条目，共享同一指纹）
    assets: HashMap<String, MirroredAsset>,
    /// sha256 -> 首个登记该内容的 asset_id（内容寻址去重账；MC 同款"同图一份"）
    by_sha: HashMap<String, String>,
}

impl AssetMirror {
    /// 空镜像仓（测试与 ws 禁用装配用）
    pub fn new() -> Self {
        Self::default()
    }

    /// 启动建仓：从 D-017 登记表全量投影，每资产读盘上 PNG 算 sha256 指纹。
    /// 同指纹去重（后到 id 复用先到 id 的内容账，日志留痕）；PNG 不可读的条目
    /// 跳过（镜像仓缺条目=配额查大小按 0 记 warn，与登记表"坏文件跳过不拖垮
    /// 启动"同一容错哲学）。
    pub fn from_registry(reg: &AvatarRegistry) -> Self {
        let mut mirror = Self::new();
        for asset in reg.list() {
            let png_path = avatar_png_path(reg.dir(), &asset.asset_id);
            let Ok(bytes) = std::fs::read(&png_path) else {
                tracing::warn!(
                    asset_id = %asset.asset_id,
                    path = %png_path.display(),
                    "[mirror] PNG 不可读，镜像仓跳过该条目（配额按 0 记）"
                );
                continue;
            };
            let mirrored = MirroredAsset {
                asset_id: asset.asset_id.clone(),
                sha256: sha256_hex(&bytes),
                bytes: bytes.len(),
                name: asset.name,
                orig_w: asset.orig_w,
                orig_h: asset.orig_h,
                height_world: asset.height_world,
                mask: asset.mask,
            };
            mirror.insert(mirrored);
        }
        tracing::info!(
            entries = mirror.assets.len(),
            unique_contents = mirror.unique_contents(),
            "[mirror] 资产镜像仓建仓完成（同图全局一份）"
        );
        mirror
    }

    /// 入仓：返回 true=同指纹复用已有内容账（去重发生）；false=新内容首记。
    /// id 层条目恒建（装戴清单任何 id 都要查得到大小）。
    pub fn insert(&mut self, asset: MirroredAsset) -> bool {
        let duplicated = self.by_sha.contains_key(&asset.sha256);
        if duplicated {
            tracing::info!(
                asset_id = %asset.asset_id,
                sha = %asset.sha256,
                "[mirror] 同指纹去重：复用既有内容账（同图全局一份）"
            );
        } else {
            self.by_sha
                .insert(asset.sha256.clone(), asset.asset_id.clone());
        }
        self.assets.insert(asset.asset_id.clone(), asset);
        duplicated
    }

    /// 按 id 查条目（挂实体渲染刀消费；本刀仅测试用，无此标注 dead_code 误报）
    #[cfg(test)]
    pub fn get(&self, id: &str) -> Option<&MirroredAsset> {
        self.assets.get(id)
    }

    /// 按 id 查字节数（配额记账用）
    pub fn size_of(&self, id: &str) -> Option<usize> {
        self.assets.get(id).map(|a| a.bytes)
    }

    /// 去重后的内容份数（"同图全局一份"的判据：两 id 同图 → 1）
    pub fn unique_contents(&self) -> usize {
        self.by_sha.len()
    }

    /// 条目数（id 层全量；本刀仅测试用，无此标注 dead_code 误报）
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.assets.len()
    }

    /// 是否为空（本刀仅测试用，无此标注 dead_code 误报）
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }
}

/// 字节流 sha256 指纹（小写 hex）
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 盘上 PNG 路径拼装（镜像仓/前端取图同一路径口径；ServeDir 同源）
pub(crate) fn avatar_png_path(dir: &Path, asset_id: &str) -> std::path::PathBuf {
    dir.join(format!("{asset_id}.png"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::avatars::{store_upload, UploadInput};

    /// 测试助手：RGBA8 像素 -> PNG 字节（png crate 编码，测试自产 fixtures；
    /// 与 avatars.rs tests 同款——私有 mod 不跨文件复用，小助手复制即可）
    fn encode_rgba_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("PNG 头应可写");
            writer.write_image_data(rgba).expect("PNG 像素应可写");
        }
        out
    }

    /// 测试助手：全实心小 PNG（连通合法图，过登记表掩码管线）
    fn solid_png(w: u32, h: u32, alpha: u8) -> Vec<u8> {
        let mut px = Vec::new();
        for _ in 0..w * h {
            px.extend_from_slice(&[255, 0, 0, alpha]);
        }
        encode_rgba_png(w, h, &px)
    }

    /// 测试助手：临时资产目录（测后留存由 OS 清理，路径带进程号防并行撞）
    fn tmp_assets_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codeworld-mirror-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir); // 上轮残留清掉（幂等）
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 测试助手：上传一张图，返回登记条目
    fn upload(reg: &AvatarRegistry, png: &[u8], name: &str) -> crate::net::avatars::AvatarAsset {
        store_upload(
            reg,
            UploadInput {
                png,
                name,
                height_world: None,
            },
        )
        .expect("合法上传应成功")
    }

    #[test]
    fn sha256_hex_is_stable_and_hex() {
        // 规则：指纹函数稳定且 64 位小写 hex（去重判据的确定性前提）
        let a = sha256_hex(b"codeworld");
        let b = sha256_hex(b"codeworld");
        assert_eq!(a, b, "同内容同指纹");
        assert_eq!(a.len(), 64, "sha256 hex 恒 64 字符");
        assert!(
            a.chars().all(|c| c.is_ascii_hexdigit())
                && a.chars().all(|c| !c.is_ascii_uppercase()),
            "小写 hex"
        );
        assert_ne!(sha256_hex(b"codeworld2"), a, "异内容异指纹");
    }

    #[test]
    fn insert_records_size_and_sha_by_id() {
        // 规则：入仓后按 id 可查大小与指纹（配额记账数据源）
        let mut m = AssetMirror::new();
        let bytes = vec![1u8, 2, 3, 4, 5];
        m.insert(MirroredAsset {
            asset_id: "av_0123456789abcdef".to_string(),
            sha256: sha256_hex(&bytes),
            bytes: bytes.len(),
            name: "test".to_string(),
            orig_w: 1,
            orig_h: 1,
            height_world: 2.0,
            mask: codeworld_core::mask::AvatarMask::new(128, 128),
        });
        assert_eq!(m.size_of("av_0123456789abcdef"), Some(5));
        assert_eq!(m.get("av_0123456789abcdef").unwrap().sha256, sha256_hex(&bytes));
        assert_eq!(m.unique_contents(), 1);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn size_of_unknown_id_is_none() {
        // 负面：镜像仓无此 id → None（调用方按 0 记 warn，不炸换血）
        let m = AssetMirror::new();
        assert_eq!(m.size_of("av_ffffffffffffffff"), None);
        assert!(m.is_empty());
    }

    #[test]
    fn from_registry_dedups_same_content_to_one_copy() {
        // 负面（卡面负面路径③）：同内容两张图（两个 asset_id）入仓——id 层全量
        // 可查（都查得到大小），内容层去重为一份（unique_contents()==1，MC 同款）
        let dir = tmp_assets_dir("dedup");
        let reg = AvatarRegistry::load(&dir);
        let png = solid_png(64, 64, 255);
        let a = upload(&reg, &png, "first");
        let b = upload(&reg, &png, "second"); // 同内容不同 id
        assert_ne!(a.asset_id, b.asset_id, "两 id 应不同");
        let mirror = AssetMirror::from_registry(&reg);
        assert_eq!(mirror.len(), 2, "id 层全量可查（装戴清单任一 id 查得到）");
        assert_eq!(mirror.unique_contents(), 1, "同图内容层只记一份");
        assert_eq!(
            mirror.size_of(&a.asset_id),
            mirror.size_of(&b.asset_id),
            "同内容同大小"
        );
        assert_eq!(
            mirror.get(&a.asset_id).unwrap().sha256,
            mirror.get(&b.asset_id).unwrap().sha256,
            "同内容同指纹"
        );
    }

    #[test]
    fn from_registry_counts_distinct_contents_separately() {
        // 边界：不同内容 → 内容账分开记（去重不是合并——异图恒多份）
        let dir = tmp_assets_dir("distinct");
        let reg = AvatarRegistry::load(&dir);
        upload(&reg, &solid_png(64, 64, 255), "red");
        upload(&reg, &solid_png(32, 64, 128), "half"); // 不同尺寸=不同字节=不同指纹
        let mirror = AssetMirror::from_registry(&reg);
        assert_eq!(mirror.len(), 2);
        assert_eq!(mirror.unique_contents(), 2, "异图恒多份");
    }

    #[test]
    fn from_registry_skips_unreadable_png() {
        // 负面：登记表有条目但盘上 PNG 丢失 → 镜像仓跳过该条目（容错不炸启动）
        let dir = tmp_assets_dir("missing");
        let reg = AvatarRegistry::load(&dir);
        let asset = upload(&reg, &solid_png(64, 64, 255), "gone");
        std::fs::remove_file(dir.join(format!("{}.png", asset.asset_id))).unwrap();
        let mirror = AssetMirror::from_registry(&reg);
        assert!(mirror.is_empty(), "PNG 缺失条目应跳过");
        assert_eq!(mirror.size_of(&asset.asset_id), None);
    }
}

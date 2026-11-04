//! arm64 目标进程的地址空间几何参数。
//!
//! 页大小可以是 4K / 16K / 64K，VA 位宽可以是 39 / 48 / 52（LVA），
//! 所有位移、掩码与大页尺寸都必须从这里派生，不能硬编码 4K。

/// 页大小、VA 位宽与由两者推导出的页表几何。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetGeometry {
    pub page_size: u64,
    pub va_bits: u32,
}

impl Default for TargetGeometry {
    fn default() -> Self {
        Self {
            page_size: 4096,
            va_bits: 48,
        }
    }
}

impl TargetGeometry {
    pub fn new(page_size: u64, va_bits: u32) -> Self {
        assert!(
            page_size.is_power_of_two() && page_size >= 4096,
            "页大小必须是 >= 4K 的 2 的幂"
        );
        assert!((32..=52).contains(&va_bits), "VA 位宽必须在 32..=52 之间");
        Self { page_size, va_bits }
    }

    pub fn page_shift(&self) -> u32 {
        self.page_size.trailing_zeros()
    }

    /// arm64 页表索引位数随页大小变化：4K -> 9、16K -> 11、64K -> 13。
    pub fn table_index_bits(&self) -> u32 {
        self.page_shift() - 3
    }

    /// 一个 PMD 项覆盖的字节数，也是 THP 大页的尺寸。
    pub fn pmd_size(&self) -> u64 {
        self.page_size << self.table_index_bits()
    }

    /// 剥离 TBI/MTE 顶层 tag，并按 VA 位宽做符号扩展。
    pub fn canonicalize(&self, addr: u64) -> u64 {
        let untagged = addr & 0x00ff_ffff_ffff_ffff;
        let shift = 64 - self.va_bits;
        (((untagged << shift) as i64) >> shift) as u64
    }

    pub fn page_of(&self, addr: u64) -> u64 {
        self.canonicalize(addr) >> self.page_shift()
    }

    /// 根据进程内最高的用户态地址推断 VA 位宽。
    ///
    /// 39 位地址是 48 位的子集，仅凭地址无法区分，按 48 位处理即可。
    pub fn va_bits_for_max_addr(max_user_addr: u64) -> u32 {
        if max_user_addr >= 1u64 << 48 {
            52
        } else {
            48
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_table_geometry_follows_granule() {
        assert_eq!(TargetGeometry::new(4096, 48).pmd_size(), 2 * 1024 * 1024);
        assert_eq!(TargetGeometry::new(16384, 48).pmd_size(), 32 * 1024 * 1024);
        assert_eq!(TargetGeometry::new(65536, 48).pmd_size(), 512 * 1024 * 1024);
    }

    #[test]
    fn canonicalize_strips_top_byte_tag() {
        let geo = TargetGeometry::new(4096, 48);
        assert_eq!(geo.canonicalize(0xAB00_7fff_ffff_f000), 0x7fff_ffff_f000);
    }

    #[test]
    fn canonicalize_sign_extends_kernel_address() {
        let geo = TargetGeometry::new(4096, 48);
        assert_eq!(
            geo.canonicalize(0x0000_ffff_8000_0000),
            0xffff_ffff_8000_0000
        );
    }

    #[test]
    fn page_of_uses_target_granule() {
        let geo = TargetGeometry::new(65536, 48);
        assert_eq!(geo.page_of(0x1_0000), 1);
    }
}

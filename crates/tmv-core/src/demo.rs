//! 合成数据：在没有 Linux /proc 的机器上预览界面（`tmv --demo`）。

use crate::pagemap::{self, PageScan, PageState};
use crate::proc::ProcessSnapshot;
use crate::vma::{Perms, SmapsStats, Vma};

pub fn snapshot() -> ProcessSnapshot {
    let mut vmas = Vec::new();
    let mut cursor = 0x0000_5500_0000_0000u64;

    add(
        &mut vmas,
        &mut cursor,
        0x2_0000,
        "r-xp",
        "/usr/bin/tmv",
        0.95,
        0.60,
    );
    add(
        &mut vmas,
        &mut cursor,
        0x4_0000,
        "r--p",
        "/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1",
        0.90,
        0.50,
    );
    add(
        &mut vmas,
        &mut cursor,
        0x1e_0000,
        "r-xp",
        "/lib/aarch64-linux-gnu/libc.so.6",
        0.90,
        0.45,
    );
    add(
        &mut vmas,
        &mut cursor,
        0xc_0000,
        "r-xp",
        "/lib/aarch64-linux-gnu/libm.so.6",
        0.85,
        0.45,
    );
    add(
        &mut vmas,
        &mut cursor,
        0x24_0000,
        "r-xp",
        "/usr/lib/aarch64-linux-gnu/libstdc++.so.6",
        0.80,
        0.40,
    );
    add(
        &mut vmas,
        &mut cursor,
        0x60_0000,
        "r-xp",
        "/usr/lib/aarch64-linux-gnu/libQt6Core.so.6",
        0.70,
        0.30,
    );
    add(
        &mut vmas,
        &mut cursor,
        0x48_0000,
        "r-xp",
        "/usr/lib/aarch64-linux-gnu/libQt6Gui.so.6",
        0.65,
        0.30,
    );
    add(
        &mut vmas,
        &mut cursor,
        0x50_0000,
        "r-xp",
        "/usr/lib/aarch64-linux-gnu/libcrypto.so.3",
        0.55,
        0.25,
    );
    add(
        &mut vmas,
        &mut cursor,
        0x140_0000,
        "r--p",
        "/usr/lib/locale/locale-archive",
        0.30,
        0.20,
    );
    add(
        &mut vmas,
        &mut cursor,
        0xc0_0000,
        "r--p",
        "/usr/share/fonts/noto/NotoSansCJK.ttc",
        0.25,
        0.20,
    );

    add(
        &mut vmas,
        &mut cursor,
        0x3200_0000,
        "rw-p",
        "[heap]",
        0.50,
        0.98,
    );
    add(&mut vmas, &mut cursor, 0x600_0000, "rw-p", "", 0.70, 0.95);
    add(&mut vmas, &mut cursor, 0x400_0000, "rw-p", "", 0.65, 0.95);
    add(&mut vmas, &mut cursor, 0x300_0000, "rw-p", "", 0.60, 0.95);
    add(&mut vmas, &mut cursor, 0x200_0000, "rw-p", "", 0.55, 0.95);
    add(&mut vmas, &mut cursor, 0x180_0000, "rw-p", "", 0.50, 0.95);

    add(&mut vmas, &mut cursor, 0x100_0000, "rw-s", "", 0.50, 0.15);
    add(
        &mut vmas,
        &mut cursor,
        0x100_0000,
        "rw-p",
        "[anon:jemalloc]",
        0.60,
        0.90,
    );

    add(&mut vmas, &mut cursor, 0x100_0000, "r-xp", "", 0.90, 0.99);
    add(&mut vmas, &mut cursor, 0x80_0000, "r-xp", "", 0.85, 0.99);
    add(&mut vmas, &mut cursor, 0x40_0000, "r-xp", "", 0.80, 0.99);
    add(&mut vmas, &mut cursor, 0x20_0000, "r-xp", "", 0.75, 0.99);

    add(
        &mut vmas,
        &mut cursor,
        0x80_0000,
        "rw-p",
        "[stack]",
        0.03,
        0.90,
    );
    add(&mut vmas, &mut cursor, 0x2000, "r-xp", "[vdso]", 1.0, 0.10);
    add(&mut vmas, &mut cursor, 0x2000, "r--p", "[vvar]", 1.0, 0.10);

    ProcessSnapshot::from_vmas(4242, vmas, true)
}

fn add(
    out: &mut Vec<Vma>,
    cursor: &mut u64,
    len: u64,
    perms: &str,
    path: &str,
    rss_ratio: f64,
    pss_ratio: f64,
) {
    let start = *cursor;
    let end = start + len;
    let rss = (len as f64 * rss_ratio) as u64;
    let pss = (rss as f64 * pss_ratio) as u64;
    let mut stats = SmapsStats {
        size: len,
        rss,
        pss,
        anonymous: rss,
        ..Default::default()
    };
    if perms.contains('s') {
        stats.shared_dirty = rss;
    } else {
        stats.private_dirty = rss;
    }

    out.push(Vma {
        start,
        end,
        perms: Perms::parse(perms).expect("示例权限合法"),
        offset: 0,
        dev: "fd:01".to_string(),
        inode: 0,
        pathname: path.to_string(),
        stats: Some(stats),
    });
    *cursor = end + 0x1000;
}

/// 生成一段合成的页级数据，用于在 macOS 上预览页图。
pub fn page_scan(start: u64, end: u64, page_size: u64, max_samples: usize) -> PageScan {
    let first = start / page_size;
    let last = end.div_ceil(page_size);
    let pages = last.saturating_sub(first);
    let budget = max_samples.max(1) as u64;
    let stride = pages.div_ceil(budget).max(1);

    let mut states = Vec::new();
    let mut rng = first ^ 0x9e37_79b9_7f4a_7c15;
    let mut index = first;
    while index < last && (states.len() as u64) < budget {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let roll = ((rng >> 33) & 0xff) as u32;

        let mut state = PageState::default();
        if roll < 106 {
            state.present = true;
            state.pfn = 0x1000 + (index % 0x2_0000);
        } else if roll < 196 {
            state.present = true;
            state.file = true;
            state.pfn = 0x4_0000 + (index % 0x2_0000);
        } else if roll < 222 {
            state.swapped = true;
        }

        if state.present {
            state.exclusive = !state.file;
            if roll % 23 == 0 {
                state.shared = true;
            }
            if !state.file && (index / 512) % 3 == 0 {
                state.thp = true;
            }
        }

        states.push(state);
        index += stride;
    }

    let summary = pagemap::summarize(&states);
    PageScan {
        start: first * page_size,
        end: last * page_size,
        page_size,
        pages,
        sampled: stride > 1,
        states,
        summary,
    }
}

/// 合成一段内存内容，用于在没有真实进程时预览内存查看器。
pub fn memory(base: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|offset| {
            let address = base.wrapping_add(offset as u64);
            // 混入可打印字符，让 ASCII 栏也能看出结构。
            match address % 7 {
                0 => b'\n',
                3 => 0,
                _ => b'a' + (address % 26) as u8,
            }
        })
        .collect()
}

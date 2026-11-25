//! Replay of the Linux/glibc preload stream. Requested live bytes are NOT RSS.
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read};

pub const RECORD_SIZE: usize = 64;
const MAGIC: u64 = 0x3150414548564d54;
#[derive(Debug, Clone)]
pub struct Allocation {
    pub size: u64,
    pub caller: u64,
    pub stack: Vec<u64>,
    pub resize_stack: Vec<u64>,
}
#[derive(Debug, Default, Serialize)]
pub struct Site {
    pub caller: u64,
    pub live_bytes: u64,
    pub live_count: u64,
    pub allocated_bytes: u64,
    pub freed_bytes: u64,
    pub peak_bytes: u64,
}
#[derive(Default)]
pub struct Ledger {
    pub pid: u64,
    pub ended: bool,
    pub records: u64,
    pub unknown_frees: u64,
    pub sites: BTreeMap<u64, Site>,
    live: HashMap<u64, Allocation>,
    pending: Vec<u8>,
    stack_prefix: Vec<u64>,
    stack_expected: usize,
}
impl Ledger {
    /// Retains partial trailing records when following a file being written.
    pub fn read(&mut self, reader: &mut impl Read) -> io::Result<()> {
        let mut buffer = [0; 65536];
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            self.pending.extend_from_slice(&buffer[..n]);
            let complete = self.pending.len() / RECORD_SIZE * RECORD_SIZE;
            let records: Vec<_> = self.pending[..complete]
                .chunks_exact(RECORD_SIZE)
                .map(|chunk| {
                    std::array::from_fn::<_, 8, _>(|i| {
                        u64::from_le_bytes(chunk[i * 8..i * 8 + 8].try_into().unwrap())
                    })
                })
                .collect();
            self.pending.drain(..complete);
            for record in records {
                self.apply(record)?;
            }
        }
        Ok(())
    }
    pub fn allocations(&self) -> impl Iterator<Item = (&u64, &Allocation)> {
        self.live.iter()
    }

    pub fn partial(&self) -> bool {
        !self.pending.is_empty() || !self.stack_prefix.is_empty()
    }
    fn apply(&mut self, w: [u64; 8]) -> io::Result<()> {
        let bad = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid heap trace: magic, sequence, PID or lifecycle mismatch",
            )
        };
        if w[0] != MAGIC || w[2] != self.records + 1 || self.ended {
            return Err(bad());
        }
        if self.records == 0 {
            if w[1] != 0 || w[3] == 0 {
                return Err(bad());
            }
            self.pid = w[3];
        } else if w[3] != self.pid || w[1] == 0 {
            return Err(bad());
        }
        if w[1] != 5 && self.stack_expected != self.stack_prefix.len() {
            return Err(bad());
        }
        if !self.stack_prefix.is_empty() && !matches!(w[1], 1 | 3 | 5) {
            return Err(bad());
        }
        match w[1] {
            0 => {}
            1 => self.add(w[4], w[6], w[7])?,
            2 => {
                self.remove(w[4]);
            }
            3 => {
                let old = self.remove(w[5]);
                let caller = old.as_ref().map(|a| a.caller).unwrap_or(w[7]);
                if w[4] != 0 {
                    self.add(w[4], w[6], caller)?;
                    if let Some(old) = old {
                        let new = self.live.get_mut(&w[4]).unwrap();
                        new.resize_stack = std::mem::replace(&mut new.stack, old.stack);
                    }
                } else {
                    self.stack_prefix.clear();
                    self.stack_expected = 0;
                }
            }
            5 => {
                if w[6] == 0 || w[6] > 32 || w[5] != self.stack_prefix.len() as u64 || w[7] == 0 {
                    return Err(bad());
                }
                if self.stack_prefix.is_empty() {
                    self.stack_expected = w[6] as usize;
                }
                if w[6] as usize != self.stack_expected
                    || self.stack_prefix.len() >= self.stack_expected
                {
                    return Err(bad());
                }
                self.stack_prefix.push(w[7]);
            }
            4 => self.ended = true,
            _ => return Err(bad()),
        }
        self.records += 1;
        Ok(())
    }
    fn add(&mut self, ptr: u64, size: u64, caller: u64) -> io::Result<()> {
        if ptr == 0 || self.live.contains_key(&ptr) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "duplicate/null live allocation",
            ));
        }
        self.live.insert(
            ptr,
            Allocation {
                size,
                caller,
                stack: std::mem::take(&mut self.stack_prefix),
                resize_stack: Vec::new(),
            },
        );
        self.stack_expected = 0;
        let site = self.sites.entry(caller).or_insert_with(|| Site {
            caller,
            ..Site::default()
        });
        site.live_bytes += size;
        site.live_count += 1;
        site.allocated_bytes += size;
        site.peak_bytes = site.peak_bytes.max(site.live_bytes);
        Ok(())
    }
    fn remove(&mut self, ptr: u64) -> Option<Allocation> {
        if ptr == 0 {
            return None;
        }
        let a = self.live.remove(&ptr);
        if let Some(a) = &a {
            let site = self.sites.get_mut(&a.caller).unwrap();
            site.live_bytes -= a.size;
            site.live_count -= 1;
            site.freed_bytes += a.size;
        } else {
            self.unknown_frees += 1;
        }
        a
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn record(op: u64, seq: u64, ptr: u64, old: u64, size: u64, caller: u64) -> Vec<u8> {
        [MAGIC, op, seq, 42, ptr, old, size, caller]
            .into_iter()
            .flat_map(u64::to_le_bytes)
            .collect()
    }
    #[test]
    fn reuse_realloc_and_cross_thread_free() {
        let mut l = Ledger::default();
        for r in [
            record(0, 1, 0, 0, 0, 0),
            record(1, 2, 100, 0, 40, 10),
            record(3, 3, 200, 100, 80, 20),
            record(2, 4, 200, 0, 0, 0),
            record(1, 5, 100, 0, 12, 20),
            record(4, 6, 0, 0, 0, 0),
        ] {
            l.read(&mut &r[..]).unwrap();
        }
        assert_eq!(l.sites[&10].live_bytes, 0);
        assert_eq!(l.sites[&10].peak_bytes, 80);
        assert_eq!(l.sites[&20].live_bytes, 12);
        assert!(l.ended);
    }
    #[test]
    fn partial_record_and_corruption() {
        let r = record(0, 1, 0, 0, 0, 0);
        let mut l = Ledger::default();
        l.read(&mut &r[..17]).unwrap();
        assert!(l.partial());
        l.read(&mut &r[17..]).unwrap();
        assert!(!l.partial());
        assert!(l.read(&mut &record(1, 3, 100, 0, 4, 10)[..]).is_err());
    }
    #[test]
    fn zero_realloc_and_unknown_free() {
        let mut l = Ledger::default();
        for r in [
            record(0, 1, 0, 0, 0, 0),
            record(1, 2, 100, 0, 10, 10),
            record(3, 3, 0, 100, 0, 20),
            record(2, 4, 999, 0, 0, 0),
        ] {
            l.read(&mut &r[..]).unwrap();
        }
        assert_eq!(l.sites[&10].live_count, 0);
        assert_eq!(l.unknown_frees, 1);
    }
}

#[cfg(test)]
mod stack_tests {
    use super::*;
    fn r(op: u64, seq: u64, ptr: u64, old: u64, size: u64, pc: u64) -> Vec<u8> {
        [MAGIC, op, seq, 42, ptr, old, size, pc]
            .into_iter()
            .flat_map(u64::to_le_bytes)
            .collect()
    }
    #[test]
    fn stack_prefix_across_reads_and_realloc_origin() {
        let mut l = Ledger::default();
        for event in [r(0, 1, 0, 0, 0, 0), r(5, 2, 0, 0, 2, 10)] {
            l.read(&mut &event[..]).unwrap();
        }
        assert!(l.partial());
        assert!(l.live.is_empty());
        for event in [
            r(5, 3, 0, 1, 2, 20),
            r(1, 4, 100, 0, 32, 10),
            r(5, 5, 0, 0, 1, 30),
            r(3, 6, 200, 100, 64, 30),
        ] {
            l.read(&mut &event[..]).unwrap();
        }
        assert_eq!(l.live[&200].stack, vec![10, 20]);
        assert_eq!(l.live[&200].resize_stack, vec![30]);
        assert_eq!(l.live[&200].caller, 10);
        assert!(!l.partial());
    }
    #[test]
    fn rejects_missing_or_unbounded_stack_frames() {
        let mut l = Ledger::default();
        l.read(&mut &r(0, 1, 0, 0, 0, 0)[..]).unwrap();
        l.read(&mut &r(5, 2, 0, 0, 2, 10)[..]).unwrap();
        assert!(l.read(&mut &r(1, 3, 100, 0, 32, 10)[..]).is_err());
        let mut l = Ledger::default();
        l.read(&mut &r(0, 1, 0, 0, 0, 0)[..]).unwrap();
        assert!(l.read(&mut &r(5, 2, 0, 0, 33, 10)[..]).is_err());
    }
}

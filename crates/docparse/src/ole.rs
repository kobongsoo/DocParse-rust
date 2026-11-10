//! OLE/CFB(복합 문서) 읽기 — 구형 Office(.doc/.xls/.ppt)와 HWP5 의 컨테이너.
//!
//! 2007 이전 Office 문서와 HWP5 는 "한 파일 안에 작은 파일시스템"이 들어 있는
//! CFB(Compound File Binary) 형식이다. 그 안의 이름 붙은 스트림(WordDocument,
//! 1Table, PowerPoint Document …)을 꺼내야 본문을 파싱할 수 있다.
//!
//! 파이썬 판은 olefile 라이브러리를 쓰지만, 이 저장소는 크레이트를 늘리지 않는
//! 관례를 지켜 필요한 만큼만 직접 구현한다. 읽기 전용이고, 스트림을 이름으로
//! 찾아 바이트로 돌려주는 것까지만 한다(쓰기·수정 없음).
//!
//! [구조 요약]
//!   · 파일은 512(또는 4096) 바이트 '섹터'로 나뉘고, 섹터 N 의 실제 위치는
//!     (N+1) * 섹터크기 다(맨 앞 한 섹터는 헤더).
//!   · FAT 은 "이 섹터 다음은 몇 번 섹터인가"를 담은 연결 리스트다.
//!   · 작은 스트림(기본 4096 바이트 미만)은 낭비를 줄이려고 '미니 섹터'(64바이트)에
//!     따로 담기고, 그 실체는 루트 엔트리의 스트림 안에 들어 있다.
//!   · 디렉터리 엔트리(128바이트)가 스트림 이름·시작섹터·크기를 갖는다.

use std::collections::HashMap;

/// 체인의 끝.
const END_OF_CHAIN: u32 = 0xFFFF_FFFE;
/// 비어 있는 섹터.
const FREE_SECT: u32 = 0xFFFF_FFFF;
/// 한없이 도는 체인을 막는 상한(정상 문서는 이 근처도 못 간다).
const MAX_CHAIN: usize = 1 << 22;

/// 열어 둔 CFB 파일 하나. 스트림 이름 → (시작섹터, 크기) 색인을 미리 만들어 둔다.
pub struct Ole {
    data: Vec<u8>,
    sector_size: usize,
    mini_sector_size: usize,
    mini_cutoff: u32,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    /// 미니 스트림 실체(루트 엔트리의 스트림)를 통째로 펼쳐 둔 것.
    mini_stream: Vec<u8>,
    /// 이름(대소문자 그대로) → (시작 섹터, 바이트 크기)
    entries: HashMap<String, (u32, u64)>,
}

/// 바이트에서 리틀엔디언 u16/u32/u64 읽기(범위를 벗어나면 None).
fn u16le(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn u32le(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn u64le(b: &[u8], off: usize) -> Option<u64> {
    b.get(off..off + 8).map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

impl Ole {
    /// 파일 앞 8바이트가 CFB 서명인지.
    pub fn is_ole(data: &[u8]) -> bool {
        data.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
    }

    /// 바이트 전체를 받아 CFB 로 연다(구조가 어긋나면 None).
    ///
    /// 손상·비표준 파일에서 패닉이 나지 않도록 모든 접근을 경계검사한다 —
    /// 수집 폴더에는 어떤 파일이 들어올지 알 수 없고, 파일 하나 때문에 배치가
    /// 죽으면 안 된다.
    pub fn open(data: Vec<u8>) -> Option<Ole> {
        if !Self::is_ole(&data) {
            return None;
        }
        let sector_shift = u16le(&data, 0x1E)?;
        let mini_shift = u16le(&data, 0x20)?;
        // 실제로 쓰이는 값은 9(512)와 12(4096) 뿐이다. 그 밖은 다루지 않는다.
        if !(7..=20).contains(&sector_shift) || !(4..=12).contains(&mini_shift) {
            return None;
        }
        let sector_size = 1usize << sector_shift;
        let mini_sector_size = 1usize << mini_shift;
        let n_fat = u32le(&data, 0x2C)? as usize;
        let dir_start = u32le(&data, 0x30)?;
        let mini_cutoff = u32le(&data, 0x38)?;
        let mini_fat_start = u32le(&data, 0x3C)?;
        let n_mini_fat = u32le(&data, 0x40)? as usize;
        let difat_start = u32le(&data, 0x44)?;
        let n_difat = u32le(&data, 0x48)? as usize;

        let mut ole = Ole {
            data, sector_size, mini_sector_size, mini_cutoff,
            fat: vec![], mini_fat: vec![], mini_stream: vec![], entries: HashMap::new(),
        };

        // ── DIFAT: "FAT 이 들어 있는 섹터 번호" 목록. 앞 109개는 헤더에 직접 있고,
        //    더 필요하면 별도 섹터를 사슬처럼 이어 붙인다.
        let mut fat_sectors: Vec<u32> = vec![];
        for i in 0..109 {
            match u32le(&ole.data, 0x4C + i * 4) {
                Some(s) if s != FREE_SECT && s < END_OF_CHAIN => fat_sectors.push(s),
                _ => {}
            }
        }
        let mut next = difat_start;
        let mut guard = 0usize;
        while next != END_OF_CHAIN && next != FREE_SECT && guard < n_difat.max(1) + 64 {
            let sec = ole.sector_bytes(next)?;
            // 마지막 4바이트는 '다음 DIFAT 섹터' 번호이고 나머지가 FAT 섹터 번호다.
            let cnt = sector_size / 4;
            for i in 0..cnt.saturating_sub(1) {
                if let Some(s) = u32le(sec, i * 4) {
                    if s != FREE_SECT && s < END_OF_CHAIN {
                        fat_sectors.push(s);
                    }
                }
            }
            next = u32le(sec, (cnt - 1) * 4)?;
            guard += 1;
        }
        if fat_sectors.len() > n_fat.max(1) * 4 + 128 {
            fat_sectors.truncate(n_fat.max(1) * 4 + 128);   // 이상값 방어
        }

        // ── FAT 본체를 이어 붙인다.
        //    sector_bytes 가 ole 를 빌리므로, 먼저 값으로 모은 뒤 한 번에 옮긴다
        //    (같은 구조체를 읽으면서 동시에 그 필드에 push 할 수는 없다).
        let mut fat: Vec<u32> = vec![];
        for s in &fat_sectors {
            let sec = match ole.sector_bytes(*s) { Some(b) => b, None => continue };
            for i in 0..sector_size / 4 {
                if let Some(v) = u32le(sec, i * 4) {
                    fat.push(v);
                }
            }
        }
        ole.fat = fat;
        if ole.fat.is_empty() {
            return None;
        }

        // ── 미니 FAT(작은 스트림 전용 연결 리스트).
        let mini_fat_bytes = ole.read_chain(mini_fat_start, None, n_mini_fat * sector_size + sector_size);
        for i in 0..mini_fat_bytes.len() / 4 {
            if let Some(v) = u32le(&mini_fat_bytes, i * 4) {
                ole.mini_fat.push(v);
            }
        }

        // ── 디렉터리: 128바이트 엔트리의 나열. 루트(type=5)의 스트림이 곧
        //    '미니 스트림 실체'이므로 먼저 찾아 펼쳐 둔다.
        let dir = ole.read_chain(dir_start, None, usize::MAX);
        let mut root: Option<(u32, u64)> = None;
        let mut pending: Vec<(String, u32, u64)> = vec![];
        for off in (0..dir.len()).step_by(128) {
            if off + 128 > dir.len() { break; }
            let e = &dir[off..off + 128];
            let obj_type = e[0x42];
            if obj_type == 0 { continue; }                 // 빈 엔트리
            let name_len = u16le(e, 0x40).unwrap_or(0) as usize;
            if name_len < 2 || name_len > 64 { continue; }
            // 이름은 UTF-16LE, 끝의 널문자(2바이트)를 뺀다.
            let mut name = String::new();
            for i in (0..name_len - 2).step_by(2) {
                if let Some(c) = u16le(e, i) {
                    if let Some(ch) = char::from_u32(c as u32) { name.push(ch); }
                }
            }
            let start = u32le(e, 0x74).unwrap_or(END_OF_CHAIN);
            let size = u64le(e, 0x78).unwrap_or(0);
            if obj_type == 5 {
                root = Some((start, size));
            } else if obj_type == 2 {
                pending.push((name, start, size));
            }
        }
        if let Some((rs, rsize)) = root {
            ole.mini_stream = ole.read_chain(rs, None, rsize as usize);
        }
        for (name, start, size) in pending {
            ole.entries.insert(name, (start, size));
        }
        Some(ole)
    }

    /// 일반 섹터 하나의 바이트 조각(범위를 벗어나면 None).
    fn sector_bytes(&self, sector: u32) -> Option<&[u8]> {
        let off = (sector as usize).checked_add(1)?.checked_mul(self.sector_size)?;
        self.data.get(off..off + self.sector_size)
    }

    /// 섹터 체인을 따라가며 바이트를 모은다.
    ///
    /// -in: start    = 시작 섹터
    /// -in: mini     = Some(()) 면 미니 섹터 체인(미니 FAT + 미니 스트림)
    /// -in: max_len  = 이만큼 모으면 멈춘다(스트림 크기로 자르기)
    fn read_chain(&self, start: u32, mini: Option<()>, max_len: usize) -> Vec<u8> {
        let (fat, unit) = match mini {
            Some(_) => (&self.mini_fat, self.mini_sector_size),
            None => (&self.fat, self.sector_size),
        };
        let mut out: Vec<u8> = vec![];
        let mut cur = start;
        let mut steps = 0usize;
        while cur != END_OF_CHAIN && cur != FREE_SECT && steps < MAX_CHAIN {
            let chunk: Option<&[u8]> = match mini {
                Some(_) => {
                    let off = (cur as usize).checked_mul(unit);
                    off.and_then(|o| self.mini_stream.get(o..o + unit))
                }
                None => self.sector_bytes(cur),
            };
            match chunk {
                Some(b) => out.extend_from_slice(b),
                None => break,
            }
            if out.len() >= max_len { break; }
            cur = match fat.get(cur as usize) { Some(v) => *v, None => break };
            steps += 1;
        }
        if max_len != usize::MAX && out.len() > max_len {
            out.truncate(max_len);
        }
        out
    }

    /// 이름으로 스트림 전체를 읽는다(없으면 None).
    /// 크기가 미니 컷오프보다 작으면 미니 섹터에서 읽는다.
    pub fn stream(&self, name: &str) -> Option<Vec<u8>> {
        let (start, size) = *self.entries.get(name)?;
        let mini = if size < self.mini_cutoff as u64 { Some(()) } else { None };
        Some(self.read_chain(start, mini, size as usize))
    }

    /// 이 스트림이 있는가.
    pub fn has(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// 들어 있는 스트림 이름들(포맷 세부 판별에 쓴다).
    pub fn names(&self) -> Vec<&str> {
        self.entries.keys().map(|s| s.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // CFB 가 아닌 바이트를 열면 조용히 None 이어야 한다(패닉 금지).
    #[test]
    fn ole가_아니면_none() {
        assert!(Ole::open(b"not an ole file at all".to_vec()).is_none());
        assert!(Ole::open(vec![]).is_none());
    }

    // 서명만 맞고 뒤가 잘린 파일에도 패닉하지 않아야 한다 — 손상 파일 하나로
    // 배치 전체가 죽으면 안 된다.
    #[test]
    fn 서명만_있고_잘린_파일도_안전하다() {
        let mut v = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        v.extend(std::iter::repeat(0u8).take(20));
        assert!(Ole::open(v).is_none());
    }

    // 섹터 크기가 말이 안 되면(0 또는 과대) 열지 않는다.
    #[test]
    fn 이상한_섹터크기는_거부한다() {
        let mut v = vec![0u8; 512];
        v[..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
        v[0x1E] = 99;   // sector_shift = 99 → 말이 안 됨
        assert!(Ole::open(v).is_none());
    }
}

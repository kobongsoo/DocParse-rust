//! HWP 5.0 텍스트 추출 (Python extract/hwp5.py 포팅)
//!
//! HWP5 는 구형 .doc 와 같은 OLE/CFB 컨테이너 안에 BodyText/SectionN 스트림을 두고,
//! 그 안에 태그 레코드로 문단을 담는다. 본문 글자는 PARA_TEXT 레코드에
//! UTF-16LE + 인라인 제어문자로 들어 있다.
//!   · 컨테이너 읽기 : ole.rs
//!   · 압축         : FileHeader 속성비트가 켜져 있으면 각 Section 이 raw-deflate
//!   · 암호화 문서   : 해독 불가 → 추출 실패로 알린다
//!
//! (구형 HWP 3.x 는 OLE 가 아니라 이 경로로 들어오지 않는다.)

use std::io::Read;

use crate::ole::Ole;

/// PARA_TEXT 레코드 태그 = HWPTAG_BEGIN(0x10) + 51 = 67. 본문 글자가 여기 담긴다.
const HWPTAG_PARA_TEXT: u32 = 67;

//------------------------------------------------------------------
// PARA_TEXT 페이로드 → 순수 텍스트
//=> UTF-16LE 로 저장된 문단 글자열을 훑어, 실제 글자만 남기고 제어문자를 처리한다.
//   HWP5 는 문단 안에 표·그림 같은 '컨트롤'을 제어문자로 끼워 넣는데, 그 컨트롤은
//   8글자(16바이트)를 차지한다 — 그만큼 건너뛰지 않으면 뒷글자가 전부 밀려 깨진다.
//    1) 2바이트씩 읽어 wchar 값을 만든다
//    2) 0/10/13 은 1글자 컨트롤(10·13 → 개행, 0 은 버림)
//    3) 그 외 1~31 은 8글자 컨트롤 → 16바이트 통째로 건너뛴다
//    4) 32 이상은 실제 글자
//------------------------------------------------------------------
fn decode_para_text(payload: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    // 서로게이트 쌍을 만나면 두 wchar 를 합쳐야 하므로 u16 버퍼에 모아 뒀다 변환한다.
    let mut units: Vec<u16> = vec![];
    while i + 2 <= payload.len() {
        let wc = u16::from_le_bytes([payload[i], payload[i + 1]]);
        match wc {
            0 => i += 2,
            10 | 13 => {
                out.push_str(&String::from_utf16_lossy(&units));
                units.clear();
                out.push('\n');
                i += 2;
            }
            1..=31 => i += 16,          // 인라인/확장 컨트롤: 8 wchar 차지
            _ => { units.push(wc); i += 2; }
        }
    }
    out.push_str(&String::from_utf16_lossy(&units));
    out
}

//------------------------------------------------------------------
// Section 스트림 → 태그 레코드 순회
//=> HWP5 레코드는 [32비트 헤더][페이로드] 나열이다. 헤더 하위 10비트가 태그ID,
//   상위 12비트가 크기이며, 크기가 0xFFF 면 '초과' 표식이라 다음 4바이트가 진짜 크기다.
//   찾는 태그의 페이로드만 모아 돌려준다.
//------------------------------------------------------------------
fn collect_para_texts(data: &[u8], out: &mut Vec<String>) {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let header = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        i += 4;
        let tag_id = header & 0x3FF;
        let mut size = ((header >> 20) & 0xFFF) as usize;
        if size == 0xFFF {
            if i + 4 > data.len() { break; }
            size = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
            i += 4;
        }
        let end = match i.checked_add(size) { Some(e) => e.min(data.len()), None => break };
        if tag_id == HWPTAG_PARA_TEXT {
            out.push(decode_para_text(&data[i..end]));
        }
        // 크기 0인 레코드에서 제자리를 맴돌지 않도록 반드시 전진시킨다.
        if size == 0 && end == i { /* 헤더만큼은 이미 전진함 */ }
        i = end;
    }
}

/// zlib 헤더 없는 raw-deflate 풀기(HWP5 규약: wbits=-15).
fn inflate_raw(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut dec = flate2::read::DeflateDecoder::new(data);
    match dec.read_to_end(&mut out) {
        // 일부만 풀려도 그만큼은 쓸모가 있다(부분 추출) — 완전 실패일 때만 포기.
        Ok(_) => Some(out),
        Err(_) if !out.is_empty() => Some(out),
        Err(_) => None,
    }
}

//------------------------------------------------------------------
// .hwp(HWP5) → 본문 텍스트 (핵심)
//    1) FileHeader(offset 36)의 속성 DWORD 로 압축·암호화 여부를 본다
//    2) 암호화 문서는 해독할 수 없으므로 실패로 알린다
//    3) BodyText/SectionN 을 번호순으로 모아 레코드를 훑는다
//
// -in: data = .hwp 파일 전체 바이트
//
// -out: Some(text) = 본문 / None = HWP5 가 아니거나 암호화·손상
//------------------------------------------------------------------
pub fn hwp5_text(data: Vec<u8>) -> Option<String> {
    let ole = Ole::open(data)?;
    let header = ole.stream("FileHeader")?;
    if header.len() < 40 {
        return None;
    }
    let flags = u32::from_le_bytes([header[36], header[37], header[38], header[39]]);
    let compressed = flags & 0x01 != 0;
    let encrypted = flags & 0x02 != 0;
    if encrypted {
        // 암호가 걸린 문서는 열 수 없다 — 조용히 빈 텍스트를 내면 '내용 없는 문서'로
        // 오해되므로 실패로 알려 상위가 '추출 실패'로 기록하게 한다.
        return None;
    }

    // BodyText/SectionN 을 번호순으로. ole.rs 는 평면 이름 목록을 주므로
    // "SectionN" 으로 시작하는 스트림을 숫자 순으로 정렬해 쓴다.
    let mut secs: Vec<(u32, String)> = ole.names().iter()
        .filter(|n| n.starts_with("Section"))
        .map(|n| {
            let num: u32 = n.trim_start_matches("Section").parse().unwrap_or(0);
            (num, n.to_string())
        })
        .collect();
    secs.sort();

    let mut parts: Vec<String> = vec![];
    for (_, name) in &secs {
        let raw = match ole.stream(name) { Some(d) => d, None => continue };
        let data = if compressed {
            // 한 섹션이 깨져도 나머지는 살린다(부분 추출).
            match inflate_raw(&raw) { Some(d) => d, None => continue }
        } else {
            raw
        };
        collect_para_texts(&data, &mut parts);
    }
    if secs.is_empty() {
        return None;      // BodyText 가 없으면 HWP5 로 볼 수 없다
    }
    Some(parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 제어문자 처리 — 이게 틀리면 뒷글자가 전부 밀려 본문이 깨진다.
    #[test]
    fn para_text의_제어문자를_건너뛴다() {
        let mut p: Vec<u8> = vec![];
        for ch in "계약".encode_utf16() { p.extend_from_slice(&ch.to_le_bytes()); }
        // 8글자(16바이트)를 차지하는 인라인 컨트롤 하나
        p.extend_from_slice(&3u16.to_le_bytes());
        p.extend(std::iter::repeat(0u8).take(14));
        for ch in "서".encode_utf16() { p.extend_from_slice(&ch.to_le_bytes()); }
        assert_eq!(decode_para_text(&p), "계약서");
    }

    // 10·13 은 개행으로 살리고 0 은 버린다.
    #[test]
    fn 줄바꿈_문단끝은_개행이_된다() {
        let mut p: Vec<u8> = vec![];
        for ch in "가".encode_utf16() { p.extend_from_slice(&ch.to_le_bytes()); }
        p.extend_from_slice(&13u16.to_le_bytes());
        for ch in "나".encode_utf16() { p.extend_from_slice(&ch.to_le_bytes()); }
        p.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(decode_para_text(&p), "가\n나");
    }

    // 레코드 헤더: 하위 10비트=태그, 상위 12비트=크기.
    #[test]
    fn para_text_레코드만_모은다() {
        let mut d: Vec<u8> = vec![];
        let mut body: Vec<u8> = vec![];
        for ch in "본문".encode_utf16() { body.extend_from_slice(&ch.to_le_bytes()); }
        // 태그 67, 레벨 0, 크기 = body.len()
        let hdr: u32 = HWPTAG_PARA_TEXT | ((body.len() as u32) << 20);
        d.extend_from_slice(&hdr.to_le_bytes());
        d.extend_from_slice(&body);
        // 다른 태그(무시돼야 함)
        let hdr2: u32 = 66 | (2u32 << 20);
        d.extend_from_slice(&hdr2.to_le_bytes());
        d.extend_from_slice(&[0xFF, 0xFF]);
        let mut out = vec![];
        collect_para_texts(&d, &mut out);
        assert_eq!(out, vec!["본문".to_string()]);
    }

    // OLE 가 아니면 조용히 None(패닉 금지).
    #[test]
    fn ole가_아니면_none() {
        assert!(hwp5_text(b"not ole".to_vec()).is_none());
    }
}

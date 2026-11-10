//! 구형 Office(97-2003 바이너리) 추출기 — doc/ppt (Python extract/office_legacy.py 포팅)
//!
//! 2007 이전 .doc/.ppt 는 OLE/CFB 안에 독자 포맷으로 들어 있어 현대 OOXML
//! 파서로는 못 읽는다. ole.rs 로 컨테이너를 열고 여기서 본문을 직접 파싱한다
//! (외부 exe·상용 컴포넌트 없이).
//!   · doc → WordDocument 스트림의 piece table 을 따라 텍스트 조각을 잇는다.
//!   · ppt → PowerPoint Document 스트림의 텍스트 원자(TextChars/TextBytes)를 모은다.
//!
//! (.xls 는 BIFF 셀 구조라 성격이 달라 여기 넣지 않았다 — 파이썬 판도 xlrd 라는
//!  별도 라이브러리에 맡긴다.)

use crate::ole::Ole;

/// 1바이트 문자 텍스트를 CP949(=EUC-KR 확장)로 읽는다.
/// 파이썬 판의 `decode("cp949", "replace")` 와 같은 자리 — 한글 구형 문서가
/// 대부분 이 인코딩이라, 여기서 틀리면 본문이 통째로 깨진다.
fn cp949(bytes: &[u8]) -> String {
    let (s, _, _) = encoding_rs::EUC_KR.decode(bytes);
    s.into_owned()
}

/// UTF-16LE 바이트를 문자열로(짝이 안 맞는 끝 바이트는 버린다).
fn utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn u16le(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn u32le(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

//------------------------------------------------------------------
// CLX 에서 piece table(Pcdt) 찾기
//=> Word 의 CLX 는 Prc(0x01)와 Pcdt(0x02) 조각의 나열이다. Pcdt 안에 piece
//   목록(PlcPcd)이 들어 있으므로 그것만 잘라 준다.
//------------------------------------------------------------------
fn find_pcdt(clx: &[u8]) -> &[u8] {
    let mut i = 0usize;
    while i < clx.len() {
        match clx[i] {
            0x01 => {
                // Prc: 다음 2바이트가 길이. 그만큼 건너뛴다.
                let n = match u16le(clx, i + 1) { Some(v) => v as usize, None => break };
                i += 3 + n;
            }
            0x02 => {
                // Pcdt: 다음 4바이트가 길이, 그 뒤가 PlcPcd 본체.
                let n = match u32le(clx, i + 1) { Some(v) => v as usize, None => break };
                let s = i + 5;
                return clx.get(s..s + n).unwrap_or(&[]);
            }
            _ => break,
        }
    }
    &[]
}

//------------------------------------------------------------------
// .doc → 본문 텍스트 (핵심)
//=> WordDocument 스트림과 Table 스트림을 읽어, piece table 이 가리키는 조각들을
//   순서대로 이어 붙인다.
//    1) FIB 플래그(0x0A)의 비트로 0Table/1Table 중 어느 쪽인지 고른다
//    2) FIB 의 fcClx(0x01A2)/lcbClx(0x01A6) 로 Table 안의 CLX 위치를 얻는다
//    3) CLX 에서 piece table 을 찾아 조각마다 압축(1바이트=CP949)/비압축
//       (UTF-16LE)을 구분해 디코딩한다
//    4) Word 특수문자(셀 구분·문단기호·필드기호)를 정리한다
//
// -in: data = .doc 파일 전체 바이트
//
// -out: Some(text) = 본문 / None = OLE 가 아니거나 구조가 어긋남(상위가 추출실패 처리)
//------------------------------------------------------------------
pub fn doc_text(data: Vec<u8>) -> Option<String> {
    let ole = Ole::open(data)?;
    let wd = ole.stream("WordDocument")?;

    // fWhichTblStm 비트(0x0200)가 서면 1Table, 아니면 0Table.
    let flags = u16le(&wd, 0x0A)?;
    let mut tbl_name = if flags & 0x0200 != 0 { "1Table" } else { "0Table" };
    if !ole.has(tbl_name) {
        // 플래그와 실제가 어긋난 파일이 있다 — 있는 쪽으로 맞춘다.
        tbl_name = if ole.has("1Table") { "1Table" } else { "0Table" };
    }
    let tbl = ole.stream(tbl_name)?;

    let fc_clx = u32le(&wd, 0x01A2)? as usize;
    let lcb_clx = u32le(&wd, 0x01A6)? as usize;
    let clx = tbl.get(fc_clx..fc_clx.checked_add(lcb_clx)?)?;

    let pcd = find_pcdt(clx);
    if pcd.len() < 4 {
        return None;
    }
    // PlcPcd = CP 배열((nP+1)*4) + Pcd 배열(nP*8).
    let n_pieces = (pcd.len() - 4) / (4 + 8);
    if n_pieces == 0 {
        return None;
    }
    let mut cps: Vec<u32> = Vec::with_capacity(n_pieces + 1);
    for k in 0..=n_pieces {
        cps.push(u32le(pcd, k * 4)?);
    }
    let base = (n_pieces + 1) * 4;

    let mut out = String::new();
    for k in 0..n_pieces {
        // Pcd 8바이트 중 offset 2 부터 4바이트가 fc(위치 + 압축 플래그).
        let fc = match u32le(pcd, base + k * 8 + 2) { Some(v) => v, None => break };
        let n_ch = cps[k + 1].saturating_sub(cps[k]) as usize;
        if n_ch == 0 { continue; }
        if fc & 0x4000_0000 != 0 {
            // 압축: 1글자 1바이트, 실제 위치는 상위 비트를 떼고 2로 나눈 값.
            let off = ((fc & 0x3FFF_FFFF) / 2) as usize;
            if let Some(b) = wd.get(off..(off + n_ch).min(wd.len())) {
                out.push_str(&cp949(b));
            }
        } else {
            // 비압축: UTF-16LE, 1글자 2바이트.
            let off = fc as usize;
            let end = (off + n_ch * 2).min(wd.len());
            if let Some(b) = wd.get(off..end) {
                out.push_str(&utf16le(b));
            }
        }
    }
    Some(clean_word_chars(&out))
}

/// Word 특수문자 정리 — 파이썬 판과 같은 치환표.
/// 표 셀 구분(0x07)은 탭, 문단·줄바꿈은 개행, 필드 기호는 지운다.
fn clean_word_chars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\u{07}' => out.push('\t'),
            '\u{0b}' | '\r' => out.push('\n'),
            // 그림 앵커·필드 시작/구분/끝 등은 본문이 아니므로 버린다.
            '\u{08}' | '\u{01}' | '\u{02}' | '\u{13}' | '\u{14}' | '\u{15}' => {}
            _ => out.push(ch),
        }
    }
    out
}

// ── .ppt : PowerPoint Document 텍스트 원자 ──────────────────
const PPT_TEXTCHARS: u16 = 0x0FA0;   // UTF-16LE
const PPT_TEXTBYTES: u16 = 0x0FA8;   // CP949(1바이트)

//------------------------------------------------------------------
// PowerPoint 레코드 트리 순회 → 텍스트 원자 수집
//=> ppt 스트림은 [8바이트 헤더][데이터] 레코드의 나열이고, 컨테이너(recVer=0xF)는
//   자식 레코드를 품는다. 재귀로 훑어 TextChars/TextBytes 만 모은다.
//   depth 를 두어 비정상 파일에서 무한히 깊어지는 것을 막는다.
//------------------------------------------------------------------
fn ppt_walk(data: &[u8], out: &mut Vec<String>, depth: u32) {
    if depth > 32 { return; }
    let mut i = 0usize;
    while i + 8 <= data.len() {
        let ver_inst = match u16le(data, i) { Some(v) => v, None => break };
        let rec_type = match u16le(data, i + 2) { Some(v) => v, None => break };
        let rec_len = match u32le(data, i + 4) { Some(v) => v as usize, None => break };
        i += 8;
        let end = match i.checked_add(rec_len) { Some(e) => e.min(data.len()), None => break };
        let body = &data[i..end];
        if ver_inst & 0x0F == 0x0F {
            ppt_walk(body, out, depth + 1);
        } else if rec_type == PPT_TEXTCHARS {
            out.push(utf16le(body));
        } else if rec_type == PPT_TEXTBYTES {
            out.push(cp949(body));
        }
        // 길이가 0이면 제자리를 맴돌게 되므로 반드시 전진시킨다.
        if rec_len == 0 { i += 1; } else { i = end; }
    }
}

//------------------------------------------------------------------
// .ppt → 본문 텍스트
//
// -in: data = .ppt 파일 전체 바이트
//
// -out: Some(text) = 슬라이드 본문 / None = OLE 가 아니거나 스트림 없음
//------------------------------------------------------------------
pub fn ppt_text(data: Vec<u8>) -> Option<String> {
    let ole = Ole::open(data)?;
    let stream = ole.stream("PowerPoint Document")?;
    let mut parts: Vec<String> = vec![];
    ppt_walk(&stream, &mut parts, 0);
    let joined = parts.iter()
        .filter(|t| !t.trim().is_empty())
        .cloned().collect::<Vec<_>>().join("\n");
    Some(joined.replace('\u{0b}', "\n").replace('\r', "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // CLX 파싱: Prc 를 건너뛰고 Pcdt 본체만 정확히 잘라내야 한다.
    #[test]
    fn clx에서_piece_table만_잘라낸다() {
        // [0x01][len=2][2바이트] [0x02][len=6][6바이트]
        let mut clx = vec![0x01, 0x02, 0x00, 0xAA, 0xBB];
        clx.push(0x02);
        clx.extend_from_slice(&6u32.to_le_bytes());
        clx.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(find_pcdt(&clx), &[1, 2, 3, 4, 5, 6]);
    }

    // Pcdt 가 없으면 빈 조각(→ 상위가 추출실패로 처리).
    #[test]
    fn pcdt가_없으면_빈다() {
        assert!(find_pcdt(&[0x01, 0x00, 0x00]).is_empty());
        assert!(find_pcdt(&[]).is_empty());
    }

    // Word 특수문자 정리 — 셀 구분은 탭, 문단은 개행, 필드기호는 제거.
    #[test]
    fn word_특수문자를_정리한다() {
        assert_eq!(clean_word_chars("가\u{07}나\u{0b}다\r라\u{13}\u{14}마"),
                   "가\t나\n다\n라마");
    }

    // 한글 CP949 디코딩이 맞아야 본문이 안 깨진다.
    #[test]
    fn cp949_한글을_읽는다() {
        // "계약서" 의 CP949 바이트
        assert_eq!(cp949(&[0xB0, 0xE8, 0xBE, 0xE0, 0xBC, 0xAD]), "계약서");
    }

    #[test]
    fn utf16le_한글을_읽는다() {
        let b: Vec<u8> = "계약서".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(utf16le(&b), "계약서");
    }

    // OLE 가 아닌 입력에 패닉하지 않는다.
    #[test]
    fn ole가_아니면_none을_준다() {
        assert!(doc_text(b"plain text".to_vec()).is_none());
        assert!(ppt_text(b"plain text".to_vec()).is_none());
    }

    // 길이 0 레코드가 있어도 무한루프에 빠지지 않는다(손상 파일 방어).
    #[test]
    fn ppt_길이0_레코드에_멈추지_않는다() {
        let data = vec![0u8; 64];       // 전부 0 → rec_len=0 인 레코드가 이어짐
        let mut out = vec![];
        ppt_walk(&data, &mut out, 0);   // 여기서 멈추지 않으면 테스트가 끝나지 않는다
        assert!(out.is_empty());
    }
}

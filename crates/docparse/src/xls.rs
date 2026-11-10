//! 구형 Excel(.xls, BIFF8) 텍스트 추출.
//!
//! 파이썬 판은 xlrd 라는 성숙한 라이브러리에 맡기지만, Rust 쪽에는 그런 의존을
//! 새로 들이지 않고 **분류에 필요한 만큼만** 직접 읽는다. 목적이 '셀 값을 정확히
//! 재현'하는 것이 아니라 '문서에 어떤 말이 적혀 있는지'를 보는 것이기 때문이다.
//!
//! 읽는 레코드:
//!   · SST(0x00FC)      — BIFF8 의 공유 문자열 표. 시트의 글자는 대부분 여기 모인다
//!   · LABELSST(0x00FD) — SST 를 가리키는 셀
//!   · LABEL(0x0204)    — 셀 안에 직접 든 문자열(구형)
//!   · RSTRING(0x00D6)  — 서식 붙은 문자열 셀
//!   · NUMBER(0x0203) · RK(0x027E) · MULRK(0x00BD) — 숫자 셀
//!   · FORMULA(0x0006)  — 수식 셀. 식이 아니라 **계산해 둔 결과값**을 읽는다
//!   · STRING(0x0207)   — 수식 결과 문자열
//!
//! 셀은 (행, 열)을 갖고 있으므로 행 단위로 탭으로 이어 붙여, 파이썬 판(xlrd)의
//! "행마다 탭 구분" 출력과 같은 모양이 되게 한다.

use std::collections::BTreeMap;

use crate::ole::Ole;

const R_SST: u16 = 0x00FC;
const R_CONTINUE: u16 = 0x003C;
const R_LABELSST: u16 = 0x00FD;
const R_LABEL: u16 = 0x0204;
const R_RSTRING: u16 = 0x00D6;
const R_NUMBER: u16 = 0x0203;
const R_RK: u16 = 0x027E;
const R_MULRK: u16 = 0x00BD;
const R_STRING: u16 = 0x0207;
const R_FORMULA: u16 = 0x0006;
// 칸 서식표(XF)와 사용자 지정 서식 문자열(FORMAT) — 날짜 셀을 가리기 위해 읽는다.
const R_XF: u16 = 0x00E0;
const R_FORMAT: u16 = 0x041E;
const R_BOF: u16 = 0x0809;
const R_EOF: u16 = 0x000A;

fn u16le(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn u32le(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// 숫자를 사람이 보던 대로 — 정수면 소수점 없이, 아니면 불필요한 0을 떼고.
fn num_str(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{}", v);
        s
    }
}

//------------------------------------------------------------------
// 칸 서식 번호(ixfe) → 날짜 종류
//=> BIFF 의 셀 레코드는 '몇 번째 칸 서식인가'(ixfe)만 들고 있다. 그 서식이
//   가리키는 서식 번호(ifmt)를 따라가야 이 칸이 날짜인지 알 수 있다.
//    1) XF 목록에서 ixfe 번째를 찾아 ifmt 를 얻는다
//    2) 사용자가 만든 서식이면 서식 문자열로, 아니면 내장 번호표로 판단한다
//
// -in: ixfe    = 셀이 가리키는 칸 서식 번호
// -in: xfs     = XF 레코드에서 모은 ifmt 목록(나온 순서 그대로)
// -in: formats = 사용자 지정 서식(번호 → 서식 문자열)
//
// -out: 날짜 종류(모르면 None — 숫자를 그대로 둔다)
// -out: error = 없음
//------------------------------------------------------------------
fn xf_date_kind(ixfe: u16, xfs: &[u16],
                formats: &std::collections::HashMap<u16, String>)
                -> crate::xlsdate::DateKind {
    use crate::xlsdate::{builtin_kind, code_kind, DateKind};
    let ifmt = match xfs.get(ixfe as usize) { Some(v) => *v, None => return DateKind::None };
    // 사용자 지정이 있으면 그것이 이긴다 — 내장 번호를 덮어쓴 파일이 있다.
    match formats.get(&ifmt) {
        Some(code) => code_kind(code),
        None => builtin_kind(ifmt),
    }
}

//------------------------------------------------------------------
// 숫자 칸을 사람이 보던 글자로 (날짜면 날짜로)
//=> 서식이 날짜일 때만 바꾼다. 서식을 안 보고 바꾸면 멀쩡한 수량·금액이 날짜가 된다.
//
// -in: v       = 셀의 숫자 값
// -in: ixfe    = 셀이 가리키는 칸 서식 번호
// -in: xfs     = XF 목록
// -in: formats = 사용자 지정 서식표
//
// -out: 날짜면 날짜 글자, 아니면 예전처럼 숫자 글자
// -out: error = 없음
//------------------------------------------------------------------
fn cell_str(v: f64, ixfe: Option<u16>, xfs: &[u16],
            formats: &std::collections::HashMap<u16, String>) -> String {
    if let Some(x) = ixfe {
        let k = xf_date_kind(x, xfs, formats);
        if let Some(t) = crate::xlsdate::serial_to_string(v, k) {
            return t;
        }
    }
    num_str(v)
}

/// RK 값(4바이트로 압축된 수) 풀기 — BIFF 규약.
fn rk_value(rk: u32) -> f64 {
    let is_int = rk & 0x02 != 0;
    let div100 = rk & 0x01 != 0;
    let mut v = if is_int {
        // 상위 30비트가 부호 있는 정수.
        ((rk as i32) >> 2) as f64
    } else {
        // 상위 30비트가 double 의 상위 비트, 나머지는 0.
        f64::from_bits(((rk & 0xFFFF_FFFC) as u64) << 32)
    };
    if div100 { v /= 100.0; }
    v
}

/// 수식 셀이 담고 있던 '계산된 결과'의 갈래.
///
/// FORMULA 레코드는 식만 갖고 있는 게 아니라 **마지막으로 계산된 값**을 함께 싣는다.
/// 그 값이 무엇이냐에 따라 뒤 처리가 달라지므로 갈래를 나눠 돌려준다.
enum FormulaVal {
    /// 숫자 결과 — 그대로 본문에 쓴다.
    Num(f64),
    /// 문자열 결과 — 글자는 **바로 뒤 STRING 레코드**에 따로 온다.
    /// 그래서 여기서는 "다음 STRING 은 이 칸 것"이라는 표시만 한다.
    Str,
    /// 논리값·오류(#N/A 등)·빈 문자열 — 본문이 아니므로 버린다.
    Skip,
}

//------------------------------------------------------------------
// FORMULA 레코드에서 '계산된 결과값' 읽기
//=> 예전에는 이 레코드를 통째로 건너뛰어, **수식으로 만든 값이 전부 사라졌다**
//   (실측: 표의 '평균 계산을 위한 초' 열 숫자 전부 유실 → 재현율 49%).
//   합계·환산처럼 사람이 표에서 읽는 값 상당수가 수식 셀이고, 하이픈 없이 적은
//   전화·계좌번호가 수식 결과로 들어 있으면 PII 검출기 눈에 아예 안 보인다.
//
//   [값이 어디 있나] 레코드 앞은 행(2)·열(2)·서식(2)이고, 그 다음 **8바이트**가
//   결과값 자리다. 이 8바이트는 두 가지로 쓰인다:
//    1) 마지막 두 바이트가 0xFFFF 면 → 숫자가 아니라 '특별한 값'이라는 표시다.
//       첫 바이트가 갈래를 말한다(0=문자열 · 1=논리값 · 2=오류 · 3=빈 문자열).
//    2) 아니면 → 8바이트 통째로 IEEE754 배정도 실수다.
//   진짜 숫자가 우연히 이 표시와 겹칠 일은 없다(그 비트꼴은 NaN 자리라서다).
//
// -in: b = FORMULA 레코드 본문(헤더 4바이트를 뺀 나머지)
//
// -out: FormulaVal — 숫자 / 문자열예고 / 버림
// -out: error = 레코드가 짧아 값을 못 읽으면 Skip
//------------------------------------------------------------------
fn formula_value(b: &[u8]) -> FormulaVal {
    let raw = match b.get(6..14) {
        Some(r) => r,
        None => return FormulaVal::Skip,
    };
    if raw[6] == 0xFF && raw[7] == 0xFF {
        return match raw[0] {
            0 => FormulaVal::Str,      // 글자는 뒤따르는 STRING 레코드에 있다
            _ => FormulaVal::Skip,     // 1=논리값 · 2=오류 · 3=빈 문자열
        };
    }
    FormulaVal::Num(f64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]))
}

/// BIFF8 문자열 하나 읽기(길이는 호출자가 준다).
///
/// 앞에 1바이트 플래그가 있고, bit0 이 서면 UTF-16LE(2바이트/글자), 아니면
/// 압축 라틴1(1바이트/글자)이다. rich/far-east 확장 필드는 건너뛴다.
/// 반환: (문자열, 소비한 바이트 수). 도중에 잘리면 None.
fn read_unicode_string(b: &[u8], mut pos: usize, n_chars: usize) -> Option<(String, usize)> {
    let start = pos;
    let flags = *b.get(pos)?;
    pos += 1;
    let wide = flags & 0x01 != 0;
    let rich = flags & 0x08 != 0;
    let far = flags & 0x04 != 0;
    let n_rich = if rich { let v = u16le(b, pos)? as usize; pos += 2; v } else { 0 };
    let n_far = if far { let v = u32le(b, pos)? as usize; pos += 4; v } else { 0 };

    let s = if wide {
        let need = n_chars.checked_mul(2)?;
        let raw = b.get(pos..pos + need)?;
        pos += need;
        let units: Vec<u16> = raw.chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        let raw = b.get(pos..pos + n_chars)?;
        pos += n_chars;
        // 압축 표기는 라틴1 — 각 바이트가 그대로 코드포인트.
        raw.iter().map(|&c| c as char).collect()
    };
    pos += n_rich * 4 + n_far;
    let _ = start;
    Some((s, pos))
}

/// SST 레코드(+ 뒤따르는 CONTINUE)를 이어 붙여 공유 문자열 표를 만든다.
///
/// [CONTINUE 경계가 핵심] 문자열 하나가 레코드 경계에서 잘릴 수 있는데, 이어지는
/// CONTINUE 조각은 **자기 플래그 바이트를 새로 갖는다**(그 뒤 글자가 1바이트인지
/// 2바이트인지 다시 알려 준다). 이걸 빼먹고 그냥 이어 붙이면 경계 뒤로 전부 한
/// 바이트씩 밀려, 표의 절반 이상이 깨지거나 유실된다(실측: 324개 중 170개만 복원).
/// 그래서 조각 경계 위치를 기억해 두고, **글자를 읽는 도중에** 경계를 만났을 때만
/// 플래그를 다시 읽는다(경계가 문자열 사이에 떨어지면 플래그는 없다).
fn parse_sst(chunks: &[&[u8]]) -> Vec<String> {
    let mut flat: Vec<u8> = vec![];
    let mut bounds: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (i, c) in chunks.iter().enumerate() {
        if i > 0 { bounds.insert(flat.len()); }   // 두 번째 조각부터가 CONTINUE 경계
        flat.extend_from_slice(c);
    }
    let mut out: Vec<String> = vec![];
    if flat.len() < 8 { return out; }
    let total = u32le(&flat, 4).unwrap_or(0) as usize;
    let mut pos = 8usize;

    while out.len() < total && pos + 3 <= flat.len() {
        let n_chars = match u16le(&flat, pos) { Some(v) => v as usize, None => break };
        pos += 2;
        let flags = match flat.get(pos) { Some(f) => *f, None => break };
        pos += 1;
        let mut wide = flags & 0x01 != 0;
        let rich = flags & 0x08 != 0;
        let far = flags & 0x04 != 0;
        let n_rich = if rich {
            match u16le(&flat, pos) { Some(v) => { pos += 2; v as usize }, None => break }
        } else { 0 };
        let n_far = if far {
            match u32le(&flat, pos) { Some(v) => { pos += 4; v as usize }, None => break }
        } else { 0 };

        // 글자를 하나씩 읽되, 조각 경계를 만나면 그 자리의 플래그를 다시 읽는다.
        let mut units: Vec<u16> = Vec::with_capacity(n_chars);
        let mut broken = false;
        for _ in 0..n_chars {
            if bounds.contains(&pos) {
                match flat.get(pos) {
                    Some(f) => { wide = f & 0x01 != 0; pos += 1; }
                    None => { broken = true; break; }
                }
            }
            if wide {
                match u16le(&flat, pos) { Some(u) => { units.push(u); pos += 2; },
                                          None => { broken = true; break; } }
            } else {
                match flat.get(pos) { Some(&b) => { units.push(b as u16); pos += 1; },
                                      None => { broken = true; break; } }
            }
        }
        out.push(String::from_utf16_lossy(&units));
        if broken { break; }
        // rich/far 확장 바이트는 본문이 아니므로 건너뛴다.
        pos = pos.saturating_add(n_rich * 4 + n_far);
        if pos > flat.len() { break; }
    }
    out
}

//------------------------------------------------------------------
// .xls(BIFF8) → 본문 텍스트 (핵심)
//=> Workbook 스트림의 레코드를 훑어 셀 값을 (행, 열)에 모으고, 행 단위로
//   탭으로 이어 붙인다. 시트가 여러 개면 시트 사이를 빈 줄로 나눈다.
//
// -in: data = .xls 파일 전체 바이트
//
// -out: Some(text) = 셀 텍스트 / None = OLE 가 아니거나 Workbook 스트림 없음
//------------------------------------------------------------------
pub fn xls_text(data: Vec<u8>) -> Option<String> {
    let ole = Ole::open(data)?;
    // BIFF8 은 "Workbook", BIFF5 이하는 "Book".
    let stream = ole.stream("Workbook").or_else(|| ole.stream("Book"))?;

    // ── 1차: SST 를 모은다(CONTINUE 포함). 셀 레코드가 이 표를 가리킨다.
    let mut sst_chunks: Vec<&[u8]> = vec![];
    // 칸 서식표(XF)는 '나온 순서'가 곧 번호다 — 순서를 흐트러뜨리면 안 된다.
    let mut xfs: Vec<u16> = Vec::new();
    let mut formats: std::collections::HashMap<u16, String> = std::collections::HashMap::new();
    let mut i = 0usize;
    let mut in_sst = false;
    while i + 4 <= stream.len() {
        let rt = u16le(&stream, i)?;
        let len = u16le(&stream, i + 2)? as usize;
        let body_start = i + 4;
        let body_end = (body_start + len).min(stream.len());
        let body = &stream[body_start..body_end];
        if rt == R_SST {
            sst_chunks.push(body);
            in_sst = true;
        } else if rt == R_CONTINUE && in_sst {
            sst_chunks.push(body);
        } else {
            in_sst = false;
            if rt == R_XF {
                // XF 레코드의 2바이트째가 이 서식이 가리키는 서식 번호(ifmt)다.
                xfs.push(u16le(body, 2).unwrap_or(0));
            } else if rt == R_FORMAT {
                // [서식번호(2)][서식 문자열]
                if let Some(id) = u16le(body, 0) {
                    if let Some(n) = u16le(body, 2) {
                        if let Some((code, _)) = read_unicode_string(body, 4, n as usize) {
                            formats.insert(id, code);
                        }
                    }
                }
            }
        }
        i = body_end;
        if len == 0 && body_end == body_start && rt == 0 { break; }   // 안전장치
    }
    let sst = parse_sst(&sst_chunks);

    // ── 2차: 셀 레코드를 (시트, 행, 열)로 모은다.
    //    시트 경계는 BOF/EOF 로 나뉘므로 BOF 를 만날 때마다 시트 번호를 올린다.
    let mut cells: BTreeMap<(usize, u16, u16), String> = BTreeMap::new();
    let mut sheet = 0usize;
    let mut seen_first_bof = false;
    // 문자열 결과를 낸 수식 셀의 자리 — 바로 뒤 STRING 레코드가 이 자리에 들어간다.
    let mut pending_str: Option<(usize, u16, u16)> = None;
    i = 0;
    while i + 4 <= stream.len() {
        let rt = match u16le(&stream, i) { Some(v) => v, None => break };
        let len = match u16le(&stream, i + 2) { Some(v) => v as usize, None => break };
        let bs = i + 4;
        let be = (bs + len).min(stream.len());
        let b = &stream[bs..be];
        // STRING 은 '바로 앞' FORMULA 의 결과다. 사이에 다른 레코드가 끼면 그 짝은
        // 깨진 것이므로, 매 레코드마다 표시를 걷어 두고 이번 레코드에서만 쓴다.
        // 이렇게 하지 않으면 엉뚱하게 멀리 떨어진 칸에 글자가 박힌다.
        let prev_pending = pending_str.take();
        match rt {
            R_BOF => {
                // 첫 BOF 는 워크북 자체 — 그 다음부터가 시트다.
                if seen_first_bof { sheet += 1; } else { seen_first_bof = true; }
            }
            R_EOF => {}
            R_LABELSST => {
                if let (Some(r), Some(c), Some(idx)) = (u16le(b, 0), u16le(b, 2), u32le(b, 6)) {
                    if let Some(s) = sst.get(idx as usize) {
                        cells.insert((sheet, r, c), s.clone());
                    }
                }
            }
            R_LABEL | R_RSTRING => {
                if let (Some(r), Some(c), Some(n)) = (u16le(b, 0), u16le(b, 2), u16le(b, 6)) {
                    if let Some((s, _)) = read_unicode_string(b, 8, n as usize) {
                        cells.insert((sheet, r, c), s);
                    }
                }
            }
            R_NUMBER => {
                if let (Some(r), Some(c)) = (u16le(b, 0), u16le(b, 2)) {
                    if let Some(raw) = b.get(6..14) {
                        let v = f64::from_le_bytes([raw[0], raw[1], raw[2], raw[3],
                                                    raw[4], raw[5], raw[6], raw[7]]);
                        // 4바이트째가 이 칸이 쓰는 칸 서식 번호(ixfe)다.
                        let s = cell_str(v, u16le(b, 4), &xfs, &formats);
                        cells.insert((sheet, r, c), s);
                    }
                }
            }
            R_RK => {
                if let (Some(r), Some(c), Some(rk)) = (u16le(b, 0), u16le(b, 2), u32le(b, 6)) {
                    let s = cell_str(rk_value(rk), u16le(b, 4), &xfs, &formats);
                    cells.insert((sheet, r, c), s);
                }
            }
            R_MULRK => {
                // [행][첫 열] (xf, rk)* [마지막 열] — 칸마다 제 서식 번호를 들고 있다.
                if let (Some(r), Some(c0)) = (u16le(b, 0), u16le(b, 2)) {
                    let n = b.len().saturating_sub(6) / 6;
                    for k in 0..n {
                        if let Some(rk) = u32le(b, 4 + k * 6 + 2) {
                            let s = cell_str(rk_value(rk), u16le(b, 4 + k * 6),
                                             &xfs, &formats);
                            cells.insert((sheet, r, c0 + k as u16), s);
                        }
                    }
                }
            }
            R_FORMULA => {
                // 식 자체가 아니라 '계산해 둔 결과'를 읽는다 — 사람이 표에서 보는 값이 그것이다.
                if let (Some(r), Some(c)) = (u16le(b, 0), u16le(b, 2)) {
                    match formula_value(b) {
                        // 수식 결과도 날짜 서식이면 날짜다 — 마감일 계산 칸이 흔하다.
                        FormulaVal::Num(v) => {
                            let s = cell_str(v, u16le(b, 4), &xfs, &formats);
                            cells.insert((sheet, r, c), s);
                        }
                        // 글자 결과는 바로 뒤 STRING 레코드에 온다 — 그 자리를 적어 둔다.
                        FormulaVal::Str => { pending_str = Some((sheet, r, c)); }
                        FormulaVal::Skip => {}
                    }
                }
            }
            R_STRING => {
                // 수식 결과 문자열. 바로 앞 FORMULA 가 자리를 알려 줬으면 그 칸에 넣는다.
                // 그래야 같은 행의 다른 칸과 한 줄로 붙어, 앞뒤가 붙어 있어야 성립하는
                // 문맥 규칙(예: 앵커 낱말 + 번호)이 제대로 걸린다.
                if let Some(n) = u16le(b, 0) {
                    if let Some((s, _)) = read_unicode_string(b, 2, n as usize) {
                        if !s.trim().is_empty() {
                            // 자리를 모르면 예전처럼 행 끝에 덧붙인다(값을 버리지는 않는다).
                            let key = prev_pending.unwrap_or((sheet, u16::MAX, cells.len() as u16));
                            cells.insert(key, s);
                        }
                    }
                }
            }
            _ => {}
        }
        i = be;
        if be <= bs && len == 0 && rt == 0 { break; }
    }

    // ── 행 단위로 탭 이어 붙이기(파이썬 xlrd 출력과 같은 모양).
    let mut out: Vec<String> = vec![];
    let mut cur: Option<(usize, u16)> = None;
    let mut row_cells: Vec<String> = vec![];
    for ((sh, r, _c), v) in &cells {
        let key = (*sh, *r);
        if cur != Some(key) {
            if !row_cells.is_empty() {
                out.push(row_cells.join("\t"));
                row_cells.clear();
            }
            // 시트가 바뀌면 빈 줄로 끊어 준다.
            if let Some((psh, _)) = cur {
                if psh != *sh { out.push(String::new()); }
            }
            cur = Some(key);
        }
        row_cells.push(v.clone());
    }
    if !row_cells.is_empty() {
        out.push(row_cells.join("\t"));
    }
    Some(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 수식 셀의 숫자 결과를 읽어야 한다 — 예전에는 이 레코드를 통째로 건너뛰어
    // 합계·환산 같은 '사람이 표에서 보는 값'이 전부 사라졌다.
    #[test]
    fn 수식셀의_숫자결과를_읽는다() {
        // 행0 열0, 서식0, 결과 243.0(IEEE754), 그 뒤는 식(본문 아님)
        let mut b = vec![0, 0, 0, 0, 0, 0];
        b.extend_from_slice(&243.0f64.to_le_bytes());
        b.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0]);
        match formula_value(&b) {
            FormulaVal::Num(v) => assert_eq!(v, 243.0),
            _ => panic!("숫자로 읽히지 않았다"),
        }
    }

    // 마지막 두 바이트가 0xFFFF 면 숫자가 아니라 '특별한 값' 표시다.
    // 첫 바이트 0 은 "글자 결과이고, 글자는 뒤 STRING 레코드에 있다"는 뜻.
    #[test]
    fn 문자열_결과는_뒤_레코드를_예고한다() {
        let b = [0u8, 0, 0, 0, 0, 0, /*값*/ 0, 0, 0, 0, 0, 0, 0xFF, 0xFF];
        assert!(matches!(formula_value(&b), FormulaVal::Str));
    }

    // 오류(#N/A 등)·논리값은 본문이 아니라 버린다 — 버리지 않으면 뜻 없는
    // 숫자가 본문에 섞여 검출기를 흔든다.
    #[test]
    fn 오류와_논리값은_버린다() {
        let err = [0u8, 0, 0, 0, 0, 0, 2, 0, 0x07, 0, 0, 0, 0xFF, 0xFF];
        assert!(matches!(formula_value(&err), FormulaVal::Skip));
        let bool_ = [0u8, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0xFF, 0xFF];
        assert!(matches!(formula_value(&bool_), FormulaVal::Skip));
    }

    // 레코드가 잘려 값 자리가 없으면 조용히 넘어가야 한다(패닉 금지).
    #[test]
    fn 짧은_수식레코드는_건너뛴다() {
        assert!(matches!(formula_value(&[0, 0, 0, 0, 0, 0]), FormulaVal::Skip));
    }

    // RK 는 정수/실수 · 100분의1 네 가지 조합이 있다.
    #[test]
    fn rk값을_네_가지_모두_푼다() {
        assert_eq!(rk_value((3 << 2) | 0b10), 3.0);
        assert_eq!(rk_value((250 << 2) | 0b11), 2.5);
        let bits = (1.0f64).to_bits();
        let rk = (bits >> 32) as u32 & 0xFFFF_FFFC;
        assert_eq!(rk_value(rk), 1.0);
    }

    // 숫자 표기 — 정수는 소수점 없이(파이썬 xlrd 출력과 눈으로 같게).
    #[test]
    fn 정수는_소수점_없이_쓴다() {
        assert_eq!(num_str(3.0), "3");
        assert_eq!(num_str(2.5), "2.5");
    }

    // BIFF8 문자열: 압축(1바이트) / 와이드(UTF-16) 둘 다.
    #[test]
    fn 압축과_와이드_문자열을_읽는다() {
        let b = vec![0x00, b'A', b'B'];
        assert_eq!(read_unicode_string(&b, 0, 2).unwrap().0, "AB");
        let mut w = vec![0x01];
        for c in "계약".encode_utf16() { w.extend_from_slice(&c.to_le_bytes()); }
        assert_eq!(read_unicode_string(&w, 0, 2).unwrap().0, "계약");
    }

    #[test]
    fn 잘린_문자열은_none() {
        assert!(read_unicode_string(&[0x01, 0x00], 0, 5).is_none());
        assert!(read_unicode_string(&[], 0, 1).is_none());
    }

    #[test]
    fn ole가_아니면_none() {
        assert!(xls_text(b"not ole".to_vec()).is_none());
    }

    // 회귀 핵심 — CONTINUE 경계에서 문자열이 잘려도 표 전체가 복원돼야 한다.
    // 이 처리가 없으면 경계 뒤로 한 바이트씩 밀려 절반 넘게 유실된다.
    #[test]
    fn sst가_continue_경계를_넘어도_전부_읽는다() {
        // 문자열 2개("가나", "다라")를 만들고, 두 번째의 글자 한가운데를 잘라
        // 두 조각으로 나눈다. 이어지는 조각 앞에는 플래그 바이트가 붙는다.
        let mut c1: Vec<u8> = vec![];
        c1.extend_from_slice(&2u32.to_le_bytes());      // total strings
        c1.extend_from_slice(&2u32.to_le_bytes());      // unique strings
        // "가나"
        c1.extend_from_slice(&2u16.to_le_bytes());
        c1.push(0x01);
        for ch in "가나".encode_utf16() { c1.extend_from_slice(&ch.to_le_bytes()); }
        // "다라" — 헤더와 첫 글자까지만 첫 조각에
        c1.extend_from_slice(&2u16.to_le_bytes());
        c1.push(0x01);
        for ch in "다".encode_utf16() { c1.extend_from_slice(&ch.to_le_bytes()); }
        // 두 번째 조각: 플래그 + 남은 글자
        let mut c2: Vec<u8> = vec![0x01];
        for ch in "라".encode_utf16() { c2.extend_from_slice(&ch.to_le_bytes()); }

        let got = parse_sst(&[&c1[..], &c2[..]]);
        assert_eq!(got, vec!["가나".to_string(), "다라".to_string()]);
    }

    // 경계 뒤 조각이 1바이트 표기로 바뀌는 경우도 따라가야 한다.
    #[test]
    fn continue_에서_표기폭이_바뀌어도_따라간다() {
        let mut c1: Vec<u8> = vec![];
        c1.extend_from_slice(&1u32.to_le_bytes());
        c1.extend_from_slice(&1u32.to_le_bytes());
        c1.extend_from_slice(&3u16.to_le_bytes());
        c1.push(0x01);                                   // 와이드로 시작
        for ch in "가".encode_utf16() { c1.extend_from_slice(&ch.to_le_bytes()); }
        let c2: Vec<u8> = vec![0x00, b'A', b'B'];        // 경계 뒤는 1바이트 표기
        assert_eq!(parse_sst(&[&c1[..], &c2[..]]), vec!["가AB".to_string()]);
    }
}

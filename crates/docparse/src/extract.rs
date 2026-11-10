//! 포맷별 텍스트 추출 (Python extract/* 포팅)
//! 지원: text · html · docx · xlsx · pptx · hwpx · pdf.
//! 구형 doc/ppt(OLE)는 ole.rs + office_legacy.rs 로 직접 파싱한다(상용 컴포넌트 없이).
//! 구형 xls(BIFF8)·hwp5(OLE+raw deflate)도 직접 파싱한다.
//! 미지원: 이미지(OCR) · 암호화 문서.
//!
//! [전역 상태 없음] 추출 모드·상한·부분추출 표식은 모두 인자([`ExtractOptions`])와
//! 반환값([`Extracted`])으로 오간다. 그래서 여러 스레드에서 동시에 불러도 된다.
//! 설계: plan/Rust-문서파서-라이브러리분리-설계-20261110.html §4·§5

use std::cell::RefCell;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;

use crate::detect::Fmt;
use crate::limits::{check_size_limit, Limits, ParserDeadline};

//------------------------------------------------------------------
// 추출 엔진 — 어느 추출기로 본문을 뽑을지
//=> 설계: plan/Rust-사이냅추출기-옵션-설계-20261019.html §3·§4
//   (분리 전 ExtractMode 와 같다. 전역에 한 번 새기던 것을 매번 인자로 넘긴다.)
//
// -필드: Builtin       = 자체 파서만(기본)
// -필드: SynapFallback = 자체 파서 1차, 실패·빈 결과·짧은 결과면 사이냅(snf_exe 경로를 쥔다)
// -필드: SynapOnly     = 모든 문서를 사이냅으로(snf_exe 경로를 쥔다)
//------------------------------------------------------------------
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Engine {
    Builtin,
    #[cfg(feature = "synap")]
    SynapFallback(PathBuf),
    #[cfg(feature = "synap")]
    SynapOnly(PathBuf),
}

impl Engine {
    //--------------------------------------------------------------
    // 로그에 적을 엔진 이름
    //
    // -in: 없음
    //
    // -out: &str = "builtin" · "snf_fallback" · "snf_only"
    // -out: error = 없음
    //--------------------------------------------------------------
    pub fn name(&self) -> &'static str {
        match self {
            Engine::Builtin => "builtin",
            #[cfg(feature = "synap")]
            Engine::SynapFallback(_) => "snf_fallback",
            #[cfg(feature = "synap")]
            Engine::SynapOnly(_) => "snf_only",
        }
    }
}

//------------------------------------------------------------------
// 추출 옵션 — 추출 한 번에 필요한 설정 전부
//=> 예전에는 모드(set_mode)·상한(set_overrides)·pdfium 위치(CSO_PDFIUM)를 프로그램
//   전역과 환경변수에서 읽었다. 이제는 이 값 하나로 넘긴다.
//
// -필드: engine     = 추출 엔진(기본 Builtin)
// -필드: limits     = 크기·시간·페이지 상한
// -필드: pdfium_lib = pdfium 동적 라이브러리 경로. None(또는 없는 파일)이면
//                     실행파일 옆 → 시스템 순으로 찾는다
// -필드: clean      = true 면 6단계 정제까지(기본). false 면 엔진이 뽑은 원시 텍스트
// -필드: label      = 로그에 적을 문서 이름(None 이면 경로)
//------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub engine: Engine,
    pub limits: Limits,
    pub pdfium_lib: Option<PathBuf>,
    pub clean: bool,
    pub label: Option<String>,
}

impl Default for ExtractOptions {
    //------------------------------------------------------------------
    // 기본 옵션 — 자체 파서 · 기본 상한 · 정제까지
    //
    // -in: 없음
    //
    // -out: ExtractOptions
    // -out: error = 없음
    //------------------------------------------------------------------
    fn default() -> Self {
        ExtractOptions {
            engine: Engine::Builtin,
            limits: Limits::default(),
            pdfium_lib: None,
            clean: true,
            label: None,
        }
    }
}

//------------------------------------------------------------------
// 부분 추출 표식 — "끝까지 못 읽고 상한에서 멈췄다"
//=> 본문이 아니라 '본문에 대한 사실'이라 text 와 따로 돌려준다.
//
// -필드: reason = 왜 멈췄나("시간 상한" · "페이지 상한(3000p)")
// -필드: unit   = 센 단위("pages" · "parts" · "sheets")
// -필드: read   = 읽은 수
// -필드: total  = 전체 수
//------------------------------------------------------------------
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partial {
    pub reason: String,
    pub unit: &'static str,
    pub read: usize,
    pub total: usize,
}

//------------------------------------------------------------------
// PDF 페이지 구성 — 스캔 PDF 처리제외 판정 재료
//
// -필드: pages      = 읽은 페이지 수
// -필드: scan_pages = 그중 글자 없이 그림만 든 페이지 수
//------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub pages: usize,
    pub scan_pages: usize,
}

//------------------------------------------------------------------
// 추출 결과
//
// -필드: text    = 본문(clean=true 면 정제한 것)
// -필드: engine  = 채택한 엔진 이름("pdf" · "docx" · … · "snf")
// -필드: fmt     = 감지(또는 지정)한 포맷
// -필드: partial = 상한에서 멈췄으면 그 표식
// -필드: layout  = PDF 를 자체 파서로 읽었으면 페이지 구성
//------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct Extracted {
    pub text: String,
    pub engine: &'static str,
    pub fmt: Fmt,
    pub partial: Option<Partial>,
    pub layout: Option<Layout>,
}

//------------------------------------------------------------------
// 추출 실패
//
// -필드: SizeLimit   = 크기 상한을 넘어 읽지 않았다(깨진 파일과 구분)
// -필드: Unsupported = 자체 파서가 없는 포맷(Builtin 엔진일 때)
// -필드: Failed      = 엔진이 읽지 못했다. 사이냅 모드면 엔진별 사유, Builtin 은 빈 문자열
//------------------------------------------------------------------
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    SizeLimit { size: u64, limit: u64, text_kind: bool },
    Unsupported(Fmt),
    Failed(String),
}

impl std::fmt::Display for ExtractError {
    //------------------------------------------------------------------
    // 사람이 읽는 실패 문장
    //=> 크기 상한은 분리 전과 같은 "크기 상한 초과: …" 문장을 낸다.
    //
    // -in: f = 출력 대상
    //
    // -out: fmt::Result
    // -out: error = 없음
    //------------------------------------------------------------------
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::SizeLimit { size, limit, text_kind } => {
                let h = crate::limits::SizeLimitHit { size: *size, limit: *limit, text_kind: *text_kind };
                write!(f, "{}", h.reason())
            }
            ExtractError::Unsupported(fmt) => write!(f, "지원하지 않는 포맷(감지={})", fmt.as_str()),
            ExtractError::Failed(why) => write!(f, "{}", why),
        }
    }
}

impl std::error::Error for ExtractError {}

//------------------------------------------------------------------
// 추출 한 번 동안의 내부 상태 (라이브러리 안에서만 쓴다)
//=> 깊은 파서(PDF·OOXML·xlsx 루프)가 상한을 보고 표식을 남길 자리다. 예전에는
//   전역 상한과 thread_local 메모가 이 일을 했다. 이제 추출 한 번마다 새로 만들어
//   함수 인자로 내려보내므로 문서끼리·스레드끼리 섞일 수 없다.
//
// -필드: limits     = 이번 추출의 상한
// -필드: pdfium_lib = pdfium 경로(지정값)
// -필드: partial    = 부분 추출 표식(파서가 적는다)
// -필드: layout     = PDF 페이지 구성(PDF 파서가 적는다)
//------------------------------------------------------------------
pub(crate) struct Run<'a> {
    limits: &'a Limits,
    pdfium_lib: Option<&'a Path>,
    partial: RefCell<Option<Partial>>,
    layout: RefCell<Option<Layout>>,
}

impl<'a> Run<'a> {
    //------------------------------------------------------------------
    // 엔진 한 번 시도용 새 상태
    //=> 엔진마다 새로 만든다 — 1차 파서가 남긴 '부분 추출'이 사이냅 결과에 붙지
    //   않게(분리 전 notes_reset 과 같은 자리).
    //
    // -in: opts = 추출 옵션
    //
    // -out: Run
    // -out: error = 없음
    //------------------------------------------------------------------
    fn new(opts: &'a ExtractOptions) -> Self {
        Run {
            limits: &opts.limits,
            pdfium_lib: opts.pdfium_lib.as_deref(),
            partial: RefCell::new(None),
            layout: RefCell::new(None),
        }
    }

    //------------------------------------------------------------------
    // '상한에서 멈췄다' 적어 두기
    //
    // -in: reason = 왜 멈췄나 · unit = 단위 이름 · read = 읽은 수 · total = 전체 수
    //
    // -out: 없음
    // -out: error = 없음
    //------------------------------------------------------------------
    fn set_partial(&self, reason: String, unit: &'static str, read: usize, total: usize) {
        *self.partial.borrow_mut() = Some(Partial { reason, unit, read, total });
    }

    //------------------------------------------------------------------
    // PDF 페이지 구성 적어 두기
    //
    // -in: pages = 읽은 페이지 수 · scan_pages = 그중 스캔 페이지 수
    //
    // -out: 없음
    // -out: error = 없음
    //------------------------------------------------------------------
    fn set_layout(&self, pages: usize, scan_pages: usize) {
        *self.layout.borrow_mut() = Some(Layout { pages, scan_pages });
    }

    //------------------------------------------------------------------
    // 적어 둔 표식 꺼내기(상태를 소비한다)
    //
    // -in: 없음
    //
    // -out: (Option<Partial>, Option<Layout>)
    // -out: error = 없음
    //------------------------------------------------------------------
    fn into_notes(self) -> (Option<Partial>, Option<Layout>) {
        (self.partial.into_inner(), self.layout.into_inner())
    }
}

//------------------------------------------------------------------
// 이 포맷에 자체 파서가 있는가
//=> extract_raw 가 다루는 포맷과 같다(파이썬 하이브리드 엔진 표와도 같다). 없는 포맷
//   (이미지 · OLE · zip · 이진 · 모름)은 사이냅 폴백 모드에서 곧바로 사이냅으로 간다.
//
// -in: fmt = 감지한 포맷
//
// -out: bool
// -out: error = 없음
//------------------------------------------------------------------
fn has_builtin(fmt: Fmt) -> bool {
    matches!(fmt, Fmt::Text | Fmt::Html | Fmt::Docx | Fmt::Pptx | Fmt::Hwpx | Fmt::Xlsx
        | Fmt::Pdf | Fmt::Doc | Fmt::Ppt | Fmt::Xls | Fmt::Hwp)
}

//------------------------------------------------------------------
// 파일 하나에서 본문 뽑기 (형식은 내용으로 감지)
//=> 형식을 모를 때 쓴다. detect_format 으로 감지한 뒤 extract_as 와 같다.
//
// -in: path = 읽을 파일
// -in: opts = 추출 옵션
//
// -out: Ok(Extracted) = 본문과 부가정보
// -out: error = Err(ExtractError) — 크기 상한 · 미지원 포맷 · 엔진 실패
//------------------------------------------------------------------
pub fn extract(path: &Path, opts: &ExtractOptions) -> Result<Extracted, ExtractError> {
    extract_as(path, crate::detect::detect_format(path), opts)
}

//------------------------------------------------------------------
// 형식을 알고 있는 파일에서 본문 뽑기 — 엔진 사슬과 폴백 판정
//=> 파이썬 HybridExtractor.extract 와 같은 규칙 · 같은 임계로 엔진을 차례로 시도한다.
//    0) 크기 상한(G1·G2)을 먼저 본다 — 넘으면 읽지 않고 SizeLimit
//    1) 엔진별 사슬: 자체 파서만 / [자체 파서] + [snf] / [snf]
//       (폴백 모드에서 자체 파서가 없는 포맷이면 [snf])
//    2) 엔진마다 '부분 추출' 표식을 새로 시작한다 — 앞 엔진의 표식이 뒤 엔진 결과에 붙지 않게
//    3) 실패 · 빈 결과 → 다음 엔진. 짧은 결과(공백 빼고 min_text_len 미만) → 붙잡아 두고 다음
//    4) 충분한 결과면 채택. 모두 짧으면 가장 긴 것. 모두 실패면 엔진별 사유를 이어 붙여 Err
//    5) 채택한 원시 텍스트를 6단계로 정제한다(clean=true 일 때)
//
// -in: path = 읽을 파일
// -in: fmt  = 포맷(이미 감지했으면 그 값)
// -in: opts = 추출 옵션
//
// -out: Ok(Extracted) = 본문과 부가정보
// -out: error = Err(ExtractError) — 크기 상한 · 미지원 포맷(Builtin) · 엔진 실패
//------------------------------------------------------------------
pub fn extract_as(path: &Path, fmt: Fmt, opts: &ExtractOptions) -> Result<Extracted, ExtractError> {
    // 크기 상한은 라이브러리가 먼저 본다 — 쓰는 쪽이 빠뜨리기 쉬운 검사라서다.
    if let Some(h) = check_size_limit(path, fmt, &opts.limits) {
        return Err(ExtractError::SizeLimit { size: h.size, limit: h.limit, text_kind: h.text_kind });
    }
    // 로그에 적을 이름 — 풀린 문서면 부른 쪽이 원래 경로를 label 로 준다.
    let label_owned;
    let label: &str = match opts.label.as_deref() {
        Some(l) => l,
        None => { label_owned = path.display().to_string(); &label_owned }
    };
    // 정제 여부를 한곳에서 — clean=false 면 원시 텍스트 그대로 돌려준다.
    let finish = |t: &str| if opts.clean { clean_text(t) } else { t.to_string() };

    #[cfg(feature = "synap")]
    let snf = match &opts.engine {
        Engine::Builtin => return extract_builtin(path, fmt, opts, &finish),
        Engine::SynapFallback(p) | Engine::SynapOnly(p) => p,
    };
    #[cfg(not(feature = "synap"))]
    {
        let _ = label;
        return extract_builtin(path, fmt, opts, &finish);
    }

    #[cfg(feature = "synap")]
    {
        let snf_secs = opts.limits.snf_timeout.as_secs();
        // 사이냅만 모드 — 파이썬 --synap-only 처럼 사이냅 하나만 부르고, 실패면 그 사유를 그대로.
        if let Engine::SynapOnly(_) = &opts.engine {
            return match crate::synap::snf_extract(snf, path, snf_secs) {
                Ok(t) => {
                    log::info!("추출 engine=snf(감지={}) file={} chars={}", fmt.as_str(), label, t.chars().count());
                    Ok(Extracted { text: finish(&t), engine: "snf", fmt, partial: None, layout: None })
                }
                Err(e) => Err(ExtractError::Failed(format!("snf:{}", e))),
            };
        }
        // 사이냅 폴백 모드 — 자체 파서가 있으면 1차, 사이냅은 언제나 마지막.
        let min_len = opts.limits.min_text_len;
        let primary = if has_builtin(fmt) { fmt.as_str() } else { "snf" };
        let mut reasons: Vec<String> = vec![];
        let mut best_short: Option<(&'static str, String)> = None;
        // 마지막으로 시도한 엔진이 남긴 표식 — 분리 전에는 엔진마다 메모를 비웠으므로,
        // 결과에 붙는 표식은 언제나 '마지막에 돈 엔진'의 것이었다. 그 규칙을 그대로 지킨다.
        let mut notes: (Option<Partial>, Option<Layout>) = (None, None);
        let engines: Vec<&'static str> = if has_builtin(fmt) { vec![fmt.as_str(), "snf"] } else { vec!["snf"] };
        for name in engines {
            let got: Result<String, String> = if name == "snf" {
                notes = (None, None);
                crate::synap::snf_extract(snf, path, snf_secs)
            } else {
                // 엔진마다 새 상태 — 1차 파서가 남긴 '부분 추출'이 사이냅 결과에 붙지 않게.
                let run = Run::new(opts);
                let r = extract_raw(path, fmt, &run).ok_or_else(|| "failed".to_string());
                notes = run.into_notes();
                r
            };
            let text = match got {
                Ok(t) => t,
                Err(e) => { reasons.push(format!("{}:{}", name, e)); continue; }
            };
            if text.trim().is_empty() {
                reasons.push(format!("{}:empty", name));
                continue;
            }
            // '성공했지만 본문이라 할 게 없는' 결과도 폴백 대상이다(파이썬과 같은 판정).
            let n = text.chars().filter(|c| !c.is_whitespace()).count();
            if n < min_len {
                reasons.push(format!("{}:short({}자)", name, n));
                if best_short.as_ref().map_or(true, |(_, b)| text.len() > b.len()) {
                    best_short = Some((name, text));
                }
                continue;
            }
            if name != primary {
                log::info!("추출 폴백 from={} to={} file={} reason={}", primary, name, label, reasons.join(";"));
            } else {
                log::info!("추출 engine={}(감지={}) file={} chars={}", name, fmt.as_str(), label, text.chars().count());
            }
            return Ok(Extracted { text: finish(&text), engine: name, fmt, partial: notes.0, layout: notes.1 });
        }
        // 모두 짧으면 가장 긴 것 — 예전에도 짧은 채로 성공이던 문서를 실패로 바꾸지 않는다.
        if let Some((name, text)) = best_short {
            log::info!("추출 짧은결과채택 engine={} file={} chars={} reason={}",
                       name, label, text.chars().count(), reasons.join(";"));
            return Ok(Extracted { text: finish(&text), engine: name, fmt, partial: notes.0, layout: notes.1 });
        }
        Err(ExtractError::Failed(format!("하이브리드 전엔진 실패(감지={}; {})", fmt.as_str(), reasons.join("; "))))
    }
}

//------------------------------------------------------------------
// 자체 파서 하나로 뽑기 (Builtin 엔진)
//=> 분리 전 extract_text 와 똑같이 돈다 — 원시 추출 → 정제. 실패 사유는 빈 문자열
//   (부른 쪽이 예전 문장 "추출 실패: … (감지=…)" 을 그대로 쓴다).
//
// -in: path   = 읽을 파일
// -in: fmt    = 포맷
// -in: opts   = 추출 옵션
// -in: finish = 정제 함수(clean 여부 반영)
//
// -out: Ok(Extracted)
// -out: error = 자체 파서 없는 포맷이면 Unsupported, 읽기 실패면 Failed("")
//------------------------------------------------------------------
fn extract_builtin(path: &Path, fmt: Fmt, opts: &ExtractOptions,
                   finish: &dyn Fn(&str) -> String) -> Result<Extracted, ExtractError> {
    if !has_builtin(fmt) {
        return Err(ExtractError::Unsupported(fmt));
    }
    let run = Run::new(opts);
    let raw = extract_raw(path, fmt, &run);
    let (partial, layout) = run.into_notes();
    match raw {
        Some(t) => Ok(Extracted { text: finish(&t), engine: fmt.as_str(), fmt, partial, layout }),
        None => Err(ExtractError::Failed(String::new())),
    }
}

// 페이지 마커 라인: 사이냅 기본 출력의 '..PAGE:12' 같은 줄.
static PAGE_MARKER: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"(?m)^\s*\.\.PAGE:\d+\s*$").unwrap());
// 제어문자(탭/개행 제외): 임베딩에 무의미하고 토크나이저를 흔들 수 있어 제거.
static CTRL: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]").unwrap());
// 줄 안의 연속 공백/탭 → 공백 1개.
static MULTI_SPACE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"[ \t]{2,}").unwrap());
// 3줄 이상 연속 빈 줄 → 2줄로 축약(문단 경계는 보존).
static MULTI_BLANK: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"\n{3,}").unwrap());

/// 추출 원시 텍스트 정제 — 파이썬 `clean.clean_text` 와 **단계·차례가 같다**.
///
/// [왜 이 함수가 생겼나] 예전에는 여기에 개행 통일 한 가지만 있었다. 파이썬은
/// 여섯 단계를 거치는데 다섯 단계가 통째로 빠져 있었다. 그래서 같은 문서에서
/// 두 판이 **서로 다른 본문**을 보고 판정했다 — 실측 d:/sample 18건이 전부
/// 갈렸다(연속공백 11 · 빈줄 17 · 탭 4건).
///
/// 갈린 본문은 곧 갈린 판정이다. 표제부(앞 400자)가 가리키는 범위가 달라지고,
/// PII 정규식의 경계 검사 결과가 달라지고, 임베딩 벡터가 달라진다. 그런데
/// 임베더가 512토큰에서 자르는 탓에 **짧은 문서에서만** 벡터 차이가 드러나,
/// 18건 중 1건만 어긋난 것처럼 보였다. 오류도 경고도 안 나는 종류였다.
///
/// 개행 통일(3-2)만은 예전부터 있었다 — CR+LF 를 2글자로 세면 표제부 경계가
/// 밀리기 때문이다(설계서 14-3 교차 검증). 그 자리를 그대로 물려받았다.
///
/// -in: text = 추출기가 뽑은 원시 텍스트
///
/// -out: 정제된 텍스트(양끝 공백 제거). 입력이 비면 빈 문자열
/// -out: error = 없음(예외 없이 언제나 문자열)
pub fn clean_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    // (1) 자모가 분리된 한글 등을 완성형(NFC)으로 통일 → 토큰 일관성 확보.
    //     맥에서 만든 문서·일부 HWP 본문이 분해형(NFD)으로 들어온다. 그대로 두면
    //     같은 낱말이 두 판에서 다른 토큰이 되어 벡터가 갈린다.
    use unicode_normalization::UnicodeNormalization;
    let mut t: String = text.nfc().collect();

    // (2) 페이지 마커는 본문이 아니므로 라인 통째로 제거.
    t = PAGE_MARKER.replace_all(&t, "").into_owned();

    // (3-1) 눈에 안 보이는 제어문자 제거(탭/개행은 남겨 구조 보존).
    t = CTRL.replace_all(&t, "").into_owned();

    // (3-2) 윈도우 개행(CR+LF)·맥 개행(CR)을 LF 로 통일.
    if t.as_bytes().contains(&b'\r') {
        t = t.replace("\r\n", "\n").replace('\r', "\n");
    }

    // (3-3) 줄 안 연속 공백 축약. 탭도 함께 걸린다 — 표 셀이 빈 칸이면 탭이
    //       여러 개 이어지는데, 그것을 공백 하나로 줄이는 것이 파이썬 동작이다.
    t = MULTI_SPACE.replace_all(&t, " ").into_owned();

    // (3-4) 각 줄 끝의 잉여 공백 제거.
    let mut out = String::with_capacity(t.len());
    for (i, line) in t.split('\n').enumerate() {
        if i > 0 { out.push('\n'); }
        out.push_str(line.trim_end());
    }

    // (3-5) 빈 줄이 3줄 이상 이어지면 2줄로 축약.
    let out = MULTI_BLANK.replace_all(&out, "\n\n").into_owned();

    out.trim().to_string()
}

//------------------------------------------------------------------
// 포맷별 자체 파서로 원시 텍스트 뽑기
//=> 포맷에 맞는 파서 하나를 부른다. 정제는 하지 않는다(부른 쪽이 한다).
//
// -in: path = 읽을 파일
// -in: fmt  = 포맷
// -in: run  = 이번 추출의 상한·표식 자리(깊은 파서가 상한을 보고 표식을 적는다)
//
// -out: Option<String> = 원시 텍스트
// -out: error = 미지원 포맷이거나 읽기 실패면 None
//------------------------------------------------------------------
fn extract_raw(path: &Path, fmt: Fmt, run: &Run) -> Option<String> {
    match fmt {
        Fmt::Text => plaintext(path),
        Fmt::Html => html(path),
        // 문단 인식: 문단 내 런(run)을 이어붙이고 문단 사이만 개행. Python 과 동일하게
        // 하여 런 경계로 쪼개진 값(예: 절번호 "4.2.2.2.")이 갈라져 오탐되는 것을 막는다.
        Fmt::Docx => docx_text(path),
        // 슬라이드 본문 + 발표자 노트. 노트에 설계·단가 같은 기밀이 적히는 일이
        // 잦아, 본문만 읽으면 그 문서는 등급이 내려앉는다.
        Fmt::Pptx => pptx_text(path, run),
        // HWPX 도 문단 단위로 이어 붙인다 — 태그 단위로 끊던 예전 판은 낱말을 쪼갰다.
        Fmt::Hwpx => for_prefix(path, "Contents/section", collect_hwpx_paragraphs, run),
        Fmt::Xlsx => xlsx(path, run),
        Fmt::Pdf => pdf_text(path, run),
        // 구형 Office(OLE): 파일 전체를 읽어 CFB 컨테이너에서 본문 스트림을 꺼낸다.
        Fmt::Doc => fs::read(path).ok().and_then(crate::office_legacy::doc_text),
        Fmt::Ppt => fs::read(path).ok().and_then(crate::office_legacy::ppt_text),
        Fmt::Xls => fs::read(path).ok().and_then(crate::xls::xls_text),
        Fmt::Hwp => fs::read(path).ok().and_then(crate::hwp5::hwp5_text),
        // 미지원 포맷: 추출 불가(별도 과제)
        _ => None,
    }
}

//------------------------------------------------------------------
// pdfium 라이브러리 경로 결정
//=> 지정 경로(옵션 pdfium_lib) → 이 라이브러리(docparse.dll/.so) 자신의 폴더
//   → 실행파일 옆 → 시스템 순.
//   지정 경로가 실제 파일이 아니면 다음 후보로 넘어간다(분리 전 CSO_PDFIUM 과 같은 규칙).
//   '자기 폴더'를 실행파일보다 먼저 보는 까닭: Python 이 DLL 을 불러 쓰면 실행파일은
//   python.exe 라서, 실행파일 옆만 보면 pdfium 을 못 찾고 PDF 가 조용히 빈 본문이 된다.
//   Rust 실행파일에 정적으로 링크된 경우엔 자기 폴더 = 실행파일 폴더라 결과가 같다.
//
// -in: explicit = 부른 쪽이 지정한 경로(없으면 None)
//
// -out: Option<PathBuf> = 쓸 라이브러리 경로(None 이면 시스템 라이브러리)
// -out: error = 없음
//------------------------------------------------------------------
fn pdfium_lib_path(explicit: Option<&Path>) -> Option<std::path::PathBuf> {
    if let Some(p) = explicit {
        if p.is_file() { return Some(p.to_path_buf()); }
    }
    // 찾아볼 폴더: 이 모듈(DLL/.so)이 있는 폴더 → 실행파일 폴더(같으면 한 번만)
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = crate::modpath::module_dir() {
        dirs.push(d);
    }
    if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    for dir in &dirs {
        for name in ["pdfium.dll", "libpdfium.so", "libpdfium.dylib"] {
            let pb = dir.join(name);
            if pb.is_file() { return Some(pb); }
        }
    }
    None
}

//------------------------------------------------------------------
// PDF 텍스트층 추출 (pdfium-render)
//=> 페이지를 차례로 읽되 시간·페이지 상한(G5)에서 멈추고, 페이지 구성과 부분추출
//   표식을 run 에 적는다.
//
// -in: path = PDF 경로
// -in: run  = 상한(parser_timeout·max_pdf_pages)·pdfium 경로·표식 자리
//
// -out: Option<String> = 페이지마다 줄을 바꿔 이은 텍스트
// -out: error = pdfium 을 못 붙이거나 PDF 를 못 열면 None
//------------------------------------------------------------------
/// PDF 텍스트층 추출 (pdfium-render). pdfium 동적 라이브러리가 있어야 한다.
/// 스캔(이미지)본은 텍스트층이 없어 빈 문자열이 될 수 있다(그건 상위에서 미분류 처리).
fn pdf_text(path: &Path, run: &Run) -> Option<String> {
    use pdfium_render::prelude::*;
    // 바인딩: 지정 경로 우선, 없으면 시스템 라이브러리.
    let bindings = match pdfium_lib_path(run.pdfium_lib) {
        Some(p) => Pdfium::bind_to_library(p).ok()?,
        None => Pdfium::bind_to_system_library().ok()?,
    };
    let pdfium = Pdfium::new(bindings);
    let doc = pdfium.load_pdf_from_file(path, None).ok()?;
    let mut out = String::new();
    // 페이지·시간 상한(G5) — 설계: plan/문서크기-상한-설계-20260904.html
    // 지금까지 이 루프에는 아무 제한이 없어, 수천 페이지 PDF 한 건이 배치 전체를
    // 무한정 붙잡을 수 있었다. 두 겹으로 막는다: ① 시간 예산 ② 페이지 수.
    // 넘겨도 실패로 처리하지 않고 '그때까지 읽은 부분'을 돌려준다 — 부분 텍스트라도
    // 등급 판정에는 대개 충분하고, 버리면 그 문서가 미분류로 쌓인다.
    let deadline = ParserDeadline::new(run.limits.parser_timeout);
    // None(무제한)은 분리 전의 0 과 같은 뜻이다.
    let max_pages = run.limits.max_pdf_pages.unwrap_or(0);
    let mut n_read = 0usize;
    let mut stopped: Option<String> = None;
    // 스캔 페이지(글자 거의 없음 + 그림이 절반 이상) 수 — 처리제외 판정 재료.
    let mut scan_pages = 0usize;
    for page in doc.pages().iter() {
        if max_pages > 0 && n_read >= max_pages {
            stopped = Some(format!("페이지 상한({}p)", max_pages));
            break;
        }
        if deadline.expired() {
            stopped = Some("시간 상한".to_string());
            break;
        }
        match page.text() {
            Ok(t) => {
                let s = t.all();
                if pdf_is_scan_page(&page, &s) {
                    scan_pages += 1;
                }
                out.push_str(&s);
                out.push('\n');
            }
            // 텍스트층을 못 열어도 그림만 든 페이지일 수 있다 — 글자 0자로 보고 판정한다.
            Err(_) => {
                if pdf_is_scan_page(&page, "") {
                    scan_pages += 1;
                }
            }
        }
        n_read += 1;
    }
    // 페이지 구성을 남긴다 — 분류 쪽이 최종 본문 길이와 함께 '처리제외(스캔 PDF)'를
    // 가린다(파이썬 판과 같은 자리·같은 값).
    run.set_layout(n_read, scan_pages);
    // 잘렸다면 반드시 남긴다 — 남기지 않으면 '왜 이 문서만 본문이 적지'를 나중에
    // 추적할 근거가 사라진다.
    if let Some(why) = stopped {
        log::warn!(target: crate::ERRLOG_TARGET, "[PDF 부분추출] {} — {} 에서 중단(pages={})",
                   path.display(), why, n_read);
        // 로그만으로는 결과를 보는 사람에게 안 닿는다 — 레코드까지 표식을 보낸다.
        run.set_partial(why, "pages", n_read, doc.pages().len() as usize);
    }
    Some(out)
}

// ── 스캔 PDF 처리제외(EXCLUDED) 판정 — 1차 개발 범위: OCR 미적용 ──
// 파이썬 판 config.SCAN_PDF_* 와 같은 값이어야 한다(tests/test_scan_pdf.py 가 지킨다).
// [값의 근거] D:\분류함 PDF 382건 실측 — 스캔본은 페이지당 0~0.1자·그림 덮음 100%,
// 그림이 많은 정상 문서(매뉴얼·제품소개서)는 페이지당 50자 이상이었다.
/// 이 글자 수(공백 제외) 미만인 페이지만 스캔 후보.
pub const SCAN_PDF_PAGE_MAX_CHARS: usize = 10;
/// 그림이 페이지 넓이의 이 비율 이상을 덮어야 스캔 페이지.
pub const SCAN_PDF_IMAGE_COVER: f64 = 0.5;
/// 스캔 페이지가 읽은 페이지의 이 비율 이상이어야 스캔 PDF.
pub const SCAN_PDF_PAGE_RATIO: f64 = 0.5;

//------------------------------------------------------------------
// 이 페이지가 스캔 페이지인가 (처리제외 판정 재료)
//=> 글자가 거의 없고, 그림이 페이지 넓이의 절반 이상을 덮으면 스캔 페이지로 본다.
//    1) 글자가 SCAN_PDF_PAGE_MAX_CHARS 이상이면 바로 아니다 — 정상 페이지는 그림을
//       훑지 않아 느려지지 않는다
//    2) 페이지 바로 아래 그림 개체의 영역을 페이지 안으로 잘라 넓이를 더한다
//       (파이썬 판 get_objects(max_depth=0) 와 같은 범위. 실측 382건에서 폼 개체 안까지
//        봐도 판정이 같았다)
//    3) 덮은 비율이 SCAN_PDF_IMAGE_COVER 이상이면 스캔 페이지
//
// -in: page = pdfium 페이지 · text = 그 페이지 원문
// -out: bool = 스캔 페이지면 true
// -out: error = 없음(크기가 0 인 페이지는 false)
//------------------------------------------------------------------
fn pdf_is_scan_page(page: &pdfium_render::prelude::PdfPage, text: &str) -> bool {
    use pdfium_render::prelude::*;
    if text.chars().filter(|c| !c.is_whitespace()).count() >= SCAN_PDF_PAGE_MAX_CHARS {
        return false;
    }
    let (w, h) = (page.width().value as f64, page.height().value as f64);
    if w <= 0.0 || h <= 0.0 {
        return false;
    }
    let mut covered = 0.0f64;
    for obj in page.objects().iter() {
        if obj.object_type() != PdfPageObjectType::Image {
            continue;
        }
        if let Ok(q) = obj.bounds() {
            let (l, r) = (q.left().value as f64, q.right().value as f64);
            let (b, t) = (q.bottom().value as f64, q.top().value as f64);
            // 페이지 밖으로 삐져나간 부분은 세지 않는다 — 비율이 1 을 넘지 않게.
            covered += (r.min(w) - l.max(0.0)).max(0.0) * (t.min(h) - b.max(0.0)).max(0.0);
        }
    }
    covered / (w * h) >= SCAN_PDF_IMAGE_COVER
}

//------------------------------------------------------------------
// 스캔 PDF 인가 — 처리제외(EXCLUDED) 판정 (파이썬 판 cli._scan_pdf_excluded 와 같은 식)
//=> 텍스트 레이어 없이 그림만 든 PDF 는 '못 읽은 문서(실패)'가 아니라 '범위 밖이라
//   처리하지 않는 문서'다. 실패로 세면 스캔본이 섞인 배치마다 종료코드 1 이 나서,
//   진짜 깨진 문서가 그 틈에 묻힌다.
//    1) 스캔 페이지가 읽은 페이지의 SCAN_PDF_PAGE_RATIO 이상이고
//    2) 최종 본문도 페이지당 SCAN_PDF_PAGE_MAX_CHARS 자 미만이어야 한다
//       (그림 페이지가 대부분이어도 몇 쪽에 본문이 있으면 읽을 것이 있는 문서다 —
//        실측 '지능형지식관리플랫폼' 12쪽 중 11쪽 그림, 1쪽 565자)
//
// -in: pages = 읽은 페이지 수 · scan_pages = 스캔 페이지 수 · body_chars = 최종 본문 글자 수
// -out: bool = 스캔 PDF 면 true
// -out: error = 없음(한 쪽도 못 읽었으면 false — 추출 실패 쪽에서 다룬다)
//------------------------------------------------------------------
pub fn scan_pdf_verdict(pages: usize, scan_pages: usize, body_chars: usize) -> bool {
    if pages == 0 {
        return false;
    }
    let n = pages as f64;
    (scan_pages as f64) / n >= SCAN_PDF_PAGE_RATIO
        && (body_chars as f64) / n < SCAN_PDF_PAGE_MAX_CHARS as f64
}

#[cfg(test)]
mod scan_pdf_tests {
    use super::*;

    //------------------------------------------------------------------
    // 그림만 든 문서는 처리제외다
    //=> 실측 스캔본의 모양 — 모든 쪽이 그림, 글자 0자.
    //------------------------------------------------------------------
    #[test]
    fn 그림만_든_문서는_처리제외() {
        assert!(scan_pdf_verdict(5, 5, 0));
        // 머리말 스탬프 몇 글자가 든 스캔본도 같다(페이지당 10자 미만).
        assert!(scan_pdf_verdict(4, 4, 30));
    }

    //------------------------------------------------------------------
    // 몇 쪽에 본문이 있으면 처리제외가 아니다
    //=> 12쪽 중 11쪽이 그림이어도 1쪽 565자면 읽을 것이 있다(실측 문서의 모양).
    //------------------------------------------------------------------
    #[test]
    fn 본문이_있으면_처리제외가_아니다() {
        assert!(!scan_pdf_verdict(12, 11, 565));
    }

    //------------------------------------------------------------------
    // 그림 페이지가 절반 미만이면 처리제외가 아니다
    //=> 글자가 없어도 그림이 아니면 '스캔본'이 아니라 '못 읽은 문서'다(본문없음 쪽).
    //------------------------------------------------------------------
    #[test]
    fn 그림이_적으면_처리제외가_아니다() {
        assert!(!scan_pdf_verdict(10, 4, 0));
        assert!(!scan_pdf_verdict(2, 0, 0));
    }

    //------------------------------------------------------------------
    // 경계값 — 절반 '이상'이면 스캔 PDF, 페이지당 10자 '미만'이어야 한다
    //=> 파이썬 판과 비교 방향(>=, <)이 같아야 두 판의 판정이 갈리지 않는다.
    //------------------------------------------------------------------
    #[test]
    fn 경계값() {
        assert!(scan_pdf_verdict(2, 1, 0));
        assert!(scan_pdf_verdict(2, 2, 19));
        assert!(!scan_pdf_verdict(2, 2, 20));
    }

    //------------------------------------------------------------------
    // 한 쪽도 못 읽었으면 판정하지 않는다
    //=> 0 으로 나누지 않고, 추출 실패 쪽에서 다루게 둔다.
    //------------------------------------------------------------------
    #[test]
    fn 페이지가_없으면_처리제외가_아니다() {
        assert!(!scan_pdf_verdict(0, 0, 0));
    }
}

/// 바이트 → 문자열 디코드: UTF-8(BOM) → cp949(EUC-KR) → latin-1.
fn decode_bytes(bytes: &[u8]) -> String {
    // UTF-8 BOM
    let b = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { &bytes[3..] } else { bytes };
    if let Ok(s) = std::str::from_utf8(b) {
        return s.to_string();
    }
    // cp949 / EUC-KR
    let (cow, _enc, had_err) = encoding_rs::EUC_KR.decode(b);
    if !had_err {
        return cow.into_owned();
    }
    // latin-1 (무손실 폴백)
    b.iter().map(|&c| c as char).collect()
}

/// 평문 파일 읽기.
fn plaintext(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(decode_bytes(&bytes))
}

// 버릴 구역(script·style·head)은 태그마다 따로 지운다.
//
// [왜 하나로 묶으면 안 되나] 예전에는
//   `<(script|style|head)[^>]*>.*?</\s*(script|style|head)\s*>`
// 한 개로 처리했는데 결함이 둘 겹쳐 있었다.
//   ① `head` 가 `<header>` 에도 걸린다 — `head` 뒤에 경계가 없어 `[^>]*` 가 "er ..." 를 삼킨다.
//   ② 여는 태그와 닫는 태그가 서로 짝일 필요가 없다 — `<head…>` 로 열고 `</script>` 로 닫아도 성립한다.
// 그래서 본문이 `<header>` 로 시작하고 문서 끝에 `<script>` 가 있는 흔한 페이지에서,
// `<header>` 부터 마지막 `</script>` 까지가 통째로 지워졌다(실측: 301,654자 중 301,603자가
// 날아가 최종 텍스트 0자 — PII 2,707건이 든 보고서 하나가 통째로 미검출됐다).
//
// Rust regex 는 역참조(\1)가 없어 "여는 것과 같은 이름으로 닫기"를 한 정규식으로 못 쓴다.
// 그래서 태그별로 하나씩 두고 차례로 지운다. `\b` 로 `<header>`·`<heading>` 을 배제한다.
static RE_DROP_BLOCKS: Lazy<[Regex; 4]> = Lazy::new(|| [
    Regex::new(r"(?is)<script\b[^>]*>.*?</\s*script\s*>").unwrap(),
    Regex::new(r"(?is)<style\b[^>]*>.*?</\s*style\s*>").unwrap(),
    Regex::new(r"(?is)<head\b[^>]*>.*?</\s*head\s*>").unwrap(),
    // noscript 안은 스크립트가 꺼졌을 때만 보이는 대체 문구라 본문이 아니다
    // (파이썬 판 _DROP 과 같은 목록으로 맞춘다).
    Regex::new(r"(?is)<noscript\b[^>]*>.*?</\s*noscript\s*>").unwrap(),
]);
static RE_TAG: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
static RE_WS: Lazy<Regex> = Lazy::new(|| Regex::new(r"[ \t\x0b\x0c\r]+").unwrap());
static RE_CDATA: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<!\[CDATA\[(.*?)\]\]>").unwrap());

/// XML 요소 안쪽 → 순수 텍스트(태그 제거 · 엔티티 복원 · CDATA 보존).
///
/// [CDATA 를 따로 다루는 이유] `<t><![CDATA[이름]]></t>` 처럼 값이 CDATA 로 감싸여
/// 오는 문서가 있다(엑셀의 inlineStr 이 대표적). 그런데 태그 제거 정규식 `<[^>]+>`
/// 은 `<![CDATA[이름]]>` 을 통째로 '태그 하나'로 보고 지워 버린다 — `<` 부터 처음
/// 만나는 `>`(=`]]>` 의 `>`)까지가 한 덩어리로 잡히기 때문이다. 그러면 셀 값이 전부
/// 사라져 **문서 전체가 빈 텍스트**가 되고, 아무 신호도 못 잡는다(실측: 이메일 787건·
/// 전화 647건이 든 주소록이 '검출 없음'으로 나왔다).
/// 그래서 CDATA 구간을 먼저 떼어 내 있는 그대로 살리고, 나머지 구간만 태그 제거·
/// 엔티티 복원을 한다(CDATA 안의 `&amp;` 는 원문 그대로가 규약이라 복원하지 않는다).
fn xml_inner_text(inner: &str) -> String {
    let mut out = String::new();
    let mut last = 0usize;
    for c in RE_CDATA.captures_iter(inner) {
        let whole = c.get(0).unwrap();
        let before = &inner[last..whole.start()];
        out.push_str(&unescape_entities(&RE_TAG.replace_all(before, "")));
        out.push_str(c.get(1).unwrap().as_str());
        last = whole.end();
    }
    out.push_str(&unescape_entities(&RE_TAG.replace_all(&inner[last..], "")));
    out
}

/// 경계에 개행을 넣을 블록 태그(파이썬 판 `html_text._BLOCK` 과 같은 목록).
///
/// [td·th 가 왜 들어 있나] 표를 `tr` 로만 끊으면 **같은 행의 셀 값이 구분자 없이
/// 이어붙는다**:
///
/// ```text
/// <td>900101-1234568</td><td>010-1234-5678</td>  ->  900101-1234568010-1234-5678
/// ```
///
/// 그러면 검출기의 경계 검사(앞뒤가 숫자면 거부)에 걸려 **두 값이 함께 사라진다**.
/// 파이썬 판은 2026-09-04 에 td·th 를 넣었는데(실측: 표가 빼곡한 문서에서 검출이
/// 37% 적었다) Rust 판이 그 수정을 못 받아 두 판의 PII 건수가 조용히 갈렸다
/// (2026-09-10 실측: ip_address 7↔5 · biz_reg_no 8↔6 · phone_mobile 7↔8).
/// 등급이 아직 안 갈렸을 뿐, 임계값 근처에서는 같은 문서가 판마다 다른 등급을
/// 받게 되는 자리였다.
///
/// [매번 컴파일하지 않는 이유] 예전에는 이 정규식을 html() 안에서 만들어 HTML
/// 파일 하나당 한 번씩 새로 컴파일했다. 목록은 고정이라 한 번만 만들면 된다.
//
// [이름 뒤에 경계를 두는 이유]  2026-09-11
// 예전에는 이름 다음이 바로 `[^>]*` 라, `<pre>` 가 `p` + `re` 로 매칭됐다.
// 파이썬 판은 태그 **이름 집합**으로 판정하므로(`tag in _BLOCK`) `pre` 는
// 블록이 아니다. 그 차이로 Rust 만 `<pre>` 자리에 개행을 넣었고, 앞의 들여쓰기
// 공백이 혼자 남은 줄이 되어 clean_text 의 줄끝 정리에 지워졌다 —
// 같은 문서에서 두 판의 본문이 달라지고 pii 좌표가 어긋났다.
// 전수 확인해 보니 pre·param·picture·progress·track·thead·link 가 걸렸다.
//
// `(?:[\s/][^>]*)?` 로 "이름 뒤는 `>` 이거나 공백이거나 `/`" 를 강제한다.
// `/` 를 받는 것은 `<br/>` 때문이다 — 파이썬 HTMLParser 도 그것을 블록으로 본다.
static RE_BLOCK_TAG: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?is)</?(?:p|div|br|tr|td|th|li|table|h[1-6]|ul|ol|section|article|header|footer|hr)(?:[\s/][^>]*)?>"
).unwrap());

/// HTML → 텍스트(태그 제거 + 엔티티 복원).
fn html(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let raw = decode_bytes(&bytes);
    // script → style → head 순서로 하나씩 지운다(head 안의 style 은 어느 쪽이 먼저든 사라진다).
    let mut no_head = raw.clone();
    for re in RE_DROP_BLOCKS.iter() {
        no_head = re.replace_all(&no_head, " ").into_owned();
    }
    // 블록 태그를 개행으로 바꿔 낱말이 붙지 않게(목록은 RE_BLOCK_TAG 주석 참고).
    let spaced = RE_BLOCK_TAG.replace_all(&no_head, "\n");
    // 태그 제거·엔티티 복원은 CDATA 를 살리는 공통 헬퍼로 (xhtml 에도 CDATA 가 온다).
    let text = xml_inner_text(&spaced);
    Some(RE_WS.replace_all(&text, " ").to_string())
}

// 필드코드에서 링크 주소만 꺼내는 정규식. 따옴표가 없는 표기도 받는다.
static RE_FIELD_LINK: Lazy<Regex> = Lazy::new(|| Regex::new(
    r#"(?i)HYPERLINK\s+(?:"([^"]*)"|(\S+))"#
).unwrap());

//------------------------------------------------------------------
// 필드코드(w:instrText)에서 '링크 주소'만 골라 담는다
//=> 워드의 필드코드에는 두 종류가 섞여 있다. 하나는 사람이 넣은 **내용**
//   (HYPERLINK 의 주소 — http://… 나 mailto:someone@example.com), 다른 하나는
//   **서식 부스러기**(PAGEREF·SEQ·TOC·STYLEREF 같은 쪽번호/목차 장치)다.
//
//   [왜 통째로 안 넣나] 실측(docx 56개)에서 필드코드 조각 809개를 종류별로 세어 보니
//     PAGEREF 584 · SEQ 61 · TOC 17 · STYLEREF 12 … 그리고 HYPERLINK 는 9개뿐이었다.
//   통째로 넣으면 800줄 남짓한 부스러기를 본문에 섞어 놓고 링크 9개를 얻는 셈이다.
//   게다가 `PAGEREF _Toc123456789 \h` 같은 조각은 **긴 숫자열**이라, 자릿수가 맞으면
//   사업자등록번호(10자리) 같은 검출기에 헛걸릴 여지까지 생긴다. 이득보다 위험이 크다.
//
//   [한 문단 안에서 이어 붙이는 이유] 워드는 필드코드를 아무 자리에서나 여러
//   조각으로 쪼개 저장한다("HYPERLINK \"http://exa" + "mple.com\""). 조각을 따로
//   보면 주소가 잘려 못 찾으므로, 한 문단(w:p) 안의 조각을 먼저 이어 붙인 뒤 찾는다.
//
// -in: xml = 문서 XML(본문·머리말·꼬리말 어느 것이든)
// -in: out = 찾은 주소를 한 줄씩 덧붙일 곳
//
// -out: 없음 (out 에 덧붙인다)
// -out: error = 예외 없음
//------------------------------------------------------------------
fn collect_field_links(xml: &str, out: &mut String) {
    let p_re = tag_re("w:p");
    let instr_re = tag_re("w:instrText");
    for p in p_re.captures_iter(xml) {
        // 한 문단 안의 조각을 먼저 이어 붙인다(위 헤더의 '쪼개 저장' 참고).
        let mut joined = String::new();
        for m in instr_re.captures_iter(&p[1]) {
            joined.push_str(&xml_inner_text(&m[1]));
        }
        if joined.is_empty() { continue; }
        for c in RE_FIELD_LINK.captures_iter(&joined) {
            let target = c.get(1).or_else(|| c.get(2)).map(|m| m.as_str().trim()).unwrap_or("");
            if !target.is_empty() {
                out.push_str(target);
                out.push('\n');
            }
        }
    }
}

//------------------------------------------------------------------
// docx → 텍스트 (본문 + 머리말·꼬리말)
//=> 예전에는 word/document.xml 만 읽었다. 그런데 회사 주소·대표전화 같은 값은
//   본문이 아니라 **머리말·꼬리말**에 박혀 있는 일이 흔하다(실측: docx 56개 중
//   30개가 머리말·꼬리말을 갖고 있었고, 거기서 ADDRESS 1건·PHONE 2건이 더 나왔다).
//   그 자리를 안 읽으면 문서에 분명히 적힌 개인정보를 놓친다.
//
//   [순서가 중요하다] 머리말을 **뒤에** 붙인다. 앞에 붙이면 '앞 400자(표제부)' 를
//   보는 신호들(제목·업무분류)이 가리키는 범위가 통째로 달라져, 이번 변경과
//   상관없는 문서까지 분류가 흔들린다. 본문을 그대로 두고 뒤에 잇는 쪽이 안전하다.
//
//   [각주·미주는 왜 안 넣나] 45개 파일에 있었지만 추가로 잡히는 PII 가 0건이었다.
//   읽는 만큼 오탐 여지만 늘어 일부러 뺐다. 필요해지면 아래 목록에 이름만 더하면 된다.
//
// -in: path = docx 경로
//
// -out: 본문 다음에 머리말·꼬리말을 이어 붙인 텍스트
// -out: error = zip 이 아니거나 못 열면 None
//------------------------------------------------------------------
fn docx_text(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;

    // 머리말·꼬리말은 header1.xml, footer2.xml 처럼 번호가 붙어 이름이 고정이 아니다.
    // 그래서 목록을 먼저 훑어 모은다(zip 을 빌린 채로는 by_name 을 못 쓴다).
    let mut extras: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        if let Ok(zf) = zip.by_index(i) {
            let n = zf.name().to_string();
            if n.ends_with(".xml")
                && (n.starts_with("word/header") || n.starts_with("word/footer")) {
                extras.push(n);
            }
        }
    }
    extras.sort();      // header1 → header2 → footer1 … 실행마다 같은 차례가 되게

    let mut out = String::new();
    let mut links = String::new();      // 필드코드의 링크는 맨 뒤에 모아 붙인다
    // 본문이 먼저 — 앞 400자(표제부)를 예전과 똑같이 유지한다.
    if let Ok(mut f) = zip.by_name("word/document.xml") {
        let mut data = String::new();
        if f.read_to_string(&mut data).is_ok() {
            collect_docx_paragraphs(&data, &mut out);
            collect_field_links(&data, &mut links);
        }
    }
    for name in &extras {
        if let Ok(mut f) = zip.by_name(name) {
            let mut data = String::new();
            if f.read_to_string(&mut data).is_ok() {
                collect_docx_paragraphs(&data, &mut out);
                collect_field_links(&data, &mut links);
            }
        }
    }
    // 링크는 본문 흐름에 끼워 넣지 않고 뒤에 붙인다 — 본문 낱말 사이에 주소가
    // 끼어들면 '앞뒤가 붙어 있어야 성립하는' 문맥 규칙이 엉뚱하게 성립/실패한다.
    out.push_str(&links);
    Some(out)
}

// (ooxml_para 제거) — '정해진 이름의 항목들에서 문단을 뽑는' 도우미였다.
// 유일한 사용처가 docx 였는데, 머리말·꼬리말(header1.xml 처럼 번호가 붙어 이름이
// 고정이 아니다)까지 읽게 되면서 docx_text 가 그 일을 직접 하게 됐다.

// (ooxml_prefix_para · ooxml_prefix_tag 제거) — 각각 pptx·hwpx 만 쓰던 한 줄짜리
// 껍데기였는데, 두 포맷 모두 자기 사정에 맞는 수집기를 직접 부르게 되면서 호출처가
// 사라졌다. 쓰는 데 없는 함수는 "이 길로도 갈 수 있나" 하는 착각만 남긴다.

//------------------------------------------------------------------
// pptx → 텍스트 (슬라이드 본문 + 발표자 노트)
//=> 예전에는 `ppt/slides/slide*.xml` 만 읽어 **발표자 노트를 통째로 놓쳤다.**
//   노트는 발표자만 보는 자리라 오히려 단가·설계 상세·내부 사정이 적히는 일이 잦고,
//   실측에서 본문 812자를 정확히 뽑고도 노트 2,000자를 놓쳐 사이냅 대비 재현율이
//   24.9% 까지 떨어진 문서가 있었다. 분류 관점에서는 놓치면 안 되는 쪽이다.
//    1) 슬라이드를 번호순으로 읽어 문단 단위로 뽑는다(예전과 같다)
//    2) 이어서 `ppt/notesSlides/notesSlide*.xml` 을 같은 방식으로 뽑아 뒤에 붙인다
//       — 노트도 슬라이드와 같은 `a:p`/`a:t` 구조라 같은 수집기를 그대로 쓴다
//
//   [왜 뒤에 붙이나] 슬라이드 N 과 노트 N 을 번갈아 끼우는 편이 문맥상 자연스럽지만,
//   둘의 짝은 파일 번호가 아니라 rels 로 맺어져 있어 번호만 보고 맞추면 어긋날 수 있다.
//   잘못 끼워 문맥을 왜곡하느니, 순서대로 뒤에 붙여 '있는 글자를 다 본다'를 택했다.
//
// -in: path = pptx 경로
// -in: run  = 이번 추출의 상한·표식 자리(슬라이드 루프의 시간 상한)
//
// -out: 슬라이드 본문 다음에 노트가 이어진 텍스트
// -out: error = zip 이 아니거나 못 열면 None (노트가 없으면 본문만)
//------------------------------------------------------------------
fn pptx_text(path: &Path, run: &Run) -> Option<String> {
    let mut out = for_prefix(path, "ppt/slides/slide",
                             |d, o| collect_paragraphs(d, "a:p", "a:t", o), run)?;
    // 노트는 없는 문서가 더 많다 — 없으면 조용히 본문만 돌려준다.
    if let Some(notes) = for_prefix(path, "ppt/notesSlides/notesSlide",
                                    |d, o| collect_paragraphs(d, "a:p", "a:t", o), run) {
        if !notes.trim().is_empty() {
            if !out.is_empty() && !out.ends_with('\n') { out.push('\n'); }
            out.push_str(&notes);
        }
    }
    Some(out)
}

//------------------------------------------------------------------
// zip 안 접두 매칭 항목을 번호순으로 읽어 콜백에 넘기기(공통)
//=> 슬라이드·섹션 XML 을 번호순으로 읽는다. 시간 상한(G5)을 넘으면 그때까지 읽은
//   항목까지만 쓰고 run 에 부분추출 표식을 적는다(실패 아님).
//
// -in: path   = zip 계열 문서 경로
// -in: prefix = 읽을 항목 이름 접두("ppt/slides/slide" 등)
// -in: f      = 항목 XML 하나를 받아 본문에 붙이는 수집기
// -in: run    = 이번 추출의 상한·표식 자리
//
// -out: Option<String> = 이어 붙인 텍스트
// -out: error = zip 이 아니거나 못 열면 None
//------------------------------------------------------------------
/// 접두 매칭 항목을 번호순으로 읽어 콜백에 넘긴다(공통).
fn for_prefix<F: Fn(&str, &mut String)>(path: &Path, prefix: &str, f: F, run: &Run) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut names: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        if let Ok(zf) = zip.by_index(i) {
            let n = zf.name().to_string();
            if n.starts_with(prefix) && n.ends_with(".xml") {
                names.push(n);
            }
        }
    }
    names.sort_by_key(|n| num_in_name(n));
    let mut out = String::new();
    // 시간 상한(G5) — 슬라이드·섹션이 수천 개인 문서에서 이 루프가 무한정 늘어난다.
    // 넘으면 그때까지 읽은 항목까지만 쓴다(실패 아님).
    let deadline = ParserDeadline::new(run.limits.parser_timeout);
    let n_total = names.len();
    for (n_done, name) in names.into_iter().enumerate() {
        if deadline.expired() {
            log::warn!(target: crate::ERRLOG_TARGET, "[OOXML 부분추출] {} — 시간 상한에서 중단(prefix={})",
                       path.display(), prefix);
            run.set_partial("시간 상한".to_string(), "parts", n_done, n_total);
            break;
        }
        if let Ok(mut zf) = zip.by_name(&name) {
            let mut data = String::new();
            if zf.read_to_string(&mut data).is_ok() {
                f(&data, &mut out);
            }
        }
    }
    Some(out)
}

/// docx 전용 문단 수집 — 파이썬 `DocxExtractor._collect_paragraphs` 와 짝이다.
///
/// 공용 `collect_paragraphs` 와 두 가지가 다르고, 그 둘이 실측에서 실제로 갈렸다:
///
///   · **빈 문단을 버리지 않는다.** 공용판은 `if !line.is_empty()` 로 건너뛰는데,
///     파이썬 docx 는 빈 문단도 한 줄로 남긴다. 그래서 문단 사이 빈 줄이 Rust 에서만
///     사라졌다(실측 d:/sample 에서 10건). 빈 줄은 장식이 아니다 — 표제부(앞 400자)가
///     닿는 범위와 임베딩 토큰 배열이 함께 밀린다.
///     pptx 는 반대다. 파이썬 `PptxExtractor._read_slide` 가 "빈 문단은 건너뜀"이라
///     공용판이 맞다 — 그래서 이 함수를 docx 전용으로 따로 둔다.
///
///   · **`<w:tab>` 을 탭 문자로 살린다.** 공용판은 `<w:t>` 만 모아 들여쓰기와 표
///     열 구분이 통째로 사라졌다. 파이썬은 `p.iter()` 로 `<w:t>`·`<w:tab>` 을
///     **문서 차례 그대로** 훑으므로, 여기서도 두 정규식의 매치를 위치순으로 섞는다.
///
/// -in: xml = word/document.xml · header*.xml · footer*.xml 본문
/// -in: out = 결과를 덧붙일 문자열(제자리에서 늘어난다)
///
/// -out: 없음(out 에 문단마다 한 줄씩 덧붙인다)
/// -out: error = 없음
fn collect_docx_paragraphs(xml: &str, out: &mut String) {
    // 관심 있는 자리만 한 번에 훑는다. 자기닫힘 갈래를 **앞에** 둔다 — 뒤에 두면
    // 여는 태그 쪽이 끝의 '/' 를 속성으로 삼켜 다음 닫는 태그까지를 통째로 먹는다.
    // `<w:tab\b` 의 \b 는 `<w:tabs>`(탭 정지 위치 '정의' 묶음)를 걸러 준다.
    static TOK: Lazy<Regex> = Lazy::new(|| Regex::new(concat!(
        r"(?s)<w:p(?:\s[^>]*?)?/>|<w:p(?:\s[^>]*)?>|</w:p>",
        r"|<w:r(?:\s[^>]*?)?/>|<w:r(?:\s[^>]*)?>|</w:r>",
        r"|<w:t(?:\s[^>]*)?>(.*?)</w:t>",
        r"|<w:tab\b[^>]*>")).unwrap());

    // 문단마다 한 줄. 문단이 '열린 차례'로 담아 두었다가 마지막에 그 차례로 내보낸다
    // — 안쪽 문단이 먼저 닫힌다고 해서 바깥 문단보다 앞서 나오면 안 된다.
    let mut lines: Vec<String> = Vec::new();
    // 지금 열려 있는 문단들: (줄 번호, 그 문단이 열릴 때의 런 깊이).
    let mut open: Vec<(usize, usize)> = Vec::new();
    // 지금 몇 겹의 런(<w:r>) 안에 있는가.
    let mut run_depth: usize = 0;

    for cap in TOK.captures_iter(xml) {
        let whole = cap.get(0).map_or("", |m| m.as_str());
        if let Some(t) = cap.get(1) {
            // <w:t> 글자 — 지금 문단이 '제 런' 안에 있을 때만 담는다.
            if let Some(&(idx, base)) = open.last() {
                if run_depth > base {
                    lines[idx].push_str(&xml_inner_text(t.as_str()));
                }
            }
            continue;
        }
        if whole.starts_with("<w:tab") {
            // 진짜 탭 문자는 런 안의 <w:tab/> 다. 문단 속성(<w:pPr><w:tabs>) 안의
            // 것은 탭 정지 '위치 정의'라 본문이 아니다 — 런 깊이로 가린다.
            if let Some(&(idx, base)) = open.last() {
                if run_depth > base {
                    lines[idx].push('\t');
                }
            }
            continue;
        }
        if whole.starts_with("</w:p>") {
            open.pop();
            continue;
        }
        if whole.starts_with("<w:p") {
            // 자기닫힘 <w:p/> 는 빈 문단이다 — 빈 줄 한 개를 남기고 끝낸다.
            // (빈 문단을 지우면 실측 '15.의료비 지원 규정' 에서 빈 줄 8개가 사라졌다.)
            if whole.ends_with("/>") {
                lines.push(String::new());
            } else {
                lines.push(String::new());
                open.push((lines.len() - 1, run_depth));
            }
            continue;
        }
        if whole.starts_with("</w:r>") {
            run_depth = run_depth.saturating_sub(1);
        } else if whole.starts_with("<w:r") && !whole.ends_with("/>") {
            run_depth += 1;
        }
    }

    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
}

//------------------------------------------------------------------
// HWPX 문단 수집 — 한 문단을 한 줄로
//=> 예전에는 `<hp:t>` **태그마다 한 줄**로 끊었다. 그런데 HWPX 는 글자 서식이 바뀌는
//   자리에서 `<hp:t>` 를 갈라 놓기 때문에, 한 낱말이 두 태그에 걸치면 그 자리에서
//   줄이 끊긴다 — 실측에서 사이냅이 "가치를·개발에·보장한다"로 뽑은 것을 이 판은
//   "가치 / 를 / 개발 / 에" 로 내놨다(재현율 96.2%·정밀도 95.4%).
//   낱말이 쪼개지면 **붙어 있어야 성립하는 검출**(전화번호·계좌번호·앵커+값)이
//   통째로 어긋난다. DOCX·PPTX 는 이미 문단 단위로 이어 붙이고 있었고, HWPX 만
//   그 보호를 못 받고 있었다.
//
//   [왜 공용 collect_paragraphs 를 못 쓰나] HWPX 는 **문단이 문단 안에 들어간다**.
//   표가 `<hp:p>` 안의 `<hp:tbl>` → `<hp:tc>` → `<hp:subList>` → 또 `<hp:p>` 로
//   내려가기 때문이다(실측 10개 파일에 중첩 14,413곳). 짝 맞추기 정규식은 바깥
//   여는 태그와 **안쪽** 닫는 태그를 짝지어 버려 문단 경계가 엉킨다.
//   그래서 여기서는 짝을 맞추지 않고 **한 번만 훑으면서** 처리한다:
//    1) `<hp:t>` 글자는 '지금 줄'에 계속 이어 붙인다
//    2) 문단이 열리거나 닫히는 자리에서 지금 줄을 끊어 낸다
//       → 안쪽 문단(표 칸)은 제 줄을 갖고, 바깥 문단의 글자도 순서대로 남는다
//    3) `<hp:lineBreak/>` 는 줄바꿈, `<hp:tab/>` 은 탭으로 살린다
//       — 없애면 두 줄이 한 낱말로 붙어 없던 말이 생긴다
//
// -in: xml = Contents/section*.xml 본문
// -in: out = 결과를 덧붙일 문자열(제자리에서 늘어난다)
//
// -out: 없음(out 에 문단마다 한 줄씩 덧붙인다)
// -out: error = 없음
//------------------------------------------------------------------
fn collect_hwpx_paragraphs(xml: &str, out: &mut String) {
    // 관심 있는 자리만 한 줄로 훑는다. 자기닫힘 `<hp:p/>` 갈래를 **앞에** 둔다 —
    // 뒤에 두면 여는 태그 쪽이 끝의 '/' 를 속성으로 삼켜 버린다(docx 에서 겪은 함정).
    static TOK: Lazy<Regex> = Lazy::new(|| Regex::new(concat!(
        r"(?s)<hp:p(?:\s[^>]*?)?/>|<hp:p(?:\s[^>]*)?>|</hp:p>",
        r"|<hp:t(?:\s[^>]*)?>(.*?)</hp:t>",
        r"|<hp:lineBreak\b[^>]*>|<hp:tab\b[^>]*>")).unwrap());
    let mut line = String::new();
    // 지금까지 모은 줄을 끊어 낸다(빈 줄은 버린다 — HWPX 는 빈 문단을 여백으로 쓴다).
    let flush = |line: &mut String, out: &mut String| {
        if !line.trim().is_empty() {
            out.push_str(line.trim_end());
            out.push('\n');
        }
        line.clear();
    };
    for cap in TOK.captures_iter(xml) {
        let whole = cap.get(0).map_or("", |m| m.as_str());
        if let Some(t) = cap.get(1) {
            // <hp:t> 안의 글자 — 낱말이 끊기지 않게 '이어' 붙인다.
            line.push_str(&xml_inner_text(t.as_str()));
        } else if whole.starts_with("<hp:lineBreak") {
            line.push('\n');
        } else if whole.starts_with("<hp:tab") {
            line.push('\t');
        } else {
            // 문단이 열리거나 닫히는 자리 — 여기가 줄 경계다.
            flush(&mut line, out);
        }
    }
    flush(&mut line, out);
}

/// 문단 인식: 각 <para>...</para> 안의 <text> 런을 '이어붙여' 한 줄, 문단 사이 개행.
fn collect_paragraphs(xml: &str, para_tag: &str, text_tag: &str, out: &mut String) {
    let para_re = tag_re(para_tag);
    let text_re = tag_re(text_tag);
    for pcap in para_re.captures_iter(xml) {
        let mut line = String::new();
        for tcap in text_re.captures_iter(&pcap[1]) {
            line.push_str(&xml_inner_text(&tcap[1]));
        }
        if !line.is_empty() {
            out.push_str(&line);
            out.push('\n');
        }
    }
}

/// xlsx: 공유문자열표 + 워크시트 인라인 문자열(inlineStr)의 <t> 를 모은다(분류엔 충분).
/// ※ sharedStrings 없이 시트에 인라인 문자열만 쓰는 파일도 있어 워크시트도 함께 훑는다.
//------------------------------------------------------------------
// 이름공간 접두를 뗀 태그 이름
//=> <x:row> 도 <row> 로 보게 한다. 정규식 판이 접두를 허용했으므로 같게 맞춘다.
//
// -in: name = 이벤트가 준 태그 이름
//
// -out: ':' 뒤 부분(접두가 없으면 그대로)
// -out: error = 없음
//------------------------------------------------------------------
fn xl_local(name: &str) -> &str {
    match name.rfind(':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

//------------------------------------------------------------------
// XML 이벤트의 글자를 본문 문자열로 바꾸기
//=> 일반 텍스트는 엔티티를 풀고, CDATA 는 이미 날것이라 그대로 쓴다
//   (정규식 판 xml_inner_text 와 같은 규칙이다).
//
//   [왜 quick-xml 의 unescape 를 안 쓰나] 그쪽이 풀어 주는 것은 XML 기본 엔티티
//   다섯 개뿐이다. 엑셀 파일에는 &#12345; 같은 숫자 참조와 이름 있는 엔티티도
//   온다. 이 프로젝트의 unescape_entities 가 그 둘을 모두 다루므로, 정규식 판과
//   **같은 글자**가 나오도록 원문 그대로 받아 그 함수에 넘긴다.
//
// -in: raw   = 이벤트가 담고 있는 원문 글자
// -in: cdata = CDATA 이면 true
//
// -out: 본문에 넣을 문자열
// -out: error = 없음(항상 문자열을 돌려준다)
//------------------------------------------------------------------
fn xl_event_text(raw: &str, cdata: bool) -> String {
    if cdata { raw.to_string() } else { unescape_entities(raw) }
}

//------------------------------------------------------------------
// 엔티티 참조 한 개를 원래 글자로
//=> quick-xml 은 &lt; · &#12345; 같은 참조를 **글자와 따로** 알려 준다.
//   이걸 받지 않고 흘려보내면 그 글자가 조용히 사라진다 — 실측에서
//   "4TB &lt; 전체" 가 "4TB 전체" 로 나와 부등호가 통째로 빠졌다.
//
//   받은 것은 '&' 와 ';' 를 뺀 알맹이(예: "lt", "#12345")다. 정규식 판과 똑같은
//   해석기를 태우려고 원래 모양으로 되돌려 unescape_entities 에 넘긴다.
//
// -in: name = 참조의 알맹이("lt" · "#x41" 등)
//
// -out: 풀어낸 글자(모르는 이름이면 "&이름;" 그대로 — 지우면 본문이 사라진다)
// -out: error = 없음
//------------------------------------------------------------------
fn xl_entity(name: &str) -> String {
    unescape_entities(&format!("&{};", name))
}

//------------------------------------------------------------------
// 시작 태그의 속성 하나 읽기
//=> t="s" 처럼 칸의 종류를 알려 주는 속성을 꺼낸다.
//
// -in: e    = 시작 태그 이벤트
// -in: want = 찾을 속성 이름(접두는 떼고 견준다)
//
// -out: 속성값(없으면 빈 문자열)
// -out: error = 없음(읽다 실패한 속성은 건너뛴다)
//------------------------------------------------------------------
fn xl_attr(e: &quick_xml::events::BytesStart, want: &str) -> String {
    for a in e.attributes().flatten() {
        if xl_local(a.key.as_ref()) == want {
            return a.value.as_ref().to_string();
        }
    }
    String::new()
}

//------------------------------------------------------------------
// 공유문자열표를 흘려 읽는다 (SAX)
//=> 정규식 판 xlsx_shared_strings 와 같은 결과를 내되, 28MB 짜리 표를 통째로
//   메모리에 올리지 않고 압축을 푸는 족족 처리하고 지나간다.
//    1) <si> 를 만나면 새 항목을 시작한다(빈 <si/> 도 한 자리를 차지한다)
//    2) <si> 안의 <t> 글자를 모두 이어 붙인다 — 서식이 섞이면 <r><t> 로 쪼개져 온다
//
//   [주의] <rPh>(일본어 읽기 힌트) 안의 <t> 도 함께 담는다. 정규식 판이 그렇게
//   하고 있었으므로, 결과를 바꾸지 않으려고 일부러 맞춘다.
//
// -in: r = sharedStrings.xml 을 읽는 입력
//
// -out: 번호 순서대로 담긴 문자열 목록
// -out: error = 모양이 깨지면 그때까지 읽은 만큼 돌려준다(예외 없음)
//------------------------------------------------------------------
fn xlsx_shared_strings_stream<R: std::io::BufRead>(r: R) -> Vec<String> {
    let mut rd = quick_xml::Reader::from_reader(r);
    rd.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_si = false;
    let mut in_t = 0usize;
    loop {
        match rd.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => match xl_local(e.name().as_ref()) {
                "si" => { in_si = true; cur.clear(); in_t = 0; }
                "t" if in_si => in_t += 1,
                _ => {}
            },
            // <si/> 는 '빈 문자열 한 칸'이다 — 세지 않으면 뒤 번호가 전부 밀려
            // 시트의 모든 문자열 칸이 엉뚱한 글자를 가리키게 된다.
            Ok(quick_xml::events::Event::Empty(e))
                if xl_local(e.name().as_ref()) == "si" => out.push(String::new()),
            Ok(quick_xml::events::Event::End(e)) => match xl_local(e.name().as_ref()) {
                "si" if in_si => { in_si = false; out.push(std::mem::take(&mut cur)); }
                "t" if in_t > 0 => in_t -= 1,
                _ => {}
            },
            Ok(quick_xml::events::Event::Text(e)) if in_t > 0 =>
                cur.push_str(&xl_event_text(e.as_ref(), false)),
            Ok(quick_xml::events::Event::CData(e)) if in_t > 0 =>
                cur.push_str(&xl_event_text(e.as_ref(), true)),
            Ok(quick_xml::events::Event::GeneralRef(e)) if in_t > 0 =>
                cur.push_str(&xl_entity(e.as_ref())),
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            // 깨진 XML 이라도 그때까지 읽은 것은 쓸모가 있다 — 버리지 않는다.
            Err(_) => break,
        }
        buf.clear();
    }
    out
}

//------------------------------------------------------------------
// 서식표(styles.xml)를 읽어 '칸 서식 번호 → 날짜 종류' 표를 만든다
//=> 엑셀은 날짜를 숫자로 저장하고 "날짜처럼 보여라"는 서식을 따로 붙인다.
//   셀의 s 속성은 이 표의 **몇 번째 칸 서식**인지를 가리킨다. 그래서 두 곳을 읽는다.
//    1) <numFmts> — 사용자가 만든 서식(164 번 이상)의 번호 → 서식 문자열
//    2) <cellXfs> — 칸 서식이 **순서대로** 들어 있고, 각자 numFmtId 를 가리킨다
//
//   [왜 cellStyleXfs 는 세면 안 되나] 같은 <xf> 이름이 <cellStyleXfs>(이름 있는
//   스타일의 원본)에도 있다. 그것까지 함께 세면 번호가 밀려 엉뚱한 칸에 날짜
//   서식이 붙는다 — 멀쩡한 숫자가 날짜로 둔갑한다. <cellXfs> 안의 것만 센다.
//
// -in: r = styles.xml 을 읽는 입력
//
// -out: 칸 서식 번호(s 속성) 순서대로의 날짜 종류 목록
// -out: error = 없음(모양이 낯설면 빈 목록 → 아무 칸도 날짜로 보지 않는다)
//------------------------------------------------------------------
fn xlsx_style_date_kinds<R: std::io::BufRead>(r: R) -> Vec<crate::xlsdate::DateKind> {
    use crate::xlsdate::{builtin_kind, code_kind, DateKind};
    let mut rd = quick_xml::Reader::from_reader(r);
    rd.config_mut().trim_text(false);
    let mut buf = Vec::new();
    // 사용자 지정 서식: 번호 → 종류.
    let mut custom: std::collections::HashMap<u16, DateKind> = std::collections::HashMap::new();
    let mut out: Vec<DateKind> = Vec::new();
    // <cellXfs> 안에 있을 때만 <xf> 를 센다.
    let mut in_cell_xfs = false;
    // <numFmt> 는 <cellXfs> 보다 앞에 오지만, 순서를 믿지 않고 둘 다 모은 뒤 맞춘다.
    let mut raw_ids: Vec<u16> = Vec::new();

    loop {
        let ev = rd.read_event_into(&mut buf);
        match ev {
            Ok(quick_xml::events::Event::Start(ref e))
            | Ok(quick_xml::events::Event::Empty(ref e)) => {
                match xl_local(e.name().as_ref()) {
                    "numFmt" => {
                        let id = xl_attr(e, "numFmtId").parse::<u16>().unwrap_or(u16::MAX);
                        let code = xl_attr(e, "formatCode");
                        if id != u16::MAX { custom.insert(id, code_kind(&code)); }
                    }
                    "cellXfs" => in_cell_xfs = true,
                    "xf" if in_cell_xfs => {
                        raw_ids.push(xl_attr(e, "numFmtId").parse::<u16>().unwrap_or(0));
                    }
                    _ => {}
                }
                // 자기닫힘 <cellXfs/> 면 바로 닫힌 것으로 본다.
                if matches!(ev, Ok(quick_xml::events::Event::Empty(_)))
                    && xl_local(e.name().as_ref()) == "cellXfs" {
                    in_cell_xfs = false;
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                if xl_local(e.name().as_ref()) == "cellXfs" { in_cell_xfs = false; }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buf.clear();
    }

    for id in raw_ids {
        // 사용자 지정이 있으면 그것이 이긴다 — 내장 번호를 덮어쓴 파일이 있다.
        out.push(match custom.get(&id) {
            Some(k) => *k,
            None => builtin_kind(id),
        });
    }
    out
}

//------------------------------------------------------------------
// 워크시트 한 장을 흘려 읽어 행 단위 텍스트로 (SAX)
//=> 정규식 판과 같은 규칙으로 칸을 읽어, 한 행의 칸들을 공백으로 이어 한 줄로
//   만든다. 셀 종류(t 속성)별 처리는 xlsx() 머리말의 [셀 종류별 처리] 와 같다.
//    1) <c> 를 만나면 t 속성을 기억하고 그 칸의 <t>·<v> 글자를 모은다
//    2) </c> 에서 종류에 따라 값을 정하고, 비어 있지 않으면 칸 목록에 넣는다
//    3) </row> 에서 칸들을 공백으로 이어 한 줄로 내보낸다
//
// -in: r      = 시트 XML 을 읽는 입력
// -in: shared = 공유문자열표(t="s" 인 칸이 번호로 가리킨다)
// -in: styles = 칸 서식 번호별 날짜 종류(빈 목록이면 날짜 해석을 하지 않는다)
// -in: out    = 결과를 이어 붙일 곳
//
// -out: 없음(out 에 행을 덧붙인다)
// -out: error = 깨진 XML 은 그때까지 읽은 행까지만 남기고 멈춘다(예외 없음)
//------------------------------------------------------------------
fn xlsx_sheet_stream<R: std::io::BufRead>(r: R, shared: &[String],
                                         styles: &[crate::xlsdate::DateKind],
                                         out: &mut String) {
    let mut rd = quick_xml::Reader::from_reader(r);
    rd.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut cells: Vec<String> = Vec::new();
    let mut in_row = false;
    // 지금 읽고 있는 칸의 상태 — 종류(t 속성), <t> 글자, 첫 <v> 글자.
    let mut kind = String::new();
    let mut style: Option<usize> = None;
    let mut c_t = String::new();
    let mut c_v = String::new();
    let mut in_c = false;
    let mut in_t = 0usize;
    let mut in_v = 0usize;
    // <v> 는 첫 것만 쓴다 — 정규식 판의 captures(첫 매치)와 같게 맞춘다.
    let mut v_done = false;

    loop {
        match rd.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => match xl_local(e.name().as_ref()) {
                "row" => { in_row = true; cells.clear(); }
                "c" => {
                    in_c = true; c_t.clear(); c_v.clear(); v_done = false;
                    in_t = 0; in_v = 0;
                    kind = xl_attr(&e, "t");
                    // s 는 '몇 번째 칸 서식인가'다 — 날짜 여부가 여기서 갈린다.
                    style = xl_attr(&e, "s").parse::<usize>().ok();
                }
                "t" if in_c => in_t += 1,
                "v" if in_c && !v_done => in_v += 1,
                _ => {}
            },
            Ok(quick_xml::events::Event::End(e)) => match xl_local(e.name().as_ref()) {
                "row" if in_row => {
                    in_row = false;
                    if !cells.is_empty() {
                        out.push_str(&cells.join(" "));
                        out.push('\n');
                    }
                }
                "c" if in_c => {
                    in_c = false;
                    let val = xlsx_cell_value(&kind, &c_t, &c_v, shared);
                    // 숫자 칸이고 서식이 날짜라면 사람이 보는 글자로 바꾼다.
                    let val = xlsx_as_date(&kind, &val, style, styles).unwrap_or(val);
                    let val = val.trim();
                    if !val.is_empty() { cells.push(val.to_string()); }
                }
                "t" if in_t > 0 => in_t -= 1,
                "v" if in_v > 0 => { in_v -= 1; if in_v == 0 { v_done = true; } }
                _ => {}
            },
            Ok(quick_xml::events::Event::Text(e)) => {
                if in_t > 0 { c_t.push_str(&xl_event_text(e.as_ref(), false)); }
                else if in_v > 0 { c_v.push_str(&xl_event_text(e.as_ref(), false)); }
            }
            Ok(quick_xml::events::Event::CData(e)) => {
                if in_t > 0 { c_t.push_str(&xl_event_text(e.as_ref(), true)); }
                else if in_v > 0 { c_v.push_str(&xl_event_text(e.as_ref(), true)); }
            }
            Ok(quick_xml::events::Event::GeneralRef(e)) => {
                if in_t > 0 { c_t.push_str(&xl_entity(e.as_ref())); }
                else if in_v > 0 { c_v.push_str(&xl_entity(e.as_ref())); }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buf.clear();
    }
}

//------------------------------------------------------------------
// 숫자 칸을 날짜 글자로 (서식이 날짜일 때만)
//=> 엑셀은 2012-12-31 을 41274 로 저장한다. 서식표에서 이 칸이 날짜 서식임을
//   확인했을 때만 바꾼다 — 서식을 안 보고 바꾸면 멀쩡한 수량·금액이 날짜가 된다.
//
// -in: kind   = 셀의 t 속성(숫자 칸이어야 한다)
// -in: val    = 지금까지 정해진 칸 값(일련번호 문자열)
// -in: style  = 셀의 s 속성(칸 서식 번호)
// -in: styles = 칸 서식별 날짜 종류
//
// -out: 바꿀 수 있으면 날짜 글자, 아니면 None(원래 값을 그대로 쓰라는 뜻)
// -out: error = 없음
//------------------------------------------------------------------
fn xlsx_as_date(kind: &str, val: &str, style: Option<usize>,
                styles: &[crate::xlsdate::DateKind]) -> Option<String> {
    // 글자 칸(t="s"/"str"/"inlineStr")·오류·논리값은 날짜가 아니다. 서식이 날짜여도
    // 건드리면 안 된다 — 문자열 "41274" 가 날짜로 둔갑한다.
    if !(kind.is_empty() || kind == "n") {
        return None;
    }
    let k = *styles.get(style?)?;
    crate::xlsdate::serial_to_string(val.trim().parse::<f64>().ok()?, k)
}

//------------------------------------------------------------------
// 칸 하나를 값으로 확정
//=> t 속성이 값의 종류를 말해 준다. 규칙은 xlsx() 머리말의 [셀 종류별 처리] 와 같다.
//
// -in: kind   = t 속성값("s"/"inlineStr"/"e"/"n"/"str"/"b" 또는 빈 문자열)
// -in: c_t    = 그 칸에서 모은 <t> 글자
// -in: c_v    = 그 칸의 첫 <v> 글자
// -in: shared = 공유문자열표
//
// -out: 칸의 본문 값(버릴 칸이면 빈 문자열)
// -out: error = 없음(번호가 표 밖이면 빈 문자열)
//------------------------------------------------------------------
fn xlsx_cell_value(kind: &str, c_t: &str, c_v: &str, shared: &[String]) -> String {
    match kind {
        // 수식 오류(#N/A 등)는 본문이 아니다.
        "e" => String::new(),
        // t="s" 면 <v> 는 글자가 아니라 공유문자열표의 **번호**다.
        "s" => c_v.trim().parse::<usize>().ok()
            .and_then(|i| shared.get(i).cloned())
            .unwrap_or_default(),
        // inlineStr 은 <is><t>, 그 밖은 <v>. 둘 다 있으면 글자 쪽을 쓴다.
        _ => if c_t.is_empty() { c_v.to_string() } else { c_t.to_string() },
    }
}

//------------------------------------------------------------------
// xlsx → 텍스트 (셀 값을 행 단위로 이어 붙인다)
//=> 예전에는 `<t>`(문자열 셀)만 모아 개행으로 이었다. 그래서 두 가지를 잃었다.
//    ① **숫자 셀을 통째로 못 읽었다.** 엑셀은 숫자·날짜·수식 결과를 `<t>` 가 아니라
//       `<v>` 에 넣는다. 하이픈 없이 적은 전화번호·계좌번호·사업자번호가 바로 그
//       숫자 셀이라, 검출기 눈에 아예 안 보였다(실측: 한 파일에서 18,531자 중
//       5,445자만 읽어 29%).
//    ② **한 행의 칸들이 개행으로 갈라졌다.** 엑셀에서 옆으로 나란한 칸은 사람 눈에
//       한 줄이다. 개행으로 끊으면 "앵커 낱말 + 숫자" 처럼 **붙어 있어야 성립하는
//       모양**이 만들어지지 않아, 주소·계좌 같은 문맥 규칙이 통째로 놓친다.
//   그래서 행(`<row>`)마다 칸(`<c>`)을 공백으로 이어 한 줄로 만든다.
//
//   [셀 종류별 처리] `t` 속성이 값의 종류를 말해 준다.
//     · t="s"        → `<v>` 는 **번호**다. 공유문자열표에서 글자를 꺼내 쓴다.
//                       (번호를 그대로 쓰면 뜻 없는 숫자가 본문에 섞인다)
//     · t="inlineStr"→ 셀 안에 `<is><t>` 로 글자가 직접 들어 있다.
//     · t="e"        → 수식 오류(#N/A 등). 본문이 아니라 버린다.
//     · 그 밖(t 없음·"n"·"str"·"b") → `<v>` 를 글자 그대로 쓴다. 숫자와 수식 결과
//                       문자열이 여기 해당한다.
//
// -in: path = xlsx 경로
// -in: run  = 이번 추출의 상한·표식 자리(시트 루프의 시간 상한)
//
// -out: 행 단위로 이어 붙인 텍스트
// -out: error = zip 이 아니거나 못 열면 None
//------------------------------------------------------------------
fn xlsx(path: &Path, run: &Run) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;

    // 워크시트 이름을 모은다(번호순으로 읽어야 sheet2 가 sheet10 보다 앞선다).
    let mut sheets: Vec<String> = Vec::new();
    let mut has_shared = false;
    for i in 0..zip.len() {
        if let Ok(zf) = zip.by_index(i) {
            let n = zf.name().to_string();
            if n == "xl/sharedStrings.xml" {
                has_shared = true;
            } else if n.starts_with("xl/worksheets/") && n.ends_with(".xml") {
                sheets.push(n);
            }
        }
    }
    sheets.sort_by_key(|n| num_in_name(n));

    // 공유문자열표 먼저(워크시트가 번호로 이걸 가리킨다).
    let mut shared: Vec<String> = Vec::new();
    if has_shared {
        if let Ok(f) = zip.by_name("xl/sharedStrings.xml") {
            // 압축을 푸는 족족 읽는다 — 28MB 짜리 표를 통째로 문자열에 올리지 않는다.
            shared = xlsx_shared_strings_stream(std::io::BufReader::new(f));
        }
    }

    // 날짜 서식표 — 없으면 빈 목록이고, 그러면 날짜 해석을 하지 않는다(예전 동작).
    let styles: Vec<crate::xlsdate::DateKind> = match zip.by_name("xl/styles.xml") {
        Ok(f) => xlsx_style_date_kinds(std::io::BufReader::new(f)),
        Err(_) => Vec::new(),
    };

    let mut out = String::new();
    // 시간 상한(G5) — 시트가 많거나 한 장이 거대한 통합문서에서 이 루프가 무한정
    // 늘어난다. 넘으면 그때까지 읽은 시트까지만 쓴다(실패 아님).
    let deadline = ParserDeadline::new(run.limits.parser_timeout);
    let n_sheets = sheets.len();
    for (n_sheets_done, name) in sheets.iter().enumerate() {
        if deadline.expired() {
            log::warn!(target: crate::ERRLOG_TARGET, "[xlsx 부분추출] {} — 시간 상한에서 중단",
                       path.display());
            run.set_partial("시간 상한".to_string(), "sheets",
                            n_sheets_done, n_sheets);
            break;
        }
        match zip.by_name(name) {
            // 시트도 흘려 읽는다 — 33MB 짜리 한 장을 메모리에 올리고 정규식을
            // 다시 돌리던 것이 큰 엑셀에서 비용의 대부분이었다.
            Ok(f) => xlsx_sheet_stream(std::io::BufReader::new(f), &shared,
                                       &styles, &mut out),
            Err(_) => continue,
        }
    }

    // 셀 메모(주석)는 셀 값이 아니라 따로 담긴다 — 시트를 다 읽은 뒤 이어 붙인다.
    let notes = xlsx_comments(&mut zip);
    if !notes.is_empty() {
        if !out.is_empty() && !out.ends_with('\n') { out.push('\n'); }
        out.push_str(&notes);
    }

    // 워크시트에서 한 줄도 못 얻었으면(모양이 낯선 파일) 공유문자열표라도 내놓는다.
    // 예전 동작의 안전망 — 아무것도 안 주는 것보다는 낫다.
    if out.is_empty() && !shared.is_empty() {
        out = shared.join("\n");
    }
    Some(out)
}

//------------------------------------------------------------------
// xlsx 셀 메모(주석) 모으기
//=> 엑셀의 '메모'는 셀 값이 아니라 `xl/comments*.xml` 에 따로 들어간다. 그래서
//   시트만 읽으면 통째로 사라졌다 — 실측에서 셀 값은 완벽히 뽑고도 작성 안내·
//   주의사항이 메모에만 있어 재현율 48.7% 가 된 신청서가 있었다. 신청서·양식류는
//   '어떻게 적어라'가 전부 메모에 있는 일이 흔하다.
//    1) 옛 방식 메모: `<comments><commentList><comment><text><r><t>글자`
//    2) 새 방식(스레드 댓글): `xl/threadedComments/threadedComment*.xml` 의 `<text>`
//    3) 메모 하나를 한 줄로 만든다 — 누가 썼는지(authors)는 본문이 아니라 뺀다
//
// -in: zip = 이미 열려 있는 xlsx 압축(여기서 항목을 더 읽는다)
//
// -out: 메모마다 한 줄씩 담긴 문자열(없으면 빈 문자열)
// -out: error = 항목을 못 읽어도 예외 없이 그만큼만 돌려준다
//------------------------------------------------------------------
fn xlsx_comments(zip: &mut zip::ZipArchive<fs::File>) -> String {
    // 이름을 먼저 모은다 — by_name 으로 읽는 동안에는 목록을 훑을 수 없다.
    let mut names: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        if let Ok(zf) = zip.by_index(i) {
            let n = zf.name().to_string();
            let old = n.starts_with("xl/comments") && n.ends_with(".xml");
            let new = n.starts_with("xl/threadedComments/") && n.ends_with(".xml");
            if old || new { names.push(n); }
        }
    }
    names.sort_by_key(|n| num_in_name(n));

    let mut out = String::new();
    for name in &names {
        let mut data = String::new();
        match zip.by_name(name) {
            Ok(mut f) => { if f.read_to_string(&mut data).is_err() { continue; } }
            Err(_) => continue,
        }
        out.push_str(&xlsx_comment_lines(&data, name.starts_with("xl/threadedComments/")));
    }
    out
}

//------------------------------------------------------------------
// 메모 XML 한 장에서 글자만 뽑기
//=> 압축을 다루는 부분과 떼어 놓아 시험하기 쉽게 만든 알맹이다.
//    1) 옛 방식이면 `<comment>` 안의 `<t>` 들을 이어 붙인다(서식 때문에 쪼개져 온다)
//    2) 새 방식이면 `<threadedComment>` 안의 `<text>` 를 그대로 쓴다
//
// -in: xml = comments*.xml 또는 threadedComment*.xml 본문
// -in: threaded = true 면 새 방식(스레드 댓글)으로 읽는다
//
// -out: 메모마다 한 줄씩 담긴 문자열
// -out: error = 모양이 낯설면 빈 문자열(예외 없음)
//------------------------------------------------------------------
fn xlsx_comment_lines(xml: &str, threaded: bool) -> String {
    let re = if threaded { tag_re("threadedComment") } else { tag_re("comment") };
    let inner_re = if threaded { tag_re("text") } else { tag_re("t") };
    let mut out = String::new();
    for cap in re.captures_iter(xml) {
        let mut line = String::new();
        for t in inner_re.captures_iter(&cap[1]) {
            line.push_str(&xml_inner_text(&t[1]));
        }
        // 메모 하나가 한 덩이로 보이게 앞뒤 공백만 다듬는다(안쪽 줄바꿈은 그대로).
        let line = line.trim();
        if !line.is_empty() {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

//------------------------------------------------------------------
// 태그 하나를 찾는 정규식을 만든다 (네임스페이스 접두 허용)
//=> `<t>` 를 찾으랬는데 파일이 `<x:t>` 로 적어 두면 한 글자도 못 뽑는다. 엑셀이
//   아닌 도구(ClosedXML·EPPlus 등)로 저장한 xlsx 가 실제로 그렇게 쓴다
//   (실측: `<x:sst><x:si><x:t>…` 인 파일에서 추출 0자 → 문서 통째로 미분류).
//   그래서 접두 없는 이름으로 부르면 접두가 붙은 것도 함께 받게 한다.
//    1) 부른 이름에 ':' 가 있으면(예: "w:t") 예전 그대로 — 그 이름만 찾는다
//    2) 없으면(예: "t") 앞에 "아무개:" 가 붙은 것도 받는다
//
// -in: tag = 찾을 태그 이름("t", "w:t" 처럼)
//
// -out: 여는 태그~닫는 태그 사이를 1번 그룹으로 잡는 정규식
// -out: error = 정규식이 안 만들어지면 panic(태그 이름은 코드 상수라 실제로 안 난다)
//------------------------------------------------------------------
fn tag_re(tag: &str) -> Regex {
    let t = regex::escape(tag);
    // 접두를 이미 지정해 부른 곳(docx w:t · pptx a:t · hwpx hp:t)의 동작은 건드리지 않는다.
    // 그쪽까지 접두를 풀면 문서 안 다른 이름공간의 같은 이름(예: 그림 속 a:t)까지
    // 딸려 들어와 기존 결과가 바뀐다.
    let px = if tag.contains(':') { "" } else { r"(?:[A-Za-z_][A-Za-z0-9_.\-]*:)?" };
    let pat = format!(r"(?s)<{px}{t}(?:\s[^>]*)?>(.*?)</{px}{t}>", px = px, t = t);
    Regex::new(&pat).unwrap()
}

// (collect_tag_text 제거) — '태그 하나하나를 한 줄로' 뽑던 도우미다. HWPX 가
// 유일한 사용처였는데 문단 단위로 바뀌면서 부르는 데가 없어졌다. 이 함수가 지키던
// CDATA·엔티티 회귀는 아래 시험이 collect_paragraphs 로 그대로 이어 받는다.

/// zip 항목 이름 속 숫자(정렬키). 예: section10.xml > section2.xml.
fn num_in_name(name: &str) -> u32 {
    let digits: String = name.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(0)
}

//------------------------------------------------------------------
// HTML 엔티티 복원
//=> 숫자 엔티티(&#NN; / &#xHH;)와 이름 엔티티(&mdash; 등)를 모두 푼다.
//
//   [이름 엔티티를 표로 두는 이유]  2026-09-11
//   예전에는 손으로 고른 여섯 개만 풀어, `&mdash;`·`&middot;` 가 본문에 글자
//   그대로 남았다(실측: doc/*.html 에 11곳). 파이썬 판은 HTMLParser 라
//   HTML5 이름 엔티티를 전부 푼다 — 그래서 같은 문서의 본문이 두 판에서
//   달라졌다. 표는 기계로 만든다(scripts/gen_html_entities.py).
//
//   [&nbsp; 주의] 파이썬은 U+00A0(줄바꿈 없는 공백)으로 푼다. 예전에는 우리만
//   일반 공백으로 바꿔 또 하나의 차이를 만들고 있었다 — 표를 따르면 맞는다.
//
//   [한 번에 훑는 이유] 예전처럼 replace 를 이어 붙이면 앞 단계가 만든 글자를
//   뒤 단계가 또 푸는 이중 복원이 생긴다(그래서 &amp; 를 맨 뒤로 미뤄야 했다).
//   한 번만 훑으면 그 순서 문제가 아예 없다.
//
// -in: s = 엔티티가 섞인 문자열
//
// -out: 푼 문자열. 모르는 이름은 건드리지 않고 그대로 둔다
// -out: error = 예외 없음
//------------------------------------------------------------------
fn unescape_entities(s: &str) -> String {
    // 엔티티가 없으면 훑지도 않는다(대부분의 조각이 여기서 끝난다).
    if !s.contains('&') {
        return s.to_string();
    }
    static RE_ENT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"&(?:#(x|X)?([0-9A-Fa-f]+)|([A-Za-z][A-Za-z0-9]*));").unwrap()
    });
    RE_ENT.replace_all(s, |c: &regex::Captures| {
        if let Some(num) = c.get(2) {
            let hex = c.get(1).is_some();
            let code = u32::from_str_radix(num.as_str(), if hex { 16 } else { 10 })
                .unwrap_or(0);
            return char::from_u32(code).map(|ch| ch.to_string()).unwrap_or_default();
        }
        let name = c.get(3).map(|m| m.as_str()).unwrap_or("");
        match crate::html_entities::lookup(name) {
            Some(v) => v.to_string(),
            // 모르는 이름은 손대지 않는다 — 지워 버리면 본문이 조용히 사라진다.
            None => c.get(0).map(|m| m.as_str().to_string()).unwrap_or_default(),
        }
    }).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── xlsx 흘려읽기(SAX) ───────────────────────────────────────────
    // 정규식으로 통째로 훑던 것을 흘려읽기로 바꾸면서(2026-09-11) 실제로 겪은
    // 함정들을 못 박아 둔다. 전부 '조용히 글자가 사라지는' 종류라 눈에 안 띈다.

    //------------------------------------------------------------------
    // 시트 한 장을 흘려 읽어 문자열로 (시험 도우미)
    //------------------------------------------------------------------
    fn sheet(xml: &str, shared: &[String]) -> String {
        sheet_st(xml, shared, &[])
    }

    //------------------------------------------------------------------
    // 시트 한 장을 서식표까지 주고 읽기 (시험 도우미)
    //------------------------------------------------------------------
    fn sheet_st(xml: &str, shared: &[String],
                styles: &[crate::xlsdate::DateKind]) -> String {
        let mut out = String::new();
        xlsx_sheet_stream(std::io::BufReader::new(xml.as_bytes()), shared, styles, &mut out);
        out
    }

    //------------------------------------------------------------------
    // 서식표(styles.xml) 읽기 (시험 도우미)
    //------------------------------------------------------------------
    fn styles(xml: &str) -> Vec<crate::xlsdate::DateKind> {
        xlsx_style_date_kinds(std::io::BufReader::new(xml.as_bytes()))
    }

    // 엑셀은 날짜를 숫자로 저장한다. 서식이 날짜라고 말할 때만 글자로 바꾼다.
    #[test]
    fn 날짜서식_숫자칸을_날짜글자로_바꾼다() {
        use crate::xlsdate::DateKind;
        let st = [DateKind::None, DateKind::Date];
        let x = r#"<sheetData><row><c s="1"><v>41274</v></c><c s="0"><v>41274</v></c>
                   </row></sheetData>"#;
        // 같은 값이라도 서식이 날짜인 칸만 바뀐다 — 아니면 수량·금액이 날짜가 된다.
        assert_eq!(sheet_st(x, &[], &st), "2012-12-31 41274
");
    }

    // 글자 칸은 서식이 날짜여도 건드리면 안 된다. 문자열 "41274" 가 날짜로 둔갑한다.
    #[test]
    fn 글자칸은_날짜서식이어도_두다() {
        use crate::xlsdate::DateKind;
        let st = [DateKind::Date];
        let sh = vec!["41274".to_string()];
        let x = r#"<sheetData><row><c t="s" s="0"><v>0</v></c>
                   <c t="inlineStr" s="0"><is><t>41274</t></is></c></row></sheetData>"#;
        assert_eq!(sheet_st(x, &sh, &st), "41274 41274
");
    }

    // <xf> 는 <cellStyleXfs>(이름 있는 스타일의 원본)에도 같은 이름으로 들어 있다.
    // 그것까지 세면 번호가 밀려 엉뚱한 칸에 날짜 서식이 붙는다.
    #[test]
    fn cellStyleXfs_는_세지_않는다() {
        use crate::xlsdate::DateKind;
        let xml = r#"<styleSheet>
          <cellStyleXfs count="2"><xf numFmtId="14"/><xf numFmtId="14"/></cellStyleXfs>
          <cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14"/></cellXfs>
        </styleSheet>"#;
        // 0번은 일반, 1번만 날짜여야 한다(cellStyleXfs 를 셌다면 전부 날짜가 된다).
        assert_eq!(styles(xml), vec![DateKind::None, DateKind::Date]);
    }

    // 사용자 지정 서식(164 이상)은 서식 '문자열'을 보고 판단한다.
    #[test]
    fn 사용자지정_서식번호를_문자열로_판단한다() {
        use crate::xlsdate::DateKind;
        let xml = r##"<styleSheet>
          <numFmts count="2">
            <numFmt numFmtId="176" formatCode="yyyy-mm-dd"/>
            <numFmt numFmtId="177" formatCode="#,##0"/>
          </numFmts>
          <cellXfs count="3"><xf numFmtId="176"/><xf numFmtId="177"/><xf numFmtId="0"/></cellXfs>
        </styleSheet>"##;
        assert_eq!(styles(xml), vec![DateKind::Date, DateKind::None, DateKind::None]);
    }

    // 서식표가 없는 파일(styles.xml 부재)에서는 날짜 해석을 아예 하지 않는다.
    #[test]
    fn 서식표가_없으면_숫자를_그대로_둔다() {
        let x = r#"<sheetData><row><c s="1"><v>41274</v></c></row></sheetData>"#;
        assert_eq!(sheet_st(x, &[], &[]), "41274
");
    }

    // quick-xml 은 `&lt;` 같은 참조를 글자와 **따로** 알려 준다. 이 이벤트를
    // 받지 않으면 부등호·따옴표가 소리 없이 사라진다 — 실측에서 "4TB &lt; 전체"
    // 가 "4TB 전체" 로 나왔다. 숫자 참조(&#65;)도 같은 길로 온다.
    #[test]
    fn 엔티티_참조를_글자로_되살린다() {
        let x = r#"<sheetData><row><c t="inlineStr"><is><t>4TB &lt; 전체</t></is></c>
                   <c t="inlineStr"><is><t>a&amp;b &#65;</t></is></c></row></sheetData>"#;
        assert_eq!(sheet(x, &[]), "4TB < 전체 a&b A\n");
    }

    // `<si/>` 는 '빈 문자열 한 칸'이다. 세지 않고 건너뛰면 그 뒤 번호가 전부
    // 하나씩 밀려, 시트의 모든 문자열 칸이 엉뚱한 글자를 가리키게 된다.
    #[test]
    fn 빈_공유문자열도_한_자리를_차지한다() {
        let xml = r#"<sst><si><t>가</t></si><si/><si><t>나</t></si></sst>"#;
        let got = xlsx_shared_strings_stream(std::io::BufReader::new(xml.as_bytes()));
        assert_eq!(got, vec!["가".to_string(), String::new(), "나".to_string()]);
        // 번호 2 가 "나" 를 가리켜야 한다(밀렸다면 여기서 깨진다).
        let x = r#"<sheetData><row><c t="s"><v>2</v></c></row></sheetData>"#;
        assert_eq!(sheet(x, &got), "나\n");
    }

    // 한 행의 칸은 공백으로 이어 **한 줄**이 돼야 한다. 개행으로 갈라지면
    // "앵커 낱말 + 숫자"처럼 붙어 있어야 성립하는 검출이 통째로 어긋난다.
    #[test]
    fn 한_행은_한_줄로_이어진다() {
        let sh = vec!["계좌번호".to_string()];
        let x = r#"<sheetData><row><c t="s"><v>0</v></c><c><v>123456</v></c></row>
                   <row><c/><c r="B2"/></row></sheetData>"#;
        // 값이 하나도 없는 행은 줄을 만들지 않는다(빈 줄이 쌓이면 본문이 지저분해진다).
        assert_eq!(sheet(x, &sh), "계좌번호 123456\n");
    }

    // t="e" 는 수식 오류(#N/A 등)라 본문이 아니다. 숫자·수식 결과는 <v> 에
    // 들어오므로 반드시 읽어야 한다 — 하이픈 없는 전화번호가 바로 이 자리다.
    #[test]
    fn 셀_종류별로_값을_가려_읽는다() {
        let x = r#"<sheetData><row><c t="e"><v>#N/A</v></c><c t="n"><v>01012345678</v></c>
                   <c t="str"><v>합계</v></c></row></sheetData>"#;
        assert_eq!(sheet(x, &[]), "01012345678 합계\n");
    }

    // 이름공간 접두가 붙은 문서도 같게 읽어야 한다(정규식 판이 허용하던 동작).
    #[test]
    fn 이름공간_접두가_붙어도_읽는다() {
        let x = r#"<x:sheetData><x:row><x:c t="inlineStr"><x:is><x:t>가나</x:t></x:is></x:c>
                   </x:row></x:sheetData>"#;
        assert_eq!(sheet(x, &[]), "가나\n");
    }

    // 태그 이름 뒤 경계가 없으면 `<pre>` 가 `p`+`re` 로 잡힌다 — 파이썬 판은
    // 태그 **이름 집합**으로 보므로 pre 는 블록이 아니다. 그 차이로 두 판의
    // 본문이 갈렸다(2026-09-11). 이름이 접두인 태그 전부를 못 박아 둔다.
    #[test]
    fn 이름이_겹치는_태그를_블록으로_보지_않는다() {
        for t in ["<pre>", "</pre>", "<param a=1>", "<picture>", "<progress>",
                  "<track>", "<thead>", "</thead>", "<link rel=x>"] {
            assert!(!RE_BLOCK_TAG.is_match(t), "{t} 를 블록으로 봤다");
        }
        // 진짜 블록은 그대로 잡혀야 한다(`<br/>` 자기닫힘 포함).
        for t in ["<p>", "</p>", "<p class=a>", "<br>", "<br/>", "<br />",
                  "<td>", "</td>", "<th>", "<h3>", "<hr/>", "<TABLE>"] {
            assert!(RE_BLOCK_TAG.is_match(t), "{t} 를 블록으로 못 봤다");
        }
    }

    // 파이썬 판은 HTMLParser 라 HTML5 이름 엔티티를 전부 푼다. 손으로 고른
    // 몇 개만 풀면 목록 밖 엔티티가 나올 때마다 같은 어긋남이 되풀이된다.
    #[test]
    fn 이름_엔티티를_표대로_푼다() {
        assert_eq!(unescape_entities("대시&mdash;점&middot;줄임&hellip;"),
                   "대시—점·줄임…");
        // &nbsp; 는 일반 공백이 아니라 U+00A0 이다(파이썬과 같아야 한다).
        assert_eq!(unescape_entities("공백&nbsp;붙임"), "공백\u{A0}붙임");
        // 숫자 엔티티(10진·16진)도 푼다.
        assert_eq!(unescape_entities("&#49;&#x32;&#X33;"), "123");
        // 모르는 이름은 지우지 않고 그대로 둔다 — 지우면 본문이 조용히 사라진다.
        assert_eq!(unescape_entities("모름&qqqq;끝"), "모름&qqqq;끝");
        // 한 번만 훑으므로 &amp;lt; 가 `<` 로 두 번 풀리지 않는다.
        assert_eq!(unescape_entities("&amp;lt;"), "&lt;");
    }

    // docx 는 문단이 문단 안에 들어간다 — 텍스트상자(<w:txbxContent>)가 런 안에
    // 있고 그 안에 또 <w:p> 가 있다. 짝 맞추기 정규식은 바깥 여는 태그와 안쪽
    // 닫는 태그를 짝지어, 텍스트상자 **뒤에 이어지는 바깥 문단의 글자**를 통째로
    // 잃었다(실측에서 "1) 개선사항 Summary" 한 줄이 사라졌다).
    #[test]
    fn docx_텍스트상자_뒤의_바깥_문단_글자를_잃지_않는다() {
        let mut out = String::new();
        collect_docx_paragraphs(
            "<w:p><w:r><w:t>앞</w:t></w:r>             <w:r><w:pict><w:txbxContent>             <w:p><w:r><w:t>상자안</w:t></w:r></w:p>             </w:txbxContent></w:pict></w:r>             <w:r><w:t>뒤</w:t></w:r></w:p>", &mut out);
        // 바깥 문단은 제 글자만(앞+뒤), 안쪽 문단은 제 줄. 문단이 열린 차례대로.
        assert_eq!(out, "앞뒤
상자안
");
    }

    // 문단 속성의 <w:pPr><w:tabs><w:tab/> 은 탭 정지 위치 '정의'이지 본문의 탭이
    // 아니다. 안쪽 문단의 정의까지 세면 없던 들여쓰기가 생긴다.
    #[test]
    fn docx_탭_정의는_탭문자가_아니다() {
        let mut out = String::new();
        collect_docx_paragraphs(
            "<w:p><w:pPr><w:tabs><w:tab w:pos=\"1\"/><w:tab w:pos=\"2\"/></w:tabs></w:pPr>             <w:r><w:t>가</w:t><w:tab/><w:t>나</w:t></w:r></w:p>", &mut out);
        assert_eq!(out, "가	나
");
    }

    // 빈 문단 <w:p/> 은 빈 줄을 남긴다 — 지우면 문서의 여백이 사라진다.
    #[test]
    fn docx_빈_문단은_빈_줄로_남는다() {
        let mut out = String::new();
        collect_docx_paragraphs(
            "<w:p><w:r><w:t>가</w:t></w:r></w:p><w:p w:rsidR=\"x\"/>             <w:p><w:r><w:t>나</w:t></w:r></w:p>", &mut out);
        assert_eq!(out, "가

나
");
    }

    // HWPX 는 서식이 바뀌는 자리에서 <hp:t> 를 가른다 — 태그마다 줄을 끊으면
    // 한 낱말이 쪼개진다("가치를" → "가치" / "를"). 문단 단위로 이어야 한다.
    #[test]
    fn hwpx_한_문단은_한_줄로_이어진다() {
        let mut out = String::new();
        collect_hwpx_paragraphs(
            "<hp:p id=\"1\"><hp:run><hp:t>가치</hp:t></hp:run>\
             <hp:run><hp:t>를 보장한다</hp:t></hp:run></hp:p>\
             <hp:p id=\"2\"><hp:run><hp:t>다음 문단</hp:t></hp:run></hp:p>", &mut out);
        assert_eq!(out, "가치를 보장한다\n다음 문단\n");
    }

    // 표는 문단 안에 문단이 들어간다(hp:p > hp:tbl > hp:tc > hp:subList > hp:p).
    //=> 짝 맞추기 정규식이면 바깥 여는 태그와 안쪽 닫는 태그를 짝지어 경계가 엉킨다.
    //   칸마다 제 줄을 갖고, 글자는 문서에 적힌 차례대로 남아야 한다.
    #[test]
    fn hwpx_표_안의_문단도_제_줄을_갖는다() {
        let mut out = String::new();
        collect_hwpx_paragraphs(
            "<hp:p id=\"1\"><hp:run><hp:t>표 앞 글</hp:t></hp:run>\
             <hp:run><hp:tbl><hp:tr>\
             <hp:tc><hp:subList><hp:p><hp:run><hp:t>왼쪽 칸</hp:t></hp:run></hp:p></hp:subList></hp:tc>\
             <hp:tc><hp:subList><hp:p><hp:run><hp:t>오른쪽 칸</hp:t></hp:run></hp:p></hp:subList></hp:tc>\
             </hp:tr></hp:tbl></hp:run></hp:p>", &mut out);
        assert_eq!(out, "표 앞 글\n왼쪽 칸\n오른쪽 칸\n");
    }

    // 문단 안 줄바꿈·탭은 살려야 한다 — 없애면 두 줄이 한 낱말로 붙어
    // 문서에 없던 말("끝다음")이 생긴다.
    #[test]
    fn hwpx_줄바꿈과_탭을_살린다() {
        let mut out = String::new();
        collect_hwpx_paragraphs(
            "<hp:p><hp:run><hp:t>끝</hp:t><hp:lineBreak/><hp:t>다음</hp:t>\
             <hp:tab/><hp:t>칸</hp:t></hp:run></hp:p>", &mut out);
        assert_eq!(out, "끝\n다음\t칸\n");
    }

    // 빈 문단(여백용)은 줄을 만들지 않는다 — HWPX 는 빈 문단을 여백으로 쓴다.
    #[test]
    fn hwpx_빈_문단은_줄을_만들지_않는다() {
        let mut out = String::new();
        collect_hwpx_paragraphs(
            "<hp:p><hp:run><hp:t>앞</hp:t></hp:run></hp:p><hp:p/>\
             <hp:p><hp:run></hp:run></hp:p><hp:p><hp:run><hp:t>뒤</hp:t></hp:run></hp:p>",
            &mut out);
        assert_eq!(out, "앞\n뒤\n");
    }

    // 엑셀 '메모'는 셀 값이 아니라 따로 담긴다 — 그래서 통째로 놓치고 있었다.
    //=> 서식이 섞이면 한 메모가 <r><t> 로 잘게 쪼개져 오므로 이어 붙여야
    //   한 문장이 된다. 작성자 이름(<authors>)은 본문이 아니라 들어오면 안 된다.
    #[test]
    fn 엑셀_셀메모를_한_줄로_모은다() {
        let xml = "<comments><authors><author>고봉수</author></authors><commentList>\
                   <comment ref=\"B2\" authorId=\"0\"><text><r><t>셀 서식을 </t></r>\
                   <r><t>텍스트로</t></r></text></comment>\
                   <comment ref=\"C3\" authorId=\"0\"><text><t>영구면 9999-12-31</t></text>\
                   </comment></commentList></comments>";
        assert_eq!(xlsx_comment_lines(xml, false),
                   "셀 서식을 텍스트로\n영구면 9999-12-31\n");
    }

    // 새 방식(스레드 댓글)은 <text> 에 글자가 통으로 들어 있다.
    #[test]
    fn 엑셀_스레드댓글도_읽는다() {
        let xml = "<threadedComments><threadedComment ref=\"A1\" dT=\"2026-01-01T00:00:00\">\
                   <text>담당자 확인 필요</text></threadedComment></threadedComments>";
        assert_eq!(xlsx_comment_lines(xml, true), "담당자 확인 필요\n");
    }

    // 메모가 없는 파일에서 없던 줄이 생기면 안 된다.
    #[test]
    fn 메모가_없으면_빈_문자열이다() {
        assert_eq!(xlsx_comment_lines("<comments><commentList/></comments>", false), "");
    }

    // 발표자 노트도 슬라이드와 같은 a:p / a:t 구조다 — 같은 수집기로 읽는다.
    //=> 노트를 안 읽던 시절 실측에서 본문 812자만 뽑고 노트 2,000자를 놓쳤다.
    #[test]
    fn 발표자_노트_구조를_문단으로_읽는다() {
        let mut out = String::new();
        collect_paragraphs(
            "<p:notes><p:cSld><p:spTree><p:sp><p:txBody>\
             <a:p><a:r><a:t>단가는 </a:t></a:r><a:r><a:t>대외비</a:t></a:r></a:p>\
             <a:p><a:r><a:t>2차 협의 예정</a:t></a:r></a:p>\
             </p:txBody></p:sp></p:spTree></p:cSld></p:notes>",
            "a:p", "a:t", &mut out);
        assert_eq!(out, "단가는 대외비\n2차 협의 예정\n");
    }

    // 회귀 핵심 — CDATA 로 감싼 값이 통째로 사라지면 안 된다.
    // 태그 제거 정규식이 <![CDATA[..]]> 를 '태그 하나'로 삼켜, 엑셀 inlineStr
    // 문서 전체가 빈 텍스트가 되던 버그를 막는다.
    #[test]
    fn cdata_안의_글자를_살린다() {
        let mut out = String::new();
        collect_paragraphs("<p><t><![CDATA[이름]]></t></p><p><t><![CDATA[회사]]></t></p>",
                           "p", "t", &mut out);
        assert_eq!(out, "이름\n회사\n");
    }

    // CDATA 와 일반 텍스트가 섞여도 둘 다 살아야 한다.
    #[test]
    fn cdata와_일반텍스트가_섞여도_된다() {
        assert_eq!(xml_inner_text("앞<![CDATA[가운데]]>뒤"), "앞가운데뒤");
        // 일반 구간의 태그는 지우고 엔티티는 복원한다.
        assert_eq!(xml_inner_text("<b>A&amp;B</b><![CDATA[C&amp;D]]>"), "A&BC&amp;D");
    }

    // 탭 정지 '정의'(<w:pPr><w:tabs>)를 탭 문자로 세면 안 된다.
    //=> 실측에서 한 문단이 그 정의를 32개 갖고 있었다. 문단 전체를 훑으면
    //   탭 32개가 새어 들어가고, 정제 단계가 그것을 공백 하나로 뭉쳐 본문에
    //   없던 들여쓰기를 만든다. 파이썬 판이 실제로 그렇게 갈려 있었다.
    #[test]
    fn 탭정지_정의는_탭문자가_아니다() {
        let mut out = String::new();
        collect_docx_paragraphs(
            "<w:p><w:pPr><w:tabs><w:tab w:val=\"left\" w:pos=\"806\"/>\
             <w:tab w:val=\"left\" w:pos=\"1612\"/></w:tabs></w:pPr>\
             <w:r><w:t>나.</w:t></w:r><w:r><w:t xml:space=\"preserve\"> </w:t></w:r>\
             <w:r><w:t>개정 시행일</w:t></w:r></w:p>", &mut out);
        assert_eq!(out, "나. 개정 시행일\n");
    }

    // 런 안의 진짜 탭(<w:tab/>)은 문서 차례 그대로 살아야 한다.
    //=> tag_re 는 짝이 있는 태그만 받아 자기닫힘 <w:tab/> 을 못 잡았고,
    //   그래서 표 열 구분과 들여쓰기가 통째로 사라졌다.
    #[test]
    fn 런_안의_탭은_살린다() {
        let mut out = String::new();
        collect_docx_paragraphs(
            "<w:p><w:r><w:t>가</w:t><w:tab/><w:t>나</w:t></w:r></w:p>", &mut out);
        assert_eq!(out, "가\t나\n");
    }

    // 빈 문단은 빈 줄로 남는다 — 파이썬 docx 와 같다(pptx 는 반대다).
    //=> 문단 사이 빈 줄이 사라지면 표제부(앞 400자)가 닿는 범위가 밀린다.
    #[test]
    fn 빈_문단은_빈_줄로_남는다() {
        let mut out = String::new();
        collect_docx_paragraphs(
            "<w:p><w:r><w:t>앞</w:t></w:r></w:p><w:p></w:p>\
             <w:p><w:r><w:t>뒤</w:t></w:r></w:p>", &mut out);
        assert_eq!(out, "앞\n\n뒤\n");
    }

    // 빈 문단은 자기닫힘 `<w:p/>` 로도 온다 — 이쪽이 실제 문서에서 더 흔하다.
    //=> 짝 있는 태그만 받는 정규식이 이것을 통째로 놓쳐, 실측 '15.의료비 지원
    //   규정'에서 빈 줄 8개가 사라졌다.
    #[test]
    fn 자기닫힘_빈_문단도_빈_줄로_남는다() {
        let mut out = String::new();
        collect_docx_paragraphs(
            "<w:p><w:r><w:t>앞</w:t></w:r></w:p><w:p/>\
             <w:p w:rsidR=\"00A1\"/><w:p><w:r><w:t>뒤</w:t></w:r></w:p>", &mut out);
        assert_eq!(out, "앞\n\n\n뒤\n");
    }

    // 정제는 파이썬 clean_text 와 단계·차례가 같아야 한다.
    #[test]
    fn 정제가_파이썬과_같은_단계를_거친다() {
        // 연속 공백/탭 → 공백 1개, 줄끝 공백 제거, 빈 줄 3+ → 2, 양끝 trim.
        assert_eq!(clean_text("  가   나  \n\n\n\n다  \n"), "가 나\n\n다");
        // 페이지 마커 줄과 제어문자는 사라진다.
        assert_eq!(clean_text("앞\n..PAGE:12\n\u{0007}뒤"), "앞\n\n뒤");
        // CR+LF 는 LF 하나로.
        assert_eq!(clean_text("가\r\n나"), "가\n나");
    }

    // CDATA 가 없으면 예전과 똑같이 동작해야 한다(기존 문서 회귀 방지).
    #[test]
    fn cdata가_없으면_종전과_같다() {
        let mut out = String::new();
        collect_paragraphs("<p><t>보고서</t></p><p><t xml:space=\"preserve\"> 초안</t></p>",
                           "p", "t", &mut out);
        assert_eq!(out, "보고서\n 초안\n");
    }

    // 문단 단위 수집(docx)에서도 CDATA 가 살아야 한다.
    #[test]
    fn 문단수집도_cdata를_살린다() {
        let mut out = String::new();
        collect_paragraphs("<w:p><w:t><![CDATA[계약서]]></w:t><w:t>초안</w:t></w:p>",
                           "w:p", "w:t", &mut out);
        assert_eq!(out, "계약서초안\n");
    }

    // 닫히지 않은 CDATA 에 패닉하거나 무한루프에 빠지지 않는다.
    #[test]
    fn 깨진_cdata에도_안전하다() {
        let _ = xml_inner_text("<t><![CDATA[끝나지 않음");
        let _ = xml_inner_text("]]>혼자 있는 종료");
        let _ = xml_inner_text("");
    }
}



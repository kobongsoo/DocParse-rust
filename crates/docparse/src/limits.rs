//! 문서 크기·시간 상한 — 설계: plan/문서크기-상한-설계-20260904.html
//!                        plan/Rust-문서파서-라이브러리분리-설계-20261110.html §4
//!
//! 라이브러리 쪽에는 '판정하는 함수'만 둔다. 상한 값을 어디서 읽을지(CLI 인자·
//! 환경변수)는 쓰는 프로그램이 정해 [`Limits`] 로 넘긴다. 예전에는 이 모듈이
//! 프로그램 전역(OnceLock)과 CSOCLASSIFY_* 환경변수를 직접 읽어, 다른 프로젝트로
//! 가져가면 그 프로그램의 환경변수 이름까지 따라갔다.
//!
//! 적용 게이트:
//!   G1 `max_file_bytes`      — 추출 전 원본 바이트(안전핀)
//!   G2 `max_file_bytes_text` — 텍스트 계열 전용(바이트=글자수라 따로)
//!   G3 글자수 상한           — [`truncate_text`] 만 준다(부르는 것은 쓰는 쪽 정책)
//!   G5 `parser_timeout` / `max_pdf_pages` — 파서 루프의 시간·분량 상한

use std::path::Path;
use std::time::{Duration, Instant};

use crate::detect::Fmt;

// 기본값 상수 — MpowerClassify 파이썬 판 config.py 와 같은 값이어야 한다.
// Limits::default() 와 쓰는 프로그램이 '같은 하나'를 보게 상수로 뽑아 둔다.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
pub const DEFAULT_MAX_FILE_BYTES_TEXT: u64 = 100 * 1024 * 1024;
pub const DEFAULT_PARSER_TIMEOUT_SECS: u64 = 60;
pub const DEFAULT_MAX_PDF_PAGES: usize = 3000;
pub const DEFAULT_SNF_TIMEOUT_SECS: u64 = 60;
pub const DEFAULT_MIN_TEXT_LEN: usize = 20;

//------------------------------------------------------------------
// 추출 상한 묶음
//=> 추출 한 번에 지킬 크기·시간·분량 상한을 한데 모은 값이다. 전역이 아니라
//   추출 함수의 인자로 넘기므로, 한 프로세스에서 서로 다른 상한을 섞어 쓸 수 있다.
//   None 은 '상한 없음'이다(예전의 0 과 같은 뜻).
//
// -필드: parser_timeout      = 파서 1파일 시간 상한(None = 무제한)
// -필드: max_pdf_pages       = PDF 페이지 수 상한(None = 무제한)
// -필드: max_file_bytes      = 원본 파일 바이트 상한(None = 무제한)
// -필드: max_file_bytes_text = 텍스트 계열(txt/csv/json/html) 바이트 상한(None = 무제한)
// -필드: snf_timeout         = 사이냅 snf_exe 1건 제한 시간(0 = 무제한)
// -필드: min_text_len        = 폴백 판정 '짧은 결과' 기준(공백 뺀 글자 수, 기본 20)
//------------------------------------------------------------------
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub parser_timeout: Option<Duration>,
    pub max_pdf_pages: Option<usize>,
    pub max_file_bytes: Option<u64>,
    pub max_file_bytes_text: Option<u64>,
    pub snf_timeout: Duration,
    pub min_text_len: usize,
}

impl Default for Limits {
    //------------------------------------------------------------------
    // 기본 상한 — 분리 전 프로그램의 코드 기본값과 같다
    //=> 값 하나라도 어긋나면 같은 문서에서 부분추출·크기초과 판정이 달라진다.
    //   그래서 위 상수에서 그대로 만들고 시험으로 고정한다.
    //
    // -in: 없음
    //
    // -out: Limits = 기본 상한
    // -out: error = 없음
    //------------------------------------------------------------------
    fn default() -> Self {
        Limits {
            parser_timeout: Some(Duration::from_secs(DEFAULT_PARSER_TIMEOUT_SECS)),
            max_pdf_pages: Some(DEFAULT_MAX_PDF_PAGES),
            max_file_bytes: Some(DEFAULT_MAX_FILE_BYTES),
            max_file_bytes_text: Some(DEFAULT_MAX_FILE_BYTES_TEXT),
            snf_timeout: Duration::from_secs(DEFAULT_SNF_TIMEOUT_SECS),
            min_text_len: DEFAULT_MIN_TEXT_LEN,
        }
    }
}

impl Limits {
    //------------------------------------------------------------------
    // 모든 상한을 끈 값
    //=> 조사·디버깅용(쓰는 프로그램의 --no-size-limit 같은 자리). 사이냅 시간과
    //   짧은 결과 기준은 '상한'이 아니라 판정 기준이라 기본값을 둔다.
    //
    // -in: 없음
    //
    // -out: Limits = 크기·시간·페이지 상한이 모두 None 인 값
    // -out: error = 없음
    //------------------------------------------------------------------
    pub fn unlimited() -> Self {
        Limits {
            parser_timeout: None,
            max_pdf_pages: None,
            max_file_bytes: None,
            max_file_bytes_text: None,
            ..Limits::default()
        }
    }
}

//------------------------------------------------------------------
// 크기 상한에 걸린 사실
//=> '너무 커서 안 읽었다'를 깨진 파일과 구분해 알리기 위한 값이다.
//
// -필드: size      = 원본 파일 바이트
// -필드: limit     = 걸린 상한 바이트
// -필드: text_kind = true 면 텍스트 계열 상한(max_file_bytes_text)에 걸렸다
//------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeLimitHit {
    pub size: u64,
    pub limit: u64,
    pub text_kind: bool,
}

impl SizeLimitHit {
    //------------------------------------------------------------------
    // 사람이 읽는 사유 문장
    //=> 분리 전 check_size_limit 가 돌려주던 문장과 한 글자도 다르지 않아야 한다 —
    //   운영 로그·결과 레코드의 error 사유가 이 문장이다.
    //
    // -in: 없음
    //
    // -out: String = "크기 상한 초과: 120.0MB > 100.0MB (MAX_FILE_BYTES)" 꼴
    // -out: error = 없음
    //------------------------------------------------------------------
    pub fn reason(&self) -> String {
        let which = if self.text_kind { "MAX_FILE_BYTES_TEXT" } else { "MAX_FILE_BYTES" };
        format!("크기 상한 초과: {} > {} ({})", human_bytes(self.size), human_bytes(self.limit), which)
    }
}

//------------------------------------------------------------------
// 원본 파일 크기 상한 검사 (G1·G2)
//=> 파서를 부르기 '전에' 원본 바이트를 보고, 상한을 넘으면 아예 읽지 않는다.
//   파서를 부른 뒤에 막으면 이미 메모리를 다 쓴 뒤라 의미가 없다.
//    1) 크기를 잰다(못 재면 통과 — 상한과 무관한 이유로 정상 문서를 잃으면 안 된다)
//    2) 텍스트 계열이면 텍스트 상한(G2), 아니면 일반 상한(G1)을 고른다
//    3) 넘으면 걸린 사실(크기·상한·종류)을 돌려준다
//
//   [왜 텍스트 계열만 다른가] txt/csv/tsv/json/html 은 바이트가 곧 글자수다.
//   반대로 pdf·pptx 는 이미지가 대부분이라 바이트와 본문 길이가 따로 논다.
//
// -in: path   = 검사할 원본 경로
// -in: fmt    = 감지된 포맷(텍스트 계열 판정에 쓴다)
// -in: limits = 적용할 상한
//
// -out: Option<SizeLimitHit> = 상한 초과면 Some, 통과면 None
// -out: error = 없음(크기를 못 재면 None = 통과)
//------------------------------------------------------------------
pub fn check_size_limit(path: &Path, fmt: Fmt, limits: &Limits) -> Option<SizeLimitHit> {
    // 크기를 못 재는 상황(권한·경쟁 삭제 등)에서 막아 버리면 상한과 무관한 이유로
    // 정상 문서를 잃는다. 그 판단은 뒤의 파서에게 맡긴다.
    let size = std::fs::metadata(path).ok()?.len();

    let (limit, text_kind) = match fmt {
        Fmt::Text | Fmt::Html => (limits.max_file_bytes_text, true),
        _ => (limits.max_file_bytes, false),
    };
    // None(또는 0)은 '상한 없음'이다.
    let limit = match limit {
        Some(n) if n > 0 => n,
        _ => return None,
    };
    if size <= limit {
        return None;
    }
    Some(SizeLimitHit { size, limit, text_kind })
}

//------------------------------------------------------------------
// 바이트 수 → 사람이 읽는 크기 문자열
//=> 상한 초과 사유 문구에 쓴다. MB 로만 찍으면 작은 값이 전부 "0MB" 로 뭉개져
//   "0MB > 0MB 초과" 같은 아무 정보 없는 문장이 된다.
//
// -in: n = 바이트 수
//
// -out: String = "328.8MB" · "512.0KB" · "900B" 처럼 읽히는 문자열
// -out: error = 없음
//------------------------------------------------------------------
pub fn human_bytes(n: u64) -> String {
    let f = n as f64;
    if n >= 1_048_576 {
        format!("{:.1}MB", f / 1_048_576.0)
    } else if n >= 1024 {
        format!("{:.1}KB", f / 1024.0)
    } else {
        format!("{}B", n)
    }
}

//------------------------------------------------------------------
// 글자수 상한 적용 (G3)
//=> 본문이 상한보다 길면 "앞부분만" 남긴다. 문서를 버리는 게 아니라 앞부분만 보고
//   판단하게 하는 것이며, 부른 쪽은 결과에 '일부만 봤다'는 표식을 달아야 한다.
//   라이브러리는 이 함수만 주고 스스로 부르지 않는다 — 본문을 얼마나 볼지는
//   쓰는 쪽(분류) 정책이다.
//
//   [왜 글자 단위인가] 파이썬 판은 `text[:limit]` 로 '글자' 수를 센다. 바이트로
//   자르면 한글 한 글자가 3바이트라 절단 위치가 달라지고, 문자 중간을 잘라 패닉이 난다.
//
// -in: text  = 추출·정규화된 본문
// -in: limit = 남길 최대 글자수(0 이면 제한 없음)
//
// -out: (String, usize, bool) = 잘린(또는 그대로인) 본문, 자르기 전 글자수, 잘렸는지
// -out: error = 없음
//------------------------------------------------------------------
pub fn truncate_text(text: String, limit: usize) -> (String, usize, bool) {
    if limit == 0 {
        // 상한 해제 — 글자수를 셀 이유도 없다(대용량에서 이 계산 자체가 비싸다).
        return (text, 0, false);
    }
    // char 경계 오프셋을 찾는다. limit 번째 char 가 없으면 = 상한 이하 = 그대로 통과.
    let cut = match text.char_indices().nth(limit) {
        None => return (text, 0, false),
        Some((byte_idx, _)) => byte_idx,
    };
    // 여기까지 왔으면 실제로 잘린다 — 이때만 전체 글자수를 센다(한 번만 훑는다).
    let n_orig = text.chars().count();
    let mut t = text;
    t.truncate(cut);
    (t, n_orig, true)
}

//------------------------------------------------------------------
// 파서 시간 예산 (G5)
//=> "이 파일에 쓸 수 있는 시간"을 들고 다니며, 페이지·시트 루프가 한 바퀴 돌 때마다
//   expired() 로 물어보게 한다. 넘기면 루프를 멈추고 그때까지 모은 텍스트만 쓴다 —
//   실패로 처리하지 않는다(부분 결과가 미분류보다 낫다).
//
// -필드: started = 생성 시각
// -필드: secs    = 허용 시간(초). 0 이면 무제한
//------------------------------------------------------------------
pub struct ParserDeadline {
    started: Instant,
    secs: u64,
}

impl ParserDeadline {
    //------------------------------------------------------------------
    // 생성자 — 시계를 지금 시작한다
    //=> 파서가 실제 일을 시작하기 직전에 만들어야 측정이 정확하다.
    //   분리 전과 같은 '초 단위' 비교를 지키려고 초로 바꿔 들고 있는다.
    //
    // -in: timeout = 허용 시간(None 이면 무제한)
    //
    // -out: ParserDeadline
    // -out: error = 없음
    //------------------------------------------------------------------
    pub fn new(timeout: Option<Duration>) -> Self {
        ParserDeadline { started: Instant::now(), secs: timeout.map_or(0, |d| d.as_secs()) }
    }

    //------------------------------------------------------------------
    // 예산을 다 썼나
    //=> 루프 안에서 매 반복 호출한다. 무제한(0) 설정이면 항상 false.
    //
    // -in: 없음
    //
    // -out: bool = true 면 시간 초과(호출부가 루프를 멈춰야 한다)
    // -out: error = 없음
    //------------------------------------------------------------------
    pub fn expired(&self) -> bool {
        if self.secs == 0 {
            return false;
        }
        self.started.elapsed().as_secs() >= self.secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    //------------------------------------------------------------------
    // 상한 미만은 손대지 않는다 (무해성 — 가장 중요)
    //------------------------------------------------------------------
    #[test]
    fn truncate_under_limit_untouched() {
        let (t, n, cut) = truncate_text("가나다라마".to_string(), 100);
        assert_eq!(t, "가나다라마");
        assert!(!cut);
        assert_eq!(n, 0); // 안 잘렸으면 굳이 세지 않는다
    }

    //------------------------------------------------------------------
    // 경계값 — 정확히 상한이면 자르지 않는다
    //------------------------------------------------------------------
    #[test]
    fn truncate_at_boundary_untouched() {
        let (t, _, cut) = truncate_text("가나다".to_string(), 3);
        assert_eq!(t, "가나다");
        assert!(!cut);
    }

    //------------------------------------------------------------------
    // 초과분은 앞부분만 남기고, 한글도 글자 단위로 잘린다
    //------------------------------------------------------------------
    #[test]
    fn truncate_over_limit_by_chars() {
        let (t, n, cut) = truncate_text("머리말본문본문꼬리말".to_string(), 3);
        assert!(cut);
        assert_eq!(t, "머리말");          // 바이트가 아니라 '글자' 3개
        assert_eq!(t.chars().count(), 3);
        assert_eq!(n, 10);
    }

    //------------------------------------------------------------------
    // 상한 0 은 '제한 없음'
    //------------------------------------------------------------------
    #[test]
    fn truncate_disabled() {
        let (t, _, cut) = truncate_text("가나다".to_string(), 0);
        assert_eq!(t, "가나다");
        assert!(!cut);
    }

    //------------------------------------------------------------------
    // 기본 상한 값 고정 — 분리 전 코드 기본값과 하나하나 같아야 한다
    //=> 기본값 하나 차이로 부분추출·크기초과 판정이 조용히 바뀌는 것을 막는다.
    //------------------------------------------------------------------
    #[test]
    fn default_limits_fixed() {
        let l = Limits::default();
        assert_eq!(l.max_file_bytes, Some(100 * 1024 * 1024));
        assert_eq!(l.max_file_bytes_text, Some(100 * 1024 * 1024));
        assert_eq!(l.max_pdf_pages, Some(3000));
        assert_eq!(l.parser_timeout, Some(Duration::from_secs(60)));
        assert_eq!(l.snf_timeout, Duration::from_secs(60));
        assert_eq!(l.min_text_len, 20);
    }

    //------------------------------------------------------------------
    // 바이트 크기 문구 — 작은 값이 "0MB" 로 뭉개지지 않는다
    //------------------------------------------------------------------
    #[test]
    fn human_bytes_readable_at_small_sizes() {
        assert_eq!(human_bytes(900), "900B");
        assert_eq!(human_bytes(10 * 1024), "10.0KB");
        assert!(human_bytes(328 * 1_048_576).ends_with("MB"));
        assert!(!human_bytes(1000).contains("0MB"));
    }

    //------------------------------------------------------------------
    // 크기 상한 사유 문장 — 분리 전 문장과 같은 모양
    //------------------------------------------------------------------
    #[test]
    fn size_limit_reason_text() {
        let h = SizeLimitHit { size: 2 * 1_048_576, limit: 1_048_576, text_kind: false };
        assert_eq!(h.reason(), "크기 상한 초과: 2.0MB > 1.0MB (MAX_FILE_BYTES)");
        let h = SizeLimitHit { size: 2048, limit: 1024, text_kind: true };
        assert_eq!(h.reason(), "크기 상한 초과: 2.0KB > 1.0KB (MAX_FILE_BYTES_TEXT)");
    }

    //------------------------------------------------------------------
    // 크기 검사 — 상한 없음(None)·넘음·안 넘음
    //------------------------------------------------------------------
    #[test]
    fn size_check_on_real_file() {
        let p = std::env::temp_dir().join(format!("docparse_size_{}.txt", std::process::id()));
        std::fs::write(&p, vec![b'a'; 2000]).unwrap();
        let mut l = Limits::default();
        assert!(check_size_limit(&p, Fmt::Text, &l).is_none());
        l.max_file_bytes_text = Some(1000);
        let h = check_size_limit(&p, Fmt::Text, &l).expect("넘어야 한다");
        assert!(h.text_kind && h.size == 2000 && h.limit == 1000);
        // 텍스트가 아닌 포맷은 일반 상한을 본다.
        assert!(check_size_limit(&p, Fmt::Docx, &l).is_none());
        l.max_file_bytes_text = None;
        assert!(check_size_limit(&p, Fmt::Text, &l).is_none());
        let _ = std::fs::remove_file(&p);
    }

    //------------------------------------------------------------------
    // 시간 예산 — 무제한은 지나지 않고, 넉넉하면 아직 안 지났다
    //------------------------------------------------------------------
    #[test]
    fn deadline_states() {
        assert!(!ParserDeadline::new(None).expired());
        assert!(!ParserDeadline::new(Some(Duration::from_secs(60))).expired());
    }
}

//! docparse_dyn — docparse.dll / libdocparse.so 를 실행 중에 불러 쓰는 Rust 감싸개
//!
//! 정본: DocParse-rust/bindings/rust/docparse_dyn.rs — 쓰는 프로젝트는 이 파일을 복사해 둔다.
//! 필요한 의존: libloading = "0.8", serde_json = "1"
//!
//! ```no_run
//! let dp = DocParse::load(std::path::Path::new("docparse.dll")).unwrap();
//! let x = dp.extract(std::path::Path::new("a.hwp"), &serde_json::json!({})).unwrap();
//! println!("{} {}", x.engine, x.text.len());
//! ```
//!
//! 이 파일은 라이브러리 형식에 맞춘 얇은 층만 둔다(JSON ↔ 구조체). 옵션을 어떻게 채울지,
//! 결과를 어떻게 쓸지는 쓰는 프로그램이 정한다.

// 쓰는 프로그램마다 필요한 함수가 달라, 안 쓰는 함수·필드가 있어도 경고하지 않는다.
#![allow(dead_code)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// 이 감싸개가 아는 함수 모양의 버전. 라이브러리의 docparse_abi_version() 과 같아야 한다.
pub const ABI_VERSION: u32 = 1;

/// 로그 콜백 모양 — level 1=error 2=warn 3=info 4=debug 5=trace, site = "모듈:줄"
pub type LogFn = extern "C" fn(level: c_int, target: *const c_char, site: *const c_char,
                               msg: *const c_char, user: *mut c_void);

type FnAbi = unsafe extern "C" fn() -> u32;
type FnVersion = unsafe extern "C" fn() -> *const c_char;
type FnDetect = unsafe extern "C" fn(*const c_char, *mut *const c_char) -> c_int;
type FnExtract = unsafe extern "C" fn(*const c_char, *const c_char, *mut *mut c_char) -> c_int;
type FnCheckSize = unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *mut *mut c_char) -> c_int;
type FnExpand = unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *mut *mut c_char) -> c_int;
type FnHint = unsafe extern "C" fn(*const c_char, *const c_char, *mut *mut c_char) -> c_int;
type FnClean = unsafe extern "C" fn(*const c_char, *mut *mut c_char) -> c_int;
type FnScan = unsafe extern "C" fn(u64, u64, u64) -> c_int;
type FnFree = unsafe extern "C" fn(*mut c_char);
type FnSetLogger = unsafe extern "C" fn(Option<LogFn>, c_int, *mut c_void);

//------------------------------------------------------------------
// 부분 추출 표식 — 상한(시간·쪽수)에서 멈춰 앞부분만 읽었을 때
// -필드: reason = 사람이 읽는 사유
// -필드: unit   = 단위("pages" 등)
// -필드: read   = 읽은 수
// -필드: total  = 전체 수
//------------------------------------------------------------------
#[derive(Debug, Clone, PartialEq)]
pub struct Partial {
    pub reason: String,
    pub unit: String,
    pub read: usize,
    pub total: usize,
}

//------------------------------------------------------------------
// PDF 페이지 구성
// -필드: pages      = 읽은 쪽 수
// -필드: scan_pages = 그림만 있는 쪽 수
//------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub pages: usize,
    pub scan_pages: usize,
}

//------------------------------------------------------------------
// 추출 결과
// -필드: text    = 본문
// -필드: engine  = 실제로 뽑은 엔진 이름("docx"·"pdf"·"snf" …)
// -필드: fmt     = 감지(또는 지정)된 형식 이름
// -필드: partial = 부분 추출 표식
// -필드: layout  = PDF 페이지 구성
//------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct Extracted {
    pub text: String,
    pub engine: String,
    pub fmt: String,
    pub partial: Option<Partial>,
    pub layout: Option<Layout>,
}

//------------------------------------------------------------------
// 추출 실패 — 라이브러리 결과 JSON 의 error.kind 별로 나눈다
// -필드: SizeLimit   = 크기 상한 초과(size·limit 바이트, text_kind = 텍스트류 상한이었나)
// -필드: Unsupported = 지원하지 않는 형식
// -필드: Failed      = 파싱 실패(사유가 빈 문자열일 수 있다 = 자체 파서가 못 읽음)
// -필드: Arg         = 인자 오류(감싸개·옵션 실수)
// -필드: Panic       = 라이브러리 내부 오류(버그)
// 모든 경우 message 는 라이브러리가 만든 사람용 문장 그대로다.
//------------------------------------------------------------------
#[derive(Debug, Clone)]
pub enum ExtractError {
    SizeLimit { size: u64, limit: u64, text_kind: bool, message: String },
    Unsupported { fmt: String, message: String },
    Failed { message: String },
    Arg { message: String },
    Panic { message: String },
}

impl ExtractError {
    //------------------------------------------------------------------
    // 라이브러리가 만든 사람용 문장
    //
    // -in: 없음
    //
    // -out: &str = 실패 문장
    // -out: error = 없음
    //------------------------------------------------------------------
    pub fn message(&self) -> &str {
        match self {
            ExtractError::SizeLimit { message, .. } | ExtractError::Unsupported { message, .. }
            | ExtractError::Failed { message } | ExtractError::Arg { message }
            | ExtractError::Panic { message } => message,
        }
    }

    //------------------------------------------------------------------
    // 결과 JSON 의 error 객체 → ExtractError
    //
    // -in: e = {"kind":…, "message":…, …}
    //
    // -out: ExtractError = 종류별 값
    // -out: error = 없음(모르는 kind 는 Failed 로 본다)
    //------------------------------------------------------------------
    fn from_json(e: &Value) -> Self {
        let message = e.get("message").and_then(Value::as_str).unwrap_or("").to_string();
        match e.get("kind").and_then(Value::as_str).unwrap_or("") {
            "size_limit" => ExtractError::SizeLimit {
                size: e.get("size").and_then(Value::as_u64).unwrap_or(0),
                limit: e.get("limit").and_then(Value::as_u64).unwrap_or(0),
                text_kind: e.get("text_kind").and_then(Value::as_bool).unwrap_or(false),
                message,
            },
            "unsupported" => ExtractError::Unsupported {
                fmt: e.get("fmt").and_then(Value::as_str).unwrap_or("unknown").to_string(),
                message,
            },
            "arg" => ExtractError::Arg { message },
            "panic" => ExtractError::Panic { message },
            _ => ExtractError::Failed { message },
        }
    }
}

impl std::fmt::Display for ExtractError {
    //------------------------------------------------------------------
    // 사람용 문장 그대로 출력
    //
    // -in: f = 출력 대상
    //
    // -out: fmt::Result
    // -out: error = 없음
    //------------------------------------------------------------------
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ExtractError {}

//------------------------------------------------------------------
// 압축 펼치기 결과
// -필드: jobs        = (실제 파일, 표시 라벨, 원래 압축 파일) 목록
// -필드: unexpanded  = 못 펼친 압축 (경로, 사유, 종류 "limit"|"split_volume"|"error")
// -필드: unsupported = 지원하지 않는 압축 (경로, 사유)
// -필드: partial     = 일부만 펼친 압축 (경로, 사유)
//------------------------------------------------------------------
#[derive(Debug, Clone, Default)]
pub struct Expanded {
    pub jobs: Vec<(PathBuf, String, Option<String>)>,
    pub unexpanded: Vec<(String, String, String)>,
    pub unsupported: Vec<(String, String)>,
    pub partial: Vec<(String, String)>,
}

//------------------------------------------------------------------
// 불러온 docparse 라이브러리 하나
//=> 함수 포인터를 미리 다 찾아 둔다. 라이브러리는 이 값이 살아 있는 동안 내려가지 않는다
//   (로그 콜백이 언제든 올 수 있으므로 보통 프로세스가 끝날 때까지 들고 있는다).
// -필드: _lib    = 불러온 라이브러리(내리지 않게 쥔다)
// -필드: path    = 불러온 파일 경로
// -필드: version = docparse_version() 값
//------------------------------------------------------------------
pub struct DocParse {
    _lib: libloading::Library,
    path: PathBuf,
    version: String,
    detect: FnDetect,
    extract: FnExtract,
    check_size: FnCheckSize,
    expand: FnExpand,
    hint: FnHint,
    clean: FnClean,
    scan: FnScan,
    free: FnFree,
    set_logger: FnSetLogger,
}

// 함수 포인터만 들고 있고 라이브러리 쪽에 전역 상태가 없으므로 스레드 간에 나눠 써도 된다.
unsafe impl Send for DocParse {}
unsafe impl Sync for DocParse {}

//------------------------------------------------------------------
// 이 OS 의 라이브러리 파일 이름
//
// -in: 없음
//
// -out: &str = "docparse.dll" | "libdocparse.so" | "libdocparse.dylib"
// -out: error = 없음
//------------------------------------------------------------------
pub fn lib_file_name() -> &'static str {
    if cfg!(windows) { "docparse.dll" } else if cfg!(target_os = "macos") { "libdocparse.dylib" } else { "libdocparse.so" }
}

//------------------------------------------------------------------
// 경로를 C 문자열로 (UTF-8)
//=> 라이브러리는 경로를 UTF-8 로 받는다. UTF-8 로 못 바꾸는 경로는 손실 변환한다
//   (그런 경로는 라이브러리에서 파일 없음으로 끝난다).
//
// -in: p = 경로
//
// -out: CString = UTF-8 경로
// -out: error = 없음(NUL 은 지운다)
//------------------------------------------------------------------
fn c_path(p: &Path) -> CString {
    c_str(&p.to_string_lossy())
}

//------------------------------------------------------------------
// &str → CString (NUL 문자는 지운다)
//
// -in: s = 문자열
//
// -out: CString
// -out: error = 없음
//------------------------------------------------------------------
fn c_str(s: &str) -> CString {
    CString::new(s.replace('\0', "")).expect("NUL 을 지웠다")
}

impl DocParse {
    //------------------------------------------------------------------
    // 라이브러리 파일 하나를 불러 함수들을 찾는다
    //=> 이름 검색(PATH·LD_LIBRARY_PATH)을 쓰지 않고 준 경로 그대로 연다 — 엉뚱한 버전을
    //   집어 오지 않게.
    //    1) 라이브러리 열기
    //    2) docparse_abi_version() == ABI_VERSION 확인(다르면 멈춘다)
    //    3) 나머지 함수 포인터 찾기
    //
    // -in: path = 라이브러리 파일 경로
    //
    // -out: DocParse = 불러온 라이브러리
    // -out: error = 못 열거나 함수가 없거나 ABI 가 다르면 Err(사람용 사유)
    //------------------------------------------------------------------
    pub fn load(path: &Path) -> Result<Self, String> {
        // 안전: docparse 라이브러리는 불러올 때 초기화 코드를 돌리지 않는다.
        let lib = unsafe { libloading::Library::new(path) }
            .map_err(|e| format!("문서파서 라이브러리를 열지 못했습니다: {} ({})", path.display(), e))?;
        // 함수 하나를 이름으로 찾아 포인터로 꺼내는 작은 도우미
        macro_rules! sym {
            ($name:literal, $t:ty) => {{
                let s: libloading::Symbol<$t> = unsafe { lib.get(concat!($name, "\0").as_bytes()) }
                    .map_err(|e| format!("문서파서 라이브러리에 {} 함수가 없습니다: {} ({})", $name, path.display(), e))?;
                *s
            }};
        }
        let abi = sym!("docparse_abi_version", FnAbi);
        let got = unsafe { abi() };
        if got != ABI_VERSION {
            return Err(format!("문서파서 라이브러리의 ABI 버전이 다릅니다: 라이브러리 {} / 감싸개 {} ({})",
                               got, ABI_VERSION, path.display()));
        }
        let ver = sym!("docparse_version", FnVersion);
        let version = unsafe { CStr::from_ptr(ver()) }.to_string_lossy().into_owned();
        let dp = DocParse {
            path: path.to_path_buf(),
            version,
            detect: sym!("docparse_detect", FnDetect),
            extract: sym!("docparse_extract", FnExtract),
            check_size: sym!("docparse_check_size", FnCheckSize),
            expand: sym!("docparse_expand", FnExpand),
            hint: sym!("docparse_split_volume_hint", FnHint),
            clean: sym!("docparse_clean_text", FnClean),
            scan: sym!("docparse_scan_pdf_verdict", FnScan),
            free: sym!("docparse_free", FnFree),
            set_logger: sym!("docparse_set_logger", FnSetLogger),
            _lib: lib,
        };
        Ok(dp)
    }

    //------------------------------------------------------------------
    // 후보 폴더들에서 라이브러리를 찾아 불러온다
    //=> 찾는 순서: explicit(파일 또는 폴더) → 환경변수 env_var → 실행파일 폴더
    //   → 실행파일 폴더의 부모(cargo test 실행파일은 target/<프로파일>/deps 에 있다).
    //
    // -in: explicit = 직접 지정한 파일·폴더(없으면 None)
    // -in: env_var  = 경로를 읽을 환경변수 이름(파일·폴더 모두 허용)
    //
    // -out: DocParse = 불러온 라이브러리
    // -out: error = 못 찾거나 못 열면 Err(찾아본 곳을 모두 적은 사유)
    //------------------------------------------------------------------
    pub fn find_and_load(explicit: Option<&Path>, env_var: &str) -> Result<Self, String> {
        let name = lib_file_name();
        let mut cands: Vec<PathBuf> = Vec::new();
        // 파일이면 그대로, 폴더면 그 안의 라이브러리 이름을 후보로
        let push = |c: &mut Vec<PathBuf>, p: PathBuf| {
            if p.is_dir() { c.push(p.join(name)) } else { c.push(p) }
        };
        if let Some(p) = explicit {
            push(&mut cands, p.to_path_buf());
        }
        if let Some(v) = std::env::var_os(env_var).filter(|v| !v.is_empty()) {
            push(&mut cands, PathBuf::from(v));
        }
        if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
            cands.push(d.join(name));
            if let Some(pd) = d.parent() {
                cands.push(pd.join(name));
            }
        }
        // 직접 지정한 것은 '없으면 실패'다 — 다른 곳의 것으로 조용히 대신하지 않는다.
        if explicit.is_some() {
            return Self::load(&cands[0]);
        }
        for c in &cands {
            if c.is_file() {
                return Self::load(c);
            }
        }
        Err(format!("문서파서 라이브러리({})를 찾을 수 없습니다 — 찾아본 곳: {} (환경변수 {} 로 알려 주세요)",
                    name, cands.iter().map(|c| c.display().to_string()).collect::<Vec<_>>().join(" , "), env_var))
    }

    //------------------------------------------------------------------
    // 불러온 라이브러리 파일 경로
    //
    // -in: 없음
    //
    // -out: &Path
    // -out: error = 없음
    //------------------------------------------------------------------
    pub fn path(&self) -> &Path {
        &self.path
    }

    //------------------------------------------------------------------
    // 라이브러리 버전 ("0.1.0+abc1234")
    //
    // -in: 없음
    //
    // -out: &str
    // -out: error = 없음
    //------------------------------------------------------------------
    pub fn version(&self) -> &str {
        &self.version
    }

    //------------------------------------------------------------------
    // JSON 결과 함수 한 번 부르기(내부 도우미)
    //=> out 자리를 만들어 부르고, 받은 문자열을 JSON 으로 읽은 뒤 docparse_free 로 돌려준다.
    //
    // -in: f = out 자리를 받아 반환 코드를 주는 호출
    //
    // -out: (c_int, Value) = 반환 코드와 결과 JSON
    // -out: error = 없음(결과가 비거나 JSON 이 아니면 panic 종류의 error JSON 을 만든다)
    //------------------------------------------------------------------
    fn call_json(&self, f: impl FnOnce(*mut *mut c_char) -> c_int) -> (c_int, Value) {
        let mut out: *mut c_char = std::ptr::null_mut();
        let code = f(&mut out);
        if out.is_null() {
            return (code, json!({"error": {"kind": "panic", "message": "문서파서가 결과를 주지 않았습니다"}}));
        }
        let s = unsafe { CStr::from_ptr(out) }.to_string_lossy().into_owned();
        unsafe { (self.free)(out) };
        let v = serde_json::from_str(&s)
            .unwrap_or_else(|e| json!({"error": {"kind": "panic", "message": format!("문서파서 결과 JSON 오류: {}", e)}}));
        (code, v)
    }

    //------------------------------------------------------------------
    // 형식 감지
    //
    // -in: path = 파일 경로
    //
    // -out: &'static str = "pdf"·"hwp"·…·"unknown" (라이브러리의 고정 문자열)
    // -out: error = 없음(실패는 "unknown")
    //------------------------------------------------------------------
    pub fn detect(&self, path: &Path) -> &'static str {
        let p = c_path(path);
        let mut out: *const c_char = std::ptr::null();
        unsafe { (self.detect)(p.as_ptr(), &mut out) };
        if out.is_null() {
            return "unknown";
        }
        // 안전: 라이브러리가 주는 형식 이름은 라이브러리가 내려가기 전까지 사는 고정 문자열이다.
        // 이 값을 쥔 쪽은 DocParse 를 프로세스 끝까지 들고 있어야 한다.
        unsafe { CStr::from_ptr(out) }.to_str().unwrap_or("unknown")
    }

    //------------------------------------------------------------------
    // 본문 추출
    //
    // -in: path = 파일 경로
    // -in: opts = 옵션 JSON(docparse.h 참고, {} 이면 전부 기본값)
    //
    // -out: Extracted = 추출 결과
    // -out: error = 실패하면 ExtractError(종류별)
    //------------------------------------------------------------------
    pub fn extract(&self, path: &Path, opts: &Value) -> Result<Extracted, ExtractError> {
        let p = c_path(path);
        let o = c_str(&opts.to_string());
        let (_code, v) = self.call_json(|out| unsafe { (self.extract)(p.as_ptr(), o.as_ptr(), out) });
        if let Some(e) = v.get("error") {
            return Err(ExtractError::from_json(e));
        }
        let us = |x: &Value, k: &str| x.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
        Ok(Extracted {
            text: v.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
            engine: v.get("engine").and_then(Value::as_str).unwrap_or("").to_string(),
            fmt: v.get("fmt").and_then(Value::as_str).unwrap_or("unknown").to_string(),
            partial: v.get("partial").filter(|x| x.is_object()).map(|x| Partial {
                reason: x.get("reason").and_then(Value::as_str).unwrap_or("").to_string(),
                unit: x.get("unit").and_then(Value::as_str).unwrap_or("").to_string(),
                read: us(x, "read"),
                total: us(x, "total"),
            }),
            layout: v.get("layout").filter(|x| x.is_object()).map(|x| Layout {
                pages: us(x, "pages"),
                scan_pages: us(x, "scan_pages"),
            }),
        })
    }

    //------------------------------------------------------------------
    // 크기 상한 검사만
    //
    // -in: path = 파일 경로
    // -in: fmt  = 형식 이름(None 이면 감지)
    // -in: opts = 옵션 JSON(limits 만 본다)
    //
    // -out: Option<(size, limit, text_kind, reason)> = 걸렸으면 Some
    // -out: error = 인자 오류는 Err(사유)
    //------------------------------------------------------------------
    pub fn check_size(&self, path: &Path, fmt: Option<&str>, opts: &Value)
                      -> Result<Option<(u64, u64, bool, String)>, String> {
        let p = c_path(path);
        let f = fmt.map(c_str);
        let o = c_str(&opts.to_string());
        let (_code, v) = self.call_json(|out| unsafe {
            (self.check_size)(p.as_ptr(), f.as_ref().map_or(std::ptr::null(), |f| f.as_ptr()), o.as_ptr(), out)
        });
        if let Some(e) = v.get("error") {
            return Err(e.get("message").and_then(Value::as_str).unwrap_or("").to_string());
        }
        Ok(v.get("hit").filter(|h| h.is_object()).map(|h| (
            h.get("size").and_then(Value::as_u64).unwrap_or(0),
            h.get("limit").and_then(Value::as_u64).unwrap_or(0),
            h.get("text_kind").and_then(Value::as_bool).unwrap_or(false),
            h.get("reason").and_then(Value::as_str).unwrap_or("").to_string(),
        )))
    }

    //------------------------------------------------------------------
    // 압축 펼치기
    //
    // -in: paths    = 대상 경로들(압축이 아닌 것은 그대로 작업이 된다)
    // -in: tmp_root = 풀 폴더
    // -in: opts     = {"max_depth":…, "max_bytes":…, "max_members":…}
    //
    // -out: Expanded = 작업 목록과 보고
    // -out: error = 인자 오류·내부 오류는 Err(사유)
    //------------------------------------------------------------------
    pub fn expand(&self, paths: &[PathBuf], tmp_root: &Path, opts: &Value) -> Result<Expanded, String> {
        let pj: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        let pj = c_str(&serde_json::to_string(&pj).expect("문자열 배열"));
        let t = c_path(tmp_root);
        let o = c_str(&opts.to_string());
        let (_code, v) = self.call_json(|out| unsafe { (self.expand)(pj.as_ptr(), t.as_ptr(), o.as_ptr(), out) });
        if let Some(e) = v.get("error") {
            return Err(e.get("message").and_then(Value::as_str).unwrap_or("").to_string());
        }
        let s = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let arr = |k: &str| v.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
        let st = v.get("stats").cloned().unwrap_or(json!({}));
        let st_arr = |k: &str| st.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(Expanded {
            jobs: arr("jobs").iter().map(|j| (PathBuf::from(s(j, "src")), s(j, "label"),
                                               j.get("origin").and_then(Value::as_str).map(str::to_string))).collect(),
            unexpanded: st_arr("unexpanded").iter().map(|x| (s(x, "path"), s(x, "reason"), s(x, "kind"))).collect(),
            unsupported: st_arr("unsupported").iter().map(|x| (s(x, "path"), s(x, "reason"))).collect(),
            partial: st_arr("partial").iter().map(|x| (s(x, "path"), s(x, "reason"))).collect(),
        })
    }

    //------------------------------------------------------------------
    // 분할 압축 조각 안내
    //
    // -in: name = 표시용 이름
    // -in: path = 실제 경로(내용까지 볼 때)
    //
    // -out: Option<String> = 조각이면 안내 문구
    // -out: error = 없음(오류는 None)
    //------------------------------------------------------------------
    pub fn split_volume_hint(&self, name: &str, path: Option<&Path>) -> Option<String> {
        let n = c_str(name);
        let p = path.map(c_path);
        let (_code, v) = self.call_json(|out| unsafe {
            (self.hint)(n.as_ptr(), p.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()), out)
        });
        v.get("hint").and_then(Value::as_str).map(str::to_string)
    }

    //------------------------------------------------------------------
    // 6단계 본문 정제
    //
    // -in: text = 원시 텍스트
    //
    // -out: String = 정제된 본문
    // -out: error = 없음(내부 오류면 원문 그대로)
    //------------------------------------------------------------------
    pub fn clean_text(&self, text: &str) -> String {
        let t = c_str(text);
        let mut out: *mut c_char = std::ptr::null_mut();
        let code = unsafe { (self.clean)(t.as_ptr(), &mut out) };
        if out.is_null() {
            return text.to_string();
        }
        let s = unsafe { CStr::from_ptr(out) }.to_string_lossy().into_owned();
        unsafe { (self.free)(out) };
        if code == 0 { s } else { text.to_string() }
    }

    //------------------------------------------------------------------
    // 스캔 PDF 판정
    //
    // -in: pages      = 읽은 쪽 수
    // -in: scan_pages = 그림만 있는 쪽 수
    // -in: body_chars = 공백 뺀 본문 글자 수
    //
    // -out: bool = 스캔 PDF 면 true
    // -out: error = 없음
    //------------------------------------------------------------------
    pub fn scan_pdf_verdict(&self, pages: usize, scan_pages: usize, body_chars: usize) -> bool {
        unsafe { (self.scan)(pages as u64, scan_pages as u64, body_chars as u64) != 0 }
    }

    //------------------------------------------------------------------
    // 로그 콜백 등록
    //=> 콜백은 추출을 부른 스레드에서 불린다. 짧게, 스레드 안전하게 쓴다.
    //
    // -in: f         = 콜백(None 이면 해제)
    // -in: max_level = 넘길 최고 레벨(1=error … 5=trace, 0 = 끔)
    // -in: user      = 콜백에 그대로 돌려줄 포인터
    //
    // -out: 없음
    // -out: error = 없음
    //------------------------------------------------------------------
    pub fn set_logger(&self, f: Option<LogFn>, max_level: i32, user: *mut c_void) {
        unsafe { (self.set_logger)(f, max_level, user) }
    }
}

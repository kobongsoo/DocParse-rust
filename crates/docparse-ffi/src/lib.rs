//! docparse C 진입점 — docparse.dll / libdocparse.so
//!
//! 설계: MpowerClassify plan/Rust-문서파서-라이브러리분리-설계-20261110.html (개정 2) §5
//!
//! 다른 프로그램(MpowerClassify Rust 판은 libloading, Python 판은 ctypes)이 이 라이브러리
//! 파일을 실행 중에 불러 쓴다. 경계에서는 C 문자열(널 종료 UTF-8)과 정수만 오간다.
//!   · 옵션·결과 → UTF-8 JSON 문자열 (필드를 더해도 옛 감싸개가 깨지지 않게)
//!   · 라이브러리가 만든 char* → 반드시 docparse_free 로 돌려받는다
//!   · Rust 패닉 → 경계 밖으로 새지 않게 잡아서 DP_ERR_PANIC 으로 바꾼다
//!   · 전역 상태 없음 → 여러 스레드에서 동시에 불러도 된다(로거 등록만 전역)
//!
//! 헤더: include/docparse.h (함수 모양을 바꾸면 ABI_VERSION 을 올리고 헤더도 같이 고친다)

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use docparse as dp;
use serde_json::{json, Value};

/// 함수 모양(이름·인자)이 바뀔 때만 올린다. 감싸개는 불러온 직후 이 값을 비교한다.
pub const ABI_VERSION: u32 = 1;

// 반환 코드 — docparse.h 의 dp_status 와 같은 값이어야 한다.
const DP_OK: c_int = 0;
const DP_ERR_ARG: c_int = 1;
const DP_ERR_NOT_FOUND: c_int = 2;
const DP_ERR_SIZE_LIMIT: c_int = 3;
const DP_ERR_UNSUPPORTED: c_int = 4;
const DP_ERR_FAILED: c_int = 5;
const DP_ERR_PANIC: c_int = 99;

//------------------------------------------------------------------
// 패닉 내용을 사람이 읽는 문장으로
//=> catch_unwind 가 돌려준 상자에서 &str 이나 String 을 꺼낸다.
//
// -in: e = 패닉 값
//
// -out: String = 패닉 메시지(모르는 모양이면 "알 수 없는 패닉")
// -out: error = 없음
//------------------------------------------------------------------
fn panic_msg(e: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "알 수 없는 패닉".to_string()
    }
}

//------------------------------------------------------------------
// C 문자열 → &str (필수 인자)
//=> NULL 이거나 UTF-8 이 아니면 오류 문장을 돌려준다(부른 쪽 실수로 죽지 않게).
//
// -in: p    = 널 종료 C 문자열 포인터
// -in: name = 오류 문장에 넣을 인자 이름
//
// -out: &str = 빌린 문자열(부른 쪽 메모리)
// -out: error = NULL·잘못된 UTF-8 이면 Err(사유)
//------------------------------------------------------------------
fn arg_str<'a>(p: *const c_char, name: &str) -> Result<&'a str, String> {
    if p.is_null() {
        return Err(format!("{} 가 NULL 입니다", name));
    }
    // 안전: 부른 쪽이 널 종료 문자열을 넘긴다는 것이 이 함수들의 약속이다.
    unsafe { CStr::from_ptr(p) }.to_str().map_err(|_| format!("{} 가 UTF-8 이 아닙니다", name))
}

//------------------------------------------------------------------
// C 문자열 → Option<&str> (선택 인자)
//=> NULL 이면 None. 나머지는 arg_str 과 같다.
//
// -in: p    = 널 종료 C 문자열 포인터(NULL 허용)
// -in: name = 오류 문장에 넣을 인자 이름
//
// -out: Option<&str> = 값 또는 None
// -out: error = 잘못된 UTF-8 이면 Err(사유)
//------------------------------------------------------------------
fn opt_str<'a>(p: *const c_char, name: &str) -> Result<Option<&'a str>, String> {
    if p.is_null() { Ok(None) } else { arg_str(p, name).map(Some) }
}

//------------------------------------------------------------------
// 옵션 JSON 읽기
//=> NULL·빈 문자열은 빈 객체({})로 본다 — 그러면 모든 값이 기본값이 된다.
//
// -in: s = JSON 문자열(없으면 None)
//
// -out: Value = JSON 객체
// -out: error = JSON 이 아니거나 객체가 아니면 Err(사유)
//------------------------------------------------------------------
fn parse_opts(s: Option<&str>) -> Result<Value, String> {
    let s = s.map(str::trim).unwrap_or("");
    if s.is_empty() {
        return Ok(json!({}));
    }
    let v: Value = serde_json::from_str(s).map_err(|e| format!("옵션 JSON 을 읽지 못했습니다: {}", e))?;
    if !v.is_object() {
        return Err("옵션 JSON 은 객체({…})여야 합니다".to_string());
    }
    Ok(v)
}

//------------------------------------------------------------------
// 상한 값 하나 읽기 (u64)
//=> 규칙: 키가 없으면 기본값, null 이나 0 이하면 '상한 없음'(None), 양수면 그 값.
//
// -in: v   = 상한들이 든 JSON 객체
// -in: key = 키 이름
// -in: def = 키가 없을 때 쓸 기본값
//
// -out: Option<u64> = 상한(None = 없음)
// -out: error = 없음(숫자가 아닌 값은 상한 없음으로 본다)
//------------------------------------------------------------------
fn lim_u64(v: &Value, key: &str, def: Option<u64>) -> Option<u64> {
    match v.get(key) {
        None => def,
        Some(x) => x.as_f64().filter(|n| *n > 0.0).map(|n| n as u64),
    }
}

//------------------------------------------------------------------
// 상한 값 하나 읽기 (초 → Duration)
//=> lim_u64 와 같은 규칙. 소수 초도 받는다.
//
// -in: v   = 상한들이 든 JSON 객체
// -in: key = 키 이름
// -in: def = 키가 없을 때 쓸 기본값
//
// -out: Option<Duration> = 시간 상한(None = 없음)
// -out: error = 없음
//------------------------------------------------------------------
fn lim_secs(v: &Value, key: &str, def: Option<Duration>) -> Option<Duration> {
    match v.get(key) {
        None => def,
        Some(x) => x.as_f64().filter(|n| *n > 0.0).map(Duration::from_secs_f64),
    }
}

//------------------------------------------------------------------
// 형식 이름 → Fmt
//=> detect 가 돌려주는 이름("pdf"·"hwp"·…)과 같은 철자를 받는다.
//
// -in: s = 형식 이름
//
// -out: Fmt = 형식
// -out: error = 모르는 이름이면 Err(사유)
//------------------------------------------------------------------
fn fmt_from_str(s: &str) -> Result<dp::Fmt, String> {
    use dp::Fmt::*;
    Ok(match s {
        "pdf" => Pdf, "hwp" => Hwp, "hwpx" => Hwpx, "doc" => Doc, "docx" => Docx,
        "xls" => Xls, "xlsx" => Xlsx, "ppt" => Ppt, "pptx" => Pptx, "html" => Html,
        "text" => Text, "image" => Image, "ole" => Ole, "zip" => Zip, "binary" => Binary,
        "unknown" => Unknown,
        other => return Err(format!("모르는 형식 이름입니다: {}", other)),
    })
}

//------------------------------------------------------------------
// 옵션 JSON → Limits
//=> 키가 없으면 Limits::default()(분리 전 상수와 같은 값)를 쓴다.
//
// -in: v = "limits" 객체(없으면 빈 객체)
//
// -out: Limits = 이번 추출의 상한
// -out: error = 없음
//------------------------------------------------------------------
fn limits_from(v: &Value) -> dp::Limits {
    let def = dp::Limits::default();
    dp::Limits {
        parser_timeout: lim_secs(v, "parser_timeout_secs", def.parser_timeout),
        max_pdf_pages: lim_u64(v, "max_pdf_pages", def.max_pdf_pages.map(|n| n as u64)).map(|n| n as usize),
        max_file_bytes: lim_u64(v, "max_file_bytes", def.max_file_bytes),
        max_file_bytes_text: lim_u64(v, "max_file_bytes_text", def.max_file_bytes_text),
        // 사이냅 시간 상한은 '없음'이 없다 — 0 이하·null 이면 기본값으로 둔다.
        snf_timeout: lim_secs(v, "snf_timeout_secs", Some(def.snf_timeout)).unwrap_or(def.snf_timeout),
        min_text_len: v.get("min_text_len").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(def.min_text_len),
    }
}

//------------------------------------------------------------------
// 옵션 JSON → ExtractOptions
//=> 설계 §5-2 의 키를 읽는다. 모르는 키는 무시한다(새 버전 옵션이 옛 DLL 에서 깨지지 않게).
//    1) engine: "builtin" | "synap_fallback" | "synap_only" (synap_* 는 synap_exe 필수)
//    2) clean(기본 true)·pdfium_lib·label
//    3) limits 객체
//
// -in: v = 옵션 JSON 객체
//
// -out: ExtractOptions = 추출 옵션
// -out: error = 엔진 이름이 틀리거나 synap_exe 가 없으면 Err(사유)
//------------------------------------------------------------------
fn extract_options(v: &Value) -> Result<dp::ExtractOptions, String> {
    let snf = v.get("synap_exe").and_then(Value::as_str).map(PathBuf::from);
    let engine = match v.get("engine").and_then(Value::as_str).unwrap_or("builtin") {
        "builtin" => dp::Engine::Builtin,
        "synap_fallback" => dp::Engine::SynapFallback(snf.ok_or("engine=synap_fallback 에는 synap_exe 가 필요합니다")?),
        "synap_only" => dp::Engine::SynapOnly(snf.ok_or("engine=synap_only 에는 synap_exe 가 필요합니다")?),
        other => return Err(format!("모르는 engine 입니다: {}", other)),
    };
    let empty = json!({});
    Ok(dp::ExtractOptions {
        engine,
        limits: limits_from(v.get("limits").unwrap_or(&empty)),
        pdfium_lib: v.get("pdfium_lib").and_then(Value::as_str).map(PathBuf::from),
        clean: v.get("clean").and_then(Value::as_bool).unwrap_or(true),
        label: v.get("label").and_then(Value::as_str).map(str::to_string),
    })
}

//------------------------------------------------------------------
// 문자열을 C 쪽으로 내보내기
//=> CString 으로 만들어 소유권을 C 쪽에 넘긴다. 해제는 docparse_free 가 한다.
//   본문 안의 NUL(\0) 문자는 C 문자열을 끊으므로 지운다(JSON 은 \u0000 으로 이미 이스케이프됨).
//
// -in: out = 결과를 받을 자리(NULL 이면 아무 일도 하지 않음)
// -in: s   = 내보낼 문자열
//
// -out: 없음
// -out: error = 없음
//------------------------------------------------------------------
fn give(out: *mut *mut c_char, s: String) {
    if out.is_null() {
        return;
    }
    let c = CString::new(s).unwrap_or_else(|e| {
        let mut b = e.into_vec();
        b.retain(|&x| x != 0);
        CString::new(b).expect("NUL 을 지웠다")
    });
    // 안전: out 이 NULL 이 아님을 위에서 확인했다.
    unsafe { *out = c.into_raw() };
}

//------------------------------------------------------------------
// 오류 JSON 만들기
//
// -in: kind    = 오류 종류("arg"·"panic"·"failed" …)
// -in: message = 사람이 읽는 사유
//
// -out: String = {"error":{"kind":…,"message":…}}
// -out: error = 없음
//------------------------------------------------------------------
fn err_json(kind: &str, message: &str) -> String {
    json!({"error": {"kind": kind, "message": message}}).to_string()
}

//------------------------------------------------------------------
// JSON 결과를 내는 함수의 공통 틀
//=> 본체를 catch_unwind 로 감싸 패닉이 C 쪽으로 새지 않게 하고, 결과 JSON 을 out 에 적는다.
//    1) out 이 NULL 이면 DP_ERR_ARG (결과를 돌려줄 곳이 없다)
//    2) 본체가 Err(사유) → DP_ERR_ARG + {"error":{"kind":"arg"}}
//    3) 패닉 → DP_ERR_PANIC + {"error":{"kind":"panic"}}
//
// -in: out  = 결과 JSON 자리
// -in: body = (코드, JSON 문자열)을 돌려주는 본체. 인자 오류는 Err
//
// -out: c_int = 반환 코드(dp_status)
// -out: error = 없음(모든 실패를 코드로 바꾼다)
//------------------------------------------------------------------
fn guarded(out: *mut *mut c_char, body: impl FnOnce() -> Result<(c_int, String), String>) -> c_int {
    if out.is_null() {
        return DP_ERR_ARG;
    }
    // 안전: 실패 때도 out 이 '쓰레기 포인터'로 남지 않게 먼저 비운다.
    unsafe { *out = std::ptr::null_mut() };
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok((code, s))) => { give(out, s); code }
        Ok(Err(why)) => { give(out, err_json("arg", &why)); DP_ERR_ARG }
        Err(e) => { give(out, err_json("panic", &format!("docparse 내부 오류: {}", panic_msg(e)))); DP_ERR_PANIC }
    }
}

//------------------------------------------------------------------
// ABI 버전
//=> 감싸개가 자기와 같은 함수 모양의 라이브러리인지 확인하는 데 쓴다.
//
// -in: 없음
//
// -out: u32 = ABI_VERSION
// -out: error = 없음
//------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn docparse_abi_version() -> u32 {
    ABI_VERSION
}

//------------------------------------------------------------------
// 라이브러리 버전 문자열
//=> "크레이트버전+git해시" (예: "0.1.0+abc1234"). 프로세스가 끝날 때까지 살아 있는
//   고정 문자열이라 부른 쪽은 해제하지 않는다.
//
// -in: 없음
//
// -out: const char* = 버전 문자열
// -out: error = 없음
//------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn docparse_version() -> *const c_char {
    static V: OnceLock<CString> = OnceLock::new();
    V.get_or_init(|| {
        CString::new(format!("{}+{}", env!("CARGO_PKG_VERSION"), env!("DOCPARSE_GIT"))).expect("NUL 없음")
    })
    .as_ptr()
}

//------------------------------------------------------------------
// 형식 감지
//=> 파일 내용으로 형식을 본다. 결과는 "pdf"·"hwp"·…·"unknown" 같은 고정 문자열이라
//   해제하지 않는다.
//
// -in: path    = 파일 경로(UTF-8)
// -in: out_fmt = 형식 이름을 받을 자리
//
// -out: dp_status = DP_OK · DP_ERR_ARG(NULL·UTF-8 아님) · DP_ERR_PANIC
// -out: error = 실패 시 *out_fmt = "unknown"
//------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn docparse_detect(path: *const c_char, out_fmt: *mut *const c_char) -> c_int {
    if out_fmt.is_null() {
        return DP_ERR_ARG;
    }
    // 형식 이름을 널 종료 고정 문자열로 바꾼다(Fmt::as_str 은 널 종료가 아니다).
    fn c_name(f: dp::Fmt) -> *const c_char {
        let s: &'static CStr = match f {
            dp::Fmt::Pdf => c"pdf", dp::Fmt::Hwp => c"hwp", dp::Fmt::Hwpx => c"hwpx",
            dp::Fmt::Doc => c"doc", dp::Fmt::Docx => c"docx", dp::Fmt::Xls => c"xls",
            dp::Fmt::Xlsx => c"xlsx", dp::Fmt::Ppt => c"ppt", dp::Fmt::Pptx => c"pptx",
            dp::Fmt::Html => c"html", dp::Fmt::Text => c"text", dp::Fmt::Image => c"image",
            dp::Fmt::Ole => c"ole", dp::Fmt::Zip => c"zip", dp::Fmt::Binary => c"binary",
            dp::Fmt::Unknown => c"unknown",
        };
        s.as_ptr()
    }
    // 안전: out_fmt 가 NULL 이 아님을 위에서 확인했다. 실패해도 읽을 값이 있게 먼저 채운다.
    unsafe { *out_fmt = c_name(dp::Fmt::Unknown) };
    let p = match arg_str(path, "path") {
        Ok(p) => p.to_string(),
        Err(_) => return DP_ERR_ARG,
    };
    match catch_unwind(|| dp::detect_format(Path::new(&p))) {
        Ok(f) => { unsafe { *out_fmt = c_name(f) }; DP_OK }
        Err(_) => DP_ERR_PANIC,
    }
}

//------------------------------------------------------------------
// 본문 추출
//=> 옵션 JSON 대로 문서 하나에서 본문을 뽑아 결과 JSON 을 돌려준다.
//    1) opts 의 "fmt" 가 있으면 감지를 건너뛰고 그 형식으로(extract_as), 없으면 감지부터
//    2) 성공: {"text","engine","fmt","partial","layout"}
//    3) 실패: {"error":{"kind":"size_limit"|"unsupported"|"failed", "message", …}}
//       파일이 아예 없어서 실패한 것은 코드만 DP_ERR_NOT_FOUND 로 구분한다(kind 는 failed)
//
// -in: path      = 파일 경로(UTF-8)
// -in: opts_json = 옵션 JSON(NULL = 전부 기본값)
// -in: out_json  = 결과 JSON 을 받을 자리 — 성공·실패 모두 docparse_free 로 돌려준다
//
// -out: dp_status = DP_OK · DP_ERR_ARG · DP_ERR_NOT_FOUND · DP_ERR_SIZE_LIMIT ·
//                   DP_ERR_UNSUPPORTED · DP_ERR_FAILED · DP_ERR_PANIC
// -out: error = 실패 사유는 out_json 의 error 에 담긴다
//------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn docparse_extract(path: *const c_char, opts_json: *const c_char, out_json: *mut *mut c_char) -> c_int {
    guarded(out_json, || {
        let path = PathBuf::from(arg_str(path, "path")?);
        let v = parse_opts(opt_str(opts_json, "opts_json")?)?;
        let opts = extract_options(&v)?;
        let res = match v.get("fmt").and_then(Value::as_str) {
            Some(f) => dp::extract_as(&path, fmt_from_str(f)?, &opts),
            None => dp::extract(&path, &opts),
        };
        let (code, out) = match res {
            Ok(x) => (DP_OK, json!({
                "text": x.text,
                "engine": x.engine,
                "fmt": x.fmt.as_str(),
                "partial": x.partial.map(|p| json!({"reason": p.reason, "unit": p.unit, "read": p.read, "total": p.total})),
                "layout": x.layout.map(|l| json!({"pages": l.pages, "scan_pages": l.scan_pages})),
            }).to_string()),
            Err(e) => {
                let message = e.to_string();
                match e {
                    dp::ExtractError::SizeLimit { size, limit, text_kind } => (DP_ERR_SIZE_LIMIT, json!({"error": {
                        "kind": "size_limit", "message": message, "size": size, "limit": limit, "text_kind": text_kind}}).to_string()),
                    dp::ExtractError::Unsupported(f) => (DP_ERR_UNSUPPORTED, json!({"error": {
                        "kind": "unsupported", "message": message, "fmt": f.as_str()}}).to_string()),
                    dp::ExtractError::Failed(_) => (DP_ERR_FAILED, json!({"error": {
                        "kind": "failed", "message": message}}).to_string()),
                }
            }
        };
        // 없는 파일은 '깨진 파일'과 운영 대응이 달라 코드만 따로 준다(kind 는 그대로 —
        // 없는 파일은 감지가 unknown 이라 unsupported 로 나온다).
        let code = if code != DP_OK && !path.exists() { DP_ERR_NOT_FOUND } else { code };
        Ok((code, out))
    })
}

//------------------------------------------------------------------
// 크기 상한 검사만 하기
//=> 추출하지 않고 '이 파일이 크기 상한에 걸리나'만 본다. 부른 쪽이 추출 전에 다른 판정
//   (예: 분할 압축 조각)과 순서를 맞춰야 할 때 쓴다. extract 도 안에서 같은 검사를 한다.
//
// -in: path      = 파일 경로(UTF-8)
// -in: fmt       = 형식 이름(NULL 이면 감지)
// -in: opts_json = 옵션 JSON(limits 만 본다, NULL = 기본값)
// -in: out_json  = {"hit": null | {"size","limit","text_kind","reason"}}
//
// -out: dp_status = DP_OK · DP_ERR_ARG · DP_ERR_PANIC
// -out: error = 인자 오류는 out_json 의 error
//------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn docparse_check_size(path: *const c_char, fmt: *const c_char, opts_json: *const c_char,
                                      out_json: *mut *mut c_char) -> c_int {
    guarded(out_json, || {
        let path = PathBuf::from(arg_str(path, "path")?);
        let fmt = match opt_str(fmt, "fmt")? {
            Some(f) => fmt_from_str(f)?,
            None => dp::detect_format(&path),
        };
        let v = parse_opts(opt_str(opts_json, "opts_json")?)?;
        let empty = json!({});
        let limits = limits_from(v.get("limits").unwrap_or(&empty));
        let hit = dp::limits::check_size_limit(&path, fmt, &limits).map(|h| json!({
            "size": h.size, "limit": h.limit, "text_kind": h.text_kind, "reason": h.reason()}));
        Ok((DP_OK, json!({"hit": hit}).to_string()))
    })
}

//------------------------------------------------------------------
// 압축 펼치기
//=> 경로 목록 중 압축 파일을 tmp_root 아래에 풀고, 추출할 작업 목록과 보고를 돌려준다.
//   임시 폴더를 어디에 만들고 언제 지울지는 부른 쪽이 정한다.
//
// -in: paths_json = 경로 배열 JSON (["a.zip","b.docx"])
// -in: tmp_root   = 풀 폴더(UTF-8)
// -in: opts_json  = {"max_depth":3,"max_bytes":…,"max_members":…} (NULL = 기본값)
// -in: out_json   = {"jobs":[{"src","label","origin"}], "stats":{"unexpanded":[…],"unsupported":[…],"partial":[…]}}
//
// -out: dp_status = DP_OK · DP_ERR_ARG · DP_ERR_PANIC
// -out: error = 인자 오류는 out_json 의 error. 압축별 실패는 stats 에 담긴다
//------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn docparse_expand(paths_json: *const c_char, tmp_root: *const c_char, opts_json: *const c_char,
                                  out_json: *mut *mut c_char) -> c_int {
    guarded(out_json, || {
        let pv: Value = serde_json::from_str(arg_str(paths_json, "paths_json")?)
            .map_err(|e| format!("paths_json 을 읽지 못했습니다: {}", e))?;
        let paths: Vec<PathBuf> = pv.as_array().ok_or("paths_json 은 문자열 배열이어야 합니다")?
            .iter()
            .map(|x| x.as_str().map(PathBuf::from).ok_or("paths_json 은 문자열 배열이어야 합니다"))
            .collect::<Result<_, _>>()?;
        let tmp = PathBuf::from(arg_str(tmp_root, "tmp_root")?);
        let v = parse_opts(opt_str(opts_json, "opts_json")?)?;
        let def = dp::archive::ArchiveOptions::default();
        let opts = dp::archive::ArchiveOptions {
            max_depth: v.get("max_depth").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(def.max_depth),
            max_bytes: lim_u64(&v, "max_bytes", def.max_bytes),
            max_members: lim_u64(&v, "max_members", def.max_members),
        };
        let mut st = dp::archive::ExpandStats::default();
        let jobs = dp::archive::expand_paths(&paths, &tmp, &opts, &mut st);
        let jobs: Vec<Value> = jobs.into_iter().map(|j| json!({
            "src": j.src.to_string_lossy(), "label": j.label, "origin": j.origin})).collect();
        let unexp: Vec<Value> = st.unexpanded.into_iter()
            .map(|(p, r, k)| json!({"path": p, "reason": r, "kind": k})).collect();
        let pairs = |v: Vec<(String, String)>| -> Vec<Value> {
            v.into_iter().map(|(p, r)| json!({"path": p, "reason": r})).collect()
        };
        Ok((DP_OK, json!({"jobs": jobs, "stats": {
            "unexpanded": unexp, "unsupported": pairs(st.unsupported), "partial": pairs(st.partial)}}).to_string()))
    })
}

//------------------------------------------------------------------
// 분할 압축 조각인지 알려 주기
//=> 이름(과 내용)을 보고 '분할 압축의 조각'이면 안내 문구를 준다.
//
// -in: name     = 표시용 이름(라벨)
// -in: path     = 실제 파일 경로(NULL 이면 이름만 본다)
// -in: out_json = {"hint": null | "…"}
//
// -out: dp_status = DP_OK · DP_ERR_ARG · DP_ERR_PANIC
// -out: error = 인자 오류는 out_json 의 error
//------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn docparse_split_volume_hint(name: *const c_char, path: *const c_char, out_json: *mut *mut c_char) -> c_int {
    guarded(out_json, || {
        let name = arg_str(name, "name")?;
        let path = opt_str(path, "path")?.map(PathBuf::from);
        let hint = dp::archive::split_volume_hint(name, path.as_deref());
        Ok((DP_OK, json!({"hint": hint}).to_string()))
    })
}

//------------------------------------------------------------------
// 6단계 본문 정제
//=> 원시 텍스트를 받아 extract(clean=true) 와 같은 정제를 한다. 결과는 JSON 이 아니라
//   정제된 본문 문자열 그대로다.
//
// -in: text     = 원시 텍스트(UTF-8)
// -in: out_text = 정제된 본문을 받을 자리(docparse_free 로 돌려준다)
//
// -out: dp_status = DP_OK · DP_ERR_ARG · DP_ERR_PANIC
// -out: error = 실패 시 out_text 는 오류 JSON
//------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn docparse_clean_text(text: *const c_char, out_text: *mut *mut c_char) -> c_int {
    guarded(out_text, || Ok((DP_OK, dp::clean_text(arg_str(text, "text")?))))
}

//------------------------------------------------------------------
// 스캔 PDF 판정
//=> 읽은 쪽 수·그림 쪽 수·본문 글자 수로 '스캔(이미지) PDF 라 처리제외' 인지 본다.
//
// -in: pages      = 읽은 쪽 수
// -in: scan_pages = 그림만 있는 쪽 수
// -in: body_chars = 공백을 뺀 본문 글자 수
//
// -out: c_int = 1(스캔 PDF) · 0(아님)
// -out: error = 없음(패닉이면 0)
//------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn docparse_scan_pdf_verdict(pages: u64, scan_pages: u64, body_chars: u64) -> c_int {
    catch_unwind(|| dp::scan_pdf_verdict(pages as usize, scan_pages as usize, body_chars as usize))
        .map(|b| b as c_int)
        .unwrap_or(0)
}

//------------------------------------------------------------------
// 라이브러리가 준 문자열 해제
//=> 이 라이브러리 함수가 out 자리에 준 char* 만 받는다. C 의 free() 로 지우면 할당기가
//   달라 죽을 수 있으므로 반드시 이것으로 돌려준다. NULL 이면 아무 일도 하지 않는다.
//
// -in: p = 돌려줄 문자열
//
// -out: 없음
// -out: error = 없음
//------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn docparse_free(p: *mut c_char) {
    if p.is_null() {
        return;
    }
    // 안전: give() 가 CString::into_raw 로 만든 포인터만 들어온다는 것이 약속이다.
    let _ = catch_unwind(|| unsafe { drop(CString::from_raw(p)) });
}

/// 로그 콜백 모양 — level 1=error 2=warn 3=info 4=debug 5=trace.
/// site 는 "모듈경로:줄" (예: "docparse::extract:812").
pub type LogFn = extern "C" fn(level: c_int, target: *const c_char, site: *const c_char,
                               msg: *const c_char, user: *mut c_void);

//------------------------------------------------------------------
// 등록된 로그 콜백 한 벌
// -필드: f    = 콜백 함수
// -필드: user = 부른 쪽이 넘긴 임의 포인터(그대로 돌려준다). 스레드 간 공유를 위해 usize 로 든다
//------------------------------------------------------------------
struct Sink {
    f: LogFn,
    user: usize,
}

static SINK: RwLock<Option<Sink>> = RwLock::new(None);

//------------------------------------------------------------------
// 표준 log 크레이트 → C 콜백 다리
//=> docparse 본체가 log::info!/warn! 으로 남긴 기록을 등록된 콜백으로 넘긴다.
//------------------------------------------------------------------
struct Bridge;

impl log::Log for Bridge {
    //------------------------------------------------------------------
    // 이 기록을 넘길지
    //=> 콜백이 등록돼 있을 때만 넘긴다(레벨은 log::set_max_level 이 거른다).
    //
    // -in: _m = 기록 정보
    //
    // -out: bool = 넘길지
    // -out: error = 없음
    //------------------------------------------------------------------
    fn enabled(&self, _m: &log::Metadata) -> bool {
        SINK.read().map(|s| s.is_some()).unwrap_or(false)
    }

    //------------------------------------------------------------------
    // 기록 하나를 콜백으로 넘기기
    //=> 레벨·target·위치("모듈:줄")·문장을 C 문자열로 바꿔 부른다.
    //
    // -in: r = 기록
    //
    // -out: 없음
    // -out: error = 없음(콜백이 없으면 버린다)
    //------------------------------------------------------------------
    fn log(&self, r: &log::Record) {
        let guard = match SINK.read() { Ok(g) => g, Err(_) => return };
        let Some(sink) = guard.as_ref() else { return };
        let level = match r.level() {
            log::Level::Error => 1, log::Level::Warn => 2, log::Level::Info => 3,
            log::Level::Debug => 4, log::Level::Trace => 5,
        };
        // 문장 속 NUL 은 C 문자열을 끊으므로 지운다.
        let c = |s: String| CString::new(s.replace('\0', "")).expect("NUL 을 지웠다");
        let target = c(r.target().to_string());
        let site = c(format!("{}:{}", r.module_path().unwrap_or("docparse"), r.line().unwrap_or(0)));
        let msg = c(r.args().to_string());
        (sink.f)(level, target.as_ptr(), site.as_ptr(), msg.as_ptr(), sink.user as *mut c_void);
    }

    fn flush(&self) {}
}

static BRIDGE: Bridge = Bridge;

//------------------------------------------------------------------
// 로그 콜백 등록
//=> 등록하지 않으면 로그를 버린다. fn 에 NULL 을 주면 등록을 풀어 다시 버린다.
//   콜백은 추출을 부른 작업 스레드에서 그대로 불린다 — 짧게, 스레드 안전하게 쓰고,
//   콜백 안에서 docparse_* 를 다시 부르지 않는다.
//
// -in: f         = 콜백(NULL 이면 해제)
// -in: max_level = 넘길 최고 레벨(1=error … 5=trace, 0 이하 = 끔)
// -in: user      = 콜백에 그대로 돌려줄 포인터
//
// -out: 없음
// -out: error = 없음
//------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn docparse_set_logger(f: Option<LogFn>, max_level: c_int, user: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // 다리는 프로세스(정확히는 이 DLL)마다 한 번만 단다. 두 번째부터는 콜백만 바꾼다.
        let _ = log::set_logger(&BRIDGE);
        let lv = match (f.is_some(), max_level) {
            (false, _) | (_, i32::MIN..=0) => log::LevelFilter::Off,
            (_, 1) => log::LevelFilter::Error,
            (_, 2) => log::LevelFilter::Warn,
            (_, 3) => log::LevelFilter::Info,
            (_, 4) => log::LevelFilter::Debug,
            _ => log::LevelFilter::Trace,
        };
        if let Ok(mut g) = SINK.write() {
            *g = f.map(|f| Sink { f, user: user as usize });
        }
        log::set_max_level(lv);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    //------------------------------------------------------------------
    // 시험 도우미: JSON 결과 함수를 불러 (코드, JSON) 으로 받기
    //
    // -in: f = out_json 자리를 받아 코드를 돌려주는 호출
    //
    // -out: (c_int, Value) = 반환 코드와 결과 JSON
    // -out: error = JSON 이 아니면 패닉(시험 실패)
    //------------------------------------------------------------------
    fn call(f: impl FnOnce(*mut *mut c_char) -> c_int) -> (c_int, Value) {
        let mut out: *mut c_char = std::ptr::null_mut();
        let code = f(&mut out);
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        docparse_free(out);
        (code, serde_json::from_str(&s).unwrap())
    }

    //------------------------------------------------------------------
    // 시험: 텍스트 파일 추출 성공 · 없는 파일은 NOT_FOUND · NULL 인자는 ARG
    //
    // -in: 없음
    //
    // -out: 없음
    // -out: error = 기대와 다르면 assert 실패
    //------------------------------------------------------------------
    #[test]
    fn 추출_성공과_오류코드() {
        let dir = std::env::temp_dir().join(format!("dpffi_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "안녕하세요 문서파서 시험 본문입니다. 충분히 긴 문장.").unwrap();
        let p = CString::new(f.to_str().unwrap()).unwrap();
        let (code, v) = call(|o| docparse_extract(p.as_ptr(), std::ptr::null(), o));
        assert_eq!(code, DP_OK, "{}", v);
        assert!(v["text"].as_str().unwrap().contains("문서파서"));
        assert_eq!(v["fmt"], "text");

        let missing = CString::new(dir.join("없음.docx").to_str().unwrap()).unwrap();
        let (code, v) = call(|o| docparse_extract(missing.as_ptr(), std::ptr::null(), o));
        assert_eq!(code, DP_ERR_NOT_FOUND, "{}", v);
        assert!(v["error"]["kind"].is_string());

        let (code, v) = call(|o| docparse_extract(std::ptr::null(), std::ptr::null(), o));
        assert_eq!(code, DP_ERR_ARG);
        assert_eq!(v["error"]["kind"], "arg");

        let bad = CString::new("{not json").unwrap();
        let (code, _) = call(|o| docparse_extract(p.as_ptr(), bad.as_ptr(), o));
        assert_eq!(code, DP_ERR_ARG);
        let _ = std::fs::remove_dir_all(&dir);
    }

    //------------------------------------------------------------------
    // 시험: 크기 상한 — 추출은 SIZE_LIMIT, check_size 는 hit 를 준다
    //
    // -in: 없음
    //
    // -out: 없음
    // -out: error = 기대와 다르면 assert 실패
    //------------------------------------------------------------------
    #[test]
    fn 크기_상한() {
        let dir = std::env::temp_dir().join(format!("dpffi_sz_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("big.txt");
        std::fs::write(&f, vec![b'a'; 4096]).unwrap();
        let p = CString::new(f.to_str().unwrap()).unwrap();
        let o = CString::new(r#"{"limits":{"max_file_bytes":100,"max_file_bytes_text":100}}"#).unwrap();
        let (code, v) = call(|out| docparse_extract(p.as_ptr(), o.as_ptr(), out));
        assert_eq!(code, DP_ERR_SIZE_LIMIT, "{}", v);
        assert_eq!(v["error"]["size"], 4096);
        let (code, v) = call(|out| docparse_check_size(p.as_ptr(), std::ptr::null(), o.as_ptr(), out));
        assert_eq!(code, DP_OK);
        assert_eq!(v["hit"]["limit"], 100);
        // 상한을 끄면(0) 걸리지 않는다
        let off = CString::new(r#"{"limits":{"max_file_bytes":0,"max_file_bytes_text":0}}"#).unwrap();
        let (_, v) = call(|out| docparse_check_size(p.as_ptr(), std::ptr::null(), off.as_ptr(), out));
        assert!(v["hit"].is_null());
        let _ = std::fs::remove_dir_all(&dir);
    }

    //------------------------------------------------------------------
    // 시험: 감지·버전·정제·스캔 판정
    //
    // -in: 없음
    //
    // -out: 없음
    // -out: error = 기대와 다르면 assert 실패
    //------------------------------------------------------------------
    #[test]
    fn 작은_함수들() {
        assert_eq!(docparse_abi_version(), ABI_VERSION);
        let v = unsafe { CStr::from_ptr(docparse_version()) }.to_str().unwrap();
        assert!(v.starts_with(env!("CARGO_PKG_VERSION")));
        let mut f: *const c_char = std::ptr::null();
        assert_eq!(docparse_detect(std::ptr::null(), &mut f), DP_ERR_ARG);
        assert_eq!(unsafe { CStr::from_ptr(f) }.to_str().unwrap(), "unknown");
        let t = CString::new("가\u{3000}나").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        assert_eq!(docparse_clean_text(t.as_ptr(), &mut out), DP_OK);
        let got = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        docparse_free(out);
        assert_eq!(got, dp::clean_text("가\u{3000}나"));
        assert_eq!(docparse_scan_pdf_verdict(0, 0, 0), 0);
        docparse_free(std::ptr::null_mut());
    }
}

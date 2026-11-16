//------------------------------------------------------------------
// dp.exe — docparse 시험 도구 (문서 하나 추출 · 여러 건 덤프 · 같음 비교)
//=> DLL 과 같은 C 진입점(docparse_*) 소스를 그대로 넣어 그 함수로 부른다. 그래서 dp 로 본 결과가 곧
//   docparse.dll / libdocparse.so 의 결과다(FFI 층의 JSON 옵션·결과·해제까지 함께 시험된다).
//   명령:
//     version                              버전 · ABI
//     detect  <파일...>                    형식 감지
//     extract <파일> [옵션]                본문(기본) 또는 결과 JSON(--json)
//     check   <파일> [--fmt F] [--limit k=v]  크기 상한만 검사
//     expand  <압축...> --tmp <폴더>       압축 펼치기 → 작업 목록 JSON
//     clean   <텍스트파일>                 6단계 정제만
//     dump    <목록.txt|폴더> <결과폴더>   문서마다 본문 .txt + index.json
//     compare <기준폴더> <새폴더>          두 dump 결과가 글자 하나까지 같은지
//------------------------------------------------------------------

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Map, Value};

// DLL 과 똑같은 C 진입점 소스를 모듈로 넣는다 — dp 로 본 결과가 곧 DLL 의 결과가 되게.
// (DLL 에서만 쓰고 dp 는 안 쓰는 함수가 있어 dead_code 경고는 끈다)
#[allow(dead_code)]
#[path = "../../docparse-ffi/src/lib.rs"]
mod ffi;

const USAGE: &str = "\
dp — docparse 시험 도구

사용법:
  dp version
  dp detect  <파일...>
  dp extract <파일> [--json] [--raw] [--fmt F] [--engine E] [--synap-exe 경로]
                    [--pdfium 경로] [--limit 키=값]... [--out 파일]
  dp check   <파일> [--fmt F] [--limit 키=값]...
  dp expand  <압축...> --tmp <폴더> [--max-depth N] [--max-bytes N] [--max-members N]
  dp clean   <텍스트파일> [--out 파일]
  dp dump    <목록.txt|폴더> <결과폴더> [extract 옵션]
  dp compare <기준폴더> <새폴더>

공통 옵션:
  --log error|warn|info|debug|trace   라이브러리 로그를 표준 오류로
  --limit 키=값   parser_timeout_secs · max_pdf_pages · max_file_bytes ·
                  max_file_bytes_text · snf_timeout_secs · min_text_len  (값 none = 상한 없음)
";

//------------------------------------------------------------------
// 명령줄 옵션
// -필드: args      = 옵션을 뺀 위치 인자
// -필드: json      = 결과 JSON 전체를 낸다(extract)
// -필드: raw       = 정제하지 않은 원시 텍스트(clean=false)
// -필드: fmt       = 감지를 건너뛰고 이 형식으로
// -필드: engine    = builtin | synap_fallback | synap_only
// -필드: synap_exe = 사이냅 snf_exe 경로
// -필드: pdfium    = pdfium 라이브러리 경로
// -필드: limits    = 상한 덮어쓰기(키 → 값, null = 상한 없음)
// -필드: out       = 결과를 쓸 파일
// -필드: tmp       = expand 가 풀 폴더
// -필드: arch      = expand 상한(max_depth · max_bytes · max_members)
// -필드: log       = 로그 레벨(1~5, 0 = 끔)
//------------------------------------------------------------------
#[derive(Default)]
struct Cli {
    args: Vec<String>,
    json: bool,
    raw: bool,
    fmt: Option<String>,
    engine: Option<String>,
    synap_exe: Option<String>,
    pdfium: Option<String>,
    limits: Map<String, Value>,
    out: Option<String>,
    tmp: Option<String>,
    arch: Map<String, Value>,
    log: i32,
}

// 사용법 오류 — 사유와 사용법을 찍고 종료 코드 1(DP_ERR_ARG 와 같은 뜻)로 끝낸다
fn die(msg: &str) -> ! {
    eprintln!("{}\n\n{}", msg, USAGE);
    std::process::exit(1)
}

//------------------------------------------------------------------
// 명령줄 읽기
//
// -in: v = 명령 이름 다음의 인자들
//
// -out: Cli
// -out: error = 값이 빠지거나 모르는 옵션이면 die(종료 코드 1)
//------------------------------------------------------------------
fn parse_cli(v: Vec<String>) -> Cli {
    let mut c = Cli::default();
    let mut it = v.into_iter();
    while let Some(a) = it.next() {
        // 값을 받는 옵션은 다음 인자를 꺼낸다
        let mut val = |name: &str| it.next().unwrap_or_else(|| die(&format!("{} 값이 없습니다", name)));
        match a.as_str() {
            "--json" => c.json = true,
            "--raw" => c.raw = true,
            "--fmt" => c.fmt = Some(val("--fmt")),
            "--engine" => c.engine = Some(val("--engine")),
            "--synap-exe" => c.synap_exe = Some(val("--synap-exe")),
            "--pdfium" => c.pdfium = Some(val("--pdfium")),
            "--out" => c.out = Some(val("--out")),
            "--tmp" => c.tmp = Some(val("--tmp")),
            "--limit" => {
                let kv = val("--limit");
                let (k, v) = kv.split_once('=').unwrap_or_else(|| die(&format!("--limit 는 키=값 모양이어야 합니다: {}", kv)));
                c.limits.insert(k.trim().to_string(), num_or_none(v, "--limit"));
            }
            "--max-depth" | "--max-bytes" | "--max-members" => {
                // 옵션 이름 --max-depth → JSON 키 max_depth
                let key = a.trim_start_matches("--").replace('-', "_");
                let v = val(&a);
                c.arch.insert(key, num_or_none(&v, &a));
            }
            "--log" => {
                c.log = match val("--log").as_str() {
                    "error" => 1, "warn" => 2, "info" => 3, "debug" => 4, "trace" => 5, "off" => 0,
                    x => die(&format!("모르는 로그 레벨: {}", x)),
                }
            }
            "-h" | "--help" => {
                print!("{}", USAGE);
                std::process::exit(0)
            }
            x if x.starts_with("--") => die(&format!("모르는 옵션: {}", x)),
            _ => c.args.push(a),
        }
    }
    c
}

// "123" → 123, "none" → null (상한 없음)
fn num_or_none(v: &str, name: &str) -> Value {
    if v.eq_ignore_ascii_case("none") {
        return Value::Null;
    }
    v.trim().parse::<u64>().map(Value::from).unwrap_or_else(|_| die(&format!("{} 값은 숫자 또는 none: {}", name, v)))
}

// 라이브러리 로그 → 표준 오류 (C 콜백 모양 그대로)
extern "C" fn on_log(level: c_int, _target: *const c_char, site: *const c_char, msg: *const c_char, _user: *mut c_void) {
    let name = ["", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"].get(level as usize).copied().unwrap_or("?");
    // 안전: 라이브러리가 콜백 동안 살아 있는 널 종료 문자열을 준다(docparse.h 약속)
    let (site, msg) = unsafe { (CStr::from_ptr(site).to_string_lossy(), CStr::from_ptr(msg).to_string_lossy()) };
    eprintln!("[dp] {} {} ({})", name, msg, site);
}

// C 문자열 만들기(경로·JSON 에 NUL 이 있으면 사용법 오류)
fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_else(|_| die("인자에 NUL 문자가 있습니다"))
}

//------------------------------------------------------------------
// 라이브러리가 준 문자열 받기
//=> char* 를 Rust 문자열로 복사하고 곧바로 docparse_free 로 돌려준다(헤더의 약속).
//
// -in: p = 라이브러리가 out 자리에 준 포인터(NULL 가능)
//
// -out: String = 받은 문자열(NULL 이면 빈 문자열)
// -out: error = 없음
//------------------------------------------------------------------
fn take(p: *mut c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // 안전: p 는 docparse_* 가 준 널 종료 문자열이다
    let s = unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() };
    ffi::docparse_free(p);
    s
}

// 결과 JSON 문자열 → Value (못 읽으면 문자열 그대로 담는다)
fn to_json(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| json!({"unparsed": s}))
}

// 상태 코드 이름 (docparse.h 의 dp_status)
fn status_name(rc: c_int) -> &'static str {
    match rc {
        0 => "DP_OK", 1 => "DP_ERR_ARG", 2 => "DP_ERR_NOT_FOUND", 3 => "DP_ERR_SIZE_LIMIT",
        4 => "DP_ERR_UNSUPPORTED", 5 => "DP_ERR_FAILED", 99 => "DP_ERR_PANIC", _ => "?",
    }
}

//------------------------------------------------------------------
// 명령줄 → docparse_extract 옵션 JSON
//
// -in: c = 명령줄 옵션
//
// -out: Value = {"clean", "engine", "synap_exe", "pdfium_lib", "fmt", "limits"} 중 준 것만
// -out: error = 없음 (값 검사는 라이브러리가 한다)
//------------------------------------------------------------------
fn extract_opts(c: &Cli) -> Value {
    let mut o = Map::new();
    // 정제는 기본으로 켜져 있으니 --raw 일 때만 끈다
    if c.raw {
        o.insert("clean".into(), json!(false));
    }
    for (k, v) in [("engine", &c.engine), ("synap_exe", &c.synap_exe), ("pdfium_lib", &c.pdfium), ("fmt", &c.fmt)] {
        if let Some(v) = v {
            o.insert(k.into(), json!(v));
        }
    }
    if !c.limits.is_empty() {
        o.insert("limits".into(), Value::Object(c.limits.clone()));
    }
    Value::Object(o)
}

//------------------------------------------------------------------
// 문서 하나 추출 (docparse_extract 그대로)
//
// -in: path = 문서 경로 / opts = 옵션 JSON
//
// -out: (상태 코드, 결과 JSON, 걸린 ms)
// -out: error = 없음 (실패도 코드와 JSON 으로 돌려준다)
//------------------------------------------------------------------
fn extract_one(path: &str, opts: &Value) -> (c_int, Value, f64) {
    let p = cstr(path);
    let o = cstr(&opts.to_string());
    let mut out: *mut c_char = std::ptr::null_mut();
    let t0 = Instant::now();
    let rc = ffi::docparse_extract(p.as_ptr(), o.as_ptr(), &mut out);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    (rc, to_json(&take(out)), ms)
}

// 결과 JSON 의 오류문
fn err_msg(v: &Value) -> String {
    v.pointer("/error/message").and_then(Value::as_str).unwrap_or("").to_string()
}

// 글자 수(바이트가 아니라 문자)
fn chars(s: &str) -> usize {
    s.chars().count()
}

// 결과를 --out 파일 또는 화면으로
fn emit(c: &Cli, body: &str) {
    match &c.out {
        Some(f) => {
            if let Some(d) = Path::new(f).parent() {
                std::fs::create_dir_all(d).ok();
            }
            std::fs::write(f, body).unwrap_or_else(|e| die(&format!("쓰기 실패 {}: {}", f, e)));
            eprintln!("[dp] 저장: {}", f);
        }
        None => println!("{}", body),
    }
}

// 결과 JSON 을 읽기 좋게
fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

//------------------------------------------------------------------
// version — 라이브러리 버전 · ABI
//
// -in: 없음
//
// -out: 종료 코드 0 (ABI 가 헤더와 다르면 1)
// -out: error = 없음
//------------------------------------------------------------------
fn cmd_version() -> i32 {
    // 안전: docparse_version 은 프로세스 끝까지 사는 고정 문자열을 준다(해제하지 않는다)
    let v = unsafe { CStr::from_ptr(ffi::docparse_version()).to_string_lossy().into_owned() };
    let abi = ffi::docparse_abi_version();
    println!("docparse {} (abi {})", v, abi);
    // 시험 도구가 알고 있는 ABI 와 다르면 표시 — 헤더(DOCPARSE_ABI_VERSION)와 맞아야 한다
    if abi != 1 {
        eprintln!("[dp] ABI 가 dp 가 아는 값(1)과 다릅니다: {}", abi);
        return 1;
    }
    0
}

//------------------------------------------------------------------
// detect — 파일마다 형식 이름
//
// -in: c.args = 파일 경로들
//
// -out: 종료 코드 0 (인자 오류가 하나라도 있으면 그 코드)
// -out: error = 없음
//------------------------------------------------------------------
fn cmd_detect(c: &Cli) -> i32 {
    if c.args.is_empty() {
        die("사용법: dp detect <파일...>");
    }
    let mut worst = 0;
    for a in &c.args {
        let p = cstr(a);
        let mut f: *const c_char = std::ptr::null();
        let rc = ffi::docparse_detect(p.as_ptr(), &mut f);
        // 안전: 실패해도 "unknown" 고정 문자열이 채워진다(해제하지 않는다)
        let name = if f.is_null() { "?".into() } else { unsafe { CStr::from_ptr(f).to_string_lossy() } };
        println!("{:<8} {}", name, a);
        if rc != 0 {
            worst = rc;
        }
    }
    worst
}

//------------------------------------------------------------------
// extract — 문서 하나에서 본문 뽑기
//=> 기본은 본문만 화면에 내고, 요약 한 줄(형식 · 엔진 · 글자 수 · 시간)은 표준 오류에 낸다.
//   --json 이면 라이브러리가 준 결과 JSON 전체를 낸다.
//
// -in: c.args = [파일]
//
// -out: 종료 코드 = 라이브러리 상태 코드(0 성공, 2 파일 없음, 3 상한, 4 지원 안 함, 5 실패 …)
// -out: error = 실패면 "실패(코드 이름): 사유" 를 표준 오류에
//------------------------------------------------------------------
fn cmd_extract(c: &Cli) -> i32 {
    let path = match c.args.as_slice() {
        [p] => p,
        _ => die("사용법: dp extract <파일> [옵션]"),
    };
    let (rc, v, ms) = extract_one(path, &extract_opts(c));
    if c.json {
        emit(c, &pretty(&v));
    }
    if rc != 0 {
        // 없는 파일은 감지가 unknown 이라 사유가 '지원하지 않는 포맷'으로 나온다 — 헷갈리지 않게 덧붙인다
        // 내장 추출기가 읽지 못하면 라이브러리는 사유를 비워 둔다(분리 전과 같은 동작) — 볼 곳을 알려 준다
        let hint = match rc {
            2 => format!(" (파일 없음: {})", path),
            5 if err_msg(&v).is_empty() => "(사유 없음 — 깨진 파일 · 암호 문서이거나, PDF 면 pdfium 을 찾지 못한 것: --log debug · --pdfium 확인)".into(),
            _ => String::new(),
        };
        eprintln!("실패({} {}): {}{}", rc, status_name(rc), err_msg(&v), hint);
        return rc;
    }
    let text = v["text"].as_str().unwrap_or("");
    if !c.json {
        emit(c, text);
    }
    // 부분 추출 · 스캔 쪽 정보는 놓치기 쉬우니 요약 줄에 붙인다
    let mut extra = String::new();
    if let Some(p) = v.get("partial").filter(|p| !p.is_null()) {
        extra.push_str(&format!(" partial={}", p));
    }
    if let Some(l) = v.get("layout").filter(|l| !l.is_null()) {
        extra.push_str(&format!(" layout={}", l));
        // PDF 면 부르는 쪽이 하는 '스캔 PDF 처리제외' 판정도 같은 함수로 해 보여 준다
        let body = text.chars().filter(|ch| !ch.is_whitespace()).count() as u64;
        let pages = l["pages"].as_u64().unwrap_or(0);
        let scan = l["scan_pages"].as_u64().unwrap_or(0);
        if ffi::docparse_scan_pdf_verdict(pages, scan, body) == 1 {
            extra.push_str(" 판정=스캔PDF(처리제외)");
        }
    }
    eprintln!("[dp] fmt={} engine={} chars={} {:.0}ms{}",
              v["fmt"].as_str().unwrap_or("?"), v["engine"].as_str().unwrap_or("?"), chars(text), ms, extra);
    0
}

//------------------------------------------------------------------
// check — 추출하지 않고 크기 상한만 검사
//
// -in: c.args = [파일] / c.fmt / c.limits
//
// -out: 종료 코드 0(통과) · 3(상한에 걸림) · 그 밖은 라이브러리 코드
// -out: error = 인자 오류는 표준 오류에
//------------------------------------------------------------------
fn cmd_check(c: &Cli) -> i32 {
    let path = match c.args.as_slice() {
        [p] => p,
        _ => die("사용법: dp check <파일> [--fmt F] [--limit 키=값]"),
    };
    let p = cstr(path);
    let fmt = c.fmt.as_deref().map(cstr);
    let o = cstr(&json!({"limits": Value::Object(c.limits.clone())}).to_string());
    let mut out: *mut c_char = std::ptr::null_mut();
    let rc = ffi::docparse_check_size(p.as_ptr(), fmt.as_ref().map_or(std::ptr::null(), |f| f.as_ptr()), o.as_ptr(), &mut out);
    let v = to_json(&take(out));
    println!("{}", pretty(&v));
    if rc != 0 {
        eprintln!("실패({} {}): {}", rc, status_name(rc), err_msg(&v));
        return rc;
    }
    // 상한에 걸렸으면 extract 가 낼 코드와 같은 3 으로 알린다
    if v["hit"].is_null() { 0 } else { 3 }
}

//------------------------------------------------------------------
// expand — 압축 펼치기
//
// -in: c.args = 압축(또는 일반 파일) 경로들 / c.tmp = 풀 폴더 / c.arch = 상한
//
// -out: 종료 코드 0 (라이브러리 코드가 0 이 아니면 그 코드)
// -out: error = --tmp 가 없으면 die
//------------------------------------------------------------------
fn cmd_expand(c: &Cli) -> i32 {
    if c.args.is_empty() {
        die("사용법: dp expand <압축...> --tmp <폴더>");
    }
    let tmp = c.tmp.clone().unwrap_or_else(|| die("--tmp <폴더> 가 필요합니다"));
    std::fs::create_dir_all(&tmp).ok();
    let paths = cstr(&json!(c.args).to_string());
    let t = cstr(&tmp);
    let o = cstr(&Value::Object(c.arch.clone()).to_string());
    let mut out: *mut c_char = std::ptr::null_mut();
    let rc = ffi::docparse_expand(paths.as_ptr(), t.as_ptr(), o.as_ptr(), &mut out);
    let v = to_json(&take(out));
    emit(c, &pretty(&v));
    if rc != 0 {
        eprintln!("실패({} {}): {}", rc, status_name(rc), err_msg(&v));
        return rc;
    }
    let n = |k: &str| v.pointer(&format!("/stats/{}", k)).and_then(Value::as_array).map_or(0, |a| a.len());
    eprintln!("[dp] 작업 {}건 · 못 푼 압축 {} · 지원 안 함 {} · 일부만 {}",
              v["jobs"].as_array().map_or(0, |a| a.len()), n("unexpanded"), n("unsupported"), n("partial"));
    0
}

//------------------------------------------------------------------
// clean — 텍스트 파일을 6단계 정제만
//
// -in: c.args = [텍스트 파일(UTF-8)]
//
// -out: 종료 코드 0 (라이브러리 코드가 0 이 아니면 그 코드)
// -out: error = 파일을 못 읽으면 die
//------------------------------------------------------------------
fn cmd_clean(c: &Cli) -> i32 {
    let path = match c.args.as_slice() {
        [p] => p,
        _ => die("사용법: dp clean <텍스트파일>"),
    };
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("읽기 실패 {}: {}", path, e)));
    let t = cstr(&raw);
    let mut out: *mut c_char = std::ptr::null_mut();
    let rc = ffi::docparse_clean_text(t.as_ptr(), &mut out);
    let s = take(out);
    if rc != 0 {
        eprintln!("실패({} {}): {}", rc, status_name(rc), s);
        return rc;
    }
    emit(c, &s);
    eprintln!("[dp] {} → {} 글자", chars(&raw), chars(&s));
    0
}

//------------------------------------------------------------------
// 목록 읽기 — 목록 파일(한 줄에 경로 하나) 또는 폴더(안의 파일을 이름 순으로, 하위 폴더 포함)
//
// -in: src = 목록 파일 또는 폴더
//
// -out: Vec<String> = 문서 경로들
// -out: error = 못 읽으면 die
//------------------------------------------------------------------
fn read_list(src: &str) -> Vec<String> {
    let p = Path::new(src);
    if p.is_dir() {
        // 폴더면 하위까지 훑어 이름 순으로 — 매번 같은 순서여야 compare 가 맞는다
        fn walk(d: &Path, acc: &mut Vec<PathBuf>) {
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() { walk(&p, acc) } else { acc.push(p) }
                }
            }
        }
        let mut v = vec![];
        walk(p, &mut v);
        v.sort();
        return v.into_iter().map(|p| p.to_string_lossy().into_owned()).collect();
    }
    std::fs::read_to_string(p).unwrap_or_else(|e| die(&format!("목록 읽기 실패 {}: {}", src, e)))
        .lines()
        // 메모장이 붙이는 BOM 과 앞뒤 공백을 떼고, 빈 줄 · # 주석 줄은 건너뛴다
        .map(|l| l.trim().trim_start_matches('\u{feff}').to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

// 파일 이름 부분
fn basename(p: &str) -> String {
    Path::new(p).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

//------------------------------------------------------------------
// dump — 목록의 문서마다 본문을 파일로 남긴다
//=> <결과폴더>/<번호>/<이름>.txt 에 본문, <결과폴더>/index.json 에 문서별 요약.
//    1) 번호는 목록 순서(0000, 0001 …) — 이름이 같은 문서가 있어도 겹치지 않게
//    2) 실패한 문서는 .txt 를 만들지 않고 index 에 rc · err 를 남긴다
//
// -in: c.args = [목록 파일 또는 폴더, 결과 폴더] / 나머지는 extract 옵션
//
// -out: 종료 코드 0 (문서별 실패는 index 에만 남기고 계속)
// -out: error = 목록을 못 읽거나 결과 폴더를 못 쓰면 die
//------------------------------------------------------------------
fn cmd_dump(c: &Cli) -> i32 {
    let (list, out) = match c.args.as_slice() {
        [l, o] => (l.clone(), o.clone()),
        _ => die("사용법: dp dump <목록.txt|폴더> <결과폴더> [extract 옵션]"),
    };
    let paths = read_list(&list);
    let opts = extract_opts(c);
    std::fs::create_dir_all(&out).unwrap_or_else(|e| die(&format!("폴더 만들기 실패 {}: {}", out, e)));
    let mut index = Map::new();
    let (mut ok, mut fail) = (0, 0);
    let t_all = Instant::now();
    for (i, p) in paths.iter().enumerate() {
        let key = format!("{:04}", i);
        let (rc, v, ms) = extract_one(p, &opts);
        let mut row = json!({"path": p, "rc": rc, "ms": ms.round()});
        if rc == 0 {
            let text = v["text"].as_str().unwrap_or("");
            let d = Path::new(&out).join(&key);
            std::fs::create_dir_all(&d).ok();
            let f = d.join(format!("{}.txt", basename(p)));
            std::fs::write(&f, text).unwrap_or_else(|e| die(&format!("쓰기 실패 {}: {}", f.display(), e)));
            row["file"] = json!(format!("{}/{}.txt", key, basename(p)));
            row["fmt"] = v["fmt"].clone();
            row["engine"] = v["engine"].clone();
            row["chars"] = json!(chars(text));
            row["partial"] = v["partial"].clone();
            row["layout"] = v["layout"].clone();
            ok += 1;
            eprintln!("[{}/{}] {:>6.0}ms {:<5} {}", i + 1, paths.len(), ms, v["fmt"].as_str().unwrap_or("?"), basename(p));
        } else {
            row["err"] = json!(err_msg(&v));
            fail += 1;
            eprintln!("[{}/{}] {:>6.0}ms 실패({}) {} — {}", i + 1, paths.len(), ms, rc, basename(p), err_msg(&v));
        }
        index.insert(key, row);
    }
    let idx = Path::new(&out).join("index.json");
    std::fs::write(&idx, pretty(&Value::Object(index))).unwrap_or_else(|e| die(&format!("index 쓰기 실패: {}", e)));
    eprintln!("끝 — 성공 {} · 실패 {} · {:.1}초 → {}", ok, fail, t_all.elapsed().as_secs_f64(), idx.display());
    0
}

//------------------------------------------------------------------
// 두 본문의 첫 차이 설명
//=> 몇 번째 글자에서 갈라지는지와 그 앞뒤 몇 글자를 보여 준다.
//
// -in: a = 기준 본문 / b = 새 본문
//
// -out: String = "글자 N: «…» ≠ «…»" (같으면 빈 문자열)
// -out: error = 없음
//------------------------------------------------------------------
fn first_diff(a: &str, b: &str) -> String {
    let (ac, bc): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let i = ac.iter().zip(&bc).take_while(|(x, y)| x == y).count();
    if i == ac.len() && i == bc.len() {
        return String::new();
    }
    // 차이 앞 10글자부터 30글자를 잘라 보여 준다. 줄바꿈은 ⏎ 로 바꿔 한 줄에 담는다
    let cut = |v: &[char]| -> String {
        let s = i.saturating_sub(10);
        v[s.min(v.len())..(i + 20).min(v.len())].iter().collect::<String>().replace('\n', "⏎")
    };
    format!("글자 {} 에서 갈림 (길이 {} → {}): «{}» ≠ «{}»", i, ac.len(), bc.len(), cut(&ac), cut(&bc))
}

// dump 결과 폴더의 index.json 읽기
fn read_index(dir: &str) -> Map<String, Value> {
    let p = Path::new(dir).join("index.json");
    let s = std::fs::read_to_string(&p).unwrap_or_else(|e| die(&format!("index.json 읽기 실패 {}: {}", p.display(), e)));
    match serde_json::from_str(&s) {
        Ok(Value::Object(m)) => m,
        _ => die(&format!("index.json 모양이 아닙니다: {}", p.display())),
    }
}

//------------------------------------------------------------------
// compare — 두 dump 결과 비교
//=> 기준 폴더의 문서마다 새 폴더의 같은 번호와 견준다.
//    1) 상태 코드(rc) · 형식(fmt) · 엔진(engine) 이 같은지
//    2) 둘 다 성공이면 본문 .txt 가 글자 하나까지 같은지(다르면 첫 차이 위치)
//
// -in: c.args = [기준 폴더, 새 폴더]
//
// -out: 종료 코드 0(전부 같음) · 1(하나라도 다름 · 없음)
// -out: error = index.json 을 못 읽으면 die
//------------------------------------------------------------------
fn cmd_compare(c: &Cli) -> i32 {
    let (a, b) = match c.args.as_slice() {
        [a, b] => (a.clone(), b.clone()),
        _ => die("사용법: dp compare <기준폴더> <새폴더>"),
    };
    let (ia, ib) = (read_index(&a), read_index(&b));
    let (mut same, mut diff, mut missing) = (0, 0, 0);
    for (key, ra) in &ia {
        let name = basename(ra["path"].as_str().unwrap_or(""));
        let Some(rb) = ib.get(key) else {
            missing += 1;
            println!("없음  {} {}", key, name);
            continue;
        };
        // 1) 요약 필드부터 — 형식이 바뀌었으면 본문 차이보다 그게 원인이다
        let mut why = String::new();
        for f in ["rc", "fmt", "engine"] {
            if ra.get(f) != rb.get(f) {
                why = format!(".{}: {} ≠ {}", f, ra.get(f).unwrap_or(&Value::Null), rb.get(f).unwrap_or(&Value::Null));
                break;
            }
        }
        // 2) 둘 다 성공이면 본문 비교
        if why.is_empty() && ra["rc"] == 0 {
            let rd = |dir: &str, r: &Value| std::fs::read_to_string(Path::new(dir).join(r["file"].as_str().unwrap_or(""))).unwrap_or_default();
            why = first_diff(&rd(&a, ra), &rd(&b, rb));
        }
        if why.is_empty() {
            same += 1;
        } else {
            diff += 1;
            println!("다름  {} {} — {}", key, name, why);
        }
    }
    println!("같음 {} · 다름 {} · 새 쪽에 없음 {} (기준 {}건)", same, diff, missing, ia.len());
    if diff == 0 && missing == 0 { 0 } else { 1 }
}

//------------------------------------------------------------------
// 진입점 — 명령 이름으로 갈라 부른다
//
// -in: 없음(명령줄)
//
// -out: 없음 (종료 코드로 결과를 알린다)
// -out: error = 모르는 명령이면 종료 코드 1
//------------------------------------------------------------------
fn main() {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        print!("{}", USAGE);
        std::process::exit(1);
    }
    let cmd = argv.remove(0);
    let c = parse_cli(argv);
    // 로그를 달라고 했을 때만 콜백을 건다 — 안 걸면 라이브러리가 로그를 버린다
    if c.log > 0 {
        ffi::docparse_set_logger(Some(on_log), c.log, std::ptr::null_mut());
    }
    let rc = match cmd.as_str() {
        "version" | "--version" | "-V" => cmd_version(),
        "detect" => cmd_detect(&c),
        "extract" => cmd_extract(&c),
        "check" => cmd_check(&c),
        "expand" => cmd_expand(&c),
        "clean" => cmd_clean(&c),
        "dump" => cmd_dump(&c),
        "compare" => cmd_compare(&c),
        "help" | "-h" | "--help" => {
            print!("{}", USAGE);
            0
        }
        x => die(&format!("모르는 명령: {}", x)),
    };
    std::process::exit(rc)
}

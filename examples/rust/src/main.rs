//! docparse Rust 예제 — 압축은 펼치고, 나온 문서마다 본문 추출.
//!
//!   cargo run -- a.hwp b.zip
//!
//! 실제 프로젝트에서는 묶음의 rust/docparse_dyn.rs 를 src/ 에 복사해 `mod docparse_dyn;` 으로 쓴다.
//! 여기서는 저장소의 정본을 바로 가리킨다.
#[path = "../../../bindings/rust/docparse_dyn.rs"]
mod docparse_dyn;

use docparse_dyn::{DocParse, ExtractError};
use serde_json::json;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::{Path, PathBuf};

// 로그 콜백 — C 함수 모양이어야 한다. 추출을 부른 스레드에서 불린다.
extern "C" fn on_log(level: c_int, _target: *const c_char, _site: *const c_char,
                     msg: *const c_char, _user: *mut c_void) {
    // 라이브러리가 넘긴 msg 는 콜백이 도는 동안만 유효한 널 종료 UTF-8 이다.
    let m = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    eprintln!("[docparse {}] {}", level, m);
}

fn main() {
    // 찾는 순서: (직접 지정) → 환경변수 DOCPARSE_LIB → 실행파일 폴더 → 그 상위 폴더
    let dp = DocParse::find_and_load(None, "DOCPARSE_LIB").unwrap_or_else(|e| {
        eprintln!("{}", e);
        std::process::exit(1);
    });
    println!("docparse {} ({})", dp.version(), dp.path().display());
    dp.set_logger(Some(on_log), 2, std::ptr::null_mut());

    let inputs: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let tmp = std::env::temp_dir().join(format!("docparse-example-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();

    // 압축은 펼쳐서 안의 문서를, 압축이 아닌 파일은 그대로 job 으로 돌려준다.
    let ex = dp.expand(&inputs, &tmp, &json!({"max_depth": 3})).expect("expand");
    for (src, label, _origin) in &ex.jobs {
        show(&dp, src, label);
    }
    println!("펼치지 못함: {:?}", ex.unexpanded);
    let _ = std::fs::remove_dir_all(&tmp);
}

fn show(dp: &DocParse, src: &Path, label: &str) {
    let opts = json!({"clean": true, "label": label, "limits": {"max_pdf_pages": 500}});
    match dp.extract(src, &opts) {
        Ok(r) => {
            let head: String = r.text.chars().take(40).collect::<String>().replace('\n', " ");
            println!("{} : {} {}자 | {}", label, r.fmt, r.text.chars().count(), head);
        }
        Err(ExtractError::SizeLimit { size, limit, .. }) => println!("{} : 크기 초과 {} > {}", label, size, limit),
        Err(e) => println!("{} : {}", label, e),
    }
}

//! 빌드 스크립트 — 라이브러리 버전 문자열에 붙일 git 해시를 새긴다.
//!
//! docparse_version() 이 "0.1.0+abc1234" 처럼 돌려줘서, 부른 프로그램이 로그·레코드에
//! "어느 파서로 뽑았나" 를 남길 수 있게 한다. 커밋 안 된 수정이 있으면 "-dirty" 를 붙인다.
//! Linux 에서는 libdocparse.so 에 RPATH=$ORIGIN 을 새겨 옆의 libpdfium.so 를 찾게 한다.

//------------------------------------------------------------------
// 빌드 진입점
//=> git 해시를 환경변수 DOCPARSE_GIT 로 컴파일러에 넘기고, Linux 면 RPATH 를 단다.
//    1) 환경변수 DOCPARSE_GIT 이 있으면 그 값, 없으면 git rev-parse --short HEAD (git 이 없으면 "nogit")
//    2) 추적 중인 파일에 수정이 있으면 "-dirty"
//    3) Linux 대상이면 링크 인자 -Wl,-rpath,$ORIGIN
//
// -in: 없음
//
// -out: 없음(cargo 지시문을 표준출력으로 낸다)
// -out: error = 없음(git 실패는 "nogit" 으로 대신)
//------------------------------------------------------------------
fn main() {
    // git 명령 한 번 실행해 성공하면 출력 한 줄을 돌려주는 작은 도우미
    let run = |args: &[&str]| {
        std::process::Command::new("git").args(args).output().ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    // 빌드서버처럼 .git 없이 소스만 올려 빌드할 때는 올린 쪽이 해시를 환경변수로 넘긴다.
    println!("cargo:rerun-if-env-changed=DOCPARSE_GIT");
    let hash = match std::env::var("DOCPARSE_GIT") {
        Ok(h) if !h.trim().is_empty() => h.trim().to_string(),
        _ => {
            let mut h = run(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "nogit".to_string());
            // 수정된 채로 빌드한 것은 구분해 둔다 — 같은 해시인데 결과가 다를 수 있기 때문이다.
            if run(&["status", "--porcelain", "--untracked-files=no"]).map_or(false, |s| !s.is_empty()) {
                h.push_str("-dirty");
            }
            h
        }
    };
    println!("cargo:rustc-env=DOCPARSE_GIT={}", hash);
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    // Linux: 라이브러리 옆의 libpdfium.so 를 동적 링커가 찾을 수 있게 $ORIGIN 을 새긴다.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-rpath,$ORIGIN");
    }
}

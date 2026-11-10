//! docparse — 문서 → 본문 추출 라이브러리
//!
//! 설계: MpowerClassify plan/Rust-문서파서-라이브러리분리-설계-20261110.html (개정 2)
//!
//! MpowerClassify-rs 실행파일 안에 섞여 있던 추출 코드를 떼어 낸 것이다. 추출 결과는
//! 분리 전과 바이트 단위로 같다. 달라진 것은 '설정을 받는 방법'뿐이다:
//!   · 추출 엔진·상한·pdfium 위치 → [`ExtractOptions`] 인자로 매번 넘긴다(전역 없음)
//!   · 부분추출·스캔 페이지 표식  → [`Extracted`] 반환값에 담겨 온다
//!   · 로그                       → 표준 `log` 크레이트로만 남긴다. 어디에 적을지는
//!                                  쓰는 프로그램이 로거를 달아 정한다
//!
//! 쓰는 순서:
//! ```no_run
//! use docparse::{extract, ExtractOptions};
//! let x = extract(std::path::Path::new("a.docx"), &ExtractOptions::default()).unwrap();
//! println!("{} ({}자)", x.engine, x.text.chars().count());
//! ```
//! 압축(feature "archive", 기본 켜짐)은 [`archive::expand_paths`] 로 펼친 Job 마다
//! `extract(&job.src, …)` 를 부르고, 결과 표시는 `job.label` 로 한다.
//!
//! 전역 상태가 없으므로 여러 스레드에서 동시에 불러도 된다.

// 시험 이름에 XML 태그 이름(cellStyleXfs 등)을 그대로 쓴 것이 있다 — 시험에서만 허용한다.
#![cfg_attr(test, allow(non_snake_case))]

pub mod detect;
mod extract;
mod hwp5;
mod html_entities;
pub mod limits;
mod modpath;
mod office_legacy;
mod ole;
mod xls;
mod xlsdate;

#[cfg(feature = "archive")]
pub mod archive;
#[cfg(feature = "synap")]
pub mod synap;

pub use detect::{detect_format, Fmt};
pub use extract::{
    clean_text, extract, extract_as, scan_pdf_verdict, Engine, ExtractError, ExtractOptions,
    Extracted, Layout, Partial,
};
pub use limits::Limits;

/// 파서가 '부분 추출' 같은 운영 경고를 남길 때 쓰는 log target.
/// 쓰는 프로그램은 이 target 의 기록을 자기 오류 로그에도 적을 수 있다
/// (MpowerClassify-rs 는 class_err_*.log 에 적는다).
pub const ERRLOG_TARGET: &str = "docparse::errlog";

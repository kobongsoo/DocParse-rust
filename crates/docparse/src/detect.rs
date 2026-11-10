//! 내용 기반 포맷 감지 (Python detect.py 포팅)
//! 확장자가 아니라 매직바이트/컨테이너 내부 구조로 판별한다.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// 감지된 포맷 종류. Python detect_format 반환값과 동일 문자열 매핑.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fmt {
    Pdf,
    Hwp,
    Hwpx,
    Doc,
    Docx,
    Xls,
    Xlsx,
    Ppt,
    Pptx,
    Html,
    Text,
    Image,
    Ole,
    Zip,
    Binary,
    Unknown,
}

impl Fmt {
    pub fn as_str(&self) -> &'static str {
        match self {
            Fmt::Pdf => "pdf", Fmt::Hwp => "hwp", Fmt::Hwpx => "hwpx",
            Fmt::Doc => "doc", Fmt::Docx => "docx", Fmt::Xls => "xls",
            Fmt::Xlsx => "xlsx", Fmt::Ppt => "ppt", Fmt::Pptx => "pptx",
            Fmt::Html => "html", Fmt::Text => "text", Fmt::Image => "image",
            Fmt::Ole => "ole", Fmt::Zip => "zip", Fmt::Binary => "binary",
            Fmt::Unknown => "unknown",
        }
    }
}

const OLE_MAGIC: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";

/// 파일 앞부분과 컨테이너 내부를 보고 포맷을 판별한다.
pub fn detect_format(path: &Path) -> Fmt {
    let mut head = [0u8; 2048];
    let n = match File::open(path).and_then(|mut f| f.read(&mut head)) {
        Ok(n) => n,
        Err(_) => return Fmt::Unknown,
    };
    let head = &head[..n];
    if head.is_empty() {
        return Fmt::Text;
    }

    // PDF
    if head.starts_with(b"%PDF-") {
        return Fmt::Pdf;
    }
    // 이미지 매직
    if head.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'])
        || head.starts_with(&[0xFF, 0xD8, 0xFF])
        || head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a")
        || head.starts_with(b"BM")
    {
        return Fmt::Image;
    }
    // OLE(구형 doc/xls/ppt/hwp 공용) → 내부 스트림 검사
    if head.starts_with(OLE_MAGIC) {
        let quick = detect_ole(head);
        // 앞 2KB 안에 스트림명이 없으면(디렉터리가 파일 뒤쪽에 있는 흔한 경우)
        // 실제 CFB 디렉터리를 읽어 정확히 가린다. 이 폴백이 없으면 구형 .doc 이
        // 'ole' 로만 잡혀 추출기가 없는 포맷 취급을 받는다.
        if quick == Fmt::Ole {
            return detect_ole_exact(path);
        }
        return quick;
    }
    // ZIP(docx/xlsx/pptx/hwpx/일반 zip 공용) → 내부 이름 검사
    if head.starts_with(ZIP_MAGIC) {
        return detect_zip(path);
    }
    // HTML 마커
    let lower: Vec<u8> = head.iter().take(512).map(|b| b.to_ascii_lowercase()).collect();
    let ls = String::from_utf8_lossy(&lower);
    if ls.contains("<!doctype html") || ls.contains("<html") || ls.contains("<head") || ls.contains("<body") {
        return Fmt::Html;
    }
    // 텍스트 vs 바이너리: NUL 바이트 유무
    if head.contains(&0u8) {
        Fmt::Binary
    } else {
        Fmt::Text
    }
}

/// OLE 내부 스트림 이름을 UTF-16LE 흔적으로 대략 판별(경량 휴리스틱).
/// 완전한 CFB 파서 대신, 스트림명이 앞부분에 UTF-16LE 로 박혀 있는 점을 이용한다.
fn detect_ole(head: &[u8]) -> Fmt {
    // UTF-16LE 로 인코딩된 대표 스트림명을 바이트열로 찾는다.
    let has = |needle: &str| -> bool {
        let utf16: Vec<u8> = needle.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        contains_subslice(head, &utf16)
    };
    if has("WordDocument") { return Fmt::Doc; }
    if has("Workbook") || has("Book") { return Fmt::Xls; }
    if has("PowerPoint Document") { return Fmt::Ppt; }
    if has("FileHeader") || has("HwpSummaryInformation") { return Fmt::Hwp; }
    Fmt::Ole
}

/// CFB 디렉터리를 실제로 읽어 OLE 세부 포맷을 가린다(위 휴리스틱이 실패했을 때만).
/// 파일 전체를 읽으므로 값이 비싸다 — 그래서 폴백으로만 쓴다.
fn detect_ole_exact(path: &Path) -> Fmt {
    let data = match std::fs::read(path) { Ok(d) => d, Err(_) => return Fmt::Ole };
    let ole = match crate::ole::Ole::open(data) { Some(o) => o, None => return Fmt::Ole };
    let names = ole.names();
    let has = |n: &str| names.iter().any(|x| *x == n);
    if has("WordDocument") { return Fmt::Doc; }
    if has("PowerPoint Document") { return Fmt::Ppt; }
    if has("Workbook") || has("Book") { return Fmt::Xls; }
    if has("FileHeader") || has("HwpSummaryInformation") { return Fmt::Hwp; }
    Fmt::Ole
}

/// ZIP 내부 항목 이름으로 OOXML/HWPX/일반 zip 을 구분.
fn detect_zip(path: &Path) -> Fmt {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Fmt::Zip,
    };
    let mut zip = match zip::ZipArchive::new(file) {
        Ok(z) => z,
        Err(_) => return Fmt::Zip,
    };
    let mut names: Vec<String> = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        if let Ok(f) = zip.by_index(i) {
            names.push(f.name().to_string());
        }
    }
    let any = |pred: &dyn Fn(&str) -> bool| names.iter().any(|n| pred(n));
    if any(&|n| n == "word/document.xml") { return Fmt::Docx; }
    if any(&|n| n == "xl/workbook.xml") { return Fmt::Xlsx; }
    if any(&|n| n == "ppt/presentation.xml") { return Fmt::Pptx; }
    if any(&|n| n.starts_with("Contents/section")) || any(&|n| n == "Contents/content.hpf") {
        return Fmt::Hwpx;
    }
    Fmt::Zip
}

fn contains_subslice(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > hay.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| w == needle)
}

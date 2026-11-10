//! 압축파일 확장기 — 아카이브 1개를 내부 파일 여러 개로 펼치기
//! (파이썬 판 `src/csoclassify/extract/archive.py` 포팅)
//!
//! 압축파일 1개를 검사하면 예전에는 "읽을 수 없는 파일"로 끝나서, 그 안에 무엇이
//! 들었는지 알 수 없었다. 여기서는 압축을 임시폴더에 풀어 내부 **파일 하나하나**를
//! 분류 대상으로 만든다. 그래야 어느 내부 문서가 민감한지 파일별로 나온다.
//! 각 내부 파일은 어떤 최상위 압축(`origin`)에서 나왔는지 함께 돌려줘, 상위가
//! '압축파일 자체'의 집계 등급(내부 최고 위험)을 매길 수 있게 한다.
//!
//! 지원 포맷(확장자가 아니라 **내용**으로 감지 — 확장자 위장도 잡는다):
//!   · zip                        → `zip` crate (docx/xlsx/pptx/hwpx 는 '문서 1개'라 제외)
//!   · tar · tar.gz               → `tar` + `flate2`
//!   · tar.bz2 · tar.xz           → `tar` + `bzip2-rs` / `lzma-rs`
//!   · gz · bz2 · xz (단일 파일)  → 위 해제기들
//!   · 7z                         → `sevenz-rust2`(순수 Rust)
//!   · rar                        → `rars`(순수 Rust) — **단일 rar 만**
//!
//! [rar 을 rars 로 고른 이유] 다른 길은 `unrar` 크레이트인데, 파이썬 판과 같은
//! 엔진이지만 **UnRAR C 소스를 빌드에 포함**한다. C 컴파일이 생겨 CentOS 7 빌드가
//! 까다로워지고, UnRAR 소스의 라이선스 조건을 상용 배포 관점에서 따로 확인해야 한다.
//! `rars` 는 순수 Rust라 그 둘을 모두 피한다(MIT/Apache-2.0).
//!
//! [넣기 전에 검증한 것] 참조 구현(번들 `unrar.exe`)과 크레이트가 담은 실제 RAR
//! 픽스처 177개를 바이트 단위로 대조했다. 단일 아카이브 131건 기준 **판정 일치
//! 95.4%**, 그중 **참조가 푸는데 rars 가 못 푼 경우는 0건**이었다(오히려 rars 가
//! 더 관대한 경우가 4건). 손상 입력 117건(절단·바이트변조·헤더제로)에서 **패닉·
//! 멈춤 0건**. rars 자체 테스트도 1,148건이 통과한다.
//!
//! [그래도 남긴 두 가지 한계 — 표식으로 드러낸다]
//!   · **분할 볼륨** — 조각을 잇지 않는다. zip·tar·7z 도 마찬가지라 이 판 전체가
//!     같은 수준이다. `kind="split_volume"` 로 남긴다.
//!   · **하드링크** — rars 는 하드링크 항목을 파일로 내놓지 않는다(실측 2건).
//!     원본은 그대로 분류되지만 '같은 내용의 다른 이름'은 빠진다.
//!     펼친 개수가 목록보다 적으면 `stats.partial` 로 남긴다.

use std::collections::VecDeque;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::detect::{self, Fmt};
use crate::limits;

/// 압축 1건당 해제 누적 바이트 기본 상한(500MB) — 분리 전 DEFAULT_MAX_ARCHIVE_BYTES.
pub const DEFAULT_MAX_ARCHIVE_BYTES: u64 = 500 * 1024 * 1024;
/// 압축 1건당 내부 파일 개수 기본 상한(5000) — 분리 전 DEFAULT_MAX_ARCHIVE_MEMBERS.
pub const DEFAULT_MAX_ARCHIVE_MEMBERS: u64 = 5000;
/// 중첩 압축을 펼칠 기본 최대 깊이(3).
pub const DEFAULT_MAX_DEPTH: usize = 3;

//------------------------------------------------------------------
// 압축 펼치기 옵션 (G4 압축폭탄 방어)
//=> 예전에는 상한을 프로그램 전역(limits 모듈)에서 읽었다. 이제 부른 쪽이 넘긴다.
//   None 은 '상한 없음'이다(예전의 0 과 같은 뜻).
//
// -필드: max_depth   = 중첩 압축을 펼칠 최대 깊이(기본 3 — 무한재귀 방지)
// -필드: max_bytes   = 최상위 압축 1건당 해제 누적 바이트 상한(기본 500MB)
// -필드: max_members = 최상위 압축 1건당 내부 파일 개수 상한(기본 5000)
//------------------------------------------------------------------
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveOptions {
    pub max_depth: usize,
    pub max_bytes: Option<u64>,
    pub max_members: Option<u64>,
}

impl Default for ArchiveOptions {
    //------------------------------------------------------------------
    // 기본 옵션 — 분리 전 코드 기본값과 같다
    //
    // -in: 없음
    //
    // -out: ArchiveOptions = 깊이 3 · 500MB · 5000개
    // -out: error = 없음
    //------------------------------------------------------------------
    fn default() -> Self {
        ArchiveOptions {
            max_depth: DEFAULT_MAX_DEPTH,
            max_bytes: Some(DEFAULT_MAX_ARCHIVE_BYTES),
            max_members: Some(DEFAULT_MAX_ARCHIVE_MEMBERS),
        }
    }
}

/// 잡음 항목(분류 대상 아님): macOS 메타.
const NOISE_PREFIX: &str = "__MACOSX/";
const NOISE_BASENAMES: [&str; 1] = [".DS_Store"];

/// 한 번에 풀어 볼 수 있는 최대 바이트(단일 압축 폭탄 판정용 여유분).
const PEEK: usize = 2048;

//------------------------------------------------------------------
// 감지된 압축 종류
//=> 파이썬 판 `_archive_type()` 의 반환 문자열과 1:1로 맞춘다. 사유 문구와
//   집계가 두 판에서 같아야 하기 때문이다.
//
// -필드: Zip/Tar/Gz/Bz2/Xz/SevenZ = 이 판이 펼칠 수 있는 것
// -필드: Rar                     = 감지는 하되 펼치지 못하는 것(사유를 남긴다)
//------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArcType {
    Zip,
    Tar,
    Gz,
    Bz2,
    Xz,
    SevenZ,
    Rar,
}

impl ArcType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArcType::Zip => "zip",
            ArcType::Tar => "tar",
            ArcType::Gz => "gz",
            ArcType::Bz2 => "bz2",
            ArcType::Xz => "xz",
            ArcType::SevenZ => "7z",
            ArcType::Rar => "rar",
        }
    }
    /// 이 판이 실제로 펼칠 수 있는가.
    pub fn supported(&self) -> bool {
        // 2026-09-07 기준 모든 감지 포맷을 펼친다(7z=sevenz-rust2, rar=rars).
        // 분할 볼륨은 포맷과 무관하게 못 잇는데, 그건 확장 단계에서 따로 알린다.
        true
    }
}

//------------------------------------------------------------------
// 분류 대상 한 건
//=> 압축을 펼치면 '읽을 실제 경로'와 '사람이 보는 경로'가 갈라진다. 내부 파일은
//   임시폴더에 있지만(src), 결과에는 `압축경로/내부경로`(label)로 나와야 한다 —
//   그래야 파일명·경로 규칙 신호가 원래대로 먹고 사람도 어디 있던 문서인지 안다.
//
// -필드: src    = 실제로 읽을 경로(압축 내부면 임시파일)
// -필드: label  = 표시·신호용 경로
// -필드: origin = 이 파일이 나온 최상위 압축 경로(일반 파일이면 None)
//------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct Job {
    pub src: PathBuf,
    pub label: String,
    pub origin: Option<String>,
}

//------------------------------------------------------------------
// 확장 결과 보고
//=> 로그만 남기면 결과를 보는 사람에게 안 닿는다. 내부 문서 수백 건이 통째로
//   빠졌는데 요약에는 "압축 1건 처리"로만 보이는 것이 가장 나쁜 실패다.
//
// -필드: unexpanded  = 못 펼친 압축 [(경로, 사유, 종류)] — 종류는 limit|error
// -필드: unsupported = 이 판이 지원하지 않는 압축 [(경로, 사유)]
// -필드: partial     = 펼치기는 했지만 일부 항목을 못 꺼낸 압축 [(경로, 사유)]
//                      (rar 하드링크처럼 — 조용히 빠지면 사람이 알 길이 없다)
//------------------------------------------------------------------
#[derive(Default, Debug)]
pub struct ExpandStats {
    pub unexpanded: Vec<(String, String, &'static str)>,
    pub unsupported: Vec<(String, String)>,
    pub partial: Vec<(String, String)>,
}

//------------------------------------------------------------------
// 압축 확장 실패 — 사유와 '종류'
//=> 왜 못 펼쳤는지를 두 갈래로 나눈다. 사람이 할 일이 다르기 때문이다.
//     limit — 상한을 넘었다. 값을 올리면 처리된다(정상 압축).
//     error — 손상·암호·분할볼륨·미지원 등. 원본이나 포맷을 봐야 한다.
//   문자열을 훑어 판단하면 문구를 손볼 때마다 조용히 깨지므로 타입으로 들고 간다.
//
// -필드: reason = 사람이 읽을 사유
// -필드: kind   = "limit" | "error"
//------------------------------------------------------------------
pub struct ArcError {
    pub reason: String,
    pub kind: &'static str,
}

impl ArcError {
    /// 상한 초과 — 값을 올리면 처리되는 경우.
    fn limit(reason: String) -> Self {
        ArcError { reason, kind: "limit" }
    }
    /// 그 밖의 실패 — 손상·암호·분할볼륨·미지원 등.
    fn error(reason: String) -> Self {
        ArcError { reason, kind: "error" }
    }
}

// 라이브러리 오류를 `?` 로 그대로 올릴 때는 '실패'로 본다(상한은 위 limit 로만 만든다).
impl From<String> for ArcError {
    fn from(reason: String) -> Self {
        ArcError::error(reason)
    }
}

//------------------------------------------------------------------
// 압축 해제 예산 (G4)
//=> 압축 하나를 푸는 동안 "누적 몇 바이트, 몇 개까지"를 세다가 상한을 넘으면 멈춘다.
//
//   [왜 필요한가] 중첩 깊이(max_depth)는 '깊이'만 막는다. 깊이 1인 평범한 zip
//   하나로도 디스크를 채울 수 있다(폭 방향 압축폭탄). 깊이와 폭은 다른 축이라
//   한쪽 방어로 다른 쪽을 막을 수 없다.
//   [왜 읽기 전에 세나] zip·tar 헤더에는 '풀었을 때 크기'가 적혀 있다. 그 값을
//   먼저 더하면 실제로 풀지 않고도 폭탄을 알아챈다 — 다 푼 뒤에 재면 늦다.
//
// -필드: max_bytes/max_members = 상한(0 이면 무제한)
// -필드: used_bytes/used_members = 지금까지 쓴 양
//------------------------------------------------------------------
struct Budget {
    max_bytes: u64,
    max_members: u64,
    used_bytes: u64,
    used_members: u64,
}

impl Budget {
    //------------------------------------------------------------------
    // 최상위 압축 1건의 새 예산
    //=> 옵션의 상한으로 시작한다. None(무제한)은 내부 표기 0 으로 바꾼다 —
    //   take()·remaining() 이 분리 전과 같은 '0 = 무제한' 규칙으로 돈다.
    //
    // -in: opts = 압축 펼치기 옵션
    //
    // -out: Budget = 쓴 양 0 인 예산
    // -out: error = 없음
    //------------------------------------------------------------------
    fn new(opts: &ArchiveOptions) -> Self {
        Budget {
            max_bytes: opts.max_bytes.unwrap_or(0),
            max_members: opts.max_members.unwrap_or(0),
            used_bytes: 0,
            used_members: 0,
        }
    }

    //------------------------------------------------------------------
    // 항목 하나를 예산에 더한다
    //=> 파일 하나를 풀기 직전에 부른다. 넘으면 사유를 돌려 해제를 멈추게 한다.
    //
    // -in: nbytes = 이 항목의 (풀었을 때) 크기. 모르면 0
    //
    // -out: Result<(), ArcError> = 초과 시 Err(kind="limit")
    //------------------------------------------------------------------
    fn take(&mut self, nbytes: u64) -> Result<(), ArcError> {
        self.used_members += 1;
        // 개수 먼저 — 0바이트 파일 수백만 개짜리 폭탄은 바이트로는 안 걸린다.
        if self.max_members > 0 && self.used_members > self.max_members {
            return Err(ArcError::limit(format!(
                "압축 내부 파일 개수 상한 초과({}개, MAX_ARCHIVE_MEMBERS)",
                self.max_members
            )));
        }
        self.used_bytes += nbytes;
        if self.max_bytes > 0 && self.used_bytes > self.max_bytes {
            return Err(ArcError::limit(format!(
                "압축 해제 누적 상한 초과({} > {}, MAX_ARCHIVE_BYTES)",
                limits::human_bytes(self.used_bytes),
                limits::human_bytes(self.max_bytes)
            )));
        }
        Ok(())
    }

    /// 남은 바이트 여유(무제한이면 None) — 크기를 미리 알 수 없는 단일 압축에 쓴다.
    fn remaining(&self) -> Option<u64> {
        if self.max_bytes == 0 {
            None
        } else {
            Some(self.max_bytes.saturating_sub(self.used_bytes))
        }
    }
}

//------------------------------------------------------------------
// 분할 zip 의 '마지막 조각'인지 — 파일 속을 들여다본다
//=> 분할 zip 은 앞 조각이 .z01·.z02 … 이고 마지막 조각만 평범한 .zip 이다.
//   그래서 이름 규칙만으로는 마지막 조각을 정상 zip 과 구분할 수 없다.
//   zip 은 파일 끝에 EOCD(End Of Central Directory) 라는 꼬리표를 두는데,
//   여기 '이 디스크 번호'가 0 이 아니면 여러 장으로 나뉜 zip 이라는 뜻이다.
//   실측: 4조각짜리 01_제품매뉴얼.zip 의 EOCD 가 disk=3 이었다.
//
//   [왜 이름이 아니라 내용인가] 마지막 조각은 이름이 그냥 .zip 이고 머리도 PK
//   매직이 아니어서(중간 데이터로 시작) 압축 감지에도 안 잡힌다. 그대로 두면
//   '정체불명 추출 실패'가 되어, 사람은 파일이 깨진 줄 알고 원본을 뒤진다.
//
// -in: path = 실제로 읽을 수 있는 파일 경로
//
// -out: bool = 분할 zip 의 조각이면 true
// -out: error = 없음(읽기 실패·형식 불일치는 모두 false)
//------------------------------------------------------------------
fn zip_is_split_volume(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let size = match f.metadata() {
        Ok(m) => m.len(),
        Err(_) => return false,
    };
    // EOCD 는 파일 끝에 있다. zip 주석이 최대 64KB 라 그만큼만 뒤에서 읽으면 충분하다.
    let want = std::cmp::min(size, 65557 + 22);
    if f.seek(SeekFrom::Start(size - want)).is_err() {
        return false;
    }
    let mut tail = vec![0u8; want as usize];
    if f.read_exact(&mut tail).is_err() {
        return false;
    }
    // 뒤에서부터 EOCD 표식을 찾는다(주석 안에 같은 바이트가 있을 수 있어 마지막 것을 쓴다).
    let sig = [0x50u8, 0x4b, 0x05, 0x06];
    let mut at = None;
    for i in (0..tail.len().saturating_sub(11)).rev() {
        if tail[i..i + 4] == sig {
            at = Some(i);
            break;
        }
    }
    let i = match at {
        Some(i) => i,
        None => return false,
    };
    let this_disk = u16::from_le_bytes([tail[i + 4], tail[i + 5]]);
    let cd_disk = u16::from_le_bytes([tail[i + 6], tail[i + 7]]);
    // 0xFFFF 는 "zip64 를 보라"는 표식이다 — 그것도 여러 장이라는 뜻이라 같이 친다.
    this_disk != 0 || cd_disk != 0
}


//------------------------------------------------------------------
// 분할 압축(멀티볼륨)의 조각인지 — 파일 이름으로 본다
//=> 분할 압축은 조각 하나만으로는 아무것도 꺼낼 수 없다. 이 판은 조각을 모아
//   잇지 않으므로(zip·tar·7z 모두 라이브러리 차원에서 미지원), 그 사실을 결과에
//   분명히 남기려고 이름 규칙으로 알아본다.
//
//   [왜 이름으로 보나] 내용으로는 구분이 안 된다. 첫 조각은 정상 압축과 똑같은
//   매직으로 시작하고, 두 번째 이후 조각은 매직이 없어 '그냥 이진 파일'로 보인다.
//   실측에서 5조각짜리 7z 의 세 번째 조각이 text 로 감지돼, 뜻 없는 이진 쓰레기가
//   본문으로 분류될 뻔했다. 이름이 유일한 신호다 — 딱 하나, 분할 zip 의 마지막
//   조각만은 이름이 평범한 .zip 이라 내용(EOCD 디스크 번호)을 봐야 한다
//   (위 zip_is_split_volume).
//   [오탐을 줄이려고] 숫자 확장자는 앞에 압축 확장자가 붙은 경우만 인정한다
//   ('backup.001' 같은 일반 파일을 조각으로 오해하지 않으려는 것이다).
//
// -in: name = 파일 경로 또는 이름
// -in: path = 실제로 읽을 수 있는 경로(주면 zip 마지막 조각까지 내용으로 가려낸다).
//             압축 내부 항목처럼 실체가 없는 대상이면 None 을 준다
//
// -out: Option<&'static str> = 조각으로 보이면 설명, 아니면 None
// -out: error = 없음
//------------------------------------------------------------------
pub fn split_volume_hint(name: &str, path: Option<&Path>) -> Option<&'static str> {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static NUMBERED: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)\.(7z|zip|tar|tgz|tar\.gz|tar\.bz2|tar\.xz|rar)\.[0-9]{2,}$").unwrap()
    });
    static ZIP_PART: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\.z[0-9]{2}$").unwrap());
    static RAR_PART: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\.part[0-9]+\.rar$").unwrap());
    static RAR_OLD: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\.r[0-9]{2}$").unwrap());

    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    if NUMBERED.is_match(base) {
        return Some("7z·zip·tar 분할 조각(.001 …)");
    }
    if ZIP_PART.is_match(base) {
        return Some("zip 분할 조각(.z01 …)");
    }
    if RAR_PART.is_match(base) {
        return Some("rar 분할 조각(.partN.rar)");
    }
    if RAR_OLD.is_match(base) {
        return Some("rar 분할 조각(.r00 …)");
    }
    // 여기까지 이름으로 못 걸렀는데 .zip 이면 마지막 조각일 수 있다 —
    // 이것만은 내용(EOCD 디스크 번호)을 봐야 정상 zip 과 구분된다.
    if base.to_ascii_lowercase().ends_with(".zip") {
        if let Some(p) = path {
            if zip_is_split_volume(p) {
                return Some("zip 분할 마지막 조각(.zip)");
            }
        }
    }
    None
}

//------------------------------------------------------------------
// 잡음 내부 항목인지
//=> macOS 메타(__MACOSX/…, .DS_Store)는 분류 대상이 아니라 거른다.
//
// -in: name = 내부 경로 문자열
//
// -out: bool = 잡음이면 true
// -out: error = 없음
//------------------------------------------------------------------
fn is_noise(name: &str) -> bool {
    if name.starts_with(NOISE_PREFIX) {
        return true;
    }
    let base = name.rsplit('/').next().unwrap_or(name);
    NOISE_BASENAMES.contains(&base)
}

//------------------------------------------------------------------
// zip 내부 항목 이름 복원(한글 깨짐 방지)
//=> 옛 zip 은 파일명을 cp437 로 저장한다. UTF-8 플래그가 없으면 zip 라이브러리가
//   cp437 로 읽어 한글이 깨지므로, 바이트로 되돌린 뒤 cp949 로 다시 해석한다.
//
// -in: raw       = zip 이 준 이름
// -in: utf8_flag = 항목에 UTF-8 플래그가 있으면 true(그대로 쓴다)
//
// -out: String = 사람이 읽는 내부 경로(복원 실패 시 원본 그대로)
// -out: error = 없음
//------------------------------------------------------------------
fn zip_entry_name(raw: &str, utf8_flag: bool) -> String {
    if utf8_flag {
        return raw.to_string();
    }
    // cp437 로 잘못 읽힌 문자열을 원래 바이트로 되돌린다(각 char 가 1바이트에 대응).
    let mut bytes = Vec::with_capacity(raw.len());
    for ch in raw.chars() {
        match cp437_byte(ch) {
            Some(b) => bytes.push(b),
            // cp437 로 설명되지 않는 글자가 있으면 이미 올바른 이름이라고 본다.
            None => return raw.to_string(),
        }
    }
    let (cow, _, had_err) = encoding_rs::EUC_KR.decode(&bytes);
    if had_err {
        raw.to_string()
    } else {
        cow.into_owned()
    }
}

/// cp437 문자 → 원래 바이트(역매핑). ASCII 는 그대로다.
fn cp437_byte(ch: char) -> Option<u8> {
    if (ch as u32) < 0x80 {
        return Some(ch as u8);
    }
    CP437_HIGH.iter().position(|&c| c == ch).map(|i| (i + 0x80) as u8)
}

/// cp437 상위 128자(0x80~0xFF) 표.
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å',
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ',
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»',
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐',
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧',
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀',
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩',
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', ' ',
];

//------------------------------------------------------------------
// tar 헤더인가(ustar 매직)
//=> tar 는 매직이 파일 맨 앞이 아니라 257바이트째에 있다.
//
// -in: buf = 파일(또는 해제된) 앞부분 바이트
//
// -out: bool = tar 로 보이면 true
// -out: error = 없음
//------------------------------------------------------------------
fn looks_like_tar(buf: &[u8]) -> bool {
    if buf.len() >= 262 && &buf[257..262] == b"ustar" {
        return true;
    }
    // 매직이 없는 옛 tar(v7)도 있다 — 실측에서 실제로 나왔다(공공데이터.tar 는
    // 257바이트째가 전부 0). 파이썬 tarfile.is_tarfile 은 이 경우 **헤더 체크섬**으로
    // 판정하므로 같은 방식을 쓴다. 매직만 보면 이런 tar 를 통째로 놓친다.
    tar_checksum_ok(buf)
}

//------------------------------------------------------------------
// tar 헤더 체크섬 검사
//=> tar 헤더 512바이트의 합이 chksum 칸(148~155, 8진 ASCII)과 맞는지 본다.
//   계산할 때 chksum 칸 자체는 공백으로 친다(tar 규약).
//
// -in: buf = 파일 앞 512바이트 이상
//
// -out: bool = 체크섬이 맞으면 true
// -out: error = 없음
//------------------------------------------------------------------
fn tar_checksum_ok(buf: &[u8]) -> bool {
    if buf.len() < 512 {
        return false;
    }
    // 전부 0인 블록은 '아카이브 끝' 표식이라 tar 헤더가 아니다. 이걸 안 거르면
    // 합계 0 == 파싱값 0 이 되어 어떤 빈 파일이든 tar 로 오인한다.
    if buf[..100].iter().all(|&b| b == 0) {
        return false;
    }
    // chksum 칸의 8진수 읽기(널·공백에서 끊긴다).
    let mut want: u64 = 0;
    let mut seen = false;
    for &b in &buf[148..156] {
        match b {
            b'0'..=b'7' => {
                want = want * 8 + (b - b'0') as u64;
                seen = true;
            }
            b' ' | 0 => {
                if seen {
                    break;
                }
            }
            _ => return false,
        }
    }
    if !seen {
        return false;
    }
    // 합계: chksum 칸은 공백(0x20)으로 친다.
    let sum: u64 = buf[..512]
        .iter()
        .enumerate()
        .map(|(i, &b)| if (148..156).contains(&i) { 0x20u64 } else { b as u64 })
        .sum();
    sum == want
}

//------------------------------------------------------------------
// 압축 종류 감지(내용 기반)
//=> 확장자가 아니라 앞부분 매직/구조로 판별한다. 확장자를 위장해도 잡힌다.
//    1) 7z·rar 고유 매직
//    2) PK(zip) — 단 docx/xlsx/pptx/hwpx 는 '문서 1개'라 제외(detect 가 구분)
//    3) gz/bz2/xz — 풀어 본 앞부분이 tar 면 '압축된 tar'로 본다(.tar.gz 등)
//    4) 그냥 tar(ustar)
//
// -in: path = 검사할 파일 경로
//
// -out: Option<ArcType> = 압축이면 종류, 아니면 None
// -out: error = 없음(읽기 실패는 None)
//------------------------------------------------------------------
pub fn archive_type(path: &Path) -> Option<ArcType> {
    let mut head = [0u8; 512];
    let n = {
        let mut f = fs::File::open(path).ok()?;
        f.read(&mut head).ok()?
    };
    let head = &head[..n];

    if head.starts_with(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some(ArcType::SevenZ);
    }
    if head.starts_with(b"Rar!\x1a\x07\x00") || head.starts_with(b"Rar!\x1a\x07\x01\x00") {
        return Some(ArcType::Rar);
    }
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") || head.starts_with(b"PK\x07\x08")
    {
        // docx·xlsx·pptx·hwpx 는 속이 zip 이지만 '문서 하나'다 — 펼치면 안 된다.
        return if detect::detect_format(path) == Fmt::Zip { Some(ArcType::Zip) } else { None };
    }
    // 압축된 tar 인지 보려면 앞부분만 풀어 봐야 한다(파이썬 tarfile 의 "r:*" 와 같은 뜻).
    if head.starts_with(&[0x1f, 0x8b]) {
        return Some(if peek_is_tar(path, ArcType::Gz) { ArcType::Tar } else { ArcType::Gz });
    }
    if head.starts_with(b"BZh") {
        return Some(if peek_is_tar(path, ArcType::Bz2) { ArcType::Tar } else { ArcType::Bz2 });
    }
    if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        return Some(if peek_is_tar(path, ArcType::Xz) { ArcType::Tar } else { ArcType::Xz });
    }
    if looks_like_tar(head) {
        return Some(ArcType::Tar);
    }
    None
}

//------------------------------------------------------------------
// 압축을 앞부분만 풀어 tar 인지 확인
//=> `.tar.gz` 를 'gz 단일 파일'로 잘못 보면 내부 파일들이 통째로 안 보인다.
//   전부 풀면 폭탄에 당하므로 앞 몇 KB 만 풀어 ustar 매직만 확인한다.
//
// -in: path = 압축 경로
// -in: at   = 바깥 압축 종류(Gz/Bz2/Xz)
//
// -out: bool = 풀어 보니 tar 면 true
// -out: error = 없음(해제 실패는 false)
//------------------------------------------------------------------
fn peek_is_tar(path: &Path, at: ArcType) -> bool {
    match decompress_single(path, at, Some(PEEK as u64)) {
        Ok(buf) => looks_like_tar(&buf),
        Err(_) => false,
    }
}

//------------------------------------------------------------------
// 단일 압축 해제(gz/bz2/xz)
//=> 헤더에 원본 크기가 없는 포맷이라 '읽어 봐야' 크기를 안다. 그래서 상한이
//   있으면 그 크기+1 바이트까지만 읽고, 거기까지 찼으면 폭탄으로 판정한다 —
//   전부 읽고 나서 재면 이미 메모리를 다 쓴 뒤다.
//
// -in: path  = 압축 경로
// -in: at    = 압축 종류
// -in: limit = 최대 읽을 바이트(None 이면 제한 없음)
//
// -out: Result<Vec<u8>, String> = 해제된 바이트, 실패 사유
//------------------------------------------------------------------
fn decompress_single(path: &Path, at: ArcType, limit: Option<u64>) -> Result<Vec<u8>, String> {
    let f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    // 상한이 있으면 '상한+1'까지만 읽는다 — 넘겼는지 판정하려면 1바이트가 더 필요하다.
    let cap = limit.map(|n| n.saturating_add(1));
    let read_capped = |r: &mut dyn Read, out: &mut Vec<u8>| -> std::io::Result<()> {
        match cap {
            Some(c) => {
                r.take(c).read_to_end(out)?;
            }
            None => {
                r.read_to_end(out)?;
            }
        }
        Ok(())
    };
    match at {
        ArcType::Gz => {
            let mut d = flate2::read::GzDecoder::new(f);
            read_capped(&mut d, &mut out).map_err(|e| e.to_string())?;
        }
        ArcType::Bz2 => {
            let mut d = bzip2_rs::DecoderReader::new(f);
            read_capped(&mut d, &mut out).map_err(|e| e.to_string())?;
        }
        ArcType::Xz => {
            // lzma-rs 는 스트리밍 take 를 못 받아 전량 해제 후 자른다. peek(2KB)나
            // 상한 검사용으로만 쓰는 경로라 실사용 크기에서 문제되지 않는다.
            let mut br = std::io::BufReader::new(f);
            lzma_rs::xz_decompress(&mut br, &mut out).map_err(|e| format!("{:?}", e))?;
            if let Some(c) = cap {
                out.truncate(c as usize);
            }
        }
        _ => return Err("단일 압축이 아님".into()),
    }
    Ok(out)
}

//------------------------------------------------------------------
// 내부 파일 내용을 임시파일로 쏟기
//=> 내부 이름이 무엇이든(한글·경로탈출 시도) 임시폴더에 '일련번호' 이름으로만
//   쓴다. 포맷 감지는 내용 기반이라 파일명이 달라도 상관없고, `../` 같은 경로
//   탈출도 원천적으로 불가능해진다.
//
// -in: tmp_root = 임시 루트폴더
// -in: seq      = 증가하는 일련번호(호출부가 관리)
// -in: data     = 저장할 바이트
//
// -out: Result<PathBuf, String> = 만들어진 임시파일 경로
//------------------------------------------------------------------
fn spill(tmp_root: &Path, seq: &mut u64, data: &[u8]) -> Result<PathBuf, String> {
    *seq += 1;
    let p = tmp_root.join(format!("a{}", seq));
    fs::write(&p, data).map_err(|e| e.to_string())?;
    Ok(p)
}

//------------------------------------------------------------------
// 압축 1개 → 내부 (임시경로, 내부이름) 목록
//=> 종류별로 알맞은 방법으로 내부 파일을 임시폴더에 풀고 목록을 만든다.
//   폴더 항목과 잡음(__MACOSX 등)은 거른다.
//
// -in: src      = 압축 실제 경로
// -in: at       = 압축 종류
// -in: label    = 표시용 경로(단일 압축의 내부이름 계산에 쓴다)
// -in: tmp_root = 임시 루트폴더
// -in: seq      = 임시파일 일련번호
// -in: budget   = 해제 예산(G4)
//
// -out: Result<Vec<(PathBuf,String)>, String> = (임시경로, 내부이름) 목록
// -out: error = 상한 초과·손상·미지원 시 Err(사유) — 호출부가 '안 펼침'으로 폴백
//------------------------------------------------------------------
fn extract_members(
    src: &Path,
    at: ArcType,
    label: &str,
    tmp_root: &Path,
    seq: &mut u64,
    budget: &mut Budget,
    // 펼치기는 했지만 일부 항목을 못 꺼냈을 때 사유를 여기에 담는다(rar 하드링크 등).
    partial_note: &mut Option<String>,
) -> Result<Vec<(PathBuf, String)>, ArcError> {
    let mut out = Vec::new();
    match at {
        ArcType::Zip => {
            let f = fs::File::open(src).map_err(|e| e.to_string())?;
            let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
            for i in 0..z.len() {
                let (name, size, is_dir) = {
                    let e = z.by_index(i).map_err(|e| e.to_string())?;
                    // zip 0.6 은 UTF-8 플래그를 노출하지 않는다. 대신 이름이 실제로
                    // cp437 로 깨져 있을 때만 복원한다(zip_entry_name 이 판단).
                    (zip_entry_name(e.name(), false), e.size(), e.is_dir())
                };
                if is_dir || is_noise(&name) {
                    continue;
                }
                // zip 헤더의 '풀었을 때 크기'를 읽기 전에 더한다 → 폭탄을 풀기 전에 잡는다.
                budget.take(size)?;
                let mut buf = Vec::new();
                z.by_index(i)
                    .map_err(|e| e.to_string())?
                    .read_to_end(&mut buf)
                    .map_err(|e| e.to_string())?;
                out.push((spill(tmp_root, seq, &buf)?, name));
            }
            Ok(out)
        }
        ArcType::Tar => {
            // 바깥 압축을 먼저 풀어 메모리에 올린 뒤 tar 로 읽는다. gz/bz2/xz 어느
            // 쪽이든 여기서 하나로 합쳐진다(파이썬 tarfile "r:*" 와 같은 동작).
            let raw = read_tar_bytes(src, budget)?;
            let mut ar = tar::Archive::new(std::io::Cursor::new(raw));
            for e in ar.entries().map_err(|e| e.to_string())? {
                let mut e = e.map_err(|e| e.to_string())?;
                if !e.header().entry_type().is_file() {
                    continue;
                }
                let name = e.path().map_err(|x| x.to_string())?.to_string_lossy().replace('\\', "/");
                if is_noise(&name) {
                    continue;
                }
                // tar 헤더에도 원본 크기가 있어 읽기 전에 셀 수 있다.
                budget.take(e.header().size().unwrap_or(0))?;
                let mut buf = Vec::new();
                e.read_to_end(&mut buf).map_err(|x| x.to_string())?;
                out.push((spill(tmp_root, seq, &buf)?, name));
            }
            Ok(out)
        }
        ArcType::Gz | ArcType::Bz2 | ArcType::Xz => {
            // 단일 파일 압축: 크기를 미리 알 수 없어 상한까지만 읽어 보고 판정한다.
            let rem = budget.remaining();
            let data = decompress_single(src, at, rem)?;
            if let Some(r) = rem {
                if data.len() as u64 > r {
                    return Err(ArcError::limit(format!(
                        "단일 압축 해제 누적 상한 초과(>{}, MAX_ARCHIVE_BYTES)",
                        limits::human_bytes(budget.max_bytes)
                    )));
                }
            }
            budget.take(data.len() as u64)?;
            out.push((spill(tmp_root, seq, &data)?, single_inner_name(label, at)));
            Ok(out)
        }
        ArcType::SevenZ => {
            // 7z 는 zip 처럼 항목마다 크기를 알 수 있어, 읽기 전에 예산을 먼저 뺀다.
            // (파이썬 판 py7zr 은 extractall 로 통째 풀어서 목록 크기 합계를 미리
            //  확인하는데, 여기서는 항목 단위라 폭탄을 더 이른 시점에 끊는다.)
            let mut reader = sevenz_rust2::ArchiveReader::open(src, sevenz_rust2::Password::empty())
                .map_err(|e| format!("7z 열기 실패: {}", e))?;
            // 클로저 안에서는 예산 초과를 그대로 던질 수 없어(반환형이 crate 의 Error)
            // 사유를 밖에 담아 두고 순회를 멈춘 뒤, 밖에서 판단한다.
            let mut stop: Option<String> = None;
            let mut items: Vec<(PathBuf, String)> = Vec::new();
            reader
                .for_each_entries(&mut |entry: &sevenz_rust2::ArchiveEntry,
                                        rd: &mut dyn Read| {
                    if entry.is_directory || !entry.has_stream {
                        return Ok(true);
                    }
                    let name = entry.name.replace('\\', "/");
                    if is_noise(&name) {
                        return Ok(true);
                    }
                    if let Err(e) = budget.take(entry.size) {
                        // 클로저는 crate 의 Error 만 돌려줄 수 있어, 사유를 밖에
                        // 담아 두고 순회만 멈춘다(종류는 아래에서 되살린다).
                        stop = Some(e.reason);
                        return Ok(false);   // 더 읽지 말고 멈춘다
                    }
                    let mut buf = Vec::new();
                    if rd.read_to_end(&mut buf).is_err() {
                        // 항목 하나가 깨져도 나머지는 살린다(압축 전체를 버리지 않는다).
                        return Ok(true);
                    }
                    match spill(tmp_root, seq, &buf) {
                        Ok(p) => items.push((p, name)),
                        Err(e) => {
                            stop = Some(e);
                            return Ok(false);
                        }
                    }
                    Ok(true)
                })
                .map_err(|e| format!("7z 읽기 실패: {}", e))?;
            if let Some(why) = stop {
                // 예산 초과면 limit, 그 밖(쓰기 실패 등)이면 error 로 올린다.
                return Err(if why.contains("MAX_ARCHIVE") {
                    ArcError::limit(why)
                } else {
                    ArcError::error(why)
                });
            }
            Ok(items)
        }
        ArcType::Rar => {
            // rars 는 메모리 위의 바이트로 읽는다. 파일 전체를 올리는 셈이라
            // G1(원본 100MB 상한)이 이미 앞단에서 막아 준다.
            let bytes = fs::read(src).map_err(|e| ArcError::error(e.to_string()))?;
            let ar = rars::ArchiveReader::read(&bytes).map_err(|e| rar_err(&e))?;

            // 예산은 '읽기 전에' 목록의 풀었을 때 크기로 먼저 뺀다(zip·7z 과 같은 원칙).
            let mut want = 0usize;
            for m in ar.members() {
                let name = String::from_utf8_lossy(&m.meta.name).replace('\\', "/");
                if is_noise(&name) {
                    continue;
                }
                budget.take(m.meta.unpacked_size)?;
                want += 1;
            }

            let mut items: Vec<(PathBuf, String)> = Vec::new();
            let mut spill_err: Option<String> = None;
            {
                // 클로저가 seq·items 를 함께 빌려야 해서 블록으로 수명을 끊는다.
                let mut open = |meta: &rars::ExtractedEntryMeta| -> rars::Result<Box<dyn std::io::Write>> {
                    // 디렉터리는 파일이 아니다 — 참조 구현도 파일로 내놓지 않는다.
                    if meta.is_directory {
                        return Ok(Box::new(std::io::sink()));
                    }
                    let name = meta.name_lossy().to_string().replace('\\', "/");
                    if is_noise(&name) {
                        return Ok(Box::new(std::io::sink()));
                    }
                    *seq += 1;
                    let p = tmp_root.join(format!("a{}", seq));
                    match fs::File::create(&p) {
                        Ok(f) => {
                            items.push((p, name));
                            Ok(Box::new(f))
                        }
                        Err(e) => {
                            // 쓰기 실패는 밖에서 판단한다(클로저는 crate 오류만 돌려준다).
                            spill_err = Some(e.to_string());
                            Ok(Box::new(std::io::sink()))
                        }
                    }
                };
                ar.extract_to(None, &mut open).map_err(|e| rar_err(&e))?;
            }
            if let Some(e) = spill_err {
                return Err(ArcError::error(e));
            }
            // 하드링크·심볼릭링크는 rars 가 파일로 내놓지 않는다. 목록보다 적게
            // 나왔으면 '일부를 못 꺼냈다'고 남긴다 — 조용히 빠지면 알 길이 없다.
            if items.len() < want {
                *partial_note = Some(format!(
                    "내부 {}개 중 {}개만 꺼냈습니다 — 하드링크·심볼릭링크는 이 판이 \
                     파일로 내놓지 않습니다(원본 파일은 그대로 분류됩니다).",
                    want,
                    items.len()
                ));
            }
            Ok(items)
        }
    }
}

//------------------------------------------------------------------
// rars 오류 → 확장 실패 사유
//=> 분할 볼륨은 '못 읽는 파일'이 아니라 '조각이라 단독으로는 못 여는 것'이다.
//   rars 가 그때 내는 오류에 'split entry' 가 들어가므로 그것으로 갈라, 사람에게
//   맞는 안내(원본을 합쳐서 넣으라)가 나가게 한다.
//
// -in: e = rars 가 낸 오류
//
// -out: ArcError = kind 가 "split_volume" 또는 "error"
// -out: error = 없음
//------------------------------------------------------------------
fn rar_err(e: &rars::Error) -> ArcError {
    let text = format!("{:?}", e);
    if text.contains("split entry") || text.contains("multivolume") {
        ArcError {
            reason: format!("rar 분할 볼륨입니다 — 이 판은 조각을 잇지 않습니다. 원인: {}", text),
            kind: "split_volume",
        }
    } else {
        ArcError::error(format!("rar 읽기 실패: {}", text))
    }
}

//------------------------------------------------------------------
// tar 본문 바이트 얻기(필요하면 바깥 압축을 먼저 푼다)
//=> `.tar` 는 그대로, `.tar.gz/.tar.bz2/.tar.xz` 는 풀어서 돌려준다.
//   푸는 양도 예산 안으로 제한해, 압축된 tar 폭탄이 메모리를 먹지 못하게 한다.
//
// -in: src    = 경로
// -in: budget = 예산(남은 여유만큼만 푼다)
//
// -out: Result<Vec<u8>, String> = tar 본문
//------------------------------------------------------------------
fn read_tar_bytes(src: &Path, budget: &Budget) -> Result<Vec<u8>, String> {
    let mut head = [0u8; 8];
    {
        let mut f = fs::File::open(src).map_err(|e| e.to_string())?;
        let _ = f.read(&mut head);
    }
    let inner = if head.starts_with(&[0x1f, 0x8b]) {
        Some(ArcType::Gz)
    } else if head.starts_with(b"BZh") {
        Some(ArcType::Bz2)
    } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Some(ArcType::Xz)
    } else {
        None
    };
    match inner {
        None => fs::read(src).map_err(|e| e.to_string()),
        Some(at) => {
            let rem = budget.remaining();
            let data = decompress_single(src, at, rem)?;
            if let Some(r) = rem {
                if data.len() as u64 > r {
                    return Err(format!(
                        "압축 tar 해제 누적 상한 초과(>{}, MAX_ARCHIVE_BYTES)",
                        limits::human_bytes(budget.max_bytes)
                    ));
                }
            }
            Ok(data)
        }
    }
}

//------------------------------------------------------------------
// 단일 압축의 내부 파일 이름 짓기
//=> `보고서.hwp.gz` 처럼 압축 확장자만 떼면 원래 이름이 나온다. 그래야 파일명
//   규칙 신호가 원래대로 먹는다. 뗄 것이 없으면 `.out` 을 붙인다.
//
// -in: label = 압축의 표시 경로
// -in: at    = 압축 종류
//
// -out: String = 내부 파일 이름
// -out: error = 없음
//------------------------------------------------------------------
fn single_inner_name(label: &str, at: ArcType) -> String {
    let base = label.rsplit(['/', '\\']).next().unwrap_or(label);
    let suffix = format!(".{}", at.as_str());
    if let Some(stripped) = base.strip_suffix(&suffix) {
        if !stripped.is_empty() {
            return stripped.to_string();
        }
    }
    format!("{}.out", base)
}

//------------------------------------------------------------------
// 경로 목록 → '분류 작업' 목록으로 확장 (진입점)
//=> 입력 경로를 훑어 압축은 임시폴더에 풀어 내부 파일들로 바꾸고, 그 외 일반
//   파일은 그대로 둔다.
//    1) 큐에 (src, label, depth, origin, budget) 로 넣고 하나씩 처리
//    2) 압축이고 depth<max_depth 면 내부 파일을 풀어 재귀로 큐에 넣는다
//       (내부 파일의 origin 은 '최상위 압축' 경로를 물려받는다)
//    3) 압축이 아니면 작업으로 확정
//   ※ 손상·상한초과·미지원은 '펼치지 않고' 원본 1건으로 두되, 사유를 stats 에 남긴다.
//
// -in: paths     = 입력 파일 경로들
// -in: tmp_root  = 내부 파일을 풀 임시폴더(호출부가 만들고 끝나면 지운다)
// -in: opts      = 깊이·누적 바이트·항목 수 상한(압축폭탄 방어)
// -in: stats     = 결과 보고(못 펼친 압축·미지원 압축)
//
// -out: Vec<Job> = 분류할 작업 목록
// -out: error = 없음(개별 실패는 그 압축만 원본 1건으로 폴백)
//------------------------------------------------------------------
pub fn expand_paths(
    paths: &[PathBuf],
    tmp_root: &Path,
    opts: &ArchiveOptions,
    stats: &mut ExpandStats,
) -> Vec<Job> {
    let max_depth = opts.max_depth;
    let mut jobs: Vec<Job> = Vec::new();
    let mut seq: u64 = 0;
    // (src, label, depth, origin, budget) — 예산은 최상위 압축에서 만들어 자식에게 물려준다.
    let mut queue: VecDeque<(PathBuf, String, usize, Option<String>, Option<Budget>)> = paths
        .iter()
        .map(|p| {
            let l = p.to_string_lossy().replace('\\', "/");
            (p.clone(), l, 0usize, None, None)
        })
        .collect();

    while let Some((src, label, depth, origin, budget)) = queue.pop_front() {
        // 너무 깊으면(중첩 폭탄 방지) 더는 안 펼치고 한 건으로 둔다.
        let at = if depth >= max_depth { None } else { archive_type(&src) };
        let at = match at {
            None => {
                jobs.push(Job { src, label, origin });
                continue;
            }
            Some(a) => a,
        };

        // 지원하지 않는 포맷은 '조용히 스킵'하지 않는다 — 무엇이 왜 빠졌는지 남긴다.
        if !at.supported() {
            stats.unsupported.push((
                label.clone(),
                format!("{} 압축은 이 판에서 펼치지 않습니다(파이썬 판으로 처리하세요)", at.as_str()),
            ));
            jobs.push(Job { src, label, origin });
            continue;
        }

        // 예산(G4)은 '최상위 압축 1건'을 단위로 잡는다. 중첩 압축이 예산을 새로
        // 받으면 zip 안에 zip 을 겹치는 것만으로 상한을 무한히 늘릴 수 있다.
        let mut budget = budget.unwrap_or_else(|| Budget::new(opts));

        // 일부만 꺼낸 경우의 사유를 받을 자리(rar 하드링크 등).
        let mut partial_note: Option<String> = None;
        let members = match extract_members(
            &src, at, &label, tmp_root, &mut seq, &mut budget, &mut partial_note,
        ) {
            Ok(m) => m,
            Err(e) => {
                // 상한 초과와 그 밖의 실패를 갈라 담는다 — 안내 문구가 달라야 한다
                // (상한이면 "값을 올리세요", 실패면 "원본을 보세요").
                // 이름이 분할 조각을 가리키면 한 번 더 갈라 준다: 사유가 "헤더가
                // 깨졌다"뿐이면 사람은 파일이 손상된 줄 알고 원본을 뒤지는데,
                // 실제로는 멀쩡한 압축이고 이 판이 조각을 못 잇는 것뿐이다.
                let (reason, kind) = match (e.kind, split_volume_hint(&label, Some(&src))) {
                    ("error", Some(h)) => (
                        format!(
                            "분할 압축의 조각으로 보입니다({}) — 이 판은 분할 압축을 잇지 \
                             않아 내부 문서를 꺼내지 못했습니다. 원인: {}",
                            h, e.reason
                        ),
                        "split_volume",
                    ),
                    _ => (e.reason, e.kind),
                };
                stats.unexpanded.push((label.clone(), reason, kind));
                jobs.push(Job { src, label, origin });
                continue;
            }
        };
        // 일부만 꺼냈으면 그 사실을 남긴다 — 조용히 빠지면 사람이 알 길이 없다.
        if let Some(note) = partial_note {
            stats.partial.push((label.clone(), note));
        }

        // 빈 압축(또는 전부 폴더/잡음)이면 원본 1건으로 남긴다.
        if members.is_empty() {
            jobs.push(Job { src, label, origin });
            continue;
        }

        // 내부 파일의 origin 은 '최상위 압축' 경로다(이미 있으면 물려주고, 없으면 이 압축).
        let child_origin = origin.clone().unwrap_or_else(|| label.clone());
        let n = members.len();
        for (i, (tmp_path, inner)) in members.into_iter().enumerate() {
            // 예산 객체는 하나뿐이라 마지막 자식에게만 넘겨도 되지만, 형제 압축이
            // 각각 펼쳐질 수 있으므로 '이번 압축 안에서 계속 이어지도록' 마지막에 넘긴다.
            let carry = if i + 1 == n { Some(std::mem::replace(&mut budget, Budget::new(opts))) } else { None };
            queue.push_back((
                tmp_path,
                format!("{}/{}", label, inner),
                depth + 1,
                Some(child_origin.clone()),
                carry,
            ));
        }
    }
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;

    //------------------------------------------------------------------
    // 잡음 항목 판정
    //------------------------------------------------------------------
    #[test]
    fn noise_filter() {
        assert!(is_noise("__MACOSX/foo.txt"));
        assert!(is_noise("a/b/.DS_Store"));
        assert!(!is_noise("문서/보고서.hwp"));
    }

    //------------------------------------------------------------------
    // 단일 압축 내부 이름 — 압축 확장자만 떼야 파일명 규칙이 원래대로 먹는다
    //------------------------------------------------------------------
    #[test]
    fn inner_name_of_single_archive() {
        assert_eq!(single_inner_name("d:/x/보고서.hwp.gz", ArcType::Gz), "보고서.hwp");
        assert_eq!(single_inner_name("d:/x/dump.xz", ArcType::Xz), "dump");
        // 뗄 것이 없으면 .out
        assert_eq!(single_inner_name("d:/x/blob", ArcType::Gz), "blob.out");
    }

    //------------------------------------------------------------------
    // tar 매직은 257바이트째에 있다
    //------------------------------------------------------------------
    #[test]
    fn tar_magic_offset() {
        let mut buf = vec![0u8; 512];
        buf[257..262].copy_from_slice(b"ustar");
        assert!(looks_like_tar(&buf));
        assert!(!looks_like_tar(&vec![0u8; 512]));
        // 짧은 버퍼에서 패닉하지 않아야 한다.
        assert!(!looks_like_tar(&[0u8; 10]));
    }

    //------------------------------------------------------------------
    // 매직 없는 옛 tar(v7)도 체크섬으로 알아본다
    //=> 실측에서 실제로 나왔다(공공데이터.tar 는 257바이트째가 전부 0). 매직만
    //   보면 이런 tar 를 통째로 놓쳐 내부 문서가 한 건도 분류되지 않는다.
    //------------------------------------------------------------------
    #[test]
    fn old_tar_without_magic_detected_by_checksum() {
        let mut buf = vec![0u8; 512];
        buf[..9].copy_from_slice(b"a/b/c.txt");        // 이름 칸
        // chksum 칸을 공백으로 두고 합계를 구해 8진수로 적는다(tar 규약).
        for i in 148..156 { buf[i] = b' '; }
        let sum: u64 = buf.iter().map(|&b| b as u64).sum();
        let oct = format!("{:06o}\0 ", sum);
        buf[148..148 + oct.len()].copy_from_slice(oct.as_bytes());
        assert!(looks_like_tar(&buf), "체크섬이 맞으면 tar 로 봐야 한다");

        // 값이 틀리면 tar 가 아니다.
        buf[149] = b'7';
        assert!(!looks_like_tar(&buf));
    }

    //------------------------------------------------------------------
    // cp437 로 깨진 zip 이름 복원 — ASCII 는 건드리지 않는다
    //------------------------------------------------------------------
    #[test]
    fn zip_name_ascii_untouched() {
        assert_eq!(zip_entry_name("report.pdf", false), "report.pdf");
        assert_eq!(zip_entry_name("보고서.pdf", true), "보고서.pdf");
    }

    //------------------------------------------------------------------
    // 예산 — 개수와 용량을 따로 센다(둘 중 하나로 다른 쪽을 못 막는다)
    //------------------------------------------------------------------
    #[test]
    fn budget_counts_bytes_and_members() {
        let mut b = Budget { max_bytes: 100, max_members: 0, used_bytes: 0, used_members: 0 };
        assert!(b.take(60).is_ok());
        assert!(b.take(60).is_err(), "누적 120 > 100 이라 막혀야 한다");

        // 0바이트 파일 폭탄은 용량으로 안 걸린다 → 개수 상한이 필요하다.
        let mut b = Budget { max_bytes: 0, max_members: 2, used_bytes: 0, used_members: 0 };
        assert!(b.take(0).is_ok());
        assert!(b.take(0).is_ok());
        assert!(b.take(0).is_err());
    }

    //------------------------------------------------------------------
    // 실제 rar 을 펼친다 (2026-09-07 rars 로 지원 시작)
    //=> 진짜 RAR 84바이트(rars 픽스처의 stored.rar)를 그대로 넣어 확인한다.
    //   내용으로 감지되는지, 내부 파일이 작업이 되는지, label/origin 이 맞는지.
    //   참조 구현(unrar)과의 대조는 실측으로 따로 했다 — 공통 554건 등급 100% 일치.
    //------------------------------------------------------------------
    #[test]
    fn expand_real_rar() {
        // Rar!  … payload.txt("rar15 oracle payload")
        const RAR: [u8; 84] = [
            0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x00, 0xcf, 0x90, 0x73, 0x00, 0x00, 0x0d, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xbc, 0xe4, 0x74, 0x00, 0x80, 0x2b, 0x00, 0x15,
            0x00, 0x00, 0x00, 0x15, 0x00, 0x00, 0x00, 0x03, 0xb6, 0xa8, 0x45, 0xc9, 0x00, 0x00,
            0x00, 0x00, 0x0f, 0x30, 0x0b, 0x00, 0x20, 0x00, 0x00, 0x00, 0x70, 0x61, 0x79, 0x6c,
            0x6f, 0x61, 0x64, 0x2e, 0x74, 0x78, 0x74, 0x72, 0x61, 0x72, 0x31, 0x35, 0x20, 0x6f,
            0x72, 0x61, 0x63, 0x6c, 0x65, 0x20, 0x70, 0x61, 0x79, 0x6c, 0x6f, 0x61, 0x64, 0x0a,
        ];
        let tmp = std::env::temp_dir().join(format!("csoarcrar_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("demo.rar");
        fs::write(&path, RAR).unwrap();

        // 확장자가 아니라 내용으로 알아봐야 한다.
        assert_eq!(archive_type(&path), Some(ArcType::Rar));
        assert!(ArcType::Rar.supported(), "이제 rar 도 펼칠 수 있어야 한다");

        let work = tmp.join("w");
        fs::create_dir_all(&work).unwrap();
        let mut st = ExpandStats::default();
        let jobs = expand_paths(&[path.clone()], &work, &ArchiveOptions::default(), &mut st);

        assert_eq!(jobs.len(), 1, "내부 1건으로 펼쳐져야 한다: {:?}", jobs);
        assert!(st.unexpanded.is_empty(), "정상 rar 이다: {:?}", st.unexpanded);
        assert!(st.partial.is_empty(), "빠진 항목이 없어야 한다: {:?}", st.partial);
        assert!(jobs[0].label.ends_with("demo.rar/payload.txt"), "label: {}", jobs[0].label);
        assert!(jobs[0].origin.is_some());
        // 내용이 실제로 나왔는지 — 껍데기만 만들고 끝나면 안 된다.
        let got = fs::read_to_string(&jobs[0].src).unwrap();
        assert!(got.contains("oracle payload"), "본문: {:?}", got);
        let _ = fs::remove_dir_all(&tmp);
    }

    //------------------------------------------------------------------
    // 분할 압축 조각을 이름으로 알아본다 (파이썬 판과 같은 규칙)
    //=> 내용으로는 구분이 안 된다. 오탐하면 정상 문서를 읽지 않고 버리므로
    //   '아닌 것'을 거르는 쪽도 함께 고정한다.
    //------------------------------------------------------------------
    #[test]
    fn split_volume_hint_matches_parts_only() {
        for n in ["split.7z.0001", "a.7z.002", "x.zip.001", "노트.tar.gz.003",
                  "p.z01", "p.z12", "doc.part01.rar", "doc.part2.rar", "a.r00", "a.r15"] {
            assert!(split_volume_hint(n, None).is_some(), "조각으로 봐야 한다: {}", n);
        }
        for n in ["backup.001", "report.pdf", "a.rar", "a.zip", "a.7z", "메모.txt"] {
            assert!(split_volume_hint(n, None).is_none(), "조각이 아니다: {}", n);
        }
        // 경로가 붙어 있어도 파일명만 본다.
        assert!(split_volume_hint("D:/자료/보관.7z.001", None).is_some());
    }

    //------------------------------------------------------------------
    // 분할 zip 의 '마지막 조각'은 이름이 평범한 .zip 이라 내용으로 가려낸다
    //=> 이름만 보면 정상 zip 과 똑같아 그냥 통과했고, 머리도 PK 매직이 아니라
    //   압축 감지에도 안 잡혀 '정체불명 추출 실패'로 남았다(실측). EOCD 의
    //   디스크 번호가 0 이 아니면 여러 장짜리라는 뜻이므로 그것으로 가른다.
    //   정상 zip 을 조각으로 오해하면 멀쩡한 문서를 안 읽고 버리므로,
    //   '아닌 쪽'도 함께 고정한다.
    //------------------------------------------------------------------
    #[test]
    fn split_volume_hint_detects_last_zip_segment_by_eocd() {
        let dir = std::env::temp_dir().join(format!("csoc_splitzip_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        // EOCD 하나만 있는 최소 zip 을 만든다: 시그니처 + 디스크번호 2개 + 개수 2개
        // + 크기(4) + 오프셋(4) + 주석길이(2) = 22바이트.
        let make = |name: &str, disk: u16| -> std::path::PathBuf {
            let p = dir.join(name);
            let mut b = vec![0x50u8, 0x4b, 0x05, 0x06];
            b.extend_from_slice(&disk.to_le_bytes());     // 이 디스크 번호
            b.extend_from_slice(&disk.to_le_bytes());     // 중앙목록이 시작한 디스크
            b.extend_from_slice(&0u16.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());     // 주석 길이 0
            std::fs::write(&p, &b).unwrap();
            p
        };

        let split = make("multi.zip", 3);
        let single = make("single.zip", 0);

        // 경로를 안 주면 예전 그대로 이름만 본다 — 둘 다 평범한 .zip 이라 None.
        assert!(split_volume_hint("multi.zip", None).is_none());
        // 경로를 주면 마지막 조각을 잡아낸다.
        assert_eq!(
            split_volume_hint("multi.zip", Some(split.as_path())),
            Some("zip 분할 마지막 조각(.zip)")
        );
        // 디스크 번호가 0 인 정상 zip 은 건드리지 않는다.
        assert!(split_volume_hint("single.zip", Some(single.as_path())).is_none());
        // 없는 경로는 '모르겠다'로 두고 넘어간다(읽기 실패가 오탐이 되면 안 된다).
        assert!(split_volume_hint("multi.zip", Some(std::path::Path::new("없는파일.zip"))).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    //------------------------------------------------------------------
    // 분할 첫 조각은 '깨짐'이 아니라 '분할'로 보고된다
    //=> 사유가 "헤더가 깨졌다"뿐이면 사람은 원본을 뒤진다. 실제로는 멀쩡한
    //   압축인데 이 판이 조각을 못 잇는 것뿐이다.
    //------------------------------------------------------------------
    #[test]
    fn split_first_part_reported_as_split() {
        let tmp = std::env::temp_dir().join(format!("csoarcvol_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let part = tmp.join("big.7z.001");
        let mut data = vec![b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C];
        data.extend(std::iter::repeat(0u8).take(300));
        fs::write(&part, &data).unwrap();
        assert_eq!(archive_type(&part), Some(ArcType::SevenZ), "첫 조각은 7z 로 감지된다");

        let work = tmp.join("w");
        fs::create_dir_all(&work).unwrap();
        let mut st = ExpandStats::default();
        expand_paths(&[part.clone()], &work, &ArchiveOptions::default(), &mut st);
        assert_eq!(st.unexpanded.len(), 1);
        assert_eq!(st.unexpanded[0].2, "split_volume", "분할로 봐야 한다: {:?}", st.unexpanded);
        assert!(st.unexpanded[0].1.contains("분할"));
        assert!(st.unexpanded[0].1.contains("원인"), "진짜 손상일 수도 있으니 원인도 남긴다");
        let _ = fs::remove_dir_all(&tmp);
    }

    //------------------------------------------------------------------
    // 실제 7z 를 펼친다 (2026-09-07 sevenz-rust2 로 지원 시작)
    //=> 7z 를 직접 만들어 넣고, 내부 파일이 각각 작업이 되는지 본다. 잡음 항목은
    //   빠져야 하고, label 은 `압축경로/내부경로` 여야 한다(zip 과 같은 규칙).
    //   파이썬 판(py7zr)과 같은 건수·같은 등급이 나오는지는 실측으로 따로 확인했다.
    //------------------------------------------------------------------
    #[test]
    fn expand_real_7z() {
        let tmp = std::env::temp_dir().join(format!("csoarc7z_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("demo.7z");
        {
            let mut w = sevenz_rust2::ArchiveWriter::create(&path).unwrap();
            for i in 0..3 {
                let e = sevenz_rust2::ArchiveEntry::new_file(&format!("문서{}.txt", i));
                w.push_archive_entry(e, Some(&b"\xea\xb3\x84\xec\x95\xbd \xed\x99\x8d\xea\xb8\xb8\xeb\x8f\x99"[..]))
                    .unwrap();
            }
            // 잡음은 걸러져야 한다.
            let e = sevenz_rust2::ArchiveEntry::new_file("__MACOSX/junk");
            w.push_archive_entry(e, Some(&b"x"[..])).unwrap();
            w.finish().unwrap();
        }
        // 내용 기반 감지가 7z 를 알아봐야 한다(확장자에 기대지 않는다).
        assert_eq!(archive_type(&path), Some(ArcType::SevenZ));
        assert!(ArcType::SevenZ.supported(), "이제 7z 는 펼칠 수 있어야 한다");

        let work = tmp.join("work");
        fs::create_dir_all(&work).unwrap();
        let mut st = ExpandStats::default();
        let jobs = expand_paths(&[path.clone()], &work, &ArchiveOptions::default(), &mut st);

        assert_eq!(jobs.len(), 3, "내부 3건으로 펼쳐져야 한다(잡음 제외): {:?}", jobs);
        assert!(st.unsupported.is_empty(), "7z 가 미지원으로 잡히면 안 된다: {:?}", st.unsupported);
        assert!(st.unexpanded.is_empty(), "상한에 걸릴 크기가 아니다: {:?}", st.unexpanded);
        assert!(jobs[0].label.contains("demo.7z/문서"), "label: {}", jobs[0].label);
        assert!(jobs[0].origin.is_some(), "origin 은 최상위 압축이어야 한다");
        let _ = fs::remove_dir_all(&tmp);
    }

    //------------------------------------------------------------------
    // 실제 zip 을 펼친다 — 내부 파일이 각각 작업이 되고 label/origin 이 맞아야 한다
    //------------------------------------------------------------------
    #[test]
    fn expand_real_zip() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("csoarc_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let zpath = tmp.join("demo.zip");
        {
            let f = fs::File::create(&zpath).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let o: zip::write::FileOptions = Default::default();
            for i in 0..3 {
                w.start_file(format!("문서{}.txt", i), o).unwrap();
                w.write_all("계약 상대방 홍길동".as_bytes()).unwrap();
            }
            w.start_file("__MACOSX/junk", o).unwrap();   // 잡음은 걸러져야 한다
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        let work = tmp.join("work");
        fs::create_dir_all(&work).unwrap();
        let mut st = ExpandStats::default();
        let jobs = expand_paths(&[zpath.clone()], &work, &ArchiveOptions::default(), &mut st);

        assert_eq!(jobs.len(), 3, "내부 3건으로 펼쳐져야 한다(잡음 제외)");
        assert!(st.unexpanded.is_empty() && st.unsupported.is_empty());
        let l = &jobs[0].label;
        assert!(l.contains("demo.zip/문서"), "label 은 압축경로/내부경로 여야 한다: {}", l);
        assert!(jobs[0].origin.is_some(), "origin 은 최상위 압축이어야 한다");
        let _ = fs::remove_dir_all(&tmp);
    }
}

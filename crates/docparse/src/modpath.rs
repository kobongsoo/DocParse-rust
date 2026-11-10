//! 이 코드가 들어 있는 모듈(DLL·.so·실행파일) 자신의 폴더 찾기
//!
//! docparse 가 공유 라이브러리(docparse.dll / libdocparse.so)로 불려 쓰일 때,
//! `std::env::current_exe()` 는 부른 프로그램(예: python.exe)을 가리킨다.
//! 같이 배포한 pdfium 은 docparse 라이브러리 옆에 있으므로, 라이브러리 자신의
//! 위치를 알아야 찾을 수 있다. 외부 크레이트 없이 OS 함수 두어 개만 직접 부른다.

use std::path::PathBuf;

//------------------------------------------------------------------
// 이 모듈 안의 아무 주소 하나 — "이 주소를 품은 모듈" 을 묻는 데 쓴다
//=> 함수 자체의 주소면 충분하다. 내용은 아무 일도 하지 않는다.
//
// -in: 없음
//
// -out: 없음
// -out: error = 없음
//------------------------------------------------------------------
#[inline(never)]
fn anchor() {}

//------------------------------------------------------------------
// 이 모듈 파일이 있는 폴더 (Windows)
//=> GetModuleHandleExW(주소로 찾기) 로 이 코드를 품은 모듈 핸들을 얻고,
//   GetModuleFileNameW 로 그 파일 경로를 읽어 부모 폴더를 돌려준다.
//    1) 참조 횟수를 올리지 않는 플래그(UNCHANGED_REFCOUNT)로 핸들만 빌린다
//    2) 경로가 버퍼보다 길면 버퍼를 늘려 다시 읽는다(260자 넘는 경로 대비)
//
// -in: 없음
//
// -out: Option<PathBuf> = 모듈 폴더. 못 구하면 None
// -out: error = 없음(실패는 None)
//------------------------------------------------------------------
#[cfg(windows)]
pub(crate) fn module_dir() -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    type Hmodule = *mut core::ffi::c_void;
    const FROM_ADDRESS: u32 = 0x4;
    const UNCHANGED_REFCOUNT: u32 = 0x2;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut Hmodule) -> i32;
        fn GetModuleFileNameW(module: Hmodule, buf: *mut u16, size: u32) -> u32;
    }

    let mut h: Hmodule = std::ptr::null_mut();
    // 함수 주소를 '모듈 이름' 자리에 넘기는 것이 FROM_ADDRESS 의 약속이다.
    let ok = unsafe {
        GetModuleHandleExW(FROM_ADDRESS | UNCHANGED_REFCOUNT, anchor as *const () as *const u16, &mut h)
    };
    if ok == 0 || h.is_null() {
        return None;
    }
    let mut cap = 512usize;
    loop {
        let mut buf = vec![0u16; cap];
        let n = unsafe { GetModuleFileNameW(h, buf.as_mut_ptr(), cap as u32) } as usize;
        if n == 0 {
            return None;
        }
        // 버퍼가 꽉 찼으면 잘린 것일 수 있다 — 두 배로 늘려 다시 읽는다.
        if n >= cap && cap < 32_768 {
            cap *= 2;
            continue;
        }
        let p = PathBuf::from(OsString::from_wide(&buf[..n]));
        return p.parent().map(|d| d.to_path_buf());
    }
}

//------------------------------------------------------------------
// 이 모듈 파일이 있는 폴더 (Linux 등 유닉스)
//=> dladdr 로 이 코드 주소를 품은 공유 객체의 경로(dli_fname)를 얻는다.
//   실행파일에 정적으로 링크된 경우에는 실행파일 경로가 나온다.
//
// -in: 없음
//
// -out: Option<PathBuf> = 모듈 폴더. 못 구하면 None
// -out: error = 없음(실패는 None)
//------------------------------------------------------------------
#[cfg(unix)]
pub(crate) fn module_dir() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_int, c_void};
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    struct DlInfo {
        dli_fname: *const c_char,
        dli_fbase: *mut c_void,
        dli_sname: *const c_char,
        dli_saddr: *mut c_void,
    }
    extern "C" {
        fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
    }

    let mut info = DlInfo {
        dli_fname: std::ptr::null(),
        dli_fbase: std::ptr::null_mut(),
        dli_sname: std::ptr::null(),
        dli_saddr: std::ptr::null_mut(),
    };
    let ok = unsafe { dladdr(anchor as *const () as *const c_void, &mut info) };
    if ok == 0 || info.dli_fname.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(info.dli_fname) };
    let p = std::path::Path::new(std::ffi::OsStr::from_bytes(name.to_bytes()));
    // 상대 경로로 나오는 경우(드물다)는 현재 폴더 기준으로 바꾼다.
    let p = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().ok()?.join(p) };
    p.parent().map(|d| d.to_path_buf())
}

//------------------------------------------------------------------
// 그 밖의 OS — 모듈 폴더를 구하지 않는다
//
// -in: 없음
//
// -out: Option<PathBuf> = 항상 None
// -out: error = 없음
//------------------------------------------------------------------
#[cfg(not(any(windows, unix)))]
pub(crate) fn module_dir() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    //------------------------------------------------------------------
    // 시험: 모듈 폴더가 실제로 있는 폴더로 나온다
    //=> 시험 실행파일에 정적으로 들어 있으므로 시험 실행파일 폴더와 같아야 한다.
    //
    // -in: 없음
    //
    // -out: 없음
    // -out: error = 다르면 assert 실패
    //------------------------------------------------------------------
    #[test]
    fn 모듈_폴더는_실행파일_폴더와_같다() {
        let d = super::module_dir().expect("모듈 폴더");
        let e = std::env::current_exe().unwrap();
        assert_eq!(d.canonicalize().unwrap(), e.parent().unwrap().canonicalize().unwrap());
    }
}

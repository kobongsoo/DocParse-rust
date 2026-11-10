//! 사이냅 문서필터(snf_exe) 추출기 — 옵션으로 고를 때만 쓴다.
//!
//! 설계: plan/Rust-사이냅추출기-옵션-설계-20261019.html §3·§5
//! 파이썬 판 `extract/synap_exe.py` · `resources.synap_exe_path` 와 **같은 계약**이다
//! (찾기 순서 · 호출 인자 · 성공 판별 · 제한 시간 · 실패 문장). 한쪽만 고치면 같은
//! 문서에서 두 판의 본문과 실패 사유가 갈린다.
//!
//! 이 라이브러리의 기본 엔진은 자체 파서다. 사이냅은 `Engine::SynapFallback` ·
//! `Engine::SynapOnly` 로 명시할 때만 쓴다. snf_exe 를 '어디서 찾을지'(배포 폴더·
//! 환경변수)는 쓰는 프로그램이 정해 경로로 넘긴다.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// snf_exe 한 건의 기본 제한 시간(초). 파이썬 `config.SNF_TIMEOUT` 과 같은 값이다.
/// 자체 파서의 --parser-timeout 과 따로 둔다(파이썬도 따로 둔다).
pub const SNF_TIMEOUT_SECS: u64 = 60;


//------------------------------------------------------------------
// 사이냅 출력파일을 둘 폴더
//=> 복호화로 풀린 문서(경로 위쪽에 cso_dec_<pid> 폴더가 있다)면 그 문서 전용 폴더에 둔다 —
//   풀린 문서의 본문이 복호화 임시 폴더 밖(다른 계정도 보는 /tmp 등)에 놓이지 않게.
//   그 폴더는 문서가 끝나면 통째로 지워진다. 그 밖은 시스템 임시 폴더.
//
// -in: input = 추출할 문서 경로
//
// -out: PathBuf = 출력파일을 만들 폴더
// -out: error = 없음
//------------------------------------------------------------------
fn out_dir_for(input: &Path) -> PathBuf {
    let in_dec = input.ancestors().skip(1).any(|a| {
        a.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("cso_dec_"))
    });
    match input.parent() {
        Some(p) if in_dec => p.to_path_buf(),
        _ => std::env::temp_dir(),
    }
}

//------------------------------------------------------------------
// snf_exe 가 남긴 말 → 글자
//=> UTF-8 로 먼저 읽고, 안 되면 cp949(윈도우 한글)로 읽는다(파이썬 _decode 와 같다).
//
// -in: raw = 프로세스 출력 바이트
//
// -out: String = 읽은 글자(앞뒤 공백 제거)
// -out: error = 없음
//------------------------------------------------------------------
fn decode_msg(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.trim().to_string(),
        Err(_) => encoding_rs::EUC_KR.decode(raw).0.trim().to_string(),
    }
}

//------------------------------------------------------------------
// 글자를 앞에서 n 자까지 자르기(글자 경계 안전)
//
// -in: s = 원문 · n = 글자 수
//
// -out: String
// -out: error = 없음
//------------------------------------------------------------------
fn head_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

//------------------------------------------------------------------
// 출력파일을 조용히 지우기(없으면 넘어간다)
//
// -in: p = 지울 파일
//
// -out: 없음
// -out: error = 없음
//------------------------------------------------------------------
fn remove_quiet(p: &Path) {
    let _ = std::fs::remove_file(p);
}

//------------------------------------------------------------------
// 사이냅으로 본문 뽑기 — 파이썬 SynapExeExtractor.extract 와 같은 계약
//=> snf_exe -U8 -NO_WITHPAGE <입력> <출력> 을 셸 없이 부르고, 출력파일로 성공을 본다.
//    1) 출력파일 자리를 정한다(복호화 문서면 그 문서 폴더 안)
//    2) 실행하고, stdout·stderr 는 따로 읽어 둔다(가득 차서 멈추지 않게)
//    3) 제한 시간을 넘으면 끝내고 남은 출력파일을 지운다
//    4) 출력파일이 있고 0바이트가 아니면 성공 — 종료코드는 믿지 않는다(성공·실패 모두 0)
//    5) 실패면 rc · 출력파일 상태 · stdout · stderr 를 파이썬과 같은 모양으로 담는다
//
// -in: exe     = snf_exe 경로
// -in: input   = 추출할 문서
// -in: timeout = 제한 시간(초, 0 이면 제한 없음)
//
// -out: Ok(String) = 뽑은 원시 텍스트(UTF-8, 깨진 바이트는 대체 문자)
// -out: error = Err(사유) — "snf_exe 타임아웃(60s)" · "rc=0; 출력파일 미생성; stdout=…" 등
//------------------------------------------------------------------
pub fn snf_extract(exe: &Path, input: &Path, timeout: u64) -> Result<String, String> {
    if !input.is_file() {
        return Err("입력 파일 없음".into());
    }
    let mut rnd = [0u8; 8];
    let _ = getrandom::fill(&mut rnd);
    let tag: String = rnd.iter().map(|b| format!("{:02x}", b)).collect();
    let out = out_dir_for(input).join(format!("csoclassify_{}.txt", tag));

    // 인자를 낱개로 넘긴다 — 공백·한글 경로가 셸에서 갈라지지 않게.
    let mut child = match Command::new(exe)
        .arg("-U8").arg("-NO_WITHPAGE").arg(input).arg(&out)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("snf_exe 를 실행하지 못했습니다({})", e)),
    };
    // 출력을 따로 읽는다 — 안 읽으면 파이프가 차서 snf_exe 가 멈출 수 있다.
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut b = vec![];
        if let Some(s) = so.as_mut() { let _ = s.read_to_end(&mut b); }
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = vec![];
        if let Some(s) = se.as_mut() { let _ = s.read_to_end(&mut b); }
        b
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if timeout > 0 && start.elapsed() >= Duration::from_secs(timeout) {
                    // 제한 시간 초과 — 끝내고, 반쯤 쓴 출력파일이 성공으로 읽히지 않게 지운다.
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = t_out.join();
                    let _ = t_err.join();
                    remove_quiet(&out);
                    return Err(format!("snf_exe 타임아웃({}s)", timeout));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break None,
        }
    };
    let stdout = t_out.join().unwrap_or_default();
    let stderr = t_err.join().unwrap_or_default();

    // 성공 판별: 출력파일이 생겼고 비어 있지 않아야 한다(종료코드는 보지 않는다).
    let len = std::fs::metadata(&out).map(|m| m.len()).ok();
    match len {
        Some(n) if n > 0 => {
            let r = std::fs::read(&out);
            remove_quiet(&out);
            match r {
                Ok(b) => Ok(String::from_utf8_lossy(&b).into_owned()),
                Err(e) => Err(format!("출력파일을 읽지 못했습니다({})", e)),
            }
        }
        _ => {
            let state = if len.is_none() { "출력파일 미생성" } else { "출력파일 0바이트" };
            let rc = status.and_then(|s| s.code()).map(|c| c.to_string()).unwrap_or_else(|| "?".into());
            let mut detail = format!("rc={}; {}", rc, state);
            let o = decode_msg(&stdout);
            let e = decode_msg(&stderr);
            if !o.is_empty() { detail += &format!("; stdout={}", head_chars(&o, 300)); }
            if !e.is_empty() { detail += &format!("; stderr={}", head_chars(&e, 300)); }
            // 0바이트로 남은 파일은 지워 다음 실행이 헷갈리지 않게 한다.
            remove_quiet(&out);
            Err(detail)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 복호화 문서는 그 문서 폴더에, 그 밖은 시스템 임시 폴더에 출력파일을 둔다.
    #[test]
    fn 출력파일_자리() {
        let dec = Path::new("/tmp/cso_dec_123/000001/000001.docx");
        assert_eq!(out_dir_for(dec), PathBuf::from("/tmp/cso_dec_123/000001"));
        let plain = Path::new("/data/docs/a.docx");
        assert_eq!(out_dir_for(plain), std::env::temp_dir());
    }
}

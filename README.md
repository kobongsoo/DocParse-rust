# DocParse-rust

문서 파일에서 본문 텍스트를 뽑는 라이브러리입니다. 원래 MpowerClassify 안에 있던 코드를 떼어 낸 독립 프로젝트이고,
**Windows `docparse.dll` · Linux `libdocparse.so`** 를 만들어 다른 프로그램이 실행 중에 불러 쓰게 합니다.

- 형식: pdf(pdfium) · docx · xlsx · pptx · hwpx · hwp(5.0) · doc · xls · ppt · html · 텍스트
- 압축 해제: zip · tar · gz · bz2 · xz · 7z · rar (중첩·분할 볼륨 안내·압축폭탄 상한)
- 사이냅(snf_exe) 외부 추출기 폴백(경로를 받아 실행만 한다)
- 전역 상태 없음 — 여러 스레드에서 동시에 불러도 된다

설계: MpowerClassify `plan/Rust-문서파서-라이브러리분리-설계-20261110.html` (개정 2)

## 내려받기

빌드한 배포 묶음은 GitHub 릴리스에 올립니다 → **[최신 릴리스](https://github.com/kobongsoo/DocParse-rust/releases/latest)** · [전체 릴리스 목록](https://github.com/kobongsoo/DocParse-rust/releases)

최신 판 바로 받기(링크는 새 릴리스를 올리면 저절로 그 판을 가리킵니다):

| 플랫폼 | 파일 |
|---|---|
| Windows x64 | [docparse-windows-x64.zip](https://github.com/kobongsoo/DocParse-rust/releases/latest/download/docparse-windows-x64.zip) |
| Linux x64 (glibc 2.17+) | [docparse-linux-x64.tgz](https://github.com/kobongsoo/DocParse-rust/releases/latest/download/docparse-linux-x64.tgz) |

풀어서 `lib/` 폴더를 그대로 쓰면 됩니다. 정확한 판은 묶음 안 `VERSION`, 파일 확인은 `SHA256SUMS` 로 합니다.
특정 판이 필요하면 릴리스 목록에서 `docparse-<버전>-<해시>-<플랫폼>` 이름의 파일을 받으세요.

릴리스 올릴 때: `package.py` 가 `dist/` 에 버전 붙은 묶음과 버전 없는 이름(`docparse-<플랫폼>.zip/.tgz`)을
함께 만듭니다. 위 링크가 동작하려면 **네 파일을 모두** 릴리스에 올립니다.

## 구조

```
crates/docparse/          본체(Rust 라이브러리) — 형식별 파서·정제·압축 해제
crates/docparse-ffi/      C 진입점 → docparse.dll / libdocparse.so
crates/docparse-cli/      시험 도구 → dp.exe / dp (C 진입점을 그대로 거쳐 문서를 읽는다)
  include/docparse.h      C 헤더 (함수 11개, 옵션·결과는 UTF-8 JSON)
bindings/python/docparse.py   ctypes 감싸개 (표준 라이브러리만)
bindings/rust/docparse_dyn.rs libloading 감싸개 (libloading · serde_json)
bindings/java/docparse/       JNA 감싸개 (Java 8+, jna 5.14+) + 작은 JSON 해석기
bindings/php/DocParse.php     FFI 감싸개 (PHP 7.4+)
examples/                 언어별 예제 (python · rust · java · php · c)
doc/docparse-라이브러리-연동설명서.html   언어별 적용 방법 설명서 (묶음 안: doc/docparse-guide.html)
doc/빌드_CLI시험_설명서.html   빌드하고 dp.exe 로 시험하는 방법 (묶음 안: doc/build-cli-guide.html)
tests/samples/            지어낸 시험 문서 14건 (+ 목록 tests/samples.txt, 다시 만들기 tools/make_samples.py)
plan/                     설계서
third_party/pdfium/       pdfium 정본 (묶음에 같이 넣는다)
scripts/                  win-build.sh · linux-build.sh · package.py
```

## 빌드

개발 중 빌드·시험은 `cargo build --release` 후 `target/release/dp.exe` 로 합니다 — 자세한 순서는 `doc/빌드_CLI시험_설명서.html`.

```bash
cargo build --release                                    # docparse.dll + dp.exe
./target/release/dp.exe extract tests/samples/보고서_예시.docx    # 본문 보기 (pdfium.dll 은 dp.exe 옆에)
./target/release/dp.exe dump tests/samples.txt work/out_a       # 여러 건 → 고친 뒤 compare 로 비교
```

배포 묶음:

```bash
bash scripts/win-build.sh      # 이 PC → dist/docparse-<버전>-windows-x64.zip
bash scripts/linux-build.sh    # 빌드서버(CentOS 7) → dist/docparse-<버전>-linux-x64.tgz
```

리눅스 빌드는 `scripts/server.env` 가 필요합니다(`server.env.sample` 참고). 빌드서버에서 만들어야 glibc 2.17 리눅스에서도 돕니다.

배포 묶음 안:

| 경로 | 내용 |
|---|---|
| `lib/` | `docparse.dll` + `pdfium.dll` (Linux: `libdocparse.so` + `libpdfium.so`, RPATH=$ORIGIN) |
| `lib/dp.exe` (Linux `lib/dp`) | 시험 도구 — 풀자마자 `lib/dp.exe extract 문서` 로 같은 코드를 시험 |
| `lib/docparse.lib` | Windows 전용 — C/C++ 가 링크할 때 쓰는 가져오기 라이브러리(정적 라이브러리 아님, 실행 때는 DLL 필요) |
| `include/docparse.h` | C 헤더 — 모든 언어의 기준 |
| `python/` | `docparse.py` 감싸개 · `example.py` |
| `rust/` | `docparse_dyn.rs` 감싸개 · `example/` (Cargo 프로젝트) |
| `java/` | `docparse/DocParse.java` · `docparse/Json.java` 감싸개 · `Example.java` |
| `php/` | `DocParse.php` 감싸개 · `example.php` |
| `c/` | `example.c` (MSVC · gcc) |
| `doc/` | `docparse-guide.html` — 언어별 적용 과정과 코드 예시 · `build-cli-guide.html` — 빌드와 dp 시험 |
| `VERSION` · `SHA256SUMS` · `CHANGELOG.md` | 버전("0.1.0+git해시")·파일 해시·변경 기록 |

## 쓰기

Python:

```python
import docparse                      # bindings/python/docparse.py
r = docparse.extract("보고서.hwp", clean=True)
print(r.engine, r.fmt, len(r.text), r.partial, r.layout)
```

라이브러리 찾는 순서: `load(path)` → 환경변수 `DOCPARSE_LIB` → 감싸개 폴더 → 그 아래 `lib/` → PyInstaller `_MEIPASS` → 실행파일 폴더.

Rust:

```rust
let dp = DocParse::find_and_load(None, "DOCPARSE_LIB")?;   // bindings/rust/docparse_dyn.rs
let x = dp.extract(Path::new("보고서.hwp"), &serde_json::json!({"clean": true}))?;
```

C: `include/docparse.h` 주석에 옵션·결과 JSON 모양과 규칙(문자열은 UTF-8, `docparse_free` 로 해제)이 있습니다.
Windows 는 `cl /utf-8 /I include main.c lib\docparse.lib`, Linux 는 `gcc -I include main.c -L lib -ldocparse -Wl,-rpath,'$ORIGIN/lib'`.

Java (JNA):

```java
DocParse dp = DocParse.load();                       // DOCPARSE_LIB → -Ddocparse.lib → ./lib → 시스템
DocParse.Extracted r = dp.extract("보고서.hwp", "{\"clean\":true}");
```

PHP (FFI):

```php
require 'DocParse.php';
$r = DocParse::load()->extract('보고서.hwp', ['clean' => true]);
```

언어별 자세한 적용 과정·주의점은 `doc/docparse-guide.html` 에 있습니다.

pdfium 은 옵션 `pdfium_lib` → **이 라이브러리 파일이 있는 폴더** → 실행파일 폴더 → 시스템 순으로 찾습니다.
Python 이 불러 쓰면 실행파일은 python.exe 이므로, pdfium 을 docparse 라이브러리 옆에 두면 됩니다.

## dp.exe 로 시험하기

`dp.exe`(Linux `dp`)는 DLL 과 같은 C 진입점을 그대로 거쳐 문서를 읽는 시험 도구입니다. 배포 묶음을 풀면 `lib/` 에
`pdfium` 옆에 들어 있어 바로 돕니다(저장소에서 빌드했다면 `target/release/dp.exe`, `pdfium.dll` 을 옆에 복사).
아래는 묶음을 푼 폴더에서 PowerShell 로 실행하는 예입니다. 자세한 설명·화면은 `doc/빌드_CLI시험_설명서.html`(묶음 안 `doc/build-cli-guide.html`).

### 버전 · 형식

```powershell
.\lib\dp.exe version                         # docparse 0.1.2+34fc2c2 (abi 1)
.\lib\dp.exe detect 보고서.hwp 자료.zip 그림.png   # 파일 내용으로 감지: hwp / zip / image
```

### 본문 뽑기

```powershell
.\lib\dp.exe extract 보고서.docx                       # 본문은 화면, 요약 한 줄은 표준 오류
#   … 본문 …
#   [dp] fmt=docx engine=docx chars=333 4ms
.\lib\dp.exe extract 보고서.docx --out 결과.txt         # 본문을 UTF-8 파일로 (PowerShell 의 > 는 UTF-16 이라 비추천)
.\lib\dp.exe extract 보고서.pdf --json                  # 라이브러리 결과 JSON 전체 (text · fmt · engine · partial · layout)
.\lib\dp.exe extract 보고서.pdf --raw                   # 6단계 정제 전 원시 텍스트
.\lib\dp.exe extract 확장자없는파일 --fmt hwp            # 감지를 건너뛰고 그 형식으로
.\lib\dp.exe extract 보고서.pdf --no-extra-text          # 본문 밖 글(주석 · 서식 칸 · 메모 · SmartArt) 빼고 — 0.1.x 와 같은 본문
```

0.2.0 부터 **본문 밖 글**도 기본으로 읽습니다 — PDF 글상자(FreeText) 주석 · PDF 서식 칸 값 · docx 메모 · docx/pptx SmartArt.
사이냅은 읽는데 docparse 는 놓치던 '화면에 보이는 글'입니다. 숨긴 글상자 · PDF 쪽지 주석은 읽지 않습니다.
옵션 `"extra_text": false`(dp `--no-extra-text`)로 끕니다. 설계: `plan/PDF-layout보존-본문밖글읽기-설계-20261118.html`.

스캔(그림) PDF 는 성공(0)이지만 본문이 비고, 요약 줄에 판정이 붙습니다:

```
[dp] fmt=pdf engine=pdf chars=0 11ms layout={"pages":2,"scan_pages":2} 판정=스캔PDF(처리제외)
```

사이냅 폴백에서 사이냅도 못 읽은 스캔 PDF 는 실패(5)지만, 오류에 쪽 구성이 실려 판정이 붙습니다(0.2.0~):

```
실패(5 DP_ERR_FAILED): 하이브리드 전엔진 실패(감지=pdf; pdf:empty; snf:…) layout={"pages":2,"scan_pages":2} 판정=스캔PDF(처리제외)
```

### 상한 · 로그 · 사이냅

```powershell
.\lib\dp.exe extract 큰문서.pdf --limit max_pdf_pages=10 --log warn    # 10쪽에서 멈춤 → partial={…}, 경고 로그도 보임
.\lib\dp.exe extract 큰문서.pdf --limit parser_timeout_secs=none       # 시간 상한 없애기 (none = 상한 없음)
.\lib\dp.exe check   큰파일.xlsx --limit max_file_bytes=1000000        # 추출 없이 크기 상한만 (걸리면 종료 코드 3)
.\lib\dp.exe extract 보고서.hwp --engine synap_fallback --synap-exe C:\Synap\snf_exe.exe   # 내장 실패 시 사이냅으로
```

`--limit` 키: `parser_timeout_secs`(60) · `max_pdf_pages`(3000) · `max_file_bytes`(100MB) · `max_file_bytes_text`(100MB) · `snf_timeout_secs`(60) · `min_text_len`(20). 여러 번 써도 됩니다.

### 압축 · 정제

```powershell
.\lib\dp.exe expand 자료.zip --tmp .\풀기               # 안쪽 압축까지 풀고 작업 목록 JSON (jobs[].src 를 extract)
.\lib\dp.exe extract .\풀기\a1                          # 풀린 파일(확장자 없음 — 내용으로 감지)
.\lib\dp.exe clean 원시.txt --out 정제.txt              # 텍스트 파일을 6단계 정제만
```

### 여러 문서 · 고치기 전후 비교

```powershell
.\lib\dp.exe dump 목록.txt .\out_before                 # 목록(한 줄에 경로 하나, UTF-8) 또는 폴더
#   [1/3]     38ms pdf   보고서.pdf
#   [2/3]      1ms 실패(4) 그림.png — 지원하지 않는 포맷(감지=image)
#   끝 — 성공 2 · 실패 1 · 0.1초 → .\out_before\index.json
.\lib\dp.exe dump 목록.txt .\out_after                  # 라이브러리를 바꾼 뒤 다시
.\lib\dp.exe compare .\out_before .\out_after           # 같으면 종료 코드 0, 다르면 1
#   다름  0000 보고서.pdf — 글자 13 에서 갈림 (길이 131 → 138): «…» ≠ «…»
#   같음 1 · 다름 1 · 새 쪽에 없음 0 (기준 3건)
```

`index.json` 에 문서마다 `rc` · `fmt` · `engine` · `chars` · `ms` · `partial` · `err` 가 남습니다. CHANGELOG 의 "결과 바뀜" 여부는 이 compare 로 확인합니다.

### 종료 코드로 가르기 (배치)

종료 코드는 라이브러리 상태 코드 그대로입니다 — 0 성공 · 1 인자 오류 · 2 파일 없음 · 3 크기 상한 · 4 지원 안 함 · 5 실패 · 99 내부 오류.

```powershell
Get-ChildItem .\문서 -File | ForEach-Object {
    .\lib\dp.exe extract $_.FullName --out ".\결과\$($_.Name).txt" 2>$null
    if ($LASTEXITCODE -ne 0) { "$LASTEXITCODE  $($_.Name)" }      # 실패한 것만 코드와 이름
}
```

Linux 는 경로 구분자만 바꿔 `./lib/dp extract 보고서.docx` 처럼 씁니다(명령 · 옵션은 같습니다).

## 버전 규칙

- `DOCPARSE_ABI_VERSION` 은 함수 모양이 바뀔 때만 올린다. 감싸개는 불러온 직후 비교하고 다르면 멈춘다.
- 새 기능은 새 JSON 키나 새 함수로 더한다(ABI 를 올리지 않고 라이브러리만 바꿔 넣을 수 있게).
- **추출 결과(본문)가 바뀌는 수정은 반드시 `CHANGELOG.md` 에 적는다.** 쓰는 프로젝트는 그것을 보고 평가셋을 다시 돌릴지 정한다.

## 시험

```bash
cargo test --workspace
```

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
  include/docparse.h      C 헤더 (함수 11개, 옵션·결과는 UTF-8 JSON)
bindings/python/docparse.py   ctypes 감싸개 (표준 라이브러리만)
bindings/rust/docparse_dyn.rs libloading 감싸개 (libloading · serde_json)
bindings/java/docparse/       JNA 감싸개 (Java 8+, jna 5.14+) + 작은 JSON 해석기
bindings/php/DocParse.php     FFI 감싸개 (PHP 7.4+)
examples/                 언어별 예제 (python · rust · java · php · c)
doc/docparse-라이브러리-연동설명서.html   언어별 적용 방법 설명서 (묶음 안: doc/docparse-guide.html)
third_party/pdfium/       pdfium 정본 (묶음에 같이 넣는다)
scripts/                  win-build.sh · linux-build.sh · package.py
```

## 빌드

```bash
bash scripts/win-build.sh      # 이 PC → dist/docparse-<버전>-windows-x64.zip
bash scripts/linux-build.sh    # 빌드서버(CentOS 7) → dist/docparse-<버전>-linux-x64.tgz
```

리눅스 빌드는 `scripts/server.env` 가 필요합니다(`server.env.sample` 참고). 빌드서버에서 만들어야 glibc 2.17 리눅스에서도 돕니다.

배포 묶음 안:

| 경로 | 내용 |
|---|---|
| `lib/` | `docparse.dll` + `pdfium.dll` (Linux: `libdocparse.so` + `libpdfium.so`, RPATH=$ORIGIN) |
| `lib/docparse.lib` | Windows 전용 — C/C++ 가 링크할 때 쓰는 가져오기 라이브러리(정적 라이브러리 아님, 실행 때는 DLL 필요) |
| `include/docparse.h` | C 헤더 — 모든 언어의 기준 |
| `python/` | `docparse.py` 감싸개 · `example.py` |
| `rust/` | `docparse_dyn.rs` 감싸개 · `example/` (Cargo 프로젝트) |
| `java/` | `docparse/DocParse.java` · `docparse/Json.java` 감싸개 · `Example.java` |
| `php/` | `DocParse.php` 감싸개 · `example.php` |
| `c/` | `example.c` (MSVC · gcc) |
| `doc/` | `docparse-guide.html` — 언어별 적용 과정과 코드 예시 |
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

## 버전 규칙

- `DOCPARSE_ABI_VERSION` 은 함수 모양이 바뀔 때만 올린다. 감싸개는 불러온 직후 비교하고 다르면 멈춘다.
- 새 기능은 새 JSON 키나 새 함수로 더한다(ABI 를 올리지 않고 라이브러리만 바꿔 넣을 수 있게).
- **추출 결과(본문)가 바뀌는 수정은 반드시 `CHANGELOG.md` 에 적는다.** 쓰는 프로젝트는 그것을 보고 평가셋을 다시 돌릴지 정한다.

## 시험

```bash
cargo test --workspace
```

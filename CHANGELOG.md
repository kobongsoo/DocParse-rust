# 변경 기록

추출 결과(본문)가 바뀌는 수정은 **결과 바뀜** 으로 표시한다. 쓰는 프로젝트는 이것을 보고 평가셋을 다시 돌릴지 정한다.

## 0.2.0 — 2026-11-18

설계: `plan/PDF-layout보존-본문밖글읽기-설계-20261118.html`

- **결과 바뀜** — 본문 밖 글을 기본으로 읽는다(옵션 `extra_text`, 기본 true). 사이냅은 읽는데 docparse 는 놓치던 '화면에 보이는 글'이다.
  - PDF: 글상자(FreeText) 주석의 글 · 서식 칸(글 칸 · 콤보 상자) 값 — 그 쪽 본문 뒤. 숨긴 글상자 · 쪽지(Text) 주석 · 체크 상자는 읽지 않는다.
  - docx: 메모(comment) — 본문 뒤 · 머리말 앞. SmartArt 글 — 본문 바로 뒤.
  - pptx: SmartArt 글 — 그 슬라이드 글 뒤(발표자 노트는 지금처럼 읽는다).
  - 해당 자리가 없는 문서는 그대로다. `D:\sample` 실측 42건 중 2건(PowerPoint 내보내기 PDF 의 글상자 주석 · SmartArt pptx)만 늘었고, 늘어난 글은 사이냅과 같다.
  - `"extra_text": false`(dp `--no-extra-text` · Python `extra_text=False`)면 0.1.x 와 바이트까지 같은 본문(42건 · 시험 문서 10건 다름 0).
  - 스캔 쪽 판정(`layout.scan_pages`)은 쪽 본문만 보므로 바뀌지 않는다. 처리제외 판정의 본문 길이는 늘어난 본문으로 센다.
- 사이냅 폴백(`synap_fallback`)에서 PDF 의 쪽 구성(`layout`)을 끝까지 남긴다. 예전에는 사이냅 차례에 지워, 사이냅이 이기거나 짧은 결과를 채택한 스캔 PDF 가 처리제외 판정을 못 받았다(Python 판 notes 규칙과 같게).
- 모든 엔진이 실패해도 PDF 파서가 잰 쪽 구성을 오류에 싣는다 — 오류 JSON `error.layout`(새 키, ABI 1 그대로). 사이냅도 못 읽은 스캔 PDF 를 `docparse_scan_pdf_verdict(pages, scan_pages, 0)` 으로 처리제외로 가를 수 있다.
  Rust 크레이트 API: `ExtractError::Failed(String)` → `Failed { reason, layout }`. 감싸개 4종에 layout 접근 추가.
- dp: `--no-extra-text`, 실패 줄 · dump index 에 layout · 판정. 시험 문서 4건 추가(주석 PDF · 메모 docx · SmartArt pptx/docx).

## 0.1.2 — 2026-11-16

- 시험 도구 `dp.exe` / `dp`(crates/docparse-cli) 추가 — C 진입점을 그대로 거쳐 version · detect · extract · check · expand · clean · dump · compare.
  배포 묶음 `lib/` 에 함께 넣는다.
- Linux tgz 안의 `dp` · `.so` 에 실행 권한(755)을 준다(Windows 에서 묶으면 권한이 빠지던 것).
- 시험용 합성 문서 10건(`tests/samples/`, 목록 `tests/samples.txt`)과 `tools/make_samples.py`, 설명서 `doc/빌드_CLI시험_설명서.html` 추가.
- 라이브러리 코드는 그대로다. 결과 바뀜 없음.

## 0.1.1 — 2026-11-16

- 배포 묶음에 Windows 가져오기 라이브러리 `lib/docparse.lib` 를 넣는다(C/C++ 빌드 때 링크용).
- Java(JNA) · PHP(FFI) 감싸개와 언어별 예제(python·rust·java·php·c), 사용 설명서(`doc/`)를 묶음에 넣는다.
- 라이브러리 코드는 그대로다. 결과 바뀜 없음.

## 0.1.0 — 2026-11-10

- MpowerClassify(Rust 판 `Rust/src`)에서 문서 추출·압축 해제 코드를 떼어 낸 첫 판. 본문 결과는 분리 전 MpowerClassify-rs 와 바이트 단위로 같다.
- C 진입점(`docparse.dll` / `libdocparse.so`) 함수 11개, ABI 버전 1.
- pdfium 을 이 라이브러리 파일이 있는 폴더에서도 찾는다(Python 에서 불러 쓸 때 python.exe 옆만 보던 문제). 결과 바뀜 없음.

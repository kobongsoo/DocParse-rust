# 변경 기록

추출 결과(본문)가 바뀌는 수정은 **결과 바뀜** 으로 표시한다. 쓰는 프로젝트는 이것을 보고 평가셋을 다시 돌릴지 정한다.

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

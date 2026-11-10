# 변경 기록

추출 결과(본문)가 바뀌는 수정은 **결과 바뀜** 으로 표시한다. 쓰는 프로젝트는 이것을 보고 평가셋을 다시 돌릴지 정한다.

## 0.1.0 — 2026-11-10

- MpowerClassify(Rust 판 `Rust/src`)에서 문서 추출·압축 해제 코드를 떼어 낸 첫 판. 본문 결과는 분리 전 MpowerClassify-rs 와 바이트 단위로 같다.
- C 진입점(`docparse.dll` / `libdocparse.so`) 함수 11개, ABI 버전 1.
- pdfium 을 이 라이브러리 파일이 있는 폴더에서도 찾는다(Python 에서 불러 쓸 때 python.exe 옆만 보던 문제). 결과 바뀜 없음.

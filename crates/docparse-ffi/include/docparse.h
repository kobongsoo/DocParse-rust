/*
 * docparse.h — 문서 → 본문 추출 라이브러리의 C 진입점
 *
 *   Windows: docparse.dll   (같은 폴더에 pdfium.dll)
 *   Linux  : libdocparse.so (같은 폴더에 libpdfium.so, RPATH=$ORIGIN)
 *
 * 규칙
 *   - 문자열은 전부 널 종료 UTF-8 이다. 경로도 UTF-8 로 넘긴다
 *     (Windows 에서 CP949 등 시스템 코드페이지로 바꾸지 않는다).
 *   - char** out 으로 받은 문자열은 성공이든 실패든 반드시 docparse_free 로 돌려준다.
 *     C 의 free() 로 지우면 안 된다(할당기가 다르다).
 *   - const char* / const char** 로 받는 것(버전·형식 이름)은 고정 문자열이라 해제하지 않는다.
 *   - 성공·실패는 반환 코드로 가른다. 자세한 사유는 결과 JSON 의 "error" 에 있다.
 *   - 전역 상태가 없어 여러 스레드에서 동시에 불러도 된다(로거 등록만 전역).
 *   - 함수 모양이 바뀌면 DOCPARSE_ABI_VERSION 이 오른다. 불러온 직후
 *     docparse_abi_version() == DOCPARSE_ABI_VERSION 인지 확인한다.
 *
 * 옵션 JSON (docparse_extract · docparse_check_size) — 빠진 키는 기본값, 모르는 키는 무시
 *   { "clean": true,                       // false = 정제 전 원시 텍스트
 *     "engine": "builtin",                 // "builtin" | "synap_fallback" | "synap_only"
 *     "synap_exe": null,                   // engine 이 synap_* 일 때 snf_exe 경로
 *     "pdfium_lib": null,                  // null = 이 DLL 폴더 → 실행파일 폴더 → 시스템
 *     "label": null,                       // 로그에 적을 문서 이름
 *     "fmt": null,                         // 주면 감지를 건너뛰고 그 형식으로
 *     "limits": { "parser_timeout_secs": 60, "max_pdf_pages": 3000,
 *                 "max_file_bytes": 104857600, "max_file_bytes_text": 104857600,
 *                 "snf_timeout_secs": 60, "min_text_len": 20 } }
 *   상한 값: 키가 없으면 기본값, null 이나 0 이하면 '상한 없음'.
 *
 * 추출 결과 JSON
 *   성공: { "text": "...", "engine": "docx", "fmt": "docx",
 *           "partial": null | {"reason":"...","unit":"pages","read":120,"total":800},
 *           "layout":  null | {"pages":800,"scan_pages":3} }
 *   실패: { "error": { "kind": "size_limit"|"unsupported"|"failed"|"arg"|"panic",
 *                      "message": "...", ... } }
 */
#ifndef DOCPARSE_H
#define DOCPARSE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define DOCPARSE_ABI_VERSION 1

typedef enum {
    DP_OK              = 0,
    DP_ERR_ARG         = 1,   /* 인자 오류: NULL·잘못된 UTF-8·JSON 파싱 실패 */
    DP_ERR_NOT_FOUND   = 2,   /* 파일이 없음 */
    DP_ERR_SIZE_LIMIT  = 3,   /* 크기 상한 초과 — 깨진 파일과 구분 */
    DP_ERR_UNSUPPORTED = 4,   /* 지원하지 않는 형식(이미지 등) */
    DP_ERR_FAILED      = 5,   /* 파싱 실패(깨진 파일·암호 등) */
    DP_ERR_PANIC       = 99   /* 라이브러리 내부 오류(버그). 메시지 첨부 */
} dp_status;

/* 1) ABI 버전 — DOCPARSE_ABI_VERSION 과 같아야 한다 */
uint32_t    docparse_abi_version(void);

/* 2) 라이브러리 버전 "0.1.0+abc1234" — 해제하지 않는다 */
const char* docparse_version(void);

/* 3) 형식 감지 — *out_fmt = "pdf"·"hwp"·"docx"·…·"unknown" (해제하지 않는다) */
int         docparse_detect(const char* path, const char** out_fmt);

/* 4) 본문 추출 — opts_json 은 NULL 가능. *out_json 은 docparse_free 로 */
int         docparse_extract(const char* path, const char* opts_json, char** out_json);

/* 5) 크기 상한 검사만 — fmt 는 NULL 이면 감지. *out_json = {"hit": null | {size,limit,text_kind,reason}} */
int         docparse_check_size(const char* path, const char* fmt, const char* opts_json, char** out_json);

/* 6) 압축 펼치기 — paths_json = ["a.zip", …], opts_json = {"max_depth":3,"max_bytes":…,"max_members":…}
 *    *out_json = {"jobs":[{"src","label","origin"}],
 *                 "stats":{"unexpanded":[{"path","reason","kind"}],"unsupported":[{"path","reason"}],"partial":[…]}} */
int         docparse_expand(const char* paths_json, const char* tmp_root, const char* opts_json, char** out_json);

/* 7) 분할 압축 조각 안내 — path 는 NULL 가능. *out_json = {"hint": null | "…"} */
int         docparse_split_volume_hint(const char* name, const char* path, char** out_json);

/* 8) 6단계 본문 정제 — *out_text 는 정제된 본문(JSON 아님). docparse_free 로 */
int         docparse_clean_text(const char* text, char** out_text);

/* 9) 스캔 PDF 판정 — 1 = 스캔(처리제외), 0 = 아님 */
int         docparse_scan_pdf_verdict(uint64_t pages, uint64_t scan_pages, uint64_t body_chars);

/* 10) 해제 — 이 라이브러리가 out 으로 준 char* 만. NULL 이면 아무 일도 하지 않는다 */
void        docparse_free(char* p);

/* 11) 로그 받기 — 등록하지 않으면 로그를 버린다. fn = NULL 이면 해제.
 *     level: 1=error 2=warn 3=info 4=debug 5=trace, max_level 0 이하 = 끔.
 *     site = "모듈경로:줄". 콜백은 추출을 부른 스레드에서 불린다 —
 *     짧게, 스레드 안전하게 쓰고, 콜백 안에서 docparse_* 를 다시 부르지 않는다. */
typedef void (*dp_log_fn)(int level, const char* target, const char* site, const char* msg, void* user);
void        docparse_set_logger(dp_log_fn fn, int max_level, void* user);

#ifdef __cplusplus
}
#endif

#endif /* DOCPARSE_H */

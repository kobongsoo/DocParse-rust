/*
 * docparse C 예제 — 파일마다 형식 감지 → 본문 추출(zip 은 펼친 결과) JSON 을 찍는다.
 *
 * Windows (MSVC, 개발자 명령 프롬프트):
 *   cl /utf-8 /I include example.c lib\docparse.lib shell32.lib
 *   (실행할 때 example.exe 옆이나 PATH 에 docparse.dll · pdfium.dll)
 *
 * Linux (gcc):
 *   gcc -std=c99 -I include example.c -L lib -ldocparse -Wl,-rpath,'$ORIGIN/lib' -o example
 *
 * 실행: example a.hwp b.zip
 *
 * 결과는 JSON 문자열이다. 실제 프로젝트에서는 cJSON·jansson 등으로 읽는다.
 * 여기서는 라이브러리 사용법만 보이려고 JSON 을 그대로 찍는다.
 */
#ifdef _WIN32
#define _CRT_SECURE_NO_WARNINGS  /* getenv 경고 끄기 */
#endif
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "docparse.h"

#ifdef _WIN32
#include <windows.h>
#include <shellapi.h>
#include <direct.h>
#define MKDIR(p) _mkdir(p)
#else
#include <sys/stat.h>
#define MKDIR(p) mkdir((p), 0700)
#endif

/* 로그 콜백 — 추출을 부른 스레드에서 불린다. 짧게 쓴다. */
static void on_log(int level, const char* target, const char* site, const char* msg, void* user)
{
    (void)target; (void)site; (void)user;
    fprintf(stderr, "[docparse %d] %s\n", level, msg);
}

/* 문자열 하나를 JSON 문자열로 쓴다(경로에 들어갈 수 있는 \ 와 " 만 이스케이프하면 충분). */
static void json_str(char* dst, size_t cap, const char* s)
{
    size_t n = 0;
    if (n < cap) dst[n++] = '"';
    for (; *s && n + 3 < cap; s++) {
        if (*s == '\\' || *s == '"') dst[n++] = '\\';
        dst[n++] = *s;
    }
    if (n < cap) dst[n++] = '"';
    dst[n < cap ? n : cap - 1] = '\0';
}

#ifdef _WIN32
/* [중요] Windows 의 main(argc, argv) 는 시스템 코드페이지(한국어 Windows 는 CP949)로 온다.
 * 라이브러리는 UTF-8 만 받으므로 그대로 넘기면 한글 경로가 DP_ERR_ARG 로 실패한다.
 * 명령줄을 UTF-16 으로 다시 읽어 UTF-8 로 바꾼다. */
static char** utf8_argv(int* argc)
{
    LPWSTR* w = CommandLineToArgvW(GetCommandLineW(), argc);
    char** a = (char**)calloc((size_t)*argc + 1, sizeof(char*));
    for (int i = 0; i < *argc; i++) {
        int n = WideCharToMultiByte(CP_UTF8, 0, w[i], -1, NULL, 0, NULL, NULL);
        a[i] = (char*)malloc((size_t)n);
        WideCharToMultiByte(CP_UTF8, 0, w[i], -1, a[i], n, NULL, NULL);
    }
    LocalFree(w);
    return a;
}
#endif

int main(int argc, char** argv)
{
#ifdef _WIN32
    argv = utf8_argv(&argc);
    /* 콘솔이 UTF-8 을 그대로 보여 주게 한다. */
    SetConsoleOutputCP(CP_UTF8);
#endif
    if (docparse_abi_version() != DOCPARSE_ABI_VERSION) {
        fprintf(stderr, "ABI 판이 다릅니다: 라이브러리 %u, 헤더 %d\n", docparse_abi_version(), DOCPARSE_ABI_VERSION);
        return 1;
    }
    printf("docparse %s\n", docparse_version());
    docparse_set_logger(on_log, 2, NULL);   /* 경고 이상만 */

    for (int i = 1; i < argc; i++) {
        /* 1) 형식 감지 — 돌려받는 문자열은 고정 문자열이라 해제하지 않는다 */
        const char* fmt = NULL;
        int code = docparse_detect(argv[i], &fmt);
        printf("\n== %s (감지=%s, 코드 %d)\n", argv[i], fmt ? fmt : "?", code);
        if (code != DP_OK) continue;

        /* 2) zip 이면 펼친 결과(JSON)만 보여 준다.
         *    실제로는 jobs[].src 마다 docparse_extract 를 부른다(JSON 해석기가 필요해 여기선 생략).
         *    7z·rar·tar 등은 감지가 따로 알려 주지 않으므로, 실제 프로그램은 모든 입력을 먼저 expand 에 넘긴다. */
        if (strcmp(fmt, "zip") == 0) {
            /* 펼친 파일은 tmp_root 바로 아래에 생긴다 — 전용 폴더를 만들어 주고, 다 쓰면 지운다. */
            const char* base = getenv("TEMP");
            if (!base) base = getenv("TMPDIR");
            if (!base) base = "/tmp";
            char tmp[1024];
            snprintf(tmp, sizeof tmp, "%s/docparse-example-%d", base, i);
            MKDIR(tmp);
            char paths[4096], quoted[4000];
            json_str(quoted, sizeof quoted, argv[i]);
            snprintf(paths, sizeof paths, "[%s]", quoted);
            char* out = NULL;
            code = docparse_expand(paths, tmp, "{\"max_depth\":3}", &out);
            printf("expand(코드 %d): %.400s\n", code, out ? out : "(null)");
            docparse_free(out);   /* 성공이든 실패든 반드시 돌려준다 */
            continue;
        }

        /* 3) 본문 추출 — 옵션은 JSON, NULL 이면 기본값 */
        char* out = NULL;
        code = docparse_extract(argv[i], "{\"clean\":true,\"limits\":{\"max_pdf_pages\":500}}", &out);
        switch (code) {
        case DP_OK:            printf("extract 성공: %.200s…\n", out); break;
        case DP_ERR_SIZE_LIMIT: printf("크기 상한 초과: %s\n", out); break;
        case DP_ERR_UNSUPPORTED: printf("지원하지 않는 형식: %s\n", out); break;
        default:               printf("실패(코드 %d): %s\n", code, out ? out : "(null)"); break;
        }
        docparse_free(out);
    }
    docparse_set_logger(NULL, 0, NULL);
    return 0;
}

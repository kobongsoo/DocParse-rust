package docparse;

import com.sun.jna.Callback;
import com.sun.jna.Library;
import com.sun.jna.Native;
import com.sun.jna.Pointer;
import com.sun.jna.ptr.PointerByReference;

import java.io.File;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * docparse.dll / libdocparse.so 의 Java 감싸개 (JNA, Java 8 이상).
 *
 * <p>필요한 의존: {@code net.java.dev.jna:jna:5.14.0} (이상). 그 밖에는 없다.
 *
 * <pre>{@code
 * DocParse dp = DocParse.load();                 // DOCPARSE_LIB → -Ddocparse.lib → ./lib → 시스템
 * DocParse.Extracted r = dp.extract("a.hwp");
 * System.out.println(r.fmt + " " + r.text.length());
 * }</pre>
 *
 * <p>규칙은 C 헤더 docparse.h 와 같다. 여러 스레드에서 동시에 불러도 된다.
 * 라이브러리가 out 으로 준 문자열은 이 클래스가 읽은 즉시 docparse_free 로 돌려준다.
 */
public final class DocParse {

    /** 이 감싸개가 아는 ABI 판. 라이브러리의 docparse_abi_version() 과 같아야 한다. */
    public static final int ABI_VERSION = 1;

    // 반환 코드 (docparse.h 의 dp_status)
    public static final int DP_OK = 0;
    public static final int DP_ERR_ARG = 1;
    public static final int DP_ERR_NOT_FOUND = 2;
    public static final int DP_ERR_SIZE_LIMIT = 3;
    public static final int DP_ERR_UNSUPPORTED = 4;
    public static final int DP_ERR_FAILED = 5;
    public static final int DP_ERR_PANIC = 99;

    /** JNA 가 C 함수에 이어 붙이는 선언. 문자열은 UTF-8 로 넘긴다(아래 load 의 옵션). */
    interface Lib extends Library {
        int docparse_abi_version();
        Pointer docparse_version();
        int docparse_detect(String path, PointerByReference outFmt);
        int docparse_extract(String path, String optsJson, PointerByReference outJson);
        int docparse_check_size(String path, String fmt, String optsJson, PointerByReference outJson);
        int docparse_expand(String pathsJson, String tmpRoot, String optsJson, PointerByReference outJson);
        int docparse_split_volume_hint(String name, String path, PointerByReference outJson);
        int docparse_clean_text(String text, PointerByReference outText);
        int docparse_scan_pdf_verdict(long pages, long scanPages, long bodyChars);
        void docparse_free(Pointer p);
        void docparse_set_logger(LogFn fn, int maxLevel, Pointer user);
    }

    /** C 로그 콜백 모양. 문자열은 UTF-8 로 직접 읽으려고 Pointer 로 받는다. */
    interface LogFn extends Callback {
        void invoke(int level, Pointer target, Pointer site, Pointer msg, Pointer user);
    }

    /** 로그를 받을 쪽이 구현한다. level: 1=error 2=warn 3=info 4=debug 5=trace. */
    public interface Logger {
        void log(int level, String target, String site, String message);
    }

    /** 추출 실패. kind = "size_limit"|"unsupported"|"failed"|"arg"|"panic"|"not_found". */
    public static class DocParseException extends RuntimeException {
        public final int code;
        public final String kind;
        /** 결과 JSON 의 "error" 객체 전체(추가 정보 포함). 없으면 빈 Map. */
        public final Map<String, Object> detail;

        DocParseException(int code, String kind, String message, Map<String, Object> detail) {
            super(message);
            this.code = code;
            this.kind = kind;
            this.detail = detail;
        }
    }

    /** 크기 상한 초과 — 깨진 파일(failed)과 구분해 처리하라고 따로 둔다. */
    public static final class SizeLimitException extends DocParseException {
        public final long size;
        public final long limit;
        public final boolean textKind;

        SizeLimitException(int code, String message, Map<String, Object> d) {
            super(code, "size_limit", message, d);
            this.size = num(d.get("size"));
            this.limit = num(d.get("limit"));
            this.textKind = Boolean.TRUE.equals(d.get("text_kind"));
        }
    }

    /** 추출 결과. partial·layout 은 없으면 null. */
    public static final class Extracted {
        public final String text;
        public final String engine;
        public final String fmt;
        /** 일부만 읽음: {reason, unit, read, total} 또는 null */
        public final Map<String, Object> partial;
        /** PDF 쪽수 정보: {pages, scan_pages} 또는 null */
        public final Map<String, Object> layout;
        /** 라이브러리가 준 원문 JSON */
        public final String json;

        @SuppressWarnings("unchecked")
        Extracted(Map<String, Object> m, String json) {
            this.text = (String) m.get("text");
            this.engine = (String) m.get("engine");
            this.fmt = (String) m.get("fmt");
            this.partial = (Map<String, Object>) m.get("partial");
            this.layout = (Map<String, Object>) m.get("layout");
            this.json = json;
        }

        @Override
        public String toString() {
            return "Extracted(engine=" + engine + ", fmt=" + fmt + ", chars=" + text.length()
                    + ", partial=" + partial + ", layout=" + layout + ")";
        }
    }

    private final Lib lib;
    private final String path;
    // 콜백 객체를 GC 가 지우면 C 쪽이 죽은 주소를 부르게 되므로 붙잡아 둔다.
    private volatile LogFn loggerRef;

    private DocParse(Lib lib, String path) {
        this.lib = lib;
        this.path = path;
    }

    /** 기본 순서로 찾아 연다: DOCPARSE_LIB 환경변수 → -Ddocparse.lib → ./lib/ → ./ → 시스템 경로. */
    public static DocParse load() {
        return load(null);
    }

    /**
     * 라이브러리를 연다. path 를 주면 그 파일만 연다.
     *
     * @throws UnsatisfiedLinkError 못 찾았거나 ABI 판이 다를 때
     */
    public static DocParse load(String path) {
        Map<String, Object> opt = new HashMap<String, Object>();
        // 라이브러리는 UTF-8 만 받는다 — Windows 기본 코드페이지(CP949)로 넘기면 한글 경로가 깨진다.
        opt.put(Library.OPTION_STRING_ENCODING, "UTF-8");
        List<String> tried = new ArrayList<String>();
        for (String cand : candidates(path)) {
            tried.add(cand);
            if (cand.indexOf(File.separatorChar) >= 0 && !new File(cand).isFile()) continue;
            Lib l;
            try {
                l = Native.load(cand, Lib.class, opt);
            } catch (UnsatisfiedLinkError e) {
                continue;
            }
            int abi = l.docparse_abi_version();
            if (abi != ABI_VERSION) {
                throw new UnsatisfiedLinkError("docparse ABI 판이 다릅니다: 라이브러리 " + abi
                        + ", 감싸개 " + ABI_VERSION + " (" + cand + ")");
            }
            return new DocParse(l, cand);
        }
        throw new UnsatisfiedLinkError("docparse 라이브러리를 찾지 못했습니다. 찾아본 곳: " + tried);
    }

    // 찾을 후보 목록(앞에서부터)
    private static List<String> candidates(String explicit) {
        List<String> c = new ArrayList<String>();
        if (explicit != null) { c.add(explicit); return c; }
        String file = System.getProperty("os.name").toLowerCase().startsWith("win") ? "docparse.dll" : "libdocparse.so";
        String env = System.getenv("DOCPARSE_LIB");
        if (env != null && !env.isEmpty()) c.add(env);
        String prop = System.getProperty("docparse.lib");
        if (prop != null && !prop.isEmpty()) c.add(prop);
        c.add(new File("lib", file).getAbsolutePath());
        c.add(new File(file).getAbsolutePath());
        // 이름만 주면 JNA 가 jna.library.path · java.library.path · 시스템 경로에서 찾는다.
        c.add("docparse");
        return c;
    }

    /** 연 라이브러리 경로(또는 이름) */
    public String libPath() { return path; }

    /** 라이브러리 ABI 판 */
    public int abiVersion() { return lib.docparse_abi_version(); }

    /** 라이브러리 버전 "0.1.0+abc1234" */
    public String version() { return utf8(lib.docparse_version()); }

    /** 형식 감지: "pdf"·"hwp"·"docx"·…·"unknown". 파일이 없으면 DocParseException(not_found). */
    public String detect(String path) {
        PointerByReference out = new PointerByReference();
        int code = lib.docparse_detect(path, out);
        // detect 가 주는 문자열은 고정 문자열이라 해제하지 않는다.
        String fmt = utf8(out.getValue());
        if (code != DP_OK) throw new DocParseException(code, kindOf(code), "형식 감지 실패: " + path, Collections.<String, Object>emptyMap());
        return fmt;
    }

    /** 기본 옵션(정제함)으로 본문 추출 */
    public Extracted extract(String path) {
        return extract(path, null);
    }

    /**
     * 본문 추출.
     *
     * @param optsJson docparse.h 의 옵션 JSON. null 이면 기본값.
     *                 예: {@code {"clean":true,"limits":{"max_pdf_pages":500}}}
     * @throws SizeLimitException 크기 상한 초과
     * @throws DocParseException  그 밖의 실패(kind 로 구분)
     */
    public Extracted extract(String path, String optsJson) {
        String json = extractJson(path, optsJson);
        return new Extracted(asMap(Json.parse(json)), json);
    }

    /** 본문 추출 — 성공하면 결과 JSON 원문을 그대로 돌려준다(Jackson·Gson 으로 읽을 때). */
    public String extractJson(String path, String optsJson) {
        PointerByReference out = new PointerByReference();
        int code = lib.docparse_extract(path, optsJson, out);
        return checked(code, take(out));
    }

    /** 크기 상한 검사만. 걸리면 {size, limit, text_kind, reason}, 아니면 null. fmt 는 null 이면 감지. */
    public Map<String, Object> checkSize(String path, String fmt, String optsJson) {
        PointerByReference out = new PointerByReference();
        int code = lib.docparse_check_size(path, fmt, optsJson, out);
        return asMap(asMap(Json.parse(checked(code, take(out)))).get("hit"));
    }

    /**
     * 압축 펼치기. 결과는 {jobs:[{src,label,origin}], stats:{unexpanded,unsupported,partial}}.
     *
     * @param optsJson {"max_depth":3,"max_bytes":…,"max_members":…} 또는 null
     */
    public Map<String, Object> expand(List<String> paths, String tmpRoot, String optsJson) {
        StringBuilder b = new StringBuilder("[");
        for (int k = 0; k < paths.size(); k++) {
            if (k > 0) b.append(',');
            b.append(Json.quote(paths.get(k)));
        }
        PointerByReference out = new PointerByReference();
        int code = lib.docparse_expand(b.append(']').toString(), tmpRoot, optsJson, out);
        return asMap(Json.parse(checked(code, take(out))));
    }

    /** 분할 압축 조각이면 안내 문구, 아니면 null. path 는 null 가능. */
    public String splitVolumeHint(String name, String path) {
        PointerByReference out = new PointerByReference();
        int code = lib.docparse_split_volume_hint(name, path, out);
        return (String) asMap(Json.parse(checked(code, take(out)))).get("hint");
    }

    /** 6단계 본문 정제 */
    public String cleanText(String text) {
        PointerByReference out = new PointerByReference();
        int code = lib.docparse_clean_text(text == null ? "" : text, out);
        String s = take(out);
        if (code != DP_OK) throw new DocParseException(code, kindOf(code), s, Collections.<String, Object>emptyMap());
        return s;
    }

    /** 스캔 PDF 판정 — true = 스캔 문서(처리 제외) */
    public boolean scanPdfVerdict(long pages, long scanPages, long bodyChars) {
        return lib.docparse_scan_pdf_verdict(pages, scanPages, bodyChars) == 1;
    }

    /**
     * 로그 받기. logger = null 이면 끈다. maxLevel 0 이하도 끔.
     * 콜백은 추출을 부른 스레드에서 불린다 — 짧게, 스레드 안전하게 쓴다.
     */
    public void setLogger(final Logger logger, int maxLevel) {
        if (logger == null || maxLevel <= 0) {
            lib.docparse_set_logger(null, 0, null);
            loggerRef = null;
            return;
        }
        LogFn fn = new LogFn() {
            public void invoke(int level, Pointer target, Pointer site, Pointer msg, Pointer user) {
                try {
                    logger.log(level, utf8(target), utf8(site), utf8(msg));
                } catch (Throwable t) {
                    // 콜백에서 던진 예외가 C 쪽으로 넘어가면 프로세스가 죽을 수 있어 여기서 삼킨다.
                }
            }
        };
        lib.docparse_set_logger(fn, maxLevel, null);
        loggerRef = fn;
    }

    // ---- 내부 도우미 ----

    // out 으로 받은 문자열을 읽고 바로 해제한다(성공이든 실패든 해제해야 한다).
    private String take(PointerByReference out) {
        Pointer p = out.getValue();
        if (p == null) throw new DocParseException(DP_ERR_PANIC, "panic", "문서파서가 결과를 주지 않았습니다", Collections.<String, Object>emptyMap());
        try {
            return utf8(p);
        } finally {
            lib.docparse_free(p);
        }
    }

    // 코드가 0 이 아니면 결과 JSON 의 error 로 예외를 만든다.
    private static String checked(int code, String json) {
        if (code == DP_OK) return json;
        Map<String, Object> e = Collections.emptyMap();
        try {
            Object err = asMap(Json.parse(json)).get("error");
            if (err != null) e = asMap(err);
        } catch (IllegalArgumentException ignore) {
            // JSON 이 아니면 원문을 메시지로 쓴다.
        }
        String kind = e.get("kind") instanceof String ? (String) e.get("kind") : kindOf(code);
        String msg = e.get("message") instanceof String ? (String) e.get("message") : json;
        if (code == DP_ERR_NOT_FOUND) kind = "not_found";
        if (code == DP_ERR_SIZE_LIMIT) throw new SizeLimitException(code, msg, e);
        throw new DocParseException(code, kind, msg, e);
    }

    private static String kindOf(int code) {
        switch (code) {
            case DP_ERR_ARG: return "arg";
            case DP_ERR_NOT_FOUND: return "not_found";
            case DP_ERR_SIZE_LIMIT: return "size_limit";
            case DP_ERR_UNSUPPORTED: return "unsupported";
            case DP_ERR_PANIC: return "panic";
            default: return "failed";
        }
    }

    private static String utf8(Pointer p) {
        return p == null ? null : p.getString(0, StandardCharsets.UTF_8.name());
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> asMap(Object o) {
        return (Map<String, Object>) o;
    }

    private static long num(Object o) {
        return o instanceof Number ? ((Number) o).longValue() : 0L;
    }
}

package docparse;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * docparse 결과 JSON 을 읽는 아주 작은 해석기 (외부 라이브러리 없이 쓰려고).
 *
 * <p>객체 → {@code Map<String,Object>}, 배열 → {@code List<Object>}, 문자열 → String,
 * 정수 → Long, 실수 → Double, true/false → Boolean, null → null.
 * Jackson·Gson 을 이미 쓰는 프로젝트라면 {@link DocParse} 의 *Json 메서드가 주는
 * 원문 문자열을 그쪽으로 읽어도 된다.
 */
public final class Json {
    private final String s;
    private int i;

    private Json(String s) { this.s = s; }

    /** JSON 문자열 하나를 읽는다. 형식이 틀리면 IllegalArgumentException. */
    public static Object parse(String text) {
        Json p = new Json(text);
        p.ws();
        Object v = p.value();
        p.ws();
        if (p.i != p.s.length()) throw p.err("뒤에 남은 글자");
        return v;
    }

    /** 문자열 값을 JSON 문자열 표기로 바꾼다("…" 와 이스케이프). null 은 "null". */
    public static String quote(String v) {
        if (v == null) return "null";
        StringBuilder b = new StringBuilder(v.length() + 2).append('"');
        for (int k = 0; k < v.length(); k++) {
            char c = v.charAt(k);
            switch (c) {
                case '"': b.append("\\\""); break;
                case '\\': b.append("\\\\"); break;
                case '\n': b.append("\\n"); break;
                case '\r': b.append("\\r"); break;
                case '\t': b.append("\\t"); break;
                default:
                    if (c < 0x20) b.append(String.format("\\u%04x", (int) c));
                    else b.append(c);
            }
        }
        return b.append('"').toString();
    }

    private IllegalArgumentException err(String why) {
        return new IllegalArgumentException("JSON 해석 실패(" + why + ", 위치 " + i + ")");
    }

    private void ws() {
        while (i < s.length() && " \t\r\n".indexOf(s.charAt(i)) >= 0) i++;
    }

    private Object value() {
        if (i >= s.length()) throw err("끝");
        char c = s.charAt(i);
        switch (c) {
            case '{': return object();
            case '[': return array();
            case '"': return string();
            case 't': lit("true"); return Boolean.TRUE;
            case 'f': lit("false"); return Boolean.FALSE;
            case 'n': lit("null"); return null;
            default: return number();
        }
    }

    private void lit(String w) {
        if (!s.startsWith(w, i)) throw err("모르는 값");
        i += w.length();
    }

    private Map<String, Object> object() {
        Map<String, Object> m = new LinkedHashMap<String, Object>();
        i++;
        ws();
        if (s.charAt(i) == '}') { i++; return m; }
        while (true) {
            ws();
            String k = string();
            ws();
            if (s.charAt(i++) != ':') throw err("':' 없음");
            ws();
            m.put(k, value());
            ws();
            char c = s.charAt(i++);
            if (c == '}') return m;
            if (c != ',') throw err("',' 없음");
        }
    }

    private List<Object> array() {
        List<Object> a = new ArrayList<Object>();
        i++;
        ws();
        if (s.charAt(i) == ']') { i++; return a; }
        while (true) {
            ws();
            a.add(value());
            ws();
            char c = s.charAt(i++);
            if (c == ']') return a;
            if (c != ',') throw err("',' 없음");
        }
    }

    private String string() {
        if (s.charAt(i) != '"') throw err("문자열 아님");
        i++;
        StringBuilder b = new StringBuilder();
        while (true) {
            char c = s.charAt(i++);
            if (c == '"') return b.toString();
            if (c != '\\') { b.append(c); continue; }
            char e = s.charAt(i++);
            switch (e) {
                case '"': b.append('"'); break;
                case '\\': b.append('\\'); break;
                case '/': b.append('/'); break;
                case 'b': b.append('\b'); break;
                case 'f': b.append('\f'); break;
                case 'n': b.append('\n'); break;
                case 'r': b.append('\r'); break;
                case 't': b.append('\t'); break;
                case 'u': b.append((char) Integer.parseInt(s.substring(i, i + 4), 16)); i += 4; break;
                default: throw err("모르는 이스케이프");
            }
        }
    }

    private Object number() {
        int st = i;
        while (i < s.length() && "+-0123456789.eE".indexOf(s.charAt(i)) >= 0) i++;
        String t = s.substring(st, i);
        if (t.isEmpty()) throw err("숫자 아님");
        if (t.indexOf('.') >= 0 || t.indexOf('e') >= 0 || t.indexOf('E') >= 0) return Double.valueOf(t);
        return Long.valueOf(t);
    }
}

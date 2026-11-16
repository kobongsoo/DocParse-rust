import docparse.DocParse;

import java.io.File;
import java.util.Arrays;
import java.util.List;
import java.util.Map;

/**
 * docparse Java 예제 — 압축은 펼치고, 나온 문서마다 본문 추출.
 *
 * <pre>
 * javac -encoding UTF-8 -cp jna-5.14.0.jar docparse/*.java Example.java
 * java  -cp "jna-5.14.0.jar;." Example a.hwp b.zip        (Linux 는 ';' 대신 ':')
 * </pre>
 */
public class Example {
    public static void main(String[] args) throws Exception {
        DocParse dp = DocParse.load();
        System.out.println("docparse " + dp.version() + " (" + dp.libPath() + ")");

        // 경고 이상 로그만 받아 표준오류로 내보낸다.
        dp.setLogger(new DocParse.Logger() {
            public void log(int level, String target, String site, String message) {
                System.err.println("[docparse " + level + "] " + message);
            }
        }, 2);

        // 압축(zip·7z·rar·tar·gz·…)은 펼쳐서 안의 문서를, 압축이 아닌 파일은 그대로 job 으로 돌려준다.
        File tmp = new File(System.getProperty("java.io.tmpdir"), "docparse-" + System.nanoTime());
        tmp.mkdirs();
        Map<String, Object> r = dp.expand(Arrays.asList(args), tmp.getPath(), "{\"max_depth\":3}");
        @SuppressWarnings("unchecked")
        List<Map<String, Object>> jobs = (List<Map<String, Object>>) r.get("jobs");
        for (Map<String, Object> j : jobs) {
            show(dp, (String) j.get("src"), (String) j.get("label"));
        }
        // 펼치지 못한 압축(암호·깨짐·상한 초과)은 stats 에 남는다.
        System.out.println("펼치지 못함: " + ((Map<?, ?>) r.get("stats")).get("unexpanded"));
    }

    static void show(DocParse dp, String src, String label) {
        try {
            DocParse.Extracted r = dp.extract(src, "{\"clean\":true,\"label\":" + docparse.Json.quote(label) + "}");
            String head = r.text.length() > 40 ? r.text.substring(0, 40) : r.text;
            System.out.println(label + " : " + r.fmt + " " + r.text.length() + "자 | " + head.replace('\n', ' '));
        } catch (DocParse.SizeLimitException e) {
            System.out.println(label + " : 크기 초과 " + e.size + " > " + e.limit);
        } catch (DocParse.DocParseException e) {
            System.out.println(label + " : " + e.kind + " — " + e.getMessage());
        }
    }
}

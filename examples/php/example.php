<?php
// docparse PHP 예제 — 압축은 펼치고, 나온 문서마다 본문 추출.
//   php -d extension=ffi example.php a.hwp b.zip
require __DIR__ . '/DocParse.php';

$dp = DocParse::load();
echo 'docparse ', $dp->version(), ' (', $dp->libPath(), ")\n";

// 경고 이상 로그만 표준오류로
$dp->setLogger(function ($level, $target, $site, $msg) {
    fwrite(STDERR, "[docparse $level] $msg\n");
}, 2);

// 한 파일 추출해 요약 한 줄 찍기
function show(DocParse $dp, string $src, string $label): void
{
    try {
        $r = $dp->extract($src, ['clean' => true, 'label' => $label]);
        $head = str_replace("\n", ' ', mb_substr($r['text'], 0, 40));
        printf("%s : %s %d자 | %s\n", $label, $r['fmt'], mb_strlen($r['text']), $head);
    } catch (DocParseSizeLimitException $e) {
        printf("%s : 크기 초과 %d > %d\n", $label, $e->size, $e->limit);
    } catch (DocParseException $e) {
        printf("%s : %s — %s\n", $label, $e->kind, $e->getMessage());
    }
}

// 압축(zip·7z·rar·tar·gz·…)은 펼쳐서 안의 문서를, 압축이 아닌 파일은 그대로 job 으로 돌려준다.
$tmp = sys_get_temp_dir() . DIRECTORY_SEPARATOR . 'docparse-' . uniqid();
mkdir($tmp);
$r = $dp->expand(array_slice($argv, 1), $tmp, ['max_depth' => 3]);
foreach ($r['jobs'] as $j) {
    show($dp, $j['src'], $j['label']);
}
// 펼치지 못한 압축(암호·깨짐·상한 초과)은 stats 에 남는다.
echo '펼치지 못함: ', json_encode($r['stats']['unexpanded'], JSON_UNESCAPED_UNICODE), "\n";

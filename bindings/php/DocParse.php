<?php
/**
 * docparse.dll / libdocparse.so 의 PHP 감싸개 (PHP 7.4 이상, FFI 확장).
 *
 * 준비
 *   - php.ini 에 extension=ffi (Windows 는 php_ffi.dll 이 ext/ 에 있다)
 *   - CLI 는 기본 설정(ffi.enable=preload)으로 바로 된다.
 *     웹(php-fpm·Apache 모듈)에서 쓰려면 ffi.enable=true 로 두거나 opcache.preload 로 미리 불러야 한다.
 *
 * 사용
 *   require 'DocParse.php';
 *   $dp = DocParse::load();                    // DOCPARSE_LIB → ./lib/ → 이 파일 옆 → 시스템
 *   $r  = $dp->extract('a.hwp');               // ['text'=>…, 'engine'=>…, 'fmt'=>…, 'partial'=>…, 'layout'=>…]
 *   echo $r['fmt'], ' ', mb_strlen($r['text']), "\n";
 *
 * 규칙은 C 헤더 docparse.h 와 같다. out 으로 받은 문자열은 이 클래스가 읽은 즉시 docparse_free 로 돌려준다.
 */

/** 추출 실패. $kind = size_limit|unsupported|failed|arg|panic|not_found */
class DocParseException extends RuntimeException
{
    /** @var string */
    public $kind;
    /** @var array 결과 JSON 의 "error" 객체 전체 */
    public $detail;

    public function __construct(int $code, string $kind, string $message, array $detail = [])
    {
        parent::__construct($message, $code);
        $this->kind = $kind;
        $this->detail = $detail;
    }
}

/** 크기 상한 초과 — 깨진 파일(failed)과 구분해 처리하라고 따로 둔다. */
class DocParseSizeLimitException extends DocParseException
{
    /** @var int */
    public $size;
    /** @var int */
    public $limit;
    /** @var bool */
    public $textKind;

    public function __construct(int $code, string $message, array $detail)
    {
        parent::__construct($code, 'size_limit', $message, $detail);
        $this->size = (int)($detail['size'] ?? 0);
        $this->limit = (int)($detail['limit'] ?? 0);
        $this->textKind = !empty($detail['text_kind']);
    }
}

final class DocParse
{
    /** 이 감싸개가 아는 ABI 판. 라이브러리의 docparse_abi_version() 과 같아야 한다. */
    const ABI_VERSION = 1;

    const DP_OK = 0;
    const DP_ERR_ARG = 1;
    const DP_ERR_NOT_FOUND = 2;
    const DP_ERR_SIZE_LIMIT = 3;
    const DP_ERR_UNSUPPORTED = 4;
    const DP_ERR_FAILED = 5;
    const DP_ERR_PANIC = 99;

    // FFI::cdef 는 전처리기(#include·#define·#ifdef)를 못 읽으므로 docparse.h 의 선언만 옮겨 둔다.
    const CDEF = <<<'C'
uint32_t    docparse_abi_version(void);
const char* docparse_version(void);
int         docparse_detect(const char* path, const char** out_fmt);
int         docparse_extract(const char* path, const char* opts_json, char** out_json);
int         docparse_check_size(const char* path, const char* fmt, const char* opts_json, char** out_json);
int         docparse_expand(const char* paths_json, const char* tmp_root, const char* opts_json, char** out_json);
int         docparse_split_volume_hint(const char* name, const char* path, char** out_json);
int         docparse_clean_text(const char* text, char** out_text);
int         docparse_scan_pdf_verdict(uint64_t pages, uint64_t scan_pages, uint64_t body_chars);
void        docparse_free(char* p);
typedef void (*dp_log_fn)(int level, const char* target, const char* site, const char* msg, void* user);
void        docparse_set_logger(dp_log_fn fn, int max_level, void* user);
C;

    /** @var FFI */
    private $ffi;
    /** @var string */
    private $path;
    /** @var callable|null 콜백을 PHP 가 지우지 않게 붙잡아 둔다 */
    private $loggerRef = null;

    private function __construct(FFI $ffi, string $path)
    {
        $this->ffi = $ffi;
        $this->path = $path;
    }

    /**
     * 라이브러리를 연다. $path 를 주면 그 파일만 연다.
     * 기본 순서: DOCPARSE_LIB 환경변수 → ./lib/ → ./ → 이 파일 폴더(와 그 lib/) → 이름만(시스템 경로).
     *
     * @throws RuntimeException 못 찾았거나 ABI 판이 다를 때
     */
    public static function load(?string $path = null): self
    {
        if (!extension_loaded('ffi')) {
            throw new RuntimeException('PHP FFI 확장이 꺼져 있습니다 (php.ini 에 extension=ffi)');
        }
        $file = PHP_OS_FAMILY === 'Windows' ? 'docparse.dll' : 'libdocparse.so';
        if ($path !== null) {
            $cands = [$path];
        } else {
            $cands = [];
            $env = getenv('DOCPARSE_LIB');
            if ($env) $cands[] = $env;
            $cands[] = getcwd() . DIRECTORY_SEPARATOR . 'lib' . DIRECTORY_SEPARATOR . $file;
            $cands[] = getcwd() . DIRECTORY_SEPARATOR . $file;
            $cands[] = __DIR__ . DIRECTORY_SEPARATOR . $file;
            $cands[] = __DIR__ . DIRECTORY_SEPARATOR . 'lib' . DIRECTORY_SEPARATOR . $file;
            $cands[] = dirname(__DIR__) . DIRECTORY_SEPARATOR . 'lib' . DIRECTORY_SEPARATOR . $file;
            $cands[] = $file;
        }
        $errors = [];
        foreach ($cands as $c) {
            // 경로가 들어 있는데 파일이 없으면 열어 보지도 않는다.
            if ((strpbrk($c, '/\\') !== false) && !is_file($c)) { $errors[] = $c; continue; }
            try {
                $ffi = FFI::cdef(self::CDEF, $c);
            } catch (FFI\Exception $e) {
                $errors[] = $c . ' (' . $e->getMessage() . ')';
                continue;
            }
            $abi = $ffi->docparse_abi_version();
            if ($abi !== self::ABI_VERSION) {
                throw new RuntimeException("docparse ABI 판이 다릅니다: 라이브러리 $abi, 감싸개 " . self::ABI_VERSION . " ($c)");
            }
            return new self($ffi, $c);
        }
        throw new RuntimeException('docparse 라이브러리를 찾지 못했습니다. 찾아본 곳: ' . implode(', ', $errors));
    }

    /** 연 라이브러리 경로 */
    public function libPath(): string { return $this->path; }

    /** 라이브러리 ABI 판 */
    public function abiVersion(): int { return $this->ffi->docparse_abi_version(); }

    /** 라이브러리 버전 "0.1.0+abc1234" (const char* 반환은 FFI 가 PHP 문자열로 바꿔 준다) */
    public function version(): string { return $this->ffi->docparse_version(); }

    /** 형식 감지: "pdf"·"hwp"·"docx"·"zip"·…·"unknown" */
    public function detect(string $path): string
    {
        $out = $this->ffi->new('const char*');
        $code = $this->ffi->docparse_detect($path, FFI::addr($out));
        // detect 가 주는 문자열은 고정 문자열이라 해제하지 않는다.
        $fmt = FFI::isNull($out) ? 'unknown' : FFI::string($out);
        if ($code !== self::DP_OK) {
            throw new DocParseException($code, self::kindOf($code), "형식 감지 실패: $path");
        }
        return $fmt;
    }

    /**
     * 본문 추출.
     *
     * @param array|null $opts docparse.h 의 옵션(배열). 예: ['clean'=>true, 'limits'=>['max_pdf_pages'=>500]]
     * @return array ['text'=>string, 'engine'=>string, 'fmt'=>string, 'partial'=>?array, 'layout'=>?array]
     * @throws DocParseSizeLimitException 크기 상한 초과
     * @throws DocParseException          그 밖의 실패($e->kind 로 구분)
     */
    public function extract(string $path, ?array $opts = null): array
    {
        $out = $this->ffi->new('char*');
        $code = $this->ffi->docparse_extract($path, self::opts($opts), FFI::addr($out));
        return $this->checked($code, $this->take($out));
    }

    /** 크기 상한 검사만. 걸리면 ['size','limit','text_kind','reason'], 아니면 null. */
    public function checkSize(string $path, ?string $fmt = null, ?array $opts = null): ?array
    {
        $out = $this->ffi->new('char*');
        $code = $this->ffi->docparse_check_size($path, $fmt, self::opts($opts), FFI::addr($out));
        return $this->checked($code, $this->take($out))['hit'] ?? null;
    }

    /**
     * 압축 펼치기.
     * @return array ['jobs'=>[['src','label','origin'],…], 'stats'=>['unexpanded'=>…,'unsupported'=>…,'partial'=>…]]
     */
    public function expand(array $paths, string $tmpRoot, ?array $opts = null): array
    {
        $out = $this->ffi->new('char*');
        $pj = json_encode(array_values($paths), JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES);
        $code = $this->ffi->docparse_expand($pj, $tmpRoot, self::opts($opts), FFI::addr($out));
        return $this->checked($code, $this->take($out));
    }

    /** 분할 압축 조각이면 안내 문구, 아니면 null */
    public function splitVolumeHint(string $name, ?string $path = null): ?string
    {
        $out = $this->ffi->new('char*');
        $code = $this->ffi->docparse_split_volume_hint($name, $path, FFI::addr($out));
        return $this->checked($code, $this->take($out))['hint'] ?? null;
    }

    /** 6단계 본문 정제 */
    public function cleanText(string $text): string
    {
        $out = $this->ffi->new('char*');
        $code = $this->ffi->docparse_clean_text($text, FFI::addr($out));
        $s = $this->take($out);
        if ($code !== self::DP_OK) throw new DocParseException($code, self::kindOf($code), $s);
        return $s;
    }

    /** 스캔 PDF 판정 — true = 스캔 문서(처리 제외) */
    public function scanPdfVerdict(int $pages, int $scanPages, int $bodyChars): bool
    {
        return $this->ffi->docparse_scan_pdf_verdict($pages, $scanPages, $bodyChars) === 1;
    }

    /**
     * 로그 받기. $fn(int $level, string $target, string $site, string $msg). null 이면 끈다.
     * PHP FFI 콜백은 PHP 를 부른 그 스레드에서만 안전하다 — PHP 는 보통 한 스레드라 문제없다.
     */
    public function setLogger(?callable $fn, int $maxLevel = 3): void
    {
        if ($fn === null || $maxLevel <= 0) {
            $this->ffi->docparse_set_logger(null, 0, null);
            $this->loggerRef = null;
            return;
        }
        $cb = function ($level, $target, $site, $msg, $user) use ($fn) {
            // 콜백에서 던진 예외가 C 쪽으로 넘어가면 안 되므로 여기서 삼킨다.
            try { $fn($level, (string)$target, (string)$site, (string)$msg); } catch (\Throwable $e) {}
        };
        $this->ffi->docparse_set_logger($cb, $maxLevel, null);
        $this->loggerRef = $cb;
    }

    // ---- 내부 도우미 ----

    // 옵션 배열 → JSON (null 이면 NULL 을 넘겨 라이브러리 기본값)
    private static function opts(?array $opts): ?string
    {
        if ($opts === null) return null;
        // 빈 배열은 [] 가 아니라 {} 로 보내야 한다.
        return $opts === [] ? '{}' : json_encode($opts, JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES);
    }

    // out 으로 받은 문자열을 읽고 바로 해제한다(성공이든 실패든 해제해야 한다).
    private function take($out): string
    {
        if (FFI::isNull($out)) {
            throw new DocParseException(self::DP_ERR_PANIC, 'panic', '문서파서가 결과를 주지 않았습니다');
        }
        try {
            return FFI::string($out);
        } finally {
            $this->ffi->docparse_free($out);
        }
    }

    // 코드가 0 이 아니면 결과 JSON 의 error 로 예외를 만든다.
    private function checked(int $code, string $json): array
    {
        $v = json_decode($json, true);
        if ($code === self::DP_OK) {
            if (!is_array($v)) throw new DocParseException(self::DP_ERR_PANIC, 'panic', "결과 JSON 을 읽지 못했습니다: $json");
            return $v;
        }
        $e = (is_array($v) && isset($v['error']) && is_array($v['error'])) ? $v['error'] : [];
        $msg = isset($e['message']) ? (string)$e['message'] : $json;
        if ($code === self::DP_ERR_SIZE_LIMIT) throw new DocParseSizeLimitException($code, $msg, $e);
        $kind = $code === self::DP_ERR_NOT_FOUND ? 'not_found' : (string)($e['kind'] ?? self::kindOf($code));
        throw new DocParseException($code, $kind, $msg, $e);
    }

    private static function kindOf(int $code): string
    {
        switch ($code) {
            case self::DP_ERR_ARG: return 'arg';
            case self::DP_ERR_NOT_FOUND: return 'not_found';
            case self::DP_ERR_SIZE_LIMIT: return 'size_limit';
            case self::DP_ERR_UNSUPPORTED: return 'unsupported';
            case self::DP_ERR_PANIC: return 'panic';
            default: return 'failed';
        }
    }
}

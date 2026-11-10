# -*- coding: utf-8 -*-
"""docparse — docparse.dll / libdocparse.so 를 ctypes 로 불러 쓰는 Python 감싸개

정본: DocParse-rust/bindings/python/docparse.py — 쓰는 프로젝트는 이 파일을 복사해 둔다.
표준 라이브러리만 쓴다(ctypes·json). Python 3.8 이상.

    import docparse
    r = docparse.extract("a.hwp", clean=False)
    r.text, r.engine, r.fmt, r.partial, r.layout

라이브러리를 찾는 순서: load(path) 로 준 경로 → 환경변수 DOCPARSE_LIB(파일·폴더)
→ 이 .py 파일 폴더 → 그 아래 lib/ → PyInstaller 묶음 폴더(sys._MEIPASS) → 실행파일 폴더.
ctypes.CDLL 은 호출하는 동안 GIL 을 놓으므로 여러 스레드에서 동시에 불러도 병렬로 돈다.
"""
import ctypes
import json
import os
import sys
import threading

ABI_VERSION = 1

__all__ = [
    "ExtractError", "SizeLimitError", "Extracted", "load", "loaded", "lib_path", "version",
    "detect_format", "extract", "check_size", "clean_text", "expand_paths",
    "split_volume_hint", "scan_pdf_verdict", "set_logger",
]

_DP_OK = 0

LOG_FN = ctypes.CFUNCTYPE(None, ctypes.c_int, ctypes.c_char_p, ctypes.c_char_p,
                          ctypes.c_char_p, ctypes.c_void_p)


#------------------------------------------------------------------
# 추출 실패 예외
#=> 문서에서 본문을 뽑지 못했다(깨짐·미지원·파서 실패). kind 에 라이브러리가 준 종류가 있다.
# -필드: kind = "unsupported" | "failed" | "arg" | "panic" | "size_limit"
#------------------------------------------------------------------
class ExtractError(Exception):
    kind = "failed"


#------------------------------------------------------------------
# 크기 상한 초과 예외
#=> 크기 상한을 넘어 읽지 않았다(깨진 파일과 구분). ExtractError 의 한 종류다.
# -필드: size      = 파일 크기(바이트)
# -필드: limit     = 걸린 상한(바이트)
# -필드: text_kind = 텍스트류 상한이었는지
#------------------------------------------------------------------
class SizeLimitError(ExtractError):
    kind = "size_limit"
    size = 0
    limit = 0
    text_kind = False


#------------------------------------------------------------------
# 추출 결과
#=> 라이브러리 결과 JSON 을 속성으로 읽기 쉽게 담는다.
# -필드: text    = 본문
# -필드: engine  = 실제로 뽑은 엔진 이름("docx"·"pdf"·"snf" …)
# -필드: fmt     = 감지(또는 지정)된 형식 이름
# -필드: partial = None 또는 (reason, unit, read, total) — 상한에서 멈춰 앞부분만 읽음
# -필드: layout  = None 또는 (pages, scan_pages) — PDF 페이지 구성
#------------------------------------------------------------------
class Extracted(object):
    __slots__ = ("text", "engine", "fmt", "partial", "layout")

    #------------------------------------------------------------------
    # 결과 JSON → 속성
    #
    # -in: d = 라이브러리 성공 결과(dict)
    #
    # -out: 없음
    # -out: error = 없음
    #------------------------------------------------------------------
    def __init__(self, d):
        self.text = d.get("text") or ""
        self.engine = d.get("engine") or ""
        self.fmt = d.get("fmt") or "unknown"
        p = d.get("partial")
        # PyO3 판과 같은 모양(튜플)으로 맞춘다 — 부르는 쪽 코드를 그대로 쓰기 위해.
        self.partial = (p["reason"], p["unit"], p["read"], p["total"]) if p else None
        lay = d.get("layout")
        self.layout = (lay["pages"], lay["scan_pages"]) if lay else None

    #------------------------------------------------------------------
    # 디버그용 표시
    #
    # -in: 없음
    #
    # -out: str
    # -out: error = 없음
    #------------------------------------------------------------------
    def __repr__(self):
        return "<Extracted engine=%s fmt=%s chars=%d partial=%r layout=%r>" % (
            self.engine, self.fmt, len(self.text), self.partial, self.layout)


_lock = threading.Lock()
_lib = None
_lib_path = None
_version = None
_logger_ref = None  # ctypes 콜백 객체 — GC 되면 라이브러리가 죽은 포인터를 부르므로 붙잡아 둔다


#------------------------------------------------------------------
# 이 OS 의 라이브러리 파일 이름
#
# -in: 없음
#
# -out: str = "docparse.dll" | "libdocparse.so" | "libdocparse.dylib"
# -out: error = 없음
#------------------------------------------------------------------
def _lib_name():
    if sys.platform.startswith("win"):
        return "docparse.dll"
    if sys.platform == "darwin":
        return "libdocparse.dylib"
    return "libdocparse.so"


#------------------------------------------------------------------
# 라이브러리 후보 경로들
#=> 모듈 설명의 찾는 순서대로 후보를 만든다. 폴더가 오면 그 안의 라이브러리 이름을 붙인다.
#
# -in: path = 직접 지정한 파일·폴더(없으면 None)
#
# -out: list[str] = 후보 경로들(앞에서부터 본다)
# -out: error = 없음
#------------------------------------------------------------------
def _candidates(path=None):
    name = _lib_name()
    out = []

    # 파일이면 그대로, 폴더면 라이브러리 이름을 붙여 후보에 넣는다
    def add(p):
        if p:
            out.append(os.path.join(p, name) if os.path.isdir(p) else p)

    add(path)
    add(os.environ.get("DOCPARSE_LIB"))
    here = os.path.dirname(os.path.abspath(__file__))
    add(here)
    add(os.path.join(here, "lib"))
    add(getattr(sys, "_MEIPASS", None))
    add(os.path.dirname(os.path.abspath(sys.executable)))
    return out


#------------------------------------------------------------------
# 함수 인자·반환 모양 등록
#=> ctypes 는 기본으로 int 를 가정하므로 포인터를 다루는 함수는 모양을 꼭 적어야 한다
#   (64비트에서 포인터가 잘린다).
#
# -in: d = ctypes.CDLL
#
# -out: 없음
# -out: error = 함수가 없으면 AttributeError
#------------------------------------------------------------------
def _declare(d):
    P = ctypes.POINTER
    c, cp, vp = ctypes.c_int, ctypes.c_char_p, ctypes.c_void_p
    d.docparse_abi_version.restype = ctypes.c_uint32
    d.docparse_abi_version.argtypes = []
    d.docparse_version.restype = cp
    d.docparse_version.argtypes = []
    d.docparse_detect.restype = c
    d.docparse_detect.argtypes = [cp, P(cp)]
    # out 문자열은 c_void_p 로 받아야 ctypes 가 bytes 로 바꿔 버리지 않고 해제할 포인터가 남는다.
    d.docparse_extract.restype = c
    d.docparse_extract.argtypes = [cp, cp, P(vp)]
    d.docparse_check_size.restype = c
    d.docparse_check_size.argtypes = [cp, cp, cp, P(vp)]
    d.docparse_expand.restype = c
    d.docparse_expand.argtypes = [cp, cp, cp, P(vp)]
    d.docparse_split_volume_hint.restype = c
    d.docparse_split_volume_hint.argtypes = [cp, cp, P(vp)]
    d.docparse_clean_text.restype = c
    d.docparse_clean_text.argtypes = [cp, P(vp)]
    d.docparse_scan_pdf_verdict.restype = c
    d.docparse_scan_pdf_verdict.argtypes = [ctypes.c_uint64, ctypes.c_uint64, ctypes.c_uint64]
    d.docparse_free.restype = None
    d.docparse_free.argtypes = [vp]
    d.docparse_set_logger.restype = None
    d.docparse_set_logger.argtypes = [LOG_FN, c, vp]


#------------------------------------------------------------------
# 라이브러리 불러오기
#=> 한 번 불러오면 프로세스가 끝날 때까지 쓴다(다시 부르면 그대로 돌려준다).
#    1) 후보를 차례로 보며 있는 파일을 연다(path 를 줬으면 그것만 본다)
#    2) ABI 버전 확인 — 다르면 RuntimeError
#    3) 함수 모양 등록, 버전 읽기
#
# -in: path = 라이브러리 파일·폴더(None 이면 찾는 순서대로)
#
# -out: ctypes.CDLL = 불러온 라이브러리
# -out: error = 못 찾으면 OSError(찾아본 곳 포함), ABI 가 다르면 RuntimeError
#------------------------------------------------------------------
def load(path=None):
    global _lib, _lib_path, _version
    with _lock:
        if _lib is not None:
            return _lib
        cands = _candidates(path)
        if path:
            cands = cands[:1]
        found = next((p for p in cands if os.path.isfile(p)), None)
        if not found:
            raise OSError("문서파서 라이브러리(%s)를 찾을 수 없습니다 — 찾아본 곳: %s "
                          "(환경변수 DOCPARSE_LIB 로 알려 주세요)" % (_lib_name(), " , ".join(cands)))
        d = ctypes.CDLL(found)
        d.docparse_abi_version.restype = ctypes.c_uint32
        got = d.docparse_abi_version()
        if got != ABI_VERSION:
            raise RuntimeError("문서파서 라이브러리의 ABI 버전이 다릅니다: 라이브러리 %d / 감싸개 %d (%s)"
                               % (got, ABI_VERSION, found))
        _declare(d)
        _version = d.docparse_version().decode("utf-8", "replace")
        _lib, _lib_path = d, found
        return d


#------------------------------------------------------------------
# 불러왔는지
#
# -in: 없음
#
# -out: bool = 이미 불러왔으면 True
# -out: error = 없음
#------------------------------------------------------------------
def loaded():
    return _lib is not None


#------------------------------------------------------------------
# 불러온 라이브러리 파일 경로
#
# -in: 없음
#
# -out: str = 경로(필요하면 이때 불러온다)
# -out: error = load 와 같음
#------------------------------------------------------------------
def lib_path():
    load()
    return _lib_path


#------------------------------------------------------------------
# 라이브러리 버전 ("0.1.0+abc1234")
#
# -in: 없음
#
# -out: str = 버전
# -out: error = load 와 같음
#------------------------------------------------------------------
def version():
    load()
    return _version


#------------------------------------------------------------------
# docparse.__version__ — 처음 읽을 때 라이브러리를 불러온다(PEP 562)
#
# -in: name = 속성 이름
#
# -out: str = __version__ 이면 버전
# -out: error = 다른 이름이면 AttributeError
#------------------------------------------------------------------
def __getattr__(name):
    if name == "__version__":
        return version()
    raise AttributeError(name)


#------------------------------------------------------------------
# 경로·문자열 → UTF-8 바이트
#=> 라이브러리는 UTF-8 만 받는다. os.PathLike 도 받는다. None 은 None 그대로.
#
# -in: s = str | bytes | os.PathLike | None
#
# -out: bytes | None
# -out: error = 없음(유닉스의 깨진 이름은 surrogateescape 로 원래 바이트를 살린다)
#------------------------------------------------------------------
def _b(s):
    if s is None:
        return None
    if isinstance(s, bytes):
        return s
    return os.fspath(s).encode("utf-8", "surrogateescape")


#------------------------------------------------------------------
# JSON 결과 함수 한 번 부르기(내부 도우미)
#=> out 자리를 만들어 부르고, 받은 문자열을 읽어 해제한 뒤 (코드, 문자열) 을 돌려준다.
#
# -in: fn   = 라이브러리 함수
# -in: args = out 앞의 인자들
#
# -out: (int, str) = 반환 코드와 결과 문자열
# -out: error = 결과가 비면 ExtractError(panic)
#------------------------------------------------------------------
def _call(fn, *args):
    out = ctypes.c_void_p()
    code = fn(*args, ctypes.byref(out))
    if not out.value:
        e = ExtractError("문서파서가 결과를 주지 않았습니다(코드 %d)" % code)
        e.kind = "panic"
        raise e
    try:
        s = ctypes.string_at(out.value).decode("utf-8", "replace")
    finally:
        _lib.docparse_free(out)
    return code, s


#------------------------------------------------------------------
# 결과 JSON 의 error → 예외
#
# -in: e = {"kind":…, "message":…, …}
#
# -out: ExtractError | SizeLimitError
# -out: error = 없음
#------------------------------------------------------------------
def _error(e):
    kind = e.get("kind") or "failed"
    msg = e.get("message") or ""
    if kind == "size_limit":
        x = SizeLimitError(msg)
        x.size, x.limit, x.text_kind = e.get("size", 0), e.get("limit", 0), bool(e.get("text_kind"))
        return x
    x = ExtractError(msg)
    x.kind = kind
    return x


#------------------------------------------------------------------
# 상한 하나를 옵션 JSON 에 넣기(내부 도우미)
#=> None 이면 키를 빼서 라이브러리 기본값을 쓰게 하고, 0 이하는 '상한 없음'(0)으로 넣는다.
#
# -in: d   = 옵션 dict
# -in: key = 키 이름
# -in: v   = 값(None·숫자)
#
# -out: 없음
# -out: error = 없음
#------------------------------------------------------------------
def _put(d, key, v):
    if v is not None:
        d[key] = v if v > 0 else 0


#------------------------------------------------------------------
# 형식 감지
#
# -in: path = 파일 경로
#
# -out: str = "pdf"·"hwp"·"docx"·…·"unknown"
# -out: error = load 실패 시 OSError/RuntimeError, 그 밖에는 "unknown"
#------------------------------------------------------------------
def detect_format(path):
    load()
    out = ctypes.c_char_p()
    _lib.docparse_detect(_b(path), ctypes.byref(out))
    return out.value.decode("ascii") if out.value else "unknown"


#------------------------------------------------------------------
# 본문 추출
#=> 인자 모양은 예전 PyO3 판(docparse 확장 모듈)과 같다.
#    1) 상한은 None = 라이브러리 기본값, 0 이하 = 상한 없음
#    2) engine 이 "synap_fallback"·"synap_only" 면 synap_exe 필요
#
# -in: path                = 파일 경로
# -in: fmt                 = 형식 이름(None 이면 감지)
# -in: clean               = True 면 6단계 정제까지(기본 False = 원시 텍스트)
# -in: parser_timeout      = 파서 시간 상한(초)
# -in: max_pdf_pages       = PDF 쪽수 상한
# -in: max_file_bytes      = 파일 크기 상한(바이트)
# -in: max_file_bytes_text = 텍스트류 파일 크기 상한(바이트)
# -in: pdfium_lib          = pdfium 라이브러리 경로(None = 이 DLL 폴더 → 실행파일 폴더 → 시스템)
# -in: engine              = "builtin"(기본) | "synap_fallback" | "synap_only"
# -in: synap_exe           = 사이냅 실행파일 경로
# -in: snf_timeout         = 사이냅 시간 상한(초)
# -in: min_text_len        = 사이냅 폴백 '짧은 결과' 기준 글자 수
# -in: label               = 로그에 적을 이름
#
# -out: Extracted = 추출 결과
# -out: error = 크기 상한이면 SizeLimitError, 그 밖의 실패는 ExtractError
#               (자체 파서가 사유 없이 못 읽으면 "자체 파서가 읽지 못했습니다(감지=…)")
#------------------------------------------------------------------
def extract(path, fmt=None, clean=False, parser_timeout=None, max_pdf_pages=None,
            max_file_bytes=None, max_file_bytes_text=None, pdfium_lib=None,
            engine="builtin", synap_exe=None, snf_timeout=None, min_text_len=None, label=None):
    load()
    lim = {}
    _put(lim, "parser_timeout_secs", parser_timeout)
    _put(lim, "max_pdf_pages", max_pdf_pages)
    _put(lim, "max_file_bytes", max_file_bytes)
    _put(lim, "max_file_bytes_text", max_file_bytes_text)
    _put(lim, "snf_timeout_secs", snf_timeout)
    if min_text_len is not None:
        lim["min_text_len"] = int(min_text_len)
    opts = {"clean": bool(clean), "engine": engine, "limits": lim}
    for k, v in (("fmt", fmt), ("synap_exe", synap_exe), ("label", label)):
        if v is not None:
            opts[k] = os.fspath(v) if k == "synap_exe" else v
    if pdfium_lib is not None:
        opts["pdfium_lib"] = os.fspath(pdfium_lib)
    _code, s = _call(_lib.docparse_extract, _b(path), json.dumps(opts).encode("utf-8"))
    d = json.loads(s)
    e = d.get("error")
    if e is not None:
        # 자체 파서가 사유 없이 못 읽은 경우 — 예전 PyO3 판과 같은 문장을 낸다.
        if e.get("kind") == "failed" and not e.get("message"):
            e = dict(e, message="자체 파서가 읽지 못했습니다(감지=%s)" % (fmt or "unknown"))
        raise _error(e)
    return Extracted(d)


#------------------------------------------------------------------
# 크기 상한 검사만
#
# -in: path                = 파일 경로
# -in: fmt                 = 형식 이름(None 이면 감지)
# -in: max_file_bytes      = 파일 크기 상한(None = 기본값, 0 이하 = 없음)
# -in: max_file_bytes_text = 텍스트류 상한(같은 규칙)
#
# -out: None 또는 dict(size, limit, text_kind, reason)
# -out: error = 인자 오류면 ExtractError
#------------------------------------------------------------------
def check_size(path, fmt=None, max_file_bytes=None, max_file_bytes_text=None):
    load()
    lim = {}
    _put(lim, "max_file_bytes", max_file_bytes)
    _put(lim, "max_file_bytes_text", max_file_bytes_text)
    _code, s = _call(_lib.docparse_check_size, _b(path), _b(fmt),
                     json.dumps({"limits": lim}).encode("utf-8"))
    d = json.loads(s)
    if d.get("error"):
        raise _error(d["error"])
    return d.get("hit")


#------------------------------------------------------------------
# 6단계 본문 정제
#
# -in: raw = 원시 텍스트
#
# -out: str = 정제된 본문
# -out: error = 내부 오류면 ExtractError
#------------------------------------------------------------------
def clean_text(raw):
    load()
    code, s = _call(_lib.docparse_clean_text, (raw or "").encode("utf-8", "surrogatepass"))
    if code != _DP_OK:
        raise _error(json.loads(s).get("error") or {})
    return s


#------------------------------------------------------------------
# 압축 펼치기
#=> 예전 PyO3 판과 같은 모양으로 돌려준다.
#
# -in: paths       = 대상 경로 목록
# -in: tmp_root    = 풀 폴더
# -in: max_depth   = 중첩 깊이 상한(기본 3)
# -in: max_bytes   = 풀린 총 크기 상한(None = 기본값, 0 이하 = 없음)
# -in: max_members = 항목 수 상한(같은 규칙)
#
# -out: (jobs, stats) — jobs = [(src, label, origin)], stats = {"unexpanded":[{path,reason,kind}],
#       "unsupported":[{path,reason}], "partial":[{path,reason}]}
# -out: error = 인자·내부 오류면 ExtractError
#------------------------------------------------------------------
def expand_paths(paths, tmp_root, max_depth=3, max_bytes=None, max_members=None):
    load()
    opts = {"max_depth": int(max_depth)}
    _put(opts, "max_bytes", max_bytes)
    _put(opts, "max_members", max_members)
    pj = json.dumps([os.fspath(p) for p in paths], ensure_ascii=False).encode("utf-8", "surrogateescape")
    _code, s = _call(_lib.docparse_expand, pj, _b(tmp_root), json.dumps(opts).encode("utf-8"))
    d = json.loads(s)
    if d.get("error"):
        raise _error(d["error"])
    jobs = [(j["src"], j["label"], j.get("origin")) for j in d.get("jobs", [])]
    return jobs, d.get("stats", {"unexpanded": [], "unsupported": [], "partial": []})


#------------------------------------------------------------------
# 분할 압축 조각 안내
#
# -in: name = 표시용 이름
# -in: path = 실제 경로(None 이면 이름만)
#
# -out: str | None = 조각이면 안내 문구
# -out: error = 없음(오류는 None)
#------------------------------------------------------------------
def split_volume_hint(name, path=None):
    load()
    _code, s = _call(_lib.docparse_split_volume_hint, _b(name), _b(path))
    return json.loads(s).get("hint")


#------------------------------------------------------------------
# 스캔 PDF 판정
#
# -in: pages      = 읽은 쪽 수
# -in: scan_pages = 그림만 있는 쪽 수
# -in: body_chars = 공백 뺀 본문 글자 수
#
# -out: bool = 스캔 PDF 면 True
# -out: error = 없음
#------------------------------------------------------------------
def scan_pdf_verdict(pages, scan_pages, body_chars):
    load()
    return bool(_lib.docparse_scan_pdf_verdict(int(pages), int(scan_pages), int(body_chars)))


#------------------------------------------------------------------
# 로그 받기
#=> fn(level, target, site, msg) 꼴의 파이썬 함수를 등록한다. None 이면 해제.
#   콜백은 추출을 부른 스레드에서 불린다(ctypes 가 GIL 을 다시 잡아 준다).
#   콜백 안에서 예외가 나면 ctypes 가 출력만 하고 넘어간다.
#
# -in: fn        = 콜백(level:int, target:str, site:str, msg:str) 또는 None
# -in: max_level = 넘길 최고 레벨(1=error 2=warn 3=info 4=debug 5=trace, 0 = 끔)
#
# -out: 없음
# -out: error = load 와 같음
#------------------------------------------------------------------
def set_logger(fn, max_level=3):
    global _logger_ref
    load()
    if fn is None:
        _lib.docparse_set_logger(LOG_FN(), 0, None)
        _logger_ref = None
        return

    # C 콜백 → 파이썬 함수. 문자열은 UTF-8 로 풀어 넘긴다.
    def _cb(level, target, site, msg, _user):
        fn(level, (target or b"").decode("utf-8", "replace"), (site or b"").decode("utf-8", "replace"),
           (msg or b"").decode("utf-8", "replace"))

    ref = LOG_FN(_cb)
    _logger_ref = ref  # 붙잡아 두지 않으면 GC 뒤 라이브러리가 죽은 포인터를 부른다
    _lib.docparse_set_logger(ref, int(max_level), None)

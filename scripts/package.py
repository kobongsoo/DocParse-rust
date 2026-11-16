# -*- coding: utf-8 -*-
#------------------------------------------------------------------
# 배포 묶음 만들기 — docparse-<버전>-<플랫폼>.zip / .tgz
#=> 빌드한 라이브러리 파일 하나를 받아, 쓰는 프로젝트가 그대로 풀어 쓸 수 있는 묶음을 만든다.
#    1) dist/docparse-<버전>-<플랫폼>/ 에 lib/ · include/ · python/ · rust/ · java/ · php/ · c/ · doc/ 를 채운다
#       Windows 는 C/C++ 링크용 가져오기 라이브러리 docparse.lib 도 lib/ 에 넣는다
#    2) VERSION · CHANGELOG.md · README.md · SHA256SUMS 를 넣는다
#    3) windows-x64 는 zip, linux-x64 는 tgz 로 묶는다
#    4) 같은 묶음을 버전 없는 이름 docparse-<플랫폼>.zip / .tgz 로도 복사한다.
#       릴리스에 이 이름도 함께 올리면 README 의 releases/latest/download/... 링크가
#       README 를 고치지 않아도 늘 최신 판을 가리킨다.
#   표준 라이브러리만 쓴다(윈도우 git bash · 리눅스 어디서나 돈다).
#
#   사용: python scripts/package.py <windows-x64|linux-x64> <라이브러리 파일> <버전>
#------------------------------------------------------------------
import hashlib
import io
import os
import shutil
import sys
import tarfile
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# 플랫폼별 (라이브러리 이름, pdfium 이름)
PLATFORMS = {
    "windows-x64": ("docparse.dll", "pdfium.dll"),
    "linux-x64": ("libdocparse.so", "libpdfium.so"),
}

# 언어별 감싸개·예제: (묶음 안 경로, 저장소 안 원본 경로)
EXTRAS = [
    ("python/docparse.py", "bindings/python/docparse.py"),
    ("python/example.py", "examples/python/example.py"),
    ("rust/docparse_dyn.rs", "bindings/rust/docparse_dyn.rs"),
    ("rust/example/Cargo.toml", "examples/rust/Cargo.toml"),
    ("rust/example/src/main.rs", "examples/rust/src/main.rs"),
    ("java/docparse/DocParse.java", "bindings/java/docparse/DocParse.java"),
    ("java/docparse/Json.java", "bindings/java/docparse/Json.java"),
    ("java/Example.java", "examples/java/Example.java"),
    ("php/DocParse.php", "bindings/php/DocParse.php"),
    ("php/example.php", "examples/php/example.php"),
    ("c/example.c", "examples/c/example.c"),
    ("doc/docparse-guide.html", "doc/docparse-라이브러리-연동설명서.html"),
]

# 저장소 안과 묶음 안에서 상대 위치가 달라지는 줄: (묶음 안 경로, 저장소 판 문자열, 묶음 판 문자열)
REWRITES = [
    ("rust/example/src/main.rs",
     '#[path = "../../../bindings/rust/docparse_dyn.rs"]',
     '#[path = "../../docparse_dyn.rs"]'),
]


#------------------------------------------------------------------
# 파일 SHA-256
#
# -in: path = 파일 경로
#
# -out: str = 16진 해시
# -out: error = 못 읽으면 OSError
#------------------------------------------------------------------
def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


#------------------------------------------------------------------
# 묶음 만들기 본체
#=> 위 머리말의 1)~4) 를 한다. 같은 이름의 옛 묶음은 지우고 새로 만든다.
#
# -in: plat    = "windows-x64" | "linux-x64"
# -in: lib     = 빌드된 라이브러리 파일 경로
# -in: version = 버전 문자열(예: "0.1.0+abc1234")
#
# -out: (arc, latest) = (버전 붙은 묶음 파일 경로, 버전 없는 이름의 복사본 경로)
# -out: error = 플랫폼을 모르거나 파일이 없으면 SystemExit
#------------------------------------------------------------------
def build(plat, lib, version):
    if plat not in PLATFORMS:
        raise SystemExit("모르는 플랫폼: %s (windows-x64 · linux-x64)" % plat)
    lib_name, pdfium_name = PLATFORMS[plat]
    pdfium = os.path.join(ROOT, "third_party", "pdfium", plat, pdfium_name)
    for p in (lib, pdfium):
        if not os.path.isfile(p):
            raise SystemExit("파일이 없습니다: %s" % p)
    # 파일 이름에 '+' 를 쓰면 일부 도구가 헷갈리므로 묶음 이름에서는 '-' 로 바꾼다.
    name = "docparse-%s-%s" % (version.replace("+", "-"), plat)
    out = os.path.join(ROOT, "dist", name)
    shutil.rmtree(out, ignore_errors=True)
    for d in ("lib", "include"):
        os.makedirs(os.path.join(out, d))
    shutil.copy2(lib, os.path.join(out, "lib", lib_name))
    shutil.copy2(pdfium, os.path.join(out, "lib", pdfium_name))
    if plat.startswith("windows"):
        # MSVC 는 cdylib 옆에 가져오기 라이브러리 docparse.dll.lib 를 만든다.
        # C/C++ 에서 링크할 때 흔히 쓰는 이름 docparse.lib 로 바꿔 넣는다(정적 라이브러리가 아니다 —
        # 실행할 때는 여전히 docparse.dll 이 필요하다).
        implib = lib + ".lib"
        if not os.path.isfile(implib):
            raise SystemExit("가져오기 라이브러리가 없습니다: %s" % implib)
        shutil.copy2(implib, os.path.join(out, "lib", "docparse.lib"))
    shutil.copy2(os.path.join(ROOT, "crates", "docparse-ffi", "include", "docparse.h"), os.path.join(out, "include"))
    # 언어별 감싸개·예제·설명서
    for dst, src in EXTRAS:
        sp = os.path.join(ROOT, *src.split("/"))
        if not os.path.isfile(sp):
            raise SystemExit("파일이 없습니다: %s" % sp)
        dp = os.path.join(out, *dst.split("/"))
        os.makedirs(os.path.dirname(dp), exist_ok=True)
        shutil.copy2(sp, dp)
    # Rust 예제는 감싸개를 상대 경로로 가리키므로 묶음 안 위치에 맞게 고쳐 쓴다.
    for dst, old, new in REWRITES:
        dp = os.path.join(out, *dst.split("/"))
        body = io.open(dp, encoding="utf-8").read()
        if old not in body:
            raise SystemExit("고쳐 쓸 줄이 없습니다(%s): %s" % (dst, old))
        io.open(dp, "w", encoding="utf-8", newline="\n").write(body.replace(old, new))
    for f in ("README.md", "CHANGELOG.md"):
        shutil.copy2(os.path.join(ROOT, f), out)
    io.open(os.path.join(out, "VERSION"), "w", encoding="utf-8", newline="\n").write(version + "\n")
    # 받는 쪽이 파일이 온전한지 확인할 수 있게 해시 목록을 남긴다.
    lines = []
    for dirpath, _dirs, files in os.walk(out):
        for f in sorted(files):
            p = os.path.join(dirpath, f)
            rel = os.path.relpath(p, out).replace("\\", "/")
            if rel != "SHA256SUMS":
                lines.append("%s  %s" % (sha256(p), rel))
    io.open(os.path.join(out, "SHA256SUMS"), "w", encoding="utf-8", newline="\n").write("\n".join(sorted(lines, key=lambda x: x[66:])) + "\n")
    # 묶기
    if plat.startswith("windows"):
        arc = out + ".zip"
        with zipfile.ZipFile(arc, "w", zipfile.ZIP_DEFLATED) as z:
            for dirpath, _dirs, files in os.walk(out):
                for f in files:
                    p = os.path.join(dirpath, f)
                    z.write(p, os.path.join(name, os.path.relpath(p, out)).replace("\\", "/"))
    else:
        arc = out + ".tgz"
        with tarfile.open(arc, "w:gz") as t:
            t.add(out, arcname=name)
    # GitHub 의 releases/latest/download/<이름> 은 이름이 판마다 같아야 늘 최신을 가리킨다.
    # 그래서 버전·해시를 뺀 이름으로 한 벌 더 둔다(내용은 위 묶음과 바이트 단위로 같다).
    latest = os.path.join(ROOT, "dist", "docparse-%s%s" % (plat, os.path.splitext(arc)[1]))
    shutil.copy2(arc, latest)
    return arc, latest


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("사용: python scripts/package.py <windows-x64|linux-x64> <라이브러리 파일> <버전>")
    arc, latest = build(sys.argv[1], sys.argv[2], sys.argv[3])
    print(arc)
    print(latest)
    # 커밋 안 된 수정으로 만든 묶음은 릴리스에 올리면 안 된다는 것을 눈에 띄게 알린다.
    if "dirty" in sys.argv[3] or "nogit" in sys.argv[3]:
        print("[주의] 커밋되지 않은 상태의 빌드입니다(%s). 릴리스에 올리지 마세요" % sys.argv[3])

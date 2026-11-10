# -*- coding: utf-8 -*-
#------------------------------------------------------------------
# 배포 묶음 만들기 — docparse-<버전>-<플랫폼>.zip / .tgz
#=> 빌드한 라이브러리 파일 하나를 받아, 쓰는 프로젝트가 그대로 풀어 쓸 수 있는 묶음을 만든다.
#    1) dist/docparse-<버전>-<플랫폼>/ 에 lib/ · include/ · python/ · rust/ 를 채운다
#    2) VERSION · CHANGELOG.md · README.md · SHA256SUMS 를 넣는다
#    3) windows-x64 는 zip, linux-x64 는 tgz 로 묶는다
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
#=> 위 머리말의 1)~3) 을 한다. 같은 이름의 옛 묶음은 지우고 새로 만든다.
#
# -in: plat    = "windows-x64" | "linux-x64"
# -in: lib     = 빌드된 라이브러리 파일 경로
# -in: version = 버전 문자열(예: "0.1.0+abc1234")
#
# -out: str = 만든 묶음 파일 경로
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
    for d in ("lib", "include", "python", "rust"):
        os.makedirs(os.path.join(out, d))
    shutil.copy2(lib, os.path.join(out, "lib", lib_name))
    shutil.copy2(pdfium, os.path.join(out, "lib", pdfium_name))
    shutil.copy2(os.path.join(ROOT, "crates", "docparse-ffi", "include", "docparse.h"), os.path.join(out, "include"))
    shutil.copy2(os.path.join(ROOT, "bindings", "python", "docparse.py"), os.path.join(out, "python"))
    shutil.copy2(os.path.join(ROOT, "bindings", "rust", "docparse_dyn.rs"), os.path.join(out, "rust"))
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
    return arc


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("사용: python scripts/package.py <windows-x64|linux-x64> <라이브러리 파일> <버전>")
    print(build(sys.argv[1], sys.argv[2], sys.argv[3]))
